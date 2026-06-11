//! # KAGE — Kinetic Adaptive Glyph Engine
//!
//! An experimental OLED-aware text rendering engine researching subpixel-accurate
//! glyph rendering on modern display layouts.
//!
//! ## Problem
//!
//! Classical ClearType-style subpixel rendering was designed for LCD panels with
//! predictable horizontal RGB/BGR stripe arrangements.  On OLED panels — especially
//! pentile RGBG (Samsung) and delta-RGB (QD-OLED) — the subpixel geometry differs
//! fundamentally:
//!
//! - Red and blue are at reduced resolution in pentile layouts
//! - Colour emitters are offset vertically in delta-RGB layouts
//! - Horizontal-only filtering introduces chromatic fringing on diagonal strokes
//!
//! KAGE addresses this by:
//!
//! 1. **Layout-aware filter kernels** — per-layout FIR taps that match subpixel geometry
//! 2. **Adaptive fringe suppression** — blending subpixel-rendered channels with a
//!    greyscale fallback at high-contrast edges, where chromatic penalty exceeds gain
//! 3. **Display profile integration** — EOTF-correct encoding and DPI-adaptive decisions
//!
//! ## Status
//!
//! Early research prototype.  Currently implemented:
//! - `profile` — display EOTF, white point, DPI
//! - `layout` — subpixel layout enum + pentile RGBG geometry and kernels
//! - `glyph` — output buffer type
//!
//! In progress:
//! - `raster` — scanline coverage accumulator
//! - `filter` — full filter pass integrating layout + fringe suppression

pub mod glyph;
pub mod layout;
pub mod profile;
