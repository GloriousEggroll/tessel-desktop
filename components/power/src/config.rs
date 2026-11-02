//! Validated, atomic storage for session power preferences (power_config.py).

use serde_json::{json, Map, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const SLEEP_ACTIONS: [&str; 3] = ["suspend", "hibernate", "suspend-then-hibernate"];
pub const LID_ACTIONS: [&str; 5] = ["suspend", "hibernate", "lock", "nothing", "poweroff"];
pub const BUTTON_ACTIONS: [&str; 5] = ["menu", "suspend", "hibernate", "poweroff", "nothing"];
pub const CRITICAL_ACTIONS: [&str; 5] = ["auto", "hibernate", "poweroff", "suspend", "nothing"];
pub const CRITICAL_LEVELS: [i64; 7] = [0, 2, 3, 4, 5, 7, 10];
pub const MAX_TIMEOUT: i64 = 24 * 60 * 60;
const MAX_FILE: usize = 65536;

/// A fresh copy of DEFAULTS (key order as in Python).
pub fn defaults() -> Value {
    json!({
        "version": 1,
        "lock_after": 300,
        "ac": {"display_off": 600, "sleep_after": 0, "sleep_action": "suspend"},
        // New configurations sleep laptops after 15 idle minutes on battery.
        "battery": {"display_off": 600, "sleep_after": 900, "sleep_action": "suspend"},
        "dim_before_lock": true,
        "lid_close": {"ac": "suspend", "battery": "suspend"},
        "power_button": "menu",
        "critical_action": "auto",
        "critical_level": 0,
    })
}

pub fn settings_path() -> PathBuf {
    tessel_core::paths::config_home().join("tessel-power/settings.json")
}

/// Python `type(value) is int` (booleans and floats excluded).
pub fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) if !number.is_f64() => number.as_i64(),
        _ => None,
    }
}

fn seconds(value: &Value, name: &str) -> Result<i64, String> {
    match as_int(value) {
        Some(v) if (0..=MAX_TIMEOUT).contains(&v) => Ok(v),
        _ => Err(format!("{name} must be a whole number of seconds from 0 to {MAX_TIMEOUT}.")),
    }
}

fn unknown_keys(map: &Map<String, Value>, allowed: &Map<String, Value>) -> bool {
    map.keys().any(|key| !allowed.contains_key(key))
}

/// Normalize partial preferences (validate_settings).
pub fn validate_settings(settings: &Value) -> Result<Value, String> {
    let Value::Object(settings) = settings else { return Err("Power settings must be an object.".into()) };
    let mut result = defaults();
    let base = result.as_object().unwrap().clone();
    if unknown_keys(settings, &base) {
        return Err("Power settings contain unrecognized options.".into());
    }
    if let Some(version) = settings.get("version") {
        if as_int(version) != Some(1) {
            return Err("This power settings version is not supported.".into());
        }
    }
    let out = result.as_object_mut().unwrap();
    if let Some(value) = settings.get("lock_after") {
        out["lock_after"] = seconds(value, "Screen lock delay")?.into();
    }
    for source in ["ac", "battery"] {
        let empty = Value::Object(Map::new());
        let supplied = settings.get(source).unwrap_or(&empty);
        let Value::Object(supplied) = supplied else {
            return Err(format!("The {source} power policy must be an object."));
        };
        if unknown_keys(supplied, base[source].as_object().unwrap()) {
            return Err(format!("The {source} power policy contains unrecognized options."));
        }
        let policy = out[source].as_object_mut().unwrap();
        for field in ["display_off", "sleep_after"] {
            if let Some(value) = supplied.get(field) {
                policy[field] = seconds(value, &format!("{source}: {field}"))?.into();
            }
        }
        if let Some(action) = supplied.get("sleep_action") {
            match action.as_str() {
                Some(a) if SLEEP_ACTIONS.contains(&a) => policy["sleep_action"] = a.into(),
                _ => return Err("Choose Sleep, Hibernate, or Sleep then hibernate.".into()),
            }
        }
    }
    if let Some(dim) = settings.get("dim_before_lock") {
        let Value::Bool(dim) = dim else { return Err("Dim screen before locking must be enabled or disabled.".into()) };
        out["dim_before_lock"] = (*dim).into();
    }
    let empty = Value::Object(Map::new());
    let lid = settings.get("lid_close").unwrap_or(&empty);
    let Value::Object(lid) = lid else { return Err("The lid close policy is invalid.".into()) };
    if lid.keys().any(|key| key != "ac" && key != "battery") {
        return Err("The lid close policy is invalid.".into());
    }
    for source in ["ac", "battery"] {
        if let Some(value) = lid.get(source) {
            match value.as_str() {
                Some(v) if LID_ACTIONS.contains(&v) => out["lid_close"][source] = v.into(),
                _ => return Err("Choose a supported lid close action.".into()),
            }
        }
    }
    for (field, choices) in [("power_button", &BUTTON_ACTIONS[..]), ("critical_action", &CRITICAL_ACTIONS[..])] {
        if let Some(value) = settings.get(field) {
            match value.as_str() {
                Some(v) if choices.contains(&v) => out[field] = v.into(),
                _ => return Err(format!("Choose a supported {}.", field.replace('_', " "))),
            }
        }
    }
    if let Some(level) = settings.get("critical_level") {
        match as_int(level) {
            Some(v) if CRITICAL_LEVELS.contains(&v) => out["critical_level"] = v.into(),
            _ => return Err("Choose a supported critical battery level.".into()),
        }
    }
    Ok(result)
}

#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Python str(OSError): "[Errno N] message".
            ConfigError::Io(error) => match error.raw_os_error() {
                Some(code) => write!(f, "[Errno {code}] {}", strip_os_suffix(&error.to_string())),
                None => write!(f, "{error}"),
            },
            ConfigError::Invalid(message) => f.write_str(message),
        }
    }
}

fn strip_os_suffix(text: &str) -> String {
    text.split(" (os error").next().unwrap_or(text).to_owned()
}

/// A missing file uses defaults; invalid existing files remain untouched.
pub fn load_settings(path: Option<&Path>) -> Result<Value, ConfigError> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(settings_path);
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(defaults()),
        Err(error) => return Err(ConfigError::Io(error)),
    };
    let mut bytes = Vec::new();
    file.take((MAX_FILE * 4 + 4) as u64).read_to_end(&mut bytes).map_err(ConfigError::Io)?;
    let text = String::from_utf8(bytes).map_err(|_| ConfigError::Invalid("The power settings file is not valid UTF-8.".into()))?;
    if text.chars().count() > MAX_FILE {
        return Err(ConfigError::Invalid("The power settings file is too large.".into()));
    }
    let value: Value = serde_json::from_str(&text).map_err(|error| ConfigError::Invalid(error.to_string()))?;
    validate_settings(&value).map_err(ConfigError::Invalid)
}

#[cfg(test)]
thread_local!(pub static FAIL_REPLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// Pretty text as `json.dump(indent=2, ensure_ascii=False)` + newline.
pub fn encode_file(settings: &Value) -> String {
    let style = JsonStyle { ensure_ascii: false, indent: Some(2), ..JsonStyle::DEFAULT };
    json_dumps(settings, &style) + "\n"
}

/// Replace only a fully written and validated preferences file.
pub fn save_settings(settings: &Value, path: Option<&Path>) -> Result<(), ConfigError> {
    use std::os::unix::fs::OpenOptionsExt;
    let validated = validate_settings(settings).map_err(ConfigError::Invalid)?;
    let path = path.map(Path::to_path_buf).unwrap_or_else(settings_path);
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    fs::create_dir_all(&parent).map_err(ConfigError::Io)?;
    let mut created = None;
    for attempt in 0..100u32 {
        let random = glib::random_int() ^ attempt.wrapping_mul(2654435761);
        let temporary = parent.join(format!(".power-settings-{random:08x}"));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary) {
            Ok(file) => {
                created = Some((file, temporary));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ConfigError::Io(error)),
        }
    }
    let Some((mut file, temporary)) = created else {
        return Err(ConfigError::Io(io::Error::from_raw_os_error(libc::EEXIST)));
    };
    let result = (|| {
        file.write_all(encode_file(&validated).as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        #[cfg(test)]
        if FAIL_REPLACE.with(|fail| fail.get()) {
            return Err(io::Error::other("disk failure"));
        }
        fs::rename(&temporary, &path)?;
        let directory = fs::File::open(&parent)?;
        directory.sync_all()
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(ConfigError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let directory = base.join(format!("power-config-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_settings_preserve_existing_idle_behavior_and_are_detached() {
        let directory = temp("missing");
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("missing/settings.json");
        let mut first = load_settings(Some(&path)).unwrap();
        assert_eq!(first, defaults());
        assert_eq!(first["lock_after"], 300);
        for source in ["ac", "battery"] {
            assert_eq!(first[source]["display_off"], 600);
        }
        assert_eq!(first["ac"]["sleep_after"], 0);
        assert_eq!(first["battery"]["sleep_after"], 900);
        assert_eq!(first["dim_before_lock"], true);
        assert_eq!(first["lid_close"], json!({"ac": "suspend", "battery": "suspend"}));
        assert_eq!((first["power_button"].clone(), first["critical_action"].clone(), first["critical_level"].clone()),
                   (json!("menu"), json!("auto"), json!(0)));
        first["ac"]["display_off"] = 0.into();
        assert_eq!(load_settings(Some(&path)).unwrap()["ac"]["display_off"], 600);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn independent_policies_round_trip_with_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = temp("roundtrip");
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("power/settings.json");
        let settings = validate_settings(&json!({"battery": {"display_off": 120, "sleep_after": 900, "sleep_action": "hibernate"}})).unwrap();
        save_settings(&settings, Some(&path)).unwrap();
        assert_eq!(load_settings(Some(&path)).unwrap(), settings);
        assert_eq!(load_settings(Some(&path)).unwrap()["ac"], defaults()["ac"]);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // Byte-identical to Python's json.dump(indent=2, ensure_ascii=False) + "\n".
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\n  \"version\": 1,\n  \"lock_after\": 300,\n  \"ac\": {\n    \"display_off\": 600,"));
        assert!(text.ends_with("\"critical_level\": 0\n}\n"));
    }

    #[test]
    fn invalid_types_versions_actions_and_bounds_are_rejected() {
        let mut invalid = vec![Value::Null, json!([]), json!({"version": true}), json!({"version": 2}), json!({"unknown": 0}),
            json!({"lock_after": true}), json!({"lock_after": 86401}), json!({"lock_after": -1}),
            json!({"ac": null}), json!({"ac": {"unknown": 0}}), json!({"battery": {"sleep_action": "shutdown"}})];
        for value in [json!(true), json!("300"), json!(1.5), json!(-1), json!(86401), Value::Null] {
            invalid.push(json!({"ac": {"display_off": value.clone()}}));
            invalid.push(json!({"battery": {"sleep_after": value}}));
        }
        for value in invalid {
            assert!(validate_settings(&value).is_err(), "{value}");
        }
        assert_eq!(validate_settings(&json!({"ac": {"display_off": 86400}})).unwrap()["ac"]["display_off"], 86400);
        assert_eq!(validate_settings(&json!(null)).unwrap_err(), "Power settings must be an object.");
        assert_eq!(validate_settings(&json!({"ac": {"display_off": -1}})).unwrap_err(),
                   "ac: display_off must be a whole number of seconds from 0 to 86400.");
    }

    #[test]
    fn existing_saved_policy_keeps_its_battery_sleep_choice() {
        let directory = temp("existing");
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("settings.json");
        fs::write(&path, json!({"version": 1, "lock_after": 300,
            "ac": {"display_off": 600, "sleep_after": 0, "sleep_action": "suspend"},
            "battery": {"display_off": 600, "sleep_after": 0, "sleep_action": "suspend"}}).to_string()).unwrap();
        let loaded = load_settings(Some(&path)).unwrap();
        assert_eq!(loaded["battery"]["sleep_after"], 0);
        assert_eq!(loaded["dim_before_lock"], true);
    }

    #[test]
    fn lid_button_and_critical_options_are_validated() {
        let valid = validate_settings(&json!({"lid_close": {"ac": "nothing"}, "power_button": "poweroff",
            "critical_action": "suspend", "critical_level": 5, "dim_before_lock": false})).unwrap();
        assert_eq!(valid["lid_close"], json!({"ac": "nothing", "battery": "suspend"}));
        for value in [json!({"lid_close": {"ac": "reboot"}}), json!({"lid_close": {"dock": "lock"}}), json!({"lid_close": []}),
                      json!({"power_button": "lock"}), json!({"critical_action": "menu"}), json!({"critical_level": 50}),
                      json!({"critical_level": true}), json!({"dim_before_lock": 1})] {
            assert!(validate_settings(&value).is_err(), "{value}");
        }
        assert_eq!(validate_settings(&json!({"power_button": "x"})).unwrap_err(), "Choose a supported power button.");
    }

    #[test]
    fn failed_atomic_replacement_preserves_previous_file_and_cleans_temporary() {
        let directory = temp("replace");
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("settings.json");
        save_settings(&defaults(), Some(&path)).unwrap();
        let original = fs::read(&path).unwrap();
        FAIL_REPLACE.with(|fail| fail.set(true));
        let result = save_settings(&json!({"ac": {"display_off": 0}}), Some(&path));
        FAIL_REPLACE.with(|fail| fail.set(false));
        assert!(matches!(result, Err(ConfigError::Io(_))));
        assert_eq!(fs::read(&path).unwrap(), original);
        let entries: Vec<_> = fs::read_dir(&directory).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(entries, vec![path]);
    }

    #[test]
    fn malformed_existing_file_is_reported_without_overwrite() {
        let directory = temp("malformed");
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("settings.json");
        for text in ["{broken".to_owned(), json!({"version": 99}).to_string(), " ".repeat(65537)] {
            fs::write(&path, &text).unwrap();
            assert!(matches!(load_settings(Some(&path)), Err(ConfigError::Invalid(_))));
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn xdg_config_home_is_respected_without_mutating_input() {
        // settings_path() follows XDG_CONFIG_HOME; checked without mutating the process env.
        let settings = json!({"ac": {"sleep_after": 300}});
        let original = settings.clone();
        let directory = temp("xdg");
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("tessel-power/settings.json");
        save_settings(&settings, Some(&path)).unwrap();
        assert_eq!(load_settings(Some(&path)).unwrap()["ac"]["sleep_after"], 300);
        assert_eq!(settings, original);
        if let Ok(home) = std::env::var("XDG_CONFIG_HOME") {
            if !home.is_empty() {
                assert_eq!(settings_path(), PathBuf::from(home).join("tessel-power/settings.json"));
            }
        }
    }
}
