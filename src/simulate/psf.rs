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
//! Sigma is derived from the panel's pixel pitch and the viewing distance:
//!
//! ```text
//! angular_pixel = pixel_pitch_mm / viewing_distance_mm   (radians, small-angle)
//! sigma_pixels  = psf_fwhm / (2√(2 ln 2))               (FWHM → sigma conversion)
//! ```
//!
//! A typical SDR OLED at 50 cm viewing distance and 0.27 mm pixel pitch gives
//! sigma ≈ 0.4–0.6 pixels, producing a subtle but physically meaningful blur.
//! Increasing viewing distance decreases the apparent PSF width.
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
///
/// Returns a new grid with the blur applied. The input grid is not modified.
///
/// # Panics
/// All kernels must have odd length (2*radius+1). Panics otherwise.
pub fn convolve_separable(
    grid: &SubpixelGrid,
    kernel_r: &[f32],
    kernel_g: &[f32],
    kernel_b: &[f32],
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
            let r = convolve1d_channel(grid, row, col, kernel_r, Channel::R, true);
            let g = convolve1d_channel(grid, row, col, kernel_g, Channel::G, true);
            let b = convolve1d_channel(grid, row, col, kernel_b, Channel::B, true);
            *horiz.pixel_mut(col, row) = SubpixelPixel { r, g, b };
        }
    }

    // ── Pass 2: vertical ──────────────────────────────────────────────────
    let mut vert = SubpixelGrid::new(w, h);

    for row in 0..h {
        for col in 0..w {
            let r = convolve1d_channel(&horiz, row, col, kernel_r, Channel::R, false);
            let g = convolve1d_channel(&horiz, row, col, kernel_g, Channel::G, false);
            let b = convolve1d_channel(&horiz, row, col, kernel_b, Channel::B, false);
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
///
/// Border handling: clamp-to-edge (repeat the border pixel value).
fn convolve1d_channel(
    grid: &SubpixelGrid,
    row: u32,
    col: u32,
    kernel: &[f32],
    channel: Channel,
    horizontal: bool,
) -> f32 {
    let radius = (kernel.len() / 2) as i32;
    let w = grid.width as i32;
    let h = grid.height as i32;
    let mut acc = 0.0f32;

    for (ki, &kv) in kernel.iter().enumerate() {
        let offset = ki as i32 - radius;

        let (sc, sr) = if horizontal {
            ((col as i32 + offset).clamp(0, w - 1) as u32, row)
        } else {
            (col, (row as i32 + offset).clamp(0, h - 1) as u32)
        };

        let px = grid.pixel(sc, sr);
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
}
