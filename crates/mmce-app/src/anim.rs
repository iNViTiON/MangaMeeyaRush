//! Paper-style page-turn animation.
//!
//! When the user advances the cursor, we build a [`PageFlip`] out of the
//! previous frame's painted rects (the "old" spread) and the current
//! frame's painted rects (the "new" spread), then render a rotating quad
//! mesh whose horizontal extent contracts with `cos(θ)` — the classic 2D
//! approximation of a page being turned about its spine.
//!
//! The simulation follows real-paperback physics:
//!
//! - A single leaf flips. It has a FRONT face (visible at t=0) and a BACK
//!   face (visible at t=1). The leaf rotates through 180° about the spine
//!   (in spread mode) or a viewport edge (in single-page mode).
//! - The OLD non-flipping side of the spread stays painted as an overlay
//!   until the leaf lands on it — this preserves the "unchanged half"
//!   illusion instead of popping straight to the new texture.
//! - A subtle perspective squish and lighting tint applied at mid-flip
//!   sells the depth without needing 3D.
//!
//! The legacy `MangaMeeyaCE.ini` `[General]` section has `PageEffect=1`
//! and `PageEffectType=0` — "Flip Effect". The default `PageEffectWait=500`
//! is honoured here as `DEFAULT_DURATION`.

use std::time::{Duration, Instant};

use egui::{
    epaint::{Mesh, Vertex},
    Color32, Pos2, Rect, Shape, TextureHandle, Ui,
};
use mmce_config::BindDir;

/// 500 ms to match the legacy `PageEffectWait` default.
pub const DEFAULT_DURATION: Duration = Duration::from_millis(500);

/// Maximum mid-flip darkening. 0 = no shading, 1 = full black at 90°.
const MAX_SHADE: f32 = 0.35;

/// Vertical contraction of the far edge at mid-flip, as a fraction of
/// page height. Adds a hint of perspective without going fully 3D.
const PERSPECTIVE_Y: f32 = 0.07;

#[derive(Clone)]
pub struct PagePaint {
    pub tex: TextureHandle,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlipDir {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HingeSide {
    Left,
    Right,
}

/// Active paper-flip transition. Carries both the outgoing snapshot and
/// the incoming rects so the mesh has a concrete start/end geometry.
pub struct PageFlip {
    pub prev: Vec<PagePaint>,
    pub next: Vec<PagePaint>,
    pub bind: BindDir,
    pub dir: FlipDir,
    pub started: Instant,
    pub duration: Duration,
}

impl PageFlip {
    pub fn new(
        prev: Vec<PagePaint>,
        next: Vec<PagePaint>,
        bind: BindDir,
        dir: FlipDir,
    ) -> Self {
        Self {
            prev,
            next,
            bind,
            dir,
            started: Instant::now(),
            duration: DEFAULT_DURATION,
        }
    }

    pub fn progress(&self) -> f32 {
        let elapsed = self.started.elapsed().as_secs_f32();
        let total = self.duration.as_secs_f32().max(0.001);
        (elapsed / total).clamp(0.0, 1.0)
    }

    pub fn is_done(&self) -> bool {
        self.progress() >= 1.0
    }
}

/// Paint the flipping leaf on top of whatever the renderer already drew.
/// Caller is expected to have already painted the new spread as the
/// background; we add overlays + the leaf over it.
pub fn paint_flip(ui: &mut Ui, flip: &PageFlip) {
    let t = ease_out_cubic(flip.progress());
    if flip.prev.len() == 2 && flip.next.len() == 2 {
        paint_spread_flip(ui, flip, t);
    } else if flip.prev.len() == 1 && flip.next.len() == 1 {
        paint_single_flip(ui, flip, t);
    }
    // Mismatched page counts (e.g. Auto mode transitioning between single
    // and spread) — silently skip the animation rather than glitch.
}

/// Real-book spread flip. One leaf rotates through the spine.
fn paint_spread_flip(ui: &mut Ui, flip: &PageFlip, t: f32) {
    let (start_side, end_side) = sides_for(flip.bind, flip.dir);

    // Physical display order is always [left, right]. Pick start/end.
    let (prev_start, prev_end) = pick_pair(&flip.prev, start_side);
    let (_next_start, next_end) = pick_pair(&flip.next, start_side);

    // The end_side of the OLD spread stays visible until the leaf lands
    // there. Paint it as an opaque overlay on top of the new-spread BG.
    paint_overlay(ui, prev_end);

    // Leaf geometry is anchored to the spine adjacent to prev_start.
    let rect = prev_start.rect;
    let page_w = rect.width();
    let page_h = rect.height();
    let hinge_x = match start_side {
        HingeSide::Left => rect.max.x,
        HingeSide::Right => rect.min.x,
    };

    let theta = t * std::f32::consts::PI;
    let cos_t = theta.cos();
    let sin_abs = theta.sin().abs();
    let far_x = match start_side {
        HingeSide::Left => hinge_x - page_w * cos_t,
        HingeSide::Right => hinge_x + page_w * cos_t,
    };

    let dy = 0.5 * page_h * PERSPECTIVE_Y * sin_abs;
    let top_y = rect.min.y + dy;
    let bot_y = rect.max.y - dy;

    let is_front = cos_t >= 0.0;
    let (tex, hinge_u, far_u) = if is_front {
        // Front face: old texture anchored to start_side. Hinge side of
        // the quad corresponds to the spine-adjacent edge of the page.
        let hinge_u = if start_side == HingeSide::Left { 1.0 } else { 0.0 };
        (&prev_start.tex, hinge_u, 1.0 - hinge_u)
    } else {
        // Back face: new texture on the opposite side.
        let hinge_u = if end_side == HingeSide::Left { 1.0 } else { 0.0 };
        (&next_end.tex, hinge_u, 1.0 - hinge_u)
    };

    let tint = shade_tint(sin_abs);

    let corners = [
        Pos2::new(hinge_x, rect.min.y),
        Pos2::new(far_x, top_y),
        Pos2::new(far_x, bot_y),
        Pos2::new(hinge_x, rect.max.y),
    ];
    let uvs = [
        Pos2::new(hinge_u, 0.0),
        Pos2::new(far_u, 0.0),
        Pos2::new(far_u, 1.0),
        Pos2::new(hinge_u, 1.0),
    ];
    paint_quad_mesh(ui, tex, corners, uvs, tint);
}

/// Single-page flip: the entire viewport is the leaf. Hinge on the edge
/// the page is turning *toward*; the far edge rotates through 180° to
/// land off-screen on the other side. At t=0.5 we swap front → back, so
/// the viewer sees the new page materialise from the far edge.
fn paint_single_flip(ui: &mut Ui, flip: &PageFlip, t: f32) {
    let (_start_side, end_side) = sides_for(flip.bind, flip.dir);
    let prev = &flip.prev[0];
    let next = &flip.next[0];

    let rect = prev.rect;
    let page_w = rect.width();
    let page_h = rect.height();
    let theta = t * std::f32::consts::PI;
    let cos_t = theta.cos();
    let sin_abs = theta.sin().abs();

    let is_front = cos_t >= 0.0;
    let scale = cos_t.abs();
    if scale < 1e-3 {
        return;
    }

    // Hinge on the end side — the leaf swings away toward that edge.
    let hinge_x = match end_side {
        HingeSide::Left => rect.min.x,
        HingeSide::Right => rect.max.x,
    };
    let far_x = match end_side {
        HingeSide::Left => rect.min.x + page_w * scale,
        HingeSide::Right => rect.max.x - page_w * scale,
    };

    let dy = 0.5 * page_h * PERSPECTIVE_Y * sin_abs;
    let top_y = rect.min.y + dy;
    let bot_y = rect.max.y - dy;

    let (tex, hinge_u, far_u) = if is_front {
        // Old page fills the whole viewport at t=0. Hinge is on end_side
        // of the viewport = end_side of the old texture.
        match end_side {
            HingeSide::Left => (&prev.tex, 0.0, 1.0),
            HingeSide::Right => (&prev.tex, 1.0, 0.0),
        }
    } else {
        // New page fills at t=1. Same UV orientation since the leaf's
        // "back face" paints the same-direction new texture.
        match end_side {
            HingeSide::Left => (&next.tex, 0.0, 1.0),
            HingeSide::Right => (&next.tex, 1.0, 0.0),
        }
    };

    let tint = shade_tint(sin_abs);

    let corners = [
        Pos2::new(hinge_x, rect.min.y),
        Pos2::new(far_x, top_y),
        Pos2::new(far_x, bot_y),
        Pos2::new(hinge_x, rect.max.y),
    ];
    let uvs = [
        Pos2::new(hinge_u, 0.0),
        Pos2::new(far_u, 0.0),
        Pos2::new(far_u, 1.0),
        Pos2::new(hinge_u, 1.0),
    ];
    paint_quad_mesh(ui, tex, corners, uvs, tint);
}

fn sides_for(bind: BindDir, dir: FlipDir) -> (HingeSide, HingeSide) {
    match (bind, dir) {
        (BindDir::RightToLeft, FlipDir::Forward) => (HingeSide::Left, HingeSide::Right),
        (BindDir::LeftToRight, FlipDir::Forward) => (HingeSide::Right, HingeSide::Left),
        (BindDir::RightToLeft, FlipDir::Backward) => (HingeSide::Right, HingeSide::Left),
        (BindDir::LeftToRight, FlipDir::Backward) => (HingeSide::Left, HingeSide::Right),
    }
}

fn pick_pair(paints: &[PagePaint], start_side: HingeSide) -> (&PagePaint, &PagePaint) {
    // `paints` is always in physical order [left, right]. Return
    // (start, end) so geometry computations can anchor to start.
    match start_side {
        HingeSide::Left => (&paints[0], &paints[1]),
        HingeSide::Right => (&paints[1], &paints[0]),
    }
}

/// Multiplicative shade applied as the vertex colour. Vertex colour in
/// egui is multiplied with the sampled texture, so `(c,c,c,255)` darkens.
fn shade_tint(sin_abs: f32) -> Color32 {
    let shade = 1.0 - MAX_SHADE * sin_abs;
    let c = (255.0 * shade).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(c, c, c)
}

fn paint_overlay(ui: &Ui, p: &PagePaint) {
    egui::Image::from_texture(&p.tex)
        .fit_to_exact_size(p.rect.size())
        .paint_at(ui, p.rect);
}

fn paint_quad_mesh(
    ui: &mut Ui,
    tex: &TextureHandle,
    corners: [Pos2; 4],
    uvs: [Pos2; 4],
    tint: Color32,
) {
    let mut mesh = Mesh::with_texture(tex.id());
    for i in 0..4 {
        mesh.vertices.push(Vertex {
            pos: corners[i],
            uv: uvs[i],
            color: tint,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    ui.painter().add(Shape::mesh(mesh));
}

/// Ease-out-cubic — same curve the legacy app seems to use (leaves fast,
/// arrives gently).
pub fn ease_out_cubic(t: f32) -> f32 {
    let x = 1.0 - t.clamp(0.0, 1.0);
    1.0 - x * x * x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_monotone() {
        assert!(ease_out_cubic(0.1) < ease_out_cubic(0.5));
        assert!(ease_out_cubic(0.5) < ease_out_cubic(0.9));
    }

    #[test]
    fn ease_out_cubic_bounds() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(ease_out_cubic(-0.1), 0.0);
        assert!((ease_out_cubic(1.5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sides_forward_rtl_flip_starts_left() {
        let (a, b) = sides_for(BindDir::RightToLeft, FlipDir::Forward);
        assert_eq!(a, HingeSide::Left);
        assert_eq!(b, HingeSide::Right);
    }

    #[test]
    fn sides_forward_ltr_flip_starts_right() {
        let (a, b) = sides_for(BindDir::LeftToRight, FlipDir::Forward);
        assert_eq!(a, HingeSide::Right);
        assert_eq!(b, HingeSide::Left);
    }

    #[test]
    fn sides_backward_reverses_forward_direction() {
        let (a, b) = sides_for(BindDir::LeftToRight, FlipDir::Backward);
        let (c, d) = sides_for(BindDir::LeftToRight, FlipDir::Forward);
        assert_eq!(a, d);
        assert_eq!(b, c);
    }
}
