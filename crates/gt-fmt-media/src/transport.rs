//! The playback control bar shared by video, audio, MIDI and playlist views.
//! Drawn on the UI layer, so it composes above the video window.

use crate::engine::{self, PlayerState};
use gt_ui::cx::Cursor;
use gt_ui::{glyph, rgba, Align, DrawCx, EventCx, Font, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Play,
    Seek,
    Mute,
    Volume,
    Audio,
    Subs,
    Bar,
}

#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    bar: Rect,
    play: Rect,
    time: Rect,
    seek: Rect,
    remain: Rect,
    mute: Rect,
    volume: Rect,
    audio: Option<Rect>,
    subs: Option<Rect>,
}

#[derive(Default)]
pub struct Transport {
    l: Layout,
    hover: Option<Part>,
    hover_x: f32,
    drag: Option<Part>,
    /// Playback position shown while dragging the seek bar.
    drag_t: Option<f64>,
    resume_after_drag: bool,
}

pub fn fmt_time(t: f64, total: f64) -> String {
    let t = t.max(0.0) as u64;
    if total >= 3600.0 {
        format!("{}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

/// Bar height in pixels.
pub fn height(scale: f32) -> f32 {
    (44.0 * scale).round()
}

impl Transport {
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn bar(&self) -> Rect {
        self.l.bar
    }

    /// Lay out inside `bar` (the rounded panel).
    fn layout(&mut self, cx: &mut DrawCx, bar: Rect, st: &PlayerState) {
        let sc = cx.scale;
        let p = |d: f32| (d * sc).round();
        let mut l = Layout { bar, ..Default::default() };
        let cy = bar.y;
        let h = bar.h;
        let btn = p(36.0);
        l.play = Rect::new(bar.x + p(6.0), cy + (h - btn) / 2.0, btn, btn);
        let mut right = bar.right() - p(8.0);
        let wide = bar.w > p(440.0);
        if !st.tracks_of("sub").is_empty() {
            right -= p(32.0);
            l.subs = Some(Rect::new(right, cy + (h - p(32.0)) / 2.0, p(32.0), p(32.0)));
        }
        if st.tracks_of("audio").len() > 1 {
            right -= p(32.0);
            l.audio = Some(Rect::new(right, cy + (h - p(32.0)) / 2.0, p(32.0), p(32.0)));
        }
        if wide {
            right -= p(70.0);
            l.volume = Rect::new(right, cy, p(64.0), h);
        }
        right -= p(32.0);
        l.mute = Rect::new(right, cy + (h - p(32.0)) / 2.0, p(32.0), p(32.0));
        let total = if st.duration > 0.0 { st.duration } else { 0.0 };
        let tw = cx.measure(&fmt_time(total.max(600.0), total), Font::Ui(12.0)).0 + p(4.0);
        right -= p(6.0);
        l.remain = Rect::new(right - tw, cy, tw, h);
        right = l.remain.x - p(10.0);
        l.time = Rect::new(l.play.right() + p(4.0), cy, tw, h);
        let sx = l.time.right() + p(10.0);
        l.seek = Rect::new(sx, cy, (right - sx).max(p(40.0)), h);
        self.l = l;
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Part> {
        let l = &self.l;
        if !l.bar.contains(x, y) {
            return None;
        }
        let near = |r: Rect| r.inset_xy(0.0, -4.0).contains(x, y);
        Some(if l.play.contains(x, y) {
            Part::Play
        } else if l.seek.contains(x, y) {
            Part::Seek
        } else if near(l.mute) {
            Part::Mute
        } else if l.volume.contains(x, y) {
            Part::Volume
        } else if l.audio.is_some_and(near) {
            Part::Audio
        } else if l.subs.is_some_and(near) {
            Part::Subs
        } else {
            Part::Bar
        })
    }

    fn seek_t(&self, x: f32, st: &PlayerState) -> f64 {
        let s = &self.l.seek;
        let f = ((x - s.x) / s.w.max(1.0)).clamp(0.0, 1.0) as f64;
        f * st.duration.max(0.0)
    }

    pub fn draw(&mut self, cx: &mut DrawCx, bar: Rect, st: &PlayerState) {
        self.layout(cx, bar, st);
        let l = self.l;
        let sc = cx.scale;
        let p = |d: f32| (d * sc).round();
        cx.round(l.bar, p(10.0), rgba(0.07, 0.07, 0.075, 0.82));
        let white = rgba(1.0, 1.0, 1.0, 0.95);
        let dim = rgba(1.0, 1.0, 1.0, 0.6);
        let hover_bg = |cx: &mut DrawCx, r: Rect, on: bool| {
            if on {
                cx.round(r, r.h / 2.0, rgba(1.0, 1.0, 1.0, 0.12));
            }
        };
        hover_bg(cx, l.play, self.hover == Some(Part::Play));
        let playing = !st.paused && !st.eof;
        cx.glyph(if playing { glyph::PAUSE } else { glyph::PLAY }, l.play, 16.0, white);
        let shown_t = self.drag_t.unwrap_or(st.time);
        cx.text(&fmt_time(shown_t, st.duration), l.time, Font::Ui(12.0), white, Align::Right);
        let remain = if st.duration > 0.0 { format!("-{}", fmt_time((st.duration - shown_t).max(0.0), st.duration)) } else { "--:--".into() };
        cx.text(&remain, l.remain, Font::Ui(12.0), dim, Align::Left);
        // Seek track.
        let s = l.seek;
        let th = p(4.0);
        let track = Rect::new(s.x, s.y + (s.h - th) / 2.0, s.w, th);
        cx.round(track, th / 2.0, rgba(1.0, 1.0, 1.0, 0.22));
        if st.duration > 0.0 {
            let frac = |t: f64| ((t / st.duration).clamp(0.0, 1.0)) as f32;
            if st.cache_time > shown_t {
                let x0 = track.x + track.w * frac(shown_t);
                let x1 = track.x + track.w * frac(st.cache_time);
                cx.round(Rect::new(x0, track.y, (x1 - x0).max(0.0), th), th / 2.0, rgba(1.0, 1.0, 1.0, 0.18));
            }
            let px = track.x + track.w * frac(shown_t);
            cx.round(Rect::new(track.x, track.y, (px - track.x).max(0.0), th), th / 2.0, white);
            for (t, _) in &st.chapters {
                if *t > 0.5 {
                    let x = (track.x + track.w * frac(*t)).round();
                    cx.fill(Rect::new(x - 1.0, track.y, p(2.0), th), rgba(0.0, 0.0, 0.0, 0.7));
                }
            }
            let active = matches!(self.hover, Some(Part::Seek)) || self.drag == Some(Part::Seek);
            if active {
                let k = p(12.0);
                cx.round(Rect::new(px - k / 2.0, track.y + th / 2.0 - k / 2.0, k, k), k / 2.0, white);
                // Time under the cursor.
                let t = self.drag_t.unwrap_or_else(|| self.seek_t(self.hover_x, st));
                let mut label = fmt_time(t, st.duration);
                if let Some((_, name)) = st.chapters.iter().rev().find(|(ct, _)| *ct <= t) {
                    if !name.is_empty() {
                        label = format!("{label}  {name}");
                    }
                }
                let (tw, tth) = cx.measure(&label, Font::Ui(12.0));
                let x = if self.drag.is_some() { px } else { self.hover_x };
                let tip = Rect::new((x - tw / 2.0 - p(8.0)).clamp(l.bar.x, l.bar.right() - tw - p(16.0)), l.bar.y - tth - p(16.0), tw + p(16.0), tth + p(8.0));
                cx.round(tip, p(5.0), rgba(0.07, 0.07, 0.075, 0.9));
                cx.text(&label, tip, Font::Ui(12.0), white, Align::Center);
            }
        }
        hover_bg(cx, l.mute, self.hover == Some(Part::Mute));
        let muted = st.mute || st.volume <= 0.0;
        cx.glyph(if muted { glyph::VOLUME_MUTE } else { glyph::VOLUME }, l.mute, 15.0, white);
        if l.volume.w > 0.0 {
            let v = l.volume;
            let vt = Rect::new(v.x, v.y + (v.h - th) / 2.0, v.w, th);
            cx.round(vt, th / 2.0, rgba(1.0, 1.0, 1.0, 0.22));
            let f = if muted { 0.0 } else { (st.volume / 100.0).clamp(0.0, 1.0) as f32 };
            cx.round(Rect::new(vt.x, vt.y, vt.w * f, th), th / 2.0, white);
            if matches!(self.hover, Some(Part::Volume)) || self.drag == Some(Part::Volume) {
                let k = p(10.0);
                let x = vt.x + vt.w * f;
                cx.round(Rect::new(x - k / 2.0, vt.y + th / 2.0 - k / 2.0, k, k), k / 2.0, white);
            }
        }
        if let Some(r) = l.audio {
            hover_bg(cx, r, self.hover == Some(Part::Audio));
            cx.glyph(glyph::AUDIO_TRACK, r, 15.0, white);
        }
        if let Some(r) = l.subs {
            hover_bg(cx, r, self.hover == Some(Part::Subs));
            let on = st.tracks_of("sub").iter().any(|t| t.selected);
            cx.glyph(glyph::SUBTITLES, r, 15.0, if on { white } else { dim });
        }
    }

    /// Mouse moved; returns true when the bar needs a redraw.
    pub fn mouse_move(&mut self, cx: &mut EventCx, x: f32, y: f32, st: &PlayerState) -> bool {
        if let Some(d) = self.drag {
            match d {
                Part::Seek => {
                    let t = self.seek_t(x, st);
                    self.drag_t = Some(t);
                    engine::with_existing(|e| e.seek(t, false));
                }
                Part::Volume => {
                    let v = ((x - self.l.volume.x) / self.l.volume.w.max(1.0)).clamp(0.0, 1.0) as f64 * 100.0;
                    engine::with_existing(|e| e.set_volume(v));
                }
                _ => {}
            }
            cx.cursor = Cursor::Hand;
            return true;
        }
        let h = self.hit(x, y);
        let changed = h != self.hover || (h == Some(Part::Seek) && (x - self.hover_x).abs() >= 1.0);
        self.hover = h;
        self.hover_x = x;
        if matches!(h, Some(Part::Play | Part::Seek | Part::Mute | Part::Volume | Part::Audio | Part::Subs)) {
            cx.cursor = Cursor::Hand;
        }
        changed
    }

    pub fn mouse_leave(&mut self) -> bool {
        let had = self.hover.is_some();
        self.hover = None;
        had
    }

    /// Returns true when the press landed on the bar (consumed).
    pub fn mouse_down(&mut self, cx: &mut EventCx, x: f32, y: f32, st: &PlayerState) -> bool {
        let Some(part) = self.hit(x, y) else { return false };
        match part {
            Part::Play => {
                let p = !(st.paused || st.eof);
                engine::with_existing(|e| e.set_pause(p));
            }
            Part::Seek if st.duration > 0.0 => {
                self.drag = Some(Part::Seek);
                self.resume_after_drag = !st.paused;
                let t = self.seek_t(x, st);
                self.drag_t = Some(t);
                engine::with_existing(|e| {
                    if !st.paused {
                        e.set_pause(true);
                    }
                    e.seek(t, false)
                });
                cx.capture = Some(true);
            }
            Part::Mute => {
                let m = !st.mute;
                engine::with_existing(|e| e.set_mute(m));
            }
            Part::Volume => {
                self.drag = Some(Part::Volume);
                let v = ((x - self.l.volume.x) / self.l.volume.w.max(1.0)).clamp(0.0, 1.0) as f64 * 100.0;
                engine::with_existing(|e| e.set_volume(v));
                cx.capture = Some(true);
            }
            Part::Audio => {
                let tracks = st.tracks_of("audio");
                if !tracks.is_empty() {
                    let cur = tracks.iter().position(|t| t.selected).unwrap_or(0);
                    let next = tracks[(cur + 1) % tracks.len()];
                    let id = next.id;
                    engine::with_existing(|e| e.set_track("audio", Some(id)));
                    cx.toast = Some(format!("Audio {}/{}: {}", (cur + 1) % tracks.len() + 1, tracks.len(), next.label()));
                }
            }
            Part::Subs => {
                let tracks = st.tracks_of("sub");
                let cur = tracks.iter().position(|t| t.selected);
                let next = match cur {
                    None => Some(0),
                    Some(i) if i + 1 < tracks.len() => Some(i + 1),
                    Some(_) => None,
                };
                let id = next.map(|i| tracks[i].id);
                engine::with_existing(|e| e.set_track("sub", id));
                cx.toast = Some(match next {
                    Some(i) => format!("Subtitles: {}", tracks[i].label()),
                    None => "Subtitles off".into(),
                });
            }
            _ => {}
        }
        cx.redraw = true;
        true
    }

    pub fn mouse_up(&mut self, cx: &mut EventCx, x: f32, st: &PlayerState) -> bool {
        let Some(d) = self.drag.take() else { return false };
        cx.capture = Some(false);
        if d == Part::Seek {
            let t = self.seek_t(x, st);
            let resume = self.resume_after_drag;
            engine::with_existing(|e| {
                e.seek(t, true);
                if resume {
                    e.set_pause(false);
                }
            });
        }
        self.drag_t = None;
        cx.redraw = true;
        true
    }

    /// Mouse wheel over the bar: volume on the volume controls, seeking elsewhere.
    pub fn wheel(&mut self, x: f32, y: f32, lines: f32, st: &PlayerState) -> bool {
        match self.hit(x, y) {
            Some(Part::Mute | Part::Volume) => {
                let v = st.volume + lines as f64 * 5.0;
                engine::with_existing(|e| e.set_volume(v));
                true
            }
            Some(_) => {
                seek_by(st, lines as f64 * 5.0);
                true
            }
            None => false,
        }
    }
}

pub fn seek_by(st: &PlayerState, d: f64) {
    if st.duration > 0.0 {
        let t = (st.time + d).clamp(0.0, st.duration);
        engine::with_existing(|e| e.seek(t, true));
    }
}

/// Keyboard shortcuts shared by the media views (when the window has focus).
pub fn key(vk: u16, st: &PlayerState) -> bool {
    match vk {
        0x4B | 0x50 => {
            // K / P: play-pause
            let p = !(st.paused || st.eof);
            engine::with_existing(|e| e.set_pause(p));
        }
        0x4A => seek_by(st, -10.0), // J
        0x4C => seek_by(st, 10.0),  // L
        0x4D => {
            let m = !st.mute;
            engine::with_existing(|e| e.set_mute(m));
        }
        0x24 => seek_by(st, -st.time), // Home
        0xBC => engine::with_existing(|e| e.command(&["frame-back-step"])).unwrap_or(()), // ,
        0xBE => engine::with_existing(|e| e.command(&["frame-step"])).unwrap_or(()),      // .
        _ => return false,
    }
    true
}
