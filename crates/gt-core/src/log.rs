use std::fs::OpenOptions;
use std::sync::Mutex;

/// Log to %LOCALAPPDATA%\Gregarioustypes\logs\<name>.log (truncated when > 4 MB).
pub fn init(name: &str) {
    let path = crate::paths::log_dir().join(format!("{name}.log"));
    if std::fs::metadata(&path).map(|m| m.len() > 4 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("old.log"));
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else { return };
    let default = if crate::settings::Settings::load().debug_log { "debug" } else { "info" };
    let filter = tracing_subscriber::EnvFilter::try_from_env("GT_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_target(false).with_writer(Mutex::new(file)).try_init();
}
