//! Image filter pipeline: rotate, clip, brightness/contrast/gamma, sharpen,
//! resize. Pure-CPU, `DynamicImage` in → `DynamicImage` out. Composable as a
//! serializable ordered [`Pipeline`] so filter state can be persisted per
//! book.

use image::{imageops, DynamicImage, GenericImageView, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

/// A single filter op. All variants are cheap to clone and `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FilterOp {
    Rotate(Rotation),
    Clip(ClipRect),
    Adjust(AdjustParams),
    Sharpen(SharpenParams),
    Resize(ResizeParams),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    /// Original orientation.
    Deg0,
    /// Clockwise 90°.
    Deg90,
    Deg180,
    /// Clockwise 270° (counter-clockwise 90°).
    Deg270,
}

impl Rotation {
    pub fn next_cw(self) -> Self {
        match self {
            Rotation::Deg0 => Rotation::Deg90,
            Rotation::Deg90 => Rotation::Deg180,
            Rotation::Deg180 => Rotation::Deg270,
            Rotation::Deg270 => Rotation::Deg0,
        }
    }

    pub fn next_ccw(self) -> Self {
        match self {
            Rotation::Deg0 => Rotation::Deg270,
            Rotation::Deg90 => Rotation::Deg0,
            Rotation::Deg180 => Rotation::Deg90,
            Rotation::Deg270 => Rotation::Deg180,
        }
    }
}

/// A crop rect in normalized [0, 1] image coordinates. `(x, y)` is the top
/// left corner and `(w, h)` the size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl ClipRect {
    pub fn full() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }

    pub fn clamp(self) -> Self {
        let x = self.x.clamp(0.0, 1.0);
        let y = self.y.clamp(0.0, 1.0);
        let w = self.w.clamp(0.0, 1.0 - x);
        let h = self.h.clamp(0.0, 1.0 - y);
        Self { x, y, w, h }
    }

    pub fn is_identity(&self) -> bool {
        self.x <= f32::EPSILON
            && self.y <= f32::EPSILON
            && (self.w - 1.0).abs() <= f32::EPSILON
            && (self.h - 1.0).abs() <= f32::EPSILON
    }
}

/// Brightness / contrast / gamma. Range conventions chosen to be intuitive:
/// brightness is `-1.0..=1.0` (−1 = black, 0 = no change, +1 = white),
/// contrast is `0.0..=2.0` (1 = no change), gamma is `0.1..=3.0` (1 = no
/// change, <1 brightens midtones, >1 darkens).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdjustParams {
    pub brightness: f32,
    pub contrast: f32,
    pub gamma: f32,
}

impl AdjustParams {
    pub fn identity() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            gamma: 1.0,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.brightness.abs() < 1e-4
            && (self.contrast - 1.0).abs() < 1e-4
            && (self.gamma - 1.0).abs() < 1e-4
    }
}

/// Unsharp-mask sharpen. `amount` scales the unsharp contribution added back
/// in; `radius` is the blur sigma used for the mask; `threshold` suppresses
/// small differences to avoid noise amplification.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SharpenParams {
    pub amount: f32,
    pub radius: f32,
    pub threshold: i32,
}

impl SharpenParams {
    pub fn identity() -> Self {
        Self {
            amount: 0.0,
            radius: 1.0,
            threshold: 0,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.amount.abs() < 1e-4
    }
}

/// Resize filter algorithm (maps 1:1 to `image::imageops::FilterType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizeAlgo {
    Nearest,
    Triangle,
    CatmullRom,
    Gaussian,
    Lanczos3,
}

impl ResizeAlgo {
    pub fn to_image_filter(self) -> imageops::FilterType {
        match self {
            ResizeAlgo::Nearest => imageops::FilterType::Nearest,
            ResizeAlgo::Triangle => imageops::FilterType::Triangle,
            ResizeAlgo::CatmullRom => imageops::FilterType::CatmullRom,
            ResizeAlgo::Gaussian => imageops::FilterType::Gaussian,
            ResizeAlgo::Lanczos3 => imageops::FilterType::Lanczos3,
        }
    }
}

/// Target size for the resize filter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ResizeTarget {
    /// Scale so the longer edge equals this many pixels, preserving aspect.
    LongestEdge(u32),
    /// Scale by a factor of the source size.
    Scale(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ResizeParams {
    pub algo: ResizeAlgo,
    pub target: ResizeTarget,
}

impl FilterOp {
    pub fn apply(&self, img: DynamicImage) -> DynamicImage {
        match self {
            FilterOp::Rotate(r) => apply_rotate(img, *r),
            FilterOp::Clip(c) => apply_clip(img, *c),
            FilterOp::Adjust(a) => apply_adjust(img, *a),
            FilterOp::Sharpen(s) => apply_sharpen(img, *s),
            FilterOp::Resize(r) => apply_resize(img, *r),
        }
    }

    /// Cheap check — skip costly operations when they'd no-op anyway.
    pub fn is_identity(&self) -> bool {
        match self {
            FilterOp::Rotate(Rotation::Deg0) => true,
            FilterOp::Clip(c) => c.is_identity(),
            FilterOp::Adjust(a) => a.is_identity(),
            FilterOp::Sharpen(s) => s.is_identity(),
            FilterOp::Resize(_) | FilterOp::Rotate(_) => false,
        }
    }
}

fn apply_rotate(img: DynamicImage, r: Rotation) -> DynamicImage {
    match r {
        Rotation::Deg0 => img,
        Rotation::Deg90 => img.rotate90(),
        Rotation::Deg180 => img.rotate180(),
        Rotation::Deg270 => img.rotate270(),
    }
}

fn apply_clip(img: DynamicImage, rect: ClipRect) -> DynamicImage {
    let rect = rect.clamp();
    if rect.is_identity() {
        return img;
    }
    let (w, h) = img.dimensions();
    let x = (rect.x * w as f32).round() as u32;
    let y = (rect.y * h as f32).round() as u32;
    let cw = ((rect.w * w as f32).round() as u32)
        .max(1)
        .min(w - x.min(w - 1));
    let ch = ((rect.h * h as f32).round() as u32)
        .max(1)
        .min(h - y.min(h - 1));
    img.crop_imm(x, y, cw, ch)
}

fn apply_adjust(img: DynamicImage, a: AdjustParams) -> DynamicImage {
    if a.is_identity() {
        return img;
    }
    let mut rgba: RgbaImage = img.to_rgba8();
    let brightness = a.brightness.clamp(-1.0, 1.0);
    let contrast = a.contrast.clamp(0.0, 4.0);
    let gamma = a.gamma.clamp(0.05, 8.0);
    let inv_gamma = 1.0 / gamma;
    let add = brightness * 255.0;

    for px in rgba.pixels_mut() {
        let Rgba(chan) = *px;
        let mut out = [chan[0], chan[1], chan[2], chan[3]];
        for v in out.iter_mut().take(3) {
            let f = *v as f32;
            // Brightness first (linear shift), then contrast around 128,
            // then gamma.
            let mut x = f + add;
            x = (x - 128.0) * contrast + 128.0;
            x = x.clamp(0.0, 255.0) / 255.0;
            x = x.powf(inv_gamma);
            *v = (x * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        *px = Rgba(out);
    }
    DynamicImage::ImageRgba8(rgba)
}

fn apply_sharpen(img: DynamicImage, s: SharpenParams) -> DynamicImage {
    if s.is_identity() {
        return img;
    }
    // `unsharpen` always blends at amount=1. To expose "amount" we build a
    // lerp between the original and the unsharpened result.
    let sharpened = img.unsharpen(s.radius.max(0.1), s.threshold);
    let amount = s.amount.clamp(0.0, 4.0);
    if (amount - 1.0).abs() < 1e-4 {
        return sharpened;
    }
    let src = img.to_rgba8();
    let sharp = sharpened.to_rgba8();
    let (w, h) = src.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (dst_px, (a, b)) in out.pixels_mut().zip(src.pixels().zip(sharp.pixels())) {
        let mut chan = [0u8; 4];
        for (i, c) in chan.iter_mut().enumerate().take(3) {
            let blended = a.0[i] as f32 + (b.0[i] as f32 - a.0[i] as f32) * amount;
            *c = blended.round().clamp(0.0, 255.0) as u8;
        }
        chan[3] = a.0[3];
        *dst_px = Rgba(chan);
    }
    DynamicImage::ImageRgba8(out)
}

fn apply_resize(img: DynamicImage, params: ResizeParams) -> DynamicImage {
    let (w, h) = img.dimensions();
    let (tw, th) = match params.target {
        ResizeTarget::LongestEdge(edge) => {
            let scale = (edge as f32 / w.max(h) as f32).min(1.0);
            if scale >= 1.0 - f32::EPSILON {
                return img;
            }
            (
                ((w as f32) * scale).round().max(1.0) as u32,
                ((h as f32) * scale).round().max(1.0) as u32,
            )
        }
        ResizeTarget::Scale(s) => {
            if (s - 1.0).abs() < 1e-4 {
                return img;
            }
            (
                ((w as f32) * s).round().max(1.0) as u32,
                ((h as f32) * s).round().max(1.0) as u32,
            )
        }
    };
    img.resize_exact(tw, th, params.algo.to_image_filter())
}

/// Ordered composition of filter ops. Apply runs in `ops` order — resize
/// last is usually what you want so the previous stages see full
/// resolution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pipeline {
    pub ops: Vec<FilterOp>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn push(&mut self, op: FilterOp) {
        self.ops.push(op);
    }

    pub fn apply(&self, img: DynamicImage) -> DynamicImage {
        self.ops
            .iter()
            .filter(|op| !op.is_identity())
            .fold(img, |acc, op| op.apply(acc))
    }

    pub fn is_identity(&self) -> bool {
        self.ops.iter().all(|op| op.is_identity())
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn sample_rgb(w: u32, h: u32) -> DynamicImage {
        let buf: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8])
        });
        DynamicImage::ImageRgb8(buf)
    }

    fn assert_dims(img: &DynamicImage, w: u32, h: u32) {
        let (iw, ih) = img.dimensions();
        assert_eq!((iw, ih), (w, h));
    }

    #[test]
    fn rotate_360_is_identity_dims() {
        let img = sample_rgb(40, 60);
        let out =
            FilterOp::Rotate(Rotation::Deg90).apply(FilterOp::Rotate(Rotation::Deg270).apply(img));
        assert_dims(&out, 40, 60);
    }

    #[test]
    fn rotate_90_swaps_dims() {
        let img = sample_rgb(40, 60);
        let out = FilterOp::Rotate(Rotation::Deg90).apply(img);
        assert_dims(&out, 60, 40);
    }

    #[test]
    fn clip_clamps_out_of_range() {
        let c = ClipRect {
            x: -0.5,
            y: 1.5,
            w: 2.0,
            h: 3.0,
        }
        .clamp();
        assert!(c.x >= 0.0 && c.y <= 1.0);
        assert!(c.x + c.w <= 1.0 + 1e-4);
        assert!(c.y + c.h <= 1.0 + 1e-4);
    }

    #[test]
    fn clip_identity_skips() {
        let img = sample_rgb(100, 200);
        let out = FilterOp::Clip(ClipRect::full()).apply(img);
        assert_dims(&out, 100, 200);
    }

    #[test]
    fn clip_half_produces_half_size() {
        let img = sample_rgb(100, 200);
        let out = FilterOp::Clip(ClipRect {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        })
        .apply(img);
        assert_dims(&out, 50, 200);
    }

    #[test]
    fn adjust_identity_preserves_bytes() {
        let img = sample_rgb(10, 10);
        let before = img.to_rgba8().into_raw();
        let out = FilterOp::Adjust(AdjustParams::identity()).apply(img);
        assert_eq!(out.to_rgba8().into_raw(), before);
    }

    #[test]
    fn adjust_full_brightness_clamps_to_white() {
        let img = sample_rgb(8, 8);
        let out = FilterOp::Adjust(AdjustParams {
            brightness: 1.0,
            contrast: 1.0,
            gamma: 1.0,
        })
        .apply(img);
        for px in out.to_rgba8().pixels() {
            assert_eq!(px.0[0], 255);
            assert_eq!(px.0[1], 255);
            assert_eq!(px.0[2], 255);
        }
    }

    #[test]
    fn resize_longest_edge_fits() {
        let img = sample_rgb(200, 100);
        let out = FilterOp::Resize(ResizeParams {
            algo: ResizeAlgo::Triangle,
            target: ResizeTarget::LongestEdge(50),
        })
        .apply(img);
        let (w, h) = out.dimensions();
        assert_eq!(w, 50);
        assert_eq!(h, 25);
    }

    #[test]
    fn resize_longest_edge_upscale_is_noop() {
        // We cap scale at 1.0 so LongestEdge never upscales — it's a "fit".
        let img = sample_rgb(20, 10);
        let out = FilterOp::Resize(ResizeParams {
            algo: ResizeAlgo::Triangle,
            target: ResizeTarget::LongestEdge(200),
        })
        .apply(img);
        assert_dims(&out, 20, 10);
    }

    #[test]
    fn sharpen_zero_amount_is_identity() {
        let img = sample_rgb(16, 16);
        let before = img.to_rgba8().into_raw();
        let out = FilterOp::Sharpen(SharpenParams::identity()).apply(img);
        assert_eq!(out.to_rgba8().into_raw(), before);
    }

    #[test]
    fn pipeline_90_then_270_preserves_dims() {
        let mut p = Pipeline::new();
        p.push(FilterOp::Rotate(Rotation::Deg90));
        p.push(FilterOp::Rotate(Rotation::Deg270));
        let out = p.apply(sample_rgb(30, 80));
        assert_dims(&out, 30, 80);
    }

    #[test]
    fn pipeline_identity_when_all_noop() {
        let mut p = Pipeline::new();
        p.push(FilterOp::Rotate(Rotation::Deg0));
        p.push(FilterOp::Adjust(AdjustParams::identity()));
        assert!(p.is_identity());
    }

    #[test]
    fn rotation_cycles_are_inverse() {
        let r = Rotation::Deg90;
        assert_eq!(r.next_cw().next_ccw(), Rotation::Deg90);
        assert_eq!(r.next_ccw().next_cw(), Rotation::Deg90);
    }

    #[test]
    fn filterop_serde_roundtrip() {
        let op = FilterOp::Resize(ResizeParams {
            algo: ResizeAlgo::Lanczos3,
            target: ResizeTarget::Scale(0.75),
        });
        let json = serde_json::to_string(&op).expect("serde_json");
        let back: FilterOp = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(op, back);
    }
}
