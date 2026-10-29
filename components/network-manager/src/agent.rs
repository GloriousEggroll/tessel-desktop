//! `secret_agent.py`: NetworkManager authentication with our dialogs, VPN
//! auth-dialog helpers and the keyring, as a raw D-Bus secret agent
//! (`org.freedesktop.NetworkManager.SecretAgent` at
//! `/org/freedesktop/NetworkManager/SecretAgent`, registered through
//! `AgentManager.RegisterWithCapabilities("org.waybar.NetworkManager", VPN_HINTS)`
//! and re-registered whenever NetworkManager (re)appears).
//!
//! [`AgentCore`] is the request logic (testable without a bus); [`AgentService`]
//! is the D-Bus transport. Requests from anyone but NetworkManager's unique
//! bus name are rejected (libnm's `verify_request`).

use crate::keyring::{secret_flags, set_secret_flags, variant_secrets, Keyring};
use crate::libnm::{self, Connection};
use crate::nm::*;
use crate::vpn_auth::{self, Field, PluginMeta, VpnProcess, ALLOW_INTERACTION, REQUEST_NEW};
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::rc::{Rc, Weak};
use std::time::Duration;
use suite_core::tr;

pub const IDENTIFIER: &str = "org.waybar.NetworkManager";
pub const VPN_HINTS: u32 = 0x1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentError {
    Failed,
    PermissionDenied,
    InvalidConnection,
    UserCanceled,
    AgentCanceled,
    NoSecrets,
}

impl AgentError {
    pub fn dbus_name(self) -> &'static str {
        match self {
            Self::Failed => "org.freedesktop.NetworkManager.SecretAgent.Failed",
            Self::PermissionDenied => "org.freedesktop.NetworkManager.SecretAgent.PermissionDenied",
            Self::InvalidConnection => "org.freedesktop.NetworkManager.SecretAgent.InvalidConnection",
            Self::UserCanceled => "org.freedesktop.NetworkManager.SecretAgent.UserCanceled",
            Self::AgentCanceled => "org.freedesktop.NetworkManager.SecretAgent.AgentCanceled",
            Self::NoSecrets => "org.freedesktop.NetworkManager.SecretAgent.NoSecrets",
        }
    }
}

pub type Failure = (AgentError, String);
pub type SecretsReply = Box<dyn FnOnce(Result<Variant, Failure>)>;
pub type OperationReply = Box<dyn FnOnce(Option<Failure>)>;

/// An open credentials prompt.
pub trait PromptHandle {
    fn close(&self);
}

/// The dialogs the agent shows (GTK in the app, fakes in tests).
pub trait Prompter {
    /// `_fields_dialog`; `remember` = initial state of "Remember credentials
    /// in the keyring" (`None`: no checkbox). `done(None)` = cancelled, else
    /// the entered `(key, value)`s and the checkbox state. Entries must be
    /// blanked afterwards.
    fn fields(&self, title: &str, message: &str, fields: &[Field], remember: Option<bool>,
              done: Box<dyn FnOnce(Option<(Vec<(String, String)>, bool)>)>) -> Result<Box<dyn PromptHandle>, String>;
    /// `on_prompt(None)`: dismiss our popup before a plugin maps its own window.
    fn before_helper(&self);
}

pub type Spawn = Rc<dyn Fn(Vec<String>, Vec<u8>, vpn_auth::Delivery) -> VpnProcess>;

struct Request {
    connection: Connection,
    path: String,
    setting: String,
    hints: Vec<String>,
    flags: u32,
    reply: Option<SecretsReply>,
    dialog: Option<Box<dyn PromptHandle>>,
    process: Option<VpnProcess>,
}

pub struct AgentCore {
    keyring: Rc<dyn Keyring>,
    prompter: Rc<dyn Prompter>,
    on_error: Box<dyn Fn(&str)>,
    pub plugin_lookup: RefCell<Rc<dyn Fn(&str) -> Option<PluginMeta>>>,
    pub trusted: RefCell<Rc<dyn Fn(&Path) -> bool>>,
    pub spawn: RefCell<Spawn>,
    requests: RefCell<BTreeMap<u64, Request>>,
    operations: RefCell<BTreeMap<u64, OperationReply>>,
    queue: RefCell<VecDeque<u64>>,
    active: Cell<Option<u64>>,
    next_token: Cell<u64>,
    closed: Cell<bool>,
    pub error: RefCell<String>,
    weak: RefCell<Weak<AgentCore>>,
}

fn fail(error: AgentError, message: &str) -> Failure {
    (error, message.to_owned())
}

impl AgentCore {
    pub fn new(keyring: Rc<dyn Keyring>, prompter: Rc<dyn Prompter>, on_error: impl Fn(&str) + 'static) -> Rc<Self> {
        let core = Rc::new(Self {
            keyring, prompter, on_error: Box::new(on_error),
            plugin_lookup: RefCell::new(Rc::new(vpn_auth::plugin_meta)),
            trusted: RefCell::new(Rc::new(vpn_auth::trusted_helper)),
            spawn: RefCell::new(Rc::new(|argv, payload, done| VpnProcess::spawn(argv, payload, done, Duration::from_secs(300)))),
            requests: RefCell::default(), operations: RefCell::default(), queue: RefCell::default(), active: Cell::new(None),
            next_token: Cell::new(0), closed: Cell::new(false), error: RefCell::default(), weak: RefCell::new(Weak::new()),
        });
        core.weak.replace(Rc::downgrade(&core));
        core
    }

    fn weak(&self) -> Weak<AgentCore> {
        self.weak.borrow().clone()
    }

    pub fn closed(&self) -> bool {
        self.closed.get()
    }

    pub fn pending(&self) -> usize {
        self.requests.borrow().len()
    }

    fn report(&self, message: &str) {
        self.error.replace(message.to_owned());
        if !self.closed.get() {
            (self.on_error)(message);
        }
    }

    fn alive(&self, token: u64) -> bool {
        self.requests.borrow().contains_key(&token)
    }

    fn with_request<T>(&self, token: u64, f: impl FnOnce(&mut Request) -> T) -> Option<T> {
        self.requests.borrow_mut().get_mut(&token).map(f)
    }

    /// `GetSecrets(connection, path, setting, hints, flags)`.
    pub fn get(&self, connection: &Variant, path: &str, setting: &str, hints: Vec<String>, flags: u32, reply: SecretsReply) {
        let parsed = Connection::from_dbus(connection).ok();
        let Some(clone) = parsed.filter(|c| !self.closed.get() && !c.uuid().is_empty() && c.setting(setting).is_some()) else {
            reply(Err(fail(AgentError::InvalidConnection, "The requested connection is unavailable.")));
            return;
        };
        clone.set_path(path);
        let token = self.next_token.get() + 1;
        self.next_token.set(token);
        let uuid = clone.uuid();
        self.requests.borrow_mut().insert(token, Request {
            connection: clone, path: path.into(), setting: setting.into(), hints, flags, reply: Some(reply), dialog: None, process: None,
        });
        let weak = self.weak();
        let setting_name = setting.to_owned();
        self.keyring.lookup(&uuid, setting, flags & ALLOW_INTERACTION != 0, Box::new(move |values, _error| {
            let Some(core) = weak.upgrade() else { return };
            core.found(token, &setting_name, values);
        }));
    }

    fn found(&self, token: u64, setting: &str, values: Vec<(String, String)>) {
        let Some(connection) = self.with_request(token, |r| r.connection.clone()) else { return };
        if !values.is_empty() && connection.update_secrets(setting, &variant_secrets(setting, &values)).is_err() {
            return self.finish(token, Err(fail(AgentError::Failed, "Saved network credentials could not be used.")));
        }
        if setting == "vpn" {
            return self.enqueue(token);
        }
        let (hints, flags) = self.with_request(token, |r| (r.hints.clone(), r.flags)).unwrap();
        let known = connection.flat_secrets(setting);
        let has = |key: &str| known.iter().any(|(k, _)| k == key);
        let (needed, _) = connection.need_secrets();
        let missing = known.is_empty() || hints.iter().any(|h| !has(h)) || needed.as_deref() == Some(setting);
        let target = connection.setting(setting).unwrap();
        let always_ask = known.iter().any(|(k, _)| secret_flags(&target, k) & libnm::NOT_SAVED != 0);
        if flags & REQUEST_NEW == 0 && !missing && !always_ask {
            self.finish(token, Ok(variant_secrets(setting, &known)));
        } else if flags & ALLOW_INTERACTION != 0 {
            self.enqueue(token);
        } else {
            self.finish(token, Err(fail(AgentError::NoSecrets, "No saved credentials are available for this connection.")));
        }
    }

    fn enqueue(&self, token: u64) {
        self.queue.borrow_mut().push_back(token);
        self.next_prompt();
    }

    fn next_prompt(&self) {
        if self.closed.get() || self.active.get().is_some() {
            return;
        }
        loop {
            let Some(token) = self.queue.borrow_mut().pop_front() else { return };
            let Some(setting) = self.with_request(token, |r| r.setting.clone()) else { continue };
            self.active.set(Some(token));
            let result = if setting == "vpn" { self.prompt_vpn(token) } else { self.prompt_connection(token) };
            if result.is_err() {
                self.finish(token, Err(fail(AgentError::Failed, "The network authentication dialog could not be opened.")));
            }
            break;
        }
    }

    /// `_connection_fields(request)`.
    pub fn connection_fields(connection: &Connection, setting_name: &str, hints: &[String]) -> Vec<Field> {
        let Some(setting) = connection.setting(setting_name) else { return Vec::new() };
        let known = connection.flat_secrets(setting_name);
        let mut hints: Vec<String> = hints.to_vec();
        if hints.is_empty() {
            let (required, needed) = connection.need_secrets();
            if required.as_deref() == Some(setting_name) {
                hints = needed;
            }
        }
        if hints.is_empty() {
            hints = match setting_name {
                "802-1x" | "pppoe" | "gsm" | "cdma" | "adsl" => vec!["password".into()],
                "wireguard" => vec!["private-key".into()],
                "802-11-wireless-security" => {
                    let key_mgmt = setting.get_string("key-mgmt").unwrap_or_default();
                    vec![match key_mgmt.as_str() {
                        "none" => format!("wep-key{}", setting.get_u32("wep-tx-keyidx")),
                        "ieee8021x" => "leap-password".into(),
                        _ => "psk".into(),
                    }]
                }
                _ => Vec::new(),
            };
        }
        let mut fields = Vec::new();
        for key in hints {
            let peer_key = setting_name == "wireguard" && key.starts_with("peers.") && key.ends_with(".preshared-key");
            if !peer_key {
                match setting.find_property(&key) {
                    Some(spec) if libnm::Setting::raw_flags(&spec) & libnm::PARAM_SECRET != 0 => {}
                    _ => continue,
                }
            }
            let caption = if peer_key { tr!("Peer preshared key") } else { suite_core::i18n::translate(&capitalize(&key.replace('-', " "))) };
            let value = known.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()).unwrap_or_default();
            fields.push((key, caption, value, true));
        }
        fields
    }

    fn prompt_connection(&self, token: u64) -> Result<(), String> {
        let (connection, setting, hints, flags) = self.with_request(token, |r| (r.connection.clone(), r.setting.clone(), r.hints.clone(), r.flags)).unwrap();
        if flags & ALLOW_INTERACTION == 0 {
            self.finish(token, Err(fail(AgentError::NoSecrets, "Interactive authentication is not allowed.")));
            return Ok(());
        }
        if flags & REQUEST_NEW != 0 {
            connection.clear_secrets();
        }
        let fields = Self::connection_fields(&connection, &setting, &hints);
        if fields.is_empty() {
            self.finish(token, Err(fail(AgentError::NoSecrets, "This connection does not request an editable credential.")));
            return Ok(());
        }
        let target = connection.setting(&setting).unwrap();
        let remember = !fields.iter().any(|(k, ..)| secret_flags(&target, k) & libnm::NOT_SAVED != 0);
        let weak = self.weak();
        let keys: Vec<String> = fields.iter().map(|f| f.0.clone()).collect();
        let handle = self.prompter.fields(&tr!("Network authentication"),
            &tr!("Enter credentials for {connection}.", connection = connection.id()), &fields, Some(remember),
            Box::new(move |response| {
                if let Some(core) = weak.upgrade() {
                    core.fields_response(token, response, Vec::new(), Some(keys));
                }
            }))?;
        self.attach_dialog(token, handle);
        Ok(())
    }

    fn attach_dialog(&self, token: u64, handle: Box<dyn PromptHandle>) {
        let stale = self.with_request(token, |r| r.dialog.replace(handle)).is_none();
        if stale {
            // The response already finished the request.
        }
    }

    /// `_fields_response`: `remember_keys` is `None` for VPN plugin fields.
    fn fields_response(&self, token: u64, response: Option<(Vec<(String, String)>, bool)>, existing: Vec<(String, String)>,
                       remember_keys: Option<Vec<String>>) {
        if !self.alive(token) {
            return;
        }
        let Some((entries, remember)) = response else {
            return self.finish(token, Err(fail(AgentError::UserCanceled, "Network authentication was cancelled.")));
        };
        let (connection, setting) = self.with_request(token, |r| (r.connection.clone(), r.setting.clone())).unwrap();
        let mut values = existing;
        for (k, v) in entries {
            values.retain(|(x, _)| *x != k);
            values.push((k, v));
        }
        let result: Result<Variant, ()> = (|| {
            if let Some(keys) = &remember_keys {
                let flags = if remember { libnm::AGENT_OWNED } else { libnm::NOT_SAVED };
                let target = connection.setting(&setting).ok_or(())?;
                for key in keys {
                    set_secret_flags(&target, key, flags).map_err(|_| ())?;
                }
            }
            let secrets = variant_secrets(&setting, &values);
            connection.update_secrets(&setting, &secrets).map_err(|_| ())?;
            connection.verify_secrets().map_err(|_| ())?;
            Ok(secrets)
        })();
        match result {
            Ok(secrets) => self.accept(token, secrets),
            Err(()) => self.finish(token, Err(fail(AgentError::Failed, "The supplied credentials could not be used."))),
        }
    }

    fn prompt_vpn(&self, token: u64) -> Result<(), String> {
        let (connection, hints, flags) = self.with_request(token, |r| (r.connection.clone(), r.hints.clone(), r.flags)).unwrap();
        let lookup = self.plugin_lookup.borrow().clone();
        let trusted = self.trusted.borrow().clone();
        let (argv, external) = vpn_auth::auth_command(&connection, &hints, flags, &*lookup, &*trusted)?;
        if !external && flags & ALLOW_INTERACTION != 0 {
            self.prompter.before_helper();
        }
        let payload = vpn_auth::request_payload(&connection)?;
        let weak = self.weak();
        let spawn = self.spawn.borrow().clone();
        let process = spawn(argv, payload, Box::new(move |output, error| {
            if let Some(core) = weak.upgrade() {
                core.vpn_finished(token, output, error, external);
            }
        }));
        self.with_request(token, |r| r.process = Some(process));
        Ok(())
    }

    fn vpn_finished(&self, token: u64, output: Vec<u8>, error: Option<String>, external: bool) {
        let Some((connection, flags)) = self.with_request(token, |r| {
            r.process = None;
            (r.connection.clone(), r.flags)
        }) else { return };
        if let Some(error) = error {
            return self.finish(token, Err((AgentError::UserCanceled, error)));
        }
        let invalid = || fail(AgentError::Failed, "The VPN authentication helper returned an invalid response.");
        match vpn_auth::parse_response(&output, external) {
            Err(_) => self.finish(token, Err(invalid())),
            Ok((title, message, values, fields)) => {
                if !fields.is_empty() {
                    if flags & ALLOW_INTERACTION == 0 {
                        return self.finish(token, Err(fail(AgentError::NoSecrets, "The VPN requires interactive authentication.")));
                    }
                    let weak = self.weak();
                    let existing = values.clone();
                    let opened = self.prompter.fields(title.as_deref().unwrap_or(""), message.as_deref().unwrap_or(""), &fields, None,
                        Box::new(move |response| {
                            if let Some(core) = weak.upgrade() {
                                core.fields_response(token, response, existing, None);
                            }
                        }));
                    match opened {
                        Ok(handle) => self.attach_dialog(token, handle),
                        Err(_) => self.finish(token, Err(invalid())),
                    }
                } else {
                    let secrets = variant_secrets("vpn", &values);
                    match connection.update_secrets("vpn", &secrets) {
                        Ok(()) => self.accept(token, secrets),
                        Err(_) => self.finish(token, Err(invalid())),
                    }
                }
            }
        }
    }

    fn accept(&self, token: u64, secrets: Variant) {
        let Some((connection, setting, flags)) = self.with_request(token, |r| (r.connection.clone(), r.setting.clone(), r.flags)) else { return };
        if flags & ALLOW_INTERACTION != 0 {
            // Saving continues even after the request has been answered.
            let weak = self.weak();
            self.keyring.save(&connection, Some(vec![setting]), Box::new(move |error| {
                if let (Some(core), Some(error)) = (weak.upgrade(), error) {
                    core.report(&error);
                }
            }));
        }
        self.finish(token, Ok(secrets));
    }

    fn finish(&self, token: u64, result: Result<Variant, Failure>) {
        let Some(mut request) = self.requests.borrow_mut().remove(&token) else { return };
        if let Some(process) = request.process.take() {
            process.cancel();
        }
        if let Some(dialog) = request.dialog.take() {
            dialog.close();
        }
        if self.active.get() == Some(token) {
            self.active.set(None);
        }
        if let Some(reply) = request.reply.take() {
            reply(result);
        }
        request.connection.clear_secrets();
        let weak = self.weak();
        // Detached: the timer runs once on the thread-default context.
        let _ = suite_core::timer::once(0, move || {
            if let Some(core) = weak.upgrade() {
                core.next_prompt();
            }
        });
    }

    /// `CancelGetSecrets(path, setting)`.
    pub fn cancel(&self, path: &str, setting: &str) {
        let tokens: Vec<u64> = self.requests.borrow().iter().filter(|(_, r)| r.path == path && r.setting == setting).map(|(t, _)| *t).collect();
        for token in tokens {
            self.finish(token, Err(fail(AgentError::AgentCanceled, "NetworkManager cancelled authentication.")));
        }
    }

    /// `SaveSecrets` / `DeleteSecrets`.
    pub fn save_delete(&self, save: bool, connection: &Variant, reply: OperationReply) {
        let token = self.next_token.get() + 1;
        self.next_token.set(token);
        let parsed = Connection::from_dbus(connection).ok().filter(|c| !c.uuid().is_empty());
        let Some(parsed) = parsed.filter(|_| !self.closed.get()) else {
            reply(Some(fail(AgentError::AgentCanceled, "Network authentication is unavailable.")));
            return;
        };
        self.operations.borrow_mut().insert(token, reply);
        let weak = self.weak();
        let finished: Box<dyn FnOnce(Option<String>)> = Box::new(move |message| {
            let Some(core) = weak.upgrade() else { return };
            let reply = core.operations.borrow_mut().remove(&token);
            if let Some(reply) = reply {
                reply(message.map(|m| (AgentError::Failed, m)));
            }
        });
        if save {
            self.keyring.save(&parsed, None, finished);
        } else {
            self.keyring.delete(&parsed.uuid(), finished);
        }
        parsed.clear_secrets();
    }

    pub fn close(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        let tokens: Vec<u64> = self.requests.borrow().keys().copied().collect();
        for token in tokens {
            self.finish(token, Err(fail(AgentError::AgentCanceled, "Network authentication is closing.")));
        }
        let operations: Vec<OperationReply> = std::mem::take(&mut *self.operations.borrow_mut()).into_values().collect();
        for reply in operations {
            reply(Some(fail(AgentError::AgentCanceled, "Network authentication is closing.")));
        }
        self.queue.borrow_mut().clear();
    }
}

const AGENT_XML: &str = r#"<node><interface name="org.freedesktop.NetworkManager.SecretAgent">
  <method name="GetSecrets"><arg name="connection" type="a{sa{sv}}" direction="in"/><arg name="connection_path" type="o" direction="in"/>
    <arg name="setting_name" type="s" direction="in"/><arg name="hints" type="as" direction="in"/><arg name="flags" type="u" direction="in"/>
    <arg name="secrets" type="a{sa{sv}}" direction="out"/></method>
  <method name="CancelGetSecrets"><arg name="connection_path" type="o" direction="in"/><arg name="setting_name" type="s" direction="in"/></method>
  <method name="SaveSecrets"><arg name="connection" type="a{sa{sv}}" direction="in"/><arg name="connection_path" type="o" direction="in"/></method>
  <method name="DeleteSecrets"><arg name="connection" type="a{sa{sv}}" direction="in"/><arg name="connection_path" type="o" direction="in"/></method>
</interface></node>"#;

/// The D-Bus side: export, verify callers, (re-)register, unregister on close.
pub struct AgentService {
    pub core: Rc<AgentCore>,
    connection: RefCell<Option<gio::DBusConnection>>,
    registration: RefCell<Option<gio::RegistrationId>>,
    watch: RefCell<Option<gio::SignalSubscriptionId>>,
    owner: RefCell<Option<String>>,
    registered: Cell<bool>,
    started: Cell<bool>,
    serial: Cell<u64>,
    weak: RefCell<Weak<AgentService>>,
}

impl AgentService {
    pub fn new(core: Rc<AgentCore>) -> Rc<Self> {
        let service = Rc::new(Self { core, connection: RefCell::new(None), registration: RefCell::new(None), watch: RefCell::new(None),
                                     owner: RefCell::new(None), registered: Cell::new(false), started: Cell::new(false),
                                     serial: Cell::new(0), weak: RefCell::new(Weak::new()) });
        service.weak.replace(Rc::downgrade(&service));
        service
    }

    /// `ready` = registered with NetworkManager (what `--check` reports).
    pub fn ready(&self) -> bool {
        self.registered.get() && !self.core.closed()
    }

    pub fn start(&self) {
        if self.started.get() || self.core.closed() {
            return;
        }
        self.started.set(true);
        let weak = self.weak.borrow().clone();
        gio::bus_get(gio::BusType::System, None::<&gio::Cancellable>, move |result| {
            let Some(service) = weak.upgrade() else { return };
            match result {
                Ok(connection) => service.attach(connection),
                Err(_) => service.core.report("Network authentication could not start. Restart Network Settings to retry."),
            }
        });
    }

    pub fn attach(&self, connection: gio::DBusConnection) {
        if self.core.closed() {
            return;
        }
        let info = gio::DBusNodeInfo::for_xml(AGENT_XML).unwrap();
        let interface = info.lookup_interface(AGENT_IFACE).unwrap();
        let weak = self.weak.borrow().clone();
        let registered = connection.register_object(AGENT_PATH, &interface)
            .method_call(move |_, sender, _, _, method, params, invocation| {
                match weak.upgrade() {
                    Some(service) => service.method(sender.unwrap_or(""), method, &params, invocation),
                    None => invocation.return_dbus_error(AgentError::AgentCanceled.dbus_name(), "Network authentication is closing."),
                }
            }).build();
        match registered {
            Ok(id) => { self.registration.replace(Some(id)); }
            Err(_) => {
                self.core.report("Network authentication could not start. Restart Network Settings to retry.");
                return;
            }
        }
        let weak = self.weak.borrow().clone();
        #[allow(deprecated)]
        let watch = connection.signal_subscribe(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"), Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"), Some(NM_BUS), gio::DBusSignalFlags::NONE, move |_, _, _, _, _, params| {
                if let (Some(service), Some((_, _, new))) = (weak.upgrade(), params.get::<(String, String, String)>()) {
                    service.owner_changed(&new);
                }
            });
        self.watch.replace(Some(watch));
        self.connection.replace(Some(connection.clone()));
        let weak = self.weak.borrow().clone();
        connection.call(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner",
                        Some(&(NM_BUS,).to_variant()), None, gio::DBusCallFlags::NONE, 10000, None::<&gio::Cancellable>, move |reply| {
            if let Some(service) = weak.upgrade() {
                let owner = reply.ok().and_then(|v| v.get::<(String,)>()).map(|v| v.0).unwrap_or_default();
                service.owner_changed(&owner);
            }
        });
    }

    fn owner_changed(&self, owner: &str) {
        self.registered.set(false);
        self.serial.set(self.serial.get() + 1);
        if owner.is_empty() || self.core.closed() {
            self.owner.replace(None);
            return;
        }
        self.owner.replace(Some(owner.to_owned()));
        let Some(connection) = self.connection.borrow().clone() else { return };
        let serial = self.serial.get();
        let weak = self.weak.borrow().clone();
        connection.call(Some(NM_BUS), AGENT_MANAGER_PATH, AGENT_MANAGER_IFACE, "RegisterWithCapabilities",
                        Some(&(IDENTIFIER, VPN_HINTS).to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 25000,
                        None::<&gio::Cancellable>, move |reply| {
            let Some(service) = weak.upgrade() else { return };
            if service.serial.get() != serial || service.core.closed() {
                return;
            }
            match reply {
                Ok(_) => {
                    service.registered.set(true);
                    service.core.error.replace(String::new());
                }
                Err(_) => service.core.report("Network authentication could not start. Restart Network Settings to retry."),
            }
        });
    }

    fn method(&self, sender: &str, method: &str, params: &Variant, invocation: gio::DBusMethodInvocation) {
        // Only NetworkManager may ask for or hand over credentials.
        let trusted = self.owner.borrow().as_deref().is_some_and(|owner| owner == sender);
        if !trusted {
            invocation.return_dbus_error(AgentError::PermissionDenied.dbus_name(), "Request by non-NetworkManager client rejected.");
            return;
        }
        match method {
            "GetSecrets" => {
                let connection = params.child_value(0);
                let path = params.child_value(1).str().unwrap_or_default().to_owned();
                let setting = params.child_value(2).str().unwrap_or_default().to_owned();
                let hints: Vec<String> = params.child_value(3).get().unwrap_or_default();
                let flags = params.child_value(4).get::<u32>().unwrap_or(0);
                self.core.get(&connection, &path, &setting, hints, flags, Box::new(move |result| match result {
                    Ok(secrets) => invocation.return_value(Some(&Variant::tuple_from_iter([secrets]))),
                    Err((error, message)) => invocation.return_dbus_error(error.dbus_name(), &message),
                }));
            }
            "CancelGetSecrets" => {
                let path = params.child_value(0).str().unwrap_or_default().to_owned();
                let setting = params.child_value(1).str().unwrap_or_default().to_owned();
                self.core.cancel(&path, &setting);
                invocation.return_value(None);
            }
            "SaveSecrets" | "DeleteSecrets" => {
                self.core.save_delete(method == "SaveSecrets", &params.child_value(0), Box::new(move |error| match error {
                    None => invocation.return_value(None),
                    Some((error, message)) => invocation.return_dbus_error(error.dbus_name(), &message),
                }));
            }
            _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", "Unknown method"),
        }
    }

    pub fn close(&self) {
        let was_registered = self.registered.get();
        self.registered.set(false);
        self.core.close();
        let Some(connection) = self.connection.take() else { return };
        if was_registered {
            // Delivered before the process exits (libnm unregisters on destroy).
            let _ = connection.call_sync(Some(NM_BUS), AGENT_MANAGER_PATH, AGENT_MANAGER_IFACE, "Unregister", None, None,
                                         gio::DBusCallFlags::NO_AUTO_START, 2000, None::<&gio::Cancellable>);
        }
        if let Some(id) = self.watch.take() {
            #[allow(deprecated)]
            connection.signal_unsubscribe(id);
        }
        if let Some(id) = self.registration.take() {
            let _ = connection.unregister_object(id);
        }
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
