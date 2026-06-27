//! OLED-aware renderer — layout-matched FIR + adaptive fringe suppression.
//!
//! This is the novel rendering path that distinguishes KAGE from standard
//! subpixel rendering.  It combines:
//!
//! 1. **Layout-aware FIR filtering** — kernel taps tuned to the actual
//!    subpixel geometry of the target panel (not just RGB stripe).
//!
//! 2. **Adaptive fringe suppression** — at high-contrast edges, where
//!    chromatic artefacts are perceptually most damaging, the renderer
//!    blends the subpixel result toward a luminance-preserving greyscale
//!    fallback.  In smooth interior regions it retains full subpixel sharpness.
//!
//! 3. **Display profile awareness** — HiDPI panels and WRGB layouts
//!    automatically fall back to greyscale, where subpixel rendering would
//!    add colour penalty without sharpness gain.
//!
//! # Fringe suppression strategy
//!
//! The chromatic penalty is the normalised discrete Laplacian of the coverage
//! signal (see [`crate::layout::pentile::chromatic_penalty`]).  It peaks at
//! hard step edges and approaches zero in smooth regions.
//!
//! At each pixel:
//! ```text
//! out = lerp(subpixel_rgb, greyscale, penalty)
//! ```
//!
//! This gives continuous control: smooth strokes get full subpixel sharpness,
//! hard edges collapse toward colour-neutral greyscale.

use crate::glyph::GlyphBuffer;
use crate::layout::SubpixelLayout;
use crate::profile::DisplayProfile;
use crate::subpixel::SubpixelGrid;

use super::subpixel_aa::encode_grid;

/// Render `buf` using OLED-aware subpixel antialiasing.
///
/// Fringe suppression is always enabled on this path.  For layouts that
/// don't benefit from subpixel rendering (WRGB, Greyscale, HiDPI), the
/// function transparently falls back to [`super::grayscale::render_grayscale`].
pub fn render_oled_aware(
    buf: &GlyphBuffer,
    layout: SubpixelLayout,
    profile: &DisplayProfile,
) -> SubpixelGrid {
    if !layout.subpixel_rendering_useful() || profile.is_hidpi() {
        return super::grayscale::render_grayscale(buf, profile);
    }

    // Build the linear-light grid with fringe suppression enabled.
    let linear_grid = SubpixelGrid::from_glyph(buf, layout, true);

    // EOTF-encode into signal domain.
    encode_grid(linear_grid, profile)
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
            let px = buf.pixel_mut(col, 0);
            px[0] = cov;
            px[1] = cov;
            px[2] = cov;
            px[3] = cov;
        }
        buf
    }

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

    /// On a hard step edge, OLED-aware output should have LESS channel spread
    /// than naive subpixel AA (fringe suppression reduces R/B deviation from G).
    #[test]
    fn fringe_suppression_reduces_channel_spread_at_edge() {
        let buf = step_edge_buf(16);
        let profile = DisplayProfile::sdr_oled();

        let oled = render_oled_aware(&buf, SubpixelLayout::PentileRgbg, &profile);
        let naive = super::super::subpixel_aa::render_subpixel(
            &buf,
            SubpixelLayout::PentileRgbg,
            &profile,
        );

        // Measure max channel spread (max(|R-G|, |B-G|)) at the edge pixel
        let edge_col = 8u32; // first white pixel
        let o = oled.pixel(edge_col, 0);
        let n = naive.pixel(edge_col, 0);

        let oled_spread = (o.r - o.g).abs().max((o.b - o.g).abs());
        let naive_spread = (n.r - n.g).abs().max((n.b - n.g).abs());

        assert!(
            oled_spread <= naive_spread + 1e-5,
            "OLED-aware spread {oled_spread:.4} should be ≤ naive spread {naive_spread:.4}"
        );
    }

    /// In a smooth uniform region, OLED-aware and naive should produce
    /// nearly identical output (zero penalty → no suppression).
    #[test]
    fn no_suppression_in_smooth_region() {
        let buf = uniform_buf(8, 1, 0.6);
        let profile = DisplayProfile::sdr_oled();

        let oled = render_oled_aware(&buf, SubpixelLayout::PentileRgbg, &profile);
        let naive = super::super::subpixel_aa::render_subpixel(
            &buf,
            SubpixelLayout::PentileRgbg,
            &profile,
        );

        // Interior pixels should be nearly identical
        for col in 1..7u32 {
            let o = oled.pixel(col, 0);
            let n = naive.pixel(col, 0);
            assert!(
                (o.r - n.r).abs() < 1e-4 && (o.g - n.g).abs() < 1e-4 && (o.b - n.b).abs() < 1e-4,
                "smooth region mismatch at col {col}: oled=({:.4},{:.4},{:.4}) naive=({:.4},{:.4},{:.4})",
                o.r, o.g, o.b, n.r, n.g, n.b
            );
        }
    }

    #[test]
    fn hidpi_falls_back_to_greyscale() {
        let buf = uniform_buf(8, 4, 0.5);
        let mut profile = DisplayProfile::sdr_oled();
        profile.device_pixel_ratio = 2.0;

        let grid = render_oled_aware(&buf, SubpixelLayout::PentileRgbg, &profile);
        for row in 0..4 {
            for col in 0..8 {
                let px = grid.pixel(col, row);
                assert!(
                    (px.r - px.g).abs() < 1e-5 && (px.g - px.b).abs() < 1e-5,
                    "HiDPI should produce greyscale at ({col},{row})"
                );
            }
        }
    }
}
