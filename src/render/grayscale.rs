//! Grayscale antialiasing renderer — the baseline path.
//!
//! All three colour channels receive the same coverage value.
//! No subpixel-level colour information is used.
//!
//! This path is the ground truth for comparison: it is immune to colour
//! fringing by construction, at the cost of horizontal sharpness.

use crate::glyph::GlyphBuffer;
use crate::profile::DisplayProfile;
use crate::subpixel::{SubpixelGrid, SubpixelPixel};

/// Render `buf` to a [`SubpixelGrid`] using pure greyscale antialiasing.
///
/// The alpha channel of `buf` is used as coverage.  EOTF encoding is applied
/// so the output is in the signal domain ready for the framebuffer.
pub fn render_grayscale(buf: &GlyphBuffer, profile: &DisplayProfile) -> SubpixelGrid {
    let mut grid = SubpixelGrid::new(buf.width, buf.height);

    for row in 0..buf.height {
        for col in 0..buf.width {
            let cov_linear = buf.pixel(col, row)[3]; // alpha = coverage in linear light
            // Encode to signal domain before writing to framebuffer-bound grid
            let cov_signal = profile.eotf.encode(cov_linear);

            *grid.pixel_mut(col, row) = SubpixelPixel {
                r: cov_signal,
                g: cov_signal,
                b: cov_signal,
            };
        }
    }

    grid
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{DisplayProfile, Eotf};

    fn flat_buf(w: u32, h: u32, cov: f32) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(w, h);
        for row in 0..h {
            for col in 0..w {
                let px = buf.pixel_mut(col, row);
                px[3] = cov; // alpha = coverage
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
    fn full_coverage_encodes_to_one() {
        let buf = flat_buf(2, 2, 1.0);
        // Use linear gamma so encode(1.0) == 1.0 exactly
        let mut profile = DisplayProfile::sdr_oled();
        profile.eotf = Eotf::Gamma(1.0);
        let grid = render_grayscale(&buf, &profile);
        let px = grid.pixel(0, 0);
        assert!((px.r - 1.0).abs() < 1e-5);
    }

    #[test]
    fn eotf_is_applied() {
        // sRGB encodes 0.5 linear to ~0.735 signal.
        // Greyscale renderer must apply this so the grid is in signal domain.
        let buf = flat_buf(1, 1, 0.5);
        let profile = DisplayProfile::sdr_oled(); // sRGB EOTF
        let grid = render_grayscale(&buf, &profile);
        let px = grid.pixel(0, 0);
        // sRGB encode(0.5) ≈ 0.7354
        assert!(
            (px.r - 0.7354).abs() < 0.01,
            "expected ~0.735, got {}",
            px.r
        );
    }
}
