//! Pentile RGBG subpixel geometry and filter kernels.
//!
//! # Layout
//!
//! Samsung pentile RGBG arranges subpixels in a 2×2 repeating unit:
//!
//! ```text
//! Column:   0   1   2   3
//! Row 0:  [ R G R G R G … ]   ← even rows: R at even cols, G at odd cols
//! Row 1:  [ G B G B G B … ]   ← odd  rows: G at even cols, B at odd cols
//! Row 2:  [ R G R G R G … ]
//! Row 3:  [ G B G B G B … ]
//! ```
//!
//! Consequences:
//! - **Green**: present at every (col, row) position → full luminance resolution.
//! - **Red**: only at (even col, even row) → ½ horizontal, ½ vertical.
//! - **Blue**: only at (odd col, odd row) → ½ horizontal, ½ vertical.
//!
//! To produce a per-logical-pixel RGBA value we must:
//! 1. Assign full coverage to the green channel at every position.
//! 2. Reconstruct red and blue from their neighbours using a 2×2 box filter.
//! 3. Apply a chromatic penalty that attenuates R/B at high-contrast edges to
//!    suppress colour fringing.

/// Returns `[r_weight, g_weight, b_weight]` describing which physical subpixels
/// are present at the given (`col_parity`, `row_parity`) position.
///
/// A weight of `1.0` means the subpixel is physically present at this position
/// (no reconstruction needed).  A weight of `0.0` means it is absent and must
/// be interpolated from neighbours.
///
/// | row_parity | col_parity | R   | G   | B   |
/// |------------|-----------|-----|-----|-----|
/// |   even     |   even    | 1.0 | 0.0 | 0.0 |
/// |   even     |   odd     | 0.0 | 1.0 | 0.0 |
/// |   odd      |   even    | 0.0 | 1.0 | 0.0 |
/// |   odd      |   odd     | 0.0 | 0.0 | 1.0 |
///
/// Note: green is present at *all* positions (half are directly driven, the
/// other half are reconstructed — but with trivial weight since the G subpixel
/// at odd-col even-row and even-col odd-row positions carries the luminance).
/// For rendering purposes, we treat green as weight 1.0 everywhere.
#[inline]
pub fn channel_weights(col_parity: u8, row_parity: u8) -> [f32; 3] {
    match (col_parity & 1, row_parity & 1) {
        (0, 0) => [1.0, 1.0, 0.0], // R present, G from neighbour
        (1, 0) => [0.0, 1.0, 0.0], // G present only
        (0, 1) => [0.0, 1.0, 0.0], // G present only
        (1, 1) => [0.0, 1.0, 1.0], // B present, G from neighbour
        _ => unreachable!(),
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

/// Apply per-channel FIR filtering to a horizontal strip of coverage values.
///
/// # Parameters
/// - `coverage`: linear-light coverage slice, one value per logical pixel column.
///   Must have at least 3 elements; the first and last columns are not written
///   (they lack a full 3-tap neighbourhood).
/// - `row_parity`: 0 for even rows, 1 for odd rows.  Selects which subpixels
///   are physically present and adjusts reconstruction accordingly.
/// - `out_r`, `out_g`, `out_b`: output slices of the same length as `coverage`.
///   Values at index 0 and `len-1` are left unchanged.
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
    if n < 3 {
        return;
    }

    for i in 1..n - 1 {
        let col_parity = (i & 1) as u8;
        let [rw, _gw, bw] = channel_weights(col_parity, row_parity);

        let [cl, cc, cr] = [coverage[i - 1], coverage[i], coverage[i + 1]];

        // Green: full-res, gentle sharpening FIR
        out_g[i] = cl * GREEN_FIR[0] + cc * GREEN_FIR[1] + cr * GREEN_FIR[2];

        // Red: half-res reconstruction.
        // When `rw == 1.0` this position has a physical R subpixel — use it
        // directly.  When `rw == 0.0` the R subpixel is absent; interpolate
        // from the nearest R positions (which are at ±1 columns).
        if rw > 0.5 {
            // Physical R subpixel present — mild filter
            out_r[i] = cl * RED_FIR[0] + cc * RED_FIR[1] + cr * RED_FIR[2];
        } else {
            // R absent — average neighbours (they have R subpixels)
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
