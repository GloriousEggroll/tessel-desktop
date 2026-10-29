//! Port of test_backend.py: radio-free adapter availability, the asynchronous
//! introspection fallback, ObjectManager signals and the error advice table.

use super::*;
use crate::dbus::{CallRecord, FakeConn};
use crate::env::FakeEnv;
use crate::manager::{Manager, Services};
use crate::testing::RecordingView;
use std::collections::BTreeMap;

const ADAPTER: &str = "/org/bluez/hci0";
const DEVICE: &str = "/org/bluez/hci0/dev_12_34_56_78_9A_BC";
const SECOND_ADAPTER: &str = "/org/bluez/hci1";
const SECOND_DEVICE: &str = "/org/bluez/hci1/dev_12_34_56_78_9A_BC";

#[derive(Clone, Debug)]
enum V {
    B(bool),
    Y(u8),
    S(&'static str),
    O(&'static str),
}

fn wrap(value: &V) -> Variant {
    match value {
        V::B(b) => b.to_variant(),
        V::Y(n) => n.to_variant(),
        V::S(s) => s.to_variant(),
        V::O(o) => crate::dbus::object_path(o),
    }
}

type PropTable = BTreeMap<(String, String), Option<Vec<(&'static str, V)>>>;

/// The Python fixture: an introspection tree plus a property table, every
/// reply completing immediately; blocking calls are impossible here.
struct Fixture {
    conn: Rc<FakeConn>,
    bt: Rc<Bt>,
    tree: Rc<RefCell<BTreeMap<String, Option<Vec<String>>>>>,
    props: Rc<RefCell<PropTable>>,
}

fn props_value(props: &[(&str, V)]) -> Props {
    props_of(&crate::dbus::vardict(&props.iter().map(|(k, v)| (*k, wrap(v))).collect::<Vec<_>>()))
}

impl Fixture {
    fn new() -> Self {
        let tree: Rc<RefCell<BTreeMap<String, Option<Vec<String>>>>> = Rc::default();
        tree.borrow_mut().insert(BLUEZ_ROOT.into(), Some(vec!["hci0".into()]));
        tree.borrow_mut().insert(ADAPTER.into(), Some(vec![]));
        let props: Rc<RefCell<PropTable>> = Rc::default();
        {
            let mut p = props.borrow_mut();
            p.insert((ADAPTER.into(), ADAPTER_IFACE.into()), Some(vec![("Powered", V::B(true)), ("Discovering", V::B(false)),
                ("Name", V::S("Test adapter")), ("Address", V::S("12:34:56:78:9A:BC"))]));
            p.insert((DEVICE.into(), DEVICE_IFACE.into()), Some(vec![("Alias", V::S("Test device")), ("Address", V::S("01:02:03:04:05:06")),
                ("Adapter", V::O(ADAPTER)), ("Connected", V::B(true)), ("Paired", V::B(true))]));
            p.insert((SECOND_ADAPTER.into(), ADAPTER_IFACE.into()), Some(vec![("Powered", V::B(false)), ("Discovering", V::B(false)),
                ("Name", V::S("Second adapter")), ("Address", V::S("AB:CD:EF:01:23:45"))]));
            p.insert((SECOND_DEVICE.into(), DEVICE_IFACE.into()), Some(vec![("Alias", V::S("Test device")),
                ("Address", V::S("01:02:03:04:05:06")), ("Adapter", V::O(SECOND_ADAPTER)), ("Connected", V::B(false)), ("Paired", V::B(false))]));
        }
        let (t, p) = (tree.clone(), props.clone());
        let conn = FakeConn::with_responder(move |call: &CallRecord| {
            if call.method == "Introspect" {
                assert_eq!(call.interface, INTROSPECT_IFACE);
                return Some(match t.borrow().get(&call.path).cloned().flatten() {
                    None => Err(BtError::glib("Temporary D-Bus failure")),
                    Some(children) => {
                        let xml = format!("<node>{}</node>", children.iter().map(|n| format!("<node name=\"{n}\"/>")).collect::<String>());
                        Ok((xml,).to_variant())
                    }
                });
            }
            if call.method == "GetManagedObjects" {
                return None;
            }
            assert_eq!((call.interface.as_str(), call.method.as_str()), (PROPS_IFACE, "GetAll"));
            let iface = call.arg_str(0).unwrap();
            Some(match p.borrow().get(&(call.path.clone(), iface)).cloned().flatten() {
                None => Err(BtError::glib("No such interface")),
                Some(props) => Ok(crate::dbus::props_reply(&props.iter().map(|(k, v)| (*k, wrap(v))).collect::<Vec<_>>())),
            })
        });
        let bt = Bt::new(conn.clone());
        Self { conn, bt, tree, props }
    }

    fn set_tree(&self, path: &str, children: Option<&[&str]>) {
        self.tree.borrow_mut().insert(path.into(), children.map(|c| c.iter().map(|s| s.to_string()).collect()));
    }

    fn update(&self, path: &str, iface: &str, key: &'static str, value: V) {
        let mut props = self.props.borrow_mut();
        let entry = props.entry((path.into(), iface.into())).or_insert_with(|| Some(Vec::new()));
        let list = entry.get_or_insert_with(Vec::new);
        match list.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => list.push((key, value)),
        }
    }

    fn set_props(&self, path: &str, iface: &str, props: Option<Vec<(&'static str, V)>>) {
        self.props.borrow_mut().insert((path.into(), iface.into()), props);
    }

    fn get(&self, path: &str, iface: &str) -> Vec<(&'static str, V)> {
        self.props.borrow().get(&(path.to_string(), iface.to_string())).cloned().flatten().unwrap()
    }

    fn rescan_fresh(&self, fresh: &[&str]) -> Vec<String> {
        let results = Rc::new(RefCell::new(Vec::new()));
        let r = results.clone();
        self.bt.rescan_async(Box::new(move |changed| r.borrow_mut().push(changed)), fresh.iter().map(|s| s.to_string()).collect());
        let results = results.borrow();
        assert_eq!(results.len(), 1, "rescan must complete exactly once");
        results[0].clone()
    }

    fn rescan(&self) -> Vec<String> {
        self.rescan_fresh(&[])
    }

    fn seed_adapter_and_device(&self) {
        self.bt.adapters.borrow_mut().insert(ADAPTER, adapter_from_props(&props_value(&self.get(ADAPTER, ADAPTER_IFACE))));
        self.bt.devices.borrow_mut().insert(DEVICE, dev_from_props(&props_value(&self.get(DEVICE, DEVICE_IFACE))));
        self.set_tree(ADAPTER, Some(&["dev_12_34_56_78_9A_BC"]));
    }

    fn seed_two_adapters_and_devices(&self) {
        self.set_tree(BLUEZ_ROOT, Some(&["hci0", "hci1"]));
        self.set_tree(ADAPTER, Some(&["dev_12_34_56_78_9A_BC"]));
        self.set_tree(SECOND_ADAPTER, Some(&["dev_12_34_56_78_9A_BC"]));
        self.rescan();
    }

    fn methods(&self) -> Vec<String> {
        self.conn.methods()
    }
}

/// `make_app()`: a manager whose refreshes queue on a fake main loop.
struct App {
    m: Rc<Manager>,
    env: Rc<FakeEnv>,
}

impl App {
    fn new(f: &Fixture) -> Self {
        let env = FakeEnv::new();
        let services = Services { load_receive_mode: Rc::new(|| "ask".into()), monitor_config: false, ..Services::default() };
        let m = Manager::new(f.bt.clone(), env.clone(), false, "en", services);
        m.set_view(Rc::new(RecordingView::default()));
        Self { m, env }
    }

    fn refreshes(&self) -> usize {
        self.env.pending_idles()
    }

    fn remove_adapter_signal(&self, adapter: &str) {
        self.objmgr("InterfacesRemoved", Variant::tuple_from_iter([crate::dbus::object_path(adapter), vec![ADAPTER_IFACE].to_variant()]));
    }

    fn objmgr(&self, member: &str, params: Variant) {
        self.m.on_objmgr_signal(&Signal { sender: BLUEZ.into(), path: BLUEZ_ROOT.into(), interface: OBJMGR_IFACE.into(),
                                          member: member.into(), params });
    }

    fn added(&self, path: &str, iface: &str, props: &[(&str, Variant)]) {
        let inner = Variant::from_dict_entry(&iface.to_variant(), &crate::dbus::vardict(props));
        let ifaces = Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(), [inner]);
        self.objmgr("InterfacesAdded", Variant::tuple_from_iter([crate::dbus::object_path(path), ifaces]));
    }
}

fn subscriptions_for(f: &Fixture, path: &str) -> usize {
    f.conn.signals.borrow().iter().filter(|(_, s)| s.3.as_deref() == Some(path) && s.2.as_deref() == Some("PropertiesChanged")).count()
}

fn set(paths: &[String]) -> std::collections::BTreeSet<String> {
    paths.iter().cloned().collect()
}

fn strs(paths: &[&str]) -> std::collections::BTreeSet<String> {
    paths.iter().map(|s| s.to_string()).collect()
}

#[test]
fn device_properties_retain_adapter_path() {
    let f = Fixture::new();
    for (adapter, device) in [(ADAPTER, DEVICE), (SECOND_ADAPTER, SECOND_DEVICE)] {
        assert_eq!(dev_from_props(&props_value(&f.get(device, DEVICE_IFACE))).adapter, adapter);
    }
}

#[test]
fn duplicate_addresses_remain_independent_across_rescans() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    assert_eq!(set(&f.bt.devices.borrow().keys()), strs(&[DEVICE, SECOND_DEVICE]));
    let first = f.bt.device(DEVICE).unwrap();
    let second = f.bt.device(SECOND_DEVICE).unwrap();
    assert_eq!(first.addr, second.addr);
    assert!(first.connected && first.paired && !second.connected && !second.paired);
    assert!(f.rescan().is_empty());
    f.update(SECOND_DEVICE, DEVICE_IFACE, "Alias", V::S("Device on second adapter"));
    f.update(SECOND_DEVICE, DEVICE_IFACE, "Trusted", V::B(true));
    f.update(SECOND_DEVICE, DEVICE_IFACE, "Paired", V::B(true));
    assert_eq!(f.rescan(), vec![SECOND_DEVICE.to_owned()]);
    assert_eq!(f.bt.device(DEVICE).unwrap(), first);
    let second = f.bt.device(SECOND_DEVICE).unwrap();
    assert_eq!(second.alias, "Device on second adapter");
    assert_eq!(second.adapter, SECOND_ADAPTER);
    assert!(second.trusted && second.paired && !second.connected);
}

#[test]
fn poll_unplug_preserves_other_adapter_and_duplicate_device() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    let other_adapter = f.bt.adapter(Some(SECOND_ADAPTER)).unwrap();
    let other_device = f.bt.device(SECOND_DEVICE).unwrap();
    f.set_tree(BLUEZ_ROOT, Some(&["hci1"]));
    assert_eq!(set(&f.rescan()), strs(&[ADAPTER, DEVICE]));
    assert_eq!(f.bt.adapters.borrow().0, vec![(SECOND_ADAPTER.to_owned(), other_adapter)]);
    assert_eq!(f.bt.devices.borrow().0, vec![(SECOND_DEVICE.to_owned(), other_device)]);
}

#[test]
fn poll_preserves_manual_unpowered_adapter_selection() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    let app = App::new(&f);
    app.m.set_default_adapter(Some(SECOND_ADAPTER));
    f.update(DEVICE, DEVICE_IFACE, "Trusted", V::B(true));
    assert!(app.m.poll_devices());
    assert_eq!(app.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(!f.bt.powered(Some(SECOND_ADAPTER)));
    assert_eq!(app.m.watched(), vec![DEVICE.to_owned()]);
    assert_eq!(app.refreshes(), 1);
}

#[test]
fn poll_unselected_adapter_removal_preserves_selection() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    let app = App::new(&f);
    for path in [ADAPTER, DEVICE, SECOND_ADAPTER, SECOND_DEVICE] {
        app.m.watch_object(path);
    }
    app.m.set_default_adapter(Some(SECOND_ADAPTER));
    f.set_tree(BLUEZ_ROOT, Some(&["hci1"]));
    assert!(app.m.poll_devices());
    assert_eq!(app.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert_eq!(set(&f.bt.devices.borrow().keys()), strs(&[SECOND_DEVICE]));
    assert_eq!(set(&app.m.watched()), strs(&[SECOND_ADAPTER, SECOND_DEVICE]));
    assert_eq!((subscriptions_for(&f, ADAPTER), subscriptions_for(&f, DEVICE)), (0, 0));
}

#[test]
fn poll_selected_adapter_removal_selects_remaining_adapter() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    let app = App::new(&f);
    for path in [ADAPTER, DEVICE, SECOND_ADAPTER, SECOND_DEVICE] {
        app.m.watch_object(path);
    }
    app.m.set_default_adapter(Some(SECOND_ADAPTER));
    f.set_tree(BLUEZ_ROOT, Some(&["hci0"]));
    assert!(app.m.poll_devices());
    assert_eq!(app.m.default_adapter().as_deref(), Some(ADAPTER));
    assert_eq!(set(&f.bt.devices.borrow().keys()), strs(&[DEVICE]));
    assert_eq!(set(&app.m.watched()), strs(&[ADAPTER, DEVICE]));
}

#[test]
fn object_manager_add_preserves_manual_adapter_selection() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    let app = App::new(&f);
    app.m.set_default_adapter(Some(SECOND_ADAPTER));
    app.added(ADAPTER, ADAPTER_IFACE, &[("Powered", true.to_variant()), ("Alias", "Updated adapter".to_variant())]);
    assert_eq!(app.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(f.bt.devices.borrow().contains(SECOND_DEVICE));
    app.added(DEVICE, DEVICE_IFACE, &[("Adapter", crate::dbus::object_path(ADAPTER)), ("Address", "01:02:03:04:05:06".to_variant()),
                                      ("Connected", false.to_variant())]);
    assert_eq!(app.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert!(f.bt.devices.borrow().contains(SECOND_DEVICE));
}

#[test]
fn object_manager_unselected_adapter_removal_preserves_selection() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    let app = App::new(&f);
    for path in [ADAPTER, DEVICE, SECOND_ADAPTER, SECOND_DEVICE] {
        app.m.watch_object(path);
    }
    app.m.set_default_adapter(Some(SECOND_ADAPTER));
    let other_adapter = f.bt.adapter(Some(SECOND_ADAPTER)).unwrap();
    let other_device = f.bt.device(SECOND_DEVICE).unwrap();
    app.remove_adapter_signal(ADAPTER);
    assert_eq!(app.m.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    assert_eq!(f.bt.adapters.borrow().0, vec![(SECOND_ADAPTER.to_owned(), other_adapter)]);
    assert_eq!(f.bt.devices.borrow().0, vec![(SECOND_DEVICE.to_owned(), other_device)]);
    assert_eq!(set(&app.m.watched()), strs(&[SECOND_ADAPTER, SECOND_DEVICE]));
}

#[test]
fn object_manager_selected_adapter_removal_selects_remaining() {
    let f = Fixture::new();
    f.seed_two_adapters_and_devices();
    let app = App::new(&f);
    for path in [ADAPTER, DEVICE, SECOND_ADAPTER, SECOND_DEVICE] {
        app.m.watch_object(path);
    }
    app.m.set_default_adapter(Some(SECOND_ADAPTER));
    app.remove_adapter_signal(SECOND_ADAPTER);
    assert_eq!(app.m.default_adapter().as_deref(), Some(ADAPTER));
    assert_eq!(set(&f.bt.adapters.borrow().keys()), strs(&[ADAPTER]));
    assert_eq!(set(&f.bt.devices.borrow().keys()), strs(&[DEVICE]));
    assert_eq!(set(&app.m.watched()), strs(&[ADAPTER, DEVICE]));
}

#[test]
fn poll_discovers_new_adapter_and_its_device() {
    let f = Fixture::new();
    f.set_tree(ADAPTER, Some(&["dev_12_34_56_78_9A_BC"]));
    assert_eq!(set(&f.rescan()), strs(&[ADAPTER, DEVICE]));
    assert!(f.bt.powered(Some(ADAPTER)));
    assert!(f.bt.device(DEVICE).unwrap().connected);
    assert!(f.rescan().is_empty());
}

#[test]
fn poll_removes_unplugged_adapter_and_its_devices() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    f.set_tree(BLUEZ_ROOT, Some(&[]));
    assert_eq!(set(&f.rescan()), strs(&[ADAPTER, DEVICE]));
    assert!(f.bt.adapters.borrow().is_empty() && f.bt.devices.borrow().is_empty());
}

#[test]
fn poll_reports_adapter_only_property_changes() {
    let f = Fixture::new();
    f.rescan();
    f.update(ADAPTER, ADAPTER_IFACE, "Powered", V::B(false));
    assert_eq!(f.rescan(), vec![ADAPTER.to_owned()]);
    assert!(!f.bt.powered(Some(ADAPTER)));
}

#[test]
fn failed_root_introspection_preserves_known_objects() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    f.set_tree(BLUEZ_ROOT, None);
    assert!(f.rescan().is_empty());
    assert!(f.bt.adapters.borrow().contains(ADAPTER) && f.bt.devices.borrow().contains(DEVICE));
}

#[test]
fn failed_adapter_properties_preserve_enumerated_adapter() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    f.set_props(ADAPTER, ADAPTER_IFACE, None);
    assert!(f.rescan().is_empty());
    assert!(f.bt.powered(Some(ADAPTER)));
    assert!(f.bt.devices.borrow().contains(DEVICE));
}

#[test]
fn failed_device_introspection_preserves_known_devices() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    f.set_tree(ADAPTER, None);
    assert!(f.rescan().is_empty());
    assert!(f.bt.devices.borrow().contains(DEVICE));
}

#[test]
fn successful_empty_device_enumeration_removes_device() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    f.set_tree(ADAPTER, Some(&[]));
    assert_eq!(f.rescan(), vec![DEVICE.to_owned()]);
    assert!(f.bt.adapters.borrow().contains(ADAPTER));
    assert!(f.bt.devices.borrow().is_empty());
}

#[test]
fn poll_reselects_adapter_and_manages_subscriptions() {
    let f = Fixture::new();
    let app = App::new(&f);
    assert!(app.m.poll_devices());
    assert_eq!(app.m.default_adapter().as_deref(), Some(ADAPTER));
    assert_eq!(app.m.watched(), vec![ADAPTER.to_owned()]);
    assert_eq!(app.refreshes(), 1);
    app.env.drain();
    f.set_tree(BLUEZ_ROOT, Some(&[]));
    assert!(app.m.poll_devices());
    assert_eq!(app.m.default_adapter(), None);
    assert!(app.m.watched().is_empty());
    assert_eq!(subscriptions_for(&f, ADAPTER), 0);
    assert_eq!(app.refreshes(), 1);
}

#[test]
fn object_manager_add_uses_payload_object_path() {
    let f = Fixture::new();
    let app = App::new(&f);
    app.added(ADAPTER, ADAPTER_IFACE, &[("Powered", true.to_variant()), ("Address", "12:34:56:78:9A:BC".to_variant())]);
    assert_eq!(app.m.default_adapter().as_deref(), Some(ADAPTER));
    assert!(f.bt.powered(Some(ADAPTER)));
    assert_eq!(app.m.watched(), vec![ADAPTER.to_owned()]);
    assert_eq!(app.refreshes(), 1);
}

#[test]
fn object_manager_remove_uses_payload_object_path() {
    let f = Fixture::new();
    f.rescan();
    let app = App::new(&f);
    app.m.set_default_adapter(Some(ADAPTER));
    app.m.watch_object(ADAPTER);
    app.remove_adapter_signal(ADAPTER);
    assert!(f.bt.adapters.borrow().is_empty());
    assert_eq!(app.m.default_adapter(), None);
    assert!(app.m.watched().is_empty());
    assert_eq!(app.refreshes(), 1);
}

#[test]
fn unrelated_interface_removal_preserves_tracked_device() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    let app = App::new(&f);
    app.m.watch_object(DEVICE);
    app.objmgr("InterfacesRemoved", Variant::tuple_from_iter([crate::dbus::object_path(DEVICE), vec!["org.bluez.Battery1"].to_variant()]));
    assert!(f.bt.devices.borrow().contains(DEVICE) && f.bt.adapters.borrow().contains(ADAPTER));
    assert_eq!(app.m.watched(), vec![DEVICE.to_owned()]);
}

#[test]
fn battery_interface_is_merged_into_device() {
    let f = Fixture::new();
    f.set_props(DEVICE, BATTERY_IFACE, Some(vec![("Percentage", V::Y(64))]));
    f.set_tree(ADAPTER, Some(&["dev_12_34_56_78_9A_BC"]));
    f.rescan();
    assert_eq!(f.bt.device(DEVICE).unwrap().battery, Some(64));
    f.set_props(DEVICE, BATTERY_IFACE, Some(vec![("Percentage", V::Y(63))]));
    assert_eq!(f.rescan(), vec![DEVICE.to_owned()]);
    assert_eq!(f.bt.device(DEVICE).unwrap().battery, Some(63));
    f.set_props(DEVICE, BATTERY_IFACE, None);
    assert_eq!(f.rescan(), vec![DEVICE.to_owned()]);
    assert_eq!(f.bt.device(DEVICE).unwrap().battery, None);
}

fn props_changed(iface: &str, props: &[(&str, Variant)]) -> Variant {
    Variant::tuple_from_iter([iface.to_variant(), crate::dbus::vardict(props), Vec::<String>::new().to_variant()])
}

#[test]
fn object_manager_battery_add_change_and_removal() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    let app = App::new(&f);
    app.added(DEVICE, BATTERY_IFACE, &[("Percentage", 80u8.to_variant())]);
    assert_eq!(f.bt.device(DEVICE).unwrap().battery, Some(80));
    // A later Device1 payload must not drop the known level.
    app.added(DEVICE, DEVICE_IFACE, &[("Address", "01:02:03:04:05:06".to_variant())]);
    assert_eq!(f.bt.device(DEVICE).unwrap().battery, Some(80));
    app.m.on_props_changed(&Signal { sender: BLUEZ.into(), path: DEVICE.into(), interface: PROPS_IFACE.into(),
        member: "PropertiesChanged".into(), params: props_changed(BATTERY_IFACE, &[("Percentage", 42u8.to_variant())]) });
    assert_eq!(f.bt.device(DEVICE).unwrap().battery, Some(42));
    app.objmgr("InterfacesRemoved", Variant::tuple_from_iter([crate::dbus::object_path(DEVICE), vec![BATTERY_IFACE].to_variant()]));
    assert_eq!(f.bt.device(DEVICE).unwrap().battery, None);
    assert!(f.bt.devices.borrow().contains(DEVICE));
}

#[test]
fn other_interface_properties_do_not_change_device_state() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    let app = App::new(&f);
    let before = app.m.event_text();
    app.m.on_props_changed(&Signal { sender: BLUEZ.into(), path: DEVICE.into(), interface: PROPS_IFACE.into(),
        member: "PropertiesChanged".into(), params: props_changed("org.bluez.MediaControl1", &[("Connected", false.to_variant())]) });
    assert!(f.bt.device(DEVICE).unwrap().connected);
    assert_eq!(app.m.event_text(), before);
}

#[test]
fn watched_objects_are_not_refetched_by_polling() {
    let f = Fixture::new();
    f.seed_adapter_and_device();
    f.conn.calls.borrow_mut().clear();
    f.update(DEVICE, DEVICE_IFACE, "Alias", V::S("Changed"));
    assert!(f.rescan_fresh(&[ADAPTER, DEVICE]).is_empty());
    assert_eq!(f.methods(), vec!["Introspect", "Introspect"]);
    assert_eq!(f.bt.device(DEVICE).unwrap().alias, "Test device");
}

#[test]
fn poll_never_overlaps_and_skips_after_close() {
    let f = Fixture::new();
    let app = App::new(&f);
    type Pending = Rc<RefCell<Vec<Box<dyn FnOnce(Vec<String>)>>>>;
    let pending: Pending = Rc::default();
    let calls = Rc::new(Cell::new(0));
    let (p, c) = (pending.clone(), calls.clone());
    f.bt.rescan_hook.replace(Some(Rc::new(Box::new(move |cb, _fresh| {
        c.set(c.get() + 1);
        p.borrow_mut().push(cb);
    }))));
    assert!(app.m.poll_devices());
    assert!(app.m.poll_devices());
    assert_eq!(calls.get(), 1);
    app.m.set_closing(true);
    let callback = pending.borrow_mut().pop().unwrap();
    callback(vec![ADAPTER.into()]);
    assert!(app.m.watched().is_empty());
    assert_eq!(app.refreshes(), 0);
    assert!(app.m.poll_devices());
    assert_eq!(calls.get(), 2);
}

#[test]
fn object_manager_failure_discovers_without_blocking() {
    let f = Fixture::new();
    f.set_tree(ADAPTER, Some(&["dev_12_34_56_78_9A_BC"]));
    let done = Rc::new(Cell::new(0));
    let d = done.clone();
    f.bt.discover(move || d.set(d.get() + 1));
    assert_eq!(done.get(), 0);
    f.conn.reply("GetManagedObjects", Err(BtError::glib("No ObjectManager")));
    assert_eq!(done.get(), 1);
    assert!(!f.bt.has_object_manager.get());
    assert!(f.bt.devices.borrow().contains(DEVICE));
}

#[test]
fn object_manager_success_marks_the_backend_and_applies_objects() {
    let f = Fixture::new();
    let done = Rc::new(Cell::new(0));
    let d = done.clone();
    f.bt.discover(move || d.set(d.get() + 1));
    let sample = crate::testing::sample("populated");
    let call = f.conn.reply("GetManagedObjects", Ok(crate::testing::managed_objects(&sample.adapters, &sample.devices)));
    assert_eq!(call.timeout, 8000);
    assert_eq!(done.get(), 1);
    assert!(f.bt.has_object_manager.get());
    assert_eq!(f.bt.devices.borrow().keys(), sample.devices.keys());
    assert_eq!(f.bt.device("/org/bluez/hci0/dev_01").unwrap(), sample.devices.get("/org/bluez/hci0/dev_01").cloned().unwrap());
}

// ------------------------------------------------------------------ errors

fn remote(name: &str, message: &str) -> BtError {
    BtError::dbus(gio::DBusError::Failed, &format!("GDBus.Error:{name}: {message}"))
}

#[test]
fn common_bluez_errors_are_explained() {
    crate::i18n::set_language("en");
    let cases = [
        ("org.bluez.Error.Failed", "br-connection-page-timeout", "not responding"),
        ("org.bluez.Error.Failed", "le-connection-abort-by-local", "not responding"),
        ("org.bluez.Error.InProgress", "Operation already in progress", "Already connecting"),
        ("org.bluez.Error.AuthenticationFailed", "Authentication Failed", "check the PIN"),
        ("org.bluez.Error.AuthenticationCanceled", "Authentication Canceled", "cancelled"),
        ("org.bluez.Error.AlreadyExists", "Already Exists", "already paired"),
        ("org.bluez.Error.NotReady", "Resource Not Ready", "turned on"),
        ("org.bluez.Error.Failed", "br-connection-profile-unavailable", "doesn't offer"),
        ("org.bluez.Error.Failed", "br-connection-key-missing", "forget it"),
        ("org.freedesktop.DBus.Error.NoReply", "Did not receive a reply", "in time"),
    ];
    for (name, message, expected) in cases {
        let (advice, details) = friendly_err(&remote(name, message));
        assert!(advice.contains(expected), "{message}: {advice}");
        assert!(!advice.contains("br-connection"));
        assert_eq!(details, message);
        // Real GDBus remote errors parse the same way.
        assert_eq!(friendly_err(&BtError::remote(name, message)), (advice, details));
    }
}

#[test]
fn unknown_error_falls_back_with_details() {
    crate::i18n::set_language("en");
    let (advice, details) = friendly_err(&BtError::glib("Something odd"));
    assert_eq!(advice, "Something went wrong (Something odd).");
    assert_eq!(details, "Something odd");
}

#[test]
fn short_err_and_error_names() {
    let error = remote("org.bluez.Error.Failed", "a: b: c");
    assert_eq!(short_err(&error), "c");
    assert_eq!(dbus_error_name(&error), "org.bluez.Error.Failed");
    assert_eq!(dbus_error_name(&BtError::runtime("plain")), "");
}

#[test]
fn value_conversions_follow_python() {
    let props = props_value(&[("Alias", V::S("")), ("Name", V::S("Named")), ("Powered", V::Y(2)), ("Address", V::O("/x"))]);
    let adapter = adapter_from_props(&props);
    assert_eq!((adapter.alias.as_str(), adapter.powered, adapter.address.as_str()), ("Named", true, "/x"));
    assert_eq!(py_int(Some(&serde_json::json!(150))), Some(100));
    assert_eq!(py_int(Some(&serde_json::json!(-3))), Some(0));
    assert_eq!(py_int(Some(&serde_json::json!("12"))), Some(12));
    assert_eq!(py_int(Some(&serde_json::json!("x"))), None);
    assert_eq!(py_str(Some(&serde_json::json!(5))), "");
    let dev = dev_from_props(&props_value(&[("Address", V::S("AA"))]));
    assert_eq!(dev.alias, "AA");
    assert_eq!(node_names("<node><node name=\"a\"/><node name=\"b\"/><node name=\"a\"/></node>"), vec!["a", "b"]);
}

#[test]
fn ordered_map_keeps_python_dict_order() {
    let mut map: OMap<i32> = OMap::default();
    map.insert("b", 1);
    map.insert("a", 2);
    map.insert("b", 3);
    assert_eq!(map.keys(), vec!["b", "a"]);
    map.remove("b");
    map.insert("b", 4);
    assert_eq!(map.keys(), vec!["a", "b"]);
    assert_eq!(map.sorted_keys(), vec!["a", "b"]);
}
