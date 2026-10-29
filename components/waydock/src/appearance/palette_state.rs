//! `$XDG_CACHE_HOME/waydock/palette.json` publisher (palette_state.py).
//!
//! `PaletteState::publish` writes compact, key-sorted JSON with a trailing
//! newline atomically (`.palette-*` temporary + replace), only when the bytes
//! differ from the last publication — or, after a restart, from the file
//! already on disk. Failures are best effort: they set `error` and return
//! `false`, never interrupting the dock.

use super::fsx::{self, error_text, PathError};
use serde_json::{json, Map, Value};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_gtk::palette::{Palette, Rgb};

/// `palette_path()`: `$XDG_CACHE_HOME/waydock/palette.json` (else `~/.cache`).
pub fn palette_path() -> PathBuf {
    suite_core::paths::cache_home().join("waydock").join("palette.json")
}

/// Application appearance keywords of `publish` (Python defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppOptions {
    pub apps_transparency: f64,
    pub apps_transparency_enabled: bool,
    pub apps_frosted: bool,
    pub apps_frost_strength: f64,
    pub apps_rounded_corners: bool,
    pub colored_window_buttons: bool,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self { apps_transparency: 5.0, apps_transparency_enabled: false, apps_frosted: false,
               apps_frost_strength: 50.0, apps_rounded_corners: true, colored_window_buttons: false }
    }
}

/// One `monitors[]` entry: `{"name", "geometry", **color_palette(...)}`.
pub fn monitor_entry(name: &str, geometry: [i64; 4], palette: &Palette) -> Value {
    json!({"name": name, "geometry": geometry, "background": palette.background,
           "foreground": palette.foreground, "accent": palette.accent})
}

/// RGB triple as a JSON array (Python tuples/lists both encode as arrays).
pub fn rgb_value(color: Rgb) -> Value {
    json!(color)
}

fn percent(value: f64) -> i64 {
    fsx::py_round(value).clamp(0.0, 100.0) as i64
}

const STYLE: JsonStyle = JsonStyle { ensure_ascii: false, sort_keys: true, indent: None, item_separator: ",", key_separator: ":" };

/// The exact bytes Python writes for one publication.
pub fn encode(is_dark: bool, transparency: f64, monitors: &[Value], options: &AppOptions) -> Vec<u8> {
    let mut payload = Map::new();
    payload.insert("version".into(), json!(1));
    payload.insert("is_dark".into(), json!(is_dark));
    payload.insert("transparency".into(), json!(percent(transparency)));
    payload.insert("apps_transparency".into(), json!(percent(options.apps_transparency)));
    payload.insert("apps_transparency_enabled".into(), json!(options.apps_transparency_enabled));
    payload.insert("apps_frosted".into(), json!(options.apps_frosted));
    payload.insert("apps_frost_strength".into(), json!(percent(options.apps_frost_strength)));
    payload.insert("apps_rounded_corners".into(), json!(options.apps_rounded_corners));
    payload.insert("colored_window_buttons".into(), json!(options.colored_window_buttons));
    payload.insert("monitors".into(), Value::Array(monitors.to_vec()));
    let mut text = json_dumps(&Value::Object(payload), &STYLE);
    text.push('\n');
    text.into_bytes()
}

type ReplaceHook = Box<dyn Fn(&Path, &Path) -> io::Result<()>>;

pub struct PaletteState {
    pub path: PathBuf,
    /// `str(OSError)` of the last failed publication, else `None`.
    pub error: Option<String>,
    written: Option<Vec<u8>>,
    replace: Option<ReplaceHook>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new(None)
    }
}

impl PaletteState {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path: path.unwrap_or_else(palette_path), error: None, written: None, replace: None }
    }

    /// Test hook replacing `os.replace` (Python tests patch it).
    pub fn set_replace_hook(&mut self, hook: Option<ReplaceHook>) {
        self.replace = hook;
    }

    /// Publish a snapshot; `true` only when the file was replaced.
    pub fn publish(&mut self, is_dark: bool, transparency: f64, monitors: &[Value], options: &AppOptions) -> bool {
        let encoded = encode(is_dark, transparency, monitors, options);
        if self.written.as_deref() == Some(&encoded[..]) {
            return false;
        }
        // Avoid a spurious change on service restart too. Later publications
        // compare in memory, including frequently refreshed window metadata.
        if self.written.is_none() {
            match std::fs::read(&self.path) {
                Ok(current) if current == encoded => {
                    self.written = Some(encoded);
                    self.error = None;
                    return false;
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    self.error = Some(error_text(&PathError::wrap(error, &self.path)));
                    return false;
                }
            }
        }
        let mut temporary = None;
        let result = (|| -> io::Result<()> {
            let parent = self.path.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(parent).map_err(|e| PathError::wrap(e, parent))?;
            let (mut file, name) = fsx::mkstemp(parent, ".palette-", "")?;
            temporary = Some(name.clone());
            file.write_all(&encoded)?;
            drop(file);
            match &self.replace {
                Some(hook) => hook(&name, &self.path),
                None => fsx::replace(&name, &self.path),
            }
        })();
        if let Some(name) = temporary {
            let _ = std::fs::remove_file(name);
        }
        match result {
            Ok(()) => {
                self.written = Some(encoded);
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error_text(&error));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn root(name: &str) -> PathBuf {
        let base = std::env::temp_dir()
            .join(format!("d-palette-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn monitors() -> Vec<Value> {
        vec![
            json!({"name": "DP-1", "geometry": [0, 0, 1920, 1080], "background": [20, 24, 29],
                   "foreground": [240, 241, 243], "accent": [70, 140, 250]}),
            json!({"name": "HDMI-A-1", "geometry": [-2560, 0, 2560, 1440], "background": [30, 20, 25],
                   "foreground": [245, 238, 243], "accent": [180, 95, 140]}),
        ]
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn opts() -> AppOptions {
        AppOptions::default()
    }

    #[test]
    fn snapshot_contains_each_monitor_and_rgb_arrays() {
        let path = root("snapshot").join("cache/waydock/palette.json");
        let mut state = PaletteState::new(Some(path.clone()));
        assert!(state.publish(true, 35.0, &monitors(), &opts()));
        let data = read(&path);
        assert_eq!(data["version"], 1);
        assert_eq!(data["is_dark"], true);
        assert_eq!(data["transparency"], 35);
        assert_eq!(data["monitors"][1]["geometry"], json!([-2560, 0, 2560, 1440]));
        assert_eq!(data["monitors"][0]["background"], json!([20, 24, 29]));
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"}\n"));
        assert!(bytes.starts_with(b"{\"apps_frost_strength\":50,\"apps_frosted\":false,"));
    }

    #[test]
    fn unchanged_payload_and_restart_do_not_replace_file() {
        let path = root("unchanged").join("cache/waydock/palette.json");
        let mut state = PaletteState::new(Some(path.clone()));
        state.publish(true, 20.0, &monitors(), &opts());
        let before = std::fs::metadata(&path).unwrap();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let hook = |calls: std::rc::Rc<std::cell::Cell<i32>>| -> ReplaceHook {
            Box::new(move |_, _| { calls.set(calls.get() + 1); Ok(()) })
        };
        state.set_replace_hook(Some(hook(calls.clone())));
        assert!(!state.publish(true, 20.0, &monitors(), &opts()));
        let mut restarted = PaletteState::new(Some(path.clone()));
        restarted.set_replace_hook(Some(hook(calls.clone())));
        assert!(!restarted.publish(true, 20.0, &monitors(), &opts()));
        assert_eq!(calls.get(), 0);
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!((before.ino(), before.mtime_nsec(), before.mtime()), (after.ino(), after.mtime_nsec(), after.mtime()));
    }

    #[test]
    fn changed_preview_geometry_and_monitor_removal_publish() {
        let path = root("changed").join("palette.json");
        let mut state = PaletteState::new(Some(path.clone()));
        let mut entries = monitors();
        state.publish(true, 20.0, &entries, &opts());
        assert!(state.publish(false, 61.0, &entries, &opts()));
        assert_eq!(read(&path)["is_dark"], false);
        assert_eq!(read(&path)["transparency"], 61);
        entries[1]["geometry"] = json!([1920, 200, 2560, 1440]);
        assert!(state.publish(false, 61.0, &entries, &opts()));
        assert_eq!(read(&path)["monitors"][1]["geometry"], json!([1920, 200, 2560, 1440]));
        assert!(state.publish(true, 20.0, &entries[..1], &opts()));
        assert_eq!(read(&path)["monitors"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn app_preferences_publish_independently_of_dock_previews() {
        let path = root("apps").join("palette.json");
        let mut state = PaletteState::new(Some(path.clone()));
        let enabled = AppOptions { apps_transparency: 35.0, apps_transparency_enabled: true, ..opts() };
        state.publish(true, 80.0, &monitors(), &enabled);
        let data = read(&path);
        assert_eq!((data["transparency"].clone(), data["apps_transparency"].clone()), (json!(80), json!(35)));
        assert_eq!(data["apps_transparency_enabled"], true);
        assert!(state.publish(true, 0.0, &monitors(), &enabled));
        assert_eq!(read(&path)["apps_transparency"], 35);
        assert!(state.publish(true, 0.0, &monitors(), &AppOptions { apps_transparency_enabled: false, ..enabled }));
        assert_eq!(read(&path)["apps_transparency_enabled"], false);
        assert_eq!(read(&path)["apps_transparency"], 35);
    }

    #[test]
    fn failed_replace_preserves_complete_old_snapshot_and_can_retry() {
        let path = root("failed").join("palette.json");
        let mut state = PaletteState::new(Some(path.clone()));
        state.publish(true, 20.0, &monitors(), &opts());
        let before = std::fs::read(&path).unwrap();
        let (expected_dir, expected_before) = (path.parent().unwrap().to_owned(), before.clone());
        state.set_replace_hook(Some(Box::new(move |source, destination| {
            // The complete new payload exists before the old inode is replaced.
            assert_eq!(read(source)["transparency"], 80);
            assert_eq!(source.parent().unwrap(), expected_dir);
            assert_eq!(std::fs::read(destination).unwrap(), expected_before);
            Err(io::Error::other("Read-only cache"))
        })));
        assert!(!state.publish(false, 80.0, &monitors(), &opts()));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(state.error.as_deref(), Some("Read-only cache"));
        let listing: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(listing, [path.clone()]);
        state.set_replace_hook(None);
        assert!(state.publish(false, 80.0, &monitors(), &opts()));
        assert!(state.error.is_none());
        assert_eq!(read(&path)["transparency"], 80);
    }

    #[test]
    fn frost_texture_and_app_corners_are_published_with_each_monitor() {
        let path = root("frost").join("palette.json");
        let mut state = PaletteState::new(Some(path.clone()));
        let mut entries = monitors();
        entries[0]["frost_texture"] = json!("/tmp/frost-a.png");
        entries[1]["frost_texture"] = json!("/tmp/frost-b.png");
        let frosted = AppOptions { apps_frosted: true, apps_rounded_corners: false, ..opts() };
        assert!(state.publish(true, 15.0, &entries, &frosted));
        let data = read(&path);
        assert_eq!(data["apps_frosted"], true);
        assert_eq!(data["apps_rounded_corners"], false);
        assert_eq!(data["monitors"][1]["frost_texture"], "/tmp/frost-b.png");
        assert!(!state.publish(true, 15.0, &entries, &frosted));
        assert!(state.publish(true, 15.0, &entries, &AppOptions { apps_rounded_corners: true, ..frosted }));
    }

    #[test]
    fn unavailable_cache_is_nonfatal_and_recoverable() {
        let base = root("blocked");
        let path = base.join("cache/waydock/palette.json");
        std::fs::create_dir_all(base.join("cache")).unwrap();
        std::fs::write(base.join("cache/waydock"), "Directory blocked by a file").unwrap();
        let mut state = PaletteState::new(Some(path.clone()));
        assert!(!state.publish(true, 20.0, &monitors(), &opts()));
        assert!(state.error.as_deref().is_some_and(|e| !e.is_empty()), "{:?}", state.error);
        std::fs::remove_file(base.join("cache/waydock")).unwrap();
        assert!(state.publish(true, 20.0, &monitors(), &opts()));
        assert!(state.error.is_none());
    }

    #[test]
    fn strength_change_is_published_without_changing_transparency() {
        let path = root("strength").join("palette.json");
        let mut state = PaletteState::new(Some(path.clone()));
        let base = AppOptions { apps_frosted: true, apps_frost_strength: 68.0, apps_transparency: 37.0, ..opts() };
        state.publish(true, 15.0, &monitors(), &base);
        assert_eq!(read(&path)["apps_frost_strength"], 68);
        assert!(state.publish(true, 15.0, &monitors(), &AppOptions { apps_frost_strength: 22.0, ..base }));
        assert_eq!(read(&path)["apps_frost_strength"], 22);
        assert_eq!(read(&path)["apps_transparency"], 37);
    }

    #[test]
    fn values_are_rounded_half_even_and_clamped() {
        let text = String::from_utf8(encode(true, 2.5, &[], &AppOptions { apps_transparency: 150.0, apps_frost_strength: -3.0, ..opts() })).unwrap();
        assert!(text.contains("\"transparency\":2,"));
        assert!(text.contains("\"apps_transparency\":100,"));
        assert!(text.contains("\"apps_frost_strength\":0,"));
        let unicode = String::from_utf8(encode(true, 0.0, &[json!({"name": "Écran"})], &opts())).unwrap();
        assert!(unicode.contains("\"name\":\"Écran\""), "ensure_ascii=False");
    }
}
