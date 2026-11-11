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
//! - Passed to the display simulator ([`crate::simulate`])
//! - Encoded with the panel's EOTF and composited into a framebuffer

use crate::glyph::GlyphBuffer;
use crate::layout::{
    pentile::{adaptive_blend, chromatic_penalty},
    BaseFilter, FilterKernel, SubpixelLayout,
};
use crate::plane::Plane;

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

    /// Build a grid from interleaved `[r, g, b, _]` values, four per pixel
    /// (the format the GPU backends read back).
    pub fn from_rgba(width: u32, height: u32, rgba: &[f32]) -> Self {
        assert_eq!(rgba.len(), (width * height * 4) as usize, "rgba length must be width*height*4");
        Self {
            width,
            height,
            pixels: rgba
                .chunks_exact(4)
                .map(|px| SubpixelPixel { r: px[0], g: px[1], b: px[2] })
                .collect(),
        }
    }

    /// Apply a layout filter to a coverage plane.
    ///
    /// Each channel samples the coverage through `filter`'s kernel, which
    /// places the sampling point at that channel's emitter. Taps outside the
    /// plane clamp to the edge, so glyphs of any width are filtered.
    pub fn from_coverage(cov: &Plane, filter: BaseFilter) -> Self {
        Self::from_kernel(cov, &filter.kernel())
    }

    /// Apply explicit filter taps to a coverage plane.
    pub fn from_kernel(cov: &Plane, kernel: &FilterKernel) -> Self {
        let mut grid = SubpixelGrid::new(cov.width, cov.height);
        for row in 0..cov.height {
            for col in 0..cov.width {
                let [r, g, b] = kernel.sample(cov, col, row);
                *grid.pixel_mut(col, row) = SubpixelPixel { r, g, b };
            }
        }
        grid
    }

    /// Build a [`SubpixelGrid`] from a rendered [`GlyphBuffer`] and a display layout.
    ///
    /// The alpha channel of the buffer is the coverage signal fed into the
    /// layout's filter ([`BaseFilter::for_layout`]).
    ///
    /// `fringe_suppress` applies the Phase 1 fringe suppressor — a blend
    /// towards green driven by the row Laplacian — on Pentile RGBG. The
    /// OLED-aware renderer no longer uses it; see
    /// [`crate::render::perceptual`] for its replacement, which works on
    /// every layout.
    pub fn from_glyph(
        buf: &GlyphBuffer,
        layout: SubpixelLayout,
        fringe_suppress: bool,
    ) -> Self {
        let cov = Plane::from_coverage(buf);
        let mut grid = Self::from_coverage(&cov, BaseFilter::for_layout(layout));

        if fringe_suppress && layout == SubpixelLayout::PentileRgbg {
            let w = cov.width as usize;
            for row in 0..cov.height {
                let coverage = &cov.data[row as usize * w..(row as usize + 1) * w];
                for col in 0..w {
                    let penalty = chromatic_penalty(coverage, col);
                    let px = grid.pixel_mut(col as u32, row);
                    // Green carries luminance.
                    let [r, g, b] = adaptive_blend(px.r, px.g, px.b, px.g, penalty);
                    *px = SubpixelPixel { r, g, b };
                }
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
    fn delta_layout_is_no_longer_greyscale() {
        // A hard horizontal edge: the delta kernel separates green from
        // red and blue across it.
        let mut buf = GlyphBuffer::new(4, 6);
        for row in 3..6 {
            for col in 0..4 {
                buf.pixel_mut(col, row)[3] = 1.0;
            }
        }
        let grid = SubpixelGrid::from_glyph(&buf, SubpixelLayout::DeltaRgb, false);
        let px = grid.pixel(1, 2);
        assert!((px.r - px.g).abs() > 0.1, "expected colour separation, got {px:?}");
    }

    #[test]
    fn from_rgba_drops_the_fourth_component() {
        let grid = SubpixelGrid::from_rgba(2, 1, &[0.1, 0.2, 0.3, 9.0, 0.4, 0.5, 0.6, 9.0]);
        let px = grid.pixel(1, 0);
        assert_eq!((px.r, px.g, px.b), (0.4, 0.5, 0.6));
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

    #[test]
    fn narrow_glyphs_are_not_black() {
        // A 1- or 2-pixel-wide glyph must survive every subpixel layout.
        for layout in [
            SubpixelLayout::RgbStripe,
            SubpixelLayout::BgrStripe,
            SubpixelLayout::PentileRgbg,
            SubpixelLayout::DeltaRgb,
        ] {
            for w in 1u32..=2 {
                let buf = uniform_buffer(w, 3, 1.0);
                let grid = SubpixelGrid::from_glyph(&buf, layout, true);
                for row in 0..3 {
                    for col in 0..w {
                        let px = grid.pixel(col, row);
                        assert!(
                            (px.r - 1.0).abs() < 1e-5 && (px.g - 1.0).abs() < 1e-5 && (px.b - 1.0).abs() < 1e-5,
                            "{layout:?} width {w} at ({col},{row}): {px:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn stripe_border_columns_keep_phase_shift() {
        // At the right edge of a solid block the border column is filtered
        // with clamp-to-edge, so a uniform row stays uniform to the border.
        let buf = uniform_buffer(5, 1, 0.4);
        let grid = SubpixelGrid::from_glyph(&buf, SubpixelLayout::RgbStripe, false);
        for col in [0u32, 4] {
            let px = grid.pixel(col, 0);
            assert!((px.r - 0.4).abs() < 1e-5 && (px.b - 0.4).abs() < 1e-5);
        }
    }
}
