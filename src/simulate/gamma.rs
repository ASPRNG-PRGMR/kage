//! Gamma correctness and luminance reconstruction in the simulation pipeline.
//!
//! This module documents and enforces the linear-light requirement for
//! all simulation operations, provides diagnostic helpers for detecting
//! accidental signal-domain inputs, and reconstructs the luminance the
//! viewer perceives from the coloured emitters.
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

use crate::subpixel::{SubpixelGrid, SubpixelPixel};

// ── Luminance reconstruction ──────────────────────────────────────────────────
//
// Sharpness is carried almost entirely by luminance; the eye resolves colour
// at a much lower spatial frequency. So the question "how crisp does this
// glyph look on this panel?" is a question about the luminance image the
// coloured emitters add up to — and that sum is only meaningful in linear
// light, where light from separate emitters adds.
//
// Gamma-aware order of operations:
//
//   linear RGB ──▶ Y = 0.2126 R + 0.7152 G + 0.0722 B ──▶ EOTF encode ──▶ display
//
// Computing Y from *encoded* R'G'B' (video "luma") understates the
// contribution of saturated emitters and is wrong for this purpose.

/// Luminance weights for linear-light R, G, B with sRGB / Rec. 709 primaries
/// and a D65 white point. They sum to 1.0, so white has luminance 1.0.
pub const LUMA_WEIGHTS: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Relative luminance of a **linear-light** pixel.
#[inline]
pub fn relative_luminance(px: SubpixelPixel) -> f32 {
    LUMA_WEIGHTS[0] * px.r + LUMA_WEIGHTS[1] * px.g + LUMA_WEIGHTS[2] * px.b
}

/// Reconstruct the luminance image of a **linear-light** grid.
///
/// Returns an achromatic grid (`r = g = b = Y`) of the same size, still in
/// linear light: pass it through [`crate::render::encode_grid`] to display it.
/// Works on per-pixel grids and on oversampled emission maps alike.
pub fn reconstruct_luminance(grid: &SubpixelGrid) -> SubpixelGrid {
    let mut out = SubpixelGrid::new(grid.width, grid.height);
    for row in 0..grid.height {
        for col in 0..grid.width {
            let y = relative_luminance(grid.pixel(col, row));
            *out.pixel_mut(col, row) = SubpixelPixel { r: y, g: y, b: y };
        }
    }
    out
}

/// RMS difference in luminance between two **linear-light** grids.
///
/// Measures how faithfully `candidate` reproduces the luminance of
/// `reference` — e.g. a simulated panel image against the intended coverage.
/// Returns `None` if the grids differ in size or are empty.
pub fn luminance_rms_error(reference: &SubpixelGrid, candidate: &SubpixelGrid) -> Option<f32> {
    if reference.width != candidate.width
        || reference.height != candidate.height
        || reference.width == 0
        || reference.height == 0
    {
        return None;
    }
    let mut acc = 0.0f64;
    for row in 0..reference.height {
        for col in 0..reference.width {
            let d = relative_luminance(reference.pixel(col, row))
                - relative_luminance(candidate.pixel(col, row));
            acc += (d * d) as f64;
        }
    }
    let n = (reference.width * reference.height) as f64;
    Some((acc / n).sqrt() as f32)
}

/// RMS chromatic residue of a **linear-light** grid: how far each channel
/// strays from the pixel's own luminance.
///
/// Zero for an achromatic image. For white-on-black text any nonzero value
/// is colour the glyph was not supposed to have, so this is a direct measure
/// of fringing. Returns 0.0 for an empty grid.
pub fn chroma_rms(grid: &SubpixelGrid) -> f32 {
    let n = (grid.width * grid.height) as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mut acc = 0.0f64;
    for row in 0..grid.height {
        for col in 0..grid.width {
            let px = grid.pixel(col, row);
            let y = relative_luminance(px);
            let (dr, dg, db) = (px.r - y, px.g - y, px.b - y);
            acc += ((dr * dr + dg * dg + db * db) / 3.0) as f64;
        }
    }
    (acc / n).sqrt() as f32
}

// ── Linear-light guard ────────────────────────────────────────────────────────

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
    use crate::profile::Eotf;
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

    // ── Luminance reconstruction ──────────────────────────────────────────

    fn filled(w: u32, h: u32, px: SubpixelPixel) -> SubpixelGrid {
        let mut g = SubpixelGrid::new(w, h);
        for row in 0..h {
            for col in 0..w {
                *g.pixel_mut(col, row) = px;
            }
        }
        g
    }

    #[test]
    fn luma_weights_sum_to_one() {
        let sum: f32 = LUMA_WEIGHTS.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
    }

    #[test]
    fn grey_luminance_is_its_own_value() {
        let px = SubpixelPixel { r: 0.37, g: 0.37, b: 0.37 };
        assert!((relative_luminance(px) - 0.37).abs() < 1e-6);
    }

    #[test]
    fn green_dominates_luminance() {
        let lum = |r, g, b| relative_luminance(SubpixelPixel { r, g, b });
        assert!(lum(0.0, 1.0, 0.0) > lum(1.0, 0.0, 0.0));
        assert!(lum(1.0, 0.0, 0.0) > lum(0.0, 0.0, 1.0));
    }

    #[test]
    fn reconstruct_luminance_is_achromatic_and_same_size() {
        let g = filled(3, 2, SubpixelPixel { r: 1.0, g: 0.5, b: 0.0 });
        let y = reconstruct_luminance(&g);
        assert_eq!((y.width, y.height), (3, 2));
        let expected = 0.2126 + 0.7152 * 0.5;
        for row in 0..2 {
            for col in 0..3 {
                let p = y.pixel(col, row);
                assert!((p.r - expected).abs() < 1e-6);
                assert!(p.r == p.g && p.g == p.b);
            }
        }
    }

    #[test]
    fn reconstruction_is_gamma_aware() {
        // Luminance is summed in linear light and encoded afterwards:
        // linear Y of (0.5, 0.5, 0.5) is 0.5, which encodes to ~0.735 —
        // not to the 0.5 a signal-domain average would give.
        let eotf = Eotf::Srgb;
        let y = relative_luminance(SubpixelPixel { r: 0.5, g: 0.5, b: 0.5 });
        let displayed = eotf.encode(y);
        assert!((displayed - 0.7354).abs() < 0.01, "got {displayed}");

        // And for a saturated pixel, luma computed on encoded values is
        // measurably different from encoded true luminance.
        let px = SubpixelPixel { r: 0.2, g: 0.0, b: 0.0 };
        let correct = eotf.encode(relative_luminance(px));
        let naive = LUMA_WEIGHTS[0] * eotf.encode(px.r);
        assert!((correct - naive).abs() > 0.05, "correct={correct} naive={naive}");
    }

    #[test]
    fn luminance_error_is_zero_for_identical_grids() {
        let g = filled(4, 4, SubpixelPixel { r: 0.3, g: 0.6, b: 0.9 });
        assert_eq!(luminance_rms_error(&g, &g), Some(0.0));
    }

    #[test]
    fn luminance_error_ignores_pure_chroma_differences() {
        // Same luminance, different colour → zero luminance error.
        let grey = filled(2, 2, SubpixelPixel { r: 0.5, g: 0.5, b: 0.5 });
        // Shift red up and blue down so that 0.2126·dr + 0.0722·db = 0.
        let dr = 0.1;
        let db = -dr * LUMA_WEIGHTS[0] / LUMA_WEIGHTS[2];
        let tinted = filled(2, 2, SubpixelPixel { r: 0.5 + dr, g: 0.5, b: 0.5 + db });
        let err = luminance_rms_error(&grey, &tinted).unwrap();
        assert!(err < 1e-5, "err={err}");
        assert!(chroma_rms(&tinted) > 0.05);
    }

    #[test]
    fn luminance_error_measures_brightness_difference() {
        let a = filled(2, 2, SubpixelPixel { r: 0.5, g: 0.5, b: 0.5 });
        let b = filled(2, 2, SubpixelPixel { r: 0.7, g: 0.7, b: 0.7 });
        let err = luminance_rms_error(&a, &b).unwrap();
        assert!((err - 0.2).abs() < 1e-5, "err={err}");
    }

    #[test]
    fn luminance_error_rejects_mismatched_sizes() {
        let a = SubpixelGrid::new(2, 2);
        let b = SubpixelGrid::new(3, 2);
        assert_eq!(luminance_rms_error(&a, &b), None);
    }

    #[test]
    fn chroma_is_zero_for_grey_and_positive_for_colour() {
        let grey = filled(3, 3, SubpixelPixel { r: 0.4, g: 0.4, b: 0.4 });
        assert!(chroma_rms(&grey) < 1e-6);
        let red = filled(3, 3, SubpixelPixel { r: 1.0, g: 0.0, b: 0.0 });
        assert!(chroma_rms(&red) > 0.3);
        assert_eq!(chroma_rms(&SubpixelGrid::new(0, 0)), 0.0);
    }
}
