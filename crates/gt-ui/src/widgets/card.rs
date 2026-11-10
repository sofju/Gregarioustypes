//! Generic info card: big icon, name, kind and facts. Fallback for any file.

use crate::cx::{Align, DrawCx, Font, Fonts};
use crate::geom::Rect;
use crate::view::View;
use crate::widgets::bitmap_from_bgra;
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, ID2D1DeviceContext};

pub struct CardData {
    /// Premultiplied BGRA icon/thumbnail.
    pub icon: Option<(u32, u32, Vec<u8>)>,
    pub name: String,
    pub kind: String,
    pub facts: Vec<(String, String)>,
    pub note: Option<String>,
}

pub struct InfoCard {
    data: CardData,
    icon: Option<ID2D1Bitmap1>,
}

impl InfoCard {
    pub fn new(dc: &ID2D1DeviceContext, data: CardData) -> Self {
        let icon = data.icon.as_ref().and_then(|(w, h, px)| bitmap_from_bgra(dc, *w, *h, px));
        Self { data, icon }
    }
}

impl View for InfoCard {
    fn kind(&self) -> &'static str {
        "card"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let rows = self.data.facts.len() as f32 + if self.data.note.is_some() { 2.0 } else { 0.0 };
        let h = (110.0 + rows * 24.0).max(220.0) * scale;
        ((640.0 * scale).min(max_w), h.min(max_h))
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let s = cx.scale;
        let pad = cx.px(28.0);
        let icon_box = cx.px(148.0);
        let ir = Rect::new(r.x + pad, r.y + pad, icon_box, icon_box);
        if let Some(icon) = &self.icon {
            let sz = unsafe { icon.GetPixelSize() };
            let k = (icon_box / sz.width.max(1) as f32).min(icon_box / sz.height.max(1) as f32).min(4.0);
            let (w, h) = (sz.width as f32 * k, sz.height as f32 * k);
            cx.bitmap(icon, Rect::new(ir.x + (icon_box - w) / 2.0, ir.y + (icon_box - h) / 2.0, w, h).snap(), None, 1.0, false);
        } else {
            cx.glyph(crate::glyph::FILE, ir, 96.0, cx.theme.text3);
        }
        let x = ir.right() + pad;
        let w = (r.right() - pad - x).max(10.0);
        let mut y = r.y + pad;
        let nh = cx.text_wrapped(&self.data.name, Rect::new(x, y, w, cx.px(60.0)), Font::Bold(20.0), cx.theme.text);
        y += nh + cx.px(4.0);
        cx.text(&self.data.kind, Rect::new(x, y, w, cx.px(20.0)), Font::Ui(13.0), cx.theme.text2, Align::Left);
        y += cx.px(30.0);
        let key_w = cx.px(110.0);
        for (k, v) in &self.data.facts {
            cx.text(k, Rect::new(x, y, key_w, cx.px(22.0)), Font::Ui(12.5), cx.theme.text2, Align::Left);
            cx.text(v, Rect::new(x + key_w, y, w - key_w, cx.px(22.0)), Font::Ui(12.5), cx.theme.text, Align::Left);
            y += cx.px(24.0);
        }
        if let Some(note) = &self.data.note {
            y += cx.px(8.0);
            cx.text_wrapped(note, Rect::new(x, y, w, r.bottom() - y), Font::Ui(12.0), cx.theme.text3);
        }
        let _ = s;
    }

    fn info(&self) -> Vec<(String, String)> {
        self.data.facts.clone()
    }
}
