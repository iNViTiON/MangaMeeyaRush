//! Info overlay (`I`) + loupe (`L`) + seek bar. All painted over the
//! central book view; none of these widgets affect layout so they can be
//! toggled without re-flowing the spread.

use egui::{
    Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Ui, Vec2,
};
use mmce_core::{Book, Spread};
use mmce_render::PageCache;

/// Paint the image-info overlay in the top-left corner. Lists the current
/// spread's entries with their dimensions (when known) and the total page
/// count.
pub fn paint_info(ui: &Ui, book: &Book, cache: &PageCache, spread: Spread) {
    let src = book.source();
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Page {}/{}", book.cursor() + 1, book.len().max(1)));
    for idx in spread.indices() {
        let name = src.entry_name(idx).unwrap_or("?").to_string();
        let dims = cache
            .page_dimensions(idx)
            .map(|(w, h)| format!("{w}×{h}"))
            .unwrap_or_else(|| "…".into());
        lines.push(format!("  [{}] {}  ({})", idx + 1, name, dims));
    }

    let font = FontId::monospace(13.0);
    let pad = 8.0;
    let line_h = 16.0;
    let origin = ui.min_rect().left_top() + Vec2::new(12.0, 12.0);

    // Measure widest line.
    let painter = ui.painter();
    let max_w = lines
        .iter()
        .map(|s| painter.layout_no_wrap(s.clone(), font.clone(), Color32::WHITE).rect.width())
        .fold(0.0_f32, f32::max);

    let panel = Rect::from_min_size(
        origin,
        Vec2::new(max_w + pad * 2.0, line_h * lines.len() as f32 + pad * 2.0),
    );
    painter.rect_filled(panel, 6.0, Color32::from_black_alpha(170));
    painter.rect_stroke(
        panel,
        6.0,
        Stroke::new(1.0, Color32::from_white_alpha(40)),
    );
    for (i, line) in lines.iter().enumerate() {
        painter.text(
            origin + Vec2::new(pad, pad + i as f32 * line_h),
            Align2::LEFT_TOP,
            line,
            font.clone(),
            Color32::LIGHT_GRAY,
        );
    }
}

/// Paint a loupe — a magnified peephole of the current spread's pixels at
/// cursor position. `cursor` is the pointer location in screen coords;
/// `book_rect` is the rect occupied by the currently-drawn spread;
/// `pages` is the `(index, texture, rect)` layout the renderer used so we
/// can figure out which page the cursor is over.
pub fn paint_loupe(
    ui: &mut Ui,
    cursor: Pos2,
    pages: &[(usize, TextureHandle, Rect)],
    magnification: f32,
    radius: f32,
) {
    // Find the page rect under the cursor.
    let Some((_, tex, rect)) = pages.iter().find(|(_, _, r)| r.contains(cursor)) else {
        return;
    };

    let tex_size = tex.size_vec2();
    let rel = cursor - rect.left_top();
    let u = (rel.x / rect.width()).clamp(0.0, 1.0);
    let v = (rel.y / rect.height()).clamp(0.0, 1.0);

    // Determine the UV window that corresponds to `radius` pixels at
    // `magnification` zoom. The window in texture-pixel space is
    // `2*radius / magnification`.
    let window_px = (2.0 * radius / magnification).max(1.0);
    let half_u = 0.5 * window_px / tex_size.x;
    let half_v = 0.5 * window_px / tex_size.y;
    let uv_rect = Rect::from_min_max(
        Pos2::new((u - half_u).max(0.0), (v - half_v).max(0.0)),
        Pos2::new((u + half_u).min(1.0), (v + half_v).min(1.0)),
    );

    let loupe_rect = Rect::from_center_size(cursor, Vec2::splat(2.0 * radius));
    let painter = ui.painter();
    painter.rect_filled(loupe_rect, radius, Color32::BLACK);
    egui::Image::from_texture(tex)
        .uv(uv_rect)
        .paint_at(ui, loupe_rect);
    painter.rect_stroke(
        loupe_rect,
        radius,
        Stroke::new(2.0, Color32::from_white_alpha(180)),
    );
}

/// Interactive seek bar painted above the status bar. Returns `Some(new_idx)`
/// if the user clicked or dragged to a different page.
pub fn seek_bar(ui: &mut Ui, book: &Book) -> Option<usize> {
    let total = book.len();
    if total <= 1 {
        return None;
    }
    let cursor = book.cursor();
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 22.0),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);

    // Track.
    painter.rect_filled(rect, 3.0, Color32::from_black_alpha(140));

    // Fill.
    let pct = (cursor as f32 + 0.5) / total as f32;
    let fill = Rect::from_min_size(rect.min, Vec2::new(rect.width() * pct, rect.height()));
    painter.rect_filled(fill, 3.0, Color32::from_rgb(120, 160, 220));

    // Thumb.
    let thumb_x = rect.min.x + rect.width() * pct;
    let thumb = Rect::from_center_size(
        Pos2::new(thumb_x, rect.center().y),
        Vec2::new(8.0, rect.height()),
    );
    painter.rect_filled(thumb, 2.0, Color32::LIGHT_BLUE);

    let mut requested: Option<usize> = None;
    let click = resp.clicked() || resp.dragged();
    if click {
        if let Some(pos) = resp.interact_pointer_pos() {
            let rel = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
            let new_idx = ((rel * total as f32).floor() as usize).min(total.saturating_sub(1));
            if new_idx != cursor {
                requested = Some(new_idx);
            }
        }
    }
    requested
}
