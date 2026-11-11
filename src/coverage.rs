//! [`Coverage`] — a glyph's coverage at pixel resolution and finer.
//!
//! A renderer that places each colour where its emitter sits needs to know
//! what the glyph looks like *between* pixel centres. One coverage value per
//! pixel cannot say: it has already averaged that detail away. `Coverage`
//! therefore carries two planes:
//!
//! - `hi` — the glyph rasterized `scale` times finer than the pixel grid in
//!   each axis;
//! - `lo` — one value per logical pixel, the box average of `hi`.
//!
//! Both are **linear area coverage** in 0..=1: the fraction of each sample
//! the outline covers. For white text on black that fraction *is* the
//! linear-light intensity, so no transfer function is applied.
//!
//! The greyscale and subpixel renderers read `lo`. The OLED-aware renderer
//! integrates `hi` over a footprint centred on each emitter
//! ([`Coverage::sample_box`]). The simulator's metrics use a second, finer
//! `Coverage` as the reference a perfect display would show.

use crate::font::loader::{FontFace, GlyphBitmap, RasterMode};
use crate::glyph::GlyphBuffer;
use crate::plane::Plane;

/// Coverage of one glyph at two resolutions.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    /// One sample per logical pixel.
    pub lo: Plane,
    /// `scale × scale` samples per logical pixel.
    pub hi: Plane,
    /// Samples of `hi` per logical pixel along each axis (≥ 1).
    pub scale: u32,
}

/// Glyph placement that comes with a rasterized [`Coverage`], in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphMetrics {
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: i32,
}

impl Coverage {
    /// Pixel-resolution coverage only (`scale` 1): `hi` is `lo`.
    pub fn from_plane(lo: Plane) -> Self {
        Self { hi: lo.clone(), lo, scale: 1 }
    }

    /// The coverage (alpha) channel of a glyph buffer, at pixel resolution.
    pub fn from_glyph(buf: &GlyphBuffer) -> Self {
        Self::from_plane(Plane::from_coverage(buf))
    }

    /// Build from a fine plane whose size is a multiple of `scale`.
    pub fn from_hi(hi: Plane, scale: u32) -> Self {
        let scale = scale.max(1);
        assert!(
            hi.width % scale == 0 && hi.height % scale == 0,
            "fine plane {}×{} is not a multiple of scale {scale}",
            hi.width,
            hi.height
        );
        let n = scale as usize;
        let stride = hi.width as usize;
        let norm = 1.0 / (n * n) as f32;
        let lo = Plane::from_fn(hi.width / scale, hi.height / scale, |col, row| {
            let (x0, y0) = (col as usize * n, row as usize * n);
            let mut acc = 0.0;
            for y in y0..y0 + n {
                acc += hi.data[y * stride + x0..y * stride + x0 + n].iter().sum::<f32>();
            }
            acc * norm
        });
        Self { lo, hi, scale }
    }

    /// Rasterize `ch` from `face` with `scale × scale` samples per pixel.
    ///
    /// The fine raster is laid over the extent of the pixel-resolution
    /// bitmap, so `lo` is that same bitmap (to within 8-bit rounding).
    /// Returns `None` if the font has no such glyph.
    pub fn rasterize(face: &FontFace, ch: char, scale: u32) -> Option<(Self, GlyphMetrics)> {
        let scale = scale.max(1);
        let coarse = face.rasterize_grey(ch)?;
        let fine = face.rasterize_grey_scaled(ch, scale)?;
        let metrics = GlyphMetrics {
            bearing_x: coarse.bearing_x,
            bearing_y: coarse.bearing_y,
            advance: coarse.advance,
        };
        Some((Self::from_bitmaps(&coarse, &fine, scale), metrics))
    }

    /// Lay the fine bitmap `fine` (rasterized `scale` times finer) over the
    /// extent of the pixel-resolution bitmap `coarse`.
    pub fn from_bitmaps(coarse: &GlyphBitmap, fine: &GlyphBitmap, scale: u32) -> Self {
        assert_eq!(coarse.mode, RasterMode::Grey, "coverage needs greyscale bitmaps");
        assert_eq!(fine.mode, RasterMode::Grey, "coverage needs greyscale bitmaps");
        let scale = scale.max(1);
        let k = scale as i32;
        // Position of the fine bitmap's top-left inside the coarse extent,
        // in fine samples. Bearings: x grows right, y grows up.
        let dx = fine.bearing_x - coarse.bearing_x * k;
        let dy = coarse.bearing_y * k - fine.bearing_y;
        let (fw, fh) = (fine.width as i32, fine.height as i32);
        let hi = Plane::from_fn(coarse.width * scale, coarse.height * scale, |x, y| {
            let (sx, sy) = (x as i32 - dx, y as i32 - dy);
            if sx >= 0 && sx < fw && sy >= 0 && sy < fh {
                fine.buffer[(sy * fw + sx) as usize] as f32 / 255.0
            } else {
                0.0
            }
        });
        Self::from_hi(hi, scale)
    }

    /// Width in logical pixels.
    pub fn width(&self) -> u32 {
        self.lo.width
    }

    /// Height in logical pixels.
    pub fn height(&self) -> u32 {
        self.lo.height
    }

    /// The pixel-resolution coverage as a [`GlyphBuffer`] (all four
    /// components equal to the coverage).
    pub fn to_glyph_buffer(&self) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(self.lo.width, self.lo.height);
        for (px, &c) in buf.pixels.chunks_exact_mut(4).zip(&self.lo.data) {
            px.fill(c);
        }
        buf
    }

    /// Mean coverage over the rectangle `w × h` logical pixels centred on
    /// (`cx`, `cy`).
    ///
    /// Coordinates are continuous: pixel `(col, row)` spans
    /// `col..col + 1` × `row..row + 1`, so its centre is at
    /// `(col + 0.5, row + 0.5)`. Outside the glyph the edge value repeats.
    pub fn sample_box(&self, cx: f32, cy: f32, w: f32, h: f32) -> f32 {
        let k = self.scale as f32;
        let min = 1.0 / k; // never narrower than one fine sample
        let (w, h) = (w.max(min), h.max(min));
        let (x0, x1) = ((cx - w / 2.0) * k, (cx + w / 2.0) * k);
        let (y0, y1) = ((cy - h / 2.0) * k, (cy + h / 2.0) * k);
        let mut acc = 0.0;
        for sy in (y0.floor() as i32)..(y1.ceil() as i32) {
            let wy = (y1.min(sy as f32 + 1.0) - y0.max(sy as f32)).max(0.0);
            if wy == 0.0 {
                continue;
            }
            let mut row = 0.0;
            for sx in (x0.floor() as i32)..(x1.ceil() as i32) {
                let wx = (x1.min(sx as f32 + 1.0) - x0.max(sx as f32)).max(0.0);
                row += wx * self.hi.get(sx, sy);
            }
            acc += wy * row;
        }
        acc / ((x1 - x0) * (y1 - y0))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A fine plane, 4 samples per pixel, lit from fine column `edge` on.
    fn step(edge: u32) -> Coverage {
        Coverage::from_hi(Plane::from_fn(16, 8, |x, _| if x >= edge { 1.0 } else { 0.0 }), 4)
    }

    #[test]
    fn lo_is_the_box_average_of_hi() {
        let cov = step(6); // edge at 1.5 px
        assert_eq!((cov.width(), cov.height()), (4, 2));
        assert_eq!(cov.lo.data[..4], [0.0, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn a_pixel_sized_box_on_a_pixel_centre_is_lo() {
        let cov = step(6);
        for col in 0..4 {
            let v = cov.sample_box(col as f32 + 0.5, 0.5, 1.0, 1.0);
            assert!((v - cov.lo.get(col, 0)).abs() < 1e-6);
        }
    }

    #[test]
    fn a_shifted_box_sees_what_is_between_pixel_centres() {
        let cov = step(6); // edge at x = 1.5
        // A 1 px box centred at x = 1.75 spans 1.25..2.25: 0.75 lit.
        assert!((cov.sample_box(1.75, 0.5, 1.0, 1.0) - 0.75).abs() < 1e-6);
        // A narrow box entirely right of the edge is fully lit.
        assert!((cov.sample_box(1.75, 0.5, 0.5, 1.0) - 1.0).abs() < 1e-6);
        // Fractional sample coverage: 1.4..1.6 straddles the edge evenly.
        assert!((cov.sample_box(1.5, 0.5, 0.2, 1.0) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn at_scale_one_a_shifted_box_interpolates_linearly() {
        let cov = Coverage::from_plane(Plane::from_fn(4, 1, |x, _| [0.0, 0.0, 1.0, 1.0][x as usize]));
        assert!((cov.sample_box(1.75, 0.5, 1.0, 1.0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn boxes_outside_the_glyph_repeat_the_edge() {
        let cov = Coverage::from_plane(Plane::from_fn(3, 3, |_, _| 0.7));
        assert!((cov.sample_box(-2.0, 1.5, 1.0, 1.0) - 0.7).abs() < 1e-6);
        assert!((cov.sample_box(1.5, 9.0, 2.5, 1.0) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn fine_bitmap_is_placed_by_its_bearings() {
        // Coarse: 2×1 px at bearing (1, 1). Fine (×2): 2×2 samples whose
        // left edge is at fine x = 3, i.e. one sample into the first pixel.
        let coarse = GlyphBitmap {
            width: 2, height: 1, bearing_x: 1, bearing_y: 1, advance: 3,
            buffer: vec![128, 128], mode: RasterMode::Grey,
        };
        let fine = GlyphBitmap {
            width: 2, height: 2, bearing_x: 3, bearing_y: 2, advance: 6,
            buffer: vec![255; 4], mode: RasterMode::Grey,
        };
        let cov = Coverage::from_bitmaps(&coarse, &fine, 2);
        assert_eq!((cov.hi.width, cov.hi.height), (4, 2));
        assert_eq!(cov.hi.data, vec![0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0]);
        assert_eq!(cov.lo.data, vec![0.5, 0.5]);
    }

    #[test]
    fn glyph_buffer_round_trip() {
        let cov = step(6);
        let buf = cov.to_glyph_buffer();
        assert_eq!(buf.pixel(1, 0), &[0.5; 4]);
        assert_eq!(Coverage::from_glyph(&buf).lo, cov.lo);
    }
}
