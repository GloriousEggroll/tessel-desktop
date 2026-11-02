//! Calendar view preferences: first day of the week and event sources
//! (calendar_preferences.py). Stored in `~/.config/tessel/calendar.json`
//! (or `XDG_CONFIG_HOME`); no file is created until the user changes an option.

use crate::civil::Date;
use crate::events::Source;
use crate::py;
use serde_json::{Map, Value};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const WEEK_STARTS: [&str; 4] = ["automatic", "sunday", "monday", "saturday"];
pub const SOURCE_KINDS: [&str; 2] = ["file", "url"];
pub const MAX_SOURCES: usize = 32;
pub const MAX_FILE_CHARS: usize = 262_144;
/// Python's `calendar` weekday numbers (Monday = 0).
pub const MONDAY: i64 = 0;
pub const SATURDAY: i64 = 5;
pub const SUNDAY: i64 = 6;

pub fn preferences_path() -> PathBuf {
    tessel_core::paths::suite_config_dir().join("calendar.json")
}

fn splitlines(text: &str) -> Vec<&str> {
    // str.splitlines() boundaries.
    let is_break = |c: char| matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(is_break) {
            Some(index) => {
                lines.push(&rest[..index]);
                let width = if rest[index..].starts_with("\r\n") { 2 } else { rest[index..].chars().next().unwrap().len_utf8() };
                rest = &rest[index + width..];
            }
            None => {
                lines.push(rest);
                break;
            }
        }
    }
    lines
}

/// Python's weekday number (Monday = 0) from `locale -k LC_TIME` output.
pub fn parse_locale_week_start(text: &str) -> i64 {
    let mut values: Vec<(String, String)> = Vec::new();
    for line in splitlines(text) {
        if let Some((key, value)) = line.split_once('=') {
            let key = py::strip(key).to_owned();
            let value = py::strip(value).trim_matches('"').to_owned();
            if let Some(entry) = values.iter_mut().find(|(k, _)| *k == key) {
                entry.1 = value;
            } else {
                values.push((key, value));
            }
        }
    }
    let get = |key: &str, default: &str| values.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_else(|| default.into());
    let reference: Vec<char> = get("week-1stday", "19971130").chars().collect();
    let part = |a: usize, b: usize| -> Option<i128> {
        let b = b.min(reference.len());
        py::int(&reference[a.min(b)..b].iter().collect::<String>())
    };
    let base = (|| {
        let (y, m, d) = (part(0, 4)?, part(4, 6)?, part(6, 8)?);
        Date::new(i64::try_from(y).ok()?, i64::try_from(m).ok()?, i64::try_from(d).ok()?)
    })();
    let offset = py::int(&get("first_weekday", "1")).map(|v| v - 1);
    match (base, offset) {
        (Some(base), Some(offset)) if (0..7).contains(&offset) => (base.weekday() + offset as i64) % 7,
        _ => SUNDAY,
    }
}

/// The locale's first weekday; read once per process (`locale` is quick).
pub fn locale_week_start() -> i64 {
    static CACHE: OnceLock<i64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let child = std::process::Command::new("locale").args(["-k", "LC_TIME"]).stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn();
        let Ok(mut child) = child else { return SUNDAY };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return SUNDAY;
                }
            }
        }
        let mut output = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            use std::io::Read;
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            output = String::from_utf8_lossy(&bytes).into_owned();
        }
        parse_locale_week_start(&output)
    })
}

/// `urllib.parse.unquote` (UTF-8, errors="replace").
fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &bytes[i + 1..i + 3];
            if hex.iter().all(|b| b.is_ascii_hexdigit()) {
                out.push(u8::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap());
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `urlparse(value).path` for a `file:` URL.
fn file_url_path(value: &str) -> String {
    let cleaned: String = value.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let rest = &cleaned[cleaned.find(':').map_or(0, |i| i + 1)..];
    let rest = rest.split('#').next().unwrap_or("");
    let rest = rest.split('?').next().unwrap_or("");
    let mut path = rest;
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find('/').unwrap_or(after.len());
        path = &after[end..];
    }
    // urlparse splits ";params" off the last path segment.
    if let Some(semicolon) = path.rfind('/').map_or(path.find(';'), |slash| path[slash..].find(';').map(|i| i + slash)) {
        path = &path[..semicolon];
    }
    path.to_owned()
}

fn home_directory() -> Option<String> {
    if let Ok(home) = std::env::var("HOME") {
        return Some(home);
    }
    // SAFETY: getpwuid returns a pointer into static storage or null.
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*entry).pw_dir).to_string_lossy().into_owned())
    }
}

/// `os.path.expanduser`.
fn expanduser(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_owned();
    }
    let end = path.find('/').unwrap_or(path.len());
    let user = &path[1..end];
    let home = if user.is_empty() {
        home_directory()
    } else {
        let Ok(name) = std::ffi::CString::new(user) else { return path.to_owned() };
        // SAFETY: getpwnam returns a pointer into static storage or null.
        unsafe {
            let entry = libc::getpwnam(name.as_ptr());
            if entry.is_null() { None } else { Some(std::ffi::CStr::from_ptr((*entry).pw_dir).to_string_lossy().into_owned()) }
        }
    };
    let Some(home) = home else { return path.to_owned() };
    let home = home.trim_end_matches('/');
    let joined = format!("{home}{}", &path[end..]);
    if joined.is_empty() { "/".into() } else { joined }
}

/// `str(pathlib.Path(text))` for an absolute path.
fn pathlib_str(path: &str) -> String {
    let prefix = if path.starts_with("//") && !path.starts_with("///") { "//" } else if path.starts_with('/') { "/" } else { "" };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let joined = prefix.to_owned() + &parts.join("/");
    if joined.is_empty() { ".".into() } else { joined }
}

/// `(kind, location)` for a user-entered file path or URL, or the ValueError text.
pub fn normalize_location(text: &str) -> Result<(String, String), String> {
    let mut value = py::strip(text).to_owned();
    if value.is_empty() {
        return Err("Enter a calendar file or address.".into());
    }
    let mut lowered = value.to_lowercase();
    if lowered.starts_with("webcal://") {
        value = format!("https://{}", value.chars().skip(9).collect::<String>());
        lowered = value.to_lowercase();
    }
    if lowered.starts_with("http://") || lowered.starts_with("https://") {
        return Ok(("url".into(), value));
    }
    if lowered.starts_with("file://") {
        value = unquote(&file_url_path(&value));
    }
    let path = expanduser(&value);
    if !path.starts_with('/') {
        return Err("Use a full file path or a web address.".into());
    }
    Ok(("file".into(), pathlib_str(&path)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarPreferences {
    pub week_start: String,
    pub sources: Vec<Source>,
}

impl Default for CalendarPreferences {
    fn default() -> Self {
        Self { week_start: "automatic".into(), sources: Vec::new() }
    }
}

impl CalendarPreferences {
    pub fn new(week_start: &str, sources: Vec<Source>) -> Self {
        let week_start = if WEEK_STARTS.contains(&week_start) { week_start } else { "automatic" };
        Self { week_start: week_start.into(), sources }
    }

    pub fn from_map(values: &Map<String, Value>) -> Self {
        let mut sources = Vec::new();
        if let Some(Value::Array(items)) = values.get("sources") {
            for item in items {
                let Value::Object(item) = item else { continue };
                let kind = item.get("kind").and_then(Value::as_str);
                let location = item.get("location").and_then(Value::as_str);
                if let (Some(kind), Some(location)) = (kind, location) {
                    if SOURCE_KINDS.contains(&kind) && !location.is_empty() {
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                        sources.push(Source { kind: kind.into(), location: location.into(), name: name.into() });
                    }
                }
            }
        }
        sources.truncate(MAX_SOURCES);
        Self::new(values.get("week_start").and_then(Value::as_str).unwrap_or(""), sources)
    }

    pub fn first_weekday(&self) -> i64 {
        self.first_weekday_with(locale_week_start)
    }

    pub fn first_weekday_with(&self, locale: impl Fn() -> i64) -> i64 {
        match self.week_start.as_str() {
            "sunday" => SUNDAY,
            "monday" => MONDAY,
            "saturday" => SATURDAY,
            _ => locale(),
        }
    }
}

#[derive(Debug)]
pub enum PreferencesError {
    /// ValueError (shown translated).
    Invalid(String),
    /// OSError / UnicodeError (shown inside "Could not save calendar preferences: {error}").
    Io(String),
}

impl std::fmt::Display for PreferencesError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Io(m) => f.write_str(m),
        }
    }
}

/// Python's `str(OSError)`: `[Errno N] strerror: 'path'`.
pub fn os_error_text(error: &io::Error, path: &std::path::Path) -> String {
    match error.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a valid C string.
            let text = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
            format!("[Errno {code}] {text}: '{}'", path.display())
        }
        None => error.to_string(),
    }
}

/// Atomic writes; readers keep the last valid file through a broken edit.
pub struct CalendarPreferencesStore {
    pub path: PathBuf,
    signature: Option<(u64, i128, u64)>,
    preferences: CalendarPreferences,
}

impl Default for CalendarPreferencesStore {
    fn default() -> Self {
        Self::new(preferences_path())
    }
}

impl CalendarPreferencesStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), signature: None, preferences: CalendarPreferences::default() }
    }

    fn read(&self) -> Result<Map<String, Value>, PreferencesError> {
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(PreferencesError::Io(os_error_text(&error, &self.path))),
        };
        let text = String::from_utf8(data).map_err(|e| {
            let at = e.utf8_error().valid_up_to();
            PreferencesError::Io(format!("'utf-8' codec can't decode byte 0x{:02x} in position {at}: invalid start byte", e.as_bytes()[at]))
        })?;
        if text.chars().count() > MAX_FILE_CHARS {
            return Err(PreferencesError::Invalid("Calendar preferences file is too large.".into()));
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(PreferencesError::Invalid("Calendar preferences must be a JSON object.".into())),
            Err(error) => Err(PreferencesError::Invalid(error.to_string())),
        }
    }

    pub fn load(&mut self) -> CalendarPreferences {
        let info = match std::fs::metadata(&self.path) {
            Ok(info) => info,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.signature = None;
                self.preferences = CalendarPreferences::default();
                return self.preferences.clone();
            }
            Err(_) => return self.preferences.clone(),
        };
        let signature = (info.ino(), info.mtime() as i128 * 1_000_000_000 + info.mtime_nsec() as i128, info.size());
        if Some(signature) != self.signature {
            if let Ok(values) = self.read() {
                self.preferences = CalendarPreferences::from_map(&values);
                self.signature = Some(signature);
            }
        }
        self.preferences.clone()
    }

    fn write(&mut self, mut values: Map<String, Value>) -> Result<CalendarPreferences, PreferencesError> {
        values.insert("version".into(), 1.into());
        let style = JsonStyle { ensure_ascii: false, sort_keys: true, indent: Some(2), ..JsonStyle::DEFAULT };
        let data = json_dumps(&Value::Object(values), &style) + "\n";
        tessel_core::clock::atomic_write(&self.path, data.as_bytes(), ".calendar-")
            .map_err(|e| PreferencesError::Io(os_error_text(&e, &self.path)))?;
        self.signature = None;
        Ok(self.load())
    }

    pub fn set_week_start(&mut self, value: &str) -> Result<CalendarPreferences, PreferencesError> {
        if !WEEK_STARTS.contains(&value) {
            return Err(PreferencesError::Invalid("Invalid first day of the week.".into()));
        }
        let mut values = self.read()?;
        values.insert("week_start".into(), value.into());
        self.write(values)
    }

    fn source_values(sources: &[Source]) -> Value {
        Value::Array(sources.iter().map(|s| serde_json::json!({"kind": s.kind, "location": s.location, "name": s.name})).collect())
    }

    pub fn add_source(&mut self, text: &str, name: &str) -> Result<CalendarPreferences, PreferencesError> {
        let (kind, location) = normalize_location(text).map_err(PreferencesError::Invalid)?;
        let mut values = self.read()?;
        let mut current = CalendarPreferences::from_map(&values).sources;
        if current.iter().any(|s| s.location == location) {
            return Err(PreferencesError::Invalid("That calendar is already added.".into()));
        }
        if current.len() >= MAX_SOURCES {
            return Err(PreferencesError::Invalid("Too many calendars.".into()));
        }
        current.push(Source { kind, location, name: name.into() });
        values.insert("sources".into(), Self::source_values(&current));
        self.write(values)
    }

    pub fn remove_source(&mut self, location: &str) -> Result<CalendarPreferences, PreferencesError> {
        let mut values = self.read()?;
        let current: Vec<Source> = CalendarPreferences::from_map(&values).sources.into_iter().filter(|s| s.location != location).collect();
        values.insert("sources".into(), Self::source_values(&current));
        self.write(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glibc_week_definitions() {
        let us = "week-ndays=7\nweek-1stday=19971130\nfirst_weekday=1\nfirst_workday=2\n";
        let gb = "week-ndays=7\nweek-1stday=19971130\nfirst_weekday=2\nfirst_workday=2\n";
        let iso = "week-1stday=19971201\nfirst_weekday=1\n";
        let middle_east = "week-1stday=19971130\nfirst_weekday=7\n";
        assert_eq!(parse_locale_week_start(us), SUNDAY);
        assert_eq!(parse_locale_week_start(gb), MONDAY);
        assert_eq!(parse_locale_week_start(iso), MONDAY);
        assert_eq!(parse_locale_week_start(middle_east), SATURDAY);
        assert_eq!(parse_locale_week_start("garbage"), SUNDAY);
        assert_eq!(parse_locale_week_start("week-1stday=x\nfirst_weekday=9"), SUNDAY);
        assert_eq!(parse_locale_week_start("week-1stday=\"19971201\"\r\nfirst_weekday = 1"), MONDAY);
    }

    #[test]
    fn override_wins_over_locale() {
        let monday = || MONDAY;
        assert_eq!(CalendarPreferences::default().first_weekday_with(monday), MONDAY);
        assert_eq!(CalendarPreferences::new("saturday", vec![]).first_weekday_with(monday), SATURDAY);
        assert_eq!(CalendarPreferences::new("sunday", vec![]).first_weekday_with(monday), SUNDAY);
        assert_eq!(CalendarPreferences::new("bogus", vec![]).week_start, "automatic");
    }

    fn temp_store() -> (PathBuf, CalendarPreferencesStore) {
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = PathBuf::from(base).join(format!("calendar-prefs-{}-{}", std::process::id(), glib::random_int()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calendar.json");
        (dir, CalendarPreferencesStore::new(path))
    }

    #[test]
    fn defaults_without_creating_a_file() {
        let (dir, mut store) = temp_store();
        let preferences = store.load();
        assert_eq!(preferences.week_start, "automatic");
        assert!(preferences.sources.is_empty());
        assert!(!store.path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sources_add_remove_and_validation() {
        let (dir, mut store) = temp_store();
        store.add_source("webcal://example.com/cal.ics", "").unwrap();
        store.add_source("/home/user/cal.ics", "Work").unwrap();
        let sources = store.load().sources;
        assert_eq!(sources.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>(), ["url", "file"]);
        assert_eq!(sources[0].location, "https://example.com/cal.ics");
        assert_eq!(sources[1].name, "Work");
        assert!(matches!(store.add_source("https://example.com/cal.ics", ""), Err(PreferencesError::Invalid(_))));
        for bad in ["", "relative/file.ics"] {
            assert!(matches!(store.add_source(bad, ""), Err(PreferencesError::Invalid(_))));
        }
        store.remove_source("https://example.com/cal.ics").unwrap();
        assert_eq!(store.load().sources.iter().map(|s| s.location.as_str()).collect::<Vec<_>>(), ["/home/user/cal.ics"]);
        store.set_week_start("monday").unwrap();
        assert_eq!(store.load().week_start, "monday");
        assert_eq!(store.load().sources.len(), 1, "Changing week start keeps sources");
        assert!(store.set_week_start("tuesday").is_err());
        let text = std::fs::read_to_string(&store.path).unwrap();
        assert_eq!(text, "{\n  \"sources\": [\n    {\n      \"kind\": \"file\",\n      \"location\": \"/home/user/cal.ics\",\n      \"name\": \"Work\"\n    }\n  ],\n  \"version\": 1,\n  \"week_start\": \"monday\"\n}\n");
        for _ in 0..31 {
            let n = glib::random_int();
            store.add_source(&format!("/x/{n}.ics"), "").unwrap();
        }
        assert!(matches!(store.add_source("/x/last.ics", ""), Err(PreferencesError::Invalid(m)) if m == "Too many calendars."));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_file_keeps_last_valid_preferences() {
        let (dir, mut store) = temp_store();
        store.set_week_start("saturday").unwrap();
        assert_eq!(store.load().week_start, "saturday");
        std::fs::write(&store.path, "{broken").unwrap();
        assert_eq!(store.load().week_start, "saturday");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_uri_and_home_expansion() {
        assert_eq!(normalize_location("file:///tmp/a%20b.ics").unwrap(), ("file".into(), "/tmp/a b.ics".into()));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(normalize_location("~/x.ics").unwrap().1, format!("{home}/x.ics"));
        assert_eq!(normalize_location("  WEBCAL://Host/a.ics ").unwrap(), ("url".into(), "https://Host/a.ics".into()));
        assert_eq!(normalize_location("/a//b/./c/").unwrap().1, "/a/b/c");
        assert_eq!(normalize_location("file://host/p;x?q#f").unwrap().1, "/p");
        assert_eq!(normalize_location("relative").unwrap_err(), "Use a full file path or a web address.");
    }
}
