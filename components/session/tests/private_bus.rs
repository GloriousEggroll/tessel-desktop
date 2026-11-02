//! The real gio `SessionBackend` / `EndSession` D-Bus path against a FAKE
//! logind + DisplayManager on a private dbus-daemon (gio::TestDBus). The
//! backend connects to that private address explicitly (never the system bus)
//! and its lock command is a recording fake, so nothing here can lock,
//! suspend, restart, power off or switch users. Also checks the binary's local
//! argument handling (no display or bus needed).

use gio::prelude::*;
use glib::Variant;
use tessel_session::backend::{Backend, CommandRunner, Connector, Done, SessionBackend};
use tessel_session::bus::{GioSystemBus, SystemBus};
use tessel_session::end::Inhibitor;
use tessel_session::windows::Window;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const LOGIN1_XML: &str = r#"<node>
 <interface name="org.freedesktop.login1.Manager">
  <method name="CanSuspend"><arg type="s" direction="out"/></method>
  <method name="CanReboot"><arg type="s" direction="out"/></method>
  <method name="CanPowerOff"><arg type="s" direction="out"/></method>
  <method name="Suspend"><arg type="b" direction="in"/></method>
  <method name="Reboot"><arg type="b" direction="in"/></method>
  <method name="PowerOff"><arg type="b" direction="in"/></method>
  <method name="ListInhibitors"><arg type="a(ssssuu)" direction="out"/></method>
 </interface>
</node>"#;

const DM_XML: &str = r#"<node>
 <interface name="org.freedesktop.DisplayManager">
  <property name="Seats" type="ao" access="read"/>
 </interface>
 <interface name="org.freedesktop.DisplayManager.Seat">
  <method name="SwitchToGreeter"/>
 </interface>
</node>"#;

type Calls = Rc<RefCell<Vec<(String, String, String)>>>;

fn pump(duration: Duration) {
    let end = Instant::now() + duration;
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait_for(what: &str, timeout: f64, mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        if condition() {
            return;
        }
        pump(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None, None::<&gio::Cancellable>).unwrap()
}

fn own(connection: &gio::DBusConnection, name: &str) {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                         Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).unwrap();
}

fn serve(connection: &gio::DBusConnection, calls: &Calls, uid: u32) {
    let login = gio::DBusNodeInfo::for_xml(LOGIN1_XML).unwrap();
    let c = calls.clone();
    connection.register_object("/org/freedesktop/login1", &login.lookup_interface("org.freedesktop.login1.Manager").unwrap())
        .method_call(move |_, _, path, interface, method, params, invocation| {
            c.borrow_mut().push((path.into(), format!("{}.{method}", interface.unwrap_or("")), params.print(false).into()));
            let reply: Option<Variant> = match method {
                "CanSuspend" => Some(("yes",).to_variant()),
                "CanReboot" => Some(("challenge",).to_variant()),
                "CanPowerOff" => Some(("no",).to_variant()),
                "ListInhibitors" => Some((vec![
                    ("shutdown".to_owned(), "Builder".to_owned(), "Saving project".to_owned(), "block".to_owned(), uid, 10u32),
                    ("shutdown".to_owned(), "Updater".to_owned(), "Installing".to_owned(), "delay".to_owned(), uid + 1, 11u32),
                    ("sleep".to_owned(), "Player".to_owned(), "Playing".to_owned(), "block".to_owned(), uid, 12u32),
                ],).to_variant()),
                "PowerOff" => {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "Fixture denied");
                    return;
                }
                _ => None,
            };
            invocation.return_value(reply.as_ref());
        })
        .build().unwrap();
    let dm = gio::DBusNodeInfo::for_xml(DM_XML).unwrap();
    connection.register_object("/org/freedesktop/DisplayManager", &dm.lookup_interface("org.freedesktop.DisplayManager").unwrap())
        .property(|_, _, _, _, name| {
            assert_eq!(name, "Seats");
            vec![glib::variant::ObjectPath::try_from("/org/freedesktop/DisplayManager/Seat0".to_owned()).unwrap()].to_variant()
        })
        .build().unwrap();
    let c = calls.clone();
    connection.register_object("/org/freedesktop/DisplayManager/Seat0", &dm.lookup_interface("org.freedesktop.DisplayManager.Seat").unwrap())
        .method_call(move |_, _, path, interface, method, params, invocation| {
            c.borrow_mut().push((path.into(), format!("{}.{method}", interface.unwrap_or("")), params.print(false).into()));
            invocation.return_value(None);
        })
        .build().unwrap();
    own(connection, "org.freedesktop.login1");
    own(connection, "org.freedesktop.DisplayManager");
}

#[derive(Default)]
struct RecordingRunner(Rc<RefCell<Vec<Vec<String>>>>);

impl CommandRunner for RecordingRunner {
    fn run(&self, argv: &[&str], done: Done) {
        assert_ne!(argv.first().copied(), Some("labwc"), "tests never run labwc --exit");
        self.0.borrow_mut().push(argv.iter().map(|a| a.to_string()).collect());
        glib::idle_add_local_once(move || done(true, String::new()));
    }
}

#[test]
fn backend_against_fake_logind_on_private_bus() {
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    let uid = unsafe { libc::getuid() };
    let calls: Calls = Rc::default();
    let service = connect(&address);
    serve(&service, &calls, uid);

    std::env::remove_var("XDG_SEAT_PATH");
    let private = address.clone();
    let connector: Connector = Box::new(move |_, done| done(Ok(GioSystemBus::new(connect(&private)) as Rc<dyn SystemBus>)));
    let runner = RecordingRunner::default();
    let argvs = runner.0.clone();
    let changes = Rc::new(std::cell::Cell::new(0u32));
    let c = changes.clone();
    // No window helper: the end-session path reports the note and still lists inhibitors.
    let backend = SessionBackend::with(Rc::new(move || c.set(c.get() + 1)), connector, Rc::new(runner), Some(None));
    backend.start();
    wait_for("capabilities", 5.0, || backend.capabilities().switch_user && backend.capabilities().sleep);
    let caps = backend.capabilities();
    assert_eq!((caps.sleep, caps.restart, caps.poweroff, caps.switch_user), (true, true, false, true));
    assert_eq!(backend.error(), "");
    assert!(changes.get() >= 4);

    let results: Rc<RefCell<Vec<(bool, String)>>> = Rc::default();
    let done = |results: &Rc<RefCell<Vec<(bool, String)>>>| -> Done {
        let r = results.clone();
        Box::new(move |ok, error| r.borrow_mut().push((ok, error)))
    };
    // Sleep: lock first, then Suspend(true) on the fake.
    backend.perform("sleep", done(&results));
    wait_for("sleep", 5.0, || results.borrow().len() == 1);
    assert_eq!(results.borrow()[0], (true, String::new()));
    assert_eq!(argvs.borrow()[0], ["systemctl", "--user", "start", "tessel-lock.service"]);
    assert!(calls.borrow().iter().any(|c| c.1 == "org.freedesktop.login1.Manager.Suspend" && c.2 == "(true,)"));
    // Switch user: lock, then SwitchToGreeter on the current seat.
    backend.perform("switch_user", done(&results));
    wait_for("switch", 5.0, || results.borrow().len() == 2);
    assert_eq!(results.borrow()[1], (true, String::new()));
    assert_eq!(argvs.borrow().len(), 2);
    assert!(calls.borrow().iter().any(|c| c.0 == "/org/freedesktop/DisplayManager/Seat0"
                                          && c.1 == "org.freedesktop.DisplayManager.Seat.SwitchToGreeter"));
    // Restart: Reboot(true), never a lock.
    backend.perform("restart", done(&results));
    wait_for("restart", 5.0, || results.borrow().len() == 3);
    assert!(calls.borrow().iter().any(|c| c.1 == "org.freedesktop.login1.Manager.Reboot" && c.2 == "(true,)"));
    assert_eq!(argvs.borrow().len(), 2);
    // Power off is unsupported by the fake (CanPowerOff = no) and refused locally.
    backend.perform("poweroff", done(&results));
    assert_eq!(results.borrow()[3], (false, "That action is unavailable.".to_owned()));
    assert!(!calls.borrow().iter().any(|c| c.1 == "org.freedesktop.login1.Manager.PowerOff"));

    // End session without a window helper: note + this user's shutdown inhibitors.
    let reports: Rc<RefCell<Vec<(Vec<Window>, Vec<Inhibitor>, String)>>> = Rc::default();
    let r = reports.clone();
    let ending = backend.end_session("logout", Rc::new(move |w, i, n| r.borrow_mut().push((w, i, n))));
    wait_for("end report", 5.0, || !reports.borrow().is_empty());
    let (windows, inhibitors, note) = reports.borrow()[0].clone();
    assert!(windows.is_empty());
    assert_eq!(note, "Open apps could not be checked.");
    assert_eq!(inhibitors, [Inhibitor { who: "Builder".into(), why: "Saving project".into(), mode: "block".into(), pid: 10 }]);
    assert!(ending.blocking());
    ending.close();
    let ending = backend.end_session("restart", Rc::new({
        let r = reports.clone();
        move |w, i, n| r.borrow_mut().push((w, i, n))
    }));
    wait_for("restart report", 5.0, || reports.borrow().len() == 2);
    assert_eq!(reports.borrow()[1].1.iter().map(|i| i.who.as_str()).collect::<Vec<_>>(), ["Builder", "Updater"]);
    ending.close();

    backend.close();
    let before = changes.get();
    pump(Duration::from_millis(100));
    assert_eq!(changes.get(), before);
    drop(service);
    bus.down();
}

#[test]
fn binary_validates_arguments_locally() {
    let binary = env!("CARGO_BIN_EXE_tessel-session");
    let run = |args: &[&str]| {
        let output = std::process::Command::new(binary).args(args).env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY").output().unwrap();
        (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned(),
         String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let (code, out, _) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("usage: tessel-session [-h] [--popup] [--menu]"), "{out}");
    assert!(out.contains("--menu                open centered on the pointer monitor (keyboard\n"), "{out}");
    let (code, _, err) = run(&["--popup-offset", "-1"]);
    assert_eq!(code, 2);
    assert!(err.ends_with("tessel-session: error: --popup-offset must be nonnegative\n"), "{err}");
    let (code, _, err) = run(&["--popup-edge", "middle"]);
    assert_eq!(code, 2);
    assert!(err.contains("invalid choice: 'middle'"), "{err}");
}
