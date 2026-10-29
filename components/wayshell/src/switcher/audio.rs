//! Port of shared/application_audio.py: on-demand per-application playback
//! mute through WirePlumber's mixer API. libwireplumber-0.5 is `dlopen`ed on
//! first use (same approach as audio-manager's wp.rs), and the core is
//! disconnected when the switcher closes, so no PipeWire proxies live for the
//! whole session. Candidate to share with audio-manager / waydock.

use crate::app_catalog::AppCatalog;
use glib::ffi::{gboolean, gpointer, GError, GType};
use glib::gobject_ffi::GObject;
use glib::prelude::*;
use glib::translate::from_glib_full;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;

pub trait Audio {
    fn start(&self);
    /// (available, muted)
    fn state(&self, app_id: &str) -> (bool, bool);
    fn toggle(&self, app_id: &str) -> bool;
    fn close(&self);
}

/// `application_key(value, catalog)`.
pub fn application_key(value: &str, catalog: &AppCatalog) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let id = catalog.resolve(Some(value), None).map(|a| a.id.clone()).unwrap_or_else(|| value.to_string());
    suite_core::pyjson::casefold(id.strip_suffix(".desktop").unwrap_or(&id))
}

/// `matches_application(app_id, props, catalog)`.
pub fn matches_application(app_id: &str, props: &HashMap<String, String>, catalog: &AppCatalog) -> bool {
    let key = application_key(app_id, catalog);
    if key.is_empty() || props.get("media.class").map(String::as_str) != Some("Stream/Output/Audio") {
        return false;
    }
    let get = |k: &str| props.get(k).filter(|v| !v.is_empty()).cloned();
    if let Some(desktop) = get("application.id").or_else(|| get("pipewire.access.portal.app_id")) {
        return key == application_key(&desktop, catalog);
    }
    let binary = get("application.process.binary").unwrap_or_default();
    let binary = Path::new(&binary).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if ["python", "python3", "wine", "wine64", "node", "java", ""].contains(&binary.as_str()) {
        return false;
    }
    key == application_key(&binary, catalog)
}

/// No audio controls (tests, or when WirePlumber is missing).
pub struct NoAudio;

impl Audio for NoAudio {
    fn start(&self) {}
    fn state(&self, _: &str) -> (bool, bool) {
        (false, false)
    }
    fn toggle(&self, _: &str) -> bool {
        false
    }
    fn close(&self) {}
}

pub fn default_audio(changed: Rc<dyn Fn()>) -> Rc<dyn Audio> {
    ApplicationAudio::new(AppCatalog::new(), changed)
}

// ----------------------------------------------------------------- WirePlumber

type Ptr = *mut c_void;
type ReadyCallback = unsafe extern "C" fn(*mut GObject, *mut gio::ffi::GAsyncResult, gpointer);

macro_rules! wp_api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        struct Api { $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        impl Api {
            unsafe fn load(handle: Ptr) -> Result<Self, String> {
                Ok(Self { $($name: {
                    let symbol = libc::dlsym(handle, concat!(stringify!($name), "\0").as_ptr().cast());
                    if symbol.is_null() {
                        return Err(format!("missing symbol {}", stringify!($name)));
                    }
                    std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(symbol)
                },)* })
            }
        }
    };
}

wp_api! {
    wp_init: fn(c_int);
    wp_core_new: fn(Ptr, Ptr, Ptr) -> *mut GObject;
    wp_core_connect: fn(*mut GObject) -> gboolean;
    wp_core_disconnect: fn(*mut GObject);
    wp_core_install_object_manager: fn(*mut GObject, *mut GObject);
    wp_core_load_component: fn(*mut GObject, *const c_char, *const c_char, Ptr, *const c_char, Ptr, Option<ReadyCallback>, gpointer);
    wp_core_load_component_finish: fn(*mut GObject, *mut gio::ffi::GAsyncResult, *mut *mut GError) -> gboolean;
    wp_object_manager_new: fn() -> *mut GObject;
    wp_object_manager_add_interest_full: fn(*mut GObject, Ptr);
    wp_object_manager_request_object_features: fn(*mut GObject, GType, u32);
    wp_object_interest_new_type: fn(GType) -> Ptr;
    wp_plugin_find: fn(*mut GObject, *const c_char) -> *mut GObject;
    wp_object_activate: fn(*mut GObject, u32, Ptr, Option<ReadyCallback>, gpointer);
    wp_object_activate_finish: fn(*mut GObject, *mut gio::ffi::GAsyncResult, *mut *mut GError) -> gboolean;
    wp_proxy_get_bound_id: fn(*mut GObject) -> u32;
    wp_pipewire_object_get_property: fn(*mut GObject, *const c_char) -> *const c_char;
    wp_properties_new_string: fn(*const c_char) -> Ptr;
    wp_node_get_type: fn() -> GType;
    wp_client_get_type: fn() -> GType;
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let handle = libc::dlopen(c"libwireplumber-0.5.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if handle.is_null() {
            return Err("Namespace Wp not available".into());
        }
        let api = Api::load(handle)?;
        (api.wp_init)(1 | 2); // PIPEWIRE | SPA_TYPES
        Ok(api)
    })
    .as_ref()
    .map_err(Clone::clone)
}

type Completion = Box<dyn FnOnce(*mut GObject, *mut gio::ffi::GAsyncResult)>;

unsafe extern "C" fn ready_trampoline(source: *mut GObject, result: *mut gio::ffi::GAsyncResult, data: gpointer) {
    let completion = Box::from_raw(data as *mut Completion);
    completion(source, result);
}

fn completion(f: impl FnOnce(*mut GObject, *mut gio::ffi::GAsyncResult) + 'static) -> gpointer {
    Box::into_raw(Box::new(Box::new(f) as Completion)) as gpointer
}

unsafe fn take_error(error: *mut GError) -> String {
    let error: glib::Error = from_glib_full(error);
    error.message().to_string()
}

pub struct ApplicationAudio {
    catalog: AppCatalog,
    changed: RefCell<Rc<dyn Fn()>>,
    core: RefCell<Option<glib::Object>>,
    manager: RefCell<Option<glib::Object>>,
    mixer: RefCell<Option<glib::Object>>,
    proxies: RefCell<Vec<(u32, glib::Object)>>,
    ready: Cell<bool>,
    closed: Cell<bool>,
    idle: RefCell<Option<glib::SourceId>>,
    pub error: RefCell<String>,
    me: RefCell<Weak<ApplicationAudio>>,
}

impl ApplicationAudio {
    pub fn new(catalog: AppCatalog, changed: Rc<dyn Fn()>) -> Rc<Self> {
        let audio = Rc::new(Self {
            catalog, changed: RefCell::new(changed), core: RefCell::new(None), manager: RefCell::new(None),
            mixer: RefCell::new(None), proxies: RefCell::default(), ready: Cell::new(false), closed: Cell::new(false),
            idle: RefCell::new(None), error: RefCell::default(), me: RefCell::new(Weak::new()),
        });
        *audio.me.borrow_mut() = Rc::downgrade(&audio);
        audio
    }

    fn notify(&self) {
        if self.closed.get() || self.idle.borrow().is_some() {
            return;
        }
        let weak = self.me.borrow().clone();
        let id = glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.idle.borrow_mut().take();
                if !this.closed.get() {
                    let changed = this.changed.borrow().clone();
                    changed();
                }
            }
        });
        *self.idle.borrow_mut() = Some(id);
    }

    fn try_start(&self) -> Result<(), String> {
        let api = api()?;
        let weak = self.me.borrow().clone();
        unsafe {
            let props = CString::new(r#"application.name = "Window audio controls""#).unwrap();
            let properties = (api.wp_properties_new_string)(props.as_ptr());
            let core: glib::Object = from_glib_full((api.wp_core_new)(std::ptr::null_mut(), std::ptr::null_mut(), properties));
            let w = weak.clone();
            core.connect_local("disconnected", false, move |_| {
                if let Some(this) = w.upgrade() {
                    this.ready.set(false);
                    *this.error.borrow_mut() = "PipeWire is unavailable".into();
                    this.proxies.borrow_mut().clear();
                    this.notify();
                }
                None
            });
            let manager: glib::Object = from_glib_full((api.wp_object_manager_new)());
            for gtype in [(api.wp_node_get_type)(), (api.wp_client_get_type)()] {
                (api.wp_object_manager_add_interest_full)(manager.as_ptr(), (api.wp_object_interest_new_type)(gtype));
                (api.wp_object_manager_request_object_features)(manager.as_ptr(), gtype, 0x11);
            }
            let w = weak.clone();
            manager.connect_local("object-added", false, move |args| {
                let Ok(object) = args[1].get::<glib::Object>() else { return None };
                if let Some(this) = w.upgrade() {
                    if !this.closed.get() {
                        let id = (api.wp_proxy_get_bound_id)(object.as_ptr());
                        let w2 = Rc::downgrade(&this);
                        object.connect_notify_local(Some("info"), move |_, _| {
                            if let Some(t) = w2.upgrade() {
                                t.notify();
                            }
                        });
                        let mut proxies = this.proxies.borrow_mut();
                        proxies.retain(|(i, _)| *i != id);
                        proxies.push((id, object));
                        drop(proxies);
                        this.notify();
                    }
                }
                None
            });
            let w = weak.clone();
            manager.connect_local("object-removed", false, move |args| {
                let Ok(object) = args[1].get::<glib::Object>() else { return None };
                if let Some(this) = w.upgrade() {
                    this.proxies.borrow_mut().retain(|(_, p)| *p != object);
                    this.notify();
                }
                None
            });
            if (api.wp_core_connect)(core.as_ptr()) == 0 {
                return Err("PipeWire is unavailable".into());
            }
            let w = weak.clone();
            let data = completion(move |source, result| {
                let Some(this) = w.upgrade() else { return };
                if this.closed.get() {
                    return;
                }
                let mut error = std::ptr::null_mut();
                if (api.wp_core_load_component_finish)(source, result, &mut error) == 0 {
                    *this.error.borrow_mut() = take_error(error);
                    this.notify();
                    return;
                }
                let plugin = (api.wp_plugin_find)(source, c"mixer-api".as_ptr());
                if plugin.is_null() {
                    *this.error.borrow_mut() = "WirePlumber mixer unavailable".into();
                    this.notify();
                    return;
                }
                let mixer: glib::Object = from_glib_full(plugin);
                let w2 = Rc::downgrade(&this);
                mixer.connect_local("changed", false, move |_| {
                    if let Some(t) = w2.upgrade() {
                        t.notify();
                    }
                    None
                });
                *this.mixer.borrow_mut() = Some(mixer.clone());
                let w3 = Rc::downgrade(&this);
                let data = completion(move |source, result| {
                    let Some(this) = w3.upgrade() else { return };
                    if this.closed.get() {
                        return;
                    }
                    let mut error = std::ptr::null_mut();
                    if (api.wp_object_activate_finish)(source, result, &mut error) != 0 {
                        this.ready.set(true);
                    } else {
                        *this.error.borrow_mut() = take_error(error);
                    }
                    this.notify();
                });
                (api.wp_object_activate)(mixer.as_ptr(), 1, std::ptr::null_mut(), Some(ready_trampoline), data);
            });
            (api.wp_core_load_component)(core.as_ptr(), c"libwireplumber-module-mixer-api".as_ptr(), c"module".as_ptr(),
                                         std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), Some(ready_trampoline), data);
            (api.wp_core_install_object_manager)(core.as_ptr(), manager.as_ptr());
            *self.core.borrow_mut() = Some(core);
            *self.manager.borrow_mut() = Some(manager);
        }
        Ok(())
    }

    fn property(object: &glib::Object, key: &str) -> Option<String> {
        let api = api().ok()?;
        let key = CString::new(key).ok()?;
        let value = unsafe { (api.wp_pipewire_object_get_property)(object.as_ptr(), key.as_ptr()) };
        (!value.is_null()).then(|| unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned())
    }

    fn props(&self, proxy: &glib::Object) -> HashMap<String, String> {
        const KEYS: [&str; 5] = ["media.class", "application.id", "pipewire.access.portal.app_id", "application.process.binary", "client.id"];
        let mut props: HashMap<String, String> = KEYS.iter().map(|k| (k.to_string(), Self::property(proxy, k).unwrap_or_default())).collect();
        let client_id = props.get("client.id").and_then(|v| v.trim().parse::<u32>().ok());
        let client = client_id.and_then(|id| self.proxies.borrow().iter().find(|(i, _)| *i == id).map(|(_, p)| p.clone()));
        if let Some(client) = client {
            for key in &KEYS[1..4] {
                if props.get(*key).is_none_or(|v| v.is_empty()) {
                    props.insert(key.to_string(), Self::property(&client, key).unwrap_or_default());
                }
            }
        }
        props
    }

    fn streams(&self, app_id: &str) -> Vec<(u32, glib::Object, bool)> {
        if !self.ready.get() || self.closed.get() {
            return Vec::new();
        }
        let (Ok(api), Some(mixer)) = (api(), self.mixer.borrow().clone()) else { return Vec::new() };
        let node_type: glib::Type = unsafe { glib::translate::from_glib((api.wp_node_get_type)()) };
        let proxies = self.proxies.borrow().clone();
        let mut result = Vec::new();
        for (id, proxy) in proxies {
            if proxy.type_().is_a(node_type) && matches_application(app_id, &self.props(&proxy), &self.catalog) {
                if let Some(volume) = mixer.emit_by_name::<Option<glib::Variant>>("get-volume", &[&id]) {
                    let dict = glib::VariantDict::new(Some(&volume));
                    let muted = dict.lookup::<bool>("mute").ok().flatten().unwrap_or(false);
                    result.push((id, proxy, muted));
                }
            }
        }
        result
    }
}

impl Audio for ApplicationAudio {
    fn start(&self) {
        if self.core.borrow().is_some() || self.closed.get() {
            return;
        }
        if let Err(error) = self.try_start() {
            *self.error.borrow_mut() = error;
            self.notify();
        }
    }

    fn state(&self, app_id: &str) -> (bool, bool) {
        let streams = self.streams(app_id);
        let available = !streams.is_empty();
        (available, available && streams.iter().all(|(_, _, m)| *m))
    }

    fn toggle(&self, app_id: &str) -> bool {
        let streams = self.streams(app_id);
        let muted = !streams.iter().all(|(_, _, m)| *m);
        let mut success = !streams.is_empty();
        let mixer = self.mixer.borrow().clone();
        if let (Some(mixer), Ok(api)) = (mixer, api()) {
            for (id, proxy, _) in &streams {
                // A removed/reused PipeWire ID must never mute a different client.
                let current = self.proxies.borrow().iter().any(|(i, p)| i == id && p == proxy);
                if current && unsafe { (api.wp_proxy_get_bound_id)(proxy.as_ptr()) } == *id {
                    let dict = glib::VariantDict::new(None);
                    dict.insert_value("mute", &muted.to_variant());
                    success = mixer.emit_by_name::<bool>("set-volume", &[id, &dict.end()]) && success;
                }
            }
        }
        self.notify();
        success
    }

    fn close(&self) {
        self.closed.set(true);
        if let Some(source) = self.idle.borrow_mut().take() {
            source.remove();
        }
        self.proxies.borrow_mut().clear();
        self.manager.borrow_mut().take();
        self.mixer.borrow_mut().take();
        if let Some(core) = self.core.borrow_mut().take() {
            if let Ok(api) = api() {
                unsafe { (api.wp_core_disconnect)(core.as_ptr()) };
            }
        }
        *self.changed.borrow_mut() = Rc::new(|| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_catalog::InfoRecord;

    fn catalog() -> AppCatalog {
        AppCatalog::with_infos(vec![
            InfoRecord { id: "org.mozilla.firefox.desktop".into(), name: "Firefox".into(), startup_wm_class: "firefox".into(), visible: true, ..Default::default() },
            InfoRecord { id: "mpv.desktop".into(), name: "mpv".into(), visible: true, ..Default::default() },
        ])
    }

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        let mut map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        map.entry("media.class".into()).or_insert_with(|| "Stream/Output/Audio".into());
        map
    }

    #[test]
    fn desktop_ids_are_authoritative_and_interpreters_never_match() {
        let c = catalog();
        assert!(matches_application("firefox", &props(&[("application.id", "org.mozilla.firefox")]), &c));
        assert!(!matches_application("mpv", &props(&[("application.id", "org.mozilla.firefox")]), &c));
        assert!(matches_application("org.mozilla.firefox", &props(&[("pipewire.access.portal.app_id", "org.mozilla.firefox")]), &c));
        assert!(matches_application("mpv", &props(&[("application.process.binary", "/usr/bin/mpv")]), &c));
        assert!(!matches_application("python3", &props(&[("application.process.binary", "python3")]), &c));
        assert!(!matches_application("mpv", &props(&[("media.class", "Stream/Input/Audio"), ("application.process.binary", "mpv")]), &c));
        assert!(!matches_application("", &props(&[("application.process.binary", "mpv")]), &c));
        assert_eq!(application_key(" Firefox ", &c), "org.mozilla.firefox");
    }
}
