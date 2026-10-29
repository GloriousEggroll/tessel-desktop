//! WorkareaClient against a FAKE org.waydock.Waydock on a private
//! dbus-daemon (gio::TestDBus): initial GetWorkareas, owner-only signals,
//! malformed payloads ignored, owner loss clears, a new owner is re-queried.
#![cfg(feature = "desktop")]

use gio::prelude::*;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use wayshell::desktop::workarea::{WorkareaClient, WorkareaSource, BUS_NAME, INTERFACE, OBJECT_PATH};

const XML: &str = r#"<node><interface name="org.waydock.Settings">
<method name="GetWorkareas"><arg type="s" direction="out"/></method>
<signal name="WorkareasChanged"><arg type="s"/></signal></interface></node>"#;

fn payload(y: i64) -> String {
    json!({"version": 1, "monitors": [{"name": "HEADLESS-1", "geometry": [0, 0, 1280, 720], "workarea": [0, y, 1280, 720 - y]}]}).to_string()
}

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, gio::Cancellable::NONE).unwrap()
}

struct FakeWaydock {
    connection: gio::DBusConnection,
    owner: gio::OwnerId,
    registration: gio::RegistrationId,
}

fn fake_waydock(address: &str, reply: Rc<RefCell<String>>) -> FakeWaydock {
    let connection = connect(address);
    let info = gio::DBusNodeInfo::for_xml(XML).unwrap().lookup_interface(INTERFACE).unwrap();
    let registration = connection
        .register_object(OBJECT_PATH, &info)
        .method_call(move |_, _, _, _, method, _, invocation| {
            assert_eq!(method, "GetWorkareas");
            invocation.return_value(Some(&(reply.borrow().clone(),).to_variant()));
        })
        .build()
        .unwrap();
    let owner = gio::bus_own_name_on_connection(&connection, BUS_NAME, gio::BusNameOwnerFlags::NONE, |_, _| {}, |_, _| {});
    FakeWaydock { connection, owner, registration }
}

fn wait(context: &glib::MainContext, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while !done() && Instant::now() < end {
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
    done()
}

fn emit(connection: &gio::DBusConnection, text: &str) {
    connection.emit_signal(None, OBJECT_PATH, INTERFACE, "WorkareasChanged", Some(&(text.to_owned(),).to_variant())).unwrap();
    connection.flush_sync(gio::Cancellable::NONE).unwrap();
}

#[test]
fn workarea_client_follows_the_owner_on_a_private_bus() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
            // and in spawned children must not look up portals or gvfs.
            for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
                std::env::set_var(key, value);
            }
            let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
            bus.up();
            let address = bus.bus_address().unwrap().to_string();
            assert!(!address.contains("/run/user/1000/bus"), "must be a private bus");
            let reply = Rc::new(RefCell::new(payload(40)));
            let mut waydock = Some(fake_waydock(&address, reply.clone()));
            let changes = Rc::new(Cell::new(0));
            let c = changes.clone();
            let client = WorkareaClient::with_connection(&connect(&address), move || c.set(c.get() + 1));
            client.start();
            assert!(wait(&context, || !client.known().is_empty()), "initial GetWorkareas");
            assert_eq!(client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)), (0, 40, 1280, 680));
            assert_eq!(client.rectangle(None, (0, 0, 1280, 720)), (0, 40, 1280, 680), "unique geometry fallback");
            assert_eq!(client.rectangle(Some("HEADLESS-1"), (0, 0, 1920, 1080)), (0, 0, 1920, 1080));

            // A live signal from the owner replaces the areas.
            emit(&waydock.as_ref().unwrap().connection, &payload(52));
            assert!(wait(&context, || client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)).1 == 52));
            // Another peer's signal (not the name owner) is ignored.
            let intruder = connect(&address);
            emit(&intruder, &payload(99));
            let settle = |ms| { let end = Instant::now() + Duration::from_millis(ms); while Instant::now() < end { context.iteration(false); std::thread::sleep(Duration::from_millis(2)); } };
            settle(200);
            assert_eq!(client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)).1, 52, "foreign sender ignored");
            // Malformed payload from the owner is ignored too.
            emit(&waydock.as_ref().unwrap().connection, "{\"version\": 2}");
            settle(150);
            assert_eq!(client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)).1, 52, "malformed ignored");
            emit(&waydock.as_ref().unwrap().connection, &payload(60));
            assert!(wait(&context, || client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)).1 == 60));
            assert_eq!(client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)).1, 60);

            // Owner loss clears every area.
            let fake = waydock.take().unwrap();
            gio::bus_unown_name(fake.owner);
            let _ = fake.connection.unregister_object(fake.registration);
            fake.connection.close_sync(gio::Cancellable::NONE).unwrap();
            assert!(wait(&context, || client.known().is_empty()), "owner loss clears");
            assert_eq!(client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)), (0, 0, 1280, 720));

            // A restarted Waydock is queried again.
            reply.replace(payload(24));
            let _again = fake_waydock(&address, reply.clone());
            assert!(wait(&context, || client.rectangle(Some("HEADLESS-1"), (0, 0, 1280, 720)).1 == 24));
            assert!(changes.get() >= 5);
            client.close();
            assert!(client.known().is_empty() && client.is_closed());
            drop(intruder);
            bus.down();
        })
        .unwrap();
}
