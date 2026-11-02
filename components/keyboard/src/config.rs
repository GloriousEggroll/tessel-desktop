//! Small, validated settings shared by the keyboard and its settings page (osk_config.py).
use serde_json::{json, Map, Value};
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const LAYOUTS: [(&str, &str); 14] = [("us", "English (US)"), ("gb", "English (UK)"), ("de", "Deutsch"),
    ("fr", "Français"), ("es", "Español"), ("it", "Italiano"), ("pt", "Português"), ("br", "Português (Brasil)"),
    ("ru", "Русский"), ("ua", "Українська"), ("tr", "Türkçe"), ("se", "Svenska"), ("no", "Norsk"), ("dk", "Dansk")];
pub const SIZES: [&str; 3] = ["small", "medium", "large"];
const KEYS: [&str; 4] = ["automatic", "layout", "size", "ignore_keyboards"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub automatic: bool,
    pub layout: String,
    pub size: String,
    pub ignore_keyboards: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { automatic: true, layout: "system".into(), size: "medium".into(), ignore_keyboards: Vec::new() }
    }
}

impl Settings {
    /// Same key order as Python's `{**DEFAULT, ...}`.
    pub fn to_value(&self) -> Value {
        json!({"automatic": self.automatic, "layout": self.layout, "size": self.size, "ignore_keyboards": self.ignore_keyboards})
    }
}

/// `validate()`: unknown keys are dropped, every known key is checked.
pub fn validate(data: &Value) -> Result<Settings, String> {
    let Value::Object(data) = data else { return Err("Invalid keyboard settings.".into()) };
    let mut merged: Map<String, Value> = match Settings::default().to_value() { Value::Object(map) => map, _ => unreachable!() };
    for key in KEYS {
        if let Some(value) = data.get(key) {
            merged.insert(key.into(), value.clone());
        }
    }
    let automatic = merged["automatic"].as_bool().ok_or("Invalid automatic setting.")?;
    let layout = merged["layout"].as_str().filter(|l| *l == "system" || LAYOUTS.iter().any(|(k, _)| k == l))
        .ok_or("Unsupported layout.")?.to_string();
    let size = merged["size"].as_str().filter(|s| SIZES.contains(s)).ok_or("Invalid keyboard size.")?.to_string();
    let invalid = || "Invalid ignored keyboards.".to_string();
    let list = merged["ignore_keyboards"].as_array().ok_or_else(invalid)?;
    if list.len() > 128 {
        return Err(invalid());
    }
    let ignore_keyboards = list.iter()
        .map(|item| item.as_str().filter(|s| s.chars().count() <= 512).map(str::to_owned).ok_or_else(invalid))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Settings { automatic, layout, size, ignore_keyboards })
}

/// `$XDG_CONFIG_HOME/tessel-keyboard/config.json`.
pub fn default_path() -> PathBuf { tessel_core::paths::config_home().join("tessel-keyboard/config.json") }

#[derive(Clone, Debug)]
pub struct Config {
    pub path: PathBuf,
}

impl Default for Config {
    fn default() -> Self { Self { path: default_path() } }
}

impl Config {
    pub fn new(path: impl Into<PathBuf>) -> Self { Self { path: path.into() } }

    pub fn load(&self) -> Result<Settings, String> {
        match fs::read_to_string(&self.path) {
            Ok(text) => validate(&serde_json::from_str(&text).map_err(|e| e.to_string())?),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Settings::default()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Validate, then write `indent=2` JSON plus `\n`, mode 0600, via a
    /// `.keyboard-*` temporary file, fsync and rename. Invalid data leaves the file untouched.
    pub fn save(&self, data: &Value) -> Result<Settings, String> {
        let settings = validate(data)?;
        let directory = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let style = JsonStyle { ensure_ascii: false, indent: Some(2), item_separator: ",", ..JsonStyle::DEFAULT };
        let text = json_dumps(&settings.to_value(), &style) + "\n";
        let (temporary, mut file) = temp_file(directory, ".keyboard-")?;
        let result = (|| {
            file.write_all(text.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
            fs::rename(&temporary, &self.path)
        })();
        if temporary.exists() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|e| e.to_string())?;
        Ok(settings)
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<Settings, String> { self.save(&settings.to_value()) }
}

/// `tempfile.mkstemp(prefix=…, dir=…)`: O_EXCL, mode 0600.
fn temp_file(directory: &Path, prefix: &str) -> Result<(PathBuf, fs::File), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    for attempt in 0..100u128 {
        let name = format!("{prefix}{:x}{:x}", std::process::id(), seed.wrapping_add(attempt * 7919) & 0xffff_ffff);
        let path = directory.join(name);
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("No usable temporary file name".into())
}

/// `$XDG_CONFIG_HOME/labwc/environment`.
pub fn environment_path() -> PathBuf { tessel_core::paths::config_home().join("labwc/environment") }

/// Python `str.replace('_','').isalnum()` (non-empty, Unicode letters/digits) and `len < 40`.
pub fn valid_layout_name(item: &str) -> bool {
    let stripped: String = item.chars().filter(|c| *c != '_').collect();
    !stripped.is_empty() && stripped.chars().all(char::is_alphanumeric) && item.chars().count() < 40
}

/// Physical keyboard layouts from labwc's environment file (last
/// `XKB_DEFAULT_LAYOUT=` line, comma separated, as written by Global Settings),
/// else this session's variable, else `["us"]`.
pub fn system_layouts(path: Option<&Path>) -> Vec<String> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(environment_path);
    let mut value = None;
    if let Ok(text) = fs::read_to_string(&path) {
        for line in text.lines() {
            let mut line = line.trim();
            if let Some(rest) = line.strip_prefix("export ") {
                line = rest.trim_start();
            }
            if let Some(rest) = line.strip_prefix("XKB_DEFAULT_LAYOUT=") {
                value = Some(rest.trim().trim_matches(|c| c == '"' || c == '\'').to_string());
            }
        }
    }
    let value = value.unwrap_or_else(|| std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_default());
    let layouts: Vec<String> = value.split(',').map(str::trim).filter(|i| valid_layout_name(i)).map(str::to_owned).collect();
    if layouts.is_empty() { vec!["us".into()] } else { layouts }
}

pub fn layout_name(config: &Settings, path: Option<&Path>) -> String {
    if config.layout != "system" {
        return config.layout.clone();
    }
    system_layouts(path).swap_remove(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let directory = base.join(format!("tessel-keyboard-config-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    // test_policy.py: test_system_layout_follows_labwc_environment
    #[test]
    fn system_layout_follows_labwc_environment() {
        let folder = temp("env");
        let path = folder.join("environment");
        fs::write(&path, "XKB_DEFAULT_OPTIONS=grp:alt_shift_toggle\nXKB_DEFAULT_LAYOUT=de,us\n").unwrap();
        assert_eq!(system_layouts(Some(&path)), ["de", "us"]);
        assert_eq!(layout_name(&validate(&json!({})).unwrap(), Some(&path)), "de");
        assert_eq!(layout_name(&validate(&json!({"layout": "fr"})).unwrap(), Some(&path)), "fr");
        fs::write(&path, "XKB_DEFAULT_LAYOUT=\"gb\"\n").unwrap();
        assert_eq!(system_layouts(Some(&path)), ["gb"]);
        fs::write(&path, "XKB_DEFAULT_LAYOUT=$(evil)\n").unwrap();
        assert_eq!(system_layouts(Some(&path)), ["us"]);
        fs::write(&path, "export XKB_DEFAULT_LAYOUT='se, ,no'\nXKB_DEFAULT_LAYOUT=dk\n").unwrap();
        assert_eq!(system_layouts(Some(&path)), ["dk"]);
        fs::remove_dir_all(folder).unwrap();
    }

    // test_policy.py: test_config_atomic_roundtrip_and_validation
    #[test]
    fn config_atomic_roundtrip_and_validation() {
        let folder = temp("roundtrip");
        let store = Config::new(folder.join("settings.json"));
        let mut data = store.load().unwrap();
        data.layout = "de".into();
        data.size = "large".into();
        store.save_settings(&data).unwrap();
        assert_eq!(store.load().unwrap(), data);
        let text = fs::read_to_string(&store.path).unwrap();
        assert_eq!(text, "{\n  \"automatic\": true,\n  \"layout\": \"de\",\n  \"size\": \"large\",\n  \"ignore_keyboards\": []\n}\n");
        assert_eq!(fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
        let mut bad = data.to_value();
        bad["layout"] = json!("us;false");
        assert!(store.save(&bad).is_err());
        assert_eq!(store.load().unwrap(), data);
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1, "no temporary files left");
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn validation_rules() {
        assert_eq!(validate(&json!({"unknown": 1})).unwrap(), Settings::default());
        assert_eq!(validate(&json!([])).unwrap_err(), "Invalid keyboard settings.");
        assert_eq!(validate(&json!({"automatic": 1})).unwrap_err(), "Invalid automatic setting.");
        assert_eq!(validate(&json!({"layout": ["us"]})).unwrap_err(), "Unsupported layout.");
        assert_eq!(validate(&json!({"size": "huge"})).unwrap_err(), "Invalid keyboard size.");
        assert!(validate(&json!({"ignore_keyboards": [1]})).is_err());
        assert!(validate(&json!({"ignore_keyboards": ["x".repeat(513)]})).is_err());
        assert!(validate(&json!({"ignore_keyboards": vec!["a"; 129]})).is_err());
        assert!(validate(&json!({"ignore_keyboards": vec!["é".repeat(512); 128]})).is_ok());
        let unicode = json!({"ignore_keyboards": ["Clavier é"]});
        let folder = temp("unicode");
        let store = Config::new(folder.join("c.json"));
        store.save(&unicode).unwrap();
        assert!(fs::read_to_string(&store.path).unwrap().contains("Clavier é"));
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn layout_names() {
        assert!(valid_layout_name("us_intl"));
        assert!(!valid_layout_name("___"));
        assert!(!valid_layout_name("a-b"));
        assert!(!valid_layout_name(&"a".repeat(40)));
        assert!(valid_layout_name(&"a".repeat(39)));
    }
}
