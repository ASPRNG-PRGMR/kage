//! Pentile RGBG subpixel geometry and filter kernels.
//!
//! # Layout
//!
//! Samsung pentile RGBG gives every logical pixel two emitters: a green one,
//! plus either a red or a blue one.  Red and blue alternate in a checkerboard,
//! so the repeating unit is 2×2 logical pixels:
//!
//! ```text
//! Column:    0      1      2      3
//! Row 0:   [R G]  [B G]  [R G]  [B G]
//! Row 1:   [B G]  [R G]  [B G]  [R G]
//! Row 2:   [R G]  [B G]  [R G]  [B G]
//! ```
//!
//! Consequences:
//! - **Green**: present in every pixel → full luminance resolution.
//! - **Red**: only where `col + row` is even → half the pixel count.
//! - **Blue**: only where `col + row` is odd → half the pixel count.
//! - A pixel that lacks red always has red in both horizontal neighbours
//!   (and likewise for blue), which is what the reconstruction below relies on.
//!
//! To produce a per-logical-pixel RGB value we must:
//! 1. Assign full coverage to the green channel at every position.
//! 2. Reconstruct red and blue at the pixels that lack them from the
//!    horizontal neighbours that have them.
//! 3. Apply a chromatic penalty that attenuates R/B at high-contrast edges to
//!    suppress colour fringing.
//!
//! The physical emitter positions and apertures for this layout live in
//! [`crate::simulate::panel`].

/// Returns `[r_weight, g_weight, b_weight]` describing which physical subpixels
/// are present at the given (`col_parity`, `row_parity`) position.
///
/// A weight of `1.0` means the subpixel is physically present in this pixel
/// (no reconstruction needed).  A weight of `0.0` means it is absent and must
/// be interpolated from neighbours.
///
/// | row_parity | col_parity | R   | G   | B   |
/// |------------|-----------|-----|-----|-----|
/// |   even     |   even    | 1.0 | 1.0 | 0.0 |
/// |   even     |   odd     | 0.0 | 1.0 | 1.0 |
/// |   odd      |   even    | 0.0 | 1.0 | 1.0 |
/// |   odd      |   odd     | 1.0 | 1.0 | 0.0 |
#[inline]
pub fn channel_weights(col_parity: u8, row_parity: u8) -> [f32; 3] {
    if (col_parity ^ row_parity) & 1 == 0 {
        [1.0, 1.0, 0.0] // R + G pixel
    } else {
        [0.0, 1.0, 1.0] // B + G pixel
    }
}

/// 3-tap FIR filter weights for the green channel.
///
/// Green is at full horizontal resolution; a mild low-pass prevents ringing
/// while preserving the sharpness advantage.  The kernel is symmetric and
/// sums to 1.0.
///
/// Taps: `[left, centre, right]`
pub const GREEN_FIR: [f32; 3] = [0.11, 0.78, 0.11];

/// 3-tap FIR filter weights for the red channel.
///
/// Red is at half horizontal resolution.  The kernel is wider to reconstruct
/// missing samples from neighbours.  Sums to 1.0.
pub const RED_FIR: [f32; 3] = [0.25, 0.50, 0.25];

/// 3-tap FIR filter weights for the blue channel.
///
/// Identical shape to red — blue has the same geometry (half-res, staggered).
pub const BLUE_FIR: [f32; 3] = [0.25, 0.50, 0.25];

/// Taps for red or blue in a pixel that has no emitter of that colour: the
/// mean of the two horizontal neighbours, which both have one.
pub const ABSENT_FIR: [f32; 3] = [0.5, 0.0, 0.5];

/// Apply per-channel FIR filtering to a horizontal strip of coverage values.
///
/// # Parameters
/// - `coverage`: linear-light coverage slice, one value per logical pixel column.
/// - `row_parity`: 0 for even rows, 1 for odd rows.  Selects which subpixels
///   are physically present and adjusts reconstruction accordingly.
/// - `out_r`, `out_g`, `out_b`: output slices of the same length as `coverage`.
///
/// Every column is written.  Taps that fall outside the row use clamp-to-edge
/// (the border value is repeated), so glyphs of any width — including 1- and
/// 2-pixel-wide ones — are filtered rather than left black.
///
/// All values are in linear light; the caller applies EOTF encoding afterwards.
pub fn filter_row(
    coverage: &[f32],
    row_parity: u8,
    out_r: &mut [f32],
    out_g: &mut [f32],
    out_b: &mut [f32],
) {
    assert_eq!(coverage.len(), out_r.len());
    assert_eq!(coverage.len(), out_g.len());
    assert_eq!(coverage.len(), out_b.len());

    let n = coverage.len();

    for i in 0..n {
        let col_parity = (i & 1) as u8;
        let [rw, _gw, bw] = channel_weights(col_parity, row_parity);

        let cl = coverage[i.saturating_sub(1)];
        let cc = coverage[i];
        let cr = coverage[(i + 1).min(n - 1)];

        // Green: full-res, gentle sharpening FIR
        out_g[i] = cl * GREEN_FIR[0] + cc * GREEN_FIR[1] + cr * GREEN_FIR[2];

        // Red: half-res reconstruction.
        // When `rw == 1.0` this pixel has a physical R subpixel — filter it
        // directly.  When `rw == 0.0` the R subpixel is absent; interpolate
        // from the horizontal neighbours, which both have one.
        if rw > 0.5 {
            out_r[i] = cl * RED_FIR[0] + cc * RED_FIR[1] + cr * RED_FIR[2];
        } else {
            out_r[i] = (cl + cr) * 0.5;
        }

        // Blue: symmetric to red
        if bw > 0.5 {
            out_b[i] = cl * BLUE_FIR[0] + cc * BLUE_FIR[1] + cr * BLUE_FIR[2];
        } else {
            out_b[i] = (cl + cr) * 0.5;
        }
    }
}

/// Estimate the chromatic penalty for a pixel at position `i` in a coverage row.
///
/// The penalty is a value in [0.0, 1.0] where:
/// - 0.0 → no penalty (smooth region, subpixel rendering is safe)
/// - 1.0 → maximum penalty (high-contrast edge, suppress chromatic components)
///
/// # Algorithm
///
/// We use the magnitude of the second-order finite difference (discrete Laplacian)
/// of the coverage as a proxy for edge sharpness.  At a hard edge the coverage
/// jumps from 0 → 1 over 1-2 pixels, giving a large Laplacian.  In smooth
/// strokes (rounded bowls, diagonals) the transition is gentler.
///
/// This is intentionally cheap — O(1) per pixel, no heap allocation — since it
/// runs inside the inner loop of the filter pass.
#[inline]
pub fn chromatic_penalty(coverage: &[f32], i: usize) -> f32 {
    if i == 0 || i >= coverage.len() - 1 {
        return 0.0;
    }
    // Discrete Laplacian: |c[i-1] - 2·c[i] + c[i+1]|
    let laplacian = (coverage[i - 1] - 2.0 * coverage[i] + coverage[i + 1]).abs();
    // Normalise: max possible Laplacian for a 0/1 step is 2.0
    (laplacian / 2.0).clamp(0.0, 1.0)
}

/// Blend subpixel-rendered channels with a greyscale fallback based on the
/// chromatic penalty.
///
/// At `penalty = 0.0` the output is purely subpixel-rendered (maximum sharpness).
/// At `penalty = 1.0` the output collapses to the greyscale coverage value
/// in all three channels (no colour fringing).
///
/// `grey` is typically the output of `out_g` (green carries luminance),
/// or a luminance-weighted mix of `r + g + b`.
#[inline]
pub fn adaptive_blend(r: f32, g: f32, b: f32, grey: f32, penalty: f32) -> [f32; 3] {
    let t = penalty.clamp(0.0, 1.0);
    [
        r + (grey - r) * t,
        g + (grey - g) * t,
        b + (grey - b) * t,
    ]
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_kernels_sum_to_one() {
        let sum_g: f32 = GREEN_FIR.iter().sum();
        let sum_r: f32 = RED_FIR.iter().sum();
        let sum_b: f32 = BLUE_FIR.iter().sum();
        assert!((sum_g - 1.0).abs() < 1e-6);
        assert!((sum_r - 1.0).abs() < 1e-6);
        assert!((sum_b - 1.0).abs() < 1e-6);
    }

    #[test]
    fn uniform_coverage_passes_through() {
        // Uniform 0.5 coverage → all channels should be 0.5 after filtering
        let cov = vec![0.5f32; 8];
        let mut r = vec![0.0f32; 8];
        let mut g = vec![0.0f32; 8];
        let mut b = vec![0.0f32; 8];
        filter_row(&cov, 0, &mut r, &mut g, &mut b);
        for i in 1..7 {
            assert!((r[i] - 0.5).abs() < 1e-5, "R[{i}] = {}", r[i]);
            assert!((g[i] - 0.5).abs() < 1e-5, "G[{i}] = {}", g[i]);
            assert!((b[i] - 0.5).abs() < 1e-5, "B[{i}] = {}", b[i]);
        }
    }

    #[test]
    fn chromatic_penalty_zero_on_flat() {
        let cov = vec![0.7f32; 8];
        for i in 1..7 {
            assert_eq!(chromatic_penalty(&cov, i), 0.0);
        }
    }

    #[test]
    fn chromatic_penalty_high_on_step_edge() {
        // Hard 0 → 1 step
        let cov = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        // Position 3 (the step pixel) should have a high penalty
        let p = chromatic_penalty(&cov, 3);
        assert!(p > 0.4, "expected high penalty at edge, got {p}");
    }

    #[test]
    fn adaptive_blend_at_zero_penalty_is_subpixel() {
        let [r, g, b] = adaptive_blend(0.8, 0.5, 0.2, 0.5, 0.0);
        assert!((r - 0.8).abs() < 1e-6);
        assert!((g - 0.5).abs() < 1e-6);
        assert!((b - 0.2).abs() < 1e-6);
    }

    #[test]
    fn adaptive_blend_at_full_penalty_is_grey() {
        let grey = 0.5;
        let [r, g, b] = adaptive_blend(0.8, 0.5, 0.2, grey, 1.0);
        assert!((r - grey).abs() < 1e-6);
        assert!((g - grey).abs() < 1e-6);
        assert!((b - grey).abs() < 1e-6);
    }

    #[test]
    fn red_and_blue_form_a_checkerboard() {
        // Every pixel has exactly one of R / B, and horizontal and vertical
        // neighbours always have the other one.
        for col in 0u8..2 {
            for row in 0u8..2 {
                let [r, _g, b] = channel_weights(col, row);
                assert_eq!(r + b, 1.0, "exactly one of R/B at ({col},{row})");
                let [r_h, _, _] = channel_weights(col ^ 1, row);
                let [r_v, _, _] = channel_weights(col, row ^ 1);
                assert_ne!(r, r_h, "R must alternate horizontally");
                assert_ne!(r, r_v, "R must alternate vertically");
            }
        }
    }

    #[test]
    fn border_columns_are_filtered_not_left_black() {
        let cov = vec![0.8f32; 6];
        for parity in 0u8..2 {
            let (mut r, mut g, mut b) = (vec![0.0f32; 6], vec![0.0f32; 6], vec![0.0f32; 6]);
            filter_row(&cov, parity, &mut r, &mut g, &mut b);
            for i in [0usize, 5] {
                assert!((r[i] - 0.8).abs() < 1e-5, "R[{i}] = {}", r[i]);
                assert!((g[i] - 0.8).abs() < 1e-5, "G[{i}] = {}", g[i]);
                assert!((b[i] - 0.8).abs() < 1e-5, "B[{i}] = {}", b[i]);
            }
        }
    }

    #[test]
    fn narrow_rows_are_filtered() {
        // 1- and 2-pixel-wide glyphs (e.g. '|', '.') must not come out black.
        for n in 1usize..=2 {
            let cov = vec![1.0f32; n];
            let (mut r, mut g, mut b) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
            filter_row(&cov, 0, &mut r, &mut g, &mut b);
            for i in 0..n {
                assert!((r[i] - 1.0).abs() < 1e-5 && (g[i] - 1.0).abs() < 1e-5 && (b[i] - 1.0).abs() < 1e-5,
                    "width {n} col {i}: ({}, {}, {})", r[i], g[i], b[i]);
            }
        }
    }

    #[test]
    fn channel_weights_coverage() {
        // Every position should always have G weight = 1.0
        for col in 0u8..2 {
            for row in 0u8..2 {
                let [_r, g, _b] = channel_weights(col, row);
                assert_eq!(g, 1.0, "G weight should always be 1.0");
            }
        }
    }
}
