//! status_backend.py: shared, asynchronous desktop indicators for every
//! Tessel Dock monitor (one monitor per status for all dock clones).
//!
//! Public API (UI agent):
//! - `StatusBackend::new(on_changed, on_error, StatusOptions)`; `StatusOptions::default()`
//!   is the real session (`~/.config/waybar/scripts`, `ProcessJob`, `GlibScheduler`,
//!   `Services::real()`); `.start()`, `.close()`.
//! - `.status() -> Map<name, indicator>` / `.get(name)`; indicator objects are
//!   Python's dicts: `{"text","tooltip","classes":[..],"class":[..],"visible"}` (+ `"state"`
//!   for screen_record; media has the MediaStatus keys; discovery adds nothing beyond `visible`).
//! - `.refresh(Some(name))` / `.refresh(None)`, `.action(name) -> bool` (`idle_inhibitor`,
//!   `showdesktop`, `media-previous|toggle|next`), `.set_show_desktop(bool)`,
//!   `.set_on_show_desktop(cb)`, `.audio_scroll(step)`, `.media_icon()`.
//! - pure helpers: `indicator`, `plain_text`, `audio_status`, `bluetooth_status`,
//!   `status_title`, `ICONS`, `STREAMS`.
//! `on_changed` is coalesced through a 0 ms scheduler timer and only fires when
//! some status value really changed.

use super::process::{DoneHandler, JobFactory, JobOptions, LineHandler, ProcessJob, StatusJob};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use tessel_core::i18n::{translate, translate_with};
use tessel_core::pyfmt::Arg;
use tessel_core::scheduler::{GlibScheduler, Scheduler};
use tessel_core::tr;

pub type Status = Map<String, Value>;

/// `ICONS` in Python dict order (refresh order).
pub const ICONS: [(&str, &str); 16] = [
    ("power", "\u{f135}"), ("battery", "\u{f244}"), ("brightness", "\u{f108}"), ("media", "\u{f001}"),
    ("discovery", "\u{f1e0}"), ("network", "\u{f6ff}"), ("capslock", ""), ("bluetooth", "\u{f294}"),
    ("audio", "\u{f028}"), ("weather", "\u{f0c2}"), ("idle_inhibitor", "\u{f070}"), ("showdesktop", "\u{f2d2}"),
    ("notifications", "\u{f0f3}"), ("screen_record", "\u{f03d}"), ("keyboard", "\u{f11c}"), ("tiling", "▦"),
];

pub const STREAMS: [(&str, &str); 9] = [
    ("power", "tessel-power/power_status.py"), ("battery", "tessel-battery/battery_status.py"),
    ("brightness", "tessel-brightness/brightness_status.py"), ("network", "tessel-network/network_status.py"),
    ("capslock", "capslock.py"), ("notifications", "tessel-notifications/notification_status.py"),
    ("weather", "tessel-weather/weather_status.py"), ("keyboard", "tessel-keyboard/osk_status.py"),
    ("tiling", "tessel-tiling/tiling_status.py"),
];

pub const WAYCAPTURE: &str = "/usr/local/bin/tessel-capture";

pub fn icon(name: &str) -> &'static str {
    ICONS.iter().find(|(key, _)| *key == name).map(|(_, icon)| *icon).unwrap_or("")
}

pub fn stream(name: &str) -> Option<&'static str> {
    STREAMS.iter().find(|(key, _)| *key == name).map(|(_, path)| *path)
}

/// Python `str.title()`.
fn py_title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_cased = false;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            if previous_cased { out.extend(ch.to_lowercase()) } else { out.extend(ch.to_uppercase()) }
            previous_cased = true;
        } else {
            out.push(ch);
            previous_cased = false;
        }
    }
    out
}

pub fn status_title(name: &str) -> String {
    match name {
        "discovery" => tr!("Discover Network Devices"),
        "brightness" => tr!("Displays"),
        _ => py_title(&name.replace('_', " ")),
    }
}

/// Convert the trusted scripts' Pango spans to safe plain GTK label text.
pub fn plain_text(value: &str) -> String {
    super::html::unescape(&super::html::strip_tags(value))
}

fn valid_class(item: &str) -> bool {
    !item.is_empty() && item.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `indicator(value, hide_empty=)`.
pub fn indicator(value: &Value, hide_empty: bool) -> Result<Status, String> {
    let Some(object) = value.as_object() else { return Err("expected indicator JSON with text".into()) };
    let Some(Value::String(text)) = object.get("text") else { return Err("expected indicator JSON with text".into()) };
    let tooltip = match object.get("tooltip") {
        None => String::new(),
        Some(Value::String(tooltip)) => tooltip.clone(),
        Some(_) => return Err("invalid indicator tooltip".into()),
    };
    let classes: Vec<String> = match object.get("class") {
        None => Vec::new(),
        Some(Value::String(class)) => if class.is_empty() { Vec::new() } else { vec![class.clone()] },
        Some(Value::Array(items)) => {
            let mut classes = Vec::new();
            for item in items {
                match item {
                    Value::String(item) => classes.push(item.clone()),
                    _ => return Err("invalid indicator classes".into()),
                }
            }
            classes
        }
        Some(_) => return Err("invalid indicator classes".into()),
    };
    let classes: Vec<Value> = classes.iter().filter(|item| valid_class(item)).map(|item| Value::String(item.to_lowercase())).collect();
    let text = plain_text(text);
    let visible = Value::Bool(if hide_empty { !text.is_empty() } else { true });
    let mut result = Map::new();
    result.insert("text".into(), Value::String(text));
    result.insert("tooltip".into(), Value::String(plain_text(&tooltip)));
    result.insert("classes".into(), Value::Array(classes.clone()));
    result.insert("class".into(), Value::Array(classes));
    result.insert("visible".into(), visible);
    Ok(result)
}

/// `wpctl get-volume` line → indicator.
pub fn audio_status(line: &str) -> Result<Status, String> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| regex::Regex::new(r"^Volume:\s+(\d+(?:\.\d+)?)\s*(\[MUTED\])?\s*$").unwrap());
    let Some(captures) = pattern.captures(line) else { return Err("invalid PipeWire volume response".into()) };
    let digits: String = captures[1].chars().map(|c| c.to_digit(10).map(|d| char::from(b'0' + d as u8)).unwrap_or(c)).collect();
    let volume = (digits.parse::<f64>().map_err(|_| "invalid PipeWire volume response".to_string())? * 100.0).round_ties_even();
    let muted = captures.get(2).is_some();
    if !(0.0..=10000.0).contains(&volume) {
        return Err("invalid PipeWire volume".into());
    }
    let volume = volume as i64;
    let icon = if muted { "\u{f6a9}" } else if volume == 0 { "\u{f026}" } else if volume < 50 { "\u{f027}" } else { "\u{f028}" };
    let tooltip = if muted { tr!("Volume: {volume}% (muted)", volume = volume) } else { tr!("Volume: {volume}%", volume = volume) };
    indicator(&json!({"text": icon, "tooltip": tooltip, "class": if muted { "muted" } else { "audio" }}), false)
}

/// BlueZ `GetManagedObjects` (as `{path: {interface: {prop: value}}}`) → indicator.
pub fn bluetooth_status(objects: &Value) -> Status {
    let empty = Map::new();
    let objects = objects.as_object().unwrap_or(&empty);
    let truthy = |value: Option<&Value>| value.is_some_and(tessel_core::pyjson::truthy);
    let adapters: Vec<&Value> = objects.values().filter_map(|interfaces| interfaces.get("org.bluez.Adapter1")).collect();
    let mut connected = Vec::new();
    for interfaces in objects.values() {
        let Some(properties) = interfaces.get("org.bluez.Device1") else { continue };
        if truthy(properties.get("Connected")) {
            let name = tessel_core::pyjson::first_truthy(&[properties.get("Alias"), properties.get("Name")])
                .map(tessel_core::pyjson::py_str).unwrap_or_else(|| tr!("Bluetooth device"));
            connected.push(name);
        }
    }
    let state = if !connected.is_empty() { "connected" }
        else if adapters.iter().any(|item| truthy(item.get("Powered"))) { "on" }
        else if !adapters.is_empty() { "off" } else { "unavailable" };
    let value = if adapters.is_empty() { tr!("no adapter") } else { translate(state) };
    let mut tooltip = translate_with("Bluetooth: {value1}", &[("value1", Arg::from(value.as_str()))]);
    if !connected.is_empty() {
        tooltip.push('\n');
        tooltip.push_str(&connected.join("\n"));
    }
    indicator(&json!({"text": if connected.is_empty() { "\u{f294}" } else { "\u{f293}" }, "tooltip": tooltip, "class": state}), false)
        .expect("valid bluetooth indicator")
}

// ---------------------------------------------------------------------------
// Injectable services (Python patches SettingsClient / DiscoveryBackend /
// _refresh_bluetooth in tests; capture/media are always real there).

pub trait KeepAwakeSource {
    fn start(&self);
    fn state(&self) -> Map<String, Value>;
    fn close(&self);
}

pub trait DiscoverySource {
    fn start(&self);
    fn snapshot(&self) -> Map<String, Value>;
    fn close(&self);
}

pub trait CaptureSource {
    fn close(&self);
}

pub trait MediaSource {
    fn start(&self);
    fn action(&self, command: &str) -> bool;
    fn icon(&self) -> Option<gio::Icon>;
    fn close(&self);
}

/// `GetManagedObjects` on the system bus; `read` replies with the unpacked dict or `str(GLib.Error)`.
pub trait BluetoothReader {
    fn read(&self, done: Box<dyn FnOnce(Result<Value, String>)>);
    fn cancel(&self);
}

pub type Notify = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct Services {
    pub keep_awake: Rc<dyn Fn(Notify) -> Rc<dyn KeepAwakeSource>>,
    pub discovery: Rc<dyn Fn(Notify) -> Rc<dyn DiscoverySource>>,
    pub capture: Rc<dyn Fn(Rc<dyn Fn(&str)>) -> Rc<dyn CaptureSource>>,
    pub media: Rc<dyn Fn(Rc<dyn Fn(Status)>) -> Rc<dyn MediaSource>>,
    pub bluetooth: Rc<dyn BluetoothReader>,
    pub tessel_capture_exists: Rc<dyn Fn() -> bool>,
}

struct RealKeepAwake(tessel_core::service_settings::SettingsClient);
impl KeepAwakeSource for RealKeepAwake {
    fn start(&self) { self.0.start() }
    fn state(&self) -> Map<String, Value> { self.0.state() }
    fn close(&self) { self.0.close() }
}

struct RealDiscovery(tessel_network_discovery::backend::DiscoveryBackend);
impl DiscoverySource for RealDiscovery {
    fn start(&self) { self.0.start() }
    fn snapshot(&self) -> Map<String, Value> { self.0.snapshot() }
    fn close(&self) { self.0.close() }
}

impl CaptureSource for super::capture::CaptureStatus {
    fn close(&self) { super::capture::CaptureStatus::close(self) }
}

/// MediaStatus `changed` payload as Python's dict.
pub fn media_value(media: &tessel_media::dock_status::DockMedia) -> Status {
    let mut value = Map::new();
    value.insert("text".into(), media.text.clone().into());
    value.insert("tooltip".into(), media.tooltip.clone().into());
    value.insert("visible".into(), media.visible.into());
    value.insert("title".into(), media.title.clone().into());
    value.insert("artist".into(), media.artist.clone().into());
    value.insert("player".into(), media.player.clone().into());
    value.insert("playing".into(), media.playing.into());
    value.insert("can_toggle".into(), media.can_toggle.into());
    value.insert("can_previous".into(), media.can_previous.into());
    value.insert("can_next".into(), media.can_next.into());
    value.insert("icon_revision".into(), media.icon_revision.into());
    value.insert("classes".into(), Value::Array(media.classes.iter().map(|c| Value::from(*c)).collect()));
    value
}

struct RealMedia(tessel_media::dock_status::MediaStatus);
impl MediaSource for RealMedia {
    fn start(&self) { self.0.start() }
    fn action(&self, command: &str) -> bool { self.0.action(command) }
    fn icon(&self) -> Option<gio::Icon> { self.0.icon() }
    fn close(&self) { self.0.close() }
}

/// The real system bus BlueZ reader (lazy `Gio.bus_get(SYSTEM)`).
#[derive(Default)]
pub struct SystemBluetooth {
    bus: Rc<RefCell<Option<gio::DBusConnection>>>,
    cancel: gio::Cancellable,
}

impl SystemBluetooth {
    fn call(bus: &gio::DBusConnection, cancel: &gio::Cancellable, done: Box<dyn FnOnce(Result<Value, String>)>) {
        bus.call(Some("org.bluez"), "/", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects", None,
                 Some(glib::VariantTy::new("(a{oa{sa{sv}}})").unwrap()), gio::DBusCallFlags::NONE, 5000, Some(cancel),
                 move |result| match result {
                     Ok(reply) => done(Ok(tessel_core::values::first(&reply))),
                     Err(error) => done(Err(tessel_core::pyerr::python_error_text(&error))),
                 });
    }
}

impl BluetoothReader for SystemBluetooth {
    fn read(&self, done: Box<dyn FnOnce(Result<Value, String>)>) {
        if let Some(bus) = self.bus.borrow().clone() {
            Self::call(&bus, &self.cancel, done);
            return;
        }
        let slot = self.bus.clone();
        let cancel = self.cancel.clone();
        gio::bus_get(gio::BusType::System, Some(&self.cancel), move |result| match result {
            Ok(bus) => {
                slot.replace(Some(bus.clone()));
                Self::call(&bus, &cancel, done);
            }
            Err(error) => done(Err(tessel_core::pyerr::python_error_text(&error))),
        });
    }
    fn cancel(&self) { gio::prelude::CancellableExt::cancel(&self.cancel) }
}

impl Services {
    /// The live session: tessel-keep-awake SettingsClient, tessel-network-discovery's
    /// DiscoveryBackend, tessel-capture's status, tessel-media's MediaStatus.
    pub fn real() -> Self {
        Self {
            keep_awake: Rc::new(|changed: Notify| {
                Rc::new(RealKeepAwake(tessel_core::service_settings::SettingsClient::new(move || changed(),
                    "org.tessel.KeepAwake", "org.tessel.KeepAwake.Controls", &tr!("Keep Awake is not running in this session."))))
                    as Rc<dyn KeepAwakeSource>
            }),
            discovery: Rc::new(|changed: Notify| {
                Rc::new(RealDiscovery(tessel_network_discovery::backend::DiscoveryBackend::new(move || changed()))) as Rc<dyn DiscoverySource>
            }),
            capture: Rc::new(|changed: Rc<dyn Fn(&str)>| {
                Rc::new(super::capture::CaptureStatus::new(move |state| changed(state))) as Rc<dyn CaptureSource>
            }),
            media: Rc::new(|changed: Rc<dyn Fn(Status)>| {
                Rc::new(RealMedia(tessel_media::dock_status::MediaStatus::new(move |media| changed(media_value(&media)))))
                    as Rc<dyn MediaSource>
            }),
            bluetooth: Rc::new(SystemBluetooth::default()),
            tessel_capture_exists: Rc::new(|| Path::new(WAYCAPTURE).is_file()),
        }
    }
}

pub struct StatusOptions {
    pub scripts: PathBuf,
    pub job_factory: JobFactory,
    pub scheduler: Rc<dyn Scheduler>,
    pub services: Services,
    pub on_show_desktop: Option<Rc<dyn Fn() -> bool>>,
}

impl Default for StatusOptions {
    fn default() -> Self {
        Self {
            scripts: tessel_core::paths::config_home().join("waybar/scripts"),
            job_factory: ProcessJob::factory(),
            scheduler: Rc::new(GlibScheduler::default()),
            services: Services::real(),
            on_show_desktop: None,
        }
    }
}

// ---------------------------------------------------------------------------

struct Inner {
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    on_show_desktop: RefCell<Option<Rc<dyn Fn() -> bool>>>,
    show_desktop: Cell<bool>,
    scripts: PathBuf,
    job_factory: JobFactory,
    scheduler: Rc<dyn Scheduler>,
    services: Services,
    status: RefCell<Status>,
    jobs: RefCell<HashMap<String, (u64, Rc<dyn StatusJob>)>>,
    job_serial: Cell<u64>,
    sources: RefCell<HashMap<String, u64>>,
    /// `self.errors` in dict insertion order: `_set` pops, `_failed` re-inserts at the end.
    errors: RefCell<Vec<(String, String)>>,
    started: Cell<bool>,
    closed: Cell<bool>,
    audio_steps: Cell<i64>,
    bus_pending: Cell<bool>,
    capture_status: RefCell<Option<Rc<dyn CaptureSource>>>,
    media_status: RefCell<Option<Rc<dyn MediaSource>>>,
    keep_awake_status: RefCell<Option<Rc<dyn KeepAwakeSource>>>,
    discovery_status: RefCell<Option<Rc<dyn DiscoverySource>>>,
}

#[derive(Clone)]
pub struct StatusBackend(Rc<Inner>);

fn unavailable(name: &str, tooltip: String) -> Status {
    indicator(&json!({"text": icon(name), "tooltip": tooltip, "class": "unavailable"}), name == "capslock").expect("valid")
}

impl StatusBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: Option<Box<dyn Fn(&str)>>, options: StatusOptions) -> Self {
        let mut status = Map::new();
        for (name, _) in ICONS {
            status.insert(name.into(), Value::Object(unavailable(name, format!("{}: waiting for status", status_title(name)))));
        }
        Self(Rc::new(Inner {
            on_changed: Box::new(on_changed), on_error: on_error.unwrap_or_else(|| Box::new(|_| {})),
            on_show_desktop: RefCell::new(options.on_show_desktop), show_desktop: Cell::new(false),
            scripts: options.scripts, job_factory: options.job_factory, scheduler: options.scheduler,
            services: options.services, status: RefCell::new(status), jobs: RefCell::default(), job_serial: Cell::new(0),
            sources: RefCell::default(), errors: RefCell::default(), started: Cell::new(false), closed: Cell::new(false),
            audio_steps: Cell::new(0), bus_pending: Cell::new(false), capture_status: RefCell::new(None),
            media_status: RefCell::new(None), keep_awake_status: RefCell::new(None), discovery_status: RefCell::new(None),
        }))
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    pub fn status(&self) -> Status { self.0.status.borrow().clone() }

    pub fn get(&self, name: &str) -> Option<Status> {
        self.0.status.borrow().get(name).and_then(|v| v.as_object().cloned())
    }

    pub fn is_closed(&self) -> bool { self.0.closed.get() }

    /// `dict(self.errors)`: current failures (`name → "<Title>: <message>"`)
    /// in Python's insertion order (most recent failure of each name last;
    /// a later successful status removes the entry).
    pub fn errors(&self) -> Vec<(String, String)> { self.0.errors.borrow().clone() }

    pub fn set_on_show_desktop(&self, callback: Option<Rc<dyn Fn() -> bool>>) {
        self.0.on_show_desktop.replace(callback);
    }

    /// The MediaStatus player icon (media_controls.py reads `media_status.icon`).
    pub fn media_icon(&self) -> Option<gio::Icon> {
        self.0.media_status.borrow().as_ref().and_then(|media| media.icon())
    }

    /// Names of running jobs (`"power"`, `"action-audio"` …) — for tests/diagnostics.
    pub fn job_names(&self) -> Vec<String> { self.0.jobs.borrow().keys().cloned().collect() }

    /// Pending scheduler id for a named timer (`"changed"`, `"power"` …).
    pub fn source(&self, name: &str) -> Option<u64> { self.0.sources.borrow().get(name).copied() }

    fn later(&self, name: &str, milliseconds: u32, callback: impl FnOnce(&StatusBackend) + 'static) {
        if self.0.closed.get() || self.0.sources.borrow().contains_key(name) {
            return;
        }
        let weak = self.weak();
        let key = name.to_owned();
        let id = self.0.scheduler.timeout(milliseconds, Box::new(move || {
            let Some(inner) = weak.upgrade() else { return };
            let backend = StatusBackend(inner);
            backend.0.sources.borrow_mut().remove(&key);
            if !backend.0.closed.get() {
                callback(&backend);
            }
        }));
        self.0.sources.borrow_mut().insert(name.to_owned(), id);
    }

    fn cancel_timer(&self, name: &str) {
        let source = self.0.sources.borrow_mut().remove(name);
        if let Some(source) = source {
            self.0.scheduler.cancel(source);
        }
    }

    fn set(&self, name: &str, value: Status) {
        self.0.errors.borrow_mut().retain(|(key, _)| key != name);
        if self.0.closed.get() {
            return;
        }
        let value = Value::Object(value);
        let changed = self.0.status.borrow().get(name) != Some(&value);
        if changed {
            self.0.status.borrow_mut().insert(name.to_owned(), value);
            self.later("changed", 0, |backend| (backend.0.on_changed)());
        }
    }

    fn failed(&self, name: &str, message: &str) {
        if self.0.closed.get() {
            return;
        }
        let error = format!("{}: {}", status_title(name), message);
        let previous = self.0.errors.borrow().iter().find(|(key, _)| key == name).map(|(_, e)| e.clone());
        self.set(name, unavailable(name, error.clone()));
        self.0.errors.borrow_mut().push((name.to_owned(), error.clone()));
        if previous.as_deref() != Some(error.as_str()) {
            (self.0.on_error)(&error);
        }
    }

    pub fn start(&self) {
        if self.0.started.get() || self.0.closed.get() {
            return;
        }
        self.0.started.set(true);
        let weak = self.weak();
        let capture = (self.0.services.capture)(Rc::new(move |state: &str| {
            if let Some(inner) = weak.upgrade() { StatusBackend(inner).capture_changed(state) }
        }));
        self.0.capture_status.replace(Some(capture));
        self.capture_changed("idle");
        let weak = self.weak();
        let media = (self.0.services.media)(Rc::new(move |value: Status| {
            if let Some(inner) = weak.upgrade() { StatusBackend(inner).set("media", value) }
        }));
        self.0.media_status.replace(Some(media.clone()));
        media.start();
        self.refresh(None);
    }

    fn capture_changed(&self, state: &str) {
        let text = match state {
            "preparing" => "Preparing recording · click to cancel",
            "recording" => "Recording · click to stop and save",
            "stopping" => "Saving recording…",
            _ => "Screen Record · click for presets · right-click for settings",
        };
        let idle = state == "idle";
        let mut value = indicator(&json!({"text": if idle { icon("screen_record") } else { "\u{f04d}" },
                                          "tooltip": translate(text), "class": if idle { "" } else { "recording" }}), false)
            .expect("valid");
        value.insert("state".into(), state.into());
        value.insert("visible".into(), (self.0.services.tessel_capture_exists)().into());
        self.set("screen_record", value);
    }

    pub fn refresh(&self, name: Option<&str>) {
        if self.0.closed.get() {
            return;
        }
        let names: Vec<&str> = match name { Some(name) => vec![name], None => ICONS.iter().map(|(n, _)| *n).collect() };
        for key in names {
            self.cancel_timer(key);
            if key == "bluetooth" {
                self.refresh_bluetooth();
            } else if let Some(path) = stream(key) {
                let mut command = vec!["/usr/bin/python3".to_owned(), "-u".to_owned(), self.0.scripts.join(path).to_string_lossy().into_owned()];
                if key == "weather" {
                    command.push("--watch".into());
                }
                self.command(key, command, true, 3000, 10000);
            } else if key == "audio" {
                self.command(key, ["/usr/bin/wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"].map(String::from).to_vec(), false, 2000, 10000);
            } else if key == "idle_inhibitor" {
                if self.0.keep_awake_status.borrow().is_none() {
                    let weak = self.weak();
                    let client = (self.0.services.keep_awake)(Rc::new(move || {
                        if let Some(inner) = weak.upgrade() { StatusBackend(inner).keep_awake_changed() }
                    }));
                    self.0.keep_awake_status.replace(Some(client.clone()));
                    client.start();
                }
            } else if key == "discovery" {
                if self.0.discovery_status.borrow().is_none() {
                    let weak = self.weak();
                    let monitor = (self.0.services.discovery)(Rc::new(move || {
                        if let Some(inner) = weak.upgrade() { StatusBackend(inner).discovery_changed() }
                    }));
                    self.0.discovery_status.replace(Some(monitor.clone()));
                    monitor.start();
                }
            } else if key == "showdesktop" {
                self.set_show_desktop(self.0.show_desktop.get());
            }
        }
    }

    /// Discovery observer callback (public for tests that drive a fake source).
    pub fn discovery_changed(&self) {
        if self.0.closed.get() {
            return;
        }
        let Some(monitor) = self.0.discovery_status.borrow().clone() else { return };
        let state = monitor.snapshot();
        let Ok(mut value) = indicator(&Value::Object(state.clone()), false) else { return };
        let truthy = |key: &str| state.get(key).is_some_and(tessel_core::pyjson::truthy);
        value.insert("visible".into(), Value::Bool(truthy("installed") || !truthy("ready")));
        self.set("discovery", value);
    }

    /// Keep-awake SettingsClient callback (public for tests).
    pub fn keep_awake_changed(&self) {
        if self.0.closed.get() {
            return;
        }
        let Some(client) = self.0.keep_awake_status.borrow().clone() else { return };
        let state = client.state();
        let source = if matches!(state.get("text"), Some(Value::String(_))) {
            Value::Object(state)
        } else {
            let error = state.get("error").filter(|v| tessel_core::pyjson::truthy(v)).map(tessel_core::pyjson::py_str)
                .unwrap_or_else(|| tr!("Keep Awake is not running in this session."));
            json!({"text": icon("idle_inhibitor"), "class": "unavailable", "tooltip": error})
        };
        if let Ok(value) = indicator(&source, false) {
            self.set("idle_inhibitor", value);
        }
    }

    fn next_serial(&self) -> u64 {
        self.0.job_serial.set(self.0.job_serial.get() + 1);
        self.0.job_serial.get()
    }

    fn is_current(&self, name: &str, serial: u64) -> bool {
        self.0.jobs.borrow().get(name).is_some_and(|(current, _)| *current == serial)
    }

    fn command(&self, name: &str, argv: Vec<String>, stream: bool, interval: u32, timeout: u32) {
        if self.0.closed.get() || self.0.jobs.borrow().contains_key(name) {
            return;
        }
        let serial = self.next_serial();
        let received = Rc::new(Cell::new(false));
        let (weak, key, got) = (self.weak(), name.to_owned(), received.clone());
        let line: LineHandler = Rc::new(move |text: &str| {
            let Some(inner) = weak.upgrade() else { return Ok(()) };
            let backend = StatusBackend(inner);
            if backend.0.closed.get() || !backend.is_current(&key, serial) {
                return Ok(());
            }
            let value = if key == "audio" {
                audio_status(text)?
            } else {
                let parsed: Value = serde_json::from_str(text).map_err(|error| python_json_error(text, &error))?;
                indicator(&parsed, key == "capslock")?
            };
            got.set(true);
            backend.set(&key, value);
            Ok(())
        });
        let (weak, key) = (self.weak(), name.to_owned());
        let done: DoneHandler = Rc::new(move |error: Option<String>| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = StatusBackend(inner);
            if backend.0.closed.get() || !backend.is_current(&key, serial) {
                return;
            }
            backend.0.jobs.borrow_mut().remove(&key);
            if error.is_some() || !received.get() || stream {
                let message = error.clone().unwrap_or_else(|| if stream { "status monitor stopped" } else { "no status returned" }.into());
                backend.failed(&key, &message);
            }
            let delay = if stream { 3000 } else if error.is_some() { interval.min(60000) } else { interval };
            let name = key.clone();
            backend.later(&key, delay, move |backend| backend.refresh(Some(&name)));
        });
        let job = (self.0.job_factory)(argv, line, done, JobOptions { stream, timeout });
        self.0.jobs.borrow_mut().insert(name.to_owned(), (serial, job.clone()));
        job.start();
    }

    fn refresh_bluetooth(&self) {
        if self.0.closed.get() || self.0.bus_pending.get() {
            return;
        }
        self.0.bus_pending.set(true);
        let weak = self.weak();
        self.0.services.bluetooth.read(Box::new(move |result| {
            let Some(inner) = weak.upgrade() else { return };
            StatusBackend(inner).bluetooth_done(result);
        }));
    }

    fn bluetooth_done(&self, result: Result<Value, String>) {
        self.0.bus_pending.set(false);
        if self.0.closed.get() {
            return;
        }
        match result {
            Err(error) => self.failed("bluetooth", &error),
            Ok(objects) => self.set("bluetooth", bluetooth_status(&objects)),
        }
        self.later("bluetooth", 5000, |backend| backend.refresh_bluetooth());
    }

    fn run_action(&self, name: &str, argv: Vec<String>, refresh: &str) -> bool {
        let key = format!("action-{name}");
        if self.0.closed.get() || self.0.jobs.borrow().contains_key(&key) {
            return false;
        }
        let serial = self.next_serial();
        let (weak, action, refresh) = (self.weak(), name.to_owned(), refresh.to_owned());
        let job_key = key.clone();
        let done: DoneHandler = Rc::new(move |error: Option<String>| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = StatusBackend(inner);
            if backend.0.closed.get() || !backend.is_current(&job_key, serial) {
                return;
            }
            backend.0.jobs.borrow_mut().remove(&job_key);
            if let Some(error) = error {
                (backend.0.on_error)(&format!("{}: {}", status_title(&action), error));
            }
            backend.refresh(Some(&refresh));
            if action == "audio" {
                backend.flush_audio();
            }
        });
        let job = (self.0.job_factory)(argv, Rc::new(|_: &str| Ok(())), done, JobOptions { stream: false, timeout: 10000 });
        self.0.jobs.borrow_mut().insert(key, (serial, job.clone()));
        job.start();
        true
    }

    pub fn action(&self, name: &str) -> bool {
        if matches!(name, "media-previous" | "media-toggle" | "media-next") && !self.0.closed.get() {
            if let Some(media) = self.0.media_status.borrow().clone() {
                return media.action(name.trim_start_matches("media-"));
            }
        }
        if name == "idle_inhibitor" {
            let script = self.0.scripts.join("tessel-keep-awake/keep_awake.py").to_string_lossy().into_owned();
            return self.run_action(name, vec!["/usr/bin/python3".into(), script, "--toggle".into()], name);
        }
        if name == "showdesktop" && !self.0.closed.get() {
            if let Some(callback) = self.0.on_show_desktop.borrow().clone() {
                return callback();
            }
        }
        false
    }

    pub fn set_show_desktop(&self, active: bool) {
        self.0.show_desktop.set(active);
        let value = indicator(&json!({"text": icon("showdesktop"),
            "tooltip": if active { tr!("Restore windows") } else { tr!("Show desktop") },
            "class": if active { json!(["active", "solid"]) } else { json!("regular") }}), false).expect("valid");
        self.set("showdesktop", value);
    }

    /// Scroll steps (+ up / − down); fractional steps truncate like `int()`.
    pub fn audio_scroll(&self, step: f64) {
        if self.0.closed.get() || !step.is_finite() {
            return;
        }
        self.0.audio_steps.set((self.0.audio_steps.get() + step.trunc() as i64).clamp(-20, 20));
        self.flush_audio();
    }

    fn flush_audio(&self) {
        let steps = self.0.audio_steps.get();
        if steps == 0 || self.0.closed.get() || self.0.jobs.borrow().contains_key("action-audio") {
            return;
        }
        self.0.audio_steps.set(0);
        let amount = format!("{}%{}", steps.abs() * 5, if steps > 0 { '+' } else { '-' });
        self.run_action("audio", ["/usr/bin/wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@"].map(String::from)
            .into_iter().chain([amount]).collect(), "audio");
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        BluetoothReader::cancel(&*self.0.services.bluetooth);
        for slot in [self.0.capture_status.borrow().clone().map(|c| Box::new(move || c.close()) as Box<dyn Fn()>),
                     self.0.media_status.borrow().clone().map(|c| Box::new(move || c.close()) as Box<dyn Fn()>),
                     self.0.keep_awake_status.borrow().clone().map(|c| Box::new(move || c.close()) as Box<dyn Fn()>),
                     self.0.discovery_status.borrow().clone().map(|c| Box::new(move || c.close()) as Box<dyn Fn()>)]
            .into_iter().flatten() {
            slot();
        }
        let sources: Vec<u64> = self.0.sources.borrow_mut().drain().map(|(_, id)| id).collect();
        for source in sources {
            self.0.scheduler.cancel(source);
        }
        let jobs: Vec<Rc<dyn StatusJob>> = self.0.jobs.borrow_mut().drain().map(|(_, (_, job))| job).collect();
        for job in jobs {
            job.close();
        }
    }
}

/// Python `json.JSONDecodeError` text for the common shapes; other errors keep
/// the position Python would report.
pub fn python_json_error(text: &str, error: &serde_json::Error) -> String {
    let trimmed = text.trim_start_matches([' ', '\t', '\n', '\r']);
    let skipped = text.chars().count() - trimmed.chars().count();
    let position = |index: usize| -> String {
        let before: String = text.chars().take(index).collect();
        let line = before.matches('\n').count() + 1;
        let column = match before.rfind('\n') { Some(newline) => before[newline + 1..].chars().count() + 1, None => index + 1 };
        format!("line {line} column {column} (char {index})")
    };
    let starts_value = trimmed.chars().next().is_some_and(|c| matches!(c, '{' | '[' | '"' | '-' | '0'..='9'))
        || ["true", "false", "null", "NaN", "Infinity"].iter().any(|literal| trimmed.starts_with(literal));
    if trimmed.is_empty() || !starts_value {
        return format!("Expecting value: {}", position(skipped));
    }
    if error.classify() == serde_json::error::Category::Syntax && error.to_string().starts_with("trailing characters") {
        // serde reports the column of the first trailing character.
        let lines: Vec<&str> = text.split('\n').collect();
        let line = error.line().saturating_sub(1);
        let index: usize = lines.iter().take(line).map(|l| l.chars().count() + 1).sum::<usize>() + error.column().saturating_sub(1);
        return format!("Extra data: {}", position(index));
    }
    error.to_string()
}

#[cfg(test)]
mod tests;
