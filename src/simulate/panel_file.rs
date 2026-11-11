//! Panel descriptions loaded from `.toml` files.
//!
//! The built-in [`PanelGeometry`] presets are nominal. A panel file replaces
//! them with numbers for one specific panel — from a datasheet, or measured
//! off a macro photograph — and can also carry that panel's per-colour
//! bleed. Both the simulator and the OLED-aware renderer (through
//! [`crate::layout::EmitterMap`]) follow whatever the file says.
//!
//! # Format
//!
//! ```toml
//! name   = "Example QD-OLED"           # required
//! source = "macro photo, 2026-11"      # optional: where the numbers came from
//! layout = "delta"                     # optional: kernel for the Subpixel AA baseline
//! cell   = [1, 1]                      # optional: unit cell in pixels (default 1×1)
//! pixel_pitch_um = 233.0               # optional: see "Units"
//!
//! [bleed]                              # optional: blur width per colour, green = 1
//! red   = 1.20
//! green = 1.00
//! blue  = 1.10
//!
//! [[emitter]]                          # one per emitter in the unit cell
//! channel = "green"                    # red | green | blue | white
//! center  = [116.5, 62.9]
//! size    = [102.5, 79.2]
//! ```
//!
//! # Units
//!
//! Without `pixel_pitch_um`, `center` and `size` are in logical pixels: the
//! unit cell spans `0..cell[0]` × `0..cell[1]`, y down. With it, they are in
//! micrometres and are divided by the pitch on load, so measurements can be
//! entered as taken.

use std::path::Path;

use serde::Deserialize;

use super::{BleedProfile, Emitter, EmitterChannel, PanelGeometry};
use crate::layout::SubpixelLayout;

/// A panel loaded from a file.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelDescription {
    pub geometry: PanelGeometry,
    /// Per-colour bleed, if the file gives one.
    pub bleed: Option<BleedProfile>,
    /// Layout whose fixed kernel the Subpixel AA baseline should use.
    pub layout: Option<SubpixelLayout>,
    /// Where the numbers came from, as stated in the file.
    pub source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPanel {
    name: String,
    source: Option<String>,
    layout: Option<String>,
    cell: Option<[u32; 2]>,
    pixel_pitch_um: Option<f32>,
    bleed: Option<RawBleed>,
    #[serde(default)]
    emitter: Vec<RawEmitter>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBleed {
    red: f32,
    green: f32,
    blue: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEmitter {
    channel: String,
    center: [f32; 2],
    size: [f32; 2],
}

impl PanelDescription {
    /// Parse the contents of a panel file.
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawPanel = toml::from_str(text).map_err(|e| e.to_string())?;

        let unit = match raw.pixel_pitch_um {
            Some(pitch) if pitch > 0.0 => 1.0 / pitch,
            Some(pitch) => return Err(format!("pixel_pitch_um must be positive, got {pitch}")),
            None => 1.0,
        };
        if raw.emitter.is_empty() {
            return Err("panel has no [[emitter]] entries".into());
        }
        let emitters = raw
            .emitter
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let channel = match e.channel.trim().to_lowercase().as_str() {
                    "red" | "r" => EmitterChannel::Red,
                    "green" | "g" => EmitterChannel::Green,
                    "blue" | "b" => EmitterChannel::Blue,
                    "white" | "w" => EmitterChannel::White,
                    other => return Err(format!("emitter {i}: unknown channel '{other}' (use red, green, blue or white)")),
                };
                Ok(Emitter::new(channel, e.center[0] * unit, e.center[1] * unit, e.size[0] * unit, e.size[1] * unit))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let [cell_w, cell_h] = raw.cell.unwrap_or([1, 1]);
        let geometry = PanelGeometry { name: raw.name, cell_w, cell_h, emitters };
        geometry.validate()?;

        let layout = match raw.layout.as_deref() {
            Some(name) => Some(
                SubpixelLayout::from_name(name)
                    .ok_or_else(|| format!("unknown layout '{name}' (use rgb, bgr, pentile, delta, wrgb or grey)"))?,
            ),
            None => None,
        };

        let bleed = match raw.bleed {
            Some(b) => {
                if !(b.red > 0.0 && b.green > 0.0 && b.blue > 0.0) {
                    return Err("bleed multipliers must be positive".into());
                }
                Some(BleedProfile { red_mult: b.red, green_mult: b.green, blue_mult: b.blue })
            }
            None => None,
        };

        Ok(Self { geometry, bleed, layout, source: raw.source })
    }

    /// Load and parse a panel file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        name = "Test stripe"
        [[emitter]]
        channel = "red"
        center = [0.2, 0.5]
        size = [0.2, 0.8]
        [[emitter]]
        channel = "green"
        center = [0.5, 0.5]
        size = [0.2, 0.8]
        [[emitter]]
        channel = "blue"
        center = [0.8, 0.5]
        size = [0.2, 0.8]
    "#;

    #[test]
    fn minimal_file_parses_with_defaults() {
        let panel = PanelDescription::parse(MINIMAL).unwrap();
        assert_eq!(panel.geometry.name, "Test stripe");
        assert_eq!((panel.geometry.cell_w, panel.geometry.cell_h), (1, 1));
        assert_eq!(panel.geometry.emitters.len(), 3);
        assert_eq!(panel.geometry.emitters[2], Emitter::new(EmitterChannel::Blue, 0.8, 0.5, 0.2, 0.8));
        assert!(panel.bleed.is_none() && panel.layout.is_none() && panel.source.is_none());
    }

    #[test]
    fn micrometres_are_divided_by_the_pitch() {
        let text = MINIMAL.replace("name = \"Test stripe\"", "name = \"um\"\npixel_pitch_um = 200.0")
            .replace("[0.2, 0.5]", "[40.0, 100.0]")
            .replace("[0.5, 0.5]", "[100.0, 100.0]")
            .replace("[0.8, 0.5]", "[160.0, 100.0]")
            .replace("[0.2, 0.8]", "[40.0, 160.0]");
        let um = PanelDescription::parse(&text).unwrap();
        let px = PanelDescription::parse(MINIMAL).unwrap();
        for (a, b) in um.geometry.emitters.iter().zip(&px.geometry.emitters) {
            assert!((a.cx - b.cx).abs() < 1e-6 && (a.w - b.w).abs() < 1e-6 && (a.h - b.h).abs() < 1e-6);
        }
    }

    #[test]
    fn optional_sections_are_read() {
        let text = format!(
            "source = \"datasheet\"\nlayout = \"rgb\"\n{MINIMAL}\n[bleed]\nred = 1.3\ngreen = 1.0\nblue = 1.05\n"
        );
        let panel = PanelDescription::parse(&text).unwrap();
        assert_eq!(panel.layout, Some(SubpixelLayout::RgbStripe));
        assert_eq!(panel.source.as_deref(), Some("datasheet"));
        let bleed = panel.bleed.unwrap();
        assert_eq!((bleed.red_mult, bleed.green_mult, bleed.blue_mult), (1.3, 1.0, 1.05));
    }

    #[test]
    fn mistakes_are_reported() {
        let err = |text: &str| PanelDescription::parse(text).unwrap_err();
        assert!(err("name = \"x\"").contains("no [[emitter]]"));
        assert!(err(&MINIMAL.replace("\"blue\"", "\"cyan\"")).contains("unknown channel"));
        assert!(err(&MINIMAL.replace("[0.8, 0.5]", "[0.95, 0.5]")).contains("outside the unit cell"));
        assert!(err(&format!("layout = \"plasma\"\n{MINIMAL}")).contains("unknown layout"));
        assert!(err(&format!("pixel_pitch_um = 0.0\n{MINIMAL}")).contains("positive"));
        assert!(err(&format!("colour = 3\n{MINIMAL}")).contains("unknown field"));
        // Dropping the blue emitter leaves the panel without one.
        let no_blue = MINIMAL.rsplit_once("[[emitter]]").unwrap().0;
        assert!(err(no_blue).contains("no Blue emitter"));
    }

    #[test]
    fn shipped_layout_files_match_the_built_in_presets() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts");
        for (file, layout) in [
            ("rgb-stripe.toml", SubpixelLayout::RgbStripe),
            ("bgr-stripe.toml", SubpixelLayout::BgrStripe),
            ("pentile-rgbg.toml", SubpixelLayout::PentileRgbg),
            ("delta-rgb.toml", SubpixelLayout::DeltaRgb),
            ("wrgb.toml", SubpixelLayout::Wrgb),
        ] {
            let panel = PanelDescription::load(dir.join(file)).unwrap_or_else(|e| panic!("{e}"));
            let preset = PanelGeometry::for_layout(layout);
            assert_eq!(panel.layout, Some(layout), "{file}");
            assert_eq!((panel.geometry.cell_w, panel.geometry.cell_h), (preset.cell_w, preset.cell_h), "{file}");
            assert_eq!(panel.geometry.emitters.len(), preset.emitters.len(), "{file}");
            for (a, b) in panel.geometry.emitters.iter().zip(&preset.emitters) {
                assert_eq!(a.channel, b.channel, "{file}");
                for (x, y) in [(a.cx, b.cx), (a.cy, b.cy), (a.w, b.w), (a.h, b.h)] {
                    assert!((x - y).abs() < 1e-4, "{file}: {a:?} vs {b:?}");
                }
            }
            let bleed = panel.bleed.expect("shipped files state their bleed");
            let default = BleedProfile::oled_default();
            assert_eq!((bleed.red_mult, bleed.green_mult, bleed.blue_mult), (default.red_mult, default.green_mult, default.blue_mult));
        }
    }
}
