//! Affinity documents (.af from Affinity 3, .afdesign, .afphoto, .afpub,
//! .aftemplate...): a proprietary zstd-compressed container. Each file keeps a
//! PNG thumbnail (about 1024 px) in its "Thmb" block, which is what is shown.

use crate::common;
use gt_core::CancelToken;
use gt_ui::{Loaded, Probe, Provider};
use memchr::memmem;

pub const MAGIC: &[u8] = &[0x00, 0xFF, 0x4B, 0x41];
const EXTS: &[&str] = &["af", "afdesign", "afphoto", "afpub", "aftemplate", "afpackage", "afassets", "afbrushes", "afmacro", "afmacros", "afstyles", "afpalette"];

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];

/// The thumbnail: the PNG following the "Thmb" tag, else the largest embedded PNG/JPEG.
pub fn thumbnail(d: &[u8]) -> Option<&[u8]> {
    if let Some(t) = memmem::find(d, b"Thmb") {
        let window = &d[t..(t + 256).min(d.len())];
        if let Some(i) = memmem::find(window, PNG) {
            let s = t + i;
            if let Some(e) = memmem::find(&d[s..], b"IEND") {
                return Some(&d[s..s + e + 8]);
            }
        }
    }
    // Fallback: the biggest complete PNG.
    let mut best: Option<&[u8]> = None;
    for s in memmem::find_iter(d, PNG) {
        if let Some(e) = memmem::find(&d[s..], b"IEND") {
            let png = &d[s..s + e + 8];
            if best.is_none_or(|b| png.len() > b.len()) {
                best = Some(png);
            }
        }
    }
    best.or_else(|| memmem::find(d, JPEG).map(|s| &d[s..]))
}

pub struct AffinityProvider;

impl Provider for AffinityProvider {
    fn id(&self) -> &'static str {
        "affinity"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        match (p.head.starts_with(MAGIC), p.ext_is(EXTS)) {
            (true, true) => 95,
            (true, false) => 75,
            _ => 0,
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let file = std::fs::File::open(&p.path).map_err(|e| e.to_string())?;
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
        let kind = match p.ext.as_str() {
            "afdesign" => "Affinity Designer document",
            "afphoto" => "Affinity Photo document",
            "afpub" => "Affinity Publisher document",
            "aftemplate" => "Affinity template",
            "af" => "Affinity document",
            _ => "Affinity file",
        };
        let mut facts = vec![("Format".to_string(), kind.to_string())];
        let Some(raster) = thumbnail(&map).and_then(|b| common::decode(b, 4096)) else {
            return Ok(common::card(p, "No preview is stored in this file: only Affinity can draw it.", facts));
        };
        facts.push(("Preview".into(), format!("{} × {} px stored in the file", raster.w, raster.h)));
        Ok(common::preview(raster, false, facts, Some("Preview stored in the file: only Affinity can draw the full document".into())))
    }
}
