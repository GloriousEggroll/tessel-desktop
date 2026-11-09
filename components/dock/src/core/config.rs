//! Persistent dock preferences and the read-only Cairo launcher migration
//! (port of dock_config.py; contract §8 "DockConfig schema").
//!
//! API:
//! - [`DockConfig`]: every Python field with the Python name, type and default
//!   (`Default`), `Clone + PartialEq + Debug`. Dict fields are [`OrderedMap`]s
//!   (insertion order = Python dict order; equality ignores order).
//! - [`DockConfig::from_value`] = `DockConfig.from_dict` (same validation
//!   order and error texts), [`DockConfig::to_value`] = `asdict` (dataclass
//!   field order), [`DockConfig::to_saved_value`] = `{"version": 1, **asdict}`.
//! - [`field_names`] (dataclass order), `effective_*`, [`DockConfig::is_group_visible`],
//!   [`DockConfig::uses_separate_app_dock`], [`DockConfig::for_app_dock`], [`DockConfig::for_monitor`].
//! - [`ConfigStore`]: `new(path)`, `default()` / [`default_path`]
//!   (`$XDG_CONFIG_HOME/tessel-dock/config.json`), `load(catalog, cairo_dir)` (never
//!   writes; sets `error` on a bad file and returns defaults), `save(&config)`
//!   (validates, atomic `.dock-settings-*` + fsync + replace, indent 2,
//!   `ensure_ascii=False`, trailing newline), [`save_bytes`] for the exact bytes.
//! - [`cairo_pins`], [`initial_pins`], [`PinCatalog`] (implemented by `AppCatalog`).
//! - Constants [`GROUPS`], [`CORNERS`], [`DEFAULT_APPS`], [`ALWAYS_VISIBLE_APPS`], [`BORDER_SIDES`].

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

use super::group_layout::{self, Layout, Layouts};
use super::ordered::OrderedMap;

pub const GROUPS: &[(&str, &str)] = &[
    ("app_menu", "Application menu"),
    ("pinned", "Pinned applications"),
    ("running", "Open windows"),
    ("files", "Home and Trash"),
    ("workspaces", "Workspaces"),
    ("media", "Media"),
    ("system_tray", "System tray"),
    ("service_apps", "Applets"),
    ("weather", "Weather"),
    ("clock", "Time and date"),
    ("session", "Logout and Show Desktop"),
    ("activities", "Activities"),
    ("focused_title", "Focused window title"),
];

pub const CORNERS: &[(&str, &str)] =
    &[("top_left", "Top left"), ("top_right", "Top right"), ("bottom_left", "Bottom left"), ("bottom_right", "Bottom right")];

pub const DEFAULT_APPS: &[(&str, &str)] = &[
    ("idle_inhibitor", "Keep Awake"),
    ("screen_record", "Screen Record"),
    ("notifications", "Notifications"),
    ("power", "Power"),
    ("battery", "Battery"),
    ("capslock", "Caps Lock"),
    ("bluetooth", "Bluetooth"),
    ("tiling", "Window Tiling"),
    ("keyboard", "On-screen Keyboard"),
    ("network", "Network"),
    ("discovery", "Discover Network Devices"),
    ("audio", "Sound"),
    ("brightness", "Displays"),
    ("showdesktop", "Minimize / restore windows"),
];
pub const ALWAYS_VISIBLE_APPS: &[&str] = &["idle_inhibitor", "capslock", "audio", "brightness", "showdesktop"];
pub const BORDER_SIDES: &[&str] = &["top", "bottom", "left", "right"];
const EDGES: &[&str] = &["top", "bottom", "left", "right"];
const ALIGNMENTS: &[&str] = &["start", "center", "end"];

/// Remembers that `hover_scale` was a JSON integer (Python keeps `1`, not
/// `1.0`, when it re-saves). Always compares equal, like `1 == 1.0`.
#[derive(Clone, Copy, Debug, Default)]
pub struct IntHint(pub bool);

impl PartialEq for IntHint {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DockConfig {
    pub pins: Vec<String>,
    pub mode: String,
    pub separate_app_dock: bool,
    pub app_dock_edge: String,
    pub app_dock_expand: bool,
    pub app_dock_icon_size: i64,
    pub app_dock_gap: i64,
    pub app_dock_float_mode: bool,
    pub app_dock_rounded_corners: bool,
    pub app_dock_transparency: i64,
    pub app_dock_transparency_enabled: bool,
    pub app_dock_frosted: bool,
    pub app_dock_frost_strength: i64,
    pub app_dock_match_wallpaper: bool,
    pub app_dock_background_color: String,
    pub app_dock_auto_hide: bool,
    pub app_dock_shine: bool,
    pub app_dock_shadow: bool,
    /// `{}` (automatic expanded/compact defaults) or all three positions.
    pub app_dock_group_layout: Layout,
    pub application_display: String,
    pub always_show_tray: Vec<String>,
    pub always_visible_apps: Vec<String>,
    pub group_layouts: Layouts,
    pub edge: String,
    pub alignment: String,
    pub monitor: String,
    /// monitor name → `{"edge"?: …, "alignment"?: …}`.
    pub monitor_overrides: OrderedMap<OrderedMap<String>>,
    pub icon_size: i64,
    pub gap: i64,
    pub hover_scale: f64,
    #[doc(hidden)]
    pub hover_scale_int: IntHint,
    pub magnification: bool,
    pub compact_bar: bool,
    pub transparency: i64,
    pub transparency_enabled: bool,
    pub frosted: bool,
    pub frost_strength: i64,
    pub apps_transparency: i64,
    pub apps_transparency_enabled: bool,
    pub apps_frosted: bool,
    pub apps_frost_strength: i64,
    pub apps_rounded_corners: bool,
    pub window_buttons_left: bool,
    pub colored_window_buttons: bool,
    pub window_shadows: bool,
    pub match_wallpaper: bool,
    pub background_color: Option<String>,
    pub rounded_corners: bool,
    pub float_mode: bool,
    pub bar_rounded_corners: bool,
    pub bar_float_mode: bool,
    pub bar_border_enabled: bool,
    pub bar_border_sides: Vec<String>,
    pub bar_border_color: String,
    /// Legacy key name; now means "keep behind windows" for all overlap.
    pub hide_on_fullscreen: bool,
    pub menu_hotkey_fullscreen: bool,
    pub group_visibility: OrderedMap<bool>,
    pub hot_corners: OrderedMap<Vec<String>>,
    pub hot_corner_delay_ms: i64,
    pub auto_hide: bool,
    pub reveal_delay_ms: i64,
    pub hide_delay_ms: i64,
}

const FIELD_NAMES: &[&str] = &[
    "pins", "mode", "separate_app_dock", "app_dock_edge", "app_dock_expand", "app_dock_icon_size", "app_dock_gap",
    "app_dock_float_mode", "app_dock_rounded_corners", "app_dock_transparency", "app_dock_transparency_enabled",
    "app_dock_frosted", "app_dock_frost_strength", "app_dock_match_wallpaper", "app_dock_background_color",
    "app_dock_auto_hide", "app_dock_shine", "app_dock_shadow", "app_dock_group_layout", "application_display", "always_show_tray",
    "always_visible_apps", "group_layouts", "edge", "alignment", "monitor", "monitor_overrides", "icon_size", "gap",
    "hover_scale", "magnification", "compact_bar", "transparency", "transparency_enabled", "frosted", "frost_strength",
    "apps_transparency", "apps_transparency_enabled", "apps_frosted", "apps_frost_strength", "apps_rounded_corners",
    "window_buttons_left", "colored_window_buttons", "window_shadows", "match_wallpaper", "background_color",
    "rounded_corners", "float_mode", "bar_rounded_corners", "bar_float_mode", "bar_border_enabled", "bar_border_sides",
    "bar_border_color", "hide_on_fullscreen", "menu_hotkey_fullscreen", "group_visibility", "hot_corners",
    "hot_corner_delay_ms", "auto_hide", "reveal_delay_ms", "hide_delay_ms",
];

/// `DockConfig.__dataclass_fields__` in declaration order.
pub fn field_names() -> &'static [&'static str] {
    FIELD_NAMES
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            pins: Vec::new(),
            mode: "dock".into(),
            separate_app_dock: false,
            app_dock_edge: "left".into(),
            app_dock_expand: true,
            app_dock_icon_size: 40,
            app_dock_gap: 8,
            app_dock_float_mode: false,
            app_dock_rounded_corners: false,
            app_dock_transparency: 5,
            app_dock_transparency_enabled: true,
            app_dock_frosted: false,
            app_dock_frost_strength: 50,
            app_dock_match_wallpaper: false,
            app_dock_background_color: "#38373a".into(),
            app_dock_auto_hide: false,
            app_dock_shine: false,
            app_dock_shadow: false,
            app_dock_group_layout: Layout::new(),
            application_display: "icons".into(),
            always_show_tray: Vec::new(),
            always_visible_apps: strings(ALWAYS_VISIBLE_APPS),
            group_layouts: Layouts::new(),
            edge: "bottom".into(),
            alignment: "center".into(),
            monitor: "all".into(),
            monitor_overrides: OrderedMap::new(),
            icon_size: 40,
            gap: 8,
            hover_scale: 1.0,
            hover_scale_int: IntHint(false),
            magnification: false,
            compact_bar: false,
            transparency: 5,
            transparency_enabled: true,
            frosted: false,
            frost_strength: 50,
            apps_transparency: 5,
            apps_transparency_enabled: false,
            apps_frosted: false,
            apps_frost_strength: 50,
            apps_rounded_corners: true,
            window_buttons_left: false,
            colored_window_buttons: false,
            window_shadows: true,
            match_wallpaper: false,
            background_color: None,
            rounded_corners: true,
            float_mode: true,
            bar_rounded_corners: false,
            bar_float_mode: false,
            bar_border_enabled: false,
            bar_border_sides: strings(BORDER_SIDES),
            bar_border_color: "#777777".into(),
            hide_on_fullscreen: true,
            menu_hotkey_fullscreen: false,
            group_visibility: GROUPS.iter().map(|(name, _)| (*name, true)).collect(),
            hot_corners: OrderedMap::new(),
            hot_corner_delay_ms: 150,
            auto_hide: false,
            reveal_delay_ms: 120,
            hide_delay_ms: 500,
        }
    }
}

fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

fn int_in(value: &Value, minimum: i64, maximum: i64) -> bool {
    value.as_i64().is_some_and(|n| (minimum..=maximum).contains(&n))
}

fn str_in(value: &Value, options: &[&str]) -> bool {
    value.as_str().is_some_and(|text| options.contains(&text))
}

fn has_control(text: &str) -> bool {
    text.chars().any(|c| (c as u32) < 32)
}

/// Python `version != 1` (where `True == 1 == 1.0`).
fn is_version_one(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() == Some(1.0),
        _ => false,
    }
}

pub fn valid_desktop_id(value: &str) -> bool {
    let length = value.chars().count();
    value.ends_with(".desktop")
        && length <= 512
        && length > ".desktop".len()
        && !value.contains('/')
        && !value.contains('\\')
        && !has_control(value)
}

pub fn valid_monitor_name(value: &str) -> bool {
    !tessel_core::pyjson::py_strip(value).is_empty() && value.chars().count() <= 256 && !has_control(value)
}

fn valid_monitor_value(value: &Value) -> bool {
    value.as_str().is_some_and(valid_monitor_name)
}

/// `list(dict.fromkeys(values))`.
fn dedupe(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values {
        if !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

fn string_list(value: &Value) -> Vec<String> {
    value.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

impl DockConfig {
    pub fn effective_transparency(&self) -> i64 {
        if self.transparency_enabled { self.transparency } else { 0 }
    }
    pub fn effective_frosted(&self) -> bool {
        self.frosted
    }
    pub fn effective_apps_transparency(&self) -> i64 {
        if self.apps_transparency_enabled { self.apps_transparency } else { 0 }
    }
    pub fn effective_apps_frosted(&self) -> bool {
        self.apps_frosted
    }
    pub fn effective_rounded_corners(&self) -> bool {
        if self.mode == "bar" { self.bar_rounded_corners } else { self.rounded_corners }
    }
    pub fn effective_float_mode(&self) -> bool {
        if self.mode == "bar" { self.bar_float_mode } else { self.float_mode }
    }
    pub fn is_group_visible(&self, name: &str) -> bool {
        self.group_visibility.get(name).copied().unwrap_or(true)
    }
    pub fn uses_separate_app_dock(&self) -> bool {
        self.separate_app_dock && self.mode == "bar" && self.edge == "top"
    }

    /// Derive the separate app dock's surface config without changing the bar.
    pub fn for_app_dock(&self) -> DockConfig {
        DockConfig {
            mode: "dock".into(),
            edge: self.app_dock_edge.clone(),
            alignment: "center".into(),
            application_display: "icons".into(),
            icon_size: self.app_dock_icon_size,
            gap: self.app_dock_gap,
            float_mode: self.app_dock_float_mode,
            rounded_corners: self.app_dock_rounded_corners,
            transparency: self.app_dock_transparency,
            transparency_enabled: self.app_dock_transparency_enabled,
            frosted: self.app_dock_frosted,
            frost_strength: self.app_dock_frost_strength,
            match_wallpaper: self.app_dock_match_wallpaper,
            background_color: Some(self.app_dock_background_color.clone()),
            auto_hide: self.app_dock_auto_hide,
            ..self.clone()
        }
    }

    /// Effective placement for one monitor; missing overrides inherit the
    /// global edge/alignment. The copy shares no mutable state.
    pub fn for_monitor(&self, name: &str) -> DockConfig {
        let mut config = self.clone();
        if let Some(override_) = self.monitor_overrides.get(name) {
            if let Some(edge) = override_.get("edge") {
                config.edge = edge.clone();
            }
            if let Some(alignment) = override_.get("alignment") {
                config.alignment = alignment.clone();
            }
        }
        config
    }

    /// `asdict(config)`: every field in dataclass order.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        let mut put = |key: &str, value: Value| {
            map.insert(key.to_owned(), value);
        };
        put("pins", Value::from(self.pins.clone()));
        put("mode", Value::from(self.mode.clone()));
        put("separate_app_dock", Value::from(self.separate_app_dock));
        put("app_dock_edge", Value::from(self.app_dock_edge.clone()));
        put("app_dock_expand", Value::from(self.app_dock_expand));
        put("app_dock_icon_size", Value::from(self.app_dock_icon_size));
        put("app_dock_gap", Value::from(self.app_dock_gap));
        put("app_dock_float_mode", Value::from(self.app_dock_float_mode));
        put("app_dock_rounded_corners", Value::from(self.app_dock_rounded_corners));
        put("app_dock_transparency", Value::from(self.app_dock_transparency));
        put("app_dock_transparency_enabled", Value::from(self.app_dock_transparency_enabled));
        put("app_dock_frosted", Value::from(self.app_dock_frosted));
        put("app_dock_frost_strength", Value::from(self.app_dock_frost_strength));
        put("app_dock_match_wallpaper", Value::from(self.app_dock_match_wallpaper));
        put("app_dock_background_color", Value::from(self.app_dock_background_color.clone()));
        put("app_dock_auto_hide", Value::from(self.app_dock_auto_hide));
        put("app_dock_shine", Value::from(self.app_dock_shine));
        put("app_dock_shadow", Value::from(self.app_dock_shadow));
        put("app_dock_group_layout", group_layout::layout_value(&self.app_dock_group_layout));
        put("application_display", Value::from(self.application_display.clone()));
        put("always_show_tray", Value::from(self.always_show_tray.clone()));
        put("always_visible_apps", Value::from(self.always_visible_apps.clone()));
        put("group_layouts", group_layout::layouts_value(&self.group_layouts));
        put("edge", Value::from(self.edge.clone()));
        put("alignment", Value::from(self.alignment.clone()));
        put("monitor", Value::from(self.monitor.clone()));
        put(
            "monitor_overrides",
            Value::Object(
                self.monitor_overrides
                    .iter()
                    .map(|(name, o)| {
                        (name.to_owned(), Value::Object(o.iter().map(|(k, v)| (k.to_owned(), Value::from(v.clone()))).collect()))
                    })
                    .collect(),
            ),
        );
        put("icon_size", Value::from(self.icon_size));
        put("gap", Value::from(self.gap));
        let integral = self.hover_scale_int.0 && self.hover_scale.fract() == 0.0 && self.hover_scale.abs() < 1e15;
        put(
            "hover_scale",
            if integral {
                Value::from(self.hover_scale as i64)
            } else {
                serde_json::Number::from_f64(self.hover_scale).map(Value::Number).unwrap_or(Value::Null)
            },
        );
        put("magnification", Value::from(self.magnification));
        put("compact_bar", Value::from(self.compact_bar));
        put("transparency", Value::from(self.transparency));
        put("transparency_enabled", Value::from(self.transparency_enabled));
        put("frosted", Value::from(self.frosted));
        put("frost_strength", Value::from(self.frost_strength));
        put("apps_transparency", Value::from(self.apps_transparency));
        put("apps_transparency_enabled", Value::from(self.apps_transparency_enabled));
        put("apps_frosted", Value::from(self.apps_frosted));
        put("apps_frost_strength", Value::from(self.apps_frost_strength));
        put("apps_rounded_corners", Value::from(self.apps_rounded_corners));
        put("window_buttons_left", Value::from(self.window_buttons_left));
        put("colored_window_buttons", Value::from(self.colored_window_buttons));
        put("window_shadows", Value::from(self.window_shadows));
        put("match_wallpaper", Value::from(self.match_wallpaper));
        put("background_color", self.background_color.clone().map(Value::from).unwrap_or(Value::Null));
        put("rounded_corners", Value::from(self.rounded_corners));
        put("float_mode", Value::from(self.float_mode));
        put("bar_rounded_corners", Value::from(self.bar_rounded_corners));
        put("bar_float_mode", Value::from(self.bar_float_mode));
        put("bar_border_enabled", Value::from(self.bar_border_enabled));
        put("bar_border_sides", Value::from(self.bar_border_sides.clone()));
        put("bar_border_color", Value::from(self.bar_border_color.clone()));
        put("hide_on_fullscreen", Value::from(self.hide_on_fullscreen));
        put("menu_hotkey_fullscreen", Value::from(self.menu_hotkey_fullscreen));
        put(
            "group_visibility",
            Value::Object(self.group_visibility.iter().map(|(k, v)| (k.to_owned(), Value::from(*v))).collect()),
        );
        put(
            "hot_corners",
            Value::Object(self.hot_corners.iter().map(|(k, v)| (k.to_owned(), Value::from(v.clone()))).collect()),
        );
        put("hot_corner_delay_ms", Value::from(self.hot_corner_delay_ms));
        put("auto_hide", Value::from(self.auto_hide));
        put("reveal_delay_ms", Value::from(self.reveal_delay_ms));
        put("hide_delay_ms", Value::from(self.hide_delay_ms));
        Value::Object(map)
    }

    /// The saved payload: `{"version": 1, **asdict(config)}`.
    pub fn to_saved_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("version".into(), Value::from(1));
        if let Value::Object(fields) = self.to_value() {
            map.extend(fields);
        }
        Value::Object(map)
    }

    /// `DockConfig.from_dict(value)`: same checks, order and messages.
    pub fn from_value(value: &Value) -> Result<DockConfig, String> {
        let Some(input) = value.as_object() else {
            return Err("Dock settings must be an object.".into());
        };
        if let Some(version) = input.get("version") {
            if !is_version_one(version) {
                return Err("This dock settings version is not supported.".into());
            }
        }
        let mut known: Map<String, Value> =
            FIELD_NAMES.iter().filter_map(|k| input.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        for prefix in ["", "apps_"] {
            let strength = format!("{prefix}frost_strength");
            if !known.contains_key(&strength) && known.get(&format!("{prefix}frosted")) == Some(&Value::Bool(true)) {
                let copied = known.get(&format!("{prefix}transparency")).cloned().unwrap_or(Value::from(20));
                known.insert(strength, copied);
            }
        }
        if let Some(Value::Array(apps)) = known.get_mut("always_visible_apps") {
            apps.retain(|name| name.as_str() != Some("media"));
        }
        // `cls(**known)`: defaults for missing fields, values untyped until checked.
        let mut c = match DockConfig::default().to_value() {
            Value::Object(map) => map,
            _ => unreachable!(),
        };
        for (key, value) in known {
            c.insert(key, value);
        }

        for name in ["background_color", "app_dock_background_color", "bar_border_color"] {
            let color = &c[name];
            if color.is_null() && name == "background_color" {
                continue;
            }
            let valid = color
                .as_str()
                .is_some_and(|s| s.chars().count() == 7 && s.starts_with('#') && s[1..].chars().all(|ch| ch.is_ascii_hexdigit()));
            if !valid {
                return Err("Choose a color in #RRGGBB format.".into());
            }
            let lowered = color.as_str().unwrap().to_lowercase();
            c.insert(name.into(), Value::from(lowered));
        }
        if !c["bar_border_enabled"].is_boolean() {
            return Err("Invalid bar border setting.".into());
        }
        let sides_valid = c["bar_border_sides"].as_array().is_some_and(|sides| sides.iter().all(|s| str_in(s, BORDER_SIDES)));
        if !sides_valid {
            return Err("Choose valid bar border sides: top, bottom, left or right.".into());
        }
        let sides = string_list(&c["bar_border_sides"]);
        let bar_border_sides: Vec<String> = BORDER_SIDES.iter().filter(|s| sides.iter().any(|x| x == *s)).map(|s| s.to_string()).collect();
        if !str_in(&c["mode"], &["dock", "bar"]) {
            return Err("Choose Dock or Bar mode.".into());
        }
        if !str_in(&c["app_dock_edge"], &["left", "right", "bottom"]) {
            return Err("Choose Left, Right or Bottom for the separate app dock.".into());
        }
        for name in [
            "separate_app_dock", "app_dock_expand", "app_dock_float_mode", "app_dock_rounded_corners",
            "app_dock_transparency_enabled", "app_dock_frosted", "app_dock_match_wallpaper", "app_dock_auto_hide",
            "app_dock_shine", "app_dock_shadow",
        ] {
            if !c[name].is_boolean() {
                return Err(format!("Invalid separate app dock setting: {name}."));
            }
        }
        if !str_in(&c["application_display"], &["icons", "title"]) {
            return Err("Choose application icons or the focused window title.".into());
        }
        let application_display = text(&c["application_display"]);
        let group_layouts = group_layout::validate_layouts(&c["group_layouts"], &application_display)?;
        let app_dock_expand = c["app_dock_expand"].as_bool().unwrap_or(true);
        let app_dock_group_layout = group_layout::validate_app_dock_layout(&c["app_dock_group_layout"], app_dock_expand)?;
        if !str_in(&c["edge"], EDGES) {
            return Err("Choose a screen edge for the dock.".into());
        }
        if !str_in(&c["alignment"], ALIGNMENTS) {
            return Err("Choose a valid dock alignment.".into());
        }
        if !valid_monitor_value(&c["monitor"]) {
            return Err("Choose a valid monitor.".into());
        }
        let Some(overrides) = c["monitor_overrides"].as_object() else {
            return Err("Monitor placement overrides must be an object.".into());
        };
        let mut monitor_overrides = OrderedMap::new();
        for (name, override_) in overrides {
            let Some(override_) = override_.as_object().filter(|_| valid_monitor_name(name)) else {
                return Err("Invalid monitor placement override.".into());
            };
            if override_.keys().any(|k| k != "edge" && k != "alignment") {
                return Err("Monitor overrides support only edge and alignment.".into());
            }
            if override_.get("edge").is_some_and(|edge| !str_in(edge, EDGES)) {
                return Err("Choose a valid screen edge for the monitor.".into());
            }
            if override_.get("alignment").is_some_and(|a| !str_in(a, ALIGNMENTS)) {
                return Err("Choose a valid alignment for the monitor.".into());
            }
            monitor_overrides.insert(name.clone(), override_.iter().map(|(k, v)| (k.clone(), text(v))).collect::<OrderedMap<String>>());
        }
        let Some(corners_map) = c["hot_corners"].as_object() else {
            return Err("Hot corners must map monitor names to enabled corners.".into());
        };
        let mut hot_corners = OrderedMap::new();
        for (monitor, corners) in corners_map {
            let Some(corners) = corners.as_array().filter(|_| valid_monitor_name(monitor)) else {
                return Err("Invalid monitor hot corner setting.".into());
            };
            if corners.iter().any(|corner| !corner.as_str().is_some_and(|s| CORNERS.iter().any(|(n, _)| *n == s))) {
                return Err("Choose a valid monitor corner.".into());
            }
            hot_corners.insert(monitor.clone(), dedupe(corners.iter().map(text)));
        }
        for (name, minimum, maximum) in [
            ("icon_size", 16, 128),
            ("gap", 0, 48),
            ("transparency", 0, 100),
            ("app_dock_icon_size", 16, 128),
            ("app_dock_gap", 0, 48),
            ("app_dock_transparency", 0, 100),
            ("app_dock_frost_strength", 0, 100),
            ("apps_transparency", 0, 100),
            ("frost_strength", 0, 100),
            ("apps_frost_strength", 0, 100),
            ("hot_corner_delay_ms", 100, 2000),
            ("reveal_delay_ms", 0, 5000),
            ("hide_delay_ms", 0, 5000),
        ] {
            if !is_int(&c[name]) || !int_in(&c[name], minimum, maximum) {
                return Err(format!("Invalid dock setting: {name}."));
            }
        }
        let hover = &c["hover_scale"];
        let hover_scale = hover.as_f64().filter(|h| h.is_finite() && (1.0..=2.0).contains(h));
        let Some(hover_scale) = hover_scale else {
            return Err("Invalid dock icon hover scale.".into());
        };
        let hover_scale_int = IntHint(is_int(hover));
        if !c["hide_on_fullscreen"].is_boolean() {
            return Err("Invalid window stacking setting.".into());
        }
        if !c["menu_hotkey_fullscreen"].is_boolean() {
            return Err("Invalid fullscreen application menu hotkey setting.".into());
        }
        let Some(visibility) = c["group_visibility"].as_object() else {
            return Err("Dock group visibility must be an object.".into());
        };
        if visibility.keys().any(|k| !GROUPS.iter().any(|(g, _)| g == k)) {
            return Err("Unknown dock group visibility setting.".into());
        }
        if visibility.values().any(|v| !v.is_boolean()) {
            return Err("Dock group visibility values must be true or false.".into());
        }
        let group_visibility: OrderedMap<bool> =
            GROUPS.iter().map(|(name, _)| (*name, visibility.get(*name).and_then(Value::as_bool).unwrap_or(true))).collect();
        if !c["auto_hide"].is_boolean() {
            return Err("Invalid automatic hiding setting.".into());
        }
        if !c["match_wallpaper"].is_boolean() {
            return Err("Invalid wallpaper color matching setting.".into());
        }
        for name in ["transparency_enabled", "frosted", "apps_transparency_enabled", "apps_frosted", "apps_rounded_corners"] {
            if !c[name].is_boolean() {
                return Err(format!("Invalid transparency setting: {name}."));
            }
        }
        for name in ["magnification", "compact_bar", "window_buttons_left", "colored_window_buttons", "window_shadows"] {
            if !c[name].is_boolean() {
                return Err(format!("Invalid appearance setting: {name}."));
            }
        }
        if !c["rounded_corners"].is_boolean() {
            return Err("Invalid rounded corners setting.".into());
        }
        if !c["float_mode"].is_boolean() {
            return Err("Invalid float mode setting.".into());
        }
        if !c["bar_rounded_corners"].is_boolean() {
            return Err("Invalid Bar rounded corners setting.".into());
        }
        if !c["bar_float_mode"].is_boolean() {
            return Err("Invalid Bar float mode setting.".into());
        }
        let pins_valid = c["pins"].as_array().is_some_and(|pins| pins.iter().all(|p| p.as_str().is_some_and(valid_desktop_id)));
        if !pins_valid {
            return Err("Pinned applications must use desktop entry IDs.".into());
        }
        let tray_valid = c["always_show_tray"].as_array().is_some_and(|items| {
            items.len() <= 512
                && items.iter().all(|ident| {
                    ident.as_str().is_some_and(|s| {
                        !tessel_core::pyjson::py_strip(s).is_empty() && s.chars().count() <= 512 && !has_control(s)
                    })
                })
        });
        if !tray_valid {
            return Err("Always show tray applications must use valid application identifiers.".into());
        }
        let apps_valid = c["always_visible_apps"]
            .as_array()
            .is_some_and(|apps| apps.iter().all(|name| name.as_str().is_some_and(|s| DEFAULT_APPS.iter().any(|(n, _)| *n == s))));
        if !apps_valid {
            return Err("Always visible apps must use supported applet names.".into());
        }
        let flag = |name: &str| c[name].as_bool().unwrap_or_default();
        let int = |name: &str| c[name].as_i64().unwrap_or_default();
        Ok(DockConfig {
            pins: dedupe(string_list(&c["pins"])),
            mode: text(&c["mode"]),
            separate_app_dock: flag("separate_app_dock"),
            app_dock_edge: text(&c["app_dock_edge"]),
            app_dock_expand,
            app_dock_icon_size: int("app_dock_icon_size"),
            app_dock_gap: int("app_dock_gap"),
            app_dock_float_mode: flag("app_dock_float_mode"),
            app_dock_rounded_corners: flag("app_dock_rounded_corners"),
            app_dock_transparency: int("app_dock_transparency"),
            app_dock_transparency_enabled: flag("app_dock_transparency_enabled"),
            app_dock_frosted: flag("app_dock_frosted"),
            app_dock_frost_strength: int("app_dock_frost_strength"),
            app_dock_match_wallpaper: flag("app_dock_match_wallpaper"),
            app_dock_background_color: text(&c["app_dock_background_color"]),
            app_dock_auto_hide: flag("app_dock_auto_hide"),
            app_dock_shine: flag("app_dock_shine"),
            app_dock_shadow: flag("app_dock_shadow"),
            app_dock_group_layout,
            application_display,
            always_show_tray: dedupe(string_list(&c["always_show_tray"])),
            always_visible_apps: dedupe(string_list(&c["always_visible_apps"])),
            group_layouts,
            edge: text(&c["edge"]),
            alignment: text(&c["alignment"]),
            monitor: text(&c["monitor"]),
            monitor_overrides,
            icon_size: int("icon_size"),
            gap: int("gap"),
            hover_scale,
            hover_scale_int,
            magnification: flag("magnification"),
            compact_bar: flag("compact_bar"),
            transparency: int("transparency"),
            transparency_enabled: flag("transparency_enabled"),
            frosted: flag("frosted"),
            frost_strength: int("frost_strength"),
            apps_transparency: int("apps_transparency"),
            apps_transparency_enabled: flag("apps_transparency_enabled"),
            apps_frosted: flag("apps_frosted"),
            apps_frost_strength: int("apps_frost_strength"),
            apps_rounded_corners: flag("apps_rounded_corners"),
            window_buttons_left: flag("window_buttons_left"),
            colored_window_buttons: flag("colored_window_buttons"),
            window_shadows: flag("window_shadows"),
            match_wallpaper: flag("match_wallpaper"),
            background_color: c["background_color"].as_str().map(str::to_owned),
            rounded_corners: flag("rounded_corners"),
            float_mode: flag("float_mode"),
            bar_rounded_corners: flag("bar_rounded_corners"),
            bar_float_mode: flag("bar_float_mode"),
            bar_border_enabled: flag("bar_border_enabled"),
            bar_border_sides,
            bar_border_color: text(&c["bar_border_color"]),
            hide_on_fullscreen: flag("hide_on_fullscreen"),
            menu_hotkey_fullscreen: flag("menu_hotkey_fullscreen"),
            group_visibility,
            hot_corners,
            hot_corner_delay_ms: int("hot_corner_delay_ms"),
            auto_hide: flag("auto_hide"),
            reveal_delay_ms: int("reveal_delay_ms"),
            hide_delay_ms: int("hide_delay_ms"),
        })
    }
}

/// What first-run pin import needs from a catalog (`AppCatalog` implements it;
/// test fakes may omit `resolve`/`default_browser` like the Python fakes).
pub trait PinCatalog {
    /// `catalog.get(desktop_id).id`.
    fn get_id(&self, desktop_id: &str) -> Option<String>;
    /// `catalog.resolve(value).id` (`hasattr(catalog, "resolve")`).
    fn resolve_id(&self, _value: &str) -> Option<String> {
        None
    }
    /// `catalog.default_browser().id` (`hasattr(catalog, "default_browser")`).
    fn default_browser_id(&self) -> Option<String> {
        None
    }
}

/// Minimal `configparser.ConfigParser(interpolation=None, strict=False)` with
/// `optionxform = str` (Python 3.14 `_read_inner` rules: full-line `#`/`;`
/// comments, empty lines kept inside values, deeper-indented continuation
/// lines, first `=`/`:` delimits). Returns the `[Desktop Entry]` options
/// (with `[DEFAULT]` fallbacks), or `None` on a parse error or a missing
/// section — Python skips such launchers.
fn desktop_entry_section(text: &str) -> Option<OrderedMap<String>> {
    let mut sections: OrderedMap<OrderedMap<Vec<String>>> = OrderedMap::new();
    let mut current: Option<String> = None;
    let mut option: Option<String> = None;
    let mut indent_level = usize::MAX;
    let mut failed = false;
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    if text.is_empty() {
        lines.clear();
    }
    for line in lines {
        let clean = line.trim();
        let comment = clean.starts_with('#') || clean.starts_with(';');
        if clean.is_empty() || comment {
            if !comment {
                if let (Some(section), Some(key)) = (&current, &option) {
                    sections[section.as_str()][key.as_str()].push(String::new());
                }
            }
            continue;
        }
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        if let (Some(section), Some(key)) = (&current, &option) {
            if !key.is_empty() {
                if indent > indent_level {
                    sections[section.as_str()][key.as_str()].push(clean.to_owned());
                    continue;
                }
            }
        }
        indent_level = indent;
        let header = clean.strip_prefix('[').and_then(|rest| rest.rfind(']').map(|end| &rest[..end])).filter(|h| !h.is_empty());
        if let Some(header) = header {
            sections.setdefault(header.to_owned(), OrderedMap::new());
            current = Some(header.to_owned());
            option = None;
            continue;
        }
        let Some(section) = &current else {
            return None; // MissingSectionHeaderError
        };
        match clean.find(['=', ':']) {
            Some(index) => {
                let key = clean[..index].trim_end().to_owned();
                if key.is_empty() {
                    failed = true;
                }
                let value = clean[index + 1..].trim().to_owned();
                sections[section.as_str()].insert(key.clone(), vec![value]);
                option = Some(key);
            }
            None => failed = true,
        }
    }
    if failed {
        return None;
    }
    let join = |values: OrderedMap<Vec<String>>| -> OrderedMap<String> {
        values.into_iter().map(|(k, v)| (k, v.join("\n").trim_end().to_owned())).collect()
    };
    let defaults = sections.remove("DEFAULT").map(join).unwrap_or_default();
    let mut entry = join(sections.remove("Desktop Entry")?);
    for (key, value) in defaults {
        if !entry.contains_key(&key) {
            entry.insert(key, value);
        }
    }
    Some(entry)
}

fn cairo_order(text: &str) -> Option<f64> {
    let order = tessel_core::pyjson::py_float(&Value::from(text))?;
    Some(if order.is_finite() { order } else { f64::INFINITY })
}

/// Resolve only installed apps from Cairo-Dock main-dock launcher metadata,
/// never its Exec. `cairo_dir` defaults to `~/.config/cairo-dock/current_theme`.
pub fn cairo_pins(catalog: &dyn PinCatalog, cairo_dir: Option<&Path>) -> Vec<String> {
    let root = cairo_dir.map(Path::to_path_buf).unwrap_or_else(|| tessel_core::paths::home().join(".config/cairo-dock/current_theme"));
    let launchers = if root.file_name().is_some_and(|n| n == "launchers") { root } else { root.join("launchers") };
    let mut paths: Vec<(String, PathBuf)> = std::fs::read_dir(&launchers)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    name.ends_with(".desktop").then(|| (name, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    let mut ordered: Vec<(f64, String, String)> = Vec::new();
    let leading_digits = regex::Regex::new(r"^\d+").expect("regex");
    for (name, path) in paths {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(text) = String::from_utf8(bytes) else { continue };
        let Some(section) = desktop_entry_section(&text) else { continue };
        if section.get("Type").map(String::as_str) != Some("Application")
            || section.get("Container").map(String::as_str).unwrap_or("_MainDock_") != "_MainDock_"
        {
            continue;
        }
        let Some(order) = cairo_order(section.get("Order").map(String::as_str).unwrap_or("inf")) else { continue };
        let mut candidates: Vec<String> = section
            .get("Origin")
            .map(String::as_str)
            .unwrap_or("")
            .split(';')
            .filter(|o| !o.is_empty())
            .map(|o| Path::new(o).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        candidates.push(name.clone());
        candidates.push(leading_digits.replace(&name, "").into_owned());
        let found = candidates.iter().find_map(|candidate| catalog.get_id(candidate).or_else(|| catalog.resolve_id(candidate)));
        if let Some(id) = found {
            ordered.push((order, name, id));
        }
    }
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
    dedupe(ordered.into_iter().map(|item| item.2))
}

/// First-run defaults only: App Center, the default browser, then Cairo pins.
pub fn initial_pins(catalog: &dyn PinCatalog, cairo_dir: Option<&Path>) -> Vec<String> {
    let mut defaults: Vec<String> = Vec::new();
    defaults.extend(catalog.get_id("org.dnf.AppCenter.desktop"));
    defaults.extend(catalog.default_browser_id());
    defaults.extend(cairo_pins(catalog, cairo_dir));
    dedupe(defaults)
}

/// `~/.config/tessel/dock/config.json`.
pub fn default_path() -> PathBuf {
    tessel_core::paths::component_config_dir("dock").join("config.json")
}

/// The exact bytes `ConfigStore.save` writes for an (already validated) config.
pub fn save_bytes(config: &DockConfig) -> String {
    let style = JsonStyle { ensure_ascii: false, indent: Some(2), ..JsonStyle::DEFAULT };
    json_dumps(&config.to_saved_value(), &style) + "\n"
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    pub path: PathBuf,
    /// Last load/save problem (`str(error)` in Python); empty when fine.
    pub error: String,
    /// A missing file means a new user, who starts on [`new_user_config`].
    new_user_layout: bool,
}

/// The Win layout of Global Settings > Desktop Layout, which a new user starts on.
const NEW_USER_LAYOUT: &str = include_str!("../../../settings/desktop/layouts/win.layout");

/// The default settings with [`NEW_USER_LAYOUT`] applied.
pub fn new_user_config() -> DockConfig {
    let layout: Value = serde_json::from_str(NEW_USER_LAYOUT).unwrap_or_default();
    let mut values = DockConfig::default().to_value();
    if let (Some(values), Some(dock)) = (values.as_object_mut(), layout["dock"].as_object()) {
        values.extend(dock.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    DockConfig::from_value(&values).unwrap_or_default()
}

impl Default for ConfigStore {
    fn default() -> Self {
        Self { new_user_layout: true, ..Self::new(default_path()) }
    }
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), error: String::new(), new_user_layout: false }
    }

    /// Read settings; only a missing file triggers defaults and the Cairo
    /// import. Never writes. A malformed file is left intact, `error` is set
    /// and defaults are returned.
    pub fn load(&mut self, catalog: Option<&dyn PinCatalog>, cairo_dir: Option<&Path>) -> DockConfig {
        self.error.clear();
        match self.read() {
            Ok(config) => config,
            Err(None) => {
                let start = if self.new_user_layout { new_user_config() } else { DockConfig::default() };
                DockConfig { pins: catalog.map(|c| initial_pins(c, cairo_dir)).unwrap_or_default(), ..start }
            }
            Err(Some(error)) => {
                self.error = error;
                DockConfig::default()
            }
        }
    }

    /// `Err(None)` = FileNotFoundError.
    fn read(&self) -> Result<DockConfig, Option<String>> {
        let os = |error: std::io::Error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                None
            } else {
                Some(tessel_core::pyerr::os_error_path(&error, &self.path))
            }
        };
        let metadata = std::fs::metadata(&self.path).map_err(os)?;
        if metadata.len() > 1024 * 1024 {
            return Err(Some("Dock settings file is too large.".into()));
        }
        let bytes = std::fs::read(&self.path).map_err(os)?;
        let text = super::pyjson_loads::decode_utf8(&bytes).map_err(Some)?;
        // Path.read_text uses universal newlines.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let value: Value = super::pyjson_loads::loads(&text).map_err(Some)?;
        DockConfig::from_value(&value).map_err(Some)
    }

    /// Validate and atomically write the config; returns the validated copy.
    pub fn save(&mut self, config: &DockConfig) -> Result<DockConfig, String> {
        let validated = DockConfig::from_value(&config.to_value())?;
        let data = save_bytes(&validated);
        tessel_core::clock::atomic_write(&self.path, data.as_bytes(), ".dock-settings-")
            .map_err(|e| tessel_core::pyerr::os_error_path(&e, &self.path))?;
        self.error.clear();
        Ok(validated)
    }
}
