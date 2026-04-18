//! Paper-style page-turn animation.
//!
//! Legacy `MangaMeeyaCE.ini` `[General].PageEffect=1` + `PageEffectType=0`
//! maps to the binary's string-table label "Flip Effect" (STRING id 510).
//! The default `PageEffectWait=500` is honoured as [`DEFAULT_DURATION`].
//!
//! Physical model:
//!
//! - A rigid leaf rotates 180° about a vertical hinge. The hinge sits at
//!   the spine in spread mode and at the horizontal centre of the
//!   viewport in single-page mode (so even a single image "folds" at the
//!   middle like a real book turning one page).
//! - The leaf has a FRONT face (the outgoing texture, visible at `t=0`)
//!   and a BACK face (the incoming texture, visible at `t=1`). Each face
//!   uses its OWN target rect so intrinsic page aspect is preserved — we
//!   don't squash the new page into the old one's rendered dimensions.
//! - Before the leaf lands, the outgoing half at the landing side stays
//!   painted as an overlay so the reader doesn't see a pop from old to
//!   new on the "unchanged" side.
//! - No faux-3D perspective: the physical paper height doesn't change as
//!   the page turns, only its projected width (`cos θ`). A subtle
//!   shadow at mid-flip sells the motion without distorting aspect.

use std::time::{Duration, Instant};

use egui::{
    epaint::{Mesh, Vertex},
    Color32, Pos2, Rect, Shape, TextureHandle, Ui,
};
use mmce_config::BindDir;

pub const DEFAULT_DURATION: Duration = Duration::from_millis(500);

/// Maximum mid-flip darkening (multiplicative tint). 0 = no shading,
/// 1 = fully black at 90°.
const MAX_SHADE: f32 = 0.35;

#[derive(Clone)]
pub struct PagePaint {
    pub tex: TextureHandle,
    /// The screen rect this page occupied when painted.
    pub rect: Rect,
    /// UV sub-rect of the texture that this paint represents. For a
    /// regular spread page it's the full `(0,0)-(1,1)` rect; for a
    /// synthesised "half" used in single-page flips it's one of
    /// `(0,0)-(0.5,1)` or `(0.5,0)-(1,1)`.
    pub uv: Rect,
}

impl PagePaint {
    pub fn full(tex: TextureHandle, rect: Rect) -> Self {
        Self {
            tex,
            rect,
            uv: Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
        }
    }

    /// Split a full-page paint into left and right halves anchored at
    /// the *viewport* centre `split_x`. Used so single-page flips fold
    /// at the centre.
    pub fn split_at(self, split_x: f32) -> (Self, Self) {
        let left_rect = Rect::from_min_max(self.rect.min, Pos2::new(split_x, self.rect.max.y));
        let right_rect =
            Rect::from_min_max(Pos2::new(split_x, self.rect.min.y), self.rect.max);

        // Map `split_x` back into the texture's UV space. The texture
        // fills `self.rect` via a linear mapping, so:
        //   u_split = lerp(uv.min.x, uv.max.x, (split_x - rect.min.x) / rect.width())
        let w = self.rect.width().max(1e-3);
        let f = ((split_x - self.rect.min.x) / w).clamp(0.0, 1.0);
        let uv_split = self.uv.min.x + (self.uv.max.x - self.uv.min.x) * f;

        let left_uv =
            Rect::from_min_max(self.uv.min, Pos2::new(uv_split, self.uv.max.y));
        let right_uv =
            Rect::from_min_max(Pos2::new(uv_split, self.uv.min.y), self.uv.max);

        (
            Self {
                tex: self.tex.clone(),
                rect: left_rect,
                uv: left_uv,
            },
            Self {
                tex: self.tex,
                rect: right_rect,
                uv: right_uv,
            },
        )
    }
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

pub struct PageFlip {
    /// Outgoing halves (or pages) in physical display order
    /// `[left, right]`. Two entries always.
    pub prev: Vec<PagePaint>,
    /// Incoming halves (or pages), `[left, right]`.
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
/// Caller is expected to have painted the new spread as the background;
/// this adds the non-flipping overlay and the rotating mesh.
pub fn paint_flip(ui: &mut Ui, flip: &PageFlip) {
    if flip.prev.len() != 2 || flip.next.len() != 2 {
        // Structure mismatch (e.g. Auto mode crossing single/spread) —
        // skip rather than glitch.
        return;
    }
    let t = ease_out_cubic(flip.progress());
    let (start_side, end_side) = sides_for(flip.bind, flip.dir);

    let (prev_start, prev_end) = pick_pair(&flip.prev, start_side);
    let (_next_start, next_end) = pick_pair(&flip.next, start_side);

    // The end-side of the OLD spread must stay visible until the leaf
    // lands on it. Paint it as an opaque overlay over the new-spread BG.
    paint_overlay(ui, prev_end);

    // Front vs back face. `cos(θ) >= 0` → we see the front (outgoing);
    // `cos(θ) < 0` → the back (incoming).
    let theta = t * std::f32::consts::PI;
    let cos_t = theta.cos();
    let sin_abs = theta.sin().abs();
    let is_front = cos_t >= 0.0;

    // Leaf geometry anchors at the hinge adjacent to the *current* face.
    // Front uses prev_start's rect; back uses next_end's rect. This is
    // why the incoming page lands at its correct final size — its rect
    // is driven by the NEW render, not a stretched version of the old.
    let face = if is_front { prev_start } else { next_end };
    let hinge_side = if is_front { start_side } else { end_side };

    let rect = face.rect;
    let page_w = rect.width();
    let hinge_x = match hinge_side {
        HingeSide::Left => rect.max.x,
        HingeSide::Right => rect.min.x,
    };
    // Projected far-edge offset from hinge.
    // Front (hinge_side = start_side): cos goes 1 → 0 as flip progresses;
    //   far_x moves from the page's outer edge toward the hinge.
    // Back  (hinge_side = end_side):   cos goes 0 → -1;
    //   far_x moves from hinge outward.
    let far_x = match hinge_side {
        HingeSide::Left => hinge_x - page_w * cos_t.abs(),
        HingeSide::Right => hinge_x + page_w * cos_t.abs(),
    };

    // Height is unchanged through the flip — paper doesn't squish.
    let top_y = rect.min.y;
    let bot_y = rect.max.y;

    // UV mapping within the face's sub-UV rect:
    //   hinge side of the quad ↔ spine-adjacent edge of the page.
    let hinge_u_local = if hinge_side == HingeSide::Left { 1.0 } else { 0.0 };
    let far_u_local = 1.0 - hinge_u_local;
    let uv = face.uv;
    let uv_x = |local_u: f32| uv.min.x + (uv.max.x - uv.min.x) * local_u;
    let hinge_u = uv_x(hinge_u_local);
    let far_u = uv_x(far_u_local);
    let uv_top = uv.min.y;
    let uv_bot = uv.max.y;

    let tint = shade_tint(sin_abs);

    let corners = [
        Pos2::new(hinge_x, top_y),
        Pos2::new(far_x, top_y),
        Pos2::new(far_x, bot_y),
        Pos2::new(hinge_x, bot_y),
    ];
    let uvs = [
        Pos2::new(hinge_u, uv_top),
        Pos2::new(far_u, uv_top),
        Pos2::new(far_u, uv_bot),
        Pos2::new(hinge_u, uv_bot),
    ];
    paint_quad_mesh(ui, &face.tex, corners, uvs, tint);
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
    match start_side {
        HingeSide::Left => (&paints[0], &paints[1]),
        HingeSide::Right => (&paints[1], &paints[0]),
    }
}

fn shade_tint(sin_abs: f32) -> Color32 {
    let shade = 1.0 - MAX_SHADE * sin_abs;
    let c = (255.0 * shade).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(c, c, c)
}

fn paint_overlay(ui: &Ui, p: &PagePaint) {
    egui::Image::from_texture(&p.tex)
        .uv(p.uv)
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
    fn sides_forward_rtl_starts_left() {
        let (a, b) = sides_for(BindDir::RightToLeft, FlipDir::Forward);
        assert_eq!(a, HingeSide::Left);
        assert_eq!(b, HingeSide::Right);
    }

    #[test]
    fn sides_forward_ltr_starts_right() {
        let (a, b) = sides_for(BindDir::LeftToRight, FlipDir::Forward);
        assert_eq!(a, HingeSide::Right);
        assert_eq!(b, HingeSide::Left);
    }

    #[test]
    fn sides_backward_is_reverse_of_forward() {
        let (a, b) = sides_for(BindDir::LeftToRight, FlipDir::Backward);
        let (c, d) = sides_for(BindDir::LeftToRight, FlipDir::Forward);
        assert_eq!(a, d);
        assert_eq!(b, c);
    }
}
