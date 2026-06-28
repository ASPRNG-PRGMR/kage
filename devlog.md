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

---

## Session 4 — Scroll-wheel zoom + adaptive window sizing
**Date:** 2026-06-28

### Changes

**`src/viz/inspector.rs`** only — no other files touched.

**Scroll-wheel zoom anchored to cursor:**

`get_scroll_wheel()` returns `Option<(f32, f32)>` where `y > 0` is scroll-up (zoom in). The anchor math keeps the source pixel under the cursor fixed as zoom changes:

```
src = pan + mouse / old_zoom      ← source pixel under cursor before
pan = src - mouse / new_zoom      ← pan that keeps it there after
```

The label strip height (`LABEL_H = 20px`) is subtracted from the mouse y coordinate so the anchor is in the glyph area coordinate space, not the full window coordinate space.

Zoom range stays 1–32×. Each scroll tick moves by 1 zoom level.

**Adaptive window sizing (80% of screen):**

minifb exposes no screen size query and no fullscreen API. The window is instead sized to 80% of the detected primary display resolution at startup, which feels native without covering the taskbar.

Screen resolution is detected via a three-step fallback chain:

1. **`/sys/class/drm/card*/card*-*/modes`** — reads kernel DRM sysfs, works on both X11 and Wayland, zero subprocesses. First line of the `modes` file is the preferred/active resolution in `WxH` format.
2. **`xrandr` subprocess** — parses the line containing `*` (active mode marker). X11 and XWayland only.
3. **Hardcoded fallback** — 1280×800 if neither source is readable.

The computed size has a minimum floor of 640×400 so the window is always usable even if the detection returns a tiny value. `resize: true` remains set so the WM can resize or maximise freely after open.

**`MouseMode::Clamp`** added to imports — returns mouse coords clamped inside the window boundary, giving no negative coords when the cursor is at the edge during a scroll.

---

## Session 5 — Phase 1.5: Pipeline refactor (linear-light renderers)
**Date:** 2026-06-28

### Why this had to happen before Phase 2

Phase 2 adds optical blur simulation between the renderer output and the framebuffer. Blur is a convolution — it must happen in linear light. The old pipeline applied EOTF encoding inside each renderer, meaning the grids reaching the inspector were already in sRGB signal space. Blurring a gamma-encoded signal produces the wrong result: dark regions would blur as if they carried more energy than they physically do.

The fix is to push EOTF encoding to the very end of the pipeline, after all filtering and simulation are complete.

### What changed

**`src/render/grayscale.rs`**
- Removed `profile.eotf.encode()` call inside the pixel loop
- Now returns raw linear-light coverage values
- Tests updated: `output_is_linear_not_encoded` replaces the old `eotf_is_applied` test (which was asserting the wrong behaviour)

**`src/render/subpixel_aa.rs`**
- `encode_grid()` call removed from `render_subpixel()`
- `encode_grid()` function itself moved to `render/mod.rs`
- Now returns linear-light `SubpixelGrid` directly from `SubpixelGrid::from_glyph()`

**`src/render/oled_aware.rs`**
- `use super::subpixel_aa::encode_grid` import removed
- `encode_grid()` call removed
- Returns linear-light grid directly

**`src/render/mod.rs`**
- `encode_grid()` moved here and made `pub` — single encoding point for the whole crate
- Doc comment updated to explain the linear-light contract
- New tests: `encode_grid_applies_eotf`, `encode_grid_is_idempotent_at_endpoints`

**`src/viz/inspector.rs`**
- `update()` gains a `profile: &DisplayProfile` parameter
- Calls `encode_grid(grid.clone(), profile)` for each of the three grids before passing to `draw()`
- `use crate::render::encode_grid` and `use crate::profile::DisplayProfile` added to imports
- The clone cost is negligible: grids are small (glyph-sized, not full-screen)

**`src/main.rs`**
- `inspector.update()` call gains `&profile` as fourth argument

### Pipeline before and after

```
Before:
  render() → EOTF-encoded SubpixelGrid → Inspector blits directly

After:
  render() → linear SubpixelGrid → encode_grid() → Inspector blits
                                 ↗
                     simulate::* (Phase 2) plugs in here
```

### Test strategy note

The comparison tests (`fringe_suppression_reduces_channel_spread_at_edge`, `no_suppression_in_smooth_region`) now compare two linear-light grids against each other. This is actually more correct than before: we're measuring the raw filter output difference, unconfounded by the nonlinearity of sRGB encoding.

---

## Session 6 — Scroll zoom fix + window centering
**Date:** 2026-06-28

### Issues reported

1. **Scroll moved the view left/right instead of zooming.** Upscroll panned right, downscroll panned left.
2. **Window opened in the top-left corner** instead of the centre of the screen.

### Root cause — scroll

The old zoom was `u32`, delta was `±1`. At zoom=8, changing to 9 is an 11% step — barely noticeable visually. The anchor pan correction (`pan = src - mouse/new_zoom`) was proportionally larger than the zoom change and dominated the frame, making it look like pure panning with no zoom. The fix is multiplicative zoom: each scroll tick multiplies by `1.2×` (zoom in) or divides by `1.2×` (zoom out). At any zoom level this is a consistent 20% visual step — clearly perceptible and correctly anchored.

`zoom` field changed from `u32` to `f32`. All downstream uses updated:
- `draw_panel`: pixel mapping uses `(wx as f32 / zoom) as i32` instead of `wx / zoom as i32`
- `draw_pixel_grid`: takes `zoom: f32`, rounds to `usize` for modulo grid line positioning
- `draw_pixel_grid`: fixed the vertical line condition from `x % zoom == 0` (absolute) to `(x - panel_x) % zoom_i == 0` (relative to panel origin — the old code drew grid lines at wrong positions when panel_x was non-zero)
- Keyboard `+`/`-` also updated to use the same `1.2×` multiplier for consistency
- Zoom range: 1.0–64.0 (extended upper bound from 32 to 64 since float zoom allows finer control)

### Root cause — centering

minifb opens windows at a system-default position (top-left or WM-chosen). `set_position()` exists on `Window` and takes `(isize, isize)` in screen coordinates. Called immediately after `Window::new()`:

```rust
let cx = ((screen_w - win_w) / 2) as isize;
let cy = ((screen_h - win_h) / 2) as isize;
win.set_position(cx, cy);
```

The screen dimensions come from the same `detect_screen_size()` already used for sizing, so no new detection code was needed.

### Files changed

`src/viz/inspector.rs` only.

---

## Session 7 — Scroll zoom: Wayland axis handling
**Date:** 2026-06-28

### Issue

Scroll still panned left/right instead of zooming, confirmed in screenshot.

### Root cause

Two things were wrong:

**1. We were only reading `scroll_y` and discarding `scroll_x`.**
On Wayland (`Axis::VerticalScroll → scroll_y`, `Axis::HorizontalScroll → scroll_x`) this should be fine — but on some Wayland compositors or with certain pointer devices, vertical scroll comes through on `scroll_x` instead. The fix: read both axes, use whichever has the larger magnitude.

**2. Wayland scroll sign vs X11.**
On X11: `Button4` (scroll up) → `scroll_y = +1.0`. Positive = up = zoom in. Correct.
On Wayland: `Axis::VerticalScroll` positive = scroll **down** (inverted relative to X11). Additionally, GNOME on Fedora enables "natural scrolling" by default which inverts again. The net effect depends on the user's settings. A diagnostic print (`eprintln!`) is added in debug builds (`#[cfg(debug_assertions)]`) so the raw scroll values can be observed in the terminal. If zoom goes the wrong direction, the sign of `raw` in the code is the thing to flip.

**3. Threshold too high.**
Old threshold was `> 0.1`. Wayland scroll values can be fractional and accumulate differently. Lowered to `> 0.05`.

### What the diagnostic shows

Run with `cargo run -- ...` (debug build) and scroll. The terminal will print:
```
[kage] scroll raw=(-0.300, 0.000)  using=-0.300
```
This tells you which axis is active and what sign. If zoom goes the wrong direction swap the `> 0.0` / `< 0.0` condition. This print only appears in debug builds and will be removed once confirmed working.

### Keyboard zoom

`+`/`-` keys always work regardless of platform scroll issues. Use these as a workaround while diagnosing.

---

## Session 8 — Alt-tab shrink fix; scroll still under investigation
**Date:** 2026-06-28

### Alt-tab window shrinking

On Wayland, when focus is lost (alt-tab, switching workspaces), GNOME sends an XDG Toplevel `Configure` event with a compositor-chosen smaller size. minifb accepts it if `resize: true` is set, causing the window to shrink. There is no `set_min_size` API on minifb's `Window`.

Fix: only accept size changes that are **larger** than the current size. This allows user-initiated resize (dragging a corner always increases size) while ignoring compositor-driven shrinks during focus loss.

```rust
let (new_w, new_h) = self.window.get_size();
if new_w > self.win_w || new_h > self.win_h {
    self.win_w = new_w;
    self.win_h = new_h;
}
```

### Scroll — still needs terminal diagnostic

The scroll diagnostic (`[kage] scroll raw=(...)`) prints to stderr when running with `cargo run` (debug build). Run the app from a terminal, scroll, and check what it prints. Three possible outcomes:

1. **Nothing prints** — scroll events aren't reaching the app at all (Wayland seat version issue or compositor not forwarding them)
2. **`scroll_x` is non-zero, `scroll_y` is zero** — events on wrong axis (code already handles this with `max(|x|, |y|)`)
3. **Values print but zoom goes wrong direction** — sign is inverted; swap the `> 0.0` condition

---

## Session 9 — Window sizing: hard minimum clamp; centering not possible on Wayland
**Date:** 2026-06-28

### Centering

`set_position()` is a no-op on Wayland. The Wayland protocol does not allow applications to set their own window position — placement is exclusively the compositor's job. On GNOME Wayland, new windows are placed by Mutter based on its own rules (usually top-left or cascaded). There is no workaround available through minifb. The `set_position()` call was removed from `Inspector::new()`. For Phase 4, switching to `winit` would allow using `request_inner_size` and the compositor will honour placement hints via xdg-activation or similar, but still not guarantee position.

### Alt-tab shrinking / initial tiny window

Root cause: on Wayland, `get_size()` returns whatever size the compositor last sent via `Configure`. GNOME sends a Configure immediately on window creation and again on focus-loss. The previous "only grow" fix failed because `win_w/win_h` was initialised from `get_size()` in the struct, not from the computed target size.

Fix: added `min_win_w` and `min_win_h` fields, set to the intended open size computed before `Window::new()`. Every frame, `get_size()` is clamped to these minimums:

```rust
self.win_w = new_w.max(self.min_win_w);
self.win_h = new_h.max(self.min_win_h);
```

This also satisfies the "don't shrink beyond glyph bounds" requirement — the minimum is the size at which all three panels are fully visible and the glyph fills the panel area at the initial zoom level.

### What cannot be fixed in minifb

- Window position on Wayland (compositor-controlled)
- True minimum size enforcement (no `set_min_size` API)
- GNOME overview thumbnail scaling (that's the WM, not our window)

All three are fixable by switching to `winit` in Phase 4.
