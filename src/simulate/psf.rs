//! Optical Point Spread Function (PSF) simulation.
//!
//! Every physical display emitter spreads light beyond its nominal pixel
//! boundary. The PSF describes the spatial distribution of that light.
//! For OLED panels, the PSF is tighter than LCD (no backlight scatter)
//! but nonzero — neighbouring subpixels receive measurable energy.
//!
//! We model the PSF as a separable Gaussian, which is a good approximation
//! for the diffraction-limited optics of a modern panel at normal viewing
//! distance. Separability means we can decompose the 2-D convolution into
//! two 1-D passes (horizontal then vertical), which is O(n·k) instead of
//! O(n·k²) for a full 2-D kernel.
//!
//! # Physical parameterisation
//!
//! Sigma is derived from the panel's pixel pitch, the viewing distance, and
//! the angular width of the PSF at the viewer's eye
//! (see [`super::gamma::viewing_distance_to_sigma`]):
//!
//! ```text
//! fwhm_mm      = fwhm_radians × viewing_distance_mm     (small-angle)
//! fwhm_pixels  = fwhm_mm / pixel_pitch_mm
//! sigma_pixels = fwhm_pixels / (2√(2 ln 2))             (FWHM → sigma)
//! ```
//!
//! A 109 dpi panel (0.233 mm pitch) at 50 cm with a 1.7 arcmin PSF gives
//! sigma ≈ 0.45 pixels — a subtle but physically meaningful blur.  Because the
//! PSF is a fixed angle, sigma *in pixels* grows with viewing distance and
//! with pixel density.
//!
//! # Correctness requirement
//!
//! **Convolution must happen in linear light.** Blurring a gamma-encoded signal
//! produces incorrect results because the nonlinear curve makes dark regions
//! appear to carry less energy than they physically do. All inputs to
//! `convolve_separable` must be linear-light [`SubpixelGrid`]s.

use crate::subpixel::{SubpixelGrid, SubpixelPixel};

// ── Kernel builder ────────────────────────────────────────────────────────────

/// Build a 1-D normalised Gaussian kernel of half-width `radius` taps.
///
/// The full kernel has `2 * radius + 1` elements and sums to 1.0.
/// Larger sigma → wider kernel → more blur.
///
/// `radius` is clamped to at least 1. A good rule: `radius = ceil(3 * sigma)`.
pub fn gaussian_kernel(sigma: f32, radius: usize) -> Vec<f32> {
    let radius = radius.max(1);
    let len = 2 * radius + 1;
    let mut k = vec![0.0f32; len];
    let s2 = 2.0 * sigma * sigma;

    for i in 0..len {
        let x = i as f32 - radius as f32;
        k[i] = (-x * x / s2).exp();
    }

    // Normalise so the kernel sums to 1.0 (energy-preserving).
    let sum: f32 = k.iter().sum();
    if sum > 1e-8 {
        for v in k.iter_mut() {
            *v /= sum;
        }
    }

    k
}

/// Recommended kernel radius for a given sigma: `ceil(3 * sigma)`, min 1.
pub fn kernel_radius(sigma: f32) -> usize {
    ((3.0 * sigma).ceil() as usize).max(1)
}

// ── Separable 2-D convolution ─────────────────────────────────────────────────

/// How the convolution treats taps that fall outside the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    /// Repeat the nearest edge sample. Keeps a uniform field uniform, and
    /// suits per-pixel grids where the edge value is a fair guess at what
    /// lies beyond.
    Clamp,
    /// Treat everything outside as black. Correct for an emission map of a
    /// glyph on a dark background: repeating an edge sample there would
    /// smear a lit emitter outward into a false bright streak.
    Zero,
}

/// Apply a separable Gaussian PSF to a linear-light [`SubpixelGrid`], with
/// clamp-to-edge borders. See [`convolve_separable_with`].
pub fn convolve_separable(
    grid: &SubpixelGrid,
    kernel_r: &[f32],
    kernel_g: &[f32],
    kernel_b: &[f32],
) -> SubpixelGrid {
    convolve_separable_with(grid, kernel_r, kernel_g, kernel_b, Border::Clamp)
}

/// Apply a separable Gaussian PSF to a linear-light [`SubpixelGrid`].
///
/// Two 1-D passes are performed: horizontal (along rows) then vertical
/// (along columns). Each channel (R, G, B) can have a different kernel,
/// allowing per-channel blur widths to model the different emitter apertures
/// of R, G, and B subpixels on OLED panels.
///
/// # Arguments
/// - `grid`      — input grid in **linear light**
/// - `kernel_r`  — 1-D Gaussian kernel for the red channel
/// - `kernel_g`  — 1-D Gaussian kernel for the green channel
/// - `kernel_b`  — 1-D Gaussian kernel for the blue channel
/// - `border`    — how taps outside the grid are handled
///
/// Returns a new grid with the blur applied. The input grid is not modified.
///
/// # Panics
/// All kernels must have odd length (2*radius+1). Panics otherwise.
pub fn convolve_separable_with(
    grid: &SubpixelGrid,
    kernel_r: &[f32],
    kernel_g: &[f32],
    kernel_b: &[f32],
    border: Border,
) -> SubpixelGrid {
    assert!(kernel_r.len() % 2 == 1, "kernel_r must have odd length");
    assert!(kernel_g.len() % 2 == 1, "kernel_g must have odd length");
    assert!(kernel_b.len() % 2 == 1, "kernel_b must have odd length");

    let w = grid.width;
    let h = grid.height;

    // ── Pass 1: horizontal ────────────────────────────────────────────────
    let mut horiz = SubpixelGrid::new(w, h);

    for row in 0..h {
        for col in 0..w {
            let r = convolve1d_channel(grid, row, col, kernel_r, Channel::R, true, border);
            let g = convolve1d_channel(grid, row, col, kernel_g, Channel::G, true, border);
            let b = convolve1d_channel(grid, row, col, kernel_b, Channel::B, true, border);
            *horiz.pixel_mut(col, row) = SubpixelPixel { r, g, b };
        }
    }

    // ── Pass 2: vertical ──────────────────────────────────────────────────
    let mut vert = SubpixelGrid::new(w, h);

    for row in 0..h {
        for col in 0..w {
            let r = convolve1d_channel(&horiz, row, col, kernel_r, Channel::R, false, border);
            let g = convolve1d_channel(&horiz, row, col, kernel_g, Channel::G, false, border);
            let b = convolve1d_channel(&horiz, row, col, kernel_b, Channel::B, false, border);
            *vert.pixel_mut(col, row) = SubpixelPixel { r, g, b };
        }
    }

    vert
}

// ── Internal helpers ──────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Channel { R, G, B }

/// Apply a 1-D kernel to one channel at position (row, col).
///
/// `horizontal = true`  → convolve along the row (vary col).
/// `horizontal = false` → convolve along the column (vary row).
fn convolve1d_channel(
    grid: &SubpixelGrid,
    row: u32,
    col: u32,
    kernel: &[f32],
    channel: Channel,
    horizontal: bool,
    border: Border,
) -> f32 {
    let radius = (kernel.len() / 2) as i32;
    let (pos, len) = if horizontal {
        (col as i32, grid.width as i32)
    } else {
        (row as i32, grid.height as i32)
    };
    let mut acc = 0.0f32;

    for (ki, &kv) in kernel.iter().enumerate() {
        let mut p = pos + ki as i32 - radius;
        if p < 0 || p >= len {
            match border {
                Border::Zero => continue,
                Border::Clamp => p = p.clamp(0, len - 1),
            }
        }

        let px = if horizontal {
            grid.pixel(p as u32, row)
        } else {
            grid.pixel(col, p as u32)
        };
        let v = match channel {
            Channel::R => px.r,
            Channel::G => px.g,
            Channel::B => px.b,
        };
        acc += v * kv;
    }

    acc
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subpixel::{SubpixelGrid, SubpixelPixel};

    fn uniform_grid(w: u32, h: u32, val: f32) -> SubpixelGrid {
        let mut g = SubpixelGrid::new(w, h);
        for row in 0..h {
            for col in 0..w {
                *g.pixel_mut(col, row) = SubpixelPixel { r: val, g: val, b: val };
            }
        }
        g
    }

    #[test]
    fn gaussian_kernel_sums_to_one() {
        for &sigma in &[0.5f32, 1.0, 2.0, 3.0] {
            let r = kernel_radius(sigma);
            let k = gaussian_kernel(sigma, r);
            let sum: f32 = k.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "kernel sum={sum} for sigma={sigma}");
        }
    }

    #[test]
    fn gaussian_kernel_is_symmetric() {
        let k = gaussian_kernel(1.5, 4);
        let n = k.len();
        for i in 0..n / 2 {
            assert!((k[i] - k[n - 1 - i]).abs() < 1e-6,
                "asymmetry at i={i}: {} vs {}", k[i], k[n-1-i]);
        }
    }

    #[test]
    fn uniform_input_unchanged_after_blur() {
        // Convolving a uniform field with any normalised kernel gives the same value.
        let grid = uniform_grid(16, 16, 0.6);
        let k = gaussian_kernel(1.0, 3);
        let out = convolve_separable(&grid, &k, &k, &k);
        for row in 0..16 {
            for col in 0..16 {
                let px = out.pixel(col, row);
                assert!((px.r - 0.6).abs() < 1e-4,
                    "uniform field changed at ({col},{row}): r={}", px.r);
            }
        }
    }

    #[test]
    fn impulse_blurs_to_gaussian_shape() {
        // Single bright pixel at centre should spread to Gaussian profile.
        let mut grid = SubpixelGrid::new(11, 11);
        *grid.pixel_mut(5, 5) = SubpixelPixel { r: 1.0, g: 1.0, b: 1.0 };

        let sigma = 1.0;
        let k = gaussian_kernel(sigma, kernel_radius(sigma));
        let out = convolve_separable(&grid, &k, &k, &k);

        // Centre should be brightest.
        let centre = out.pixel(5, 5).r;
        let neighbour = out.pixel(6, 5).r;
        let far = out.pixel(8, 5).r;
        assert!(centre > neighbour, "centre ({centre}) should be > neighbour ({neighbour})");
        assert!(neighbour > far, "neighbour ({neighbour}) should be > far ({far})");
        assert!(centre > 0.0, "centre should be positive after blur");
    }

    #[test]
    fn energy_is_conserved_after_blur() {
        // Sum of all pixel values should be the same before and after (normalised kernel).
        let mut grid = SubpixelGrid::new(16, 16);
        *grid.pixel_mut(8, 8) = SubpixelPixel { r: 1.0, g: 1.0, b: 1.0 };

        let k = gaussian_kernel(1.5, 4);
        let out = convolve_separable(&grid, &k, &k, &k);

        let sum_in: f32 = (0..16u32).flat_map(|r| (0..16u32).map(move |c| (r, c)))
            .map(|(r, c)| { let p = grid.pixel(c, r); p.r + p.g + p.b })
            .sum();
        let sum_out: f32 = (0..16u32).flat_map(|r| (0..16u32).map(move |c| (r, c)))
            .map(|(r, c)| { let p = out.pixel(c, r); p.r + p.g + p.b })
            .sum();

        assert!((sum_in - sum_out).abs() < 0.01,
            "energy not conserved: in={sum_in:.4} out={sum_out:.4}");
    }

    #[test]
    fn zero_border_does_not_smear_edge_samples() {
        // A lit sample on the left edge. Clamp repeats it outward, so the
        // edge keeps far more light than it physically would; Zero lets the
        // light that leaves the grid go.
        let mut grid = SubpixelGrid::new(9, 1);
        *grid.pixel_mut(0, 0) = SubpixelPixel { r: 1.0, g: 1.0, b: 1.0 };
        let k = gaussian_kernel(1.5, 4);

        let clamp = convolve_separable_with(&grid, &k, &k, &k, Border::Clamp);
        let zero = convolve_separable_with(&grid, &k, &k, &k, Border::Zero);
        assert!(clamp.pixel(0, 0).r > zero.pixel(0, 0).r * 1.5,
            "clamp={} zero={}", clamp.pixel(0, 0).r, zero.pixel(0, 0).r);
    }

    #[test]
    fn zero_border_matches_clamp_away_from_edges() {
        let mut grid = SubpixelGrid::new(21, 21);
        *grid.pixel_mut(10, 10) = SubpixelPixel { r: 1.0, g: 0.5, b: 0.25 };
        let k = gaussian_kernel(1.0, 3);
        let clamp = convolve_separable_with(&grid, &k, &k, &k, Border::Clamp);
        let zero = convolve_separable_with(&grid, &k, &k, &k, Border::Zero);
        for (col, row) in [(10u32, 10u32), (11, 10), (10, 12), (0, 0)] {
            assert!((clamp.pixel(col, row).r - zero.pixel(col, row).r).abs() < 1e-6);
        }
    }

    #[test]
    fn zero_border_never_gains_energy() {
        let grid = uniform_grid(8, 8, 1.0);
        let k = gaussian_kernel(1.5, 4);
        let out = convolve_separable_with(&grid, &k, &k, &k, Border::Zero);
        for row in 0..8 {
            for col in 0..8 {
                assert!(out.pixel(col, row).g <= 1.0 + 1e-5);
            }
        }
        // Interior is untouched, corners have lost light past two edges.
        assert!(out.pixel(0, 0).g < 0.5);
    }
}
