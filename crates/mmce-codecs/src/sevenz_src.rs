//! 7z / cb7 archive as a PageSource.
//!
//! Enumeration reads entry names from the archive header only — nothing is
//! decompressed at open, however large or solid the archive. Reads
//! use `ArchiveReader::read_file` on a pooled reader instance so decoder
//! workers can decompress concurrently — each instance maintains its own
//! solid-block state, so multiple readers in parallel do give real
//! throughput on NVMe even though a single solid block read is inherently
//! sequential.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sevenz_rust2::{ArchiveReader, Password};

use crate::{is_image_path, CodecError, PageSource};

/// Cap on pooled reader instances. Each holds an open file handle + a
/// decompression context; the cap keeps descriptor and memory use bounded.
const MAX_POOL: usize = 8;

pub struct SevenzSource {
    name: String,
    path: PathBuf,
    entries: Vec<Entry>,
    pool: Mutex<Vec<ArchiveReader<std::fs::File>>>,
}

struct Entry {
    archive_name: String,
    display: String,
}

impl SevenzSource {
    pub fn open(path: &Path) -> Result<Self, CodecError> {
        let reader = open_reader(path)?;

        // The header already lists every entry. Walking `for_each_entries`
        // instead would push the whole archive through the solid-block
        // decoder just to learn the names.
        let mut entries: Vec<Entry> = reader
            .archive()
            .files
            .iter()
            .filter(|e| !e.is_directory() && is_image_path(Path::new(e.name())))
            .map(|e| Entry {
                archive_name: e.name().to_string(),
                display: e.name().to_string(),
            })
            .collect();

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
            // Seed the pool with the reader we just opened.
            pool: Mutex::new(vec![reader]),
        })
    }

    fn acquire(&self) -> Result<ArchiveReader<std::fs::File>, CodecError> {
        if let Some(r) = self.pool.lock().ok().and_then(|mut p| p.pop()) {
            return Ok(r);
        }
        open_reader(&self.path)
    }

    fn release(&self, reader: ArchiveReader<std::fs::File>) {
        if let Ok(mut pool) = self.pool.lock() {
            if pool.len() < MAX_POOL {
                pool.push(reader);
            }
        }
    }
}

fn open_reader(path: &Path) -> Result<ArchiveReader<std::fs::File>, CodecError> {
    let mut reader = ArchiveReader::open(path, Password::empty())
        .map_err(|e| CodecError::Sevenz(e.to_string()))?;
    // sevenz-rust2 defaults to one LZMA2 decode thread per core on every
    // reader. Parallelism here comes from the pool (one reader per decoder
    // worker), so per-read threads only oversubscribe — and the
    // multi-threaded decoder deadlocks on corrupt input, wedging the worker
    // forever (see `sevenz_open_does_not_decompress`).
    reader.set_thread_count(1);
    Ok(reader)
}

impl PageSource for SevenzSource {
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
        let mut reader = self.acquire()?;
        let result = reader
            .read_file(&entry.archive_name)
            .map_err(|e| CodecError::Sevenz(e.to_string()));
        self.release(reader);
        result
    }
}
