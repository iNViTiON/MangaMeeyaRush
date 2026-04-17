//! Texture cache + background prefetch + fit geometry.
//!
//! The cache stores decoded CPU images (RGBA8) keyed by page index, with an
//! LRU eviction. A background worker thread decodes pages requested by the
//! app and pushes them into the cache. egui textures are created lazily on
//! the UI thread the first time a page is painted.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use egui::{ColorImage, Context, TextureHandle, TextureOptions, Vec2};
use image::GenericImageView;
use mmce_codecs::{decode_image, PageSource};
use mmce_config::FitMode;

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
        let rgba = img.to_rgba8().into_raw();
        Self {
            size: [w as usize, h as usize],
            pixels: Arc::new(rgba),
        }
    }

    pub fn to_color_image(&self) -> ColorImage {
        ColorImage::from_rgba_unmultiplied(self.size, &self.pixels)
    }
}

enum WorkerMsg {
    Decode { index: usize },
    SwapSource(Arc<dyn PageSource>),
    Shutdown,
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
    tex: Mutex<HashMap<usize, TextureHandle>>,
    tx: Sender<WorkerMsg>,
    _workers: Vec<thread::JoinHandle<()>>,
    source: Mutex<Arc<dyn PageSource>>,
    egui_ctx: Context,
}

impl PageCache {
    pub fn new(ctx: Context, source: Arc<dyn PageSource>, capacity: usize) -> Self {
        let inner = Arc::new(Mutex::new(CacheInner {
            order: VecDeque::new(),
            map: HashMap::new(),
            capacity: capacity.max(2),
        }));
        let (tx, rx) = mpsc::channel();
        let rx = Arc::new(Mutex::new(rx));
        let workers = (0..2)
            .map(|i| spawn_worker(i, rx.clone(), inner.clone(), source.clone(), ctx.clone()))
            .collect();
        Self {
            inner,
            tex: Mutex::new(HashMap::new()),
            tx,
            _workers: workers,
            source: Mutex::new(source),
            egui_ctx: ctx,
        }
    }

    /// Intrinsic size of a decoded page, if it's currently in the cache.
    /// Does not touch LRU order.
    pub fn page_dimensions(&self, idx: usize) -> Option<(u32, u32)> {
        let g = self.inner.lock().ok()?;
        g.map.get(&idx).map(|p| (p.size[0] as u32, p.size[1] as u32))
    }

    pub fn swap_source(&self, source: Arc<dyn PageSource>) {
        {
            let mut g = self.inner.lock().unwrap();
            g.order.clear();
            g.map.clear();
        }
        self.tex.lock().unwrap().clear();
        *self.source.lock().unwrap() = source.clone();
        let _ = self.tx.send(WorkerMsg::SwapSource(source));
    }

    /// Request a page; if cached, returns an egui texture handle. If not,
    /// schedules a decode.
    pub fn texture(&self, index: usize) -> Option<TextureHandle> {
        if let Some(t) = self.tex.lock().unwrap().get(&index) {
            return Some(t.clone());
        }
        let decoded = {
            let mut g = self.inner.lock().unwrap();
            g.get(index)
        };
        if let Some(d) = decoded {
            let ci = d.to_color_image();
            let handle = self.egui_ctx.load_texture(
                format!("mmce_page_{index}"),
                ci,
                TextureOptions::LINEAR,
            );
            self.tex.lock().unwrap().insert(index, handle.clone());
            Some(handle)
        } else {
            let _ = self.tx.send(WorkerMsg::Decode { index });
            None
        }
    }

    /// Hint: we're currently viewing these pages; decode them and the
    /// neighbours for snappy navigation.
    pub fn prefetch(&self, window: &[usize], neighbours: usize) {
        let len = self.source.lock().unwrap().len();
        let mut set: std::collections::BTreeSet<usize> = window.iter().copied().collect();
        for &w in window {
            for k in 1..=neighbours {
                if let Some(next) = w.checked_add(k) {
                    if next < len {
                        set.insert(next);
                    }
                }
                if let Some(prev) = w.checked_sub(k) {
                    set.insert(prev);
                }
            }
        }
        for idx in set {
            let already = self.inner.lock().unwrap().map.contains_key(&idx);
            if !already {
                let _ = self.tx.send(WorkerMsg::Decode { index: idx });
            }
        }
    }
}

impl Drop for PageCache {
    fn drop(&mut self) {
        // One Shutdown per worker so every thread exits.
        for _ in 0..self._workers.len() {
            let _ = self.tx.send(WorkerMsg::Shutdown);
        }
    }
}

fn spawn_worker(
    id: usize,
    rx: Arc<Mutex<Receiver<WorkerMsg>>>,
    cache: Arc<Mutex<CacheInner>>,
    initial: Arc<dyn PageSource>,
    ctx: Context,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("mmce-decoder-{id}"))
        .spawn(move || {
            let mut source = initial;
            loop {
                // Lock only around recv — decoding runs unlocked so workers
                // can process pages in parallel.
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
                match msg {
                    WorkerMsg::Shutdown => break,
                    WorkerMsg::SwapSource(s) => source = s,
                    WorkerMsg::Decode { index } => {
                        let already = cache.lock().unwrap().map.contains_key(&index);
                        if already {
                            continue;
                        }
                        if index >= source.len() {
                            continue;
                        }
                        match source.read(index).and_then(|b| decode_image(&b)) {
                            Ok(img) => {
                                let page = DecodedPage::from_dynamic(img);
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
