//! InDesign documents (.indd, .indt): a proprietary database. What is readable
//! is the document's XMP packet: page count and size, fonts, links, colorants,
//! and the page previews InDesign saves (by default the first two pages).
//!
//! When an IDML export of the same document sits next to it, the IDML renderer
//! shows every page instead.

use crate::common::{self, all, first};
use gt_core::CancelToken;
use gt_fmt_doc::raster::{RasterPage, RasterSource};
use gt_ui::{Loaded, Probe, Provider};
use memchr::memmem;
use std::sync::Arc;

/// InDesign database GUID that starts every .indd/.indt.
pub const MAGIC: &[u8] = &[0x06, 0x06, 0xED, 0xF5, 0xD8, 0x1D, 0x46, 0xE5, 0xBD, 0x31, 0xEF, 0xE7, 0xFE, 0x74, 0xB7, 0x1D];

pub struct Doc {
    pub facts: Vec<(String, String)>,
    pub pages: usize,
    /// Page size in points.
    pub page_size: Option<(f32, f32)>,
    /// (page number, base64 JPEG) in page order.
    pub thumbs: Vec<(usize, String)>,
}

const OBJECT_HEADER: &[u8] = &[0xDE, 0x39, 0x39, 0x79, 0x51, 0x88, 0x4B, 0x6C, 0x8E, 0x63, 0xEE, 0xF8, 0xAE, 0xE0, 0xDD, 0x38];

/// The live XMP: a "contiguous object" after the database pages. (Copies found
/// elsewhere in the file can be stale, in freed pages, and cut by page trailers.)
fn contiguous_xmp(d: &[u8]) -> Option<String> {
    if d.len() < 8192 || !d.starts_with(MAGIC) {
        return None;
    }
    let le = d[24] == 1;
    let u32_at = |o: usize| -> Option<u32> {
        let b: [u8; 4] = d.get(o..o + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let u64_at = |o: usize| -> Option<u64> {
        let b: [u8; 8] = d.get(o..o + 8)?.try_into().ok()?;
        Some(if le { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) })
    };
    // Two master pages; the one with the higher sequence number is current.
    let base = if u64_at(4096 + 264)? > u64_at(264)? { 4096 } else { 0 };
    let pages = u32_at(base + 280)? as usize;
    let mut o = pages.checked_mul(4096)?;
    for _ in 0..64 {
        if d.get(o..o + 16)? != OBJECT_HEADER {
            break;
        }
        let len = u32_at(o + 24)? as usize;
        let data = d.get(o + 32..o + 32 + len)?;
        if data.len() > 4 && data[4..].starts_with(b"<?xpacket") {
            return Some(String::from_utf8_lossy(&data[4..]).into_owned());
        }
        o += 32 + len + 32;
    }
    None
}

/// The document's own XMP packet (placed images carry theirs too).
fn document_xmp(d: &[u8]) -> Option<String> {
    if let Some(x) = contiguous_xmp(d) {
        return Some(x);
    }
    let mut best: Option<String> = None;
    for s in memmem::find_iter(d, b"<?xpacket begin") {
        let Some(e) = memmem::find(&d[s..], b"<?xpacket end") else { continue };
        // Packets cut by database page trailers (NUL runs) are stale copies.
        if d[s..s + e].contains(&0) {
            continue;
        }
        let x = String::from_utf8_lossy(&d[s..s + e]);
        let indesign = x.contains("application/x-indesign") || x.contains("<xmp:CreatorTool>Adobe InDesign");
        if indesign && x.contains("xmpTPg:NPages") {
            return Some(x.into_owned());
        }
        if indesign && best.is_none() {
            best = Some(x.into_owned());
        }
    }
    best
}

pub fn parse(d: &[u8]) -> Option<Doc> {
    let xs = document_xmp(d)?;
    let x = xs.as_str();
    let mut facts = Vec::new();
    if let (Some(i), Some(j)) = (x.find("<dc:title>"), x.find("</dc:title>")) {
        if let Some(t) = first(&x[i..j.max(i)], "rdf:li") {
            facts.push(("Title".to_string(), t));
        }
    }
    let pages: usize = first(x, "xmpTPg:NPages").and_then(|v| v.parse().ok()).unwrap_or(0);
    if pages > 0 {
        facts.push(("Pages".into(), pages.to_string()));
    }
    let mut page_size = None;
    if let Some(i) = x.find("xmpTPg:MaxPageSize") {
        let seg = &x[i..(i + 600).min(x.len())];
        let w: Option<f32> = first(seg, "stDim:w").and_then(|v| v.parse().ok());
        let h: Option<f32> = first(seg, "stDim:h").and_then(|v| v.parse().ok());
        let k = common::unit_to_pt(&first(seg, "stDim:unit").unwrap_or_default());
        if let (Some(w), Some(h)) = (w, h) {
            page_size = Some((w * k, h * k));
            facts.push(("Page size".into(), gt_fmt_doc::pdf::paper_size(w * k, h * k)));
        }
    }
    for (tag, label) in [("xmp:CreateDate", "Created"), ("xmp:ModifyDate", "Modified")] {
        if let Some(d) = first(x, tag).and_then(|v| common::xmp_date(&v)) {
            facts.push((label.into(), d));
        }
    }
    if let Some(app) = first(x, "xmp:CreatorTool") {
        facts.push(("Application".into(), app));
    }
    let mut fonts = all(x, "stFnt:fontName");
    fonts.sort();
    fonts.dedup();
    if !fonts.is_empty() {
        let shown: Vec<&str> = fonts.iter().take(12).map(|s| s.as_str()).collect();
        let more = if fonts.len() > 12 { format!(" and {} more", fonts.len() - 12) } else { String::new() };
        facts.push((format!("Fonts ({})", fonts.len()), format!("{}{more}", shown.join(", "))));
    }
    if let Some(i) = x.find("xmpTPg:PlateNames") {
        let end = x[i..].find("</xmpTPg:PlateNames>").map(|j| i + j).unwrap_or(x.len());
        let plates = all(&x[i..end], "rdf:li");
        if !plates.is_empty() {
            facts.push(("Colorants".into(), plates.join(", ")));
        }
    }
    let mut links: Vec<String> = all(x, "stRef:filePath").into_iter().map(|p| percent_decode(p.rsplit(['/', '\\', ':']).next().unwrap_or(&p))).collect();
    links.sort();
    links.dedup();
    if !links.is_empty() {
        let shown: Vec<&str> = links.iter().take(10).map(|s| s.as_str()).collect();
        let more = if links.len() > 10 { format!(" and {} more", links.len() - 10) } else { String::new() };
        facts.push((format!("Links ({})", links.len()), format!("{}{more}", shown.join(", "))));
    }
    // Page previews: <xmp:PageInfo><rdf:Seq><rdf:li> pageNumber, format, image </rdf:li>...
    let mut thumbs = Vec::new();
    if let Some(i) = x.find("<xmp:PageInfo>") {
        let end = x[i..].find("</xmp:PageInfo>").map(|j| i + j).unwrap_or(x.len());
        let block = &x[i..end];
        for (k, li) in block.split("<rdf:li").skip(1).enumerate() {
            let n = first(li, "xmpGImg:pageNumber").and_then(|v| v.parse().ok()).unwrap_or(k + 1);
            if let Some(img) = first(li, "xmpGImg:image") {
                thumbs.push((n, img));
            }
        }
    }
    if thumbs.is_empty() {
        if let Some(img) = first(x, "xmpGImg:image") {
            thumbs.push((1, img));
        }
    }
    Some(Doc { facts, pages, page_size, thumbs })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// IDML export of the same document next to it ("x.indd" → "x.idml").
/// Only when it is at least as recent as the INDD: an older export would show
/// a different version of the document.
pub fn sibling_idml(p: &std::path::Path) -> Option<std::path::PathBuf> {
    let c = p.with_extension("idml");
    let mt = |q: &std::path::Path| std::fs::metadata(q).and_then(|m| m.modified()).ok();
    let (Some(a), Some(b)) = (mt(p), mt(&c)) else { return None };
    (b + std::time::Duration::from_secs(300) >= a).then_some(c)
}

pub struct InddProvider;

impl Provider for InddProvider {
    fn id(&self) -> &'static str {
        "indd"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        match (p.head.starts_with(MAGIC), p.ext_is(&["indd", "indt", "indb"])) {
            (true, _) => 95,
            (false, true) => 40,
            _ => 0,
        }
    }

    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let file = std::fs::File::open(&p.path).map_err(|e| e.to_string())?;
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
        let doc = parse(&map);
        let mut facts = doc.as_ref().map(|d| d.facts.clone()).unwrap_or_default();
        facts.insert(0, ("Format".into(), "Adobe InDesign document".into()));
        // A full rendering from the IDML export when there is one.
        if let Some(idml) = sibling_idml(&p.path) {
            match crate::idml::load(&idml, cancel) {
                Ok((src, mut f)) => {
                    f.retain(|(k, _)| k != "Format");
                    facts.retain(|(k, _)| !f.iter().any(|(k2, _)| k2 == k));
                    facts.extend(f);
                    facts.push(("Rendered from".into(), format!("{} (IDML export next to it)", idml.file_name().unwrap_or_default().to_string_lossy())));
                    let note = format!("Drawn from {} next to this file", idml.file_name().unwrap_or_default().to_string_lossy());
                    return Ok(Box::new(common::PagesLoaded { src, facts, note: Some(note) }));
                }
                Err(e) => tracing::warn!("sibling IDML {}: {e}", idml.display()),
            }
        }
        let Some(doc) = doc else {
            return Ok(common::card(p, "No metadata or previews found in this InDesign file.", facts));
        };
        let mut pages = Vec::new();
        for (_, b64) in &doc.thumbs {
            let clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
            let Some(bytes) = gt_fmt_media::probe::base64_decode(&clean) else { continue };
            let Some(r) = common::decode(&bytes, 2048) else { continue };
            // Previews are padded to a square: keep the page itself.
            let r = trim_padding(r);
            let h_pt = doc.page_size.map(|s| s.1).unwrap_or(842.0);
            let size = (h_pt * r.w as f32 / r.h.max(1) as f32, h_pt);
            pages.push(RasterPage { raster: r, size });
        }
        if pages.is_empty() {
            return Ok(common::card(p, "InDesign saved no page previews in this file (Preferences › File Handling › Save Preview Images).", facts));
        }
        let note = if doc.pages > pages.len() {
            format!("InDesign saved previews of {} of {} pages; an IDML export next to it shows them all", pages.len(), doc.pages)
        } else {
            format!("Previews of the first {} page{} saved by InDesign; an IDML export next to it shows them all", pages.len(), if pages.len() == 1 { "" } else { "s" })
        };
        let src = RasterSource { pages, paper: true, dip_per_unit: 96.0 / 72.0 };
        Ok(Box::new(common::PagesLoaded { src: Arc::new(src), facts, note: Some(note) }))
    }
}

/// InDesign pads page previews to a square with a flat colour: cut it off.
fn trim_padding(r: gt_fmt_doc::pages::Raster) -> gt_fmt_doc::pages::Raster {
    let (w, h) = (r.w as usize, r.h as usize);
    if w < 4 || h < 4 {
        return r;
    }
    let px = |x: usize, y: usize| {
        let o = (y * w + x) * 4;
        [r.data[o], r.data[o + 1], r.data[o + 2]]
    };
    let bg = px(0, 0);
    // Only when the corners agree (a real padding colour).
    if [px(w - 1, 0), px(0, h - 1), px(w - 1, h - 1)].iter().any(|c| c != &bg) {
        return r;
    }
    let same = |c: [u8; 3]| (0..3).all(|k| (c[k] as i32 - bg[k] as i32).abs() <= 6);
    let row_bg = |y: usize| (0..w).all(|x| same(px(x, y)));
    let col_bg = |x: usize| (0..h).all(|y| same(px(x, y)));
    let mut y0 = 0;
    while y0 < h / 2 && row_bg(y0) {
        y0 += 1;
    }
    let mut y1 = h;
    while y1 > h / 2 && row_bg(y1 - 1) {
        y1 -= 1;
    }
    let mut x0 = 0;
    while x0 < w / 2 && col_bg(x0) {
        x0 += 1;
    }
    let mut x1 = w;
    while x1 > w / 2 && col_bg(x1 - 1) {
        x1 -= 1;
    }
    // Padding is on two opposite sides only; a page with a white margin keeps it.
    let horizontal = x0 > 0 && w - x1 > 0 && y0 == 0 && y1 == h;
    let vertical = y0 > 0 && h - y1 > 0 && x0 == 0 && x1 == w;
    if !(horizontal || vertical) {
        return r;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut data = Vec::with_capacity(cw * ch * 4);
    for y in y0..y1 {
        data.extend_from_slice(&r.data[(y * w + x0) * 4..(y * w + x1) * 4]);
    }
    gt_fmt_doc::pages::Raster { w: cw as u32, h: ch as u32, data }
}
