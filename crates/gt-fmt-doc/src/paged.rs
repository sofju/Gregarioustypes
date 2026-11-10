//! Paged document view: continuous vertical scrolling for multi-page documents,
//! fit-to-window for single pages (SVG, EPS, one artboard), zoom about the
//! cursor, and background rasterization at exactly the on-screen resolution.
//!
//! Each page keeps a "base" bitmap of the whole page (capped at ~24 MP) and,
//! when zoomed in beyond that cap, a "detail" bitmap of the visible region at
//! full resolution. Older bitmaps stay on screen, stretched, until crisp ones arrive.

use crate::pages::{PageSource, Raster, Renderer};
use gt_ui::cx::{Cursor, Fonts};
use gt_ui::scroll::Scroll;
use gt_ui::widgets::bitmap_from_bgra;
use gt_ui::{glyph, rgba, Action, Align, DrawCx, Event, EventCx, Font, HostCx, MouseButton, Rect, View};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_0, VK_1, VK_ADD, VK_DOWN, VK_OEM_MINUS, VK_OEM_PLUS, VK_SUBTRACT, VK_UP};

const MAX_BASE_PX: f32 = 24.0e6;
const MAX_DIM: f32 = 8192.0;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    FitWidth,
    FitPage,
    /// Multiplier of 100 % (1 unit = `dip_per_unit` DIPs).
    Custom(f32),
}

struct Tile {
    scale: f32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    bmp: ID2D1Bitmap1,
}

#[derive(Default)]
struct PageBmps {
    base: Option<Tile>,
    detail: Option<Tile>,
}

pub struct PagedView {
    r: Renderer,
    src: Arc<dyn PageSource>,
    sizes: Vec<(f32, f32)>,
    dpu: f32,
    paper: bool,
    pub zoom: Zoom,
    /// Effective zoom at the last layout.
    z: f32,
    scroll: Scroll,
    bmps: Vec<PageBmps>,
    incoming: Vec<(usize, f32, i32, i32, Raster)>,
    pending: HashSet<u64>,
    req_scale: f32,
    /// Page tops in content pixels (last layout).
    tops: Vec<f32>,
    content_w: f32,
    view: Rect,
    drag: Option<(f32, f32, f32, f32)>,
    indicator_until: Option<Instant>,
    current: usize,
    facts: Vec<(String, String)>,
    kind: &'static str,
    /// Text drawn over the pages (a note about the source), and until when.
    pub banner: Option<String>,
    banner_until: Option<Instant>,
}

fn hash_key(page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    (page, scale.to_bits(), x, y, w, h).hash(&mut hs);
    hs.finish()
}

impl PagedView {
    pub fn new(src: Arc<dyn PageSource>, facts: Vec<(String, String)>) -> Self {
        let n = src.count();
        let sizes: Vec<(f32, f32)> = (0..n).map(|i| src.size(i)).collect();
        let dpu = src.dip_per_unit();
        let paper = src.paper();
        // Slides and landscape pages fit whole; portrait documents fit their width.
        let landscape = sizes.first().is_some_and(|s| s.0 > s.1 * 1.05);
        let zoom = if n > 1 && !landscape { Zoom::FitWidth } else { Zoom::FitPage };
        PagedView {
            r: Renderer::new(src.clone()),
            src,
            sizes,
            dpu,
            paper,
            zoom,
            z: 1.0,
            scroll: Scroll::default(),
            bmps: (0..n).map(|_| PageBmps::default()).collect(),
            incoming: Vec::new(),
            pending: HashSet::new(),
            req_scale: 0.0,
            tops: Vec::new(),
            content_w: 0.0,
            view: Rect::default(),
            drag: None,
            indicator_until: None,
            current: 0,
            facts,
            kind: if n > 1 && paper { "doc" } else { "vector" },
            banner: None,
            banner_until: None,
        }
    }

    /// A note shown over the pages for a few seconds ("previews of 2 of 13 pages").
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.banner = Some(note.into());
        self.banner_until = Some(Instant::now() + Duration::from_secs(6));
        self
    }

    /// Show a raster rendered during loading as page `page`'s base until the real one arrives.
    pub fn with_first(mut self, page: usize, scale: f32, raster: Raster) -> Self {
        self.incoming.push((page, scale, 0, 0, raster));
        self
    }

    pub fn source(&self) -> &Arc<dyn PageSource> {
        &self.src
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    fn margin(&self, scale: f32) -> f32 {
        ((if self.sizes.len() > 1 { 16.0 } else { 24.0 }) * scale).round()
    }

    fn gap(scale: f32) -> f32 {
        (12.0 * scale).round()
    }

    /// Effective zoom for the current mode and view size.
    fn effective_zoom(&self, view: Rect, scale: f32) -> f32 {
        let m = self.margin(scale);
        let unit = self.dpu * scale;
        match self.zoom {
            Zoom::Custom(z) => z,
            Zoom::FitWidth => {
                let mw = self.sizes.iter().map(|s| s.0).fold(1.0f32, f32::max);
                ((view.w - 2.0 * m - 8.0 * scale) / (mw * unit)).clamp(MIN_ZOOM, MAX_ZOOM)
            }
            Zoom::FitPage => {
                let (pw, ph) = self.sizes.get(self.current).copied().unwrap_or((612.0, 792.0));
                ((view.w - 2.0 * m) / (pw * unit)).min((view.h - 2.0 * m) / (ph * unit)).clamp(MIN_ZOOM, MAX_ZOOM)
            }
        }
    }

    fn page_px(&self, i: usize, scale: f32) -> (f32, f32) {
        let (w, h) = self.sizes[i];
        let k = self.dpu * self.z * scale;
        ((w * k).round().max(1.0), (h * k).round().max(1.0))
    }

    fn layout(&mut self, view: Rect, scale: f32) {
        self.view = view;
        self.z = self.effective_zoom(view, scale);
        let m = self.margin(scale);
        let g = Self::gap(scale);
        self.tops.clear();
        let mut y = m;
        let mut maxw = 0.0f32;
        for i in 0..self.sizes.len() {
            let (w, h) = self.page_px(i, scale);
            self.tops.push(y);
            y += h + g;
            maxw = maxw.max(w);
        }
        let mut content_h = y - g + m;
        let content_w = (maxw + 2.0 * m).max(view.w);
        // A single page smaller than the view is centred vertically.
        if self.sizes.len() == 1 && content_h < view.h {
            self.tops[0] += ((view.h - content_h) / 2.0).round();
            content_h = view.h;
        }
        self.content_w = content_w;
        self.scroll.set_extent(content_w, content_h.max(view.h), view.w, view.h);
    }

    /// On-screen rectangle of page `i` (last layout).
    fn page_rect(&self, i: usize, scale: f32) -> Rect {
        let (w, h) = self.page_px(i, scale);
        let x = self.view.x + ((self.content_w - w) / 2.0).round() - self.scroll.x;
        let y = self.view.y + self.tops[i] - self.scroll.y;
        Rect::new(x, y, w, h)
    }

    fn visible_pages(&self, scale: f32) -> std::ops::Range<usize> {
        if self.tops.is_empty() {
            return 0..0;
        }
        let top = self.scroll.y;
        let bottom = self.scroll.y + self.view.h;
        let first = self.tops.partition_point(|&t| t <= top).saturating_sub(1);
        let mut last = first;
        while last < self.tops.len() && self.tops[last] < bottom {
            last += 1;
        }
        let first = (first..last).find(|&i| self.tops[i] + self.page_px(i, scale).1 >= top).unwrap_or(first);
        first..last.max(first + 1).min(self.tops.len())
    }

    /// Change zoom keeping the document point under (ax, ay) in place.
    fn zoom_to(&mut self, new_zoom: Zoom, ax: f32, ay: f32, scale: f32) {
        let view = self.view;
        if self.tops.is_empty() {
            self.zoom = new_zoom;
            return;
        }
        let cy = ay - view.y + self.scroll.y;
        let i = self.tops.partition_point(|&t| t <= cy).saturating_sub(1).min(self.sizes.len() - 1);
        let r0 = self.page_rect(i, scale);
        let fx = (ax - r0.x) / r0.w;
        let fy = (ay - r0.y) / r0.h;
        self.zoom = match new_zoom {
            Zoom::Custom(z) => Zoom::Custom(z.clamp(MIN_ZOOM, MAX_ZOOM)),
            z => z,
        };
        self.layout(view, scale);
        let r1 = self.page_rect(i, scale);
        let (nx, ny) = (r1.x + fx * r1.w, r1.y + fy * r1.h);
        self.scroll.scroll_by(nx - ax, ny - ay);
        self.indicator_until = None;
    }

    fn zoom_by(&mut self, factor: f32, ax: f32, ay: f32, scale: f32) {
        let z = (self.z * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.zoom_to(Zoom::Custom(z), ax, ay, scale);
    }

    fn take_incoming(&mut self, cx: &DrawCx) {
        for d in self.r.take_done() {
            self.pending.remove(&d.key);
            if let Some(r) = d.raster {
                self.incoming.push((d.page, d.scale, d.x, d.y, r));
            }
        }
        for (page, scale, x, y, r) in self.incoming.drain(..) {
            let Some(bmp) = bitmap_from_bgra(cx.dc, r.w, r.h, &r.data) else { continue };
            let Some(slot) = self.bmps.get_mut(page) else { continue };
            let tile = Tile { scale, x, y, w: r.w, h: r.h, bmp };
            let (pw, ph) = self.sizes[page];
            let whole = x == 0 && y == 0 && (r.w as f32 - pw * scale).abs() <= 1.5 && (r.h as f32 - ph * scale).abs() <= 1.5;
            if whole {
                slot.base = Some(tile);
            } else {
                slot.detail = Some(tile);
            }
        }
    }

    /// Queue renders for what is (about to be) on screen.
    fn request(&mut self, scale: f32) {
        let s = self.dpu * self.z * scale;
        if (s - self.req_scale).abs() > 1e-4 {
            self.r.invalidate();
            self.pending.clear();
            self.req_scale = s;
        }
        let vis = self.visible_pages(scale);
        let n = self.sizes.len();
        let mut jobs: Vec<(u64, usize, f32, (i32, i32, u32, u32))> = Vec::new();
        // Neighbours first (they run last: the queue is LIFO).
        let mut order: Vec<usize> = Vec::new();
        if vis.end < n {
            order.push(vis.end);
        }
        if vis.end + 1 < n {
            order.push(vis.end + 1);
        }
        if vis.start > 0 {
            order.push(vis.start - 1);
        }
        let visible: Vec<usize> = vis.clone().collect();
        order.extend(visible.iter().rev());
        for &i in &order {
            let (pw, ph) = (self.sizes[i].0 * s, self.sizes[i].1 * s);
            let cap = (MAX_BASE_PX / (pw * ph)).sqrt().min(MAX_DIM / pw.max(ph)).min(1.0);
            let bs = s * cap;
            let (bw, bh) = ((self.sizes[i].0 * bs).round().max(1.0) as u32, (self.sizes[i].1 * bs).round().max(1.0) as u32);
            let have = self.bmps[i].base.as_ref().is_some_and(|t| (t.scale - bs).abs() < 1e-4);
            if !have {
                jobs.push((hash_key(i, bs, 0, 0, bw, bh), i, bs, (0, 0, bw, bh)));
            }
            // Zoomed in past the cap: a full-resolution tile of the visible part.
            if cap < 1.0 && visible.contains(&i) {
                let pr = self.page_rect(i, scale);
                let vx0 = (self.view.x - pr.x).max(0.0);
                let vy0 = (self.view.y - pr.y).max(0.0);
                let vx1 = (self.view.right() - pr.x).min(pr.w);
                let vy1 = (self.view.bottom() - pr.y).min(pr.h);
                if vx1 > vx0 && vy1 > vy0 {
                    let ok = self.bmps[i].detail.as_ref().is_some_and(|t| {
                        (t.scale - s).abs() < 1e-4 && t.x as f32 <= vx0 && t.y as f32 <= vy0 && (t.x + t.w as i32) as f32 >= vx1 && (t.y + t.h as i32) as f32 >= vy1
                    });
                    if !ok {
                        let mx = self.view.w * 0.5;
                        let my = self.view.h * 0.5;
                        let grid = 256.0;
                        let x0 = (((vx0 - mx).max(0.0)) / grid).floor() * grid;
                        let y0 = (((vy0 - my).max(0.0)) / grid).floor() * grid;
                        let x1 = (vx1 + mx).min(pr.w).ceil();
                        let y1 = (vy1 + my).min(pr.h).ceil();
                        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
                        jobs.push((hash_key(i, s, x0 as i32, y0 as i32, w, h), i, s, (x0 as i32, y0 as i32, w, h)));
                    }
                }
            }
        }
        for (key, i, sc, rect) in jobs {
            if self.pending.insert(key) {
                self.r.submit(key, i, sc, rect);
            }
        }
        // Forget bitmaps far from the viewport.
        let keep_lo = vis.start.saturating_sub(6);
        let keep_hi = vis.end + 6;
        for (i, b) in self.bmps.iter_mut().enumerate() {
            if i < keep_lo || i >= keep_hi {
                b.base = None;
            }
            if !vis.contains(&i) {
                b.detail = None;
            }
        }
    }

    fn update_current(&mut self, scale: f32) {
        let vis = self.visible_pages(scale);
        if vis.is_empty() {
            return;
        }
        // The page covering the middle of the view.
        let mid = self.scroll.y + self.view.h * 0.4;
        let i = self.tops.partition_point(|&t| t <= mid).saturating_sub(1);
        self.current = i.clamp(vis.start, vis.end - 1);
    }

    fn label(&self, i: usize) -> String {
        match self.src.label(i) {
            Some(l) if !l.is_empty() && l != (i + 1).to_string() => format!("{l} ({} of {})", i + 1, self.sizes.len()),
            _ => format!("{} of {}", i + 1, self.sizes.len()),
        }
    }

    fn scroll_event(&mut self, cx: &mut EventCx) {
        if self.sizes.len() > 1 {
            self.indicator_until = Some(Instant::now() + Duration::from_millis(1200));
        }
        self.update_current(cx.scale);
        cx.redraw = true;
    }

    fn go_page(&mut self, i: usize, scale: f32) {
        if let Some(&t) = self.tops.get(i) {
            let m = self.margin(scale);
            self.scroll.y = 0.0;
            self.scroll.scroll_by(0.0, t - m);
        }
    }
}

fn draw_checker(cx: &DrawCx, r: Rect) {
    let c = (8.0 * cx.scale).round().max(4.0);
    cx.fill(r, rgba(0.85, 0.85, 0.85, 1.0));
    let mut y = r.y;
    let mut row = 0;
    while y < r.bottom() {
        let mut x = r.x + if row % 2 == 0 { 0.0 } else { c };
        while x < r.right() {
            let w = c.min(r.right() - x);
            let h = c.min(r.bottom() - y);
            cx.fill(Rect::new(x, y, w, h), rgba(1.0, 1.0, 1.0, 1.0));
            x += 2.0 * c;
        }
        y += c;
        row += 1;
    }
}

impl View for PagedView {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let m = self.margin(scale);
        let (pw, ph) = self.sizes.first().copied().unwrap_or((612.0, 792.0));
        let unit = self.dpu * scale;
        let (pw, ph) = (pw * unit, ph * unit);
        let k = ((max_w * 0.92 - 2.0 * m) / pw).min((max_h * 0.94 - 2.0 * m) / ph);
        // Vector art is not blown up beyond 100 % just to fill the screen.
        let k = if self.paper { k } else { k.min(1.0).max(((360.0 * scale) / pw.max(ph)).min(k)) };
        let extra = if self.sizes.len() > 1 { 14.0 * scale } else { 0.0 };
        ((pw * k + 2.0 * m + extra).max(320.0 * scale), (ph * k + 2.0 * m).max(240.0 * scale))
    }

    fn attach(&mut self, host: &HostCx) {
        self.r.set_notify(Some((host.parent.0 as isize, host.wake_msg)));
    }

    fn detach(&mut self) {
        self.r.set_notify(None);
    }

    fn wake(&mut self) -> bool {
        !self.r.mailbox.done.lock().is_empty()
    }

    fn draw(&mut self, cx: &mut DrawCx, rect: Rect) {
        let sc = cx.scale;
        self.layout(rect, sc);
        self.take_incoming(cx);
        self.update_current(sc);
        self.request(sc);
        cx.clip(rect);
        let vis = self.visible_pages(sc);
        for i in vis {
            let pr = self.page_rect(i, sc).snap();
            if self.paper {
                let sh = (2.0 * sc).round();
                cx.fill(Rect::new(pr.x - 1.0, pr.y - 1.0, pr.w + 2.0, pr.h + 2.0), rgba(0.0, 0.0, 0.0, 0.35));
                cx.fill(Rect::new(pr.x + sh, pr.y + sh, pr.w, pr.h), rgba(0.0, 0.0, 0.0, 0.25));
                cx.fill(pr, rgba(1.0, 1.0, 1.0, 1.0));
            } else {
                draw_checker(cx, pr);
            }
            let b = &self.bmps[i];
            if let Some(t) = &b.base {
                cx.bitmap(&t.bmp, pr, None, 1.0, false);
            }
            if let Some(t) = &b.detail {
                let k = pr.w / (self.sizes[i].0 * t.scale);
                let d = Rect::new(pr.x + t.x as f32 * k, pr.y + t.y as f32 * k, t.w as f32 * k, t.h as f32 * k);
                cx.bitmap(&t.bmp, d, None, 1.0, (k - 1.0).abs() < 0.001);
            }
        }
        cx.unclip();
        self.scroll.draw(cx, rect);
        if self.banner_until.is_some_and(|u| Instant::now() >= u) {
            self.banner = None;
            self.banner_until = None;
        }
        if let Some(b) = &self.banner {
            let (tw, _) = cx.measure(b, Font::Ui(12.5));
            let p = Rect::new(rect.x + (rect.w - tw) / 2.0 - cx.px(14.0), rect.bottom() - cx.px(48.0), tw + cx.px(28.0), cx.px(30.0));
            cx.round(p, p.h / 2.0, rgba(0.1, 0.1, 0.1, 0.88));
            cx.text(b, p, Font::Ui(12.5), cx.theme.text, Align::Center);
        } else if let Some(until) = self.indicator_until {
            let left = until.saturating_duration_since(Instant::now()).as_secs_f32();
            if left > 0.0 {
                let a = (left / 0.3).min(1.0);
                let t = self.label(self.current);
                let (tw, _) = cx.measure(&t, Font::Semibold(12.5));
                let p = Rect::new(rect.x + (rect.w - tw) / 2.0 - cx.px(14.0), rect.bottom() - cx.px(48.0), tw + cx.px(28.0), cx.px(30.0));
                cx.round(p, p.h / 2.0, rgba(0.1, 0.1, 0.1, 0.85 * a));
                let mut c = cx.theme.text;
                c.a = a;
                cx.text(&t, p, Font::Semibold(12.5), c, Align::Center);
            } else {
                self.indicator_until = None;
            }
        }
    }

    fn next_frame_in(&self) -> Option<Duration> {
        if let Some(u) = self.banner_until {
            return Some(u.saturating_duration_since(Instant::now()).max(Duration::from_millis(16)));
        }
        self.indicator_until.map(|u| {
            let left = u.saturating_duration_since(Instant::now());
            if left > Duration::from_millis(300) {
                left - Duration::from_millis(300)
            } else {
                Duration::from_millis(16)
            }
        })
    }

    fn tick(&mut self, _env: &gt_ui::ViewEnv) -> bool {
        self.indicator_until.is_some() || self.banner_until.is_some()
    }

    fn event(&mut self, cx: &mut EventCx, rect: Rect, ev: &Event) {
        let sc = cx.scale;
        if self.tops.is_empty() {
            self.layout(rect, sc);
        }
        match *ev {
            Event::Wheel { x, y, lines, ctrl: true, .. } => {
                self.zoom_by(1.15f32.powf(lines), x, y, sc);
                cx.redraw = true;
                return;
            }
            Event::MouseDown { x, y, button: MouseButton::Left, double: true, .. } if self.sizes.len() == 1 => {
                if matches!(self.zoom, Zoom::Custom(_)) {
                    self.zoom_to(Zoom::FitPage, x, y, sc);
                } else {
                    let target = (self.z * 2.5).max(1.0);
                    self.zoom_to(Zoom::Custom(target), x, y, sc);
                }
                cx.redraw = true;
                return;
            }
            _ => {}
        }
        if self.scroll.event(cx, rect, ev, 40.0 * sc) {
            self.scroll_event(cx);
            return;
        }
        match *ev {
            Event::MouseDown { x, y, button: MouseButton::Left, .. } => {
                if self.scroll.max_x() > 0.0 || self.scroll.max_y() > 0.0 {
                    self.drag = Some((x, y, self.scroll.x, self.scroll.y));
                    cx.capture = Some(true);
                }
            }
            Event::MouseMove { x, y } => {
                if let Some((x0, y0, sx, sy)) = self.drag {
                    self.scroll.x = sx - (x - x0);
                    self.scroll.y = sy - (y - y0);
                    self.scroll.clamp();
                    self.scroll_event(cx);
                }
                if self.scroll.max_x() > 0.0 || self.scroll.max_y() > 0.0 {
                    cx.cursor = Cursor::Hand;
                }
            }
            Event::MouseUp { .. } => {
                if self.drag.take().is_some() {
                    cx.capture = Some(false);
                }
            }
            Event::Key { vk, ctrl, .. } => {
                let center = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
                if ctrl && (vk == VK_OEM_PLUS.0 || vk == VK_ADD.0) {
                    self.zoom_by(1.25, center.0, center.1, sc);
                } else if ctrl && (vk == VK_OEM_MINUS.0 || vk == VK_SUBTRACT.0) {
                    self.zoom_by(0.8, center.0, center.1, sc);
                } else if ctrl && vk == VK_0.0 {
                    let landscape = self.sizes.first().is_some_and(|s| s.0 > s.1 * 1.05);
                    let fit = if self.sizes.len() > 1 && !landscape { Zoom::FitWidth } else { Zoom::FitPage };
                    self.zoom_to(fit, center.0, center.1, sc);
                } else if ctrl && vk == VK_1.0 {
                    self.zoom_to(Zoom::Custom(1.0), center.0, center.1, sc);
                } else if !ctrl && (vk == VK_UP.0 || vk == VK_DOWN.0) && self.scroll.max_y() > 0.0 {
                    let d = if vk == VK_UP.0 { -40.0 } else { 40.0 } * sc;
                    self.scroll.scroll_by(0.0, d);
                    self.scroll_event(cx);
                } else if self.scroll.key(vk, 40.0 * sc) {
                    self.scroll_event(cx);
                    return;
                } else {
                    return;
                }
                cx.redraw = true;
            }
            _ => {}
        }
    }

    fn actions(&self) -> Vec<Action> {
        let pct = format!("{:.0}%", self.z * 100.0);
        let multi = self.sizes.len() > 1;
        let (tip, active) = match self.zoom {
            Zoom::FitWidth => ("Fit width — click for whole page", true),
            Zoom::FitPage => (if multi { "Whole page — click to fit width" } else { "Fit to window" }, true),
            Zoom::Custom(_) => ("Zoom — click to fit (Ctrl+0)", false),
        };
        vec![Action::new("fit", glyph::ZOOM_IN, tip).label(pct).active(active)]
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        if id == "fit" {
            let sc = cx.scale;
            let next = match (self.zoom, self.sizes.len() > 1) {
                (Zoom::FitWidth, _) => Zoom::FitPage,
                (_, true) => Zoom::FitWidth,
                (_, false) => Zoom::FitPage,
            };
            let keep = self.current;
            self.zoom = next;
            self.layout(self.view, sc);
            if next == Zoom::FitPage && self.sizes.len() > 1 {
                self.go_page(keep, sc);
            }
            cx.redraw = true;
        }
    }

    fn copy_text(&self) -> Option<String> {
        self.src.text(self.current)
    }

    fn info(&self) -> Vec<(String, String)> {
        self.facts.clone()
    }

    fn subtitle(&self) -> Option<String> {
        if self.sizes.len() > 1 {
            Some(format!("Page {}", self.label(self.current)))
        } else {
            None
        }
    }

    fn wants_focus(&self) -> bool {
        true
    }
}
