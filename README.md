# KAGE — Kinetic Adaptive Glyph Engine
### OLED-Aware Text Rendering Research Sandbox

> A programmable experimental text rendering engine and display simulator for improving text clarity on modern OLED displays with non-standard subpixel layouts (WOLED, Pentile, Triangular).

![KAGE inspector: one glyph antialiased three ways](docs/inspector-raw.png)

*The inspector: one glyph antialiased three ways for a Pentile RGBG panel. The OLED-aware panel looks the most coloured here, and that is the point — press `S` to see the same three as the physical panel would emit them:*

![KAGE inspector: simulated panel view](docs/inspector-simulated.png)

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

**Phases 1 to 4 are complete.** The rendering sandbox, the zoomable inspector, the display simulator and the OLED-aware renderer (each colour sampled at its emitter, from supersampled coverage) are implemented and tested. OpenGL and Vulkan backends compute the same grids, and the inspector supports live tuning with automatic CPU fallback.

| Phase | Status | Description |
|-------|--------|-------------|
| **Phase 1** | ✅ Complete | Rendering sandbox + zoomable inspector |
| **Phase 1.5** | ✅ Complete | Pipeline refactor — renderers return linear light |
| **Phase 2** | ✅ Complete | Physical subpixel geometry, optical PSF blur, per-channel bleed, luminance reconstruction |
| **Phase 3** | ✅ Complete | Emitter-position sampling on every layout, supersampled coverage, CSF-based metric and auto-tuning, panel files |
| **Phase 4** | ✅ Complete | OpenGL / Vulkan backends, CPU fallback, live tuning UI |

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
# Simplest: system default font, glyph 'A' at 64px, pentile layout
cargo run --release

# Pick a glyph, or a specific font
cargo run --release -- --glyph g
cargo run --release -- --font /path/to/font.ttf --glyph A

# Full options
cargo run --release -- \
  --font /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf \
  --glyph g \
  --layout pentile \
  --size 96

# --font is optional: without it KAGE uses the system's default sans-serif
# (via fontconfig's fc-match, falling back to a list of common font paths).
# Available layouts: rgb | bgr | pentile | delta | wrgb | grey
# On Ubuntu, a good test font: /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf
# On macOS:                     /Library/Fonts/Arial.ttf
```

### GPU backends

```bash
cargo run --release -- --backend opengl
cargo run --release -- --backend vulkan --start-simulated
cargo run --release --no-default-features -- --backend cpu
```

CPU is the default. Cargo features `opengl` and `vulkan` are both enabled by default; `--no-default-features` builds without GPU libraries. EGL and Vulkan drivers are loaded at runtime. A missing driver, disabled feature or rendering failure falls back to CPU with a warning; the tuning bar shows the backend actually used and the error. Failed initialisation is remembered.

OpenGL renders a full-target triangle into an RGBA32F texture through an off-screen EGL context. Vulkan dispatches an 8×8 compute shader over host-visible buffers. Both share the GLSL kernels and emitter sampling in `shaders/common.glsl`; neither presents the window. Simulation and auto-tuning stay on the CPU.

The reported render time includes all three modes and readback, excluding backend creation. Drivers can still defer work until the first draw, so the first run can be slower. The tuning bar reports the latest run. For repeatable warm timings, run `cargo run --release --example pipeline_timing -- /path/to/font.ttf` (optional second argument: simulation scale).

The four settings are luma and chroma footprint (0.25–3 px, step 0.05), edge suppression (0–1, step 0.05), and viewing distance (200–1500 mm, step 25). Select with Tab and adjust with brackets. Backend switching and edits keep the current zoom and view. Auto-tuning requires simulation and may take longer than an individual adjustment.

Snapshots include the bar and need no display server:

```bash
cargo run --release -- --backend vulkan --snapshot /tmp/kage.ppm
```

### The display simulation

The panel simulation (emitters at their physical positions, then optical blur) is computed on every run. Press `S` in the inspector to toggle raw ↔ simulated.

```bash
# Open directly in the simulated view
cargo run --release -- --glyph g --layout pentile --start-simulated

# Bare emitters, no blur — see the subpixel structure itself
cargo run --release -- --glyph g --psf-sigma 0 --start-simulated

# Blur derived from viewing distance (mm) and panel DPI
cargo run --release -- --glyph g --viewing-dist 700 --dpi 140

# Save the first frame as an image and exit
cargo run --release -- --glyph g --snapshot frame.ppm
```

| Flag | Default | Meaning |
|------|---------|---------|
| `--no-simulate` | off | Skip the simulation (it is on by default) |
| `--start-simulated` | off | Open in the simulated view |
| `--snapshot <file>` | — | Write the first frame to a PPM image and exit |
| `--psf-sigma <px>` | derived | Blur sigma in logical pixels; `0` disables blur. Overrides the two flags below |
| `--viewing-dist <mm>` | 500 | Eye-to-panel distance |
| `--psf-fwhm <arcmin>` | 1.7 | Angular width of the PSF at the eye |
| `--sim-scale <n>` | 6 | Emission-map samples per pixel per axis (1–16) |
| `--dpi`, `--dpr` | 109, 1.0 | Panel density; also drives the HiDPI greyscale fallback |

### The OLED-aware renderer

```bash
# Use a panel description instead of a built-in layout
cargo run --release -- --panel layouts/delta-rgb.toml --glyph g

# Search for the best footprints for this glyph, panel and distance
cargo run --release -- --layout delta --glyph g --auto-tune

# Set them by hand
cargo run --release -- --luma-footprint 0.8 --chroma-footprint 1.3 --edge-suppression 0.5
```

| Flag | Default | Meaning |
|------|---------|---------|
| `--panel <file>` | — | Panel description (`.toml`): emitter geometry, bleed, layout. See [`layouts/README.md`](layouts/README.md) |
| `--supersample <n>` | 4 | Coverage samples per pixel per axis given to the renderers (1–8) |
| `--luma-footprint <px>` | 1.0 | Width sampled around each green / white emitter |
| `--chroma-footprint <px>` | 1.0 | Width sampled around each red / blue emitter |
| `--edge-suppression <k>` | 0 | How far red / blue move to the luma footprint at strong edges (0–1) |
| `--auto-tune` | off | Pick the three settings above by minimising simulated error |
| `--chroma-cutoff <cpd>` | 4 | Spatial frequency at which the eye's colour sensitivity has halved; defines "visible" fringing |

### Metrics

The CLI prints three numbers per rendering mode, all measured on the simulated panel image:

- **Luminance error** — RMS luminance difference from what an ideal display would show: the glyph rasterized at the simulation's resolution, seen through the same optical blur. The reference has the glyph's real shape between pixel centres, so placing light at the right sub-pixel position is rewarded.
- **Visible chroma** — RMS colour left after the eye's chromatic low-pass. This is fringing. The inspector shows it as "Colour fringing".
- **Chroma residue** — RMS of all colour, including the pixel-scale emitter pattern the eye averages to grey.

Glyph `g` at 20 px, 109 DPI, viewed from 500 mm (nominal panel geometries):

| Layout | Mode | Luminance error | Visible chroma |
|--------|------|-----------------|----------------|
| Pentile RGBG | Greyscale / Subpixel / **OLED-aware** | 0.085 / 0.109 / **0.062** | 0.031 / 0.027 / **0.011** |
| RGB stripe | Greyscale / Subpixel / **OLED-aware** | 0.069 / 0.091 / **0.062** | 0.029 / 0.021 / **0.007** |
| Delta RGB | Greyscale / Subpixel / **OLED-aware** | 0.077 / 0.085 / **0.056** | 0.030 / 0.024 / **0.008** |
| WRGB | Greyscale / Subpixel / **OLED-aware** | 0.078 / 0.078 / **0.064** | 0.004 / 0.004 / 0.004 |

These are simulator results on nominal geometry, not measurements of real panels or of readers.

### Inspector controls

| Input | Action |
|-------|--------|
| Scroll wheel | Zoom in / out, anchored to cursor position |
| `+` / `=`, `-` | Zoom in / out (keyboard) |
| Click + drag | Pan |
| Arrow keys | Pan by one pixel |
| `R` | Reset: fit the glyph to the panel |
| `1` `2` `3` | Show one panel: Greyscale AA / Subpixel AA / OLED-Aware |
| `A` | All three side by side (default) |
| `S` | Toggle raw render ↔ simulated panel |
| `L` | Toggle luminance-only view |
| `E` | Simulated view: auto-exposure ↔ clip |
| `H` | Toggle per-channel heatmap overlay |
| `Tab` / `Shift+Tab` | Select next / previous tuning setting |
| `[` / `]` | Decrease / increase selected setting (repeats while held) |
| `T` | Auto-tune OLED-aware parameters using the CPU simulator |
| `B` | Cycle CPU → OpenGL → Vulkan |
| `Backspace` | Reset settings and backend to their starting values |
| `Esc` | Quit |

The window has a normal title bar, so it can be maximised, tiled or closed like any other. On Wayland desktops KAGE opens through XWayland to get one, because the windowing library draws no title bar of its own on compositors such as GNOME; set `KAGE_BACKEND=wayland` to use the native backend instead.

The window explains itself: a header says what is on screen and which view is active, each panel has a title and a one-line description of its rendering strategy, and the footer lists the controls. Interface text scales up with the window, and long lines wrap rather than run off the edge. The glyph opens fitted to the panel and stays fitted until you zoom or pan — including when the window is maximised. At zoom ≥ 8× faint lines mark pixel boundaries. The heatmap mode (`H`) false-colours the image to make R/B channel imbalance visible. In the simulated view each panel's heading also shows its luminance error and colour fringing (visible chroma).

In the simulated view each logical pixel is drawn from its individual emitters, so zooming in shows the real subpixel structure. The simulated image is luminance-preserving, which makes bare emitters brighter than 1.0; auto-exposure (on by default) scales all three panels by one common factor so they stay comparable, and `E` switches to clipping instead.

---

## Architecture

```
font::loader::FontFace
      │  FreeType rasterization, at pixel resolution and N× finer
      ▼
coverage::Coverage          lo: 1 sample / pixel     hi: N×N samples / pixel   (linear area coverage)
      │
      │     CPU renderers below, or gpu::{opengl, vulkan} with shared GLSL
      ├──▶ render::grayscale    lo copied to R, G, B              ──┐
      ├──▶ render::subpixel_aa  lo through a fixed 3×3 kernel     ──┤──▶ SubpixelGrid (linear light, 1 RGB / pixel)
      └──▶ render::oled_aware   hi averaged over a footprint      ──┘         │
                                centred on each emitter                        │
                                (positions from PanelGeometry)                 │
                                            ┌──────────────────────────────────┤
                                            │                     simulate::simulate_panel()
                                            │                       panel::emit()   emitters at physical positions
                                            │                       psf + bleed     per-channel Gaussian blur
                                            │                              │
                                            │                 SubpixelGrid (linear light, S×S samples / pixel)
                                            │                              │──▶ metrics vs simulate_ideal(coverage at S×)
                                            ▼                              ▼
                                    viz::Inspector   S toggles raw ↔ simulated
                                      (luminance) → (exposure) → render::encode_grid()   ← EOTF applied once
                                             │
                                        framebuffer
```

`pipeline::Pipeline` runs this chain. Everything upstream of `encode_grid()` is linear light. Filtering, blur and luminance sums are only physically meaningful there.

### How the OLED-aware renderer works

A pixel's three colours do not come from the same place. If all three are given the same value (greyscale), a white pixel at the left edge of a stem shows red further left than blue, and the edge is tinted. That mismatch is colour fringing.

The OLED-aware renderer does not filter pixel values. For every colour of every pixel it asks where that colour's emitter sits on this panel, and takes the mean of the glyph's supersampled coverage over a footprint centred there. Each emitter then shows the part of the glyph that is actually in front of it. The raw RGB looks coloured; the light it produces is not.

- **Every layout, one code path.** Emitter positions come from the panel geometry (`layout::EmitterMap`), built-in or loaded from a `.toml` file. RGB/BGR stripe, Pentile RGBG and Delta RGB differ only in that data.
- **WRGB.** The panel routes white through its white emitter, an eighth of a pixel left of centre, so all channels are sampled there: luminance is reconstructed where it is emitted.
- **Frequency-aware.** Coverage is rasterized 4× finer than the pixel grid, so detail between pixel centres reaches the emitters instead of being averaged away first.
- **Luma / chroma decoupling.** Green and white emitters carry luminance, red and blue mostly colour, and the eye resolves luminance much more finely. The two groups have separate footprint widths.
- **Edge-aware chroma suppression.** Where the coverage has a strong edge (2-D Sobel) along an axis in which this panel's colours are displaced, red and blue move to the luminance footprint.
- **Matched to the eye.** `--auto-tune` searches those settings for the lowest luminance error plus *visible* chroma, where visible means surviving the chromatic contrast-sensitivity low-pass at the current viewing distance.

At the default viewing conditions the sweep found one-pixel footprints for all colours and no edge suppression to be at or within about 2 % of the best setting on every built-in layout, so those are the defaults. Narrower luma footprints do win for individual glyphs and at larger sizes (try `--auto-tune`).

### Key data types

| Type | Location | Description |
|------|----------|-------------|
| `GlyphBuffer` | `src/glyph.rs` | Linear-light RGBA f32 pixel buffer |
| `Plane` | `src/plane.rs` | Single-channel f32 image, clamp-to-edge sampling |
| `Coverage` | `src/coverage.rs` | Glyph coverage at pixel resolution and supersampled |
| `GlyphBitmap` | `src/font/loader.rs` | Raw FreeType bitmap (u8, grey or LCD) |
| `FontFace` | `src/font/loader.rs` | FreeType face with pixel size set |
| `SubpixelLayout` | `src/layout/mod.rs` | Enum of known panel geometries |
| `FilterKernel` / `BaseFilter` | `src/layout/mod.rs` | Fixed 3×3 taps per colour channel; which one a layout uses |
| `EmitterMap` | `src/layout/emitters.rs` | Emitter position per colour per pixel of the unit cell |
| `SubpixelGrid` | `src/subpixel/grid.rs` | Per-pixel (R,G,B) energy after rendering |
| `RenderParams` | `src/render/perceptual.rs` | OLED-aware settings: footprints, edge suppression |
| `DisplayProfile` | `src/profile.rs` | EOTF, white point, DPI, DPR |
| `RenderMode` | `src/render/mod.rs` | Greyscale / SubpixelAa / OledAware |
| `PanelGeometry` | `src/simulate/panel.rs` | Unit cell of emitters: position, aperture, colour |
| `PanelDescription` | `src/simulate/panel_file.rs` | A panel loaded from `.toml` |
| `SimulationParams` | `src/simulate/mod.rs` | PSF sigma and per-channel bleed |
| `Backend` / `Gpu` / `RenderJob` | `src/gpu/mod.rs` | Backend selection, lazy drivers and shared shader inputs |
| `Pipeline` / `Tuning` | `src/pipeline.rs` | Glyph → renderers → simulation → metrics; auto-tune |
| `ViewSet` | `src/viz/inspector.rs` | The three modes' grids at one pipeline stage |
| `Inspector` | `src/viz/inspector.rs` | minifb window, zoom/pan/heatmap/simulate |

---

## Module Reference

### `src/glyph.rs` — `GlyphBuffer`
Flat `[R, G, B, A: f32]` buffer in linear light (before EOTF encoding). The alpha channel holds greyscale coverage for compositing. R/G/B hold per-channel subpixel values when subpixel rendering is active. Provides `encode_eotf()` and `to_rgba8()`.

### `src/profile.rs` — `DisplayProfile`
Describes the physical display: EOTF (sRGB, Gamma, PQ), white point (D65, DCI-P3), DPI, and device pixel ratio. `is_hidpi()` returns true at ≥192 effective DPI, triggering greyscale fallback. `sdr_oled()` constructs a sensible default for typical Samsung OLED monitors.

### `src/plane.rs`, `src/coverage.rs` — `Plane`, `Coverage`
`Plane` is a scalar image whose reads clamp to the edge. `Coverage` holds a glyph twice: `lo`, one sample per pixel, and `hi`, `scale × scale` samples per pixel, with `lo` the box average of `hi`. `Coverage::rasterize()` draws both from a `FontFace`; `sample_box()` averages `hi` over a rectangle at any sub-pixel position. Coverage is linear area coverage — no transfer function is applied.

### `src/layout/` — layouts, kernels, emitter positions
- `mod.rs` — `SubpixelLayout` (`RgbStripe`, `BgrStripe`, `PentileRgbg`, `DeltaRgb`, `Wrgb`, `Greyscale`), `FilterKernel` (separable 3×3 taps per channel) and `BaseFilter` (which kernel a layout's Subpixel AA baseline uses).
- `pentile.rs` — Pentile RGBG taps (`GREEN_FIR`, `RED_FIR`, `BLUE_FIR`, `ABSENT_FIR`) and the Phase 1 row filter and Laplacian penalty.
- `delta.rs` — Delta-RGB 2-D kernel: each channel interpolated at its emitter's offset, horizontally and vertically.
- `emitters.rs` — `EmitterMap::from_geometry()`: the area-weighted centroid of each colour's emitters in each pixel of the unit cell (up to 2×2). Panels with a white emitter map all three channels to it.

### `src/font/loader.rs` — `FontFace`, `GlyphBitmap`
FreeType-backed loader. `rasterize_grey()` produces 8-bit coverage; `rasterize_grey_scaled(ch, n)` draws the same hinted outline on a grid `n` times finer; `rasterize_lcd()` produces FreeType's own LCD-filtered output.

### `src/subpixel/grid.rs` — `SubpixelGrid`
One RGB value per logical pixel. `from_coverage(plane, filter)` applies a fixed kernel; `from_glyph(buf, layout, fringe_suppress)` is the Phase 1 entry point and still offers the Laplacian blend on Pentile. All filters clamp to the edge, so every column is written whatever the glyph width.

### `src/render/` — Three rendering strategies

| Module | Strategy | Input |
|--------|----------|-------|
| `grayscale.rs` | All channels = coverage | pixel coverage |
| `subpixel_aa.rs` | Fixed kernel per layout (stripe phase shift, Pentile FIR, Delta 2-D) — the baseline | pixel coverage |
| `oled_aware.rs` + `perceptual.rs` | Footprint average at each emitter | supersampled coverage + `EmitterMap` |
| `csf.rs` | Luminance and chromatic contrast-sensitivity models, and the Gaussian widths derived from them | — |

All renderers return **linear-light** grids and fall back to greyscale on HiDPI panels. Subpixel AA is also greyscale on WRGB. `render_coverage()` is the dispatch that takes a `Coverage`; `render()` is the convenience form for a `GlyphBuffer`. `encode_grid()` is the single point where EOTF encoding happens.

### `src/simulate/` — Display simulation

| Module | Role |
|--------|------|
| `panel.rs` | `PanelGeometry`: emitters per unit cell. `emit()` paints a grid onto an oversampled emission map; `integrate_pixels()` goes back to one value per pixel |
| `psf.rs` | Gaussian kernel builder and separable convolution, with clamp or black borders |
| `bleed.rs` | `BleedProfile`: per-channel sigma multipliers (red 1.20, green 1.00, blue 1.10) |
| `panel_file.rs` | `PanelDescription`: geometry, bleed and layout from a `.toml` file |
| `gamma.rs` | Viewing distance → sigma, linear-light guard, luminance reconstruction, luminance-error, chroma and visible-chroma metrics |
| `mod.rs` | `SimulationParams`, `simulate()` (per-pixel blur only), `simulate_panel()` (emitters + blur), `simulate_ideal()` (the metrics' reference) |

**Panel geometries.** `PanelGeometry::for_layout()` returns a preset for each `SubpixelLayout`. The presets encode each layout's *topology* — which emitters exist, their order and relative placement — with **nominal** aperture sizes. They are not measurements of a particular panel. Measured values go in a panel file (`--panel`, format in [`layouts/README.md`](layouts/README.md)), in pixels or micrometres; the files shipped in `layouts/` reproduce the nominal presets as templates.

| Layout | Unit cell | Emitters |
|--------|-----------|----------|
| RGB / BGR stripe | 1×1 | Three tall bars |
| Pentile RGBG | 2×2 | Narrow green in every pixel, larger red or blue alternating in a checkerboard |
| Delta RGB | 1×1 | Green above; red and blue side by side about half a pixel below |
| WRGB | 1×1 | Four bars, R W B G; the achromatic part of a pixel goes to white |
| Greyscale | 1×1 | One full-pixel source |

**Energy.** Emission is luminance-preserving: each emitter is driven brighter than the pixel value by the inverse of its channel's fill factor, so a white field integrates back to white on every layout.

**Blur.** Sigma is in logical pixels. It comes from `--psf-sigma`, or from a fixed angular PSF at the eye: `sigma_px = (fwhm_rad × distance / pitch) / 2.355`. Because the PSF is an angle, sigma in pixels grows with viewing distance and with pixel density. The simulated image has the glyph bitmap's extent; blur that would land outside it is dropped.

### `src/gpu/` — GPU backends
`mod.rs` defines `Backend`, `RenderJob`, the std140 `ShaderParams` block and lazy `Gpu` initialisation. `opengl.rs` runs fragment shaders through dynamically loaded EGL; `vulkan.rs` compiles GLSL with naga and dispatches compute work through ash. `shaders.rs` assembles the shared files in `shaders/`. Both return linear `SubpixelGrid`s, with parity tests for every layout and mode.

### `src/pipeline.rs` — `Pipeline`
Owns the glyph coverage, the panel and the profile; `run(&Tuning)` renders the three modes, simulates and scores them, caching what a settings change does not affect. All three modes run on the selected backend; errors recompute the whole run on CPU and are reported in `Output`. Baseline simulation caches retain CPU grids to avoid invalidation by insignificant GPU rounding. `auto_tune()` is a coordinate descent over footprint widths and edge suppression that minimises luminance error plus visible chroma.

### `src/viz/inspector.rs` — `Inspector`
minifb window with a header, three titled panels, a tuning bar and a key-hint footer; zoom (1–64×), pan, single-panel and side-by-side modes, a raw ↔ simulated toggle, a luminance-only view, and a false-colour heatmap overlay for channel imbalance analysis. `attach_pipeline()` enables live settings, auto-tuning and backend switching. `--snapshot` uses the same drawing code without opening a window. It holds linear-light `ViewSet`s, re-encodes only when the view changes, and redraws only when something on screen changes.

### `src/viz/text.rs` — `TextRenderer`
Draws the inspector's own interface text: glyphs rasterized through `FontFace`, cached per size and blended in linear light. Uses the system default font. At zoom ≥ 8× a pixel grid is drawn over the zoomed output. The label strip uses colour-coded accent bars (blue = greyscale, orange = subpixel AA, green = OLED-aware).

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

### Phase 2 — Display Simulation ✅
- [x] Optical PSF (point spread function) convolution — `src/simulate/psf.rs`
- [x] Subpixel light bleed and lateral diffusion — `src/simulate/bleed.rs`
- [x] Model physical OLED subpixel arrangements — `src/simulate/panel.rs` (nominal presets per layout; datasheet values plug into `PanelGeometry`)
- [x] Gamma-aware luminance reconstruction — `src/simulate/gamma.rs`
- [x] PSF sigma derived from viewing distance and DPI
- [x] Inspector: raw ↔ simulated toggle, luminance view, exposure control
- [x] Luminance-error and chroma-residue metrics printed per rendering mode

### Phase 3 — Experimental Rendering Techniques ✅
- [x] Luma/chroma decoupling — separate footprints for luminance-carrying (G, W) and colour-carrying (R, B) emitters
- [x] Edge-aware chroma suppression beyond the Laplacian — 2-D Sobel, weighted by the axes the panel fringes in
- [x] Frequency-aware rendering — supersampled coverage sampled at emitter positions
- [x] Perceptual filtering matched to the CSF — chromatic-CSF visible-chroma metric; `--auto-tune` minimises it with luminance error
- [x] Delta-RGB (QD-OLED) 2-D kernel — `src/layout/delta.rs`
- [x] WRGB white-channel luminance reconstruction — sampled at the white emitter
- [x] Fringe suppression for RGB/BGR stripe — same emitter-sampling path as every other layout
- [x] Per-panel geometry and bleed loaded from `.toml` — `--panel`, `layouts/` (the loader is done; the shipped files are nominal, no measured panel data exists yet)

### Phase 4 — GPU Acceleration ✅
- [x] OpenGL rendering backend
- [x] GLSL fragment shader implementations of all three render modes
- [x] Vulkan compute shader backend
- [x] Interactive real-time parameter tuning UI
- [x] `src/gpu/` module

---

## Technical Stack

| Component | Technology |
|-----------|------------|
| Language | Rust (edition 2021) |
| Font loading & rasterization | `freetype` crate (FreeType 2) |
| Windowing / framebuffer | `minifb` software presentation |
| CLI | `clap` v4 (derive API) |
| Panel files | `toml` + `serde` |
| Rendering — Phases 1–3 | CPU software renderer |
| Rendering — Phase 4 | `glow` + dynamic `khronos-egl`; `ash` + `naga` GLSL → SPIR-V |

---

## Repository Structure

```
kage/
├── src/
│   ├── main.rs                    # CLI entry point (clap), event loop
│   ├── lib.rs                     # Crate root, module declarations
│   ├── glyph.rs                   # GlyphBuffer — linear-light RGBA f32 buffer
│   ├── plane.rs                   # Plane — single-channel image, Gaussian blur
│   ├── coverage.rs                # Coverage — pixel + supersampled glyph coverage
│   ├── pipeline.rs                # Pipeline — render, simulate, score, auto-tune
│   ├── profile.rs                 # DisplayProfile — EOTF, white point, DPI
│   ├── gpu/                       # Lazy backends, shared parameters, OpenGL and Vulkan
│   ├── font/
│   │   ├── mod.rs                 # Re-exports FontFace
│   │   └── loader.rs              # FreeType loader, scaled rasterization
│   ├── layout/
│   │   ├── mod.rs                 # SubpixelLayout, FilterKernel, BaseFilter
│   │   ├── pentile.rs             # Pentile RGBG taps, chromatic_penalty()
│   │   ├── delta.rs               # Delta-RGB 2-D kernel
│   │   └── emitters.rs            # EmitterMap — emitter positions per pixel
│   ├── subpixel/
│   │   ├── mod.rs                 # Re-exports SubpixelGrid, SubpixelPixel
│   │   └── grid.rs                # SubpixelGrid, fixed-kernel filtering
│   ├── render/
│   │   ├── mod.rs                 # RenderMode, render(), render_coverage(), encode_grid()
│   │   ├── grayscale.rs           # Greyscale AA renderer
│   │   ├── subpixel_aa.rs         # Fixed-kernel subpixel renderer (baseline)
│   │   ├── oled_aware.rs          # OLED-aware renderer, GlyphBuffer entry points
│   │   ├── perceptual.rs          # Emitter-position sampling, RenderParams
│   │   └── csf.rs                 # Contrast-sensitivity models
│   ├── simulate/
│   │   ├── mod.rs                 # SimulationParams, simulate_panel(), simulate_ideal()
│   │   ├── panel.rs               # PanelGeometry, emit(), integrate_pixels()
│   │   ├── panel_file.rs          # PanelDescription — .toml panel files
│   │   ├── psf.rs                 # Gaussian kernels, separable convolution
│   │   ├── bleed.rs               # Per-channel bleed profile
│   │   └── gamma.rs               # Sigma from viewing distance, luminance, metrics
│   └── viz/
│       ├── mod.rs                 # Re-exports Inspector, ViewSet
│       ├── inspector.rs           # minifb window: header, panels, zoom, pan, raw/simulated views
│       ├── tuning.rs              # Live setting ranges, steps and accessors
│       └── text.rs                # Text drawing for the inspector's interface
├── shaders/                       # Shared GLSL, fragment and compute wrappers
├── examples/                      # Reproducible pipeline timing harness
├── layouts/                       # Panel description files (.toml) and their format
├── docs/                          # Screenshots used in this README
├── Cargo.toml
├── devlog.md
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
