//! MIDI: track/instrument summary from our own SMF parser, playback through
//! rustysynth with a bundled General MIDI SoundFont, streamed to mpv as a WAV
//! via the `gtmidi://` protocol (so the shared transport bar just works).

use crate::mpv::{Mpv, StreamCbInfo};
use parking_lot::Mutex;
use rustysynth::{MidiFile, MidiFileSequencer, SoundFont, Synthesizer, SynthesizerSettings};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

const RATE: u32 = 44100;
const TAIL_SECS: f64 = 1.5;

pub const GM: [&str; 128] = [
    "Acoustic Grand Piano", "Bright Acoustic Piano", "Electric Grand Piano", "Honky-tonk Piano", "Electric Piano 1", "Electric Piano 2", "Harpsichord", "Clavinet",
    "Celesta", "Glockenspiel", "Music Box", "Vibraphone", "Marimba", "Xylophone", "Tubular Bells", "Dulcimer", "Drawbar Organ", "Percussive Organ", "Rock Organ",
    "Church Organ", "Reed Organ", "Accordion", "Harmonica", "Tango Accordion", "Acoustic Guitar (nylon)", "Acoustic Guitar (steel)", "Electric Guitar (jazz)",
    "Electric Guitar (clean)", "Electric Guitar (muted)", "Overdriven Guitar", "Distortion Guitar", "Guitar Harmonics", "Acoustic Bass", "Electric Bass (finger)",
    "Electric Bass (pick)", "Fretless Bass", "Slap Bass 1", "Slap Bass 2", "Synth Bass 1", "Synth Bass 2", "Violin", "Viola", "Cello", "Contrabass",
    "Tremolo Strings", "Pizzicato Strings", "Orchestral Harp", "Timpani", "String Ensemble 1", "String Ensemble 2", "Synth Strings 1", "Synth Strings 2",
    "Choir Aahs", "Voice Oohs", "Synth Voice", "Orchestra Hit", "Trumpet", "Trombone", "Tuba", "Muted Trumpet", "French Horn", "Brass Section", "Synth Brass 1",
    "Synth Brass 2", "Soprano Sax", "Alto Sax", "Tenor Sax", "Baritone Sax", "Oboe", "English Horn", "Bassoon", "Clarinet", "Piccolo", "Flute", "Recorder",
    "Pan Flute", "Blown Bottle", "Shakuhachi", "Whistle", "Ocarina", "Lead 1 (square)", "Lead 2 (sawtooth)", "Lead 3 (calliope)", "Lead 4 (chiff)",
    "Lead 5 (charang)", "Lead 6 (voice)", "Lead 7 (fifths)", "Lead 8 (bass + lead)", "Pad 1 (new age)", "Pad 2 (warm)", "Pad 3 (polysynth)", "Pad 4 (choir)",
    "Pad 5 (bowed)", "Pad 6 (metallic)", "Pad 7 (halo)", "Pad 8 (sweep)", "FX 1 (rain)", "FX 2 (soundtrack)", "FX 3 (crystal)", "FX 4 (atmosphere)",
    "FX 5 (brightness)", "FX 6 (goblins)", "FX 7 (echoes)", "FX 8 (sci-fi)", "Sitar", "Banjo", "Shamisen", "Koto", "Kalimba", "Bagpipe", "Fiddle", "Shanai",
    "Tinkle Bell", "Agogo", "Steel Drums", "Woodblock", "Taiko Drum", "Melodic Tom", "Synth Drum", "Reverse Cymbal", "Guitar Fret Noise", "Breath Noise",
    "Seashore", "Bird Tweet", "Telephone Ring", "Helicopter", "Applause", "Gunshot",
];

/// Strip a RIFF RMID wrapper; returns the Standard MIDI File bytes.
pub fn smf_bytes(data: &[u8]) -> Option<&[u8]> {
    if data.starts_with(b"MThd") {
        return Some(data);
    }
    if data.len() > 20 && &data[0..4] == b"RIFF" && &data[8..12] == b"RMID" {
        let mut i = 12;
        while i + 8 <= data.len() {
            let len = u32::from_le_bytes(data[i + 4..i + 8].try_into().ok()?) as usize;
            if &data[i..i + 4] == b"data" {
                return data.get(i + 8..(i + 8 + len).min(data.len()));
            }
            i += 8 + len + (len & 1);
        }
    }
    // Some files carry junk before the header.
    let pos = data.windows(4).take(4096).position(|w| w == b"MThd")?;
    Some(&data[pos..])
}

#[derive(Clone, Debug, Default)]
pub struct MidiInfo {
    pub format: u16,
    pub tracks: u16,
    pub division: u16,
    pub title: String,
    pub copyright: String,
    pub track_names: Vec<String>,
    /// (channel 1-16, instrument name)
    pub instruments: Vec<(u8, String)>,
    pub tempo_bpm: f64,
    pub time_sig: String,
    pub key_sig: String,
    pub notes: u64,
    pub lyrics: bool,
    pub duration: f64,
}

fn vlq(d: &[u8], i: &mut usize) -> Option<u32> {
    let mut v = 0u32;
    for _ in 0..4 {
        let b = *d.get(*i)?;
        *i += 1;
        v = (v << 7) | (b & 0x7F) as u32;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    Some(v)
}

fn text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.trim().to_string(),
        Err(_) => gt_core::encoding::decode_1252(b).trim().to_string(),
    }
}

const KEYS: [&str; 15] = ["C♭", "G♭", "D♭", "A♭", "E♭", "B♭", "F", "C", "G", "D", "A", "E", "B", "F♯", "C♯"];

pub fn parse_info(smf: &[u8]) -> Option<MidiInfo> {
    if smf.len() < 14 || &smf[0..4] != b"MThd" {
        return None;
    }
    let hl = u32::from_be_bytes(smf[4..8].try_into().ok()?) as usize;
    let mut m = MidiInfo {
        format: u16::from_be_bytes([smf[8], smf[9]]),
        tracks: u16::from_be_bytes([smf[10], smf[11]]),
        division: u16::from_be_bytes([smf[12], smf[13]]),
        ..Default::default()
    };
    let mut programs: [Option<u8>; 16] = [None; 16];
    let mut used = [false; 16];
    let mut i = 8 + hl;
    let mut track = 0;
    while i + 8 <= smf.len() {
        let len = u32::from_be_bytes(smf[i + 4..i + 8].try_into().ok()?) as usize;
        let body = &smf[i + 8..(i + 8 + len).min(smf.len())];
        let is_track = &smf[i..i + 4] == b"MTrk";
        i += 8 + len;
        if !is_track {
            continue;
        }
        let mut j = 0;
        let mut running = 0u8;
        while j < body.len() {
            if vlq(body, &mut j).is_none() {
                break;
            }
            let Some(&b) = body.get(j) else { break };
            let status = if b & 0x80 != 0 {
                j += 1;
                b
            } else {
                running
            };
            match status {
                0xFF => {
                    let Some(&kind) = body.get(j) else { break };
                    j += 1;
                    let Some(l) = vlq(body, &mut j) else { break };
                    let data = &body[j.min(body.len())..(j + l as usize).min(body.len())];
                    j += l as usize;
                    match kind {
                        0x02 if m.copyright.is_empty() => m.copyright = text(data),
                        0x03 => {
                            let t = text(data);
                            if track == 0 && m.title.is_empty() && !t.is_empty() {
                                m.title = t.clone();
                            }
                            if !t.is_empty() {
                                m.track_names.push(t);
                            }
                        }
                        0x05 => m.lyrics = true,
                        0x51 if data.len() == 3 && m.tempo_bpm == 0.0 => {
                            let us = ((data[0] as u32) << 16) | ((data[1] as u32) << 8) | data[2] as u32;
                            if us > 0 {
                                m.tempo_bpm = 60_000_000.0 / us as f64;
                            }
                        }
                        0x58 if data.len() >= 2 && m.time_sig.is_empty() => m.time_sig = format!("{}/{}", data[0], 1u32 << data[1].min(6)),
                        0x59 if data.len() >= 2 && m.key_sig.is_empty() => {
                            let sf = (data[0] as i8).clamp(-7, 7);
                            m.key_sig = format!("{} {}", KEYS[(sf + 7) as usize], if data[1] == 1 { "minor" } else { "major" });
                        }
                        _ => {}
                    }
                }
                0xF0 | 0xF7 => {
                    let Some(l) = vlq(body, &mut j) else { break };
                    j += l as usize;
                }
                s if s >= 0x80 => {
                    running = s;
                    let ch = (s & 0x0F) as usize;
                    let n = match s & 0xF0 {
                        0xC0 | 0xD0 => 1,
                        _ => 2,
                    };
                    let d = &body[j.min(body.len())..(j + n).min(body.len())];
                    match s & 0xF0 {
                        0x90 if d.len() == 2 && d[1] > 0 => {
                            m.notes += 1;
                            used[ch] = true;
                        }
                        0xC0 if !d.is_empty() && programs[ch].is_none() => programs[ch] = Some(d[0] & 0x7F),
                        _ => {}
                    }
                    j += n;
                }
                _ => break,
            }
        }
        track += 1;
    }
    for ch in 0..16 {
        if !used[ch] {
            continue;
        }
        let name = if ch == 9 { "Drum kit".to_string() } else { GM[programs[ch].unwrap_or(0) as usize].to_string() };
        m.instruments.push((ch as u8 + 1, name));
    }
    if m.tempo_bpm == 0.0 {
        m.tempo_bpm = 120.0;
    }
    Some(m)
}

// ------------------------------------------------------------------ SoundFont

static SOUNDFONT: OnceLock<Result<Arc<SoundFont>, String>> = OnceLock::new();

pub fn soundfont_path() -> Option<std::path::PathBuf> {
    let dir = gt_core::paths::deps_dir().join("soundfont");
    let mut found: Vec<_> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| matches!(gt_core::sniff::ext(p).as_str(), "sf2" | "sf3")).collect();
    found.sort();
    found.into_iter().next()
}

/// The bundled General MIDI SoundFont (loaded once, ~100 ms).
pub fn soundfont() -> Result<Arc<SoundFont>, String> {
    SOUNDFONT
        .get_or_init(|| {
            let t = std::time::Instant::now();
            let path = soundfont_path().ok_or("no SoundFont in deps/soundfont")?;
            let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let sf = SoundFont::new(&mut std::io::BufReader::new(f)).map_err(|e| format!("{e:?}"))?;
            tracing::info!("SoundFont {} loaded in {:.0} ms", path.display(), t.elapsed().as_secs_f64() * 1000.0);
            Ok(Arc::new(sf))
        })
        .clone()
}

// ------------------------------------------------------------------ gtmidi:// streams

struct Source {
    midi: Arc<MidiFile>,
    sf: Arc<SoundFont>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SOURCES: OnceLock<Mutex<HashMap<u64, Source>>> = OnceLock::new();

fn sources() -> &'static Mutex<HashMap<u64, Source>> {
    SOURCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Make a MIDI file playable; returns the `gtmidi://` URL for mpv and its length.
pub fn prepare(smf: &[u8]) -> Result<(String, f64), String> {
    let sf = soundfont()?;
    let midi = MidiFile::new(&mut std::io::Cursor::new(smf)).map_err(|e| format!("{e:?}"))?;
    let len = midi.get_length();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut map = sources().lock();
    // Keep only a handful of sources alive (each holds the parsed file).
    if map.len() > 8 {
        let oldest = *map.keys().min().unwrap();
        map.remove(&oldest);
    }
    map.insert(id, Source { midi: Arc::new(midi), sf });
    Ok((format!("gtmidi://{id}"), len))
}

struct Stream {
    midi: Arc<MidiFile>,
    seq: MidiFileSequencer,
    /// Byte position in the virtual WAV.
    pos: u64,
    total_samples: u64,
    rendered: u64,
    pending: Vec<u8>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Stream {
    fn header(&self) -> [u8; 44] {
        let data = (self.total_samples * 4) as u32;
        let mut h = [0u8; 44];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
        h[8..16].copy_from_slice(b"WAVEfmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes());
        h[20..22].copy_from_slice(&1u16.to_le_bytes());
        h[22..24].copy_from_slice(&2u16.to_le_bytes());
        h[24..28].copy_from_slice(&RATE.to_le_bytes());
        h[28..32].copy_from_slice(&(RATE * 4).to_le_bytes());
        h[32..34].copy_from_slice(&4u16.to_le_bytes());
        h[34..36].copy_from_slice(&16u16.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&data.to_le_bytes());
        h
    }

    fn restart(&mut self) {
        self.seq.play(&self.midi, false);
        self.rendered = 0;
        self.pending.clear();
    }

    /// Render `n` samples; when `keep` is false they are discarded (seeking).
    fn render(&mut self, n: usize, keep: bool) {
        let n = n.min((self.total_samples - self.rendered) as usize);
        if n == 0 {
            return;
        }
        self.left.resize(n, 0.0);
        self.right.resize(n, 0.0);
        self.seq.render(&mut self.left[..n], &mut self.right[..n]);
        self.rendered += n as u64;
        if keep {
            self.pending.reserve(n * 4);
            for k in 0..n {
                let l = (self.left[k] * 32767.0).clamp(-32768.0, 32767.0) as i16;
                let r = (self.right[k] * 32767.0).clamp(-32768.0, 32767.0) as i16;
                self.pending.extend_from_slice(&l.to_le_bytes());
                self.pending.extend_from_slice(&r.to_le_bytes());
            }
        }
    }
}

unsafe extern "C" fn s_read(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64 {
    let s = unsafe { &mut *(cookie as *mut Stream) };
    let total = 44 + s.total_samples * 4;
    if s.pos >= total {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, nbytes as usize) };
    let mut w = 0usize;
    if s.pos < 44 {
        let h = s.header();
        let n = ((44 - s.pos) as usize).min(out.len());
        out[..n].copy_from_slice(&h[s.pos as usize..s.pos as usize + n]);
        s.pos += n as u64;
        w += n;
    }
    while w < out.len() && s.pos < total {
        if s.pending.is_empty() {
            s.render(4096, true);
            if s.pending.is_empty() {
                break;
            }
        }
        let n = s.pending.len().min(out.len() - w);
        out[w..w + n].copy_from_slice(&s.pending[..n]);
        s.pending.drain(..n);
        w += n;
        s.pos += n as u64;
    }
    w as i64
}

unsafe extern "C" fn s_seek(cookie: *mut c_void, offset: i64) -> i64 {
    let s = unsafe { &mut *(cookie as *mut Stream) };
    let total = 44 + s.total_samples * 4;
    let offset = (offset.max(0) as u64).min(total);
    let target = offset.saturating_sub(44) / 4;
    let sub = (offset.saturating_sub(44) % 4) as usize;
    if target < s.rendered || s.pos > offset {
        s.restart();
    }
    s.pending.clear();
    while s.rendered < target {
        let n = (target - s.rendered).min(16384) as usize;
        s.render(n, false);
    }
    if offset > 44 && sub > 0 {
        s.render(1, true);
        s.pending.drain(..sub);
    }
    s.pos = offset;
    offset as i64
}

unsafe extern "C" fn s_size(cookie: *mut c_void) -> i64 {
    let s = unsafe { &*(cookie as *const Stream) };
    (44 + s.total_samples * 4) as i64
}

unsafe extern "C" fn s_close(cookie: *mut c_void) {
    drop(unsafe { Box::from_raw(cookie as *mut Stream) });
}

unsafe extern "C" fn s_open(_user: *mut c_void, uri: *mut c_char, info: *mut StreamCbInfo) -> c_int {
    const LOADING_FAILED: c_int = -13;
    let uri = unsafe { CStr::from_ptr(uri) }.to_string_lossy().into_owned();
    let Some(id) = uri.strip_prefix("gtmidi://").and_then(|s| s.parse::<u64>().ok()) else { return LOADING_FAILED };
    let (midi, sf) = match sources().lock().get(&id) {
        Some(s) => (s.midi.clone(), s.sf.clone()),
        None => return LOADING_FAILED,
    };
    let settings = SynthesizerSettings::new(RATE as i32);
    let synth = match Synthesizer::new(&sf, &settings) {
        Ok(s) => s,
        Err(_) => return LOADING_FAILED,
    };
    let mut seq = MidiFileSequencer::new(synth);
    seq.play(&midi, false);
    let total_samples = ((midi.get_length() + TAIL_SECS) * RATE as f64) as u64;
    let s = Box::new(Stream { midi, seq, pos: 0, total_samples, rendered: 0, pending: Vec::new(), left: Vec::new(), right: Vec::new() });
    unsafe {
        *info = StreamCbInfo {
            cookie: Box::into_raw(s) as *mut c_void,
            read_fn: Some(s_read),
            seek_fn: Some(s_seek),
            size_fn: Some(s_size),
            close_fn: Some(s_close),
            cancel_fn: None,
        };
    }
    0
}

pub fn register_protocol(m: &Mpv) {
    if let Err(e) = m.add_stream_protocol("gtmidi", std::ptr::null_mut(), s_open) {
        tracing::warn!("gtmidi protocol: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generated_song() {
        // Header + one track: name, program change, one note.
        let mut trk = vec![0x00, 0xFF, 0x03, 0x04, b'T', b'e', b's', b't', 0x00, 0xC0, 0x30, 0x00, 0x90, 60, 100, 0x60, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00];
        let mut smf = b"MThd".to_vec();
        smf.extend_from_slice(&[0, 0, 0, 6, 0, 0, 0, 1, 0x01, 0xE0]);
        smf.extend_from_slice(b"MTrk");
        smf.extend_from_slice(&(trk.len() as u32).to_be_bytes());
        smf.append(&mut trk);
        let m = parse_info(&smf).unwrap();
        assert_eq!(m.title, "Test");
        assert_eq!(m.notes, 1);
        assert_eq!(m.instruments, vec![(1, "String Ensemble 1".to_string())]);
        let mut riff = b"RIFF\0\0\0\0RMIDdata".to_vec();
        riff.extend_from_slice(&(smf.len() as u32).to_le_bytes());
        riff.extend_from_slice(&smf);
        assert_eq!(smf_bytes(&riff).unwrap(), &smf[..]);
    }
}
