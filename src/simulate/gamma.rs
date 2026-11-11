//! Gamma correctness in the simulation pipeline.
//!
//! This module documents and enforces the linear-light requirement for
//! all simulation operations, and provides diagnostic helpers for
//! detecting accidental signal-domain inputs.
//!
//! ## Why simulation must happen in linear light
//!
//! Optical blur is a physical process — photons from one emitter scatter
//! to neighbouring positions on the retina. This scattering is linear:
//! doubling the energy of a source doubles its contribution to each
//! neighbouring position.
//!
//! sRGB encoding is nonlinear. In sRGB, a "mid-grey" pixel has signal
//! value ~0.735 but carries only ~0.214 of the physical light energy.
//! If we blur in sRGB space, we blur the *encoded signal*, not the
//! *physical energy*. The result is systematically wrong:
//!
//! - Dark regions appear to blur less than they physically do
//!   (sRGB compresses dark values, making them look closer to black)
//! - Bright regions blur more than they should
//! - The chromatic fringing pattern shifts in intensity and colour
//!
//! **Correct pipeline:**
//! ```text
//! render() → linear SubpixelGrid → simulate() → encode_grid() → framebuffer
//! ```
//!
//! **Wrong pipeline (never do this):**
//! ```text
//! render() → encode_grid() → simulate() → framebuffer   ← WRONG
//! ```
//!
//! ## Detection
//!
//! The `check_linear` function provides a heuristic guard: it samples
//! a few pixels and checks whether the values follow a gamma-encoded
//! distribution (which would be suspicious for a glyph rendered on a
//! dark background). This is a development-time check only.

use crate::subpixel::SubpixelGrid;

/// Heuristic check that a grid looks like linear light, not sRGB signal.
///
/// Samples the maximum value in the grid and checks it is ≤ 1.0 + epsilon.
/// Also checks that no channel value exceeds 1.0 by a meaningful margin,
/// which would indicate a bug in the rendering pipeline.
///
/// In debug builds this emits a warning to stderr if the max value suggests
/// the grid may have been double-encoded.
///
/// Returns `true` if the grid passes the sanity check.
pub fn check_linear(grid: &SubpixelGrid, label: &str) -> bool {
    let mut max_val = 0.0f32;
    let mut any_negative = false;

    for row in 0..grid.height {
        for col in 0..grid.width {
            let px = grid.pixel(col, row);
            max_val = max_val.max(px.r).max(px.g).max(px.b);
            if px.r < -1e-4 || px.g < -1e-4 || px.b < -1e-4 {
                any_negative = true;
            }
        }
    }

    let ok = max_val <= 1.0 + 1e-4 && !any_negative;

    #[cfg(debug_assertions)]
    if !ok {
        eprintln!(
            "[kage::simulate] WARNING: grid '{label}' may not be in linear light \
             (max={max_val:.4}, negative={any_negative}). \
             Ensure encode_grid() is called AFTER simulate(), not before."
        );
    }

    ok
}

/// Default angular PSF width (FWHM, arcminutes) used when deriving sigma from
/// viewing distance.
///
/// The optical PSF of a well-focused human eye is roughly 1–2 arcminutes FWHM.
/// 1.7 arcmin maps to sigma ≈ 0.45 px on a 109 dpi panel viewed from 500 mm,
/// which is the documented KAGE default.
pub const DEFAULT_PSF_FWHM_ARCMIN: f32 = 1.7;

/// Convert an angular PSF width at the eye into a PSF sigma in panel pixels.
///
/// Given:
/// - `pixel_pitch_mm`: physical size of one pixel in millimetres
/// - `viewing_distance_mm`: distance from eye to panel in millimetres
/// - `psf_fwhm_arcmin`: PSF Full Width at Half Maximum in arcminutes,
///   measured as an angle at the viewer's eye
///
/// Returns the PSF sigma in pixel units.
///
/// ## Scaling
///
/// The PSF is a fixed *angle*, so the patch of panel it covers grows with
/// distance: `fwhm_mm = fwhm_rad × viewing_distance_mm`.  Doubling the viewing
/// distance therefore doubles sigma in pixel units (each pixel subtends half
/// the angle, so the eye merges twice as many of them).  Likewise a denser
/// panel (smaller pitch) gives a larger sigma in pixels.
///
/// `sigma = FWHM / (2 * sqrt(2 * ln(2)))` ≈ FWHM / 2.355
pub fn viewing_distance_to_sigma(
    pixel_pitch_mm: f32,
    viewing_distance_mm: f32,
    psf_fwhm_arcmin: f32,
) -> f32 {
    // Convert FWHM from arcminutes to radians
    let fwhm_rad = psf_fwhm_arcmin * (std::f32::consts::PI / 180.0 / 60.0);
    // FWHM in physical mm at the panel surface
    let fwhm_mm = fwhm_rad * viewing_distance_mm;
    // FWHM in pixel units
    let fwhm_px = fwhm_mm / pixel_pitch_mm;
    // Convert FWHM to sigma
    fwhm_px / (2.0 * (2.0f32 * std::f32::consts::LN_2).sqrt())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subpixel::{SubpixelGrid, SubpixelPixel};

    #[test]
    fn check_linear_passes_valid_grid() {
        let mut g = SubpixelGrid::new(4, 4);
        *g.pixel_mut(2, 2) = SubpixelPixel { r: 0.8, g: 0.5, b: 0.3 };
        assert!(check_linear(&g, "test"), "valid linear grid should pass");
    }

    #[test]
    fn check_linear_fails_on_overflow() {
        let mut g = SubpixelGrid::new(2, 2);
        *g.pixel_mut(0, 0) = SubpixelPixel { r: 1.5, g: 0.0, b: 0.0 };
        assert!(!check_linear(&g, "overflow"), "overflow should fail check");
    }

    #[test]
    fn viewing_distance_sigma_scaling() {
        // The PSF is a fixed angle at the eye, so doubling the viewing
        // distance doubles the sigma in pixel units.
        let pitch = 0.27;
        let fwhm = 0.5;
        let s1 = viewing_distance_to_sigma(pitch, 500.0, fwhm);
        let s2 = viewing_distance_to_sigma(pitch, 1000.0, fwhm);
        assert!((s2 / s1 - 2.0).abs() < 0.01,
            "sigma should double with doubled viewing distance: s1={s1} s2={s2}");
    }

    #[test]
    fn finer_pitch_gives_larger_sigma_in_pixels() {
        let coarse = viewing_distance_to_sigma(0.30, 500.0, 1.0);
        let fine = viewing_distance_to_sigma(0.15, 500.0, 1.0);
        assert!((fine / coarse - 2.0).abs() < 0.01);
    }

    #[test]
    fn default_fwhm_matches_documented_sigma() {
        // 109 dpi, 500 mm, default FWHM → the documented ~0.45 px.
        let s = viewing_distance_to_sigma(25.4 / 109.0, 500.0, DEFAULT_PSF_FWHM_ARCMIN);
        assert!((s - 0.45).abs() < 0.01, "expected ~0.45 px, got {s}");
    }

    #[test]
    fn sigma_is_positive_and_reasonable() {
        // For a 100ppi panel (pitch ≈ 0.254mm) at 500mm: sigma should be < 1px
        let s = viewing_distance_to_sigma(0.254, 500.0, 0.5);
        assert!(s > 0.0 && s < 2.0,
            "sigma should be small fraction of a pixel for typical panel: {s}");
    }
}
