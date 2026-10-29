//! Real gio transport against fake UPower / KAuth services on a private
//! dbus-daemon (never the session or system bus), plus the `battery-status`
//! and `battery-manager --check` binaries. Runs sequentially on the main
//! thread (harness = false) because the backend's timers use the default
//! GLib main context.

mod support;

use suite_power::backend::{Mode, Options};
use suite_core::bus::{Bus, BusError, Call, GioBus};
use suite_power::charge_limits::{self, encode_arguments, qstring};
use suite_core::scheduler::GlibScheduler;
use suite_power::{BatteryModel, PowerBackend, PowerSystem};
use gio::prelude::*;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;
use support::*;

const SECOND: Duration = Duration::from_secs(1);
const LAPTOP: &str = "/org/freedesktop/UPower/devices/battery_BAT0";
const MOUSE: &str = "/org/freedesktop/UPower/devices/mouse_dev_01";

fn laptop(upower: &FakeUPower, announce: bool) {
    upower.add_device(LAPTOP, &[("Type", 2u32.to_variant()), ("PowerSupply", true.to_variant()), ("NativePath", "BAT0".to_variant()),
        ("Model", "Laptop battery".to_variant()), ("Serial", "42".to_variant()), ("Percentage", 60.0f64.to_variant()),
        ("Energy", 30.0f64.to_variant()), ("EnergyFull", 50.0f64.to_variant()), ("State", 2u32.to_variant()),
        ("IconName", "battery-good-symbolic".to_variant()), ("Capacity", 91.0f64.to_variant()), ("ChargeCycles", 164i32.to_variant()),
        ("TimeToEmpty", 10860i64.to_variant()), ("ChargeThresholdSupported", true.to_variant()),
        ("ChargeStartThreshold", 75u32.to_variant()), ("ChargeEndThreshold", 80u32.to_variant()),
        ("ChargeThresholdSettingsSupported", 3u32.to_variant())], announce);
}

fn mouse(upower: &FakeUPower, announce: bool) {
    upower.add_device(MOUSE, &[("Type", 5u32.to_variant()), ("Vendor", "Logitech".to_variant()), ("Model", "MX Master".to_variant()),
        ("NativePath", "/org/bluez/hci0/dev_01".to_variant()), ("Percentage", 55.0f64.to_variant()), ("State", 2u32.to_variant())], announce);
}

fn private_connector(address: String) -> suite_power::backend::Connector {
    Box::new(move |done| {
        gio::DBusConnection::for_address(&address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None, None::<&gio::Cancellable>, move |result| match result {
                Ok(connection) => done(Ok(GioBus::new(connection) as Rc<dyn Bus>)),
                Err(error) => done(Err(BusError::from_glib(&error))),
            });
    })
}

fn backend(bus: &PrivateBus, inspect: suite_power::backend::Inspector) -> (PowerBackend, Rc<RefCell<usize>>) {
    let changed = Rc::new(RefCell::new(0));
    let count = changed.clone();
    let options = Options { mode: Mode::BatteriesOnly, connector: private_connector(bus.address.clone()),
                           scheduler: Rc::new(GlibScheduler::default()), inspect, ..Options::default() };
    (PowerBackend::new(move || *count.borrow_mut() += 1, |_| {}, options), changed)
}

fn no_limits(dir: &Path) -> suite_power::backend::Inspector {
    let root = dir.to_path_buf();
    Box::new(move |devices| charge_limits::inspect_limits(devices, &root, &[]))
}

fn upower_backend_follows_signals_and_owner_changes() {
    let bus = PrivateBus::start("a");
    let upower = FakeUPower::start(&bus, false);
    laptop(&upower, false);
    mouse(&upower, false);
    let empty = scratch("empty");
    let (backend, changed) = backend(&bus, no_limits(&empty));
    backend.start();
    assert!(wait_until(5 * SECOND, || backend.power_ready()), "backend ready");
    assert!(backend.power_source_known() && backend.has_battery() && !backend.on_battery());
    assert_eq!(backend.percentage(), Some(60.0));
    let batteries = backend.batteries();
    assert_eq!(batteries.len(), 2);
    assert_eq!(batteries[0]["name"], "Laptop battery");
    assert_eq!(batteries[0]["health"], 91.0);
    assert_eq!(batteries[0]["cycles"], 164);
    assert_eq!(batteries[0]["charge_end_threshold"], 80);
    assert_eq!(batteries[0]["charge_limit_adjustable"], false);
    assert_eq!(batteries[1]["name"], "Logitech MX Master");
    assert_eq!(batteries[1]["bluetooth"], true);
    assert!(*changed.borrow() > 0);

    upower.set_on_battery(true);
    assert!(wait_until(5 * SECOND, || backend.on_battery() && backend.power_source_known()), "OnBattery signal");
    upower.set_device(MOUSE, "Percentage", 40.0f64.to_variant());
    assert!(wait_until(5 * SECOND, || backend.batteries()[1]["percentage"] == 40.0), "device PropertiesChanged");
    upower.remove_device(MOUSE);
    assert!(wait_until(5 * SECOND, || backend.batteries().len() == 1), "DeviceRemoved");
    mouse(&upower, true);
    assert!(wait_until(5 * SECOND, || backend.batteries().len() == 2), "DeviceAdded");

    // UPower restarts: state invalidated at once, then re-read.
    release_name(&upower.connection, "org.freedesktop.UPower");
    assert!(wait_until(5 * SECOND, || !backend.power_ready()), "owner lost");
    assert!(backend.batteries().is_empty());
    request_name(&upower.connection, "org.freedesktop.UPower");
    assert!(wait_until(5 * SECOND, || backend.power_ready() && backend.batteries().len() == 2), "owner back");

    // UPower charge protection through EnableChargeThreshold, with read-back.
    assert!(BatteryModel::set_charge_thresholds(&backend, LAPTOP, true));
    assert!(wait_until(5 * SECOND, || !backend.busy()), "threshold write finished");
    assert_eq!(upower.state.borrow().enable_calls, [(LAPTOP.to_owned(), true, true)]);
    assert_eq!(backend.batteries()[0]["charge_threshold_enabled"], true);
    assert!(backend.error().is_empty(), "{}", backend.error());
    backend.close();
    println!("ok upower_backend_follows_signals_and_owner_changes");
}

fn reply_blob(code: u32, kind: u32) -> Vec<u8> {
    let mut blob = encode_arguments(&BTreeMap::new()).unwrap();
    blob.extend(code.to_be_bytes());
    blob.extend(kind.to_be_bytes());
    blob.extend(qstring(""));
    blob
}

/// The fixture "helper" writes the fixture sysfs (a temp dir), as KDE's helper would write /sys.
fn kauth_end_to_end_against_fixture_sysfs() {
    let bus = PrivateBus::start("k");
    let upower = FakeUPower::start(&bus, false);
    laptop(&upower, false);
    let root = scratch("sysfs");
    let supplies = root.join("power_supply");
    let battery = supplies.join("BAT0");
    std::fs::create_dir_all(&battery).unwrap();
    for (key, value) in [("type", "Battery"), ("scope", "System"), ("present", "1"), ("serial_number", "42"),
                         ("charge_control_start_threshold", "75"), ("charge_control_end_threshold", "80")] {
        std::fs::write(battery.join(key), value).unwrap();
    }
    let helper = root.join("helper");
    std::fs::write(&helper, "fixture").unwrap();
    let fixture = battery.clone();
    let (_kauth, calls) = fake_kauth(&bus, move |action, blob| {
        if action.ends_with(".setthreshold") {
            // Decode our own QVariantMap subset: count, then (QString key, type, null, i32).
            let map = charge_limits::decode_reply(&[blob, &reply_blob(0, 2)[4..]].concat()).unwrap();
            if let Some(start) = map.get("chargeStartThreshold") {
                std::fs::write(fixture.join("charge_control_start_threshold"), start.to_string()).unwrap();
            }
            std::fs::write(fixture.join("charge_control_end_threshold"), map["chargeStopThreshold"].to_string()).unwrap();
        }
        reply_blob(0, 2)
    });
    let inspect_root: PathBuf = supplies.clone();
    let helpers = vec![helper];
    let (backend, _) = backend(&bus, Box::new(move |devices| charge_limits::inspect_limits(devices, &inspect_root, &helpers)));
    backend.start();
    assert!(wait_until(5 * SECOND, || backend.power_ready()));
    let limit = backend.charge_limit();
    assert_eq!((limit["supported"].clone(), limit["maximum"].clone()), (json!(true), json!(80)));
    assert!(calls.borrow().is_empty(), "reads never activate the helper");
    assert!(backend.set_charge_limit(&json!(60)));
    assert!(wait_until(5 * SECOND, || !backend.busy()), "charge limit finished");
    assert!(backend.error().is_empty(), "{}", backend.error());
    assert_eq!(backend.charge_limit()["maximum"], 60);
    let calls = calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].0.ends_with(".getthreshold") && !calls[0].3, "read is not interactive");
    assert!(calls[1].0.ends_with(".setthreshold") && calls[1].3, "write allows interactive authorization");
    assert!(calls.iter().all(|c| c.2 == "(saya{sv}aya{sh})"));
    let expected = encode_arguments(&BTreeMap::from([("chargeStartThreshold".into(), 59), ("chargeStopThreshold".into(), 60)])).unwrap();
    assert_eq!(calls[1].1, expected);
    assert_eq!(std::fs::read_to_string(battery.join("charge_control_end_threshold")).unwrap(), "60");
    drop(calls);
    backend.close();
    let _ = std::fs::remove_dir_all(&root);
    println!("ok kauth_end_to_end_against_fixture_sysfs");
}

fn kauth_denial_maps_to_message() {
    let bus = PrivateBus::start("d");
    let (_kauth, _calls) = fake_kauth(&bus, |_, _| reply_blob(4, 0));
    let gio_bus = GioBus::new(bus.connect());
    let result: Rc<RefCell<Option<Result<glib::Variant, BusError>>>> = Rc::new(RefCell::new(None));
    let sink = result.clone();
    let (name, path, interface) = charge_limits::ENDPOINT;
    let call = Call::new(name, path, interface, "performAction", Some(charge_limits::parameters("setthreshold",
        Some(&BTreeMap::from([("chargeStopThreshold".into(), 70)]))).unwrap())).mutation();
    gio_bus.call(call, Box::new(move |reply| { sink.replace(Some(reply)); }));
    assert!(wait_until(5 * SECOND, || result.borrow().is_some()));
    let reply = result.borrow_mut().take().unwrap().unwrap();
    let error = charge_limits::decode_reply(&charge_limits::reply_blob(&reply).unwrap()).unwrap_err();
    assert_eq!(error, "Authorization was canceled or denied.");
    // A missing helper is a remote D-Bus error with the prefix stripped.
    let sink = result.clone();
    gio_bus.call(Call::new("org.example.Missing", "/", "org.example.Missing", "Nope", None), Box::new(move |reply| { sink.replace(Some(reply)); }));
    assert!(wait_until(5 * SECOND, || result.borrow().is_some()));
    let error = result.borrow_mut().take().unwrap().unwrap_err();
    assert_eq!(error.name.as_deref(), Some("org.freedesktop.DBus.Error.ServiceUnknown"));
    assert!(!error.message.contains("GDBus.Error"), "{}", error.message);
    println!("ok kauth_denial_maps_to_message");
}

struct Lines {
    receiver: mpsc::Receiver<String>,
}

impl Lines {
    fn new(stdout: std::process::ChildStdout) -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self { receiver }
    }

    /// Next line, while serving the fake services on this thread.
    fn next(&self) -> Option<serde_json::Value> {
        let mut found = None;
        wait_until(5 * SECOND, || {
            if let Ok(line) = self.receiver.try_recv() {
                found = Some(serde_json::from_str(&line).expect("JSON line"));
            }
            found.is_some()
        });
        found
    }
}

fn status_stream_prints_changes_only_and_exits_cleanly() {
    let bus = PrivateBus::start("s");
    let upower = FakeUPower::start(&bus, false);
    laptop(&upower, false);
    let locale = scratch("noloc");
    let mut child = Command::new(env!("CARGO_BIN_EXE_battery-status"))
        .env("DBUS_SYSTEM_BUS_ADDRESS", &bus.address).env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("DESKTOP_SUITE_LOCALE_DIR", &locale).env("LANGUAGE", "en").env("HOME", &locale)
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let lines = Lines::new(child.stdout.take().unwrap());
    let first = lines.next().expect("first line");
    assert_eq!(first["class"], json!(["unavailable"]));
    assert!(first["tooltip"].as_str().unwrap().starts_with("Power source unavailable\n"));
    let plugged = lines.next().expect("plugged line");
    assert_eq!(plugged["text"], "\u{f1e6}");
    assert_eq!(plugged["tooltip"], "Plugged in · 60%\nLaptop battery: 60% · Discharging\nClick for batteries · Right-click for charging settings");
    upower.set_on_battery(true);
    // Uncertain source first, then the re-read.
    let mut line = lines.next().expect("battery line");
    if line["class"] == json!(["unavailable"]) {
        line = lines.next().expect("battery line after re-read");
    }
    assert_eq!(line["class"], json!(["battery"]));
    assert_eq!(line["text"], "\u{f241}");
    assert!(line["tooltip"].as_str().unwrap().starts_with("On battery · 60%\n"));
    // A charge update that does not change the text prints nothing.
    upower.set_device(LAPTOP, "Capacity", 90.0f64.to_variant());
    settle(Duration::from_millis(400));
    assert!(lines.receiver.try_recv().is_err(), "unchanged status is not reprinted");
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let mut status = None;
    wait_until(5 * SECOND, || { status = child.try_wait().unwrap(); status.is_some() });
    assert_eq!(status.expect("exited on SIGTERM").code(), Some(0));

    // Closed stdout: the next change ends the stream.
    let mut child = Command::new(env!("CARGO_BIN_EXE_battery-status"))
        .env("DBUS_SYSTEM_BUS_ADDRESS", &bus.address).env("HOME", &locale)
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    drop(reader);
    upower.set_on_battery(false);
    let mut status = None;
    wait_until(5 * SECOND, || { status = child.try_wait().unwrap(); status.is_some() });
    assert!(status.is_some(), "stream exits on a closed pipe");
    let _ = std::fs::remove_dir_all(&locale);
    println!("ok status_stream_prints_changes_only_and_exits_cleanly");
}

fn check_without_primary_fails_and_bad_arguments_exit_2() {
    let bus = PrivateBus::start("c");
    let home = scratch("home");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_battery-manager")).args(args)
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address).env("DBUS_SYSTEM_BUS_ADDRESS", &bus.address)
            .env("HOME", &home).env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY")
            .output().unwrap()
    };
    let output = run(&["--check"]);
    assert_ne!(output.status.code(), Some(0), "--check never becomes the primary");
    assert!(output.stdout.is_empty());
    let output = run(&["--popup", "--settings"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("error: argument --settings: not allowed with argument --popup"));
    let output = run(&["--popup-offset", "-1"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).ends_with("error: Popup offset must be nonnegative\n"));
    let output = run(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("--popup-offset POPUP_OFFSET"));
    let _ = std::fs::remove_dir_all(&home);
    println!("ok check_without_primary_fails_and_bad_arguments_exit_2");
}

fn main() {
    if std::process::Command::new("dbus-daemon").arg("--version").output().is_err() {
        println!("skipped: dbus-daemon not available");
        return;
    }
    upower_backend_follows_signals_and_owner_changes();
    kauth_end_to_end_against_fixture_sysfs();
    kauth_denial_maps_to_message();
    status_stream_prints_changes_only_and_exits_cleanly();
    check_without_primary_fails_and_bad_arguments_exit_2();
    println!("test result: ok. 5 passed");
}
