//! Photoshop PSD / PSB: the merged composite every Photoshop file stores
//! (8/16/32-bit, RGB, grayscale, CMYK, Lab, indexed, bitmap, duotone,
//! multichannel; raw, PackBits or ZIP), with the embedded ICC profile.
//!
//! Files saved without "Maximize Compatibility" hold no real composite: the
//! visible pixel layers are then composited here (normal blending, opacity,
//! group visibility; effects, masks and adjustment layers are not applied), and
//! as a last resort the stored thumbnail is shown.

use super::cmyk_to_image;
use crate::color::Color;
use crate::{util, DecodeError, Image, Pixels, Result};

fn err(s: &str) -> DecodeError {
    DecodeError(format!("PSD: {s}"))
}

struct R<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn need(&self, n: usize) -> Result<()> {
        if self.p + n > self.d.len() {
            Err(err("truncated file"))
        } else {
            Ok(())
        }
    }
    fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        self.p += 1;
        Ok(self.d[self.p - 1])
    }
    fn u16(&mut self) -> Result<u16> {
        self.need(2)?;
        self.p += 2;
        Ok(u16::from_be_bytes([self.d[self.p - 2], self.d[self.p - 1]]))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32> {
        self.need(4)?;
        self.p += 4;
        Ok(u32::from_be_bytes(self.d[self.p - 4..self.p].try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64> {
        self.need(8)?;
        self.p += 8;
        Ok(u64::from_be_bytes(self.d[self.p - 8..self.p].try_into().unwrap()))
    }
    /// Section length: 4 bytes in PSD, 8 in PSB.
    fn len(&mut self, psb: bool) -> Result<usize> {
        Ok(if psb { self.u64()? as usize } else { self.u32()? as usize })
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.need(n)?;
        self.p += n;
        Ok(&self.d[self.p - n..self.p])
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.need(n)?;
        self.p += n;
        Ok(())
    }
}

#[derive(Default)]
struct Resources<'a> {
    icc: Option<&'a [u8]>,
    thumbnail: Option<&'a [u8]>,
    /// From the version-info resource: false when "Maximize Compatibility" was off.
    real_merged: Option<bool>,
    dpi: Option<f32>,
    exif: Option<&'a [u8]>,
    /// Alpha/spot channel display info (colour, solidity, kind 2 = spot).
    display: Vec<([u8; 3], f32, u8)>,
}

fn resources<'a>(d: &'a [u8]) -> Resources<'a> {
    let mut res = Resources::default();
    let mut r = R { d, p: 0 };
    while r.p + 12 <= d.len() {
        let Ok(sig) = r.bytes(4) else { break };
        if sig != b"8BIM" && sig != b"MeSa" && sig != b"AgHg" && sig != b"PHUT" && sig != b"DCSR" {
            break;
        }
        let (Ok(id), Ok(nlen)) = (r.u16(), r.u8()) else { break };
        // Pascal name padded to an even total (length byte included).
        let pad = if (nlen as usize + 1) % 2 == 1 { 1 } else { 0 };
        if r.skip(nlen as usize + pad).is_err() {
            break;
        }
        let Ok(size) = r.u32() else { break };
        let Ok(data) = r.bytes(size as usize) else { break };
        if size % 2 == 1 {
            let _ = r.skip(1);
        }
        match id {
            1039 => res.icc = Some(data),
            1036 if data.len() > 28 => res.thumbnail = Some(&data[28..]),
            1057 if data.len() >= 5 => res.real_merged = Some(data[4] != 0),
            1005 if data.len() >= 4 => res.dpi = Some(u32::from_be_bytes(data[0..4].try_into().unwrap()) as f32 / 65536.0),
            1058 => res.exif = Some(data),
            // DisplayInfo: per extra channel colour space, colour, opacity, kind.
            1077 if data.len() >= 4 => res.display = display_info(&data[4..], 13),
            1007 if res.display.is_empty() => res.display = display_info(data, 14),
            _ => {}
        }
    }
    res
}

fn display_info(d: &[u8], stride: usize) -> Vec<([u8; 3], f32, u8)> {
    d.chunks_exact(stride)
        .map(|c| {
            let space = u16::from_be_bytes([c[0], c[1]]);
            let v = |i: usize| u16::from_be_bytes([c[2 + 2 * i], c[3 + 2 * i]]);
            let rgb = match space {
                0 => [(v(0) >> 8) as u8, (v(1) >> 8) as u8, (v(2) >> 8) as u8],
                2 => {
                    // CMYK components: 0 = 100 % ink.
                    let k = |i: usize| 1.0 - v(i) as f32 / 65535.0;
                    let (c, m, y, kk) = (k(0), k(1), k(2), k(3));
                    [((1.0 - c) * (1.0 - kk) * 255.0) as u8, ((1.0 - m) * (1.0 - kk) * 255.0) as u8, ((1.0 - y) * (1.0 - kk) * 255.0) as u8]
                }
                7 => lab_to_srgb(v(0) as f32 / 100.0, v(1) as i16 as f32 / 100.0, v(2) as i16 as f32 / 100.0),
                8 => {
                    let g = (255.0 - v(0) as f32 / 10000.0 * 255.0) as u8;
                    [g, g, g]
                }
                _ => [0, 0, 0],
            };
            let solidity = (u16::from_be_bytes([c[10], c[11]]) as f32 / 100.0).clamp(0.0, 1.0);
            (rgb, solidity, c[12])
        })
        .collect()
}

/// Spot channels printed over the composite (multiply with the ink colour).
fn apply_spots(img: &mut Image, planes: &[Vec<u8>], first_extra: usize, display: &[([u8; 3], f32, u8)], depth: u16) {
    let n = (img.width * img.height) as usize;
    // `solidity` (0..1): 0 = transparent ink (multiplies what is under it),
    // 1 = opaque ink (covers it). Density comes from the channel.
    for (k, &(ink, solidity, kind)) in display.iter().enumerate() {
        let Some(p) = planes.get(first_extra + k) else { break };
        if kind != 2 {
            continue;
        }
        let dens = |i: usize| -> f32 {
            let v = match depth {
                16 => be16(p, i) as f32 / 65535.0,
                32 => 1.0 - be32f(p, i).clamp(0.0, 1.0),
                _ => p[i] as f32 / 255.0,
            };
            1.0 - v
        };
        let mix = |base: f32, ink: f32, d: f32| -> f32 { base * (1.0 - d) + d * (base * ink * (1.0 - solidity) + ink * solidity) };
        match &mut img.pixels {
            Pixels::Rgba8(v) => {
                for i in 0..n {
                    let d = dens(i);
                    if d <= 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        v[4 * i + c] = (mix(v[4 * i + c] as f32 / 255.0, ink[c] as f32 / 255.0, d) * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            Pixels::Rgba16(v) => {
                for i in 0..n {
                    let d = dens(i);
                    for c in 0..3 {
                        v[4 * i + c] = (mix(v[4 * i + c] as f32 / 65535.0, ink[c] as f32 / 255.0, d) * 65535.0).round().clamp(0.0, 65535.0) as u16;
                    }
                }
            }
            Pixels::RgbaF16(v) => {
                for i in 0..n {
                    let d = dens(i);
                    if d <= 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        let lin = crate::color::srgb_eotf(ink[c] as f32 / 255.0);
                        let x = mix(util::f16_to_f32(v[4 * i + c]), lin, d);
                        v[4 * i + c] = util::f32_to_f16(&[x])[0];
                    }
                }
            }
        }
    }
}

/// PackBits rows into `out` (exactly `out.len()` bytes).
fn packbits(src: &[u8], out: &mut [u8]) -> usize {
    let (mut i, mut o) = (0usize, 0usize);
    while i < src.len() && o < out.len() {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let c = (n as usize + 1).min(out.len() - o).min(src.len() - i);
            out[o..o + c].copy_from_slice(&src[i..i + c]);
            i += n as usize + 1;
            o += c;
        } else if n != -128 {
            let c = ((1 - n as isize) as usize).min(out.len() - o);
            if i < src.len() {
                out[o..o + c].fill(src[i]);
            }
            i += 1;
            o += c;
        }
    }
    i
}

/// Undo ZIP-with-prediction: per-row deltas (16-bit as words; 32-bit floats are
/// stored byte-planar per row, then delta-coded bytewise).
fn unpredict(buf: &mut [u8], w: usize, rows: usize, depth: u16) {
    match depth {
        8 => {
            for row in buf.chunks_mut(w).take(rows) {
                for x in 1..row.len() {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
            }
        }
        16 => {
            for row in buf.chunks_mut(w * 2).take(rows) {
                let mut prev = 0u16;
                for x in 0..row.len() / 2 {
                    let v = u16::from_be_bytes([row[2 * x], row[2 * x + 1]]).wrapping_add(prev);
                    row[2 * x..2 * x + 2].copy_from_slice(&v.to_be_bytes());
                    prev = v;
                }
            }
        }
        32 => {
            let mut tmp = vec![0u8; w * 4];
            for row in buf.chunks_mut(w * 4).take(rows) {
                for x in 1..row.len() {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
                // Bytes are grouped: all first bytes, all second bytes...
                for x in 0..w {
                    for b in 0..4 {
                        tmp[x * 4 + b] = row[b * w + x];
                    }
                }
                row.copy_from_slice(&tmp);
            }
        }
        _ => {}
    }
}

fn inflate(src: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut v = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(src, size.max(1)).map_err(|_| err("bad ZIP data"))?;
    v.resize(size, 0);
    Ok(v)
}

/// Decode `n` planes of w×h at `depth` bits (big-endian samples).
fn planes(r: &mut R, n: usize, w: usize, h: usize, depth: u16, psb: bool) -> Result<Vec<Vec<u8>>> {
    let row = (w * depth as usize).div_ceil(8);
    let plane = row * h;
    let comp = r.u16()?;
    let mut out = Vec::with_capacity(n);
    match comp {
        0 => {
            for _ in 0..n {
                let avail = (r.d.len() - r.p).min(plane);
                let mut p = r.bytes(avail)?.to_vec();
                p.resize(plane, 0);
                out.push(p);
            }
        }
        1 => {
            let mut counts = Vec::with_capacity(n * h);
            for _ in 0..n * h {
                counts.push(if psb { r.u32()? as usize } else { r.u16()? as usize });
            }
            for c in 0..n {
                let mut p = vec![0u8; plane];
                for y in 0..h {
                    let len = counts[c * h + y];
                    let src = r.bytes(len.min(r.d.len() - r.p))?;
                    packbits(src, &mut p[y * row..(y + 1) * row]);
                }
                out.push(p);
            }
        }
        2 | 3 => {
            // One zlib stream for all planes.
            let rest = &r.d[r.p..];
            let mut all = inflate(rest, plane * n)?;
            if comp == 3 {
                for p in all.chunks_mut(plane) {
                    unpredict(p, w, h, depth);
                }
            }
            out = all.chunks(plane).map(|c| c.to_vec()).collect();
        }
        _ => return Err(err("unknown compression")),
    }
    Ok(out)
}

fn be16(p: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([p[2 * i], p[2 * i + 1]])
}

fn be32f(p: &[u8], i: usize) -> f32 {
    f32::from_be_bytes(p[4 * i..4 * i + 4].try_into().unwrap())
}

/// CIE L*a*b* (D50) → sRGB 8-bit.
fn lab_to_srgb(l: f32, a: f32, b: f32) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let f = |t: f32| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    let (x, y, z) = (0.9642 * f(fx), f(fy), 0.8249 * f(fz));
    // XYZ (D50) → linear sRGB (Bradford-adapted to D65).
    let r = 3.1338561 * x - 1.6168667 * y - 0.4906146 * z;
    let g = -0.9787684 * x + 1.9161415 * y + 0.0334540 * z;
    let bl = 0.0719453 * x - 0.2289914 * y + 1.4052427 * z;
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0 + 0.5) as u8
    };
    [enc(r), enc(g), enc(bl)]
}

struct Header {
    psb: bool,
    w: usize,
    h: usize,
    depth: u16,
    mode: u16,
}

const MODES: &[&str] = &["Bitmap", "Grayscale", "Indexed", "RGB", "CMYK", "", "", "Multichannel", "Duotone", "Lab"];

/// Merged-image planes → Image.
fn build(hd: &Header, planes: &[Vec<u8>], palette: &[u8], icc: Option<&[u8]>, alpha_plane: Option<usize>) -> Result<Image> {
    let (w, h) = (hd.w, hd.h);
    let n = w * h;
    let plane = |i: usize| planes.get(i).map(|p| p.as_slice());
    let alpha8 = |i: usize| -> u8 {
        match (alpha_plane.and_then(plane), hd.depth) {
            (Some(a), 8) => a[i],
            (Some(a), 16) => (be16(a, i) >> 8) as u8,
            (Some(a), 32) => (be32f(a, i).clamp(0.0, 1.0) * 255.0) as u8,
            _ => 255,
        }
    };
    let format = if hd.psb { "Photoshop large document (PSB)" } else { "Photoshop (PSD)" };
    let mut img;
    match (hd.mode, hd.depth) {
        // Float: linear light, Photoshop's HDR mode.
        (3 | 1 | 9 | 7 | 8, 32) => {
            let gray = hd.mode != 3 || planes.len() < 3;
            let mut f = vec![0f32; n * 4];
            for i in 0..n {
                let (r, g, b) = if gray {
                    let v = be32f(plane(0).ok_or(err("no data"))?, i);
                    (v, v, v)
                } else {
                    (be32f(planes[0].as_slice(), i), be32f(planes[1].as_slice(), i), be32f(planes[2].as_slice(), i))
                };
                f[4 * i] = r;
                f[4 * i + 1] = g;
                f[4 * i + 2] = b;
                f[4 * i + 3] = alpha_plane.and_then(plane).map(|a| be32f(a, i).clamp(0.0, 1.0)).unwrap_or(1.0);
            }
            img = Image::new(w as u32, h as u32, Pixels::RgbaF16(util::f32_to_f16(&f)), format);
            img.color = Color::linear_709("Linear (32-bit HDR)");
            img.hdr_float = true;
            img.bit_depth = 32;
        }
        (3, 16) | (1 | 8 | 7, 16) => {
            let gray = hd.mode != 3 || planes.len() < 3;
            let mut v = vec![0u16; n * 4];
            for i in 0..n {
                let (r, g, b) = if gray {
                    let s = be16(plane(0).ok_or(err("no data"))?, i);
                    (s, s, s)
                } else {
                    (be16(&planes[0], i), be16(&planes[1], i), be16(&planes[2], i))
                };
                v[4 * i] = r;
                v[4 * i + 1] = g;
                v[4 * i + 2] = b;
                v[4 * i + 3] = alpha_plane.and_then(plane).map(|a| be16(a, i)).unwrap_or(0xFFFF);
            }
            img = Image::new(w as u32, h as u32, Pixels::Rgba16(v), format);
            img.bit_depth = 16;
        }
        (4, 8 | 16) => {
            let mut cmyk = vec![0u8; n * 4];
            for c in 0..4 {
                let p = plane(c).ok_or(err("missing CMYK plane"))?;
                for i in 0..n {
                    cmyk[4 * i + c] = if hd.depth == 16 { (be16(p, i) >> 8) as u8 } else { p[i] };
                }
            }
            img = Image::new(w as u32, h as u32, Pixels::Rgba8(Vec::new()), format);
            // PSD stores CMYK inverted (255 = no ink), like Adobe JPEGs.
            cmyk_to_image(&mut img, &cmyk, icc, true);
            if let (Some(_), Pixels::Rgba8(v)) = (alpha_plane, &mut img.pixels) {
                for i in 0..n {
                    v[4 * i + 3] = alpha8(i);
                }
            }
            img.bit_depth = hd.depth as u32;
            return Ok(img);
        }
        (9, 8 | 16) => {
            let mut v = vec![0u8; n * 4];
            for i in 0..n {
                let s = |c: usize| -> f32 {
                    let p = planes[c].as_slice();
                    if hd.depth == 16 { be16(p, i) as f32 / 65535.0 } else { p[i] as f32 / 255.0 }
                };
                let rgb = lab_to_srgb(s(0) * 100.0, s(1) * 255.0 - 128.0, s(2) * 255.0 - 128.0);
                v[4 * i..4 * i + 3].copy_from_slice(&rgb);
                v[4 * i + 3] = alpha8(i);
            }
            img = Image::new(w as u32, h as u32, Pixels::Rgba8(v), format);
            img.bit_depth = hd.depth as u32;
            return Ok(img);
        }
        (2, 8) => {
            let idx = plane(0).ok_or(err("no data"))?;
            let mut v = vec![0u8; n * 4];
            for i in 0..n {
                let k = idx[i] as usize;
                if palette.len() >= 768 {
                    v[4 * i] = palette[k];
                    v[4 * i + 1] = palette[256 + k];
                    v[4 * i + 2] = palette[512 + k];
                }
                v[4 * i + 3] = 255;
            }
            img = Image::new(w as u32, h as u32, Pixels::Rgba8(v), format);
            return Ok(img);
        }
        (0, 1) => {
            let bits = plane(0).ok_or(err("no data"))?;
            let row = w.div_ceil(8);
            let mut v = vec![0u8; n * 4];
            for y in 0..h {
                for x in 0..w {
                    let on = bits[y * row + x / 8] & (0x80 >> (x % 8)) != 0;
                    let c = if on { 0 } else { 255 };
                    let o = 4 * (y * w + x);
                    v[o..o + 3].fill(c);
                    v[o + 3] = 255;
                }
            }
            img = Image::new(w as u32, h as u32, Pixels::Rgba8(v), format);
            img.bit_depth = 1;
            return Ok(img);
        }
        (_, 8) => {
            let gray = hd.mode != 3 || planes.len() < 3;
            let mut v = vec![0u8; n * 4];
            for i in 0..n {
                if gray {
                    v[4 * i..4 * i + 3].fill(plane(0).ok_or(err("no data"))?[i]);
                } else {
                    v[4 * i] = planes[0][i];
                    v[4 * i + 1] = planes[1][i];
                    v[4 * i + 2] = planes[2][i];
                }
                v[4 * i + 3] = alpha8(i);
            }
            img = Image::new(w as u32, h as u32, Pixels::Rgba8(v), format);
        }
        _ => return Err(err(&format!("unsupported mode {} at {} bits", hd.mode, hd.depth))),
    }
    img.alpha = alpha_plane.is_some();
    if hd.depth != 32 {
        super::apply_color(&mut img, icc, None, None, 0.0);
    }
    Ok(img)
}

struct Layer {
    rect: (i32, i32, i32, i32),
    channels: Vec<(i16, usize)>,
    opacity: u8,
    visible: bool,
    /// lsct section type: 0 layer, 1/2 group start (open/closed), 3 group end.
    section: u32,
    effective: bool,
}

/// Layer records and their channel data offsets, from the layer-info block.
fn layers(r: &mut R, psb: bool) -> Result<(Vec<Layer>, Vec<usize>, bool)> {
    let mut list = Vec::new();
    let count = r.i16()?;
    let merged_alpha = count < 0;
    for _ in 0..count.unsigned_abs() {
        let (t, l, b, rt) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
        let nch = r.u16()? as usize;
        let mut channels = Vec::with_capacity(nch);
        for _ in 0..nch {
            let id = r.i16()?;
            let len = r.len(psb)?;
            channels.push((id, len));
        }
        r.skip(4)?; // 8BIM
        let _blend = r.bytes(4)?;
        let opacity = r.u8()?;
        let _clip = r.u8()?;
        let flags = r.u8()?;
        r.skip(1)?;
        let extra = r.u32()? as usize;
        let end = r.p + extra;
        let mut section = 0;
        // Mask and blending ranges, then the name, then tagged blocks.
        let ml = r.u32()? as usize;
        r.skip(ml)?;
        let bl = r.u32()? as usize;
        r.skip(bl)?;
        let nl = r.u8()? as usize;
        r.skip(((nl + 1).div_ceil(4) * 4) - 1)?;
        while r.p + 12 <= end {
            let sig = r.bytes(4)?;
            if sig != b"8BIM" && sig != b"8B64" {
                break;
            }
            let key = r.bytes(4)?;
            let long = psb && matches!(key, b"LMsk" | b"Lr16" | b"Lr32" | b"Layr" | b"Mt16" | b"Mt32" | b"Mtrn" | b"Alph" | b"FMsk" | b"lnk2" | b"FEid" | b"FXid" | b"PxSD");
            let len = if long { r.u64()? as usize } else { r.u32()? as usize };
            let data = r.bytes(len.min(end.saturating_sub(r.p)))?;
            if (key == b"lsct" || key == b"lsdk") && data.len() >= 4 {
                section = u32::from_be_bytes(data[0..4].try_into().unwrap());
            }
            // Blocks are padded to even (4 in some writers); tolerate both.
            if len % 2 == 1 && r.p < end {
                r.skip(1)?;
            }
        }
        r.p = end;
        list.push(Layer { rect: (t, l, b, rt), channels, opacity, visible: flags & 0x02 == 0, section, effective: false });
    }
    // Channel data follows, in layer order.
    let mut offsets = Vec::with_capacity(list.len());
    for l in &list {
        offsets.push(r.p);
        let total: usize = l.channels.iter().map(|c| c.1).sum();
        r.skip(total.min(r.d.len() - r.p))?;
    }
    // Visibility through groups: walk top-down.
    let mut stack = vec![true];
    for l in list.iter_mut().rev() {
        let parent = *stack.last().unwrap_or(&true);
        match l.section {
            1 | 2 => {
                stack.push(parent && l.visible);
            }
            3 => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            _ => l.effective = parent && l.visible,
        }
    }
    Ok((list, offsets, merged_alpha))
}

/// Normal-blend the visible 8-bit RGB/gray pixel layers.
fn composite(d: &[u8], hd: &Header, list: &[Layer], offsets: &[usize]) -> Option<Vec<u8>> {
    if hd.depth != 8 || !matches!(hd.mode, 1 | 3) {
        return None;
    }
    let (w, h) = (hd.w, hd.h);
    let mut acc = vec![0f32; w * h * 4];
    let mut drawn = 0;
    for (l, &off) in list.iter().zip(offsets) {
        let (t, lf, b, rt) = l.rect;
        let (lw, lh) = ((rt - lf).max(0) as usize, (b - t).max(0) as usize);
        if !l.effective || l.section != 0 || lw == 0 || lh == 0 || l.opacity == 0 {
            continue;
        }
        let mut r = R { d, p: off };
        let mut chans: Vec<(i16, Vec<u8>)> = Vec::new();
        for &(id, len) in &l.channels {
            let start = r.p;
            // User/vector masks have their own rectangles: skip them.
            if id >= -1 {
                let mut cr = R { d, p: start };
                if let Ok(mut p) = planes(&mut cr, 1, lw, lh, 8, hd.psb) {
                    chans.push((id, p.pop().unwrap_or_default()));
                }
            }
            r.p = start + len;
        }
        let get = |id: i16| chans.iter().find(|c| c.0 == id).map(|c| c.1.as_slice());
        let gray = hd.mode == 1;
        let (Some(c0), c1, c2) = (get(0), get(1), get(2)) else { continue };
        let a = get(-1);
        let op = l.opacity as f32 / 255.0;
        for y in 0..lh {
            let dy = t + y as i32;
            if dy < 0 || dy >= h as i32 {
                continue;
            }
            for x in 0..lw {
                let dx = lf + x as i32;
                if dx < 0 || dx >= w as i32 {
                    continue;
                }
                let i = y * lw + x;
                let sa = a.map(|a| a[i] as f32 / 255.0).unwrap_or(1.0) * op;
                if sa <= 0.0 {
                    continue;
                }
                let (sr, sg, sb) = if gray {
                    (c0[i], c0[i], c0[i])
                } else {
                    (c0[i], c1.map(|c| c[i]).unwrap_or(0), c2.map(|c| c[i]).unwrap_or(0))
                };
                let o = 4 * (dy as usize * w + dx as usize);
                let da = acc[o + 3];
                let out_a = sa + da * (1.0 - sa);
                for (k, s) in [sr, sg, sb].into_iter().enumerate() {
                    acc[o + k] = (s as f32 * sa + acc[o + k] * da * (1.0 - sa)) / out_a.max(1e-6);
                }
                acc[o + 3] = out_a;
            }
        }
        drawn += 1;
    }
    if drawn == 0 {
        return None;
    }
    Some(acc.chunks(4).flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, (p[3] * 255.0).round() as u8]).collect())
}

pub fn decode(d: &[u8]) -> Result<Image> {
    let mut r = R { d, p: 0 };
    if r.bytes(4)? != b"8BPS" {
        return Err(err("not a Photoshop file"));
    }
    let version = r.u16()?;
    let psb = version == 2;
    r.skip(6)?;
    let channels = r.u16()? as usize;
    let h = r.u32()? as usize;
    let w = r.u32()? as usize;
    let depth = r.u16()?;
    let mode = r.u16()?;
    if w == 0 || h == 0 || channels == 0 {
        return Err(err("empty image"));
    }
    let hd = Header { psb, w, h, depth, mode };
    let cm_len = r.u32()? as usize;
    let palette = r.bytes(cm_len)?;
    let res_len = r.u32()? as usize;
    let res = resources(r.bytes(res_len)?);
    let lm_len = r.len(psb)?;
    let lm_start = r.p;
    let mut layer_info = None;
    if lm_len >= 4 {
        let mut lr = R { d, p: lm_start };
        if let Ok(li_len) = lr.len(psb) {
            if li_len >= 2 {
                layer_info = layers(&mut lr, psb).ok();
            }
        }
    }
    r.p = lm_start + lm_len.min(d.len() - lm_start);
    let real = res.real_merged.unwrap_or(true);
    let merged_alpha = layer_info.as_ref().map(|l| l.2).unwrap_or(false);
    let layer_count = layer_info.as_ref().map(|l| l.0.iter().filter(|x| x.section == 0).count()).unwrap_or(0);
    // Colour channels per mode; an extra one is the merged transparency.
    let colour = match mode {
        3 | 9 => 3,
        4 => 4,
        _ => 1,
    };
    let mut note = None;
    let mut spots = 0;
    let mut img = if real {
        let pl = planes(&mut r, channels, w, h, depth, psb)?;
        // A negative layer count marks the first extra channel as the merged transparency.
        let alpha_plane = (channels > colour && merged_alpha).then_some(colour);
        let mut im = build(&hd, &pl, palette, res.icc, alpha_plane)?;
        if res.display.iter().any(|d| d.2 == 2) {
            apply_spots(&mut im, &pl, colour + merged_alpha as usize, &res.display, depth);
            spots = res.display.iter().filter(|d| d.2 == 2).count();
        }
        im
    } else {
        let built = layer_info.as_ref().and_then(|(list, offs, _)| composite(d, &hd, list, offs));
        match built {
            Some(v) => {
                note = Some(format!("Rebuilt from {layer_count} layers: saved without Maximize Compatibility (effects, masks and blend modes not applied)"));
                let mut im = Image::new(w as u32, h as u32, Pixels::Rgba8(v), if psb { "Photoshop large document (PSB)" } else { "Photoshop (PSD)" });
                im.alpha = true;
                super::apply_color(&mut im, res.icc, None, None, 0.0);
                im
            }
            None => {
                let th = res.thumbnail.ok_or(err("saved without Maximize Compatibility and without a thumbnail"))?;
                let mut im = super::jpeg::decode(th, &super::DecodeOptions::default())?;
                note = Some("Showing the embedded thumbnail: saved without Maximize Compatibility".into());
                im.full_size = Some((w as u32, h as u32));
                im
            }
        }
    };
    let mode_name = MODES.get(mode as usize).copied().filter(|m| !m.is_empty()).unwrap_or("Unknown");
    img.facts.push(("Colour mode".into(), format!("{mode_name}, {depth} bits per channel")));
    if layer_count > 0 {
        img.facts.push(("Layers".into(), layer_count.to_string()));
    }
    if channels > colour + merged_alpha as usize {
        let extra = channels - colour - merged_alpha as usize;
        let v = if spots > 0 { format!("{extra} ({spots} spot colour{} shown)", if spots == 1 { "" } else { "s" }) } else { extra.to_string() };
        img.facts.push(("Extra channels".into(), v));
    }
    if let Some(dpi) = res.dpi.filter(|d| *d > 0.0) {
        img.facts.push(("Resolution".into(), format!("{dpi:.0} ppi")));
    }
    if let Some(n) = note {
        img.facts.push(("Composite".into(), n));
    }
    let _ = super::apply_exif(&mut img, res.exif, true);
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_runs_and_literals() {
        // 3 literals, then a run of 4 × 0xAA, then 1 literal.
        let src = [2u8, 1, 2, 3, 0xFD, 0xAA, 0, 9];
        let mut out = [0u8; 8];
        packbits(&src, &mut out);
        assert_eq!(out, [1, 2, 3, 0xAA, 0xAA, 0xAA, 0xAA, 9]);
    }

    #[test]
    fn lab_white_and_black() {
        assert_eq!(lab_to_srgb(100.0, 0.0, 0.0), [255, 255, 255]);
        assert_eq!(lab_to_srgb(0.0, 0.0, 0.0), [0, 0, 0]);
    }

    #[test]
    fn minimal_rgb_psd() {
        // 2×1 RGB, raw: red then blue.
        let mut d = b"8BPS".to_vec();
        d.extend([0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0, 2, 0, 8, 0, 3]);
        d.extend([0u8; 12]);
        d.extend([0, 0]);
        d.extend([255, 0, 0, 0, 0, 255]);
        let img = decode(&d).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        let Pixels::Rgba8(v) = img.pixels else { panic!("8-bit expected") };
        assert_eq!(&v[..8], &[255, 0, 0, 255, 0, 0, 255, 255]);
    }
}
