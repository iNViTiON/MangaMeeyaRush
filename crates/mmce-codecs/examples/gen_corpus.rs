//! Synthetic manga-library corpus for explorer / reader benchmarks.
//!
//! ```sh
//! cargo run --release -p mmce-codecs --example gen_corpus -- <out_dir> [--scale F] [--seed N]
//! ```
//!
//! Shape (scale 1.0, ~14 GB of real bytes + sparse bulk):
//!
//! ```text
//! <out>/Library/
//!   Main/                    1062 zips, flat, real bytes (the worst case)
//!     <14 author folders>/   4-52 zips each, sparse filler pages
//!     Loose Pages .../       7 book folders x ~19 loose JPEGs (FolderSource)
//!   Zips A|B|C/              41 / 77 / 121 zips, sparse filler; B has a .rar,
//!                            C has the unreadable / malformed edge cases
//!   Loose JPEGs/             14 JPEGs
//!   <two Thai-named folders, one ending in !!>/  ~34 small JPEGs each
//!   Video Dump/              111 mp4 + 1 flv (sparse), 16 big PNG, 12 JPEG, 4 subfolders
//!   + 12 loose files         mp4, jpg, big png, .bat, .rar
//! ```
//!
//! Main-folder zips match a sampled real collection: pages p10 18 / p50 26 /
//! p90 106 / max 248, JPEGs ~275 KB (p10 174 / p90 454 KB) at ~1500 px, mixed
//! grayscale and colour; ~98% stored, ~2% deflated; ~2.5% with an internal
//! subfolder; a few PNGs. Archive sizes land at p50 ~7.6 MB / p90 ~32 MB /
//! max ~226 MB.
//!
//! "Sparse filler" pages are zero-filled holes (valid stored entries, correct
//! CRC, no disk blocks). Only the cover and page 2 are real, so explorer
//! scans read exactly what they would on a real file, but books in those
//! folders can't be read past page 2. Video files are sparse throughout —
//! the explorer never opens them.
//!
//! Page bytes come from a pool of pre-encoded images (calibrated to target
//! sizes) reused across archives, so generating 14 GB is pure I/O.

use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::{DynamicImage, GrayImage, ImageEncoder, RgbImage};

// ---------------------------------------------------------------------------
// Deterministic RNG (xorshift64*), no extra deps.

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: u64, hi_incl: u64) -> u64 {
        lo + self.next() % (hi_incl - lo + 1)
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
    fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
    /// Standard normal via Box-Muller.
    fn normal(&mut self) -> f64 {
        let u1 = self.f64().max(1e-12);
        let u2 = self.f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

// ---------------------------------------------------------------------------
// Synthetic page content: panels, line art, screentone, noise.

fn synth_gray(w: u32, h: u32, noise: u8, seed: u64) -> GrayImage {
    let mut r = Rng::new(seed);
    let mut img = GrayImage::from_pixel(w, h, image::Luma([245]));
    // Panels with screentone fills and line art.
    let panels = r.range(3, 6);
    let mut y = 20u32;
    for p in 0..panels {
        let ph = ((h - 40) / panels as u32).saturating_sub(12);
        let (x0, y0, x1, y1) = (20, y, w - 20, (y + ph).min(h - 20));
        y += ph + 12;
        let tone_period = r.range(4, 7) as u32;
        let tone_level = r.range(90, 200) as u8;
        // Screentone is the expensive part for JPEG; keep it to part of
        // some panels so noise alone can calibrate the size upward.
        let tone_frac = if r.chance(0.5) { r.f64() * 0.35 } else { 0.0 };
        for yy in y0..y1 {
            for xx in x0..x1 {
                let border = xx < x0 + 3 || xx >= x1 - 3 || yy < y0 + 3 || yy >= y1 - 3;
                let v = if border {
                    10
                } else if (xx as f64) < x0 as f64 + (x1 - x0) as f64 * tone_frac
                    && (xx % tone_period == 0 && yy % tone_period == 0)
                {
                    tone_level / 2
                } else {
                    245
                };
                img.put_pixel(xx, yy, image::Luma([v]));
            }
        }
        // Line art strokes.
        for _ in 0..(50 + p * 15) {
            let (ax, ay) = (
                r.range(x0 as u64, x1 as u64 - 1) as i64,
                r.range(y0 as u64, y1 as u64 - 1) as i64,
            );
            let len = r.range(10, 160) as i64;
            let (dx, dy) = (r.range(0, 200) as i64 - 100, r.range(0, 200) as i64 - 100);
            for s in 0..len {
                let px = ax + dx * s / 100;
                let py = ay + dy * s / 100;
                if px >= x0 as i64 && px < x1 as i64 && py >= y0 as i64 && py < y1 as i64 {
                    img.put_pixel(px as u32, py as u32, image::Luma([20]));
                }
            }
        }
    }
    if noise > 0 {
        for px in img.pixels_mut() {
            let n = (r.next() % (2 * noise as u64 + 1)) as i32 - noise as i32;
            px.0[0] = (px.0[0] as i32 + n).clamp(0, 255) as u8;
        }
    }
    img
}

fn synth_rgb(w: u32, h: u32, noise: u8, seed: u64) -> RgbImage {
    let g = synth_gray(w, h, 0, seed);
    let mut r = Rng::new(seed ^ 0xc0ffee);
    let (c0, c1) = (
        [
            r.range(40, 255) as f32,
            r.range(40, 255) as f32,
            r.range(40, 255) as f32,
        ],
        [
            r.range(40, 255) as f32,
            r.range(40, 255) as f32,
            r.range(40, 255) as f32,
        ],
    );
    RgbImage::from_fn(w, h, |x, y| {
        let t = (x + y) as f32 / (w + h) as f32;
        let l = g.get_pixel(x, y).0[0] as f32 / 255.0;
        let n = if noise > 0 {
            (r.next() % (2 * noise as u64 + 1)) as f32 - noise as f32
        } else {
            0.0
        };
        let ch = |i: usize| ((c0[i] * (1.0 - t) + c1[i] * t) * l + n).clamp(0.0, 255.0) as u8;
        image::Rgb([ch(0), ch(1), ch(2)])
    })
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(img)
        .unwrap();
    out
}

fn encode_png(img: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            img.as_bytes(),
            img.width(),
            img.height(),
            img.color().into(),
        )
        .unwrap();
    out
}

/// Encode a page whose JPEG size lands near `target` bytes by bisecting the
/// noise amplitude.
fn calibrated_jpeg(w: u32, h: u32, color: bool, target: usize, seed: u64) -> Vec<u8> {
    let make = |noise: u8| {
        let img = if color {
            DynamicImage::ImageRgb8(synth_rgb(w, h, noise, seed))
        } else {
            DynamicImage::ImageLuma8(synth_gray(w, h, noise, seed))
        };
        encode_jpeg(&img, 85)
    };
    let (mut lo, mut hi) = (0u8, 96u8);
    let mut best: Option<Vec<u8>> = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let out = make(mid);
        let below = out.len() < target;
        if best
            .as_ref()
            .is_none_or(|b| out.len().abs_diff(target) < b.len().abs_diff(target))
        {
            best = Some(out);
        }
        if below {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    best.unwrap_or_else(|| make(lo))
}

// ---------------------------------------------------------------------------
// Page pool.

#[derive(Clone)]
struct Blob {
    data: Arc<Vec<u8>>,
    /// Raw-deflate payload, for the ~2% of zips that compress entries.
    deflated: Arc<Vec<u8>>,
    crc: u32,
    ext: &'static str,
}

impl Blob {
    fn new(data: Vec<u8>, ext: &'static str) -> Self {
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&data).unwrap();
        Self {
            crc: crc.sum(),
            deflated: Arc::new(enc.finish().unwrap()),
            data: Arc::new(data),
            ext,
        }
    }
    fn len(&self) -> usize {
        self.data.len()
    }
}

struct Pool {
    gray: Vec<Blob>,
    color: Vec<Blob>,
    covers: Vec<Blob>,
    png_pages: Vec<Blob>,
    big_png: Vec<Blob>,
    big_jpg: Vec<Blob>,
}

impl Pool {
    /// Pool page whose size is closest to `target`, with some jitter so
    /// consecutive picks differ.
    fn near<'a>(set: &'a [Blob], target: usize, r: &mut Rng) -> &'a Blob {
        let mut best: Vec<&Blob> = set.iter().collect();
        best.sort_by_key(|b| b.len().abs_diff(target));
        best[(r.next() % 3.min(best.len() as u64)) as usize]
    }
}

fn par_map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let n = std::thread::available_parallelism().map_or(4, |n| n.get());
    let items: Vec<(usize, T)> = items.into_iter().enumerate().collect();
    let queue = std::sync::Mutex::new(items);
    let out = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..n {
            s.spawn(|| loop {
                let Some((i, t)) = queue.lock().unwrap().pop() else {
                    break;
                };
                let r = f(t);
                out.lock().unwrap().push((i, r));
            });
        }
    });
    let mut out = out.into_inner().unwrap();
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

fn build_pool(seed: u64) -> Pool {
    let t = std::time::Instant::now();
    let dims = [
        (1056u32, 1500u32),
        (1100, 1560),
        (1200, 1700),
        (960, 1360),
        (1064, 1508),
    ];
    // Gray pages: log-spaced 95 KB..1 MB, denser around the 175-450 KB body.
    let gray_targets: Vec<usize> = (0..40)
        .map(|i| {
            let f = i as f64 / 39.0;
            let kb = if f < 0.1 {
                95.0 + f / 0.1 * 80.0
            } else if f < 0.9 {
                175.0 * (454.0f64 / 175.0).powf((f - 0.1) / 0.8)
            } else {
                454.0 + (f - 0.9) / 0.1 * 550.0
            };
            (kb * 1024.0) as usize
        })
        .collect();
    let jobs: Vec<(usize, usize)> = gray_targets.into_iter().enumerate().collect();
    let gray = par_map(jobs, |(i, target)| {
        let (w, h) = dims[i % dims.len()];
        Blob::new(
            calibrated_jpeg(w, h, false, target, seed ^ (i as u64) << 8),
            "jpg",
        )
    });
    let color_jobs: Vec<(usize, usize)> = (0..24)
        .map(|i| (i, ((160.0 + 900.0 * i as f64 / 23.0) * 1024.0) as usize))
        .collect();
    let color = par_map(color_jobs, |(i, target)| {
        let (w, h) = dims[(i + 2) % dims.len()];
        Blob::new(
            calibrated_jpeg(w, h, true, target, seed ^ 0x51 ^ (i as u64) << 9),
            "jpg",
        )
    });
    let cover_jobs: Vec<(usize, usize)> = (0..16)
        .map(|i| (i, ((220.0 + 700.0 * i as f64 / 15.0) * 1024.0) as usize))
        .collect();
    let covers = par_map(cover_jobs, |(i, target)| {
        let (w, h) = dims[(i + 1) % dims.len()];
        Blob::new(
            calibrated_jpeg(w, h, true, target, seed ^ 0xc0 ^ (i as u64) << 10),
            "jpg",
        )
    });
    let png_pages = par_map((0..3).collect(), |i: u64| {
        let img = DynamicImage::ImageLuma8(synth_gray(1056, 1500, 6, seed ^ 0x77 ^ i));
        Blob::new(encode_png(&img), "png")
    });
    // Big PNG / JPEG photos (3-8 MB): the heavy-decode loose-file case.
    let big_png = par_map((0..8).collect(), |i: u64| {
        let (w, h) = (2000 + (i as u32 % 3) * 200, 1500 + (i as u32 % 2) * 150);
        let img = DynamicImage::ImageRgb8(synth_rgb(
            w,
            h,
            [3, 6, 10, 16][i as usize % 4],
            seed ^ 0xb16 ^ i,
        ));
        Blob::new(encode_png(&img), "png")
    });
    let big_jpg = par_map((0..4).collect(), |i: u64| {
        let img = DynamicImage::ImageRgb8(synth_rgb(
            3200,
            2400,
            [8, 14, 20, 28][i as usize],
            seed ^ 0xb17 ^ i,
        ));
        Blob::new(encode_jpeg(&img, 92), "jpg")
    });
    let kb = |s: &[Blob]| {
        let mut v: Vec<usize> = s.iter().map(|b| b.len() / 1024).collect();
        v.sort();
        format!("{}..{} KB", v[0], v[v.len() - 1])
    };
    eprintln!(
        "pool: gray {} | color {} | covers {} | png pages {} | big png {} | big jpg {} ({:.1}s)",
        kb(&gray),
        kb(&color),
        kb(&covers),
        kb(&png_pages),
        kb(&big_png),
        kb(&big_jpg),
        t.elapsed().as_secs_f64()
    );
    Pool {
        gray,
        color,
        covers,
        png_pages,
        big_png,
        big_jpg,
    }
}

// ---------------------------------------------------------------------------
// Minimal ZIP writer: stored or deflated entries, optional sparse (hole)
// entries, UTF-8 names. No zip64 (archives stay well under 4 GiB).

enum Payload<'a> {
    Blob(&'a Blob),
    /// `len` zero bytes written as a hole.
    Hole(usize),
}

struct ZipOut {
    f: BufWriter<File>,
    pos: u64,
    central: Vec<u8>,
    count: u16,
    deflate: bool,
}

const DOS_TIME: u16 = (12 << 11) | (30 << 5);
const DOS_DATE: u16 = ((2016 - 1980) << 9) | (8 << 5) | 14;

impl ZipOut {
    fn create(path: &Path, deflate: bool) -> std::io::Result<Self> {
        Ok(Self {
            f: BufWriter::with_capacity(1 << 20, File::create(path)?),
            pos: 0,
            central: Vec::new(),
            count: 0,
            deflate,
        })
    }

    fn header(
        &mut self,
        name: &str,
        method: u16,
        crc: u32,
        csize: u32,
        usize_: u32,
        ext_attr: u32,
    ) -> std::io::Result<()> {
        let offset = self.pos as u32;
        let flags: u16 = 1 << 11; // UTF-8 names
        let version: u16 = if method == 8 { 20 } else { 10 };
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        for v in [version, flags, method, DOS_TIME, DOS_DATE] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&csize.to_le_bytes());
        h.extend_from_slice(&usize_.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        self.f.write_all(&h)?;
        self.pos += h.len() as u64;

        let c = &mut self.central;
        c.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        for v in [0x031e_u16, version, flags, method, DOS_TIME, DOS_DATE] {
            c.extend_from_slice(&v.to_le_bytes());
        }
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&csize.to_le_bytes());
        c.extend_from_slice(&usize_.to_le_bytes());
        c.extend_from_slice(&(name.len() as u16).to_le_bytes());
        for v in [0u16, 0, 0, 0] {
            c.extend_from_slice(&v.to_le_bytes()); // extra, comment, disk, int attr
        }
        c.extend_from_slice(&ext_attr.to_le_bytes());
        c.extend_from_slice(&offset.to_le_bytes());
        c.extend_from_slice(name.as_bytes());
        self.count += 1;
        Ok(())
    }

    fn dir(&mut self, name: &str) -> std::io::Result<()> {
        self.header(name, 0, 0, 0, 0, (0o040755 << 16) | 0x10)
    }

    fn file(&mut self, name: &str, payload: Payload) -> std::io::Result<()> {
        let attr = 0o100644 << 16;
        match payload {
            Payload::Blob(b) if self.deflate => {
                self.header(
                    name,
                    8,
                    b.crc,
                    b.deflated.len() as u32,
                    b.len() as u32,
                    attr,
                )?;
                self.f.write_all(&b.deflated)?;
                self.pos += b.deflated.len() as u64;
            }
            Payload::Blob(b) => {
                self.header(name, 0, b.crc, b.len() as u32, b.len() as u32, attr)?;
                self.f.write_all(&b.data)?;
                self.pos += b.len() as u64;
            }
            Payload::Hole(len) => {
                let crc = zeros_crc(len);
                self.header(name, 0, crc, len as u32, len as u32, attr)?;
                self.f.seek(SeekFrom::Current(len as i64))?;
                self.pos += len as u64;
            }
        }
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<u64> {
        let cd_off = self.pos as u32;
        self.f.write_all(&self.central)?;
        let mut e = Vec::with_capacity(22);
        e.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        for v in [0u16, 0, self.count, self.count] {
            e.extend_from_slice(&v.to_le_bytes());
        }
        e.extend_from_slice(&(self.central.len() as u32).to_le_bytes());
        e.extend_from_slice(&cd_off.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        self.f.write_all(&e)?;
        self.f.flush()?;
        Ok(self.pos + self.central.len() as u64 + 22)
    }
}

fn zeros_crc(len: usize) -> u32 {
    static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];
    let mut crc = flate2::Crc::new();
    let mut left = len;
    while left > 0 {
        let n = left.min(ZEROS.len());
        crc.update(&ZEROS[..n]);
        left -= n;
    }
    crc.sum()
}

/// Sparse file of `len` bytes starting with `magic`.
fn sparse_file(path: &Path, magic: &[u8], len: u64) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(magic)?;
    f.set_len(len)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Names: long romanised titles, circle/author tags, event codes, volume runs.

const CIRCLES: &[&str] = &[
    "Akane-dou",
    "Shiroi Neko Koubou",
    "Yuuhi no Mori",
    "Kumo Kumo Club",
    "Hoshikuzu Drops",
    "Studio Tsukimi",
    "Nanairo Lab",
    "Kaze no Machi",
    "Mikan Factory",
    "Ginga Kissa",
    "Usagi Paradise",
    "Kurogane Works",
    "Hanabi-ya",
    "Momiji Garden",
    "Soramimi Teishoku",
    "Natsukusa Press",
    "Yume no Kakera",
    "Tsubasa Engine",
    "Koharu Biyori",
    "Ameiro Records",
];
const AUTHORS: &[&str] = &[
    "Kanzaki Ren",
    "Mizuno Haru",
    "Shinonome Aki",
    "Tachibana Yuu",
    "Hayase Mio",
    "Kurosawa Jin",
    "Amamiya Sora",
    "Fujisaki Rin",
    "Kisaragi Nao",
    "Hoshino Kei",
    "Sakuraba Itsuki",
    "Minase Kaoru",
    "Asahina Ryo",
    "Yukimura Sei",
    "Takanashi Hotaru",
    "Ichinose Tomo",
];
const WORDS: &[&str] = &[
    "Hoshizora",
    "Kanata",
    "Natsuiro",
    "Memories",
    "Sakura",
    "Overture",
    "Kimi to Boku",
    "Tsuki ga Kirei",
    "Afterglow",
    "Lemonade",
    "Seaside",
    "Hanasaku",
    "Iroha",
    "Shoujo",
    "Rhapsody",
    "Kiseki",
    "Yuugure",
    "Holiday",
    "Daydream",
    "Sekai no Owari",
    "Himitsu",
    "Koi Moyou",
    "Tasogare",
    "Sweet",
    "Days",
    "Story",
    "Lovers",
    "Garden",
    "Melody",
    "Shiori",
    "Hikari",
    "Kaze",
    "Umi",
    "Sora",
    "Yoru",
    "Asa",
    "Hajimete no",
    "Futari no",
    "Ano Hi no",
];
const EVENTS: &[&str] = &[
    "(C87)",
    "(C88)",
    "(C89)",
    "(C90)",
    "(C92)",
    "(C95)",
    "(C97)",
    "(C100)",
    "(C102)",
    "(COMIC1☆9)",
    "(COMIC1☆13)",
    "(Reitaisai 12)",
    "(SC2016 Summer)",
    "(Gataket 140)",
    "(Mimiket 38)",
    "(Kemoket 7)",
];
const TAILS: &[&str] = &[
    "[English]",
    "[English] [Team Hoshi]",
    "[English] [Digital]",
    "[Digital]",
    "[Chinese]",
    "[English] [Decensored]",
    "[English] [Ongoing]",
    "",
];

fn title(r: &mut Rng) -> String {
    let n = r.range(2, 4);
    let mut t: Vec<&str> = (0..n).map(|_| *r.pick(WORDS)).collect();
    t.dedup();
    let mut s = t.join(" ");
    if r.chance(0.35) {
        s = format!("{s} ~{} {}~", r.pick(WORDS), r.pick(WORDS));
    }
    if r.chance(0.2) {
        s.push_str(r.pick(&["!", "!!", "?", "♪", "...", "!?"]));
    }
    s
}

fn tag(r: &mut Rng) -> String {
    if r.chance(0.7) {
        format!("[{} ({})]", r.pick(CIRCLES), r.pick(AUTHORS))
    } else {
        format!("[{}]", r.pick(AUTHORS))
    }
}

fn volume(style: u64, v: usize) -> String {
    match style {
        0 => format!("- {v:03} -"),
        1 => format!("Vol. {v}"),
        2 => format!("Ch. {v:02}"),
        3 => format!("Part {v}"),
        4 => format!("#{v}"),
        _ => format!("{v}"),
    }
}

fn clamp_name(mut s: String, ext: &str) -> String {
    // ext4 / btrfs limit names to 255 bytes.
    while s.len() + ext.len() > 250 {
        s.pop();
    }
    format!("{}{ext}", s.trim_end())
}

/// `count` archive names: series runs (up to 24 volumes) and one-shots.
fn archive_names(r: &mut Rng, count: usize, ext_mix: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let run = if r.chance(0.45) {
            1
        } else {
            (r.range(2, 8) + if r.chance(0.2) { r.range(8, 16) } else { 0 }) as usize
        };
        let (tg, ti, style) = (tag(r), title(r), r.range(0, 5));
        let ev = r.pick(EVENTS).to_string();
        let tail = r.pick(TAILS).to_string();
        for v in 1..=run.min(count - out.len()) {
            let vol = if run == 1 {
                String::new()
            } else {
                format!(" {}", volume(style, v))
            };
            let ext = if ext_mix && r.chance(0.3) {
                ".cbz"
            } else {
                ".zip"
            };
            let name = format!("{ev} {tg} {ti}{vol} {tail}");
            out.push(clamp_name(
                name.split_whitespace().collect::<Vec<_>>().join(" "),
                ext,
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Archive planning.

struct Plan {
    pages: usize,
    avg_page: usize,
    color_pages: bool,
    deflate: bool,
    subfolder: bool,
    png_cover: bool,
    png_pages: usize,
    naming: u64,
}

fn sample_pages(r: &mut Rng) -> usize {
    let u = r.f64();
    let lerp = |a: f64, b: f64, t: f64| (a + (b - a) * t).round() as usize;
    if u < 0.10 {
        lerp(6.0, 18.0, u / 0.10)
    } else if u < 0.50 {
        lerp(18.0, 26.0, (u - 0.10) / 0.40)
    } else if u < 0.90 {
        lerp(26.0, 106.0, ((u - 0.50) / 0.40).powf(1.6))
    } else {
        lerp(106.0, 200.0, (u - 0.90) / 0.10)
    }
}

fn plan_main(r: &mut Rng, count: usize) -> Vec<Plan> {
    let mut plans: Vec<Plan> = (0..count)
        .map(|_| {
            // Lognormal: median 262 KB, p10 ~174 KB, p90 ~454 KB.
            let avg = (262.0 * (0.33 * r.normal()).exp() * 1024.0) as usize;
            Plan {
                pages: sample_pages(r),
                avg_page: avg.clamp(90 << 10, 1000 << 10),
                color_pages: r.chance(0.08),
                deflate: r.chance(0.02),
                subfolder: r.chance(0.025),
                png_cover: false,
                png_pages: 0,
                naming: r.range(0, 4),
            }
        })
        .collect();
    // Pinned outliers: the max (248 colour pages ≈ 226 MB), a 248-page
    // gray book for reading tests, and the tiny 0.6 MB archives.
    plans[count / 3] = Plan {
        pages: 248,
        avg_page: 910 << 10,
        color_pages: true,
        deflate: false,
        subfolder: false,
        png_cover: false,
        png_pages: 0,
        naming: 0,
    };
    plans[count / 2] = Plan {
        pages: 248,
        avg_page: 270 << 10,
        color_pages: false,
        deflate: false,
        subfolder: false,
        png_cover: false,
        png_pages: 0,
        naming: 0,
    };
    for k in 0..3 {
        let p = &mut plans[(k * 97 + 11) % count];
        p.pages = 5;
        p.avg_page = 118 << 10;
    }
    // A few PNG covers and PNG pages (~0.1% of images).
    for k in 0..4 {
        plans[(k * 211 + 40) % count].png_cover = true;
    }
    for k in 0..8 {
        plans[(k * 127 + 63) % count].png_pages = 6;
    }
    plans
}

fn page_name(style: u64, i: usize, n: usize, ext: &str) -> String {
    match style {
        0 => format!("{:03}.{ext}", i + 1),
        1 => format!("p{:02}.{ext}", i + 1),
        2 => format!("{}.{ext}", i + 1), // unpadded: natural sort matters
        _ => format!("Scan_{:0w$}.{ext}", i + 1, w = n.to_string().len().max(2)),
    }
}

/// Write one archive from a plan. `sparse_after` = number of real pages;
/// the rest are holes (`usize::MAX` = all real).
fn write_archive(
    path: &Path,
    plan: &Plan,
    pool: &Pool,
    r: &mut Rng,
    sparse_after: usize,
) -> std::io::Result<u64> {
    let mut z = ZipOut::create(path, plan.deflate)?;
    let prefix = if plan.subfolder {
        let d = format!(
            "{}/",
            path.file_stem()
                .unwrap()
                .to_string_lossy()
                .chars()
                .take(40)
                .collect::<String>()
                .trim()
        );
        z.dir(&d)?;
        d
    } else {
        String::new()
    };
    for i in 0..plan.pages {
        let blob = if i == 0 {
            if plan.png_cover {
                r.pick(&pool.png_pages)
            } else if r.chance(0.7) {
                Pool::near(&pool.covers, plan.avg_page * 13 / 10, r)
            } else {
                Pool::near(&pool.gray, plan.avg_page, r)
            }
        } else if i >= plan.pages - plan.png_pages.min(plan.pages - 1) {
            r.pick(&pool.png_pages)
        } else if plan.color_pages {
            Pool::near(&pool.color, plan.avg_page, r)
        } else {
            Pool::near(&pool.gray, plan.avg_page, r)
        };
        let name = format!(
            "{prefix}{}",
            page_name(plan.naming, i, plan.pages, blob.ext)
        );
        if i < sparse_after {
            z.file(&name, Payload::Blob(blob))?;
        } else {
            z.file(&name, Payload::Hole(blob.len()))?;
        }
    }
    z.finish()
}

fn quantiles(mut v: Vec<f64>) -> String {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    format!(
        "min {:.1} p10 {:.1} p50 {:.1} p90 {:.1} max {:.1}",
        v[0],
        q(0.1),
        q(0.5),
        q(0.9),
        v[v.len() - 1]
    )
}

// ---------------------------------------------------------------------------

fn write_loose_jpegs(
    dir: &Path,
    n: usize,
    pool: &[Blob],
    r: &mut Rng,
    names: impl Fn(usize) -> String,
) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    for i in 0..n {
        fs::write(dir.join(names(i)), &*r.pick(pool).data)?;
    }
    Ok(())
}

fn zip_folder(
    dir: &Path,
    count: usize,
    total_mb: f64,
    pool: &Pool,
    r: &mut Rng,
) -> std::io::Result<u64> {
    fs::create_dir_all(dir)?;
    let names = archive_names(r, count, true);
    let avg_bytes = total_mb * 1e6 / count as f64;
    let mut total = 0;
    for name in names {
        let target = (avg_bytes * (0.6 * r.normal()).exp()).max(0.6e6);
        let avg_page = (r.range(200, 360) as usize) << 10;
        let plan = Plan {
            pages: ((target / avg_page as f64).round() as usize).clamp(4, 248),
            avg_page,
            color_pages: false,
            deflate: false,
            subfolder: r.chance(0.025),
            png_cover: false,
            png_pages: 0,
            naming: r.range(0, 3),
        };
        total += write_archive(&dir.join(name), &plan, pool, r, 2)?;
    }
    Ok(total)
}

fn edge_cases(dir: &Path, pool: &Pool, r: &mut Rng) -> std::io::Result<()> {
    let good = Plan {
        pages: 8,
        avg_page: 260 << 10,
        color_pages: false,
        deflate: false,
        subfolder: false,
        png_cover: false,
        png_pages: 0,
        naming: 0,
    };
    // Truncated: central directory missing.
    let tmp = dir.join("zz_edge truncated.zip");
    write_archive(&tmp, &good, pool, r, usize::MAX)?;
    let bytes = fs::read(&tmp)?;
    fs::write(&tmp, &bytes[..bytes.len() * 6 / 10])?;
    fs::write(dir.join("zz_edge empty.cbz"), b"")?;
    fs::write(
        dir.join("zz_edge not a zip.zip"),
        b"This is a text file with a .zip extension.\n",
    )?;
    // Cover is SOI + garbage (libjpeg must not abort the process).
    let mut z = ZipOut::create(&dir.join("zz_edge corrupt cover.zip"), false)?;
    let mut junk = vec![0xFF, 0xD8, 0xFF, 0xE0];
    junk.extend((0..200_000).map(|_| r.next() as u8));
    z.file("001.jpg", Payload::Blob(&Blob::new(junk, "jpg")))?;
    z.file("002.jpg", Payload::Blob(r.pick(&pool.gray)))?;
    z.finish()?;
    // Cover is a zero-byte file.
    let mut z = ZipOut::create(&dir.join("zz_edge empty cover.zip"), false)?;
    z.file("001.jpg", Payload::Blob(&Blob::new(Vec::new(), "jpg")))?;
    z.file("002.jpg", Payload::Blob(r.pick(&pool.gray)))?;
    z.finish()?;
    // No images at all; directories only; hostile entry names.
    let mut z = ZipOut::create(&dir.join("zz_edge no images.zip"), false)?;
    z.file(
        "readme.txt",
        Payload::Blob(&Blob::new(b"nothing to see".to_vec(), "txt")),
    )?;
    z.finish()?;
    let mut z = ZipOut::create(&dir.join("zz_edge dirs only.zip"), false)?;
    z.dir("a/")?;
    z.dir("a/b/")?;
    z.finish()?;
    let mut z = ZipOut::create(&dir.join("zz_edge hostile names.zip"), false)?;
    z.file("../escape.jpg", Payload::Blob(r.pick(&pool.gray)))?;
    z.file("/abs.jpg", Payload::Blob(r.pick(&pool.gray)))?;
    z.file("ok/002.jpg", Payload::Blob(r.pick(&pool.gray)))?;
    z.finish()?;
    // JPEG header claiming 60000x60000 with a truncated body: a decoder
    // that trusts the header allocates ~10 GB.
    let mut bomb = encode_jpeg(&DynamicImage::ImageLuma8(GrayImage::new(64, 64)), 80);
    if let Some(p) = bomb.windows(2).position(|w| w == [0xFF, 0xC0]) {
        bomb[p + 5..p + 7].copy_from_slice(&60000u16.to_be_bytes());
        bomb[p + 7..p + 9].copy_from_slice(&60000u16.to_be_bytes());
    }
    let mut z = ZipOut::create(&dir.join("zz_edge huge dims cover.zip"), false)?;
    z.file("001.jpg", Payload::Blob(&Blob::new(bomb, "jpg")))?;
    z.finish()?;
    Ok(())
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let out = PathBuf::from(
        args.get(1)
            .expect("usage: gen_corpus <out_dir> [--scale F] [--seed N]"),
    );
    let opt = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
    };
    let scale: f64 = opt("--scale").map_or(1.0, |s| s.parse().unwrap());
    let seed: u64 = opt("--seed").map_or(87, |s| s.parse().unwrap());
    let sc = |n: usize| ((n as f64 * scale).round() as usize).max(1);
    let t0 = std::time::Instant::now();

    let lib = out.join("Library");
    let main_dir = lib.join("Main");
    fs::create_dir_all(&main_dir)?;
    let pool = build_pool(seed);
    let mut r = Rng::new(seed);

    // 1. Main flat folder (real bytes).
    let count = sc(1062);
    let names = archive_names(&mut r, count, false);
    let plans = plan_main(&mut r, count);
    let jobs: Vec<(usize, String)> = names.into_iter().enumerate().collect();
    let results = par_map(jobs, |(i, name)| {
        let mut rr = Rng::new(seed ^ 0xa11 ^ (i as u64) << 16);
        let size =
            write_archive(&main_dir.join(&name), &plans[i], &pool, &mut rr, usize::MAX).unwrap();
        (size, plans[i].pages)
    });
    let total: u64 = results.iter().map(|(s, _)| s).sum();
    let pages: usize = results.iter().map(|(_, p)| p).sum();
    eprintln!(
        "Main: {count} zips, {:.2} GB, avg page {:.0} KB\n  archive MB: {}\n  pages: {}",
        total as f64 / 1e9,
        total as f64 / pages as f64 / 1024.0,
        quantiles(results.iter().map(|(s, _)| *s as f64 / 1e6).collect()),
        quantiles(results.iter().map(|(_, p)| *p as f64).collect()),
    );

    // 2. Author folders (sparse filler).
    let authors: [(usize, f64); 14] = [
        (4, 30.0),
        (5, 38.0),
        (8, 70.0),
        (12, 110.0),
        (17, 160.0),
        (21, 210.0),
        (26, 240.0),
        (33, 300.0),
        (39, 380.0),
        (45, 420.0),
        (50, 250.0),
        (50, 520.0),
        (51, 780.0),
        (52, 610.0),
    ];
    for (i, (n, mb)) in authors.iter().enumerate() {
        let name = format!(
            "[{}] {}",
            AUTHORS[i % AUTHORS.len()],
            if i % 3 == 0 { "Collected Works" } else { "" }
        );
        let dir = main_dir.join(name.trim());
        let bytes = zip_folder(&dir, sc(*n), mb * scale, &pool, &mut r)?;
        eprintln!(
            "  {}: {} zips, {:.0} MB apparent",
            dir.file_name().unwrap().to_string_lossy(),
            sc(*n),
            bytes as f64 / 1e6
        );
    }

    // 3. Loose-image book folders, depth 2 (real bytes).
    let loose = main_dir.join("Loose Pages ~ Scanned Books");
    for b in 0..7 {
        let pages = [19, 18, 20, 19, 21, 18, 19][b];
        write_loose_jpegs(
            &loose.join(format!("Book {} ~ {}", b + 1, title(&mut r))),
            pages,
            &pool.gray[20..36],
            &mut r,
            |i| format!("{:03}.jpg", i + 1),
        )?;
    }

    // 4. Parent folder: zip-only folders, loose images, Thai names, video dump, loose noise.
    let a = zip_folder(&lib.join("Zips A"), sc(41), 520.0 * scale, &pool, &mut r)?;
    let b = zip_folder(&lib.join("Zips B"), sc(77), 900.0 * scale, &pool, &mut r)?;
    sparse_file(
        &lib.join("Zips B")
            .join("Some Old Release (unpacked never).rar"),
        b"Rar!\x1a\x07\x00",
        22_000_000,
    )?;
    let c = zip_folder(&lib.join("Zips C"), sc(121), 1300.0 * scale, &pool, &mut r)?;
    edge_cases(&lib.join("Zips C"), &pool, &mut r)?;
    eprintln!(
        "Zips A/B/C: {:.0} / {:.0} / {:.0} MB apparent",
        a as f64 / 1e6,
        b as f64 / 1e6,
        c as f64 / 1e6
    );

    write_loose_jpegs(
        &lib.join("Loose JPEGs"),
        14,
        &pool.covers[8..],
        &mut r,
        |i| format!("IMG_{:04}.jpg", 2041 + i),
    )?;
    write_loose_jpegs(
        &lib.join("การ์ตูนรวมเล่ม ฉบับพิเศษ เล่มที่หนึ่ง"),
        33,
        &pool.gray[24..34],
        &mut r,
        |i| format!("หน้า {:02}.jpg", i + 1),
    )?;
    write_loose_jpegs(
        &lib.join("มังงะสุดฮา ภาคพิเศษ รวมตอนจบ!!"),
        35,
        &pool.gray[24..34],
        &mut r,
        |i| format!("{:03}.jpg", i + 1),
    )?;

    let video = lib.join("Video Dump");
    for sub in ["2019", "2020", "misc clips", "เก่า"] {
        fs::create_dir_all(video.join(sub))?;
        for k in 0..3 {
            sparse_file(
                &video.join(sub).join(format!("clip_{k:02}.mp4")),
                b"\0\0\0\x20ftypisom",
                50_000_000 + k * 30_000_000,
            )?;
        }
    }
    let mut vbytes = 0u64;
    for k in 0..111 - 12 {
        let extra = if k % 17 == 0 { 600_000_000 } else { 0 };
        let len = r.range(20, 300) * 1_000_000 + extra;
        sparse_file(
            &video.join(format!("video_{k:03}.mp4")),
            b"\0\0\0\x20ftypisom",
            len,
        )?;
        vbytes += len;
    }
    sparse_file(&video.join("old_capture.flv"), b"FLV\x01", 700_000_000)?;
    for k in 0..16 {
        fs::write(
            video.join(format!("screenshot_{k:02}.png")),
            &*pool.big_png[k % pool.big_png.len()].data,
        )?;
    }
    for k in 0..12 {
        fs::write(
            video.join(format!("frame_{k:02}.jpg")),
            &*pool.big_jpg[k % pool.big_jpg.len()].data,
        )?;
    }
    eprintln!(
        "Video Dump: ~{:.1} GB apparent (sparse)",
        vbytes as f64 / 1e9
    );

    sparse_file(&lib.join("trailer.mp4"), b"\0\0\0\x20ftypisom", 310_000_000)?;
    for k in 0..4 {
        fs::write(
            lib.join(format!("wallpaper_{k}.jpg")),
            &*pool.big_jpg[k].data,
        )?;
        fs::write(
            lib.join(format!("poster_{k}.png")),
            &*pool.big_png[k + 4].data,
        )?;
    }
    fs::write(
        lib.join("rename_all.bat"),
        b"@echo off\r\nfor %%f in (*.cbz) do ren \"%%f\" \"%%~nf.zip\"\r\n",
    )?;
    sparse_file(
        &lib.join("Archive Pack Vol 1-3.rar"),
        b"Rar!\x1a\x07\x00",
        22_000_000,
    )?;

    eprintln!(
        "done in {:.1}s → {}",
        t0.elapsed().as_secs_f64(),
        lib.display()
    );
    Ok(())
}
