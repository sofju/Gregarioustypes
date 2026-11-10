//! Shared pieces: embedded-preview views, info cards, light XMP reading.

use gt_fmt_doc::pages::{PageSource, Raster};
use gt_fmt_doc::paged::PagedView;
use gt_fmt_doc::raster::RasterSource;
use gt_ui::widgets::card::{CardData, InfoCard};
use gt_ui::{Loaded, Probe, View, ViewEnv};
use std::sync::Arc;

pub fn decode(bytes: &[u8], max: u32) -> Option<Raster> {
    let b = gt_fmt_media::cover::decode_bytes(bytes, max)?;
    Some(Raster { w: b.width, h: b.height, data: b.data })
}

/// Pages ready for the page viewer, with an optional note shown briefly.
pub struct PagesLoaded {
    pub src: Arc<dyn PageSource>,
    pub facts: Vec<(String, String)>,
    pub note: Option<String>,
}

impl Loaded for PagesLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        let v = PagedView::new(self.src, self.facts);
        Box::new(match self.note {
            Some(n) => v.with_note(n),
            None => v,
        })
    }
}

/// A single embedded preview bitmap shown at its own size.
pub fn preview(raster: Raster, paper: bool, facts: Vec<(String, String)>, note: Option<String>) -> Box<dyn Loaded> {
    Box::new(PagesLoaded { src: Arc::new(RasterSource::single(raster, paper)), facts, note })
}

struct CardLoaded(CardData);

impl Loaded for CardLoaded {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        Box::new(InfoCard::new(&env.gfx.dc, self.0))
    }
}

/// Info card with the shell icon, size and facts, and a note.
pub fn card(p: &Probe, note: &str, facts: Vec<(String, String)>) -> Box<dyn Loaded> {
    let path = p.path.display().to_string();
    let icon = gt_shell::info::image(&path, 256, false).map(|b| (b.width, b.height, b.data));
    let mut f = vec![("Size".to_string(), gt_core::fmt::size_long(p.size))];
    f.extend(facts);
    Box::new(CardLoaded(CardData { icon, name: p.name(), kind: gt_shell::info::type_name(&path), facts: f, note: Some(note.to_string()) }))
}

pub fn unescape(s: &str) -> String {
    s.replace("&#xA;", "\n").replace("&#xa;", "\n").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Text of every `<tag>…</tag>` (also `tag="…"` attributes), in order.
pub fn all(xml: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&open) {
        let s = from + i + open.len();
        let Some(j) = xml[s..].find(&close) else { break };
        out.push(unescape(xml[s..s + j].trim()));
        from = s + j;
    }
    let attr = format!("{tag}=\"");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&attr) {
        let s = from + i + attr.len();
        let Some(j) = xml[s..].find('"') else { break };
        out.push(unescape(&xml[s..s + j]));
        from = s + j;
    }
    out
}

pub fn first(xml: &str, tag: &str) -> Option<String> {
    all(xml, tag).into_iter().find(|v| !v.is_empty())
}

/// "2024-03-05T10:20:00+01:00" → local short date.
pub fn xmp_date(s: &str) -> Option<String> {
    let d = s.trim();
    let n = |a: usize, b: usize| d.get(a..b).and_then(|x| x.parse::<u16>().ok());
    let tz = d.get(19..).unwrap_or("");
    let tz = if tz.starts_with('Z') {
        "Z".to_string()
    } else if let Some(t) = tz.strip_prefix(|c| c == '+' || c == '-') {
        format!("{}{}'{}'", &tz[..1], t.get(0..2).unwrap_or("00"), t.get(3..5).unwrap_or("00"))
    } else {
        String::new()
    };
    let pdf = format!("D:{:04}{:02}{:02}{:02}{:02}00{tz}", n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13).unwrap_or(0), n(14, 16).unwrap_or(0));
    gt_fmt_doc::pdf::pdf_date(&pdf.replace('Z', "+00'00'"))
}

/// Points per unit for XMP `stDim:unit`.
pub fn unit_to_pt(unit: &str) -> f32 {
    match unit.to_ascii_lowercase().as_str() {
        "millimeters" | "mm" => 72.0 / 25.4,
        "centimeters" | "cm" => 72.0 / 2.54,
        "inches" | "in" => 72.0,
        "picas" => 12.0,
        _ => 1.0,
    }
}
