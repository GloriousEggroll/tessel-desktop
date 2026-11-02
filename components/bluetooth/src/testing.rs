//! Test fixtures shared by the unit tests and the headless GTK tests: the
//! Python `FakeBT` (in-memory BlueZ whose asynchronous operations the test
//! completes explicitly) on top of [`FakeConn`], sample device sets, and a
//! [`View`] that records what the manager rendered. Nothing here opens a bus.

use crate::backend::*;
use crate::dbus::{props_reply, vardict, BtError, CallRecord, FakeConn};
use crate::manager::*;
use crate::transfer::ReceiveRequest;
use glib::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub const ADAPTER: &str = "/org/bluez/hci0";
pub const SECOND_ADAPTER: &str = "/org/bluez/hci1";

/// `device(suffix, alias, adapter, **properties)` of test_ui.py.
pub fn device(suffix: &str, alias: &str, adapter: &str) -> (String, Device) {
    (format!("{adapter}/dev_{suffix}"), Device {
        adapter: adapter.into(), addr: format!("A4:5E:60:12:34:{suffix}"), alias: alias.into(), icon: "bluetooth".into(),
        ..Default::default()
    })
}

pub struct Sample {
    pub adapters: OMap<Adapter>,
    pub devices: OMap<Device>,
}

/// FakeBT's states: populated, off, empty, no-adapter, multi-adapter.
pub fn sample(state: &str) -> Sample {
    let mut adapters = OMap::default();
    adapters.insert(ADAPTER, Adapter { powered: state != "off", discovering: false, alias: "My computer".into(),
                                       address: "18:56:80:AB:CD:EF".into() });
    let mut samples = vec![
        device("01", "WH-1000XM5", ADAPTER), device("02", "MX Master 3S", ADAPTER),
        device("03", "Magic Keyboard", ADAPTER), device("04", "Pixel 9", ADAPTER),
    ];
    samples[0].1 = Device { connected: true, paired: true, bonded: true, trusted: true, icon: "audio-headphones".into(), ..samples[0].1.clone() };
    samples[1].1 = Device { paired: true, bonded: true, trusted: true, icon: "input-mouse".into(), ..samples[1].1.clone() };
    samples[2].1.icon = "input-keyboard".into();
    samples[3].1.icon = "phone".into();
    let mut devices = OMap::default();
    for (path, dev) in &samples {
        devices.insert(path, dev.clone());
    }
    if state == "multi-adapter" {
        adapters.get_mut(ADAPTER).unwrap().alias = "Built-in Bluetooth".into();
        adapters.insert(SECOND_ADAPTER, Adapter { powered: true, discovering: false, alias: "USB Bluetooth".into(),
                                                  address: "24:6F:28:12:34:56".into() });
        for (path, dev) in &samples {
            devices.insert(&path.replace(ADAPTER, SECOND_ADAPTER), Device { adapter: SECOND_ADAPTER.into(), ..dev.clone() });
        }
    }
    if state == "empty" || state == "no-adapter" {
        devices.clear();
    }
    if state == "no-adapter" {
        adapters.clear();
    }
    if state == "off" {
        for (_, dev) in devices.0.iter_mut() {
            dev.connected = false;
        }
    }
    Sample { adapters, devices }
}

pub fn adapter_props(ad: &Adapter) -> Variant {
    vardict(&[("Powered", ad.powered.to_variant()), ("Discovering", ad.discovering.to_variant()),
              ("Alias", ad.alias.to_variant()), ("Address", ad.address.to_variant())])
}

pub fn device_props(dev: &Device) -> Variant {
    let mut props = vec![("Address", dev.addr.to_variant()), ("Alias", dev.alias.to_variant()),
                         ("Connected", dev.connected.to_variant()), ("Paired", dev.paired.to_variant()),
                         ("Bonded", dev.bonded.to_variant()), ("Trusted", dev.trusted.to_variant()),
                         ("Pairing", dev.pairing.to_variant()), ("Icon", dev.icon.to_variant()), ("UUIDs", dev.uuids.to_variant())];
    if !dev.adapter.is_empty() {
        props.push(("Adapter", crate::dbus::object_path(&dev.adapter)));
    }
    if dev.class != 0 {
        props.push(("Class", (dev.class as u32).to_variant()));
    }
    vardict(&props)
}

/// A `(a{oa{sa{sv}}})` GetManagedObjects reply for a sample.
pub fn managed_objects(adapters: &OMap<Adapter>, devices: &OMap<Device>) -> Variant {
    let entry = |path: &str, ifaces: Vec<(&str, Variant)>| {
        let inner: Vec<Variant> = ifaces.into_iter().map(|(n, p)| Variant::from_dict_entry(&n.to_variant(), &p)).collect();
        Variant::from_dict_entry(&crate::dbus::object_path(path), &Variant::array_from_iter_with_type(
            glib::VariantTy::new("{sa{sv}}").unwrap(), inner))
    };
    let mut entries = Vec::new();
    for (path, ad) in adapters.iter() {
        entries.push(entry(path, vec![(ADAPTER_IFACE, adapter_props(ad))]));
    }
    for (path, dev) in devices.iter() {
        let mut ifaces = vec![(DEVICE_IFACE, device_props(dev))];
        if let Some(level) = dev.battery {
            ifaces.push((BATTERY_IFACE, vardict(&[("Percentage", (level as u8).to_variant())])));
        }
        entries.push(entry(path, ifaces));
    }
    Variant::tuple_from_iter([Variant::array_from_iter_with_type(glib::VariantTy::new("{oa{sa{sv}}}").unwrap(), entries)])
}

/// Python `FakeBT`: the app's model is mutated by `complete()` like the
/// Python fake mutates its dictionaries, then the callback runs.
pub struct FakeBluez {
    pub conn: Rc<FakeConn>,
    pub bt: Rc<Bt>,
    pub sessions_by_adapter: RefCell<HashMap<String, i64>>,
    pub external_discovery: Cell<bool>,
    deferred: RefCell<Option<Sample>>,
}

impl FakeBluez {
    pub fn new(state: &str, defer_discovery: bool) -> Rc<Self> {
        let initial = sample(state);
        let reply = managed_objects(&initial.adapters, &initial.devices);
        let conn = if defer_discovery {
            FakeConn::new()
        } else {
            FakeConn::with_responder(move |c| (c.method == "GetManagedObjects").then(|| Ok(reply.clone())))
        };
        let bt = Bt::new(conn.clone());
        Rc::new(Self { conn, bt, sessions_by_adapter: RefCell::default(), external_discovery: Cell::new(false),
                       deferred: RefCell::new(defer_discovery.then_some(initial)) })
    }

    /// `complete_discovery(adapters=None, devices=None)`.
    pub fn complete_discovery(&self, adapters: Option<OMap<Adapter>>, devices: Option<OMap<Device>>) {
        let initial = self.deferred.take().expect("No discovery callback is pending");
        let adapters = adapters.unwrap_or(initial.adapters);
        let devices = devices.unwrap_or(initial.devices);
        self.conn.reply("GetManagedObjects", Ok(managed_objects(&adapters, &devices)));
    }

    /// `history`: every BlueZ operation except discovery.
    pub fn history(&self) -> Vec<CallRecord> {
        self.conn.calls.borrow().iter().filter(|c| c.method != "GetManagedObjects").cloned().collect()
    }

    pub fn clear_history(&self) {
        self.conn.calls.borrow_mut().retain(|c| c.method == "GetManagedObjects");
    }

    /// `[(method, path)]` of pending operations (discovery excluded).
    pub fn pending(&self) -> Vec<(String, String)> {
        self.conn.pending.borrow().iter().filter(|(c, _)| c.method != "GetManagedObjects")
            .map(|(c, _)| (fake_method(c), c.path.clone())).collect()
    }

    pub fn first_pending(&self) -> Option<CallRecord> {
        self.conn.pending.borrow().iter().find(|(c, _)| c.method != "GetManagedObjects").map(|(c, _)| c.clone())
    }

    pub fn discovery_sessions(&self) -> i64 {
        self.sessions_by_adapter.borrow().values().sum()
    }

    pub fn sessions(&self, adapter: &str) -> Option<i64> {
        self.sessions_by_adapter.borrow().get(adapter).copied()
    }

    pub fn info_reply(&self, path: &str) -> Variant {
        let dev = self.bt.device(path).unwrap_or_default();
        let owner = if dev.adapter.is_empty() { path.rsplit_once('/').map(|(h, _)| h.to_owned()).unwrap_or_default() } else { dev.adapter.clone() };
        props_reply(&[("Alias", dev.alias.to_variant()), ("Address", dev.addr.to_variant()),
                      ("Adapter", crate::dbus::object_path(&owner)), ("Connected", dev.connected.to_variant()),
                      ("Paired", dev.paired.to_variant()), ("Trusted", dev.trusted.to_variant()), ("UUIDs", dev.uuids.to_variant())])
    }

    /// `FakeBT.complete(error, result)` for the oldest pending operation.
    pub fn complete(&self, error: Option<BtError>) -> CallRecord {
        let call = self.first_pending().expect("a pending BlueZ operation");
        let method = fake_method(&call);
        let path = call.path.clone();
        let mut result = ().to_variant();
        if error.is_none() {
            let mut adapters = self.bt.adapters.borrow_mut();
            let mut devices = self.bt.devices.borrow_mut();
            if let Some(prop) = method.strip_prefix("Set:") {
                let value = call.params.as_ref().map(|p| p.child_value(2).as_variant().unwrap()).unwrap();
                let flag = value.get::<bool>().unwrap_or(false);
                if let Some(ad) = adapters.get_mut(&path) {
                    if prop == "Powered" {
                        ad.powered = flag;
                        if !flag {
                            self.sessions_by_adapter.borrow_mut().insert(path.clone(), 0);
                            ad.discovering = false;
                        }
                    }
                } else if let Some(dev) = devices.get_mut(&path) {
                    if prop == "Trusted" {
                        dev.trusted = flag;
                    }
                }
            } else if method == "Pair" {
                let dev = devices.get_mut(&path).expect("pair target");
                dev.paired = true;
                dev.bonded = true;
            } else if method == "Connect" || method == "Disconnect" {
                devices.get_mut(&path).expect("connect target").connected = method == "Connect";
            } else if method == "StartDiscovery" || method == "StopDiscovery" {
                let mut sessions = self.sessions_by_adapter.borrow_mut();
                let count = sessions.get(&path).copied().unwrap_or(0) + if method == "StartDiscovery" { 1 } else { -1 };
                sessions.insert(path.clone(), count.max(0));
                if let Some(ad) = adapters.get_mut(&path) {
                    ad.discovering = (path == ADAPTER && self.external_discovery.get()) || sessions[&path] > 0;
                }
            } else if method == "RemoveDevice" {
                let device_path = call.arg_str(0).unwrap();
                let owner = devices.get(&device_path).map(|d| d.adapter.clone()).filter(|a| !a.is_empty())
                    .unwrap_or_else(|| device_path.rsplit_once('/').map(|(h, _)| h.to_owned()).unwrap_or_default());
                assert_eq!(owner, path, "RemoveDevice was sent to the wrong adapter");
                devices.remove(&device_path);
            } else if method == "GetAll" {
                drop((adapters, devices));
                result = self.info_reply(&path);
            }
        }
        let outcome = match error {
            Some(error) => Err(error),
            None => Ok(result),
        };
        let target = call.clone();
        self.conn.reply_where(move |c| c.method == target.method && c.path == target.path && c.method != "GetManagedObjects", outcome);
        call
    }
}

/// Python FakeBT names property writes `Set:<Prop>`.
pub fn fake_method(call: &CallRecord) -> String {
    if call.method == "Set" && call.interface == PROPS_IFACE {
        format!("Set:{}", call.arg_str(1).unwrap_or_default())
    } else {
        call.method.clone()
    }
}

/// Records what the manager renders.
#[derive(Default)]
pub struct RecordingView {
    pub state: RefCell<Option<StateView>>,
    pub lists: RefCell<Option<ListsView>>,
    pub events: RefCell<Vec<EventView>>,
    pub hides: Cell<u32>,
    pub dialogs: RefCell<Vec<(Rc<ReceiveRequest>, String, String)>>,
}

impl RecordingView {
    pub fn state(&self) -> StateView {
        self.state.borrow().clone().expect("a rendered state")
    }

    pub fn lists(&self) -> ListsView {
        self.lists.borrow().clone().expect("rendered lists")
    }

    pub fn last_event(&self) -> Option<EventView> {
        self.events.borrow().last().cloned()
    }
}

impl View for RecordingView {
    fn apply(&self, state: &StateView) {
        self.state.replace(Some(state.clone()));
    }
    fn event(&self, event: &EventView) {
        self.events.borrow_mut().push(event.clone());
    }
    fn hide_add_event(&self) {
        self.hides.set(self.hides.get() + 1);
    }
    fn lists(&self, lists: &ListsView) {
        self.lists.replace(Some(lists.clone()));
    }
    fn sync_switches(&self, _trusted: &[(String, bool)]) {}
    fn receive_dialog(&self, request: &Rc<ReceiveRequest>, summary: &str, body: &str) {
        self.dialogs.borrow_mut().push((request.clone(), summary.into(), body.into()));
    }
}

/// A `BtError` like `GLib.Error(message)`.
pub fn gerr(message: &str) -> BtError {
    BtError::glib(message)
}
