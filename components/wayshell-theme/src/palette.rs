//! Waydock palette cache reader of wayshell_theme.py (`palette_path`,
//! `read_palette`, `monitor_palette`).

use crate::css::{frost_texture_path, Colors};
use serde_json::{Map, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

/// `$XDG_CACHE_HOME/waydock/palette.json` (`~/.cache` when unset or empty).
pub fn palette_path() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| suite_core::paths::home().join(".cache"));
    cache.join("waydock").join("palette.json")
}

/// One validated `monitors[]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorPalette {
    pub name: String,
    pub geometry: [i64; 4],
    pub colors: Colors,
    /// Always `None` (legacy wallpaper textures are ignored).
    pub frost_texture: Option<String>,
}

/// A validated snapshot. `value` is the normalized dict Python returns
/// (unknown keys kept, defaults filled in, `frost_texture: null` per monitor).
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub value: Value,
    pub is_dark: bool,
    pub apps_transparency: i64,
    pub apps_transparency_enabled: bool,
    pub apps_frost_strength: i64,
    pub apps_frosted: bool,
    pub apps_rounded_corners: bool,
    pub monitors: Vec<MonitorPalette>,
}

/// Python `type(v) is int` (bools excluded, floats excluded).
fn py_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) if number.is_i64() => number.as_i64(),
        Value::Number(number) if number.is_u64() => Some(i64::MAX),
        _ => None,
    }
}

/// Python `type(v) in (int, float)`.
fn py_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

fn get_or<'a>(map: &'a Map<String, Value>, key: &str, default: &'a Value) -> &'a Value {
    map.get(key).unwrap_or(default)
}

/// `read_palette(path)`: an absent/malformed cache gives `None`.
pub fn read_palette(path: &Path) -> Option<Palette> {
    let mut raw = Vec::new();
    std::fs::File::open(path).ok()?.take(131073).read_to_end(&mut raw).ok()?;
    if raw.len() > 131072 {
        return None;
    }
    parse_palette(&raw)
}

/// Validation of `read_palette` on already-read bytes.
pub fn parse_palette(raw: &[u8]) -> Option<Palette> {
    let mut value: Value = serde_json::from_slice(raw).ok()?;
    let map = value.as_object_mut()?;
    // Python `value.get("version") != 1`: equality, so 1.0 and true pass.
    let version_ok = match map.get("version") {
        Some(Value::Bool(true)) => true,
        Some(Value::Number(number)) => number.as_f64() == Some(1.0),
        _ => false,
    };
    if !version_ok {
        return None;
    }
    let is_dark = map.get("is_dark")?.as_bool()?;
    let five = Value::from(5);
    let no = Value::Bool(false);
    let yes = Value::Bool(true);
    let transparency = get_or(map, "apps_transparency", &five).clone();
    let enabled = get_or(map, "apps_transparency_enabled", &no).clone();
    let enabled_flag = enabled.as_bool()?;
    let transparency_value = py_int(&transparency)?;
    if !(0..=100).contains(&transparency_value) {
        return None;
    }
    let strength_default = if map.contains_key("apps_transparency") { transparency.clone() } else { Value::from(50) };
    let strength = get_or(map, "apps_frost_strength", &strength_default).clone();
    let strength_value = py_int(&strength)?;
    if !(0..=100).contains(&strength_value) {
        return None;
    }
    map.insert("apps_frost_strength".into(), strength);
    map.insert("apps_transparency_enabled".into(), enabled);
    map.insert("apps_transparency".into(), transparency);
    let frosted = get_or(map, "apps_frosted", &no).clone();
    map.insert("apps_frosted".into(), frosted);
    let rounded = get_or(map, "apps_rounded_corners", &yes).clone();
    map.insert("apps_rounded_corners".into(), rounded);
    let apps_frosted = map["apps_frosted"].as_bool()?;
    let apps_rounded_corners = map["apps_rounded_corners"].as_bool()?;
    let monitors = map.get_mut("monitors")?.as_array_mut()?;
    if monitors.is_empty() || monitors.len() > 64 {
        return None;
    }
    let mut parsed = Vec::with_capacity(monitors.len());
    for monitor in monitors.iter_mut() {
        let monitor = monitor.as_object_mut()?;
        let name = monitor.get("name")?.as_str()?.to_owned();
        let texture = frost_texture_path(monitor.get("frost_texture"));
        monitor.insert("frost_texture".into(), texture.clone().map_or(Value::Null, Value::from));
        let geometry = monitor.get("geometry")?.as_array()?;
        if geometry.len() != 4 {
            return None;
        }
        let mut numbers = [0i64; 4];
        for (slot, number) in numbers.iter_mut().zip(geometry) {
            *slot = py_int(number)?;
        }
        if numbers[2] <= 0 || numbers[3] <= 0 {
            return None;
        }
        let mut rgbs = [[0f64; 3]; 3];
        for (slot, key) in rgbs.iter_mut().zip(["background", "foreground", "accent"]) {
            let rgb = monitor.get(key)?.as_array()?;
            if rgb.len() != 3 {
                return None;
            }
            for (channel, number) in slot.iter_mut().zip(rgb) {
                let number = py_number(number)?;
                if !(0.0..=255.0).contains(&number) {
                    return None;
                }
                *channel = number;
            }
        }
        parsed.push(MonitorPalette {
            name, geometry: numbers, frost_texture: texture,
            colors: Colors { background: rgbs[0], foreground: rgbs[1], accent: rgbs[2] },
        });
    }
    Some(Palette {
        value, is_dark, apps_transparency: transparency_value, apps_transparency_enabled: enabled_flag,
        apps_frost_strength: strength_value, apps_frosted, apps_rounded_corners, monitors: parsed,
    })
}

/// `monitor_palette(snapshot, name=None, geometry=None)`: connector first,
/// then geometry, else the first entry.
pub fn monitor_palette<'a>(snapshot: &'a Palette, name: Option<&str>, geometry: Option<[i64; 4]>) -> &'a MonitorPalette {
    let entries = &snapshot.monitors;
    name.and_then(|name| entries.iter().find(|entry| entry.name == name))
        .or_else(|| geometry.and_then(|geometry| entries.iter().find(|entry| entry.geometry == geometry)))
        .unwrap_or(&entries[0])
}
