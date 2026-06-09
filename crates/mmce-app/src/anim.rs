//! Paper-style page-turn animation.
//!
//! Legacy `MangaMeeyaCE.ini` `[General].PageEffect=1` + `PageEffectType=0`
//! maps to the binary's string-table label "Flip Effect" (STRING id 510).
//! The default `PageEffectWait=500` is honoured as [`DEFAULT_DURATION`].
//!
//! Physical model:
//!
//! - A paper leaf rotates 180° about a vertical hinge. The hinge sits at
//!   the spine in spread mode and at the horizontal centre of the
//!   viewport in single-page mode (so even a single image "folds" at the
//!   middle like a real book turning one page).
//! - The leaf is tessellated into vertical strips so it can curl gently
//!   at mid-flip. The curl is modelled as a horizontal offset driven by
//!   `sin(strip_u * π)` and a subtle vertical "droop" at the free edge.
//!   The hinge column stays exactly on `hinge_x` — no sliding, no pop.
//! - The leaf has a FRONT face (the outgoing texture, visible at `t=0`)
//!   and a BACK face (the incoming texture, visible at `t=1`). Each face
//!   uses its OWN target rect so intrinsic page aspect is preserved.
//! - Before the leaf lands, the outgoing half at the landing side stays
//!   painted as an overlay so the reader doesn't see a pop from old to
//!   new on the "unchanged" side. A soft drop shadow tracks the leaf as
//!   it lifts and lands.
//! - No faux-3D perspective: the physical paper height doesn't change as
//!   the page turns, only its projected width (`cos θ`). The per-vertex
//!   shading and curl sell the motion without distorting aspect.

use std::time::{Duration, Instant};

use egui::{
    epaint::{Mesh, Vertex},
    Color32, Pos2, Rect, Shape, TextureHandle, Ui,
};
use mmce_config::BindDir;

pub const DEFAULT_DURATION: Duration = Duration::from_millis(500);

/// Number of vertical strips in the tessellated leaf. 16 strips ⇒
/// 17 columns × 2 rows = 34 verts / 32 triangles. Plenty for smooth
/// curl, trivial for the GPU.
const STRIP_COUNT: usize = 16;

/// Maximum mid-flip darkening near the hinge (multiplicative tint).
/// Chosen to read as "paper catching shadow" without looking cartoony.
const MAX_SHADE_HINGE: f32 = 0.32;

/// Maximum mid-flip darkening at the far (free) edge. Lower than the
/// hinge value — the curved inner crease catches less light than the
/// outer surface, so the far edge stays brighter.
const MAX_SHADE_FAR: f32 = 0.14;

/// Mid-flip horizontal curl amplitude as a fraction of projected page
/// width. The curl pulls the middle of the leaf slightly *inward*
/// (toward the hinge), like gently bending a sheet.
const CURL_AMP: f32 = 0.055;

/// Mid-flip vertical droop at the free edge as a fraction of page
/// height. Paper is not perfectly rigid — the far edge sags a touch.
const DROOP_AMP: f32 = 0.018;

/// Maximum alpha of the soft drop shadow cast by the lifted leaf onto
/// the landing spread. Kept subtle so it reads as depth, not gimmick.
const SHADOW_MAX_ALPHA: f32 = 0.28;

/// Width of the drop-shadow band as a fraction of page width.
const SHADOW_BAND_FRAC: f32 = 0.22;

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
        let right_rect = Rect::from_min_max(Pos2::new(split_x, self.rect.min.y), self.rect.max);

        // Map `split_x` back into the texture's UV space. The texture
        // fills `self.rect` via a linear mapping, so:
        //   u_split = lerp(uv.min.x, uv.max.x, (split_x - rect.min.x) / rect.width())
        let w = self.rect.width().max(1e-3);
        let f = ((split_x - self.rect.min.x) / w).clamp(0.0, 1.0);
        let uv_split = self.uv.min.x + (self.uv.max.x - self.uv.min.x) * f;

        let left_uv = Rect::from_min_max(self.uv.min, Pos2::new(uv_split, self.uv.max.y));
        let right_uv = Rect::from_min_max(Pos2::new(uv_split, self.uv.min.y), self.uv.max);

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
    pub fn new(prev: Vec<PagePaint>, next: Vec<PagePaint>, bind: BindDir, dir: FlipDir) -> Self {
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
    let t = ease_in_out_quart(flip.progress());
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

    // Soft drop shadow on the landing spread, under the lifted leaf.
    // Drawn BEFORE the mesh so the leaf occludes it as it lands flush.
    paint_drop_shadow(ui, face.rect, hinge_side, sin_abs);

    paint_leaf_mesh(ui, face, hinge_side, cos_t, sin_abs);
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

/// Per-vertex multiplicative tint. The shading gradient is linear in
/// `u` (where `u=0` is the free edge, `u=1` is the hinge). Near the
/// hinge the "inside" of the fold catches less light and reads darker;
/// the bright far edge stays more luminous. Both ends darken smoothly
/// as `sin_abs` peaks at 90°.
fn shade_tint_at(u: f32, sin_abs: f32) -> Color32 {
    let shade_amount = lerp(MAX_SHADE_FAR, MAX_SHADE_HINGE, u.clamp(0.0, 1.0));
    let shade = 1.0 - shade_amount * sin_abs;
    let c = (255.0 * shade).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(c, c, c)
}

fn paint_overlay(ui: &Ui, p: &PagePaint) {
    egui::Image::from_texture(&p.tex)
        .uv(p.uv)
        .fit_to_exact_size(p.rect.size())
        .paint_at(ui, p.rect);
}

/// Build and emit the tessellated leaf mesh. The mesh is organised as
/// `STRIP_COUNT + 1` columns × 2 rows. Column 0 sits at the free edge,
/// column `STRIP_COUNT` sits on the hinge. Strip `i` is the quad
/// between column `i` and column `i+1`.
fn paint_leaf_mesh(ui: &mut Ui, face: &PagePaint, hinge_side: HingeSide, cos_t: f32, sin_abs: f32) {
    let rect = face.rect;
    let page_w = rect.width();
    let page_h = rect.height();
    let hinge_x = match hinge_side {
        HingeSide::Left => rect.max.x,
        HingeSide::Right => rect.min.x,
    };
    // Sign of "far-edge direction" relative to the hinge.
    let far_sign: f32 = match hinge_side {
        HingeSide::Left => -1.0,
        HingeSide::Right => 1.0,
    };
    // Rigid-projection width (paper doesn't squish, only projects).
    let projected_w = page_w * cos_t.abs();

    // UV mapping: the hinge column maps to the spine-adjacent UV edge,
    // and the far column maps to the outer edge of the face's sub-UV.
    let hinge_u_local = if hinge_side == HingeSide::Left {
        1.0
    } else {
        0.0
    };
    let far_u_local = 1.0 - hinge_u_local;
    let uv = face.uv;
    let uv_hinge = uv.min.x + (uv.max.x - uv.min.x) * hinge_u_local;
    let uv_far = uv.min.x + (uv.max.x - uv.min.x) * far_u_local;
    let uv_top = uv.min.y;
    let uv_bot = uv.max.y;

    let mut mesh = Mesh::with_texture(face.tex.id());
    let cols = STRIP_COUNT + 1;
    mesh.vertices.reserve(cols * 2);
    mesh.indices.reserve(STRIP_COUNT * 6);

    for i in 0..cols {
        // `u` runs 0 at the free edge → 1 at the hinge, matching the
        // shading model (hinge darker).
        let u = i as f32 / STRIP_COUNT as f32;

        // Base rigid-rotation position for this column: a linear
        // interpolation between the hinge and the projected far edge.
        let base_offset = (1.0 - u) * projected_w * far_sign;

        // Subtle curl: middle columns bow slightly inward (toward the
        // hinge). Zero at both ends, peak at u = 0.5. Scaled by
        // `sin_abs` so it vanishes at rest.
        let curl_shape = (u * std::f32::consts::PI).sin();
        let curl_offset = -far_sign * CURL_AMP * page_w * curl_shape * sin_abs;

        let col_x = hinge_x + base_offset + curl_offset;

        // Vertical droop: the free edge sags slightly. Zero at hinge,
        // peak at u = 0. Scaled by `sin_abs` so the leaf is flat at
        // rest and at landing.
        let droop = (1.0 - u) * DROOP_AMP * page_h * sin_abs;

        let top_y = rect.min.y + droop;
        let bot_y = rect.max.y - droop;

        // Per-column UV (linearly interpolated across the sub-UV).
        let uv_x = lerp(uv_far, uv_hinge, u);

        let tint = shade_tint_at(u, sin_abs);

        mesh.vertices.push(Vertex {
            pos: Pos2::new(col_x, top_y),
            uv: Pos2::new(uv_x, uv_top),
            color: tint,
        });
        mesh.vertices.push(Vertex {
            pos: Pos2::new(col_x, bot_y),
            uv: Pos2::new(uv_x, uv_bot),
            color: tint,
        });
    }

    // Triangulate adjacent columns into quads.
    for i in 0..STRIP_COUNT {
        let top_l = (i * 2) as u32;
        let bot_l = top_l + 1;
        let top_r = ((i + 1) * 2) as u32;
        let bot_r = top_r + 1;
        mesh.indices
            .extend_from_slice(&[top_l, top_r, bot_r, top_l, bot_r, bot_l]);
    }

    ui.painter().add(Shape::mesh(mesh));
}

/// Soft drop shadow on the static landing spread. Drawn as a narrow
/// gradient band adjacent to the hinge on the landing side. Alpha
/// peaks at mid-flip (when the leaf is highest) and fades to zero at
/// rest and at landing.
fn paint_drop_shadow(ui: &mut Ui, rect: Rect, hinge_side: HingeSide, sin_abs: f32) {
    let alpha = (SHADOW_MAX_ALPHA * sin_abs).clamp(0.0, 1.0);
    if alpha <= 0.001 {
        return;
    }
    let page_w = rect.width();
    let band_w = page_w * SHADOW_BAND_FRAC;

    // Shadow falls on the *opposite* side of the hinge from the face
    // (i.e. onto the already-painted landing half). Anchoring: fully
    // opaque edge sits at the hinge, fading outward away from it.
    let (inner_x, outer_x) = match hinge_side {
        HingeSide::Left => (rect.max.x, rect.max.x + band_w),
        HingeSide::Right => (rect.min.x, rect.min.x - band_w),
    };

    let top_y = rect.min.y;
    let bot_y = rect.max.y;
    let inner = Color32::from_black_alpha((alpha * 255.0) as u8);
    let outer = Color32::from_black_alpha(0);
    let white_uv = egui::epaint::WHITE_UV;

    let mut mesh = Mesh::default();
    let verts = [
        (Pos2::new(inner_x, top_y), inner),
        (Pos2::new(outer_x, top_y), outer),
        (Pos2::new(outer_x, bot_y), outer),
        (Pos2::new(inner_x, bot_y), inner),
    ];
    for (pos, color) in verts {
        mesh.vertices.push(Vertex {
            pos,
            uv: white_uv,
            color,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    ui.painter().add(Shape::mesh(mesh));
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Legacy cubic easing, kept for back-compat with anything that may
/// import it. Current flips use [`ease_in_out_quart`].
#[allow(dead_code)]
pub fn ease_out_cubic(t: f32) -> f32 {
    let x = 1.0 - t.clamp(0.0, 1.0);
    1.0 - x * x * x
}

/// Symmetric quartic ease-in-out. Motion accelerates from rest,
/// peaks in the middle, and decelerates into the landing. Feels
/// natural for a page turn — no bounce, no rubber-band.
pub fn ease_in_out_quart(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        8.0 * t * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - f * f * f * f / 2.0
    }
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
    fn ease_in_out_quart_bounds_and_midpoint() {
        assert_eq!(ease_in_out_quart(0.0), 0.0);
        assert!((ease_in_out_quart(1.0) - 1.0).abs() < 1e-6);
        assert!((ease_in_out_quart(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(ease_in_out_quart(-0.1), 0.0);
        assert!((ease_in_out_quart(1.5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ease_in_out_quart_monotone() {
        assert!(ease_in_out_quart(0.1) < ease_in_out_quart(0.25));
        assert!(ease_in_out_quart(0.25) < ease_in_out_quart(0.5));
        assert!(ease_in_out_quart(0.5) < ease_in_out_quart(0.75));
        assert!(ease_in_out_quart(0.75) < ease_in_out_quart(0.9));
    }

    #[test]
    fn ease_in_out_quart_is_slower_at_edges() {
        // Symmetric ease-in-out accrues progress more slowly than the
        // linear baseline near the endpoints.
        assert!(ease_in_out_quart(0.1) < 0.1);
        assert!(ease_in_out_quart(0.9) > 0.9);
    }

    #[test]
    fn shade_tint_at_rest_is_white() {
        assert_eq!(shade_tint_at(0.5, 0.0), Color32::from_rgb(255, 255, 255));
    }

    #[test]
    fn shade_tint_hinge_darker_than_far_mid_flip() {
        let hinge = shade_tint_at(1.0, 1.0);
        let far = shade_tint_at(0.0, 1.0);
        assert!(hinge.r() < far.r());
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
