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
pub mod sevenz_src;
pub mod zip_src;

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

/// Decode `bytes` into a `DynamicImage` sized for a thumbnail whose longest
/// edge is `target` px.
///
/// For JPEG covers this uses libjpeg-turbo's DCT-domain downscale (decode
/// straight to the smallest 1/8…1/1 step whose longest edge is still
/// ≥ `target`), which is ~1.5–2× faster than a full decode on the covers we
/// measured and also collapses the caller's later box-shrink. Any non-JPEG
/// input, CMYK/YCCK JPEG, or decode error falls through to the full-resolution
/// zune path — so the worst case is "slower", never "wrong" or "missing".
pub fn decode_cover_image(bytes: &[u8], target: u32) -> Option<DynamicImage> {
    decode_jpeg_scaled(bytes, target).or_else(|| decode_image(bytes).ok())
}

/// JPEG-only DCT-scaled decode via mozjpeg (libjpeg-turbo). Returns `None` for
/// anything mozjpeg can't cleanly turn into RGB — non-JPEG (rejected up front by
/// the SOI check), CMYK/YCCK (`rgb()` errors with `JERR_CONVERSION_NOTIMPL`), or
/// a malformed stream — letting `decode_cover_image` fall back to the zune path.
///
/// The SOI guard matters: handed non-JPEG bytes, libjpeg's default error path
/// can abort the process rather than return an error, so we must never feed it
/// anything that isn't a JPEG (e.g. the PNG covers in a mixed folder).
fn decode_jpeg_scaled(bytes: &[u8], target: u32) -> Option<DynamicImage> {
    if !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return None;
    }
    // A malformed-but-SOI-valid JPEG can make mozjpeg panic (the PNG case
    // proved its error path unwinds rather than returning `Err`). Contain it
    // so one bad cover degrades to the zune fallback instead of unwinding the
    // decode worker thread.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode_jpeg_scaled_inner(bytes, target)
    }))
    .ok()
    .flatten()
}

fn decode_jpeg_scaled_inner(bytes: &[u8], target: u32) -> Option<DynamicImage> {
    let mut dec = mozjpeg::Decompress::new_mem(bytes).ok()?;
    let (w, h) = (dec.width() as u64, dec.height() as u64);
    if w == 0 || h == 0 {
        return None;
    }
    // Smallest n in 1..=8 (scale = n/8) whose output's longest edge is still
    // ≥ target, so the caller's fit-shrink never has to upscale.
    let n = (target.max(1) as u64 * 8).div_ceil(w.max(h)).clamp(1, 8) as u8;
    dec.scale(n);
    let mut started = dec.rgb().ok()?;
    let (ow, oh) = (started.width(), started.height());
    let pixels: Vec<u8> = started.read_scanlines::<u8>().ok()?;
    started.finish().ok()?;
    // mozjpeg packs scanlines tightly (RGB, no row padding); be explicit so a
    // surprise buffer size can't slip a sheared image into `from_raw`.
    if pixels.len() != ow.checked_mul(oh)?.checked_mul(3)? {
        return None;
    }
    let buf = image::RgbImage::from_raw(ow as u32, oh as u32, pixels)?;
    Some(DynamicImage::ImageRgb8(buf))
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

/// Fast path for "give me the cover image bytes" — avoids the full
/// `open_source` + enumerate pipeline. Primarily a win for 7z: the normal
/// `SevenzSource::open` walks every entry through the solid-block decoder
/// just to list them, which can take seconds on big archives. Here we stop
/// after the first image.
///
/// For folders and ZIPs the savings vs. `open_source(path)?.read(0)` are
/// small (central-directory scan is already cheap), but going through this
/// entry point keeps the caller simple.
pub fn cover_image(path: &Path) -> Option<Vec<u8>> {
    if path.is_dir() {
        folder_cover(path)
    } else if is_archive_path(path) {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "zip" | "cbz" => zip_cover(path),
            "7z" | "cb7" => sevenz_cover(path),
            _ => None,
        }
    } else if is_image_path(path) {
        fs::read(path).ok()
    } else {
        None
    }
}

fn folder_cover(dir: &Path) -> Option<Vec<u8>> {
    // Match FolderSource's sort ordering so the cover matches page 0.
    let src = folder::FolderSource::open(dir).ok()?;
    if src.is_empty() {
        return None;
    }
    src.read(0).ok()
}

fn zip_cover(path: &Path) -> Option<Vec<u8>> {
    // ZipArchive's central directory scan is O(entries) seek+read but
    // compressed bytes are only touched for the entry we actually read.
    // Delegate to the normal source — already minimal.
    let src = zip_src::ZipSource::open(path).ok()?;
    if src.is_empty() {
        return None;
    }
    src.read(0).ok()
}

fn sevenz_cover(path: &Path) -> Option<Vec<u8>> {
    use sevenz_rust2::{ArchiveReader, Password};
    let mut reader = ArchiveReader::open(path, Password::empty()).ok()?;
    let mut cover: Option<Vec<u8>> = None;
    let _ = reader.for_each_entries(|entry, r| {
        if cover.is_some() {
            // Already got the cover — stop iteration.
            return Ok(false);
        }
        if !entry.is_directory && is_image_path(Path::new(entry.name())) {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            std::io::copy(r, &mut buf).map_err(sevenz_rust2::Error::from)?;
            cover = Some(buf);
            return Ok(false);
        }
        // Consume the stream even for non-image entries — the solid-block
        // decoder must stay in sync.
        std::io::copy(r, &mut std::io::sink()).map_err(sevenz_rust2::Error::from)?;
        Ok(true)
    });
    cover
}

// Avoid unused warning from `Mutex` import when tests are off.
#[allow(dead_code)]
fn _touch_mutex() -> Mutex<()> {
    Mutex::new(())
}

// We depend on flate2 directly so the workspace's `zlib-rs` backend
// feature actually gets unified into the final flate2 compilation (zip
// and png both pull flate2 transitively). The crate itself is unused at
// our level — this reference keeps it alive through dead-code analysis.
#[allow(dead_code)]
#[allow(unused_imports)]
mod _flate2_anchor {
    use flate2 as _;
}

#[cfg(test)]
mod cover_decode_tests {
    use super::*;
    use image::GenericImageView;

    /// A smooth gradient compresses cleanly, keeping JPEG artefacts low so the
    /// pixel-similarity test can use a tight threshold.
    fn smooth(w: u32, h: u32) -> image::RgbImage {
        let mut img = image::RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 128]);
        }
        img
    }

    fn encode(img: image::RgbImage, fmt: image::ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, fmt)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn scaled_jpeg_is_actually_downscaled() {
        // Proves the mozjpeg path engaged AND read full source dims pre-scale:
        // a full-decode fallback would return 1024x1536 unchanged.
        let jpg = encode(smooth(1024, 1536), image::ImageFormat::Jpeg);
        let img = decode_cover_image(&jpg, 96).expect("decode");
        let (w, h) = img.dimensions();
        assert!(w.max(h) < 1536 / 2, "expected DCT-downscaled decode, got {w}x{h}");
        assert!(w.max(h) >= 96, "longest edge must stay >= target, got {w}x{h}");
    }

    #[test]
    fn scaled_decode_matches_full_decode() {
        // The decisive check: same pixels, not just same size. A channel swap
        // or sheared buffer passes a dimension assert but blows this up.
        let jpg = encode(smooth(800, 1200), image::ImageFormat::Jpeg);
        let fast = decode_cover_image(&jpg, 88)
            .unwrap()
            .thumbnail_exact(64, 96)
            .into_rgb8();
        let full = decode_image(&jpg)
            .unwrap()
            .thumbnail_exact(64, 96)
            .into_rgb8();
        assert_eq!(fast.dimensions(), full.dimensions());
        let (a, b) = (fast.as_raw(), full.as_raw());
        let sum: u64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64)
            .sum();
        let mad = sum as f64 / a.len() as f64;
        assert!(mad < 12.0, "mean abs pixel diff {mad:.2} — channel order/garble?");
    }

    #[test]
    fn non_jpeg_falls_back_to_full_decode() {
        let png = encode(smooth(300, 400), image::ImageFormat::Png);
        let img = decode_cover_image(&png, 88).expect("decode");
        assert_eq!(img.dimensions(), (300, 400));
    }

    #[test]
    fn tiny_jpeg_is_not_upscaled() {
        let jpg = encode(smooth(40, 60), image::ImageFormat::Jpeg);
        let img = decode_cover_image(&jpg, 88).expect("decode");
        let (w, h) = img.dimensions();
        assert!(w <= 40 && h <= 60, "tiny cover must not upscale, got {w}x{h}");
    }

    #[test]
    fn malformed_jpeg_does_not_panic() {
        // SOI marker followed by garbage. mozjpeg's error path may panic; the
        // catch_unwind in decode_jpeg_scaled must contain it so the worker
        // thread survives and the caller just gets None. If this test panics
        // (or aborts the process) the containment is insufficient.
        let mut corrupt = vec![0xFF, 0xD8, 0xFF];
        corrupt.extend(std::iter::repeat_n(0xA5, 512));
        assert!(decode_cover_image(&corrupt, 88).is_none());
    }

    #[test]
    fn grayscale_jpeg_decodes_to_equal_rgb_channels() {
        // Scanned B&W manga pages are 1-component JPEGs. mozjpeg's `.rgb()`
        // must upsample them to three equal channels, not drop them — a future
        // switch off `.rgb()` (e.g. to `.image()`/grayscale) would fail the
        // `w*h*3` guard and silently route every grayscale cover to the slow
        // fallback; this pins that contract.
        let mut img = image::GrayImage::new(640, 960);
        for (x, _y, p) in img.enumerate_pixels_mut() {
            *p = image::Luma([(x * 255 / 640) as u8]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        let jpg = out.into_inner();

        let decoded = decode_cover_image(&jpg, 96).expect("decode");
        let (w, h) = decoded.dimensions();
        assert!(w.max(h) < 960 / 2 && w.max(h) >= 96, "expected downscaled, got {w}x{h}");
        let rgb = decoded.to_rgb8();
        for (x, y) in [(0u32, 0u32), (w / 2, h / 2), (w - 1, h - 1)] {
            let [r, g, b] = rgb.get_pixel(x, y).0;
            // Exact for a true grayscale JPEG; tolerate ±2 in case the encoder
            // emitted 3-component YCbCr from the gray source.
            assert!(r.abs_diff(g) <= 2 && g.abs_diff(b) <= 2, "channels diverge at {x},{y}: {r},{g},{b}");
        }
    }
}
