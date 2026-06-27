# KAGE — Development Log

---

## Session 1 — Phase 1 Complete
**Date:** 2026-06-27

### What was done

Phase 1 of the KAGE rendering sandbox is complete. Starting from the pre-existing skeleton (`glyph.rs`, `profile.rs`, `layout/mod.rs`, `layout/pentile.rs`), the full Phase 1 pipeline was built out.

---

### Files added this session

| File | What it does |
|------|--------------|
| `Cargo.toml` | Added `freetype = "0.7"`, `minifb = "0.27"`, `clap = "4"` |
| `src/lib.rs` | Updated: declares all new modules |
| `src/main.rs` | Full CLI (`clap` derive), font loading, three-mode render dispatch, inspector event loop |
| `src/font/mod.rs` | Re-exports `FontFace` |
| `src/font/loader.rs` | FreeType font loader: `rasterize_grey()`, `rasterize_lcd()`, `GlyphBitmap::into_glyph_buffer()` |
| `src/subpixel/mod.rs` | Re-exports `SubpixelGrid`, `SubpixelPixel` |
| `src/subpixel/grid.rs` | `SubpixelGrid`: layout-aware filtering, RGB/BGR stripe FIR, pentile dispatch, `to_argb8_display()` |
| `src/render/mod.rs` | `RenderMode` enum, `render()` dispatch function |
| `src/render/grayscale.rs` | Greyscale AA renderer: coverage → EOTF encoded grey |
| `src/render/subpixel_aa.rs` | ClearType-style subpixel renderer + shared `encode_grid()` helper |
| `src/render/oled_aware.rs` | OLED-aware renderer: layout FIR + adaptive fringe suppression |
| `src/viz/mod.rs` | Re-exports `Inspector` |
| `src/viz/inspector.rs` | minifb inspector: zoom (1–32×), pan, single/side-by-side views, heatmap overlay |

---

### Architecture decisions made

**Why `minifb` and not `winit` + `softbuffer`?**
`minifb` gives a working software framebuffer window in ~5 lines for the MVP. `winit` is the right long-term choice (it's what the README originally listed) but requires significantly more boilerplate to get pixels on screen. The swap is trivial — minifb is only in `viz/inspector.rs` and can be replaced without touching any other module.

**Why greyscale FreeType bitmap as the coverage source, not LCD bitmap?**
The greyscale path gives a single-channel coverage signal that can be fed into any layout filter without pre-baking RGB stripe assumptions. Using the LCD bitmap would require undoing FreeType's built-in LCD filter before re-applying KAGE's own filter — that's lossy and unnecessary. The LCD bitmap is still accessible via `rasterize_lcd()` for comparison purposes.

**Why `f32` linear light throughout `GlyphBuffer` and `SubpixelGrid`?**
All filtering (FIR convolution, adaptive blending, luminance weighting) must happen in linear light to be physically correct. EOTF encoding to signal domain happens once, as the last step before writing to the framebuffer, in `encode_grid()`. Doing this any earlier would corrupt the filter math.

**Adaptive fringe suppression — current approach and its limits.**
The chromatic penalty is the normalised discrete Laplacian of the 1-D coverage signal. This is cheap (O(1) per pixel) and gives a reasonable proxy for edge sharpness. Its limits:
- It's 1-D: misses diagonal strokes where fringing is introduced by 2-D subpixel offsets (relevant for Delta-RGB)
- It doesn't model the human CSF — the penalty is based on signal geometry, not perceptual salience
- Phase 3 will replace/augment this with a frequency-aware perceptual model

**Delta-RGB falls back to greyscale.**
The QD-OLED triangular layout requires a 2-D kernel (vertical subpixel offsets between R/B and G). Building that kernel correctly needs detailed panel geometry data. Rather than implement it incorrectly now, Delta-RGB falls back to greyscale and is marked for Phase 3.

---

### Bugs fixed during implementation

1. **Dead no-op block in `subpixel/grid.rs`**: An `if fringe_suppress && layout == PentileRgbg { // Already applied above }` block was left in after refactoring. It was a no-op but confusing — removed.

2. **Unused imports**: `grayscale.rs` imported `SubpixelLayout` but never used it (greyscale ignores layout). `subpixel_aa.rs` and `oled_aware.rs` imported `SubpixelPixel` but only access its fields via `pixel_mut()` references. Cleaned up.

3. **`_swap` parameter in `rgb_stripe_filter_row`**: The original draft added a `_swap: bool` parameter but the swap was already handled by the caller passing `out_b` and `out_r` in swapped positions for BGR mode. Removed the dead parameter.

4. **`SubpixelPixel` not re-exported**: `subpixel/mod.rs` originally only re-exported `SubpixelGrid`. `grayscale.rs` uses `SubpixelPixel { r, g, b }` struct literal syntax which requires the type to be in scope. Added `SubpixelPixel` to the re-export.

---

### Test coverage added

Every new module has a `#[cfg(test)]` block. Key tests:

| Module | Tests |
|--------|-------|
| `font/loader.rs` | `srgb_to_linear` endpoints and midpoint; grey bitmap roundtrip; LCD channel separation |
| `subpixel/grid.rs` | Greyscale uniform passthrough; RGB stripe uniform passthrough; ARGB8 white/black encoding |
| `render/grayscale.rs` | All channels equal; zero coverage is black; full coverage encodes to 1; EOTF is applied |
| `render/subpixel_aa.rs` | HiDPI falls back to greyscale; WRGB falls back to greyscale; channels differ at hard edge |
| `render/oled_aware.rs` | Fringe suppression reduces channel spread at edge; no suppression in smooth region; HiDPI fallback |

Pre-existing tests in `glyph.rs`, `profile.rs`, `layout/mod.rs`, `layout/pentile.rs` were retained unchanged.

---

### How to build and run

```bash
# Install system FreeType (once)
sudo apt install libfreetype6-dev    # Ubuntu/Debian
brew install freetype                 # macOS

# Build
cargo build --release

# Run with a font on your system
cargo run --release -- \
  --font /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf \
  --glyph g \
  --layout pentile \
  --size 96
```

---

### What to do next (Phase 2)

The immediate next step after checking that Phase 1 builds and the inspector works is to set up `src/simulate/`. Phase 2 adds optical blur between the virtual subpixel grid and the final displayed image — this is what distinguishes *simulating* a real OLED panel from just visualising the raw subpixel energy.

**Phase 2 priorities in order:**

1. **`src/simulate/optical_blur.rs`** — Convolve the `SubpixelGrid` with a Gaussian PSF to simulate how light from neighbouring subpixels bleeds together at viewing distance. Start with a simple separable Gaussian (fast, decomposable into 1-D passes). The PSF sigma should be parameterised in physical units (micrometres) and converted to pixels using the display's DPI.

2. **`src/simulate/gamma.rs`** — Gamma-aware reconstruction: before optical blending, linearise; after blending, re-encode. This is important because blurring in signal space (after EOTF encoding) gives the wrong result — optical physics happen in linear light.

3. **Panel geometry from datasheets** — For Pentile RGBG, the actual subpixel pitch from a Samsung panel datasheet (e.g. the S95C or Odyssey G8) would let the optical blur be physically calibrated rather than approximate.

4. **Subpixel bleed model** — A more detailed model where each colour channel has its own PSF width (OLED emitters for R, G, B have different aperture sizes).

**Phase 2 does not require any new external dependencies** — everything can be done with pure Rust math. A 2-D convolution can be implemented as two 1-D passes for a separable kernel, which is fast enough for the MVP grid sizes.

---

### Open questions / things to measure

- Does the adaptive blend threshold (discrete Laplacian / 2.0) need tuning? At the moment the penalty reaches 1.0 (full greyscale) at a hard step edge. A softer curve might give better visual results on gradual transitions.
- Is green really the best luminance proxy for `adaptive_blend`? Luminance-weighted average `(0.2126R + 0.7152G + 0.0722B)` would be more physically correct.
- For Pentile, the reconstruction of absent R and B from neighbours (`(cl + cr) * 0.5`) is a box average. A tent or Lanczos-2 kernel would reduce aliasing on the reconstructed channels.

---

## Session 2 — Build errors fixed
**Date:** 2026-06-27

### Root cause

Three errors, one common root:

**The `freetype` crate on crates.io at version `0.7.x` is a raw FFI binding** (Servo project origin). It exposes only `freetype::freetype::FT_*` C types and `extern "C"` functions — no `Library`, `Face`, `RenderMode`, or `face::LoadFlag`. The previous `Cargo.toml` used `freetype = "0.7"` which resolved to this raw binding.

The intended crate is **freetype-rs** (PistonDevelopers), which provides the ergonomic safe wrapper with `Library::init()`, `Face`, `RenderMode`, `LoadFlag`, etc. That crate's *package name* on crates.io is `freetype-rs`, but its *lib name* is `freetype` — so `use freetype::Library` works once you declare the dependency correctly.

### Fixes

**`Cargo.toml`** — Use the `package` key to rename the dependency:
```toml
# Before (wrong — resolves to raw FFI crate):
freetype = "0.7"

# After (correct — resolves to PistonDevelopers safe wrapper):
freetype = { package = "freetype-rs", version = "0.38" }
```
Also bumped `minifb` from `0.27` to `0.28` to resolve the deprecation warning.

**`src/font/loader.rs`** — No import changes needed; the imports were already correct for freetype-rs. Two API differences to note for freetype-rs 0.38 vs older versions:
- `get_char_index()` returns `Option<u32>` (not `u32`). The `?` operator handles this cleanly — `None` if the character isn't in the font.
- `bitmap.width()` and `bitmap.rows()` return `i32` (not `u32`). Added `.max(0) as u32` casts.
- `face_index` in `new_face()` is `isize` — `0` is fine (literal `0` coerces to `isize`).

**`src/viz/inspector.rs`** — Replaced deprecated `limit_update_rate()` with `set_target_fps(60)`.

### Lesson

When a crate's lib name differs from its package name, `package = "..."` in Cargo.toml is the fix. Always verify which crate you're actually getting by checking `Cargo.lock` after the first `cargo fetch` — the lock file shows the exact package name and version that resolved.

---

## Session 3 — System library missing (freetype2)
**Date:** 2026-06-27

### Error

```
Package freetype2 was not found in the pkg-config search path.
```

`freetype-sys` (the C binding layer under `freetype-rs`) uses `pkg-config` to locate the system FreeType library at build time. It wasn't installed.

### Two ways to fix this

**Option A — Install the system library (recommended if you want the system FreeType):**
```bash
# Ubuntu / Debian
sudo apt install libfreetype6-dev

# Arch
sudo pacman -S freetype2

# macOS
brew install freetype
```

**Option B — Use the bundled feature (no system library needed, compiles from source):**
```toml
freetype = { package = "freetype-rs", version = "0.38", features = ["bundled"] }
```

The `bundled` feature activates a `cc::Build` path in `freetype-sys/build.rs` that compiles the vendored FreeType C source directly into the binary. It also vendors libpng. First build is slower (compiling C), but works with zero system dependencies. This is what the project now uses.

### Tradeoffs

| | System library | Bundled |
|--|--|--|
| First build speed | Fast | Slow (compiles C) |
| System deps | `libfreetype6-dev` + `pkg-config` | None (just a C compiler) |
| Version control | Whatever distro ships | Pinned to freetype-sys vendored version |
| CI / reproducibility | Fragile across distros | Hermetic |

For a research project where portability matters more than binary size, bundled is the better default.
