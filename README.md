# KAGE — Kinetic Adaptive Glyph Engine
### OLED-Aware Text Rendering Research Sandbox

> A programmable experimental text rendering engine and display simulator for improving text clarity on modern OLED displays with non-standard subpixel layouts (WOLED, Pentile, Triangular).

---

## Why This Project Exists

Traditional subpixel rendering — most notably Microsoft's **ClearType** — was engineered around **RGB stripe LCD** displays. Each pixel in that layout contains three predictably ordered subpixels: Red, Green, Blue, from left to right.

Modern OLED displays break that assumption entirely. Common OLED subpixel layouts include:

| Layout | Description |
|--------|-------------|
| **WRGB (WOLED)** | Adds a White subpixel alongside R, G, B |
| **Pentile RGBG** | Alternating RG / BG pairs; fewer subpixels per pixel |
| **Delta-RGB** | Triangular arrangement (QD-OLED, Alienware); vertical offsets between R/B and G |
| **Asymmetric** | Vendor-specific irregular arrangements |

Running standard LCD subpixel rendering on these displays causes:
- **Color fringing** — colored halos around letterforms
- **Reduced sharpness** — blurred or mushy glyph edges
- **Chromatic edge artifacts** — incorrect color bleeding at transitions
- **Inconsistent luminance** — uneven brightness across glyph strokes

Most Linux rendering pipelines respond by either assuming RGB stripe (wrong) or disabling subpixel rendering entirely and falling back to grayscale antialiasing (safe but blurry). Neither is satisfying.

This project explores a third path: **display-aware perceptual text reconstruction** — rendering decisions made with explicit knowledge of the physical subpixel structure of the target display.

---

## Current Status

**Phase 1 complete.** The full rendering sandbox and zoomable inspector are implemented and ready to build.

| Phase | Status | Description |
|-------|--------|-------------|
| **Phase 1** | ✅ Complete | Rendering sandbox + zoomable inspector |
| **Phase 1.5** | ✅ Complete | Pipeline refactor — renderers return linear light |
| **Phase 2** | 🔲 Next | Optical blur simulation, subpixel bleed, gamma reconstruction |
| **Phase 3** | 🔲 Planned | Experimental rendering: luma/chroma decoupling, frequency-aware |
| **Phase 4** | 🔲 Planned | GPU acceleration (OpenGL → Vulkan) |

---

## Getting Started

### Prerequisites

```bash
# Rust toolchain (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# No system FreeType library needed — the "bundled" feature compiles
# FreeType from source. A C compiler is required (installed by default
# on most Linux distros and macOS with Xcode Command Line Tools).
```

### Build

```bash
git clone <repo>
cd kage
cargo build --release
```

### Run the inspector

```bash
# Basic usage — render glyph 'A' at 64px on a pentile layout
cargo run --release -- --font /path/to/font.ttf --glyph A

# Full options
cargo run --release -- \
  --font /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf \
  --glyph g \
  --layout pentile \
  --size 96

# Available layouts: rgb | bgr | pentile | delta | wrgb | grey
# On Ubuntu, a good test font: /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf
# On macOS:                     /Library/Fonts/Arial.ttf
```

### Inspector controls

| Input | Action |
|-------|--------|
| Scroll wheel | Zoom in / out, anchored to cursor position |
| `+` / `=` | Zoom in (keyboard) |
| `-` | Zoom out (keyboard) |
| Arrow keys | Pan |
| `1` | Greyscale AA only |
| `2` | Subpixel AA (ClearType-style) only |
| `3` | OLED-Aware only |
| `A` | Side-by-side comparison (default) |
| `H` | Toggle per-channel heatmap overlay |
| `Esc` | Quit |

The default view is **side-by-side**: Greyscale AA | Subpixel AA | OLED-Aware, left to right. At zoom ≥ 8× individual pixel grid lines are drawn. The heatmap mode (`H`) false-colours the image to make R/B channel imbalance visible — useful for measuring fringing.

---

## Architecture

```
font::loader::FontFace
      │  FreeType rasterization → GlyphBitmap (u8 grey or LCD)
      ▼
glyph::GlyphBuffer          [R, G, B, A: f32]  linear light, width×height×4
      │
      ├──▶ render::grayscale    ──┐
      ├──▶ render::subpixel_aa  ──┤──▶ SubpixelGrid (linear light)
      └──▶ render::oled_aware   ──┘         │
                                      render::encode_grid()   ← EOTF applied once
                                             │
                                    viz::Inspector   (minifb window, zoom/pan/heatmap)
                                    simulate::*      (Phase 2 — operates in linear light)
```

### Key data types

| Type | Location | Description |
|------|----------|-------------|
| `GlyphBuffer` | `src/glyph.rs` | Linear-light RGBA f32 pixel buffer |
| `GlyphBitmap` | `src/font/loader.rs` | Raw FreeType bitmap (u8, grey or LCD) |
| `FontFace` | `src/font/loader.rs` | FreeType face with pixel size set |
| `SubpixelLayout` | `src/layout/mod.rs` | Enum of known panel geometries |
| `SubpixelGrid` | `src/subpixel/grid.rs` | Per-pixel (R,G,B) energy after layout filtering |
| `DisplayProfile` | `src/profile.rs` | EOTF, white point, DPI, DPR |
| `RenderMode` | `src/render/mod.rs` | Greyscale / SubpixelAa / OledAware |
| `Inspector` | `src/viz/inspector.rs` | minifb window, zoom/pan/heatmap |

---

## Module Reference

### `src/glyph.rs` — `GlyphBuffer`
Flat `[R, G, B, A: f32]` buffer in linear light (before EOTF encoding). The alpha channel holds greyscale coverage for compositing. R/G/B hold per-channel subpixel values when subpixel rendering is active. Provides `encode_eotf()` and `to_rgba8()`.

### `src/profile.rs` — `DisplayProfile`
Describes the physical display: EOTF (sRGB, Gamma, PQ), white point (D65, DCI-P3), DPI, and device pixel ratio. `is_hidpi()` returns true at ≥192 effective DPI, triggering greyscale fallback. `sdr_oled()` constructs a sensible default for typical Samsung OLED monitors.

### `src/layout/mod.rs` — `SubpixelLayout`
Enum of panel geometries: `RgbStripe`, `BgrStripe`, `PentileRgbg`, `DeltaRgb`, `Wrgb`, `Greyscale`. `subpixel_rendering_useful()` returns false for WRGB and Greyscale (chromatic penalty exceeds gain). `channel_weights(col_parity, row_parity)` returns per-channel presence weights for layout-aware filtering.

### `src/layout/pentile.rs` — Pentile RGBG geometry
FIR kernel constants (`GREEN_FIR`, `RED_FIR`, `BLUE_FIR`), `filter_row()` applying them, `chromatic_penalty()` computing the normalised discrete Laplacian, and `adaptive_blend()` lerping between subpixel and greyscale outputs.

### `src/font/loader.rs` — `FontFace`, `GlyphBitmap`
FreeType-backed loader. `rasterize_grey()` produces 8-bit greyscale coverage. `rasterize_lcd()` produces 3-byte-per-pixel RGB subpixel output (FreeType's built-in LCD filter applied). `GlyphBitmap::into_glyph_buffer()` converts to linear-light `GlyphBuffer` via sRGB EOTF decode.

### `src/subpixel/grid.rs` — `SubpixelGrid`
Virtual subpixel grid. `from_glyph(buf, layout, fringe_suppress)` applies the layout-appropriate filter row by row. `to_argb8_display()` packs into `Vec<u32>` for minifb. The RGB stripe path uses a 3-tap phase-shifted FIR. Pentile uses `layout::pentile::filter_row()` plus optional `adaptive_blend()`.

### `src/render/` — Three rendering strategies

| Module | Strategy | Fringe suppression |
|--------|----------|--------------------|
| `grayscale.rs` | All channels = coverage, linear light | N/A |
| `subpixel_aa.rs` | Layout FIR applied, linear light | No |
| `oled_aware.rs` | Layout FIR + adaptive blend, linear light | Yes |

All three return **linear-light** grids. All three auto-fall-back to greyscale on HiDPI or WRGB layouts. `encode_grid()` in `render/mod.rs` is the single point where EOTF encoding happens, called by the inspector (and in future by the simulate module) immediately before display.

### `src/viz/inspector.rs` — `Inspector`
minifb window with zoom (1–32×), pan, three single-panel views, side-by-side mode, and a false-colour heatmap overlay for channel imbalance analysis. At zoom ≥ 8× a pixel grid is drawn over the zoomed output. The label strip uses colour-coded accent bars (blue = greyscale, orange = subpixel AA, green = OLED-aware).

---

## Development Phases

### Phase 1 — Rendering Sandbox ✅
- [x] Load TrueType / OpenType fonts via FreeType
- [x] Rasterize glyphs (greyscale and LCD modes)
- [x] GlyphBuffer in linear-light f32
- [x] SubpixelLayout enum + Pentile RGBG geometry and FIR kernels
- [x] DisplayProfile with EOTF (sRGB, Gamma, PQ HDR), white point, DPI
- [x] Three renderers: Greyscale AA, Subpixel AA, OLED-Aware
- [x] Adaptive fringe suppression (chromatic penalty + adaptive blend)
- [x] SubpixelGrid with layout-aware filtering
- [x] Zoomable inspector: zoom, pan, single/side-by-side, heatmap

### Phase 1.5 — Pipeline Refactor ✅
- [x] Renderers return linear-light `SubpixelGrid` (no EOTF inside renderers)
- [x] `encode_grid()` moved to `render/mod.rs` — single encoding point
- [x] Inspector calls `encode_grid()` per frame before blitting
- [x] Phase 2 simulate module can now receive linear grids directly

### Phase 2 — Display Simulation 🔲
- [ ] Optical PSF (point spread function) convolution
- [ ] Subpixel light bleed and lateral diffusion
- [ ] Model physical OLED subpixel arrangements from datasheets
- [ ] Gamma-aware luminance reconstruction
- [ ] `src/simulate/optical_blur.rs`
- [ ] `src/simulate/gamma.rs`

### Phase 3 — Experimental Rendering Techniques 🔲
- [ ] Luma/chroma decoupling (sharpen luma, suppress chroma at edges)
- [ ] Edge-aware chroma suppression beyond the current Laplacian approximation
- [ ] Frequency-aware rendering (preserve high-frequency luminance detail)
- [ ] Perceptual filtering matched to human contrast sensitivity function (CSF)
- [ ] Delta-RGB (QD-OLED) 2-D kernel (currently falls back to greyscale)
- [ ] WRGB white-channel luminance reconstruction

### Phase 4 — GPU Acceleration 🔲
- [ ] OpenGL rendering backend
- [ ] GLSL fragment shader implementations of all three render modes
- [ ] Vulkan compute shader backend
- [ ] Interactive real-time parameter tuning UI
- [ ] `src/gpu/` module

---

## Technical Stack

| Component | Technology |
|-----------|------------|
| Language | Rust (edition 2021) |
| Font loading & rasterization | `freetype` crate (FreeType 2) |
| Windowing / framebuffer | `minifb` (software, no GPU) |
| CLI | `clap` v4 (derive API) |
| Rendering — Phase 1 | CPU software renderer |
| Rendering — Phase 3+ | OpenGL + GLSL shaders |
| Rendering — Phase 4 | Vulkan + compute shaders |

---

## Repository Structure

```
kage/
├── src/
│   ├── main.rs                    # CLI entry point (clap), event loop
│   ├── lib.rs                     # Crate root, module declarations
│   ├── glyph.rs                   # GlyphBuffer — linear-light RGBA f32 buffer
│   ├── profile.rs                 # DisplayProfile — EOTF, white point, DPI
│   ├── font/
│   │   ├── mod.rs                 # Re-exports FontFace
│   │   └── loader.rs              # FreeType loader, GlyphBitmap → GlyphBuffer
│   ├── layout/
│   │   ├── mod.rs                 # SubpixelLayout enum, channel_weights()
│   │   └── pentile.rs             # Pentile RGBG FIR kernels, chromatic_penalty()
│   ├── subpixel/
│   │   ├── mod.rs                 # Re-exports SubpixelGrid, SubpixelPixel
│   │   └── grid.rs                # Layout-aware filtering → SubpixelGrid (linear light)
│   ├── render/
│   │   ├── mod.rs                 # RenderMode enum, render() dispatch, encode_grid()
│   │   ├── grayscale.rs           # Greyscale AA renderer (returns linear light)
│   │   ├── subpixel_aa.rs         # ClearType-style subpixel renderer (returns linear light)
│   │   └── oled_aware.rs          # OLED-aware renderer (returns linear light)
│   ├── simulate/                  # Phase 2 — optical blur, gamma reconstruction
│   │   ├── mod.rs
│   │   ├── optical_blur.rs
│   │   └── gamma.rs
│   └── viz/
│       ├── mod.rs                 # Re-exports Inspector
│       └── inspector.rs           # minifb window: zoom, pan, heatmap, side-by-side
├── assets/
│   └── fonts/                     # Place test .ttf / .otf files here
├── layouts/                       # Future: .toml custom layout definitions
├── shaders/                       # Future: GLSL shaders (Phase 4)
├── Cargo.toml
└── README.md
```

---

## Long-Term Vision

If the research is productive, potential directions include:

- **Compositor integration** — apply OLED-aware rendering at the Wayland/X11 compositor level
- **Linux desktop integration** — patches or plugins for fontconfig / cairo / FreeType
- **Panel-specific rendering profiles** — per-display calibration files (`.toml`)
- **Adaptive calibration** — measure display subpixel geometry at runtime
- **ML-assisted optimization** — learned perceptual rendering filters

---

## Research Intersections

This project draws from:
- Typography and font hinting (TrueType bytecode, stem alignment)
- Signal processing (sampling theory, FIR filter design, antialiasing as low-pass filtering)
- Display technology (OLED panel datasheets, subpixel geometry, optical PSF)
- GPU rendering pipelines (fragment shaders, texture sampling)
- Color science (gamma, sRGB, PQ HDR, chromatic adaptation)
- Human visual perception (contrast sensitivity function, spatial frequency response)
