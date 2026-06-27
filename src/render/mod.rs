//! Rendering strategies — all return linear-light [`SubpixelGrid`]s.
//!
//! # Pipeline
//!
//! ```text
//! GlyphBuffer (linear light)
//!     │
//!     ├── render_grayscale()   ──┐
//!     ├── render_subpixel()    ──┤──▶ SubpixelGrid (linear light)
//!     └── render_oled_aware()  ──┘         │
//!                                    encode_grid()   ← EOTF applied once here
//!                                          │
//!                               framebuffer / simulate module
//! ```
//!
//! EOTF encoding is intentionally kept out of the individual renderers.
//! All filtering, blending, and future optical-blur simulation (Phase 2)
//! must operate in linear light.  The caller applies [`encode_grid`] as
//! the final step before writing to a u8 framebuffer.

pub mod grayscale;
pub mod subpixel_aa;
pub mod oled_aware;

pub use grayscale::render_grayscale;
pub use subpixel_aa::render_subpixel;
pub use oled_aware::render_oled_aware;

use crate::glyph::GlyphBuffer;
use crate::layout::SubpixelLayout;
use crate::profile::DisplayProfile;
use crate::subpixel::SubpixelGrid;

/// The three rendering strategies available in Phase 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Greyscale antialiasing — all channels equal, no subpixel colour.
    Greyscale,
    /// Standard ClearType-style horizontal subpixel AA.
    SubpixelAa,
    /// OLED-aware rendering with layout-matched FIR and adaptive fringe suppression.
    OledAware,
}

impl RenderMode {
    pub fn label(&self) -> &'static str {
        match self {
            RenderMode::Greyscale  => "Greyscale AA",
            RenderMode::SubpixelAa => "Subpixel AA (ClearType-style)",
            RenderMode::OledAware  => "OLED-Aware",
        }
    }
}

/// Dispatch to the correct renderer.
///
/// Returns a **linear-light** [`SubpixelGrid`].  Call [`encode_grid`] before
/// writing to a framebuffer or passing to the simulate module.
pub fn render(
    buf: &GlyphBuffer,
    mode: RenderMode,
    layout: SubpixelLayout,
    profile: &DisplayProfile,
) -> SubpixelGrid {
    match mode {
        RenderMode::Greyscale  => render_grayscale(buf, profile),
        RenderMode::SubpixelAa => render_subpixel(buf, layout, profile),
        RenderMode::OledAware  => render_oled_aware(buf, layout, profile),
    }
}

/// EOTF-encode a linear-light [`SubpixelGrid`] into the signal domain.
///
/// This is the single point where linear → signal encoding happens.
/// Call this after all filtering and simulation are complete, immediately
/// before writing pixel values to a u8 framebuffer.
///
/// Alpha is not encoded (it remains linear for compositing).
pub fn encode_grid(mut grid: SubpixelGrid, profile: &DisplayProfile) -> SubpixelGrid {
    for row in 0..grid.height {
        for col in 0..grid.width {
            let px = grid.pixel_mut(col, row);
            px.r = profile.eotf.encode(px.r);
            px.g = profile.eotf.encode(px.g);
            px.b = profile.eotf.encode(px.b);
        }
    }
    grid
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph::GlyphBuffer;
    use crate::profile::DisplayProfile;

    fn uniform_buf(w: u32, h: u32, cov: f32) -> GlyphBuffer {
        let mut buf = GlyphBuffer::new(w, h);
        for row in 0..h {
            for col in 0..w {
                let px = buf.pixel_mut(col, row);
                px[0] = cov; px[1] = cov; px[2] = cov; px[3] = cov;
            }
        }
        buf
    }

    #[test]
    fn encode_grid_applies_eotf() {
        // 0.5 linear → ~0.735 sRGB signal
        let buf = uniform_buf(2, 2, 0.5);
        let profile = DisplayProfile::sdr_oled();
        let linear = render_grayscale(&buf, &profile);
        // Confirm linear before encoding
        assert!((linear.pixel(0, 0).r - 0.5).abs() < 1e-5);
        // Encode and confirm signal domain
        let encoded = encode_grid(linear, &profile);
        assert!(
            (encoded.pixel(0, 0).r - 0.7354).abs() < 0.01,
            "expected ~0.735 sRGB, got {}",
            encoded.pixel(0, 0).r
        );
    }

    #[test]
    fn encode_grid_is_idempotent_at_endpoints() {
        let buf = uniform_buf(1, 1, 0.0);
        let profile = DisplayProfile::sdr_oled();
        let g = render_grayscale(&buf, &profile);
        let e = encode_grid(g, &profile);
        assert!(e.pixel(0, 0).r < 1e-6, "black stays black after encode");

        let buf1 = uniform_buf(1, 1, 1.0);
        let g1 = render_grayscale(&buf1, &profile);
        let e1 = encode_grid(g1, &profile);
        assert!((e1.pixel(0, 0).r - 1.0).abs() < 1e-5, "white stays white after encode");
    }
}
