//! Archive Manager preferences: `~/.config/desktop-suite/archive-manager.json`
//! (Ark's Configure dialog: General, Extraction, Plugins, Previews), the
//! plugin table, and the GTK settings page that both the app's Settings
//! dialog and Global Settings embed (they edit the same file; each reloads
//! when the other writes it).

pub mod page;
pub mod plugins;

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Mark a string constant for translation (the catalog scanner collects it).
#[macro_export]
macro_rules! n_ {
    ($s:expr) => {
        $s
    };
}

pub const FILE_NAME: &str = "archive-manager.json";
pub const MAX_RECENT: usize = 12;

/// `$XDG_CONFIG_HOME/desktop-suite/archive-manager.json`.
pub fn default_path() -> PathBuf {
    suite_core::paths::suite_config_dir().join(FILE_NAME)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    // General
    pub show_info_panel: bool,
    // Extraction
    pub open_destination: bool,
    pub close_after_extraction: bool,
    pub preserve_paths: bool,
    pub extract_subfolder: bool,
    // Plugins: ids of disabled backends.
    pub disabled_plugins: Vec<String>,
    // Previews
    pub internal_preview: bool,
    pub limit_preview_size: bool,
    pub preview_max_mb: u32,
    // State (not shown in the settings page)
    pub recent: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show_info_panel: true,
            open_destination: false,
            close_after_extraction: false,
            preserve_paths: true,
            extract_subfolder: true,
            disabled_plugins: Vec::new(),
            internal_preview: true,
            limit_preview_size: true,
            preview_max_mb: 50,
            recent: Vec::new(),
        }
    }
}

/// Keys of the boolean settings, in page order.
pub const BOOL_KEYS: &[&str] = &["show_info_panel", "open_destination", "close_after_extraction", "preserve_paths",
                                 "extract_subfolder", "internal_preview", "limit_preview_size"];

fn boolean(map: &Map<String, Value>, key: &str, default: bool) -> bool {
    map.get(key).and_then(Value::as_bool).unwrap_or(default)
}

impl Settings {
    /// Lenient: unknown keys are ignored, wrong types fall back to defaults.
    pub fn from_map(map: &Map<String, Value>) -> Self {
        let d = Settings::default();
        let strings = |key: &str| -> Vec<String> {
            map.get(key).and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string).collect())
                .unwrap_or_default()
        };
        let mut recent = strings("recent");
        recent.dedup();
        recent.truncate(MAX_RECENT);
        Settings {
            show_info_panel: boolean(map, "show_info_panel", d.show_info_panel),
            open_destination: boolean(map, "open_destination", d.open_destination),
            close_after_extraction: boolean(map, "close_after_extraction", d.close_after_extraction),
            preserve_paths: boolean(map, "preserve_paths", d.preserve_paths),
            extract_subfolder: boolean(map, "extract_subfolder", d.extract_subfolder),
            disabled_plugins: strings("disabled_plugins").into_iter().filter(|p| plugins::find(p).is_some()).collect(),
            internal_preview: boolean(map, "internal_preview", d.internal_preview),
            limit_preview_size: boolean(map, "limit_preview_size", d.limit_preview_size),
            preview_max_mb: map.get("preview_max_mb").and_then(Value::as_u64).map(|v| v.clamp(1, 10_000) as u32)
                .unwrap_or(d.preview_max_mb),
            recent,
        }
    }

    pub fn to_map(&self) -> Map<String, Value> {
        let value = json!({
            "version": 1,
            "show_info_panel": self.show_info_panel,
            "open_destination": self.open_destination,
            "close_after_extraction": self.close_after_extraction,
            "preserve_paths": self.preserve_paths,
            "extract_subfolder": self.extract_subfolder,
            "disabled_plugins": self.disabled_plugins,
            "internal_preview": self.internal_preview,
            "limit_preview_size": self.limit_preview_size,
            "preview_max_mb": self.preview_max_mb,
            "recent": self.recent,
        });
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    pub fn get_bool(&self, key: &str) -> bool {
        match key {
            "show_info_panel" => self.show_info_panel,
            "open_destination" => self.open_destination,
            "close_after_extraction" => self.close_after_extraction,
            "preserve_paths" => self.preserve_paths,
            "extract_subfolder" => self.extract_subfolder,
            "internal_preview" => self.internal_preview,
            "limit_preview_size" => self.limit_preview_size,
            _ => false,
        }
    }

    pub fn set_bool(&mut self, key: &str, value: bool) {
        match key {
            "show_info_panel" => self.show_info_panel = value,
            "open_destination" => self.open_destination = value,
            "close_after_extraction" => self.close_after_extraction = value,
            "preserve_paths" => self.preserve_paths = value,
            "extract_subfolder" => self.extract_subfolder = value,
            "internal_preview" => self.internal_preview = value,
            "limit_preview_size" => self.limit_preview_size = value,
            _ => {}
        }
    }

    pub fn plugin_enabled(&self, id: &str) -> bool {
        !self.disabled_plugins.iter().any(|p| p == id)
    }

    pub fn set_plugin_enabled(&mut self, id: &str, enabled: bool) {
        self.disabled_plugins.retain(|p| p != id);
        if !enabled {
            self.disabled_plugins.push(id.to_string());
            self.disabled_plugins.sort();
        }
    }

    pub fn preview_limit_bytes(&self) -> Option<u64> {
        self.limit_preview_size.then(|| u64::from(self.preview_max_mb) * 1024 * 1024)
    }

    pub fn add_recent(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_string());
        self.recent.truncate(MAX_RECENT);
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
        suite_core::clock::atomic_write(&self.path, text.as_bytes(), ".archive-manager-").map_err(|e| e.to_string())
    }

    /// Change the stored settings with `edit` (re-read first so concurrent
    /// writers - the app and Global Settings - do not undo each other).
    pub fn update(&self, edit: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let mut settings = self.load();
        edit(&mut settings);
        self.save(&settings)?;
        Ok(settings)
    }

    /// Call `changed` whenever the file is written (by anyone).
    pub fn watch(&self, changed: impl Fn() + 'static) -> Option<gio::FileMonitor> {
        let directory = self.path.parent()?.to_path_buf();
        let _ = std::fs::create_dir_all(&directory);
        let monitor = gio::File::for_path(&directory)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>).ok()?;
        let name = self.path.file_name()?.to_os_string();
        use gio::prelude::*;
        monitor.connect_changed(move |_, file, other, event| {
            let hit = |f: Option<&gio::File>| f.and_then(|f| f.basename()).is_some_and(|b| b.as_os_str() == name);
            let relevant = matches!(event, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created
                | gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedIn | gio::FileMonitorEvent::Renamed);
            if relevant && (hit(Some(file)) || hit(other)) {
                changed();
            }
        });
        Some(monitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("archive-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_match_ark() {
        let s = Settings::default();
        assert!(s.show_info_panel && s.preserve_paths && s.extract_subfolder && s.internal_preview);
        assert!(!s.open_destination && !s.close_after_extraction);
        assert_eq!(s.preview_limit_bytes(), Some(50 * 1024 * 1024));
    }

    #[test]
    fn lenient_parsing_and_round_trip() {
        let map = serde_json::from_str::<Value>(r#"{"show_info_panel": "x", "open_destination": true,
            "disabled_plugins": ["cli7z", "bogus", 3], "preview_max_mb": 0, "recent": ["/a", "/a", "/b"]}"#)
            .unwrap().as_object().cloned().unwrap();
        let s = Settings::from_map(&map);
        assert!(s.show_info_panel);
        assert!(s.open_destination);
        assert_eq!(s.disabled_plugins, vec!["cli7z"]);
        assert_eq!(s.preview_max_mb, 1);
        assert_eq!(s.recent, vec!["/a", "/b"]);
        assert_eq!(Settings::from_map(&s.to_map()), s);
    }

    #[test]
    fn store_merges_and_keeps_unknown_keys() {
        let dir = temp("store");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"future_key": 7, "preserve_paths": false}"#).unwrap();
        let store = Store::new(Some(path.clone()));
        assert!(!store.load().preserve_paths);
        store.update(|s| s.set_plugin_enabled("clizip", false)).unwrap();
        let map = Store::read_map(&path);
        assert_eq!(map["future_key"], 7);
        assert_eq!(map["preserve_paths"], false);
        assert_eq!(map["disabled_plugins"], json!(["clizip"]));
        store.update(|s| s.set_plugin_enabled("clizip", true)).unwrap();
        assert!(store.load().plugin_enabled("clizip"));
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

    #[test]
    fn recent_is_bounded_and_most_recent_first() {
        let mut s = Settings::default();
        for i in 0..20 {
            s.add_recent(&format!("/r{i}"));
        }
        s.add_recent("/r5");
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0], "/r5");
    }
}
