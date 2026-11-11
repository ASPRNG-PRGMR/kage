//! Run with `cargo run --release --example pipeline_timing -- /path/to/font.ttf [sim-scale]`.
use kage::{coverage::Coverage, font::FontFace, gpu::Backend, layout::SubpixelLayout, pipeline::{Pipeline, PipelineConfig, Tuning}, profile::DisplayProfile, render::{csf, RenderParams}, simulate::{BleedProfile, PanelGeometry, DEFAULT_PSF_FWHM_ARCMIN}};
use std::time::Instant;
fn main() {
    let font = std::env::args().nth(1).expect("pass a font path");
    let scale: u32 = std::env::args().nth(2).map(|s| s.parse().expect("integer simulation scale")).unwrap_or(6);
    let face = FontFace::load(&font, 64).unwrap();
    for backend in Backend::ALL {
        let config = PipelineConfig { layout: SubpixelLayout::PentileRgbg, geometry: PanelGeometry::for_layout(SubpixelLayout::PentileRgbg), bleed: BleedProfile::oled_default(), profile: DisplayProfile::sdr_oled(), simulate: true, sim_scale: scale, psf_fwhm_arcmin: DEFAULT_PSF_FWHM_ARCMIN, psf_sigma: None, chroma_cutoff_cpd: csf::CHROMA_HALF_CPD };
        let (cov, _) = Coverage::rasterize(&face, 'A', 4).unwrap();
        let (fine, _) = Coverage::rasterize(&face, 'A', scale).unwrap();
        let mut p = Pipeline::new(config, cov, fine.hi).unwrap();
        let mut tuning = Tuning { backend, render: RenderParams::default(), viewing_distance_mm: 500.0 };
        let first = p.run(&tuning);
        let mut render = 0.0;
        let start = Instant::now();
        for i in 0..20 { tuning.render.luma_footprint = 1.0 + (i % 2) as f32 * 0.05; render += p.run(&tuning).render_time.as_secs_f64(); }
        println!("{} {}: render {:.3} ms, cached update {:.3} ms", backend, first.device, render * 50.0, start.elapsed().as_secs_f64() * 50.0);
        let start = Instant::now();
        for i in 0..10 { tuning.viewing_distance_mm = 500.0 + (i % 2) as f32 * 25.0; p.run(&tuning); }
        println!("distance update {:.3} ms", start.elapsed().as_secs_f64() * 100.0);
    }
}
