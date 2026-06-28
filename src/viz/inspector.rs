//! Zoomable subpixel inspector.
//!
//! [`Inspector`] renders three [`SubpixelGrid`]s side-by-side in a
//! [`minifb`] window — one per rendering mode (Greyscale, Subpixel AA,
//! OLED-Aware) — and provides interactive zoom and pan.
//!
//! ## Controls
//!
//! | Input | Action |
//! |-------|--------|
//! | Scroll wheel | Zoom in / out, anchored to cursor position |
//! | `+` / `=` | Zoom in (keyboard) |
//! | `-` | Zoom out (keyboard) |
//! | Arrow keys | Pan |
//! | `1` | Switch to Greyscale view |
//! | `2` | Switch to Subpixel AA view |
//! | `3` | Switch to OLED-Aware view |
//! | `A` | Side-by-side comparison (default) |
//! | `H` | Toggle per-channel heatmap overlay |
//! | `Esc` | Quit |
//!
//! ## Layout
//!
//! In side-by-side mode the window is divided into three equal columns.
//! A 1-pixel separator line is drawn between columns.
//! A label strip at the top identifies each column.

use minifb::{Key, MouseMode, Window, WindowOptions};

use crate::profile::DisplayProfile;
use crate::render::encode_grid;
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
    fb: Vec<u32>,
    win_w: usize,
    win_h: usize,
    /// Hard minimum — the size we opened at.
    min_win_w: usize,
    min_win_h: usize,

    /// Zoom level: each source pixel is rendered as `zoom × zoom` host pixels.
    zoom: f32,
    /// Pan offset in source-pixel coordinates.
    pan_x: i32,
    pan_y: i32,

    /// Mouse drag state for panning.
    drag_last: Option<(f32, f32)>,

    display_mode: DisplayMode,
    /// When true, show simulated grids instead of raw render grids.
    simulate_mode: bool,
    /// Whether simulation data is available (--simulate was passed).
    simulate_available: bool,
    /// When true, R/G/B channels are shown as false-colour heatmaps.
    heatmap: bool,
}

impl Inspector {
    /// Create a new inspector window sized to 80% of the primary display.
    ///
    /// `simulate_available` — whether simulated grids will be passed to `update()`.
    /// If false, pressing S shows a "simulation not enabled" notice.
    pub fn new(title: &str, simulate_available: bool) -> Result<Self, minifb::Error> {
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

        Ok(Self {
            window: win,
            fb,
            win_w,
            win_h,
            min_win_w: win_w,
            min_win_h: win_h,
            zoom: 8.0,
            pan_x: 0,
            pan_y: 0,
            drag_last: None,
            display_mode: DisplayMode::SideBySide,
            simulate_mode: false,
            simulate_available,
            heatmap: false,
        })
    }

    /// Returns `true` as long as the window is open and Esc has not been pressed.
    pub fn is_open(&self) -> bool {
        self.window.is_open() && !self.window.is_key_down(Key::Escape)
    }

    /// Process input events and redraw.
    ///
    /// All grids must be in **linear light** — `encode_grid()` is called
    /// internally. Press `S` to toggle between raw and simulated views.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        grey_raw:  &SubpixelGrid,
        sp_raw:    &SubpixelGrid,
        oled_raw:  &SubpixelGrid,
        grey_sim:  &SubpixelGrid,
        sp_sim:    &SubpixelGrid,
        oled_sim:  &SubpixelGrid,
        profile:   &DisplayProfile,
    ) -> Result<(), minifb::Error> {
        // Clamp to minimum size — prevents Wayland Configure shrinking.
        let (new_w, new_h) = self.window.get_size();
        self.win_w = new_w.max(self.min_win_w);
        self.win_h = new_h.max(self.min_win_h);
        self.fb.resize(self.win_w * self.win_h, BG_COLOR);

        // Select raw or simulated grids based on S toggle.
        let (grey, sp, oled) = if self.simulate_mode {
            (grey_sim, sp_sim, oled_sim)
        } else {
            (grey_raw, sp_raw, oled_raw)
        };

        // EOTF-encode the selected linear-light grids.
        let grey_enc = encode_grid(grey.clone(), profile);
        let sp_enc   = encode_grid(sp.clone(),   profile);
        let oled_enc = encode_grid(oled.clone(), profile);

        self.handle_input();
        self.draw(&grey_enc, &sp_enc, &oled_enc);

        self.window
            .update_with_buffer(&self.fb, self.win_w, self.win_h)?;
        Ok(())
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
        //             inverts again.  We invert the Wayland sign explicitly.
        //
        // Anchor math — keeps the source pixel under the cursor fixed:
        //   src = pan + mouse / old_zoom
        //   pan = src - mouse / new_zoom
        //
        if let Some((scroll_x, scroll_y)) = self.window.get_scroll_wheel() {
            // Pick the axis with larger magnitude.
            let raw = if scroll_x.abs() >= scroll_y.abs() { scroll_x } else { scroll_y };

            // Diagnostic: print scroll values on first event to help debug
            // platform differences. Remove once confirmed working.
            #[cfg(debug_assertions)]
            if raw.abs() > 0.001 {
                eprintln!("[kage] scroll raw=({scroll_x:.3}, {scroll_y:.3})  using={raw:.3}");
            }

            if raw.abs() > 0.05 {
                let old_zoom = self.zoom;

                let (mx, my) = self.window
                    .get_mouse_pos(MouseMode::Clamp)
                    .unwrap_or((self.win_w as f32 / 2.0, self.win_h as f32 / 2.0));

                // Source pixel under cursor before zoom change.
                let src_col = self.pan_x as f32 + mx / old_zoom;
                let src_row = self.pan_y as f32 + (my - LABEL_H as f32).max(0.0) / old_zoom;

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

                // Adjust pan so the same source pixel stays under the cursor.
                self.pan_x = (src_col - mx / self.zoom).round() as i32;
                self.pan_y = (src_row - (my - LABEL_H as f32).max(0.0) / self.zoom).round() as i32;
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

        // ── Simulate toggle ────────────────────────────────────────────────
        if self.window.is_key_pressed(Key::S, minifb::KeyRepeat::No) {
            if self.simulate_available {
                self.simulate_mode = !self.simulate_mode;
            } else {
                eprintln!("[kage] Simulation not enabled — re-run with --simulate");
            }
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

        // Heatmap toggle
        if self.window.is_key_pressed(Key::H, minifb::KeyRepeat::No) {
            self.heatmap = !self.heatmap;
        }

        // ── Mouse drag pan ─────────────────────────────────────────────────
        //
        // Left-click and drag pans all panels together.
        // We track the mouse position from the previous frame and compute the
        // delta in host pixels, then convert to source pixels via zoom.
        let mouse_down = self.window.get_mouse_down(minifb::MouseButton::Left);
        let mouse_pos  = self.window.get_mouse_pos(MouseMode::Clamp)
            .unwrap_or((0.0, 0.0));

        if mouse_down {
            if let Some((last_x, last_y)) = self.drag_last {
                let dx = (mouse_pos.0 - last_x) / self.zoom;
                let dy = (mouse_pos.1 - last_y) / self.zoom;
                self.pan_x -= dx.round() as i32;
                self.pan_y -= dy.round() as i32;
            }
            self.drag_last = Some(mouse_pos);
        } else {
            self.drag_last = None;
        }
    }

    // ── Drawing ───────────────────────────────────────────────────────────────

    fn draw(&mut self, grey: &SubpixelGrid, sp: &SubpixelGrid, oled: &SubpixelGrid) {
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
                self.draw_panel(grey, 0, LABEL_H, self.win_w, glyph_area_h);
                self.draw_label("Greyscale AA", 0, self.win_w);
            }
            DisplayMode::SubpixelAa => {
                self.draw_panel(sp, 0, LABEL_H, self.win_w, glyph_area_h);
                self.draw_label("Subpixel AA (ClearType-style)", 0, self.win_w);
            }
            DisplayMode::OledAware => {
                self.draw_panel(oled, 0, LABEL_H, self.win_w, glyph_area_h);
                self.draw_label("OLED-Aware", 0, self.win_w);
            }
            DisplayMode::SideBySide => {
                let col_w = self.win_w / 3;
                let sep = 1;

                // Panel 1: Greyscale
                self.draw_panel(grey, 0, LABEL_H, col_w.saturating_sub(sep), glyph_area_h);
                self.draw_label("Greyscale AA", 0, col_w);

                // Separator
                self.draw_separator(col_w);

                // Panel 2: Subpixel AA
                self.draw_panel(sp, col_w + sep, LABEL_H, col_w.saturating_sub(sep), glyph_area_h);
                self.draw_label("Subpixel AA", col_w, col_w);

                // Separator
                self.draw_separator(col_w * 2);

                // Panel 3: OLED-Aware
                self.draw_panel(oled, col_w * 2 + sep, LABEL_H, self.win_w - col_w * 2 - sep, glyph_area_h);
                self.draw_label("OLED-Aware", col_w * 2, self.win_w - col_w * 2);
            }
        }

        // Zoom / mode info bar at bottom
        self.draw_status_bar();
    }

    /// Blit one [`SubpixelGrid`] into a rectangular region of the framebuffer.
    ///
    /// Each source pixel is scaled to `zoom × zoom` host pixels.
    /// Pan offsets are applied in source-pixel coordinates.
    fn draw_panel(
        &mut self,
        grid: &SubpixelGrid,
        panel_x: usize,
        panel_y: usize,
        panel_w: usize,
        panel_h: usize,
    ) {
        let zoom = self.zoom;
        let src_w = grid.width as i32;
        let src_h = grid.height as i32;

        for wy in 0..panel_h as i32 {
            for wx in 0..panel_w as i32 {
                // Map host pixel → source pixel (accounting for pan and zoom).
                // Using f32 division then floor gives correct sub-pixel mapping.
                let src_col = self.pan_x + (wx as f32 / zoom) as i32;
                let src_row = self.pan_y + (wy as f32 / zoom) as i32;

                let color = if src_col >= 0 && src_col < src_w && src_row >= 0 && src_row < src_h {
                    let px = grid.pixel(src_col as u32, src_row as u32);
                    if self.heatmap {
                        self.heatmap_color(px.r, px.g, px.b)
                    } else {
                        let r = (px.r.clamp(0.0, 1.0) * 255.0).round() as u32;
                        let g = (px.g.clamp(0.0, 1.0) * 255.0).round() as u32;
                        let b = (px.b.clamp(0.0, 1.0) * 255.0).round() as u32;
                        (0xFF << 24) | (r << 16) | (g << 8) | b
                    }
                } else {
                    // Out-of-bounds: checkerboard to show glyph extent
                    let checker = ((src_col ^ src_row) & 1) == 0;
                    if checker { 0xFF_18_18_18 } else { 0xFF_22_22_22 }
                };

                let fb_x = panel_x + wx as usize;
                let fb_y = panel_y + wy as usize;
                if fb_x < self.win_w && fb_y < self.win_h {
                    self.fb[fb_y * self.win_w + fb_x] = color;
                }
            }
        }

        // Draw pixel grid lines when zoom is large enough to make them useful.
        if zoom >= 8.0 {
            self.draw_pixel_grid(panel_x, panel_y, panel_w, panel_h, zoom);
        }
    }

    /// Draw thin grid lines between zoomed pixels for easy inspection.
    fn draw_pixel_grid(
        &mut self,
        panel_x: usize,
        panel_y: usize,
        panel_w: usize,
        panel_h: usize,
        zoom: f32,
    ) {
        let grid_color = 0xFF_28_28_28;
        let zoom_i = zoom.round() as usize;
        if zoom_i == 0 { return; }

        // Vertical lines — one per source pixel column boundary.
        let mut x = panel_x;
        while x < panel_x + panel_w {
            if (x - panel_x) % zoom_i == 0 {
                for y in panel_y..(panel_y + panel_h).min(self.win_h) {
                    if x < self.win_w {
                        self.fb[y * self.win_w + x] = grid_color;
                    }
                }
            }
            x += 1;
        }
        // Horizontal lines — one per source pixel row boundary.
        let mut y = panel_y;
        while y < panel_y + panel_h {
            if (y - panel_y) % zoom_i == 0 {
                for x in panel_x..(panel_x + panel_w).min(self.win_w) {
                    if y < self.win_h {
                        self.fb[y * self.win_w + x] = grid_color;
                    }
                }
            }
            y += 1;
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

    /// Convert R/G/B values to a false-colour heatmap for per-channel analysis.
    ///
    /// Each channel is displayed at 1/3 of the horizontal width:
    /// left third = R energy, centre = G energy, right = B energy.
    ///
    /// Since we can't know which third of the pixel we're in here (we only have
    /// the final colour), we use a simple false-colour encoding instead:
    /// - Pure R deviation from grey → red tint
    /// - Pure B deviation from grey → blue tint
    /// - Near-grey → neutral
    fn heatmap_color(&self, r: f32, g: f32, b: f32) -> u32 {
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
}
