//! Blu-ray structure: playlists (MPLS) with durations, chapters and streams, and
//! the disc title from `META/DL/bdmt_*.xml`. Enough for an instant summary; the
//! full analysis is BDInfo's job.

#[derive(Clone, Debug, Default)]
pub struct Stream {
    pub kind: &'static str,
    pub codec: String,
    pub detail: String,
    pub lang: String,
}

#[derive(Clone, Debug, Default)]
pub struct Playlist {
    pub name: String,
    pub seconds: f64,
    pub clips: Vec<String>,
    pub chapters: usize,
    pub streams: Vec<Stream>,
    /// Kept by BDInfo's filter (not short, not looping).
    pub presented: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Disc {
    pub title: String,
    pub uhd: bool,
    pub playlists: Vec<Playlist>,
    /// Extra facts (volume label, features).
    pub facts: Vec<(String, String)>,
    /// The stream files are still AACS-encrypted.
    pub encrypted: bool,
}

impl Disc {
    /// BDInfo's first presented playlist, or else the longest one.
    pub fn main(&self) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.presented).or_else(|| self.playlists.iter().max_by(|a, b| a.seconds.partial_cmp(&b.seconds).unwrap_or(std::cmp::Ordering::Equal)))
    }
}

fn be16(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize)
}

fn be32(b: &[u8], i: usize) -> Option<u64> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?) as u64)
}

fn codec(t: u8) -> (&'static str, &'static str) {
    match t {
        0x01 => ("Video", "MPEG-1"),
        0x02 => ("Video", "MPEG-2"),
        0x1B => ("Video", "AVC"),
        0x20 => ("Video", "MVC"),
        0x24 => ("Video", "HEVC"),
        0xEA => ("Video", "VC-1"),
        0x80 => ("Audio", "LPCM"),
        0x81 => ("Audio", "Dolby Digital"),
        0x82 => ("Audio", "DTS"),
        0x83 => ("Audio", "Dolby TrueHD"),
        0x84 | 0xA1 => ("Audio", "Dolby Digital Plus"),
        0x85 => ("Audio", "DTS-HD High Res"),
        0x86 => ("Audio", "DTS-HD Master Audio"),
        0xA2 => ("Audio", "DTS Express"),
        0x90 => ("Subtitles", "PGS"),
        0x91 => ("Menu", "IGS"),
        0x92 => ("Subtitles", "Text"),
        _ => ("Other", "unknown"),
    }
}

fn video_detail(b: u8) -> String {
    let fmt = match b >> 4 {
        1 => "480i",
        2 => "576i",
        3 => "480p",
        4 => "1080i",
        5 => "720p",
        6 => "1080p",
        7 => "576p",
        8 => "2160p",
        _ => "",
    };
    let rate = match b & 0x0F {
        1 => "23.976",
        2 => "24",
        3 => "25",
        4 => "29.97",
        6 => "50",
        7 => "59.94",
        _ => "",
    };
    if rate.is_empty() {
        fmt.to_string()
    } else {
        format!("{fmt} {rate} fps")
    }
}

fn audio_detail(b: u8) -> String {
    let ch = match b >> 4 {
        1 => "mono",
        3 => "stereo",
        6 => "multi-channel",
        12 => "stereo + multi-channel",
        _ => "",
    };
    let rate = match b & 0x0F {
        1 => "48 kHz",
        4 => "96 kHz",
        5 => "192 kHz",
        12 => "48/192 kHz",
        14 => "48/96 kHz",
        _ => "",
    };
    [ch, rate].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

fn lang(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim_matches(char::from(0)).to_string()
}

/// Parse one `.mpls`.
pub fn parse_mpls(name: &str, b: &[u8]) -> Option<Playlist> {
    if b.len() < 40 || &b[0..4] != b"MPLS" {
        return None;
    }
    let pl = be32(b, 8)? as usize;
    let marks = be32(b, 12)? as usize;
    let n_items = be16(b, pl + 6)?;
    let mut p = Playlist { name: name.to_string(), ..Default::default() };
    let mut i = pl + 10;
    for item in 0..n_items {
        let len = be16(b, i)?;
        let start = i + 2;
        let clip = String::from_utf8_lossy(b.get(start..start + 5)?).to_string();
        let multi_angle = b.get(start + 10).map(|v| v & 0x10 != 0).unwrap_or(false);
        let tin = be32(b, start + 12)?;
        let tout = be32(b, start + 16)?;
        p.seconds += tout.saturating_sub(tin) as f64 / 45000.0;
        p.clips.push(format!("{clip}.m2ts"));
        if item == 0 {
            // STN table of the first item describes the streams.
            let mut s = start + 32;
            if multi_angle {
                let n_angles = *b.get(s)? as usize;
                s += 2 + (n_angles.saturating_sub(1)) * 10;
            }
            let counts = b.get(s + 4..s + 11)?;
            let total: usize = counts.iter().take(7).map(|&c| c as usize).sum();
            let mut q = s + 16;
            for _ in 0..total {
                let el = *b.get(q)? as usize;
                q += 1 + el;
                let al = *b.get(q)? as usize;
                let attr = b.get(q + 1..q + 1 + al)?;
                q += 1 + al;
                let Some(&t) = attr.first() else { continue };
                let (kind, name) = codec(t);
                let (detail, language) = match kind {
                    "Video" => (attr.get(1).map(|&v| video_detail(v)).unwrap_or_default(), String::new()),
                    "Audio" => (attr.get(1).map(|&v| audio_detail(v)).unwrap_or_default(), attr.get(2..5).map(lang).unwrap_or_default()),
                    "Subtitles" => (String::new(), attr.get(1..4).map(lang).unwrap_or_default()),
                    _ => (String::new(), String::new()),
                };
                p.streams.push(Stream { kind, codec: name.to_string(), detail, lang: language });
            }
        }
        i = start + len;
    }
    if let Some(n) = be16(b, marks + 4) {
        p.chapters = (0..n).filter(|k| b.get(marks + 6 + k * 14 + 1) == Some(&1)).count();
    }
    Some(p)
}

/// Disc title from a `bdmt_xxx.xml` (`<di:name>`).
pub fn meta_title(xml: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(xml);
    let start = s.find("<di:name>")? + 9;
    let end = s[start..].find("</di:name>")? + start;
    Some(s[start..end].trim().replace("&amp;", "&"))
}

pub fn fmt_duration(secs: f64) -> String {
    let t = secs.round() as u64;
    format!("{}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
}

impl Playlist {
    pub fn describe_streams(&self) -> Vec<(String, String)> {
        self.streams
            .iter()
            .map(|s| {
                let mut v = s.codec.to_string();
                // The panel keeps the headline; mastering metadata stays in the report.
                let detail: Vec<&str> = s.detail.split(" / ").map(str::trim).filter(|d| !d.is_empty() && !d.starts_with("Mastering") && !d.starts_with("Maximum")).collect();
                if !detail.is_empty() {
                    v = format!("{v} · {}", detail.join(" · "));
                }
                if !s.lang.is_empty() {
                    v = format!("{v} ({})", s.lang);
                }
                (s.kind.to_string(), v)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpls_from_tsmuxer() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/archives/generated/bluray-folder/BDMV/PLAYLIST/00000.mpls");
        let Ok(b) = std::fs::read(&p) else { return };
        let pl = parse_mpls("00000.mpls", &b).unwrap();
        assert!((pl.seconds - 12.0).abs() < 0.5, "{}", pl.seconds);
        assert_eq!(pl.clips, vec!["00000.m2ts"]);
        assert_eq!(pl.streams[0].codec, "HEVC");
        assert_eq!(pl.streams[1].codec, "Dolby Digital");
        assert_eq!(pl.streams[1].lang, "eng");
        assert_eq!(pl.chapters, 3);
    }
}
