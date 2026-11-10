//! Sectioned document: headings, copyable key/value fields, and a collapsible file
//! tree with size columns. Used by torrents now; archives, playlists and Blu-ray
//! summaries later.

use crate::cx::{Align, Cursor, DrawCx, EventCx, Font, Fonts};
use crate::geom::Rect;
use crate::glyph;
use crate::scroll::Scroll;
use crate::theme::hex;
use crate::view::{Event, MouseButton, View};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Row {
    Heading(String),
    /// Click copies the value.
    Field { key: String, value: String, mono: bool },
    Text(String),
    Spacer,
}

impl Row {
    pub fn field(k: impl Into<String>, v: impl Into<String>) -> Row {
        Row::Field { key: k.into(), value: v.into(), mono: false }
    }
    pub fn mono(k: impl Into<String>, v: impl Into<String>) -> Row {
        Row::Field { key: k.into(), value: v.into(), mono: true }
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Extra column values after the size column.
    pub extra: Vec<String>,
    pub children: Vec<usize>,
    pub expanded: bool,
    pub depth: u16,
    pub files: u64,
    /// Caller's id for files (archive entry index), `u32::MAX` when none.
    pub id: u32,
    /// Encrypted entry: drawn with a lock.
    pub locked: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    /// Column titles after "Name" and "Size".
    pub extra_columns: Vec<String>,
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
}

/// Builds a tree from file paths, aggregating folder sizes.
#[derive(Default)]
pub struct TreeBuilder {
    tree: Tree,
    index: HashMap<(usize, String), usize>,
}

const ROOT: usize = usize::MAX;

impl TreeBuilder {
    pub fn new(extra_columns: Vec<String>) -> Self {
        Self { tree: Tree { extra_columns, ..Default::default() }, index: HashMap::new() }
    }

    fn child(&mut self, parent: usize, name: &str, is_dir: bool, depth: u16) -> usize {
        if let Some(&i) = self.index.get(&(parent, name.to_string())) {
            return i;
        }
        let i = self.tree.nodes.len();
        self.tree.nodes.push(Node { name: name.to_string(), is_dir, size: 0, extra: Vec::new(), children: Vec::new(), expanded: false, depth, files: 0, id: u32::MAX, locked: false });
        if parent == ROOT {
            self.tree.roots.push(i);
        } else {
            self.tree.nodes[parent].children.push(i);
        }
        self.index.insert((parent, name.to_string()), i);
        i
    }

    pub fn add_file(&mut self, path: &[String], size: u64, extra: Vec<String>) {
        self.add_entry(path, size, extra, u32::MAX, false);
    }

    /// A file with the caller's id (activation reports it) and an encrypted flag.
    pub fn add_entry(&mut self, path: &[String], size: u64, extra: Vec<String>, id: u32, locked: bool) {
        if path.is_empty() {
            return;
        }
        let mut parent = ROOT;
        for (d, comp) in path[..path.len() - 1].iter().enumerate() {
            parent = self.child(parent, comp, true, d as u16);
        }
        let leaf = self.child(parent, &path[path.len() - 1], false, (path.len() - 1) as u16);
        self.tree.nodes[leaf].size = size;
        self.tree.nodes[leaf].files = 1;
        self.tree.nodes[leaf].extra = extra;
        self.tree.nodes[leaf].id = id;
        self.tree.nodes[leaf].locked = locked;
    }

    /// Set the extra columns of a folder (e.g. its date from the archive).
    pub fn dir_extra(&mut self, path: &[String], extra: Vec<String>) {
        self.add_dir(path);
        let mut parent = ROOT;
        for comp in path {
            match self.index.get(&(parent, comp.clone())) {
                Some(&i) => parent = i,
                None => return,
            }
        }
        if parent != ROOT {
            self.tree.nodes[parent].extra = extra;
        }
    }

    pub fn add_dir(&mut self, path: &[String]) {
        let mut parent = ROOT;
        for (d, comp) in path.iter().enumerate() {
            parent = self.child(parent, comp, true, d as u16);
        }
    }

    pub fn finish(mut self, expand_depth: u16) -> Tree {
        fn total(t: &mut Tree, i: usize) -> (u64, u64) {
            if !t.nodes[i].is_dir {
                return (t.nodes[i].size, 1);
            }
            let kids = t.nodes[i].children.clone();
            let (mut s, mut f) = (0, 0);
            for k in kids {
                let (a, b) = total(t, k);
                s += a;
                f += b;
            }
            t.nodes[i].size = s;
            t.nodes[i].files = f;
            (s, f)
        }
        let roots = self.tree.roots.clone();
        for r in &roots {
            total(&mut self.tree, *r);
        }
        // Folders first, then case-insensitive name order.
        let nodes = self.tree.nodes.clone();
        let sort = |v: &mut Vec<usize>| v.sort_by(|&a, &b| nodes[b].is_dir.cmp(&nodes[a].is_dir).then_with(|| nodes[a].name.to_lowercase().cmp(&nodes[b].name.to_lowercase())));
        sort(&mut self.tree.roots);
        for i in 0..self.tree.nodes.len() {
            let mut c = std::mem::take(&mut self.tree.nodes[i].children);
            sort(&mut c);
            self.tree.nodes[i].children = c;
            if self.tree.nodes[i].is_dir && self.tree.nodes[i].depth < expand_depth {
                self.tree.nodes[i].expanded = true;
            }
        }
        self.tree
    }
}

#[derive(Clone, Copy, Debug)]
enum Line {
    Row(usize),
    TreeHeader,
    Node(usize),
}

pub struct DocList {
    rows: Vec<Row>,
    tree: Option<Tree>,
    lines: Vec<Line>,
    tops: Vec<f32>,
    total: f32,
    scroll: Scroll,
    hover: Option<usize>,
    kind: &'static str,
    subtitle: Option<String>,
    key_w: f32,
    selected: Option<usize>,
    activated: Option<u32>,
    /// Width of the extra columns (DIP).
    extra_w: f32,
}

fn line_h(l: &Line, rows: &[Row]) -> f32 {
    match l {
        Line::Row(i) => match rows[*i] {
            Row::Heading(_) => 38.0,
            Row::Field { .. } => 26.0,
            Row::Text(_) => 22.0,
            Row::Spacer => 10.0,
        },
        Line::TreeHeader => 28.0,
        Line::Node(_) => 24.0,
    }
}

impl DocList {
    pub fn new(kind: &'static str, rows: Vec<Row>, tree: Option<Tree>, subtitle: Option<String>) -> Self {
        let mut d = Self { rows, tree, lines: Vec::new(), tops: Vec::new(), total: 0.0, scroll: Scroll::default(), hover: None, kind, subtitle, key_w: 120.0, selected: None, activated: None, extra_w: EXTRA_W };
        d.rebuild();
        d
    }

    fn rebuild(&mut self) {
        self.lines.clear();
        for i in 0..self.rows.len() {
            self.lines.push(Line::Row(i));
        }
        if let Some(t) = &self.tree {
            self.lines.push(Line::TreeHeader);
            fn walk(t: &Tree, i: usize, out: &mut Vec<Line>) {
                out.push(Line::Node(i));
                if t.nodes[i].expanded {
                    for &c in &t.nodes[i].children {
                        walk(t, c, out);
                    }
                }
            }
            for &r in &t.roots {
                walk(t, r, &mut self.lines);
            }
        }
        self.tops.clear();
        let mut y = 0.0;
        for l in &self.lines {
            self.tops.push(y);
            y += line_h(l, &self.rows);
        }
        self.total = y;
    }

    fn line_at(&self, y_dip: f32) -> Option<usize> {
        if y_dip < 0.0 || y_dip >= self.total {
            return None;
        }
        Some(self.tops.partition_point(|&t| t <= y_dip).saturating_sub(1))
    }

    /// The file id the user double-clicked (or pressed Enter on), once.
    pub fn take_activated(&mut self) -> Option<u32> {
        self.activated.take()
    }

    pub fn set_extra_width(&mut self, w: f32) {
        self.extra_w = w;
    }

    pub fn set_key_width(&mut self, w: f32) {
        self.key_w = w;
    }

    fn content(&self, r: Rect, s: f32) -> Rect {
        r.inset_xy((20.0 * s).round(), (6.0 * s).round())
    }
}

const SIZE_W: f32 = 96.0;

/// Icon (and tint) for a file name.
fn file_glyph(name: &str) -> (char, Option<crate::theme::Color>) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "avif" | "tif" | "tiff" | "psd" | "svg" | "jxl" | "exr" | "dng" | "ico" => ('\u{EB9F}', Some(hex(0x6CCB5F, 1.0))),
        "mp4" | "mkv" | "mov" | "avi" | "wmv" | "m2ts" | "ts" | "webm" | "mpg" | "vob" | "flv" | "m4v" => (glyph::VIDEO, Some(hex(0xC88AFF, 1.0))),
        "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "ape" | "ac3" | "dts" | "mid" => (glyph::MUSIC, Some(hex(0xFF8AB4, 1.0))),
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "cab" | "iso" | "dmg" | "deb" | "rpm" | "pkg" | "cpio" | "wim" | "zst" => ('\u{E7B8}', Some(hex(0xFFB454, 1.0))),
        "txt" | "md" | "nfo" | "log" | "ini" | "cfg" | "json" | "xml" | "html" | "htm" | "css" | "js" | "rs" | "py" | "c" | "h" | "cpp" | "sh" => (glyph::DOC, None),
        "exe" | "dll" | "msi" | "sys" | "bat" | "cmd" | "ps1" => ('\u{E756}', Some(hex(0x4CC2FF, 1.0))),
        "pdf" => ('\u{EA90}', Some(hex(0xFF6B6B, 1.0))),
        _ => (glyph::FILE, None),
    }
}
const EXTRA_W: f32 = 150.0;

impl View for DocList {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let extra = self.tree.as_ref().map(|t| t.extra_columns.len()).unwrap_or(0) as f32;
        let w = (720.0 + extra * self.extra_w) * scale;
        let h = (self.total + 20.0) * scale;
        (w.min(max_w), h.clamp(240.0 * scale, max_h * 0.85))
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let s = cx.scale;
        let a = self.content(r, s);
        self.scroll.set_extent(a.w, self.total * s, a.w, a.h);
        cx.clip(r);
        let y0 = self.scroll.y / s;
        let first = self.line_at(y0).unwrap_or(0);
        let extra_cols = self.tree.as_ref().map(|t| t.extra_columns.clone()).unwrap_or_default();
        let ew = self.extra_w;
        let right_cols = (SIZE_W + ew * extra_cols.len() as f32) * s;
        for li in first..self.lines.len() {
            let top = a.y + self.tops[li] * s - self.scroll.y;
            if top > r.bottom() {
                break;
            }
            let h = line_h(&self.lines[li], &self.rows) * s;
            let lr = Rect::new(a.x, top, a.w, h);
            let hovered = self.hover == Some(li);
            match self.lines[li] {
                Line::Row(i) => match &self.rows[i] {
                    Row::Heading(t) => {
                        let tr = Rect::new(lr.x, lr.y + 10.0 * s, lr.w, lr.h - 12.0 * s);
                        cx.text(t, tr, Font::Semibold(13.5), cx.theme.text, Align::Left);
                        cx.hline(lr.x, lr.right(), lr.bottom() - 1.0, cx.theme.separator);
                    }
                    Row::Field { key, value, mono } => {
                        if hovered {
                            cx.round(lr.inset_xy(-6.0 * s, 1.0 * s), 4.0 * s, cx.theme.hover);
                        }
                        let kw = (self.key_w * s).round();
                        cx.text(key, Rect::new(lr.x, lr.y, kw - 8.0 * s, lr.h), Font::Ui(12.5), cx.theme.text2, Align::Left);
                        let font = if *mono { Font::Mono(12.0) } else { Font::Ui(12.5) };
                        let vr = Rect::new(lr.x + kw, lr.y, lr.w - kw - if hovered { 24.0 * s } else { 0.0 }, lr.h);
                        cx.text(value, vr, font, cx.theme.text, Align::Left);
                        if hovered {
                            cx.glyph(glyph::COPY, Rect::new(lr.right() - 22.0 * s, lr.y, 20.0 * s, lr.h), 12.0, cx.theme.text2);
                        }
                    }
                    Row::Text(t) => cx.text(t, lr, Font::Ui(12.5), cx.theme.text2, Align::Left),
                    Row::Spacer => {}
                },
                Line::TreeHeader => {
                    let hr = Rect::new(lr.x, lr.y + 4.0 * s, lr.w, lr.h - 4.0 * s);
                    cx.text("Name", Rect::new(hr.x + 26.0 * s, hr.y, hr.w - right_cols, hr.h), Font::Semibold(11.5), cx.theme.text3, Align::Left);
                    let mut x = hr.right() - right_cols;
                    cx.text("Size", Rect::new(x, hr.y, SIZE_W * s - 8.0 * s, hr.h), Font::Semibold(11.5), cx.theme.text3, Align::Right);
                    x += SIZE_W * s;
                    for c in &extra_cols {
                        cx.text(c, Rect::new(x + 12.0 * s, hr.y, ew * s - 12.0 * s, hr.h), Font::Semibold(11.5), cx.theme.text3, Align::Left);
                        x += ew * s;
                    }
                    cx.hline(lr.x, lr.right(), lr.bottom() - 1.0, cx.theme.separator);
                }
                Line::Node(n) => {
                    let Some(t) = &self.tree else { continue };
                    let node = &t.nodes[n];
                    if self.selected == Some(li) {
                        cx.round(lr.inset_xy(-6.0 * s, 0.0), 4.0 * s, cx.theme.selection);
                    } else if hovered {
                        cx.round(lr.inset_xy(-6.0 * s, 0.0), 4.0 * s, cx.theme.hover);
                    } else if li % 2 == 1 {
                        cx.fill(lr, cx.theme.row_alt);
                    }
                    let indent = node.depth as f32 * 18.0 * s;
                    let cr = Rect::new(lr.x + indent, lr.y, 16.0 * s, lr.h);
                    if node.is_dir && !node.children.is_empty() {
                        cx.glyph(if node.expanded { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT }, cr, 9.0, cx.theme.text2);
                    }
                    let ir = Rect::new(cr.right() + 2.0 * s, lr.y, 18.0 * s, lr.h);
                    if node.is_dir {
                        cx.glyph(glyph::FOLDER, ir, 14.0, hex(0xFFC83D, 1.0));
                    } else {
                        let (g, c) = file_glyph(&node.name);
                        cx.glyph(g, ir, 13.0, c.unwrap_or(cx.theme.text2));
                    }
                    let name_x = ir.right() + 6.0 * s;
                    let mut name_w = (lr.right() - right_cols - name_x - 8.0 * s).max(0.0);
                    if node.locked {
                        name_w = (name_w - 20.0 * s).max(0.0);
                        let (tw, _) = cx.measure(&node.name, Font::Ui(12.5));
                        let lx = name_x + tw.min(name_w) + 6.0 * s;
                        cx.glyph(glyph::LOCK, Rect::new(lx, lr.y, 14.0 * s, lr.h), 11.0, hex(0xFFB454, 1.0));
                    }
                    cx.text(&node.name, Rect::new(name_x, lr.y, name_w, lr.h), Font::Ui(12.5), cx.theme.text, Align::Left);
                    let mut x = lr.right() - right_cols;
                    let size = if node.is_dir { format!("{} · {}", gt_core::fmt::size(node.size), node.files) } else { gt_core::fmt::size(node.size) };
                    cx.text(&size, Rect::new(x, lr.y, SIZE_W * s - 8.0 * s, lr.h), Font::Ui(12.0), cx.theme.text2, Align::Right);
                    x += SIZE_W * s;
                    for v in &node.extra {
                        cx.text(v, Rect::new(x + 12.0 * s, lr.y, ew * s - 12.0 * s, lr.h), Font::Ui(12.0), cx.theme.text2, Align::Left);
                        x += ew * s;
                    }
                }
            }
        }
        cx.unclip();
        self.scroll.draw(cx, r);
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        let s = cx.scale;
        if self.scroll.event(cx, r, ev, 24.0 * s) {
            return;
        }
        let a = self.content(r, s);
        let at = |me: &Self, y: f32| me.line_at((y - a.y + me.scroll.y) / s);
        match *ev {
            Event::MouseMove { x, y } => {
                let h = if a.contains(x, y) { at(self, y) } else { None };
                let h = h.filter(|&i| matches!(self.lines[i], Line::Node(_)) || matches!(self.lines[i], Line::Row(j) if matches!(self.rows[j], Row::Field { .. })));
                if h != self.hover {
                    self.hover = h;
                    cx.redraw = true;
                }
                if h.is_some() {
                    cx.cursor = Cursor::Hand;
                }
            }
            Event::MouseLeave => {
                if self.hover.take().is_some() {
                    cx.redraw = true;
                }
            }
            Event::MouseDown { y, button: MouseButton::Left, double, .. } => {
                let Some(li) = at(self, y) else { return };
                match self.lines[li] {
                    Line::Row(i) => {
                        if let Row::Field { key, value, .. } = &self.rows[i] {
                            cx.copy = Some(value.clone());
                            cx.toast = Some(format!("{key} copied"));
                        }
                    }
                    Line::Node(n) => {
                        if let Some(t) = &mut self.tree {
                            if t.nodes[n].is_dir {
                                t.nodes[n].expanded = !t.nodes[n].expanded;
                                self.rebuild();
                                self.selected = None;
                                cx.redraw = true;
                            } else {
                                self.selected = Some(li);
                                if double && t.nodes[n].id != u32::MAX {
                                    self.activated = Some(t.nodes[n].id);
                                }
                                cx.redraw = true;
                            }
                        }
                    }
                    Line::TreeHeader => {}
                }
            }
            Event::Key { vk: 0x0D, .. } => {
                if let (Some(li), Some(t)) = (self.selected, &self.tree) {
                    if let Line::Node(n) = self.lines[li] {
                        if t.nodes[n].id != u32::MAX {
                            self.activated = Some(t.nodes[n].id);
                        }
                    }
                }
            }
            Event::Key { vk, .. } => cx.redraw |= self.scroll.key(vk, 24.0 * s),
            _ => {}
        }
    }

    fn subtitle(&self) -> Option<String> {
        self.subtitle.clone()
    }

    fn info(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Field { key, value, .. } => Some((key.clone(), value.clone())),
                _ => None,
            })
            .take(12)
            .collect()
    }

    fn copy_text(&self) -> Option<String> {
        let mut out = String::new();
        for r in &self.rows {
            match r {
                Row::Heading(h) => out.push_str(&format!("\r\n{h}\r\n")),
                Row::Field { key, value, .. } => out.push_str(&format!("{key}: {value}\r\n")),
                Row::Text(t) => out.push_str(&format!("{t}\r\n")),
                Row::Spacer => {}
            }
        }
        Some(out)
    }
}
