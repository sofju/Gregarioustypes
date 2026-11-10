//! Ghostscript (bundled `deps/ghostscript`): EPS, PostScript and PostScript-based
//! Illustrator files are converted to PDF once (cached), then shown with PDFium,
//! so zooming stays vector-sharp.

use crate::helper;
use gt_core::CancelToken;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn exe() -> PathBuf {
    gt_core::paths::deps_dir().join("ghostscript").join("bin").join("gswin64c.exe")
}

pub fn available() -> bool {
    exe().is_file()
}

/// DOS EPS binary header: (PostScript range, TIFF preview range).
pub struct DosEps {
    pub ps: (u32, u32),
    pub tiff: Option<(u32, u32)>,
    pub wmf: Option<(u32, u32)>,
}

pub fn dos_eps(head: &[u8]) -> Option<DosEps> {
    if head.len() < 30 || head[..4] != [0xC5, 0xD0, 0xD3, 0xC6] {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes(head[o..o + 4].try_into().unwrap());
    let opt = |o: u32, l: u32| (o != 0 && l != 0).then_some((o, l));
    Some(DosEps { ps: (u(4), u(8)), wmf: opt(u(12), u(16)), tiff: opt(u(20), u(24)) })
}

/// The preview image stored in a DOS EPS (TIFF), decoded.
pub fn dos_preview(path: &Path, d: &DosEps) -> Option<crate::pages::Raster> {
    use std::io::{Read, Seek, SeekFrom};
    let (off, len) = d.tiff?;
    if len > 64 << 20 {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(off as u64)).ok()?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).ok()?;
    let b = gt_fmt_media::cover::decode_bytes(&buf, 2048)?;
    Some(crate::pages::Raster { w: b.width, h: b.height, data: b.data })
}

/// "%%BoundingBox: 0 0 595 842" from the first 64 KB (aspect of the art).
pub fn bounding_box(head: &[u8]) -> Option<(f32, f32)> {
    let start = dos_eps(head).map(|d| d.ps.0 as usize).filter(|&o| o < head.len()).unwrap_or(0);
    let text = String::from_utf8_lossy(&head[start..]);
    for key in ["%%HiResBoundingBox:", "%%BoundingBox:"] {
        if let Some(i) = text.find(key) {
            let rest = &text[i + key.len()..];
            let line = rest.lines().next().unwrap_or("");
            let v: Vec<f32> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() == 4 && v[2] > v[0] && v[3] > v[1] {
                return Some((v[2] - v[0], v[3] - v[1]));
            }
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Single illustration: crop to its bounding box.
    Eps,
    /// Multi-page PostScript document.
    Ps,
}

/// Convert to PDF (cached by path, size and mtime). Returns the PDF path.
pub fn to_pdf(src: &Path, kind: Kind, cancel: &CancelToken) -> Result<PathBuf, String> {
    let out = helper::cache_file("gs", src, "gs10080-v1", "pdf");
    if out.is_file() {
        return Ok(out);
    }
    if !available() {
        return Err("Ghostscript is not installed in deps\\ghostscript.".into());
    }
    let tmp = out.with_extension("part");
    let mut cmd = Command::new(exe());
    cmd.args(["-q", "-dNOPAUSE", "-dBATCH", "-dSAFER", "-dNOINTERPOLATE", "-sDEVICE=pdfwrite", "-dCompatibilityLevel=1.7", "-dAutoRotatePages=/None"]);
    if kind == Kind::Eps {
        cmd.arg("-dEPSCrop");
    }
    cmd.arg(format!("-sOutputFile={}", tmp.display()));
    // SAFER restricts file access; the input is named explicitly so it may be read.
    cmd.arg("-f").arg(src);
    let t = std::time::Instant::now();
    let r = helper::run(&mut cmd, cancel, Duration::from_secs(90));
    match r {
        Ok(()) if tmp.metadata().map(|m| m.len() > 0).unwrap_or(false) => {
            std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
            tracing::info!("ghostscript {} in {:.0} ms", src.display(), t.elapsed().as_secs_f64() * 1000.0);
            Ok(out)
        }
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Err("Ghostscript produced no pages.".into())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}
