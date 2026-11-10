//! Monospace text view with wrapping, selection and copy, optional syntax
//! colours (filled in by a highlighter thread while the text is shown) and
//! line numbers.

use crate::cx::{Cursor, DrawCx, EventCx, Font, Fonts, TextRun};
use crate::theme::Color;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use crate::geom::Rect;
use crate::glyph;
use crate::scroll::Scroll;
use crate::view::{Action, Event, MouseButton, View};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_A;

const FONT_DIP: f32 = 13.0;

/// Colour of part of a line: columns of the tab-expanded line, 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub len: u32,
    pub rgb: u32,
    pub bold: bool,
    pub italic: bool,
}

/// Syntax colours of a text, line by line, written by a highlighter thread and
/// read by the view as they arrive.
pub struct Highlight {
    pub language: String,
    lines: Mutex<Vec<Vec<Span>>>,
    done: AtomicBool,
    /// Lines past this limit stay plain.
    pub limit: Option<usize>,
}

impl Highlight {
    pub fn new(language: impl Into<String>, limit: Option<usize>) -> Arc<Self> {
        Arc::new(Highlight { language: language.into(), lines: Mutex::new(Vec::new()), done: AtomicBool::new(false), limit })
    }
    pub fn extend(&self, lines: impl IntoIterator<Item = Vec<Span>>) {
        if let Ok(mut l) = self.lines.lock() {
            l.extend(lines);
        }
    }
    pub fn finish(&self) {
        self.done.store(true, Ordering::Release);
    }
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
    pub fn ready(&self) -> usize {
        self.lines.lock().map(|l| l.len()).unwrap_or(0)
    }
    /// The colours computed so far.
    pub fn snapshot(&self) -> Vec<Vec<Span>> {
        self.lines.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

fn rgb(c: u32) -> Color {
    crate::theme::hex(c, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
struct Pos {
    line: usize,
    ch: usize,
}

pub struct TextView {
    lines: Vec<String>,
    line_chars: Vec<usize>,
    wrap: bool,
    /// (line, first char, char count) for each visual row.
    rows: Vec<(usize, usize, usize)>,
    rows_cols: usize,
    scroll: Scroll,
    metrics: Option<(f32, f32, f32)>, // (scale, char width, line height)
    anchor: Option<Pos>,
    caret: Option<Pos>,
    dragging: bool,
    note: Option<String>,
    subtitle: Option<String>,
    max_cols: usize,
    /// Provider showing the rendered form (Markdown, HTML): a button switches back.
    rendered: Option<&'static str>,
    highlight: Option<Arc<Highlight>>,
    /// Highlighted lines when last drawn (redraw as more arrive).
    drawn_ready: usize,
    numbers: bool,
}

fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let n = 4 - (col % 4);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

impl TextView {
    pub fn new(text: &str, wrap: bool, subtitle: Option<String>, note: Option<String>) -> Self {
        let lines: Vec<String> = text.split('\n').map(|l| expand_tabs(l.strip_suffix('\r').unwrap_or(l))).collect();
        let line_chars: Vec<usize> = lines.iter().map(|l| l.chars().count()).collect();
        let max_cols = line_chars.iter().copied().max().unwrap_or(0);
        Self {
            lines,
            line_chars,
            wrap,
            rows: Vec::new(),
            rows_cols: 0,
            scroll: Scroll::default(),
            metrics: None,
            anchor: None,
            caret: None,
            dragging: false,
            note,
            subtitle,
            max_cols,
            rendered: None,
            highlight: None,
            drawn_ready: 0,
            numbers: false,
        }
    }

    /// Syntax colours, arriving progressively.
    pub fn with_highlight(mut self, h: Option<Arc<Highlight>>) -> Self {
        self.highlight = h;
        self
    }

    pub fn with_line_numbers(mut self, on: bool) -> Self {
        self.numbers = on;
        self
    }

    /// Width of the line-number column.
    fn gutter(&self, cw: f32, scale: f32) -> f32 {
        if !self.numbers {
            return 0.0;
        }
        let digits = self.lines.len().max(1).ilog10() as f32 + 1.0;
        (digits.max(3.0) * cw + 14.0 * scale).round()
    }

    /// Offer a button that switches to the rendered view (provider id).
    pub fn with_rendered(mut self, provider: &'static str) -> Self {
        self.rendered = Some(provider);
        self
    }

    fn metrics(&mut self, fonts: &mut Fonts, scale: f32) -> (f32, f32) {
        if let Some((s, cw, lh)) = self.metrics {
            if s == scale {
                return (cw, lh);
            }
        }
        let (w, _) = fonts.measure("MMMMMMMMMM", Font::Mono(FONT_DIP), scale, 100_000.0, false);
        let cw = w / 10.0;
        let lh = (FONT_DIP * 1.45 * scale).round();
        self.metrics = Some((scale, cw, lh));
        (cw, lh)
    }

    fn rebuild_rows(&mut self, cols: usize) {
        let cols = cols.max(8);
        if self.rows_cols == cols && !self.rows.is_empty() {
            return;
        }
        self.rows.clear();
        for (i, &n) in self.line_chars.iter().enumerate() {
            if !self.wrap || n <= cols {
                self.rows.push((i, 0, n));
            } else {
                // Word wrap: break after the last space in the row when there is one
                // in its last third, otherwise break mid-word.
                let chars: Vec<char> = self.lines[i].chars().collect();
                let mut start = 0;
                while start < n {
                    let mut len = cols.min(n - start);
                    if start + len < n {
                        let min = len - len / 3;
                        if let Some(sp) = (min..len).rev().find(|&k| chars[start + k] == ' ') {
                            len = sp + 1;
                        }
                    }
                    self.rows.push((i, start, len));
                    start += len;
                }
            }
        }
        self.rows_cols = cols;
    }

    fn text_area(&self, r: Rect, scale: f32) -> Rect {
        let pad = (14.0 * scale).round();
        Rect::new(r.x + pad, r.y + (8.0 * scale).round(), (r.w - pad * 2.0).max(1.0), (r.h - (8.0 * scale).round()).max(1.0))
    }

    fn layout(&mut self, fonts: &mut Fonts, scale: f32, r: Rect) -> (Rect, f32, f32) {
        let (cw, lh) = self.metrics(fonts, scale);
        let mut a = self.text_area(r, scale);
        let g = self.gutter(cw, scale);
        a.x += g;
        a.w = (a.w - g).max(1.0);
        let cols = (a.w / cw).floor() as usize;
        self.rebuild_rows(if self.wrap { cols } else { usize::MAX / 4 });
        let content_w = if self.wrap { a.w } else { self.max_cols as f32 * cw + cw };
        let note_h = if self.note.is_some() { lh * 2.0 } else { 0.0 };
        self.scroll.set_extent(content_w, self.rows.len() as f32 * lh + note_h + lh, a.w, a.h);
        (a, cw, lh)
    }

    fn pos_at(&self, a: Rect, cw: f32, lh: f32, x: f32, y: f32) -> Pos {
        if self.rows.is_empty() {
            return Pos::default();
        }
        let row = (((y - a.y + self.scroll.y) / lh).floor().max(0.0) as usize).min(self.rows.len() - 1);
        let (line, start, len) = self.rows[row];
        let col = ((x - a.x + self.scroll.x) / cw).round().max(0.0) as usize;
        Pos { line, ch: start + col.min(len) }
    }

    fn selection(&self) -> Option<(Pos, Pos)> {
        let (a, b) = (self.anchor?, self.caret?);
        if a == b {
            return None;
        }
        Some(if a < b { (a, b) } else { (b, a) })
    }

    fn slice_chars(s: &str, start: usize, len: usize) -> &str {
        let mut it = s.char_indices();
        let b0 = it.nth(start).map(|(i, _)| i).unwrap_or(s.len());
        let b1 = if len == 0 { b0 } else { s[b0..].char_indices().nth(len).map(|(i, _)| b0 + i).unwrap_or(s.len()) };
        &s[b0..b1]
    }
}

impl View for TextView {
    fn kind(&self) -> &'static str {
        "text"
    }

    fn preferred_size(&mut self, fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let (cw, lh) = self.metrics(fonts, scale);
        let cols = self.max_cols.clamp(60, 110) as f32;
        let w = (cols * cw + 40.0 * scale + self.gutter(cw, scale)).min(max_w);
        let h = ((self.lines.len() as f32 + 1.0) * lh + 30.0 * scale).clamp(200.0 * scale, max_h * 0.85);
        (w.max(480.0 * scale).min(max_w), h)
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let (a, cw, lh) = self.layout(cx.fonts, cx.scale, r);
        let sel = self.selection();
        cx.clip(r);
        let first = (self.scroll.y / lh).floor() as usize;
        let count = (a.h / lh).ceil() as usize + 1;
        let col0 = if self.wrap { 0 } else { (self.scroll.x / cw).floor() as usize };
        let ncols = (a.w / cw).ceil() as usize + 1;
        let hl = self.highlight.clone();
        let spans = hl.as_ref().and_then(|h| h.lines.lock().ok());
        self.drawn_ready = spans.as_ref().map(|s| s.len()).unwrap_or(0);
        let gutter = self.gutter(cw, cx.scale);
        if gutter > 0.0 {
            cx.vline((a.x - (7.0 * cx.scale).round()).round(), r.y, r.bottom(), cx.theme.separator);
        }
        for vr in first..(first + count).min(self.rows.len()) {
            let (line, start, len) = self.rows[vr];
            let y = a.y + vr as f32 * lh - self.scroll.y;
            if let Some((s0, s1)) = sel {
                let (rs, re) = (Pos { line, ch: start }, Pos { line, ch: start + len });
                if s1 > rs && s0 <= re {
                    let c0 = if s0 > rs { s0.ch - start } else { 0 };
                    let c1 = if s1 < re { s1.ch - start } else { len + if s1.line > line { 1 } else { 0 } };
                    let x0 = a.x + c0 as f32 * cw - if self.wrap { 0.0 } else { self.scroll.x };
                    cx.fill(Rect::new(x0, y, (c1.saturating_sub(c0)) as f32 * cw, lh), cx.theme.selection);
                }
            }
            let (vs, vl) = if self.wrap { (start, len) } else { (start + col0.min(len), len.saturating_sub(col0).min(ncols)) };
            let s = Self::slice_chars(&self.lines[line], vs, vl);
            let x = a.x + if self.wrap { 0.0 } else { (col0 as f32 * cw) - self.scroll.x };
            if gutter > 0.0 && start == 0 {
                // Right-aligned in the gutter, outside the horizontal scroll.
                let n = (line + 1).to_string();
                let nx = a.x - (14.0 * cx.scale).round() - n.len() as f32 * cw;
                cx.text_at(&n, nx, y, gutter, lh, Font::Mono(FONT_DIP), cx.theme.text3);
            }
            // Colours of the visible part of the row.
            let runs: Vec<TextRun> = match spans.as_ref().and_then(|sp| sp.get(line)) {
                Some(line_spans) => line_spans
                    .iter()
                    .filter_map(|sp| {
                        let (s0, s1) = (sp.start as usize, (sp.start + sp.len) as usize);
                        let (a0, a1) = (s0.max(vs), s1.min(vs + vl));
                        (a1 > a0).then(|| TextRun { start: a0 - vs, len: a1 - a0, color: rgb(sp.rgb), bold: sp.bold, italic: sp.italic })
                    })
                    .collect(),
                None => Vec::new(),
            };
            cx.text_runs(s, x, y, a.w + cw * 2.0, lh, Font::Mono(FONT_DIP), cx.theme.text, &runs);
        }
        drop(spans);
        if let Some(note) = &self.note {
            let y = a.y + self.rows.len() as f32 * lh - self.scroll.y + lh * 0.5;
            cx.text_at(note, a.x, y, a.w, lh, Font::Ui(12.0), cx.theme.text3);
        }
        cx.unclip();
        self.scroll.draw(cx, r);
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        let (a, cw, lh) = self.layout(cx.fonts, cx.scale, r);
        if self.scroll.event(cx, r, ev, lh) {
            return;
        }
        match *ev {
            Event::MouseDown { x, y, button: MouseButton::Left, shift, double } => {
                let p = self.pos_at(a, cw, lh, x, y);
                if double {
                    self.anchor = Some(Pos { line: p.line, ch: 0 });
                    self.caret = Some(Pos { line: p.line, ch: self.line_chars[p.line] });
                } else {
                    if !shift || self.anchor.is_none() {
                        self.anchor = Some(p);
                    }
                    self.caret = Some(p);
                    self.dragging = true;
                    cx.capture = Some(true);
                }
                cx.redraw = true;
            }
            Event::MouseMove { x, y } => {
                cx.cursor = Cursor::IBeam;
                if self.dragging {
                    if y > a.bottom() {
                        self.scroll.scroll_by(0.0, lh);
                    } else if y < a.y {
                        self.scroll.scroll_by(0.0, -lh);
                    }
                    self.caret = Some(self.pos_at(a, cw, lh, x, y));
                    cx.redraw = true;
                }
            }
            Event::MouseUp { .. } => {
                if self.dragging {
                    self.dragging = false;
                    cx.capture = Some(false);
                }
            }
            Event::Key { vk, ctrl: true, .. } if vk == VK_A.0 => {
                self.anchor = Some(Pos::default());
                let last = self.lines.len().saturating_sub(1);
                self.caret = Some(Pos { line: last, ch: self.line_chars.get(last).copied().unwrap_or(0) });
                cx.redraw = true;
            }
            Event::Key { vk, .. } => {
                cx.redraw |= self.scroll.key(vk, lh);
            }
            _ => {}
        }
    }

    fn next_frame_in(&self) -> Option<std::time::Duration> {
        // Colours still arriving.
        self.highlight.as_ref().filter(|h| !h.is_done() || h.ready() != self.drawn_ready).map(|_| std::time::Duration::from_millis(120))
    }

    fn tick(&mut self, _env: &crate::provider::ViewEnv) -> bool {
        self.highlight.as_ref().is_some_and(|h| h.ready() != self.drawn_ready)
    }

    fn actions(&self) -> Vec<Action> {
        let mut v = Vec::new();
        if self.rendered.is_some() {
            v.push(Action::new("rendered", glyph::DOC, "Show it rendered"));
        }
        v.push(Action::new("numbers", glyph::LIST, if self.numbers { "Hide line numbers" } else { "Show line numbers" }).active(self.numbers));
        v.push(Action::new("wrap", glyph::WRAP, if self.wrap { "Don't wrap lines" } else { "Wrap lines" }).active(self.wrap));
        v
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        if id == "rendered" {
            cx.switch_provider = self.rendered;
            return;
        }
        if id == "numbers" {
            self.numbers = !self.numbers;
            self.rows.clear();
            self.rows_cols = 0;
            cx.redraw = true;
            return;
        }
        if id == "wrap" {
            self.wrap = !self.wrap;
            self.rows.clear();
            self.rows_cols = 0;
            self.scroll.x = 0.0;
            cx.redraw = true;
        }
    }

    fn copy_text(&self) -> Option<String> {
        let (s0, s1) = self.selection()?;
        let mut out = String::new();
        for line in s0.line..=s1.line {
            let l = &self.lines[line];
            let n = self.line_chars[line];
            let a = if line == s0.line { s0.ch } else { 0 };
            let b = if line == s1.line { s1.ch.min(n) } else { n };
            out.push_str(Self::slice_chars(l, a, b.saturating_sub(a)));
            if line != s1.line {
                out.push_str("\r\n");
            }
        }
        Some(out)
    }

    fn subtitle(&self) -> Option<String> {
        self.subtitle.clone()
    }

    fn wants_focus(&self) -> bool {
        true
    }

    fn info(&self) -> Vec<(String, String)> {
        let mut v = vec![("Lines".into(), gt_core::fmt::group(self.lines.len() as u64)), ("Longest line".into(), format!("{} characters", self.max_cols))];
        if let Some(h) = &self.highlight {
            let note = match h.limit {
                Some(n) if self.lines.len() > n => format!(" (first {} lines coloured)", gt_core::fmt::group(n as u64)),
                _ => String::new(),
            };
            v.push(("Syntax".into(), format!("{}{note}", h.language)));
        }
        v
    }
}
