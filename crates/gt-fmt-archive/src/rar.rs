//! Files inside RAR archives come out through the official 7-Zip program
//! (`deps/7zip/7z.exe`), run as a separate process. 7-Zip's RAR decoders carry
//! the unRAR licence restriction, which isn't compatible with the GPL, so the
//! 7z.dll loaded by this process is built without them (scripts/fetch-deps.ps1);
//! RAR archives still open and list through it.

use crate::sevenzip::Progress;
use gt_core::CancelToken;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 7-Zip's handler names for RAR 1.5–4 and RAR 5 archives.
pub fn is_rar(format: &str) -> bool {
    format.eq_ignore_ascii_case("rar") || format.eq_ignore_ascii_case("rar5")
}

fn exe() -> PathBuf {
    gt_core::paths::deps_dir().join("7zip").join("7z.exe")
}

/// Extract `entry` (its path inside the archive, `size` bytes) of the RAR
/// archive at `archive` into `sink`.
pub fn extract(archive: &Path, entry: &str, size: u64, sink: &mut dyn Write, progress: &Progress, cancel: &CancelToken) -> Result<(), String> {
    let exe = exe();
    if !exe.is_file() {
        return Err("files inside RAR archives need 7-Zip (deps\\7zip\\7z.exe)".into());
    }
    let mut child = Command::new(&exe)
        // e: extract; -so: to stdout; -spd: the name is literal, not a wildcard;
        // -p-: never ask for a password; -bd: no progress indicator.
        .args(["e", "-so", "-spd", "-p-", "-bd", "-y", "--"])
        .arg(archive)
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("7-Zip didn't start: {e}"))?;
    let mut out = child.stdout.take().ok_or("no output pipe")?;
    let mut err = child.stderr.take().ok_or("no error pipe")?;
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    // Ends the process when the preview is cancelled or the reader goes away.
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let stop = stop.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || loop {
            if stop.load(Ordering::Relaxed) || cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return None,
            }
        })
    };
    progress.total_bytes.store(size, Ordering::Relaxed);
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut failed = None;
    loop {
        match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = sink.write_all(&buf[..n]) {
                    failed = Some(e.to_string());
                    break;
                }
                done += n as u64;
                progress.done_bytes.store(done, Ordering::Relaxed);
            }
            Err(e) => {
                failed = Some(e.to_string());
                break;
            }
        }
    }
    if failed.is_some() {
        stop.store(true, Ordering::Relaxed);
    }
    drop(out);
    let status = watcher.join().ok().flatten();
    let errors = errors.join().unwrap_or_default();
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    if let Some(e) = failed {
        return Err(e);
    }
    // 0: done; 1: warnings only (e.g. data after the end of the archive).
    match status.and_then(|s| s.code()) {
        Some(0) => Ok(()),
        Some(1) if done == size => Ok(()),
        _ => {
            let lower = errors.to_ascii_lowercase();
            if lower.contains("wrong password") || lower.contains("encrypted") {
                return Err("this file is encrypted (password required)".into());
            }
            let detail = errors.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("no details");
            tracing::warn!("7z.exe on {}: {}", archive.display(), errors.trim());
            Err(format!("7-Zip couldn't extract it ({detail})"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RAR samples of the corpus (WinRAR / rar.exe made), when present.
    fn samples() -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/archives")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rar")) {
                    out.push(p);
                }
            }
        }
        out
    }

    #[test]
    fn extracts_through_7z_exe() {
        if !exe().is_file() {
            return;
        }
        let cancel = CancelToken::new();
        let mut compressed = 0;
        for path in samples() {
            let head = std::fs::read(&path).unwrap();
            let listing = crate::listing::open(&path, &head[..head.len().min(4096)], Arc::new(Progress::default()), &cancel).unwrap();
            let access = listing.access.as_ref().unwrap();
            for file in listing.entries.iter().filter(|e| !e.is_dir && !e.encrypted && e.size > 0).take(3) {
                let mut data = Vec::new();
                crate::listing::extract(access, file.id, &mut data, Arc::new(Progress::default()), &cancel).unwrap();
                assert_eq!(data.len() as u64, file.size, "{} in {}", file.path, path.display());
            }
            // The 7z.dll in this process has no RAR decoders (no unRAR code):
            // compressed (not stored) items fail in-process.
            if let crate::listing::Access::Archive(arc) = access {
                let o = arc.lock();
                for e in o.chain[0].entries(&cancel).iter().filter(|e| !e.is_dir && !e.method.contains("m0")) {
                    let r = o.chain[0].extract(e.index, Box::new(Vec::new()), Arc::new(Progress::default()), &cancel);
                    assert_eq!(r.err().as_deref(), Some("unsupported compression method"), "{}", e.path);
                    compressed += 1;
                }
            }
        }
        if !samples().is_empty() {
            assert!(compressed > 0, "no compressed RAR item in the samples");
        }
    }
}
