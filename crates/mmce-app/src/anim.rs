//! Minimal transition support for page turns. Keeps the previous spread's
//! painted rects around for a short duration and fades them out over a
//! new spread — a cheap crossfade that makes page turns feel snappier
//! without a dedicated render pipeline.

use std::time::{Duration, Instant};

use egui::{Color32, Rect, TextureHandle};

pub const DEFAULT_DURATION: Duration = Duration::from_millis(160);

/// One painted page from the previous frame — we hold on to the texture
/// handle (keeps GPU memory alive) and the screen rect it lived at.
#[derive(Clone)]
pub struct PagePaint {
    pub tex: TextureHandle,
    pub rect: Rect,
}

/// Active cross-fade. `started` is monotonic; `duration` drives the
/// lerp. Call `progress` each frame to get `[0.0, 1.0]`. When ≥1, drop it.
pub struct PageFade {
    pub prev: Vec<PagePaint>,
    pub started: Instant,
    pub duration: Duration,
}

impl PageFade {
    pub fn new(prev: Vec<PagePaint>) -> Self {
        Self {
            prev,
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

/// Cubic ease-out — standard shape for "leaves fast, arrives gentle."
pub fn ease_out_cubic(t: f32) -> f32 {
    let x = 1.0 - t.clamp(0.0, 1.0);
    1.0 - x * x * x
}

/// Paint the outgoing spread with a fading alpha. Call this AFTER the new
/// spread has drawn so the previous frame sits on top and melts away.
pub fn paint_fade(ui: &mut egui::Ui, fade: &PageFade) {
    let t = ease_out_cubic(fade.progress());
    let alpha = (255.0 * (1.0 - t)) as u8;
    if alpha == 0 {
        return;
    }
    let tint = Color32::from_white_alpha(alpha);
    for p in &fade.prev {
        egui::Image::from_texture(&p.tex)
            .tint(tint)
            .fit_to_exact_size(p.rect.size())
            .paint_at(ui, p.rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_monotone() {
        let a = ease_out_cubic(0.1);
        let b = ease_out_cubic(0.5);
        let c = ease_out_cubic(0.9);
        assert!(a < b);
        assert!(b < c);
        assert!(c <= 1.0);
    }

    #[test]
    fn ease_out_cubic_bounds() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        // out-of-range values clamp.
        assert_eq!(ease_out_cubic(-0.1), 0.0);
        assert!((ease_out_cubic(1.5) - 1.0).abs() < 1e-6);
    }
}
