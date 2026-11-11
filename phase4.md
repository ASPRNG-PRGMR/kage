# Phase 4 — GPU acceleration and live tuning

Instructions for implementing Phase 4 of KAGE. Written for a model (or person) starting with no context beyond this repository. Read the README's Architecture section and `devlog.md` Session 14 first; this document assumes them.

## Goal

Tick the five Phase 4 boxes in the README:

1. OpenGL rendering backend
2. GLSL fragment shader implementations of all three render modes
3. Vulkan compute shader backend
4. Interactive real-time parameter tuning UI
5. `src/gpu/` module

Done means: both backends produce the same `SubpixelGrid` as the CPU renderers (to float rounding) for every layout and mode, the inspector can switch between CPU / OpenGL / Vulkan and change the OLED-aware settings while running, and a machine with no GPU driver still runs everything on the CPU.

## Starting point

### What already exists on `main`

| Thing | Where | Why it matters here |
|-------|-------|---------------------|
| The three renderers | `render::render_coverage(cov, mode, layout, map, profile, params)` | The reference the GPU must match |
| Which path a mode takes | `render::samples_at_emitters`, `render::base_filter` | Greyscale fallbacks (HiDPI, WRGB in Subpixel AA) — reproduce them exactly |
| Fixed kernels | `layout::FilterKernel { h, v, pentile }`, `FilterKernel::sample_channel` | Mode "kernel" in the shader |
| Emitter sampling | `render::perceptual::render_at_emitters`, `edge_strength`, `EDGE_LO/HI`, `FOOTPRINT_HEIGHT` | Mode "emitters" in the shader |
| Box footprint | `coverage::Coverage::sample_box` | Port line for line |
| Emitter positions | `layout::EmitterMap` (cells up to `MAX_CELL` = 2) | Uploaded as 12 sites |
| The chain | `pipeline::Pipeline::run(&Tuning)`, `run_with(tuning, oled_grid, time)`, `auto_tune` | Where a backend plugs in |
| The window | `viz::Inspector` (minifb, software framebuffer) | Stays as is; gains a tuning bar |

### The branch `phase4-gpu-backends`

Items 1, 2, 3 and 5 were prototyped before Phase 4 was put on hold. The branch holds two commits on top of `main`:

- `feat(gpu): OpenGL backend with GLSL fragment shader for all three modes`
- `feat(gpu): Vulkan compute shader backend`

They add `src/gpu/{mod,shaders,opengl,vulkan}.rs`, `shaders/{params,common}.glsl`, `shaders/render.{vert,frag,comp}` and the Cargo features. On the development machine (Mesa RADV on an AMD iGPU, NVIDIA proprietary GL via EGL) both backends matched the CPU with a largest difference of 1.2e-7 across all six layouts, three modes, two parameter sets and supersampling 1 and 4.

**Decide first whether to build on it.** Recommended: `git merge --ff-only phase4-gpu-backends` (or cherry-pick the two commits), re-run `cargo test`, read the code, then continue from "Pipeline integration" below. If you would rather write it fresh, the design sections that follow describe the same architecture in enough detail to do so. Either way the branch has not been reviewed by anyone and has not been run under the Vulkan validation layers (they were not installed) — do that if you can (`VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`).

## Design

### Principle: the GPU computes grids, the inspector stays software

Neither backend draws to the screen. Each takes a job and returns a `SubpixelGrid`; the inspector presents it exactly as it presents CPU output. This keeps minifb, keeps the simulated view working unchanged, and makes switching backend at run time trivial. Do not replace the windowing layer in this phase.

### One GLSL implementation, two wrappers

Write the rendering code once and share it:

```
shaders/params.glsl    member list of the Params uniform block
shaders/common.glsl    kage_kernel(), kage_box(), kage_edge(), kage_emitters(), kage_render()
shaders/render.vert    full-target triangle from gl_VertexID (no vertex buffers)
shaders/render.frag    GLSL 3.30: samplers + accessors + `//#include` lines + main
shaders/render.comp    GLSL 4.50: storage buffers + accessors + `//#include` lines + main
```

GLSL has no `#include`. Resolve `//#include params.glsl` / `//#include common.glsl` by text substitution in Rust (`include_str!` each file, splice at start-up).

`common.glsl` needs only two things from its includer: the block instance `P`, and

```glsl
float kage_lo(ivec2 p);   // pixel-resolution coverage, clamped to the edge
float kage_hi(ivec2 p);   // supersampled coverage, clamped to the edge
```

OpenGL implements them with `texelFetch` on `R32F` textures; Vulkan with indexed loads from `float data[]` storage buffers.

Three modes, selected by `P.dims.w`:

| Mode | Renderer | Code |
|------|----------|------|
| 0 | Greyscale AA | `vec3(kage_lo(p))` |
| 1 | Subpixel AA | separable 3×3 taps per channel; with the Pentile flag, the colour absent at `(x + y)` parity uses taps `(0.5, 0, 0.5)` |
| 2 | OLED-aware | per colour: `kage_box` at `p + site`, width = luma or chroma footprint; red/blue blended towards the luma-footprint sample by `edge_suppression · smoothstep(lo, hi, sobel)` |

Port the CPU arithmetic in the same order of operations. In particular `kage_box` must clamp the footprint to at least one fine sample, iterate `floor(x0) .. ceil(x1)`, and weight each sample by its overlap — as `Coverage::sample_box` does.

### The parameter block

One `#[repr(C)]` struct, every field a 16-byte vector, so Rust layout and std140 agree with no padding to think about:

```rust
pub struct ShaderParams {
    pub dims: [i32; 4],            // width, height, samples per pixel, mode
    pub cell: [i32; 4],            // cell w, cell h, pentile flag, achromatic flag
    pub taps_h: [[f32; 4]; 3],     // fixed kernel, horizontal, per channel
    pub taps_v: [[f32; 4]; 3],     // fixed kernel, vertical, per channel
    pub footprint: [f32; 4],       // luma, chroma, edge suppression, height
    pub edge: [f32; 4],            // axis weight x, y; smoothstep lo, hi
    pub sites: [[f32; 4]; 12],     // [cell pixel * 3 + colour] = x, y, present, 0
}
```

Build it in one place (`RenderJob::new`) from the same arguments `render_coverage` takes, calling `samples_at_emitters` and `base_filter` so the mode decision cannot drift from the CPU's. Declare the block with an **instance name** in both shaders (`uniform Params { … } P;`) — it avoids relying on anonymous-block support in the SPIR-V compiler. Add tests that `size_of::<ShaderParams>() == 22 * 16` and that the field order in `params.glsl` matches.

### Module layout

```
src/gpu/mod.rs       Backend {Cpu, OpenGl, Vulkan}, ShaderParams, RenderJob, trait GpuRenderer, Gpu (lazy backends)
src/gpu/shaders.rs   source assembly
src/gpu/opengl.rs    GlRenderer            #[cfg(feature = "opengl")]
src/gpu/vulkan.rs    VulkanRenderer        #[cfg(feature = "vulkan")]
```

```rust
pub trait GpuRenderer {
    fn render(&mut self, job: &RenderJob) -> Result<SubpixelGrid, String>;
    fn device(&self) -> &str;
}
```

`Gpu` creates each backend on first use and remembers a failure, so a missing driver is probed once and reported as an `Err`, never a panic. Cargo features `opengl` and `vulkan`, both default; the crate must build and pass tests with `--no-default-features`.

### OpenGL backend

Dependencies: `glow = "0.14"`, `khronos-egl = { version = "6", features = ["dynamic"] }` (loads `libEGL` at run time — no link-time dependency).

1. `egl::DynamicInstance::<egl::EGL1_5>::load_required()`.
2. Display: try `get_platform_display(EGL_PLATFORM_SURFACELESS_MESA = 0x31DD, DEFAULT_DISPLAY, &[ATTRIB_NONE])` with no surface; if that fails, `get_display(DEFAULT_DISPLAY)` with a 1×1 pbuffer. Do not use the window's display: the inspector removes `WAYLAND_DISPLAY` to get an X11 window, and Mesa's Wayland platform has no pbuffers.
3. `bind_api(OPENGL_API)`, config with `RENDERABLE_TYPE = OPENGL_BIT`, context 3.3 core, `make_current`.
4. `glow::Context::from_loader_function` over `egl.get_proc_address`.
5. Program from `render.vert` + `render.frag`; bind block `Params` to binding 0; samplers `t_lo`, `t_hi` to units 0, 1.
6. Per job: upload `lo` and `hi` as `R32F` (`NEAREST`, `CLAMP_TO_EDGE`), allocate an `RGBA32F` target of `width × height` on an FBO, check completeness, upload the UBO, `viewport`, draw 3 vertices, `read_pixels(RGBA, FLOAT)`, `SubpixelGrid::from_rgba`.

Upload rows top-down and read them back unflipped: with `ivec2(gl_FragCoord.xy)` as the pixel index, texture row 0 = glyph row 0 on both sides and no flip is needed anywhere. Call `make_current` at the start of every job and in `Drop`.

### Vulkan backend

Dependencies: `ash = "0.38"`, `naga = { version = "22", features = ["glsl-in", "spv-out"] }`. There is no `glslc`/`glslangValidator` on the development machine and none should be required: compile `render.comp` to SPIR-V with naga when the backend starts (`front::glsl::Frontend::parse` → `valid::Validator::validate` → `back::spv::write_vec` with `PipelineOptions { Compute, "main" }`). The naga 22 API differs from later versions; pin it or adapt.

Use **storage buffers, not images** — no layouts, views, samplers or transitions:

| Binding | Type | Contents |
|---------|------|----------|
| 0 | storage, readonly | `float lo[]` |
| 1 | storage, readonly | `float hi[]` |
| 2 | storage | `vec4 result[]` |
| 3 | uniform (std140) | `Params` |

All four in `HOST_VISIBLE | HOST_COHERENT` memory, mapped for their lifetime, recreated only when a job needs more room. Per job: memcpy inputs, record `bind pipeline → bind set → dispatch(ceil(w/8), ceil(h/8), 1) → memory barrier (SHADER_WRITE → HOST_READ, COMPUTE_SHADER → HOST)`, submit with a fence, wait, memcpy the result out. The shader's `main` must return early for invocations past the glyph's edge.

Device choice: among devices with a compute queue prefer integrated, then discrete, then virtual, then CPU (llvmpipe). For work this small, waking a discrete GPU costs more than it saves. Instance API version 1.1, no extensions, no layers.

Destroy in reverse order in `Drop`; construct a struct with null handles early so that an error half-way through `new()` cleans up through `Drop` (destroying a null handle is a no-op).

### The parity test

One helper used by both backends' tests: for supersampling 1 and 4, every `SubpixelLayout::ALL`, every `RenderMode::ALL`, default params and one non-default set (e.g. 0.55 / 1.7 / 0.8), render a synthetic coverage (a ring plus a slanted bar, antialiased at the fine resolution) on CPU and GPU and assert every channel differs by < 1e-4. If the backend cannot be created, print why and return — CI machines have no GPU.

## Pipeline integration (not started)

In `src/pipeline.rs`:

- Add `backend: Backend` to `Tuning`; give `Pipeline` a `Gpu`.
- `run()` renders **all three** modes on the selected backend and times that, on the CPU too, so the times are comparable. Warm the backend up (`gpu.renderer(backend)`) before starting the timer: the first call compiles shaders.
- If the backend fails, fall back to the CPU for that run and report it: add to `Output` the backend actually used, the device name and an optional error string. Never abort.
- Keep the simulation cache for the greyscale and Subpixel AA images keyed on the CPU grids; GPU output differs by ~1e-7, which is immaterial to the metrics. Say so in a comment.
- `auto_tune` stays on the CPU (it calls `objective` dozens of times; the simulation dominates).
- Tests: with each backend available, `run()` returns the same metrics as the CPU run to 1e-4; with a backend that fails to initialise, `run()` still returns output and sets the error.

CLI (`src/main.rs`): `--backend cpu|opengl|vulkan` (default `cpu`). Print the backend, device and render time; print a one-line warning on fallback.

## Tuning UI (not started)

Extend `viz::Inspector`; it currently holds fixed `ViewSet`s handed to `Inspector::new`.

- `Inspector::attach_pipeline(pipeline: Pipeline, tuning: Tuning)`. When attached, a settings change calls `pipeline.run(&tuning)` and replaces `raw`, `simulated`, `config.metrics` and `config.simulation_note`, then `rebuild_view()`. Move the sigma → sentence helper out of `main.rs` so both can use it.
- A **tuning bar** between the panels and the footer: one chip per setting showing name and value, the selected one highlighted; at the right, the backend, render time, and the fallback error if any. Reserve its height in `layout_cards` (add a parameter; update the three layout tests).
- Settings, in a static table of `{ name, unit, min, max, step, get, set }`:

  | Setting | Range | Step |
  |---------|-------|------|
  | Luma footprint | 0.25–3.0 px | 0.05 |
  | Chroma footprint | 0.25–3.0 px | 0.05 |
  | Edge suppression | 0–1 | 0.05 |
  | Viewing distance | 200–1500 mm | 25 |

- Keys (check none collides with the existing ones in the README's table):

  | Key | Action |
  |-----|--------|
  | `Tab` / `Shift+Tab` | Select next / previous setting |
  | `[` / `]` | Decrease / increase (key repeat on) |
  | `T` | Auto-tune |
  | `B` | Next backend |
  | `Backspace` | Reset all settings to their starting values |

- Add a revision counter and the selected index to `FrameState`, or the window will not redraw after a change.
- "Real-time" target: a 64 px glyph should update without perceptible lag in a release build. The renderers take well under a millisecond; the cost is `simulate_panel` (the convolution in `simulate/psf.rs` calls a function per tap per channel). Measure first. If it lags, the fix is in that convolution's inner loop (operate on per-channel `Vec<f32>` rows), not in the GPU code.
- The bar must appear in `--snapshot` output, so the README screenshot can show it.

## Documentation

- README: status table, Phase 4 checklist, a "GPU backends" section (features, how each works, `--backend`), the controls table, the architecture diagram, the module reference, repository structure, technical stack. Regenerate both screenshots with `--snapshot` (PPM → PNG).
- `devlog.md`: a session entry — what was built, measured CPU/OpenGL/Vulkan render times for a stated glyph and machine, what was verified and how, what was not.
- Crate docs in `src/lib.rs`.

State plainly what was not verified. In particular, keyboard handling in the inspector cannot be exercised headlessly; say so rather than implying it was tested.

## Verification checklist

- [ ] `cargo test` passes; `cargo test --no-default-features` passes
- [ ] Both parity tests pass on a machine with drivers, and skip cleanly without
- [ ] `--backend opengl` and `--backend vulkan` print the same metrics table as `--backend cpu`
- [ ] Forcing a backend failure (e.g. `VK_ICD_FILENAMES=/nonexistent`) falls back to the CPU with a message
- [ ] Snapshots from all three backends are pixel-identical or differ by at most 1/255
- [ ] Vulkan validation layers report nothing, if they are available
- [ ] Every commit builds and passes its own tests

## Conventions

- Commit in logical steps (backend, backend, pipeline, UI, docs), conventional-commit subjects as in `git log`.
- **No `Co-Authored-By` or other attribution lines in commit messages.**
- British spelling in prose and identifiers already established (`grey`, `colour` in docs; code uses existing names).
- Every `unsafe` block gets a `SAFETY:` comment.
- Out of scope: replacing minifb, GPU simulation, GPU auto-tune, text layout beyond one glyph.
