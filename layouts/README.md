# Panel files

Each `.toml` file here describes one panel's subpixel geometry. Pass one with
`--panel`:

```bash
cargo run --release -- --panel layouts/delta-rgb.toml --glyph g
```

The simulator draws the emitters the file lists, and the OLED-aware renderer
samples each colour at the emitter positions it gives, so a custom panel
changes both the rendering and its evaluation.

**The files shipped here are nominal.** They reproduce the built-in presets
and exist as templates. None of them is a measurement of a real panel.

## Format

```toml
name   = "Example QD-OLED"        # required
source = "macro photo, 2026-11"   # optional: where the numbers came from
layout = "delta"                  # optional: rgb | bgr | pentile | delta | wrgb | grey
cell   = [1, 1]                   # optional: unit cell in pixels, default [1, 1]
pixel_pitch_um = 233.0            # optional: see Units

[bleed]                           # optional: blur width per colour, green = 1
red   = 1.20
green = 1.00
blue  = 1.10

[[emitter]]                       # one block per emitter in the unit cell
channel = "green"                 # red | green | blue | white
center  = [116.5, 62.9]
size    = [102.5, 79.2]
```

| Key | Meaning |
|-----|---------|
| `name` | Shown in the CLI output. |
| `source` | Free text recording where the numbers came from. |
| `layout` | Which fixed kernel the *Subpixel AA* baseline uses, and the default tuning. Overrides `--layout`. |
| `cell` | Size of the repeating unit in pixels. The renderer supports up to 2×2. |
| `pixel_pitch_um` | Pixel pitch in micrometres. |
| `[bleed]` | How much wider each colour's optical blur is than green's. |
| `[[emitter]]` | An emitter: its colour, centre and aperture size. |

Every panel needs at least one red, one green and one blue emitter, and each
emitter must lie inside the unit cell.

## Units

Without `pixel_pitch_um`, `center` and `size` are in logical pixels: the unit
cell spans `0..cell[0]` horizontally and `0..cell[1]` vertically, with y
pointing down.

With `pixel_pitch_um`, they are in micrometres and are divided by the pitch
when the file is loaded, so measurements can be typed in as taken.

## Measuring a panel

1. Photograph a white area of the panel through a macro lens or microscope,
   with something of known size in frame (or use the known pixel pitch:
   25400 / DPI micrometres).
2. Pick one repeating unit. For each emitter note the centre and the width
   and height of the lit area, relative to the unit's top-left corner.
3. Enter them as `[[emitter]]` blocks with `pixel_pitch_um` set.
