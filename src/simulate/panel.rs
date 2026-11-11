//! Physical subpixel arrangement of a panel.
//!
//! The renderers produce one `(R, G, B)` triple per logical pixel, but a real
//! panel does not emit that triple from one spot.  Each colour comes from a
//! separate emitter with its own position and aperture inside the pixel, and
//! on some panels (pentile) a pixel does not have every colour at all.
//!
//! [`PanelGeometry`] describes that arrangement as a repeating unit cell of
//! [`Emitter`]s.  [`emit`] paints a rendered [`SubpixelGrid`] onto an
//! oversampled *emission map* — the light the panel surface actually gives
//! off — which the PSF blur in [`super::psf`] then turns into what the viewer
//! sees.
//!
//! ```text
//! SubpixelGrid (w × h, one RGB triple per pixel)
//!     │  emit(geometry, scale)
//!     ▼
//! emission map (w·scale × h·scale, each emitter lit at its own position)
//!     │  PSF convolution (sigma · scale)
//!     ▼
//! simulated view
//! ```
//!
//! # About the built-in geometries
//!
//! The presets returned by [`PanelGeometry::for_layout`] encode the *topology*
//! of each layout (which emitters exist, their order and relative placement)
//! with **nominal** aperture sizes.  They are not measurements of a specific
//! panel.  To calibrate against a datasheet or a macro photograph, build a
//! [`PanelGeometry`] by hand with the measured centres and apertures — all
//! fields are public and expressed in logical-pixel units, so a measurement in
//! micrometres divides by the pixel pitch.
//!
//! # Energy convention
//!
//! Emission is **luminance-preserving**: an emitter is driven brighter than
//! the pixel value by the inverse of its channel's fill factor, so the spatial
//! mean of the emission map equals the rendered value.  A uniform white field
//! therefore integrates back to white on every layout, and individual
//! emitters legitimately exceed 1.0 before the PSF spreads them out.

use crate::layout::SubpixelLayout;
use crate::subpixel::{SubpixelGrid, SubpixelPixel};

// ── Emitters ──────────────────────────────────────────────────────────────────

/// Which colour an emitter produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitterChannel {
    Red,
    Green,
    Blue,
    /// White emitter (WOLED).  Carries the achromatic part of the pixel.
    White,
}

/// One physical emitter inside the repeating unit cell.
///
/// All coordinates are in logical-pixel units relative to the top-left corner
/// of the unit cell.  The emitter is driven by the logical pixel that contains
/// its centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Emitter {
    pub channel: EmitterChannel,
    /// Centre x within the unit cell.
    pub cx: f32,
    /// Centre y within the unit cell.
    pub cy: f32,
    /// Aperture width.
    pub w: f32,
    /// Aperture height.
    pub h: f32,
}

impl Emitter {
    pub const fn new(channel: EmitterChannel, cx: f32, cy: f32, w: f32, h: f32) -> Self {
        Self { channel, cx, cy, w, h }
    }

    /// Aperture area in square logical pixels.
    #[inline]
    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    /// The pixel within the unit cell that drives this emitter.
    #[inline]
    fn cell_pixel(&self) -> (u32, u32) {
        (self.cx.floor() as u32, self.cy.floor() as u32)
    }
}

// ── PanelGeometry ─────────────────────────────────────────────────────────────

/// A panel's subpixel arrangement: a unit cell of emitters that tiles the
/// screen.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelGeometry {
    /// Human-readable name, shown by the CLI.
    pub name: String,
    /// Unit cell width in logical pixels.
    pub cell_w: u32,
    /// Unit cell height in logical pixels.
    pub cell_h: u32,
    /// Emitters within the unit cell.
    pub emitters: Vec<Emitter>,
}

use EmitterChannel::{Blue, Green, Red, White};

impl PanelGeometry {
    /// Nominal geometry for a [`SubpixelLayout`].
    pub fn for_layout(layout: SubpixelLayout) -> Self {
        match layout {
            SubpixelLayout::RgbStripe => Self::rgb_stripe(),
            SubpixelLayout::BgrStripe => Self::bgr_stripe(),
            SubpixelLayout::PentileRgbg => Self::pentile_rgbg(),
            SubpixelLayout::DeltaRgb => Self::delta_rgb(),
            SubpixelLayout::Wrgb => Self::wrgb(),
            SubpixelLayout::Greyscale => Self::greyscale(),
        }
    }

    /// Horizontal RGB stripe: three tall bars, red on the left.
    pub fn rgb_stripe() -> Self {
        Self {
            name: "RGB stripe (nominal)".into(),
            cell_w: 1,
            cell_h: 1,
            emitters: vec![
                Emitter::new(Red, 1.0 / 6.0, 0.5, 0.24, 0.78),
                Emitter::new(Green, 0.5, 0.5, 0.24, 0.78),
                Emitter::new(Blue, 5.0 / 6.0, 0.5, 0.24, 0.78),
            ],
        }
    }

    /// Horizontal BGR stripe: as RGB stripe with red and blue swapped.
    pub fn bgr_stripe() -> Self {
        Self {
            name: "BGR stripe (nominal)".into(),
            cell_w: 1,
            cell_h: 1,
            emitters: vec![
                Emitter::new(Blue, 1.0 / 6.0, 0.5, 0.24, 0.78),
                Emitter::new(Green, 0.5, 0.5, 0.24, 0.78),
                Emitter::new(Red, 5.0 / 6.0, 0.5, 0.24, 0.78),
            ],
        }
    }

    /// Pentile RGBG: every pixel has a narrow green emitter on the right and a
    /// larger red *or* blue emitter on the left, red and blue alternating in a
    /// checkerboard (matches [`crate::layout::pentile::channel_weights`]).
    pub fn pentile_rgbg() -> Self {
        let red = |px: f32, py: f32| Emitter::new(Red, px + 0.28, py + 0.5, 0.30, 0.50);
        let blue = |px: f32, py: f32| Emitter::new(Blue, px + 0.28, py + 0.5, 0.36, 0.62);
        let green = |px: f32, py: f32| Emitter::new(Green, px + 0.76, py + 0.5, 0.18, 0.46);
        Self {
            name: "Pentile RGBG (nominal)".into(),
            cell_w: 2,
            cell_h: 2,
            emitters: vec![
                red(0.0, 0.0), green(0.0, 0.0),
                blue(1.0, 0.0), green(1.0, 0.0),
                blue(0.0, 1.0), green(0.0, 1.0),
                red(1.0, 1.0), green(1.0, 1.0),
            ],
        }
    }

    /// Delta / triangular RGB (QD-OLED style): green above, red and blue side
    /// by side below it, so red and blue sit half a pixel lower than green.
    pub fn delta_rgb() -> Self {
        Self {
            name: "Delta RGB (nominal)".into(),
            cell_w: 1,
            cell_h: 1,
            emitters: vec![
                Emitter::new(Green, 0.50, 0.27, 0.44, 0.34),
                Emitter::new(Red, 0.26, 0.73, 0.36, 0.36),
                Emitter::new(Blue, 0.75, 0.73, 0.32, 0.36),
            ],
        }
    }

    /// WOLED stripe with a white emitter: four bars in R, W, B, G order.
    pub fn wrgb() -> Self {
        Self {
            name: "WRGB stripe (nominal)".into(),
            cell_w: 1,
            cell_h: 1,
            emitters: vec![
                Emitter::new(Red, 0.125, 0.5, 0.17, 0.72),
                Emitter::new(White, 0.375, 0.5, 0.17, 0.72),
                Emitter::new(Blue, 0.625, 0.5, 0.17, 0.72),
                Emitter::new(Green, 0.875, 0.5, 0.17, 0.72),
            ],
        }
    }

    /// No subpixel structure: each pixel is one full-aperture source.
    pub fn greyscale() -> Self {
        Self {
            name: "Unstructured (full-pixel emitters)".into(),
            cell_w: 1,
            cell_h: 1,
            emitters: vec![
                Emitter::new(Red, 0.5, 0.5, 1.0, 1.0),
                Emitter::new(Green, 0.5, 0.5, 1.0, 1.0),
                Emitter::new(Blue, 0.5, 0.5, 1.0, 1.0),
            ],
        }
    }

    /// Fraction of the panel area covered by `channel`'s apertures.
    ///
    /// Zero if the panel has no emitter of that channel.
    pub fn fill_factor(&self, channel: EmitterChannel) -> f32 {
        let area: f32 = self
            .emitters
            .iter()
            .filter(|e| e.channel == channel)
            .map(Emitter::area)
            .sum();
        area / (self.cell_w * self.cell_h) as f32
    }

    /// Whether the panel has a white emitter.
    pub fn has_white(&self) -> bool {
        self.emitters.iter().any(|e| e.channel == White)
    }

    /// Check that the geometry is usable: a non-empty cell, every emitter with
    /// a positive aperture lying inside the cell, and red, green and blue all
    /// present.
    pub fn validate(&self) -> Result<(), String> {
        if self.cell_w == 0 || self.cell_h == 0 {
            return Err("unit cell must be at least 1×1 pixels".into());
        }
        let (cw, ch) = (self.cell_w as f32, self.cell_h as f32);
        const EPS: f32 = 1e-4;
        for (i, e) in self.emitters.iter().enumerate() {
            if !(e.w > 0.0 && e.h > 0.0) {
                return Err(format!("emitter {i} has a non-positive aperture"));
            }
            let inside = e.cx - e.w / 2.0 >= -EPS
                && e.cy - e.h / 2.0 >= -EPS
                && e.cx + e.w / 2.0 <= cw + EPS
                && e.cy + e.h / 2.0 <= ch + EPS;
            if !inside {
                return Err(format!("emitter {i} extends outside the unit cell"));
            }
        }
        for ch in [Red, Green, Blue] {
            if self.fill_factor(ch) <= 0.0 {
                return Err(format!("panel has no {ch:?} emitter"));
            }
        }
        Ok(())
    }
}

// ── Emission ──────────────────────────────────────────────────────────────────

/// Paint a rendered grid onto an oversampled emission map.
///
/// The output is `grid.width * scale` × `grid.height * scale` samples, in
/// linear light.  Each emitter is drawn as an antialiased rectangle at its
/// physical position, lit with the value of the logical pixel that drives it.
///
/// - Red / green / blue emitters take the matching channel of their pixel.
///   A pixel with no emitter for a channel (pentile) simply does not show
///   that channel — which is exactly the loss the renderers must work around.
/// - On panels with a white emitter, the achromatic part `min(r, g, b)` goes
///   to white and only the remainder to the colour emitters.
///
/// Emission is luminance-preserving (see the module docs), so values above
/// 1.0 are expected inside apertures.
///
/// `scale` is clamped to at least 1.  Input must be linear light.
pub fn emit(grid: &SubpixelGrid, geometry: &PanelGeometry, scale: u32) -> SubpixelGrid {
    let scale = scale.max(1);
    let mut out = SubpixelGrid::new(grid.width * scale, grid.height * scale);
    if grid.width == 0 || grid.height == 0 || geometry.cell_w == 0 || geometry.cell_h == 0 {
        return out;
    }

    let gain = |ch: EmitterChannel| {
        let f = geometry.fill_factor(ch);
        if f > 0.0 { 1.0 / f } else { 0.0 }
    };
    let (gain_r, gain_g, gain_b, gain_w) = (gain(Red), gain(Green), gain(Blue), gain(White));
    let has_white = geometry.has_white();
    let s = scale as f32;

    for row in 0..grid.height {
        for col in 0..grid.width {
            let px = grid.pixel(col, row);
            let white = if has_white { px.r.min(px.g).min(px.b).max(0.0) } else { 0.0 };
            let cell_col = col % geometry.cell_w;
            let cell_row = row % geometry.cell_h;

            for e in &geometry.emitters {
                if e.cell_pixel() != (cell_col, cell_row) {
                    continue;
                }
                // Radiance of this emitter, as an RGB triple.
                let rgb = match e.channel {
                    Red => [(px.r - white) * gain_r, 0.0, 0.0],
                    Green => [0.0, (px.g - white) * gain_g, 0.0],
                    Blue => [0.0, 0.0, (px.b - white) * gain_b],
                    White => [white * gain_w; 3],
                };
                if rgb == [0.0; 3] {
                    continue;
                }

                // Aperture rectangle in oversampled sample coordinates.
                let ox = (col - cell_col) as f32;
                let oy = (row - cell_row) as f32;
                let x0 = (ox + e.cx - e.w / 2.0) * s;
                let x1 = (ox + e.cx + e.w / 2.0) * s;
                let y0 = (oy + e.cy - e.h / 2.0) * s;
                let y1 = (oy + e.cy + e.h / 2.0) * s;
                paint_rect(&mut out, x0, y0, x1, y1, rgb);
            }
        }
    }

    out
}

/// Add `rgb`, weighted by area coverage, to every sample the rectangle
/// touches.  Parts of the rectangle outside the grid are dropped.
fn paint_rect(out: &mut SubpixelGrid, x0: f32, y0: f32, x1: f32, y1: f32, rgb: [f32; 3]) {
    let (w, h) = (out.width as f32, out.height as f32);
    let (x0, x1) = (x0.max(0.0), x1.min(w));
    let (y0, y1) = (y0.max(0.0), y1.min(h));
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let sx_end = (x1.ceil() as u32).min(out.width);
    let sy_end = (y1.ceil() as u32).min(out.height);
    for sy in (y0.floor() as u32)..sy_end {
        let cov_y = (y1.min(sy as f32 + 1.0) - y0.max(sy as f32)).max(0.0);
        for sx in (x0.floor() as u32)..sx_end {
            let cov_x = (x1.min(sx as f32 + 1.0) - x0.max(sx as f32)).max(0.0);
            let cov = cov_x * cov_y;
            let p = out.pixel_mut(sx, sy);
            p.r += rgb[0] * cov;
            p.g += rgb[1] * cov;
            p.b += rgb[2] * cov;
        }
    }
}

/// Integrate an oversampled map back to one value per logical pixel.
///
/// This is the inverse resolution change of [`emit`]: each output pixel is the
/// mean of its `scale × scale` block, i.e. the light that pixel area gives off
/// on average.  Used to compare a simulated result against the render it came
/// from.  Trailing samples that do not fill a whole block are ignored.
pub fn integrate_pixels(map: &SubpixelGrid, scale: u32) -> SubpixelGrid {
    let scale = scale.max(1);
    let (w, h) = (map.width / scale, map.height / scale);
    let mut out = SubpixelGrid::new(w, h);
    let norm = 1.0 / (scale * scale) as f32;

    for row in 0..h {
        for col in 0..w {
            let mut acc = SubpixelPixel::default();
            for sy in 0..scale {
                for sx in 0..scale {
                    let p = map.pixel(col * scale + sx, row * scale + sy);
                    acc.r += p.r;
                    acc.g += p.g;
                    acc.b += p.b;
                }
            }
            *out.pixel_mut(col, row) = SubpixelPixel {
                r: acc.r * norm,
                g: acc.g * norm,
                b: acc.b * norm,
            };
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_LAYOUTS: [SubpixelLayout; 6] = [
        SubpixelLayout::RgbStripe,
        SubpixelLayout::BgrStripe,
        SubpixelLayout::PentileRgbg,
        SubpixelLayout::DeltaRgb,
        SubpixelLayout::Wrgb,
        SubpixelLayout::Greyscale,
    ];

    fn uniform(w: u32, h: u32, r: f32, g: f32, b: f32) -> SubpixelGrid {
        let mut grid = SubpixelGrid::new(w, h);
        for row in 0..h {
            for col in 0..w {
                *grid.pixel_mut(col, row) = SubpixelPixel { r, g, b };
            }
        }
        grid
    }

    fn mean(grid: &SubpixelGrid) -> [f32; 3] {
        let mut acc = [0.0f64; 3];
        for row in 0..grid.height {
            for col in 0..grid.width {
                let p = grid.pixel(col, row);
                acc[0] += p.r as f64;
                acc[1] += p.g as f64;
                acc[2] += p.b as f64;
            }
        }
        let n = (grid.width * grid.height) as f64;
        [(acc[0] / n) as f32, (acc[1] / n) as f32, (acc[2] / n) as f32]
    }

    #[test]
    fn builtin_geometries_are_valid() {
        for layout in ALL_LAYOUTS {
            let g = PanelGeometry::for_layout(layout);
            g.validate().unwrap_or_else(|e| panic!("{layout:?}: {e}"));
        }
    }

    #[test]
    fn validate_rejects_bad_geometry() {
        let mut g = PanelGeometry::rgb_stripe();
        g.emitters[0].cx = 0.95; // red bar now pokes out of the cell
        assert!(g.validate().is_err());

        let mut g = PanelGeometry::rgb_stripe();
        g.emitters.remove(2); // no blue
        assert!(g.validate().is_err());
    }

    #[test]
    fn fill_factors_are_physical() {
        for layout in ALL_LAYOUTS {
            let g = PanelGeometry::for_layout(layout);
            let total: f32 = [Red, Green, Blue, White].iter().map(|&c| g.fill_factor(c)).sum();
            if layout != SubpixelLayout::Greyscale {
                assert!(total > 0.0 && total <= 1.0, "{layout:?}: total fill {total}");
            }
        }
    }

    #[test]
    fn pentile_has_half_as_many_red_and_blue_as_green() {
        let g = PanelGeometry::pentile_rgbg();
        let count = |c| g.emitters.iter().filter(|e| e.channel == c).count();
        assert_eq!(count(Green), 4);
        assert_eq!(count(Red), 2);
        assert_eq!(count(Blue), 2);
    }

    #[test]
    fn pentile_geometry_matches_renderer_checkerboard() {
        use crate::layout::pentile::channel_weights;
        let g = PanelGeometry::pentile_rgbg();
        for e in &g.emitters {
            let (px, py) = e.cell_pixel();
            let [rw, gw, bw] = channel_weights(px as u8, py as u8);
            let present = match e.channel {
                Red => rw,
                Green => gw,
                Blue => bw,
                White => 0.0,
            };
            assert_eq!(present, 1.0, "{:?} emitter at pixel ({px},{py})", e.channel);
        }
    }

    #[test]
    fn emission_map_has_scaled_dimensions() {
        let grid = uniform(5, 3, 1.0, 1.0, 1.0);
        let map = emit(&grid, &PanelGeometry::rgb_stripe(), 6);
        assert_eq!((map.width, map.height), (30, 18));
    }

    #[test]
    fn white_field_integrates_back_to_white() {
        // Luminance preservation: mean emission equals the rendered value.
        for layout in ALL_LAYOUTS {
            let geometry = PanelGeometry::for_layout(layout);
            let grid = uniform(8, 8, 1.0, 1.0, 1.0);
            let map = emit(&grid, &geometry, 8);
            let [r, g, b] = mean(&map);
            assert!(
                (r - 1.0).abs() < 1e-3 && (g - 1.0).abs() < 1e-3 && (b - 1.0).abs() < 1e-3,
                "{layout:?}: mean emission ({r}, {g}, {b})"
            );
        }
    }

    #[test]
    fn single_cell_layouts_preserve_each_pixel() {
        // Where every pixel has every colour, each pixel integrates to itself.
        for layout in [SubpixelLayout::RgbStripe, SubpixelLayout::DeltaRgb, SubpixelLayout::Wrgb] {
            let geometry = PanelGeometry::for_layout(layout);
            let grid = uniform(4, 4, 0.8, 0.5, 0.2);
            let back = integrate_pixels(&emit(&grid, &geometry, 10), 10);
            for row in 0..4 {
                for col in 0..4 {
                    let p = back.pixel(col, row);
                    assert!(
                        (p.r - 0.8).abs() < 1e-3 && (p.g - 0.5).abs() < 1e-3 && (p.b - 0.2).abs() < 1e-3,
                        "{layout:?} at ({col},{row}): {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn pentile_pixels_lack_one_colour() {
        let grid = uniform(4, 4, 1.0, 1.0, 1.0);
        let back = integrate_pixels(&emit(&grid, &PanelGeometry::pentile_rgbg(), 10), 10);
        for row in 0..4u32 {
            for col in 0..4u32 {
                let p = back.pixel(col, row);
                assert!((p.g - 1.0).abs() < 1e-3, "green in every pixel");
                if (col + row) % 2 == 0 {
                    assert!((p.r - 2.0).abs() < 1e-3 && p.b.abs() < 1e-6, "R pixel ({col},{row}): {p:?}");
                } else {
                    assert!((p.b - 2.0).abs() < 1e-3 && p.r.abs() < 1e-6, "B pixel ({col},{row}): {p:?}");
                }
            }
        }
    }

    #[test]
    fn stripe_order_is_physical() {
        // Centroid of red light is left of blue on RGB, right of it on BGR.
        let centroid = |geometry: &PanelGeometry| {
            let map = emit(&uniform(1, 1, 1.0, 1.0, 1.0), geometry, 12);
            let (mut rx, mut rs, mut bx, mut bs) = (0.0, 0.0, 0.0, 0.0);
            for y in 0..map.height {
                for x in 0..map.width {
                    let p = map.pixel(x, y);
                    rx += p.r * x as f32;
                    rs += p.r;
                    bx += p.b * x as f32;
                    bs += p.b;
                }
            }
            (rx / rs, bx / bs)
        };
        let (r, b) = centroid(&PanelGeometry::rgb_stripe());
        assert!(r < b, "RGB: red {r} should be left of blue {b}");
        let (r, b) = centroid(&PanelGeometry::bgr_stripe());
        assert!(r > b, "BGR: red {r} should be right of blue {b}");
    }

    #[test]
    fn delta_red_and_blue_sit_below_green() {
        let map = emit(&uniform(1, 1, 1.0, 1.0, 1.0), &PanelGeometry::delta_rgb(), 20);
        let (mut gy, mut gs, mut ry, mut rs) = (0.0, 0.0, 0.0, 0.0);
        for y in 0..map.height {
            for x in 0..map.width {
                let p = map.pixel(x, y);
                gy += p.g * y as f32;
                gs += p.g;
                ry += p.r * y as f32;
                rs += p.r;
            }
        }
        let offset = (ry / rs - gy / gs) / 20.0;
        assert!((offset - 0.46).abs() < 0.05, "R should sit ~half a pixel below G, got {offset}");
    }

    #[test]
    fn wrgb_grey_uses_only_the_white_emitter() {
        let geometry = PanelGeometry::wrgb();
        let map = emit(&uniform(1, 1, 0.6, 0.6, 0.6), &geometry, 16);
        // Every lit sample must be achromatic.
        for y in 0..map.height {
            for x in 0..map.width {
                let p = map.pixel(x, y);
                assert!((p.r - p.g).abs() < 1e-5 && (p.g - p.b).abs() < 1e-5, "chromatic sample at ({x},{y}): {p:?}");
            }
        }
        // And only the white bar's columns are lit (second of four bars).
        assert!(map.pixel(6, 8).g > 0.0, "white bar should be lit");
        assert_eq!(map.pixel(2, 8).g, 0.0, "red bar should be dark");
    }

    #[test]
    fn wrgb_coloured_pixel_splits_white_and_colour() {
        let geometry = PanelGeometry::wrgb();
        let back = integrate_pixels(&emit(&uniform(2, 2, 1.0, 0.4, 0.4), &geometry, 16), 16);
        let p = back.pixel(0, 0);
        assert!((p.r - 1.0).abs() < 1e-3 && (p.g - 0.4).abs() < 1e-3 && (p.b - 0.4).abs() < 1e-3, "{p:?}");
    }

    #[test]
    fn black_stays_black() {
        for layout in ALL_LAYOUTS {
            let map = emit(&uniform(3, 3, 0.0, 0.0, 0.0), &PanelGeometry::for_layout(layout), 6);
            assert_eq!(mean(&map), [0.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn fractional_apertures_are_antialiased() {
        // At scale 1 an emitter covers a fraction of the single sample, and
        // the total must still be exact.
        let map = emit(&uniform(3, 3, 1.0, 1.0, 1.0), &PanelGeometry::delta_rgb(), 1);
        let [r, g, b] = mean(&map);
        assert!((r - 1.0).abs() < 1e-4 && (g - 1.0).abs() < 1e-4 && (b - 1.0).abs() < 1e-4);
    }
}
