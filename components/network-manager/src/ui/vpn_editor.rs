//! `vpn_editor.py`: our VPN settings form and NetworkManager's non-UI
//! import/export plugins. No client, secret service, native editor widget or
//! external editor process is created here; edits affect a caller-owned
//! in-memory connection. Plugin loading keeps libnm's ownership checks.

use crate::libnm::{self, Connection, EditorPlugin, VpnPluginInfo};
use crate::ui::style::{expand, label, margins, style};
use gtk::prelude::*;
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use suite_core::pyjson::casefold;
use suite_core::tr;

pub const SERVICE_PREFIX: &str = "org.freedesktop.NetworkManager.";

type Field = (&'static str, String, Option<Vec<(&'static str, String)>>);

/// Installed plugins' public VPN data names (unlisted options stay in the
/// advanced editor and are never normalized).
pub fn protocol_fields(protocol: &str) -> Vec<Field> {
    let plain = |k: &'static str, t: String| (k, t, None);
    match protocol {
        "openvpn" => vec![plain("remote", tr!("Gateway")), plain("port", tr!("Port")),
            ("connection-type", tr!("Authentication"), Some(vec![("password", tr!("Password")), ("tls", tr!("Certificates")),
                ("password-tls", tr!("Password and certificates")), ("static-key", tr!("Static key"))])),
            plain("username", tr!("Username")), plain("ca", "CA certificate".into()), plain("cert", tr!("Client certificate")),
            plain("key", tr!("Private key")), plain("static-key", tr!("Static key file"))],
        "openconnect" => vec![plain("gateway", tr!("Gateway")), plain("protocol", tr!("Protocol")), plain("cacert", "CA certificate".into()),
            plain("usercert", tr!("Client certificate")), plain("userkey", tr!("Private key"))],
        "vpnc" => vec![plain("IPSec gateway", tr!("Gateway")), plain("IPSec ID", tr!("Group name")),
            plain("Xauth username", tr!("Username")), plain("Domain", tr!("Domain"))],
        "pptp" => vec![plain("gateway", tr!("Gateway")), plain("user", tr!("Username")), plain("domain", tr!("Domain"))],
        "l2tp" => vec![plain("gateway", tr!("Gateway")), plain("user", tr!("Username")), plain("domain", tr!("Domain")),
            ("ipsec-enabled", tr!("Use IPsec"), Some(vec![("no", "No".into()), ("yes", tr!("Yes"))]))],
        "libreswan" => vec![plain("right", tr!("Gateway")), plain("leftid", tr!("Group or local identity")),
            plain("leftxauthusername", tr!("Username")), plain("rightid", tr!("Remote identity"))],
        _ => Vec::new(),
    }
}

pub fn protocol_secrets(protocol: &str) -> Vec<(&'static str, String)> {
    match protocol {
        "openvpn" => vec![("password", tr!("Password")), ("cert-pass", tr!("Private key password"))],
        "openconnect" => vec![("key_pass", tr!("Private key password"))],
        "vpnc" => vec![("Xauth password", tr!("Password")), ("IPSec secret", tr!("Group password"))],
        "pptp" => vec![("password", tr!("Password"))],
        "l2tp" => vec![("password", tr!("Password")), ("ipsec-psk", tr!("IPsec pre-shared key"))],
        "libreswan" => vec![("xauthpassword", tr!("Password")), ("pskvalue", tr!("Pre-shared key"))],
        _ => Vec::new(),
    }
}

pub fn protocol_name(protocol: &str) -> Option<String> {
    Some(match protocol {
        "openvpn" => "OpenVPN".into(), "openconnect" => "OpenConnect".into(), "vpnc" => tr!("Cisco compatible VPN"),
        "pptp" => "PPTP".into(), "l2tp" => "L2TP".into(), "libreswan" => tr!("IPsec (Libreswan)"),
        _ => return None,
    })
}

/// Import/export of one VPN file format (libnm editor plugin; fakes in tests).
pub trait FormatPlugin {
    fn capabilities(&self) -> u32;
    fn property(&self, name: &str) -> String;
    fn import(&self, path: &str) -> Result<Connection, String>;
    fn export(&self, path: &str, connection: &Connection) -> Result<bool, String>;
}

impl FormatPlugin for EditorPlugin {
    fn capabilities(&self) -> u32 {
        EditorPlugin::capabilities(self)
    }
    fn property(&self, name: &str) -> String {
        EditorPlugin::property(self, name)
    }
    fn import(&self, path: &str) -> Result<Connection, String> {
        EditorPlugin::import(self, path).map_err(|e| e.message().to_owned())
    }
    fn export(&self, path: &str, connection: &Connection) -> Result<bool, String> {
        EditorPlugin::export(self, path, connection).map_err(|e| e.message().to_owned())
    }
}

/// Plugin metadata (`.name` file) and its optional format plugin.
pub struct PluginSource {
    pub service: String,
    pub name: String,
    pub load: Box<dyn Fn() -> Result<Rc<dyn FormatPlugin>, String>>,
}

impl PluginSource {
    pub fn from_info(info: VpnPluginInfo) -> Self {
        let (service, name) = (info.service(), info.name());
        Self { service, name, load: Box::new(move || info.load_editor_plugin().map(|p| Rc::new(p) as Rc<dyn FormatPlugin>)
                                                      .map_err(|e| e.message().to_owned())) }
    }
}

#[derive(Clone)]
pub struct PluginInfo {
    pub service: String,
    pub name: String,
    pub description: String,
    pub can_import: bool,
    pub can_export: bool,
    plugin: Option<Rc<dyn FormatPlugin>>,
}

/// Discover installed services; use their plugins only for file formats.
pub struct VpnPluginCatalog {
    pub plugins: Vec<PluginInfo>,
}

impl VpnPluginCatalog {
    pub fn installed() -> Self {
        Self::new(VpnPluginInfo::list_load().into_iter().map(PluginSource::from_info).collect())
    }

    pub fn new(infos: Vec<PluginSource>) -> Self {
        let mut plugins: Vec<PluginInfo> = Vec::new();
        for info in infos {
            if info.service.is_empty() || plugins.iter().any(|p| p.service == info.service) {
                continue;
            }
            let short = if info.name.is_empty() { info.service.rsplit('.').next().unwrap_or("").to_owned() } else { info.name.clone() };
            let mut name = protocol_name(&short).unwrap_or_else(|| short.clone());
            let (mut capabilities, mut description, mut plugin) = (0, String::new(), None);
            // The service stays configurable with our own form when its
            // optional import/export library is missing or rejected.
            if let Ok(loaded) = (info.load)() {
                capabilities = loaded.capabilities();
                let plugin_name = loaded.property("name");
                if !plugin_name.is_empty() {
                    name = plugin_name;
                }
                description = loaded.property("description");
                plugin = Some(loaded);
            }
            plugins.push(PluginInfo { service: info.service, name, description, can_import: capabilities & libnm::VPN_IMPORT != 0,
                                      can_export: capabilities & libnm::VPN_EXPORT != 0, plugin });
        }
        plugins.sort_by(|a, b| (casefold(&a.name), a.service.clone()).cmp(&(casefold(&b.name), b.service.clone())));
        Self { plugins }
    }

    pub fn find(&self, service: &str) -> Option<&PluginInfo> {
        self.plugins.iter().find(|p| p.service == service)
    }

    /// Import a user-selected file into memory without saving/activating.
    pub fn import_connection(&self, path: &str, service: Option<&str>) -> Result<Connection, String> {
        for item in self.plugins.iter().filter(|p| p.can_import && service.is_none_or(|s| s == p.service)) {
            let Some(plugin) = &item.plugin else { continue };
            if let Ok(connection) = plugin.import(path) {
                if connection.setting("vpn").is_some_and(|s| s.vpn_service_type() == item.service) {
                    return Ok(connection);
                }
            }
        }
        Err("No installed VPN plugin could import this file.".into())
    }

    /// Export without password values into a private file, atomically.
    pub fn export_connection(&self, path: &Path, connection: &Connection, overwrite: bool) -> Result<PathBuf, String> {
        let setting = vpn_setting(connection)?;
        let item = self.find(&setting.vpn_service_type()).filter(|i| i.can_export)
            .ok_or_else(|| tr!("This VPN type does not support exporting."))?;
        let plugin = item.plugin.clone().ok_or_else(|| tr!("This VPN type does not support exporting."))?;
        let failed = || tr!("The VPN configuration could not be exported.");
        let copy = connection.duplicate();
        copy.clear_secrets();
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let temporary = mkstemp(parent, ".vpn-export-").map_err(|_| failed())?;
        let result = (|| {
            if !plugin.export(&temporary.to_string_lossy(), &copy).map_err(|_| failed())? {
                return Err(failed());
            }
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).map_err(|_| failed())?;
            if overwrite {
                std::fs::rename(&temporary, path).map_err(|_| failed())?;
            } else {
                std::fs::hard_link(&temporary, path).map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists {
                    "A file already exists at the chosen location.".to_owned()
                } else { failed() })?;
            }
            Ok(path.to_path_buf())
        })();
        let _ = std::fs::remove_file(&temporary);
        result
    }
}

/// `tempfile.mkstemp(prefix, dir)`: an exclusive new file, closed.
fn mkstemp(dir: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    for _ in 0..100 {
        let candidate = dir.join(format!("{prefix}{}", suite_core::secrets::token_urlsafe(6).replace(['-', '_'], "x")));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode_private().open(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
}

trait PrivateMode {
    fn mode_private(&mut self) -> &mut Self;
}

impl PrivateMode for std::fs::OpenOptions {
    fn mode_private(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, 0o600)
    }
}

pub fn vpn_setting(connection: &Connection) -> Result<libnm::Setting, String> {
    match connection.setting("vpn") {
        Some(setting) if !setting.vpn_service_type().is_empty() => Ok(setting),
        _ => Err(tr!("Choose a VPN service before editing its settings.")),
    }
}

/// Configuration options only; credential values are never read.
pub fn vpn_data(connection: &Connection) -> Result<Vec<(String, String)>, String> {
    let setting = vpn_setting(connection)?;
    Ok(setting.vpn_data_keys().into_iter().map(|k| { let v = setting.vpn_data_item(&k).unwrap_or_default(); (k, v) }).collect())
}

fn get<'a>(values: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Validate common edited fields without rewriting imported options.
pub fn validate_vpn_data(service: &str, values: &[(String, String)], initial: Option<&[(String, String)]>) -> Result<(), String> {
    let same = |a: &[(String, String)], b: &[(String, String)]| {
        let mut a = a.to_vec();
        let mut b = b.to_vec();
        a.sort();
        b.sort();
        a == b
    };
    if let Some(initial) = initial.filter(|i| !i.is_empty()) {
        if same(values, initial) {
            return Ok(());
        }
    }
    let protocol = service.rsplit('.').next().unwrap_or("");
    let gateway = match protocol {
        "openvpn" => Some("remote"), "openconnect" | "pptp" | "l2tp" => Some("gateway"), "vpnc" => Some("IPSec gateway"),
        "libreswan" => Some("right"), _ => None,
    };
    if let Some(gateway) = gateway {
        if get(values, gateway).unwrap_or("").trim().is_empty() {
            return Err(tr!("Enter the VPN gateway."));
        }
    }
    let initial = initial.unwrap_or(&[]);
    let port = get(values, "port").unwrap_or("");
    if !port.is_empty() && Some(port) != get(initial, "port") {
        let valid = port.chars().all(|c| c.is_ascii_digit()) && port.parse::<u32>().is_ok_and(|p| (1..=65535).contains(&p));
        if !valid {
            return Err(tr!("The VPN port must be a number from 1 to 65535."));
        }
    }
    let auth = get(values, "connection-type").unwrap_or("");
    if protocol == "openvpn" && (initial.is_empty() || auth != get(initial, "connection-type").unwrap_or(""))
        && !["password", "tls", "password-tls", "static-key"].contains(&auth) {
        return Err(tr!("Choose an OpenVPN authentication method."));
    }
    Ok(())
}

/// Validate first, then replace VPN data while preserving untouched secrets.
/// `secret_changes` maps a key to a new value or `None` (explicit removal);
/// new secrets are agent-owned, existing storage flags are kept.
pub fn apply_vpn_settings(connection: &Connection, rows: &[(String, String)], secret_changes: &[(String, Option<String>)])
                          -> Result<(), String> {
    let setting = vpn_setting(connection)?;
    let mut values: Vec<(String, String)> = Vec::new();
    for (key, value) in rows {
        if key.is_empty() || key.contains('\0') {
            return Err(tr!("Every VPN option needs a valid name."));
        }
        if value.contains('\0') {
            return Err("A VPN option contains an invalid value.".into());
        }
        if values.iter().any(|(k, _)| k == key) {
            return Err("VPN option names must be unique.".into());
        }
        values.push((key.clone(), value.clone()));
    }
    for (key, value) in secret_changes {
        if key.is_empty() || key.contains('\0') {
            return Err(tr!("Every VPN password needs a valid name."));
        }
        if value.as_ref().is_some_and(|v| v.contains('\0')) {
            return Err("A VPN password contains an invalid value.".into());
        }
    }
    // A cloned setting: failed input never partially changes a draft.
    let replacement = setting.duplicate();
    let existing = setting.vpn_secret_keys();
    for key in replacement.vpn_data_keys() {
        replacement.vpn_remove_data_item(&key);
    }
    for (key, value) in &values {
        replacement.vpn_add_data_item(key, value);
    }
    for (key, value) in secret_changes {
        match value {
            None => replacement.vpn_remove_secret(key),
            Some(value) => {
                replacement.vpn_add_secret(key, value);
                if get(&values, &format!("{key}-flags")).is_none() && !existing.contains(key) {
                    replacement.set_secret_flags(key, libnm::AGENT_OWNED).map_err(|e| e.message().to_owned())?;
                }
            }
        }
    }
    connection.add_setting(&replacement);
    Ok(())
}

pub enum VpnControl {
    #[allow(deprecated)]
    Choice(gtk::ComboBoxText),
    Entry(gtk::Entry),
}

impl VpnControl {
    #[allow(deprecated)]
    pub fn value(&self) -> String {
        match self {
            VpnControl::Choice(c) => c.active_id().map(|s| s.to_string()).unwrap_or_default(),
            VpnControl::Entry(e) => e.text().to_string(),
        }
    }
}

/// Our GTK controls for a VPN draft; never a plugin widget.
pub struct VpnSettingsForm {
    pub root: gtk::Box,
    connection: Connection,
    pub service: String,
    pub protocol: String,
    initial: Vec<(String, String)>,
    pub fields: Vec<(String, VpnControl)>,
    pub secret_fields: Vec<(String, gtk::Entry)>,
    pub data_rows: RefCell<Vec<(gtk::Box, gtk::Entry, gtk::Entry)>>,
    rows_box: gtk::Box,
    on_changed: Option<Rc<dyn Fn()>>,
    weak: RefCell<Weak<VpnSettingsForm>>,
}

impl VpnSettingsForm {
    #[allow(deprecated)]
    pub fn new(connection: &Connection, on_changed: Option<Rc<dyn Fn()>>) -> Result<Rc<Self>, String> {
        let setting = vpn_setting(connection)?;
        let service = setting.vpn_service_type();
        let protocol = service.rsplit('.').next().unwrap_or("").to_owned();
        let initial = vpn_data(connection)?;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
        root.append(&label(&protocol_name(&protocol).unwrap_or_else(|| "VPN settings".into()), Some("section-title")));
        let grid = gtk::Grid::builder().row_spacing(10).column_spacing(14).build();
        root.append(&grid);
        let changed = on_changed.clone();
        let notify = move || if let Some(c) = &changed { c() };
        let notify = Rc::new(notify);
        let mut fields = Vec::new();
        for (index, (key, title, choices)) in protocol_fields(&protocol).into_iter().enumerate() {
            let mut value = get(&initial, key).unwrap_or("").to_owned();
            if protocol == "openvpn" && key == "connection-type" && initial.is_empty() {
                value = "password".into();
            }
            let control = if let Some(choices) = choices {
                let w = gtk::ComboBoxText::new();
                w.append(Some(""), &tr!("Default"));
                let mut ids = vec![String::new()];
                for (ident, display) in choices {
                    w.append(Some(ident), &display);
                    ids.push(ident.into());
                }
                if !ids.contains(&value) {
                    w.append(Some(&value), &value);
                }
                w.set_active_id(Some(&value));
                let n = notify.clone();
                w.connect_changed(move |_| n());
                w.set_hexpand(true);
                grid.attach(&label(&title, None), 0, index as i32, 1, 1);
                grid.attach(&w, 1, index as i32, 1, 1);
                VpnControl::Choice(w)
            } else {
                let w = gtk::Entry::new();
                w.set_text(&value);
                let n = notify.clone();
                w.connect_changed(move |_| n());
                w.set_hexpand(true);
                grid.attach(&label(&title, None), 0, index as i32, 1, 1);
                grid.attach(&w, 1, index as i32, 1, 1);
                VpnControl::Entry(w)
            };
            fields.push((key.to_owned(), control));
        }
        let passwords = gtk::Expander::new(Some(&tr!("Passwords")));
        let password_grid = gtk::Grid::builder().row_spacing(10).column_spacing(12).build();
        margins(&password_grid, 8);
        passwords.set_child(Some(&password_grid));
        let mut specs: Vec<(String, String)> = protocol_secrets(&protocol).into_iter().map(|(k, t)| (k.to_owned(), t)).collect();
        for key in setting.vpn_secret_keys() {
            if !specs.iter().any(|(k, _)| *k == key) {
                specs.push((key.clone(), key));
            }
        }
        let mut secret_fields = Vec::new();
        for (index, (key, title)) in specs.iter().enumerate() {
            let e = gtk::Entry::new();
            e.set_visibility(false);
            e.set_input_purpose(gtk::InputPurpose::Password);
            e.set_placeholder_text(Some(&tr!("Leave unchanged")));
            e.set_hexpand(true);
            let n = notify.clone();
            e.connect_changed(move |_| n());
            password_grid.attach(&label(title, None), 0, index as i32, 1, 1);
            password_grid.attach(&e, 1, index as i32, 1, 1);
            secret_fields.push((key.clone(), e));
        }
        if !specs.is_empty() {
            root.append(&passwords);
        }
        let advanced = gtk::Expander::new(Some(&tr!("Advanced options")));
        let boxed = gtk::Box::new(gtk::Orientation::Vertical, 8);
        margins(&boxed, 8);
        advanced.set_child(Some(&boxed));
        let note = label(&tr!("Additional options for this VPN protocol. Existing values are kept exactly."), Some("muted"));
        note.set_wrap(true);
        boxed.append(&note);
        let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 7);
        boxed.append(&rows_box);
        let add = gtk::Button::with_label(&tr!("Add option"));
        boxed.append(&add);
        root.append(&advanced);
        let form = Rc::new(Self { root, connection: connection.clone(), service, protocol, initial: initial.clone(), fields, secret_fields,
                                  data_rows: RefCell::default(), rows_box, on_changed, weak: RefCell::new(Weak::new()) });
        form.weak.replace(Rc::downgrade(&form));
        for (key, value) in &initial {
            if !form.fields.iter().any(|(k, _)| k == key) {
                form.add_option(key, value);
            }
        }
        let w = Rc::downgrade(&form);
        add.connect_clicked(move |_| if let Some(f) = w.upgrade() { f.add_option("", ""); });
        Ok(form)
    }

    fn changed(&self) {
        if let Some(c) = &self.on_changed {
            c();
        }
    }

    pub fn add_option(&self, key: &str, value: &str) -> (gtk::Entry, gtk::Entry) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let (key_entry, value_entry) = (gtk::Entry::new(), gtk::Entry::new());
        key_entry.set_placeholder_text(Some(&tr!("Option")));
        value_entry.set_placeholder_text(Some(&tr!("Value")));
        key_entry.set_text(key);
        value_entry.set_text(value);
        value_entry.set_hexpand(true);
        row.append(&key_entry);
        row.append(&expand(&value_entry, &row));
        let remove = gtk::Button::from_icon_name("list-remove-symbolic");
        remove.set_tooltip_text(Some(&tr!("Remove option")));
        row.append(&style(&remove, &["icon-button"]));
        self.data_rows.borrow_mut().push((row.clone(), key_entry.clone(), value_entry.clone()));
        let (w, r) = (self.weak.borrow().clone(), row.clone());
        remove.connect_clicked(move |_| if let Some(f) = w.upgrade() {
            f.data_rows.borrow_mut().retain(|(x, _, _)| *x != r);
            f.rows_box.remove(&r);
            f.changed();
        });
        for e in [&key_entry, &value_entry] {
            let w = self.weak.borrow().clone();
            e.connect_changed(move |_| if let Some(f) = w.upgrade() { f.changed() });
        }
        self.rows_box.append(&row);
        row.set_visible(true);
        (key_entry, value_entry)
    }

    pub fn apply_to(&self, target: &Connection) -> Result<(), String> {
        if vpn_setting(target)?.vpn_service_type() != self.service {
            return Err(tr!("The VPN service changed. Reopen its settings."));
        }
        let mut rows: Vec<(String, String)> = Vec::new();
        for (key, control) in &self.fields {
            let value = control.value();
            if !value.is_empty() || get(&self.initial, key) == Some(value.as_str()) {
                rows.push((key.clone(), value));
            }
        }
        for (_, key, value) in self.data_rows.borrow().iter() {
            // An unused new row is harmless; a value without a name is invalid.
            if !key.text().is_empty() || !value.text().is_empty() {
                rows.push((key.text().to_string(), value.text().to_string()));
            }
        }
        let secrets: Vec<(String, Option<String>)> = self.secret_fields.iter().filter(|(_, e)| !e.text().is_empty())
            .map(|(k, e)| (k.clone(), Some(e.text().to_string()))).collect();
        validate_vpn_data(&self.service, &rows, Some(&self.initial))?;
        apply_vpn_settings(target, &rows, &secrets)
    }
}

impl crate::ui::forms::Page for VpnSettingsForm {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn prepare(&self) -> crate::ui::forms::FormResult<Vec<crate::ui::forms::Operation>> {
        Ok(Vec::new())
    }
    fn apply(&self) -> Result<(), String> {
        self.apply_to(&self.connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libnm::Setting;
    use std::cell::Cell;

    fn connection(service: &str) -> Connection {
        let result = Connection::new().unwrap();
        let general = Setting::new("connection").unwrap();
        general.set_str("id", Some("VPN fixture"));
        general.set_str("uuid", Some("11111111-1111-4111-8111-111111111111"));
        general.set_str("type", Some("vpn"));
        result.add_setting(&general);
        let vpn = Setting::new("vpn").unwrap();
        vpn.set_str("service-type", Some(&format!("{SERVICE_PREFIX}{service}")));
        vpn.set_str("user-name", Some("fixture-user"));
        vpn.vpn_add_data_item("remote", "vpn.example.invalid");
        result.add_setting(&vpn);
        result
    }

    struct FakePlugin {
        service: String,
        caps: u32,
        imports: RefCell<Vec<String>>,
        exports: Cell<u32>,
    }

    impl FormatPlugin for FakePlugin {
        fn capabilities(&self) -> u32 { self.caps }
        fn property(&self, name: &str) -> String { if name == "name" { self.service.clone() } else { "Fixture format".into() } }
        fn import(&self, path: &str) -> Result<Connection, String> {
            self.imports.borrow_mut().push(path.into());
            if self.service == "pptp" {
                return Err("private plugin diagnostic must not escape".into());
            }
            Ok(connection(&self.service))
        }
        fn export(&self, path: &str, profile: &Connection) -> Result<bool, String> {
            self.exports.set(self.exports.get() + 1);
            assert!(profile.setting("vpn").unwrap().vpn_secret_keys().is_empty(), "exports must not receive password values");
            std::fs::write(path, "fixture configuration").unwrap();
            Ok(true)
        }
    }

    fn fake(service: &str, caps: u32) -> Rc<FakePlugin> {
        Rc::new(FakePlugin { service: service.into(), caps, imports: RefCell::default(), exports: Cell::new(0) })
    }

    fn source(plugin: Rc<FakePlugin>, error: bool) -> PluginSource {
        let p: Rc<dyn FormatPlugin> = plugin.clone();
        PluginSource { service: format!("{SERVICE_PREFIX}{}", plugin.service), name: plugin.service.clone(),
                       load: Box::new(move || if error { Err("Unusable system plugin".into()) } else { Ok(p.clone()) }) }
    }

    fn rows(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn common_validation_rejects_empty_gateway_bad_port_and_unknown_new_auth() {
        let service = format!("{SERVICE_PREFIX}openvpn");
        for values in [rows(&[]), rows(&[("remote", "vpn.example.invalid"), ("port", "70000")]),
                       rows(&[("remote", "vpn.example.invalid"), ("port", "not a port")]),
                       rows(&[("remote", "vpn.example.invalid"), ("connection-type", "unsupported")])] {
            assert!(validate_vpn_data(&service, &values, None).is_err(), "{values:?}");
        }
        validate_vpn_data(&service, &rows(&[("remote", "vpn.example.invalid"), ("port", "1194"), ("connection-type", "password")]), None).unwrap();
    }

    #[test]
    fn unchanged_imported_options_are_not_reinterpreted() {
        let values = rows(&[("remote", "imported.example.invalid"), ("connection-type", "future-mode"), ("port", "future-port")]);
        validate_vpn_data(&format!("{SERVICE_PREFIX}openvpn"), &values, Some(&values)).unwrap();
    }

    #[test]
    fn unknown_options_and_non_data_properties_round_trip_exactly() {
        let draft = connection("openvpn");
        let vpn = draft.setting("vpn").unwrap();
        vpn.vpn_add_data_item("Unknown Option ", " first line\nsecond=line\\value ");
        vpn.vpn_add_data_item("empty", "");
        let before = vpn_data(&draft).unwrap();
        apply_vpn_settings(&draft, &before, &[]).unwrap();
        assert_eq!(vpn_data(&draft).unwrap(), before);
        assert_eq!(draft.setting("vpn").unwrap().get_string("user-name").as_deref(), Some("fixture-user"));
        assert_eq!(draft.id(), "VPN fixture");
    }

    #[test]
    fn data_updates_preserve_secret_values_and_storage_flags() {
        let draft = connection("openvpn");
        let vpn = draft.setting("vpn").unwrap();
        vpn.vpn_add_secret("password", "fixture-only");
        vpn.set_secret_flags("password", libnm::NOT_SAVED).unwrap();
        let mut values = vpn_data(&draft).unwrap();
        for v in values.iter_mut() {
            if v.0 == "remote" {
                v.1 = "changed.example.invalid".into();
            }
        }
        apply_vpn_settings(&draft, &values, &[]).unwrap();
        let after = draft.setting("vpn").unwrap();
        assert_eq!(after.vpn_secret("password").as_deref(), Some("fixture-only"));
        assert_eq!(after.vpn_data_item("password-flags").as_deref(), Some("2"));
        assert_eq!(after.vpn_data_item("remote").as_deref(), Some("changed.example.invalid"));
    }

    #[test]
    fn explicit_secret_update_removal_and_new_agent_owned_value() {
        let draft = connection("openvpn");
        draft.setting("vpn").unwrap().vpn_add_secret("existing", "old fixture");
        draft.setting("vpn").unwrap().vpn_add_secret("remove", "old fixture");
        let data = vpn_data(&draft).unwrap();
        apply_vpn_settings(&draft, &data, &[("existing".into(), Some("changed fixture".into())), ("remove".into(), None),
                                            ("new".into(), Some("new fixture".into()))]).unwrap();
        let after = draft.setting("vpn").unwrap();
        assert_eq!(after.vpn_secret("existing").as_deref(), Some("changed fixture"));
        assert!(after.vpn_data_item("existing-flags").is_none());
        assert!(after.vpn_secret("remove").is_none());
        assert_eq!(after.vpn_data_item("new-flags").as_deref(), Some("1"));
    }

    #[test]
    fn invalid_or_duplicate_rows_leave_draft_unchanged() {
        let cases: Vec<(Vec<(String, String)>, Vec<(String, Option<String>)>)> = vec![
            (rows(&[("same", "a"), ("same", "b")]), vec![]), (rows(&[("", "value")]), vec![]),
            (rows(&[("key", "bad\0value")]), vec![]), (rows(&[("key", "value")]), vec![("secret".into(), Some("bad\0value".into()))])];
        for (r, s) in cases {
            let draft = connection("openvpn");
            let before = draft.to_dbus(libnm::ALL);
            assert!(apply_vpn_settings(&draft, &r, &s).is_err());
            assert_eq!(draft.to_dbus(libnm::ALL), before);
        }
    }

    #[test]
    fn catalog_deduplicates_services_and_does_not_require_native_editor_ui() {
        let plugin = fake("l2tp", 3 | 4);
        let catalog = VpnPluginCatalog::new(vec![source(plugin.clone(), false), source(plugin, false)]);
        assert_eq!(catalog.plugins.len(), 1);
        assert!(catalog.plugins[0].can_import && catalog.plugins[0].can_export);
    }

    #[test]
    fn missing_optional_plugin_keeps_service_available_for_custom_form() {
        let catalog = VpnPluginCatalog::new(vec![source(fake("openvpn", 3), true)]);
        assert_eq!(catalog.plugins[0].service, format!("{SERVICE_PREFIX}openvpn"));
        assert_eq!(catalog.plugins[0].name, "OpenVPN");
        assert!(!catalog.plugins[0].can_import && !catalog.plugins[0].can_export);
    }

    #[test]
    fn import_checks_plugin_service_and_sanitizes_errors() {
        let plugin = fake("openvpn", 3);
        let catalog = VpnPluginCatalog::new(vec![source(plugin.clone(), false)]);
        let draft = catalog.import_connection("fixture.ovpn", None).unwrap();
        assert_eq!(draft.setting("vpn").unwrap().vpn_service_type(), format!("{SERVICE_PREFIX}openvpn"));
        assert_eq!(*plugin.imports.borrow(), ["fixture.ovpn"]);
        let catalog = VpnPluginCatalog::new(vec![source(fake("pptp", 3), false)]);
        let error = catalog.import_connection("fixture.ovpn", None).unwrap_err();
        assert!(error.contains("No installed VPN plugin") && !error.contains("private"));
    }

    #[test]
    fn export_omits_secrets_and_uses_private_atomic_file() {
        let plugin = fake("openvpn", 3);
        let catalog = VpnPluginCatalog::new(vec![source(plugin.clone(), false)]);
        let draft = connection("openvpn");
        draft.setting("vpn").unwrap().vpn_add_secret("password", "fixture-only");
        let dir = std::env::temp_dir().join(format!("vpn-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vpn.conf");
        assert_eq!(catalog.export_connection(&path, &draft, false).unwrap(), path);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fixture configuration");
        assert!(catalog.export_connection(&path, &draft, false).unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "the temporary is always removed");
        catalog.export_connection(&path, &draft, true).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(draft.setting("vpn").unwrap().vpn_secret("password").as_deref(), Some("fixture-only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The installed OpenVPN plugin (read-only use): import a synthetic file
    /// into memory, then export it privately without secrets. Skips when absent.
    #[test]
    fn installed_openvpn_plugin_imports_and_exports_without_secrets() {
        let catalog = VpnPluginCatalog::installed();
        let Some(item) = catalog.find("org.freedesktop.NetworkManager.openvpn").filter(|p| p.can_import && p.can_export) else {
            eprintln!("openvpn plugin not installed: skipped");
            return;
        };
        assert!(!item.name.is_empty());
        let dir = std::env::temp_dir().join(format!("vpn-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("office.ovpn");
        std::fs::write(dir.join("ca.crt"), "-----BEGIN CERTIFICATE-----\nsynthetic\n-----END CERTIFICATE-----\n").unwrap();
        std::fs::write(&source, "client\ndev tun\nproto udp\nremote vpn.example.invalid 1194\nauth-user-pass\nca ca.crt\ncipher AES-256-GCM\n").unwrap();
        let connection = catalog.import_connection(&source.display().to_string(), None).unwrap();
        let setting = vpn_setting(&connection).unwrap();
        assert_eq!(setting.vpn_service_type(), "org.freedesktop.NetworkManager.openvpn");
        assert!(setting.vpn_data_item("remote").is_some_and(|r| r.contains("vpn.example.invalid")));
        setting.vpn_add_secret("password", "synthetic-never-exported");
        let target = dir.join("export.conf");
        catalog.export_connection(&target, &connection, false).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("vpn.example.invalid") && !text.contains("synthetic-never-exported"));
        assert!(catalog.export_connection(&target, &connection, false).is_err(), "no overwrite without asking");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "no temporary left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
