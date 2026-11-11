//! Minimal text drawing for the inspector's own interface.
//!
//! Glyphs are rasterized with the same FreeType loader the engine uses
//! ([`FontFace`]), cached per size, and alpha-blended into the framebuffer in
//! linear light.

use std::collections::HashMap;

use crate::font::FontFace;
use crate::profile::Eotf;

/// One rasterized glyph, ready to blit.
struct CachedGlyph {
    width: i32,
    height: i32,
    /// Offset from the pen position to the bitmap's left edge.
    left: i32,
    /// Offset from the baseline up to the bitmap's top edge.
    top: i32,
    advance: i32,
    coverage: Vec<u8>,
}

/// Draws UI text at a handful of pixel sizes from one font file.
pub struct TextRenderer {
    path: String,
    faces: HashMap<u32, FontFace>,
    glyphs: HashMap<(u32, char), Option<CachedGlyph>>,
    /// sRGB byte → linear light.
    to_linear: [f32; 256],
}

impl TextRenderer {
    /// Create a renderer for the font at `path`.
    ///
    /// Returns `None` if the font cannot be loaded, in which case the
    /// inspector runs without text.
    pub fn new(path: &str) -> Option<Self> {
        FontFace::load(path, 16).ok()?;
        let mut to_linear = [0.0f32; 256];
        for (i, v) in to_linear.iter_mut().enumerate() {
            *v = Eotf::Srgb.decode(i as f32 / 255.0);
        }
        Some(Self {
            path: path.to_owned(),
            faces: HashMap::new(),
            glyphs: HashMap::new(),
            to_linear,
        })
    }

    fn face(&mut self, px: u32) -> Option<&FontFace> {
        if !self.faces.contains_key(&px) {
            let face = FontFace::load(&self.path, px).ok()?;
            self.faces.insert(px, face);
        }
        self.faces.get(&px)
    }

    fn glyph(&mut self, px: u32, ch: char) -> Option<&CachedGlyph> {
        if !self.glyphs.contains_key(&(px, ch)) {
            let glyph = self.face(px).and_then(|face| face.rasterize_grey(ch)).map(|bm| CachedGlyph {
                width: bm.width as i32,
                height: bm.height as i32,
                left: bm.bearing_x,
                top: bm.bearing_y,
                advance: bm.advance,
                coverage: bm.buffer,
            });
            self.glyphs.insert((px, ch), glyph);
        }
        self.glyphs.get(&(px, ch)).and_then(Option::as_ref)
    }

    /// Width of `text` in host pixels at size `px`.
    pub fn width(&mut self, px: u32, text: &str) -> i32 {
        text.chars()
            .map(|ch| self.glyph(px, ch).map_or(0, |g| g.advance))
            .sum()
    }

    /// Draw `text` with its top-left corner at (`x`, `y`).
    ///
    /// Pixels at or beyond `clip_right` are not drawn. Characters the font
    /// lacks are skipped. Returns the x-coordinate after the last glyph.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        fb: &mut [u32],
        fb_w: usize,
        fb_h: usize,
        x: i32,
        y: i32,
        px: u32,
        color: u32,
        clip_right: i32,
        text: &str,
    ) -> i32 {
        let ascent = match self.face(px) {
            Some(face) => face.ascent(),
            None => return x,
        };
        let baseline = y + ascent;
        let fg = [
            self.to_linear[((color >> 16) & 0xFF) as usize],
            self.to_linear[((color >> 8) & 0xFF) as usize],
            self.to_linear[(color & 0xFF) as usize],
        ];
        let to_linear = self.to_linear;
        let clip_right = clip_right.min(fb_w as i32);

        let mut pen = x;
        for ch in text.chars() {
            let Some(g) = self.glyph(px, ch) else { continue };
            for gy in 0..g.height {
                let py = baseline - g.top + gy;
                if py < 0 || py >= fb_h as i32 {
                    continue;
                }
                for gx in 0..g.width {
                    let pxl = pen + g.left + gx;
                    if pxl < 0 || pxl >= clip_right {
                        continue;
                    }
                    let cov = g.coverage.get((gy * g.width + gx) as usize).copied().unwrap_or(0);
                    if cov == 0 {
                        continue;
                    }
                    let a = cov as f32 / 255.0;
                    let idx = py as usize * fb_w + pxl as usize;
                    let bg = fb[idx];
                    let mut out = 0xFF00_0000u32;
                    for (i, shift) in [16u32, 8, 0].into_iter().enumerate() {
                        let b = to_linear[((bg >> shift) & 0xFF) as usize];
                        let v = Eotf::Srgb.encode(b + (fg[i] - b) * a);
                        out |= ((v * 255.0).round() as u32) << shift;
                    }
                    fb[idx] = out;
                }
            }
            pen += g.advance;
        }
        pen
    }
}
