//! night_light.py: night light settings and the night-light.service runner
//! (wlsunset), restricted to SDR outputs: gamma tables would distort HDR output.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use tiling_manager::xml::{Element, Node};

use crate::compat::{self, local_lower, value, Result};

pub const UNIT: &str = "night-light.service";
pub const MIN_TEMPERATURE: i64 = 1900;
pub const MAX_TEMPERATURE: i64 = 6000;
pub const DAY_TEMPERATURE: i64 = 6500;
static TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([01]?\d|2[0-3]):([0-5]\d)$").unwrap());

/// `global-settings/night-light.service`, installed to `~/.config/systemd/user/`.
pub const SERVICE: &str = "[Unit]
Description=Night light (warmer screen colors with wlsunset)
Documentation=man:wlsunset(1)
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=exec
EnvironmentFile=-%h/.config/labwc/environment
ExecStart=/usr/bin/python3 %h/.local/share/global-settings/night_light.py --run
Restart=on-failure
RestartSec=5

[Install]
WantedBy=graphical-session.target
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NightLightConfig {
    pub enabled: bool,
    /// "sun" (sunset to sunrise) or "manual".
    pub schedule: String,
    pub sunset: String,
    pub sunrise: String,
    pub temperature: i64,
}

impl Default for NightLightConfig {
    fn default() -> Self {
        NightLightConfig { enabled: false, schedule: "sun".into(), sunset: "20:00".into(), sunrise: "07:00".into(), temperature: 4000 }
    }
}

impl NightLightConfig {
    /// `asdict(config)` in field order.
    pub fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("enabled".into(), Value::Bool(self.enabled));
        map.insert("schedule".into(), Value::String(self.schedule.clone()));
        map.insert("sunset".into(), Value::String(self.sunset.clone()));
        map.insert("sunrise".into(), Value::String(self.sunrise.clone()));
        map.insert("temperature".into(), Value::from(self.temperature));
        Value::Object(map)
    }
}

pub fn validate(data: &Value) -> Result<NightLightConfig> {
    let Value::Object(map) = data else { return value("Invalid night light settings.") };
    let mut values = NightLightConfig::default().to_json();
    for (key, item) in map {
        if let Some(slot) = values.get_mut(key) {
            *slot = item.clone();
        }
    }
    let enabled = values["enabled"].as_bool();
    let schedule = values["schedule"].as_str().filter(|s| ["sun", "manual"].contains(s));
    let (Some(enabled), Some(schedule)) = (enabled, schedule) else { return value("Invalid night light settings.") };
    let mut times = Vec::new();
    for key in ["sunset", "sunrise"] {
        let Some(text) = values[key].as_str().filter(|t| TIME.is_match(t)) else {
            return value("Times must use the 24-hour HH:MM format.");
        };
        let (hours, minutes) = text.split_once(':').unwrap();
        times.push(format!("{:02}:{minutes}", hours.parse::<i64>().unwrap_or(0)));
    }
    let temperature = match &values["temperature"] {
        Value::Number(n) if n.is_i64() => n.as_i64().unwrap(),
        _ => return value("Invalid color temperature."),
    };
    if !(MIN_TEMPERATURE..=MAX_TEMPERATURE).contains(&temperature) {
        return value("Invalid color temperature.");
    }
    Ok(NightLightConfig { enabled, schedule: schedule.into(), sunrise: times.pop().unwrap(), sunset: times.pop().unwrap(), temperature })
}

/// `json.dumps(asdict(config), indent=2) + '\n'`.
pub fn dumps(config: &NightLightConfig) -> String {
    suite_core::pyfmt::json_dumps(&config.to_json(), &suite_core::pyfmt::JsonStyle { indent: Some(2), item_separator: ",", ..suite_core::pyfmt::JsonStyle::DEFAULT }) + "\n"
}

#[derive(Clone, Debug)]
pub struct NightLightStore {
    pub path: PathBuf,
    pub weather: PathBuf,
}

impl Default for NightLightStore {
    fn default() -> Self { Self::new(None, None) }
}

impl NightLightStore {
    pub fn new(path: Option<&Path>, weather: Option<&Path>) -> NightLightStore {
        let home = crate::labwc_settings::config_home();
        NightLightStore {
            path: path.map(Path::to_path_buf).unwrap_or_else(|| home.join("night-light/config.json")),
            weather: weather.map(Path::to_path_buf).unwrap_or_else(|| home.join("weather-manager/config.json")),
        }
    }

    pub fn load(&self) -> NightLightConfig {
        std::fs::read_to_string(&self.path).ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|data| validate(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, config: &NightLightConfig) -> Result<NightLightConfig> {
        let config = validate(&config.to_json())?;
        crate::labwc_settings::atomic_write(&self.path, dumps(&config).as_bytes())?;
        Ok(config)
    }

    /// `(name, latitude, longitude)` saved in Weather settings, or None.
    pub fn location(&self) -> Option<(String, f64, f64)> {
        if std::fs::metadata(&self.weather).ok()?.len() > 65536 {
            return None;
        }
        let data: Value = serde_json::from_str(&std::fs::read_to_string(&self.weather).ok()?).ok()?;
        let number = |v: &Value| match v { Value::Number(n) => n.as_f64(), _ => None };
        let latitude = number(data.get("latitude")?)?;
        let longitude = number(data.get("longitude")?)?;
        if latitude.is_finite() && longitude.is_finite() && (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude) {
            let name = data.get("name").filter(|v| suite_core::pyjson::truthy(v)).map(suite_core::pyjson::py_str).unwrap_or_default();
            return Some((name, latitude, longitude));
        }
        None
    }
}

fn boolean(text: &str, default: bool) -> bool {
    match compat::strip(text).to_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => true,
        "no" | "false" | "0" | "off" => false,
        _ => default,
    }
}

/// ElementTree `.text`: character data before the first child element.
fn leading_text(element: &Element) -> String {
    let mut text = String::new();
    for node in &element.children {
        match node {
            Node::Text(t) | Node::CData(t) => text.push_str(t),
            Node::Element(_) => break,
            _ => {}
        }
    }
    text
}

/// `(global HDR, {output: enabled})` from labwc's rc.xml.
pub fn hdr_policy(rc_text: &str) -> (bool, HashMap<String, bool>) {
    let Ok(document) = tiling_manager::xml::parse(rc_text.as_bytes(), None) else { return (false, HashMap::new()) };
    let (mut enabled, mut outputs) = (false, HashMap::new());
    fn walk(element: &Element, enabled: &mut bool, outputs: &mut HashMap<String, bool>) {
        let tag = local_lower(&element.name);
        if tag == "core" {
            for child in element.elements() {
                if local_lower(&child.name) == "hdr" {
                    *enabled = boolean(&leading_text(child), *enabled);
                }
            }
        } else if tag == "hdroutputs" {
            for output in element.elements() {
                let name = output.attribute("name").unwrap_or("");
                if let (false, Some(state)) = (name.is_empty(), output.attribute("enabled")) {
                    outputs.insert(name.to_owned(), boolean(state, false));
                }
            }
        }
        for child in element.elements() {
            walk(child, enabled, outputs);
        }
    }
    walk(&document.root, &mut enabled, &mut outputs);
    (enabled, outputs)
}

/// `wlr-randr --json` names of enabled outputs, None when unknown.
pub fn enabled_outputs() -> Option<Vec<String>> {
    let mut command = std::process::Command::new("wlr-randr");
    command.arg("--json");
    let output = crate::input_devices::run_timeout(command, 5)?;
    parse_outputs(&String::from_utf8_lossy(&output.stdout))
}

pub fn parse_outputs(text: &str) -> Option<Vec<String>> {
    let data: Value = serde_json::from_str(text).ok()?;
    let mut names = Vec::new();
    for item in data.as_array()? {
        if item.get("enabled").is_some_and(suite_core::pyjson::truthy) {
            names.push(item.get("name")?.as_str()?.to_owned());
        }
    }
    Some(names)
}

/// `(sdr, hdr)` output names.
pub fn split_outputs(outputs: &[String], rc_text: &str) -> (Vec<String>, Vec<String>) {
    let (default, overrides) = hdr_policy(rc_text);
    let hdr: Vec<String> = outputs.iter().filter(|n| *overrides.get(*n).unwrap_or(&default)).cloned().collect();
    (outputs.iter().filter(|n| !hdr.contains(n)).cloned().collect(), hdr)
}

pub fn rc_text() -> String {
    std::fs::read(crate::labwc_settings::rc_path()).ok().and_then(|d| String::from_utf8(d).ok()).unwrap_or_default()
}

/// wlsunset argv, or None when nothing should run.
pub fn command(config: &NightLightConfig, location: Option<&(String, f64, f64)>, sdr_outputs: &[String],
               all_outputs: &[String], program: &str) -> Option<Vec<String>> {
    if !config.enabled || sdr_outputs.is_empty() {
        return None;
    }
    let mut argv: Vec<String> = vec![program.into(), "-t".into(), config.temperature.to_string(), "-T".into(), DAY_TEMPERATURE.to_string()];
    match location {
        Some(location) if config.schedule == "sun" => {
            argv.extend(["-l".into(), format!("{:.4}", location.1), "-L".into(), format!("{:.4}", location.2)]);
        }
        _ => argv.extend(["-S".into(), config.sunrise.clone(), "-s".into(), config.sunset.clone()]),
    }
    let sdr: std::collections::BTreeSet<&String> = sdr_outputs.iter().collect();
    let all: std::collections::BTreeSet<&String> = all_outputs.iter().collect();
    if sdr != all {
        for name in sdr_outputs {
            argv.extend(["-o".into(), name.clone()]);
        }
    }
    Some(argv)
}

/// `night_light.py --run` minus the final exec: the argv to exec, or the exit
/// status and the messages printed to stderr.
pub fn run_plan(store: &NightLightStore, outputs: Option<Vec<String>>, rc: &str, which: &dyn Fn(&str) -> bool)
                -> std::result::Result<Vec<String>, (i32, Vec<String>)> {
    let config = store.load();
    let mut messages = Vec::new();
    let (outputs, sdr) = match outputs {
        None => (vec!["*".to_owned()], vec!["*".to_owned()]),
        Some(outputs) => {
            let (sdr, hdr) = split_outputs(&outputs, rc);
            if !hdr.is_empty() {
                messages.push(format!("Night light skips HDR outputs: {}", hdr.join(", ")));
            }
            (outputs, sdr)
        }
    };
    let Some(argv) = command(&config, store.location().as_ref(), &sdr, &outputs, "wlsunset") else {
        messages.push("Night light is off or no SDR output is available.".into());
        return Err((0, messages));
    };
    if !which(&argv[0]) {
        messages.push("wlsunset is not installed.".into());
        return Err((1, messages));
    }
    if !messages.is_empty() {
        for message in &messages {
            eprintln!("{message}");
        }
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    fn cfg(enabled: bool, schedule: &str, sunset: &str, sunrise: &str, temperature: i64) -> NightLightConfig {
        NightLightConfig { enabled, schedule: schedule.into(), sunset: sunset.into(), sunrise: sunrise.into(), temperature }
    }

    #[test]
    fn config_validation_and_location() {
        let root = scratch("night");
        let store = NightLightStore::new(Some(&root.join("n.json")), Some(&root.join("w.json")));
        assert_eq!(store.load(), NightLightConfig::default());
        store.save(&cfg(true, "manual", "7:05", "06:30", 3500)).unwrap();
        assert_eq!(store.load().sunset, "07:05");
        assert_eq!(std::fs::read_to_string(&store.path).unwrap(),
                   "{\n  \"enabled\": true,\n  \"schedule\": \"manual\",\n  \"sunset\": \"07:05\",\n  \"sunrise\": \"06:30\",\n  \"temperature\": 3500\n}\n");
        assert!(store.save(&NightLightConfig { sunset: "25:00".into(), ..Default::default() }).unwrap_err().is_value());
        assert!(store.save(&NightLightConfig { temperature: 9000, ..Default::default() }).unwrap_err().is_value());
        assert!(store.location().is_none());
        std::fs::write(root.join("w.json"), r#"{"name": "Oslo", "latitude": 59.9, "longitude": 10.7}"#).unwrap();
        assert_eq!(store.location(), Some(("Oslo".into(), 59.9, 10.7)));
        std::fs::write(root.join("w.json"), r#"{"latitude": true, "longitude": 10.7}"#).unwrap();
        assert!(store.location().is_none());
    }

    #[test]
    fn hdr_outputs_are_excluded() {
        let rc = r#"<labwc_config><core><hdr>yes</hdr></core><hdrOutputs><output name="DP-1" enabled="no"/></hdrOutputs></labwc_config>"#;
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(split_outputs(&names(&["DP-1", "HDMI-A-1"]), rc), (names(&["DP-1"]), names(&["HDMI-A-1"])));
        assert_eq!(split_outputs(&names(&["DP-1"]), "<labwc_config/>"), (names(&["DP-1"]), vec![]));
        let config = cfg(true, "sun", "20:00", "07:00", 3000);
        let location = ("X".to_string(), 1.5, 2.25);
        assert_eq!(command(&config, Some(&location), &names(&["DP-1"]), &names(&["DP-1", "HDMI-A-1"]), "wlsunset").unwrap(),
                   names(&["wlsunset", "-t", "3000", "-T", "6500", "-l", "1.5000", "-L", "2.2500", "-o", "DP-1"]));
        assert_eq!(command(&config, None, &names(&["DP-1"]), &names(&["DP-1"]), "wlsunset").unwrap(),
                   names(&["wlsunset", "-t", "3000", "-T", "6500", "-S", "07:00", "-s", "20:00"]));
        assert!(command(&config, None, &[], &names(&["HDMI-A-1"]), "wlsunset").is_none());
        assert!(command(&NightLightConfig::default(), None, &names(&["DP-1"]), &names(&["DP-1"]), "wlsunset").is_none());
        assert_eq!(parse_outputs(r#"[{"name":"DP-1","enabled":true},{"name":"X","enabled":false}]"#), Some(names(&["DP-1"])));
    }

    #[test]
    fn unit_file_runs_the_installed_runner() {
        assert!(SERVICE.contains("ExecStart=/usr/bin/python3 %h/.local/share/global-settings/night_light.py --run"));
        assert!(SERVICE.contains("PartOf=graphical-session.target"));
        let python = std::path::Path::new(env!("HOME")).join("btmanager/global-settings/night-light.service");
        if let Ok(text) = std::fs::read_to_string(python) {
            assert_eq!(text, SERVICE);
        }
    }

    #[test]
    fn run_plan_without_outputs_or_program() {
        let root = scratch("night-run");
        let store = NightLightStore::new(Some(&root.join("n.json")), Some(&root.join("w.json")));
        assert_eq!(run_plan(&store, None, "", &|_| true).unwrap_err().0, 0);
        store.save(&cfg(true, "manual", "20:00", "07:00", 3000)).unwrap();
        assert_eq!(run_plan(&store, None, "", &|_| false).unwrap_err().0, 1);
        assert_eq!(run_plan(&store, None, "", &|_| true).unwrap()[..3], ["wlsunset", "-t", "3000"]);
    }
}
