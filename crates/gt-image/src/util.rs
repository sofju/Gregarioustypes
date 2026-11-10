//! Pixel utilities: conversions, CMYK, downscaling to the texture limit, HDR analysis.

use crate::color::{Lum, Transfer};
use crate::{Image, Pixels};
use half::f16;
use rayon::prelude::*;

pub fn f32_to_f16(v: &[f32]) -> Vec<u16> {
    let mut out = vec![0u16; v.len()];
    out.par_chunks_mut(1 << 16).zip(v.par_chunks(1 << 16)).for_each(|(o, i)| {
        for (d, s) in o.iter_mut().zip(i) {
            *d = f16::from_f32(*s).to_bits();
        }
    });
    out
}

pub fn f16_to_f32(bits: u16) -> f32 {
    f16::from_bits(bits).to_f32()
}

/// Naive CMYK → RGB when no profile is available (`inverted`: Adobe-style storage).
pub fn cmyk_naive(cmyk: &[u8], inverted: bool) -> Vec<u8> {
    let mut out = vec![0u8; cmyk.len()];
    out.par_chunks_mut(4).zip(cmyk.par_chunks(4)).for_each(|(o, i)| {
        let (c, m, y, k) = if inverted { (255 - i[0], 255 - i[1], 255 - i[2], 255 - i[3]) } else { (i[0], i[1], i[2], i[3]) };
        let w = (255 - k as u32) as f32 / 255.0;
        o[0] = ((255 - c as u32) as f32 * w) as u8;
        o[1] = ((255 - m as u32) as f32 * w) as u8;
        o[2] = ((255 - y as u32) as f32 * w) as u8;
        o[3] = 255;
    });
    out
}

/// Box-downscale by an integer factor (keeps the format).
pub fn downscale(p: &Pixels, w: u32, h: u32, f: u32) -> (Pixels, u32, u32) {
    let (nw, nh) = ((w / f).max(1), (h / f).max(1));
    let (w, f) = (w as usize, f as usize);
    let n = (f * f) as f32;
    macro_rules! go {
        ($src:expr, $t:ty, $to:expr, $from:expr) => {{
            let mut out = vec![<$t>::default(); nw as usize * nh as usize * 4];
            out.par_chunks_mut(nw as usize * 4).enumerate().for_each(|(y, row)| {
                for x in 0..nw as usize {
                    let mut acc = [0f32; 4];
                    for dy in 0..f {
                        for dx in 0..f {
                            let i = ((y * f + dy) * w + x * f + dx) * 4;
                            for c in 0..4 {
                                acc[c] += $from($src[i + c]);
                            }
                        }
                    }
                    for c in 0..4 {
                        row[x * 4 + c] = $to(acc[c] / n);
                    }
                }
            });
            out
        }};
    }
    let out = match p {
        Pixels::Rgba8(s) => Pixels::Rgba8(go!(s, u8, |v: f32| v.round() as u8, |v: u8| v as f32)),
        Pixels::Rgba16(s) => Pixels::Rgba16(go!(s, u16, |v: f32| v.round() as u16, |v: u16| v as f32)),
        Pixels::RgbaF16(s) => Pixels::RgbaF16(go!(s, u16, |v: f32| f16::from_f32(v).to_bits(), f16_to_f32)),
    };
    (out, nw, nh)
}

/// D3D11 limits textures to 16384 pixels per side.
pub fn fit_texture_limit(img: &mut Image, max: u32) {
    let big = img.width.max(img.height);
    if big <= max {
        return;
    }
    let f = big.div_ceil(max);
    let full = img.full_size.unwrap_or((img.width, img.height));
    let (p, w, h) = downscale(&img.pixels, img.width, img.height, f);
    img.pixels = p;
    img.width = w;
    img.height = h;
    img.full_size = Some(full);
}

fn pq_eotf_nits(e: f32) -> f32 {
    let (m1, m2, c1, c2, c3) = (0.1593017578125f32, 78.84375f32, 0.8359375f32, 18.8515625f32, 18.6875f32);
    let p = e.clamp(0.0, 1.0).powf(1.0 / m2);
    10000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}

fn hlg_inv_oetf(e: f32) -> f32 {
    let (a, b, c) = (0.17883277f32, 0.28466892f32, 0.55991073f32);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - c) / a).exp() + b) / 12.0
    }
}

/// Brightest meaningful value of the image, in nits, for tone mapping (99.9th
/// percentile of sampled max-channel values, so a few hot pixels don't dim it).
/// For relative content the unit is "SDR white = 203 nits" (BT.2408 reference).
pub fn content_peak_nits(img: &Image) -> f32 {
    if !img.color.is_hdr() && !img.hdr_float {
        return 203.0;
    }
    let n = (img.width * img.height) as usize;
    let step = (n / 1_000_000).max(1);
    let mut samples: Vec<f32> = match &img.pixels {
        Pixels::Rgba8(v) => (0..n).step_by(step).map(|i| v[i * 4..i * 4 + 3].iter().copied().max().unwrap_or(0) as f32 / 255.0).collect(),
        Pixels::Rgba16(v) => (0..n).step_by(step).map(|i| v[i * 4..i * 4 + 3].iter().copied().max().unwrap_or(0) as f32 / 65535.0).collect(),
        Pixels::RgbaF16(v) => (0..n).step_by(step).map(|i| (0..3).map(|c| f16_to_f32(v[i * 4 + c])).fold(0.0f32, f32::max)).collect(),
    };
    if samples.is_empty() {
        return 203.0;
    }
    let k = ((samples.len() as f32) * 0.999) as usize;
    let k = k.min(samples.len() - 1);
    samples.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let v = samples[k].max(0.0);
    let nits = match (img.color.lum, img.color.transfer) {
        (Lum::Pq, _) => pq_eotf_nits(v),
        (Lum::Hlg, _) => 1000.0 * hlg_inv_oetf(v).powf(1.2),
        (Lum::ScRgb, _) => v * 80.0,
        (Lum::Relative, Transfer::Linear) => v * 203.0,
        _ => 203.0,
    };
    let meta = if img.max_cll > 0.0 { img.max_cll } else { img.mastering_max };
    if meta > 0.0 && img.color.lum == Lum::Pq {
        nits.min(meta.max(203.0))
    } else {
        nits.max(1.0)
    }
}
