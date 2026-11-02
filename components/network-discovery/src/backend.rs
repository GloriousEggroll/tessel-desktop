//! Avahi discovery control adapted from NDDT (MIT; see upstream/LICENSE),
//! port of discovery_backend.py.
//!
//! Observe systemd over D-Bus instead of repeatedly spawning status commands.
//! Only an explicit user action invokes the original privileged systemctl
//! change. Tessel Dock embeds this backend in-process (status_backend.py).

use gio::prelude::*;
use regex::Regex;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::rc::{Rc, Weak};
use std::sync::LazyLock;

pub const SOCKET: &str = "avahi-daemon.socket";
pub const SERVICE: &str = "avahi-daemon.service";
pub const UNITS: [&str; 2] = [SOCKET, SERVICE];
pub const BUS_NAME: &str = "org.freedesktop.systemd1";
pub const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
pub const MANAGER: &str = "org.freedesktop.systemd1.Manager";
pub const UNIT_INTERFACE: &str = "org.freedesktop.systemd1.Unit";

static PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[\w.-]+-quark:\s*)?(?:GDBus\.Error:[\w.]+:\s*)?").unwrap());
static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\(\d+\)$").unwrap());

const FRIENDLY: [(&[&str], &str); 4] = [
    (&["AccessDenied", "PermissionDenied", "InteractiveAuthorizationRequired", "Not authorized", "not authorized"],
     "The system did not allow this change."),
    (&["NoSuchUnit", "LoadFailed", "NotFound", "not found", "does not exist"], "Avahi is not installed."),
    (&["ServiceUnknown", "NameHasNoOwner", "Disconnected", "Could not connect", "connection refused"],
     "The system service manager is unavailable."),
    (&["Timeout", "TimedOut", "timed out", "NoReply"], "The system service manager did not respond in time."),
];

/// Python `str.splitlines()` line boundaries.
fn last_line(text: &str) -> &str {
    let boundaries = ['\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}'];
    let trimmed = text.trim_end_matches(boundaries);
    match trimmed.rfind(boundaries) {
        Some(index) => &trimmed[index + trimmed[index..].chars().next().unwrap().len_utf8()..],
        None => trimmed,
    }
}

/// `friendly_error(text)`: short, readable text for D-Bus or systemctl
/// errors; empty when cancelled.
pub fn friendly_text(text: &str) -> String {
    let text = text.trim();
    let lowered = text.to_lowercase();
    if matches!(lowered.trim_end_matches('.'), "operation was cancelled" | "cancelled" | "request dismissed") {
        return String::new();
    }
    for (needles, message) in FRIENDLY {
        if needles.iter().any(|needle| text.contains(needle)) {
            return message.into();
        }
    }
    let line = if text.is_empty() { "" } else { last_line(text) };
    let line = PREFIX.replace(line, "");
    CODE.replace(&line, "").trim().to_owned()
}

/// `friendly_error(GLib.Error)`: str(GLib.Error) includes the error quark,
/// remote D-Bus error name and a numeric code; Gio CANCELLED is silent.
pub fn friendly_error(error: &glib::Error) -> String {
    if error.matches(gio::IOErrorEnum::Cancelled) {
        return String::new();
    }
    friendly_text(error.message())
}

/// The privileged command: enabling persists the socket; disabling stops
/// both so socket activation cannot immediately bring discovery back.
pub fn change_command(enabled: bool) -> Vec<String> {
    let mut command = vec!["/usr/bin/pkexec".to_owned(), "/usr/bin/systemctl".to_owned()];
    if enabled {
        command.extend(["enable", "--now", SOCKET].map(String::from));
    } else {
        command.extend(["disable", "--now", SOCKET, SERVICE].map(String::from));
    }
    command
}

/// `change_command(value)` for untyped input (JSON): non-bools are refused.
pub fn change_command_value(value: &Value) -> Result<Vec<String>, String> {
    match value {
        Value::Bool(enabled) => Ok(change_command(*enabled)),
        _ => Err("Discovery state must be a boolean".into()),
    }
}

/// `summarize(units, ready, error, busy)`: the dock/`--status` snapshot.
pub fn summarize(units: &Map<String, Value>, ready: bool, error: &str, busy: bool) -> Map<String, Value> {
    let field = |info: &Value, key: &str| info.get(key).cloned();
    let installed = units.values().any(|info| match field(info, "LoadState") {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty() && s != "not-found",
        Some(_) => true,
    });
    let enabled = units.values().any(|info| {
        matches!(field(info, "ActiveState"), Some(Value::String(s)) if ["active", "activating", "reloading"].contains(&s.as_str()))
    });
    let label = if busy {
        "Waiting for authentication…"
    } else if !error.is_empty() {
        error
    } else if ready && !installed {
        "Avahi is not installed"
    } else if enabled {
        "Enabled"
    } else if ready {
        "Disabled"
    } else {
        "Checking…"
    };
    let class = if busy {
        "pending"
    } else if !error.is_empty() || !ready || !installed {
        "unavailable"
    } else if enabled {
        "active"
    } else {
        "inactive"
    };
    let mut state = Map::new();
    state.insert("text".into(), "\u{f1e0}".into());
    state.insert("tooltip".into(), format!("Discover Network Devices: {label}\nClick for discovery controls").into());
    state.insert("class".into(), class.into());
    state.insert("ready".into(), (ready && error.is_empty()).into());
    state.insert("installed".into(), installed.into());
    state.insert("enabled".into(), enabled.into());
    state.insert("busy".into(), busy.into());
    state.insert("error".into(), error.into());
    state.insert("units".into(), Value::Object(units.clone()));
    state
}

pub fn snapshot_text(state: &Map<String, Value>) -> String {
    tessel_core::pyfmt::json_dumps(&Value::Object(state.clone()), &tessel_core::pyfmt::JsonStyle::DEFAULT)
}

/// Starts the privileged command (tests substitute a recorder).
pub type Spawner = Rc<dyn Fn(&[String]) -> Result<gio::Subprocess, glib::Error>>;

pub fn default_spawner() -> Spawner {
    Rc::new(|argv: &[String]| {
        let argv: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
        gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE)
    })
}

/// `done(success, error)` of `set_enabled`.
pub type Done = Box<dyn FnOnce(bool, String)>;

struct Inner {
    changed: Box<dyn Fn()>,
    spawner: Spawner,
    manager: RefCell<Option<gio::DBusProxy>>,
    manager_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    units: RefCell<Map<String, Value>>,
    proxies: RefCell<Vec<(String, gio::DBusProxy)>>,
    unit_handlers: RefCell<Vec<(gio::DBusProxy, glib::SignalHandlerId)>>,
    pending: RefCell<BTreeSet<String>>,
    error: RefCell<String>,
    ready: Cell<bool>,
    busy: Cell<bool>,
    closed: Cell<bool>,
    started: Cell<bool>,
    cancel: gio::Cancellable,
    process: RefCell<Option<gio::Subprocess>>,
    generation: Cell<u64>,
}

/// systemd's view of the two Avahi units, kept current from D-Bus signals.
#[derive(Clone)]
pub struct DiscoveryBackend(Rc<Inner>);

impl DiscoveryBackend {
    pub fn new(changed: impl Fn() + 'static) -> Self { Self::with_spawner(changed, default_spawner()) }

    pub fn with_spawner(changed: impl Fn() + 'static, spawner: Spawner) -> Self {
        Self(Rc::new(Inner {
            changed: Box::new(changed), spawner, manager: RefCell::new(None), manager_handlers: RefCell::default(),
            units: RefCell::default(), proxies: RefCell::default(), unit_handlers: RefCell::default(),
            pending: RefCell::default(), error: RefCell::default(), ready: Cell::new(false), busy: Cell::new(false),
            closed: Cell::new(false), started: Cell::new(false), cancel: gio::Cancellable::new(),
            process: RefCell::new(None), generation: Cell::new(0),
        }))
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    fn upgrade(weak: &Weak<Inner>) -> Option<Self> { weak.upgrade().map(Self) }

    fn changed(&self) { (self.0.changed)() }

    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn busy(&self) -> bool { self.0.busy.get() }
    pub fn closed(&self) -> bool { self.0.closed.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn units(&self) -> Map<String, Value> { self.0.units.borrow().clone() }

    /// Test/state seeding (Python tests assign `backend.ready`/`backend.units`).
    pub fn set_state(&self, ready: bool, units: Map<String, Value>) {
        self.0.ready.set(ready);
        self.0.units.replace(units);
    }

    pub fn snapshot(&self) -> Map<String, Value> {
        summarize(&self.0.units.borrow(), self.0.ready.get(), &self.0.error.borrow(), self.0.busy.get())
    }

    pub fn start(&self) {
        if self.0.started.get() || self.0.closed.get() {
            return;
        }
        self.0.started.set(true);
        let weak = self.weak();
        gio::DBusProxy::for_bus(gio::BusType::System, gio::DBusProxyFlags::DO_NOT_AUTO_START, None, BUS_NAME,
                                MANAGER_PATH, MANAGER, Some(&self.0.cancel), move |result| {
            let Some(this) = Self::upgrade(&weak) else { return };
            match result {
                Err(error) => this.failed_error(&error),
                Ok(manager) => this.manager_ready(manager),
            }
        });
    }

    fn manager_ready(&self, manager: gio::DBusProxy) {
        if self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        let signal = manager.connect_local("g-signal", false, move |values| {
            if let Some(this) = Self::upgrade(&weak) {
                let name = values.get(2).and_then(|v| v.get::<String>().ok()).unwrap_or_default();
                let parameters = values.get(3).and_then(|v| v.get::<glib::Variant>().ok());
                this.manager_signal(&name, parameters.as_ref());
            }
            None
        });
        let weak = self.weak();
        let owner = manager.connect_notify_local(Some("g-name-owner"), move |_, _| {
            if let Some(this) = Self::upgrade(&weak) {
                if !this.0.closed.get() {
                    this.subscribe();
                }
            }
        });
        self.0.manager_handlers.replace(vec![signal, owner]);
        self.0.manager.replace(Some(manager));
        self.subscribe();
    }

    fn subscribe(&self) {
        let Some(manager) = self.0.manager.borrow().clone() else { return };
        if manager.name_owner().is_none() {
            self.failed_text("The system service manager is unavailable.");
            return;
        }
        let weak = self.weak();
        manager.call("Subscribe", None, gio::DBusCallFlags::NONE, 5000, Some(&self.0.cancel), move |result| {
            let Some(this) = Self::upgrade(&weak) else { return };
            if let Err(error) = result {
                if !error.message().contains("AlreadySubscribed") {
                    this.failed_error(&error);
                    return;
                }
            }
            if !this.0.closed.get() {
                this.refresh();
            }
        });
    }

    fn manager_signal(&self, name: &str, parameters: Option<&glib::Variant>) {
        let first_string = || parameters.and_then(|p| p.try_child_value(0)).and_then(|v| v.str().map(String::from));
        let relevant = match name {
            "UnitNew" | "UnitRemoved" => first_string().is_some_and(|unit| UNITS.contains(&unit.as_str())),
            "UnitFilesChanged" => true,
            "Reloading" => parameters.and_then(|p| p.try_child_value(0)).and_then(|v| v.get::<bool>()) == Some(false),
            _ => false,
        };
        if relevant && self.0.pending.borrow().is_empty() {
            self.refresh();
        }
    }

    fn failed_error(&self, error: &glib::Error) { self.set_failed(friendly_error(error)) }

    fn failed_text(&self, text: &str) { self.set_failed(friendly_text(text)) }

    /// Python `_failed(error)` with the friendly text already computed.
    fn set_failed(&self, text: String) {
        if !self.0.closed.get() {
            self.0.error.replace(if text.is_empty() { "The discovery status could not be read.".into() } else { text });
            self.0.ready.set(true);
            self.changed();
        }
    }

    pub fn refresh(&self) {
        let Some(manager) = self.0.manager.borrow().clone() else { return };
        if self.0.closed.get() {
            return;
        }
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        self.0.pending.replace(UNITS.iter().map(|u| u.to_string()).collect());
        self.0.ready.set(false);
        self.0.error.replace(String::new());
        self.0.units.replace(Map::new());
        for (proxy, handler) in self.0.unit_handlers.take() {
            proxy.disconnect(handler);
        }
        self.0.proxies.replace(Vec::new());
        for unit in UNITS {
            let weak = self.weak();
            let connection = manager.connection();
            manager.call("LoadUnit", Some(&(unit,).to_variant()), gio::DBusCallFlags::NONE, 5000, Some(&self.0.cancel),
                         move |result| {
                let Some(this) = Self::upgrade(&weak) else { return };
                let current = || !this.0.closed.get() && generation == this.0.generation.get();
                let path = match result.map(|reply| reply.try_child_value(0).and_then(|v| v.get::<glib::variant::ObjectPath>())) {
                    Ok(Some(path)) => path,
                    Ok(None) => {
                        if current() {
                            this.0.error.replace("Unexpected reply from the system service manager.".into());
                            this.finished_unit(unit);
                        }
                        return;
                    }
                    Err(error) => {
                        if current() {
                            let message = error.message();
                            if message.contains("NoSuchUnit") || message.contains("LoadFailed") {
                                let mut info = Map::new();
                                info.insert("LoadState".into(), "not-found".into());
                                info.insert("ActiveState".into(), "inactive".into());
                                this.0.units.borrow_mut().insert(unit.into(), Value::Object(info));
                            } else {
                                this.0.error.replace(friendly_error(&error));
                            }
                            this.finished_unit(unit);
                        }
                        return;
                    }
                };
                if !current() {
                    return;
                }
                let weak = this.weak();
                gio::DBusProxy::new(&connection, gio::DBusProxyFlags::DO_NOT_AUTO_START, None, Some(BUS_NAME), path.as_str(),
                                    UNIT_INTERFACE, Some(&this.0.cancel), move |result| {
                    let Some(this) = Self::upgrade(&weak) else { return };
                    let current = !this.0.closed.get() && generation == this.0.generation.get();
                    let proxy = match result {
                        Err(error) => {
                            if current {
                                this.0.error.replace(friendly_error(&error));
                                this.finished_unit(unit);
                            }
                            return;
                        }
                        Ok(proxy) => proxy,
                    };
                    if !current {
                        return;
                    }
                    this.0.proxies.borrow_mut().push((unit.into(), proxy.clone()));
                    let weak = this.weak();
                    let handler = proxy.connect_local("g-properties-changed", false, move |values| {
                        if let (Some(this), Some(proxy)) = (Self::upgrade(&weak), values.first().and_then(|v| v.get::<gio::DBusProxy>().ok())) {
                            this.read_unit(unit, &proxy);
                            this.changed();
                        }
                        None
                    });
                    this.0.unit_handlers.borrow_mut().push((proxy.clone(), handler));
                    this.read_unit(unit, &proxy);
                    this.finished_unit(unit);
                });
            });
        }
    }

    fn read_unit(&self, unit: &str, proxy: &gio::DBusProxy) {
        let mut info = Map::new();
        for key in ["LoadState", "ActiveState", "UnitFileState"] {
            let value = proxy.cached_property(key).map(|v| variant_json(&v)).unwrap_or_else(|| "".into());
            info.insert(key.into(), value);
        }
        self.0.units.borrow_mut().insert(unit.into(), Value::Object(info));
    }

    fn finished_unit(&self, unit: &str) {
        let empty = {
            let mut pending = self.0.pending.borrow_mut();
            pending.remove(unit);
            pending.is_empty()
        };
        if empty {
            self.0.ready.set(true);
            self.changed();
        }
    }

    /// Run the privileged change; false when refused (closed, busy, not ready
    /// or not installed). `done(success, error)` follows completion.
    pub fn set_enabled(&self, enabled: bool, done: Done) -> bool {
        let state = self.snapshot();
        if self.0.closed.get() || self.0.busy.get() || state["ready"] != Value::Bool(true) || state["installed"] != Value::Bool(true) {
            return false;
        }
        self.0.error.replace(String::new());
        let process = match (self.0.spawner)(&change_command(enabled)) {
            Ok(process) => process,
            Err(error) => {
                let text = friendly_error(&error);
                let text = if text.is_empty() { "The discovery setting could not be changed.".to_owned() } else { text };
                self.0.error.replace(text.clone());
                self.changed();
                done(false, text);
                return true;
            }
        };
        self.0.process.replace(Some(process.clone()));
        self.0.busy.set(true);
        self.changed();
        let weak = self.weak();
        let finished_process = process.clone();
        process.communicate_utf8_async(None, None::<&gio::Cancellable>, move |result| {
            let process = finished_process;
            let (success, error) = match result {
                Ok((_, stderr)) => {
                    let success = process.is_successful();
                    // Cancellation is an ordinary outcome, not an alarming error.
                    let cancelled = process.has_exited() && matches!(process.exit_status(), 126 | 127);
                    let error = if success || cancelled {
                        String::new()
                    } else {
                        let text = friendly_text(stderr.as_deref().unwrap_or("").trim());
                        if text.is_empty() { "The discovery setting could not be changed.".into() } else { text }
                    };
                    (success, error)
                }
                Err(error) => (false, friendly_error(&error)),
            };
            let Some(this) = Self::upgrade(&weak) else { return };
            this.0.process.replace(None);
            this.0.busy.set(false);
            if !this.0.closed.get() {
                this.refresh();
                done(success, error);
                this.changed();
            }
        });
        true
    }

    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        self.0.cancel.cancel();
        if let Some(manager) = self.0.manager.borrow().clone() {
            for handler in self.0.manager_handlers.take() {
                manager.disconnect(handler);
            }
        }
        for (proxy, handler) in self.0.unit_handlers.take() {
            proxy.disconnect(handler);
        }
        if let Some(manager) = self.0.manager.borrow().clone() {
            if manager.name_owner().is_some() {
                manager.call("Unsubscribe", None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>, |_| {});
            }
        }
        self.0.proxies.replace(Vec::new());
    }
}

/// `Variant.unpack()` for the string/bool/number properties systemd exposes.
fn variant_json(value: &glib::Variant) -> Value {
    if let Some(text) = value.str() {
        return text.into();
    }
    if let Some(flag) = value.get::<bool>() {
        return flag.into();
    }
    if let Some(number) = value.get::<u64>().or_else(|| value.get::<u32>().map(u64::from)) {
        return number.into();
    }
    if let Some(number) = value.get::<i64>().or_else(|| value.get::<i32>().map(i64::from)) {
        return number.into();
    }
    value.print(false).to_string().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn units(value: Value) -> Map<String, Value> { value.as_object().unwrap().clone() }

    #[test]
    fn either_socket_or_service_can_enable_discovery() {
        for active in [SOCKET, SERVICE] {
            let mut map = Map::new();
            for name in [SOCKET, SERVICE] {
                map.insert(name.into(), json!({"LoadState": "loaded", "ActiveState": if name == active { "active" } else { "inactive" }}));
            }
            assert_eq!(summarize(&map, true, "", false)["enabled"], true);
        }
        let state = summarize(&units(json!({SOCKET: {"LoadState": "loaded", "ActiveState": "inactive"}})), true, "", false);
        assert_eq!(state["enabled"], false);
        assert_eq!(state["installed"], true);
        assert_eq!(state["class"], "inactive");
    }

    #[test]
    fn missing_and_unreadable_are_distinct() {
        let missing = summarize(&units(json!({SOCKET: {"LoadState": "not-found"}})), true, "", false);
        assert_eq!(missing["ready"], true);
        assert_eq!(missing["installed"], false);
        assert_eq!(missing["tooltip"], "Discover Network Devices: Avahi is not installed\nClick for discovery controls");
        let unavailable = summarize(&Map::new(), true, "System bus unavailable", false);
        assert_eq!(unavailable["ready"], false);
        assert!(unavailable["tooltip"].as_str().unwrap().contains("System bus unavailable"));
        assert_eq!(summarize(&Map::new(), false, "", false)["tooltip"], "Discover Network Devices: Checking…\nClick for discovery controls");
        assert_eq!(summarize(&Map::new(), false, "", true)["class"], "pending");
    }

    #[test]
    fn snapshot_json_matches_python() {
        let state = summarize(&units(json!({SOCKET: {"LoadState": "loaded", "ActiveState": "active", "UnitFileState": "enabled"}})),
                              true, "", false);
        assert_eq!(snapshot_text(&state),
                   "{\"text\": \"\\uf1e0\", \"tooltip\": \"Discover Network Devices: Enabled\\nClick for discovery controls\", \"class\": \"active\", \"ready\": true, \"installed\": true, \"enabled\": true, \"busy\": false, \"error\": \"\", \"units\": {\"avahi-daemon.socket\": {\"LoadState\": \"loaded\", \"ActiveState\": \"active\", \"UnitFileState\": \"enabled\"}}}");
    }

    #[test]
    fn commands_preserve_original_persistent_enable_and_disable_behavior() {
        assert_eq!(change_command(true), ["/usr/bin/pkexec", "/usr/bin/systemctl", "enable", "--now", SOCKET]);
        assert_eq!(change_command(false), ["/usr/bin/pkexec", "/usr/bin/systemctl", "disable", "--now", SOCKET, SERVICE]);
        for value in [json!("true"), json!("avahi;exit"), json!(1), json!(null)] {
            assert!(change_command_value(&value).is_err());
        }
        assert_eq!(change_command_value(&json!(true)).unwrap(), change_command(true));
    }

    #[test]
    fn errors_are_friendly_without_quarks_or_dbus_names() {
        assert_eq!(friendly_text("GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: Permission denied"),
                   "The system did not allow this change.");
        assert_eq!(friendly_text("GDBus.Error:org.freedesktop.systemd1.NoSuchUnit: Unit x not loaded."), "Avahi is not installed.");
        assert_eq!(friendly_text("GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: gone"),
                   "The system service manager is unavailable.");
        assert_eq!(friendly_text("Timeout was reached"), "The system service manager did not respond in time.");
        let cancelled = glib::Error::new(gio::IOErrorEnum::Cancelled, "Operation was cancelled");
        assert_eq!(friendly_error(&cancelled), "");
        assert_eq!(friendly_text("Request dismissed."), "");
        assert_eq!(friendly_text("GDBus.Error:org.example.Odd: Something odd happened"), "Something odd happened");
        assert_eq!(friendly_text("g-io-error-quark: first\nGDBus.Error:org.example.Odd: last line (5)"), "last line");
        assert_eq!(friendly_text(""), "");
        let backend = DiscoveryBackend::new(|| {});
        backend.failed_text("GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: Permission denied");
        assert!(!backend.snapshot()["tooltip"].as_str().unwrap().contains("GDBus"));
        backend.failed_text("");
        assert_eq!(backend.error(), "The discovery status could not be read.");
    }
}
