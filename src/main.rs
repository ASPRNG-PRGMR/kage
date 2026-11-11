//! KAGE — entry point and CLI.
//!
//! Usage:
//!   kage --font <path> --glyph <char> --layout <layout> [options]
//!
//! Examples:
//!   kage --font /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf --glyph A
//!   kage --font ./assets/fonts/test.ttf --glyph g --layout pentile --size 96
//!   kage --font ./assets/fonts/test.ttf --glyph Q --layout rgb --simulate
//!   kage --font ./assets/fonts/test.ttf --glyph g --simulate --psf-sigma 0.8
//!   kage --font ./assets/fonts/test.ttf --glyph g --simulate --psf-sigma 0   (bare emitters)

use clap::Parser;
use kage::{
    font::FontFace,
    layout::SubpixelLayout,
    profile::DisplayProfile,
    render::{render, RenderMode},
    simulate::{
        chroma_rms, integrate_pixels, luminance_rms_error, simulate_panel, PanelGeometry,
        SimulationParams, DEFAULT_OVERSAMPLE, DEFAULT_PSF_FWHM_ARCMIN,
    },
    subpixel::SubpixelGrid,
    viz::{Inspector, ViewSet},
};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "kage",
    about = "KAGE — Kinetic Adaptive Glyph Engine\nOLED-aware subpixel text rendering inspector",
    version
)]
struct Args {
    /// Path to a TrueType or OpenType font file.
    #[arg(short, long)]
    font: String,

    /// Unicode character to render (single character).
    #[arg(short, long, default_value = "A")]
    glyph: char,

    /// Subpixel layout of the target display.
    /// One of: rgb, bgr, pentile, delta, wrgb, grey
    #[arg(short, long, default_value = "pentile")]
    layout: String,

    /// Render size in device pixels (cap height).
    #[arg(short, long, default_value_t = 64)]
    size: u32,

    /// Disable adaptive fringe suppression in the OLED-aware renderer.
    #[arg(long, default_value_t = false)]
    no_fringe: bool,

    /// Display DPI (affects HiDPI fallback threshold).
    #[arg(long, default_value_t = 109.0)]
    dpi: f32,

    /// Device pixel ratio (1.0 = standard, 2.0 = HiDPI).
    #[arg(long, default_value_t = 1.0)]
    dpr: f32,

    /// Enable Phase 2 panel simulation: physical emitter placement for the
    /// chosen layout, then optical blur.
    /// Press S in the inspector to toggle between raw and simulated views.
    #[arg(long, default_value_t = false)]
    simulate: bool,

    /// PSF sigma in pixel units for optical blur simulation.
    /// Overrides --viewing-dist. 0 shows the bare emitters with no blur.
    /// Without this flag sigma is derived from --viewing-dist and --dpi
    /// (0.45 for a 109dpi panel at 50cm).
    #[arg(long)]
    psf_sigma: Option<f32>,

    /// Viewing distance in mm for physically-derived PSF sigma.
    /// Uses display DPI to convert to pixel units.
    /// Ignored if --psf-sigma is set.
    #[arg(long, default_value_t = 500.0)]
    viewing_dist: f32,

    /// PSF width (FWHM) in arcminutes at the viewer's eye, used with
    /// --viewing-dist to derive the PSF sigma. Ignored if --psf-sigma is set.
    #[arg(long, default_value_t = DEFAULT_PSF_FWHM_ARCMIN)]
    psf_fwhm: f32,

    /// Simulation oversampling: emission-map samples per pixel, per axis.
    /// Higher resolves emitter shapes more finely and costs more time.
    #[arg(long, default_value_t = DEFAULT_OVERSAMPLE, value_parser = clap::value_parser!(u32).range(1..=16))]
    sim_scale: u32,
}

fn parse_layout(s: &str) -> SubpixelLayout {
    match s.to_lowercase().as_str() {
        "rgb"     => SubpixelLayout::RgbStripe,
        "bgr"     => SubpixelLayout::BgrStripe,
        "pentile" => SubpixelLayout::PentileRgbg,
        "delta"   => SubpixelLayout::DeltaRgb,
        "wrgb"    => SubpixelLayout::Wrgb,
        "grey" | "gray" | "greyscale" | "grayscale" => SubpixelLayout::Greyscale,
        other => {
            eprintln!("Unknown layout '{other}', defaulting to pentile.");
            SubpixelLayout::PentileRgbg
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args = Args::parse();

    // ── Display profile ────────────────────────────────────────────────────────
    let mut profile = DisplayProfile::sdr_oled();
    profile.dpi = args.dpi;
    profile.device_pixel_ratio = args.dpr;

    let layout = parse_layout(&args.layout);

    // ── Simulation params ──────────────────────────────────────────────────────
    let sim_params = if let Some(sigma) = args.psf_sigma {
        let mut p = SimulationParams::oled_default();
        p.sigma = sigma.max(0.0);
        p
    } else {
        SimulationParams::from_viewing_distance(&profile, args.viewing_dist, args.psf_fwhm)
    };
    let geometry = PanelGeometry::for_layout(layout);

    println!("KAGE — Kinetic Adaptive Glyph Engine");
    println!("  Font:       {}", args.font);
    println!("  Glyph:      '{}'  (U+{:04X})", args.glyph, args.glyph as u32);
    println!("  Size:       {}px", args.size);
    println!("  Layout:     {:?}", layout);
    println!("  DPI:        {} (DPR {}×)", profile.dpi, profile.device_pixel_ratio);
    println!("  HiDPI:      {}", profile.is_hidpi());
    if args.simulate {
        println!("  Simulate:   ON  (sigma={:.3}px, {}× oversampling)", sim_params.sigma, args.sim_scale);
        println!("  Panel:      {}", geometry.name);
    } else {
        println!("  Simulate:   OFF  (re-run with --simulate)");
    }
    println!();

    // ── Font loading ───────────────────────────────────────────────────────────
    let face = match FontFace::load(&args.font, args.size) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error loading font '{}': {e}", args.font);
            std::process::exit(1);
        }
    };

    // ── Rasterize glyph ────────────────────────────────────────────────────────
    let bitmap = match face.rasterize_grey(args.glyph) {
        Some(b) => b,
        None => {
            eprintln!(
                "Glyph '{}' (U+{:04X}) not found in font.",
                args.glyph, args.glyph as u32
            );
            std::process::exit(1);
        }
    };

    println!(
        "Glyph bitmap: {}×{} pixels  bearing=({},{})  advance={}px",
        bitmap.width, bitmap.height, bitmap.bearing_x, bitmap.bearing_y, bitmap.advance
    );

    let glyph_buf = bitmap.into_glyph_buffer();

    // ── Render (linear light) ──────────────────────────────────────────────────
    let grey_raw  = render(&glyph_buf, RenderMode::Greyscale,  layout, &profile);
    let sp_raw    = render(&glyph_buf, RenderMode::SubpixelAa, layout, &profile);
    // The OLED-aware path is the layout filter plus fringe suppression, so
    // with suppression disabled it is the plain subpixel renderer.
    let oled_mode = if args.no_fringe { RenderMode::SubpixelAa } else { RenderMode::OledAware };
    let oled_raw  = render(&glyph_buf, oled_mode, layout, &profile);

    println!("Rendered three grids ({}×{}):", grey_raw.width, grey_raw.height);
    println!("  Greyscale AA, Subpixel AA, OLED-Aware");

    // ── Simulate (linear light — emitter placement + PSF convolution) ──────────
    let simulated = args.simulate.then(|| {
        let run = |grid: &SubpixelGrid| simulate_panel(grid, &geometry, &sim_params, args.sim_scale);
        let set = ViewSet {
            grey: run(&grey_raw),
            subpixel: run(&sp_raw),
            oled: run(&oled_raw),
            scale: args.sim_scale,
        };

        // Luminance error: the simulated image, integrated back to one value
        // per pixel, against the intended coverage. Chroma: colour residue in
        // what the viewer sees — fringing, for white-on-black text.
        println!();
        println!("Simulated panel ({}×{} samples):", set.grey.width, set.grey.height);
        println!("  {:<13} {:>14} {:>14}", "", "luminance err", "chroma residue");
        for (label, grid) in [
            ("Greyscale AA", &set.grey),
            ("Subpixel AA", &set.subpixel),
            ("OLED-Aware", &set.oled),
        ] {
            let per_pixel = integrate_pixels(grid, args.sim_scale);
            let luma_err = luminance_rms_error(&grey_raw, &per_pixel).unwrap_or(f32::NAN);
            println!("  {:<13} {:>14.4} {:>14.4}", label, luma_err, chroma_rms(grid));
        }
        set
    });

    println!();
    println!("Controls:");
    println!("  Scroll/+/-  zoom (anchored to cursor)");
    println!("  Click+drag  pan");
    println!("  Arrow keys  pan (fine)");
    println!("  1/2/3       single panel  |  A  side-by-side");
    println!("  S           toggle raw ↔ simulated panel");
    println!("  L           luminance-only view");
    println!("  E           auto-exposure ↔ clip (simulated view)");
    println!("  H           heatmap overlay");
    println!("  Esc         quit");

    // ── Inspector window ───────────────────────────────────────────────────────
    let raw = ViewSet {
        grey: grey_raw,
        subpixel: sp_raw,
        oled: oled_raw,
        scale: 1,
    };
    let mut inspector = match Inspector::new(
        &format!("KAGE — '{}' @ {}px — {:?}", args.glyph, args.size, layout),
        raw,
        simulated,
        profile,
    ) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open inspector window: {e}");
            std::process::exit(1);
        }
    };

    // Main event loop
    while inspector.is_open() {
        if let Err(e) = inspector.update() {
            eprintln!("Inspector error: {e}");
            break;
        }
    }
}
