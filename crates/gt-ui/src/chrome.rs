//! Title bar: proxy icon, file name, "n of N", and the button strip.

use crate::cx::{Align, DrawCx, Font, Fonts};
use crate::geom::Rect;
use crate::glyph;
use crate::view::Action;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

#[derive(Clone, Debug)]
pub struct Button {
    pub id: String,
    pub glyph: char,
    pub label: Option<String>,
    pub tip: String,
    pub active: bool,
    pub rect: Rect,
    /// Label hidden because the bar is too narrow (the tooltip still names it).
    pub compact: bool,
}

impl Button {
    fn new(id: impl Into<String>, glyph: char, tip: impl Into<String>) -> Self {
        Self { id: id.into(), glyph, label: None, tip: tip.into(), active: false, rect: Rect::default(), compact: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Button(usize),
    /// Draggable caption area.
    Caption,
    /// The proxy icon (drag the file out).
    Icon,
    None,
}

/// State the window feeds into the title bar.
#[derive(Clone, Debug, Default)]
pub struct ChromeState {
    pub multi: Option<(usize, usize)>,
    pub pinned: bool,
    pub fullscreen: bool,
    pub open_with: Option<String>,
    pub info_open: bool,
    pub has_file: bool,
    /// Window-level actions (ids get an "app:" prefix), shown after the view's.
    pub extra: Vec<Action>,
}

#[derive(Default)]
pub struct Chrome {
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<ID2D1Bitmap1>,
    pub buttons: Vec<Button>,
    icon_rect: Rect,
    caption: Rect,
    counter: Option<(usize, usize)>,
    hover: Option<usize>,
    pressed: Option<usize>,
}

pub fn height(scale: f32) -> f32 {
    (40.0 * scale).round()
}

impl Chrome {
    /// Rebuild the button list. View actions come first (left of the standard buttons).
    pub fn set_buttons(&mut self, view_actions: &[Action], st: &ChromeState) {
        let mut b = Vec::new();
        if let Some(back) = st.extra.iter().find(|a| a.id == "back") {
            b.push(Button::new("app:back", back.glyph, back.tip.clone()));
        }
        if st.multi.is_some() {
            b.push(Button::new("prev", glyph::PREV, "Previous item"));
            b.push(Button::new("next", glyph::NEXT, "Next item"));
        }
        for a in view_actions {
            let mut x = Button::new(format!("view:{}", a.id), a.glyph, match &a.label {
                Some(l) => format!("{} ({l})", a.tip),
                None => a.tip.clone(),
            });
            x.label = a.label.clone();
            x.active = a.active;
            b.push(x);
        }
        for a in st.extra.iter().filter(|a| a.id != "back") {
            let mut x = Button::new(format!("app:{}", a.id), a.glyph, a.tip.clone());
            x.label = a.label.clone();
            x.active = a.active;
            b.push(x);
        }
        if st.has_file {
            let mut info = Button::new("info", glyph::INFO, "Info (Ctrl+I)");
            info.active = st.info_open;
            b.push(info);
            let mut open = Button::new("open", glyph::OPEN_IN_NEW, "Open with the default app (Enter)");
            open.label = Some(match &st.open_with {
                Some(app) => format!("Open with {app}"),
                None => "Open".into(),
            });
            b.push(open);
            b.push(Button::new("openwith", glyph::OPEN_WITH, "Open with another app…"));
        }
        let mut pin = Button::new("pin", if st.pinned { glyph::UNPIN } else { glyph::PIN }, if st.pinned { "Unpin" } else { "Keep open" });
        pin.active = st.pinned;
        b.push(pin);
        b.push(Button::new("fullscreen", if st.fullscreen { glyph::BACK_TO_WINDOW } else { glyph::FULLSCREEN }, "Full screen (Alt+Space / F11)"));
        b.push(Button::new("close", glyph::CLOSE, "Close (Space / Esc)"));
        self.counter = st.multi;
        self.buttons = b;
        self.hover = None;
        self.pressed = None;
    }

    pub fn layout(&mut self, fonts: &mut Fonts, scale: f32, width: f32) {
        let h = height(scale);
        let bw = (40.0 * scale).round();
        let pad = (6.0 * scale).round();
        let n = self.buttons.len();
        let is_nav = |id: &str| id == "prev" || id == "next" || id == "app:back";
        // The file name keeps at least ~30% of the bar: drop button labels until it
        // fits, view-action labels first, then "Open with …".
        let widths = |fonts: &mut Fonts, buttons: &[Button], labels: &[bool]| -> Vec<f32> {
            buttons
                .iter()
                .zip(labels)
                .map(|(b, &show)| match (&b.label, show) {
                    (Some(l), true) => fonts.measure(l, Font::Ui(12.5), scale, 10_000.0, false).0 + (44.0 * scale).round(),
                    _ if b.id == "close" => (46.0 * scale).round(),
                    _ => bw,
                })
                .collect()
        };
        let mut labels: Vec<bool> = self.buttons.iter().map(|b| b.label.is_some()).collect();
        let nav_w = self.buttons.iter().filter(|b| is_nav(&b.id)).count() as f32 * (32.0 * scale).round();
        let min_caption = (width * 0.3).max(160.0 * scale);
        let mut drop_order: Vec<usize> = (0..n).filter(|&i| labels[i] && self.buttons[i].id.starts_with("view:")).collect();
        drop_order.extend((0..n).filter(|&i| labels[i] && self.buttons[i].id.starts_with("app:")));
        drop_order.extend((0..n).filter(|&i| labels[i] && !self.buttons[i].id.starts_with("view:") && !self.buttons[i].id.starts_with("app:")));
        let mut ws = widths(fonts, &self.buttons, &labels);
        for i in drop_order {
            let strip: f32 = (0..n).filter(|&j| !is_nav(&self.buttons[j].id)).map(|j| ws[j]).sum();
            if width - strip - nav_w - (40.0 * scale) >= min_caption {
                break;
            }
            labels[i] = false;
            ws = widths(fonts, &self.buttons, &labels);
        }
        for (b, show) in self.buttons.iter_mut().zip(&labels) {
            b.compact = !show;
        }
        // Right-aligned strip, except prev/next which sit at the far left.
        let mut left_x = pad;
        let mut right_x = width;
        for i in (0..n).rev() {
            if is_nav(&self.buttons[i].id) {
                continue;
            }
            right_x -= ws[i];
            self.buttons[i].rect = Rect::new(right_x, 0.0, ws[i], h);
        }
        for b in self.buttons.iter_mut().filter(|b| b.id == "app:back" || b.id == "prev" || b.id == "next") {
            b.rect = Rect::new(left_x, (4.0 * scale).round(), (32.0 * scale).round(), h - (8.0 * scale).round());
            left_x += b.rect.w;
        }
        let icon = (20.0 * scale).round();
        self.icon_rect = Rect::new(left_x + pad, ((h - icon) / 2.0).round(), icon, icon);
        self.caption = Rect::new(self.icon_rect.right() + pad, 0.0, (right_x - self.icon_rect.right() - pad * 2.0).max(0.0), h);
    }

    pub fn draw(&mut self, cx: &mut DrawCx, width: f32) {
        let s = cx.scale;
        let h = height(s);
        cx.fill(Rect::new(0.0, 0.0, width, h), cx.theme.title_bg);
        cx.hline(0.0, width, h - 1.0, cx.theme.separator);
        if let Some(icon) = &self.icon {
            cx.bitmap(icon, self.icon_rect, None, 1.0, false);
        } else {
            cx.glyph(glyph::DOC, self.icon_rect, 16.0, cx.theme.text2);
        }
        // Title, subtitle and counter share the caption area.
        let mut title = self.title.clone();
        if let Some((i, n)) = self.counter {
            title = format!("{title}  —  {} of {n}", i + 1);
        }
        let (tw, _) = cx.measure(&title, Font::Semibold(13.0));
        let cap = self.caption;
        cx.text(&title, cap, Font::Semibold(13.0), cx.theme.text, Align::Left);
        if let Some(sub) = &self.subtitle {
            let x = cap.x + tw + (12.0 * s).round();
            if x < cap.right() {
                cx.text(sub, Rect::new(x, 0.0, cap.right() - x, h), Font::Ui(12.0), cx.theme.text2, Align::Left);
            }
        }
        for (i, b) in self.buttons.iter().enumerate() {
            let r = b.rect;
            let hovered = self.hover == Some(i);
            let inner = if b.id == "close" { r } else { r.inset_xy((3.0 * s).round(), (5.0 * s).round()) };
            if hovered {
                let c = if b.id == "close" {
                    cx.theme.close_hover
                } else if self.pressed == Some(i) {
                    cx.theme.pressed
                } else {
                    cx.theme.hover
                };
                if b.id == "close" {
                    cx.fill(inner, c);
                } else {
                    cx.round(inner, 4.0 * s, c);
                }
            } else if b.active {
                cx.round(inner, 4.0 * s, cx.theme.hover);
            }
            let fg = if b.active { cx.theme.accent } else { cx.theme.text };
            match b.label.as_ref().filter(|_| !b.compact) {
                Some(label) => {
                    let g = Rect::new(r.x + (8.0 * s).round(), r.y, (24.0 * s).round(), r.h);
                    cx.glyph(b.glyph, g, 14.0, fg);
                    cx.text(label, Rect::new(g.right() + (2.0 * s).round(), r.y, r.w - g.w - (14.0 * s).round(), r.h), Font::Ui(12.5), fg, Align::Left);
                }
                None => cx.glyph(b.glyph, r, if b.id == "close" { 13.0 } else { 15.0 }, fg),
            }
        }
    }

    pub fn hit(&self, x: f32, y: f32, scale: f32) -> Hit {
        if y >= height(scale) {
            return Hit::None;
        }
        if let Some(i) = self.buttons.iter().position(|b| b.rect.contains(x, y)) {
            return Hit::Button(i);
        }
        if self.icon_rect.inset(-4.0 * scale).contains(x, y) {
            return Hit::Icon;
        }
        Hit::Caption
    }

    /// Update hover; returns true when a redraw is needed.
    pub fn set_hover(&mut self, b: Option<usize>) -> bool {
        let changed = self.hover != b;
        self.hover = b;
        if b.is_none() {
            self.pressed = None;
        }
        changed
    }

    pub fn set_pressed(&mut self, b: Option<usize>) -> bool {
        let changed = self.pressed != b;
        self.pressed = b;
        changed
    }

    pub fn tooltip(&self) -> Option<&str> {
        self.hover.and_then(|i| self.buttons.get(i)).map(|b| b.tip.as_str())
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hover
    }
}
