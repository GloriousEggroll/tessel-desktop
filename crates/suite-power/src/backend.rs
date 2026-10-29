//! Asynchronous power profiles, device batteries and logind power actions
//! (power_backend.py, one state machine for power-manager and, in
//! [`Mode::BatteriesOnly`], battery-manager).
//!
//! Startup and refresh are read-only. Profile changes and sleep happen only
//! through the explicit action methods. Automatic sleep never asks for
//! authorization or overrides idle/sleep inhibitors.

use crate::charge_limits::{self, Records};
use crate::devices::{battery_percentage, device_batteries, device_identity, is_system_battery, system_batteries};
pub use crate::devices::{UPOWER_DEVICE};
use suite_core::bus::{Bus, BusError, Call, Signal, PROPERTIES};
use suite_core::scheduler::{GlibScheduler, Scheduler};
use suite_core::values::{self, first, int, is_false, is_true, text};
use glib::prelude::*;
use glib::Variant;
use serde_json::{json, Map, Value};
use std::cell::{Cell, Ref, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::{Rc, Weak};

pub type Endpoint = (&'static str, &'static str, &'static str);
pub const PPD_ENDPOINTS: [Endpoint; 2] = [
    ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles", "org.freedesktop.UPower.PowerProfiles"),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles", "net.hadess.PowerProfiles"),
];
pub const UPOWER: Endpoint = ("org.freedesktop.UPower", "/org/freedesktop/UPower", "org.freedesktop.UPower");
pub const LOGIN: Endpoint = ("org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager");
pub const LOGIN_SESSION: &str = "org.freedesktop.login1.Session";
pub const SLEEP_METHODS: [(&str, &str); 3] = [("suspend", "Suspend"), ("hibernate", "Hibernate"),
                                             ("suspend-then-hibernate", "SuspendThenHibernate")];
pub const CAPABILITIES: [&str; 4] = ["yes", "challenge", "no", "na"];

/// Block weak inhibitors too; automatic actions must not bypass user intent.
pub fn inhibited(inhibitors: &Value, subjects: &[&str]) -> Result<bool, String> {
    let invalid = || "Invalid power inhibition response.".to_owned();
    let Value::Array(rows) = inhibitors else { return Err(invalid()) };
    for row in rows {
        let Value::Array(row) = row else { return Err(invalid()) };
        if row.len() != 6 {
            return Err(invalid());
        }
        let (Value::String(what), Value::String(mode)) = (&row[0], &row[3]) else { return Err(invalid()) };
        if (mode == "block" || mode == "block-weak") && what.split(':').any(|w| subjects.contains(&w)) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub type Capabilities = BTreeMap<String, String>;

pub fn unknown_capabilities() -> Capabilities {
    SLEEP_METHODS.iter().map(|(a, _)| (a.to_string(), "na".to_string())).collect()
}

/// Everything consumers read (Python attributes of PowerBackend).
#[derive(Clone, Debug)]
pub struct BackendState {
    pub ready: bool,
    pub profiles_ready: bool,
    pub power_ready: bool,
    pub sleep_ready: bool,
    pub busy: bool,
    pub error: String,
    pub profiles: Vec<String>,
    pub active_profile: String,
    pub performance_degraded: String,
    pub has_battery: bool,
    pub on_battery: bool,
    pub percentage: Option<f64>,
    /// Highest UPower WarningLevel of any system battery (1 none … 5 action).
    pub warning_level: i64,
    pub batteries: Vec<Value>,
    pub power_source_known: bool,
    pub capabilities: Capabilities,
    pub charge_limit: Value,
}

impl Default for BackendState {
    fn default() -> Self {
        Self { ready: false, profiles_ready: false, power_ready: false, sleep_ready: false, busy: false, error: String::new(),
               profiles: Vec::new(), active_profile: String::new(), performance_degraded: String::new(), has_battery: false,
               on_battery: false, percentage: None, warning_level: 0, batteries: Vec::new(), power_source_known: false,
               capabilities: unknown_capabilities(), charge_limit: charge_limits::initial_state() }
    }
}

impl BackendState {
    pub fn capability(&self, action: &str) -> Option<&str> {
        self.capabilities.get(action).map(String::as_str)
    }
}

pub type Guard = Rc<dyn Fn() -> bool>;

/// What the application, idle controller, alerts and UI use.
pub trait PowerSystem {
    fn state(&self) -> Ref<'_, BackendState>;
    fn start(&self);
    fn close(&self);
    fn set_profile(&self, profile: &str) -> bool;
    /// Check immediately before a display/lock action. Unknown means blocked.
    fn idle_inhibited(&self, callback: Box<dyn FnOnce(bool)>);
    fn sleep(&self, action: &str, interactive: bool, guard: Option<Guard>) -> bool;
    fn explicit_power(&self, action: &str, callback: Option<Box<dyn FnOnce(bool)>>, interactive: bool) -> bool;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Full,
    /// Display-only battery consumers (Python `batteries_only=True`).
    BatteriesOnly,
    /// Rust-only: the Waybar/Waydock status stream needs nothing but profiles.
    ProfilesOnly,
}

pub type Connector = Box<dyn Fn(Box<dyn FnOnce(Result<Rc<dyn Bus>, BusError>)>)>;
pub type Inspector = Box<dyn Fn(&Map<String, Value>) -> (Value, Records)>;

pub struct Options {
    pub mode: Mode,
    pub connector: Connector,
    pub scheduler: Rc<dyn Scheduler>,
    pub inspect: Inspector,
    pub session_id: Box<dyn Fn() -> String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Full,
            connector: Box::new(|done| suite_core::bus::connect(gio::BusType::System, None, done)),
            scheduler: Rc::new(GlibScheduler::default()),
            inspect: Box::new(|devices| charge_limits::inspect_limits(devices, std::path::Path::new(charge_limits::SUPPLIES),
                                                                     &charge_limits::helper_files())),
            session_id: Box::new(|| std::env::var("XDG_SESSION_ID").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "auto".into())),
        }
    }
}

const PROFILES: usize = 0;
const POWER: usize = 1;
const SLEEP: usize = 2;

pub struct Inner {
    mode: Mode,
    state: RefCell<BackendState>,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    connector: Connector,
    scheduler: Rc<dyn Scheduler>,
    inspect: Inspector,
    session_id: Box<dyn Fn() -> String>,
    bus: RefCell<Option<Rc<dyn Bus>>>,
    subscriptions: RefCell<Vec<u32>>,
    sources: RefCell<HashMap<&'static str, u64>>,
    closed: Cell<bool>,
    started: Cell<bool>,
    connecting: Cell<bool>,
    generation: Cell<u64>,
    reads: [Cell<u64>; 3],
    errors: RefCell<Vec<(String, String)>>,
    profile_endpoint: Cell<Option<usize>>,
    operation: Cell<u64>,
    power_devices: RefCell<Map<String, Value>>,
    upower_on_battery: RefCell<Option<Value>>,
    power_epoch: Cell<u64>,
    battery_action: RefCell<Option<String>>,
    charge_limit_action: Cell<bool>,
    charge_limit_error: RefCell<String>,
    charge_records: RefCell<Records>,
}

#[derive(Clone)]
pub struct PowerBackend(pub Rc<Inner>);

type Done = Box<dyn FnOnce(&Rc<Inner>, Result<Value, BusError>)>;

impl PowerBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static, options: Options) -> Self {
        let inner = Rc::new(Inner {
            mode: options.mode, state: RefCell::new(BackendState::default()), on_changed: Box::new(on_changed),
            on_error: Box::new(on_error), connector: options.connector, scheduler: options.scheduler,
            inspect: options.inspect, session_id: options.session_id, bus: RefCell::new(None),
            subscriptions: RefCell::new(Vec::new()), sources: RefCell::new(HashMap::new()), closed: Cell::new(false),
            started: Cell::new(false), connecting: Cell::new(false), generation: Cell::new(0),
            reads: [Cell::new(0), Cell::new(0), Cell::new(0)], errors: RefCell::new(Vec::new()),
            profile_endpoint: Cell::new(None), operation: Cell::new(0), power_devices: RefCell::new(Map::new()),
            upower_on_battery: RefCell::new(None), power_epoch: Cell::new(0), battery_action: RefCell::new(None),
            charge_limit_action: Cell::new(false), charge_limit_error: RefCell::new(String::new()),
            charge_records: RefCell::new(Records::new()),
        });
        let (limit, records) = (inner.inspect)(&Map::new());
        inner.state.borrow_mut().charge_limit = limit;
        inner.charge_records.replace(records);
        Self(inner)
    }

    /// Install an already connected bus (tests: Python sets `_connection` directly).
    pub fn attach(&self, bus: Rc<dyn Bus>) {
        self.0.bus.replace(Some(bus));
        self.0.state.borrow_mut().ready = true;
    }

    pub fn refresh(&self) { self.0.refresh() }
    pub fn refresh_profiles(&self) { self.0.refresh_profiles() }
    pub fn refresh_power(&self) { self.0.refresh_power() }
    pub fn refresh_sleep(&self) { self.0.refresh_sleep() }
    pub fn subscribe(&self) { self.0.subscribe() }
    pub fn has_source(&self, key: &str) -> bool { self.0.sources.borrow().contains_key(key) }
    pub fn profile_endpoint(&self) -> Option<Endpoint> { self.0.profile_endpoint.get().map(|i| PPD_ENDPOINTS[i]) }
    pub fn power_devices(&self) -> Map<String, Value> { self.0.power_devices.borrow().clone() }
    pub fn set_power_device(&self, path: &str, properties: Value) { self.0.power_devices.borrow_mut().insert(path.into(), properties); }
    pub fn publish_batteries(&self) { self.0.publish_batteries() }
    pub fn state_mut(&self) -> std::cell::RefMut<'_, BackendState> { self.0.state.borrow_mut() }

    /// Signal entry points (also used by tests, like the Python `_power_signal` calls).
    pub fn owner_changed(&self, name: &str) { self.0.owner_changed(name) }
    pub fn power_signal(&self, signal: &Signal) { self.0.power_signal(signal) }
    pub fn bus_closed(&self) { self.0.bus_closed() }

    pub fn set_charge_thresholds(&self, path: &Value, enabled: &Value) -> bool { self.0.set_charge_thresholds(path, enabled) }
    pub fn set_charge_limit(&self, maximum: &Value) -> bool { self.0.set_charge_limit(maximum) }

    /// `PowerBackend(on_changed, on_error, batteries_only=True)` on the system bus:
    /// UPower only (battery-manager and its indicator stream).
    pub fn batteries_only(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) -> Self {
        Self::new(on_changed, on_error, Options { mode: Mode::BatteriesOnly, ..Options::default() })
    }

    pub fn ready(&self) -> bool { self.0.state.borrow().ready }
    /// Highest UPower WarningLevel of any system battery (1 none … 5 action).
    pub fn warning_level(&self) -> i64 { self.0.state.borrow().warning_level }
}

/// What the battery UI, indicator and `--check` read (Python attribute access
/// on `PowerBackend`); implemented by [`PowerBackend`] and UI test fakes.
pub trait BatteryModel {
    fn power_ready(&self) -> bool;
    fn power_source_known(&self) -> bool;
    fn on_battery(&self) -> bool;
    fn has_battery(&self) -> bool;
    fn percentage(&self) -> Option<f64>;
    fn busy(&self) -> bool;
    fn error(&self) -> String;
    fn batteries(&self) -> Vec<Value>;
    fn charge_limit(&self) -> Value;
    fn set_charge_thresholds(&self, path: &str, enabled: bool) -> bool;
    fn set_charge_limit(&self, maximum: &Value) -> bool;
}

impl BatteryModel for PowerBackend {
    fn power_ready(&self) -> bool { self.0.state.borrow().power_ready }
    fn power_source_known(&self) -> bool { self.0.state.borrow().power_source_known }
    fn on_battery(&self) -> bool { self.0.state.borrow().on_battery }
    fn has_battery(&self) -> bool { self.0.state.borrow().has_battery }
    fn percentage(&self) -> Option<f64> { self.0.state.borrow().percentage }
    fn busy(&self) -> bool { self.0.state.borrow().busy }
    fn error(&self) -> String { self.0.state.borrow().error.clone() }
    fn batteries(&self) -> Vec<Value> { self.0.state.borrow().batteries.clone() }
    fn charge_limit(&self) -> Value { self.0.state.borrow().charge_limit.clone() }
    fn set_charge_thresholds(&self, path: &str, enabled: bool) -> bool {
        self.0.set_charge_thresholds(&Value::from(path), &Value::Bool(enabled))
    }
    fn set_charge_limit(&self, maximum: &Value) -> bool { self.0.set_charge_limit(maximum) }
}

impl PowerSystem for PowerBackend {
    fn state(&self) -> Ref<'_, BackendState> { self.0.state.borrow() }
    fn start(&self) {
        if self.0.closed.get() || self.0.started.get() {
            return;
        }
        self.0.started.set(true);
        self.0.connect();
    }
    fn close(&self) { self.0.close() }
    fn set_profile(&self, profile: &str) -> bool { self.0.set_profile(profile) }
    fn idle_inhibited(&self, callback: Box<dyn FnOnce(bool)>) { self.0.idle_inhibited(callback) }
    fn sleep(&self, action: &str, interactive: bool, guard: Option<Guard>) -> bool { self.0.sleep(action, interactive, guard) }
    fn explicit_power(&self, action: &str, callback: Option<Box<dyn FnOnce(bool)>>, interactive: bool) -> bool {
        self.0.explicit_power(action, callback, interactive)
    }
}

fn error_text(error: &BusError) -> String {
    if let Some(name) = &error.name {
        if ["NotAuthorized", "AccessDenied", "InteractiveAuthorizationRequired", "Cancelled", "Canceled"]
            .iter().any(|part| name.contains(part)) {
            return "Authorization was canceled or denied.".into();
        }
    }
    error.message.clone()
}

impl Inner {
    fn notify(&self) {
        {
            let errors = self.errors.borrow();
            let mut state = self.state.borrow_mut();
            state.error = errors.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>().join("\n");
            let busy = state.busy;
            state.charge_limit["busy"] = busy.into();
        }
        if !self.closed.get() {
            (self.on_changed)();
        }
    }

    fn set_error(&self, section: &str, message: &str) {
        {
            let mut errors = self.errors.borrow_mut();
            let position = errors.iter().position(|(s, _)| s == section);
            match (message.is_empty(), position) {
                (false, Some(i)) => errors[i].1 = message.into(),
                (false, None) => errors.push((section.into(), message.into())),
                (true, Some(i)) => { errors.remove(i); }
                (true, None) => {}
            }
        }
        self.notify();
    }

    fn pop_error(&self, section: &str) {
        self.errors.borrow_mut().retain(|(s, _)| s != section);
    }

    fn fail(&self, message: &str) -> bool {
        self.set_error("action", message);
        if !self.closed.get() {
            (self.on_error)(message);
        }
        false
    }

    fn later(self: &Rc<Self>, key: &'static str, milliseconds: u32, callback: fn(&Rc<Inner>)) {
        if self.closed.get() || self.sources.borrow().contains_key(key) {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = self.scheduler.timeout(milliseconds, Box::new(move || {
            let Some(this) = weak.upgrade() else { return };
            this.sources.borrow_mut().remove(key);
            if !this.closed.get() {
                callback(&this);
            }
        }));
        self.sources.borrow_mut().insert(key, id);
    }

    fn connect(self: &Rc<Self>) {
        if self.closed.get() || self.connecting.get() || self.bus.borrow().is_some() {
            return;
        }
        self.connecting.set(true);
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        (self.connector)(Box::new(move |result| {
            let Some(this) = weak.upgrade() else { return };
            if this.closed.get() || generation != this.generation.get() {
                return;
            }
            this.connecting.set(false);
            match result {
                Err(error) => {
                    this.set_error("bus", &format!("System power services are unavailable: {}", error_text(&error)));
                    this.later("connect", 5000, |this| this.connect());
                }
                Ok(bus) => {
                    let weak = Rc::downgrade(&this);
                    bus.on_closed(Some(Box::new(move || {
                        if let Some(this) = weak.upgrade() {
                            // Deferred: never re-enter the connection from its own signal.
                            let weak = Rc::downgrade(&this);
                            glib::idle_add_local_once(move || if let Some(this) = weak.upgrade() { this.bus_closed() });
                        }
                    })));
                    this.bus.replace(Some(bus));
                    this.state.borrow_mut().ready = true;
                    this.pop_error("bus");
                    this.subscribe();
                    this.refresh();
                }
            }
        }));
    }

    fn subscribe(self: &Rc<Self>) {
        let Some(bus) = self.bus.borrow().clone() else { return };
        let handler = |weak: Weak<Inner>, f: fn(&Rc<Inner>, &Signal)| -> suite_core::bus::SignalHandler {
            Rc::new(move |signal: &Signal| if let Some(this) = weak.upgrade() { f(&this, signal) })
        };
        let mut ids = Vec::new();
        let endpoints: Vec<Endpoint> = match self.mode {
            Mode::Full => vec![PPD_ENDPOINTS[0], PPD_ENDPOINTS[1], UPOWER, LOGIN],
            Mode::BatteriesOnly => vec![UPOWER],
            Mode::ProfilesOnly => PPD_ENDPOINTS.to_vec(),
        };
        for endpoint in endpoints {
            ids.push(bus.subscribe(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"), Some("NameOwnerChanged"),
                Some("/org/freedesktop/DBus"), Some(endpoint.0), handler(Rc::downgrade(self), |this, signal| {
                    if let Some(name) = signal.params.try_child_value(0).and_then(|v| v.get::<String>()) {
                        this.owner_changed(&name);
                    }
                })));
        }
        if self.mode != Mode::BatteriesOnly {
            for (name, path, interface) in PPD_ENDPOINTS {
                ids.push(bus.subscribe(Some(name), Some(PROPERTIES), Some("PropertiesChanged"), Some(path), Some(interface),
                    handler(Rc::downgrade(self), |this, _| this.later("profiles", 100, |this| this.refresh_profiles()))));
            }
        }
        if self.mode != Mode::ProfilesOnly {
            ids.push(bus.subscribe(Some(UPOWER.0), Some(PROPERTIES), Some("PropertiesChanged"), None, None,
                                   handler(Rc::downgrade(self), |this, signal| this.power_signal(signal))));
            ids.push(bus.subscribe(Some(UPOWER.0), Some(UPOWER.2), None, Some(UPOWER.1), None,
                                   handler(Rc::downgrade(self), |this, signal| this.power_signal(signal))));
        }
        if self.mode == Mode::Full {
            ids.push(bus.subscribe(Some(LOGIN.0), Some(LOGIN.2), Some("PrepareForSleep"), Some(LOGIN.1), None,
                handler(Rc::downgrade(self), |this, signal| {
                    if signal.params.try_child_value(0).and_then(|v| v.get::<bool>()) == Some(false) {
                        this.refresh();
                    }
                })));
        }
        self.subscriptions.borrow_mut().extend(ids);
    }

    fn detach(&self) {
        if let Some(bus) = self.bus.take() {
            for id in self.subscriptions.borrow_mut().drain(..) {
                bus.unsubscribe(id);
            }
            bus.on_closed(None);
        }
        self.subscriptions.borrow_mut().clear();
    }

    fn reset_power_state(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.ready = false;
            state.profiles_ready = false;
            state.power_ready = false;
            state.sleep_ready = false;
            state.power_source_known = false;
            state.busy = false;
        }
        self.battery_action.replace(None);
        self.power_devices.borrow_mut().clear();
        self.charge_limit_action.set(false);
    }

    fn bus_closed(self: &Rc<Self>) {
        self.generation.set(self.generation.get() + 1);
        if let Some(bus) = self.bus.borrow().clone() {
            bus.cancel_reads();
        }
        self.detach();
        self.reset_power_state();
        self.state.borrow_mut().capabilities = unknown_capabilities();
        self.operation.set(self.operation.get() + 1);
        self.power_epoch.set(self.power_epoch.get() + 1);
        self.publish_batteries();
        self.set_error("bus", "System power services are disconnected.");
        self.later("connect", 2000, |this| this.connect());
    }

    fn close(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        self.generation.set(self.generation.get() + 1);
        if let Some(bus) = self.bus.borrow().clone() {
            bus.cancel_reads();
        }
        for (_, id) in self.sources.borrow_mut().drain() {
            self.scheduler.cancel(id);
        }
        self.detach();
        self.reset_power_state();
        self.publish_batteries();
    }

    fn publish_batteries(&self) {
        let devices = self.power_devices.borrow().clone();
        let busy_path = self.battery_action.borrow().clone();
        let mut batteries = device_batteries(&devices, busy_path.as_deref());
        let (mut limit, records) = (self.inspect)(&devices);
        let busy = self.state.borrow().busy;
        limit["busy"] = busy.into();
        limit["error"] = self.charge_limit_error.borrow().clone().into();
        let action = self.charge_limit_action.get();
        for device in batteries.iter_mut() {
            let path = device["path"].as_str().unwrap_or("").to_owned();
            let record = records.get(&path);
            device["charge_limit_adjustable"] = record.is_some().into();
            if let Some(record) = record {
                device["charge_start_threshold"] = record.start.map(Value::from).unwrap_or(Value::Null);
                device["charge_end_threshold"] = record.stop.into();
                device["charge_threshold_enabled"] = (record.stop < 100).into();
                let busy = device["busy"].as_bool().unwrap_or(false) || action;
                device["busy"] = busy.into();
            }
        }
        self.charge_records.replace(records);
        let mut state = self.state.borrow_mut();
        state.batteries = batteries;
        state.charge_limit = limit;
    }

    fn request(self: &Rc<Self>, endpoint: (&str, &str, &str), method: &str, params: Option<Variant>, interface: Option<&str>,
               interactive: bool, mutation: bool, done: Done) {
        let bus = self.bus.borrow().clone();
        let Some(bus) = bus.filter(|_| !self.closed.get()) else {
            done(self, Err(BusError::local("The system bus is unavailable.")));
            return;
        };
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        let mut call = Call::new(endpoint.0, endpoint.1, interface.unwrap_or(endpoint.2), method, params);
        call.interactive = interactive;
        call.mutation = mutation;
        bus.call(call, Box::new(move |reply| {
            let Some(this) = weak.upgrade() else { return };
            if !this.closed.get() && generation == this.generation.get() {
                done(&this, reply.map(|v| first(&v)));
            }
        }));
    }

    fn properties(self: &Rc<Self>, endpoint: (&str, &str, &str), done: Done) {
        let params = (endpoint.2,).to_variant();
        self.request(endpoint, "GetAll", Some(params), Some(PROPERTIES), false, false, done);
    }

    fn owner_changed(self: &Rc<Self>, name: &str) {
        if PPD_ENDPOINTS.iter().any(|e| e.0 == name) {
            self.bump(PROFILES);
            self.state.borrow_mut().profiles_ready = false;
            self.profile_endpoint.set(None);
            self.later("profiles", 100, |this| this.refresh_profiles());
        } else if name == UPOWER.0 {
            self.bump(POWER);
            {
                let mut state = self.state.borrow_mut();
                state.power_ready = false;
                state.power_source_known = false;
            }
            self.power_epoch.set(self.power_epoch.get() + 1);
            self.power_devices.borrow_mut().clear();
            if self.battery_action.borrow().is_some() || self.charge_limit_action.get() {
                self.operation.set(self.operation.get() + 1);
                self.battery_action.replace(None);
                self.charge_limit_action.set(false);
                self.state.borrow_mut().busy = false;
            }
            self.publish_batteries();
            self.later("power", 100, |this| this.refresh_power());
        } else if name == LOGIN.0 {
            self.bump(SLEEP);
            {
                let mut state = self.state.borrow_mut();
                state.sleep_ready = false;
                state.capabilities = unknown_capabilities();
            }
            self.later("sleep", 100, |this| this.refresh_sleep());
        }
        self.notify();
    }

    fn bump(&self, read: usize) -> u64 {
        let value = self.reads[read].get() + 1;
        self.reads[read].set(value);
        value
    }

    fn power_signal(self: &Rc<Self>, signal: &Signal) {
        // Charge updates can arrive every few seconds. Marking those updates as
        // an unknown source would continuously reset the desktop's idle timers.
        let mut uncertain = false;
        if signal.interface == PROPERTIES && signal.member == "PropertiesChanged" {
            let params = values::to_value(&signal.params);
            let changed_interface = params[0].as_str().unwrap_or("");
            let changed = params[1].as_object().cloned().unwrap_or_default();
            let invalidated: Vec<String> = params[2].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if signal.path == UPOWER.1 && changed_interface == UPOWER.2 {
                uncertain = invalidated.iter().any(|k| k == "OnBattery")
                    || changed.get("OnBattery").is_some_and(|v| Some(v) != self.upower_on_battery.borrow().as_ref());
            } else if changed_interface == UPOWER_DEVICE {
                let previous = self.power_devices.borrow().get(&signal.path).cloned().unwrap_or(json!({}));
                let mut updated = previous.clone();
                for (k, v) in &changed {
                    updated[k.as_str()] = v.clone();
                }
                let was_supply = is_system_battery(&previous);
                let is_supply = is_system_battery(&updated);
                uncertain = was_supply != is_supply;
                if (was_supply || is_supply) && invalidated.iter().any(|k| ["Type", "PowerSupply", "IsPresent", "State"].contains(&k.as_str())) {
                    uncertain = true;
                }
                if int(updated.get("Type")) == Some(3) && is_supply && changed.contains_key("State") {
                    let discharging = |d: &Value| matches!(int(d.get("State")), Some(2 | 3));
                    uncertain |= discharging(&previous) != discharging(&updated);
                }
            }
        } else if signal.interface == UPOWER.2 && signal.member == "DeviceRemoved" {
            let removed_path = signal.params.try_child_value(0).and_then(|v| v.str().map(String::from)).unwrap_or_default();
            let removed = self.power_devices.borrow_mut().shift_remove(&removed_path).unwrap_or(json!({}));
            uncertain = is_system_battery(&removed);
            if uncertain {
                self.state.borrow_mut().power_source_known = false;
            }
            if self.battery_action.borrow().as_deref() == Some(removed_path.as_str()) {
                self.finish_action(self.operation.get(), Some("That battery is no longer available."));
                self.operation.set(self.operation.get() + 1);
            }
            self.publish_batteries();
            self.notify();
        }
        // A refresh discovers added supplies without interrupting idle timers.
        self.bump(POWER);
        if uncertain {
            self.state.borrow_mut().power_source_known = false;
            self.notify();
        }
        self.later("power", 100, |this| this.refresh_power());
    }

    fn refresh(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        if self.bus.borrow().is_none() {
            self.connect();
            return;
        }
        if self.mode != Mode::BatteriesOnly {
            self.refresh_profiles();
        }
        if self.mode != Mode::ProfilesOnly {
            self.refresh_power();
        }
        if self.mode == Mode::Full {
            self.refresh_sleep();
        }
        // A fallback only: UPower/logind signals deliver changes immediately.
        let delay = if self.mode == Mode::BatteriesOnly { 120000 } else { 30000 };
        self.later("refresh", delay, |this| this.refresh());
    }

    fn refresh_profiles(self: &Rc<Self>) {
        let generation = self.bump(PROFILES);
        self.attempt_profiles(generation, 0);
    }

    fn attempt_profiles(self: &Rc<Self>, generation: u64, index: usize) {
        let endpoint = PPD_ENDPOINTS[index];
        self.properties(endpoint, Box::new(move |this, reply| {
            if generation != this.reads[PROFILES].get() {
                return;
            }
            let props = match reply {
                Err(_) if index + 1 < PPD_ENDPOINTS.len() => return this.attempt_profiles(generation, index + 1),
                Err(_) => {
                    this.state.borrow_mut().profiles_ready = false;
                    this.profile_endpoint.set(None);
                    this.set_error("profiles", "Power profiles are unavailable. Check power-profiles-daemon.");
                    return;
                }
                Ok(props) => props,
            };
            let names: Vec<Option<String>> = props.get("Profiles").and_then(Value::as_array).map(|rows| rows.iter()
                .map(|row| row.get("Profile").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)).collect())
                .unwrap_or_default();
            let active = props.get("ActiveProfile").and_then(Value::as_str).map(String::from);
            let valid = !names.is_empty() && names.iter().all(Option::is_some)
                && active.as_ref().is_some_and(|a| names.iter().any(|n| n.as_deref() == Some(a)));
            if !valid {
                this.state.borrow_mut().profiles_ready = false;
                this.set_error("profiles", "The power profile service returned incomplete settings.");
                return;
            }
            let mut unique: Vec<String> = Vec::new();
            for name in names.into_iter().flatten() {
                if !unique.contains(&name) {
                    unique.push(name);
                }
            }
            let degraded = [props.get("PerformanceDegraded"), props.get("PerformanceInhibited")].into_iter()
                .map(|v| text(v)).find(|s| !s.is_empty()).unwrap_or_default();
            {
                let mut state = this.state.borrow_mut();
                state.profiles = unique;
                state.active_profile = active.unwrap();
                state.performance_degraded = degraded;
                state.profiles_ready = true;
            }
            this.profile_endpoint.set(Some(index));
            this.set_error("profiles", "");
        }));
    }

    fn refresh_power(self: &Rc<Self>) {
        let generation = self.bump(POWER);
        #[derive(Default)]
        struct Read {
            manager: Option<Value>,
            devices: Option<Map<String, Value>>,
            errors: usize,
        }
        let read = Rc::new(RefCell::new(Read::default()));
        let finish = move |this: &Rc<Inner>, read: &Rc<RefCell<Read>>| {
            if generation != this.reads[POWER].get() {
                return;
            }
            let (manager, devices, errors) = {
                let r = read.borrow();
                let (Some(manager), Some(devices)) = (r.manager.clone(), r.devices.clone()) else { return };
                (manager, devices, r.errors)
            };
            let on_battery = manager.get("OnBattery").and_then(Value::as_bool);
            let Some(manager_on_battery) = on_battery.filter(|_| errors == 0) else {
                {
                    let mut state = this.state.borrow_mut();
                    state.power_ready = false;
                    state.power_source_known = false;
                }
                this.set_error("power", "The current power source is unavailable. Check UPower.");
                return;
            };
            this.power_devices.replace(devices.clone());
            this.upower_on_battery.replace(Some(Value::Bool(manager_on_battery)));
            let batteries = system_batteries(devices.values());
            let has_battery = !batteries.is_empty();
            // UPower's root OnBattery covers laptops. Some UPS drivers only
            // expose discharging on the UPS device itself.
            let on_battery = manager_on_battery || batteries.iter()
                .any(|d| int(d.get("Type")) == Some(3) && matches!(int(d.get("State")), Some(2 | 3)));
            let percentage = battery_percentage(&batteries);
            let warning = batteries.iter().filter_map(|d| int(d.get("WarningLevel"))).filter(|l| (0..=5).contains(l)).max().unwrap_or(0);
            {
                let mut state = this.state.borrow_mut();
                state.has_battery = has_battery;
                state.on_battery = on_battery;
                state.percentage = percentage;
                state.warning_level = warning;
            }
            this.publish_batteries();
            {
                let mut state = this.state.borrow_mut();
                state.power_ready = true;
                state.power_source_known = true;
            }
            this.set_error("power", "");
        };
        let finish = Rc::new(finish);
        let (r, f) = (read.clone(), finish.clone());
        self.properties(UPOWER, Box::new(move |this, reply| {
            match reply {
                Ok(value) => r.borrow_mut().manager = Some(value),
                Err(_) => {
                    let mut r = r.borrow_mut();
                    r.manager = Some(json!({}));
                    r.errors += 1;
                }
            }
            f(this, &r);
        }));
        let (r, f) = (read, finish);
        self.request(UPOWER, "EnumerateDevices", None, None, false, false, Box::new(move |this, reply| {
            if generation != this.reads[POWER].get() {
                return;
            }
            let paths = match reply {
                Err(_) => {
                    {
                        let mut read = r.borrow_mut();
                        read.errors += 1;
                        read.devices = Some(Map::new());
                    }
                    return f(this, &r);
                }
                Ok(value) => {
                    let mut unique: Vec<String> = Vec::new();
                    for path in value.as_array().into_iter().flatten().filter_map(Value::as_str) {
                        if !unique.iter().any(|p| p == path) {
                            unique.push(path.to_owned());
                        }
                    }
                    unique
                }
            };
            if paths.is_empty() {
                r.borrow_mut().devices = Some(Map::new());
                return f(this, &r);
            }
            let pending = Rc::new(RefCell::new(paths.clone()));
            let collected = Rc::new(RefCell::new(Map::new()));
            for path in paths {
                let (r, f, pending, collected) = (r.clone(), f.clone(), pending.clone(), collected.clone());
                let device_path = path.clone();
                this.properties((UPOWER.0, &path, UPOWER_DEVICE), Box::new(move |this, reply| {
                    pending.borrow_mut().retain(|p| *p != device_path);
                    match reply {
                        Ok(props) => { collected.borrow_mut().insert(device_path.clone(), props); }
                        Err(_) => r.borrow_mut().errors += 1,
                    }
                    if pending.borrow().is_empty() {
                        r.borrow_mut().devices = Some(collected.borrow().clone());
                        f(this, &r);
                    }
                }));
            }
        }));
    }

    fn refresh_sleep(self: &Rc<Self>) {
        let generation = self.bump(SLEEP);
        let pending = Rc::new(RefCell::new(SLEEP_METHODS.iter().map(|(a, _)| a.to_string()).collect::<Vec<_>>()));
        let capabilities = Rc::new(RefCell::new(Capabilities::new()));
        let failures = Rc::new(Cell::new(0usize));
        for (action, method) in SLEEP_METHODS {
            let (pending, capabilities, failures) = (pending.clone(), capabilities.clone(), failures.clone());
            self.request(LOGIN, &format!("Can{method}"), None, None, false, false, Box::new(move |this, reply| {
                if generation != this.reads[SLEEP].get() {
                    return;
                }
                pending.borrow_mut().retain(|a| a != action);
                let mut answer = match &reply { Ok(v) => v.as_str().unwrap_or("na").to_owned(), Err(_) => "na".into() };
                if !CAPABILITIES.contains(&answer.as_str()) {
                    answer = "na".into();
                }
                capabilities.borrow_mut().insert(action.into(), answer);
                if reply.is_err() {
                    failures.set(failures.get() + 1);
                }
                if !pending.borrow().is_empty() {
                    return;
                }
                // Older logind versions may lack one method, while suspend works.
                let ready = failures.get() < SLEEP_METHODS.len();
                {
                    let mut state = this.state.borrow_mut();
                    state.capabilities = capabilities.borrow().clone();
                    state.sleep_ready = ready;
                }
                this.set_error("sleep", if ready { "" } else { "Sleep controls are unavailable. Check systemd-logind." });
            }));
        }
    }

    fn begin_action(&self) -> Option<u64> {
        if self.closed.get() || self.state.borrow().busy {
            return None;
        }
        let operation = self.operation.get() + 1;
        self.operation.set(operation);
        self.state.borrow_mut().busy = true;
        self.pop_error("action");
        self.notify();
        Some(operation)
    }

    fn finish_action(&self, operation: u64, error: Option<&str>) {
        if self.closed.get() || operation != self.operation.get() {
            return;
        }
        self.state.borrow_mut().busy = false;
        self.battery_action.replace(None);
        self.charge_limit_action.set(false);
        self.publish_batteries();
        match error {
            Some(error) => { self.fail(error); }
            None => self.set_error("action", ""),
        }
    }

    fn set_profile(self: &Rc<Self>, profile: &str) -> bool {
        let (ready, known) = {
            let state = self.state.borrow();
            (state.profiles_ready, state.profiles.iter().any(|p| p == profile))
        };
        let Some(index) = self.profile_endpoint.get().filter(|_| ready) else {
            return self.fail("Power profiles are unavailable.");
        };
        if !known {
            return self.fail("That power profile is not available on this device.");
        }
        let Some(operation) = self.begin_action() else { return false };
        let endpoint = PPD_ENDPOINTS[index];
        let params = (endpoint.2, "ActiveProfile", profile.to_variant()).to_variant();
        self.request(endpoint, "Set", Some(params), Some(PROPERTIES), true, true, Box::new(move |this, reply| {
            this.finish_action(operation, reply.err().map(|e| error_text(&e)).as_deref());
            this.refresh_profiles();
        }));
        true
    }

    fn set_charge_thresholds(self: &Rc<Self>, path: &Value, enabled: &Value) -> bool {
        let (Value::String(path), Value::Bool(enabled)) = (path, enabled) else {
            return self.fail("Choose a battery and a valid charge limit setting.");
        };
        let (path, enabled) = (path.clone(), *enabled);
        if !self.state.borrow().power_ready || !self.power_devices.borrow().contains_key(&path) {
            return self.fail("That battery is no longer available.");
        }
        self.publish_batteries();
        if self.charge_records.borrow().contains_key(&path) {
            return self.fail("Use Maximum charge to adjust the laptop battery limits.");
        }
        let original = self.power_devices.borrow()[&path].clone();
        let single = |props: &Value| {
            let mut map = Map::new();
            map.insert(path.clone(), props.clone());
            device_batteries(&map, None)
        };
        let battery = single(&original);
        if battery.first().is_none_or(|b| b["charge_threshold_supported"] != true) {
            return self.fail("Charge limits are not supported by this battery.");
        }
        let Some(operation) = self.begin_action() else { return false };
        self.battery_action.replace(Some(path.clone()));
        self.publish_batteries();
        self.notify();
        let identity = device_identity(&original);
        let epoch = self.power_epoch.get();
        let endpoint_path = path.clone();
        let current = {
            let (path, identity) = (path.clone(), identity.clone());
            Rc::new(move |this: &Rc<Inner>| {
                !this.closed.get() && operation == this.operation.get() && epoch == this.power_epoch.get()
                    && this.state.borrow().power_ready
                    && this.power_devices.borrow().get(&path).is_some_and(|d| identity == device_identity(d))
            })
        };
        self.properties((UPOWER.0, &endpoint_path, UPOWER_DEVICE), Box::new(move |this, reply| {
            if operation != this.operation.get() || this.closed.get() {
                return;
            }
            if !current(this) {
                return this.finish_action(operation, Some("That battery changed or is no longer available."));
            }
            let properties = match reply {
                Err(error) => return this.finish_action(operation, Some(&error_text(&error))),
                Ok(properties) => properties,
            };
            let mut map = Map::new();
            map.insert(path.clone(), properties.clone());
            let battery = device_batteries(&map, None);
            if identity != device_identity(&properties) || battery.first().is_none_or(|b| b["charge_threshold_supported"] != true) {
                this.finish_action(operation, Some("Charge limits are no longer supported by this battery."));
                this.refresh_power();
                return;
            }
            this.power_devices.borrow_mut().insert(path.clone(), properties.clone());
            if properties.get("ChargeThresholdEnabled") == Some(&Value::Bool(enabled)) {
                return this.finish_action(operation, None);
            }
            let (path2, identity2, current2) = (path.clone(), identity.clone(), current.clone());
            this.request((UPOWER.0, &path, UPOWER_DEVICE), "EnableChargeThreshold", Some((enabled,).to_variant()), None, true, true,
                Box::new(move |this, reply| {
                    if operation != this.operation.get() || this.closed.get() {
                        return;
                    }
                    if let Err(error) = reply {
                        this.finish_action(operation, Some(&error_text(&error)));
                        this.refresh_power();
                        return;
                    }
                    let path3 = path2.clone();
                    this.properties((UPOWER.0, &path2, UPOWER_DEVICE), Box::new(move |this, reply| {
                        if operation != this.operation.get() || this.closed.get() {
                            return;
                        }
                        if !current2(this) {
                            return this.finish_action(operation, Some("That battery changed or is no longer available."));
                        }
                        match reply {
                            Err(_) => this.finish_action(operation, Some("The charge setting changed, but its current status could not be read.")),
                            Ok(result) if device_identity(&result) != identity2 => {
                                this.finish_action(operation, Some("That battery changed or is no longer available."))
                            }
                            Ok(result) => {
                                let applied = result.get("ChargeThresholdEnabled") == Some(&Value::Bool(enabled));
                                this.power_devices.borrow_mut().insert(path3.clone(), result);
                                this.finish_action(operation, if applied { None } else { Some("The battery did not apply the requested charge limit.") });
                            }
                        }
                        this.refresh_power();
                    }));
                }));
        }));
        true
    }

    fn set_charge_limit(self: &Rc<Self>, maximum_value: &Value) -> bool {
        if !self.state.borrow().power_ready {
            return self.fail("The laptop battery information is not available.");
        }
        self.publish_batteries();
        let (supported, reason) = {
            let state = self.state.borrow();
            (state.charge_limit["supported"] == true, text(state.charge_limit.get("reason")))
        };
        if !supported {
            return self.fail(&reason);
        }
        if let Err(error) = charge_limits::arguments_for(maximum_value, &self.charge_records.borrow()) {
            return self.fail(&error);
        }
        if self.state.borrow().busy || self.closed.get() {
            return false;
        }
        let maximum = int(Some(maximum_value)).unwrap();
        if self.charge_records.borrow().values().all(|r| r.stop == maximum) {
            return true;
        }
        let expected = charge_limits::identity(&self.charge_records.borrow());
        let epoch = self.power_epoch.get();
        let Some(operation) = self.begin_action() else { return false };
        self.charge_limit_action.set(true);
        self.charge_limit_error.replace(String::new());
        self.publish_batteries();
        self.notify();
        let current = move |this: &Rc<Inner>| {
            !this.closed.get() && operation == this.operation.get() && epoch == this.power_epoch.get() && this.state.borrow().power_ready
        };
        let finish = move |this: &Rc<Inner>, error: Option<String>| {
            this.charge_limit_error.replace(error.clone().unwrap_or_default());
            this.finish_action(operation, error.as_deref());
        };
        fn bytes(value: &Value) -> Vec<u8> {
            value.as_array().map(|a| a.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect()).unwrap_or_default()
        }
        let maximum_copy = maximum_value.clone();
        let get = charge_limits::parameters("getthreshold", None).expect("valid action");
        self.request(charge_limits::ENDPOINT, "performAction", Some(get), None, false, false, Box::new(move |this, reply| {
            if !current(this) {
                return;
            }
            let reply = match reply {
                Err(error) => return finish(this, Some(error_text(&error))),
                Ok(reply) => reply,
            };
            let arguments = (|| {
                charge_limits::decode_reply(&bytes(&reply))?;
                this.publish_batteries();
                if charge_limits::identity(&this.charge_records.borrow()) != expected {
                    return Err("The laptop batteries changed. Review the current batteries and try again.".to_owned());
                }
                charge_limits::arguments_for(&maximum_copy, &this.charge_records.borrow())
            })();
            let arguments = match arguments {
                Err(error) => return finish(this, Some(error)),
                Ok(arguments) => arguments,
            };
            let set = charge_limits::parameters("setthreshold", Some(&arguments)).expect("valid arguments");
            let expected = expected.clone();
            this.request(charge_limits::ENDPOINT, "performAction", Some(set), None, true, true, Box::new(move |this, reply| {
                if !current(this) {
                    return;
                }
                let mut failure = match &reply { Err(error) => Some(error_text(error)), Ok(_) => None };
                if failure.is_none() {
                    if let Err(error) = charge_limits::decode_reply(&bytes(reply.as_ref().unwrap())) {
                        failure = Some(error);
                    }
                }
                this.publish_batteries();
                if failure.is_none() && (charge_limits::identity(&this.charge_records.borrow()) != expected
                    || this.charge_records.borrow().values().any(|r| r.stop != maximum)) {
                    failure = Some("The requested charge limit could not be confirmed on every laptop battery.".into());
                }
                finish(this, failure);
                this.refresh_power();
            }));
        }));
        true
    }

    fn idle_inhibited(self: &Rc<Self>, callback: Box<dyn FnOnce(bool)>) {
        let (ready, sleep_ready) = { let s = self.state.borrow(); (s.ready, s.sleep_ready) };
        if self.closed.get() || !ready || !sleep_ready {
            return callback(true);
        }
        self.request(LOGIN, "ListInhibitors", None, None, false, false, Box::new(move |this, reply| {
            let blocked = match reply { Err(_) => true, Ok(rows) => inhibited(&rows, &["idle"]).unwrap_or(true) };
            if blocked {
                callback(true);
            } else {
                this.session_active(Box::new(move |active| callback(!active)));
            }
        }));
    }

    /// Only the foreground local desktop may issue automatic power actions.
    fn session_active(self: &Rc<Self>, callback: Box<dyn FnOnce(bool)>) {
        let session = (self.session_id)();
        self.request(LOGIN, "GetSession", Some((session,).to_variant()), None, false, false, Box::new(move |this, reply| {
            let path = match reply { Ok(Value::String(path)) if !path.is_empty() => path, _ => return callback(false) };
            this.properties((LOGIN.0, &path, LOGIN_SESSION), Box::new(move |_, reply| {
                let Ok(Value::Object(state)) = reply else { return callback(false) };
                callback(is_true(state.get("Active")) && is_false(state.get("Remote"))
                         && matches!(state.get("Type").and_then(Value::as_str), Some("wayland" | "x11")));
            }));
        }));
    }

    fn allowed(&self, action: &str, interactive: bool) -> bool {
        let state = self.state.borrow();
        state.sleep_ready && match state.capability(action) {
            Some("yes") => true,
            Some("challenge") => interactive,
            _ => false,
        }
    }

    fn sleep(self: &Rc<Self>, action: &str, interactive: bool, guard: Option<Guard>) -> bool {
        let Some(&(_, method)) = SLEEP_METHODS.iter().find(|(a, _)| *a == action) else {
            return self.fail("Choose a supported sleep action.");
        };
        if !self.allowed(action, interactive) {
            return self.fail("That sleep action is unavailable or needs interactive authorization.");
        }
        let Some(operation) = self.begin_action() else { return false };
        let action = action.to_owned();
        self.request(LOGIN, "ListInhibitors", None, None, false, false, Box::new(move |this, reply| {
            if operation != this.operation.get() {
                return;
            }
            let (blocked, failed) = match &reply {
                Err(_) => (true, true),
                Ok(rows) => match inhibited(rows, &["idle", "sleep"]) { Ok(b) => (b, false), Err(_) => (true, false) },
            };
            if blocked {
                return this.finish_action(operation, Some(if failed { "Cannot verify sleep inhibitors; no power action was taken." }
                                                         else { "Sleep is currently inhibited by an application." }));
            }
            if !this.allowed(&action, interactive) {
                return this.finish_action(operation, Some("The sleep action is no longer available."));
            }
            let this2 = this.clone();
            let action2 = action.clone();
            let guard2 = guard.clone();
            this.session_active(Box::new(move |active| {
                let this = this2;
                if operation != this.operation.get() {
                    return;
                }
                if !active {
                    return this.finish_action(operation, None);
                }
                // A user's input or changed settings while asynchronous checks
                // were in flight must cancel the automatic request.
                if let Some(guard) = &guard2 {
                    if !guard() {
                        return this.finish_action(operation, None);
                    }
                }
                if !this.allowed(&action2, interactive) {
                    return this.finish_action(operation, None);
                }
                this.request(LOGIN, method, Some((interactive,).to_variant()), None, interactive, true, Box::new(move |this, reply| {
                    this.finish_action(operation, reply.err().map(|e| error_text(&e)).as_deref());
                }));
            }));
        }));
        true
    }

    fn explicit_power(self: &Rc<Self>, action: &str, callback: Option<Box<dyn FnOnce(bool)>>, interactive: bool) -> bool {
        let done: Box<dyn FnOnce(bool)> = callback.unwrap_or_else(|| Box::new(|_| {}));
        let method = match action {
            "poweroff" => "PowerOff",
            _ => match SLEEP_METHODS.iter().find(|(a, _)| *a == action) {
                Some((_, method)) => method,
                None => {
                    done(false);
                    return self.fail("Choose a supported power action.");
                }
            },
        };
        if self.closed.get() || self.bus.borrow().is_none() {
            done(false);
            return self.fail("System power services are unavailable.");
        }
        let run = move |this: &Rc<Inner>, capability: String| {
            let allowed = capability == "yes" || (interactive && capability == "challenge");
            if !allowed {
                done(false);
                this.fail("That power action is unavailable or needs authorization.");
                return;
            }
            this.clone().session_active(Box::new({
                let this = this.clone();
                move |active| {
                    if !active {
                        return done(false);
                    }
                    this.request(LOGIN, method, Some((interactive,).to_variant()), None, interactive, true, Box::new(move |this, reply| {
                        if let Err(error) = &reply {
                            this.fail(&error_text(error));
                        }
                        done(reply.is_ok());
                    }));
                }
            }));
        };
        if action == "poweroff" {
            self.request(LOGIN, "CanPowerOff", None, None, false, false, Box::new(move |this, reply| {
                let capability = match reply { Ok(Value::String(s)) => s, Ok(_) => "na".into(), Err(_) => "na".into() };
                run(this, capability);
            }));
        } else {
            let capability = {
                let state = self.state.borrow();
                if state.sleep_ready { state.capability(action).unwrap_or("na").to_owned() } else { "na".to_owned() }
            };
            run(self, capability);
        }
        true
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod battery_tests;
