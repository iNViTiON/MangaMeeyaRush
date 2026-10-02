//! Explorer gallery view: a grid of cover thumbnails for folders, archives
//! and loose images in a browsed directory. Click a tile to open the
//! item, or navigate with the keyboard (arrows + Enter + Backspace).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Color32, Context, Layout, ScrollArea, Sense, TextStyle, Ui, Vec2};
use mmce_codecs::{is_archive_path, is_image_path, open_source, PageSource};

use crate::thumbs::{PageRef, ThumbStatus, ThumbnailCache};

/// Type-to-jump: keystrokes closer together than this extend one search.
const TYPEAHEAD_TIMEOUT: Duration = Duration::from_millis(1000);

/// Minimum / maximum tile footprint (width). Drives thumb size and label
/// area together so aspect stays readable.
pub const TILE_W_MIN: f32 = 90.0;
pub const TILE_W_MAX: f32 = 320.0;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub label: String,
    pub kind: EntryKind,
    /// Page index when this tile is a page of the archive being browsed
    /// (`EntryKind::Page`); `path` is then a unique key, not a real file.
    pub page: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    ParentDir,
    Folder,
    Archive,
    Image,
    /// A page inside the archive the explorer is showing as thumbnails.
    Page,
}

/// The explorer's browsing state — which directory we're showing.
pub struct ExplorerState {
    pub current: PathBuf,
    entries: Vec<Entry>,
    /// Lowercased filter substring; empty = no filter.
    pub filter: String,
    /// Whether the filter bar is visible. The filter bar stays open while
    /// the user types; `Esc` or clicking ✕ clears and hides it.
    pub show_filter: bool,
    pub cache: ThumbnailCache,
    /// Keyboard selection index into the *visible* (post-filter) entries.
    pub selection: usize,
    /// Tile footprint in logical px.
    pub tile_w: f32,
    /// Thumbnail box in pixels. Decoder uses this as its target resolution.
    pub thumb_size: u32,
    /// Last-observed columns — used for up/down keyboard navigation.
    pub columns: usize,
    /// Set while showing an archive's pages as thumbnails instead of a
    /// directory listing; `current` is then the archive file itself.
    pages: Option<Arc<dyn PageSource>>,
    /// What the scroll area was last centred on. The selected tile is only
    /// scrolled into view when this changes, so wheel / trackpad scrolling
    /// isn't fought by a per-frame "keep the selection centred".
    scrolled_to: Option<(PathBuf, String, usize, u32)>,
    /// Type-to-jump buffer and when it was last extended.
    typed: String,
    typed_at: Option<Instant>,
}

impl ExplorerState {
    pub fn new(ctx: &Context, start: PathBuf) -> Self {
        let mut s = Self {
            current: start,
            entries: Vec::new(),
            filter: String::new(),
            show_filter: false,
            cache: ThumbnailCache::new(ctx.clone(), 768),
            selection: 0,
            // Three `thumb_smaller` steps below the previous 180.0 default:
            // 180 / 1.2³ ≈ 104.2. `thumb_size` follows `tile_w * 0.89`
            // rounded to the nearest 8 (see `set_tile_width`) → 88.
            tile_w: 104.0,
            thumb_size: 88,
            columns: 1,
            pages: None,
            scrolled_to: None,
            typed: String::new(),
            typed_at: None,
        };
        s.refresh();
        s
    }

    pub fn cd(&mut self, path: PathBuf) {
        if path != self.current || self.pages.is_some() {
            self.cache.cancel_dir();
        }
        self.pages = None;
        self.current = path;
        self.selection = 0;
        self.refresh();
    }

    /// Show `archive`'s pages as thumbnails (the archive-file counterpart of
    /// browsing a folder of images) with page `page` selected. Returns false,
    /// leaving the view untouched, if the archive can't be opened.
    pub fn show_pages(&mut self, archive: &Path, page: usize) -> bool {
        let reuse = self.pages.is_some() && self.current == archive;
        if !reuse {
            let Ok(src) = open_source(archive) else {
                return false;
            };
            self.cache.cancel_dir();
            self.pages = Some(Arc::from(src));
            self.current = archive.to_path_buf();
            self.refresh();
        }
        self.selection = self
            .entries
            .iter()
            .position(|e| e.page == Some(page))
            .unwrap_or(0);
        true
    }

    /// The archive whose pages are on screen, if any.
    pub fn browsing_archive(&self) -> Option<&Path> {
        self.pages.as_ref().map(|_| self.current.as_path())
    }

    pub fn refresh(&mut self) {
        self.entries = match &self.pages {
            Some(src) => list_pages(&self.current, src.as_ref()),
            None => list_dir(&self.current),
        };
        if self.selection >= self.entries.len() {
            self.selection = self.entries.len().saturating_sub(1);
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entries actually on screen after the filter is applied.
    pub fn visible_entries(&self) -> Vec<&Entry> {
        if self.filter.is_empty() {
            return self.entries.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.label.to_lowercase().contains(&needle))
            .collect()
    }

    /// Selected path in the filtered view, if any.
    pub fn selected_path(&self) -> Option<PathBuf> {
        // Page tiles aren't files: rename / delete must never see them.
        self.visible_entries()
            .get(self.selection)
            .filter(|e| e.page.is_none())
            .map(|e| e.path.clone())
    }

    /// True while a type-to-jump search is in progress (its buffer hasn't
    /// timed out). Keys that normally act on their own (like `E`) are typed
    /// instead, so words containing them can be completed.
    pub fn typing(&self, now: Instant) -> bool {
        !self.typed.is_empty()
            && self
                .typed_at
                .is_some_and(|t| now.duration_since(t) < TYPEAHEAD_TIMEOUT)
    }

    /// Type-to-jump: extend the search with `text` and select the first
    /// visible entry whose name starts with it (case-insensitive), searching
    /// forward from the selection and wrapping. A lone character starts
    /// *after* the selection and pressing the same one again cycles through
    /// the entries with that initial. Returns whether the selection moved.
    pub fn type_jump(&mut self, text: &str, now: Instant) -> bool {
        if !self.typing(now) {
            self.typed.clear();
        }
        for c in text.chars().filter(|c| !c.is_control()) {
            if c.is_whitespace() && self.typed.is_empty() {
                continue;
            }
            self.typed.push(c);
        }
        if self.typed.is_empty() {
            return false;
        }
        self.typed_at = Some(now);

        let lower: Vec<char> = self.typed.to_lowercase().chars().collect();
        let first = lower[0];
        let cycling = lower.iter().all(|&c| c == first);
        let needle: String = if cycling {
            first.to_string()
        } else {
            lower.iter().collect()
        };
        let start = if cycling {
            self.selection + 1
        } else {
            self.selection
        };
        let names: Vec<String> = self
            .visible_entries()
            .iter()
            .map(|e| {
                if e.kind == EntryKind::ParentDir {
                    String::new()
                } else {
                    e.label.to_lowercase()
                }
            })
            .collect();
        let n = names.len();
        let hit = (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| names[i].starts_with(&needle));
        match hit {
            Some(i) if i != self.selection => {
                self.selection = i;
                true
            }
            _ => false,
        }
    }

    /// Keep thumbnails and tiles proportional. Re-decode on change; existing
    /// textures keep being drawn (scaled) until the new size lands.
    pub fn set_tile_width(&mut self, w: f32) {
        let clamped = w.clamp(TILE_W_MIN, TILE_W_MAX);
        if (clamped - self.tile_w).abs() < 0.5 {
            return;
        }
        self.tile_w = clamped;
        // Thumb size ~ 89% of tile width, rounded to nearest 8 for nicer
        // LRU reuse when the user flips back and forth.
        let target = (clamped * 0.89) as u32;
        self.thumb_size = (target / 8) * 8;
        self.cache.cancel_queued();
    }

    pub fn thumb_bigger(&mut self) {
        self.set_tile_width(self.tile_w * 1.2);
    }

    pub fn thumb_smaller(&mut self) {
        self.set_tile_width(self.tile_w / 1.2);
    }

    /// Selected entry, if any.
    pub fn selected(&self) -> Option<&Entry> {
        // `selection` indexes the filtered view, not `entries`.
        self.visible_entries().get(self.selection).copied()
    }

    pub fn move_selection(&mut self, dx: isize, dy: isize) {
        let n = self.visible_entries().len();
        if n == 0 {
            return;
        }
        let cols = self.columns.max(1) as isize;
        let cur = self.selection as isize;
        let want = cur + dx + dy * cols;
        let last = (n - 1) as isize;
        self.selection = want.clamp(0, last) as usize;
    }

    pub fn go_parent(&mut self) -> Option<PathBuf> {
        let up = self.current.parent()?.to_path_buf();
        let was = self.current.clone();
        self.cd(up);
        // Best-effort: highlight where we came from.
        if let Some(i) = self
            .entries
            .iter()
            .position(|e| e.path == was && e.kind != EntryKind::ParentDir)
        {
            self.selection = i;
        }
        Some(self.current.clone())
    }

    /// Rename the currently-selected entry and refresh. `new_name` is the
    /// new *filename* (no directory part); it's placed in the entry's
    /// parent directory.
    pub fn rename_selected(&mut self, new_name: &str) -> std::io::Result<()> {
        let Some(src) = self.selected_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no selection",
            ));
        };
        let parent = src.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir")
        })?;
        let dst = parent.join(new_name);
        if dst.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "target already exists",
            ));
        }
        std::fs::rename(&src, &dst)?;
        self.refresh();
        // Re-select the renamed entry if we can find it in the filtered view.
        if let Some(i) = self.visible_entries().iter().position(|e| e.path == dst) {
            self.selection = i;
        }
        Ok(())
    }

    /// Delete the currently-selected entry (recursively for directories).
    pub fn delete_selected(&mut self) -> std::io::Result<()> {
        let Some(path) = self.selected_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no selection",
            ));
        };
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        self.refresh();
        Ok(())
    }
}

fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();

    if let Some(parent) = dir.parent() {
        out.push(Entry {
            path: parent.to_path_buf(),
            label: "⬆ Parent".into(),
            kind: EntryKind::ParentDir,
            page: None,
        });
    }

    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    let mut folders: Vec<Entry> = Vec::new();
    let mut archives: Vec<Entry> = Vec::new();
    let mut images: Vec<Entry> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        let label = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if label.starts_with('.') {
            continue;
        }
        if ft.is_dir() {
            folders.push(Entry {
                path: p,
                label,
                kind: EntryKind::Folder,
                page: None,
            });
        } else if ft.is_file() {
            if is_archive_path(&p) {
                archives.push(Entry {
                    path: p,
                    label,
                    kind: EntryKind::Archive,
                    page: None,
                });
            } else if is_image_path(&p) {
                images.push(Entry {
                    path: p,
                    label,
                    kind: EntryKind::Image,
                    page: None,
                });
            }
        }
    }
    folders.sort_by(|a, b| natord::compare(&a.label, &b.label));
    archives.sort_by(|a, b| natord::compare(&a.label, &b.label));
    images.sort_by(|a, b| natord::compare(&a.label, &b.label));

    out.extend(folders);
    out.extend(archives);
    out.extend(images);
    out
}

fn page_ref(pages: Option<&Arc<dyn PageSource>>, entry: &Entry) -> Option<PageRef> {
    Some(PageRef {
        source: pages?.clone(),
        index: entry.page?,
    })
}

/// Entries for an archive shown as thumbnails: a tile per page, in reading
/// order, behind a parent tile that leaves the archive.
fn list_pages(archive: &Path, src: &dyn PageSource) -> Vec<Entry> {
    let mut out = Vec::with_capacity(src.len() + 1);
    if let Some(parent) = archive.parent() {
        out.push(Entry {
            path: parent.to_path_buf(),
            label: "⬆ Parent".into(),
            kind: EntryKind::ParentDir,
            page: None,
        });
    }
    for i in 0..src.len() {
        let name = src.entry_name(i).unwrap_or("");
        let label = name.rsplit(['/', '\\']).next().unwrap_or(name).to_string();
        out.push(Entry {
            // Unique cache key; never touched on disk.
            path: PathBuf::from(format!("{}\u{0}{i}", archive.display())),
            label,
            kind: EntryKind::Page,
            page: Some(i),
        });
    }
    out
}

/// Paint the explorer gallery. Returns the path the user clicked, if any.
/// Caller decides whether that path becomes the new `current` (navigation)
/// or the opened book.
pub fn draw(ui: &mut Ui, state: &mut ExplorerState) -> Option<Entry> {
    let mut clicked: Option<Entry> = None;

    ui.horizontal(|ui| {
        ui.heading("📂 Explorer");
        ui.separator();
        ui.label(
            egui::RichText::new(state.current.display().to_string()).color(Color32::LIGHT_GRAY),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{} px", state.thumb_size))
                    .small()
                    .color(Color32::DARK_GRAY),
            );
            ui.label(
                egui::RichText::new("Ctrl ± resize")
                    .small()
                    .color(Color32::DARK_GRAY),
            );
        });
    });

    if state.show_filter {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Filter:")
                    .small()
                    .color(Color32::LIGHT_GRAY),
            );
            let resp = ui.text_edit_singleline(&mut state.filter);
            resp.request_focus();
            if ui.small_button("✕").clicked() {
                state.filter.clear();
                state.show_filter = false;
            }
            let count = state.visible_entries().len();
            ui.label(
                egui::RichText::new(format!("{count} matches"))
                    .small()
                    .color(Color32::DARK_GRAY),
            );
        });
    }
    ui.separator();

    let tile_w = state.tile_w;
    let tile_h = state.thumb_size as f32 + 56.0;
    let thumb_h = state.thumb_size as f32;
    let avail_w = ui.available_width();
    let columns = ((avail_w / tile_w).floor() as usize).max(1);
    state.columns = columns;

    // Snapshot mutable data into stack vars so we can borrow &self below.
    let selection = state.selection;
    let thumb_size = state.thumb_size;

    // Materialise the filtered view once, then walk it in visual order.
    // `selection` is an index into this vector.
    let visible: Vec<Entry> = state.visible_entries().into_iter().cloned().collect();

    // Collect paths we rendered (so we can prefetch the N rows above/below
    // the visible clip on the next pass). `visible_rects` pairs each entry
    // with the rect we reserved for it inside the scroll area.
    let mut visible_rects: Vec<(PathBuf, egui::Rect, Option<PageRef>)> = Vec::new();

    // Centre the selected tile only when the selection (or what it indexes)
    // changed, not every frame — otherwise mouse / trackpad scrolling snaps
    // straight back to it.
    let scroll_key = (
        state.current.clone(),
        state.filter.clone(),
        state.selection,
        state.tile_w.to_bits(),
    );
    let scroll_to_selection = state.scrolled_to.as_ref() != Some(&scroll_key);
    let pages = state.pages.clone();

    let clip = ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            let clip_rect = ui.clip_rect();
            if visible.is_empty() {
                let msg = if state.filter.is_empty() {
                    "(empty folder)"
                } else {
                    "(no matches)"
                };
                ui.colored_label(Color32::GRAY, msg);
                return clip_rect;
            }
            let mut it = visible.iter().enumerate();
            loop {
                let row: Vec<(usize, &Entry)> = it.by_ref().take(columns).collect();
                if row.is_empty() {
                    break;
                }
                ui.horizontal(|ui| {
                    for (i, entry) in row {
                        let selected = i == selection;
                        let (hit, rect) = draw_tile(
                            ui,
                            entry,
                            &state.cache,
                            tile_w,
                            tile_h,
                            thumb_h,
                            thumb_size,
                            selected,
                            selected && scroll_to_selection,
                            pages.as_ref(),
                        );
                        if hit {
                            clicked = Some(entry.clone());
                        }
                        // The parent tile is a glyph, not a cover.
                        if entry.kind != EntryKind::ParentDir {
                            visible_rects.push((
                                entry.path.clone(),
                                rect,
                                page_ref(pages.as_ref(), entry),
                            ));
                        }
                    }
                });
            }
            clip_rect
        })
        .inner;

    // Prefetch off-screen tiles in the current filtered view at low
    // priority, nearest the viewport first. Visible tiles were already
    // queued at HIGH priority inside `draw_tile` via `thumbnail()`. Dedup in
    // the worker pool means re-calling this each frame is idempotent.
    prefetch_nearby(&state.cache, &visible_rects, clip, thumb_size);
    state.scrolled_to = Some(scroll_key);

    clicked
}

/// Upper bound on decoded-but-not-yet-shown thumbnail pixels the prefetcher
/// may ask for. Whole-directory prefetch of a huge folder at the largest tile
/// size would otherwise stage gigabytes of RGBA.
const PREFETCH_BUDGET_BYTES: usize = 128 << 20;

fn prefetch_nearby(
    cache: &ThumbnailCache,
    rects: &[(PathBuf, egui::Rect, Option<PageRef>)],
    clip: egui::Rect,
    thumb_size: u32,
) {
    // Square RGBA is the worst case per tile; portrait covers use less.
    let per_tile = (thumb_size as usize).pow(2).max(1) * 4;
    let budget = (PREFETCH_BUDGET_BYTES / per_tile).max(64);

    // Visible tiles took the HIGH-priority path in `thumbnail()`. Fan out
    // from them, two rows' worth below for every one above: the low queue is
    // FIFO, so this order is the order covers arrive in, and users mostly
    // scroll forward.
    let on_screen = |r: &egui::Rect| clip.intersects(*r);
    let first = rects.iter().position(|(_, r, _)| on_screen(r)).unwrap_or(0);
    let last = rects
        .iter()
        .rposition(|(_, r, _)| on_screen(r))
        .map_or(first, |i| i + 1);
    let mut below = rects[last..].iter();
    let mut above = rects[..first].iter().rev();
    let mut n = 0;
    while n < budget {
        let batch = [below.next(), below.next(), above.next()];
        if batch.iter().all(Option::is_none) {
            break;
        }
        for (path, _, src) in batch.into_iter().flatten() {
            cache.prefetch(path, thumb_size, src.as_ref());
            n += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tile(
    ui: &mut Ui,
    entry: &Entry,
    cache: &ThumbnailCache,
    tile_w: f32,
    tile_h: f32,
    thumb_h: f32,
    thumb_size: u32,
    selected: bool,
    scroll_into_view: bool,
    pages: Option<&Arc<dyn PageSource>>,
) -> (bool, egui::Rect) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());

    // Selection always requests scroll-into-view so keyboard nav can move
    // off-screen selection back into sight. Must run before the clip-rect
    // early-return below.
    if scroll_into_view {
        resp.scroll_to_me(Some(Align::Center));
    }

    // Skip all paint + thumbnail() requests for tiles outside the visible
    // viewport. The allocate_exact_size above still reserves space so the
    // scroll area sizes correctly — we just don't touch the cache. This is
    // the single biggest anti-flicker fix: the decoder queue no longer
    // drowns in requests for off-screen rows. We still return the reserved
    // rect so the caller can decide whether to prefetch this tile.
    if !ui.clip_rect().intersects(rect) {
        return (false, rect);
    }

    let painter = ui.painter_at(rect);
    let bg = if selected {
        Color32::from_rgb(56, 72, 120)
    } else if resp.hovered() {
        Color32::from_rgb(40, 40, 48)
    } else {
        Color32::from_rgb(24, 24, 28)
    };
    painter.rect_filled(rect, 6.0, bg);
    if selected {
        painter.rect_stroke(rect, 6.0, egui::Stroke::new(2.0_f32, Color32::LIGHT_BLUE));
    }

    let pad = 10.0;
    let thumb_rect = egui::Rect::from_min_size(
        rect.min + Vec2::new(pad, pad),
        Vec2::new(tile_w - pad * 2.0, thumb_h),
    );
    paint_thumb(ui, thumb_rect, entry, cache, thumb_size, pages);

    let label_rect = egui::Rect::from_min_size(
        rect.min + Vec2::new(pad, pad + thumb_h + 6.0),
        Vec2::new(tile_w - pad * 2.0, tile_h - thumb_h - pad * 2.0 - 6.0),
    );
    let color = match entry.kind {
        EntryKind::ParentDir => Color32::LIGHT_BLUE,
        EntryKind::Folder => Color32::from_rgb(230, 200, 120),
        EntryKind::Archive => Color32::from_rgb(180, 220, 255),
        EntryKind::Image | EntryKind::Page => Color32::WHITE,
    };
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(label_rect)
            .layout(Layout::top_down(Align::Center)),
        |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&entry.label)
                        .text_style(TextStyle::Body)
                        .color(color),
                )
                .truncate(),
            );
        },
    );

    // Show the full filename when labels get truncated by the tile width.
    let resp = resp.on_hover_text(&entry.label);
    (resp.clicked(), rect)
}

fn paint_thumb(
    ui: &mut Ui,
    rect: egui::Rect,
    entry: &Entry,
    cache: &ThumbnailCache,
    thumb_size: u32,
    pages: Option<&Arc<dyn PageSource>>,
) {
    let painter = ui.painter_at(rect);
    if entry.kind == EntryKind::ParentDir {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "⬆",
            egui::FontId::proportional(rect.height() * 0.5),
            Color32::LIGHT_BLUE,
        );
        return;
    }

    match cache.thumbnail(&entry.path, thumb_size, page_ref(pages, entry).as_ref()) {
        ThumbStatus::Ready(tex) => {
            let s = tex.size_vec2();
            let scale = (rect.width() / s.x).min(rect.height() / s.y);
            let draw = s * scale;
            let top_left = rect.center() - draw * 0.5;
            let target = egui::Rect::from_min_size(top_left, draw);
            egui::Image::from_texture(&tex)
                .fit_to_exact_size(draw)
                .paint_at(ui, target);
        }
        ThumbStatus::Pending => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(rect.height() * 0.35),
                Color32::DARK_GRAY,
            );
        }
        ThumbStatus::Failed => {
            let glyph = match entry.kind {
                EntryKind::Folder => "📁",
                EntryKind::Archive => "📦",
                EntryKind::Image | EntryKind::Page => "🖼",
                EntryKind::ParentDir => "⬆",
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(rect.height() * 0.5),
                Color32::DARK_GRAY,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explorer over a temp dir holding `names` as (empty) folders.
    fn explorer_with(names: &[&str]) -> (tempfile::TempDir, ExplorerState) {
        let dir = tempfile::tempdir().unwrap();
        for n in names {
            fs::create_dir(dir.path().join(n)).unwrap();
        }
        let state = ExplorerState::new(&Context::default(), dir.path().to_path_buf());
        (dir, state)
    }

    fn selected_label(s: &ExplorerState) -> String {
        s.selected().unwrap().label.clone()
    }

    #[test]
    fn type_jump_selects_by_prefix_case_insensitively() {
        let (_d, mut s) = explorer_with(&["Alpha", "beta", "Bravo", "gamma"]);
        let t = Instant::now();
        assert!(s.type_jump("g", t));
        assert_eq!(selected_label(&s), "gamma");
        // A new search after the timeout; a longer prefix finds "Bravo".
        let t2 = t + Duration::from_secs(5);
        assert!(s.type_jump("br", t2));
        assert_eq!(selected_label(&s), "Bravo");
    }

    #[test]
    fn repeated_letter_cycles_through_initials() {
        let (_d, mut s) = explorer_with(&["alpha", "beta", "bravo", "bulb"]);
        let mut t = Instant::now();
        let mut seen = Vec::new();
        for _ in 0..4 {
            s.type_jump("b", t);
            seen.push(selected_label(&s));
            t += Duration::from_millis(100);
        }
        assert_eq!(seen, ["beta", "bravo", "bulb", "beta"]);
    }

    #[test]
    fn typing_times_out_and_starts_a_new_search() {
        let (_d, mut s) = explorer_with(&["abc", "xyz"]);
        let t = Instant::now();
        s.type_jump("x", t);
        assert!(s.typing(t + Duration::from_millis(500)));
        assert!(!s.typing(t + Duration::from_secs(2)));
        // After the timeout "a" is a fresh search, not "xa".
        assert!(s.type_jump("a", t + Duration::from_secs(2)));
        assert_eq!(selected_label(&s), "abc");
    }

    #[test]
    fn type_jump_matches_japanese_names() {
        let (_d, mut s) = explorer_with(&["あいう", "かきく", "ABC"]);
        assert!(s.type_jump("か", Instant::now()));
        assert_eq!(selected_label(&s), "かきく");
    }

    #[test]
    fn type_jump_ignores_leading_space_and_missing_matches() {
        let (_d, mut s) = explorer_with(&["alpha"]);
        let before = s.selection;
        assert!(!s.type_jump(" ", Instant::now()));
        assert!(!s.type_jump("q", Instant::now()));
        assert_eq!(s.selection, before);
    }

    #[test]
    fn type_jump_never_selects_the_parent_tile() {
        let (_d, mut s) = explorer_with(&["alpha"]);
        // "⬆ Parent" must not be reachable by typing its label.
        assert!(!s.type_jump("p", Instant::now()));
        assert_eq!(s.selected().unwrap().kind, EntryKind::ParentDir);
    }

    #[test]
    fn selected_follows_the_filter() {
        let (_d, mut s) = explorer_with(&["alpha", "beta"]);
        s.filter = "beta".into();
        s.selection = 0;
        assert_eq!(selected_label(&s), "beta");
    }

    #[test]
    fn archive_pages_are_listed_and_not_deletable() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("book.cbz");
        {
            use std::io::Write;
            let mut w = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let png = {
                let mut out = Vec::new();
                image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8))
                    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                    .unwrap();
                out
            };
            for n in ["ch/10.png", "ch/2.png", "ch/1.png"] {
                w.start_file(n, opts).unwrap();
                w.write_all(&png).unwrap();
            }
            w.finish().unwrap();
        }
        let mut s = ExplorerState::new(&Context::default(), dir.path().to_path_buf());
        assert!(s.show_pages(&zip_path, 1));
        assert_eq!(s.browsing_archive(), Some(zip_path.as_path()));
        let labels: Vec<_> = s.entries().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["⬆ Parent", "1.png", "2.png", "10.png"]);
        // Page 1 (0-based) is selected: "2.png".
        assert_eq!(selected_label(&s), "2.png");
        assert_eq!(s.selected().unwrap().page, Some(1));
        assert_eq!(
            s.selected_path(),
            None,
            "pages must not be rename/delete targets"
        );
        // Leaving the archive restores the directory listing, archive selected.
        s.go_parent();
        assert!(s.browsing_archive().is_none());
        assert_eq!(selected_label(&s), "book.cbz");
    }

    #[test]
    fn show_pages_fails_cleanly_on_a_broken_archive() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.zip");
        fs::write(&bad, b"not a zip").unwrap();
        let mut s = ExplorerState::new(&Context::default(), dir.path().to_path_buf());
        assert!(!s.show_pages(&bad, 0));
        assert!(s.browsing_archive().is_none());
    }
}
