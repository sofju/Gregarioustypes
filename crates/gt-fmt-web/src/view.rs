//! Web previews (HTML, Markdown, SWF) shown by the shared WebView2.

use crate::engine::{self, Site, HOST};
use gt_core::CancelToken;
use gt_ui::cx::Fonts;
use gt_ui::{glyph, rgba, Action, Align, DrawCx, EventCx, Font, HostCx, Loaded, Probe, Provider, Rect, View, ViewEnv};
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows::Win32::Foundation::RECT;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Html,
    Markdown,
    Swf,
}

pub struct WebView {
    kind: Kind,
    site: Site,
    url: String,
    bg: u32,
    facts: Vec<(String, String)>,
    /// Stage size for SWF (twips → pixels).
    stage: Option<(f32, f32)>,
    subtitle: Option<String>,
    attached: Option<Instant>,
}

/// Highest folder above `p` ("C:\"): pages may load files from anywhere on
/// their drive; nothing leaves the machine.
fn drive_root(p: &Path) -> PathBuf {
    p.ancestors().last().map(|a| a.to_path_buf()).unwrap_or_else(|| p.to_path_buf())
}

impl View for WebView {
    fn kind(&self) -> &'static str {
        match self.kind {
            Kind::Html => "html",
            Kind::Markdown => "markdown",
            Kind::Swf => "swf",
        }
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        match (self.kind, self.stage) {
            (Kind::Swf, Some((w, h))) => {
                let (w, h) = (w * scale, h * scale);
                let k = (max_w * 0.9 / w).min(max_h * 0.9 / h).min(2.0);
                ((w * k).max(320.0 * scale), (h * k).max(240.0 * scale))
            }
            (Kind::Markdown, _) => ((960.0 * scale).min(max_w * 0.9), max_h * 0.9),
            _ => ((1100.0 * scale).min(max_w * 0.9), max_h * 0.9),
        }
    }

    fn draw(&mut self, cx: &mut DrawCx, rect: Rect) {
        // This layer sits above the page: once it is on screen, leave it clear.
        if engine::showing() {
            return;
        }
        let bg = rgba(((self.bg >> 16) & 255) as f32 / 255.0, ((self.bg >> 8) & 255) as f32 / 255.0, (self.bg & 255) as f32 / 255.0, 1.0);
        cx.fill(rect, bg);
        if let Some(e) = engine::error() {
            let r = rect.inset(cx.px(32.0));
            cx.glyph(glyph::WARNING, Rect::new(r.x, r.y + r.h / 2.0 - cx.px(56.0), r.w, cx.px(36.0)), 26.0, rgba(1.0, 0.7, 0.4, 1.0));
            cx.text(&format!("WebView2 is not available: {e}"), Rect::new(r.x, r.y + r.h / 2.0, r.w, cx.px(30.0)), Font::Ui(13.0), cx.theme.text2, Align::Center);
            return;
        }
        // Still loading after a moment: say so.
        if self.attached.is_some_and(|t| t.elapsed().as_millis() > 400) {
            let dark = (self.bg >> 16) & 255 < 128;
            let c = if dark { cx.theme.text3 } else { rgba(0.4, 0.4, 0.42, 1.0) };
            cx.text("Loading…", rect, Font::Ui(13.0), c, Align::Center);
        }
    }

    fn next_frame_in(&self) -> Option<std::time::Duration> {
        (self.attached.is_some() && !engine::showing()).then(|| std::time::Duration::from_millis(200))
    }

    fn tick(&mut self, _env: &ViewEnv) -> bool {
        true
    }

    fn attach(&mut self, host: &HostCx) {
        self.attached = Some(Instant::now());
        engine::show(host.parent, host.wake_msg, self.site.clone(), self.url.clone(), self.bg);
    }

    fn detach(&mut self) {
        self.attached = None;
        engine::hide();
    }

    fn layout_native(&mut self, rect: Rect, _host: &HostCx) {
        engine::place(RECT { left: rect.x.round() as i32, top: rect.y.round() as i32, right: rect.right().round() as i32, bottom: rect.bottom().round() as i32 });
    }

    fn native_content(&self) -> bool {
        engine::showing()
    }

    fn wake(&mut self) -> bool {
        true
    }

    fn actions(&self) -> Vec<Action> {
        match self.kind {
            Kind::Html | Kind::Markdown => vec![Action::new("source", glyph::CODE, "Show the source text")],
            Kind::Swf => Vec::new(),
        }
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        if id == "source" {
            cx.switch_provider = Some("text");
        }
    }

    fn info(&self) -> Vec<(String, String)> {
        self.facts.clone()
    }

    fn subtitle(&self) -> Option<String> {
        self.subtitle.clone()
    }

    /// A click into the page gives it the keyboard (selection, Ctrl+C, scrolling).
    fn wants_focus(&self) -> bool {
        true
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        if self.attached.is_some() {
            engine::hide();
        }
    }
}

struct WebLoaded(WebView);

impl Loaded for WebLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        Box::new(self.0)
    }
}

fn looks_like_html(head: &[u8]) -> bool {
    let t = String::from_utf8_lossy(&head[..head.len().min(1024)]).to_ascii_lowercase();
    let t = t.trim_start_matches('\u{feff}').trim_start();
    t.starts_with("<!doctype html") || t.starts_with("<html") || (t.starts_with("<?xml") && t.contains("<html"))
}

fn html_title(head: &[u8]) -> Option<String> {
    let t = String::from_utf8_lossy(head);
    let l = t.to_ascii_lowercase();
    let i = l.find("<title")?;
    let j = i + l[i..].find('>')? + 1;
    let k = j + l[j..].find("</title>")?;
    let s = t.get(j..k)?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!s.is_empty()).then_some(s)
}

pub struct HtmlProvider;

const HTML_EXTS: &[&str] = &["htm", "html", "xhtml", "xht", "shtml"];

impl Provider for HtmlProvider {
    fn id(&self) -> &'static str {
        "html"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        let score = match (p.ext_is(HTML_EXTS), looks_like_html(&p.head)) {
            (true, _) => 85,
            // ".web" and other ambiguous extensions holding HTML.
            (false, true) => 55,
            _ => 0,
        };
        if score > 0 {
            engine::request_warm();
        }
        score
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let title = html_title(&p.head);
        let mut facts = Vec::new();
        if let Some(t) = &title {
            facts.push(("Title".to_string(), t.clone()));
        }
        facts.push(("Rendering".into(), "WebView2 · scripts and network off".into()));
        Ok(Box::new(WebLoaded(WebView {
            kind: Kind::Html,
            site: Site { page: None, root: Some(drive_root(&p.path)), scripts: false, ..Default::default() },
            url: engine::url_for(&p.path),
            bg: 0xFFFFFF,
            facts,
            stage: None,
            subtitle: title,
            attached: None,
        })))
    }
}

pub struct MarkdownProvider;

impl Provider for MarkdownProvider {
    fn id(&self) -> &'static str {
        "markdown"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.ext_is(crate::markdown::EXTS) {
            return 0;
        }
        engine::request_warm();
        90
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        if p.size > 32 << 20 {
            return Err("Markdown file too large to render".into());
        }
        let md = crate::markdown::read(&p.path)?;
        let facts = crate::markdown::facts(&md);
        let title = md.lines().find_map(|l| l.strip_prefix("# ")).map(|t| t.trim().to_string());
        Ok(Box::new(WebLoaded(WebView {
            kind: Kind::Markdown,
            site: Site { page: None, root: Some(drive_root(&p.path)), scripts: false, ..Default::default() },
            url: engine::url_for(&p.path),
            bg: 0x1C1C1D,
            facts,
            stage: None,
            subtitle: title,
            attached: None,
        })))
    }
}

/// SWF header: version, stage size (pixels), frame rate, frame count.
pub struct SwfHeader {
    pub compression: &'static str,
    pub version: u8,
    pub length: u32,
    pub size: Option<(f32, f32)>,
    pub fps: Option<f32>,
    pub frames: Option<u16>,
}

pub fn swf_header(head: &[u8]) -> Option<SwfHeader> {
    if head.len() < 8 || head[1..3] != *b"WS" {
        return None;
    }
    let compression = match head[0] {
        b'F' => "none",
        b'C' => "zlib",
        b'Z' => "LZMA",
        _ => return None,
    };
    let version = head[3];
    let length = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    let body: Vec<u8> = match head[0] {
        b'F' => head[8..].to_vec(),
        b'C' => {
            let mut d = miniz_oxide::inflate::core::DecompressorOxide::new();
            let mut out = vec![0u8; 64];
            let flags = miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER | miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF | miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_HAS_MORE_INPUT;
            let (_, _, n) = miniz_oxide::inflate::core::decompress(&mut d, &head[8..], &mut out, 0, flags);
            out.truncate(n);
            out
        }
        _ => Vec::new(),
    };
    let mut h = SwfHeader { compression, version, length, size: None, fps: None, frames: None };
    if body.len() >= 2 {
        // RECT: 5-bit field size, then Xmin, Xmax, Ymin, Ymax in twips.
        let nbits = (body[0] >> 3) as usize;
        let mut bit = 5usize;
        let mut read = |n: usize| -> Option<i32> {
            let mut v: u32 = 0;
            for _ in 0..n {
                let byte = *body.get(bit / 8)?;
                v = (v << 1) | ((byte >> (7 - bit % 8)) & 1) as u32;
                bit += 1;
            }
            // Sign-extend.
            Some(if n > 0 && v & (1 << (n - 1)) != 0 { (v as i32) - (1 << n) } else { v as i32 })
        };
        if let (Some(x0), Some(x1), Some(y0), Some(y1)) = (read(nbits), read(nbits), read(nbits), read(nbits)) {
            let (w, hh) = ((x1 - x0) as f32 / 20.0, (y1 - y0) as f32 / 20.0);
            if w > 0.0 && hh > 0.0 && w < 20000.0 && hh < 20000.0 {
                h.size = Some((w, hh));
            }
            let at = (5 + 4 * nbits).div_ceil(8);
            if body.len() >= at + 4 {
                h.fps = Some(body[at + 1] as f32 + body[at] as f32 / 256.0);
                h.frames = Some(u16::from_le_bytes([body[at + 2], body[at + 3]]));
            }
        }
    }
    Some(h)
}

fn ruffle_page(swf_url: &str) -> String {
    format!(
        r##"<!doctype html><html><head><meta charset="utf-8">
<style>html,body{{margin:0;width:100%;height:100%;background:#000;overflow:hidden}}#c,ruffle-player{{width:100%;height:100%;display:block}}</style>
<script>window.RufflePlayer=window.RufflePlayer||{{}};window.RufflePlayer.config={{publicPath:"{HOST}__ruffle__/",autoplay:"on",unmuteOverlay:"hidden",letterbox:"on",allowNetworking:"none",splashScreen:false,preloader:false,warnOnUnsupportedContent:false,showSwfDownload:false,contextMenu:"rightClickOnly",openUrlMode:"deny",backgroundColor:"#000000"}};</script>
<script src="{HOST}__ruffle__/ruffle.js"></script></head>
<body><div id="c"></div><script>
const r = window.RufflePlayer.newest();
const p = r.createPlayer();
document.getElementById("c").appendChild(p);
const api = p.ruffle ? p.ruffle() : p;
api.load({{url: "{swf_url}"}});
</script></body></html>"##
    )
}

pub struct SwfProvider;

impl Provider for SwfProvider {
    fn id(&self) -> &'static str {
        "swf"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || swf_header(&p.head).is_none() {
            return 0;
        }
        if !gt_core::paths::deps_dir().join("ruffle").join("ruffle.js").is_file() {
            return 0;
        }
        engine::request_warm();
        if p.ext == "swf" {
            95
        } else {
            70
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let h = swf_header(&p.head).ok_or("Not a Flash file")?;
        let mut facts = vec![("Flash version".to_string(), h.version.to_string())];
        if let Some((w, hh)) = h.size {
            facts.push(("Stage".into(), format!("{w:.0} × {hh:.0} px")));
        }
        if let Some(f) = h.fps {
            facts.push(("Frame rate".into(), format!("{f:.2} fps")));
        }
        if let Some(n) = h.frames {
            let dur = h.fps.filter(|f| *f > 0.0).map(|f| format!(" ({})", gt_core::fmt::duration(n as f64 / f as f64))).unwrap_or_default();
            facts.push(("Frames".into(), format!("{n}{dur}")));
        }
        facts.push(("Compression".into(), h.compression.into()));
        facts.push(("Uncompressed size".into(), gt_core::fmt::size(h.length as u64)));
        facts.push(("Player".into(), "Ruffle (network off)".into()));
        let sub = h.size.map(|(w, hh)| format!("{w:.0}×{hh:.0} · Flash {}", h.version));
        let swf_url = engine::url_for(&p.path);
        Ok(Box::new(WebLoaded(WebView {
            kind: Kind::Swf,
            site: Site { page: Some(ruffle_page(&swf_url)), root: Some(drive_root(&p.path)), scripts: true, ..Default::default() },
            url: format!("{HOST}__page__"),
            bg: 0x000000,
            facts,
            stage: h.size,
            subtitle: sub,
            attached: None,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swf_rect() {
        // FWS v10, RECT nbits=15: 0..11000 x 0..8000 twips (550×400), 24 fps, 10 frames.
        let mut bits: Vec<u8> = Vec::new();
        let mut acc: u64 = 0;
        let mut n = 0;
        let mut push = |v: u64, w: usize, bits: &mut Vec<u8>| {
            for i in (0..w).rev() {
                acc = (acc << 1) | ((v >> i) & 1);
                n += 1;
                if n == 8 {
                    bits.push(acc as u8);
                    acc = 0;
                    n = 0;
                }
            }
        };
        push(15, 5, &mut bits);
        for v in [0u64, 11000, 0, 8000] {
            push(v, 15, &mut bits);
        }
        push(0, 7, &mut bits);
        let mut f = b"FWS\x0a\x00\x10\x00\x00".to_vec();
        f.extend(bits);
        f.extend([0x00, 24, 10, 0]);
        let h = swf_header(&f).unwrap();
        assert_eq!(h.size, Some((550.0, 400.0)));
        assert_eq!(h.fps, Some(24.0));
        assert_eq!(h.frames, Some(10));
    }
}
