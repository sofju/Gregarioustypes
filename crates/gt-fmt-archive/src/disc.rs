//! Disc images that 7-Zip can't read directly, presented to it as a plain ISO
//! (2048-byte user-data sectors):
//! - raw CD sectors (2352/2448/2336 bytes): BIN/CUE, MDF, CloneCD IMG, BlindWrite
//!   B5I/B6I, DiscJuggler CDI, and anything else with sync patterns;
//! - 2048-byte data at an offset: Nero NRG, Instant Copy PDI, images with headers;
//! - compressed UltraISO ISZ (zlib chunks).
//! PowerISO DAA, gBurner GBI and MagicISO UIF are recognised but not decoded.

use crate::sevenzip::Source;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const SYNC: [u8; 12] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

#[derive(Clone, Debug, Default)]
pub struct AudioTrack {
    pub number: u32,
    pub seconds: f64,
}

#[derive(Clone, Debug, Default)]
pub struct DiscInfo {
    /// "BIN/CUE (raw Mode 1, 2352-byte sectors)"...
    pub layout: String,
    pub data_file: Option<PathBuf>,
    pub audio_tracks: Vec<AudioTrack>,
}

/// A view of 2048-byte user data inside a larger sector layout.
pub struct SectorView<R: Read + Seek + Send> {
    inner: R,
    base: u64,
    sector: u64,
    data_off: u64,
    sectors: u64,
    pos: u64,
}

impl<R: Read + Seek + Send> SectorView<R> {
    pub fn new(inner: R, base: u64, sector: u64, data_off: u64, sectors: u64) -> Self {
        SectorView { inner, base, sector, data_off, sectors, pos: 0 }
    }
}

impl<R: Read + Seek + Send> Read for SectorView<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let total = self.sectors * 2048;
        if self.pos >= total || out.is_empty() {
            return Ok(0);
        }
        let s = self.pos / 2048;
        let within = self.pos % 2048;
        let n = ((2048 - within) as usize).min(out.len()).min((total - self.pos) as usize);
        self.inner.seek(SeekFrom::Start(self.base + s * self.sector + self.data_off + within))?;
        let got = self.inner.read(&mut out[..n])?;
        self.pos += got as u64;
        Ok(got)
    }
}

impl<R: Read + Seek + Send> Seek for SectorView<R> {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        let total = (self.sectors * 2048) as i64;
        let np = match p {
            SeekFrom::Start(v) => v as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => total + d,
        };
        if np < 0 {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        self.pos = np as u64;
        Ok(self.pos)
    }
}

impl<R: Read + Seek + Send> Source for SectorView<R> {
    fn len(&self) -> u64 {
        self.sectors * 2048
    }
}

fn read_at(f: &mut File, off: u64, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    if f.seek(SeekFrom::Start(off)).is_err() {
        return Vec::new();
    }
    let mut got = 0;
    while got < n {
        match f.read(&mut b[got..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    b.truncate(got);
    b
}

fn bcd(b: u8) -> u64 {
    ((b >> 4) * 10 + (b & 0x0F)) as u64
}

/// Volume descriptors at logical sector 16 ("\x01CD001", "\x00BEA01", "\x02CD001"...).
fn has_volume_descriptor(sector: &[u8]) -> bool {
    sector.len() >= 6 && matches!(&sector[1..6], b"CD001" | b"BEA01" | b"NSR02" | b"NSR03" | b"TEA01" | b"BOOT2")
}

/// Raw layout guessed from the first sync pattern: (sector size, data offset, base).
fn raw_layout(f: &mut File, size: u64, start: u64) -> Option<(u64, u64, u64)> {
    let head = read_at(f, start, 64 * 1024);
    let p = head.windows(12).position(|w| w == SYNC)? as u64 + start;
    let first = read_at(f, p, 2448 * 2 + 16);
    let sector = [2352u64, 2448]
        .into_iter()
        .find(|&ss| first.len() as u64 >= ss + 12 && first[ss as usize..ss as usize + 12] == SYNC)
        .unwrap_or(2352);
    let mode = first.get(15).copied().unwrap_or(1);
    let data_off = if mode == 2 { 24 } else { 16 };
    // The header's address says which sector this is; most images start at LBA 0.
    let lba = (bcd(first[12]) * 60 + bcd(first[13])) * 75 + bcd(first[14]);
    let lba = lba.saturating_sub(150);
    let base = if lba * sector <= p && lba < 1000 { p - lba * sector } else { p };
    // Confirm: a volume descriptor at sector 16.
    let vd = read_at(f, base + 16 * sector + data_off, 8);
    if has_volume_descriptor(&vd) && base + 17 * sector <= size {
        return Some((sector, data_off, base));
    }
    None
}

/// 2048-byte (or Mode 2 form 1 2336-byte) data at an offset: find sector 16.
fn offset_layout(f: &mut File, size: u64) -> Option<(u64, u64, u64)> {
    let head = read_at(f, 0, 4 << 20);
    for (i, w) in head.windows(6).enumerate() {
        if matches!(&w[1..6], b"CD001" | b"BEA01") && (w[0] == 0x01 || w[0] == 0x00) {
            for (ss, off) in [(2048u64, 0u64), (2336, 8), (2352, 16), (2352, 24), (2448, 16)] {
                let pos = i as u64;
                if pos >= 16 * ss + off && (pos - off) % ss == 0 || pos >= 16 * ss + off {
                    let base = pos.checked_sub(16 * ss + off)?;
                    let vd = read_at(f, base + 16 * ss + off, 6);
                    if has_volume_descriptor(&vd) && base + 17 * ss <= size {
                        return Some((ss, off, base));
                    }
                }
            }
        }
    }
    None
}

fn view(f: File, size: u64, (ss, off, base): (u64, u64, u64)) -> Box<dyn Source> {
    let sectors = (size - base) / ss;
    Box::new(SectorView::new(f, base, ss, off, sectors))
}

fn describe(ss: u64, off: u64) -> String {
    match (ss, off) {
        (2048, _) => "2048-byte sectors".into(),
        (2352, 16) => "raw Mode 1, 2352-byte sectors".into(),
        (2352, 24) => "raw Mode 2, 2352-byte sectors".into(),
        (2448, _) => "raw sectors with subchannel data (2448 bytes)".into(),
        (2336, _) => "Mode 2, 2336-byte sectors".into(),
        (s, _) => format!("{s}-byte sectors"),
    }
}

/// Parse a cue sheet: (data file, first data track's sector layout, audio tracks).
fn parse_cue(cue: &Path) -> Option<(PathBuf, Option<(u64, u64)>, Vec<AudioTrack>)> {
    let text = std::fs::read(cue).ok()?;
    let text = String::from_utf8_lossy(&text);
    let mut file: Option<PathBuf> = None;
    let mut layout: Option<(u64, u64)> = None;
    let mut audio = Vec::new();
    let mut tracks: Vec<(u32, bool, u64)> = Vec::new(); // (number, audio, index01 frame)
    let mut current: Option<(u32, bool)> = None;
    for line in text.lines() {
        let l = line.trim();
        let up = l.to_ascii_uppercase();
        if up.starts_with("FILE ") && file.is_none() {
            let name = l[5..].trim();
            let name = if let Some(rest) = name.strip_prefix('"') { rest.split('"').next().unwrap_or("") } else { name.split_whitespace().next().unwrap_or("") };
            file = Some(cue.parent()?.join(name));
        } else if up.starts_with("TRACK ") {
            let mut it = up.split_whitespace().skip(1);
            let n: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let mode = it.next().unwrap_or("");
            let is_audio = mode == "AUDIO";
            if !is_audio && layout.is_none() {
                layout = Some(match mode {
                    "MODE1/2048" => (2048, 0),
                    "MODE1/2352" => (2352, 16),
                    "MODE2/2336" => (2336, 8),
                    "MODE2/2352" => (2352, 24),
                    _ => (2352, 16),
                });
            }
            current = Some((n, is_audio));
        } else if up.starts_with("INDEX 01") {
            let t = up.split_whitespace().nth(2).unwrap_or("0:0:0");
            let p: Vec<u64> = t.split(':').filter_map(|x| x.parse().ok()).collect();
            if p.len() == 3 {
                if let Some((n, a)) = current {
                    tracks.push((n, a, (p[0] * 60 + p[1]) * 75 + p[2]));
                }
            }
        }
    }
    let file = file?;
    let total_frames = std::fs::metadata(&file).map(|m| m.len() / 2352).unwrap_or(0);
    for (k, &(n, a, start)) in tracks.iter().enumerate() {
        if a {
            let end = tracks.get(k + 1).map(|t| t.2).unwrap_or(total_frames);
            audio.push(AudioTrack { number: n, seconds: end.saturating_sub(start) as f64 / 75.0 });
        }
    }
    Some((file, layout, audio))
}

/// Nero footer: first track's (offset, sector size, data offset).
fn nero(f: &mut File, size: u64) -> Option<(u64, u64, u64)> {
    let tail = read_at(f, size.checked_sub(12)?, 12);
    let chunks_at = if &tail[0..4] == b"NER5" {
        u64::from_be_bytes(tail[4..12].try_into().ok()?)
    } else if &tail[4..8] == b"NERO" {
        u32::from_be_bytes(tail[8..12].try_into().ok()?) as u64
    } else {
        return None;
    };
    let data = read_at(f, chunks_at, (size - chunks_at).min(1 << 20) as usize);
    let mut i = 0;
    while i + 8 <= data.len() {
        let id = &data[i..i + 4];
        let len = u32::from_be_bytes(data[i + 4..i + 8].try_into().ok()?) as usize;
        let body = &data[i + 8..(i + 8 + len).min(data.len())];
        match id {
            b"DAOX" if body.len() >= 22 + 42 => {
                let t = &body[22..];
                let ss = u16::from_be_bytes([t[12], t[13]]) as u64;
                let idx1 = u64::from_be_bytes(t[26..34].try_into().ok()?);
                let off = if ss == 2352 { 16 } else if ss == 2336 { 8 } else { 0 };
                return Some((ss.max(2048), off, idx1));
            }
            b"DAOI" if body.len() >= 22 + 30 => {
                let t = &body[22..];
                let ss = u16::from_be_bytes([t[12], t[13]]) as u64;
                let idx1 = u32::from_be_bytes(t[22..26].try_into().ok()?) as u64;
                let off = if ss == 2352 { 16 } else if ss == 2336 { 8 } else { 0 };
                return Some((ss.max(2048), off, idx1));
            }
            b"ETN2" if body.len() >= 32 => {
                let start = u64::from_be_bytes(body[0..8].try_into().ok()?);
                let mode = u32::from_be_bytes(body[16..20].try_into().ok()?);
                let ss = if mode == 3 || mode == 6 { 2352 } else { 2048 };
                return Some((ss, if ss == 2352 { 16 } else { 0 }, start));
            }
            b"END!" => break,
            _ => {}
        }
        i += 8 + len;
    }
    None
}

pub enum Opened {
    /// Hand this to 7-Zip as an ISO.
    Iso(Box<dyn Source>, DiscInfo),
    /// Recognised, but its compression isn't supported.
    Unsupported(&'static str),
}

/// Disc-image extensions handled here (plain `.iso` goes to 7-Zip directly).
pub const EXTS: &[&str] = &["bin", "cue", "mdf", "mds", "nrg", "img", "ccd", "cdi", "pdi", "b5i", "b6i", "b5t", "b6t", "isz", "daa", "gbi", "uif"];

pub fn sniff_compressed(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"DAA") && head.len() > 16 && head[3] == 0 {
        Some("PowerISO DAA")
    } else if head.starts_with(b"GBI") {
        Some("gBurner GBI")
    } else if head.starts_with(b"bbis") || (head.len() > 4 && &head[0..4] == b"blhr") {
        Some("MagicISO UIF")
    } else {
        None
    }
}

pub fn open(path: &Path, head: &[u8]) -> Option<Opened> {
    let ext = gt_core::sniff::ext(path);
    if head.starts_with(b"IsZ!") {
        return crate::isz::open(path).map(|s| Opened::Iso(s, DiscInfo { layout: "UltraISO compressed image (ISZ)".into(), ..Default::default() }));
    }
    if let Some(name) = sniff_compressed(head) {
        return Some(Opened::Unsupported(name));
    }
    if ext == "uif" {
        return Some(Opened::Unsupported("MagicISO UIF"));
    }
    // Descriptor files point at their data file.
    let mut data = path.to_path_buf();
    let mut info = DiscInfo::default();
    let mut forced: Option<(u64, u64)> = None;
    match ext.as_str() {
        "cue" => {
            let (file, layout, audio) = parse_cue(path)?;
            data = file;
            forced = layout;
            info.audio_tracks = audio;
        }
        "mds" | "ccd" | "b5t" | "b6t" => {
            let target = match ext.as_str() {
                "mds" => "mdf",
                "ccd" => "img",
                "b5t" => "b5i",
                _ => "b6i",
            };
            data = path.with_extension(target);
        }
        _ => {}
    }
    let mut f = File::open(&data).ok()?;
    let size = f.metadata().ok()?.len();
    if data != path {
        info.data_file = Some(data.clone());
    }
    let kind = match ext.as_str() {
        "cue" | "bin" => "BIN/CUE",
        "mdf" | "mds" => "Alcohol 120% MDF",
        "nrg" => "Nero NRG",
        "img" | "ccd" => "CloneCD IMG",
        "cdi" => "DiscJuggler CDI",
        "pdi" => "Instant CD/DVD PDI",
        "b5i" | "b5t" => "BlindWrite 5",
        "b6i" | "b6t" => "BlindWrite 6",
        _ => "Disc image",
    };
    if ext == "nrg" {
        if let Some((ss, off, base)) = nero(&mut f, size) {
            info.layout = format!("{kind} ({})", describe(ss, off));
            let end = size.saturating_sub(12);
            return Some(Opened::Iso(Box::new(SectorView::new(f, base, ss, off, (end - base) / ss)), info));
        }
    }
    if let Some((ss, off)) = forced {
        if ss == 2048 {
            info.layout = format!("{kind} ({})", describe(ss, off));
            return Some(Opened::Iso(view(f, size, (2048, 0, 0)), info));
        }
        if let Some(l) = raw_layout(&mut f, size, 0) {
            info.layout = format!("{kind} ({})", describe(l.0, l.1));
            return Some(Opened::Iso(view(f, size, l), info));
        }
        let l = (ss, off, 0);
        info.layout = format!("{kind} ({})", describe(ss, off));
        return Some(Opened::Iso(view(f, size, l), info));
    }
    if let Some(l) = raw_layout(&mut f, size, 0) {
        info.layout = format!("{kind} ({})", describe(l.0, l.1));
        return Some(Opened::Iso(view(f, size, l), info));
    }
    if let Some(l) = offset_layout(&mut f, size) {
        info.layout = if l.2 == 0 && l.0 == 2048 { format!("{kind} (plain ISO data)") } else { format!("{kind} ({}, data at offset {})", describe(l.0, l.1), l.2) };
        return Some(Opened::Iso(view(f, size, l), info));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sector_view_maps_user_data() {
        // Two raw sectors whose user data is "AAAA..." then "BBBB...".
        let mut raw = Vec::new();
        for (i, c) in [b'A', b'B'].iter().enumerate() {
            raw.extend_from_slice(&SYNC);
            raw.extend_from_slice(&[0, 2, i as u8, 1]);
            raw.extend(std::iter::repeat_n(*c, 2048));
            raw.extend(std::iter::repeat_n(0u8, 288));
        }
        let mut v = SectorView::new(std::io::Cursor::new(raw), 0, 2352, 16, 2);
        let mut buf = vec![0u8; 4096];
        let mut n = 0;
        while n < buf.len() {
            let k = v.read(&mut buf[n..]).unwrap();
            if k == 0 {
                break;
            }
            n += k;
        }
        assert_eq!(n, 4096);
        assert!(buf[..2048].iter().all(|&b| b == b'A'));
        assert!(buf[2048..].iter().all(|&b| b == b'B'));
        v.seek(SeekFrom::Start(2047)).unwrap();
        let mut two = [0u8; 2];
        v.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"AB");
    }
}
