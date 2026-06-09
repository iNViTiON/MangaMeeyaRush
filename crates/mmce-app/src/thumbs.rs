//! Thumbnail cache for the explorer view.
//!
//! Pipeline:
//! 1. UI calls `thumbnail(path, size)` per visible tile (and `prefetch` for
//!    nearby rows). Both return immediately — `thumbnail` returns a texture
//!    if ready, otherwise schedules a decode.
//! 2. A pool of workers pulls requests from a priority deque (high-priority
//!    user-visible requests are served before low-priority prefetches).
//! 3. Workers read cover bytes (each worker opens its own archive handle so
//!    reads are parallel on NVMe), decode to a small RGBA buffer using a
//!    fast box-filter thumbnailer, and hand the pixels to the UI thread via
//!    the staging map. The UI uploads them as egui textures on next repaint.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use image::GenericImageView;
use mmce_codecs::{cover_image, decode_cover_image};

struct Decoded {
    size: [usize; 2],
    pixels: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Priority {
    High,
    Low,
}

struct Request {
    path: PathBuf,
    thumb_size: u32,
    gen: u64,
    priority: Priority,
}

enum Msg {
    Request(Request),
    Shutdown,
}

struct Queue {
    /// Priority deque: high-priority items get `push_front`, low-priority
    /// items get `push_back`. Workers `pop_front`. If a request for the
    /// same path is already queued, we upgrade its priority instead of
    /// enqueueing a duplicate.
    items: VecDeque<Msg>,
}

impl Queue {
    fn new() -> Self {
        Self {
            items: VecDeque::new(),
        }
    }

    fn enqueue(&mut self, req: Request) {
        if let Some(pos) = self.items.iter().position(|m| match m {
            Msg::Request(r) => r.path == req.path,
            Msg::Shutdown => false,
        }) {
            if let Some(Msg::Request(old)) = self.items.remove(pos) {
                let priority = if old.priority == Priority::High || req.priority == Priority::High {
                    Priority::High
                } else {
                    Priority::Low
                };
                let merged = Request {
                    path: req.path,
                    thumb_size: req.thumb_size,
                    gen: req.gen,
                    priority,
                };
                self.push(merged);
                return;
            }
        }
        self.push(req);
    }

    fn push(&mut self, req: Request) {
        match req.priority {
            Priority::High => self.items.push_front(Msg::Request(req)),
            Priority::Low => self.items.push_back(Msg::Request(req)),
        }
    }

    fn push_shutdown(&mut self) {
        self.items.push_back(Msg::Shutdown);
    }

    fn pop(&mut self) -> Option<Msg> {
        self.items.pop_front()
    }
}

struct Inner {
    /// Paths currently enqueued or being decoded. Used to deduplicate
    /// further requests for the same path.
    pending: HashMap<PathBuf, ()>,
    /// Freshly decoded items waiting to be promoted to textures on the UI
    /// thread. Staging buffer — the UI drains it on every repaint, so we
    /// don't cap it. Capping here races UI drainage and causes visible
    /// tiles to bounce between Ready → Pending.
    decoded: HashMap<PathBuf, Option<Decoded>>,
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
        self.map.get_mut(path).map(|e| {
            e.sort = c;
            &*e
        })
    }

    fn insert(&mut self, path: PathBuf, handle: Option<TextureHandle>) {
        self.counter += 1;
        let entry = TexEntry {
            handle,
            sort: self.counter,
        };
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
    queue: Arc<(Mutex<Queue>, Condvar)>,
    tex: Mutex<TexStore>,
    worker_count: usize,
    _workers: Vec<thread::JoinHandle<()>>,
    /// Incremented on `clear()` — worker results from a previous generation
    /// are discarded so the fresh cache only contains the new thumb size.
    gen: Arc<Mutex<u64>>,
}

impl ThumbnailCache {
    pub fn new(ctx: Context, capacity: usize) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            pending: HashMap::new(),
            decoded: HashMap::new(),
        }));
        let queue = Arc::new((Mutex::new(Queue::new()), Condvar::new()));
        let gen = Arc::new(Mutex::new(0u64));
        // One core for UI, the rest for thumbs. Floor of 4 so even a
        // 2-core box parallelizes decode a bit; ceiling of 12 so big
        // workstations don't context-switch themselves to death. Past
        // ~12 threads thumb decoding is memory-bandwidth-bound and extra
        // workers just thrash cache.
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_sub(1)
            .clamp(4, 12);
        let workers = (0..n)
            .map(|i| spawn_worker(i, queue.clone(), inner.clone(), ctx.clone(), gen.clone()))
            .collect();
        Self {
            ctx,
            inner,
            queue,
            tex: Mutex::new(TexStore {
                map: HashMap::new(),
                counter: 0,
                // Large cap because eviction below a few thousand starts to
                // churn for big libraries. We only call `thumbnail()` for
                // visible tiles, but users can still scroll a lot.
                capacity: capacity.max(4096),
            }),
            worker_count: n,
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
                    // Premultiplied skips compositor alpha math. Covers
                    // are opaque (JPEG has no alpha; PNG covers are
                    // overwhelmingly opaque) so the result is identical.
                    let ci = ColorImage::from_rgba_premultiplied(d.size, &d.pixels);
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

        self.enqueue(path, thumb_size, Priority::High);
        ThumbStatus::Pending
    }

    /// Ask the worker pool to decode `path` in the background at low
    /// priority. Used to prefetch adjacent tiles so the user doesn't see
    /// spinners when they scroll.
    pub fn prefetch(&self, path: &Path, thumb_size: u32) {
        if self.tex.lock().unwrap().map.contains_key(path) {
            return;
        }
        if self.inner.lock().unwrap().decoded.contains_key(path) {
            return;
        }
        self.enqueue(path, thumb_size, Priority::Low);
    }

    fn enqueue(&self, path: &Path, thumb_size: u32, priority: Priority) {
        let newly_queued = {
            let mut g = self.inner.lock().unwrap();
            if g.pending.contains_key(path) {
                false
            } else {
                g.pending.insert(path.to_path_buf(), ());
                true
            }
        };
        let req_gen = *self.gen.lock().unwrap();
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        if newly_queued {
            q.enqueue(Request {
                path: path.to_path_buf(),
                thumb_size,
                gen: req_gen,
                priority,
            });
            cvar.notify_one();
        } else if priority == Priority::High {
            // Existing request — bump priority so it gets served next.
            q.enqueue(Request {
                path: path.to_path_buf(),
                thumb_size,
                gen: req_gen,
                priority,
            });
            cvar.notify_one();
        }
    }
}

impl Drop for ThumbnailCache {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        for _ in 0..self.worker_count {
            q.push_shutdown();
        }
        cvar.notify_all();
    }
}

pub enum ThumbStatus {
    Pending,
    Ready(TextureHandle),
    Failed,
}

fn spawn_worker(
    id: usize,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    inner: Arc<Mutex<Inner>>,
    ctx: Context,
    gen: Arc<Mutex<u64>>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("mmce-thumbs-{id}"))
        .spawn(move || loop {
            let msg = {
                let (lock, cvar) = &*queue;
                let mut q = lock.lock().unwrap();
                loop {
                    if let Some(m) = q.pop() {
                        break m;
                    }
                    q = match cvar.wait(q) {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                }
            };
            let req = match msg {
                Msg::Shutdown => break,
                Msg::Request(r) => r,
            };
            // If a clear() ran since this request was queued, drop it on the
            // floor — its resolution would be stale.
            if *gen.lock().unwrap() != req.gen {
                inner.lock().unwrap().pending.remove(&req.path);
                continue;
            }
            let decoded = decode_cover(&req.path, req.thumb_size);
            {
                let mut g = inner.lock().unwrap();
                g.pending.remove(&req.path);
                if *gen.lock().unwrap() == req.gen {
                    g.decoded.insert(req.path.clone(), decoded);
                }
            }
            ctx.request_repaint();
        })
        .expect("spawn thumbnail worker")
}

fn decode_cover(path: &Path, thumb_size: u32) -> Option<Decoded> {
    let bytes = cover_image(path)?;
    // DCT-scaled decode for JPEG covers (libjpeg-turbo), full zune decode for
    // everything else. Returns an image whose longest edge is already ≥
    // `thumb_size`, so the fit-shrink below is cheap and never upscales.
    let img = decode_cover_image(&bytes, thumb_size)?;
    let (w, h) = img.dimensions();
    let scale = (thumb_size as f32 / w.max(1) as f32)
        .min(thumb_size as f32 / h.max(1) as f32)
        .min(1.0);
    let tw = ((w as f32) * scale).round().max(1.0) as u32;
    let th = ((h as f32) * scale).round().max(1.0) as u32;
    // `thumbnail_exact` uses a fast box filter — 2-5x faster than
    // `resize_exact(Triangle)` on large covers, and visually fine at the
    // small sizes we render tiles at.
    let resized = img.thumbnail_exact(tw, th);
    // `into_rgba8` is zero-copy when the thumb is already RGBA8 (common
    // for PNG covers). `to_rgba8` would clone every time.
    let rgba = resized.into_rgba8().into_raw();
    Some(Decoded {
        size: [tw as usize, th as usize],
        pixels: rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_push_order_is_front_for_high() {
        let mut q = Queue::new();
        q.enqueue(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::Low,
        });
        q.enqueue(Request {
            path: PathBuf::from("/b"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::High,
        });
        match q.pop() {
            Some(Msg::Request(r)) => assert_eq!(r.path, PathBuf::from("/b")),
            _ => panic!("expected Request"),
        }
        match q.pop() {
            Some(Msg::Request(r)) => assert_eq!(r.path, PathBuf::from("/a")),
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn duplicate_request_upgrades_priority() {
        let mut q = Queue::new();
        q.enqueue(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::Low,
        });
        q.enqueue(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::High,
        });
        assert_eq!(q.items.len(), 1);
        match q.pop() {
            Some(Msg::Request(r)) => {
                assert_eq!(r.path, PathBuf::from("/a"));
                assert_eq!(r.priority, Priority::High);
            }
            _ => panic!("expected Request"),
        }
    }
}
