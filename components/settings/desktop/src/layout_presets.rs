//! Desktop Layout presets (layout_presets.py): dock/panel, appearance,
//! desktop behavior, clock, workspaces and tiling.
//!
//! A preset is a JSON `.layout` file with an SVG icon of the same name. The
//! shipped presets are embedded (copied from tessel-settings/layouts);
//! [`load_presets_from`] reads a directory like Python's `load_presets(dir)`.
//! Wallpapers and pinned applications are never part of a preset.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};
use tessel_core::clock::{ClockPreferences, ClockPreferencesStore};
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::pyjson::{casefold, py_strip};
use tessel_dock::core::config::DockConfig;
use tessel_dock::dbus::settings_api::py_value_eq;

use crate::bridge::{settings_fields, PresetBridge};

pub const PREVIOUS_KEY: &str = "previous";
pub const MAX_PRESET_BYTES: u64 = 256 * 1024;

/// Names for the settings most people recognize; the rest are counted.
pub const FIELD_LABELS: &[(&str, &str)] = &[
    ("mode", "Dock or panel style"), ("edge", "Position on screen"), ("alignment", "Icon alignment"),
    ("icon_size", "Icon size"), ("separate_app_dock", "Separate app dock"), ("auto_hide", "Auto-hide"),
    ("float_mode", "Floating dock"), ("magnification", "Magnification"), ("bar_islands", "Split bar into islands"), ("group_layouts", "Panel item arrangement"),
    ("group_visibility", "Panel items shown"), ("transparency", "Dock transparency"), ("frosted", "Dock blur"),
    ("match_wallpaper", "Match wallpaper colors"), ("apps_transparency", "Application transparency"),
    ("apps_frosted", "Application blur"), ("apps_rounded_corners", "Rounded corners"),
    ("window_buttons_left", "Window buttons on the left"), ("colored_window_buttons", "Colored window buttons"),
    ("window_shadows", "Window shadows"), ("hot_corners", "Hot corners"),
];

/// The shipped presets (`layouts/*.layout` + `*.svg`), in file-name order.
pub const BUILTIN: &[(&str, &[u8], &[u8])] = &[
    ("gnome", include_bytes!("../layouts/gnome.layout"), include_bytes!("../layouts/gnome.svg")),
    ("gnome-ubuntu", include_bytes!("../layouts/gnome-ubuntu.layout"), include_bytes!("../layouts/gnome-ubuntu.svg")),
    ("kde", include_bytes!("../layouts/kde.layout"), include_bytes!("../layouts/kde.svg")),
    ("mac", include_bytes!("../layouts/mac.layout"), include_bytes!("../layouts/mac.svg")),
    ("tessel", include_bytes!("../layouts/tessel.layout"), include_bytes!("../layouts/tessel.svg")),
    ("win", include_bytes!("../layouts/win.layout"), include_bytes!("../layouts/win.svg")),
];

/// `Preset.icon`: a file next to the `.layout`, or an embedded SVG.
#[derive(Clone, Debug, PartialEq)]
pub enum PresetIcon {
    File(PathBuf),
    Embedded(&'static [u8]),
}

impl PresetIcon {
    /// `preset.icon.is_file()`.
    pub fn exists(&self) -> bool {
        match self {
            PresetIcon::File(path) => path.is_file(),
            PresetIcon::Embedded(_) => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub key: String,
    pub name: String,
    pub icon: PresetIcon,
    /// Field → value, in file order.
    pub dock: Map<String, Value>,
    pub workspaces: Option<i64>,
    pub gapless: Option<bool>,
    pub clock: Option<Map<String, Value>>,
}

impl Preset {
    pub fn new(key: &str, name: &str, icon: PresetIcon, dock: Map<String, Value>, workspaces: Option<i64>,
               gapless: Option<bool>) -> Self {
        Self { key: key.into(), name: name.into(), icon, dock, workspaces, gapless, clock: None }
    }
}

fn is_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

/// `{**asdict(base), **overrides}` → `DockConfig.from_dict`.
fn merged_config(base: &DockConfig, overrides: &Map<String, Value>) -> Result<DockConfig, String> {
    let Value::Object(mut values) = base.to_value() else { return Err("Invalid dock settings.".into()) };
    for (key, value) in overrides {
        values.insert(key.clone(), value.clone());
    }
    DockConfig::from_value(&Value::Object(values))
}

pub fn parse_preset(key: &str, data: &Value, icon: PresetIcon) -> Result<Preset, String> {
    let Some(object) = data.as_object().filter(|o| o.get("version").is_some_and(|v| py_value_eq(v, &Value::from(1)))) else {
        return Err(format!("Unsupported layout preset: {key}"));
    };
    let name = object.get("name");
    let empty = Value::Object(Map::new());
    let dock = object.get("dock").unwrap_or(&empty);
    let (Some(name), Some(dock)) = (name.and_then(Value::as_str).filter(|n| !py_strip(n).is_empty()), dock.as_object()) else {
        return Err(format!("Invalid layout preset: {key}"));
    };
    let allowed = settings_fields();
    if dock.keys().any(|k| !allowed.contains(k)) {
        return Err(format!("Layout preset {key} contains settings it cannot change."));
    }
    // Validate against the dock's own rules before offering the preset.
    merged_config(&DockConfig::default(), dock)?;
    let workspaces = match object.get("workspaces") {
        None | Some(Value::Null) => None,
        Some(value) if is_int(value) && value.as_i64().is_some_and(|n| n >= 1) => value.as_i64(),
        Some(_) => return Err(format!("Invalid workspace count in layout preset: {key}")),
    };
    let gapless = match object.get("gapless") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(flag)) => Some(*flag),
        Some(_) => return Err(format!("Invalid tiling setting in layout preset: {key}")),
    };
    let clock = match object.get("clock") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let fields = ClockPreferences::default().to_map();
            let invalid = || format!("Invalid clock settings in layout preset: {key}");
            let clock = value.as_object().ok_or_else(invalid)?;
            if clock.keys().any(|k| !fields.contains_key(k)) {
                return Err(invalid());
            }
            let mut wanted = fields.clone();
            wanted.extend(clock.clone());
            let parsed = ClockPreferences::from_map(&wanted).to_map();
            if !py_value_eq(&Value::Object(parsed), &Value::Object(wanted)) {
                return Err(invalid());
            }
            Some(clock.clone())
        }
    };
    Ok(Preset { key: key.into(), name: py_strip(name).to_owned(), icon, dock: dock.clone(), workspaces, gapless, clock })
}

fn decode(bytes: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn sort_presets(presets: &mut [Preset]) {
    presets.sort_by_key(|preset| casefold(&preset.name));
}

/// `load_presets()` for the embedded presets.
pub fn load_presets() -> (Vec<Preset>, Vec<String>) {
    let (mut presets, mut problems) = (Vec::new(), Vec::new());
    for (key, layout, icon) in BUILTIN {
        match decode(layout).and_then(|data| parse_preset(key, &data, PresetIcon::Embedded(icon))) {
            Ok(preset) => presets.push(preset),
            Err(error) => problems.push(error),
        }
    }
    sort_presets(&mut presets);
    (presets, problems)
}

/// `load_presets(directory)`: valid presets sorted by name, plus messages
/// for any that were skipped.
pub fn load_presets_from(directory: &Path) -> (Vec<Preset>, Vec<String>) {
    let (mut presets, mut problems) = (Vec::new(), Vec::new());
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory).map(|entries| entries.filter_map(|e| e.ok())
        .map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "layout")).collect()).unwrap_or_default();
    paths.sort();
    for path in paths {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let result = (|| {
            let size = std::fs::metadata(&path).map_err(|e| tessel_core::pyerr::os_error_path(&e, &path))?.len();
            if size > MAX_PRESET_BYTES {
                return Err(format!("Layout preset is too large: {stem}"));
            }
            let bytes = std::fs::read(&path).map_err(|e| tessel_core::pyerr::os_error_path(&e, &path))?;
            parse_preset(&stem, &decode(&bytes)?, PresetIcon::File(directory.join(format!("{stem}.svg"))))
        })();
        match result {
            Ok(preset) => presets.push(preset),
            Err(error) => problems.push(error),
        }
    }
    sort_presets(&mut presets);
    (presets, problems)
}

/// `capture(config, workspaces, gapless, name=…, clock=…)`.
pub fn capture(config: &DockConfig, workspaces: Option<i64>, gapless: Option<bool>, name: &str,
               clock: Option<&ClockPreferences>) -> Value {
    let values = config.to_value();
    let mut dock = Map::new();
    for key in settings_fields() {
        if let Some(value) = values.get(&key) {
            dock.insert(key, value.clone());
        }
    }
    let mut data = Map::new();
    data.insert("version".into(), 1.into());
    data.insert("name".into(), name.into());
    data.insert("dock".into(), Value::Object(dock));
    if let Some(count) = workspaces {
        data.insert("workspaces".into(), count.into());
    }
    if let Some(gapless) = gapless {
        data.insert("gapless".into(), gapless.into());
    }
    if let Some(clock) = clock {
        data.insert("clock".into(), Value::Object(clock.to_map()));
    }
    Value::Object(data)
}

pub fn desired_config(preset: &Preset, config: &DockConfig) -> Result<DockConfig, String> {
    merged_config(config, &preset.dock)
}

fn clock_value(clock: &ClockPreferences, key: &str) -> Value {
    clock.to_map().get(key).cloned().unwrap_or(Value::Null)
}

/// Whether the desktop already uses this preset's saved choices.
pub fn matches(preset: &Preset, config: &DockConfig, workspaces: Option<i64>, gapless: Option<bool>,
               clock: Option<&ClockPreferences>) -> bool {
    let current = config.to_value();
    let Ok(wanted) = desired_config(preset, config).map(|c| c.to_value()) else { return false };
    preset.dock.keys().all(|key| py_value_eq(current.get(key).unwrap_or(&Value::Null), wanted.get(key).unwrap_or(&Value::Null)))
        && (preset.workspaces.is_none() || workspaces.is_none() || preset.workspaces == workspaces)
        && (preset.gapless.is_none() || gapless.is_none() || preset.gapless == gapless)
        && match (&preset.clock, clock) {
            (Some(wanted), Some(clock)) => wanted.iter().all(|(key, value)| py_value_eq(&clock_value(clock, key), value)),
            _ => true,
        }
}

/// `WorkspaceConfig.load()` state (count + signature for the conflict check).
pub use gs_system::workspace_config::WorkspaceState;

pub trait WorkspaceBackend: Send + Sync {
    fn load(&self) -> Result<WorkspaceState, String>;
    fn save(&self, count: i64, expected: &WorkspaceState) -> Result<WorkspaceState, String>;
}

pub trait TilingBackend: Send + Sync {
    fn load_preferences(&self) -> Result<Map<String, Value>, String>;
    fn set_gapless(&self, enabled: bool) -> Result<(), String>;
}

impl WorkspaceBackend for gs_system::workspace_config::WorkspaceConfig {
    fn load(&self) -> Result<WorkspaceState, String> {
        gs_system::workspace_config::WorkspaceConfig::load(self).map_err(|e| e.message().to_owned())
    }
    fn save(&self, count: i64, expected: &WorkspaceState) -> Result<WorkspaceState, String> {
        gs_system::workspace_config::WorkspaceConfig::save(self, count, expected).map_err(|e| e.message().to_owned())
    }
}

impl TilingBackend for tessel_tiling::config::TilingConfig {
    fn load_preferences(&self) -> Result<Map<String, Value>, String> {
        tessel_tiling::config::TilingConfig::load_preferences(self).map_err(|e| e.0)
    }
    fn set_gapless(&self, enabled: bool) -> Result<(), String> {
        tessel_tiling::config::TilingConfig::set_gapless(self, enabled).map_err(|e| e.0)
    }
}

pub trait ClockBackend: Send + Sync {
    fn load(&self) -> Result<ClockPreferences, String>;
    fn update(&self, changes: &Map<String, Value>) -> Result<(), String>;
}

impl ClockBackend for Mutex<ClockPreferencesStore> {
    fn load(&self) -> Result<ClockPreferences, String> { Ok(self.lock().unwrap().load()) }
    fn update(&self, changes: &Map<String, Value>) -> Result<(), String> {
        let path = self.lock().unwrap().path.clone();
        self.lock().unwrap().update(changes).map(|_| ()).map_err(|error| match error {
            tessel_core::clock::StoreError::Io(error) => tessel_core::pyerr::os_error_path(&error, &path),
            other => other.to_string(),
        })
    }
}

/// Apply each part independently; return messages for parts that failed.
pub fn apply_preset(preset: &Preset, bridge: &dyn PresetBridge, workspace_backend: Option<&dyn WorkspaceBackend>,
                    tiling_backend: Option<&dyn TilingBackend>, clock_store: Option<&dyn ClockBackend>) -> Result<Vec<String>, String> {
    let mut errors = Vec::new();
    let desired = desired_config(preset, &bridge.config())?;
    if !bridge.save_config(&desired) {
        let error = bridge.error();
        errors.push(if error.is_empty() { "The dock settings could not be saved.".to_owned() } else { error });
    }
    if let (Some(count), Some(backend)) = (preset.workspaces, workspace_backend) {
        let result = backend.load().and_then(|state| if state.count != count { backend.save(count, &state).map(|_| ()) } else { Ok(()) });
        if let Err(error) = result {
            errors.push(error);
        }
    }
    if let (Some(gapless), Some(backend)) = (preset.gapless, tiling_backend) {
        let result = backend.load_preferences().and_then(|prefs| {
            if (prefs.get("gapless") == Some(&Value::Bool(true))) != gapless { backend.set_gapless(gapless) } else { Ok(()) }
        });
        if let Err(error) = result {
            errors.push(error);
        }
    }
    if let (Some(clock), Some(store)) = (&preset.clock, clock_store) {
        let result = store.load().and_then(|current| {
            let current = current.to_map();
            let changes: Map<String, Value> = clock.iter()
                .filter(|(key, value)| !current.get(*key).is_some_and(|c| py_value_eq(c, value)))
                .map(|(k, v)| (k.clone(), v.clone())).collect();
            if changes.is_empty() { Ok(()) } else { store.update(&changes) }
        });
        if let Err(error) = result {
            errors.push(error);
        }
    }
    Ok(errors)
}

/// `$XDG_STATE_HOME/tessel-settings/previous-layout.json`.
pub fn previous_path() -> PathBuf {
    tessel_core::paths::component_state_dir("settings").join("previous-layout.json")
}

/// The exact bytes `save_previous` writes (`json.dump(indent=2, sort_keys=True)` + newline).
pub fn previous_bytes(data: &Value) -> String {
    let style = JsonStyle { indent: Some(2), sort_keys: true, ..JsonStyle::DEFAULT };
    json_dumps(data, &style) + "\n"
}

/// Keep one snapshot of the desktop from before the last preset was applied.
pub fn save_previous(data: &Value, path: Option<&Path>) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let path = path.map(Path::to_path_buf).unwrap_or_else(previous_path);
    let parent = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    if !parent.is_dir() {
        if let Some(grand) = parent.parent() {
            std::fs::create_dir_all(grand)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(&parent) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
    }
    let (file, temporary) = make_temp(&parent, ".previous-")?;
    let result = (|| {
        use std::io::Write;
        let mut file = file;
        file.write_all(previous_bytes(data).as_bytes())?;
        drop(file);
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&temporary, &path)
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map(|_| path)
}

/// `tempfile.mkstemp(prefix=…, dir=…)`: O_EXCL, mode 0600.
fn make_temp(directory: &Path, prefix: &str) -> std::io::Result<(std::fs::File, PathBuf)> {
    use std::os::unix::fs::OpenOptionsExt;
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64
        ^ (std::process::id() as u64) << 32;
    for _ in 0..100 {
        let mut name = String::from(prefix);
        for _ in 0..8 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            name.push(LETTERS[(seed >> 33) as usize % LETTERS.len()] as char);
        }
        let candidate = directory.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
}

/// The saved snapshot as a preset, or `None` when there is none or it is unusable.
pub fn load_previous(path: Option<&Path>, name: &str) -> Option<Preset> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(previous_path);
    if std::fs::metadata(&path).ok()?.len() > MAX_PRESET_BYTES {
        return None;
    }
    let mut data = decode(&std::fs::read(&path).ok()?).ok()?;
    if let Value::Object(map) = &mut data {
        map.insert("name".into(), name.into());
    }
    let parent = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    parse_preset(PREVIOUS_KEY, &data, PresetIcon::File(parent.join(format!("{PREVIOUS_KEY}.svg")))).ok()
}

/// Translate with named values (Python `translate(text, **values)`).
pub type Translate<'a> = &'a dyn Fn(&str, &[(&str, String)]) -> String;

/// The untranslated default: `text.format(**values)`.
pub fn identity(text: &str, values: &[(&str, String)]) -> String {
    let mut result = text.to_owned();
    for (name, value) in values {
        result = result.replace(&format!("{{{name}}}"), value);
    }
    result
}

/// `tr` through suite_core's catalogs.
pub fn translated(text: &str, values: &[(&str, String)]) -> String {
    let args: Vec<(&str, tessel_core::pyfmt::Arg)> = values.iter().map(|(k, v)| (*k, tessel_core::pyfmt::Arg::from(v.clone()))).collect();
    tessel_core::i18n::translate_with(text, &args)
}

/// Plain-language list of what applying `preset` would change.
pub fn describe_changes(preset: &Preset, config: &DockConfig, workspaces: Option<i64>, gapless: Option<bool>,
                        clock: Option<&ClockPreferences>, translate: Translate) -> Result<Vec<String>, String> {
    let tr = translate;
    let current = config.to_value();
    let wanted = desired_config(preset, config)?.to_value();
    let mut keys: Vec<&String> = preset.dock.keys().collect();
    keys.sort();
    let changed: Vec<&String> = keys.into_iter()
        .filter(|key| !py_value_eq(current.get(*key).unwrap_or(&Value::Null), wanted.get(*key).unwrap_or(&Value::Null))).collect();
    let named: Vec<String> = changed.iter().filter_map(|key| FIELD_LABELS.iter().find(|(k, _)| k == key))
        .map(|(_, label)| tr(label, &[])).collect();
    let mut lines = Vec::new();
    if !named.is_empty() {
        lines.push(tr("Dock and appearance: {items}", &[("items", named.join(", "))]));
    }
    let others = changed.len() - named.len();
    if others > 0 {
        lines.push(tr("{count} other dock and panel settings", &[("count", others.to_string())]));
    }
    if let (Some(new), Some(old)) = (preset.workspaces, workspaces) {
        if new != old {
            lines.push(tr("Workspaces: {old} → {new}", &[("old", old.to_string()), ("new", new.to_string())]));
        }
    }
    if let (Some(new), Some(old)) = (preset.gapless, gapless) {
        if new != old {
            lines.push(tr(if new { "Gapless tiling: on" } else { "Gapless tiling: off" }, &[]));
        }
    }
    if let (Some(wanted), Some(clock)) = (&preset.clock, clock) {
        if wanted.iter().any(|(key, value)| !py_value_eq(&clock_value(clock, key), value)) {
            lines.push(tr("Clock format", &[]));
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar reaches the screen edges, so its own outline stays square, but
    /// the applications menu it opens (the menu hotkey) floats on its own and
    /// takes this setting.
    #[test]
    fn mac_bar_keeps_square_edges_and_opens_a_rounded_menu() {
        let (_, bytes, icon) = BUILTIN.iter().find(|(key, ..)| *key == "mac").unwrap();
        let preset = parse_preset("mac", &decode(bytes).unwrap(), PresetIcon::Embedded(icon)).unwrap();
        let config = merged_config(&DockConfig::default(), &preset.dock).unwrap();
        assert_eq!(config.mode, "bar");
        assert!(config.effective_rounded_corners(), "the menu opened from the bar is rounded");
        assert!(!config.effective_float_mode(), "a bar that doesn't float keeps square outer corners");
        assert!(config.apps_rounded_corners);
    }

    /// Stock GNOME (Fedora, Debian) has no dock on screen: the dash sits at the
    /// bottom and comes out when asked for. Ubuntu's keeps its full-height
    /// dock on the left.
    #[test]
    fn stock_gnome_hides_its_dash_at_the_bottom_and_ubuntu_keeps_the_side_dock() {
        let load = |key: &str| {
            let (_, bytes, icon) = BUILTIN.iter().find(|(k, ..)| *k == key).unwrap();
            let preset = parse_preset(key, &decode(bytes).unwrap(), PresetIcon::Embedded(icon)).unwrap();
            let config = merged_config(&DockConfig::default(), &preset.dock).unwrap();
            (preset, config)
        };
        let (gnome, config) = load("gnome");
        assert_eq!(gnome.name, "GNOME");
        assert!(config.uses_separate_app_dock());
        assert_eq!((config.app_dock_edge.as_str(), config.app_dock_auto_hide, config.app_dock_expand), ("bottom", true, false));
        assert_eq!(gnome.dock["group_visibility"]["focused_title"], Value::Bool(false));
        let (ubuntu, config) = load("gnome-ubuntu");
        assert_eq!(ubuntu.name, "GNOME (Ubuntu)");
        assert_eq!((config.app_dock_edge.as_str(), config.app_dock_auto_hide, config.app_dock_expand), ("left", false, true));
    }

    /// Tessel's own layout: one floating top bar in three islands that also
    /// holds the applications, with gaps between tiled windows. The other
    /// layouts turn the islands off again.
    #[test]
    fn tessel_is_one_top_bar_in_islands_and_the_others_join_it_back() {
        for (key, bytes, icon) in BUILTIN {
            let preset = parse_preset(key, &decode(bytes).unwrap(), PresetIcon::Embedded(icon)).unwrap();
            let config = merged_config(&DockConfig::default(), &preset.dock).unwrap();
            assert_eq!(config.uses_bar_islands(), *key == "tessel", "{key}");
            assert!(preset.dock.contains_key("bar_islands"), "{key} must say whether the bar is split");
            if *key != "tessel" {
                continue;
            }
            assert_eq!(preset.name, "Tessel");
            assert_eq!((config.mode.as_str(), config.edge.as_str()), ("bar", "top"));
            assert!(!config.uses_separate_app_dock() && config.effective_float_mode() && config.effective_rounded_corners());
            assert_eq!(preset.gapless, Some(false));
            let layout = &preset.dock["group_layouts"]["bar"];
            assert_eq!(layout["start"], serde_json::json!(["app_menu", "workspaces", "pinned", "running", "files"]));
            assert_eq!(layout["center"], serde_json::json!(["clock", "weather"]));
            assert!(config.always_visible_apps.iter().any(|name| name == "tiling"));
        }
    }
}
