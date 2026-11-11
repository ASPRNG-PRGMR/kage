//! Delta-RGB (QD-OLED) 2-D filter kernel.
//!
//! # Layout
//!
//! Each pixel has three emitters arranged in a triangle: green in the upper
//! half, red and blue side by side in the lower half.
//!
//! ```text
//!   ┌───────────┐
//!   │    GGG    │   green   centred, about a quarter pixel above centre
//!   │           │
//!   │  RR   BB  │   red     a quarter pixel left of and below centre
//!   └───────────┘   blue    a quarter pixel right of and below centre
//! ```
//!
//! A horizontal-only filter cannot describe this: green is displaced
//! *vertically* from red and blue, so a horizontal edge fringes green on one
//! side and magenta on the other.
//!
//! # Kernel
//!
//! Each channel should show the image as it is at its own emitter, so each
//! channel samples the coverage at its emitter's offset from the pixel
//! centre, by linear interpolation in both axes. That is a separable 3×3
//! kernel per channel — [`crate::layout::shift_taps`] horizontally times the
//! same vertically — and it sums to one, so flat areas are unchanged.

use super::{shift_taps, FilterKernel};
use crate::simulate::{EmitterChannel, PanelGeometry};

/// Offset of each channel's emitter from the pixel centre, in pixels
/// (`[x, y]`, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelOffsets {
    pub red: [f32; 2],
    pub green: [f32; 2],
    pub blue: [f32; 2],
}

/// Offsets of the nominal Delta-RGB geometry
/// ([`PanelGeometry::delta_rgb`]).
pub const NOMINAL: ChannelOffsets = ChannelOffsets {
    red: [-0.24, 0.23],
    green: [0.0, -0.23],
    blue: [0.25, 0.23],
};

impl ChannelOffsets {
    /// Emitter centroids of a panel whose unit cell is a single pixel.
    ///
    /// Returns `None` for larger unit cells (their offsets differ from pixel
    /// to pixel) or if a colour is missing.
    pub fn from_geometry(geometry: &PanelGeometry) -> Option<Self> {
        if geometry.cell_w != 1 || geometry.cell_h != 1 {
            return None;
        }
        let centroid = |channel: EmitterChannel| {
            let (mut x, mut y, mut area) = (0.0f32, 0.0f32, 0.0f32);
            for e in geometry.emitters.iter().filter(|e| e.channel == channel) {
                x += e.cx * e.area();
                y += e.cy * e.area();
                area += e.area();
            }
            (area > 0.0).then(|| [x / area - 0.5, y / area - 0.5])
        };
        Some(Self {
            red: centroid(EmitterChannel::Red)?,
            green: centroid(EmitterChannel::Green)?,
            blue: centroid(EmitterChannel::Blue)?,
        })
    }
}

/// The 2-D kernel that samples each channel at its emitter.
pub fn kernel(offsets: &ChannelOffsets) -> FilterKernel {
    let per_channel = [offsets.red, offsets.green, offsets.blue];
    FilterKernel {
        h: per_channel.map(|o| shift_taps(o[0])),
        v: per_channel.map(|o| shift_taps(o[1])),
        pentile: false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::Plane;

    #[test]
    fn nominal_offsets_match_the_simulated_panel() {
        let from_panel = ChannelOffsets::from_geometry(&PanelGeometry::delta_rgb()).unwrap();
        for (a, b) in [
            (from_panel.red, NOMINAL.red),
            (from_panel.green, NOMINAL.green),
            (from_panel.blue, NOMINAL.blue),
        ] {
            assert!((a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn multi_pixel_cells_have_no_single_offset() {
        assert!(ChannelOffsets::from_geometry(&PanelGeometry::pentile_rgbg()).is_none());
    }

    #[test]
    fn taps_sum_to_one() {
        let k = kernel(&NOMINAL);
        for ch in 0..3 {
            assert!((k.h[ch].iter().sum::<f32>() - 1.0).abs() < 1e-6);
            assert!((k.v[ch].iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn horizontal_edge_separates_green_from_red_and_blue() {
        // Black above, white below: the row above the edge is lit only by
        // channels whose emitters sit low in the pixel (red, blue).
        let cov = Plane::from_fn(5, 6, |_, r| if r >= 3 { 1.0 } else { 0.0 });
        let k = kernel(&NOMINAL);
        let [r, g, b] = k.sample(&cov, 2, 2);
        assert!(r > 0.1 && b > 0.1, "red/blue reach down into the white: {r} {b}");
        assert_eq!(g, 0.0, "green sits high and sees only black");
        // …and the first white row has green dimmed, red and blue full.
        let [r, g, b] = k.sample(&cov, 2, 3);
        assert!((r - 1.0).abs() < 1e-6 && (b - 1.0).abs() < 1e-6);
        assert!(g < 0.9);
    }

    #[test]
    fn vertical_edge_separates_red_from_blue() {
        let cov = Plane::from_fn(6, 5, |c, _| if c >= 3 { 1.0 } else { 0.0 });
        let k = kernel(&NOMINAL);
        let [r, g, b] = k.sample(&cov, 2, 2);
        assert_eq!(r, 0.0);
        assert_eq!(g, 0.0);
        assert!(b > 0.1, "blue sits right of centre and reaches the white: {b}");
    }
}
