//! The discovery backend against a FAKE systemd on a private dbus-daemon
//! (gio::TestDBus, also exported as the "system" bus). No real Avahi or
//! systemd unit is ever queried or changed; the privileged command is a
//! recording shell script, never pkexec.

use gio::prelude::*;
use network_discovery::backend::{change_command, snapshot_text, DiscoveryBackend, Spawner, SERVICE, SOCKET};
use network_discovery::browse::ServiceBrowser;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(predicate: impl Fn() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.01);
        if predicate() {
            return true;
        }
    }
    false
}

const MANAGER_XML: &str = r#"<node><interface name="org.freedesktop.systemd1.Manager">
  <method name="Subscribe"/><method name="Unsubscribe"/>
  <method name="LoadUnit"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
  <signal name="UnitNew"><arg type="s"/><arg type="o"/></signal>
  <signal name="UnitRemoved"><arg type="s"/><arg type="o"/></signal>
  <signal name="UnitFilesChanged"/>
  <signal name="Reloading"><arg type="b"/></signal>
</interface></node>"#;

const UNIT_XML: &str = r#"<node><interface name="org.freedesktop.systemd1.Unit">
  <property name="LoadState" type="s" access="read"/>
  <property name="ActiveState" type="s" access="read"/>
  <property name="UnitFileState" type="s" access="read"/>
</interface></node>"#;

type UnitState = (String, String, String);

/// `org.freedesktop.systemd1` on its own private-bus connection.
struct FakeSystemd {
    connection: gio::DBusConnection,
    units: Rc<RefCell<HashMap<String, Option<UnitState>>>>,
    calls: Rc<RefCell<Vec<String>>>,
    load_error: Rc<RefCell<Option<(String, String)>>>,
    owner: RefCell<Option<gio::OwnerId>>,
    owned: Rc<Cell<bool>>,
}

fn unit_path(unit: &str) -> String {
    let mut path = String::from("/org/freedesktop/systemd1/unit/");
    for byte in unit.bytes() {
        if byte.is_ascii_alphanumeric() { path.push(byte as char) } else { path.push_str(&format!("_{byte:02x}")) }
    }
    path
}

fn object_path(path: &str) -> glib::variant::ObjectPath { glib::variant::ObjectPath::try_from(path.to_owned()).unwrap() }

impl FakeSystemd {
    fn new(address: &str) -> Self {
        let connection = gio::DBusConnection::for_address_sync(address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None::<&gio::DBusAuthObserver>, None::<&gio::Cancellable>).unwrap();
        let units: Rc<RefCell<HashMap<String, Option<UnitState>>>> = Rc::default();
        let calls: Rc<RefCell<Vec<String>>> = Rc::default();
        let load_error: Rc<RefCell<Option<(String, String)>>> = Rc::default();
        let manager = gio::DBusNodeInfo::for_xml(MANAGER_XML).unwrap();
        let interface = manager.lookup_interface("org.freedesktop.systemd1.Manager").unwrap();
        let (u, c, e) = (units.clone(), calls.clone(), load_error.clone());
        let id = connection.register_object("/org/freedesktop/systemd1", &interface)
            .method_call(move |_, _, _, _, method, params, invocation| {
                c.borrow_mut().push(method.to_owned());
                if method != "LoadUnit" {
                    invocation.return_value(None);
                    return;
                }
                let (unit,) = params.get::<(String,)>().unwrap();
                if let Some((name, message)) = e.borrow().clone() {
                    invocation.return_dbus_error(&name, &message);
                } else if u.borrow().get(&unit).cloned().flatten().is_some() {
                    invocation.return_value(Some(&(object_path(&unit_path(&unit)),).to_variant()));
                } else {
                    invocation.return_dbus_error("org.freedesktop.systemd1.NoSuchUnit", &format!("Unit {unit} not found."));
                }
            }).build().unwrap();
        std::mem::forget(id);
        let unit_info = gio::DBusNodeInfo::for_xml(UNIT_XML).unwrap();
        let interface = unit_info.lookup_interface("org.freedesktop.systemd1.Unit").unwrap();
        for unit in [SOCKET, SERVICE] {
            let (u, name) = (units.clone(), unit.to_owned());
            let id = connection.register_object(&unit_path(unit), &interface)
                .property(move |_, _, _, _, property| {
                    let state = u.borrow().get(&name).cloned().flatten().unwrap_or_default();
                    match property {
                        "LoadState" => state.0.to_variant(),
                        "ActiveState" => state.1.to_variant(),
                        _ => state.2.to_variant(),
                    }
                }).build().unwrap();
            std::mem::forget(id);
        }
        let fake = Self { connection, units, calls, load_error, owner: RefCell::new(None), owned: Rc::new(Cell::new(false)) };
        fake.own();
        fake
    }

    fn own(&self) {
        let (acquired, lost) = (self.owned.clone(), self.owned.clone());
        let id = gio::bus_own_name_on_connection(&self.connection, "org.freedesktop.systemd1", gio::BusNameOwnerFlags::NONE,
                                                 move |_, _| acquired.set(true), move |_, _| lost.set(false));
        self.owner.replace(Some(id));
        assert!(wait_for(|| self.owned.get(), 3.0));
    }

    fn disown(&self) {
        if let Some(id) = self.owner.take() {
            gio::bus_unown_name(id);
        }
        self.owned.set(false);
    }

    fn set(&self, unit: &str, state: Option<(&str, &str, &str)>) {
        self.units.borrow_mut().insert(unit.into(), state.map(|(a, b, c)| (a.into(), b.into(), c.into())));
    }

    /// Change ActiveState and emit PropertiesChanged like systemd does.
    fn activate(&self, unit: &str, active: &str) {
        if let Some(Some(state)) = self.units.borrow_mut().get_mut(unit) {
            state.1 = active.into();
        }
        let changed: HashMap<String, glib::Variant> = [("ActiveState".to_owned(), active.to_variant())].into();
        let parameters = ("org.freedesktop.systemd1.Unit", changed, Vec::<String>::new()).to_variant();
        self.connection.emit_signal(None, &unit_path(unit), "org.freedesktop.DBus.Properties", "PropertiesChanged",
                                    Some(&parameters)).unwrap();
    }

    fn signal(&self, name: &str, parameters: Option<glib::Variant>) {
        self.connection.emit_signal(None, "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager", name,
                                    parameters.as_ref()).unwrap();
    }

    fn count(&self, method: &str) -> usize { self.calls.borrow().iter().filter(|c| *c == method).count() }
}

struct Recorder {
    backend: DiscoveryBackend,
    changes: Rc<Cell<usize>>,
}

fn backend_with(spawner: Option<Spawner>) -> Recorder {
    let changes = Rc::new(Cell::new(0));
    let counter = changes.clone();
    let changed = move || counter.set(counter.get() + 1);
    let backend = match spawner {
        Some(spawner) => DiscoveryBackend::with_spawner(changed, spawner),
        None => DiscoveryBackend::new(changed),
    };
    Recorder { backend, changes }
}

fn units_are_observed_through_systemd_signals(fake: &FakeSystemd) {
    fake.set(SOCKET, Some(("loaded", "inactive", "disabled")));
    fake.set(SERVICE, Some(("loaded", "inactive", "indirect")));
    let r = backend_with(None);
    r.backend.start();
    assert!(wait_for(|| r.backend.ready(), 3.0), "backend never became ready");
    let state = r.backend.snapshot();
    assert_eq!((state["ready"].clone(), state["installed"].clone(), state["enabled"].clone(), state["class"].clone()),
               (Value::Bool(true), Value::Bool(true), Value::Bool(false), Value::from("inactive")));
    assert_eq!(state["units"][SOCKET]["UnitFileState"], "disabled");
    assert_eq!(fake.count("Subscribe"), 1);
    assert_eq!(fake.count("LoadUnit"), 2);
    // PropertiesChanged updates without another LoadUnit.
    let before = r.changes.get();
    fake.activate(SERVICE, "active");

    let ok = wait_for(|| r.backend.snapshot()["enabled"] == true, 3.0);
    assert!(ok, "{:?} changes {}", r.backend.snapshot(), r.changes.get() - before);
    assert!(r.changes.get() > before);
    assert_eq!(fake.count("LoadUnit"), 2);
    // Relevant manager signals refresh; unrelated ones do not.
    fake.signal("UnitNew", Some(("other.service", object_path("/x")).to_variant()));
    fake.signal("Reloading", Some((true,).to_variant()));
    settle(0.2);
    assert_eq!(fake.count("LoadUnit"), 2);
    fake.signal("UnitFilesChanged", None);
    assert!(wait_for(|| fake.count("LoadUnit") == 4, 3.0));
    assert!(wait_for(|| r.backend.ready(), 3.0));
    fake.signal("Reloading", Some((false,).to_variant()));
    assert!(wait_for(|| fake.count("LoadUnit") == 6, 3.0));
    assert!(wait_for(|| r.backend.ready(), 3.0));
    fake.signal("UnitRemoved", Some((SOCKET, object_path("/x")).to_variant()));
    assert!(wait_for(|| fake.count("LoadUnit") == 8, 3.0));
    assert!(wait_for(|| r.backend.ready(), 3.0));
    // A refresh drops the old unit proxies: one PropertiesChanged = one update.
    let before = r.changes.get();
    fake.activate(SOCKET, "active");
    assert!(wait_for(|| r.changes.get() > before, 3.0));
    settle(0.2);
    assert_eq!(r.changes.get(), before + 1);
    r.backend.close();
    assert!(wait_for(|| fake.count("Unsubscribe") == 1, 3.0));
    // Closed: signals are ignored.
    fake.signal("UnitFilesChanged", None);
    settle(0.2);
    assert_eq!(fake.count("LoadUnit"), 8);
    println!("ok units_are_observed_through_systemd_signals");
}

fn missing_avahi_and_errors(fake: &FakeSystemd) {
    fake.set(SOCKET, None);
    fake.set(SERVICE, None);
    let r = backend_with(None);
    r.backend.start();
    assert!(wait_for(|| r.backend.ready(), 3.0));
    let state = r.backend.snapshot();
    assert_eq!((state["ready"].clone(), state["installed"].clone(), state["class"].clone()),
               (Value::Bool(true), Value::Bool(false), Value::from("unavailable")));
    assert_eq!(state["units"][SOCKET], serde_json::json!({"LoadState": "not-found", "ActiveState": "inactive"}));
    r.backend.close();
    fake.load_error.replace(Some(("org.freedesktop.DBus.Error.AccessDenied".into(), "Permission denied".into())));
    let r = backend_with(None);
    r.backend.start();
    assert!(wait_for(|| r.backend.ready(), 3.0));
    let state = r.backend.snapshot();
    assert_eq!(state["error"], "The system did not allow this change.");
    assert_eq!(state["ready"], false);
    assert!(!state["tooltip"].as_str().unwrap().contains("GDBus"));
    r.backend.close();
    fake.load_error.replace(None);
    println!("ok missing_avahi_and_errors");
}

fn service_manager_restart_resubscribes(fake: &FakeSystemd) {
    fake.set(SOCKET, Some(("loaded", "active", "enabled")));
    fake.set(SERVICE, Some(("loaded", "active", "indirect")));
    let r = backend_with(None);
    r.backend.start();
    assert!(wait_for(|| r.backend.ready() && r.backend.snapshot()["enabled"] == true, 3.0));
    let subscribed = fake.count("Subscribe");
    fake.disown();
    assert!(wait_for(|| r.backend.error() == "The system service manager is unavailable.", 3.0));
    assert_eq!(r.backend.snapshot()["class"], "unavailable");
    fake.own();
    assert!(wait_for(|| fake.count("Subscribe") == subscribed + 1, 3.0), "no resubscribe after the owner returned");
    assert!(wait_for(|| r.backend.ready() && r.backend.error().is_empty(), 3.0));
    assert_eq!(r.backend.snapshot()["class"], "active");
    r.backend.close();
    println!("ok service_manager_restart_resubscribes");
}

/// A spawner that runs `sh` instead of pkexec, logging argv.
fn script_spawner(log: std::path::PathBuf, script: Rc<RefCell<String>>, spawned: Rc<Cell<usize>>) -> Spawner {
    Rc::new(move |argv: &[String]| {
        spawned.set(spawned.get() + 1);
        if script.borrow().is_empty() {
            return Err(glib::Error::new(glib::FileError::Noent, "Cannot start polkit"));
        }
        let body = format!("printf '%s\\n' \"$@\" > '{}'; {}", log.display(), script.borrow());
        let mut full: Vec<&OsStr> = vec![OsStr::new("sh"), OsStr::new("-c"), OsStr::new(&body), OsStr::new("sh")];
        full.extend(argv.iter().map(OsStr::new));
        gio::Subprocess::newv(&full, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE)
    })
}

type Outcome = Rc<RefCell<Option<(bool, String)>>>;

fn request(backend: &DiscoveryBackend, enabled: bool, result: &Outcome) -> bool {
    let slot = result.clone();
    backend.set_enabled(enabled, Box::new(move |ok, error| { slot.replace(Some((ok, error))); }))
}

fn outcome(backend: &DiscoveryBackend, result: &Outcome) -> (bool, String) {
    assert!(wait_for(|| result.borrow().is_some(), 5.0), "the change never finished");
    assert!(wait_for(|| backend.ready(), 3.0));
    result.take().unwrap()
}

fn privileged_changes_are_async_single_flight(fake: &FakeSystemd, scratch: &std::path::Path) {
    fake.set(SOCKET, Some(("loaded", "active", "enabled")));
    fake.set(SERVICE, Some(("loaded", "inactive", "indirect")));
    let log = scratch.join("argv.log");
    let script = Rc::new(RefCell::new("exit 0".to_owned()));
    let spawned = Rc::new(Cell::new(0));
    let r = backend_with(Some(script_spawner(log.clone(), script.clone(), spawned.clone())));
    let result: Outcome = Rc::default();
    // Not ready: refused, nothing spawned.
    assert!(!request(&r.backend, true, &result));
    assert_eq!(spawned.get(), 0);
    r.backend.start();
    assert!(wait_for(|| r.backend.ready(), 3.0));
    assert!(request(&r.backend, false, &result));
    assert!(r.backend.busy());
    assert_eq!(r.backend.snapshot()["class"], "pending");
    assert_eq!(r.backend.snapshot()["tooltip"], "Discover Network Devices: Waiting for authentication…\nClick for discovery controls");
    assert!(!request(&r.backend, false, &Rc::default()), "repeated clicks do not overlap");
    assert_eq!(spawned.get(), 1);
    let loads = fake.count("LoadUnit");
    assert_eq!(outcome(&r.backend, &result), (true, String::new()));
    assert!(!r.backend.busy());
    let argv: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
    assert_eq!(argv, change_command(false));
    assert!(fake.count("LoadUnit") >= loads + 2, "a change always refreshes");
    // Authentication cancelled (126): not an error.
    *script.borrow_mut() = "echo 'Not authorized' >&2; exit 126".into();
    assert!(request(&r.backend, true, &result));
    assert_eq!(outcome(&r.backend, &result), (false, String::new()));
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().collect::<Vec<_>>(), change_command(true));
    // Real failures are friendly.
    *script.borrow_mut() = "echo 'Error: GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: no' >&2; exit 1".into();
    request(&r.backend, true, &result);
    assert_eq!(outcome(&r.backend, &result), (false, "The system did not allow this change.".into()));
    *script.borrow_mut() = "echo 'Failed to enable unit: odd thing happened (3)' >&2; exit 1".into();
    request(&r.backend, true, &result);
    assert_eq!(outcome(&r.backend, &result), (false, "Failed to enable unit: odd thing happened".into()));
    *script.borrow_mut() = "exit 1".into();
    request(&r.backend, true, &result);
    assert_eq!(outcome(&r.backend, &result), (false, "The discovery setting could not be changed.".into()));
    // Spawn failure: not busy, reported synchronously.
    script.borrow_mut().clear();
    assert!(request(&r.backend, true, &result));
    assert!(!r.backend.busy());
    let (ok, error) = result.take().unwrap();
    assert!(!ok && error.contains("Cannot start polkit"), "{error}");
    r.backend.close();
    // Not installed: refused.
    fake.set(SOCKET, None);
    fake.set(SERVICE, None);
    let r = backend_with(Some(script_spawner(log, script, spawned.clone())));
    r.backend.start();
    assert!(wait_for(|| r.backend.ready(), 3.0));
    let before = spawned.get();
    assert!(!request(&r.backend, true, &result));
    assert_eq!(spawned.get(), before);
    r.backend.close();
    println!("ok privileged_changes_are_async_single_flight");
}

fn browser_is_async_single_flight_and_reports_missing_tools() {
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    let output = "=;e;IPv4;Box;_ssh._tcp;local;box.local;192.0.2.9;22;";
    let browser = ServiceBrowser::with_command(Rc::new(move |_| counter.set(counter.get() + 1)),
        vec!["sh".into(), "-c".into(), format!("sleep 0.1; printf '%s\\n' '{output}'"), "--terminate".into()]);
    assert!(browser.start());
    assert!(!browser.start(), "single flight");
    assert!(browser.running());
    assert!(browser.command().iter().any(|a| a == "--terminate"));
    assert!(wait_for(|| calls.get() == 1, 5.0));
    assert!(!browser.running());
    assert_eq!(browser.devices()[0].services, ["Remote login (SSH)"]);
    assert_eq!(browser.devices()[0].address, "192.0.2.9");
    assert_eq!(browser.error(), "");
    browser.close();
    assert!(!browser.start(), "closed browsers never start");
    let missing = ServiceBrowser::with_command(Rc::new(|_| {}), vec!["/nonexistent/avahi-browse".into()]);
    assert!(!missing.start());
    assert_eq!(missing.error(), "Avahi browsing tools are not installed.");
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    let failing = ServiceBrowser::with_command(Rc::new(move |_| counter.set(counter.get() + 1)),
                                               vec!["sh".into(), "-c".into(), "exit 1".into()]);
    failing.start();
    assert!(wait_for(|| calls.get() == 1, 5.0));
    assert_eq!(failing.error(), "Avahi is not running. Turn on discovery to look for devices.");
    // Closing mid-run reports nothing.
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    let slow = ServiceBrowser::with_command(Rc::new(move |_| counter.set(counter.get() + 1)),
                                            vec!["sh".into(), "-c".into(), "sleep 5".into()]);
    slow.start();
    slow.close();
    settle(0.3);
    assert_eq!(calls.get(), 0);
    assert!(!slow.running());
    let real = ServiceBrowser::new(Rc::new(|_| {}));
    assert_eq!(real.command(), ["/usr/bin/avahi-browse", "--all", "--parsable", "--terminate", "--resolve"]);
    println!("ok browser_is_async_single_flight_and_reports_missing_tools");
}

fn status_command_prints_one_json_line(fake: &FakeSystemd, address: &str) {
    fake.set(SOCKET, Some(("loaded", "active", "enabled")));
    fake.set(SERVICE, Some(("loaded", "inactive", "indirect")));
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_network-discovery")).arg("--status")
        .env("DBUS_SYSTEM_BUS_ADDRESS", address).env("DBUS_SESSION_BUS_ADDRESS", address)
        .env("DESKTOP_SUITE_LOCALE_DIR", "/nonexistent").env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY")
        .stdout(std::process::Stdio::piped()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "--status did not finish");
        settle(0.01);
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    let state: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(state["class"], "active");
    let local = network_discovery::backend::summarize(state["units"].as_object().unwrap(), true, "", false);
    assert_eq!(text.trim_end(), snapshot_text(&local));
    println!("ok status_command_prints_one_json_line");
}

fn main() {
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    assert_eq!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(), address, "TestDBus did not take over the session bus");
    // SAFETY: single-threaded test binary, before any bus is used.
    unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    let scratch = std::env::temp_dir().join(format!("discovery-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let fake = FakeSystemd::new(&address);
    units_are_observed_through_systemd_signals(&fake);
    missing_avahi_and_errors(&fake);
    service_manager_restart_resubscribes(&fake);
    privileged_changes_are_async_single_flight(&fake, &scratch);
    browser_is_async_single_flight_and_reports_missing_tools();
    status_command_prints_one_json_line(&fake, &address);
    let _ = std::fs::remove_dir_all(&scratch);
    println!("private_bus: all passed");
    drop(fake);
    bus.down();
}
