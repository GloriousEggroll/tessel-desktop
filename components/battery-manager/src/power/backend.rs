//! Asynchronous UPower battery state and explicit charging controls
//! (`power_backend.PowerBackend(batteries_only=True)`).
//!
//! Startup and refresh are read-only. Charge settings change only through the
//! explicit action methods, after re-validating the affected hardware. Only
//! UPower is subscribed (no power profiles, no logind): this is the subset
//! Battery Manager owns; power-manager extends the same state machine.

use super::bus::{Bus, BusError, Call, Reply, Signal, SignalHandler, PROPERTIES};
use super::charge_limits::{self, Records};
use super::devices::{battery_percentage, device_batteries, device_identity, is_system_battery, system_batteries, UPOWER, UPOWER_DEVICE};
use super::scheduler::{GlibScheduler, Scheduler};
use super::values::{first, int, int_in, is_true, py_eq, to_value};
use glib::prelude::*;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};

/// What the battery UI, indicator and `--check` read (Python attribute access).
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

pub type Connector = Box<dyn Fn(Box<dyn FnOnce(Result<Rc<dyn Bus>, BusError>)>)>;
pub type Inspector = Box<dyn Fn(&Map<String, Value>) -> (Value, Records)>;

/// Injection points (Python tests patch `Gio.bus_get` and `inspect_limits`).
pub struct Options {
    pub connector: Connector,
    pub scheduler: Rc<dyn Scheduler>,
    pub inspect: Inspector,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connector: Box::new(|done| super::bus::connect(gio::BusType::System, None, done)),
            scheduler: Rc::new(GlibScheduler::default()),
            inspect: Box::new(|devices| {
                charge_limits::inspect_limits(devices, Path::new(charge_limits::SUPPLIES), &charge_limits::helper_files())
            }),
        }
    }
}

#[derive(Default)]
struct PowerRead {
    manager: Option<Value>,
    devices: Option<Map<String, Value>>,
    errors: Vec<BusError>,
}

pub struct State {
    pub ready: bool,
    pub power_ready: bool,
    pub busy: bool,
    pub error: String,
    pub has_battery: bool,
    pub on_battery: bool,
    pub percentage: Option<f64>,
    /// Highest UPower WarningLevel of any system battery (1 none … 5 action).
    pub warning_level: i64,
    pub batteries: Vec<Value>,
    pub power_source_known: bool,
    pub charge_limit: Value,
    closed: bool,
    started: bool,
    connecting: bool,
    generation: u64,
    reads_power: u64,
    errors: Vec<(String, String)>,
    operation: u64,
    power_devices: Map<String, Value>,
    upower_on_battery: Value,
    power_epoch: u64,
    battery_action: Option<String>,
    charge_limit_action: bool,
    charge_limit_error: String,
    charge_records: Records,
    sources: HashMap<&'static str, u64>,
    subscriptions: Vec<u32>,
}

struct Inner {
    state: RefCell<State>,
    bus: RefCell<Option<Rc<dyn Bus>>>,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    options: Options,
}

/// Cheap clonable handle (`Rc`).
#[derive(Clone)]
pub struct PowerBackend(Rc<Inner>);

type Callback = Box<dyn FnOnce(&PowerBackend, Reply)>;

fn error_text(error: &BusError) -> String {
    let name = error.name.as_deref().unwrap_or("");
    if ["NotAuthorized", "AccessDenied", "InteractiveAuthorizationRequired", "Cancelled", "Canceled"]
        .iter().any(|part| name.contains(part)) {
        return "Authorization was canceled or denied.".into();
    }
    error.message.clone()
}

impl PowerBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static, options: Options) -> Self {
        let (charge_limit, charge_records) = (options.inspect)(&Map::new());
        let state = State {
            ready: false, power_ready: false, busy: false, error: String::new(), has_battery: false, on_battery: false,
            percentage: None, warning_level: 0, batteries: Vec::new(), power_source_known: false, charge_limit,
            closed: false, started: false, connecting: false, generation: 0, reads_power: 0, errors: Vec::new(),
            operation: 0, power_devices: Map::new(), upower_on_battery: Value::Null, power_epoch: 0,
            battery_action: None, charge_limit_action: false, charge_limit_error: String::new(), charge_records,
            sources: HashMap::new(), subscriptions: Vec::new(),
        };
        Self(Rc::new(Inner { state: RefCell::new(state), bus: RefCell::new(None), on_changed: Box::new(on_changed),
                             on_error: Box::new(on_error), options }))
    }

    /// The system-bus backend used by the application and the indicator stream.
    pub fn system(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) -> Self {
        Self::new(on_changed, on_error, Options::default())
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn upgrade(weak: &Weak<Inner>) -> Option<Self> {
        weak.upgrade().map(Self)
    }

    fn state(&self) -> std::cell::RefMut<'_, State> {
        self.0.state.borrow_mut()
    }

    /// Read access to the published state.
    pub fn with<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        f(&self.0.state.borrow())
    }

    pub fn ready(&self) -> bool {
        self.with(|s| s.ready)
    }

    pub fn warning_level(&self) -> i64 {
        self.with(|s| s.warning_level)
    }

    fn closed(&self) -> bool {
        self.with(|s| s.closed)
    }

    fn publish_batteries(&self) {
        let devices = {
            let s = self.0.state.borrow();
            (s.power_devices.clone(), s.battery_action.clone())
        };
        let mut batteries = device_batteries(&devices.0, devices.1.as_deref());
        let (mut charge_limit, records) = (self.0.options.inspect)(&devices.0);
        let mut s = self.state();
        charge_limit["busy"] = json!(s.busy);
        charge_limit["error"] = json!(s.charge_limit_error);
        for device in &mut batteries {
            let path = device["path"].as_str().unwrap_or_default().to_owned();
            let record = records.get(&path);
            device["charge_limit_adjustable"] = json!(record.is_some());
            if let Some(record) = record {
                // UPower's percentages describe its preset, not necessarily
                // what another authorized controller applied to the hardware.
                device["charge_start_threshold"] = json!(record.start);
                device["charge_end_threshold"] = json!(record.stop);
                device["charge_threshold_enabled"] = json!(record.stop < 100);
                device["busy"] = json!(is_true(device.get("busy")) || s.charge_limit_action);
            }
        }
        s.batteries = batteries;
        s.charge_limit = charge_limit;
        s.charge_records = records;
    }

    fn notify(&self) {
        let closed = {
            let mut s = self.state();
            s.error = s.errors.iter().map(|e| e.1.as_str()).collect::<Vec<_>>().join("\n");
            let busy = s.busy;
            s.charge_limit["busy"] = json!(busy);
            s.closed
        };
        if !closed {
            (self.0.on_changed)();
        }
    }

    fn set_error(&self, section: &str, message: &str) {
        {
            let mut s = self.state();
            if message.is_empty() {
                s.errors.retain(|e| e.0 != section);
            } else if let Some(entry) = s.errors.iter_mut().find(|e| e.0 == section) {
                entry.1 = message.to_owned();
            } else {
                s.errors.push((section.to_owned(), message.to_owned()));
            }
        }
        self.notify();
    }

    fn fail(&self, message: &str) -> bool {
        self.set_error("action", message);
        if !self.closed() {
            (self.0.on_error)(message);
        }
        false
    }

    fn later(&self, key: &'static str, milliseconds: u32, callback: fn(&PowerBackend)) {
        {
            let s = self.0.state.borrow();
            if s.closed || s.sources.contains_key(key) {
                return;
            }
        }
        let weak = self.downgrade();
        let id = self.0.options.scheduler.timeout(milliseconds, Box::new(move || {
            let Some(this) = Self::upgrade(&weak) else { return };
            let closed = {
                let mut s = this.state();
                s.sources.remove(key);
                s.closed
            };
            if !closed {
                callback(&this);
            }
        }));
        self.state().sources.insert(key, id);
    }

    /// Whether a keyed timer (`_sources`) is pending.
    pub fn has_timer(&self, key: &str) -> bool {
        self.with(|s| s.sources.contains_key(key))
    }

    pub fn start(&self) {
        {
            let mut s = self.state();
            if s.closed || s.started {
                return;
            }
            s.started = true;
        }
        self.connect();
    }

    fn connect(&self) {
        let generation = {
            let mut s = self.state();
            if s.closed || s.connecting || self.0.bus.borrow().is_some() {
                return;
            }
            s.connecting = true;
            s.generation
        };
        let weak = self.downgrade();
        (self.0.options.connector)(Box::new(move |result| {
            if let Some(this) = Self::upgrade(&weak) {
                this.connected(generation, result);
            }
        }));
    }

    fn connected(&self, generation: u64, result: Result<Rc<dyn Bus>, BusError>) {
        {
            let mut s = self.state();
            if s.closed || generation != s.generation {
                return;
            }
            s.connecting = false;
        }
        match result {
            Err(error) => {
                self.set_error("bus", &format!("System power services are unavailable: {}", error_text(&error)));
                self.later("connect", 5000, |this| this.connect());
            }
            Ok(bus) => {
                self.0.bus.replace(Some(bus.clone()));
                let weak = self.downgrade();
                bus.on_closed(Some(Box::new(move || {
                    if let Some(this) = Self::upgrade(&weak) {
                        this.bus_closed();
                    }
                })));
                {
                    let mut s = self.state();
                    s.ready = true;
                    s.errors.retain(|e| e.0 != "bus");
                }
                self.subscribe();
                self.refresh();
            }
        }
    }

    fn subscribe(&self) {
        let Some(bus) = self.0.bus.borrow().clone() else { return };
        let weak = self.downgrade();
        let owner: SignalHandler = Rc::new(move |signal: &Signal| {
            if let Some(this) = Self::upgrade(&weak) {
                this.owner_changed(&signal.params);
            }
        });
        let weak = self.downgrade();
        let power: SignalHandler = Rc::new(move |signal: &Signal| {
            if let Some(this) = Self::upgrade(&weak) {
                this.power_signal(&signal.path, &signal.interface, &signal.member, &signal.params);
            }
        });
        // batteries_only: only the UPower endpoint (no PPD, no logind).
        let ids = vec![
            bus.subscribe(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"), Some("NameOwnerChanged"),
                          Some("/org/freedesktop/DBus"), Some(UPOWER.0), owner),
            bus.subscribe(Some(UPOWER.0), Some(PROPERTIES), Some("PropertiesChanged"), None, None, power.clone()),
            bus.subscribe(Some(UPOWER.0), Some(UPOWER.2), None, Some(UPOWER.1), None, power),
        ];
        self.state().subscriptions.extend(ids);
    }

    fn detach(&self) {
        let bus = self.0.bus.borrow_mut().take();
        let subscriptions = std::mem::take(&mut self.state().subscriptions);
        if let Some(bus) = bus {
            for id in subscriptions {
                bus.unsubscribe(id);
            }
            bus.on_closed(None);
        }
    }

    fn bus_closed(&self) {
        let bus = self.0.bus.borrow().clone();
        if let Some(bus) = bus {
            bus.cancel_reads();
        }
        self.state().generation += 1;
        self.detach();
        {
            let mut s = self.state();
            s.ready = false;
            s.power_ready = false;
            s.power_source_known = false;
            s.operation += 1;
            s.busy = false;
            s.battery_action = None;
            s.power_epoch += 1;
            s.power_devices.clear();
            s.charge_limit_action = false;
        }
        self.publish_batteries();
        self.set_error("bus", "System power services are disconnected.");
        self.later("connect", 2000, |this| this.connect());
    }

    /// Stop all reads, timers and subscriptions; pending replies are dropped.
    pub fn close(&self) {
        let sources: Vec<u64> = {
            let mut s = self.state();
            if s.closed {
                return;
            }
            s.closed = true;
            s.generation += 1;
            s.sources.drain().map(|(_, id)| id).collect()
        };
        let bus = self.0.bus.borrow().clone();
        if let Some(bus) = bus {
            bus.cancel_reads();
        }
        for id in sources {
            self.0.options.scheduler.cancel(id);
        }
        self.detach();
        {
            let mut s = self.state();
            s.ready = false;
            s.power_ready = false;
            s.power_source_known = false;
            s.busy = false;
            s.battery_action = None;
            s.power_devices.clear();
            s.charge_limit_action = false;
        }
        self.publish_batteries();
    }

    fn request(&self, call: Call, callback: Callback) {
        let (closed, generation) = self.with(|s| (s.closed, s.generation));
        let bus = self.0.bus.borrow().clone();
        let Some(bus) = bus.filter(|_| !closed) else {
            callback(self, Err(BusError::local("The system bus is unavailable.")));
            return;
        };
        let weak = self.downgrade();
        bus.call(call, Box::new(move |reply| {
            let Some(this) = Self::upgrade(&weak) else { return };
            let current = this.with(|s| !s.closed && generation == s.generation);
            if current {
                callback(&this, reply);
            }
        }));
    }

    fn properties(&self, name: &str, path: &str, interface: &str, callback: Callback) {
        self.request(Call::new(name, path, PROPERTIES, "GetAll", Some((interface,).to_variant())), callback);
    }

    /// `_owner_changed` (signal arguments `(sss)`).
    pub fn owner_changed(&self, params: &glib::Variant) {
        let Some((name, _old, _new)) = params.get::<(String, String, String)>() else { return };
        if name == UPOWER.0 {
            {
                let mut s = self.state();
                s.reads_power += 1;
                s.power_ready = false;
                s.power_source_known = false;
                s.power_epoch += 1;
                s.power_devices.clear();
                if s.battery_action.is_some() || s.charge_limit_action {
                    s.operation += 1;
                    s.battery_action = None;
                    s.charge_limit_action = false;
                    s.busy = false;
                }
            }
            self.publish_batteries();
            self.later("power", 100, |this| this.refresh_power());
        }
        self.notify();
    }

    /// `_power_signal`: UPower property changes and DeviceAdded/DeviceRemoved.
    pub fn power_signal(&self, path: &str, interface: &str, member: &str, params: &glib::Variant) {
        // Charge updates can arrive every few seconds. Marking those updates as
        // an unknown source would continuously reset the desktop's idle timers.
        let mut uncertain = false;
        if interface == PROPERTIES && member == "PropertiesChanged" {
            if params.type_().as_str() != "(sa{sv}as)" {
                return;
            }
            let changed_interface = params.child_value(0).str().unwrap_or_default().to_owned();
            let changed = to_value(&params.child_value(1));
            let invalidated: Vec<String> = params.child_value(2).get().unwrap_or_default();
            let s = self.0.state.borrow();
            if path == UPOWER.1 && changed_interface == UPOWER.2 {
                uncertain = invalidated.iter().any(|name| name == "OnBattery")
                    || changed.get("OnBattery").is_some_and(|value| !py_eq(Some(value), Some(&s.upower_on_battery)));
            } else if changed_interface == UPOWER_DEVICE {
                let previous = s.power_devices.get(path).cloned().unwrap_or_else(|| json!({}));
                let mut updated = previous.clone();
                if let (Some(target), Some(source)) = (updated.as_object_mut(), changed.as_object()) {
                    for (key, value) in source {
                        target.insert(key.clone(), value.clone());
                    }
                }
                let was_supply = is_system_battery(&previous);
                let is_supply = is_system_battery(&updated);
                uncertain = was_supply != is_supply;
                if (was_supply || is_supply)
                    && invalidated.iter().any(|name| ["Type", "PowerSupply", "IsPresent", "State"].contains(&name.as_str())) {
                    uncertain = true;
                }
                if int_in(updated.get("Type"), &[3]) && is_supply && changed.get("State").is_some() {
                    uncertain |= int_in(previous.get("State"), &[2, 3]) != int_in(updated.get("State"), &[2, 3]);
                }
            }
        } else if interface == UPOWER.2 && member == "DeviceRemoved" {
            let removed_path = params.try_child_value(0).and_then(|v| v.str().map(String::from)).unwrap_or_default();
            let (action, operation) = {
                let mut s = self.state();
                let removed = s.power_devices.shift_remove(&removed_path).unwrap_or_else(|| json!({}));
                uncertain = is_system_battery(&removed);
                if uncertain {
                    s.power_source_known = false;
                }
                (s.battery_action.clone(), s.operation)
            };
            if action.as_deref() == Some(removed_path.as_str()) {
                self.finish_action(operation, Some("That battery is no longer available."));
                self.state().operation += 1;
            }
            self.publish_batteries();
            self.notify();
        }
        // A refresh discovers added supplies without interrupting idle timers
        // merely because a Bluetooth mouse/headset has appeared.
        self.state().reads_power += 1;
        if uncertain {
            self.state().power_source_known = false;
            self.notify();
        }
        self.later("power", 100, |this| this.refresh_power());
    }

    pub fn refresh(&self) {
        if self.closed() {
            return;
        }
        if self.0.bus.borrow().is_none() {
            self.connect();
            return;
        }
        self.refresh_power();
        // A fallback only: UPower signals deliver changes immediately.
        // Display-only battery consumers need full re-enumeration rarely.
        self.later("refresh", 120000, |this| this.refresh());
    }

    fn refresh_power(&self) {
        let generation = {
            let mut s = self.state();
            s.reads_power += 1;
            s.reads_power
        };
        let read = Rc::new(RefCell::new(PowerRead::default()));
        let manager_read = read.clone();
        self.properties(UPOWER.0, UPOWER.1, UPOWER.2, Box::new(move |this, reply| {
            {
                let mut read = manager_read.borrow_mut();
                match reply {
                    Ok(value) => read.manager = Some(first(&value)),
                    Err(error) => {
                        read.manager = Some(json!({}));
                        read.errors.push(error);
                    }
                }
            }
            this.finish_power(generation, &manager_read);
        }));
        let enumerate_read = read;
        self.request(Call::new(UPOWER.0, UPOWER.1, UPOWER.2, "EnumerateDevices", None), Box::new(move |this, reply| {
            if generation != this.with(|s| s.reads_power) {
                return;
            }
            let value = match reply {
                Ok(value) => value,
                Err(error) => {
                    {
                        let mut read = enumerate_read.borrow_mut();
                        read.errors.push(error);
                        read.devices = Some(Map::new());
                    }
                    this.finish_power(generation, &enumerate_read);
                    return;
                }
            };
            let mut paths: Vec<String> = Vec::new();
            if let Value::Array(items) = first(&value) {
                for item in items {
                    if let Value::String(path) = item {
                        if !paths.contains(&path) {
                            paths.push(path);
                        }
                    }
                }
            }
            if paths.is_empty() {
                enumerate_read.borrow_mut().devices = Some(Map::new());
                this.finish_power(generation, &enumerate_read);
                return;
            }
            let pending = Rc::new(RefCell::new(paths.clone()));
            let devices = Rc::new(RefCell::new(Map::new()));
            for path in paths {
                let (pending, devices, read) = (pending.clone(), devices.clone(), enumerate_read.clone());
                let device_path = path.clone();
                this.properties(UPOWER.0, &path, UPOWER_DEVICE, Box::new(move |this, reply| {
                    pending.borrow_mut().retain(|p| *p != device_path);
                    match reply {
                        Ok(value) => {
                            devices.borrow_mut().insert(device_path, first(&value));
                        }
                        Err(error) => read.borrow_mut().errors.push(error),
                    }
                    if pending.borrow().is_empty() {
                        read.borrow_mut().devices = Some(devices.borrow().clone());
                        this.finish_power(generation, &read);
                    }
                }));
            }
        }));
    }

    fn finish_power(&self, generation: u64, read: &Rc<RefCell<PowerRead>>) {
        if generation != self.with(|s| s.reads_power) {
            return;
        }
        let (manager, devices, failed) = {
            let read = read.borrow();
            let (Some(manager), Some(devices)) = (read.manager.clone(), read.devices.clone()) else { return };
            (manager, devices, !read.errors.is_empty())
        };
        let on_battery = match manager.get("OnBattery") {
            Some(Value::Bool(flag)) if !failed => *flag,
            _ => {
                {
                    let mut s = self.state();
                    s.power_ready = false;
                    s.power_source_known = false;
                }
                self.set_error("power", "The current power source is unavailable. Check UPower.");
                return;
            }
        };
        {
            let mut s = self.state();
            let batteries = system_batteries(devices.values());
            s.has_battery = !batteries.is_empty();
            // UPower's root OnBattery covers laptops. Some UPS drivers only
            // expose discharging on the UPS device itself.
            s.on_battery = on_battery || batteries.iter()
                .any(|device| int_in(device.get("Type"), &[3]) && int_in(device.get("State"), &[2, 3]));
            s.percentage = battery_percentage(&batteries);
            s.warning_level = batteries.iter().filter_map(|device| int(device.get("WarningLevel")))
                .filter(|level| (0..=5).contains(level)).max().unwrap_or(0);
            s.upower_on_battery = json!(on_battery);
            s.power_devices = devices;
        }
        self.publish_batteries();
        {
            let mut s = self.state();
            s.power_ready = true;
            s.power_source_known = true;
        }
        self.set_error("power", "");
    }

    fn begin_action(&self) -> Option<u64> {
        let operation = {
            let mut s = self.state();
            if s.closed || s.busy {
                return None;
            }
            s.operation += 1;
            s.busy = true;
            s.errors.retain(|e| e.0 != "action");
            s.operation
        };
        self.notify();
        Some(operation)
    }

    fn finish_action(&self, operation: u64, error: Option<&str>) {
        {
            let mut s = self.state();
            if s.closed || operation != s.operation {
                return;
            }
            s.busy = false;
            s.battery_action = None;
            s.charge_limit_action = false;
        }
        self.publish_batteries();
        match error {
            Some(error) => {
                self.fail(error);
            }
            None => self.set_error("action", ""),
        }
    }

    /// Enable the battery's configured charge limits through UPower.
    ///
    /// Read back supported hardware and identity before the authorized mutation;
    /// startup, hotplug and refresh never change charging behavior.
    pub fn set_charge_thresholds(&self, path: &str, enabled: bool) -> bool {
        let known = self.with(|s| s.power_ready && s.power_devices.contains_key(path));
        if !known {
            return self.fail("That battery is no longer available.");
        }
        self.publish_batteries();
        if self.with(|s| s.charge_records.contains_key(path)) {
            return self.fail("Use Maximum charge to adjust the laptop battery limits.");
        }
        let original = self.with(|s| s.power_devices[path].clone());
        let supported = |properties: &Value| {
            let single = Map::from_iter([(path.to_owned(), properties.clone())]);
            device_batteries(&single, None).first().is_some_and(|b| is_true(b.get("charge_threshold_supported")))
        };
        if !supported(&original) {
            return self.fail("Charge limits are not supported by this battery.");
        }
        let Some(operation) = self.begin_action() else { return false };
        self.state().battery_action = Some(path.to_owned());
        self.publish_batteries();
        self.notify();
        let identity = device_identity(&original);
        let epoch = self.with(|s| s.power_epoch);
        let path = path.to_owned();

        let still_current = {
            let (path, identity) = (path.clone(), identity.clone());
            Rc::new(move |this: &PowerBackend| this.with(|s| {
                !s.closed && operation == s.operation && epoch == s.power_epoch && s.power_ready
                    && s.power_devices.get(&path).is_some_and(|device| device_identity(device) == identity)
            }))
        };
        let pending = move |this: &PowerBackend| this.with(|s| operation == s.operation && !s.closed);

        let endpoint_path = path.clone();
        self.properties(UPOWER.0, &path, UPOWER_DEVICE, Box::new(move |this, reply| {
            if !pending(this) {
                return;
            }
            if !still_current(this) {
                this.finish_action(operation, Some("That battery changed or is no longer available."));
                return;
            }
            let properties = match reply {
                Ok(value) => first(&value),
                Err(error) => {
                    this.finish_action(operation, Some(&error_text(&error)));
                    return;
                }
            };
            let single = Map::from_iter([(endpoint_path.clone(), properties.clone())]);
            let battery = device_batteries(&single, None);
            if identity != device_identity(&properties) || !battery.first().is_some_and(|b| is_true(b.get("charge_threshold_supported"))) {
                this.finish_action(operation, Some("Charge limits are no longer supported by this battery."));
                this.refresh_power();
                return;
            }
            this.state().power_devices.insert(endpoint_path.clone(), properties.clone());
            if properties.get("ChargeThresholdEnabled") == Some(&Value::Bool(enabled)) {
                this.finish_action(operation, None);
                return;
            }
            let (write_path, current) = (endpoint_path.clone(), still_current.clone());
            let call = Call::new(UPOWER.0, &endpoint_path, UPOWER_DEVICE, "EnableChargeThreshold", Some((enabled,).to_variant())).mutation();
            this.request(call, Box::new(move |this, reply| {
                if !pending(this) {
                    return;
                }
                if let Err(error) = reply {
                    this.finish_action(operation, Some(&error_text(&error)));
                    this.refresh_power();
                    return;
                }
                let read_path = write_path.clone();
                this.properties(UPOWER.0, &write_path, UPOWER_DEVICE, Box::new(move |this, reply| {
                    if !pending(this) {
                        return;
                    }
                    if !current(this) {
                        this.finish_action(operation, Some("That battery changed or is no longer available."));
                        return;
                    }
                    match reply {
                        Err(_) => this.finish_action(operation, Some("The charge setting changed, but its current status could not be read.")),
                        Ok(value) => {
                            let result = first(&value);
                            if device_identity(&result) != identity {
                                this.finish_action(operation, Some("That battery changed or is no longer available."));
                            } else {
                                let applied = result.get("ChargeThresholdEnabled") == Some(&Value::Bool(enabled));
                                this.state().power_devices.insert(read_path, result);
                                this.finish_action(operation, (!applied).then_some("The battery did not apply the requested charge limit."));
                            }
                        }
                    }
                    this.refresh_power();
                }));
            }));
        }));
        true
    }

    /// Explicitly apply one global maximum through KDE's trusted helper.
    ///
    /// Reads and opening the application never activate this helper. Its
    /// existing polkit policy authorizes the actual D-Bus sender. The helper
    /// writes all supported batteries, so the UI exposes one global setting.
    pub fn set_charge_limit(&self, maximum: &Value) -> bool {
        if !self.with(|s| s.power_ready) {
            return self.fail("The laptop battery information is not available.");
        }
        self.publish_batteries();
        let (supported, reason) = self.with(|s| (is_true(s.charge_limit.get("supported")),
                                                 s.charge_limit["reason"].as_str().unwrap_or_default().to_owned()));
        if !supported {
            return self.fail(&reason);
        }
        if let Err(error) = self.with(|s| charge_limits::arguments_for(maximum, &s.charge_records)) {
            return self.fail(&error);
        }
        if self.with(|s| s.busy || s.closed) {
            return false;
        }
        let maximum = int(Some(maximum)).unwrap_or_default();
        if self.with(|s| s.charge_records.values().all(|record| record.stop == maximum)) {
            return true;
        }
        let expected = self.with(|s| charge_limits::identity(&s.charge_records));
        let epoch = self.with(|s| s.power_epoch);
        let Some(operation) = self.begin_action() else { return false };
        {
            let mut s = self.state();
            s.charge_limit_action = true;
            s.charge_limit_error.clear();
        }
        self.publish_batteries();
        self.notify();

        let current = move |this: &PowerBackend| this.with(|s| {
            !s.closed && operation == s.operation && epoch == s.power_epoch && s.power_ready
        });
        let finish = move |this: &PowerBackend, error: Option<String>| {
            this.state().charge_limit_error = error.clone().unwrap_or_default();
            this.finish_action(operation, error.as_deref());
        };
        let (endpoint_name, endpoint_path, endpoint_interface) = charge_limits::ENDPOINT;
        let read_expected = expected.clone();
        let read_back: Callback = Box::new(move |this, reply| {
            if !current(this) {
                return;
            }
            let mut failure = match reply {
                Err(error) => Some(error_text(&error)),
                Ok(value) => charge_limits::reply_blob(&value).and_then(|blob| charge_limits::decode_reply(&blob)).err(),
            };
            this.publish_batteries();
            if failure.is_none() && this.with(|s| charge_limits::identity(&s.charge_records) != read_expected
                    || s.charge_records.values().any(|record| record.stop != maximum)) {
                failure = Some("The requested charge limit could not be confirmed on every laptop battery.".into());
            }
            finish(this, failure);
            this.refresh_power();
        });
        let verified: Callback = Box::new(move |this, reply| {
            if !current(this) {
                return;
            }
            let value = match reply {
                Ok(value) => value,
                Err(error) => return finish(this, Some(error_text(&error))),
            };
            let checked = (|| {
                charge_limits::decode_reply(&charge_limits::reply_blob(&value)?)?;
                this.publish_batteries();
                if this.with(|s| charge_limits::identity(&s.charge_records)) != expected {
                    return Err("The laptop batteries changed. Review the current batteries and try again.".to_owned());
                }
                let arguments = this.with(|s| charge_limits::arguments_for(&json!(maximum), &s.charge_records))?;
                charge_limits::parameters("setthreshold", Some(&arguments))
            })();
            match checked {
                Err(error) => finish(this, Some(error)),
                Ok(parameters) => {
                    let call = Call::new(endpoint_name, endpoint_path, endpoint_interface, "performAction", Some(parameters)).mutation();
                    this.request(call, read_back);
                }
            }
        });
        let parameters = charge_limits::parameters("getthreshold", None).expect("static parameters");
        self.request(Call::new(endpoint_name, endpoint_path, endpoint_interface, "performAction", Some(parameters)), verified);
        true
    }
}

impl BatteryModel for PowerBackend {
    fn power_ready(&self) -> bool { self.with(|s| s.power_ready) }
    fn power_source_known(&self) -> bool { self.with(|s| s.power_source_known) }
    fn on_battery(&self) -> bool { self.with(|s| s.on_battery) }
    fn has_battery(&self) -> bool { self.with(|s| s.has_battery) }
    fn percentage(&self) -> Option<f64> { self.with(|s| s.percentage) }
    fn busy(&self) -> bool { self.with(|s| s.busy) }
    fn error(&self) -> String { self.with(|s| s.error.clone()) }
    fn batteries(&self) -> Vec<Value> { self.with(|s| s.batteries.clone()) }
    fn charge_limit(&self) -> Value { self.with(|s| s.charge_limit.clone()) }
    fn set_charge_thresholds(&self, path: &str, enabled: bool) -> bool { PowerBackend::set_charge_thresholds(self, path, enabled) }
    fn set_charge_limit(&self, maximum: &Value) -> bool { PowerBackend::set_charge_limit(self, maximum) }
}

#[cfg(test)]
mod tests;
