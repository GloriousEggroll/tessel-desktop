//! login-manager/login_model.py: system-wide greeter appearance and the
//! installed Wayland desktop sessions. Shared by login-manager and Global
//! Settings (moved from gs-desktop).
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use suite_core::pyjson::casefold;

pub const CONFIG: &str = "/etc/waylogin/appearance.json";
pub const PLACEMENTS: [&str; 4] = ["fill", "fit", "stretch", "center"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub picture: String,
    pub placement: String,
    pub background_color: String,
    pub blur_strength: i64,
    pub transparency: i64,
    pub idle_seconds: i64,
    pub clock_24h: bool,
    pub default_session: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            picture: "/usr/share/wallpapers/nobara-winds1.webp".into(), placement: "fill".into(),
            background_color: "#161820".into(), blur_strength: 50, transparency: 5, idle_seconds: 30, clock_24h: false,
            default_session: "labwc".into(),
        }
    }
}

const FIELDS: [&str; 8] = ["picture", "placement", "background_color", "blur_strength", "transparency", "idle_seconds",
                           "clock_24h", "default_session"];

fn is_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

impl Appearance {
    /// `asdict(appearance)` in field order.
    pub fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("picture".into(), self.picture.clone().into());
        map.insert("placement".into(), self.placement.clone().into());
        map.insert("background_color".into(), self.background_color.clone().into());
        map.insert("blur_strength".into(), self.blur_strength.into());
        map.insert("transparency".into(), self.transparency.into());
        map.insert("idle_seconds".into(), self.idle_seconds.into());
        map.insert("clock_24h".into(), self.clock_24h.into());
        map.insert("default_session".into(), self.default_session.clone().into());
        map
    }

    /// `Appearance.load(path=CONFIG)`.
    pub fn load(path: Option<&Path>) -> Result<Appearance, String> {
        let path = path.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(CONFIG));
        if !path.exists() {
            return Ok(Appearance::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| suite_core::pyerr::os_error_path(&e, &path))?;
        let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Appearance::from_dict(&value)
    }

    /// `Appearance.from_dict(data)`: unknown keys are ignored, values validated.
    pub fn from_dict(data: &Value) -> Result<Appearance, String> {
        let data = data.as_object().ok_or("Appearance settings must be an object")?;
        let mut values = Appearance::default().to_map();
        for key in FIELDS {
            if let Some(value) = data.get(key) {
                values.insert(key.into(), value.clone());
            }
        }
        for (key, low, high) in [("blur_strength", 0, 100), ("transparency", 0, 100), ("idle_seconds", 10, 3600)] {
            if !is_int(&values[key]) || !values[key].as_i64().is_some_and(|n| (low..=high).contains(&n)) {
                return Err(format!("Invalid {key}"));
            }
        }
        if !values["placement"].as_str().is_some_and(|p| PLACEMENTS.contains(&p)) {
            return Err("Invalid wallpaper placement".into());
        }
        match values["picture"].as_str() {
            Some(picture) if picture.is_empty() || picture.starts_with('/') => {}
            _ => return Err("Wallpaper must be an absolute, system-readable path".into()),
        }
        if !values["clock_24h"].is_boolean() || !values["default_session"].is_string() {
            return Err("Invalid clock or session setting".into());
        }
        let color: Vec<char> = values["background_color"].as_str().unwrap_or("").chars().collect();
        if color.len() != 7 || color[0] != '#' || color[1..].iter().any(|c| !c.is_ascii_hexdigit()) {
            return Err("Invalid background color".into());
        }
        let text = |key: &str| values[key].as_str().unwrap_or_default().to_owned();
        Ok(Appearance {
            picture: text("picture"), placement: text("placement"), background_color: text("background_color"),
            blur_strength: values["blur_strength"].as_i64().unwrap(), transparency: values["transparency"].as_i64().unwrap(),
            idle_seconds: values["idle_seconds"].as_i64().unwrap(), clock_24h: values["clock_24h"].as_bool().unwrap(),
            default_session: text("default_session"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub ident: String,
    pub name: String,
    pub argv: Vec<String>,
    pub desktops: String,
}

/// POSIX `shlex.split` (no comments).
pub fn shlex_split(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some('"') | Some('\\') | Some('$') | Some('`') | Some('\n') => word.push(chars.next().unwrap()),
                            _ => word.push('\\'),
                        },
                        Some(c) => word.push(c),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err("No escaped character".into()),
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// `shutil.which(name)`.
pub fn which(name: &str) -> Option<PathBuf> {
    fn executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0)
    }
    if name.contains('/') {
        return executable(Path::new(name)).then(|| PathBuf::from(name));
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).map(|dir| dir.join(name)).find(|path| executable(path))
}

/// `configparser` (interpolation=None, strict=False) `[Desktop Entry]` section.
fn desktop_entry(text: &str) -> Result<Option<Vec<(String, String)>>, String> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut last_key: Option<String> = None;
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') || stripped.starts_with(';') {
            last_key = None;
            continue;
        }
        let indented = line.starts_with([' ', '\t']);
        if indented && last_key.is_some() && !sections.is_empty() {
            let key = last_key.clone().unwrap();
            if let Some(entry) = sections.last_mut().unwrap().1.iter_mut().rev().find(|(k, _)| *k == key) {
                entry.1.push('\n');
                entry.1.push_str(stripped);
            }
            continue;
        }
        if stripped.starts_with('[') && stripped.ends_with(']') {
            let name = stripped[1..stripped.len() - 1].to_owned();
            if !sections.iter().any(|(n, _)| *n == name) {
                sections.push((name.clone(), Vec::new()));
            } else {
                let index = sections.iter().position(|(n, _)| *n == name).unwrap();
                let section = sections.remove(index);
                sections.push(section);
            }
            last_key = None;
            continue;
        }
        let Some(section) = sections.last_mut() else { return Err("File contains no section headers.".into()) };
        let Some(at) = stripped.find(['=', ':']) else { return Err("Source contains parsing errors".into()) };
        let key = stripped[..at].trim().to_lowercase();
        let value = stripped[at + 1..].trim().to_owned();
        section.1.retain(|(k, _)| *k != key);
        section.1.push((key.clone(), value));
        last_key = Some(key);
    }
    Ok(sections.into_iter().find(|(name, _)| name == "Desktop Entry").map(|(_, entries)| entries))
}

/// `entry.getboolean(key, fallback=False)`.
fn boolean(value: Option<&String>) -> Result<bool, String> {
    match value.map(|v| v.to_lowercase()) {
        None => Ok(false),
        Some(v) if ["1", "yes", "true", "on"].contains(&v.as_str()) => Ok(true),
        Some(v) if ["0", "no", "false", "off"].contains(&v.as_str()) => Ok(false),
        Some(v) => Err(format!("Not a boolean: {v}")),
    }
}

fn session_from(path: &Path, stem: &str) -> Result<Option<Session>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let Some(entry) = desktop_entry(&text)? else { return Err("Desktop Entry".into()) };
    let get = |key: &str| entry.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    if boolean(get("hidden").as_ref())? || boolean(get("nodisplay").as_ref())? {
        return Ok(None);
    }
    if get("type").unwrap_or_else(|| "Application".into()) != "Application" {
        return Ok(None);
    }
    let exec = get("exec").ok_or("Exec")?;
    let argv = shlex_split(&exec)?;
    // Session entries must be launchable without file/URL parameters.
    if argv.is_empty() || argv.iter().any(|part| part.replace("%%", "").contains('%')) {
        return Ok(None);
    }
    let argv: Vec<String> = argv.into_iter().map(|part| part.replace("%%", "%")).collect();
    let try_exec = get("tryexec").unwrap_or_else(|| argv[0].clone());
    if which(&argv[0]).is_none() || which(&try_exec).is_none() {
        return Ok(None);
    }
    let desktops = get("desktopnames").unwrap_or_else(|| stem.to_owned());
    let desktops = desktops.split(';').filter(|d| !d.is_empty()).collect::<Vec<_>>().join(":");
    Ok(Some(Session { ident: stem.into(), name: get("name").unwrap_or_else(|| stem.into()), argv, desktops }))
}

pub fn default_session_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/usr/local/share/wayland-sessions"), PathBuf::from("/usr/share/wayland-sessions")]
}

/// `sessions(roots)`: launchable Wayland sessions, first root wins per id.
pub fn sessions(roots: &[PathBuf]) -> Vec<Session> {
    let mut result = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for root in roots {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(root).map(|entries| entries.filter_map(|e| e.ok()).map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "desktop")).collect()).unwrap_or_default();
        paths.sort();
        for path in paths {
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if seen.contains(&stem) {
                continue;
            }
            seen.push(stem.clone());
            if let Ok(Some(session)) = session_from(&path, &stem) {
                result.push(session);
            }
        }
    }
    result.sort_by_key(|item| casefold(&item.name));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn appearance_validation_matches_python_messages() {
        assert_eq!(Appearance::from_dict(&json!({})).unwrap(), Appearance::default());
        assert_eq!(Appearance::from_dict(&json!({"picture": "", "extra": 1})).unwrap().picture, "");
        for (data, message) in [
            (json!([]), "Appearance settings must be an object"),
            (json!({"blur_strength": -1}), "Invalid blur_strength"),
            (json!({"transparency": true}), "Invalid transparency"),
            (json!({"idle_seconds": 0}), "Invalid idle_seconds"),
            (json!({"placement": "other"}), "Invalid wallpaper placement"),
            (json!({"picture": "relative.png"}), "Wallpaper must be an absolute, system-readable path"),
            (json!({"clock_24h": "true"}), "Invalid clock or session setting"),
            (json!({"default_session": 3}), "Invalid clock or session setting"),
            (json!({"background_color": "url(file:///etc/passwd)"}), "Invalid background color"),
        ] {
            assert_eq!(Appearance::from_dict(&data).unwrap_err(), message, "{data}");
        }
    }

    #[test]
    fn sessions_follow_python_rules() {
        let root = std::env::temp_dir().join(format!("gs-login-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("labwc.desktop"), "[Desktop Entry]\nName=labwc\nExec=sh -c 'labwc'\nDesktopNames=wlroots;labwc;\n").unwrap();
        std::fs::write(b.join("labwc.desktop"), "[Desktop Entry]\nName=Shadowed\nExec=sh\n").unwrap();
        std::fs::write(a.join("files.desktop"), "[Desktop Entry]\nName=Files\nExec=sh %f\n").unwrap();
        std::fs::write(a.join("hidden.desktop"), "[Desktop Entry]\nName=Hidden\nExec=sh\nHidden=true\n").unwrap();
        std::fs::write(a.join("missing.desktop"), "[Desktop Entry]\nName=Missing\nExec=/nonexistent/bin\n").unwrap();
        std::fs::write(b.join("alpha.desktop"), "[Desktop Entry]\nName=alpha\nExec=sh 100%%\nTryExec=sh\n").unwrap();
        let found = sessions(&[a.clone(), b.clone()]);
        assert_eq!(found.iter().map(|s| s.ident.as_str()).collect::<Vec<_>>(), ["alpha", "labwc"]);
        assert_eq!(found[1].argv, ["sh", "-c", "labwc"]);
        assert_eq!(found[1].desktops, "wlroots:labwc");
        assert_eq!(found[0].argv, ["sh", "100%"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shlex_matches_python() {
        assert_eq!(shlex_split(r#"a "b c" 'd e' f\ g "h\"i""#).unwrap(), ["a", "b c", "d e", "f g", "h\"i"]);
        assert!(shlex_split("'open").is_err());
    }
}
