//! Keep the native focus outline in sync with per-monitor desktop accents
//! (tiling_outline.py `OutlineSync`).

use crate::client::{ClientError, Reply};
use crate::compat::{as_int, eq_bool, truthy};
use regex::Regex;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

pub type Call = Arc<dyn Fn(&str) -> Result<Reply, ClientError> + Send + Sync>;

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,120}$").unwrap());

pub fn preferences_path() -> PathBuf { tessel_core::paths::config_home().join("tessel-tiling/config.json") }

pub fn palette_path() -> PathBuf { tessel_core::paths::cache_home().join("tessel-dock/palette.json") }

/// `OutlineSync.read`: a JSON object, else `{}` (missing, > 1 MiB, invalid).
pub fn read_object(path: &Path) -> Map<String, Value> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() <= 1024 * 1024 => {}
        _ => return Map::new(),
    }
    let Ok(text) = std::fs::read(path) else { return Map::new() };
    let Ok(text) = String::from_utf8(text) else { return Map::new() };
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

type Snapshot = (Option<Value>, bool, Vec<(String, i64, i64, i64)>);

pub struct OutlineSync {
    pub preferences: PathBuf,
    pub palette: PathBuf,
    call: Call,
    previous: Option<Snapshot>,
}

impl OutlineSync {
    pub fn new(preferences: Option<PathBuf>, palette: Option<PathBuf>, call: Call) -> Self {
        Self { preferences: preferences.unwrap_or_else(preferences_path), palette: palette.unwrap_or_else(palette_path),
               call, previous: None }
    }

    /// Valid `(name, r, g, b)` monitor accents (at most 64).
    pub fn colors(&self) -> Vec<(String, i64, i64, i64)> {
        let palette = read_object(&self.palette);
        let Some(Value::Array(monitors)) = palette.get("monitors") else { return Vec::new() };
        let mut colors = Vec::new();
        for monitor in monitors.iter().take(64) {
            let Value::Object(monitor) = monitor else { continue };
            let (Some(Value::String(name)), Some(Value::Array(color))) = (monitor.get("name"), monitor.get("accent")) else {
                continue;
            };
            if !NAME.is_match(name) || color.len() != 3 {
                continue;
            }
            let channels: Vec<i64> = color.iter().filter_map(|c| as_int(Some(c))).filter(|c| (0..=255).contains(c)).collect();
            if channels.len() == 3 {
                colors.push((name.clone(), channels[0], channels[1], channels[2]));
            }
        }
        colors
    }

    /// `sync(state)`: false when the compositor has no outline support.
    pub fn sync(&mut self, state: &Reply) -> Result<bool, ClientError> {
        if !truthy(state.get("outline_supported")) {
            return Ok(false);
        }
        let preferences = read_object(&self.preferences);
        // `.get('focus_outline', True) is not False`
        let enabled = !matches!(preferences.get("focus_outline"), Some(Value::Bool(false)));
        let colors = self.colors();
        let snapshot: Snapshot = (state.get("compositor_pid").cloned(), enabled, colors.clone());
        let outline_matches = eq_bool(state.get("outline_enabled"), enabled);
        if self.previous.as_ref() == Some(&snapshot) && outline_matches {
            return Ok(true);
        }
        if !outline_matches {
            (self.call)(if enabled { "outline on" } else { "outline off" })?;
        }
        for (name, red, green, blue) in &colors {
            (self.call)(&format!("outline-color {name} {red} {green} {blue}"))?;
        }
        self.previous = Some(snapshot);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn recorder() -> (Call, Arc<Mutex<Vec<String>>>) {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = calls.clone();
        (Arc::new(move |command: &str| {
            log.lock().unwrap().push(command.into());
            Ok(Map::new())
        }), calls)
    }

    fn take(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> { std::mem::take(&mut *calls.lock().unwrap()) }

    fn object(value: Value) -> Reply { value.as_object().unwrap().clone() }

    #[test]
    fn palette_changes_are_per_monitor_and_idle_sync_is_quiet() {
        let root = crate::testing::scratch("outline-palette");
        let (prefs, palette) = (root.join("preferences.json"), root.join("palette.json"));
        std::fs::write(&prefs, r#"{"enabled":true,"focus_outline":true}"#).unwrap();
        std::fs::write(&palette, json!({"monitors": [
            {"name": "DP-2", "accent": [120, 40, 190]},
            {"name": "HDMI-A-1", "accent": [40, 180, 80]},
            {"name": "bad\noutline off", "accent": [1, 2, 3]}]}).to_string()).unwrap();
        let (call, calls) = recorder();
        let mut sync = OutlineSync::new(Some(prefs.clone()), Some(palette.clone()), call);
        let state = object(json!({"outline_supported": true, "outline_enabled": true, "compositor_pid": 123}));
        sync.sync(&state).unwrap();
        assert_eq!(take(&calls), ["outline-color DP-2 120 40 190", "outline-color HDMI-A-1 40 180 80"]);
        sync.sync(&state).unwrap();
        assert!(take(&calls).is_empty());
        std::fs::write(&palette, r#"{"monitors":[{"name":"DP-2","accent":[3,2,1]}]}"#).unwrap();
        sync.sync(&state).unwrap();
        assert_eq!(take(&calls), ["outline-color DP-2 3 2 1"]);
        std::fs::write(&prefs, r#"{"focus_outline":false}"#).unwrap();
        sync.sync(&state).unwrap();
        assert!(take(&calls).contains(&"outline off".to_owned()));
        assert!(!sync.sync(&Map::new()).unwrap());
        assert!(take(&calls).is_empty());
        let mut next = state.clone();
        next.insert("compositor_pid".into(), json!(124));
        sync.sync(&next).unwrap();
        assert!(take(&calls).contains(&"outline off".to_owned()));
    }

    #[test]
    fn invalid_palettes_send_no_colors() {
        let root = crate::testing::scratch("outline-invalid");
        let palette = root.join("palette.json");
        let (call, calls) = recorder();
        let mut sync = OutlineSync::new(Some(root.join("none.json")), Some(palette.clone()), call);
        let state = object(json!({"outline_supported": true, "outline_enabled": true}));
        for text in ["[1]", "{\"monitors\": {}}", "not json",
                     r#"{"monitors":[{"name":"A","accent":[1,2]},{"name":"B","accent":[1,2,256]},{"name":"C","accent":[1,2,true]},{"name":"D","accent":[1.0,2,3]},{"name":"","accent":[1,2,3]}]}"#] {
            std::fs::write(&palette, text).unwrap();
            sync.previous = None;
            sync.sync(&state).unwrap();
            assert!(take(&calls).is_empty(), "{text}");
        }
        std::fs::write(&palette, format!("{{\"monitors\":[{}]}}", vec![r#"{"name":"X","accent":[1,2,3]}"#; 70].join(","))).unwrap();
        sync.previous = None;
        sync.sync(&state).unwrap();
        assert_eq!(take(&calls).len(), 64);
    }
}
