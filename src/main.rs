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
    layout::SubpixelLayout,
    profile::DisplayProfile,
    render::{render, RenderMode},
    simulate::{
        chroma_rms, integrate_pixels, luminance_rms_error, simulate_panel, PanelGeometry,
        SimulationParams, DEFAULT_OVERSAMPLE, DEFAULT_PSF_FWHM_ARCMIN,
    },
    subpixel::SubpixelGrid,
    viz::{Inspector, InspectorConfig, PanelMetrics, ViewSet},
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
    /// Defaults to the system's sans-serif font (see `default_font`).
    #[arg(short, long)]
    font: Option<String>,

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

    /// Skip the panel simulation (physical emitter placement + optical
    /// blur). It is on by default; press S in the inspector to view it.
    #[arg(long, default_value_t = false)]
    no_simulate: bool,

    /// Accepted for compatibility; the simulation is on by default.
    #[arg(long, default_value_t = false, hide = true)]
    simulate: bool,

    /// Open the inspector showing the simulated panel instead of the raw render.
    #[arg(long, default_value_t = false)]
    start_simulated: bool,

    /// Write the first frame of the inspector to a PPM image and exit.
    #[arg(long, value_name = "FILE")]
    snapshot: Option<String>,

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

    let layout = parse_layout(&args.layout);

    let font = match args.font.clone().or_else(default_font) {
        Some(path) => path,
        None => {
            eprintln!("No default font found on this system. Pass one with --font <path>.");
            std::process::exit(1);
        }
    };

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
    println!("  Font:       {}{}", font, if args.font.is_none() { "  (system default)" } else { "" });
    println!("  Glyph:      '{}'  (U+{:04X})", args.glyph, args.glyph as u32);
    println!("  Size:       {}px", args.size);
    println!("  Layout:     {:?}", layout);
    println!("  DPI:        {} (DPR {}×)", profile.dpi, profile.device_pixel_ratio);
    println!("  HiDPI:      {}", profile.is_hidpi());
    let simulate = !args.no_simulate;
    let _ = args.simulate; // on by default; flag kept so old commands still run
    if simulate {
        println!("  Simulate:   ON  (sigma={:.3}px, {}× oversampling)", sim_params.sigma, args.sim_scale);
        println!("  Panel:      {}", geometry.name);
    } else {
        println!("  Simulate:   OFF  (--no-simulate)");
    }
    println!();

    // ── Font loading ───────────────────────────────────────────────────────────
    let face = match FontFace::load(&font, args.size) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error loading font '{font}': {e}");
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
    let mut metrics: Option<[PanelMetrics; 3]> = None;
    let simulated = simulate.then(|| {
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
        let measure = |label: &str, grid: &SubpixelGrid| {
            let per_pixel = integrate_pixels(grid, args.sim_scale);
            let m = PanelMetrics {
                luminance_error: luminance_rms_error(&grey_raw, &per_pixel).unwrap_or(f32::NAN),
                chroma: chroma_rms(grid),
            };
            println!("  {:<13} {:>14.4} {:>14.4}", label, m.luminance_error, m.chroma);
            m
        };
        metrics = Some([
            measure("Greyscale AA", &set.grey),
            measure("Subpixel AA", &set.subpixel),
            measure("OLED-Aware", &set.oled),
        ]);
        set
    });

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
    println!("  Esc         quit");

    // ── Inspector window ───────────────────────────────────────────────────────
    let raw = ViewSet {
        grey: grey_raw,
        subpixel: sp_raw,
        oled: oled_raw,
        scale: 1,
    };
    let font_name = std::path::Path::new(&font)
        .file_stem()
        // Variable fonts are often named like "NotoSans[wght]"; drop the axis list.
        .map(|stem| stem.to_string_lossy().split('[').next().unwrap_or_default().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| font.clone());
    let simulation_note = simulated.as_ref().map(|_| {
        if sim_params.sigma > 0.0 {
            format!(
                "Blurred as the eye would see it (sigma {:.2} px). Zoom in to see the emitters.",
                sim_params.sigma
            )
        } else {
            "No optical blur: these are the bare emitters.".to_owned()
        }
    });
    let config = InspectorConfig {
        window_title: format!("KAGE — '{}' @ {}px — {}", args.glyph, args.size, layout.label()),
        subject: format!("Glyph '{}'  ·  {}  ·  {} px", args.glyph, font_name, args.size),
        layout_name: layout.label().to_owned(),
        simulation_note,
        metrics,
        // Interface text always uses a system font, whatever --font is: the
        // font under test may not have Latin letters at all.
        ui_font: default_font().or_else(|| Some(font.clone())),
    };
    let mut inspector = match Inspector::new(config, raw, simulated, profile) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open inspector window: {e}");
            std::process::exit(1);
        }
    };
    if args.start_simulated {
        inspector.set_simulated(true);
    }

    if let Some(path) = &args.snapshot {
        let result = inspector.update().map_err(|e| e.to_string()).and_then(|()| {
            inspector.save_ppm(path).map_err(|e| e.to_string())
        });
        match result {
            Ok(()) => println!("Wrote snapshot to {path}"),
            Err(e) => {
                eprintln!("Snapshot failed: {e}");
                std::process::exit(1);
            }
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
