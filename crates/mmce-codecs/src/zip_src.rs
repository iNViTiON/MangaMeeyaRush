//! ZIP / CBZ archive as a PageSource.
//!
//! Entries are discovered once at open, filtered to images and sorted by
//! natural order. Reads stream through a shared `Mutex<ZipArchive>` on the
//! underlying file.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zip::ZipArchive;

use crate::{is_image_path, CodecError, PageSource};

pub struct ZipSource {
    name: String,
    entries: Vec<Entry>,
    archive: Mutex<ZipArchive<File>>,
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
            entries,
            archive: Mutex::new(archive),
        })
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
        let mut archive = self
            .archive
            .lock()
            .map_err(|_| CodecError::Other("zip mutex poisoned".into()))?;
        let mut f = archive.by_index(entry.zip_index)?;
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }
}
