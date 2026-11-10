//! Cover art: embedded pictures (from MediaInfo) or folder images, decoded with
//! WIC into premultiplied BGRA.

use gt_shell::info::Bgra;
use std::path::Path;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

fn factory() -> Option<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok() }
}

fn convert(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode, max: u32) -> Option<Bgra> {
    unsafe {
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let k = (max as f32 / w.max(h) as f32).min(1.0);
        let (tw, th) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
        let source: IWICBitmapSource = if (tw, th) != (w, h) {
            let sc = f.CreateBitmapScaler().ok()?;
            sc.Initialize(frame, tw, th, WICBitmapInterpolationModeHighQualityCubic).ok()?;
            sc.into()
        } else {
            frame.clone().into()
        };
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&source, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let stride = tw * 4;
        let mut data = vec![0u8; (stride * th) as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut data).ok()?;
        Some(Bgra { width: tw, height: th, data })
    }
}

/// Decode image bytes (JPEG, PNG, GIF, BMP...) scaled to fit `max` pixels.
pub fn decode_bytes(bytes: &[u8], max: u32) -> Option<Bgra> {
    let f = factory()?;
    unsafe {
        let stream = f.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        convert(&f, &frame, max)
    }
}

pub fn decode_file(path: &Path, max: u32) -> Option<Bgra> {
    let bytes = std::fs::read(path).ok()?;
    decode_bytes(&bytes, max)
}

/// `cover.jpg`, `folder.jpg`... next to an audio file.
pub fn folder_art(path: &Path) -> Option<std::path::PathBuf> {
    const NAMES: &[&str] = &["cover", "folder", "front", "albumart", "album", "albumartsmall"];
    let dir = path.parent()?;
    let mut best: Option<(usize, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let ext = gt_core::sniff::ext(&p);
        if !matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
            continue;
        }
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let stem = stem.split(['{', '_', ' ']).next().unwrap_or("").to_string();
        if let Some(rank) = NAMES.iter().position(|n| *n == stem) {
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, p));
            }
        }
    }
    best.map(|(_, p)| p)
}
