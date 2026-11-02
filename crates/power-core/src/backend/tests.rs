//! Port of test_power_backend.py and the backend part of
//! test_power_charge_limits.py: deferred fake D-Bus replies only.

use super::*;
use tessel_core::bus::{FakeBus, Reply};
use crate::charge_limits::Record;
use tessel_core::scheduler::FakeScheduler;

use glib::Variant;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn variant(value: &Value) -> Variant {
    match value {
        Value::Bool(b) => b.to_variant(),
        Value::Number(n) if n.is_f64() => n.as_f64().unwrap().to_variant(),
        Value::Number(n) if n.as_i64().is_some_and(|v| v < 0) => (n.as_i64().unwrap() as i32).to_variant(),
        Value::Number(n) => (n.as_u64().unwrap() as u32).to_variant(),
        Value::Array(rows) => {
            let dicts: Vec<Variant> = rows.iter().map(|row| dict(row)).collect();
            Variant::array_from_iter_with_type(glib::VariantTy::VARDICT, dicts)
        }
        Value::String(s) => s.to_variant(),
        _ => "".to_variant(),
    }
}

fn dict(value: &Value) -> Variant {
    let d = glib::VariantDict::new(None);
    for (k, v) in value.as_object().unwrap() {
        d.insert_value(k, &variant(v));
    }
    d.end()
}

pub fn props(value: Value) -> Reply {
    Ok(Variant::tuple_from_iter([dict(&value)]))
}

pub fn dbus_error(message: &str) -> Reply {
    Err(BusError::remote("org.freedesktop.DBus.Error.ServiceUnknown", message))
}

fn unit() -> Reply {
    Ok(().to_variant())
}

fn string(value: &str) -> Reply {
    Ok((value,).to_variant())
}

fn inhibitors(rows: &[(&str, &str, &str, &str, u32, u32)]) -> Reply {
    let rows: Vec<(String, String, String, String, u32, u32)> =
        rows.iter().map(|r| (r.0.into(), r.1.into(), r.2.into(), r.3.into(), r.4, r.5)).collect();
    Ok((rows,).to_variant())
}

pub struct Fx {
    pub backend: PowerBackend,
    pub bus: Rc<FakeBus>,
    pub changed: Rc<Cell<usize>>,
    pub errors: Rc<RefCell<Vec<String>>>,
    pub scheduler: Rc<FakeScheduler>,
}

fn no_limits() -> Inspector {
    Box::new(|_| (charge_limits::initial_state(), Records::new()))
}

fn fixture_with(mode: Mode, inspect: Inspector) -> Fx {
    let changed = Rc::new(Cell::new(0));
    let errors = Rc::new(RefCell::new(Vec::new()));
    let scheduler = Rc::new(FakeScheduler::default());
    let (c, e) = (changed.clone(), errors.clone());
    let options = Options { mode, connector: Box::new(|_| {}), scheduler: scheduler.clone(), inspect,
                            session_id: Box::new(|| "auto".into()) };
    let backend = PowerBackend::new(move || c.set(c.get() + 1), move |m| e.borrow_mut().push(m.into()), options);
    let bus = FakeBus::new();
    backend.attach(bus.clone());
    Fx { backend, bus, changed, errors, scheduler }
}

fn fixture() -> Fx {
    fixture_with(Mode::Full, no_limits())
}

impl Fx {
    fn reply(&self, method: &str, result: Reply) -> tessel_core::bus::Call {
        self.bus.reply(method, result, None, None, 0)
    }

    fn reply_profiles(&self, name: Option<&str>, active: &str) {
        self.bus.reply("GetAll", props(json!({"Profiles": [{"Profile": "power-saver"}, {"Profile": "balanced"}, {"Profile": "performance"}],
            "ActiveProfile": active, "PerformanceDegraded": ""})), None, Some(name.unwrap_or(PPD_ENDPOINTS[0].0)), 0);
    }

    fn reply_power(&self, devices: &[Value], on_battery: bool) {
        self.bus.reply("GetAll", props(json!({"OnBattery": on_battery})), Some(UPOWER.1), None, 0);
        let paths: Vec<glib::variant::ObjectPath> = (0..devices.len())
            .map(|i| glib::variant::ObjectPath::try_from(format!("/org/freedesktop/UPower/devices/test{i}")).unwrap()).collect();
        self.reply("EnumerateDevices", Ok((paths,).to_variant()));
        for (i, device) in devices.iter().enumerate() {
            self.bus.reply("GetAll", props(device.clone()), Some(&format!("/org/freedesktop/UPower/devices/test{i}")), None, 0);
        }
    }

    fn reply_sleep(&self, suspend: &str, hibernate: &str, combined: &str) {
        for (method, value) in [("CanSuspend", suspend), ("CanHibernate", hibernate), ("CanSuspendThenHibernate", combined)] {
            self.reply(method, string(value));
        }
    }

    fn ready_sleep(&self) {
        self.backend.refresh_sleep();
        self.reply_sleep("yes", "yes", "yes");
    }

    fn reply_session(&self, active: bool, remote: bool, kind: &str) {
        self.reply("GetSession", Ok((glib::variant::ObjectPath::try_from("/session").unwrap(),).to_variant()));
        self.bus.reply("GetAll", props(json!({"Active": active, "Remote": remote, "Type": kind})), Some("/session"), None, 0);
    }

    fn state(&self) -> BackendState {
        self.backend.state().clone()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.backend.close();
    }
}

fn properties_changed(path: &str, interface: &str, changed: &[(&str, Variant)], invalidated: &[&str]) -> Signal {
    let d = glib::VariantDict::new(None);
    for (k, v) in changed {
        d.insert_value(k, v);
    }
    let invalidated: Vec<String> = invalidated.iter().map(|s| s.to_string()).collect();
    Signal { sender: UPOWER.0.into(), path: path.into(), interface: PROPERTIES.into(), member: "PropertiesChanged".into(),
             params: (interface, d.end(), invalidated).to_variant() }
}

fn device_signal(member: &str, path: &str) -> Signal {
    Signal { sender: UPOWER.0.into(), path: UPOWER.1.into(), interface: UPOWER.2.into(), member: member.into(),
             params: (glib::variant::ObjectPath::try_from(path).unwrap(),).to_variant() }
}

#[test]
fn start_async_and_read_only() {
    let pending: Rc<RefCell<Vec<Box<dyn FnOnce(Result<Rc<dyn tessel_core::bus::Bus>, BusError>)>>>> = Rc::default();
    let p = pending.clone();
    let scheduler = Rc::new(FakeScheduler::default());
    let options = Options { mode: Mode::Full, connector: Box::new(move |done| p.borrow_mut().push(done)), scheduler,
                            inspect: no_limits(), session_id: Box::new(|| "auto".into()) };
    let backend = PowerBackend::new(|| {}, |_| {}, options);
    backend.start();
    backend.start();
    assert!(!backend.state().ready);
    assert_eq!(pending.borrow().len(), 1);
    let bus = FakeBus::new();
    let done = pending.borrow_mut().pop().unwrap();
    done(Ok(bus.clone()));
    assert!(backend.state().ready);
    let methods: Vec<String> = bus.methods().into_iter().collect();
    assert_eq!(methods, ["CanHibernate", "CanSuspend", "CanSuspendThenHibernate", "EnumerateDevices", "GetAll"]);
    assert!(bus.closed_handler.borrow().is_some());
    assert_eq!(bus.signals.borrow().len(), 9);
    backend.close();
}

#[test]
fn subsystems_independent_and_legacy_profile_fallback() {
    let f = fixture();
    f.backend.refresh();
    f.bus.reply("GetAll", dbus_error("Service unavailable"), None, Some(PPD_ENDPOINTS[0].0), 0);
    f.reply_profiles(Some(PPD_ENDPOINTS[1].0), "balanced");
    f.bus.reply("GetAll", dbus_error("x"), Some(UPOWER.1), None, 0);
    f.reply("EnumerateDevices", dbus_error("x"));
    f.reply_sleep("yes", "no", "challenge");
    let s = f.state();
    assert!(s.profiles_ready && s.sleep_ready && !s.power_ready && !s.power_source_known);
    assert_eq!(s.capability("hibernate"), Some("no"));
    assert_eq!(s.capability("suspend-then-hibernate"), Some("challenge"));
    assert_eq!(f.backend.profile_endpoint(), Some(PPD_ENDPOINTS[1]));
}

#[test]
fn profile_change_is_explicit_authorized_property_write() {
    let f = fixture();
    f.backend.refresh_profiles();
    f.reply_profiles(None, "balanced");
    assert!(!f.backend.set_profile("invalid"));
    assert!(!f.bus.called("Set"));
    assert!(f.backend.set_profile("performance"));
    assert!(f.state().busy);
    assert!(!f.backend.set_profile("power-saver"));
    let write = f.bus.last_pending();
    assert_eq!(write.method, "Set");
    assert_eq!(write.interface, PROPERTIES);
    let params = write.params.unwrap();
    assert_eq!(params.child_value(0).str(), Some(PPD_ENDPOINTS[0].2));
    assert_eq!(params.child_value(1).str(), Some("ActiveProfile"));
    assert_eq!(params.child_value(2).as_variant().unwrap().str(), Some("performance"));
    assert!(write.interactive && write.mutation, "interactive, uncancellable mutation");
    f.reply("Set", unit());
    f.reply_profiles(None, "performance");
    assert!(!f.state().busy);
    assert_eq!(f.state().active_profile, "performance");
}

#[test]
fn batteries_ignore_peripherals_and_weight_capacity() {
    let f = fixture();
    f.backend.refresh_power();
    f.reply_power(&[
        json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Energy": 10.0, "EnergyFull": 20.0, "Percentage": 50.0}),
        json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Energy": 60.0, "EnergyFull": 80.0, "Percentage": 75.0}),
        json!({"Type": 5, "PowerSupply": false, "IsPresent": true, "Percentage": 1.0}),
        json!({"Type": 2, "PowerSupply": false, "IsPresent": true, "Percentage": 2.0}),
        json!({"Type": 2, "PowerSupply": true, "IsPresent": false, "Percentage": 0.0})], true);
    let s = f.state();
    assert!(s.has_battery && s.on_battery && s.power_source_known);
    assert_eq!(s.percentage, Some(70.0));
}

#[test]
fn warning_level_comes_only_from_system_batteries() {
    let f = fixture();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Percentage": 4.0, "WarningLevel": 4}),
                    json!({"Type": 5, "PowerSupply": false, "IsPresent": true, "Percentage": 1.0, "WarningLevel": 5})], true);
    assert_eq!(f.state().warning_level, 4);
}

#[test]
fn explicit_actions_ignore_idle_inhibitors_but_require_capability_and_session() {
    let f = fixture();
    f.ready_sleep();
    let results = Rc::new(RefCell::new(Vec::new()));
    let r = results.clone();
    let record = move || -> Option<Box<dyn FnOnce(bool)>> { let r = r.clone(); Some(Box::new(move |ok| r.borrow_mut().push(ok))) };
    f.backend.explicit_power("hibernate", record(), false);
    assert!(!f.bus.has_pending("ListInhibitors"));
    f.reply_session(true, false, "wayland");
    let call = f.reply("Hibernate", unit());
    assert_eq!(call.params.unwrap().child_value(0).get::<bool>(), Some(false));
    assert_eq!(*results.borrow(), [true]);
    f.backend.state_mut().capabilities.insert("suspend".into(), "challenge".into());
    f.backend.explicit_power("suspend", record(), false);
    assert_eq!(results.borrow().last(), Some(&false));
    assert!(!f.bus.called("Suspend"));
    f.backend.explicit_power("poweroff", record(), true);
    f.reply("CanPowerOff", string("challenge"));
    f.reply_session(false, false, "wayland");
    assert_eq!(results.borrow().last(), Some(&false));
    assert!(!f.bus.called("PowerOff"));
    f.backend.explicit_power("poweroff", record(), true);
    f.reply("CanPowerOff", string("challenge"));
    f.reply_session(true, false, "wayland");
    let call = f.reply("PowerOff", dbus_error("Operation inhibited"));
    assert!(call.interactive);
    assert_eq!(results.borrow().last(), Some(&false));
    assert!(f.errors.borrow().last().unwrap().contains("Operation inhibited"));
}

#[test]
fn desktop_peripherals_do_not_create_battery_policy() {
    let f = fixture();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 5, "PowerSupply": false, "IsPresent": true, "Percentage": 55.0}),
                    json!({"Type": 12, "PowerSupply": false, "IsPresent": true, "Percentage": 40.0})], false);
    let s = f.state();
    assert!(!s.has_battery && !s.on_battery && s.percentage.is_none() && s.power_source_known);
    assert_eq!(s.batteries.len(), 2);
    let mut percentages: Vec<f64> = s.batteries.iter().map(|b| b["percentage"].as_f64().unwrap()).collect();
    percentages.sort_by(f64::total_cmp);
    assert_eq!(percentages, [40.0, 55.0]);
    assert!(s.batteries.iter().all(|b| b["power_supply"] == false));
}

fn battery_with_limits() -> Value {
    json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "NativePath": "BAT0", "Model": "Laptop battery",
           "Serial": "test-battery", "Percentage": 60.0, "ChargeThresholdSupported": true, "ChargeThresholdEnabled": false,
           "ChargeStartThreshold": 75, "ChargeEndThreshold": 80, "ChargeThresholdSettingsSupported": 3})
}

const TEST0: &str = "/org/freedesktop/UPower/devices/test0";

fn ready_battery_with_limits(f: &Fx) -> Value {
    f.backend.refresh_power();
    let battery = battery_with_limits();
    f.reply_power(std::slice::from_ref(&battery), false);
    battery
}

#[test]
fn charge_limit_waits_for_revalidation_and_readback() {
    let f = fixture();
    let mut battery = ready_battery_with_limits(&f);
    assert!(f.backend.set_charge_thresholds(&json!(TEST0), &json!(true)));
    assert!(f.state().busy);
    assert_eq!(f.state().batteries[0]["busy"], true);
    assert_eq!(f.state().batteries[0]["charge_threshold_enabled"], false);
    assert!(!f.bus.called("EnableChargeThreshold"));
    assert!(!f.backend.set_charge_thresholds(&json!(TEST0), &json!(false)));
    f.bus.reply("GetAll", props(battery.clone()), Some(TEST0), None, 0);
    let write = f.bus.last_pending();
    assert_eq!((write.path.as_str(), write.interface.as_str(), write.method.as_str()), (TEST0, UPOWER_DEVICE, "EnableChargeThreshold"));
    assert_eq!(write.params.unwrap().child_value(0).get::<bool>(), Some(true));
    assert!(write.interactive && write.mutation);
    f.reply("EnableChargeThreshold", unit());
    assert!(f.state().busy);
    battery["ChargeThresholdEnabled"] = true.into();
    f.bus.reply("GetAll", props(battery), Some(TEST0), None, 0);
    assert!(!f.state().busy);
    assert_eq!(f.state().batteries[0]["charge_threshold_enabled"], true);
    assert_eq!(f.state().batteries[0]["busy"], false);
}

#[test]
fn charge_limit_validation_and_unsupported_peripheral_never_mutate() {
    let f = fixture();
    let battery = ready_battery_with_limits(&f);
    for (path, enabled) in [(json!(TEST0), json!(1)), (json!(TEST0), json!("false")), (json!("/missing"), json!(true))] {
        assert!(!f.backend.set_charge_thresholds(&path, &enabled));
    }
    let mut changed = battery.clone();
    changed["PowerSupply"] = false.into();
    f.backend.set_power_device(TEST0, changed);
    assert!(!f.backend.set_charge_thresholds(&json!(TEST0), &json!(true)));
    let mut changed = battery;
    changed["ChargeThresholdSupported"] = false.into();
    f.backend.set_power_device(TEST0, changed);
    assert!(!f.backend.set_charge_thresholds(&json!(TEST0), &json!(true)));
    assert!(!f.bus.called("EnableChargeThreshold"));
}

#[test]
fn charge_limit_hot_unplug_invalidates_pending_validation() {
    let f = fixture();
    let battery = ready_battery_with_limits(&f);
    f.backend.set_charge_thresholds(&json!(TEST0), &json!(true));
    f.backend.power_signal(&device_signal("DeviceRemoved", TEST0));
    f.bus.reply("GetAll", props(battery), Some(TEST0), None, 0);
    assert!(!f.state().busy);
    assert!(f.state().batteries.is_empty());
    assert!(!f.bus.called("EnableChargeThreshold"));
}

#[test]
fn charge_limit_service_loss_invalidates_pending_validation() {
    let f = fixture();
    let battery = ready_battery_with_limits(&f);
    f.backend.set_charge_thresholds(&json!(TEST0), &json!(true));
    f.backend.owner_changed(UPOWER.0);
    f.bus.reply("GetAll", props(battery), Some(TEST0), None, 0);
    assert!(!f.state().busy);
    assert!(f.state().batteries.is_empty());
    assert!(!f.bus.called("EnableChargeThreshold"));
}

#[test]
fn charge_limit_replacement_and_changed_capability_do_not_mutate() {
    for (key, value) in [("Serial", json!("new-battery")), ("ChargeThresholdSupported", json!(false))] {
        let f = fixture();
        let battery = ready_battery_with_limits(&f);
        f.backend.set_charge_thresholds(&json!(TEST0), &json!(true));
        let mut changed = battery.clone();
        changed[key] = value;
        f.bus.reply("GetAll", props(changed), Some(TEST0), None, 0);
        assert!(!f.state().busy);
        assert!(!f.bus.called("EnableChargeThreshold"));
    }
}

#[test]
fn charge_limit_denial_preserves_last_confirmed_setting() {
    let f = fixture();
    let battery = ready_battery_with_limits(&f);
    f.backend.set_charge_thresholds(&json!(TEST0), &json!(true));
    f.bus.reply("GetAll", props(battery), Some(TEST0), None, 0);
    f.reply("EnableChargeThreshold", Err(BusError::remote("org.freedesktop.UPower.Device.NotAuthorized", "Permission denied")));
    assert!(!f.state().busy);
    assert_eq!(f.state().batteries[0]["charge_threshold_enabled"], false);
    assert!(f.errors.borrow().last().unwrap().contains("Authorization"));
}

#[test]
fn charge_limit_unchanged_setting_does_not_write() {
    let f = fixture();
    let battery = ready_battery_with_limits(&f);
    f.backend.set_charge_thresholds(&json!(TEST0), &json!(false));
    f.bus.reply("GetAll", props(battery), Some(TEST0), None, 0);
    assert!(!f.state().busy);
    assert!(!f.bus.called("EnableChargeThreshold"));
}

#[test]
fn charge_limit_readback_detects_unapplied_mutation() {
    let f = fixture();
    let battery = ready_battery_with_limits(&f);
    f.backend.set_charge_thresholds(&json!(TEST0), &json!(true));
    f.bus.reply("GetAll", props(battery.clone()), Some(TEST0), None, 0);
    f.reply("EnableChargeThreshold", unit());
    f.bus.reply("GetAll", props(battery), Some(TEST0), None, 0);
    assert!(!f.state().busy);
    assert_eq!(f.state().batteries[0]["charge_threshold_enabled"], false);
    assert!(f.errors.borrow().last().unwrap().contains("did not apply"));
}

#[test]
fn ups_power_source_and_hotplug_refresh() {
    let f = fixture();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 3, "PowerSupply": true, "IsPresent": true, "State": 2, "Percentage": 80.0})], false);
    assert!(f.state().has_battery && f.state().on_battery);
    f.backend.power_signal(&device_signal("DeviceRemoved", TEST0));
    assert!(!f.state().power_source_known);
    assert!(f.backend.has_source("power"));
    f.backend.refresh_power();
    f.reply_power(&[], false);
    let s = f.state();
    assert!(!s.has_battery && !s.on_battery && s.power_source_known);
}

#[test]
fn charge_and_peripheral_signals_do_not_reset_idle_policy() {
    let f = fixture();
    f.backend.refresh_power();
    f.reply_power(&[json!({"Type": 2, "PowerSupply": true, "IsPresent": true, "Percentage": 80.0})], true);
    for percentage in [79.0f64, 78.0, 77.0] {
        f.backend.power_signal(&properties_changed(TEST0, UPOWER_DEVICE, &[("Percentage", percentage.to_variant())], &[]));
        assert!(f.state().power_source_known && f.state().on_battery);
    }
    f.backend.power_signal(&device_signal("DeviceAdded", "/headset"));
    assert!(f.state().power_source_known);
    f.backend.power_signal(&properties_changed(UPOWER.1, UPOWER.2, &[("OnBattery", false.to_variant())], &[]));
    assert!(!f.state().power_source_known);
}

#[test]
fn failed_device_read_never_assumes_ac() {
    let f = fixture();
    f.backend.refresh_power();
    f.bus.reply("GetAll", props(json!({"OnBattery": false})), Some(UPOWER.1), None, 0);
    f.reply("EnumerateDevices", Ok((vec![glib::variant::ObjectPath::try_from("/battery").unwrap()],).to_variant()));
    f.bus.reply("GetAll", dbus_error("x"), Some("/battery"), None, 0);
    assert!(!f.state().power_source_known && !f.state().power_ready);
}

#[test]
fn stale_profile_reply_cannot_override_newer_read() {
    let f = fixture();
    f.backend.refresh_profiles();
    f.backend.refresh_profiles();
    f.bus.reply("GetAll", props(json!({"Profiles": [{"Profile": "performance"}], "ActiveProfile": "performance"})),
                None, Some(PPD_ENDPOINTS[0].0), 1);
    f.reply_profiles(None, "balanced");
    assert_eq!(f.state().active_profile, "performance");
}

#[test]
fn service_loss_invalidates_state_and_late_replies() {
    let f = fixture();
    f.backend.refresh_profiles();
    f.reply_profiles(None, "balanced");
    f.backend.refresh_profiles();
    f.backend.owner_changed(PPD_ENDPOINTS[0].0);
    f.reply_profiles(None, "balanced");
    assert!(!f.state().profiles_ready);
    assert!(!f.backend.set_profile("performance"));
    f.backend.refresh_profiles();
    f.reply_profiles(None, "balanced");
    assert!(f.state().profiles_ready);
}

#[test]
fn sleep_waits_for_fresh_inhibitors_and_never_bypasses() {
    let f = fixture();
    f.ready_sleep();
    assert!(f.backend.sleep("suspend", false, None));
    assert!(!f.bus.called("Suspend"));
    f.reply("ListInhibitors", inhibitors(&[("idle:sleep", "game", "In game", "block-weak", 1000, 100)]));
    assert!(!f.state().busy);
    assert!(!f.bus.called("Suspend"));
    assert!(f.errors.borrow().last().unwrap().contains("inhibited"));
    assert!(f.backend.sleep("suspend", false, None));
    f.reply("ListInhibitors", inhibitors(&[("sleep", "locker", "Lock before suspend", "delay", 1000, 101)]));
    f.reply_session(true, false, "wayland");
    let action = f.bus.last_pending();
    assert_eq!(action.method, "Suspend");
    assert_eq!(action.params.unwrap().child_value(0).get::<bool>(), Some(false));
    assert!(!action.interactive);
    f.reply("Suspend", unit());
    assert!(!f.state().busy);
}

#[test]
fn auto_sleep_disallows_challenge_and_inhibitor_query_failure() {
    let f = fixture();
    f.backend.refresh_sleep();
    f.reply_sleep("challenge", "yes", "yes");
    assert!(!f.backend.sleep("suspend", false, None));
    assert!(f.backend.sleep("hibernate", false, None));
    f.reply("ListInhibitors", dbus_error("x"));
    assert!(!f.bus.called("Suspend") && !f.bus.called("Hibernate"));
    assert!(!f.state().busy);
}

#[test]
fn logind_loss_while_checking_cancels_sleep() {
    let f = fixture();
    f.ready_sleep();
    f.backend.sleep("suspend", false, None);
    f.backend.owner_changed(LOGIN.0);
    f.reply("ListInhibitors", inhibitors(&[]));
    assert!(!f.bus.called("Suspend"));
    assert!(!f.state().busy);
}

#[test]
fn activity_during_inhibitor_check_cancels_automatic_sleep() {
    let f = fixture();
    f.ready_sleep();
    let idle = Rc::new(Cell::new(true));
    let i = idle.clone();
    f.backend.sleep("suspend", false, Some(Rc::new(move || i.get())));
    idle.set(false);
    f.reply("ListInhibitors", inhibitors(&[]));
    f.reply_session(true, false, "wayland");
    assert!(!f.bus.called("Suspend"));
    assert!(!f.state().busy);
    assert!(f.errors.borrow().is_empty());
}

#[test]
fn idle_guard_fail_closed_and_only_idle_blocks_display() {
    let f = fixture();
    let answers = Rc::new(RefCell::new(Vec::new()));
    let push = |answers: &Rc<RefCell<Vec<bool>>>| -> Box<dyn FnOnce(bool)> { let a = answers.clone(); Box::new(move |v| a.borrow_mut().push(v)) };
    f.backend.idle_inhibited(push(&answers));
    assert_eq!(*answers.borrow(), [true]);
    f.ready_sleep();
    f.backend.idle_inhibited(push(&answers));
    f.reply("ListInhibitors", inhibitors(&[("sleep", "app", "Work", "block", 1000, 10)]));
    f.reply_session(true, false, "wayland");
    assert_eq!(answers.borrow().last(), Some(&false));
    f.backend.idle_inhibited(push(&answers));
    f.reply("ListInhibitors", inhibitors(&[("idle", "movie", "Playback", "block", 1000, 20)]));
    assert_eq!(answers.borrow().last(), Some(&true));
}

#[test]
fn inactive_remote_and_unknown_sessions_block_automatic_sleep() {
    let f = fixture();
    f.ready_sleep();
    for (active, remote, kind) in [(false, false, "wayland"), (true, true, "wayland"), (true, false, "tty")] {
        f.backend.sleep("suspend", false, None);
        f.reply("ListInhibitors", inhibitors(&[]));
        f.reply_session(active, remote, kind);
        assert!(!f.state().busy);
        assert!(!f.bus.called("Suspend"));
    }
    f.backend.sleep("suspend", false, None);
    f.reply("ListInhibitors", inhibitors(&[]));
    f.reply("GetSession", dbus_error("x"));
    assert!(!f.state().busy);
    assert!(f.errors.borrow().is_empty());
    assert!(!f.bus.called("Suspend"));
}

#[test]
fn idle_guard_requires_active_local_graphical_session() {
    let f = fixture();
    f.ready_sleep();
    let answers = Rc::new(RefCell::new(Vec::new()));
    let push = |answers: &Rc<RefCell<Vec<bool>>>| -> Box<dyn FnOnce(bool)> { let a = answers.clone(); Box::new(move |v| a.borrow_mut().push(v)) };
    f.backend.idle_inhibited(push(&answers));
    f.reply("ListInhibitors", inhibitors(&[]));
    assert_eq!(f.bus.last_pending().params.unwrap().child_value(0).str(), Some("auto"));
    assert!(answers.borrow().is_empty());
    f.reply_session(false, false, "wayland");
    assert_eq!(*answers.borrow(), [true]);
    f.backend.idle_inhibited(push(&answers));
    f.reply("ListInhibitors", inhibitors(&[]));
    f.reply("GetSession", Ok((glib::variant::ObjectPath::try_from("/session").unwrap(),).to_variant()));
    f.bus.reply("GetAll", dbus_error("x"), Some("/session"), None, 0);
    assert_eq!(answers.borrow().last(), Some(&true));
    f.backend.idle_inhibited(push(&answers));
    f.reply("ListInhibitors", dbus_error("x"));
    assert_eq!(answers.borrow().last(), Some(&true));
}

#[test]
fn bus_close_invalidates_callbacks_and_schedules_reconnect() {
    let f = fixture();
    f.backend.refresh_profiles();
    f.backend.bus_closed();
    f.reply_profiles(None, "balanced");
    let s = f.state();
    assert!(!s.ready && !s.profiles_ready && !s.power_source_known);
    assert!(f.backend.has_source("connect"));
    assert!(f.bus.cancelled.get() >= 1);
}

#[test]
fn close_suppresses_pending_profile_change_callback() {
    let f = fixture();
    f.backend.refresh_profiles();
    f.reply_profiles(None, "balanced");
    f.backend.set_profile("performance");
    f.backend.close();
    let changed = f.changed.get();
    f.reply("Set", unit());
    assert_eq!(f.changed.get(), changed);
    assert!(!f.state().busy);
    let _ = &f.scheduler;
}

#[test]
fn devices_filter_synthetic_supply_and_absent_batteries() {
    let devices = json!({
        "/org/freedesktop/UPower/devices/DisplayDevice": {"Type": 2, "IsPresent": true},
        "/ac": {"Type": 1, "Online": true},
        "/absent": {"Type": 2, "IsPresent": false},
        "/controller": {"Type": 12, "IsPresent": true, "Model": "Controller", "Percentage": 100.0, "BatteryLevel": 1, "State": 4},
        "/battery": {"Type": 2, "IsPresent": true, "PowerSupply": true, "NativePath": "BAT0", "Percentage": 50.0},
    });
    let result = device_batteries(devices.as_object().unwrap(), None);
    let paths: Vec<&str> = result.iter().map(|d| d["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["/battery", "/controller"]);
    assert_eq!(result[1]["state"], "Fully charged");
    assert_eq!(result[1]["percentage"], 100.0);
    assert!(result[1]["health"].is_null() && result[1]["cycles"].is_null());
    assert_eq!(result[1]["charge_threshold_supported"], false);
    assert_eq!(result[0]["name"], "BAT0");
}

#[test]
fn coarse_unknown_and_invalid_percentages_are_not_fabricated() {
    for (level, expected) in [(0, "Unknown"), (3, "Low"), (4, "Critical"), (8, "Full")] {
        let devices = json!({"/device": {"Type": 5, "BatteryLevel": level, "Percentage": 50.0}});
        let result = &device_batteries(devices.as_object().unwrap(), None)[0];
        assert_eq!(result["level"], expected);
        assert!(result["percentage"].is_null());
    }
    for percentage in [Value::Null, json!(-1), json!(101), json!(true), json!("50")] {
        let devices = json!({"/device": {"Type": 5, "Percentage": percentage}});
        assert!(device_batteries(devices.as_object().unwrap(), None)[0]["percentage"].is_null());
    }
}

#[test]
fn unknown_health_duration_and_cycle_sentinels_are_hidden() {
    let devices = json!({"/device": {"Type": 12, "Capacity": 0.0, "TimeToEmpty": 0, "TimeToFull": -1, "ChargeCycles": -1}});
    let result = &device_batteries(devices.as_object().unwrap(), None)[0];
    for key in ["health", "time_to_empty", "time_to_full", "cycles"] {
        assert!(result[key].is_null(), "{key}");
    }
    let devices = json!({"/device": {"Type": 2, "Capacity": 93.0, "TimeToEmpty": 7200, "TimeToFull": 3600, "ChargeCycles": 123}});
    let result = &device_batteries(devices.as_object().unwrap(), None)[0];
    assert_eq!((result["health"].as_f64(), result["time_to_empty"].as_i64(), result["time_to_full"].as_i64(), result["cycles"].as_i64()),
               (Some(93.0), Some(7200), Some(3600), Some(123)));
}

#[test]
fn firmware_charge_control_does_not_display_dummy_percentages() {
    let mut values = json!({"Type": 2, "IsPresent": true, "PowerSupply": true, "ChargeThresholdSupported": true,
        "ChargeThresholdEnabled": true, "ChargeThresholdSettingsSupported": 4, "ChargeStartThreshold": 0, "ChargeEndThreshold": 100});
    let one = |v: &Value| device_batteries(json!({"/battery": v}).as_object().unwrap(), None)[0].clone();
    let result = one(&values);
    assert_eq!(result["charge_threshold_supported"], true);
    assert!(result["charge_start_threshold"].is_null() && result["charge_end_threshold"].is_null());
    values.as_object_mut().unwrap().remove("ChargeThresholdSettingsSupported");
    values["ChargeStartThreshold"] = json!(4294967295u64);
    values["ChargeEndThreshold"] = json!(80);
    let result = one(&values);
    assert!(result["charge_start_threshold"].is_null());
    assert_eq!(result["charge_end_threshold"], 80);
}

#[test]
fn bluetooth_requires_known_transport_not_wireless_product_name() {
    for (native, kind, expected) in [("/org/bluez/hci0/dev_01", 17, true), ("", 28, true),
                                     ("ps-controller-battery-aa:bb:cc:dd:ee:ff", 12, false)] {
        let devices = json!({"/device": {"Type": kind, "NativePath": native, "Model": "Wireless controller"}});
        assert_eq!(device_batteries(devices.as_object().unwrap(), None)[0]["bluetooth"], expected);
    }
}

#[test]
fn invalid_battery_percentages_are_not_displayed() {
    let b = |v: Value| v;
    assert_eq!(battery_percentage(&[&b(json!({"Percentage": null}))]), None);
    assert_eq!(battery_percentage(&[&b(json!({"Percentage": 101}))]), None);
    assert_eq!(battery_percentage(&[&b(json!({"Percentage": 80})), &b(json!({"Percentage": 20}))]), Some(50.0));
    assert!(system_batteries([&json!({"Type": 5, "PowerSupply": true, "IsPresent": true})]).is_empty());
}

#[test]
fn inhibitor_subjects_and_modes() {
    let row = json!([["handle-lid-switch:sleep", "a", "b", "block", 0, 1]]);
    assert_eq!(inhibited(&row, &["sleep"]), Ok(true));
    assert_eq!(inhibited(&row, &["idle"]), Ok(false));
    assert_eq!(inhibited(&json!([["idle", "a", "b", "delay", 0, 1]]), &["idle"]), Ok(false));
    assert!(inhibited(&json!([["idle"]]), &["idle"]).is_err());
}

// --- Charge-limit backend tests (test_power_charge_limits.BackendTests) ---

struct LimitFx {
    fx: Fx,
    state: Rc<RefCell<Value>>,
    records: Rc<RefCell<Records>>,
}

fn limit_fixture() -> LimitFx {
    let state = Rc::new(RefCell::new(json!({"supported": true, "maximum": 80, "battery_count": 1, "busy": false, "error": "",
                                            "reason": "All laptop batteries"})));
    let records = Rc::new(RefCell::new(Records::from([("/BAT0".to_owned(), Record { name: "BAT0".into(), start: Some(75), stop: 80,
        identity: vec!["sysfs/BAT0".into(), "serial".into()] })])));
    let (s, r) = (state.clone(), records.clone());
    let inspect: Inspector = Box::new(move |devices| {
        if devices.is_empty() {
            let mut state = s.borrow().clone();
            state["supported"] = false.into();
            return (state, Records::new());
        }
        (s.borrow().clone(), r.borrow().clone())
    });
    let fx = fixture_with(Mode::Full, inspect);
    fx.backend.state_mut().power_ready = true;
    fx.backend.set_power_device("/BAT0", json!({"NativePath": "BAT0", "Type": 2, "PowerSupply": true, "IsPresent": true,
        "ChargeThresholdSupported": true, "ChargeEndThreshold": 90, "ChargeStartThreshold": 85}));
    fx.backend.publish_batteries();
    LimitFx { fx, state, records }
}

fn kauth_reply(code: u32, kind: u32) -> Reply {
    let mut blob = charge_limits::encode_arguments(&Default::default()).unwrap();
    blob.extend(code.to_be_bytes());
    blob.extend(kind.to_be_bytes());
    blob.extend(0u32.to_be_bytes());
    Ok((blob,).to_variant())
}

fn action_name(call: &tessel_core::bus::Call) -> String {
    call.params.as_ref().unwrap().child_value(0).get::<String>().unwrap()
}

#[test]
fn refresh_reads_actual_values_without_any_helper_calls() {
    let l = limit_fixture();
    assert!(l.fx.bus.calls.borrow().is_empty());
    assert_eq!(l.fx.state().batteries[0]["charge_limit_adjustable"], true);
    assert_eq!(l.fx.state().batteries[0]["charge_end_threshold"], 80);
    assert!(!l.fx.backend.set_charge_thresholds(&json!("/BAT0"), &json!(true)));
    assert!(l.fx.bus.calls.borrow().is_empty());
}

#[test]
fn battery_only_startup_and_refresh_use_only_upower() {
    let f = fixture_with(Mode::BatteriesOnly, no_limits());
    f.backend.subscribe();
    f.backend.refresh();
    assert!(!f.bus.calls.borrow().is_empty());
    assert!(f.bus.calls.borrow().iter().all(|c| c.name == UPOWER.0));
    let methods: Vec<String> = f.bus.methods().into_iter().collect();
    assert_eq!(methods, ["EnumerateDevices", "GetAll"]);
    assert_eq!(f.bus.signals.borrow().len(), 3);
}

#[test]
fn profiles_only_stream_backend_reads_only_power_profiles() {
    let f = fixture_with(Mode::ProfilesOnly, no_limits());
    f.backend.subscribe();
    f.backend.refresh();
    assert!(f.bus.calls.borrow().iter().all(|c| c.name == PPD_ENDPOINTS[0].0));
    assert_eq!(f.bus.signals.borrow().len(), 4);
}

#[test]
fn apply_revalidates_then_authorizes_then_reads_hardware() {
    let l = limit_fixture();
    let f = &l.fx;
    assert!(f.backend.set_charge_limit(&json!(60)));
    assert_eq!(f.state().charge_limit["busy"], true);
    assert!(action_name(&f.bus.calls.borrow().last().unwrap().clone()).ends_with(".getthreshold"));
    f.reply("performAction", kauth_reply(0, 2));
    let mutation = f.bus.calls.borrow().last().unwrap().clone();
    assert!(action_name(&mutation).ends_with(".setthreshold"));
    let blob: Vec<u8> = mutation.params.as_ref().unwrap().child_value(3).get().unwrap();
    assert_eq!(blob, charge_limits::encode_arguments(&[("chargeStartThreshold".to_owned(), 59), ("chargeStopThreshold".to_owned(), 60)]
        .into_iter().collect()).unwrap());
    assert!(mutation.interactive && mutation.mutation);
    assert_eq!(f.state().charge_limit["maximum"], 80);
    {
        let mut records = l.records.borrow_mut();
        let record = records.get_mut("/BAT0").unwrap();
        record.stop = 60;
        record.start = Some(59);
    }
    l.state.borrow_mut()["maximum"] = 60.into();
    f.reply("performAction", kauth_reply(0, 2));
    assert!(!f.state().busy);
    assert_eq!(f.state().charge_limit["maximum"], 60);
    assert!(f.errors.borrow().is_empty());
}

#[test]
fn cancel_and_unapplied_success_do_not_invent_new_value() {
    let l = limit_fixture();
    let f = &l.fx;
    for result in [kauth_reply(4, 0), kauth_reply(0, 2)] {
        f.backend.set_charge_limit(&json!(60));
        f.reply("performAction", kauth_reply(0, 2));
        f.reply("performAction", result);
        assert!(!f.state().busy);
        assert_ne!(f.state().charge_limit["error"], "");
        assert_eq!(f.state().charge_limit["maximum"], 80);
    }
}

#[test]
fn hotplug_before_authorization_prevents_mutation() {
    let l = limit_fixture();
    let f = &l.fx;
    f.backend.set_charge_limit(&json!(60));
    l.records.borrow_mut().get_mut("/BAT0").unwrap().identity = vec!["replacement".into()];
    f.reply("performAction", kauth_reply(0, 2));
    assert_eq!(f.bus.calls.borrow().len(), 1);
    assert!(f.state().charge_limit["error"].as_str().unwrap().contains("changed"));
}

#[test]
fn validation_unchanged_and_overlap_never_send_extra_mutations() {
    let l = limit_fixture();
    let f = &l.fx;
    for value in [json!(true), json!(0), json!(101), json!(50.5), json!("60")] {
        assert!(!f.backend.set_charge_limit(&value));
    }
    assert!(f.backend.set_charge_limit(&json!(80)));
    assert!(f.bus.calls.borrow().is_empty());
    f.backend.set_charge_limit(&json!(60));
    assert!(!f.backend.set_charge_limit(&json!(50)));
    assert_eq!(f.bus.calls.borrow().len(), 1);
}

#[test]
fn read_transport_failure_and_close_drop_callbacks() {
    let l = limit_fixture();
    let f = &l.fx;
    f.backend.set_charge_limit(&json!(60));
    f.reply("performAction", dbus_error("x"));
    assert!(!f.state().busy);
    assert_ne!(f.state().charge_limit["error"], "");
    f.backend.set_charge_limit(&json!(60));
    f.backend.close();
    f.reply("performAction", kauth_reply(0, 2));
    assert_eq!(f.bus.calls.borrow().len(), 2);
}
