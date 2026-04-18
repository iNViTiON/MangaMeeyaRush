//! eframe-backed app layer for mmce.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Color32, Context, CursorIcon, Pos2, Vec2, ViewportCommand};
use mmce_codecs::{folder_has_direct_images, is_archive_path, PageSource};
use mmce_config::{FitMode, PageMode, Settings};
use mmce_core::{stride, Book, Spread, ViewerState};
use mmce_filters::{FilterOp, Pipeline, Rotation};
use mmce_render::{compute_size, PageCache};

mod anim;
mod bookmarks;
mod dialogs;
mod explorer;
mod file_ops;
mod history;
mod input;
mod overlay;
mod playback;
mod settings;
mod thumbs;

pub(crate) use settings::SettingsDialog;

use anim::{FlipDir, PageFlip, PagePaint};
use bookmarks::BookmarksDialog;
use dialogs::GotoDialog;
use file_ops::{ConfirmDelete, RenameDialog};
use history::HistoryDialog;
use playback::Playback;

use explorer::{Entry, EntryKind, ExplorerState};

#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    pub paths: Vec<PathBuf>,
    pub fullscreen: bool,
    pub last: bool,
    pub ini: Option<PathBuf>,
    pub view_mode: Option<u8>,
    pub add: bool,
}

pub struct App {
    settings: Settings,
    settings_path: PathBuf,
    book: Option<Book>,
    cache: Option<PageCache>,
    viewer: ViewerState,
    /// Current image-filter state (rotate/clip/adjust/sharpen/resize). When
    /// this changes we push it to the render cache which invalidates and
    /// re-decodes pages.
    pub(crate) filters: FilterState,
    status: String,
    last_error: Option<String>,
    pub(crate) current_path: Option<PathBuf>,
    pub(crate) view: View,
    pub(crate) explorer: Option<ExplorerState>,
    /// Toggleable overlays / HUD elements.
    pub(crate) overlays: Overlays,
    pub(crate) playback: Playback,
    pub(crate) goto: GotoDialog,
    pub(crate) bookmarks: BookmarksDialog,
    pub(crate) history: HistoryDialog,
    pub(crate) confirm_delete: ConfirmDelete,
    pub(crate) rename: RenameDialog,
    pub(crate) settings_dialog: SettingsDialog,
    /// Persistent store for history, bookmarks, and per-book state.
    /// `None` when the DB couldn't open (rare; we fall back to in-memory).
    pub(crate) store: Option<mmce_store::Store>,
    /// ID of the currently-open book in the store, if any. Used to attach
    /// bookmarks and record reading progress.
    pub(crate) current_book_id: Option<mmce_store::BookId>,
    /// Rects painted last frame — consulted on cursor change to snapshot
    /// the outgoing spread for the page-turn animation.
    last_book_paint: Vec<PagePaint>,
    last_book_cursor: Option<usize>,
    flip: Option<PageFlip>,
    pub(crate) animations_enabled: bool,
    /// Most recently observed pointer position, used to decide when the
    /// cursor is idle and the seek bar hover-zone check.
    last_pointer_pos: Option<Pos2>,
    last_pointer_move: Instant,
}

#[derive(Debug, Clone)]
pub struct Overlays {
    pub info: bool,
    pub loupe: bool,
    pub seekbar: bool,
    pub loupe_radius: f32,
    pub loupe_magnification: f32,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            info: false,
            loupe: false,
            seekbar: true,
            loupe_radius: 120.0,
            loupe_magnification: 3.0,
        }
    }
}

/// High-level knobs the user actually touches. Serialized separately from
/// the raw `Pipeline` so we can keep a stable UI order even as filters
/// compose different ways under the hood.
#[derive(Debug, Clone)]
pub struct FilterState {
    pub rotation: Rotation,
}

impl FilterState {
    pub fn identity() -> Self {
        Self { rotation: Rotation::Deg0 }
    }

    /// Build the actual pipeline that runs on each decoded page.
    pub fn pipeline(&self) -> Pipeline {
        let mut p = Pipeline::new();
        if self.rotation != Rotation::Deg0 {
            p.push(FilterOp::Rotate(self.rotation));
        }
        p
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Book,
    Explorer,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cli: CliArgs) -> Self {
        let settings_path = cli
            .ini
            .clone()
            .unwrap_or_else(default_settings_path);

        let mut settings = Settings::load(&settings_path).unwrap_or_default();
        if cli.fullscreen {
            settings.general.fullscreen = true;
        }

        let mut viewer = ViewerState::from_settings(&settings);
        if cli.fullscreen {
            viewer.fullscreen = true;
        }

        let start_path = cli.paths.first().cloned().or_else(|| {
            if cli.last {
                settings.general.current_folder.clone()
            } else {
                None
            }
        });

        let view = match (cli.view_mode, start_path.is_some()) {
            (Some(2), _) => View::Explorer,
            (_, false) => View::Explorer,
            _ => View::Book,
        };

        let store = open_store_or_warn();

        let mut app = Self {
            settings,
            settings_path,
            book: None,
            cache: None,
            viewer,
            filters: FilterState::identity(),
            status: String::new(),
            last_error: None,
            current_path: None,
            view,
            explorer: None,
            overlays: Overlays::default(),
            playback: Playback::default(),
            goto: GotoDialog::default(),
            bookmarks: BookmarksDialog::default(),
            history: HistoryDialog::default(),
            confirm_delete: ConfirmDelete::default(),
            rename: RenameDialog::default(),
            settings_dialog: SettingsDialog::default(),
            store,
            current_book_id: None,
            last_book_paint: Vec::new(),
            last_book_cursor: None,
            flip: None,
            animations_enabled: true,
            last_pointer_pos: None,
            last_pointer_move: Instant::now(),
        };

        if let Some(p) = start_path {
            app.open_path(&cc.egui_ctx, &p);
        }

        if app.view == View::Explorer && app.explorer.is_none() {
            let dir = app.explorer_start_dir();
            app.explorer = Some(ExplorerState::new(&cc.egui_ctx, dir));
        }

        if app.viewer.fullscreen {
            cc.egui_ctx
                .send_viewport_cmd(ViewportCommand::Fullscreen(true));
        }
        app
    }

    fn explorer_start_dir(&self) -> PathBuf {
        if let Some(p) = &self.current_path {
            if p.is_dir() {
                return p.clone();
            }
            if let Some(parent) = p.parent() {
                return parent.to_path_buf();
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    pub fn open_path(&mut self, ctx: &Context, path: &Path) {
        if path.is_dir() && !folder_has_direct_images(path) {
            self.current_path = Some(path.to_path_buf());
            match self.explorer.as_mut() {
                Some(e) => e.cd(path.to_path_buf()),
                None => self.explorer = Some(ExplorerState::new(ctx, path.to_path_buf())),
            }
            self.view = View::Explorer;
            self.last_error = None;
            self.status = format!("Browsing {}", path.display());
            return;
        }
        match Book::open(path) {
            Ok(book) => {
                let pages = book.len();
                let source: Arc<dyn PageSource> = book.source().clone();
                let cache = PageCache::new(
                    ctx.clone(),
                    source,
                    self.settings.cache.picture_cache_size as usize,
                );
                cache.set_pipeline(self.filters.pipeline());
                self.status = format!("Loaded {} ({} pages)", book.title(), pages);
                self.last_error = if pages == 0 {
                    Some(format!(
                        "No images found in {} — try a folder of PNG/JPEG pages or an archive.",
                        path.display()
                    ))
                } else {
                    None
                };
                self.settings.general.current_folder = Some(path.to_path_buf());
                self.current_path = Some(path.to_path_buf());
                self.book = Some(book);
                self.cache = Some(cache);
                if pages > 0 {
                    self.view = View::Book;
                }
                self.attach_store_book(path, pages);
            }
            Err(e) => {
                self.last_error = Some(format!("open {}: {}", path.display(), e));
                self.status = self.last_error.clone().unwrap_or_default();
            }
        }
    }

    fn save_settings(&mut self) {
        self.settings.general.fullscreen = self.viewer.fullscreen;
        self.settings.scale.mode = self.viewer.fit;
        self.settings.scale.optional_scale = self.viewer.zoom;
        self.settings.view.page_mode = self.viewer.page_mode;
        self.settings.view.bind_dir = self.viewer.bind_dir;
        if let Err(e) = self.settings.save(&self.settings_path) {
            log::warn!("save settings: {e}");
        }
    }

    /// Backspace in Book view: return to the explorer rooted at the book's
    /// parent directory, with the book's own entry pre-selected and
    /// scrolled into view. Fullscreen state is preserved — only the user's
    /// explicit F11 / Esc / Alt+Enter toggles flip it.
    pub(crate) fn back_to_explorer(&mut self, ctx: &Context) {
        let cur = match &self.current_path {
            Some(p) => p.clone(),
            None => return,
        };
        let parent = match cur.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        };

        // Build (or reuse) the explorer at `parent`.
        match self.explorer.as_mut() {
            Some(state) if state.current == parent => { /* already there */ }
            Some(state) => state.cd(parent.clone()),
            None => self.explorer = Some(ExplorerState::new(ctx, parent.clone())),
        }

        // Select the entry that matches the book we just left. For folder
        // books whose path is the folder itself this is a direct hit; for a
        // loose image we fall back to selecting the image's own tile.
        if let Some(state) = self.explorer.as_mut() {
            if let Some(idx) = state.entries().iter().position(|e| e.path == cur) {
                state.selection = idx;
            }
        }

        self.view = View::Explorer;
    }

    pub(crate) fn toggle_explorer(&mut self, ctx: &Context) {
        self.view = match self.view {
            View::Book => {
                // Always point the explorer at the current book's
                // directory when opening from Book view — otherwise a
                // stale ExplorerState (from, say, the initial launch or
                // previous browsing) would strand the user one level up,
                // making it impossible to descend into the book's own
                // subfolders. Also try to pre-select the current book so
                // it's obvious where they came from.
                let dir = self.explorer_start_dir();
                match self.explorer.as_mut() {
                    Some(state) if state.current == dir => { /* already there */ }
                    Some(state) => state.cd(dir),
                    None => self.explorer = Some(ExplorerState::new(ctx, dir)),
                }
                if let (Some(state), Some(cur)) =
                    (self.explorer.as_mut(), self.current_path.as_ref())
                {
                    if let Some(idx) = state.entries().iter().position(|e| &e.path == cur) {
                        state.selection = idx;
                    }
                }
                View::Explorer
            }
            View::Explorer => {
                if self.book.is_some() {
                    View::Book
                } else {
                    View::Explorer
                }
            }
        };
    }

    pub(crate) fn open_dialog(&mut self, ctx: &Context) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter(
                "Images & archives",
                &[
                    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff",
                    "zip", "cbz", "7z", "cb7",
                ],
            )
            .pick_file()
        {
            self.open_path(ctx, &p);
        }
    }

    pub(crate) fn open_folder_dialog(&mut self, ctx: &Context) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.open_path(ctx, &dir);
        }
    }

    fn handle_explorer_click(&mut self, ctx: &Context, entry: Entry) {
        match entry.kind {
            EntryKind::ParentDir => {
                if let Some(state) = self.explorer.as_mut() {
                    state.cd(entry.path);
                }
            }
            EntryKind::Folder => {
                if folder_has_direct_images(&entry.path) {
                    self.open_path(ctx, &entry.path);
                } else if let Some(state) = self.explorer.as_mut() {
                    state.cd(entry.path);
                }
            }
            EntryKind::Archive | EntryKind::Image => {
                self.open_path(ctx, &entry.path);
            }
        }
    }

    pub(crate) fn explorer_activate(&mut self, ctx: &Context) {
        let entry = self.explorer.as_ref().and_then(|e| e.selected().cloned());
        if let Some(e) = entry {
            self.handle_explorer_click(ctx, e);
        }
    }

    pub(crate) fn explorer_up(&mut self) {
        if let Some(state) = self.explorer.as_mut() {
            state.go_parent();
        }
    }

    pub(crate) fn explorer_move(&mut self, dx: isize, dy: isize) {
        if let Some(state) = self.explorer.as_mut() {
            state.move_selection(dx, dy);
        }
    }

    pub(crate) fn explorer_thumb_bigger(&mut self) {
        if let Some(state) = self.explorer.as_mut() {
            state.thumb_bigger();
        }
    }

    pub(crate) fn explorer_thumb_smaller(&mut self) {
        if let Some(state) = self.explorer.as_mut() {
            state.thumb_smaller();
        }
    }

    /// Rotate the viewer clockwise or counter-clockwise by 90°. Pushes the
    /// updated pipeline to the page cache which triggers a re-decode.
    pub(crate) fn rotate(&mut self, cw: bool) {
        self.filters.rotation = if cw {
            self.filters.rotation.next_cw()
        } else {
            self.filters.rotation.next_ccw()
        };
        if let Some(cache) = self.cache.as_ref() {
            cache.set_pipeline(self.filters.pipeline());
        }
    }

    /// Drop all active filters back to identity.
    pub(crate) fn reset_filters(&mut self) {
        self.filters = FilterState::identity();
        if let Some(cache) = self.cache.as_ref() {
            cache.set_pipeline(self.filters.pipeline());
        }
    }

    pub(crate) fn toggle_info_overlay(&mut self) {
        self.overlays.info = !self.overlays.info;
    }

    pub(crate) fn toggle_loupe(&mut self) {
        self.overlays.loupe = !self.overlays.loupe;
    }

    pub(crate) fn toggle_seekbar(&mut self) {
        self.overlays.seekbar = !self.overlays.seekbar;
    }

    pub(crate) fn goto_page(&mut self, idx: usize) {
        if let Some(b) = self.book.as_mut() {
            b.goto(idx);
        }
    }

    pub(crate) fn open_goto_dialog(&mut self) {
        let cur = self.book.as_ref().map(|b| b.cursor() + 1).unwrap_or(1);
        self.goto.open(cur);
    }

    pub(crate) fn toggle_playback(&mut self) {
        self.playback.toggle();
    }

    pub(crate) fn start_playback(&mut self, forward: bool) {
        self.playback.start(forward);
    }

    pub(crate) fn stop_playback(&mut self) {
        self.playback.pause();
    }

    // ---------- store integration --------------------------------------

    fn attach_store_book(&mut self, path: &Path, _page_count: usize) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path_str = canon.to_string_lossy().to_string();
        let kind = classify_path(&canon);
        match store.upsert_book(&path_str, kind) {
            Ok(id) => {
                // We deliberately DO NOT auto-jump to last_page on open —
                // the user asked for explicit nav only. Bookmarks + the
                // history dialog still exist for getting back to a spot.
                self.current_book_id = Some(id);
            }
            Err(e) => {
                log::warn!("store upsert_book: {e}");
            }
        }
    }

    /// Record that the book was opened, and stamp the page count — but
    /// deliberately leave `last_page` at 0 so we don't persist a reading
    /// position between sessions. The user asked for explicit nav only.
    fn touch_store_position(&self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let Some(id) = self.current_book_id else {
            return;
        };
        let Some(book) = self.book.as_ref() else {
            return;
        };
        if let Err(e) = store.touch_book(id, 0, book.len()) {
            log::warn!("store touch_book: {e}");
        }
    }

    pub(crate) fn toggle_bookmark(&mut self) {
        let (Some(store), Some(id), Some(book)) =
            (self.store.as_ref(), self.current_book_id, self.book.as_ref())
        else {
            return;
        };
        let page = book.cursor();
        match store.list_bookmarks(id) {
            Ok(rows) => {
                if rows.iter().any(|r| r.page == page as i64) {
                    let _ = store.remove_bookmark(id, page);
                    self.status = format!("Removed bookmark at page {}", page + 1);
                } else {
                    let label = book
                        .source()
                        .entry_name(page)
                        .map(|s| s.to_string());
                    let _ = store.add_bookmark(id, page, label.as_deref());
                    self.status = format!("Bookmarked page {}", page + 1);
                }
            }
            Err(e) => log::warn!("list_bookmarks: {e}"),
        }
    }

    pub(crate) fn open_bookmarks_dialog(&mut self) {
        if let (Some(store), Some(id)) = (self.store.as_ref(), self.current_book_id) {
            if let Ok(rows) = store.list_bookmarks(id) {
                self.bookmarks.open(rows);
            }
        }
    }

    pub(crate) fn open_history_dialog(&mut self) {
        if let Some(store) = self.store.as_ref() {
            if let Ok(rows) = store.recent_books(50) {
                self.history.open(rows);
            }
        }
    }

    pub(crate) fn refresh_explorer(&mut self) {
        if let Some(state) = self.explorer.as_mut() {
            state.refresh();
        }
    }

    pub(crate) fn toggle_explorer_filter(&mut self) {
        if let Some(state) = self.explorer.as_mut() {
            state.show_filter = !state.show_filter;
            if !state.show_filter {
                state.filter.clear();
            }
        }
    }

    pub(crate) fn close_explorer_filter(&mut self) {
        if let Some(state) = self.explorer.as_mut() {
            state.filter.clear();
            state.show_filter = false;
        }
    }

    /// Open the rename dialog for the currently-selected entry in the
    /// explorer. No-op in Book view.
    pub(crate) fn open_rename_dialog(&mut self) {
        let Some(state) = self.explorer.as_ref() else {
            return;
        };
        let Some(path) = state.selected_path() else {
            return;
        };
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            return;
        }
        self.rename.open_for(&name);
    }

    /// Open the delete-confirmation dialog for the selected entry.
    pub(crate) fn open_delete_dialog(&mut self) {
        let Some(state) = self.explorer.as_ref() else {
            return;
        };
        let Some(path) = state.selected_path() else {
            return;
        };
        self.confirm_delete.open_for(path);
    }

    pub(crate) fn open_settings_dialog(&mut self) {
        self.settings_dialog.open_dialog();
    }

    /// The directory whose *siblings* Shift+Up/Down should walk. In Book
    /// view that's the book's containing folder (or the archive file
    /// itself, or the dir holding a loose image). In Explorer view it's
    /// whatever the user has browsed into, not the first path they opened.
    fn current_nav_dir(&self) -> Option<PathBuf> {
        match self.view {
            View::Explorer => self.explorer.as_ref().map(|e| e.current.clone()),
            View::Book => {
                let p = self.current_path.as_ref()?;
                if p.is_dir() || is_archive_path(p) {
                    Some(p.clone())
                } else {
                    p.parent().map(Path::to_path_buf)
                }
            }
        }
    }

    /// Navigate to the previous / next sibling folder or archive at the
    /// current navigation level (Shift+Up / Shift+Down).
    pub(crate) fn jump_sibling(&mut self, ctx: &Context, delta: isize) {
        let cur = match self.current_nav_dir() {
            Some(p) => p,
            None => return,
        };
        let parent = match cur.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        };
        // Same ordering the explorer grid uses: folders first (natural
        // sort), then archives (natural sort). Everything else is ignored
        // — Shift+↑/↓ should only hop between "book-shaped" neighbours.
        let (mut folders, mut archives): (Vec<PathBuf>, Vec<PathBuf>) = match fs::read_dir(&parent) {
            Ok(rd) => rd.flatten().filter_map(|e| Some(e.path())).fold(
                (Vec::new(), Vec::new()),
                |(mut f, mut a), p| {
                    if p.is_dir() {
                        f.push(p);
                    } else if is_archive_path(&p) {
                        a.push(p);
                    }
                    (f, a)
                },
            ),
            Err(_) => return,
        };
        let name_key = |p: &Path| {
            p.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        folders.sort_by(|a, b| natord::compare(&name_key(a), &name_key(b)));
        archives.sort_by(|a, b| natord::compare(&name_key(a), &name_key(b)));
        let mut siblings: Vec<PathBuf> = folders;
        siblings.extend(archives);
        if siblings.is_empty() {
            return;
        }
        let idx = siblings.iter().position(|p| p == &cur).unwrap_or(0) as isize;
        let last = (siblings.len() - 1) as isize;
        let new = (idx + delta).clamp(0, last) as usize;
        let next = siblings[new].clone();
        if next != cur {
            self.open_path(ctx, &next);
        }
    }

    pub(crate) fn advance_pages(&mut self, delta: isize) {
        if let Some(b) = self.book.as_mut() {
            b.advance(delta);
        }
    }

    /// Resolve Auto → Single/Spread using whatever page dimensions are
    /// currently cached. Unknown pages default to portrait (spread).
    fn effective_mode(&self) -> PageMode {
        match (self.viewer.page_mode, self.book.as_ref(), self.cache.as_ref()) {
            (PageMode::Auto, Some(book), Some(cache)) => {
                let a = book.cursor();
                let a_land = cache
                    .page_dimensions(a)
                    .map(|(w, h)| w > h)
                    .unwrap_or(false);
                let b_land = if a + 1 < book.len() {
                    cache
                        .page_dimensions(a + 1)
                        .map(|(w, h)| w > h)
                        .unwrap_or(false)
                } else {
                    true
                };
                if a_land || b_land {
                    PageMode::Single
                } else {
                    PageMode::Spread
                }
            }
            (mode, _, _) => mode,
        }
    }

    /// Stride in pages for the current navigation mode.
    pub(crate) fn current_stride(&self) -> usize {
        stride(self.effective_mode())
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        input::handle(self, ctx);

        // Slideshow tick. Runs before any painting so a delta applies to
        // this frame's cursor.
        if let Some(book) = self.book.as_ref() {
            if let Some(delta) = self.playback.tick(book.cursor(), book.len()) {
                if let Some(b) = self.book.as_mut() {
                    b.advance(delta);
                }
            }
            if let Some(after) = self.playback.repaint_after() {
                ctx.request_repaint_after(after);
            }
        }

        // Goto-page dialog.
        if let Some(total) = self.book.as_ref().map(|b| b.len()) {
            if let Some(idx) = self.goto.show(ctx, total) {
                self.goto_page(idx);
            }
        }

        // Bookmarks + history dialogs.
        if let Some(page) = self.bookmarks.show(ctx) {
            self.goto_page(page);
        }
        if let Some(path) = self.history.show(ctx) {
            self.open_path(ctx, &path);
        }

        // File-op dialogs (explorer).
        if let Some(_path) = self.confirm_delete.show(ctx) {
            if let Some(state) = self.explorer.as_mut() {
                match state.delete_selected() {
                    Ok(_) => self.status = "Deleted.".into(),
                    Err(e) => self.status = format!("Delete failed: {e}"),
                }
            }
        }
        if let Some(new_name) = self.rename.show(ctx) {
            if let Some(state) = self.explorer.as_mut() {
                match state.rename_selected(&new_name) {
                    Ok(_) => self.status = format!("Renamed to {new_name}"),
                    Err(e) => self.status = format!("Rename failed: {e}"),
                }
            }
        }

        settings::show(ctx, self);

        let bg_book = egui::Color32::from_rgb(
            (self.viewer.bg_color & 0xFF) as u8,
            ((self.viewer.bg_color >> 8) & 0xFF) as u8,
            ((self.viewer.bg_color >> 16) & 0xFF) as u8,
        );

        let mut clicked: Option<Entry> = None;
        let mut seek_requested: Option<usize> = None;
        let effective = self.effective_mode();

        // Pointer activity tracking — drives idle cursor hiding and the
        // hover-activated seek bar below.
        let now = Instant::now();
        if let Some(pos) = ctx.pointer_latest_pos() {
            if self.last_pointer_pos != Some(pos) {
                self.last_pointer_pos = Some(pos);
                self.last_pointer_move = now;
            }
        }

        // Detect cursor changes + direction so we can kick off a page
        // flip. Starting the flip is deferred until after draw_spread so
        // we have the NEW rects to anchor the landing geometry against.
        let cursor_change = match (self.book.as_ref(), self.last_book_cursor) {
            (Some(b), Some(prev)) if b.cursor() != prev => {
                Some(if b.cursor() > prev {
                    FlipDir::Forward
                } else {
                    FlipDir::Backward
                })
            }
            _ => None,
        };

        let mut book_page_rects: Vec<(usize, egui::TextureHandle, egui::Rect)> = Vec::new();

        CentralPanel::default()
            .frame(egui::Frame::none().fill(bg_book))
            .show(ctx, |ui| match self.view {
                View::Book => match (self.book.as_ref(), self.cache.as_ref()) {
                    (Some(book), Some(cache)) if book.len() > 0 => {
                        let spread = book.current_spread(effective, self.viewer.bind_dir);
                        book_page_rects = draw_spread(
                            ui,
                            spread,
                            cache,
                            &self.viewer,
                            self.settings.scale.no_zoom_in,
                        );
                        // Start a new page-flip if the cursor changed
                        // this frame. We have both the prev paint (from
                        // last frame) and the new paint (just captured).
                        if let Some(dir) = cursor_change {
                            if self.animations_enabled
                                && !self.last_book_paint.is_empty()
                                && !book_page_rects.is_empty()
                                && self.last_book_paint.len() == book_page_rects.len()
                            {
                                let next_raw: Vec<PagePaint> = book_page_rects
                                    .iter()
                                    .map(|(_, tex, rect)| {
                                        PagePaint::full(tex.clone(), *rect)
                                    })
                                    .collect();
                                // The flip's hinge should always live at
                                // the viewport centre, regardless of pan
                                // or uneven page widths.
                                let spine_x = ui.min_rect().center().x;
                                let (prev_pair, next_pair) =
                                    normalise_pair(&self.last_book_paint, &next_raw, spine_x);
                                self.flip = Some(PageFlip::new(
                                    prev_pair,
                                    next_pair,
                                    self.viewer.bind_dir,
                                    dir,
                                ));
                            } else {
                                self.flip = None;
                            }
                        }

                        // Paint the flipping leaf over the freshly-drawn
                        // new spread.
                        if let Some(flip) = self.flip.as_ref() {
                            anim::paint_flip(ui, flip);
                        }
                        if self.overlays.info {
                            overlay::paint_info(ui, book, cache, spread);
                        }
                        if self.overlays.loupe {
                            if let Some(cursor) = ui.ctx().pointer_latest_pos() {
                                overlay::paint_loupe(
                                    ui,
                                    cursor,
                                    &book_page_rects,
                                    self.overlays.loupe_magnification,
                                    self.overlays.loupe_radius,
                                );
                            }
                        }

                        // Hover-activated seek bar. Lives entirely inside
                        // the central panel so the image never shrinks
                        // to accommodate it.
                        if self.overlays.seekbar && book.len() > 1 {
                            let ptr_in_zone = ui
                                .ctx()
                                .pointer_latest_pos()
                                .map(|p| {
                                    p.y >= ui.max_rect().max.y - SEEK_BAR_ACTIVATION_ZONE
                                        && ui.max_rect().x_range().contains(p.x)
                                })
                                .unwrap_or(false);
                            if ptr_in_zone {
                                if let Some(idx) = paint_seekbar_overlay(ui, book) {
                                    seek_requested = Some(idx);
                                }
                            }
                        }

                        let window: Vec<usize> = spread.indices().collect();
                        if self.settings.cache.preload {
                            cache.prefetch(&window, 2);
                        }
                    }
                    _ => draw_welcome(ui, self.last_error.as_deref()),
                },
                View::Explorer => {
                    if let Some(state) = self.explorer.as_mut() {
                        clicked = explorer::draw(ui, state);
                    }
                }
            });

        if let Some(entry) = clicked {
            self.handle_explorer_click(ctx, entry);
        }
        if let Some(idx) = seek_requested {
            self.goto_page(idx);
        }

        // Idle cursor hiding — keeps the book view cinematic. Active
        // only when the pointer hasn't moved for `POINTER_IDLE_TIMEOUT`
        // and the user isn't hovering over the seek-bar zone (so a
        // parked mouse doesn't lose its pointer just before clicking
        // the bar).
        if matches!(self.view, View::Book) {
            let idle = now.duration_since(self.last_pointer_move);
            let in_bar_zone = self
                .last_pointer_pos
                .map(|p| {
                    p.y >= ctx.screen_rect().max.y - SEEK_BAR_ACTIVATION_ZONE
                })
                .unwrap_or(false);
            if idle >= POINTER_IDLE_TIMEOUT && !in_bar_zone {
                ctx.set_cursor_icon(CursorIcon::None);
            } else {
                // Schedule a repaint when we should re-evaluate the
                // idle state, so the cursor actually disappears without
                // needing any input.
                if idle < POINTER_IDLE_TIMEOUT {
                    let remaining = POINTER_IDLE_TIMEOUT - idle + Duration::from_millis(50);
                    ctx.request_repaint_after(remaining);
                }
            }
        }

        // Remember the rects we just painted so the next cursor change
        // can snapshot them for the outgoing flip. Drive repaints while
        // the flip is active, and drop it when it completes.
        self.last_book_paint = book_page_rects
            .iter()
            .map(|(_, tex, rect)| PagePaint::full(tex.clone(), *rect))
            .collect();
        if let Some(b) = self.book.as_ref() {
            self.last_book_cursor = Some(b.cursor());
        }
        let drop_flip = self.flip.as_ref().map(|f| f.is_done()).unwrap_or(false);
        if drop_flip {
            self.flip = None;
        } else if self.flip.is_some() {
            ctx.request_repaint();
        }

        // Status bar.
        egui::TopBottomPanel::bottom("status")
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_black_alpha(160))
                    .inner_margin(egui::Margin::symmetric(8.0, 2.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    match self.view {
                        View::Book => {
                            if let Some(book) = &self.book {
                                let mode_str = match self.viewer.page_mode {
                                    PageMode::Single => "single",
                                    PageMode::Spread => "spread",
                                    PageMode::Auto => match effective {
                                        PageMode::Single => "auto→1",
                                        _ => "auto→2",
                                    },
                                };
                                let rot_str = match self.filters.rotation {
                                    Rotation::Deg0 => "",
                                    Rotation::Deg90 => " rot=90°",
                                    Rotation::Deg180 => " rot=180°",
                                    Rotation::Deg270 => " rot=270°",
                                };
                                ui.colored_label(
                                    Color32::LIGHT_GRAY,
                                    format!(
                                        "{} — {}/{}   [{}]   fit={:?} zoom={:.2}{}",
                                        book.title(),
                                        book.cursor() + 1,
                                        book.len().max(1),
                                        mode_str,
                                        self.viewer.fit,
                                        self.viewer.zoom,
                                        rot_str,
                                    ),
                                );
                            } else {
                                ui.colored_label(Color32::LIGHT_GRAY, &self.status);
                            }
                        }
                        View::Explorer => {
                            ui.colored_label(Color32::LIGHT_GRAY, "Explorer");
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.colored_label(
                            Color32::DARK_GRAY,
                            match self.view {
                                View::Book => {
                                    "E explorer · PgUp/Dn ±10 · Shift+↑/↓ sibling · Space mode"
                                }
                                View::Explorer => {
                                    "↑↓←→ select · Enter open · ⌫ up · Ctrl± resize"
                                }
                            },
                        );
                    });
                });
            });

        // File drops.
        let dropped: Vec<_> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(first) = dropped.first() {
            self.open_path(ctx, first);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
        self.touch_store_position();
    }
}

fn draw_welcome(ui: &mut egui::Ui, err: Option<&str>) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("mmce")
                    .color(Color32::LIGHT_GRAY)
                    .size(48.0),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "Press E for the explorer gallery, O to pick a file, \
                     Shift+O for a folder, or drop a path here.",
                )
                .color(Color32::GRAY),
            );
            if let Some(msg) = err {
                ui.add_space(12.0);
                ui.colored_label(Color32::LIGHT_RED, msg);
            }
        });
    });
}

/// Paint the current spread and return the `(idx, tex, rect)` triples
/// for the painted pages (used by overlays, the loupe, and the page-flip
/// animation). Spread layout anchors each page's spine-adjacent edge to
/// the viewport centre — so the fold line is always exactly in the
/// middle, even when the two pages have different aspect ratios. Single
/// pages are centred in the full viewport; the halves they get split
/// into for the flip animation meet at that same middle line.
fn draw_spread(
    ui: &mut egui::Ui,
    spread: Spread,
    cache: &PageCache,
    viewer: &ViewerState,
    no_zoom_in_cfg: bool,
) -> Vec<(usize, egui::TextureHandle, egui::Rect)> {
    let viewport = ui.available_size();
    let origin = ui.min_rect().left_top();

    let pages: Vec<(usize, egui::TextureHandle)> = spread
        .indices()
        .filter_map(|idx| cache.texture(idx).map(|t| (idx, t)))
        .collect();

    if pages.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.spinner();
        });
        return Vec::new();
    }

    let pan = Vec2::new(viewer.pan[0], viewer.pan[1]);
    let no_zoom_in = no_zoom_in_cfg && viewer.fit != FitMode::Custom;

    let mut rects = Vec::with_capacity(pages.len());
    match pages.len() {
        1 => {
            let (idx, tex) = pages.into_iter().next().unwrap();
            let intrinsic = tex.size_vec2();
            let rendered =
                compute_size(intrinsic, viewport, viewer.fit, viewer.zoom, no_zoom_in);
            if rendered.x <= 0.0 || rendered.y <= 0.0 {
                return rects;
            }
            let offset = (viewport - rendered) * 0.5 + pan;
            let rect = egui::Rect::from_min_size(origin + offset, rendered);
            egui::Image::from_texture(&tex)
                .fit_to_exact_size(rendered)
                .paint_at(ui, rect);
            rects.push((idx, tex, rect));
        }
        2 => {
            // Each page is fit independently into half the viewport,
            // then anchored so its spine-adjacent edge sits on the
            // viewport's vertical centre line. The other edge and the
            // height are free to vary between pages.
            let spine_x = origin.x + viewport.x * 0.5;
            let half = Vec2::new(viewport.x * 0.5, viewport.y);
            for (i, (idx, tex)) in pages.into_iter().enumerate() {
                let intrinsic = tex.size_vec2();
                let rendered =
                    compute_size(intrinsic, half, viewer.fit, viewer.zoom, no_zoom_in);
                if rendered.x <= 0.0 || rendered.y <= 0.0 {
                    continue;
                }
                let y = origin.y + (viewport.y - rendered.y) * 0.5 + pan.y;
                // i == 0 is the physical left page; anchor its right
                // edge at the spine. i == 1 is the right page; anchor
                // its left edge at the spine.
                let x = if i == 0 {
                    spine_x - rendered.x + pan.x
                } else {
                    spine_x + pan.x
                };
                let rect = egui::Rect::from_min_size(egui::pos2(x, y), rendered);
                egui::Image::from_texture(&tex)
                    .fit_to_exact_size(rendered)
                    .paint_at(ui, rect);
                rects.push((idx, tex, rect));
            }
        }
        _ => {}
    }
    rects
}

/// Build the `[left, right]` pair the flip animator wants. For spread
/// mode it's a pass-through (the draw code already anchors both pages
/// to `spine_x`). For single-page mode we split each page at `spine_x`
/// so the flip folds at the viewport centre — which is the fixed point
/// the user asked for, independent of the page's intrinsic width.
fn normalise_pair(
    prev: &[PagePaint],
    next: &[PagePaint],
    spine_x: f32,
) -> (Vec<PagePaint>, Vec<PagePaint>) {
    match (prev.len(), next.len()) {
        (1, 1) => {
            let (pl, pr) = prev[0].clone().split_at(spine_x);
            let (nl, nr) = next[0].clone().split_at(spine_x);
            (vec![pl, pr], vec![nl, nr])
        }
        _ => (prev.to_vec(), next.to_vec()),
    }
}

/// Height in logical px of the bottom strip that activates the seek bar
/// on hover. Roomy enough to reach with a quick flick from anywhere on
/// screen without requiring pixel-perfect aim.
const SEEK_BAR_ACTIVATION_ZONE: f32 = 110.0;

/// How long the pointer must stay still before we hide it in Book view.
const POINTER_IDLE_TIMEOUT: Duration = Duration::from_millis(2000);

/// Paint a semi-transparent seek bar over the bottom of the central
/// panel and return the target page index if the user clicked/dragged
/// to a new spot.
fn paint_seekbar_overlay(
    ui: &mut egui::Ui,
    book: &Book,
) -> Option<usize> {
    let viewport = ui.max_rect();
    let bar_h = 28.0;
    let side_pad = 16.0;
    let bottom_pad = 6.0;
    let bar_rect = egui::Rect::from_min_size(
        egui::Pos2::new(
            viewport.min.x + side_pad,
            viewport.max.y - bar_h - bottom_pad,
        ),
        Vec2::new(viewport.width() - 2.0 * side_pad, bar_h),
    );
    ui.painter()
        .rect_filled(bar_rect, 6.0, Color32::from_black_alpha(170));
    let mut result: Option<usize> = None;
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(bar_rect.shrink2(Vec2::new(8.0, 3.0))),
        |ui| {
            result = overlay::seek_bar(ui, book);
        },
    );
    result
}

fn default_settings_path() -> PathBuf {
    if let Some(dirs) = dirs_config() {
        dirs.join("mmce").join("mmce.ini")
    } else {
        PathBuf::from("mmce.ini")
    }
}

fn default_store_path() -> PathBuf {
    if let Some(dirs) = dirs_config() {
        dirs.join("mmce").join("state.db")
    } else {
        PathBuf::from("state.db")
    }
}

fn open_store_or_warn() -> Option<mmce_store::Store> {
    let path = default_store_path();
    match mmce_store::Store::open(&path) {
        Ok(s) => {
            // One-shot import of a legacy MangaMeeyaCE.ini sitting next to
            // our config, so existing users land on a familiar setup.
            import_legacy_ini_once(&s);
            Some(s)
        }
        Err(e) => {
            log::warn!("state.db unavailable ({e}); falling back to in-memory");
            mmce_store::Store::open_memory().ok()
        }
    }
}

/// If we haven't yet imported the legacy INI into SQLite, look for a
/// `MangaMeeyaCE.ini` alongside our `mmce.ini`, parse it, and copy a
/// small whitelist of keys into the `setting` table. Marks the import as
/// done so we don't touch the user's legacy file again.
fn import_legacy_ini_once(store: &mmce_store::Store) {
    match store.get_setting("legacy_ini_imported") {
        Ok(Some(_)) => return,
        Err(e) => {
            log::warn!("legacy-ini import: settings read failed: {e}");
            return;
        }
        Ok(None) => {}
    }

    // Try both the canonical XDG location and the fallback cwd.
    let candidates = [
        default_settings_path()
            .parent()
            .map(|p| p.join("MangaMeeyaCE.ini")),
        Some(PathBuf::from("MangaMeeyaCE.ini")),
    ];

    let mut imported = 0usize;
    for candidate in candidates.into_iter().flatten() {
        if !candidate.exists() {
            continue;
        }
        if let Ok(settings) = mmce_config::Settings::load(&candidate) {
            // Mirror the keys we can express as plain setting rows. The
            // structured sections are already loaded through the normal
            // Settings path; this is purely so a fresh user's
            // preferences survive if they blow away mmce.ini later.
            let rot = match settings.view.page_mode {
                mmce_config::PageMode::Single => "single",
                mmce_config::PageMode::Spread => "spread",
                mmce_config::PageMode::Auto => "auto",
            };
            let _ = store.set_setting("legacy.page_mode", rot);
            let _ = store.set_setting(
                "legacy.bind_dir",
                match settings.view.bind_dir {
                    mmce_config::BindDir::LeftToRight => "ltr",
                    mmce_config::BindDir::RightToLeft => "rtl",
                },
            );
            let _ = store.set_setting(
                "legacy.bg_color",
                &settings.general.bg_color.to_string(),
            );
            imported += 1;
            break;
        }
    }

    let _ = store.set_setting(
        "legacy_ini_imported",
        if imported > 0 { "1" } else { "0" },
    );
    if imported > 0 {
        log::info!("imported legacy MangaMeeyaCE.ini into state.db");
    }
}

fn classify_path(path: &Path) -> &'static str {
    if path.is_dir() {
        "folder"
    } else {
        match path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("zip" | "cbz") => "zip",
            Some("7z" | "cb7") => "sevenz",
            _ => "image",
        }
    }
}

fn dirs_config() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}
