//! NFO / DIZ / ASC / ANS: scene ASCII and ANSI art rendered with the IBM VGA font.

pub mod grid;
pub mod raster;
pub mod sauce;
pub mod view;

use gt_core::encoding::{decode, detect_dos_text, unicode_to_cp437, Encoding};
use gt_core::CancelToken;
use gt_ui::widgets::text::TextView;
use gt_ui::{Loaded, Probe, Provider, View, ViewEnv};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct NfoProvider;

const DOS_EXTS: &[&str] = &["nfo", "diz", "asc", "ans", "ice"];

fn box_bytes(head: &[u8]) -> usize {
    head.iter().filter(|&&b| (0xB0..=0xDF).contains(&b)).count()
}

impl Provider for NfoProvider {
    fn id(&self) -> &'static str {
        "nfo"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        if p.ext == "nfo" && gt_core::sniff::is_msinfo_xml(&p.head) {
            return 0; // msinfo32 export: XML, handled by the text provider
        }
        if p.ext_is(DOS_EXTS) {
            return 60;
        }
        // DOS art saved as .txt.
        if p.ext == "txt" && box_bytes(&p.head) >= 16 && std::str::from_utf8(&p.head).is_err() {
            return 55;
        }
        0
    }
    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        if p.size > 16 * 1024 * 1024 {
            return Err("file too large for an NFO".into());
        }
        let data = std::fs::read(&p.path).map_err(|e| e.to_string())?;
        let (content, sauce) = sauce::split(&data);
        let content = content.to_vec();
        let encoding = load_override(&p.path).unwrap_or_else(|| detect_dos_text(&content));
        let ansi = p.ext == "ans" || p.ext == "ice" || sauce.as_ref().is_some_and(|s| s.is_ansi()) || content.windows(2).any(|w| w == b"\x1b[");
        Ok(Box::new(NfoData { path: p.path.clone(), raw: content, sauce, encoding, ansi }))
    }
}

pub struct NfoData {
    pub path: PathBuf,
    pub raw: Vec<u8>,
    pub sauce: Option<sauce::Sauce>,
    pub encoding: Encoding,
    pub ansi: bool,
}

/// Bytes for the VGA renderer: CP437 as-is, other encodings mapped back to CP437.
/// Returns the bytes and the number of characters that have no CP437 glyph.
pub fn to_cp437(raw: &[u8], enc: Encoding) -> (Vec<u8>, usize) {
    if enc == Encoding::Cp437 {
        return (raw.to_vec(), 0);
    }
    let mut missing = 0;
    let out = decode(raw, enc)
        .chars()
        .map(|c| {
            unicode_to_cp437(c).or_else(|| approximate(c)).unwrap_or_else(|| {
                missing += 1;
                b'?'
            })
        })
        .collect();
    (out, missing)
}

/// Closest CP437 glyph for common typographic characters the VGA font lacks.
fn approximate(c: char) -> Option<u8> {
    Some(match c {
        '\u{2010}'..='\u{2015}' | '\u{2212}' => b'-',
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => b'\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => b'"',
        '\u{2026}' => 0xFA, // … → ·
        '\u{2022}' => 0x07, // • → bullet glyph
        '\u{00A9}' => b'c',
        '\u{00AE}' => b'R',
        '\u{2122}' => b'T',
        '\u{20AC}' => b'E',
        '\u{FEFF}' => return None,
        _ => return None,
    })
}

impl Loaded for NfoData {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        let d = *self;
        // A Unicode file full of characters the VGA font lacks reads better as text.
        if d.encoding != Encoding::Cp437 {
            let (_, missing) = to_cp437(&d.raw, d.encoding);
            let non_ascii = decode(&d.raw, d.encoding).chars().filter(|c| *c as u32 >= 0x80).count();
            if missing > 0 && missing * 50 > non_ascii.max(1) {
                let text = decode(&d.raw, d.encoding);
                return Box::new(TextView::new(&text, false, Some(d.encoding.label().into()), None));
            }
        }
        Box::new(view::DosArtView::new(d, env.settings))
    }
}

fn overrides() -> &'static Mutex<HashMap<String, Encoding>> {
    static M: OnceLock<Mutex<HashMap<String, Encoding>>> = OnceLock::new();
    M.get_or_init(|| {
        let path = gt_core::paths::data_dir().join("nfo-encodings.json");
        let map = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Mutex::new(map)
    })
}

fn key(path: &Path) -> String {
    path.display().to_string().to_lowercase()
}

pub fn load_override(path: &Path) -> Option<Encoding> {
    overrides().lock().get(&key(path)).copied()
}

/// Remember the encoding chosen for this file.
pub fn save_override(path: &Path, enc: Encoding) {
    let mut m = overrides().lock();
    m.insert(key(path), enc);
    if let Ok(b) = serde_json::to_vec_pretty(&*m) {
        let _ = std::fs::write(gt_core::paths::data_dir().join("nfo-encodings.json"), b);
    }
}
