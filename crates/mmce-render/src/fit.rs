//! Pure fit/zoom math, separated so it's unit-testable without a GPU context.

use egui::Vec2;
use mmce_config::FitMode;

pub struct FitParams {
    pub image: Vec2,
    pub viewport: Vec2,
    pub fit: FitMode,
    pub zoom: f32,
    pub no_zoom_in: bool,
}

/// Returns the logical rendered size of the image given the fit mode.
pub fn fit_rect(p: FitParams) -> Vec2 {
    if p.image.x <= 0.0 || p.image.y <= 0.0 {
        return Vec2::ZERO;
    }
    let vp = p.viewport.max(Vec2::new(1.0, 1.0));
    let scale = match p.fit {
        FitMode::Original => 1.0,
        FitMode::Fit => (vp.x / p.image.x).min(vp.y / p.image.y),
        FitMode::FitWidth => vp.x / p.image.x,
        FitMode::FitHeight => vp.y / p.image.y,
        FitMode::Custom => p.zoom,
    };
    let scale = if p.no_zoom_in && p.fit != FitMode::Custom {
        scale.min(1.0)
    } else {
        scale
    };
    p.image * scale.max(0.01)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmce_config::FitMode;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    #[test]
    fn fit_picks_smaller_axis() {
        let r = fit_rect(FitParams {
            image: v(2000.0, 1000.0),
            viewport: v(1000.0, 1000.0),
            fit: FitMode::Fit,
            zoom: 1.0,
            no_zoom_in: false,
        });
        assert_eq!(r, v(1000.0, 500.0));
    }

    #[test]
    fn fit_width_ignores_height_overflow() {
        let r = fit_rect(FitParams {
            image: v(1000.0, 2000.0),
            viewport: v(500.0, 500.0),
            fit: FitMode::FitWidth,
            zoom: 1.0,
            no_zoom_in: false,
        });
        assert_eq!(r, v(500.0, 1000.0));
    }

    #[test]
    fn no_zoom_in_caps_at_100() {
        let r = fit_rect(FitParams {
            image: v(100.0, 100.0),
            viewport: v(1000.0, 1000.0),
            fit: FitMode::Fit,
            zoom: 1.0,
            no_zoom_in: true,
        });
        assert_eq!(r, v(100.0, 100.0));
    }

    #[test]
    fn custom_scale_not_capped_by_no_zoom_in() {
        let r = fit_rect(FitParams {
            image: v(100.0, 100.0),
            viewport: v(1000.0, 1000.0),
            fit: FitMode::Custom,
            zoom: 2.5,
            no_zoom_in: true,
        });
        assert_eq!(r, v(250.0, 250.0));
    }

    #[test]
    fn zero_image_returns_zero() {
        let r = fit_rect(FitParams {
            image: v(0.0, 0.0),
            viewport: v(500.0, 500.0),
            fit: FitMode::Fit,
            zoom: 1.0,
            no_zoom_in: false,
        });
        assert_eq!(r, Vec2::ZERO);
    }
}
