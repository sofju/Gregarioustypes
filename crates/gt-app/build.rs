//! Generates the application icon (drawn in code, multi-size .ico) and embeds it with
//! the application manifest (PerMonitorV2 DPI, UTF-8 code page, common controls v6).

use std::fs;
use std::path::PathBuf;

/// Signed distance to a rounded rectangle centred at (0.5, 0.5).
fn rounded_rect(x: f32, y: f32, half: f32, r: f32) -> f32 {
    let qx = (x - 0.5).abs() - (half - r);
    let qy = (y - 0.5).abs() - (half - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
}

/// Premultiplied BGRA, top-down. A blue tile with a white magnifying lens.
fn draw(size: u32) -> Vec<u8> {
    let ss = 4;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let u = (x as f32 + (sx as f32 + 0.5) / ss as f32) / size as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / ss as f32) / size as f32;
                    if rounded_rect(u, v, 0.47, 0.13) > 0.0 {
                        continue;
                    }
                    // Background gradient.
                    let t = (u + v) / 2.0;
                    let (mut cr, mut cg, mut cb) = (0.23 + (0.10 - 0.23) * t, 0.60 + (0.40 - 0.60) * t, 0.90 + (0.72 - 0.90) * t);
                    // Lens ring and handle.
                    let d = ((u - 0.44).powi(2) + (v - 0.42).powi(2)).sqrt();
                    let ring = (d - 0.20).abs() < 0.055;
                    let handle = segment(u, v, 0.58, 0.57, 0.76, 0.76) < 0.06;
                    if d < 0.145 {
                        // Glass: lighter tint with a highlight.
                        cr = cr * 0.55 + 0.45;
                        cg = cg * 0.55 + 0.45;
                        cb = cb * 0.55 + 0.45;
                        if ((u - 0.39).powi(2) + (v - 0.36).powi(2)).sqrt() < 0.045 {
                            cr = 1.0;
                            cg = 1.0;
                            cb = 1.0;
                        }
                    }
                    if ring || handle {
                        cr = 1.0;
                        cg = 1.0;
                        cb = 1.0;
                    }
                    r += cr;
                    g += cg;
                    b += cb;
                    a += 1.0;
                }
            }
            let n = (ss * ss) as f32;
            let i = ((y * size + x) * 4) as usize;
            out[i] = (b / n * 255.0) as u8;
            out[i + 1] = (g / n * 255.0) as u8;
            out[i + 2] = (r / n * 255.0) as u8;
            out[i + 3] = (a / n * 255.0) as u8;
        }
    }
    out
}

fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut images = Vec::new();
    for &s in sizes {
        let px = draw(s);
        let mut img = Vec::new();
        // BITMAPINFOHEADER, height doubled for XOR + AND masks.
        img.extend_from_slice(&40u32.to_le_bytes());
        img.extend_from_slice(&(s as i32).to_le_bytes());
        img.extend_from_slice(&((s * 2) as i32).to_le_bytes());
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        img.extend_from_slice(&[0u8; 24]);
        // ICO wants straight alpha, bottom-up.
        for y in (0..s).rev() {
            for x in 0..s {
                let i = ((y * s + x) * 4) as usize;
                let a = px[i + 3] as u32;
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
                img.extend_from_slice(&[un(px[i]), un(px[i + 1]), un(px[i + 2]), px[i + 3]]);
            }
        }
        let mask_row = s.div_ceil(32) * 4;
        img.extend(std::iter::repeat_n(0u8, (mask_row * s) as usize));
        images.push((s, img));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, img) in &images {
        let d = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[d, d, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for (_, img) in &images {
        out.extend_from_slice(img);
    }
    out
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Gregarioustypes" version="0.1.0.0"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/></application>
  </compatibility>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    fs::write(out.join("gt.ico"), ico(&[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();
    fs::write(out.join("app.manifest"), MANIFEST).unwrap();
    let rc = out.join("app.rc");
    fs::write(&rc, "1 ICON \"gt.ico\"\r\n1 24 \"app.manifest\"\r\n").unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_required().unwrap();
}
