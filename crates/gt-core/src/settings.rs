use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Master switch for the Space hook.
    pub enabled: bool,
    pub start_with_windows: bool,
    /// Hide the preview when an app other than Explorer or the viewer comes to the front.
    pub hide_on_focus_loss: bool,
    /// Alt+Space in Explorer opens the preview in full screen (Quick Look's Option+Space).
    pub alt_space_fullscreen: bool,
    /// Space also previews in Open/Save dialogs.
    pub file_dialogs: bool,
    pub autoplay: bool,
    /// Engine chosen per extension (lower-case, no dot) → provider id.
    pub formats: BTreeMap<String, String>,
    pub display: DisplaySettings,
    pub nfo: NfoSettings,
    pub text: TextSettings,
    pub engines: EngineSettings,
    pub cache: CacheSettings,
    /// Verbose logs (also `GT_LOG`).
    pub debug_log: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct EngineSettings {
    /// Release heavy engines (web view, video player, LibreOffice, Blender)
    /// after this many minutes without use; 0 = keep them.
    pub idle_minutes: u32,
    /// Start the web view and the video player with the previewer, so the first
    /// web page, model or video opens faster (costs memory while idle).
    pub keep_warm: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct CacheSettings {
    /// Conversions kept on disk (LibreOffice PDFs, Blender models...), in MB.
    pub budget_mb: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct DisplaySettings {
    /// Panel peak brightness override in nits (EDID/driver values can be wrong).
    pub peak_nits_override: Option<f32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct NfoSettings {
    /// "dos" (light grey on black), "white", "amber", "green", "light".
    pub theme: String,
    /// 0 = automatic (one VGA pixel per DIP), otherwise a fixed integer scale.
    pub scale: u32,
    /// 8 or 9 pixel cells; 0 = from SAUCE, else 9 like VGA text mode.
    pub letter_spacing: u32,
    /// Honour the SAUCE legacy aspect-ratio flag.
    pub aspect_correction: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct TextSettings {
    pub wrap: bool,
    /// Text files larger than this are previewed partially.
    pub max_preview_bytes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            start_with_windows: true,
            hide_on_focus_loss: true,
            alt_space_fullscreen: true,
            file_dialogs: true,
            autoplay: true,
            formats: BTreeMap::new(),
            display: DisplaySettings::default(),
            nfo: NfoSettings::default(),
            text: TextSettings::default(),
            engines: EngineSettings::default(),
            cache: CacheSettings::default(),
            debug_log: false,
        }
    }
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self { idle_minutes: 10, keep_warm: false }
    }
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self { budget_mb: 4096 }
    }
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self { peak_nits_override: None }
    }
}

impl Default for NfoSettings {
    fn default() -> Self {
        Self { theme: "dos".into(), scale: 0, letter_spacing: 0, aspect_correction: true }
    }
}

impl Default for TextSettings {
    fn default() -> Self {
        Self { wrap: true, max_preview_bytes: 8 * 1024 * 1024 }
    }
}

static IDLE_MINUTES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(10);

/// Minutes an engine may sit unused before it is closed (0: never); set by
/// the viewer from the settings, read by LibreOffice and Blender services.
pub fn idle_minutes() -> u32 {
    IDLE_MINUTES.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_idle_minutes(m: u32) {
    IDLE_MINUTES.store(m, std::sync::atomic::Ordering::Relaxed);
}

impl Settings {
    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("settings.toml")
    }

    /// Load settings, writing the defaults on first run.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("settings.toml invalid ({e}); using defaults");
                Settings::default()
            }),
            Err(_) => {
                let s = Settings::default();
                s.save();
                s
            }
        }
    }

    pub fn save(&self) {
        if let Ok(text) = toml::to_string_pretty(self) {
            // Write then rename: a reader never sees half a file.
            let path = Self::path();
            let tmp = path.with_extension("toml.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    /// The engine chosen for this extension, if any.
    pub fn engine_for(&self, ext: &str) -> Option<&str> {
        self.formats.get(ext).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings page edits the JSON form and sends it back whole.
    #[test]
    fn json_round_trip() {
        let mut s = Settings::default();
        s.formats.insert("svg".into(), "text".into());
        s.display.peak_nits_override = Some(1100.0);
        let mut v = serde_json::to_value(&s).unwrap();
        v["display"]["peak_nits_override"] = serde_json::Value::Null;
        v["engines"]["idle_minutes"] = serde_json::json!(0);
        let back: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(back.engine_for("svg"), Some("text"));
        assert_eq!(back.display.peak_nits_override, None);
        assert_eq!(back.engines.idle_minutes, 0);
        // And as TOML on disk.
        let t = toml::to_string_pretty(&back).unwrap();
        let again: Settings = toml::from_str(&t).unwrap();
        assert_eq!(again.formats.len(), 1);
    }
}
