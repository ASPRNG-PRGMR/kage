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

// Layout, in host pixels.
const HEADER_H: usize = 72;
const FOOTER_H: usize = 38;
const MARGIN: usize = 14;
const GAP: usize = 12;
const CARD_HEAD_H: usize = 62;
const PAD: i32 = 14;

// Palette.
const BG: u32 = 0xFF_0F_11_15;
const HEADER_BG: u32 = 0xFF_15_18_21;
const CARD_BG: u32 = 0xFF_17_1A_22;
const VIEW_BG: u32 = 0xFF_08_09_0C;
const BORDER: u32 = 0xFF_26_2B_36;
const GLYPH_OUTLINE: u32 = 0xFF_2E_34_42;
const TEXT: u32 = 0xFF_E6_E9_EF;
const TEXT_DIM: u32 = 0xFF_8B_93_A3;
const CHIP_BG: u32 = 0xFF_22_27_33;
const CHIP_ON_BG: u32 = 0xFF_2A_42_6B;
const CHIP_ON_TEXT: u32 = 0xFF_D6_E6_FF;

const ZOOM_MIN: f32 = 1.0;
const ZOOM_MAX: f32 = 64.0;
const ZOOM_STEP: f32 = 1.2;

/// Title, accent colour and one-line description of each panel.
const PANELS: [(&str, u32, &str); 3] = [
    (
        "Greyscale AA",
        0xFF_4A_90_D9,
        "Baseline: every subpixel gets the same coverage. No colour fringes, softest edges.",
    ),
    (
        "Subpixel AA",
        0xFF_E8_7B_3A,
        "ClearType-style: each colour channel filtered separately. Sharper, but fringes.",
    ),
    (
        "OLED-Aware",
        0xFF_5A_C9_5A,
        "Filter matched to the panel's subpixel layout; fringes suppressed at hard edges.",
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
    /// RMS luminance difference from the intended coverage.
    pub luminance_error: f32,
    /// RMS colour residue in the simulated image (fringing).
    pub chroma: f32,
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

/// Lay out the cards for a window of `win_w` × `win_h`.
fn layout_cards(win_w: usize, win_h: usize, mode: DisplayMode) -> Vec<Card> {
    let indices: &[usize] = match mode {
        DisplayMode::Greyscale => &[0],
        DisplayMode::SubpixelAa => &[1],
        DisplayMode::OledAware => &[2],
        DisplayMode::SideBySide => &[0, 1, 2],
    };
    let n = indices.len() as i32;
    let top = (HEADER_H + MARGIN) as i32;
    let bottom = win_h as i32 - (FOOTER_H + MARGIN) as i32;
    let region_w = win_w as i32 - 2 * MARGIN as i32;
    let card_w = ((region_w - GAP as i32 * (n - 1)) / n).max(0);
    let card_h = (bottom - top).max(0);

    indices
        .iter()
        .enumerate()
        .map(|(slot, &index)| {
            let outer = Rect {
                x: MARGIN as i32 + slot as i32 * (card_w + GAP as i32),
                y: top,
                w: card_w,
                h: card_h,
            };
            let view = Rect {
                x: outer.x + 1,
                y: outer.y + CARD_HEAD_H as i32,
                w: (outer.w - 2).max(0),
                h: (outer.h - CARD_HEAD_H as i32 - 1).max(0),
            };
            Card { index, outer, view }
        })
        .collect()
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
}

/// Interactive zoomable subpixel inspector.
pub struct Inspector {
    window: Window,
    config: InspectorConfig,
    text: Option<TextRenderer>,
    fb: Vec<u32>,
    win_w: usize,
    win_h: usize,
    /// Hard minimum — the size we opened at.
    min_win_w: usize,
    min_win_h: usize,

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
        let (screen_w, screen_h) = detect_screen_size();
        let win_w = ((screen_w as f64 * SCREEN_FRACTION) as usize).max(640);
        let win_h = ((screen_h as f64 * SCREEN_FRACTION) as usize).max(400);

        let mut win = Window::new(
            &config.window_title,
            win_w,
            win_h,
            WindowOptions {
                resize: true,
                ..Default::default()
            },
        )?;
        win.set_target_fps(60);

        let text = config.ui_font.as_deref().and_then(TextRenderer::new);

        let mut inspector = Self {
            window: win,
            config,
            text,
            fb: vec![BG; win_w * win_h],
            win_w,
            win_h,
            min_win_w: win_w,
            min_win_h: win_h,
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
        self.window.is_open() && !self.window.is_key_down(Key::Escape)
    }

    /// Process input events and redraw if anything changed.
    pub fn update(&mut self) -> Result<(), minifb::Error> {
        // Clamp to minimum size — prevents Wayland Configure shrinking.
        let (new_w, new_h) = self.window.get_size();
        self.win_w = new_w.max(self.min_win_w);
        self.win_h = new_h.max(self.min_win_h);
        self.fb.resize(self.win_w * self.win_h, BG);

        self.handle_input();
        if self.auto_fit {
            self.fit();
        }

        let state = self.frame_state();
        if self.drawn != Some(state) {
            self.draw();
            self.drawn = Some(state);
        }

        self.window
            .update_with_buffer(&self.fb, self.win_w, self.win_h)?;
        Ok(())
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
        }
    }

    fn cards(&self) -> Vec<Card> {
        layout_cards(self.win_w, self.win_h, self.display_mode)
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
        // ── Scroll-wheel zoom, anchored to the cursor ──────────────────────
        //
        // Which axis carries vertical scroll differs between backends, so the
        // axis with the larger magnitude is used. A positive value zooms out.
        if let Some((scroll_x, scroll_y)) = self.window.get_scroll_wheel() {
            let raw = if scroll_x.abs() >= scroll_y.abs() { scroll_x } else { scroll_y };
            if raw.abs() > 0.05 {
                let (mx, my) = self
                    .window
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

                let factor = if raw > 0.0 { 1.0 / ZOOM_STEP } else { ZOOM_STEP };
                self.zoom_about(factor, rel_x, rel_y);
            }
        }

        // ── Keyboard zoom, about the viewport centre ───────────────────────
        if self.window.is_key_pressed(Key::Equal, KeyRepeat::Yes) {
            self.zoom_about(ZOOM_STEP, 0.0, 0.0);
        }
        if self.window.is_key_pressed(Key::Minus, KeyRepeat::Yes) {
            self.zoom_about(1.0 / ZOOM_STEP, 0.0, 0.0);
        }

        // ── Pan (arrow keys): one logical pixel per press ──────────────────
        for (key, dx, dy) in [
            (Key::Left, -1.0, 0.0),
            (Key::Right, 1.0, 0.0),
            (Key::Up, 0.0, -1.0),
            (Key::Down, 0.0, 1.0),
        ] {
            if self.window.is_key_pressed(key, KeyRepeat::Yes) {
                self.center_x += dx;
                self.center_y += dy;
                self.auto_fit = false;
            }
        }

        // ── Reset ──────────────────────────────────────────────────────────
        if self.window.is_key_pressed(Key::R, KeyRepeat::No) {
            self.auto_fit = true;
        }

        // ── View toggles (each changes what is encoded for display) ────────
        let mut view_changed = false;

        if self.window.is_key_pressed(Key::S, KeyRepeat::No) && self.simulated.is_some() {
            self.simulate_mode = !self.simulate_mode;
            view_changed = true;
        }
        if self.window.is_key_pressed(Key::L, KeyRepeat::No) {
            self.luminance = !self.luminance;
            view_changed = true;
        }
        if self.window.is_key_pressed(Key::E, KeyRepeat::No) {
            self.auto_exposure = !self.auto_exposure;
            view_changed = true;
        }
        if self.window.is_key_pressed(Key::H, KeyRepeat::No) {
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
            if self.window.is_key_pressed(key, KeyRepeat::No) {
                self.display_mode = mode;
            }
        }

        // ── Mouse drag pan: the glyph follows the cursor ───────────────────
        let mouse_down = self.window.get_mouse_down(MouseButton::Left);
        let mouse_pos = self.window.get_mouse_pos(MouseMode::Clamp).unwrap_or((0.0, 0.0));

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

    /// Draw a small labelled box whose right edge is at `right`.
    /// Returns the x-coordinate of its left edge.
    fn chip_right(&mut self, right: i32, y: i32, label: &str, on: bool) -> i32 {
        let w = self.text_width(12, label) + 16;
        let x = right - w;
        let (bg, fg) = if on { (CHIP_ON_BG, CHIP_ON_TEXT) } else { (CHIP_BG, TEXT_DIM) };
        self.fill_rect(Rect { x, y, w, h: 22 }, bg);
        self.text(x + 8, y + 4, 12, fg, right, label);
        x
    }

    // ── Drawing ───────────────────────────────────────────────────────────────

    fn draw(&mut self) {
        self.fb.fill(BG);
        self.draw_header();
        for card in self.cards() {
            self.draw_card(card);
        }
        self.draw_footer();
    }

    /// Title, a sentence saying what is on screen, and the current view state.
    fn draw_header(&mut self) {
        let w = self.win_w as i32;
        self.fill_rect(Rect { x: 0, y: 0, w, h: HEADER_H as i32 }, HEADER_BG);
        self.fill_rect(Rect { x: 0, y: HEADER_H as i32 - 1, w, h: 1 }, BORDER);

        let left = MARGIN as i32 + 2;
        let right = w - MARGIN as i32 - 2;

        // Line 1, right: what is being rendered.
        let subject = self.config.subject.clone();
        let subject_x = (right - self.text_width(13, &subject)).max(left);
        self.text(subject_x, 16, 13, TEXT_DIM, right, &subject);

        // Line 1, left: name and tagline.
        let x = self.text(left, 9, 24, TEXT, subject_x - 16, "KAGE");
        self.text(x + 12, 17, 14, TEXT_DIM, subject_x - 16, "OLED-aware subpixel text rendering inspector");

        // Line 2, right: view state.
        let chip_y = 40;
        let simulated = self.showing_simulation();
        let zoom_label = format!("zoom {:.1}×", self.zoom);
        let mut cx = self.chip_right(right, chip_y, &zoom_label, false);
        if self.heatmap {
            cx = self.chip_right(cx - 6, chip_y, "HEATMAP", true);
        }
        if self.luminance {
            cx = self.chip_right(cx - 6, chip_y, "LUMINANCE", true);
        }
        if simulated && !self.auto_exposure {
            cx = self.chip_right(cx - 6, chip_y, "CLIPPED", true);
        }
        cx = if simulated {
            self.chip_right(cx - 6, chip_y, "SIMULATED PANEL", true)
        } else {
            self.chip_right(cx - 6, chip_y, "RAW RENDER", false)
        };

        // Line 2, left: what the viewer is looking at.
        let layout = &self.config.layout_name;
        let line = if simulated {
            let note = self.config.simulation_note.as_deref().unwrap_or("");
            format!(
                "The same glyph as a {layout} panel would emit it: every subpixel drawn where it physically sits. {note}"
            )
        } else if self.simulated.is_some() {
            format!(
                "One glyph, antialiased three ways for a {layout} display. Shown: the renderer's output, one RGB value per pixel. Press S to see it on the physical panel."
            )
        } else {
            format!(
                "One glyph, antialiased three ways for a {layout} display. Shown: the renderer's output, one RGB value per pixel."
            )
        };
        self.text(left, 44, 13, TEXT, cx - 16, &line);
    }

    /// One panel: accent bar, title, description, metrics, viewport.
    fn draw_card(&mut self, card: Card) {
        let (name, accent, description) = PANELS[card.index];
        let o = card.outer;

        self.fill_rect(o, BORDER);
        self.fill_rect(Rect { x: o.x + 1, y: o.y + 1, w: o.w - 2, h: o.h - 2 }, CARD_BG);
        self.fill_rect(Rect { x: o.x, y: o.y, w: o.w, h: 3 }, accent);

        let left = o.x + PAD;
        let right = o.right() - PAD;
        let name_end = self.text(left, o.y + 13, 17, TEXT, right, name);

        // Metrics, right-aligned on the title line, when they fit.
        if self.showing_simulation() {
            if let Some(metrics) = self.config.metrics {
                let m = metrics[card.index];
                let label = format!(
                    "luminance error {:.3}   colour fringing {:.3}",
                    m.luminance_error, m.chroma
                );
                let x = right - self.text_width(12, &label);
                if x > name_end + 16 {
                    self.text(x, o.y + 17, 12, TEXT_DIM, right, &label);
                }
            }
        }

        self.text(left, o.y + 38, 12, TEXT_DIM, right, description);
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

    /// Key hints along the bottom edge.
    fn draw_footer(&mut self) {
        let w = self.win_w as i32;
        let top = self.win_h as i32 - FOOTER_H as i32;
        self.fill_rect(Rect { x: 0, y: top, w, h: FOOTER_H as i32 }, HEADER_BG);
        self.fill_rect(Rect { x: 0, y: top, w, h: 1 }, BORDER);

        let simulate_hint = if self.simulated.is_some() { "simulated panel" } else { "simulation off" };
        let hints = [
            ("Scroll", "zoom"),
            ("Drag", "pan"),
            ("R", "reset"),
            ("1 2 3", "one panel"),
            ("A", "all"),
            ("S", simulate_hint),
            ("L", "luminance"),
            ("E", "exposure"),
            ("H", "heatmap"),
            ("Esc", "quit"),
        ];

        let right = w - MARGIN as i32;
        let mut x = MARGIN as i32;
        let y = top + 9;
        for (key, label) in hints {
            let key_w = self.text_width(12, key) + 12;
            let label_w = self.text_width(12, label);
            if x + key_w + 6 + label_w > right {
                break;
            }
            self.fill_rect(Rect { x, y, w: key_w, h: 20 }, CHIP_BG);
            self.text(x + 6, y + 3, 12, TEXT, right, key);
            x = self.text(x + key_w + 6, y + 3, 12, TEXT_DIM, right, label) + 18;
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
        let cards = layout_cards(1280, 800, DisplayMode::SideBySide);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards.iter().map(|c| c.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        for pair in cards.windows(2) {
            assert_eq!(pair[0].outer.w, pair[1].outer.w);
            assert_eq!(pair[1].outer.x - pair[0].outer.right(), GAP as i32);
        }
        let last = cards[2];
        assert!(last.outer.right() <= 1280 - MARGIN as i32);
        assert!(last.outer.y + last.outer.h <= 800 - FOOTER_H as i32);
        assert!(last.view.y >= last.outer.y + CARD_HEAD_H as i32);
    }

    #[test]
    fn single_mode_shows_the_selected_panel_full_width() {
        let cards = layout_cards(1280, 800, DisplayMode::OledAware);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].index, 2);
        assert_eq!(cards[0].outer.w, 1280 - 2 * MARGIN as i32);
    }

    #[test]
    fn layout_survives_a_tiny_window() {
        for card in layout_cards(10, 10, DisplayMode::SideBySide) {
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
}
