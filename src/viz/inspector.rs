//! Zoomable subpixel inspector.
//!
//! [`Inspector`] renders three [`SubpixelGrid`]s side-by-side in a
//! [`minifb`] window — one per rendering mode (Greyscale, Subpixel AA,
//! OLED-Aware) — and provides interactive zoom and pan.
//!
//! It holds two [`ViewSet`]s: the raw renderer output (one sample per logical
//! pixel) and, optionally, the simulated panel image (oversampled, with each
//! emitter at its physical position). Zoom and pan are always expressed in
//! logical pixels, so toggling between the two keeps the glyph in place.
//!
//! ## Controls
//!
//! | Input | Action |
//! |-------|--------|
//! | Scroll wheel | Zoom in / out, anchored to cursor position |
//! | `+` / `=` | Zoom in (keyboard) |
//! | `-` | Zoom out (keyboard) |
//! | Click + drag | Pan |
//! | Arrow keys | Pan by one pixel |
//! | `1` | Switch to Greyscale view |
//! | `2` | Switch to Subpixel AA view |
//! | `3` | Switch to OLED-Aware view |
//! | `A` | Side-by-side comparison (default) |
//! | `S` | Toggle raw ↔ simulated panel view |
//! | `L` | Toggle luminance-only view |
//! | `E` | Toggle auto-exposure ↔ clipping (simulated view) |
//! | `H` | Toggle per-channel heatmap overlay |
//! | `Esc` | Quit |
//!
//! The window title shows which of these are active.
//!
//! ## Layout
//!
//! In side-by-side mode the window is divided into three equal columns.
//! A 1-pixel separator line is drawn between columns.
//! A label strip at the top identifies each column.

use minifb::{Key, MouseMode, Window, WindowOptions};

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
/// Height of the label strip at the top of the window, in host pixels.
const LABEL_H: usize = 20;
/// Colour of the separator line between panels (dark grey).
const SEPARATOR_COLOR: u32 = 0xFF_33_33_33;
/// Background colour (near-black).
const BG_COLOR: u32 = 0xFF_10_10_10;
/// Label strip background.
const LABEL_BG: u32 = 0xFF_1A_1A_2E;
/// Label text colour — reserved for Phase 4 when real text rendering lands in the inspector.
#[allow(dead_code)]
const LABEL_FG: u32 = 0xFF_E0_E0_E0;

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

/// Interactive zoomable subpixel inspector.
pub struct Inspector {
    window: Window,
    title: String,
    fb: Vec<u32>,
    win_w: usize,
    win_h: usize,
    /// Hard minimum — the size we opened at.
    min_win_w: usize,
    min_win_h: usize,

    /// Renderer output, linear light.
    raw: ViewSet,
    /// Simulated panel image, linear light (`None` without `--simulate`).
    simulated: Option<ViewSet>,
    profile: DisplayProfile,
    /// Display-ready (EOTF-encoded) grids for the current view settings,
    /// in panel order: greyscale, subpixel AA, OLED-aware.
    shown: [SubpixelGrid; 3],
    /// Samples per logical pixel of `shown`.
    shown_scale: u32,

    /// Zoom level: each logical pixel is rendered as `zoom × zoom` host pixels.
    zoom: f32,
    /// Pan offset in logical-pixel coordinates.
    pan_x: i32,
    pan_y: i32,

    /// Mouse drag state for panning.
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
}

impl Inspector {
    /// Create a new inspector window sized to 80% of the primary display.
    ///
    /// `raw` is the renderer output and `simulated` the optional panel
    /// simulation; both must be in **linear light**. EOTF encoding with
    /// `profile` happens inside the inspector, once per view change.
    pub fn new(
        title: &str,
        raw: ViewSet,
        simulated: Option<ViewSet>,
        profile: DisplayProfile,
    ) -> Result<Self, minifb::Error> {
        let (screen_w, screen_h) = detect_screen_size();
        let win_w = ((screen_w as f64 * SCREEN_FRACTION) as usize).max(640);
        let win_h = ((screen_h as f64 * SCREEN_FRACTION) as usize).max(400);

        let mut win = Window::new(
            title,
            win_w,
            win_h,
            WindowOptions {
                resize: true,
                ..Default::default()
            },
        )?;
        win.set_target_fps(60);

        let fb = vec![BG_COLOR; win_w * win_h];

        let mut inspector = Self {
            window: win,
            title: title.to_owned(),
            fb,
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
            pan_x: 0,
            pan_y: 0,
            drag_last: None,
            display_mode: DisplayMode::SideBySide,
            simulate_mode: false,
            luminance: false,
            auto_exposure: true,
            heatmap: false,
        };
        inspector.rebuild_view();
        Ok(inspector)
    }

    /// Returns `true` as long as the window is open and Esc has not been pressed.
    pub fn is_open(&self) -> bool {
        self.window.is_open() && !self.window.is_key_down(Key::Escape)
    }

    /// Process input events and redraw.
    pub fn update(&mut self) -> Result<(), minifb::Error> {
        // Clamp to minimum size — prevents Wayland Configure shrinking.
        let (new_w, new_h) = self.window.get_size();
        self.win_w = new_w.max(self.min_win_w);
        self.win_h = new_h.max(self.min_win_h);
        self.fb.resize(self.win_w * self.win_h, BG_COLOR);

        self.handle_input();
        self.draw();

        self.window
            .update_with_buffer(&self.fb, self.win_w, self.win_h)?;
        Ok(())
    }

    // ── View pipeline ─────────────────────────────────────────────────────────

    /// Recompute the display-ready grids for the current view settings.
    ///
    /// Everything up to the final step stays in linear light:
    /// select raw / simulated → (luminance) → (exposure) → EOTF encode.
    fn rebuild_view(&mut self) {
        let simulated = self.simulate_mode && self.simulated.is_some();
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

        let mut title = self.title.clone();
        title.push_str(if simulated { " — simulated panel" } else { " — raw render" });
        if simulated {
            title.push_str(if self.auto_exposure { " (auto-exposed)" } else { " (clipped)" });
        }
        if self.luminance {
            title.push_str(" — luminance");
        }
        if self.heatmap {
            title.push_str(" — heatmap");
        }
        self.window.set_title(&title);
    }

    /// Left edge (host x) of the panel under host x-coordinate `mx`.
    fn panel_origin_x(&self, mx: f32) -> f32 {
        if self.display_mode != DisplayMode::SideBySide {
            return 0.0;
        }
        let col_w = (self.win_w / 3).max(1);
        let idx = ((mx.max(0.0) as usize) / col_w).min(2);
        // Panels after the first start one pixel in, past the separator.
        (idx * col_w + usize::from(idx > 0)) as f32
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    fn handle_input(&mut self) {
        // ── Scroll-wheel zoom, anchored to cursor position ─────────────────
        //
        // Wayland and X11 differ in scroll axis sign and which axis carries
        // vertical scroll data.  We read both axes and use whichever has the
        // larger magnitude, then treat positive = zoom in on both platforms.
        //
        // On X11:    Button4 (up) → scroll_y = +1.0, Button5 (down) → -1.0
        // On Wayland: VerticalScroll positive = scroll DOWN (inverted vs X11)
        //             Additionally, natural scrolling (common on GNOME/Fedora)
        //             inverts again.
        //
        // Anchor math — keeps the logical pixel under the cursor fixed, with
        // the cursor position measured from the origin of the panel it is in:
        //   src = pan + rel / old_zoom
        //   pan = src - rel / new_zoom
        //
        if let Some((scroll_x, scroll_y)) = self.window.get_scroll_wheel() {
            // Pick the axis with larger magnitude.
            let raw = if scroll_x.abs() >= scroll_y.abs() { scroll_x } else { scroll_y };

            // Diagnostic: print scroll values to help debug platform
            // differences. Remove once confirmed working.
            #[cfg(debug_assertions)]
            if raw.abs() > 0.001 {
                eprintln!("[kage] scroll raw=({scroll_x:.3}, {scroll_y:.3})  using={raw:.3}");
            }

            if raw.abs() > 0.05 {
                let old_zoom = self.zoom;

                let (mx, my) = self.window
                    .get_mouse_pos(MouseMode::Clamp)
                    .unwrap_or((self.win_w as f32 / 2.0, self.win_h as f32 / 2.0));

                let rel_x = mx - self.panel_origin_x(mx);
                let rel_y = (my - LABEL_H as f32).max(0.0);

                // Logical pixel under cursor before zoom change.
                let src_col = self.pan_x as f32 + rel_x / old_zoom;
                let src_row = self.pan_y as f32 + rel_y / old_zoom;

                // Multiplicative zoom: 1.2× per tick.
                // Positive raw = scroll up on X11 → zoom in.
                // On Wayland, if the sign is wrong the diagnostic above will show it —
                // invert `raw` here if needed based on the printed values.
                const ZOOM_FACTOR: f32 = 1.2;
                if raw > 0.0 {
                    self.zoom = (self.zoom * ZOOM_FACTOR).min(64.0);
                } else {
                    self.zoom = (self.zoom / ZOOM_FACTOR).max(1.0);
                }

                // Adjust pan so the same logical pixel stays under the cursor.
                self.pan_x = (src_col - rel_x / self.zoom).round() as i32;
                self.pan_y = (src_row - rel_y / self.zoom).round() as i32;
            }
        }

        // ── Keyboard zoom (always works, use this if scroll is misbehaving) ─
        if self.window.is_key_pressed(Key::Equal, minifb::KeyRepeat::Yes) {
            self.zoom = (self.zoom * 1.2).min(64.0);
        }
        if self.window.is_key_pressed(Key::Minus, minifb::KeyRepeat::Yes) {
            self.zoom = (self.zoom / 1.2).max(1.0);
        }

        // ── Pan (arrow keys) ───────────────────────────────────────────────
        let pan_step = 1i32;
        if self.window.is_key_pressed(Key::Left, minifb::KeyRepeat::Yes) {
            self.pan_x -= pan_step;
        }
        if self.window.is_key_pressed(Key::Right, minifb::KeyRepeat::Yes) {
            self.pan_x += pan_step;
        }
        if self.window.is_key_pressed(Key::Up, minifb::KeyRepeat::Yes) {
            self.pan_y -= pan_step;
        }
        if self.window.is_key_pressed(Key::Down, minifb::KeyRepeat::Yes) {
            self.pan_y += pan_step;
        }

        // ── View toggles (each changes what is encoded for display) ────────
        let mut view_changed = false;

        if self.window.is_key_pressed(Key::S, minifb::KeyRepeat::No) {
            if self.simulated.is_some() {
                self.simulate_mode = !self.simulate_mode;
                view_changed = true;
            } else {
                eprintln!("[kage] Simulation not enabled — re-run with --simulate");
            }
        }
        if self.window.is_key_pressed(Key::L, minifb::KeyRepeat::No) {
            self.luminance = !self.luminance;
            view_changed = true;
        }
        if self.window.is_key_pressed(Key::E, minifb::KeyRepeat::No) {
            self.auto_exposure = !self.auto_exposure;
            view_changed = true;
        }
        if self.window.is_key_pressed(Key::H, minifb::KeyRepeat::No) {
            self.heatmap = !self.heatmap;
            view_changed = true;
        }

        if view_changed {
            self.rebuild_view();
        }

        // Display mode
        if self.window.is_key_pressed(Key::Key1, minifb::KeyRepeat::No) {
            self.display_mode = DisplayMode::Greyscale;
        }
        if self.window.is_key_pressed(Key::Key2, minifb::KeyRepeat::No) {
            self.display_mode = DisplayMode::SubpixelAa;
        }
        if self.window.is_key_pressed(Key::Key3, minifb::KeyRepeat::No) {
            self.display_mode = DisplayMode::OledAware;
        }
        if self.window.is_key_pressed(Key::A, minifb::KeyRepeat::No) {
            self.display_mode = DisplayMode::SideBySide;
        }

        // ── Mouse drag pan ─────────────────────────────────────────────────
        //
        // Left-click and drag pans all panels together. Pan is in whole
        // logical pixels, so only the whole-pixel part of the drag is
        // consumed each frame; the remainder stays in `drag_last` and
        // accumulates, which keeps slow drags from being rounded away.
        let mouse_down = self.window.get_mouse_down(minifb::MouseButton::Left);
        let mouse_pos  = self.window.get_mouse_pos(MouseMode::Clamp)
            .unwrap_or((0.0, 0.0));

        if mouse_down {
            match self.drag_last {
                Some((last_x, last_y)) => {
                    let dx = ((mouse_pos.0 - last_x) / self.zoom).trunc();
                    let dy = ((mouse_pos.1 - last_y) / self.zoom).trunc();
                    self.pan_x -= dx as i32;
                    self.pan_y -= dy as i32;
                    self.drag_last = Some((last_x + dx * self.zoom, last_y + dy * self.zoom));
                }
                None => self.drag_last = Some(mouse_pos),
            }
        } else {
            self.drag_last = None;
        }
    }

    // ── Drawing ───────────────────────────────────────────────────────────────

    fn draw(&mut self) {
        // Clear
        for px in self.fb.iter_mut() {
            *px = BG_COLOR;
        }

        // Draw label strip
        for x in 0..self.win_w {
            for y in 0..LABEL_H.min(self.win_h) {
                self.fb[y * self.win_w + x] = LABEL_BG;
            }
        }

        let glyph_area_h = self.win_h.saturating_sub(LABEL_H);

        match self.display_mode {
            DisplayMode::Greyscale => {
                self.draw_panel(0, 0, LABEL_H, self.win_w, glyph_area_h);
                self.draw_label("Greyscale AA", 0, self.win_w);
            }
            DisplayMode::SubpixelAa => {
                self.draw_panel(1, 0, LABEL_H, self.win_w, glyph_area_h);
                self.draw_label("Subpixel AA (ClearType-style)", 0, self.win_w);
            }
            DisplayMode::OledAware => {
                self.draw_panel(2, 0, LABEL_H, self.win_w, glyph_area_h);
                self.draw_label("OLED-Aware", 0, self.win_w);
            }
            DisplayMode::SideBySide => {
                let col_w = self.win_w / 3;
                let sep = 1;

                // Panel 1: Greyscale
                self.draw_panel(0, 0, LABEL_H, col_w.saturating_sub(sep), glyph_area_h);
                self.draw_label("Greyscale AA", 0, col_w);

                // Separator
                self.draw_separator(col_w);

                // Panel 2: Subpixel AA
                self.draw_panel(1, col_w + sep, LABEL_H, col_w.saturating_sub(sep), glyph_area_h);
                self.draw_label("Subpixel AA", col_w, col_w);

                // Separator
                self.draw_separator(col_w * 2);

                // Panel 3: OLED-Aware
                self.draw_panel(2, col_w * 2 + sep, LABEL_H, self.win_w - col_w * 2 - sep, glyph_area_h);
                self.draw_label("OLED-Aware", col_w * 2, self.win_w - col_w * 2);
            }
        }

        // Zoom / mode info bar at bottom
        self.draw_status_bar();
    }

    /// Blit `self.shown[index]` into a rectangular region of the framebuffer.
    ///
    /// Each logical pixel is scaled to `zoom × zoom` host pixels, and is made
    /// of `shown_scale × shown_scale` grid samples. Pan offsets are applied in
    /// logical-pixel coordinates.
    fn draw_panel(
        &mut self,
        index: usize,
        panel_x: usize,
        panel_y: usize,
        panel_w: usize,
        panel_h: usize,
    ) {
        let zoom = self.zoom;
        let scale = self.shown_scale as f32;
        let grid = &self.shown[index];
        let src_w = grid.width as i32;
        let src_h = grid.height as i32;

        for wy in 0..panel_h as i32 {
            let fb_y = panel_y + wy as usize;
            if fb_y >= self.win_h {
                break;
            }
            // Host pixel → logical pixel (pan + zoom) → grid sample (scale).
            let ly = self.pan_y as f32 + wy as f32 / zoom;
            let src_row = (ly * scale).floor() as i32;

            for wx in 0..panel_w as i32 {
                let fb_x = panel_x + wx as usize;
                if fb_x >= self.win_w {
                    break;
                }
                let lx = self.pan_x as f32 + wx as f32 / zoom;
                let src_col = (lx * scale).floor() as i32;

                let color = if src_col >= 0 && src_col < src_w && src_row >= 0 && src_row < src_h {
                    let px = grid.pixel(src_col as u32, src_row as u32);
                    if self.heatmap {
                        heatmap_color(px.r, px.g, px.b)
                    } else {
                        let r = (px.r.clamp(0.0, 1.0) * 255.0).round() as u32;
                        let g = (px.g.clamp(0.0, 1.0) * 255.0).round() as u32;
                        let b = (px.b.clamp(0.0, 1.0) * 255.0).round() as u32;
                        (0xFF << 24) | (r << 16) | (g << 8) | b
                    }
                } else {
                    // Out-of-bounds: per-logical-pixel checkerboard to show glyph extent
                    let checker = ((lx.floor() as i32 ^ ly.floor() as i32) & 1) == 0;
                    if checker { 0xFF_18_18_18 } else { 0xFF_22_22_22 }
                };

                self.fb[fb_y * self.win_w + fb_x] = color;
            }
        }

        // Draw pixel grid lines when zoom is large enough to make them useful.
        if zoom >= 8.0 {
            self.draw_pixel_grid(panel_x, panel_y, panel_w, panel_h, zoom);
        }
    }

    /// Draw thin grid lines on logical-pixel boundaries for easy inspection.
    ///
    /// A line is drawn on the first host pixel of each logical pixel, using
    /// the same host → logical mapping as [`Self::draw_panel`], so the lines
    /// stay on the boundaries at fractional zoom levels too.
    fn draw_pixel_grid(
        &mut self,
        panel_x: usize,
        panel_y: usize,
        panel_w: usize,
        panel_h: usize,
        zoom: f32,
    ) {
        let grid_color = 0xFF_28_28_28;
        let starts_pixel = |w: usize| {
            w == 0 || (w as f32 / zoom).floor() != ((w - 1) as f32 / zoom).floor()
        };
        let x_end = (panel_x + panel_w).min(self.win_w);
        let y_end = (panel_y + panel_h).min(self.win_h);

        // Vertical lines — one per logical pixel column boundary.
        for x in panel_x..x_end {
            if starts_pixel(x - panel_x) {
                for y in panel_y..y_end {
                    self.fb[y * self.win_w + x] = grid_color;
                }
            }
        }
        // Horizontal lines — one per logical pixel row boundary.
        for y in panel_y..y_end {
            if starts_pixel(y - panel_y) {
                for x in panel_x..x_end {
                    self.fb[y * self.win_w + x] = grid_color;
                }
            }
        }
    }

    /// Draw a vertical separator at `x`.
    fn draw_separator(&mut self, x: usize) {
        for y in 0..self.win_h {
            if x < self.win_w {
                self.fb[y * self.win_w + x] = SEPARATOR_COLOR;
            }
        }
    }

    /// Draw a simple coloured label bar along the bottom of the label strip.
    /// Colour coding: blue = greyscale, orange = subpixel AA, green = OLED-aware.
    /// A brighter shade indicates simulated mode is active.
    fn draw_label(&mut self, label: &str, x_start: usize, width: usize) {
        let accent = match label {
            t if t.contains("Grey") => {
                if self.simulate_mode { 0xFF_6A_B0_F5 } else { 0xFF_4A_90_D9 }
            }
            t if t.contains("Subpixel") || t.contains("Clear") => {
                if self.simulate_mode { 0xFF_FF_A0_5A } else { 0xFF_E8_7B_3A }
            }
            _ => {
                if self.simulate_mode { 0xFF_7A_E9_7A } else { 0xFF_5A_C9_5A }
            }
        };
        let accent_y = LABEL_H.saturating_sub(3);
        for dy in 0..3 {
            let y = accent_y + dy;
            for x in x_start..(x_start + width).min(self.win_w) {
                if y < self.win_h {
                    self.fb[y * self.win_w + x] = accent;
                }
            }
        }
    }

    /// Draw a one-line status bar at the very bottom of the window.
    fn draw_status_bar(&mut self) {
        if self.win_h < 2 { return; }
        let bar_color = 0xFF_0A_0A_16;
        let y = self.win_h - 1;
        for x in 0..self.win_w {
            self.fb[y * self.win_w + x] = bar_color;
        }
    }
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
}
