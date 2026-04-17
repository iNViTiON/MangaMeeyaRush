//! Loose-image folder as a PageSource.
//!
//! By default we recursively walk one level of subfolders — this matches the
//! common manga-on-disk layout where a "volume" is a folder of chapter
//! subfolders full of PNG/JPEG pages. Depth is capped so we don't wander
//! through an entire home directory if the user pointed us at the wrong
//! place.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{is_image_path, CodecError, PageSource};

/// How many directory levels below `root` we will include by default.
/// 0 = only the root (match the legacy `SubFolderLoad=0`). A "library"
/// folder full of chapter subfolders is meant to be browsed in the
/// explorer, not flattened into a single book.
pub const DEFAULT_SUBFOLDER_DEPTH: usize = 0;

/// Returns true if `dir` has at least one image directly inside it (not
/// counting subfolders). Used to decide whether a folder should be opened
/// as a book or browsed as a library.
pub fn folder_has_direct_images(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_file() && is_image_path(&p) {
            return true;
        }
    }
    false
}

pub struct FolderSource {
    root: PathBuf,
    name: String,
    entries: Vec<Entry>,
}

struct Entry {
    path: PathBuf,
    /// Display key used for natural sorting — relative path from `root`.
    sort_key: String,
}

impl FolderSource {
    pub fn open(dir: &Path) -> Result<Self, CodecError> {
        Self::open_with_depth(dir, DEFAULT_SUBFOLDER_DEPTH)
    }

    pub fn open_with_depth(dir: &Path, max_depth: usize) -> Result<Self, CodecError> {
        let mut entries: Vec<Entry> = Vec::new();
        walk(dir, dir, max_depth, &mut entries)?;
        entries.sort_by(|a, b| natord::compare(&a.sort_key, &b.sort_key));

        let name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        Ok(Self {
            root: dir.to_path_buf(),
            name,
            entries,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<Entry>) -> Result<(), CodecError> {
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = match e {
            Ok(e) => e,
            Err(_) => continue,
        };
        let p = e.path();
        // Resolve file type without following symlinks to avoid cycles.
        let ft = match e.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_file() && is_image_path(&p) {
            let sort_key = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            out.push(Entry { path: p, sort_key });
        } else if ft.is_dir() {
            subdirs.push(p);
        }
    }
    if depth > 0 {
        subdirs.sort_by(|a, b| natord::compare(&a.to_string_lossy(), &b.to_string_lossy()));
        for sub in subdirs {
            walk(root, &sub, depth - 1, out)?;
        }
    }
    Ok(())
}

impl PageSource for FolderSource {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn entry_name(&self, idx: usize) -> Option<&str> {
        self.entries.get(idx).map(|e| e.sort_key.as_str())
    }

    fn read(&self, idx: usize) -> Result<Vec<u8>, CodecError> {
        let e = self
            .entries
            .get(idx)
            .ok_or(CodecError::OutOfRange(idx, self.entries.len()))?;
        crate::read_all(&e.path)
    }
}
