//! The 3D preview: our three.js page in the shared WebView2, fed either the
//! file itself (glTF, USD, Collada...) or a GLB converted here. The page stays
//! loaded between 3D files, so moving through a folder only swaps the model.

use gt_fmt_web::engine::{self, Blob, Site, HOST};
use gt_ui::cx::Fonts;
use gt_ui::widgets::bitmap_from_bgra;
use gt_ui::{glyph, rgba, Action, Align, DrawCx, EventCx, Font, HostCx, Loaded, Rect, View, ViewEnv};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

const PAGE: &str = include_str!("../assets/viewer.html");
const PAGE_URL: &str = "https://gt.localhost/__page__?view=3d";
const BG: u32 = 0x1F1F21;
const HOME: char = '\u{E80F}';
const WIREFRAME: char = '\u{F0E2}';

static BLOB_ID: AtomicU64 = AtomicU64::new(1);

/// Any thread: a model is selected; start WebView2 and load the 3D page hidden.
pub fn preload() {
    let site = Site { page: Some(PAGE.to_string()), scripts: true, messages: true, reuse: Some(r#"{"clear":true}"#.to_string()), ..Default::default() };
    engine::request_preload(PAGE_URL, site);
}

#[derive(Clone)]
pub enum Content {
    /// The page reads the file itself; `kind` picks its loader.
    File { path: PathBuf, kind: &'static str },
    /// Converted here.
    Glb(Arc<Vec<u8>>),
    /// Converted earlier and cached on disk.
    GlbFile(PathBuf),
}

/// What a background conversion (Blender) delivers to `View::upgrade`.
pub type Converted = Result<(Content, Vec<(String, String)>, Option<f64>), String>;

/// Premultiplied BGRA picture shown while converting.
pub struct Thumb {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

pub struct ModelView {
    content: Option<Content>,
    path: PathBuf,
    facts: Vec<(String, String)>,
    unit_meters: Option<f64>,
    stats: Option<Value>,
    error: Option<String>,
    host: Option<HostCx>,
    attached: Option<Instant>,
    wire: bool,
    thumb: Option<Thumb>,
    thumb_bmp: Option<ID2D1Bitmap1>,
    status: Option<Arc<Mutex<String>>>,
    started: Instant,
    /// Blender conversion after the page could not read the file itself.
    fallback: Option<(Arc<Mutex<Option<Converted>>>, gt_core::CancelToken)>,
    fallback_tried: bool,
}

/// Highest folder above `p`: models may load their textures and buffers from
/// anywhere on their drive (`..\textures`); nothing leaves the machine.
fn drive_root(p: &Path) -> PathBuf {
    p.ancestors().last().map(|a| a.to_path_buf()).unwrap_or_else(|| p.to_path_buf())
}

impl ModelView {
    pub fn new(path: &Path, content: Content, facts: Vec<(String, String)>, unit_meters: Option<f64>) -> Self {
        ModelView {
            content: Some(content),
            path: path.to_path_buf(),
            facts,
            unit_meters,
            stats: None,
            error: None,
            host: None,
            attached: None,
            wire: false,
            thumb: None,
            thumb_bmp: None,
            status: None,
            started: Instant::now(),
            fallback: None,
            fallback_tried: false,
        }
    }

    fn apply(&mut self, r: Converted) {
        match r {
            Ok((content, facts, unit)) => {
                self.facts.retain(|(k, _)| !facts.iter().any(|(k2, _)| k2 == k));
                self.facts.extend(facts);
                self.unit_meters = unit;
                self.content = Some(content);
                self.status = None;
                self.error = None;
                self.show();
            }
            Err(e) => {
                self.error = Some(e);
                self.status = None;
            }
        }
    }

    /// The page could not read the file: let Blender import it, if it can.
    fn try_fallback(&mut self, why: &str) {
        if self.fallback_tried || !crate::blend::available() {
            return;
        }
        let Some(Content::File { path, kind }) = self.content.clone() else { return };
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let Some(importer) = crate::blend::importer_for(kind, &ext) else { return };
        let Some(host) = self.host else { return };
        tracing::info!("{}: three.js failed ({why}); importing with Blender", path.display());
        self.fallback_tried = true;
        let status = Arc::new(Mutex::new("Converting with Blender…".to_string()));
        self.status = Some(status.clone());
        self.content = None;
        self.started = Instant::now();
        engine::hide();
        let slot: Arc<Mutex<Option<Converted>>> = Arc::default();
        let cancel = gt_core::CancelToken::new();
        let (s2, c2) = (slot.clone(), cancel.clone());
        let (parent, wake) = (host.parent.0 as isize, host.wake_msg);
        let why = why.to_string();
        let _ = std::thread::Builder::new().name("gt-3d-fallback".into()).spawn(move || {
            let r = crate::blend::convert(&path, importer, &c2, &|s| *status.lock() = s);
            if c2.is_cancelled() {
                return;
            }
            *s2.lock() = Some(match r {
                Ok((glb, mut facts)) => {
                    facts.push(("Converted by".into(), "Blender (the viewer could not read it directly)".into()));
                    Ok((Content::GlbFile(glb), facts, Some(1.0)))
                }
                Err(e) => Err(format!("This model could not be displayed.
{why}
{e}")),
            });
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(windows::Win32::Foundation::HWND(parent as *mut _)), wake, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
            }
        });
        self.fallback = Some((slot, cancel));
    }

    fn poll_fallback(&mut self) -> bool {
        let r = self.fallback.as_ref().and_then(|(slot, _)| slot.lock().take());
        match r {
            Some(r) => {
                self.fallback = None;
                self.apply(r);
                true
            }
            None => false,
        }
    }

    /// Waiting for a conversion: `thumb` and `status` are shown meanwhile.
    pub fn converting(path: &Path, thumb: Option<Thumb>, status: Arc<Mutex<String>>, facts: Vec<(String, String)>) -> Self {
        let mut v = ModelView::new(path, Content::Glb(Arc::default()), facts, None);
        v.content = None;
        v.thumb = thumb;
        v.status = Some(status);
        v
    }

    fn show(&mut self) {
        let (Some(host), Some(content)) = (self.host, self.content.clone()) else { return };
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut blobs = Vec::new();
        let (url, kind) = match content {
            Content::File { path, kind } => (engine::url_for(&path), kind),
            Content::Glb(b) => {
                let n = format!("m{}.glb", BLOB_ID.fetch_add(1, Ordering::Relaxed));
                blobs.push((n.clone(), Blob::Mem(b), "model/gltf-binary"));
                (format!("{HOST}__blob__/{n}"), "gltf")
            }
            Content::GlbFile(p) => {
                let n = format!("m{}.glb", BLOB_ID.fetch_add(1, Ordering::Relaxed));
                blobs.push((n.clone(), Blob::File(p), "model/gltf-binary"));
                (format!("{HOST}__blob__/{n}"), "gltf")
            }
        };
        let hello = json!({ "load": { "url": url, "type": kind, "name": name } }).to_string();
        let site = Site {
            page: Some(PAGE.to_string()),
            root: Some(drive_root(&self.path)),
            scripts: true,
            messages: true,
            hello: Some(hello),
            reuse: Some(r#"{"clear":true}"#.to_string()),
            blobs,
        };
        self.stats = None;
        if self.wire {
            self.wire = false;
        }
        engine::show(host.parent, host.wake_msg, site, PAGE_URL.to_string(), BG);
    }

    fn size_text(&self, s: &Value) -> Option<String> {
        let v: Vec<f64> = s["size"].as_array()?.iter().filter_map(|x| x.as_f64()).collect();
        if v.len() != 3 || v.iter().all(|x| *x <= 0.0) {
            return None;
        }
        let fmt3 = |k: f64, unit: &str| {
            let f = |x: f64| {
                let x = x * k;
                if x >= 100.0 {
                    format!("{x:.0}")
                } else if x >= 10.0 {
                    format!("{x:.1}")
                } else {
                    format!("{x:.2}")
                }
            };
            format!("{} × {} × {} {unit}", f(v[0]), f(v[1]), f(v[2]))
        };
        let max = v.iter().cloned().fold(0.0, f64::max);
        Some(match self.unit_meters {
            // STL: millimetres by convention.
            Some(u) if (u - 0.001).abs() < 1e-9 => fmt3(1.0, "mm"),
            Some(u) => {
                let m = max * u;
                if m >= 1.0 {
                    fmt3(u, "m")
                } else if m >= 0.01 {
                    fmt3(u * 100.0, "cm")
                } else {
                    fmt3(u * 1000.0, "mm")
                }
            }
            None => fmt3(1.0, "units"),
        })
    }
}

impl View for ModelView {
    fn kind(&self) -> &'static str {
        "3d"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let w = (1240.0 * scale).min(max_w * 0.86);
        let h = (w * 0.7).min(max_h * 0.86);
        (w, h)
    }

    fn draw(&mut self, cx: &mut DrawCx, rect: Rect) {
        // The page sits under this layer: leave its area clear once it shows.
        if engine::showing() {
            return;
        }
        cx.fill(rect, rgba(0.122, 0.122, 0.129, 1.0));
        let warn = |cx: &mut DrawCx, text: &str| {
            let r = rect.inset(cx.px(32.0));
            cx.glyph(glyph::WARNING, Rect::new(r.x, r.y + r.h / 2.0 - cx.px(60.0), r.w, cx.px(40.0)), 28.0, rgba(1.0, 0.7, 0.4, 1.0));
            let tr = Rect::new(r.x, r.y + r.h / 2.0 - cx.px(10.0), r.w, cx.px(120.0));
            let (tw, _) = cx.fonts.measure(text, Font::Ui(13.0), cx.scale, tr.w, true);
            let tr = Rect::new(r.x + ((r.w - tw) / 2.0).max(0.0), tr.y, tw.min(r.w) + 1.0, tr.h);
            cx.text_wrapped(text, tr, Font::Ui(13.0), cx.theme.text2);
        };
        if let Some(e) = engine::error() {
            warn(cx, &format!("WebView2 is not available: {e}"));
            return;
        }
        if let Some(e) = &self.error {
            let e = e.clone();
            warn(cx, &e);
            return;
        }
        if let Some(status) = &self.status {
            if self.content.is_none() {
                if self.thumb_bmp.is_none() {
                    if let Some(t) = &self.thumb {
                        self.thumb_bmp = bitmap_from_bgra(cx.dc, t.w, t.h, &t.bgra);
                    }
                }
                let area = rect.inset(cx.px(40.0));
                let area = Rect::new(area.x, area.y, area.w, area.h - cx.px(40.0));
                if let (Some(b), Some(t)) = (&self.thumb_bmp, &self.thumb) {
                    let k = (area.w / t.w as f32).min(area.h / t.h as f32).min(3.0 * cx.scale);
                    let (w, h) = (t.w as f32 * k, t.h as f32 * k);
                    let d = Rect::new(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h).snap();
                    cx.bitmap(b, d, None, 1.0, true);
                } else {
                    cx.glyph('\u{F158}', area, 56.0, rgba(1.0, 1.0, 1.0, 0.18));
                }
                let secs = self.started.elapsed().as_secs();
                let s = status.lock().clone();
                let t = if secs >= 2 { format!("{s} · {secs} s") } else { s };
                let (tw, _) = cx.measure(&t, Font::Ui(12.5));
                let p = Rect::new(rect.x + (rect.w - tw) / 2.0 - cx.px(14.0), rect.bottom() - cx.px(46.0), tw + cx.px(28.0), cx.px(30.0));
                cx.round(p, p.h / 2.0, rgba(0.1, 0.1, 0.1, 0.9));
                cx.text(&t, p, Font::Ui(12.5), cx.theme.text2, Align::Center);
                return;
            }
        }
        if self.attached.is_some_and(|t| t.elapsed().as_millis() > 400) {
            cx.text("Loading…", rect, Font::Ui(13.0), cx.theme.text3, Align::Center);
        }
    }

    fn upgrade(&mut self, _env: &ViewEnv, payload: Box<dyn std::any::Any + Send>) -> bool {
        let Ok(r) = payload.downcast::<Converted>() else { return false };
        self.apply(*r);
        true
    }

    fn next_frame_in(&self) -> Option<Duration> {
        if self.attached.is_none() || engine::showing() {
            return None;
        }
        Some(Duration::from_millis(if self.status.is_some() { 1000 } else { 200 }))
    }

    fn tick(&mut self, _env: &ViewEnv) -> bool {
        true
    }

    fn attach(&mut self, host: &HostCx) {
        self.host = Some(*host);
        self.attached = Some(Instant::now());
        if self.content.is_some() {
            self.show();
        }
    }

    fn detach(&mut self) {
        if let Some((_, c)) = self.fallback.take() {
            c.cancel();
        }
        self.attached = None;
        self.host = None;
        engine::hide();
    }

    fn layout_native(&mut self, rect: Rect, _host: &HostCx) {
        engine::place(RECT { left: rect.x.round() as i32, top: rect.y.round() as i32, right: rect.right().round() as i32, bottom: rect.bottom().round() as i32 });
    }

    fn native_content(&self) -> bool {
        engine::showing()
    }

    fn wake(&mut self) -> bool {
        self.poll_fallback();
        for m in engine::take_messages() {
            let v: Value = serde_json::from_str(&m).unwrap_or_default();
            match v["type"].as_str() {
                Some("loaded") => {
                    let s = v["stats"].clone();
                    tracing::info!("3D model shown in {} ms: {} triangles", s["ms"], s["triangles"]);
                    self.stats = Some(s);
                }
                Some("error") => {
                    let why = v["message"].as_str().unwrap_or("").to_string();
                    tracing::warn!("3D page: {why}");
                    self.try_fallback(&why);
                }
                Some("wire") => self.wire = v["on"].as_bool().unwrap_or(false),
                Some("missing") => {
                    let n = v["count"].as_u64().unwrap_or(0);
                    if n > 0 && !self.facts.iter().any(|(k, _)| k == "Missing textures") {
                        self.facts.push(("Missing textures".into(), n.to_string()));
                    }
                }
                _ => {}
            }
        }
        true
    }

    fn actions(&self) -> Vec<Action> {
        if self.content.is_none() {
            return Vec::new();
        }
        vec![Action::new("reset", HOME, "Reset the view (double-click)"), Action::new("wire", WIREFRAME, "Wireframe (W)").active(self.wire)]
    }

    fn on_action(&mut self, id: &str, _cx: &mut EventCx) {
        match id {
            "reset" => engine::post(r#"{"cmd":"reset"}"#),
            "wire" => {
                self.wire = !self.wire;
                engine::post(&json!({ "cmd": "wire", "on": self.wire }).to_string());
            }
            _ => {}
        }
    }

    fn info(&self) -> Vec<(String, String)> {
        let mut f = self.facts.clone();
        let Some(s) = &self.stats else { return f };
        let n = |k: &str| s[k].as_u64().unwrap_or(0);
        if n("triangles") > 0 {
            f.push(("Triangles".into(), gt_core::fmt::group(n("triangles"))));
            f.push(("Vertices".into(), gt_core::fmt::group(n("vertices"))));
        }
        if n("points") > 0 {
            f.push(("Points".into(), gt_core::fmt::group(n("points"))));
        }
        if n("meshes") > 0 {
            f.push(("Meshes".into(), gt_core::fmt::group(n("meshes"))));
        }
        if n("materials") > 0 {
            f.push(("Materials".into(), n("materials").to_string()));
        }
        if n("textures") > 0 {
            f.push(("Textures".into(), n("textures").to_string()));
        }
        if n("morphs") > 0 && !f.iter().any(|(k, _)| k == "Blend shapes") {
            f.push(("Morph targets".into(), n("morphs").to_string()));
        }
        if let Some(sz) = self.size_text(s) {
            f.push(("Dimensions".into(), sz));
        }
        if let Some(a) = s["animations"].as_array().filter(|a| !a.is_empty()) {
            let list: Vec<String> = a
                .iter()
                .take(6)
                .map(|c| {
                    let name = c["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("unnamed");
                    format!("{name} ({})", gt_core::fmt::duration(c["duration"].as_f64().unwrap_or(0.0)))
                })
                .collect();
            let more = if a.len() > 6 { format!(" and {} more", a.len() - 6) } else { String::new() };
            f.push(("Animations".into(), format!("{}{more}", list.join(", "))));
        }
        // The file's own facts first (Blender's material count...), then the page's.
        let mut seen = std::collections::HashSet::new();
        f.retain(|(k, _)| seen.insert(k.clone()));
        f
    }

    fn subtitle(&self) -> Option<String> {
        if let Some(s) = &self.status {
            if self.content.is_none() {
                return Some(s.lock().clone());
            }
        }
        let s = self.stats.as_ref()?;
        let tris = s["triangles"].as_u64().unwrap_or(0);
        let mut parts = Vec::new();
        if tris > 0 {
            parts.push(format!("{} triangles", gt_core::fmt::group(tris)));
        } else if let Some(p) = s["points"].as_u64().filter(|p| *p > 0) {
            parts.push(format!("{} points", gt_core::fmt::group(p)));
        }
        match s["animations"].as_array().map(|a| a.len()).unwrap_or(0) {
            0 => {}
            1 => parts.push("1 animation".into()),
            k => parts.push(format!("{k} animations")),
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// Clicking into the model gives the page the keyboard (W, Home, K...).
    fn wants_focus(&self) -> bool {
        true
    }
}

impl Drop for ModelView {
    fn drop(&mut self) {
        if self.attached.is_some() {
            engine::hide();
        }
    }
}

/// Everything a `ModelView` is made of (built on the UI thread).
pub struct ModelLoaded {
    pub path: PathBuf,
    pub content: Option<Content>,
    pub facts: Vec<(String, String)>,
    pub unit_meters: Option<f64>,
    pub thumb: Option<Thumb>,
    pub status: Option<Arc<Mutex<String>>>,
    pub upgrade: Option<gt_ui::provider::Upgrade>,
}

impl ModelLoaded {
    pub fn ready(path: &Path, content: Content, facts: Vec<(String, String)>, unit_meters: Option<f64>) -> Self {
        ModelLoaded { path: path.to_path_buf(), content: Some(content), facts, unit_meters, thumb: None, status: None, upgrade: None }
    }
}

impl Loaded for ModelLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        let s = *self;
        match (s.content, s.status) {
            (Some(c), _) => Box::new(ModelView::new(&s.path, c, s.facts, s.unit_meters)),
            (None, status) => Box::new(ModelView::converting(&s.path, s.thumb, status.unwrap_or_default(), s.facts)),
        }
    }
    fn take_upgrade(&mut self) -> Option<gt_ui::provider::Upgrade> {
        self.upgrade.take()
    }
}
