//! KAGE — entry point and CLI.
//!
//! Usage:
//!   kage [--font <path>] [--glyph <char>] [--layout <layout>] [options]
//!
//! With no --font, the system's default sans-serif font is used.
//!
//! Examples:
//!   kage
//!   kage --glyph g --layout pentile --simulate
//!   kage --font /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf --glyph A
//!   kage --font ./assets/fonts/test.ttf --glyph g --layout pentile --size 96
//!   kage --font ./assets/fonts/test.ttf --glyph Q --layout rgb --simulate
//!   kage --font ./assets/fonts/test.ttf --glyph g --simulate --psf-sigma 0.8
//!   kage --font ./assets/fonts/test.ttf --glyph g --simulate --psf-sigma 0   (bare emitters)

use clap::Parser;
use kage::{
    font::FontFace,
    gpu::Backend,
    layout::SubpixelLayout,
    pipeline::{simulation_note, Pipeline, PipelineConfig, Tuning, DEFAULT_SUPERSAMPLE},
    profile::DisplayProfile,
    render::{csf, RenderParams},
    simulate::{BleedProfile, PanelDescription, PanelGeometry, DEFAULT_OVERSAMPLE, DEFAULT_PSF_FWHM_ARCMIN},
    viz::{Inspector, InspectorConfig},
};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "kage",
    about = "KAGE — Kinetic Adaptive Glyph Engine\nOLED-aware subpixel text rendering inspector",
    version
)]
struct Args {
    /// Rendering backend (falls back to CPU if unavailable).
    #[arg(long, default_value = "cpu", value_parser = ["cpu", "opengl", "vulkan"])]
    backend: String,

    /// Path to a TrueType / OpenType font file. Defaults to the system's
    /// sans-serif font.
    #[arg(short, long)]
    font: Option<String>,

    /// Character to render.
    #[arg(short, long, default_value = "A")]
    glyph: char,

    /// Subpixel layout: rgb | bgr | pentile | delta | wrgb | grey.
    #[arg(short, long, default_value = "pentile")]
    layout: String,

    /// Panel description file (.toml) with emitter geometry and bleed.
    /// Replaces the layout's nominal geometry; see layouts/README.md.
    #[arg(long, value_name = "FILE")]
    panel: Option<String>,

    /// Glyph pixel size.
    #[arg(short, long, default_value_t = 64)]
    size: u32,

    /// Coverage samples per pixel per axis given to the renderers (1–8).
    /// 1 disables supersampling.
    #[arg(long, default_value_t = DEFAULT_SUPERSAMPLE, value_parser = clap::value_parser!(u32).range(1..=8))]
    supersample: u32,

    /// OLED-aware: footprint width of green / white emitters, in pixels.
    #[arg(long, value_name = "PX")]
    luma_footprint: Option<f32>,

    /// OLED-aware: footprint width of red / blue emitters, in pixels.
    #[arg(long, value_name = "PX")]
    chroma_footprint: Option<f32>,

    /// OLED-aware: how far red / blue move to the luminance footprint at
    /// strong edges (0–1).
    #[arg(long, value_name = "K")]
    edge_suppression: Option<f32>,

    /// Search for the OLED-aware settings with the lowest simulated error
    /// for this glyph, panel and viewing distance.
    #[arg(long, default_value_t = false)]
    auto_tune: bool,

    /// Accepted for compatibility: sets --edge-suppression 0.
    #[arg(long, default_value_t = false, hide = true)]
    no_fringe: bool,

    /// Display DPI (used for the HiDPI greyscale fallback and blur widths).
    #[arg(long, default_value_t = 109.0)]
    dpi: f32,

    /// Device pixel ratio.
    #[arg(long, default_value_t = 1.0)]
    dpr: f32,

    /// Skip the panel simulation (S, metrics and auto-tune are unavailable).
    #[arg(long, default_value_t = false)]
    no_simulate: bool,

    /// Accepted for compatibility: simulation is on by default.
    #[arg(long, default_value_t = false, hide = true)]
    simulate: bool,

    /// Open in the simulated-panel view.
    #[arg(long, default_value_t = false)]
    start_simulated: bool,

    /// Write the first frame to a PPM image and exit.
    #[arg(long, value_name = "FILE")]
    snapshot: Option<String>,

    /// Optical blur sigma in logical pixels; 0 disables blur. Overrides
    /// --viewing-dist and --psf-fwhm for the blur.
    #[arg(long)]
    psf_sigma: Option<f32>,

    /// Eye-to-panel distance in millimetres.
    #[arg(long, default_value_t = 500.0)]
    viewing_dist: f32,

    /// Angular width (FWHM) of the eye's point spread function, arcminutes.
    #[arg(long, default_value_t = DEFAULT_PSF_FWHM_ARCMIN)]
    psf_fwhm: f32,

    /// Frequency at which the eye's colour sensitivity has halved, in
    /// cycles per degree. Sets what counts as visible fringing.
    #[arg(long, default_value_t = csf::CHROMA_HALF_CPD)]
    chroma_cutoff: f32,

    /// Simulation samples per logical pixel per axis (1–16).
    #[arg(long, default_value_t = DEFAULT_OVERSAMPLE, value_parser = clap::value_parser!(u32).range(1..=16))]
    sim_scale: u32,
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

// ── Default font ──────────────────────────────────────────────────────────────

/// Common sans-serif fonts, tried in order when fontconfig is unavailable.
const FALLBACK_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
    "C:\\Windows\\Fonts\\arial.ttf",
];

/// Find a font to use when `--font` is not given.
///
/// Asks fontconfig for the system's default sans-serif (`fc-match`), then
/// falls back to a list of well-known paths.
fn default_font() -> Option<String> {
    let from_fontconfig = std::process::Command::new("fc-match")
        .args(["--format", "%{file}", "sans-serif"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|path| !path.is_empty() && std::path::Path::new(path).is_file());

    from_fontconfig.or_else(|| {
        FALLBACK_FONTS
            .iter()
            .find(|path| std::path::Path::new(path).is_file())
            .map(|path| (*path).to_owned())
    })
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args = Args::parse();

    // ── Display profile ────────────────────────────────────────────────────────
    let mut profile = DisplayProfile::sdr_oled();
    profile.dpi = args.dpi;
    profile.device_pixel_ratio = args.dpr;

    // ── Panel ──────────────────────────────────────────────────────────────────
    let mut layout = SubpixelLayout::from_name(&args.layout).unwrap_or_else(|| {
        eprintln!("Unknown layout '{}', defaulting to pentile.", args.layout);
        SubpixelLayout::PentileRgbg
    });
    let mut geometry = PanelGeometry::for_layout(layout);
    let mut bleed = BleedProfile::oled_default();
    let mut panel_source = None;
    if let Some(path) = &args.panel {
        let panel = PanelDescription::load(path).unwrap_or_else(|e| fail(format!("Cannot load panel file: {e}")));
        if let Some(l) = panel.layout {
            layout = l;
        }
        if let Some(b) = panel.bleed {
            bleed = b;
        }
        geometry = panel.geometry;
        panel_source = Some(panel.source.unwrap_or_else(|| path.clone()));
    }

    let font = args
        .font
        .clone()
        .or_else(default_font)
        .unwrap_or_else(|| fail("No default font found on this system. Pass one with --font <path>."));

    let simulate = !args.no_simulate;
    let _ = args.simulate; // on by default; flag kept so old commands still run

    println!("KAGE — Kinetic Adaptive Glyph Engine");
    println!("  Font:       {}{}", font, if args.font.is_none() { "  (system default)" } else { "" });
    println!("  Glyph:      '{}'  (U+{:04X})", args.glyph, args.glyph as u32);
    println!("  Size:       {}px  ({}× supersampled coverage)", args.size, args.supersample);
    println!("  Layout:     {}", layout.label());
    println!("  Panel:      {}{}", geometry.name, panel_source.map(|s| format!("  [{s}]")).unwrap_or_default());
    println!("  DPI:        {} (DPR {}×)", profile.dpi, profile.device_pixel_ratio);
    println!("  HiDPI:      {}", profile.is_hidpi());

    // ── Font and pipeline ──────────────────────────────────────────────────────
    let face = FontFace::load(&font, args.size).unwrap_or_else(|e| fail(format!("Error loading font '{font}': {e}")));
    let config = PipelineConfig {
        layout,
        geometry,
        bleed,
        profile: profile.clone(),
        simulate,
        sim_scale: args.sim_scale,
        psf_fwhm_arcmin: args.psf_fwhm,
        psf_sigma: args.psf_sigma.map(|s| s.max(0.0)),
        chroma_cutoff_cpd: args.chroma_cutoff,
    };
    let (mut pipeline, glyph) = match Pipeline::from_font(config, &face, args.glyph, args.supersample) {
        Ok(Some(built)) => built,
        Ok(None) => fail(format!("Glyph '{}' (U+{:04X}) not found in font.", args.glyph, args.glyph as u32)),
        Err(e) => fail(format!("Cannot render on this panel: {e}")),
    };
    let (width, height) = (pipeline.coverage().width(), pipeline.coverage().height());
    println!();
    println!(
        "Glyph bitmap: {width}×{height} pixels  bearing=({},{})  advance={}px",
        glyph.bearing_x, glyph.bearing_y, glyph.advance
    );

    // ── Settings ───────────────────────────────────────────────────────────────
    let mut render = RenderParams::for_layout(layout);
    if let Some(v) = args.luma_footprint {
        render.luma_footprint = v;
    }
    if let Some(v) = args.chroma_footprint {
        render.chroma_footprint = v;
    }
    if let Some(v) = args.edge_suppression {
        render.edge_suppression = v;
    }
    if args.no_fringe {
        render.edge_suppression = 0.0;
    }
    let mut tuning = Tuning { backend: Backend::from_name(&args.backend).unwrap(), render: render.clamped(), viewing_distance_mm: args.viewing_dist };
    if args.auto_tune {
        match pipeline.auto_tune(&tuning) {
            Some(best) => tuning.render = best,
            None => eprintln!("--auto-tune needs the simulation; ignoring it because of --no-simulate."),
        }
    }
    println!(
        "OLED-aware:   luma footprint {:.2} px, chroma footprint {:.2} px, edge suppression {:.2}{}",
        tuning.render.luma_footprint,
        tuning.render.chroma_footprint,
        tuning.render.edge_suppression,
        if args.auto_tune && simulate { "  (auto-tuned)" } else { "" }
    );

    // ── Render, simulate, score ────────────────────────────────────────────────
    let output = pipeline.run(&tuning);
    println!("Backend: {} — {} — {:.3} ms (three modes)", output.backend, output.device, output.render_time.as_secs_f64() * 1000.0);
    if let Some(error) = &output.backend_error { eprintln!("Warning: {} failed; using CPU: {error}", tuning.backend); }
    if let (Some(set), Some(metrics)) = (&output.simulated, &output.metrics) {
        println!();
        println!(
            "Simulated panel ({}×{} samples, blur sigma {:.3} px, viewed from {:.0} mm):",
            set.grey.width, set.grey.height, output.sigma, tuning.viewing_distance_mm
        );
        println!("  {:<13} {:>14} {:>15} {:>15}", "", "luminance err", "visible chroma", "chroma residue");
        for (label, m) in ["Greyscale AA", "Subpixel AA", "OLED-Aware"].iter().zip(metrics) {
            println!("  {:<13} {:>14.4} {:>15.4} {:>15.4}", label, m.luminance_error, m.visible_chroma, m.chroma);
        }
    } else {
        println!();
        println!("Simulation off (--no-simulate).");
    }

    println!();
    println!("Controls:");
    println!("  Scroll/+/-  zoom (anchored to cursor)");
    println!("  Click+drag  pan            Arrow keys  pan by one pixel");
    println!("  R           reset view");
    println!("  1/2/3       single panel   A  side-by-side");
    println!("  S           toggle raw ↔ simulated panel");
    println!("  L           luminance-only view");
    println!("  E           auto-exposure ↔ clip (simulated view)");
    println!("  H           heatmap overlay");
    println!("  Tab/Shift+Tab select setting; [/] adjust; T auto-tune; B backend; Backspace reset settings");
    println!("  Esc         quit");

    // ── Inspector window ───────────────────────────────────────────────────────
    let font_name = std::path::Path::new(&font)
        .file_stem()
        // Variable fonts are often named like "NotoSans[wght]"; drop the axis list.
        .map(|stem| stem.to_string_lossy().split('[').next().unwrap_or_default().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| font.clone());

    let config = InspectorConfig {
        window_title: format!("KAGE — '{}' @ {}px — {}", args.glyph, args.size, layout.label()),
        subject: format!("Glyph '{}'  ·  {}  ·  {} px", args.glyph, font_name, args.size),
        layout_name: layout.label().to_owned(),
        simulation_note: output.simulated.as_ref().map(|_| simulation_note(output.sigma)),
        metrics: output.metrics,
        // Interface text always uses a system font, whatever --font is: the
        // font under test may not have Latin letters at all.
        ui_font: default_font().or_else(|| Some(font.clone())),
    };

    let create = if args.snapshot.is_some() { Inspector::headless } else { Inspector::new };
    let mut inspector = create(config, output.raw, output.simulated, profile)
        .unwrap_or_else(|e| fail(format!("Failed to open inspector window: {e}")));
    inspector.attach_pipeline(pipeline, tuning);
    if args.start_simulated {
        inspector.set_simulated(true);
    }

    if let Some(path) = &args.snapshot {
        let result = inspector
            .update()
            .map_err(|e| e.to_string())
            .and_then(|()| inspector.save_ppm(path).map_err(|e| e.to_string()));
        match result {
            Ok(()) => println!("Wrote snapshot to {path}"),
            Err(e) => fail(format!("Snapshot failed: {e}")),
        }
        return;
    }

    // Main event loop
    while inspector.is_open() {
        if let Err(e) = inspector.update() {
            eprintln!("Inspector error: {e}");
            break;
        }
    }
}

