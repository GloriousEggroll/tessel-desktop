//! weather_backend.py: configuration, Open-Meteo queries and the shared,
//! cross-process disk cache. Network methods block: call them off the UI thread.

use crate::compat::{json_error, os_error, py_eq, truthy};
use crate::dates::{self, Date};
use serde_json::{Map, Value};
use std::fmt;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tessel_core::pyfmt::{float_repr, json_dumps, JsonStyle};

pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub const MAX_CONFIG_BYTES: usize = 8192;
pub const MAX_CACHE_FILES: usize = 8;
pub const NETWORK_TIMEOUT: u64 = 8;
pub const RETRY_SECONDS: f64 = 60.0;
pub const MAX_STALE_SECONDS: f64 = 7.0 * 86400.0;
pub const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
pub const GEOCODING_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
pub const NO_LOCATION: &str = "Choose a location in Weather Settings.";
const IMPERIAL_TERRITORIES: [&str; 3] = ["US", "LR", "MM"];
const MAX_TIMESTAMP: f64 = 253402300799.0;

/// A readable configuration, network, or response error.
#[derive(Clone, Debug, PartialEq)]
pub struct WeatherError(pub String);

impl fmt::Display for WeatherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl std::error::Error for WeatherError {}

fn err<T>(message: &str) -> Result<T, WeatherError> { Err(WeatherError(message.to_owned())) }

/// A JSON number that remembers whether it was an int (the cache key hashes its text).
#[derive(Clone, Copy, Debug)]
pub enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    pub fn value(self) -> f64 {
        match self {
            Num::Int(v) => v as f64,
            Num::Float(v) => v,
        }
    }
    /// `str()` of the Python number.
    pub fn text(self) -> String {
        match self {
            Num::Int(v) => v.to_string(),
            Num::Float(v) => float_repr(v),
        }
    }
    pub fn to_json(self) -> Value {
        match self {
            Num::Int(v) => Value::from(v),
            Num::Float(v) => serde_json::Number::from_f64(v).map(Value::Number).unwrap_or(Value::Null),
        }
    }
    fn from_json(value: &Value) -> Option<Self> {
        let number = value.as_number()?;
        number.as_i64().map(Num::Int).or_else(|| number.as_f64().map(Num::Float))
    }
}

impl PartialEq for Num {
    fn eq(&self, other: &Self) -> bool { self.value() == other.value() }
}

impl From<f64> for Num {
    fn from(value: f64) -> Self { Num::Float(value) }
}

pub fn measurement_locale(environ: &dyn Fn(&str) -> Option<String>) -> String {
    for name in ["LC_ALL", "LC_MEASUREMENT", "LANG"] {
        if let Some(value) = environ(name).filter(|v| !v.is_empty()) {
            return value;
        }
    }
    "C".into()
}

/// Fahrenheit for imperial-measurement locales (US, Liberia, Myanmar), else Celsius.
pub fn default_units_with(environ: &dyn Fn(&str) -> Option<String>) -> String {
    let locale = measurement_locale(environ);
    let value = locale.split('.').next().unwrap_or("").split('@').next().unwrap_or("");
    let territory = value.split_once('_').map(|t| t.1).unwrap_or("");
    if IMPERIAL_TERRITORIES.contains(&territory.to_uppercase().as_str()) { "fahrenheit" } else { "celsius" }.into()
}

pub fn default_units() -> String { default_units_with(&|name| std::env::var(name).ok()) }

#[derive(Clone, Debug, PartialEq)]
pub struct WeatherConfig {
    pub name: String,
    pub latitude: Num,
    pub longitude: Num,
    pub timezone: String,
    pub units: String,
    pub refresh_minutes: i64,
    pub week: String,
}

impl Default for WeatherConfig {
    /// No default city: an empty name means "no location chosen yet".
    fn default() -> Self {
        Self { name: String::new(), latitude: Num::Float(0.0), longitude: Num::Float(0.0), timezone: "UTC".into(),
               units: default_units(), refresh_minutes: 60, week: "upcoming".into() }
    }
}

impl WeatherConfig {
    pub fn new(name: &str, latitude: f64, longitude: f64, timezone: &str, units: &str) -> Self {
        Self { name: name.into(), latitude: latitude.into(), longitude: longitude.into(), timezone: timezone.into(),
               units: units.into(), ..Self::default() }
    }

    /// `asdict(config)` in field order.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("name".into(), Value::from(self.name.clone()));
        map.insert("latitude".into(), self.latitude.to_json());
        map.insert("longitude".into(), self.longitude.to_json());
        map.insert("timezone".into(), Value::from(self.timezone.clone()));
        map.insert("units".into(), Value::from(self.units.clone()));
        map.insert("refresh_minutes".into(), Value::from(self.refresh_minutes));
        map.insert("week".into(), Value::from(self.week.clone()));
        Value::Object(map)
    }

    fn raw(&self) -> Map<String, Value> {
        let mut map = match self.to_json() { Value::Object(map) => map, _ => unreachable!() };
        // Keep non-finite floats invalid (JSON cannot hold them).
        for (key, value) in [("latitude", self.latitude), ("longitude", self.longitude)] {
            if !value.value().is_finite() {
                map.insert(key.into(), Value::String("nan".into()));
            }
        }
        map
    }
}

pub fn has_location(config: &WeatherConfig) -> bool { !config.name.trim().is_empty() }

#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub name: String,
    pub latitude: Num,
    pub longitude: Num,
    pub timezone: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentWeather {
    pub time: String,
    pub temp: f64,
    pub code: i64,
    pub is_day: bool,
    pub apparent: Option<f64>,
    pub humidity: Option<f64>,
    pub wind: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForecastDay {
    pub date: Date,
    pub temp_min: Option<f64>,
    pub temp_max: Option<f64>,
    pub code: Option<i64>,
    pub precip_probability: Option<f64>,
    pub past: bool,
}

impl ForecastDay {
    pub fn empty(date: Date, past: bool) -> Self {
        Self { date, temp_min: None, temp_max: None, code: None, precip_probability: None, past }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WeatherSnapshot {
    pub config: WeatherConfig,
    pub current: Option<CurrentWeather>,
    pub days: Vec<ForecastDay>,
    pub fetched_at: Option<f64>,
    pub stale: bool,
    pub error: Option<String>,
}

/// `XDG_*` only when it is an absolute path (weather_backend._xdg_directory).
pub fn xdg_directory(variable: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(variable) {
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => tessel_core::paths::home().join(fallback),
    }
}

/// `_number`: finite int/float (never bool), else None.
pub fn number(value: &Value) -> Option<f64> {
    value.as_number().and_then(|n| n.as_f64()).filter(|v| v.is_finite())
}

/// `json.loads` that rejects NaN/Infinity (serde_json never accepts them).
pub fn finite_json(data: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(data).map_err(|e| json_error(data, &e))
}

/// Validate config fields given as JSON values (types are checked like Python).
fn validate_map(map: &Map<String, Value>, require_location: bool) -> Result<WeatherConfig, WeatherError> {
    let get = |key: &str| map.get(key).cloned().unwrap_or(Value::Null);
    let name = get("name");
    let name_text = name.as_str();
    if !require_location && name_text == Some("") {
    } else if name_text.is_none_or(|n| n.trim().is_empty() || n.chars().count() > 160) {
        return err("Enter a location name of 1 to 160 characters.");
    }
    let name_text = name_text.unwrap_or("");
    if name_text.chars().any(|c| (c as u32) < 32) {
        return err("The location name contains unsupported characters.");
    }
    let (latitude, longitude) = (number(&get("latitude")), number(&get("longitude")));
    if !latitude.is_some_and(|v| (-90.0..=90.0).contains(&v)) || !longitude.is_some_and(|v| (-180.0..=180.0).contains(&v)) {
        return err("Enter valid latitude and longitude coordinates.");
    }
    let timezone = get("timezone");
    if !timezone.as_str().is_some_and(|tz| tz.chars().count() <= 100 && dates::zone_exists(tz)) {
        return err("Choose a valid time zone for this location.");
    }
    let units = get("units");
    if !matches!(units.as_str(), Some("fahrenheit" | "celsius")) {
        return err("Choose Fahrenheit or Celsius.");
    }
    let refresh = get("refresh_minutes").as_i64().filter(|v| (15..=1440).contains(v));
    let Some(refresh) = refresh.filter(|_| get("refresh_minutes").as_number().is_some_and(|n| n.is_i64() || n.is_u64())) else {
        return err("The refresh interval must be between 15 and 1440 minutes.");
    };
    let week = get("week");
    if !matches!(week.as_str(), Some("upcoming" | "current")) {
        return err("Choose the upcoming or current Sunday–Saturday week.");
    }
    Ok(WeatherConfig {
        name: name_text.to_owned(),
        latitude: Num::from_json(&get("latitude")).unwrap_or(Num::Float(0.0)),
        longitude: Num::from_json(&get("longitude")).unwrap_or(Num::Float(0.0)),
        timezone: timezone.as_str().unwrap_or_default().to_owned(),
        units: units.as_str().unwrap_or_default().to_owned(),
        refresh_minutes: refresh,
        week: week.as_str().unwrap_or_default().to_owned(),
    })
}

pub fn validate_config(config: &WeatherConfig, require_location: bool) -> Result<WeatherConfig, WeatherError> {
    validate_map(&config.raw(), require_location).map(|_| config.clone())
}

fn read_json(path: &Path, limit: usize) -> Result<Value, ReadError> {
    let mut file = std::fs::File::open(path).map_err(ReadError::Io)?;
    let mut data = Vec::new();
    Read::by_ref(&mut file).take(limit as u64 + 1).read_to_end(&mut data).map_err(ReadError::Io)?;
    if data.len() > limit {
        return Err(ReadError::Invalid("The weather data file is too large.".into()));
    }
    finite_json(&data).map_err(ReadError::Invalid)
}

enum ReadError {
    Io(std::io::Error),
    Invalid(String),
}

/// `json.dump(value, ensure_ascii=False, allow_nan=False)` to a `.weather-*.tmp`
/// file in the same folder, fsync, then rename over `path`.
pub fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    if !parent.is_dir() {
        std::fs::create_dir_all(parent).map_err(|e| os_error(&e, Some(parent)))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let style = JsonStyle { ensure_ascii: false, ..JsonStyle::DEFAULT };
    let text = json_dumps(value, &style);
    let mut attempt = 0u32;
    let (temporary, mut file) = loop {
        let candidate = parent.join(format!(".weather-{}.tmp", random_suffix()));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&candidate) {
            Ok(file) => break (candidate, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 100 => attempt += 1,
            Err(e) => return Err(os_error(&e, Some(&candidate))),
        }
    };
    let result = file.write_all(text.as_bytes()).and_then(|_| file.flush()).and_then(|_| file.sync_all())
        .map_err(|e| os_error(&e, None))
        .and_then(|_| std::fs::rename(&temporary, path).map_err(|e| os_error(&e, Some(&temporary))));
    let _ = std::fs::remove_file(&temporary);
    result
}

fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut bytes = [0u8; 8];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        let _ = random.read_exact(&mut bytes);
    }
    bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

pub struct ConfigStore {
    pub path: PathBuf,
    error: Mutex<Option<String>>,
}

impl ConfigStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        let path = path.unwrap_or_else(|| xdg_directory("XDG_CONFIG_HOME", ".config").join("tessel-weather/config.json"));
        Self { path, error: Mutex::new(None) }
    }

    pub fn error(&self) -> Option<String> { self.error.lock().unwrap().clone() }

    fn set_error(&self, error: Option<String>) { *self.error.lock().unwrap() = error; }

    /// Return only an explicitly saved, valid location (never the default city).
    pub fn configured(&self) -> Option<WeatherConfig> {
        let value = read_json(&self.path, MAX_CONFIG_BYTES).ok()?;
        let map = value.as_object()?;
        if !["name", "latitude", "longitude"].iter().all(|k| map.contains_key(*k)) {
            return None;
        }
        let config = self.load();
        (self.error().is_none() && has_location(&config)).then_some(config)
    }

    pub fn load(&self) -> WeatherConfig {
        self.set_error(None);
        let failure = match read_json(&self.path, MAX_CONFIG_BYTES) {
            Err(ReadError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return WeatherConfig::default(),
            Err(ReadError::Io(e)) => os_error(&e, Some(&self.path)),
            Err(ReadError::Invalid(message)) => message,
            Ok(Value::Object(value)) => {
                let fields = ["name", "latitude", "longitude", "timezone", "units", "refresh_minutes", "week"];
                let mut values: Map<String, Value> = Map::new();
                for key in fields {
                    if let Some(item) = value.get(key) {
                        values.insert(key.into(), item.clone());
                    }
                }
                if ["name", "latitude", "longitude"].iter().all(|k| values.contains_key(*k)) {
                    values.entry("timezone").or_insert_with(|| Value::from("America/Denver"));
                    values.entry("units").or_insert_with(|| Value::from("fahrenheit"));
                } else {
                    // A file without a location never chose one; keep only preferences.
                    for key in ["name", "latitude", "longitude", "timezone"] {
                        values.remove(key);
                    }
                    values.entry("units").or_insert_with(|| Value::from("fahrenheit"));
                }
                let defaults = WeatherConfig::default();
                let mut complete = defaults.raw();
                for (key, item) in values {
                    complete.insert(key, item);
                }
                match validate_map(&complete, false) {
                    Ok(config) => return config,
                    Err(error) => error.0,
                }
            }
            Ok(_) => "Invalid weather configuration.".into(),
        };
        self.set_error(Some(failure));
        WeatherConfig::default()
    }

    pub fn save(&self, config: &WeatherConfig) -> Result<(), WeatherError> {
        validate_config(config, true)?;
        atomic_json(&self.path, &config.to_json())
            .map_err(|error| WeatherError(format!("Could not save weather settings: {error}")))?;
        self.set_error(None);
        Ok(())
    }
}

/// Seven chronological local dates, starting on Sunday (including today).
pub fn week_dates(today: Date, week: &str) -> Result<Vec<Date>, WeatherError> {
    if week != "upcoming" && week != "current" {
        return err("Unknown forecast week.");
    }
    let since_sunday = ((today.weekday() + 1) % 7) as i64;
    let offset = if week == "current" { -since_sunday } else { (-since_sunday).rem_euclid(7) };
    let start = today.add_days(offset);
    Ok((0..7).map(|i| start.add_days(i)).collect())
}

/// `urllib.parse.quote_plus`.
pub fn quote_plus(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(byte as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn urlencode(parameters: &[(&str, String)]) -> String {
    parameters.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v))).collect::<Vec<_>>().join("&")
}

pub fn forecast_url(config: &WeatherConfig) -> Result<String, WeatherError> {
    validate_config(config, true)?;
    let mut parameters = vec![
        ("latitude", config.latitude.text()), ("longitude", config.longitude.text()), ("timezone", config.timezone.clone()),
        ("current", "temperature_2m,weather_code,is_day,apparent_temperature,relative_humidity_2m,wind_speed_10m".into()),
        ("daily", "temperature_2m_min,temperature_2m_max,weather_code,precipitation_probability_max".into()),
        ("temperature_unit", config.units.clone()),
        ("wind_speed_unit", if config.units == "fahrenheit" { "mph" } else { "kmh" }.into()),
        ("forecast_days", "16".into()),
    ];
    if config.week == "current" {
        parameters.push(("past_days", "7".into()));
    }
    Ok(format!("{FORECAST_URL}?{}", urlencode(&parameters)))
}

pub type Fetch = Arc<dyn Fn(&str) -> Result<Value, WeatherError> + Send + Sync>;

/// The real HTTPS fetch (`fetch_json`).
pub fn fetch_json(url: &str) -> Result<Value, WeatherError> {
    let data = crate::http::get(url, "Tessel-Weather/1.0", "application/json", Duration::from_secs(NETWORK_TIMEOUT),
                                MAX_RESPONSE_BYTES, false)
        .map_err(|e| match e {
            crate::http::HttpError::Status(code) => WeatherError(format!("The weather service is unavailable (HTTP {code}).")),
            crate::http::HttpError::Transport => WeatherError("Could not reach the weather service. Check your connection.".into()),
        })?;
    decode_fetched(&data)
}

/// Body checks of `fetch_json`: size, finite JSON, an object without `error`.
pub fn decode_fetched(data: &[u8]) -> Result<Value, WeatherError> {
    if data.len() > MAX_RESPONSE_BYTES {
        return err("The weather service returned too much data.");
    }
    let value = finite_json(data).map_err(|_| WeatherError("The weather service returned invalid data.".into()))?;
    if !value.is_object() || value.get("error").is_some_and(truthy) {
        return err("The weather service could not provide this forecast.");
    }
    Ok(value)
}

pub fn real_fetch() -> Fetch { Arc::new(fetch_json) }

fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(0.0)),
        other => json_dumps(other, &JsonStyle { ensure_ascii: false, ..JsonStyle::DEFAULT }),
    }
}

pub fn search_locations(query: &str, fetch: &Fetch) -> Result<Vec<Location>, WeatherError> {
    let query = query.trim();
    if query.chars().count() < 2 {
        return Ok(Vec::new());
    }
    if query.chars().count() > 100 || query.chars().any(|c| (c as u32) < 32) {
        return err("Enter a shorter city or postal code.");
    }
    let url = format!("{GEOCODING_URL}?{}", urlencode(&[("name", query.into()), ("count", "10".into()),
                                                        ("language", "en".into()), ("format", "json".into())]));
    let result = fetch(&url)?;
    let entries = match result.get("results") {
        None => return Ok(Vec::new()),
        Some(Value::Array(entries)) => entries.clone(),
        Some(_) => return err("The location service returned invalid data."),
    };
    let mut locations = Vec::new();
    for entry in entries.iter().take(10) {
        let Some(entry) = entry.as_object() else { continue };
        let mut parts: Vec<String> = Vec::new();
        for key in ["name", "admin1", "country"] {
            if let Some(value) = entry.get(key).filter(|v| truthy(v)) {
                let part = py_str(value).trim().to_owned();
                if !parts.contains(&part) {
                    parts.push(part);
                }
            }
        }
        let mut map = WeatherConfig::default().raw();
        map.insert("name".into(), Value::from(parts.join(", ")));
        for key in ["latitude", "longitude", "timezone"] {
            map.insert(key.into(), entry.get(key).cloned().unwrap_or(Value::Null));
        }
        let Ok(config) = validate_map(&map, true) else { continue };
        locations.push(Location { name: config.name, latitude: config.latitude, longitude: config.longitude,
                                  timezone: config.timezone });
    }
    Ok(locations)
}

fn incomplete() -> WeatherError { WeatherError("The weather service returned an incomplete forecast.".into()) }

pub fn decode_response(value: &Value, config: &WeatherConfig, now: f64, fetched_at: Option<f64>, stale: bool,
                       error: Option<String>) -> Result<WeatherSnapshot, WeatherError> {
    let current = value.get("current").ok_or_else(incomplete)?;
    let daily = value.get("daily").ok_or_else(incomplete)?;
    let current = current.as_object().ok_or_else(incomplete)?;
    let temperature = number(current.get("temperature_2m").ok_or_else(incomplete)?);
    let code = number(current.get("weather_code").ok_or_else(incomplete)?);
    let timestamp = current.get("time").ok_or_else(incomplete)?;
    let (Some(temperature), Some(code), Some(timestamp)) = (temperature, code, timestamp.as_str()) else {
        return Err(incomplete());
    };
    if code.fract() != 0.0 || !dates::valid_iso_datetime(timestamp) {
        return Err(incomplete());
    }
    let opt = |key: &str| current.get(key).and_then(number);
    let current = CurrentWeather {
        time: timestamp.to_owned(), temp: temperature, code: code as i64,
        is_day: current.get("is_day").map(truthy).unwrap_or(true),
        apparent: opt("apparent_temperature"), humidity: opt("relative_humidity_2m"), wind: opt("wind_speed_10m"),
    };
    let daily = daily.as_object().ok_or_else(incomplete)?;
    let dates_list = daily.get("time").ok_or_else(incomplete)?.as_array().ok_or_else(incomplete)?;
    if dates_list.is_empty() || dates_list.len() > 31 {
        return Err(incomplete());
    }
    let mut indices: Vec<(Date, usize)> = Vec::new();
    for (index, item) in dates_list.iter().enumerate() {
        let date = item.as_str().and_then(Date::from_iso).ok_or_else(incomplete)?;
        indices.retain(|(known, _)| *known != date);
        indices.push((date, index));
    }
    let today = dates::local_date(now, &config.timezone).ok_or_else(incomplete)?;
    let number_at = |field: &str, index: usize| -> Option<f64> {
        daily.get(field).and_then(Value::as_array).and_then(|values| values.get(index)).and_then(number)
    };
    let mut days = Vec::new();
    for day in week_dates(today, &config.week).map_err(|_| incomplete())? {
        let Some(&(_, index)) = indices.iter().find(|(date, _)| *date == day) else {
            days.push(ForecastDay::empty(day, day < today));
            continue;
        };
        let code = number_at("weather_code", index);
        let probability = number_at("precipitation_probability_max", index).filter(|p| (0.0..=100.0).contains(p));
        days.push(ForecastDay {
            date: day, temp_min: number_at("temperature_2m_min", index), temp_max: number_at("temperature_2m_max", index),
            code: code.filter(|c| c.fract() == 0.0).map(|c| c as i64), precip_probability: probability, past: day < today,
        });
    }
    Ok(WeatherSnapshot { config: config.clone(), current: Some(current), days, fetched_at, stale, error })
}

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn system_clock() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Synchronous worker API with cross-process locking and last-good fallback.
pub struct WeatherClient {
    pub store: Arc<ConfigStore>,
    pub cache_dir: PathBuf,
    pub fetch: Fetch,
    pub clock: Clock,
}

fn entry_error(entry: &Map<String, Value>) -> Option<String> {
    match entry.get("error") {
        Some(value) if truthy(value) => Some(py_str(value)),
        _ => None,
    }
}

impl WeatherClient {
    pub fn new(store: Arc<ConfigStore>, cache_dir: Option<PathBuf>, fetch: Option<Fetch>, clock: Option<Clock>) -> Self {
        Self { store, cache_dir: cache_dir.unwrap_or_else(|| xdg_directory("XDG_CACHE_HOME", ".cache").join("tessel-weather")),
               fetch: fetch.unwrap_or_else(real_fetch), clock: clock.unwrap_or_else(|| Arc::new(system_clock)) }
    }

    /// sha256 of `json.dumps(asdict(config), sort_keys=True)`.
    pub fn key(config: &WeatherConfig) -> String {
        let text = json_dumps(&config.to_json(), &JsonStyle { sort_keys: true, ..JsonStyle::DEFAULT });
        glib::compute_checksum_for_string(glib::ChecksumType::Sha256, &text).map(|s| s.to_string()).unwrap_or_default()
    }

    pub fn cache_path(&self, config: &WeatherConfig) -> PathBuf { self.cache_dir.join(format!("{}.json", Self::key(config))) }

    fn cached(&self, path: &Path, config: &WeatherConfig) -> Map<String, Value> {
        let Ok(Value::Object(value)) = read_json(path, MAX_RESPONSE_BYTES + MAX_CONFIG_BYTES) else { return Map::new() };
        if !value.get("version").is_some_and(|v| py_eq(v, &Value::from(1)))
            || !value.get("config").is_some_and(|c| py_eq(c, &config.to_json())) {
            return Map::new();
        }
        for name in ["fetched_at", "attempted_at"] {
            match value.get(name) {
                None | Some(Value::Null) => {}
                Some(stamp) => match number(stamp) {
                    Some(v) if (0.0..=MAX_TIMESTAMP).contains(&v) => {}
                    _ => return Map::new(),
                },
            }
        }
        if value.get("fetched_at").and_then(number).is_some() {
            if let Some(response) = value.get("response") {
                if decode_response(response, config, (self.clock)(), None, false, None).is_err() {
                    return Map::new();
                }
            }
        }
        value
    }

    fn fresh(entry: &Map<String, Value>, config: &WeatherConfig, now: f64) -> bool {
        let Some(fetched) = entry.get("fetched_at").and_then(number) else { return false };
        let age = now - fetched;
        (0.0..config.refresh_minutes as f64 * 60.0).contains(&age)
            && dates::local_date(fetched, &config.timezone).is_some()
            && dates::local_date(fetched, &config.timezone) == dates::local_date(now, &config.timezone)
    }

    fn snapshot(&self, entry: &Map<String, Value>, config: &WeatherConfig, now: f64, error: Option<String>)
                -> Result<WeatherSnapshot, WeatherError> {
        let fetched = entry.get("fetched_at").and_then(number);
        let error = error.or_else(|| entry_error(entry));
        if let (Some(fetched), Some(response)) = (fetched, entry.get("response")) {
            if (0.0..=MAX_STALE_SECONDS).contains(&(now - fetched)) {
                let stale = !Self::fresh(entry, config, now) || error.is_some();
                return decode_response(response, config, now, Some(fetched), stale, error);
            }
        }
        let today = dates::local_date(now, &config.timezone).ok_or_else(|| WeatherError("Invalid time.".into()))?;
        let days = week_dates(today, &config.week)?.into_iter().map(|day| ForecastDay::empty(day, day < today)).collect();
        Ok(WeatherSnapshot { config: config.clone(), current: None, days, fetched_at: None, stale: false, error })
    }

    pub fn read(&self, config: Option<&WeatherConfig>, force: bool) -> Result<WeatherSnapshot, WeatherError> {
        let config = match config {
            Some(config) => validate_config(config, false)?,
            None => validate_config(&self.store.load(), false)?,
        };
        if !has_location(&config) {
            // Never fetch a forecast for a placeholder location.
            return Ok(WeatherSnapshot { config, current: None, days: Vec::new(), fetched_at: None, stale: false,
                                        error: Some(NO_LOCATION.into()) });
        }
        let started = (self.clock)();
        let path = self.cache_path(&config);
        let mut entry = self.cached(&path, &config);
        let attempted = entry.get("attempted_at").and_then(number);
        let retrying = |attempted: Option<f64>, now: f64| attempted.is_some_and(|a| (0.0..RETRY_SECONDS).contains(&(now - a)));
        if !force && (Self::fresh(&entry, &config, started) || retrying(attempted, started)) {
            return self.snapshot(&entry, &config, started, None);
        }
        // Ok(Err(..)) propagates like a Python WeatherError; Err(text) is an
        // OSError, reported as an unavailable cache with the last good data.
        let result: Result<Result<WeatherSnapshot, WeatherError>, String> = (|| {
            if !self.cache_dir.is_dir() {
                std::fs::create_dir_all(&self.cache_dir).map_err(|e| os_error(&e, Some(&self.cache_dir)))?;
                let _ = std::fs::set_permissions(&self.cache_dir, std::fs::Permissions::from_mode(0o700));
            }
            let lock_path = self.cache_dir.join("fetch.lock");
            let lock = std::fs::OpenOptions::new().append(true).create(true).open(&lock_path)
                .map_err(|e| os_error(&e, Some(&lock_path)))?;
            let deadline = Instant::now() + Duration::from_secs(NETWORK_TIMEOUT + 2);
            loop {
                use std::os::fd::AsRawFd;
                // SAFETY: flock on an fd we own.
                if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                    return Err(os_error(&error, None));
                }
                if entry.get("response").is_some_and(truthy) || Instant::now() >= deadline {
                    return Ok(self.snapshot(&entry, &config, started, Some("Weather refresh is in progress.".into())));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // Another app/process may have completed this exact refresh (the lock
            // is released when `lock` is dropped).
            entry = self.cached(&path, &config);
            let now = (self.clock)();
            let fetched = entry.get("fetched_at").and_then(number);
            let attempted = entry.get("attempted_at").and_then(number);
            if (Self::fresh(&entry, &config, now) && (!force || fetched.is_some_and(|f| f >= started)))
                || (!force && retrying(attempted, now)) {
                return Ok(self.snapshot(&entry, &config, now, None));
            }
            let outcome = forecast_url(&config).and_then(|url| (self.fetch)(&url)).and_then(|response| {
                let finished = (self.clock)();
                decode_response(&response, &config, finished, None, false, None)?;
                Ok((response, finished))
            });
            entry = match outcome {
                Ok((response, finished)) => {
                    let mut fresh = Map::new();
                    fresh.insert("version".into(), Value::from(1));
                    fresh.insert("config".into(), config.to_json());
                    fresh.insert("response".into(), response);
                    fresh.insert("fetched_at".into(), Value::from(finished));
                    fresh.insert("attempted_at".into(), Value::from(finished));
                    fresh.insert("error".into(), Value::Null);
                    fresh
                }
                Err(error) => {
                    let mut failed = entry.clone();
                    failed.insert("version".into(), Value::from(1));
                    failed.insert("config".into(), config.to_json());
                    failed.insert("attempted_at".into(), Value::from((self.clock)()));
                    failed.insert("error".into(), Value::from(error.0));
                    failed
                }
            };
            atomic_json(&path, &Value::Object(entry.clone()))?;
            self.prune(&path);
            drop(lock);
            Ok(self.snapshot(&entry, &config, (self.clock)(), None))
        })();
        match result {
            Ok(snapshot) => snapshot,
            Err(error) => self.snapshot(&entry, &config, (self.clock)(), Some(format!("Weather cache is unavailable: {error}"))),
        }
    }

    fn prune(&self, current: &Path) {
        let Ok(listing) = std::fs::read_dir(&self.cache_dir) else { return };
        let mut others: Vec<(i128, PathBuf)> = Vec::new();
        for item in listing.flatten() {
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".json") else { continue };
            if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) || path == current {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            use std::os::unix::fs::MetadataExt;
            others.push((meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128, path));
        }
        others.sort();
        let excess = others.len().saturating_sub(MAX_CACHE_FILES - 1);
        for (_, path) in others.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Seconds until a snapshot's forecast should next be read (cache TTL, retry, or new local day).
pub fn refresh_delay(snapshot: &WeatherSnapshot, now: f64) -> Option<i64> {
    let config = &snapshot.config;
    if !has_location(config) {
        return None;
    }
    let fetched = snapshot.fetched_at.filter(|f| f.is_finite());
    let mut delay = match fetched {
        Some(fetched) if snapshot.error.is_none() && !snapshot.stale => {
            config.refresh_minutes as f64 * 60.0 - (now - fetched).max(0.0)
        }
        _ => RETRY_SECONDS,
    };
    if let Some(local) = dates::local_date(now, &config.timezone) {
        if let Some(midnight) = dates::midnight(local.add_days(1), &config.timezone) {
            delay = delay.min(midnight - now + 1.0);
        }
    }
    Some((delay.ceil() as i64).min(1440 * 60).max(5))
}

pub fn weather_description(code: Option<i64>) -> &'static str {
    match code {
        Some(0) => "Clear",
        Some(1) => "Mostly clear",
        Some(2) => "Partly cloudy",
        Some(3) => "Overcast",
        Some(45 | 48) => "Fog",
        Some(51 | 53 | 55) => "Drizzle",
        Some(56 | 57) => "Freezing drizzle",
        Some(61 | 63 | 65) => "Rain",
        Some(66 | 67) => "Freezing rain",
        Some(71 | 73 | 75 | 77) => "Snow",
        Some(80..=82) => "Rain showers",
        Some(85 | 86) => "Snow showers",
        Some(95 | 96 | 99) => "Thunderstorms",
        _ => "Unavailable",
    }
}

pub fn weather_icon(code: Option<i64>, is_day: bool) -> &'static str {
    match code {
        Some(0 | 1) => if is_day { "weather-clear-symbolic" } else { "weather-clear-night-symbolic" },
        Some(2) => if is_day { "weather-few-clouds-symbolic" } else { "weather-few-clouds-night-symbolic" },
        Some(3) => "weather-overcast-symbolic",
        Some(45 | 48) => "weather-fog-symbolic",
        Some(71 | 73 | 75 | 77 | 85 | 86) => "weather-snow-symbolic",
        Some(95 | 96 | 99) => "weather-storm-symbolic",
        Some(51 | 53 | 55 | 56 | 57 | 61 | 63 | 65 | 66 | 67 | 80 | 81 | 82) => "weather-showers-symbolic",
        _ => "weather-severe-alert-symbolic",
    }
}

/// `"—"` or `f"{round(value):d}°F"` (Python rounds half to even).
pub fn format_temperature(value: Option<f64>, units: &str) -> String {
    match value {
        None => "—".into(),
        Some(value) => format!("{}°{}", value.round_ties_even() as i64, if units == "fahrenheit" { 'F' } else { 'C' }),
    }
}
