//! OLED-aware renderer — each colour sampled where its emitter sits.
//!
//! Returns a **linear-light** [`SubpixelGrid`].  The caller applies
//! [`crate::render::encode_grid`] before writing to a framebuffer.
//!
//! The method is described in [`crate::render::perceptual`]. In short:
//! instead of filtering pixel values with a fixed kernel, every colour of
//! every pixel takes the mean of the glyph's supersampled coverage over a
//! footprint centred on that colour's emitter, with separate footprint
//! widths for luminance-carrying and colour-carrying emitters.
//!
//! The functions here are the convenience entry points that start from a
//! [`GlyphBuffer`], i.e. pixel-resolution coverage, where a shifted
//! footprint can only interpolate between neighbouring pixels. For
//! supersampled coverage use [`crate::render::render_coverage`].

use super::perceptual::{render_at_emitters, RenderParams};
use super::{samples_at_emitters, RenderMode};
use crate::coverage::Coverage;
use crate::glyph::GlyphBuffer;
use crate::layout::{EmitterMap, SubpixelLayout};
use crate::profile::DisplayProfile;
use crate::subpixel::SubpixelGrid;

/// Render `buf` with the OLED-aware renderer and the layout's tuned
/// settings ([`RenderParams::for_layout`]).
///
/// Output is in **linear light** — apply [`crate::render::encode_grid`]
/// before display.
pub fn render_oled_aware(
    buf: &GlyphBuffer,
    layout: SubpixelLayout,
    profile: &DisplayProfile,
) -> SubpixelGrid {
    render_oled_aware_with(buf, layout, profile, &RenderParams::for_layout(layout))
}

/// Render `buf` with the OLED-aware renderer and explicit settings, using
/// the layout's nominal emitter positions.
pub fn render_oled_aware_with(
    buf: &GlyphBuffer,
    layout: SubpixelLayout,
    profile: &DisplayProfile,
    params: &RenderParams,
) -> SubpixelGrid {
    if !samples_at_emitters(RenderMode::OledAware, profile) {
        return super::grayscale::render_grayscale(buf, profile);
    }
    render_at_emitters(&Coverage::from_glyph(buf), &EmitterMap::for_layout(layout), params)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph::GlyphBuffer;
    use crate::profile::DisplayProfile;

    fn step_edge_buf(width: u32) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(width, 1);
        let mid = width / 2;
        for col in 0..width {
            let cov = if col < mid { 0.0 } else { 1.0 };
            buf.pixel_mut(col, 0).fill(cov);
        }
        buf
    }

    fn uniform_buf(w: u32, h: u32, cov: f32) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(w, h);
        buf.pixels.fill(cov);
        buf
    }

    #[test]
    fn output_is_linear_not_encoded() {
        let buf = uniform_buf(8, 2, 0.5);
        let profile = DisplayProfile::sdr_oled();
        for layout in [SubpixelLayout::PentileRgbg, SubpixelLayout::DeltaRgb, SubpixelLayout::Wrgb] {
            let grid = render_oled_aware(&buf, layout, &profile);
            for col in 0..8u32 {
                let px = grid.pixel(col, 0);
                assert!((px.g - 0.5).abs() < 1e-4, "{layout:?}: expected linear 0.5, got {} at col {col}", px.g);
            }
        }
    }

    #[test]
    fn colours_separate_at_an_edge_on_every_colour_layout() {
        // That separation is the point: each emitter shows what is in
        // front of it, and they are in different places.
        let buf = step_edge_buf(16);
        let profile = DisplayProfile::sdr_oled();
        for layout in [SubpixelLayout::RgbStripe, SubpixelLayout::BgrStripe, SubpixelLayout::PentileRgbg, SubpixelLayout::DeltaRgb] {
            let grid = render_oled_aware_with(&buf, layout, &profile, &RenderParams::default());
            let spread = (7..=8).map(|c| { let p = grid.pixel(c, 0); (p.r - p.g).abs().max((p.b - p.g).abs()) }).fold(0.0f32, f32::max);
            assert!(spread > 0.05, "{layout:?}: spread {spread}");
        }
    }

    #[test]
    fn stripe_orders_mirror_each_other() {
        let buf = step_edge_buf(16);
        let profile = DisplayProfile::sdr_oled();
        let rgb = render_oled_aware_with(&buf, SubpixelLayout::RgbStripe, &profile, &RenderParams::default());
        let bgr = render_oled_aware_with(&buf, SubpixelLayout::BgrStripe, &profile, &RenderParams::default());
        for col in 0..16 {
            let (a, b) = (rgb.pixel(col, 0), bgr.pixel(col, 0));
            assert!((a.r - b.b).abs() < 1e-6 && (a.b - b.r).abs() < 1e-6 && (a.g - b.g).abs() < 1e-6);
        }
    }

    #[test]
    fn wrgb_is_achromatic_but_not_plain_greyscale() {
        let buf = step_edge_buf(16);
        let profile = DisplayProfile::sdr_oled();
        let oled = render_oled_aware_with(&buf, SubpixelLayout::Wrgb, &profile, &RenderParams::default());
        for col in 0..16 {
            let p = oled.pixel(col, 0);
            assert!((p.r - p.g).abs() < 1e-6 && (p.g - p.b).abs() < 1e-6);
        }
        // The first lit pixel is sampled at the white emitter, an eighth of
        // a pixel left of centre.
        assert!((oled.pixel(8, 0).g - 0.875).abs() < 1e-6);
    }

    #[test]
    fn hidpi_falls_back_to_greyscale() {
        let buf = step_edge_buf(16);
        let mut profile = DisplayProfile::sdr_oled();
        profile.device_pixel_ratio = 2.0;
        let grid = render_oled_aware(&buf, SubpixelLayout::PentileRgbg, &profile);
        for col in 0..16 {
            let px = grid.pixel(col, 0);
            assert!((px.r - px.g).abs() < 1e-5 && (px.g - px.b).abs() < 1e-5, "HiDPI should produce greyscale at col {col}");
        }
    }
}
