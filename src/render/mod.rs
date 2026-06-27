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

/// Dispatch to the correct renderer and return a ready-to-display [`SubpixelGrid`].
///
/// The grid values are in linear light.  Call [`crate::profile::DisplayProfile`]
/// EOTF encoding before compositing into a u8 framebuffer.
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
