//! Recent-books (history) dialog. Opens on Ctrl+H. Returns the path of
//! the clicked entry so the caller can reopen it.

use std::path::PathBuf;

use egui::{Align2, Context, Key, ScrollArea};
use mmce_store::BookRow;

#[derive(Debug, Default)]
pub struct HistoryDialog {
    pub open: bool,
    pub rows: Vec<BookRow>,
}

impl HistoryDialog {
    pub fn open(&mut self, rows: Vec<BookRow>) {
        self.rows = rows;
        self.open = true;
    }

    pub fn show(&mut self, ctx: &Context) -> Option<PathBuf> {
        if !self.open {
            return None;
        }
        let mut result: Option<PathBuf> = None;
        let mut close = false;
        egui::Window::new("Recent")
            .collapsible(false)
            .resizable(true)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_size([520.0, 440.0])
            .show(ctx, |ui| {
                if self.rows.is_empty() {
                    ui.colored_label(egui::Color32::GRAY, "No history yet.");
                } else {
                    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                        for row in &self.rows {
                            let display = short_label(&row.path);
                            let progress = match row.page_count {
                                Some(total) if total > 0 => {
                                    format!("  ({}/{})", row.last_page + 1, total)
                                }
                                _ => String::new(),
                            };
                            if ui
                                .button(format!("{}{}", display, progress))
                                .clicked()
                            {
                                result = Some(PathBuf::from(&row.path));
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

fn short_label(path: &str) -> String {
    // Trim to the last 2 path components so long paths stay readable.
    let parts: Vec<&str> = path.rsplitn(3, std::path::MAIN_SEPARATOR).collect();
    match parts.as_slice() {
        [last, parent, _, ..] => format!("{parent}/{last}"),
        [last, ..] => last.to_string(),
        _ => path.to_string(),
    }
}
