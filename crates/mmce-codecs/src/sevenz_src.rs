//! 7z / cb7 archive as a PageSource.
//!
//! Enumeration walks the archive once at open via `ArchiveReader::for_each_entries`
//! to collect image entry names. Reads use `ArchiveReader::read_file` by entry
//! name. Because solid archives may re-decompress upstream blocks per read, we
//! lean on mmce-render's LRU cache to absorb repeated access.

use std::path::Path;
use std::sync::Mutex;

use sevenz_rust2::{ArchiveReader, Password};

use crate::{is_image_path, CodecError, PageSource};

pub struct SevenzSource {
    name: String,
    entries: Vec<Entry>,
    reader: Mutex<ArchiveReader<std::fs::File>>,
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
                std::io::copy(r, &mut std::io::sink())
                    .map_err(sevenz_rust2::Error::from)?;
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
            entries,
            reader: Mutex::new(reader),
        })
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
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| CodecError::Other("7z mutex poisoned".into()))?;
        // `read_file` returns the decompressed bytes directly.
        reader
            .read_file(&entry.archive_name)
            .map_err(|e| CodecError::Sevenz(e.to_string()))
    }
}
