//! Textures referenced by converted models: found next to the model (the way
//! DCC tools look for them), embedded as-is when the browser can decode them,
//! otherwise (TGA, TIFF, PSD...) decoded here and re-encoded as PNG.

use std::path::{Path, PathBuf};

/// Where a model's textures may live, best first.
pub fn candidates(model: &Path, name: &str) -> Vec<PathBuf> {
    let name = name.replace('\\', "/");
    let dir = model.parent().unwrap_or(Path::new("."));
    let file = name.rsplit('/').next().unwrap_or(&name).to_string();
    let mut v = Vec::new();
    let p = PathBuf::from(&name);
    if p.is_absolute() {
        v.push(p.clone());
    } else if !name.is_empty() {
        v.push(dir.join(&name));
    }
    if file.is_empty() {
        return v;
    }
    let stem = model.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    v.push(dir.join(&file));
    for sub in ["textures", "Textures", "texture", "tex", "maps", "images", "Images", &format!("{stem}.fbm")] {
        v.push(dir.join(sub).join(&file));
    }
    if let Some(up) = dir.parent() {
        for sub in ["textures", "Textures", "texture", "tex", "maps", "images"] {
            v.push(up.join(sub).join(&file));
        }
    }
    v
}

/// Read the first existing candidate.
pub fn find(model: &Path, names: &[&str]) -> Option<Vec<u8>> {
    for n in names.iter().filter(|n| !n.is_empty()) {
        for c in candidates(model, n) {
            if c.is_file() {
                if let Ok(b) = std::fs::read(&c) {
                    return Some(b);
                }
            }
        }
    }
    None
}

/// Bytes and mime type the 3D page can use, converting when needed.
pub fn web(bytes: Vec<u8>, name: &str) -> Option<(Vec<u8>, &'static str)> {
    if let Some(m) = crate::glb::web_image_mime(&bytes) {
        return Some((bytes, m));
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let head = &bytes[..bytes.len().min(64 << 10)];
    let kind = gt_image::detect(head, &ext).map(|(k, _)| k).or(if ext == "tga" { Some(gt_image::Kind::Tga) } else { None })?;
    let opts = gt_image::DecodeOptions { full: true, ..Default::default() };
    let img = match gt_image::decode_bytes(&bytes, kind, &opts) {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!("texture {name}: {}", e.0);
            return None;
        }
    };
    let (w, h) = (img.width, img.height);
    let rgba: Vec<u8> = match &img.pixels {
        gt_image::Pixels::Rgba8(v) => v.clone(),
        gt_image::Pixels::Rgba16(v) => v.iter().map(|x| (x >> 8) as u8).collect(),
        gt_image::Pixels::RgbaF16(v) => v.iter().map(|x| (f16_to_f32(*x).clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0 + 0.5) as u8).collect(),
    };
    png(&rgba, w, h, img.alpha).map(|p| (p, "image/png"))
}

fn png(rgba: &[u8], w: u32, h: u32, alpha: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut e = ::png::Encoder::new(&mut out, w, h);
        e.set_color(if alpha { ::png::ColorType::Rgba } else { ::png::ColorType::Rgb });
        e.set_depth(::png::BitDepth::Eight);
        e.set_compression(::png::Compression::Fast);
        let mut wr = e.write_header().ok()?;
        if alpha {
            wr.write_image_data(rgba).ok()?;
        } else {
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            wr.write_image_data(&rgb).ok()?;
        }
    }
    Some(out)
}

fn f16_to_f32(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f32;
    match e {
        0 => s * m * 2f32.powi(-24),
        31 => s * f32::INFINITY,
        _ => s * (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Does this PNG/WebP/GIF carry an alpha channel (base colour cut-outs)?
pub fn has_alpha(b: &[u8]) -> bool {
    if b.starts_with(b"\x89PNG") && b.len() > 26 {
        return matches!(b[25], 4 | 6) || b.windows(4).take(4096).any(|w| w == b"tRNS");
    }
    false
}
