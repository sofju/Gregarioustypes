//! BMP, ICO/CUR, TGA and Radiance HDR via the `image` crate.

use super::Kind;
use crate::{util, Color, Image, Pixels, Result};

pub fn decode(d: &[u8], kind: Kind) -> Result<Image> {
    let fmt = match kind {
        Kind::Bmp => image::ImageFormat::Bmp,
        Kind::Ico => image::ImageFormat::Ico,
        Kind::Tga => image::ImageFormat::Tga,
        _ => image::ImageFormat::Hdr,
    };
    let dynimg = image::load_from_memory_with_format(d, fmt).map_err(|e| crate::DecodeError(e.to_string()))?;
    let (w, h) = (dynimg.width(), dynimg.height());
    if kind == Kind::Hdr {
        let f = dynimg.to_rgba32f();
        let mut img = Image::new(w, h, Pixels::RgbaF16(util::f32_to_f16(f.as_raw())), "Radiance HDR");
        img.color = Color::linear_709("Linear Rec. 709");
        img.bit_depth = 32;
        img.hdr_float = true;
        return Ok(img);
    }
    let alpha = dynimg.color().has_alpha();
    let mut img = Image::new(w, h, Pixels::Rgba8(dynimg.to_rgba8().into_raw()), match kind {
        Kind::Bmp => "BMP",
        Kind::Ico => "Icon",
        _ => "TGA",
    });
    img.alpha = alpha;
    Ok(img)
}
