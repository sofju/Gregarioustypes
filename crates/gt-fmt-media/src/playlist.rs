//! Playlists (M3U/M3U8, PLS, ASX/WVX/WAX, XSPF, WPL/ZPL, RAM, Monkey's Audio APL
//! and unknown line/XML lists): an entry list that plays through the shared engine.

use crate::engine::{self, LoadOpts};
use crate::transport::{self, Transport};
use gt_core::encoding;
use gt_ui::cx::Cursor;
use gt_ui::{glyph, rgba, Action, Align, DrawCx, Event, EventCx, Font, Fonts, HostCx, Rect, View};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub title: String,
    pub location: String,
    pub path: Option<PathBuf>,
    pub duration: f64,
    pub exists: bool,
    pub remote: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Playlist {
    pub name: String,
    pub kind: &'static str,
    pub entries: Vec<Entry>,
}

pub const EXTS: &[&str] = &["m3u", "m3u8", "pls", "xpl", "wvx", "wax", "asx", "wpl", "zpl", "xspf", "ram", "jmx", "apl"];

fn decode(bytes: &[u8], utf8_hint: bool) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let u: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        return String::from_utf16_lossy(&u);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) if utf8_hint => String::from_utf8_lossy(bytes).into_owned(),
        Err(_) => encoding::decode_1252(bytes),
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&amp;", "&").trim().to_string()
}

fn resolve(base: &Path, loc: &str) -> Entry {
    let loc = loc.trim();
    let mut e = Entry { location: loc.to_string(), ..Default::default() };
    let lower = loc.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("file:") {
        let raw = &loc[loc.len() - rest.len()..];
        let raw = raw.trim_start_matches('/');
        let p = PathBuf::from(percent_decode(raw).replace('/', "\\"));
        e.exists = p.exists();
        e.path = Some(p);
    } else if lower.contains("://") || lower.starts_with("mms:") || lower.starts_with("rtsp:") {
        e.remote = true;
    } else {
        let p = PathBuf::from(loc.replace('/', "\\"));
        let p = if p.is_absolute() { p } else { base.join(p) };
        e.exists = p.exists();
        e.path = Some(p);
    }
    e
}

/// Find `<tag ...>` elements (case-insensitive) and return (attributes, inner text) pairs.
fn elements<'a>(xml: &'a str, lower: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut i = 0;
    while let Some(p) = lower[i..].find(&open) {
        let s = i + p;
        let after = s + open.len();
        let next = lower[after..].chars().next().unwrap_or('>');
        if !(next.is_whitespace() || next == '>' || next == '/') {
            i = after;
            continue;
        }
        let Some(gt) = lower[after..].find('>') else { break };
        let attrs_end = after + gt;
        let attrs = &xml[after..attrs_end];
        if attrs.ends_with('/') {
            out.push((attrs, ""));
            i = attrs_end + 1;
            continue;
        }
        let body_start = attrs_end + 1;
        let body_end = lower[body_start..].find(&close).map(|e| body_start + e).unwrap_or(xml.len());
        out.push((attrs, &xml[body_start..body_end]));
        i = body_end.max(body_start);
    }
    out
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut i = 0;
    while let Some(p) = lower[i..].find(name) {
        let s = i + p;
        let before_ok = s == 0 || !lower.as_bytes()[s - 1].is_ascii_alphanumeric();
        let rest = lower[s + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let off = attrs.len() - rest.len() + 1;
            let v = attrs[off..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let end = v[1..].find(q)?;
                return Some(xml_unescape(&v[1..1 + end]));
            }
            let end = v.find(char::is_whitespace).unwrap_or(v.len());
            return Some(xml_unescape(&v[..end]));
        }
        i = s + name.len();
    }
    None
}

fn first_text(body: &str, lower_body: &str, tag: &str) -> Option<String> {
    elements(body, lower_body, tag).first().map(|(_, t)| xml_unescape(t))
}

pub fn parse(path: &Path, bytes: &[u8]) -> Option<Playlist> {
    let ext = gt_core::sniff::ext(path);
    let base = path.parent().unwrap_or(Path::new("."));
    let text = decode(bytes, ext == "m3u8");
    let trimmed = text.trim_start();
    let lower = text.to_ascii_lowercase();
    let mut pl = Playlist { name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), ..Default::default() };
    if trimmed.starts_with("[Monkey's Audio Image Link File]") || ext == "apl" && lower.contains("image file=") {
        pl.kind = "Monkey's Audio link";
        let get = |k: &str| text.lines().find_map(|l| l.split_once('=').filter(|(a, _)| a.trim().eq_ignore_ascii_case(k)).map(|(_, v)| v.trim().to_string()));
        let file = get("Image File")?;
        let mut e = resolve(base, &file);
        let (s, f) = (get("Start Block").unwrap_or_default(), get("Finish Block").unwrap_or_default());
        e.title = format!("{} (blocks {}–{})", file, s, f);
        pl.entries.push(e);
        return Some(pl);
    }
    if lower.contains("[playlist]") && lower.contains("file1=") {
        pl.kind = "PLS";
        let mut files: Vec<(usize, Entry)> = Vec::new();
        let mut titles = std::collections::HashMap::new();
        let mut lens = std::collections::HashMap::new();
        for l in text.lines() {
            let Some((k, v)) = l.split_once('=') else { continue };
            let k = k.trim().to_ascii_lowercase();
            let num = |p: &str| k.strip_prefix(p).and_then(|n| n.parse::<usize>().ok());
            if let Some(n) = num("file") {
                files.push((n, resolve(base, v)));
            } else if let Some(n) = num("title") {
                titles.insert(n, v.trim().to_string());
            } else if let Some(n) = num("length") {
                lens.insert(n, v.trim().parse::<f64>().unwrap_or(-1.0));
            }
        }
        files.sort_by_key(|(n, _)| *n);
        for (n, mut e) in files {
            e.title = titles.remove(&n).unwrap_or_default();
            e.duration = lens.get(&n).copied().unwrap_or(0.0).max(0.0);
            pl.entries.push(e);
        }
        return Some(pl);
    }
    if trimmed.starts_with('<') {
        if lower.contains("<asx") {
            pl.kind = "ASX";
            if let Some(t) = elements(&text, &lower, "asx").first().and_then(|(_, body)| {
                let lb = body.to_ascii_lowercase();
                let before_entry = &body[..lb.find("<entry").unwrap_or(body.len())];
                first_text(before_entry, &before_entry.to_ascii_lowercase(), "title")
            }) {
                pl.name = t;
            }
            for (_, body) in elements(&text, &lower, "entry") {
                let lb = body.to_ascii_lowercase();
                let refs = elements(body, &lb, "ref");
                let Some(href) = refs.iter().find_map(|(a, _)| attr(a, "href")) else { continue };
                let mut e = resolve(base, &href);
                e.title = first_text(body, &lb, "title").unwrap_or_default();
                if let Some(d) = elements(body, &lb, "duration").first().and_then(|(a, _)| attr(a, "value")) {
                    e.duration = parse_clock(&d);
                }
                pl.entries.push(e);
            }
            for (a, _) in elements(&text, &lower, "entryref") {
                if let Some(h) = attr(a, "href") {
                    pl.entries.push(resolve(base, &h));
                }
            }
        } else if lower.contains("xspf.org") || lower.contains("<tracklist") {
            pl.kind = "XSPF";
            for (_, body) in elements(&text, &lower, "track") {
                let lb = body.to_ascii_lowercase();
                let Some(loc) = first_text(body, &lb, "location") else { continue };
                let mut e = resolve(base, &loc);
                e.title = first_text(body, &lb, "title").unwrap_or_default();
                if let Some(c) = first_text(body, &lb, "creator") {
                    if !c.is_empty() && !e.title.is_empty() {
                        e.title = format!("{c} - {}", e.title);
                    }
                }
                e.duration = first_text(body, &lb, "duration").and_then(|d| d.parse::<f64>().ok()).unwrap_or(0.0) / 1000.0;
                pl.entries.push(e);
            }
        } else if lower.contains("<smil") || lower.contains("<media") {
            pl.kind = if ext == "zpl" { "ZPL" } else { "WPL" };
            if let Some(t) = first_text(&text, &lower, "title") {
                pl.name = t;
            }
            for (a, _) in elements(&text, &lower, "media") {
                if let Some(src) = attr(a, "src") {
                    pl.entries.push(resolve(base, &src));
                }
            }
        } else {
            // Unknown XML list: anything with src/href/location.
            pl.kind = "XML playlist";
            let mut i = 0;
            while let Some(p) = lower[i..].find('<') {
                let s = i + p + 1;
                let Some(e) = lower[s..].find('>') else { break };
                let attrs = &text[s..s + e];
                if let Some(src) = attr(attrs, "src").or_else(|| attr(attrs, "href")).or_else(|| attr(attrs, "location")).or_else(|| attr(attrs, "file")) {
                    let mut en = resolve(base, &src);
                    en.title = attr(attrs, "title").unwrap_or_default();
                    pl.entries.push(en);
                }
                i = s + e;
            }
            for loc in elements(&text, &lower, "location") {
                pl.entries.push(resolve(base, &xml_unescape(loc.1)));
            }
        }
        return (!pl.entries.is_empty()).then_some(pl);
    }
    // Line-based: M3U (with #EXTINF), RAM, plain lists.
    pl.kind = match ext.as_str() {
        "ram" => "RealAudio metafile",
        "m3u8" => "M3U8",
        "m3u" => "M3U",
        _ => "Playlist",
    };
    let mut pending: Option<(f64, String)> = None;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(rest) = l.strip_prefix("#EXTINF:") {
            let (d, t) = rest.split_once(',').unwrap_or((rest, ""));
            let d = d.split_whitespace().next().unwrap_or("").parse::<f64>().unwrap_or(-1.0);
            pending = Some((d.max(0.0), t.trim().to_string()));
            continue;
        }
        if let Some(n) = l.strip_prefix("#PLAYLIST:") {
            pl.name = n.trim().to_string();
            continue;
        }
        if l.starts_with('#') || l.starts_with("--stop--") {
            continue;
        }
        let mut e = resolve(base, l);
        if let Some((d, t)) = pending.take() {
            e.duration = d;
            e.title = t;
        }
        pl.entries.push(e);
    }
    // Reject text that merely resembles a list (needs at least one plausible entry).
    let plausible = pl.entries.iter().any(|e| e.exists || e.remote || e.location.contains('.'));
    (plausible && !pl.entries.is_empty()).then_some(pl)
}

fn parse_clock(s: &str) -> f64 {
    let mut t = 0.0;
    for part in s.split(':') {
        t = t * 60.0 + part.trim().parse::<f64>().unwrap_or(0.0);
    }
    t
}

// ------------------------------------------------------------------ view

pub struct PlaylistView {
    pl: Playlist,
    host: Option<HostCx>,
    selected: Option<usize>,
    scroll: f32,
    hover: Option<usize>,
    /// Index of the first entry handed to mpv (playlist-pos is relative to it).
    play_from: Option<usize>,
    playable: Vec<usize>,
    transport: Transport,
    drawn_time: f64,
    scale: f32,
}

impl PlaylistView {
    pub fn new(pl: Playlist) -> Self {
        let playable = pl.entries.iter().enumerate().filter(|(_, e)| e.exists || e.remote).map(|(i, _)| i).collect();
        PlaylistView { pl, host: None, selected: None, scroll: 0.0, hover: None, play_from: None, playable, transport: Transport::default(), drawn_time: -1.0, scale: 1.0 }
    }

    fn row_h(scale: f32) -> f32 {
        (30.0 * scale).round()
    }

    fn header_h(scale: f32) -> f32 {
        (58.0 * scale).round()
    }

    fn playing(&self) -> bool {
        self.play_from.is_some()
    }

    fn st(&self) -> engine::PlayerState {
        if !self.playing() {
            return Default::default();
        }
        engine::with_existing(|e| e.st.clone()).unwrap_or_default()
    }

    /// Entry index currently playing.
    fn current(&self) -> Option<usize> {
        let from = self.play_from?;
        let pos = self.st().playlist_pos;
        let k = self.playable.iter().position(|&i| i == from)?;
        self.playable.get(k + pos.max(0) as usize).copied()
    }

    fn url(e: &Entry) -> String {
        match &e.path {
            Some(p) => p.display().to_string(),
            None => e.location.clone(),
        }
    }

    fn play(&mut self, from: usize) {
        let Some(host) = self.host else { return };
        let Some(k) = self.playable.iter().position(|&i| i == from) else { return };
        let list: Vec<String> = self.playable[k..].iter().map(|&i| Self::url(&self.pl.entries[i])).collect();
        engine::with(host.parent, |e| {
            e.set_display(host.hdr, host.peak_nits);
            e.load(&list[0], LoadOpts { audio_only: false, pause: false, start: 0.0, hdr_source: false, extra: Vec::new() });
            for u in &list[1..] {
                e.append(u, false);
            }
            e.follow_playlist(true);
        });
        self.play_from = Some(from);
        self.selected = Some(from);
    }

    fn stop(&mut self) {
        if self.play_from.take().is_some() {
            engine::with_existing(|e| e.stop());
        }
    }

    /// (video area, list area, transport bar)
    fn layout(&self, r: Rect, scale: f32) -> (Option<Rect>, Rect, Option<Rect>) {
        let pad = (12.0 * scale).round();
        let st = self.st();
        let bar_h = if self.playing() { transport::height(scale) + pad * 2.0 } else { 0.0 };
        let video = (self.playing() && st.video_w > 0).then(|| Rect::new(r.x, r.y, r.w, ((r.h - bar_h) * 0.6).round()));
        let top = video.map(|v| v.bottom()).unwrap_or(r.y);
        let list = Rect::new(r.x, top, r.w, (r.bottom() - bar_h - top).max(0.0));
        let bar = self.playing().then(|| Rect::new(r.x + pad, r.bottom() - bar_h + pad, r.w - 2.0 * pad, transport::height(scale)));
        (video, list, bar)
    }

    fn total(&self) -> f64 {
        self.pl.entries.iter().map(|e| e.duration).sum()
    }

    fn row_at(&self, list: Rect, scale: f32, y: f32) -> Option<usize> {
        let top = list.y + Self::header_h(scale);
        if y < top {
            return None;
        }
        let i = ((y - top + self.scroll) / Self::row_h(scale)) as usize;
        (i < self.pl.entries.len()).then_some(i)
    }
}

impl View for PlaylistView {
    fn kind(&self) -> &'static str {
        "playlist"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let rows = self.pl.entries.len().clamp(4, 16) as f32;
        let h = Self::header_h(scale) + rows * Self::row_h(scale) + transport::height(scale) + 40.0 * scale;
        ((760.0 * scale).min(max_w), h.min(max_h))
    }

    fn attach(&mut self, host: &HostCx) {
        self.host = Some(*host);
    }

    fn detach(&mut self) {
        self.stop();
    }

    fn native_content(&self) -> bool {
        true
    }

    fn layout_native(&mut self, rect: Rect, host: &HostCx) {
        self.host = Some(*host);
        if !self.playing() {
            return;
        }
        let (video, _, _) = self.layout(rect, self.scale);
        engine::with_existing(|e| match video {
            Some(v) if e.st.first_frame => e.place((v.x as i32, v.y as i32, v.w as i32, v.h as i32), true),
            _ => e.place((0, 0, 0, 0), false),
        });
    }

    fn wake(&mut self) -> bool {
        if !self.playing() {
            engine::with_existing(|e| e.pump());
            return false;
        }
        let c = engine::with_existing(|e| e.pump()).unwrap_or_default();
        if !c.any {
            return false;
        }
        if c.time_only {
            return (self.st().time - self.drawn_time).abs() >= 0.25;
        }
        true
    }

    fn actions(&self) -> Vec<Action> {
        if self.playing() {
            vec![Action::new("stop", glyph::LIST, "Stop playback").label("Stop")]
        } else if !self.playable.is_empty() {
            vec![Action::new("playall", glyph::PLAY, "Play every entry in order").label("Play all")]
        } else {
            Vec::new()
        }
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        match id {
            "playall" => {
                if let Some(&i) = self.playable.first() {
                    self.play(self.selected.filter(|s| self.playable.contains(s)).unwrap_or(i));
                }
            }
            "stop" => self.stop(),
            _ => {}
        }
        cx.redraw = true;
        cx.relayout = false;
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let s = cx.scale;
        self.scale = s;
        let (video, list, bar) = self.layout(r, s);
        let st = self.st();
        // The window leaves the content area clear for the video: paint ours.
        cx.fill(Rect::new(r.x, list.y, r.w, r.bottom() - list.y), cx.theme.window_bg);
        if let Some(v) = video {
            if !st.first_frame {
                cx.fill(v, rgba(0.0, 0.0, 0.0, 1.0));
            }
        }
        let pad = cx.px(16.0);
        let hh = Self::header_h(s);
        let head = Rect::new(list.x + pad, list.y, list.w - 2.0 * pad, hh);
        cx.text(&self.pl.name, Rect::new(head.x, head.y + cx.px(8.0), head.w, cx.px(24.0)), Font::Semibold(15.0), cx.theme.text, Align::Left);
        let mut sub = format!("{} · {} entr{}", self.pl.kind, self.pl.entries.len(), if self.pl.entries.len() == 1 { "y" } else { "ies" });
        let missing = self.pl.entries.iter().filter(|e| !e.exists && !e.remote).count();
        if missing > 0 {
            sub += &format!(" · {missing} missing");
        }
        let total = self.total();
        if total > 0.0 {
            sub += &format!(" · {}", gt_core::fmt::duration(total));
        }
        cx.text(&sub, Rect::new(head.x, head.y + cx.px(32.0), head.w, cx.px(18.0)), Font::Ui(12.0), cx.theme.text3, Align::Left);
        cx.hline(list.x, list.right(), list.y + hh - 1.0, cx.theme.separator);
        let rh = Self::row_h(s);
        let body = Rect::new(list.x, list.y + hh, list.w, (list.h - hh).max(0.0));
        let max_scroll = (self.pl.entries.len() as f32 * rh - body.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let current = self.current();
        cx.clip(body);
        let num_w = cx.px(44.0);
        let dur_w = cx.px(64.0);
        for (i, e) in self.pl.entries.iter().enumerate() {
            let y = body.y + i as f32 * rh - self.scroll;
            if y + rh < body.y || y > body.bottom() {
                continue;
            }
            let row = Rect::new(body.x, y, body.w, rh);
            if Some(i) == self.selected {
                cx.fill(row, cx.theme.selection);
            } else if Some(i) == self.hover {
                cx.fill(row, cx.theme.hover);
            } else if i % 2 == 1 {
                cx.fill(row, cx.theme.row_alt);
            }
            let ok = e.exists || e.remote;
            let color = if ok { cx.theme.text } else { rgba(1.0, 0.55, 0.5, 0.9) };
            let n = Rect::new(row.x, row.y, num_w, rh);
            if Some(i) == current {
                cx.glyph(if st.paused { glyph::PAUSE } else { glyph::PLAY }, n, 11.0, cx.theme.accent);
            } else {
                cx.text(&(i + 1).to_string(), n.inset_xy(cx.px(8.0), 0.0), Font::Ui(12.0), cx.theme.text3, Align::Right);
            }
            let name = if e.title.is_empty() {
                e.path.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| e.location.clone())
            } else {
                e.title.clone()
            };
            let tx = row.x + num_w + cx.px(8.0);
            let tw = (row.w - num_w - dur_w - cx.px(24.0)).max(0.0);
            let title_w = (tw * 0.55).round();
            cx.text(&name, Rect::new(tx, row.y, title_w, rh), Font::Ui(12.5), color, Align::Left);
            let loc = if ok { e.location.clone() } else { format!("missing: {}", e.location) };
            cx.text(&loc, Rect::new(tx + title_w + cx.px(12.0), row.y, tw - title_w - cx.px(12.0), rh), Font::Ui(11.5), cx.theme.text3, Align::Left);
            if e.duration > 0.0 {
                cx.text(&transport::fmt_time(e.duration, e.duration), Rect::new(row.right() - dur_w - cx.px(12.0), row.y, dur_w, rh), Font::Ui(12.0), cx.theme.text2, Align::Right);
            }
        }
        cx.unclip();
        if let Some(b) = bar {
            self.transport.draw(cx, b, &st);
            self.drawn_time = st.time;
        }
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        let s = cx.scale;
        let (_, list, _) = self.layout(r, s);
        let st = self.st();
        match *ev {
            Event::MouseMove { x, y } => {
                if self.playing() && self.transport.mouse_move(cx, x, y, &st) {
                    cx.redraw = true;
                }
                let h = if list.contains(x, y) { self.row_at(list, s, y) } else { None };
                if h != self.hover {
                    self.hover = h;
                    cx.redraw = true;
                }
                if h.is_some() {
                    cx.cursor = Cursor::Hand;
                }
            }
            Event::MouseLeave => {
                self.hover = None;
                self.transport.mouse_leave();
                cx.redraw = true;
            }
            Event::MouseDown { x, y, double, .. } => {
                if self.playing() && self.transport.mouse_down(cx, x, y, &st) {
                    return;
                }
                if !list.contains(x, y) {
                    return;
                }
                if let Some(i) = self.row_at(list, s, y) {
                    self.selected = Some(i);
                    if double {
                        if self.playable.contains(&i) {
                            self.play(i);
                        } else {
                            cx.toast = Some("This entry is missing".into());
                        }
                    }
                    cx.redraw = true;
                }
            }
            Event::MouseUp { x, .. } => {
                self.transport.mouse_up(cx, x, &st);
            }
            Event::Wheel { x, y, lines, horizontal: false, .. } => {
                if self.playing() && self.transport.wheel(x, y, lines, &st) {
                    cx.redraw = true;
                    return;
                }
                self.scroll -= lines * Self::row_h(s) * 3.0;
                cx.redraw = true;
            }
            Event::Key { vk, .. } => {
                if vk == 0x0D {
                    if let Some(i) = self.selected {
                        self.play(i);
                    }
                } else if self.playing() {
                    transport::key(vk, &st);
                }
                cx.redraw = true;
            }
            _ => {}
        }
    }

    fn wants_focus(&self) -> bool {
        false
    }

    fn info(&self) -> Vec<(String, String)> {
        let mut v = vec![("Playlist".into(), self.pl.kind.to_string()), ("Entries".into(), self.pl.entries.len().to_string())];
        let total = self.total();
        if total > 0.0 {
            v.push(("Total length".into(), gt_core::fmt::duration(total)));
        }
        v
    }

    fn subtitle(&self) -> Option<String> {
        Some(format!("{} · {}", self.pl.kind, self.pl.entries.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_and_pls() {
        let dir = std::env::temp_dir();
        let m3u = b"#EXTM3U\n#EXTINF:123,Artist - Song\nmusic/song.mp3\nhttp://example.com/live\n";
        let p = parse(&dir.join("x.m3u"), m3u).unwrap();
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].title, "Artist - Song");
        assert_eq!(p.entries[0].duration, 123.0);
        assert!(p.entries[1].remote);
        let pls = b"[playlist]\nFile1=a.mp3\nTitle1=A\nLength1=10\nFile2=b.mp3\nNumberOfEntries=2\n";
        let p = parse(&dir.join("x.pls"), pls).unwrap();
        assert_eq!(p.kind, "PLS");
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].title, "A");
    }

    #[test]
    fn asx_and_xspf() {
        let dir = std::env::temp_dir();
        let asx = br#"<ASX version="3.0"><Title>Mix</Title><Entry><Title>One</Title><Ref HREF="one.wma"/></Entry><Entry><ref href='two.wma' /></Entry></ASX>"#;
        let p = parse(&dir.join("x.wax"), asx).unwrap();
        assert_eq!(p.kind, "ASX");
        assert_eq!(p.name, "Mix");
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].title, "One");
        assert_eq!(p.entries[1].location, "two.wma");
        let xspf = br#"<?xml version="1.0"?><playlist xmlns="http://xspf.org/ns/0/"><trackList><track><location>file:///C:/Music/a%20b.flac</location><title>T</title><duration>5000</duration></track></trackList></playlist>"#;
        let p = parse(&dir.join("x.xspf"), xspf).unwrap();
        assert_eq!(p.entries[0].path.as_deref(), Some(Path::new("C:\\Music\\a b.flac")));
        assert_eq!(p.entries[0].duration, 5.0);
    }
}
