//! weather_tray.py: standalone StatusNotifierItem (+ com.canonical.dbusmenu)
//! with the same forecast and settings as Waydock. One timer for the next due
//! refresh; settings and shared-cache writes are noticed through file monitors.

use crate::backend::{format_temperature, refresh_delay, system_clock, weather_description, weather_icon, ConfigStore,
                     WeatherConfig, WeatherSnapshot};
use crate::ui::WeatherSource;
use crate::worker::Worker;
use gio::prelude::*;
use glib::variant::ToVariant;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;
use suite_core::i18n::translate;
use suite_core::tr;

pub const ITEM: &str = "org.kde.StatusNotifierItem";
pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";
pub const MENU: &str = "com.canonical.dbusmenu";
pub const ITEM_PATH: &str = "/StatusNotifierItem";
pub const MENU_PATH: &str = "/Menu";
pub const XML: &str = r#"<node><interface name="org.kde.StatusNotifierItem">
<property name="Category" type="s" access="read"/>
<property name="Id" type="s" access="read"/>
<property name="Title" type="s" access="read"/>
<property name="Status" type="s" access="read"/>
<property name="WindowId" type="u" access="read"/>
<property name="IconName" type="s" access="read"/>
<property name="IconPixmap" type="a(iiay)" access="read"/>
<property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
<property name="ItemIsMenu" type="b" access="read"/>
<property name="Menu" type="o" access="read"/>
<method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
<signal name="NewIcon"/><signal name="NewToolTip"/><signal name="NewTitle"/>
</interface><interface name="com.canonical.dbusmenu">
<property name="Version" type="u" access="read"/>
<property name="TextDirection" type="s" access="read"/>
<property name="Status" type="s" access="read"/>
<property name="IconThemePath" type="as" access="read"/>
<method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
<method name="GetGroupProperties"><arg type="ai" direction="in"/><arg type="as" direction="in"/><arg type="a(ia{sv})" direction="out"/></method>
<method name="GetProperty"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
<method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method>
<method name="EventGroup"><arg type="a(isvu)" direction="in"/><arg type="ai" direction="out"/></method>
<method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
<method name="AboutToShowGroup"><arg type="ai" direction="in"/><arg type="ai" direction="out"/><arg type="ai" direction="out"/></method>
<signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
</interface></node>"#;

/// What the tray asks the application to do.
pub trait TrayHost {
    fn show_popup(&self, edge: &str, offset: i32);
    fn show_settings(&self);
    fn quit(&self);
}

/// `html.escape(text)` (quote=True).
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

pub struct Inner {
    host: Weak<dyn TrayHost>,
    store: Arc<ConfigStore>,
    client: Arc<dyn WeatherSource>,
    edge: String,
    offset: i32,
    pub closed: Cell<bool>,
    worker: Worker,
    pub config: RefCell<Option<WeatherConfig>>,
    pub snapshot: RefCell<Option<WeatherSnapshot>>,
    pub pending: Cell<bool>,
    pub icon: RefCell<String>,
    pub title: RefCell<String>,
    pub description: RefCell<String>,
    bus: gio::DBusConnection,
    registrations: RefCell<Vec<gio::RegistrationId>>,
    watch: RefCell<Option<Box<dyn FnOnce()>>>,
    cancel: gio::Cancellable,
    pub timer: RefCell<Option<glib::SourceId>>,
    debounce: RefCell<Option<glib::SourceId>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    pub cache_name: RefCell<Option<String>>,
    sleep: RefCell<Option<(gio::DBusConnection, gio::SignalSubscriptionId)>>,
}

#[derive(Clone)]
pub struct WeatherTray(pub Rc<Inner>);

fn up(weak: &Weak<Inner>) -> Option<WeatherTray> { weak.upgrade().map(WeatherTray) }

impl WeatherTray {
    pub fn new(host: Weak<dyn TrayHost>, bus: gio::DBusConnection, store: Arc<ConfigStore>, client: Arc<dyn WeatherSource>,
               edge: &str, offset: i32) -> Self {
        let tray = Self(Rc::new(Inner {
            host, store: store.clone(), client: client.clone(), edge: edge.into(), offset, closed: Cell::new(false),
            worker: Worker::new(), config: RefCell::new(None), snapshot: RefCell::new(None), pending: Cell::new(false),
            icon: RefCell::new("weather-few-clouds-symbolic".into()), title: RefCell::new(tr!("Weather")),
            description: RefCell::new(tr!("Choose a location in Weather Settings")), bus: bus.clone(),
            registrations: RefCell::new(Vec::new()), watch: RefCell::new(None), cancel: gio::Cancellable::new(),
            timer: RefCell::new(None), debounce: RefCell::new(None), monitors: RefCell::new(Vec::new()),
            cache_name: RefCell::new(None), sleep: RefCell::new(None),
        }));
        let info = gio::DBusNodeInfo::for_xml(XML).expect("SNI introspection");
        for (path, interface) in [(ITEM_PATH, ITEM), (MENU_PATH, MENU)] {
            let interface_info = info.lookup_interface(interface).expect("interface");
            let (w1, w2) = (Rc::downgrade(&tray.0), Rc::downgrade(&tray.0));
            let registration = bus.register_object(path, &interface_info)
                .method_call(move |_, _, _, interface, method, parameters, invocation| {
                    if let Some(tray) = up(&w1) { tray.method(interface.unwrap_or(""), method, &parameters, invocation) }
                })
                .property(move |_, _, _, interface, name| {
                    up(&w2).map(|tray| tray.property(interface, name)).unwrap_or_else(|| "".to_variant())
                })
                .build();
            if let Ok(id) = registration {
                tray.0.registrations.borrow_mut().push(id);
            }
        }
        let weak = Rc::downgrade(&tray.0);
        let watch = gio::bus_watch_name_on_connection(&bus, WATCHER, gio::BusNameWatcherFlags::NONE,
            move |_, _, _| if let Some(tray) = up(&weak) { tray.watcher() }, |_, _| {});
        tray.0.watch.replace(Some(Box::new(move || gio::bus_unwatch_name(watch))));
        tray.watch_file(&gio::File::for_path(&store.path), false);
        if let Some(cache_dir) = client.cache_dir() {
            tray.watch_file(&gio::File::for_path(cache_dir), true);
        }
        // Monotonic timers pause during suspend; reschedule after resume.
        let weak = Rc::downgrade(&tray.0);
        gio::bus_get(gio::BusType::System, Some(&tray.0.cancel), move |result| {
            let (Some(tray), Ok(system)) = (up(&weak), result) else { return };
            if tray.0.closed.get() {
                return;
            }
            let weak = Rc::downgrade(&tray.0);
            #[allow(deprecated)]
            let id = system.signal_subscribe(Some("org.freedesktop.login1"), Some("org.freedesktop.login1.Manager"),
                Some("PrepareForSleep"), Some("/org/freedesktop/login1"), None, gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, parameters| {
                    let Some(tray) = up(&weak) else { return };
                    if parameters.child_value(0).get::<bool>() == Some(false) && !tray.0.closed.get() {
                        tray.schedule(Some(5));
                    }
                });
            tray.0.sleep.replace(Some((system, id)));
        });
        tray.poll();
        tray
    }

    fn watch_file(&self, file: &gio::File, cache: bool) {
        let monitor = if cache {
            file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        } else {
            file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        };
        let Ok(monitor) = monitor else { return };
        let weak = Rc::downgrade(&self.0);
        monitor.connect_changed(move |_, file, other, event| {
            if let Some(tray) = up(&weak) { tray.file_changed(file, other, event, cache) }
        });
        self.0.monitors.borrow_mut().push(monitor);
    }

    fn file_changed(&self, file: &gio::File, other: Option<&gio::File>, event: gio::FileMonitorEvent, cache: bool) {
        if self.0.closed.get() || event == gio::FileMonitorEvent::AttributeChanged {
            return;
        }
        if cache {
            let names: Vec<String> = [Some(file), other].into_iter().flatten()
                .filter_map(|f| f.basename().map(|b| b.to_string_lossy().into_owned())).collect();
            let name = self.0.cache_name.borrow().clone();
            if name.as_ref().is_none_or(|n| !names.contains(n)) || self.0.pending.get() {
                return;
            }
        }
        if self.0.debounce.borrow().is_none() {
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_local_once(Duration::from_millis(250), move || {
                let Some(tray) = up(&weak) else { return };
                tray.0.debounce.replace(None);
                if !tray.0.closed.get() {
                    tray.poll();
                }
            });
            self.0.debounce.replace(Some(id));
        }
    }

    pub fn schedule(&self, seconds: Option<i64>) {
        if let Some(id) = self.0.timer.take() {
            id.remove();
        }
        if let (Some(seconds), false) = (seconds, self.0.closed.get()) {
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_seconds_local_once(seconds.max(1) as u32, move || {
                let Some(tray) = up(&weak) else { return };
                tray.0.timer.replace(None);
                tray.poll();
            });
            self.0.timer.replace(Some(id));
        }
    }

    fn watcher(&self) {
        // Re-register after panel restarts; use our unique bus name and object path.
        self.0.bus.call(Some(WATCHER), "/StatusNotifierWatcher", WATCHER, "RegisterStatusNotifierItem",
                        Some(&(ITEM_PATH,).to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 3000, Some(&self.0.cancel),
                        |_| {});
    }

    fn property(&self, interface: &str, name: &str) -> Variant {
        if interface == MENU {
            return match name {
                "Version" => 3u32.to_variant(),
                "TextDirection" => "ltr".to_variant(),
                "Status" => "normal".to_variant(),
                _ => Vec::<String>::new().to_variant(),
            };
        }
        match name {
            "Category" => "ApplicationStatus".to_variant(),
            "Id" => "desktop-suite-weather".to_variant(),
            "Title" => self.0.title.borrow().to_variant(),
            "Status" => "Active".to_variant(),
            "IconName" => self.0.icon.borrow().to_variant(),
            "WindowId" => 0u32.to_variant(),
            "IconPixmap" => Vec::<(i32, i32, Vec<u8>)>::new().to_variant(),
            "ToolTip" => (self.0.icon.borrow().clone(), Vec::<(i32, i32, Vec<u8>)>::new(), escape(&self.0.title.borrow()),
                          escape(&self.0.description.borrow())).to_variant(),
            "ItemIsMenu" => false.to_variant(),
            _ => glib::variant::ObjectPath::try_from(MENU_PATH.to_owned()).unwrap().to_variant(),
        }
    }

    /// dbusmenu item properties, filtered to `names` when given.
    pub fn props(ident: i32, names: &[String]) -> Result<glib::VariantDict, String> {
        let dict = glib::VariantDict::new(None);
        let mut values: Vec<(&str, Variant)> = Vec::new();
        match ident {
            0 => values.push(("children-display", "submenu".to_variant())),
            1..=3 => {
                let (text, icon) = match ident { 1 => ("Weather", "weather-few-clouds"), 2 => ("Weather Settings", "preferences-system"),
                                                 _ => ("Quit", "application-exit") };
                values.push(("label", translate(text).to_variant()));
                values.push(("enabled", true.to_variant()));
                values.push(("visible", true.to_variant()));
                values.push(("icon-name", icon.to_variant()));
            }
            _ => return Err("Unknown menu item".into()),
        }
        for (key, value) in values {
            if names.is_empty() || names.iter().any(|n| n == key) {
                dict.insert_value(key, &value);
            }
        }
        Ok(dict)
    }

    fn action(&self, ident: i32) {
        let Some(host) = self.0.host.upgrade() else { return };
        match ident {
            1 => host.show_popup(&self.0.edge, self.0.offset),
            2 => host.show_settings(),
            3 => {
                glib::idle_add_local_once(move || host.quit());
            }
            _ => {}
        }
    }

    fn later(&self, ident: i32) {
        let weak = Rc::downgrade(&self.0);
        glib::idle_add_local_once(move || if let Some(tray) = up(&weak) { tray.action(ident) });
    }

    fn method(&self, interface: &str, method: &str, args: &Variant, invocation: gio::DBusMethodInvocation) {
        match self.method_result(interface, method, args) {
            Ok(result) => invocation.return_value(result.as_ref()),
            Err(error) => invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", &error),
        }
    }

    pub fn method_result(&self, interface: &str, method: &str, args: &Variant) -> Result<Option<Variant>, String> {
        if interface == ITEM {
            if matches!(method, "Activate" | "SecondaryActivate" | "ContextMenu") {
                self.later(if method == "Activate" { 1 } else { 2 });
            }
            return Ok(None);
        }
        let entry = |ident: i32, names: &[String]| -> Result<Variant, String> {
            Ok(Variant::tuple_from_iter([ident.to_variant(), Self::props(ident, names)?.end(),
                                         Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, Vec::<Variant>::new())]))
        };
        match method {
            "GetLayout" => {
                let (ident, depth, names) = args.get::<(i32, i32, Vec<String>)>().ok_or("Invalid arguments")?;
                let children: Vec<Variant> = if ident == 0 && depth != 0 {
                    (1..=3).map(|i| entry(i, &names).map(|e| Variant::from_variant(&e))).collect::<Result<_, _>>()?
                } else { Vec::new() };
                let layout = Variant::tuple_from_iter([ident.to_variant(), Self::props(ident, &names)?.end(),
                                                       Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, children)]);
                Ok(Some(Variant::tuple_from_iter([1u32.to_variant(), layout])))
            }
            "GetGroupProperties" => {
                let (ids, names) = args.get::<(Vec<i32>, Vec<String>)>().ok_or("Invalid arguments")?;
                let ids = if ids.is_empty() { vec![0, 1, 2, 3] } else { ids };
                let items: Vec<Variant> = ids.into_iter().filter(|i| (0..=3).contains(i))
                    .map(|i| Self::props(i, &names).map(|p| Variant::tuple_from_iter([i.to_variant(), p.end()])))
                    .collect::<Result<_, _>>()?;
                let array = Variant::array_from_iter_with_type(glib::VariantTy::new("(ia{sv})").unwrap(), items);
                Ok(Some(Variant::tuple_from_iter([array])))
            }
            "GetProperty" => {
                let (ident, name) = args.get::<(i32, String)>().ok_or("Invalid arguments")?;
                let value = Self::props(ident, &[])?.lookup_value(&name, None).ok_or_else(|| format!("'{name}'"))?;
                Ok(Some(Variant::tuple_from_iter([Variant::from_variant(&value)])))
            }
            "Event" | "EventGroup" => {
                let events: Vec<(i32, String)> = if method == "Event" {
                    let ident = args.child_value(0).get::<i32>().ok_or("Invalid arguments")?;
                    let event = args.child_value(1).get::<String>().ok_or("Invalid arguments")?;
                    vec![(ident, event)]
                } else {
                    let list = args.child_value(0);
                    (0..list.n_children()).map(|i| {
                        let item = list.child_value(i);
                        Some((item.child_value(0).get::<i32>()?, item.child_value(1).get::<String>()?))
                    }).collect::<Option<_>>().ok_or("Invalid arguments")?
                };
                let mut errors = Vec::new();
                for (ident, event) in events {
                    if !(0..=3).contains(&ident) {
                        errors.push(ident);
                    } else if event == "clicked" {
                        self.later(ident);
                    }
                }
                Ok((method == "EventGroup").then(|| (errors,).to_variant()))
            }
            "AboutToShow" => Ok(Some((false,).to_variant())),
            "AboutToShowGroup" => {
                let (ids,) = args.get::<(Vec<i32>,)>().ok_or("Invalid arguments")?;
                let unknown: Vec<i32> = ids.into_iter().filter(|i| !(0..=3).contains(i)).collect();
                Ok(Some((Vec::<i32>::new(), unknown).to_variant()))
            }
            _ => Ok(None),
        }
    }

    /// Re-read settings; read the (cached) forecast when settings changed, the
    /// shared cache changed, or the scheduled refresh is due.
    pub fn poll(&self) {
        if self.0.closed.get() {
            return;
        }
        let config = self.0.store.configured();
        if config != *self.0.config.borrow() {
            self.0.worker.cancel();
            self.0.pending.set(false);
            self.0.config.replace(config.clone());
            self.0.snapshot.replace(None);
            self.update();
        }
        let Some(config) = config else {
            self.0.cache_name.replace(None);
            self.schedule(None);
            return;
        };
        self.0.cache_name.replace(self.0.client.cache_path(&config)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())));
        if !self.0.pending.get() {
            self.0.pending.set(true);
            let weak = Rc::downgrade(&self.0);
            let client = self.0.client.clone();
            self.0.worker.run(move || client.read(&config, false), move |snapshot: Option<WeatherSnapshot>, _error| {
                let Some(tray) = up(&weak) else { return };
                tray.0.pending.set(false);
                let delay = snapshot.as_ref().map(|s| refresh_delay(s, system_clock())).unwrap_or(Some(60));
                if snapshot.is_some() {
                    tray.0.snapshot.replace(snapshot);
                }
                tray.update();
                tray.schedule(delay);
            });
        }
    }

    pub fn update(&self) {
        let mut icon = "weather-few-clouds-symbolic".to_owned();
        let mut title = tr!("Weather");
        let mut description = tr!("Choose a location in Weather Settings");
        let config = self.0.config.borrow().clone();
        if let Some(config) = &config {
            title = config.name.clone();
            description = tr!("Loading weather…");
        }
        if let Some(snapshot) = self.0.snapshot.borrow().as_ref() {
            match (&snapshot.current, &config) {
                (Some(current), Some(config)) => {
                    icon = weather_icon(Some(current.code), current.is_day).into();
                    title = format!("{} · {}", config.name, format_temperature(Some(current.temp), &config.units));
                    description = translate(weather_description(Some(current.code)));
                }
                _ => description = tr!("Weather unavailable"),
            }
            if snapshot.stale || snapshot.error.is_some() {
                description += &format!("\n{}", tr!("Saved weather · waiting for an update"));
            }
        }
        self.0.icon.replace(icon);
        self.0.title.replace(title);
        self.0.description.replace(description);
        for signal in ["NewIcon", "NewToolTip", "NewTitle"] {
            let _ = self.0.bus.emit_signal(None, ITEM_PATH, ITEM, signal, None);
        }
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.worker.close();
        self.0.cancel.cancel();
        self.schedule(None);
        if let Some(id) = self.0.debounce.take() {
            id.remove();
        }
        for monitor in self.0.monitors.borrow_mut().drain(..) {
            monitor.cancel();
        }
        if let Some((system, id)) = self.0.sleep.take() {
            #[allow(deprecated)]
            system.signal_unsubscribe(id);
        }
        if let Some(unwatch) = self.0.watch.take() {
            unwatch();
        }
        for id in self.0.registrations.borrow_mut().drain(..) {
            let _ = self.0.bus.unregister_object(id);
        }
    }
}
