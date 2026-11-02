//! LockMonitor (osk_session.py) against a fake systemd unit on a private
//! `gio::TestDBus` bus — never the real session bus.
use gio::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_keyboard::session::{LockMonitor, SYSTEMD, UNIT_INTERFACE, UNIT_PATH};

const XML: &str = r#"<node><interface name="org.freedesktop.DBus.Properties">
<method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
<signal name="PropertiesChanged"><arg type="s"/><arg type="a{sv}"/><arg type="as"/></signal></interface></node>"#;

fn settle(seconds: f64, done: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end && !done() {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn changed(service: &gio::DBusConnection, interface: &str, state: &str) {
    let changes = glib::VariantDict::new(None);
    changes.insert_value("ActiveState", &state.to_variant());
    service.emit_signal(None, UNIT_PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                        Some(&glib::Variant::tuple_from_iter([interface.to_variant(), changes.end(), Vec::<String>::new().to_variant()]))).unwrap();
    service.flush_sync(None::<&gio::Cancellable>).unwrap();
}

fn main() {
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap();
    assert!(!address.is_empty());

    // Without any systemd on the bus: nothing is reported (NO_AUTO_START, error ignored).
    let reports = Rc::new(RefCell::new(Vec::<bool>::new()));
    let sink = reports.clone();
    let monitor = LockMonitor::new(move |locked| sink.borrow_mut().push(locked));
    settle(0.5, || false);
    assert!(reports.borrow().is_empty());
    monitor.close();

    let service = gio::DBusConnection::for_address_sync(&address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>).unwrap();
    let info = gio::DBusNodeInfo::for_xml(XML).unwrap().lookup_interface("org.freedesktop.DBus.Properties").unwrap();
    let initial = Rc::new(RefCell::new("activating".to_string()));
    let state = initial.clone();
    let _registration = service.register_object(UNIT_PATH, &info).method_call(move |_, _, _, _, method, parameters, invocation| {
        let (interface, property) = parameters.get::<(String, String)>().unwrap();
        assert_eq!((method, interface.as_str(), property.as_str()), ("Get", UNIT_INTERFACE, "ActiveState"));
        invocation.return_value(Some(&(state.borrow().to_variant(),).to_variant()));
    }).build().unwrap();
    service.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                      Some(&(SYSTEMD, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 1000, None::<&gio::Cancellable>).unwrap();

    let reports = Rc::new(RefCell::new(Vec::<bool>::new()));
    let sink = reports.clone();
    let monitor = LockMonitor::new(move |locked| sink.borrow_mut().push(locked));
    settle(2.0, || !reports.borrow().is_empty());
    assert_eq!(*reports.borrow(), [true], "initial activating = locked");
    changed(&service, UNIT_INTERFACE, "inactive");
    settle(2.0, || reports.borrow().len() >= 2);
    changed(&service, UNIT_INTERFACE, "failed");
    settle(2.0, || reports.borrow().len() >= 3);
    // Another interface is ignored.
    changed(&service, "org.freedesktop.systemd1.Service", "inactive");
    changed(&service, UNIT_INTERFACE, "deactivating");
    settle(2.0, || reports.borrow().len() >= 4);
    assert_eq!(*reports.borrow(), [true, false, true, true], "failed stays locked on change");
    monitor.close();
    changed(&service, UNIT_INTERFACE, "inactive");
    settle(0.3, || false);
    assert_eq!(reports.borrow().len(), 4, "closed monitors report nothing");

    // Initial "failed" is not locked (only signals treat failed as locked).
    *initial.borrow_mut() = "failed".into();
    let sink = reports.clone();
    let monitor = LockMonitor::new(move |locked| sink.borrow_mut().push(locked));
    settle(2.0, || reports.borrow().len() >= 5);
    assert_eq!(reports.borrow()[4], false);
    monitor.close();
    drop(service);
    bus.stop(); // down() would wait for the process-wide session connection.
    println!("ok lock_monitor");
}
