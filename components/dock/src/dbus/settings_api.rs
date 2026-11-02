//! settings_api.py: validated settings access for Global Settings without
//! launching another dock — `org.tessel.Settings` at `/org/tessel/Settings`
//! on the application's connection (the app owns `org.tessel.Tessel Dock`).
//!
//! Methods: `Read() -> s`, `Apply(s) -> s`, `Preview(s)`, `Restore()`,
//! `GetWorkareas() -> s`, `GetPopupPlacement(ss) -> s`, `GetTrayApps() -> s`,
//! signal `WorkareasChanged(s)` (idle-coalesced, only when the payload
//! changes). Errors: `org.tessel.Settings.Error` with Python's messages. The
//! preview is restored when its owner's unique name vanishes and on close.
//!
//! API: implement [`SettingsHost`] in the application, then
//! `SettingsApi::new(&connection, host) -> Result<SettingsApi, glib::Error>`,
//! `.queue_workareas_changed()`, `.workareas()`, `.snapshot()`,
//! `.popup_placement(monitor, group)`, `.preview_owner()`, `.close()`.
//! Pure: [`apply_patch`], [`workarea_snapshot`], [`validate_preview`], field sets
//! (`APPEARANCE_FIELDS`, `BORDER_FIELDS`, `DESKTOP_FIELDS`, `LAYOUT_FIELDS`,
//! [`dock_fields`], [`settings_fields`]).

use crate::core::config::{field_names, DockConfig};
use crate::core::group_layout::{default_layout, APP_DOCK_GROUPS, LAYOUT_GROUPS};
use gio::prelude::*;
use glib::Variant;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const BUS_NAME: &str = "org.tessel.Tessel Dock";
pub const OBJECT_PATH: &str = "/org/tessel/Settings";
pub const INTERFACE: &str = "org.tessel.Settings";
pub const ERROR: &str = "org.tessel.Settings.Error";

pub const APPEARANCE_FIELDS: &[&str] = &["transparency", "transparency_enabled", "frosted", "frost_strength", "match_wallpaper",
    "apps_transparency", "apps_transparency_enabled", "apps_frosted", "apps_frost_strength", "apps_rounded_corners",
    "window_buttons_left", "colored_window_buttons", "window_shadows"];
pub const BORDER_FIELDS: &[&str] = &["bar_border_enabled", "bar_border_sides", "bar_border_color"];
const DOCK_BASE_FIELDS: &[&str] = &["mode", "application_display", "monitor", "monitor_overrides", "icon_size", "gap", "transparency",
    "transparency_enabled", "match_wallpaper", "background_color", "frosted", "frost_strength", "rounded_corners", "float_mode",
    "bar_rounded_corners", "bar_float_mode", "group_visibility", "group_layouts", "hide_on_fullscreen", "auto_hide",
    "always_show_tray", "always_visible_apps", "magnification", "hover_scale", "compact_bar"];
pub const DESKTOP_FIELDS: &[&str] = &["menu_hotkey_fullscreen", "hot_corners", "hot_corner_delay_ms"];
/// Desktop Layout presets also set the global placement and timing that no
/// single page edits. Pinned applications are personal and never part of one.
pub const LAYOUT_FIELDS: &[&str] = &["edge", "alignment", "hide_delay_ms", "reveal_delay_ms"];
const PREVIEW_REQUIRED: &[&str] = &["transparency", "match_wallpaper", "rounded_corners", "float_mode"];
const PREVIEW_OPTIONAL: &[&str] = &["transparency_enabled", "frosted", "frost_strength"];

pub const XML: &str = r#"<node><interface name="org.tessel.Settings">
<method name="Read"><arg type="s" direction="out"/></method>
<method name="Apply"><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
<method name="Preview"><arg type="s" direction="in"/></method>
<method name="Restore"/>
<method name="GetWorkareas"><arg type="s" direction="out"/></method>
<method name="GetPopupPlacement"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
<method name="GetTrayApps"><arg type="s" direction="out"/></method>
<signal name="WorkareasChanged"><arg type="s"/></signal>
</interface></node>"#;

/// `DOCK_FIELDS` (+ `separate_app_dock`, every `app_dock_*`, the border fields).
pub fn dock_fields() -> BTreeSet<&'static str> {
    let mut fields: BTreeSet<&'static str> = DOCK_BASE_FIELDS.iter().copied().collect();
    fields.extend(field_names().iter().copied().filter(|name| *name == "separate_app_dock" || name.starts_with("app_dock_")));
    fields.extend(BORDER_FIELDS.iter().copied());
    fields
}

/// `SETTINGS_FIELDS` = dock | desktop | appearance | layout (never `pins`).
pub fn settings_fields() -> BTreeSet<&'static str> {
    let mut fields = dock_fields();
    fields.extend(DESKTOP_FIELDS.iter().chain(APPEARANCE_FIELDS).chain(LAYOUT_FIELDS).copied());
    fields
}

/// Python `==` on JSON values (`1 == 1.0 == True`, deep).
pub fn py_value_eq(a: &Value, b: &Value) -> bool {
    fn numeric(value: &Value) -> Option<f64> {
        match value {
            Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
            Value::Number(number) => number.as_f64(),
            _ => None,
        }
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_value_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(key, value)| y.get(key).is_some_and(|other| py_value_eq(value, other)))
        }
        _ => match (numeric(a), numeric(b)) {
            (Some(p), Some(q)) => p == q,
            _ => a == b,
        },
    }
}

fn keys(map: &Map<String, Value>) -> BTreeSet<&str> { map.keys().map(String::as_str).collect() }

/// Merge only edited settings, preserving pins and concurrent other pages.
pub fn apply_patch(current: &DockConfig, request: &Value) -> Result<DockConfig, String> {
    let invalid = || "Invalid settings update.".to_string();
    let request = request.as_object().ok_or_else(invalid)?;
    if keys(request) != BTreeSet::from(["changes", "expected"]) {
        return Err(invalid());
    }
    let (Some(changes), Some(expected)) = (request["changes"].as_object(), request["expected"].as_object()) else { return Err(invalid()) };
    if keys(changes) != keys(expected) {
        return Err(invalid());
    }
    let allowed = settings_fields();
    if changes.keys().any(|key| !allowed.contains(key.as_str())) {
        return Err("This setting cannot be changed here.".into());
    }
    let Value::Object(mut values) = current.to_value() else { return Err(invalid()) };
    if changes.keys().any(|key| !py_value_eq(values.get(key).unwrap_or(&Value::Null), &expected[key])) {
        return Err("Dock settings changed elsewhere. Reset this page before applying again.".into());
    }
    for (key, value) in changes {
        values.insert(key.clone(), value.clone());
    }
    DockConfig::from_value(&Value::Object(values))
}

fn is_int(value: &Value) -> bool { matches!(value, Value::Number(n) if n.is_i64() || n.is_u64()) }

fn int_in(value: &Value, low: i64, high: i64) -> bool {
    is_int(value) && value.as_i64().is_some_and(|n| (low..=high).contains(&n))
}

/// Validate a `Preview` payload; returns the values handed to `preview_appearance`
/// (border fields normalised through `DockConfig::from_value`).
pub fn validate_preview(values: &Value) -> Result<Map<String, Value>, String> {
    let preview = || "Invalid appearance preview.".to_string();
    let mut values = values.as_object().cloned().ok_or_else(preview)?;
    let present = keys(&values);
    if !PREVIEW_REQUIRED.iter().all(|key| present.contains(key))
        || present.iter().any(|key| !PREVIEW_REQUIRED.contains(key) && !PREVIEW_OPTIONAL.contains(key) && !BORDER_FIELDS.contains(key)) {
        return Err(preview());
    }
    if !int_in(&values["transparency"], 0, 100) {
        return Err("Invalid transparency.".into());
    }
    if let Some(strength) = values.get("frost_strength") {
        if !int_in(strength, 0, 100) {
            return Err("Invalid Blur strength.".into());
        }
    }
    if values.iter().any(|(key, value)| key != "transparency" && key != "frost_strength" && !BORDER_FIELDS.contains(&key.as_str()) && !value.is_boolean()) {
        return Err(preview());
    }
    let border: Map<String, Value> = BORDER_FIELDS.iter().filter_map(|key| values.get(*key).map(|v| (key.to_string(), v.clone()))).collect();
    let border = DockConfig::from_value(&Value::Object(border))?.to_value();
    for key in BORDER_FIELDS {
        if values.contains_key(*key) {
            values.insert(key.to_string(), border[*key].clone());
        }
    }
    Ok(values)
}

/// `(x, y, width, height)`.
pub type Rect = (i64, i64, i64, i64);

/// Describe the actual monitor strips reserved by mapped bar sensors.
/// `reservations`: `(monitor, edge, exclusive zone)`.
pub fn workarea_snapshot(monitors: &[(String, Rect)], reservations: &[(String, String, i64)]) -> Value {
    let mut result = Vec::new();
    for (name, geometry) in monitors {
        let (x, y, width, height) = *geometry;
        let mut insets = [("top", 0i64), ("bottom", 0), ("left", 0), ("right", 0)];
        for (monitor, edge, zone) in reservations {
            if monitor == name && *zone > 0 {
                if let Some(entry) = insets.iter_mut().find(|(side, _)| side == edge) {
                    entry.1 += zone;
                }
            }
        }
        let inset = |side: &str| insets.iter().find(|(s, _)| *s == side).unwrap().1;
        let left = width.min(inset("left"));
        let right = (width - left).min(inset("right"));
        let top = height.min(inset("top"));
        let bottom = (height - top).min(inset("bottom"));
        result.push(json!({"name": name, "geometry": [x, y, width, height],
                           "workarea": [x + left, y + top, width - left - right, height - top - bottom]}));
    }
    json!({"version": 1, "monitors": result})
}

pub struct MonitorInfo {
    pub name: String,
    pub label: String,
    pub geometry: Rect,
}

/// One live dock surface, for `GetPopupPlacement` and workareas.
pub struct DockInfo {
    pub monitor_name: String,
    pub is_app_dock: bool,
    pub bar_mode: bool,
    pub config: DockConfig,
    /// `dock.dock_dimensions()` → (width, height).
    pub dimensions: (i64, i64),
    pub horizontal: bool,
    pub edge_gap: i64,
    /// Magnifier headroom.
    pub headroom: i64,
    /// `Gtk4LayerShell.get_exclusive_zone(dock.reservation or dock.sensor)`.
    pub exclusive_zone: i64,
}

/// What the Tessel Dock application provides to the Settings API.
pub trait SettingsHost {
    fn config(&self) -> DockConfig;
    /// `save_config(config, rebuild=)`; false → "The dock settings could not be saved."
    fn save_config(&self, config: DockConfig, rebuild: bool) -> bool;
    /// `preview_appearance(**values)`.
    fn preview_appearance(&self, values: &Map<String, Value>);
    fn restore_appearance(&self);
    /// `tray_entries(tray_backend)`: `[{id,label,running}]` sorted by label casefold.
    fn tray_apps(&self) -> Value;
    /// `monitor_entries()`.
    fn monitors(&self) -> Vec<MonitorInfo>;
    /// `application.docks` (empty before the first build).
    fn docks(&self) -> Vec<DockInfo>;
}

struct Inner {
    connection: gio::DBusConnection,
    host: Rc<dyn SettingsHost>,
    preview_owner: RefCell<Option<String>>,
    workarea_source: RefCell<Option<glib::Source>>,
    last_workareas: RefCell<Option<String>>,
    workareas_override: RefCell<Option<Box<dyn Fn() -> String>>>,
    closed: Cell<bool>,
    registration: RefCell<Option<gio::RegistrationId>>,
    subscription: RefCell<Option<gio::SignalSubscription>>,
}

#[derive(Clone)]
pub struct SettingsApi(Rc<Inner>);

/// `GLib.idle_add` on the thread-default context, for a non-Send closure.
fn idle_once(callback: impl FnOnce() + 'static) -> glib::Source {
    use glib::translate::{from_glib_full, ToGlibPtr};
    type Callback = Box<dyn FnMut() -> bool>;
    unsafe extern "C" fn trampoline(data: glib::ffi::gpointer) -> glib::ffi::gboolean {
        let callback = unsafe { &mut *(data as *mut Callback) };
        callback() as glib::ffi::gboolean
    }
    unsafe extern "C" fn destroy(data: glib::ffi::gpointer) {
        drop(unsafe { Box::from_raw(data as *mut Callback) });
    }
    let mut callback = Some(callback);
    let boxed: Callback = Box::new(move || {
        if let Some(callback) = callback.take() {
            callback();
        }
        false
    });
    unsafe {
        let source: glib::Source = from_glib_full(glib::ffi::g_idle_source_new());
        let data = Box::into_raw(Box::new(boxed));
        glib::ffi::g_source_set_callback(source.to_glib_none().0, Some(trampoline), data as glib::ffi::gpointer, Some(destroy));
        let context = glib::MainContext::ref_thread_default();
        glib::ffi::g_source_attach(source.to_glib_none().0, context.to_glib_none().0);
        source
    }
}

fn dumps(value: &Value) -> String { json_dumps(value, &JsonStyle::DEFAULT) }

impl SettingsApi {
    pub fn new(connection: &gio::DBusConnection, host: Rc<dyn SettingsHost>) -> Result<Self, glib::Error> {
        let api = Self(Rc::new(Inner {
            connection: connection.clone(), host, preview_owner: RefCell::new(None), workarea_source: RefCell::new(None),
            last_workareas: RefCell::new(None), workareas_override: RefCell::new(None), closed: Cell::new(false),
            registration: RefCell::new(None), subscription: RefCell::new(None),
        }));
        let info = gio::DBusNodeInfo::for_xml(XML)?.lookup_interface(INTERFACE).expect("settings interface");
        let weak = api.weak();
        let registration = connection.register_object(OBJECT_PATH, &info)
            .method_call(move |_, sender, _, _, method, params, invocation| {
                match weak.upgrade() {
                    Some(inner) => SettingsApi(inner).call(sender, method, &params, invocation),
                    None => invocation.return_dbus_error(ERROR, "The dock is closing."),
                }
            })
            .build()?;
        api.0.registration.replace(Some(registration));
        let weak = api.weak();
        let subscription = connection.subscribe_to_signal(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"), Some("/org/freedesktop/DBus"), None, gio::DBusSignalFlags::NONE, move |signal| {
                if let (Some(inner), Some((name, old, new))) = (weak.upgrade(), signal.parameters.get::<(String, String, String)>()) {
                    let api = SettingsApi(inner);
                    if !old.is_empty() && new.is_empty() && api.preview_owner().as_deref() == Some(name.as_str()) {
                        api.restore(&name);
                    }
                }
            });
        api.0.subscription.replace(Some(subscription));
        Ok(api)
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    pub fn preview_owner(&self) -> Option<String> { self.0.preview_owner.borrow().clone() }

    /// Tests: replace `workareas()` (Python patches `api.workareas`).
    pub fn set_workareas_override(&self, source: Option<Box<dyn Fn() -> String>>) { self.0.workareas_override.replace(source); }

    pub fn snapshot(&self) -> Value {
        let monitors: Vec<Value> = self.0.host.monitors().into_iter().map(|m| json!({"name": m.name, "label": m.label})).collect();
        json!({"config": self.0.host.config().to_value(), "tray_apps": self.0.host.tray_apps(), "monitors": monitors})
    }

    /// The `GetWorkareas` / `WorkareasChanged` payload (compact JSON).
    pub fn workareas(&self) -> String {
        if let Some(source) = self.0.workareas_override.borrow().as_ref() {
            return source();
        }
        let monitors: Vec<(String, Rect)> = self.0.host.monitors().into_iter().map(|m| (m.name, m.geometry)).collect();
        let reservations: Vec<(String, String, i64)> = self.0.host.docks().into_iter().filter(|d| d.bar_mode)
            .map(|d| (d.monitor_name, d.config.edge, d.exclusive_zone)).collect();
        json_dumps(&workarea_snapshot(&monitors, &reservations), &JsonStyle::COMPACT)
    }

    pub fn popup_placement(&self, monitor: &str, group: &str) -> Result<Value, String> {
        if !LAYOUT_GROUPS.iter().any(|(name, _)| *name == group) {
            return Err("Unknown popup group".into());
        }
        let docks = self.0.host.docks();
        let candidates: Vec<&DockInfo> = docks.iter().filter(|d| d.monitor_name == monitor).collect();
        let dock = candidates.iter().find(|d| d.is_app_dock && APP_DOCK_GROUPS.contains(&group)).or(candidates.first()).copied();
        let config = match dock { Some(dock) => dock.config.clone(), None => self.0.host.config().for_monitor(monitor) };
        let layout = config.group_layouts.get(&config.mode).filter(|layout| !layout.is_empty()).cloned()
            .unwrap_or_else(|| default_layout(&config.mode, &config.application_display));
        let align = layout.iter().find(|(_, groups)| groups.iter().any(|g| g == group)).map(|(position, _)| position.to_owned())
            .unwrap_or_else(|| "end".into());
        let mut offset = 12;
        if let Some(dock) = dock {
            let (width, height) = dock.dimensions;
            offset = (if dock.horizontal { height } else { width }) + dock.edge_gap + dock.headroom + 8;
        }
        Ok(json!({"edge": config.edge, "offset": offset.clamp(12, 4096), "align": align}))
    }

    /// Geometry can settle over multiple GTK allocations: coalesce in an idle
    /// and publish only when the effective rectangles change.
    pub fn queue_workareas_changed(&self) {
        if self.0.closed.get() || self.0.workarea_source.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let source = idle_once(move || {
            if let Some(inner) = weak.upgrade() { SettingsApi(inner).publish_workareas() }
        });
        self.0.workarea_source.replace(Some(source));
    }

    fn publish_workareas(&self) {
        self.0.workarea_source.replace(None);
        let payload = self.workareas();
        if self.0.last_workareas.borrow().as_deref() != Some(payload.as_str()) {
            self.0.last_workareas.replace(Some(payload.clone()));
            let _ = self.0.connection.emit_signal(None, OBJECT_PATH, INTERFACE, "WorkareasChanged", Some(&(payload,).to_variant()));
        }
    }

    fn argument(params: &Variant, index: usize) -> Result<String, String> {
        params.try_child_value(index).and_then(|v| v.get::<String>()).ok_or_else(|| "Invalid settings update.".to_string())
    }

    fn parse(text: &str) -> Result<Value, String> {
        serde_json::from_str(text).map_err(|error| crate::status::backend::python_json_error(text, &error))
    }

    fn handle(&self, sender: Option<&str>, method: &str, params: &Variant) -> Result<Option<Variant>, String> {
        let host = &self.0.host;
        match method {
            "Read" => Ok(Some((dumps(&self.snapshot()),).to_variant())),
            "GetWorkareas" => Ok(Some((self.workareas(),).to_variant())),
            "GetPopupPlacement" => {
                let placement = self.popup_placement(&Self::argument(params, 0)?, &Self::argument(params, 1)?)?;
                Ok(Some((dumps(&placement),).to_variant()))
            }
            "GetTrayApps" => Ok(Some((dumps(&host.tray_apps()),).to_variant())),
            "Apply" => {
                let request = Self::parse(&Self::argument(params, 0)?)?;
                let config = apply_patch(&host.config(), &request)?;
                let rebuild = request["changes"].as_object().is_some_and(|changes| changes.keys()
                    .any(|key| !APPEARANCE_FIELDS.contains(&key.as_str()) && !BORDER_FIELDS.contains(&key.as_str())));
                if !host.save_config(config, rebuild) {
                    return Err("The dock settings could not be saved.".into());
                }
                self.restore(sender.unwrap_or(""));
                Ok(Some((dumps(&self.snapshot()),).to_variant()))
            }
            "Preview" => {
                let values = validate_preview(&Self::parse(&Self::argument(params, 0)?)?)?;
                self.0.preview_owner.replace(sender.map(str::to_owned));
                host.preview_appearance(&values);
                Ok(None)
            }
            "Restore" => {
                self.restore(sender.unwrap_or(""));
                Ok(None)
            }
            _ => Err(format!("Unknown method {method}")),
        }
    }

    fn call(&self, sender: Option<&str>, method: &str, params: &Variant, invocation: gio::DBusMethodInvocation) {
        match self.handle(sender, method, params) {
            Ok(value) => invocation.return_value(value.as_ref()),
            Err(error) => invocation.return_dbus_error(ERROR, &error),
        }
    }

    fn restore(&self, sender: &str) {
        let owner = self.preview_owner();
        if owner.as_deref() == Some(sender) {
            self.0.preview_owner.replace(None);
            self.0.host.restore_appearance();
        }
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        if let Some(source) = self.0.workarea_source.take() {
            source.destroy();
        }
        if let Some(owner) = self.preview_owner() {
            self.restore(&owner);
        }
        self.0.subscription.take();
        if let Some(registration) = self.0.registration.take() {
            let _ = self.0.connection.unregister_object(registration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(values: Value) -> DockConfig { DockConfig::from_value(&values).unwrap() }

    fn patch(current: &DockConfig, changes: Value, expected: Value) -> Result<DockConfig, String> {
        apply_patch(current, &json!({"changes": changes, "expected": expected}))
    }

    #[test]
    fn default_app_visibility_preserves_tray_choices_and_pins() {
        let current = config(json!({"pins": ["browser.desktop"], "always_show_tray": ["id:Nextcloud"]}));
        let result = patch(&current, json!({"always_visible_apps": []}), json!({"always_visible_apps": current.always_visible_apps})).unwrap();
        assert!(result.always_visible_apps.is_empty());
        assert_eq!(result.always_show_tray, current.always_show_tray);
        assert_eq!(result.pins, current.pins);
    }

    #[test]
    fn application_display_updates_without_touching_saved_pin_order() {
        let current = config(json!({"pins": ["browser.desktop", "files.desktop"]}));
        let result = patch(&current, json!({"application_display": "title"}), json!({"application_display": "icons"})).unwrap();
        assert_eq!(result.application_display, "title");
        assert_eq!(result.pins, current.pins);
        assert_eq!(current.application_display, "icons");
        assert!(patch(&current, json!({"application_display": "unknown"}), json!({"application_display": "icons"})).is_err());
    }

    #[test]
    fn changed_fields_merge_without_overwriting_pins_or_other_page() {
        let current = config(json!({"pins": ["org.example.New.desktop"], "hot_corners": {"DP-1": ["top_left"]}}));
        let result = patch(&current, json!({"transparency": 45}), json!({"transparency": 5})).unwrap();
        assert_eq!(result.transparency, 45);
        assert_eq!(result.pins, current.pins);
        assert_eq!(result.hot_corners, current.hot_corners);
        assert_eq!(current.transparency, 5);
    }

    #[test]
    fn conflicting_external_edit_requires_refresh() {
        let error = patch(&config(json!({"transparency": 30})), json!({"transparency": 45}), json!({"transparency": 20})).unwrap_err();
        assert_eq!(error, "Dock settings changed elsewhere. Reset this page before applying again.");
        // Python equality: 30 == 30.0.
        assert!(patch(&config(json!({"transparency": 30})), json!({"transparency": 45}), json!({"transparency": 30.0})).is_ok());
    }

    #[test]
    fn application_transparency_changes_leave_dock_choices_intact() {
        let current = config(json!({"transparency": 80, "transparency_enabled": false}));
        let result = patch(&current, json!({"apps_transparency": 30, "apps_transparency_enabled": true}),
                           json!({"apps_transparency": 5, "apps_transparency_enabled": false})).unwrap();
        assert_eq!(result.transparency, 80);
        assert!(!result.transparency_enabled);
        assert_eq!(result.effective_apps_transparency(), 30);
        let after = patch(&result, json!({"transparency_enabled": true}), json!({"transparency_enabled": false})).unwrap();
        assert_eq!(after.effective_transparency(), 80);
        assert_eq!(after.effective_apps_transparency(), 30);
    }

    #[test]
    fn internal_fields_and_invalid_values_are_rejected() {
        for (field, value, old) in [("pins", json!([]), json!([])), ("auto_hide", json!("yes"), json!(false)),
                                    ("transparency", json!(101), json!(5)), ("mode", json!("invalid"), json!("dock"))] {
            assert!(patch(&DockConfig::default(), json!({field: value}), json!({field: old})).is_err(), "{field}");
        }
        assert_eq!(patch(&DockConfig::default(), json!({"pins": []}), json!({"pins": []})).unwrap_err(), "This setting cannot be changed here.");
        for request in [json!([]), json!({"changes": {}}), json!({"changes": {"gap": 1}, "expected": {}}), json!({"changes": [], "expected": []}),
                        json!({"changes": {}, "expected": {}, "extra": 1})] {
            assert_eq!(apply_patch(&DockConfig::default(), &request).unwrap_err(), "Invalid settings update.", "{request}");
        }
    }

    #[test]
    fn app_material_and_corners_are_independent_from_dock() {
        let before = config(json!({"transparency": 78, "rounded_corners": true, "match_wallpaper": false}));
        let after = patch(&before, json!({"apps_frosted": true, "apps_rounded_corners": false}),
                          json!({"apps_frosted": false, "apps_rounded_corners": true})).unwrap();
        let mut expected = before.to_value();
        expected["apps_frosted"] = json!(true);
        expected["apps_rounded_corners"] = json!(false);
        assert_eq!(after.to_value(), expected);
        for field in ["apps_frosted", "apps_rounded_corners"] {
            let old = before.to_value()[field].clone();
            assert!(patch(&before, json!({field: "yes"}), json!({field: old})).is_err());
        }
    }

    #[test]
    fn strength_changes_preserve_other_material_choices() {
        let before = config(json!({"frosted": true, "frost_strength": 35, "transparency": 24, "apps_frosted": true,
                                   "apps_frost_strength": 60, "apps_transparency": 48}));
        let after = patch(&before, json!({"apps_frost_strength": 75}), json!({"apps_frost_strength": 60})).unwrap();
        let mut expected = before.to_value();
        expected["apps_frost_strength"] = json!(75);
        assert_eq!(after.to_value(), expected);
        for field in ["frost_strength", "apps_frost_strength"] {
            for invalid in [json!(true), json!(-1), json!(101), json!(0.5)] {
                let old = before.to_value()[field].clone();
                assert!(patch(&before, json!({field: invalid}), json!({field: old})).is_err(), "{field} {invalid}");
            }
        }
    }

    #[test]
    fn appearance_toggle_preserves_per_mode_choices() {
        let before = config(json!({"mode": "bar", "float_mode": true, "rounded_corners": true, "bar_float_mode": false,
                                   "bar_rounded_corners": false}));
        let after = patch(&before, json!({"match_wallpaper": true}), json!({"match_wallpaper": false})).unwrap();
        let mut expected = before.to_value();
        expected["match_wallpaper"] = json!(true);
        assert_eq!(after.to_value(), expected);
    }

    #[test]
    fn preview_validation() {
        let base = json!({"transparency": 60, "match_wallpaper": true, "rounded_corners": true, "float_mode": false});
        assert!(validate_preview(&base).is_ok());
        let mut full = base.clone();
        for (key, value) in [("transparency_enabled", json!(false)), ("frosted", json!(true)), ("frost_strength", json!(45)),
                             ("bar_border_enabled", json!(true)), ("bar_border_sides", json!(["right"])), ("bar_border_color", json!("#AABBCC"))] {
            full[key] = value;
        }
        let values = validate_preview(&full).unwrap();
        assert_eq!(values["bar_border_color"], "#aabbcc");
        assert_eq!(values["bar_border_sides"], json!(["right"]));
        let minimal = json!({"transparency": 0, "match_wallpaper": false, "rounded_corners": false, "float_mode": false});
        for (field, invalid) in [("bar_border_enabled", json!(1)), ("bar_border_sides", json!(["bad"])), ("bar_border_sides", json!("top")),
                                 ("bar_border_color", json!("#abcdef;"))] {
            let mut value = minimal.clone();
            value[field] = invalid;
            assert!(validate_preview(&value).is_err(), "{field}");
        }
        for invalid in [json!(true), json!(-1), json!(101), json!(12.5)] {
            let mut value = base.clone();
            value["frost_strength"] = invalid;
            assert_eq!(validate_preview(&value).unwrap_err(), "Invalid Blur strength.");
        }
        let mut value = base.clone();
        value["transparency"] = json!(true);
        assert_eq!(validate_preview(&value).unwrap_err(), "Invalid transparency.");
        let mut value = base.clone();
        value["frosted"] = json!(1);
        assert_eq!(validate_preview(&value).unwrap_err(), "Invalid appearance preview.");
        let mut value = base.clone();
        value["pins"] = json!([]);
        assert_eq!(validate_preview(&value).unwrap_err(), "Invalid appearance preview.");
        assert_eq!(validate_preview(&json!({"transparency": 1})).unwrap_err(), "Invalid appearance preview.");
    }

    #[test]
    fn all_edges_are_in_global_coordinates_and_other_outputs_are_untouched() {
        let geometry = (-1920, 100, 1920, 1080);
        for (edge, expected) in [("top", [-1920, 150, 1920, 1030]), ("bottom", [-1920, 100, 1920, 1030]),
                                 ("left", [-1870, 100, 1870, 1080]), ("right", [-1920, 100, 1870, 1080])] {
            let payload = workarea_snapshot(&[("left".into(), geometry), ("right".into(), (0, 0, 2560, 1440))],
                                            &[("left".into(), edge.into(), 50), ("right".into(), "top".into(), -1)]);
            assert_eq!(payload["monitors"][0]["workarea"], json!(expected));
            assert_eq!(payload["monitors"][1]["workarea"], json!([0, 0, 2560, 1440]));
        }
        let payload = workarea_snapshot(&[("DP-1".into(), (0, 0, 1920, 1080))], &[("DP-1".into(), "top".into(), 48)]);
        assert_eq!(json_dumps(&payload, &JsonStyle::COMPACT),
                   r#"{"version":1,"monitors":[{"name":"DP-1","geometry":[0,0,1920,1080],"workarea":[0,48,1920,1032]}]}"#);
        // Oversized reservations clamp to the monitor.
        let payload = workarea_snapshot(&[("A".into(), (0, 0, 100, 100))], &[("A".into(), "left".into(), 80), ("A".into(), "right".into(), 80)]);
        assert_eq!(payload["monitors"][0]["workarea"], json!([80, 0, 0, 100]));
    }

    #[test]
    fn field_sets_match_python() {
        let fields = settings_fields();
        assert!(!fields.contains("pins"));
        assert!(fields.contains("app_dock_group_layout") && fields.contains("separate_app_dock") && fields.contains("edge"));
        assert!(field_names().iter().all(|name| *name == "pins" || fields.contains(name)), "every other field is patchable");
    }
}
