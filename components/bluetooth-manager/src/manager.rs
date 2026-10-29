//! The toolkit-independent half of `class App` (btmanager.py): adapter
//! selection, the introspection poll, the discovery-session state machine
//! (`_drive_scan_session`), device actions with busy tracking, the event bar
//! rules, the list/row view models and the incoming-file UX. The GTK layer
//! (`ui.rs`) renders the views this produces and forwards user input; tests
//! drive it directly with a `FakeConn` BlueZ and a recording view.

use crate::backend::*;
use crate::dbus::{object_tuple, BtError, Conn, Reply, Signal};
use crate::env::Env;
use crate::i18n::{tr, tr1, trn, P};
use crate::settings;
use crate::transfer::{Notification, Notify, PromptHandle, ReceiveRequest, Receiver, ReceiverCallbacks};
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

pub const HINT_ADD: &str = "Press \"Add device\" to connect a new device.";

/// Everything `_apply_state` writes to widgets.
#[derive(Clone, Debug, PartialEq)]
pub struct StateView {
    pub adapter_choices: Vec<(String, String)>,
    /// `choices != self._adapter_choices`: rebuild the combo model.
    pub choices_changed: bool,
    pub active_adapter: Option<String>,
    pub adapter_sensitive: bool,
    pub adapter_tooltip: String,
    pub power_active: bool,
    pub power_sensitive: bool,
    pub add_sensitive: bool,
    pub add: Option<AddView>,
    pub ctrl_text: String,
    pub status_text: String,
    pub powered: bool,
    /// "devices" or "off".
    pub page: &'static str,
    pub off_title: String,
    pub off_description_visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AddView {
    pub owner_label: String,
    pub scan_sensitive: bool,
    pub scanning: bool,
    pub status: String,
}

/// `set_event(...)`.
#[derive(Clone, Debug, PartialEq)]
pub struct EventView {
    pub text: String,
    pub tooltip: String,
    /// The main bar's `.error` (`is_error and text`).
    pub error: bool,
    /// The Add device label's `.error` (`is_error`).
    pub add_error: bool,
}

/// One device row; a row widget is reused while its view is equal (the
/// Python `_state_signature`).
#[derive(Clone, Debug, PartialEq)]
pub struct RowView {
    pub path: String,
    pub dev: Device,
    pub owner: String,
    pub show_trust: bool,
    /// Compact two-line layout (`popup_mode and show_trust`).
    pub popup: bool,
    pub can_act: bool,
    pub busy: Vec<String>,
    pub forgetting: bool,
    pub powered: bool,
    pub power_pending: bool,
    pub lang: String,
    pub supports_push: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SectionView {
    pub title: String,
    pub count: usize,
    pub rows: Vec<RowView>,
    pub empty_text: String,
    pub empty_icon: &'static str,
    pub show_trust: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListsView {
    pub connected: SectionView,
    pub known: SectionView,
    pub nearby: Option<SectionView>,
}

/// What the GTK window (or a test recorder) implements.
pub trait View {
    fn apply(&self, state: &StateView);
    fn event(&self, event: &EventView);
    fn hide_add_event(&self);
    fn lists(&self, lists: &ListsView);
    /// `_sync_switches`: `(path, trusted)` for every known device.
    fn sync_switches(&self, trusted: &[(String, bool)]);
    /// The Gtk.Dialog fallback of an incoming-file prompt.
    fn receive_dialog(&self, request: &Rc<ReceiveRequest>, summary: &str, body: &str);
}

pub type ReceiverFactory = Rc<dyn Fn(ReceiverCallbacks, Rc<dyn Conn>) -> Rc<dyn Receiver>>;
pub type NotifierFactory = Rc<dyn Fn(Rc<dyn Conn>, &str) -> Option<Rc<dyn Notify>>>;

/// Replaceable services (Python tests patch these module attributes).
#[derive(Clone)]
pub struct Services {
    pub receiver: ReceiverFactory,
    pub notifier: NotifierFactory,
    pub download_dir: Rc<dyn Fn() -> PathBuf>,
    pub load_receive_mode: Rc<dyn Fn() -> String>,
    pub save_receive_mode: Rc<dyn Fn(&str) -> Result<(), String>>,
    /// `_open_received(path, folder)`; `None` = the real launcher.
    pub open_received: Option<Rc<dyn Fn(&Path, bool)>>,
    /// Watch the settings file (primary only).
    pub monitor_config: bool,
}

impl Default for Services {
    fn default() -> Self {
        Self {
            receiver: Rc::new(|callbacks, conn| Rc::new(crate::transfer::ObexReceiver::new(callbacks, Some(conn), None))),
            notifier: Rc::new(|conn, name| Some(Rc::new(crate::transfer::Notifier::new(conn, name, None)) as Rc<dyn Notify>)),
            download_dir: Rc::new(download_dir),
            load_receive_mode: Rc::new(|| settings::load_receive_mode(None)),
            save_receive_mode: Rc::new(|mode| settings::save_receive_mode(mode, None).map_err(|e| e.text().to_owned())),
            open_received: None,
            monitor_config: true,
        }
    }
}

/// `download_dir()`: the XDG download folder, else `~/Downloads`.
pub fn download_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(|| suite_core::paths::home().join("Downloads"))
}

#[derive(Default)]
struct State {
    default_adapter: Option<String>,
    display_adapter: Option<String>,
    power_pending: OMap<bool>,
    busy: HashMap<String, BTreeSet<String>>,
    rebuilding: bool,
    sub_ids: OMap<u32>,
    forgetting: HashSet<String>,
    poll_id: Option<u64>,
    poll_pending: bool,
    scan_pending: bool,
    scan_active: bool,
    scan_wanted: bool,
    scan_adapter: Option<String>,
    scan_revision: u64,
    add_open: bool,
    add_adapter: Option<String>,
    closing: bool,
    adapter_choices: Vec<(String, String)>,
    event_adapter: Option<String>,
    event_text: String,
    event_error: bool,
    add_event_visible: bool,
    receive_mode: String,
}

pub struct Manager {
    pub bt: Rc<Bt>,
    pub env: Rc<dyn Env>,
    pub popup_mode: bool,
    pub lang: String,
    pub services: Services,
    s: RefCell<State>,
    view: RefCell<Option<Rc<dyn View>>>,
    weak: RefCell<Weak<Manager>>,
    receive_bus: RefCell<Option<Rc<dyn Conn>>>,
    receiver: RefCell<Option<Rc<dyn Receiver>>>,
    notifier: RefCell<Option<Rc<dyn Notify>>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// Python `_transfers` (count of running senders).
    pub transfers: Cell<usize>,
    /// `_sync_receive_combo` after the settings file changed the mode.
    mode_listener: RefCell<Option<Rc<dyn Fn()>>>,
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/').map(|(head, _)| head.to_owned()).unwrap_or_default()
}

/// `_device_adapter(dev)`: the `Adapter` property, else the parent path.
pub fn device_adapter(dev: &Device, path: &str) -> String {
    if dev.adapter.is_empty() { parent_path(path) } else { dev.adapter.clone() }
}

/// `_supports_object_push`: no UUIDs yet ⇒ allowed.
pub fn supports_object_push(dev: &Device) -> bool {
    dev.uuids.is_empty() || dev.uuids.iter().any(|u| u.to_lowercase() == OBJECT_PUSH_UUID)
}

/// Python `str.casefold()` sort key.
fn casefold(text: &str) -> String {
    suite_core::pyjson::casefold(text)
}

impl Manager {
    pub fn new(bt: Rc<Bt>, env: Rc<dyn Env>, popup_mode: bool, lang: &str, services: Services) -> Rc<Self> {
        let receive_mode = (services.load_receive_mode)();
        let manager = Rc::new(Self {
            bt, env, popup_mode, lang: lang.into(), services, s: RefCell::new(State { receive_mode, ..Default::default() }),
            view: RefCell::new(None), weak: RefCell::new(Weak::new()), receive_bus: RefCell::new(None), receiver: RefCell::new(None),
            notifier: RefCell::new(None), monitor: RefCell::new(None), transfers: Cell::new(0),
            mode_listener: RefCell::new(None),
        });
        manager.weak.replace(Rc::downgrade(&manager));
        manager
    }

    fn me(&self) -> Weak<Manager> {
        self.weak.borrow().clone()
    }

    pub fn set_view(&self, view: Rc<dyn View>) {
        self.view.replace(Some(view));
    }

    fn view(&self) -> Option<Rc<dyn View>> {
        self.view.borrow().clone()
    }

    /// The end of `build_ui` and `App.__init__`: initial hint, discovery, state.
    pub fn start(&self) {
        self.set_event(&tr(HINT_ADD), false, None, None);
        let weak = self.me();
        self.bt.discover(move || if let Some(m) = weak.upgrade() { m.on_loaded() });
    }

    // ---------------------------------------------------------------- getters
    pub fn default_adapter(&self) -> Option<String> { self.s.borrow().default_adapter.clone() }
    pub fn set_default_adapter(&self, path: Option<&str>) { self.s.borrow_mut().default_adapter = path.map(str::to_owned); }
    pub fn closing(&self) -> bool { self.s.borrow().closing }
    pub fn scan_active(&self) -> bool { self.s.borrow().scan_active }
    pub fn scan_pending(&self) -> bool { self.s.borrow().scan_pending }
    pub fn scan_wanted(&self) -> bool { self.s.borrow().scan_wanted }
    pub fn scan_adapter(&self) -> Option<String> { self.s.borrow().scan_adapter.clone() }
    pub fn add_adapter(&self) -> Option<String> { self.s.borrow().add_adapter.clone() }
    pub fn add_open(&self) -> bool { self.s.borrow().add_open }
    pub fn event_text(&self) -> String { self.s.borrow().event_text.clone() }
    pub fn event_error(&self) -> bool { self.s.borrow().event_error }
    pub fn add_event_visible(&self) -> bool { self.s.borrow().add_event_visible }
    pub fn forgetting(&self) -> HashSet<String> { self.s.borrow().forgetting.clone() }
    pub fn watched(&self) -> Vec<String> { self.s.borrow().sub_ids.keys() }
    pub fn poll_id(&self) -> Option<u64> { self.s.borrow().poll_id }
    pub fn receive_mode(&self) -> String { self.s.borrow().receive_mode.clone() }
    pub fn set_receive_mode_value(&self, mode: &str) { self.s.borrow_mut().receive_mode = mode.into(); }
    pub fn power_pending(&self, adapter: &str) -> Option<bool> { self.s.borrow().power_pending.get(adapter).copied() }
    pub fn has_receiver(&self) -> bool { self.receiver.borrow().is_some() }
    pub fn set_notifier(&self, notifier: Option<Rc<dyn Notify>>) { self.notifier.replace(notifier); }
    pub fn set_closing(&self, closing: bool) { self.s.borrow_mut().closing = closing; }

    // ---------------------------------------------------------------- events
    pub fn on_loaded(&self) {
        if self.closing() {
            return;
        }
        let paths: Vec<String> = self.bt.adapters.borrow().keys().into_iter().chain(self.bt.devices.borrow().keys()).collect();
        for path in paths {
            self.watch_object(&path);
        }
        if self.bt.has_object_manager.get() {
            let weak = self.me();
            self.bt.subscribe_object_manager(Rc::new(move |signal| if let Some(m) = weak.upgrade() { m.on_objmgr_signal(signal) }));
        } else {
            self.start_polling();
        }
        self.pick_default_adapter();
        self.refresh_ui();
        if self.bt.adapters.borrow().is_empty() {
            self.set_event(&tr("Bluetooth adapter not found"), true, None, None);
        }
    }

    pub fn on_objmgr_signal(&self, signal: &Signal) {
        let (obj_path, ifaces, removed) = object_signal(signal);
        if signal.member == "InterfacesAdded" {
            self.bt.apply_native(&obj_path, &ifaces);
            let known = self.bt.adapters.borrow().contains(&obj_path) || self.bt.devices.borrow().contains(&obj_path);
            if known {
                self.watch_object(&obj_path);
            }
            self.pick_default_adapter();
        } else if signal.member == "InterfacesRemoved" {
            if removed.iter().any(|i| i == ADAPTER_IFACE) {
                self.bt.adapters.borrow_mut().remove(&obj_path);
                let prefix = format!("{obj_path}/");
                let owned: Vec<String> = self.bt.devices.borrow().iter()
                    .filter(|(p, d)| d.adapter == obj_path || p.starts_with(&prefix)).map(|(p, _)| p.clone()).collect();
                for dev_path in owned {
                    self.bt.devices.borrow_mut().remove(&dev_path);
                    self.unwatch_object(&dev_path);
                }
            }
            if removed.iter().any(|i| i == DEVICE_IFACE) {
                self.bt.devices.borrow_mut().remove(&obj_path);
            } else if removed.iter().any(|i| i == BATTERY_IFACE) {
                if let Some(dev) = self.bt.devices.borrow_mut().get_mut(&obj_path) {
                    dev.battery = None;
                }
            }
            let known = self.bt.adapters.borrow().contains(&obj_path) || self.bt.devices.borrow().contains(&obj_path);
            if !known {
                self.unwatch_object(&obj_path);
            }
            self.pick_default_adapter();
        }
        self.refresh_ui();
    }

    /// `(s, a{sv}, [as])`: only each interface's own properties are applied.
    pub fn on_props_changed(&self, signal: &Signal) {
        let params = &signal.params;
        if params.n_children() == 0 {
            return;
        }
        let iface = params.child_value(0).str().unwrap_or_default().to_owned();
        let changed = if params.n_children() > 1 { props_of(&params.child_value(1)) } else { Props::new() };
        let path = signal.path.as_str();
        if iface == ADAPTER_IFACE {
            if let Some(ad) = self.bt.adapters.borrow_mut().get_mut(path) {
                if let Some(v) = changed.get("Powered") { ad.powered = py_bool(Some(v)) }
                if let Some(v) = changed.get("Discovering") { ad.discovering = py_bool(Some(v)) }
                if let Some(v) = changed.get("Name") { ad.alias = py_str(Some(v)) }
                if let Some(v) = changed.get("Alias") { ad.alias = py_str(Some(v)) }
                if let Some(v) = changed.get("Address") { ad.address = py_str(Some(v)) }
            }
        }
        if iface == BATTERY_IFACE {
            if let (Some(dev), Some(level)) = (self.bt.devices.borrow_mut().get_mut(path), changed.get("Percentage")) {
                dev.battery = py_int(Some(level));
            }
        }
        if iface == DEVICE_IFACE {
            let mut event = None;
            if let Some(dev) = self.bt.devices.borrow_mut().get_mut(path) {
                if let Some(v) = changed.get("Connected") {
                    dev.connected = py_bool(Some(v));
                    let owner = if dev.adapter.is_empty() { parent_path(path) } else { dev.adapter.clone() };
                    let text = if dev.connected { tr1("Connected to %s", &dev.alias) } else { tr1("Disconnected from %s", &dev.alias) };
                    event = Some((text, owner));
                }
                if let Some(v) = changed.get("Adapter") { dev.adapter = py_str(Some(v)) }
                if let Some(v) = changed.get("Paired") { dev.paired = py_bool(Some(v)) }
                if let Some(v) = changed.get("Bonded") { dev.bonded = py_bool(Some(v)) }
                if let Some(v) = changed.get("Trusted") { dev.trusted = py_bool(Some(v)) }
                if let Some(v) = changed.get("Pairing") { dev.pairing = py_bool(Some(v)) }
                if let Some(v) = changed.get("Icon") { dev.icon = py_str(Some(v)) }
                if let Some(v) = changed.get("Class") { dev.class = v.as_i64().unwrap_or(0) }
                if changed.contains_key("UUIDs") { dev.uuids = uuids_of(changed.get("UUIDs")) }
                if let Some(v) = changed.get("Alias") {
                    let alias = py_str(Some(v));
                    dev.alias = if alias.is_empty() { dev.addr.clone() } else { alias };
                }
            }
            if let Some((text, owner)) = event {
                self.set_event(&text, false, Some(&owner), None);
            }
        }
        self.refresh_ui();
    }

    pub fn watch_object(&self, path: &str) {
        if self.s.borrow().sub_ids.contains(path) {
            return;
        }
        let weak = self.me();
        let id = self.bt.subscribe(BLUEZ, path, PROPS_IFACE, "PropertiesChanged",
                                   Rc::new(move |signal| if let Some(m) = weak.upgrade() { m.on_props_changed(signal) }));
        self.s.borrow_mut().sub_ids.insert(path, id);
    }

    pub fn unwatch_object(&self, path: &str) {
        let id = self.s.borrow_mut().sub_ids.remove(path);
        if let Some(id) = id.filter(|id| *id != 0) {
            self.bt.conn.unsubscribe(id);
        }
    }

    /// Device discovery and property updates must not undo a manual choice.
    pub fn pick_default_adapter(&self) {
        let adapters = self.bt.adapters.borrow();
        let current = self.s.borrow().default_adapter.clone();
        if current.as_deref().is_some_and(|c| adapters.contains(c)) {
            return;
        }
        let paths = adapters.sorted_keys();
        let pick = paths.iter().find(|p| adapters.get(p).is_some_and(|a| a.powered)).or(paths.first()).cloned();
        drop(adapters);
        self.s.borrow_mut().default_adapter = pick;
    }

    pub fn adapter_label(&self, path: Option<&str>) -> String {
        let ad = self.bt.adapter(path).unwrap_or_default();
        let tail = path.filter(|p| !p.is_empty()).map(|p| p.rsplit('/').next().unwrap_or("").to_owned());
        [Some(ad.alias), tail, Some(ad.address)].into_iter().flatten().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ")
    }

    /// The adapter combo's "changed" handler (after the applying-state guard).
    pub fn adapter_changed(&self, path: Option<&str>) {
        let Some(path) = path else { return };
        if !self.bt.adapters.borrow().contains(path) || self.default_adapter().as_deref() == Some(path) {
            return;
        }
        self.s.borrow_mut().default_adapter = Some(path.into());
        self.apply_state();
        self.rebuild_lists();
    }

    // ---------------------------------------------------------------- polling
    pub fn start_polling(&self) {
        if self.s.borrow().poll_id.is_some() {
            return;
        }
        let weak = self.me();
        let id = self.env.repeat(2000, Box::new(move || weak.upgrade().is_some_and(|m| m.poll_devices())));
        self.s.borrow_mut().poll_id = Some(id);
    }

    /// Without ObjectManager there are no InterfacesAdded/Removed signals.
    pub fn poll_devices(&self) -> bool {
        let start = !self.bt.has_object_manager.get() && !self.s.borrow().poll_pending;
        if start {
            self.s.borrow_mut().poll_pending = true;
            let fresh: HashSet<String> = self.s.borrow().sub_ids.keys().into_iter().collect();
            let weak = self.me();
            self.bt.rescan_async(Box::new(move |changed| if let Some(m) = weak.upgrade() { m.on_rescanned(changed) }), fresh);
        }
        true
    }

    pub fn on_rescanned(&self, changed: Vec<String>) {
        self.s.borrow_mut().poll_pending = false;
        if self.closing() {
            return;
        }
        for path in &changed {
            let known = self.bt.adapters.borrow().contains(path) || self.bt.devices.borrow().contains(path);
            if known { self.watch_object(path) } else { self.unwatch_object(path) }
        }
        if !changed.is_empty() {
            self.pick_default_adapter();
            self.refresh_ui();
        }
    }

    // ---------------------------------------------------------------- power
    /// The power switch's `notify::active` (after the applying-state guard).
    pub fn power_toggled(&self, desired: bool) {
        let Some(adapter) = self.default_adapter().filter(|a| self.bt.adapters.borrow().contains(a)) else {
            self.apply_state();
            return;
        };
        if self.s.borrow().power_pending.contains(&adapter) {
            return;
        }
        self.s.borrow_mut().power_pending.insert(&adapter, desired);
        let weak = self.me();
        let target = adapter.clone();
        self.bt.prop_set_async(&adapter, ADAPTER_IFACE, "Powered", desired.to_variant(), move |reply| {
            if let Some(m) = weak.upgrade() { m.on_power_done(reply.err(), &target, desired) }
        });
        self.refresh_ui();
    }

    fn on_power_done(&self, err: Option<BtError>, adapter: &str, desired: bool) {
        self.s.borrow_mut().power_pending.remove(adapter);
        match err {
            Some(err) => self.error_event(&err, Some(adapter), None),
            None => {
                if let Some(ad) = self.bt.adapters.borrow_mut().get_mut(adapter) {
                    ad.powered = desired;
                }
                let text = tr(if desired { "Bluetooth is on" } else { "Bluetooth is off" });
                self.set_event(&text, false, Some(adapter), None);
            }
        }
        self.refresh_ui();
    }

    // ---------------------------------------------------------------- add window / scanning
    /// `_on_add_device_clicked` before a window exists: may it open?
    pub fn can_open_add(&self) -> bool {
        self.bt.powered(self.default_adapter().as_deref())
    }

    /// The Add device window was built and shown.
    pub fn add_window_opened(&self) {
        {
            let mut s = self.s.borrow_mut();
            s.add_adapter = s.default_adapter.clone();
            s.add_open = true;
        }
        self.request_scan(true);
        self.apply_state();
        self.rebuild_lists();
    }

    /// `_on_add_window_destroyed` (the UI also drops nearby-only rows).
    pub fn add_window_destroyed(&self) {
        {
            let mut s = self.s.borrow_mut();
            s.add_open = false;
            s.add_adapter = None;
            s.add_event_visible = false;
        }
        self.request_scan(false);
    }

    pub fn scan_clicked(&self) {
        let scanning = {
            let s = self.s.borrow();
            if !s.add_open || s.scan_pending {
                return;
            }
            s.scan_wanted || (s.scan_active && s.scan_adapter == s.add_adapter)
        };
        self.request_scan(!scanning);
    }

    pub fn request_scan(&self, start: bool) {
        let hide = {
            let mut s = self.s.borrow_mut();
            s.scan_wanted = start;
            s.scan_revision += 1;
            let hide = s.add_open;
            if hide {
                s.add_event_visible = false;
            }
            hide
        };
        if hide {
            if let Some(view) = self.view() {
                view.hide_add_event();
            }
        }
        self.drive_scan_session();
        self.refresh_ui();
    }

    fn sync_scan_target(&self) -> bool {
        let mut s = self.s.borrow_mut();
        let mut changed = false;
        if s.add_open && s.add_adapter != s.default_adapter {
            s.add_adapter = s.default_adapter.clone();
            changed = true;
        }
        if s.scan_wanted && !self.bt.powered(s.add_adapter.as_deref()) {
            s.scan_wanted = false;
            changed = true;
        }
        if changed {
            s.scan_revision += 1;
        }
        changed
    }

    /// Balance this client's own BlueZ discovery session, including pending
    /// closes. `Adapter.Discovering` is shared with other applications: start
    /// our own session on opening and release only that one on closing. A
    /// close/reopen during a D-Bus call only updates the desired state; its
    /// callback reconciles before issuing the next operation.
    pub fn drive_scan_session(&self) {
        let (start, adapter, revision) = {
            let mut s = self.s.borrow_mut();
            if s.scan_pending {
                return;
            }
            // Losing power or unplugging the owner releases its BlueZ
            // sessions. Check the owner, not the newly selected adapter.
            if s.scan_active && !self.bt.powered(s.scan_adapter.as_deref()) {
                s.scan_active = false;
                s.scan_adapter = None;
            }
            let target = s.add_adapter.clone();
            let wanted = s.scan_wanted && s.add_open && self.bt.powered(target.as_deref()) && !s.closing;
            let (start, adapter) = if s.scan_active && (!wanted || s.scan_adapter != target) {
                (false, s.scan_adapter.clone())
            } else if wanted && !s.scan_active {
                (true, target)
            } else {
                return;
            };
            s.scan_pending = true;
            if start {
                s.scan_adapter = adapter.clone();
            }
            (start, adapter, s.scan_revision)
        };
        let weak = self.me();
        let path = adapter.clone().unwrap_or_default();
        self.bt.call_async(&path, ADAPTER_IFACE, if start { "StartDiscovery" } else { "StopDiscovery" }, None, DEFAULT_TIMEOUT,
                           move |reply| if let Some(m) = weak.upgrade() { m.scan_done(reply, start, adapter, revision) });
    }

    fn scan_done(&self, reply: Reply, start: bool, adapter: Option<String>, revision: u64) {
        // Hotplug signals can select a fallback before their idle UI refresh
        // runs. Reconcile it before handling the old call's result.
        self.pick_default_adapter();
        self.sync_scan_target();
        self.s.borrow_mut().scan_pending = false;
        match reply {
            Err(err) => {
                let adapter_on = self.bt.powered(adapter.as_deref());
                let request_changed = {
                    let mut s = self.s.borrow_mut();
                    if start || !adapter_on {
                        s.scan_active = false;
                        s.scan_adapter = None;
                    }
                    revision != s.scan_revision
                };
                if start && request_changed {
                    // A failed start on the previous adapter must not cancel a
                    // newer request to scan with a different adapter.
                    self.drive_scan_session();
                } else if !start && !adapter_on {
                    self.drive_scan_session();
                } else {
                    // Leave a failed stop retryable, without retrying forever.
                    let mut s = self.s.borrow_mut();
                    s.scan_wanted = s.scan_active && s.scan_adapter == s.add_adapter;
                }
                if start || adapter_on {
                    let add_adapter = self.add_adapter();
                    if !start && adapter != add_adapter {
                        self.error_event(&err, None, Some(&self.adapter_label(adapter.as_deref())));
                    } else {
                        self.error_event(&err, adapter.as_deref(), None);
                    }
                }
            }
            Ok(_) => {
                {
                    let mut s = self.s.borrow_mut();
                    s.scan_active = start;
                    s.scan_adapter = if start { adapter } else { None };
                }
                self.drive_scan_session();
            }
        }
        self.refresh_ui();
    }

    // ---------------------------------------------------------------- device actions
    fn current(&self, path: &str, snapshot: &Device) -> Device {
        self.bt.device(path).unwrap_or_else(|| snapshot.clone())
    }

    pub fn connect_clicked(&self, path: &str, snapshot: &Device) {
        let dev = self.current(path, snapshot);
        if self.is_busy(path, "connect") {
            return;
        }
        self.mark_busy(path, "connect");
        let weak = self.me();
        let p = path.to_owned();
        if dev.paired {
            self.bt.call_async(path, DEVICE_IFACE, "Connect", None, DEFAULT_TIMEOUT,
                               move |r| if let Some(m) = weak.upgrade() { m.on_connect_done(r.err(), &p, &dev) });
        } else {
            // Unknown device: pair (auto-bond), then connect.
            self.bt.call_async(path, DEVICE_IFACE, "Pair", None, 60000,
                               move |r| if let Some(m) = weak.upgrade() { m.on_pair_done(r.err(), &p, dev) });
        }
    }

    fn on_pair_done(&self, err: Option<BtError>, path: &str, dev: Device) {
        if let Some(err) = err {
            self.unmark_busy(path, "connect");
            self.error_event(&err, Some(&device_adapter(&dev, path)), None);
            self.refresh_ui();
            return;
        }
        let weak = self.me();
        let p = path.to_owned();
        self.bt.call_async(path, DEVICE_IFACE, "Connect", None, DEFAULT_TIMEOUT,
                           move |r| if let Some(m) = weak.upgrade() { m.on_connect_done(r.err(), &p, &dev) });
    }

    fn on_connect_done(&self, err: Option<BtError>, path: &str, dev: &Device) {
        self.unmark_busy(path, "connect");
        let owner = device_adapter(dev, path);
        match err {
            Some(err) => self.error_event(&err, Some(&owner), None),
            None => self.set_event(&tr1("Connected to %s", &dev.alias), false, Some(&owner), None),
        }
        self.refresh_ui();
    }

    pub fn disconnect_clicked(&self, path: &str, snapshot: &Device) {
        let dev = self.current(path, snapshot);
        if self.is_busy(path, "disconnect") {
            return;
        }
        self.mark_busy(path, "disconnect");
        let weak = self.me();
        let p = path.to_owned();
        self.bt.call_async(path, DEVICE_IFACE, "Disconnect", None, DEFAULT_TIMEOUT, move |r| {
            if let Some(m) = weak.upgrade() {
                m.unmark_busy(&p, "disconnect");
                if let Err(err) = r {
                    m.error_event(&err, Some(&device_adapter(&dev, &p)), None);
                }
                m.refresh_ui();
            }
        });
    }

    /// The trust switch (after the applying-state guard). `revert(value)`
    /// puts the switch back (guarded) when BlueZ refuses.
    pub fn trust_toggled(&self, path: &str, snapshot: &Device, desired: bool, revert: Box<dyn FnOnce(bool)>) {
        let dev = self.current(path, snapshot);
        if self.is_busy(path, "trust") {
            return;
        }
        self.mark_busy(path, "trust");
        let weak = self.me();
        let p = path.to_owned();
        self.bt.prop_set_async(path, DEVICE_IFACE, "Trusted", desired.to_variant(), move |r| {
            let Some(m) = weak.upgrade() else { return };
            m.unmark_busy(&p, "trust");
            let owner = device_adapter(&dev, &p);
            match r {
                Err(err) => {
                    revert(!desired);
                    m.error_event(&err, Some(&owner), None);
                }
                Ok(_) => {
                    let text = if desired { tr1("%s is now trusted", &dev.alias) } else { tr1("%s is no longer trusted", &dev.alias) };
                    m.set_event(&text, false, Some(&owner), None);
                }
            }
            m.refresh_ui();
        });
    }

    pub fn forget_clicked(&self, path: &str, snapshot: &Device) {
        let dev = self.current(path, snapshot);
        if self.is_busy(path, "forget") {
            return;
        }
        self.mark_busy(path, "forget");
        self.s.borrow_mut().forgetting.insert(path.into());
        let adapter = device_adapter(&dev, path);
        if !self.bt.adapters.borrow().contains(&adapter) {
            self.unmark_busy(path, "forget");
            self.s.borrow_mut().forgetting.remove(path);
            return;
        }
        let weak = self.me();
        let p = path.to_owned();
        self.bt.call_async(&adapter, ADAPTER_IFACE, "RemoveDevice", Some(object_tuple(path)), 20000, move |r| {
            let Some(m) = weak.upgrade() else { return };
            if r.is_err() {
                // The adapter is gone or the method unsupported: try Unpair.
                let weak = m.me();
                let p2 = p.clone();
                m.bt.call_async(&p, DEVICE_IFACE, "Unpair", None, 20000,
                                move |r2| if let Some(m) = weak.upgrade() { m.on_forget_done(r2.err(), &p2, &dev) });
            } else {
                m.on_forget_done(None, &p, &dev);
            }
        });
    }

    fn on_forget_done(&self, err: Option<BtError>, path: &str, dev: &Device) {
        self.unmark_busy(path, "forget");
        self.s.borrow_mut().forgetting.remove(path);
        let owner = device_adapter(dev, path);
        match err {
            Some(err) => self.error_event(&err, Some(&owner), None),
            None => self.set_event(&tr1("Device %s forgotten", &dev.alias), false, Some(&owner), None),
        }
        self.refresh_ui();
    }

    // ---------------------------------------------------------------- busy
    pub fn busy_flags(&self, path: &str) -> Vec<String> {
        self.s.borrow().busy.get(path).map(|set| set.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn is_busy(&self, path: &str, kind: &str) -> bool {
        self.s.borrow().busy.get(path).is_some_and(|set| set.contains(kind))
    }

    pub fn mark_busy(&self, path: &str, kind: &str) {
        self.s.borrow_mut().busy.entry(path.into()).or_default().insert(kind.into());
        self.refresh_ui();
    }

    pub fn unmark_busy(&self, path: &str, kind: &str) {
        let mut s = self.s.borrow_mut();
        if let Some(set) = s.busy.get_mut(path) {
            set.remove(kind);
            if set.is_empty() {
                s.busy.remove(path);
            }
        }
    }

    // ---------------------------------------------------------------- refresh
    pub fn refresh_ui(&self) {
        let schedule = {
            let mut s = self.s.borrow_mut();
            if s.closing || s.rebuilding {
                false
            } else {
                s.rebuilding = true;
                true
            }
        };
        if schedule {
            let weak = self.me();
            self.env.idle(Box::new(move || if let Some(m) = weak.upgrade() { m.do_refresh() }));
        }
    }

    pub fn do_refresh(&self) {
        self.s.borrow_mut().rebuilding = false;
        if self.closing() {
            return;
        }
        self.apply_state();
        self.rebuild_lists();
    }

    /// `_apply_state`.
    pub fn apply_state(&self) -> StateView {
        let current = self.default_adapter();
        if !current.as_deref().is_some_and(|c| self.bt.adapters.borrow().contains(c)) {
            self.pick_default_adapter();
        }
        let default = self.default_adapter();
        let stale_event = {
            let mut s = self.s.borrow_mut();
            let selection_changed = s.display_adapter != default;
            s.display_adapter = default.clone();
            selection_changed && s.event_adapter != default
        };
        let ad = self.bt.adapter(default.as_deref());
        let powered = ad.as_ref().is_some_and(|a| a.powered);
        let scan_changed = self.sync_scan_target();
        let owner_lost = {
            let s = self.s.borrow();
            s.scan_active && !self.bt.powered(s.scan_adapter.as_deref())
        };
        if scan_changed || owner_lost {
            self.drive_scan_session();
        }
        let choices: Vec<(String, String)> = self.bt.adapters.borrow().sorted_keys().into_iter()
            .map(|path| { let label = self.adapter_label(Some(&path)); (path, label) }).collect();
        let view = {
            let mut s = self.s.borrow_mut();
            let choices_changed = choices != s.adapter_choices;
            if choices_changed {
                s.adapter_choices = choices.clone();
            }
            let power_pending = default.as_deref().is_some_and(|d| s.power_pending.contains(d));
            let add = s.add_open.then(|| {
                let owner_label = if ad.is_some() { tr1("Controller: %s", self.adapter_label(s.add_adapter.as_deref())) }
                                  else { tr("Bluetooth adapter not found") };
                let scanning = powered && (s.scan_wanted || (s.scan_active && s.scan_adapter == s.add_adapter));
                AddView {
                    owner_label,
                    scan_sensitive: powered && !power_pending && !s.scan_pending,
                    scanning,
                    status: if ad.is_none() { tr("Bluetooth adapter not found") } else if !powered { tr("Bluetooth is off") }
                            else if scanning { tr("Scanning for nearby devices…") } else { tr("Scan stopped") },
                }
            });
            let name = ad.as_ref().map(|a| if a.alias.is_empty() { a.address.clone() } else { a.alias.clone() }).unwrap_or_default();
            let status_text = if ad.is_some() { tr1("Controller: %s", name) } else { tr("Bluetooth adapter not found") };
            StateView {
                adapter_tooltip: if choices.is_empty() { tr("Bluetooth adapter not found") } else { self.adapter_label(default.as_deref()) },
                adapter_sensitive: choices.len() > 1,
                adapter_choices: choices,
                choices_changed,
                active_adapter: default.clone(),
                power_active: default.as_deref().and_then(|d| s.power_pending.get(d).copied()).unwrap_or(powered),
                power_sensitive: ad.is_some() && !power_pending,
                add_sensitive: powered && !power_pending,
                add,
                ctrl_text: tr(if powered { "Bluetooth is on" } else if ad.is_some() { "Bluetooth is off" } else { "Bluetooth" }),
                status_text,
                powered,
                page: if powered { "devices" } else { "off" },
                off_title: tr(if ad.is_some() { "Bluetooth is off" } else { "Bluetooth adapter not found" }),
                off_description_visible: ad.is_some(),
            }
        };
        if let Some(v) = self.view() {
            v.apply(&view);
        }
        // Discovery finishes asynchronously. The startup no-adapter message must
        // clear as soon as an adapter arrives, and return if it disappears.
        // Unrelated action results stay while an adapter remains available.
        let (event_text, event_error) = (self.event_text(), self.event_error());
        let hints = [tr(HINT_ADD), tr("Bluetooth is off"), tr("Bluetooth is on"), tr("Bluetooth adapter not found")];
        if stale_event || ad.is_none() || hints.contains(&event_text) {
            let hint = if powered { tr(HINT_ADD) } else { view.off_title.clone() };
            if event_text != hint || event_error != ad.is_none() {
                self.set_event(&hint, ad.is_none(), None, None);
            }
        }
        if stale_event && self.add_open() {
            self.s.borrow_mut().add_event_visible = false;
            if let Some(v) = self.view() {
                v.hide_add_event();
            }
        }
        view
    }

    /// `_rebuild_lists`.
    pub fn rebuild_lists(&self) -> ListsView {
        let default = self.default_adapter();
        let ad = self.bt.adapter(default.as_deref());
        let powered = ad.as_ref().is_some_and(|a| a.powered);
        let (scanning, add_open, power_pending) = {
            let s = self.s.borrow();
            (s.scan_wanted || (s.scan_active && s.scan_adapter == s.add_adapter), s.add_open,
             default.as_deref().is_some_and(|d| s.power_pending.contains(d)))
        };
        let nearby_empty = if powered && scanning { tr("Scanning for nearby devices…") }
            else if !powered { tr(if ad.is_some() { "Bluetooth is off" } else { "Bluetooth adapter not found" }) }
            else { tr("No nearby devices yet — start a scan.") };
        let mut devs: Vec<(String, Device)> = self.bt.devices.borrow().iter()
            .filter(|(p, d)| default.as_deref().is_some_and(|def| !def.is_empty() && device_adapter(d, p) == def))
            .map(|(p, d)| (p.clone(), d.clone())).collect();
        devs.sort_by_key(|(_, d)| casefold(&d.alias));
        let mut buckets: [Vec<(String, Device)>; 3] = Default::default();
        for (path, dev) in devs {
            let index = if dev.connected { 0 } else if dev.paired || dev.bonded || dev.trusted { 1 } else { 2 };
            buckets[index].push((path, dev));
        }
        let s = self.s.borrow();
        let make = |index: usize, title: &str, empty: String, icon: &'static str, show_trust: bool| {
            let rows: Vec<RowView> = buckets[index].iter().map(|(path, dev)| {
                let owner = device_adapter(dev, path);
                RowView {
                    path: path.clone(), dev: dev.clone(), owner: owner.clone(), show_trust,
                    popup: self.popup_mode && show_trust,
                    can_act: self.bt.powered(Some(&owner)) && !s.power_pending.contains(&owner),
                    busy: s.busy.get(path).map(|b| b.iter().cloned().collect()).unwrap_or_default(),
                    forgetting: s.forgetting.contains(path), powered, power_pending, lang: self.lang.clone(),
                    supports_push: supports_object_push(dev),
                }
            }).collect();
            SectionView { title: tr(title), count: rows.len(), rows, empty_text: empty, empty_icon: icon, show_trust }
        };
        let lists = ListsView {
            connected: make(0, "Connected devices", tr("No connected devices"), "bluetooth-symbolic", true),
            known: make(1, "Known devices", tr("No known devices"), "bookmark-new-symbolic", true),
            nearby: add_open.then(|| make(2, "Nearby devices", nearby_empty, "edit-find-symbolic", false)),
        };
        drop(s);
        if let Some(v) = self.view() {
            v.lists(&lists);
            let trusted: Vec<(String, bool)> = self.bt.devices.borrow().iter().map(|(p, d)| (p.clone(), d.trusted)).collect();
            v.sync_switches(&trusted);
        }
        lists
    }

    // ---------------------------------------------------------------- event bar
    /// `_error_event`: plain advice, the raw reason as a tooltip.
    pub fn error_event(&self, err: &BtError, adapter: Option<&str>, prefix: Option<&str>) {
        let (mut advice, details) = friendly_err(err);
        if let Some(prefix) = prefix.filter(|p| !p.is_empty()) {
            advice = format!("{prefix}: {advice}");
        }
        self.set_event(&advice, true, adapter, Some(&tr1("Details: %s", details)));
    }

    /// `set_event`: ignored when `adapter` is not the selected adapter.
    pub fn set_event(&self, text: &str, is_error: bool, adapter: Option<&str>, details: Option<&str>) {
        {
            let mut s = self.s.borrow_mut();
            if let Some(adapter) = adapter {
                if s.default_adapter.as_deref() != Some(adapter) {
                    return;
                }
            }
            s.event_adapter = s.default_adapter.clone();
            s.event_text = text.into();
            s.event_error = is_error && !text.is_empty();
            if s.add_open {
                s.add_event_visible = !text.is_empty();
            }
        }
        let tooltip = match details.filter(|d| !d.is_empty()) {
            Some(details) => format!("{text}\n{details}"),
            None => text.to_owned(),
        };
        if let Some(v) = self.view() {
            v.event(&EventView { text: text.into(), tooltip, error: is_error && !text.is_empty(), add_error: is_error });
        }
    }

    // ---------------------------------------------------------------- closing
    /// `_on_window_destroyed` (the UI destroys dialogs and the add window).
    pub fn window_destroyed(&self) {
        let poll = {
            let mut s = self.s.borrow_mut();
            s.closing = true;
            s.poll_id.take()
        };
        self.stop_receiver();
        if let Some(id) = poll {
            self.env.cancel(id);
        }
    }

    // ---------------------------------------------------------------- sending
    /// `_send_device_snapshot`: the device plus its owner's address now.
    pub fn send_snapshot(&self, path: &str, dev: &Device) -> SendTarget {
        let owner = self.bt.adapter(Some(&device_adapter(dev, path)));
        SendTarget { path: path.into(), dev: dev.clone(), source_address: owner.map(|o| o.address) }
    }

    /// `_send_error`: revalidate the device, its owner's power and busy state.
    pub fn send_error(&self, target: &SendTarget) -> Option<String> {
        let current = self.bt.device(&target.path);
        let adapter = device_adapter(&target.dev, &target.path);
        let owner = self.bt.adapter(Some(&adapter));
        let unavailable = match (&current, &owner) {
            (Some(current), Some(owner)) => owner.address.is_empty() || current.addr != target.dev.addr
                || device_adapter(current, &target.path) != adapter
                || target.source_address.as_deref() != Some(owner.address.as_str()),
            _ => true,
        };
        if unavailable {
            return Some(tr("The device is no longer available."));
        }
        let (current, owner) = (current.unwrap(), owner.unwrap());
        if !owner.powered || self.power_pending(&adapter) == Some(false) {
            return Some(tr("Bluetooth is off"));
        }
        if !supports_object_push(&current) {
            return Some(tr("This device does not support file transfers."));
        }
        if !self.busy_flags(&target.path).is_empty() {
            return Some(tr("Device is busy. Try again shortly."));
        }
        None
    }

    // ---------------------------------------------------------------- receiving
    /// Serve incoming files from this (primary, long-lived) process.
    pub fn start_receiver(&self, connection: Rc<dyn Conn>) {
        if self.receive_bus.borrow().is_some() || self.closing() {
            return;
        }
        self.receive_bus.replace(Some(connection.clone()));
        self.notifier.replace((self.services.notifier)(connection, &tr("Bluetooth")));
        if self.services.monitor_config {
            let file = gio::File::for_path(settings::config_file());
            match file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>) {
                Ok(monitor) => {
                    let weak = self.me();
                    monitor.connect_changed(move |_, _, _, _| if let Some(m) = weak.upgrade() { m.on_receive_config_changed() });
                    self.monitor.replace(Some(monitor));
                }
                Err(_) => {
                    self.monitor.replace(None);
                }
            }
        }
        self.apply_receive_mode();
    }

    pub fn on_receive_config_changed(&self) {
        let mode = (self.services.load_receive_mode)();
        if mode != self.receive_mode() {
            self.set_receive_mode_value(&mode);
            let listener = self.mode_listener.borrow().clone();
            if let Some(listener) = listener {
                listener();
            }
            self.apply_receive_mode();
        }
    }

    pub fn set_mode_listener(&self, listener: Rc<dyn Fn()>) {
        self.mode_listener.replace(Some(listener));
    }

    /// The Receive files drop-down (after the applying-state guard).
    /// `Err(text)` = could not save; the UI re-syncs the control.
    pub fn receive_mode_selected(&self, index: u32) -> Result<(), ()> {
        let Some(mode) = settings::RECEIVE_MODES.get(index as usize) else { return Ok(()) };
        if *mode == self.receive_mode() {
            return Ok(());
        }
        if let Err(text) = (self.services.save_receive_mode)(mode) {
            let short = text.rsplit_once(": ").map(|(_, t)| t.to_owned()).unwrap_or(text);
            self.set_event(&tr1("Error: %s", short), true, None, None);
            return Err(());
        }
        self.set_receive_mode_value(mode);
        self.apply_receive_mode();
        Ok(())
    }

    pub fn apply_receive_mode(&self) {
        let Some(bus) = self.receive_bus.borrow().clone() else { return };
        let want = self.receive_mode() == "ask" && !self.closing();
        let have = self.receiver.borrow().is_some();
        if want && !have {
            let callbacks = self.receiver_callbacks();
            let receiver = (self.services.receiver)(callbacks, bus);
            self.receiver.replace(Some(receiver.clone()));
            receiver.start();
        } else if !want && have {
            if let Some(receiver) = self.receiver.take() {
                receiver.stop();
            }
        }
    }

    fn receiver_callbacks(&self) -> ReceiverCallbacks {
        let w = || self.me();
        let (a, b, c, d) = (w(), w(), w(), w());
        ReceiverCallbacks {
            on_request: Some(Rc::new(move |r| if let Some(m) = a.upgrade() { m.on_receive_request(r) })),
            on_cancel: Some(Rc::new(move |r| if let Some(m) = b.upgrade() { m.on_receive_cancelled(r) })),
            on_progress: Some(Rc::new(move |r| if let Some(m) = c.upgrade() { m.on_receive_progress(r, false) })),
            on_done: Some(Rc::new(move |r| if let Some(m) = d.upgrade() { m.on_receive_done(r) })),
            on_status: Some(Rc::new(|registered, error| {
                if let (Some(error), false) = (error, registered) {
                    eprintln!("btmanager: cannot receive files over Bluetooth: {}", error.message);
                }
            })),
        }
    }

    pub fn stop_receiver(&self) {
        if let Some(receiver) = self.receiver.take() {
            receiver.stop();
        }
        if let Some(monitor) = self.monitor.take() {
            monitor.cancel();
        }
        if let Some(notifier) = self.notifier.take() {
            notifier.destroy();
        }
    }

    pub fn sender_name(&self, address: &str) -> String {
        let wanted = address.to_uppercase();
        if wanted.is_empty() {
            return tr("A nearby device");
        }
        for (_, dev) in self.bt.devices.borrow().iter() {
            if dev.addr.to_uppercase() == wanted {
                return if dev.alias.is_empty() { address.to_owned() } else { dev.alias.clone() };
            }
        }
        address.to_owned()
    }

    /// `_receive_text(request)`: device, name, size.
    pub fn receive_text(&self, request: &ReceiveRequest) -> Vec<(&'static str, P)> {
        let size = request.size.get();
        vec![
            ("device", self.sender_name(&request.address.borrow()).into()),
            ("name", request.name.borrow().clone().into()),
            ("size", if size > 0 { glib::format_size(size).to_string() } else { tr("unknown size") }.into()),
        ]
    }

    pub fn on_receive_request(&self, request: &Rc<ReceiveRequest>) {
        if self.closing() || self.receive_mode() != "ask" {
            request.reject();
            return;
        }
        let text = self.receive_text(request);
        let summary = tr("Incoming file");
        let body = trn("%(device)s wants to send you “%(name)s” (%(size)s).", &text);
        let notifier = self.notifier.borrow().clone();
        match notifier.filter(|n| n.supports_actions()) {
            Some(notifier) => {
                let mut note = Notification::new(&summary, &body);
                note.actions = vec![("accept".into(), tr("Accept")), ("decline".into(), tr("Decline"))];
                let (w1, w2, w3) = (self.me(), self.me(), self.me());
                let (r1, r2, r3, r4) = (request.clone(), request.clone(), request.clone(), request.clone());
                note.on_action = Some(Rc::new(move |key| if let Some(m) = w1.upgrade() { m.answer_receive(&r1, key == "accept") }));
                note.on_closed = Some(Rc::new(move |_| if let Some(m) = w2.upgrade() { m.answer_receive(&r2, false) }));
                note.urgency = 2;
                note.timeout = 0;
                note.on_shown = Some(Box::new(move |ident| r3.user_data.borrow_mut().notification = Some(ident)));
                let (s, b) = (summary.clone(), body.clone());
                note.on_error = Some(Box::new(move |_| if let Some(m) = w3.upgrade() { m.receive_dialog(&r4, &s, &b) }));
                notifier.notify(note);
            }
            None => self.receive_dialog(request, &summary, &body),
        }
    }

    fn receive_dialog(&self, request: &Rc<ReceiveRequest>, summary: &str, body: &str) {
        if request.state() != "pending" || self.closing() {
            return;
        }
        if let Some(v) = self.view() {
            v.receive_dialog(request, summary, body);
        }
    }

    fn close_receive_prompt(&self, request: &ReceiveRequest) {
        let dialog = request.user_data.borrow_mut().dialog.take();
        if let Some(dialog) = dialog.filter(|d| d.alive()) {
            dialog.destroy();
        }
    }

    pub fn answer_receive(&self, request: &Rc<ReceiveRequest>, accept: bool) {
        if request.state() != "pending" {
            return;
        }
        self.close_receive_prompt(request);
        if !accept {
            request.reject();
            let ident = request.user_data.borrow_mut().notification.take().unwrap_or(0);
            let notifier = self.notifier.borrow().clone();
            if let Some(n) = notifier {
                n.close(ident);
            }
            return;
        }
        if request.accept(&(self.services.download_dir)()).is_none() {
            let error = request.error.borrow().as_ref().map(short_err).unwrap_or_default();
            let mut values = self.receive_text(request);
            values.push(("error", error.into()));
            let body = trn("Couldn't save “%(name)s”: %(error)s", &values);
            self.notify_receive(request, &tr("File transfer failed"), &body, Notification::new("", ""));
            return;
        }
        request.user_data.borrow_mut().last_update = Some(0);
        self.on_receive_progress(request, true);
    }

    fn notify_receive(&self, request: &Rc<ReceiveRequest>, summary: &str, body: &str, mut options: Notification) {
        let Some(notifier) = self.notifier.borrow().clone() else { return };
        options.summary = summary.into();
        options.body = body.into();
        options.replaces = request.user_data.borrow().notification.unwrap_or(0);
        let r = request.clone();
        options.on_shown = Some(Box::new(move |ident| r.user_data.borrow_mut().notification = Some(ident)));
        notifier.notify(options);
    }

    pub fn on_receive_progress(&self, request: &Rc<ReceiveRequest>, force: bool) {
        if request.state() != "active" {
            return;
        }
        let size = request.size.get();
        let fraction = if size > 0 { (request.transferred.get() as f64 / size as f64).min(1.0) } else { 0.0 };
        let percent = (fraction * 100.0) as i64;
        let now = self.env.monotonic();
        {
            let mut data = request.user_data.borrow_mut();
            if !force && (data.percent == Some(percent) || now - data.last_update.unwrap_or(0) < 500000) {
                return;
            }
            data.percent = Some(percent);
            data.last_update = Some(now);
        }
        let text = self.receive_text(request);
        let total = text.iter().find(|(k, _)| *k == "size").map(|(_, v)| v.clone()).unwrap_or(P::S(String::new()));
        let body = trn("“%(name)s” from %(device)s", &text) + "\n"
            + &trn("%(done)s of %(total)s", &[("done", glib::format_size(request.transferred.get()).to_string().into()), ("total", total)]);
        let options = Notification { value: Some(percent), timeout: 0, urgency: 1, ..Default::default() };
        self.notify_receive(request, &tr("Receiving file…"), &body, options);
    }

    pub fn on_receive_done(&self, request: &Rc<ReceiveRequest>) {
        let mut text = self.receive_text(request);
        if request.state() == "complete" {
            let path = request.path.borrow().clone().unwrap_or_default();
            let folder = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty()).unwrap_or_else(|| path.to_string_lossy().into_owned());
            text.push(("folder", folder.into()));
            let mut options = Notification { urgency: 1, timeout: -1, ..Default::default() };
            options.actions = vec![("open".into(), tr("Open")), ("folder".into(), tr("Show in folder"))];
            let weak = self.me();
            let target = path.clone();
            options.on_action = Some(Rc::new(move |key| if let Some(m) = weak.upgrade() { m.open_received(&target, key == "folder") }));
            self.notify_receive(request, &tr("File received"), &trn("“%(name)s” from %(device)s was saved to %(folder)s.", &text), options);
            self.set_event(&tr1("Received %s", request.name.borrow().clone()), false, None, None);
        } else {
            let options = Notification { urgency: 1, timeout: -1, ..Default::default() };
            self.notify_receive(request, &tr("File transfer failed"), &trn("“%(name)s” from %(device)s was not received.", &text), options);
        }
    }

    pub fn on_receive_cancelled(&self, request: &Rc<ReceiveRequest>) {
        self.close_receive_prompt(request);
        let ident = request.user_data.borrow_mut().notification.take().unwrap_or(0);
        let notifier = self.notifier.borrow().clone();
        if let Some(n) = notifier {
            n.close(ident);
        }
    }

    /// Track the GTK fallback prompt so answering elsewhere closes it.
    pub fn set_receive_prompt(&self, request: &ReceiveRequest, dialog: Rc<dyn PromptHandle>) {
        request.user_data.borrow_mut().dialog = Some(dialog);
    }

    pub fn open_received(&self, path: &Path, folder: bool) {
        if let Some(hook) = &self.services.open_received {
            hook(path, folder);
            return;
        }
        let uri = gio::File::for_path(path).uri().to_string();
        if !folder {
            launch_uri(&uri);
            return;
        }
        let directory = gio::File::for_path(path.parent().unwrap_or(Path::new("/"))).uri().to_string();
        let Some(bus) = self.receive_bus.borrow().clone() else {
            launch_uri(&directory);
            return;
        };
        let params = (vec![uri], "").to_variant();
        bus.call("org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1", "ShowItems",
                 Some(params), 5000, Box::new(move |reply| if reply.is_err() { launch_uri(&directory) }));
    }
}

fn launch_uri(uri: &str) {
    let text = uri.to_owned();
    gio::AppInfo::launch_default_for_uri_async(uri, None::<&gio::AppLaunchContext>, None::<&gio::Cancellable>, move |result| {
        if let Err(error) = result {
            eprintln!("btmanager: cannot open {text}: {}", error.message());
        }
    });
}

/// `dict(dev, _source_address=...)`.
#[derive(Clone, Debug)]
pub struct SendTarget {
    pub path: String,
    pub dev: Device,
    pub source_address: Option<String>,
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
