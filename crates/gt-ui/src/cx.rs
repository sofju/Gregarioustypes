//! Drawing and event contexts handed to views.

use crate::geom::Rect;
use crate::theme::{Color, Theme};
use std::collections::HashMap;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1DeviceContext, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
};
use windows_numerics::Vector2;

/// Font roles; sizes are in DIPs and scaled by the context.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Font {
    Ui(f32),
    Semibold(f32),
    Bold(f32),
    Mono(f32),
    Icon(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Cache of DirectWrite text formats keyed by role and pixel size.
pub struct Fonts {
    dwrite: IDWriteFactory,
    cache: HashMap<(u8, u32, bool), IDWriteTextFormat>,
}

impl Fonts {
    pub fn new(dwrite: IDWriteFactory) -> Self {
        Self { dwrite, cache: HashMap::new() }
    }

    pub fn dwrite(&self) -> &IDWriteFactory {
        &self.dwrite
    }

    fn spec(font: Font) -> (u8, PCWSTR, DWRITE_FONT_WEIGHT, f32) {
        match font {
            Font::Ui(s) => (0, w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_NORMAL, s),
            Font::Semibold(s) => (1, w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_SEMI_BOLD, s),
            Font::Bold(s) => (2, w!("Segoe UI Variable Display"), DWRITE_FONT_WEIGHT_BOLD, s),
            Font::Mono(s) => (3, w!("Cascadia Mono"), DWRITE_FONT_WEIGHT_NORMAL, s),
            Font::Icon(s) => (4, w!("Segoe Fluent Icons"), DWRITE_FONT_WEIGHT_NORMAL, s),
        }
    }

    /// Text format at `scale`; `wrap` selects word wrapping vs. single line with ellipsis.
    pub fn get(&mut self, font: Font, scale: f32, wrap: bool) -> IDWriteTextFormat {
        let (kind, family, weight, size) = Self::spec(font);
        let px = (size * scale * 10.0).round() as u32;
        if let Some(f) = self.cache.get(&(kind, px, wrap)) {
            return f.clone();
        }
        let f = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, px as f32 / 10.0, w!(""))
                .expect("CreateTextFormat");
            // Monospace text uses a fixed line pitch so fallback fonts (emoji, CJK)
            // never change the row height.
            if kind == 3 {
                let lh = (size * scale * 1.45).round();
                let _ = f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, lh, (lh * 0.78).round());
            }
            if wrap {
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            } else {
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&f) {
                    let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                    let _ = f.SetTrimming(&trim, &sign);
                }
            }
            f
        };
        self.cache.insert((kind, px, wrap), f.clone());
        f
    }

    pub fn layout(&mut self, text: &str, font: Font, scale: f32, max_w: f32, max_h: f32, wrap: bool) -> Option<IDWriteTextLayout> {
        let fmt = self.get(font, scale, wrap);
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&wide, &fmt, max_w.max(1.0), max_h.max(1.0)).ok() }
    }

    /// Width and height of `text` laid out within `max_w`.
    pub fn measure(&mut self, text: &str, font: Font, scale: f32, max_w: f32, wrap: bool) -> (f32, f32) {
        let Some(l) = self.layout(text, font, scale, max_w, 100_000.0, wrap) else { return (0.0, 0.0) };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
        }
        (m.widthIncludingTrailingWhitespace, m.height)
    }
}

/// A run of characters drawn differently by `DrawCx::text_runs`.
#[derive(Clone, Copy, Debug)]
pub struct TextRun {
    /// First character (not byte) and number of characters.
    pub start: usize,
    pub len: usize,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

pub struct DrawCx<'a> {
    pub dc: &'a ID2D1DeviceContext,
    pub brush: &'a ID2D1SolidColorBrush,
    pub fonts: &'a mut Fonts,
    pub theme: &'a Theme,
    /// Physical pixels per DIP.
    pub scale: f32,
    /// True when drawing on the FP16 scRGB layer (colours are linear, 1.0 = 80 nits).
    pub hdr_layer: bool,
    /// scRGB value of SDR white on the current display (1.0 when HDR is off).
    pub sdr_scale: f32,
    /// Display peak divided by SDR white (1.0 when HDR is off).
    pub headroom: f32,
}

impl<'a> DrawCx<'a> {
    pub fn px(&self, dip: f32) -> f32 {
        (dip * self.scale).round()
    }

    fn set(&self, c: Color) {
        unsafe { self.brush.SetColor(&c as *const D2D1_COLOR_F) };
    }

    pub fn fill(&self, r: Rect, c: Color) {
        self.set(c);
        unsafe {
            let _ = self.dc.FillRectangle(&r.d2d(), self.brush);
        }
    }

    pub fn round(&self, r: Rect, radius: f32, c: Color) {
        self.set(c);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.dc.FillRoundedRectangle(&rr, self.brush) };
    }

    pub fn stroke(&self, r: Rect, c: Color, width: f32) {
        self.set(c);
        unsafe {
            let _ = self.dc.DrawRectangle(&r.d2d(), self.brush, width, None);
        }
    }

    pub fn hline(&self, x0: f32, x1: f32, y: f32, c: Color) {
        self.fill(Rect::new(x0, y, x1 - x0, 1.0_f32.max((self.scale).floor())), c);
    }

    pub fn vline(&self, x: f32, y0: f32, y1: f32, c: Color) {
        self.fill(Rect::new(x, y0, 1.0_f32.max((self.scale).floor()), y1 - y0), c);
    }

    /// Draw a single line of text, vertically centred in `r`, trimmed with an ellipsis.
    pub fn text(&mut self, s: &str, r: Rect, font: Font, c: Color, align: Align) {
        if s.is_empty() || r.w <= 0.0 {
            return;
        }
        let Some(layout) = self.fonts.layout(s, font, self.scale, r.w, r.h, false) else { return };
        unsafe {
            let _ = layout.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        }
        self.set(c);
        unsafe {
            self.dc.DrawTextLayout(Vector2 { X: r.x, Y: r.y }, &layout, self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
    }

    /// Draw wrapped text from the top of `r`; returns the height used.
    pub fn text_wrapped(&mut self, s: &str, r: Rect, font: Font, c: Color) -> f32 {
        let Some(layout) = self.fonts.layout(s, font, self.scale, r.w, r.h.max(1.0), true) else { return 0.0 };
        unsafe {
            let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        }
        self.set(c);
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            self.dc.DrawTextLayout(Vector2 { X: r.x, Y: r.y }, &layout, self.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            let _ = layout.GetMetrics(&mut m);
        }
        m.height
    }

    /// Draw text at an exact origin without trimming (used by monospace views).
    pub fn text_at(&mut self, s: &str, x: f32, y: f32, max_w: f32, line_h: f32, font: Font, c: Color) {
        if s.is_empty() {
            return;
        }
        let Some(layout) = self.fonts.layout(s, font, self.scale, max_w, line_h, false) else { return };
        self.set(c);
        unsafe {
            self.dc.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, self.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
    }

    /// Like `text_at`, with parts in other colours (and bold/italic): one layout
    /// for the whole line, each run drawn with its own brush.
    pub fn text_runs(&mut self, s: &str, x: f32, y: f32, max_w: f32, line_h: f32, font: Font, c: Color, runs: &[TextRun]) {
        if s.is_empty() {
            return;
        }
        if runs.is_empty() {
            return self.text_at(s, x, y, max_w, line_h, font, c);
        }
        let Some(layout) = self.fonts.layout(s, font, self.scale, max_w, line_h, false) else { return };
        // Runs count characters; DirectWrite counts UTF-16 units.
        let utf16: Option<Vec<u32>> = (!s.is_ascii()).then(|| {
            let mut v = Vec::with_capacity(s.len() + 1);
            let mut n = 0u32;
            for ch in s.chars() {
                v.push(n);
                n += ch.len_utf16() as u32;
            }
            v.push(n);
            v
        });
        let at = |i: usize| -> u32 {
            match &utf16 {
                Some(v) => v[i.min(v.len() - 1)],
                None => i.min(s.len()) as u32,
            }
        };
        let mut brushes: HashMap<u32, windows::core::IUnknown> = HashMap::new();
        unsafe {
            for r in runs {
                let (a, b) = (at(r.start), at(r.start + r.len));
                if b <= a {
                    continue;
                }
                let range = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE { startPosition: a, length: b - a };
                if r.color != c {
                    let key = ((r.color.r * 255.0) as u32) << 16 | ((r.color.g * 255.0) as u32) << 8 | (r.color.b * 255.0) as u32;
                    let brush = match brushes.get(&key) {
                        Some(b) => Some(b.clone()),
                        None => self.dc.CreateSolidColorBrush(&r.color, None).ok().and_then(|b| windows::core::Interface::cast::<windows::core::IUnknown>(&b).ok()).inspect(|b| {
                            brushes.insert(key, b.clone());
                        }),
                    };
                    if let Some(b) = brush {
                        let _ = layout.SetDrawingEffect(&b, range);
                    }
                }
                if r.bold {
                    let _ = layout.SetFontWeight(windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_BOLD, range);
                }
                if r.italic {
                    let _ = layout.SetFontStyle(windows::Win32::Graphics::DirectWrite::DWRITE_FONT_STYLE_ITALIC, range);
                }
            }
            self.set(c);
            self.dc.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, self.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
    }

    pub fn glyph(&mut self, g: char, r: Rect, size_dip: f32, c: Color) {
        let mut buf = [0u8; 4];
        self.text(g.encode_utf8(&mut buf), r, Font::Icon(size_dip), c, Align::Center);
    }

    pub fn measure(&mut self, s: &str, font: Font) -> (f32, f32) {
        self.fonts.measure(s, font, self.scale, 100_000.0, false)
    }

    pub fn bitmap(&self, bmp: &ID2D1Bitmap1, dst: Rect, src: Option<Rect>, opacity: f32, nearest: bool) {
        let mode = if nearest { D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR } else { D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC };
        let d = dst.d2d();
        let s = src.map(|s| s.d2d());
        unsafe {
            self.dc.DrawBitmap(bmp, Some(&d), opacity, mode, s.as_ref().map(|s| s as *const _), None);
        }
    }

    pub fn clip(&self, r: Rect) {
        unsafe {
            let _ = self.dc.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
    }

    pub fn unclip(&self) {
        unsafe {
            let _ = self.dc.PopAxisAlignedClip();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Arrow,
    Hand,
    IBeam,
}

/// Context for input handling: views set flags the window acts upon.
pub struct EventCx<'a> {
    pub hwnd: HWND,
    pub scale: f32,
    pub fonts: &'a mut Fonts,
    pub redraw: bool,
    pub capture: Option<bool>,
    pub cursor: Cursor,
    pub toast: Option<String>,
    /// Text to put on the clipboard.
    pub copy: Option<String>,
    /// Ask the window to re-run layout (preferred size changed).
    pub relayout: bool,
    /// Ask the window to reload the current item with another provider (by id).
    pub switch_provider: Option<&'static str>,
    /// Ask the window to toggle full screen.
    pub fullscreen: bool,
}

impl<'a> EventCx<'a> {
    pub fn new(hwnd: HWND, scale: f32, fonts: &'a mut Fonts) -> Self {
        Self { hwnd, scale, fonts, redraw: false, capture: None, cursor: Cursor::Arrow, toast: None, copy: None, relayout: false, switch_provider: None, fullscreen: false }
    }
    pub fn px(&self, dip: f32) -> f32 {
        (dip * self.scale).round()
    }
}
