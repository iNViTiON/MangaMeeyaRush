//! Image decoders and archive readers unified behind a `PageSource` trait.
//!
//! Supported page sources:
//! - Folder (loose images in a directory)
//! - ZIP archive
//! - 7z archive
//!
//! Image decoding is delegated to the `image` crate (PNG, JPEG, GIF, BMP,
//! WebP, TIFF).

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::DynamicImage;

pub mod folder;
pub mod zip_src;
pub mod sevenz_src;

pub use folder::folder_has_direct_images;

/// Static set of image file extensions we will attempt to decode.
pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "webp", "tif", "tiff",
];

pub fn is_image_path(path: &Path) -> bool {
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => IMAGE_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext)),
        None => false,
    }
}

pub fn is_archive_path(path: &Path) -> bool {
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            matches!(ext.as_str(), "zip" | "cbz" | "7z" | "cb7")
        }
        None => false,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("7z: {0}")]
    Sevenz(String),
    #[error("unsupported archive format: {0:?}")]
    UnsupportedArchive(PathBuf),
    #[error("page index {0} out of range (len {1})")]
    OutOfRange(usize, usize),
    #[error("{0}")]
    Other(String),
}

/// A sequence of addressable "pages" (images), backed by a folder or an
/// archive file.
pub trait PageSource: Send + Sync {
    /// Total number of pages.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Display name of the source (folder name or archive filename).
    fn name(&self) -> &str;

    /// Entry name of page `idx` (for display + sorting).
    fn entry_name(&self, idx: usize) -> Option<&str>;

    /// Read the raw bytes of page `idx`.
    fn read(&self, idx: usize) -> Result<Vec<u8>, CodecError>;
}

/// Helper: decode raw image bytes into a DynamicImage, honouring EXIF
/// orientation metadata where available.
pub fn decode_image(bytes: &[u8]) -> Result<DynamicImage, CodecError> {
    let cursor = Cursor::new(bytes);
    let mut reader = image::ImageReader::new(cursor).with_guessed_format()?;
    reader.no_limits();
    let img = reader.decode()?;
    Ok(img)
}

/// Open any supported source from a path. If the path is a directory we build
/// a FolderSource; if it's an archive file we route by extension; if it's a
/// loose image file we build a FolderSource over its parent directory, scoped
/// to the single file's siblings.
pub fn open_source(path: &Path) -> Result<Box<dyn PageSource>, CodecError> {
    if path.is_dir() {
        Ok(Box::new(folder::FolderSource::open(path)?))
    } else if is_archive_path(path) {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "zip" | "cbz" => Ok(Box::new(zip_src::ZipSource::open(path)?)),
            "7z" | "cb7" => Ok(Box::new(sevenz_src::SevenzSource::open(path)?)),
            _ => Err(CodecError::UnsupportedArchive(path.to_path_buf())),
        }
    } else if is_image_path(path) {
        // Open the parent dir and land on this file.
        let parent = path.parent().unwrap_or(Path::new("."));
        Ok(Box::new(folder::FolderSource::open(parent)?))
    } else {
        Err(CodecError::UnsupportedArchive(path.to_path_buf()))
    }
}

/// Read a whole file into memory (helper used by archive readers).
pub(crate) fn read_all(path: &Path) -> Result<Vec<u8>, CodecError> {
    let mut buf = Vec::new();
    fs::File::open(path)?.read_to_end(&mut buf)?;
    Ok(buf)
}

// Avoid unused warning from `Mutex` import when tests are off.
#[allow(dead_code)]
fn _touch_mutex() -> Mutex<()> {
    Mutex::new(())
}
