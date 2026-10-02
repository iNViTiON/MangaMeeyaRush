//! ZIP / CBZ archive as a PageSource.
//!
//! Entries are discovered once at open, filtered to images and sorted by
//! natural order. Reads are parallel-safe: each read acquires a
//! `ZipArchive<File>` from a pool (or spins up a new one if the pool is
//! empty). This lets decoder workers inflate concurrently on NVMe-class
//! storage instead of serializing behind a single mutex.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zip::ZipArchive;

use crate::{is_image_path, CodecError, PageSource};

/// Hard cap on the pooled archive instances. Each instance holds an open
/// file handle; the cap keeps us from exhausting descriptors if a decoder
/// pool temporarily balloons.
const MAX_POOL: usize = 8;

type Archive = ZipArchive<SeekBufReader<File>>;

pub struct ZipSource {
    name: String,
    path: PathBuf,
    entries: Vec<Entry>,
    pool: Mutex<Vec<Archive>>,
}

struct Entry {
    zip_index: usize,
    display: String,
}

impl ZipSource {
    pub fn open(path: &Path) -> Result<Self, CodecError> {
        let archive = open_archive(path)?;

        // Enumerate from the in-memory central directory only. `by_index`
        // would seek to each entry's local header (one random read per page,
        // scattered across the whole file) and allocate a decompressor —
        // costly on a cold explorer scan where we only want the cover.
        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let Some(name) = archive.name_for_index(i) else {
                continue;
            };
            if name.ends_with('/') || name.ends_with('\\') {
                continue;
            }
            if is_image_path(Path::new(name)) {
                entries.push(Entry {
                    zip_index: i,
                    display: name.to_string(),
                });
            }
        }
        entries.sort_by(|a, b| natord::compare(&a.display, &b.display));

        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        Ok(Self {
            name,
            path: path.to_path_buf(),
            entries,
            // Seed the pool with the archive we just scanned so the first
            // read is a zero-cost reuse.
            pool: Mutex::new(vec![archive]),
        })
    }

    /// Pop an archive handle from the pool, opening a fresh one if the
    /// pool is empty. Cheap in the reuse case; a fresh open costs one
    /// `File::open` + central directory scan (~ms for typical archives).
    fn acquire(&self) -> Result<Archive, CodecError> {
        if let Some(a) = self.pool.lock().ok().and_then(|mut p| p.pop()) {
            return Ok(a);
        }
        open_archive(&self.path)
    }

    fn release(&self, archive: Archive) {
        if let Ok(mut pool) = self.pool.lock() {
            if pool.len() < MAX_POOL {
                pool.push(archive);
            }
        }
    }
}

impl PageSource for ZipSource {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn entry_name(&self, idx: usize) -> Option<&str> {
        self.entries.get(idx).map(|e| e.display.as_str())
    }

    fn read(&self, idx: usize) -> Result<Vec<u8>, CodecError> {
        let entry = self
            .entries
            .get(idx)
            .ok_or(CodecError::OutOfRange(idx, self.entries.len()))?;
        let mut archive = self.acquire()?;
        let result = (|| {
            let mut f = archive.by_index(entry.zip_index)?;
            let mut buf = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut buf)?;
            Ok(buf)
        })();
        self.release(archive);
        result
    }
}

fn open_archive(path: &Path) -> Result<Archive, CodecError> {
    Ok(ZipArchive::new(SeekBufReader::new(File::open(path)?))?)
}

/// Reads smaller than this go through the buffer; larger ones (entry data:
/// flate2's 32 KiB chunks, `read_to_end` into a pre-sized Vec) bypass it so
/// page bytes aren't memcpy'd twice.
const DIRECT_READ_MIN: usize = 8 * 1024;
const SEEK_BUF_CAP: usize = 64 * 1024;

/// A buffered reader whose buffer survives seeks that land inside it.
///
/// zip's central-directory parser does a handful of tiny reads per entry and
/// then `seek(SeekFrom::Start(stream_position()))`. `std::io::BufReader`
/// discards its buffer on every `seek`, so it degrades to ~5 syscalls per
/// entry — hundreds per cover on a typical CBZ, each a potential round-trip
/// on FUSE / network mounts. Here a seek is just a cursor move; the
/// underlying file is only touched when a read falls outside the buffer.
struct SeekBufReader<R> {
    inner: R,
    buf: Box<[u8]>,
    /// File offset of `buf[0]`.
    buf_start: u64,
    /// Valid bytes in `buf`.
    filled: usize,
    /// Logical position seen by the caller.
    pos: u64,
    /// Where `inner` actually is, if known. Lets consecutive direct reads
    /// skip the `lseek`.
    inner_pos: Option<u64>,
}

impl<R: Read + Seek> SeekBufReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buf: vec![0; SEEK_BUF_CAP].into_boxed_slice(),
            buf_start: 0,
            filled: 0,
            pos: 0,
            inner_pos: None,
        }
    }

    fn sync_inner(&mut self) -> io::Result<()> {
        if self.inner_pos != Some(self.pos) {
            self.inner.seek(SeekFrom::Start(self.pos))?;
            self.inner_pos = Some(self.pos);
        }
        Ok(())
    }
}

impl<R: Read + Seek> Read for SeekBufReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let buf_end = self.buf_start + self.filled as u64;
        if self.pos >= self.buf_start && self.pos < buf_end {
            let off = (self.pos - self.buf_start) as usize;
            let n = out.len().min(self.filled - off);
            out[..n].copy_from_slice(&self.buf[off..off + n]);
            self.pos += n as u64;
            return Ok(n);
        }
        self.sync_inner()?;
        if out.len() >= DIRECT_READ_MIN {
            let n = self.inner.read(out)?;
            self.pos += n as u64;
            self.inner_pos = Some(self.pos);
            return Ok(n);
        }
        let n = self.inner.read(&mut self.buf)?;
        self.buf_start = self.pos;
        self.filled = n;
        self.inner_pos = Some(self.pos + n as u64);
        let k = out.len().min(n);
        out[..k].copy_from_slice(&self.buf[..k]);
        self.pos += k as u64;
        Ok(k)
    }
}

impl<R: Read + Seek> Seek for SeekBufReader<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = match from {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => self
                .pos
                .checked_add_signed(d)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?,
            SeekFrom::End(_) => {
                let p = self.inner.seek(from)?;
                self.inner_pos = Some(p);
                p
            }
        };
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Drive `SeekBufReader<Cursor>` and a bare `Cursor` through the same
    /// pseudo-random seek/read script and require identical bytes and
    /// positions — covers buffer hits, partial hits at the buffer edge,
    /// direct reads, refills, End-relative seeks and reads past EOF.
    #[test]
    fn seek_buf_reader_matches_plain_cursor() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let data: Vec<u8> = (0..300_000).map(|_| rnd() as u8).collect();
        let len = data.len() as u64;
        let mut ours = SeekBufReader::new(Cursor::new(data.clone()));
        let mut reference = Cursor::new(data);
        for _ in 0..5_000 {
            let from = match rnd() % 4 {
                0 => SeekFrom::Start(rnd() % (len + 100)),
                1 => SeekFrom::Current((rnd() % 2_000) as i64 - 1_000),
                2 => SeekFrom::End(-((rnd() % 5_000) as i64)),
                _ => SeekFrom::Current(0),
            };
            let a = ours.seek(from).ok();
            let b = reference.seek(from).ok();
            assert_eq!(a, b);
            let size = match rnd() % 3 {
                0 => (rnd() % 64) as usize,
                1 => (rnd() % 4_096) as usize,
                _ => (rnd() % 100_000) as usize,
            };
            let mut x = vec![0u8; size];
            let mut y = vec![0u8; size];
            let ok_ours = ours.read_exact(&mut x).is_ok();
            let ok_ref = reference.read_exact(&mut y).is_ok();
            assert_eq!(ok_ours, ok_ref);
            if ok_ref {
                assert_eq!(x, y);
                assert_eq!(ours.stream_position().unwrap(), reference.position());
            } else {
                // Position after a failed read_exact is unspecified; re-sync.
                ours.seek(SeekFrom::Start(0)).unwrap();
                reference.seek(SeekFrom::Start(0)).unwrap();
            }
        }
    }
}
