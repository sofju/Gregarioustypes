//! Wrappers over the native shim decoders.

use super::{apply_color, apply_exif, cmyk_to_image, DecodeOptions};
use crate::color::Transfer;
use crate::ffi::*;
use crate::{err, gainmap, util, Frame, GainMap, GainMapKind, Image, Pixels, Result};
use std::path::Path;

fn pixels_of(g: &GtImg) -> Option<Pixels> {
    if g.pixels.is_null() || g.width <= 0 || g.height <= 0 {
        return None;
    }
    let (w, h, stride) = (g.width as usize, g.height as usize, g.stride as usize);
    let row = |y: usize, bytes: usize| unsafe { std::slice::from_raw_parts(g.pixels.add(y * stride), bytes) };
    Some(match g.format {
        GT_RGBA8 | GT_CMYK8 => {
            let mut v = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                v.extend_from_slice(row(y, w * 4));
            }
            Pixels::Rgba8(v)
        }
        GT_RGBA16 | GT_RGBAF16 => {
            let mut v: Vec<u16> = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                let r = row(y, w * 8);
                v.extend(r.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])));
            }
            if g.format == GT_RGBA16 {
                Pixels::Rgba16(v)
            } else {
                Pixels::RgbaF16(v)
            }
        }
        GT_RGBAF32 => {
            let mut f: Vec<f32> = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                let r = row(y, w * 16);
                f.extend(r.chunks_exact(4).map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]])));
            }
            Pixels::RgbaF16(util::f32_to_f16(&f))
        }
        _ => return None,
    })
}

/// Convert a native result into an Image (pixels, colour, metadata, gain map).
fn convert(n: &Native, format: &'static str, orientation_from_exif: bool) -> Result<Image> {
    let g = &*n.0;
    let Some(px) = pixels_of(g) else { return err(format!("{format}: no pixels")) };
    let mut img = Image::new(g.width as u32, g.height as u32, Pixels::Rgba8(Vec::new()), format);
    img.alpha = g.has_alpha != 0;
    img.bit_depth = g.bit_depth.max(1) as u32;
    img.orientation = g.orientation.clamp(1, 8) as u8;
    img.max_cll = g.max_cll;
    img.mastering_max = g.mastering_max;
    img.frame_count = g.frames.max(1) as u32;
    img.first_delay_ms = g.delay_ms.max(0) as u32;
    if g.full_width > 0 && (g.full_width != g.width || g.full_height != g.height) {
        img.full_size = Some((g.full_width as u32, g.full_height as u32));
    }
    let icc = Native::bytes(g.icc, g.icc_len);
    let exif = Native::bytes(g.exif, g.exif_len);
    if g.format == GT_CMYK8 {
        if let Pixels::Rgba8(v) = &px {
            cmyk_to_image(&mut img, v, icc.as_deref(), false);
        }
    } else {
        img.pixels = px;
        let cicp = (g.cicp_primaries >= 0 || g.cicp_transfer >= 0).then_some((g.cicp_primaries.max(1), g.cicp_transfer.max(2)));
        let prim = (g.has_prim != 0).then_some(g.prim);
        apply_color(&mut img, icc.as_deref(), cicp, prim, g.gamma);
    }
    img.hdr_float = matches!(g.format, GT_RGBAF16 | GT_RGBAF32) && img.color.transfer == Transfer::Linear;
    let ex = apply_exif(&mut img, exif.as_deref(), orientation_from_exif);
    // Gain map.
    if !g.gainmap.is_null() {
        let gm = unsafe { &*g.gainmap };
        if let Some(gp) = pixels_of(gm) {
            let (kind, transfer, label) = if g.gm_kind == GT_GM_APPLE {
                let headroom = ex.as_ref().and_then(|e| Some(gainmap::apple_headroom(e.apple_33?, e.apple_48?))).unwrap_or(4.0);
                (Some(GainMapKind::Apple { headroom }), Transfer::Srgb, "Apple HDR")
            } else {
                (
                    Some(GainMapKind::Iso {
                        min: g.gm_min,
                        max: g.gm_max,
                        gamma: g.gm_gamma,
                        base_offset: g.gm_base_offset,
                        alt_offset: g.gm_alt_offset,
                        base_headroom: g.gm_base_headroom,
                        alt_headroom: g.gm_alt_headroom,
                    }),
                    Transfer::Linear,
                    "ISO 21496-1",
                )
            };
            if let Some(kind) = kind {
                img.gain_map = Some(GainMap { width: gm.width as u32, height: gm.height as u32, pixels: gp, kind, transfer, label });
            }
        }
    }
    Ok(img)
}

fn run(f: impl FnOnce(*mut GtImg) -> i32, name: &str) -> Result<Native> {
    let mut n = Native::zeroed();
    if f(n.ptr()) != 0 {
        let e = n.error();
        return err(if e.is_empty() { format!("{name}: decode failed") } else { e });
    }
    Ok(n)
}

pub fn avif(d: &[u8]) -> Result<Image> {
    let n = run(|o| unsafe { gt_avif_decode(d.as_ptr(), d.len(), o) }, "avif")?;
    convert(&n, "AVIF", false)
}

pub fn heif(d: &[u8]) -> Result<Image> {
    let n = run(|o| unsafe { gt_heif_decode(d.as_ptr(), d.len(), o) }, "heif")?;
    convert(&n, "HEIF", false)
}

pub fn jxl(d: &[u8]) -> Result<Image> {
    let n = run(|o| unsafe { gt_jxl_decode(d.as_ptr(), d.len(), o) }, "jxl")?;
    convert(&n, "JPEG XL", false)
}

pub fn tiff(path: &Path) -> Result<Image> {
    let wide: Vec<u16> = path.as_os_str().encode_wide_z();
    let n = run(|o| unsafe { gt_tiff_decode(wide.as_ptr(), o) }, "tiff")?;
    convert(&n, "TIFF", false)
}

pub fn exr(path: &Path) -> Result<Image> {
    let p = std::ffi::CString::new(path.to_string_lossy().as_bytes()).map_err(|e| crate::DecodeError(e.to_string()))?;
    let n = run(|o| unsafe { gt_exr_decode(p.as_ptr(), o) }, "exr")?;
    convert(&n, "OpenEXR", false)
}

pub fn j2k(d: &[u8], opts: &DecodeOptions) -> Result<Image> {
    let (mw, mh) = if opts.full { (0, 0) } else { (opts.target_w as i32, opts.target_h as i32) };
    let n = run(|o| unsafe { gt_j2k_decode(d.as_ptr(), d.len(), mw, mh, o) }, "jpeg2000")?;
    convert(&n, "JPEG 2000", false)
}

pub fn webp(d: &[u8], opts: &DecodeOptions) -> Result<Image> {
    let n = run(|o| unsafe { gt_webp_decode(d.as_ptr(), d.len(), o) }, "webp")?;
    let mut img = convert(&n, "WebP", true)?;
    if img.frame_count > 1 {
        // Decode the remaining frames up to the memory budget.
        let (mut w, mut h, mut frames, mut loops) = (0, 0, 0, 0);
        let a = unsafe { gt_webp_anim_open(d.as_ptr(), d.len(), &mut w, &mut h, &mut frames, &mut loops) };
        if !a.is_null() {
            let size = (w * h * 4) as usize;
            let mut buf = vec![0u8; size];
            let mut ts = 0;
            let mut prev_ts = 0;
            let mut first = true;
            let mut used = size;
            while unsafe { gt_webp_anim_next(a, buf.as_mut_ptr(), &mut ts) } != 0 {
                let delay = (ts - prev_ts).max(10) as u32;
                prev_ts = ts;
                if first {
                    img.first_delay_ms = delay;
                    first = false;
                    continue;
                }
                used += size;
                if used > opts.anim_budget {
                    break;
                }
                img.frames.push(Frame { pixels: Pixels::Rgba8(buf.clone()), delay_ms: delay });
            }
            unsafe { gt_webp_anim_close(a) };
        }
    }
    Ok(img)
}

/// Camera raw: the embedded preview for a fast first stage, LibRaw processing when
/// `opts.full` (or when there is no usable preview).
pub fn raw(d: &[u8], opts: &DecodeOptions) -> Result<Image> {
    if !opts.full {
        let mut bm = Native::zeroed();
        let (mut jpeg, mut jlen, mut rw, mut rh, mut flip) = (std::ptr::null_mut(), 0usize, 0, 0, 1);
        let kind = unsafe { gt_raw_thumb(d.as_ptr(), d.len(), bm.ptr(), &mut jpeg, &mut jlen, &mut rw, &mut rh, &mut flip) };
        let preview = if kind == 1 && !jpeg.is_null() {
            let bytes = unsafe { std::slice::from_raw_parts(jpeg, jlen) }.to_vec();
            unsafe { gt_free_buf(jpeg as _) };
            super::jpeg::decode(&bytes, opts).ok()
        } else if kind == 2 {
            convert(&bm, "Camera raw", false).ok()
        } else {
            None
        };
        if let Some(mut p) = preview {
            // Use the preview when it is reasonably large; the full decode comes later.
            if p.width.max(p.height) >= 1024 || p.width as i32 * 2 >= rw {
                p.orientation = flip.clamp(1, 8) as u8;
                p.format = "Camera raw (embedded preview)";
                p.full_size = Some((rw.max(1) as u32, rh.max(1) as u32));
                p.facts.retain(|(k, _)| k != "Taken");
                return Ok(p);
            }
        }
    }
    // A half-size demosaic still covers screens up to ~4K and is 4× faster.
    let half = opts.target_w.max(opts.target_h) <= 4000;
    let n = run(|o| unsafe { gt_raw_process(d.as_ptr(), d.len(), half as i32, o) }, "raw")?;
    let mut img = convert(&n, "Camera raw", false)?;
    if half {
        img.full_size = img.full_size.or(Some((img.width * 2, img.height * 2)));
    } else {
        img.full_size = None;
    }
    Ok(img)
}

trait EncodeWideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}

impl EncodeWideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}
