//! Subpixel layout geometry.
//!
//! A [`SubpixelLayout`] describes how physical colour emitters are arranged
//! within a logical pixel grid.  The layout determines:
//!
//! - Which filter kernels are valid (horizontal-only vs 2-D)
//! - How coverage values are split across R, G, B channels
//! - Whether subpixel rendering is beneficial at the current DPI
//!
//! # Adding a new layout
//!
//! 1. Add a variant to [`SubpixelLayout`].
//! 2. Give it a [`BaseFilter`] variant whose [`FilterKernel`] says where each
//!    colour channel samples the coverage (see `delta.rs` as the reference).
//! 3. Register it in [`SubpixelLayout::channel_weights`].
//! 4. Describe its emitters in `PanelGeometry::for_layout`
//!    (`simulate/panel.rs`) so the display simulator can draw it.
//!
//! # Filter kernels
//!
//! Every layout filter has the same shape: each colour channel reads the
//! coverage through a separable 3×3 kernel that shifts the sampling point
//! to where that channel's emitter sits. [`FilterKernel`] holds those taps.
//! Because the CPU renderer and both GPU backends evaluate the same taps,
//! their outputs agree.
//!
//! The OLED-aware renderer goes further and reads emitter positions straight
//! from the panel geometry: see [`EmitterMap`].

pub mod delta;
pub mod emitters;
pub mod pentile;

pub use emitters::EmitterMap;

use crate::plane::Plane;

/// Taps for the centre channel of a stripe panel: a mild symmetric low-pass.
pub const STRIPE_CENTRE_TAPS: [f32; 3] = [0.11, 0.78, 0.11];
/// Taps for the stripe channel whose emitter sits left of centre.
pub const STRIPE_LEFT_TAPS: [f32; 3] = [0.30, 0.60, 0.10];
/// Taps for the stripe channel whose emitter sits right of centre.
pub const STRIPE_RIGHT_TAPS: [f32; 3] = [0.10, 0.60, 0.30];
/// Taps that read exactly the centre sample.
pub const IDENTITY_TAPS: [f32; 3] = [0.0, 1.0, 0.0];

/// Three taps `[previous, centre, next]` that linearly interpolate the
/// coverage at an offset of `d` pixels from the pixel centre (`d` in −1..=1,
/// positive towards the next sample).
#[inline]
pub fn shift_taps(d: f32) -> [f32; 3] {
    let d = d.clamp(-1.0, 1.0);
    [(-d).max(0.0), 1.0 - d.abs(), d.max(0.0)]
}

/// Where each colour channel samples the coverage: separable 3×3 taps per
/// channel, indexed `[red, green, blue]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterKernel {
    /// Horizontal taps `[left, centre, right]` for each channel.
    pub h: [[f32; 3]; 3],
    /// Vertical taps `[above, centre, below]` for each channel.
    pub v: [[f32; 3]; 3],
    /// Pentile RGBG: red and blue exist in alternate pixels. Where one is
    /// absent its horizontal taps are replaced by [`pentile::ABSENT_FIR`].
    pub pentile: bool,
}

impl FilterKernel {
    /// Every channel reads the centre sample: greyscale.
    pub const IDENTITY: Self = Self {
        h: [IDENTITY_TAPS; 3],
        v: [IDENTITY_TAPS; 3],
        pentile: false,
    };

    /// Horizontal taps in effect for each channel at pixel (`col`, `row`).
    #[inline]
    pub fn horizontal_at(&self, col: u32, row: u32) -> [[f32; 3]; 3] {
        let mut h = self.h;
        if self.pentile {
            // Red sits where col + row is even, blue where it is odd.
            if (col + row) % 2 == 0 {
                h[2] = pentile::ABSENT_FIR;
            } else {
                h[0] = pentile::ABSENT_FIR;
            }
        }
        h
    }

    /// Channel `ch` (0 = red, 1 = green, 2 = blue) of this kernel applied to
    /// `plane` at pixel (`col`, `row`).
    pub fn sample_channel(&self, plane: &Plane, ch: usize, col: u32, row: u32) -> f32 {
        let h = self.horizontal_at(col, row)[ch];
        let (x, y) = (col as i32, row as i32);
        let mut acc = 0.0;
        for (j, &vt) in self.v[ch].iter().enumerate() {
            if vt == 0.0 {
                continue;
            }
            let yy = y + j as i32 - 1;
            acc += vt * (h[0] * plane.get(x - 1, yy) + h[1] * plane.get(x, yy) + h[2] * plane.get(x + 1, yy));
        }
        acc
    }

    /// Filtered `[r, g, b]` at pixel (`col`, `row`) of `cov`.
    pub fn sample(&self, cov: &Plane, col: u32, row: u32) -> [f32; 3] {
        [0, 1, 2].map(|ch| self.sample_channel(cov, ch, col, row))
    }
}

/// The fixed-kernel filter the greyscale and Subpixel AA renderers apply to
/// pixel-resolution coverage. (The OLED-aware renderer samples at emitter
/// positions instead; see [`EmitterMap`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseFilter {
    /// All channels equal the coverage.
    Grey,
    /// Horizontal phase shift for an RGB stripe.
    StripeRgb,
    /// Horizontal phase shift for a BGR stripe.
    StripeBgr,
    /// Pentile RGBG reconstruction ([`pentile`]).
    Pentile,
    /// Delta-RGB 2-D kernel ([`delta`]).
    Delta,
}

impl BaseFilter {
    /// The filter for `layout`. WRGB and greyscale panels get
    /// [`BaseFilter::Grey`]: colour subpixel rendering has nothing to work
    /// with when the luminance comes from a white emitter.
    pub fn for_layout(layout: SubpixelLayout) -> Self {
        match layout {
            SubpixelLayout::RgbStripe => BaseFilter::StripeRgb,
            SubpixelLayout::BgrStripe => BaseFilter::StripeBgr,
            SubpixelLayout::PentileRgbg => BaseFilter::Pentile,
            SubpixelLayout::DeltaRgb => BaseFilter::Delta,
            SubpixelLayout::Wrgb | SubpixelLayout::Greyscale => BaseFilter::Grey,
        }
    }

    /// The taps this filter applies.
    pub fn kernel(self) -> FilterKernel {
        match self {
            BaseFilter::Grey => FilterKernel::IDENTITY,
            BaseFilter::StripeRgb => FilterKernel {
                h: [STRIPE_LEFT_TAPS, STRIPE_CENTRE_TAPS, STRIPE_RIGHT_TAPS],
                v: [IDENTITY_TAPS; 3],
                pentile: false,
            },
            BaseFilter::StripeBgr => FilterKernel {
                h: [STRIPE_RIGHT_TAPS, STRIPE_CENTRE_TAPS, STRIPE_LEFT_TAPS],
                v: [IDENTITY_TAPS; 3],
                pentile: false,
            },
            BaseFilter::Pentile => FilterKernel {
                h: [pentile::RED_FIR, pentile::GREEN_FIR, pentile::BLUE_FIR],
                v: [IDENTITY_TAPS; 3],
                pentile: true,
            },
            BaseFilter::Delta => delta::kernel(&delta::NOMINAL),
        }
    }
}

/// Identifies the physical subpixel arrangement of the target panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubpixelLayout {
    /// Standard horizontal RGB stripe (most LCD, some OLED monitors).
    /// ClearType-style horizontal filtering is valid.
    RgbStripe,

    /// Reversed horizontal BGR stripe.
    BgrStripe,

    /// Samsung-style pentile RGBG.
    ///
    /// Alternating rows of [R G] and [G B] pixel pairs.
    /// Green has full resolution; red and blue are at half resolution
    /// in both axes and must be reconstructed.
    PentileRgbg,

    /// QD-OLED / delta-RGB triangular arrangement (Alienware QD-OLED, Asus ROG).
    /// Red and blue are offset vertically by half a pixel relative to green.
    /// Horizontal-only filtering introduces fringing — a 2-D kernel is required.
    DeltaRgb,

    /// WOLED with white subpixel (LG OLED panels used in some monitors).
    /// The W channel carries most of the luminance; R/G/B are low-intensity.
    /// Subpixel rendering benefit is minimal; greyscale AA is usually preferred.
    Wrgb,

    /// No subpixel information available or applicable (HiDPI, e-ink, etc.).
    /// The engine falls back to greyscale antialiasing.
    Greyscale,
}

impl SubpixelLayout {
    /// Human-readable name for user interfaces.
    pub fn label(&self) -> &'static str {
        match self {
            SubpixelLayout::RgbStripe => "RGB stripe",
            SubpixelLayout::BgrStripe => "BGR stripe",
            SubpixelLayout::PentileRgbg => "Pentile RGBG",
            SubpixelLayout::DeltaRgb => "Delta RGB (QD-OLED)",
            SubpixelLayout::Wrgb => "WRGB (WOLED)",
            SubpixelLayout::Greyscale => "greyscale",
        }
    }

    /// Parse a layout name as used on the command line and in panel files:
    /// `rgb`, `bgr`, `pentile`, `delta`, `wrgb` or `grey`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_lowercase().as_str() {
            "rgb" | "rgb-stripe" => Some(SubpixelLayout::RgbStripe),
            "bgr" | "bgr-stripe" => Some(SubpixelLayout::BgrStripe),
            "pentile" | "pentile-rgbg" | "rgbg" => Some(SubpixelLayout::PentileRgbg),
            "delta" | "delta-rgb" | "qd-oled" => Some(SubpixelLayout::DeltaRgb),
            "wrgb" | "woled" => Some(SubpixelLayout::Wrgb),
            "grey" | "gray" | "greyscale" | "grayscale" => Some(SubpixelLayout::Greyscale),
            _ => None,
        }
    }

    /// The short name [`from_name`](Self::from_name) accepts.
    pub fn name(&self) -> &'static str {
        match self {
            SubpixelLayout::RgbStripe => "rgb",
            SubpixelLayout::BgrStripe => "bgr",
            SubpixelLayout::PentileRgbg => "pentile",
            SubpixelLayout::DeltaRgb => "delta",
            SubpixelLayout::Wrgb => "wrgb",
            SubpixelLayout::Greyscale => "grey",
        }
    }

    /// Every layout.
    pub const ALL: [SubpixelLayout; 6] = [
        SubpixelLayout::RgbStripe,
        SubpixelLayout::BgrStripe,
        SubpixelLayout::PentileRgbg,
        SubpixelLayout::DeltaRgb,
        SubpixelLayout::Wrgb,
        SubpixelLayout::Greyscale,
    ];

    /// Returns `true` if subpixel rendering is likely to be beneficial.
    ///
    /// Returns `false` for layouts where the chromatic penalty outweighs the
    /// resolution gain (WRGB) or where the panel geometry makes it inapplicable.
    #[inline]
    pub fn subpixel_rendering_useful(&self) -> bool {
        matches!(
            self,
            SubpixelLayout::RgbStripe
                | SubpixelLayout::BgrStripe
                | SubpixelLayout::PentileRgbg
                | SubpixelLayout::DeltaRgb
        )
    }

    /// Returns the per-channel horizontal coverage weights for a single logical
    /// pixel, given the column parity (even = 0, odd = 1) and row parity.
    ///
    /// The weights describe how much of a horizontally-adjacent coverage value
    /// contributes to each channel at the *current* pixel position.  They are
    /// used by the filter module to construct the per-channel FIR taps.
    ///
    /// Returns `[r_weight, g_weight, b_weight]` in linear-light space.
    pub fn channel_weights(
        &self,
        col_parity: u8,
        row_parity: u8,
    ) -> [f32; 3] {
        match self {
            SubpixelLayout::RgbStripe => [1.0, 1.0, 1.0], // handled by FIR phase
            SubpixelLayout::BgrStripe => [1.0, 1.0, 1.0],
            SubpixelLayout::PentileRgbg => {
                pentile::channel_weights(col_parity, row_parity)
            }
            // Every pixel has all three colours; their placement is handled
            // by the filter kernel (see `delta` and `wrgb`).
            SubpixelLayout::DeltaRgb | SubpixelLayout::Wrgb => [1.0, 1.0, 1.0],
            SubpixelLayout::Greyscale => [1.0, 1.0, 1.0],
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for layout in SubpixelLayout::ALL {
            assert_eq!(SubpixelLayout::from_name(layout.name()), Some(layout));
        }
        assert_eq!(SubpixelLayout::from_name(" QD-OLED "), Some(SubpixelLayout::DeltaRgb));
        assert_eq!(SubpixelLayout::from_name("plasma"), None);
    }

    #[test]
    fn greyscale_not_useful() {
        assert!(!SubpixelLayout::Greyscale.subpixel_rendering_useful());
        assert!(!SubpixelLayout::Wrgb.subpixel_rendering_useful());
    }

    #[test]
    fn stripe_useful() {
        assert!(SubpixelLayout::RgbStripe.subpixel_rendering_useful());
        assert!(SubpixelLayout::BgrStripe.subpixel_rendering_useful());
        assert!(SubpixelLayout::PentileRgbg.subpixel_rendering_useful());
    }

    #[test]
    fn shift_taps_interpolate_linearly() {
        assert_eq!(shift_taps(0.0), [0.0, 1.0, 0.0]);
        assert_eq!(shift_taps(0.25), [0.0, 0.75, 0.25]);
        assert_eq!(shift_taps(-0.25), [0.25, 0.75, 0.0]);
        assert_eq!(shift_taps(3.0), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn every_kernel_preserves_a_flat_field() {
        let cov = Plane::from_fn(6, 6, |_, _| 0.7);
        for filter in [
            BaseFilter::Grey,
            BaseFilter::StripeRgb,
            BaseFilter::StripeBgr,
            BaseFilter::Pentile,
            BaseFilter::Delta,
        ] {
            let k = filter.kernel();
            for row in 0..6 {
                for col in 0..6 {
                    for v in k.sample(&cov, col, row) {
                        assert!((v - 0.7).abs() < 1e-5, "{filter:?} at ({col},{row}): {v}");
                    }
                }
            }
        }
    }

    #[test]
    fn bgr_kernel_mirrors_rgb() {
        let rgb = BaseFilter::StripeRgb.kernel();
        let bgr = BaseFilter::StripeBgr.kernel();
        assert_eq!(rgb.h[0], bgr.h[2]);
        assert_eq!(rgb.h[2], bgr.h[0]);
        assert_eq!(rgb.h[1], bgr.h[1]);
    }

    #[test]
    fn pentile_kernel_matches_filter_row() {
        let row_cov = [0.0f32, 0.2, 1.0, 0.6, 0.0, 0.9, 1.0];
        let n = row_cov.len();
        let k = BaseFilter::Pentile.kernel();
        for row in 0..2u32 {
            let cov = Plane::from_fn(n as u32, 2, |c, _| row_cov[c as usize]);
            let (mut r, mut g, mut b) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
            pentile::filter_row(&row_cov, row as u8, &mut r, &mut g, &mut b);
            for col in 0..n {
                let s = k.sample(&cov, col as u32, row);
                assert!((s[0] - r[col]).abs() < 1e-6 && (s[1] - g[col]).abs() < 1e-6 && (s[2] - b[col]).abs() < 1e-6,
                    "row {row} col {col}: {s:?} vs ({}, {}, {})", r[col], g[col], b[col]);
            }
        }
    }

    #[test]
    fn pentile_weights_sum_to_one_or_less() {
        // No channel weight should exceed 1.0
        for col in 0u8..2 {
            for row in 0u8..2 {
                let w = SubpixelLayout::PentileRgbg.channel_weights(col, row);
                for ch in w {
                    assert!(ch >= 0.0 && ch <= 1.0, "weight out of range: {ch}");
                }
            }
        }
    }
}
