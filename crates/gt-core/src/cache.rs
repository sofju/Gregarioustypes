//! The disk cache of conversions (LibreOffice PDFs, Blender models, Ghostscript
//! renders, BDInfo reports): measured, cleared on request, and kept under the
//! budget by removing the least recently used files.

use std::path::PathBuf;
use std::time::SystemTime;

/// Cache folders holding conversion results (not engine profiles or web data).
pub const CONVERSION_DIRS: &[&str] = &["office", "blender", "gs", "bdinfo"];

fn files() -> Vec<(PathBuf, u64, SystemTime)> {
    let root = crate::paths::cache_dir();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = CONVERSION_DIRS.iter().map(|d| root.join(d)).collect();
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                // Last use: access time when the volume keeps it, else modification.
                let t = m.accessed().or_else(|_| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((e.path(), m.len(), t));
            }
        }
    }
    out
}

/// Bytes used by conversion results.
pub fn size() -> u64 {
    files().iter().map(|f| f.1).sum()
}

/// Remove every conversion result; returns the bytes freed.
pub fn clear() -> u64 {
    let mut freed = 0;
    for (p, len, _) in files() {
        if std::fs::remove_file(&p).is_ok() {
            freed += len;
        }
    }
    freed
}

/// Remove the least recently used results until the cache fits `budget` bytes.
pub fn trim(budget: u64) -> u64 {
    let mut f = files();
    let total: u64 = f.iter().map(|x| x.1).sum();
    if total <= budget {
        return 0;
    }
    f.sort_by_key(|x| x.2);
    let mut over = total - budget;
    let mut freed = 0;
    for (p, len, _) in f {
        if over == 0 {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            freed += len;
            over = over.saturating_sub(len);
        }
    }
    tracing::info!("cache trimmed: {} freed", crate::fmt::size(freed));
    freed
}
