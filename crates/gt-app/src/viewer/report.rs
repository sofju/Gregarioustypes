//! A text report sheet drawn over the preview (MediaInfo now, BDInfo later):
//! header with a Summary/Full switch, Copy, Save and Close; the body is a
//! selectable monospace text view.

use gt_core::CancelToken;
use gt_ui::cx::Cursor;
use gt_ui::widgets::text::TextView;
use gt_ui::{glyph, rgba, Align, DrawCx, Event, EventCx, Font, MouseButton, Rect, View};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    Summary,
    Full,
    Copy,
    Save,
    Close,
}

pub enum Click {
    None,
    /// Switch between summary and full report.
    Mode(bool),
    Copy,
    Save,
    Close,
}

pub struct Report {
    pub title: &'static str,
    pub path: String,
    pub full: bool,
    pub id: u64,
    pub text: Option<String>,
    pub error: Option<String>,
    /// MediaInfo-style reports offer a Summary/Full switch.
    pub switchable: bool,
    /// Status shown while the report is being produced.
    pub progress: Arc<Mutex<String>>,
    pub cancel: CancelToken,
    view: Option<TextView>,
    buttons: Vec<(Btn, Rect)>,
    hover: Option<Btn>,
    body: Rect,
}

impl Report {
    pub fn new(title: &'static str, path: String, full: bool, id: u64) -> Self {
        Report {
            title,
            path,
            full,
            id,
            text: None,
            error: None,
            switchable: true,
            progress: Arc::new(Mutex::new(String::new())),
            cancel: CancelToken::new(),
            view: None,
            buttons: Vec::new(),
            hover: None,
            body: Rect::default(),
        }
    }

    pub fn is_loading(&self) -> bool {
        self.text.is_none() && self.error.is_none()
    }

    pub fn set_result(&mut self, r: Result<String, String>) {
        match r {
            Ok(t) => {
                self.view = Some(TextView::new(&t, false, None, None));
                self.text = Some(t);
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
                self.text = None;
                self.view = None;
            }
        }
    }

    pub fn loading(&mut self, id: u64) {
        self.id = id;
        self.text = None;
        self.view = None;
        self.error = None;
    }

    pub fn copy_text(&self) -> Option<String> {
        self.view
            .as_ref()
            .and_then(|v| v.copy_text())
            .or(self.text.clone())
    }

    fn header_h(scale: f32) -> f32 {
        (46.0 * scale).round()
    }

    pub fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let sc = cx.scale;
        let p = |d: f32| (d * sc).round();
        cx.fill(r, rgba(0.115, 0.115, 0.12, 1.0));
        let hh = Self::header_h(cx.scale);
        let head = Rect::new(r.x, r.y, r.w, hh);
        cx.hline(r.x, r.right(), head.bottom() - 1.0, cx.theme.separator);
        cx.glyph(
            glyph::REPORT,
            Rect::new(r.x + p(12.0), r.y, p(24.0), hh),
            15.0,
            cx.theme.accent,
        );
        let (tw, _) = cx.measure(self.title, Font::Semibold(14.0));
        cx.text(
            self.title,
            Rect::new(r.x + p(42.0), r.y, tw + p(4.0), hh),
            Font::Semibold(14.0),
            cx.theme.text,
            Align::Left,
        );
        // Segmented Summary / Full switch.
        let mut buttons = Vec::new();
        let seg_h = p(28.0);
        let sy = r.y + (hh - seg_h) / 2.0;
        if self.switchable {
            let mut x = r.x + p(42.0) + tw + p(20.0);
            let seg_total = Rect::new(x, sy, p(150.0), seg_h);
            cx.round(seg_total, p(6.0), rgba(1.0, 1.0, 1.0, 0.06));
            for (b, label) in [(Btn::Summary, "Summary"), (Btn::Full, "Full")] {
                let br = Rect::new(x, sy, p(75.0), seg_h);
                let on = (b == Btn::Full) == self.full;
                if on {
                    cx.round(br.inset(p(2.0)), p(5.0), rgba(1.0, 1.0, 1.0, 0.14));
                } else if self.hover == Some(b) {
                    cx.round(br.inset(p(2.0)), p(5.0), rgba(1.0, 1.0, 1.0, 0.07));
                }
                cx.text(
                    label,
                    br,
                    Font::Ui(12.5),
                    if on { cx.theme.text } else { cx.theme.text2 },
                    Align::Center,
                );
                buttons.push((b, br));
                x += br.w;
            }
        }
        // Right side: Copy, Save, Close.
        let mut right = r.right() - p(8.0);
        for (b, g, label) in [
            (Btn::Close, glyph::CLOSE, ""),
            (Btn::Save, glyph::SAVE, "Save…"),
            (Btn::Copy, glyph::COPY, "Copy"),
        ] {
            let lw = if label.is_empty() {
                0.0
            } else {
                cx.measure(label, Font::Ui(12.5)).0 + p(8.0)
            };
            let w = p(34.0) + lw;
            right -= w;
            let br = Rect::new(right, sy, w, seg_h);
            if self.hover == Some(b) {
                cx.round(br, p(6.0), rgba(1.0, 1.0, 1.0, 0.09));
            }
            cx.glyph(
                g,
                Rect::new(br.x + p(4.0), br.y, p(26.0), br.h),
                13.0,
                cx.theme.text,
            );
            if !label.is_empty() {
                cx.text(
                    label,
                    Rect::new(br.x + p(30.0), br.y, lw, br.h),
                    Font::Ui(12.5),
                    cx.theme.text,
                    Align::Left,
                );
            }
            buttons.push((b, br));
            right -= p(4.0);
        }
        self.buttons = buttons;
        self.body = Rect::new(r.x, head.bottom(), r.w, (r.h - hh).max(0.0));
        match (&mut self.view, &self.error) {
            (Some(v), _) => v.draw(cx, self.body),
            (None, Some(e)) => {
                cx.text_wrapped(
                    e,
                    self.body.inset(p(20.0)),
                    Font::Ui(13.0),
                    rgba(1.0, 0.62, 0.55, 1.0),
                );
            }
            (None, None) => {
                let status = self.progress.lock().clone();
                let t = if status.is_empty() {
                    "Analysing…".to_string()
                } else {
                    status
                };
                cx.text(
                    &t,
                    self.body.inset(p(20.0)),
                    Font::Ui(13.0),
                    cx.theme.text2,
                    Align::Left,
                );
            }
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Btn> {
        let _ = &self.cancel;
        self.buttons
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(b, _)| *b)
    }

    pub fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) -> Click {
        match *ev {
            Event::MouseMove { x, y } => {
                let h = self.hit(x, y);
                if h != self.hover {
                    self.hover = h;
                    cx.redraw = true;
                }
                if h.is_some() {
                    cx.cursor = Cursor::Hand;
                    return Click::None;
                }
            }
            Event::MouseLeave => {
                self.hover = None;
                cx.redraw = true;
            }
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(b) = self.hit(x, y) {
                    cx.redraw = true;
                    return match b {
                        Btn::Summary => Click::Mode(false),
                        Btn::Full => Click::Mode(true),
                        Btn::Copy => Click::Copy,
                        Btn::Save => Click::Save,
                        Btn::Close => Click::Close,
                    };
                }
                if y < r.y + Self::header_h(cx.scale) {
                    return Click::None;
                }
            }
            Event::Key { vk: 0x1B, .. } => return Click::Close,
            _ => {}
        }
        if let Some(v) = self.view.as_mut() {
            v.event(cx, self.body, ev);
        }
        Click::None
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
