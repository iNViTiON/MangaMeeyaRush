//! Texture cache + background prefetch + fit geometry.
//!
//! The cache stores decoded CPU images (RGBA8) keyed by page index, with an
//! LRU eviction. A pool of workers decodes pages in parallel; visible pages
//! use a high-priority lane, prefetch uses low-priority. egui textures are
//! created lazily on the UI thread the first time a page is painted.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use egui::{ColorImage, Context, TextureFilter, TextureHandle, TextureOptions, Vec2};
use image::GenericImageView;
use mmce_codecs::{decode_image, PageSource};
use mmce_config::FitMode;
use mmce_filters::Pipeline;

pub mod fit;

pub use fit::{fit_rect, FitParams};

/// A decoded image buffer in RGBA8 ready to be uploaded as a texture.
#[derive(Clone)]
pub struct DecodedPage {
    pub size: [usize; 2],
    pub pixels: Arc<Vec<u8>>,
}

impl DecodedPage {
    pub fn from_dynamic(img: image::DynamicImage) -> Self {
        let (w, h) = img.dimensions();
        // `into_rgba8` is zero-copy when the image is already
        // `ImageRgba8` (common for PNG books). `to_rgba8` on `&self`
        // would clone even in that case — wasting ~80 MB on a 4K page.
        let rgba = img.into_rgba8().into_raw();
        Self {
            size: [w as usize, h as usize],
            pixels: Arc::new(rgba),
        }
    }

    pub fn to_color_image(&self) -> ColorImage {
        // `from_rgba_premultiplied` skips the per-pixel alpha math the
        // compositor would otherwise do. Manga pages are opaque (JPEG
        // has no alpha; PNGs are overwhelmingly opaque) so the result
        // is visually identical but cheaper to render. For the edge
        // case of a transparent PNG the difference is imperceptible at
        // the sizes pages are displayed.
        ColorImage::from_rgba_premultiplied(self.size, &self.pixels)
    }
}

/// Page texture options: linear min/mag + linear mipmaps. The mipmap
/// chain is what makes fit-to-screen zoom-out look clean instead of
/// shimmering — the GPU samples a pre-filtered smaller level instead of
/// bilinear-averaging over thousands of texels per screen pixel. It also
/// speeds up fragment shading since less data crosses the sampler.
const PAGE_TEX_OPTS: TextureOptions = TextureOptions {
    magnification: TextureFilter::Linear,
    minification: TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: Some(TextureFilter::Linear),
};

/// Bounded LRU of GPU texture handles, mirroring the CPU cache so VRAM
/// doesn't balloon as the user navigates. Dropping a `TextureHandle`
/// tells egui's backend to free the texture on the next frame.
struct TexLru {
    map: HashMap<usize, TextureHandle>,
    order: VecDeque<usize>,
    capacity: usize,
}

impl TexLru {
    fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(4),
        }
    }

    fn get(&mut self, idx: usize) -> Option<TextureHandle> {
        let hit = self.map.get(&idx).cloned();
        if hit.is_some() {
            self.order.retain(|x| *x != idx);
            self.order.push_back(idx);
        }
        hit
    }

    fn insert(&mut self, idx: usize, handle: TextureHandle) {
        if self.map.contains_key(&idx) {
            self.order.retain(|x| *x != idx);
        } else if self.map.len() >= self.capacity {
            if let Some(evict) = self.order.pop_front() {
                // Dropping the TextureHandle signals egui to release
                // the GPU texture on the next frame.
                self.map.remove(&evict);
            }
        }
        self.order.push_back(idx);
        self.map.insert(idx, handle);
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Priority {
    High,
    Low,
}

enum WorkerMsg {
    Decode {
        index: usize,
        priority: Priority,
        epoch: u64,
    },
    SwapSource(Arc<dyn PageSource>),
    SwapPipeline(Arc<Pipeline>),
    Shutdown,
}

/// Priority deque: high-priority items at the front, low-priority at the
/// back, control messages (Swap/Shutdown) always at the front so they're
/// observed promptly by idle workers.
struct Queue {
    items: VecDeque<WorkerMsg>,
}

impl Queue {
    fn new() -> Self {
        Self {
            items: VecDeque::new(),
        }
    }

    fn push(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Decode {
                priority: Priority::High,
                ..
            } => self.items.push_front(msg),
            WorkerMsg::Decode {
                priority: Priority::Low,
                ..
            } => self.items.push_back(msg),
            // Control messages always go to the front.
            WorkerMsg::SwapSource(_) | WorkerMsg::SwapPipeline(_) | WorkerMsg::Shutdown => {
                self.items.push_front(msg)
            }
        }
    }

    fn pop(&mut self) -> Option<WorkerMsg> {
        self.items.pop_front()
    }

    /// Drop every pending decode whose epoch is older than `current`. Used
    /// when the user jumps far from the previous window — stale prefetch
    /// wastes CPU on pages that are no longer relevant.
    fn cancel_stale(&mut self, current: u64) {
        self.items.retain(|m| match m {
            WorkerMsg::Decode { epoch, .. } => *epoch >= current,
            _ => true,
        });
    }
}

struct CacheInner {
    order: VecDeque<usize>,
    map: HashMap<usize, DecodedPage>,
    capacity: usize,
}

impl CacheInner {
    fn put(&mut self, idx: usize, page: DecodedPage) {
        if self.map.contains_key(&idx) {
            self.order.retain(|x| *x != idx);
        } else if self.map.len() >= self.capacity {
            if let Some(evict) = self.order.pop_front() {
                self.map.remove(&evict);
            }
        }
        self.order.push_back(idx);
        self.map.insert(idx, page);
    }

    fn touch(&mut self, idx: usize) {
        if self.map.contains_key(&idx) {
            self.order.retain(|x| *x != idx);
            self.order.push_back(idx);
        }
    }

    fn get(&mut self, idx: usize) -> Option<DecodedPage> {
        let hit = self.map.get(&idx).cloned();
        if hit.is_some() {
            self.touch(idx);
        }
        hit
    }
}

pub struct PageCache {
    inner: Arc<Mutex<CacheInner>>,
    tex: Mutex<TexLru>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    worker_count: usize,
    _workers: Vec<thread::JoinHandle<()>>,
    source: Mutex<Arc<dyn PageSource>>,
    pipeline: Mutex<Arc<Pipeline>>,
    egui_ctx: Context,
    /// Monotonic epoch used for stale-decode cancellation. Incremented on
    /// source/pipeline swap and when the prefetch window jumps far.
    epoch: Arc<AtomicU64>,
    /// Last prefetch centre, used to detect big jumps.
    last_centre: Mutex<Option<usize>>,
}

impl PageCache {
    pub fn new(ctx: Context, source: Arc<dyn PageSource>, capacity: usize) -> Self {
        // Minimum 8 so spread-mode viewing with 4-forward prefetch fits
        // without evicting the current spread.
        let capacity = capacity.max(8);
        let inner = Arc::new(Mutex::new(CacheInner {
            order: VecDeque::new(),
            map: HashMap::new(),
            capacity,
        }));
        let pipeline = Arc::new(Pipeline::new());
        let queue = Arc::new((Mutex::new(Queue::new()), Condvar::new()));
        let epoch = Arc::new(AtomicU64::new(0));
        // CPU-scaled decoder pool. One core reserved for UI; floor 2 so
        // even a 2-core machine still pipelines read + decode; ceiling 8
        // because past that you saturate NVMe queue depth and libjpeg
        // inflate threads on typical workloads.
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_sub(1)
            .clamp(2, 8);
        let workers = (0..n)
            .map(|i| {
                spawn_worker(
                    i,
                    queue.clone(),
                    inner.clone(),
                    source.clone(),
                    pipeline.clone(),
                    ctx.clone(),
                    epoch.clone(),
                )
            })
            .collect();
        Self {
            inner,
            tex: Mutex::new(TexLru::new(capacity)),
            queue,
            worker_count: n,
            _workers: workers,
            source: Mutex::new(source),
            pipeline: Mutex::new(pipeline),
            egui_ctx: ctx,
            epoch,
            last_centre: Mutex::new(None),
        }
    }

    /// Swap in a new filter pipeline. Drops cached decodes (they were
    /// produced against the old pipeline) and signals workers to rebuild.
    pub fn set_pipeline(&self, pipeline: Pipeline) {
        let arc = Arc::new(pipeline);
        {
            let mut g = self.inner.lock().unwrap();
            g.order.clear();
            g.map.clear();
        }
        self.tex.lock().unwrap().clear();
        *self.pipeline.lock().unwrap() = arc.clone();
        self.bump_epoch();
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.cancel_stale(self.epoch.load(Ordering::Relaxed));
        q.push(WorkerMsg::SwapPipeline(arc));
        cvar.notify_all();
        self.egui_ctx.request_repaint();
    }

    /// Intrinsic size of a decoded page, if it's currently in the cache.
    /// Does not touch LRU order.
    pub fn page_dimensions(&self, idx: usize) -> Option<(u32, u32)> {
        let g = self.inner.lock().ok()?;
        g.map
            .get(&idx)
            .map(|p| (p.size[0] as u32, p.size[1] as u32))
    }

    pub fn swap_source(&self, source: Arc<dyn PageSource>) {
        {
            let mut g = self.inner.lock().unwrap();
            g.order.clear();
            g.map.clear();
        }
        self.tex.lock().unwrap().clear();
        *self.source.lock().unwrap() = source.clone();
        *self.last_centre.lock().unwrap() = None;
        self.bump_epoch();
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.cancel_stale(self.epoch.load(Ordering::Relaxed));
        q.push(WorkerMsg::SwapSource(source));
        cvar.notify_all();
    }

    /// Request a page; if cached, returns an egui texture handle. If not,
    /// schedules a HIGH-priority decode.
    pub fn texture(&self, index: usize) -> Option<TextureHandle> {
        if let Some(t) = self.tex.lock().unwrap().get(index) {
            return Some(t);
        }
        let decoded = {
            let mut g = self.inner.lock().unwrap();
            g.get(index)
        };
        if let Some(d) = decoded {
            let ci = d.to_color_image();
            let handle =
                self.egui_ctx
                    .load_texture(format!("mmce_page_{index}"), ci, PAGE_TEX_OPTS);
            self.tex.lock().unwrap().insert(index, handle.clone());
            Some(handle)
        } else {
            self.enqueue(index, Priority::High);
            None
        }
    }

    /// Symmetric prefetch window (backwards-compat). Equivalent to
    /// `prefetch_directed(window, neighbours, neighbours, 0)`.
    pub fn prefetch(&self, window: &[usize], neighbours: usize) {
        self.prefetch_directed(window, neighbours, neighbours, 0);
    }

    /// Directional prefetch. `hint_direction` +1 = reading forward (default
    /// bias), -1 = reading backward, 0 = symmetric. Forward/backward counts
    /// are how many neighbours to decode in each direction (before the
    /// direction swap implied by `hint_direction`).
    pub fn prefetch_directed(
        &self,
        window: &[usize],
        forward_n: usize,
        backward_n: usize,
        hint_direction: isize,
    ) {
        if window.is_empty() {
            return;
        }
        let (fwd, bwd) = if hint_direction < 0 {
            (backward_n, forward_n)
        } else {
            (forward_n, backward_n)
        };
        let len = self.source.lock().unwrap().len();
        let centre = window_centre(window);

        // Big jumps bump the epoch so already-queued low-priority decodes
        // for the old window get cancelled.
        {
            let mut last = self.last_centre.lock().unwrap();
            let jump = match *last {
                Some(prev) => centre.abs_diff(prev),
                None => 0,
            };
            *last = Some(centre);
            let reach = fwd.max(bwd).max(2);
            if jump > reach * 2 {
                self.bump_epoch();
                let (lock, cvar) = &*self.queue;
                let mut q = lock.lock().unwrap();
                q.cancel_stale(self.epoch.load(Ordering::Relaxed));
                // No notify: workers will pick up the next high-priority
                // item on their own wakeup.
                let _ = cvar;
            }
        }

        let mut high: BTreeSet<usize> = window.iter().copied().collect();
        let mut low: BTreeSet<usize> = BTreeSet::new();
        for &w in window {
            for k in 1..=fwd {
                if let Some(next) = w.checked_add(k) {
                    if next < len && !high.contains(&next) {
                        low.insert(next);
                    }
                }
            }
            for k in 1..=bwd {
                if let Some(prev) = w.checked_sub(k) {
                    if !high.contains(&prev) {
                        low.insert(prev);
                    }
                }
            }
        }
        // Make sure the high set doesn't also appear in low.
        for idx in &high {
            low.remove(idx);
        }
        // Enqueue — high first so they win even if a worker picks the next
        // message the moment it's pushed.
        high.retain(|&idx| !self.inner.lock().unwrap().map.contains_key(&idx));
        low.retain(|&idx| !self.inner.lock().unwrap().map.contains_key(&idx));

        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        let epoch = self.epoch.load(Ordering::Relaxed);
        for idx in high {
            q.push(WorkerMsg::Decode {
                index: idx,
                priority: Priority::High,
                epoch,
            });
        }
        for idx in low {
            q.push(WorkerMsg::Decode {
                index: idx,
                priority: Priority::Low,
                epoch,
            });
        }
        cvar.notify_all();
    }

    fn enqueue(&self, index: usize, priority: Priority) {
        let epoch = self.epoch.load(Ordering::Relaxed);
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.push(WorkerMsg::Decode {
            index,
            priority,
            epoch,
        });
        cvar.notify_one();
    }

    fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }
}

fn window_centre(window: &[usize]) -> usize {
    if window.is_empty() {
        return 0;
    }
    let min = *window.iter().min().unwrap();
    let max = *window.iter().max().unwrap();
    min + (max - min) / 2
}

impl Drop for PageCache {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        for _ in 0..self.worker_count {
            q.push(WorkerMsg::Shutdown);
        }
        cvar.notify_all();
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    id: usize,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    cache: Arc<Mutex<CacheInner>>,
    initial: Arc<dyn PageSource>,
    initial_pipeline: Arc<Pipeline>,
    ctx: Context,
    epoch: Arc<AtomicU64>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("mmce-decoder-{id}"))
        .spawn(move || {
            let mut source = initial;
            let mut pipeline = initial_pipeline;
            loop {
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
                match msg {
                    WorkerMsg::Shutdown => break,
                    WorkerMsg::SwapSource(s) => source = s,
                    WorkerMsg::SwapPipeline(p) => pipeline = p,
                    WorkerMsg::Decode {
                        index,
                        epoch: req_epoch,
                        ..
                    } => {
                        // Skip stale requests (user jumped far away between
                        // enqueue and now).
                        if req_epoch < epoch.load(Ordering::Relaxed) {
                            continue;
                        }
                        let already = cache.lock().unwrap().map.contains_key(&index);
                        if already {
                            continue;
                        }
                        if index >= source.len() {
                            continue;
                        }
                        match source.read(index).and_then(|b| decode_image(&b)) {
                            Ok(img) => {
                                let filtered = if pipeline.is_identity() {
                                    img
                                } else {
                                    pipeline.apply(img)
                                };
                                let page = DecodedPage::from_dynamic(filtered);
                                cache.lock().unwrap().put(index, page);
                                ctx.request_repaint();
                            }
                            Err(e) => {
                                log::warn!("decode page {index} failed: {e}");
                            }
                        }
                    }
                }
            }
            let _ = id;
        })
        .expect("spawn decoder thread")
}

/// Compute the rendered size (in logical egui units) for a single page
/// within a viewport, honouring the fit mode and zoom.
pub fn compute_size(
    image_size: Vec2,
    viewport: Vec2,
    fit: FitMode,
    zoom: f32,
    no_zoom_in: bool,
) -> Vec2 {
    fit::fit_rect(FitParams {
        image: image_size,
        viewport,
        fit,
        zoom,
        no_zoom_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_prioritises_high_over_low() {
        let mut q = Queue::new();
        q.push(WorkerMsg::Decode {
            index: 1,
            priority: Priority::Low,
            epoch: 0,
        });
        q.push(WorkerMsg::Decode {
            index: 2,
            priority: Priority::High,
            epoch: 0,
        });
        match q.pop() {
            Some(WorkerMsg::Decode { index, .. }) => assert_eq!(index, 2),
            _ => panic!("expected decode msg"),
        }
        match q.pop() {
            Some(WorkerMsg::Decode { index, .. }) => assert_eq!(index, 1),
            _ => panic!("expected decode msg"),
        }
    }

    #[test]
    fn cancel_stale_drops_old_epochs() {
        let mut q = Queue::new();
        q.push(WorkerMsg::Decode {
            index: 1,
            priority: Priority::Low,
            epoch: 0,
        });
        q.push(WorkerMsg::Decode {
            index: 2,
            priority: Priority::Low,
            epoch: 1,
        });
        q.cancel_stale(1);
        assert_eq!(q.items.len(), 1);
        match q.pop() {
            Some(WorkerMsg::Decode { index, .. }) => assert_eq!(index, 2),
            _ => panic!("expected decode msg"),
        }
    }

    #[test]
    fn cancel_stale_keeps_control_messages() {
        let mut q = Queue::new();
        q.push(WorkerMsg::Decode {
            index: 1,
            priority: Priority::Low,
            epoch: 0,
        });
        q.push(WorkerMsg::Shutdown);
        q.cancel_stale(5);
        // Only the control message should survive.
        assert_eq!(q.items.len(), 1);
        assert!(matches!(q.pop(), Some(WorkerMsg::Shutdown)));
    }

    #[test]
    fn window_centre_of_single_item_is_item() {
        assert_eq!(window_centre(&[7]), 7);
    }

    #[test]
    fn window_centre_of_spread_is_midpoint() {
        assert_eq!(window_centre(&[4, 5]), 4);
        assert_eq!(window_centre(&[10, 20]), 15);
    }
}
