//! ZIP / CBZ archive as a PageSource.
//!
//! Entries are discovered once at open, filtered to images and sorted by
//! natural order. Reads are parallel-safe: each read acquires a
//! `ZipArchive<File>` from a pool (or spins up a new one if the pool is
//! empty). This lets decoder workers inflate concurrently on NVMe-class
//! storage instead of serializing behind a single mutex.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zip::ZipArchive;

use crate::{is_image_path, CodecError, PageSource};

/// Hard cap on the pooled archive instances. Each instance holds an open
/// file handle; the cap keeps us from exhausting descriptors if a decoder
/// pool temporarily balloons.
const MAX_POOL: usize = 8;

pub struct ZipSource {
    name: String,
    path: PathBuf,
    entries: Vec<Entry>,
    pool: Mutex<Vec<ZipArchive<File>>>,
}

struct Entry {
    zip_index: usize,
    display: String,
}

impl ZipSource {
    pub fn open(path: &Path) -> Result<Self, CodecError> {
        let file = File::open(path)?;
        let mut archive = ZipArchive::new(file)?;

        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let f = archive.by_index(i)?;
            if f.is_dir() {
                continue;
            }
            let name = f.enclosed_name().unwrap_or_else(|| PathBuf::from(f.name()));
            if is_image_path(&name) {
                entries.push(Entry {
                    zip_index: i,
                    display: name.display().to_string(),
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
    fn acquire(&self) -> Result<ZipArchive<File>, CodecError> {
        if let Some(a) = self.pool.lock().ok().and_then(|mut p| p.pop()) {
            return Ok(a);
        }
        let file = File::open(&self.path)?;
        Ok(ZipArchive::new(file)?)
    }

    fn release(&self, archive: ZipArchive<File>) {
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
