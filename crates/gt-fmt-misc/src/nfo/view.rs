//! The DOS art view: integer-scaled, nearest-neighbour VGA rendering with themes,
//! encoding and letter-spacing toggles, SAUCE info, selection and Unicode copy.

use super::grid::Grid;
use super::raster::{self, Region, VgaFont, VGA_PALETTE};
use super::sauce::Sauce;
use super::{save_override, to_cp437, NfoData};
use gt_core::encoding::{Encoding, CP437};
use gt_core::settings::Settings;
use gt_ui::cx::{Cursor, Fonts};
use gt_ui::scroll::Scroll;
use gt_ui::widgets::{bitmap_from_bgra, update_bitmap};
use gt_ui::{glyph, Action, DrawCx, Event, EventCx, MouseButton, Rect, View};
use std::path::PathBuf;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_0, VK_A, VK_ADD, VK_OEM_MINUS, VK_OEM_PLUS, VK_SUBTRACT};

/// (id, label, foreground BGRA, background BGRA)
const THEMES: [(&str, &str, [u8; 4], [u8; 4]); 5] = [
    ("dos", "DOS", [0xAA, 0xAA, 0xAA, 0xFF], [0x00, 0x00, 0x00, 0xFF]),
    ("white", "White", [0xFF, 0xFF, 0xFF, 0xFF], [0x00, 0x00, 0x00, 0xFF]),
    ("amber", "Amber", [0x00, 0xB0, 0xFF, 0xFF], [0x00, 0x00, 0x00, 0xFF]),
    ("green", "Green", [0x33, 0xFF, 0x33, 0xFF], [0x00, 0x00, 0x00, 0xFF]),
    ("light", "Light", [0x1E, 0x1E, 0x1E, 0xFF], [0xF3, 0xF3, 0xF3, 0xFF]),
];

#[derive(Clone, Copy, Default)]
struct Geom {
    s: f32,
    ar: f32,
    x0: f32,
    y0: f32,
}

pub struct DosArtView {
    path: PathBuf,
    raw: Vec<u8>,
    sauce: Option<Sauce>,
    encoding: Encoding,
    ansi: bool,
    grid: Grid,
    font: &'static VgaFont,
    cell_w: usize,
    theme: usize,
    zoom: Option<u32>,
    fixed_scale: u32,
    aspect: bool,
    scroll: Scroll,
    bmp: Option<(ID2D1Bitmap1, u32, u32)>,
    buf: Vec<u8>,
    anchor: Option<(usize, usize)>,
    caret: Option<(usize, usize)>,
    dragging: bool,
    geom: Geom,
}

impl DosArtView {
    pub fn new(d: NfoData, settings: &Settings) -> Self {
        let font = raster::font_for(d.sauce.as_ref().map(|s| s.font.as_str()));
        let cell_w = match settings.nfo.letter_spacing {
            8 | 9 => settings.nfo.letter_spacing as usize,
            _ => d.sauce.as_ref().and_then(|s| s.letter_spacing()).unwrap_or(8) as usize,
        };
        let theme = THEMES.iter().position(|t| t.0 == settings.nfo.theme).unwrap_or(0);
        let aspect = settings.nfo.aspect_correction && d.sauce.as_ref().is_some_and(|s| s.stretch());
        let mut v = Self {
            path: d.path,
            raw: d.raw,
            sauce: d.sauce,
            encoding: d.encoding,
            ansi: d.ansi,
            grid: Grid::default(),
            font,
            cell_w,
            theme,
            zoom: None,
            fixed_scale: settings.nfo.scale,
            aspect,
            scroll: Scroll::default(),
            bmp: None,
            buf: Vec::new(),
            anchor: None,
            caret: None,
            dragging: false,
            geom: Geom::default(),
        };
        v.rebuild();
        v
    }

    fn rebuild(&mut self) {
        let (bytes, _) = to_cp437(&self.raw, self.encoding);
        self.grid = if self.ansi {
            let width = self.sauce.as_ref().map(|s| s.tinfo1 as usize).filter(|&w| w > 0).unwrap_or(80);
            Grid::ansi(&bytes, width, self.sauce.as_ref().is_some_and(|s| s.ice_colors()))
        } else {
            Grid::plain(&bytes)
        };
        self.anchor = None;
        self.caret = None;
    }

    fn palette(&self) -> [[u8; 4]; 16] {
        let mut p = VGA_PALETTE;
        if !self.grid.colored {
            p[7] = THEMES[self.theme].2;
            p[0] = THEMES[self.theme].3;
        }
        p
    }

    fn bg(&self) -> gt_ui::Color {
        let c = self.palette()[0];
        gt_ui::rgba(c[2] as f32 / 255.0, c[1] as f32 / 255.0, c[0] as f32 / 255.0, 1.0)
    }

    fn aspect_factor(&self) -> f32 {
        if self.aspect {
            if self.cell_w == 9 {
                1.35
            } else {
                1.2
            }
        } else {
            1.0
        }
    }

    fn content_px(&self) -> (usize, usize) {
        (self.grid.cols.max(1) * self.cell_w, self.grid.rows.len().max(1) * self.font.height)
    }

    /// Largest integer scale that fits, starting from one VGA pixel per DIP.
    fn auto_scale(&self, dpi_scale: f32, avail_w: f32) -> f32 {
        if let Some(z) = self.zoom {
            return z as f32;
        }
        let mut s = if self.fixed_scale > 0 { self.fixed_scale } else { dpi_scale.round().max(1.0) as u32 };
        let (w, _) = self.content_px();
        while s > 1 && (w as f32 * s as f32) > avail_w {
            s -= 1;
        }
        s as f32
    }

    fn pad(scale: f32) -> f32 {
        (16.0 * scale).round()
    }

    fn cell_at(&self, x: f32, y: f32) -> (usize, usize) {
        let g = self.geom;
        let vx = ((x - g.x0 + self.scroll.x.round()) / g.s).max(0.0) as usize;
        let vy = ((y - g.y0 + self.scroll.y.round()) / (g.s * g.ar)).max(0.0) as usize;
        let row = (vy / self.font.height).min(self.grid.rows.len().saturating_sub(1));
        let col = (vx / self.cell_w).min(self.grid.cols);
        (row, col)
    }

    fn selection(&self) -> Option<raster::Selection> {
        let (a, b) = (self.anchor?, self.caret?);
        if a == b {
            return None;
        }
        let (s, e) = if a < b { (a, b) } else { (b, a) };
        Some((s, (e.0, e.1 + 1)))
    }

    fn text_of(&self, sel: Option<raster::Selection>) -> String {
        let mut out = String::new();
        for (r, line) in self.grid.rows.iter().enumerate() {
            let (c0, c1) = match sel {
                Some(((r0, c0), (r1, c1))) => {
                    if r < r0 || r > r1 {
                        continue;
                    }
                    (if r == r0 { c0 } else { 0 }, if r == r1 { c1 } else { usize::MAX })
                }
                None => (0, usize::MAX),
            };
            let s: String = line.iter().skip(c0).take(c1.saturating_sub(c0)).map(|c| if c.ch == 0 { ' ' } else { CP437[c.ch as usize] }).collect();
            out.push_str(s.trim_end());
            out.push_str("\r\n");
        }
        out.trim_end_matches("\r\n").to_string()
    }

    fn short_encoding(&self) -> &'static str {
        match self.encoding {
            Encoding::Cp437 => "CP437",
            Encoding::Utf8 => "UTF-8",
            Encoding::Windows1252 => "1252",
            Encoding::Utf16Le => "UTF-16",
            Encoding::Utf16Be => "UTF-16BE",
        }
    }
}

impl View for DosArtView {
    fn kind(&self) -> &'static str {
        "nfo"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let pad = Self::pad(scale);
        let s = self.auto_scale(scale, max_w - pad * 2.0);
        let (w, h) = self.content_px();
        let cw = w as f32 * s + pad * 2.0 + (10.0 * scale).round();
        let ch = h as f32 * s * self.aspect_factor() + pad * 2.0;
        (cw.max(520.0 * scale).min(max_w), ch.clamp(240.0 * scale, max_h))
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        cx.fill(r, self.bg());
        let pad = Self::pad(cx.scale);
        let s = self.auto_scale(cx.scale, r.w - pad * 2.0);
        let ar = self.aspect_factor();
        let (cw, ch) = self.content_px();
        let (dw, dh) = (cw as f32 * s, ch as f32 * s * ar);
        let x0 = (r.x + ((r.w - dw) / 2.0).max(pad)).round();
        let y0 = (r.y + pad).round();
        self.geom = Geom { s, ar, x0, y0 };
        self.scroll.set_extent(dw + 2.0 * ((x0 - r.x).max(pad)), dh + pad * 2.0, r.w, r.h);
        let (sx, sy) = (self.scroll.x.round(), self.scroll.y.round());

        // Visible region in VGA pixels.
        let vx = ((sx - (x0 - r.x)).max(0.0) / s).floor() as usize;
        let vy = ((sy - pad).max(0.0) / (s * ar)).floor() as usize;
        let vw = ((r.w / s).ceil() as usize + 2).min(cw.saturating_sub(vx));
        let vh = ((r.h / (s * ar)).ceil() as usize + 2).min(ch.saturating_sub(vy));
        if vw == 0 || vh == 0 {
            self.scroll.draw(cx, r);
            return;
        }
        let region = Region { x: vx, y: vy, w: vw, h: vh };
        let sel = self.selection();
        let palette = self.palette();
        raster::render(&self.grid, self.font, self.cell_w, &palette, &region, sel, &mut self.buf);
        let reuse = matches!(&self.bmp, Some((_, w, h)) if *w == vw as u32 && *h == vh as u32);
        if reuse {
            if let Some((b, w, _)) = &self.bmp {
                update_bitmap(b, *w, &self.buf);
            }
        } else {
            self.bmp = bitmap_from_bgra(cx.dc, vw as u32, vh as u32, &self.buf).map(|b| (b, vw as u32, vh as u32));
        }
        if let Some((b, _, _)) = &self.bmp {
            let dx = x0 - sx + vx as f32 * s;
            let dy = y0 - sy + vy as f32 * s * ar;
            cx.clip(r);
            cx.bitmap(b, Rect::new(dx, dy, vw as f32 * s, (vh as f32 * s * ar).round()), None, 1.0, true);
            cx.unclip();
        }
        self.scroll.draw(cx, r);
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        let line = (self.font.height as f32 * self.geom.s.max(1.0) * self.geom.ar.max(1.0)).round();
        match *ev {
            Event::Wheel { lines, ctrl: true, .. } => {
                let cur = self.auto_scale(cx.scale, r.w) as i32;
                let next = (cur + if lines > 0.0 { 1 } else { -1 }).clamp(1, 8) as u32;
                self.zoom = Some(next);
                cx.redraw = true;
                cx.toast = Some(format!("{next}×"));
                return;
            }
            _ => {}
        }
        if self.scroll.event(cx, r, ev, line) {
            return;
        }
        match *ev {
            Event::MouseDown { x, y, button: MouseButton::Left, shift, double } => {
                let c = self.cell_at(x, y);
                if double {
                    let n = self.grid.rows.get(c.0).map(|l| l.len()).unwrap_or(0);
                    self.anchor = Some((c.0, 0));
                    self.caret = Some((c.0, n.saturating_sub(1)));
                } else {
                    if !shift || self.anchor.is_none() {
                        self.anchor = Some(c);
                    }
                    self.caret = Some(c);
                    self.dragging = true;
                    cx.capture = Some(true);
                }
                cx.redraw = true;
            }
            Event::MouseMove { x, y } => {
                cx.cursor = Cursor::IBeam;
                if self.dragging {
                    if y > r.bottom() {
                        self.scroll.scroll_by(0.0, line);
                    } else if y < r.y {
                        self.scroll.scroll_by(0.0, -line);
                    }
                    self.caret = Some(self.cell_at(x, y));
                    cx.redraw = true;
                }
            }
            Event::MouseUp { .. } => {
                if self.dragging {
                    self.dragging = false;
                    cx.capture = Some(false);
                }
            }
            Event::Key { vk, ctrl: true, .. } => {
                if vk == VK_A.0 {
                    self.anchor = Some((0, 0));
                    let last = self.grid.rows.len().saturating_sub(1);
                    self.caret = Some((last, self.grid.cols));
                    cx.redraw = true;
                } else if vk == VK_OEM_PLUS.0 || vk == VK_ADD.0 || vk == VK_OEM_MINUS.0 || vk == VK_SUBTRACT.0 {
                    let up = vk == VK_OEM_PLUS.0 || vk == VK_ADD.0;
                    let cur = self.auto_scale(cx.scale, r.w) as i32;
                    self.zoom = Some((cur + if up { 1 } else { -1 }).clamp(1, 8) as u32);
                    cx.redraw = true;
                } else if vk == VK_0.0 {
                    self.zoom = None;
                    cx.redraw = true;
                }
            }
            Event::Key { vk, .. } => cx.redraw |= self.scroll.key(vk, line),
            _ => {}
        }
    }

    fn actions(&self) -> Vec<Action> {
        let mut a = vec![
            Action::new("enc", glyph::FONT, "Text encoding (click to change)").label(self.short_encoding()),
            Action::new("spacing", glyph::CODE, "Letter spacing: 8 or 9 pixel cells").label(format!("{} px", self.cell_w)),
        ];
        if !self.grid.colored {
            a.push(Action::new("theme", glyph::COLOR, format!("Colours: {}", THEMES[self.theme].1)));
        }
        if self.sauce.as_ref().is_some_and(|s| s.stretch()) {
            a.push(Action::new("aspect", glyph::ZOOM_IN, "Legacy aspect ratio (SAUCE)").active(self.aspect));
        }
        a.push(Action::new("text", glyph::DOC, "Show as plain text"));
        a
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        match id {
            "enc" => {
                let order = [Encoding::Cp437, Encoding::Utf8, Encoding::Windows1252];
                let i = order.iter().position(|e| *e == self.encoding).map(|i| (i + 1) % order.len()).unwrap_or(0);
                self.encoding = order[i];
                save_override(&self.path, self.encoding);
                self.rebuild();
                cx.toast = Some(format!("Encoding: {}", self.encoding.label()));
                cx.relayout = true;
            }
            "spacing" => {
                self.cell_w = if self.cell_w == 8 { 9 } else { 8 };
                cx.toast = Some(format!("{}-pixel cells", self.cell_w));
                cx.relayout = true;
            }
            "theme" => {
                self.theme = (self.theme + 1) % THEMES.len();
                let mut s = Settings::load();
                s.nfo.theme = THEMES[self.theme].0.into();
                s.save();
                cx.toast = Some(format!("Colours: {}", THEMES[self.theme].1));
            }
            "aspect" => {
                self.aspect = !self.aspect;
                cx.relayout = true;
            }
            "text" => cx.switch_provider = Some("text"),
            _ => {}
        }
        cx.redraw = true;
    }

    fn copy_text(&self) -> Option<String> {
        Some(self.text_of(self.selection()))
    }

    fn subtitle(&self) -> Option<String> {
        Some(format!("{} · {}×{} · {}", self.short_encoding(), self.grid.cols, self.grid.rows.len(), self.font.name))
    }

    fn wants_focus(&self) -> bool {
        true
    }

    fn info(&self) -> Vec<(String, String)> {
        let mut v = vec![
            ("Encoding".into(), self.encoding.label().into()),
            ("Size".into(), format!("{} columns × {} lines", self.grid.cols, self.grid.rows.len())),
            ("Font".into(), format!("{} ({}-pixel cells)", self.font.name, self.cell_w)),
            ("Colours".into(), if self.grid.colored { "ANSI 16-colour".into() } else { THEMES[self.theme].1.to_string() }),
        ];
        if let Some(s) = &self.sauce {
            for (k, val) in [("Title", &s.title), ("Author", &s.author), ("Group", &s.group)] {
                if !val.is_empty() {
                    v.push((k.into(), val.clone()));
                }
            }
            if !s.date.trim().is_empty() {
                v.push(("Date".into(), s.date_pretty()));
            }
            v.push(("SAUCE type".into(), s.type_name().into()));
            if !s.font.is_empty() {
                v.push(("SAUCE font".into(), s.font.clone()));
            }
            if s.ice_colors() {
                v.push(("iCE colours".into(), "Yes".into()));
            }
            if s.stretch() {
                v.push(("Aspect".into(), "Legacy (stretched)".into()));
            }
            for (i, c) in s.comments.iter().enumerate() {
                v.push((if i == 0 { "Comments".into() } else { String::new() }, c.clone()));
            }
        }
        v
    }
}
