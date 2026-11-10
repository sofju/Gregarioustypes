//! JPEG XR through the Windows Imaging Component (built into Windows). HDR captures
//! (half/float/fixed-point scRGB) decode to linear half floats.

use crate::{util, Color, Image, Lum, Pixels, Result};
use std::path::Path;
use windows::core::{GUID, HSTRING};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

fn e(x: windows::core::Error) -> crate::DecodeError {
    crate::DecodeError(format!("wic: {}", x.message()))
}

const HDR_FORMATS: &[GUID] = &[
    GUID_WICPixelFormat64bppRGBAHalf,
    GUID_WICPixelFormat64bppRGBHalf,
    GUID_WICPixelFormat48bppRGBHalf,
    GUID_WICPixelFormat128bppRGBAFloat,
    GUID_WICPixelFormat128bppRGBFloat,
    GUID_WICPixelFormat128bppPRGBAFloat,
    GUID_WICPixelFormat64bppRGBAFixedPoint,
    GUID_WICPixelFormat64bppRGBFixedPoint,
    GUID_WICPixelFormat48bppRGBFixedPoint,
    GUID_WICPixelFormat32bppRGBE,
    GUID_WICPixelFormat16bppGrayHalf,
    GUID_WICPixelFormat32bppGrayFloat,
];

pub fn decode(path: &Path) -> Result<Image> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(e)?;
        let dec = wic.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).map_err(e)?;
        let frame = dec.GetFrame(0).map_err(e)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(e)?;
        let pf = frame.GetPixelFormat().map_err(e)?;
        let hdr = HDR_FORMATS.contains(&pf);
        let target = if hdr { GUID_WICPixelFormat64bppRGBAHalf } else { GUID_WICPixelFormat32bppRGBA };
        let conv = wic.CreateFormatConverter().map_err(e)?;
        conv.Initialize(&frame, &target, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(e)?;
        let bpp = if hdr { 8 } else { 4 };
        let stride = w * bpp;
        let mut buf = vec![0u8; (stride * h) as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut buf).map_err(e)?;
        let pixels = if hdr { Pixels::RgbaF16(buf.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])).collect()) } else { Pixels::Rgba8(buf) };
        let mut img = Image::new(w, h, pixels, "JPEG XR");
        if hdr {
            img.color = Color { lum: Lum::ScRgb, ..Color::linear_709("scRGB (linear, 80 nits = 1.0)") };
            img.bit_depth = 16;
            img.hdr_float = true;
        }
        let _ = util::f16_to_f32;
        Ok(img)
    }
}
