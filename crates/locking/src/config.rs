//! tessel-lock/lock_config.py: tessel-lock appearance. Idle policy remains
//! owned by Power management. Shared by tessel-lock, tessel-login,
//! home-assistant-screen and Global Settings (moved from gs-desktop).
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const IMAGE_EXTENSIONS: [&str; 8] = [".png", ".jpg", ".jpeg", ".webp", ".bmp", ".tif", ".tiff", ".avif"];
pub const WALLPAPER_MODES: [&str; 4] = ["desktop", "picture", "slideshow", "color"];
pub const PLACEMENTS: [&str; 4] = ["fill", "fit", "stretch", "center"];

pub fn config_home() -> PathBuf {
    tessel_core::paths::config_home()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockConfig {
    pub wallpaper_mode: String,
    pub picture: String,
    pub slideshow: Vec<String>,
    pub interval: i64,
    pub placement: String,
    pub background_color: String,
    pub follow_appearance: bool,
    pub transparency_enabled: bool,
    pub transparency: i64,
    pub blur: bool,
    pub blur_strength: i64,
    pub show_assistant_name: bool,
    pub show_voice: bool,
    pub assistant_name: String,
    pub voice_log: String,
    pub show_weather: bool,
    pub show_forecast: bool,
    pub pause_media: bool,
    /// Media controls appear only while something is playing or paused.
    pub show_media: bool,
    pub show_battery: bool,
    /// Count only ("3 new notifications"); names are opt-in, content never shown.
    pub show_notifications: bool,
    pub notification_app_names: bool,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            wallpaper_mode: "desktop".into(), picture: String::new(), slideshow: Vec::new(), interval: 300,
            placement: "fill".into(), background_color: "#161820".into(), follow_appearance: true,
            transparency_enabled: true, transparency: 5, blur: true, blur_strength: 50, show_assistant_name: true,
            show_voice: true, assistant_name: "Kasumi".into(), voice_log: "~/.hermes/logs/agent.log".into(),
            show_weather: false, show_forecast: false, pause_media: false, show_media: true, show_battery: true,
            show_notifications: true, notification_app_names: false,
        }
    }
}

const BOOL_FIELDS: [&str; 12] = ["follow_appearance", "transparency_enabled", "blur", "show_assistant_name", "show_voice",
    "show_weather", "show_forecast", "pause_media", "show_media", "show_battery", "show_notifications", "notification_app_names"];

fn is_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

fn text_ok(value: &str) -> bool {
    value.chars().count() <= 4096 && !value.contains('\0')
}

impl LockConfig {
    /// `dataclasses.asdict` (field order).
    pub fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("wallpaper_mode".into(), self.wallpaper_mode.clone().into());
        map.insert("picture".into(), self.picture.clone().into());
        map.insert("slideshow".into(), self.slideshow.clone().into());
        map.insert("interval".into(), self.interval.into());
        map.insert("placement".into(), self.placement.clone().into());
        map.insert("background_color".into(), self.background_color.clone().into());
        map.insert("follow_appearance".into(), self.follow_appearance.into());
        map.insert("transparency_enabled".into(), self.transparency_enabled.into());
        map.insert("transparency".into(), self.transparency.into());
        map.insert("blur".into(), self.blur.into());
        map.insert("blur_strength".into(), self.blur_strength.into());
        map.insert("show_assistant_name".into(), self.show_assistant_name.into());
        map.insert("show_voice".into(), self.show_voice.into());
        map.insert("assistant_name".into(), self.assistant_name.clone().into());
        map.insert("voice_log".into(), self.voice_log.clone().into());
        for key in ["show_weather", "show_forecast", "pause_media", "show_media", "show_battery", "show_notifications",
                    "notification_app_names"] {
            map.insert(key.into(), self.flag(key).into());
        }
        map
    }

    fn flag(&self, key: &str) -> bool {
        match key {
            "follow_appearance" => self.follow_appearance,
            "transparency_enabled" => self.transparency_enabled,
            "blur" => self.blur,
            "show_assistant_name" => self.show_assistant_name,
            "show_voice" => self.show_voice,
            "show_weather" => self.show_weather,
            "show_forecast" => self.show_forecast,
            "pause_media" => self.pause_media,
            "show_media" => self.show_media,
            "show_battery" => self.show_battery,
            "show_notifications" => self.show_notifications,
            "notification_app_names" => self.notification_app_names,
            _ => false,
        }
    }

    /// `LockConfig.parse(values)`.
    pub fn parse(values: &Value) -> Result<LockConfig, String> {
        let values = values.as_object().ok_or("Lock screen settings must be an object.")?;
        let defaults = LockConfig::default().to_map();
        let clean: Map<String, Value> = defaults.iter()
            .map(|(key, value)| (key.clone(), values.get(key).cloned().unwrap_or_else(|| value.clone()))).collect();
        let text = |key: &str| clean[key].as_str().map(str::to_owned);
        if !clean["wallpaper_mode"].as_str().is_some_and(|m| WALLPAPER_MODES.contains(&m)) {
            return Err("Choose a supported wallpaper mode.".into());
        }
        if !clean["placement"].as_str().is_some_and(|m| PLACEMENTS.contains(&m)) {
            return Err("Choose a supported wallpaper placement.".into());
        }
        if BOOL_FIELDS.iter().any(|key| !clean[*key].is_boolean()) {
            return Err("Invalid lock screen toggle.".into());
        }
        for (key, lower, upper) in [("interval", 30, 86400), ("transparency", 0, 100), ("blur_strength", 0, 100)] {
            if !is_int(&clean[key]) || !clean[key].as_i64().is_some_and(|n| (lower..=upper).contains(&n)) {
                return Err(format!("Invalid lock screen {key}."));
            }
        }
        for key in ["picture", "assistant_name", "voice_log", "background_color"] {
            if !text(key).is_some_and(|t| text_ok(&t)) {
                return Err("Invalid lock screen text.".into());
            }
        }
        let color = text("background_color").unwrap_or_default();
        let chars: Vec<char> = color.chars().collect();
        if chars.len() != 7 || chars[0] != '#' || chars[1..].iter().any(|c| !c.is_ascii_hexdigit()) {
            return Err("Background color must be a six-digit hex color, such as #161820.".into());
        }
        let slides = clean["slideshow"].as_array().filter(|s| s.len() <= 500)
            .ok_or("Choose at most 500 slideshow pictures.")?;
        let mut slideshow: Vec<String> = Vec::new();
        for path in slides {
            let path = path.as_str().filter(|p| text_ok(p)).ok_or("Invalid slideshow picture.")?;
            if !slideshow.iter().any(|p| p == path) {
                slideshow.push(path.to_owned());
            }
        }
        let flag = |key: &str| clean[key].as_bool().unwrap_or(false);
        Ok(LockConfig {
            wallpaper_mode: text("wallpaper_mode").unwrap(), picture: text("picture").unwrap(), slideshow,
            interval: clean["interval"].as_i64().unwrap(), placement: text("placement").unwrap(),
            background_color: color, follow_appearance: flag("follow_appearance"),
            transparency_enabled: flag("transparency_enabled"), transparency: clean["transparency"].as_i64().unwrap(),
            blur: flag("blur"), blur_strength: clean["blur_strength"].as_i64().unwrap(),
            show_assistant_name: flag("show_assistant_name"), show_voice: flag("show_voice"),
            assistant_name: text("assistant_name").unwrap(), voice_log: text("voice_log").unwrap(),
            show_weather: flag("show_weather"), show_forecast: flag("show_forecast"), pause_media: flag("pause_media"),
            show_media: flag("show_media"), show_battery: flag("show_battery"),
            show_notifications: flag("show_notifications"), notification_app_names: flag("notification_app_names"),
        })
    }
}

/// `json.dump(asdict(config), indent=2)` + newline.
pub fn config_bytes(config: &LockConfig) -> String {
    let style = JsonStyle { indent: Some(2), ..JsonStyle::DEFAULT };
    json_dumps(&Value::Object(config.to_map()), &style) + "\n"
}

pub struct ConfigStore {
    pub path: PathBuf,
}

impl Default for ConfigStore {
    fn default() -> Self { Self::new(None) }
}

impl ConfigStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path: path.unwrap_or_else(|| tessel_core::paths::suite_config_dir().join("lock-screen.json")) }
    }

    pub fn load(&self) -> Result<LockConfig, String> {
        if !self.path.exists() {
            return Ok(LockConfig::default());
        }
        let data = std::fs::read(&self.path).map_err(|e| tessel_core::pyerr::os_error_path(&e, &self.path))?;
        let text = String::from_utf8(data).map_err(|e| e.to_string())?;
        if text.chars().count() > 1024 * 1024 {
            return Err("Lock screen configuration is too large.".into());
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        LockConfig::parse(&value)
    }

    /// `update(**changes)`: validate the merged settings and replace the file atomically.
    pub fn update(&self, changes: &Map<String, Value>) -> Result<LockConfig, String> {
        let mut values = self.load()?.to_map();
        for (key, value) in changes {
            if !values.contains_key(key) {
                return Err(format!("LockConfig.__init__() got an unexpected keyword argument '{key}'"));
            }
            values.insert(key.clone(), value.clone());
        }
        let config = LockConfig::parse(&Value::Object(values))?;
        let parent = self.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        std::fs::create_dir_all(&parent).map_err(|e| tessel_core::pyerr::os_error_path(&e, &parent))?;
        // mkstemp (0600) + fsync + os.replace; the mode stays 0600 as in Python.
        tessel_core::clock::atomic_write(&self.path, config_bytes(&config).as_bytes(), ".lock-screen-")
            .map_err(|e| tessel_core::pyerr::os_error_path(&e, &self.path))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        Ok(config)
    }
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| {
        let path = dir.join(name);
        std::fs::metadata(&path).is_ok_and(|m| m.is_file() && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0)
    }))
}

/// `importlib.util.find_spec('homeassistant')` approximated by looking for the
/// package in the system and user Python site directories.
fn python_package(name: &str) -> bool {
    let home = tessel_core::paths::home();
    let mut roots = vec![PathBuf::from("/usr/lib"), PathBuf::from("/usr/lib64"), PathBuf::from("/usr/local/lib"),
                         PathBuf::from("/usr/local/lib64"), home.join(".local/lib")];
    roots.retain(|r| r.is_dir());
    roots.iter().any(|root| std::fs::read_dir(root).map(|entries| entries.filter_map(|e| e.ok()).any(|entry| {
        let file = entry.file_name().to_string_lossy().into_owned();
        file.starts_with("python3") && ["site-packages", "dist-packages"].iter().any(|site| {
            let package = entry.path().join(site).join(name);
            package.join("__init__.py").is_file() || package.with_extension("py").is_file()
        })
    })).unwrap_or(false))
}

/// The user's Home Assistant is the Kasumi/Makima voice installation.
pub fn home_assistant_installed() -> bool {
    let roots = [config_home().join("systemd/user"), PathBuf::from("/usr/lib/systemd/user"),
                 PathBuf::from("/etc/systemd/system"), PathBuf::from("/usr/lib/systemd/system")];
    let units = ["makima-voice.service", "home-assistant.service", "homeassistant.service", "hass.service"];
    if roots.iter().any(|root| units.iter().any(|unit| root.join(unit).exists())) {
        return true;
    }
    which("hass") || python_package("homeassistant")
}

/// `desktop_wallpaper(monitor)`: the monitor's still-image wallpaper.
pub fn desktop_wallpaper(monitor: &str) -> Option<PathBuf> {
    let path = tessel_core::paths::component_config_dir("shell").join("wallpapers.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // No wallpaper was ever chosen: every screen shows the default one.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return tessel_core::wallpaper::default_wallpaper(None),
        Err(_) => return None,
    };
    let data: Value = serde_json::from_str(&text).ok()?;
    let Some(entry) = data.as_object()?.get(monitor).cloned() else { return tessel_core::wallpaper::default_wallpaper(None) };
    let file = entry.as_object()?.get("file_path").and_then(Value::as_str).unwrap_or("").to_owned();
    let path = if let Some(rest) = file.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') { tessel_core::paths::home().join(rest.trim_start_matches('/')) } else { PathBuf::from(&file) }
    } else {
        PathBuf::from(&file)
    };
    let suffix = path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
    (IMAGE_EXTENSIONS.contains(&suffix.as_str()) && path.is_file()).then_some(path)
}

/// `appearance(config, palette)` → (transparency, blur strength).
pub fn appearance(config: &LockConfig, palette: Option<&Map<String, Value>>) -> (i64, i64) {
    if let Some(palette) = palette.filter(|p| config.follow_appearance && !p.is_empty()) {
        let truthy = |key: &str| palette.get(key).is_some_and(tessel_core::pyjson::truthy);
        let number = |key: &str, default: i64| palette.get(key).and_then(Value::as_i64).unwrap_or(default);
        return (if truthy("apps_transparency_enabled") { number("apps_transparency", 5) } else { 0 },
                if truthy("apps_frosted") { number("apps_frost_strength", 50) } else { 0 });
    }
    (if config.transparency_enabled { config.transparency } else { 0 }, if config.blur { config.blur_strength } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_defaults_validation_and_dedupe() {
        assert_eq!(LockConfig::parse(&json!({})).unwrap(), LockConfig::default());
        let config = LockConfig::parse(&json!({"slideshow": ["/a", "/b", "/a"], "interval": 60})).unwrap();
        assert_eq!(config.slideshow, vec!["/a", "/b"]);
        for (value, message) in [
            (json!([]), "Lock screen settings must be an object."),
            (json!({"wallpaper_mode": "video"}), "Choose a supported wallpaper mode."),
            (json!({"placement": "tile"}), "Choose a supported wallpaper placement."),
            (json!({"blur": 1}), "Invalid lock screen toggle."),
            (json!({"interval": 10}), "Invalid lock screen interval."),
            (json!({"transparency": true}), "Invalid lock screen transparency."),
            (json!({"blur_strength": 50.0}), "Invalid lock screen blur_strength."),
            (json!({"picture": 3}), "Invalid lock screen text."),
            (json!({"background_color": "#12345"}), "Background color must be a six-digit hex color, such as #161820."),
            (json!({"slideshow": "x"}), "Choose at most 500 slideshow pictures."),
            (json!({"slideshow": [1]}), "Invalid slideshow picture."),
        ] {
            assert_eq!(LockConfig::parse(&value).unwrap_err(), message, "{value}");
        }
    }

    #[test]
    fn store_writes_python_bytes() {
        let dir = std::env::temp_dir().join(format!("gs-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ConfigStore::new(Some(dir.join("tessel/lock-screen.json")));
        assert_eq!(store.load().unwrap(), LockConfig::default());
        let mut changes = Map::new();
        changes.insert("slideshow".into(), json!(["/x.png"]));
        changes.insert("assistant_name".into(), json!("Ünï"));
        store.update(&changes).unwrap();
        let text = std::fs::read_to_string(&store.path).unwrap();
        assert!(text.starts_with("{\n  \"wallpaper_mode\": \"desktop\",\n  \"picture\": \"\",\n  \"slideshow\": [\n    \"/x.png\"\n  ],\n"));
        assert!(text.contains("\"assistant_name\": \"\\u00dcn\\u00ef\""));
        assert!(text.ends_with("\"notification_app_names\": false\n}\n"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(store.load().unwrap().assistant_name, "Ünï");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appearance_follows_palette_or_own_values() {
        let config = LockConfig::default();
        let palette = json!({"apps_transparency_enabled": true, "apps_transparency": 20, "apps_frosted": false});
        assert_eq!(appearance(&config, palette.as_object()), (20, 0));
        assert_eq!(appearance(&LockConfig { follow_appearance: false, ..config.clone() }, palette.as_object()), (5, 50));
        assert_eq!(appearance(&config, None), (5, 50));
    }
}
