//! Copied from components/network-manager/tests/support/fake_nm.rs (FAKE
//! NetworkManager for the private bus) so the Global Settings tests stay stable.
//! FAKE NetworkManager, Secret Service and notification server for a private
//! bus (tests/private_bus.rs, examples/fake_network_services.rs). They only
//! record calls and serve fixture data: nothing reaches the real system.
#![allow(dead_code)]

use gio::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub const NM: &str = "org.freedesktop.NetworkManager";

#[derive(Default)]
pub struct World {
    pub agent: String,
    pub calls: Vec<(String, String, String)>,
    pub secrets: Vec<(BTreeMap<String, String>, String)>,
    pub notifications: Vec<String>,
}

pub type Shared = Rc<RefCell<World>>;

pub fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>)
        .expect("private bus")
}

pub fn own(connection: &gio::DBusConnection, name: &str) {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                         Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).expect("own name");
}

pub fn release(connection: &gio::DBusConnection, name: &str) {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
                         Some(&(name,).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).expect("release");
}

fn tuple(items: &[Variant]) -> Variant {
    Variant::tuple_from_iter(items.iter().cloned())
}

pub fn o(path: &str) -> Variant {
    glib::variant::ObjectPath::try_from(path).unwrap().to_variant()
}

fn ao(paths: &[&str]) -> Variant {
    Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, paths.iter().map(|p| o(p)))
}

fn dict(items: Vec<(&str, Variant)>) -> Variant {
    let d = glib::VariantDict::new(None);
    for (k, v) in items {
        d.insert_value(k, &v);
    }
    d.end()
}

fn record(world: &Shared, interface: &str, method: &str, params: &Variant) {
    world.borrow_mut().calls.push((interface.into(), method.into(), params.print(false).to_string()));
}

pub fn wired_settings() -> Variant {
    let connection = dict(vec![("id", "Wired connection".to_variant()), ("uuid", "11111111-1111-4111-8111-111111111111".to_variant()),
                               ("type", "802-3-ethernet".to_variant()), ("interface-name", "enp4s0".to_variant()),
                               ("timestamp", 1789912800u64.to_variant())]);
    settings(vec![("connection", connection), ("802-3-ethernet", dict(vec![])), ("ipv4", dict(vec![("method", "auto".to_variant())])),
                  ("ipv6", dict(vec![("method", "auto".to_variant())]))])
}

pub fn wifi_settings() -> Variant {
    let connection = dict(vec![("id", "Home Wi-Fi".to_variant()), ("uuid", "22222222-2222-4222-8222-222222222222".to_variant()),
                               ("type", "802-11-wireless".to_variant())]);
    let wireless = dict(vec![("ssid", b"Home Wi-Fi".to_vec().to_variant()), ("mode", "infrastructure".to_variant())]);
    let security = dict(vec![("key-mgmt", "wpa-psk".to_variant()), ("psk-flags", 1u32.to_variant())]);
    settings(vec![("connection", connection), ("802-11-wireless", wireless), ("802-11-wireless-security", security),
                  ("ipv4", dict(vec![("method", "auto".to_variant())])), ("ipv6", dict(vec![("method", "auto".to_variant())]))])
}

fn settings(items: Vec<(&str, Variant)>) -> Variant {
    Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(),
                                       items.into_iter().map(|(k, v)| Variant::from_dict_entry(&k.to_variant(), &v)))
}

/// `{path: {iface: props}}` of the fixture: wired (connected) + Wi-Fi adapter + one AP.
pub fn objects(networking: bool) -> Variant {
    let mut objects: Vec<(&str, Vec<(&str, Variant)>)> = Vec::new();
    objects.push(("/org/freedesktop/NetworkManager", vec![(NM, dict(vec![
        ("NetworkingEnabled", networking.to_variant()), ("WirelessEnabled", true.to_variant()), ("WirelessHardwareEnabled", true.to_variant()),
        ("WwanEnabled", true.to_variant()), ("WwanHardwareEnabled", true.to_variant()), ("Connectivity", 4u32.to_variant()),
        ("ConnectivityCheckUri", "http://check.invalid/".to_variant()),
        ("Devices", ao(&["/org/freedesktop/NetworkManager/Devices/10", "/org/freedesktop/NetworkManager/Devices/11"])),
        ("ActiveConnections", ao(&["/org/freedesktop/NetworkManager/ActiveConnection/10"]))]))]));
    objects.push(("/org/freedesktop/NetworkManager/Settings", vec![("org.freedesktop.NetworkManager.Settings", dict(vec![
        ("Connections", ao(&["/org/freedesktop/NetworkManager/Settings/1", "/org/freedesktop/NetworkManager/Settings/2"])),
        ("Hostname", "fixture".to_variant())]))]));
    for path in ["/org/freedesktop/NetworkManager/Settings/1", "/org/freedesktop/NetworkManager/Settings/2"] {
        objects.push((path, vec![("org.freedesktop.NetworkManager.Settings.Connection", dict(vec![
            ("Flags", 0u32.to_variant()), ("VersionId", 3u64.to_variant()), ("Unsaved", false.to_variant())]))]));
    }
    let device = |iface: &str, kind: u32, state: u32, active: &str| dict(vec![
        ("Interface", iface.to_variant()), ("DeviceType", kind.to_variant()), ("State", state.to_variant()),
        ("StateReason", (state, 0u32).to_variant()), ("HwAddress", "02:00:00:00:00:10".to_variant()), ("Driver", "fixture".to_variant()),
        ("Mtu", 1500u32.to_variant()), ("Managed", true.to_variant()), ("Ip4Config", o("/")), ("Ip6Config", o("/")),
        ("Ifindex", 0u32.to_variant()), ("ActiveConnection", o(active))]);
    objects.push(("/org/freedesktop/NetworkManager/Devices/10", vec![
        ("org.freedesktop.NetworkManager.Device", device("enp4s0", 1, 100, "/org/freedesktop/NetworkManager/ActiveConnection/10")),
        ("org.freedesktop.NetworkManager.Device.Wired", dict(vec![("Carrier", true.to_variant()), ("Speed", 1000u32.to_variant())]))]));
    objects.push(("/org/freedesktop/NetworkManager/Devices/11", vec![
        ("org.freedesktop.NetworkManager.Device", device("wlan0", 2, 30, "/")),
        ("org.freedesktop.NetworkManager.Device.Wireless", dict(vec![
            ("AccessPoints", ao(&["/org/freedesktop/NetworkManager/AccessPoint/1"])), ("ActiveAccessPoint", o("/")),
            ("Bitrate", 0u32.to_variant()), ("LastScan", 100i64.to_variant())]))]));
    objects.push(("/org/freedesktop/NetworkManager/AccessPoint/1", vec![("org.freedesktop.NetworkManager.AccessPoint", dict(vec![
        ("Ssid", b"Home Wi-Fi".to_vec().to_variant()), ("Strength", 89u8.to_variant()), ("Frequency", 5180u32.to_variant()),
        ("HwAddress", "02:00:00:00:00:99".to_variant()), ("Flags", 1u32.to_variant()), ("WpaFlags", 0u32.to_variant()),
        ("RsnFlags", 0x100u32.to_variant()), ("Mode", 2u32.to_variant())]))]));
    objects.push(("/org/freedesktop/NetworkManager/ActiveConnection/10", vec![("org.freedesktop.NetworkManager.Connection.Active", dict(vec![
        ("Id", "Wired connection".to_variant()), ("Uuid", "11111111-1111-4111-8111-111111111111".to_variant()),
        ("Type", "802-3-ethernet".to_variant()), ("Devices", ao(&["/org/freedesktop/NetworkManager/Devices/10"])),
        ("State", 2u32.to_variant()), ("StateFlags", 0u32.to_variant()), ("Default", true.to_variant()), ("Default6", false.to_variant()),
        ("Vpn", false.to_variant()), ("Ip4Config", o("/")), ("Ip6Config", o("/"))]))]));
    let entries = objects.into_iter().map(|(path, ifaces)| {
        let inner = Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(),
            ifaces.into_iter().map(|(i, p)| Variant::from_dict_entry(&i.to_variant(), &p)));
        Variant::from_dict_entry(&o(path), &inner)
    });
    tuple(&[Variant::array_from_iter_with_type(glib::VariantTy::new("{oa{sa{sv}}}").unwrap(), entries)])
}

type Method = Rc<dyn Fn(&str, &Variant, gio::DBusMethodInvocation)>;

fn export(connection: &gio::DBusConnection, path: &str, xml: &str, interface: &str, method: Method,
          getter: Option<Rc<dyn Fn(&str) -> Variant>>) -> gio::RegistrationId {
    let info = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let interface = info.lookup_interface(interface).unwrap();
    let builder = connection.register_object(path, &interface)
        .method_call(move |_, _, _, _, name, params, invocation| method(name, &params, invocation));
    let builder = match getter {
        Some(getter) => builder.property(move |_, _, _, _, name| getter(name)),
        None => builder,
    };
    builder.build().unwrap()
}

pub struct FakeNm {
    pub connection: gio::DBusConnection,
    pub networking: Rc<RefCell<bool>>,
}

impl FakeNm {
    /// Emit `PropertiesChanged` on the NetworkManager root object.
    pub fn root_changed(&self, name: &str, value: Variant) {
        self.connection.emit_signal(None, "/org/freedesktop/NetworkManager", "org.freedesktop.DBus.Properties", "PropertiesChanged",
            Some(&tuple(&[NM.to_variant(), dict(vec![(name, value)]), Vec::<String>::new().to_variant()]))).unwrap();
    }
}

pub fn serve_nm(address: &str, world: &Shared) -> FakeNm {
    let connection = connect(address);
    let networking = Rc::new(RefCell::new(true));
    let (w, n) = (world.clone(), networking.clone());
    export(&connection, "/org/freedesktop", r#"<node><interface name="org.freedesktop.DBus.ObjectManager">
        <method name="GetManagedObjects"><arg type="a{oa{sa{sv}}}" direction="out"/></method>
        <signal name="InterfacesAdded"><arg type="o"/><arg type="a{sa{sv}}"/></signal>
        <signal name="InterfacesRemoved"><arg type="o"/><arg type="as"/></signal></interface></node>"#,
        "org.freedesktop.DBus.ObjectManager", Rc::new(move |method, params, invocation| {
            record(&w, "ObjectManager", method, params);
            invocation.return_value(Some(&objects(*n.borrow())));
        }), None);
    for (path, settings) in [("/org/freedesktop/NetworkManager/Settings/1", wired_settings()), ("/org/freedesktop/NetworkManager/Settings/2", wifi_settings())] {
        let w = world.clone();
        export(&connection, path, r#"<node><interface name="org.freedesktop.NetworkManager.Settings.Connection">
            <method name="GetSettings"><arg type="a{sa{sv}}" direction="out"/></method>
            <method name="GetSecrets"><arg type="s" direction="in"/><arg type="a{sa{sv}}" direction="out"/></method>
            <method name="Delete"/><method name="Update2"><arg type="a{sa{sv}}" direction="in"/><arg type="u" direction="in"/>
              <arg type="a{sv}" direction="in"/><arg type="a{sv}" direction="out"/></method>
            <signal name="Updated"/></interface></node>"#, "org.freedesktop.NetworkManager.Settings.Connection",
            Rc::new(move |method, params, invocation| {
                record(&w, "Settings.Connection", method, params);
                match method {
                    "GetSettings" => invocation.return_value(Some(&tuple(&[settings.clone()]))),
                    _ => invocation.return_dbus_error("org.freedesktop.NetworkManager.PermissionDenied", "fixture is read-only"),
                }
            }), None);
    }
    let w = world.clone();
    export(&connection, "/org/freedesktop/NetworkManager", r#"<node><interface name="org.freedesktop.NetworkManager">
        <method name="Enable"><arg type="b" direction="in"/></method>
        <method name="ActivateConnection"><arg type="o" direction="in"/><arg type="o" direction="in"/><arg type="o" direction="in"/><arg type="o" direction="out"/></method>
        <method name="AddAndActivateConnection"><arg type="a{sa{sv}}" direction="in"/><arg type="o" direction="in"/><arg type="o" direction="in"/>
          <arg type="o" direction="out"/><arg type="o" direction="out"/></method>
        <method name="DeactivateConnection"><arg type="o" direction="in"/></method>
        <property name="WirelessEnabled" type="b" access="readwrite"/></interface></node>"#, NM,
        Rc::new(move |method, params, invocation| {
            record(&w, "NetworkManager", method, params);
            invocation.return_dbus_error("org.freedesktop.NetworkManager.PermissionDenied", "fixture is read-only");
        }), Some(Rc::new(|_| true.to_variant())));
    // Global Settings addition: per-adapter Wi-Fi scans are recorded (and rejected).
    let w = world.clone();
    export(&connection, "/org/freedesktop/NetworkManager/Devices/11", r#"<node><interface name="org.freedesktop.NetworkManager.Device.Wireless">
        <method name="RequestScan"><arg type="a{sv}" direction="in"/></method></interface></node>"#,
        "org.freedesktop.NetworkManager.Device.Wireless", Rc::new(move |method, params, invocation| {
            record(&w, "Device.Wireless", method, params);
            invocation.return_dbus_error("org.freedesktop.NetworkManager.PermissionDenied", "fixture is read-only");
        }), None);
    let w = world.clone();
    let info = gio::DBusNodeInfo::for_xml(r#"<node><interface name="org.freedesktop.NetworkManager.AgentManager">
        <method name="RegisterWithCapabilities"><arg type="s" direction="in"/><arg type="u" direction="in"/></method>
        <method name="Unregister"/></interface></node>"#).unwrap();
    connection.register_object("/org/freedesktop/NetworkManager/AgentManager", &info.lookup_interface("org.freedesktop.NetworkManager.AgentManager").unwrap())
        .method_call(move |_, sender, _, _, method, params, invocation| {
            record(&w, "AgentManager", method, &params);
            if method == "RegisterWithCapabilities" {
                w.borrow_mut().agent = sender.unwrap_or_default().to_owned();
            }
            invocation.return_value(None);
        }).build().unwrap();
    own(&connection, NM);
    FakeNm { connection, networking }
}

/// A minimal Secret Service: one unlocked item with a synthetic psk.
pub fn serve_secrets(address: &str, world: &Shared) -> gio::DBusConnection {
    let connection = connect(address);
    let w = world.clone();
    export(&connection, "/org/freedesktop/secrets", r#"<node><interface name="org.freedesktop.Secret.Service">
        <method name="OpenSession"><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="v" direction="out"/><arg type="o" direction="out"/></method>
        <method name="SearchItems"><arg type="a{ss}" direction="in"/><arg type="ao" direction="out"/><arg type="ao" direction="out"/></method>
        <method name="GetSecrets"><arg type="ao" direction="in"/><arg type="o" direction="in"/><arg type="a{o(oayays)}" direction="out"/></method>
        <method name="ReadAlias"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
        <method name="Unlock"><arg type="ao" direction="in"/><arg type="ao" direction="out"/><arg type="o" direction="out"/></method>
        </interface></node>"#, "org.freedesktop.Secret.Service", Rc::new(move |method, params, invocation| {
            record(&w, "Secret.Service", method, params);
            let empty = || Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, Vec::<Variant>::new());
            match method {
                "OpenSession" => invocation.return_value(Some(&tuple(&["".to_variant().to_variant(), o("/org/freedesktop/secrets/session/1")]))),
                "SearchItems" => {
                    let attrs: std::collections::HashMap<String, String> = params.child_value(0).get().unwrap_or_default();
                    let matches = w.borrow().secrets.iter().any(|(a, _)| attrs.iter().all(|(k, v)| a.get(k) == Some(v)));
                    let found = if matches { ao(&["/org/freedesktop/secrets/collection/login/1"]) } else { empty() };
                    invocation.return_value(Some(&tuple(&[found, empty()])));
                }
                "GetSecrets" => {
                    let value = w.borrow().secrets.first().map(|s| s.1.clone()).unwrap_or_default();
                    let secret = tuple(&[o("/org/freedesktop/secrets/session/1"), Vec::<u8>::new().to_variant(),
                                         value.into_bytes().to_variant(), "text/plain".to_variant()]);
                    let entry = Variant::from_dict_entry(&o("/org/freedesktop/secrets/collection/login/1"), &secret);
                    invocation.return_value(Some(&tuple(&[Variant::array_from_iter_with_type(glib::VariantTy::new("{o(oayays)}").unwrap(), [entry])])));
                }
                "ReadAlias" => invocation.return_value(Some(&tuple(&[o("/org/freedesktop/secrets/collection/login")]))),
                _ => invocation.return_value(Some(&tuple(&[empty(), o("/")]))),
            }
        }), None);
    let w = world.clone();
    export(&connection, "/org/freedesktop/secrets/collection/login/1", r#"<node><interface name="org.freedesktop.Secret.Item">
        <method name="Delete"><arg type="o" direction="out"/></method>
        <property name="Attributes" type="a{ss}" access="read"/></interface></node>"#, "org.freedesktop.Secret.Item",
        Rc::new(move |method, params, invocation| {
            record(&w, "Secret.Item", method, params);
            invocation.return_value(Some(&tuple(&[o("/")])));
        }), Some({
            let w = world.clone();
            Rc::new(move |_| {
                let attrs = w.borrow().secrets.first().map(|s| s.0.clone()).unwrap_or_default();
                let map: std::collections::HashMap<String, String> = attrs.into_iter().collect();
                map.to_variant()
            })
        }));
    own(&connection, "org.freedesktop.secrets");
    connection
}

/// org.freedesktop.Notifications: records summaries / closes.
pub fn serve_notifications(address: &str, world: &Shared) -> gio::DBusConnection {
    let connection = connect(address);
    let w = world.clone();
    export(&connection, "/org/freedesktop/Notifications", r#"<node><interface name="org.freedesktop.Notifications">
        <method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/>
          <arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
        <method name="CloseNotification"><arg type="u" direction="in"/></method>
        <method name="GetCapabilities"><arg type="as" direction="out"/></method>
        <method name="GetServerInformation"><arg type="s" direction="out"/><arg type="s" direction="out"/><arg type="s" direction="out"/><arg type="s" direction="out"/></method>
        <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal><signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
        </interface></node>"#, "org.freedesktop.Notifications", Rc::new(move |method, params, invocation| {
            record(&w, "Notifications", method, params);
            match method {
                "Notify" => {
                    let summary = params.child_value(3).str().unwrap_or_default().to_owned();
                    let actions: Vec<String> = params.child_value(5).get().unwrap_or_default();
                    w.borrow_mut().notifications.push(format!("{summary}|{}", actions.join(",")));
                    invocation.return_value(Some(&(7u32,).to_variant()));
                }
                "GetCapabilities" => invocation.return_value(Some(&(vec!["actions", "body"],).to_variant())),
                "GetServerInformation" => invocation.return_value(Some(&("fake", "test", "1", "1.2").to_variant())),
                _ => {
                    w.borrow_mut().notifications.push("closed".into());
                    invocation.return_value(None);
                }
            }
        }), None);
    own(&connection, "org.freedesktop.Notifications");
    connection
}
