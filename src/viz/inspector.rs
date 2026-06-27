//! Zoomable subpixel inspector.
//!
//! [`Inspector`] renders three [`SubpixelGrid`]s side-by-side in a
//! [`minifb`] window — one per rendering mode (Greyscale, Subpixel AA,
//! OLED-Aware) — and provides interactive zoom and pan.
//!
//! ## Controls
//!
//! | Key | Action |
//! |-----|--------|
//! | `+` / `=` | Zoom in |
//! | `-` | Zoom out |
//! | Arrow keys | Pan |
//! | `1` | Switch to Greyscale view |
//! | `2` | Switch to Subpixel AA view |
//! | `3` | Switch to OLED-Aware view |
//! | `A` | Cycle through all three (side-by-side) |
//! | `H` | Toggle per-channel heatmap overlay |
//! | `Esc` | Quit |
//!
//! ## Layout
//!
//! In side-by-side mode the window is divided into three equal columns.
//! A 1-pixel separator line is drawn between columns.
//! A label strip at the top identifies each column.

use minifb::{Key, Window, WindowOptions};

use crate::subpixel::SubpixelGrid;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default window width in host pixels.
const DEFAULT_WIN_W: usize = 960;
/// Default window height in host pixels (label strip + glyph area).
const DEFAULT_WIN_H: usize = 600;
/// Height of the label strip at the top of the window, in host pixels.
const LABEL_H: usize = 20;
/// Colour of the separator line between panels (dark grey).
const SEPARATOR_COLOR: u32 = 0xFF_33_33_33;
/// Background colour (near-black).
const BG_COLOR: u32 = 0xFF_10_10_10;
/// Label strip background.
const LABEL_BG: u32 = 0xFF_1A_1A_2E;
/// Label text colour (drawn as a solid block — no font rendering in the inspector itself).
/// Label text colour — reserved for Phase 4 when real text rendering lands in the inspector.
#[allow(dead_code)]
const LABEL_FG: u32 = 0xFF_E0_E0_E0;

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

    /// Zoom level: each source pixel is rendered as `zoom × zoom` host pixels.
    zoom: u32,
    /// Pan offset in source-pixel coordinates.
    pan_x: i32,
    pan_y: i32,

    display_mode: DisplayMode,
    /// When true, R/G/B channels are shown as false-colour heatmaps.
    heatmap: bool,
}

impl Inspector {
    /// Create a new inspector window.
    pub fn new(title: &str) -> Result<Self, minifb::Error> {
        let mut win = Window::new(
            title,
            DEFAULT_WIN_W,
            DEFAULT_WIN_H,
            WindowOptions {
                resize: true,
                ..Default::default()
            },
        )?;
        // ~60 fps update limit
        win.set_target_fps(60);

        let fb = vec![BG_COLOR; DEFAULT_WIN_W * DEFAULT_WIN_H];

        Ok(Self {
            window: win,
            fb,
            win_w: DEFAULT_WIN_W,
            win_h: DEFAULT_WIN_H,
            zoom: 8,
            pan_x: 0,
            pan_y: 0,
            display_mode: DisplayMode::SideBySide,
            heatmap: false,
        })
    }

    /// Returns `true` as long as the window is open and Esc has not been pressed.
    pub fn is_open(&self) -> bool {
        self.window.is_open() && !self.window.is_key_down(Key::Escape)
    }

    /// Process input events and redraw.
    ///
    /// Call this once per frame inside your event loop.
    ///
    /// # Parameters
    /// - `grey`   — output of the greyscale renderer
    /// - `sp`     — output of the subpixel AA renderer
    /// - `oled`   — output of the OLED-aware renderer
    pub fn update(
        &mut self,
        grey: &SubpixelGrid,
        sp: &SubpixelGrid,
        oled: &SubpixelGrid,
    ) -> Result<(), minifb::Error> {
        // Refresh window size (supports resize)
        self.win_w = self.window.get_size().0;
        self.win_h = self.window.get_size().1;
        self.fb.resize(self.win_w * self.win_h, BG_COLOR);

        self.handle_input();
        self.draw(grey, sp, oled);

        self.window
            .update_with_buffer(&self.fb, self.win_w, self.win_h)?;
        Ok(())
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    fn handle_input(&mut self) {
        // Zoom
        if self.window.is_key_pressed(Key::Equal, minifb::KeyRepeat::Yes) {
            self.zoom = (self.zoom + 1).min(32);
        }
        if self.window.is_key_pressed(Key::Minus, minifb::KeyRepeat::Yes) {
            self.zoom = (self.zoom.saturating_sub(1)).max(1);
        }

        // Pan
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
        let zoom = self.zoom as i32;
        let src_w = grid.width as i32;
        let src_h = grid.height as i32;

        for wy in 0..panel_h as i32 {
            for wx in 0..panel_w as i32 {
                // Map host pixel → source pixel (accounting for pan and zoom)
                let src_col = self.pan_x + wx / zoom;
                let src_row = self.pan_y + wy / zoom;

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
                    // Out-of-bounds: checkerboard to show extent
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

        // Draw pixel grid lines at high zoom
        if zoom >= 8 {
            self.draw_pixel_grid(panel_x, panel_y, panel_w, panel_h, zoom as usize);
        }
    }

    /// Draw thin grid lines between zoomed pixels for easy inspection.
    fn draw_pixel_grid(
        &mut self,
        panel_x: usize,
        panel_y: usize,
        panel_w: usize,
        panel_h: usize,
        zoom: usize,
    ) {
        let grid_color = 0xFF_28_28_28;
        // Vertical lines
        let mut x = panel_x;
        while x < panel_x + panel_w {
            if x % zoom == 0 {
                for y in panel_y..(panel_y + panel_h).min(self.win_h) {
                    if x < self.win_w {
                        self.fb[y * self.win_w + x] = grid_color;
                    }
                }
            }
            x += 1;
        }
        // Horizontal lines
        let mut y = panel_y;
        while y < panel_y + panel_h {
            if (y - panel_y) % zoom == 0 {
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

    /// Draw a simple coloured label bar (no glyph rendering — solid colour blocks
    /// approximate text until Phase 4 brings real text rendering to the UI).
    fn draw_label(&mut self, _text: &str, x_start: usize, width: usize) {
        // Draw a 4-pixel accent line along the bottom of the label strip
        // coloured differently per panel to make identification easy.
        let accent = match _text {
            t if t.contains("Grey")    => 0xFF_4A_90_D9,  // blue
            t if t.contains("Clear") || t.contains("Subpixel") => 0xFF_E8_7B_3A, // orange
            _                           => 0xFF_5A_C9_5A,  // green (OLED-aware)
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
