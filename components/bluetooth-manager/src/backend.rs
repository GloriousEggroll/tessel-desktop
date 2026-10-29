//! `class BT` of btmanager.py: the BlueZ model on the system bus. Discovery
//! through ObjectManager, or the asynchronous Introspect + GetAll fallback
//! (`rescan_async` / `_reconcile`) that never removes objects on a failed
//! reply. Plus the BlueZ error advice table (`friendly_err`).

use crate::dbus::{BtError, Conn, Reply, Signal, SignalHandler};
use crate::i18n::{tr, tr1};
use glib::prelude::*;
use glib::Variant;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use suite_core::pyjson;

pub const BLUEZ: &str = "org.bluez";
pub const BLUEZ_ROOT: &str = "/org/bluez";
pub const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
pub const OBJMGR_IFACE: &str = "org.freedesktop.DBus.ObjectManager";
pub const MANAGER_IFACE: &str = "org.bluez.Manager1";
pub const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
pub const DEVICE_IFACE: &str = "org.bluez.Device1";
pub const BATTERY_IFACE: &str = "org.bluez.Battery1";
pub const INTROSPECT_IFACE: &str = "org.freedesktop.DBus.Introspectable";
pub const OBJECT_PUSH_UUID: &str = "00001105-0000-1000-8000-00805f9b34fb";
/// `call_async` default timeout.
pub const DEFAULT_TIMEOUT: i32 = 30000;

/// Insertion-ordered map (Python dict semantics: re-assignment keeps the
/// position, a popped key re-added goes last).
#[derive(Clone, Debug, PartialEq)]
pub struct OMap<V>(pub Vec<(String, V)>);

impl<V> Default for OMap<V> {
    fn default() -> Self { Self(Vec::new()) }
}

impl<V> OMap<V> {
    pub fn get(&self, key: &str) -> Option<&V> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }
    pub fn insert(&mut self, key: &str, value: V) {
        match self.get_mut(key) {
            Some(slot) => *slot = value,
            None => self.0.push((key.to_owned(), value)),
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let index = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(index).1)
    }
    pub fn keys(&self) -> Vec<String> {
        self.0.iter().map(|(k, _)| k.clone()).collect()
    }
    pub fn len(&self) -> usize { self.0.len() }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
    pub fn clear(&mut self) { self.0.clear() }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.0.iter().map(|(k, v)| (k, v))
    }
    /// `sorted(dict)`.
    pub fn sorted_keys(&self) -> Vec<String> {
        let mut keys = self.keys();
        keys.sort();
        keys
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Adapter {
    pub powered: bool,
    pub discovering: bool,
    pub alias: String,
    pub address: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub addr: String,
    pub adapter: String,
    pub alias: String,
    pub connected: bool,
    pub paired: bool,
    pub bonded: bool,
    pub trusted: bool,
    pub pairing: bool,
    pub icon: String,
    pub class: i64,
    pub uuids: Vec<String>,
    /// `dev["battery"]` (absent = None).
    pub battery: Option<i64>,
}

pub type Props = Map<String, Value>;

/// PyGObject-unpacked `a{sv}`.
pub fn props_of(variant: &Variant) -> Props {
    match suite_core::values::to_value(variant) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// `unwrap(res)` of an `(a{sv})` reply.
pub fn first_props(reply: &Variant) -> Props {
    if reply.n_children() > 0 { props_of(&reply.child_value(0)) } else { Map::new() }
}

/// `_bool(v)`.
pub fn py_bool(value: Option<&Value>) -> bool {
    value.is_some_and(pyjson::truthy)
}

/// `_str(v)`: strings (and object paths) only.
pub fn py_str(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// `_int(v)`: `max(0, min(100, int(v)))` or None.
pub fn py_int(value: Option<&Value>) -> Option<i64> {
    pyjson::py_int(value).map(|n| n.clamp(0, 100))
}

pub fn adapter_from_props(props: &Props) -> Adapter {
    let alias = py_str(props.get("Alias"));
    Adapter {
        powered: py_bool(props.get("Powered")),
        discovering: py_bool(props.get("Discovering")),
        alias: if alias.is_empty() { py_str(props.get("Name")) } else { alias },
        address: py_str(props.get("Address")),
    }
}

pub fn uuids_of(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(pyjson::py_str).collect(),
        Some(Value::String(text)) => text.chars().map(String::from).collect(),
        _ => Vec::new(),
    }
}

pub fn dev_from_props(props: &Props) -> Device {
    let addr = py_str(props.get("Address"));
    let alias = py_str(props.get("Alias"));
    Device {
        alias: if alias.is_empty() { addr.clone() } else { alias },
        adapter: py_str(props.get("Adapter")),
        connected: py_bool(props.get("Connected")),
        paired: py_bool(props.get("Paired")),
        bonded: py_bool(props.get("Bonded")),
        trusted: py_bool(props.get("Trusted")),
        pairing: py_bool(props.get("Pairing")),
        icon: py_str(props.get("Icon")),
        class: props.get("Class").and_then(Value::as_i64).unwrap_or(0),
        uuids: uuids_of(props.get("UUIDs")),
        battery: None,
        addr,
    }
}

/// The child `<node name="…">` names of introspection XML, deduplicated.
pub fn node_names(xml: &str) -> Vec<String> {
    let pattern = regex::Regex::new(r#"<node name="([^"]+)""#).unwrap();
    let mut names: Vec<String> = Vec::new();
    for capture in pattern.captures_iter(xml) {
        let name = capture[1].to_owned();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

type AdapterEntry = Option<Props>;

#[derive(Default)]
struct Rescan {
    pending: usize,
    roots: Option<Vec<String>>,
    /// `None` = failed; `Some(None)` = fresh ("False": signals keep it current).
    adapter_props: HashMap<String, Option<AdapterEntry>>,
    adapter_children: HashMap<String, Option<Vec<String>>>,
    device_props: HashMap<String, Option<Props>>,
    battery_props: HashMap<String, Option<Props>>,
    done: Option<Box<dyn FnOnce(Vec<String>)>>,
}

pub type RescanHook = Box<dyn Fn(Box<dyn FnOnce(Vec<String>)>, HashSet<String>)>;

pub struct Bt {
    pub conn: Rc<dyn Conn>,
    pub has_object_manager: Cell<bool>,
    pub adapters: RefCell<OMap<Adapter>>,
    pub devices: RefCell<OMap<Device>>,
    /// Test seam (`mock.patch.object(bt, "rescan_async")`).
    pub rescan_hook: RefCell<Option<Rc<RescanHook>>>,
    /// ObjectManager signal subscriptions (Global Settings' `ObservedBluetooth`
    /// releases them in [`Bt::close`]).
    pub objmgr_ids: RefCell<Vec<u32>>,
}

impl Bt {
    pub fn new(conn: Rc<dyn Conn>) -> Rc<Self> {
        Rc::new(Self { conn, has_object_manager: Cell::new(false), adapters: RefCell::default(), devices: RefCell::default(),
                       rescan_hook: RefCell::new(None), objmgr_ids: RefCell::default() })
    }

    pub fn call_async(&self, path: &str, iface: &str, method: &str, params: Option<Variant>, timeout: i32,
                      cb: impl FnOnce(Reply) + 'static) {
        self.conn.call(BLUEZ, path, iface, method, params, timeout, Box::new(cb));
    }

    pub fn prop_set_async(&self, path: &str, iface: &str, prop: &str, value: Variant, cb: impl FnOnce(Reply) + 'static) {
        self.call_async(path, PROPS_IFACE, "Set", Some((iface, prop, value).to_variant()), 15000, cb);
    }

    pub fn subscribe(&self, sender: &str, path: &str, iface: &str, member: &str, cb: SignalHandler) -> u32 {
        self.conn.subscribe(Some(sender), Some(iface), Some(member), Some(path), None, cb)
    }

    pub fn subscribe_object_manager(&self, cb: SignalHandler) {
        let added = self.subscribe(BLUEZ, BLUEZ_ROOT, OBJMGR_IFACE, "InterfacesAdded", cb.clone());
        let removed = self.subscribe(BLUEZ, BLUEZ_ROOT, OBJMGR_IFACE, "InterfacesRemoved", cb);
        self.objmgr_ids.borrow_mut().extend([added, removed]);
    }

    /// `ObservedBluetooth.close()`: drop the ObjectManager subscriptions (the
    /// standalone app keeps them for its lifetime).
    pub fn close(&self) {
        for id in self.objmgr_ids.take() {
            self.conn.unsubscribe(id);
        }
    }

    /// `discover(cb)`: GetManagedObjects (8 s), else the introspection fallback.
    pub fn discover(self: &Rc<Self>, cb: impl FnOnce() + 'static) {
        let this = self.clone();
        self.call_async(BLUEZ_ROOT, OBJMGR_IFACE, "GetManagedObjects", None, 8000, move |reply| match reply {
            Ok(result) => {
                this.has_object_manager.set(true);
                this.apply_managed_objects(&result);
                cb();
            }
            Err(_) => {
                this.has_object_manager.set(false);
                this.rescan_async(Box::new(move |_| cb()), HashSet::new());
            }
        });
    }

    /// Apply a `(a{oa{sa{sv}}})` reply.
    pub fn apply_managed_objects(&self, reply: &Variant) {
        if reply.n_children() == 0 {
            return;
        }
        for entry in reply.child_value(0).iter() {
            let path = entry.child_value(0).str().unwrap_or_default().to_owned();
            let ifaces = interfaces_of(&entry.child_value(1));
            self.apply_native(&path, &ifaces);
        }
    }

    pub fn introspect_async(&self, path: &str, cb: impl FnOnce(Option<Vec<String>>) + 'static) {
        self.call_async(path, INTROSPECT_IFACE, "Introspect", None, 10000, move |reply| match reply {
            Err(_) => cb(None),
            Ok(result) => {
                let xml = if result.n_children() > 0 { result.child_value(0).str().unwrap_or_default().to_owned() } else { String::new() };
                cb(Some(node_names(&xml)));
            }
        });
    }

    pub fn get_all_async(&self, path: &str, iface: &str, cb: impl FnOnce(Option<Props>) + 'static) {
        self.call_async(path, PROPS_IFACE, "GetAll", Some((iface,).to_variant()), 10000,
                        move |reply| cb(reply.ok().map(|r| first_props(&r))));
    }

    /// `rescan_async(cb, fresh)`: `cb(changed_paths)` runs exactly once.
    pub fn rescan_async(self: &Rc<Self>, cb: Box<dyn FnOnce(Vec<String>)>, fresh: HashSet<String>) {
        let hook = self.rescan_hook.borrow().clone();
        if let Some(hook) = hook {
            return hook(cb, fresh);
        }
        let state = Rc::new(RefCell::new(Rescan { pending: 1, done: Some(cb), ..Default::default() }));
        let fresh = Rc::new(fresh);
        let this = self.clone();
        let st = state.clone();
        self.introspect_async(BLUEZ_ROOT, move |names| this.roots_loaded(&st, &fresh, names));
    }

    fn settle(self: &Rc<Self>, state: &Rc<RefCell<Rescan>>) {
        let finished = {
            let mut s = state.borrow_mut();
            s.pending -= 1;
            s.pending == 0
        };
        if finished {
            let (roots, adapter_props, adapter_children, device_props, battery_props, done) = {
                let mut s = state.borrow_mut();
                (s.roots.take(), std::mem::take(&mut s.adapter_props), std::mem::take(&mut s.adapter_children),
                 std::mem::take(&mut s.device_props), std::mem::take(&mut s.battery_props), s.done.take())
            };
            let changed = self.reconcile(roots.as_deref(), &adapter_props, &adapter_children, &device_props, &battery_props);
            if let Some(done) = done {
                done(changed);
            }
        }
    }

    fn roots_loaded(self: &Rc<Self>, state: &Rc<RefCell<Rescan>>, fresh: &Rc<HashSet<String>>, names: Option<Vec<String>>) {
        state.borrow_mut().roots = names.clone();
        for name in names.unwrap_or_default() {
            let ad_path = format!("{BLUEZ_ROOT}/{name}");
            state.borrow_mut().pending += 1;
            let (this, st, fr, p) = (self.clone(), state.clone(), fresh.clone(), ad_path.clone());
            if self.adapters.borrow().contains(&ad_path) && fresh.contains(&ad_path) {
                state.borrow_mut().adapter_props.insert(ad_path.clone(), Some(None));
                self.introspect_async(&ad_path, move |names| this.children_loaded(&st, &fr, &p, names));
            } else {
                self.get_all_async(&ad_path, ADAPTER_IFACE, move |props| this.adapter_loaded(&st, &fr, &p, props));
            }
        }
        self.settle(state);
    }

    fn adapter_loaded(self: &Rc<Self>, state: &Rc<RefCell<Rescan>>, fresh: &Rc<HashSet<String>>, ad_path: &str, props: Option<Props>) {
        let request = props.is_some() || self.adapters.borrow().contains(ad_path);
        state.borrow_mut().adapter_props.insert(ad_path.to_owned(), props.map(Some));
        if request {
            state.borrow_mut().pending += 1;
            let (this, st, fr, p) = (self.clone(), state.clone(), fresh.clone(), ad_path.to_owned());
            self.introspect_async(ad_path, move |names| this.children_loaded(&st, &fr, &p, names));
        }
        self.settle(state);
    }

    fn children_loaded(self: &Rc<Self>, state: &Rc<RefCell<Rescan>>, fresh: &Rc<HashSet<String>>, ad_path: &str,
                       names: Option<Vec<String>>) {
        state.borrow_mut().adapter_children.insert(ad_path.to_owned(), names.clone());
        for name in names.unwrap_or_default() {
            let dev_path = format!("{ad_path}/{name}");
            if self.devices.borrow().contains(&dev_path) && fresh.contains(&dev_path) {
                continue;
            }
            state.borrow_mut().pending += 1;
            let (this, st, p) = (self.clone(), state.clone(), dev_path.clone());
            self.get_all_async(&dev_path, DEVICE_IFACE, move |props| {
                st.borrow_mut().device_props.insert(p, props);
                this.settle(&st);
            });
            state.borrow_mut().pending += 1;
            let (this, st, p) = (self.clone(), state.clone(), dev_path.clone());
            self.get_all_async(&dev_path, BATTERY_IFACE, move |props| {
                st.borrow_mut().battery_props.insert(p, props);
                this.settle(&st);
            });
        }
        self.settle(state);
    }

    fn reconcile(&self, roots: Option<&[String]>, adapter_props: &HashMap<String, Option<AdapterEntry>>,
                 adapter_children: &HashMap<String, Option<Vec<String>>>, device_props: &HashMap<String, Option<Props>>,
                 battery_props: &HashMap<String, Option<Props>>) -> Vec<String> {
        let Some(roots) = roots else { return Vec::new() };
        let mut changed = Vec::new();
        let adapter_paths: Vec<String> = roots.iter().map(|child| format!("{BLUEZ_ROOT}/{child}")).collect();
        let mut adapters = self.adapters.borrow_mut();
        let mut devices = self.devices.borrow_mut();
        for ad_path in adapters.keys() {
            if !adapter_paths.contains(&ad_path) {
                adapters.remove(&ad_path);
                changed.push(ad_path);
            }
        }
        let mut existing: HashSet<String> = HashSet::new();
        for ad_path in &adapter_paths {
            // `aprops = adapter_props.get(ad_path)`: missing or failed = None.
            let entry = adapter_props.get(ad_path);
            let aprops: Option<&Props> = entry.and_then(|e| e.as_ref()).and_then(|e| e.as_ref());
            let is_none = entry.is_none_or(|e| e.is_none());
            if let Some(props) = aprops.filter(|p| !p.is_empty()) {
                let new_ad = adapter_from_props(props);
                if adapters.get(ad_path) != Some(&new_ad) {
                    adapters.insert(ad_path, new_ad);
                    changed.push(ad_path.clone());
                }
            } else if is_none && !adapters.contains(ad_path) {
                continue;
            }
            if !adapters.contains(ad_path) {
                continue;
            }
            let Some(Some(device_names)) = adapter_children.get(ad_path) else {
                let prefix = format!("{ad_path}/");
                existing.extend(devices.keys().into_iter().filter(|p| p.starts_with(&prefix)));
                continue;
            };
            for dev in device_names {
                let dev_path = format!("{ad_path}/{dev}");
                existing.insert(dev_path.clone());
                let Some(Some(dprops)) = device_props.get(&dev_path) else { continue };
                let mut new_dev = dev_from_props(dprops);
                let level = battery_props.get(&dev_path).and_then(|b| b.as_ref()).filter(|b| !b.is_empty())
                    .and_then(|b| py_int(b.get("Percentage")));
                if level.is_some() {
                    new_dev.battery = level;
                }
                if devices.get(&dev_path) != Some(&new_dev) {
                    devices.insert(&dev_path, new_dev);
                    changed.push(dev_path);
                }
            }
        }
        for path in devices.keys() {
            if !existing.contains(&path) {
                devices.remove(&path);
                changed.push(path);
            }
        }
        changed
    }

    /// Store one object's interfaces (`apply_native`).
    pub fn apply_native(&self, path: &str, ifaces: &[(String, Props)]) {
        let get = |name: &str| ifaces.iter().find(|(n, _)| n == name).map(|(_, p)| p);
        if let Some(props) = get(ADAPTER_IFACE) {
            self.adapters.borrow_mut().insert(path, adapter_from_props(props));
        }
        let mut devices = self.devices.borrow_mut();
        if let Some(props) = get(DEVICE_IFACE) {
            let previous = devices.get(path).and_then(|d| d.battery);
            let mut device = dev_from_props(props);
            if previous.is_some() {
                device.battery = previous;
            }
            devices.insert(path, device);
        }
        if let Some(props) = get(BATTERY_IFACE) {
            if let Some(device) = devices.get_mut(path) {
                if let Some(level) = props.get("Percentage").filter(|v| !v.is_null()) {
                    device.battery = py_int(Some(level));
                }
            }
        }
    }

    pub fn forget_object(&self, path: &str) {
        self.adapters.borrow_mut().remove(path);
        self.devices.borrow_mut().remove(path);
    }

    pub fn adapter(&self, path: Option<&str>) -> Option<Adapter> {
        path.and_then(|p| self.adapters.borrow().get(p).cloned())
    }

    pub fn device(&self, path: &str) -> Option<Device> {
        self.devices.borrow().get(path).cloned()
    }

    pub fn powered(&self, path: Option<&str>) -> bool {
        self.adapter(path).is_some_and(|a| a.powered)
    }
}

/// `a{sa{sv}}` → `[(interface, props)]`.
pub fn interfaces_of(variant: &Variant) -> Vec<(String, Props)> {
    variant.iter().map(|entry| {
        let name = entry.child_value(0).str().unwrap_or_default().to_owned();
        (name, props_of(&entry.child_value(1)))
    }).collect()
}

/// `(o a{sa{sv}})` InterfacesAdded payload / `(o as)` InterfacesRemoved.
pub fn object_signal(signal: &Signal) -> (String, Vec<(String, Props)>, Vec<String>) {
    let params = &signal.params;
    let path = if params.n_children() > 0 { params.child_value(0).str().unwrap_or_default().to_owned() } else { String::new() };
    if params.n_children() < 2 {
        return (path, Vec::new(), Vec::new());
    }
    let second = params.child_value(1);
    if signal.member == "InterfacesAdded" {
        (path, interfaces_of(&second), Vec::new())
    } else {
        (path, Vec::new(), second.iter().filter_map(|v| v.str().map(str::to_owned)).collect())
    }
}

// ------------------------------------------------------------------ errors

/// `short_err(err)`: the text after the last `": "`.
pub fn short_err(err: &BtError) -> String {
    match err.message.rsplit_once(": ") {
        Some((_, tail)) => tail.to_owned(),
        None => err.message.clone(),
    }
}

/// `dbus_error_name(err)`: `GDBus.Error:<name>` from the message.
pub fn dbus_error_name(err: &BtError) -> String {
    let pattern = regex::Regex::new(r"GDBus\.Error:([\w.]+)").unwrap();
    pattern.captures(&err.message).map(|c| c[1].to_owned()).unwrap_or_default()
}

pub const NOT_RESPONDING: &str = "Device not responding — make sure it's turned on, nearby and in pairing mode.";

/// `ERROR_ADVICE`: first match on the lowercase name + message wins.
pub const ERROR_ADVICE: &[(&[&str], &str)] = &[
    (&["authenticationcanceled", "authentication canceled"], "Pairing was cancelled."),
    (&["authenticationtimeout", "authentication timeout"],
     "Pairing timed out — confirm the request on the device and try again."),
    (&["authenticationfailed", "authenticationrejected", "authentication failed", "authentication rejected",
       "br-connection-key-missing", "key missing"],
     "Authentication failed — check the PIN or confirm the code on the device. If it was paired before, forget it and pair again."),
    (&["inprogress", "in progress", "already in progress", "br-connection-busy", "le-connection-busy"],
     "Already connecting… please wait a moment."),
    (&["alreadyconnected", "br-connection-already-connected", "le-connection-already-connected"],
     "The device is already connected."),
    (&["alreadyexists", "already exists"], "The device is already paired."),
    (&["adapter-not-powered", "notready", "not ready", "resource not ready"],
     "Bluetooth is not ready — make sure it is turned on."),
    (&["page-timeout", "connection-timeout", "connectionattemptfailed", "host is down", "no route to host", "abort-by-local",
       "abort-by-remote", "create-socket", "br-connection-unknown", "le-connection-unknown",
       "software caused connection abort", "connection-attempt-failed"],
     NOT_RESPONDING),
    (&["profile-unavailable", "protocol not available", "notavailable", "notsupported", "not supported",
       "br-connection-not-supported"],
     "This device doesn't offer a connection this computer can use."),
    (&["refused", "connection-canceled", "br-connection-canceled", "le-connection-canceled"],
     "The device refused the connection. Make sure it accepts new connections."),
    (&["doesnotexist", "does not exist", "unknownobject", "unknown object"], "The device is no longer available."),
    (&["noreply", "timedout", "timed out", "timeout was reached"], "The device didn't respond in time — try again."),
    (&["notpermitted", "not permitted", "notauthorized", "not authorized", "accessdenied", "permission denied"],
     "Bluetooth refused this request. Check that you are allowed to use Bluetooth."),
];

/// `friendly_err(err)` → (translated advice, raw details).
pub fn friendly_err(err: &BtError) -> (String, String) {
    let details = short_err(err);
    let haystack = format!("{} {}", dbus_error_name(err), details).to_lowercase();
    for (needles, advice) in ERROR_ADVICE {
        if needles.iter().any(|needle| haystack.contains(needle)) {
            return (tr(advice), details);
        }
    }
    (tr1("Something went wrong (%s).", details.clone()), details)
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
