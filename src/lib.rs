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
//! 1. **Sampling at the emitter** — each colour of each pixel takes the
//!    glyph's coverage where that colour's emitter physically sits, read
//!    from supersampled coverage and a description of the panel
//! 2. **Simulating the panel** — emitters at their real positions, optical
//!    blur, per-colour bleed — and scoring every renderer against what an
//!    ideal display would show
//! 3. **Display profile integration** — EOTF-correct encoding and
//!    DPI-adaptive decisions
//!
//! ## Architecture
//!
//! ```text
//! font::loader ──▶ coverage::Coverage ──▶ render::{grayscale, subpixel_aa, oled_aware}
//!                  (pixel + supersampled)              ↓
//!                                         subpixel::SubpixelGrid  (linear light)
//!                                            ↓                ↓
//!                                            │     simulate::simulate_panel
//!                                            │     (emitter placement + PSF blur)
//!                                            ↓                ↓
//!                                           viz::Inspector  (EOTF-encodes, minifb window)
//! ```
//!
//! [`pipeline::Pipeline`] runs that chain and computes the metrics.
//!
//! ## Status — Phases 1 to 3 complete
//!
//! - `profile`   — display EOTF, white point, DPI
//! - `layout`    — subpixel layouts, fixed filter kernels (stripe, Pentile,
//!   Delta-RGB) and per-pixel emitter positions
//! - `glyph`     — pixel buffer type (linear-light RGBA f32)
//! - `plane`     — single-channel image with clamp-to-edge sampling
//! - `coverage`  — glyph coverage at pixel resolution and supersampled
//! - `font`      — FreeType font loading and rasterization
//! - `subpixel`  — virtual subpixel grid, kernel filtering
//! - `render`    — three rendering strategies: greyscale, subpixel AA,
//!   OLED-aware; contrast-sensitivity model
//! - `simulate`  — physical subpixel geometry (built-in or from `.toml`),
//!   optical PSF blur, per-channel bleed, luminance reconstruction, metrics
//! - `pipeline`  — glyph → renderers → simulation → metrics, auto-tuning
//! - `viz`       — zoomable subpixel inspector (minifb) with raw / simulated views

pub mod coverage;
pub mod font;
pub mod glyph;
pub mod gpu;
pub mod layout;
pub mod pipeline;
pub mod plane;
pub mod profile;
pub mod render;
pub mod simulate;
pub mod subpixel;
pub mod viz;
