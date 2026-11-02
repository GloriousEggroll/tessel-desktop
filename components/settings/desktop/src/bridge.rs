//! `dock_pages.DockSettingsBridge`: edit only one page's dock fields and
//! leave every unrelated live choice intact.
//!
//! Online (the dock owns `org.tessel.Tessel Dock`): `Read`/`Apply`/`Preview`/
//! `Restore`/`GetTrayApps` on `org.tessel.Settings` at `/org/tessel/Settings`,
//! always with `NO_AUTO_START`. Offline: `tessel_dock::core::config::ConfigStore`
//! (`$XDG_CONFIG_HOME/tessel-dock/config.json`, the dock's own byte format) and
//! `settings_api::apply_patch`, so concurrent edits of other fields survive.
//!
//! The blocking part ([`BridgeCore`]) is `Send + Sync` so the Desktop Layout
//! page can drive it from a worker thread, exactly as Python did; the GTK part
//! ([`DockSettingsBridge`]) owns the preview timer and the error callback.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gio::prelude::*;
use gtk::prelude::*;
use glib::Variant;
use serde_json::{Map, Value};
use tessel_core::pyerr::python_error_text;
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_dock::core::config::{ConfigStore, DockConfig};
use tessel_dock::core::monitors::monitor_label;
use tessel_dock::core::tray_prefs::TrayEntry;
use tessel_dock::dbus::settings_api::{self, apply_patch, py_value_eq, BUS_NAME, INTERFACE, OBJECT_PATH};
use tessel_dock::ui::settings::{AppearancePreview, SettingsApp};

/// `(name, label)` of one output, as the dock reports it.
pub type MonitorEntry = (String, String);

/// `DOCK_FIELDS` as a set of owned names.
pub fn dock_fields() -> BTreeSet<String> {
    settings_api::dock_fields().into_iter().map(str::to_owned).collect()
}

pub fn desktop_fields() -> BTreeSet<String> {
    settings_api::DESKTOP_FIELDS.iter().map(|s| s.to_string()).collect()
}

pub fn appearance_fields() -> BTreeSet<String> {
    settings_api::APPEARANCE_FIELDS.iter().map(|s| s.to_string()).collect()
}

/// `SETTINGS_FIELDS` (= `layout_presets.PRESET_FIELDS`).
pub fn settings_fields() -> BTreeSet<String> {
    settings_api::settings_fields().into_iter().map(str::to_owned).collect()
}

fn dumps(value: &Value) -> String {
    json_dumps(value, &JsonStyle::DEFAULT)
}

/// `DockConfig.from_dict(asdict(previous) | {key: new[key] for key in fields})`
/// (Python's `dataclasses.replace(previous, **…)`).
pub fn replace_fields(previous: &DockConfig, new: &DockConfig, fields: &BTreeSet<String>) -> DockConfig {
    let (Value::Object(mut base), Value::Object(update)) = (previous.to_value(), new.to_value()) else {
        return previous.clone();
    };
    for key in fields {
        if let Some(value) = update.get(key) {
            base.insert(key.clone(), value.clone());
        }
    }
    DockConfig::from_value(&Value::Object(base)).unwrap_or_else(|_| previous.clone())
}

/// `[{"name", "label"}]` from GDK (`_display_monitors`). GTK thread only.
pub fn display_monitors() -> Vec<MonitorEntry> {
    let mut monitors = Vec::new();
    let Some(display) = gtk::gdk::Display::default() else { return monitors };
    let model = display.monitors();
    for index in 0..model.n_items() {
        let Some(monitor) = model.item(index).and_downcast::<gtk::gdk::Monitor>() else { continue };
        let name = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty())
            .unwrap_or_else(|| format!("Display-{}", index + 1));
        let label = monitor_label(&name, monitor.description().as_deref().unwrap_or(""), monitor.model().as_deref().unwrap_or(""));
        monitors.push((name, label));
    }
    monitors
}

struct Data {
    config: DockConfig,
    monitors: Vec<MonitorEntry>,
    tray_apps: Vec<TrayEntry>,
    online: bool,
    error: String,
    offline_monitors: Vec<MonitorEntry>,
    closed: bool,
}

/// Construction options (`DockSettingsBridge(application, *, fields, on_error, bus, store, read)`).
pub struct BridgeOptions {
    pub fields: BTreeSet<String>,
    pub bus: Option<gio::DBusConnection>,
    pub store: Option<ConfigStore>,
    pub read: bool,
    /// Test seam for Python's overridden `_running()` (the offline test bridge).
    pub running: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self { fields: dock_fields(), bus: None, store: None, read: true, running: None }
    }
}

/// The blocking half: D-Bus calls and the offline store under one lock.
pub struct BridgeCore {
    pub fields: BTreeSet<String>,
    /// `None` only for offline test bridges (a `running` override and no bus).
    bus: Option<gio::DBusConnection>,
    running_override: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    store: Mutex<ConfigStore>,
    /// Python `self._lock` (an RLock around every refresh/save).
    op: Mutex<()>,
    data: Mutex<Data>,
}

fn tray_from_value(value: &Value) -> Vec<TrayEntry> {
    value.as_array().map(|items| items.iter().filter_map(|item| {
        Some(TrayEntry {
            id: item.get("id")?.as_str()?.to_owned(),
            label: item.get("label").map(tessel_core::pyjson::py_str).unwrap_or_default(),
            running: item.get("running").is_some_and(tessel_core::pyjson::truthy),
        })
    }).collect()).unwrap_or_default()
}

fn monitors_from_value(value: &Value) -> Result<Vec<MonitorEntry>, String> {
    let items = value.as_array().ok_or_else(|| "'monitors' must be a list".to_owned())?;
    items.iter().map(|item| {
        let name = item.get("name").and_then(Value::as_str).ok_or_else(|| "'name'".to_owned())?;
        let label = item.get("label").and_then(Value::as_str).ok_or_else(|| "'label'".to_owned())?;
        Ok((name.to_owned(), label.to_owned()))
    }).collect()
}

impl BridgeCore {
    fn data(&self) -> std::sync::MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> DockConfig { self.data().config.clone() }
    pub fn set_config(&self, config: DockConfig) { self.data().config = config; }
    pub fn monitors(&self) -> Vec<MonitorEntry> { self.data().monitors.clone() }
    pub fn tray_apps(&self) -> Vec<TrayEntry> { self.data().tray_apps.clone() }
    pub fn online(&self) -> bool { self.data().online }
    /// Test seam (Python tests assign `bridge.online`).
    pub fn set_online(&self, online: bool) { self.data().online = online; }
    pub fn error(&self) -> String { self.data().error.clone() }
    pub fn closed(&self) -> bool { self.data().closed }
    pub fn bus(&self) -> Option<&gio::DBusConnection> { self.bus.as_ref() }

    fn connection(&self) -> Result<&gio::DBusConnection, String> {
        self.bus.as_ref().ok_or_else(|| "No D-Bus connection".to_owned())
    }
    pub fn store_path(&self) -> std::path::PathBuf { self.store.lock().unwrap().path.clone() }

    /// `_running()`: `NameHasOwner(org.tessel.Tessel Dock)` without auto-start.
    pub fn running(&self) -> Result<bool, String> {
        if let Some(running) = &self.running_override {
            return Ok(running());
        }
        let reply = self.connection()?.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus",
            "NameHasOwner", Some(&(BUS_NAME,).to_variant()), Some(glib::VariantTy::new("(b)").unwrap()),
            gio::DBusCallFlags::NO_AUTO_START, 1000, gio::Cancellable::NONE).map_err(|e| python_error_text(&e))?;
        Ok(reply.child_value(0).get::<bool>().unwrap_or(false))
    }

    /// `_call(method, payload)`: one JSON string in, one JSON string out.
    fn call(&self, method: &str, payload: Option<&Value>) -> Result<Value, String> {
        let params: Option<Variant> = payload.map(|p| (dumps(p),).to_variant());
        let reply = self.connection()?.call_sync(Some(BUS_NAME), OBJECT_PATH, INTERFACE, method, params.as_ref(), None,
            gio::DBusCallFlags::NO_AUTO_START, 2000, gio::Cancellable::NONE).map_err(|e| python_error_text(&e))?;
        let text = reply.try_child_value(0).and_then(|v| v.get::<String>()).unwrap_or_default();
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    fn take_snapshot(&self, snapshot: &Value) -> Result<(), String> {
        let config = DockConfig::from_value(snapshot.get("config").ok_or("'config'")?)?;
        let monitors = monitors_from_value(snapshot.get("monitors").ok_or("'monitors'")?)?;
        let tray = snapshot.get("tray_apps").map(tray_from_value).unwrap_or_default();
        let mut data = self.data();
        data.config = config;
        data.monitors = monitors;
        data.tray_apps = tray;
        Ok(())
    }

    fn load_store(&self) -> Result<DockConfig, String> {
        let mut store = self.store.lock().unwrap();
        let config = store.load(None, None);
        if !store.error.is_empty() {
            return Err(store.error.clone());
        }
        Ok(config)
    }

    /// `_read()`. `gdk_monitors` is the GTK-thread monitor list (Python reads
    /// GDK only on the main thread; workers use the last snapshot).
    fn read(&self, gdk_monitors: Option<Vec<MonitorEntry>>) -> Result<(), String> {
        let online = self.running()?;
        self.data().online = online;
        if online {
            let snapshot = self.call("Read", None)?;
            self.take_snapshot(&snapshot)
        } else {
            self.data().tray_apps.clear();
            let config = self.load_store()?;
            let mut data = self.data();
            data.config = config;
            data.monitors = gdk_monitors.unwrap_or_else(|| data.offline_monitors.clone());
            Ok(())
        }
    }

    /// `refresh(fields=…, report=…)` without the report callback (the GTK
    /// side reports). Returns `Err(message)` and records `error`.
    pub fn refresh(&self, fields: Option<&BTreeSet<String>>, gdk_monitors: Option<Vec<MonitorEntry>>) -> Result<(), String> {
        let _guard = self.op.lock().unwrap_or_else(|e| e.into_inner());
        let previous = self.config();
        let result = self.read(gdk_monitors);
        match result {
            Ok(()) => {
                if let Some(fields) = fields {
                    let current = self.config();
                    self.set_config(replace_fields(&previous, &current, fields));
                }
                self.data().error.clear();
                Ok(())
            }
            Err(error) => {
                self.data().error = error.clone();
                Err(error)
            }
        }
    }

    /// The `{"changes", "expected"}` request for this page's fields.
    pub fn request_for(&self, config: &DockConfig) -> Value {
        let old = self.config().to_value();
        let desired = config.to_value();
        let mut changes = Map::new();
        let mut expected = Map::new();
        for key in &self.fields {
            let (Some(a), Some(b)) = (old.get(key), desired.get(key)) else { continue };
            if !py_value_eq(a, b) {
                changes.insert(key.clone(), b.clone());
                expected.insert(key.clone(), a.clone());
            }
        }
        let mut request = Map::new();
        request.insert("changes".into(), Value::Object(changes));
        request.insert("expected".into(), Value::Object(expected));
        Value::Object(request)
    }

    /// `_save_config` minus `restore_appearance()` (the caller restores on
    /// the thread it runs on).
    pub fn save(&self, config: &DockConfig) -> Result<(), String> {
        let _guard = self.op.lock().unwrap_or_else(|e| e.into_inner());
        let request = self.request_for(config);
        let result = (|| -> Result<(), String> {
            if self.running()? {
                let snapshot = self.call("Apply", Some(&request))?;
                self.take_snapshot(&snapshot)?;
                self.data().online = true;
            } else {
                let current = self.load_store()?;
                let patched = apply_patch(&current, &request)?;
                let saved = self.store.lock().unwrap().save(&patched)?;
                let mut data = self.data();
                data.config = saved;
                data.online = false;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.data().error.clear();
                Ok(())
            }
            Err(error) => {
                self.data().error = error.clone();
                Err(error)
            }
        }
    }

    /// `Restore` from a worker thread (blocking, short timeout).
    pub fn restore_blocking(&self) {
        if let (true, Some(bus)) = (self.online(), &self.bus) {
            let _ = bus.call_sync(Some(BUS_NAME), OBJECT_PATH, INTERFACE, "Restore", None, None,
                gio::DBusCallFlags::NO_AUTO_START, 1500, gio::Cancellable::NONE);
        }
    }

    pub fn mark_closed(&self) { self.data().closed = true; }
    pub fn set_offline_monitors(&self, monitors: Vec<MonitorEntry>) { self.data().offline_monitors = monitors; }
}

/// What a bridge looks like from a worker (`layout_presets.apply_preset`,
/// `LayoutSettings.refresh`). The real core implements it; tests use fakes.
pub trait PresetBridge: Send + Sync {
    fn config(&self) -> DockConfig;
    fn refresh(&self) -> bool;
    fn save_config(&self, config: &DockConfig) -> bool;
    fn error(&self) -> String;
    fn close(&self);
}

impl PresetBridge for BridgeCore {
    fn config(&self) -> DockConfig { BridgeCore::config(self) }
    fn refresh(&self) -> bool { BridgeCore::refresh(self, None, None).is_ok() }
    fn save_config(&self, config: &DockConfig) -> bool {
        let ok = self.save(config).is_ok();
        if ok {
            self.restore_blocking();
        }
        ok
    }
    fn error(&self) -> String { BridgeCore::error(self) }
    fn close(&self) {
        // restore_appearance() + _closed; the page closes on the GTK thread.
        if let (true, Some(bus)) = (self.online(), &self.bus) {
            bus.call(Some(BUS_NAME), OBJECT_PATH, INTERFACE, "Restore", None, None,
                gio::DBusCallFlags::NO_AUTO_START, 1500, gio::Cancellable::NONE, |_| {});
        }
        self.mark_closed();
    }
}

/// Optional arguments of `preview_appearance(...)`.
#[derive(Clone, Debug, Default)]
pub struct PreviewArgs {
    pub transparency: i64,
    pub match_wallpaper: bool,
    pub rounded_corners: Option<bool>,
    pub float_mode: Option<bool>,
    pub transparency_enabled: Option<bool>,
    pub frosted: Option<bool>,
    pub frost_strength: Option<i64>,
    pub bar_border_enabled: Option<bool>,
    pub bar_border_sides: Option<Vec<String>>,
    pub bar_border_color: Option<String>,
}

type ErrorCallback = Rc<dyn Fn(&str)>;

struct GtkSide {
    core: Arc<BridgeCore>,
    application: Option<gtk::Application>,
    on_error: RefCell<ErrorCallback>,
    preview_source: RefCell<Option<glib::SourceId>>,
    preview_values: RefCell<Option<Map<String, Value>>>,
}

/// The GTK-thread bridge handle (cheap to clone).
#[derive(Clone)]
pub struct DockSettingsBridge(Rc<GtkSide>);

impl DockSettingsBridge {
    /// `DockSettingsBridge(application, fields=…, on_error=…, bus=…, store=…, read=…)`.
    pub fn new(application: Option<gtk::Application>, options: BridgeOptions,
               on_error: Option<ErrorCallback>) -> Result<Self, String> {
        Self::with_monitors(application, options, on_error, display_monitors())
    }

    /// A bridge built without GDK (unit tests): no display monitors.
    pub fn new_without_display(application: Option<gtk::Application>, options: BridgeOptions) -> Result<Self, String> {
        Self::with_monitors(application, options, None, Vec::new())
    }

    fn with_monitors(application: Option<gtk::Application>, options: BridgeOptions, on_error: Option<ErrorCallback>,
                     monitors: Vec<MonitorEntry>) -> Result<Self, String> {
        let store = options.store.unwrap_or_default();
        let bus = match (options.bus, &options.running) {
            (Some(bus), _) => Some(bus),
            (None, Some(_)) => None,
            (None, None) => Some(gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| python_error_text(&e))?),
        };
        let core = Arc::new(BridgeCore {
            fields: options.fields,
            bus,
            running_override: options.running,
            store: Mutex::new(store),
            op: Mutex::new(()),
            data: Mutex::new(Data {
                config: DockConfig::default(), monitors: Vec::new(), tray_apps: Vec::new(), online: false,
                error: String::new(), offline_monitors: monitors.clone(), closed: false,
            }),
        });
        if options.read {
            core.read(Some(monitors))?;
        } else {
            // Start from the saved file; refresh_async() then reads the live
            // dock without blocking the GTK thread on D-Bus timeouts.
            let config = core.load_store()?;
            let mut data = core.data();
            data.config = config;
            data.monitors = monitors;
        }
        Ok(Self(Rc::new(GtkSide {
            core, application, on_error: RefCell::new(on_error.unwrap_or_else(|| Rc::new(|_| {}))),
            preview_source: RefCell::new(None), preview_values: RefCell::new(None),
        })))
    }

    pub fn core(&self) -> Arc<BridgeCore> { self.0.core.clone() }
    pub fn fields(&self) -> &BTreeSet<String> { &self.0.core.fields }
    pub fn config(&self) -> DockConfig { self.0.core.config() }
    pub fn online(&self) -> bool { self.0.core.online() }
    pub fn set_online(&self, online: bool) { self.0.core.set_online(online) }
    pub fn error(&self) -> String { self.0.core.error() }
    pub fn tray_apps(&self) -> Vec<TrayEntry> { self.0.core.tray_apps() }
    pub fn monitors(&self) -> Vec<MonitorEntry> { self.0.core.monitors() }
    pub fn closed(&self) -> bool { self.0.core.closed() }
    /// Python `bridge._preview_values` (tests).
    pub fn preview_values(&self) -> Option<Map<String, Value>> { self.0.preview_values.borrow().clone() }

    pub fn set_on_error(&self, callback: ErrorCallback) { self.0.on_error.replace(callback); }
    fn report(&self, message: &str) {
        let callback = self.0.on_error.borrow().clone();
        callback(message);
    }

    /// `monitor_entries()` → `(name, label)`.
    pub fn monitor_entries(&self) -> Vec<MonitorEntry> { self.monitors() }

    /// `refresh(fields=None, report=True)` on the GTK thread.
    pub fn refresh(&self, fields: Option<&BTreeSet<String>>, report: bool) -> bool {
        match self.0.core.refresh(fields, Some(display_monitors())) {
            Ok(()) => true,
            Err(error) => {
                if report {
                    self.report(&error);
                }
                false
            }
        }
    }

    /// `_background(work, done)`: worker thread; `done(ok)` on the GTK thread
    /// unless the bridge closed meanwhile (failures are reported first).
    fn background(&self, work: impl FnOnce(&BridgeCore) -> bool + Send + 'static, done: Option<Box<dyn FnOnce(bool)>>) {
        self.0.core.set_offline_monitors(display_monitors());
        let core = self.0.core.clone();
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let ok = gio::spawn_blocking(move || work(&core)).await.unwrap_or(false);
            if this.closed() {
                return;
            }
            if !ok {
                this.report(&this.error());
            }
            if let Some(done) = done {
                done(ok);
            }
        });
    }

    pub fn refresh_async(&self, done: Option<Box<dyn FnOnce(bool)>>, fields: Option<BTreeSet<String>>) {
        self.background(move |core| core.refresh(fields.as_ref(), None).is_ok(), done);
    }

    pub fn save_config_async(&self, config: DockConfig, done: Option<Box<dyn FnOnce(bool)>>) {
        let this = self.clone();
        let done: Box<dyn FnOnce(bool)> = Box::new(move |ok| {
            if ok {
                this.restore_appearance();
            }
            if let Some(done) = done {
                done(ok);
            }
        });
        self.background(move |core| core.save(&config).is_ok(), Some(done));
    }

    /// `save_config(config, rebuild=False, *, report=True)` on the GTK thread.
    pub fn save_config(&self, config: &DockConfig, report: bool) -> bool {
        match self.0.core.save(config) {
            Ok(()) => {
                self.restore_appearance();
                true
            }
            Err(error) => {
                if report {
                    self.report(&error);
                }
                false
            }
        }
    }

    /// `refresh_tray_entries(done)`: `GetTrayApps`, empty on any failure.
    pub fn refresh_tray_entries(&self, done: Box<dyn FnOnce()>) {
        let this = self.clone();
        let Some(bus) = self.0.core.bus.clone() else {
            this.0.core.data().tray_apps.clear();
            done();
            return;
        };
        bus.call(Some(BUS_NAME), OBJECT_PATH, INTERFACE, "GetTrayApps", None, None,
            gio::DBusCallFlags::NO_AUTO_START, 1500, gio::Cancellable::NONE, move |result| {
                if this.closed() {
                    return;
                }
                let apps = result.ok().and_then(|reply| reply.try_child_value(0)).and_then(|v| v.get::<String>())
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok()).map(|v| tray_from_value(&v))
                    .unwrap_or_default();
                this.0.core.data().tray_apps = apps;
                done();
            });
    }

    /// `preview_appearance(transparency, match_wallpaper, …)`: coalesced 75 ms `Preview`.
    pub fn preview_appearance(&self, args: PreviewArgs) {
        if self.closed() || !self.online() {
            return;
        }
        let config = self.config();
        let mut values = Map::new();
        values.insert("transparency".into(), args.transparency.into());
        values.insert("match_wallpaper".into(), args.match_wallpaper.into());
        values.insert("rounded_corners".into(), args.rounded_corners.unwrap_or_else(|| config.effective_rounded_corners()).into());
        values.insert("float_mode".into(), args.float_mode.unwrap_or_else(|| config.effective_float_mode()).into());
        if let Some(value) = args.transparency_enabled {
            values.insert("transparency_enabled".into(), value.into());
        }
        if let Some(value) = args.frosted {
            values.insert("frosted".into(), value.into());
        }
        if let Some(value) = args.frost_strength {
            values.insert("frost_strength".into(), value.into());
        }
        if let Some(value) = args.bar_border_enabled {
            values.insert("bar_border_enabled".into(), value.into());
        }
        if let Some(value) = args.bar_border_sides {
            values.insert("bar_border_sides".into(), value.into());
        }
        if let Some(value) = args.bar_border_color {
            values.insert("bar_border_color".into(), value.into());
        }
        self.0.preview_values.replace(Some(values));
        if self.0.preview_source.borrow().is_none() {
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_local(std::time::Duration::from_millis(75), move || {
                if let Some(inner) = weak.upgrade() {
                    DockSettingsBridge(inner).send_preview();
                }
                glib::ControlFlow::Break
            });
            self.0.preview_source.replace(Some(id));
        }
    }

    fn send_preview(&self) {
        self.0.preview_source.take();
        if self.closed() {
            return;
        }
        if let (Some(values), Some(bus)) = (self.0.preview_values.borrow().clone(), self.0.core.bus.as_ref()) {
            bus.call(Some(BUS_NAME), OBJECT_PATH, INTERFACE, "Preview",
                Some(&(dumps(&Value::Object(values)),).to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 1500,
                gio::Cancellable::NONE, |_| {});
        }
    }

    /// `restore_appearance()`.
    pub fn restore_appearance(&self) {
        if let Some(id) = self.0.preview_source.take() {
            id.remove();
        }
        self.0.preview_values.replace(None);
        if let (true, Some(bus)) = (self.online(), self.0.core.bus.as_ref()) {
            bus.call(Some(BUS_NAME), OBJECT_PATH, INTERFACE, "Restore", None, None,
                gio::DBusCallFlags::NO_AUTO_START, 1500, gio::Cancellable::NONE, |_| {});
        }
    }

    pub fn close(&self) {
        self.restore_appearance();
        self.0.core.mark_closed();
    }
}

impl SettingsApp for DockSettingsBridge {
    fn config(&self) -> DockConfig { DockSettingsBridge::config(self) }
    fn save_config(&self, config: DockConfig, _rebuild: bool) -> bool { DockSettingsBridge::save_config(self, &config, true) }
    fn preview_appearance(&self, preview: &AppearancePreview) {
        DockSettingsBridge::preview_appearance(self, PreviewArgs {
            transparency: preview.transparency,
            match_wallpaper: preview.match_wallpaper,
            rounded_corners: Some(preview.rounded_corners),
            float_mode: Some(preview.float_mode),
            transparency_enabled: Some(preview.transparency_enabled),
            frosted: Some(preview.frosted),
            frost_strength: Some(preview.frost_strength),
            bar_border_enabled: Some(preview.bar_border_enabled),
            bar_border_sides: Some(preview.bar_border_sides.clone()),
            bar_border_color: Some(preview.bar_border_color.clone()),
        })
    }
    fn restore_appearance(&self) { DockSettingsBridge::restore_appearance(self) }
    fn monitor_entries(&self) -> Vec<(String, String)> { self.monitors() }
    fn tray_entries(&self) -> Vec<TrayEntry> { self.tray_apps() }
    fn refresh_tray_entries(&self, done: Box<dyn FnOnce()>) { DockSettingsBridge::refresh_tray_entries(self, done) }
    fn application(&self) -> Option<gtk::Application> { self.0.application.clone() }
    fn show_error(&self, message: &str) { self.report(message) }
}
