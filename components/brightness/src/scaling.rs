//! Asynchronous scale controls using the same tessel-displays IPC as its GUI
//! (`display_scaling.py`).
//!
//! Only explicit setters change configuration. Discovery and polling issue
//! GET; all socket operations run off the GTK thread and have bounded
//! responses/time.

use tessel_core::scheduler::Scheduler;
use crate::compat::yaml;
use crate::model::{self, UNAVAILABLE_CONTROL};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const MIN_SCALE: f64 = 1.0;
pub const MAX_SCALE: f64 = 2.5;
pub const SCALE_STEP: f64 = 0.125;
pub const UNAVAILABLE: &str = "Display scaling is unavailable. Start the display management service.";

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let mut paths = vec![runtime.join("tessel-displays.sock")];
    let terminal = std::env::var("XDG_VTNR").unwrap_or_default();
    if !terminal.is_empty() && terminal.chars().all(|c| c.is_ascii_digit()) {
        paths.push(runtime.join(format!("tessel-displays.{terminal}.sock")));
    }
    // A session started before tessel-displays had its own daemon keeps way-displays until logout.
    paths.push(runtime.join("way-displays.sock"));
    paths.iter().find(|p| p.exists()).cloned().unwrap_or_else(|| paths[0].clone())
}

/// Validate a decoded reply (`DONE: true`, int `RC`, `RC == 0`).
pub fn check_reply(result: Value) -> Result<Value, String> {
    // Changes are answered with a sequence whose last item is the outcome.
    let result = match result {
        Value::Array(mut items) => items.pop().unwrap_or(Value::Null),
        other => other,
    };
    let valid = result.is_object() && result.get("DONE") == Some(&Value::Bool(true)) && model::integer(&result["RC"]).is_some();
    if !valid {
        return Err("The display service returned an invalid response.".into());
    }
    if result["RC"] != 0 {
        let messages = result.get("MESSAGES").filter(|m| model::truthy(m));
        return Err(match messages {
            Some(messages) => python_str(messages).chars().take(500).collect(),
            None => "The display service rejected the change.".into(),
        });
    }
    Ok(result)
}

/// `str()` of a decoded YAML value, close enough to Python's repr for messages.
fn python_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => format!("[{}]", items.iter().map(|i| match i {
            Value::String(text) => format!("'{text}'"),
            other => python_str(other),
        }).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

/// One request/response exchange (`ipc_request`).
pub fn ipc_request(operation: &str, config: Option<&Value>) -> Result<Value, String> {
    let payload = yaml::request(operation, config);
    let deadline = Instant::now() + Duration::from_secs(5);
    let os = |e: std::io::Error| tessel_core::pyerr::os_error_text(&e);
    let mut client = std::os::unix::net::UnixStream::connect(socket_path()).map_err(os)?;
    client.set_write_timeout(Some(Duration::from_secs(5))).map_err(os)?;
    client.write_all(payload.as_bytes()).map_err(os)?;
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("The display service did not respond in time.".into());
        }
        client.set_read_timeout(Some(remaining)).map_err(os)?;
        let count = match client.read(&mut chunk) {
            Ok(count) => count,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                return Err("timed out".into()),
            Err(e) => return Err(os(e)),
        };
        if count == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..count]);
        if data.len() > 16 * 1024 * 1024 {
            return Err("The display service response is too large.".into());
        }
    }
    check_reply(yaml::load(&data)?)
}

pub fn validated_scale(value: f64) -> Result<f64, String> {
    if !value.is_finite() || !(MIN_SCALE..=MAX_SCALE).contains(&value) {
        return Err("Choose a display scale from 100% to 250%.".into());
    }
    Ok((value / SCALE_STEP).round_ties_even() * SCALE_STEP)
}

fn scale_value(value: Option<&Value>) -> Option<f64> {
    value.and_then(model::number).filter(|v| *v > 0.0 && *v <= 16.0)
}

pub struct Snapshot {
    pub auto_scale: bool,
    pub displays: Vec<Value>,
}

pub fn snapshot(data: &Value) -> Result<Snapshot, String> {
    let state = data.get("STATE").filter(|s| s.is_object());
    let heads = state.and_then(|s| s.get("HEADS")).and_then(Value::as_array);
    let config = data.get("CFG").and_then(Value::as_object);
    let (Some(heads), Some(config)) = (heads, config) else {
        return Err("The display service returned an invalid monitor list.".into());
    };
    let empty = Map::new();
    let overrides = match config.get("SCALE") {
        None => &empty,
        Some(value) if !model::truthy(value) => &empty,
        Some(Value::Object(map)) => map,
        Some(_) => return Err("The display service returned invalid scale overrides.".into()),
    };
    let heads: Vec<&Value> = heads.iter().filter(|h| h.is_object()).collect();
    let mut names: HashMap<&str, usize> = HashMap::new();
    let mut descriptions: HashMap<&str, usize> = HashMap::new();
    for head in &heads {
        if let Some(name) = head.get("NAME").and_then(Value::as_str) {
            *names.entry(name).or_default() += 1;
        }
        if let Some(description) = head.get("DESCRIPTION").and_then(Value::as_str) {
            *descriptions.entry(description).or_default() += 1;
        }
    }
    let mut displays = Vec::new();
    for head in heads {
        let (Some(connector), Some(description)) = (head.get("NAME").and_then(Value::as_str), head.get("DESCRIPTION").and_then(Value::as_str))
        else { continue };
        if connector.is_empty() || description.is_empty() {
            continue;
        }
        let current = match head.get("CURRENT") {
            None => json!({}),
            Some(value) if !model::truthy(value) => json!({}),
            Some(value) if value.is_object() => value.clone(),
            Some(_) => continue,
        };
        let explicit = scale_value(overrides.get(description));
        // The full manager displays the configured override when one exists.
        let value = explicit.or_else(|| scale_value(current.get("SCALE")));
        let enabled = current.get("ENABLED") == Some(&Value::Bool(true));
        let unique = names[connector] == 1 && descriptions[description] == 1;
        let identity: Vec<Value> = ["NAME", "DESCRIPTION", "MAKE", "MODEL", "SERIAL_NUMBER"].iter()
            .map(|k| head.get(*k).cloned().unwrap_or(Value::Null)).collect();
        let encoded = json_dumps(&Value::from(identity), &JsonStyle { sort_keys: true, ..JsonStyle::DEFAULT });
        let scale_id: String = Sha256::digest(encoded.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let error = if !unique {
            "This display cannot be identified uniquely."
        } else if !enabled {
            "Enable this display to change its scale."
        } else if value.is_none() {
            "The display service did not report a valid scale."
        } else {
            ""
        };
        displays.push(json!({"connector": connector, "name": description, "enabled": enabled, "scale_id": scale_id,
            "scale": {"supported": unique && enabled && value.is_some(), "value": value, "busy": false,
                      "override": explicit.is_some(), "error": error}}));
    }
    Ok(Snapshot { auto_scale: config.get("AUTO_SCALE") == Some(&Value::Bool(true)), displays })
}

/// What `merge_displays` needs to know about the scale service.
pub trait ScaleView {
    fn scale_ready(&self) -> bool;
    fn scale_busy(&self) -> bool;
    fn scale_error(&self) -> String;
    fn scale_displays(&self) -> Vec<Value>;
}

/// Keep hardware identities separate from the scale daemon's identities.
pub fn merge_displays(hardware: &[Value], scaling: Option<&dyn ScaleView>) -> Vec<Value> {
    let mut result: Vec<Value> = hardware.to_vec();
    if let Some(scaling) = scaling {
        for display in scaling.scale_displays() {
            let connector = model::text(&display, "connector").to_owned();
            let index = match result.iter().position(|row| model::text(row, "connector") == connector) {
                Some(index) => index,
                None => {
                    let feature = json!({"supported": false, "value": null, "raw": null, "max": null, "busy": false,
                                         "error": UNAVAILABLE_CONTROL});
                    result.push(json!({"connector": connector, "name": display["name"], "enabled": display["enabled"],
                        "id": format!("scale:{}", model::text(&display, "scale_id")), "method": "unsupported", "geometry": null,
                        "error": "", "brightness": feature.clone(), "contrast": feature}));
                    result.len() - 1
                }
            };
            let row = &mut result[index];
            row["scale_id"] = display["scale_id"].clone();
            row["scale"] = display["scale"].clone();
            row["scale"]["busy"] = scaling.scale_busy().into();
            if !scaling.scale_ready() {
                let error = scaling.scale_error();
                row["scale"]["supported"] = false.into();
                row["scale"]["error"] = if error.is_empty() { UNAVAILABLE.into() } else { error.into() };
            }
        }
    }
    let fallback_error = scaling.map(|s| s.scale_error()).unwrap_or_else(|| UNAVAILABLE.into());
    for row in &mut result {
        if row.get("scale_id").is_none() {
            row["scale_id"] = "".into();
        }
        if row.get("scale").is_none() {
            row["scale"] = json!({"supported": false, "value": null, "busy": false, "override": false, "error": fallback_error});
        }
    }
    result
}

pub type Requester = Arc<dyn Fn(&str, Option<&Value>) -> Result<Value, String> + Send + Sync>;

enum Work {
    Get,
    Scale(String, f64, String),
    Auto(bool),
}

struct Finished {
    get: bool,
    value: Option<Snapshot>,
    error: String,
}

fn work(request: &Requester, closed: &AtomicBool, job: &Work) -> Result<Snapshot, String> {
    let current = snapshot(&request("GET", None)?)?;
    if matches!(job, Work::Get) || closed.load(Ordering::SeqCst) {
        return Ok(current);
    }
    match job {
        Work::Scale(connector, value, expected_id) => {
            let display = current.displays.iter().find(|d| {
                model::text(d, "connector") == connector && model::text(d, "scale_id") == expected_id && model::supported(d, "scale")
            }).ok_or_else(|| "The connected display changed before its scale could be applied.".to_owned())?;
            let mut scale = Map::new();
            scale.insert(model::text(display, "name").into(), json!(value));
            request("CFG_SET", Some(&json!({"SCALE": scale})))?;
        }
        Work::Auto(enabled) => {
            request("CFG_SET", Some(&json!({"AUTO_SCALE": enabled})))?;
            if !enabled {
                // Same behavior as the display manager: disabling automatic
                // scaling clears overrides only for currently attached heads.
                let mut overrides = Map::new();
                for item in &current.displays {
                    let value = item["scale"]["value"].as_f64().filter(|v| *v != 0.0).unwrap_or(1.0);
                    overrides.insert(model::text(item, "name").into(), json!(value));
                }
                if !overrides.is_empty() {
                    request("CFG_DEL", Some(&json!({"SCALE": overrides})))?;
                }
            }
        }
        Work::Get => unreachable!(),
    }
    request("CFG_WRITE", None).map_err(|error| format!("Scaling changed, but the display settings could not be saved: {error}"))?;
    snapshot(&request("GET", None)?)
}

pub struct ScaleOptions {
    pub request: Requester,
    pub scheduler: Rc<dyn Scheduler>,
    pub spawn_events: bool,
}

pub struct Inner {
    changed: Box<dyn Fn()>,
    options: RefCell<Option<ScaleOptions>>,
    scheduler: Rc<dyn Scheduler>,
    ready: Cell<bool>,
    busy: Cell<bool>,
    auto_scale: Cell<bool>,
    error: RefCell<String>,
    displays: RefCell<Vec<Value>>,
    jobs: RefCell<Option<mpsc::Sender<Work>>>,
    events: RefCell<Option<async_channel::Receiver<Finished>>>,
    closed: Cell<bool>,
    worker_closed: Arc<AtomicBool>,
    started: Cell<bool>,
    refresh_pending: Cell<bool>,
    poll: Cell<Option<u64>>,
}

#[derive(Clone)]
pub struct ScaleBackend(pub Rc<Inner>);

impl ScaleView for ScaleBackend {
    fn scale_ready(&self) -> bool { self.0.ready.get() }
    fn scale_busy(&self) -> bool { self.0.busy.get() }
    fn scale_error(&self) -> String { self.0.error.borrow().clone() }
    fn scale_displays(&self) -> Vec<Value> { self.0.displays.borrow().clone() }
}

impl ScaleBackend {
    pub fn new(changed: impl Fn() + 'static, options: ScaleOptions) -> Self {
        Self(Rc::new(Inner {
            changed: Box::new(changed), scheduler: options.scheduler.clone(), options: RefCell::new(Some(options)),
            ready: Cell::new(false), busy: Cell::new(false), auto_scale: Cell::new(false), error: RefCell::new(String::new()),
            displays: RefCell::new(Vec::new()), jobs: RefCell::new(None), events: RefCell::new(None), closed: Cell::new(false),
            worker_closed: Arc::new(AtomicBool::new(false)), started: Cell::new(false), refresh_pending: Cell::new(false),
            poll: Cell::new(None),
        }))
    }

    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn busy(&self) -> bool { self.0.busy.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn displays(&self) -> Vec<Value> { self.0.displays.borrow().clone() }
    pub fn refresh_pending(&self) -> bool { self.0.refresh_pending.get() }
    pub fn poll_timer(&self) -> Option<u64> { self.0.poll.get() }

    pub fn status(&self) -> Value {
        json!({"ready": self.0.ready.get(), "busy": self.0.busy.get(), "error": *self.0.error.borrow(),
               "auto_scale": self.0.auto_scale.get()})
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub fn start(&self) {
        if self.0.started.get() || self.0.closed.get() {
            return;
        }
        let Some(options) = self.0.options.take() else { return };
        self.0.started.set(true);
        let (jobs, receiver) = mpsc::channel::<Work>();
        let (events, event_receiver) = async_channel::unbounded();
        let (request, closed) = (options.request.clone(), self.0.worker_closed.clone());
        std::thread::Builder::new().name("display-scaling".into()).spawn(move || {
            while let Ok(job) = receiver.recv() {
                if closed.load(Ordering::SeqCst) {
                    return;
                }
                let get = matches!(job, Work::Get);
                let (value, error) = match work(&request, &closed, &job) {
                    Ok(value) => (Some(value), String::new()),
                    Err(error) => {
                        let value = if get { None } else { request("GET", None).ok().and_then(|data| snapshot(&data).ok()) };
                        (value, error)
                    }
                };
                let _ = events.send_blocking(Finished { get, value, error });
            }
        }).expect("display-scaling thread");
        self.0.jobs.replace(Some(jobs));
        if options.spawn_events {
            let (weak, receiver) = (self.weak(), event_receiver.clone());
            glib::MainContext::default().spawn_local(async move {
                while let Ok(event) = receiver.recv().await {
                    let Some(inner) = weak.upgrade() else { return };
                    ScaleBackend(inner).finished(event);
                }
            });
        }
        self.0.events.replace(Some(event_receiver));
        self.refresh();
        self.arm_poll();
    }

    fn arm_poll(&self) {
        let weak = self.weak();
        let id = self.0.scheduler.timeout(5000, Box::new(move || {
            let Some(inner) = weak.upgrade() else { return };
            let backend = ScaleBackend(inner);
            backend.0.poll.set(None);
            backend.refresh();
            if !backend.0.closed.get() {
                backend.arm_poll();
            }
        }));
        self.0.poll.set(Some(id));
    }

    /// Deliver finished requests (tests; production uses a main-context task).
    pub fn pump(&self) {
        loop {
            let event = match self.0.events.borrow().as_ref().map(|r| r.try_recv()) {
                Some(Ok(event)) => event,
                _ => return,
            };
            self.finished(event);
        }
    }

    pub fn refresh(&self) {
        if self.0.closed.get() || !self.0.started.get() || self.0.refresh_pending.get() || self.0.busy.get() {
            return;
        }
        self.0.refresh_pending.set(true);
        self.send(Work::Get);
    }

    fn send(&self, job: Work) {
        if let Some(jobs) = self.0.jobs.borrow().as_ref() {
            let _ = jobs.send(job);
        }
    }

    fn reject(&self, error: &str) -> bool {
        self.0.error.replace(error.into());
        (self.0.changed)();
        false
    }

    pub fn set_scale(&self, connector: &str, value: f64, expected_id: &str) -> bool {
        let value = match validated_scale(value) {
            Ok(value) => value,
            Err(error) => return self.reject(&error),
        };
        let available = self.0.displays.borrow().iter().any(|d| {
            model::text(d, "connector") == connector && model::text(d, "scale_id") == expected_id && model::supported(d, "scale")
        });
        if !available {
            return self.reject("This display is no longer available for scaling. Refresh and try again.");
        }
        self.enqueue(Work::Scale(connector.into(), value, expected_id.into()))
    }

    pub fn set_auto_scale(&self, enabled: bool) -> bool {
        self.enqueue(Work::Auto(enabled))
    }

    /// Non-bool D-Bus/JSON input (`type(enabled) is not bool`).
    pub fn reject_auto_scale(&self) -> bool {
        self.reject("Automatic scaling must be on or off.")
    }

    fn enqueue(&self, job: Work) -> bool {
        if self.0.closed.get() || !self.0.started.get() || !self.0.ready.get() {
            return self.reject(UNAVAILABLE);
        }
        if self.0.busy.get() {
            return self.reject("A display scaling change is already in progress.");
        }
        self.0.busy.set(true);
        self.0.error.replace(String::new());
        (self.0.changed)();
        self.send(job);
        true
    }

    fn finished(&self, event: Finished) {
        if self.0.closed.get() {
            return;
        }
        if event.get {
            self.0.refresh_pending.set(false);
        } else {
            self.0.busy.set(false);
        }
        match event.value {
            Some(value) => {
                self.0.displays.replace(value.displays);
                self.0.auto_scale.set(value.auto_scale);
                self.0.ready.set(true);
            }
            None => self.0.ready.set(false),
        }
        self.0.error.replace(event.error);
        (self.0.changed)();
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.worker_closed.store(true, Ordering::SeqCst);
        if let Some(id) = self.0.poll.take() {
            self.0.scheduler.cancel(id);
        }
        self.0.jobs.replace(None);
        self.0.ready.set(false);
        self.0.busy.set(false);
    }
}
