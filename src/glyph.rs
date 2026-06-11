//! [`GlyphBuffer`] — the output type produced by the rendering pipeline.
//!
//! A `GlyphBuffer` holds a single glyph's rendered pixels as packed RGBA bytes
//! in linear-light space (before EOTF encoding).  The caller encodes to the
//! target signal domain using the display profile before blending into a
//! framebuffer.

/// A rendered glyph as a flat RGBA pixel buffer.
///
/// Pixels are stored in row-major order, left-to-right, top-to-bottom.
/// Each pixel is four `f32` values: `[R, G, B, A]` in linear light, [0.0, 1.0].
///
/// The alpha channel holds the greyscale coverage value (used for compositing).
/// R/G/B hold the subpixel-rendered per-channel values (may differ from alpha
/// when subpixel rendering is active).
#[derive(Debug, Clone)]
pub struct GlyphBuffer {
    pub width: u32,
    pub height: u32,
    /// Flat `[R, G, B, A]` in linear light, `width * height * 4` elements.
    pub pixels: Vec<f32>,
}

impl GlyphBuffer {
    /// Allocate a zeroed buffer for a glyph of `width × height` pixels.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0.0; (width * height * 4) as usize],
        }
    }

    /// Returns a mutable slice for the pixel at `(col, row)`: `[R, G, B, A]`.
    #[inline]
    pub fn pixel_mut(&mut self, col: u32, row: u32) -> &mut [f32] {
        let idx = ((row * self.width + col) * 4) as usize;
        &mut self.pixels[idx..idx + 4]
    }

    /// Returns a shared slice for the pixel at `(col, row)`: `[R, G, B, A]`.
    #[inline]
    pub fn pixel(&self, col: u32, row: u32) -> &[f32] {
        let idx = ((row * self.width + col) * 4) as usize;
        &self.pixels[idx..idx + 4]
    }

    /// Encode all channels in-place using `encode_fn` (typically an EOTF encoder).
    ///
    /// Alpha is not encoded — it remains in linear light for compositing.
    pub fn encode_eotf(&mut self, encode_fn: impl Fn(f32) -> f32) {
        for chunk in self.pixels.chunks_exact_mut(4) {
            chunk[0] = encode_fn(chunk[0]); // R
            chunk[1] = encode_fn(chunk[1]); // G
            chunk[2] = encode_fn(chunk[2]); // B
                                             // chunk[3] = alpha, not encoded
        }
    }

    /// Pack into a `Vec<u8>` of RGBA8 values (signal domain, after EOTF encoding).
    ///
    /// Clamps each channel to [0, 1] before converting to u8.
    pub fn to_rgba8(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_roundtrip() {
        let mut buf = GlyphBuffer::new(4, 4);
        let p = buf.pixel_mut(2, 1);
        p[0] = 0.8;
        p[1] = 0.5;
        p[2] = 0.2;
        p[3] = 1.0;
        let q = buf.pixel(2, 1);
        assert_eq!(q, &[0.8, 0.5, 0.2, 1.0]);
    }

    #[test]
    fn encode_eotf_skips_alpha() {
        let mut buf = GlyphBuffer::new(1, 1);
        let p = buf.pixel_mut(0, 0);
        p[0] = 1.0;
        p[1] = 1.0;
        p[2] = 1.0;
        p[3] = 0.5; // alpha
        buf.encode_eotf(|v| v * 0.5); // halve all colour channels
        let q = buf.pixel(0, 0);
        assert!((q[0] - 0.5).abs() < 1e-6);
        assert_eq!(q[3], 0.5, "alpha must not be encoded");
    }

    #[test]
    fn to_rgba8_clamps_and_rounds() {
        let mut buf = GlyphBuffer::new(1, 1);
        let p = buf.pixel_mut(0, 0);
        p[0] = 1.0;
        p[1] = 0.0;
        p[2] = 0.5;
        p[3] = 1.0;
        let bytes = buf.to_rgba8();
        assert_eq!(bytes[0], 255);
        assert_eq!(bytes[1], 0);
        assert_eq!(bytes[2], 128);
        assert_eq!(bytes[3], 255);
    }
}
