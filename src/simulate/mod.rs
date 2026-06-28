//! Display simulation — optical blur and per-channel subpixel bleed.
//!
//! Phase 2 of KAGE models what a real OLED panel physically does to the
//! rendered subpixel energy before it reaches the viewer's eye.
//!
//! ## Pipeline position
//!
//! ```text
//! render()               → linear SubpixelGrid
//!     │
//! simulate()             → linear SubpixelGrid  (blur applied in linear light)
//!     │
//! render::encode_grid()  → signal SubpixelGrid  (EOTF encoding, once, last)
//!     │
//! viz::Inspector         → framebuffer
//! ```
//!
//! ## What is simulated
//!
//! 1. **Optical PSF** — each emitter spreads light to neighbouring pixels.
//!    Modelled as a separable Gaussian convolution in linear light.
//!
//! 2. **Per-channel bleed** — R, G, B emitters have different aperture sizes
//!    on OLED panels. Red bleeds slightly more than green; blue is in between.
//!    Modelled via per-channel sigma multipliers in [`BleedProfile`].
//!
//! ## What is not yet simulated (Phase 3+)
//!
//! - Angular dependency of the PSF (wider at oblique angles)
//! - Panel-specific subpixel geometry corrections
//! - Temporal effects (OLED PWM dimming)
//! - Human visual system MTF / contrast sensitivity function

pub mod bleed;
pub mod gamma;
pub mod psf;

pub use bleed::BleedProfile;
pub use gamma::{check_linear, viewing_distance_to_sigma};
pub use psf::{convolve_separable, gaussian_kernel, kernel_radius};

use crate::profile::DisplayProfile;
use crate::subpixel::SubpixelGrid;

// ── SimulationParams ──────────────────────────────────────────────────────────

/// Parameters controlling the optical blur simulation.
#[derive(Debug, Clone)]
pub struct SimulationParams {
    /// Base PSF sigma in pixel units.
    ///
    /// A good physical starting point for a 109 dpi OLED at 50 cm viewing
    /// distance is ~0.45 pixels. Use `viewing_distance_to_sigma()` to derive
    /// this from physical measurements.
    ///
    /// Set to 0.0 to disable blur (identity passthrough).
    pub sigma: f32,

    /// Per-channel bleed profile.
    ///
    /// Use [`BleedProfile::oled_default()`] for OLED panels.
    /// Use [`BleedProfile::uniform()`] for a channel-independent baseline.
    pub bleed: BleedProfile,
}

impl SimulationParams {
    /// Physically-derived default for a ~109 dpi SDR OLED at 50 cm.
    pub fn oled_default() -> Self {
        Self {
            sigma: 0.45,
            bleed: BleedProfile::oled_default(),
        }
    }

    /// Minimal blur — useful for comparing almost-raw output.
    pub fn subtle() -> Self {
        Self {
            sigma: 0.25,
            bleed: BleedProfile::uniform(),
        }
    }

    /// Strong blur — exaggerates the PSF for visual demonstration.
    pub fn strong() -> Self {
        Self {
            sigma: 1.2,
            bleed: BleedProfile::oled_default(),
        }
    }

    /// Identity — no blur, passes through unchanged. Useful for A/B toggle.
    pub fn identity() -> Self {
        Self {
            sigma: 0.0,
            bleed: BleedProfile::uniform(),
        }
    }

    /// Derive sigma from physical display and viewing parameters.
    ///
    /// - `profile.dpi` is used to compute pixel pitch
    /// - `viewing_distance_mm` is the eye-to-panel distance
    /// - `psf_fwhm_arcmin` is the desired PSF width in arcminutes
    pub fn from_viewing_distance(
        profile: &DisplayProfile,
        viewing_distance_mm: f32,
        psf_fwhm_arcmin: f32,
    ) -> Self {
        let pixel_pitch_mm = 25.4 / profile.dpi;
        let sigma = viewing_distance_to_sigma(pixel_pitch_mm, viewing_distance_mm, psf_fwhm_arcmin);
        Self {
            sigma,
            bleed: BleedProfile::oled_default(),
        }
    }
}

// ── simulate() ────────────────────────────────────────────────────────────────

/// Apply optical blur simulation to a linear-light [`SubpixelGrid`].
///
/// Returns a new grid with the PSF convolution applied.
/// The input grid is not modified.
///
/// If `params.sigma` is 0.0 or below, the input is returned unchanged
/// (identity passthrough — useful for the raw/simulated toggle).
///
/// # Correctness
///
/// The input **must** be in linear light. In debug builds, `check_linear`
/// is called and emits a warning if the values look suspicious.
pub fn simulate(grid: &SubpixelGrid, params: &SimulationParams) -> SubpixelGrid {
    #[cfg(debug_assertions)]
    check_linear(grid, "simulate input");

    if params.sigma <= 0.0 {
        return grid.clone();
    }

    let (kr, kg, kb) = params.bleed.kernels(params.sigma);
    convolve_separable(grid, &kr, &kg, &kb)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subpixel::{SubpixelGrid, SubpixelPixel};

    fn point_grid(w: u32, h: u32, cx: u32, cy: u32) -> SubpixelGrid {
        let mut g = SubpixelGrid::new(w, h);
        *g.pixel_mut(cx, cy) = SubpixelPixel { r: 1.0, g: 1.0, b: 1.0 };
        g
    }

    #[test]
    fn identity_params_passthrough() {
        let grid = point_grid(8, 8, 4, 4);
        let out = simulate(&grid, &SimulationParams::identity());
        // Should be identical
        for row in 0..8 {
            for col in 0..8 {
                let a = grid.pixel(col, row);
                let b = out.pixel(col, row);
                assert!((a.r - b.r).abs() < 1e-6 && (a.g - b.g).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn simulate_spreads_energy() {
        let grid = point_grid(16, 16, 8, 8);
        let params = SimulationParams::oled_default();
        let out = simulate(&grid, &params);

        // Neighbours should now have nonzero energy.
        let centre = out.pixel(8, 8).g;
        let neighbour = out.pixel(9, 8).g;
        assert!(centre > 0.0, "centre should have energy");
        assert!(neighbour > 0.0, "neighbour should receive bleed");
        assert!(centre > neighbour, "centre should be brightest");
    }

    #[test]
    fn strong_blur_spreads_further_than_subtle() {
        let grid = point_grid(32, 32, 16, 16);
        let subtle = simulate(&grid, &SimulationParams::subtle());
        let strong = simulate(&grid, &SimulationParams::strong());

        // At distance 5 from centre, strong should have more energy than subtle.
        let subtle_far = subtle.pixel(21, 16).g;
        let strong_far = strong.pixel(21, 16).g;
        assert!(strong_far >= subtle_far,
            "strong blur should spread further: strong={strong_far} subtle={subtle_far}");
    }

    #[test]
    fn oled_red_channel_wider_than_green() {
        // With oled_default bleed, red should be more spread than green.
        let mut grid = SubpixelGrid::new(32, 32);
        *grid.pixel_mut(16, 16) = SubpixelPixel { r: 1.0, g: 1.0, b: 0.0 };

        let params = SimulationParams::oled_default();
        let out = simulate(&grid, &params);

        // At distance 3 from centre, red should be >= green (wider PSF).
        let r_far = out.pixel(19, 16).r;
        let g_far = out.pixel(19, 16).g;
        assert!(r_far >= g_far,
            "red should bleed at least as much as green: r={r_far} g={g_far}");
    }
}
