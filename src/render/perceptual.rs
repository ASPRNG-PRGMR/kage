//! The OLED-aware renderer's sampling stage.
//!
//! # Sampling at the emitter
//!
//! A pixel's three colours do not come from the same place. On a
//! conventional renderer that mismatch *is* colour fringing: a white pixel
//! at the left edge of a stem shows its red emitter further left than its
//! blue one, so the edge is tinted. The fix is not to mute colour afterwards
//! but to ask the right question in the first place — each colour takes the
//! glyph's coverage **where its own emitter sits** ([`EmitterMap`]):
//!
//! ```text
//! value(colour, pixel) = mean coverage over a footprint centred on that emitter
//! ```
//!
//! The per-pixel RGB that results looks coloured, but the light it produces
//! is not: every emitter shows the part of the glyph that is actually in
//! front of it. Measured in the panel simulator this removes most visible
//! fringing *and* lowers luminance error, where muting pixel-level colour
//! raises fringing (see the devlog, Session 14).
//!
//! # Frequency-aware: supersampled coverage
//!
//! Positions between pixel centres only mean something if the coverage has
//! detail there. The footprint is therefore integrated over coverage
//! rasterized several times finer than the pixel grid
//! ([`Coverage`]), so luminance detail above the pixel Nyquist frequency
//! reaches the emitters instead of being averaged away first.
//!
//! # Luma / chroma decoupling: two footprints
//!
//! Footprint width trades sharpness against aliasing. A footprint one pixel
//! wide is alias-free and soft; a narrower one is sharper, but emitters of
//! one colour are a whole pixel apart, so it under-samples — and because
//! each colour under-samples at a different phase, the aliasing shows up as
//! colour.
//!
//! The eye treats the two very differently ([`super::csf`]): it resolves
//! luminance far more finely than colour. Green (and white) emitters carry
//! most of the luminance; red and blue carry mostly colour. So they get
//! separate footprints:
//!
//! - [`RenderParams::luma_footprint`] for green / white — narrow keeps
//!   luminance detail;
//! - [`RenderParams::chroma_footprint`] for red / blue — wide keeps colour
//!   aliasing below what the eye can see.
//!
//! # Edge-aware chroma suppression
//!
//! Unequal footprints have a cost of their own: at a hard edge, red and
//! blue ramp more slowly than green, which tints the edge. Where the
//! coverage has a strong edge along an axis this panel fringes in, red and
//! blue are blended towards the luminance footprint:
//!
//! ```text
//! g = |(a_x · ∂x, a_y · ∂y) coverage|              2-D Sobel, 0 … 1
//! e = edge_suppression · smoothstep(EDGE_LO, EDGE_HI, g)
//! value = own_footprint + e · (luma_footprint − own_footprint)
//! ```
//!
//! Unlike the Phase 1 row Laplacian this is two-dimensional, so it sees
//! diagonal strokes and — on panels whose colours are displaced vertically —
//! horizontal edges; and it follows the panel
//! ([`EmitterMap::fringe_axis`]), so a stripe panel is not penalised for
//! edges that cannot fringe on it.
//!
//! # Matching the settings to the eye
//!
//! Which footprints are best depends on the panel and on how far away the
//! viewer is. [`crate::pipeline::Pipeline::auto_tune`] searches for the settings that
//! minimise luminance error plus CSF-weighted visible chroma in the
//! simulator. [`RenderParams::for_layout`] holds the defaults.

use crate::coverage::Coverage;
use crate::layout::{EmitterMap, SubpixelLayout};
use crate::plane::Plane;
use crate::subpixel::{SubpixelGrid, SubpixelPixel};

/// Gradient magnitude below which an edge gets no chroma suppression.
pub const EDGE_LO: f32 = 0.10;
/// Gradient magnitude at which edge suppression is at full strength.
pub const EDGE_HI: f32 = 0.60;

/// Narrowest and widest footprint, in pixels, a renderer will use.
pub const FOOTPRINT_RANGE: (f32, f32) = (0.25, 3.0);

/// Height of every footprint, in pixels. Emitters of one colour are one
/// pixel apart vertically on every supported panel, and no colour is
/// vertically finer than that, so a one-pixel height is alias-free.
pub const FOOTPRINT_HEIGHT: f32 = 1.0;

// ── Parameters ────────────────────────────────────────────────────────────────

/// Settings of the OLED-aware renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderParams {
    /// Footprint width of the luminance-carrying emitters (green, white),
    /// in pixels.
    pub luma_footprint: f32,
    /// Footprint width of the colour-carrying emitters (red, blue), in
    /// pixels.
    pub chroma_footprint: f32,
    /// How far red and blue move to the luminance footprint at strong
    /// edges, 0..=1.
    pub edge_suppression: f32,
}

impl Default for RenderParams {
    /// One-pixel footprints for every colour and no edge adaptation: plain
    /// alias-free sampling at the emitters.
    fn default() -> Self {
        Self { luma_footprint: 1.0, chroma_footprint: 1.0, edge_suppression: 0.0 }
    }
}

impl RenderParams {
    /// Default settings for a built-in layout.
    ///
    /// Every layout currently gets one-pixel footprints and no edge
    /// adaptation. A sweep over glyphs at text sizes (109 DPI, 500 mm,
    /// nominal geometries) found no other setting that was better on
    /// average by more than about 2 % of the objective; individual glyphs,
    /// larger sizes and other viewing distances do have different optima,
    /// which is what [`crate::pipeline::Pipeline::auto_tune`] is for.
    pub fn for_layout(_layout: SubpixelLayout) -> Self {
        Self::default()
    }

    /// The same settings with every value forced into its valid range.
    pub fn clamped(self) -> Self {
        let (lo, hi) = FOOTPRINT_RANGE;
        Self {
            luma_footprint: self.luma_footprint.clamp(lo, hi),
            chroma_footprint: self.chroma_footprint.clamp(lo, hi),
            edge_suppression: self.edge_suppression.clamp(0.0, 1.0),
        }
    }
}

// ── The stage ─────────────────────────────────────────────────────────────────

#[inline]
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Edge strength of `cov` at (`col`, `row`), with horizontal and vertical
/// gradients weighted by `axis`. A full-contrast edge along a weighted axis
/// gives 1.
pub fn edge_strength(cov: &Plane, col: u32, row: u32, axis: [f32; 2]) -> f32 {
    let (x, y) = (col as i32, row as i32);
    let c = |dx: i32, dy: i32| cov.get(x + dx, y + dy);
    let gx = (c(1, -1) + 2.0 * c(1, 0) + c(1, 1)) - (c(-1, -1) + 2.0 * c(-1, 0) + c(-1, 1));
    let gy = (c(-1, 1) + 2.0 * c(0, 1) + c(1, 1)) - (c(-1, -1) + 2.0 * c(0, -1) + c(1, -1));
    // The Sobel weights sum to 4, so this is the difference between the two
    // neighbours along each axis, smoothed across the other.
    let (gx, gy) = (axis[0] * gx / 4.0, axis[1] * gy / 4.0);
    (gx * gx + gy * gy).sqrt()
}

/// Render `cov` by sampling each colour at its emitter.
pub fn render_at_emitters(cov: &Coverage, map: &EmitterMap, params: &RenderParams) -> SubpixelGrid {
    let p = params.clamped();
    let axis = map.fringe_axis();
    let (w, h) = (cov.width(), cov.height());
    let mut out = SubpixelGrid::new(w, h);
    for row in 0..h {
        for col in 0..w {
            let sites = map.at(col, row);
            let edge = if p.edge_suppression > 0.0 {
                p.edge_suppression * smoothstep(EDGE_LO, EDGE_HI, edge_strength(&cov.lo, col, row, axis))
            } else {
                0.0
            };
            let sample = |k: usize| {
                let (cx, cy) = (col as f32 + sites[k].pos[0], row as f32 + sites[k].pos[1]);
                let luma = cov.sample_box(cx, cy, p.luma_footprint, FOOTPRINT_HEIGHT);
                // Green carries luminance; so does everything on a panel
                // whose light comes from white emitters.
                if k == 1 || map.achromatic {
                    return luma;
                }
                let own = cov.sample_box(cx, cy, p.chroma_footprint, FOOTPRINT_HEIGHT);
                own + edge * (luma - own)
            };
            *out.pixel_mut(col, row) = SubpixelPixel { r: sample(0), g: sample(1), b: sample(2) };
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A vertical stem with edges at x = 4.25 and 7.5, four samples per pixel.
    fn stem() -> Coverage {
        Coverage::from_hi(Plane::from_fn(48, 16, |x, _| if (17..30).contains(&x) { 1.0 } else { 0.0 }), 4)
    }

    fn params(luma: f32, chroma: f32, edge: f32) -> RenderParams {
        RenderParams { luma_footprint: luma, chroma_footprint: chroma, edge_suppression: edge }
    }

    #[test]
    fn centred_emitters_with_pixel_footprints_give_greyscale() {
        let cov = stem();
        let out = render_at_emitters(&cov, &EmitterMap::centred(), &RenderParams::default());
        for col in 0..cov.width() {
            let px = out.pixel(col, 1);
            let c = cov.lo.get(col as i32, 1);
            assert!((px.r - c).abs() < 1e-6 && (px.g - c).abs() < 1e-6 && (px.b - c).abs() < 1e-6);
        }
    }

    #[test]
    fn each_colour_sees_the_glyph_at_its_own_emitter() {
        // Left edge at x = 4.25. In pixel 4 the red bar (x = 4.17) is just
        // outside the stem and the blue bar (x = 4.83) well inside.
        let cov = stem();
        let out = render_at_emitters(&cov, &EmitterMap::for_layout(SubpixelLayout::RgbStripe), &RenderParams::default());
        let px = out.pixel(4, 1);
        assert!(px.r < px.g && px.g < px.b, "{px:?}");
        // 1 px boxes centred at 4.167 / 4.5 / 4.833 against an edge at 4.25.
        assert!((px.r - (4.6667 - 4.25)).abs() < 1e-3, "{}", px.r);
        assert!((px.g - 0.75).abs() < 1e-3, "{}", px.g);
        assert!((px.b - 1.0).abs() < 1e-3, "{}", px.b);
    }

    #[test]
    fn flat_fields_stay_flat_on_every_layout() {
        let cov = Coverage::from_hi(Plane::from_fn(24, 24, |_, _| 0.6), 4);
        for layout in [
            SubpixelLayout::RgbStripe, SubpixelLayout::BgrStripe, SubpixelLayout::PentileRgbg,
            SubpixelLayout::DeltaRgb, SubpixelLayout::Wrgb, SubpixelLayout::Greyscale,
        ] {
            let out = render_at_emitters(&cov, &EmitterMap::for_layout(layout), &params(0.5, 2.0, 0.7));
            for row in 0..cov.height() {
                for col in 0..cov.width() {
                    let px = out.pixel(col, row);
                    for v in [px.r, px.g, px.b] {
                        assert!((v - 0.6).abs() < 1e-5, "{layout:?} ({col},{row}): {v}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_narrow_luma_footprint_is_sharper() {
        let cov = stem();
        let map = EmitterMap::for_layout(SubpixelLayout::RgbStripe);
        let soft = render_at_emitters(&cov, &map, &params(1.0, 1.0, 0.0));
        let sharp = render_at_emitters(&cov, &map, &params(0.4, 1.0, 0.0));
        // Green in pixel 4 (centre 4.5, edge at 4.25): the narrow footprint
        // is entirely inside the stem.
        assert!(sharp.pixel(4, 1).g > soft.pixel(4, 1).g);
        assert!((sharp.pixel(4, 1).g - 1.0).abs() < 1e-5);
        // Red and blue did not change.
        assert_eq!(sharp.pixel(4, 1).r, soft.pixel(4, 1).r);
    }

    #[test]
    fn a_wide_chroma_footprint_softens_only_red_and_blue() {
        let cov = stem();
        let map = EmitterMap::for_layout(SubpixelLayout::RgbStripe);
        let a = render_at_emitters(&cov, &map, &params(1.0, 1.0, 0.0));
        let b = render_at_emitters(&cov, &map, &params(1.0, 2.0, 0.0));
        assert_eq!(a.pixel(4, 1).g, b.pixel(4, 1).g);
        assert!(b.pixel(3, 1).b > a.pixel(3, 1).b, "wider footprint reaches the stem from further away");
    }

    #[test]
    fn edge_suppression_pulls_chroma_footprints_to_luma_at_edges() {
        let cov = stem();
        let map = EmitterMap::for_layout(SubpixelLayout::RgbStripe);
        let wide = render_at_emitters(&cov, &map, &params(1.0, 2.0, 0.0));
        let equal = render_at_emitters(&cov, &map, &params(1.0, 1.0, 0.0));
        let adaptive = render_at_emitters(&cov, &map, &params(1.0, 2.0, 1.0));
        // At the edge pixel the adaptive result equals the equal-footprint one…
        assert!((adaptive.pixel(4, 1).r - equal.pixel(4, 1).r).abs() < 1e-5);
        assert!((adaptive.pixel(4, 1).r - wide.pixel(4, 1).r).abs() > 1e-3);
        // …and far from any edge nothing changes.
        assert_eq!(adaptive.pixel(0, 1).r, wide.pixel(0, 1).r);
    }

    #[test]
    fn white_emitter_panels_stay_achromatic() {
        let cov = stem();
        let out = render_at_emitters(&cov, &EmitterMap::for_layout(SubpixelLayout::Wrgb), &params(0.6, 2.0, 0.5));
        for col in 0..cov.width() {
            let px = out.pixel(col, 1);
            assert!(px.r == px.g && px.g == px.b, "{px:?}");
        }
        // Sampled left of centre: pixel 4 (white at 4.375, edge at 4.25).
        let plain = cov.lo.get(4, 1);
        assert!(out.pixel(4, 1).g != plain);
    }

    #[test]
    fn output_is_in_range() {
        let cov = stem();
        let out = render_at_emitters(&cov, &EmitterMap::for_layout(SubpixelLayout::DeltaRgb), &params(0.1, 9.0, 3.0));
        for row in 0..cov.height() {
            for col in 0..cov.width() {
                let px = out.pixel(col, row);
                for v in [px.r, px.g, px.b] {
                    assert!((0.0..=1.0 + 1e-6).contains(&v));
                }
            }
        }
    }

    #[test]
    fn edge_strength_is_one_on_a_full_step_and_zero_on_flat() {
        let cov = Plane::from_fn(8, 5, |c, _| if c >= 4 { 1.0 } else { 0.0 });
        assert!((edge_strength(&cov, 3, 2, [1.0, 1.0]) - 1.0).abs() < 1e-6);
        assert!((edge_strength(&cov, 4, 2, [1.0, 1.0]) - 1.0).abs() < 1e-6);
        assert_eq!(edge_strength(&cov, 1, 2, [1.0, 1.0]), 0.0);
        let soft = Plane::from_fn(8, 5, |c, _| [0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0][c as usize]);
        assert!((edge_strength(&soft, 2, 2, [1.0, 1.0]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn edge_strength_follows_the_panel_axis() {
        // A horizontal edge does not fringe on a stripe panel.
        let cov = Plane::from_fn(6, 8, |_, r| if r >= 4 { 1.0 } else { 0.0 });
        let stripe = EmitterMap::for_layout(SubpixelLayout::RgbStripe).fringe_axis();
        let delta = EmitterMap::for_layout(SubpixelLayout::DeltaRgb).fringe_axis();
        assert_eq!(edge_strength(&cov, 2, 4, stripe), 0.0);
        assert!(edge_strength(&cov, 2, 4, delta) > 0.9);
    }

    #[test]
    fn params_are_clamped() {
        let p = params(0.0, 99.0, -1.0).clamped();
        assert_eq!((p.luma_footprint, p.chroma_footprint, p.edge_suppression), (FOOTPRINT_RANGE.0, FOOTPRINT_RANGE.1, 0.0));
    }
}
