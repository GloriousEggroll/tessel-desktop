//! Serialized hardware brightness controls; no gamma or software substitutes
//! (`brightness_backend.py`).
//!
//! Only `set_value()`/`set_raw()` write hardware. Discovery and probing are
//! read-only, run on the `display-hardware` worker thread, and never restore
//! remembered values. Hardware is probed once, then again only after a monitor
//! hotplug or an explicit refresh (opening the brightness controls); displays
//! without DDC are not re-probed until they are reconnected, because every
//! failed ddcutil probe floods syslog.

use tessel_core::scheduler::Scheduler;
use crate::model::{self, feature_code, feature_state, integer, number, Callback, FEATURES, UNAVAILABLE_CONTROL};
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::time::Duration;

pub const HOTPLUG_SETTLE_MS: u32 = 750;
pub const LOGIN_NAME: &str = "org.freedesktop.login1";
pub const LOGIN_PATH: &str = "/org/freedesktop/login1";
pub const LOGIN_MANAGER: &str = "org.freedesktop.login1.Manager";
pub const LOGIN_SESSION: &str = "org.freedesktop.login1.Session";

static VCP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*VCP\s+(?:0x)?([0-9a-fA-F]{2})\s+C\s+(\d+)\s+(\d+)\s*$").unwrap());
static INTERNAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:eDP|LVDS|DSI)-").unwrap());
static CONNECTOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.:-]+$").unwrap());
static DRM_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^card\d+-.+$").unwrap());
static CARD_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^card\d+-").unwrap());
static I2C: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^i2c-(\d+)$").unwrap());

/// Only continuous, numeric VCP values are usable as hardware sliders.
pub fn parse_vcp(text: &str) -> HashMap<String, (i64, i64)> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let Some(found) = VCP.captures(line) else { continue };
        let (Ok(raw), Ok(maximum)) = (found[2].parse::<i64>(), found[3].parse::<i64>()) else { continue };
        if 0 <= raw && raw <= maximum && maximum <= 65535 && maximum > 0 {
            values.insert(found[1].to_uppercase(), (raw, maximum));
        }
    }
    values
}

pub fn edid_hash(data: &[u8]) -> Result<String, String> {
    if data.len() < 128 || !data.starts_with(b"\x00\xff\xff\xff\xff\xff\xff\x00") {
        return Err("The monitor's hardware identity is unavailable.".into());
    }
    Ok(Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect())
}

/// Translate output-management physical modes into logical screen bounds.
pub fn logical_geometry(output: &Value) -> Option<[i64; 4]> {
    let position = output.get("position")?.as_object()?;
    let (x, y) = (integer(position.get("x")?)?, integer(position.get("y")?)?);
    let modes: Vec<&Value> = output.get("modes").and_then(Value::as_array).map(|modes| {
        modes.iter().filter(|m| m.is_object() && m.get("current") == Some(&Value::Bool(true))).collect()
    }).unwrap_or_default();
    let scale = match output.get("scale") {
        None => 1.0,
        Some(value) => number(value)?,
    };
    if modes.len() != 1 || scale <= 0.0 {
        return None;
    }
    let width = integer(modes[0].get("width")?)?;
    let height = integer(modes[0].get("height")?)?;
    if width <= 0 || height <= 0 {
        return None;
    }
    let transform = output.get("transform").and_then(Value::as_str).unwrap_or("normal");
    let (width, height) = if ["90", "270", "flipped-90", "flipped-270"].contains(&transform) { (height, width) } else { (width, height) };
    Some([x, y, ((width as f64 / scale) as i64).max(1), ((height as f64 / scale) as i64).max(1)])
}

pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Everything the worker touches on the machine (the Python tests patch these).
pub trait System: Send + Sync {
    fn which(&self, name: &str) -> Option<String>;
    /// `Err` is the RuntimeError text (timeout / could not run).
    fn run(&self, argv: &[String], timeout: Duration) -> Result<CommandOutput, String>;
    fn drm_root(&self) -> PathBuf;
    fn backlight_root(&self) -> PathBuf;
    /// logind `GetSession` then `guard()` then `Session.SetBrightness("backlight", name, raw)`.
    fn set_backlight(&self, name: &str, raw: u32, guard: &dyn Fn() -> Result<(), String>) -> Result<(), String>;
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration)
    }
    /// Whether labwc drives `connector` in HDR (some monitors then lock brightness and contrast).
    fn hdr_enabled(&self, connector: &str) -> bool;
    /// Remembered `display id` + `feature` pairs the display refused to change while in HDR.
    fn load_hdr_locks(&self) -> Vec<String>;
    fn save_hdr_locks(&self, locks: &[String]);
}

/// Shown (in English, for logs and D-Bus) when HDR locks a control.
pub const HDR_LOCKED: &str = "This display does not allow changing this while HDR is on.";
const HDR_LOCKS_FILE: &str = "hdr-locked.json";

/// The real machine: `/sys/class/{drm,backlight}`, subprocesses with
/// `LC_ALL=C` and a deadline, and the system bus for logind.
pub struct RealSystem;

pub fn run_command(argv: &[String], timeout: Duration) -> Result<CommandOutput, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let name = Path::new(&argv[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut child = Command::new(&argv[0]).args(&argv[1..]).env("LC_ALL", "C").stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .map_err(|e| format!("Could not run {name}: {}", tessel_core::pyerr::os_error_path(&e, Path::new(&argv[0]))))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || { let mut s = Vec::new(); let _ = stdout.read_to_end(&mut s); s });
    let err = std::thread::spawn(move || { let mut s = Vec::new(); let _ = stderr.read_to_end(&mut s); s });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The monitor did not respond before the hardware timeout.".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(format!("Could not run {name}: {}", tessel_core::pyerr::os_error_text(&error))),
        }
    };
    let decode = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    Ok(CommandOutput { status: status.code().unwrap_or(-1), stdout: decode(out.join().unwrap_or_default()),
                       stderr: decode(err.join().unwrap_or_default()) })
}

impl System for RealSystem {
    fn which(&self, name: &str) -> Option<String> {
        tessel_core::shell::which(name).map(|p| p.to_string_lossy().into_owned())
    }

    fn run(&self, argv: &[String], timeout: Duration) -> Result<CommandOutput, String> {
        run_command(argv, timeout)
    }

    fn drm_root(&self) -> PathBuf {
        PathBuf::from("/sys/class/drm")
    }

    fn backlight_root(&self) -> PathBuf {
        PathBuf::from("/sys/class/backlight")
    }

    fn hdr_enabled(&self, connector: &str) -> bool {
        tessel_core::labwc_hdr::hdr_enabled(connector)
    }

    fn load_hdr_locks(&self) -> Vec<String> {
        let path = tessel_core::paths::component_state_dir("brightness").join(HDR_LOCKS_FILE);
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok()).unwrap_or_default()
    }

    fn save_hdr_locks(&self, locks: &[String]) {
        let directory = tessel_core::paths::component_state_dir("brightness");
        let data = serde_json::to_vec(locks).unwrap_or_default();
        if let Err(error) = tessel_core::fsutil::write_private(&directory, HDR_LOCKS_FILE, ".hdr-locked-", &data) {
            eprintln!("Could not remember HDR-locked display controls: {error}");
        }
    }

    fn set_backlight(&self, name: &str, raw: u32, guard: &dyn Fn() -> Result<(), String>) -> Result<(), String> {

        let message = |e: glib::Error| e.message().to_owned();
        let connection = gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>).map_err(message)?;
        let session_id = std::env::var("XDG_SESSION_ID").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "auto".into());
        let reply = connection.call_sync(Some(LOGIN_NAME), LOGIN_PATH, LOGIN_MANAGER, "GetSession",
            Some(&(session_id,).into()), Some(glib::VariantTy::new("(o)").unwrap()), gio::DBusCallFlags::NONE, 5000,
            None::<&gio::Cancellable>).map_err(message)?;
        let session = reply.child_value(0).str().unwrap_or("").to_owned();
        guard()?;
        connection.call_sync(Some(LOGIN_NAME), &session, LOGIN_SESSION, "SetBrightness",
            Some(&("backlight", name, raw).into()), None, gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>)
            .map_err(message)?;
        Ok(())
    }
}

/// What the core, keyboard and application need from a hardware provider
/// (the Python duck-typed backend interface).
pub trait Hardware {
    fn ready(&self) -> bool;
    fn busy(&self) -> bool;
    fn error(&self) -> String;
    fn displays(&self) -> Vec<Value>;
    fn start(&self);
    fn close(&self);
    fn refresh(&self);
    fn read_feature(&self, connector: &str, feature: &str, callback: Option<Callback>, expected_id: Option<&str>) -> bool;
    fn set_raw(&self, connector: &str, feature: &str, raw: i64, callback: Option<Callback>, expected_id: Option<&str>,
               expected_raw: Option<i64>) -> bool;
    fn set_value(&self, connector: &str, feature: &str, percent: f64, callback: Option<Callback>, expected_id: Option<&str>) -> bool;
}

/// GDK's live monitor list (`items-changed`, `notify::geometry`, `notify::scale-factor`).
pub trait MonitorWatch {
    fn watch(&self, changed: Rc<dyn Fn()>);
    fn close(&self);
}

#[derive(Clone, Debug)]
struct Record {
    connector: String,
    name: String,
    id: String,
    enabled: bool,
    geometry: Option<[i64; 4]>,
    method: String,
    error: String,
    path: Option<PathBuf>,
    edid: Option<Vec<u8>>,
    bus: Option<u32>,
    backlight: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
struct Identity(String, String, Option<PathBuf>, String, Option<u32>, Option<PathBuf>, bool);

impl Record {
    fn identity(&self) -> Identity {
        Identity(self.connector.clone(), self.id.clone(), self.path.clone(), self.method.clone(), self.bus, self.backlight.clone(),
                 self.enabled)
    }

    fn public(&self) -> Value {
        let mut record = Map::new();
        record.insert("connector".into(), self.connector.clone().into());
        record.insert("name".into(), self.name.clone().into());
        record.insert("id".into(), self.id.clone().into());
        record.insert("enabled".into(), self.enabled.into());
        record.insert("method".into(), self.method.clone().into());
        record.insert("error".into(), self.error.clone().into());
        record.insert("geometry".into(), geometry_value(self.geometry));
        for feature in FEATURES {
            let unsupported = self.method == "unsupported" || (self.method == "backlight" && feature == "contrast");
            let state = if unsupported {
                let error = if feature == "contrast" && self.method == "backlight" {
                    "Internal display contrast is not exposed by its backlight."
                } else {
                    &self.error
                };
                feature_state(Some(false), None, None, error)
            } else {
                feature_state(None, None, None, "")
            };
            record.insert(feature.into(), state);
        }
        Value::Object(record)
    }
}

fn geometry_value(geometry: Option<[i64; 4]>) -> Value {
    geometry.map(|g| Value::from(g.to_vec())).unwrap_or(Value::Null)
}

fn command_error(result: &CommandOutput, fallback: &str) -> String {
    let detail = if !result.stderr.is_empty() { &result.stderr } else { &result.stdout };
    let detail = detail.trim();
    if detail.is_empty() { fallback.to_owned() } else { detail.chars().take(350).collect() }
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| tessel_core::pyerr::os_error_path(&e, path))
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| tessel_core::pyerr::os_error_path(&e, path))
}

/// `Path.resolve()` (non-strict).
fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => resolve(parent).join(name),
        _ => path.to_path_buf(),
    }
}

fn below(path: &Path, ancestor: &Path) -> bool {
    path != ancestor && path.starts_with(ancestor)
}

enum Job {
    Refresh(bool),
    Read { request: u64, connector: String, feature: String, expected_id: String },
    Write { request: u64, connector: String, feature: String, target: i64, expected_id: String, expected_max: i64,
            expected_raw: Option<i64> },
}

enum Event {
    Snapshot(Vec<Value>),
    RefreshFinished(Option<Vec<Value>>, String),
    Finished { request: u64, connector: String, feature: String, state: Option<Value>, success: bool, error: String },
}

struct Worker {
    system: Arc<dyn System>,
    closed: Arc<AtomicBool>,
    events: async_channel::Sender<Event>,
    /// Worker-owned snapshots; the main thread receives copies.
    records: Map<String, Value>,
    hardware: HashMap<String, Record>,
    /// connector → (identity, record) of displays whose probe failed.
    no_ddc: HashMap<String, (Identity, Value)>,
    /// `id\nfeature` the display refused to change while in HDR.
    hdr_locks: HashSet<String>,
}

fn hdr_key(id: &str, feature: &str) -> String {
    format!("{id}\n{feature}")
}

impl Worker {
    /// Whether `feature` on this display is currently locked by HDR.
    fn hdr_locked(&self, hardware: &Record, feature: &str) -> bool {
        self.hdr_locks.contains(&hdr_key(&hardware.id, feature)) && self.system.hdr_enabled(&hardware.connector)
    }

    /// Mark a readable feature state as HDR-locked (disabled, with the reason).
    fn apply_hdr_lock(&self, hardware: &Record, feature: &str, state: &mut Value) {
        if state.get("supported") == Some(&Value::Bool(true)) && self.hdr_locked(hardware, feature) {
            state["hdr_locked"] = true.into();
            state["error"] = HDR_LOCKED.into();
        }
    }

    fn remember_hdr_lock(&mut self, hardware: &Record, feature: &str, locked: bool) {
        let key = hdr_key(&hardware.id, feature);
        let changed = if locked { self.hdr_locks.insert(key) } else { self.hdr_locks.remove(&key) };
        if changed {
            let mut locks: Vec<String> = self.hdr_locks.iter().cloned().collect();
            locks.sort();
            self.system.save_hdr_locks(&locks);
        }
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn send(&self, event: Event) {
        let _ = self.events.send_blocking(event);
    }

    fn run(&mut self, receiver: mpsc::Receiver<Job>) {
        while let Ok(job) = receiver.recv() {
            if self.closed() {
                return;
            }
            let (request, connector, feature) = match &job {
                Job::Refresh(_) => (0, String::new(), String::new()),
                Job::Read { request, connector, feature, .. } | Job::Write { request, connector, feature, .. } =>
                    (*request, connector.clone(), feature.clone()),
            };
            let refresh = matches!(job, Job::Refresh(_));
            let result = match job {
                Job::Refresh(full) => self.refresh_work(full),
                Job::Read { request, connector, feature, expected_id } => self.read_work(request, &connector, &feature, &expected_id),
                Job::Write { request, connector, feature, target, expected_id, expected_max, expected_raw } =>
                    self.write_work(request, &connector, &feature, target, &expected_id, expected_max, expected_raw),
            };
            if let Err(error) = result {
                // A disappeared sysfs file is an ordinary hotplug failure.
                if refresh {
                    self.send(Event::RefreshFinished(None, error));
                } else {
                    self.send(Event::Finished { request, connector, feature, state: None, success: false, error });
                }
            }
        }
    }

    fn ddc_bus(connector_path: &Path) -> Result<u32, String> {
        // DisplayPort AUX adapters live below the connector. The generic ddc
        // link may point to a different legacy adapter on the same GPU.
        let mut local = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(connector_path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(found) = I2C.captures(&name) {
                    if entry.path().exists() {
                        if let Ok(number) = found[1].parse::<u32>() {
                            local.insert(number);
                        }
                    }
                }
            }
        }
        if local.len() == 1 {
            return Ok(local.into_iter().next().unwrap());
        }
        if !local.is_empty() {
            return Err("More than one DDC adapter belongs to this connector.".into());
        }
        let linked = connector_path.join("ddc");
        if linked.exists() {
            let target = resolve(&linked);
            let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(found) = I2C.captures(&name) {
                if let Ok(number) = found[1].parse::<u32>() {
                    return Ok(number);
                }
            }
        }
        Err("This connector has no accessible hardware DDC adapter.".into())
    }

    fn backlight(&self, connector_path: &Path, internal_count: usize) -> Result<Option<PathBuf>, String> {
        let root = self.system.backlight_root();
        let mut candidates: Vec<PathBuf> = if root.exists() {
            std::fs::read_dir(&root).map_err(|e| tessel_core::pyerr::os_error_path(&e, &root))?.flatten().map(|e| e.path()).collect()
        } else {
            Vec::new()
        };
        candidates.sort();
        let resolved = resolve(connector_path);
        let mut owned: Vec<PathBuf> = candidates.iter().filter(|candidate| {
            let device = resolve(&candidate.join("device"));
            let target = resolve(candidate);
            device == resolved || below(&device, &resolved) || below(&target, &resolved)
        }).cloned().collect();
        if owned.is_empty() && internal_count == 1 {
            owned = candidates;
        }
        if owned.is_empty() {
            return Ok(None);
        }
        let ranked: Vec<(u8, PathBuf)> = owned.into_iter().map(|candidate| {
            let kind = std::fs::read_to_string(candidate.join("type")).map(|t| t.trim().to_owned()).unwrap_or_default();
            let rank = match kind.as_str() { "raw" => 0, "platform" => 1, "firmware" => 2, _ => 3 };
            (rank, candidate)
        }).collect();
        let best_rank = ranked.iter().map(|r| r.0).min().unwrap();
        let best: Vec<PathBuf> = ranked.into_iter().filter(|r| r.0 == best_rank).map(|r| r.1).collect();
        if best.len() != 1 {
            return Err("The internal display has more than one matching backlight.".into());
        }
        Ok(best.into_iter().next())
    }

    fn discover(&self) -> Result<Vec<Record>, String> {
        let executable = self.system.which("wlr-randr").ok_or_else(|| "Display controls require wlr-randr.".to_owned())?;
        let result = self.system.run(&[executable, "--json".into()], Duration::from_secs(4))?;
        if result.status != 0 {
            return Err(command_error(&result, "Cannot read the Wayland outputs."));
        }
        let outputs: Value = serde_json::from_str(&result.stdout).map_err(|e| e.to_string())?;
        let outputs = outputs.as_array().ok_or_else(|| "The compositor returned an invalid display list.".to_owned())?;
        let root = self.system.drm_root();
        let mut drm: Vec<PathBuf> = std::fs::read_dir(&root).map_err(|e| tessel_core::pyerr::os_error_path(&e, &root))?.flatten()
            .filter(|e| DRM_NAME.is_match(&e.file_name().to_string_lossy())).map(|e| e.path()).collect();
        drm.sort();
        let internal_count = outputs.iter().filter(|o| INTERNAL.is_match(model::text(o, "name"))).count();
        let ddcutil = self.system.which("ddcutil").is_some();
        let mut hardware = Vec::new();
        for output in outputs {
            let Some(connector) = output.get("name").and_then(Value::as_str) else { continue };
            if !CONNECTOR.is_match(connector) {
                continue;
            }
            let description = output.get("description").filter(|d| model::truthy(d))
                .map(|d| d.as_str().map(String::from).unwrap_or_else(|| d.to_string())).unwrap_or_else(|| connector.to_owned());
            let mut record = Record {
                connector: connector.into(), name: description, id: format!("unidentified:{connector}"),
                enabled: output.get("enabled") == Some(&Value::Bool(true)), geometry: logical_geometry(output),
                method: "unsupported".into(), error: String::new(), path: None, edid: None, bus: None, backlight: None,
            };
            let outcome = (|| -> Result<(), String> {
                let mut candidates = Vec::new();
                for path in &drm {
                    let name = path.file_name().unwrap().to_string_lossy();
                    if CARD_PREFIX.replace(&name, "") == connector && read_text(&path.join("status"))?.trim() == "connected" {
                        candidates.push(path.clone());
                    }
                }
                if candidates.len() != 1 {
                    return Err("This display cannot be matched uniquely to a hardware connector.".into());
                }
                let path = candidates.remove(0);
                let raw_edid = read_bytes(&path.join("edid"))?;
                record.path = Some(path.clone());
                record.edid = Some(raw_edid.clone());
                record.id = edid_hash(&raw_edid)?;
                if INTERNAL.is_match(connector) {
                    if let Some(backlight) = self.backlight(&path, internal_count)? {
                        record.method = "backlight".into();
                        record.backlight = Some(backlight);
                    }
                }
                if record.method != "backlight" {
                    let bus = Self::ddc_bus(&path)?;
                    if !ddcutil {
                        return Err("Hardware monitor controls require ddcutil.".into());
                    }
                    record.method = "ddc".into();
                    record.bus = Some(bus);
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                record.error = error;
            }
            hardware.push(record);
        }
        let mut counts: HashMap<String, usize> = HashMap::new();
        for record in &hardware {
            *counts.entry(record.id.clone()).or_default() += 1;
        }
        for record in &mut hardware {
            if counts[&record.id] > 1 {
                // Some displays ship identical EDIDs/serials. Never restore
                // one display's brightness onto its indistinguishable sibling.
                record.id = format!("{}:{}", record.id, record.connector);
            }
        }
        Ok(hardware)
    }

    fn get_values(&self, hardware: &Record, features: &[&str]) -> Result<(HashMap<String, (i64, i64)>, String), String> {
        if hardware.method == "backlight" {
            let path = hardware.backlight.as_ref().unwrap();
            let parse = |name: &str| -> Result<i64, String> {
                let text = read_text(&path.join(name))?;
                text.trim().parse::<i64>().map_err(|_| format!("invalid literal for int() with base 10: '{}'", text.trim()))
            };
            let maximum = parse("max_brightness")?;
            let raw = parse("brightness")?;
            if !(0 <= raw && raw <= maximum && maximum <= u32::MAX as i64) || maximum == 0 {
                return Err("The internal display returned an invalid brightness range.".into());
            }
            return Ok((HashMap::from([("brightness".to_owned(), (raw, maximum))]), String::new()));
        }
        let mut argv = vec![self.system.which("ddcutil").unwrap_or_else(|| "ddcutil".into()), "--noconfig".into(), "--bus".into(),
                            hardware.bus.unwrap_or(0).to_string(), "getvcp".into()];
        argv.extend(features.iter().map(|f| feature_code(f).unwrap().to_owned()));
        argv.push("--terse".into());
        let result = self.system.run(&argv, Duration::from_secs(6))?;
        let parsed = parse_vcp(&result.stdout);
        let values = features.iter().filter_map(|f| parsed.get(feature_code(f).unwrap()).map(|v| (f.to_string(), *v))).collect();
        Ok((values, command_error(&result, "This monitor does not expose that hardware control.")))
    }

    fn probe(&self, hardware: &Record, record: &mut Value) {
        if hardware.method == "unsupported" {
            return;
        }
        if !hardware.enabled {
            for feature in FEATURES {
                record[feature] = feature_state(Some(false), None, None, "Enable this display to use its hardware controls.");
            }
            return;
        }
        match self.get_values(hardware, &FEATURES) {
            Ok((values, error)) => {
                for feature in FEATURES {
                    if let Some((raw, maximum)) = values.get(feature) {
                        record[feature] = feature_state(Some(true), Some(*raw), Some(*maximum), "");
                        self.apply_hdr_lock(hardware, feature, &mut record[feature]);
                    } else if hardware.method != "backlight" || feature != "contrast" {
                        record[feature] = feature_state(Some(false), None, None, UNAVAILABLE_CONTROL);
                        record[feature]["diagnostic"] = error.clone().into();
                    }
                }
                if values.is_empty() {
                    record["error"] = UNAVAILABLE_CONTROL.into();
                    record["diagnostic"] = error.into();
                    record["method"] = "unsupported".into();
                }
            }
            Err(error) => {
                record["error"] = UNAVAILABLE_CONTROL.into();
                record["diagnostic"] = error.clone().into();
                record["method"] = "unsupported".into();
                for feature in FEATURES {
                    record[feature] = feature_state(Some(false), None, None, UNAVAILABLE_CONTROL);
                    record[feature]["diagnostic"] = error.clone().into();
                }
            }
        }
    }

    fn refresh_work(&mut self, full: bool) -> Result<(), String> {
        let hardware = self.discover()?;
        let present: HashSet<String> = hardware.iter().map(|r| r.connector.clone()).collect();
        // Unplugged: a reconnected display gets one fresh probe.
        self.no_ddc.retain(|connector, _| present.contains(connector));
        let mut records = Map::new();
        let mut probes = Vec::new();
        for item in &hardware {
            let connector = &item.connector;
            let same = self.hardware.get(connector).is_some_and(|previous| previous.identity() == item.identity());
            let mut enabled_identity = item.clone();
            enabled_identity.enabled = true;
            let remembered = self.no_ddc.get(connector).filter(|r| r.0 == enabled_identity.identity());
            let mut record = if let Some(remembered) = remembered {
                let mut record = remembered.1.clone();
                if !item.enabled {
                    record = item.public();
                    self.probe(item, &mut record);
                }
                record
            } else if let Some(previous) = self.records.get(connector).filter(|_| same && !full) {
                previous.clone()
            } else {
                probes.push(item.clone());
                item.public()
            };
            record["name"] = item.name.clone().into();
            record["enabled"] = item.enabled.into();
            record["geometry"] = geometry_value(item.geometry);
            records.insert(connector.clone(), record);
        }
        self.send(Event::Snapshot(records.values().cloned().collect()));
        for item in probes {
            if self.closed() {
                return Ok(());
            }
            let connector = item.connector.clone();
            let mut record = records[&connector].clone();
            self.probe(&item, &mut record);
            if item.method == "ddc" && item.enabled && record["method"] == "unsupported" {
                self.no_ddc.insert(connector.clone(), (item.identity(), record.clone()));
            }
            records.insert(connector, record);
            self.send(Event::Snapshot(records.values().cloned().collect()));
        }
        self.hardware = hardware.into_iter().map(|r| (r.connector.clone(), r)).collect();
        self.records = records;
        self.send(Event::RefreshFinished(Some(self.records.values().cloned().collect()), String::new()));
        Ok(())
    }

    fn validate(&self, expected: &Record, current: &Record) -> Result<(), String> {
        if self.closed() {
            return Err("The display request was canceled.".into());
        }
        if expected.identity() != current.identity() || !current.enabled {
            return Err("The display changed or disconnected before the hardware request.".into());
        }
        let path = current.path.as_ref().ok_or("The monitor connected to this output has changed.")?;
        if read_text(&path.join("status"))?.trim() != "connected" || Some(read_bytes(&path.join("edid"))?) != current.edid {
            return Err("The monitor connected to this output has changed.".into());
        }
        if current.method == "ddc" && Some(Self::ddc_bus(path)?) != current.bus {
            return Err("The monitor's DDC adapter changed before the request.".into());
        }
        if current.method == "backlight" && !current.backlight.as_ref().is_some_and(|b| b.exists()) {
            return Err("The internal display backlight disconnected.".into());
        }
        Ok(())
    }

    fn current(&self, connector: &str) -> Result<(Option<Record>, Option<Record>), String> {
        let expected = self.hardware.get(connector).cloned();
        let current = self.discover()?.into_iter().find(|r| r.connector == connector);
        Ok((expected, current))
    }

    fn read_work(&mut self, request: u64, connector: &str, feature: &str, expected_id: &str) -> Result<(), String> {
        let (expected, current) = self.current(connector)?;
        let (Some(expected), Some(current)) = (expected, current) else {
            return Err("The monitor changed or disconnected; no hardware value was read.".into());
        };
        if current.id != expected_id || current.method == "unsupported" {
            return Err("The monitor changed or disconnected; no hardware value was read.".into());
        }
        self.validate(&expected, &current)?;
        let (values, error) = self.get_values(&current, &[feature])?;
        self.validate(&expected, &current)?;
        let Some((raw, maximum)) = values.get(feature) else { return Err(error) };
        let mut state = feature_state(Some(true), Some(*raw), Some(*maximum), "");
        self.apply_hdr_lock(&current, feature, &mut state);
        if let Some(record) = self.records.get_mut(connector) {
            record[feature] = state.clone();
        }
        self.send(Event::Finished { request, connector: connector.into(), feature: feature.into(), state: Some(state),
                                    success: true, error: String::new() });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn write_work(&mut self, request: u64, connector: &str, feature: &str, target: i64, expected_id: &str, expected_max: i64,
                  expected_raw: Option<i64>) -> Result<(), String> {
        let (expected, current) = self.current(connector)?;
        let (Some(expected), Some(current)) = (expected, current) else {
            return Err("The monitor changed or disconnected; no hardware value was written.".into());
        };
        if current.id != expected_id {
            return Err("The monitor changed or disconnected; no hardware value was written.".into());
        }
        self.validate(&expected, &current)?;
        let (values, error) = self.get_values(&current, &[feature])?;
        let Some(&(before, maximum)) = values.get(feature) else { return Err(error) };
        if maximum != expected_max || target > maximum {
            return Err("The monitor's hardware range changed; no value was written.".into());
        }
        if before != target && self.hdr_locked(&current, feature) {
            let mut state = feature_state(Some(true), Some(before), Some(maximum), "");
            self.apply_hdr_lock(&current, feature, &mut state);
            if let Some(record) = self.records.get_mut(connector) {
                record[feature] = state.clone();
            }
            self.send(Event::Finished { request, connector: connector.into(), feature: feature.into(), state: Some(state),
                                        success: false, error: HDR_LOCKED.into() });
            return Ok(());
        }
        if expected_raw.is_some_and(|raw| raw != before) {
            let error = "The monitor setting changed externally; no hardware value was written.";
            let state = feature_state(Some(true), Some(before), Some(maximum), error);
            if let Some(record) = self.records.get_mut(connector) {
                record[feature] = state.clone();
            }
            self.send(Event::Finished { request, connector: connector.into(), feature: feature.into(), state: Some(state),
                                        success: false, error: error.into() });
            return Ok(());
        }
        // Read-only probing can take time. Recheck the exact EDID and bus again
        // immediately before sending a command to that physical output.
        self.validate(&expected, &current)?;
        if before != target {
            if current.method == "backlight" {
                let name = current.backlight.as_ref().and_then(|b| b.file_name()).map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.system.set_backlight(&name, target as u32, &|| self.validate(&expected, &current))?;
            } else {
                let argv = vec![self.system.which("ddcutil").unwrap_or_else(|| "ddcutil".into()), "--noconfig".into(),
                                "--bus".into(), current.bus.unwrap_or(0).to_string(), "setvcp".into(),
                                feature_code(feature).unwrap().into(), target.to_string(), "--noverify".into()];
                let result = self.system.run(&argv, Duration::from_secs(6))?;
                if result.status != 0 {
                    return Err(command_error(&result, "The monitor rejected the hardware value."));
                }
            }
        }
        let mut actual = None;
        let mut error = String::new();
        for attempt in 0..3u32 {
            if attempt > 0 {
                self.system.sleep(Duration::from_secs_f64(0.15 * attempt as f64));
            }
            self.validate(&expected, &current)?;
            let (values, failure) = self.get_values(&current, &[feature])?;
            error = failure;
            self.validate(&expected, &current)?;
            if let Some(&(value, confirmed_max)) = values.get(feature) {
                if confirmed_max != maximum {
                    return Err("The monitor's hardware range changed during the request.".into());
                }
                actual = Some(value);
                if value == target {
                    break;
                }
            }
        }
        let Some(actual) = actual else {
            return Err(format!("The hardware write could not be confirmed: {error}"));
        };
        let success = actual == target;
        // A display that ignores the change while in HDR has locked the control
        // (Alienware QD-OLEDs do this); remember it so the slider shows why.
        if before != target && current.method == "ddc" && actual == before && self.system.hdr_enabled(connector) {
            self.remember_hdr_lock(&current, feature, true);
        } else if success && before != target {
            self.remember_hdr_lock(&current, feature, false);
        }
        let error = if success { String::new() } else { format!("The display reported {actual} after requesting {target}.") };
        let mut state = feature_state(Some(true), Some(actual), Some(maximum), &error);
        self.apply_hdr_lock(&current, feature, &mut state);
        let error = state["error"].as_str().unwrap_or_default().to_owned();
        if let Some(record) = self.records.get_mut(connector) {
            record[feature] = state.clone();
        }
        self.send(Event::Finished { request, connector: connector.into(), feature: feature.into(), state: Some(state), success, error });
        Ok(())
    }
}

pub struct Options {
    pub system: Arc<dyn System>,
    pub scheduler: Rc<dyn Scheduler>,
    pub monitors: Option<Box<dyn MonitorWatch>>,
    /// Deliver worker results from a main-context task (production). Tests
    /// pass `false` and call [`DisplayBackend::pump`].
    pub spawn_events: bool,
}

pub struct Inner {
    ready: Cell<bool>,
    busy: Cell<bool>,
    error: RefCell<String>,
    displays: RefCell<Vec<Value>>,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    options: RefCell<Option<Options>>,
    scheduler: Rc<dyn Scheduler>,
    started: Cell<bool>,
    closed: Cell<bool>,
    worker_closed: Arc<AtomicBool>,
    jobs: RefCell<Option<mpsc::Sender<Job>>>,
    thread: RefCell<Option<std::thread::JoinHandle<()>>>,
    events: RefCell<Option<async_channel::Receiver<Event>>>,
    poll_source: Cell<Option<u64>>,
    monitors: RefCell<Option<Box<dyn MonitorWatch>>>,
    refresh_pending: Cell<bool>,
    refresh_full: Cell<bool>,
    refresh_again: Cell<bool>,
    pending: RefCell<BTreeMap<u64, (String, String, Option<Callback>)>>,
    next_request: Cell<u64>,
}

#[derive(Clone)]
pub struct DisplayBackend(pub Rc<Inner>);

impl DisplayBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static, options: Options) -> Self {
        Self(Rc::new(Inner {
            ready: Cell::new(false), busy: Cell::new(false), error: RefCell::new(String::new()), displays: RefCell::new(Vec::new()),
            on_changed: Box::new(on_changed), on_error: Box::new(on_error), scheduler: options.scheduler.clone(),
            options: RefCell::new(Some(options)), started: Cell::new(false), closed: Cell::new(false),
            worker_closed: Arc::new(AtomicBool::new(false)), jobs: RefCell::new(None), thread: RefCell::new(None),
            events: RefCell::new(None), poll_source: Cell::new(None), monitors: RefCell::new(None),
            refresh_pending: Cell::new(false), refresh_full: Cell::new(false), refresh_again: Cell::new(false),
            pending: RefCell::new(BTreeMap::new()), next_request: Cell::new(0),
        }))
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// The hotplug settle timer (tests).
    pub fn poll_source(&self) -> Option<u64> {
        self.0.poll_source.get()
    }

    pub fn has_monitor_watch(&self) -> bool {
        self.0.monitors.borrow().is_some()
    }

    pub fn join_worker(&self) {
        if let Some(thread) = self.0.thread.take() {
            let _ = thread.join();
        }
    }

    fn notify(&self) {
        let busy = self.0.refresh_pending.get() || !self.0.pending.borrow().is_empty();
        self.0.busy.set(busy);
        {
            let pending = self.0.pending.borrow();
            let mut displays = self.0.displays.borrow_mut();
            for display in displays.iter_mut() {
                let connector = model::text(display, "connector").to_owned();
                for feature in FEATURES {
                    let busy = pending.values().any(|(c, f, _)| *c == connector && f == feature);
                    if display.get(feature).is_some_and(Value::is_object) {
                        display[feature]["busy"] = busy.into();
                    }
                }
            }
        }
        if !self.0.closed.get() {
            (self.0.on_changed)();
        }
    }

    /// Deliver every worker result that has arrived (main thread).
    pub fn pump(&self) {
        loop {
            let event = match self.0.events.borrow().as_ref().map(|r| r.try_recv()) {
                Some(Ok(event)) => event,
                _ => return,
            };
            self.handle(event);
        }
    }

    fn handle(&self, event: Event) {
        if self.0.closed.get() {
            return;
        }
        match event {
            Event::Snapshot(displays) => {
                self.0.displays.replace(displays);
                self.notify();
            }
            Event::RefreshFinished(displays, error) => self.refresh_finished(displays, error),
            Event::Finished { request, connector, feature, state, success, error } =>
                self.write_finished(request, &connector, &feature, state, success, error),
        }
    }

    fn watch_monitors(&self, monitors: Option<Box<dyn MonitorWatch>>) {
        let Some(monitors) = monitors else { return };
        let weak = self.weak();
        // Layout changes (position, scale, transform) only re-read wlr-randr;
        // hardware is probed again only for connectors whose identity changed.
        monitors.watch(Rc::new(move || if let Some(inner) = weak.upgrade() { DisplayBackend(inner).schedule_hotplug() }));
        self.0.monitors.replace(Some(monitors));
    }

    pub fn schedule_hotplug(&self) {
        if self.0.closed.get() {
            return;
        }
        if let Some(source) = self.0.poll_source.take() {
            self.0.scheduler.cancel(source);
        }
        // Let wlr-randr and sysfs settle; a hotplug emits several GDK changes.
        let weak = self.weak();
        let id = self.0.scheduler.timeout(HOTPLUG_SETTLE_MS, Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.poll_source.set(None);
                let backend = DisplayBackend(inner);
                if !backend.0.closed.get() {
                    backend.queue_refresh(false);
                }
            }
        }));
        self.0.poll_source.set(Some(id));
    }

    pub fn queue_refresh(&self, full: bool) {
        if self.0.closed.get() || !self.0.started.get() {
            return;
        }
        if self.0.refresh_pending.get() {
            // Do not drop a hotplug or an open request that arrives mid-refresh.
            self.0.refresh_full.set(self.0.refresh_full.get() || full);
            self.0.refresh_again.set(true);
            return;
        }
        self.0.refresh_pending.set(true);
        self.0.refresh_again.set(false);
        self.notify();
        self.send(Job::Refresh(full));
    }

    fn send(&self, job: Job) {
        if let Some(jobs) = self.0.jobs.borrow().as_ref() {
            let _ = jobs.send(job);
        }
    }

    fn refresh_finished(&self, displays: Option<Vec<Value>>, error: String) {
        self.0.refresh_pending.set(false);
        self.0.ready.set(error.is_empty());
        self.0.error.replace(error.clone());
        match displays {
            Some(displays) => {
                self.0.displays.replace(displays);
            }
            None => {
                // A stale connector must never remain writable after discovery failed.
                for display in self.0.displays.borrow_mut().iter_mut() {
                    display["enabled"] = false.into();
                    display["error"] = error.clone().into();
                }
            }
        }
        if self.0.refresh_again.get() {
            let full = self.0.refresh_full.replace(false);
            self.queue_refresh(full);
            return;
        }
        self.notify();
    }

    fn reject(&self, message: &str, callback: Option<Callback>) -> bool {
        self.0.error.replace(message.into());
        if !self.0.closed.get() {
            (self.0.on_error)(message);
            self.notify();
        }
        model::call(callback, false, message);
        false
    }

    fn display(&self, connector: &str) -> Option<Value> {
        self.0.displays.borrow().iter().find(|d| model::text(d, "connector") == connector).cloned()
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_write(&self, connector: &str, feature: &str, value: f64, raw: bool, callback: Option<Callback>,
                     expected_id: Option<&str>, expected_raw: Option<i64>) -> bool {
        if self.0.closed.get() || !self.0.started.get() || !self.0.ready.get() {
            return self.reject("Display controls are not ready.", callback);
        }
        let display = self.display(connector);
        let Some(display) = display.filter(|d| feature_code(feature).is_some() && d["enabled"] == true && model::supported(d, feature))
        else {
            return self.reject("This display does not expose that hardware control.", callback);
        };
        let id = model::text(&display, "id").to_owned();
        if expected_id.is_some_and(|expected| expected != id) {
            return self.reject("The monitor connected to this output has changed.", callback);
        }
        let maximum = integer(&display[feature]["max"]).unwrap_or(0);
        let target = if raw { value as i64 } else { (value * maximum as f64 / 100.0 + 0.5).floor() as i64 };
        if !(0 <= target && target <= maximum) {
            return self.reject("The saved hardware value is outside the display's current range.", callback);
        }
        let request = self.0.next_request.get() + 1;
        self.0.next_request.set(request);
        self.0.pending.borrow_mut().insert(request, (connector.into(), feature.into(), callback));
        self.0.error.replace(String::new());
        if let Some(display) = self.0.displays.borrow_mut().iter_mut().find(|d| model::text(d, "connector") == connector) {
            display[feature]["error"] = "".into();
        }
        self.notify();
        self.send(Job::Write { request, connector: connector.into(), feature: feature.into(), target, expected_id: id,
                               expected_max: maximum, expected_raw });
        true
    }

    fn write_finished(&self, request: u64, connector: &str, feature: &str, state: Option<Value>, success: bool, error: String) {
        let Some(pending) = self.0.pending.borrow_mut().remove(&request) else { return };
        if let Some(display) = self.0.displays.borrow_mut().iter_mut().find(|d| model::text(d, "connector") == connector) {
            if let Some(state) = state {
                display[feature] = state;
            }
            display[feature]["error"] = error.clone().into();
        }
        self.0.error.replace(error.clone());
        self.notify();
        if !error.is_empty() {
            (self.0.on_error)(&error);
        }
        model::call(pending.2, success, &error);
    }
}

impl Hardware for DisplayBackend {
    fn ready(&self) -> bool {
        self.0.ready.get()
    }

    fn busy(&self) -> bool {
        self.0.busy.get()
    }

    fn error(&self) -> String {
        self.0.error.borrow().clone()
    }

    fn displays(&self) -> Vec<Value> {
        self.0.displays.borrow().clone()
    }

    fn start(&self) {
        if self.0.closed.get() || self.0.started.get() {
            return;
        }
        let Some(options) = self.0.options.take() else { return };
        self.0.started.set(true);
        let (jobs, receiver) = mpsc::channel();
        let (events, event_receiver) = async_channel::unbounded();
        let mut worker = Worker { system: options.system.clone(), closed: self.0.worker_closed.clone(), events,
                                  records: Map::new(), hardware: HashMap::new(), no_ddc: HashMap::new(),
                                  hdr_locks: options.system.load_hdr_locks().into_iter().collect() };
        let thread = std::thread::Builder::new().name("display-hardware".into()).spawn(move || worker.run(receiver))
            .expect("display-hardware thread");
        self.0.jobs.replace(Some(jobs));
        self.0.thread.replace(Some(thread));
        if options.spawn_events {
            let weak = self.weak();
            let receiver = event_receiver.clone();
            glib::MainContext::default().spawn_local(async move {
                while let Ok(event) = receiver.recv().await {
                    let Some(inner) = weak.upgrade() else { return };
                    DisplayBackend(inner).handle(event);
                }
            });
        }
        self.0.events.replace(Some(event_receiver));
        self.refresh();
        self.watch_monitors(options.monitors);
    }

    fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.worker_closed.store(true, Ordering::SeqCst);
        if let Some(source) = self.0.poll_source.take() {
            self.0.scheduler.cancel(source);
        }
        if let Some(monitors) = self.0.monitors.take() {
            monitors.close();
        }
        // Dropping the sender ends the worker once its current command returns.
        // Do not block GTK on a monitor that stopped answering.
        self.0.jobs.replace(None);
        let pending: Vec<_> = std::mem::take(&mut *self.0.pending.borrow_mut()).into_values().collect();
        for (_, _, callback) in pending {
            model::call(callback, false, "Display controls were closed before the request completed.");
        }
        self.0.ready.set(false);
        self.0.busy.set(false);
    }

    fn refresh(&self) {
        self.queue_refresh(true);
    }

    fn read_feature(&self, connector: &str, feature: &str, callback: Option<Callback>, expected_id: Option<&str>) -> bool {
        if self.0.closed.get() || !self.0.started.get() || !self.0.ready.get() {
            return self.reject("Display controls are not ready.", callback);
        }
        let Some(display) = self.display(connector).filter(|d| feature_code(feature).is_some() && d["enabled"] == true) else {
            return self.reject("This display is unavailable.", callback);
        };
        let id = model::text(&display, "id").to_owned();
        if expected_id.is_some_and(|expected| expected != id) {
            return self.reject("The monitor connected to this output has changed.", callback);
        }
        let request = self.0.next_request.get() + 1;
        self.0.next_request.set(request);
        self.0.pending.borrow_mut().insert(request, (connector.into(), feature.into(), callback));
        self.notify();
        self.send(Job::Read { request, connector: connector.into(), feature: feature.into(), expected_id: id });
        true
    }

    fn set_raw(&self, connector: &str, feature: &str, raw: i64, callback: Option<Callback>, expected_id: Option<&str>,
               expected_raw: Option<i64>) -> bool {
        if raw < 0 {
            return self.reject("The saved hardware value is invalid.", callback);
        }
        if expected_raw.is_some_and(|r| r < 0) {
            return self.reject("The expected hardware value is invalid.", callback);
        }
        self.enqueue_write(connector, feature, raw as f64, true, callback, expected_id, expected_raw)
    }

    fn set_value(&self, connector: &str, feature: &str, percent: f64, callback: Option<Callback>, expected_id: Option<&str>) -> bool {
        if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
            return self.reject("Choose a brightness or contrast from 0 to 100 percent.", callback);
        }
        self.enqueue_write(connector, feature, percent, false, callback, expected_id, None)
    }
}
