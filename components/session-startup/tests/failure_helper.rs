//! tessel-failed (FailureHelperTests of test_configure_session.py) end to end on a
//! private dbus-daemon (no service directories, so nothing is ever auto-started) with a fake
//! org.freedesktop.Notifications server and a logging fake `systemctl`. The same scenarios
//! run against `desktop_suite_failed.py` and must record the same calls.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use gio::prelude::*;

use common::*;

struct Bus {
    daemon: Child,
    address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn private_bus(root: &Path) -> Option<Bus> {
    let config = root.join("bus.conf");
    fs::write(
        &config,
        format!(
            "<busconfig><type>session</type><listen>unix:tmpdir={}</listen><auth>EXTERNAL</auth>\
             <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
            root.display()
        ),
    )
    .unwrap();
    let mut daemon = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .arg("--nofork")
        .arg("--print-address=1")
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(daemon.stdout.take().unwrap()), &mut line).unwrap();
    Some(Bus { daemon, address: line.trim().to_owned() })
}

/// What the fake server does after answering Notify with id 7.
#[derive(Clone, Copy)]
enum Then {
    Restart,
    Close,
}

/// Fake notification server on its own thread; returns the printed Notify parameters.
fn serve(address: &str, then: Then) -> (Arc<Mutex<Vec<String>>>, mpsc::Sender<()>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (address, recorded) = (address.to_owned(), calls.clone());
    std::thread::spawn(move || {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let connection = gio::DBusConnection::for_address_sync(
                    &address,
                    gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                    None,
                    gio::Cancellable::NONE,
                )
                .unwrap();
                let info = gio::DBusNodeInfo::for_xml(
                    "<node><interface name='org.freedesktop.Notifications'><method name='Notify'>\
                     <arg type='s' direction='in'/><arg type='u' direction='in'/><arg type='s' direction='in'/><arg type='s' direction='in'/>\
                     <arg type='s' direction='in'/><arg type='as' direction='in'/><arg type='a{sv}' direction='in'/><arg type='i' direction='in'/>\
                     <arg type='u' direction='out'/></method></interface></node>",
                )
                .unwrap();
                let emitter = connection.clone();
                let _registration = connection
                    .register_object("/org/freedesktop/Notifications", &info.interfaces()[0])
                    .method_call(move |_, _, _, _, _, parameters, invocation| {
                        recorded.lock().unwrap().push(parameters.print(true).to_string());
                        invocation.return_value(Some(&(7u32,).to_variant()));
                        {
                            let emit = |member: &str, value: glib::Variant| {
                                emitter.emit_signal(None, "/org/freedesktop/Notifications", "org.freedesktop.Notifications", member, Some(&value)).unwrap();
                            };
                            match then {
                                Then::Restart => {
                                    emit("ActionInvoked", (6u32, "restart").to_variant());
                                    emit("ActionInvoked", (7u32, "restart").to_variant());
                                }
                                Then::Close => {
                                    emit("ActionInvoked", (6u32, "restart").to_variant());
                                    emit("NotificationClosed", (7u32, 2u32).to_variant());
                                }
                            }
                        }
                    })
                    .build()
                    .unwrap();
                connection
                    .call_sync(
                        Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                        Some(&("org.freedesktop.Notifications", 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE,
                    )
                    .unwrap();
                ready_tx.send(()).unwrap();
                while stop_rx.try_recv().is_err() {
                    context.iteration(false);
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    (calls, stop_tx)
}

fn helper(rust: bool, root: &Path, address: &str, args: &[&str], extra: &[(&str, &str)]) -> (Output, String) {
    let log = root.join(if rust { "rust.log" } else { "python.log" });
    let _ = fs::remove_file(&log);
    let bin = fake_bin(&root.join(if rust { "r" } else { "p" }), &["systemctl"], &log);
    let suite = suite_root();
    let mut command = if rust {
        Command::new(env!("CARGO_BIN_EXE_tessel-failed"))
    } else {
        let suite = suite.clone().unwrap();
        let mut command = Command::new("python3");
        command.arg("-B").arg(suite.join("tessel-session-startup/desktop_suite_failed.py"));
        command
    };
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    command
        .args(args)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("LANG", "C.UTF-8")
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .env("DESKTOP_SUITE_FAILED_WAIT_MS", "10000");
    if let (false, Some(suite)) = (rust, &suite) {
        command.env("PYTHONPATH", suite.join("shared"));
    }
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    (output, fs::read_to_string(&log).unwrap_or_default())
}

fn python_available() -> bool {
    suite_root().is_some()
        && Command::new("python3").args(["-c", "import gi; gi.require_version('Gio', '2.0'); from gi.repository import Gio"]).output().is_ok_and(|o| o.status.success())
}

fn setup(name: &str) -> Option<(PathBuf, Bus)> {
    let root = scratch(&format!("failed-{name}"));
    let bus = private_bus(&root)?;
    Some((root, bus))
}

#[test]
fn invalid_unit_names_are_rejected() {
    for args in [&["../../evil"][..], &[], &["a.service", "b.service"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_tessel-failed")).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(String::from_utf8_lossy(&output.stderr), "usage: desktop_suite_failed.py UNIT.service\n");
    }
}

#[test]
fn session_ending_is_not_reported() {
    let root = scratch("failed-ending");
    let runtime = root.display().to_string();
    let (output, log) = helper(true, &root, "unix:path=/nonexistent", &["tessel-dock.service"], &[("WAYLAND_DISPLAY", "gone-0"), ("XDG_RUNTIME_DIR", &runtime)]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(log, "", "systemctl must not run once the compositor socket is gone");
}

#[test]
fn restart_action_resets_failed_state_and_starts_unit() {
    let mut outcomes = Vec::new();
    for rust in [true, false] {
        if !rust && !python_available() {
            continue;
        }
        // A fresh bus per run: the previous fake server still owns the name.
        let Some((root, bus)) = setup(if rust { "restart-rust" } else { "restart-python" }) else { return };
        let (calls, stop) = serve(&bus.address, Then::Restart);
        let (output, log) = helper(rust, &root, &bus.address, &["tessel-network.service"], &[]);
        let _ = stop.send(());
        assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(
            log,
            "systemctl --user is-active --quiet graphical-session.target\nsystemctl --user reset-failed tessel-network.service\nsystemctl --user start --no-block tessel-network.service\n"
        );
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("'Network stopped working'"), "{}", calls[0]);
        assert!(calls[0].contains("['restart', 'Restart']"));
        outcomes.push((calls, log, String::from_utf8_lossy(&output.stderr).into_owned()));
    }
    if outcomes.len() == 2 {
        assert_eq!(outcomes[0], outcomes[1], "Rust and Python differ");
    }
}

#[test]
fn closing_the_notification_does_not_restart() {
    let Some((root, bus)) = setup("closed") else { return };
    let (_calls, stop) = serve(&bus.address, Then::Close);
    let started = std::time::Instant::now();
    let (output, log) = helper(true, &root, &bus.address, &["tessel-auth.service"], &[]);
    let _ = stop.send(());
    assert_eq!(output.status.code(), Some(0));
    assert!(started.elapsed() < Duration::from_secs(5), "NotificationClosed must end the wait");
    assert_eq!(log, "systemctl --user is-active --quiet graphical-session.target\n");
}

#[test]
fn missing_notification_service_is_reported_on_stderr() {
    let Some((root, bus)) = setup("noserver") else { return };
    let mut texts = Vec::new();
    for rust in [true, false] {
        if !rust && !python_available() {
            continue;
        }
        let (output, _log) = helper(rust, &root, &bus.address, &["tessel-notifications.service"], &[]);
        assert_eq!(output.status.code(), Some(0), "rust={rust} {}", String::from_utf8_lossy(&output.stderr));
        texts.push(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    assert!(texts[0].starts_with("tessel-notifications.service failed; no notification could be shown: "), "{}", texts[0]);
    if texts.len() == 2 {
        assert_eq!(texts[0], texts[1]);
    }
}
