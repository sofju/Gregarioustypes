//! Quick MediaInfo probe: what the window needs before playback starts (display
//! size, audio-only or not, tags and cover art) and the Info panel facts.

use crate::mediainfo::{MediaInfo, Stream};
use gt_core::fmt;
use std::path::Path;
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct VideoTrack {
    pub format: String,
    pub profile: String,
    pub width: u32,
    pub height: u32,
    /// Display aspect ratio (0 when unknown).
    pub dar: f64,
    pub rotation: f64,
    pub fps: f64,
    pub bit_depth: u32,
    pub hdr: String,
    pub transfer: String,
    pub primaries: String,
    pub max_cll: String,
    pub mastering: String,
    pub bitrate: f64,
    pub scan: String,
    pub chroma: String,
    pub title: String,
}

#[derive(Clone, Debug, Default)]
pub struct AudioTrack {
    pub format: String,
    pub commercial: String,
    pub channels: u32,
    pub rate: f64,
    pub bit_depth: u32,
    pub bitrate: f64,
    pub language: String,
    pub title: String,
    pub mode: String,
}

#[derive(Clone, Debug, Default)]
pub struct TextTrack {
    pub format: String,
    pub language: String,
    pub title: String,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub container: String,
    pub profile: String,
    /// Seconds (0 when unknown).
    pub duration: f64,
    pub bitrate: f64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track: String,
    pub date: String,
    pub genre: String,
    pub composer: String,
    pub comment: String,
    /// First embedded picture (JPEG/PNG bytes).
    pub cover: Option<Vec<u8>>,
    pub video: Vec<VideoTrack>,
    pub audio: Vec<AudioTrack>,
    pub text: Vec<TextTrack>,
    pub chapters: usize,
    pub elapsed_ms: f64,
}

fn num(s: &str) -> f64 {
    let s = s.split(" / ").next().unwrap_or("").trim();
    s.parse::<f64>().unwrap_or(0.0)
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        })
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn probe(path: &Path) -> Result<Summary, String> {
    let t = Instant::now();
    let mut mi = MediaInfo::new()?;
    mi.option("ParseSpeed", "0");
    mi.option("Cover_Data", "base64");
    if !mi.open(path) {
        return Err("MediaInfo does not recognise this file".into());
    }
    let g = |p: &str| mi.get(Stream::General, 0, p);
    let mut s = Summary {
        container: g("Format"),
        profile: g("Format_Profile"),
        duration: num(&g("Duration")) / 1000.0,
        bitrate: num(&g("OverallBitRate")),
        title: g("Title"),
        artist: g("Performer"),
        album: g("Album"),
        album_artist: g("Album/Performer"),
        track: {
            let (p, t) = (g("Track/Position"), g("Track/Position_Total"));
            if !p.is_empty() && !t.is_empty() {
                format!("{p} of {t}")
            } else {
                p
            }
        },
        date: {
            let d = g("Recorded_Date");
            if d.is_empty() {
                g("Released_Date")
            } else {
                d
            }
        },
        genre: g("Genre"),
        composer: g("Composer"),
        comment: g("Comment"),
        ..Default::default()
    };
    if s.title.is_empty() {
        s.title = g("Movie");
    }
    if s.title.is_empty() {
        s.title = g("Track");
    }
    let cover = g("Cover_Data");
    if !cover.is_empty() {
        s.cover = cover.split(" / ").next().and_then(base64_decode).filter(|b| b.len() > 16);
    }
    for i in 0..mi.count(Stream::Video) {
        let v = |p: &str| mi.get(Stream::Video, i, p);
        let mut hdr = v("HDR_Format/String");
        if hdr.is_empty() {
            hdr = v("HDR_Format");
        }
        s.video.push(VideoTrack {
            format: v("Format"),
            profile: v("Format_Profile"),
            width: num(&v("Width")) as u32,
            height: num(&v("Height")) as u32,
            dar: num(&v("DisplayAspectRatio")),
            rotation: num(&v("Rotation")),
            fps: num(&v("FrameRate")),
            bit_depth: num(&v("BitDepth")) as u32,
            hdr,
            transfer: v("transfer_characteristics"),
            primaries: v("colour_primaries"),
            max_cll: v("MaxCLL"),
            mastering: v("MasteringDisplay_Luminance"),
            bitrate: num(&v("BitRate")),
            scan: v("ScanType"),
            chroma: v("ChromaSubsampling"),
            title: v("Title"),
        });
    }
    for i in 0..mi.count(Stream::Audio) {
        let a = |p: &str| mi.get(Stream::Audio, i, p);
        s.audio.push(AudioTrack {
            format: a("Format"),
            commercial: a("Format_Commercial_IfAny"),
            channels: num(&a("Channel(s)")) as u32,
            rate: num(&a("SamplingRate")),
            bit_depth: num(&a("BitDepth")) as u32,
            bitrate: num(&a("BitRate")),
            language: a("Language/String"),
            title: a("Title"),
            mode: a("Compression_Mode"),
        });
    }
    for i in 0..mi.count(Stream::Text) {
        let x = |p: &str| mi.get(Stream::Text, i, p);
        s.text.push(TextTrack { format: x("Format"), language: x("Language/String"), title: x("Title") });
    }
    if mi.count(Stream::Menu) > 0 {
        let (b, e) = (num(&mi.get(Stream::Menu, 0, "Chapters_Pos_Begin")), num(&mi.get(Stream::Menu, 0, "Chapters_Pos_End")));
        s.chapters = (e - b).max(0.0) as usize;
    }
    if s.duration <= 0.0 {
        s.duration = s.video.first().map(|_| num(&mi.get(Stream::Video, 0, "Duration")) / 1000.0).unwrap_or(0.0);
    }
    if s.duration <= 0.0 && !s.audio.is_empty() {
        s.duration = num(&mi.get(Stream::Audio, 0, "Duration")) / 1000.0;
    }
    s.elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;
    Ok(s)
}

impl VideoTrack {
    /// Size as displayed: anamorphic pixels stretched, rotation applied.
    pub fn display_size(&self) -> (f64, f64) {
        let (w, h) = (self.width.max(1) as f64, self.height.max(1) as f64);
        let w = if self.dar > 0.1 { (h * self.dar).round() } else { w };
        let r = self.rotation.rem_euclid(180.0);
        if (r - 90.0).abs() < 1.0 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// "HDR10", "HLG", "Dolby Vision" or "" for SDR.
    pub fn hdr_kind(&self) -> &'static str {
        let h = self.hdr.to_ascii_lowercase();
        if h.contains("dolby vision") {
            "Dolby Vision"
        } else if h.contains("hdr10+") || h.contains("2094") {
            "HDR10+"
        } else if self.transfer.contains("PQ") || self.transfer.contains("2084") || h.contains("2086") {
            "HDR10"
        } else if self.transfer.contains("HLG") {
            "HLG"
        } else {
            ""
        }
    }
}

pub fn channels_name(n: u32) -> String {
    match n {
        0 => String::new(),
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n} ch"),
    }
}

fn khz(rate: f64) -> String {
    if rate <= 0.0 {
        String::new()
    } else if (rate / 1000.0).fract() == 0.0 {
        format!("{} kHz", rate / 1000.0)
    } else {
        format!("{:.1} kHz", rate / 1000.0)
    }
}

pub fn kbps(b: f64) -> String {
    if b <= 0.0 {
        String::new()
    } else if b >= 10_000_000.0 {
        format!("{:.1} Mb/s", b / 1e6)
    } else {
        format!("{} kb/s", (b / 1000.0).round())
    }
}

fn join(parts: &[String]) -> String {
    parts.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

impl AudioTrack {
    pub fn describe(&self) -> String {
        let fmt = if !self.commercial.is_empty() && self.commercial != self.format { self.commercial.clone() } else { self.format.clone() };
        let bits = if self.bit_depth > 0 && (self.mode == "Lossless" || self.format.starts_with("PCM") || self.format == "FLAC") { format!("{}-bit", self.bit_depth) } else { String::new() };
        join(&[fmt, channels_name(self.channels), khz(self.rate), bits, kbps(self.bitrate)])
    }
}

impl VideoTrack {
    pub fn describe(&self) -> String {
        let (w, h) = self.display_size();
        let codec = if self.profile.is_empty() { self.format.clone() } else { format!("{} {}", self.format, self.profile.split('@').next().unwrap_or("")) };
        let fps = if self.fps > 0.0 { format!("{} fps", trim_num(self.fps)) } else { String::new() };
        let bits = if self.bit_depth > 8 { format!("{}-bit", self.bit_depth) } else { String::new() };
        join(&[format!("{}×{}", w as u32, h as u32), codec, fps, bits])
    }
}

fn trim_num(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Summary {
    pub fn has_video(&self) -> bool {
        self.video.iter().any(|v| v.width > 0)
    }

    /// Facts for the Info panel.
    pub fn facts(&self) -> Vec<(String, String)> {
        let mut f = Vec::new();
        let mut push = |k: &str, v: String| {
            if !v.is_empty() {
                f.push((k.to_string(), v));
            }
        };
        if self.duration > 0.0 {
            push("Duration", fmt::duration(self.duration));
        }
        push("Format", join(&[self.container.clone(), self.profile.clone(), kbps(self.bitrate)]));
        for (i, v) in self.video.iter().enumerate() {
            push(if i == 0 { "Video" } else { "Video (more)" }, v.describe());
            let kind = v.hdr_kind();
            if !kind.is_empty() {
                let mut d = vec![if v.hdr.is_empty() { kind.to_string() } else { v.hdr.clone() }];
                d.push(v.transfer.clone());
                d.push(v.primaries.clone());
                if !v.max_cll.is_empty() {
                    d.push(format!("MaxCLL {}", v.max_cll));
                }
                push("HDR", join(&d));
            }
        }
        for (i, a) in self.audio.iter().enumerate() {
            let mut d = a.describe();
            let tag = join(&[a.language.clone(), a.title.clone()]);
            if !tag.is_empty() {
                d = format!("{d} ({tag})");
            }
            push(if self.audio.len() > 1 { "Audio track" } else { "Audio" }, d);
            let _ = i;
        }
        if !self.text.is_empty() {
            let subs: Vec<String> = self.text.iter().map(|t| join(&[t.format.clone(), t.language.clone()])).collect();
            push("Subtitles", subs.join(", "));
        }
        if self.chapters > 0 {
            push("Chapters", self.chapters.to_string());
        }
        push("Title", self.title.clone());
        push("Artist", self.artist.clone());
        push("Album", self.album.clone());
        if self.album_artist != self.artist {
            push("Album artist", self.album_artist.clone());
        }
        push("Track", self.track.clone());
        push("Year", self.date.clone());
        push("Genre", self.genre.clone());
        push("Composer", self.composer.clone());
        push("Comment", self.comment.clone());
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64() {
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64_decode("AAEC/w==").unwrap(), vec![0, 1, 2, 255]);
    }

    #[test]
    fn display_size() {
        let v = VideoTrack { width: 720, height: 576, dar: 16.0 / 9.0, ..Default::default() };
        assert_eq!(v.display_size(), (1024.0, 576.0));
        let r = VideoTrack { width: 1280, height: 720, rotation: 90.0, ..Default::default() };
        assert_eq!(r.display_size(), (720.0, 1280.0));
    }
}
