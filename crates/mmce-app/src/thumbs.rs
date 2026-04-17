//! Thumbnail cache for the explorer view.
//!
//! One background worker decodes cover thumbnails for any path (folder,
//! archive, or loose image). The UI calls `thumbnail(path)` which returns a
//! texture handle if one is cached; otherwise the path is queued for the
//! worker and a spinner can be drawn in the meantime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use image::imageops::FilterType;
use image::GenericImageView;
use mmce_codecs::{decode_image, is_archive_path, is_image_path, open_source};


struct Decoded {
    size: [usize; 2],
    pixels: Vec<u8>,
}

enum Msg {
    Request { path: PathBuf, thumb_size: u32, gen: u64 },
    Shutdown,
}

struct Inner {
    pending: HashMap<PathBuf, ()>,
    /// Freshly decoded items waiting to be promoted to textures on the UI
    /// thread. This is a *staging buffer*: the UI drains it on every
    /// repaint, so we don't cap it — capping here races UI drainage and
    /// causes visible tiles to bounce between Ready → Pending.
    decoded: HashMap<PathBuf, Option<Decoded>>, // None = failed
}

impl Inner {
    fn put(&mut self, path: PathBuf, d: Option<Decoded>) {
        self.decoded.insert(path, d);
    }
}

struct TexEntry {
    handle: Option<TextureHandle>,
    sort: u64,
}

struct TexStore {
    map: HashMap<PathBuf, TexEntry>,
    counter: u64,
    capacity: usize,
}

impl TexStore {
    fn get(&mut self, path: &Path) -> Option<&TexEntry> {
        if !self.map.contains_key(path) {
            return None;
        }
        self.counter += 1;
        let c = self.counter;
        self.map.get_mut(path).map(|e| { e.sort = c; &*e })
    }

    fn insert(&mut self, path: PathBuf, handle: Option<TextureHandle>) {
        self.counter += 1;
        let entry = TexEntry { handle, sort: self.counter };
        if self.map.len() >= self.capacity && !self.map.contains_key(&path) {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.sort)
                .map(|(p, _)| p.clone())
            {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(path, entry);
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

pub struct ThumbnailCache {
    ctx: Context,
    inner: Arc<Mutex<Inner>>,
    tex: Mutex<TexStore>,
    tx: Sender<Msg>,
    _workers: Vec<thread::JoinHandle<()>>,
    /// Tracks how many times `clear()` has run — worker results from a
    /// previous generation are discarded so the fresh cache only contains
    /// the new thumb size.
    gen: Arc<Mutex<u64>>,
}

impl ThumbnailCache {
    pub fn new(ctx: Context, capacity: usize) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            pending: HashMap::new(),
            decoded: HashMap::new(),
        }));
        let (tx, rx) = mpsc::channel();
        let rx = Arc::new(Mutex::new(rx));
        let gen = Arc::new(Mutex::new(0u64));
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        let workers = (0..n)
            .map(|i| spawn_worker(i, rx.clone(), inner.clone(), ctx.clone(), gen.clone()))
            .collect();
        Self {
            ctx,
            inner,
            tex: Mutex::new(TexStore {
                map: HashMap::new(),
                counter: 0,
                // Large cap because eviction below a few thousand starts to
                // churn for big libraries (we only ever call thumbnail()
                // for visible tiles now, but users can still scroll a lot).
                capacity: capacity.max(4096),
            }),
            tx,
            _workers: workers,
            gen,
        }
    }

    /// Invalidate all cached thumbnails — used when the tile/thumb size
    /// changes and we need to re-decode at the new resolution.
    pub fn clear(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.decoded.clear();
            g.pending.clear();
        }
        self.tex.lock().unwrap().clear();
        *self.gen.lock().unwrap() += 1;
    }

    /// Returns a texture for `path` if decoded; otherwise schedules a decode
    /// and returns `Pending`. `Failed` means the path couldn't decode — the
    /// UI should fall back to an icon.
    pub fn thumbnail(&self, path: &Path, thumb_size: u32) -> ThumbStatus {
        if let Some(entry) = self.tex.lock().unwrap().get(path) {
            return match &entry.handle {
                Some(t) => ThumbStatus::Ready(t.clone()),
                None => ThumbStatus::Failed,
            };
        }

        let decoded_state: Option<Option<Decoded>> = {
            let mut g = self.inner.lock().unwrap();
            g.decoded.remove(path)
        };
        if let Some(state) = decoded_state {
            match state {
                Some(d) => {
                    let ci = ColorImage::from_rgba_unmultiplied(d.size, &d.pixels);
                    let handle = self.ctx.load_texture(
                        format!("mmce_thumb_{}", path.display()),
                        ci,
                        TextureOptions::LINEAR,
                    );
                    self.tex
                        .lock()
                        .unwrap()
                        .insert(path.to_path_buf(), Some(handle.clone()));
                    return ThumbStatus::Ready(handle);
                }
                None => {
                    self.tex.lock().unwrap().insert(path.to_path_buf(), None);
                    return ThumbStatus::Failed;
                }
            }
        }

        let newly_queued = {
            let mut g = self.inner.lock().unwrap();
            if g.pending.contains_key(path) {
                false
            } else {
                g.pending.insert(path.to_path_buf(), ());
                true
            }
        };
        if newly_queued {
            let gen = *self.gen.lock().unwrap();
            let _ = self.tx.send(Msg::Request {
                path: path.to_path_buf(),
                thumb_size,
                gen,
            });
        }
        ThumbStatus::Pending
    }
}

impl Drop for ThumbnailCache {
    fn drop(&mut self) {
        for _ in 0..self._workers.len() {
            let _ = self.tx.send(Msg::Shutdown);
        }
    }
}

pub enum ThumbStatus {
    Pending,
    Ready(TextureHandle),
    Failed,
}

fn spawn_worker(
    id: usize,
    rx: Arc<Mutex<Receiver<Msg>>>,
    inner: Arc<Mutex<Inner>>,
    ctx: Context,
    gen: Arc<Mutex<u64>>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("mmce-thumbs-{id}"))
        .spawn(move || loop {
            // Lock only around recv; decoding proceeds in parallel.
            let msg = {
                let guard = match rx.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                match guard.recv() {
                    Ok(m) => m,
                    Err(_) => break,
                }
            };
            let (path, thumb_size, req_gen) = match msg {
                Msg::Shutdown => break,
                Msg::Request { path, thumb_size, gen } => (path, thumb_size, gen),
            };
            // If a clear() ran since this request was queued, drop it on the
            // floor — its resolution would be stale.
            if *gen.lock().unwrap() != req_gen {
                inner.lock().unwrap().pending.remove(&path);
                continue;
            }
            let decoded = decode_cover(&path, thumb_size);
            {
                let mut g = inner.lock().unwrap();
                g.pending.remove(&path);
                // Check the gen hasn't changed during decode.
                if *gen.lock().unwrap() == req_gen {
                    g.put(path.clone(), decoded);
                }
            }
            ctx.request_repaint();
        })
        .expect("spawn thumbnail worker")
}

fn decode_cover(path: &Path, thumb_size: u32) -> Option<Decoded> {
    let bytes = cover_bytes(path)?;
    let img = decode_image(&bytes).ok()?;
    let (w, h) = img.dimensions();
    let scale = (thumb_size as f32 / w.max(1) as f32)
        .min(thumb_size as f32 / h.max(1) as f32)
        .min(1.0);
    let tw = ((w as f32) * scale).round().max(1.0) as u32;
    let th = ((h as f32) * scale).round().max(1.0) as u32;
    let resized = img.resize_exact(tw, th, FilterType::Triangle);
    let rgba = resized.to_rgba8().into_raw();
    Some(Decoded {
        size: [tw as usize, th as usize],
        pixels: rgba,
    })
}

fn cover_bytes(path: &Path) -> Option<Vec<u8>> {
    if path.is_dir() || is_archive_path(path) {
        let src = open_source(path).ok()?;
        if src.is_empty() {
            return None;
        }
        src.read(0).ok()
    } else if is_image_path(path) {
        std::fs::read(path).ok()
    } else {
        None
    }
}
