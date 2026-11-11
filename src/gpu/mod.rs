//! GPU implementations of the three renderers.
//!
//! Two backends compute exactly what the CPU renderers compute:
//!
//! - [`opengl`] — a GLSL **fragment shader** drawing into a float texture
//!   through an off-screen EGL context;
//! - [`vulkan`] — a GLSL **compute shader** over storage buffers.
//!
//! Both are built from the same GLSL source ([`shaders`]): the rendering
//! code is shared, and each backend only supplies how coverage is read
//! (texel fetches or buffer loads) and where the result goes. They take the
//! same [`ShaderParams`] block, and the tests compare their output with the
//! CPU renderers' sample for sample.
//!
//! Neither backend draws to the screen. They return a
//! [`SubpixelGrid`], and the inspector presents it the same way it presents
//! CPU output — which is what lets it switch backends while running.
//!
//! Each backend is behind a Cargo feature (`opengl`, `vulkan`, both on by
//! default) and is initialised on first use. A machine without a working
//! driver gets an error from [`Gpu::render`], not a failure to start.

use std::fmt;

use crate::coverage::Coverage;
use crate::layout::{BaseFilter, EmitterMap, FilterKernel, SubpixelLayout};
use crate::profile::DisplayProfile;
use crate::render::perceptual::{EDGE_HI, EDGE_LO, FOOTPRINT_HEIGHT};
use crate::render::{base_filter, samples_at_emitters, RenderMode, RenderParams};
use crate::subpixel::SubpixelGrid;

#[cfg(feature = "opengl")]
pub mod opengl;
pub mod shaders;
#[cfg(feature = "vulkan")]
pub mod vulkan;

// ── Backend ───────────────────────────────────────────────────────────────────

/// Where the renderers run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// The software renderers in [`crate::render`].
    Cpu,
    /// GLSL fragment shader, off-screen OpenGL 3.3.
    OpenGl,
    /// GLSL compute shader, Vulkan.
    Vulkan,
}

impl Backend {
    pub const ALL: [Backend; 3] = [Backend::Cpu, Backend::OpenGl, Backend::Vulkan];

    /// Name for user interfaces.
    pub fn label(self) -> &'static str {
        match self {
            Backend::Cpu => "CPU",
            Backend::OpenGl => "OpenGL",
            Backend::Vulkan => "Vulkan",
        }
    }

    /// Parse a command-line name: `cpu`, `opengl` / `gl`, `vulkan` / `vk`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_lowercase().as_str() {
            "cpu" | "software" => Some(Backend::Cpu),
            "opengl" | "gl" => Some(Backend::OpenGl),
            "vulkan" | "vk" => Some(Backend::Vulkan),
            _ => None,
        }
    }

    /// The backend after this one, wrapping around.
    pub fn next(self) -> Self {
        match self {
            Backend::Cpu => Backend::OpenGl,
            Backend::OpenGl => Backend::Vulkan,
            Backend::Vulkan => Backend::Cpu,
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ── Shader parameters ─────────────────────────────────────────────────────────

/// Shader `mode` values (`ShaderParams::dims[3]`).
pub const MODE_GREY: i32 = 0;
pub const MODE_KERNEL: i32 = 1;
pub const MODE_EMITTERS: i32 = 2;

/// Number of emitter sites the shaders hold: a 2×2 unit cell, three colours.
pub const MAX_SITES: usize = 12;

/// The `Params` uniform block of `shaders/params.glsl`, byte for byte.
///
/// Every field is a 16-byte vector, so the Rust layout and std140 agree.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderParams {
    /// Width, height in logical pixels; fine samples per pixel; mode.
    pub dims: [i32; 4],
    /// Unit cell width, height; Pentile flag; achromatic flag.
    pub cell: [i32; 4],
    /// Fixed kernel: horizontal taps for red, green, blue.
    pub taps_h: [[f32; 4]; 3],
    /// Fixed kernel: vertical taps for red, green, blue.
    pub taps_v: [[f32; 4]; 3],
    /// Luma footprint, chroma footprint, edge suppression, footprint height.
    pub footprint: [f32; 4],
    /// Gradient weight x, y; smoothstep low, high.
    pub edge: [f32; 4],
    /// Emitter position in the pixel, per cell pixel and colour.
    pub sites: [[f32; 4]; MAX_SITES],
}

impl ShaderParams {
    /// The block as bytes, ready to upload.
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `ShaderParams` is `repr(C)` and made only of `i32` / `f32`
        // arrays with no padding (every field is a multiple of 16 bytes), so
        // every byte is initialised.
        unsafe { std::slice::from_raw_parts((self as *const Self).cast::<u8>(), std::mem::size_of::<Self>()) }
    }
}

// ── Render job ────────────────────────────────────────────────────────────────

/// One glyph to render in one mode: the inputs every backend needs.
#[derive(Debug, Clone)]
pub struct RenderJob<'a> {
    pub coverage: &'a Coverage,
    pub params: ShaderParams,
}

impl<'a> RenderJob<'a> {
    /// Describe rendering `coverage` in `mode`. The arguments mean what they
    /// mean to [`crate::render::render_coverage`], and the job reproduces
    /// its decisions (greyscale fallbacks included).
    pub fn new(
        coverage: &'a Coverage,
        mode: RenderMode,
        layout: SubpixelLayout,
        map: &EmitterMap,
        profile: &DisplayProfile,
        params: &RenderParams,
    ) -> Result<Self, String> {
        let at_emitters = samples_at_emitters(mode, profile);
        let filter = base_filter(mode, layout, profile);
        let shader_mode = if at_emitters {
            MODE_EMITTERS
        } else if filter == BaseFilter::Grey {
            MODE_GREY
        } else {
            MODE_KERNEL
        };
        let kernel = if at_emitters { FilterKernel::IDENTITY } else { filter.kernel() };

        if map.sites.len() * 3 > MAX_SITES {
            return Err(format!("unit cell {}×{} is too large for the shaders", map.cell_w, map.cell_h));
        }
        let mut sites = [[0.5, 0.5, 0.0, 0.0]; MAX_SITES];
        for (i, pixel) in map.sites.iter().enumerate() {
            for (k, site) in pixel.iter().enumerate() {
                sites[i * 3 + k] = [site.pos[0], site.pos[1], if site.present { 1.0 } else { 0.0 }, 0.0];
            }
        }

        let p = params.clamped();
        let axis = map.fringe_axis();
        let pad = |t: [f32; 3]| [t[0], t[1], t[2], 0.0];
        Ok(Self {
            coverage,
            params: ShaderParams {
                dims: [coverage.width() as i32, coverage.height() as i32, coverage.scale as i32, shader_mode],
                cell: [map.cell_w as i32, map.cell_h as i32, kernel.pentile as i32, map.achromatic as i32],
                taps_h: kernel.h.map(pad),
                taps_v: kernel.v.map(pad),
                footprint: [p.luma_footprint, p.chroma_footprint, p.edge_suppression, FOOTPRINT_HEIGHT],
                edge: [axis[0], axis[1], EDGE_LO, EDGE_HI],
                sites,
            },
        })
    }

    /// Output size in logical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.coverage.width(), self.coverage.height())
    }
}

/// A backend that can run a [`RenderJob`].
pub trait GpuRenderer {
    /// Render the job. The result is linear light, like the CPU renderers'.
    fn render(&mut self, job: &RenderJob) -> Result<SubpixelGrid, String>;

    /// The device doing the work, for display.
    fn device(&self) -> &str;
}

// ── Gpu ───────────────────────────────────────────────────────────────────────

/// The GPU backends, each created the first time it is asked for.
#[derive(Default)]
pub struct Gpu {
    #[cfg(feature = "opengl")]
    opengl: Option<Result<opengl::GlRenderer, String>>,
    #[cfg(feature = "vulkan")]
    vulkan: Option<Result<vulkan::VulkanRenderer, String>>,
}

impl Gpu {
    pub fn new() -> Self {
        Self::default()
    }

    /// The renderer for `backend`, initialising it if needed.
    ///
    /// Fails for [`Backend::Cpu`] (there is nothing to return), for a
    /// backend compiled out, and when the driver cannot be initialised. The
    /// failure is remembered, so a missing driver is probed once.
    pub fn renderer(&mut self, backend: Backend) -> Result<&mut dyn GpuRenderer, String> {
        match backend {
            Backend::Cpu => Err("the CPU backend is not a GPU renderer".into()),
            Backend::OpenGl => {
                #[cfg(feature = "opengl")]
                {
                    self.opengl
                        .get_or_insert_with(opengl::GlRenderer::new)
                        .as_mut()
                        .map(|r| r as &mut dyn GpuRenderer)
                        .map_err(|e| e.clone())
                }
                #[cfg(not(feature = "opengl"))]
                Err("this build has no OpenGL backend (enable the `opengl` feature)".into())
            }
            Backend::Vulkan => {
                #[cfg(feature = "vulkan")]
                {
                    self.vulkan
                        .get_or_insert_with(vulkan::VulkanRenderer::new)
                        .as_mut()
                        .map(|r| r as &mut dyn GpuRenderer)
                        .map_err(|e| e.clone())
                }
                #[cfg(not(feature = "vulkan"))]
                Err("this build has no Vulkan backend (enable the `vulkan` feature)".into())
            }
        }
    }

    /// Run `job` on `backend`.
    pub fn render(&mut self, backend: Backend, job: &RenderJob) -> Result<SubpixelGrid, String> {
        self.renderer(backend)?.render(job)
    }

    /// Name of the device behind `backend`, initialising it if needed.
    pub fn device(&mut self, backend: Backend) -> Result<String, String> {
        Ok(self.renderer(backend)?.device().to_owned())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::plane::Plane;
    use crate::render::render_coverage;

    #[test]
    fn shader_params_match_the_std140_block() {
        // dims, cell, 3 + 3 tap vectors, footprint, edge, 12 sites.
        assert_eq!(std::mem::size_of::<ShaderParams>(), (2 + 6 + 2 + MAX_SITES) * 16);
        assert_eq!(std::mem::align_of::<ShaderParams>(), 4);
    }

    #[test]
    fn backend_names_round_trip_and_cycle() {
        for b in Backend::ALL {
            assert_eq!(Backend::from_name(b.label()), Some(b));
        }
        assert_eq!(Backend::from_name("gl"), Some(Backend::OpenGl));
        assert_eq!(Backend::from_name("metal"), None);
        assert_eq!(Backend::Cpu.next().next().next(), Backend::Cpu);
    }

    #[test]
    fn job_reproduces_the_cpu_dispatch() {
        let cov = Coverage::from_hi(Plane::new(8, 8), 2);
        let profile = DisplayProfile::sdr_oled();
        let mut hidpi = profile.clone();
        hidpi.device_pixel_ratio = 2.0;
        let job = |mode, layout, profile: &DisplayProfile| {
            RenderJob::new(&cov, mode, layout, &EmitterMap::for_layout(layout), profile, &RenderParams::default()).unwrap().params
        };
        use SubpixelLayout::*;
        assert_eq!(job(RenderMode::Greyscale, PentileRgbg, &profile).dims, [4, 4, 2, MODE_GREY]);
        assert_eq!(job(RenderMode::SubpixelAa, PentileRgbg, &profile).dims[3], MODE_KERNEL);
        assert_eq!(job(RenderMode::SubpixelAa, PentileRgbg, &profile).cell, [2, 2, 1, 0]);
        assert_eq!(job(RenderMode::SubpixelAa, Wrgb, &profile).dims[3], MODE_GREY);
        assert_eq!(job(RenderMode::OledAware, Wrgb, &profile).dims[3], MODE_EMITTERS);
        assert_eq!(job(RenderMode::OledAware, Wrgb, &profile).cell[3], 1);
        assert_eq!(job(RenderMode::OledAware, RgbStripe, &hidpi).dims[3], MODE_GREY);
        let stripe = job(RenderMode::OledAware, RgbStripe, &profile);
        assert!((stripe.sites[0][0] - 1.0 / 6.0).abs() < 1e-6 && (stripe.sites[2][0] - 5.0 / 6.0).abs() < 1e-6);
        assert_eq!(stripe.edge, [1.0, 0.0, EDGE_LO, EDGE_HI]);
    }

    /// A glyph-like test pattern: a ring and a slanted bar, antialiased at
    /// the fine resolution.
    pub(crate) fn test_coverage(scale: u32) -> Coverage {
        let (w, h) = (13 * scale, 11 * scale);
        let hi = Plane::from_fn(w, h, |sx, sy| {
            let (x, y) = ((sx as f32 + 0.5) / scale as f32, (sy as f32 + 0.5) / scale as f32);
            let r = ((x - 5.2).powi(2) + (y - 5.4).powi(2)).sqrt();
            let ring = (1.0 - ((r - 3.1).abs() - 0.8) * scale as f32).clamp(0.0, 1.0);
            let bar = (1.0 - ((x - 0.4 * y - 8.3).abs() - 0.6) * scale as f32).clamp(0.0, 1.0);
            ring.max(bar)
        });
        Coverage::from_hi(hi, scale)
    }

    /// Render every mode on every layout with `renderer` and compare with
    /// the CPU. Shared by the backend tests.
    pub(crate) fn assert_matches_cpu(renderer: &mut dyn GpuRenderer) {
        let profile = DisplayProfile::sdr_oled();
        let settings = [
            RenderParams::default(),
            RenderParams { luma_footprint: 0.55, chroma_footprint: 1.7, edge_suppression: 0.8 },
        ];
        let mut worst = 0.0f32;
        for scale in [1, 4] {
            let cov = test_coverage(scale);
            for layout in SubpixelLayout::ALL {
                let map = EmitterMap::for_layout(layout);
                for mode in RenderMode::ALL {
                    for params in &settings {
                        let cpu = render_coverage(&cov, mode, layout, &map, &profile, params);
                        let job = RenderJob::new(&cov, mode, layout, &map, &profile, params).unwrap();
                        let gpu = renderer.render(&job).unwrap_or_else(|e| panic!("{layout:?} {mode:?}: {e}"));
                        assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));
                        for row in 0..cpu.height {
                            for col in 0..cpu.width {
                                let (a, b) = (cpu.pixel(col, row), gpu.pixel(col, row));
                                let d = (a.r - b.r).abs().max((a.g - b.g).abs()).max((a.b - b.b).abs());
                                assert!(
                                    d < 1e-4,
                                    "{layout:?} {mode:?} scale {scale} {params:?} at ({col},{row}): cpu {a:?} gpu {b:?}"
                                );
                                worst = worst.max(d);
                            }
                        }
                    }
                }
            }
        }
        println!("{}: largest difference from CPU {worst:.2e}", renderer.device());
    }
}
