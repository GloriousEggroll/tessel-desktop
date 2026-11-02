//! Screenshot result preferences shared by Capture and Global Settings
//! (capture_settings.py). The values live in Capture's own config.json beside
//! its window preferences; writers merge single keys so the running window and
//! Global Settings never discard each other's changes.

use gtk::prelude::*;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tessel_core::tr;

pub const KEYS: [&str; 2] = ["copy_to_clipboard", "open_editor"];

pub fn default(key: &str) -> bool {
    key == "copy_to_clipboard"
}

pub fn config_path() -> PathBuf {
    crate::compat::config_root().join("desktop-capture/config.json")
}

fn resolve(path: Option<&Path>) -> PathBuf {
    path.map(Path::to_path_buf).unwrap_or_else(config_path)
}

fn read(path: &Path) -> Map<String, Value> {
    match std::fs::read(path).ok().and_then(|data| serde_json::from_slice::<Value>(&data).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

pub fn get(key: &str, path: Option<&Path>) -> bool {
    match read(&resolve(path)).get(key) {
        Some(Value::Bool(value)) => *value,
        _ => default(key),
    }
}

/// `{key: get(key)}` in DEFAULTS order.
pub fn values(path: Option<&Path>) -> Map<String, Value> {
    let data = read(&resolve(path));
    KEYS.iter().map(|key| {
        let value = match data.get(*key) { Some(Value::Bool(v)) => *v, _ => default(key) };
        (key.to_string(), Value::Bool(value))
    }).collect()
}

/// Merge one key; `mkstemp(prefix='.config-')` + replace (0600).
pub fn set_value(key: &str, value: bool, path: Option<&Path>) -> Result<(), String> {
    if !KEYS.contains(&key) {
        return Err(key.to_string());
    }
    let path = resolve(path);
    let mut data = read(&path);
    data.insert(key.to_string(), Value::Bool(value));
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&parent).map_err(|e| tessel_core::pyerr::os_error_path(&e, &parent))?;
    let text = crate::compat::json_indent2(&Value::Object(data)) + "\n";
    let temporary = crate::media::mkstemp(&parent, ".config-", "").map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
    let result = std::fs::write(&temporary, text).and_then(|_| std::fs::rename(&temporary, &path));
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| tessel_core::pyerr::os_error_text(&e))
}

pub fn copy_enabled(path: Option<&Path>) -> bool {
    get("copy_to_clipboard", path)
}

pub fn editor_enabled(path: Option<&Path>) -> bool {
    get("open_editor", path)
}

pub fn titles(key: &str) -> (&'static str, &'static str) {
    match key {
        "copy_to_clipboard" => ("Copy screenshots to clipboard automatically",
                                "Every new screenshot is also placed on the clipboard as an image."),
        _ => ("Open the editor after taking a screenshot",
              "Annotate or crop each screenshot before sharing it. The notification Edit button always opens it."),
    }
}

/// A labelled switch bound to one preference; returns (row, switch).
pub fn switch_row(key: &'static str, path: Option<PathBuf>) -> (gtk::Box, gtk::Switch) {
    let (title, detail) = titles(key);
    let row = gtk::Box::builder().spacing(16).build();
    let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(3).hexpand(true).build();
    labels.append(&gtk::Label::builder().label(tr!(title)).xalign(0.0).wrap(true).build());
    let hint = gtk::Label::builder().label(tr!(detail)).xalign(0.0).wrap(true).max_width_chars(50).build();
    hint.add_css_class("subtitle");
    hint.add_css_class("dim-label");
    labels.append(&hint);
    row.append(&labels);
    let switch = gtk::Switch::builder().active(get(key, path.as_deref())).valign(gtk::Align::Center).build();
    tessel_gtk::utils::accessible(&switch, &tr!(title));
    switch.connect_active_notify(move |widget| {
        if let Err(error) = set_value(key, widget.is_active(), path.as_deref()) {
            eprintln!("tessel-capture: could not save {key}: {error}");
        }
    });
    row.append(&switch);
    (row, switch)
}

/// Self-contained controls for a Global Settings "Screenshots" section.
pub struct SettingsPage {
    pub page: gtk::Box,
    pub switches: HashMap<&'static str, gtk::Switch>,
}

pub fn build_page(path: Option<PathBuf>) -> SettingsPage {
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).build();
    tessel_gtk::utils::set_margins(&page, 20);
    let heading = gtk::Label::builder().label(tr!("Screenshots")).xalign(0.0).build();
    heading.add_css_class("title-2");
    page.append(&heading);
    let mut switches = HashMap::new();
    for key in KEYS {
        let (row, switch) = switch_row(key, path.clone());
        page.append(&row);
        switches.insert(key, switch);
    }
    SettingsPage { page, switches }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_merge_into_shared_config() {
        let directory = std::env::temp_dir().join(format!("capture-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let config = directory.join("config").join("config.json");
        assert!(copy_enabled(Some(&config)));
        assert!(!editor_enabled(Some(&config)));
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, r#"{"delay": 3, "open_editor": "yes"}"#).unwrap();
        assert!(!editor_enabled(Some(&config)), "non-bool values fall back to the default");
        set_value("copy_to_clipboard", false, Some(&config)).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{\n  \"delay\": 3,\n  \"open_editor\": \"yes\",\n  \"copy_to_clipboard\": false\n}\n");
        assert!(set_value("delay", true, Some(&config)).is_err());
        let values = values(Some(&config));
        assert_eq!(serde_json::Value::Object(values), serde_json::json!({"copy_to_clipboard": false, "open_editor": false}));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(directory);
    }
}
