//! Spreadsheets (.xls, .xlsx, .xlsm, .xlsb, .ods) as a native grid: calamine
//! reads the cell values (cached formula results included), the grid draws
//! only what is on screen, with sheet tabs, cell selection and copy as TSV.
//! "Print layout" converts the workbook with LibreOffice for the real look.

use crate::staged::StagedView;
use calamine::{Data, Range, Reader};
use gt_core::CancelToken;
use gt_ui::cx::{Cursor, Fonts};
use gt_ui::scroll::Scroll;
use gt_ui::{glyph, rgba, Action, Align, DrawCx, Event, EventCx, Font, Loaded, MouseButton, Probe, Provider, Rect, Request, View, ViewEnv};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_UP};

pub const EXTS: &[&str] = &["xls", "xlsx", "xlsm", "xlsb", "xla", "xlam", "ods", "fods"];

pub struct Sheet {
    pub name: String,
    pub hidden: bool,
    pub range: Range<Data>,
    /// Column widths in DIPs (sized from the content).
    widths: Vec<f32>,
}

fn general(f: f64) -> String {
    if f.is_nan() {
        return "#NUM!".into();
    }
    if f == f.trunc() && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    let a = f.abs();
    if !(1e-5..1e11).contains(&a) {
        let s = format!("{:.5E}", f);
        // 1.23450E3 → 1.2345E+03 (Excel style)
        if let Some((m, e)) = s.split_once('E') {
            let m = m.trim_end_matches('0').trim_end_matches('.');
            let e: i32 = e.parse().unwrap_or(0);
            return format!("{m}E{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
        }
        return s;
    }
    // Up to 10 significant digits, like Excel's General format.
    let digits = (10 - (a.log10().floor() as i32 + 1)).clamp(0, 10) as usize;
    let s = format!("{:.*}", digits, f);
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Excel serial date → "2024-03-05" / "2024-03-05 14:30" / "14:30:00".
fn excel_date(v: f64, duration: bool) -> String {
    if duration {
        let secs = (v * 86400.0).round() as i64;
        return format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60);
    }
    let days = v.floor() as i64;
    let frac = v - v.floor();
    let secs = (frac * 86400.0).round() as i64;
    if days == 0 {
        return format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60);
    }
    // Days since 1899-12-30 → civil date.
    let z = days - 25569 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    if secs == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, (secs / 60) % 60)
    }
}

pub fn cell_text(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => general(*f),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Data::DateTime(dt) => excel_date(dt.as_f64(), dt.is_duration()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.replace('T', " "),
        Data::Error(e) => e.to_string(),
    }
}

fn is_number(d: &Data) -> bool {
    matches!(d, Data::Int(_) | Data::Float(_) | Data::DateTime(_) | Data::DateTimeIso(_) | Data::DurationIso(_))
}

fn col_name(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

fn size_columns(range: &Range<Data>) -> Vec<f32> {
    let (h, w) = (range.height(), range.width());
    let mut widths = vec![64.0f32; w];
    for r in 0..h.min(300) {
        for (c, wd) in widths.iter_mut().enumerate() {
            if let Some(d) = range.get((r, c)) {
                let n = match d {
                    Data::String(s) => s.lines().map(|l| l.chars().count()).max().unwrap_or(0),
                    Data::Empty => 0,
                    other => cell_text(other).chars().count(),
                };
                *wd = wd.max((n as f32 * 7.2 + 14.0).min(300.0));
            }
        }
    }
    widths
}

pub fn load_workbook(path: &std::path::Path, cancel: &CancelToken) -> Result<(Vec<Sheet>, Vec<(String, String)>), String> {
    let mut wb = calamine::open_workbook_auto(path).map_err(|e| {
        let s = e.to_string();
        if s.to_ascii_lowercase().contains("password") || s.contains("encrypt") {
            "This workbook is password protected.".to_string()
        } else {
            format!("Cannot read this workbook: {s}")
        }
    })?;
    let meta: Vec<(String, bool, bool)> =
        wb.sheets_metadata().iter().map(|s| (s.name.clone(), s.visible != calamine::SheetVisible::Visible, s.typ != calamine::SheetType::WorkSheet)).collect();
    let mut sheets = Vec::new();
    let mut cells = 0usize;
    for (name, hidden, chart) in meta {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        if chart {
            continue;
        }
        match wb.worksheet_range(&name) {
            Ok(range) => {
                cells += range.get_size().0 * range.get_size().1;
                let widths = size_columns(&range);
                sheets.push(Sheet { name, hidden, range, widths });
            }
            Err(e) => tracing::warn!("sheet {name}: {e}"),
        }
    }
    if sheets.is_empty() {
        return Err("This workbook has no worksheets.".into());
    }
    let visible = sheets.iter().filter(|s| !s.hidden).count();
    let mut facts = vec![(
        "Sheets".to_string(),
        if visible == sheets.len() { sheets.len().to_string() } else { format!("{} ({} hidden)", sheets.len(), sheets.len() - visible) },
    )];
    let first = &sheets[0].range;
    if let (Some(s), Some(e)) = (first.start(), first.end()) {
        facts.push(("Used range".into(), format!("{}{}:{}{} ({} × {})", col_name(s.1 as usize), s.0 + 1, col_name(e.1 as usize), e.0 + 1, first.height(), first.width())));
    }
    facts.push(("Cells".into(), gt_core::fmt::group(cells as u64)));
    // Hidden sheets go last.
    sheets.sort_by_key(|s| s.hidden);
    Ok((sheets, facts))
}

pub struct SheetView {
    path: PathBuf,
    sheets: Vec<Sheet>,
    cur: usize,
    scroll: Vec<Scroll>,
    sel: Option<((usize, usize), (usize, usize))>,
    facts: Vec<(String, String)>,
    tabs: Vec<(usize, Rect)>,
    grid: Rect,
    selecting: bool,
    request: Option<Request>,
}

impl SheetView {
    pub fn new(path: PathBuf, sheets: Vec<Sheet>, facts: Vec<(String, String)>) -> Self {
        let n = sheets.len();
        SheetView { path, sheets, cur: 0, scroll: vec![Scroll::default(); n], sel: None, facts, tabs: Vec::new(), grid: Rect::default(), selecting: false, request: None }
    }

    fn row_h(scale: f32) -> f32 {
        (22.0 * scale).round()
    }

    fn head_h(scale: f32) -> f32 {
        (24.0 * scale).round()
    }

    fn rowhead_w(&self, scale: f32) -> f32 {
        let rows = self.sheets[self.cur].range.end().map(|e| e.0 + 1).unwrap_or(1);
        ((rows.to_string().len() as f32 * 7.5 + 18.0).max(36.0) * scale).round()
    }

    fn tabs_h(&self, scale: f32) -> f32 {
        if self.sheets.len() > 1 {
            (30.0 * scale).round()
        } else {
            0.0
        }
    }

    /// Absolute row/column of the first cell of the used range.
    fn origin(&self) -> (usize, usize) {
        self.sheets[self.cur].range.start().map(|s| (s.0 as usize, s.1 as usize)).unwrap_or((0, 0))
    }

    fn col_x(&self, scale: f32) -> Vec<f32> {
        let mut xs = Vec::with_capacity(self.sheets[self.cur].widths.len() + 1);
        let mut x = 0.0;
        xs.push(0.0);
        for w in &self.sheets[self.cur].widths {
            x += (w * scale).round();
            xs.push(x);
        }
        xs
    }

    /// Cell under a point (relative indices into the range).
    fn hit(&self, x: f32, y: f32, scale: f32) -> Option<(usize, usize)> {
        let g = self.grid;
        let rw = self.rowhead_w(scale);
        let hh = Self::head_h(scale);
        if !g.contains(x, y) || x < g.x + rw || y < g.y + hh {
            return None;
        }
        let s = &self.scroll[self.cur];
        let r = ((y - g.y - hh + s.y) / Self::row_h(scale)).floor() as usize;
        let cx = x - g.x - rw + s.x;
        let xs = self.col_x(scale);
        let c = xs.partition_point(|&v| v <= cx).saturating_sub(1);
        let range = &self.sheets[self.cur].range;
        (r < range.height() && c < range.width()).then_some((r, c))
    }

    fn selection_text(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.sel?;
        let (ra, rb) = (r0.min(r1), r0.max(r1));
        let (ca, cb) = (c0.min(c1), c0.max(c1));
        let range = &self.sheets[self.cur].range;
        let mut out = String::new();
        for r in ra..=rb.min(ra + 100_000) {
            let row: Vec<String> = (ca..=cb).map(|c| range.get((r, c)).map(cell_text).unwrap_or_default()).collect();
            out.push_str(&row.join("\t"));
            out.push('\n');
        }
        Some(out.trim_end_matches('\n').to_string())
    }

    fn ensure_visible(&mut self, cell: (usize, usize), scale: f32) {
        let xs = self.col_x(scale);
        let rh = Self::row_h(scale);
        let vw = self.grid.w - self.rowhead_w(scale);
        let vh = self.grid.h - Self::head_h(scale) - self.tabs_h(scale);
        let s = &mut self.scroll[self.cur];
        let top = cell.0 as f32 * rh;
        if top < s.y {
            s.y = top;
        } else if top + rh > s.y + vh {
            s.y = top + rh - vh;
        }
        let (l, r) = (xs[cell.1], xs[cell.1 + 1]);
        if l < s.x {
            s.x = l;
        } else if r > s.x + vw {
            s.x = r - vw;
        }
        s.clamp();
    }
}

impl View for SheetView {
    fn kind(&self) -> &'static str {
        "sheet"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let w: f32 = self.sheets[0].widths.iter().map(|w| w * scale).sum::<f32>() + self.rowhead_w(scale) + 16.0 * scale;
        let h = self.sheets[0].range.height() as f32 * Self::row_h(scale) + Self::head_h(scale) + self.tabs_h(scale) + 16.0 * scale;
        (w.clamp(560.0 * scale, max_w * 0.9), h.clamp(320.0 * scale, max_h * 0.9))
    }

    fn draw(&mut self, cx: &mut DrawCx, rect: Rect) {
        let sc = cx.scale;
        self.grid = rect;
        let rw = self.rowhead_w(sc);
        let hh = Self::head_h(sc);
        let rh = Self::row_h(sc);
        let th = self.tabs_h(sc);
        let xs = self.col_x(sc);
        let sheet = &self.sheets[self.cur];
        let range = &sheet.range;
        let (o_r, o_c) = self.origin();
        let body = Rect::new(rect.x + rw, rect.y + hh, rect.w - rw, rect.h - hh - th);
        let content_w = *xs.last().unwrap_or(&0.0) + 40.0 * sc;
        let content_h = range.height() as f32 * rh + 40.0 * sc;
        let s = &mut self.scroll[self.cur];
        s.set_extent(content_w, content_h, body.w, body.h);
        let (sx, sy) = (s.x, s.y);
        cx.fill(rect, rgba(0.105, 0.105, 0.11, 1.0));
        let grid_c = rgba(1.0, 1.0, 1.0, 0.07);
        let head_bg = rgba(0.15, 0.15, 0.155, 1.0);
        let r0 = (sy / rh).floor() as usize;
        let r1 = (((sy + body.h) / rh).ceil() as usize).min(range.height());
        let c0 = xs.partition_point(|&v| v <= sx).saturating_sub(1);
        let c1 = xs.partition_point(|&v| v < sx + body.w).min(range.width());
        let sel = self.sel.map(|((a, b), (c, d))| ((a.min(c), b.min(d)), (a.max(c), b.max(d))));
        cx.clip(body);
        // Selection fill.
        if let Some(((ra, ca), (rb, cb))) = sel {
            let x0 = body.x + xs[ca.min(xs.len() - 1)] - sx;
            let x1 = body.x + xs[(cb + 1).min(xs.len() - 1)] - sx;
            let y0 = body.y + ra as f32 * rh - sy;
            let y1 = body.y + (rb + 1) as f32 * rh - sy;
            cx.fill(Rect::new(x0, y0, x1 - x0, y1 - y0), rgba(0.30, 0.76, 1.0, 0.18));
            cx.stroke(Rect::new(x0, y0, x1 - x0, y1 - y0), cx.theme.accent, (1.5 * sc).round());
        }
        for r in r0..r1 {
            let y = body.y + r as f32 * rh - sy;
            cx.hline(body.x, body.right(), y + rh - 1.0, grid_c);
            let mut c = c0;
            while c < c1 {
                let x = body.x + xs[c] - sx;
                let Some(d) = range.get((r, c)) else {
                    c += 1;
                    continue;
                };
                if matches!(d, Data::Empty) {
                    c += 1;
                    continue;
                }
                let t = cell_text(d);
                let num = is_number(d);
                // Text spills over empty neighbours to the right, like Excel.
                let mut end = c + 1;
                if !num {
                    while end < range.width() && end < c + 8 && matches!(range.get((r, end)), Some(Data::Empty) | None) {
                        end += 1;
                    }
                }
                let w = xs[end] - xs[c];
                let color = match d {
                    Data::Error(_) => rgba(1.0, 0.55, 0.5, 1.0),
                    Data::Bool(_) => rgba(0.75, 0.85, 1.0, 1.0),
                    _ => cx.theme.text,
                };
                let line = t.lines().next().unwrap_or("");
                cx.text(line, Rect::new(x + 5.0 * sc, y, w - 10.0 * sc, rh), Font::Ui(12.0), color, if num { Align::Right } else { Align::Left });
                c += 1;
            }
        }
        for c in c0..c1 {
            let x = body.x + xs[c + 1] - sx;
            cx.vline(x - 1.0, body.y, body.bottom(), grid_c);
        }
        cx.unclip();
        // Column header.
        let head = Rect::new(rect.x, rect.y, rect.w, hh);
        cx.fill(head, head_bg);
        cx.clip(Rect::new(body.x, rect.y, body.w, hh));
        for c in c0..c1 {
            let x = body.x + xs[c] - sx;
            let w = xs[c + 1] - xs[c];
            let on = sel.is_some_and(|((_, ca), (_, cb))| c >= ca && c <= cb);
            if on {
                cx.fill(Rect::new(x, rect.y, w, hh), rgba(0.30, 0.76, 1.0, 0.16));
            }
            cx.text(&col_name(o_c + c), Rect::new(x, rect.y, w, hh), Font::Ui(11.5), if on { cx.theme.text } else { cx.theme.text2 }, Align::Center);
            cx.vline(x + w - 1.0, rect.y, rect.y + hh, grid_c);
        }
        cx.unclip();
        cx.hline(rect.x, rect.right(), rect.y + hh - 1.0, rgba(1.0, 1.0, 1.0, 0.14));
        // Row header.
        let rows = Rect::new(rect.x, body.y, rw, body.h);
        cx.fill(rows, head_bg);
        cx.clip(rows);
        for r in r0..r1 {
            let y = body.y + r as f32 * rh - sy;
            let on = sel.is_some_and(|((ra, _), (rb, _))| r >= ra && r <= rb);
            if on {
                cx.fill(Rect::new(rect.x, y, rw, rh), rgba(0.30, 0.76, 1.0, 0.16));
            }
            cx.text(&(o_r + r + 1).to_string(), Rect::new(rect.x, y, rw - 8.0 * sc, rh), Font::Ui(11.5), if on { cx.theme.text } else { cx.theme.text2 }, Align::Right);
            cx.hline(rect.x, rect.x + rw, y + rh - 1.0, grid_c);
        }
        cx.unclip();
        cx.vline(rect.x + rw - 1.0, rect.y, body.bottom(), rgba(1.0, 1.0, 1.0, 0.14));
        cx.fill(Rect::new(rect.x, rect.y, rw, hh), head_bg);
        self.scroll[self.cur].draw(cx, body);
        // Sheet tabs.
        self.tabs.clear();
        if th > 0.0 {
            let bar = Rect::new(rect.x, rect.bottom() - th, rect.w, th);
            cx.fill(bar, rgba(0.13, 0.13, 0.135, 1.0));
            cx.hline(bar.x, bar.right(), bar.y, rgba(1.0, 1.0, 1.0, 0.1));
            let mut x = bar.x + 6.0 * sc;
            for (i, sh) in self.sheets.iter().enumerate() {
                let (tw, _) = cx.measure(&sh.name, Font::Ui(12.0));
                let r = Rect::new(x, bar.y + 3.0 * sc, tw + 22.0 * sc, th - 6.0 * sc);
                if r.x > bar.right() {
                    break;
                }
                if i == self.cur {
                    cx.round(r, 4.0 * sc, rgba(1.0, 1.0, 1.0, 0.12));
                }
                let color = if sh.hidden {
                    cx.theme.text3
                } else if i == self.cur {
                    cx.theme.text
                } else {
                    cx.theme.text2
                };
                cx.text(&sh.name, r, if i == self.cur { Font::Semibold(12.0) } else { Font::Ui(12.0) }, color, Align::Center);
                self.tabs.push((i, r));
                x = r.right() + 4.0 * sc;
            }
        }
        if range.height() == 0 || range.width() == 0 {
            cx.text("This sheet is empty", body, Font::Ui(13.0), cx.theme.text3, Align::Center);
        }
    }

    fn event(&mut self, cx: &mut EventCx, rect: Rect, ev: &Event) {
        let sc = cx.scale;
        if let Event::MouseDown { x, y, button: MouseButton::Left, .. } = *ev {
            if let Some(&(i, _)) = self.tabs.iter().find(|(_, r)| r.contains(x, y)) {
                if i != self.cur {
                    self.cur = i;
                    self.sel = None;
                    cx.redraw = true;
                }
                return;
            }
        }
        let rw = self.rowhead_w(sc);
        let body = Rect::new(rect.x + rw, rect.y + Self::head_h(sc), rect.w - rw, rect.h - Self::head_h(sc) - self.tabs_h(sc));
        if self.scroll[self.cur].event(cx, body, ev, Self::row_h(sc)) {
            return;
        }
        match *ev {
            Event::MouseDown { x, y, button: MouseButton::Left, shift, .. } => {
                if let Some(cell) = self.hit(x, y, sc) {
                    self.sel = Some(match (shift, self.sel) {
                        (true, Some((a, _))) => (a, cell),
                        _ => (cell, cell),
                    });
                    self.selecting = true;
                    cx.capture = Some(true);
                    cx.redraw = true;
                }
            }
            Event::MouseMove { x, y } => {
                if self.selecting {
                    if let (Some(cell), Some((a, _))) = (self.hit(x, y, sc), self.sel) {
                        self.sel = Some((a, cell));
                        cx.redraw = true;
                    }
                }
                if self.tabs.iter().any(|(_, r)| r.contains(x, y)) {
                    cx.cursor = Cursor::Hand;
                }
            }
            Event::MouseUp { .. } => {
                if self.selecting {
                    self.selecting = false;
                    cx.capture = Some(false);
                }
            }
            Event::Key { vk, ctrl, shift } => {
                let range = &self.sheets[self.cur].range;
                let (h, w) = (range.height(), range.width());
                if h == 0 || w == 0 {
                    return;
                }
                if ctrl && (vk == VK_NEXT.0 || vk == VK_PRIOR.0) {
                    let n = self.sheets.len();
                    self.cur = if vk == VK_NEXT.0 { (self.cur + 1) % n } else { (self.cur + n - 1) % n };
                    self.sel = None;
                    cx.redraw = true;
                    return;
                }
                let Some((anchor, mut c)) = self.sel else {
                    if [VK_UP.0, VK_DOWN.0, VK_HOME.0, VK_END.0, VK_NEXT.0, VK_PRIOR.0].contains(&vk) && self.scroll[self.cur].key(vk, Self::row_h(sc)) {
                        cx.redraw = true;
                    }
                    return;
                };
                let page = ((body.h / Self::row_h(sc)) as usize).max(1);
                match vk {
                    v if v == VK_UP.0 => c.0 = c.0.saturating_sub(1),
                    v if v == VK_DOWN.0 => c.0 = (c.0 + 1).min(h - 1),
                    v if v == VK_LEFT.0 => c.1 = c.1.saturating_sub(1),
                    v if v == VK_RIGHT.0 => c.1 = (c.1 + 1).min(w - 1),
                    v if v == VK_PRIOR.0 => c.0 = c.0.saturating_sub(page),
                    v if v == VK_NEXT.0 => c.0 = (c.0 + page).min(h - 1),
                    v if v == VK_HOME.0 => c = if ctrl { (0, 0) } else { (c.0, 0) },
                    v if v == VK_END.0 => c = if ctrl { (h - 1, w - 1) } else { (c.0, w - 1) },
                    _ => return,
                }
                self.sel = Some(if shift { (anchor, c) } else { (c, c) });
                self.ensure_visible(c, sc);
                cx.redraw = true;
            }
            _ => {}
        }
    }

    fn actions(&self) -> Vec<Action> {
        if crate::office::available() {
            vec![Action::new("layout", glyph::DOC, "Print layout (LibreOffice)").label("Layout")]
        } else {
            Vec::new()
        }
    }

    fn on_action(&mut self, id: &str, _cx: &mut EventCx) {
        if id == "layout" {
            let path = self.path.clone();
            let status = Arc::new(Mutex::new("Converting with LibreOffice…".to_string()));
            let view = StagedView::spawn(status, self.facts.clone(), 1.414, move |cancel, st| {
                let st2 = st.clone();
                let pdf = crate::office::to_pdf(&path, cancel, &move |s| *st2.lock() = s)?;
                let src = crate::pdf::open(&pdf).map_err(|e| e.to_string())?;
                let mut f = crate::pdf::facts(&src);
                f.retain(|(k, _)| k == "Pages" || k == "Page size");
                Ok((src as Arc<dyn crate::pages::PageSource>, f))
            });
            self.request = Some(Request::PushView { view: Box::new(view), title: "Print layout".into() });
        }
    }

    fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    fn copy_text(&self) -> Option<String> {
        self.selection_text()
    }

    fn info(&self) -> Vec<(String, String)> {
        self.facts.clone()
    }

    fn subtitle(&self) -> Option<String> {
        let s = &self.sheets[self.cur];
        let (o_r, o_c) = self.origin();
        let mut t = if self.sheets.len() > 1 { format!("{} · ", s.name) } else { String::new() };
        if let Some(((a, b), (c, d))) = self.sel {
            if (a, b) == (c, d) {
                t.push_str(&format!("{}{}", col_name(o_c + b), o_r + a + 1));
            } else {
                t.push_str(&format!("{}{}:{}{}", col_name(o_c + b.min(d)), o_r + a.min(c) + 1, col_name(o_c + b.max(d)), o_r + a.max(c) + 1));
            }
        } else {
            t.push_str(&format!("{} rows", gt_core::fmt::group(s.range.height() as u64)));
        }
        Some(t)
    }

    fn wants_focus(&self) -> bool {
        true
    }
}

pub struct SheetProvider;

impl Provider for SheetProvider {
    fn id(&self) -> &'static str {
        "sheet"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.ext_is(EXTS) {
            return 0;
        }
        let ole = p.head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]);
        let zip = p.head.starts_with(b"PK");
        if ole || zip || p.head.starts_with(b"<?xml") {
            85
        } else {
            0
        }
    }

    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let pr = crate::props::read(&p.path, &p.head);
        if pr.encrypted {
            return Ok(crate::provider::card(p, "This workbook is password protected.".into(), pr.facts));
        }
        let (sheets, mut facts) = match load_workbook(&p.path, cancel) {
            Ok(x) => x,
            Err(e) if e.contains("password") => return Ok(crate::provider::card(p, e, pr.facts)),
            Err(e) => return Err(e),
        };
        let mut all = pr.facts;
        all.retain(|(k, _)| k != "Pages" && k != "Words");
        all.append(&mut facts);
        Ok(Box::new(SheetLoaded(p.path.clone(), sheets, all)))
    }
}

struct SheetLoaded(PathBuf, Vec<Sheet>, Vec<(String, String)>);

impl Loaded for SheetLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        Box::new(SheetView::new(self.0, self.1, self.2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_and_date_formats() {
        assert_eq!(general(3.0), "3");
        assert_eq!(general(0.1 + 0.2), "0.3");
        assert_eq!(general(1234.5678), "1234.5678");
        assert_eq!(general(1.0e12), "1000000000000");
        assert_eq!(general(1.5e-7), "1.5E-07");
        assert_eq!(excel_date(45356.0, false), "2024-03-05");
        assert_eq!(excel_date(45356.5, false), "2024-03-05 12:00");
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(701), "ZZ");
        assert_eq!(col_name(702), "AAA");
    }
}
