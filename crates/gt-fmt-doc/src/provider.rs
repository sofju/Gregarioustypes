//! Providers: PDF, Illustrator, EPS/PostScript, SVG, and office documents
//! converted by LibreOffice.

use crate::pages::{PageSource, Raster};
use crate::paged::PagedView;
use crate::staged::{Converted, StagedView};
use crate::{ghostscript, office, pdf, props, svg};
use gt_core::CancelToken;
use gt_ui::widgets::card::{CardData, InfoCard};
use gt_ui::{Loaded, Probe, Provider, Upgrade, View, ViewEnv};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

/// System DPI scale (the window is usually on the primary monitor).
fn dpi_scale() -> f32 {
    (unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0).max(1.0)
}

/// Render the first page at the scale the view will most likely use, so the
/// first frame already shows the page (approximates the window's sizing).
fn first_raster(src: &dyn PageSource, screen: (u32, u32)) -> Option<(f32, Raster)> {
    let n = src.count();
    if n == 0 || screen.0 == 0 {
        return None;
    }
    let s = dpi_scale();
    let (pw, ph) = src.size(0);
    let unit = src.dip_per_unit() * s;
    let m = ((if n > 1 { 16.0 } else { 24.0 }) * s).round();
    let ch = (40.0 * s).round();
    let (mw, mh) = (screen.0 as f32 * 0.88, (screen.1 as f32 - 48.0 * s) * 0.88);
    let (avail_w, avail_h) = (mw, mh - ch);
    let mut k = ((avail_w * 0.92 - 2.0 * m) / (pw * unit)).min((avail_h * 0.94 - 2.0 * m) / (ph * unit));
    if !src.paper() {
        k = k.min(1.0).max(((360.0 * s) / (pw * unit).max(ph * unit)).min(k));
    }
    let extra = if n > 1 { 14.0 * s } else { 0.0 };
    let vw = (pw * unit * k + 2.0 * m + extra).max(320.0 * s).clamp(460.0 * s, mw).round();
    let vh = ((ph * unit * k + 2.0 * m).max(240.0 * s) + ch).clamp(240.0 * s, mh).round() - ch;
    let z = if n > 1 {
        let maxw = (0..n.min(64)).map(|i| src.size(i).0).fold(1.0f32, f32::max);
        (vw - 2.0 * m - 8.0 * s) / (maxw * unit)
    } else {
        ((vw - 2.0 * m) / (pw * unit)).min((vh - 2.0 * m) / (ph * unit))
    };
    let scale = src.dip_per_unit() * z * s;
    let (w, h) = ((pw * scale).round().max(1.0) as u32, (ph * scale).round().max(1.0) as u32);
    if (w as u64) * (h as u64) > 24_000_000 {
        return None;
    }
    let t = std::time::Instant::now();
    let r = src.render(0, scale, 0, 0, w, h)?;
    tracing::debug!("first page {w}×{h} in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    Some((scale, r))
}

pub(crate) fn card(p: &Probe, note: String, facts: Vec<(String, String)>) -> Box<dyn Loaded> {
    let icon = gt_shell::info::image(&p.path.display().to_string(), 256, false).map(|b| (b.width, b.height, b.data));
    let mut f = vec![("Size".to_string(), gt_core::fmt::size_long(p.size))];
    f.extend(facts);
    Box::new(CardLoaded(CardData { icon, name: p.name(), kind: gt_shell::info::type_name(&p.path.display().to_string()), facts: f, note: Some(note) }))
}

struct CardLoaded(CardData);

impl Loaded for CardLoaded {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        Box::new(InfoCard::new(&env.gfx.dc, self.0))
    }
}

/// Pages ready to show.
struct PagesLoaded {
    src: Arc<dyn PageSource>,
    facts: Vec<(String, String)>,
    first: Option<(f32, Raster)>,
}

impl Loaded for PagesLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        let mut v = PagedView::new(self.src, self.facts);
        if let Some((s, r)) = self.first {
            v = v.with_first(0, s, r);
        }
        Box::new(v)
    }
}

/// Waiting for a conversion; the upgrade delivers the pages.
struct StagedLoaded {
    facts: Vec<(String, String)>,
    preview: Option<Raster>,
    aspect: f32,
    status: Arc<Mutex<String>>,
    job: Option<Box<dyn FnOnce(&CancelToken, &Arc<Mutex<String>>) -> Converted + Send>>,
}

impl Loaded for StagedLoaded {
    fn take_upgrade(&mut self) -> Option<Upgrade> {
        let job = self.job.take()?;
        let status = self.status.clone();
        Some(Box::new(move |cancel: &CancelToken| {
            let r = job(cancel, &status);
            if cancel.is_cancelled() {
                return None;
            }
            Some(Box::new(r) as Box<dyn std::any::Any + Send>)
        }))
    }

    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        Box::new(StagedView::new(self.status, self.facts, self.aspect).with_preview(self.preview))
    }
}

fn open_pdf_pages(path: &std::path::Path) -> Converted {
    let src = pdf::open(path).map_err(|e| e.to_string())?;
    let facts = pdf::facts(&src);
    Ok((src as Arc<dyn PageSource>, facts))
}

fn has_pdf_magic(head: &[u8]) -> bool {
    head.len() >= 5 && head[..head.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

fn is_postscript(head: &[u8]) -> bool {
    head.starts_with(b"%!") || ghostscript::dos_eps(head).is_some()
}

pub struct PdfProvider;

impl Provider for PdfProvider {
    fn id(&self) -> &'static str {
        "pdf"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        let magic = has_pdf_magic(&p.head);
        match (p.ext.as_str(), magic) {
            ("pdf" | "ai", true) => 95,
            ("pdf", false) => 40,
            (_, true) if p.head.starts_with(b"%PDF-") => 85,
            _ => 0,
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let src = match pdf::open(&p.path) {
            Ok(s) => s,
            Err(crate::pdfium::OpenError::Password) => return Ok(card(p, "This PDF is password protected.".into(), Vec::new())),
            Err(e) => return Err(e.to_string()),
        };
        let mut facts = pdf::facts(&src);
        if p.ext == "ai" {
            if pdf::is_ai_placeholder(&src) {
                return ai_without_pdf(p);
            }
            facts.retain(|(k, _)| k != "PDF version");
            facts.insert(0, ("Format".into(), "Adobe Illustrator (PDF-compatible)".into()));
            let n = src.doc.page_count();
            if n > 1 {
                facts.retain(|(k, _)| k != "Pages");
                facts.insert(1, ("Artboards".into(), n.to_string()));
            }
        }
        if src.doc.page_count() == 0 {
            return Err("This PDF has no pages.".into());
        }
        let first = first_raster(&*src, p.screen);
        Ok(Box::new(PagesLoaded { src, facts, first }))
    }
}

/// The base64 JPEG thumbnail in an Illustrator file's XMP (`xmpGImg:image`).
fn xmp_thumbnail(path: &std::path::Path) -> Option<Raster> {
    let data = std::fs::read(path).ok()?;
    let key = b"xmpGImg:image>";
    let i = data.windows(key.len()).position(|w| w == key)? + key.len();
    let end = i + data[i..].iter().position(|&b| b == b'<')?;
    let text = String::from_utf8_lossy(&data[i..end]).replace("&#xA;", "").replace("&#xa;", "");
    let bytes = gt_fmt_media::probe::base64_decode(&text)?;
    let b = gt_fmt_media::cover::decode_bytes(&bytes, 2048)?;
    Some(Raster { w: b.width, h: b.height, data: b.data })
}

fn ai_without_pdf(p: &Probe) -> Result<Box<dyn Loaded>, String> {
    let facts = vec![("Format".to_string(), "Adobe Illustrator (native data only)".to_string())];
    let preview = xmp_thumbnail(&p.path);
    let note = if preview.is_some() {
        "Saved without PDF content: showing the thumbnail stored in the file"
    } else {
        "Saved without PDF content and without a thumbnail: only Illustrator can draw it"
    };
    let status = Arc::new(Mutex::new(note.to_string()));
    Ok(Box::new(FinalPreview { facts, preview, status }))
}

/// A preview image with a permanent note (no conversion follows).
struct FinalPreview {
    facts: Vec<(String, String)>,
    preview: Option<Raster>,
    status: Arc<Mutex<String>>,
}

impl Loaded for FinalPreview {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        Box::new(StagedView::new(self.status, self.facts, 1.0).with_preview(self.preview).finished())
    }
}

pub struct PostScriptProvider;

const PS_EXTS: &[&str] = &["eps", "epsf", "epsi", "ps", "ai"];

impl Provider for PostScriptProvider {
    fn id(&self) -> &'static str {
        "postscript"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.ext_is(PS_EXTS) {
            return 0;
        }
        if is_postscript(&p.head) {
            90
        } else {
            0
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let dos = ghostscript::dos_eps(&p.head);
        let text = String::from_utf8_lossy(&p.head[..p.head.len().min(4096)]).to_string();
        let eps = p.ext != "ps" || text.contains("EPSF");
        let kind = if eps { ghostscript::Kind::Eps } else { ghostscript::Kind::Ps };
        let mut facts = Vec::new();
        let format = if p.ext == "ai" {
            "Adobe Illustrator (PostScript)"
        } else if dos.is_some() {
            "EPS with preview"
        } else if eps {
            "Encapsulated PostScript"
        } else {
            "PostScript"
        };
        facts.push(("Format".to_string(), format.to_string()));
        for key in ["%%Title:", "%%Creator:", "%%CreationDate:", "%%For:"] {
            if let Some(i) = text.find(key) {
                let v = text[i + key.len()..].lines().next().unwrap_or("").trim().trim_matches(['(', ')']).to_string();
                let label = match key {
                    "%%Title:" => "Title",
                    "%%Creator:" => "Application",
                    "%%CreationDate:" => "Created",
                    _ => "Author",
                };
                if !v.is_empty() {
                    facts.push((label.into(), v));
                }
            }
        }
        if let Some((w, h)) = ghostscript::bounding_box(&p.head) {
            facts.push(("Bounding box".into(), format!("{:.0} × {:.0} pt ({:.0} × {:.0} mm)", w, h, w / 72.0 * 25.4, h / 72.0 * 25.4)));
        }
        let cached = crate::helper::cache_file("gs", &p.path, "gs10080-v1", "pdf");
        if cached.is_file() {
            if let Ok((src, mut f)) = open_pdf_pages(&cached) {
                f.retain(|(k, _)| k == "Pages" || k == "Page size");
                facts.extend(f);
                let first = first_raster(&*src, p.screen);
                return Ok(Box::new(PagesLoaded { src, facts, first }));
            }
        }
        let preview = dos.as_ref().and_then(|d| ghostscript::dos_preview(&p.path, d));
        let aspect = ghostscript::bounding_box(&p.head).map(|(w, h)| w / h).unwrap_or(0.77);
        let path = p.path.clone();
        let status = Arc::new(Mutex::new("Rendering with Ghostscript…".to_string()));
        Ok(Box::new(StagedLoaded {
            facts,
            preview,
            aspect,
            status,
            job: Some(Box::new(move |cancel: &CancelToken, _st: &Arc<Mutex<String>>| {
                let pdf_path = ghostscript::to_pdf(&path, kind, cancel)?;
                let (src, mut f) = open_pdf_pages(&pdf_path)?;
                f.retain(|(k, _)| k == "Pages" || k == "Page size");
                Ok((src, f))
            })),
        }))
    }
}

pub struct SvgProvider;

impl Provider for SvgProvider {
    fn id(&self) -> &'static str {
        "svg"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        match p.ext.as_str() {
            "svg" | "svgz" => 90,
            _ => {
                let t = String::from_utf8_lossy(&p.head[..p.head.len().min(1024)]);
                if t.trim_start_matches('\u{feff}').trim_start().starts_with("<svg") {
                    60
                } else {
                    0
                }
            }
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let (src, facts) = svg::open(&p.path)?;
        let first = first_raster(&*src, p.screen);
        Ok(Box::new(PagesLoaded { src, facts, first }))
    }
}

/// Formats LibreOffice converts to PDF.
pub const OFFICE_EXTS: &[&str] = &[
    "doc", "dot", "docx", "docm", "dotx", "dotm", "odt", "ott", "fodt", "wpd", "wps", "ppt", "pps", "pot", "pptx", "pptm", "ppsx", "ppsm", "potx", "odp", "otp", "fodp",
    "odg", "otg", "fodg", "vsd", "vsdx", "pub", "key", "pages", "numbers", "xls", "xlsx", "xlsm", "xlsb", "ods", "fods",
];

pub struct OfficeProvider;

impl Provider for OfficeProvider {
    fn id(&self) -> &'static str {
        "office"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.ext_is(OFFICE_EXTS) || !office::available() {
            return 0;
        }
        // Spreadsheets open in the grid first; this provider is their "layout" view.
        if crate::sheet::EXTS.contains(&p.ext.as_str()) {
            return 30;
        }
        // An office document is selected or a neighbour of the selection: start
        // LibreOffice now so its conversion does not wait for start-up.
        office::prewarm();
        let ole = p.head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]);
        let zip = p.head.starts_with(b"PK");
        if ole || zip || p.head.starts_with(b"<?xml") {
            80
        } else {
            50
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let pr = props::read(&p.path, &p.head);
        if pr.encrypted {
            return Ok(card(p, "This document is password protected.".into(), pr.facts));
        }
        office_staged(&p.path, &p.head, p.screen)
    }
}

/// A LibreOffice-converted document: cached PDF right away, else a staged view.
pub fn office_staged(path: &std::path::Path, head: &[u8], screen: (u32, u32)) -> Result<Box<dyn Loaded>, String> {
    let pr = props::read(path, head);
    let mut facts = pr.facts;
    let cached = office::cached_pdf(path);
    if cached.is_file() {
        if let Ok((src, f)) = open_pdf_pages(&cached) {
            merge_pdf_facts(&mut facts, f);
            let first = first_raster(&*src, screen);
            return Ok(Box::new(PagesLoaded { src, facts, first }));
        }
    }
    let aspect = match pr.kind {
        Some("presentation") => 16.0 / 9.0,
        Some("spreadsheet") => 1.414,
        _ => 0.707,
    };
    let path: PathBuf = path.to_path_buf();
    let status = Arc::new(Mutex::new("Converting with LibreOffice…".to_string()));
    Ok(Box::new(StagedLoaded {
        facts,
        preview: pr.thumbnail,
        aspect,
        status,
        job: Some(Box::new(move |cancel: &CancelToken, st: &Arc<Mutex<String>>| {
            let st2 = st.clone();
            let pdf_path = office::to_pdf(&path, cancel, &move |s| *st2.lock() = s)?;
            let (src, f) = open_pdf_pages(&pdf_path)?;
            let mut keep = Vec::new();
            merge_pdf_facts(&mut keep, f);
            Ok((src, keep))
        })),
    }))
}

/// Keep the PDF's page count/size; the original document's own properties win.
fn merge_pdf_facts(facts: &mut Vec<(String, String)>, pdf: Vec<(String, String)>) {
    for (k, v) in pdf {
        if k == "Page size" {
            facts.push((k, v));
        } else if k == "Pages" && !facts.iter().any(|(k2, _)| k2 == "Pages" || k2 == "Slides") {
            let v = if v.parse::<u32>().ok() == Some(office::MAX_PAGES) { format!("{v} (preview limit)") } else { v };
            facts.push((k, v));
        }
    }
}

/// Office documents whose conversion is worth starting LibreOffice for in advance.
pub fn wants_office(ext: &str) -> bool {
    OFFICE_EXTS.contains(&ext) && !crate::sheet::EXTS.contains(&ext)
}
