//! FAKE localed + AccountsService for tests/measure_headless.sh: serves fixed
//! read-only values on $DBUS_SESSION_BUS_ADDRESS (a private bus); setters are
//! only logged. Writes argv[1] once the names are owned.

use gio::prelude::*;

fn interface(xml: &str, name: &str) -> gio::DBusInterfaceInfo {
    gio::DBusNodeInfo::for_xml(xml).unwrap().lookup_interface(name).unwrap()
}

fn main() {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("private bus address");
    let connection = gio::DBusConnection::for_address_sync(&address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>)
        .unwrap();
    for name in ["org.freedesktop.locale1", "org.freedesktop.Accounts"] {
        connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                             Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).unwrap();
    }
    let log = |method: &str, params: &glib::Variant| eprintln!("fake: {method}{}", params.print(false));
    connection.register_object("/org/freedesktop/locale1", &interface(r#"<node><interface name="org.freedesktop.locale1">
        <method name="SetLocale"><arg type="as" direction="in"/><arg type="b" direction="in"/></method>
        <property name="Locale" type="as" access="read"/></interface></node>"#, "org.freedesktop.locale1"))
        .method_call(move |_, _, _, _, method, params, invocation| { log(method, &params); invocation.return_value(None) })
        .property(|_, _, _, _, _| vec!["LANG=en_US.UTF-8".to_owned()].to_variant())
        .build().unwrap();
    connection.register_object("/org/freedesktop/Accounts", &interface(r#"<node><interface name="org.freedesktop.Accounts">
        <method name="FindUserById"><arg type="x" direction="in"/><arg type="o" direction="out"/></method>
        </interface></node>"#, "org.freedesktop.Accounts"))
        .method_call(|_, _, _, _, _, _, invocation| {
            invocation.return_value(Some(&(glib::variant::ObjectPath::try_from("/org/freedesktop/Accounts/User1000").unwrap(),).to_variant()))
        }).build().unwrap();
    connection.register_object("/org/freedesktop/Accounts/User1000", &interface(r#"<node><interface name="org.freedesktop.Accounts.User">
        <method name="SetLanguage"><arg type="s" direction="in"/></method>
        <property name="Language" type="s" access="read"/></interface></node>"#, "org.freedesktop.Accounts.User"))
        .method_call(move |_, _, _, _, method, params, invocation| { log(method, &params); invocation.return_value(None) })
        .property(|_, _, _, _, _| "en_US.UTF-8".to_variant())
        .build().unwrap();
    if let Some(ready) = std::env::args().nth(1) {
        std::fs::write(ready, b"").unwrap();
    }
    glib::MainLoop::new(None, false).run();
}
