//! OpenGL backend: the renderers as a GLSL fragment shader.
//!
//! The glyph is drawn off-screen. An EGL context is created without a
//! window, the two coverage planes are uploaded as `R32F` textures, one
//! triangle covering the target runs `shaders/render.frag` once per logical
//! pixel into an `RGBA32F` texture, and the result is read back.
//!
//! Nothing here depends on the window system: the context is created on
//! Mesa's surfaceless platform when that is available, and on the default
//! display with a 1×1 pbuffer otherwise.

use glow::HasContext;
use khronos_egl as egl;

use super::{shaders, GpuRenderer, RenderJob};
use crate::subpixel::SubpixelGrid;

type Egl = egl::DynamicInstance<egl::EGL1_5>;

/// `EGL_PLATFORM_SURFACELESS_MESA`
const PLATFORM_SURFACELESS: egl::Enum = 0x31DD;

/// An off-screen OpenGL 3.3 context with the KAGE shader program.
pub struct GlRenderer {
    egl: Egl,
    display: egl::Display,
    context: egl::Context,
    surface: Option<egl::Surface>,
    gl: glow::Context,
    program: glow::Program,
    vao: glow::VertexArray,
    ubo: glow::Buffer,
    fbo: glow::Framebuffer,
    /// Coverage at pixel resolution, supersampled coverage, render target.
    textures: [glow::Texture; 3],
    device: String,
}

impl GlRenderer {
    /// Create the context and compile the shaders.
    pub fn new() -> Result<Self, String> {
        // SAFETY: loads the system's libEGL; we rely on it implementing EGL.
        let egl = unsafe { Egl::load_required() }.map_err(|e| format!("cannot load libEGL: {e}"))?;

        let mut last_error = String::from("no EGL display available");
        // Surfaceless first: it needs no window system at all.
        for surfaceless in [true, false] {
            match Self::with_display(&egl, surfaceless) {
                Ok((display, context, surface)) => return Self::finish(egl, display, context, surface),
                Err(e) => last_error = e,
            }
        }
        Err(format!("cannot create an OpenGL context: {last_error}"))
    }

    /// Open a display and create a current OpenGL 3.3 core context on it.
    fn with_display(egl: &Egl, surfaceless: bool) -> Result<(egl::Display, egl::Context, Option<egl::Surface>), String> {
        let err = |what: &str, e: egl::Error| format!("{what}: {e}");
        // SAFETY: EGL_DEFAULT_DISPLAY is valid for both calls.
        let display = unsafe {
            if surfaceless {
                egl.get_platform_display(PLATFORM_SURFACELESS, egl::DEFAULT_DISPLAY, &[egl::ATTRIB_NONE])
                    .map_err(|e| err("surfaceless display", e))?
            } else {
                egl.get_display(egl::DEFAULT_DISPLAY).ok_or("no default EGL display")?
            }
        };
        egl.initialize(display).map_err(|e| err("eglInitialize", e))?;

        let build = || -> Result<(egl::Context, Option<egl::Surface>), String> {
            egl.bind_api(egl::OPENGL_API).map_err(|e| err("eglBindAPI(OpenGL)", e))?;
            let surface_type = if surfaceless { egl::DONT_CARE } else { egl::PBUFFER_BIT };
            let config = egl
                .choose_first_config(
                    display,
                    &[egl::SURFACE_TYPE, surface_type, egl::RENDERABLE_TYPE, egl::OPENGL_BIT, egl::NONE],
                )
                .map_err(|e| err("eglChooseConfig", e))?
                .ok_or("no EGL config supports OpenGL")?;
            let context = egl
                .create_context(
                    display,
                    config,
                    None,
                    &[
                        egl::CONTEXT_MAJOR_VERSION, 3,
                        egl::CONTEXT_MINOR_VERSION, 3,
                        egl::CONTEXT_OPENGL_PROFILE_MASK, egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
                        egl::NONE,
                    ],
                )
                .map_err(|e| err("eglCreateContext(OpenGL 3.3 core)", e))?;
            let surface = if surfaceless {
                None
            } else {
                Some(
                    egl.create_pbuffer_surface(display, config, &[egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE])
                        .map_err(|e| {
                            let _ = egl.destroy_context(display, context);
                            err("eglCreatePbufferSurface", e)
                        })?,
                )
            };
            if let Err(e) = egl.make_current(display, surface, surface, Some(context)) {
                if let Some(surface) = surface { let _ = egl.destroy_surface(display, surface); }
                let _ = egl.destroy_context(display, context);
                return Err(err("eglMakeCurrent", e));
            }
            Ok((context, surface))
        };
        match build() {
            Ok((context, surface)) => Ok((display, context, surface)),
            Err(e) => {
                let _ = egl.terminate(display);
                Err(e)
            }
        }
    }

    /// Build the GL objects on the now-current context.
    fn finish(egl: Egl, display: egl::Display, context: egl::Context, surface: Option<egl::Surface>) -> Result<Self, String> {
        // SAFETY: the context created above is current on this thread, and
        // the loader returns that context's function pointers.
        unsafe {
            let gl = glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name).map_or(std::ptr::null(), |f| f as *const _)
            });
            let device = format!(
                "{} (OpenGL {})",
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::VERSION).split_whitespace().next().unwrap_or("?")
            );

            let build = || -> Result<_, String> {
            let program = link_program(&gl, &shaders::vertex(), &shaders::fragment())?;
            gl.use_program(Some(program));
            let block = gl.get_uniform_block_index(program, "Params").ok_or("shader has no Params block")?;
            gl.uniform_block_binding(program, block, 0);
            for (unit, name) in ["t_lo", "t_hi"].iter().enumerate() {
                let location = gl.get_uniform_location(program, name);
                gl.uniform_1_i32(location.as_ref(), unit as i32);
            }

            let vao = gl.create_vertex_array()?;
            let ubo = gl.create_buffer()?;
            let fbo = gl.create_framebuffer()?;
            let textures = [gl.create_texture()?, gl.create_texture()?, gl.create_texture()?];
            for texture in textures {
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                for (parameter, value) in [
                    (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                    (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
                }
            }
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);

            Ok((program, vao, ubo, fbo, textures))
            };
            match build() {
                Ok((program, vao, ubo, fbo, textures)) => Ok(Self { egl, display, context, surface, gl, program, vao, ubo, fbo, textures, device }),
                Err(e) => {
                    // Destroying the unshared context releases partially built GL objects.
                    let _ = egl.make_current(display, None, None, None);
                    if let Some(surface) = surface { let _ = egl.destroy_surface(display, surface); }
                    let _ = egl.destroy_context(display, context);
                    let _ = egl.terminate(display);
                    Err(e)
                }
            }
        }
    }

    fn make_current(&self) -> Result<(), String> {
        self.egl
            .make_current(self.display, self.surface, self.surface, Some(self.context))
            .map_err(|e| format!("eglMakeCurrent: {e}"))
    }
}

/// View a float slice as bytes for upload.
fn bytes_of(data: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any alignment suits u8.
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), std::mem::size_of_val(data)) }
}

unsafe fn link_program(gl: &glow::Context, vertex: &str, fragment: &str) -> Result<glow::Program, String> {
    let program = gl.create_program()?;
    let mut compiled = Vec::new();
    for (kind, name, source) in [(glow::VERTEX_SHADER, "vertex", vertex), (glow::FRAGMENT_SHADER, "fragment", fragment)] {
        let shader = gl.create_shader(kind)?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            return Err(format!("{name} shader failed to compile:\n{}", gl.get_shader_info_log(shader)));
        }
        gl.attach_shader(program, shader);
        compiled.push(shader);
    }
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        return Err(format!("shader program failed to link:\n{}", gl.get_program_info_log(program)));
    }
    for shader in compiled {
        gl.detach_shader(program, shader);
        gl.delete_shader(shader);
    }
    Ok(program)
}

impl GpuRenderer for GlRenderer {
    fn render(&mut self, job: &RenderJob) -> Result<SubpixelGrid, String> {
        let (w, h) = job.size();
        if w == 0 || h == 0 {
            return Ok(SubpixelGrid::new(w, h));
        }
        self.make_current()?;
        let gl = &self.gl;
        let cov = job.coverage;
        let [t_lo, t_hi, t_out] = self.textures;

        // SAFETY: all objects belong to the current context; slices passed
        // to GL match the dimensions given alongside them.
        unsafe {
            let upload = |unit: u32, texture: glow::Texture, width: u32, height: u32, data: &[f32]| {
                gl.active_texture(glow::TEXTURE0 + unit);
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_image_2d(
                    glow::TEXTURE_2D, 0, glow::R32F as i32, width as i32, height as i32, 0,
                    glow::RED, glow::FLOAT, Some(bytes_of(data)),
                );
            };
            upload(0, t_lo, cov.lo.width, cov.lo.height, &cov.lo.data);
            upload(1, t_hi, cov.hi.width, cov.hi.height, &cov.hi.data);

            // Render target. Bound to unit 2 so it never aliases an input.
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, Some(t_out));
            gl.tex_image_2d(
                glow::TEXTURE_2D, 0, glow::RGBA32F as i32, w as i32, h as i32, 0,
                glow::RGBA, glow::FLOAT, None,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t_out), 0);
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("float render target is not supported (framebuffer status {status:#x})"));
            }

            gl.bind_buffer(glow::UNIFORM_BUFFER, Some(self.ubo));
            gl.buffer_data_u8_slice(glow::UNIFORM_BUFFER, job.params.as_bytes(), glow::DYNAMIC_DRAW);
            gl.bind_buffer_base(glow::UNIFORM_BUFFER, 0, Some(self.ubo));

            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            let mut rgba = vec![0.0f32; (w * h * 4) as usize];
            let bytes = std::slice::from_raw_parts_mut(rgba.as_mut_ptr().cast::<u8>(), std::mem::size_of_val(rgba.as_slice()));
            gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::FLOAT, glow::PixelPackData::Slice(bytes));
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);

            let error = gl.get_error();
            if error != glow::NO_ERROR {
                return Err(format!("OpenGL error {error:#x} while rendering"));
            }
            Ok(SubpixelGrid::from_rgba(w, h, &rgba))
        }
    }

    fn device(&self) -> &str {
        &self.device
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        if self.make_current().is_ok() {
            // SAFETY: the objects were created on this context, now current.
            unsafe {
                for texture in self.textures {
                    self.gl.delete_texture(texture);
                }
                self.gl.delete_framebuffer(self.fbo);
                self.gl.delete_buffer(self.ubo);
                self.gl.delete_vertex_array(self.vao);
                self.gl.delete_program(self.program);
            }
        }
        let _ = self.egl.make_current(self.display, None, None, None);
        if let Some(surface) = self.surface {
            let _ = self.egl.destroy_surface(self.display, surface);
        }
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a working OpenGL 3.3 driver; skipped (with a note) without one.
    #[test]
    fn matches_the_cpu_renderers() {
        match GlRenderer::new() {
            Ok(mut renderer) => crate::gpu::tests::assert_matches_cpu(&mut renderer),
            Err(e) => eprintln!("skipping: no OpenGL backend on this machine ({e})"),
        }
    }
}
