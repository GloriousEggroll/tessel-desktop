//! Validated application preferences and interoperable GTK bookmarks
//! (file_preferences.py).
//!
//! Reading never creates or modifies files. Every write is atomic and serialized
//! with other instances of this app (`<file>.lock`), retaining valid existing GTK
//! bookmark lines.

use crate::error::{Error, Result};
use crate::fsx;
use crate::pyos::{prefix_chars, splitlines};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::pyjson::{is_py_space, py_strip};

/// `DEFAULTS` in Python key order.
pub static DEFAULTS: LazyLock<Map<String, Value>> = LazyLock::new(|| {
    let value = json!({
        "view": "icons", "icon_size": 64, "sort": "name", "descending": false,
        "folders_first": true, "natural_sort": true, "group_by": "none",
        "show_hidden": false, "show_previews": true, "show_statusbar": true,
        "show_sidebar": true, "show_information": true, "remember_folders": false,
        "split_view": false, "location_editable": false, "single_click": false,
        "confirm_delete": true, "restore_session": true, "tabs": [], "tab_splits": [],
        "active_tab": 0, "window_size": [1100, 760], "bookmarks": [], "folders": {},
        "search_saved": [], "information_width": 360, "folder_peek": true, "vim_keys": false,
    });
    match value {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
});

pub const FOLDER_KEYS: [&str; 9] = ["view", "icon_size", "sort", "descending", "folders_first",
                                    "natural_sort", "group_by", "show_hidden", "show_previews"];

fn choices(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "view" => Some(&["icons", "compact", "details", "columns"]),
        "sort" => Some(&["name", "size", "modified", "type"]),
        "group_by" => Some(&["none", "name", "type", "size", "modified"]),
        _ => None,
    }
}

const PREFERENCES_STYLE: JsonStyle = JsonStyle { ensure_ascii: false, sort_keys: false, indent: Some(2),
                                                 item_separator: ",", key_separator: ": " };

/// `$XDG_CONFIG_HOME` when absolute, else `~/.config` (pure form for tests).
fn config_home_from(value: Option<std::ffi::OsString>, home: &Path) -> PathBuf {
    match value {
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => home.join(".config"),
    }
}

fn config_home() -> PathBuf {
    config_home_from(std::env::var_os("XDG_CONFIG_HOME"), &suite_core::paths::home())
}

/// Default preferences file: `$XDG_CONFIG_HOME/desktop-suite/file-manager.json`.
pub fn default_preferences_path() -> PathBuf {
    config_home().join("desktop-suite/file-manager.json")
}

/// Default GTK bookmarks file: `$XDG_CONFIG_HOME/gtk-3.0/bookmarks`.
pub fn default_bookmarks_path() -> PathBuf {
    config_home().join("gtk-3.0/bookmarks")
}

/// `^[A-Za-z][A-Za-z0-9+.-]*:`, at most 8192 characters, no whitespace or NUL.
pub fn valid_uri(value: &str) -> bool {
    let count = value.chars().count();
    if count == 0 || count > 8192 || value.chars().any(|c| is_py_space(c) || c == '\0') {
        return false;
    }
    let bytes = value.as_bytes();
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    let mut index = 1;
    while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || b"+.-".contains(&bytes[index])) {
        index += 1;
    }
    bytes.get(index) == Some(&b':')
}

fn valid_uri_value(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(text)) if valid_uri(text))
}

fn label(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => {
            let cleaned = text.replace('\0', "").replace(['\r', '\n'], " ");
            prefix_chars(py_strip(&cleaned), 1024).to_string()
        }
        _ => String::new(),
    }
}

/// `type(value) is int` (JSON integer, not bool/float).
fn py_int(value: &Value) -> Option<i128> {
    match value {
        Value::Number(number) => number.as_i64().map(i128::from).or_else(|| number.as_u64().map(i128::from)),
        _ => None,
    }
}

fn option(key: &str, value: &Value) -> Value {
    let default = &DEFAULTS[key];
    if default.is_boolean() {
        return if value.is_boolean() { value.clone() } else { default.clone() };
    }
    if let Some(allowed) = choices(key) {
        return match value {
            Value::String(text) if allowed.contains(&text.as_str()) => value.clone(),
            _ => default.clone(),
        };
    }
    if key == "icon_size" {
        return py_int(value).map_or_else(|| default.clone(), |v| json!(v.clamp(16, 256) as i64));
    }
    if key == "information_width" {
        return py_int(value).map_or_else(|| default.clone(), |v| json!(v.clamp(300, 800) as i64));
    }
    default.clone()
}

fn bookmarks(values: Option<&Value>) -> Vec<Value> {
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let Some(Value::Array(values)) = values else { return result };
    for entry in values.iter().take(512) {
        let Value::Object(entry) = entry else { continue };
        if !valid_uri_value(entry.get("uri")) {
            continue;
        }
        let uri = entry["uri"].as_str().unwrap_or_default().to_string();
        if seen.insert(uri.clone()) {
            let mut item = Map::new();
            item.insert("uri".into(), Value::String(uri));
            item.insert("label".into(), Value::String(label(Some(entry.get("label").unwrap_or(&json!(""))))));
            result.push(Value::Object(item));
        }
    }
    result
}

/// Normalize untrusted disk data; unknown keys are intentionally ignored.
pub fn validate(values: &Value) -> Map<String, Value> {
    let empty = Map::new();
    let values = values.as_object().unwrap_or(&empty);
    let mut result = DEFAULTS.clone();
    for (key, default) in DEFAULTS.iter() {
        if default.is_boolean() || choices(key).is_some() || key == "icon_size" || key == "information_width" {
            result.insert(key.clone(), option(key, values.get(key).unwrap_or(default)));
        }
    }
    let mut tabs = Vec::new();
    let mut tab_splits = Vec::new();
    if let Some(Value::Array(source)) = values.get("tabs") {
        let splits = match values.get("tab_splits") {
            Some(Value::Array(splits)) => splits.as_slice(),
            _ => &[],
        };
        for (index, uri) in source.iter().take(64).enumerate() {
            if let Value::String(text) = uri {
                if valid_uri(text) {
                    tabs.push(uri.clone());
                    let split = splits.get(index);
                    tab_splits.push(if valid_uri_value(split) { split.cloned().unwrap_or_default() } else { json!("") });
                }
            }
        }
    }
    let tab_count = tabs.len() as i128;
    result.insert("tabs".into(), Value::Array(tabs));
    result.insert("tab_splits".into(), Value::Array(tab_splits));
    if let Some(active) = values.get("active_tab").map_or(Some(0), py_int) {
        result.insert("active_tab".into(), json!(active.min((tab_count - 1).max(0)).max(0) as i64));
    }
    if let Some(Value::Array(size)) = values.get("window_size") {
        if size.len() == 2 {
            if let (Some(width), Some(height)) = (py_int(&size[0]), py_int(&size[1])) {
                result.insert("window_size".into(),
                              json!([width.clamp(640, 7680) as i64, height.clamp(400, 4320) as i64]));
            }
        }
    }
    result.insert("bookmarks".into(), Value::Array(bookmarks(values.get("bookmarks"))));
    let mut folders = Map::new();
    if let Some(Value::Object(source)) = values.get("folders") {
        for (uri, options) in source.iter().take(4096) {
            if let (true, Value::Object(options)) = (valid_uri(uri), options) {
                let mut normal = Map::new();
                for key in FOLDER_KEYS {
                    if let Some(value) = options.get(key) {
                        normal.insert(key.into(), option(key, value));
                    }
                }
                folders.insert(uri.clone(), Value::Object(normal));
            }
        }
    }
    result.insert("folders".into(), Value::Object(folders));
    let mut searches = Vec::new();
    if let Some(Value::Array(source)) = values.get("search_saved") {
        for search in source.iter().take(128) {
            let Value::Object(search) = search else { continue };
            let (true, Some(Value::String(query))) = (valid_uri_value(search.get("uri")), search.get("query")) else {
                continue;
            };
            let mut name = label(search.get("name"));
            if name.is_empty() {
                name = prefix_chars(query, 128).to_string();
            }
            let mut item = Map::new();
            item.insert("name".into(), json!(name));
            item.insert("uri".into(), search["uri"].clone());
            item.insert("query".into(), json!(prefix_chars(query, 4096)));
            item.insert("contents".into(), json!(search.get("contents") == Some(&Value::Bool(true))));
            searches.push(Value::Object(item));
        }
    }
    result.insert("search_saved".into(), Value::Array(searches));
    result
}

fn dumps(data: &Map<String, Value>) -> String {
    json_dumps(&Value::Object(data.clone()), &PREFERENCES_STYLE) + "\n"
}

fn unknown_keys<'a>(keys: impl Iterator<Item = &'a String>, allowed: &[&str]) -> Vec<String> {
    let mut unknown: Vec<String> = keys.filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
    unknown.sort();
    unknown.dedup();
    unknown
}

/// `~/.config/desktop-suite/file-manager.json` (or an explicit path).
#[derive(Clone, Debug)]
pub struct FilePreferences {
    pub path: PathBuf,
    pub data: Map<String, Value>,
}

impl FilePreferences {
    pub fn new(path: Option<PathBuf>) -> Self {
        let path = path.unwrap_or_else(default_preferences_path);
        let data = Self::read_path(&path);
        FilePreferences { path, data }
    }

    fn read_path(path: &Path) -> Map<String, Value> {
        let Ok(meta) = std::fs::metadata(path) else { return validate(&Value::Null) };
        if meta.len() > 8 * 1024 * 1024 {
            return validate(&Value::Null);
        }
        let Ok(bytes) = std::fs::read(path) else { return validate(&Value::Null) };
        let Ok(text) = String::from_utf8(bytes) else { return validate(&Value::Null) };
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => validate(&value),
            Err(_) => validate(&Value::Null),
        }
    }

    /// A copy of the value (`Value::Null` when missing, like `get(key)` → None).
    pub fn get(&self, key: &str) -> Value {
        self.data.get(key).cloned().unwrap_or(Value::Null)
    }

    pub fn get_or(&self, key: &str, default: Value) -> Value {
        self.data.get(key).cloned().unwrap_or(default)
    }

    pub fn set(&mut self, key: &str, value: Value) -> Result<()> {
        let mut map = Map::new();
        map.insert(key.into(), value);
        self.update(map)
    }

    /// Persist a partial update without replacing another window's options.
    pub fn update(&mut self, changes: Map<String, Value>) -> Result<()> {
        let allowed: Vec<&str> = DEFAULTS.keys().map(String::as_str).collect();
        let unknown = unknown_keys(changes.keys(), &allowed);
        if !unknown.is_empty() {
            return Err(Error::key(format!("Unknown file manager preferences: {}", unknown.join(", "))));
        }
        let path = self.path.clone();
        fsx::locked(&path, || {
            let mut current = Self::read_path(&path);
            for (key, value) in changes {
                current.insert(key, value);
            }
            self.data = validate(&Value::Object(current));
            fsx::atomic_write(&path, dumps(&self.data).as_bytes(), true)
        })
    }

    /// Persist explicit edits to `.data`; set/update are preferred for merging.
    pub fn save(&mut self) -> Result<()> {
        let path = self.path.clone();
        fsx::locked(&path, || {
            self.data = validate(&Value::Object(self.data.clone()));
            fsx::atomic_write(&path, dumps(&self.data).as_bytes(), true)
        })
    }

    pub fn folder_options(&self, uri: &str) -> Map<String, Value> {
        let mut options = Map::new();
        for key in FOLDER_KEYS {
            options.insert(key.into(), self.data.get(key).cloned().unwrap_or(Value::Null));
        }
        if let Some(Value::Object(saved)) = self.data.get("folders").and_then(|f| f.get(uri)) {
            for (key, value) in saved {
                options.insert(key.clone(), value.clone());
            }
        }
        options
    }

    pub fn set_folder_options(&mut self, uri: &str, values: Map<String, Value>) -> Result<()> {
        if !valid_uri(uri) {
            return Err(Error::value("A folder URI is required"));
        }
        let unknown = unknown_keys(values.keys(), &FOLDER_KEYS);
        if !unknown.is_empty() {
            return Err(Error::key(format!("Unknown folder options: {}", unknown.join(", "))));
        }
        let path = self.path.clone();
        fsx::locked(&path, || {
            let mut current = Self::read_path(&path);
            let folders = current.entry("folders").or_insert_with(|| json!({}));
            if let Value::Object(folders) = folders {
                let options = folders.entry(uri.to_string()).or_insert_with(|| json!({}));
                if let Value::Object(options) = options {
                    for (key, value) in &values {
                        options.insert(key.clone(), option(key, value));
                    }
                }
            }
            self.data = validate(&Value::Object(current));
            fsx::atomic_write(&path, dumps(&self.data).as_bytes(), true)
        })
    }
}

/// One GTK bookmark (`{"uri": …, "label": …}` in Python).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub uri: String,
    pub label: String,
}

/// `~/.config/gtk-3.0/bookmarks`: `<uri>[ <label>]` lines; others are retained.
#[derive(Clone, Debug)]
pub struct GTKBookmarks {
    pub path: PathBuf,
}

impl GTKBookmarks {
    pub fn new(path: Option<PathBuf>) -> Self {
        GTKBookmarks { path: path.unwrap_or_else(default_bookmarks_path) }
    }

    fn lines(&self) -> Vec<String> {
        let Ok(bytes) = std::fs::read(&self.path) else { return Vec::new() };
        let Ok(text) = String::from_utf8(bytes) else { return Vec::new() };
        splitlines(&text).into_iter().map(str::to_string).collect()
    }

    fn parse(line: &str) -> Option<Bookmark> {
        let (uri, label) = match line.split_once(' ') {
            Some((uri, label)) => (uri, label),
            None => (line, ""),
        };
        valid_uri(uri).then(|| Bookmark { uri: uri.to_string(), label: label.to_string() })
    }

    fn parse_uri(line: &str) -> Option<String> {
        Self::parse(line).map(|b| b.uri)
    }

    pub fn list(&self) -> Vec<Bookmark> {
        let entries: Vec<Value> = self.lines().iter().filter_map(|line| Self::parse(line))
            .map(|b| json!({"uri": b.uri, "label": b.label})).collect();
        bookmarks(Some(&Value::Array(entries))).into_iter()
            .map(|v| Bookmark { uri: v["uri"].as_str().unwrap_or_default().into(),
                                label: v["label"].as_str().unwrap_or_default().into() })
            .collect()
    }

    /// Append a bookmark; an existing URI keeps its label and position.
    pub fn add(&self, uri: &str, label_text: &str) -> Result<()> {
        if !valid_uri(uri) {
            return Err(Error::value("A valid bookmark URI is required"));
        }
        fsx::locked(&self.path, || {
            let mut lines = self.lines();
            if lines.iter().any(|line| Self::parse_uri(line).as_deref() == Some(uri)) {
                return Ok(());
            }
            let label_text = label(Some(&json!(label_text)));
            lines.push(if label_text.is_empty() { uri.to_string() } else { format!("{uri} {label_text}") });
            fsx::atomic_write(&self.path, (lines.join("\n") + "\n").as_bytes(), true)
        })
    }

    pub fn remove(&self, uri: &str) -> Result<()> {
        fsx::locked(&self.path, || {
            let lines = self.lines();
            let updated: Vec<String> = lines.iter().filter(|line| Self::parse_uri(line).as_deref() != Some(uri))
                .cloned().collect();
            if updated != lines {
                let text = updated.join("\n") + if updated.is_empty() { "" } else { "\n" };
                fsx::atomic_write(&self.path, text.as_bytes(), true)?;
            }
            Ok(())
        })
    }

    pub fn rename(&self, uri: &str, label_text: &str) -> Result<()> {
        let label_text = label(Some(&json!(label_text)));
        fsx::locked(&self.path, || {
            let lines = self.lines();
            let replacement = if label_text.is_empty() { uri.to_string() } else { format!("{uri} {label_text}") };
            let updated: Vec<String> = lines.iter()
                .map(|line| if Self::parse_uri(line).as_deref() == Some(uri) { replacement.clone() } else { line.clone() })
                .collect();
            if updated != lines {
                fsx::atomic_write(&self.path, (updated.join("\n") + "\n").as_bytes(), true)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
