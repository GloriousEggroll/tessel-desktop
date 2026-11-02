//! FAKE logind / UPower / power-profiles-daemon / notification services for
//! a private bus (shared by tests/private_bus.rs and examples/fake_power_services.rs).
//! They only record calls: nothing is ever forwarded to the real system.
#![allow(dead_code)]

use gio::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default)]
pub struct World {
    pub calls: Vec<(String, String, String)>,
    pub on_battery: bool,
    pub battery: Option<HashMap<&'static str, Variant>>,
    pub active_profile: String,
    pub notifications: Vec<(String, String, u8)>,
    pub lid_closed: bool,
}

pub type Shared = Rc<RefCell<World>>;



fn connect(address: &str, name: &str) -> gio::DBusConnection {
    let connection = gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>)
        .expect("private bus");
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                         Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>)
        .expect("own name");
    connection
}

type Method = Rc<dyn Fn(&str, &Variant, gio::DBusMethodInvocation)>;
type Getter = Rc<dyn Fn(&str) -> Variant>;

fn export(connection: &gio::DBusConnection, path: &str, xml: &str, interface: &str, method: Method, getter: Getter) {
    let info = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let interface = info.lookup_interface(interface).unwrap();
    connection.register_object(path, &interface)
        .method_call(move |_, _, _, _, name, params, invocation| method(name, &params, invocation))
        .property(move |_, _, _, _, name| getter(name))
        .build().unwrap();
}

fn record(world: &Shared, interface: &str, method: &str, params: &Variant) {
    world.borrow_mut().calls.push((interface.into(), method.into(), params.print(false).to_string()));
}

pub fn serve(address: &str, world: &Shared) -> Vec<gio::DBusConnection> {
    let mut keep = Vec::new();
    // --- logind (fake: records calls, never forwards anything) ---
    let login = connect(address, "org.freedesktop.login1");
    let w = world.clone();
    export(&login, "/org/freedesktop/login1", r#"<node><interface name="org.freedesktop.login1.Manager">
        <method name="CanSuspend"><arg type="s" direction="out"/></method>
        <method name="CanHibernate"><arg type="s" direction="out"/></method>
        <method name="CanSuspendThenHibernate"><arg type="s" direction="out"/></method>
        <method name="CanPowerOff"><arg type="s" direction="out"/></method>
        <method name="ListInhibitors"><arg type="a(ssssuu)" direction="out"/></method>
        <method name="GetSession"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
        <method name="Inhibit"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/>
          <arg type="s" direction="in"/><arg type="h" direction="out"/></method>
        <method name="Suspend"><arg type="b" direction="in"/></method>
        <method name="Hibernate"><arg type="b" direction="in"/></method>
        <method name="SuspendThenHibernate"><arg type="b" direction="in"/></method>
        <method name="PowerOff"><arg type="b" direction="in"/></method>
        <property name="LidClosed" type="b" access="read"/><property name="Docked" type="b" access="read"/>
        <signal name="PrepareForSleep"><arg type="b"/></signal>
      </interface></node>"#, "org.freedesktop.login1.Manager",
        Rc::new(move |method, params, invocation| {
            record(&w, "login1", method, params);
            match method {
                m if m.starts_with("Can") => invocation.return_value(Some(&("yes",).to_variant())),
                "ListInhibitors" => invocation.return_value(Some(&(Vec::<(String, String, String, String, u32, u32)>::new(),).to_variant())),
                "GetSession" => invocation.return_value(Some(&(glib::variant::ObjectPath::try_from("/org/freedesktop/login1/session/auto").unwrap(),).to_variant())),
                "Inhibit" => {
                    let (read, write) = std::io::pipe().unwrap();
                    drop(write);
                    let list = gio::UnixFDList::new();
                    list.append(&read).unwrap();
                    invocation.return_value_with_unix_fd_list(Some(&(glib::variant::Handle(0),).to_variant()), Some(&list));
                }
                _ => invocation.return_value(None),
            }
        }),
        { let w = world.clone(); Rc::new(move |name| match name { "LidClosed" => w.borrow().lid_closed.to_variant(), _ => false.to_variant() }) });
    export(&login, "/org/freedesktop/login1/session/auto", r#"<node><interface name="org.freedesktop.login1.Session">
        <property name="Active" type="b" access="read"/><property name="Remote" type="b" access="read"/>
        <property name="Type" type="s" access="read"/></interface></node>"#, "org.freedesktop.login1.Session",
        Rc::new(|_, _, invocation| invocation.return_value(None)),
        Rc::new(|name| match name { "Active" => true.to_variant(), "Remote" => false.to_variant(), _ => "wayland".to_variant() }));
    keep.push(login);
    // --- UPower ---
    let upower = connect(address, "org.freedesktop.UPower");
    let w = world.clone();
    export(&upower, "/org/freedesktop/UPower", r#"<node><interface name="org.freedesktop.UPower">
        <method name="EnumerateDevices"><arg type="ao" direction="out"/></method>
        <property name="OnBattery" type="b" access="read"/><property name="LidIsPresent" type="b" access="read"/>
        <signal name="DeviceAdded"><arg type="o"/></signal><signal name="DeviceRemoved"><arg type="o"/></signal>
      </interface></node>"#, "org.freedesktop.UPower",
        Rc::new(move |method, params, invocation| {
            record(&w, "upower", method, params);
            let paths: Vec<glib::variant::ObjectPath> = if w.borrow().battery.is_some() {
                vec![glib::variant::ObjectPath::try_from("/org/freedesktop/UPower/devices/battery_BAT0").unwrap()]
            } else { vec![] };
            invocation.return_value(Some(&(paths,).to_variant()));
        }),
        { let w = world.clone(); Rc::new(move |name| match name { "OnBattery" => w.borrow().on_battery.to_variant(), _ => true.to_variant() }) });
    let w = world.clone();
    export(&upower, "/org/freedesktop/UPower/devices/battery_BAT0", r#"<node><interface name="org.freedesktop.UPower.Device">
        <property name="Type" type="u" access="read"/><property name="PowerSupply" type="b" access="read"/>
        <property name="IsPresent" type="b" access="read"/><property name="Percentage" type="d" access="read"/>
        <property name="WarningLevel" type="u" access="read"/><property name="State" type="u" access="read"/>
        <property name="NativePath" type="s" access="read"/><property name="Model" type="s" access="read"/>
      </interface></node>"#, "org.freedesktop.UPower.Device",
        Rc::new(|_, _, invocation| invocation.return_value(None)),
        Rc::new(move |name| w.borrow().battery.as_ref().and_then(|b| b.get(name).cloned()).unwrap_or_else(|| 0u32.to_variant())));
    keep.push(upower);
    // --- power-profiles-daemon ---
    let ppd = connect(address, "org.freedesktop.UPower.PowerProfiles");
    let info = gio::DBusNodeInfo::for_xml(r#"<node><interface name="org.freedesktop.UPower.PowerProfiles">
        <property name="Profiles" type="aa{sv}" access="read"/><property name="ActiveProfile" type="s" access="readwrite"/>
        <property name="PerformanceDegraded" type="s" access="read"/></interface></node>"#).unwrap();
    let interface = info.lookup_interface("org.freedesktop.UPower.PowerProfiles").unwrap();
    let (w1, w2) = (world.clone(), world.clone());
    ppd.register_object("/org/freedesktop/UPower/PowerProfiles", &interface)
        .method_call(|_, _, _, _, _, _, invocation| invocation.return_value(None))
        .property(move |_, _, _, _, name| match name {
            "Profiles" => {
                let rows: Vec<HashMap<String, Variant>> = ["power-saver", "balanced", "performance"].iter()
                    .map(|p| HashMap::from([("Profile".to_owned(), p.to_variant())])).collect();
                rows.to_variant()
            }
            "ActiveProfile" => w1.borrow().active_profile.to_variant(),
            _ => "".to_variant(),
        })
        .set_property(move |_, _, _, _, name, value| {
            record(&w2, "ppd", &format!("Set{name}"), &value);
            w2.borrow_mut().active_profile = value.get::<String>().unwrap_or_default();
            true
        })
        .build().unwrap();
    keep.push(ppd);
    // --- notifications ---
    let notes = connect(address, "org.freedesktop.Notifications");
    let w = world.clone();
    export(&notes, "/org/freedesktop/Notifications", r#"<node><interface name="org.freedesktop.Notifications">
        <method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
          <arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/>
          <arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
        <method name="CloseNotification"><arg type="u" direction="in"/></method>
        <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal></interface></node>"#, "org.freedesktop.Notifications",
        Rc::new(move |method, params, invocation| {
            if method == "Notify" {
                let summary: String = params.child_value(3).get().unwrap();
                let body: String = params.child_value(4).get().unwrap();
                let urgency = glib::VariantDict::new(Some(&params.child_value(6))).lookup::<u8>("urgency").ok().flatten().unwrap_or(9);
                w.borrow_mut().notifications.push((summary, body, urgency));
                let id = w.borrow().notifications.len() as u32;
                invocation.return_value(Some(&(id,).to_variant()));
            } else {
                invocation.return_value(None);
            }
        }), Rc::new(|_| "".to_variant()));
    keep.push(notes);
    keep
}

