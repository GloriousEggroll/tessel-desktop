//! Protocol and client tests (ports of test_notification_service.py and
//! test_notification_client.py) on a private dbus-daemon. The real session
//! bus is never used: `DBUS_SESSION_BUS_ADDRESS` is pointed at this file's own
//! daemon before any connection is made, and every connection is by address.

use gio::prelude::*;
use glib::Variant;
use tessel_notifications::client::NotificationClient;
use tessel_notifications::model::{NotificationModel, NotifyArgs, SharedModel};
use tessel_notifications::service::{Command, NotificationService, BUS_NAME, CONTROL_INTERFACE, INTERFACE, OBJECT_PATH};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A private bus that dies with this test process (a watchdog shell).
/// tools/headless.md: the session policy without any service directories, so
/// nothing (portal, gvfs, ...) can be D-Bus-activated; `lock()` and every
/// fixture drop fail the test if the daemon ever logged an activation.
fn bus() -> &'static String {
    &private_bus().0
}

static BUS: OnceLock<(String, PathBuf)> = OnceLock::new();

fn private_bus() -> &'static (String, PathBuf) {
    BUS.get_or_init(|| {
        let dir = temp_dir("bus");
        let config: String = std::fs::read_to_string("/usr/share/dbus-1/session.conf").expect("session.conf").lines()
            .filter(|line| !line.trim_start().starts_with("<standard_session_servicedirs")
                && !line.trim_start().starts_with("<servicedir"))
            .map(|line| if line.trim_start().starts_with("<listen>") {
                format!("  <listen>unix:tmpdir={}</listen>\n", dir.display())
            } else {
                format!("{line}\n")
            })
            .collect();
        assert!(!config.contains("servicedir"), "private bus config keeps a service dir");
        let conf = dir.join("bus.conf");
        let log = dir.join("bus.log");
        std::fs::write(&conf, config).unwrap();
        let script = "dbus-daemon --config-file=\"$1\" --nofork --print-address=1 2>\"$2\" & d=$!; trap 'kill $d' EXIT; \
                      while kill -0 \"$3\" 2>/dev/null; do sleep 0.5; done";
        let mut child = Process::new("sh").args(["-c", script, "sh"]).arg(&conf).arg(&log).arg(std::process::id().to_string())
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("dbus-daemon");
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let address = line.trim().to_owned();
        assert!(address.starts_with("unix:") && address.contains(dir.to_str().unwrap()), "private bus address: {address}");
        // SAFETY: set once, before any GIO session-bus lookup in this process.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
        // Spawned GTK/GIO binaries must not look up portals or gvfs either.
        for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
            unsafe { std::env::set_var(key, value) };
        }
        std::mem::forget(child);
        (address, log)
    })
}

fn assert_no_activation() {
    if let Some((_, log)) = BUS.get() {
        let log = std::fs::read_to_string(log).unwrap_or_default();
        assert!(!log.contains("Activating service"), "UNEXPECTED D-Bus activation on the private bus:\n{log}");
    }
}

/// All tests share one GLib main context; serialize them.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_no_activation();
    guard
}

fn connection() -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(bus(),
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None, None::<&gio::Cancellable>).expect("private bus connection")
}

fn temp_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("nm-dbus-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn settle(mut predicate: impl FnMut() -> bool, seconds: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    predicate()
}

type Signals = Rc<RefCell<Vec<(String, String, Variant)>>>;

struct Fixture {
    model: SharedModel,
    service: NotificationService,
    client: gio::DBusConnection,
    server: gio::DBusConnection,
    commands: Rc<RefCell<Vec<Command>>>,
    signals: Signals,
    _subscription: gio::SignalSubscription,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.close();
        let _ = self.client.close_sync(None::<&gio::Cancellable>);
        let _ = self.server.close_sync(None::<&gio::Cancellable>);
        if !std::thread::panicking() {
            assert_no_activation();
        }
    }
}

fn fixture(name: &str) -> Fixture {
    let server = connection();
    let client = connection();
    let model = SharedModel::new(NotificationModel::new(Some(&temp_dir(name))));
    let commands = Rc::new(RefCell::new(Vec::new()));
    let sink = commands.clone();
    let service = NotificationService::new(&server, &model, Some(Rc::new(move |command| {
        sink.borrow_mut().push(command);
        Ok(())
    })));
    service.start().unwrap();
    let signals: Signals = Rc::new(RefCell::new(Vec::new()));
    let sink = signals.clone();
    let subscription = client.subscribe_to_signal(Some(BUS_NAME), None, None, Some(OBJECT_PATH), None,
        gio::DBusSignalFlags::NONE, move |signal| {
            sink.borrow_mut().push((signal.interface_name.to_owned(), signal.signal_name.to_owned(), signal.parameters.clone()));
        });
    Fixture { model, service, client, server, commands, signals, _subscription: subscription }
}

fn call_on(connection: &gio::DBusConnection, method: &str, parameters: Option<Variant>, interface: &str)
           -> Result<Variant, glib::Error> {
    let result = Rc::new(RefCell::new(None));
    let sink = result.clone();
    connection.call(Some(BUS_NAME), OBJECT_PATH, interface, method, parameters.as_ref(), None,
                    gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>, move |reply| {
                        sink.replace(Some(reply));
                    });
    assert!(settle(|| result.borrow().is_some(), 3.0), "private bus call did not finish");
    result.take().unwrap()
}

fn remote(error: &glib::Error) -> Option<String> {
    gio::DBusError::remote_error(error).map(String::from)
}

impl Fixture {
    fn call(&self, method: &str, parameters: Option<Variant>, interface: &str) -> Result<Variant, glib::Error> {
        call_on(&self.client, method, parameters, interface)
    }

    fn notify(&self, name: &str, replaces: u32, summary: &str, actions: &[&str], hints: Vec<(&str, Variant)>, timeout: i32) -> u32 {
        let hints: std::collections::HashMap<String, Variant> = hints.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        let actions: Vec<String> = actions.iter().map(|s| s.to_string()).collect();
        let params = (name, replaces, "dialog-information", summary, "<b>Plain</b>", actions, hints, timeout).to_variant();
        self.call("Notify", Some(params), INTERFACE).unwrap().child_value(0).get::<u32>().unwrap()
    }

    fn has_signal(&self, interface: &str, member: &str, args: &Variant) -> bool {
        self.signals.borrow().iter().any(|(i, m, p)| i == interface && m == member && p == args)
    }

    fn state(&self) -> Value {
        let reply = self.call("GetState", None, CONTROL_INTERFACE).unwrap();
        serde_json::from_str(&reply.child_value(0).get::<String>().unwrap()).unwrap()
    }
}

fn capabilities_identity_and_notify_replacement() {
    let _guard = lock();
    let f = fixture("identity");
    let caps = f.call("GetCapabilities", None, INTERFACE).unwrap().child_value(0).get::<Vec<String>>().unwrap();
    let mut sorted = caps.clone();
    sorted.sort();
    assert_eq!(sorted, ["actions", "body", "icon-static", "persistence"]);
    assert!(!caps.iter().any(|c| c == "sound"));
    let info = f.call("GetServerInformation", None, INTERFACE).unwrap();
    assert_eq!(info, ("Tessel Dock Notifications", "Tessel Dock", "1.0", "1.2").to_variant());
    let ident = f.notify("Protocol Test", 0, "Hello", &[], vec![("urgency", 2u8.to_variant()), ("value", 62i32.to_variant())], 0);
    assert_eq!(f.notify("Protocol Test", ident, "Replacement", &[], vec![], 0), ident);
    let state = f.state();
    assert_eq!(state["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(state["notifications"][0]["summary"], "Replacement");
    assert_eq!(state["notifications"][0]["body"], "<b>Plain</b>");
    assert!(ident > 0);
    // Compact separators, like json.dumps(separators=(",", ":")).
    let text = f.call("GetState", None, CONTROL_INTERFACE).unwrap().child_value(0).get::<String>().unwrap();
    assert!(text.starts_with("{\"settings\":{\"do_not_disturb\":false,"), "{text}");
}

fn close_emits_standard_reason_and_invalid_id_error() {
    let _guard = lock();
    let f = fixture("close");
    let ident = f.notify("Protocol Test", 0, "Hello", &[], vec![], 0);
    f.call("CloseNotification", Some((ident,).to_variant()), INTERFACE).unwrap();
    assert!(settle(|| f.has_signal(INTERFACE, "NotificationClosed", &(ident, 3u32).to_variant()), 1.0));
    let error = f.call("CloseNotification", Some((ident,).to_variant()), INTERFACE).unwrap_err();
    assert_eq!(remote(&error).as_deref(), Some("org.freedesktop.Notifications.InvalidId"));
}

fn actions_and_resident_semantics() {
    let _guard = lock();
    let f = fixture("actions");
    let ident = f.notify("Protocol Test", 0, "Hello", &["default", "Open"], vec![("resident", true.to_variant())], 0);
    f.call("Activate", Some((ident, "default").to_variant()), CONTROL_INTERFACE).unwrap();
    assert!(settle(|| f.has_signal(INTERFACE, "ActionInvoked", &(ident, "default").to_variant()), 1.0));
    assert!(f.model.with(|m| m.items[&ident].active));
    f.call("Dismiss", Some((ident,).to_variant()), CONTROL_INTERFACE).unwrap();
    assert!(settle(|| f.has_signal(INTERFACE, "NotificationClosed", &(ident, 2u32).to_variant()), 1.0));
}

fn closed_and_action_signals_are_unicast_to_the_sender() {
    let _guard = lock();
    let f = fixture("unicast");
    let bystander = connection();
    let seen = Rc::new(RefCell::new(0));
    let sink = seen.clone();
    let _watch = bystander.subscribe_to_signal(Some(BUS_NAME), Some(INTERFACE), None, Some(OBJECT_PATH), None,
        gio::DBusSignalFlags::NONE, move |_| *sink.borrow_mut() += 1);
    let ident = f.notify("Protocol Test", 0, "Hello", &["go", "Go"], vec![], 0);
    f.call("Activate", Some((ident, "go").to_variant()), CONTROL_INTERFACE).unwrap();
    assert!(settle(|| f.has_signal(INTERFACE, "NotificationClosed", &(ident, 2u32).to_variant()), 1.0));
    assert!(f.has_signal(INTERFACE, "ActionInvoked", &(ident, "go").to_variant()));
    settle(|| false, 0.1);
    assert_eq!(*seen.borrow(), 0, "other clients never see another sender's closures");
    let _ = bystander.close_sync(None::<&gio::Cancellable>);
}

fn custom_settings_history_commands_and_signal() {
    let _guard = lock();
    let f = fixture("controls");
    f.notify("Protocol Test", 0, "Hello", &[], vec![], 0);
    f.call("SetSettings", Some(("{\"do_not_disturb\":true}",).to_variant()), CONTROL_INTERFACE).unwrap();
    assert!(f.model.with(|m| m.settings["do_not_disturb"] == json!(true)));
    f.call("MarkRead", None, CONTROL_INTERFACE).unwrap();
    assert_eq!(f.model.with(|m| m.unread()), 0);
    f.call("ShowCenter", None, CONTROL_INTERFACE).unwrap();
    f.call("ShowSettings", None, CONTROL_INTERFACE).unwrap();
    f.call("ShowCenterAt", Some(("top", 24u32).to_variant()), CONTROL_INTERFACE).unwrap();
    assert_eq!(*f.commands.borrow(), [Command::ShowCenter, Command::ShowSettings, Command::ShowCenterAt("top".into(), 24)]);
    let kept = f.notify("Other app", 0, "Hello", &[], vec![], 0);
    let cleared = f.notify("Protocol Test", 0, "Hello", &[], vec![("desktop-entry", "org.test.App".to_variant())], 0);
    f.call("ClearApp", Some(("org.test.App",).to_variant()), CONTROL_INTERFACE).unwrap();
    assert!(settle(|| f.has_signal(INTERFACE, "NotificationClosed", &(cleared, 2u32).to_variant()), 1.0));
    assert!(f.model.with(|m| m.items.contains_key(&kept)));
    let long = "x".repeat(257);
    let error = f.call("ClearApp", Some((long.as_str(),).to_variant()), CONTROL_INTERFACE).unwrap_err();
    assert_eq!(remote(&error).as_deref(), Some("org.freedesktop.DBus.Error.InvalidArgs"));
    f.call("ClearHistory", None, CONTROL_INTERFACE).unwrap();
    assert!(f.model.with(|m| m.items.is_empty()));
    assert!(settle(|| f.signals.borrow().iter().any(|(_, m, _)| m == "StateChanged"), 1.0));
    let error = f.call("SetSettings", Some(("{\"timeout_ms\":-99}",).to_variant()), CONTROL_INTERFACE).unwrap_err();
    assert_eq!(remote(&error).as_deref(), Some("org.freedesktop.DBus.Error.InvalidArgs"));
    let huge = format!("{{\"muted_apps\":[\"{}\"]}}", "y".repeat(16400));
    let error = f.call("SetSettings", Some((huge.as_str(),).to_variant()), CONTROL_INTERFACE).unwrap_err();
    assert!(error.message().contains("Settings are too large"));
}

fn invalid_popup_position_is_invalid_args() {
    let _guard = lock();
    let server = connection();
    let client = connection();
    let model = SharedModel::new(NotificationModel::new(Some(&temp_dir("position"))));
    let service = NotificationService::new(&server, &model, Some(Rc::new(|command| match command {
        Command::ShowCenterAt(edge, offset) if !["top", "bottom", "left", "right"].contains(&edge.as_str()) || offset > 4096 =>
            Err("Invalid notification popup position".into()),
        _ => Ok(()),
    })));
    service.start().unwrap();
    let error = call_on(&client, "ShowCenterAt", Some(("middle", 4u32).to_variant()), CONTROL_INTERFACE).unwrap_err();
    assert_eq!(remote(&error).as_deref(), Some("org.freedesktop.DBus.Error.InvalidArgs"));
    service.close();
    let _ = client.close_sync(None::<&gio::Cancellable>);
    let _ = server.close_sync(None::<&gio::Cancellable>);
}

fn existing_server_is_never_replaced_or_queued() {
    let _guard = lock();
    let f = fixture("ownership");
    let second_connection = connection();
    let second = NotificationService::new(&second_connection, &f.model, None);
    assert_eq!(second.start().unwrap_err(), "Another notification server is already running");
    let info = f.call("GetServerInformation", None, INTERFACE).unwrap();
    assert_eq!(info.child_value(0).get::<String>().unwrap(), "Tessel Dock Notifications");
    f.service.close();
    second.start().unwrap();
    let info = f.call("GetServerInformation", None, INTERFACE).unwrap();
    assert_eq!(info.child_value(0).get::<String>().unwrap(), "Tessel Dock Notifications");
    second.close();
    let _ = second_connection.close_sync(None::<&gio::Cancellable>);
}

// --- client (test_notification_client.py) ---

struct ClientFixture {
    model: SharedModel,
    service: NotificationService,
    connection: gio::DBusConnection,
    commands: Rc<RefCell<Vec<Command>>>,
    client: NotificationClient,
    changed: Rc<RefCell<Vec<bool>>>,
}

impl Drop for ClientFixture {
    fn drop(&mut self) {
        self.client.close();
        self.service.close();
        let _ = self.connection.close_sync(None::<&gio::Cancellable>);
    }
}

fn client_fixture(name: &str) -> ClientFixture {
    bus();
    let connection = connection();
    let model = SharedModel::new(NotificationModel::new(Some(&temp_dir(name))));
    let commands = Rc::new(RefCell::new(Vec::new()));
    let sink = commands.clone();
    let service = NotificationService::new(&connection, &model, Some(Rc::new(move |c| { sink.borrow_mut().push(c); Ok(()) })));
    let changed = Rc::new(RefCell::new(Vec::new()));
    let slot: Rc<RefCell<Option<NotificationClient>>> = Rc::new(RefCell::new(None));
    let (sink, reader) = (changed.clone(), slot.clone());
    let client = NotificationClient::new(move || {
        let ready = reader.borrow().as_ref().is_some_and(|c| c.ready());
        sink.borrow_mut().push(ready);
    });
    slot.replace(Some(client.clone()));
    ClientFixture { model, service, connection, commands, client, changed }
}

impl ClientFixture {
    fn notify(&self, actions: &[&str]) -> u32 {
        let sender = self.connection.unique_name().map(|n| n.to_string()).unwrap_or_default();
        self.model.notify(NotifyArgs { app_name: "Test app".into(), summary: "Private notification".into(), body: "Body".into(),
            actions: actions.iter().map(|s| s.to_string()).collect(), ..Default::default() }, &sender)
    }
}

fn client_controls_and_change_signals_update_asynchronously() {
    let _guard = lock();
    let f = client_fixture("client-controls");
    f.service.start().unwrap();
    f.client.start();
    assert!(settle(|| f.client.ready(), 2.0));
    let ident = f.notify(&["default", "Open"]);
    assert!(settle(|| f.client.state().get("unread") == Some(&json!(1)), 2.0));
    assert_eq!(f.client.state()["notifications"][0]["id"], json!(ident));
    f.client.set_settings(&json!({"do_not_disturb": true, "timeout_ms": 9000}));
    assert!(settle(|| f.client.state()["settings"]["do_not_disturb"] == json!(true), 2.0));
    assert_eq!(f.model.with(|m| m.timeout_ms()), 9000);
    f.client.mark_read();
    assert!(settle(|| f.client.state()["unread"] == json!(0), 2.0));
    f.client.activate(ident, "default");
    assert!(settle(|| f.client.state()["notifications"][0]["active"] == json!(false), 2.0));
    f.client.dismiss(ident);
    assert!(settle(|| f.client.state()["notifications"].as_array().is_some_and(|l| l.is_empty()), 2.0));
    f.model.notify(NotifyArgs { app_name: "Grouped app".into(), summary: "Grouped".into(), ..Default::default() }, "");
    f.client.clear_app("Grouped app");
    assert!(settle(|| f.model.with(|m| m.items.is_empty()), 2.0));
    f.notify(&[]);
    f.client.clear_history();
    assert!(settle(|| f.model.with(|m| m.items.is_empty()), 2.0));
    f.client.show_settings();
    assert!(settle(|| f.commands.borrow().contains(&Command::ShowSettings), 2.0));
}

fn client_missing_service_recovery_loss_and_restart() {
    let _guard = lock();
    let f = client_fixture("client-restart");
    f.client.start();
    assert!(settle(|| !f.client.error().is_empty(), 2.0));
    assert!(!f.client.ready());
    assert!(f.client.error().contains("not running"));
    assert!(!f.client.set_settings(&json!({"do_not_disturb": true})));
    f.service.start().unwrap();
    assert!(settle(|| f.client.ready(), 2.0));
    assert_eq!(f.client.error(), "");
    f.notify(&[]);
    assert!(settle(|| f.client.state().get("notifications").and_then(Value::as_array).is_some_and(|l| !l.is_empty()), 2.0));
    f.service.close();
    assert!(settle(|| !f.client.ready(), 2.0));
    assert!(f.client.state().is_empty());
    f.service.start().unwrap();
    assert!(settle(|| f.client.ready(), 2.0));
    assert_eq!(f.client.state()["notifications"].as_array().unwrap().len(), 1);
}

fn client_errors_are_visible_and_close_cancels_pending_callbacks() {
    let _guard = lock();
    let f = client_fixture("client-errors");
    f.service.start().unwrap();
    f.client.start();
    assert!(settle(|| f.client.ready(), 2.0));
    f.client.set_settings(&json!({"timeout_ms": -1}));
    assert!(settle(|| !f.client.error().is_empty(), 2.0));
    assert!(f.client.ready());
    f.client.set_settings(&json!({"timeout_ms": 10000}));
    assert!(settle(|| f.client.error().is_empty(), 2.0));
    assert_eq!(f.model.with(|m| m.timeout_ms()), 10000);
    f.client.close();
    let before = f.changed.borrow().len();
    f.notify(&[]);
    settle(|| false, 0.1);
    assert_eq!(f.changed.borrow().len(), before);
    assert!(!f.client.ready());
    assert!(!f.client.clear_history());
}

fn run_binary(binary: &str, args: &[&str]) -> std::process::Output {
    let (sender, receiver) = std::sync::mpsc::channel();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (binary, address) = (binary.to_owned(), bus().clone());
    std::thread::spawn(move || {
        let output = Process::new(binary).args(&args).env("DBUS_SESSION_BUS_ADDRESS", &address).env("LANGUAGE", "en")
            .output().unwrap();
        let _ = sender.send(output);
    });
    let mut output = None;
    settle(|| {
        if output.is_none() {
            output = receiver.try_recv().ok();
        }
        output.is_some()
    }, 6.0);
    output.expect("command finished")
}

fn cli_check_settings_and_popup_against_private_bus() {
    let _guard = lock();
    let f = fixture("cli");
    let binary = env!("CARGO_BIN_EXE_tessel-notifications");
    let check = run_binary(binary, &["--check"]);
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
    let parsed: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(parsed["ready"], json!(true));
    assert!(String::from_utf8_lossy(&check.stdout).starts_with("{\"ready\": true, \"unread\": 0, \"settings\": {"));
    assert!(run_binary(binary, &["--settings"]).status.success());
    assert!(run_binary(binary, &["--popup-edge", "left", "--popup-offset", "12"]).status.success());
    let bad = run_binary(binary, &["--popup-offset", "5000"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("error: Popup offset must be between 0 and 4096"));
    settle(|| f.commands.borrow().len() >= 2, 1.0);
    assert_eq!(*f.commands.borrow(), [Command::ShowSettings, Command::ShowCenterAt("left".into(), 12)]);
    f.service.close();
    let missing = run_binary(binary, &["--check"]);
    assert_eq!(missing.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.starts_with("Notifications: g-dbus-error-quark: "), "{stderr}");
}

fn status_stream_reports_service_changes() {
    let _guard = lock();
    let f = fixture("status");
    let binary = env!("CARGO_BIN_EXE_tessel-notifications-status");
    let mut child = Process::new(binary).env("DBUS_SESSION_BUS_ADDRESS", bus()).env("LANGUAGE", "en")
        .stdout(Stdio::piped()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let lines = RefCell::new(Vec::<Value>::new());
    let wait_for = |class: &str| settle(|| {
        while let Ok(line) = receiver.try_recv() {
            lines.borrow_mut().push(serde_json::from_str(&line).unwrap());
        }
        lines.borrow().last().is_some_and(|v| v["class"] == class)
    }, 3.0);
    assert!(wait_for("notifications"));
    f.notify("Status", 0, "Hello", &[], vec![], 0);
    assert!(wait_for("unread"));
    f.call("SetSettings", Some(("{\"do_not_disturb\":true}",).to_variant()), CONTROL_INTERFACE).unwrap();
    assert!(wait_for("paused"));
    f.service.close();
    assert!(wait_for("unavailable"));
    for pair in lines.borrow().windows(2) {
        assert_ne!(pair[0], pair[1], "only changes are printed");
    }
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert!(child.wait().unwrap().success());
}

/// Single-threaded runner: GLib objects (and their callbacks on the shared
/// default main context) must stay on the thread that created them.
fn main() {
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let tests: &[(&str, fn())] = &[
        ("capabilities_identity_and_notify_replacement", capabilities_identity_and_notify_replacement),
        ("close_emits_standard_reason_and_invalid_id_error", close_emits_standard_reason_and_invalid_id_error),
        ("actions_and_resident_semantics", actions_and_resident_semantics),
        ("closed_and_action_signals_are_unicast_to_the_sender", closed_and_action_signals_are_unicast_to_the_sender),
        ("custom_settings_history_commands_and_signal", custom_settings_history_commands_and_signal),
        ("invalid_popup_position_is_invalid_args", invalid_popup_position_is_invalid_args),
        ("existing_server_is_never_replaced_or_queued", existing_server_is_never_replaced_or_queued),
        ("client_controls_and_change_signals_update_asynchronously", client_controls_and_change_signals_update_asynchronously),
        ("client_missing_service_recovery_loss_and_restart", client_missing_service_recovery_loss_and_restart),
        ("client_errors_are_visible_and_close_cancels_pending_callbacks", client_errors_are_visible_and_close_cancels_pending_callbacks),
        ("cli_check_settings_and_popup_against_private_bus", cli_check_settings_and_popup_against_private_bus),
        ("status_stream_reports_service_changes", status_stream_reports_service_changes),
    ];
    let mut failed = Vec::new();
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        match std::panic::catch_unwind(test) {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failed.push(*name);
            }
        }
    }
    println!("test result: {}. {} failed", if failed.is_empty() { "ok" } else { "FAILED" }, failed.len());
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
