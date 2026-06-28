//! Per-channel subpixel light bleed model.
//!
//! On OLED panels, R, G, and B emitters have physically different aperture
//! sizes. This results in different amounts of optical bleed per channel:
//!
//! - **Green** emitters are smallest and most efficient — tightest PSF.
//! - **Red** emitters have a larger aperture — wider PSF, more bleed.
//! - **Blue** emitters are smallest in area but lower efficiency means
//!   manufacturers sometimes use larger apertures — slightly wider than green.
//!
//! These ratios are approximations derived from published OLED panel teardowns
//! and characterisation papers. The `BleedProfile` struct exposes them as
//! multipliers on a base sigma, allowing calibration per panel.
//!
//! ## Usage
//!
//! ```ignore
//! let profile = BleedProfile::oled_default();
//! let (kr, kg, kb) = profile.kernels(base_sigma);
//! let blurred = convolve_separable(&grid, &kr, &kg, &kb);
//! ```

use super::psf::{gaussian_kernel, kernel_radius};

/// Per-channel PSF width multipliers relative to a base sigma.
///
/// A multiplier of 1.0 means this channel uses exactly the base sigma.
/// Values > 1.0 produce a wider PSF (more bleed) for that channel.
#[derive(Debug, Clone)]
pub struct BleedProfile {
    /// Sigma multiplier for the red channel.
    pub red_mult: f32,
    /// Sigma multiplier for the green channel.
    pub green_mult: f32,
    /// Sigma multiplier for the blue channel.
    pub blue_mult: f32,
}

impl BleedProfile {
    /// Conservative OLED default: red bleeds slightly more than green,
    /// blue is between the two.
    ///
    /// Derived from characterisation of Samsung AMOLED panels.
    /// These values are starting points for research, not definitive constants.
    pub fn oled_default() -> Self {
        Self {
            red_mult:   1.20,
            green_mult: 1.00,
            blue_mult:  1.10,
        }
    }

    /// All channels identical — useful as a baseline / control condition.
    pub fn uniform() -> Self {
        Self {
            red_mult:   1.0,
            green_mult: 1.0,
            blue_mult:  1.0,
        }
    }

    /// Build per-channel Gaussian kernels from a base sigma.
    ///
    /// Returns `(kernel_r, kernel_g, kernel_b)`, each a normalised 1-D
    /// Gaussian kernel ready to pass to [`super::psf::convolve_separable`].
    pub fn kernels(&self, base_sigma: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let sigma_r = base_sigma * self.red_mult;
        let sigma_g = base_sigma * self.green_mult;
        let sigma_b = base_sigma * self.blue_mult;

        let kr = gaussian_kernel(sigma_r, kernel_radius(sigma_r));
        let kg = gaussian_kernel(sigma_g, kernel_radius(sigma_g));
        let kb = gaussian_kernel(sigma_b, kernel_radius(sigma_b));

        (kr, kg, kb)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oled_default_red_wider_than_green() {
        let p = BleedProfile::oled_default();
        assert!(p.red_mult > p.green_mult,
            "red should bleed more than green on OLED");
    }

    #[test]
    fn kernels_all_sum_to_one() {
        let p = BleedProfile::oled_default();
        let (kr, kg, kb) = p.kernels(1.0);
        let sr: f32 = kr.iter().sum();
        let sg: f32 = kg.iter().sum();
        let sb: f32 = kb.iter().sum();
        assert!((sr - 1.0).abs() < 1e-5, "R kernel sum={sr}");
        assert!((sg - 1.0).abs() < 1e-5, "G kernel sum={sg}");
        assert!((sb - 1.0).abs() < 1e-5, "B kernel sum={sb}");
    }

    #[test]
    fn uniform_profile_all_equal_kernels() {
        let p = BleedProfile::uniform();
        let (kr, kg, kb) = p.kernels(1.5);
        assert_eq!(kr.len(), kg.len());
        assert_eq!(kg.len(), kb.len());
        for i in 0..kr.len() {
            assert!((kr[i] - kg[i]).abs() < 1e-6);
            assert!((kg[i] - kb[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn red_kernel_wider_than_green_kernel() {
        let p = BleedProfile::oled_default();
        let (kr, kg, _) = p.kernels(1.0);
        // Wider kernel has more taps.
        assert!(kr.len() >= kg.len(),
            "red kernel should be at least as wide as green");
    }
}
