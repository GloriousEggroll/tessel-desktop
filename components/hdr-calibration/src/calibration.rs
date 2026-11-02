//! calibration.py: HDR setup storage and transactional labwc integration (no
//! GUI dependency). Everything that touches the running session goes through
//! [`Host`], so tests (and the headless measurement) fake the compositor.

use crate::compat::{self, etree};
use crate::correction;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use way_displays::color_config::{self, ColorError, Status, Transaction};

pub const BEGIN: &str = "<!-- tessel-hdr-calibration begin -->";
pub const END: &str = "<!-- tessel-hdr-calibration end -->";
static BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("(?s){}.*?{}", regex::escape(BEGIN), regex::escape(END))).unwrap());
static CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)</(?:labwc_config|openbox_config)\s*>").unwrap());
static SELF_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)/\s*>").unwrap());
/// `(?=(?:\s|<!--.*?-->)*$)` (Python `$` also matches before a final newline).
static ONLY_TRAILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\A(?:\s|<!--.*?-->)*\n?\z").unwrap());
static HEX16: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9a-f]{16}\z").unwrap());
static HEX64: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9a-f]{64}\z").unwrap());

/// `(key, English label, white target)`.
pub const PRESETS: &[(&str, &str, Option<f64>)] = &[
    ("custom", "Custom HDR setup", None),
    ("sdr-video", "SDR video — 100-nit white", Some(100.0)),
    ("sdr-photo", "SDR photo — 120-nit white", Some(120.0)),
];
pub const FIELDS: [&str; 4] = ["black", "peak", "fullFrame", "white"];
pub const STATUS_FIELDS: [&str; 4] = ["hdr_target_black_nits", "hdr_target_peak_nits", "hdr_target_full_frame_nits", "hdr_reference_white_nits"];

pub type Profile = Map<String, Value>;

/// Python exception classes the callers distinguish.
#[derive(Debug, Clone, PartialEq)]
pub enum CalError {
    Value(String),
    Os(String),
    Runtime(String),
}

impl std::fmt::Display for CalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalError::Value(t) | CalError::Os(t) | CalError::Runtime(t) => f.write_str(t),
        }
    }
}

impl From<ColorError> for CalError {
    fn from(error: ColorError) -> Self {
        match error {
            ColorError::Value(t) => CalError::Value(t),
            ColorError::Os(t) => CalError::Os(t),
            ColorError::Runtime(t) => CalError::Runtime(t),
        }
    }
}

fn value_error(text: &str) -> CalError {
    CalError::Value(text.into())
}

/// Python `os.environ.get(name, Path.home() / fallback)`.
fn env_dir(name: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(name) {
        Some(value) => PathBuf::from(value),
        None => tessel_core::paths::home().join(fallback),
    }
}

pub fn config_home() -> PathBuf {
    env_dir("XDG_CONFIG_HOME", ".config")
}

pub fn state_home() -> PathBuf {
    env_dir("XDG_STATE_HOME", ".local/state")
}

/// The running session, as calibration.py sees it. Tests and the measurement fake it.
pub trait Host: Send + Sync {
    /// `color_config.read_status()`.
    fn read_status(&self) -> Status;
    /// `calibration.session()`.
    fn session(&self) -> Result<Status, CalError> {
        session_from(self.read_status())
    }
    /// `os.kill(pid, sig)`; `Err(ESRCH)` is Python's ProcessLookupError.
    fn signal(&self, pid: i32, signal: i32) -> std::io::Result<()>;
    fn cmdline(&self, pid: i64) -> std::io::Result<Vec<u8>>;
    fn cwd(&self, pid: i64) -> PathBuf;
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The live session: status keyfile, /proc and real signals.
pub struct RealHost;

impl Host for RealHost {
    fn read_status(&self) -> Status {
        color_config::read_status_file(color_config::status_path().as_deref()).unwrap_or_default()
    }

    fn signal(&self, pid: i32, signal: i32) -> std::io::Result<()> {
        if unsafe { libc::kill(pid, signal) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }

    fn cmdline(&self, pid: i64) -> std::io::Result<Vec<u8>> {
        std::fs::read(format!("/proc/{pid}/cmdline"))
    }

    fn cwd(&self, pid: i64) -> PathBuf {
        compat::resolve(Path::new(&format!("/proc/{pid}/cwd")))
    }
}

fn py_float_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => py_float_text(s),
        _ => None,
    }
}

/// Python `float(str)`.
pub fn py_float_text(text: &str) -> Option<f64> {
    let cleaned = text.trim();
    if cleaned.is_empty() || cleaned.starts_with('_') || cleaned.ends_with('_') || cleaned.contains("__") {
        return None;
    }
    cleaned.replace('_', "").parse::<f64>().ok()
}

fn float_of(text: &str) -> Result<f64, CalError> {
    py_float_text(text).ok_or_else(|| CalError::Value(format!("could not convert string to float: {}", py_repr(text))))
}

fn py_repr(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn field<'a>(profile: &'a Profile, key: &str) -> Option<&'a str> {
    profile.get(key).and_then(Value::as_str)
}

fn is_one(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::Bool(b)) => *b,
        _ => false,
    }
}

pub fn preset_white(key: &str) -> Option<Option<f64>> {
    PRESETS.iter().find(|(k, _, _)| *k == key).map(|(_, _, white)| *white)
}

pub fn preset_label(key: &str) -> &'static str {
    PRESETS.iter().find(|(k, _, _)| *k == key).map(|(_, label, _)| *label).unwrap_or("Custom HDR setup")
}

/// `validate(profile)`: the normalised profile (preset added, values as floats).
pub fn validate(profile: &Profile) -> Result<Profile, CalError> {
    if !is_one(profile.get("version")) {
        return Err(value_error("Unsupported HDR setup profile version."));
    }
    let identity = field(profile, "displayId").unwrap_or("");
    if !HEX16.is_match(identity) || u64::from_str_radix(identity, 16).unwrap_or(0) == 0 {
        return Err(value_error("The monitor must provide a stable EDID identity."));
    }
    if field(profile, "connector").is_none_or(str::is_empty) {
        return Err(value_error("Choose a connected monitor."));
    }
    let mode = field(profile, "mode");
    if mode.is_none_or(|m| !(1..=100).contains(&m.trim().chars().count())) {
        return Err(value_error("Enter the monitor’s picture mode (up to 100 characters)."));
    }
    let method = field(profile, "method");
    if !matches!(method, Some("visual") | Some("meter-readings")) {
        return Err(value_error("Choose visual estimates or measured luminance readings."));
    }
    if method == Some("meter-readings") && field(profile, "instrument").unwrap_or("").trim().is_empty() {
        return Err(value_error("Record the instrument used for the measured readings."));
    }
    let mut values = [0.0; 4];
    for (slot, key) in values.iter_mut().zip(FIELDS) {
        *slot = profile.get(key).and_then(py_float_value).ok_or_else(|| value_error("Enter all four luminance values."))?;
    }
    let [black, peak, full, white] = values;
    if !values.iter().all(|v| v.is_finite())
        || !(0.0 <= black && black < white && (20.0..=500.0).contains(&white) && (40.0..=10000.0).contains(&peak) && white <= full && full <= peak) {
        return Err(value_error("Use 20–500 nits for SDR white, 40–10,000 for peak, black below white, and white ≤ full-screen ≤ peak."));
    }
    let preset = match profile.get("preset") {
        None => "custom".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => String::new(),
    };
    match preset_white(&preset) {
        Some(Some(target)) if white != target => return Err(value_error(
            "The selected editing preset does not match its white target. Choose Custom to change it.")),
        None => return Err(value_error("The selected editing preset does not match its white target. Choose Custom to change it.")),
        _ => {}
    }
    match profile.get("correction") {
        None | Some(Value::Null) => {}
        Some(Value::Object(lut)) => {
            let path_ok = lut.get("path").and_then(Value::as_str).is_some_and(|p| Path::new(p).is_absolute());
            let sha_ok = lut.get("sha256").and_then(Value::as_str).is_some_and(|s| HEX64.is_match(s));
            if lut.get("encoding").and_then(Value::as_str) != Some("BT2020-PQ") || !path_ok || !sha_ok {
                return Err(value_error("Invalid HDR output correction reference."));
            }
        }
        Some(_) => return Err(value_error("Invalid HDR output correction reference.")),
    }
    if mode.unwrap().contains(['\n', '\r', '\0']) {
        return Err(value_error("The picture mode must be a single line."));
    }
    let mut result = profile.clone();
    result.insert("preset".into(), json!(preset));
    for (key, value) in FIELDS.iter().zip(values) {
        result.insert((*key).into(), json!(value));
    }
    Ok(result)
}

pub fn profiles_path() -> PathBuf {
    config_home().join("tessel-hdr-calibration/profiles.json")
}

pub fn read_profiles() -> Result<Vec<Profile>, CalError> {
    let path = profiles_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(CalError::Os(tessel_core::pyerr::os_error_path(&e, &path))),
    };
    let data: Value = serde_json::from_str(&data).map_err(|e| CalError::Value(e.to_string()))?;
    let profiles = match (data.get("version"), data.get("profiles")) {
        (version, Some(Value::Array(list))) if is_one(version) => list,
        _ => return Err(value_error("Unsupported HDR profile store.")),
    };
    profiles.iter().map(|p| match p {
        Value::Object(map) => validate(map),
        _ => Err(value_error("Unsupported HDR setup profile version.")),
    }).collect()
}

pub fn json_text(value: &Value) -> String {
    let style = tessel_core::pyfmt::JsonStyle { indent: Some(2), item_separator: ",", ..tessel_core::pyfmt::JsonStyle::DEFAULT };
    tessel_core::pyfmt::json_dumps(value, &style) + "\n"
}

fn identity_of(profile: &Profile) -> (String, String, String, String) {
    let get = |k: &str| field(profile, k).unwrap_or("").to_owned();
    (get("displayId"), get("connector"), get("mode"), field(profile, "preset").unwrap_or("custom").to_owned())
}

pub fn save_profile(profile: &Profile) -> Result<(), CalError> {
    let profile = validate(profile)?;
    let path = profiles_path();
    let _lock = compat::locked(&path).map_err(CalError::Os)?;
    let key = identity_of(&profile);
    let mut saved: Vec<Value> = read_profiles()?.into_iter().filter(|p| identity_of(p) != key).map(Value::Object).collect();
    let mut entry = profile.clone();
    entry.insert("saved".into(), json!(compat::iso_utc_now()));
    saved.push(Value::Object(entry));
    color_config::atomic_write(&path, json_text(&json!({"version": 1, "profiles": saved})).as_bytes(), 0o600)?;
    Ok(())
}

fn session_from(status: Status) -> Result<Status, CalError> {
    if status.get("compositor").and_then(|c| c.get("hdr_calibration_supported")).map(String::as_str) != Some("true") {
        return Err(value_error("Start a session with the updated labwc compositor to use HDR calibration."));
    }
    Ok(status)
}

pub type Section = std::collections::BTreeMap<String, String>;

pub fn check_monitor(status: &Status, connector: &str, identity: Option<&str>) -> Result<Section, CalError> {
    let monitor = status.get(connector).cloned().unwrap_or_default();
    let get = |k: &str| monitor.get(k).map(String::as_str);
    if get("enabled") != Some("true") || get("hdr") != Some("true") || get("bit_depth") != Some("10") {
        return Err(value_error("Enable HDR on this monitor in Displays first. A 10-bit HDR output is required."));
    }
    let actual = get("display_id").unwrap_or("");
    if !HEX16.is_match(actual) || u64::from_str_radix(actual, 16).unwrap_or(0) == 0 {
        return Err(value_error("This output has no stable monitor identity; automatic profile application is unavailable."));
    }
    if let Some(identity) = identity.filter(|i| !i.is_empty()) {
        if actual != identity {
            return Err(value_error("A different monitor is now connected. Select it and start a new setup."));
        }
    }
    Ok(monitor)
}

fn compositor_pid(status: &Status) -> Result<i64, CalError> {
    let pid = status.get("compositor").and_then(|c| c.get("pid"))
        .ok_or_else(|| CalError::Value("KeyError: 'pid'".into()))?;
    tessel_core::pyjson::py_int(Some(&json!(pid))).ok_or_else(|| CalError::Value(format!("invalid literal for int() with base 10: {}", py_repr(pid))))
}

/// The rc.xml labwc was started with (`-c`, `-C`, else `$XDG_CONFIG_HOME/labwc/rc.xml`).
pub fn config_path(status: &Status, host: &dyn Host) -> Result<PathBuf, CalError> {
    let pid = compositor_pid(status)?;
    let bytes = host.cmdline(pid).map_err(|e| CalError::Os(tessel_core::pyerr::os_error_path(&e, Path::new(&format!("/proc/{pid}/cmdline")))))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| CalError::Value(e.to_string()))?;
    let args: Vec<&str> = text.split('\0').collect();
    let name = Path::new(args[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name != "labwc" || args.contains(&"--merge-config") || args.contains(&"-m") {
        return Err(value_error("Cannot determine a single labwc configuration for this session."));
    }
    let mut folder = config_home().join("labwc");
    let mut config_file: Option<PathBuf> = None;
    for (i, arg) in args.iter().enumerate() {
        let next = args.get(i + 1);
        if (*arg == "-c" || *arg == "--config") && next.is_some() {
            config_file = Some(PathBuf::from(next.unwrap()));
        } else if let Some(value) = arg.strip_prefix("--config=") {
            config_file = Some(PathBuf::from(value));
        } else if arg.starts_with("-c") && !arg.starts_with("--") && arg.len() > 2 {
            config_file = Some(PathBuf::from(&arg[2..]));
        } else if (*arg == "-C" || *arg == "--config-dir") && next.is_some() {
            folder = PathBuf::from(next.unwrap());
        } else if let Some(value) = arg.strip_prefix("--config-dir=") {
            folder = PathBuf::from(value);
        } else if arg.starts_with("-C") && arg.len() > 2 {
            folder = PathBuf::from(&arg[2..]);
        }
    }
    let path = expanduser(&config_file.unwrap_or_else(|| folder.join("rc.xml")));
    Ok(if path.is_absolute() { path } else { host.cwd(pid).join(path) })
}

fn expanduser(path: &Path) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix('~')) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(format!("{}{rest}", tessel_core::paths::home().display()))
        }
        _ => path.to_path_buf(),
    }
}

fn trailing_only(text: &str, from: usize) -> bool {
    ONLY_TRAILING.is_match(&text[from..])
}

/// ElementTree `_value`: case-insensitive attribute, overridden by a child element.
fn element_value(node: &etree::Element, name: &str) -> String {
    let lower = name.to_lowercase();
    let mut value = node.attrib.iter().find(|(k, _)| k.to_lowercase() == lower).map(|(_, v)| v.clone()).unwrap_or_default();
    for child in &node.children {
        if child.tag.rsplit('}').next().unwrap_or("").to_lowercase() == lower {
            value = child.text.clone().unwrap_or_default();
        }
    }
    value
}

/// `format(value, '.9g')`.
pub fn g9(value: f64) -> String {
    tessel_core::pyfmt::format_value(&tessel_core::pyfmt::Arg::Float(value), ".9g").unwrap_or_else(|_| value.to_string())
}

/// Merge one monitor into the managed block, under the colour controls' lock.
pub fn write_config(path: &Path, connector: &str, profile: Option<&Profile>) -> Result<Transaction, CalError> {
    let profile = match profile {
        Some(profile) => {
            let profile = validate(profile)?;
            if field(&profile, "connector") != Some(connector) {
                return Err(value_error("The profile belongs to another connector."));
            }
            Some(profile)
        }
        None => None,
    };
    let path = compat::resolve(path);
    let _lock = compat::locked(&path).map_err(CalError::Os)?;
    let existed = path.exists();
    let before = if existed {
        std::fs::read(&path).map_err(|e| CalError::Os(tessel_core::pyerr::os_error_path(&e, &path)))?
    } else {
        b"<labwc_config>\n</labwc_config>\n".to_vec()
    };
    let mode = if existed {
        std::fs::metadata(&path).map_err(|e| CalError::Os(tessel_core::pyerr::os_error_path(&e, &path)))?.permissions().mode() & 0o7777
    } else {
        0o600
    };
    let mut text = String::from_utf8(before.clone()).map_err(|e| CalError::Value(format!("'utf-8' codec can't decode bytes: {e}")))?;
    color_config::parse_colors(&text)?;
    let matches: Vec<String> = BLOCK.find_iter(&text).map(|m| m.as_str().to_owned()).collect();
    if matches.len() > 1 || text.matches(BEGIN).count() != matches.len() || text.matches(END).count() != matches.len() {
        return Err(value_error("The managed HDR calibration block is malformed."));
    }
    let mut section = match matches.first() {
        Some(block) => etree::fromstring(&block.replace(BEGIN, "").replace(END, ""))
            .map_err(|e| CalError::Value(format!("The labwc configuration is not valid XML: {e}")))?,
        None => etree::Element::new("hdrCalibration"),
    };
    section.children.retain(|node| element_value(node, "name") != connector);
    match &profile {
        Some(profile) => {
            let number = |k: &str| g9(profile[k].as_f64().unwrap_or(0.0));
            let mut attrs: Vec<(String, String)> = vec![
                ("name".into(), connector.into()), ("displayId".into(), field(profile, "displayId").unwrap_or("").into())];
            for key in FIELDS {
                attrs.push((key.into(), number(key)));
            }
            attrs.push(("pictureMode".into(), field(profile, "mode").unwrap_or("").into()));
            if let Some(Value::Object(lut)) = profile.get("correction") {
                attrs.push(("correction".into(), lut.get("path").and_then(Value::as_str).unwrap_or("").into()));
                attrs.push(("correctionSha256".into(), lut.get("sha256").and_then(Value::as_str).unwrap_or("").into()));
            }
            let mut element = etree::Element::new("output");
            element.attrib = attrs;
            section.children.push(element);
        }
        None => {
            // Explicit reset also supersedes any earlier hand-written section.
            section.sub_element("output", &[("name", connector), ("enabled", "false")]);
        }
    }
    section.tail = None;
    etree::indent(&mut section, "  ");
    let block = format!("{BEGIN}\n{}\n{END}\n", etree::tostring(&section));
    text = BLOCK.replace_all(&text, "").into_owned();
    let find_close = |text: &str| CLOSE.find_iter(text).find(|m| trailing_only(text, m.end())).map(|m| m.start());
    let close = match find_close(&text) {
        Some(close) => close,
        None => {
            let root = etree::fromstring(&text).map_err(|e| CalError::Value(format!("The labwc configuration is not valid XML: {e}")))?;
            let positions: Vec<(usize, usize)> = SELF_CLOSE.find_iter(&text)
                .filter(|m| trailing_only(&text, m.end())).map(|m| (m.start(), m.end())).collect();
            if !root.children.is_empty() || positions.is_empty() {
                return Err(value_error("Could not locate the labwc configuration root."));
            }
            let mut replaced = String::new();
            let mut offset = 0;
            for (start, end) in positions {
                replaced.push_str(&text[offset..start]);
                replaced.push_str(&format!(">\n</{}>\n", root.tag));
                offset = end;
            }
            replaced.push_str(&text[offset..]);
            text = replaced;
            find_close(&text).ok_or_else(|| value_error("Could not locate the labwc configuration root."))?
        }
    };
    let after = format!("{}{block}{}", &text[..close], &text[close..]).into_bytes();
    color_config::parse_colors(std::str::from_utf8(&after).unwrap())?;
    let backup = state_home().join("tessel/backups/hdr").join(format!("{}-rc.xml", compat::time_ns()));
    color_config::atomic_write(&backup, &before, 0o600)?;
    color_config::atomic_write(&path, &after, mode)?;
    Ok(Transaction { path, before, after, mode, existed, backup })
}

/// `math.isclose(a, b, rel_tol, abs_tol)`.
fn isclose(a: f64, b: f64, rel: f64, abs: f64) -> bool {
    if a == b {
        return true;
    }
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
    (a - b).abs() <= (rel * a.abs().max(b.abs())).max(abs)
}

/// Whether the compositor reports `profile` (or the reset state for `None`) as committed.
pub fn matches(monitor: &Section, profile: Option<&Profile>) -> Result<bool, CalError> {
    let get = |k: &str| monitor.get(k).map(String::as_str);
    let expected = profile.and_then(|p| p.get("correction")).and_then(Value::as_object);
    if get("hdr_correction_pending") == Some("true") || get("hdr_correction_error").is_some_and(|e| !e.is_empty()) {
        return Ok(false);
    }
    match expected.filter(|e| !e.is_empty()) {
        Some(expected) => {
            if get("hdr_correction_active") != Some("true")
                || get("hdr_correction_sha256") != expected.get("sha256").and_then(Value::as_str)
                || get("hdr_correction_path") != expected.get("path").and_then(Value::as_str) {
                return Ok(false);
            }
        }
        None => {
            if get("hdr_correction_active") == Some("true") || get("hdr_correction_path").is_some_and(|p| !p.is_empty()) {
                return Ok(false);
            }
        }
    }
    let Some(profile) = profile else {
        let white = float_of(get("hdr_reference_white_nits").unwrap_or("0"))?;
        return Ok(get("hdr_calibration_active") == Some("false") && isclose(white, 203.0, 0.0, 0.001));
    };
    if get("hdr_calibration_active") != Some("true") {
        return Ok(false);
    }
    for (key, status_key) in FIELDS.iter().zip(STATUS_FIELDS) {
        let actual = float_of(get(status_key).unwrap_or("nan"))?;
        let wanted = profile.get(*key).and_then(py_float_value).unwrap_or(f64::NAN);
        if !isclose(actual, wanted, 1e-6, 1e-5) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Write, reload (SIGHUP) and wait up to 8 s for the compositor to commit; roll back on any failure.
pub fn apply(connector: &str, profile: Option<&Profile>, host: &dyn Host) -> Result<PathBuf, CalError> {
    let status = host.session()?;
    let profile = match profile {
        Some(profile) => {
            let profile = validate(profile)?;
            if let Some(Value::Object(lut)) = profile.get("correction") {
                if status.get("compositor").and_then(|c| c.get("hdr_correction_supported")).map(String::as_str) != Some("true") {
                    return Err(value_error("Start a session with HDR output correction support before applying this profile."));
                }
                let path = lut.get("path").and_then(Value::as_str).unwrap_or("");
                let (_, sha, _) = correction::read_cube(Path::new(path), field(&profile, "displayId").unwrap_or(""), field(&profile, "mode").unwrap_or(""))?;
                if Some(sha.as_str()) != lut.get("sha256").and_then(Value::as_str) {
                    return Err(value_error("The saved HDR correction file has changed. Import it again."));
                }
            }
            Some(profile)
        }
        None => None,
    };
    let monitor = check_monitor(&status, connector, profile.as_ref().and_then(|p| field(p, "displayId")))?;
    let pid = compositor_pid(&status)?;
    let transaction = write_config(&config_path(&status, host)?, connector, profile.as_ref())?;
    let attempt = || -> Result<PathBuf, CalError> {
        host.signal(pid as i32, libc::SIGHUP).map_err(|e| CalError::Os(tessel_core::pyerr::os_error_text(&e)))?;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let current = host.session()?;
            if compositor_pid(&current)? != pid {
                return Err(value_error("The compositor session changed during calibration."));
            }
            let active = check_monitor(&current, connector, monitor.get("display_id").map(String::as_str))?;
            if let Some(error) = active.get("hdr_correction_error").filter(|e| !e.is_empty()) {
                return Err(CalError::Value(error.clone()));
            }
            if matches(&active, profile.as_ref())? {
                if let Some(profile) = &profile {
                    save_profile(profile)?;
                }
                return Ok(transaction.backup.clone());
            }
            host.sleep(Duration::from_millis(100));
        }
        Err(value_error("The compositor did not apply the HDR setup. The previous configuration has been restored."))
    };
    match attempt() {
        Ok(backup) => Ok(backup),
        Err(error) => {
            color_config::rollback(&transaction)?;
            match host.signal(pid as i32, libc::SIGHUP) {
                Err(e) if e.raw_os_error() != Some(libc::ESRCH) => return Err(CalError::Os(tessel_core::pyerr::os_error_text(&e))),
                _ => {}
            }
            Err(error)
        }
    }
}

fn section_json(section: &Section) -> Value {
    Value::Object(section.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

/// Record the active software configuration; never claims physical validation.
pub fn workspace_report(connector: &str, profile: &Profile, host: &dyn Host) -> Result<PathBuf, CalError> {
    let profile = validate(profile)?;
    let status = host.session()?;
    let monitor = check_monitor(&status, connector, field(&profile, "displayId"))?;
    let matches_active = matches(&monitor, Some(&profile))?;
    let file_matches = match profile.get("correction") {
        Some(Value::Object(lut)) if !lut.is_empty() => {
            let path = lut.get("path").and_then(Value::as_str).unwrap_or("");
            let (_, sha, _) = correction::read_cube(Path::new(path), field(&profile, "displayId").unwrap_or(""), field(&profile, "mode").unwrap_or(""))?;
            json!(Some(sha.as_str()) == lut.get("sha256").and_then(Value::as_str))
        }
        _ => Value::Null,
    };
    let report = json!({
        "version": 1, "created": compat::iso_utc_now(), "connector": connector, "displayId": profile["displayId"],
        "pictureMode": profile["mode"], "requested": profile, "compositor": section_json(&status.get("compositor").cloned().unwrap_or_default()),
        "active": section_json(&monitor), "software_configuration_matches": matches_active,
        "correction_file_matches": file_matches, "meter_verification": "not performed",
        "application_preview_verification": "not performed", "measured_luminance": null, "measured_color_error": null,
        "physical_color_accuracy": "unverified",
    });
    let path = state_home().join("tessel-hdr-calibration/reports").join(format!("{}.json", compat::time_ns()));
    color_config::atomic_write(&path, json_text(&report).as_bytes(), 0o600)?;
    Ok(path)
}
