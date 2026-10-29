//! `connection_profiles.py`: the connection type catalogue, setting schema
//! introspection (libnm GObject properties), draft factories and the
//! non-activating `ProfileStore` (`AddConnection2` / `Update2`).
//! Existing secrets are never requested when an editor opens; a credential
//! change fetches preservation data (`GetSecrets`) only on an explicit save.

use crate::backend::{tuple, NetworkBackend, Remote};
use crate::libnm::{self, Connection, Setting, VpnPluginInfo};
use crate::nm::*;
use glib::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use suite_core::bus::{BusError, Call};

#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionType {
    pub id: String,
    pub name: String,
    pub category: String,
    pub setting: String,
    pub vpn_service: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PropertySpec {
    pub name: String,
    pub label: String,
    pub description: String,
    /// GObject value type name (`gboolean`, `gchararray`, `GStrv`, …).
    pub value_type: String,
    pub dbus_type: String,
    pub secret: bool,
    pub required: bool,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ProfileDraft {
    pub connection: Connection,
    pub original: Option<Remote>,
    pub version_id: u64,
}

impl ProfileDraft {
    pub fn new(connection: Connection) -> Self {
        Self { connection, original: None, version_id: 0 }
    }

    pub fn is_new(&self) -> bool {
        self.original.is_none()
    }
}

/// Connection base types (availability checked against the installed libnm).
pub const BASE_TYPES: &[(&str, &str, &str)] = &[
    ("802-3-ethernet", "Ethernet", "Hardware"), ("802-11-wireless", "Wi-Fi", "Hardware"),
    ("gsm", "Mobile broadband (GSM / LTE)", "Hardware"), ("cdma", "Mobile broadband (CDMA)", "Hardware"),
    ("bluetooth", "Bluetooth network", "Hardware"), ("infiniband", "InfiniBand", "Hardware"), ("adsl", "ADSL", "Hardware"),
    ("pppoe", "DSL / PPPoE", "Hardware"), ("wpan", "IEEE 802.15.4", "Hardware"), ("6lowpan", "6LoWPAN", "Hardware"),
    ("802-11-olpc-mesh", "OLPC mesh", "Hardware"), ("wifi-p2p", "Wi-Fi Direct", "Hardware"), ("wimax", "WiMAX", "Hardware"),
    ("vpn", "VPN", "VPN"), ("wireguard", "WireGuard", "VPN"),
    ("bond", "Bond", "Virtual"), ("bridge", "Bridge", "Virtual"), ("team", "Team", "Virtual"), ("vlan", "VLAN", "Virtual"),
    ("vrf", "VRF", "Virtual"), ("veth", "Virtual Ethernet pair", "Virtual"), ("dummy", "Dummy interface", "Virtual"),
    ("macvlan", "MACVLAN / MACVTAP", "Virtual"), ("ipvlan", "IPVLAN", "Virtual"), ("ip-tunnel", "IP tunnel", "Virtual"),
    ("tun", "TUN / TAP", "Virtual"), ("vxlan", "VXLAN", "Virtual"), ("geneve", "GENEVE", "Virtual"), ("macsec", "MACsec", "Virtual"),
    ("hsr", "HSR / PRP", "Virtual"), ("ovs-bridge", "Open vSwitch bridge", "Virtual"), ("ovs-port", "Open vSwitch port", "Virtual"),
    ("ovs-interface", "Open vSwitch interface", "Virtual"), ("loopback", "Loopback", "Virtual"), ("generic", "Generic device", "Virtual"),
];

pub fn connection_types() -> Vec<ConnectionType> {
    let mut result: Vec<ConnectionType> = BASE_TYPES.iter().filter(|(name, ..)| libnm::setting_exists(name))
        .map(|(name, title, category)| ConnectionType { id: (*name).into(), name: (*title).into(), category: (*category).into(),
                                                        setting: (*name).into(), vpn_service: String::new() })
        .collect();
    // Metadata only: never load another application's GUI/editor plugin.
    for plugin in VpnPluginInfo::list_load() {
        let service = plugin.service();
        if !service.is_empty() {
            let name = plugin.name();
            result.push(ConnectionType { id: format!("vpn:{service}"), name: if name.is_empty() { service.clone() } else { name },
                                         category: "VPN".into(), setting: "vpn".into(), vpn_service: service });
        }
    }
    result
}

fn label_for(name: &str) -> String {
    match name {
        "id" => "Connection name", "uuid" => "Connection ID", "ssid" => "Network name (SSID)", "interface-name" => "Interface",
        "mtu" => "MTU", "dns" => "DNS servers", "mac-address" => "Hardware address", "cloned-mac-address" => "Cloned hardware address",
        "psk" => "Password", "private-key" => "Private key", "autoconnect" => "Connect automatically",
        other => return capitalize(&other.replace('-', " ")),
    }.into()
}

fn bounds(spec: &glib::ParamSpec) -> (Option<f64>, Option<f64>) {
    macro_rules! try_bounds {
        ($($t:ty),*) => {$(
            if let Some(s) = spec.downcast_ref::<$t>() {
                return (Some(s.minimum() as f64), Some(s.maximum() as f64));
            }
        )*};
    }
    try_bounds!(glib::ParamSpecInt, glib::ParamSpecUInt, glib::ParamSpecInt64, glib::ParamSpecUInt64, glib::ParamSpecLong,
                glib::ParamSpecULong, glib::ParamSpecChar, glib::ParamSpecUChar, glib::ParamSpecDouble, glib::ParamSpecFloat);
    (None, None)
}

/// Writable properties of a setting, without inspecting values.
pub fn setting_schema(setting: &Setting) -> Vec<PropertySpec> {
    setting.properties().into_iter().filter(|p| p.flags().contains(glib::ParamFlags::WRITABLE)).map(|prop| {
        let flags = Setting::raw_flags(&prop);
        let (minimum, maximum) = bounds(&prop);
        PropertySpec {
            name: prop.name().to_owned(), label: label_for(prop.name()), description: prop.blurb().unwrap_or_default().to_owned(),
            value_type: prop.value_type().name().to_owned(), dbus_type: setting.dbus_type(prop.name()),
            secret: flags & libnm::PARAM_SECRET != 0, required: flags & libnm::PARAM_REQUIRED != 0, minimum, maximum,
        }
    }).collect()
}

pub fn new_draft(type_id: &str, name: Option<&str>) -> Result<ProfileDraft, String> {
    let selected = connection_types().into_iter().find(|t| t.id == type_id).ok_or("Choose a supported connection type.")?;
    let connection = Connection::new().map_err(|e| e.message().to_owned())?;
    let general = Setting::new("connection").ok_or("Choose a supported connection type.")?;
    general.set_str("id", Some(&name.map(String::from).unwrap_or_else(|| format!("New {}", selected.name))));
    general.set_str("uuid", Some(&libnm::uuid_generate()));
    general.set_str("type", Some(&selected.setting));
    general.set_bool("autoconnect", true);
    connection.add_setting(&general);
    let primary = Setting::new(&selected.setting).ok_or("Choose a supported connection type.")?;
    connection.add_setting(&primary);
    match selected.setting.as_str() {
        "802-11-wireless" => primary.set_str("mode", Some("infrastructure")),
        "bluetooth" => primary.set_str("type", Some("panu")),
        "infiniband" => primary.set_str("transport-mode", Some("datagram")),
        "adsl" => {
            primary.set_str("protocol", Some("pppoe"));
            primary.set_str("encapsulation", Some("llc"));
        }
        "tun" => primary.set_int("mode", 1),
        "ip-tunnel" => primary.set_int("mode", 1),
        "macvlan" => primary.set_int("mode", 2),
        "vlan" => primary.set_int("id", 1),
        "ovs-interface" => primary.set_str("type", Some("internal")),
        _ => {}
    }
    if !selected.vpn_service.is_empty() {
        primary.set_str("service-type", Some(&selected.vpn_service));
    }
    if ["pppoe", "adsl", "gsm", "cdma"].contains(&selected.setting.as_str()) {
        connection.add_setting(&Setting::new("ppp").ok_or("Choose a supported connection type.")?);
    }
    if selected.setting == "pppoe" {
        connection.add_setting(&Setting::new("802-3-ethernet").ok_or("Choose a supported connection type.")?);
    }
    if selected.setting == "ovs-port" {
        general.set_str("port-type", Some("ovs-bridge"));
    } else if selected.setting == "ovs-interface" {
        general.set_str("port-type", Some("ovs-port"));
    }
    if selected.setting != "ovs-bridge" && selected.setting != "ovs-port" {
        let ipv4 = Setting::new("ipv4").unwrap();
        let ipv6 = Setting::new("ipv6").unwrap();
        ipv4.set_str("method", Some("auto"));
        ipv6.set_str("method", Some("auto"));
        if ["wireguard", "tun", "ip-tunnel", "6lowpan", "wpan"].contains(&selected.setting.as_str()) {
            ipv4.set_str("method", Some("disabled"));
        }
        if ["wireguard", "tun", "ip-tunnel", "wpan"].contains(&selected.setting.as_str()) {
            ipv6.set_str("method", Some("disabled"));
        }
        connection.add_setting(&ipv4);
        connection.add_setting(&ipv6);
    }
    Ok(ProfileDraft::new(connection))
}

pub fn edit_draft(remote: &Remote) -> Result<ProfileDraft, String> {
    let clone = remote.to_connection().map_err(|_| "That connection is unavailable.".to_owned())?;
    // A remote object normally has no secrets; clear any so an unchanged
    // masked entry cannot become a secret update.
    clone.clear_secrets();
    Ok(ProfileDraft { connection: clone, original: Some(remote.clone()), version_id: remote.version_id })
}

/// libnm emits empty setting maps (and VPN's empty `secrets`) even without secrets.
pub fn has_secrets(connection: &Connection) -> bool {
    let value = connection.to_dbus(libnm::WITH_SECRETS);
    value.iter().any(|setting| {
        setting.child_value(1).iter().any(|entry| {
            let v = crate::nm::unbox(&entry.child_value(1)).unwrap_or_else(|| entry.child_value(1));
            match v.classify() {
                glib::VariantClass::String => !v.str().unwrap_or_default().is_empty(),
                glib::VariantClass::Array => v.n_children() > 0,
                _ => true,
            }
        })
    })
}

pub fn secret_settings(connection: &Connection) -> Vec<String> {
    connection.settings().iter().filter(|s| setting_schema(s).iter().any(|p| p.secret)).map(|s| s.name()).collect()
}

fn merge_secrets(connection: &Connection, secrets: &Variant) -> Result<(), glib::Error> {
    for entry in secrets.iter() {
        let name = entry.child_value(0).str().unwrap_or_default().to_owned();
        if entry.child_value(1).n_children() > 0 && connection.setting(&name).is_some() {
            connection.update_secrets(&name, secrets)?;
        }
    }
    Ok(())
}

pub struct StoreInner {
    backend: NetworkBackend,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    pub busy: Cell<bool>,
    pub error: RefCell<String>,
    closed: Cell<bool>,
    serial: Cell<u64>,
    weak: RefCell<Weak<StoreInner>>,
}

/// Save one editor's draft without activating or reapplying connections.
#[derive(Clone)]
pub struct ProfileStore(pub Rc<StoreInner>);

pub type Saved = Rc<dyn Fn(&Remote)>;

struct Save {
    draft: Rc<RefCell<ProfileDraft>>,
    prepared: Connection,
    original: Option<Remote>,
    serial: u64,
    generation: u64,
    client: u64,
    on_saved: Saved,
}

impl ProfileStore {
    pub fn new(backend: &NetworkBackend, on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) -> Self {
        let inner = Rc::new(StoreInner { backend: backend.clone(), on_changed: Box::new(on_changed), on_error: Box::new(on_error),
                                         busy: Cell::new(false), error: RefCell::default(), closed: Cell::new(false),
                                         serial: Cell::new(0), weak: RefCell::new(Weak::new()) });
        inner.weak.replace(Rc::downgrade(&inner));
        Self(inner)
    }

    pub fn busy(&self) -> bool {
        self.0.busy.get()
    }

    pub fn error(&self) -> String {
        self.0.error.borrow().clone()
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.serial.set(self.0.serial.get() + 1);
        self.0.busy.set(false);
    }

    fn fail(&self, message: &str) -> bool {
        self.0.error.replace(message.to_owned());
        self.0.busy.set(false);
        if !self.0.closed.get() {
            (self.0.on_error)(message);
            (self.0.on_changed)();
        }
        false
    }

    fn fail_bus(&self, error: &BusError) -> bool {
        self.fail(&error.message.clone())
    }

    pub fn save(&self, draft: &Rc<RefCell<ProfileDraft>>, on_saved: Saved) -> bool {
        if self.0.closed.get() || self.0.busy.get() {
            return false;
        }
        let backend = &self.0.backend;
        if !backend.ready() || backend.client() == 0 {
            return self.fail("NetworkManager is unavailable.");
        }
        let (connection, original, version_id) = {
            let d = draft.borrow();
            (d.connection.clone(), d.original.clone(), d.version_id)
        };
        let prepared = connection.duplicate();
        let checked: Result<(), String> = (|| {
            prepared.normalize().map_err(|e| e.message().to_owned())?;
            prepared.verify().map_err(|e| e.message().to_owned())?;
            prepared.verify_secrets().map_err(|e| e.message().to_owned())?;
            if let Some(original) = &original {
                if prepared.uuid() != original.uuid {
                    return Err("The connection ID cannot be changed while editing a saved connection.".into());
                }
                let current = backend.remote(&original.uuid);
                if current.as_ref().map(|c| c.path.as_str()) != Some(original.path.as_str()) {
                    return Err("That saved connection is no longer available. Reopen its settings.".into());
                }
                let current = current.unwrap();
                if original.external() || current.external() {
                    return Err("This connection is managed externally. Edit it in its managing application.".into());
                }
                if version_id != 0 && current.version_id != version_id {
                    return Err("This connection changed while it was being edited. Reopen its settings.".into());
                }
            }
            Ok(())
        })();
        if let Err(message) = checked {
            return self.fail(&message);
        }
        self.0.busy.set(true);
        self.0.error.replace(String::new());
        self.0.serial.set(self.0.serial.get() + 1);
        let save = Rc::new(Save { draft: draft.clone(), prepared, original, serial: self.0.serial.get(), generation: backend.generation(),
                                  client: backend.client(), on_saved });
        (self.0.on_changed)();
        if save.original.is_none() || !has_secrets(&save.prepared) {
            self.persist(&save);
            return self.0.busy.get();
        }
        // Update2 keeps daemon/agent secrets when none are submitted; once any
        // secret is supplied, every preserved secret must be merged first.
        let edited = save.prepared.to_dbus(libnm::WITH_SECRETS);
        let pending: Vec<String> = save.original.as_ref().and_then(|o| o.to_connection().ok())
            .map(|o| secret_settings(&o)).unwrap_or_default().into_iter()
            .filter(|name| save.prepared.setting(name).is_some()).collect();
        self.read_next(save, Rc::new(edited), Rc::new(RefCell::new(pending.into())));
        self.0.busy.get()
    }

    fn alive(&self, save: &Save) -> bool {
        if self.0.closed.get() || save.serial != self.0.serial.get() {
            return false;
        }
        let backend = &self.0.backend;
        if save.client != backend.client() || !backend.ready() || save.generation != backend.generation() {
            self.0.serial.set(self.0.serial.get() + 1);
            self.fail("NetworkManager changed while saving. Reopen the connection settings.");
            return false;
        }
        true
    }

    fn read_next(&self, save: Rc<Save>, edited: Rc<Variant>, pending: Rc<RefCell<std::collections::VecDeque<String>>>) {
        if !self.alive(&save) {
            save.prepared.clear_secrets();
            return;
        }
        let Some(name) = pending.borrow_mut().pop_front() else {
            if let Err(error) = merge_secrets(&save.prepared, &edited) {
                save.prepared.clear_secrets();
                self.fail(error.message());
                return;
            }
            return self.persist(&save);
        };
        let Some(bus) = self.0.backend.bus() else {
            self.fail("NetworkManager is unavailable.");
            return;
        };
        let path = save.original.as_ref().unwrap().path.clone();
        let weak = self.0.weak.borrow().clone();
        bus.call(Call::new(NM_BUS, &path, CONNECTION_IFACE, "GetSecrets", Some((name.as_str(),).to_variant())), Box::new(move |reply| {
            let Some(inner) = weak.upgrade() else { return };
            let store = ProfileStore(inner);
            match reply {
                Err(error) => {
                    save.prepared.clear_secrets();
                    if store.alive(&save) {
                        store.fail(&format!("Unable to preserve existing credentials: {}", error.message));
                    }
                }
                Ok(value) => {
                    if !store.alive(&save) {
                        save.prepared.clear_secrets();
                        return;
                    }
                    let secrets = value.child_value(0);
                    if let Err(error) = save.prepared.update_secrets(&name, &secrets) {
                        save.prepared.clear_secrets();
                        store.fail(&format!("Unable to preserve existing credentials: {}", error.message()));
                        return;
                    }
                    store.read_next(save, edited, pending);
                }
            }
        }));
    }

    fn persist(&self, save: &Rc<Save>) {
        if !self.alive(save) {
            save.prepared.clear_secrets();
            return;
        }
        let backend = self.0.backend.clone();
        let Some(bus) = backend.bus() else {
            self.fail("NetworkManager is unavailable.");
            return;
        };
        if let Err(error) = save.prepared.verify_secrets() {
            save.prepared.clear_secrets();
            self.fail(error.message());
            return;
        }
        let payload = save.prepared.to_dbus(libnm::ALL);
        let call = match &save.original {
            None => Call::new(NM_BUS, SETTINGS_PATH, SETTINGS_IFACE, "AddConnection2",
                              Some(tuple(&[payload.clone(), (ADD2_TO_DISK | ADD2_BLOCK_AUTOCONNECT).to_variant(), glib::VariantDict::new(None).end()]))),
            Some(original) => {
                let current = backend.remote(&original.uuid);
                if current.as_ref().map(|c| c.path.as_str()) != Some(original.path.as_str()) {
                    save.prepared.clear_secrets();
                    self.fail("That saved connection is no longer available.");
                    return;
                }
                if current.unwrap().external() {
                    save.prepared.clear_secrets();
                    self.fail("This connection is now managed externally. Reopen its settings.");
                    return;
                }
                let args = glib::VariantDict::new(None);
                let version = save.draft.borrow().version_id;
                if version != 0 {
                    args.insert_value("version-id", &version.to_variant());
                }
                Call::new(NM_BUS, &original.path, CONNECTION_IFACE, "Update2",
                          Some(tuple(&[payload.clone(), (UPDATE2_TO_DISK | UPDATE2_NO_REAPPLY).to_variant(), args.end()])))
            }
        };
        let mut call = call.mutation();
        call.interactive = true;
        let weak = self.0.weak.borrow().clone();
        let save = save.clone();
        bus.call(call, Box::new(move |reply| {
            let Some(inner) = weak.upgrade() else { return };
            let store = ProfileStore(inner);
            save.prepared.clear_secrets();
            let remote = match reply {
                Err(error) => {
                    if store.alive(&save) {
                        store.fail_bus(&error);
                    }
                    return;
                }
                Ok(value) => match &save.original {
                    Some(original) => original.clone(),
                    None => {
                        let path = value.child_value(0).str().unwrap_or_default().to_owned();
                        let settings = parse_settings(&save.prepared.to_dbus(libnm::WITH_NON_SECRET));
                        Remote { path, uuid: save.prepared.uuid(), settings, flags: 0, version_id: 0 }
                    }
                },
            };
            if !store.alive(&save) {
                return;
            }
            store.0.busy.set(false);
            {
                let mut draft = save.draft.borrow_mut();
                draft.version_id = store.0.backend.remote(&remote.uuid).map(|r| r.version_id).unwrap_or(remote.version_id);
                draft.original = Some(remote.clone());
                draft.connection.clear_secrets();
            }
            store.0.backend.refresh();
            (store.0.on_changed)();
            (save.on_saved)(&remote);
        }));
    }
}

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod tests;
