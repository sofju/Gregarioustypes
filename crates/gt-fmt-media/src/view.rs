//! Video and audio previews driven by the shared mpv engine.

use crate::engine::{self, Changes, LoadOpts, PlayerState};
use crate::probe::Summary;
use crate::transport::{self, Transport};
use gt_shell::info::Bgra;
use gt_ui::widgets::bitmap_from_bgra;
use gt_ui::{glyph, rgba, Align, DrawCx, Event, EventCx, Font, Fonts, HostCx, Rect, View, ViewEnv};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

const HIDE_AFTER: Duration = Duration::from_millis(2200);

/// What to hand to mpv and how.
#[derive(Clone, Debug)]
pub struct Source {
    pub path: String,
    /// URL given to mpv (a path, `dvd://`, `gtmidi://...`).
    pub url: String,
    pub extra: Vec<String>,
}

impl Source {
    pub fn file(path: &str) -> Source {
        Source { path: path.to_string(), url: path.to_string(), extra: Vec::new() }
    }
}

/// State shared by the media views: engine link, transport, resume point.
struct Player {
    src: Source,
    audio_only: bool,
    hdr: bool,
    attached: bool,
    host: Option<HostCx>,
    resume: f64,
    transport: Transport,
    drawn_time: f64,
}

impl Player {
    fn new(src: Source, audio_only: bool) -> Player {
        Player { src, audio_only, hdr: false, attached: false, host: None, resume: 0.0, transport: Transport::default(), drawn_time: -1.0 }
    }

    fn attach(&mut self, host: &HostCx) {
        self.host = Some(*host);
        self.attached = true;
        let (audio_only, resume, hdr_source) = (self.audio_only, self.resume, self.hdr);
        let src = self.src.clone();
        engine::with(host.parent, |e| {
            e.pump();
            e.set_display(host.hdr, host.peak_nits);
            e.load(&src.url, LoadOpts { audio_only, pause: !host.autoplay, start: resume, hdr_source, extra: src.extra.clone() });
        });
    }

    fn detach(&mut self) {
        if !self.attached {
            return;
        }
        self.attached = false;
        engine::with_existing(|e| {
            let st = &e.st;
            self.resume = if st.ended || st.eof || st.duration - st.time < 1.0 { 0.0 } else { st.time };
            e.stop();
        });
    }

    fn state(&self) -> PlayerState {
        if !self.attached {
            return PlayerState::default();
        }
        engine::with_existing(|e| e.st.clone()).unwrap_or_default()
    }

    fn pump(&mut self) -> Changes {
        if !self.attached {
            return Changes::default();
        }
        engine::with_existing(|e| e.pump()).unwrap_or_default()
    }

    fn failure(&self) -> Option<String> {
        engine::with_existing(|e| e.failure().map(|s| s.to_string())).flatten()
    }
}

fn draw_status(cx: &mut DrawCx, r: Rect, text: &str, error: bool) {
    let (tw, th) = cx.measure(text, Font::Ui(13.0));
    let w = (tw + cx.px(32.0)).min(r.w - cx.px(24.0));
    let b = Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - th) / 2.0 - cx.px(10.0), w, th + cx.px(20.0));
    cx.round(b, cx.px(8.0), rgba(0.07, 0.07, 0.075, 0.85));
    let c = if error { rgba(1.0, 0.62, 0.55, 1.0) } else { cx.theme.text2 };
    cx.text(text, b.inset_xy(cx.px(12.0), 0.0), Font::Ui(13.0), c, Align::Center);
}

fn fit(src_w: f32, src_h: f32, r: Rect) -> Rect {
    let k = (r.w / src_w.max(1.0)).min(r.h / src_h.max(1.0));
    let (w, h) = (src_w * k, src_h * k);
    Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0, w, h).snap()
}

// ------------------------------------------------------------------ video

pub struct VideoView {
    p: Player,
    summary: Option<Summary>,
    poster: Option<ID2D1Bitmap1>,
    natural: (f64, f64),
    last_move: Instant,
    mouse_in: bool,
    controls: bool,
    content: Rect,
}

impl VideoView {
    pub fn new(env: &ViewEnv, src: Source, summary: Option<Summary>, poster: Option<Bgra>) -> VideoView {
        let poster = poster.and_then(|b| bitmap_from_bgra(&env.gfx.dc, b.width, b.height, &b.data));
        Self::build(src, summary, poster)
    }

    /// A player for something that isn't a plain file (a Blu-ray title...).
    /// `hdr`: the source is known to be HDR (tone-map to the display peak).
    pub fn for_source(src: Source, summary: Option<Summary>, hdr: bool) -> VideoView {
        let mut v = Self::build(src, summary, None);
        v.p.hdr |= hdr;
        v
    }

    fn build(src: Source, summary: Option<Summary>, poster: Option<ID2D1Bitmap1>) -> VideoView {
        let natural = summary.as_ref().and_then(|s| s.video.iter().find(|v| v.width > 0)).map(|v| v.display_size()).unwrap_or((1920.0, 1080.0));
        let mut p = Player::new(src, false);
        // Unknown summary (MediaInfo missing): assume HDR when in doubt is wrong for
        // SDR, so only trust what MediaInfo reports.
        p.hdr = summary.as_ref().is_some_and(|s| s.video.iter().any(|v| !v.hdr_kind().is_empty()));
        VideoView { p, summary, poster, natural, last_move: Instant::now(), mouse_in: false, controls: true, content: Rect::default() }
    }

    fn want_controls(&self, st: &PlayerState) -> bool {
        self.p.transport.dragging() || st.paused || st.eof || !st.first_frame || (self.mouse_in && self.last_move.elapsed() < HIDE_AFTER)
    }

    fn bar_rect(&self, r: Rect, scale: f32) -> Rect {
        let m = (12.0 * scale).round();
        let w = (r.w - 2.0 * m).min((720.0 * scale).round());
        let h = transport::height(scale);
        Rect::new(r.x + (r.w - w) / 2.0, r.bottom() - h - m, w, h)
    }
}

impl View for VideoView {
    fn kind(&self) -> &'static str {
        "video"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let (w, h) = (self.natural.0 as f32 * scale, self.natural.1 as f32 * scale);
        let mut k = (max_w / w).min(max_h / h).min(1.0);
        // Tiny videos open at a usable size.
        let min_w = 560.0 * scale;
        if w * k < min_w {
            k = (min_w / w).min(max_w / w).min(max_h / h);
        }
        ((w * k).round(), (h * k).round())
    }

    fn attach(&mut self, host: &HostCx) {
        self.p.attach(host);
        self.last_move = Instant::now();
        self.controls = true;
    }

    fn upgrade(&mut self, env: &ViewEnv, payload: Box<dyn std::any::Any + Send>) -> bool {
        let Ok(b) = payload.downcast::<Bgra>() else { return false };
        self.poster = bitmap_from_bgra(&env.gfx.dc, b.width, b.height, &b.data);
        !self.p.state().first_frame
    }

    fn detach(&mut self) {
        self.p.detach();
    }

    fn layout_native(&mut self, rect: Rect, host: &HostCx) {
        self.content = rect;
        if !self.p.attached {
            return;
        }
        let failed = self.p.failure().is_some();
        engine::with_existing(|e| {
            e.set_display(host.hdr, host.peak_nits);
            let visible = e.st.first_frame && e.st.error.is_none() && !failed;
            e.place((rect.x as i32, rect.y as i32, rect.w as i32, rect.h as i32), visible);
        });
    }

    fn native_content(&self) -> bool {
        true
    }

    fn wake(&mut self) -> bool {
        let c = self.p.pump();
        if !c.any {
            return false;
        }
        let st = self.p.state();
        let want = self.want_controls(&st);
        if c.time_only {
            if !want || (st.time - self.p.drawn_time).abs() < 0.2 {
                return false;
            }
        }
        true
    }

    fn next_frame_in(&self) -> Option<Duration> {
        if !self.p.attached || !self.controls {
            return None;
        }
        let st = self.p.state();
        if st.paused || st.eof || self.p.transport.dragging() || !self.mouse_in {
            return if self.mouse_in || st.paused { None } else { Some(Duration::from_millis(400)) };
        }
        Some(HIDE_AFTER.saturating_sub(self.last_move.elapsed()).max(Duration::from_millis(50)))
    }

    fn tick(&mut self, _env: &ViewEnv) -> bool {
        let st = self.p.state();
        let want = self.want_controls(&st);
        let changed = want != self.controls;
        self.controls = want;
        changed
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let st = self.p.state();
        let failure = self.p.failure();
        let error = failure.clone().or(st.error.clone());
        if !st.first_frame || error.is_some() {
            cx.fill(r, rgba(0.0, 0.0, 0.0, 1.0));
            if let Some(b) = &self.poster {
                let sz = unsafe { b.GetPixelSize() };
                cx.bitmap(b, fit(sz.width as f32, sz.height as f32, r), None, 1.0, false);
            }
            if let Some(e) = &error {
                draw_status(cx, r, &format!("Can't play this file: {e}"), true);
            } else if self.p.attached && st.started && !st.loaded && self.poster.is_none() {
                draw_status(cx, r, "Loading…", false);
            }
        }
        self.controls = self.want_controls(&st) && error.is_none();
        if self.controls && self.p.attached {
            let bar = self.bar_rect(r, cx.scale);
            self.p.transport.draw(cx, bar, &st);
        }
        self.p.drawn_time = st.time;
        if st.buffering && st.first_frame {
            draw_status(cx, Rect::new(r.x, r.y, r.w, r.h * 0.3), "Buffering…", false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        let st = self.p.state();
        match *ev {
            Event::MouseMove { x, y } => {
                let was = self.controls;
                self.mouse_in = r.contains(x, y);
                self.last_move = Instant::now();
                self.controls = self.want_controls(&st);
                let bar = self.controls && self.p.transport.mouse_move(cx, x, y, &st);
                if bar || was != self.controls {
                    cx.redraw = true;
                }
            }
            Event::MouseLeave => {
                self.mouse_in = false;
                if self.p.transport.mouse_leave() {
                    cx.redraw = true;
                }
                let was = self.controls;
                self.controls = self.want_controls(&st);
                cx.redraw |= was != self.controls;
            }
            Event::MouseDown { x, y, double, .. } => {
                if self.controls && self.p.transport.mouse_down(cx, x, y, &st) {
                    return;
                }
                if !r.contains(x, y) || !self.p.attached {
                    return;
                }
                let p = !(st.paused || st.eof);
                engine::with_existing(|e| e.set_pause(p));
                if double {
                    cx.fullscreen = true;
                }
                cx.redraw = true;
            }
            Event::MouseUp { x, .. } => {
                self.p.transport.mouse_up(cx, x, &st);
            }
            Event::Wheel { x, y, lines, horizontal: false, .. } => {
                if !(self.controls && self.p.transport.wheel(x, y, lines, &st)) {
                    transport::seek_by(&st, lines as f64 * 5.0);
                }
                self.last_move = Instant::now();
                cx.redraw = true;
            }
            Event::Key { vk, .. } => {
                if transport::key(vk, &st) {
                    cx.redraw = true;
                }
            }
            _ => {}
        }
    }

    fn info(&self) -> Vec<(String, String)> {
        let mut v = self.summary.as_ref().map(|s| s.facts()).unwrap_or_default();
        let st = self.p.state();
        if !st.hwdec.is_empty() && st.hwdec != "no" {
            v.push(("Decoder".into(), format!("hardware ({})", st.hwdec)));
        } else if st.first_frame {
            v.push(("Decoder".into(), "software".into()));
        }
        if !st.out_gamma.is_empty() {
            let out = match st.out_gamma.as_str() {
                "pq" => format!("HDR10 (PQ), tone-mapped to {:.0} nits", st.out_max_luma),
                "hlg" => "HLG".into(),
                "linear" => "scRGB (linear)".into(),
                g => format!("SDR ({g})"),
            };
            v.push(("Output".into(), out));
        }
        v
    }

    fn subtitle(&self) -> Option<String> {
        let s = self.summary.as_ref()?;
        let v = s.video.iter().find(|v| v.width > 0)?;
        let (w, h) = v.display_size();
        let mut parts = vec![format!("{}×{}", w as u32, h as u32)];
        let hdr = v.hdr_kind();
        if !hdr.is_empty() {
            parts.push(hdr.to_string());
        }
        if s.duration > 0.0 {
            parts.push(transport::fmt_time(s.duration, s.duration));
        }
        Some(parts.join(" · "))
    }
}

// ------------------------------------------------------------------ audio

pub struct AudioView {
    p: Player,
    summary: Option<Summary>,
    cover: Option<ID2D1Bitmap1>,
    title: String,
    lines: Vec<String>,
    note: Option<String>,
    /// Glyph shown when there is no cover.
    glyph: char,
    playable: bool,
    extra_facts: Vec<(String, String)>,
    duration_hint: f64,
}

impl AudioView {
    pub fn new(env: &ViewEnv, src: Source, summary: Option<Summary>, cover: Option<Bgra>, fallback_title: &str) -> AudioView {
        let cover = cover.and_then(|b| bitmap_from_bgra(&env.gfx.dc, b.width, b.height, &b.data));
        let (title, lines) = match &summary {
            Some(s) => {
                let title = if s.title.is_empty() { fallback_title.to_string() } else { s.title.clone() };
                let mut lines = Vec::new();
                if !s.artist.is_empty() {
                    lines.push(s.artist.clone());
                }
                let album = [s.album.clone(), s.date.clone()].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
                if !album.is_empty() {
                    lines.push(album);
                }
                let fmt = s.audio.first().map(|a| a.describe()).unwrap_or_else(|| s.container.clone());
                if !fmt.is_empty() {
                    lines.push(fmt);
                }
                (title, lines)
            }
            None => (fallback_title.to_string(), Vec::new()),
        };
        AudioView { p: Player::new(src, true), summary, cover, title, lines, note: None, glyph: glyph::MUSIC, playable: true, extra_facts: Vec::new(), duration_hint: 0.0 }
    }

    /// Metadata only, with a note explaining why it can't play (DRM...).
    pub fn not_playable(mut self, note: &str) -> Self {
        self.playable = false;
        self.note = Some(note.to_string());
        self
    }

    pub fn with_glyph(mut self, g: char) -> Self {
        self.glyph = g;
        self
    }

    pub fn add_facts(&mut self, f: Vec<(String, String)>) {
        self.extra_facts.extend(f);
    }

    pub fn set_duration(&mut self, d: f64) {
        self.duration_hint = d;
    }

    pub fn with_lines(mut self, title: String, lines: Vec<String>) -> Self {
        self.title = title;
        self.lines = lines;
        self
    }

    fn layout(&self, r: Rect, scale: f32) -> (Rect, Rect, Rect) {
        let p = |d: f32| (d * scale).round();
        let pad = p(20.0);
        let bar_h = if self.playable { transport::height(scale) } else { 0.0 };
        let body = Rect::new(r.x + pad, r.y + pad, r.w - 2.0 * pad, (r.h - 2.0 * pad - bar_h - if self.playable { p(14.0) } else { 0.0 }).max(0.0));
        let art = body.h.min(body.w * 0.45).min(p(360.0));
        let art_r = Rect::new(body.x, body.y + (body.h - art) / 2.0, art, art);
        let text = Rect::new(art_r.right() + p(20.0), body.y, (body.right() - art_r.right() - p(20.0)).max(0.0), body.h);
        let bar = Rect::new(r.x + pad, r.bottom() - pad - bar_h, r.w - 2.0 * pad, bar_h);
        (art_r, text, bar)
    }
}

impl View for AudioView {
    fn kind(&self) -> &'static str {
        "audio"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let h = if self.playable { 300.0 } else { 250.0 };
        ((620.0 * scale).min(max_w), (h * scale).min(max_h))
    }

    fn attach(&mut self, host: &HostCx) {
        if self.playable {
            self.p.attach(host);
        }
    }

    fn detach(&mut self) {
        self.p.detach();
    }

    fn layout_native(&mut self, _rect: Rect, _host: &HostCx) {
        if self.p.attached {
            engine::with_existing(|e| e.place((0, 0, 0, 0), false));
        }
    }

    fn wake(&mut self) -> bool {
        let c = self.p.pump();
        if !c.any {
            return false;
        }
        if c.time_only {
            let st = self.p.state();
            return (st.time - self.p.drawn_time).abs() >= 0.2;
        }
        true
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let (art, text, bar) = self.layout(r, cx.scale);
        match &self.cover {
            Some(b) => {
                let sz = unsafe { b.GetPixelSize() };
                let d = fit(sz.width as f32, sz.height as f32, art);
                cx.round(Rect::new(d.x - 1.0, d.y - 1.0, d.w + 2.0, d.h + 2.0), cx.px(6.0), rgba(0.0, 0.0, 0.0, 0.35));
                cx.bitmap(b, d, None, 1.0, false);
            }
            None => {
                cx.round(art, cx.px(8.0), rgba(1.0, 1.0, 1.0, 0.06));
                cx.glyph(self.glyph, art, (art.h / cx.scale * 0.42).max(24.0), cx.theme.text3);
            }
        }
        let mut y = text.y + cx.px(6.0);
        let th = cx.text_wrapped(&self.title, Rect::new(text.x, y, text.w, cx.px(56.0)), Font::Bold(19.0), cx.theme.text);
        y += th + cx.px(8.0);
        for (i, l) in self.lines.iter().enumerate() {
            let (font, color) = if i == 0 { (Font::Ui(14.0), cx.theme.text2) } else { (Font::Ui(12.5), cx.theme.text3) };
            cx.text(l, Rect::new(text.x, y, text.w, cx.px(20.0)), font, color, Align::Left);
            y += cx.px(22.0);
        }
        let st = self.p.state();
        let err = self.p.failure().or(st.error.clone());
        if let Some(n) = &self.note {
            y += cx.px(8.0);
            cx.glyph(glyph::LOCK, Rect::new(text.x, y, cx.px(18.0), cx.px(18.0)), 13.0, rgba(1.0, 0.75, 0.4, 1.0));
            cx.text_wrapped(n, Rect::new(text.x + cx.px(24.0), y, text.w - cx.px(24.0), cx.px(60.0)), Font::Ui(12.5), rgba(1.0, 0.75, 0.4, 1.0));
        } else if let Some(e) = err {
            y += cx.px(8.0);
            cx.text_wrapped(&format!("Can't play: {e}"), Rect::new(text.x, y, text.w, cx.px(60.0)), Font::Ui(12.5), rgba(1.0, 0.62, 0.55, 1.0));
        }
        if self.playable && bar.h > 0.0 {
            self.p.transport.draw(cx, bar, &st);
            self.p.drawn_time = st.time;
        }
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        if !self.playable {
            return;
        }
        let st = self.p.state();
        match *ev {
            Event::MouseMove { x, y } => {
                if self.p.transport.mouse_move(cx, x, y, &st) {
                    cx.redraw = true;
                }
            }
            Event::MouseLeave => {
                cx.redraw |= self.p.transport.mouse_leave();
            }
            Event::MouseDown { x, y, .. } => {
                if !self.p.transport.mouse_down(cx, x, y, &st) && r.contains(x, y) {
                    let (art, _, _) = self.layout(r, cx.scale);
                    if art.contains(x, y) {
                        let p = !(st.paused || st.eof);
                        engine::with_existing(|e| e.set_pause(p));
                        cx.redraw = true;
                    }
                }
            }
            Event::MouseUp { x, .. } => {
                self.p.transport.mouse_up(cx, x, &st);
            }
            Event::Wheel { x, y, lines, horizontal: false, .. } => {
                if !self.p.transport.wheel(x, y, lines, &st) {
                    transport::seek_by(&st, lines as f64 * 5.0);
                }
                cx.redraw = true;
            }
            Event::Key { vk, .. } => {
                if transport::key(vk, &st) {
                    cx.redraw = true;
                }
            }
            _ => {}
        }
    }

    fn info(&self) -> Vec<(String, String)> {
        let mut v = self.summary.as_ref().map(|s| s.facts()).unwrap_or_default();
        v.extend(self.extra_facts.iter().cloned());
        let st = self.p.state();
        if !st.audio_desc.is_empty() {
            v.push(("Output".into(), st.audio_desc.clone()));
        }
        v
    }

    fn subtitle(&self) -> Option<String> {
        let d = self.summary.as_ref().map(|s| s.duration).unwrap_or(self.duration_hint);
        (d > 0.0).then(|| transport::fmt_time(d, d))
    }
}
