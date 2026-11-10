//! Xara documents (.xar, compressed .web, .wix, .art): a record stream
//! ("XARA£\r\n", then tag/size/data records). Only Xara can draw the vector
//! content; the file carries a preview bitmap (JPEG, GIF, PNG or BMP, up to
//! 512 px) right after the header, which is what is shown.

use crate::common;
use gt_core::CancelToken;
use gt_ui::{Loaded, Probe, Provider};

pub const MAGIC: &[u8] = b"XARA\xA3\xA3\x0D\x0A";
const EXTS: &[&str] = &["xar", "web", "wix", "art"];

/// TAG_FILEHEADER text: producer and version strings.
fn header_strings(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for &b in data {
        if (0x20..0x7F).contains(&b) {
            cur.push(b);
        } else {
            if cur.len() >= 3 {
                out.push(String::from_utf8_lossy(&cur).trim().to_string());
            }
            cur.clear();
        }
    }
    if cur.len() >= 3 {
        out.push(String::from_utf8_lossy(&cur).trim().to_string());
    }
    out.retain(|s| s != "CXN" && !s.is_empty());
    out
}

pub struct Parsed {
    pub preview: Option<Vec<u8>>,
    pub strings: Vec<String>,
}

pub fn parse(d: &[u8]) -> Option<Parsed> {
    if !d.starts_with(MAGIC) {
        return None;
    }
    let mut o = MAGIC.len();
    let mut out = Parsed { preview: None, strings: Vec::new() };
    for _ in 0..64 {
        if o + 8 > d.len() {
            break;
        }
        let tag = u32::from_le_bytes(d[o..o + 4].try_into().ok()?);
        let size = u32::from_le_bytes(d[o + 4..o + 8].try_into().ok()?) as usize;
        let data = d.get(o + 8..o + 8 + size)?;
        if tag == 2 {
            out.strings = header_strings(data);
        }
        let image = data.starts_with(&[0xFF, 0xD8, 0xFF]) || data.starts_with(b"\x89PNG") || data.starts_with(b"GIF8") || data.starts_with(b"BM");
        if image && out.preview.is_none() {
            out.preview = Some(data.to_vec());
            break;
        }
        // Records past TAG_STARTCOMPRESSION (30) are zlib-compressed: stop there.
        if tag == 30 {
            break;
        }
        o += 8 + size;
    }
    Some(out)
}

pub struct XaraProvider;

impl Provider for XaraProvider {
    fn id(&self) -> &'static str {
        "xara"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.head.starts_with(MAGIC) {
            return 0;
        }
        if p.ext_is(EXTS) {
            95
        } else {
            80
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        // The preview sits within the first records: the 64 KB head is usually enough.
        let parsed = match parse(&p.head).filter(|x| x.preview.is_some()) {
            Some(x) => x,
            None => {
                let data = std::fs::read(&p.path).map_err(|e| e.to_string())?;
                parse(&data).ok_or("Not a Xara file")?
            }
        };
        let kind = match p.ext.as_str() {
            "web" => "Xara web document (compressed)",
            "wix" => "Xara web document",
            _ => "Xara document",
        };
        let mut facts = vec![("Format".to_string(), kind.to_string())];
        if let Some(app) = parsed.strings.iter().find(|s| s.contains("Xara") || s.contains("Designer")) {
            facts.push(("Saved by".into(), app.clone()));
        }
        let Some(raster) = parsed.preview.as_deref().and_then(|b| common::decode(b, 4096)) else {
            return Ok(common::card(p, "No preview is stored in this Xara file: only Xara can draw it.", facts));
        };
        facts.push(("Preview".into(), format!("{} × {} px stored in the file", raster.w, raster.h)));
        Ok(common::preview(raster, false, facts, Some("Preview stored in the file: only Xara can draw the full design".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_preview_record() {
        let mut d = MAGIC.to_vec();
        let hdr = b"CXN\x10\x00\x00\x00Xara Designer Pro+\0";
        d.extend((2u32).to_le_bytes());
        d.extend((hdr.len() as u32).to_le_bytes());
        d.extend(hdr);
        let img = b"GIF89a....";
        d.extend((61u32).to_le_bytes());
        d.extend((img.len() as u32).to_le_bytes());
        d.extend(img);
        let p = parse(&d).unwrap();
        assert_eq!(p.preview.as_deref(), Some(&img[..]));
        assert!(p.strings.iter().any(|s| s == "Xara Designer Pro+"));
    }
}
