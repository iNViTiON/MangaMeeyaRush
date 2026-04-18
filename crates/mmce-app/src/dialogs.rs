//! Modal dialogs: goto-page, about, etc.

use egui::{Align2, Context, Key};

/// State for the Go-to-page dialog.
#[derive(Debug, Default)]
pub struct GotoDialog {
    pub open: bool,
    pub input: String,
}

impl GotoDialog {
    pub fn open(&mut self, current_page_one_based: usize) {
        self.open = true;
        self.input = current_page_one_based.to_string();
    }

    /// Draw the dialog. Returns `Some(idx)` (0-based) if the user
    /// confirmed a valid page number.
    pub fn show(&mut self, ctx: &Context, total_pages: usize) -> Option<usize> {
        if !self.open {
            return None;
        }
        let mut result: Option<usize> = None;
        let mut close = false;
        egui::Window::new("Go to page")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Page number (1 – {}):", total_pages.max(1)));
                let resp = ui.text_edit_singleline(&mut self.input);
                resp.request_focus();
                ui.horizontal(|ui| {
                    if ui.button("Go").clicked() {
                        result = parse_page(&self.input, total_pages);
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                if ctx.input(|i| i.key_pressed(Key::Enter)) {
                    result = parse_page(&self.input, total_pages);
                    close = true;
                }
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
            });
        if close {
            self.open = false;
        }
        result
    }
}

fn parse_page(input: &str, total: usize) -> Option<usize> {
    let n: usize = input.trim().parse().ok()?;
    if n == 0 || total == 0 {
        return None;
    }
    Some(n.saturating_sub(1).min(total - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clamps_to_last() {
        assert_eq!(parse_page("100", 10), Some(9));
        assert_eq!(parse_page("1", 10), Some(0));
        assert_eq!(parse_page("5", 10), Some(4));
    }

    #[test]
    fn parse_rejects_zero_and_garbage() {
        assert_eq!(parse_page("0", 10), None);
        assert_eq!(parse_page("", 10), None);
        assert_eq!(parse_page("abc", 10), None);
    }
}
