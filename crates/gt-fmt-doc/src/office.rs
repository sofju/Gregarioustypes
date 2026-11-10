//! LibreOffice (bundled `deps/libreoffice`) as a warm conversion service:
//! its own Python runs `lo_server.py`, which keeps a headless soffice open and
//! converts documents to PDF on request. Results are cached by path, size and
//! mtime, so a document is only ever converted once.

use crate::helper;
use gt_core::CancelToken;
use parking_lot::Mutex;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject};

const SCRIPT: &str = include_str!("../assets/lo_server.py");
const VERSION: &str = "26.8.2";
/// Longest document converted (pages); the rest is left out.
pub const MAX_PAGES: u32 = 1000;

pub fn program_dir() -> PathBuf {
    gt_core::paths::deps_dir().join("libreoffice").join("program")
}

pub fn available() -> bool {
    program_dir().join("soffice.exe").is_file() && program_dir().join("python.exe").is_file()
}

struct Service {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    /// Python and the soffice it starts: terminated together.
    job: HANDLE,
    next_id: u64,
    last_used: Instant,
}

unsafe impl Send for Service {}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{{\"quit\": true}}");
        let _ = self.stdin.flush();
        // soffice gets a moment to shut down cleanly (profile writes), then the job is ended.
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(1500) {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        unsafe {
            let _ = TerminateJobObject(self.job, 0);
            let _ = CloseHandle(self.job);
        }
    }
}

static SERVICE: Mutex<Option<Service>> = Mutex::new(None);

fn file_url(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    let mut out = String::from("file:///");
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn start(status: &dyn Fn(String)) -> Result<Service, String> {
    if !available() {
        return Err("LibreOffice is not installed in deps\\libreoffice.".into());
    }
    let dir = gt_core::paths::cache_dir();
    let script = dir.join("lo_server.py");
    if std::fs::read_to_string(&script).ok().as_deref() != Some(SCRIPT) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&script, SCRIPT).map_err(|e| e.to_string())?;
    }
    let profile = dir.join("lo-profile");
    let first = !profile.exists();
    status(if first { "Setting up LibreOffice (first run)…".into() } else { "Starting LibreOffice…".into() });
    let prog = program_dir();
    let mut cmd = Command::new(prog.join("python.exe"));
    cmd.arg("-u")
        .arg(&script)
        .arg(prog.join("soffice.exe"))
        .arg(file_url(&profile))
        .arg(format!("gt_lo_{}", std::process::id()))
        .current_dir(&prog)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let t = Instant::now();
    let mut child = helper::spawn(&mut cmd, false).map_err(|e| format!("cannot start LibreOffice's Python: {e}"))?;
    let job = unsafe { CreateJobObjectW(None, None) }.map_err(|e| e.to_string())?;
    unsafe {
        let _ = AssignProcessToJobObject(job, HANDLE(child.as_raw_handle()));
    }
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("gt-lo-stdout".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(l) = line else { break };
                if tx.send(l).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let svc = Service { child, stdin, lines: rx, job, next_id: 1, last_used: Instant::now() };
    match svc.lines.recv_timeout(Duration::from_secs(180)) {
        Ok(l) => {
            let v: serde_json::Value = serde_json::from_str(&l).map_err(|e| format!("LibreOffice: {e}: {l}"))?;
            if v["ready"].as_bool() != Some(true) {
                return Err(format!("LibreOffice did not start: {}", v["error"].as_str().unwrap_or("unknown error")));
            }
            tracing::info!("LibreOffice ready in {:.1} s (script reports {} ms)", t.elapsed().as_secs_f64(), v["ms"]);
        }
        Err(_) => return Err("LibreOffice did not start (no answer).".into()),
    }
    Ok(svc)
}

fn idle_watchdog() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("gt-lo-idle".into()).spawn(|| loop {
            std::thread::sleep(Duration::from_secs(30));
            if let Some(mut g) = SERVICE.try_lock() {
                let idle = gt_core::settings::idle_minutes();
                if idle > 0 && g.as_ref().is_some_and(|s| s.last_used.elapsed() > Duration::from_secs(idle as u64 * 60)) {
                    tracing::info!("LibreOffice idle: shutting it down");
                    *g = None;
                }
            }
        });
    });
}

static WARMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Start LibreOffice in the background (an office file is selected or about to
/// be), so the conversion does not also pay for start-up. Cheap when running.
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
    let _ = std::thread::Builder::new().name("gt-lo-warm".into()).spawn(|| {
        let mut g = SERVICE.lock();
        if g.is_none() {
            match start(&|_| {}) {
                Ok(s) => *g = Some(s),
                Err(e) => tracing::warn!("LibreOffice prewarm: {e}"),
            }
        }
        drop(g);
        WARMING.store(false, std::sync::atomic::Ordering::Release);
        idle_watchdog();
    });
}

pub fn cached_pdf(src: &Path) -> PathBuf {
    helper::cache_file("office", src, &format!("lo-{VERSION}-v2-{MAX_PAGES}"), "pdf")
}

/// Convert `src` to PDF (cached). `status` receives progress text.
pub fn to_pdf(src: &Path, cancel: &CancelToken, status: &dyn Fn(String)) -> Result<PathBuf, String> {
    let out = cached_pdf(src);
    if out.is_file() {
        return Ok(out);
    }
    // Wait for the service (another conversion may be running).
    let mut g = loop {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        if let Some(g) = SERVICE.try_lock_for(Duration::from_millis(100)) {
            break g;
        }
        status("Waiting for LibreOffice…".into());
    };
    if out.is_file() {
        return Ok(out);
    }
    if g.as_mut().is_some_and(|s| !matches!(s.child.try_wait(), Ok(None))) {
        *g = None;
    }
    if g.is_none() {
        *g = Some(start(status)?);
        idle_watchdog();
    }
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    status("Converting with LibreOffice…".into());
    let tmp = out.with_extension("part");
    let svc = g.as_mut().unwrap();
    let id = svc.next_id;
    svc.next_id += 1;
    let abs_src = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    let abs_tmp = std::path::absolute(&tmp).unwrap_or_else(|_| tmp.clone());
    let req = serde_json::json!({ "id": id, "in": abs_src.display().to_string(), "out": abs_tmp.display().to_string(), "pages": format!("1-{MAX_PAGES}") });
    if writeln!(svc.stdin, "{req}").and_then(|_| svc.stdin.flush()).is_err() {
        *g = None;
        return Err("LibreOffice stopped unexpectedly.".into());
    }
    let t = Instant::now();
    // A conversion cannot be interrupted; one that was cancelled still completes
    // into the cache, which makes coming back to the file instant.
    let resp = loop {
        match svc.lines.recv_timeout(Duration::from_millis(250)) {
            Ok(l) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) else { continue };
                if v["id"].as_u64() == Some(id) {
                    break v;
                }
            }
            Err(RecvTimeoutError::Timeout) if t.elapsed() < Duration::from_secs(180) => continue,
            Err(RecvTimeoutError::Timeout) => {
                *g = None;
                let _ = std::fs::remove_file(&tmp);
                return Err("LibreOffice took too long and was stopped.".into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                *g = None;
                let _ = std::fs::remove_file(&tmp);
                return Err("LibreOffice stopped while converting this file.".into());
            }
        }
    };
    svc.last_used = Instant::now();
    if resp["ok"].as_bool() == Some(true) && tmp.is_file() {
        std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
        tracing::info!("LibreOffice converted {} in {} ms", src.display(), resp["ms"]);
        Ok(out)
    } else {
        let _ = std::fs::remove_file(&tmp);
        let e = resp["error"].as_str().unwrap_or("conversion failed").to_string();
        tracing::warn!("LibreOffice {}: {e}", src.display());
        Err(friendly(&e))
    }
}

fn friendly(e: &str) -> String {
    // The message quotes the file URL ("Unsupported URL <file:///...>"): judge by the rest.
    let mut l = e.to_ascii_lowercase();
    while let (Some(a), Some(b)) = (l.find('<'), l.find('>')) {
        if b <= a {
            break;
        }
        l.replace_range(a..=b, "");
    }
    if l.contains("password") || l.contains("encrypt") {
        "This document is password protected.".into()
    } else if l.contains("cannot open") || l.contains("ioexception") || l.contains("illegalargument") {
        "LibreOffice cannot open this file (it may be damaged or in an unsupported format).".into()
    } else {
        format!("LibreOffice could not convert this file: {e}")
    }
}
