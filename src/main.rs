//! KAGE — entry point and CLI.
//!
//! Usage:
//!   kage --font <path> --glyph <char> --layout <layout> [--size <px>] [--no-fringe]
//!
//! Examples:
//!   kage --font /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf --glyph A
//!   kage --font ./assets/fonts/test.ttf --glyph g --layout pentile --size 96
//!   kage --font ./assets/fonts/test.ttf --glyph Q --layout rgb --no-fringe

use clap::Parser;
use kage::{
    font::FontFace,
    layout::SubpixelLayout,
    profile::DisplayProfile,
    render::{render, RenderMode},
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

    println!("KAGE — Kinetic Adaptive Glyph Engine");
    println!("  Font:   {}", args.font);
    println!("  Glyph:  '{}'  (U+{:04X})", args.glyph, args.glyph as u32);
    println!("  Size:   {}px", args.size);
    println!("  Layout: {:?}", layout);
    println!("  DPI:    {} (DPR {}×)", profile.dpi, profile.device_pixel_ratio);
    println!("  HiDPI:  {}", profile.is_hidpi());
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
    // We use the greyscale FreeType bitmap as the source coverage map.
    // The render layer applies the per-layout filter to produce per-channel output.
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

    // ── Render three modes ─────────────────────────────────────────────────────
    let grey_grid = render(&glyph_buf, RenderMode::Greyscale,  layout, &profile);
    let sp_grid   = render(&glyph_buf, RenderMode::SubpixelAa, layout, &profile);
    let oled_grid = render(&glyph_buf, RenderMode::OledAware,  layout, &profile);

    println!("Rendered three grids:");
    println!("  Greyscale AA   — {}×{}", grey_grid.width, grey_grid.height);
    println!("  Subpixel AA    — {}×{}", sp_grid.width,   sp_grid.height);
    println!("  OLED-Aware     — {}×{}", oled_grid.width, oled_grid.height);
    println!();
    println!("Controls: +/- zoom · Arrows pan · 1/2/3 single panel · A side-by-side · H heatmap · Esc quit");

    // ── Inspector window ───────────────────────────────────────────────────────
    let mut inspector = match Inspector::new(&format!(
        "KAGE — '{}' @ {}px — {:?}",
        args.glyph, args.size, layout
    )) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open inspector window: {e}");
            std::process::exit(1);
        }
    };

    // Main event loop
    while inspector.is_open() {
        if let Err(e) = inspector.update(&grey_grid, &sp_grid, &oled_grid) {
            eprintln!("Inspector error: {e}");
            break;
        }
    }
}
