use std::path::PathBuf;

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// %APPDATA%\Gregarioustypes (settings).
pub fn config_dir() -> PathBuf {
    let d = env_dir("APPDATA").join("Gregarioustypes");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// %LOCALAPPDATA%\Gregarioustypes (cache, logs, per-file state).
pub fn data_dir() -> PathBuf {
    let d = env_dir("LOCALAPPDATA").join("Gregarioustypes");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn cache_dir() -> PathBuf {
    let d = data_dir().join("cache");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn log_dir() -> PathBuf {
    let d = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Directory holding the executable; bundled `deps/` and `assets/` live next to it
/// (or two levels up when running from `target/<profile>/`).
pub fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default()
}

/// Locate the bundled dependency folder: `<exe>/deps`, else `<repo>/deps` in dev builds.
/// (Cargo's own `target/<profile>/deps` is skipped: it holds build artefacts.)
pub fn deps_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let exe = exe_dir();
        let ours = |d: &PathBuf| d.join("mpv").is_dir() || d.join("mediainfo").is_dir() || d.join("7zip-lgpl").is_dir();
        if let Some(d) = std::env::var_os("GT_DEPS").map(PathBuf::from) {
            if ours(&d) {
                return d;
            }
        }
        // `<exe>/deps` when installed; the repository's `deps` from `target/<profile>[/examples]`.
        for cand in exe.ancestors().take(4).map(|a| a.join("deps")) {
            if ours(&cand) {
                return std::fs::canonicalize(&cand).map(strip_verbatim).unwrap_or(cand);
            }
        }
        exe.join("deps")
    })
    .clone()
}

/// `\\?\C:\x` → `C:\x` (some libraries choke on verbatim paths).
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}
