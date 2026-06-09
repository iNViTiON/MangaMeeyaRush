//! 7z / cb7 archive as a PageSource.
//!
//! Enumeration walks the archive once at open via
//! `ArchiveReader::for_each_entries` to collect image entry names. Reads
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
        let mut reader = ArchiveReader::open(path, Password::empty())
            .map_err(|e| CodecError::Sevenz(e.to_string()))?;

        let mut entries = Vec::new();
        reader
            .for_each_entries(|entry, r| {
                if !entry.is_directory && is_image_path(Path::new(entry.name())) {
                    entries.push(Entry {
                        archive_name: entry.name().to_string(),
                        display: entry.name().to_string(),
                    });
                }
                // We still need to consume the reader stream even for files we
                // skip, to keep the solid-block decoder advancing correctly.
                std::io::copy(r, &mut std::io::sink()).map_err(sevenz_rust2::Error::from)?;
                Ok(true)
            })
            .map_err(|e| CodecError::Sevenz(e.to_string()))?;

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
            // Seed the pool with the reader we just used for enumeration.
            pool: Mutex::new(vec![reader]),
        })
    }

    fn acquire(&self) -> Result<ArchiveReader<std::fs::File>, CodecError> {
        if let Some(r) = self.pool.lock().ok().and_then(|mut p| p.pop()) {
            return Ok(r);
        }
        ArchiveReader::open(&self.path, Password::empty())
            .map_err(|e| CodecError::Sevenz(e.to_string()))
    }

    fn release(&self, reader: ArchiveReader<std::fs::File>) {
        if let Ok(mut pool) = self.pool.lock() {
            if pool.len() < MAX_POOL {
                pool.push(reader);
            }
        }
    }
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
