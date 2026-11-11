//! Zoomable subpixel inspector.
//!
//! [`Inspector`] shows one glyph rendered three ways — Greyscale AA,
//! Subpixel AA and OLED-Aware — as three titled panels in a [`minifb`]
//! window, with a header explaining what is on screen and a footer listing
//! the controls.
//!
//! It holds two [`ViewSet`]s: the raw renderer output (one sample per logical
//! pixel) and, optionally, the simulated panel image (oversampled, with each
//! emitter at its physical position). Zoom and the view centre are expressed
//! in logical pixels, so toggling between the two keeps the glyph in place.
//!
//! ## Controls
//!
//! | Input | Action |
//! |-------|--------|
//! | Scroll wheel | Zoom in / out, anchored to cursor position |
//! | `+` / `=`, `-` | Zoom in / out (keyboard) |
//! | Click + drag | Pan |
//! | Arrow keys | Pan by one pixel |
//! | `R` | Reset: fit the glyph to the panel |
//! | `1` `2` `3` | Show a single panel |
//! | `A` | All three side by side (default) |
//! | `S` | Toggle raw ↔ simulated panel view |
//! | `L` | Toggle luminance-only view |
//! | `E` | Toggle auto-exposure ↔ clipping (simulated view) |
//! | `H` | Toggle per-channel heatmap overlay |
//! | `Esc` | Quit |

use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};

use super::text::TextRenderer;
use super::tuning::SETTINGS;
use crate::pipeline::{simulation_note, Pipeline, Tuning};
use crate::profile::DisplayProfile;
use crate::render::encode_grid;
use crate::simulate::reconstruct_luminance;
use crate::subpixel::SubpixelGrid;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Fraction of screen width/height the window occupies on open.
const SCREEN_FRACTION: f64 = 0.80;
/// Fallback window width if screen resolution cannot be determined.
const FALLBACK_WIN_W: usize = 1280;
/// Fallback window height.
const FALLBACK_WIN_H: usize = 800;

// Layout, in host pixels at interface scale 1.0 (see `Ui`).
const HEADER_H: i32 = 112;
const FOOTER_H: i32 = 48;
const TUNING_H: i32 = 112;
const MARGIN: i32 = 16;
const GAP: i32 = 14;
const CARD_HEAD_H: i32 = 96;
const PAD: i32 = 16;

// Text sizes, in pixels at interface scale 1.0.
const SIZE_TITLE: i32 = 30;
const SIZE_TAGLINE: i32 = 17;
const SIZE_BODY: i32 = 16;
const SIZE_CARD_TITLE: i32 = 21;
const SIZE_CARD_BODY: i32 = 15;
const SIZE_SMALL: i32 = 14;

// Palette.
const BG: u32 = 0xFF_0F_11_15;
const HEADER_BG: u32 = 0xFF_16_19_22;
const CARD_BG: u32 = 0xFF_18_1B_24;
const VIEW_BG: u32 = 0xFF_08_09_0C;
const BORDER: u32 = 0xFF_2B_31_3D;
const GLYPH_OUTLINE: u32 = 0xFF_2E_34_42;
/// Headings and anything that must stand out.
const TEXT: u32 = 0xFF_F4_F6_FA;
/// Running text.
const TEXT_BODY: u32 = 0xFF_CD_D3_DE;
/// Labels and secondary detail.
const TEXT_DIM: u32 = 0xFF_A3_AB_BA;
const CHIP_BG: u32 = 0xFF_27_2D_3A;
const CHIP_ON_BG: u32 = 0xFF_2F_55_91;
const CHIP_ON_TEXT: u32 = 0xFF_FF_FF_FF;
const KEY_BG: u32 = 0xFF_33_3A_49;

const ZOOM_MIN: f32 = 1.0;
const ZOOM_MAX: f32 = 64.0;
const ZOOM_STEP: f32 = 1.2;

/// Title, accent colour and one-line description of each panel.
const PANELS: [(&str, u32, &str); 3] = [
    (
        "Greyscale AA",
        0xFF_4A_90_D9,
        "The baseline. Every subpixel gets the same value: no colour fringes, but the softest edges.",
    ),
    (
        "Subpixel AA",
        0xFF_E8_7B_3A,
        "ClearType-style. A fixed kernel filters each colour channel of the pixel grid: edges pick up colour.",
    ),
    (
        "OLED-Aware",
        0xFF_5A_C9_5A,
        "KAGE's approach. Each colour is sampled from the glyph at the spot where its emitter sits on this panel.",
    ),
];

// ── Screen resolution detection ───────────────────────────────────────────────

/// Attempt to read the primary display resolution from the kernel DRM subsystem.
///
/// `/sys/class/drm/` is available on Linux regardless of whether the session
/// is X11 or Wayland.  Each connected output exposes a `modes` file whose
/// first line is the preferred/active mode in `WxH` format.
///
/// Falls back to `xrandr` output parsing (X11 / XWayland), then to the
/// hardcoded fallback dimensions if neither source is readable.
fn detect_screen_size() -> (usize, usize) {
    // ── Strategy 1: /sys/class/drm (X11 + Wayland, no subprocess) ────────
    if let Some(res) = read_drm_modes() {
        return res;
    }

    // ── Strategy 2: xrandr (X11 / XWayland only) ─────────────────────────
    if let Some(res) = read_xrandr() {
        return res;
    }

    // ── Strategy 3: hardcoded fallback ────────────────────────────────────
    (FALLBACK_WIN_W, FALLBACK_WIN_H)
}

/// Parse `/sys/class/drm/card*/card*-*/modes` for the first valid `WxH` entry.
fn read_drm_modes() -> Option<(usize, usize)> {
    use std::fs;

    let drm = std::path::Path::new("/sys/class/drm");
    if !drm.exists() {
        return None;
    }

    let entries = fs::read_dir(drm).ok()?;

    for entry in entries.flatten() {
        let path = entry.path().join("modes");
        if !path.exists() {
            continue;
        }
        let content = fs::read_to_string(&path).ok()?;
        let first_line = content.lines().next()?;
        if let Some((w, h)) = parse_wxh(first_line) {
            if w > 0 && h > 0 {
                return Some((w, h));
            }
        }
    }
    None
}

/// Run `xrandr` and parse the first connected output's current mode.
fn read_xrandr() -> Option<(usize, usize)> {
    use std::process::Command;

    let output = Command::new("xrandr").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);

    // Look for lines like: "   1920x1080     60.00*+"
    // The asterisk marks the current active mode.
    for line in text.lines() {
        if line.contains('*') {
            let token = line.split_whitespace().next()?;
            if let Some((w, h)) = parse_wxh(token) {
                return Some((w, h));
            }
        }
    }
    None
}

/// Parse a `WxH` or `W×H` string into `(width, height)`.
fn parse_wxh(s: &str) -> Option<(usize, usize)> {
    // Accept both ASCII 'x' and Unicode '×' as separators.
    let sep = if s.contains('x') { 'x' } else { '×' };
    let mut parts = s.splitn(2, sep);
    let w = parts.next()?.trim().parse::<usize>().ok()?;
    let h = parts.next()?.trim()
        // strip trailing characters like refresh rate suffixes ("1080p60")
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<usize>()
        .ok()?;
    Some((w, h))
}

// ── ViewSet ───────────────────────────────────────────────────────────────────

/// The three rendering modes' output at one stage of the pipeline.
///
/// All grids are in **linear light** and share the same dimensions.
#[derive(Debug, Clone)]
pub struct ViewSet {
    pub grey: SubpixelGrid,
    pub subpixel: SubpixelGrid,
    pub oled: SubpixelGrid,
    /// Samples per logical pixel along each axis: 1 for renderer output,
    /// the oversampling factor for a simulated panel image.
    pub scale: u32,
}

impl ViewSet {
    /// Largest channel value across all three grids.
    fn peak(&self) -> f32 {
        let mut peak = 0.0f32;
        for grid in [&self.grey, &self.subpixel, &self.oled] {
            for row in 0..grid.height {
                for col in 0..grid.width {
                    let px = grid.pixel(col, row);
                    peak = peak.max(px.r).max(px.g).max(px.b);
                }
            }
        }
        peak
    }
}

/// Scale every channel of `grid` by `factor`.
fn scale_grid(mut grid: SubpixelGrid, factor: f32) -> SubpixelGrid {
    for row in 0..grid.height {
        for col in 0..grid.width {
            let px = grid.pixel_mut(col, row);
            px.r *= factor;
            px.g *= factor;
            px.b *= factor;
        }
    }
    grid
}

// ── Configuration ─────────────────────────────────────────────────────────────

/// Simulation quality numbers for one rendering mode, shown in its panel
/// heading while the simulated view is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelMetrics {
    /// RMS luminance difference from what an ideal display would show.
    pub luminance_error: f32,
    /// RMS of all colour in the simulated image, visible or not.
    pub chroma: f32,
    /// RMS colour left after the eye's chromatic low-pass: fringing.
    pub visible_chroma: f32,
}

/// What the inspector should say about what it is showing.
#[derive(Debug, Clone, Default)]
pub struct InspectorConfig {
    /// OS window title.
    pub window_title: String,
    /// What is being rendered, e.g. `Glyph 'g' · Noto Sans · 64 px`.
    pub subject: String,
    /// Display name of the target subpixel layout, e.g. `Pentile RGBG`.
    pub layout_name: String,
    /// Short description of the simulation settings, e.g. blur width.
    pub simulation_note: Option<String>,
    /// Per-panel metrics, in panel order (greyscale, subpixel AA, OLED-aware).
    pub metrics: Option<[PanelMetrics; 3]>,
    /// Font file used for the interface text. Without one (or if it fails to
    /// load) the inspector still works, but shows no text.
    pub ui_font: Option<String>,
}

// ── Layout ────────────────────────────────────────────────────────────────────

/// A rectangle in host pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x as f32
            && px < (self.x + self.w) as f32
            && py >= self.y as f32
            && py < (self.y + self.h) as f32
    }

    fn right(&self) -> i32 {
        self.x + self.w
    }
}

/// One panel: a heading above a zoomable viewport.
#[derive(Debug, Clone, Copy)]
struct Card {
    /// Which rendering mode it shows (index into [`PANELS`]).
    index: usize,
    /// The whole card, heading included.
    outer: Rect,
    /// The viewport the glyph is drawn into.
    view: Rect,
}

/// Interface scale: every layout length and text size is multiplied by it,
/// so the interface stays readable when the window is large.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ui(f32);

impl Ui {
    /// Scale for a window `win_h` pixels tall: 1.0 up to 900 px, growing to
    /// 1.5 for tall windows, in steps of 1/8 so only a few text sizes are
    /// ever rasterized.
    fn for_window(win_h: usize) -> Self {
        let raw = (win_h as f32 / 900.0).clamp(1.0, 1.5);
        Ui((raw * 8.0).floor() / 8.0)
    }

    /// Scale a length.
    fn len(self, v: i32) -> i32 {
        (v as f32 * self.0).round() as i32
    }

    /// Scale a text size.
    fn px(self, v: i32) -> u32 {
        self.len(v).max(1) as u32
    }
}

/// Lay out the cards for a window of `win_w` × `win_h`.
fn layout_cards(win_w: usize, win_h: usize, mode: DisplayMode, ui: Ui, tuning_height: i32) -> Vec<Card> {
    let indices: &[usize] = match mode {
        DisplayMode::Greyscale => &[0],
        DisplayMode::SubpixelAa => &[1],
        DisplayMode::OledAware => &[2],
        DisplayMode::SideBySide => &[0, 1, 2],
    };
    let n = indices.len() as i32;
    let (margin, gap, head) = (ui.len(MARGIN), ui.len(GAP), ui.len(CARD_HEAD_H));
    let top = ui.len(HEADER_H) + margin;
    let bottom = win_h as i32 - ui.len(FOOTER_H + tuning_height) - margin;
    let region_w = win_w as i32 - 2 * margin;
    let card_w = ((region_w - gap * (n - 1)) / n).max(0);
    let card_h = (bottom - top).max(0);

    indices
        .iter()
        .enumerate()
        .map(|(slot, &index)| {
            let outer = Rect {
                x: margin + slot as i32 * (card_w + gap),
                y: top,
                w: card_w,
                h: card_h,
            };
            let view = Rect {
                x: outer.x + 1,
                y: outer.y + head,
                w: (outer.w - 2).max(0),
                h: (outer.h - head - 1).max(0),
            };
            Card { index, outer, view }
        })
        .collect()
}

/// Break `text` into at most `max_lines` lines no wider than `max_w`,
/// measuring with `width`. Text that does not fit ends in an ellipsis.
fn wrap_text(text: &str, max_w: i32, max_lines: usize, mut width: impl FnMut(&str) -> i32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut words = text.split_whitespace().peekable();

    while let Some(word) = words.next() {
        let candidate = if current.is_empty() { word.to_owned() } else { format!("{current} {word}") };
        if current.is_empty() || width(&candidate) <= max_w {
            current = candidate;
        } else if lines.len() + 1 < max_lines {
            lines.push(std::mem::replace(&mut current, word.to_owned()));
        } else {
            // Out of lines: drop words until the ellipsis fits.
            let mut cut = current;
            while !cut.is_empty() && width(&format!("{cut}…")) > max_w {
                match cut.rfind(' ') {
                    Some(i) => cut.truncate(i),
                    None => cut.clear(),
                }
            }
            lines.push(format!("{cut}…"));
            return lines;
        }
        if words.peek().is_none() {
            break;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Whether to open the window through X11 rather than native Wayland.
///
/// minifb's Wayland backend gives no title bar on compositors without
/// server-side decorations (GNOME), so the window cannot be maximised or
/// moved with the mouse. Through XWayland the window manager supplies the
/// usual title bar. Set `KAGE_BACKEND=wayland` to keep the native backend.
fn prefer_x11() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let forced_wayland = std::env::var("KAGE_BACKEND").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"));
    set("WAYLAND_DISPLAY") && set("DISPLAY") && !forced_wayland
}

// ── Inspector ─────────────────────────────────────────────────────────────────

/// Display mode for the inspector window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// Show only the greyscale grid.
    Greyscale,
    /// Show only the subpixel AA grid.
    SubpixelAa,
    /// Show only the OLED-aware grid.
    OledAware,
    /// Show all three side-by-side.
    SideBySide,
}

/// Everything that determines what a frame looks like. A frame is only
/// redrawn when this changes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FrameState {
    win_w: usize,
    win_h: usize,
    zoom: f32,
    center_x: f32,
    center_y: f32,
    display_mode: DisplayMode,
    simulate_mode: bool,
    luminance: bool,
    auto_exposure: bool,
    heatmap: bool,
    revision: u64,
    selected: usize,
}

/// Interactive zoomable subpixel inspector.
pub struct Inspector {
    window: Option<Window>,
    pipeline: Option<Pipeline>,
    tuning: Option<Tuning>,
    starting: Option<Tuning>,
    selected: usize,
    revision: u64,
    backend_status: String,
    backend_error: Option<String>,
    config: InspectorConfig,
    text: Option<TextRenderer>,
    fb: Vec<u32>,
    win_w: usize,
    win_h: usize,
    /// Smallest size the interface is drawn at. On native Wayland this is
    /// the size the window opened at, because the compositor reports
    /// spurious smaller sizes; elsewhere it is a small fixed floor.
    min_win_w: usize,
    min_win_h: usize,
    /// True when running on native Wayland, where a positive scroll value
    /// means "down"; on X11 it means "up".
    wayland: bool,

    /// Renderer output, linear light.
    raw: ViewSet,
    /// Simulated panel image, linear light (`None` if simulation is off).
    simulated: Option<ViewSet>,
    profile: DisplayProfile,
    /// Display-ready (EOTF-encoded) grids for the current view settings,
    /// in panel order: greyscale, subpixel AA, OLED-aware.
    shown: [SubpixelGrid; 3],
    /// Samples per logical pixel of `shown`.
    shown_scale: u32,

    /// Zoom level: each logical pixel is rendered as `zoom × zoom` host pixels.
    zoom: f32,
    /// Logical-pixel coordinate shown at the centre of every viewport.
    center_x: f32,
    center_y: f32,
    /// While true the glyph is kept fitted to the viewport (until the user
    /// zooms or pans).
    auto_fit: bool,

    /// Mouse position on the previous frame of a drag.
    drag_last: Option<(f32, f32)>,

    display_mode: DisplayMode,
    /// When true, show the simulated panel image instead of renderer output.
    simulate_mode: bool,
    /// When true, show reconstructed luminance instead of colour.
    luminance: bool,
    /// When true, the simulated view is scaled so its brightest emitter fits
    /// in range; when false, values above 1.0 clip.
    auto_exposure: bool,
    /// When true, R/G/B channels are shown as false-colour heatmaps.
    heatmap: bool,

    /// State of the frame currently in `fb`.
    drawn: Option<FrameState>,
}

impl Inspector {
    /// Create a new inspector window sized to 80% of the primary display.
    ///
    /// `raw` is the renderer output and `simulated` the optional panel
    /// simulation; both must be in **linear light**. EOTF encoding with
    /// `profile` happens inside the inspector, once per view change.
    pub fn new(
        config: InspectorConfig,
        raw: ViewSet,
        simulated: Option<ViewSet>,
        profile: DisplayProfile,
    ) -> Result<Self, minifb::Error> {
        Self::build(config, raw, simulated, profile, false)
    }

    /// Create a deterministic 1280×800 inspector for snapshots, without a display server.
    pub fn headless(config: InspectorConfig, raw: ViewSet, simulated: Option<ViewSet>, profile: DisplayProfile) -> Result<Self, minifb::Error> {
        Self::build(config, raw, simulated, profile, true)
    }

    fn build(config: InspectorConfig, raw: ViewSet, simulated: Option<ViewSet>, profile: DisplayProfile, headless: bool) -> Result<Self, minifb::Error> {
        let (screen_w, screen_h) = if headless { (1600, 1000) } else { detect_screen_size() };
        let win_w = ((screen_w as f64 * SCREEN_FRACTION) as usize).max(640);
        let win_h = ((screen_h as f64 * SCREEN_FRACTION) as usize).max(400);

        let options = || WindowOptions { resize: true, ..Default::default() };

        // Prefer X11 (XWayland) so the window gets a title bar with
        // maximise / close buttons; fall back to native Wayland if that fails.
        let wayland_display = std::env::var_os("WAYLAND_DISPLAY");
        let mut wayland = wayland_display.as_ref().is_some_and(|v| !v.is_empty());
        let win = if headless { None } else {
        let mut win = if prefer_x11() {
            std::env::remove_var("WAYLAND_DISPLAY");
            match Window::new(&config.window_title, win_w, win_h, options()) {
                Ok(win) => {
                    wayland = false;
                    win
                }
                Err(_) => {
                    if let Some(value) = &wayland_display {
                        std::env::set_var("WAYLAND_DISPLAY", value);
                    }
                    Window::new(&config.window_title, win_w, win_h, options())?
                }
            }
        } else {
            Window::new(&config.window_title, win_w, win_h, options())?
        };
        win.set_target_fps(60);
        Some(win)
        };

        let (min_win_w, min_win_h) = if wayland { (win_w, win_h) } else { (480, 360) };

        let text = config.ui_font.as_deref().and_then(TextRenderer::new);

        let mut inspector = Self {
            window: win,
            pipeline: None, tuning: None, starting: None, selected: 0, revision: 0,
            backend_status: String::new(), backend_error: None,
            config,
            text,
            fb: vec![BG; win_w * win_h],
            win_w,
            win_h,
            min_win_w,
            min_win_h,
            wayland,
            raw,
            simulated,
            profile,
            shown: [
                SubpixelGrid::new(0, 0),
                SubpixelGrid::new(0, 0),
                SubpixelGrid::new(0, 0),
            ],
            shown_scale: 1,
            zoom: 8.0,
            center_x: 0.0,
            center_y: 0.0,
            auto_fit: true,
            drag_last: None,
            display_mode: DisplayMode::SideBySide,
            simulate_mode: false,
            luminance: false,
            auto_exposure: true,
            heatmap: false,
            drawn: None,
        };
        inspector.rebuild_view();
        inspector.fit();
        Ok(inspector)
    }

    /// Returns `true` as long as the window is open and Esc has not been pressed.
    pub fn is_open(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.is_open() && !w.is_key_down(Key::Escape))
    }

    /// Process input events and redraw if anything changed.
    pub fn update(&mut self) -> Result<(), minifb::Error> {
        // Follow the window size, down to the minimum (see `min_win_w`).
        let (new_w, new_h) = self.window.as_ref().map_or((self.win_w, self.win_h), Window::get_size);
        self.win_w = new_w.max(self.min_win_w);
        self.win_h = new_h.max(self.min_win_h);
        self.fb.resize(self.win_w * self.win_h, BG);

        if self.window.is_some() { self.handle_input(); }
        if self.auto_fit {
            self.fit();
        }

        let state = self.frame_state();
        if self.drawn != Some(state) {
            self.draw();
            self.drawn = Some(state);
        }

        if let Some(window) = &mut self.window { window.update_with_buffer(&self.fb, self.win_w, self.win_h)?; }
        Ok(())
    }

    /// Attach the live pipeline, remembering the initial settings for Backspace.
    pub fn attach_pipeline(&mut self, pipeline: Pipeline, tuning: Tuning) {
        self.pipeline = Some(pipeline);
        self.tuning = Some(tuning);
        self.starting = Some(tuning);
        self.refresh_tuning();
    }

    fn refresh_tuning(&mut self) {
        let (Some(pipeline), Some(tuning)) = (&mut self.pipeline, self.tuning) else { return; };
        let out = pipeline.run(&tuning);
        self.backend_status = format!("{}  {:.3} ms", out.backend, out.render_time.as_secs_f64() * 1000.0);
        self.backend_error = out.backend_error.map(|e| format!("{} → CPU: {e}", tuning.backend));
        self.config.simulation_note = out.simulated.as_ref().map(|_| simulation_note(out.sigma));
        self.config.metrics = out.metrics;
        self.raw = out.raw;
        self.simulated = out.simulated;
        self.revision += 1;
        self.rebuild_view();
    }

    fn handle_tuning(&mut self) {
        let Some(mut tuning) = self.tuning else { return; };
        let window = self.window.as_ref().unwrap();
        if window.is_key_pressed(Key::Tab, KeyRepeat::Yes) {
            let backwards = window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);
            self.selected = (self.selected + if backwards { SETTINGS.len() - 1 } else { 1 }) % SETTINGS.len();
        }
        for (key, direction) in [(Key::LeftBracket, -1.0), (Key::RightBracket, 1.0)] {
            if window.is_key_pressed(key, KeyRepeat::Yes) { SETTINGS[self.selected].adjust(&mut tuning, direction); }
        }
        if window.is_key_pressed(Key::B, KeyRepeat::No) { tuning.backend = tuning.backend.next(); }
        if window.is_key_pressed(Key::Backspace, KeyRepeat::No) { tuning = self.starting.unwrap(); }
        if window.is_key_pressed(Key::T, KeyRepeat::No) {
            if let Some(best) = self.pipeline.as_mut().and_then(|p| p.auto_tune(&tuning)) { tuning.render = best; }
        }
        if Some(tuning) != self.tuning {
            self.tuning = Some(tuning);
            self.refresh_tuning();
        }
    }

    /// Switch between the raw render and the simulated panel image.
    /// Has no effect if no simulation was supplied.
    pub fn set_simulated(&mut self, simulated: bool) {
        if self.simulated.is_some() && self.simulate_mode != simulated {
            self.simulate_mode = simulated;
            self.rebuild_view();
        }
    }

    /// Write the frame currently on screen to a binary PPM image.
    pub fn save_ppm(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        use std::io::Write;
        let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
        write!(out, "P6\n{} {}\n255\n", self.win_w, self.win_h)?;
        for px in &self.fb {
            out.write_all(&[(px >> 16) as u8, (px >> 8) as u8, *px as u8])?;
        }
        out.flush()
    }

    fn frame_state(&self) -> FrameState {
        FrameState {
            win_w: self.win_w,
            win_h: self.win_h,
            zoom: self.zoom,
            center_x: self.center_x,
            center_y: self.center_y,
            display_mode: self.display_mode,
            simulate_mode: self.simulate_mode,
            luminance: self.luminance,
            auto_exposure: self.auto_exposure,
            heatmap: self.heatmap,
            revision: self.revision,
            selected: self.selected,
        }
    }

    fn ui(&self) -> Ui {
        Ui::for_window(self.win_h)
    }

    fn tuning_height(&self) -> i32 {
        if self.pipeline.is_none() { 0 } else if self.win_w < 800 { TUNING_H + 32 } else { TUNING_H }
    }

    fn cards(&self) -> Vec<Card> {
        layout_cards(self.win_w, self.win_h, self.display_mode, self.ui(), self.tuning_height())
    }

    /// Whether the simulated image is what is currently on screen.
    fn showing_simulation(&self) -> bool {
        self.simulate_mode && self.simulated.is_some()
    }

    /// Centre the glyph and zoom so it fills most of a viewport.
    fn fit(&mut self) {
        let (gw, gh) = (self.raw.grey.width as f32, self.raw.grey.height as f32);
        self.center_x = gw / 2.0;
        self.center_y = gh / 2.0;
        if let Some(card) = self.cards().first() {
            if gw > 0.0 && gh > 0.0 && card.view.w > 0 && card.view.h > 0 {
                let fit = (card.view.w as f32 / gw).min(card.view.h as f32 / gh) * 0.82;
                self.zoom = fit.clamp(ZOOM_MIN, ZOOM_MAX);
            }
        }
    }

    // ── View pipeline ─────────────────────────────────────────────────────────

    /// Recompute the display-ready grids for the current view settings.
    ///
    /// Everything up to the final step stays in linear light:
    /// select raw / simulated → (luminance) → (exposure) → EOTF encode.
    fn rebuild_view(&mut self) {
        let simulated = self.showing_simulation();
        let set = match (&self.simulated, simulated) {
            (Some(sim), true) => sim,
            _ => &self.raw,
        };

        // One exposure factor for all three panels keeps them comparable.
        // The simulated image is luminance-preserving, so bare emitters
        // exceed 1.0; renderer output never does.
        let exposure = if simulated && self.auto_exposure {
            1.0 / set.peak().max(1.0)
        } else {
            1.0
        };

        let prepare = |grid: &SubpixelGrid| {
            let grid = if self.luminance {
                reconstruct_luminance(grid)
            } else {
                grid.clone()
            };
            let grid = if exposure != 1.0 { scale_grid(grid, exposure) } else { grid };
            encode_grid(grid, &self.profile)
        };

        self.shown = [prepare(&set.grey), prepare(&set.subpixel), prepare(&set.oled)];
        self.shown_scale = set.scale.max(1);
        // The grids changed even if no view flag did (first build).
        self.drawn = None;
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    /// Multiply the zoom by `factor`, keeping the logical pixel at host
    /// offset (`rel_x`, `rel_y`) from the viewport centre where it is.
    fn zoom_about(&mut self, factor: f32, rel_x: f32, rel_y: f32) {
        let old = self.zoom;
        let new = (old * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        // src = center + rel / old  must equal  center' + rel / new
        self.center_x += rel_x / old - rel_x / new;
        self.center_y += rel_y / old - rel_y / new;
        self.zoom = new;
        self.auto_fit = false;
    }

    fn handle_input(&mut self) {
        self.handle_tuning();
        // ── Scroll-wheel zoom, anchored to the cursor ──────────────────────
        //
        // Which axis carries vertical scroll differs between backends, so the
        // axis with the larger magnitude is used. Scrolling up zooms in: that
        // is a positive value on X11 and a negative one on Wayland.
        if let Some((scroll_x, scroll_y)) = self.window.as_ref().unwrap().get_scroll_wheel() {
            let raw = if scroll_x.abs() >= scroll_y.abs() { scroll_x } else { scroll_y };
            if raw.abs() > 0.05 {
                let (mx, my) = self
                    .window.as_ref().unwrap()
                    .get_mouse_pos(MouseMode::Clamp)
                    .unwrap_or((self.win_w as f32 / 2.0, self.win_h as f32 / 2.0));

                // Anchor on the viewport under the cursor (or the first one).
                let cards = self.cards();
                let view = cards
                    .iter()
                    .find(|c| c.outer.contains(mx, my))
                    .or(cards.first())
                    .map(|c| c.view);
                let (rel_x, rel_y) = match view {
                    Some(v) => (
                        mx - (v.x as f32 + v.w as f32 / 2.0),
                        my - (v.y as f32 + v.h as f32 / 2.0),
                    ),
                    None => (0.0, 0.0),
                };

                let zoom_in = (raw > 0.0) != self.wayland;
                let factor = if zoom_in { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
                self.zoom_about(factor, rel_x, rel_y);
            }
        }

        // ── Keyboard zoom, about the viewport centre ───────────────────────
        if self.window.as_ref().unwrap().is_key_pressed(Key::Equal, KeyRepeat::Yes) {
            self.zoom_about(ZOOM_STEP, 0.0, 0.0);
        }
        if self.window.as_ref().unwrap().is_key_pressed(Key::Minus, KeyRepeat::Yes) {
            self.zoom_about(1.0 / ZOOM_STEP, 0.0, 0.0);
        }

        // ── Pan (arrow keys): one logical pixel per press ──────────────────
        for (key, dx, dy) in [
            (Key::Left, -1.0, 0.0),
            (Key::Right, 1.0, 0.0),
            (Key::Up, 0.0, -1.0),
            (Key::Down, 0.0, 1.0),
        ] {
            if self.window.as_ref().unwrap().is_key_pressed(key, KeyRepeat::Yes) {
                self.center_x += dx;
                self.center_y += dy;
                self.auto_fit = false;
            }
        }

        // ── Reset ──────────────────────────────────────────────────────────
        if self.window.as_ref().unwrap().is_key_pressed(Key::R, KeyRepeat::No) {
            self.auto_fit = true;
        }

        // ── View toggles (each changes what is encoded for display) ────────
        let mut view_changed = false;

        if self.window.as_ref().unwrap().is_key_pressed(Key::S, KeyRepeat::No) && self.simulated.is_some() {
            self.simulate_mode = !self.simulate_mode;
            view_changed = true;
        }
        if self.window.as_ref().unwrap().is_key_pressed(Key::L, KeyRepeat::No) {
            self.luminance = !self.luminance;
            view_changed = true;
        }
        if self.window.as_ref().unwrap().is_key_pressed(Key::E, KeyRepeat::No) {
            self.auto_exposure = !self.auto_exposure;
            view_changed = true;
        }
        if self.window.as_ref().unwrap().is_key_pressed(Key::H, KeyRepeat::No) {
            self.heatmap = !self.heatmap;
        }

        if view_changed {
            self.rebuild_view();
        }

        // ── Display mode ───────────────────────────────────────────────────
        for (key, mode) in [
            (Key::Key1, DisplayMode::Greyscale),
            (Key::Key2, DisplayMode::SubpixelAa),
            (Key::Key3, DisplayMode::OledAware),
            (Key::A, DisplayMode::SideBySide),
        ] {
            if self.window.as_ref().unwrap().is_key_pressed(key, KeyRepeat::No) {
                self.display_mode = mode;
            }
        }

        // ── Mouse drag pan: the glyph follows the cursor ───────────────────
        let mouse_down = self.window.as_ref().unwrap().get_mouse_down(MouseButton::Left);
        let mouse_pos = self.window.as_ref().unwrap().get_mouse_pos(MouseMode::Clamp).unwrap_or((0.0, 0.0));

        if mouse_down {
            if let Some((last_x, last_y)) = self.drag_last {
                let (dx, dy) = (mouse_pos.0 - last_x, mouse_pos.1 - last_y);
                if dx != 0.0 || dy != 0.0 {
                    self.center_x -= dx / self.zoom;
                    self.center_y -= dy / self.zoom;
                    self.auto_fit = false;
                }
            }
            self.drag_last = Some(mouse_pos);
        } else {
            self.drag_last = None;
        }
    }

    // ── Drawing primitives ────────────────────────────────────────────────────

    fn fill_rect(&mut self, r: Rect, color: u32) {
        let x0 = r.x.max(0) as usize;
        let y0 = r.y.max(0) as usize;
        let x1 = (r.x + r.w).clamp(0, self.win_w as i32) as usize;
        let y1 = (r.y + r.h).clamp(0, self.win_h as i32) as usize;
        if x0 >= x1 {
            return;
        }
        for y in y0..y1 {
            self.fb[y * self.win_w + x0..y * self.win_w + x1].fill(color);
        }
    }

    /// Draw text with its top-left at (`x`, `y`); returns the end x.
    fn text(&mut self, x: i32, y: i32, px: u32, color: u32, clip_right: i32, s: &str) -> i32 {
        match self.text.as_mut() {
            Some(t) => t.draw(&mut self.fb, self.win_w, self.win_h, x, y, px, color, clip_right, s),
            None => x,
        }
    }

    fn text_width(&mut self, px: u32, s: &str) -> i32 {
        self.text.as_mut().map_or(0, |t| t.width(px, s))
    }

    /// Draw `text` wrapped to `max_lines` lines within `left..right`,
    /// starting at `y`. Returns the y-coordinate below the last line.
    #[allow(clippy::too_many_arguments)]
    fn paragraph(&mut self, left: i32, right: i32, y: i32, px: u32, color: u32, max_lines: usize, s: &str) -> i32 {
        let line_h = (px as f32 * 1.45).round() as i32;
        let lines = match self.text.as_mut() {
            Some(t) => wrap_text(s, right - left, max_lines, |line| t.width(px, line)),
            None => return y,
        };
        let mut line_y = y;
        for line in &lines {
            self.text(left, line_y, px, color, right, line);
            line_y += line_h;
        }
        line_y
    }

    /// Draw a small labelled box whose right edge is at `right`.
    /// Returns the x-coordinate of its left edge.
    fn chip_right(&mut self, right: i32, y: i32, label: &str, on: bool) -> i32 {
        let ui = self.ui();
        let px = ui.px(SIZE_SMALL);
        let pad = ui.len(11);
        let w = self.text_width(px, label) + 2 * pad;
        let h = ui.len(28);
        let x = right - w;
        let (bg, fg) = if on { (CHIP_ON_BG, CHIP_ON_TEXT) } else { (CHIP_BG, TEXT_BODY) };
        self.fill_rect(Rect { x, y, w, h }, bg);
        self.text(x + pad, y + (h - px as i32) / 2 - ui.len(2), px, fg, right, label);
        x
    }

    // ── Drawing ───────────────────────────────────────────────────────────────

    fn draw(&mut self) {
        self.fb.fill(BG);
        self.draw_header();
        for card in self.cards() {
            self.draw_card(card);
        }
        self.draw_tuning();
        self.draw_footer();
    }

    /// Title, the current view state, and a sentence saying what is on screen.
    fn draw_header(&mut self) {
        let ui = self.ui();
        let w = self.win_w as i32;
        let header_h = ui.len(HEADER_H);
        self.fill_rect(Rect { x: 0, y: 0, w, h: header_h }, HEADER_BG);
        self.fill_rect(Rect { x: 0, y: header_h - 1, w, h: 1 }, BORDER);

        let left = ui.len(MARGIN) + 2;
        let right = w - ui.len(MARGIN) - 2;

        // Row 1, right: view state.
        let chip_y = ui.len(14);
        let gap = ui.len(8);
        let simulated = self.showing_simulation();
        let zoom_label = format!("Zoom {:.1}×", self.zoom);
        let mut cx = self.chip_right(right, chip_y, &zoom_label, false);
        if self.heatmap {
            cx = self.chip_right(cx - gap, chip_y, "Heatmap", true);
        }
        if self.luminance {
            cx = self.chip_right(cx - gap, chip_y, "Luminance only", true);
        }
        if simulated && !self.auto_exposure {
            cx = self.chip_right(cx - gap, chip_y, "Clipped", true);
        }
        cx = if simulated {
            self.chip_right(cx - gap, chip_y, "View: simulated panel", true)
        } else {
            self.chip_right(cx - gap, chip_y, "View: raw render", false)
        };

        // Row 1, left: name and tagline.
        let clip = cx - ui.len(16);
        let x = self.text(left, ui.len(8), ui.px(SIZE_TITLE), TEXT, clip, "KAGE");
        self.text(
            x + ui.len(14),
            ui.len(19),
            ui.px(SIZE_TAGLINE),
            TEXT_DIM,
            clip,
            "OLED-aware subpixel text rendering inspector",
        );

        // Rows 2–3, right: what is being rendered.
        let body = ui.px(SIZE_BODY);
        let row2 = ui.len(54);
        let subject = self.config.subject.clone();
        let subject_x = (right - self.text_width(body, &subject)).max(left);
        self.text(subject_x, row2, body, TEXT_DIM, right, &subject);

        // Rows 2–3, left: what the viewer is looking at.
        let layout = &self.config.layout_name;
        let line = if simulated {
            let note = self.config.simulation_note.as_deref().unwrap_or("");
            format!(
                "How a {layout} panel would actually show each version: every subpixel is drawn where it physically sits. {note}"
            )
        } else if self.simulated.is_some() {
            format!(
                "One glyph, antialiased three ways for a {layout} display. You are looking at the renderer's output: one RGB value per pixel. Press S to see how the physical panel would show it."
            )
        } else {
            format!(
                "One glyph, antialiased three ways for a {layout} display. You are looking at the renderer's output: one RGB value per pixel."
            )
        };
        self.paragraph(left, subject_x - ui.len(32), row2, body, TEXT_BODY, 2, &line);
    }

    /// One panel: accent bar, title, metrics, description, viewport.
    fn draw_card(&mut self, card: Card) {
        let ui = self.ui();
        let (name, accent, description) = PANELS[card.index];
        let o = card.outer;

        self.fill_rect(o, BORDER);
        self.fill_rect(Rect { x: o.x + 1, y: o.y + 1, w: o.w - 2, h: o.h - 2 }, CARD_BG);
        self.fill_rect(Rect { x: o.x, y: o.y, w: o.w, h: ui.len(4) }, accent);

        let left = o.x + ui.len(PAD);
        let right = o.right() - ui.len(PAD);
        let title_px = ui.px(SIZE_CARD_TITLE);
        let name_end = self.text(left, o.y + ui.len(13), title_px, TEXT, right, name);

        // Metrics, right-aligned on the title line, when they fit.
        if self.showing_simulation() {
            if let Some(metrics) = self.config.metrics {
                let m = metrics[card.index];
                let small = ui.px(SIZE_SMALL);
                let label = format!(
                    "Luminance error {:.3}    Colour fringing {:.3}",
                    m.luminance_error, m.visible_chroma
                );
                let x = right - self.text_width(small, &label);
                if x > name_end + ui.len(16) {
                    self.text(x, o.y + ui.len(19), small, TEXT_DIM, right, &label);
                }
            }
        }

        self.paragraph(left, right, o.y + ui.len(46), ui.px(SIZE_CARD_BODY), TEXT_BODY, 2, description);
        self.fill_rect(Rect { x: o.x + 1, y: card.view.y - 1, w: o.w - 2, h: 1 }, BORDER);

        self.draw_view(card);
    }

    /// Blit `self.shown[card.index]` into the card's viewport.
    ///
    /// Each logical pixel covers `zoom × zoom` host pixels and is made of
    /// `shown_scale × shown_scale` grid samples. The logical point
    /// (`center_x`, `center_y`) lands at the centre of the viewport.
    fn draw_view(&mut self, card: Card) {
        let v = card.view;
        if v.w <= 0 || v.h <= 0 {
            return;
        }
        let zoom = self.zoom;
        let scale = self.shown_scale as f32;
        let show_lines = zoom >= 8.0;
        let heatmap = self.heatmap;
        let grid = &self.shown[card.index];
        let (src_w, src_h) = (grid.width as i32, grid.height as i32);

        // Host offset → (grid sample index, starts a new logical pixel).
        let axis = |len: i32, center: f32| -> Vec<(i32, bool)> {
            let mut prev = i32::MIN;
            (0..len)
                .map(|i| {
                    let logical = center + (i as f32 + 0.5 - len as f32 / 2.0) / zoom;
                    let cell = logical.floor() as i32;
                    let boundary = prev != i32::MIN && cell != prev;
                    prev = cell;
                    ((logical * scale).floor() as i32, boundary)
                })
                .collect()
        };
        let cols = axis(v.w, self.center_x);
        let rows = axis(v.h, self.center_y);

        let (win_w, win_h) = (self.win_w as i32, self.win_h as i32);
        for (wy, &(src_row, line_y)) in rows.iter().enumerate() {
            let fb_y = v.y + wy as i32;
            if fb_y < 0 || fb_y >= win_h {
                continue;
            }
            for (wx, &(src_col, line_x)) in cols.iter().enumerate() {
                let fb_x = v.x + wx as i32;
                if fb_x < 0 || fb_x >= win_w {
                    continue;
                }
                let inside = src_col >= 0 && src_col < src_w && src_row >= 0 && src_row < src_h;
                let color = if inside {
                    let px = grid.pixel(src_col as u32, src_row as u32);
                    let color = if heatmap {
                        heatmap_color(px.r, px.g, px.b)
                    } else {
                        pack_rgb(px.r, px.g, px.b)
                    };
                    // Faint lines on logical-pixel boundaries.
                    if show_lines && (line_x || line_y) { lighten(color, 0.10) } else { color }
                } else {
                    VIEW_BG
                };
                self.fb[(fb_y * win_w + fb_x) as usize] = color;
            }
        }

        // Outline the glyph bitmap so its extent is visible against the
        // background (the bitmap itself is mostly black).
        let host_x = |lx: f32| v.x + (v.w as f32 / 2.0 + (lx - self.center_x) * zoom).round() as i32;
        let host_y = |ly: f32| v.y + (v.h as f32 / 2.0 + (ly - self.center_y) * zoom).round() as i32;
        let (gw, gh) = (src_w as f32 / scale, src_h as f32 / scale);
        let (x0, x1) = (host_x(0.0) - 1, host_x(gw));
        let (y0, y1) = (host_y(0.0) - 1, host_y(gh));
        let clip = |r: Rect| {
            let cx0 = r.x.max(v.x);
            let cy0 = r.y.max(v.y);
            let cx1 = (r.x + r.w).min(v.right());
            let cy1 = (r.y + r.h).min(v.y + v.h);
            Rect { x: cx0, y: cy0, w: (cx1 - cx0).max(0), h: (cy1 - cy0).max(0) }
        };
        for r in [
            Rect { x: x0, y: y0, w: x1 - x0 + 1, h: 1 },
            Rect { x: x0, y: y1, w: x1 - x0 + 1, h: 1 },
            Rect { x: x0, y: y0, w: 1, h: y1 - y0 + 1 },
            Rect { x: x1, y: y0, w: 1, h: y1 - y0 + 1 },
        ] {
            let r = clip(r);
            if r.w > 0 && r.h > 0 {
                self.fill_rect(r, GLYPH_OUTLINE);
            }
        }
    }

    fn draw_tuning(&mut self) {
        let Some(tuning) = self.tuning else { return; };
        let ui = self.ui();
        let y = self.win_h as i32 - ui.len(FOOTER_H + self.tuning_height());
        let mut chip_y = y;
        let right = self.win_w as i32 - ui.len(MARGIN);
        let mut x = ui.len(MARGIN);
        for (i, setting) in SETTINGS.iter().enumerate() {
            let label = format!("{} {:.2} {}", setting.name, (setting.get)(&tuning), setting.unit);
            let width = self.text_width(ui.px(SIZE_SMALL), &label) + ui.len(22);
            if x + width > right { x = ui.len(MARGIN); chip_y += ui.len(32); }
            self.chip_right(x + width, chip_y, &label, i == self.selected);
            x += width + ui.len(8);
        }
        let status = self.backend_status.clone();
        // At narrow widths, move status below the chips instead of overlapping them.
        let status_y = if x + self.text_width(ui.px(SIZE_SMALL), &status) > right { chip_y + ui.len(32) } else { chip_y };
        self.chip_right(right, status_y, &status, false);
        self.text(ui.len(MARGIN), y + ui.len(self.tuning_height() - 48), ui.px(SIZE_SMALL), TEXT_DIM, right,
            "Tab / Shift+Tab: select   [ / ]: adjust   T: auto-tune   B: backend   Backspace: reset settings");
        if let Some(error) = self.backend_error.clone() {
            self.text(ui.len(MARGIN), y + ui.len(self.tuning_height() - 24), ui.px(SIZE_SMALL), TEXT_BODY, right, &error);
        }
    }

    /// Key hints along the bottom edge.
    fn draw_footer(&mut self) {
        let ui = self.ui();
        let w = self.win_w as i32;
        let footer_h = ui.len(FOOTER_H);
        let top = self.win_h as i32 - footer_h;
        self.fill_rect(Rect { x: 0, y: top, w, h: footer_h }, HEADER_BG);
        self.fill_rect(Rect { x: 0, y: top, w, h: 1 }, BORDER);

        let simulate_hint = if self.simulated.is_some() { "Simulated panel" } else { "Simulation off" };
        let hints = [
            ("Scroll", "Zoom"),
            ("Drag", "Pan"),
            ("R", "Reset view"),
            ("1  2  3", "One panel"),
            ("A", "All panels"),
            ("S", simulate_hint),
            ("L", "Luminance"),
            ("E", "Exposure"),
            ("H", "Heatmap"),
            ("Esc", "Quit"),
        ];

        let px = ui.px(SIZE_SMALL);
        let key_h = ui.len(26);
        let key_pad = ui.len(9);
        let right = w - ui.len(MARGIN);
        let y = top + (footer_h - key_h) / 2;
        let text_y = y + (key_h - px as i32) / 2 - ui.len(2);
        let mut x = ui.len(MARGIN);
        for (key, label) in hints {
            let key_w = self.text_width(px, key) + 2 * key_pad;
            let label_w = self.text_width(px, label);
            if x + key_w + ui.len(8) + label_w > right {
                break;
            }
            self.fill_rect(Rect { x, y, w: key_w, h: key_h }, KEY_BG);
            self.text(x + key_pad, text_y, px, TEXT, right, key);
            x = self.text(x + key_w + ui.len(8), text_y, px, TEXT_BODY, right, label) + ui.len(22);
        }
    }
}

/// Pack signal-domain R/G/B in [0, 1] into `0xFF_RR_GG_BB`.
#[inline]
fn pack_rgb(r: f32, g: f32, b: f32) -> u32 {
    let r = (r.clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (g.clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (b.clamp(0.0, 1.0) * 255.0).round() as u32;
    (0xFF << 24) | (r << 16) | (g << 8) | b
}

/// Move each channel of `color` towards white by `amount` in [0, 1].
#[inline]
fn lighten(color: u32, amount: f32) -> u32 {
    let ch = |shift: u32| {
        let c = ((color >> shift) & 0xFF) as f32;
        ((c + (255.0 - c) * amount).round() as u32) << shift
    };
    0xFF00_0000 | ch(16) | ch(8) | ch(0)
}

/// Convert R/G/B values to a false-colour heatmap for per-channel analysis.
///
/// - R above the pixel's mean → red tint
/// - B above the pixel's mean → blue tint
/// - Near-grey → neutral
fn heatmap_color(r: f32, g: f32, b: f32) -> u32 {
    // Amplify channel imbalance for visibility
    let scale = 3.0;
    let grey = (r + g + b) / 3.0;
    let dr = ((r - grey) * scale).clamp(-1.0, 1.0);
    let db = ((b - grey) * scale).clamp(-1.0, 1.0);

    let base = (grey.clamp(0.0, 1.0) * 180.0) as u32;
    let r_out = (base as f32 + dr * 75.0).clamp(0.0, 255.0) as u32;
    let g_out = base;
    let b_out = (base as f32 + db * 75.0).clamp(0.0, 255.0) as u32;

    (0xFF << 24) | (r_out << 16) | (g_out << 8) | b_out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subpixel::SubpixelPixel;

    #[test]
    fn attached_pipeline_refreshes_grids_metrics_and_frame_revision() {
        use crate::{coverage::Coverage, gpu::Backend, layout::SubpixelLayout, pipeline::PipelineConfig, plane::Plane, render::{csf, RenderParams}, simulate::{BleedProfile, PanelGeometry, DEFAULT_PSF_FWHM_ARCMIN}};
        let layout = SubpixelLayout::PentileRgbg;
        let profile = DisplayProfile::sdr_oled();
        let config = PipelineConfig { layout, geometry: PanelGeometry::for_layout(layout), bleed: BleedProfile::oled_default(), profile: profile.clone(), simulate: true, sim_scale: 4, psf_fwhm_arcmin: DEFAULT_PSF_FWHM_ARCMIN, psf_sigma: None, chroma_cutoff_cpd: csf::CHROMA_HALF_CPD };
        let fine = Plane::from_fn(24, 24, |x, y| if x > y { 1.0 } else { 0.0 });
        let mut pipeline = Pipeline::new(config, Coverage::from_hi(fine.clone(), 4), fine).unwrap();
        let tuning = Tuning { backend: Backend::Cpu, render: RenderParams::default(), viewing_distance_mm: 500.0 };
        let out = pipeline.run(&tuning);
        let mut inspector = Inspector::headless(InspectorConfig::default(), out.raw, out.simulated, profile).unwrap();
        inspector.attach_pipeline(pipeline, tuning);
        inspector.set_simulated(true);
        inspector.update().unwrap();
        let before = inspector.frame_state();
        let metrics = inspector.config.metrics;
        let note = inspector.config.simulation_note.clone();
        inspector.tuning.as_mut().unwrap().viewing_distance_mm = 900.0;
        inspector.refresh_tuning();
        inspector.update().unwrap();
        assert_ne!(before, inspector.frame_state());
        assert_ne!(note, inspector.config.simulation_note);
        assert_ne!(metrics, inspector.config.metrics);
        assert!(inspector.simulate_mode);
        assert_eq!(inspector.starting, Some(tuning));
        let state = inspector.frame_state();
        inspector.selected = 1;
        assert_ne!(state, inspector.frame_state());
        inspector.win_w = 480;
        assert!(inspector.tuning_height() > TUNING_H);
    }

    #[test]
    fn parse_wxh_accepts_common_forms() {
        assert_eq!(parse_wxh("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_wxh("2560x1440p60"), Some((2560, 1440)));
        assert_eq!(parse_wxh("garbage"), None);
    }

    #[test]
    fn view_set_peak_spans_all_grids() {
        let mut oled = SubpixelGrid::new(2, 2);
        *oled.pixel_mut(1, 1) = SubpixelPixel { r: 0.2, g: 3.5, b: 0.1 };
        let set = ViewSet {
            grey: SubpixelGrid::new(2, 2),
            subpixel: SubpixelGrid::new(2, 2),
            oled,
            scale: 1,
        };
        assert_eq!(set.peak(), 3.5);
    }

    #[test]
    fn scale_grid_scales_every_channel() {
        let mut g = SubpixelGrid::new(1, 1);
        *g.pixel_mut(0, 0) = SubpixelPixel { r: 2.0, g: 4.0, b: 1.0 };
        let p = scale_grid(g, 0.25).pixel(0, 0);
        assert_eq!((p.r, p.g, p.b), (0.5, 1.0, 0.25));
    }

    #[test]
    fn heatmap_is_neutral_for_grey_and_tinted_for_imbalance() {
        let channels = |c: u32| ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
        let (r, g, b) = channels(heatmap_color(0.5, 0.5, 0.5));
        assert!(r == g && g == b);
        let (r, _, b) = channels(heatmap_color(0.9, 0.5, 0.1));
        assert!(r > b, "red excess should tint red");
    }

    #[test]
    fn side_by_side_lays_out_three_equal_cards_inside_the_window() {
        let cards = layout_cards(1280, 800, DisplayMode::SideBySide, Ui(1.0), TUNING_H);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards.iter().map(|c| c.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        for pair in cards.windows(2) {
            assert_eq!(pair[0].outer.w, pair[1].outer.w);
            assert_eq!(pair[1].outer.x - pair[0].outer.right(), GAP);
        }
        let last = cards[2];
        assert!(last.outer.right() <= 1280 - MARGIN);
        assert!(last.outer.y + last.outer.h <= 800 - FOOTER_H - TUNING_H);
        assert!(last.view.y >= last.outer.y + CARD_HEAD_H);
    }

    #[test]
    fn single_mode_shows_the_selected_panel_full_width() {
        let cards = layout_cards(1280, 800, DisplayMode::OledAware, Ui(1.0), TUNING_H);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].index, 2);
        assert_eq!(cards[0].outer.w, 1280 - 2 * MARGIN);
    }

    #[test]
    fn layout_survives_a_tiny_window() {
        for card in layout_cards(10, 10, DisplayMode::SideBySide, Ui(1.5), TUNING_H) {
            assert!(card.outer.w >= 0 && card.outer.h >= 0);
            assert!(card.view.w >= 0 && card.view.h >= 0);
        }
    }

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect { x: 10, y: 20, w: 5, h: 5 };
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(14.9, 24.9));
        assert!(!r.contains(15.0, 22.0));
        assert!(!r.contains(9.9, 22.0));
    }

    #[test]
    fn lighten_moves_towards_white() {
        assert_eq!(lighten(0xFF_00_00_00, 0.0), 0xFF_00_00_00);
        assert_eq!(lighten(0xFF_00_00_00, 1.0), 0xFF_FF_FF_FF);
        assert_eq!(lighten(0xFF_FF_FF_FF, 0.5), 0xFF_FF_FF_FF);
    }

    #[test]
    fn pack_rgb_clamps() {
        assert_eq!(pack_rgb(2.0, -1.0, 0.5), 0xFF_FF_00_80);
    }

    #[test]
    fn ui_scale_grows_with_window_height_in_steps() {
        assert_eq!(Ui::for_window(600), Ui(1.0));
        assert_eq!(Ui::for_window(900), Ui(1.0));
        assert_eq!(Ui::for_window(1200), Ui(1.25));
        assert_eq!(Ui::for_window(4000), Ui(1.5));
        assert_eq!(Ui(1.25).len(16), 20);
        assert_eq!(Ui(1.0).px(0), 1);
    }

    /// Eight pixels per character: enough to test the wrapping logic.
    fn mono(s: &str) -> i32 {
        s.chars().count() as i32 * 8
    }

    #[test]
    fn wrap_keeps_short_text_on_one_line() {
        assert_eq!(wrap_text("hello world", 200, 2, mono), vec!["hello world"]);
    }

    #[test]
    fn wrap_breaks_between_words() {
        // 80 px = 10 characters per line.
        assert_eq!(wrap_text("aaaa bbbb cccc", 80, 3, mono), vec!["aaaa bbbb", "cccc"]);
    }

    #[test]
    fn wrap_ends_with_an_ellipsis_when_out_of_lines() {
        let lines = wrap_text("aaaa bbbb cccc dddd eeee", 80, 2, mono);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "aaaa bbbb");
        assert!(lines[1].ends_with('…'), "{lines:?}");
        assert!(mono(&lines[1]) <= 80, "{lines:?}");
    }

    #[test]
    fn wrap_handles_empty_text() {
        assert!(wrap_text("", 100, 2, mono).is_empty());
    }
}
