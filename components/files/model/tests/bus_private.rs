//! test_file_manager_bus.py FileManagerBusTests on a PRIVATE bus
//! (gio::TestDBus: its own dbus-daemon, no service directories, so nothing
//! can be activated). Never the real session bus.

use tessel_files_model::bus::{FileManagerBus, FileManagerHost, BUS_NAME, INTERFACE, OBJECT_PATH};
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
enum Call {
    Open(Vec<String>, bool, bool),
    Properties(Vec<String>),
}

struct Host {
    bus: gio::DBusConnection,
    calls: RefCell<Vec<Call>>,
}

impl FileManagerHost for Host {
    fn connection(&self) -> Option<gio::DBusConnection> {
        Some(self.bus.clone())
    }
    fn open_locations(&self, files: Vec<gio::File>, select: bool, new_window: bool) -> Result<(), String> {
        self.calls.borrow_mut().push(Call::Open(files.iter().map(|f| f.uri().to_string()).collect(), select, new_window));
        Ok(())
    }
    fn show_properties(&self, files: Vec<gio::File>) -> Result<(), String> {
        self.calls.borrow_mut().push(Call::Properties(files.iter().map(|f| f.uri().to_string()).collect()));
        Ok(())
    }
}

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(
        address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        None::<&gio::Cancellable>,
    )
    .expect("private bus connection")
}

fn wait(context: &glib::MainContext, what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("Timed out waiting for FileManager1: {what}");
}

fn daemon(client: &gio::DBusConnection, method: &str, parameters: glib::Variant) -> glib::Variant {
    client
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            Some(&parameters),
            None,
            gio::DBusCallFlags::NONE,
            1500,
            None::<&gio::Cancellable>,
        )
        .expect(method)
}

fn has_owner(client: &gio::DBusConnection) -> bool {
    daemon(client, "NameHasOwner", (BUS_NAME,).to_variant()).child_value(0).get::<bool>().unwrap()
}

fn call(context: &glib::MainContext, client: &gio::DBusConnection, method: &str, uris: &[&str]) -> Result<(), glib::Error> {
    let result: Rc<RefCell<Option<Result<(), glib::Error>>>> = Rc::default();
    let slot = result.clone();
    let uris: Vec<String> = uris.iter().map(|u| (*u).to_owned()).collect();
    client.call(
        Some(BUS_NAME),
        OBJECT_PATH,
        INTERFACE,
        method,
        Some(&(uris, "").to_variant()),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        1500,
        None::<&gio::Cancellable>,
        move |reply| {
            slot.replace(Some(reply.map(|_| ())));
        },
    );
    wait(context, method, || result.borrow().is_some());
    let value = result.borrow_mut().take().unwrap();
    value
}

struct Case {
    host: Rc<Host>,
    client: gio::DBusConnection,
    is_default: Rc<Cell<bool>>,
    service: FileManagerBus,
}

fn case(address: &str) -> Case {
    let host = Rc::new(Host { bus: connect(address), calls: RefCell::default() });
    let client = connect(address);
    let is_default = Rc::new(Cell::new(true));
    let flag = is_default.clone();
    let service = FileManagerBus::new(host.clone(), Some(Box::new(move || flag.get())));
    Case { host, client, is_default, service }
}

fn finish(case: Case) {
    case.service.close();
    for connection in [&case.host.bus, &case.client] {
        if !connection.is_closed() {
            let _ = connection.close_sync(None::<&gio::Cancellable>);
        }
    }
}

#[test]
fn file_manager_bus_on_private_bus() {
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().expect("private bus address").to_string();
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            // test_open_select_properties_validation_and_close
            let c = case(&address);
            assert!(c.service.start());
            wait(&context, "available", || c.service.available());
            call(&context, &c.client, "ShowFolders", &["file:///home/test", "sftp://host/work"]).unwrap();
            call(&context, &c.client, "ShowItems", &["file:///home/test/note.txt"]).unwrap();
            call(&context, &c.client, "ShowItemProperties", &["file:///home/test/note.txt"]).unwrap();
            call(&context, &c.client, "ShowFolders", &[]).unwrap();
            assert_eq!(
                *c.host.calls.borrow(),
                [
                    Call::Open(vec!["file:///home/test".into(), "sftp://host/work".into()], false, false),
                    Call::Open(vec!["file:///home/test/note.txt".into()], true, false),
                    Call::Properties(vec!["file:///home/test/note.txt".into()]),
                ]
            );
            let error = call(&context, &c.client, "ShowFolders", &["not a URI"]).unwrap_err();
            assert_eq!(gio::DBusError::remote_error(&error).as_deref(), Some("org.freedesktop.DBus.Error.InvalidArgs"));
            let many: Vec<String> = (0..129).map(|i| format!("file:///f{i}")).collect();
            let many: Vec<&str> = many.iter().map(String::as_str).collect();
            assert!(call(&context, &c.client, "ShowItems", &many).is_err());
            assert_eq!(c.host.calls.borrow().len(), 3);
            // No longer the default: refused with NotSupported, nothing opened.
            c.is_default.set(false);
            let error = call(&context, &c.client, "ShowFolders", &["file:///x"]).unwrap_err();
            assert_eq!(gio::DBusError::remote_error(&error).as_deref(), Some("org.freedesktop.DBus.Error.NotSupported"));
            c.is_default.set(true);
            c.service.close();
            wait(&context, "released", || !has_owner(&c.client));
            assert!(!c.service.available());
            finish(c);

            // test_existing_manager_keeps_name_and_default_change_releases_it
            let c = case(&address);
            let reply = daemon(&c.client, "RequestName", (BUS_NAME, 4u32).to_variant());
            assert_eq!(reply.child_value(0).get::<u32>(), Some(1));
            c.service.start();
            wait(&context, "request answered", || !c.service.pending());
            assert!(!c.service.available());
            assert_eq!(c.service.last_error().as_deref(), Some("Another file manager owns FileManager1"));
            let owner = daemon(&c.client, "GetNameOwner", (BUS_NAME,).to_variant()).child_value(0).get::<String>().unwrap();
            assert_eq!(Some(owner), c.client.unique_name().map(|s| s.to_string()));
            daemon(&c.client, "ReleaseName", (BUS_NAME,).to_variant());
            wait(&context, "taken over after release", || c.service.available());
            c.is_default.set(false);
            c.service.default_changed();
            wait(&context, "released on default change", || !has_owner(&c.client));
            assert!(!c.service.available());
            finish(c);

            // test_close_during_pending_request_does_not_leave_name_owned
            let c = case(&address);
            c.service.start();
            c.service.close();
            wait(&context, "pending finished", || !c.service.pending());
            wait(&context, "not owned", || !has_owner(&c.client));
            assert!(!c.service.available());
            finish(c);
        })
        .unwrap();
    bus.down();
}
