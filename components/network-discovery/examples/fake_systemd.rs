//! A fake `org.freedesktop.systemd1` for measurements (tests/measure_headless.sh):
//! serves avahi-daemon.socket/.service (socket active) on the bus named by
//! DBUS_SYSTEM_BUS_ADDRESS, which must be a private bus. Touches the file
//! given as argv[1] once the name is owned.

use gio::prelude::*;

const MANAGER_XML: &str = r#"<node><interface name="org.freedesktop.systemd1.Manager">
  <method name="Subscribe"/><method name="Unsubscribe"/>
  <method name="LoadUnit"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
</interface></node>"#;
const UNIT_XML: &str = r#"<node><interface name="org.freedesktop.systemd1.Unit">
  <property name="LoadState" type="s" access="read"/><property name="ActiveState" type="s" access="read"/>
  <property name="UnitFileState" type="s" access="read"/></interface></node>"#;

fn unit_path(unit: &str) -> String {
    let mut path = String::from("/org/freedesktop/systemd1/unit/");
    for byte in unit.bytes() {
        if byte.is_ascii_alphanumeric() { path.push(byte as char) } else { path.push_str(&format!("_{byte:02x}")) }
    }
    path
}

fn main() {
    let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").expect("DBUS_SYSTEM_BUS_ADDRESS");
    assert!(!address.contains("/run/dbus/system_bus_socket"), "refusing the real system bus");
    let ready = std::env::args().nth(1).expect("ready file");
    let main_loop = glib::MainLoop::new(None, false);
    let connection = gio::DBusConnection::for_address_sync(&address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None::<&gio::DBusAuthObserver>, None::<&gio::Cancellable>).unwrap();
    let manager = gio::DBusNodeInfo::for_xml(MANAGER_XML).unwrap();
    let _m = connection.register_object("/org/freedesktop/systemd1", &manager.lookup_interface("org.freedesktop.systemd1.Manager").unwrap())
        .method_call(|_, _, _, _, method, params, invocation| {
            if method == "LoadUnit" {
                let (unit,) = params.get::<(String,)>().unwrap();
                let path = glib::variant::ObjectPath::try_from(unit_path(&unit)).unwrap();
                invocation.return_value(Some(&(path,).to_variant()));
            } else {
                invocation.return_value(None);
            }
        }).build().unwrap();
    let units = gio::DBusNodeInfo::for_xml(UNIT_XML).unwrap();
    let interface = units.lookup_interface("org.freedesktop.systemd1.Unit").unwrap();
    let mut ids = Vec::new();
    for (unit, active) in [("avahi-daemon.socket", "active"), ("avahi-daemon.service", "inactive")] {
        ids.push(connection.register_object(&unit_path(unit), &interface).property(move |_, _, _, _, name| match name {
            "LoadState" => "loaded".to_variant(),
            "ActiveState" => active.to_variant(),
            _ => "enabled".to_variant(),
        }).build().unwrap());
    }
    let _owner = gio::bus_own_name_on_connection(&connection, "org.freedesktop.systemd1", gio::BusNameOwnerFlags::NONE,
        move |_, _| { std::fs::write(&ready, b"").unwrap(); }, |_, _| {});
    main_loop.run();
}
