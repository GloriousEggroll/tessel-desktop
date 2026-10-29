//! `KeyringStore` of secret_agent.py over the raw Secret Service D-Bus API
//! (`org.freedesktop.secrets`, the libsecret/nm-applet schema
//! `org.freedesktop.NetworkManager.Connection` with DONT_MATCH_NAME, so old
//! saved passwords keep working). "plain" session; items in the default
//! collection labelled `Network password for {id}`.

use crate::libnm::{self, Connection, Setting};
use glib::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::rc::Rc;
use suite_core::bus::{Bus, BusError, Call};

pub const SCHEMA: &str = "org.freedesktop.NetworkManager.Connection";
const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const PROMPT_IFACE: &str = "org.freedesktop.Secret.Prompt";

pub const READ_FAILED: &str = "Saved network passwords could not be read from the keyring.";
pub const SAVE_FAILED: &str = "The network password could not be saved to the keyring.";
pub const UPDATE_FAILED: &str = "Saved network passwords could not be updated in the keyring.";
pub const DELETE_FAILED: &str = "Saved network passwords could not be removed from the keyring.";

pub type Values = Vec<(String, String)>;

/// The keyring operations the agent needs (fakes in tests).
pub trait Keyring {
    fn lookup(&self, uuid: &str, setting: &str, interactive: bool, done: Box<dyn FnOnce(Values, Option<String>)>);
    /// Store storable secrets; `setting_names` limits which settings are replaced.
    fn save(&self, connection: &Connection, setting_names: Option<Vec<String>>, done: Box<dyn FnOnce(Option<String>)>);
    fn delete(&self, uuid: &str, done: Box<dyn FnOnce(Option<String>)>);
}

/// Read flags (`secret_flags` of secret_agent.py).
pub fn secret_flags(setting: &Setting, key: &str) -> u32 {
    let name = setting.name();
    if name == "vpn" {
        return match setting.vpn_data_item(&format!("{key}-flags")) {
            None => 0,
            Some(text) if text.is_empty() => 0,
            Some(text) => text.trim().parse::<u32>().unwrap_or(libnm::NOT_SAVED),
        };
    }
    if name == "wireguard" && key.starts_with("peers.") {
        let public = key.strip_prefix("peers.").and_then(|k| k.strip_suffix(".preshared-key")).unwrap_or("");
        return setting.peers().iter().find(|p| p.public_key() == public).map(|p| p.preshared_key_flags()).unwrap_or(libnm::NOT_SAVED);
    }
    let property = if ["wep-key0", "wep-key1", "wep-key2", "wep-key3"].contains(&key) { "wep-key-flags".to_owned() } else { format!("{key}-flags") };
    if setting.find_property(&property).is_some() { setting.get_u32(&property) } else { 0 }
}

pub fn set_secret_flags(setting: &Setting, key: &str, flags: u32) -> Result<(), String> {
    if setting.name() == "wireguard" && key.starts_with("peers.") {
        let public = key.strip_prefix("peers.").and_then(|k| k.strip_suffix(".preshared-key")).unwrap_or("");
        for (index, current) in setting.peers().iter().enumerate() {
            if current.public_key() == public {
                let peer = current.duplicate(true);
                peer.set_preshared_key_flags(flags);
                setting.set_peer(&peer, index as u32);
                return Ok(());
            }
        }
        return Err("The requested WireGuard peer is no longer available.".into());
    }
    setting.set_secret_flags(key, flags).map_err(|e| e.message().to_owned())
}

/// Interoperable credential keys `(setting, key, value)`, incl. WireGuard peers.
pub fn storable_secrets(connection: &Connection) -> Vec<(String, String, String)> {
    let dict = connection.to_dbus(libnm::WITH_SECRETS);
    let mut result = Vec::new();
    for setting in connection.settings() {
        let name = setting.name();
        for (key, value) in libnm::flat_secrets(&dict, &name) {
            let flags = secret_flags(&setting, &key);
            if !value.is_empty() && flags & libnm::AGENT_OWNED != 0 && flags & libnm::NOT_SAVED == 0 {
                result.push((name.clone(), key, value));
            }
        }
    }
    result
}

/// `variant_secrets(setting_name, values)` → `a{sa{sv}}`.
pub fn variant_secrets(setting_name: &str, values: &[(String, String)]) -> Variant {
    let payload = glib::VariantDict::new(None);
    if setting_name == "vpn" {
        let map: std::collections::HashMap<String, String> = values.iter().cloned().collect();
        payload.insert_value("secrets", &map.to_variant());
    } else {
        let mut peers = Vec::new();
        for (key, value) in values {
            if setting_name == "wireguard" && key.starts_with("peers.") && key.ends_with(".preshared-key") && key.len() >= 20 {
                let peer = glib::VariantDict::new(None);
                peer.insert_value("public-key", &key[6..key.len() - 14].to_variant());
                peer.insert_value("preshared-key", &value.to_variant());
                peers.push(peer.end());
            } else if key == "password-raw" {
                payload.insert_value(key, &value.as_bytes().to_vec().to_variant());
            } else {
                payload.insert_value(key, &value.to_variant());
            }
        }
        if !peers.is_empty() {
            payload.insert_value("peers", &Variant::array_from_iter_with_type(glib::VariantTy::VARDICT, peers));
        }
    }
    let entry = Variant::from_dict_entry(&setting_name.to_variant(), &payload.end());
    Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(), [entry])
}

fn attributes(pairs: &[(&str, &str)]) -> Variant {
    let map: std::collections::HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    map.to_variant()
}

fn tuple(items: &[Variant]) -> Variant {
    Variant::tuple_from_iter(items.iter().cloned())
}

fn paths(value: &Variant) -> Vec<String> {
    value.iter().filter_map(|v| v.str().map(String::from)).collect()
}

fn obj(path: &str) -> Variant {
    glib::variant::ObjectPath::try_from(path).map(|p| p.to_variant()).unwrap_or_else(|_| glib::variant::ObjectPath::try_from("/").unwrap().to_variant())
}

/// Secret Service client over a session [`Bus`].
pub struct SecretService {
    bus: RefCell<Option<Rc<dyn Bus>>>,
    session: Rc<RefCell<Option<String>>>,
    connector: Box<dyn Fn(Box<dyn FnOnce(Option<Rc<dyn Bus>>)>)>,
}

type Done<T> = Box<dyn FnOnce(Result<T, BusError>)>;

impl SecretService {
    pub fn new() -> Rc<Self> {
        Self::with_connector(Box::new(|done| {
            gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |result| {
                done(result.ok().map(|c| suite_core::bus::GioBus::new(c) as Rc<dyn Bus>))
            })
        }))
    }

    pub fn with_bus(bus: Rc<dyn Bus>) -> Rc<Self> {
        let service = Self::with_connector(Box::new(|done| done(None)));
        service.bus.replace(Some(bus));
        service
    }

    fn with_connector(connector: Box<dyn Fn(Box<dyn FnOnce(Option<Rc<dyn Bus>>)>)>) -> Rc<Self> {
        Rc::new(Self { bus: RefCell::new(None), session: Rc::default(), connector })
    }

    fn with_bus_ready(self: &Rc<Self>, then: Box<dyn FnOnce(Option<Rc<dyn Bus>>)>) {
        if let Some(bus) = self.bus.borrow().clone() {
            then(Some(bus));
            return;
        }
        let this = self.clone();
        (self.connector)(Box::new(move |bus| {
            if let Some(bus) = &bus {
                this.bus.replace(Some(bus.clone()));
            }
            then(bus);
        }));
    }

    fn call(bus: &Rc<dyn Bus>, path: &str, iface: &str, method: &str, params: Option<Variant>, done: Done<Variant>) {
        let mut call = Call::new(SERVICE, path, iface, method, params);
        if matches!(method, "CreateItem" | "Delete" | "Unlock" | "Prompt") {
            call = call.mutation();
            call.interactive = false;
        }
        bus.call(call, Box::new(move |reply| done(reply)));
    }

    fn open_session(self: &Rc<Self>, bus: &Rc<dyn Bus>, done: Done<String>) {
        if let Some(session) = self.session.borrow().clone() {
            done(Ok(session));
            return;
        }
        let session = self.session.clone();
        Self::call(bus, SERVICE_PATH, SERVICE_IFACE, "OpenSession", Some(tuple(&["plain".to_variant(), "".to_variant().to_variant()])),
            Box::new(move |reply| match reply {
                Ok(value) => {
                    let path = value.try_child_value(1).and_then(|v| v.str().map(String::from)).unwrap_or_default();
                    session.replace(Some(path.clone()));
                    done(Ok(path));
                }
                Err(e) => done(Err(e)),
            }));
    }

    /// Run a prompt (`Prompt("")` → `Completed(b dismissed, v result)`).
    fn prompt(bus: &Rc<dyn Bus>, prompt: &str, done: Done<Option<Variant>>) {
        if prompt == "/" || prompt.is_empty() {
            done(Ok(None));
            return;
        }
        let done = Rc::new(RefCell::new(Some(done)));
        let id = Rc::new(std::cell::Cell::new(0u32));
        let (d, bus2, id2) = (done.clone(), bus.clone(), id.clone());
        let subscription = bus.subscribe(Some(SERVICE), Some(PROMPT_IFACE), Some("Completed"), Some(prompt), None, Rc::new(move |signal| {
            bus2.unsubscribe(id2.get());
            if let Some(done) = d.borrow_mut().take() {
                let dismissed = signal.params.try_child_value(0).and_then(|v| v.get::<bool>()).unwrap_or(true);
                if dismissed {
                    done(Err(BusError::local("The keyring prompt was dismissed.")));
                } else {
                    done(Ok(signal.params.try_child_value(1).and_then(|v| crate::nm::unbox(&v))));
                }
            }
        }));
        id.set(subscription);
        let bus3 = bus.clone();
        Self::call(bus, prompt, PROMPT_IFACE, "Prompt", Some(("",).to_variant()), Box::new(move |reply| {
            if let Err(error) = reply {
                bus3.unsubscribe(id.get());
                if let Some(done) = done.borrow_mut().take() {
                    done(Err(error));
                }
            }
        }));
    }

    fn search(bus: &Rc<dyn Bus>, attrs: Variant, done: Done<(Vec<String>, Vec<String>)>) {
        Self::call(bus, SERVICE_PATH, SERVICE_IFACE, "SearchItems", Some(tuple(&[attrs])), Box::new(move |reply| {
            done(reply.map(|v| (paths(&v.child_value(0)), paths(&v.child_value(1)))))
        }));
    }

    fn unlock(bus: &Rc<dyn Bus>, objects: Vec<String>, done: Done<Vec<String>>) {
        if objects.is_empty() {
            done(Ok(Vec::new()));
            return;
        }
        let items: Vec<Variant> = objects.iter().map(|p| obj(p)).collect();
        let array = Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, items);
        let bus2 = bus.clone();
        Self::call(bus, SERVICE_PATH, SERVICE_IFACE, "Unlock", Some(tuple(&[array])), Box::new(move |reply| match reply {
            Err(e) => done(Err(e)),
            Ok(value) => {
                let mut unlocked = paths(&value.child_value(0));
                let prompt = value.child_value(1).str().unwrap_or("/").to_owned();
                Self::prompt(&bus2, &prompt, Box::new(move |result| match result {
                    Ok(Some(more)) => {
                        unlocked.extend(paths(&more));
                        done(Ok(unlocked))
                    }
                    Ok(None) => done(Ok(unlocked)),
                    Err(e) => done(Err(e)),
                }));
            }
        }));
    }

    fn lookup_items(self: &Rc<Self>, bus: Rc<dyn Bus>, items: Vec<String>, done: Box<dyn FnOnce(Values, Option<String>)>) {
        if items.is_empty() {
            done(Vec::new(), None);
            return;
        }
        let this = self.clone();
        let bus2 = bus.clone();
        self.open_session(&bus, Box::new(move |session| {
            let Ok(session) = session else { return done(Vec::new(), Some(READ_FAILED.into())) };
            let array = Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, items.iter().map(|p| obj(p)));
            let bus3 = bus2.clone();
            Self::call(&bus2, SERVICE_PATH, SERVICE_IFACE, "GetSecrets", Some(tuple(&[array, obj(&session)])), Box::new(move |reply| {
                let Ok(reply) = reply else { return done(Vec::new(), Some(READ_FAILED.into())) };
                let secrets: Vec<(String, Vec<u8>)> = reply.child_value(0).iter().filter_map(|entry| {
                    let path = entry.child_value(0).str()?.to_owned();
                    let secret = entry.child_value(1);
                    Some((path, secret.try_child_value(2)?.fixed_array::<u8>().ok()?.to_vec()))
                }).collect();
                let remaining = Rc::new(std::cell::Cell::new(secrets.len()));
                let values: Rc<RefCell<Values>> = Rc::default();
                let failure: Rc<RefCell<Option<String>>> = Rc::default();
                let done = Rc::new(RefCell::new(Some(done)));
                if secrets.is_empty() {
                    if let Some(done) = done.borrow_mut().take() {
                        done(Vec::new(), None);
                    }
                    return;
                }
                for (path, secret) in secrets {
                    let (remaining, values, failure, done) = (remaining.clone(), values.clone(), failure.clone(), done.clone());
                    let _ = &this;
                    Self::call(&bus3, &path, "org.freedesktop.DBus.Properties", "Get", Some((ITEM_IFACE, "Attributes").to_variant()),
                        Box::new(move |reply| {
                            match reply {
                                Ok(value) => {
                                    let attrs = crate::nm::unbox(&value.child_value(0)).unwrap_or_else(|| value.child_value(0));
                                    let key = attrs.iter().find(|e| e.child_value(0).str() == Some("setting-key"))
                                        .and_then(|e| e.child_value(1).str().map(String::from));
                                    match (key, String::from_utf8(secret)) {
                                        (Some(key), Ok(text)) if !key.is_empty() => values.borrow_mut().push((key, text)),
                                        _ => {}
                                    }
                                }
                                Err(_) => { failure.replace(Some(READ_FAILED.into())); }
                            }
                            remaining.set(remaining.get() - 1);
                            if remaining.get() == 0 {
                                if let Some(done) = done.borrow_mut().take() {
                                    done(values.take(), failure.take());
                                }
                            }
                        }));
                }
            }));
        }));
    }

    fn default_collection(bus: &Rc<dyn Bus>, done: Done<String>) {
        let bus2 = bus.clone();
        Self::call(bus, SERVICE_PATH, SERVICE_IFACE, "ReadAlias", Some(("default",).to_variant()), Box::new(move |reply| match reply {
            Err(e) => done(Err(e)),
            Ok(value) => {
                let path = value.child_value(0).str().unwrap_or("/").to_owned();
                if path == "/" {
                    return done(Err(BusError::local("No default keyring collection.")));
                }
                let bus3 = bus2.clone();
                let collection = path.clone();
                Self::call(&bus2, &path, "org.freedesktop.DBus.Properties", "Get", Some((COLLECTION_IFACE, "Locked").to_variant()),
                    Box::new(move |reply| {
                        let locked = reply.ok().and_then(|v| crate::nm::unbox(&v.child_value(0))).and_then(|v| v.get::<bool>()).unwrap_or(false);
                        if !locked {
                            return done(Ok(collection));
                        }
                        Self::unlock(&bus3, vec![collection.clone()], Box::new(move |result| match result {
                            Ok(unlocked) if unlocked.contains(&collection) => done(Ok(collection)),
                            Ok(_) => done(Err(BusError::local("The keyring is locked."))),
                            Err(e) => done(Err(e)),
                        }));
                    }));
            }
        }));
    }

    fn clear(bus: &Rc<dyn Bus>, attrs: Variant, done: Box<dyn FnOnce(Result<(), BusError>)>) {
        let bus2 = bus.clone();
        Self::search(bus, attrs, Box::new(move |result| match result {
            Err(e) => done(Err(e)),
            Ok((unlocked, locked)) => {
                let items: Vec<String> = unlocked.into_iter().chain(locked).collect();
                if items.is_empty() {
                    return done(Ok(()));
                }
                let remaining = Rc::new(std::cell::Cell::new(items.len()));
                let failure: Rc<RefCell<Option<BusError>>> = Rc::default();
                let done = Rc::new(RefCell::new(Some(done)));
                for item in items {
                    let (remaining, failure, done, bus3) = (remaining.clone(), failure.clone(), done.clone(), bus2.clone());
                    Self::call(&bus2, &item, ITEM_IFACE, "Delete", None, Box::new(move |reply| {
                        let finish = move |error: Option<BusError>| {
                            if let Some(error) = error {
                                failure.replace(Some(error));
                            }
                            remaining.set(remaining.get() - 1);
                            if remaining.get() == 0 {
                                if let Some(done) = done.borrow_mut().take() {
                                    match failure.take() {
                                        Some(e) => done(Err(e)),
                                        None => done(Ok(())),
                                    }
                                }
                            }
                        };
                        match reply {
                            Err(e) => finish(Some(e)),
                            Ok(value) => {
                                let prompt = value.child_value(0).str().unwrap_or("/").to_owned();
                                Self::prompt(&bus3, &prompt, Box::new(move |r| finish(r.err())));
                            }
                        }
                    }));
                }
            }
        }));
    }

    fn store(self: &Rc<Self>, bus: Rc<dyn Bus>, uuid: String, label: String, entries: Vec<(String, String, String)>,
             done: Box<dyn FnOnce(Option<String>)>) {
        if entries.is_empty() {
            done(None);
            return;
        }
        let bus2 = bus.clone();
        let this = self.clone();
        Self::default_collection(&bus, Box::new(move |collection| {
            let Ok(collection) = collection else { return done(Some(SAVE_FAILED.into())) };
            let bus3 = bus2.clone();
            this.open_session(&bus2, Box::new(move |session| {
                let Ok(session) = session else { return done(Some(SAVE_FAILED.into())) };
                let remaining = Rc::new(std::cell::Cell::new(entries.len()));
                let failure: Rc<RefCell<Option<String>>> = Rc::default();
                let done = Rc::new(RefCell::new(Some(done)));
                for (setting, key, value) in entries {
                    let properties = glib::VariantDict::new(None);
                    properties.insert_value("org.freedesktop.Secret.Item.Label", &label.to_variant());
                    properties.insert_value("org.freedesktop.Secret.Item.Attributes", &attributes(&[
                        ("connection-uuid", &uuid), ("setting-name", &setting), ("setting-key", &key), ("xdg:schema", SCHEMA)]));
                    let secret = tuple(&[obj(&session), Vec::<u8>::new().to_variant(), value.as_bytes().to_vec().to_variant(),
                                         "text/plain".to_variant()]);
                    let (remaining, failure, done, bus4) = (remaining.clone(), failure.clone(), done.clone(), bus3.clone());
                    Self::call(&bus3, &collection, COLLECTION_IFACE, "CreateItem", Some(tuple(&[properties.end(), secret, true.to_variant()])),
                        Box::new(move |reply| {
                            let finish = move |ok: bool| {
                                if !ok {
                                    failure.replace(Some(SAVE_FAILED.into()));
                                }
                                remaining.set(remaining.get() - 1);
                                if remaining.get() == 0 {
                                    if let Some(done) = done.borrow_mut().take() {
                                        done(failure.take());
                                    }
                                }
                            };
                            match reply {
                                Err(_) => finish(false),
                                Ok(value) => {
                                    let prompt = value.child_value(1).str().unwrap_or("/").to_owned();
                                    Self::prompt(&bus4, &prompt, Box::new(move |r| finish(r.is_ok())));
                                }
                            }
                        }));
                }
            }));
        }));
    }
}

impl Keyring for Rc<SecretService> {
    fn lookup(&self, uuid: &str, setting: &str, interactive: bool, done: Box<dyn FnOnce(Values, Option<String>)>) {
        let attrs = attributes(&[("connection-uuid", uuid), ("setting-name", setting)]);
        let this = self.clone();
        self.with_bus_ready(Box::new(move |bus| {
            let Some(bus) = bus else { return done(Vec::new(), Some(READ_FAILED.into())) };
            let bus2 = bus.clone();
            SecretService::search(&bus, attrs, Box::new(move |result| {
                let Ok((unlocked, locked)) = result else { return done(Vec::new(), Some(READ_FAILED.into())) };
                if interactive && !locked.is_empty() {
                    let this2 = this.clone();
                    let bus3 = bus2.clone();
                    SecretService::unlock(&bus2, locked, Box::new(move |result| {
                        let mut items = unlocked;
                        match result {
                            Ok(more) => items.extend(more),
                            Err(_) => return done(Vec::new(), Some(READ_FAILED.into())),
                        }
                        this2.lookup_items(bus3, items, done);
                    }));
                } else {
                    this.lookup_items(bus2, unlocked, done);
                }
            }));
        }));
    }

    fn save(&self, connection: &Connection, setting_names: Option<Vec<String>>, done: Box<dyn FnOnce(Option<String>)>) {
        let mut entries = storable_secrets(connection);
        if let Some(names) = &setting_names {
            entries.retain(|e| names.contains(&e.0));
        }
        let (uuid, label) = (connection.uuid(), format!("Network password for {}", connection.id()));
        let this = self.clone();
        self.with_bus_ready(Box::new(move |bus| {
            let Some(bus) = bus else { return done(Some(SAVE_FAILED.into())) };
            match setting_names {
                // A GetSecrets request may carry one setting's credentials only:
                // keep other settings' saved passwords.
                Some(names) => {
                    if names.is_empty() {
                        return this.store(bus, uuid, label, entries, done);
                    }
                    let remaining = Rc::new(std::cell::Cell::new(names.len()));
                    let failure: Rc<RefCell<bool>> = Rc::default();
                    let next: Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>> = Rc::new(RefCell::new(None));
                    {
                        let (this, bus, uuid, label) = (this.clone(), bus.clone(), uuid.clone(), label.clone());
                        next.replace(Some(Box::new(move |failed| {
                            if failed {
                                done(Some(UPDATE_FAILED.into()));
                            } else {
                                this.store(bus, uuid, label, entries, done);
                            }
                        })));
                    }
                    for name in names {
                        let (remaining, failure, next) = (remaining.clone(), failure.clone(), next.clone());
                        SecretService::clear(&bus, attributes(&[("connection-uuid", &uuid), ("setting-name", &name)]), Box::new(move |r| {
                            if r.is_err() {
                                failure.replace(true);
                            }
                            remaining.set(remaining.get() - 1);
                            if remaining.get() == 0 {
                                if let Some(next) = next.borrow_mut().take() {
                                    next(*failure.borrow());
                                }
                            }
                        }));
                    }
                }
                None => {
                    let (this, bus2) = (this.clone(), bus.clone());
                    SecretService::clear(&bus, attributes(&[("connection-uuid", &uuid)]), Box::new(move |r| {
                        if r.is_err() {
                            return done(Some(DELETE_FAILED.into()));
                        }
                        this.store(bus2, uuid, label, entries, done);
                    }));
                }
            }
        }));
    }

    fn delete(&self, uuid: &str, done: Box<dyn FnOnce(Option<String>)>) {
        let attrs = attributes(&[("connection-uuid", uuid)]);
        self.with_bus_ready(Box::new(move |bus| {
            let Some(bus) = bus else { return done(Some(DELETE_FAILED.into())) };
            SecretService::clear(&bus, attrs, Box::new(move |r| done(r.err().map(|_| DELETE_FAILED.to_owned()))));
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use suite_core::bus::FakeBus;

    fn wifi_connection(flags: u32, psk: &str) -> Connection {
        let connection = Connection::new().unwrap();
        let general = Setting::new("connection").unwrap();
        general.set_str("id", Some("Synthetic credential test"));
        general.set_str("uuid", Some("11111111-2222-4333-8444-555555555555"));
        general.set_str("type", Some("802-11-wireless"));
        connection.add_setting(&general);
        let wireless = Setting::new("802-11-wireless").unwrap();
        wireless.set_bytes("ssid", Some(b"Synthetic network"));
        connection.add_setting(&wireless);
        let security = Setting::new("802-11-wireless-security").unwrap();
        security.set_str("key-mgmt", Some("wpa-psk"));
        security.set_secret_flags("psk", flags).unwrap();
        security.set_str("psk", Some(psk));
        connection.add_setting(&security);
        connection
    }

    fn attr(value: &Variant, key: &str) -> Option<String> {
        value.iter().find(|e| e.child_value(0).str() == Some(key)).and_then(|e| e.child_value(1).str().map(String::from))
    }

    #[test]
    fn lookup_uses_search_unlocked_items_and_plain_session() {
        let bus = FakeBus::new();
        let service = SecretService::with_bus(bus.clone());
        let replies: Rc<RefCell<Vec<(Values, Option<String>)>>> = Rc::default();
        let r = replies.clone();
        service.lookup("uuid-1", "802-11-wireless-security", false, Box::new(move |v, e| r.borrow_mut().push((v, e))));
        let search = bus.last_call();
        let attrs = search.params.clone().unwrap().child_value(0);
        assert_eq!(attr(&attrs, "connection-uuid").as_deref(), Some("uuid-1"));
        assert!(attr(&attrs, "xdg:schema").is_none(), "DONT_MATCH_NAME");
        let items = Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, [obj("/item/1")]);
        let locked = Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, [obj("/item/2")]);
        bus.reply("SearchItems", Ok(tuple(&[items, locked])), None, None, 0);
        assert!(!bus.called("Unlock"), "non-interactive lookups never unlock");
        bus.reply("OpenSession", Ok(tuple(&["".to_variant().to_variant(), obj("/session/1")])), None, None, 0);
        let secret = tuple(&[obj("/session/1"), Vec::<u8>::new().to_variant(), b"synthetic-passphrase".to_vec().to_variant(), "text/plain".to_variant()]);
        let entry = Variant::from_dict_entry(&obj("/item/1"), &secret);
        bus.reply("GetSecrets", Ok(tuple(&[Variant::array_from_iter_with_type(glib::VariantTy::new("{o(oayays)}").unwrap(), [entry])])), None, None, 0);
        let attrs = attributes(&[("setting-key", "psk"), ("setting-name", "802-11-wireless-security")]);
        bus.reply("Get", Ok(tuple(&[attrs.to_variant()])), Some("/item/1"), None, 0);
        assert_eq!(*replies.borrow(), [(vec![("psk".to_string(), "synthetic-passphrase".to_string())], None)]);
    }

    #[test]
    fn prompt_save_clears_only_requested_setting() {
        let bus = FakeBus::new();
        let service = SecretService::with_bus(bus.clone());
        let connection = wifi_connection(libnm::AGENT_OWNED, "synthetic-wifi-passphrase");
        let dot1x = Setting::new("802-1x").unwrap();
        dot1x.set_secret_flags("password", libnm::AGENT_OWNED).unwrap();
        dot1x.set_str("password", Some("synthetic-enterprise-passphrase"));
        connection.add_setting(&dot1x);
        let replies: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let r = replies.clone();
        service.save(&connection, Some(vec!["802-1x".into()]), Box::new(move |e| r.borrow_mut().push(e)));
        let search = bus.last_call().params.unwrap().child_value(0);
        assert_eq!(attr(&search, "setting-name").as_deref(), Some("802-1x"));
        let item = Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, [obj("/old")]);
        bus.reply("SearchItems", Ok(tuple(&[item, Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, Vec::<Variant>::new())])), None, None, 0);
        bus.reply("Delete", Ok(tuple(&[obj("/")])), Some("/old"), None, 0);
        bus.reply("ReadAlias", Ok(tuple(&[obj("/collection/login")])), None, None, 0);
        bus.reply("Get", Ok(tuple(&[false.to_variant().to_variant()])), None, None, 0);
        bus.reply("OpenSession", Ok(tuple(&["".to_variant().to_variant(), obj("/session/1")])), None, None, 0);
        let create = bus.last_call();
        assert_eq!(create.method, "CreateItem");
        let params = create.params.unwrap();
        let properties = params.child_value(0);
        let attrs = properties.iter().find(|e| e.child_value(0).str() == Some("org.freedesktop.Secret.Item.Attributes")).unwrap()
            .child_value(1).as_variant().unwrap();
        assert_eq!(attr(&attrs, "setting-name").as_deref(), Some("802-1x"));
        assert_eq!(attr(&attrs, "xdg:schema").as_deref(), Some(SCHEMA));
        assert_eq!(params.child_value(1).child_value(2).fixed_array::<u8>().unwrap(), b"synthetic-enterprise-passphrase");
        bus.reply("CreateItem", Ok(tuple(&[obj("/new"), obj("/")])), None, None, 0);
        assert!(!bus.has_pending("CreateItem"), "only one secret stored");
        assert_eq!(*replies.borrow(), [None]);
    }

    #[test]
    fn ask_every_time_clears_old_secret_without_storing() {
        let bus = FakeBus::new();
        let service = SecretService::with_bus(bus.clone());
        let connection = wifi_connection(libnm::NOT_SAVED, "synthetic-temporary-passphrase");
        let replies: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let r = replies.clone();
        service.save(&connection, Some(vec!["802-11-wireless-security".into()]), Box::new(move |e| r.borrow_mut().push(e)));
        let empty = || Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, Vec::<Variant>::new());
        bus.reply("SearchItems", Ok(tuple(&[empty(), empty()])), None, None, 0);
        assert!(!bus.called("CreateItem"));
        assert_eq!(*replies.borrow(), [None]);
    }

    #[test]
    fn storable_secret_flags() {
        for (flags, expected) in [(0, false), (libnm::AGENT_OWNED, true), (libnm::NOT_SAVED, false), (libnm::AGENT_OWNED | libnm::NOT_SAVED, false)] {
            assert_eq!(!storable_secrets(&wifi_connection(flags, "synthetic-passphrase")).is_empty(), expected, "{flags}");
        }
    }

    #[test]
    fn vpn_and_wep_storable_secret_flags() {
        let connection = Connection::new().unwrap();
        let general = Setting::new("connection").unwrap();
        general.set_str("uuid", Some("11111111-2222-4333-8444-555555555556"));
        general.set_str("type", Some("vpn"));
        connection.add_setting(&general);
        let vpn = Setting::new("vpn").unwrap();
        vpn.set_str("service-type", Some("org.freedesktop.NetworkManager.openvpn"));
        vpn.vpn_add_data_item("password-flags", "1");
        vpn.vpn_add_secret("password", "synthetic-passphrase");
        vpn.vpn_add_secret("token", "synthetic-temporary-token");
        vpn.vpn_add_data_item("token-flags", "2");
        connection.add_setting(&vpn);
        assert_eq!(storable_secrets(&connection), [("vpn".to_string(), "password".to_string(), "synthetic-passphrase".to_string())]);
        let wifi = wifi_connection(libnm::AGENT_OWNED, "");
        let security = wifi.setting("802-11-wireless-security").unwrap();
        security.set_str("key-mgmt", Some("none"));
        security.set_int("wep-key-flags", libnm::AGENT_OWNED as i64);
        security.set_str("wep-key0", Some("abcde"));
        security.set_str("wep-key2", Some("fghij"));
        let keys: std::collections::BTreeSet<String> = storable_secrets(&wifi).into_iter().map(|e| e.1).collect();
        assert_eq!(keys, ["wep-key0".to_string(), "wep-key2".to_string()].into_iter().collect());
        security.set_int("wep-key-flags", libnm::NOT_SAVED as i64);
        assert!(storable_secrets(&wifi).is_empty());
    }
}
