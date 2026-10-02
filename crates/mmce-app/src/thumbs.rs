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
//!
//! Textures remember the thumb size they were decoded for. When the tile size
//! changes the old texture keeps being drawn (scaled) while a decode at the
//! new size runs, so Ctrl± never blanks the grid.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use image::GenericImageView;
use mmce_codecs::{cover_image, decode_cover_image};

struct Decoded {
    size: [usize; 2],
    pixels: Vec<u8>,
}

/// A worker result waiting for the UI thread. `result` is `None` when the
/// path couldn't be decoded.
struct Staged {
    thumb_size: u32,
    result: Option<Decoded>,
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

    fn position(&self, path: &Path) -> Option<usize> {
        self.items.iter().position(|m| match m {
            Msg::Request(r) => r.path == path,
            Msg::Shutdown => false,
        })
    }

    /// `pos` is where a request for the same path already sits, if any: merge
    /// into it (keeping the higher priority, adopting the newer size/gen).
    fn merge_or_push(&mut self, pos: Option<usize>, req: Request) {
        if let Some(pos) = pos {
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

    /// Bump an already-queued request for `req.path` to high priority,
    /// adopting `req`'s thumb size. Returns `false` (and queues nothing) when
    /// the path isn't in the queue, i.e. a worker already has it in flight.
    fn promote(&mut self, req: Request) -> bool {
        let pos = self.position(&req.path);
        if pos.is_some() {
            self.merge_or_push(pos, req);
        }
        pos.is_some()
    }

    /// Remove every queued request (shutdown markers stay) and return their
    /// paths.
    fn drain_requests(&mut self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        self.items.retain(|m| match m {
            Msg::Request(r) => {
                paths.push(r.path.clone());
                false
            }
            Msg::Shutdown => true,
        });
        paths
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
    decoded: HashMap<PathBuf, Staged>,
}

struct TexEntry {
    handle: Option<TextureHandle>,
    /// Thumb size the texture was decoded for. A mismatch with the size the
    /// UI now wants means "draw it anyway, but re-decode".
    thumb_size: u32,
    sort: u64,
}

impl TexEntry {
    /// Nothing more to decode for `thumb_size`: either the texture matches,
    /// or the path failed (failure doesn't depend on size).
    fn satisfies(&self, thumb_size: u32) -> bool {
        self.handle.is_none() || self.thumb_size == thumb_size
    }
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

    fn insert(&mut self, path: PathBuf, handle: Option<TextureHandle>, thumb_size: u32) {
        self.counter += 1;
        let entry = TexEntry {
            handle,
            thumb_size,
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
}

/// Relaxed atomic counters behind [`ThumbnailCache::take_stats`].
#[derive(Default)]
struct Counters {
    decodes: AtomicU64,
    discarded: AtomicU64,
    uploads: AtomicU64,
    calls: AtomicU64,
    calls_pending: AtomicU64,
}

/// Diagnostics snapshot for benchmarks and debugging (see
/// `examples/library_sim.rs`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ThumbStats {
    pub workers: usize,
    /// Covers decoded by workers, failures included.
    pub decodes: u64,
    /// Decoded results dropped because `cancel_dir()` ran mid-decode.
    pub discarded: u64,
    /// Textures uploaded.
    pub uploads: u64,
    pub textures: usize,
    /// Decoded, not yet uploaded (off-screen prefetch results).
    pub staged: usize,
    pub staged_bytes: usize,
    /// Queued + in flight.
    pub pending: usize,
    pub queued: usize,
    /// `thumbnail()` calls since the previous `take_stats()`, and how many
    /// of them returned `Pending`. One frame's worth = visible tiles.
    pub calls: u64,
    pub calls_pending: u64,
}

pub struct ThumbnailCache {
    ctx: Context,
    counters: Arc<Counters>,
    inner: Arc<Mutex<Inner>>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    tex: Mutex<TexStore>,
    worker_count: usize,
    _workers: Vec<thread::JoinHandle<()>>,
    /// Incremented on `cancel_dir()` — worker results from a previous
    /// generation are discarded instead of piling up in the staging map.
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
        let counters = Arc::new(Counters::default());
        // Worker-pool size: ~1.5x cores by default, env-overridable. See
        // worker_pool_size() for the rationale and the MMCE_THUMB_WORKERS knob.
        let n = worker_pool_size();
        let workers = (0..n)
            .map(|i| {
                spawn_worker(
                    i,
                    queue.clone(),
                    inner.clone(),
                    ctx.clone(),
                    gen.clone(),
                    counters.clone(),
                )
            })
            .collect();
        Self {
            ctx,
            counters,
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

    /// Diagnostics snapshot. Resets the per-call counters (`calls`,
    /// `calls_pending`), so calling it once per frame yields per-frame
    /// visible-tile counts.
    pub fn take_stats(&self) -> ThumbStats {
        let c = &self.counters;
        let (staged, staged_bytes, pending) = {
            let g = self.inner.lock().unwrap();
            let bytes = g
                .decoded
                .values()
                .filter_map(|s| s.result.as_ref().map(|d| d.pixels.len()))
                .sum();
            (g.decoded.len(), bytes, g.pending.len())
        };
        let queued = {
            let (lock, _) = &*self.queue;
            lock.lock().unwrap().items.len()
        };
        ThumbStats {
            workers: self.worker_count,
            decodes: c.decodes.load(Ordering::Relaxed),
            discarded: c.discarded.load(Ordering::Relaxed),
            uploads: c.uploads.load(Ordering::Relaxed),
            textures: self.tex.lock().unwrap().map.len(),
            staged,
            staged_bytes,
            pending,
            queued,
            calls: c.calls.swap(0, Ordering::Relaxed),
            calls_pending: c.calls_pending.swap(0, Ordering::Relaxed),
        }
    }

    /// Drop every queued (not yet started) request. Used when the thumb size
    /// changes: the queue holds old-size work, and the next frame re-requests
    /// what's on screen at the new size. Textures stay — they keep being drawn
    /// scaled until the new decode lands.
    pub fn cancel_queued(&self) {
        let dropped = {
            let (lock, _) = &*self.queue;
            lock.lock().unwrap().drain_requests()
        };
        let mut g = self.inner.lock().unwrap();
        for p in dropped {
            g.pending.remove(&p);
        }
    }

    /// The explorer moved to another directory: drop the old directory's
    /// queued prefetch backlog (so the new one isn't stuck behind it) and its
    /// decoded-but-never-shown results (so they don't accumulate across a
    /// session). Results still in flight are discarded when they land.
    /// Uploaded textures are kept, so going back shows seen tiles instantly.
    pub fn cancel_dir(&self) {
        self.cancel_queued();
        // Bump under the inner lock: a worker checks gen and inserts while
        // holding it, so no stale result can slip in after the clear.
        let mut g = self.inner.lock().unwrap();
        *self.gen.lock().unwrap() += 1;
        g.decoded.clear();
    }

    /// Returns a texture for `path` if decoded; otherwise schedules a decode
    /// and returns `Pending`. `Failed` means the path couldn't decode — the
    /// UI should fall back to an icon.
    pub fn thumbnail(&self, path: &Path, thumb_size: u32) -> ThumbStatus {
        self.counters.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(status) = self.tex_status(path, thumb_size, true) {
            return status;
        }

        let staged = self.inner.lock().unwrap().decoded.remove(path);
        if let Some(staged) = staged {
            let handle = staged.result.map(|d| {
                // Premultiplied skips compositor alpha math. Covers are
                // opaque (JPEG has no alpha; PNG covers are overwhelmingly
                // opaque) so the result is identical.
                let ci = ColorImage::from_rgba_premultiplied(d.size, &d.pixels);
                self.counters.uploads.fetch_add(1, Ordering::Relaxed);
                self.ctx.load_texture(
                    format!("mmce_thumb_{}", path.display()),
                    ci,
                    TextureOptions::LINEAR,
                )
            });
            self.tex
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), handle, staged.thumb_size);
            if let Some(status) = self.tex_status(path, thumb_size, true) {
                return status;
            }
        }

        self.enqueue(path, thumb_size, Priority::High);
        // A texture at another size (tile size just changed) beats a
        // placeholder: draw it scaled until the re-decode lands.
        self.tex_status(path, thumb_size, false).unwrap_or_else(|| {
            self.counters.calls_pending.fetch_add(1, Ordering::Relaxed);
            ThumbStatus::Pending
        })
    }

    /// Status from the texture store. With `exact`, only a texture that
    /// needs no further decode counts; otherwise any texture does.
    fn tex_status(&self, path: &Path, thumb_size: u32, exact: bool) -> Option<ThumbStatus> {
        let mut tex = self.tex.lock().unwrap();
        let entry = tex.get(path)?;
        if exact && !entry.satisfies(thumb_size) {
            return None;
        }
        Some(match &entry.handle {
            Some(t) => ThumbStatus::Ready(t.clone()),
            None => ThumbStatus::Failed,
        })
    }

    /// Ask the worker pool to decode `path` in the background at low
    /// priority. Used to prefetch adjacent tiles so the user doesn't see
    /// spinners when they scroll.
    pub fn prefetch(&self, path: &Path, thumb_size: u32) {
        if let Some(e) = self.tex.lock().unwrap().map.get(path) {
            if e.satisfies(thumb_size) {
                return;
            }
        }
        if let Some(s) = self.inner.lock().unwrap().decoded.get(path) {
            if s.result.is_none() || s.thumb_size == thumb_size {
                return;
            }
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
        if !newly_queued && priority == Priority::Low {
            return;
        }
        let req = Request {
            path: path.to_path_buf(),
            thumb_size,
            gen: *self.gen.lock().unwrap(),
            priority,
        };
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        if newly_queued {
            // Not pending ⇒ not queued (the queue is a subset of `pending`),
            // so skip `enqueue`'s duplicate scan: entering a big folder
            // queues every tile in one frame, and scanning made that O(n²)
            // — ~100 ms of UI stall for 1000 entries.
            q.push(req);
            cvar.notify_one();
        } else {
            // Already pending: bump it to the front if it's still queued.
            // If a worker has it in flight, leave it alone — visible tiles
            // re-request every frame, and re-queueing an in-flight path used
            // to decode each on-screen cover 2-3x over.
            q.promote(req);
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
    counters: Arc<Counters>,
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
            let result = decode_cover(&req.path, req.thumb_size);
            counters.decodes.fetch_add(1, Ordering::Relaxed);
            {
                let mut g = inner.lock().unwrap();
                g.pending.remove(&req.path);
                if *gen.lock().unwrap() != req.gen {
                    counters.discarded.fetch_add(1, Ordering::Relaxed);
                } else {
                    g.decoded.insert(
                        req.path.clone(),
                        Staged {
                            thumb_size: req.thumb_size,
                            result,
                        },
                    );
                }
            }
            match req.priority {
                Priority::High => ctx.request_repaint(),
                // Off-screen prefetch: nothing visible changed, so don't force
                // a full explorer frame per result. The deferred repaint still
                // picks up a tile that scrolled into view mid-decode.
                Priority::Low => ctx.request_repaint_after(Duration::from_millis(100)),
            }
        })
        .expect("spawn thumbnail worker")
}

/// Worker-pool size for thumbnail decoding. Defaults to ~1.5x cores (cores +
/// cores/2). Cold thumbnail scans are I/O-wait-bound — a worker blocks on a cold
/// archive read and idles its core — so to keep all C cores busy when a fraction
/// `rho` of each cover's wall-time is blocking I/O you need ~C/(1-rho) threads:
/// a MULTIPLIER on core count, not a fixed addend. Measured rho ~= 0.4 on native
/// cold (the cold floor ~= the warm time), implying a ~1.66x optimum; 1.5x
/// captures most of it while leaving the eframe UI thread some headroom. Warm
/// throughput is flat out to 4x cores, so the oversubscription is ~free when
/// reads hit the page cache. Floor 4 for tiny boxes; ceiling 16 as a safety cap
/// (memory + UI thread) on huge machines.
///
/// `MMCE_THUMB_WORKERS` overrides it: parsed as usize, ignored if unset,
/// non-numeric, or zero, and clamped to [1, 64] so a typo can't spawn thousands
/// of threads. Read fresh on every `ThumbnailCache::new`, so it takes effect on
/// the next cache creation rather than retroactively on a live pool. Push it
/// higher on slow / FUSE storage, where cold reads block longer (larger `rho`)
/// and there is more idle to reclaim.
fn worker_pool_size() -> usize {
    let env_override = std::env::var_os("MMCE_THUMB_WORKERS")
        .and_then(|v| v.to_str().and_then(|s| s.trim().parse::<usize>().ok()))
        .filter(|&n| n > 0)
        .map(|n| n.clamp(1, 64));
    env_override.unwrap_or_else(|| {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        (cores * 3 / 2).clamp(4, 16)
    })
}

fn decode_cover(path: &Path, thumb_size: u32) -> Option<Decoded> {
    #[cfg(test)]
    tests::count_decode(path);
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
    use std::time::Instant;

    /// Per-path decode counts. Keyed by path so tests running in parallel
    /// (each in its own temp dir) don't see each other's decodes.
    static DECODES: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    pub(super) fn count_decode(path: &Path) {
        let mut g = DECODES.lock().unwrap();
        *g.get_or_insert_with(HashMap::new)
            .entry(path.to_path_buf())
            .or_default() += 1;
    }

    fn decodes_of(path: &Path) -> usize {
        DECODES
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|m| m.get(path).copied())
            .unwrap_or(0)
    }

    fn write_covers(dir: &Path, n: usize, w: u32, h: u32) -> Vec<PathBuf> {
        (0..n)
            .map(|i| {
                let img = image::RgbImage::from_fn(w, h, |x, y| {
                    image::Rgb([(x + i as u32) as u8, y as u8, (x ^ y) as u8])
                });
                let p = dir.join(format!("cover{i:02}.png"));
                img.save(&p).unwrap();
                p
            })
            .collect()
    }

    /// Drive `thumbnail()` like the UI does (every visible tile, every frame)
    /// until `done` holds for all of them.
    fn frames_until(
        cache: &ThumbnailCache,
        paths: &[PathBuf],
        size: u32,
        mut done: impl FnMut(&ThumbStatus) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let all = paths.iter().all(|p| done(&cache.thumbnail(p, size)));
            if all {
                return;
            }
            assert!(Instant::now() < deadline, "thumbnails never became ready");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn visible_tiles_are_decoded_once() {
        // Re-requesting an on-screen tile every frame while its decode is in
        // flight must not queue a second decode of the same cover.
        let dir = tempfile::tempdir().unwrap();
        let paths = write_covers(dir.path(), 12, 600, 900);
        let cache = ThumbnailCache::new(Context::default(), 64);
        frames_until(&cache, &paths, 64, |s| matches!(s, ThumbStatus::Ready(_)));
        thread::sleep(Duration::from_millis(100)); // let any stray duplicate land
        for p in &paths {
            assert_eq!(decodes_of(p), 1, "{} decoded more than once", p.display());
        }
    }

    #[test]
    fn resize_keeps_drawing_old_texture_until_redecoded() {
        let dir = tempfile::tempdir().unwrap();
        let paths = write_covers(dir.path(), 3, 400, 600);
        let cache = ThumbnailCache::new(Context::default(), 64);
        frames_until(&cache, &paths, 64, |s| matches!(s, ThumbStatus::Ready(_)));

        cache.cancel_queued();
        // First frame at the new size: the 64px texture is still served.
        for p in &paths {
            match cache.thumbnail(p, 128) {
                ThumbStatus::Ready(t) => assert_eq!(t.size()[1], 64),
                _ => panic!("tile blanked on resize"),
            }
        }
        // ...and is replaced once the 128px decode lands.
        frames_until(
            &cache,
            &paths,
            128,
            |s| matches!(s, ThumbStatus::Ready(t) if t.size()[1] == 128),
        );
    }

    #[test]
    fn cancel_dir_drops_backlog_and_staged_results() {
        let dir = tempfile::tempdir().unwrap();
        let paths = write_covers(dir.path(), 4, 64, 64);
        let cache = ThumbnailCache::new(Context::default(), 64);
        for p in &paths {
            cache.prefetch(p, 32);
        }
        cache.cancel_dir();
        thread::sleep(Duration::from_millis(200));
        let g = cache.inner.lock().unwrap();
        assert!(
            g.decoded.is_empty(),
            "stale results staged after cancel_dir"
        );
        assert!(g.pending.is_empty(), "stale requests still pending");
    }

    #[test]
    fn promote_does_not_requeue_in_flight_path() {
        let mut q = Queue::new();
        // Nothing queued for /a: a worker popped it and is decoding.
        let promoted = q.promote(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::High,
        });
        assert!(!promoted);
        assert!(q.items.is_empty());
    }

    #[test]
    fn promote_bumps_queued_request_and_adopts_new_size() {
        let mut q = Queue::new();
        for p in ["/a", "/b"] {
            q.push(Request {
                path: PathBuf::from(p),
                thumb_size: 64,
                gen: 0,
                priority: Priority::Low,
            });
        }
        assert!(q.promote(Request {
            path: PathBuf::from("/b"),
            thumb_size: 128,
            gen: 0,
            priority: Priority::High,
        }));
        match q.pop() {
            Some(Msg::Request(r)) => {
                assert_eq!(r.path, PathBuf::from("/b"));
                assert_eq!(r.thumb_size, 128);
                assert_eq!(r.priority, Priority::High);
            }
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn drain_requests_keeps_shutdown_markers() {
        let mut q = Queue::new();
        q.push(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::Low,
        });
        q.push_shutdown();
        assert_eq!(q.drain_requests(), vec![PathBuf::from("/a")]);
        assert!(matches!(q.pop(), Some(Msg::Shutdown)));
        assert!(q.pop().is_none());
    }

    #[test]
    fn priority_push_order_is_front_for_high() {
        let mut q = Queue::new();
        q.push(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::Low,
        });
        q.push(Request {
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
        q.push(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::Low,
        });
        assert!(q.promote(Request {
            path: PathBuf::from("/a"),
            thumb_size: 64,
            gen: 0,
            priority: Priority::High,
        }));
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
