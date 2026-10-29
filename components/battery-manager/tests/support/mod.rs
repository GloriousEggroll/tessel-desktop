//! Private dbus-daemon plus fake UPower / KDE KAuth services for tests.
//! Never touches the real session or system bus.

#![allow(dead_code)]

use gio::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("bm{}{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct PrivateBus {
    pub address: String,
    daemon: Child,
    pub dir: PathBuf,
}

impl PrivateBus {
    pub fn start(name: &str) -> Self {
        let dir = scratch(name);
        let config = std::fs::read_to_string("/usr/share/dbus-1/session.conf").expect("session.conf");
        let mut out = String::new();
        for line in config.lines() {
            let trimmed = line.trim();
            // No activation of real services on the private bus.
            if trimmed.starts_with("<standard_session_servicedirs") || trimmed.starts_with("<servicedir")
                || trimmed.starts_with("<includedir") || trimmed.starts_with("<include ") {
                continue;
            }
            if trimmed.starts_with("<listen>") {
                out.push_str(&format!("  <listen>unix:tmpdir={}</listen>\n", dir.display()));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        let path = dir.join("d.conf");
        std::fs::write(&path, out).unwrap();
        let mut daemon = Command::new("dbus-daemon").arg(format!("--config-file={}", path.display()))
            .args(["--nofork", "--print-address=1"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("dbus-daemon");
        let mut line = String::new();
        BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let address = line.trim().to_owned();
        assert!(address.starts_with("unix:"), "private bus address {address:?}");
        Self { address, daemon, dir }
    }

    pub fn connect(&self) -> gio::DBusConnection {
        gio::DBusConnection::for_address_sync(&self.address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None, None::<&gio::Cancellable>).expect("private bus connection")
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Iterate the default main context until `predicate` holds (or `timeout`).
pub fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let context = glib::MainContext::default();
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    predicate()
}

pub fn settle(duration: Duration) {
    wait_until(duration, || false);
}

pub fn request_name(connection: &gio::DBusConnection, name: &str) {
    let reply = connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).expect("RequestName");
    assert_eq!(reply.get::<(u32,)>(), Some((1,)), "own {name}");
}

pub fn release_name(connection: &gio::DBusConnection, name: &str) {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
        Some(&(name,).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).expect("ReleaseName");
}

const DEVICE_PROPERTIES: &[(&str, &str)] = &[
    ("NativePath", "s"), ("Vendor", "s"), ("Model", "s"), ("Serial", "s"), ("Type", "u"), ("PowerSupply", "b"),
    ("IsPresent", "b"), ("Percentage", "d"), ("Energy", "d"), ("EnergyFull", "d"), ("State", "u"), ("BatteryLevel", "u"),
    ("IconName", "s"), ("IsRechargeable", "b"), ("TimeToEmpty", "x"), ("TimeToFull", "x"), ("Capacity", "d"),
    ("ChargeCycles", "i"), ("WarningLevel", "u"), ("ChargeThresholdSupported", "b"), ("ChargeThresholdEnabled", "b"),
    ("ChargeStartThreshold", "u"), ("ChargeEndThreshold", "u"), ("ChargeThresholdSettingsSupported", "u"),
];

fn interface(xml: &str, name: &str) -> gio::DBusInterfaceInfo {
    gio::DBusNodeInfo::for_xml(xml).unwrap().lookup_interface(name).unwrap()
}

fn device_xml() -> String {
    let properties: String = DEVICE_PROPERTIES.iter()
        .map(|(name, kind)| format!("<property name='{name}' type='{kind}' access='read'/>")).collect();
    format!("<node><interface name='org.freedesktop.UPower.Device'>
      <method name='EnableChargeThreshold'><arg type='b' direction='in'/></method>{properties}</interface></node>")
}

const MANAGER_XML: &str = "<node><interface name='org.freedesktop.UPower'>
  <method name='EnumerateDevices'><arg type='ao' direction='out'/></method>
  <signal name='DeviceAdded'><arg type='o'/></signal><signal name='DeviceRemoved'><arg type='o'/></signal>
  <property name='OnBattery' type='b' access='read'/></interface></node>";

fn default_for(kind: &str) -> glib::Variant {
    match kind {
        "s" => "".to_variant(),
        "u" => 0u32.to_variant(),
        "b" => false.to_variant(),
        "d" => 0.0f64.to_variant(),
        "x" => 0i64.to_variant(),
        "i" => 0i32.to_variant(),
        _ => unreachable!(),
    }
}

pub type Device = BTreeMap<String, glib::Variant>;

#[derive(Default)]
pub struct UPowerState {
    pub on_battery: bool,
    pub devices: BTreeMap<String, (Device, Option<gio::RegistrationId>)>,
    pub enable_calls: Vec<(String, bool, bool)>,
}

/// A fake `org.freedesktop.UPower` on its own private-bus connection.
pub struct FakeUPower {
    pub connection: gio::DBusConnection,
    pub state: Rc<RefCell<UPowerState>>,
}

impl FakeUPower {
    pub fn start(bus: &PrivateBus, on_battery: bool) -> Self {
        Self::from_connection(bus.connect(), on_battery)
    }

    pub fn from_connection(connection: gio::DBusConnection, on_battery: bool) -> Self {
        let state = Rc::new(RefCell::new(UPowerState { on_battery, ..Default::default() }));
        let (methods, properties) = (state.clone(), state.clone());
        connection.register_object("/org/freedesktop/UPower", &interface(MANAGER_XML, "org.freedesktop.UPower"))
            .method_call(move |_, _, _, _, method, _, invocation| {
                assert_eq!(method, "EnumerateDevices");
                let paths: Vec<glib::Variant> = methods.borrow().devices.keys()
                    .map(|p| glib::variant::ObjectPath::try_from(p.as_str()).unwrap().to_variant()).collect();
                let list = glib::Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, paths);
                invocation.return_value(Some(&glib::Variant::tuple_from_iter([list])));
            })
            .property(move |_, _, _, _, _| properties.borrow().on_battery.to_variant())
            .build().unwrap();
        request_name(&connection, "org.freedesktop.UPower");
        Self { connection, state }
    }

    pub fn add_device(&self, path: &str, values: &[(&str, glib::Variant)], announce: bool) {
        let mut device: Device = DEVICE_PROPERTIES.iter().map(|(name, kind)| (name.to_string(), default_for(kind))).collect();
        device.insert("BatteryLevel".into(), 1u32.to_variant());
        device.insert("IsPresent".into(), true.to_variant());
        for (name, value) in values {
            device.insert(name.to_string(), value.clone());
        }
        let (methods, properties, device_path) = (self.state.clone(), self.state.clone(), path.to_owned());
        let id = self.connection.register_object(path, &interface(&device_xml(), "org.freedesktop.UPower.Device"))
            .method_call(move |connection, _, path, _, method, params, invocation| {
                assert_eq!(method, "EnableChargeThreshold");
                let (enabled,) = params.get::<(bool,)>().unwrap();
                let interactive = invocation.message().flags().contains(gio::DBusMessageFlags::ALLOW_INTERACTIVE_AUTHORIZATION);
                {
                    let mut state = methods.borrow_mut();
                    state.enable_calls.push((path.to_owned(), enabled, interactive));
                    if let Some((device, _)) = state.devices.get_mut(path) {
                        device.insert("ChargeThresholdEnabled".into(), enabled.to_variant());
                    }
                }
                let _ = connection;
                invocation.return_value(None);
            })
            .property(move |_, _, _, _, name| {
                properties.borrow().devices.get(&device_path).and_then(|(d, _)| d.get(name).cloned())
                    .unwrap_or_else(|| false.to_variant())
            })
            .build().unwrap();
        self.state.borrow_mut().devices.insert(path.to_owned(), (device, Some(id)));
        if announce {
            self.emit("DeviceAdded", path);
        }
    }

    fn emit(&self, signal: &str, path: &str) {
        let object = glib::Variant::tuple_from_iter([glib::variant::ObjectPath::try_from(path).unwrap().to_variant()]);
        self.connection.emit_signal(None, "/org/freedesktop/UPower", "org.freedesktop.UPower", signal, Some(&object)).unwrap();
    }

    pub fn remove_device(&self, path: &str) {
        let removed = self.state.borrow_mut().devices.remove(path);
        if let Some((_, Some(id))) = removed {
            self.connection.unregister_object(id).unwrap();
        }
        self.emit("DeviceRemoved", path);
    }

    pub fn set_device(&self, path: &str, name: &str, value: glib::Variant) {
        if let Some((device, _)) = self.state.borrow_mut().devices.get_mut(path) {
            device.insert(name.into(), value.clone());
        }
        let changed = glib::VariantDict::new(None);
        changed.insert_value(name, &value);
        let params = glib::Variant::tuple_from_iter(["org.freedesktop.UPower.Device".to_variant(), changed.end(), Vec::<String>::new().to_variant()]);
        self.connection.emit_signal(None, path, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&params)).unwrap();
    }

    pub fn set_on_battery(&self, on_battery: bool) {
        self.state.borrow_mut().on_battery = on_battery;
        let changed = glib::VariantDict::new(None);
        changed.insert_value("OnBattery", &on_battery.to_variant());
        let params = glib::Variant::tuple_from_iter(["org.freedesktop.UPower".to_variant(), changed.end(), Vec::<String>::new().to_variant()]);
        self.connection.emit_signal(None, "/org/freedesktop/UPower", "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&params)).unwrap();
    }
}

/// Recorded `performAction` calls: (action, arguments blob, signature, interactive).
pub type KAuthCalls = Rc<RefCell<Vec<(String, Vec<u8>, String, bool)>>>;

const KAUTH_XML: &str = "<node><interface name='org.kde.kf6auth'><method name='performAction'>
  <arg type='s' direction='in'/><arg type='ay' direction='in'/><arg type='a{sv}' direction='in'/>
  <arg type='ay' direction='in'/><arg type='a{sh}' direction='in'/><arg type='ay' direction='out'/></method></interface></node>";

/// A fake KDE charge-threshold helper. `apply(arguments blob)` simulates the
/// helper writing the *fixture* sysfs files; `reply()` builds the KAuth reply.
pub fn fake_kauth(bus: &PrivateBus, reply: impl Fn(&str, &[u8]) -> Vec<u8> + 'static) -> (gio::DBusConnection, KAuthCalls) {
    let connection = bus.connect();
    let calls: KAuthCalls = Rc::new(RefCell::new(Vec::new()));
    let record = calls.clone();
    connection.register_object("/", &interface(KAUTH_XML, "org.kde.kf6auth"))
        .method_call(move |_, _, _, _, _, params, invocation| {
            let action = params.child_value(0).str().unwrap().to_owned();
            let blob = params.child_value(3).fixed_array::<u8>().unwrap().to_vec();
            let interactive = invocation.message().flags().contains(gio::DBusMessageFlags::ALLOW_INTERACTIVE_AUTHORIZATION);
            record.borrow_mut().push((action.clone(), blob.clone(), params.type_().as_str().to_owned(), interactive));
            let out = reply(&action, &blob);
            invocation.return_value(Some(&glib::Variant::tuple_from_iter([glib::Variant::array_from_fixed_array(&out)])));
        })
        .build().unwrap();
    request_name(&connection, "org.kde.powerdevil.chargethresholdhelper");
    (connection, calls)
}
