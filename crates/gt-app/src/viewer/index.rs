//! The index sheet: every item of a multi-selection as a thumbnail grid (like
//! Quick Look's). Thumbnails come from the shell on a background thread;
//! clicking one shows that item.

use gt_shell::info::Bgra;
use gt_ui::cx::Fonts;
use gt_ui::widgets::bitmap_from_bgra;
use gt_ui::{glyph, rgba, Align, DrawCx, Event, EventCx, Font, HostCx, Rect, Request, View};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

const TILE: f32 = 196.0;
const GAP: f32 = 14.0;
const CAPTION: f32 = 40.0;
const MARGIN: f32 = 24.0;

pub struct IndexView {
    items: Vec<String>,
    current: usize,
    thumbs: Arc<Mutex<Vec<Option<Bgra>>>>,
    bitmaps: Vec<Option<ID2D1Bitmap1>>,
    stop: Arc<AtomicBool>,
    started: bool,
    hover: Option<usize>,
    scroll: f32,
    /// Tile rectangles from the last draw (for hit testing).
    tiles: Vec<Rect>,
    content_h: f32,
    view_h: f32,
    request: Option<Request>,
}

impl IndexView {
    pub fn new(items: Vec<String>, current: usize) -> Self {
        let n = items.len();
        IndexView {
            items,
            current,
            thumbs: Arc::new(Mutex::new((0..n).map(|_| None).collect())),
            bitmaps: (0..n).map(|_| None).collect(),
            stop: Arc::default(),
            started: false,
            hover: None,
            scroll: 0.0,
            tiles: Vec::new(),
            content_h: 0.0,
            view_h: 0.0,
            request: None,
        }
    }

    fn start(&mut self, host: &HostCx) {
        if self.started {
            return;
        }
        self.started = true;
        let (items, thumbs, stop) = (self.items.clone(), self.thumbs.clone(), self.stop.clone());
        let (parent, wake) = (host.parent.0 as isize, host.wake_msg);
        let px = 256;
        let _ = std::thread::Builder::new().name("gt-index".into()).spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            // The current item first, then the rest in order.
            for (i, p) in items.iter().enumerate() {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let t = gt_shell::info::thumbnail(p, px).or_else(|| gt_shell::info::image(p, px, false));
                thumbs.lock()[i] = t;
                if i % 4 == 3 || i + 1 == items.len() {
                    unsafe {
                        let _ = PostMessageW(Some(HWND(parent as *mut _)), wake, WPARAM(0), LPARAM(0));
                    }
                }
            }
        });
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.tiles.iter().position(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
    }
}

impl View for IndexView {
    fn kind(&self) -> &'static str {
        "index"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let n = self.items.len().max(1) as f32;
        let cols = (n * 1.6).sqrt().ceil().clamp(2.0, 6.0);
        let rows = (n / cols).ceil();
        let w = (cols * (TILE + GAP) - GAP + 2.0 * MARGIN) * scale;
        let h = (rows * (TILE + CAPTION + GAP) - GAP + 2.0 * MARGIN) * scale;
        (w.min(max_w * 0.9), h.min(max_h * 0.9))
    }

    fn draw(&mut self, cx: &mut DrawCx, rect: Rect) {
        {
            let mut t = self.thumbs.lock();
            for (i, slot) in t.iter_mut().enumerate() {
                if let (Some(b), None) = (slot.as_ref(), self.bitmaps[i].as_ref()) {
                    self.bitmaps[i] = bitmap_from_bgra(cx.dc, b.width, b.height, &b.data);
                    *slot = None;
                }
            }
        }
        let (tile, gap, cap, margin) = (cx.px(TILE), cx.px(GAP), cx.px(CAPTION), cx.px(MARGIN));
        let cols = (((rect.w - 2.0 * margin + gap) / (tile + gap)).floor() as usize).max(1);
        let rows = self.items.len().div_ceil(cols);
        let grid_w = cols as f32 * (tile + gap) - gap;
        let x0 = rect.x + ((rect.w - grid_w) / 2.0).max(margin);
        self.content_h = rows as f32 * (tile + cap + gap) - gap + 2.0 * margin;
        self.view_h = rect.h;
        self.scroll = self.scroll.clamp(0.0, (self.content_h - rect.h).max(0.0));
        let y0 = rect.y + margin - self.scroll;
        self.tiles.clear();
        cx.clip(rect);
        for (i, path) in self.items.iter().enumerate() {
            let (c, r) = (i % cols, i / cols);
            let t = Rect::new(x0 + c as f32 * (tile + gap), y0 + r as f32 * (tile + cap + gap), tile, tile + cap);
            self.tiles.push(t);
            if t.bottom() < rect.y || t.y > rect.bottom() {
                continue;
            }
            let hovered = self.hover == Some(i);
            let bg = if hovered { rgba(1.0, 1.0, 1.0, 0.09) } else { rgba(1.0, 1.0, 1.0, 0.035) };
            cx.round(t, cx.px(8.0), bg);
            if i == self.current {
                cx.stroke(Rect::new(t.x + 1.0, t.y + 1.0, t.w - 2.0, t.h - 2.0), cx.theme.accent, cx.px(2.0));
            }
            let area = Rect::new(t.x + cx.px(12.0), t.y + cx.px(12.0), t.w - cx.px(24.0), tile - cx.px(18.0));
            match &self.bitmaps[i] {
                Some(b) => {
                    let s = unsafe { b.GetPixelSize() };
                    let k = (area.w / s.width.max(1) as f32).min(area.h / s.height.max(1) as f32).min(2.0);
                    let (w, h) = (s.width as f32 * k, s.height as f32 * k);
                    let d = Rect::new(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h).snap();
                    cx.bitmap(b, d, None, 1.0, false);
                }
                None => cx.glyph(glyph::FILE, area, 40.0, rgba(1.0, 1.0, 1.0, 0.18)),
            }
            let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
            let tr = Rect::new(t.x + cx.px(8.0), t.y + tile, t.w - cx.px(16.0), cap - cx.px(8.0));
            let color = if i == self.current { cx.theme.text } else { cx.theme.text2 };
            cx.text(name, tr, Font::Ui(12.5), color, Align::Center);
        }
        cx.unclip();
    }

    fn event(&mut self, cx: &mut EventCx, _rect: Rect, ev: &Event) {
        match ev {
            Event::MouseMove { x, y } => {
                let h = self.hit(*x, *y);
                if h != self.hover {
                    self.hover = h;
                    cx.redraw = true;
                }
                cx.cursor = if h.is_some() { gt_ui::cx::Cursor::Hand } else { gt_ui::cx::Cursor::Arrow };
            }
            Event::MouseLeave => {
                if self.hover.take().is_some() {
                    cx.redraw = true;
                }
            }
            Event::MouseDown { x, y, .. } => {
                if let Some(i) = self.hit(*x, *y) {
                    self.request = Some(Request::ShowItem(i));
                    cx.redraw = true;
                }
            }
            Event::Wheel { lines, horizontal: false, .. } => {
                self.scroll = (self.scroll - lines * cx.px(48.0)).clamp(0.0, (self.content_h - self.view_h).max(0.0));
                cx.redraw = true;
            }
            _ => {}
        }
    }

    fn attach(&mut self, host: &HostCx) {
        self.start(host);
    }

    fn detach(&mut self) {
        self.stop.store(true, Ordering::Release);
    }

    fn wake(&mut self) -> bool {
        true
    }

    fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    fn subtitle(&self) -> Option<String> {
        Some(format!("{} items", self.items.len()))
    }
}

impl Drop for IndexView {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
