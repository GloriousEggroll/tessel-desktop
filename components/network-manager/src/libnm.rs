//! Minimal C FFI to libnm (`libnm.so.0`, package `NetworkManager-libnm`),
//! loaded with `dlopen` on first use so the status stream and the read-only
//! backend never need it. Used only for libnm-only connection logic that has
//! no D-Bus equivalent: `normalize()/verify()/verify_secrets()/need_secrets()`,
//! `update_secrets()`, setting GObject introspection for the generic editor,
//! secret-flag accessors, IP address/route/rule and WireGuard peer boxed types,
//! `nm_utils_*` validators and VPN plugin discovery / editor-plugin
//! import/export. No `NMClient` is ever created: NetworkManager itself is
//! reached only through raw D-Bus (`backend.rs`, `agent.rs`).

#![allow(non_camel_case_types, clippy::missing_safety_doc)]

use glib::translate::*;
use glib::{ffi as gffi, gobject_ffi as gobj, Variant};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::ptr;
use std::sync::OnceLock;

pub type GType = glib::ffi::GType;
type B = gffi::gboolean;
type Obj = *mut gobj::GObject;
type Err = *mut *mut gffi::GError;

macro_rules! nm_api {
    ($($name:ident : fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        pub struct Api { $(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        impl Api {
            unsafe fn load(handle: *mut c_void) -> Option<Self> {
                Some(Self { $($name: {
                    let symbol = CString::new(stringify!($name)).unwrap();
                    let address = libc::dlsym(handle, symbol.as_ptr());
                    if address.is_null() { return None; }
                    std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(address)
                },)* })
            }
        }
    };
}

nm_api! {
    nm_simple_connection_new: fn() -> Obj;
    nm_simple_connection_new_from_dbus: fn(*mut gffi::GVariant, Err) -> Obj;
    nm_simple_connection_new_clone: fn(Obj) -> Obj;
    nm_connection_to_dbus: fn(Obj, c_uint) -> *mut gffi::GVariant;
    nm_connection_normalize: fn(Obj, *mut gffi::GHashTable, *mut B, Err) -> B;
    nm_connection_verify: fn(Obj, Err) -> B;
    nm_connection_verify_secrets: fn(Obj, Err) -> B;
    nm_connection_need_secrets: fn(Obj, *mut *mut gffi::GPtrArray) -> *const c_char;
    nm_connection_update_secrets: fn(Obj, *const c_char, *mut gffi::GVariant, Err) -> B;
    nm_connection_clear_secrets: fn(Obj);
    nm_connection_get_setting_by_name: fn(Obj, *const c_char) -> Obj;
    nm_connection_get_settings: fn(Obj, *mut c_uint) -> *mut Obj;
    nm_connection_add_setting: fn(Obj, Obj);
    nm_connection_remove_setting: fn(Obj, GType);
    nm_connection_get_uuid: fn(Obj) -> *const c_char;
    nm_connection_get_id: fn(Obj) -> *const c_char;
    nm_connection_get_connection_type: fn(Obj) -> *const c_char;
    nm_connection_get_path: fn(Obj) -> *const c_char;
    nm_connection_set_path: fn(Obj, *const c_char);
    nm_setting_get_name: fn(Obj) -> *const c_char;
    nm_setting_lookup_type: fn(*const c_char) -> GType;
    nm_setting_get_dbus_property_type: fn(Obj, *const c_char) -> *const gffi::GVariantType;
    nm_setting_get_secret_flags: fn(Obj, *const c_char, *mut c_uint, Err) -> B;
    nm_setting_set_secret_flags: fn(Obj, *const c_char, c_uint, Err) -> B;
    nm_setting_duplicate: fn(Obj) -> Obj;
    nm_setting_verify: fn(Obj, Obj, Err) -> B;
    nm_setting_option_get_all_names: fn(Obj, *mut c_uint) -> *const *const c_char;
    nm_setting_option_get: fn(Obj, *const c_char) -> *mut gffi::GVariant;
    nm_setting_option_set: fn(Obj, *const c_char, *mut gffi::GVariant);
    nm_ethtool_optname_is_feature: fn(*const c_char) -> B;
    nm_ethtool_optname_is_pause: fn(*const c_char) -> B;
    nm_ethtool_optname_is_eee: fn(*const c_char) -> B;
    nm_setting_vpn_get_service_type: fn(Obj) -> *const c_char;
    nm_setting_vpn_get_data_keys: fn(Obj, *mut c_uint) -> *mut *const c_char;
    nm_setting_vpn_get_data_item: fn(Obj, *const c_char) -> *const c_char;
    nm_setting_vpn_add_data_item: fn(Obj, *const c_char, *const c_char);
    nm_setting_vpn_remove_data_item: fn(Obj, *const c_char) -> B;
    nm_setting_vpn_get_secret_keys: fn(Obj, *mut c_uint) -> *mut *const c_char;
    nm_setting_vpn_get_secret: fn(Obj, *const c_char) -> *const c_char;
    nm_setting_vpn_add_secret: fn(Obj, *const c_char, *const c_char);
    nm_setting_vpn_remove_secret: fn(Obj, *const c_char) -> B;
    nm_setting_wireguard_get_peers_len: fn(Obj) -> c_uint;
    nm_setting_wireguard_get_peer: fn(Obj, c_uint) -> *mut c_void;
    nm_setting_wireguard_set_peer: fn(Obj, *mut c_void, c_uint);
    nm_setting_wireguard_append_peer: fn(Obj, *mut c_void);
    nm_setting_wireguard_clear_peers: fn(Obj) -> c_uint;
    nm_wireguard_peer_new: fn() -> *mut c_void;
    nm_wireguard_peer_new_clone: fn(*const c_void, B) -> *mut c_void;
    nm_wireguard_peer_unref: fn(*mut c_void);
    nm_wireguard_peer_get_public_key: fn(*const c_void) -> *const c_char;
    nm_wireguard_peer_set_public_key: fn(*mut c_void, *const c_char, B) -> B;
    nm_wireguard_peer_get_endpoint: fn(*const c_void) -> *const c_char;
    nm_wireguard_peer_set_endpoint: fn(*mut c_void, *const c_char, B) -> B;
    nm_wireguard_peer_get_allowed_ips_len: fn(*const c_void) -> c_uint;
    nm_wireguard_peer_get_allowed_ip: fn(*const c_void, c_uint, *mut B) -> *const c_char;
    nm_wireguard_peer_clear_allowed_ips: fn(*mut c_void);
    nm_wireguard_peer_append_allowed_ip: fn(*mut c_void, *const c_char, B) -> B;
    nm_wireguard_peer_get_persistent_keepalive: fn(*const c_void) -> u16;
    nm_wireguard_peer_set_persistent_keepalive: fn(*mut c_void, u16);
    nm_wireguard_peer_get_preshared_key: fn(*const c_void) -> *const c_char;
    nm_wireguard_peer_set_preshared_key: fn(*mut c_void, *const c_char, B) -> B;
    nm_wireguard_peer_get_preshared_key_flags: fn(*const c_void) -> c_uint;
    nm_wireguard_peer_set_preshared_key_flags: fn(*mut c_void, c_uint);
    nm_setting_ip_config_get_method: fn(Obj) -> *const c_char;
    nm_setting_ip_config_get_num_addresses: fn(Obj) -> c_uint;
    nm_setting_ip_config_get_address: fn(Obj, c_int) -> *mut c_void;
    nm_setting_ip_config_clear_addresses: fn(Obj);
    nm_setting_ip_config_add_address: fn(Obj, *mut c_void) -> B;
    nm_setting_ip_config_get_num_routes: fn(Obj) -> c_uint;
    nm_setting_ip_config_get_route: fn(Obj, c_int) -> *mut c_void;
    nm_setting_ip_config_clear_routes: fn(Obj);
    nm_setting_ip_config_add_route: fn(Obj, *mut c_void) -> B;
    nm_setting_ip_config_get_num_routing_rules: fn(Obj) -> c_uint;
    nm_setting_ip_config_get_routing_rule: fn(Obj, c_uint) -> *mut c_void;
    nm_setting_ip_config_clear_routing_rules: fn(Obj);
    nm_setting_ip_config_add_routing_rule: fn(Obj, *mut c_void);
    nm_ip_address_new: fn(c_int, *const c_char, c_uint, Err) -> *mut c_void;
    nm_ip_address_dup: fn(*mut c_void) -> *mut c_void;
    nm_ip_address_unref: fn(*mut c_void);
    nm_ip_address_get_address: fn(*mut c_void) -> *const c_char;
    nm_ip_address_get_prefix: fn(*mut c_void) -> c_uint;
    nm_ip_address_set_address: fn(*mut c_void, *const c_char);
    nm_ip_address_set_prefix: fn(*mut c_void, c_uint);
    nm_ip_address_get_attribute_names: fn(*mut c_void) -> *mut *mut c_char;
    nm_ip_address_get_attribute: fn(*mut c_void, *const c_char) -> *mut gffi::GVariant;
    nm_ip_address_set_attribute: fn(*mut c_void, *const c_char, *mut gffi::GVariant);
    nm_ip_route_new: fn(c_int, *const c_char, c_uint, *const c_char, i64, Err) -> *mut c_void;
    nm_ip_route_dup: fn(*mut c_void) -> *mut c_void;
    nm_ip_route_unref: fn(*mut c_void);
    nm_ip_route_get_dest: fn(*mut c_void) -> *const c_char;
    nm_ip_route_get_prefix: fn(*mut c_void) -> c_uint;
    nm_ip_route_get_next_hop: fn(*mut c_void) -> *const c_char;
    nm_ip_route_get_metric: fn(*mut c_void) -> i64;
    nm_ip_route_set_dest: fn(*mut c_void, *const c_char);
    nm_ip_route_set_prefix: fn(*mut c_void, c_uint);
    nm_ip_route_set_next_hop: fn(*mut c_void, *const c_char);
    nm_ip_route_set_metric: fn(*mut c_void, i64);
    nm_ip_route_get_attribute_names: fn(*mut c_void) -> *mut *mut c_char;
    nm_ip_route_get_attribute: fn(*mut c_void, *const c_char) -> *mut gffi::GVariant;
    nm_ip_route_set_attribute: fn(*mut c_void, *const c_char, *mut gffi::GVariant);
    nm_ip_route_attribute_validate: fn(*const c_char, *mut gffi::GVariant, c_int, *mut B, Err) -> B;
    nm_ip_routing_rule_from_string: fn(*const c_char, c_uint, *mut gffi::GHashTable, Err) -> *mut c_void;
    nm_ip_routing_rule_to_string: fn(*const c_void, c_uint, *mut gffi::GHashTable, Err) -> *mut c_char;
    nm_ip_routing_rule_get_addr_family: fn(*const c_void) -> c_int;
    nm_ip_routing_rule_unref: fn(*mut c_void);
    nm_utils_wpa_psk_valid: fn(*const c_char) -> B;
    nm_utils_wep_key_valid: fn(*const c_char, c_uint) -> B;
    nm_utils_ssid_to_utf8: fn(*const u8, usize) -> *mut c_char;
    nm_utils_uuid_generate: fn() -> *mut c_char;
    nm_vpn_plugin_info_list_load: fn() -> *mut gffi::GSList;
    nm_vpn_plugin_info_new_search_file: fn(*const c_char, *const c_char) -> Obj;
    nm_vpn_plugin_info_get_service: fn(Obj) -> *const c_char;
    nm_vpn_plugin_info_get_name: fn(Obj) -> *const c_char;
    nm_vpn_plugin_info_get_auth_dialog: fn(Obj) -> *const c_char;
    nm_vpn_plugin_info_supports_hints: fn(Obj) -> B;
    nm_vpn_plugin_info_lookup_property: fn(Obj, *const c_char, *const c_char) -> *const c_char;
    nm_vpn_plugin_info_load_editor_plugin: fn(Obj, Err) -> Obj;
    nm_vpn_editor_plugin_get_capabilities: fn(Obj) -> c_uint;
    nm_vpn_editor_plugin_import: fn(Obj, *const c_char, Err) -> Obj;
    nm_vpn_editor_plugin_export: fn(Obj, *const c_char, Obj, Err) -> B;
}

static API: OnceLock<Option<Api>> = OnceLock::new();

/// The loaded library, or `None` when `libnm.so.0` (or a symbol) is missing.
pub fn api() -> Option<&'static Api> {
    API.get_or_init(|| unsafe {
        let name = CString::new(std::env::var("NETWORK_MANAGER_LIBNM").unwrap_or_else(|_| "libnm.so.0".into())).ok()?;
        let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if handle.is_null() {
            return None;
        }
        Api::load(handle)
    }).as_ref()
}

pub fn available() -> bool {
    api().is_some()
}

fn nm() -> Result<&'static Api, glib::Error> {
    api().ok_or_else(|| glib::Error::new(gio::IOErrorEnum::NotSupported, "NetworkManager's libnm (libnm.so.0) is not installed."))
}

pub const WITH_NON_SECRET: u32 = 1;
pub const WITH_SECRETS: u32 = 2;
pub const ALL: u32 = 0;
pub const AGENT_OWNED: u32 = 1;
pub const NOT_SAVED: u32 = 2;
pub const PARAM_REQUIRED: u32 = 1 << 9;
pub const PARAM_SECRET: u32 = 1 << 10;
pub const AF_INET: i32 = libc::AF_INET;
pub const AF_INET6: i32 = libc::AF_INET6;
pub const RULE_AF_INET: u32 = 1;
pub const RULE_AF_INET6: u32 = 2;
pub const RULE_VALIDATE: u32 = 4;
pub const VPN_IMPORT: u32 = 1;
pub const VPN_EXPORT: u32 = 2;

fn cstr(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap()
}

unsafe fn opt_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() { None } else { Some(CStr::from_ptr(ptr).to_string_lossy().into_owned()) }
}

unsafe fn take_error(error: *mut gffi::GError) -> glib::Error {
    if error.is_null() {
        glib::Error::new(gio::IOErrorEnum::Failed, "The connection settings are invalid.")
    } else {
        from_glib_full(error)
    }
}

unsafe fn object(ptr: Obj, full: bool) -> Option<glib::Object> {
    if ptr.is_null() {
        None
    } else if full {
        Some(from_glib_full(ptr))
    } else {
        Some(from_glib_none(ptr))
    }
}

/// An `NMConnection` (always an `NMSimpleConnection` here).
#[derive(Clone, Debug)]
pub struct Connection(pub glib::Object);

impl PartialEq for Connection {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Connection {
    fn raw(&self) -> Obj {
        self.0.as_ptr()
    }

    pub fn new() -> Result<Self, glib::Error> {
        let nm = nm()?;
        unsafe { Ok(Self(object((nm.nm_simple_connection_new)(), true).unwrap())) }
    }

    /// `NM.SimpleConnection.new_from_dbus` (a{sa{sv}}).
    pub fn from_dbus(dict: &Variant) -> Result<Self, glib::Error> {
        let nm = nm()?;
        if dict.type_().as_str() != "a{sa{sv}}" {
            return Err(glib::Error::new(gio::IOErrorEnum::InvalidArgument, "The connection settings are invalid."));
        }
        unsafe {
            let mut error = ptr::null_mut();
            let raw = (nm.nm_simple_connection_new_from_dbus)(dict.as_ptr(), &mut error);
            object(raw, true).map(Self).ok_or_else(|| take_error(error))
        }
    }

    pub fn duplicate(&self) -> Self {
        let nm = api().unwrap();
        unsafe { Self(object((nm.nm_simple_connection_new_clone)(self.raw()), true).unwrap()) }
    }

    pub fn to_dbus(&self, flags: u32) -> Variant {
        let nm = api().unwrap();
        unsafe {
            let raw = (nm.nm_connection_to_dbus)(self.raw(), flags);
            if raw.is_null() {
                Variant::from_dict_entries_empty()
            } else {
                from_glib_none(raw)
            }
        }
    }

    pub fn normalize(&self) -> Result<(), glib::Error> {
        let nm = api().unwrap();
        unsafe {
            let mut error = ptr::null_mut();
            if (nm.nm_connection_normalize)(self.raw(), ptr::null_mut(), ptr::null_mut(), &mut error) != 0 {
                Ok(())
            } else {
                Err(take_error(error))
            }
        }
    }

    pub fn verify(&self) -> Result<(), glib::Error> {
        let nm = api().unwrap();
        unsafe {
            let mut error = ptr::null_mut();
            if (nm.nm_connection_verify)(self.raw(), &mut error) != 0 { Ok(()) } else { Err(take_error(error)) }
        }
    }

    pub fn verify_secrets(&self) -> Result<(), glib::Error> {
        let nm = api().unwrap();
        unsafe {
            let mut error = ptr::null_mut();
            if (nm.nm_connection_verify_secrets)(self.raw(), &mut error) != 0 { Ok(()) } else { Err(take_error(error)) }
        }
    }

    /// `(setting name, hints)` of the first setting still missing secrets.
    pub fn need_secrets(&self) -> (Option<String>, Vec<String>) {
        let nm = api().unwrap();
        unsafe {
            let mut hints: *mut gffi::GPtrArray = ptr::null_mut();
            let name = opt_string((nm.nm_connection_need_secrets)(self.raw(), &mut hints));
            let mut out = Vec::new();
            if !hints.is_null() {
                for i in 0..(*hints).len {
                    if let Some(text) = opt_string(*(*hints).pdata.add(i as usize) as *const c_char) {
                        out.push(text);
                    }
                }
                gffi::g_ptr_array_unref(hints);
            }
            (name, out)
        }
    }

    pub fn update_secrets(&self, setting: &str, secrets: &Variant) -> Result<(), glib::Error> {
        let nm = api().unwrap();
        let name = cstr(setting);
        unsafe {
            let mut error = ptr::null_mut();
            if (nm.nm_connection_update_secrets)(self.raw(), name.as_ptr(), secrets.as_ptr(), &mut error) != 0 {
                Ok(())
            } else {
                Err(take_error(error))
            }
        }
    }

    pub fn clear_secrets(&self) {
        let nm = api().unwrap();
        unsafe { (nm.nm_connection_clear_secrets)(self.raw()) }
    }

    pub fn setting(&self, name: &str) -> Option<Setting> {
        let nm = api()?;
        let name = cstr(name);
        unsafe { object((nm.nm_connection_get_setting_by_name)(self.raw(), name.as_ptr()), false).map(Setting) }
    }

    pub fn settings(&self) -> Vec<Setting> {
        let nm = api().unwrap();
        unsafe {
            let mut length = 0;
            let array = (nm.nm_connection_get_settings)(self.raw(), &mut length);
            let mut out = Vec::new();
            if !array.is_null() {
                for i in 0..length as usize {
                    if let Some(setting) = object(*array.add(i), false) {
                        out.push(Setting(setting));
                    }
                }
                gffi::g_free(array as *mut c_void);
            }
            out
        }
    }

    /// Adds (replacing a setting of the same type); the connection keeps its own reference.
    pub fn add_setting(&self, setting: &Setting) {
        let nm = api().unwrap();
        unsafe {
            gobj::g_object_ref(setting.0.as_ptr() as *mut _);
            (nm.nm_connection_add_setting)(self.raw(), setting.0.as_ptr());
        }
    }

    pub fn remove_setting(&self, name: &str) {
        if let Some(gtype) = setting_type(name) {
            let nm = api().unwrap();
            unsafe { (nm.nm_connection_remove_setting)(self.raw(), gtype) }
        }
    }

    pub fn uuid(&self) -> String {
        let nm = api().unwrap();
        unsafe { opt_string((nm.nm_connection_get_uuid)(self.raw())).unwrap_or_default() }
    }

    pub fn id(&self) -> String {
        let nm = api().unwrap();
        unsafe { opt_string((nm.nm_connection_get_id)(self.raw())).unwrap_or_default() }
    }

    pub fn connection_type(&self) -> String {
        let nm = api().unwrap();
        unsafe { opt_string((nm.nm_connection_get_connection_type)(self.raw())).unwrap_or_default() }
    }

    pub fn path(&self) -> String {
        let nm = api().unwrap();
        unsafe { opt_string((nm.nm_connection_get_path)(self.raw())).unwrap_or_default() }
    }

    pub fn set_path(&self, path: &str) {
        let nm = api().unwrap();
        let path = cstr(path);
        unsafe { (nm.nm_connection_set_path)(self.raw(), path.as_ptr()) }
    }

    /// `connection.to_dbus(WITH_SECRETS).unpack()[setting]` flattened to text
    /// secrets (`flat_secrets` of secret_agent.py).
    pub fn flat_secrets(&self, setting: &str) -> Vec<(String, String)> {
        flat_secrets(&self.to_dbus(WITH_SECRETS), setting)
    }
}

trait EmptyDict {
    fn from_dict_entries_empty() -> Variant;
}

impl EmptyDict for Variant {
    fn from_dict_entries_empty() -> Variant {
        Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(), Vec::<Variant>::new())
    }
}

/// `{setting: {key: value}}` text secrets of a serialized connection; VPN
/// secrets come from the `secrets` map and WireGuard peers become
/// `peers.<public-key>.preshared-key`.
pub fn flat_secrets(dict: &Variant, setting_name: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(values) = lookup(dict, setting_name, "a{sv}") else {
        return out;
    };
    if setting_name == "vpn" {
        if let Some(secrets) = lookup(&values, "secrets", "a{ss}") {
            for entry in secrets.iter() {
                if let (Some(k), Some(v)) = (entry.child_value(0).str().map(String::from), entry.child_value(1).str().map(String::from)) {
                    out.push((k, v));
                }
            }
        }
        return out;
    }
    for entry in values.iter() {
        let key = entry.child_value(0).str().unwrap_or_default().to_owned();
        let value = crate::nm::unbox(&entry.child_value(1)).unwrap();
        if let Some(text) = value.str() {
            out.push((key, text.to_owned()));
        }
    }
    if setting_name == "wireguard" {
        if let Some(peers) = lookup(&values, "peers", "aa{sv}") {
            for peer in peers.iter() {
                let public = lookup(&peer, "public-key", "s").and_then(|v| v.str().map(String::from));
                let secret = lookup(&peer, "preshared-key", "s").and_then(|v| v.str().map(String::from));
                if let (Some(public), Some(secret)) = (public, secret) {
                    if !public.is_empty() && !secret.is_empty() {
                        out.push((format!("peers.{public}.preshared-key"), secret));
                    }
                }
            }
        }
    }
    out
}

/// `dict[key]` of an `a{sv}` / `a{sa{sv}}` / `a{ss}` variant, if of type `ty`.
pub fn lookup(dict: &Variant, key: &str, ty: &str) -> Option<Variant> {
    if !dict.type_().is_array() {
        return None;
    }
    for entry in dict.iter() {
        if entry.n_children() == 2 && entry.child_value(0).str() == Some(key) {
            let value = entry.child_value(1);
            let value = crate::nm::unbox(&value).unwrap_or(value);
            return (value.type_().as_str() == ty).then_some(value);
        }
    }
    None
}

/// An `NMSetting` subclass instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting(pub glib::Object);

impl Setting {
    fn raw(&self) -> Obj {
        self.0.as_ptr()
    }

    /// A new, default setting of the named type (`NM.Setting<Type>.new()`).
    pub fn new(name: &str) -> Option<Self> {
        let gtype = setting_type(name)?;
        unsafe {
            let raw = gobj::g_object_new(gtype, ptr::null());
            object(raw as Obj, true).map(Setting)
        }
    }

    pub fn name(&self) -> String {
        let nm = api().unwrap();
        unsafe { opt_string((nm.nm_setting_get_name)(self.raw())).unwrap_or_default() }
    }

    pub fn duplicate(&self) -> Self {
        let nm = api().unwrap();
        unsafe { Self(object((nm.nm_setting_duplicate)(self.raw()), true).unwrap()) }
    }

    pub fn verify(&self) -> Result<(), glib::Error> {
        let nm = api().unwrap();
        unsafe {
            let mut error = ptr::null_mut();
            if (nm.nm_setting_verify)(self.raw(), ptr::null_mut(), &mut error) != 0 { Ok(()) } else { Err(take_error(error)) }
        }
    }

    pub fn find_property(&self, name: &str) -> Option<glib::ParamSpec> {
        self.0.find_property(name)
    }

    pub fn properties(&self) -> Vec<glib::ParamSpec> {
        self.0.list_properties().to_vec()
    }

    /// Raw `GParamSpec.flags` including libnm's user flags (SECRET, REQUIRED).
    pub fn raw_flags(spec: &glib::ParamSpec) -> u32 {
        unsafe { (*spec.as_ptr()).flags as u32 }
    }

    pub fn dbus_type(&self, property: &str) -> String {
        let nm = api().unwrap();
        let name = cstr(property);
        unsafe {
            let ty = (nm.nm_setting_get_dbus_property_type)(self.raw(), name.as_ptr());
            if ty.is_null() {
                String::new()
            } else {
                let text = gffi::g_variant_type_dup_string(ty);
                let out = opt_string(text).unwrap_or_default();
                gffi::g_free(text as *mut c_void);
                out
            }
        }
    }

    pub fn get(&self, property: &str) -> Option<glib::Value> {
        self.find_property(property)?;
        Some(self.0.property_value(property))
    }

    pub fn get_string(&self, property: &str) -> Option<String> {
        self.get(property).and_then(|v| v.get::<Option<String>>().ok().flatten())
    }

    pub fn get_bool(&self, property: &str) -> bool {
        self.get(property).and_then(|v| v.get::<bool>().ok()).unwrap_or(false)
    }

    pub fn get_u32(&self, property: &str) -> u32 {
        self.get(property).and_then(|v| value_as_i64(&v)).map(|v| v as u32).unwrap_or(0)
    }

    pub fn get_i64(&self, property: &str) -> Option<i64> {
        self.get(property).and_then(|v| value_as_i64(&v))
    }

    pub fn get_strv(&self, property: &str) -> Vec<String> {
        self.get(property).and_then(|v| v.get::<Vec<String>>().ok()).unwrap_or_default()
    }

    pub fn get_bytes(&self, property: &str) -> Option<Vec<u8>> {
        self.get(property).and_then(|v| v.get::<Option<glib::Bytes>>().ok().flatten()).map(|b| b.to_vec())
    }

    pub fn set(&self, property: &str, value: &glib::Value) -> Result<(), String> {
        let spec = self.find_property(property).ok_or_else(|| format!("No property {property}"))?;
        let converted = convert_value(value, spec.value_type()).ok_or_else(|| format!("Invalid value for {property}"))?;
        if !spec.flags().contains(glib::ParamFlags::WRITABLE) {
            return Err(format!("{property} is read-only"));
        }
        self.0.set_property_from_value(property, &converted);
        Ok(())
    }

    pub fn set_str(&self, property: &str, value: Option<&str>) {
        let _ = self.set(property, &value.to_value());
    }

    pub fn set_bool(&self, property: &str, value: bool) {
        let _ = self.set(property, &value.to_value());
    }

    pub fn set_int(&self, property: &str, value: i64) {
        let _ = self.set_int_checked(property, value);
    }

    pub fn set_int_checked(&self, property: &str, value: i64) -> Result<(), String> {
        let spec = self.find_property(property).ok_or_else(|| format!("No property {property}"))?;
        let value = int_value(spec.value_type(), value).ok_or_else(|| format!("Invalid value for {property}"))?;
        self.0.set_property_from_value(property, &value);
        Ok(())
    }

    pub fn set_bytes(&self, property: &str, value: Option<&[u8]>) {
        let bytes = value.map(glib::Bytes::from);
        let _ = self.set(property, &bytes.to_value());
    }

    pub fn set_strv(&self, property: &str, values: &[String]) {
        let strv: glib::StrV = values.iter().map(|s| glib::GString::from(s.as_str())).collect();
        let _ = self.set(property, &strv.to_value());
    }

    /// `GHashTable<string,string>` properties (bond options, VPN data, …).
    pub fn get_hash(&self, property: &str) -> Vec<(String, String)> {
        let Some(value) = self.get(property) else { return Vec::new() };
        let mut out = Vec::new();
        unsafe {
            let table = gobj::g_value_get_boxed(value.to_glib_none().0) as *mut gffi::GHashTable;
            if table.is_null() {
                return out;
            }
            let mut iter: gffi::GHashTableIter = std::mem::zeroed();
            gffi::g_hash_table_iter_init(&mut iter, table);
            let (mut key, mut val) = (ptr::null_mut(), ptr::null_mut());
            while gffi::g_hash_table_iter_next(&mut iter, &mut key, &mut val) != 0 {
                if let Some(k) = opt_string(key as *const c_char) {
                    out.push((k, opt_string(val as *const c_char).unwrap_or_default()));
                }
            }
        }
        out.sort();
        out
    }

    pub fn set_hash(&self, property: &str, values: &[(String, String)]) -> Result<(), String> {
        let spec = self.find_property(property).ok_or_else(|| format!("No property {property}"))?;
        unsafe {
            let table = gffi::g_hash_table_new_full(Some(gffi::g_str_hash), Some(gffi::g_str_equal), Some(gffi::g_free), Some(gffi::g_free));
            for (k, v) in values {
                let (k, v) = (cstr(k), cstr(v));
                gffi::g_hash_table_insert(table, gffi::g_strdup(k.as_ptr()) as *mut c_void, gffi::g_strdup(v.as_ptr()) as *mut c_void);
            }
            let mut value = glib::Value::from_type(spec.value_type());
            gobj::g_value_take_boxed(value.to_glib_none_mut().0, table as *const c_void);
            self.0.set_property_from_value(property, &value);
        }
        Ok(())
    }

    /// `nm_setting_get_secret_flags` (0 when the property has no flags).
    pub fn secret_flags(&self, key: &str) -> Option<u32> {
        let nm = api().unwrap();
        let name = cstr(key);
        unsafe {
            let mut flags = 0;
            let mut error = ptr::null_mut();
            if (nm.nm_setting_get_secret_flags)(self.raw(), name.as_ptr(), &mut flags, &mut error) != 0 {
                Some(flags)
            } else {
                if !error.is_null() {
                    gffi::g_error_free(error);
                }
                None
            }
        }
    }

    pub fn set_secret_flags(&self, key: &str, flags: u32) -> Result<(), glib::Error> {
        let nm = api().unwrap();
        let name = cstr(key);
        unsafe {
            let mut error = ptr::null_mut();
            if (nm.nm_setting_set_secret_flags)(self.raw(), name.as_ptr(), flags, &mut error) != 0 { Ok(()) } else { Err(take_error(error)) }
        }
    }

    // --- VPN ---
    unsafe fn keys(array: *mut *const c_char, length: c_uint) -> Vec<String> {
        let mut out = Vec::new();
        if !array.is_null() {
            for i in 0..length as usize {
                if let Some(text) = opt_string(*array.add(i)) {
                    out.push(text);
                }
            }
            gffi::g_free(array as *mut c_void);
        }
        out
    }

    pub fn vpn_service_type(&self) -> String {
        let nm = api().unwrap();
        unsafe { opt_string((nm.nm_setting_vpn_get_service_type)(self.raw())).unwrap_or_default() }
    }

    pub fn vpn_data_keys(&self) -> Vec<String> {
        let nm = api().unwrap();
        unsafe {
            let mut length = 0;
            let array = (nm.nm_setting_vpn_get_data_keys)(self.raw(), &mut length);
            Self::keys(array, length)
        }
    }

    pub fn vpn_data_item(&self, key: &str) -> Option<String> {
        let nm = api().unwrap();
        let key = cstr(key);
        unsafe { opt_string((nm.nm_setting_vpn_get_data_item)(self.raw(), key.as_ptr())) }
    }

    pub fn vpn_add_data_item(&self, key: &str, value: &str) {
        let nm = api().unwrap();
        let (key, value) = (cstr(key), cstr(value));
        unsafe { (nm.nm_setting_vpn_add_data_item)(self.raw(), key.as_ptr(), value.as_ptr()) }
    }

    pub fn vpn_remove_data_item(&self, key: &str) {
        let nm = api().unwrap();
        let key = cstr(key);
        unsafe { (nm.nm_setting_vpn_remove_data_item)(self.raw(), key.as_ptr()); }
    }

    pub fn vpn_secret_keys(&self) -> Vec<String> {
        let nm = api().unwrap();
        unsafe {
            let mut length = 0;
            let array = (nm.nm_setting_vpn_get_secret_keys)(self.raw(), &mut length);
            Self::keys(array, length)
        }
    }

    pub fn vpn_secret(&self, key: &str) -> Option<String> {
        let nm = api().unwrap();
        let key = cstr(key);
        unsafe { opt_string((nm.nm_setting_vpn_get_secret)(self.raw(), key.as_ptr())) }
    }

    pub fn vpn_add_secret(&self, key: &str, value: &str) {
        let nm = api().unwrap();
        let (key, value) = (cstr(key), cstr(value));
        unsafe { (nm.nm_setting_vpn_add_secret)(self.raw(), key.as_ptr(), value.as_ptr()) }
    }

    pub fn vpn_remove_secret(&self, key: &str) {
        let nm = api().unwrap();
        let key = cstr(key);
        unsafe { (nm.nm_setting_vpn_remove_secret)(self.raw(), key.as_ptr()); }
    }

    // --- WireGuard peers ---
    pub fn peers(&self) -> Vec<Peer> {
        let nm = api().unwrap();
        unsafe {
            let count = (nm.nm_setting_wireguard_get_peers_len)(self.raw());
            (0..count).map(|i| Peer::from_none((nm.nm_setting_wireguard_get_peer)(self.raw(), i))).collect()
        }
    }

    pub fn set_peer(&self, peer: &Peer, index: u32) {
        let nm = api().unwrap();
        unsafe { (nm.nm_setting_wireguard_set_peer)(self.raw(), peer.0, index) }
    }

    pub fn append_peer(&self, peer: &Peer) {
        let nm = api().unwrap();
        unsafe { (nm.nm_setting_wireguard_append_peer)(self.raw(), peer.0) }
    }

    pub fn clear_peers(&self) {
        let nm = api().unwrap();
        unsafe { (nm.nm_setting_wireguard_clear_peers)(self.raw()); }
    }

    // --- IP config ---
    pub fn ip_method(&self) -> Option<String> {
        let nm = api().unwrap();
        unsafe { opt_string((nm.nm_setting_ip_config_get_method)(self.raw())) }
    }

    pub fn addresses(&self) -> Vec<IpAddress> {
        let nm = api().unwrap();
        unsafe {
            let n = (nm.nm_setting_ip_config_get_num_addresses)(self.raw());
            (0..n as c_int).map(|i| IpAddress((nm.nm_ip_address_dup)((nm.nm_setting_ip_config_get_address)(self.raw(), i)))).collect()
        }
    }

    pub fn set_addresses(&self, values: &[IpAddress]) {
        let nm = api().unwrap();
        unsafe {
            (nm.nm_setting_ip_config_clear_addresses)(self.raw());
            for v in values {
                (nm.nm_setting_ip_config_add_address)(self.raw(), v.0);
            }
        }
    }

    pub fn routes(&self) -> Vec<IpRoute> {
        let nm = api().unwrap();
        unsafe {
            let n = (nm.nm_setting_ip_config_get_num_routes)(self.raw());
            (0..n as c_int).map(|i| IpRoute((nm.nm_ip_route_dup)((nm.nm_setting_ip_config_get_route)(self.raw(), i)))).collect()
        }
    }

    pub fn set_routes(&self, values: &[IpRoute]) {
        let nm = api().unwrap();
        unsafe {
            (nm.nm_setting_ip_config_clear_routes)(self.raw());
            for v in values {
                (nm.nm_setting_ip_config_add_route)(self.raw(), v.0);
            }
        }
    }

    pub fn routing_rule_strings(&self) -> Vec<String> {
        let nm = api().unwrap();
        unsafe {
            let n = (nm.nm_setting_ip_config_get_num_routing_rules)(self.raw());
            (0..n).filter_map(|i| {
                let rule = (nm.nm_setting_ip_config_get_routing_rule)(self.raw(), i);
                let text = (nm.nm_ip_routing_rule_to_string)(rule, 0, ptr::null_mut(), ptr::null_mut());
                let out = opt_string(text);
                gffi::g_free(text as *mut c_void);
                out
            }).collect()
        }
    }

    /// Parse and validate rules first; replace only when every line is valid.
    pub fn set_routing_rules(&self, lines: &[String], family: i32) -> Result<(), String> {
        let nm = api().unwrap();
        let flags = if family == AF_INET { RULE_AF_INET } else { RULE_AF_INET6 } | RULE_VALIDATE;
        let mut rules = Vec::new();
        unsafe {
            for line in lines {
                let text = cstr(line);
                let mut error = ptr::null_mut();
                let rule = (nm.nm_ip_routing_rule_from_string)(text.as_ptr(), flags, ptr::null_mut(), &mut error);
                if rule.is_null() || (nm.nm_ip_routing_rule_get_addr_family)(rule) != family {
                    if !error.is_null() {
                        gffi::g_error_free(error);
                    }
                    if !rule.is_null() {
                        (nm.nm_ip_routing_rule_unref)(rule);
                    }
                    for r in rules {
                        (nm.nm_ip_routing_rule_unref)(r);
                    }
                    return Err(line.clone());
                }
                rules.push(rule);
            }
            (nm.nm_setting_ip_config_clear_routing_rules)(self.raw());
            for rule in rules {
                (nm.nm_setting_ip_config_add_routing_rule)(self.raw(), rule);
                (nm.nm_ip_routing_rule_unref)(rule);
            }
        }
        Ok(())
    }

    // --- ethtool / generic options ---
    pub fn option_names(&self) -> Vec<String> {
        let nm = api().unwrap();
        unsafe {
            let mut length = 0;
            let array = (nm.nm_setting_option_get_all_names)(self.raw(), &mut length);
            let mut out = Vec::new();
            if !array.is_null() {
                for i in 0..length as usize {
                    if let Some(text) = opt_string(*array.add(i)) {
                        out.push(text);
                    }
                }
            }
            out
        }
    }

    pub fn option(&self, name: &str) -> Option<Variant> {
        let nm = api().unwrap();
        let name = cstr(name);
        unsafe {
            let raw = (nm.nm_setting_option_get)(self.raw(), name.as_ptr());
            if raw.is_null() { None } else { Some(from_glib_none(raw)) }
        }
    }

    pub fn set_option(&self, name: &str, value: Option<&Variant>) {
        let nm = api().unwrap();
        let name = cstr(name);
        unsafe {
            (nm.nm_setting_option_set)(self.raw(), name.as_ptr(), value.map(|v| v.as_ptr()).unwrap_or(ptr::null_mut()));
        }
    }
}

pub fn ethtool_option_is_bool(name: &str) -> bool {
    let Some(nm) = api() else { return false };
    let name = cstr(name);
    unsafe {
        (nm.nm_ethtool_optname_is_feature)(name.as_ptr()) != 0 || (nm.nm_ethtool_optname_is_pause)(name.as_ptr()) != 0
            || (nm.nm_ethtool_optname_is_eee)(name.as_ptr()) != 0
    }
}

fn int_value(gtype: glib::Type, value: i64) -> Option<glib::Value> {
    use glib::Type as T;
    Some(match gtype {
        T::I32 => i32::try_from(value).ok()?.to_value(),
        T::U32 => u32::try_from(value).ok()?.to_value(),
        T::I64 => value.to_value(),
        T::U64 => u64::try_from(value).ok()?.to_value(),
        T::I8 => i8::try_from(value).ok()?.to_value(),
        T::U8 => u8::try_from(value).ok()?.to_value(),
        T::I_LONG => (value as libc::c_long).to_value(),
        T::U_LONG => (u64::try_from(value).ok()? as libc::c_ulong).to_value(),
        t if t.is_a(T::ENUM) => unsafe {
            let mut v = glib::Value::from_type(t);
            gobj::g_value_set_enum(v.to_glib_none_mut().0, i32::try_from(value).ok()?);
            v
        },
        t if t.is_a(T::FLAGS) => unsafe {
            let mut v = glib::Value::from_type(t);
            gobj::g_value_set_flags(v.to_glib_none_mut().0, u32::try_from(value).ok()?);
            v
        },
        _ => return None,
    })
}

/// Integer/enum/flags representation of a property value (`int(value)`).
pub fn value_as_i64(value: &glib::Value) -> Option<i64> {
    use glib::Type as T;
    let t = value.type_();
    unsafe {
        Some(match t {
            T::I32 => value.get::<i32>().ok()? as i64,
            T::U32 => value.get::<u32>().ok()? as i64,
            T::I64 => value.get::<i64>().ok()?,
            T::U64 => value.get::<u64>().ok()? as i64,
            T::I8 => value.get::<i8>().ok()? as i64,
            T::U8 => value.get::<u8>().ok()? as i64,
            T::I_LONG => value.get::<libc::c_long>().ok()? as i64,
            T::U_LONG => value.get::<libc::c_ulong>().ok()? as i64,
            t if t.is_a(T::ENUM) => gobj::g_value_get_enum(value.to_glib_none().0) as i64,
            t if t.is_a(T::FLAGS) => gobj::g_value_get_flags(value.to_glib_none().0) as i64,
            _ => return None,
        })
    }
}

fn convert_value(value: &glib::Value, target: glib::Type) -> Option<glib::Value> {
    if value.type_().is_a(target) {
        return Some(value.clone());
    }
    if let Some(number) = value_as_i64(value) {
        return int_value(target, number);
    }
    let mut out = glib::Value::from_type(target);
    unsafe {
        if gobj::g_value_type_transformable(value.type_().into_glib(), target.into_glib()) != 0
            && gobj::g_value_transform(value.to_glib_none().0, out.to_glib_none_mut().0) != 0 {
            return Some(out);
        }
    }
    None
}

/// Enum values `(value, nick)` of an enum GType (`__enum_values__`).
pub fn enum_values(gtype: glib::Type) -> Vec<(i64, String)> {
    let Some(class) = glib::EnumClass::with_type(gtype) else { return Vec::new() };
    let mut values: Vec<(i64, String)> = class.values().iter().map(|v| (v.value() as i64, v.nick().to_owned())).collect();
    values.sort_by_key(|v| v.0);
    values
}

pub fn setting_type(name: &str) -> Option<GType> {
    let nm = api()?;
    let name = cstr(name);
    let gtype = unsafe { (nm.nm_setting_lookup_type)(name.as_ptr()) };
    if gtype == 0 { None } else { Some(gtype) }
}

pub fn setting_exists(name: &str) -> bool {
    setting_type(name).is_some()
}

/// A WireGuard peer (`NMWireGuardPeer`, refcounted boxed type).
pub struct Peer(*mut c_void);

impl Peer {
    unsafe fn from_none(raw: *mut c_void) -> Self {
        let nm = api().unwrap();
        Self((nm.nm_wireguard_peer_new_clone)(raw, 1))
    }

    pub fn new() -> Self {
        let nm = api().unwrap();
        unsafe { Self((nm.nm_wireguard_peer_new)()) }
    }

    pub fn duplicate(&self, with_secrets: bool) -> Self {
        let nm = api().unwrap();
        unsafe { Self((nm.nm_wireguard_peer_new_clone)(self.0, with_secrets as B)) }
    }

    pub fn public_key(&self) -> String {
        unsafe { opt_string((api().unwrap().nm_wireguard_peer_get_public_key)(self.0)).unwrap_or_default() }
    }

    pub fn set_public_key(&self, key: &str) -> bool {
        let key = cstr(key);
        unsafe { (api().unwrap().nm_wireguard_peer_set_public_key)(self.0, key.as_ptr(), 0) != 0 }
    }

    pub fn endpoint(&self) -> String {
        unsafe { opt_string((api().unwrap().nm_wireguard_peer_get_endpoint)(self.0)).unwrap_or_default() }
    }

    pub fn set_endpoint(&self, endpoint: &str) -> bool {
        let endpoint = cstr(endpoint);
        unsafe { (api().unwrap().nm_wireguard_peer_set_endpoint)(self.0, endpoint.as_ptr(), 0) != 0 }
    }

    pub fn allowed_ips(&self) -> Vec<String> {
        let nm = api().unwrap();
        unsafe {
            let n = (nm.nm_wireguard_peer_get_allowed_ips_len)(self.0);
            (0..n).filter_map(|i| opt_string((nm.nm_wireguard_peer_get_allowed_ip)(self.0, i, ptr::null_mut()))).collect()
        }
    }

    pub fn clear_allowed_ips(&self) {
        unsafe { (api().unwrap().nm_wireguard_peer_clear_allowed_ips)(self.0) }
    }

    pub fn append_allowed_ip(&self, ip: &str) -> bool {
        let ip = cstr(ip);
        unsafe { (api().unwrap().nm_wireguard_peer_append_allowed_ip)(self.0, ip.as_ptr(), 0) != 0 }
    }

    pub fn persistent_keepalive(&self) -> u16 {
        unsafe { (api().unwrap().nm_wireguard_peer_get_persistent_keepalive)(self.0) }
    }

    pub fn set_persistent_keepalive(&self, value: u16) {
        unsafe { (api().unwrap().nm_wireguard_peer_set_persistent_keepalive)(self.0, value) }
    }

    pub fn preshared_key(&self) -> Option<String> {
        unsafe { opt_string((api().unwrap().nm_wireguard_peer_get_preshared_key)(self.0)) }
    }

    pub fn set_preshared_key(&self, key: &str) -> bool {
        let key = cstr(key);
        unsafe { (api().unwrap().nm_wireguard_peer_set_preshared_key)(self.0, key.as_ptr(), 0) != 0 }
    }

    pub fn preshared_key_flags(&self) -> u32 {
        unsafe { (api().unwrap().nm_wireguard_peer_get_preshared_key_flags)(self.0) }
    }

    pub fn set_preshared_key_flags(&self, flags: u32) {
        unsafe { (api().unwrap().nm_wireguard_peer_set_preshared_key_flags)(self.0, flags) }
    }
}

impl Default for Peer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(nm) = api() {
            unsafe { (nm.nm_wireguard_peer_unref)(self.0) }
        }
    }
}

unsafe fn attribute_names(raw: *mut *mut c_char) -> Vec<String> {
    let mut out = Vec::new();
    if !raw.is_null() {
        let mut i = 0;
        while !(*raw.add(i)).is_null() {
            out.push(CStr::from_ptr(*raw.add(i)).to_string_lossy().into_owned());
            i += 1;
        }
        gffi::g_strfreev(raw);
    }
    out
}

/// `NMIPAddress` (owned copy).
pub struct IpAddress(*mut c_void);

impl IpAddress {
    pub fn new(family: i32, address: &str, prefix: u32) -> Result<Self, glib::Error> {
        let nm = nm()?;
        let address = cstr(address);
        unsafe {
            let mut error = ptr::null_mut();
            let raw = (nm.nm_ip_address_new)(family, address.as_ptr(), prefix, &mut error);
            if raw.is_null() { Err(take_error(error)) } else { Ok(Self(raw)) }
        }
    }
    pub fn duplicate(&self) -> Self {
        unsafe { Self((api().unwrap().nm_ip_address_dup)(self.0)) }
    }
    pub fn address(&self) -> String {
        unsafe { opt_string((api().unwrap().nm_ip_address_get_address)(self.0)).unwrap_or_default() }
    }
    pub fn prefix(&self) -> u32 {
        unsafe { (api().unwrap().nm_ip_address_get_prefix)(self.0) }
    }
    pub fn set_address(&self, address: &str) {
        let address = cstr(address);
        unsafe { (api().unwrap().nm_ip_address_set_address)(self.0, address.as_ptr()) }
    }
    pub fn set_prefix(&self, prefix: u32) {
        unsafe { (api().unwrap().nm_ip_address_set_prefix)(self.0, prefix) }
    }
    pub fn attributes(&self) -> Vec<(String, Variant)> {
        let nm = api().unwrap();
        unsafe {
            attribute_names((nm.nm_ip_address_get_attribute_names)(self.0)).into_iter().filter_map(|name| {
                let key = cstr(&name);
                let raw = (nm.nm_ip_address_get_attribute)(self.0, key.as_ptr());
                if raw.is_null() { None } else { Some((name, from_glib_none(raw))) }
            }).collect()
        }
    }
    pub fn set_attribute(&self, name: &str, value: Option<&Variant>) {
        let key = cstr(name);
        unsafe {
            (api().unwrap().nm_ip_address_set_attribute)(self.0, key.as_ptr(),
                value.map(|v| ToGlibPtr::<*mut gffi::GVariant>::to_glib_full(v)).unwrap_or(ptr::null_mut()));
        }
    }
}

impl Drop for IpAddress {
    fn drop(&mut self) {
        unsafe { (api().unwrap().nm_ip_address_unref)(self.0) }
    }
}

/// `NMIPRoute` (owned copy).
pub struct IpRoute(*mut c_void);

impl IpRoute {
    pub fn new(family: i32, dest: &str, prefix: u32, next_hop: Option<&str>, metric: i64) -> Result<Self, glib::Error> {
        let nm = nm()?;
        let dest = cstr(dest);
        let hop = next_hop.map(cstr);
        unsafe {
            let mut error = ptr::null_mut();
            let raw = (nm.nm_ip_route_new)(family, dest.as_ptr(), prefix, hop.as_ref().map(|h| h.as_ptr()).unwrap_or(ptr::null()),
                                           metric, &mut error);
            if raw.is_null() { Err(take_error(error)) } else { Ok(Self(raw)) }
        }
    }
    pub fn duplicate(&self) -> Self {
        unsafe { Self((api().unwrap().nm_ip_route_dup)(self.0)) }
    }
    pub fn dest(&self) -> String {
        unsafe { opt_string((api().unwrap().nm_ip_route_get_dest)(self.0)).unwrap_or_default() }
    }
    pub fn prefix(&self) -> u32 {
        unsafe { (api().unwrap().nm_ip_route_get_prefix)(self.0) }
    }
    pub fn next_hop(&self) -> Option<String> {
        unsafe { opt_string((api().unwrap().nm_ip_route_get_next_hop)(self.0)) }
    }
    pub fn metric(&self) -> i64 {
        unsafe { (api().unwrap().nm_ip_route_get_metric)(self.0) }
    }
    pub fn set_dest(&self, dest: &str) {
        let dest = cstr(dest);
        unsafe { (api().unwrap().nm_ip_route_set_dest)(self.0, dest.as_ptr()) }
    }
    pub fn set_prefix(&self, prefix: u32) {
        unsafe { (api().unwrap().nm_ip_route_set_prefix)(self.0, prefix) }
    }
    pub fn set_next_hop(&self, hop: Option<&str>) {
        let hop = hop.map(cstr);
        unsafe { (api().unwrap().nm_ip_route_set_next_hop)(self.0, hop.as_ref().map(|h| h.as_ptr()).unwrap_or(ptr::null())) }
    }
    pub fn set_metric(&self, metric: i64) {
        unsafe { (api().unwrap().nm_ip_route_set_metric)(self.0, metric) }
    }
    pub fn attributes(&self) -> Vec<(String, Variant)> {
        let nm = api().unwrap();
        unsafe {
            attribute_names((nm.nm_ip_route_get_attribute_names)(self.0)).into_iter().filter_map(|name| {
                let key = cstr(&name);
                let raw = (nm.nm_ip_route_get_attribute)(self.0, key.as_ptr());
                if raw.is_null() { None } else { Some((name, from_glib_none(raw))) }
            }).collect()
        }
    }
    pub fn set_attribute(&self, name: &str, value: Option<&Variant>) {
        let key = cstr(name);
        unsafe {
            (api().unwrap().nm_ip_route_set_attribute)(self.0, key.as_ptr(),
                value.map(|v| ToGlibPtr::<*mut gffi::GVariant>::to_glib_full(v)).unwrap_or(ptr::null_mut()));
        }
    }
}

impl Drop for IpRoute {
    fn drop(&mut self) {
        unsafe { (api().unwrap().nm_ip_route_unref)(self.0) }
    }
}

pub fn route_attribute_valid(name: &str, value: &Variant, family: i32) -> bool {
    let Some(nm) = api() else { return false };
    let key = cstr(name);
    unsafe {
        let mut error = ptr::null_mut();
        let ok = (nm.nm_ip_route_attribute_validate)(key.as_ptr(), value.as_ptr(), family, ptr::null_mut(), &mut error) != 0;
        if !error.is_null() {
            gffi::g_error_free(error);
        }
        ok
    }
}

pub fn wpa_psk_valid(psk: &str) -> bool {
    match api() {
        Some(nm) => {
            let psk = cstr(psk);
            unsafe { (nm.nm_utils_wpa_psk_valid)(psk.as_ptr()) != 0 }
        }
        None => {
            let n = psk.len();
            (8..=63).contains(&n) || (n == 64 && psk.chars().all(|c| c.is_ascii_hexdigit()))
        }
    }
}

/// `nm_utils_wep_key_valid(key, NM_WEP_KEY_TYPE_KEY=1 | PASSPHRASE=2)`.
pub fn wep_key_valid(key: &str, key_type: u32) -> bool {
    match api() {
        Some(nm) => {
            let key = cstr(key);
            unsafe { (nm.nm_utils_wep_key_valid)(key.as_ptr(), key_type) != 0 }
        }
        None => false,
    }
}

/// `NM.utils_ssid_to_utf8` (libnm guesses the legacy encoding), else lossy UTF-8.
pub fn ssid_to_utf8(ssid: &[u8]) -> String {
    if let Some(nm) = api() {
        unsafe {
            let raw = (nm.nm_utils_ssid_to_utf8)(ssid.as_ptr(), ssid.len());
            if !raw.is_null() {
                let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
                gffi::g_free(raw as *mut c_void);
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    String::from_utf8_lossy(ssid).into_owned()
}

pub fn uuid_generate() -> String {
    if let Some(nm) = api() {
        unsafe {
            let raw = (nm.nm_utils_uuid_generate)();
            if !raw.is_null() {
                let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
                gffi::g_free(raw as *mut c_void);
                return text;
            }
        }
    }
    // RFC 4122 v4 from getrandom.
    let mut bytes = [0u8; 16];
    unsafe { libc::getrandom(bytes.as_mut_ptr() as *mut c_void, 16, 0) };
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// `NM.VpnPluginInfo` metadata (.name files) and its optional editor plugin.
#[derive(Clone, Debug)]
pub struct VpnPluginInfo(pub glib::Object);

impl VpnPluginInfo {
    pub fn list_load() -> Vec<Self> {
        let Some(nm) = api() else { return Vec::new() };
        let mut out = Vec::new();
        unsafe {
            let list = (nm.nm_vpn_plugin_info_list_load)();
            let mut node = list;
            while !node.is_null() {
                if let Some(obj) = object((*node).data as Obj, true) {
                    out.push(Self(obj));
                }
                node = (*node).next;
            }
            gffi::g_slist_free(list);
        }
        out
    }

    /// `NM.VpnPluginInfo.new_search_file(None, service)`.
    pub fn search(service: &str) -> Option<Self> {
        let nm = api()?;
        let service = cstr(service);
        unsafe { object((nm.nm_vpn_plugin_info_new_search_file)(ptr::null(), service.as_ptr()), true).map(Self) }
    }

    pub fn service(&self) -> String {
        unsafe { opt_string((api().unwrap().nm_vpn_plugin_info_get_service)(self.0.as_ptr())).unwrap_or_default() }
    }
    pub fn name(&self) -> String {
        unsafe { opt_string((api().unwrap().nm_vpn_plugin_info_get_name)(self.0.as_ptr())).unwrap_or_default() }
    }
    pub fn auth_dialog(&self) -> Option<String> {
        unsafe { opt_string((api().unwrap().nm_vpn_plugin_info_get_auth_dialog)(self.0.as_ptr())) }
    }
    pub fn supports_hints(&self) -> bool {
        unsafe { (api().unwrap().nm_vpn_plugin_info_supports_hints)(self.0.as_ptr()) != 0 }
    }
    pub fn lookup_property(&self, group: &str, key: &str) -> Option<String> {
        let (group, key) = (cstr(group), cstr(key));
        unsafe { opt_string((api().unwrap().nm_vpn_plugin_info_lookup_property)(self.0.as_ptr(), group.as_ptr(), key.as_ptr())) }
    }

    /// libnm keeps its ownership and library validation checks.
    pub fn load_editor_plugin(&self) -> Result<EditorPlugin, glib::Error> {
        let nm = nm()?;
        unsafe {
            let mut error = ptr::null_mut();
            let raw = (nm.nm_vpn_plugin_info_load_editor_plugin)(self.0.as_ptr(), &mut error);
            object(raw, false).map(EditorPlugin).ok_or_else(|| take_error(error))
        }
    }
}

#[derive(Clone, Debug)]
pub struct EditorPlugin(pub glib::Object);

impl EditorPlugin {
    pub fn capabilities(&self) -> u32 {
        unsafe { (api().unwrap().nm_vpn_editor_plugin_get_capabilities)(self.0.as_ptr()) }
    }
    pub fn property(&self, name: &str) -> String {
        if self.0.find_property(name).is_none() {
            return String::new();
        }
        self.0.property_value(name).get::<Option<String>>().ok().flatten().unwrap_or_default()
    }
    pub fn import(&self, path: &str) -> Result<Connection, glib::Error> {
        let nm = nm()?;
        let path = cstr(path);
        unsafe {
            let mut error = ptr::null_mut();
            let raw = (nm.nm_vpn_editor_plugin_import)(self.0.as_ptr(), path.as_ptr(), &mut error);
            object(raw, true).map(Connection).ok_or_else(|| take_error(error))
        }
    }
    pub fn export(&self, path: &str, connection: &Connection) -> Result<bool, glib::Error> {
        let nm = nm()?;
        let path = cstr(path);
        unsafe {
            let mut error = ptr::null_mut();
            if (nm.nm_vpn_editor_plugin_export)(self.0.as_ptr(), path.as_ptr(), connection.0.as_ptr(), &mut error) != 0 {
                Ok(true)
            } else if error.is_null() {
                Ok(false)
            } else {
                Err(take_error(error))
            }
        }
    }
}

use glib::prelude::*;
