//! PDF documents (and anything converted to PDF) as a page source, plus the
//! facts shown in the Info panel.

use crate::pages::{PageSource, Raster};
use crate::pdfium::{Document, OpenError};
use std::path::Path;
use std::sync::Arc;

pub struct PdfSource {
    pub doc: Document,
}

impl PageSource for PdfSource {
    fn count(&self) -> usize {
        self.doc.page_count()
    }
    fn size(&self, page: usize) -> (f32, f32) {
        self.doc.page_size(page)
    }
    fn render(&self, page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster> {
        self.doc.render(page, scale, x, y, w, h).map(|data| Raster { w, h, data })
    }
    fn text(&self, page: usize) -> Option<String> {
        self.doc.page_text(page)
    }
    fn label(&self, page: usize) -> Option<String> {
        let l = self.doc.page_label(page);
        (!l.is_empty()).then_some(l)
    }
}

pub fn open(path: &Path) -> Result<Arc<PdfSource>, OpenError> {
    Ok(Arc::new(PdfSource { doc: Document::open(path)? }))
}

/// "D:20240105123000+01'00'" → "05/01/2024 12:30" in the user's format.
pub fn pdf_date(s: &str) -> Option<String> {
    let d = s.trim().trim_start_matches("D:");
    let num = |a: usize, b: usize| d.get(a..b).and_then(|x| x.parse::<u16>().ok());
    let y = num(0, 4)?;
    let mo = num(4, 6).unwrap_or(1);
    let da = num(6, 8).unwrap_or(1);
    let h = num(8, 10).unwrap_or(0);
    let mi = num(10, 12).unwrap_or(0);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return Some(y.to_string());
    }
    // Days since the Unix epoch (civil calendar), then the local formatter.
    let (yy, mm) = if mo <= 2 { (y as i64 - 1, mo as i64 + 9) } else { (y as i64, mo as i64 - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + da as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut secs = days * 86400 + h as i64 * 3600 + mi as i64 * 60;
    // Time-zone offset ("+01'00'", "Z"): convert to UTC, the formatter shows local time.
    if let Some(tz) = d.get(14..) {
        let sign = match tz.as_bytes().first() {
            Some(b'+') => -1,
            Some(b'-') => 1,
            _ => 0,
        };
        if sign != 0 {
            let digits: String = tz[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let oh = digits.get(0..2).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            let om = digits.get(2..4).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            secs += sign * (oh * 3600 + om * 60);
        }
    }
    Some(gt_core::fmt::unix_time_short(secs))
}

/// "210 × 297 mm (A4)".
pub fn paper_size(w: f32, h: f32) -> String {
    const NAMES: &[(&str, f32, f32)] = &[
        ("A3", 841.9, 1190.6),
        ("A4", 595.3, 841.9),
        ("A5", 419.5, 595.3),
        ("B5", 498.9, 708.7),
        ("Letter", 612.0, 792.0),
        ("Legal", 612.0, 1008.0),
        ("Tabloid", 792.0, 1224.0),
        ("Executive", 522.0, 756.0),
    ];
    let (a, b) = (w.min(h), w.max(h));
    let name = NAMES.iter().find(|(_, x, y)| (a - x).abs() < 3.0 && (b - y).abs() < 3.0).map(|(n, _, _)| *n);
    let mm = |p: f32| p / 72.0 * 25.4;
    let mut s = format!("{:.0} × {:.0} mm", mm(w), mm(h));
    if let Some(n) = name {
        s.push_str(&format!(" ({n}{})", if w > h { ", landscape" } else { "" }));
    } else {
        s.push_str(&format!(" ({:.2} × {:.2} in)", w / 72.0, h / 72.0));
    }
    s
}

pub fn facts(src: &PdfSource) -> Vec<(String, String)> {
    let d = &src.doc;
    let mut f = Vec::new();
    for (tag, label) in [("Title", "Title"), ("Author", "Author"), ("Subject", "Subject"), ("Keywords", "Keywords")] {
        let v = d.meta(tag);
        if !v.is_empty() {
            f.push((label.to_string(), v));
        }
    }
    let n = d.page_count();
    f.push(("Pages".into(), gt_core::fmt::group(n as u64)));
    if n > 0 {
        let (w, h) = d.page_size(0);
        let mixed = (1..n.min(200)).any(|i| {
            let (a, b) = d.page_size(i);
            (a - w).abs() > 1.0 || (b - h).abs() > 1.0
        });
        f.push(("Page size".into(), format!("{}{}", paper_size(w, h), if mixed { ", mixed sizes" } else { "" })));
    }
    for (tag, label) in [("CreationDate", "Created"), ("ModDate", "Modified")] {
        if let Some(v) = pdf_date(&d.meta(tag)) {
            f.push((label.to_string(), v));
        }
    }
    for (tag, label) in [("Creator", "Application"), ("Producer", "Producer")] {
        let v = d.meta(tag);
        if !v.is_empty() {
            f.push((label.to_string(), v));
        }
    }
    if let Some(v) = d.version() {
        f.push(("PDF version".into(), v));
    }
    if let Some(r) = d.security_revision() {
        let algo = match r {
            2 => "RC4 40-bit",
            3 => "RC4 128-bit",
            4 => "RC4/AES 128-bit",
            5 | 6 => "AES 256-bit",
            _ => "standard security",
        };
        f.push(("Encryption".into(), format!("{algo} (opens without a password)")));
    }
    let outline = d.outline(1);
    if !outline.is_empty() {
        f.push(("Bookmarks".into(), "Yes".into()));
    }
    f
}

/// Illustrator files saved without "Create PDF Compatible File" hold only a
/// placeholder page telling you so.
pub fn is_ai_placeholder(src: &PdfSource) -> bool {
    src.doc.page_count() <= 1 && src.doc.page_text(0).is_some_and(|t| t.contains("without PDF Content") || t.contains("without PDF content"))
}
