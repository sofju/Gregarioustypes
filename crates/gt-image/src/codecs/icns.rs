//! Apple icon files: the largest decodable entry (PNG/RLE via the `icns` crate,
//! JPEG 2000 entries via OpenJPEG).

use crate::ffi::{gt_j2k_decode, Native};
use crate::{err, Image, Pixels, Result};

pub fn decode(d: &[u8]) -> Result<Image> {
    let family = icns::IconFamily::read(std::io::Cursor::new(d)).map_err(|e| crate::DecodeError(format!("icns: {e}")))?;
    let mut types = family.available_icons();
    types.sort_by_key(|t| std::cmp::Reverse(t.pixel_width()));
    for t in types {
        if let Ok(icon) = family.get_icon_with_type(t) {
            let rgba = icon.convert_to(icns::PixelFormat::RGBA);
            let mut img = Image::new(rgba.width(), rgba.height(), Pixels::Rgba8(rgba.data().to_vec()), "Apple Icon Image");
            img.alpha = true;
            img.facts.push(("Sizes".into(), format!("{} entries, largest {}×{}", family.elements.len(), rgba.width(), rgba.height())));
            return Ok(img);
        }
    }
    // JPEG 2000 entries the icns crate can't decode.
    let mut best: Option<Image> = None;
    for e in &family.elements {
        let data = &e.data;
        if !(data.starts_with(&[0xFF, 0x4F]) || data.starts_with(&[0, 0, 0, 0x0C, b'j', b'P'])) {
            continue;
        }
        let mut n = Native::zeroed();
        if unsafe { gt_j2k_decode(data.as_ptr(), data.len(), 0, 0, n.ptr()) } == 0 {
            let g = &*n.0;
            if best.as_ref().map(|b| b.width < g.width as u32).unwrap_or(true) {
                let px = unsafe { std::slice::from_raw_parts(g.pixels, (g.stride * g.height) as usize) };
                let pixels = if g.format == crate::ffi::GT_RGBA16 { Pixels::Rgba16(px.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])).collect()) } else { Pixels::Rgba8(px.to_vec()) };
                let mut img = Image::new(g.width as u32, g.height as u32, pixels, "Apple Icon Image");
                img.alpha = true;
                best = Some(img);
            }
        }
    }
    best.map(Ok).unwrap_or_else(|| err("icns: no decodable icon"))
}
