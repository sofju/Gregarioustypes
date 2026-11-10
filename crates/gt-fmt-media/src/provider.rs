//! Providers: audio/video, MIDI, playlists and CD track stubs.

use crate::probe::{self, Summary};
use crate::special::{self, Drm};
use crate::view::{AudioView, Source, VideoView};
use crate::{cover, midi, playlist, AUDIO_EXTS, VIDEO_EXTS};
use gt_core::CancelToken;
use gt_shell::info::Bgra;
use gt_ui::widgets::card::{CardData, InfoCard};
use gt_ui::{glyph, Loaded, Probe, Provider, View, ViewEnv};

fn is_dts_sync(h: &[u8]) -> bool {
    h.starts_with(&[0x7F, 0xFE, 0x80, 0x01]) || h.starts_with(&[0xFE, 0x7F, 0x01, 0x80]) || h.starts_with(&[0x1F, 0xFF, 0xE8, 0x00]) || h.starts_with(&[0x64, 0x58, 0x20, 0x25])
}

fn looks_textual(h: &[u8]) -> bool {
    let n = h.len().min(512);
    n > 0 && h[..n].iter().all(|&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7F).contains(&b))
}

pub struct MediaProvider;

impl Provider for MediaProvider {
    fn id(&self) -> &'static str {
        "media"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        let e = p.ext.as_str();
        match e {
            "ts" => {
                if gt_core::sniff::is_mpeg_ts(&p.head) {
                    90
                } else {
                    0
                }
            }
            "dts" => {
                if is_dts_sync(&p.head) || !looks_textual(&p.head) {
                    70
                } else {
                    0
                }
            }
            // Camera raw wins when LibRaw recognises the file; headerless PCM otherwise.
            "raw" => 15,
            "ifo" => {
                if special::is_dvd_ifo(&p.head) {
                    85
                } else {
                    0
                }
            }
            "aa" => 80,
            _ if VIDEO_EXTS.contains(&e) || AUDIO_EXTS.contains(&e) => 60,
            _ => 0,
        }
    }

    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let path = p.path.display().to_string();
        let name = p.name();
        if let Some(drm) = special::detect_drm(&p.path, &p.head) {
            let summary = probe::probe(&p.path).ok();
            let cover = summary.as_ref().and_then(|s| s.cover.as_deref()).and_then(|b| cover::decode_bytes(b, 1024));
            let mut facts = Vec::new();
            if drm == Drm::Audible {
                for (k, v) in special::audible_tags(&p.path) {
                    if matches!(k.as_str(), "title" | "author" | "narrator" | "pubdate" | "provider" | "short_title") {
                        facts.push((k, v));
                    }
                }
            }
            return Ok(Box::new(MediaLoaded { kind: Kind::Protected(drm, facts), src: Source::file(&path), summary, image: cover, name }));
        }
        if p.ext == "raw" {
            // Headerless PCM: guess CD audio (16-bit stereo 44.1 kHz).
            let mut src = Source::file(&path);
            src.extra = vec!["demuxer=rawaudio".into(), "demuxer-rawaudio-rate=44100".into(), "demuxer-rawaudio-channels=2".into(), "demuxer-rawaudio-format=s16le".into()];
            let mut s = Summary { container: "Raw PCM (assumed 16-bit stereo 44.1 kHz)".into(), ..Default::default() };
            s.duration = p.size as f64 / (44100.0 * 4.0);
            return Ok(Box::new(MediaLoaded { kind: Kind::Audio, src, summary: Some(s), image: None, name }));
        }
        let summary = match probe::probe(&p.path) {
            Ok(s) => {
                tracing::info!("MediaInfo probe {:.1} ms: {} video, {} audio", s.elapsed_ms, s.video.len(), s.audio.len());
                Some(s)
            }
            Err(e) => {
                tracing::warn!("MediaInfo: {e}");
                None
            }
        };
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let mut src = Source::file(&path);
        if p.ext == "ifo" {
            src.url = "dvd://".into();
            if let Some(dev) = special::dvd_device(&p.path) {
                src.extra.push(format!("dvd-device={dev}"));
            }
        }
        let has_video = match &summary {
            Some(s) => s.has_video() || p.ext == "ifo",
            None => VIDEO_EXTS.contains(&p.ext.as_str()),
        };
        if has_video {
            // Explorer's cached thumbnail (fetched as a second stage) covers the gap
            // until the first frame.
            Ok(Box::new(MediaLoaded { kind: Kind::Video, src, summary, image: None, name }))
        } else {
            let mut img = summary.as_ref().and_then(|s| s.cover.as_deref()).and_then(|b| cover::decode_bytes(b, 1024));
            if img.is_none() {
                img = cover::folder_art(&p.path).and_then(|f| cover::decode_file(&f, 1024));
            }
            Ok(Box::new(MediaLoaded { kind: Kind::Audio, src, summary, image: img, name }))
        }
    }
}

enum Kind {
    Video,
    Audio,
    Protected(Drm, Vec<(String, String)>),
}

struct MediaLoaded {
    kind: Kind,
    src: Source,
    summary: Option<Summary>,
    image: Option<Bgra>,
    name: String,
}

impl Loaded for MediaLoaded {
    fn take_upgrade(&mut self) -> Option<gt_ui::Upgrade> {
        if !matches!(self.kind, Kind::Video) {
            return None;
        }
        let path = self.src.path.clone();
        Some(Box::new(move |_cancel: &CancelToken| gt_shell::info::image(&path, 512, true).map(|b| Box::new(b) as Box<dyn std::any::Any + Send>)))
    }

    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        let m = *self;
        match m.kind {
            Kind::Video => Box::new(VideoView::new(env, m.src, m.summary, m.image)),
            Kind::Audio => Box::new(AudioView::new(env, m.src, m.summary, m.image, &m.name)),
            Kind::Protected(drm, facts) => {
                let video = m.summary.as_ref().is_some_and(|s| s.has_video());
                let mut v = AudioView::new(env, m.src, m.summary, m.image, &m.name).not_playable(drm.message()).with_glyph(if video { glyph::VIDEO } else { glyph::MUSIC });
                v.add_facts(std::iter::once(("Protection".to_string(), drm.name().to_string())).chain(facts).collect());
                Box::new(v)
            }
        }
    }
}

// ------------------------------------------------------------------ MIDI

pub struct MidiProvider;

impl Provider for MidiProvider {
    fn id(&self) -> &'static str {
        "midi"
    }

    fn score(&self, p: &Probe) -> u32 {
        let midi_ext = matches!(p.ext.as_str(), "mid" | "midi" | "rmi" | "rmid" | "rim" | "kar" | "smf");
        let magic = p.head.starts_with(b"MThd") || (p.head.len() > 12 && &p.head[0..4] == b"RIFF" && &p.head[8..12] == b"RMID");
        match (midi_ext, magic) {
            (_, true) => 95,
            (true, false) => 40,
            _ => 0,
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let data = std::fs::read(&p.path).map_err(|e| e.to_string())?;
        let smf = midi::smf_bytes(&data).ok_or("not a Standard MIDI File")?;
        let info = midi::parse_info(smf).ok_or("unreadable MIDI header")?;
        let prepared = midi::prepare(smf);
        Ok(Box::new(MidiLoaded { info, prepared, name: p.name(), path: p.path.display().to_string() }))
    }
}

struct MidiLoaded {
    info: midi::MidiInfo,
    prepared: Result<(String, f64), String>,
    name: String,
    path: String,
}

impl Loaded for MidiLoaded {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        let m = *self;
        let i = &m.info;
        let duration = m.prepared.as_ref().map(|(_, d)| *d).unwrap_or(0.0);
        let title = if i.title.is_empty() { m.name.clone() } else { i.title.clone() };
        let mut lines = vec![format!("MIDI type {} · {} track{} · {:.0} bpm", i.format, i.tracks, if i.tracks == 1 { "" } else { "s" }, i.tempo_bpm)];
        let instruments: Vec<String> = i.instruments.iter().map(|(_, n)| n.clone()).collect();
        if !instruments.is_empty() {
            lines.push(instruments.join(" · "));
        }
        if !i.copyright.is_empty() {
            lines.push(i.copyright.clone());
        }
        let mut facts = vec![("Duration".to_string(), gt_core::fmt::duration(duration)), ("Format".into(), format!("Standard MIDI File, type {}", i.format)), ("Tracks".into(), i.tracks.to_string()), ("Tempo".into(), format!("{:.1} bpm", i.tempo_bpm))];
        if !i.time_sig.is_empty() {
            facts.push(("Time signature".into(), i.time_sig.clone()));
        }
        if !i.key_sig.is_empty() {
            facts.push(("Key".into(), i.key_sig.clone()));
        }
        facts.push(("Notes".into(), gt_core::fmt::group(i.notes)));
        for (ch, n) in &i.instruments {
            facts.push((format!("Channel {ch}"), n.clone()));
        }
        if !i.track_names.is_empty() {
            facts.push(("Track names".into(), i.track_names.join(", ")));
        }
        match m.prepared {
            Ok((url, _)) => {
                let src = Source { path: m.path, url, extra: vec!["demuxer=lavf".into()] };
                let mut v = AudioView::new(env, src, None, None, &title).with_lines(title.clone(), lines).with_glyph(glyph::MUSIC);
                v.add_facts(facts);
                v.set_duration(duration);
                Box::new(v)
            }
            Err(e) => {
                let mut v = AudioView::new(env, Source::file(&m.path), None, None, &title).with_lines(title.clone(), lines).not_playable(&format!("Can't synthesise: {e}"));
                v.add_facts(facts);
                Box::new(v)
            }
        }
    }
}

// ------------------------------------------------------------------ playlists

pub struct PlaylistProvider;

impl Provider for PlaylistProvider {
    fn id(&self) -> &'static str {
        "playlist"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !playlist::EXTS.contains(&p.ext.as_str()) {
            return 0;
        }
        75
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let bytes = if p.complete() { p.head.clone() } else { std::fs::read(&p.path).map_err(|e| e.to_string())? };
        let pl = playlist::parse(&p.path, &bytes).ok_or("no entries found in this playlist")?;
        Ok(Box::new(PlaylistLoaded(pl)))
    }
}

struct PlaylistLoaded(playlist::Playlist);

impl Loaded for PlaylistLoaded {
    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        Box::new(playlist::PlaylistView::new(self.0))
    }
}

// ------------------------------------------------------------------ CD tracks

pub struct CdaProvider;

impl Provider for CdaProvider {
    fn id(&self) -> &'static str {
        "cda"
    }

    fn score(&self, p: &Probe) -> u32 {
        if special::parse_cda(&p.head).is_some() {
            95
        } else {
            0
        }
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let t = special::parse_cda(&p.head).ok_or("not a CD track file")?;
        let icon = gt_shell::info::image(&p.path.display().to_string(), 256, false);
        Ok(Box::new(CdaLoaded { t, name: p.name(), path: p.path.clone(), icon }))
    }
}

struct CdaLoaded {
    t: special::CdaTrack,
    name: String,
    path: std::path::PathBuf,
    icon: Option<Bgra>,
}

impl Loaded for CdaLoaded {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        let t = &self.t;
        let facts = vec![
            ("Track".to_string(), t.track.to_string()),
            ("Length".into(), crate::transport::fmt_time(t.seconds(), t.seconds())),
            ("Starts at".into(), crate::transport::fmt_time(t.start_seconds(), t.start_seconds())),
            ("Disc ID".into(), format!("{:08X}", t.serial)),
        ];
        if special::is_cd_drive(&self.path) {
            let drive = self.path.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
            let src = Source { path: self.path.display().to_string(), url: format!("cdda://{}", t.track), extra: vec![format!("cdda-device={drive}")] };
            let title = format!("Audio CD track {}", t.track);
            let mut v = AudioView::new(env, src, None, None, &title).with_glyph(glyph::DISC).with_lines(title.clone(), vec![crate::transport::fmt_time(t.seconds(), t.seconds())]);
            v.add_facts(facts);
            v.set_duration(t.seconds());
            return Box::new(v);
        }
        let icon = self.icon.map(|b| (b.width, b.height, b.data));
        Box::new(InfoCard::new(
            &env.gfx.dc,
            CardData {
                icon,
                name: self.name,
                kind: "Audio CD track".into(),
                facts,
                note: Some("This is Windows' placeholder for a track on an audio CD; the music is read from the disc itself.".into()),
            },
        ))
    }
}
