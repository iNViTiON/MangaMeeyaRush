//! Dialogs for destructive or renaming file operations in the explorer.

use std::path::PathBuf;

use egui::{Align2, Color32, Context, Key};

/// Confirmation dialog for Delete. Host code passes the target path in on
/// open; on confirm, it performs the fs delete itself.
#[derive(Debug, Default)]
pub struct ConfirmDelete {
    pub open: bool,
    pub path: Option<PathBuf>,
    pub is_dir: bool,
}

impl ConfirmDelete {
    pub fn open_for(&mut self, path: PathBuf) {
        self.is_dir = path.is_dir();
        self.path = Some(path);
        self.open = true;
    }

    /// Returns `Some(path)` iff user confirmed.
    pub fn show(&mut self, ctx: &Context) -> Option<PathBuf> {
        if !self.open {
            return None;
        }
        let mut confirm = false;
        let mut close = false;
        egui::Window::new("Delete?")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if let Some(p) = &self.path {
                    let kind = if self.is_dir { "folder" } else { "file" };
                    ui.label(format!("Permanently delete this {kind}?"));
                    ui.add_space(4.0);
                    ui.colored_label(Color32::LIGHT_GRAY, p.display().to_string());
                    if self.is_dir {
                        ui.add_space(4.0);
                        ui.colored_label(
                            Color32::from_rgb(230, 180, 120),
                            "This removes the folder and everything inside it.",
                        );
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() || ctx.input(|i| i.key_pressed(Key::Enter)) {
                        confirm = true;
                        close = true;
                    }
                    if ui.button("Cancel").clicked() || ctx.input(|i| i.key_pressed(Key::Escape)) {
                        close = true;
                    }
                });
            });
        if close {
            self.open = false;
        }
        if confirm {
            self.path.take()
        } else {
            None
        }
    }
}

/// Rename dialog. Host passes the current file name in on open; on confirm,
/// returns the new name (no directory component).
#[derive(Debug, Default)]
pub struct RenameDialog {
    pub open: bool,
    pub input: String,
    pub original: String,
}

impl RenameDialog {
    pub fn open_for(&mut self, name: &str) {
        self.input = name.to_string();
        self.original = name.to_string();
        self.open = true;
    }

    pub fn show(&mut self, ctx: &Context) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut committed: Option<String> = None;
        let mut close = false;
        egui::Window::new("Rename")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Rename “{}” to:", self.original));
                let resp = ui.text_edit_singleline(&mut self.input);
                resp.request_focus();
                ui.horizontal(|ui| {
                    let confirm =
                        ui.button("Rename").clicked() || ctx.input(|i| i.key_pressed(Key::Enter));
                    if confirm {
                        let trimmed = self.input.trim().to_string();
                        if !trimmed.is_empty() && trimmed != self.original {
                            committed = Some(trimmed);
                        }
                        close = true;
                    }
                    if ui.button("Cancel").clicked() || ctx.input(|i| i.key_pressed(Key::Escape)) {
                        close = true;
                    }
                });
            });
        if close {
            self.open = false;
        }
        committed
    }
}
