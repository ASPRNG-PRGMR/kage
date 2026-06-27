//! Virtual subpixel grid.
//!
//! A [`SubpixelGrid`] is a 2-D array of `(R, G, B)` energy values, one entry
//! per *logical pixel*.  It is produced by mapping a [`GlyphBuffer`] through a
//! [`SubpixelLayout`], applying the layout's per-channel filter, and optionally
//! running adaptive fringe suppression.
//!
//! The grid is the intermediate representation between the rendered glyph and
//! the final display framebuffer.  It can be:
//! - Displayed directly (zoom + inspect)
//! - Passed to a display simulator (Phase 2)
//! - Encoded with the panel's EOTF and composited into a framebuffer

use crate::glyph::GlyphBuffer;
use crate::layout::{
    pentile::{adaptive_blend, chromatic_penalty, filter_row},
    SubpixelLayout,
};

// ── SubpixelGrid ──────────────────────────────────────────────────────────────

/// Per-pixel RGB energy on the virtual subpixel grid.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubpixelPixel {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

/// A 2-D grid of [`SubpixelPixel`] values produced by layout-aware filtering.
#[derive(Debug, Clone)]
pub struct SubpixelGrid {
    pub width: u32,
    pub height: u32,
    pixels: Vec<SubpixelPixel>,
}

impl SubpixelGrid {
    /// Allocate a zeroed grid.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![SubpixelPixel::default(); (width * height) as usize],
        }
    }

    #[inline]
    pub fn pixel(&self, col: u32, row: u32) -> SubpixelPixel {
        self.pixels[(row * self.width + col) as usize]
    }

    #[inline]
    pub fn pixel_mut(&mut self, col: u32, row: u32) -> &mut SubpixelPixel {
        &mut self.pixels[(row * self.width + col) as usize]
    }

    /// Build a [`SubpixelGrid`] from a rendered [`GlyphBuffer`] and a display layout.
    ///
    /// The alpha channel of the buffer is used as the coverage signal fed into
    /// the per-layout filter.  The filter produces per-channel R/G/B values
    /// which are then optionally blended with greyscale via the adaptive fringe
    /// suppressor.
    ///
    /// `fringe_suppress`: when `true`, the adaptive chromatic penalty is applied
    /// to reduce colour fringing at high-contrast edges.
    pub fn from_glyph(
        buf: &GlyphBuffer,
        layout: SubpixelLayout,
        fringe_suppress: bool,
    ) -> Self {
        let w = buf.width;
        let h = buf.height;
        let mut grid = SubpixelGrid::new(w, h);

        for row in 0..h {
            // Extract the coverage (alpha) row as a slice of f32.
            let coverage: Vec<f32> = (0..w)
                .map(|col| buf.pixel(col, row)[3])
                .collect();

            let mut out_r = vec![0.0f32; w as usize];
            let mut out_g = vec![0.0f32; w as usize];
            let mut out_b = vec![0.0f32; w as usize];

            match layout {
                SubpixelLayout::Greyscale | SubpixelLayout::Wrgb => {
                    // Greyscale: all channels equal coverage.
                    for col in 0..w as usize {
                        out_r[col] = coverage[col];
                        out_g[col] = coverage[col];
                        out_b[col] = coverage[col];
                    }
                }

                SubpixelLayout::RgbStripe => {
                    // Standard ClearType-style horizontal shift.
                    // R lags by 1/3 pixel, G is centred, B leads by 1/3 pixel.
                    // We approximate this with a 3-tap displacement FIR.
                    rgb_stripe_filter_row(&coverage, &mut out_r, &mut out_g, &mut out_b);
                }

                SubpixelLayout::BgrStripe => {
                    // Same as RGB but R and B are swapped.
                    rgb_stripe_filter_row(&coverage, &mut out_b, &mut out_g, &mut out_r);
                }

                SubpixelLayout::PentileRgbg => {
                    let row_parity = (row & 1) as u8;
                    filter_row(&coverage, row_parity, &mut out_r, &mut out_g, &mut out_b);

                    if fringe_suppress {
                        for col in 0..w as usize {
                            let penalty = chromatic_penalty(&coverage, col);
                            let grey = out_g[col]; // green carries luminance
                            let [r2, g2, b2] =
                                adaptive_blend(out_r[col], out_g[col], out_b[col], grey, penalty);
                            out_r[col] = r2;
                            out_g[col] = g2;
                            out_b[col] = b2;
                        }
                    }
                }

                SubpixelLayout::DeltaRgb => {
                    // Delta-RGB (QD-OLED) requires a 2-D kernel — not implemented yet.
                    // Fall back to greyscale for now; Phase 3 will add the full kernel.
                    for col in 0..w as usize {
                        out_r[col] = coverage[col];
                        out_g[col] = coverage[col];
                        out_b[col] = coverage[col];
                    }
                }
            }

            // Write results into grid.
            for col in 0..w {
                let px = grid.pixel_mut(col, row);
                px.r = out_r[col as usize];
                px.g = out_g[col as usize];
                px.b = out_b[col as usize];
            }
        }

        grid
    }

    /// Pack the grid into a `Vec<u32>` of ARGB8 values suitable for `minifb`.
    ///
    /// Each u32 is `0xFF_RR_GG_BB` in host byte order.
    /// Values are clamped to [0, 1] and assumed to already be EOTF-encoded
    /// (i.e. in signal domain).
    pub fn to_argb8_display(&self) -> Vec<u32> {
        self.pixels
            .iter()
            .map(|px| {
                let r = (px.r.clamp(0.0, 1.0) * 255.0).round() as u32;
                let g = (px.g.clamp(0.0, 1.0) * 255.0).round() as u32;
                let b = (px.b.clamp(0.0, 1.0) * 255.0).round() as u32;
                (0xFF << 24) | (r << 16) | (g << 8) | b
            })
            .collect()
    }
}

// ── RGB stripe filter ─────────────────────────────────────────────────────────

/// 3-tap horizontal displacement filter for RGB/BGR stripe panels.
///
/// Simulates ClearType-style phase-shifted sampling: each colour channel
/// samples the coverage waveform at a slightly different horizontal position,
/// corresponding to the physical subpixel offset within the logical pixel.
///
/// Taps approximate the subpixel offsets:
/// - Left channel (R for RGB):  weighted toward the left neighbour
/// - Centre channel (G):        centred
/// - Right channel (B for RGB): weighted toward the right neighbour
///
/// For BGR panels, the caller passes out_b and out_r in swapped argument positions.
fn rgb_stripe_filter_row(
    coverage: &[f32],
    out_left: &mut [f32],
    out_centre: &mut [f32],
    out_right: &mut [f32],
) {
    let n = coverage.len();
    if n < 3 {
        return;
    }
    for i in 1..n - 1 {
        let cl = coverage[i - 1];
        let cc = coverage[i];
        let cr = coverage[i + 1];
        // Left-phase tap (R): emphasises left neighbour
        out_left[i]   = 0.30 * cl + 0.60 * cc + 0.10 * cr;
        // Centre tap (G): symmetric gentle sharpening
        out_centre[i] = 0.11 * cl + 0.78 * cc + 0.11 * cr;
        // Right-phase tap (B): emphasises right neighbour
        out_right[i]  = 0.10 * cl + 0.60 * cc + 0.30 * cr;
    }
    // Border pixels: copy coverage unfiltered
    if n > 0 {
        out_left[0] = coverage[0];
        out_centre[0] = coverage[0];
        out_right[0] = coverage[0];
        let last = n - 1;
        out_left[last] = coverage[last];
        out_centre[last] = coverage[last];
        out_right[last] = coverage[last];
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph::GlyphBuffer;

    fn uniform_buffer(w: u32, h: u32, cov: f32) -> GlyphBuffer {
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
    fn greyscale_layout_uniform_input() {
        let buf = uniform_buffer(8, 4, 0.5);
        let grid = SubpixelGrid::from_glyph(&buf, SubpixelLayout::Greyscale, false);
        for row in 0..4 {
            for col in 0..8 {
                let px = grid.pixel(col, row);
                assert!((px.r - 0.5).abs() < 1e-5);
                assert!((px.g - 0.5).abs() < 1e-5);
                assert!((px.b - 0.5).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn rgb_stripe_uniform_passthrough() {
        let buf = uniform_buffer(8, 4, 0.7);
        let grid = SubpixelGrid::from_glyph(&buf, SubpixelLayout::RgbStripe, false);
        // Interior pixels should all be close to 0.7
        for row in 0..4 {
            for col in 1..7u32 {
                let px = grid.pixel(col, row);
                assert!((px.r - 0.7).abs() < 1e-5, "R at ({col},{row}) = {}", px.r);
                assert!((px.g - 0.7).abs() < 1e-5, "G at ({col},{row}) = {}", px.g);
                assert!((px.b - 0.7).abs() < 1e-5, "B at ({col},{row}) = {}", px.b);
            }
        }
    }

    #[test]
    fn to_argb8_white() {
        let buf = uniform_buffer(2, 2, 1.0);
        let grid = SubpixelGrid::from_glyph(&buf, SubpixelLayout::Greyscale, false);
        let argb = grid.to_argb8_display();
        for px in argb {
            assert_eq!(px, 0xFF_FF_FF_FF);
        }
    }

    #[test]
    fn to_argb8_black() {
        let buf = uniform_buffer(2, 2, 0.0);
        let grid = SubpixelGrid::from_glyph(&buf, SubpixelLayout::Greyscale, false);
        let argb = grid.to_argb8_display();
        for px in argb {
            assert_eq!(px, 0xFF_00_00_00);
        }
    }
}
