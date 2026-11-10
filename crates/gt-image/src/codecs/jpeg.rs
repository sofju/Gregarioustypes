//! JPEG via libjpeg-turbo (scaled DCT decode), with ICC/EXIF/XMP, Adobe CMYK, and
//! Ultra HDR / ISO 21496-1 gain maps found through the MPF index.

use super::{apply_color, apply_exif, cmyk_to_image, DecodeOptions};
use crate::color::Transfer;
use crate::ffi::*;
use crate::{err, gainmap, GainMap, GainMapKind, Image, Pixels, Result};

#[derive(Default)]
struct Markers {
    icc: Vec<(u8, Vec<u8>)>,
    exif: Option<Vec<u8>>,
    xmp: Option<String>,
    /// (TIFF header position, [(size, offset)])
    mpf: Option<(usize, Vec<(u32, u32)>)>,
    iso: Option<Vec<u8>>,
    adobe: bool,
}

fn be16(d: &[u8], i: usize) -> Option<usize> {
    d.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
}

fn markers(d: &[u8]) -> Markers {
    let mut m = Markers::default();
    let mut i = 2;
    while i + 4 <= d.len() && d[i] == 0xFF {
        let marker = d[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if (0xD0..=0xD8).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let Some(len) = be16(d, i + 2) else { break };
        let start = i + 4;
        let end = (i + 2 + len).min(d.len());
        if start > end {
            break;
        }
        let p = &d[start..end];
        match marker {
            0xE1 if p.starts_with(b"Exif\0\0") => m.exif = Some(p.to_vec()),
            0xE1 if p.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => m.xmp = Some(String::from_utf8_lossy(&p[29..]).into_owned()),
            0xE2 if p.starts_with(b"ICC_PROFILE\0") && p.len() > 14 => m.icc.push((p[12], p[14..].to_vec())),
            0xE2 if p.starts_with(b"MPF\0") => m.mpf = parse_mpf(d, start + 4),
            0xE2 if p.starts_with(b"urn:iso:std:iso:ts:21496:-1\0") => m.iso = Some(p[28..].to_vec()),
            0xEE if p.starts_with(b"Adobe") => m.adobe = true,
            _ => {}
        }
        i = end;
    }
    m
}

/// MPF index (CIPA DC-007): image sizes and offsets relative to the MPF TIFF header.
fn parse_mpf(d: &[u8], base: usize) -> Option<(usize, Vec<(u32, u32)>)> {
    let t = d.get(base..)?;
    let le = match t.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_ = |o: usize| t.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_ = |o: usize| t.get(o..o + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
    let ifd = u32_(4)? as usize;
    let n = u16_(ifd)? as usize;
    let mut entries = Vec::new();
    for k in 0..n.min(64) {
        let e = ifd + 2 + k * 12;
        if u16_(e)? == 0xB002 {
            let count = u32_(e + 4)? as usize / 16;
            let off = u32_(e + 8)? as usize;
            for j in 0..count.min(16) {
                let r = off + j * 16;
                entries.push((u32_(r + 4)?, u32_(r + 8)?));
            }
        }
    }
    (!entries.is_empty()).then_some((base, entries))
}

fn icc_of(m: &Markers) -> Option<Vec<u8>> {
    if m.icc.is_empty() {
        return None;
    }
    let mut parts = m.icc.clone();
    parts.sort_by_key(|(s, _)| *s);
    Some(parts.into_iter().flat_map(|(_, b)| b).collect())
}

fn scale_for(w: i32, h: i32, opts: &DecodeOptions) -> i32 {
    if opts.full {
        return 1;
    }
    // Compare against the target in either orientation.
    let (tw, th) = (opts.target_w.max(opts.target_h) as i32, opts.target_w.min(opts.target_h) as i32);
    let (big, small) = (w.max(h), w.min(h));
    for d in [8, 4, 2] {
        if (big + d - 1) / d >= tw && (small + d - 1) / d >= th {
            return d;
        }
    }
    1
}

fn decode_raw(d: &[u8], denom: i32) -> Result<Native> {
    let mut n = Native::zeroed();
    if unsafe { gt_jpeg_decode(d.as_ptr(), d.len(), denom, n.ptr()) } != 0 {
        return err(n.error());
    }
    Ok(n)
}

pub fn decode(d: &[u8], opts: &DecodeOptions) -> Result<Image> {
    let m = markers(d);
    let (mut w, mut h, mut cmyk) = (0, 0, 0);
    if unsafe { gt_jpeg_header(d.as_ptr(), d.len(), &mut w, &mut h, &mut cmyk) } != 0 {
        return err("jpeg: invalid header");
    }
    let denom = scale_for(w, h, opts);
    let n = decode_raw(d, denom)?;
    let g = &*n.0;
    let px = unsafe { std::slice::from_raw_parts(g.pixels, (g.stride * g.height) as usize) }.to_vec();
    let mut img = Image::new(g.width as u32, g.height as u32, Pixels::Rgba8(Vec::new()), "JPEG");
    if denom > 1 {
        img.full_size = Some((w as u32, h as u32));
    }
    let icc = icc_of(&m);
    if g.format == GT_CMYK8 {
        cmyk_to_image(&mut img, &px, icc.as_deref(), m.adobe);
    } else {
        img.pixels = Pixels::Rgba8(px);
        apply_color(&mut img, icc.as_deref(), None, None, 0.0);
    }
    apply_exif(&mut img, m.exif.as_deref(), true);
    if let Some(gm) = gain_map(d, &m, denom) {
        img.gain_map = Some(gm);
    }
    Ok(img)
}

/// The secondary MPF image is a gain map when it carries hdrgm XMP or ISO metadata.
fn gain_map(d: &[u8], m: &Markers, denom: i32) -> Option<GainMap> {
    let (base, entries) = m.mpf.as_ref()?;
    for &(size, offset) in entries.iter().skip(1) {
        let start = base + offset as usize;
        let bytes = d.get(start..start + size as usize)?;
        if !bytes.starts_with(&[0xFF, 0xD8]) {
            continue;
        }
        let gm = markers(bytes);
        let kind: Option<GainMapKind> = gm.iso.as_deref().and_then(gainmap::parse_iso21496).or_else(|| gm.xmp.as_deref().and_then(gainmap::parse_hdrgm_xmp));
        let Some(kind) = kind else { continue };
        // Keep the gain map's resolution proportional to the base decode.
        let n = decode_raw(bytes, if denom > 2 { denom / 2 } else { 1 }).ok()?;
        let g = &*n.0;
        let px = unsafe { std::slice::from_raw_parts(g.pixels, (g.stride * g.height) as usize) }.to_vec();
        return Some(GainMap { width: g.width as u32, height: g.height as u32, pixels: Pixels::Rgba8(px), kind, transfer: Transfer::Linear, label: "Ultra HDR" });
    }
    None
}
