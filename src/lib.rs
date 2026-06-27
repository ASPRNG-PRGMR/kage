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
//! ## Architecture
//!
//! ```text
//! font::loader  →  glyph::GlyphBuffer  →  render::{grayscale,subpixel_aa,oled_aware}
//!                                                  ↓
//!                                       subpixel::SubpixelGrid
//!                                                  ↓
//!                                         viz::Inspector  (minifb window)
//! ```
//!
//! ## Status — Phase 1 complete
//!
//! Implemented:
//! - `profile`   — display EOTF, white point, DPI
//! - `layout`    — subpixel layout enum, pentile RGBG geometry and kernels
//! - `glyph`     — output buffer type (linear-light RGBA f32)
//! - `font`      — FreeType font loading and rasterization (grey + LCD modes)
//! - `subpixel`  — virtual subpixel grid, layout-aware filtering
//! - `render`    — three rendering strategies: greyscale, subpixel AA, OLED-aware
//! - `viz`       — zoomable subpixel inspector (minifb, keyboard-controlled)
//!
//! In progress (Phase 2):
//! - `simulate`  — optical blur, subpixel bleed, gamma-aware reconstruction

pub mod font;
pub mod glyph;
pub mod layout;
pub mod profile;
pub mod render;
pub mod subpixel;
pub mod viz;
