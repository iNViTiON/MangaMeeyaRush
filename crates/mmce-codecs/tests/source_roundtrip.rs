//! Integration tests for folder + zip page sources.

use std::fs;
use std::io::Write;

use image::{ImageBuffer, Rgb};
use mmce_codecs::{decode_image, open_source, PageSource};

fn make_png(seed: u8) -> Vec<u8> {
    let buf: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(40, 60, |x, y| {
        Rgb([
            ((x + seed as u32) % 255) as u8,
            ((y + seed as u32) % 255) as u8,
            seed,
        ])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(buf)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn folder_source_natural_sort_and_decode() {
    let dir = tempfile::tempdir().unwrap();
    // Deliberately write in reverse to verify sort.
    for (i, name) in ["10.png", "2.png", "1.png"].iter().enumerate() {
        fs::write(dir.path().join(name), make_png(i as u8)).unwrap();
    }
    let src = open_source(dir.path()).unwrap();
    assert_eq!(src.len(), 3);
    assert_eq!(src.entry_name(0), Some("1.png"));
    assert_eq!(src.entry_name(1), Some("2.png"));
    assert_eq!(src.entry_name(2), Some("10.png"));

    let bytes = src.read(0).unwrap();
    let img = decode_image(&bytes).unwrap();
    assert_eq!(img.width(), 40);
    assert_eq!(img.height(), 60);
}

#[test]
fn zip_source_reads_images_by_index() {
    let dir = tempfile::tempdir().unwrap();
    let zip_path = dir.path().join("book.cbz");
    {
        let f = fs::File::create(&zip_path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (i, name) in ["page01.png", "page02.png", "page10.png"].iter().enumerate() {
            w.start_file(*name, opts).unwrap();
            w.write_all(&make_png(i as u8 + 1)).unwrap();
        }
        // Add a non-image — should be ignored.
        w.start_file("note.txt", opts).unwrap();
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();
    }

    let src = open_source(&zip_path).unwrap();
    assert_eq!(src.len(), 3);
    assert_eq!(src.entry_name(0), Some("page01.png"));
    assert_eq!(src.entry_name(2), Some("page10.png"));

    let bytes = src.read(1).unwrap();
    let img = decode_image(&bytes).unwrap();
    assert_eq!(img.width(), 40);
}

#[test]
fn folder_source_default_is_direct_only() {
    let dir = tempfile::tempdir().unwrap();
    let ch_a = dir.path().join("Chapter_01");
    fs::create_dir_all(&ch_a).unwrap();
    fs::write(ch_a.join("01.png"), make_png(1)).unwrap();

    let src = open_source(dir.path()).unwrap();
    assert_eq!(src.len(), 0, "library folders must not swallow chapter pages");
}

#[test]
fn folder_has_direct_images_flags_library_vs_book() {
    let dir = tempfile::tempdir().unwrap();
    let ch = dir.path().join("Chapter_01");
    fs::create_dir_all(&ch).unwrap();
    fs::write(ch.join("01.png"), make_png(1)).unwrap();

    assert!(
        !mmce_codecs::folder_has_direct_images(dir.path()),
        "library folder should not have direct images"
    );
    assert!(
        mmce_codecs::folder_has_direct_images(&ch),
        "chapter folder should have direct images"
    );
}

#[test]
fn loose_image_opens_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.png"), make_png(7)).unwrap();
    fs::write(dir.path().join("b.png"), make_png(8)).unwrap();
    let src = open_source(&dir.path().join("a.png")).unwrap();
    assert_eq!(src.len(), 2);
}
