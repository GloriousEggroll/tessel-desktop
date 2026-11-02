//! The dock's light/dark appearance and Papirus icons (dock_appearance.py).
//!
//! `AppearanceController` resolves `is_dark` from the settings portal
//! (`org.freedesktop.portal.Settings` `ReadAll(["org.freedesktop.appearance"])`
//! + `SettingChanged` for `color-scheme`: 1 dark, 2 light) and otherwise from
//! GTK (`gtk-application-prefer-dark-theme` or a theme name containing
//! "dark"). It overrides only process-local GTK settings (never the user's
//! desktop settings): the prefer-dark flag while the portal is explicit, and
//! the icon theme (`Papirus-Dark`/`Papirus-Light`, else `Papirus`). When the
//! user chose an installed non-Papirus theme in Global Settings (gsettings
//! `org.gnome.desktop.interface icon-theme`), the dock uses that one instead.
//! `on_changed(controller)` runs initially and whenever `(is_dark,
//! icon_theme_name)` changes. GTK objects and the portal are injectable.

use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

pub const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const PORTAL_INTERFACE: &str = "org.freedesktop.portal.Settings";
pub const APPEARANCE: &str = "org.freedesktop.appearance";
pub const PREFER_DARK: &str = "gtk-application-prefer-dark-theme";
pub const THEME_NAME: &str = "gtk-theme-name";
pub const ICON_THEME: &str = "gtk-icon-theme-name";

/// The parts of `Gtk.Settings` the controller uses.
pub trait SettingsBackend {
    fn prefer_dark(&self) -> bool;
    fn set_prefer_dark(&self, value: bool);
    fn string(&self, name: &str) -> Option<String>;
    fn set_string(&self, name: &str, value: &str);
    fn reset(&self, name: &str);
    fn connect_notify(&self, name: &str, callback: Box<dyn Fn()>) -> u64;
    fn disconnect(&self, handler: u64);
}

/// The parts of `Gtk.IconTheme` the controller uses.
pub trait IconThemeBackend {
    fn search_path(&self) -> Vec<PathBuf>;
    fn set_search_path(&self, paths: &[PathBuf]);
    fn connect_changed(&self, callback: Box<dyn Fn()>) -> u64;
    fn disconnect(&self, handler: u64);
}

/// The user's desktop icon theme choice (gsettings `icon-theme`), read
/// separately because the dock overrides its own GTK setting.
pub trait DesktopIconTheme {
    fn get(&self) -> Option<String>;
    fn connect_changed(&self, callback: Box<dyn Fn()>) -> u64;
    fn disconnect(&self, handler: u64);
}

/// `org.gnome.desktop.interface icon-theme`.
pub struct GioDesktopIconTheme {
    settings: gio::Settings,
    handlers: RefCell<HashMap<u64, glib::SignalHandlerId>>,
    next: Cell<u64>,
}

impl GioDesktopIconTheme {
    pub fn new() -> Option<Self> {
        tessel_core::icon_theme::interface_settings()
            .map(|settings| Self { settings, handlers: RefCell::default(), next: Cell::new(0) })
    }
}

impl DesktopIconTheme for GioDesktopIconTheme {
    fn get(&self) -> Option<String> {
        Some(self.settings.string(tessel_core::icon_theme::GSETTINGS_KEY).to_string()).filter(|v| !v.is_empty())
    }

    fn connect_changed(&self, callback: Box<dyn Fn()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        let handler = self.settings.connect_changed(Some(tessel_core::icon_theme::GSETTINGS_KEY), move |_, _| callback());
        self.handlers.borrow_mut().insert(id, handler);
        id
    }

    fn disconnect(&self, handler: u64) {
        if let Some(handler) = self.handlers.borrow_mut().remove(&handler) {
            self.settings.disconnect(handler);
        }
    }
}

/// `Gtk.Settings.get_default()`.
pub struct GtkSettings {
    settings: gtk::Settings,
    handlers: RefCell<HashMap<u64, glib::SignalHandlerId>>,
    next: Cell<u64>,
}

impl GtkSettings {
    pub fn default_settings() -> Option<Self> {
        gtk::Settings::default().map(|settings| Self { settings, handlers: RefCell::default(), next: Cell::new(0) })
    }
}

impl SettingsBackend for GtkSettings {
    fn prefer_dark(&self) -> bool {
        self.settings.property::<bool>(PREFER_DARK)
    }

    fn set_prefer_dark(&self, value: bool) {
        self.settings.set_property(PREFER_DARK, value);
    }

    fn string(&self, name: &str) -> Option<String> {
        self.settings.property::<Option<String>>(name)
    }

    fn set_string(&self, name: &str, value: &str) {
        self.settings.set_property(name, value);
    }

    fn reset(&self, name: &str) {
        self.settings.reset_property(name);
    }

    fn connect_notify(&self, name: &str, callback: Box<dyn Fn()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        let handler = self.settings.connect_notify_local(Some(name), move |_, _| callback());
        self.handlers.borrow_mut().insert(id, handler);
        id
    }

    fn disconnect(&self, handler: u64) {
        if let Some(handler) = self.handlers.borrow_mut().remove(&handler) {
            self.settings.disconnect(handler);
        }
    }
}

/// `Gtk.IconTheme.get_for_display(Gdk.Display.get_default())`.
pub struct GtkIconTheme {
    theme: gtk::IconTheme,
    handlers: RefCell<HashMap<u64, glib::SignalHandlerId>>,
    next: Cell<u64>,
}

impl GtkIconTheme {
    pub fn for_default_display() -> Option<Self> {
        let display = gtk::gdk::Display::default()?;
        Some(Self { theme: gtk::IconTheme::for_display(&display), handlers: RefCell::default(), next: Cell::new(0) })
    }
}

impl IconThemeBackend for GtkIconTheme {
    fn search_path(&self) -> Vec<PathBuf> {
        self.theme.search_path()
    }

    fn set_search_path(&self, paths: &[PathBuf]) {
        let paths: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
        self.theme.set_search_path(&paths);
    }

    fn connect_changed(&self, callback: Box<dyn Fn()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        let handler = self.theme.connect_changed(move |_| callback());
        self.handlers.borrow_mut().insert(id, handler);
        id
    }

    fn disconnect(&self, handler: u64) {
        if let Some(handler) = self.handlers.borrow_mut().remove(&handler) {
            self.theme.disconnect(handler);
        }
    }
}

/// Issues `ReadAll`; the reply comes back through
/// [`AppearanceController::portal_read`] with the same serial.
pub trait PortalReader {
    fn read_all(&self, controller: &AppearanceController, serial: u64);
}

/// Where the portal lives.
#[derive(Clone, Default)]
pub enum PortalSource {
    /// The session bus (production).
    #[default]
    Session,
    /// A specific connection (tests: a private bus).
    Connection(gio::DBusConnection),
    /// No portal (GTK fallback only).
    None,
}

/// Injectable GTK objects and portal (defaults: the real ones).
#[derive(Default)]
pub struct ControllerOptions {
    pub settings: Option<Rc<dyn SettingsBackend>>,
    pub icon_theme: Option<Rc<dyn IconThemeBackend>>,
    pub portal: PortalSource,
    /// The user's icon theme choice (default: gsettings, unless `no_gtk_defaults`).
    pub desktop_icon_theme: Option<Rc<dyn DesktopIconTheme>>,
    /// Use the injected objects even when they are `None` (no GTK lookups).
    pub no_gtk_defaults: bool,
}

struct ProxyReader(gio::DBusProxy, gio::Cancellable);

impl PortalReader for ProxyReader {
    fn read_all(&self, controller: &AppearanceController, serial: u64) {
        // ReadAll is supported by both versions of the Settings portal and
        // avoids the legacy Read method's doubly nested variant response.
        let weak = Rc::downgrade(&controller.0);
        let parameters = (vec![APPEARANCE.to_owned()],).to_variant();
        self.0.call("ReadAll", Some(&parameters), gio::DBusCallFlags::NONE, 3000, Some(&self.1), move |result| {
            if let Some(inner) = weak.upgrade() {
                AppearanceController(inner).portal_read(serial, result.ok().as_ref());
            }
        });
    }
}

struct Inner {
    on_changed: Option<Box<dyn Fn(&AppearanceController)>>,
    options: RefCell<ControllerOptions>,
    is_dark: Cell<bool>,
    icon_theme_name: RefCell<Option<String>>,
    settings: RefCell<Option<Rc<dyn SettingsBackend>>>,
    settings_signals: RefCell<Vec<u64>>,
    icon_theme: RefCell<Option<Rc<dyn IconThemeBackend>>>,
    icon_theme_signal: Cell<u64>,
    desktop_icon_theme: RefCell<Option<Rc<dyn DesktopIconTheme>>>,
    desktop_icon_signal: Cell<u64>,
    icon_overridden: Cell<bool>,
    proxy: RefCell<Option<gio::DBusProxy>>,
    proxy_signals: RefCell<Vec<glib::SignalHandlerId>>,
    reader: RefCell<Option<Rc<dyn PortalReader>>>,
    cancellable: gio::Cancellable,
    portal_scheme: Cell<i64>,
    read_serial: Cell<u64>,
    prefer_overridden: Cell<bool>,
    syncing: Cell<bool>,
    started: Cell<bool>,
    closed: Cell<bool>,
    reported: RefCell<Option<(bool, Option<String>)>>,
}

#[derive(Clone)]
pub struct AppearanceController(Rc<Inner>);

/// Unwrap nested variants; `type(value) is int` and in (1, 2), else 0.
pub fn scheme_value(value: &glib::Variant) -> i64 {
    let mut value = value.clone();
    while value.is_type(glib::VariantTy::VARIANT) {
        match value.as_variant() {
            Some(inner) => value = inner,
            None => break,
        }
    }
    let number = value.get::<u32>().map(i64::from).or_else(|| value.get::<i32>().map(i64::from))
        .or_else(|| value.get::<u8>().map(i64::from)).or_else(|| value.get::<i16>().map(i64::from))
        .or_else(|| value.get::<u16>().map(i64::from)).or_else(|| value.get::<i64>())
        .or_else(|| value.get::<u64>().and_then(|v| i64::try_from(v).ok()));
    match number {
        Some(n @ (1 | 2)) => n,
        _ => 0,
    }
}

impl AppearanceController {
    pub fn new(on_changed: Option<Box<dyn Fn(&AppearanceController)>>, options: ControllerOptions) -> Self {
        Self(Rc::new(Inner {
            on_changed, options: RefCell::new(options), is_dark: Cell::new(false), icon_theme_name: RefCell::new(None),
            settings: RefCell::new(None), settings_signals: RefCell::default(), icon_theme: RefCell::new(None),
            icon_theme_signal: Cell::new(0), desktop_icon_theme: RefCell::new(None), desktop_icon_signal: Cell::new(0),
            icon_overridden: Cell::new(false), proxy: RefCell::new(None),
            proxy_signals: RefCell::default(), reader: RefCell::new(None), cancellable: gio::Cancellable::new(),
            portal_scheme: Cell::new(0), read_serial: Cell::new(0), prefer_overridden: Cell::new(false),
            syncing: Cell::new(false), started: Cell::new(false), closed: Cell::new(false), reported: RefCell::new(None),
        }))
    }

    pub fn is_dark(&self) -> bool {
        self.0.is_dark.get()
    }

    pub fn icon_theme_name(&self) -> Option<String> {
        self.0.icon_theme_name.borrow().clone()
    }

    pub fn is_closed(&self) -> bool {
        self.0.closed.get()
    }

    pub fn icon_overridden(&self) -> bool {
        self.0.icon_overridden.get()
    }

    pub fn cancellable(&self) -> &gio::Cancellable {
        &self.0.cancellable
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub fn start(&self) {
        let inner = &self.0;
        if inner.started.get() || inner.closed.get() {
            return;
        }
        inner.started.set(true);
        let (settings, icon_theme, desktop_icon_theme, portal) = {
            let mut options = inner.options.borrow_mut();
            let gtk = !options.no_gtk_defaults;
            let settings = options.settings.take().or_else(|| if gtk {
                GtkSettings::default_settings().map(|s| Rc::new(s) as Rc<dyn SettingsBackend>) } else { None });
            let icon_theme = options.icon_theme.take().or_else(|| if gtk {
                GtkIconTheme::for_default_display().map(|t| Rc::new(t) as Rc<dyn IconThemeBackend>) } else { None });
            let desktop = options.desktop_icon_theme.take().or_else(|| if gtk {
                GioDesktopIconTheme::new().map(|d| Rc::new(d) as Rc<dyn DesktopIconTheme>) } else { None });
            (settings, icon_theme, desktop, options.portal.clone())
        };
        if let Some(desktop) = &desktop_icon_theme {
            let weak = self.weak();
            inner.desktop_icon_signal.set(desktop.connect_changed(Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    AppearanceController(inner).settings_changed();
                }
            })));
        }
        *inner.desktop_icon_theme.borrow_mut() = desktop_icon_theme;
        if let Some(settings) = &settings {
            for name in [THEME_NAME, PREFER_DARK, ICON_THEME] {
                let weak = self.weak();
                let id = settings.connect_notify(name, Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        AppearanceController(inner).settings_changed();
                    }
                }));
                inner.settings_signals.borrow_mut().push(id);
            }
        }
        *inner.settings.borrow_mut() = settings;
        if let Some(theme) = &icon_theme {
            let weak = self.weak();
            inner.icon_theme_signal.set(theme.connect_changed(Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    AppearanceController(inner).settings_changed();
                }
            })));
        }
        *inner.icon_theme.borrow_mut() = icon_theme;
        self.apply();
        let flags = gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES;
        let weak = self.weak();
        let ready = move |result: Result<gio::DBusProxy, glib::Error>| {
            if let (Some(inner), Ok(proxy)) = (weak.upgrade(), result) {
                AppearanceController(inner).proxy_ready(proxy);
            }
        };
        match portal {
            PortalSource::Session => gio::DBusProxy::for_bus(gio::BusType::Session, flags, None, PORTAL_NAME, PORTAL_PATH,
                                                             PORTAL_INTERFACE, Some(&inner.cancellable), ready),
            PortalSource::Connection(connection) => gio::DBusProxy::new(&connection, flags, None, Some(PORTAL_NAME), PORTAL_PATH,
                                                                        PORTAL_INTERFACE, Some(&inner.cancellable), ready),
            PortalSource::None => {}
        }
    }

    pub fn close(&self) {
        let inner = &self.0;
        inner.closed.set(true);
        inner.read_serial.set(inner.read_serial.get() + 1);
        inner.cancellable.cancel();
        let settings = inner.settings.borrow().clone();
        for handler in inner.settings_signals.borrow_mut().drain(..) {
            if let Some(settings) = &settings {
                settings.disconnect(handler);
            }
        }
        let theme = inner.icon_theme.borrow_mut().take();
        if inner.icon_theme_signal.get() != 0 {
            if let Some(theme) = &theme {
                theme.disconnect(inner.icon_theme_signal.get());
            }
        }
        inner.icon_theme_signal.set(0);
        let desktop = inner.desktop_icon_theme.borrow_mut().take();
        if let (Some(desktop), handler @ 1..) = (&desktop, inner.desktop_icon_signal.get()) {
            desktop.disconnect(handler);
        }
        inner.desktop_icon_signal.set(0);
        let proxy = inner.proxy.borrow_mut().take();
        for handler in inner.proxy_signals.borrow_mut().drain(..) {
            if let Some(proxy) = &proxy {
                proxy.disconnect(handler);
            }
        }
        *inner.reader.borrow_mut() = None;
    }

    pub fn settings_changed(&self) {
        if !self.0.syncing.get() && !self.0.closed.get() {
            self.apply();
        }
    }

    /// Invalidate changed icon files in place, keeping the theme: GTK's
    /// search-path setter flushes its icon cache even for identical paths.
    pub fn refresh_icons(&self) -> bool {
        let theme = self.0.icon_theme.borrow().clone();
        let Some(theme) = theme.filter(|_| !self.0.closed.get() && !self.0.syncing.get()) else { return false };
        self.0.syncing.set(true);
        theme.set_search_path(&theme.search_path());
        self.0.syncing.set(false);
        true
    }

    /// The user's installed non-Papirus choice, else the Papirus variant
    /// for `dark` (else `Papirus`), else `None` (no override).
    fn icon_theme_for(&self, dark: bool) -> Option<String> {
        let variant = if dark { "Papirus-Dark" } else { "Papirus-Light" };
        let theme = self.0.icon_theme.borrow().clone();
        let paths = theme.map(|t| t.search_path()).unwrap_or_default();
        let desktop = self.0.desktop_icon_theme.borrow().clone();
        if let Some(chosen) = desktop.and_then(|d| d.get()) {
            if !tessel_core::icon_theme::is_papirus_family(&chosen) && tessel_core::icon_theme::is_installed(&paths, &chosen) {
                return Some(chosen);
            }
        }
        if paths.iter().any(|p| p.join(variant).join("index.theme").is_file()) {
            return Some(variant.into());
        }
        if paths.iter().any(|p| p.join("Papirus").join("index.theme").is_file()) {
            return Some("Papirus".into());
        }
        None
    }

    fn apply(&self) {
        let inner = &self.0;
        if inner.closed.get() || inner.syncing.get() {
            return;
        }
        inner.syncing.set(true);
        let settings = inner.settings.borrow().clone();
        let dark = match &settings {
            Some(settings) => {
                let scheme = inner.portal_scheme.get();
                if scheme == 1 || scheme == 2 {
                    let dark = scheme == 1;
                    inner.prefer_overridden.set(true);
                    if settings.prefer_dark() != dark {
                        settings.set_prefer_dark(dark);
                    }
                    dark
                } else {
                    if inner.prefer_overridden.get() {
                        // Remove our override rather than restoring a stale
                        // value from before a desktop theme change.
                        inner.prefer_overridden.set(false);
                        settings.reset(PREFER_DARK);
                    }
                    settings.prefer_dark()
                        || tessel_core::pyjson::casefold(&settings.string(THEME_NAME).unwrap_or_default()).contains("dark")
                }
            }
            None => inner.portal_scheme.get() == 1,
        };
        let mut theme_name = self.icon_theme_for(dark);
        if let Some(settings) = &settings {
            match &theme_name {
                Some(name) => {
                    if settings.string(ICON_THEME).as_deref() != Some(name) {
                        inner.icon_overridden.set(true);
                        settings.set_string(ICON_THEME, name);
                    }
                }
                None => {
                    if inner.icon_overridden.get() {
                        // Release our process-local override so GTK resolves
                        // the current desktop setting, not an old saved value.
                        inner.icon_overridden.set(false);
                        settings.reset(ICON_THEME);
                    }
                    theme_name = settings.string(ICON_THEME);
                }
            }
        }
        inner.is_dark.set(dark);
        *inner.icon_theme_name.borrow_mut() = theme_name.clone();
        inner.syncing.set(false);
        let state = Some((dark, theme_name));
        if *inner.reported.borrow() != state {
            *inner.reported.borrow_mut() = state;
            if let Some(callback) = &inner.on_changed {
                callback(self);
            }
        }
    }

    fn proxy_ready(&self, proxy: gio::DBusProxy) {
        if self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        let signal = proxy.connect_local("g-signal", false, move |values| {
            let name = values.get(2).and_then(|v| v.get::<String>().ok());
            let parameters = values.get(3).and_then(|v| v.get::<glib::Variant>().ok());
            if let (Some(inner), Some(name), Some(parameters)) = (weak.upgrade(), name, parameters) {
                AppearanceController(inner).portal_signal(&name, &parameters);
            }
            None
        });
        let weak = self.weak();
        let owner = proxy.connect_local("notify::g-name-owner", false, move |values| {
            let proxy = values.first().and_then(|v| v.get::<gio::DBusProxy>().ok());
            if let (Some(inner), Some(proxy)) = (weak.upgrade(), proxy) {
                AppearanceController(inner).portal_owner_changed(proxy.name_owner().is_some());
            }
            None
        });
        *self.0.proxy_signals.borrow_mut() = vec![signal, owner];
        *self.0.reader.borrow_mut() = Some(Rc::new(ProxyReader(proxy.clone(), self.0.cancellable.clone())));
        *self.0.proxy.borrow_mut() = Some(proxy);
        self.read_portal();
    }

    /// `_portal_ready` with an injected reader (unit tests).
    pub fn portal_ready_with(&self, reader: Rc<dyn PortalReader>) {
        if self.0.closed.get() {
            return;
        }
        *self.0.reader.borrow_mut() = Some(reader);
        self.read_portal();
    }

    pub fn portal_owner_changed(&self, has_owner: bool) {
        let inner = &self.0;
        inner.read_serial.set(inner.read_serial.get() + 1);
        if inner.closed.get() {
            return;
        }
        inner.portal_scheme.set(0);
        self.apply();
        if has_owner {
            self.read_portal();
        }
    }

    pub fn read_portal(&self) {
        let inner = &self.0;
        inner.read_serial.set(inner.read_serial.get() + 1);
        let serial = inner.read_serial.get();
        let reader = inner.reader.borrow().clone();
        if let Some(reader) = reader {
            reader.read_all(self, serial);
        }
    }

    /// A `ReadAll` reply (`(a{sa{sv}})`), `None` on error.
    pub fn portal_read(&self, serial: u64, reply: Option<&glib::Variant>) {
        let Some(reply) = reply else { return };
        if !reply.is_type(glib::VariantTy::new("(a{sa{sv}})").unwrap()) {
            return;
        }
        if self.0.closed.get() || serial != self.0.read_serial.get() {
            return;
        }
        let namespaces = reply.child_value(0);
        let settings = namespaces.iter().filter(|entry| entry.child_value(0).str() == Some(APPEARANCE))
            .last().map(|entry| entry.child_value(1));
        let value = settings.and_then(|settings| glib::VariantDict::new(Some(&settings)).lookup_value("color-scheme", None));
        self.set_scheme(value.as_ref());
    }

    /// `g-signal` handler (`SettingChanged(ssv)`).
    pub fn portal_signal(&self, name: &str, parameters: &glib::Variant) {
        if self.0.closed.get() || name != "SettingChanged" || parameters.n_children() != 3 {
            return;
        }
        let namespace = parameters.child_value(0).str().map(str::to_owned);
        let key = parameters.child_value(1).str().map(str::to_owned);
        if namespace.as_deref() == Some(APPEARANCE) && key.as_deref() == Some("color-scheme") {
            // A late initial reply must not undo this.
            self.0.read_serial.set(self.0.read_serial.get() + 1);
            self.set_scheme(Some(&parameters.child_value(2)));
        }
    }

    fn set_scheme(&self, value: Option<&glib::Variant>) {
        self.0.portal_scheme.set(value.map(scheme_value).unwrap_or(0));
        self.apply();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSettings {
        desktop: RefCell<HashMap<String, Value>>,
        values: RefCell<HashMap<String, Value>>,
        signals: RefCell<Vec<(u64, String, Rc<dyn Fn()>)>>,
        writes: RefCell<Vec<(String, Value)>>,
        resets: RefCell<Vec<String>>,
        next: Cell<u64>,
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Value {
        Bool(bool),
        Str(String),
    }

    impl FakeSettings {
        fn new() -> Rc<Self> {
            let desktop: HashMap<String, Value> = [(THEME_NAME, Value::Str("Adwaita".into())), (PREFER_DARK, Value::Bool(false)),
                                                    (ICON_THEME, Value::Str("Breeze".into()))]
                .into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
            Rc::new(Self { values: RefCell::new(desktop.clone()), desktop: RefCell::new(desktop), signals: RefCell::default(),
                           writes: RefCell::default(), resets: RefCell::default(), next: Cell::new(0) })
        }

        fn set(&self, name: &str, value: Value) {
            self.writes.borrow_mut().push((name.into(), value.clone()));
            self.values.borrow_mut().insert(name.into(), value);
            let callbacks: Vec<Rc<dyn Fn()>> = self.signals.borrow().iter().filter(|s| s.1 == name).map(|s| s.2.clone()).collect();
            for callback in callbacks {
                callback();
            }
        }

        fn get(&self, name: &str) -> Value {
            self.values.borrow()[name].clone()
        }
    }

    impl SettingsBackend for FakeSettings {
        fn prefer_dark(&self) -> bool {
            self.get(PREFER_DARK) == Value::Bool(true)
        }
        fn set_prefer_dark(&self, value: bool) {
            self.set(PREFER_DARK, Value::Bool(value));
        }
        fn string(&self, name: &str) -> Option<String> {
            match self.get(name) { Value::Str(s) => Some(s), _ => None }
        }
        fn set_string(&self, name: &str, value: &str) {
            self.set(name, Value::Str(value.into()));
        }
        fn reset(&self, name: &str) {
            self.resets.borrow_mut().push(name.into());
            let value = self.desktop.borrow()[name].clone();
            self.set(name, value);
        }
        fn connect_notify(&self, name: &str, callback: Box<dyn Fn()>) -> u64 {
            let id = self.next.get() + 1;
            self.next.set(id);
            self.signals.borrow_mut().push((id, name.into(), Rc::from(callback)));
            id
        }
        fn disconnect(&self, handler: u64) {
            self.signals.borrow_mut().retain(|s| s.0 != handler);
        }
    }

    struct FakeIconTheme {
        paths: Vec<PathBuf>,
        changed: RefCell<Option<Rc<dyn Fn()>>>,
        set_calls: RefCell<Vec<Vec<PathBuf>>>,
        on_set: RefCell<Option<Box<dyn Fn()>>>,
    }

    impl IconThemeBackend for FakeIconTheme {
        fn search_path(&self) -> Vec<PathBuf> {
            self.paths.clone()
        }
        fn set_search_path(&self, paths: &[PathBuf]) {
            self.set_calls.borrow_mut().push(paths.to_vec());
            if let Some(hook) = &*self.on_set.borrow() {
                hook();
            }
        }
        fn connect_changed(&self, callback: Box<dyn Fn()>) -> u64 {
            *self.changed.borrow_mut() = Some(Rc::from(callback));
            1
        }
        fn disconnect(&self, _: u64) {
            *self.changed.borrow_mut() = None;
        }
    }

    #[derive(Default)]
    struct FakeDesktop {
        value: RefCell<Option<String>>,
        changed: RefCell<Option<Rc<dyn Fn()>>>,
    }

    impl FakeDesktop {
        fn choose(&self, theme: Option<&str>) {
            self.value.replace(theme.map(str::to_owned));
            let callback = self.changed.borrow().clone();
            if let Some(callback) = callback {
                callback();
            }
        }
    }

    impl DesktopIconTheme for FakeDesktop {
        fn get(&self) -> Option<String> {
            self.value.borrow().clone()
        }
        fn connect_changed(&self, callback: Box<dyn Fn()>) -> u64 {
            *self.changed.borrow_mut() = Some(Rc::from(callback));
            7
        }
        fn disconnect(&self, handler: u64) {
            assert_eq!(handler, 7);
            *self.changed.borrow_mut() = None;
        }
    }

    #[derive(Default)]
    struct FakeReader(RefCell<Vec<u64>>);
    impl PortalReader for FakeReader {
        fn read_all(&self, _: &AppearanceController, serial: u64) {
            self.0.borrow_mut().push(serial);
        }
    }

    struct Fixture {
        icons: PathBuf,
        settings: Rc<FakeSettings>,
        theme: Rc<FakeIconTheme>,
        desktop: Rc<FakeDesktop>,
        events: Rc<RefCell<Vec<(bool, Option<String>)>>>,
        controller: AppearanceController,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let icons = std::env::temp_dir().join(format!("d-appearance-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&icons);
            for theme in ["Papirus", "Papirus-Light", "Papirus-Dark"] {
                std::fs::create_dir_all(icons.join(theme)).unwrap();
                std::fs::write(icons.join(theme).join("index.theme"), "").unwrap();
            }
            let settings = FakeSettings::new();
            let theme = Rc::new(FakeIconTheme { paths: vec![icons.clone()], changed: RefCell::default(), set_calls: RefCell::default(),
                                                on_set: RefCell::default() });
            let events = Rc::new(RefCell::new(Vec::new()));
            let recorder = events.clone();
            let desktop = Rc::new(FakeDesktop::default());
            let controller = AppearanceController::new(
                Some(Box::new(move |c: &AppearanceController| recorder.borrow_mut().push((c.is_dark(), c.icon_theme_name())))),
                ControllerOptions { settings: Some(settings.clone()), icon_theme: Some(theme.clone()), portal: PortalSource::None,
                                    desktop_icon_theme: Some(desktop.clone()), no_gtk_defaults: true });
            Self { icons, settings, theme, desktop, events, controller }
        }

        fn signal_scheme(&self, value: u32) {
            let parameters = (APPEARANCE, "color-scheme", value.to_variant()).to_variant();
            self.controller.portal_signal("SettingChanged", &parameters);
        }

        fn portal_ready(&self) -> Rc<FakeReader> {
            let reader = Rc::new(FakeReader::default());
            self.controller.portal_ready_with(reader.clone());
            reader
        }

        fn last(&self) -> (bool, Option<String>) {
            self.events.borrow().last().cloned().unwrap()
        }

        fn remove_indexes(&self) {
            for theme in ["Papirus", "Papirus-Light", "Papirus-Dark"] {
                let _ = std::fs::remove_file(self.icons.join(theme).join("index.theme"));
            }
        }

        fn install(&self, theme: &str) {
            std::fs::create_dir_all(self.icons.join(theme)).unwrap();
            std::fs::write(self.icons.join(theme).join("index.theme"), "[Icon Theme]\nName=x\nDirectories=48x48/apps\n").unwrap();
        }

        fn theme_changed(&self) {
            let callback = self.theme.changed.borrow().clone().unwrap();
            callback();
        }
    }

    fn event(dark: bool, name: &str) -> (bool, Option<String>) {
        (dark, Some(name.into()))
    }

    fn reply(scheme: u32) -> glib::Variant {
        let inner: HashMap<String, glib::Variant> = [("color-scheme".to_owned(), scheme.to_variant())].into_iter().collect();
        let outer: HashMap<String, HashMap<String, glib::Variant>> = [(APPEARANCE.to_owned(), inner)].into_iter().collect();
        (outer,).to_variant()
    }

    #[test]
    fn default_papirus_does_not_modify_desktop_theme_or_dark_preference() {
        let f = Fixture::new("default");
        f.controller.start();
        assert_eq!(*f.events.borrow(), [event(false, "Papirus-Light")]);
        assert_eq!(*f.settings.writes.borrow(), [(ICON_THEME.to_owned(), Value::Str("Papirus-Light".into()))]);
        assert_eq!(f.settings.desktop.borrow()[ICON_THEME], Value::Str("Breeze".into()));
    }

    #[test]
    fn gtk_theme_and_preference_changes_work_without_portal() {
        let f = Fixture::new("gtk");
        f.controller.start();
        f.settings.set(THEME_NAME, Value::Str("Breeze-Dark".into()));
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
        f.settings.set(THEME_NAME, Value::Str("Breeze".into()));
        assert_eq!(f.last(), event(false, "Papirus-Light"));
        f.settings.set(PREFER_DARK, Value::Bool(true));
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
    }

    #[test]
    fn explicit_portal_light_overrides_dark_gtk_and_recovers_latest_fallback() {
        let f = Fixture::new("portal");
        f.settings.values.borrow_mut().insert(THEME_NAME.into(), Value::Str("Breeze-Dark".into()));
        f.controller.start();
        f.signal_scheme(2);
        assert_eq!(f.last(), event(false, "Papirus-Light"));
        f.signal_scheme(1);
        assert!(f.settings.prefer_dark());
        f.settings.desktop.borrow_mut().insert(PREFER_DARK.into(), Value::Bool(false));
        f.settings.set(THEME_NAME, Value::Str("Adwaita".into()));
        f.signal_scheme(0);
        assert_eq!(f.last(), event(false, "Papirus-Light"));
        assert_eq!(*f.settings.resets.borrow(), [PREFER_DARK]);
    }

    #[test]
    fn icon_reset_is_repaired_without_callback_or_recursive_settings_loop() {
        let f = Fixture::new("repair");
        f.controller.start();
        f.settings.set(ICON_THEME, Value::Str("Breeze".into()));
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Papirus-Light"));
        assert_eq!(f.events.borrow().len(), 1);
        assert_eq!(f.settings.writes.borrow().len(), 3);
    }

    #[test]
    fn missing_variant_uses_base_papirus() {
        let f = Fixture::new("variant");
        std::fs::remove_file(f.icons.join("Papirus-Dark/index.theme")).unwrap();
        f.controller.start();
        f.signal_scheme(1);
        assert_eq!(f.last(), event(true, "Papirus"));
    }

    #[test]
    fn missing_papirus_keeps_desktop_icons_without_an_override() {
        let f = Fixture::new("nopapirus");
        f.remove_indexes();
        f.controller.start();
        assert_eq!(*f.events.borrow(), [event(false, "Breeze")]);
        assert!(f.settings.writes.borrow().is_empty());
        f.settings.set(ICON_THEME, Value::Str("Adwaita".into()));
        assert_eq!(f.last(), event(false, "Adwaita"));
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Adwaita"));
        assert!(f.settings.resets.borrow().is_empty());
    }

    #[test]
    fn removed_papirus_releases_override_to_latest_desktop_theme() {
        let f = Fixture::new("removed");
        f.controller.start();
        f.settings.desktop.borrow_mut().insert(ICON_THEME.into(), Value::Str("Adwaita".into()));
        f.remove_indexes();
        f.theme_changed();
        assert_eq!(f.last(), event(false, "Adwaita"));
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Adwaita"));
        assert_eq!(*f.settings.resets.borrow(), [ICON_THEME]);
        assert!(!f.controller.icon_overridden());
    }

    #[test]
    fn papirus_installed_later_is_used_on_theme_change() {
        let f = Fixture::new("later");
        f.remove_indexes();
        f.controller.start();
        std::fs::write(f.icons.join("Papirus/index.theme"), "").unwrap();
        f.theme_changed();
        assert_eq!(f.last(), event(false, "Papirus"));
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Papirus"));
    }

    #[test]
    fn initial_portal_read_and_live_change_do_not_apply_stale_reply() {
        let f = Fixture::new("stale");
        f.controller.start();
        let reader = f.portal_ready();
        let serial = *reader.0.borrow().last().unwrap();
        f.signal_scheme(1);
        f.controller.portal_read(serial, Some(&reply(2)));
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
        f.controller.read_portal();
        let serial = *reader.0.borrow().last().unwrap();
        f.controller.portal_read(serial, Some(&reply(2)));
        assert_eq!(f.last(), event(false, "Papirus-Light"));
    }

    #[test]
    fn portal_loss_restores_gtk_then_reconnect_reads_again() {
        let f = Fixture::new("loss");
        f.controller.start();
        let reader = f.portal_ready();
        f.signal_scheme(1);
        f.controller.portal_owner_changed(false);
        assert_eq!(f.last(), event(false, "Papirus-Light"));
        assert!(!f.settings.prefer_dark());
        f.controller.portal_owner_changed(true);
        assert_eq!(reader.0.borrow().len(), 2);
    }

    #[test]
    fn portal_errors_and_unknown_scheme_keep_gtk_fallback() {
        let f = Fixture::new("errors");
        f.settings.values.borrow_mut().insert(THEME_NAME.into(), Value::Str("Adwaita-dark".into()));
        f.controller.start();
        let reader = f.portal_ready();
        let serial = *reader.0.borrow().last().unwrap();
        f.controller.portal_read(serial, None);
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
        f.signal_scheme(2);
        f.signal_scheme(99);
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
        assert_eq!(scheme_value(&"x".to_variant()), 0);
        assert_eq!(scheme_value(&true.to_variant()), 0);
        assert_eq!(scheme_value(&1i32.to_variant().to_variant()), 1);
    }

    #[test]
    fn close_disconnects_handlers_and_ignores_pending_results() {
        let f = Fixture::new("close");
        f.controller.start();
        let _reader = f.portal_ready();
        f.controller.close();
        assert!(f.settings.signals.borrow().is_empty());
        assert!(f.theme.changed.borrow().is_none());
        assert!(f.controller.cancellable().is_cancelled());
        f.settings.set(ICON_THEME, Value::Str("Breeze".into()));
        f.signal_scheme(1);
        assert_eq!(*f.events.borrow(), [event(false, "Papirus-Light")]);
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Breeze"));
    }

    #[test]
    fn icon_refresh_keeps_theme_preferences_and_does_not_restyle() {
        let f = Fixture::new("refresh");
        f.controller.start();
        let writes = f.settings.writes.borrow().len();
        let events = f.events.borrow().clone();
        let weak = Rc::downgrade(&f.controller.0);
        *f.theme.on_set.borrow_mut() = Some(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                AppearanceController(inner).settings_changed();
            }
        }));
        assert!(f.controller.refresh_icons());
        assert_eq!(*f.theme.set_calls.borrow(), [vec![f.icons.clone()]]);
        assert_eq!(f.settings.writes.borrow().len(), writes);
        assert_eq!(*f.events.borrow(), events);
        assert_eq!(f.controller.icon_theme_name().as_deref(), Some("Papirus-Light"));
        f.controller.settings_changed();
        assert_eq!(*f.events.borrow(), events);
    }

    #[test]
    fn papirus_family_or_missing_choice_keeps_papirus_following_dark_mode() {
        let f = Fixture::new("family");
        f.desktop.choose(Some("Papirus-Dark"));
        f.controller.start();
        assert_eq!(f.last(), event(false, "Papirus-Light"));
        f.desktop.choose(Some("Papirus"));
        f.signal_scheme(1);
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
        // A chosen theme that is not installed does not replace Papirus.
        f.desktop.choose(Some("breeze"));
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
        f.desktop.choose(None);
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
    }

    #[test]
    fn non_papirus_choice_replaces_the_papirus_override_and_follows_changes() {
        let f = Fixture::new("choice");
        f.install("Adwaita");
        f.install("Fluent");
        f.desktop.choose(Some("Adwaita"));
        f.controller.start();
        assert_eq!(*f.events.borrow(), [event(false, "Adwaita")]);
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Adwaita"));
        // Dark mode no longer switches Papirus variants.
        f.signal_scheme(1);
        assert_eq!(f.last(), event(true, "Adwaita"));
        f.desktop.choose(Some("Fluent"));
        assert_eq!(f.last(), event(true, "Fluent"));
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Fluent"));
        // Back to Papirus: dark mode decides the variant again.
        f.desktop.choose(Some("Papirus-Light"));
        assert_eq!(f.last(), event(true, "Papirus-Dark"));
        assert_eq!(f.settings.string(ICON_THEME).as_deref(), Some("Papirus-Dark"));
        assert_eq!(f.settings.desktop.borrow()[ICON_THEME], Value::Str("Breeze".into()));
        f.controller.close();
        assert!(f.desktop.changed.borrow().is_none());
    }

    #[test]
    fn icon_refresh_is_ignored_before_start_and_after_close() {
        let f = Fixture::new("ignored");
        assert!(!f.controller.refresh_icons());
        f.controller.start();
        f.controller.close();
        assert!(!f.controller.refresh_icons());
        assert!(f.theme.set_calls.borrow().is_empty());
    }
}
