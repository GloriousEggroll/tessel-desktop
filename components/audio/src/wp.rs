//! The real WirePlumber 0.5 connector (audio_backend.py:298-406, 680-756).
//!
//! libwireplumber is opened with `dlopen` on first use (no -devel package or
//! build-time link is needed, and processes that never connect never load it).
//! Its objects are GObjects, so signals, properties and the mixer/default-nodes
//! action signals go through glib-rs.

use crate::backend::{Connection, Connector, Event, EventSink, MixerVolume, NativeDefaults, NativeMixer, NativeProxy, ParamValue, PendingValue, PendingValues, Plugin};
use glib::ffi::{gboolean, gpointer, GError, GType};
use glib::gobject_ffi::GObject;
use glib::prelude::*;
use glib::translate::{from_glib, from_glib_full};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::rc::Rc;
use std::sync::OnceLock;

type Ptr = *mut c_void;
type ReadyCallback = unsafe extern "C" fn(*mut GObject, *mut gio::ffi::GAsyncResult, gpointer);

/// `str(GLib.Error)` as PyGObject prints it.
pub fn gerror_str(error: &glib::Error) -> String {
    format!("{}: {} ({})", error.domain().as_str(), error.message(), error_code(error))
}

fn error_code(error: &glib::Error) -> i32 {
    error.code()
}

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
    wp_pipewire_object_set_param: fn(*mut GObject, *const c_char, u32, Ptr) -> gboolean;
    wp_global_proxy_get_global_properties: fn(*mut GObject) -> Ptr;
    wp_properties_new_string: fn(*const c_char) -> Ptr;
    wp_properties_get: fn(Ptr, *const c_char) -> *const c_char;
    wp_properties_unref: fn(Ptr);
    wp_metadata_set: fn(*mut GObject, u32, *const c_char, *const c_char, *const c_char);
    wp_spa_pod_builder_new_object: fn(*const c_char, *const c_char) -> Ptr;
    wp_spa_pod_builder_add_property: fn(Ptr, *const c_char);
    wp_spa_pod_builder_add_boolean: fn(Ptr, gboolean);
    wp_spa_pod_builder_add_int: fn(Ptr, i32);
    wp_spa_pod_builder_end: fn(Ptr) -> Ptr;
    wp_spa_pod_builder_unref: fn(Ptr);
    wp_node_get_type: fn() -> GType;
    wp_device_get_type: fn() -> GType;
    wp_link_get_type: fn() -> GType;
    wp_metadata_get_type: fn() -> GType;
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let handle = libc::dlopen(c"libwireplumber-0.5.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if handle.is_null() {
            let reason = CStr::from_ptr(libc::dlerror()).to_string_lossy().into_owned();
            return Err(format!("WirePlumber 0.5 is unavailable: {reason}"));
        }
        let api = Api::load(handle)?;
        (api.wp_init)(1 | 2); // WP_INIT_PIPEWIRE | WP_INIT_SPA_TYPES
        Ok(api)
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn cstring(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap()
}

/// Box an async completion for a C GAsyncReadyCallback.
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
    gerror_str(&error)
}

struct WpProxy(glib::Object);

impl NativeProxy for WpProxy {
    fn bound_id(&self) -> i64 {
        api().map_or(0, |api| unsafe { (api.wp_proxy_get_bound_id)(self.0.as_ptr()) } as i64)
    }
    fn property(&self, key: &str) -> Option<String> {
        let api = api().ok()?;
        let key = cstring(key);
        let value = unsafe { (api.wp_pipewire_object_get_property)(self.0.as_ptr(), key.as_ptr()) };
        (!value.is_null()).then(|| unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned())
    }
    fn set_param(&self, id: &str, flags: u32, values: &[(&str, ParamValue)]) -> bool {
        let Ok(api) = api() else { return false };
        unsafe {
            let type_name = cstring(&format!("Spa:Pod:Object:Param:{id}"));
            let id_name = cstring(id);
            let builder = (api.wp_spa_pod_builder_new_object)(type_name.as_ptr(), id_name.as_ptr());
            if builder.is_null() {
                return false;
            }
            for (key, value) in values {
                let key = cstring(key);
                (api.wp_spa_pod_builder_add_property)(builder, key.as_ptr());
                match value {
                    ParamValue::Bool(flag) => (api.wp_spa_pod_builder_add_boolean)(builder, *flag as gboolean),
                    ParamValue::Int(int) => (api.wp_spa_pod_builder_add_int)(builder, *int as i32),
                }
            }
            let pod = (api.wp_spa_pod_builder_end)(builder);
            (api.wp_spa_pod_builder_unref)(builder);
            if pod.is_null() {
                return false;
            }
            // set_param takes ownership of the pod (transfer full).
            (api.wp_pipewire_object_set_param)(self.0.as_ptr(), id_name.as_ptr(), flags, pod) != 0
        }
    }
    fn metadata_set(&self, subject: i64, key: &str, kind: &str, value: &str) {
        let Ok(api) = api() else { return };
        let (key, kind, value) = (cstring(key), cstring(kind), cstring(value));
        unsafe { (api.wp_metadata_set)(self.0.as_ptr(), subject as u32, key.as_ptr(), kind.as_ptr(), value.as_ptr()) };
    }
    fn key(&self) -> usize {
        self.0.as_ptr() as usize
    }
}

struct WpMixer(glib::Object);

fn variant_float(value: &glib::Variant) -> Option<f64> {
    value.get::<f64>().or_else(|| value.get::<i32>().map(f64::from))
}

impl NativeMixer for WpMixer {
    fn get_volume(&self, id: i64) -> Option<MixerVolume> {
        let data = self.0.emit_by_name::<Option<glib::Variant>>("get-volume", &[&(id as u32)])?;
        let dict = glib::VariantDict::new(Some(&data));
        let mut volume = MixerVolume {
            volume: dict.lookup_value("volume", None).as_ref().and_then(variant_float),
            mute: dict.lookup_value("mute", None).and_then(|v| v.get::<bool>()),
            channels: Vec::new(),
        };
        if let Some(channels) = dict.lookup_value("channelVolumes", None) {
            for entry in channels.iter() {
                let key = entry.child_value(0).str().unwrap_or_default().to_owned();
                let inner = entry.child_value(1).as_variant().unwrap_or(entry.child_value(1));
                let channel = glib::VariantDict::new(Some(&inner));
                volume.channels.push((
                    key,
                    channel.lookup_value("volume", None).as_ref().and_then(variant_float),
                    channel.lookup_value("channel", None).and_then(|v| v.str().map(str::to_owned)),
                ));
            }
        }
        Some(volume)
    }
    fn set_volume(&self, id: i64, values: &PendingValues) -> bool {
        let dict = glib::VariantDict::new(None);
        for (key, value) in values {
            match value {
                PendingValue::Volume(v) => dict.insert_value(key, &v.to_variant()),
                PendingValue::Mute(m) => dict.insert_value(key, &m.to_variant()),
                PendingValue::Channels(channels) => {
                    let all = glib::VariantDict::new(None);
                    for (index, v) in channels {
                        let channel = glib::VariantDict::new(None);
                        channel.insert_value("volume", &v.to_variant());
                        all.insert_value(index, &channel.end());
                    }
                    dict.insert_value(key, &all.end());
                }
            }
        }
        self.0.emit_by_name::<bool>("set-volume", &[&(id as u32), &dict.end()])
    }
}

struct WpDefaults(glib::Object);

impl NativeDefaults for WpDefaults {
    fn set_default_configured_node_name(&self, media_class: &str, name: &str) -> bool {
        self.0.emit_by_name::<bool>("set-default-configured-node-name", &[&media_class, &name])
    }
}

struct WpConnection {
    core: glib::Object,
    _manager: glib::Object,
}

impl Connection for WpConnection {
    fn disconnect(&self) {
        if let Ok(api) = api() {
            unsafe { (api.wp_core_disconnect)(self.core.as_ptr()) };
        }
    }
}

pub struct WpConnector;

impl Connector for WpConnector {
    fn connect(&self, events: EventSink) -> Result<Box<dyn Connection>, String> {
        let api = api()?;
        unsafe {
            let props = cstring(r#"application.name = "Audio Settings" application.icon-name = "audio-volume-high-symbolic""#);
            let properties = (api.wp_properties_new_string)(props.as_ptr());
            let core: glib::Object = from_glib_full((api.wp_core_new)(std::ptr::null_mut(), std::ptr::null_mut(), properties));
            let sink = events.clone();
            core.connect_local("disconnected", false, move |_| {
                sink(Event::Disconnected);
                None
            });
            let manager: glib::Object = from_glib_full((api.wp_object_manager_new)());
            for (gtype, features) in [((api.wp_node_get_type)(), 0x11), ((api.wp_device_get_type)(), 0x291),
                                      ((api.wp_link_get_type)(), 0x11), ((api.wp_metadata_get_type)(), 0x10011)] {
                (api.wp_object_manager_add_interest_full)(manager.as_ptr(), (api.wp_object_interest_new_type)(gtype));
                (api.wp_object_manager_request_object_features)(manager.as_ptr(), gtype, features);
            }
            let metadata_type: glib::Type = from_glib((api.wp_metadata_get_type)());
            let sink = events.clone();
            manager.connect_local("object-added", false, move |args| {
                let Ok(object) = args[1].get::<glib::Object>() else { return None };
                let mut default = false;
                if object.type_().is_a(metadata_type) {
                    let props = (api.wp_global_proxy_get_global_properties)(object.as_ptr());
                    if !props.is_null() {
                        let value = (api.wp_properties_get)(props, c"metadata.name".as_ptr());
                        default = !value.is_null() && CStr::from_ptr(value).to_bytes() == b"default";
                        (api.wp_properties_unref)(props);
                    }
                }
                sink(Event::ObjectAdded(Rc::new(WpProxy(object)), default));
                None
            });
            let sink = events.clone();
            manager.connect_local("object-removed", false, move |args| {
                if let Ok(object) = args[1].get::<glib::Object>() {
                    sink(Event::ObjectRemoved(object.as_ptr() as usize));
                }
                None
            });
            let sink = events.clone();
            manager.connect_local("installed", false, move |_| {
                sink(Event::Installed);
                None
            });
            if (api.wp_core_connect)(core.as_ptr()) == 0 {
                return Err("PipeWire is not available. Check that PipeWire and WirePlumber are running.".into());
            }
            for name in ["mixer-api", "default-nodes-api"] {
                let component = cstring(&format!("libwireplumber-module-{name}"));
                let sink = events.clone();
                let data = completion(move |source, result| load_finished(api, source, result, name, sink));
                (api.wp_core_load_component)(core.as_ptr(), component.as_ptr(), c"module".as_ptr(), std::ptr::null_mut(),
                                             std::ptr::null(), std::ptr::null_mut(), Some(ready_trampoline), data);
            }
            (api.wp_core_install_object_manager)(core.as_ptr(), manager.as_ptr());
            Ok(Box::new(WpConnection { core, _manager: manager }))
        }
    }
}

unsafe fn load_finished(api: &'static Api, core: *mut GObject, result: *mut gio::ffi::GAsyncResult, name: &'static str, sink: EventSink) {
    let mut error = std::ptr::null_mut();
    if (api.wp_core_load_component_finish)(core, result, &mut error) == 0 {
        sink(Event::ComponentLoaded(name, Err(take_error(error))));
        return;
    }
    let plugin_name = cstring(name);
    let plugin = (api.wp_plugin_find)(core, plugin_name.as_ptr());
    if plugin.is_null() {
        sink(Event::ComponentLoaded(name, Err(format!("WirePlumber {name} module is unavailable."))));
        return;
    }
    let plugin: glib::Object = from_glib_full(plugin);
    let loaded = if name == "mixer-api" {
        // scale = 1: cubic, matching desktop volume percentages.
        if let Some(pspec) = plugin.find_property("scale") {
            let value = glib::EnumClass::with_type(pspec.value_type()).and_then(|class| class.to_value(1));
            if let Some(value) = value {
                plugin.set_property_from_value("scale", &value);
            }
        }
        let changed = sink.clone();
        plugin.connect_local("changed", false, move |args| {
            if let Ok(id) = args[1].get::<u32>() {
                changed(Event::MixerChanged(id as i64));
            }
            None
        });
        Plugin::Mixer(Rc::new(WpMixer(plugin.clone())))
    } else {
        let changed = sink.clone();
        plugin.connect_local("changed", false, move |_| {
            changed(Event::DefaultsChanged);
            None
        });
        Plugin::Defaults(Rc::new(WpDefaults(plugin.clone())))
    };
    sink(Event::ComponentLoaded(name, Ok(loaded)));
    let activated = sink.clone();
    let data = completion(move |source, result| {
        let mut error = std::ptr::null_mut();
        let outcome = if (api.wp_object_activate_finish)(source, result, &mut error) != 0 { Ok(()) } else { Err(take_error(error)) };
        activated(Event::PluginActivated(name, outcome));
    });
    (api.wp_object_activate)(plugin.as_ptr(), 1, std::ptr::null_mut(), Some(ready_trampoline), data);
}
