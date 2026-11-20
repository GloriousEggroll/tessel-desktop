//! `~/.config/tessel/shell/wallpapers.json` (CONFIG_FILE) and
//! `~/.config/tessel/shell/desktop_view_order.json`
//! (ORDER_CONFIG). Byte format matches Python's `json.dump`: indent=2 for the
//! wallpapers file, default separators (`", "`, `": "`) for positions.

use super::icon_layout::{Point, Positions};
use serde_json::{Map, Value};
use std::path::PathBuf;
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const MODES: [&str; 6] = ["Fill", "Fit", "Stretch", "Center", "Tile", "Color Only"];
pub const VIEW_PREFS: &str = "_view_prefs";

pub const INDENT2: JsonStyle = JsonStyle { indent: Some(2), ..JsonStyle::DEFAULT };

pub fn config_file() -> PathBuf {
    tessel_core::paths::component_config_dir("shell").join("wallpapers.json")
}

pub fn order_config() -> PathBuf {
    tessel_core::paths::component_config_dir("shell").join("desktop_view_order.json")
}

/// The folder shown on the desktop: the XDG desktop folder
/// (`XDG_DESKTOP_DIR` in `~/.config/user-dirs.dirs`, which has a translated
/// name in many languages), or `~/Desktop` when there is none.
pub fn desktop_dir() -> PathBuf {
    let home = tessel_core::paths::home();
    std::fs::read_to_string(tessel_core::paths::config_home().join("user-dirs.dirs")).ok()
        .and_then(|text| xdg_desktop_dir(&text, &home))
        .unwrap_or_else(|| home.join("Desktop"))
}

/// `XDG_DESKTOP_DIR` from the text of `user-dirs.dirs`. The last assignment
/// wins. A relative path, and the home folder itself (how xdg-user-dirs
/// writes "no desktop folder"), give `None`.
pub fn xdg_desktop_dir(text: &str, home: &std::path::Path) -> Option<PathBuf> {
    let value = text.lines().rev().find_map(|line| {
        let value = line.trim().strip_prefix("XDG_DESKTOP_DIR")?.trim_start().strip_prefix('=')?.trim();
        value.strip_prefix('"')?.strip_suffix('"')
    })?;
    // The shell escapes xdg-user-dirs writes: a backslash before the next character.
    let mut unescaped = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        unescaped.push(if c == '\\' { chars.next()? } else { c });
    }
    let path = match unescaped.strip_prefix("$HOME") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(&unescaped),
    };
    let path: PathBuf = path.components().collect();
    (path.is_absolute() && path != home).then_some(path)
}

/// Make the desktop folder when it is missing (a new account before
/// xdg-user-dirs has run, or a folder the user removed).
pub fn ensure_desktop_dir() -> PathBuf {
    let path = desktop_dir();
    if let Err(error) = std::fs::create_dir_all(&path) {
        eprintln!("could not create {}: {error}", path.display());
    }
    path
}

/// `_read_cfg`: `{}` when missing or unreadable.
pub fn read_cfg() -> Map<String, Value> {
    let path = config_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(map)) => map,
            Ok(_) => Map::new(),
            Err(error) => {
                eprintln!("read_cfg failed: {error}");
                Map::new()
            }
        },
        Err(_) => Map::new(),
    }
}

/// Non-atomic rewrite, exactly like `json.dump(data, f, indent=2)`.
pub fn write_cfg(data: &Map<String, Value>) -> std::io::Result<()> {
    let path = config_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, json_dumps(&Value::Object(data.clone()), &INDENT2))
}

/// One monitor's entry as written by Python.
pub fn wallpaper_entry(file_path: Option<&str>, mode: &str, color: &str) -> Value {
    let mut entry = Map::new();
    entry.insert("file_path".into(), file_path.map(|p| Value::String(p.into())).unwrap_or(Value::Null));
    entry.insert("mode".into(), Value::String(mode.into()));
    entry.insert("color".into(), Value::String(color.into()));
    Value::Object(entry)
}

/// `_apply_wallpaper_to_keys`' persistence: read (any error → {}), update
/// the keys, rewrite. `color` is `Gdk.RGBA.to_string()`.
pub fn merge_wallpapers(keys: &[String], file_path: Option<&str>, mode: &str, color: &str) -> std::io::Result<()> {
    let mut data = match std::fs::read(config_file()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let persist = if mode == "Color Only" { None } else { file_path };
    for key in keys {
        data.insert(key.clone(), wallpaper_entry(persist, mode, color));
    }
    write_cfg(&data)
}

/// (hide_icons, show_hidden) from `_view_prefs` (Python `bool()` of values).
pub fn load_view_prefs() -> (bool, bool) {
    let data = read_cfg();
    let prefs = data.get(VIEW_PREFS).cloned().unwrap_or(Value::Object(Map::new()));
    let flag = |key: &str| prefs.get(key).map(tessel_core::pyjson::truthy).unwrap_or(false);
    (flag("hide_icons"), flag("show_hidden"))
}

/// `_save_view_prefs`: merge into the file, keeping every other key.
pub fn save_view_prefs(hide_icons: bool, show_hidden: bool) -> std::io::Result<()> {
    let mut data = read_cfg();
    let prefs = data.entry(VIEW_PREFS.to_owned()).or_insert_with(|| Value::Object(Map::new()));
    if !prefs.is_object() {
        // Python would raise here (item assignment on a non-dict); keep the file.
        return Ok(());
    }
    let prefs = prefs.as_object_mut().unwrap();
    prefs.insert("hide_icons".into(), Value::Bool(hide_icons));
    prefs.insert("show_hidden".into(), Value::Bool(show_hidden));
    write_cfg(&data)
}

// ---------------------------------------------------------------- positions

/// `load_saved_positions`: `{}` on any error (or non-object content).
pub fn load_positions() -> Positions {
    match std::fs::read(order_config()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        Some(Value::Object(map)) => map,
        _ => Positions::new(),
    }
}

fn write_positions(positions: &Positions) -> std::io::Result<()> {
    let path = order_config();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, json_dumps(&Value::Object(positions.clone()), &JsonStyle::DEFAULT))
}

/// `save_all_positions`: file monitors must not be woken for unchanged
/// placement snapshots. Returns true when the file was written.
pub fn save_all_positions(positions: &Positions) -> bool {
    if let Some(current) = std::fs::read(order_config()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if current == Value::Object(positions.clone()) {
            return false;
        }
    }
    write_positions(positions).is_ok()
}

/// `save_icon_position` after clamping: always rewrites.
pub fn store_position(name: &str, point: Point) {
    let mut positions = load_positions();
    positions.insert(name.to_owned(), super::icon_layout::point_value(point));
    let _ = write_positions(&positions);
}

/// `_write_positions_vertical_fill` persistence (always rewrites).
pub fn write_all_positions(positions: &Positions) {
    let _ = write_positions(positions);
}

#[cfg(test)]
mod tests {
    #[test]
    fn desktop_folder_follows_user_dirs() {
        let home = std::path::Path::new("/home/u");
        let dir = |text: &str| super::xdg_desktop_dir(text, home);
        assert_eq!(dir("XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n"), Some("/home/u/Desktop".into()));
        assert_eq!(dir("# comment\nXDG_DOWNLOAD_DIR=\"$HOME/DL\"\n  XDG_DESKTOP_DIR = \"$HOME/デスクトップ\"\n"), Some("/home/u/デスクトップ".into()));
        assert_eq!(dir("XDG_DESKTOP_DIR=\"/mnt/data/My Desk\"\n"), Some("/mnt/data/My Desk".into()));
        assert_eq!(dir("XDG_DESKTOP_DIR=\"$HOME/a\\\"b\"\n"), Some("/home/u/a\"b".into()));
        // The last assignment wins.
        assert_eq!(dir("XDG_DESKTOP_DIR=\"$HOME/A\"\nXDG_DESKTOP_DIR=\"$HOME/B\"\n"), Some("/home/u/B".into()));
        // Disabled (the home folder), relative, unquoted or missing: no XDG desktop folder.
        assert_eq!(dir("XDG_DESKTOP_DIR=\"$HOME/\"\n"), None);
        assert_eq!(dir("XDG_DESKTOP_DIR=\"$HOME\"\n"), None);
        assert_eq!(dir("XDG_DESKTOP_DIR=\"Desktop\"\n"), None);
        assert_eq!(dir("XDG_DESKTOP_DIR=$HOME/Desktop\n"), None);
        assert_eq!(dir("XDG_DESKTOP_DIRS=\"$HOME/x\"\nXDG_MUSIC_DIR=\"$HOME/Music\"\n"), None);
        assert_eq!(dir("XDG_DESKTOP_DIR=\"$HOMEdir/x\"\n"), None);
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn byte_formats_match_python_json_dump() {
        let data = json!({"HDMI-A-1": {"file_path": null, "mode": "Color Only", "color": "rgb(0,0,0)"}, "_view_prefs": {"hide_icons": false, "show_hidden": true}, "x": "é"});
        // python3 -c 'import json;print(json.dumps(d, indent=2), end="")'
        assert_eq!(json_dumps(&data, &INDENT2), "{\n  \"HDMI-A-1\": {\n    \"file_path\": null,\n    \"mode\": \"Color Only\",\n    \"color\": \"rgb(0,0,0)\"\n  },\n  \"_view_prefs\": {\n    \"hide_icons\": false,\n    \"show_hidden\": true\n  },\n  \"x\": \"\\u00e9\"\n}");
        let positions = json!({"computer.desktop": [12, 22], "user-home.desktop": [12, 150]});
        assert_eq!(json_dumps(&positions, &JsonStyle::DEFAULT), "{\"computer.desktop\": [12, 22], \"user-home.desktop\": [12, 150]}");
    }
}
