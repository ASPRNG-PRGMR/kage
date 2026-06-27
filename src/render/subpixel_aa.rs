//! Standard ClearType-style horizontal subpixel antialiasing.
//!
//! Returns a **linear-light** [`SubpixelGrid`].  The caller applies
//! [`crate::render::encode_grid`] before writing to a framebuffer.

use crate::glyph::GlyphBuffer;
use crate::layout::SubpixelLayout;
use crate::profile::DisplayProfile;
use crate::subpixel::SubpixelGrid;

/// Render `buf` using standard subpixel antialiasing for `layout`.
///
/// No fringe suppression is applied.  Output is in **linear light**.
pub fn render_subpixel(
    buf: &GlyphBuffer,
    layout: SubpixelLayout,
    profile: &DisplayProfile,
) -> SubpixelGrid {
    if !layout.subpixel_rendering_useful() || profile.is_hidpi() {
        return super::grayscale::render_grayscale(buf, profile);
    }

    // Layout filter applied, fringe suppression OFF. Returns linear light.
    SubpixelGrid::from_glyph(buf, layout, false)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph::GlyphBuffer;
    use crate::profile::DisplayProfile;

    fn uniform_buf(w: u32, h: u32, cov: f32) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(w, h);
        for row in 0..h {
            for col in 0..w {
                let px = buf.pixel_mut(col, row);
                px[0] = cov; px[1] = cov; px[2] = cov; px[3] = cov;
            }
        }
        buf
    }

    #[test]
    fn output_is_linear_not_encoded() {
        // Uniform 0.5 coverage → all channels should be 0.5 linear, not ~0.735 sRGB.
        let buf = uniform_buf(8, 4, 0.5);
        let profile = DisplayProfile::sdr_oled();
        let grid = render_subpixel(&buf, SubpixelLayout::RgbStripe, &profile);
        for col in 1..7u32 {
            let px = grid.pixel(col, 0);
            assert!(
                (px.g - 0.5).abs() < 1e-4,
                "expected linear 0.5, got {} at col {col}",
                px.g
            );
        }
    }

    #[test]
    fn hidpi_falls_back_to_greyscale() {
        let buf = uniform_buf(8, 4, 0.6);
        let mut profile = DisplayProfile::sdr_oled();
        profile.device_pixel_ratio = 2.0;

        let grey_grid = super::super::grayscale::render_grayscale(&buf, &profile);
        let sp_grid   = render_subpixel(&buf, SubpixelLayout::RgbStripe, &profile);

        for row in 0..4 {
            for col in 1..7u32 {
                let g = grey_grid.pixel(col, row);
                let s = sp_grid.pixel(col, row);
                assert!(
                    (g.r - s.r).abs() < 1e-5,
                    "HiDPI fallback mismatch at ({col},{row})"
                );
            }
        }
    }

    #[test]
    fn wrgb_falls_back_to_greyscale() {
        let buf = uniform_buf(8, 4, 0.5);
        let profile = DisplayProfile::sdr_oled();
        let grid = render_subpixel(&buf, SubpixelLayout::Wrgb, &profile);
        for row in 0..4 {
            for col in 0..8 {
                let px = grid.pixel(col, row);
                assert!((px.r - px.g).abs() < 1e-5);
                assert!((px.g - px.b).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn rgb_stripe_channels_differ_on_edge() {
        let mut buf = GlyphBuffer::new(8, 1);
        for col in 0..8 {
            let cov = if col < 4 { 0.0 } else { 1.0 };
            let px = buf.pixel_mut(col, 0);
            px[0] = cov; px[1] = cov; px[2] = cov; px[3] = cov;
        }
        let profile = DisplayProfile::sdr_oled();
        let grid = render_subpixel(&buf, SubpixelLayout::RgbStripe, &profile);
        let edge = grid.pixel(4, 0);
        let all_equal = (edge.r - edge.g).abs() < 1e-4 && (edge.g - edge.b).abs() < 1e-4;
        assert!(!all_equal, "expected channel separation at edge");
    }
}
