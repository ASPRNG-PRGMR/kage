//! Greyscale antialiasing renderer — the baseline path.
//!
//! All three colour channels receive the same coverage value.
//! No subpixel-level colour information is used.
//!
//! Returns a **linear-light** [`SubpixelGrid`].  The caller applies
//! [`crate::render::encode_grid`] before writing to a framebuffer.

use crate::glyph::GlyphBuffer;
use crate::profile::DisplayProfile;
use crate::subpixel::{SubpixelGrid, SubpixelPixel};

/// Render `buf` to a linear-light [`SubpixelGrid`] using greyscale antialiasing.
///
/// The alpha channel of `buf` is used as coverage.  All three colour channels
/// are set to the same linear-light coverage value — no EOTF encoding is applied.
pub fn render_grayscale(buf: &GlyphBuffer, profile: &DisplayProfile) -> SubpixelGrid {
    // `profile` is used only for the HiDPI check; greyscale is always the
    // correct fallback regardless of layout, so we accept it here for API
    // symmetry with the other renderers.
    let _ = profile; // intentionally unused beyond the call site fallback

    let mut grid = SubpixelGrid::new(buf.width, buf.height);

    for row in 0..buf.height {
        for col in 0..buf.width {
            let cov = buf.pixel(col, row)[3]; // alpha = linear-light coverage
            *grid.pixel_mut(col, row) = SubpixelPixel {
                r: cov,
                g: cov,
                b: cov,
            };
        }
    }

    grid
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::DisplayProfile;

    fn flat_buf(w: u32, h: u32, cov: f32) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(w, h);
        for row in 0..h {
            for col in 0..w {
                buf.pixel_mut(col, row)[3] = cov;
            }
        }
        buf
    }

    #[test]
    fn all_channels_equal() {
        let buf = flat_buf(4, 4, 0.5);
        let profile = DisplayProfile::sdr_oled();
        let grid = render_grayscale(&buf, &profile);
        for row in 0..4 {
            for col in 0..4 {
                let px = grid.pixel(col, row);
                assert!((px.r - px.g).abs() < 1e-6, "R != G at ({col},{row})");
                assert!((px.g - px.b).abs() < 1e-6, "G != B at ({col},{row})");
            }
        }
    }

    #[test]
    fn zero_coverage_is_black() {
        let buf = flat_buf(2, 2, 0.0);
        let profile = DisplayProfile::sdr_oled();
        let grid = render_grayscale(&buf, &profile);
        let px = grid.pixel(0, 0);
        assert!(px.r < 1e-6 && px.g < 1e-6 && px.b < 1e-6);
    }

    #[test]
    fn output_is_linear_not_encoded() {
        // 0.5 coverage should come out as 0.5 linear — NOT sRGB encoded (~0.735).
        let buf = flat_buf(1, 1, 0.5);
        let profile = DisplayProfile::sdr_oled();
        let grid = render_grayscale(&buf, &profile);
        let px = grid.pixel(0, 0);
        assert!(
            (px.r - 0.5).abs() < 1e-5,
            "renderer should return linear light, not sRGB signal. got r={}",
            px.r
        );
    }

    #[test]
    fn full_coverage_is_one_linear() {
        let buf = flat_buf(2, 2, 1.0);
        let profile = DisplayProfile::sdr_oled();
        let grid = render_grayscale(&buf, &profile);
        let px = grid.pixel(0, 0);
        assert!((px.r - 1.0).abs() < 1e-5);
    }
}
