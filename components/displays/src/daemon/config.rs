//! The daemon's configuration: `~/.config/tessel/displays/cfg.yaml`, in the
//! format way-displays uses, so an existing `~/.config/way-displays/cfg.yaml`
//! keeps working.

use crate::compat::yaml;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// CALLBACK_TITLE and CALLBACK_MSG are translated; CALLBACK_LEVEL is not, so scripts can test it.
pub const DEFAULT_CALLBACK: &str = "notify-send \"${CALLBACK_TITLE}\" \"${CALLBACK_MSG}\"";
/// Earlier defaults, which saved configurations may still contain.
const OLD_DEFAULT_CALLBACKS: [&str; 2] = [
    "notify-send \"way-displays ${CALLBACK_LEVEL}\" \"${CALLBACK_MSG}\"",
    "notify-send \"tessel-displays ${CALLBACK_LEVEL}\" \"${CALLBACK_MSG}\"",
];
/// The internal panel, turned off with the lid closed unless DISABLED says otherwise.
pub const LAPTOP_PANEL: &str = "!^eDP-[0-9]";
/// AUTO_SCALE_DPI meaning "choose by screen size". It is way-displays' default, which
/// every saved file and /etc/way-displays/cfg.yaml carry, so it does not show a choice.
pub const SCREEN_SIZE_DPI: i32 = 96;
const HEADER: &str = "# Tessel display layout. Global Settings > Displays writes this file.\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange { Row, Column }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align { Top, Middle, Bottom, Left, Right }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Round { Nearest, Up, Down }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level { Debug, Info, Warning, Error }

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModeChoice {
    Max,
    MaxPreferredRefresh,
    Size { width: i32, height: i32, mhz: Option<i32> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LidCondition { Closed, Open, NotPresent }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Condition {
    pub plugged: Vec<String>,
    pub unplugged: Vec<String>,
    pub lid: Option<LidCondition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cfg {
    pub arrange: Arrange,
    pub align: Align,
    pub order: Vec<String>,
    pub scaling: bool,
    /// 1 / SCALE_ROUND_TO: scales are multiples of 1/this.
    pub round_base: i32,
    pub round: Round,
    pub auto_scale: bool,
    pub dpi: i32,
    pub auto_min: f64,
    pub auto_max: f64,
    pub scales: Vec<(String, f64)>,
    pub modes: Vec<(String, ModeChoice)>,
    pub transforms: Vec<(String, i32)>,
    pub vrr_off: Vec<String>,
    pub callback: String,
    pub lid_monitor: bool,
    pub threshold: Level,
    pub disabled: Vec<(String, Vec<Condition>)>,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            arrange: Arrange::Row, align: Align::Top, order: Vec::new(), scaling: true, round_base: 8, round: Round::Nearest,
            auto_scale: false, dpi: SCREEN_SIZE_DPI, auto_min: 1.0, auto_max: -1.0, scales: Vec::new(), modes: Vec::new(), transforms: Vec::new(),
            vrr_off: Vec::new(), callback: DEFAULT_CALLBACK.into(), lid_monitor: true, threshold: Level::Warning,
            disabled: vec![(LAPTOP_PANEL.into(), vec![Condition { lid: Some(LidCondition::Closed), ..Default::default() }])],
        }
    }
}

fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) => match text.to_ascii_uppercase().as_str() {
            "ON" | "TRUE" | "YES" => Some(true),
            "OFF" | "FALSE" | "NO" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map(|items| items.iter().filter_map(text).collect()).unwrap_or_default()
}

/// The `wl_output.transform` value of a TRANSFORM name.
pub fn transform_value(name: &str) -> Option<i32> {
    Some(match name {
        "90" => 1,
        "180" => 2,
        "270" => 3,
        "FLIPPED" => 4,
        "FLIPPED-90" => 5,
        "FLIPPED-180" => 6,
        "FLIPPED-270" => 7,
        _ => return None,
    })
}

pub fn transform_name(value: i32) -> &'static str {
    match value {
        1 => "90",
        2 => "180",
        3 => "270",
        4 => "FLIPPED",
        5 => "FLIPPED-90",
        6 => "FLIPPED-180",
        7 => "FLIPPED-270",
        _ => "NORMAL",
    }
}

fn mode_choice(value: &Value) -> Option<ModeChoice> {
    let map = value.as_object()?;
    if map.get("MAX").and_then(boolean) == Some(true) {
        return Some(ModeChoice::Max);
    }
    if map.get("MAX_PREFERRED_REFRESH").and_then(boolean) == Some(true) {
        return Some(ModeChoice::MaxPreferredRefresh);
    }
    let width = map.get("WIDTH").and_then(number)? as i32;
    let height = map.get("HEIGHT").and_then(number)? as i32;
    let mhz = map.get("HZ").and_then(number).map(|hz| (hz * 1000.0).round() as i32);
    (width > 0 && height > 0).then_some(ModeChoice::Size { width, height, mhz })
}

fn conditions(value: &Value) -> Vec<Condition> {
    let Some(list) = value.get("IF").and_then(Value::as_array) else { return Vec::new() };
    list.iter().map(|item| Condition {
        plugged: strings(item.get("PLUGGED")),
        unplugged: strings(item.get("UNPLUGGED")),
        lid: item.get("LID").and_then(Value::as_str).and_then(|lid| match lid {
            "CLOSED" => Some(LidCondition::Closed),
            "OPEN" => Some(LidCondition::Open),
            "NOT_PRESENT" => Some(LidCondition::NotPresent),
            _ => None,
        }),
    }).collect()
}

/// A CFG mapping: the whole file, or the part an IPC request names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Partial {
    pub arrange: Option<Arrange>,
    pub align: Option<Align>,
    pub scaling: Option<bool>,
    pub auto_scale: Option<bool>,
    pub callback: Option<String>,
    pub order: Vec<String>,
    /// Every key named, also those whose value is unusable (a removal only needs the key).
    pub scale_keys: Vec<String>,
    pub scales: Vec<(String, f64)>,
    pub mode_keys: Vec<String>,
    pub modes: Vec<(String, ModeChoice)>,
    pub transform_keys: Vec<String>,
    pub transforms: Vec<(String, i32)>,
    pub vrr_off: Vec<String>,
    pub disabled: Vec<(String, Vec<Condition>)>,
}

impl Partial {
    pub fn from_value(cfg: &Value) -> Self {
        let mut out = Self::default();
        let Some(map) = cfg.as_object() else { return out };
        out.arrange = map.get("ARRANGE").and_then(Value::as_str).and_then(|v| match v {
            "ROW" => Some(Arrange::Row),
            "COLUMN" => Some(Arrange::Column),
            _ => None,
        });
        out.align = map.get("ALIGN").and_then(Value::as_str).and_then(|v| match v {
            "TOP" => Some(Align::Top),
            "MIDDLE" => Some(Align::Middle),
            "BOTTOM" => Some(Align::Bottom),
            "LEFT" => Some(Align::Left),
            "RIGHT" => Some(Align::Right),
            _ => None,
        });
        out.scaling = map.get("SCALING").and_then(boolean);
        out.auto_scale = map.get("AUTO_SCALE").and_then(boolean);
        out.callback = map.get("CALLBACK_CMD").and_then(Value::as_str).map(str::to_owned);
        out.order = strings(map.get("ORDER"));
        out.vrr_off = strings(map.get("VRR_OFF"));
        if let Some(scales) = map.get("SCALE").and_then(Value::as_object) {
            for (name, value) in scales {
                out.scale_keys.push(name.clone());
                if let Some(scale) = number(value).filter(|scale| *scale > 0.0) {
                    out.scales.push((name.clone(), scale));
                }
            }
        }
        if let Some(modes) = map.get("MODE").and_then(Value::as_object) {
            for (name, value) in modes {
                out.mode_keys.push(name.clone());
                if let Some(mode) = mode_choice(value) {
                    out.modes.push((name.clone(), mode));
                }
            }
        }
        if let Some(transforms) = map.get("TRANSFORM").and_then(Value::as_object) {
            for (name, value) in transforms {
                out.transform_keys.push(name.clone());
                if let Some(transform) = text(value).and_then(|v| transform_value(&v)) {
                    out.transforms.push((name.clone(), transform));
                }
            }
        }
        if let Some(disabled) = map.get("DISABLED").and_then(Value::as_object) {
            out.disabled = disabled.iter().map(|(name, value)| (name.clone(), conditions(value))).collect();
        }
        out
    }
}

fn upsert<T>(list: &mut Vec<(String, T)>, key: String, value: T) {
    match list.iter_mut().find(|(name, _)| *name == key) {
        Some(entry) => entry.1 = value,
        None => list.push((key, value)),
    }
}

impl Cfg {
    pub fn from_value(value: &Value) -> Self {
        let partial = Partial::from_value(value);
        let mut cfg = Self::default();
        let map = value.as_object();
        let get = |key: &str| map.and_then(|m| m.get(key));
        if let Some(arrange) = partial.arrange { cfg.arrange = arrange; }
        if let Some(align) = partial.align { cfg.align = align; }
        if let Some(scaling) = partial.scaling { cfg.scaling = scaling; }
        if let Some(auto_scale) = partial.auto_scale { cfg.auto_scale = auto_scale; }
        if let Some(callback) = partial.callback {
            cfg.callback = OLD_DEFAULT_CALLBACKS.iter().fold(callback, |text, old| text.replace(old, DEFAULT_CALLBACK));
        }
        cfg.order = partial.order;
        cfg.scales = partial.scales;
        cfg.modes = partial.modes;
        cfg.transforms = partial.transforms;
        cfg.vrr_off = partial.vrr_off;
        // A DISABLED key, even empty, replaces the default lid rule.
        if get("DISABLED").is_some() {
            cfg.disabled = partial.disabled;
        }
        if let Some(round) = get("SCALE_ROUND_TO").and_then(number) {
            cfg.round_base = match round {
                r if (r - 0.125).abs() < 1e-6 => 8,
                r if (r - 0.25).abs() < 1e-6 => 4,
                r if (r - 0.5).abs() < 1e-6 => 2,
                r if (r - 1.0).abs() < 1e-6 => 1,
                _ => cfg.round_base,
            };
        }
        match get("SCALE_ROUND_STRATEGY").and_then(Value::as_str) {
            Some("UP") => cfg.round = Round::Up,
            Some("DOWN") => cfg.round = Round::Down,
            Some("NEAREST") => cfg.round = Round::Nearest,
            _ => {}
        }
        if let Some(dpi) = get("AUTO_SCALE_DPI").and_then(number).filter(|dpi| *dpi > 8.0) { cfg.dpi = dpi as i32; }
        if let Some(min) = get("AUTO_SCALE_MIN").and_then(number).filter(|v| *v != 0.0) { cfg.auto_min = min; }
        if let Some(max) = get("AUTO_SCALE_MAX").and_then(number).filter(|v| *v != 0.0) { cfg.auto_max = max; }
        if let Some(monitor) = get("LAPTOP_LID_MONITOR").and_then(boolean) { cfg.lid_monitor = monitor; }
        match get("LOG_THRESHOLD").and_then(Value::as_str) {
            Some("DEBUG") => cfg.threshold = Level::Debug,
            Some("INFO") => cfg.threshold = Level::Info,
            Some("WARNING") => cfg.threshold = Level::Warning,
            Some("ERROR") => cfg.threshold = Level::Error,
            _ => {}
        }
        cfg.fix();
        cfg
    }

    /// An ALIGN that does not suit ARRANGE falls back to that arrangement's default.
    fn fix(&mut self) {
        self.align = match (self.arrange, self.align) {
            (Arrange::Column, Align::Top | Align::Bottom) => Align::Left,
            (Arrange::Row, Align::Left | Align::Right) => Align::Top,
            (_, align) => align,
        };
    }

    pub fn load(text: &str) -> Result<Self, String> {
        match yaml::load(text)? {
            value @ Value::Object(_) => Ok(Self::from_value(&value)),
            Value::Null => Ok(Self::default()),
            _ => Err("expected a mapping".into()),
        }
    }

    pub fn merged_set(&self, from: &Partial) -> Self {
        let mut merged = self.clone();
        if let Some(arrange) = from.arrange { merged.arrange = arrange; }
        if let Some(align) = from.align { merged.align = align; }
        if let Some(scaling) = from.scaling { merged.scaling = scaling; }
        if let Some(auto_scale) = from.auto_scale { merged.auto_scale = auto_scale; }
        if let Some(callback) = &from.callback { merged.callback = callback.clone(); }
        for (name, value) in &from.scales { upsert(&mut merged.scales, name.clone(), *value); }
        for (name, value) in &from.modes { upsert(&mut merged.modes, name.clone(), value.clone()); }
        for (name, value) in &from.transforms { upsert(&mut merged.transforms, name.clone(), *value); }
        for (name, value) in &from.disabled { upsert(&mut merged.disabled, name.clone(), value.clone()); }
        for name in &from.vrr_off {
            if !merged.vrr_off.contains(name) {
                merged.vrr_off.push(name.clone());
            }
        }
        if !from.order.is_empty() {
            merged.order = from.order.clone();
        }
        merged.fix();
        merged
    }

    pub fn merged_del(&self, from: &Partial) -> Self {
        let mut merged = self.clone();
        merged.scales.retain(|(name, _)| !from.scale_keys.contains(name));
        merged.modes.retain(|(name, _)| !from.mode_keys.contains(name));
        merged.transforms.retain(|(name, _)| !from.transform_keys.contains(name));
        merged.vrr_off.retain(|name| !from.vrr_off.contains(name));
        merged.disabled.retain(|(name, _)| !from.disabled.iter().any(|(other, _)| other == name));
        // Any ORDER in a removal means no saved order at all.
        if !from.order.is_empty() {
            merged.order.clear();
        }
        // Any CALLBACK_CMD in a removal means no callback at all.
        if from.callback.is_some() {
            merged.callback.clear();
        }
        merged
    }

    /// CFG_TOGGLE: a boolean present in the request flips, whatever its value.
    pub fn merged_toggle(&self, from: &Partial) -> Self {
        let mut merged = self.clone();
        if from.scaling.is_some() { merged.scaling = !merged.scaling; }
        if from.auto_scale.is_some() { merged.auto_scale = !merged.auto_scale; }
        for name in &from.vrr_off {
            match merged.vrr_off.iter().position(|other| other == name) {
                Some(index) => { merged.vrr_off.remove(index); }
                None => merged.vrr_off.push(name.clone()),
            }
        }
        for (name, value) in &from.disabled {
            match merged.disabled.iter().position(|(other, _)| other == name) {
                Some(index) => { merged.disabled.remove(index); }
                None => merged.disabled.push((name.clone(), value.clone())),
            }
        }
        merged
    }

    pub fn to_value(&self) -> Value {
        let or_null = |map: Map<String, Value>| if map.is_empty() { Value::Null } else { Value::Object(map) };
        let mut out = Map::new();
        out.insert("ARRANGE".into(), match self.arrange { Arrange::Row => "ROW", Arrange::Column => "COLUMN" }.into());
        out.insert("ALIGN".into(), match self.align {
            Align::Top => "TOP", Align::Middle => "MIDDLE", Align::Bottom => "BOTTOM", Align::Left => "LEFT", Align::Right => "RIGHT",
        }.into());
        out.insert("ORDER".into(), json!(self.order));
        out.insert("SCALING".into(), self.scaling.into());
        out.insert("SCALE_ROUND_TO".into(), json!(1.0 / f64::from(self.round_base)));
        out.insert("SCALE_ROUND_STRATEGY".into(), match self.round { Round::Nearest => "NEAREST", Round::Up => "UP", Round::Down => "DOWN" }.into());
        out.insert("AUTO_SCALE".into(), self.auto_scale.into());
        out.insert("AUTO_SCALE_DPI".into(), self.dpi.into());
        out.insert("AUTO_SCALE_MIN".into(), json!(self.auto_min));
        out.insert("AUTO_SCALE_MAX".into(), json!(self.auto_max));
        out.insert("SCALE".into(), or_null(self.scales.iter().map(|(name, value)| (name.clone(), json!(value))).collect()));
        out.insert("MODE".into(), or_null(self.modes.iter().map(|(name, mode)| (name.clone(), match mode {
            ModeChoice::Max => json!({"MAX": true}),
            ModeChoice::MaxPreferredRefresh => json!({"MAX_PREFERRED_REFRESH": true}),
            ModeChoice::Size { width, height, mhz: Some(mhz) } => json!({"WIDTH": width, "HEIGHT": height, "HZ": f64::from(*mhz) / 1000.0}),
            ModeChoice::Size { width, height, mhz: None } => json!({"WIDTH": width, "HEIGHT": height}),
        })).collect()));
        out.insert("TRANSFORM".into(), or_null(self.transforms.iter().map(|(name, value)| (name.clone(), transform_name(*value).into())).collect()));
        out.insert("VRR_OFF".into(), json!(self.vrr_off));
        out.insert("CALLBACK_CMD".into(), self.callback.clone().into());
        out.insert("LAPTOP_LID_MONITOR".into(), self.lid_monitor.into());
        out.insert("LOG_THRESHOLD".into(), self.threshold.name().into());
        out.insert("DISABLED".into(), or_null(self.disabled.iter().map(|(name, list)| {
            if list.is_empty() {
                return (name.clone(), Value::Null);
            }
            let conditions: Vec<Value> = list.iter().map(|condition| {
                let mut item = Map::new();
                if !condition.plugged.is_empty() { item.insert("PLUGGED".into(), json!(condition.plugged)); }
                if !condition.unplugged.is_empty() { item.insert("UNPLUGGED".into(), json!(condition.unplugged)); }
                if let Some(lid) = condition.lid {
                    item.insert("LID".into(), match lid {
                        LidCondition::Closed => "CLOSED", LidCondition::Open => "OPEN", LidCondition::NotPresent => "NOT_PRESENT",
                    }.into());
                }
                Value::Object(item)
            }).collect();
            (name.clone(), json!({"IF": conditions}))
        }).collect()));
        Value::Object(out)
    }

    pub fn file_text(&self) -> String {
        format!("{HEADER}{}", yaml::safe_dump(&self.to_value()))
    }
}

/// Where the settings are written.
pub fn config_path() -> PathBuf {
    tessel_core::paths::component_config_dir("displays").join("cfg.yaml")
}

/// Where they are read from, first existing wins: Tessel's own file, then the
/// files way-displays used, which is how an existing setup carries over.
pub fn read_candidates() -> Vec<PathBuf> {
    vec![
        config_path(),
        tessel_core::paths::config_home().join("way-displays/cfg.yaml"),
        PathBuf::from("/etc/tessel/displays/cfg.yaml"),
        PathBuf::from("/usr/local/etc/way-displays/cfg.yaml"),
        PathBuf::from("/etc/way-displays/cfg.yaml"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r##"# man 5 way-displays
ARRANGE: ROW
ALIGN: MIDDLE
ORDER:
- Dell Inc. AW3225QF 2Q49YZ3 (DP-2)
- UGU UGREEN-UHD 0x01000E00 (HDMI-A-1)
SCALING: TRUE
SCALE_ROUND_TO: 0.125
SCALE_ROUND_STRATEGY: NEAREST
AUTO_SCALE: TRUE
AUTO_SCALE_DPI: 96
AUTO_SCALE_MIN: 1
AUTO_SCALE_MAX: -1
SCALE:
  (null) (null) (DP-2): 1
  Dell Inc. AW3225QF 2Q49YZ3 (DP-1): 1.25
MODE:
  Dell Inc. AW3225QF 2Q49YZ3 (DP-2):
    WIDTH: 3840
    HEIGHT: 2160
    HZ: 239.99
  '!^HDMI':
    MAX: TRUE
TRANSFORM:
  HDMI-A-1: 90
VRR_OFF:
- (null) (null) (DP-2)
CALLBACK_CMD: "# way-displays-gui primary output begin\nif true; then\n  echo 'a'\nfi\n"
LAPTOP_LID_MONITOR: TRUE
LOG_THRESHOLD: INFO
DISABLED:
"##;

    #[test]
    fn a_way_displays_file_loads_unchanged() {
        let cfg = Cfg::load(FILE).unwrap();
        assert_eq!((cfg.arrange, cfg.align, cfg.scaling, cfg.auto_scale), (Arrange::Row, Align::Middle, true, true));
        assert_eq!(cfg.order[1], "UGU UGREEN-UHD 0x01000E00 (HDMI-A-1)");
        assert_eq!(cfg.scales, vec![("(null) (null) (DP-2)".to_owned(), 1.0), ("Dell Inc. AW3225QF 2Q49YZ3 (DP-1)".to_owned(), 1.25)]);
        assert_eq!(cfg.modes[0].1, ModeChoice::Size { width: 3840, height: 2160, mhz: Some(239_990) });
        assert_eq!(cfg.modes[1].1, ModeChoice::Max);
        assert_eq!(cfg.transforms, vec![("HDMI-A-1".to_owned(), 1)]);
        assert_eq!(cfg.vrr_off, ["(null) (null) (DP-2)"]);
        assert!(cfg.callback.starts_with("# way-displays-gui primary output begin\n") && cfg.callback.ends_with("fi\n"));
        assert_eq!(cfg.threshold, Level::Info);
        assert!(cfg.disabled.is_empty(), "an empty DISABLED turns the default lid rule off");
    }

    #[test]
    fn a_saved_default_notification_becomes_the_translated_one() {
        let file = "CALLBACK_CMD: \"# way-displays-gui primary output begin\\nxrandr\\n# end\\nnotify-send \\\"way-displays ${CALLBACK_LEVEL}\\\" \\\"${CALLBACK_MSG}\\\"\"\n";
        let cfg = Cfg::load(file).unwrap();
        assert!(cfg.callback.starts_with("# way-displays-gui primary output begin\nxrandr\n"), "{}", cfg.callback);
        assert!(cfg.callback.ends_with(DEFAULT_CALLBACK), "{}", cfg.callback);
        let own = "CALLBACK_CMD: my-script \"${CALLBACK_LEVEL}\"\n";
        assert_eq!(Cfg::load(own).unwrap().callback, "my-script \"${CALLBACK_LEVEL}\"");
    }

    #[test]
    fn what_the_daemon_writes_loads_back_the_same() {
        let cfg = Cfg::load(FILE).unwrap();
        assert_eq!(Cfg::load(&cfg.file_text()).unwrap(), cfg);
        let defaults = Cfg::default();
        assert_eq!(Cfg::load(&defaults.file_text()).unwrap(), defaults);
    }

    #[test]
    fn tessels_defaults_are_read_before_way_displays_and_add_only_auto_scale() {
        let text = include_str!("../../../../packaging/defaults/etc/tessel/displays/cfg.yaml");
        assert_eq!(Cfg::load(text).unwrap(), Cfg { auto_scale: true, ..Default::default() });
        let candidates = read_candidates();
        let at = |path: &str| candidates.iter().position(|c| c == &PathBuf::from(path)).unwrap();
        assert_eq!(candidates[0], config_path());
        assert!(at("/etc/tessel/displays/cfg.yaml") < at("/etc/way-displays/cfg.yaml"));
    }

    #[test]
    fn defaults_keep_nobaras_choices() {
        let cfg = Cfg::load("").unwrap();
        assert!(!cfg.auto_scale, "Nobara's way-displays did not auto-scale unless asked");
        assert_eq!(cfg.threshold, Level::Warning);
        assert_eq!(cfg.disabled[0].0, LAPTOP_PANEL);
        assert_eq!(cfg.disabled[0].1[0].lid, Some(LidCondition::Closed));
    }

    #[test]
    fn set_and_del_merge_like_the_gui_expects() {
        let cfg = Cfg::load(FILE).unwrap();
        let set = Partial::from_value(&json!({"SCALE": {"X": 1.5}, "VRR_OFF": ["X"], "DISABLED": {"Y": null}, "ORDER": ["Z"], "TRANSFORM": {"Z": "FLIPPED-90"}}));
        let merged = cfg.merged_set(&set);
        assert!(merged.scales.contains(&("X".to_owned(), 1.5)) && merged.scales.len() == 3);
        assert_eq!(merged.order, ["Z"]);
        assert!(merged.vrr_off.contains(&"X".to_owned()) && merged.vrr_off.contains(&"(null) (null) (DP-2)".to_owned()));
        assert!(merged.transforms.contains(&("Z".to_owned(), 5)));
        assert_eq!(merged.disabled, vec![("Y".to_owned(), vec![])]);

        let del = Partial::from_value(&json!({"SCALE": {"X": true}, "VRR_OFF": ["X"], "DISABLED": {"Y": null}, "TRANSFORM": {"Z": "NORMAL"}, "CALLBACK_CMD": ""}));
        let removed = merged.merged_del(&del);
        assert_eq!(removed.scales, cfg.scales);
        assert_eq!(removed.vrr_off, cfg.vrr_off);
        assert!(removed.disabled.is_empty());
        assert_eq!(removed.transforms, cfg.transforms);
        assert_eq!(removed.callback, "", "CALLBACK_CMD in a removal turns the callback off");
    }

    #[test]
    fn toggle_flips_what_it_names() {
        let cfg = Cfg::load(FILE).unwrap();
        let toggle = Partial::from_value(&json!({"AUTO_SCALE": true, "VRR_OFF": ["(null) (null) (DP-2)", "New"], "DISABLED": {"Y": null}}));
        let flipped = cfg.merged_toggle(&toggle);
        assert!(!flipped.auto_scale);
        assert_eq!(flipped.vrr_off, ["New"]);
        assert_eq!(flipped.disabled.len(), 1);
        assert_eq!(flipped.merged_toggle(&toggle).auto_scale, cfg.auto_scale);
    }

    #[test]
    fn an_align_that_does_not_suit_the_arrangement_is_replaced() {
        assert_eq!(Cfg::load("ARRANGE: COLUMN\nALIGN: TOP\n").unwrap().align, Align::Left);
        assert_eq!(Cfg::load("ARRANGE: ROW\nALIGN: RIGHT\n").unwrap().align, Align::Top);
    }
}
