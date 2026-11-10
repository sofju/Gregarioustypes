//! Hex dump for binary files without a better preview.

use crate::cx::{DrawCx, EventCx, Font, Fonts};
use crate::geom::Rect;
use crate::scroll::Scroll;
use crate::view::{Event, View};

const FONT_DIP: f32 = 12.5;

pub struct HexView {
    data: Vec<u8>,
    total: u64,
    scroll: Scroll,
    metrics: Option<(f32, f32, f32)>,
}

impl HexView {
    pub fn new(data: Vec<u8>, total: u64) -> Self {
        Self { data, total, scroll: Scroll::default(), metrics: None }
    }

    fn metrics(&mut self, fonts: &mut Fonts, scale: f32) -> (f32, f32) {
        if let Some((s, cw, lh)) = self.metrics {
            if s == scale {
                return (cw, lh);
            }
        }
        let (w, _) = fonts.measure("0000000000", Font::Mono(FONT_DIP), scale, 100_000.0, false);
        let m = (w / 10.0, (FONT_DIP * 1.5 * scale).round());
        self.metrics = Some((scale, m.0, m.1));
        m
    }

    fn rows(&self) -> usize {
        self.data.len().div_ceil(16) + if (self.data.len() as u64) < self.total { 2 } else { 0 }
    }
}

impl View for HexView {
    fn kind(&self) -> &'static str {
        "hex"
    }

    fn preferred_size(&mut self, fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let (cw, lh) = self.metrics(fonts, scale);
        ((78.0 * cw + 40.0 * scale).min(max_w), ((self.rows() as f32 + 1.0) * lh + 24.0 * scale).clamp(200.0 * scale, max_h * 0.8))
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let (cw, lh) = self.metrics(cx.fonts, cx.scale);
        let a = r.inset_xy(cx.px(14.0), cx.px(8.0));
        self.scroll.set_extent(a.w, self.rows() as f32 * lh, a.w, a.h);
        cx.clip(r);
        let first = (self.scroll.y / lh).floor() as usize;
        let n = (a.h / lh).ceil() as usize + 1;
        let full_rows = self.data.len().div_ceil(16);
        for row in first..(first + n).min(self.rows()) {
            let y = a.y + row as f32 * lh - self.scroll.y;
            if row >= full_rows {
                if row == full_rows + 1 {
                    let msg = format!("… {} more bytes not shown", gt_core::fmt::group(self.total - self.data.len() as u64));
                    cx.text_at(&msg, a.x, y, a.w, lh, Font::Ui(12.0), cx.theme.text3);
                }
                continue;
            }
            let off = row * 16;
            let chunk = &self.data[off..(off + 16).min(self.data.len())];
            let mut hexs = String::with_capacity(50);
            for (i, b) in chunk.iter().enumerate() {
                if i == 8 {
                    hexs.push(' ');
                }
                hexs.push_str(&format!("{b:02X} "));
            }
            let ascii: String = chunk.iter().map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '·' }).collect();
            cx.text_at(&format!("{off:08X}"), a.x, y, cw * 9.0, lh, Font::Mono(FONT_DIP), cx.theme.text3);
            cx.text_at(&hexs, a.x + cw * 10.0, y, cw * 50.0, lh, Font::Mono(FONT_DIP), cx.theme.text);
            cx.text_at(&ascii, a.x + cw * 60.0, y, cw * 17.0, lh, Font::Mono(FONT_DIP), cx.theme.text2);
        }
        cx.unclip();
        self.scroll.draw(cx, r);
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        let (_, lh) = self.metrics(cx.fonts, cx.scale);
        if !self.scroll.event(cx, r, ev, lh) {
            if let Event::Key { vk, .. } = *ev {
                cx.redraw |= self.scroll.key(vk, lh);
            }
        }
    }
}
