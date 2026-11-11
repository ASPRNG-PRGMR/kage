//! [`EmitterMap`] — where each colour's light comes from, pixel by pixel.
//!
//! The OLED-aware renderer does not use a fixed kernel per layout. It asks
//! one question of the panel geometry: *for this pixel and this colour,
//! where is the emitter?* — and samples the glyph there. An `EmitterMap` is
//! the answer, precomputed from a [`PanelGeometry`]. Because it is derived
//! from the same description the simulator draws, a panel loaded from a
//! `.toml` file drives the renderer as well as the simulation.
//!
//! # White emitters (WRGB)
//!
//! A WRGB panel moves the achromatic part of a pixel to its white emitter,
//! and for white text that is all of it: the colour emitters stay dark. So
//! on a panel with white emitters every channel is sampled at the white
//! emitter, which reconstructs the luminance where it is actually emitted
//! and keeps the output achromatic.
//!
//! # Pixels that lack a colour (Pentile)
//!
//! Where a pixel has no emitter of some colour the value computed for it is
//! never displayed. It is sampled at the position that colour has in the
//! pixels that do have it, so the raw image stays continuous.

use crate::simulate::{EmitterChannel, PanelGeometry};
use super::SubpixelLayout;

/// Largest unit cell, in pixels per side, the renderer supports. The GPU
/// shaders hold `MAX_CELL × MAX_CELL` sites.
pub const MAX_CELL: u32 = 2;

/// One colour in one pixel of the unit cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Site {
    /// Emitter centroid within the pixel, `[x, y]` in 0..=1 (y down).
    pub pos: [f32; 2],
    /// Whether this pixel has an emitter of this colour at all.
    pub present: bool,
}

/// Emitter positions for every pixel of a panel's unit cell.
#[derive(Debug, Clone, PartialEq)]
pub struct EmitterMap {
    pub cell_w: u32,
    pub cell_h: u32,
    /// `[red, green, blue]` sites per cell pixel, row-major.
    pub sites: Vec<[Site; 3]>,
    /// True when luminance comes from white emitters (see module docs).
    pub achromatic: bool,
}

impl EmitterMap {
    /// Every colour at the pixel centre: an unstructured display.
    pub fn centred() -> Self {
        Self {
            cell_w: 1,
            cell_h: 1,
            sites: vec![[Site { pos: [0.5, 0.5], present: true }; 3]],
            achromatic: false,
        }
    }

    /// The map for a built-in layout's nominal geometry.
    pub fn for_layout(layout: SubpixelLayout) -> Self {
        Self::from_geometry(&PanelGeometry::for_layout(layout))
            .expect("built-in geometries have unit cells within MAX_CELL")
    }

    /// Derive the map from a panel description.
    ///
    /// Fails if the unit cell is larger than [`MAX_CELL`] pixels per side.
    pub fn from_geometry(geometry: &PanelGeometry) -> Result<Self, String> {
        let (cw, ch) = (geometry.cell_w, geometry.cell_h);
        if cw == 0 || ch == 0 || cw > MAX_CELL || ch > MAX_CELL {
            return Err(format!(
                "the renderer supports unit cells from 1×1 to {MAX_CELL}×{MAX_CELL} pixels, this panel's is {cw}×{ch}"
            ));
        }
        let achromatic = geometry.has_white();

        // Area-weighted centroid of `channel` inside cell pixel (px, py),
        // relative to that pixel's corner.
        let centroid = |channel: EmitterChannel, px: u32, py: u32| -> Option<[f32; 2]> {
            let (mut x, mut y, mut area) = (0.0f32, 0.0f32, 0.0f32);
            for e in geometry.emitters.iter().filter(|e| e.channel == channel) {
                if (e.cx.floor() as u32, e.cy.floor() as u32) == (px, py) {
                    x += (e.cx - px as f32) * e.area();
                    y += (e.cy - py as f32) * e.area();
                    area += e.area();
                }
            }
            (area > 0.0).then(|| [x / area, y / area])
        };

        let channels = if achromatic {
            [EmitterChannel::White; 3]
        } else {
            [EmitterChannel::Red, EmitterChannel::Green, EmitterChannel::Blue]
        };

        let mut sites = vec![[Site { pos: [0.5, 0.5], present: false }; 3]; (cw * ch) as usize];
        for (k, &channel) in channels.iter().enumerate() {
            let found: Vec<Option<[f32; 2]>> = (0..cw * ch).map(|i| centroid(channel, i % cw, i / cw)).collect();
            let present: Vec<[f32; 2]> = found.iter().flatten().copied().collect();
            if present.is_empty() {
                return Err(format!("panel has no {channel:?} emitter"));
            }
            // Where the colour is absent, use its mean position elsewhere.
            let n = present.len() as f32;
            let mean = [
                present.iter().map(|p| p[0]).sum::<f32>() / n,
                present.iter().map(|p| p[1]).sum::<f32>() / n,
            ];
            for (i, f) in found.iter().enumerate() {
                sites[i][k] = Site { pos: f.unwrap_or(mean), present: f.is_some() };
            }
        }
        Ok(Self { cell_w: cw, cell_h: ch, sites, achromatic })
    }

    /// The three sites of the pixel at (`col`, `row`).
    #[inline]
    pub fn at(&self, col: u32, row: u32) -> &[Site; 3] {
        &self.sites[((row % self.cell_h) * self.cell_w + col % self.cell_w) as usize]
    }

    /// Whether emitter positions differ between colours horizontally
    /// (`[0]`) and vertically (`[1]`), as 1.0 or 0.0.
    ///
    /// An edge can only fringe along an axis in which the colours are
    /// displaced from one another: a stripe panel fringes on vertical edges
    /// only, a delta panel on all of them.
    pub fn fringe_axis(&self) -> [f32; 2] {
        let mut axis = [0.0f32; 2];
        for site in &self.sites {
            for (a, slot) in axis.iter_mut().enumerate() {
                let spread = site.iter().any(|s| (s.pos[a] - site[0].pos[a]).abs() > 1e-4);
                if spread {
                    *slot = 1.0;
                }
            }
        }
        axis
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulate::Emitter;

    #[test]
    fn stripe_sites_are_the_three_bars() {
        let map = EmitterMap::for_layout(SubpixelLayout::RgbStripe);
        assert_eq!((map.cell_w, map.cell_h), (1, 1));
        let [r, g, b] = *map.at(5, 9);
        assert!((r.pos[0] - 1.0 / 6.0).abs() < 1e-5 && (g.pos[0] - 0.5).abs() < 1e-5 && (b.pos[0] - 5.0 / 6.0).abs() < 1e-5);
        assert!(r.present && g.present && b.present);
        assert_eq!(map.fringe_axis(), [1.0, 0.0]);
        // BGR mirrors it.
        let bgr = EmitterMap::for_layout(SubpixelLayout::BgrStripe);
        assert!((bgr.at(0, 0)[0].pos[0] - 5.0 / 6.0).abs() < 1e-5);
    }

    #[test]
    fn delta_colours_are_displaced_in_both_axes() {
        let map = EmitterMap::for_layout(SubpixelLayout::DeltaRgb);
        let [r, g, b] = *map.at(0, 0);
        assert!(g.pos[1] < 0.5 && r.pos[1] > 0.5 && b.pos[1] > 0.5);
        assert!(r.pos[0] < 0.5 && b.pos[0] > 0.5);
        assert_eq!(map.fringe_axis(), [1.0, 1.0]);
    }

    #[test]
    fn pentile_red_and_blue_alternate() {
        let map = EmitterMap::for_layout(SubpixelLayout::PentileRgbg);
        assert_eq!((map.cell_w, map.cell_h), (2, 2));
        for row in 0..4 {
            for col in 0..4 {
                let [r, g, b] = *map.at(col, row);
                assert!(g.present);
                assert_eq!(r.present, (col + row) % 2 == 0, "red at ({col},{row})");
                assert_eq!(b.present, (col + row) % 2 == 1, "blue at ({col},{row})");
                // Absent colours are sampled where they sit elsewhere.
                assert!((r.pos[0] - 0.28).abs() < 1e-5 && (b.pos[0] - 0.28).abs() < 1e-5);
                assert!((g.pos[0] - 0.76).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn wrgb_samples_every_channel_at_the_white_emitter() {
        let map = EmitterMap::for_layout(SubpixelLayout::Wrgb);
        assert!(map.achromatic);
        for site in map.at(0, 0) {
            assert!((site.pos[0] - 0.375).abs() < 1e-5 && (site.pos[1] - 0.5).abs() < 1e-5);
        }
        assert_eq!(map.fringe_axis(), [0.0, 0.0]);
    }

    #[test]
    fn greyscale_is_centred() {
        assert_eq!(EmitterMap::for_layout(SubpixelLayout::Greyscale), EmitterMap::centred());
    }

    #[test]
    fn oversized_cells_are_rejected() {
        let mut big = PanelGeometry::pentile_rgbg();
        big.cell_w = 3;
        assert!(EmitterMap::from_geometry(&big).is_err());
    }

    #[test]
    fn split_emitters_use_the_area_weighted_centroid() {
        let mut panel = PanelGeometry::rgb_stripe();
        // A second, twice-as-large green emitter further right.
        panel.emitters.push(Emitter::new(EmitterChannel::Green, 0.8, 0.5, 0.24, 1.56 / 2.0 * 2.0));
        let g = EmitterMap::from_geometry(&panel).unwrap().at(0, 0)[1];
        assert!((g.pos[0] - (0.5 + 2.0 * 0.8) / 3.0).abs() < 1e-5);
    }
}
