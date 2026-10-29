//! Port of test_ui.py's behaviour checks (AdapterStateTests, MultiAdapterUITests,
//! AdvancedUITests' validation, UITests) against the toolkit-independent
//! manager, with the Python FakeBT (`testing::FakeBluez`) and a recording
//! view. Widget wiring itself is covered by tests/ui_headless.rs.

use super::*;
use crate::env::FakeEnv;
use crate::testing::*;

struct Fx {
    bz: Rc<FakeBluez>,
    m: Rc<Manager>,
    env: Rc<FakeEnv>,
    view: Rc<RecordingView>,
    saved: Rc<RefCell<Vec<String>>>,
}

impl Fx {
    fn with(state: &str, defer: bool, popup: bool) -> Self {
        crate::i18n::set_language("en");
        let bz = FakeBluez::new(state, defer);
        let env = FakeEnv::new();
        let saved: Rc<RefCell<Vec<String>>> = Rc::default();
        let s = saved.clone();
        let services = Services {
            load_receive_mode: Rc::new(|| "ask".into()),
            save_receive_mode: Rc::new(move |mode| { s.borrow_mut().push(mode.into()); Ok(()) }),
            receiver: Rc::new(|_, _| panic!("no OBEX agent in tests")),
            monitor_config: false,
            ..Services::default()
        };
        let m = Manager::new(bz.bt.clone(), env.clone(), popup, "en", services);
        let view = Rc::new(RecordingView::default());
        m.set_view(view.clone());
        m.start();
        m.apply_state();
        let fx = Self { bz, m, env, view, saved };
        fx.drain();
        fx
    }

    fn new() -> Self {
        Self::with("populated", false, false)
    }

    fn drain(&self) {
        self.env.drain();
    }

    fn refresh(&self) {
        self.m.refresh_ui();
        self.drain();
    }

    fn state(&self) -> StateView {
        self.view.state()
    }

    fn add(&self) -> AddView {
        self.state().add.expect("add window state")
    }

    /// `app._rows` (nearby rows vanish with the Add device window).
    fn rows(&self) -> Vec<RowView> {
        let lists = self.view.lists();
        let mut rows: Vec<RowView> = lists.connected.rows.iter().chain(lists.known.rows.iter()).cloned().collect();
        if self.m.add_open() {
            if let Some(nearby) = &lists.nearby {
                rows.extend(nearby.rows.iter().cloned());
            }
        }
        rows
    }

    fn row(&self, path: &str) -> RowView {
        self.rows().into_iter().find(|r| r.path == path).unwrap_or_else(|| panic!("no row {path}"))
    }

    fn has_row(&self, path: &str) -> bool {
        self.rows().iter().any(|r| r.path == path)
    }

    fn group(&self, path: &str) -> &'static str {
        let lists = self.view.lists();
        if lists.connected.rows.iter().any(|r| r.path == path) { return "connected" }
        if lists.known.rows.iter().any(|r| r.path == path) { return "known" }
        if lists.nearby.as_ref().is_some_and(|n| n.rows.iter().any(|r| r.path == path)) { return "nearby" }
        "none"
    }

    fn select(&self, adapter: &str) {
        self.m.adapter_changed(Some(adapter));
        self.refresh();
        assert_eq!(self.m.default_adapter().as_deref(), Some(adapter));
        assert_eq!(self.state().active_adapter.as_deref(), Some(adapter));
    }

    fn complete(&self, method: &str, path: &str, error: Option<BtError>) {
        let first = self.bz.first_pending().expect("pending");
        assert_eq!((fake_method(&first), first.path.clone()), (method.to_owned(), path.to_owned()));
        self.bz.complete(error);
        self.drain();
        assert!(self.bz.discovery_sessions() <= 1);
    }

    fn open_add(&self, complete: bool) {
        assert!(self.m.can_open_add());
        self.m.add_window_opened();
        self.drain();
        assert!(self.m.add_open());
        assert_eq!(self.m.add_adapter(), self.m.default_adapter());
        if complete {
            let adapter = self.m.default_adapter().unwrap();
            self.complete("StartDiscovery", &adapter, None);
        }
    }

    fn close_add(&self) {
        self.m.add_window_destroyed();
        self.drain();
    }

    fn connect_sensitive(&self, path: &str) -> bool {
        let row = self.row(path);
        row.can_act && row.busy.is_empty()
    }

    fn dev(&self, path: &str) -> Device {
        self.bz.bt.device(path).unwrap()
    }

    fn assert_rows_owned_by(&self, adapter: &str) {
        let rows = self.rows();
        assert!(!rows.is_empty());
        for row in rows {
            assert_eq!(device_adapter(&self.dev(&row.path), &row.path), adapter);
        }
    }

    fn objmgr(&self, member: &str, params: glib::Variant) {
        self.m.on_objmgr_signal(&Signal { sender: BLUEZ.into(), path: BLUEZ_ROOT.into(), interface: OBJMGR_IFACE.into(),
                                          member: member.into(), params });
    }

    fn remove_adapter(&self, adapter: &str) {
        self.objmgr("InterfacesRemoved", glib::Variant::tuple_from_iter([crate::dbus::object_path(adapter),
                                                                         vec![ADAPTER_IFACE].to_variant()]));
    }

    fn props_changed(&self, path: &str, iface: &str, props: &[(&str, glib::Variant)]) {
        self.m.on_props_changed(&Signal { sender: BLUEZ.into(), path: path.into(), interface: PROPS_IFACE.into(),
            member: "PropertiesChanged".into(), params: glib::Variant::tuple_from_iter([iface.to_variant(),
                crate::dbus::vardict(props), Vec::<String>::new().to_variant()]) });
    }

    fn trust(&self, path: &str, desired: bool) -> Rc<Cell<Option<bool>>> {
        let reverted = Rc::new(Cell::new(None));
        let r = reverted.clone();
        self.m.trust_toggled(path, &self.dev(path), desired, Box::new(move |v| r.set(Some(v))));
        self.drain();
        reverted
    }
}

const CONNECTED: &str = "/org/bluez/hci0/dev_01";
const KNOWN: &str = "/org/bluez/hci0/dev_02";
const NEARBY: &str = "/org/bluez/hci0/dev_03";
const PHONE: &str = "/org/bluez/hci0/dev_04";

// ------------------------------------------------------------------ AdapterStateTests

fn assert_unavailable(f: &Fx) {
    assert!(!f.state().power_sensitive);
    assert!(!f.state().add_sensitive);
    assert_eq!(f.m.event_text(), "Bluetooth adapter not found");
}

fn assert_available(f: &Fx) {
    assert!(f.state().power_sensitive);
    assert!(f.state().add_sensitive);
    assert_ne!(f.m.event_text(), "Bluetooth adapter not found");
    assert!(!f.m.event_error());
}

#[test]
fn deferred_discovery_clears_startup_adapter_warning() {
    let f = Fx::with("populated", true, false);
    assert_unavailable(&f);
    f.bz.complete_discovery(None, None);
    f.drain();
    assert_available(&f);
    assert!(f.state().power_active);
    assert!(f.bz.history().is_empty());
}

#[test]
fn discovery_without_an_adapter_keeps_controls_disabled() {
    let f = Fx::with("populated", true, false);
    assert_unavailable(&f);
    f.bz.complete_discovery(Some(OMap::default()), Some(OMap::default()));
    f.drain();
    assert_unavailable(&f);
    assert!(f.m.event_error());
    assert!(!f.state().power_active);
    assert!(f.bz.history().is_empty());
}

#[test]
fn adapter_removal_and_return_update_warning_and_controls() {
    let f = Fx::new();
    let adapter = f.bz.bt.adapters.borrow_mut().remove(ADAPTER).unwrap();
    assert_eq!(f.m.default_adapter().as_deref(), Some(ADAPTER));
    f.refresh();
    assert_unavailable(&f);
    f.bz.bt.adapters.borrow_mut().insert(ADAPTER, adapter);
    f.refresh();
    assert_available(&f);
    assert!(f.bz.history().is_empty());
}

#[test]
fn available_adapter_preserves_unrelated_action_error() {
    let f = Fx::new();
    let message = "Error: Connection failed".to_owned();
    f.m.set_event(&message, true, None, None);
    f.refresh();
    assert!(f.state().power_sensitive && f.state().add_sensitive);
    assert_eq!(f.m.event_text(), message);
    assert!(f.m.event_error());
}

#[test]
fn object_manager_adapter_removal_and_addition_update_controls() {
    let f = Fx::new();
    f.remove_adapter(ADAPTER);
    f.drain();
    assert_unavailable(&f);
    let props = crate::dbus::vardict(&[("Powered", true.to_variant()), ("Discovering", false.to_variant()),
                                       ("Name", "My computer".to_variant()), ("Address", "18:56:80:AB:CD:EF".to_variant())]);
    let ifaces = glib::Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(),
                                                          [glib::Variant::from_dict_entry(&ADAPTER_IFACE.to_variant(), &props)]);
    f.objmgr("InterfacesAdded", glib::Variant::tuple_from_iter([crate::dbus::object_path(ADAPTER), ifaces]));
    f.drain();
    assert_available(&f);
    assert_eq!(f.m.default_adapter().as_deref(), Some(ADAPTER));
    assert!(f.bz.history().is_empty());
}

#[test]
fn adapter_only_poll_changes_refresh_availability() {
    let f = Fx::new();
    f.bz.bt.has_object_manager.set(false);
    let adapter = f.bz.bt.adapter(Some(ADAPTER)).unwrap();
    let step = Rc::new(Cell::new(0));
    let (bt, s) = (Rc::downgrade(&f.bz.bt), step.clone());
    f.bz.bt.rescan_hook.replace(Some(Rc::new(Box::new(move |cb, _| {
        let bt = bt.upgrade().unwrap();
        if s.get() == 0 { bt.adapters.borrow_mut().remove(ADAPTER); } else { bt.adapters.borrow_mut().insert(ADAPTER, adapter.clone()); }
        s.set(s.get() + 1);
        cb(vec![ADAPTER.into()]);
    }))));
    f.m.poll_devices();
    f.drain();
    assert_unavailable(&f);
    f.m.poll_devices();
    f.drain();
    assert_available(&f);
    assert!(f.bz.history().is_empty());
}

#[test]
fn startup_polls_without_object_manager() {
    let f = Fx::with("populated", true, false);
    f.bz.conn.reply("GetManagedObjects", Err(BtError::glib("No ObjectManager")));
    // The fallback's introspection of /org/bluez is now pending.
    assert!(f.bz.conn.has_pending("Introspect"));
    f.bz.conn.reply("Introspect", Ok(("<node/>",).to_variant()));
    f.drain();
    assert_eq!(f.env.timers.borrow().clone(), vec![(1, 2000)]);
    assert!(f.m.poll_id().is_some());
    f.m.window_destroyed();
    assert_eq!(*f.env.cancelled.borrow(), vec![1]);
}

// ------------------------------------------------------------------ MultiAdapterUITests

fn multi() -> Fx {
    let f = Fx::with("multi-adapter", false, false);
    f.select(ADAPTER);
    f
}

#[test]
fn duplicate_addresses_are_listed_per_adapter() {
    let f = multi();
    let (first, second) = ("/org/bluez/hci0/dev_02", "/org/bluez/hci1/dev_02");
    assert_eq!(f.dev(first).addr, f.dev(second).addr);
    assert!(f.has_row(first) && !f.has_row(second));
    f.assert_rows_owned_by(ADAPTER);
    f.select(SECOND_ADAPTER);
    assert!(f.has_row(second) && !f.has_row(first));
    f.assert_rows_owned_by(SECOND_ADAPTER);
    assert!(f.bz.history().is_empty());
}

#[test]
fn device_owner_property_and_parent_path_fallback() {
    let f = multi();
    let (_, mut explicit) = device("05", "Explicit owner", SECOND_ADAPTER);
    explicit.paired = true;
    let explicit_path = "/org/bluez/hci0/dev_05";
    f.bz.bt.devices.borrow_mut().insert(explicit_path, explicit);
    let fallback = "/org/bluez/hci1/dev_02";
    f.bz.bt.devices.borrow_mut().get_mut(fallback).unwrap().adapter = String::new();
    f.refresh();
    assert!(!f.has_row(explicit_path));
    f.select(SECOND_ADAPTER);
    assert!(f.has_row(explicit_path) && f.has_row(fallback));
}

#[test]
fn actions_target_only_the_selected_adapters_devices() {
    let f = multi();
    for (adapter, other) in [(ADAPTER, SECOND_ADAPTER), (SECOND_ADAPTER, ADAPTER)] {
        f.select(adapter);
        let path = format!("{adapter}/dev_02");
        let prefix = format!("{other}/");
        let others: Vec<(String, Device)> = f.bz.bt.devices.borrow().iter().filter(|(p, _)| p.starts_with(&prefix))
            .map(|(p, d)| (p.clone(), d.clone())).collect();
        let other_power = f.bz.bt.powered(Some(other));
        f.trust(&path, false);
        f.complete("Set:Trusted", &path, None);
        assert!(!f.dev(&path).trusted);
        f.m.connect_clicked(&path, &f.dev(&path));
        f.drain();
        f.complete("Connect", &path, None);
        assert!(f.dev(&path).connected);
        f.m.forget_clicked(&path, &f.dev(&path));
        f.drain();
        assert_eq!(f.bz.first_pending().unwrap().arg_str(0).as_deref(), Some(path.as_str()));
        f.complete("RemoveDevice", adapter, None);
        assert!(!f.bz.bt.devices.borrow().contains(&path));
        f.m.power_toggled(false);
        f.drain();
        f.complete("Set:Powered", adapter, None);
        assert!(!f.bz.bt.powered(Some(adapter)));
        assert_eq!(f.bz.bt.powered(Some(other)), other_power);
        for (p, d) in others {
            assert_eq!(f.bz.bt.device(&p).unwrap(), d);
        }
    }
}

#[test]
fn forget_uses_device_owner_after_selection_changes() {
    let f = multi();
    let path = "/org/bluez/hci0/dev_02";
    let previous = f.row(path);
    f.select(SECOND_ADAPTER);
    f.m.forget_clicked(&previous.path, &previous.dev);
    f.drain();
    assert_eq!(f.bz.first_pending().unwrap().arg_str(0).as_deref(), Some(path));
    f.complete("RemoveDevice", ADAPTER, None);
    assert!(!f.bz.bt.devices.borrow().contains(path));
    assert!(f.has_row("/org/bluez/hci1/dev_02"));
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
}

#[test]
fn old_power_callback_cannot_change_new_selection() {
    let f = multi();
    for error in [None, Some(gerr("Power change failed"))] {
        f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = true;
        f.bz.bt.adapters.borrow_mut().get_mut(SECOND_ADAPTER).unwrap().powered = false;
        f.select(ADAPTER);
        f.m.power_toggled(false);
        f.drain();
        assert_eq!(f.bz.first_pending().unwrap().path, ADAPTER);
        f.select(SECOND_ADAPTER);
        let failed = error.is_some();
        f.bz.complete(error);
        assert!(!f.state().power_active);
        f.drain();
        assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
        assert!(!f.bz.bt.powered(Some(SECOND_ADAPTER)));
        assert!(!f.state().add_sensitive);
        assert_eq!(f.bz.bt.powered(Some(ADAPTER)), failed);
        assert!(f.bz.pending().is_empty());
        // The old adapter's result never reaches the new selection's event bar.
        assert!(!f.m.event_text().contains("Power change failed"));
    }
}

#[test]
fn power_toggle_ignores_repeats_while_pending_and_shows_result() {
    let f = Fx::new();
    f.m.power_toggled(false);
    f.m.power_toggled(true);
    assert_eq!(f.bz.pending().len(), 1);
    f.drain();
    assert!(!f.state().power_sensitive && !f.state().power_active);
    let call = f.bz.first_pending().unwrap();
    assert_eq!(call.timeout, 15000);
    f.complete("Set:Powered", ADAPTER, None);
    assert_eq!(f.m.event_text(), "Bluetooth is off");
    assert_eq!(f.state().page, "off");
    assert!(f.state().power_sensitive);
}

#[test]
fn device_signals_and_polling_preserve_manual_selection() {
    let f = multi();
    f.select(SECOND_ADAPTER);
    f.props_changed("/org/bluez/hci0/dev_02", DEVICE_IFACE, &[("Alias", "Renamed mouse".to_variant())]);
    f.drain();
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    let props = crate::dbus::vardict(&[("Adapter", crate::dbus::object_path(ADAPTER)), ("Address", "A4:5E:60:12:34:05".to_variant())]);
    let ifaces = glib::Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(),
                                                          [glib::Variant::from_dict_entry(&DEVICE_IFACE.to_variant(), &props)]);
    f.objmgr("InterfacesAdded", glib::Variant::tuple_from_iter([crate::dbus::object_path("/org/bluez/hci0/dev_05"), ifaces]));
    f.drain();
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    f.bz.bt.has_object_manager.set(false);
    f.bz.bt.rescan_hook.replace(Some(Rc::new(Box::new(|cb, _| cb(vec![ADAPTER.into()])))));
    f.m.poll_devices();
    f.drain();
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    f.assert_rows_owned_by(SECOND_ADAPTER);
    assert!(f.bz.history().is_empty());
}

#[test]
fn unselected_removal_preserves_selection_and_selected_removal_falls_back() {
    let f = multi();
    let first = f.bz.bt.adapter(Some(ADAPTER)).unwrap();
    f.select(SECOND_ADAPTER);
    f.remove_adapter(ADAPTER);
    f.drain();
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    f.bz.bt.adapters.borrow_mut().insert(ADAPTER, first);
    f.refresh();
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    f.remove_adapter(SECOND_ADAPTER);
    f.drain();
    assert_eq!(f.m.default_adapter().as_deref(), Some(ADAPTER));
    assert_eq!(f.state().active_adapter.as_deref(), Some(ADAPTER));
    assert!(f.state().power_sensitive);
}

#[test]
fn switch_during_scan_start_releases_old_session_before_new_start() {
    let f = multi();
    f.open_add(false);
    f.select(SECOND_ADAPTER);
    assert!(f.m.add_open());
    assert_eq!(f.m.add_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert_eq!(f.bz.pending().len(), 1);
    f.complete("StartDiscovery", ADAPTER, None);
    f.complete("StopDiscovery", ADAPTER, None);
    f.complete("StartDiscovery", SECOND_ADAPTER, None);
    assert_eq!(f.m.scan_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert_eq!(f.bz.sessions(ADAPTER), Some(0));
    f.assert_rows_owned_by(SECOND_ADAPTER);
}

#[test]
fn switch_during_active_scan_retargets_single_popup() {
    let f = multi();
    f.open_add(true);
    f.select(SECOND_ADAPTER);
    f.complete("StopDiscovery", ADAPTER, None);
    f.complete("StartDiscovery", SECOND_ADAPTER, None);
    assert_eq!(f.m.add_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert_eq!(f.m.scan_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(f.m.scan_active());
    f.assert_rows_owned_by(SECOND_ADAPTER);
}

#[test]
fn failed_old_scan_start_does_not_cancel_new_adapter_request() {
    let f = multi();
    f.open_add(false);
    f.select(SECOND_ADAPTER);
    f.complete("StartDiscovery", ADAPTER, Some(gerr("Old adapter unavailable")));
    f.complete("StartDiscovery", SECOND_ADAPTER, None);
    assert_eq!(f.m.scan_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(f.m.scan_active());
    assert_eq!(f.bz.discovery_sessions(), 1);
}

#[test]
fn removed_adapter_start_failure_before_refresh_scans_fallback() {
    let f = multi();
    f.open_add(false);
    f.remove_adapter(ADAPTER);
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    // Complete the old call before the deferred state refresh runs.
    f.bz.complete(Some(gerr("Old adapter removed")));
    f.drain();
    assert_eq!(f.bz.pending(), vec![("StartDiscovery".to_owned(), SECOND_ADAPTER.to_owned())]);
    f.complete("StartDiscovery", SECOND_ADAPTER, None);
    assert_eq!(f.m.add_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert_eq!(f.m.scan_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(f.m.scan_active());
}

#[test]
fn fallback_scan_error_before_refresh_remains_visible() {
    let f = multi();
    f.open_add(false);
    f.remove_adapter(ADAPTER);
    f.bz.complete(Some(gerr("Old adapter removed")));
    assert_eq!(f.bz.pending(), vec![("StartDiscovery".to_owned(), SECOND_ADAPTER.to_owned())]);
    // Both D-Bus callbacks arrive before the queued state refresh.
    f.bz.complete(Some(gerr("Fallback discovery failed")));
    f.drain();
    assert_eq!(f.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(!f.m.scan_active() && !f.m.scan_pending());
    assert!(f.m.event_text().contains("Fallback discovery failed"));
    assert!(f.m.add_event_visible());
    assert!(f.view.last_event().unwrap().text.contains("Fallback discovery failed"));
    assert!(f.m.event_error());
}

#[test]
fn failed_old_scan_stop_never_starts_a_second_session() {
    let f = multi();
    f.open_add(true);
    f.select(SECOND_ADAPTER);
    f.complete("StopDiscovery", ADAPTER, Some(gerr("Stop failed")));
    assert!(f.bz.pending().is_empty());
    assert_eq!(f.bz.sessions(ADAPTER), Some(1));
    assert_eq!(f.bz.sessions(SECOND_ADAPTER).unwrap_or(0), 0);
    assert_eq!(f.m.add_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(!f.add().scanning);
    // The failed stop on the old adapter is reported with its label.
    assert!(f.m.event_text().starts_with("Built-in Bluetooth · hci0 · 18:56:80:AB:CD:EF: "), "{}", f.m.event_text());
}

#[test]
fn switch_during_requested_stop_does_not_restart_scanning() {
    let f = multi();
    f.open_add(true);
    f.m.scan_clicked();
    f.drain();
    f.select(SECOND_ADAPTER);
    f.complete("StopDiscovery", ADAPTER, None);
    assert!(f.bz.pending().is_empty());
    assert_eq!(f.m.add_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(!f.m.scan_active());
    assert_eq!(f.bz.discovery_sessions(), 0);
    assert!(!f.add().scanning);
}

#[test]
fn switch_to_powered_off_adapter_releases_previous_scan() {
    let f = multi();
    f.open_add(true);
    f.bz.bt.adapters.borrow_mut().get_mut(SECOND_ADAPTER).unwrap().powered = false;
    f.select(SECOND_ADAPTER);
    f.complete("StopDiscovery", ADAPTER, None);
    assert!(f.bz.pending().is_empty());
    assert_eq!(f.bz.discovery_sessions(), 0);
    assert!(!f.m.scan_active());
    assert!(!f.add().scan_sensitive);
    assert_eq!(f.m.add_adapter().as_deref(), Some(SECOND_ADAPTER));
}

#[test]
fn reopening_add_window_uses_selected_adapter_and_displays_owner() {
    let f = multi();
    f.open_add(true);
    f.close_add();
    f.complete("StopDiscovery", ADAPTER, None);
    f.select(SECOND_ADAPTER);
    f.open_add(true);
    assert_eq!(f.m.add_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert_eq!(f.m.scan_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(f.add().owner_label.contains("USB Bluetooth"));
    f.assert_rows_owned_by(SECOND_ADAPTER);
}

// ------------------------------------------------------------------ AdvancedUITests (validation)

fn advanced() -> (Fx, String) {
    let f = multi();
    let path = "/org/bluez/hci0/dev_02".to_owned();
    f.bz.bt.devices.borrow_mut().get_mut(&path).unwrap().uuids = vec![OBJECT_PUSH_UUID.into()];
    f.refresh();
    (f, path)
}

#[test]
fn transfer_availability_tracks_services_power_and_busy_state() {
    let (f, path) = advanced();
    let item_sensitive = |f: &Fx| { let r = f.row(&path); r.can_act && r.busy.is_empty() && r.supports_push };
    assert!(item_sensitive(&f));
    f.bz.bt.devices.borrow_mut().get_mut(&path).unwrap().uuids = vec!["0000110b-0000-1000-8000-00805f9b34fb".into()];
    f.refresh();
    assert!(!item_sensitive(&f));
    f.bz.bt.devices.borrow_mut().get_mut(&path).unwrap().uuids = vec![];
    f.refresh();
    assert!(item_sensitive(&f));
    f.m.mark_busy(&path, "connect");
    f.drain();
    assert!(!item_sensitive(&f));
    f.m.unmark_busy(&path, "connect");
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = false;
    f.refresh();
    assert!(!item_sensitive(&f));
    // Object Push is matched case-insensitively.
    assert!(supports_object_push(&Device { uuids: vec![OBJECT_PUSH_UUID.to_uppercase()], ..Default::default() }));
}

#[test]
fn send_targets_keep_original_owner_and_revalidate() {
    let (f, path) = advanced();
    let target = f.m.send_snapshot(&path, &f.dev(&path));
    assert_eq!(target.source_address.as_deref(), Some("18:56:80:AB:CD:EF"));
    f.select(SECOND_ADAPTER);
    // Switching the selected adapter does not invalidate the transfer.
    assert_eq!(f.m.send_error(&target), None);
    f.m.mark_busy(&path, "connect");
    assert_eq!(f.m.send_error(&target).as_deref(), Some("Device is busy. Try again shortly."));
    f.m.unmark_busy(&path, "connect");
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = false;
    assert_eq!(f.m.send_error(&target).as_deref(), Some("Bluetooth is off"));
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = true;
    f.bz.bt.devices.borrow_mut().get_mut(&path).unwrap().uuids = vec!["0000110b-0000-1000-8000-00805f9b34fb".into()];
    assert_eq!(f.m.send_error(&target).as_deref(), Some("This device does not support file transfers."));
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().address = "00:00:00:00:00:01".into();
    assert_eq!(f.m.send_error(&target).as_deref(), Some("The device is no longer available."));
    f.bz.bt.devices.borrow_mut().remove(&path);
    assert_eq!(f.m.send_error(&target).as_deref(), Some("The device is no longer available."));
    let orphan = SendTarget { path: "/org/bluez/hci9/dev_x".into(), dev: Device::default(), source_address: None };
    assert_eq!(f.m.send_error(&orphan).as_deref(), Some("The device is no longer available."));
}

// ------------------------------------------------------------------ UITests

#[test]
fn main_window_shows_only_saved_devices() {
    let f = Fx::new();
    let paths: std::collections::BTreeSet<String> = f.rows().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, [CONNECTED, KNOWN].iter().map(|s| s.to_string()).collect());
    assert!(f.view.lists().nearby.is_none() && !f.m.add_open());
    assert!(f.state().add_sensitive);
    assert!(f.bz.history().is_empty());
}

#[test]
fn add_window_automatically_scans_once() {
    let f = Fx::new();
    f.open_add(false);
    assert_eq!(f.bz.first_pending().unwrap().method, "StartDiscovery");
    assert!(!f.add().scan_sensitive);
    f.m.scan_clicked();
    assert_eq!(f.bz.pending().len(), 1);
    f.bz.complete(None);
    f.drain();
    assert!(f.m.scan_active() && f.add().scan_sensitive && f.add().scanning);
    assert_eq!(f.group(NEARBY), "nearby");
}

#[test]
fn closing_popup_releases_discovery_and_nearby_rows() {
    let f = Fx::new();
    f.open_add(true);
    f.close_add();
    assert!(!f.m.add_open() && !f.has_row(NEARBY));
    assert_eq!(f.bz.first_pending().unwrap().method, "StopDiscovery");
    f.bz.complete(None);
    f.drain();
    assert!(!f.m.scan_active());
    assert_eq!(f.bz.discovery_sessions(), 0);
}

#[test]
fn closing_during_start_releases_session_after_completion() {
    let f = Fx::new();
    f.open_add(false);
    f.close_add();
    assert_eq!(f.bz.pending(), vec![("StartDiscovery".to_owned(), ADAPTER.to_owned())]);
    f.bz.complete(None);
    f.drain();
    assert_eq!(f.bz.pending(), vec![("StopDiscovery".to_owned(), ADAPTER.to_owned())]);
    f.bz.complete(None);
    f.drain();
    assert!(!f.m.add_open() && !f.m.scan_active());
    assert_eq!(f.bz.discovery_sessions(), 0);
}

#[test]
fn reopening_popup_acquires_a_new_discovery_session() {
    let f = Fx::new();
    f.open_add(true);
    f.close_add();
    f.bz.complete(None);
    f.drain();
    f.open_add(true);
    assert!(f.m.scan_active());
    assert_eq!(f.bz.discovery_sessions(), 1);
}

#[test]
fn reopening_during_stop_starts_after_completion() {
    let f = Fx::new();
    f.open_add(true);
    f.close_add();
    assert_eq!(f.bz.first_pending().unwrap().method, "StopDiscovery");
    f.open_add(false);
    assert_eq!(f.bz.pending().len(), 1);
    f.bz.complete(None);
    f.drain();
    assert_eq!(f.bz.first_pending().unwrap().method, "StartDiscovery");
    f.bz.complete(None);
    f.drain();
    assert!(f.m.add_open() && f.m.scan_active());
    assert_eq!(f.bz.discovery_sessions(), 1);
}

#[test]
fn external_discovery_does_not_replace_our_session() {
    let f = Fx::new();
    f.bz.external_discovery.set(true);
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().discovering = true;
    f.open_add(true);
    assert_eq!(f.bz.discovery_sessions(), 1);
    f.m.scan_clicked();
    f.drain();
    assert_eq!(f.bz.first_pending().unwrap().method, "StopDiscovery");
    f.bz.complete(None);
    f.drain();
    assert!(!f.m.scan_active());
    assert!(f.bz.bt.adapter(Some(ADAPTER)).unwrap().discovering);
    assert!(!f.add().scanning);
    assert_eq!(f.add().status, "Scan stopped");
    f.close_add();
    assert!(f.bz.pending().is_empty());
}

#[test]
fn popup_controls_disable_when_radio_becomes_unavailable() {
    let f = Fx::new();
    f.open_add(true);
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = false;
    f.refresh();
    assert!(!f.state().add_sensitive);
    assert!(!f.add().scan_sensitive);
    for row in f.rows() {
        assert!(!(row.can_act && row.busy.is_empty()));
    }
    // Power loss releases our session without a StopDiscovery call.
    assert!(f.bz.pending().is_empty() && !f.m.scan_active());
    f.bz.bt.adapters.borrow_mut().clear();
    f.m.pick_default_adapter();
    f.refresh();
    assert!(!f.state().power_sensitive && !f.state().add_sensitive && !f.add().scan_sensitive);
    assert_eq!(f.add().owner_label, "Bluetooth adapter not found");
    assert_eq!(f.add().status, "Bluetooth adapter not found");
}

#[test]
fn grouping_and_device_changes() {
    let f = Fx::new();
    f.open_add(true);
    assert_eq!((f.group(CONNECTED), f.group(KNOWN), f.group(NEARBY)), ("connected", "known", "nearby"));
    f.bz.bt.devices.borrow_mut().get_mut(KNOWN).unwrap().connected = true;
    f.bz.bt.devices.borrow_mut().get_mut(NEARBY).unwrap().trusted = true;
    f.bz.bt.devices.borrow_mut().remove(PHONE);
    f.refresh();
    assert_eq!((f.group(KNOWN), f.group(NEARBY)), ("connected", "known"));
    assert!(!f.has_row(PHONE));
    assert!(f.bz.history().iter().all(|c| c.method == "StartDiscovery"));
}

#[test]
fn rows_are_sorted_by_casefolded_alias_and_counted() {
    let f = Fx::new();
    let (p, mut d) = device("09", "apple", ADAPTER);
    d.paired = true;
    f.bz.bt.devices.borrow_mut().insert(&p, d);
    let (p2, mut d2) = device("08", "Zebra", ADAPTER);
    d2.paired = true;
    f.bz.bt.devices.borrow_mut().insert(&p2, d2);
    f.refresh();
    let known = f.view.lists().known;
    let names: Vec<String> = known.rows.iter().map(|r| r.dev.alias.clone()).collect();
    assert_eq!(names, vec!["apple", "MX Master 3S", "Zebra"]);
    assert_eq!(known.count, 3);
    assert_eq!(known.title, "Known devices");
}

#[test]
fn unrelated_refresh_keeps_row_views_equal() {
    let f = Fx::new();
    let row = f.row(KNOWN);
    f.bz.bt.devices.borrow_mut().get_mut(PHONE).unwrap().alias = "A renamed phone".into();
    f.refresh();
    assert_eq!(f.row(KNOWN), row);
    assert!(f.bz.history().is_empty());
}

#[test]
fn backend_dictionary_replacement_keeps_actions_current() {
    let f = Fx::new();
    f.open_add(true);
    let original = f.row(NEARBY);
    let replacement = f.dev(NEARBY);
    f.bz.bt.devices.borrow_mut().insert(NEARBY, replacement);
    f.refresh();
    assert_eq!(f.row(NEARBY), original);
    f.bz.bt.devices.borrow_mut().get_mut(NEARBY).unwrap().paired = true;
    f.refresh();
    assert_eq!(f.group(NEARBY), "known");
    let row = f.row(NEARBY);
    f.m.connect_clicked(&row.path, &row.dev);
    f.drain();
    assert_eq!(f.bz.first_pending().unwrap().method, "Connect");
    f.bz.complete(None);
    f.drain();
    assert_eq!(f.group(NEARBY), "connected");
}

#[test]
fn power_off_disables_device_actions() {
    let f = Fx::new();
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = false;
    f.refresh();
    assert!(!f.state().power_active && !f.state().add_sensitive);
    for path in [CONNECTED, KNOWN] {
        assert!(!f.connect_sensitive(path));
    }
    assert_eq!(f.state().page, "off");
    assert_eq!(f.state().off_title, "Bluetooth is off");
    assert!(f.state().off_description_visible);
    assert_eq!(f.state().ctrl_text, "Bluetooth is off");
    assert_eq!(f.m.event_text(), "Bluetooth is off");
    assert!(f.bz.history().is_empty());
}

#[test]
fn missing_adapter_disables_controls() {
    let f = Fx::new();
    f.bz.bt.adapters.borrow_mut().clear();
    f.m.pick_default_adapter();
    f.refresh();
    assert!(!f.state().power_sensitive && !f.state().add_sensitive);
    assert!(f.rows().is_empty());
    assert_eq!(f.state().ctrl_text, "Bluetooth");
    assert_eq!(f.state().status_text, "Bluetooth adapter not found");
    assert!(!f.state().off_description_visible);
    assert_eq!(f.state().adapter_tooltip, "Bluetooth adapter not found");
}

#[test]
fn scan_can_start_and_stop() {
    let f = Fx::new();
    f.open_add(false);
    f.bz.complete(None);
    f.drain();
    assert!(f.bz.bt.adapter(Some(ADAPTER)).unwrap().discovering);
    assert!(f.add().scanning);
    assert_eq!(f.add().status, "Scanning for nearby devices…");
    assert_eq!(f.view.lists().nearby.unwrap().empty_text, "Scanning for nearby devices…");
    f.m.scan_clicked();
    f.drain();
    assert_eq!(f.bz.first_pending().unwrap().method, "StopDiscovery");
    f.bz.complete(None);
    f.drain();
    assert!(!f.bz.bt.adapter(Some(ADAPTER)).unwrap().discovering);
    assert!(!f.add().scanning);
    assert_eq!(f.add().status, "Scan stopped");
    f.m.scan_clicked();
    f.drain();
    assert_eq!(f.bz.first_pending().unwrap().method, "StartDiscovery");
    f.bz.complete(None);
    f.drain();
    assert!(f.m.scan_active());
}

#[test]
fn failed_scan_restores_idle_controls() {
    let f = Fx::new();
    f.open_add(false);
    assert!(!f.add().scan_sensitive);
    f.bz.complete(Some(gerr("Discovery failed")));
    f.drain();
    assert!(!f.bz.bt.adapter(Some(ADAPTER)).unwrap().discovering);
    assert!(f.add().scan_sensitive && !f.add().scanning);
    assert_eq!(f.add().status, "Scan stopped");
    assert!(f.m.event_text().contains("Discovery failed"));
    assert!(f.m.add_event_visible());
    assert!(f.view.last_event().unwrap().tooltip.contains("Details: Discovery failed"));
}

#[test]
fn connect_pairs_and_updates_group() {
    let f = Fx::new();
    f.open_add(true);
    let row = f.row(NEARBY);
    f.m.connect_clicked(&row.path, &row.dev);
    f.drain();
    assert!(!f.connect_sensitive(NEARBY));
    assert_eq!(f.row(NEARBY).busy, vec!["connect"]);
    let pair = f.bz.first_pending().unwrap();
    assert_eq!((pair.method.as_str(), pair.timeout), ("Pair", 60000));
    f.bz.complete(None);
    assert_eq!(f.bz.first_pending().unwrap().method, "Connect");
    f.bz.complete(None);
    f.drain();
    assert_eq!(f.group(NEARBY), "connected");
    assert!(f.connect_sensitive(NEARBY));
    assert_eq!(f.m.event_text(), "Connected to Magic Keyboard");
}

#[test]
fn failed_pairing_clears_busy_state_without_connecting() {
    let f = Fx::new();
    f.open_add(true);
    let row = f.row(NEARBY);
    f.m.connect_clicked(&row.path, &row.dev);
    f.bz.complete(Some(BtError::remote("org.bluez.Error.AuthenticationFailed", "Authentication Failed")));
    f.drain();
    assert!(f.bz.pending().is_empty());
    assert!(f.connect_sensitive(NEARBY));
    assert!(f.m.event_text().contains("check the PIN"));
}

#[test]
fn failed_connection_clears_busy_state() {
    let f = Fx::new();
    f.m.connect_clicked(KNOWN, &f.dev(KNOWN));
    f.drain();
    assert!(!f.connect_sensitive(KNOWN));
    f.m.connect_clicked(KNOWN, &f.dev(KNOWN));
    assert_eq!(f.bz.pending().len(), 1);
    f.bz.complete(Some(gerr("Connection failed")));
    f.drain();
    assert!(f.connect_sensitive(KNOWN));
    assert!(f.m.event_text().contains("Connection failed"));
}

#[test]
fn disconnect_updates_group() {
    let f = Fx::new();
    f.m.disconnect_clicked(CONNECTED, &f.dev(CONNECTED));
    f.drain();
    assert_eq!(f.bz.first_pending().unwrap().method, "Disconnect");
    assert!(!f.connect_sensitive(CONNECTED));
    f.bz.complete(None);
    f.drain();
    assert_eq!(f.group(CONNECTED), "known");
}

#[test]
fn trust_action_and_busy_state() {
    let f = Fx::new();
    let reverted = f.trust(KNOWN, false);
    assert_eq!(fake_method(&f.bz.first_pending().unwrap()), "Set:Trusted");
    assert!(!f.connect_sensitive(KNOWN));
    f.bz.complete(None);
    f.drain();
    assert_eq!(f.group(KNOWN), "known");
    assert!(!f.dev(KNOWN).trusted);
    assert_eq!(reverted.get(), None);
    assert_eq!(f.m.event_text(), "MX Master 3S is no longer trusted");
}

#[test]
fn trust_switch_reverts_on_error() {
    let f = Fx::new();
    let reverted = f.trust(KNOWN, false);
    f.bz.complete(Some(gerr("Trust refused")));
    f.drain();
    assert_eq!(reverted.get(), Some(true));
    assert!(f.dev(KNOWN).trusted);
    assert!(f.m.event_error());
}

#[test]
fn forget_removes_device() {
    let f = Fx::new();
    f.m.forget_clicked(KNOWN, &f.dev(KNOWN));
    f.drain();
    assert!(f.row(KNOWN).forgetting);
    let call = f.bz.first_pending().unwrap();
    assert_eq!((call.method.as_str(), call.timeout), ("RemoveDevice", 20000));
    f.bz.complete(None);
    f.drain();
    assert!(!f.has_row(KNOWN));
    assert!(f.m.forgetting().is_empty());
    assert_eq!(f.m.event_text(), "Device MX Master 3S forgotten");
}

#[test]
fn forget_falls_back_to_unpair_when_remove_device_fails() {
    let f = Fx::new();
    f.m.forget_clicked(KNOWN, &f.dev(KNOWN));
    f.bz.complete(Some(gerr("Not supported")));
    let unpair = f.bz.first_pending().unwrap();
    assert_eq!((unpair.method.as_str(), unpair.path.as_str(), unpair.timeout), ("Unpair", KNOWN, 20000));
    f.bz.conn.reply("Unpair", Err(gerr("Still failing")));
    f.drain();
    assert!(f.m.forgetting().is_empty());
    assert!(f.m.event_text().contains("Still failing"));
}

#[test]
fn shared_language_does_not_read_bluetooth_override() {
    // The app's language is suite_core's selected language (language.json).
    let f = Fx::new();
    assert_eq!(f.m.lang, "en");
    assert!(f.bz.history().is_empty());
}

#[test]
fn battery_level_is_shown_and_follows_property_changes() {
    let f = Fx::new();
    f.props_changed(CONNECTED, BATTERY_IFACE, &[("Percentage", 76u8.to_variant())]);
    f.drain();
    assert_eq!(f.row(CONNECTED).dev.battery, Some(76));
    f.props_changed(CONNECTED, BATTERY_IFACE, &[("Percentage", 15u8.to_variant())]);
    f.drain();
    assert_eq!(f.row(CONNECTED).dev.battery, Some(15));
    assert_eq!(f.row(KNOWN).dev.battery, None);
}

#[test]
fn connection_signals_announce_changes_for_the_selected_adapter_only() {
    let f = multi();
    f.props_changed("/org/bluez/hci0/dev_02", DEVICE_IFACE, &[("Connected", true.to_variant())]);
    assert_eq!(f.m.event_text(), "Connected to MX Master 3S");
    f.props_changed("/org/bluez/hci1/dev_02", DEVICE_IFACE, &[("Connected", true.to_variant())]);
    assert_eq!(f.m.event_text(), "Connected to MX Master 3S");
    f.props_changed("/org/bluez/hci0/dev_02", DEVICE_IFACE, &[("Connected", false.to_variant()), ("Alias", "".to_variant()),
                                                                ("UUIDs", vec![OBJECT_PUSH_UUID].to_variant()), ("Class", 0x200404u32.to_variant())]);
    assert_eq!(f.m.event_text(), "Disconnected from MX Master 3S");
    let dev = f.dev("/org/bluez/hci0/dev_02");
    assert_eq!((dev.alias.as_str(), dev.class, dev.uuids.len()), ("A4:5E:60:12:34:02", 0x200404, 1));
    f.props_changed(ADAPTER, ADAPTER_IFACE, &[("Alias", "Renamed".to_variant()), ("Powered", false.to_variant())]);
    let ad = f.bz.bt.adapter(Some(ADAPTER)).unwrap();
    assert_eq!((ad.alias.as_str(), ad.powered), ("Renamed", false));
}

#[test]
fn bluez_errors_show_plain_advice_with_details() {
    let f = Fx::new();
    f.m.connect_clicked(KNOWN, &f.dev(KNOWN));
    f.bz.complete(Some(BtError::dbus(gio::DBusError::Failed, "GDBus.Error:org.bluez.Error.Failed: br-connection-page-timeout")));
    f.drain();
    let text = f.m.event_text();
    assert!(text.contains("not responding") && !text.contains("br-connection"));
    let event = f.view.last_event().unwrap();
    assert!(event.tooltip.contains("br-connection-page-timeout"));
    assert!(event.error && f.m.event_error());
}

#[test]
fn stale_event_is_replaced_when_selection_changes() {
    let f = multi();
    f.m.set_event("Error: Something", true, None, None);
    f.select(SECOND_ADAPTER);
    assert_eq!(f.m.event_text(), HINT_ADD.replace('\\', ""));
    assert!(!f.m.event_error());
}

#[test]
fn adapter_selector_lists_sorted_labels_and_tracks_changes() {
    let f = multi();
    let state = f.state();
    assert_eq!(state.adapter_choices, vec![
        (ADAPTER.to_owned(), "Built-in Bluetooth · hci0 · 18:56:80:AB:CD:EF".to_owned()),
        (SECOND_ADAPTER.to_owned(), "USB Bluetooth · hci1 · 24:6F:28:12:34:56".to_owned()),
    ]);
    assert!(state.adapter_sensitive);
    f.refresh();
    assert!(!f.state().choices_changed);
    assert_eq!(f.state().status_text, "Controller: Built-in Bluetooth");
    f.m.adapter_changed(Some("/org/bluez/hci7"));
    assert_eq!(f.m.default_adapter().as_deref(), Some(ADAPTER));
}

#[test]
fn receive_setting_saves_policy_without_starting_agent() {
    let f = Fx::new();
    assert_eq!(f.m.receive_mode(), "ask");
    assert!(f.m.receive_mode_selected(0).is_ok());
    assert_eq!(*f.saved.borrow(), vec!["off"]);
    assert_eq!(f.m.receive_mode(), "off");
    assert!(!f.m.has_receiver());
    assert!(f.m.receive_mode_selected(0).is_ok());
    assert_eq!(f.saved.borrow().len(), 1);
}

#[test]
fn receive_setting_save_failure_reports_and_keeps_mode() {
    let bz = FakeBluez::new("populated", false);
    let env = FakeEnv::new();
    let services = Services { load_receive_mode: Rc::new(|| "ask".into()), monitor_config: false,
        save_receive_mode: Rc::new(|_| Err("[Errno 13] Permission denied: '/ro/btmanager/settings.ini.tmp'".into())), ..Services::default() };
    let m = Manager::new(bz.bt.clone(), env, false, "en", services);
    m.set_view(Rc::new(RecordingView::default()));
    assert!(m.receive_mode_selected(0).is_err());
    assert_eq!(m.receive_mode(), "ask");
    assert_eq!(m.event_text(), "Error: '/ro/btmanager/settings.ini.tmp'");
}

// ------------------------------------------------------------------ receiving

#[derive(Default)]
struct FakeNotifier {
    shown: RefCell<Vec<(String, String, Vec<String>, u32, Option<i64>, i32, u8, Option<Rc<dyn Fn(&str)>>, Option<Rc<dyn Fn(u32)>>)>>,
    closed: RefCell<Vec<u32>>,
    actions: Cell<bool>,
}

impl crate::transfer::Notify for FakeNotifier {
    fn supports_actions(&self) -> bool { self.actions.get() }
    fn notify(&self, n: crate::transfer::Notification) {
        let ident = if n.replaces != 0 { n.replaces } else { self.shown.borrow().len() as u32 + 1 };
        self.shown.borrow_mut().push((n.summary, n.body, n.actions.iter().map(|(k, _)| k.clone()).collect(), n.replaces, n.value,
                                      n.timeout, n.urgency, n.on_action, n.on_closed));
        if let Some(shown) = n.on_shown {
            shown(ident);
        }
    }
    fn close(&self, id: u32) { self.closed.borrow_mut().push(id) }
    fn destroy(&self) {}
}

fn receive_fx(download: &str) -> (Fx, Rc<FakeNotifier>, Rc<RefCell<Vec<(PathBuf, bool)>>>) {
    crate::i18n::set_language("en");
    let bz = FakeBluez::new("populated", false);
    let env = FakeEnv::new();
    let opened: Rc<RefCell<Vec<(PathBuf, bool)>>> = Rc::default();
    let o = opened.clone();
    let folder = PathBuf::from(download);
    let services = Services {
        load_receive_mode: Rc::new(|| "ask".into()), monitor_config: false,
        download_dir: Rc::new(move || folder.clone()),
        open_received: Some(Rc::new(move |p, f| o.borrow_mut().push((p.to_path_buf(), f)))),
        ..Services::default()
    };
    let m = Manager::new(bz.bt.clone(), env.clone(), false, "en", services);
    let view = Rc::new(RecordingView::default());
    m.set_view(view.clone());
    m.start();
    env.drain();
    let notifier = Rc::new(FakeNotifier { actions: Cell::new(true), ..Default::default() });
    m.set_notifier(Some(notifier.clone()));
    (Fx { bz, m, env, view, saved: Rc::default() }, notifier, opened)
}

fn temp_folder() -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("btmanager-downloads-{}-{}", std::process::id(), suite_core::secrets::token_urlsafe(6)))
}

#[test]
fn incoming_file_prompt_accept_progress_and_completion() {
    let folder = temp_folder();
    let (f, notifier, opened) = receive_fx(folder.to_str().unwrap());
    let request = ReceiveRequest::detached("photo.jpg", 2048, &f.dev(CONNECTED).addr.to_lowercase());
    f.m.on_receive_request(&request);
    let prompt = notifier.shown.borrow().last().map(|s| (s.1.clone(), s.2.clone(), s.5, s.6, s.7.clone(), s.8.clone())).unwrap();
    assert!(prompt.0.contains("WH-1000XM5") && prompt.0.contains("photo.jpg") && prompt.0.contains(&glib::format_size(2048).to_string()));
    assert_eq!(prompt.1, vec!["accept", "decline"]);
    assert_eq!((prompt.2, prompt.3), (0, 2));
    (prompt.4.unwrap())("accept");
    (prompt.5.unwrap())(2);
    assert_eq!(request.state(), "active");
    let path = request.path.borrow().clone().unwrap();
    assert_eq!(path, folder.join("photo.jpg"));
    request.transferred.set(1024);
    f.m.on_receive_progress(&request, true);
    {
        let shown = notifier.shown.borrow();
        let last = shown.last().unwrap();
        assert_eq!((last.0.as_str(), last.4, last.3, last.5), ("Receiving file…", Some(50), 1, 0));
        assert!(last.1.contains("of"));
    }
    // Progress is throttled: same percent, or under 500 ms.
    let count = notifier.shown.borrow().len();
    f.m.on_receive_progress(&request, false);
    request.transferred.set(1536);
    f.env.now.set(100000);
    f.m.on_receive_progress(&request, false);
    assert_eq!(notifier.shown.borrow().len(), count);
    f.env.now.set(700000);
    f.m.on_receive_progress(&request, false);
    assert_eq!(notifier.shown.borrow().len(), count + 1);
    request.set_state("complete");
    f.m.on_receive_done(&request);
    let done = notifier.shown.borrow().last().map(|s| (s.0.clone(), s.1.clone(), s.2.clone(), s.7.clone())).unwrap();
    assert_eq!(done.0, "File received");
    assert!(done.1.contains("was saved to"));
    assert_eq!(done.2, vec!["open", "folder"]);
    (done.3.clone().unwrap())("folder");
    (done.3.unwrap())("open");
    assert_eq!(*opened.borrow(), vec![(path.clone(), true), (path.clone(), false)]);
    assert_eq!(f.m.event_text(), "Received photo.jpg");
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn incoming_file_can_be_declined_or_falls_back_to_dialog() {
    let folder = temp_folder();
    let (f, notifier, _) = receive_fx(folder.to_str().unwrap());
    let declined = ReceiveRequest::detached("photo.jpg", 2048, "");
    f.m.on_receive_request(&declined);
    assert!(notifier.shown.borrow().last().unwrap().1.contains("A nearby device"));
    let on_closed = notifier.shown.borrow().last().unwrap().8.clone().unwrap();
    on_closed(2);
    assert_eq!(declined.state(), "rejected");
    assert_eq!(*notifier.closed.borrow(), vec![1]);
    f.m.set_notifier(None);
    let request = ReceiveRequest::detached("notes.txt", 0, "");
    f.m.on_receive_request(&request);
    let (r, summary, body) = f.view.dialogs.borrow().last().cloned().unwrap();
    assert!(Rc::ptr_eq(&r, &request));
    assert_eq!(summary, "Incoming file");
    assert!(body.contains("unknown size"));
    f.m.answer_receive(&request, true);
    assert_eq!(request.state(), "active");
    let off = ReceiveRequest::detached("x", 1, "");
    f.m.set_receive_mode_value("off");
    f.m.on_receive_request(&off);
    assert_eq!(off.state(), "rejected");
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn notification_errors_and_no_action_servers_use_the_dialog() {
    let (f, notifier, _) = receive_fx("/nonexistent");
    notifier.actions.set(false);
    let request = ReceiveRequest::detached("a.txt", 3, "");
    f.m.on_receive_request(&request);
    assert_eq!(f.view.dialogs.borrow().len(), 1);
    assert!(notifier.shown.borrow().is_empty());
}

#[test]
fn unsaveable_download_reports_failure() {
    let folder = temp_folder();
    std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
    std::fs::write(&folder, "not a directory").unwrap();
    let (f, notifier, _) = receive_fx(folder.to_str().unwrap());
    let request = ReceiveRequest::detached("photo.jpg", 2, "");
    f.m.on_receive_request(&request);
    let accept = notifier.shown.borrow().last().unwrap().7.clone().unwrap();
    accept("accept");
    assert_eq!(request.state(), "rejected");
    let last = notifier.shown.borrow().last().map(|s| (s.0.clone(), s.1.clone(), s.3, s.5)).unwrap();
    assert_eq!(last.0, "File transfer failed");
    assert!(last.1.starts_with("Couldn't save “photo.jpg”: "), "{}", last.1);
    assert_eq!((last.2, last.3), (1, -1));
    let _ = std::fs::remove_file(folder);
}

#[test]
fn failed_receive_and_cancelled_prompt() {
    let (f, notifier, _) = receive_fx("/nonexistent");
    let request = ReceiveRequest::detached("photo.jpg", 2, "");
    request.user_data.borrow_mut().notification = Some(4);
    request.set_state("failed");
    f.m.on_receive_done(&request);
    let last = notifier.shown.borrow().last().map(|s| (s.0.clone(), s.1.clone(), s.3)).unwrap();
    assert_eq!(last, ("File transfer failed".into(), "“photo.jpg” from A nearby device was not received.".into(), 4));
    f.m.on_receive_cancelled(&request);
    assert_eq!(*notifier.closed.borrow(), vec![4]);
}

#[test]
fn primary_follows_receive_policy() {
    crate::i18n::set_language("en");
    let bz = FakeBluez::new("populated", false);
    let started = Rc::new(RefCell::new(Vec::new()));
    struct R(Rc<RefCell<Vec<&'static str>>>);
    impl crate::transfer::Receiver for R {
        fn start(&self) { self.0.borrow_mut().push("start") }
        fn stop(&self) { self.0.borrow_mut().push("stop") }
    }
    let s = started.clone();
    let mode = Rc::new(RefCell::new("ask".to_owned()));
    let mm = mode.clone();
    let services = Services {
        receiver: Rc::new(move |_, _| Rc::new(R(s.clone()))),
        notifier: Rc::new(|_, _| None),
        load_receive_mode: Rc::new(move || mm.borrow().clone()),
        monitor_config: false,
        ..Services::default()
    };
    let m = Manager::new(bz.bt.clone(), FakeEnv::new(), true, "en", services);
    m.set_view(Rc::new(RecordingView::default()));
    m.apply_receive_mode();
    assert!(started.borrow().is_empty(), "only the primary (with a receive bus) serves pushes");
    m.start_receiver(crate::dbus::FakeConn::new());
    assert_eq!(*started.borrow(), vec!["start"]);
    *mode.borrow_mut() = "off".into();
    m.on_receive_config_changed();
    assert_eq!(*started.borrow(), vec!["start", "stop"]);
    assert!(!m.has_receiver());
    *mode.borrow_mut() = "ask".into();
    m.on_receive_config_changed();
    m.window_destroyed();
    assert_eq!(*started.borrow(), vec!["start", "stop", "start", "stop"]);
    assert!(m.closing());
}

#[test]
fn sender_names_match_addresses_case_insensitively() {
    let f = Fx::new();
    assert_eq!(f.m.sender_name("a4:5e:60:12:34:01"), "WH-1000XM5");
    assert_eq!(f.m.sender_name("00:11:22:33:44:55"), "00:11:22:33:44:55");
    assert_eq!(f.m.sender_name(""), "A nearby device");
}
