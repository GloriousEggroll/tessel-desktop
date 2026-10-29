//! `network_backend.py`: asynchronous NetworkManager controls and plain
//! presentation models, over raw D-Bus (no libnm `NMClient`).
//!
//! Starting the backend only reads: `GetNameOwner`, one
//! `ObjectManager.GetManagedObjects` and `Settings.Connection.GetSettings`
//! (never `GetSecrets`), then incremental `InterfacesAdded/Removed`,
//! `PropertiesChanged` and `Updated` signals. Mutations happen only in the
//! named action methods; secrets never enter the models.

use crate::compat;
use crate::libnm::{self, Connection, Setting};
use crate::model::*;
use crate::nm::{self, *};
use glib::prelude::*;
use glib::Variant;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use suite_core::bus::{Bus, BusError, Call, Signal};
use suite_core::scheduler::{GlibScheduler, Scheduler};

/// Observable state (the Python backend's public attributes).
#[derive(Clone, Debug, Default)]
pub struct State {
    pub ready: bool,
    pub error: String,
    pub busy: HashSet<String>,
    pub networking_enabled: bool,
    pub wireless_enabled: bool,
    pub wireless_hardware_enabled: bool,
    pub wwan_enabled: bool,
    pub wwan_hardware_enabled: bool,
    pub hostname: String,
    pub connectivity: String,
    pub connectivity_uri: String,
    pub devices: Map<Device>,
    pub networks: Map<WifiNetwork>,
    pub profiles: Map<Profile>,
    pub active: Map<ActiveConnection>,
}

impl State {
    pub fn new() -> Self {
        Self { connectivity: "Unknown".into(), ..Default::default() }
    }
}

/// A saved profile as NetworkManager holds it (libnm `NMRemoteConnection`).
#[derive(Clone, Debug)]
pub struct Remote {
    pub path: String,
    pub uuid: String,
    pub settings: Settings,
    pub flags: u32,
    pub version_id: u64,
}

impl Remote {
    pub fn external(&self) -> bool {
        self.flags & SETTINGS_FLAG_EXTERNAL != 0
    }

    pub fn to_connection(&self) -> Result<Connection, glib::Error> {
        let connection = Connection::from_dbus(&settings_variant(&self.settings))?;
        connection.set_path(&self.path);
        Ok(connection)
    }
}

pub type Connector = Box<dyn Fn(Box<dyn FnOnce(Result<Rc<dyn Bus>, BusError>)>)>;

/// The libnm-only checks Python made on libnm objects. `None` = not known yet
/// (libnm's cache lags our D-Bus cache): treated as false and re-checked.
pub trait Matcher {
    /// Called once when the backend attaches; `ready` asks for a refresh.
    fn start(&self, _ready: Box<dyn Fn()>) {}
    /// `NMDevice.connection_compatible(remote)`.
    fn compatible(&self, objects: &Objects, device: &str, connection: &str, settings: &Settings) -> Option<bool>;
    /// `NMAccessPoint.connection_valid(remote)`.
    fn ap_valid(&self, objects: &Objects, device: &str, ap: &str, connection: &str, settings: &Settings) -> Option<bool>;
    /// `NMDevice.get_description()`.
    fn description(&self, objects: &Objects, device: &str) -> Option<String>;
}

/// The daemon: a read-only libnm `NMClient` created on attach.
#[derive(Default)]
pub struct LibnmMatcher(Rc<RefCell<Option<libnm::Client>>>);

impl Matcher for LibnmMatcher {
    fn start(&self, ready: Box<dyn Fn()>) {
        let _ = libnm::api();
        let slot = self.0.clone();
        libnm::Client::new_async(move |client| {
            let ok = client.is_some();
            slot.replace(client);
            if ok {
                ready();
            }
        });
    }
    fn compatible(&self, _: &Objects, device: &str, connection: &str, _: &Settings) -> Option<bool> {
        self.0.borrow().as_ref()?.connection_compatible(device, connection)
    }
    fn ap_valid(&self, _: &Objects, device: &str, ap: &str, connection: &str, _: &Settings) -> Option<bool> {
        self.0.borrow().as_ref()?.ap_connection_valid(device, ap, connection)
    }
    fn description(&self, _: &Objects, device: &str) -> Option<String> {
        self.0.borrow().as_ref()?.description(device)
    }
}

/// The status stream: no libnm at all (its output needs none of these).
pub struct NoMatcher;

impl Matcher for NoMatcher {
    fn compatible(&self, _: &Objects, _: &str, _: &str, _: &Settings) -> Option<bool> {
        Some(false)
    }
    fn ap_valid(&self, _: &Objects, _: &str, _: &str, _: &str, _: &Settings) -> Option<bool> {
        Some(false)
    }
    fn description(&self, _: &Objects, _: &str) -> Option<String> {
        Some(String::new())
    }
}

pub struct Options {
    pub connector: Connector,
    pub scheduler: Rc<dyn Scheduler>,
    pub matcher: Rc<dyn Matcher>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connector: Box::new(|done| compat::connect(gio::BusType::System, move |r| done(r.map(|(bus, _)| bus)))),
            scheduler: Rc::new(GlibScheduler::default()),
            matcher: Rc::new(LibnmMatcher::default()),
        }
    }
}

impl Options {
    /// `network-status`: same backend without libnm.
    pub fn status() -> Self {
        Self { matcher: Rc::new(NoMatcher), ..Self::default() }
    }
}

pub struct Inner {
    state: RefCell<State>,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    connector: Connector,
    scheduler: Rc<dyn Scheduler>,
    matcher: Rc<dyn Matcher>,
    matcher_started: Cell<bool>,
    misses: Cell<u32>,
    bus: RefCell<Option<Rc<dyn Bus>>>,
    client: Cell<u64>,
    next_client: Cell<u64>,
    owner: RefCell<Option<String>>,
    objects: RefCell<Objects>,
    settings: RefCell<HashMap<String, Settings>>,
    closed: Cell<bool>,
    started: Cell<bool>,
    generation: Cell<u64>,
    load_serial: Cell<u64>,
    loading: Cell<bool>,
    dirty: Cell<bool>,
    sources: RefCell<HashMap<String, u64>>,
    subscriptions: RefCell<Vec<u32>>,
    scans: RefCell<HashMap<String, i64>>,
    weak: RefCell<Weak<Inner>>,
}

#[derive(Clone)]
pub struct NetworkBackend(pub Rc<Inner>);

fn lock_detail(details: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(entry) = details.iter_mut().find(|(k, _)| k == key) {
        entry.1 = value;
    } else {
        details.push((key.into(), value));
    }
}

fn ssid_label(ssid: &[u8]) -> String {
    if ssid.is_empty() {
        return "Hidden network".into();
    }
    libnm::ssid_to_utf8(ssid)
}

/// `make_wifi_connection(network, password)`: a saved Wi-Fi profile in
/// memory with keyring-owned (AGENT_OWNED) secrets, normalized and verified.
pub fn make_wifi_connection(network: &WifiNetwork, password: Option<&str>) -> Result<Connection, String> {
    if network.ssid.is_empty() {
        return Err("Use Hidden Network to enter this network's name and security.".into());
    }
    if network.security == "wpa-enterprise" {
        return Err("This network requires enterprise authentication settings.".into());
    }
    if network.security == "wpa-personal" {
        if let Some(password) = password {
            if !libnm::wpa_psk_valid(password) {
                return Err("A WPA password must contain 8–63 characters or 64 hexadecimal digits.".into());
            }
        }
    }
    if network.security == "sae" {
        if let Some(password) = password {
            if !(1..=63).contains(&password.len()) {
                return Err("A WPA3 password must contain 1–63 bytes.".into());
            }
        }
    }
    let connection = Connection::new().map_err(|e| e.message().to_owned())?;
    let general = Setting::new("connection").ok_or("The connection settings are invalid.")?;
    general.set_str("id", Some(&network.label));
    general.set_str("uuid", Some(&libnm::uuid_generate()));
    general.set_str("type", Some("802-11-wireless"));
    general.set_bool("autoconnect", true);
    // Match desktop-applet behaviour: credentials belong to the signed-in user.
    general.set_strv("permissions", &[format!("user:{}:", current_user())]);
    connection.add_setting(&general);
    let wireless = Setting::new("802-11-wireless").ok_or("The connection settings are invalid.")?;
    wireless.set_bytes("ssid", Some(&network.ssid));
    wireless.set_str("mode", Some("infrastructure"));
    connection.add_setting(&wireless);
    if network.security != "open" {
        let security = Setting::new("802-11-wireless-security").ok_or("The connection settings are invalid.")?;
        let key_mgmt = match network.security.as_str() {
            "wpa-personal" => "wpa-psk",
            "sae" => "sae",
            "owe" => "owe",
            "wep" => "none",
            _ => return Err("This wireless security type requires advanced connection settings.".into()),
        };
        security.set_str("key-mgmt", Some(key_mgmt));
        if network.security == "sae" || network.security == "owe" {
            security.set_int("pmf", 3);
        }
        if network.security == "wep" {
            let mut key_type = 1;
            if let Some(password) = password {
                if !libnm::wep_key_valid(password, key_type) {
                    key_type = 2;
                }
                if !libnm::wep_key_valid(password, key_type) {
                    return Err("Enter a valid WEP key or passphrase.".into());
                }
                security.set_str("wep-key0", Some(password));
            }
            security.set_int("wep-key-type", key_type as i64);
            security.set_int("wep-tx-keyidx", 0);
            security.set_secret_flags("wep-key0", libnm::AGENT_OWNED).map_err(|e| e.message().to_owned())?;
        } else if network.security == "wpa-personal" || network.security == "sae" {
            if let Some(password) = password {
                security.set_str("psk", Some(password));
            }
            security.set_secret_flags("psk", libnm::AGENT_OWNED).map_err(|e| e.message().to_owned())?;
        }
        connection.add_setting(&security);
    }
    for name in ["ipv4", "ipv6"] {
        let ip = Setting::new(name).ok_or("The connection settings are invalid.")?;
        ip.set_str("method", Some("auto"));
        connection.add_setting(&ip);
    }
    connection.normalize().map_err(|e| e.message().to_owned())?;
    connection.verify().map_err(|e| e.message().to_owned())?;
    Ok(connection)
}

pub fn current_user() -> String {
    // getpass.getuser(): LOGNAME, USER, LNAME, USERNAME, then the passwd entry.
    for key in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                return value;
            }
        }
    }
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if !entry.is_null() && !(*entry).pw_name.is_null() {
            return std::ffi::CStr::from_ptr((*entry).pw_name).to_string_lossy().into_owned();
        }
    }
    String::new()
}

impl NetworkBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static, options: Options) -> Self {
        let inner = Rc::new(Inner {
            state: RefCell::new(State::new()), on_changed: Box::new(on_changed), on_error: Box::new(on_error),
            connector: options.connector, scheduler: options.scheduler, matcher: options.matcher, matcher_started: Cell::new(false),
            misses: Cell::new(0), bus: RefCell::new(None), client: Cell::new(0),
            next_client: Cell::new(0), owner: RefCell::new(None), objects: RefCell::new(Objects::new()),
            settings: RefCell::new(HashMap::new()), closed: Cell::new(false), started: Cell::new(false),
            generation: Cell::new(0), load_serial: Cell::new(0), loading: Cell::new(false), dirty: Cell::new(false),
            sources: RefCell::new(HashMap::new()), subscriptions: RefCell::new(Vec::new()), scans: RefCell::new(HashMap::new()),
            weak: RefCell::new(Weak::new()),
        });
        inner.weak.replace(Rc::downgrade(&inner));
        Self(inner)
    }

    pub fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn weak(&self) -> Weak<Inner> {
        self.0.weak.borrow().clone()
    }

    pub fn state(&self) -> Ref<'_, State> {
        self.0.state.borrow()
    }

    /// Tests only: change the observable state directly.
    pub fn state_mut(&self) -> RefMut<'_, State> {
        self.0.state.borrow_mut()
    }

    pub fn ready(&self) -> bool {
        self.state().ready
    }

    pub fn error(&self) -> String {
        self.state().error.clone()
    }

    pub fn generation(&self) -> u64 {
        self.0.generation.get()
    }

    /// The current NetworkManager client (0 = none), for stale-reply checks.
    pub fn client(&self) -> u64 {
        self.0.client.get()
    }

    pub fn bus(&self) -> Option<Rc<dyn Bus>> {
        self.0.bus.borrow().clone()
    }

    pub fn closed(&self) -> bool {
        self.0.closed.get()
    }

    pub fn start(&self) {
        if self.0.started.get() || self.0.closed.get() {
            return;
        }
        self.0.started.set(true);
        self.connect();
    }

    fn connect(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.generation.set(self.0.generation.get() + 1);
        let generation = self.0.generation.get();
        let weak = self.weak();
        (self.0.connector)(Box::new(move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = NetworkBackend(inner);
            if backend.0.closed.get() || generation != backend.0.generation.get() {
                return;
            }
            match result {
                Ok(bus) => backend.attach(bus),
                Err(error) => {
                    backend.fail_bus(&error);
                    let b = backend.clone();
                    backend.later("reconnect", 3000, Box::new(move || b.connect()));
                }
            }
        }));
    }

    /// Use an existing bus connection (tests and `connect`).
    pub fn attach(&self, bus: Rc<dyn Bus>) {
        self.detach();
        self.0.started.set(true);
        let serial = self.0.next_client.get() + 1;
        self.0.next_client.set(serial);
        self.0.client.set(serial);
        self.0.bus.replace(Some(bus.clone()));
        let weak = self.weak();
        let owner_handler = Rc::new(move |signal: &Signal| {
            if let Some(inner) = weak.upgrade() {
                let backend = NetworkBackend(inner);
                if let Some((name, _old, new)) = signal.params.get::<(String, String, String)>() {
                    if name == NM_BUS {
                        backend.owner_changed(&new);
                    }
                }
            }
        });
        let weak = self.weak();
        let nm_handler = Rc::new(move |signal: &Signal| {
            if let Some(inner) = weak.upgrade() {
                NetworkBackend(inner).nm_signal(signal);
            }
        });
        let mut subscriptions = vec![
            bus.subscribe(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"), Some("NameOwnerChanged"),
                          Some("/org/freedesktop/DBus"), Some(NM_BUS), owner_handler),
            bus.subscribe(Some(NM_BUS), None, None, None, None, nm_handler),
        ];
        let weak = self.weak();
        bus.on_closed(Some(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                let backend = NetworkBackend(inner);
                backend.not_running();
                backend.0.client.set(0);
                let b = backend.clone();
                backend.later("reconnect", 3000, Box::new(move || b.connect()));
            }
        })));
        self.0.subscriptions.borrow_mut().append(&mut subscriptions);
        if !self.0.matcher_started.replace(true) {
            let weak = self.weak();
            self.0.matcher.start(Box::new(move || if let Some(inner) = weak.upgrade() { NetworkBackend(inner).changed() }));
        }
        let weak = self.weak();
        bus.call(Call::new("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner",
                           Some((NM_BUS,).to_variant())), Box::new(move |reply| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = NetworkBackend(inner);
            if backend.0.closed.get() || backend.0.client.get() != serial {
                return;
            }
            match reply {
                Ok(value) => {
                    let owner = value.get::<(String,)>().map(|v| v.0).unwrap_or_default();
                    backend.owner_changed(&owner);
                }
                Err(_) => backend.owner_changed(""),
            }
        }));
    }

    fn detach(&self) {
        if let Some(bus) = self.0.bus.take() {
            for id in self.0.subscriptions.borrow_mut().drain(..) {
                bus.unsubscribe(id);
            }
            bus.on_closed(None);
            bus.cancel_reads();
        }
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.load_serial.set(self.0.load_serial.get() + 1);
        for (_, id) in self.0.sources.borrow_mut().drain() {
            self.0.scheduler.cancel(id);
        }
        self.detach();
        self.0.client.set(0);
        let mut state = self.0.state.borrow_mut();
        state.ready = false;
        state.busy.clear();
    }

    fn later(&self, name: &str, milliseconds: u32, callback: Box<dyn FnOnce()>) {
        if self.0.sources.borrow().contains_key(name) || self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        let key = name.to_owned();
        let id = self.0.scheduler.timeout(milliseconds, Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.sources.borrow_mut().remove(&key);
                if !inner.closed.get() {
                    callback();
                }
            }
        }));
        self.0.sources.borrow_mut().insert(name.to_owned(), id);
    }

    fn notify(&self) {
        if !self.0.closed.get() {
            (self.0.on_changed)();
        }
    }

    /// `_fail(message)`: record, report and notify; always `false`.
    pub fn fail(&self, message: &str) -> bool {
        self.0.state.borrow_mut().error = message.to_owned();
        if !self.0.closed.get() {
            (self.0.on_error)(message);
            self.notify();
        }
        false
    }

    fn fail_bus(&self, error: &BusError) -> bool {
        let message = friendly_bus(error);
        if message.is_empty() {
            // The person cancelled (for example a polkit prompt): no error.
            self.notify();
            return false;
        }
        self.fail(&message)
    }

    fn changed(&self) {
        let b = self.clone();
        self.later("refresh", 60, Box::new(move || b.refresh()));
    }

    // --- daemon tracking ---

    fn owner_changed(&self, owner: &str) {
        if owner.is_empty() {
            self.0.owner.replace(None);
            self.not_running();
        } else {
            self.0.owner.replace(Some(owner.to_owned()));
            self.load();
        }
    }

    /// `get_nm_running() == False`.
    fn not_running(&self) {
        self.0.load_serial.set(self.0.load_serial.get() + 1);
        self.0.loading.set(false);
        self.0.objects.borrow_mut().clear();
        self.0.settings.borrow_mut().clear();
        self.0.scans.borrow_mut().clear();
        {
            let mut state = self.0.state.borrow_mut();
            if state.ready {
                self.0.generation.set(self.0.generation.get() + 1);
            }
            state.ready = false;
            state.busy.clear();
            state.devices.clear();
            state.networks.clear();
            state.profiles.clear();
            state.active.clear();
            state.error = "NetworkManager is not running.".into();
        }
        self.notify();
    }

    fn load(&self) {
        if self.0.closed.get() {
            return;
        }
        if self.0.loading.get() {
            self.0.dirty.set(true);
            return;
        }
        let Some(bus) = self.bus() else { return };
        self.0.loading.set(true);
        self.0.dirty.set(false);
        let serial = self.0.load_serial.get() + 1;
        self.0.load_serial.set(serial);
        let weak = self.weak();
        bus.call(Call::new(NM_BUS, OBJECT_MANAGER_PATH, "org.freedesktop.DBus.ObjectManager", "GetManagedObjects", None),
                 Box::new(move |reply| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = NetworkBackend(inner);
            if backend.0.closed.get() || backend.0.load_serial.get() != serial {
                return;
            }
            match reply {
                Ok(value) => backend.loaded(serial, &value),
                Err(error) => {
                    backend.0.loading.set(false);
                    let name = error.name.clone().unwrap_or_default();
                    if name.ends_with("ServiceUnknown") || name.ends_with("NameHasNoOwner") {
                        backend.not_running();
                    } else {
                        backend.fail_bus(&error);
                        let b = backend.clone();
                        backend.later("reload", 3000, Box::new(move || b.load()));
                    }
                }
            }
        }));
    }

    fn loaded(&self, serial: u64, value: &Variant) {
        let mut objects = Objects::new();
        let dict = if value.type_().as_str() == "(a{oa{sa{sv}}})" { value.child_value(0) } else { value.clone() };
        for entry in dict.iter() {
            let path = entry.child_value(0).str().unwrap_or_default().to_owned();
            let mut interfaces = HashMap::new();
            for iface in entry.child_value(1).iter() {
                interfaces.insert(iface.child_value(0).str().unwrap_or_default().to_owned(), props(&iface.child_value(1)));
            }
            objects.insert(path, interfaces);
        }
        let paths = prop_paths(&objects, SETTINGS_PATH, SETTINGS_IFACE, "Connections");
        self.0.objects.replace(objects);
        self.0.settings.borrow_mut().retain(|path, _| paths.contains(path));
        let pending = Rc::new(Cell::new(paths.len()));
        let finish = {
            let weak = self.weak();
            Rc::new(move || {
                let Some(inner) = weak.upgrade() else { return };
                let backend = NetworkBackend(inner);
                if backend.0.load_serial.get() != serial || backend.0.closed.get() {
                    return;
                }
                backend.0.loading.set(false);
                backend.refresh();
                if backend.0.dirty.get() {
                    backend.load();
                }
            })
        };
        if paths.is_empty() {
            finish();
            return;
        }
        let Some(bus) = self.bus() else { return };
        for path in paths {
            let (weak, pending, finish) = (self.weak(), pending.clone(), finish.clone());
            let key = path.clone();
            bus.call(Call::new(NM_BUS, &path, CONNECTION_IFACE, "GetSettings", None), Box::new(move |reply| {
                let Some(inner) = weak.upgrade() else { return };
                if inner.load_serial.get() != serial {
                    return;
                }
                match reply {
                    Ok(value) => { inner.settings.borrow_mut().insert(key, parse_settings(&value)); }
                    Err(_) => { inner.settings.borrow_mut().remove(&key); }
                }
                pending.set(pending.get() - 1);
                if pending.get() == 0 {
                    finish();
                }
            }));
        }
    }

    fn fetch_settings(&self, path: &str) {
        let Some(bus) = self.bus() else { return };
        let weak = self.weak();
        let serial = self.0.load_serial.get();
        let key = path.to_owned();
        bus.call(Call::new(NM_BUS, path, CONNECTION_IFACE, "GetSettings", None), Box::new(move |reply| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.load_serial.get() != serial || inner.closed.get() {
                return;
            }
            match reply {
                Ok(value) => { inner.settings.borrow_mut().insert(key, parse_settings(&value)); }
                Err(_) => { inner.settings.borrow_mut().remove(&key); }
            }
            NetworkBackend(inner).changed();
        }));
    }

    fn nm_signal(&self, signal: &Signal) {
        if self.0.closed.get() || self.0.owner.borrow().is_none() {
            return;
        }
        let member = signal.member.as_str();
        let iface = signal.interface.as_str();
        // Failure reports (state-changed handlers) work while loading too.
        match (iface, member) {
            (DEVICE_IFACE, "StateChanged") => {
                if let Some((new, _old, reason)) = signal.params.get::<(u32, u32, u32)>() {
                    if new == 120 {
                        let iface_name = prop_str(&self.0.objects.borrow(), &signal.path, DEVICE_IFACE, "Interface");
                        self.fail(&format!("Connection on {iface_name} failed: {}.", device_reason_name(reason)));
                    }
                }
            }
            (ACTIVE_IFACE, "StateChanged") => {
                if let Some((state, reason)) = signal.params.get::<(u32, u32)>() {
                    if state == 4 && ![1, 0, 2, 11].contains(&reason) {
                        let id = prop_str(&self.0.objects.borrow(), &signal.path, ACTIVE_IFACE, "Id");
                        let id = if id.is_empty() { "Connection".to_owned() } else { id };
                        self.fail(&format!("{id} failed: {}.", active_reason_name(reason)));
                    }
                }
            }
            (VPN_IFACE, "VpnStateChanged") => {
                if let Some((state, reason)) = signal.params.get::<(u32, u32)>() {
                    if state == 6 {
                        let id = prop_str(&self.0.objects.borrow(), &signal.path, ACTIVE_IFACE, "Id");
                        let id = if id.is_empty() { "VPN connection".to_owned() } else { id };
                        self.fail(&format!("{id} failed: {}.", vpn_reason_name(reason)));
                    }
                }
            }
            _ => {}
        }
        if self.0.loading.get() {
            if matches!(member, "PropertiesChanged" | "InterfacesAdded" | "InterfacesRemoved" | "Updated") {
                self.0.dirty.set(true);
            }
            return;
        }
        match member {
            "InterfacesAdded" => {
                if let Some(path) = signal.params.try_child_value(0).and_then(|v| v.str().map(String::from)) {
                    let mut added_connection = false;
                    {
                        let mut objects = self.0.objects.borrow_mut();
                        let object = objects.entry(path.clone()).or_default();
                        for entry in signal.params.child_value(1).iter() {
                            let name = entry.child_value(0).str().unwrap_or_default().to_owned();
                            added_connection |= name == CONNECTION_IFACE;
                            object.insert(name, props(&entry.child_value(1)));
                        }
                    }
                    if added_connection {
                        self.fetch_settings(&path);
                    }
                }
                self.changed();
            }
            "InterfacesRemoved" => {
                if let Some(path) = signal.params.try_child_value(0).and_then(|v| v.str().map(String::from)) {
                    let names: Vec<String> = signal.params.child_value(1).iter().filter_map(|v| v.str().map(String::from)).collect();
                    let mut objects = self.0.objects.borrow_mut();
                    if let Some(object) = objects.get_mut(&path) {
                        for name in &names {
                            object.remove(name);
                        }
                        if object.is_empty() {
                            objects.remove(&path);
                        }
                    }
                    if names.iter().any(|n| n == CONNECTION_IFACE) {
                        self.0.settings.borrow_mut().remove(&path);
                    }
                }
                self.changed();
            }
            "PropertiesChanged" => {
                let params = &signal.params;
                let (target, changed) = if iface == "org.freedesktop.DBus.Properties" {
                    (params.try_child_value(0).and_then(|v| v.str().map(String::from)), params.try_child_value(1))
                } else {
                    // Legacy per-interface PropertiesChanged(a{sv}).
                    (Some(iface.to_owned()), params.try_child_value(0))
                };
                if let (Some(target), Some(changed)) = (target, changed) {
                    if changed.type_().as_str() == "a{sv}" {
                        let mut objects = self.0.objects.borrow_mut();
                        let entry = objects.entry(signal.path.clone()).or_default().entry(target).or_default();
                        for (k, v) in props(&changed) {
                            entry.insert(k, v);
                        }
                    }
                }
                self.changed();
            }
            "Updated" if iface == CONNECTION_IFACE => {
                self.fetch_settings(&signal.path);
            }
            "StateChanged" | "VpnStateChanged" | "AccessPointAdded" | "AccessPointRemoved" => self.changed(),
            _ => {}
        }
    }

    // --- models ---

    /// Rebuild every model from the D-Bus cache (`refresh()`; never mutates).
    pub fn refresh(&self) {
        if self.0.closed.get() || self.0.client.get() == 0 || self.0.owner.borrow().is_none() || self.0.loading.get() {
            return;
        }
        let objects = self.0.objects.borrow();
        let settings = self.0.settings.borrow();
        let recovered = !self.0.state.borrow().ready;
        let mut state = self.0.state.borrow_mut();
        state.ready = true;
        if recovered {
            state.error.clear();
        }
        state.networking_enabled = prop_bool(&objects, NM_PATH, NM_IFACE, "NetworkingEnabled");
        state.wireless_enabled = prop_bool(&objects, NM_PATH, NM_IFACE, "WirelessEnabled");
        state.wireless_hardware_enabled = prop_bool(&objects, NM_PATH, NM_IFACE, "WirelessHardwareEnabled");
        state.wwan_enabled = prop_bool(&objects, NM_PATH, NM_IFACE, "WwanEnabled");
        state.wwan_hardware_enabled = prop_bool(&objects, NM_PATH, NM_IFACE, "WwanHardwareEnabled");
        state.hostname = prop_str(&objects, SETTINGS_PATH, SETTINGS_IFACE, "Hostname");
        state.connectivity = nm::connectivity(prop_u32(&objects, NM_PATH, NM_IFACE, "Connectivity")).into();
        state.connectivity_uri = prop_str(&objects, NM_PATH, NM_IFACE, "ConnectivityCheckUri");
        let devices: Vec<String> = prop_paths(&objects, NM_PATH, NM_IFACE, "Devices").into_iter()
            .filter(|p| objects.get(p).is_some_and(|o| o.contains_key(DEVICE_IFACE))).collect();
        let matcher = self.0.matcher.clone();
        let misses = Cell::new(0u32);
        let known = |value: Option<bool>| -> bool {
            if value.is_none() {
                misses.set(misses.get() + 1);
            }
            value.unwrap_or(false)
        };
        let mut profiles: Vec<(String, String)> = Vec::new();
        for path in prop_paths(&objects, SETTINGS_PATH, SETTINGS_IFACE, "Connections") {
            if let Some(uuid) = settings.get(&path).and_then(|s| get_str(s, "connection", "uuid")).filter(|u| !u.is_empty()) {
                if !profiles.iter().any(|(u, _)| *u == uuid) {
                    profiles.push((uuid, path));
                }
            }
        }
        let active_paths: Vec<String> = prop_paths(&objects, NM_PATH, NM_IFACE, "ActiveConnections").into_iter()
            .filter(|p| objects.get(p).is_some_and(|o| o.contains_key(ACTIVE_IFACE))).collect();

        let mut active_models: Map<ActiveConnection> = Map::new();
        for path in &active_paths {
            let p = |n: &str| prop_str(&objects, path, ACTIVE_IFACE, n);
            let connection_type = p("Type");
            let kind = type_kind(&connection_type);
            let id = { let id = p("Id"); if id.is_empty() { "Unnamed connection".to_owned() } else { id } };
            let status = prop_u32(&objects, path, ACTIVE_IFACE, "State");
            let mut details = vec![("Connection".to_owned(), id.clone()), ("Type".to_owned(), title(&kind)),
                                   ("State".to_owned(), active_state(status).to_owned())];
            for (family, name) in [(4u8, "Ip4Config"), (6, "Ip6Config")] {
                if let Some(config) = prop_path(&objects, path, ACTIVE_IFACE, name) {
                    for (k, v) in ip_details(&objects, &config, family) {
                        lock_detail(&mut details, &k, v);
                    }
                }
            }
            let mut model = ActiveConnection::new(path, &p("Uuid"), &id, &kind);
            model.device_ids = prop_paths(&objects, path, ACTIVE_IFACE, "Devices");
            model.state = status;
            model.state_label = active_state(status).into();
            model.default = prop_bool(&objects, path, ACTIVE_IFACE, "Default");
            model.default6 = prop_bool(&objects, path, ACTIVE_IFACE, "Default6");
            model.vpn = prop_bool(&objects, path, ACTIVE_IFACE, "Vpn");
            model.details = details;
            model.external = prop_u32(&objects, path, ACTIVE_IFACE, "StateFlags") & ACTIVATION_FLAG_EXTERNAL != 0;
            active_models.insert(path.clone(), model);
        }

        let mut profile_models: Map<Profile> = Map::new();
        for (uuid, path) in &profiles {
            let s = &settings[path];
            let connection_type = get_str(s, "connection", "type").unwrap_or_default();
            let kind = type_kind(&connection_type);
            let name = get_str(s, "connection", "id").filter(|n| !n.is_empty()).unwrap_or_else(|| "Unnamed connection".into());
            let mut model = Profile::new(uuid, &name, &kind);
            model.interface = get_str(s, "connection", "interface-name").unwrap_or_default();
            model.device_ids = devices.iter().filter(|d| known(matcher.compatible(&objects, d, path, s))).cloned().collect();
            model.active_ids = active_models.iter().filter(|(_, a)| a.profile_id == *uuid).map(|(k, _)| k.clone()).collect();
            if s.contains_key("802-11-wireless") {
                model.ssid = get_bytes(s, "802-11-wireless", "ssid").unwrap_or_default();
                model.security = profile_security(s);
            }
            model.timestamp = get_u64(s, "connection", "timestamp").unwrap_or(0);
            model.autoconnect = get_bool(s, "connection", "autoconnect").unwrap_or(true);
            model.external = prop_u32(&objects, path, CONNECTION_IFACE, "Flags") & SETTINGS_FLAG_EXTERNAL != 0;
            profile_models.insert(uuid.clone(), model);
        }

        let mut device_models: Map<Device> = Map::new();
        let mut groups: Map<WifiNetwork> = Map::new();
        let mut scans = self.0.scans.borrow_mut();
        for path in &devices {
            let d = |n: &str| prop_str(&objects, path, DEVICE_IFACE, n);
            let kind = device_kind(prop_u32(&objects, path, DEVICE_IFACE, "DeviceType"));
            let interface = d("Interface");
            let status = prop_u32(&objects, path, DEVICE_IFACE, "State");
            let mut details = vec![("Interface".to_owned(), interface.clone()), ("Type".to_owned(), title(&kind)),
                                   ("State".to_owned(), device_state(status).to_owned())];
            for (label, value) in [("Hardware address", d("HwAddress")), ("Driver", d("Driver"))] {
                if !value.is_empty() {
                    details.push((label.into(), value));
                }
            }
            let mtu = prop_u32(&objects, path, DEVICE_IFACE, "Mtu");
            if mtu != 0 {
                details.push(("MTU".into(), mtu.to_string()));
            }
            if status == 120 || status == 20 {
                let reason = prop(&objects, path, DEVICE_IFACE, "StateReason").and_then(|v| v.get::<(u32, u32)>()).map(|v| v.1).unwrap_or(0);
                details.push(("Reason".into(), capitalize(&device_reason_name(reason))));
            }
            for (family, name) in [(4u8, "Ip4Config"), (6, "Ip6Config")] {
                if let Some(config) = prop_path(&objects, path, DEVICE_IFACE, name) {
                    for (k, v) in ip_details(&objects, &config, family) {
                        lock_detail(&mut details, &k, v);
                    }
                }
            }
            let mut carrier = false;
            if kind == "wired" {
                carrier = prop_bool(&objects, path, WIRED_IFACE, "Carrier");
                lock_detail(&mut details, "Cable", if carrier { "Connected" } else { "Unplugged" }.into());
                let speed = prop_u32(&objects, path, WIRED_IFACE, "Speed");
                if speed != 0 {
                    lock_detail(&mut details, "Link speed", format!("{speed} Mb/s"));
                }
            }
            if kind == "wifi" {
                let bitrate = prop_u32(&objects, path, WIRELESS_IFACE, "Bitrate");
                if bitrate != 0 {
                    lock_detail(&mut details, "Link speed", format!("{} Mb/s", compat::format_g(bitrate as f64 / 1000.0)));
                }
                let current = prop_path(&objects, path, WIRELESS_IFACE, "ActiveAccessPoint")
                    .filter(|ap| objects.get(ap).is_some_and(|o| o.contains_key(AP_IFACE)));
                if let Some(ap) = &current {
                    lock_detail(&mut details, "Network", ssid_label(&prop_bytes(&objects, ap, AP_IFACE, "Ssid")));
                    lock_detail(&mut details, "Access point", prop_str(&objects, ap, AP_IFACE, "HwAddress"));
                    lock_detail(&mut details, "Frequency", format!("{} MHz", prop_u32(&objects, ap, AP_IFACE, "Frequency")));
                    lock_detail(&mut details, "Signal", format!("{}%", prop_u32(&objects, ap, AP_IFACE, "Strength")));
                }
                let last_scan = prop_i64(&objects, path, WIRELESS_IFACE, "LastScan");
                if let Some(&scan) = scans.get(path) {
                    if last_scan > scan {
                        scans.remove(path);
                        state.busy.remove(&format!("scan:{path}"));
                    }
                }
                for ap in prop_paths(&objects, path, WIRELESS_IFACE, "AccessPoints") {
                    if !objects.get(&ap).is_some_and(|o| o.contains_key(AP_IFACE)) {
                        continue;
                    }
                    let ssid = prop_bytes(&objects, &ap, AP_IFACE, "Ssid");
                    let security = ap_security(prop_u32(&objects, &ap, AP_IFACE, "Flags"), prop_u32(&objects, &ap, AP_IFACE, "WpaFlags"),
                                               prop_u32(&objects, &ap, AP_IFACE, "RsnFlags"));
                    // Hidden beacons have no stable SSID to group safely.
                    let hex: String = ssid.iter().map(|b| format!("{b:02x}")).collect();
                    let key = format!("{path}|{}|{security}", if hex.is_empty() { ap.clone() } else { hex });
                    let mut saved: Vec<String> = Vec::new();
                    for (uuid, profile_path) in &profiles {
                        if profile_models.get(uuid).is_some_and(|p| p.device_ids.contains(path))
                            && known(matcher.ap_valid(&objects, path, &ap, profile_path, &settings[profile_path])) {
                            saved.push(uuid.clone());
                        }
                    }
                    saved.sort_by(|a, b| {
                        let (pa, pb) = (&profile_models.get(a).unwrap(), &profile_models.get(b).unwrap());
                        (std::cmp::Reverse(pa.timestamp), suite_core::pyjson::casefold(&pa.name))
                            .cmp(&(std::cmp::Reverse(pb.timestamp), suite_core::pyjson::casefold(&pb.name)))
                    });
                    let mut candidate = WifiNetwork::new(&key, path, &ssid, &ssid_label(&ssid), security,
                                                         prop_u32(&objects, &ap, AP_IFACE, "Strength") as i64, &ap);
                    candidate.profile_ids = saved;
                    candidate.active = current.as_deref() == Some(ap.as_str());
                    candidate.frequency = prop_u32(&objects, &ap, AP_IFACE, "Frequency");
                    candidate.bssid = prop_str(&objects, &ap, AP_IFACE, "HwAddress");
                    match groups.get_mut(&key) {
                        None => groups.insert(key, candidate),
                        Some(previous) if candidate.strength > previous.strength => {
                            candidate.active = candidate.active || previous.active;
                            candidate.profile_ids = union(&candidate.profile_ids, &previous.profile_ids);
                            *previous = candidate;
                        }
                        Some(previous) => {
                            previous.active = previous.active || candidate.active;
                            previous.profile_ids = union(&previous.profile_ids, &candidate.profile_ids);
                        }
                    }
                }
            }
            let description = match matcher.description(&objects, path) {
                Some(text) => text,
                None => {
                    misses.set(misses.get() + 1);
                    String::new()
                }
            };
            let name = if !description.is_empty() { description } else if !interface.is_empty() { interface.clone() } else { title(&kind) };
            let mut model = Device::new(path, &interface, &name, &kind);
            model.state = status;
            model.state_label = device_state(status).into();
            model.managed = prop_bool(&objects, path, DEVICE_IFACE, "Managed");
            model.active_ids = active_models.iter().filter(|(_, a)| a.device_ids.contains(path)).map(|(k, _)| k.clone()).collect();
            model.details = details;
            model.carrier = carrier;
            device_models.insert(path.clone(), model);
        }
        state.devices = device_models;
        state.profiles = profile_models;
        state.active = active_models;
        state.networks = groups;
        drop(state);
        drop(scans);
        drop(settings);
        drop(objects);
        // libnm's object cache lags ours: re-check shortly (bounded).
        let misses = misses.get();
        if misses > 0 && self.0.misses.get() < 40 {
            self.0.misses.set(self.0.misses.get() + 1);
            let b = self.clone();
            self.later("libnm-sync", 250, Box::new(move || b.refresh()));
        } else if misses == 0 {
            self.0.misses.set(0);
        }
        self.notify();
    }

    pub fn remote(&self, uuid: &str) -> Option<Remote> {
        let objects = self.0.objects.borrow();
        let settings = self.0.settings.borrow();
        for path in prop_paths(&objects, SETTINGS_PATH, SETTINGS_IFACE, "Connections") {
            if let Some(s) = settings.get(&path) {
                if get_str(s, "connection", "uuid").as_deref() == Some(uuid) {
                    return Some(Remote { path: path.clone(), uuid: uuid.into(), settings: s.clone(),
                                         flags: prop_u32(&objects, &path, CONNECTION_IFACE, "Flags"),
                                         version_id: prop_u64(&objects, &path, CONNECTION_IFACE, "VersionId") });
                }
            }
        }
        None
    }

    fn device_exists(&self, path: &str) -> bool {
        let objects = self.0.objects.borrow();
        prop_paths(&objects, NM_PATH, NM_IFACE, "Devices").iter().any(|p| p == path)
            && objects.get(path).is_some_and(|o| o.contains_key(DEVICE_IFACE))
    }

    fn ap_exists(&self, path: &str) -> bool {
        self.0.objects.borrow().get(path).is_some_and(|o| o.contains_key(AP_IFACE))
    }

    fn active_object(&self, path: &str) -> Option<(String, u32)> {
        let objects = self.0.objects.borrow();
        objects.get(path)?.get(ACTIVE_IFACE)?;
        Some((prop_str(&objects, path, ACTIVE_IFACE, "Uuid"), prop_u32(&objects, path, ACTIVE_IFACE, "StateFlags")))
    }

    fn last_scan(&self, path: &str) -> i64 {
        prop_i64(&self.0.objects.borrow(), path, WIRELESS_IFACE, "LastScan")
    }

    // --- actions ---

    fn can_act(&self, key: &str) -> bool {
        let unavailable = self.0.closed.get() || !self.state().ready || self.0.client.get() == 0;
        if unavailable {
            return self.fail("NetworkManager is unavailable.");
        }
        if self.state().busy.contains(key) {
            return self.fail("That network request is already in progress.");
        }
        true
    }

    fn run(&self, key: &str, call: Call, success: Option<Box<dyn FnOnce(&NetworkBackend, Variant)>>) -> bool {
        if !self.can_act(key) {
            return false;
        }
        let Some(bus) = self.bus() else { return self.fail("NetworkManager is unavailable.") };
        {
            let mut state = self.0.state.borrow_mut();
            state.busy.insert(key.to_owned());
            state.error.clear();
        }
        let (generation, client) = (self.0.generation.get(), self.0.client.get());
        self.notify();
        let weak = self.weak();
        let key = key.to_owned();
        // Do not pretend cancelling a callback can undo an action: closing
        // suppresses replies but never queues a second write.
        bus.call(call, Box::new(move |reply| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = NetworkBackend(inner);
            let current = !backend.0.closed.get() && generation == backend.0.generation.get() && client == backend.0.client.get();
            match reply {
                Err(error) => {
                    if current {
                        backend.0.state.borrow_mut().busy.remove(&key);
                        backend.fail_bus(&error);
                        backend.changed();
                    }
                }
                Ok(value) => {
                    if !current {
                        return;
                    }
                    match success {
                        Some(success) => success(&backend, value),
                        None => { backend.0.state.borrow_mut().busy.remove(&key); }
                    }
                    backend.changed();
                    backend.notify();
                }
            }
        }));
        true
    }

    pub fn set_networking(&self, enabled: bool) -> bool {
        let key = "radio:networking";
        if !self.can_act(key) {
            return false;
        }
        self.run(key, Call::new(NM_BUS, NM_PATH, NM_IFACE, "Enable", Some((enabled,).to_variant())).mutation(), None)
    }

    fn set_property(&self, key: &str, property: &str, enabled: bool) -> bool {
        self.run(key, Call::new(NM_BUS, NM_PATH, "org.freedesktop.DBus.Properties", "Set",
                                Some((NM_IFACE, property, enabled.to_variant()).to_variant())).mutation(), None)
    }

    pub fn set_wireless(&self, enabled: bool) -> bool {
        let key = "radio:wifi";
        if !self.can_act(key) {
            return false;
        }
        if enabled && !self.state().wireless_hardware_enabled {
            return self.fail("Wi-Fi is blocked by a hardware switch or airplane mode.");
        }
        self.set_property(key, "WirelessEnabled", enabled)
    }

    pub fn set_wwan(&self, enabled: bool) -> bool {
        let key = "radio:wwan";
        if !self.can_act(key) {
            return false;
        }
        if enabled && !self.state().wwan_hardware_enabled {
            return self.fail("Mobile broadband is blocked by a hardware switch or airplane mode.");
        }
        self.set_property(key, "WwanEnabled", enabled)
    }

    /// One adapter's runtime management and kernel link state:
    /// `Device.SetManaged(YES|NO, RUNTIME|SET_ADMIN_STATE)` — never the
    /// legacy `Managed` property, never shared radios or networking.
    pub fn set_device_enabled(&self, device_id: &str, enabled: bool) -> bool {
        if device_id.is_empty() {
            return self.fail("Choose a valid network adapter.");
        }
        let key = format!("device:{device_id}");
        if !self.can_act(&key) {
            return false;
        }
        let model = self.state().devices.get(device_id).cloned();
        let Some(model) = model.filter(|_| self.device_exists(device_id)) else {
            return self.fail("That network adapter is no longer available.");
        };
        if model.kind != "wired" && model.kind != "wifi" {
            return self.fail("Only LAN and Wi-Fi adapters can be enabled or disabled here.");
        }
        let value = if enabled { MANAGED_YES } else { MANAGED_NO };
        self.run(&key, Call::new(NM_BUS, device_id, DEVICE_IFACE, "SetManaged",
                                 Some((value, MANAGED_RUNTIME | MANAGED_SET_ADMIN_STATE).to_variant())).mutation(), None)
    }

    /// `(device path, ap path)` for a network, when both still exist.
    pub fn get_wifi_objects(&self, network_id: &str) -> (Option<String>, Option<String>) {
        let Some(network) = self.state().networks.get(network_id).cloned() else { return (None, None) };
        let device = self.device_exists(&network.device_id).then(|| network.device_id.clone());
        let ap = self.ap_exists(&network.ap_path).then(|| network.ap_path.clone());
        (device, ap)
    }

    /// Prepare dialog settings without connecting or requesting secrets.
    pub fn build_wifi_connection(&self, network_id: &str) -> Option<Connection> {
        let Some(network) = self.state().networks.get(network_id).cloned() else {
            self.fail("That wireless network is no longer available. Scan again.");
            return None;
        };
        let result = if network.security != "wpa-enterprise" {
            make_wifi_connection(&network, None)
        } else {
            // Enterprise credentials are entered in our own form.
            let mut open = network.clone();
            open.security = "open".into();
            make_wifi_connection(&open, None).and_then(|connection| {
                let security = Setting::new("802-11-wireless-security").ok_or("The connection settings are invalid.")?;
                security.set_str("key-mgmt", Some("wpa-eap"));
                connection.add_setting(&security);
                connection.add_setting(&Setting::new("802-1x").ok_or("The connection settings are invalid.")?);
                Ok(connection)
            })
        };
        match result {
            Ok(connection) => Some(connection),
            Err(message) => {
                self.fail(&message);
                None
            }
        }
    }

    pub fn scan(&self, device_id: Option<&str>) -> bool {
        let (ready, networking, wireless, hardware) = {
            let s = self.state();
            (s.ready, s.networking_enabled, s.wireless_enabled, s.wireless_hardware_enabled)
        };
        if !ready || !networking || !wireless {
            return self.fail("Enable networking and Wi-Fi before scanning.");
        }
        if !hardware {
            return self.fail("Wi-Fi is blocked by a hardware switch or airplane mode.");
        }
        let paths: Vec<String> = match device_id {
            Some(id) => vec![id.to_owned()],
            None => self.state().devices.values().filter(|d| d.kind == "wifi" && d.managed).map(|d| d.id.clone()).collect(),
        };
        if paths.is_empty() {
            return self.fail("No Wi-Fi adapter is available.");
        }
        let mut accepted = false;
        for path in paths {
            let model = self.state().devices.get(&path).cloned();
            if !self.device_exists(&path) || model.as_ref().is_none_or(|m| m.kind != "wifi" || !m.managed) {
                self.fail("That Wi-Fi adapter is no longer available.");
                continue;
            }
            let key = format!("scan:{path}");
            if self.state().busy.contains(&key) {
                continue;
            }
            let last = self.last_scan(&path);
            self.0.scans.borrow_mut().insert(path.clone(), last);
            let (scan_path, scan_key) = (path.clone(), key.clone());
            let requested: Box<dyn FnOnce(&NetworkBackend, Variant)> = Box::new(move |backend, _| {
                let b = backend.clone();
                let (path, key) = (scan_path.clone(), scan_key.clone());
                backend.later(&scan_key, 12000, Box::new(move || {
                    b.0.scans.borrow_mut().remove(&path);
                    b.0.state.borrow_mut().busy.remove(&key);
                    b.refresh();
                }));
            });
            let empty = glib::VariantDict::new(None).end();
            let queued = self.run(&key, Call::new(NM_BUS, &path, WIRELESS_IFACE, "RequestScan", Some(tuple(&[empty]))).mutation(),
                                  Some(requested));
            if !queued {
                self.0.scans.borrow_mut().remove(&path);
            }
            accepted = queued || accepted;
        }
        accepted
    }

    fn vpn_externally_owned(&self, profile_id: &str) -> bool {
        if let Some(remote) = self.remote(profile_id) {
            if remote.external() {
                return true;
            }
        }
        let objects = self.0.objects.borrow();
        prop_paths(&objects, NM_PATH, NM_IFACE, "ActiveConnections").iter().any(|path| {
            prop_str(&objects, path, ACTIVE_IFACE, "Uuid") == profile_id
                && prop_u32(&objects, path, ACTIVE_IFACE, "StateFlags") & ACTIVATION_FLAG_EXTERNAL != 0
        })
    }

    pub fn activate_profile(&self, profile_id: &str, device_id: Option<&str>) -> bool {
        let key = format!("activate:{profile_id}");
        if !self.can_act(&key) {
            return false;
        }
        let remote = self.remote(profile_id);
        let model = self.state().profiles.get(profile_id).cloned();
        let (Some(remote), Some(model)) = (remote, model) else {
            return self.fail("That saved connection no longer exists.");
        };
        if (model.kind == "vpn" || model.kind == "wireguard") && (model.external || self.vpn_externally_owned(profile_id)) {
            return self.fail("This VPN is managed externally. Use its managing application to connect it.");
        }
        let s = self.state().clone();
        if !s.networking_enabled {
            return self.fail("Enable networking before connecting.");
        }
        let device = match device_id {
            Some(id) => {
                if !self.device_exists(id) || (model.kind != "vpn" && !model.device_ids.iter().any(|d| d == id)) {
                    return self.fail("That device cannot use this connection.");
                }
                Some(id.to_owned())
            }
            None if model.device_ids.len() == 1 && model.kind != "vpn" => Some(model.device_ids[0].clone()),
            None => None,
        };
        if model.kind == "wifi" {
            if !s.wireless_enabled || !s.wireless_hardware_enabled {
                return self.fail("Enable Wi-Fi and unblock its hardware switch before connecting.");
            }
        } else if model.kind == "mobile" && (!s.wwan_enabled || !s.wwan_hardware_enabled) {
            return self.fail("Enable mobile broadband and unblock its hardware switch before connecting.");
        }
        if let Some(device) = &device {
            if !s.devices.get(device).is_some_and(|d| d.managed) {
                return self.fail("That device is not managed by NetworkManager.");
            }
        }
        let device = path_variant(device.as_deref());
        self.run(&key, Call::new(NM_BUS, NM_PATH, NM_IFACE, "ActivateConnection",
                                 Some(tuple(&[path_variant(Some(&remote.path)), device, path_variant(None)]))).mutation(), None)
    }

    /// Activate a complete connection from our Wi-Fi form (`AddAndActivate`).
    pub fn activate_connection(&self, connection: &Connection, device_id: Option<&str>, ap_path: Option<&str>) -> bool {
        let uuid = connection.uuid();
        let key = format!("activate:{}", if uuid.is_empty() { "new" } else { &uuid });
        if !self.can_act(&key) {
            return false;
        }
        let s = self.state().clone();
        if !s.networking_enabled {
            return self.fail("Enable networking before connecting.");
        }
        if let Some(id) = device_id.filter(|d| !d.is_empty()) {
            if !self.device_exists(id) {
                return self.fail("That network device is no longer available.");
            }
            if !s.devices.get(id).is_some_and(|d| d.managed) {
                return self.fail("That device is not managed by NetworkManager.");
            }
        }
        if connection.connection_type() == "802-11-wireless" && (!s.wireless_enabled || !s.wireless_hardware_enabled) {
            return self.fail("Enable Wi-Fi and unblock its hardware switch before connecting.");
        }
        if let Some(ap) = ap_path.filter(|a| !a.is_empty()) {
            if !self.ap_exists(ap) {
                return self.fail("That wireless access point is no longer available. Scan again.");
            }
        }
        if let Err(error) = connection.normalize().and_then(|_| connection.verify()) {
            return self.fail(error.message());
        }
        let device = path_variant(device_id.filter(|d| !d.is_empty()));
        let ap = path_variant(ap_path.filter(|a| !a.is_empty()));
        let settings = connection.to_dbus(libnm::ALL);
        self.run(&key, Call::new(NM_BUS, NM_PATH, NM_IFACE, "AddAndActivateConnection",
                                 Some(tuple(&[settings, device, ap]))).mutation(), None)
    }

    pub fn connect_wifi(&self, network_id: &str, password: Option<&str>) -> bool {
        let key = format!("wifi:{network_id}");
        if !self.can_act(&key) {
            return false;
        }
        let s = self.state().clone();
        let Some(network) = s.networks.get(network_id).cloned() else {
            return self.fail("That wireless network is no longer available. Scan again.");
        };
        if !s.networking_enabled || !s.wireless_enabled {
            return self.fail("Enable networking and Wi-Fi before connecting.");
        }
        if !s.wireless_hardware_enabled {
            return self.fail("Wi-Fi is blocked by a hardware switch or airplane mode.");
        }
        if !s.devices.get(&network.device_id).is_some_and(|d| d.managed) {
            return self.fail("That wireless adapter is not managed by NetworkManager.");
        }
        if let Some(profile) = network.profile_ids.first() {
            return self.activate_profile(profile, Some(&network.device_id));
        }
        if !self.device_exists(&network.device_id) || !self.ap_exists(&network.ap_path) {
            return self.fail("That wireless adapter or access point is no longer available.");
        }
        let connection = match make_wifi_connection(&network, password) {
            Ok(connection) => connection,
            Err(message) => return self.fail(&message),
        };
        let settings = connection.to_dbus(libnm::ALL);
        connection.clear_secrets();
        self.run(&key, Call::new(NM_BUS, NM_PATH, NM_IFACE, "AddAndActivateConnection",
                                 Some(tuple(&[settings, path_variant(Some(&network.device_id)), path_variant(Some(&network.ap_path))])))
                 .mutation(), None)
    }

    pub fn deactivate(&self, active_id: &str) -> bool {
        let Some((uuid, flags)) = self.active_object(active_id) else {
            return self.fail("That connection is no longer active.");
        };
        let model = self.state().active.get(active_id).cloned();
        if let Some(model) = model {
            if model.vpn || model.kind == "vpn" || model.kind == "wireguard" {
                let profile_external = self.state().profiles.get(&model.profile_id).is_some_and(|p| p.external)
                    || self.remote(&model.profile_id).is_some_and(|r| r.external());
                if model.external || flags & ACTIVATION_FLAG_EXTERNAL != 0 || profile_external {
                    return self.fail("This VPN is managed externally. Use its managing application to disconnect it.");
                }
            }
        }
        let _ = uuid;
        self.run(&format!("deactivate:{active_id}"), Call::new(NM_BUS, NM_PATH, NM_IFACE, "DeactivateConnection",
                 Some(tuple(&[path_variant(Some(active_id))]))).mutation(), None)
    }

    pub fn disconnect_device(&self, device_id: &str) -> bool {
        if !self.device_exists(device_id) {
            return self.fail("That network device is no longer available.");
        }
        self.run(&format!("disconnect:{device_id}"), Call::new(NM_BUS, device_id, DEVICE_IFACE, "Disconnect", None).mutation(), None)
    }

    pub fn delete_profile(&self, profile_id: &str) -> bool {
        let Some(remote) = self.remote(profile_id) else {
            return self.fail("That saved connection no longer exists.");
        };
        self.run(&format!("delete:{profile_id}"), Call::new(NM_BUS, &remote.path, CONNECTION_IFACE, "Delete", None).mutation(), None)
    }
}

/// A tuple of already-built variants (a Rust tuple of `Variant`s would box them as `v`).
pub fn tuple(items: &[Variant]) -> Variant {
    Variant::tuple_from_iter(items.iter().cloned())
}

/// `o` variant; `None` → `"/"`.
pub fn path_variant(path: Option<&str>) -> Variant {
    glib::variant::ObjectPath::try_from(path.unwrap_or("/")).unwrap_or_else(|_| glib::variant::ObjectPath::try_from("/").unwrap()).to_variant()
}

fn props(dict: &Variant) -> std::collections::BTreeMap<String, Variant> {
    let mut out = std::collections::BTreeMap::new();
    for entry in dict.iter() {
        if let (Some(key), Some(value)) = (entry.child_value(0).str().map(String::from), crate::nm::unbox(&entry.child_value(1))) {
            out.insert(key, value);
        }
    }
    out
}

#[cfg(test)]
#[path = "backend_tests.rs"]
pub(crate) mod tests;
