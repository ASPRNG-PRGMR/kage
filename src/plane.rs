//! [`Plane`] — a single-channel `f32` image with clamp-to-edge sampling.
//!
//! The layout filters and the perceptual stage work on scalar fields
//! (coverage, luminance, one chroma component). A `Plane` is that field.
//! Reads outside the image return the nearest edge value, which is what the
//! GPU backends do too, so the CPU and GPU paths agree at the borders.

use crate::glyph::GlyphBuffer;

/// Largest Gaussian radius, in samples, used by [`Plane::blur`]. The GPU
/// shaders loop over the same bound.
pub const MAX_BLUR_RADIUS: usize = 32;

/// A `width × height` grid of `f32`, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Plane {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

impl Plane {
    /// A zero-filled plane.
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0.0; (width * height) as usize] }
    }

    /// The coverage (alpha) channel of a glyph buffer.
    pub fn from_coverage(buf: &GlyphBuffer) -> Self {
        Self {
            width: buf.width,
            height: buf.height,
            data: buf.pixels.chunks_exact(4).map(|px| px[3]).collect(),
        }
    }

    /// Build a plane by evaluating `f(col, row)` at every sample.
    pub fn from_fn(width: u32, height: u32, mut f: impl FnMut(u32, u32) -> f32) -> Self {
        let mut data = Vec::with_capacity((width * height) as usize);
        for row in 0..height {
            for col in 0..width {
                data.push(f(col, row));
            }
        }
        Self { width, height, data }
    }

    /// Sample at (`x`, `y`), clamping coordinates to the image. An empty
    /// plane reads as zero.
    #[inline]
    pub fn get(&self, x: i32, y: i32) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.data[y * self.width as usize + x]
    }

    /// Separable Gaussian blur with clamp-to-edge borders.
    ///
    /// `sigma` is in samples; `sigma <= 0` returns the plane unchanged.
    pub fn blur(&self, sigma: f32) -> Plane {
        let kernel = blur_kernel(sigma);
        if kernel.len() == 1 {
            return self.clone();
        }
        let radius = (kernel.len() / 2) as i32;
        let pass = |src: &Plane, dx: i32, dy: i32| {
            Plane::from_fn(src.width, src.height, |col, row| {
                kernel
                    .iter()
                    .enumerate()
                    .map(|(i, &k)| {
                        let o = i as i32 - radius;
                        k * src.get(col as i32 + o * dx, row as i32 + o * dy)
                    })
                    .sum()
            })
        };
        pass(&pass(self, 1, 0), 0, 1)
    }
}

/// Radius, in samples, of the Gaussian used for `sigma`: three sigma,
/// capped at [`MAX_BLUR_RADIUS`]; zero when there is no blur.
pub fn blur_radius(sigma: f32) -> usize {
    if sigma > 0.0 {
        ((3.0 * sigma).ceil() as usize).clamp(1, MAX_BLUR_RADIUS)
    } else {
        0
    }
}

/// Normalised Gaussian kernel for `sigma`, of length `2 * blur_radius + 1`.
/// For `sigma <= 0` this is the identity kernel `[1.0]`.
pub fn blur_kernel(sigma: f32) -> Vec<f32> {
    let radius = blur_radius(sigma) as i32;
    if radius == 0 {
        return vec![1.0];
    }
    let weights: Vec<f32> = (-radius..=radius)
        .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = weights.iter().sum();
    weights.into_iter().map(|w| w / sum).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_clamps_to_the_edge() {
        let p = Plane::from_fn(3, 2, |c, r| (r * 3 + c) as f32);
        assert_eq!(p.get(-5, 0), 0.0);
        assert_eq!(p.get(7, 0), 2.0);
        assert_eq!(p.get(1, -1), 1.0);
        assert_eq!(p.get(2, 9), 5.0);
    }

    #[test]
    fn empty_plane_reads_zero() {
        assert_eq!(Plane::new(0, 0).get(0, 0), 0.0);
    }

    #[test]
    fn kernel_is_normalised_and_symmetric() {
        for sigma in [0.3f32, 1.0, 2.5, 40.0] {
            let k = blur_kernel(sigma);
            assert_eq!(k.len() % 2, 1);
            assert!(k.len() <= 2 * MAX_BLUR_RADIUS + 1);
            assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-5);
            for i in 0..k.len() / 2 {
                assert!((k[i] - k[k.len() - 1 - i]).abs() < 1e-7);
            }
        }
    }

    #[test]
    fn zero_sigma_is_identity() {
        let p = Plane::from_fn(4, 4, |c, r| (c * r) as f32);
        assert_eq!(p.blur(0.0), p);
        assert_eq!(blur_kernel(-1.0), vec![1.0]);
    }

    #[test]
    fn blur_keeps_a_flat_field_flat() {
        let p = Plane::from_fn(9, 7, |_, _| 0.6);
        for v in p.blur(1.7).data {
            assert!((v - 0.6).abs() < 1e-5);
        }
    }

    #[test]
    fn blur_spreads_an_impulse_symmetrically() {
        let p = Plane::from_fn(11, 11, |c, r| if c == 5 && r == 5 { 1.0 } else { 0.0 });
        let b = p.blur(1.0);
        assert!(b.get(5, 5) < 1.0 && b.get(5, 5) > b.get(4, 5));
        assert!((b.get(4, 5) - b.get(6, 5)).abs() < 1e-6);
        assert!((b.get(5, 4) - b.get(5, 6)).abs() < 1e-6);
        assert!((b.data.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn from_coverage_reads_alpha() {
        let mut buf = GlyphBuffer::new(2, 1);
        buf.pixel_mut(1, 0)[3] = 0.25;
        let p = Plane::from_coverage(&buf);
        assert_eq!(p.data, vec![0.0, 0.25]);
    }
}
