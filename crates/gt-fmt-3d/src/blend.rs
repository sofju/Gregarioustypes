//! Blender files: the preview Blender stores in the file (shown at once), then
//! the bundled Blender converts the scene to GLB in the background (cached).
//! Blender runs as a warm service (`blender_server.py`), like LibreOffice: it
//! starts when a .blend file is selected and quits after a while unused.

use crate::view::{Content, Converted, ModelLoaded, Thumb};
use gt_core::CancelToken;
use gt_fmt_doc::helper;
use gt_ui::{Loaded, Probe, Provider};
use parking_lot::Mutex;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SCRIPT: &str = include_str!("../assets/blender_server.py");
const VERSION: &str = "5.2.3";
const TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub const EXTS: &[&str] = &["blend", "blend1", "blend2"];
const ZSTD: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
const GZIP: &[u8] = &[0x1F, 0x8B];

pub fn exe() -> PathBuf {
    gt_core::paths::deps_dir().join("blender").join("blender.exe")
}

pub fn available() -> bool {
    exe().is_file()
}

pub fn sniff(head: &[u8]) -> bool {
    head.starts_with(b"BLENDER") || head.starts_with(ZSTD) || head.starts_with(GZIP)
}

/// The start of the file, decompressed (zstd since Blender 3.0, gzip before).
pub fn uncompressed_head(path: &Path, head: &[u8], limit: usize) -> Option<(Vec<u8>, Option<&'static str>)> {
    if head.starts_with(b"BLENDER") {
        if head.len() >= limit {
            return Some((head.to_vec(), None));
        }
        let mut b = Vec::new();
        std::fs::File::open(path).ok()?.take(limit as u64).read_to_end(&mut b).ok()?;
        return Some((b, None));
    }
    let f = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut out = Vec::new();
    if head.starts_with(ZSTD) {
        // Blender writes many small frames; decode them until we have enough.
        let mut f = f;
        while out.len() < limit {
            let Ok(mut d) = ruzstd::decoding::StreamingDecoder::new(&mut f) else { break };
            let before = out.len();
            let _ = (&mut d).take((limit - out.len()) as u64).read_to_end(&mut out);
            if out.len() == before {
                break;
            }
        }
        return out.starts_with(b"BLENDER").then_some((out, Some("zstd")));
    }
    if head.starts_with(GZIP) {
        let _ = flate2::read::GzDecoder::new(f).take(limit as u64).read_to_end(&mut out);
        return out.starts_with(b"BLENDER").then_some((out, Some("gzip")));
    }
    None
}

pub struct Header {
    /// "5.2" from "0502" / "4.5" from "405".
    pub version: String,
    /// Header length and block-header layout.
    len: usize,
    large: bool,
    ptr8: bool,
    big_endian: bool,
}

pub fn header(d: &[u8]) -> Option<Header> {
    if !d.starts_with(b"BLENDER") || d.len() < 12 {
        return None;
    }
    // Blender 5.0+: "BLENDER17-01v0502" — header size, file format version, endianness, version.
    if d.len() >= 17 && d[7..9].iter().all(|c| c.is_ascii_digit()) && d[9] == b'-' {
        let v: u32 = std::str::from_utf8(&d[13..17]).ok()?.parse().ok()?;
        return Some(Header { version: format!("{}.{}", v / 100, v % 100), len: 17, large: true, ptr8: true, big_endian: d[12] == b'V' });
    }
    let v: u32 = std::str::from_utf8(&d[9..12]).ok()?.parse().ok()?;
    Some(Header { version: format!("{}.{}", v / 100, v % 100), len: 12, large: false, ptr8: d[7] == b'-', big_endian: d[8] == b'V' })
}

/// The preview Blender saved in the file (`TEST` block), as premultiplied BGRA.
pub fn thumbnail(d: &[u8], h: &Header) -> Option<Thumb> {
    let mut o = h.len;
    let rd32 = |b: &[u8]| if h.big_endian { u32::from_be_bytes(b.try_into().unwrap()) } else { u32::from_le_bytes(b.try_into().unwrap()) };
    let rd64 = |b: &[u8]| if h.big_endian { u64::from_be_bytes(b.try_into().unwrap()) } else { u64::from_le_bytes(b.try_into().unwrap()) };
    for _ in 0..64 {
        let code = d.get(o..o + 4)?;
        let (len, bh) = if h.large {
            (rd64(d.get(o + 16..o + 24)?) as usize, 32)
        } else {
            (rd32(d.get(o + 4..o + 8)?) as usize, if h.ptr8 { 24 } else { 20 })
        };
        if code == b"TEST" {
            let b = d.get(o + bh..o + bh + len)?;
            let w = rd32(&b[0..4]) as usize;
            let hh = rd32(&b[4..8]) as usize;
            if w == 0 || hh == 0 || w > 4096 || hh > 4096 || b.len() < 8 + w * hh * 4 {
                return None;
            }
            let px = &b[8..8 + w * hh * 4];
            let mut bgra = vec![0u8; w * hh * 4];
            // Rows are stored bottom-up.
            for y in 0..hh {
                let src = &px[(hh - 1 - y) * w * 4..(hh - y) * w * 4];
                let dst = &mut bgra[y * w * 4..(y + 1) * w * 4];
                for (s, t) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                    let a = s[3] as u32;
                    t[0] = (s[2] as u32 * a / 255) as u8;
                    t[1] = (s[1] as u32 * a / 255) as u8;
                    t[2] = (s[0] as u32 * a / 255) as u8;
                    t[3] = s[3];
                }
            }
            return Some(Thumb { w: w as u32, h: hh as u32, bgra });
        }
        if code == b"GLOB" || code == b"DNA1" || code == b"ENDB" {
            return None;
        }
        o += bh + len;
    }
    None
}

struct Service {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    last_used: Instant,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{{\"quit\": true}}");
        let _ = self.stdin.flush();
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(800) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static SERVICE: Mutex<Option<Service>> = Mutex::new(None);
static WARMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn parse(line: &str) -> Option<serde_json::Value> {
    serde_json::from_str(line.strip_prefix("@@GT ")?).ok()
}

fn start() -> Result<Service, String> {
    if !available() {
        return Err("Blender is not installed in deps\\blender.".into());
    }
    let dir = gt_core::paths::cache_dir();
    let script = dir.join("blender_server.py");
    if std::fs::read_to_string(&script).ok().as_deref() != Some(SCRIPT) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&script, SCRIPT).map_err(|e| e.to_string())?;
    }
    let t = Instant::now();
    let mut cmd = Command::new(exe());
    cmd.args(["-b", "--factory-startup", "-noaudio", "--disable-autoexec", "--offline-mode", "--python-exit-code", "1", "--python"])
        .arg(&script)
        // Preferences, caches and add-ons stay in our cache, not the user's AppData.
        .env("BLENDER_USER_RESOURCES", dir.join("blender-user"))
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = helper::spawn(&mut cmd, true).map_err(|e| format!("cannot start Blender: {e}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("gt-blender-stdout".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(l) = line else { break };
                if l.starts_with("@@GT ") && tx.send(l).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let svc = Service { child, stdin, lines: rx, next_id: 1, last_used: Instant::now() };
    match svc.lines.recv_timeout(Duration::from_secs(60)) {
        Ok(l) => {
            let v = parse(&l).ok_or("Blender: unexpected answer")?;
            if v["ready"].as_bool() != Some(true) {
                return Err("Blender did not start".into());
            }
            tracing::info!("Blender {} ready in {:.1} s", v["version"].as_str().unwrap_or("?"), t.elapsed().as_secs_f64());
        }
        Err(_) => return Err("Blender did not start (no answer).".into()),
    }
    Ok(svc)
}

fn idle_watchdog() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("gt-blender-idle".into()).spawn(|| loop {
            std::thread::sleep(Duration::from_secs(30));
            if let Some(mut g) = SERVICE.try_lock() {
                let idle = gt_core::settings::idle_minutes();
                if idle > 0 && g.as_ref().is_some_and(|s| s.last_used.elapsed() > Duration::from_secs(idle as u64 * 60)) {
                    tracing::info!("Blender idle: shutting it down");
                    *g = None;
                }
            }
        });
    });
}

/// Start Blender in the background (a .blend file is selected or about to be).
pub fn prewarm() {
    use std::sync::atomic::Ordering;
    if !available() {
        return;
    }
    if let Some(g) = SERVICE.try_lock() {
        if g.is_some() {
            return;
        }
    }
    if WARMING.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new().name("gt-blender-warm".into()).spawn(|| {
        let mut g = SERVICE.lock();
        if g.is_none() {
            match start() {
                Ok(s) => *g = Some(s),
                Err(e) => tracing::warn!("Blender prewarm: {e}"),
            }
        }
        drop(g);
        WARMING.store(false, std::sync::atomic::Ordering::Release);
        idle_watchdog();
    });
}

/// Cached GLB and facts for `src` opened (`kind` "blend") or imported by Blender.
pub fn cached(src: &Path, kind: &str) -> (PathBuf, PathBuf) {
    let glb = helper::cache_file("blender", src, &format!("blender-{VERSION}-v2-{kind}"), "glb");
    let facts = glb.with_extension("json");
    (glb, facts)
}

fn read_facts(p: &Path) -> Vec<(String, String)> {
    let v: serde_json::Value = std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v.as_array().map(|a| a.iter().filter_map(|x| Some((x[0].as_str()?.to_string(), x[1].as_str()?.to_string()))).collect()).unwrap_or_default()
}

/// Formats Blender can import when the page can't read them itself.
pub fn importer_for(kind: &str, ext: &str) -> Option<&'static str> {
    match (kind, ext) {
        ("usd", _) => Some("usd"),
        ("ply", _) => Some("ply"),
        ("gltf", _) => Some("gltf"),
        (_, "fbx") => Some("fbx"),
        (_, "obj") => Some("obj"),
        (_, "stl") => Some("stl"),
        (_, "abc") => Some("abc"),
        _ => None,
    }
}

/// Convert a .blend file to GLB with Blender (cached): the GLB and Blender's facts.
pub fn to_glb(src: &Path, cancel: &CancelToken, status: &dyn Fn(String)) -> Result<(PathBuf, Vec<(String, String)>), String> {
    convert(src, "blend", cancel, status)
}

/// Open (`kind` "blend") or import (`importer_for`) `src` in Blender and write GLB (cached).
pub fn convert(src: &Path, kind: &str, cancel: &CancelToken, status: &dyn Fn(String)) -> Result<(PathBuf, Vec<(String, String)>), String> {
    let (out, facts_path) = cached(src, kind);
    if out.is_file() {
        return Ok((out, read_facts(&facts_path)));
    }
    let mut g = loop {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        if let Some(g) = SERVICE.try_lock_for(Duration::from_millis(100)) {
            break g;
        }
        status("Waiting for Blender…".into());
    };
    if g.is_none() {
        status("Starting Blender…".into());
        *g = Some(start()?);
        idle_watchdog();
    }
    status("Converting with Blender…".into());
    let tmp = out.with_extension("part.glb");
    let _ = std::fs::remove_file(&tmp);
    let svc = g.as_mut().unwrap();
    let id = svc.next_id;
    svc.next_id += 1;
    let req = serde_json::json!({ "id": id, "kind": kind, "src": std::path::absolute(src).unwrap_or(src.to_path_buf()), "out": tmp });
    if writeln!(svc.stdin, "{req}").and_then(|_| svc.stdin.flush()).is_err() {
        *g = None;
        return Err("Blender stopped unexpectedly".into());
    }
    let t = Instant::now();
    let answer = loop {
        if cancel.is_cancelled() {
            // A conversion can't be interrupted: the service is restarted.
            *g = None;
            let _ = std::fs::remove_file(&tmp);
            return Err("cancelled".into());
        }
        if t.elapsed() > TIMEOUT {
            *g = None;
            return Err("Blender took too long to convert this file".into());
        }
        match svc.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(l) => match parse(&l) {
                Some(v) if v["id"].as_u64() == Some(id) => break v,
                _ => continue,
            },
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                *g = None;
                return Err("Blender stopped while converting this file".into());
            }
        }
    };
    svc.last_used = Instant::now();
    drop(g);
    if answer["ok"].as_bool() != Some(true) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Blender could not convert this file: {}", answer["error"].as_str().unwrap_or("unknown error")));
    }
    tracing::info!("{}: Blender opened it in {} ms, GLB written in {} ms total", src.display(), answer["ms_open"], answer["ms"]);
    std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
    let facts = answer["facts"].clone();
    let _ = std::fs::write(&facts_path, facts.to_string());
    Ok((out, read_facts(&facts_path)))
}

pub struct BlendProvider;

impl Provider for BlendProvider {
    fn id(&self) -> &'static str {
        "blend"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.ext_is(EXTS) || !sniff(&p.head) {
            return 0;
        }
        prewarm();
        crate::view::preload();
        95
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let (data, compression) = uncompressed_head(&p.path, &p.head, 1 << 20).ok_or("not a Blender file")?;
        let h = header(&data).ok_or("not a Blender file")?;
        let mut facts = vec![("Format".to_string(), format!("Blender {} file{}", h.version, compression.map(|c| format!(", compressed ({c})")).unwrap_or_default()))];
        let thumb = thumbnail(&data, &h);
        let (glb, facts_path) = cached(&p.path, "blend");
        if glb.is_file() {
            facts.extend(read_facts(&facts_path));
            return Ok(Box::new(ModelLoaded::ready(&p.path, Content::GlbFile(glb), facts, Some(1.0))));
        }
        if !available() {
            return Ok(crate::card(p, "Blender is not installed in deps\\blender, so only the saved preview can be shown.".into(), facts));
        }
        let status = Arc::new(Mutex::new("Starting Blender…".to_string()));
        let path = p.path.clone();
        let s2 = status.clone();
        let upgrade: gt_ui::provider::Upgrade = Box::new(move |cancel: &CancelToken| {
            let r: Converted = match to_glb(&path, cancel, &|s| *s2.lock() = s) {
                Ok((glb, facts)) => Ok((Content::GlbFile(glb), facts, Some(1.0))),
                Err(e) if e == "cancelled" => return None,
                Err(e) => Err(e),
            };
            Some(Box::new(r) as Box<dyn std::any::Any + Send>)
        });
        Ok(Box::new(ModelLoaded { path: p.path.clone(), content: None, facts, unit_meters: Some(1.0), thumb, status: Some(status), upgrade: Some(upgrade) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        let h = header(b"BLENDER17-01v0502REND").unwrap();
        assert_eq!(h.version, "5.2");
        assert!(h.large && h.len == 17);
        let h = header(b"BLENDER-v405REND").unwrap();
        assert_eq!(h.version, "4.5");
        assert!(!h.large && h.ptr8 && h.len == 12);
    }

    #[test]
    fn thumbnail_block() {
        // New layout: code, SDNA index, old pointer, length (u64), count.
        let mut d = b"BLENDER17-01v0502".to_vec();
        let mut test = Vec::new();
        test.extend(2u32.to_le_bytes());
        test.extend(1u32.to_le_bytes());
        test.extend([255, 0, 0, 255, 0, 0, 255, 255]);
        d.extend(b"TEST");
        d.extend(0u32.to_le_bytes());
        d.extend(0u64.to_le_bytes());
        d.extend((test.len() as u64).to_le_bytes());
        d.extend(1u64.to_le_bytes());
        d.extend(test);
        let h = header(&d).unwrap();
        let t = thumbnail(&d, &h).unwrap();
        assert_eq!((t.w, t.h), (2, 1));
        // Red → BGRA (0, 0, 255, 255), blue → (255, 0, 0, 255).
        assert_eq!(t.bgra, vec![0, 0, 255, 255, 255, 0, 0, 255]);
    }
}
