//! [`DisplayProfile`] describes the physical characteristics of the target display.
//!
//! The profile drives two decisions in the rendering pipeline:
//! - Which subpixel layout geometry to use (→ `layout` module)
//! - How to linearise coverage values before compositing (EOTF + white point)

/// Electro-optical transfer function of the panel.
///
/// Coverage values produced by the rasteriser are in [0.0, 1.0] linear light.
/// Before they reach the framebuffer they must be encoded with the panel's EOTF
/// inverse (i.e. the OETF).  We store the *panel* EOTF here and invert on output.
#[derive(Debug, Clone, PartialEq)]
pub enum Eotf {
    /// Pure power curve.  sRGB uses γ ≈ 2.2 as a simplification; true sRGB uses
    /// a piecewise function (see [`Eotf::Srgb`]).
    Gamma(f32),

    /// IEC 61966-2-1 piecewise sRGB curve.  Correct for most SDR OLED monitors.
    Srgb,

    /// ITU-R BT.2100 PQ (ST 2084).  Used by HDR OLED panels.
    /// `peak_nits` is the panel's peak luminance in cd/m².
    Pq { peak_nits: f32 },
}

impl Eotf {
    /// Encode a linear-light value in [0, 1] to the signal domain [0, 1].
    ///
    /// For sRGB this is the standard piecewise OETF.
    /// For PQ the output is normalised to `peak_nits`.
    #[inline]
    pub fn encode(&self, linear: f32) -> f32 {
        let v = linear.clamp(0.0, 1.0);
        match self {
            Eotf::Gamma(g) => v.powf(1.0 / g),
            Eotf::Srgb => {
                if v <= 0.003_130_8 {
                    12.92 * v
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            }
            Eotf::Pq { peak_nits } => {
                // Normalise to absolute luminance, then apply ST 2084 OETF.
                let y = v * peak_nits / 10_000.0;
                pq_oetf(y)
            }
        }
    }

    /// Decode a signal-domain value [0, 1] to linear light [0, 1].
    #[inline]
    pub fn decode(&self, signal: f32) -> f32 {
        let v = signal.clamp(0.0, 1.0);
        match self {
            Eotf::Gamma(g) => v.powf(*g),
            Eotf::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Eotf::Pq { peak_nits } => {
                let abs_nits = pq_eotf(v) * 10_000.0;
                (abs_nits / peak_nits).clamp(0.0, 1.0)
            }
        }
    }
}

/// ITU-R BT.2100 PQ OETF (signal → linear normalised to 10 000 cd/m²).
fn pq_oetf(y: f32) -> f32 {
    const M1: f32 = 0.159_301_758;
    const M2: f32 = 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.6875;

    let yp = y.powf(M1);
    ((C1 + C2 * yp) / (1.0 + C3 * yp)).powf(M2)
}

/// ITU-R BT.2100 PQ EOTF (linear normalised to 10 000 cd/m² → signal).
fn pq_eotf(e: f32) -> f32 {
    const M1_INV: f32 = 1.0 / 0.159_301_758;
    const M2_INV: f32 = 1.0 / 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.6875;

    let ep = e.powf(M2_INV);
    let num = (ep - C1).max(0.0);
    let den = C2 - C3 * ep;
    (num / den).powf(M1_INV)
}

/// CIE xy chromaticity of the display white point.
///
/// Used to adapt the subpixel filter weights when the panel's native white
/// deviates from D65 — common on high-gamut OLED panels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WhitePoint {
    pub x: f32,
    pub y: f32,
}

impl WhitePoint {
    /// Standard D65 (sRGB, most OLED SDR panels).
    pub const D65: Self = Self { x: 0.3127, y: 0.3290 };

    /// DCI-P3 white (some wide-gamut panels).
    pub const DCI_P3: Self = Self { x: 0.3140, y: 0.3510 };
}

/// Physical description of the target display.
///
/// Construct with [`DisplayProfile::sdr_oled`] for typical Samsung OLED panels,
/// or use the builder to describe unusual configurations.
#[derive(Debug, Clone)]
pub struct DisplayProfile {
    /// Panel EOTF. Most OLED monitors present as sRGB to the OS.
    pub eotf: Eotf,

    /// Chromaticity of the display white point.
    pub white_point: WhitePoint,

    /// Physical dots-per-inch of the display.
    ///
    /// Used to scale hinting thresholds: stems that are sub-pixel at 96 dpi
    /// may be multi-pixel at 220 dpi (HiDPI), where subpixel rendering
    /// is less beneficial and greyscale AA may be preferred outright.
    pub dpi: f32,

    /// Device pixel ratio (CSS pixels → physical pixels).
    /// 1.0 for standard density, 2.0 for typical HiDPI.
    pub device_pixel_ratio: f32,
}

impl DisplayProfile {
    /// A typical SDR Samsung OLED monitor (e.g. Odyssey G8, Smart Monitor M8).
    /// sRGB EOTF, D65 white point, 109 dpi, 1× DPR.
    pub fn sdr_oled() -> Self {
        Self {
            eotf: Eotf::Srgb,
            white_point: WhitePoint::D65,
            dpi: 109.0,
            device_pixel_ratio: 1.0,
        }
    }

    /// Returns `true` if the effective DPI is high enough that
    /// greyscale antialiasing is probably preferable to subpixel rendering.
    ///
    /// Threshold: 192 physical dpi (equivalent to ~2× 96 dpi).
    #[inline]
    pub fn is_hidpi(&self) -> bool {
        self.dpi * self.device_pixel_ratio >= 192.0
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrip() {
        let eotf = Eotf::Srgb;
        for &v in &[0.0f32, 0.001, 0.04, 0.18, 0.5, 0.9, 1.0] {
            let encoded = eotf.encode(v);
            let decoded = eotf.decode(encoded);
            assert!(
                (decoded - v).abs() < 1e-5,
                "sRGB roundtrip failed at {v}: got {decoded}"
            );
        }
    }

    #[test]
    fn gamma_roundtrip() {
        let eotf = Eotf::Gamma(2.2);
        for &v in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let rt = eotf.decode(eotf.encode(v));
            assert!((rt - v).abs() < 1e-5, "gamma roundtrip failed at {v}");
        }
    }

    #[test]
    fn hidpi_threshold() {
        let mut p = DisplayProfile::sdr_oled(); // 109 dpi, 1×
        assert!(!p.is_hidpi());
        p.device_pixel_ratio = 2.0; // effectively 218 dpi
        assert!(p.is_hidpi());
    }
}
