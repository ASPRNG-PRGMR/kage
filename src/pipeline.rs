//! The whole chain from a glyph to what the inspector shows.
//!
//! ```text
//! Coverage ──▶ three renderers ──▶ panel simulation ──▶ metrics
//!   (font)      (render)             (simulate)
//! ```
//!
//! [`Pipeline`] owns everything that stays fixed for a session — the glyph,
//! the panel, the display profile — and [`Pipeline::run`] recomputes the
//! rest for one [`Tuning`]. The inspector calls it again whenever a setting
//! changes, so results that do not depend on the changed setting are cached.
//!
//! # Metrics
//!
//! Each rendering mode is scored on its simulated panel image:
//!
//! - **Luminance error** — RMS difference in luminance from what an ideal
//!   display would show ([`simulate_ideal`]): the glyph rasterized at the
//!   simulation's resolution, seen through the same optics. Because that
//!   reference has the glyph's true shape between pixel centres, a renderer
//!   is credited for placing light at the right sub-pixel position.
//! - **Visible chroma** — colour left in the image after the eye's chromatic
//!   low-pass ([`visible_chroma_rms`]): fringing.
//! - **Chroma residue** — all colour in the image, visible or not.
//!
//! [`Pipeline::auto_tune`] searches the OLED-aware renderer's settings for
//! the lowest luminance error plus visible chroma. That sum is the sense in
//! which the renderer is matched to the eye's contrast sensitivity: the
//! chroma term only counts what survives the chromatic CSF at the current
//! viewing distance.

use std::time::{Duration, Instant};

use crate::coverage::{Coverage, GlyphMetrics};
use crate::font::FontFace;
use crate::gpu::{Backend, Gpu, RenderJob};
use crate::layout::{EmitterMap, SubpixelLayout};
use crate::plane::Plane;
use crate::profile::DisplayProfile;
use crate::render::{csf, render_coverage, RenderMode, RenderParams};
use crate::simulate::{
    chroma_rms, luminance_rms_error, simulate_ideal, simulate_panel, viewing_distance_to_sigma,
    visible_chroma_rms, BleedProfile, PanelGeometry, SimulationParams,
};
use crate::subpixel::SubpixelGrid;
use crate::viz::{PanelMetrics, ViewSet};

/// Default supersampling of the glyph coverage given to the renderers.
pub const DEFAULT_SUPERSAMPLE: u32 = 4;

/// Everything about a session that tuning does not change.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Layout: picks the Subpixel AA baseline's fixed kernel.
    pub layout: SubpixelLayout,
    /// Emitter geometry: drives the simulation and the OLED-aware renderer.
    pub geometry: PanelGeometry,
    /// Per-colour optical bleed of the panel.
    pub bleed: BleedProfile,
    pub profile: DisplayProfile,
    /// Whether to simulate the panel at all.
    pub simulate: bool,
    /// Simulation samples per logical pixel per axis.
    pub sim_scale: u32,
    /// Angular width of the eye's point spread function.
    pub psf_fwhm_arcmin: f32,
    /// Fixed blur sigma in pixels; `None` derives it from the viewing
    /// distance.
    pub psf_sigma: Option<f32>,
    /// Frequency at which chromatic sensitivity has halved, cycles/degree.
    pub chroma_cutoff_cpd: f32,
}

/// The settings that can change while the inspector is open.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub backend: Backend,
    pub render: RenderParams,
    /// Eye-to-panel distance. Sets the optical blur (unless fixed) and the
    /// scale of the chromatic low-pass in the visible-chroma metric.
    pub viewing_distance_mm: f32,
}

/// One evaluation of the pipeline.
#[derive(Debug, Clone)]
pub struct Output {
    pub backend: Backend,
    pub device: String,
    pub backend_error: Option<String>,
    /// Renderer output, linear light, one sample per pixel.
    pub raw: ViewSet,
    /// Simulated panel image, linear light (`None` if simulation is off).
    pub simulated: Option<ViewSet>,
    /// Scores per mode, in panel order (`None` if simulation is off).
    pub metrics: Option<[PanelMetrics; 3]>,
    /// Optical blur used, in logical pixels.
    pub sigma: f32,
    /// Time spent in the three renderers.
    pub render_time: Duration,
}

/// Simulation results that depend only on the viewing distance.
struct ViewCache {
    sigma: f32,
    chroma_sigma: f32,
    reference: SubpixelGrid,
    grey: (SubpixelGrid, PanelMetrics),
    subpixel: (SubpixelGrid, PanelMetrics),
}

/// A glyph, a panel and the machinery to render and score it.
pub struct Pipeline {
    config: PipelineConfig,
    /// Coverage given to the renderers.
    coverage: Coverage,
    /// The glyph at the simulation's resolution: the metrics' reference.
    reference: Plane,
    map: EmitterMap,
    /// Greyscale and Subpixel AA output: independent of every setting.
    fixed: [SubpixelGrid; 2],
    cache: Option<ViewCache>,
    gpu: Gpu,
}

impl Pipeline {
    /// Build a pipeline from coverage already rasterized.
    ///
    /// `reference` is the same glyph with `config.sim_scale` samples per
    /// pixel. Fails if the panel's unit cell is too large for the renderer
    /// or the two coverages disagree in size.
    pub fn new(config: PipelineConfig, coverage: Coverage, reference: Plane) -> Result<Self, String> {
        let map = EmitterMap::from_geometry(&config.geometry)?;
        let scale = config.sim_scale.max(1);
        if (reference.width, reference.height) != (coverage.width() * scale, coverage.height() * scale) {
            return Err(format!(
                "reference coverage is {}×{}, expected {}×{}",
                reference.width,
                reference.height,
                coverage.width() * scale,
                coverage.height() * scale
            ));
        }
        let fixed = [RenderMode::Greyscale, RenderMode::SubpixelAa].map(|mode| {
            render_coverage(&coverage, mode, config.layout, &map, &config.profile, &RenderParams::default())
        });
        Ok(Self { config, coverage, reference, map, fixed, cache: None, gpu: Gpu::new() })
    }

    /// Rasterize `ch` from `face` and build a pipeline for it.
    ///
    /// `supersample` is the resolution of the coverage the renderers see,
    /// in samples per pixel per axis. Returns `Ok(None)` if the font has no
    /// such glyph.
    pub fn from_font(
        config: PipelineConfig,
        face: &FontFace,
        ch: char,
        supersample: u32,
    ) -> Result<Option<(Self, GlyphMetrics)>, String> {
        let Some((coverage, metrics)) = Coverage::rasterize(face, ch, supersample) else {
            return Ok(None);
        };
        let Some((fine, _)) = Coverage::rasterize(face, ch, config.sim_scale.max(1)) else {
            return Ok(None);
        };
        Ok(Some((Self::new(config, coverage, fine.hi)?, metrics)))
    }

    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    pub fn emitter_map(&self) -> &EmitterMap {
        &self.map
    }

    /// Optical blur, in logical pixels, at `viewing_distance_mm`.
    pub fn sigma(&self, viewing_distance_mm: f32) -> f32 {
        self.config.psf_sigma.unwrap_or_else(|| {
            let pitch_mm = 25.4 / self.config.profile.dpi;
            viewing_distance_to_sigma(pitch_mm, viewing_distance_mm, self.config.psf_fwhm_arcmin)
        })
    }

    fn sim_params(&self, viewing_distance_mm: f32) -> SimulationParams {
        SimulationParams { sigma: self.sigma(viewing_distance_mm).max(0.0), bleed: self.config.bleed.clone() }
    }

    /// Render the OLED-aware grid for `params` on the CPU.
    pub fn render_oled(&self, params: &RenderParams) -> SubpixelGrid {
        render_coverage(&self.coverage, RenderMode::OledAware, self.config.layout, &self.map, &self.config.profile, params)
    }

    /// Simulate `grid` on the panel and score the result.
    fn simulate(&self, grid: &SubpixelGrid, sim: &SimulationParams, reference: &SubpixelGrid, chroma_sigma: f32) -> (SubpixelGrid, PanelMetrics) {
        let scale = self.config.sim_scale.max(1);
        let image = simulate_panel(grid, &self.config.geometry, sim, scale);
        let metrics = PanelMetrics {
            luminance_error: luminance_rms_error(reference, &image).unwrap_or(f32::NAN),
            chroma: chroma_rms(&image),
            visible_chroma: visible_chroma_rms(&image, scale, chroma_sigma),
        };
        (image, metrics)
    }

    /// Make sure the cache holds results for this viewing distance.
    fn prepare(&mut self, viewing_distance_mm: f32) {
        let sim = self.sim_params(viewing_distance_mm);
        let chroma_sigma = csf::gaussian_sigma_px(self.config.chroma_cutoff_cpd, &self.config.profile, viewing_distance_mm);
        if self.cache.as_ref().is_some_and(|c| c.sigma == sim.sigma && c.chroma_sigma == chroma_sigma) {
            return;
        }
        let reference = simulate_ideal(&self.reference, &sim, self.config.sim_scale.max(1));
        // Baseline simulations use CPU grids: GPU rounding (~1e-7) is
        // immaterial to metrics and must not invalidate this cache.
        let grey = self.simulate(&self.fixed[0], &sim, &reference, chroma_sigma);
        let subpixel = self.simulate(&self.fixed[1], &sim, &reference, chroma_sigma);
        self.cache = Some(ViewCache { sigma: sim.sigma, chroma_sigma, reference, grey, subpixel });
    }

    /// Render all three modes, excluding backend initialisation from timing.
    /// A failed GPU run is discarded in full and recomputed on the CPU.
    pub fn run(&mut self, tuning: &Tuning) -> Output {
        let mut backend_error = None;
        if tuning.backend != Backend::Cpu {
            match self.gpu.renderer(tuning.backend) {
                Ok(renderer) => {
                    let device = renderer.device().to_owned();
                    let start = Instant::now();
                    let grids: Result<Vec<_>, String> = RenderMode::ALL.iter().map(|&mode| {
                        let job = RenderJob::new(&self.coverage, mode, self.config.layout,
                            &self.map, &self.config.profile, &tuning.render)?;
                        renderer.render(&job)
                    }).collect();
                    let elapsed = start.elapsed();
                    match grids {
                        Ok(mut grids) => {
                            let oled = grids.pop().unwrap();
                            let subpixel = grids.pop().unwrap();
                            let grey = grids.pop().unwrap();
                            let mut out = self.run_with(tuning, oled, elapsed);
                            out.raw.grey = grey;
                            out.raw.subpixel = subpixel;
                            out.backend = tuning.backend;
                            out.device = device;
                            return out;
                        }
                        Err(e) => backend_error = Some(e),
                    }
                }
                Err(e) => backend_error = Some(e),
            }
        }
        let start = Instant::now();
        let [grey, subpixel, oled] = RenderMode::ALL.map(|mode| render_coverage(
            &self.coverage, mode, self.config.layout, &self.map, &self.config.profile, &tuning.render));
        let elapsed = start.elapsed();
        let mut out = self.run_with(tuning, oled, elapsed);
        out.raw.grey = grey;
        out.raw.subpixel = subpixel;
        out.backend_error = backend_error;
        out
    }

    /// As [`run`](Self::run), with the OLED-aware grid supplied by the
    /// caller (a GPU backend, for instance).
    pub fn run_with(&mut self, tuning: &Tuning, oled: SubpixelGrid, render_time: Duration) -> Output {
        let sigma = self.sigma(tuning.viewing_distance_mm).max(0.0);
        let raw = ViewSet { grey: self.fixed[0].clone(), subpixel: self.fixed[1].clone(), oled, scale: 1 };
        if !self.config.simulate {
            return Output { backend: Backend::Cpu, device: "CPU software renderer".into(), backend_error: None, raw, simulated: None, metrics: None, sigma, render_time };
        }
        self.prepare(tuning.viewing_distance_mm);
        let cache = self.cache.as_ref().expect("prepare() fills the cache");
        let sim = self.sim_params(tuning.viewing_distance_mm);
        let (oled_image, oled_metrics) = self.simulate(&raw.oled, &sim, &cache.reference, cache.chroma_sigma);
        let simulated = ViewSet {
            grey: cache.grey.0.clone(),
            subpixel: cache.subpixel.0.clone(),
            oled: oled_image,
            scale: self.config.sim_scale.max(1),
        };
        let metrics = [cache.grey.1, cache.subpixel.1, oled_metrics];
        Output { backend: Backend::Cpu, device: "CPU software renderer".into(), backend_error: None, raw, simulated: Some(simulated), metrics: Some(metrics), sigma, render_time }
    }

    /// Luminance error plus visible chroma of the OLED-aware renderer with
    /// `params`, at `viewing_distance_mm`. Lower is better. `None` if
    /// simulation is off.
    pub fn objective(&mut self, params: &RenderParams, viewing_distance_mm: f32) -> Option<f32> {
        if !self.config.simulate {
            return None;
        }
        self.prepare(viewing_distance_mm);
        let cache = self.cache.as_ref()?;
        let sim = self.sim_params(viewing_distance_mm);
        let (_, m) = self.simulate(&self.render_oled(params), &sim, &cache.reference, cache.chroma_sigma);
        Some(m.luminance_error + m.visible_chroma)
    }

    /// Search for the OLED-aware settings with the lowest
    /// [`objective`](Self::objective), starting from `tuning.render`.
    ///
    /// The search is coordinate descent over a fixed grid of footprint
    /// widths and edge-suppression strengths; a setting only changes if
    /// that strictly lowers the objective. Returns `None` if simulation is
    /// off.
    pub fn auto_tune(&mut self, tuning: &Tuning) -> Option<RenderParams> {
        const LUMA: [f32; 9] = [0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2];
        const CHROMA: [f32; 9] = [0.5, 0.7, 0.85, 1.0, 1.15, 1.3, 1.6, 2.0, 2.5];
        const EDGE: [f32; 3] = [0.0, 0.5, 1.0];

        let distance = tuning.viewing_distance_mm;
        let mut best = tuning.render.clamped();
        let mut best_score = self.objective(&best, distance)?;
        for _round in 0..3 {
            let before = best;
            for i in 0..LUMA.len() + CHROMA.len() + EDGE.len() {
                // One axis at a time, each building on the best so far.
                let candidate = if i < LUMA.len() {
                    RenderParams { luma_footprint: LUMA[i], ..best }
                } else if i < LUMA.len() + CHROMA.len() {
                    RenderParams { chroma_footprint: CHROMA[i - LUMA.len()], ..best }
                } else {
                    RenderParams { edge_suppression: EDGE[i - LUMA.len() - CHROMA.len()], ..best }
                };
                if candidate == best {
                    continue;
                }
                let score = self.objective(&candidate, distance)?;
                if score < best_score - 1e-6 {
                    best_score = score;
                    best = candidate;
                }
            }
            if best == before {
                break;
            }
        }
        Some(best)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulate::DEFAULT_PSF_FWHM_ARCMIN;

    const SIM: u32 = 4;

    /// A slanted stroke, defined analytically so it can be sampled at any
    /// resolution: coverage 1 where `0.35·y + 3.3 < x < 0.35·y + 5.6`.
    fn stroke(scale: u32) -> Plane {
        let (w, h) = (10 * scale, 9 * scale);
        Plane::from_fn(w, h, |sx, sy| {
            let (x, y) = ((sx as f32 + 0.5) / scale as f32, (sy as f32 + 0.5) / scale as f32);
            let left = 0.35 * y + 3.3;
            if x > left && x < left + 2.3 { 1.0 } else { 0.0 }
        })
    }

    fn config(layout: SubpixelLayout, simulate: bool) -> PipelineConfig {
        PipelineConfig {
            layout,
            geometry: PanelGeometry::for_layout(layout),
            bleed: BleedProfile::oled_default(),
            profile: DisplayProfile::sdr_oled(),
            simulate,
            sim_scale: SIM,
            psf_fwhm_arcmin: DEFAULT_PSF_FWHM_ARCMIN,
            psf_sigma: None,
            chroma_cutoff_cpd: csf::CHROMA_HALF_CPD,
        }
    }

    fn pipeline(layout: SubpixelLayout, simulate: bool) -> Pipeline {
        Pipeline::new(config(layout, simulate), Coverage::from_hi(stroke(4), 4), stroke(SIM)).unwrap()
    }

    fn tuning() -> Tuning {
        Tuning { backend: Backend::Cpu, render: RenderParams::default(), viewing_distance_mm: 500.0 }
    }

    #[test]
    fn backend_metrics_match_or_report_cpu_fallback() {
        for layout in SubpixelLayout::ALL {
            let mut p = pipeline(layout, true);
            let cpu = p.run(&tuning());
            for backend in [Backend::OpenGl, Backend::Vulkan] {
                let out = p.run(&Tuning { backend, ..tuning() });
                if let Some(error) = &out.backend_error {
                    eprintln!("{backend} fallback: {error}");
                    assert_eq!(out.backend, Backend::Cpu);
                    assert!(!error.is_empty());
                } else {
                    assert_eq!(out.backend, backend);
                }
                for (a, b) in cpu.metrics.unwrap().iter().zip(out.metrics.unwrap()) {
                    assert!((a.luminance_error - b.luminance_error).abs() < 1e-4);
                    assert!((a.visible_chroma - b.visible_chroma).abs() < 1e-4);
                    assert!((a.chroma - b.chroma).abs() < 1e-4);
                }
            }
        }
    }

    #[test]
    fn output_has_three_grids_at_both_stages() {
        let out = pipeline(SubpixelLayout::PentileRgbg, true).run(&tuning());
        assert_eq!((out.raw.grey.width, out.raw.grey.height, out.raw.scale), (10, 9, 1));
        let sim = out.simulated.unwrap();
        assert_eq!((sim.oled.width, sim.oled.height, sim.scale), (10 * SIM, 9 * SIM, SIM));
        assert!(out.metrics.is_some());
        assert!(out.sigma > 0.3 && out.sigma < 0.6);
    }

    #[test]
    fn simulation_can_be_turned_off() {
        let mut p = pipeline(SubpixelLayout::PentileRgbg, false);
        let out = p.run(&tuning());
        assert!(out.simulated.is_none() && out.metrics.is_none());
        assert!(p.objective(&RenderParams::default(), 500.0).is_none());
        assert!(p.auto_tune(&tuning()).is_none());
    }

    #[test]
    fn oled_aware_beats_both_baselines_on_every_colour_layout() {
        for layout in [SubpixelLayout::RgbStripe, SubpixelLayout::BgrStripe, SubpixelLayout::PentileRgbg, SubpixelLayout::DeltaRgb] {
            let [grey, subpixel, oled] = pipeline(layout, true).run(&tuning()).metrics.unwrap();
            assert!(oled.luminance_error < grey.luminance_error, "{layout:?} luminance vs grey: {oled:?} {grey:?}");
            assert!(oled.luminance_error < subpixel.luminance_error, "{layout:?} luminance vs subpixel: {oled:?} {subpixel:?}");
            assert!(oled.visible_chroma < grey.visible_chroma, "{layout:?} chroma vs grey: {oled:?} {grey:?}");
            assert!(oled.visible_chroma < subpixel.visible_chroma, "{layout:?} chroma vs subpixel: {oled:?} {subpixel:?}");
        }
    }

    #[test]
    fn oled_aware_lowers_luminance_error_on_wrgb() {
        let [grey, _, oled] = pipeline(SubpixelLayout::Wrgb, true).run(&tuning()).metrics.unwrap();
        assert!(oled.luminance_error < grey.luminance_error, "{oled:?} vs {grey:?}");
    }

    #[test]
    fn fixed_sigma_overrides_the_viewing_distance() {
        let mut cfg = config(SubpixelLayout::RgbStripe, true);
        cfg.psf_sigma = Some(0.8);
        let p = Pipeline::new(cfg, Coverage::from_hi(stroke(4), 4), stroke(SIM)).unwrap();
        assert_eq!(p.sigma(300.0), 0.8);
        assert_eq!(p.sigma(900.0), 0.8);
        let derived = pipeline(SubpixelLayout::RgbStripe, true);
        assert!(derived.sigma(900.0) > derived.sigma(300.0));
    }

    #[test]
    fn auto_tune_never_makes_the_objective_worse() {
        for layout in [SubpixelLayout::RgbStripe, SubpixelLayout::PentileRgbg, SubpixelLayout::DeltaRgb] {
            let mut p = pipeline(layout, true);
            let start = Tuning { backend: Backend::Cpu, render: RenderParams { luma_footprint: 0.5, chroma_footprint: 2.0, edge_suppression: 0.0 }, viewing_distance_mm: 500.0 };
            let before = p.objective(&start.render, 500.0).unwrap();
            let tuned = p.auto_tune(&start).unwrap();
            let after = p.objective(&tuned, 500.0).unwrap();
            assert!(after < before, "{layout:?}: {before} → {after}");
        }
    }

    #[test]
    fn mismatched_reference_is_rejected() {
        let err = Pipeline::new(config(SubpixelLayout::RgbStripe, true), Coverage::from_hi(stroke(4), 4), stroke(3));
        assert!(err.is_err());
    }

    #[test]
    fn oversized_unit_cells_are_rejected() {
        let mut cfg = config(SubpixelLayout::PentileRgbg, true);
        cfg.geometry.cell_w = 3;
        assert!(Pipeline::new(cfg, Coverage::from_hi(stroke(4), 4), stroke(SIM)).is_err());
    }
}

/// One sentence describing the optical blur for the inspector header.
pub fn simulation_note(sigma: f32) -> String {
    if sigma > 0.0 {
        format!("Blurred as the eye would see it (sigma {sigma:.2} px). Zoom in to see the emitters.")
    } else {
        "No optical blur: these are the bare emitters.".to_owned()
    }
}
