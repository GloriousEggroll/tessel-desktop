//! Shared clock presentation preferences; never changes the system clock
//! (port of shared/clock_preferences.py).

use crate::pyfmt::{json_dumps, JsonStyle};
use serde_json::{Map, Value};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const DATE_ORDERS: [&str; 6] = ["mdy", "dmy", "ymd", "ydm", "myd", "dym"];
pub const DATE_SEPARATORS: [&str; 3] = ["/", "-", "."];
const BOOL_KEYS: [&str; 8] = ["stacked", "date_first", "use_24_hour", "show_date", "show_year", "show_weekday",
                              "month_name", "short_names"];

/// `~/.config/desktop-suite/clock.json`.
pub fn preferences_path() -> PathBuf {
    crate::paths::suite_config_dir().join("clock.json")
}

/// The local time fields the formatter needs. `strftime` must be locale-aware
/// (`%p`, `%a`, `%A`, `%b`, `%B`); `glib::DateTime` implements it.
pub trait ClockTime {
    fn hour(&self) -> i32;
    fn minute(&self) -> i32;
    fn day(&self) -> i32;
    fn month(&self) -> i32;
    fn year(&self) -> i32;
    fn strftime(&self, format: &str) -> String;
}

impl ClockTime for glib::DateTime {
    fn hour(&self) -> i32 { glib::DateTime::hour(self) }
    fn minute(&self) -> i32 { glib::DateTime::minute(self) }
    fn day(&self) -> i32 { self.day_of_month() }
    fn month(&self) -> i32 { glib::DateTime::month(self) }
    fn year(&self) -> i32 { glib::DateTime::year(self) }
    fn strftime(&self, format: &str) -> String { self.format(format).map(Into::into).unwrap_or_default() }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockPreferences {
    pub stacked: bool,
    pub date_first: bool,
    pub use_24_hour: bool,
    pub date_order: String,
    pub date_separator: String,
    pub show_date: bool,
    pub show_year: bool,
    pub show_weekday: bool,
    pub month_name: bool,
    pub short_names: bool,
}

impl Default for ClockPreferences {
    fn default() -> Self {
        Self { stacked: true, date_first: false, use_24_hour: false, date_order: "mdy".into(), date_separator: "/".into(),
               show_date: true, show_year: true, show_weekday: false, month_name: false, short_names: false }
    }
}

impl ClockPreferences {
    fn bool_mut(&mut self, key: &str) -> Option<&mut bool> {
        Some(match key {
            "stacked" => &mut self.stacked,
            "date_first" => &mut self.date_first,
            "use_24_hour" => &mut self.use_24_hour,
            "show_date" => &mut self.show_date,
            "show_year" => &mut self.show_year,
            "show_weekday" => &mut self.show_weekday,
            "month_name" => &mut self.month_name,
            "short_names" => &mut self.short_names,
            _ => return None,
        })
    }

    /// Unknown or wrongly typed values fall back to their defaults.
    pub fn from_map(values: &Map<String, Value>) -> Self {
        let mut result = Self::default();
        for key in BOOL_KEYS {
            if let Some(Value::Bool(flag)) = values.get(key) {
                *result.bool_mut(key).unwrap() = *flag;
            }
        }
        if let Some(order) = values.get("date_order").and_then(Value::as_str).filter(|v| DATE_ORDERS.contains(v)) {
            result.date_order = order.into();
        }
        if let Some(sep) = values.get("date_separator").and_then(Value::as_str).filter(|v| DATE_SEPARATORS.contains(v)) {
            result.date_separator = sep.into();
        }
        result
    }

    /// `dataclasses.asdict` order.
    pub fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        for (key, value) in [("stacked", self.stacked), ("date_first", self.date_first), ("use_24_hour", self.use_24_hour)] {
            map.insert(key.into(), value.into());
        }
        map.insert("date_order".into(), self.date_order.clone().into());
        map.insert("date_separator".into(), self.date_separator.clone().into());
        for (key, value) in [("show_date", self.show_date), ("show_year", self.show_year), ("show_weekday", self.show_weekday),
                             ("month_name", self.month_name), ("short_names", self.short_names)] {
            map.insert(key.into(), value.into());
        }
        map
    }

    pub fn format_time(&self, value: &impl ClockTime) -> String {
        if self.use_24_hour {
            return format!("{:02}:{:02}", value.hour(), value.minute());
        }
        // Some 24-hour locales leave %p empty. An explicit 12-hour preference
        // still needs to distinguish morning from evening in those locales.
        let period = value.strftime("%p").trim().to_owned();
        let period = if period.is_empty() { if value.hour() < 12 { "AM".into() } else { "PM".into() } } else { period };
        let hour = if value.hour() % 12 == 0 { 12 } else { value.hour() % 12 };
        format!("{hour}:{:02} {period}", value.minute())
    }

    pub fn format_date(&self, value: &impl ClockTime) -> String {
        if !self.show_date {
            return String::new();
        }
        let month = if self.month_name {
            value.strftime(if self.short_names { "%b" } else { "%B" })
        } else {
            value.month().to_string()
        };
        // A named month reads naturally with spaces ("Oct 25 2026").
        let separator = if self.month_name { " " } else { self.date_separator.as_str() };
        self.date_order.chars()
            .filter(|part| *part != 'y' || self.show_year)
            .map(|part| match part {
                'm' => month.clone(),
                'd' => value.day().to_string(),
                _ => format!("{:04}", value.year()),
            })
            .collect::<Vec<_>>()
            .join(separator)
    }

    pub fn format_weekday(&self, value: &impl ClockTime) -> String {
        if self.show_weekday { value.strftime(if self.short_names { "%a" } else { "%A" }) } else { String::new() }
    }

    /// `(time, date)`, or `(date, time)` with `date_first`.
    pub fn ordered_parts(&self, value: &impl ClockTime) -> (String, String) {
        let (time, date) = (self.format_time(value), self.format_date(value));
        if self.date_first { (date, time) } else { (time, date) }
    }
}

#[derive(Debug)]
pub enum StoreError {
    /// Python's ValueError: unknown/invalid preference or a damaged file.
    Invalid(String),
    Io(io::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self { Self::Io(error) }
}

/// Atomically write settings and cheaply follow changes from another process.
///
/// Readers keep the last valid configuration through a failed/incomplete edit.
/// No configuration file is created until the user changes an option.
pub struct ClockPreferencesStore {
    pub path: PathBuf,
    signature: Option<(u64, i128, u64)>,
    preferences: ClockPreferences,
}

#[cfg(test)]
thread_local!(static FAIL_REPLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

impl Default for ClockPreferencesStore {
    fn default() -> Self { Self::new(preferences_path()) }
}

impl ClockPreferencesStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), signature: None, preferences: ClockPreferences::default() }
    }

    fn read(&self) -> Result<Map<String, Value>, StoreError> {
        let data = match fs::read(&self.path) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(error.into()),
        };
        let data = String::from_utf8(data).map_err(|_| StoreError::Invalid("Clock preferences are not UTF-8.".into()))?;
        if data.chars().count() > 65536 {
            return Err(StoreError::Invalid("Clock preferences file is too large.".into()));
        }
        match serde_json::from_str(&data) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(StoreError::Invalid("Clock preferences must be a JSON object.".into())),
            Err(error) => Err(StoreError::Invalid(error.to_string())),
        }
    }

    /// Current preferences, re-read only when the file's inode/mtime/size changed.
    pub fn load(&mut self) -> ClockPreferences {
        let info = match fs::metadata(&self.path) {
            Ok(info) => info,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.signature = None;
                self.preferences = ClockPreferences::default();
                return self.preferences.clone();
            }
            Err(_) => return self.preferences.clone(),
        };
        let signature = (info.ino(), info.mtime() as i128 * 1_000_000_000 + info.mtime_nsec() as i128, info.size());
        if Some(signature) != self.signature {
            if let Ok(values) = self.read() {
                self.preferences = ClockPreferences::from_map(&values);
                self.signature = Some(signature);
            }
        }
        self.preferences.clone()
    }

    /// Validate and merge `changes` into the file as it is now (other keys,
    /// including unknown future ones, are preserved), then reload.
    pub fn update(&mut self, changes: &Map<String, Value>) -> Result<ClockPreferences, StoreError> {
        let fields = ClockPreferences::default().to_map();
        if changes.keys().any(|key| !fields.contains_key(key)) {
            return Err(StoreError::Invalid("Unknown clock preference.".into()));
        }
        let mut candidate = fields.clone();
        candidate.extend(changes.clone());
        let parsed = ClockPreferences::from_map(&candidate).to_map();
        let same_type = |a: &Value, b: &Value| matches!((a, b), (Value::Bool(_), Value::Bool(_)) | (Value::String(_), Value::String(_)));
        if changes.iter().any(|(key, value)| parsed[key] != *value || !same_type(value, &fields[key])) {
            return Err(StoreError::Invalid("Invalid clock preference.".into()));
        }
        // Read at the moment of saving so two settings windows changing different
        // controls do not overwrite preferences loaded when they were opened.
        let mut values = self.read()?;
        values.extend(changes.clone());
        values.insert("version".into(), 1.into());
        let style = JsonStyle { ensure_ascii: false, sort_keys: true, indent: Some(2), ..JsonStyle::DEFAULT };
        let data = json_dumps(&Value::Object(values), &style) + "\n";
        atomic_write(&self.path, data.as_bytes(), ".clock-")?;
        self.signature = None;
        Ok(self.load())
    }
}

/// Write `data` to a temporary file beside `path`, fsync it and rename it over
/// `path`. The temporary file is removed on any failure.
pub fn atomic_write(path: &Path, data: &[u8], prefix: &str) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let (mut file, temporary) = create_temporary(parent, prefix)?;
    let result = (|| {
        file.write_all(data)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        #[cfg(test)]
        if FAIL_REPLACE.with(|fail| fail.get()) {
            return Err(io::Error::other("read only"));
        }
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

// tempfile.mkstemp: O_EXCL, mode 0600, random suffix.
fn create_temporary(directory: &Path, prefix: &str) -> io::Result<(fs::File, PathBuf)> {
    use std::os::unix::fs::OpenOptionsExt;
    for attempt in 0..100u32 {
        let random = glib::random_int() ^ attempt.wrapping_mul(2654435761);
        let path = directory.join(format!("{prefix}{random:08x}"));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "No usable temporary file name found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Clone)]
    struct Time { hour: i32, minute: i32, day: i32, month: i32, year: i32, names: fn(&str) -> Option<&'static str> }

    fn english(format: &str) -> Option<&'static str> {
        Some(match format { "%a" => "Mon", "%A" => "Monday", "%b" => "Sep", "%B" => "September", _ => return None })
    }

    impl ClockTime for Time {
        fn hour(&self) -> i32 { self.hour }
        fn minute(&self) -> i32 { self.minute }
        fn day(&self) -> i32 { self.day }
        fn month(&self) -> i32 { self.month }
        fn year(&self) -> i32 { self.year }
        fn strftime(&self, format: &str) -> String {
            if let Some(name) = (self.names)(format) {
                return name.into();
            }
            if format == "%p" { if self.hour < 12 { "AM" } else { "PM" }.into() } else { String::new() }
        }
    }

    fn now() -> Time { Time { hour: 13, minute: 4, day: 21, month: 9, year: 2026, names: english } }

    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!("suite-clock-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory.join("clock.json")
    }

    fn changes(value: Value) -> Map<String, Value> { value.as_object().unwrap().clone() }

    fn with(build: impl FnOnce(&mut ClockPreferences)) -> ClockPreferences {
        let mut preferences = ClockPreferences::default();
        build(&mut preferences);
        preferences
    }

    #[test]
    fn default_preserves_existing_clock_without_creating_preferences() {
        let path = temp();
        let settings = ClockPreferencesStore::new(&path).load();
        assert!(settings.stacked && !settings.date_first && settings.show_date && settings.show_year && !settings.show_weekday);
        assert_eq!(settings.format_weekday(&now()), "");
        assert_eq!(settings.format_date(&now()), "9/21/2026");
        assert_eq!(settings.format_time(&now()), "1:04 PM");
        assert!(!path.exists());
    }

    #[test]
    fn glib_datetime_formats_like_strftime() {
        let value = glib::DateTime::new(&glib::TimeZone::utc(), 2026, 9, 21, 13, 4, 0.0).unwrap();
        let settings = ClockPreferences::default();
        assert_eq!(settings.format_date(&value), "9/21/2026");
        assert!(settings.format_time(&value).starts_with("1:04 "));
    }

    #[test]
    fn twelve_and_twenty_four_hour_midnight_noon_and_evening() {
        for (hour, twelve, twenty_four) in [(0, "12:04 AM", "00:04"), (12, "12:04 PM", "12:04"), (13, "1:04 PM", "13:04"),
                                            (23, "11:04 PM", "23:04")] {
            let value = Time { hour, ..now() };
            assert_eq!(ClockPreferences::default().format_time(&value), twelve);
            assert_eq!(with(|p| p.use_24_hour = true).format_time(&value), twenty_four);
        }
    }

    #[test]
    fn month_names_and_abbreviations() {
        let value = Time { day: 25, month: 10, hour: 17, minute: 30, names: |f| Some(match f {
            "%a" => "Sun", "%A" => "Sunday", "%b" => "Oct", "%B" => "October", _ => return None }), ..now() };
        let mac = with(|p| { p.month_name = true; p.short_names = true; p.show_year = false; p.show_weekday = true });
        assert_eq!((mac.format_weekday(&value), mac.format_date(&value)), ("Sun".into(), "Oct 25".into()));
        let full = with(|p| { p.month_name = true; p.date_order = "dmy".into(); p.show_weekday = true });
        assert_eq!((full.format_weekday(&value), full.format_date(&value)), ("Sunday".into(), "25 October 2026".into()));
        assert_eq!(with(|p| p.short_names = true).format_date(&value), "10/25/2026");
        let loaded = ClockPreferences::from_map(&changes(json!({"month_name": true, "short_names": "yes"})));
        assert_eq!((loaded.month_name, loaded.short_names), (true, false));
    }

    #[test]
    fn every_date_permutation_and_separator_and_label_order() {
        let mut orders: Vec<_> = DATE_ORDERS.to_vec();
        orders.sort();
        assert_eq!(orders, ["dmy", "dym", "mdy", "myd", "ydm", "ymd"]);
        for order in DATE_ORDERS {
            for separator in DATE_SEPARATORS {
                for stacked in [true, false] {
                    for reverse in [true, false] {
                        let settings = with(|p| { p.stacked = stacked; p.date_first = reverse;
                                                  p.date_order = order.into(); p.date_separator = separator.into() });
                        let expected = order.chars().map(|c| match c { 'd' => "21", 'm' => "9", _ => "2026" })
                            .collect::<Vec<_>>().join(separator);
                        assert_eq!(settings.format_date(&now()), expected);
                        let parts = settings.ordered_parts(&now());
                        assert_eq!(if reverse { parts.0 } else { parts.1 }, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn twelve_hour_preference_has_period_in_locales_without_am_pm_names() {
        let value = Time { names: |f| if f == "%p" { Some("") } else { None }, ..now() };
        assert_eq!(ClockPreferences::default().format_time(&Time { hour: 1, minute: 5, ..value.clone() }), "1:05 AM");
        assert_eq!(ClockPreferences::default().format_time(&Time { hour: 13, minute: 5, ..value }), "1:05 PM");
    }

    #[test]
    fn hidden_year_keeps_remaining_date_order_and_separators() {
        for order in DATE_ORDERS {
            for separator in DATE_SEPARATORS {
                let settings = with(|p| { p.show_year = false; p.date_order = order.into(); p.date_separator = separator.into() });
                let expected = if order.find('d') < order.find('m') { format!("21{separator}9") } else { format!("9{separator}21") };
                assert_eq!(settings.format_date(&now()), expected);
                assert_eq!(settings.format_date(&now()).matches(separator).count(), 1);
            }
        }
    }

    #[test]
    fn date_and_localized_weekday_visibility_are_independent() {
        let value = Time { names: |f| if f == "%A" { Some("lunes") } else { None }, ..now() };
        for date_visible in [false, true] {
            for weekday_visible in [false, true] {
                for date_first in [false, true] {
                    let settings = with(|p| { p.show_date = date_visible; p.show_weekday = weekday_visible; p.date_first = date_first });
                    let expected = if date_visible { "9/21/2026" } else { "" };
                    assert_eq!(settings.format_date(&value), expected);
                    assert_eq!(settings.format_weekday(&value), if weekday_visible { "lunes" } else { "" });
                    let parts = settings.ordered_parts(&value);
                    assert_eq!(if date_first { parts.0 } else { parts.1 }, expected);
                }
            }
        }
    }

    #[test]
    fn visibility_preferences_round_trip_without_discarding_format() {
        let path = temp();
        let (mut writer, mut reader) = (ClockPreferencesStore::new(&path), ClockPreferencesStore::new(&path));
        writer.update(&changes(json!({"date_order": "ydm", "date_separator": "-", "show_year": false, "show_weekday": true}))).unwrap();
        writer.update(&changes(json!({"show_date": false}))).unwrap();
        assert_eq!(reader.load().format_date(&now()), "");
        assert!(reader.load().show_weekday);
        writer.update(&changes(json!({"show_date": true}))).unwrap();
        assert_eq!(reader.load().format_date(&now()), "21-9");
        assert!(reader.load().show_weekday);
        assert!(!reader.load().show_year);
    }

    #[test]
    fn separate_readers_see_changes_and_writers_preserve_other_options() {
        let path = temp();
        let mut settings = ClockPreferencesStore::new(&path);
        let mut dock = ClockPreferencesStore::new(&path);
        let mut second_settings_window = ClockPreferencesStore::new(&path);
        dock.load();
        second_settings_window.load();
        settings.update(&changes(json!({"stacked": false, "use_24_hour": true}))).unwrap();
        assert!(!dock.load().stacked);
        assert!(dock.load().use_24_hour);
        second_settings_window.update(&changes(json!({"date_order": "ydm", "date_first": true}))).unwrap();
        assert_eq!(dock.load(), with(|p| { p.stacked = false; p.use_24_hour = true; p.date_order = "ydm".into(); p.date_first = true }));
        assert_eq!(ClockPreferencesStore::new(&path).load(), dock.load());
    }

    #[test]
    fn invalid_values_fall_back_and_invalid_file_keeps_last_good_state() {
        let path = temp();
        fs::write(&path, json!({"stacked": "false", "use_24_hour": 1, "show_date": "false", "show_year": 0,
                                "show_weekday": [], "date_order": ["y", "m", "d"], "date_separator": "<"}).to_string()).unwrap();
        let mut store = ClockPreferencesStore::new(&path);
        assert_eq!(store.load(), ClockPreferences::default());
        store.update(&changes(json!({"date_order": "dmy"}))).unwrap();
        fs::write(&path, "{").unwrap();
        assert_eq!(store.load().date_order, "dmy");
        fs::write(&path, "[]").unwrap();
        assert_eq!(store.load().date_order, "dmy");
        fs::remove_file(&path).unwrap();
        assert_eq!(store.load(), ClockPreferences::default());
    }

    #[test]
    fn failed_save_retains_complete_previous_file_and_cleans_temporary() {
        let path = temp();
        let mut store = ClockPreferencesStore::new(&path);
        store.update(&changes(json!({"date_order": "ymd"}))).unwrap();
        let before = fs::read(&path).unwrap();
        FAIL_REPLACE.with(|fail| fail.set(true));
        let result = store.update(&changes(json!({"stacked": false})));
        FAIL_REPLACE.with(|fail| fail.set(false));
        assert!(matches!(result, Err(StoreError::Io(_))));
        assert_eq!(fs::read(&path).unwrap(), before);
        let entries: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(entries, vec![path.clone()]);
    }

    #[test]
    fn saves_preserve_unknown_keys_and_reject_invalid_control_values() {
        let path = temp();
        fs::write(&path, json!({"future_option": "preserved"}).to_string()).unwrap();
        let mut store = ClockPreferencesStore::new(&path);
        store.update(&changes(json!({"date_order": "dym"}))).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["future_option"], "preserved");
        assert_eq!(fs::read_to_string(&path).unwrap(),
                   "{\n  \"date_order\": \"dym\",\n  \"future_option\": \"preserved\",\n  \"version\": 1\n}\n");
        let before = fs::read(&path).unwrap();
        for invalid in [json!({"date_order": "my"}), json!({"date_separator": "!"}), json!({"stacked": 1}),
                        json!({"use_24_hour": "yes"}), json!({"system_time": "12:00"}), json!({"show_date": 0}),
                        json!({"show_year": "false"}), json!({"show_weekday": 1})] {
            assert!(matches!(store.update(&changes(invalid.clone())), Err(StoreError::Invalid(_))), "{invalid}");
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}
