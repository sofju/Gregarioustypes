//! A document that needs converting first (LibreOffice, Ghostscript): shows a
//! quick preview (embedded thumbnail or extracted text) with a status line,
//! then becomes a `PagedView` when the converted PDF arrives as an upgrade.

use crate::pages::{PageSource, Raster};
use crate::paged::PagedView;
use gt_ui::cx::Fonts;
use gt_ui::widgets::bitmap_from_bgra;
use gt_ui::widgets::text::TextView;
use gt_ui::{glyph, rgba, Action, Align, DrawCx, Event, EventCx, Font, HostCx, Rect, Request, View, ViewEnv};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

/// What the background conversion delivers.
pub type Converted = Result<(Arc<dyn PageSource>, Vec<(String, String)>), String>;

pub struct StagedView {
    inner: Option<PagedView>,
    preview: Option<Raster>,
    preview_bmp: Option<ID2D1Bitmap1>,
    text: Option<TextView>,
    status: Arc<Mutex<String>>,
    error: Option<String>,
    facts: Vec<(String, String)>,
    started: Instant,
    host: Option<HostCx>,
    /// Page aspect (w / h) used for the window size until pages arrive.
    aspect: f32,
    kind: &'static str,
    /// Nothing more is coming: the status is a permanent note.
    done: bool,
    /// Conversion running on a thread of our own (views pushed from another view).
    own: Option<(Arc<Mutex<Option<Converted>>>, gt_core::CancelToken)>,
}

impl StagedView {
    pub fn new(status: Arc<Mutex<String>>, facts: Vec<(String, String)>, aspect: f32) -> Self {
        StagedView {
            inner: None,
            preview: None,
            preview_bmp: None,
            text: None,
            status,
            error: None,
            facts,
            started: Instant::now(),
            host: None,
            aspect,
            kind: "doc",
            done: false,
            own: None,
        }
    }

    pub fn with_preview(mut self, r: Option<Raster>) -> Self {
        if let Some(r) = r {
            self.aspect = r.w as f32 / r.h.max(1) as f32;
            self.preview = Some(r);
        }
        self
    }

    pub fn with_text(mut self, t: Option<String>) -> Self {
        if let Some(t) = t.filter(|t| !t.trim().is_empty()) {
            self.text = Some(TextView::new(&t, true, None, None));
        }
        self
    }

    /// Run `job` on a background thread; the view picks up its result itself.
    pub fn spawn(status: Arc<Mutex<String>>, facts: Vec<(String, String)>, aspect: f32, job: impl FnOnce(&gt_core::CancelToken, &Arc<Mutex<String>>) -> Converted + Send + 'static) -> Self {
        let mut v = StagedView::new(status.clone(), facts, aspect);
        let slot: Arc<Mutex<Option<Converted>>> = Arc::default();
        let cancel = gt_core::CancelToken::new();
        let (s2, c2) = (slot.clone(), cancel.clone());
        let _ = std::thread::Builder::new().name("gt-doc-convert".into()).spawn(move || {
            let r = job(&c2, &status);
            *s2.lock() = Some(r);
        });
        v.own = Some((slot, cancel));
        v
    }

    fn poll_own(&mut self) -> bool {
        let r = match &self.own {
            Some((slot, _)) => slot.lock().take(),
            None => None,
        };
        match r {
            Some(r) => {
                self.own = None;
                self.set_result(r);
                true
            }
            None => false,
        }
    }

    pub fn finished(mut self) -> Self {
        self.done = true;
        self
    }

    pub fn kind(mut self, k: &'static str) -> Self {
        self.kind = k;
        self
    }

    /// Ready immediately (conversion was cached).
    pub fn ready(src: Arc<dyn PageSource>, facts: Vec<(String, String)>) -> Self {
        let mut v = StagedView::new(Arc::default(), Vec::new(), 0.75);
        v.inner = Some(PagedView::new(src, facts));
        v
    }

    fn set_result(&mut self, r: Converted) {
        match r {
            Ok((src, mut facts)) => {
                let mut all = self.facts.clone();
                all.retain(|(k, _)| !facts.iter().any(|(k2, _)| k2 == k));
                all.append(&mut facts);
                let mut p = PagedView::new(src, all);
                if let Some(h) = &self.host {
                    p.attach(h);
                }
                self.inner = Some(p);
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl View for StagedView {
    fn kind(&self) -> &'static str {
        match &self.inner {
            Some(p) => p.kind(),
            None => self.kind,
        }
    }

    fn preferred_size(&mut self, fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        if let Some(p) = self.inner.as_mut() {
            return p.preferred_size(fonts, scale, max_w, max_h);
        }
        let m = 16.0 * scale;
        let a = self.aspect.max(0.2);
        let h = max_h * 0.94;
        let w = ((h - 2.0 * m) * a + 2.0 * m + 14.0 * scale).min(max_w * 0.92);
        let h = if w >= max_w * 0.92 { ((w - 2.0 * m) / a + 2.0 * m).min(h) } else { h };
        (w, h)
    }

    fn draw(&mut self, cx: &mut DrawCx, rect: Rect) {
        self.poll_own();
        if let Some(p) = self.inner.as_mut() {
            return p.draw(cx, rect);
        }
        if let Some(e) = &self.error {
            let r = rect.inset(cx.px(32.0));
            cx.glyph(glyph::WARNING, Rect::new(r.x, r.y + r.h / 2.0 - cx.px(60.0), r.w, cx.px(40.0)), 28.0, rgba(1.0, 0.75, 0.4, 1.0));
            let tr = Rect::new(r.x, r.y + r.h / 2.0 - cx.px(10.0), r.w, cx.px(120.0));
            let (tw, _) = cx.fonts.measure(e, Font::Ui(13.0), cx.scale, tr.w, true);
            let tr = Rect::new(r.x + ((r.w - tw) / 2.0).max(0.0), tr.y, tw.min(r.w) + 1.0, tr.h);
            cx.text_wrapped(e, tr, Font::Ui(13.0), cx.theme.text2);
            return;
        }
        if let Some(t) = self.text.as_mut() {
            t.draw(cx, Rect::new(rect.x, rect.y, rect.w, rect.h - cx.px(56.0)));
        } else {
            if self.preview_bmp.is_none() {
                if let Some(r) = &self.preview {
                    self.preview_bmp = bitmap_from_bgra(cx.dc, r.w, r.h, &r.data);
                }
            }
            let area = rect.inset(cx.px(24.0));
            let area = Rect::new(area.x, area.y, area.w, area.h - cx.px(40.0));
            match (&self.preview_bmp, &self.preview) {
                (Some(b), Some(r)) => {
                    let k = (area.w / r.w as f32).min(area.h / r.h as f32).min(3.0 * cx.scale);
                    let (w, h) = (r.w as f32 * k, r.h as f32 * k);
                    let d = Rect::new(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h).snap();
                    cx.fill(Rect::new(d.x + cx.px(2.0), d.y + cx.px(2.0), d.w, d.h), rgba(0.0, 0.0, 0.0, 0.3));
                    cx.bitmap(b, d, None, 1.0, false);
                }
                _ => {
                    // An empty sheet of the expected shape.
                    let a = self.aspect.max(0.2);
                    let h = area.h.min(area.w / a);
                    let w = h * a;
                    let d = Rect::new(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h).snap();
                    cx.fill(d, rgba(1.0, 1.0, 1.0, 0.06));
                    cx.glyph(glyph::DOC, d, 40.0, rgba(1.0, 1.0, 1.0, 0.25));
                }
            }
        }
        let secs = self.started.elapsed().as_secs();
        let status = self.status.lock().clone();
        let t = if secs >= 2 && !self.done { format!("{status} · {secs} s") } else { status };
        let (tw, _) = cx.measure(&t, Font::Ui(12.5));
        let p = Rect::new(rect.x + (rect.w - tw) / 2.0 - cx.px(14.0), rect.bottom() - cx.px(46.0), tw + cx.px(28.0), cx.px(30.0));
        cx.round(p, p.h / 2.0, rgba(0.1, 0.1, 0.1, 0.9));
        cx.text(&t, p, Font::Ui(12.5), cx.theme.text2, Align::Center);
    }

    fn upgrade(&mut self, _env: &ViewEnv, payload: Box<dyn std::any::Any + Send>) -> bool {
        match payload.downcast::<Converted>() {
            Ok(r) => {
                self.set_result(*r);
                true
            }
            Err(_) => false,
        }
    }

    fn next_frame_in(&self) -> Option<Duration> {
        match &self.inner {
            Some(p) => p.next_frame_in(),
            None if self.error.is_none() && !self.done => Some(Duration::from_millis(1000)),
            None => None,
        }
    }

    fn tick(&mut self, env: &ViewEnv) -> bool {
        if self.poll_own() {
            return true;
        }
        match self.inner.as_mut() {
            Some(p) => p.tick(env),
            None => self.error.is_none() && !self.done,
        }
    }

    fn event(&mut self, cx: &mut EventCx, rect: Rect, ev: &Event) {
        if let Some(p) = self.inner.as_mut() {
            return p.event(cx, rect, ev);
        }
        if let Some(t) = self.text.as_mut() {
            t.event(cx, Rect::new(rect.x, rect.y, rect.w, rect.h - cx.px(56.0)), ev);
        }
    }

    fn actions(&self) -> Vec<Action> {
        self.inner.as_ref().map(|p| p.actions()).unwrap_or_default()
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        if let Some(p) = self.inner.as_mut() {
            p.on_action(id, cx);
        }
    }

    fn copy_text(&self) -> Option<String> {
        match (&self.inner, &self.text) {
            (Some(p), _) => p.copy_text(),
            (None, Some(t)) => t.copy_text(),
            _ => None,
        }
    }

    fn info(&self) -> Vec<(String, String)> {
        match &self.inner {
            Some(p) => p.info(),
            None => self.facts.clone(),
        }
    }

    fn subtitle(&self) -> Option<String> {
        self.inner.as_ref().and_then(|p| p.subtitle())
    }

    fn wants_focus(&self) -> bool {
        true
    }

    fn attach(&mut self, host: &HostCx) {
        self.host = Some(*host);
        if let Some(p) = self.inner.as_mut() {
            p.attach(host);
        }
    }

    fn detach(&mut self) {
        self.host = None;
        if let Some(p) = self.inner.as_mut() {
            p.detach();
        }
    }

    fn wake(&mut self) -> bool {
        self.poll_own() || self.inner.as_mut().is_some_and(|p| p.wake())
    }

    fn take_request(&mut self) -> Option<Request> {
        self.inner.as_mut().and_then(|p| p.take_request())
    }
}

impl Drop for StagedView {
    fn drop(&mut self) {
        if let Some((_, c)) = &self.own {
            c.cancel();
        }
    }
}
