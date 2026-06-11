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
//! 2. Implement a module under `layout/` (see `pentile.rs` as the reference).
//! 3. Register in [`SubpixelLayout::filter_weights`] and [`SubpixelLayout::channel_offsets`].

pub mod pentile;

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
            // Delta and WRGB — 2-D kernels, placeholder until those modules land
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
