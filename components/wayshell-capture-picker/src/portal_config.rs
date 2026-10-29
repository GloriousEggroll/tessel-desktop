//! Configure only the wlroots/labwc backend's chooser, retaining other options
//! (capture_portal_config.py).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const DESKTOPS: [&str; 2] = ["wlroots", "labwc"];

/// Python's `\s` for `str` patterns.
const PY_SPACE: &str = r"[\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}]";

fn header() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(&format!(r"^{s}*\[([^\]]+)\]{s}*(?:[;#].*)?$", s = PY_SPACE)).unwrap())
}

fn assignment() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(&format!(r"^{s}*(chooser_type|chooser_cmd){s}*=", s = PY_SPACE)).unwrap())
}

/// `str.splitlines(keepends=True)`.
pub fn splitlines_keepends(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let end = match ch {
            '\r' => match chars.peek() {
                Some(&(next, '\n')) => {
                    chars.next();
                    next + 1
                }
                _ => index + 1,
            },
            '\n' | '\x0B' | '\x0C' | '\x1C' | '\x1D' | '\x1E' | '\u{85}' | '\u{2028}' | '\u{2029}' => index + ch.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLauncher;

impl std::fmt::Display for InvalidLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Invalid capture picker launcher path.") }
}

impl std::error::Error for InvalidLauncher {}

pub fn chooser_config(original: &str, launcher: &Path) -> Result<String, InvalidLauncher> {
    let launcher = launcher.to_string_lossy();
    if launcher.contains('\n') || launcher.contains('\r') {
        return Err(InvalidLauncher);
    }
    let values = [("chooser_type", "dmenu".to_owned()), ("chooser_cmd", format!("exec {}", suite_core::shell::shell_quote(&launcher)))];
    let value = |name: &str| values.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str()).unwrap_or("");
    let mut section = String::new();
    let mut found: Vec<&str> = Vec::new();
    let mut output = String::with_capacity(original.len() + 128);
    for line in splitlines_keepends(original) {
        let stripped = line.trim_end_matches(['\r', '\n']);
        if let Some(captures) = header().captures(stripped) {
            section = captures[1].to_owned();
        }
        let assigned = assignment().captures(line).map(|c| c.get(1).unwrap().as_str());
        match assigned {
            Some(name) if section == "screencast" => {
                let name = if name == "chooser_type" { "chooser_type" } else { "chooser_cmd" };
                output.push_str(name);
                output.push('=');
                output.push_str(value(name));
                output.push('\n');
                if !found.contains(&name) {
                    found.push(name);
                }
            }
            _ => output.push_str(line),
        }
    }
    let missing: Vec<&str> = values.iter().map(|(n, _)| *n).filter(|n| !found.contains(n)).collect();
    if !missing.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        // Repeated INI sections are supported by xdpw/inih. Keeping a trailing
        // section avoids moving comments or rewriting unrelated settings.
        if section != "screencast" {
            output.push_str(if output.is_empty() { "[screencast]\n" } else { "\n[screencast]\n" });
        }
        for name in missing {
            output.push_str(name);
            output.push('=');
            output.push_str(value(name));
            output.push('\n');
        }
    }
    Ok(output)
}

#[derive(Debug)]
pub enum ConfigError {
    Launcher(InvalidLauncher),
    Io(PathBuf, std::io::Error),
    Unicode(PathBuf),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Launcher(error) => error.fmt(f),
            ConfigError::Io(path, error) => f.write_str(&suite_core::pyerr::os_error_path(error, path)),
            ConfigError::Unicode(path) => write!(f, "'utf-8' codec can't decode {}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// One planned file: (path, content, mode).
pub type Planned = (PathBuf, Vec<u8>, u32);

/// The effective configuration per desktop (local/desktop, local/config,
/// system/desktop, system/config), with only the chooser overridden.
pub fn configuration_files(config_home: &Path, launcher: &Path, system_root: &Path) -> Result<Vec<Planned>, ConfigError> {
    let local = config_home.join("xdg-desktop-portal-wlr");
    let system = system_root.join("xdg-desktop-portal-wlr");
    let mut result = Vec::new();
    for desktop in DESKTOPS {
        // Match backend precedence, including a user generic config taking
        // priority over a system desktop-specific config. Clone inherited
        // values before overriding the chooser so fps/hooks/buffer flags stay.
        let candidates = [local.join(desktop), local.join("config"), system.join(desktop), system.join("config")];
        let existing = candidates.iter().find(|path| path.is_file());
        let before = match existing {
            Some(path) => {
                let bytes = std::fs::read(path).map_err(|e| ConfigError::Io(path.clone(), e))?;
                String::from_utf8(bytes).map_err(|_| ConfigError::Unicode(path.clone()))?
            }
            None => String::new(),
        };
        let data = chooser_config(&before, launcher).map_err(ConfigError::Launcher)?;
        result.push((local.join(desktop), data.into_bytes(), 0o644));
    }
    Ok(result)
}
