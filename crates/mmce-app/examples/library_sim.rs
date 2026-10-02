//! Headless browse simulation over a real or synthetic library.
//!
//! Drives the real explorer gallery (`explorer::draw` + `ThumbnailCache`)
//! and the real reader cache (`mmce_core::Book` + `mmce_render::PageCache`)
//! through egui frames with no window and no GPU, paced like eframe: a
//! frame runs when something requests a repaint, at most once per 16.7 ms.
//! Prints a summary plus one `RESULT key=value ...` line for scripts.
//!
//! ```sh
//! cargo run --release -p mmce-app --example library_sim -- <scenario> <path> [--key value]...
//! ```
//!
//! | scenario  | path | what it does |
//! |-----------|------|--------------|
//! | `scan`    | dir  | open the explorer, wait until every thumbnail is decoded |
//! | `scroll`  | dir  | settle the first screen, fling to the end one screen per `--step-ms`, settle, fling back |
//! | `folders` | dir  | visit each subfolder in explorer order; settle and back out, or hop siblings every `--dwell-ms` |
//! | `read`    | book | read forward (`--spreads`, `--flip-ms` after each spread shows), back 20, then a big jump |
//! | `hop`     | dir  | open and leave `--books` books, `--dwell-ms` each |
//!
//! Options: `--screen 1920x1080`, `--timeout-s 300`. For cold runs drop the
//! page cache first (`sync; echo 3 > /proc/sys/vm/drop_caches`).
//! `MMCE_THUMB_WORKERS` sizes the thumbnail pool exactly as in the app.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use mmce_app::explorer::{self, EntryKind, ExplorerState};
use mmce_app::thumbs::ThumbStats;
use mmce_config::{BindDir, PageMode};
use mmce_core::Book;
use mmce_render::PageCache;

const VSYNC: Duration = Duration::from_micros(16_667);
static PANICS: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// eframe-like frame pacing.

struct Pacer {
    ctx: egui::Context,
    t0: Instant,
    screen: egui::Vec2,
    /// Earliest requested repaint, set by egui's repaint callback (which
    /// worker threads trigger via `request_repaint`).
    wake: Arc<(Mutex<Option<Instant>>, Condvar)>,
    last_frame: Option<Instant>,
    frame_ms: Vec<f32>,
    rss_peak_kb: u64,
    threads_peak: u64,
    last_sample: Instant,
}

impl Pacer {
    fn new(screen: egui::Vec2) -> Self {
        let ctx = egui::Context::default();
        let wake = Arc::new((Mutex::new(Some(Instant::now())), Condvar::new()));
        let w = wake.clone();
        ctx.set_request_repaint_callback(move |info| {
            let at = Instant::now() + info.delay;
            let (m, cv) = &*w;
            let mut g = m.lock().unwrap();
            if g.is_none_or(|t| at < t) {
                *g = Some(at);
            }
            cv.notify_all();
        });
        // egui builds its font atlas on the first pass (~150 ms); the app
        // pays that at startup, not when the explorer opens.
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| ui.label("warm-up"));
        });
        Self {
            ctx,
            t0: Instant::now(),
            screen,
            wake,
            last_frame: None,
            frame_ms: Vec::new(),
            rss_peak_kb: 0,
            threads_peak: 0,
            last_sample: Instant::now(),
        }
    }

    /// Block until a repaint is due or `max_wait` passes, then honour vsync.
    fn wait(&mut self, max_wait: Duration) {
        let give_up = Instant::now() + max_wait;
        {
            let (m, cv) = &*self.wake;
            let mut g = m.lock().unwrap();
            loop {
                let now = Instant::now();
                if g.is_some_and(|t| t <= now) || now >= give_up {
                    break;
                }
                let until = g.map_or(give_up, |t| t.min(give_up));
                g = cv.wait_timeout(g, until - now).unwrap().0;
            }
            *g = None;
        }
        if let Some(next) = self.last_frame.map(|t| t + VSYNC) {
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            }
        }
    }

    fn frame(&mut self, mut ui_fn: impl FnMut(&mut egui::Ui)) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)),
            time: Some(self.t0.elapsed().as_secs_f64()),
            focused: true,
            ..Default::default()
        };
        let t = Instant::now();
        // Texture deltas are dropped: there is no GPU. egui still tracks
        // allocations, so freed handles behave as in the app.
        let _ = self.ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| ui_fn(ui));
        });
        self.frame_ms.push(t.elapsed().as_secs_f32() * 1e3);
        self.last_frame = Some(t);
        if self.last_sample.elapsed() > Duration::from_millis(50) {
            self.sample();
        }
    }

    fn sample(&mut self) {
        self.last_sample = Instant::now();
        let (rss, threads) = proc_status();
        self.rss_peak_kb = self.rss_peak_kb.max(rss);
        self.threads_peak = self.threads_peak.max(threads);
    }

    fn frame_summary(&self) -> String {
        let mut v = self.frame_ms.clone();
        if v.is_empty() {
            return "frames=0".into();
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
        format!(
            "frames={} ui_ms_p50={:.2} ui_ms_p95={:.2} ui_ms_max={:.1}",
            v.len(),
            q(0.5),
            q(0.95),
            v[v.len() - 1]
        )
    }
}

/// (VmRSS kB, thread count) of this process.
fn proc_status() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (field("VmRSS:"), field("Threads:"))
}

fn vm_hwm_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        / 1024.0
}

fn ms(d: Duration) -> String {
    format!("{:.0}", d.as_secs_f64() * 1e3)
}

fn opt_ms(d: Option<Duration>) -> String {
    d.map_or("-".into(), ms)
}

fn quantiles_ms(mut v: Vec<f64>) -> String {
    if v.is_empty() {
        return "n=0".into();
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    format!(
        "n={} p50={:.0} p90={:.0} p99={:.0} max={:.0}",
        v.len(),
        q(0.5),
        q(0.9),
        q(0.99),
        v[v.len() - 1]
    )
}

// ---------------------------------------------------------------------------
// Explorer scenarios.

/// Per-frame bookkeeping for "how long until the screen is filled".
#[derive(Default)]
struct Settle {
    first_thumb: Option<Duration>,
    visible_ready: Option<Duration>,
    visible_tiles: u64,
}

impl Settle {
    /// Feed one frame's stats; `since` is the time origin for this settle.
    fn observe(&mut self, st: &ThumbStats, uploads_before: u64, since: Instant) {
        if self.first_thumb.is_none() && st.uploads > uploads_before {
            self.first_thumb = Some(since.elapsed());
        }
        if self.visible_ready.is_none() && st.calls > 0 && st.calls_pending == 0 {
            self.visible_ready = Some(since.elapsed());
            self.visible_tiles = st.calls;
        }
    }
}

fn cover_entries(state: &ExplorerState) -> usize {
    state
        .entries()
        .iter()
        .filter(|e| e.kind != EntryKind::ParentDir)
        .count()
}

/// Run frames until the visible tiles are all ready (or `limit`).
fn settle(p: &mut Pacer, state: &mut ExplorerState, limit: Duration) -> (Settle, ThumbStats) {
    let since = Instant::now();
    let base = state.cache.take_stats();
    let mut s = Settle::default();
    let mut last = base;
    while s.visible_ready.is_none() && since.elapsed() < limit {
        p.wait(Duration::from_millis(100));
        p.frame(|ui| {
            explorer::draw(ui, state);
        });
        last = state.cache.take_stats();
        s.observe(&last, base.uploads, since);
    }
    (s, last)
}

fn scan(dir: &Path, o: &Opts) {
    let mut p = Pacer::new(o.screen);
    let t0 = Instant::now();
    let mut state = ExplorerState::new(&p.ctx, dir.to_path_buf());
    let t_list = t0.elapsed();
    let n = cover_entries(&state);
    let mut s = Settle::default();
    let mut done = None;
    let mut st = state.cache.take_stats();
    let (mut staged_peak, mut staged_bytes_peak, mut queued_peak) = (0, 0, 0);
    while t0.elapsed() < o.timeout {
        p.wait(Duration::from_millis(100));
        p.frame(|ui| {
            explorer::draw(ui, &mut state);
        });
        st = state.cache.take_stats();
        s.observe(&st, 0, t0);
        staged_peak = staged_peak.max(st.staged);
        staged_bytes_peak = staged_bytes_peak.max(st.staged_bytes);
        queued_peak = queued_peak.max(st.queued);
        if st.pending == 0 && st.decodes as usize >= n {
            done = Some(t0.elapsed());
            break;
        }
    }
    p.sample();
    let all = done.unwrap_or(t0.elapsed());
    let rate = st.decodes as f64 / all.as_secs_f64();
    println!(
        "scan {}: {n} entries, workers {} | list {} ms | first thumb {} ms | screen ({} tiles) {} ms | all {} ms{} | {:.0} thumbs/s",
        dir.display(),
        st.workers,
        ms(t_list),
        opt_ms(s.first_thumb),
        s.visible_tiles,
        opt_ms(s.visible_ready),
        ms(all),
        if done.is_none() { " (TIMEOUT)" } else { "" },
        rate,
    );
    println!(
        "  decodes {} uploads {} textures {} staged_peak {} ({:.0} MB) queued_peak {} | {} | rss_peak {:.0} MB threads_peak {}",
        st.decodes,
        st.uploads,
        st.textures,
        staged_peak,
        staged_bytes_peak as f64 / 1e6,
        queued_peak,
        p.frame_summary(),
        vm_hwm_mb(),
        p.threads_peak,
    );
    println!(
        "RESULT scenario=scan workers={} entries={n} list_ms={} first_ms={} screen_ms={} all_ms={} done={} rate={rate:.1} decodes={} uploads={} textures={} staged_peak={staged_peak} staged_mb_peak={:.1} queued_peak={queued_peak} rss_mb={:.0} {} panics={}",
        st.workers,
        ms(t_list),
        opt_ms(s.first_thumb),
        opt_ms(s.visible_ready),
        ms(all),
        done.is_some(),
        st.decodes,
        st.uploads,
        st.textures,
        staged_bytes_peak as f64 / 1e6,
        vm_hwm_mb(),
        p.frame_summary(),
        PANICS.load(Ordering::Relaxed),
    );
}

fn scroll(dir: &Path, o: &Opts) {
    let mut p = Pacer::new(o.screen);
    let mut state = ExplorerState::new(&p.ctx, dir.to_path_buf());
    let n = state.visible_entries().len();
    let (first, _) = settle(&mut p, &mut state, o.timeout);
    let rows = ((o.screen.y - 70.0) / (state.thumb_size as f32 + 60.0))
        .floor()
        .max(1.0) as isize;

    let fling = |p: &mut Pacer, state: &mut ExplorerState, down: bool| {
        let before = state.cache.take_stats();
        let t = Instant::now();
        let mut next = Instant::now();
        let mut steps = 0;
        loop {
            let at_end = if down {
                state.selection + 1 >= n
            } else {
                state.selection == 0
            };
            if at_end {
                break;
            }
            if Instant::now() >= next {
                state.move_selection(0, if down { rows } else { -rows });
                steps += 1;
                next += o.step;
            }
            p.wait(next.saturating_duration_since(Instant::now()));
            p.frame(|ui| {
                explorer::draw(ui, state);
            });
        }
        let fling_time = t.elapsed();
        let (s, after) = settle(p, state, o.timeout);
        (fling_time, steps, s, before, after)
    };

    let (f_down, steps_down, s_down, b_down, a_down) = fling(&mut p, &mut state, true);
    let (f_up, steps_up, s_up, _, a_up) = fling(&mut p, &mut state, false);
    p.sample();
    println!(
        "scroll {}: {n} entries | first screen {} ms | fling down {} screens in {} ms -> settle {} ms ({} decodes during fling+settle, discarded {}) | fling up {} screens in {} ms -> settle {} ms",
        dir.display(),
        opt_ms(first.visible_ready),
        steps_down,
        ms(f_down),
        opt_ms(s_down.visible_ready),
        a_down.decodes - b_down.decodes,
        a_down.discarded,
        steps_up,
        ms(f_up),
        opt_ms(s_up.visible_ready),
    );
    println!(
        "RESULT scenario=scroll workers={} entries={n} first_ms={} down_steps={steps_down} down_fling_ms={} down_settle_ms={} up_steps={steps_up} up_fling_ms={} up_settle_ms={} decodes={} textures={} staged={} rss_mb={:.0} {} panics={}",
        a_up.workers,
        opt_ms(first.visible_ready),
        ms(f_down),
        opt_ms(s_down.visible_ready),
        ms(f_up),
        opt_ms(s_up.visible_ready),
        a_up.decodes,
        a_up.textures,
        a_up.staged,
        vm_hwm_mb(),
        p.frame_summary(),
        PANICS.load(Ordering::Relaxed),
    );
}

fn folders(dir: &Path, o: &Opts) {
    let mut p = Pacer::new(o.screen);
    let mut state = ExplorerState::new(&p.ctx, dir.to_path_buf());
    settle(&mut p, &mut state, o.timeout);
    let subs: Vec<PathBuf> = state
        .entries()
        .iter()
        .filter(|e| e.kind == EntryKind::Folder)
        .map(|e| e.path.clone())
        .collect();
    let hop = !o.dwell.is_zero();
    let (mut firsts, mut screens, mut backs) = (Vec::new(), Vec::new(), Vec::new());
    let mut timeouts = 0;
    for sub in &subs {
        let before = state.cache.take_stats();
        state.cd(sub.clone());
        let n = cover_entries(&state);
        let limit = if hop { o.dwell } else { o.timeout };
        let (s, after) = settle(&mut p, &mut state, limit);
        if let Some(f) = s.first_thumb {
            firsts.push(f.as_secs_f64() * 1e3);
        }
        match s.visible_ready {
            Some(v) => screens.push(v.as_secs_f64() * 1e3),
            None if !hop => timeouts += 1,
            None => {}
        }
        if !hop {
            println!(
                "  {:<48} {:>3} entries | first {:>5} ms | screen {:>5} ms | decodes {:>4}",
                sub.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .chars()
                    .take(48)
                    .collect::<String>(),
                n,
                opt_ms(s.first_thumb),
                opt_ms(s.visible_ready),
                after.decodes - before.decodes,
            );
            state.go_parent();
            let (b, _) = settle(&mut p, &mut state, o.timeout);
            if let Some(v) = b.visible_ready {
                backs.push(v.as_secs_f64() * 1e3);
            }
        }
    }
    p.sample();
    let st = state.cache.take_stats();
    println!(
        "folders {} ({}): {} subfolders | first thumb ms {} | screen ms {} | back-out ms {} | decodes {} discarded {} staged {} textures {}",
        dir.display(),
        if hop { format!("hop every {} ms", ms(o.dwell)) } else { "settle + back".into() },
        subs.len(),
        quantiles_ms(firsts.clone()),
        quantiles_ms(screens.clone()),
        quantiles_ms(backs.clone()),
        st.decodes,
        st.discarded,
        st.staged,
        st.textures,
    );
    println!(
        "RESULT scenario=folders mode={} workers={} subfolders={} first_p50_ms={:.0} screen_p50_ms={:.0} screen_max_ms={:.0} back_p50_ms={:.0} timeouts={timeouts} decodes={} discarded={} rss_mb={:.0} panics={}",
        if hop { "hop" } else { "settle" },
        st.workers,
        subs.len(),
        median(&firsts),
        median(&screens),
        screens.iter().cloned().fold(0.0, f64::max),
        median(&backs),
        st.decodes,
        st.discarded,
        vm_hwm_mb(),
        PANICS.load(Ordering::Relaxed),
    );
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

// ---------------------------------------------------------------------------
// Reader scenarios.

const MODE: PageMode = PageMode::Spread;
const DIR: BindDir = BindDir::RightToLeft;
/// `anim::DEFAULT_DURATION`: the flip animation repaints every frame.
const FLIP_ANIM: Duration = Duration::from_millis(500);

struct Reader {
    book: Book,
    cache: PageCache,
    hint: isize,
    anim_until: Instant,
}

impl Reader {
    fn open(p: &Pacer, path: &Path) -> Result<(Self, Duration), String> {
        let t = Instant::now();
        let book = Book::open(path).map_err(|e| e.to_string())?;
        let open = t.elapsed();
        // As `App::open_path`: default PictureCacheSize 64, identity pipeline.
        let cache = PageCache::new(p.ctx.clone(), book.source().clone(), 64);
        cache.set_pipeline(mmce_filters::Pipeline::new());
        Ok((
            Self {
                book,
                cache,
                hint: 0,
                anim_until: Instant::now(),
            },
            open,
        ))
    }

    /// One app frame in Book view: `draw_spread`'s `texture()` calls, the
    /// spinner while pages are missing, the flip animation's repaints, then
    /// `prefetch_directed(&window, 4, 1, hint)` as in `App::update`.
    /// Returns whether the whole spread was drawable.
    fn frame(&mut self, p: &mut Pacer) -> bool {
        let window: Vec<usize> = self.book.current_spread(MODE, DIR).indices().collect();
        let cache = &self.cache;
        let animating = Instant::now() < self.anim_until;
        let mut shown = false;
        p.frame(|ui| {
            let got = window
                .iter()
                .filter(|&&i| cache.texture(i).is_some())
                .count();
            shown = got == window.len();
            if got == 0 {
                ui.spinner();
            }
            if animating {
                ui.ctx().request_repaint();
            }
        });
        self.cache.prefetch_directed(&window, 4, 1, self.hint);
        shown
    }

    /// Frames until the current spread is drawable; returns the latency.
    fn until_shown(&mut self, p: &mut Pacer, limit: Duration) -> Option<Duration> {
        let t = Instant::now();
        loop {
            if self.frame(p) {
                return Some(t.elapsed());
            }
            if t.elapsed() > limit {
                return None;
            }
            p.wait(Duration::from_millis(50));
        }
    }

    fn flip(&mut self, forward: bool) {
        if forward {
            self.book.next_spread(MODE);
        } else {
            self.book.prev_spread(MODE);
        }
        self.hint = if forward { 1 } else { -1 };
        self.anim_until = Instant::now() + FLIP_ANIM;
    }

    /// Keep framing for `d` (the reader looking at the page).
    fn dwell(&mut self, p: &mut Pacer, d: Duration) {
        let end = Instant::now() + d;
        while Instant::now() < end {
            p.wait(end.saturating_duration_since(Instant::now()));
            self.frame(p);
        }
    }
}

fn read(path: &Path, o: &Opts) {
    let mut p = Pacer::new(o.screen);
    let t0 = Instant::now();
    let (mut r, open) = match Reader::open(&p, path) {
        Ok(x) => x,
        Err(e) => {
            println!("RESULT scenario=read error={e:?}");
            return;
        }
    };
    let pages = r.book.len();
    let first = r.until_shown(&mut p, o.timeout).map(|_| t0.elapsed());

    let flips = |p: &mut Pacer, r: &mut Reader, n: usize, forward: bool| {
        let mut lat = Vec::new();
        let mut stalls = 0;
        for _ in 0..n {
            let before = r.book.cursor();
            r.dwell(p, o.flip);
            r.flip(forward);
            if r.book.cursor() == before {
                break;
            }
            match r.until_shown(p, o.timeout) {
                Some(l) => {
                    if l > VSYNC * 2 {
                        stalls += 1;
                    }
                    lat.push(l.as_secs_f64() * 1e3);
                }
                None => stalls += 1,
            }
        }
        (lat, stalls)
    };
    let (fwd, fwd_stalls) = flips(&mut p, &mut r, o.spreads, true);
    let (back, back_stalls) = flips(&mut p, &mut r, 20, false);
    // Big jump to the far end of the book: outside the 64-page cache, and
    // (with --flip-ms 0) while the prefetch queue is still full, so the
    // epoch-cancel path has stale work to drop.
    let before_jump = r.cache.stats();
    r.dwell(&mut p, o.flip);
    let target = if r.book.cursor() > pages / 2 {
        pages / 20
    } else {
        pages * 9 / 10
    };
    r.book.goto(target);
    r.hint = 0;
    let jump = r.until_shown(&mut p, o.timeout);
    r.dwell(&mut p, Duration::from_millis(1500));
    p.sample();
    let st = r.cache.stats();
    println!(
        "read {}: {pages} pages | open {} ms | first spread {} ms | flip every {} ms after shown",
        path.file_name().unwrap().to_string_lossy(),
        ms(open),
        opt_ms(first),
        ms(o.flip),
    );
    println!(
        "  forward  flip->shown ms {} | waits >2 frames: {fwd_stalls}",
        quantiles_ms(fwd.clone())
    );
    println!(
        "  backward flip->shown ms {} | waits >2 frames: {back_stalls}",
        quantiles_ms(back.clone())
    );
    println!(
        "  big jump to page {}: {} ms, stale requests skipped {}",
        target,
        opt_ms(jump),
        st.stale_skipped - before_jump.stale_skipped,
    );
    println!(
        "  decodes {} (duplicates {}, already-cached skips {}, failed {}) | cached {} pages {:.0} MB | textures {} | queued now {} | {} | rss_peak {:.0} MB",
        st.decodes,
        st.duplicate_decodes,
        st.already_skipped,
        st.failed,
        st.cached,
        st.cached_bytes as f64 / 1e6,
        st.textures,
        st.queued,
        p.frame_summary(),
        vm_hwm_mb(),
    );
    println!(
        "RESULT scenario=read pages={pages} workers={} open_ms={} first_ms={} fwd_p50={:.0} fwd_max={:.0} fwd_stalls={fwd_stalls} back_p50={:.0} back_stalls={back_stalls} jump_ms={} decodes={} dup={} already={} stale={} cached={} cached_mb={:.0} textures={} queued={} rss_mb={:.0} panics={}",
        st.workers,
        ms(open),
        opt_ms(first),
        median(&fwd),
        fwd.iter().cloned().fold(0.0, f64::max),
        median(&back),
        opt_ms(jump),
        st.decodes,
        st.duplicate_decodes,
        st.already_skipped,
        st.stale_skipped,
        st.cached,
        st.cached_bytes as f64 / 1e6,
        st.textures,
        st.queued,
        vm_hwm_mb(),
        PANICS.load(Ordering::Relaxed),
    );
}

fn hop(dir: &Path, o: &Opts) {
    let mut p = Pacer::new(o.screen);
    let mut books: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    books.retain(|b| mmce_codecs::is_archive_path(b));
    books.sort_by(|a, b| natord::compare(&a.to_string_lossy(), &b.to_string_lossy()));
    books.truncate(o.books);
    let (mut opens, mut firsts) = (Vec::new(), Vec::new());
    let (mut left_early, mut errors) = (0, 0);
    let t0 = Instant::now();
    for b in &books {
        let t = Instant::now();
        match Reader::open(&p, b) {
            Ok((mut r, open)) => {
                opens.push(open.as_secs_f64() * 1e3);
                match r.until_shown(&mut p, o.dwell) {
                    Some(_) => firsts.push(t.elapsed().as_secs_f64() * 1e3),
                    None => left_early += 1,
                }
                // Leaving the book drops its cache mid-decode, as the app does.
            }
            Err(_) => errors += 1,
        }
        p.sample();
    }
    let total = t0.elapsed();
    std::thread::sleep(Duration::from_millis(500));
    let (_, threads_after) = proc_status();
    println!(
        "hop {}: {} books, dwell {} ms | open ms {} | first spread ms {} | left before shown {left_early} | errors {errors} | {} ms total | threads peak {} after {} | rss_peak {:.0} MB",
        dir.display(),
        books.len(),
        ms(o.dwell),
        quantiles_ms(opens.clone()),
        quantiles_ms(firsts.clone()),
        ms(total),
        p.threads_peak,
        threads_after,
        vm_hwm_mb(),
    );
    println!(
        "RESULT scenario=hop books={} dwell_ms={} open_p50={:.1} open_max={:.1} first_p50={:.0} first_max={:.0} left_early={left_early} errors={errors} threads_peak={} threads_after={threads_after} rss_mb={:.0} panics={}",
        books.len(),
        ms(o.dwell),
        median(&opens),
        opens.iter().cloned().fold(0.0, f64::max),
        median(&firsts),
        firsts.iter().cloned().fold(0.0, f64::max),
        p.threads_peak,
        vm_hwm_mb(),
        PANICS.load(Ordering::Relaxed),
    );
}

// ---------------------------------------------------------------------------

struct Opts {
    screen: egui::Vec2,
    timeout: Duration,
    step: Duration,
    dwell: Duration,
    flip: Duration,
    spreads: usize,
    books: usize,
}

fn main() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::Relaxed);
        default_hook(info);
    }));

    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: library_sim <scan|scroll|folders|read|hop> <path> [--screen WxH] [--timeout-s N] [--step-ms N] [--dwell-ms N] [--flip-ms N] [--spreads N] [--books N]";
    let (Some(scenario), Some(path)) = (args.get(1), args.get(2)) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let opt = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let num = |k: &str, d: u64| opt(k).map_or(d, |v| v.parse().expect(k));
    let screen = opt("--screen")
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some(egui::vec2(w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or(egui::vec2(1920.0, 1080.0));
    let o = Opts {
        screen,
        timeout: Duration::from_secs(num("--timeout-s", 300)),
        step: Duration::from_millis(num("--step-ms", 50)),
        dwell: Duration::from_millis(num("--dwell-ms", 0)),
        flip: Duration::from_millis(num("--flip-ms", 400)),
        spreads: num("--spreads", 60) as usize,
        books: num("--books", 40) as usize,
    };
    let path = Path::new(path);
    match scenario.as_str() {
        "scan" => scan(path, &o),
        "scroll" => scroll(path, &o),
        "folders" => folders(path, &o),
        "read" => read(path, &o),
        "hop" => hop(
            path,
            &Opts {
                dwell: if o.dwell.is_zero() {
                    Duration::from_millis(250)
                } else {
                    o.dwell
                },
                ..o
            },
        ),
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
}
