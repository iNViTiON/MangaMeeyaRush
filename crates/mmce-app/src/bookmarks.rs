//! Bookmark list dialog. Opens on Ctrl+B, listing bookmarks for the
//! current book. Clicking an entry returns the page index so the caller
//! can jump.

use egui::{Align2, Context, Key, ScrollArea};
use mmce_store::BookmarkRow;

#[derive(Debug, Default)]
pub struct BookmarksDialog {
    pub open: bool,
    pub rows: Vec<BookmarkRow>,
}

impl BookmarksDialog {
    pub fn open(&mut self, rows: Vec<BookmarkRow>) {
        self.rows = rows;
        self.open = true;
    }

    /// Show the modal. Returns `Some(page)` (0-based) if the user picks
    /// an entry.
    pub fn show(&mut self, ctx: &Context) -> Option<usize> {
        if !self.open {
            return None;
        }
        let mut result: Option<usize> = None;
        let mut close = false;
        egui::Window::new("Bookmarks")
            .collapsible(false)
            .resizable(true)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_size([420.0, 360.0])
            .show(ctx, |ui| {
                if self.rows.is_empty() {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        "No bookmarks yet — press Ctrl+D to add one.",
                    );
                } else {
                    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                        for row in &self.rows {
                            let label = row
                                .label
                                .clone()
                                .unwrap_or_else(|| format!("Page {}", row.page + 1));
                            if ui
                                .button(format!("{}   (page {})", label, row.page + 1))
                                .clicked()
                            {
                                result = Some(row.page as usize);
                                close = true;
                            }
                        }
                    });
                }
                ui.separator();
                if ui.button("Close").clicked()
                    || ctx.input(|i| i.key_pressed(Key::Escape))
                {
                    close = true;
                }
            });
        if close {
            self.open = false;
        }
        result
    }
}
