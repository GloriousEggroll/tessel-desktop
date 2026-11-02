//! FAKE `org.bluez` (on `$DBUS_SYSTEM_BUS_ADDRESS`) and `org.bluez.obex`
//! agent manager (on the session bus) for headless runs and measurements.
//! Serves the test_ui.py "populated" sample; discovery/connect/trust calls
//! succeed and only change this process's memory. Refuses to start unless the
//! "system" bus address is set explicitly to a non-default (private) bus, so
//! it can never claim names on the real system bus. Usage: fake_bluez [ready-file]

use tessel_bluetooth::backend::*;
use tessel_bluetooth::testing::{adapter_props, device_props, managed_objects, sample};
use gio::prelude::*;
use glib::Variant;
use std::rc::Rc;

fn serve(conn: &gio::DBusConnection, path: &str, xml: &str, reply: impl Fn(&str, &Variant) -> Option<Variant> + 'static) {
    let info = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let iface = info.interfaces()[0].clone();
    conn.register_object(path, &iface).method_call(move |_, _, _, _, method, params, invocation| {
        invocation.return_value(reply(method, &params).as_ref());
    }).build().unwrap();
}

fn own(conn: &gio::DBusConnection, name: &str) {
    conn.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                   Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>).unwrap();
}

fn main() {
    let system = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_default();
    if system.is_empty() || system.contains("/run/dbus/system_bus_socket") || std::env::var("XDG_RUNTIME_DIR").as_deref() == Ok("/run/user/1000") {
        eprintln!("fake_bluez: refusing to run without a private DBUS_SYSTEM_BUS_ADDRESS / XDG_RUNTIME_DIR");
        std::process::exit(2);
    }
    let conn = gio::DBusConnection::for_address_sync(&system, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>).expect("private system bus");
    let state = Rc::new(sample("populated"));
    let s = state.clone();
    serve(&conn, BLUEZ_ROOT, r#"<node><interface name="org.freedesktop.DBus.ObjectManager">
<method name="GetManagedObjects"><arg type="a{oa{sa{sv}}}" direction="out"/></method></interface></node>"#,
          move |_, _| Some(managed_objects(&s.adapters, &s.devices)));
    let props_xml = r#"<node><interface name="org.freedesktop.DBus.Properties">
<method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method>
<method name="Set"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/></method></interface></node>"#;
    for (path, adapter) in state.adapters.iter() {
        let ad = adapter.clone();
        serve(&conn, path, props_xml, move |m, _| (m == "GetAll").then(|| Variant::tuple_from_iter([adapter_props(&ad)])));
        serve(&conn, path, r#"<node><interface name="org.bluez.Adapter1"><method name="StartDiscovery"/><method name="StopDiscovery"/>
<method name="RemoveDevice"><arg type="o" direction="in"/></method></interface></node>"#, |_, _| None);
    }
    for (path, device) in state.devices.iter() {
        let dev = device.clone();
        serve(&conn, path, props_xml, move |m, _| (m == "GetAll").then(|| Variant::tuple_from_iter([device_props(&dev)])));
        serve(&conn, path, r#"<node><interface name="org.bluez.Device1"><method name="Connect"/><method name="Disconnect"/>
<method name="Pair"/></interface></node>"#, |_, _| None);
    }
    own(&conn, BLUEZ);
    let session = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).expect("session bus");
    serve(&session, "/org/bluez/obex", r#"<node><interface name="org.bluez.obex.AgentManager1">
<method name="RegisterAgent"><arg type="o" direction="in"/></method><method name="UnregisterAgent"><arg type="o" direction="in"/></method>
</interface></node>"#, |_, _| None);
    own(&session, "org.bluez.obex");
    if let Some(ready) = std::env::args().nth(1) {
        std::fs::write(ready, "ready").unwrap();
    }
    glib::MainLoop::new(None, false).run();
}
