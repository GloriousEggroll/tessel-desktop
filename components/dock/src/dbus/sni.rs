//! status_tray.py backend: a shared StatusNotifier watcher + host for every
//! dock clone (the GTK widgets — StatusTray / TrayExpander — live in `ui`).
//!
//! The watcher object is always registered at `/StatusNotifierWatcher`; the
//! name is requested with `BusNameOwnerFlags::NONE` (queued: an existing panel
//! stays the watcher until it exits, it is never replaced). While another
//! process owns the watcher we register as its host (`RegisterStatusNotifierHost`)
//! and read `RegisteredStatusNotifierItems`; `host_registered` turns true only
//! after BOTH replies succeed for the current watcher generation. Items are keyed
//! `name + path`, retained across a watcher hand-off, and removed when their
//! owner vanishes.
//!
//! Public API (UI agent):
//! - `StatusTrayBackend::new(changed, connection)`, `.start()`, `.close()`,
//!   `.subscribe(cb) -> u64` / `.unsubscribe(id)` (clones share one backend);
//! - `.items() -> Vec<Rc<TrayItem>>` (registration order), `.item(key)`, `.ready()`,
//!   `.is_watcher()`, `.host_registered()`, `.watcher_owner()`, `.error()`, `.connection()`;
//! - `.action(&item, Action, failed)` → `Activate(ii)` / `SecondaryActivate(ii)` /
//!   `ContextMenu(ii)` (SCREEN coordinates) / `Scroll(is)`; `failed` runs if the
//!   call errors (StatusTray falls back to the DBusMenu then);
//! - `TrayItem`: `key()`, `name`, `path`, `ready()`, `icon_serial()`, `property(name)`,
//!   `string(name)`, `boolean(name)`, `properties()`, `tooltip_text()`, `pixmap(prefix, size)`,
//!   `menu_path()`, `is_passive()`, `needs_attention()`, `icon_signature(size, theme_serial)`
//!   (StatusTray._update_icon's cache key);
//! - pure: `parse_service`, `tooltip_text`, `pixmap_rgba`, `pixmaps_from_variant`.

use crate::dbus::readiness::TrayReadiness;
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
pub const ITEM: &str = "org.kde.StatusNotifierItem";
pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
pub const ICON_PROPERTIES: [&str; 7] = ["IconName", "IconPixmap", "AttentionIconName", "AttentionIconPixmap",
                                         "OverlayIconName", "OverlayIconPixmap", "IconThemePath"];
pub const ICON_SIGNALS: [&str; 3] = ["NewIcon", "NewAttentionIcon", "NewOverlayIcon"];
pub const WATCHER_XML: &str = r#"<node><interface name="org.kde.StatusNotifierWatcher">
<method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
<method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
<property name="RegisteredStatusNotifierItems" type="as" access="read"/>
<property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
<property name="ProtocolVersion" type="i" access="read"/>
<signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
<signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
<signal name="StatusNotifierHostRegistered"/>
<signal name="StatusNotifierHostUnregistered"/>
</interface></node>"#;

/// Accept both registration forms and canonical watcher item identifiers.
pub fn parse_service(service: &str, sender: Option<&str>) -> Result<(String, String), String> {
    if service.chars().count() > 1024 {
        return Err("Invalid status notifier service".into());
    }
    let (name, path) = if service.starts_with('/') {
        (sender.unwrap_or("").to_owned(), service.to_owned())
    } else if let Some((name, suffix)) = service.split_once('/') {
        (name.to_owned(), format!("/{suffix}"))
    } else {
        (service.to_owned(), "/StatusNotifierItem".to_owned())
    };
    if name.is_empty() || !gio::dbus_is_name(&name) || !Variant::is_object_path(&path) {
        return Err("Invalid status notifier address".into());
    }
    Ok((name, path))
}

fn html_body(body: &str) -> String {
    static BREAK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = BREAK.get_or_init(|| regex::Regex::new(r"(?i)<br\s*/?>|</p>").unwrap());
    let body = pattern.replace_all(body, "\n");
    let text = crate::status::html::unescape(&crate::status::html::strip_tags(&body));
    tessel_core::pyjson::py_strip(&text).to_owned()
}

/// Python `str(value)` of a property for display.
fn variant_text(value: &Variant) -> String {
    match value.str() {
        Some(text) => text.to_owned(),
        None => tessel_core::pyjson::py_str(&tessel_core::values::to_value(value)),
    }
}

fn variant_truthy(value: &Variant) -> bool { tessel_core::pyjson::truthy(&tessel_core::values::to_value(value)) }

/// Plain tooltip text: ToolTip `(sa(iiay)ss)` title + HTML-stripped body, else Title/Id/"Application".
pub fn tooltip_text(properties: &HashMap<String, Variant>) -> String {
    if let Some(value) = properties.get("ToolTip") {
        if value.is_container() && value.n_children() == 4 && !value.type_().is_array() {
            let title = variant_text(&value.child_value(2));
            let body = html_body(&variant_text(&value.child_value(3)));
            return [title, body].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n");
        }
    }
    for key in ["Title", "Id"] {
        if let Some(value) = properties.get(key).filter(|value| variant_truthy(value)) {
            return variant_text(value);
        }
    }
    "Application".into()
}

/// `(width, height, ARGB32 network-order bytes)` entries of an `a(iiay)` value.
pub fn pixmaps_from_variant(value: &Variant) -> Vec<(i64, i64, Vec<u8>)> {
    let mut result = Vec::new();
    if !value.is_container() {
        return result;
    }
    for entry in value.iter() {
        if !entry.is_container() || entry.n_children() != 3 || entry.type_().is_array() {
            continue;
        }
        let number = |v: Variant| -> Option<i64> { v.get::<i32>().map(i64::from).or_else(|| v.get::<i64>()) };
        let (Some(width), Some(height)) = (number(entry.child_value(0)), number(entry.child_value(1))) else { continue };
        let data = entry.child_value(2);
        let Some(bytes) = data.fixed_array::<u8>().ok().map(<[u8]>::to_vec) else { continue };
        result.push((width, height, bytes));
    }
    result
}

/// Select a bounded ARGB network-order pixmap (smallest ≥ size, dims ≤ 1024) and convert it to RGBA.
pub fn pixmap_rgba(pixmaps: &[(i64, i64, Vec<u8>)], size: i64) -> Option<(i64, i64, Vec<u8>)> {
    let valid: Vec<&(i64, i64, Vec<u8>)> = pixmaps.iter().filter(|(width, height, data)| {
        (1..=1024).contains(width) && (1..=1024).contains(height) && data.len() as i64 == width * height * 4
    }).collect();
    // min() keeps the first of equal keys.
    let best = valid.into_iter().min_by_key(|(width, height, _)| {
        let largest = (*width).max(*height);
        (largest < size, (largest - size).abs())
    })?;
    let (width, height, argb) = best;
    let mut rgba = vec![0u8; argb.len()];
    for (target, source) in rgba.chunks_exact_mut(4).zip(argb.chunks_exact(4)) {
        target.copy_from_slice(&[source[1], source[2], source[3], source[0]]);
    }
    Some((*width, *height, rgba))
}

/// `StatusTray._update_icon` cache key (icon size, theme serial, icon serial,
/// attention, the icon property values).
#[derive(Clone, Debug, PartialEq)]
pub struct IconSignature {
    pub size: i32,
    pub theme_serial: u64,
    pub icon_serial: u64,
    pub attention: bool,
    pub values: Vec<Option<Variant>>,
}

pub struct TrayItem {
    pub name: String,
    pub path: String,
    properties: RefCell<HashMap<String, Variant>>,
    proxy: RefCell<Option<gio::DBusProxy>>,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    refresh_pending: Cell<bool>,
    refresh_again: Cell<bool>,
    ready: Cell<bool>,
    icon_serial: Cell<u64>,
}

impl TrayItem {
    pub fn new(name: &str, path: &str) -> Self {
        Self {
            name: name.into(), path: path.into(), properties: RefCell::default(), proxy: RefCell::new(None),
            handlers: RefCell::default(), refresh_pending: Cell::new(false), refresh_again: Cell::new(false),
            ready: Cell::new(false), icon_serial: Cell::new(0),
        }
    }

    pub fn key(&self) -> String { format!("{}{}", self.name, self.path) }
    pub fn ready(&self) -> bool { self.ready.get() }
    pub fn icon_serial(&self) -> u64 { self.icon_serial.get() }
    pub fn properties(&self) -> HashMap<String, Variant> { self.properties.borrow().clone() }
    pub fn property(&self, name: &str) -> Option<Variant> { self.properties.borrow().get(name).cloned() }
    /// A string/object-path property (`None` for other types).
    pub fn string(&self, name: &str) -> Option<String> { self.property(name).and_then(|v| v.str().map(str::to_owned)) }
    pub fn boolean(&self, name: &str) -> bool { self.property(name).is_some_and(|v| variant_truthy(&v)) }
    pub fn tooltip_text(&self) -> String { tooltip_text(&self.properties.borrow()) }
    /// `Status` defaults to "Active".
    pub fn status(&self) -> String { self.string("Status").unwrap_or_else(|| "Active".into()) }
    pub fn is_passive(&self) -> bool { self.status() == "Passive" }
    pub fn needs_attention(&self) -> bool { self.string("Status").as_deref() == Some("NeedsAttention") }
    /// A usable DBusMenu path (`Menu` is an object path other than "/").
    pub fn menu_path(&self) -> Option<String> {
        self.string("Menu").filter(|path| path != "/" && Variant::is_object_path(path))
    }
    /// `pixmap_rgba(properties[prefix + "Pixmap"], size)` with prefix "Icon"/"AttentionIcon"/"OverlayIcon".
    pub fn pixmap(&self, prefix: &str, size: i64) -> Option<(i64, i64, Vec<u8>)> {
        let value = self.property(&format!("{prefix}Pixmap"))?;
        pixmap_rgba(&pixmaps_from_variant(&value), size)
    }
    pub fn icon_signature(&self, size: i32, theme_serial: u64) -> IconSignature {
        IconSignature {
            size, theme_serial, icon_serial: self.icon_serial.get(), attention: self.needs_attention(),
            values: ICON_PROPERTIES.iter().map(|name| self.property(name)).collect(),
        }
    }
    /// For tests / fake hosts.
    pub fn set_properties(&self, properties: HashMap<String, Variant>) { self.properties.replace(properties); }
    pub fn set_ready(&self, ready: bool) { self.ready.set(ready) }

    /// `_item_signal`: icon signals invalidate the cached file even with an unchanged IconName.
    pub fn note_signal(&self, signal: &str) {
        if ICON_SIGNALS.contains(&signal) {
            self.icon_serial.set(self.icon_serial.get() + 1);
        }
    }

    /// `_item_properties_changed`.
    pub fn note_properties_changed<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        if names.into_iter().any(|name| ICON_PROPERTIES.contains(&name)) {
            self.icon_serial.set(self.icon_serial.get() + 1);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Activate { x: i32, y: i32 },
    SecondaryActivate { x: i32, y: i32 },
    ContextMenu { x: i32, y: i32 },
    /// `delta` = ±120·steps, `orientation` "vertical" / "horizontal".
    Scroll { delta: i32, orientation: String },
}

struct Inner {
    items: RefCell<Vec<Rc<TrayItem>>>,
    error: RefCell<String>,
    ready: Cell<bool>,
    is_watcher: Cell<bool>,
    host_registered: Cell<bool>,
    connection: RefCell<Option<gio::DBusConnection>>,
    listeners: RefCell<Vec<(u64, Rc<dyn Fn()>)>>,
    next_listener: Cell<u64>,
    closed: Cell<bool>,
    started: Cell<bool>,
    cancellable: gio::Cancellable,
    owned_name: RefCell<Option<gio::OwnerId>>,
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
    unwatch: RefCell<Option<Box<dyn FnOnce()>>>,
    object: RefCell<Option<gio::RegistrationId>>,
    watcher_owner: RefCell<Option<String>>,
    watcher_generation: Cell<u64>,
    host_acknowledged: Cell<bool>,
    items_received: Cell<bool>,
}

#[derive(Clone)]
pub struct StatusTrayBackend(Rc<Inner>);

impl TrayReadiness for StatusTrayBackend {
    fn ready(&self) -> bool { self.0.ready.get() }
    fn host_registered(&self) -> bool { self.0.host_registered.get() }
}

impl StatusTrayBackend {
    pub fn new(changed: Option<Rc<dyn Fn()>>, connection: Option<gio::DBusConnection>) -> Self {
        let backend = Self(Rc::new(Inner {
            items: RefCell::default(), error: RefCell::default(), ready: Cell::new(false), is_watcher: Cell::new(false),
            host_registered: Cell::new(false), connection: RefCell::new(connection), listeners: RefCell::default(),
            next_listener: Cell::new(0), closed: Cell::new(false), started: Cell::new(false),
            cancellable: gio::Cancellable::new(), owned_name: RefCell::new(None), subscriptions: RefCell::default(),
            unwatch: RefCell::new(None), object: RefCell::new(None), watcher_owner: RefCell::new(None),
            watcher_generation: Cell::new(0), host_acknowledged: Cell::new(false), items_received: Cell::new(false),
        }));
        if let Some(changed) = changed {
            backend.subscribe(changed);
        }
        backend
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }
    fn upgrade(weak: &Weak<Inner>) -> Option<Self> { weak.upgrade().map(Self) }

    pub fn items(&self) -> Vec<Rc<TrayItem>> { self.0.items.borrow().clone() }
    pub fn item(&self, key: &str) -> Option<Rc<TrayItem>> { self.0.items.borrow().iter().find(|item| item.key() == key).cloned() }
    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn is_watcher(&self) -> bool { self.0.is_watcher.get() }
    pub fn host_registered(&self) -> bool { self.0.host_registered.get() }
    pub fn items_received(&self) -> bool { self.0.items_received.get() }
    pub fn watcher_owner(&self) -> Option<String> { self.0.watcher_owner.borrow().clone() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn connection(&self) -> Option<gio::DBusConnection> { self.0.connection.borrow().clone() }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }

    pub fn subscribe(&self, changed: Rc<dyn Fn()>) -> u64 {
        let id = self.0.next_listener.get() + 1;
        self.0.next_listener.set(id);
        self.0.listeners.borrow_mut().push((id, changed));
        id
    }

    pub fn unsubscribe(&self, id: u64) { self.0.listeners.borrow_mut().retain(|(known, _)| *known != id); }

    fn changed(&self) {
        if self.0.closed.get() {
            return;
        }
        let listeners: Vec<Rc<dyn Fn()>> = self.0.listeners.borrow().iter().map(|(_, cb)| cb.clone()).collect();
        for callback in listeners {
            callback();
        }
    }

    fn is_current(&self, item: &Rc<TrayItem>) -> bool {
        self.0.items.borrow().iter().any(|known| Rc::ptr_eq(known, item))
    }

    pub fn start(&self) {
        if self.0.started.get() {
            return;
        }
        self.0.started.set(true);
        if self.connection().is_some() {
            self.connect();
            return;
        }
        let weak = self.weak();
        gio::bus_get(gio::BusType::Session, Some(&self.0.cancellable), move |result| {
            let Some(this) = Self::upgrade(&weak) else { return };
            match result {
                Err(error) => {
                    if !this.0.closed.get() {
                        this.0.error.replace(tessel_core::pyerr::python_error_text(&error));
                        this.changed();
                    }
                }
                Ok(connection) => {
                    this.0.connection.replace(Some(connection));
                    if !this.0.closed.get() {
                        this.connect();
                    }
                }
            }
        });
    }

    fn connect(&self) {
        let connection = self.connection().expect("connection");
        let info = gio::DBusNodeInfo::for_xml(WATCHER_XML).expect("watcher xml").lookup_interface(WATCHER).expect("watcher interface");
        let (method, property) = (self.weak(), self.weak());
        let registration = connection.register_object(WATCHER_PATH, &info)
            .method_call(move |_, sender, _, _, method_name, params, invocation| {
                match Self::upgrade(&method) {
                    Some(this) => this.method(sender, method_name, &params, invocation),
                    None => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method_name),
                }
            })
            .property(move |_, _, _, _, name| {
                let this = Self::upgrade(&property);
                match name {
                    "RegisteredStatusNotifierItems" => {
                        let keys: Vec<String> = this.map(|t| t.items().iter().map(|i| i.key()).collect()).unwrap_or_default();
                        keys.to_variant()
                    }
                    "IsStatusNotifierHostRegistered" => true.to_variant(),
                    _ => 0i32.to_variant(),
                }
            })
            .build();
        if let Ok(id) = registration {
            self.0.object.replace(Some(id));
        }
        let weak = self.weak();
        let owner_changed = connection.subscribe_to_signal(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"), Some("/org/freedesktop/DBus"), None, gio::DBusSignalFlags::NONE, move |signal| {
                if let (Some(this), Some((name, previous, current))) = (Self::upgrade(&weak), signal.parameters.get::<(String, String, String)>()) {
                    this.owner_changed(&name, &previous, &current);
                }
            });
        let weak = self.weak();
        let watcher_signals = connection.subscribe_to_signal(None, Some(WATCHER), None, Some(WATCHER_PATH), None,
            gio::DBusSignalFlags::NONE, move |signal| {
                if let Some(this) = Self::upgrade(&weak) {
                    this.watcher_signal(signal.sender_name, signal.signal_name, signal.parameters);
                }
            });
        self.0.subscriptions.replace(vec![owner_changed, watcher_signals]);
        let (appeared, vanished) = (self.weak(), self.weak());
        let watch = gio::bus_watch_name_on_connection(&connection, WATCHER, gio::BusNameWatcherFlags::NONE,
            move |_, _, owner| { if let Some(this) = Self::upgrade(&appeared) { this.watcher_appeared(owner) } },
            move |_, _| { if let Some(this) = Self::upgrade(&vanished) { this.watcher_vanished() } });
        self.0.unwatch.replace(Some(Box::new(move || gio::bus_unwatch_name(watch))));
        // Queued, never replacing an existing watcher.
        let owner = gio::bus_own_name_on_connection(&connection, WATCHER, gio::BusNameOwnerFlags::NONE, |_, _| {}, |_, _| {});
        self.0.owned_name.replace(Some(owner));
        self.0.ready.set(true);
        self.changed();
    }

    fn method(&self, sender: Option<&str>, method: &str, params: &Variant, invocation: gio::DBusMethodInvocation) {
        if method != "RegisterStatusNotifierItem" && method != "RegisterStatusNotifierHost" {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
            return;
        }
        let service = params.try_child_value(0).and_then(|v| v.get::<String>());
        let parsed = match service {
            Some(service) => parse_service(&service, sender),
            None => Err("Invalid status notifier service".into()),
        };
        let (name, path) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", &error);
                return;
            }
        };
        if method == "RegisterStatusNotifierItem" {
            self.add(&format!("{name}{path}"));
        } else {
            self.emit("StatusNotifierHostRegistered", None);
        }
        invocation.return_value(Some(&().to_variant()));
    }

    fn emit(&self, signal: &str, value: Option<&str>) {
        if self.0.is_watcher.get() && !self.0.closed.get() {
            if let Some(connection) = self.connection() {
                let _ = connection.emit_signal(None, WATCHER_PATH, WATCHER, signal, value.map(|v| (v,).to_variant()).as_ref());
            }
        }
    }

    fn items_changed(&self) {
        if self.0.is_watcher.get() && !self.0.closed.get() {
            if let Some(connection) = self.connection() {
                let keys: Vec<String> = self.items().iter().map(|i| i.key()).collect();
                let changed = glib::VariantDict::new(None);
                changed.insert_value("RegisteredStatusNotifierItems", &keys.to_variant());
                let body = (WATCHER, changed.end(), Vec::<String>::new()).to_variant();
                let _ = connection.emit_signal(None, WATCHER_PATH, PROPERTIES, "PropertiesChanged", Some(&body));
            }
        }
        self.changed();
    }

    fn watcher_appeared(&self, owner: &str) {
        if self.0.closed.get() {
            return;
        }
        let connection = self.connection().expect("connection");
        self.0.watcher_owner.replace(Some(owner.to_owned()));
        self.0.watcher_generation.set(self.0.watcher_generation.get() + 1);
        let generation = self.0.watcher_generation.get();
        let own = connection.unique_name().map(|n| n.to_string()).unwrap_or_default();
        let is_watcher = owner == own;
        self.0.is_watcher.set(is_watcher);
        self.0.host_acknowledged.set(is_watcher);
        self.0.items_received.set(is_watcher);
        self.0.host_registered.set(is_watcher);
        if is_watcher {
            self.emit("StatusNotifierHostRegistered", None);
            self.items_changed();
            return;
        }
        let weak = self.weak();
        connection.call(Some(owner), WATCHER_PATH, WATCHER, "RegisterStatusNotifierHost", Some(&(own.as_str(),).to_variant()),
            None, gio::DBusCallFlags::NONE, 5000, Some(&self.0.cancellable), move |result| {
                if result.is_err() {
                    return;
                }
                let Some(this) = Self::upgrade(&weak) else { return };
                if !this.0.closed.get() && generation == this.0.watcher_generation.get() {
                    this.0.host_acknowledged.set(true);
                    this.0.host_registered.set(this.0.items_received.get());
                    this.changed();
                }
            });
        let weak = self.weak();
        connection.call(Some(owner), WATCHER_PATH, PROPERTIES, "Get", Some(&(WATCHER, "RegisteredStatusNotifierItems").to_variant()),
            Some(glib::VariantTy::new("(v)").unwrap()), gio::DBusCallFlags::NONE, 5000, Some(&self.0.cancellable), move |result| {
                let Ok(reply) = result else { return };
                let Some(this) = Self::upgrade(&weak) else { return };
                if this.0.closed.get() || generation != this.0.watcher_generation.get() {
                    return;
                }
                let services: Vec<String> = reply.child_value(0).as_variant().and_then(|v| v.get::<Vec<String>>()).unwrap_or_default();
                for service in services {
                    this.add(&service);
                }
                this.0.items_received.set(true);
                this.0.host_registered.set(this.0.host_acknowledged.get());
                this.changed();
            });
    }

    fn watcher_vanished(&self) {
        self.0.watcher_owner.replace(None);
        self.0.watcher_generation.set(self.0.watcher_generation.get() + 1);
        self.0.is_watcher.set(false);
        self.0.host_registered.set(false);
        self.0.host_acknowledged.set(false);
        self.0.items_received.set(false);
        // Keep items while our queued ownership is granted. Individual owners
        // are watched independently, so exited applications still disappear.
    }

    fn watcher_signal(&self, sender: &str, signal: &str, parameters: &Variant) {
        if self.0.closed.get() || self.0.is_watcher.get() || self.0.watcher_owner.borrow().as_deref() != Some(sender) {
            return;
        }
        let Some(service) = parameters.try_child_value(0).and_then(|v| v.get::<String>()) else { return };
        match signal {
            "StatusNotifierItemRegistered" => self.add(&service),
            "StatusNotifierItemUnregistered" => self.remove(&service, None),
            _ => {}
        }
    }

    fn owner_changed(&self, name: &str, previous: &str, current: &str) {
        if previous.is_empty() || !current.is_empty() {
            return;
        }
        for item in self.items() {
            let owner = item.proxy.borrow().as_ref().and_then(|p| p.name_owner()).map(|o| o.to_string());
            if item.name == name || owner.as_deref() == Some(name) {
                self.remove(&item.key(), None);
            }
        }
    }

    fn add(&self, service: &str) {
        let Ok((name, path)) = parse_service(service, None) else { return };
        let key = format!("{name}{path}");
        if self.0.closed.get() || self.item(&key).is_some() {
            return;
        }
        let item = Rc::new(TrayItem::new(&name, &path));
        self.0.items.borrow_mut().push(item.clone());
        self.emit("StatusNotifierItemRegistered", Some(&key));
        self.items_changed();
        let weak = self.weak();
        let connection = self.connection().expect("connection");
        gio::DBusProxy::new(&connection, gio::DBusProxyFlags::DO_NOT_AUTO_START, None, Some(&name), &path, ITEM,
                            Some(&self.0.cancellable), move |result| {
            let Some(this) = Self::upgrade(&weak) else { return };
            this.item_ready(result, item);
        });
    }

    fn item_ready(&self, result: Result<gio::DBusProxy, glib::Error>, item: Rc<TrayItem>) {
        let proxy = match result {
            Ok(proxy) => proxy,
            Err(_) => {
                self.remove(&item.key(), Some(&item));
                return;
            }
        };
        if self.0.closed.get() || !self.is_current(&item) {
            return;
        }
        item.proxy.replace(Some(proxy.clone()));
        if proxy.name_owner().is_none() {
            self.remove(&item.key(), Some(&item));
            return;
        }
        {
            let mut properties = item.properties.borrow_mut();
            for name in proxy.cached_property_names() {
                if let Some(value) = proxy.cached_property(&name) {
                    properties.insert(name.to_string(), value);
                }
            }
            item.ready.set(!properties.is_empty());
        }
        let (weak, signal_item) = (self.weak(), Rc::downgrade(&item));
        let signal = proxy.connect_local("g-signal", false, move |values| {
            let name: String = values.get(2).and_then(|v| v.get().ok())?;
            if let (Some(this), Some(item)) = (Self::upgrade(&weak), signal_item.upgrade()) {
                item.note_signal(&name);
                this.refresh_item(&item);
            }
            None
        });
        let (weak, props_item) = (self.weak(), Rc::downgrade(&item));
        let properties = proxy.connect_local("g-properties-changed", false, move |values| {
            let changed: Option<Variant> = values.get(1).and_then(|v| v.get().ok());
            let invalidated: Vec<String> = values.get(2).and_then(|v| v.get::<Vec<String>>().ok()).unwrap_or_default();
            if let (Some(this), Some(item)) = (Self::upgrade(&weak), props_item.upgrade()) {
                let mut names: Vec<String> = changed.map(|c| c.iter().filter_map(|e| e.child_value(0).get::<String>()).collect()).unwrap_or_default();
                names.extend(invalidated);
                item.note_properties_changed(names.iter().map(String::as_str));
                this.refresh_item(&item);
            }
            None
        });
        let (weak, owner_item) = (self.weak(), Rc::downgrade(&item));
        let owner = proxy.connect_notify_local(Some("g-name-owner"), move |proxy, _| {
            if let (Some(this), Some(item)) = (Self::upgrade(&weak), owner_item.upgrade()) {
                if proxy.name_owner().is_none() {
                    this.remove(&item.key(), Some(&item));
                }
            }
        });
        item.handlers.replace(vec![signal, properties, owner]);
        self.refresh_item(&item);
        self.changed();
    }

    /// Re-read all item properties (coalesced: one GetAll in flight per item).
    pub fn refresh_item(&self, item: &Rc<TrayItem>) {
        if self.0.closed.get() || !self.is_current(item) {
            return;
        }
        if item.refresh_pending.get() {
            item.refresh_again.set(true);
            return;
        }
        item.refresh_pending.set(true);
        let Some(connection) = self.connection() else { return };
        let (weak, pending) = (self.weak(), item.clone());
        connection.call(Some(&item.name), &item.path, PROPERTIES, "GetAll", Some(&(ITEM,).to_variant()),
            Some(glib::VariantTy::new("(a{sv})").unwrap()), gio::DBusCallFlags::NO_AUTO_START, 5000,
            Some(&self.0.cancellable), move |result| {
                pending.refresh_pending.set(false);
                // A disappearing or briefly busy app must not take down the dock.
                let Ok(reply) = result else { return };
                let Some(this) = Self::upgrade(&weak) else { return };
                if this.0.closed.get() || !this.is_current(&pending) {
                    return;
                }
                let mut properties = HashMap::new();
                for entry in reply.child_value(0).iter() {
                    if let (Some(key), Some(value)) = (entry.child_value(0).get::<String>(), entry.child_value(1).as_variant()) {
                        properties.insert(key, value);
                    }
                }
                pending.properties.replace(properties);
                pending.ready.set(true);
                this.changed();
                if pending.refresh_again.get() {
                    pending.refresh_again.set(false);
                    this.refresh_item(&pending);
                }
            });
    }

    fn remove(&self, key: &str, expected: Option<&Rc<TrayItem>>) {
        let item = {
            let mut items = self.0.items.borrow_mut();
            let Some(index) = items.iter().position(|item| item.key() == key) else { return };
            if expected.is_some_and(|expected| !Rc::ptr_eq(expected, &items[index])) {
                return;
            }
            items.remove(index)
        };
        if let Some(proxy) = item.proxy.borrow().as_ref() {
            for handler in item.handlers.take() {
                proxy.disconnect(handler);
            }
        }
        self.emit("StatusNotifierItemUnregistered", Some(key));
        self.items_changed();
    }

    /// Call an item method; false when the item is gone/not ready.
    pub fn action(&self, item: &Rc<TrayItem>, action: Action, failed: Option<Box<dyn FnOnce()>>) -> bool {
        if self.0.closed.get() || !self.is_current(item) || !item.ready.get() {
            return false;
        }
        let (method, parameters) = match action {
            Action::Scroll { delta, orientation } => ("Scroll", (delta, orientation).to_variant()),
            Action::Activate { x, y } => ("Activate", (x, y).to_variant()),
            Action::SecondaryActivate { x, y } => ("SecondaryActivate", (x, y).to_variant()),
            Action::ContextMenu { x, y } => ("ContextMenu", (x, y).to_variant()),
        };
        let Some(connection) = self.connection() else { return false };
        let (weak, target) = (self.weak(), item.clone());
        connection.call(Some(&item.name), &item.path, ITEM, method, Some(&parameters), None, gio::DBusCallFlags::NO_AUTO_START,
            5000, Some(&self.0.cancellable), move |result| {
                if result.is_ok() {
                    return;
                }
                if let (Some(failed), Some(this)) = (failed, Self::upgrade(&weak)) {
                    if !this.0.closed.get() && this.is_current(&target) {
                        failed();
                    }
                }
            });
        true
    }

    /// Python `action(item, name, ...)` by method name.
    pub fn action_named(&self, item: &Rc<TrayItem>, method: &str, x: i32, y: i32, delta: i32, orientation: &str,
                        failed: Option<Box<dyn FnOnce()>>) -> bool {
        let action = match method {
            "Scroll" => Action::Scroll { delta, orientation: orientation.into() },
            "Activate" => Action::Activate { x, y },
            "SecondaryActivate" => Action::SecondaryActivate { x, y },
            "ContextMenu" => Action::ContextMenu { x, y },
            _ => return false,
        };
        self.action(item, action, failed)
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.cancellable.cancel();
        if let Some(unwatch) = self.0.unwatch.take() {
            unwatch();
        }
        if let Some(owner) = self.0.owned_name.take() {
            gio::bus_unown_name(owner);
        }
        self.0.subscriptions.take();
        if let (Some(connection), Some(object)) = (self.connection(), self.0.object.take()) {
            let _ = connection.unregister_object(object);
        }
        for item in self.items() {
            self.remove(&item.key(), None);
        }
        self.0.listeners.borrow_mut().clear();
        self.0.ready.set(false);
        self.0.host_registered.set(false);
        self.0.is_watcher.set(false);
        self.0.watcher_owner.replace(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_forms_and_invalid_addresses() {
        assert_eq!(parse_service("org.example.App", None).unwrap(), ("org.example.App".into(), "/StatusNotifierItem".into()));
        assert_eq!(parse_service("/Item", Some(":1.7")).unwrap(), (":1.7".into(), "/Item".into()));
        assert_eq!(parse_service(":1.7/Item", None).unwrap(), (":1.7".into(), "/Item".into()));
        for value in ["", "invalid", "/no_sender", ":1.7/bad-path"] {
            assert_eq!(parse_service(value, None).unwrap_err(), "Invalid status notifier address", "{value}");
        }
        assert_eq!(parse_service(&"a".repeat(1025), None).unwrap_err(), "Invalid status notifier service");
    }

    #[test]
    fn network_order_alpha_and_best_pixmap_are_preserved() {
        let tiny = (1, 1, vec![128, 5, 7, 9]);
        let big = (2, 2, [255u8, 1, 2, 3].repeat(4));
        assert_eq!(pixmap_rgba(&[big.clone(), tiny.clone()], 1), Some((1, 1, vec![5, 7, 9, 128])));
        assert_eq!(pixmap_rgba(&[tiny.clone(), big.clone()], 2), Some((2, 2, [1u8, 2, 3, 255].repeat(4))));
        assert_eq!(pixmap_rgba(&[(5000, 1, vec![]), (1, 1, vec![2]), (0, 1, vec![])], 22), None);
        // Via the D-Bus a(iiay) value.
        let variant = vec![(1i32, 1i32, vec![128u8, 5, 7, 9])].to_variant();
        assert_eq!(pixmaps_from_variant(&variant), vec![(1, 1, vec![128, 5, 7, 9])]);
        // Smallest ≥ size wins over a closer smaller one.
        let s16 = (16, 16, vec![0; 16 * 16 * 4]);
        let s20 = (20, 20, vec![0; 20 * 20 * 4]);
        let s32 = (32, 32, vec![0; 32 * 32 * 4]);
        assert_eq!(pixmap_rgba(&[s16, s32.clone(), s20], 22).unwrap().0, 32);
    }

    #[test]
    fn tooltips_are_plain_readable_text() {
        let mut properties = HashMap::new();
        let tooltip = ("", Vec::<(i32, i32, Vec<u8>)>::new(), "Music", "<b>A &amp; B</b><br/>Playing").to_variant();
        properties.insert("ToolTip".to_owned(), tooltip);
        assert_eq!(tooltip_text(&properties), "Music\nA & B\nPlaying");
        let mut properties = HashMap::new();
        properties.insert("Title".to_owned(), "Idle inhibitor".to_variant());
        assert_eq!(tooltip_text(&properties), "Idle inhibitor");
        properties.insert("Title".to_owned(), "".to_variant());
        properties.insert("Id".to_owned(), "app-id".to_variant());
        assert_eq!(tooltip_text(&properties), "app-id");
        assert_eq!(tooltip_text(&HashMap::new()), "Application");
    }

    #[test]
    fn icon_signals_force_same_filename_reload_and_title_signals_do_not() {
        let item = TrayItem::new("org.example.Test", "/Item");
        item.note_signal("NewToolTip");
        assert_eq!(item.icon_serial(), 0);
        item.note_signal("NewIcon");
        assert_eq!(item.icon_serial(), 1);
        item.note_properties_changed(["Title"]);
        assert_eq!(item.icon_serial(), 1);
        item.note_properties_changed(["IconPixmap"]);
        assert_eq!(item.icon_serial(), 2);
    }

    #[test]
    fn icon_signature_ignores_metadata_but_tracks_icon_values() {
        let item = TrayItem::new("org.example.Test", "/Item");
        let mut properties = HashMap::new();
        properties.insert("IconName".to_owned(), "network-wired".to_variant());
        properties.insert("Title".to_owned(), "Title".to_variant());
        item.set_properties(properties.clone());
        let first = item.icon_signature(22, 0);
        properties.insert("Title".to_owned(), "Title 2".to_variant());
        item.set_properties(properties.clone());
        assert_eq!(item.icon_signature(22, 0), first);
        properties.insert("IconPixmap".to_owned(), vec![(1i32, 1i32, vec![255u8, 1, 2, 3])].to_variant());
        item.set_properties(properties.clone());
        let pixmap = item.icon_signature(22, 0);
        assert_ne!(pixmap, first);
        properties.insert("IconPixmap".to_owned(), vec![(1i32, 1i32, vec![255u8, 1, 2, 3])].to_variant());
        item.set_properties(properties.clone());
        assert_eq!(item.icon_signature(22, 0), pixmap);
        properties.insert("IconPixmap".to_owned(), vec![(1i32, 1i32, vec![255u8, 4, 5, 6])].to_variant());
        item.set_properties(properties.clone());
        assert_ne!(item.icon_signature(22, 0), pixmap);
        let current = item.icon_signature(22, 0);
        item.note_signal("NewIcon");
        assert_ne!(item.icon_signature(22, 0), current);
        assert_ne!(item.icon_signature(24, 0), item.icon_signature(22, 0));
        assert_ne!(item.icon_signature(22, 1), item.icon_signature(22, 0));
        properties.insert("Status".to_owned(), "NeedsAttention".to_variant());
        let before = item.icon_signature(22, 0);
        item.set_properties(properties);
        assert!(item.icon_signature(22, 0).attention && !before.attention);
    }
}
