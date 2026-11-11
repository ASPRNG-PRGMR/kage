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
//! 1. **Physical subpixel arrangement** — each colour comes from its own
//!    emitter at its own position and aperture inside the pixel, and some
//!    layouts lack a colour in some pixels. Modelled by [`PanelGeometry`],
//!    painted onto an oversampled emission map by [`panel::emit`].
//!
//! 2. **Optical PSF** — light spreads before it reaches the retina.
//!    Modelled as a separable Gaussian convolution in linear light.
//!
//! 3. **Per-channel bleed** — R, G, B emitters have different aperture sizes
//!    on OLED panels. Red bleeds slightly more than green; blue is in between.
//!    Modelled via per-channel sigma multipliers in [`BleedProfile`].
//!
//! 4. **Luminance reconstruction** — what the eye integrates from the
//!    coloured emitters, computed in linear light ([`gamma`]).
//!
//! Two entry points share those pieces:
//!
//! - [`simulate`] blurs the per-pixel grid directly (no emitter structure).
//! - [`simulate_panel`] places the emitters first, then blurs — the full model.
//!
//! ## What is not yet simulated (Phase 3+)
//!
//! - Angular dependency of the PSF (wider at oblique angles)
//! - Light spilling outside the glyph bitmap: the convolution clamps to the
//!   edge instead of extending into the surrounding background
//! - Measured, panel-specific apertures (the built-in geometries are nominal)
//! - Temporal effects (OLED PWM dimming)
//! - Human visual system MTF / contrast sensitivity function

pub mod bleed;
pub mod gamma;
pub mod panel;
pub mod psf;

pub use bleed::BleedProfile;
pub use gamma::{
    check_linear, chroma_rms, luminance_rms_error, reconstruct_luminance, relative_luminance,
    viewing_distance_to_sigma, DEFAULT_PSF_FWHM_ARCMIN, LUMA_WEIGHTS,
};
pub use panel::{emit, integrate_pixels, Emitter, EmitterChannel, PanelGeometry};
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
    /// - `psf_fwhm_arcmin` is the PSF width in arcminutes at the eye
    ///   (see [`DEFAULT_PSF_FWHM_ARCMIN`])
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

// ── simulate_panel() ──────────────────────────────────────────────────────────

/// Default oversampling factor for [`simulate_panel`]: emission-map samples
/// per logical pixel along each axis.
pub const DEFAULT_OVERSAMPLE: u32 = 6;

/// Simulate what the viewer sees of a rendered grid on a physical panel.
///
/// 1. [`panel::emit`] lights each emitter of `geometry` at its real position,
///    producing an emission map `oversample` times larger than `grid` in each
///    axis.
/// 2. The PSF (with per-channel bleed) is applied to that map. `params.sigma`
///    stays in logical-pixel units and is scaled to map samples internally.
///
/// With `params.sigma <= 0.0` the bare emission map is returned — a view of
/// the subpixel structure with no optical blur.
///
/// The result is in linear light and luminance-preserving: its spatial mean
/// equals that of the input, so individual samples can exceed 1.0. Use
/// [`integrate_pixels`] with the same `oversample` to return to one value per
/// logical pixel.
pub fn simulate_panel(
    grid: &SubpixelGrid,
    geometry: &PanelGeometry,
    params: &SimulationParams,
    oversample: u32,
) -> SubpixelGrid {
    #[cfg(debug_assertions)]
    check_linear(grid, "simulate_panel input");

    let oversample = oversample.max(1);
    let emission = emit(grid, geometry, oversample);

    if params.sigma <= 0.0 {
        return emission;
    }

    let (kr, kg, kb) = params.bleed.kernels(params.sigma * oversample as f32);
    convolve_separable(&emission, &kr, &kg, &kb)
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

    // ── simulate_panel ────────────────────────────────────────────────────

    fn white(w: u32, h: u32) -> SubpixelGrid {
        let mut g = SubpixelGrid::new(w, h);
        for row in 0..h {
            for col in 0..w {
                *g.pixel_mut(col, row) = SubpixelPixel { r: 1.0, g: 1.0, b: 1.0 };
            }
        }
        g
    }

    #[test]
    fn panel_simulation_is_oversampled() {
        let out = simulate_panel(
            &white(4, 3),
            &PanelGeometry::rgb_stripe(),
            &SimulationParams::oled_default(),
            5,
        );
        assert_eq!((out.width, out.height), (20, 15));
    }

    #[test]
    fn panel_simulation_without_blur_is_the_emission_map() {
        let grid = point_grid(4, 4, 1, 1);
        let geometry = PanelGeometry::pentile_rgbg();
        let out = simulate_panel(&grid, &geometry, &SimulationParams::identity(), 6);
        let map = emit(&grid, &geometry, 6);
        for row in 0..out.height {
            for col in 0..out.width {
                assert_eq!(out.pixel(col, row).g, map.pixel(col, row).g);
            }
        }
    }

    #[test]
    fn panel_simulation_preserves_white_on_every_layout() {
        use crate::layout::SubpixelLayout::*;
        const S: u32 = 6;
        for layout in [RgbStripe, BgrStripe, PentileRgbg, DeltaRgb, Wrgb, Greyscale] {
            let out = simulate_panel(
                &white(12, 12),
                &PanelGeometry::for_layout(layout),
                &SimulationParams::oled_default(),
                S,
            );
            // Mean over the central 4×4 pixels: whole unit cells, and far
            // enough from the border that edge handling plays no part.
            let mut acc = [0.0f32; 3];
            for row in 4 * S..8 * S {
                for col in 4 * S..8 * S {
                    let p = out.pixel(col, row);
                    acc[0] += p.r;
                    acc[1] += p.g;
                    acc[2] += p.b;
                }
            }
            let n = (4 * S * 4 * S) as f32;
            let [r, g, b] = [acc[0] / n, acc[1] / n, acc[2] / n];
            assert!(
                (r - 1.0).abs() < 0.01 && (g - 1.0).abs() < 0.01 && (b - 1.0).abs() < 0.01,
                "{layout:?}: mean ({r}, {g}, {b})"
            );
        }
    }

    #[test]
    fn blur_merges_stripe_emitters_towards_white() {
        // Unblurred, a white pixel is three saturated bars. At the default
        // viewing-distance PSF the bars mostly merge: the per-sample channel
        // imbalance must drop sharply.
        let spread = |grid: &SubpixelGrid| {
            let mut worst = 0.0f32;
            // Interior only, away from the clamp-to-edge border.
            for row in grid.height / 3..2 * grid.height / 3 {
                for col in grid.width / 3..2 * grid.width / 3 {
                    let p = grid.pixel(col, row);
                    worst = worst.max((p.r - p.g).abs()).max((p.b - p.g).abs());
                }
            }
            worst
        };
        let geometry = PanelGeometry::rgb_stripe();
        let raw = simulate_panel(&white(9, 9), &geometry, &SimulationParams::identity(), 6);
        let seen = simulate_panel(&white(9, 9), &geometry, &SimulationParams::oled_default(), 6);
        assert!(spread(&seen) < spread(&raw) * 0.2,
            "blur should merge emitters: raw={} seen={}", spread(&raw), spread(&seen));
    }

    #[test]
    fn sigma_is_in_logical_pixels_regardless_of_oversample() {
        // The same sigma at two oversampling factors must give the same
        // per-pixel result once integrated back.
        let grid = point_grid(9, 9, 4, 4);
        let geometry = PanelGeometry::greyscale();
        let params = SimulationParams::strong();
        let a = integrate_pixels(&simulate_panel(&grid, &geometry, &params, 4), 4);
        let b = integrate_pixels(&simulate_panel(&grid, &geometry, &params, 8), 8);
        for (col, row) in [(4u32, 4u32), (5, 4), (6, 4), (4, 6)] {
            let (pa, pb) = (a.pixel(col, row).g, b.pixel(col, row).g);
            assert!((pa - pb).abs() < 0.01, "({col},{row}): {pa} vs {pb}");
        }
    }
}
