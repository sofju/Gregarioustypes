//! RTF laid out into pages by the system RichEdit (msftedit.dll) and recorded as
//! vector EMF pages: shown in the page viewer like a PDF (crisp at any zoom,
//! text copy per page). The preview window has no GDI surface, so a live
//! RichEdit child window could not be shown in it anyway.

use crate::pages::{PageSource, Raster};
use crate::paged::PagedView;
use gt_core::CancelToken;
use gt_ui::{Loaded, Probe, Provider, View, ViewEnv};
use std::sync::Arc;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::UI::Controls::RichEdit::{
    CHARRANGE, EDITSTREAM, EM_EXLIMITTEXT, EM_FORMATRANGE, EM_GETTEXTLENGTHEX, EM_GETTEXTRANGE, EM_STREAMIN, FORMATRANGE, GETTEXTLENGTHEX, GTL_NUMCHARS, GTL_PRECISE, SF_RTF, TEXTRANGEW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// `{\info{\title X}{\author Y}{\creatim\yr2024\mo3\dy5\hr10\min20}...}`.
fn info(data: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(&data[..data.len().min(256 * 1024)]);
    let mut f = Vec::new();
    let Some(i) = text.find("{\\info") else { return f };
    let block = &text[i..];
    let end = {
        let mut depth = 0i32;
        let mut e = block.len();
        for (k, ch) in block.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        e = k;
                        break;
                    }
                }
                _ => {}
            }
        }
        e
    };
    let block = &block[..end];
    let field = |name: &str| -> Option<String> {
        let k = format!("{{\\{name}");
        let i = block.find(&k)? + k.len();
        let j = block[i..].find('}')?;
        let raw = block[i..i + j].trim();
        Some(decode_rtf_text(raw)).filter(|s| !s.is_empty())
    };
    for (n, label) in [("title", "Title"), ("subject", "Subject"), ("author", "Author"), ("company", "Company"), ("operator", "Last saved by")] {
        if let Some(v) = field(n) {
            f.push((label.to_string(), v));
        }
    }
    let date = |name: &str| -> Option<String> {
        let k = format!("{{\\{name}");
        let i = block.find(&k)? + k.len();
        let j = block[i..].find('}')?;
        let s = &block[i..i + j];
        let num = |key: &str| -> u32 {
            s.find(key).map(|p| s[p + key.len()..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)).unwrap_or(0)
        };
        let y = num("\\yr");
        (y > 1900).then(|| format!("D:{:04}{:02}{:02}{:02}{:02}", y, num("\\mo").max(1), num("\\dy").max(1), num("\\hr"), num("\\min")))
    };
    for (n, label) in [("creatim", "Created"), ("revtim", "Modified")] {
        if let Some(d) = date(n).and_then(|d| crate::pdf::pdf_date(&d)) {
            f.push((label.to_string(), d));
        }
    }
    for (n, label) in [("nofpages", "Pages"), ("nofwords", "Words")] {
        let k = format!("\\{n}");
        if let Some(p) = block.find(&k) {
            let v: String = block[p + k.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if !v.is_empty() && v != "0" {
                f.push((label.to_string(), v));
            }
        }
    }
    f
}

/// Plain text of a simple RTF run: `\'e9` escapes and `\uN?` code points.
fn decode_rtf_text(s: &str) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            if b[i + 1] == b'\'' && i + 3 < b.len() {
                if let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16) {
                    out.push_str(&gt_core::encoding::decode_1252(&[v]));
                }
                i += 4;
                continue;
            }
            if b[i + 1] == b'u' && b.get(i + 2).is_some_and(|c| c.is_ascii_digit() || *c == b'-') {
                let mut j = i + 2;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'-') {
                    j += 1;
                }
                if let Ok(n) = s[i + 2..j].parse::<i32>() {
                    let n = if n < 0 { n + 65536 } else { n } as u32;
                    if let Some(c) = char::from_u32(n) {
                        out.push(c);
                    }
                }
                // Skip the ANSI fallback character.
                i = j + if b.get(j) == Some(&b' ') { 1 } else { 0 };
                if i < b.len() && b[i] != b'\\' {
                    i += 1;
                }
                continue;
            }
            // Other control word: skip it and one trailing space.
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'-') {
                j += 1;
            }
            if j < b.len() && b[j] == b' ' {
                j += 1;
            }
            i = j.max(i + 2);
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out.trim().to_string()
}

/// Page size and margins in twips from `\paperw \paperh \margl \margr \margt \margb`.
fn page_setup(data: &[u8]) -> (i32, i32, [i32; 4]) {
    let text = String::from_utf8_lossy(&data[..data.len().min(64 * 1024)]);
    let num = |key: &str, def: i32| -> i32 {
        let k = format!("\\{key}");
        let mut from = 0;
        while let Some(i) = text[from..].find(&k) {
            let at = from + i + k.len();
            let v: String = text[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if !v.is_empty() {
                return v.parse().unwrap_or(def);
            }
            from = at;
        }
        def
    };
    // RTF defaults: US Letter, 1.25" side and 1" top/bottom margins.
    let w = num("paperw", 12240).clamp(1440, 1440 * 60);
    let h = num("paperh", 15840).clamp(1440, 1440 * 60);
    let m = [num("margl", 1800), num("margt", 1440), num("margr", 1800), num("margb", 1440)];
    let m = m.map(|v| v.clamp(0, 1440 * 4));
    (w, h, m)
}

struct Page {
    emf: HENHMETAFILE,
    text: String,
}

/// RTF laid out into pages by RichEdit (EM_FORMATRANGE), each recorded as a
/// vector EMF, so any page renders at any zoom from any thread.
pub struct RtfSource {
    pages: Vec<Page>,
    /// Page size in points.
    size: (f32, f32),
}

unsafe impl Send for RtfSource {}
unsafe impl Sync for RtfSource {}

impl Drop for RtfSource {
    fn drop(&mut self) {
        for p in &self.pages {
            unsafe {
                let _ = DeleteEnhMetaFile(Some(p.emf));
            }
        }
    }
}

unsafe extern "system" fn stream_in(cookie: usize, buf: *mut u8, cb: i32, pcb: *mut i32) -> u32 {
    let c = &mut *(cookie as *mut (&[u8], usize));
    let n = (cb as usize).min(c.0.len() - c.1);
    std::ptr::copy_nonoverlapping(c.0.as_ptr().add(c.1), buf, n);
    c.1 += n;
    *pcb = n as i32;
    0
}

const MAX_PAGES: usize = 2000;

impl RtfSource {
    /// Paginate on the calling thread (a hidden RichEdit lives only during this call).
    pub fn new(data: &[u8], cancel: &CancelToken) -> Result<RtfSource, String> {
        unsafe {
            LoadLibraryW(w!("msftedit.dll")).map_err(|e| format!("RichEdit unavailable: {e}"))?;
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let style = WS_POPUP | WINDOW_STYLE((ES_MULTILINE | ES_READONLY) as u32);
            let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("RICHEDIT50W"), w!(""), style, 0, 0, 800, 600, None, None, Some(hinst.into()), None).map_err(|e| e.to_string())?;
            let r = Self::paginate(h, data, cancel);
            let _ = DestroyWindow(h);
            r
        }
    }

    unsafe fn paginate(h: HWND, data: &[u8], cancel: &CancelToken) -> Result<RtfSource, String> {
        SendMessageW(h, EM_EXLIMITTEXT, Some(WPARAM(0)), Some(LPARAM(isize::MAX)));
        let mut cur: (&[u8], usize) = (data, 0);
        let es = EDITSTREAM { dwCookie: &mut cur as *mut _ as usize, dwError: 0, pfnCallback: Some(stream_in) };
        SendMessageW(h, EM_STREAMIN, Some(WPARAM(SF_RTF as usize)), Some(LPARAM(&es as *const _ as isize)));
        let gtl = GETTEXTLENGTHEX { flags: GTL_NUMCHARS | GTL_PRECISE, codepage: 1200 };
        let len = SendMessageW(h, EM_GETTEXTLENGTHEX, Some(WPARAM(&gtl as *const _ as usize)), Some(LPARAM(0))).0 as i32;
        let (pw, ph, m) = page_setup(data);
        let screen = GetDC(None);
        let dpi_x = GetDeviceCaps(Some(screen), LOGPIXELSX).max(1);
        let dpi_y = GetDeviceCaps(Some(screen), LOGPIXELSY).max(1);
        // EMF frame in 0.01 mm.
        let frame = RECT { left: 0, top: 0, right: (pw as i64 * 2540 / 1440) as i32, bottom: (ph as i64 * 2540 / 1440) as i32 };
        let page_rc = RECT { left: 0, top: 0, right: pw, bottom: ph };
        let body_rc = RECT { left: m[0], top: m[1], right: (pw - m[2]).max(m[0] + 720), bottom: (ph - m[3]).max(m[1] + 720) };
        let mut pages = Vec::new();
        let mut start = 0i32;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let mdc = CreateEnhMetaFileW(Some(screen), PCWSTR::null(), Some(&frame), PCWSTR::null());
            if mdc.is_invalid() {
                break;
            }
            // Twips → device units of the reference DC, so the EMF maps 1:1 onto its frame.
            let _ = SetMapMode(mdc, MM_ANISOTROPIC);
            let _ = SetWindowExtEx(mdc, (pw as i64 * dpi_x as i64 / 1440) as i32, (ph as i64 * dpi_y as i64 / 1440) as i32, None);
            let _ = SetViewportExtEx(mdc, (pw as i64 * dpi_x as i64 / 1440) as i32, (ph as i64 * dpi_y as i64 / 1440) as i32, None);
            let fr = FORMATRANGE { hdc: mdc, hdcTarget: screen, rc: body_rc, rcPage: page_rc, chrg: CHARRANGE { cpMin: start, cpMax: -1 } };
            let next = SendMessageW(h, EM_FORMATRANGE, Some(WPARAM(1)), Some(LPARAM(&fr as *const _ as isize))).0 as i32;
            let emf = CloseEnhMetaFile(mdc);
            // Page text, for copy.
            let end = if next > start { next } else { len };
            let mut buf = vec![0u16; (end - start).max(0) as usize + 1];
            let tr = TEXTRANGEW { chrg: CHARRANGE { cpMin: start, cpMax: end }, lpstrText: PWSTR(buf.as_mut_ptr()) };
            let got = SendMessageW(h, EM_GETTEXTRANGE, Some(WPARAM(0)), Some(LPARAM(&tr as *const _ as isize))).0 as usize;
            let text = String::from_utf16_lossy(&buf[..got.min(buf.len())]).replace('\r', "\n");
            pages.push(Page { emf, text });
            if next <= start || next >= len || pages.len() >= MAX_PAGES {
                break;
            }
            start = next;
        }
        SendMessageW(h, EM_FORMATRANGE, Some(WPARAM(0)), Some(LPARAM(0)));
        ReleaseDC(None, screen);
        if pages.is_empty() {
            return Err("RichEdit could not lay out this document.".into());
        }
        Ok(RtfSource { pages, size: (pw as f32 / 20.0, ph as f32 / 20.0) })
    }
}

impl PageSource for RtfSource {
    fn count(&self) -> usize {
        self.pages.len()
    }

    fn size(&self, _page: usize) -> (f32, f32) {
        self.size
    }

    fn render(&self, page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster> {
        let p = self.pages.get(page)?;
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, bmp.into());
            let n = (w * h) as usize;
            std::ptr::write_bytes(bits as *mut u8, 0xFF, n * 4);
            let (pw, ph) = (self.size.0 * scale, self.size.1 * scale);
            let dst = RECT { left: -x, top: -y, right: (pw.round() as i32) - x, bottom: (ph.round() as i32) - y };
            let _ = PlayEnhMetaFile(dc, p.emf, &dst);
            let _ = GdiFlush();
            let mut data = std::slice::from_raw_parts(bits as *const u8, n * 4).to_vec();
            // GDI leaves alpha undefined: the page is opaque.
            for px in data.chunks_exact_mut(4) {
                px[3] = 255;
            }
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
            Some(Raster { w, h, data })
        }
    }

    fn text(&self, page: usize) -> Option<String> {
        self.pages.get(page).map(|p| p.text.clone())
    }
}

pub struct RtfProvider;

impl Provider for RtfProvider {
    fn id(&self) -> &'static str {
        "rtf"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        // Many ".doc" files are really RTF.
        if p.head.starts_with(b"{\\rtf") {
            95
        } else {
            0
        }
    }

    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        if p.size > 256 << 20 {
            return Err("RTF file too large to preview".into());
        }
        let data = std::fs::read(&p.path).map_err(|e| e.to_string())?;
        let t = std::time::Instant::now();
        let mut facts = info(&data);
        let src = RtfSource::new(&data, cancel)?;
        tracing::info!("rtf: {} pages laid out in {:.0} ms", src.pages.len(), t.elapsed().as_secs_f64() * 1000.0);
        facts.retain(|(k, _)| k != "Pages");
        facts.push(("Pages".into(), src.pages.len().to_string()));
        facts.push(("Page size".into(), crate::pdf::paper_size(src.size.0, src.size.1)));
        let src: Arc<dyn PageSource> = Arc::new(src);
        Ok(Box::new(RtfLoaded(src, facts)))
    }
}

struct RtfLoaded(Arc<dyn PageSource>, Vec<(String, String)>);

impl Loaded for RtfLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        Box::new(PagedView::new(self.0, self.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtf_info_block() {
        let rtf = r"{\rtf1\ansi{\info{\title Caf\'e9 report}{\author Jane \u8364? Doe}{\creatim\yr2024\mo3\dy5\hr10\min20}{\nofpages3}}Hello}".as_bytes();
        let f = info(rtf);
        assert!(f.contains(&("Title".into(), "Café report".into())), "{f:?}");
        assert!(f.contains(&("Author".into(), "Jane € Doe".into())), "{f:?}");
        assert!(f.iter().any(|(k, _)| k == "Created"));
        assert!(f.contains(&("Pages".into(), "3".into())));
    }
}
