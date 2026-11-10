//! Format detection and dispatch.

mod gif;
mod icns;
mod jpeg;
mod native;
mod png;
mod psd;
mod simple;
mod wic;

use crate::color::{self, Color, Icc};
use crate::ffi::{TYPE_CMYK_8, TYPE_CMYK_8_REV, TYPE_RGBA_16, TYPE_RGBA_8};
use crate::{util, Image, Pixels, Result};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Ico,
    Tga,
    Hdr,
    Tiff,
    WebP,
    Avif,
    Heif,
    Jxl,
    Jxr,
    Exr,
    J2k,
    Icns,
    Raw,
    Psd,
}

pub const RAW_EXTS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "3fr", "erf", "kdc", "mrw", "x3f", "iiq", "mos", "rwl", "dcr",
];

/// Extensions handled by the image provider.
pub const EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "bmp", "dib", "ico", "cur", "tga", "icb", "vda", "vst", "hdr", "rgbe", "tif", "tiff", "webp", "avif", "avifs",
    "heic", "heif", "hif", "heics", "heifs", "avci", "jxl", "jxr", "wdp", "hdp", "exr", "j2k", "jp2", "jpx", "j2c", "jpf", "jpc", "jpm", "icns", "psd", "psb", "pdd",
];

fn ftyp_brands(h: &[u8]) -> Vec<[u8; 4]> {
    if h.len() < 16 || &h[4..8] != b"ftyp" {
        return Vec::new();
    }
    let size = (u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize).min(h.len()).max(16);
    let mut v = vec![[h[8], h[9], h[10], h[11]]];
    let mut i = 16;
    while i + 4 <= size {
        v.push([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        i += 4;
    }
    v
}

/// Identify an image by magic bytes (and extension where there is no magic).
/// Returns the kind and a confidence score for the provider registry.
pub fn detect(head: &[u8], ext: &str) -> Option<(Kind, u32)> {
    let by_ext = |k: Kind| (k, 95);
    let h = head;
    let magic = if h.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if h.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Kind::Png)
    } else if h.starts_with(b"GIF87a") || h.starts_with(b"GIF89a") {
        Some(Kind::Gif)
    } else if h.len() > 12 && &h[0..4] == b"RIFF" && &h[8..12] == b"WEBP" {
        Some(Kind::WebP)
    } else if h.starts_with(&[0xFF, 0x0A]) || h.starts_with(&[0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A]) {
        Some(Kind::Jxl)
    } else if h.starts_with(&[0x49, 0x49, 0xBC]) {
        Some(Kind::Jxr)
    } else if h.starts_with(&[0x76, 0x2F, 0x31, 0x01]) {
        Some(Kind::Exr)
    } else if h.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) || h.starts_with(&[0, 0, 0, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A]) {
        Some(Kind::J2k)
    } else if h.starts_with(b"8BPS") && h.len() > 5 && (h[5] == 1 || h[5] == 2) {
        Some(Kind::Psd)
    } else if h.starts_with(b"icns") {
        Some(Kind::Icns)
    } else if h.starts_with(b"#?RADIANCE") || h.starts_with(b"#?RGBE") {
        Some(Kind::Hdr)
    } else if h.starts_with(b"BM") && matches!(ext, "bmp" | "dib") {
        Some(Kind::Bmp)
    } else if (h.starts_with(&[0, 0, 1, 0]) || h.starts_with(&[0, 0, 2, 0])) && matches!(ext, "ico" | "cur") {
        Some(Kind::Ico)
    } else if h.starts_with(b"II*\0") || h.starts_with(b"MM\0*") || h.starts_with(b"II+\0") || h.starts_with(b"MM\0+") {
        Some(if RAW_EXTS.contains(&ext) { Kind::Raw } else { Kind::Tiff })
    } else {
        let brands = ftyp_brands(h);
        if brands.iter().any(|b| b == b"avif" || b == b"avis") {
            Some(Kind::Avif)
        } else if brands.iter().any(|b| b == b"crx ") {
            Some(Kind::Raw)
        } else if brands.iter().any(|b| matches!(b, b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs" | b"mif1" | b"msf1" | b"avci")) {
            Some(Kind::Heif)
        } else {
            None
        }
    };
    if let Some(k) = magic {
        return Some((k, if EXTS.contains(&ext) || RAW_EXTS.contains(&ext) { 95 } else { 85 }));
    }
    match ext {
        "tga" | "icb" | "vda" | "vst" => Some((Kind::Tga, 60)),
        _ if RAW_EXTS.contains(&ext) => Some(by_ext(Kind::Raw)),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DecodeOptions {
    /// Display size the image must at least cover (for reduced decodes).
    pub target_w: u32,
    pub target_h: u32,
    /// Decode at full resolution even when a reduced decode would do.
    pub full: bool,
    /// Memory budget for animation frames, in bytes.
    pub anim_budget: usize,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self { target_w: 3840, target_h: 2160, full: false, anim_budget: 384 << 20 }
    }
}

pub fn decode(path: &Path, kind: Kind, opts: &DecodeOptions) -> Result<Image> {
    let read = || std::fs::read(path).map_err(|e| crate::DecodeError(e.to_string()));
    let mut img = match kind {
        Kind::Jpeg => jpeg::decode(&read()?, opts)?,
        Kind::Png => png::decode(&read()?, opts)?,
        Kind::Gif => gif::decode(&read()?, opts)?,
        Kind::Bmp | Kind::Ico | Kind::Tga | Kind::Hdr => simple::decode(&read()?, kind)?,
        Kind::Icns => icns::decode(&read()?)?,
        Kind::Jxr => wic::decode(path)?,
        Kind::Tiff => native::tiff(path)?,
        Kind::Exr => native::exr(path)?,
        Kind::WebP => native::webp(&read()?, opts)?,
        Kind::Avif => native::avif(&read()?)?,
        Kind::Heif => native::heif(&read()?)?,
        Kind::Jxl => native::jxl(&read()?)?,
        Kind::J2k => native::j2k(&read()?, opts)?,
        Kind::Raw => native::raw(&read()?, opts)?,
        Kind::Psd => psd::decode(&read()?)?,
    };
    util::fit_texture_limit(&mut img, 16384);
    Ok(img)
}

/// Decode from memory (textures embedded in 3D files). Formats whose decoders
/// need a file go through a temporary file.
pub fn decode_bytes(d: &[u8], kind: Kind, opts: &DecodeOptions) -> Result<Image> {
    let mut img = match kind {
        Kind::Jpeg => jpeg::decode(d, opts)?,
        Kind::Png => png::decode(d, opts)?,
        Kind::Gif => gif::decode(d, opts)?,
        Kind::Bmp | Kind::Ico | Kind::Tga | Kind::Hdr => simple::decode(d, kind)?,
        Kind::Icns => icns::decode(d)?,
        Kind::WebP => native::webp(d, opts)?,
        Kind::Avif => native::avif(d)?,
        Kind::Heif => native::heif(d)?,
        Kind::Jxl => native::jxl(d)?,
        Kind::J2k => native::j2k(d, opts)?,
        Kind::Raw => native::raw(d, opts)?,
        Kind::Psd => psd::decode(d)?,
        Kind::Jxr | Kind::Tiff | Kind::Exr => {
            let tmp = std::env::temp_dir().join(format!("gt-tex-{}-{:p}.bin", std::process::id(), d.as_ptr()));
            std::fs::write(&tmp, d).map_err(|e| crate::DecodeError(e.to_string()))?;
            let r = decode(&tmp, kind, opts);
            let _ = std::fs::remove_file(&tmp);
            return r;
        }
    };
    util::fit_texture_limit(&mut img, 16384);
    Ok(img)
}

/// Apply embedded colour information: CICP wins for PQ/HLG, then ICC, then CICP,
/// then sRGB. ICC profiles the shader can't express are converted on the CPU.
pub(crate) fn apply_color(img: &mut Image, icc: Option<&[u8]>, cicp: Option<(i32, i32)>, prim: Option<[f32; 8]>, gamma: f32) {
    let hdr_cicp = matches!(cicp, Some((_, 16 | 18)));
    if let (Some(icc), false) = (icc.filter(|b| b.len() > 128), hdr_cicp) {
        match color::from_icc(icc) {
            Some(Icc::Shader(c)) => {
                img.color = c;
                return;
            }
            Some(Icc::Cmm { name, .. }) => {
                let n = (img.width * img.height) as usize;
                let converted = match &img.pixels {
                    Pixels::Rgba8(v) => color::cmm_to_linear(icc, v, TYPE_RGBA_8, 4, n),
                    Pixels::Rgba16(v) => color::cmm_to_linear(icc, img.pixels.as_bytes(), TYPE_RGBA_16, 8, n).filter(|_| !v.is_empty()),
                    Pixels::RgbaF16(_) => None,
                };
                if let Some(f) = converted {
                    img.pixels = Pixels::RgbaF16(util::f32_to_f16(&f));
                    img.color = Color::linear_709(&name);
                    return;
                }
            }
            None => {}
        }
    }
    match cicp {
        Some((cp, tc)) if cp >= 0 && tc >= 0 && !(cp == 2 && tc == 2) => {
            let cp = if cp == 2 { 1 } else { cp };
            img.color = color::from_cicp(cp, tc, gamma, prim);
        }
        _ => {
            if let Some(p) = prim {
                img.color = color::from_cicp(1, 13, gamma, Some(p));
            }
        }
    }
}

/// CMYK pixels → RGBA: through the embedded profile when present, else naively.
pub(crate) fn cmyk_to_image(img: &mut Image, cmyk: &[u8], icc: Option<&[u8]>, inverted: bool) {
    let n = (img.width * img.height) as usize;
    if let Some(icc) = icc {
        if let Some(Icc::Cmm { name, space }) = color::from_icc(icc) {
            if space == crate::ffi::SIG_CMYK {
                let fmt = if inverted { TYPE_CMYK_8_REV } else { TYPE_CMYK_8 };
                if let Some(f) = color::cmm_to_linear(icc, cmyk, fmt, 4, n) {
                    img.pixels = Pixels::RgbaF16(util::f32_to_f16(&f));
                    img.color = Color::linear_709(&format!("{name} (CMYK)"));
                    return;
                }
            }
        }
    }
    img.pixels = Pixels::Rgba8(util::cmyk_naive(cmyk, inverted));
    img.color = Color::srgb();
    img.color.name = "CMYK (no profile)".into();
}

/// EXIF facts and orientation (when the decoder hasn't applied orientation itself).
pub(crate) fn apply_exif(img: &mut Image, exif: Option<&[u8]>, take_orientation: bool) -> Option<crate::exif::Exif> {
    let e = crate::exif::parse(exif?)?;
    if take_orientation {
        if let Some(o) = e.orientation {
            img.orientation = o;
        }
    }
    img.facts.extend(e.facts());
    Some(e)
}
