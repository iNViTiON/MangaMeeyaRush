//! Integration tests for folder + zip page sources.

use std::fs;
use std::io::Write;

use image::{ImageBuffer, Rgb};
use mmce_codecs::{decode_image, open_source};

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
        for (i, name) in ["page01.png", "page02.png", "page10.png"]
            .iter()
            .enumerate()
        {
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
    assert_eq!(
        src.len(),
        0,
        "library folders must not swallow chapter pages"
    );
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

#[test]
fn zip_source_supports_concurrent_reads() {
    use std::sync::Arc;
    use std::thread;

    let dir = tempfile::tempdir().unwrap();
    let zip_path = dir.path().join("concurrent.cbz");
    {
        let f = fs::File::create(&zip_path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for i in 0..16 {
            w.start_file(format!("p{i:02}.png"), opts).unwrap();
            w.write_all(&make_png(i as u8)).unwrap();
        }
        w.finish().unwrap();
    }

    let src: Arc<dyn mmce_codecs::PageSource> = Arc::from(open_source(&zip_path).unwrap());
    // Hammer the source from multiple threads to exercise the pool.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let s = src.clone();
        handles.push(thread::spawn(move || {
            for i in 0..16 {
                let bytes = s.read(i).expect("concurrent read");
                let img = decode_image(&bytes).expect("concurrent decode");
                assert_eq!(img.width(), 40);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

/// Write a 7z whose archive order is deliberately not natural order, with a
/// non-image and a directory mixed in. `solid` packs everything into one
/// block (the expensive case for random access).
fn write_7z(path: &std::path::Path, solid: bool) -> Vec<(String, Vec<u8>)> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
    let files: Vec<(String, Vec<u8>)> = vec![
        ("vol/10.png".into(), make_png(10)),
        ("vol/2.png".into(), make_png(2)),
        ("readme.txt".into(), b"not a page".to_vec()),
        ("vol/1.png".into(), make_png(1)),
    ];
    let mut w = ArchiveWriter::create(path).unwrap();
    w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory("vol"), None)
        .unwrap();
    if solid {
        let entries = files
            .iter()
            .map(|(n, _)| ArchiveEntry::new_file(n))
            .collect();
        let readers = files
            .iter()
            .map(|(_, d)| SourceReader::new(d.as_slice()))
            .collect();
        w.push_archive_entries(entries, readers).unwrap();
    } else {
        for (n, d) in &files {
            w.push_archive_entry(ArchiveEntry::new_file(n), Some(d.as_slice()))
                .unwrap();
        }
    }
    w.finish().unwrap();
    files
}

#[test]
fn sevenz_source_lists_naturally_sorted_images_and_reads_them() {
    for solid in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.cb7");
        let files = write_7z(&path, solid);

        let src = open_source(&path).unwrap();
        let names: Vec<_> = (0..src.len()).map(|i| src.entry_name(i).unwrap()).collect();
        assert_eq!(
            names,
            ["vol/1.png", "vol/2.png", "vol/10.png"],
            "solid={solid}"
        );
        assert_eq!(src.read(2).unwrap(), files[0].1, "solid={solid}");

        // The explorer cover is the book's first page, not the first image
        // in archive order (vol/10.png here).
        let cover = mmce_codecs::cover_image(&path).unwrap();
        assert_eq!(cover, files[3].1, "solid={solid}");
    }
}

#[test]
fn sevenz_open_does_not_decompress() {
    // Corrupt the packed data (it starts right after the 32-byte signature
    // header) but leave the end-of-file header intact. Listing pages must
    // still work: open reads the header only. Decompressing at open — as
    // enumeration used to — fails on this archive.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.cb7");
    write_7z(&path, true);
    let mut bytes = fs::read(&path).unwrap();
    for b in &mut bytes[64..256] {
        *b ^= 0x5a;
    }
    fs::write(&path, &bytes).unwrap();

    let src = open_source(&path).expect("open must not touch packed data");
    assert_eq!(src.len(), 3);

    // Reading the corrupt pages must error, not hang: sevenz-rust2's
    // multi-threaded LZMA2 decoder deadlocks here, which is why readers are
    // pinned to `set_thread_count(1)`. Watchdog so a regression fails
    // instead of wedging the test run.
    let src: std::sync::Arc<dyn mmce_codecs::PageSource> = std::sync::Arc::from(src);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send((0..src.len()).any(|i| src.read(i).is_err()));
    });
    let any_err = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("7z decode of corrupt data hung — keep set_thread_count(1)");
    assert!(any_err, "fixture should really be corrupt");
}
