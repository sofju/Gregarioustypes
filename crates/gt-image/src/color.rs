//! Colour description: transfer functions, primaries → linear Rec.709 (scRGB) matrices,
//! CICP mapping and ICC handling (matrix/TRC extraction, or a full lcms2 transform).

use crate::ffi::*;
use std::ffi::c_void;

/// How pixel values are encoded (decoded in the shader).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transfer {
    Linear,
    Srgb,
    /// Decode exponent (value^g).
    Gamma(f32),
    /// BT.709 / BT.601 inverse OETF.
    Bt709,
    /// SMPTE ST 2084; decodes to nits / 10000.
    Pq,
    /// ARIB STD-B67; decodes to scene-linear [0,1].
    Hlg,
    /// Per-channel 1D lookup table from an ICC profile.
    Lut,
}

/// What a decoded linear value of 1.0 means.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lum {
    /// 1.0 = SDR (diffuse) white; values above are highlights.
    Relative,
    /// Absolute nits from PQ.
    Pq,
    /// Scene light from HLG, displayed through the BT.2100 OOTF.
    Hlg,
    /// scRGB: 1.0 = 80 nits (JPEG XR HDR captures).
    ScRgb,
}

#[derive(Clone, Debug)]
pub struct Color {
    pub transfer: Transfer,
    /// Linear source RGB → linear Rec.709 (rows).
    pub to709: [[f32; 3]; 3],
    pub lum: Lum,
    /// TRC lookup table (RGB per entry) for `Transfer::Lut`.
    pub lut: Option<Vec<[f32; 4]>>,
    pub name: String,
}

pub const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Color {
    pub fn srgb() -> Self {
        Self { transfer: Transfer::Srgb, to709: IDENTITY, lum: Lum::Relative, lut: None, name: "sRGB".into() }
    }
    pub fn linear_709(name: &str) -> Self {
        Self { transfer: Transfer::Linear, to709: IDENTITY, lum: Lum::Relative, lut: None, name: name.into() }
    }
    pub fn is_hdr(&self) -> bool {
        matches!(self.lum, Lum::Pq | Lum::Hlg | Lum::ScRgb)
    }
}

type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn mulv(a: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

fn inv(m: &M3) -> M3 {
    let [a, b, c] = m[0];
    let [d, e, f] = m[1];
    let [g, h, i] = m[2];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let k = if det.abs() < 1e-12 { 0.0 } else { 1.0 / det };
    [
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ]
}

fn xy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// RGB → XYZ for primaries/white given as rx ry gx gy bx by wx wy.
fn rgb_to_xyz(p: [f64; 8]) -> M3 {
    let r = xy_to_xyz(p[0], p[1]);
    let g = xy_to_xyz(p[2], p[3]);
    let b = xy_to_xyz(p[4], p[5]);
    let w = xy_to_xyz(p[6], p[7]);
    let pm: M3 = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = mulv(&inv(&pm), w);
    [0, 1, 2].map(|i| [pm[i][0] * s[0], pm[i][1] * s[1], pm[i][2] * s[2]])
}

const BRADFORD: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
pub const D65: [f64; 2] = [0.3127, 0.3290];
const D50_XYZ: [f64; 3] = [0.9642, 1.0, 0.8249];

fn adapt(src_white: [f64; 3], dst_white: [f64; 3]) -> M3 {
    let s = mulv(&BRADFORD, src_white);
    let d = mulv(&BRADFORD, dst_white);
    let scale: M3 = [[d[0] / s[0], 0.0, 0.0], [0.0, d[1] / s[1], 0.0], [0.0, 0.0, d[2] / s[2]]];
    mul(&inv(&BRADFORD), &mul(&scale, &BRADFORD))
}

const BT709: [f64; 8] = [0.64, 0.33, 0.30, 0.60, 0.15, 0.06, 0.3127, 0.3290];

fn xyz_to_709() -> M3 {
    inv(&rgb_to_xyz(BT709))
}

fn to_f32(m: M3) -> [[f32; 3]; 3] {
    m.map(|r| r.map(|v| v as f32))
}

/// Matrix converting linear RGB with the given primaries/white to linear Rec.709 (D65).
pub fn to709_from_xy(p: [f32; 8]) -> [[f32; 3]; 3] {
    let p64 = p.map(|v| v as f64);
    let m = rgb_to_xyz(p64);
    let white = xy_to_xyz(p64[6], p64[7]);
    let d65 = xy_to_xyz(D65[0], D65[1]);
    to_f32(mul(&xyz_to_709(), &mul(&adapt(white, d65), &m)))
}

/// ITU-T H.273 colour primaries.
pub fn cicp_primaries(cp: i32) -> Option<([f32; 8], &'static str)> {
    Some(match cp {
        1 => ([0.64, 0.33, 0.30, 0.60, 0.15, 0.06, 0.3127, 0.3290], "Rec. 709"),
        4 => ([0.67, 0.33, 0.21, 0.71, 0.14, 0.08, 0.310, 0.316], "BT.470 M"),
        5 => ([0.64, 0.33, 0.29, 0.60, 0.15, 0.06, 0.3127, 0.3290], "BT.601 625"),
        6 | 7 => ([0.630, 0.340, 0.310, 0.595, 0.155, 0.070, 0.3127, 0.3290], "BT.601 525"),
        9 => ([0.708, 0.292, 0.170, 0.797, 0.131, 0.046, 0.3127, 0.3290], "Rec. 2020"),
        11 => ([0.680, 0.320, 0.265, 0.690, 0.150, 0.060, 0.314, 0.351], "DCI-P3"),
        12 => ([0.680, 0.320, 0.265, 0.690, 0.150, 0.060, 0.3127, 0.3290], "Display P3"),
        22 => ([0.630, 0.340, 0.295, 0.605, 0.155, 0.077, 0.3127, 0.3290], "EBU 3213"),
        _ => return None,
    })
}

/// ITU-T H.273 transfer characteristics.
pub fn cicp_transfer(tc: i32, gamma: f32) -> (Transfer, Lum, &'static str) {
    match tc {
        1 | 6 | 14 | 15 => (Transfer::Bt709, Lum::Relative, "BT.709"),
        4 => (Transfer::Gamma(if gamma > 0.0 { gamma } else { 2.2 }), Lum::Relative, "gamma"),
        5 => (Transfer::Gamma(2.8), Lum::Relative, "gamma 2.8"),
        8 => (Transfer::Linear, Lum::Relative, "linear"),
        16 => (Transfer::Pq, Lum::Pq, "PQ"),
        17 => (Transfer::Gamma(2.6), Lum::Relative, "DCI gamma 2.6"),
        18 => (Transfer::Hlg, Lum::Hlg, "HLG"),
        _ => (Transfer::Srgb, Lum::Relative, "sRGB"),
    }
}

pub fn from_cicp(cp: i32, tc: i32, gamma: f32, custom: Option<[f32; 8]>) -> Color {
    let (prim, pname) = match custom {
        Some(p) => (p, "custom primaries"),
        None => cicp_primaries(cp).unwrap_or(([0.64, 0.33, 0.30, 0.60, 0.15, 0.06, 0.3127, 0.3290], "Rec. 709")),
    };
    let (transfer, lum, tname) = cicp_transfer(tc, gamma);
    let to709 = if cp == 1 && custom.is_none() { IDENTITY } else { to709_from_xy(prim) };
    let name = match (pname, tname) {
        ("Rec. 709", "sRGB") => "sRGB".to_string(),
        ("Rec. 2020", "PQ") => "Rec. 2100 PQ (HDR10)".to_string(),
        ("Rec. 2020", "HLG") => "Rec. 2100 HLG".to_string(),
        (p, t) => format!("{p}, {t}"),
    };
    Color { transfer, to709, lum, lut: None, name }
}

pub fn srgb_eotf(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

// ---------------------------------------------------------------- ICC

pub enum Icc {
    /// Matrix/TRC (or gray TRC) profile, evaluated on the GPU.
    Shader(Color),
    /// Needs a full CMM transform on the CPU (LUT-based RGB, CMYK...).
    Cmm { name: String, space: u32 },
}

struct Profile(*mut c_void);
impl Drop for Profile {
    fn drop(&mut self) {
        unsafe {
            cmsCloseProfile(self.0);
        }
    }
}

fn open(icc: &[u8]) -> Option<Profile> {
    let p = unsafe { cmsOpenProfileFromMem(icc.as_ptr() as _, icc.len() as u32) };
    (!p.is_null()).then_some(Profile(p))
}

pub fn icc_description(icc: &[u8]) -> Option<String> {
    let p = open(icc)?;
    let mut buf = [0i8; 256];
    let n = unsafe { cmsGetProfileInfoASCII(p.0, 0, c"en".as_ptr(), c"US".as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    (n > 1).then(|| {
        let b: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        String::from_utf8_lossy(&b).trim().to_string()
    })
}

fn tag_xyz(p: &Profile, sig: u32) -> Option<[f64; 3]> {
    let t = unsafe { cmsReadTag(p.0, sig) } as *const CieXyz;
    (!t.is_null()).then(|| unsafe { [(*t).x, (*t).y, (*t).z] })
}

const LUT_N: usize = 1024;

fn trc_lut(p: &Profile, sigs: [u32; 3]) -> Option<Vec<[f32; 4]>> {
    let curves: Vec<*mut c_void> = sigs.iter().map(|&s| unsafe { cmsReadTag(p.0, s) }).collect();
    if curves.iter().any(|c| c.is_null()) {
        return None;
    }
    Some(
        (0..LUT_N)
            .map(|i| {
                let x = i as f32 / (LUT_N - 1) as f32;
                let v = [0, 1, 2].map(|c| unsafe { cmsEvalToneCurveFloat(curves[c], x) });
                [v[0], v[1], v[2], 1.0]
            })
            .collect(),
    )
}

fn lut_is_srgb(lut: &[[f32; 4]]) -> bool {
    lut.iter().enumerate().all(|(i, e)| {
        let s = srgb_eotf(i as f32 / (LUT_N - 1) as f32);
        (0..3).all(|c| (e[c] - s).abs() < 0.0025)
    })
}

/// Pure power curve? Returns the exponent.
fn lut_gamma(lut: &[[f32; 4]]) -> Option<f32> {
    let i = LUT_N / 2;
    let x = i as f32 / (LUT_N - 1) as f32;
    let g = lut[i][0].max(1e-6).ln() / x.ln();
    let ok = lut.iter().enumerate().skip(8).all(|(k, e)| {
        let x = k as f32 / (LUT_N - 1) as f32;
        let v = x.powf(g);
        (0..3).all(|c| (e[c] - v).abs() < 0.002)
    });
    ok.then_some(g)
}

pub fn from_icc(icc: &[u8]) -> Option<Icc> {
    let p = open(icc)?;
    let space = unsafe { cmsGetColorSpace(p.0) };
    let name = icc_description(icc).unwrap_or_else(|| "ICC profile".into());
    let shaper = unsafe { cmsIsMatrixShaper(p.0) } != 0;
    if space == SIG_RGB && shaper {
        let (r, g, b) = (tag_xyz(&p, TAG_RXYZ)?, tag_xyz(&p, TAG_GXYZ)?, tag_xyz(&p, TAG_BXYZ)?);
        let m: M3 = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let to709 = to_f32(mul(&xyz_to_709(), &mul(&adapt(D50_XYZ, xy_to_xyz(D65[0], D65[1])), &m)));
        let lut = trc_lut(&p, [TAG_RTRC, TAG_GTRC, TAG_BTRC])?;
        let (transfer, lut) = if lut_is_srgb(&lut) {
            (Transfer::Srgb, None)
        } else if let Some(g) = lut_gamma(&lut) {
            (Transfer::Gamma(g), None)
        } else {
            (Transfer::Lut, Some(lut))
        };
        return Some(Icc::Shader(Color { transfer, to709, lum: Lum::Relative, lut, name }));
    }
    if space == SIG_GRAY {
        if let Some(lut) = trc_lut(&p, [TAG_KTRC, TAG_KTRC, TAG_KTRC]) {
            let transfer = if lut_is_srgb(&lut) { Transfer::Srgb } else { Transfer::Lut };
            let lut = (transfer == Transfer::Lut).then_some(lut);
            return Some(Icc::Shader(Color { transfer, to709: IDENTITY, lum: Lum::Relative, lut, name }));
        }
    }
    Some(Icc::Cmm { name, space })
}

/// Linear Rec.709 (scRGB) profile used as the CMM output.
fn linear_709_profile() -> Option<Profile> {
    unsafe {
        let white = CieXyY { x: D65[0], y: D65[1], yy: 1.0 };
        let prim = CieXyYTriple {
            r: CieXyY { x: 0.64, y: 0.33, yy: 1.0 },
            g: CieXyY { x: 0.30, y: 0.60, yy: 1.0 },
            b: CieXyY { x: 0.15, y: 0.06, yy: 1.0 },
        };
        let g = cmsBuildGamma(std::ptr::null_mut(), 1.0);
        let curves = [g, g, g];
        let p = cmsCreateRGBProfile(&white, &prim, curves.as_ptr());
        cmsFreeToneCurve(g);
        (!p.is_null()).then_some(Profile(p))
    }
}

/// Convert pixels through a full ICC transform to linear Rec.709 floats (RGBA).
/// `in_fmt` is an lcms pixel type (`TYPE_RGBA_8`, `TYPE_CMYK_8`, ...), `bpp` bytes per input pixel.
pub fn cmm_to_linear(icc: &[u8], input: &[u8], in_fmt: u32, bpp: usize, count: usize) -> Option<Vec<f32>> {
    let src = open(icc)?;
    let dst = linear_709_profile()?;
    let has_alpha = in_fmt == TYPE_RGBA_8 || in_fmt == TYPE_RGBA_16;
    let flags = FLAGS_NOCACHE | if has_alpha { FLAGS_COPY_ALPHA } else { 0 };
    let t = unsafe { cmsCreateTransform(src.0, in_fmt, dst.0, TYPE_RGBA_FLT, 0, flags) };
    if t.is_null() {
        return None;
    }
    let mut out = vec![1.0f32; count * 4];
    use rayon::prelude::*;
    let chunk = 64 * 1024;
    let tp = t as usize;
    out.par_chunks_mut(chunk * 4).enumerate().for_each(|(i, o)| {
        let n = o.len() / 4;
        let start = i * chunk * bpp;
        let src = &input[start..start + n * bpp];
        unsafe { cmsDoTransform(tp as *mut c_void, src.as_ptr() as _, o.as_mut_ptr() as _, n as u32) };
        if !has_alpha {
            for px in o.chunks_exact_mut(4) {
                px[3] = 1.0;
            }
        }
    });
    unsafe { cmsDeleteTransform(t) };
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p3_to_709_maps_white_to_white() {
        let m = to709_from_xy(cicp_primaries(12).unwrap().0);
        let w: Vec<f32> = (0..3).map(|i| m[i].iter().sum()).collect();
        for v in w {
            assert!((v - 1.0).abs() < 1e-3);
        }
        // P3 red is outside Rec.709: negative green/blue components.
        assert!(m[1][0] < 0.0 && m[0][0] > 1.0);
    }

    #[test]
    fn bt2020_red_matrix() {
        let m = to709_from_xy(cicp_primaries(9).unwrap().0);
        // Known BT.2020 → BT.709 conversion: first row ≈ [1.6605, -0.5876, -0.0728].
        assert!((m[0][0] - 1.6605).abs() < 0.002, "{}", m[0][0]);
        assert!((m[0][1] + 0.5876).abs() < 0.002);
    }
}
