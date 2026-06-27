//! Standard ClearType-style horizontal subpixel antialiasing.
//!
//! This is the reference subpixel path — it uses the display layout's
//! channel filter but applies **no** fringe suppression.  It is the
//! direct analog of what FreeType's `FT_RENDER_MODE_LCD` produces,
//! adapted to operate on an arbitrary [`SubpixelLayout`].
//!
//! Use this renderer to:
//! - Establish a baseline for subpixel sharpness
//! - Measure the colour fringing introduced by naive subpixel rendering
//! - Compare against [`super::oled_aware`] which adds fringe suppression

use crate::glyph::GlyphBuffer;
use crate::layout::SubpixelLayout;
use crate::profile::DisplayProfile;
use crate::subpixel::SubpixelGrid;

/// Render `buf` using standard subpixel antialiasing for `layout`.
///
/// No adaptive fringe suppression is applied — this is the "naive" subpixel
/// path that produces maximum sharpness but may exhibit colour fringing on
/// OLED panels with non-RGB-stripe geometries.
///
/// Output is EOTF-encoded and ready for the framebuffer.
pub fn render_subpixel(
    buf: &GlyphBuffer,
    layout: SubpixelLayout,
    profile: &DisplayProfile,
) -> SubpixelGrid {
    // If the layout doesn't benefit from subpixel rendering, fall back to grey.
    if !layout.subpixel_rendering_useful() || profile.is_hidpi() {
        return super::grayscale::render_grayscale(buf, profile);
    }

    // Build the grid through the layout filter, fringe suppression OFF.
    let linear_grid = SubpixelGrid::from_glyph(buf, layout, false);

    // EOTF-encode the linear grid.
    encode_grid(linear_grid, profile)
}

/// EOTF-encode every pixel in a linear-light [`SubpixelGrid`].
pub(crate) fn encode_grid(mut grid: SubpixelGrid, profile: &DisplayProfile) -> SubpixelGrid {
    for row in 0..grid.height {
        for col in 0..grid.width {
            let px = grid.pixel_mut(col, row);
            px.r = profile.eotf.encode(px.r);
            px.g = profile.eotf.encode(px.g);
            px.b = profile.eotf.encode(px.b);
        }
    }
    grid
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
                px[0] = cov;
                px[1] = cov;
                px[2] = cov;
                px[3] = cov;
            }
        }
        buf
    }

    #[test]
    fn hidpi_falls_back_to_greyscale() {
        let buf = uniform_buf(8, 4, 0.6);
        let mut profile = DisplayProfile::sdr_oled();
        profile.device_pixel_ratio = 2.0; // triggers HiDPI

        let grey_grid = super::super::grayscale::render_grayscale(&buf, &profile);
        let sp_grid = render_subpixel(&buf, SubpixelLayout::RgbStripe, &profile);

        // Both should produce identical output under HiDPI
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
        // WRGB layout marks subpixel rendering as not useful
        let buf = uniform_buf(8, 4, 0.5);
        let profile = DisplayProfile::sdr_oled();
        let grid = render_subpixel(&buf, SubpixelLayout::Wrgb, &profile);

        // All channels must be equal (greyscale fallback)
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
        // Create a hard step edge: first half black, second half white
        let mut buf = GlyphBuffer::new(8, 1);
        for col in 0..8 {
            let cov = if col < 4 { 0.0 } else { 1.0 };
            let px = buf.pixel_mut(col, 0);
            px[0] = cov;
            px[1] = cov;
            px[2] = cov;
            px[3] = cov;
        }
        let profile = DisplayProfile::sdr_oled();
        let grid = render_subpixel(&buf, SubpixelLayout::RgbStripe, &profile);

        // At the edge pixel (col=4), R/G/B should differ because
        // the phase-shifted taps sample different coverage values
        let edge = grid.pixel(4, 0);
        let all_equal = (edge.r - edge.g).abs() < 1e-4 && (edge.g - edge.b).abs() < 1e-4;
        assert!(!all_equal, "expected channel separation at edge, got equal channels");
    }
}
