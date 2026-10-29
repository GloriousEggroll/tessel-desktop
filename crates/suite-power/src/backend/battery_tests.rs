//! Battery-manager coverage of the shared backend in `Mode::BatteriesOnly`
//! (ported from battery-manager; test_power_backend.py battery cases and
//! test_power_charge_limits.py BackendTests). Never contacts real services.

use super::*;
use suite_core::bus::FakeBus;
use crate::charge_limits::{encode_arguments, qstring, Record};
use suite_core::scheduler::FakeScheduler;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

fn variant(value: &Value) -> glib::Variant {
    match value {
        Value::Bool(flag) => flag.to_variant(),
        Value::Number(n) if n.is_f64() => n.as_f64().unwrap().to_variant(),
        Value::Number(n) if n.as_i64().is_some_and(|v| v < 0) => (n.as_i64().unwrap() as i32).to_variant(),
        Value::Number(n) => match u32::try_from(n.as_u64().unwrap()) {
            Ok(v) => v.to_variant(),
            Err(_) => n.as_u64().unwrap().to_variant(),
        },
        Value::String(s) => s.to_variant(),
        other => panic!("unsupported fixture value {other}"),
    }
}

/// `props(**values)`: an `(a{sv})` GetAll reply.
fn props(values: &Value) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    for (key, value) in values.as_object().unwrap() {
        dict.insert_value(key, &variant(value));
    }
    glib::Variant::tuple_from_iter([dict.end()])
}

fn dbus_error() -> BusError {
    BusError::remote("org.freedesktop.DBus.Error.ServiceUnknown", "Service unavailable")
}

fn unit() -> glib::Variant {
    ().to_variant()
}

fn paths(count: usize) -> glib::Variant {
    let list: Vec<glib::Variant> = (0..count)
        .map(|i| glib::variant::ObjectPath::try_from(format!("/org/freedesktop/UPower/devices/test{i}")).unwrap().to_variant()).collect();
    glib::Variant::tuple_from_iter([glib::Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, list)])
}

type Limits = Rc<RefCell<Option<(Value, Records)>>>;

struct Fixture {
    backend: PowerBackend,
    bus: Rc<FakeBus>,
    scheduler: Rc<FakeScheduler>,
    changed: Rc<Cell<usize>>,
    errors: Rc<RefCell<Vec<String>>>,
    limits: Limits,
    connects: Rc<RefCell<Vec<Box<dyn FnOnce(Result<Rc<dyn Bus>, BusError>)>>>>,
}

impl Fixture {
    fn new() -> Self {
        let scheduler = Rc::new(FakeScheduler::default());
        let changed = Rc::new(Cell::new(0));
        let errors = Rc::new(RefCell::new(Vec::new()));
        let limits: Limits = Rc::new(RefCell::new(None));
        let connects = Rc::new(RefCell::new(Vec::new()));
        let empty = std::env::temp_dir().join(format!("battery-backend-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty);
        let (inspect_limits, pending) = (limits.clone(), connects.clone());
        let options = Options {
            mode: Mode::BatteriesOnly,
            session_id: Box::new(|| "auto".into()),
            connector: Box::new(move |done| pending.borrow_mut().push(done)),
            scheduler: scheduler.clone(),
            inspect: Box::new(move |devices| match &*inspect_limits.borrow() {
                Some((state, records)) if !devices.is_empty() => (state.clone(), records.clone()),
                Some(_) => ({ let mut s = charge_limits::initial_state(); s["supported"] = json!(false); s }, Records::new()),
                None => charge_limits::inspect_limits(devices, &empty, &[]),
            }),
        };
        let (count, sink) = (changed.clone(), errors.clone());
        let backend = PowerBackend::new(move || count.set(count.get() + 1), move |m| sink.borrow_mut().push(m.to_owned()), options);
        let bus = FakeBus::new();
        backend.0.bus.replace(Some(bus.clone() as Rc<dyn Bus>));
        backend.state_mut().ready = true;
        Self { backend, bus, scheduler, changed, errors, limits, connects }
    }

    fn reply_power(&self, devices: &[Value], on_battery: bool) {
        self.bus.reply("GetAll", Ok(props(&json!({"OnBattery": on_battery}))), Some(UPOWER.1), None, 0);
        self.bus.reply("EnumerateDevices", Ok(paths(devices.len())), None, None, 0);
        for (index, device) in devices.iter().enumerate() {
            let path = format!("/org/freedesktop/UPower/devices/test{index}");
            self.bus.reply("GetAll", Ok(props(device)), Some(&path), None, 0);
        }
    }

    fn battery_with_limits() -> Value {
        json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "NativePath": "BAT0",
               "Model": "Laptop battery", "Serial": "test-battery", "Percentage": 60.0,
               "ChargeThresholdSupported": true, "ChargeThresholdEnabled": false,
               "ChargeStartThreshold": 75, "ChargeEndThreshold": 80, "ChargeThresholdSettingsSupported": 3})
    }

    fn ready_battery_with_limits(&self) -> (String, Value) {
        self.backend.refresh_power();
        let battery = Self::battery_with_limits();
        self.reply_power(std::slice::from_ref(&battery), false);
        ("/org/freedesktop/UPower/devices/test0".into(), battery)
    }

    fn enable_calls(&self) -> usize {
        self.bus.calls.borrow().iter().filter(|c| c.method == "EnableChargeThreshold").count()
    }

    fn battery(&self) -> Value {
        self.backend.batteries()[0].clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.backend.close();
    }
}

fn properties_changed(interface: &str, changed: &[(&str, glib::Variant)], invalidated: &[&str]) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    for (key, value) in changed {
        dict.insert_value(key, value);
    }
    let invalidated: Vec<String> = invalidated.iter().map(|s| s.to_string()).collect();
    glib::Variant::tuple_from_iter([interface.to_variant(), dict.end(), invalidated.to_variant()])
}

/// A UPower signal as the bus delivers it (`_power_signal` arguments).
fn sig(path: &str, interface: &str, member: &str, params: &glib::Variant) -> Signal {
    Signal { sender: UPOWER.0.into(), path: path.into(), interface: interface.into(), member: member.into(), params: params.clone() }
}

fn object(path: &str) -> glib::Variant {
    glib::Variant::tuple_from_iter([glib::variant::ObjectPath::try_from(path).unwrap().to_variant()])
}

#[test]
fn start_async_and_read_only_battery_subset() {
    let f = Fixture::new();
    f.backend.0.bus.replace(None);
    f.backend.state_mut().ready = false;
    f.backend.start();
    f.backend.start();
    assert!(!f.backend.ready());
    assert_eq!(f.connects.borrow().len(), 1);
    let done = f.connects.borrow_mut().pop().unwrap();
    done(Ok(f.bus.clone() as Rc<dyn Bus>));
    assert!(f.backend.ready());
    assert_eq!(f.bus.methods(), ["EnumerateDevices", "GetAll"].iter().map(|s| s.to_string()).collect());
    assert!(f.bus.calls.borrow().iter().all(|c| c.name == UPOWER.0 && !c.mutation && !c.interactive));
    // battery_only: NameOwnerChanged(UPower), PropertiesChanged, UPower signals.
    let signals = f.bus.signals.borrow();
    assert_eq!(signals.len(), 3);
    assert_eq!(signals[0].4.as_deref(), Some(UPOWER.0));
    assert!(f.bus.closed_handler.borrow().is_some());
    assert!(f.backend.has_source("refresh"));
    assert_eq!(f.scheduler.delays(), [120000]);
}

#[test]
fn connect_failure_reports_and_retries() {
    let f = Fixture::new();
    f.backend.0.bus.replace(None);
    f.backend.start();
    let done = f.connects.borrow_mut().pop().unwrap();
    done(Err(BusError::local("No such file")));
    assert_eq!(f.backend.error(), "System power services are unavailable: No such file");
    assert!(f.backend.has_source("connect"));
    assert_eq!(f.scheduler.delays(), [5000]);
}

#[test]
fn batteries_ignore_peripherals_and_weight_capacity() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.reply_power(&[
        json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Energy": 10.0, "EnergyFull": 20.0, "Percentage": 50.0}),
        json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Energy": 60.0, "EnergyFull": 80.0, "Percentage": 75.0}),
        json!({"Type": 5, "PowerSupply": false, "IsPresent": true, "Percentage": 1.0}),
        json!({"Type": 2, "PowerSupply": false, "IsPresent": true, "Percentage": 2.0}),
        json!({"Type": 2, "PowerSupply": true, "IsPresent": false, "Percentage": 0.0}),
    ], true);
    assert!(f.backend.has_battery());
    assert!(f.backend.on_battery());
    assert!(f.backend.power_source_known());
    assert_eq!(f.backend.percentage(), Some(70.0));
    assert!(f.changed.get() > 0);
}

#[test]
fn warning_level_comes_only_from_system_batteries() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Percentage": 4.0, "WarningLevel": 4}),
                    json!({"Type": 5, "PowerSupply": false, "IsPresent": true, "Percentage": 1.0, "WarningLevel": 5})], true);
    assert_eq!(f.backend.warning_level(), 4);
}

#[test]
fn desktop_peripherals_do_not_create_battery_policy() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 5, "PowerSupply": false, "IsPresent": true, "Percentage": 55.0}),
                    json!({"Type": 12, "PowerSupply": false, "IsPresent": true, "Percentage": 40.0})], false);
    assert!(!f.backend.has_battery());
    assert!(!f.backend.on_battery());
    assert_eq!(f.backend.percentage(), None);
    assert!(f.backend.power_source_known());
    let batteries = f.backend.batteries();
    assert_eq!(batteries.len(), 2);
    let mut levels: Vec<f64> = batteries.iter().map(|b| b["percentage"].as_f64().unwrap()).collect();
    levels.sort_by(f64::total_cmp);
    assert_eq!(levels, [40.0, 55.0]);
    assert!(batteries.iter().all(|b| b["power_supply"] == false));
    assert!(batteries.iter().all(|b| b["charge_limit_adjustable"] == false));
}

#[test]
fn charge_limit_waits_for_revalidation_and_readback() {
    let f = Fixture::new();
    let (path, mut battery) = f.ready_battery_with_limits();
    assert!(BatteryModel::set_charge_thresholds(&f.backend, &path, true));
    assert!(f.backend.busy());
    assert_eq!(f.battery()["busy"], true);
    assert_eq!(f.battery()["charge_threshold_enabled"], false);
    assert_eq!(f.enable_calls(), 0);
    assert!(!BatteryModel::set_charge_thresholds(&f.backend, &path, false));
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    let write = f.bus.last_pending();
    assert_eq!((write.path.as_str(), write.interface.as_str(), write.method.as_str()), (path.as_str(), UPOWER_DEVICE, "EnableChargeThreshold"));
    assert_eq!(write.params.unwrap().get::<(bool,)>(), Some((true,)));
    assert!(write.interactive);
    assert!(write.mutation, "mutations are not cancellable");
    f.bus.reply("EnableChargeThreshold", Ok(unit()), None, None, 0);
    assert!(f.backend.busy());
    battery["ChargeThresholdEnabled"] = json!(true);
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    assert!(!f.backend.busy());
    assert_eq!(f.battery()["charge_threshold_enabled"], true);
    assert_eq!(f.battery()["busy"], false);
}

#[test]
fn charge_limit_validation_and_unsupported_peripheral_never_mutate() {
    let f = Fixture::new();
    let (path, battery) = f.ready_battery_with_limits();
    // Python's (path, 1) / (path, "false") type cases are rejected by the signature.
    assert!(!BatteryModel::set_charge_thresholds(&f.backend, "/missing", true));
    let mut changed = battery.clone();
    changed["PowerSupply"] = json!(false);
    f.backend.set_power_device(&path, changed);
    assert!(!BatteryModel::set_charge_thresholds(&f.backend, &path, true));
    let mut changed = battery.clone();
    changed["ChargeThresholdSupported"] = json!(false);
    f.backend.set_power_device(&path, changed);
    assert!(!BatteryModel::set_charge_thresholds(&f.backend, &path, true));
    assert_eq!(f.enable_calls(), 0);
    assert_eq!(f.errors.borrow().last().unwrap(), "Charge limits are not supported by this battery.");
}

#[test]
fn charge_limit_hot_unplug_invalidates_pending_validation() {
    let f = Fixture::new();
    let (path, battery) = f.ready_battery_with_limits();
    BatteryModel::set_charge_thresholds(&f.backend, &path, true);
    f.backend.power_signal(&sig(UPOWER.1, UPOWER.2, "DeviceRemoved", &object(&path)));
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    assert!(!f.backend.busy());
    assert!(f.backend.batteries().is_empty());
    assert_eq!(f.enable_calls(), 0);
}

#[test]
fn charge_limit_service_loss_invalidates_pending_validation() {
    let f = Fixture::new();
    let (path, battery) = f.ready_battery_with_limits();
    BatteryModel::set_charge_thresholds(&f.backend, &path, true);
    f.backend.owner_changed(UPOWER.0);
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    assert!(!f.backend.busy());
    assert!(f.backend.batteries().is_empty());
    assert_eq!(f.enable_calls(), 0);
    assert!(!f.backend.power_ready());
    assert!(f.backend.has_source("power"));
}

#[test]
fn charge_limit_replacement_and_changed_capability_do_not_mutate() {
    for changed in [json!({"Serial": "new-battery"}), json!({"ChargeThresholdSupported": false})] {
        let f = Fixture::new();
        let (path, battery) = f.ready_battery_with_limits();
        BatteryModel::set_charge_thresholds(&f.backend, &path, true);
        let mut replaced = battery.clone();
        for (key, value) in changed.as_object().unwrap() {
            replaced[key] = value.clone();
        }
        f.bus.reply("GetAll", Ok(props(&replaced)), Some(&path), None, 0);
        assert!(!f.backend.busy());
        assert_eq!(f.enable_calls(), 0);
        // The rejection re-reads UPower.
        f.reply_power(std::slice::from_ref(&battery), false);
        assert!(f.backend.power_ready());
    }
}

#[test]
fn charge_limit_denial_preserves_last_confirmed_setting() {
    let f = Fixture::new();
    let (path, battery) = f.ready_battery_with_limits();
    BatteryModel::set_charge_thresholds(&f.backend, &path, true);
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    f.bus.reply("EnableChargeThreshold", Err(BusError::remote("org.freedesktop.UPower.Device.NotAuthorized", "Permission denied")), None, None, 0);
    assert!(!f.backend.busy());
    assert_eq!(f.battery()["charge_threshold_enabled"], false);
    assert!(f.errors.borrow().last().unwrap().contains("Authorization"));
}

#[test]
fn charge_limit_unchanged_setting_does_not_write() {
    let f = Fixture::new();
    let (path, battery) = f.ready_battery_with_limits();
    BatteryModel::set_charge_thresholds(&f.backend, &path, false);
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    assert!(!f.backend.busy());
    assert_eq!(f.enable_calls(), 0);
}

#[test]
fn charge_limit_readback_detects_unapplied_mutation() {
    let f = Fixture::new();
    let (path, battery) = f.ready_battery_with_limits();
    BatteryModel::set_charge_thresholds(&f.backend, &path, true);
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    f.bus.reply("EnableChargeThreshold", Ok(unit()), None, None, 0);
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    assert!(!f.backend.busy());
    assert_eq!(f.battery()["charge_threshold_enabled"], false);
    assert!(f.errors.borrow().last().unwrap().contains("did not apply"));
}

#[test]
fn ups_power_source_and_hotplug_refresh() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 3, "PowerSupply": true, "IsPresent": true, "State": 2, "Percentage": 80.0})], false);
    assert!(f.backend.has_battery());
    assert!(f.backend.on_battery());
    f.backend.power_signal(&sig(UPOWER.1, UPOWER.2, "DeviceRemoved", &object("/org/freedesktop/UPower/devices/test0")));
    assert!(!f.backend.power_source_known());
    assert!(f.backend.has_source("power"));
    f.backend.refresh_power();
    f.reply_power(&[], false);
    assert!(!f.backend.has_battery());
    assert!(!f.backend.on_battery());
    assert!(f.backend.power_source_known());
}

#[test]
fn charge_and_peripheral_signals_do_not_reset_idle_policy() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Percentage": 80.0})], true);
    for percentage in [79.0, 78.0, 77.0] {
        f.backend.power_signal(&sig("/org/freedesktop/UPower/devices/test0", PROPERTIES, "PropertiesChanged",
            &properties_changed(UPOWER_DEVICE, &[("Percentage", percentage.to_variant())], &[])));
        assert!(f.backend.power_source_known());
        assert!(f.backend.on_battery());
    }
    f.backend.power_signal(&sig(UPOWER.1, UPOWER.2, "DeviceAdded", &object("/headset")));
    assert!(f.backend.power_source_known());
    // Same OnBattery value: still certain.
    f.backend.power_signal(&sig(UPOWER.1, PROPERTIES, "PropertiesChanged",
        &properties_changed(UPOWER.2, &[("OnBattery", true.to_variant())], &[])));
    assert!(f.backend.power_source_known());
    f.backend.power_signal(&sig(UPOWER.1, PROPERTIES, "PropertiesChanged",
        &properties_changed(UPOWER.2, &[("OnBattery", false.to_variant())], &[])));
    assert!(!f.backend.power_source_known());
}

#[test]
fn supply_identity_and_ups_state_changes_are_uncertain() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 3, "PowerSupply": true, "IsPresent": true, "State": 1, "Percentage": 80.0})], false);
    let path = "/org/freedesktop/UPower/devices/test0";
    f.backend.power_signal(&sig(path, PROPERTIES, "PropertiesChanged", &properties_changed(UPOWER_DEVICE, &[("State", 4u32.to_variant())], &[])));
    assert!(f.backend.power_source_known(), "charging → full is still on AC");
    f.backend.power_signal(&sig(path, PROPERTIES, "PropertiesChanged", &properties_changed(UPOWER_DEVICE, &[("State", 2u32.to_variant())], &[])));
    assert!(!f.backend.power_source_known());
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 3, "PowerSupply": true, "IsPresent": true, "State": 1})], false);
    f.backend.power_signal(&sig(path, PROPERTIES, "PropertiesChanged", &properties_changed(UPOWER_DEVICE, &[], &["PowerSupply"])));
    assert!(!f.backend.power_source_known());
}

#[test]
fn failed_device_read_never_assumes_ac() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.bus.reply("GetAll", Ok(props(&json!({"OnBattery": false}))), Some(UPOWER.1), None, 0);
    let list = glib::Variant::tuple_from_iter([glib::Variant::array_from_iter_with_type(
        glib::VariantTy::OBJECT_PATH, [glib::variant::ObjectPath::try_from("/battery").unwrap().to_variant()])]);
    f.bus.reply("EnumerateDevices", Ok(list), None, None, 0);
    f.bus.reply("GetAll", Err(dbus_error()), Some("/battery"), None, 0);
    assert!(!f.backend.power_source_known());
    assert!(!f.backend.power_ready());
    assert_eq!(f.backend.error(), "The current power source is unavailable. Check UPower.");
}

#[test]
fn missing_on_battery_and_stale_reads_are_ignored() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.backend.refresh_power();
    // Replies to the first (stale) read change nothing.
    f.bus.reply("GetAll", Ok(props(&json!({"OnBattery": true}))), Some(UPOWER.1), None, 0);
    f.bus.reply("EnumerateDevices", Ok(paths(0)), None, None, 0);
    assert!(!f.backend.power_ready());
    f.bus.reply("GetAll", Ok(props(&json!({"Other": 1}))), Some(UPOWER.1), None, 0);
    f.bus.reply("EnumerateDevices", Ok(paths(0)), None, None, 0);
    assert!(!f.backend.power_ready());
}

#[test]
fn bus_close_invalidates_callbacks_and_schedules_reconnect() {
    let f = Fixture::new();
    f.backend.refresh_power();
    f.backend.bus_closed();
    f.bus.reply("GetAll", Ok(props(&json!({"OnBattery": false}))), Some(UPOWER.1), None, 0);
    assert!(!f.backend.ready());
    assert!(!f.backend.power_source_known());
    assert!(f.backend.has_source("connect"));
    assert_eq!(f.backend.error(), "System power services are disconnected.");
    assert!(f.bus.cancelled.get() > 0);
}

#[test]
fn close_suppresses_pending_callbacks() {
    let f = Fixture::new();
    let (path, battery) = f.ready_battery_with_limits();
    BatteryModel::set_charge_thresholds(&f.backend, &path, true);
    f.backend.close();
    let changed = f.changed.get();
    f.bus.reply("GetAll", Ok(props(&battery)), Some(&path), None, 0);
    assert_eq!(f.changed.get(), changed);
    assert!(!f.backend.busy());
    assert_eq!(f.enable_calls(), 0);
    assert!(f.scheduler.pending.borrow().is_empty());
}

// --- KAuth charge limits (test_power_charge_limits.BackendTests) ---------------

fn kauth_reply(code: u32, kind: u32, description: &str) -> glib::Variant {
    let mut blob = encode_arguments(&BTreeMap::new()).unwrap();
    blob.extend(code.to_be_bytes());
    blob.extend(kind.to_be_bytes());
    blob.extend(qstring(description));
    glib::Variant::tuple_from_iter([glib::Variant::array_from_fixed_array(&blob)])
}

fn ok() -> glib::Variant {
    kauth_reply(0, 2, "")
}

struct Limited {
    f: Fixture,
}

impl Limited {
    fn new() -> Self {
        let f = Fixture::new();
        let state = json!({"supported": true, "maximum": 80, "battery_count": 1, "busy": false, "error": "", "reason": "All laptop batteries"});
        let records = Records::from([("/BAT0".to_owned(), Record { name: "BAT0".into(), start: Some(75), stop: 80,
                                                                    identity: vec!["sysfs/BAT0".into(), "serial".into()] })]);
        f.limits.replace(Some((state, records)));
        {
            f.backend.state_mut().power_ready = true;
            f.backend.set_power_device("/BAT0", json!({"NativePath": "BAT0", "Type": 2, "PowerSupply": true, "IsPresent": true,
                "ChargeThresholdSupported": true, "ChargeEndThreshold": 90, "ChargeStartThreshold": 85}));
        }
        f.backend.publish_batteries();
        Self { f }
    }

    fn set_hardware(&self, stop: i64, start: i64) {
        let mut limits = self.f.limits.borrow_mut();
        let (state, records) = limits.as_mut().unwrap();
        state["maximum"] = json!(stop);
        let record = records.get_mut("/BAT0").unwrap();
        record.stop = stop;
        record.start = Some(start);
    }

    fn action(call: &Call) -> String {
        call.params.as_ref().unwrap().child_value(0).str().unwrap().to_owned()
    }
}

#[test]
fn refresh_reads_actual_values_without_any_helper_calls() {
    let l = Limited::new();
    assert!(l.f.bus.calls.borrow().is_empty());
    let battery = l.f.battery();
    assert_eq!(battery["charge_limit_adjustable"], true);
    assert_eq!(battery["charge_end_threshold"], 80);
    assert_eq!(battery["charge_start_threshold"], 75);
    assert_eq!(battery["charge_threshold_enabled"], true);
    assert!(!BatteryModel::set_charge_thresholds(&l.f.backend, "/BAT0", true));
    assert_eq!(l.f.errors.borrow().last().unwrap(), "Use Maximum charge to adjust the laptop battery limits.");
    assert!(l.f.bus.calls.borrow().is_empty());
}

#[test]
fn apply_revalidates_then_authorizes_then_reads_hardware() {
    let l = Limited::new();
    assert!(l.f.backend.set_charge_limit(&json!(60)));
    assert_eq!(l.f.backend.charge_limit()["busy"], true);
    assert_eq!(l.f.battery()["busy"], true);
    let read = l.f.bus.last_call();
    assert!(Limited::action(&read).ends_with(".getthreshold"));
    assert_eq!((read.name.as_str(), read.path.as_str(), read.interface.as_str()), charge_limits::ENDPOINT);
    assert!(!read.mutation && !read.interactive);
    l.f.bus.reply("performAction", Ok(ok()), None, None, 0);
    let mutation = l.f.bus.last_call();
    assert!(Limited::action(&mutation).ends_with(".setthreshold"));
    let expected = encode_arguments(&BTreeMap::from([("chargeStartThreshold".into(), 59), ("chargeStopThreshold".into(), 60)])).unwrap();
    assert_eq!(mutation.params.as_ref().unwrap().child_value(3).fixed_array::<u8>().unwrap(), expected);
    assert!(mutation.interactive);
    assert!(mutation.mutation);
    assert_eq!(l.f.backend.charge_limit()["maximum"], 80);
    l.set_hardware(60, 59);
    l.f.bus.reply("performAction", Ok(ok()), None, None, 0);
    assert!(!l.f.backend.busy());
    assert_eq!(l.f.backend.charge_limit()["maximum"], 60);
    assert!(l.f.errors.borrow().is_empty());
    // The read-back refreshes UPower.
    assert!(l.f.bus.calls.borrow().iter().any(|c| c.method == "EnumerateDevices"));
}

#[test]
fn cancel_and_unapplied_success_do_not_invent_new_value() {
    let l = Limited::new();
    for result in [kauth_reply(4, 0, ""), ok()] {
        l.f.backend.set_charge_limit(&json!(60));
        l.f.bus.reply("performAction", Ok(ok()), None, None, 0);
        l.f.bus.reply("performAction", Ok(result), None, None, 0);
        assert!(!l.f.backend.busy());
        assert!(!l.f.backend.charge_limit()["error"].as_str().unwrap().is_empty());
        assert_eq!(l.f.backend.charge_limit()["maximum"], 80);
    }
    assert!(l.f.errors.borrow()[0].contains("denied"));
    assert!(l.f.errors.borrow()[1].contains("could not be confirmed"));
}

#[test]
fn hotplug_before_authorization_prevents_mutation() {
    let l = Limited::new();
    l.f.backend.set_charge_limit(&json!(60));
    l.f.limits.borrow_mut().as_mut().unwrap().1.get_mut("/BAT0").unwrap().identity = vec!["replacement".into()];
    l.f.bus.reply("performAction", Ok(ok()), None, None, 0);
    assert_eq!(l.f.bus.calls.borrow().len(), 1);
    assert!(l.f.backend.charge_limit()["error"].as_str().unwrap().contains("changed"));
}

#[test]
fn validation_unchanged_and_overlap_never_send_extra_mutations() {
    let l = Limited::new();
    for value in [json!(true), json!(0), json!(101), json!(50.5), json!("60")] {
        assert!(!l.f.backend.set_charge_limit(&value), "{value}");
    }
    assert!(l.f.backend.set_charge_limit(&json!(80)));
    assert!(l.f.bus.calls.borrow().is_empty());
    l.f.backend.set_charge_limit(&json!(60));
    assert!(!l.f.backend.set_charge_limit(&json!(50)));
    assert_eq!(l.f.bus.calls.borrow().len(), 1);
}

#[test]
fn read_transport_failure_and_close_drop_callbacks() {
    let l = Limited::new();
    l.f.backend.set_charge_limit(&json!(60));
    l.f.bus.reply("performAction", Err(dbus_error()), None, None, 0);
    assert!(!l.f.backend.busy());
    assert_eq!(l.f.backend.charge_limit()["error"], "Service unavailable");
    l.f.backend.set_charge_limit(&json!(60));
    l.f.backend.close();
    l.f.bus.reply("performAction", Ok(ok()), None, None, 0);
    assert_eq!(l.f.bus.calls.borrow().len(), 2);
}

#[test]
fn unsupported_or_unready_limits_fail_without_calls() {
    let l = Limited::new();
    l.f.backend.state_mut().power_ready = false;
    assert!(!l.f.backend.set_charge_limit(&json!(60)));
    assert_eq!(l.f.errors.borrow().last().unwrap(), "The laptop battery information is not available.");
    l.f.backend.state_mut().power_ready = true;
    l.f.limits.borrow_mut().as_mut().unwrap().0["supported"] = json!(false);
    l.f.limits.borrow_mut().as_mut().unwrap().0["reason"] = json!("Adjusting charge limits requires the KDE charge-limit helper.");
    assert!(!l.f.backend.set_charge_limit(&json!(60)));
    assert!(l.f.errors.borrow().last().unwrap().contains("KDE"));
    assert!(l.f.bus.calls.borrow().is_empty());
}

#[test]
fn battery_only_startup_and_refresh_use_only_upower() {
    let f = Fixture::new();
    f.backend.subscribe();
    f.backend.refresh();
    assert!(!f.bus.calls.borrow().is_empty());
    assert!(f.bus.calls.borrow().iter().all(|c| c.name == UPOWER.0));
    assert_eq!(f.bus.methods().len(), 2);
    assert_eq!(f.bus.signals.borrow().len(), 3);
}
