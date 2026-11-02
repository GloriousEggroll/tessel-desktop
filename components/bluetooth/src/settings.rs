//! `${XDG_CONFIG_HOME:-~/.config}/tessel-bluetooth/settings.ini`: `[receive] mode =
//! off|ask` (`load_receive_mode` / `save_receive_mode`), with a small port of
//! the `configparser.ConfigParser(interpolation=None)` reader/writer so other
//! sections survive a save byte-for-byte as Python would rewrite them.

use std::path::{Path, PathBuf};

pub const RECEIVE_MODES: [&str; 2] = ["off", "ask"];
pub const DEFAULT_RECEIVE_MODE: &str = "ask";

/// Python reads `XDG_CONFIG_HOME` even when empty (`os.environ.get`).
pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir).join("tessel-bluetooth"),
        None => tessel_core::paths::home().join(".config").join("tessel-bluetooth"),
    }
}

pub fn config_file() -> PathBuf {
    config_dir().join("settings.ini")
}

/// One section: `(name, [(key, value)])`; keys are lower-cased (`optionxform`).
pub type Section = (String, Vec<(String, String)>);

#[derive(Debug, PartialEq)]
pub struct ParseError(pub String);

/// `ConfigParser.read_string` semantics for what tessel-bluetooth needs: sections,
/// `key = value` / `key: value`, `#`/`;` comment lines, indented continuation
/// lines, strict duplicate detection and `MissingSectionHeaderError`.
pub fn parse(text: &str) -> Result<Vec<Section>, ParseError> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<usize> = None;
    let mut last_key: Option<usize> = None;
    let mut indent_of_key = 0usize;
    for (number, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let stripped = line.trim();
        let indent = line.len() - line.trim_start().len();
        if stripped.is_empty() {
            // Blank lines end a value (empty_lines_in_values keeps them, but
            // trailing blank lines are stripped; tessel-bluetooth values are one line).
            last_key = None;
            continue;
        }
        if stripped.starts_with('#') || stripped.starts_with(';') {
            continue;
        }
        if let (Some(section), Some(key)) = (current, last_key) {
            if indent > indent_of_key {
                let value = &mut sections[section].1[key].1;
                value.push('\n');
                value.push_str(stripped);
                continue;
            }
        }
        if stripped.len() > 2 && stripped.starts_with('[') && stripped.ends_with(']') {
            let name = stripped[1..stripped.len() - 1].to_owned();
            if sections.iter().any(|(n, _)| *n == name) {
                return Err(ParseError(format!("While reading from '<string>' [line {:2}]: section '{name}' already exists", number + 1)));
            }
            sections.push((name, Vec::new()));
            current = Some(sections.len() - 1);
            last_key = None;
            continue;
        }
        let Some(section) = current else {
            return Err(ParseError(format!("File contains no section headers.\nfile: '<string>', line: {}\n{:?}", number + 1, raw)));
        };
        let split = line.find(['=', ':']);
        let Some(at) = split else {
            return Err(ParseError(format!("Source contains parsing errors: '<string>'\n\t[line {:2}]: {:?}", number + 1, raw)));
        };
        let key = line[..at].trim().to_lowercase();
        let value = line[at + 1..].trim().to_owned();
        if key.is_empty() {
            return Err(ParseError(format!("Source contains parsing errors: '<string>'\n\t[line {:2}]: {:?}", number + 1, raw)));
        }
        if sections[section].1.iter().any(|(k, _)| *k == key) {
            return Err(ParseError(format!("While reading from '<string>' [line {:2}]: option '{key}' in section '{}' already exists",
                                          number + 1, sections[section].0)));
        }
        sections[section].1.push((key, value));
        last_key = Some(sections[section].1.len() - 1);
        indent_of_key = indent;
    }
    Ok(sections)
}

/// `ConfigParser.write`: `[name]`, `key = value` (continuation lines indented
/// with a tab), a blank line after every section; `DEFAULT` first.
pub fn write(sections: &[Section]) -> String {
    let mut out = String::new();
    let ordered = sections.iter().filter(|(n, _)| n == "DEFAULT").chain(sections.iter().filter(|(n, _)| n != "DEFAULT"));
    for (name, items) in ordered {
        out.push_str(&format!("[{name}]\n"));
        for (key, value) in items {
            let value = value.replace('\n', "\n\t");
            out.push_str(&format!("{key} = {value}\n"));
        }
        out.push('\n');
    }
    out
}

fn read_text(path: &Path) -> Option<Result<String, ()>> {
    match std::fs::read(path) {
        // configparser.read() silently skips files it cannot open.
        Err(_) => None,
        Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| ())),
    }
}

/// `load_receive_mode(path)`: "off" or "ask" (default on any problem).
pub fn load_receive_mode(path: Option<&Path>) -> String {
    let path = path.map(Path::to_path_buf).unwrap_or_else(config_file);
    let sections = match read_text(&path) {
        None => Vec::new(),
        Some(Err(())) => return DEFAULT_RECEIVE_MODE.into(),
        Some(Ok(text)) => match parse(&text) {
            Ok(sections) => sections,
            Err(_) => return DEFAULT_RECEIVE_MODE.into(),
        },
    };
    let mode = sections.iter().find(|(n, _)| n == "receive")
        .and_then(|(_, items)| items.iter().find(|(k, _)| k == "mode")).map(|(_, v)| v.trim().to_lowercase())
        .or_else(|| sections.iter().find(|(n, _)| n == "DEFAULT")
            .and_then(|(_, items)| items.iter().find(|(k, _)| k == "mode")).map(|(_, v)| v.trim().to_lowercase()))
        .unwrap_or_else(|| DEFAULT_RECEIVE_MODE.into());
    if RECEIVE_MODES.contains(&mode.as_str()) { mode } else { DEFAULT_RECEIVE_MODE.into() }
}

#[derive(Debug)]
pub enum SaveError {
    /// `ValueError("Unknown receive mode: 'x'")`.
    Invalid(String),
    /// `OSError` text (`[Errno n] …: 'path'`).
    Io(String),
}

/// `save_receive_mode(mode, path)`: keep other sections, write `<path>.tmp`
/// and replace.
pub fn save_receive_mode(mode: &str, path: Option<&Path>) -> Result<(), SaveError> {
    if !RECEIVE_MODES.contains(&mode) {
        return Err(SaveError::Invalid(format!("Unknown receive mode: '{mode}'")));
    }
    let path = path.map(Path::to_path_buf).unwrap_or_else(config_file);
    let mut sections = match read_text(&path) {
        Some(Ok(text)) => parse(&text).unwrap_or_default(),
        _ => Vec::new(),
    };
    match sections.iter_mut().find(|(n, _)| n == "receive") {
        Some((_, items)) => match items.iter_mut().find(|(k, _)| k == "mode") {
            Some(entry) => entry.1 = mode.into(),
            None => items.push(("mode".into(), mode.into())),
        },
        None => sections.push(("receive".into(), vec![("mode".into(), mode.into())])),
    }
    let io = |e: std::io::Error, p: &Path| SaveError::Io(tessel_core::pyerr::os_error_path(&e, p));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(e, parent))?;
    }
    let mut temporary = path.clone().into_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, write(&sections)).map_err(|e| io(e, &temporary))?;
    std::fs::rename(&temporary, &path).map_err(|e| io(e, &temporary))?;
    Ok(())
}

/// `short_err(OSError)` = the text after the last ": ".
impl SaveError {
    pub fn text(&self) -> &str {
        match self {
            SaveError::Invalid(t) | SaveError::Io(t) => t,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("tessel-bluetooth-settings-{}-{}", std::process::id(), tessel_core::secrets::token_urlsafe(6)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn receive_mode_round_trip_preserves_other_sections() {
        let folder = temp();
        let path = folder.join("tessel-bluetooth").join("settings.ini");
        assert_eq!(load_receive_mode(Some(&path)), DEFAULT_RECEIVE_MODE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[other]\nkey = value\n").unwrap();
        save_receive_mode("off", Some(&path)).unwrap();
        assert_eq!(load_receive_mode(Some(&path)), "off");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("key = value"));
        assert_eq!(text, "[other]\nkey = value\n\n[receive]\nmode = off\n\n");
        assert!(!path.with_extension("ini.tmp").exists());
        std::fs::write(&path, "[receive]\nmode = bogus\n").unwrap();
        assert_eq!(load_receive_mode(Some(&path)), DEFAULT_RECEIVE_MODE);
        assert!(matches!(save_receive_mode("always", Some(&path)), Err(SaveError::Invalid(t)) if t == "Unknown receive mode: 'always'"));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn parser_follows_configparser() {
        assert_eq!(parse("[receive]\nMode : ASK \n").unwrap(), vec![("receive".to_owned(), vec![("mode".to_owned(), "ASK".to_owned())])]);
        assert!(parse("mode = ask\n").is_err());
        assert!(parse("[a]\n[a]\n").is_err());
        assert!(parse("[a]\nk = 1\nk = 2\n").is_err());
        assert!(parse("[a]\njunk\n").is_err());
        let folded = parse("[a]\nk = one\n  two\n# c\n").unwrap();
        assert_eq!(folded[0].1[0].1, "one\ntwo");
        assert_eq!(write(&folded), "[a]\nk = one\n\ttwo\n\n");
        let folder = temp();
        let path = folder.join("settings.ini");
        std::fs::write(&path, "[receive]\nmode = Off\n").unwrap();
        assert_eq!(load_receive_mode(Some(&path)), "off");
        std::fs::write(&path, "garbage without header\n").unwrap();
        assert_eq!(load_receive_mode(Some(&path)), "ask");
        save_receive_mode("off", Some(&path)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[receive]\nmode = off\n\n");
        std::fs::write(&path, [0xffu8, 0xfe]).unwrap();
        assert_eq!(load_receive_mode(Some(&path)), "ask");
        std::fs::remove_dir_all(folder).unwrap();
    }
}
