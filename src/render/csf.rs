//! Contrast sensitivity of the eye, and the filter widths derived from it.
//!
//! How much detail the eye resolves depends on spatial frequency, measured
//! in cycles per degree of visual angle (cpd), and it differs sharply
//! between brightness and colour:
//!
//! - **Luminance** sensitivity is band-pass. It peaks around 8 cpd and is
//!   still useful past 30 cpd. Fine luminance detail is what reads as
//!   "sharp".
//! - **Chromatic** sensitivity is low-pass. It is highest for broad areas of
//!   colour and falls away quickly; colour detail finer than roughly ten cpd
//!   is not seen as colour at all — it averages to grey.
//!
//! Subpixel rendering lives in that gap. Driving one colour emitter harder
//! than its neighbours moves luminance by a fraction of a pixel, at the cost
//! of pixel-scale colour error. Where that colour error is fine-grained it
//! is invisible; where it adds up over a few pixels it is a visible fringe.
//!
//! KAGE uses this in two places:
//!
//! - the **visible chroma** metric
//!   ([`crate::simulate::visible_chroma_rms`]) low-passes the simulated
//!   image's colour with the chromatic filter defined here before measuring
//!   it, so only fringing the eye would see is counted;
//! - **auto-tuning** ([`crate::pipeline::Pipeline::auto_tune`]) minimises
//!   that metric together with luminance error, which matches the renderer's
//!   footprints to the eye at the current viewing distance.
//!
//! The curves below are standard engineering approximations, not
//! measurements of any particular observer.

use crate::profile::DisplayProfile;

/// Frequency of peak luminance sensitivity in [`luminance_csf`], in cpd.
pub const LUMA_PEAK_CPD: f32 = 8.0;

/// Default frequency at which chromatic sensitivity has halved, in cpd.
/// A nominal value; `--chroma-cutoff` changes it.
pub const CHROMA_HALF_CPD: f32 = 4.0;

/// Logical pixels per degree of visual angle at the centre of the screen.
pub fn pixels_per_degree(dpi: f32, viewing_distance_mm: f32) -> f32 {
    let pitch_mm = 25.4 / dpi;
    let mm_per_degree = 2.0 * viewing_distance_mm * (0.5f32.to_radians()).tan();
    mm_per_degree / pitch_mm
}

/// Relative luminance contrast sensitivity at `f` cpd (Mannos & Sakrison's
/// 1974 model): band-pass, about 1.0 at its peak near 8 cpd.
pub fn luminance_csf(f: f32) -> f32 {
    let f = f.max(0.0);
    2.6 * (0.0192 + 0.114 * f) * (-(0.114 * f).powf(1.1)).exp()
}

/// Relative chromatic contrast sensitivity at `f` cpd: a Gaussian low-pass
/// equal to 1.0 at zero and 0.5 at `half_cpd`.
pub fn chroma_csf(f: f32, half_cpd: f32) -> f32 {
    if half_cpd <= 0.0 {
        return if f <= 0.0 { 1.0 } else { 0.0 };
    }
    0.5f32.powf((f / half_cpd).powi(2))
}

/// Standard deviation, in degrees, of the spatial Gaussian whose frequency
/// response falls to one half at `half_cpd`.
///
/// A Gaussian of deviation σ has response `exp(−2π²σ²f²)`; setting that to
/// ½ gives `σ = sqrt(ln 2 / 2) / (π f)`.
pub fn gaussian_sigma_deg(half_cpd: f32) -> f32 {
    (std::f32::consts::LN_2 / 2.0).sqrt() / (std::f32::consts::PI * half_cpd.max(1e-3))
}

/// The same deviation in logical pixels on `profile`'s panel seen from
/// `viewing_distance_mm`.
pub fn gaussian_sigma_px(half_cpd: f32, profile: &DisplayProfile, viewing_distance_mm: f32) -> f32 {
    gaussian_sigma_deg(half_cpd) * pixels_per_degree(profile.dpi, viewing_distance_mm)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_per_degree_for_a_desktop_monitor() {
        // 109 DPI at 500 mm: 0.233 mm pixels, 8.73 mm per degree.
        let ppd = pixels_per_degree(109.0, 500.0);
        assert!((ppd - 37.45).abs() < 0.1, "got {ppd}");
        // Twice as far, twice as many pixels per degree.
        assert!((pixels_per_degree(109.0, 1000.0) / ppd - 2.0).abs() < 1e-3);
    }

    #[test]
    fn luminance_csf_is_band_pass_peaking_near_eight_cpd() {
        let peak = (1..600)
            .map(|i| i as f32 * 0.1)
            .max_by(|a, b| luminance_csf(*a).total_cmp(&luminance_csf(*b)))
            .unwrap();
        assert!((peak - LUMA_PEAK_CPD).abs() < 1.0, "peak at {peak} cpd");
        assert!((luminance_csf(peak) - 1.0).abs() < 0.05);
        assert!(luminance_csf(0.5) < 0.5 * luminance_csf(peak));
        assert!(luminance_csf(40.0) < 0.2 * luminance_csf(peak));
    }

    #[test]
    fn chroma_csf_is_low_pass_and_halves_at_the_cutoff() {
        assert_eq!(chroma_csf(0.0, 4.0), 1.0);
        assert!((chroma_csf(4.0, 4.0) - 0.5).abs() < 1e-6);
        assert!(chroma_csf(12.0, 4.0) < 0.01);
        assert!(chroma_csf(1.0, 4.0) > chroma_csf(2.0, 4.0));
    }

    #[test]
    fn chroma_falls_off_long_before_luminance() {
        // At 16 cpd luminance is still near its peak; colour is gone.
        assert!(luminance_csf(16.0) > 0.6);
        assert!(chroma_csf(16.0, CHROMA_HALF_CPD) < 1e-3);
    }

    #[test]
    fn gaussian_sigma_gives_half_response_at_the_cutoff() {
        let f = 4.0f32;
        let sigma = gaussian_sigma_deg(f);
        let response = (-2.0 * std::f32::consts::PI.powi(2) * sigma * sigma * f * f).exp();
        assert!((response - 0.5).abs() < 1e-5);
    }

    #[test]
    fn sigma_in_pixels_grows_with_distance_and_density() {
        let near = gaussian_sigma_px(4.0, &DisplayProfile::sdr_oled(), 400.0);
        let far = gaussian_sigma_px(4.0, &DisplayProfile::sdr_oled(), 800.0);
        assert!((far / near - 2.0).abs() < 1e-3);
        let mut dense = DisplayProfile::sdr_oled();
        dense.dpi *= 2.0;
        assert!((gaussian_sigma_px(4.0, &dense, 400.0) / near - 2.0).abs() < 1e-3);
        // Default case: a visible colour fringe is a couple of pixels wide.
        let default = gaussian_sigma_px(CHROMA_HALF_CPD, &DisplayProfile::sdr_oled(), 500.0);
        assert!(default > 1.0 && default < 3.0, "got {default}");
    }
}
