//! Scroll state with Windows 11 style overlay scrollbars.

use crate::cx::{DrawCx, EventCx};
use crate::geom::Rect;
use crate::view::{Event, MouseButton};

#[derive(Clone, Debug, Default)]
pub struct Scroll {
    pub x: f32,
    pub y: f32,
    pub content_w: f32,
    pub content_h: f32,
    pub view_w: f32,
    pub view_h: f32,
    drag: Option<(bool, f32, f32)>, // (vertical, grab offset, start scroll)
    hover: bool,
}

impl Scroll {
    pub fn set_extent(&mut self, content_w: f32, content_h: f32, view_w: f32, view_h: f32) {
        self.content_w = content_w;
        self.content_h = content_h;
        self.view_w = view_w;
        self.view_h = view_h;
        self.clamp();
    }

    pub fn max_x(&self) -> f32 {
        (self.content_w - self.view_w).max(0.0)
    }
    pub fn max_y(&self) -> f32 {
        (self.content_h - self.view_h).max(0.0)
    }

    pub fn clamp(&mut self) {
        self.x = self.x.clamp(0.0, self.max_x());
        self.y = self.y.clamp(0.0, self.max_y());
    }

    pub fn scroll_by(&mut self, dx: f32, dy: f32) -> bool {
        let (ox, oy) = (self.x, self.y);
        self.x += dx;
        self.y += dy;
        self.clamp();
        (ox, oy) != (self.x, self.y)
    }

    fn vthumb(&self, r: Rect, s: f32) -> Option<Rect> {
        if self.content_h <= self.view_h + 0.5 {
            return None;
        }
        let track = r.h - 4.0 * s;
        let th = (track * self.view_h / self.content_h).max(24.0 * s);
        let ty = r.y + 2.0 * s + (track - th) * (self.y / self.max_y().max(1.0));
        let w = if self.hover || self.drag.is_some() { 8.0 * s } else { 4.0 * s };
        Some(Rect::new(r.right() - w - 2.0 * s, ty, w, th))
    }

    fn hthumb(&self, r: Rect, s: f32) -> Option<Rect> {
        if self.content_w <= self.view_w + 0.5 {
            return None;
        }
        let track = r.w - 14.0 * s;
        let tw = (track * self.view_w / self.content_w).max(24.0 * s);
        let tx = r.x + 2.0 * s + (track - tw) * (self.x / self.max_x().max(1.0));
        let h = if self.hover || self.drag.is_some() { 8.0 * s } else { 4.0 * s };
        Some(Rect::new(tx, r.bottom() - h - 2.0 * s, tw, h))
    }

    pub fn draw(&self, cx: &DrawCx, r: Rect) {
        let s = cx.scale;
        if let Some(t) = self.vthumb(r, s) {
            cx.round(t, t.w / 2.0, cx.theme.scrollbar);
        }
        if let Some(t) = self.hthumb(r, s) {
            cx.round(t, t.h / 2.0, cx.theme.scrollbar);
        }
    }

    /// Handle scrollbar dragging and wheel; returns true when consumed.
    pub fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event, line: f32) -> bool {
        let s = cx.scale;
        match *ev {
            Event::Wheel { lines, horizontal, ctrl, .. } if !ctrl => {
                let d = -lines * line * 3.0;
                let moved = if horizontal || (self.content_h <= self.view_h && self.content_w > self.view_w) { self.scroll_by(-d, 0.0) } else { self.scroll_by(0.0, d) };
                cx.redraw |= moved;
                moved
            }
            Event::MouseDown { x, y, button: MouseButton::Left, .. } => {
                let near_v = x >= r.right() - 14.0 * s && self.content_h > self.view_h;
                let near_h = y >= r.bottom() - 14.0 * s && self.content_w > self.view_w;
                if near_v {
                    let t = self.vthumb(r, s).unwrap_or_default();
                    if !t.contains(t.x + 1.0, y) {
                        // Page jump towards the click.
                        let page = if y < t.y { -self.view_h } else { self.view_h };
                        self.scroll_by(0.0, page);
                    }
                    self.drag = Some((true, y, self.y));
                    cx.capture = Some(true);
                    cx.redraw = true;
                    return true;
                }
                if near_h {
                    self.drag = Some((false, x, self.x));
                    cx.capture = Some(true);
                    cx.redraw = true;
                    return true;
                }
                false
            }
            Event::MouseMove { x, y } => {
                let hv = x >= r.right() - 14.0 * s || y >= r.bottom() - 14.0 * s;
                if hv != self.hover {
                    self.hover = hv;
                    cx.redraw = true;
                }
                if let Some((vertical, grab, start)) = self.drag {
                    if vertical {
                        let track = r.h - 4.0 * s;
                        let th = (track * self.view_h / self.content_h).max(24.0 * s);
                        let ratio = self.max_y() / (track - th).max(1.0);
                        self.y = start + (y - grab) * ratio;
                    } else {
                        let track = r.w - 14.0 * s;
                        let tw = (track * self.view_w / self.content_w).max(24.0 * s);
                        let ratio = self.max_x() / (track - tw).max(1.0);
                        self.x = start + (x - grab) * ratio;
                    }
                    self.clamp();
                    cx.redraw = true;
                    return true;
                }
                false
            }
            Event::MouseUp { .. } => {
                if self.drag.take().is_some() {
                    cx.capture = Some(false);
                    cx.redraw = true;
                    return true;
                }
                false
            }
            Event::MouseLeave => {
                if self.hover {
                    self.hover = false;
                    cx.redraw = true;
                }
                false
            }
            _ => false,
        }
    }

    /// PageUp/PageDown/Home/End handling.
    pub fn key(&mut self, vk: u16, line: f32) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_END, VK_HOME, VK_NEXT, VK_PRIOR};
        match vk {
            v if v == VK_PRIOR.0 => self.scroll_by(0.0, -(self.view_h - line)),
            v if v == VK_NEXT.0 => self.scroll_by(0.0, self.view_h - line),
            v if v == VK_HOME.0 => self.scroll_by(0.0, -self.y),
            v if v == VK_END.0 => self.scroll_by(0.0, self.max_y()),
            _ => false,
        }
    }
}
