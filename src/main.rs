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

use clap::Parser;
use kage::{
    font::FontFace,
    layout::SubpixelLayout,
    profile::DisplayProfile,
    render::{render, RenderMode},
    simulate::{simulate, SimulationParams},
    viz::Inspector,
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

    /// Enable Phase 2 optical blur simulation.
    /// Press S in the inspector to toggle between raw and simulated views.
    #[arg(long, default_value_t = false)]
    simulate: bool,

    /// PSF sigma in pixel units for optical blur simulation.
    /// Overrides --viewing-dist. Default: 0.45 (typical 109dpi OLED at 50cm).
    #[arg(long)]
    psf_sigma: Option<f32>,

    /// Viewing distance in mm for physically-derived PSF sigma.
    /// Uses display DPI to convert to pixel units.
    /// Ignored if --psf-sigma is set.
    #[arg(long, default_value_t = 500.0)]
    viewing_dist: f32,
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
    let sim_params = if args.simulate {
        if let Some(sigma) = args.psf_sigma {
            let mut p = SimulationParams::oled_default();
            p.sigma = sigma;
            p
        } else {
            SimulationParams::from_viewing_distance(&profile, args.viewing_dist, 0.5)
        }
    } else {
        SimulationParams::identity()
    };

    println!("KAGE — Kinetic Adaptive Glyph Engine");
    println!("  Font:       {}", args.font);
    println!("  Glyph:      '{}'  (U+{:04X})", args.glyph, args.glyph as u32);
    println!("  Size:       {}px", args.size);
    println!("  Layout:     {:?}", layout);
    println!("  DPI:        {} (DPR {}×)", profile.dpi, profile.device_pixel_ratio);
    println!("  HiDPI:      {}", profile.is_hidpi());
    if args.simulate {
        println!("  Simulate:   ON  (sigma={:.3}px)", sim_params.sigma);
    } else {
        println!("  Simulate:   OFF  (press S in inspector to toggle)");
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
    let oled_raw  = render(&glyph_buf, RenderMode::OledAware,  layout, &profile);

    // ── Simulate (linear light — PSF convolution) ──────────────────────────────
    let grey_sim  = simulate(&grey_raw,  &sim_params);
    let sp_sim    = simulate(&sp_raw,    &sim_params);
    let oled_sim  = simulate(&oled_raw,  &sim_params);

    println!("Rendered three grids ({}×{}):", grey_raw.width, grey_raw.height);
    println!("  Greyscale AA, Subpixel AA, OLED-Aware");
    if args.simulate {
        println!("  Simulated grids ready (PSF sigma={:.3}px)", sim_params.sigma);
    }
    println!();
    println!("Controls:");
    println!("  Scroll/+/-  zoom (anchored to cursor)");
    println!("  Click+drag  pan");
    println!("  Arrow keys  pan (fine)");
    println!("  1/2/3       single panel  |  A  side-by-side");
    println!("  S           toggle raw ↔ simulated");
    println!("  H           heatmap overlay");
    println!("  Esc         quit");

    // ── Inspector window ───────────────────────────────────────────────────────
    let mut inspector = match Inspector::new(
        &format!("KAGE — '{}' @ {}px — {:?}", args.glyph, args.size, layout),
        args.simulate,
    ) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open inspector window: {e}");
            std::process::exit(1);
        }
    };

    // Main event loop
    while inspector.is_open() {
        if let Err(e) = inspector.update(
            &grey_raw,  &sp_raw,  &oled_raw,
            &grey_sim,  &sp_sim,  &oled_sim,
            &profile,
        ) {
            eprintln!("Inspector error: {e}");
            break;
        }
    }
}
