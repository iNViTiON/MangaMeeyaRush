//! Explorer gallery view: a grid of cover thumbnails for folders, archives
//! and loose images in a browsed directory. Click a tile to open the
//! item, or navigate with the keyboard (arrows + Enter + Backspace).

use std::fs;
use std::path::{Path, PathBuf};

use egui::{Align, Color32, Context, Layout, ScrollArea, Sense, TextStyle, Ui, Vec2};
use mmce_codecs::{is_archive_path, is_image_path};

use crate::thumbs::{ThumbStatus, ThumbnailCache};

/// Minimum / maximum tile footprint (width). Drives thumb size and label
/// area together so aspect stays readable.
pub const TILE_W_MIN: f32 = 90.0;
pub const TILE_W_MAX: f32 = 320.0;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub label: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    ParentDir,
    Folder,
    Archive,
    Image,
}

/// The explorer's browsing state — which directory we're showing.
pub struct ExplorerState {
    pub current: PathBuf,
    entries: Vec<Entry>,
    pub cache: ThumbnailCache,
    /// Keyboard selection index into `entries`.
    pub selection: usize,
    /// Tile footprint in logical px.
    pub tile_w: f32,
    /// Thumbnail box in pixels. Decoder uses this as its target resolution.
    pub thumb_size: u32,
    /// Last-observed columns — used for up/down keyboard navigation.
    pub columns: usize,
}

impl ExplorerState {
    pub fn new(ctx: &Context, start: PathBuf) -> Self {
        let mut s = Self {
            current: start,
            entries: Vec::new(),
            cache: ThumbnailCache::new(ctx.clone(), 768),
            selection: 0,
            tile_w: 180.0,
            thumb_size: 160,
            columns: 1,
        };
        s.refresh();
        s
    }

    pub fn cd(&mut self, path: PathBuf) {
        self.current = path;
        self.selection = 0;
        self.refresh();
    }

    pub fn refresh(&mut self) {
        self.entries = list_dir(&self.current);
        if self.selection >= self.entries.len() {
            self.selection = self.entries.len().saturating_sub(1);
        }
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Keep thumbnails and tiles proportional. Re-decode on change.
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
        self.cache.clear();
    }

    pub fn thumb_bigger(&mut self) {
        self.set_tile_width(self.tile_w * 1.2);
    }

    pub fn thumb_smaller(&mut self) {
        self.set_tile_width(self.tile_w / 1.2);
    }

    /// Selected entry, if any.
    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.selection)
    }

    pub fn move_selection(&mut self, dx: isize, dy: isize) {
        if self.entries.is_empty() {
            return;
        }
        let cols = self.columns.max(1) as isize;
        let cur = self.selection as isize;
        let want = cur + dx + dy * cols;
        let last = (self.entries.len() - 1) as isize;
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
}

fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();

    if let Some(parent) = dir.parent() {
        out.push(Entry {
            path: parent.to_path_buf(),
            label: "⬆ Parent".into(),
            kind: EntryKind::ParentDir,
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
            });
        } else if ft.is_file() {
            if is_archive_path(&p) {
                archives.push(Entry {
                    path: p,
                    label,
                    kind: EntryKind::Archive,
                });
            } else if is_image_path(&p) {
                images.push(Entry {
                    path: p,
                    label,
                    kind: EntryKind::Image,
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

/// Paint the explorer gallery. Returns the path the user clicked, if any.
/// Caller decides whether that path becomes the new `current` (navigation)
/// or the opened book.
pub fn draw(ui: &mut Ui, state: &mut ExplorerState) -> Option<Entry> {
    let mut clicked: Option<Entry> = None;

    ui.horizontal(|ui| {
        ui.heading("📂 Explorer");
        ui.separator();
        ui.label(
            egui::RichText::new(state.current.display().to_string())
                .color(Color32::LIGHT_GRAY),
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

    ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            if state.entries.is_empty() {
                ui.colored_label(Color32::GRAY, "(empty folder)");
                return;
            }
            let mut it = state.entries.iter().enumerate();
            loop {
                let row: Vec<(usize, &Entry)> = it.by_ref().take(columns).collect();
                if row.is_empty() {
                    break;
                }
                ui.horizontal(|ui| {
                    for (i, entry) in row {
                        let selected = i == selection;
                        let hit = draw_tile(
                            ui,
                            entry,
                            &state.cache,
                            tile_w,
                            tile_h,
                            thumb_h,
                            thumb_size,
                            selected,
                        );
                        if hit {
                            clicked = Some(entry.clone());
                        }
                    }
                });
            }
        });

    clicked
}

fn draw_tile(
    ui: &mut Ui,
    entry: &Entry,
    cache: &ThumbnailCache,
    tile_w: f32,
    tile_h: f32,
    thumb_h: f32,
    thumb_size: u32,
    selected: bool,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());

    // Selection always requests scroll-into-view so keyboard nav can move
    // off-screen selection back into sight. Must run before the clip-rect
    // early-return below.
    if selected {
        resp.scroll_to_me(Some(Align::Center));
    }

    // Skip all paint + thumbnail() requests for tiles outside the visible
    // viewport. The allocate_exact_size above still reserves space so the
    // scroll area sizes correctly — we just don't touch the cache. This is
    // the single biggest anti-flicker fix: the decoder queue no longer
    // drowns in requests for off-screen rows.
    if !ui.clip_rect().intersects(rect) {
        return false;
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
        painter.rect_stroke(rect, 6.0, egui::Stroke::new(2.0, Color32::LIGHT_BLUE));
    }

    let pad = 10.0;
    let thumb_rect = egui::Rect::from_min_size(
        rect.min + Vec2::new(pad, pad),
        Vec2::new(tile_w - pad * 2.0, thumb_h),
    );
    paint_thumb(ui, thumb_rect, entry, cache, thumb_size);

    let label_rect = egui::Rect::from_min_size(
        rect.min + Vec2::new(pad, pad + thumb_h + 6.0),
        Vec2::new(tile_w - pad * 2.0, tile_h - thumb_h - pad * 2.0 - 6.0),
    );
    let color = match entry.kind {
        EntryKind::ParentDir => Color32::LIGHT_BLUE,
        EntryKind::Folder => Color32::from_rgb(230, 200, 120),
        EntryKind::Archive => Color32::from_rgb(180, 220, 255),
        EntryKind::Image => Color32::WHITE,
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

    resp.clicked()
}

fn paint_thumb(
    ui: &mut Ui,
    rect: egui::Rect,
    entry: &Entry,
    cache: &ThumbnailCache,
    thumb_size: u32,
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

    match cache.thumbnail(&entry.path, thumb_size) {
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
                EntryKind::Image => "🖼",
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
