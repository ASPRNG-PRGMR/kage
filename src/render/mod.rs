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
//! The greyscale and subpixel renderers apply a fixed kernel to
//! pixel-resolution coverage ([`BaseFilter`], chosen by [`base_filter`]).
//! The OLED-aware renderer instead samples supersampled coverage at each
//! emitter's position ([`perceptual`]), controlled by [`RenderParams`].
//!
//! EOTF encoding is intentionally kept out of the individual renderers.
//! All filtering, blending, and display simulation (`crate::simulate`)
//! must operate in linear light.  The caller applies [`encode_grid`] as
//! the final step before writing to a u8 framebuffer.

pub mod csf;
pub mod grayscale;
pub mod oled_aware;
pub mod perceptual;
pub mod subpixel_aa;

pub use grayscale::render_grayscale;
pub use oled_aware::{render_oled_aware, render_oled_aware_with};
pub use perceptual::{render_at_emitters, RenderParams};
pub use subpixel_aa::render_subpixel;

use crate::coverage::Coverage;
use crate::glyph::GlyphBuffer;
use crate::layout::{BaseFilter, EmitterMap, SubpixelLayout};
use crate::profile::DisplayProfile;
use crate::subpixel::SubpixelGrid;

/// The three rendering strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Greyscale antialiasing — all channels equal, no subpixel colour.
    Greyscale,
    /// Fixed-kernel subpixel AA on pixel-resolution coverage.
    SubpixelAa,
    /// Supersampled coverage sampled at each emitter's position.
    OledAware,
}

impl RenderMode {
    pub const ALL: [RenderMode; 3] = [RenderMode::Greyscale, RenderMode::SubpixelAa, RenderMode::OledAware];

    pub fn label(&self) -> &'static str {
        match self {
            RenderMode::Greyscale  => "Greyscale AA",
            RenderMode::SubpixelAa => "Subpixel AA (ClearType-style)",
            RenderMode::OledAware  => "OLED-Aware",
        }
    }
}

/// The fixed kernel `mode` applies to pixel-resolution coverage on `layout`
/// for a panel described by `profile`.
///
/// Greyscale mode and HiDPI panels always get [`BaseFilter::Grey`]. For
/// [`RenderMode::OledAware`] this is only the fallback kernel: that mode
/// samples at emitter positions unless [`samples_at_emitters`] says no.
pub fn base_filter(mode: RenderMode, layout: SubpixelLayout, profile: &DisplayProfile) -> BaseFilter {
    if mode == RenderMode::Greyscale || profile.is_hidpi() {
        return BaseFilter::Grey;
    }
    BaseFilter::for_layout(layout)
}

/// Whether `mode` samples at emitter positions on this panel. On a HiDPI
/// panel the emitters are too fine for it to matter, and every mode falls
/// back to greyscale.
pub fn samples_at_emitters(mode: RenderMode, profile: &DisplayProfile) -> bool {
    mode == RenderMode::OledAware && !profile.is_hidpi()
}

/// Dispatch to the correct renderer, with [`RenderParams::for_layout`] and
/// the layout's nominal emitter positions.
///
/// `buf` holds pixel-resolution coverage; use [`render_coverage`] to give
/// the OLED-aware renderer supersampled coverage.
///
/// Returns a **linear-light** [`SubpixelGrid`].  Call [`encode_grid`] before
/// writing to a framebuffer.
pub fn render(
    buf: &GlyphBuffer,
    mode: RenderMode,
    layout: SubpixelLayout,
    profile: &DisplayProfile,
) -> SubpixelGrid {
    render_coverage(
        &Coverage::from_glyph(buf),
        mode,
        layout,
        &EmitterMap::for_layout(layout),
        profile,
        &RenderParams::for_layout(layout),
    )
}

/// Dispatch to the correct renderer.
///
/// `layout` selects the fixed kernel of the Subpixel AA mode; `map` gives
/// the emitter positions and `params` the settings of the OLED-aware mode.
pub fn render_coverage(
    cov: &Coverage,
    mode: RenderMode,
    layout: SubpixelLayout,
    map: &EmitterMap,
    profile: &DisplayProfile,
    params: &RenderParams,
) -> SubpixelGrid {
    if samples_at_emitters(mode, profile) {
        render_at_emitters(cov, map, params)
    } else {
        SubpixelGrid::from_coverage(&cov.lo, base_filter(mode, layout, profile))
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
    fn base_filter_follows_mode_layout_and_density() {
        let profile = DisplayProfile::sdr_oled();
        let mut hidpi = profile.clone();
        hidpi.device_pixel_ratio = 2.0;
        use SubpixelLayout::*;
        assert_eq!(base_filter(RenderMode::Greyscale, PentileRgbg, &profile), BaseFilter::Grey);
        assert_eq!(base_filter(RenderMode::SubpixelAa, PentileRgbg, &profile), BaseFilter::Pentile);
        assert_eq!(base_filter(RenderMode::SubpixelAa, DeltaRgb, &profile), BaseFilter::Delta);
        assert_eq!(base_filter(RenderMode::SubpixelAa, Wrgb, &profile), BaseFilter::Grey);
        assert_eq!(base_filter(RenderMode::SubpixelAa, RgbStripe, &hidpi), BaseFilter::Grey);
        assert!(samples_at_emitters(RenderMode::OledAware, &profile));
        assert!(!samples_at_emitters(RenderMode::OledAware, &hidpi));
        assert!(!samples_at_emitters(RenderMode::SubpixelAa, &profile));
    }

    #[test]
    fn render_coverage_uses_the_fine_plane_only_in_oled_mode() {
        use crate::plane::Plane;
        // Edge at x = 2.25: invisible at pixel resolution beyond lo = 0.75.
        let cov = Coverage::from_hi(Plane::from_fn(24, 8, |x, _| if x >= 9 { 1.0 } else { 0.0 }), 4);
        let profile = DisplayProfile::sdr_oled();
        let layout = SubpixelLayout::RgbStripe;
        let map = EmitterMap::for_layout(layout);
        let params = RenderParams::default();
        let grey = render_coverage(&cov, RenderMode::Greyscale, layout, &map, &profile, &params);
        assert!((grey.pixel(2, 1).g - 0.75).abs() < 1e-6);
        let oled = render_coverage(&cov, RenderMode::OledAware, layout, &map, &profile, &params);
        let px = oled.pixel(2, 1);
        assert!(px.r < px.g && px.g < px.b, "{px:?}");
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
