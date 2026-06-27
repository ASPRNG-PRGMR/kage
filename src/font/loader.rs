//! FreeType-backed font loading and glyph rasterization.
//!
//! Uses the `freetype-rs` crate (package = "freetype-rs", lib name = "freetype").
//!
//! [`FontFace`] wraps a FreeType face and exposes:
//! - [`FontFace::rasterize_grey`] — grayscale antialiased coverage bitmap
//! - [`FontFace::rasterize_lcd`]  — horizontal RGB subpixel bitmap
//!
//! Both return a [`GlyphBitmap`] which the render layer maps into a [`GlyphBuffer`].

use std::path::Path;

use freetype::{face::LoadFlag, Library, RenderMode};

use crate::glyph::GlyphBuffer;

// ── Public types ─────────────────────────────────────────────────────────────

/// Raw bitmap produced by FreeType, before channel separation.
#[derive(Debug, Clone)]
pub struct GlyphBitmap {
    /// Logical pixel width of the glyph.
    /// For LCD mode this is already divided by 3 (one entry per logical column).
    pub width: u32,
    /// Pixel height of the bitmap.
    pub height: u32,
    /// Horizontal bearing in pixels (cursor x to left edge of bitmap).
    pub bearing_x: i32,
    /// Vertical bearing in pixels (baseline to top edge of bitmap).
    pub bearing_y: i32,
    /// Horizontal cursor advance in pixels (26.6 fixed → pixels, shifted right by 6).
    pub advance: i32,
    /// Raw byte buffer.
    /// Grey mode: 1 byte per pixel, row-major.
    /// LCD mode:  3 bytes per logical pixel (R, G, B), row-major.
    pub buffer: Vec<u8>,
    /// Which render mode produced this bitmap.
    pub mode: RasterMode,
}

/// Which FreeType render mode was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterMode {
    /// `FT_RENDER_MODE_NORMAL` — 8-bit grayscale coverage, 1 byte/pixel.
    Grey,
    /// `FT_RENDER_MODE_LCD` — 8-bit per R/G/B subpixel, 3 bytes/logical pixel.
    Lcd,
}

/// A loaded TrueType / OpenType face at a fixed pixel size.
pub struct FontFace {
    /// Kept alive: the Library must outlive all faces it created.
    _library: Library,
    face: freetype::Face,
}

impl FontFace {
    /// Load a font from `path` and set the render height to `px` device pixels.
    ///
    /// Passing `width = 0` to FreeType means "same as height" (square pixels).
    pub fn load(path: impl AsRef<Path>, px: u32) -> Result<Self, freetype::Error> {
        let library = Library::init()?;
        let face = library.new_face(path.as_ref().as_os_str(), 0)?;
        face.set_pixel_sizes(0, px)?;
        Ok(Self {
            _library: library,
            face,
        })
    }

    /// Rasterize `ch` using grayscale antialiasing (`FT_RENDER_MODE_NORMAL`).
    ///
    /// Returns `None` if the character is not present in the font.
    pub fn rasterize_grey(&self, ch: char) -> Option<GlyphBitmap> {
        let idx = self.face.get_char_index(ch as usize)?;

        self.face.load_glyph(idx, LoadFlag::DEFAULT).ok()?;
        self.face.glyph().render_glyph(RenderMode::Normal).ok()?;

        let g = self.face.glyph();
        let bm = g.bitmap();

        Some(GlyphBitmap {
            width:     bm.width().max(0) as u32,
            height:    bm.rows().max(0) as u32,
            bearing_x: g.bitmap_left(),
            bearing_y: g.bitmap_top(),
            advance:   (g.advance().x >> 6) as i32,
            buffer:    bm.buffer().to_vec(),
            mode:      RasterMode::Grey,
        })
    }

    /// Rasterize `ch` using horizontal LCD subpixel antialiasing (`FT_RENDER_MODE_LCD`).
    ///
    /// The returned bitmap uses 3 bytes per logical pixel column (R, G, B).
    /// FreeType's built-in 5-tap LCD filter is applied before the bytes reach
    /// the buffer.  This is the standard ClearType-style path.
    ///
    /// Returns `None` if the character is not present in the font.
    pub fn rasterize_lcd(&self, ch: char) -> Option<GlyphBitmap> {
        let idx = self.face.get_char_index(ch as usize)?;

        self.face.load_glyph(idx, LoadFlag::DEFAULT).ok()?;
        self.face.glyph().render_glyph(RenderMode::Lcd).ok()?;

        let g = self.face.glyph();
        let bm = g.bitmap();

        // In LCD mode FreeType reports width as the byte width (3× logical columns).
        let byte_width = bm.width().max(0) as u32;
        let logical_width = byte_width / 3;

        Some(GlyphBitmap {
            width:     logical_width,
            height:    bm.rows().max(0) as u32,
            bearing_x: g.bitmap_left(),
            bearing_y: g.bitmap_top(),
            advance:   (g.advance().x >> 6) as i32,
            buffer:    bm.buffer().to_vec(),
            mode:      RasterMode::Lcd,
        })
    }

    /// The pixel height this face was configured with.
    pub fn pixel_size(&self) -> u32 {
        self.face
            .size_metrics()
            .map(|m| m.y_ppem as u32)
            .unwrap_or(0)
    }
}

// ── Conversion: GlyphBitmap → GlyphBuffer ────────────────────────────────────

impl GlyphBitmap {
    /// Convert to a [`GlyphBuffer`] in linear light.
    ///
    /// FreeType outputs bytes in the sRGB signal domain.  This function
    /// decodes each byte to linear light using the sRGB EOTF before storing it.
    ///
    /// **Grey mode**: R = G = B = A = coverage (single channel → all channels).
    ///
    /// **LCD mode**: R, G, B are set independently per channel.
    /// Alpha = luminance-weighted average (used for alpha compositing).
    pub fn into_glyph_buffer(self) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(self.width, self.height);

        match self.mode {
            RasterMode::Grey => {
                for row in 0..self.height {
                    for col in 0..self.width {
                        let idx = (row * self.width + col) as usize;
                        let raw = self.buffer.get(idx).copied().unwrap_or(0);
                        let cov = srgb_to_linear(raw as f32 / 255.0);
                        let px = buf.pixel_mut(col, row);
                        px[0] = cov;
                        px[1] = cov;
                        px[2] = cov;
                        px[3] = cov;
                    }
                }
            }
            RasterMode::Lcd => {
                // FreeType LCD pitch = logical_width * 3 bytes per row.
                let pitch = (self.width * 3) as usize;
                for row in 0..self.height {
                    for col in 0..self.width {
                        let base = row as usize * pitch + col as usize * 3;
                        let r = srgb_to_linear(
                            self.buffer.get(base).copied().unwrap_or(0) as f32 / 255.0,
                        );
                        let g = srgb_to_linear(
                            self.buffer.get(base + 1).copied().unwrap_or(0) as f32 / 255.0,
                        );
                        let b = srgb_to_linear(
                            self.buffer.get(base + 2).copied().unwrap_or(0) as f32 / 255.0,
                        );
                        // Rec. 709 luminance weights for compositing alpha
                        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                        let px = buf.pixel_mut(col, row);
                        px[0] = r;
                        px[1] = g;
                        px[2] = b;
                        px[3] = luma;
                    }
                }
            }
        }

        buf
    }
}

/// IEC 61966-2-1 sRGB EOTF: signal [0,1] → linear light [0,1].
#[inline]
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_to_linear_endpoints() {
        assert!(srgb_to_linear(0.0).abs() < 1e-6);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn srgb_to_linear_midpoint_below_half() {
        // sRGB signal 0.5 decodes to linear ~0.214 (darker than linear midpoint).
        let lin = srgb_to_linear(0.5);
        assert!(lin < 0.5, "sRGB 0.5 signal should decode below linear 0.5, got {lin}");
        assert!(lin > 0.1, "sanity lower bound");
    }

    #[test]
    fn grey_bitmap_to_buffer_black_and_white() {
        let bm = GlyphBitmap {
            width: 2, height: 1,
            bearing_x: 0, bearing_y: 0, advance: 2,
            buffer: vec![0u8, 255u8],
            mode: RasterMode::Grey,
        };
        let buf = bm.into_glyph_buffer();
        // Pixel (0,0): linear black
        assert!(buf.pixel(0, 0)[0] < 1e-6, "expected black at (0,0)");
        // Pixel (1,0): linear white (sRGB 255 → linear 1.0)
        assert!((buf.pixel(1, 0)[0] - 1.0).abs() < 1e-4, "expected white at (1,0)");
        // All channels equal in grey mode
        let p = buf.pixel(1, 0);
        assert!((p[0] - p[1]).abs() < 1e-6);
        assert!((p[0] - p[3]).abs() < 1e-6);
    }

    #[test]
    fn lcd_bitmap_separates_channels() {
        let bm = GlyphBitmap {
            width: 1, height: 1,
            bearing_x: 0, bearing_y: 0, advance: 1,
            buffer: vec![255u8, 128u8, 0u8],
            mode: RasterMode::Lcd,
        };
        let buf = bm.into_glyph_buffer();
        let p = buf.pixel(0, 0);
        assert!((p[0] - 1.0).abs() < 1e-4, "R should be ~1.0 linear, got {}", p[0]);
        assert!(p[1] > 0.1 && p[1] < 0.9, "G should be mid-range, got {}", p[1]);
        assert!(p[2] < 1e-6, "B should be ~0.0 linear, got {}", p[2]);
    }
}
