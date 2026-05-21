# KAGE - Kinetic Adaptive Glyph Engine
### OLED-Aware Text Rendering Research Sandbox

> A programmable experimental text rendering engine and display simulator for improving text clarity on modern OLED displays with non-standard subpixel layouts (WOLED, Pentile, Triangular).

---

## Why This Project Exists

Traditional subpixel rendering — most notably Microsoft's **ClearType** — was engineered around **RGB stripe LCD** displays. Each pixel in that layout contains three predictably ordered subpixels: Red, Green, Blue, from left to right.

Modern OLED displays break that assumption entirely. Common OLED subpixel layouts include:

| Layout | Description |
|--------|-------------|
| **WRGB (WOLED)** | Adds a White subpixel alongside R, G, B |
| **Pentile** | Alternating RG / BG pairs; fewer subpixels per pixel |
| **Triangular** | Subpixels arranged in a triangle, not a row |
| **Asymmetric** | Vendor-specific irregular arrangements |

Running standard LCD subpixel rendering on these displays causes:
- **Color fringing** — colored halos around letterforms
- **Reduced sharpness** — blurred or mushy glyph edges
- **Chromatic edge artifacts** — incorrect color bleeding at transitions
- **Inconsistent luminance** — uneven brightness across glyph strokes

Most Linux rendering pipelines respond by either assuming RGB stripe (wrong) or disabling subpixel rendering entirely and falling back to grayscale antialiasing (safe but blurry). Neither is satisfying.

This project explores a third path: **display-aware perceptual text reconstruction** — rendering decisions made with explicit knowledge of the physical subpixel structure of the target display.

---

## Project Goal

Build a programmable text rendering **research platform** capable of:

- Simulating arbitrary subpixel layouts (RGB, WRGB, Pentile, custom)
- Rendering font glyphs directly onto virtual subpixel grids
- Experimenting with OLED-aware antialiasing techniques
- Studying perceptual reconstruction methods
- Reducing color fringing without sacrificing sharpness
- Comparing rendering strategies both visually (zoom/inspect) and quantitatively (metrics)

**This is not a font smoothing utility.** It is a rendering and display simulation research framework.

---

## Core Concept

Instead of rendering text to conventional pixels and then sampling, the renderer operates **directly on explicit subpixel structures**.

Each subpixel is treated as an individually addressable unit of light. The renderer models:

- **Subpixel energy distribution** — how much energy each subpixel carries
- **Optical blending** — how adjacent subpixels blend at viewing distance
- **Chromatic edge behavior** — color artifacts that emerge at glyph boundaries
- **Perceptual luminance reconstruction** — how the human visual system interprets the resulting image

### Example Subpixel Layouts

```
Standard RGB Stripe        WOLED (WRGB)           Pentile-like
────────────────────       ──────────────────     ──────────────
[R][G][B]                  [W][R][G][B]           [R][G]
[R][G][B]                  [W][R][G][B]           [B][G]
[R][G][B]                  [W][R][G][B]           [R][G]
```

The renderer encodes each layout as a programmable subpixel map. New layouts can be defined and tested without changing the rendering core.

---

## Development Phases

### Phase 1 — Rendering Sandbox *(current focus)*
- [ ] Load TrueType / OpenType fonts via FreeType
- [ ] Rasterize individual glyphs
- [ ] Render onto programmable subpixel layout grids
- [ ] Zoom and inspect subpixel-level output interactively

### Phase 2 — Display Simulation
- [ ] Simulate optical blur (viewing distance, panel aperture)
- [ ] Simulate subpixel light bleed and lateral diffusion
- [ ] Model actual OLED subpixel arrangements from datasheets
- [ ] Implement gamma-aware luminance reconstruction

### Phase 3 — Experimental Rendering Techniques
- [ ] OLED-aware subpixel antialiasing
- [ ] Luma/chroma decoupling (sharpen luma, soften chroma)
- [ ] Edge-aware chroma suppression (reduce fringing at boundaries)
- [ ] Frequency-aware rendering (preserve high-frequency detail)
- [ ] Perceptual filtering matched to human contrast sensitivity

### Phase 4 — GPU Acceleration
- [ ] Real-time GLSL shader-based rendering
- [ ] OpenGL rendering backend
- [ ] Vulkan backend (later)
- [ ] Interactive parameter tuning UI

---

## First Milestone (MVP)

A **zoomable subpixel visualization tool** that can:

1. Load a font and render a large glyph (e.g., 64px–128px)
2. Display the glyph on multiple simulated subpixel layouts side by side
3. Compare three rendering modes:
   - Grayscale antialiasing (baseline)
   - Standard RGB subpixel antialiasing (ClearType-style)
   - OLED-aware rendering (experimental)
4. Visualize raw subpixel energy per channel as a heatmap

This MVP is entirely software-rendered — no GPU required at this stage.

---

## Technical Stack

| Component | Technology |
|-----------|------------|
| **Language** | Rust |
| **Font loading & rasterization** | FreeType (via Rust bindings) |
| **Windowing** | `winit` or `glfw` |
| **Rendering — Phase 1** | Software (CPU) |
| **Rendering — Phase 3+** | OpenGL with GLSL shaders |
| **Rendering — Phase 4** | Vulkan with compute shaders |
| **Shader language** | GLSL |

---

## Research Intersections

This project sits at the intersection of several fields. Expect to read papers and references from:

- Typography and font hinting
- Signal processing (sampling theory, antialiasing as low-pass filtering)
- Display technology (OLED panel datasheets, subpixel geometry)
- GPU rendering pipelines
- Color science (gamma, color spaces, chromatic adaptation)
- Human visual perception (contrast sensitivity, spatial frequency response)

---

## Long-Term Vision

If the research is productive, potential directions include:

- **Compositor integration** — apply OLED-aware rendering at the Wayland/X11 compositor level
- **Linux desktop integration** — patches or plugins for fontconfig / cairo / FreeType
- **Panel-specific rendering profiles** — per-display calibration files
- **Adaptive calibration** — measure display characteristics at runtime
- **ML-assisted optimization** — learned perceptual rendering filters

---

## Current Status

**Early research and architecture phase.**

The subpixel layout model and rendering sandbox architecture are being designed. No GPU code yet — Phase 1 is CPU-only.

---

## Getting Started (once MVP is ready)

```bash
# Build
cargo build --release

# Run the visualization tool with a test font
cargo run -- --font /path/to/font.ttf --glyph A --layout woled

# Layouts: rgb | woled | pentile | custom:<layout_file>
```

> This section will be expanded as the codebase develops.

---

## Repository Structure (planned)

```
oled-text-renderer/
├── src/
│   ├── main.rs               # Entry point, windowing
│   ├── font/
│   │   └── loader.rs         # FreeType font loading and rasterization
│   ├── subpixel/
│   │   ├── layout.rs         # Subpixel layout definitions (RGB, WRGB, Pentile)
│   │   └── grid.rs           # Virtual subpixel grid rendering
│   ├── render/
│   │   ├── grayscale.rs      # Baseline grayscale AA
│   │   ├── subpixel_aa.rs    # Standard RGB subpixel AA
│   │   └── oled_aware.rs     # Experimental OLED-aware rendering
│   ├── simulate/
│   │   ├── optical_blur.rs   # Optical blending simulation
│   │   └── gamma.rs          # Gamma-aware reconstruction
│   └── viz/
│       └── inspector.rs      # Zoomable subpixel inspector UI
├── assets/
│   └── fonts/                # Test fonts
├── layouts/
│   └── *.toml                # Custom subpixel layout definitions
├── shaders/                  # GLSL shaders (Phase 3+)
├── Cargo.toml
└── README.md
```
