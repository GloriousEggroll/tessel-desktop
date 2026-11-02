//! Clipboard preferences, `~/.config/tessel/clipboard.json`:
//!
//! ```json
//! {"version": 1, "keep_clipboard": true, "keep_primary": false, "skip_secrets": true, "max_mb": 64}
//! ```
//!
//! The tessel-clipboard daemon re-reads the file when it changes; Global Settings
//! embeds [`page`] (feature `page`).

#[cfg(feature = "page")]
pub mod page;

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Marks a string for the translation catalogs without translating it here.
#[macro_export]
macro_rules! n_ {
    ($s:expr) => {
        $s
    };
}

pub const FILE_NAME: &str = "clipboard.json";
pub const MAX_MB_LIMIT: u32 = 1024;

pub fn default_path() -> PathBuf {
    tessel_core::paths::suite_config_dir().join(FILE_NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Keep the clipboard (Ctrl+C) after the application that copied it closes.
    pub keep_clipboard: bool,
    /// The same for the middle-click (primary) selection.
    pub keep_primary: bool,
    /// Never keep copies a password manager marks as secret.
    pub skip_secrets: bool,
    /// Largest copy (all of its formats together) that is kept, in MiB.
    pub max_mb: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { keep_clipboard: true, keep_primary: false, skip_secrets: true, max_mb: 64 }
    }
}

pub const BOOL_KEYS: &[&str] = &["keep_clipboard", "keep_primary", "skip_secrets"];

impl Settings {
    pub fn from_map(map: &Map<String, Value>) -> Self {
        let d = Settings::default();
        let boolean = |key: &str, default: bool| map.get(key).and_then(Value::as_bool).unwrap_or(default);
        Settings {
            keep_clipboard: boolean("keep_clipboard", d.keep_clipboard),
            keep_primary: boolean("keep_primary", d.keep_primary),
            skip_secrets: boolean("skip_secrets", d.skip_secrets),
            max_mb: map.get("max_mb").and_then(Value::as_u64).map(|v| v.clamp(1, u64::from(MAX_MB_LIMIT)) as u32)
                .unwrap_or(d.max_mb),
        }
    }

    pub fn to_map(&self) -> Map<String, Value> {
        match json!({
            "version": 1,
            "keep_clipboard": self.keep_clipboard,
            "keep_primary": self.keep_primary,
            "skip_secrets": self.skip_secrets,
            "max_mb": self.max_mb,
        }) {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    pub fn get_bool(&self, key: &str) -> bool {
        match key {
            "keep_clipboard" => self.keep_clipboard,
            "keep_primary" => self.keep_primary,
            "skip_secrets" => self.skip_secrets,
            _ => false,
        }
    }

    pub fn set_bool(&mut self, key: &str, value: bool) {
        match key {
            "keep_clipboard" => self.keep_clipboard = value,
            "keep_primary" => self.keep_primary = value,
            "skip_secrets" => self.skip_secrets = value,
            _ => {}
        }
    }

    pub fn max_bytes(&self) -> usize {
        self.max_mb as usize * 1024 * 1024
    }
}

/// The settings file: reading never creates it; writes merge into the
/// current file (unknown keys are kept) and replace it atomically.
#[derive(Clone, Debug)]
pub struct Store {
    pub path: PathBuf,
}

impl Store {
    pub fn new(path: Option<PathBuf>) -> Self {
        Store { path: path.unwrap_or_else(default_path) }
    }

    fn read_map(path: &Path) -> Map<String, Value> {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned()).unwrap_or_default()
    }

    pub fn load(&self) -> Settings {
        Settings::from_map(&Self::read_map(&self.path))
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        let mut map = Self::read_map(&self.path);
        for (key, value) in settings.to_map() {
            map.insert(key, value);
        }
        let text = serde_json::to_string_pretty(&Value::Object(map)).map_err(|e| e.to_string())? + "\n";
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        tessel_core::clock::atomic_write(&self.path, text.as_bytes(), ".clipboard-").map_err(|e| e.to_string())
    }

    /// Change the stored settings with `edit` (re-read first so concurrent
    /// writers do not undo each other).
    pub fn update(&self, edit: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let mut settings = self.load();
        edit(&mut settings);
        self.save(&settings)?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clipboard-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_keep_the_clipboard_only() {
        let s = Settings::default();
        assert!(s.keep_clipboard && s.skip_secrets && !s.keep_primary);
        assert_eq!(s.max_bytes(), 64 * 1024 * 1024);
    }

    #[test]
    fn lenient_parsing_and_round_trip() {
        let map = serde_json::from_str::<Value>(r#"{"keep_clipboard": "x", "keep_primary": true, "max_mb": 0}"#)
            .unwrap().as_object().cloned().unwrap();
        let s = Settings::from_map(&map);
        assert!(s.keep_clipboard && s.keep_primary);
        assert_eq!(s.max_mb, 1);
        assert_eq!(Settings::from_map(&s.to_map()), s);
        let huge = serde_json::from_str::<Value>(r#"{"max_mb": 999999}"#).unwrap().as_object().cloned().unwrap();
        assert_eq!(Settings::from_map(&huge).max_mb, MAX_MB_LIMIT);
    }

    #[test]
    fn store_merges_and_keeps_unknown_keys() {
        let dir = temp("store");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"future_key": 7, "skip_secrets": false}"#).unwrap();
        let store = Store::new(Some(path.clone()));
        assert!(!store.load().skip_secrets);
        store.update(|s| s.set_bool("keep_primary", true)).unwrap();
        let map = Store::read_map(&path);
        assert_eq!(map["future_key"], 7);
        assert_eq!(map["skip_secrets"], false);
        assert_eq!(map["keep_primary"], true);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_is_not_created_by_reading() {
        let dir = temp("missing");
        let store = Store::new(Some(dir.join("sub").join(FILE_NAME)));
        assert_eq!(store.load(), Settings::default());
        assert!(!store.path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
