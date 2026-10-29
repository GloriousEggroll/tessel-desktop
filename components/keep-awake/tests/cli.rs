//! The real binary on a private bus (session and "system" = one private
//! dbus-daemon) with a fake logind: launcher-only rule, GApplication
//! command-line forwarding, D-Bus settings interfaces, timer restore and
//! FD release on SIGTERM. No display is given to the children.

mod common;

use common::{settle, wait_for, FakeLogin, PrivateBus, Read};
use gio::prelude::*;
use keep_awake::client::KeepAwakeClient;
use keep_awake::ui::Controls;
use serde_json::Value;
use std::cell::RefCell;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_keep-awake");

struct Env {
    state: PathBuf,
}

impl Env {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command.args(args).env("XDG_STATE_HOME", &self.state).env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY")
            .env("DESKTOP_SUITE_LOCALE_DIR", "/nonexistent")
            .stdout(Stdio::piped()).stderr(Stdio::piped());
        command
    }

    /// Run a client invocation while pumping our main loop (the fake logind lives here).
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let mut child = self.command(args).spawn().unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            settle(0.01);
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < end, "client {args:?} did not return");
        }
        let output = child.wait_with_output().unwrap();
        (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into(),
         String::from_utf8_lossy(&output.stderr).into())
    }

    fn status(&self) -> Value {
        let (code, out, err) = self.run(&["--status"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.ends_with("}\n"), "{out:?}");
        assert_eq!(out.matches('\n').count(), 1);
        serde_json::from_str(&out).unwrap()
    }

    fn daemon(&self) -> Child {
        let mut command = self.command(&["--background"]);
        command.stdout(Stdio::null());
        command.spawn().unwrap()
    }

    fn timer(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.state.join("keep-awake/timer.json")).unwrap()).unwrap()
    }
}

fn has_owner(name: &str) -> bool {
    let connection = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).unwrap();
    suite_core::single_instance::name_has_owner(&connection, name)
}

fn terminate(child: &mut Child) -> i32 {
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        settle(0.01);
        if let Some(status) = child.try_wait().unwrap() {
            return status.code().unwrap_or(-1);
        }
        assert!(Instant::now() < end, "daemon ignored SIGTERM");
    }
}

#[test]
fn cli() {
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    let bus = PrivateBus::up();
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let state = base.join(format!("keep-awake-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let env = Env { state: state.clone() };
    let login = FakeLogin::new(&bus.address);

    // Local argparse errors and help never contact the bus.
    let (code, out, err) = env.run(&["--popup-offset", "x"]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.starts_with("usage: keep-awake [-h]"), "{err}");
    assert!(err.ends_with("keep-awake: error: argument --popup-offset: invalid int value: 'x'\n"), "{err}");
    let (code, out, _) = env.run(&["-h"]);
    assert_eq!(code, 0);
    assert!(out.contains("Manual Keep Awake controls, explicitly started for the labwc session."));

    // Launcher-only: with no primary, controls fail and never create a daemon/backend.
    for flag in ["--toggle", "--status", "--enable", "--popup"] {
        let (code, out, _) = env.run(&[flag]);
        assert_ne!(code, 0, "{flag} must not become the primary");
        assert_eq!(out, "");
    }
    assert!(!has_owner("org.waybar.KeepAwake"));
    assert!(login.calls.borrow().is_empty());
    assert!(!state.join("keep-awake").exists());

    // The daemon: a client following Changed like Waydock's indicator does.
    let updates = Rc::new(RefCell::new(0u32));
    let counter = updates.clone();
    let client = KeepAwakeClient::new(move || *counter.borrow_mut() += 1);
    client.start();
    let signals: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let sink = signals.clone();
    let session = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).unwrap();
    #[allow(deprecated)]
    let _subscription = session.signal_subscribe(None, None, Some("Changed"), Some("/org/waybar/KeepAwake"), None,
        gio::DBusSignalFlags::NONE, move |_, _, _, interface, _, params| {
            sink.borrow_mut().push((interface.into(), params.child_value(0).get::<String>().unwrap()));
        });
    let mut daemon = env.daemon();
    assert!(wait_for(|| has_owner("org.waybar.KeepAwake"), 5.0), "daemon did not register");
    assert!(wait_for(|| client.ready(), 5.0), "daemon never saw the fake logind");
    let status = env.status();
    assert_eq!(status["class"], "inactive");
    assert_eq!(status["ready"], true);
    let keys: Vec<&str> = status.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["text", "tooltip", "duration", "until", "class", "active", "ready", "requested", "pending", "error"]);

    // A second --background is forwarded to the primary and returns at once.
    let (code, _, _) = env.run(&["--background"]);
    assert_eq!(code, 0);

    let (code, _, err) = env.run(&["--enable"]);
    assert_eq!((code, err.as_str()), (0, ""), "--enable must not try to open the popup");
    assert!(wait_for(|| client.active(), 5.0));
    assert_eq!(login.calls.borrow()[0],
               ("idle:sleep".into(), "Keep Awake".into(), "Keep this desktop awake until turned off".into(), "block".into()));
    assert_eq!(login.reader(0), Read::WouldBlock);
    let status = env.status();
    assert_eq!((status["class"].as_str(), status["text"].as_str()), (Some("active"), Some("\u{f06e}")));
    assert_eq!(status["tooltip"],
        "Keep Awake: Active\nAutomatic dimming, display off, and sleep are paused.\nClick for controls · Right-click to toggle");

    let (code, _, _) = env.run(&["--disable"]);
    assert_eq!(code, 0);
    assert!(wait_for(|| login.reader(0) == Read::Eof, 5.0), "--disable kept the inhibitor");
    let (code, _, _) = env.run(&["--toggle"]);
    assert_eq!(code, 0);
    assert!(wait_for(|| client.active(), 5.0));
    assert_eq!(login.reader(1), Read::WouldBlock);

    // D-Bus controls (Global Settings path).
    client.set_enabled(false);
    assert!(wait_for(|| !client.active() && login.reader(1) == Read::Eof, 5.0));
    let reply = session.call_sync(Some("org.waybar.KeepAwake"), "/org/waybar/KeepAwake", "org.waybar.KeepAwake.Duration",
        "SetDuration", Some(&(45u32,).to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 15000,
        None::<&gio::Cancellable>);
    let error = reply.unwrap_err();
    assert_eq!(gio::DBusError::remote_error(&error).as_deref(), Some("org.waybar.KeepAwake.Duration.Error"));
    let mut stripped = error.clone();
    gio::DBusError::strip_remote_error(&mut stripped);
    assert_eq!(stripped.message(), "Unsupported Keep Awake duration.");
    signals.borrow_mut().clear();
    assert!(client.request_duration(30));
    assert!(wait_for(|| client.duration() == 30, 5.0));
    assert!(wait_for(|| signals.borrow().iter().any(|(i, _)| i == "org.waybar.KeepAwake.Duration"), 5.0));
    assert!(signals.borrow().iter().any(|(i, _)| i == "org.waybar.KeepAwake.Controls"));
    let reply = session.call_sync(Some("org.waybar.KeepAwake"), "/org/waybar/KeepAwake", "org.waybar.KeepAwake.Controls",
        "SetEnabled", Some(&(true,).to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 15000,
        None::<&gio::Cancellable>).unwrap();
    assert_eq!(reply.get::<(bool, String)>(), Some((true, String::new())));
    assert!(wait_for(|| client.active(), 5.0));
    let until = client.until().expect("a 30 minute timer has an end time");
    let now = keep_awake::compat::now();
    assert!((until - now - 1800.0).abs() < 30.0);
    assert_eq!(env.timer()["duration"], 30);
    assert_eq!(env.timer()["until"].as_f64(), Some(until));
    let get_state = session.call_sync(Some("org.waybar.KeepAwake"), "/org/waybar/KeepAwake", "org.waybar.KeepAwake.Duration",
        "GetState", None, None, gio::DBusCallFlags::NO_AUTO_START, 15000, None::<&gio::Cancellable>).unwrap();
    let state_json: Value = serde_json::from_str(&get_state.child_value(0).get::<String>().unwrap()).unwrap();
    assert_eq!(state_json["until"].as_f64(), Some(until));

    // logind loss: unavailable + FD released; reappearance re-acquires.
    let held = login.readers.borrow().len() - 1;
    login.disown();
    assert!(wait_for(|| !client.ready(), 5.0));
    assert_eq!(env.status()["class"], "unavailable");
    assert!(wait_for(|| login.reader(held) == Read::Eof, 5.0));
    login.own();
    assert!(wait_for(|| client.active(), 5.0), "did not re-acquire after logind returned");

    // SIGTERM releases the FD but keeps the unexpired timer for the next start.
    let held = login.readers.borrow().len() - 1;
    assert_eq!(terminate(&mut daemon), 0);
    assert!(wait_for(|| login.reader(held) == Read::Eof, 5.0), "shutdown kept the inhibitor");
    assert_eq!(env.timer()["until"].as_f64(), Some(until));
    assert!(wait_for(|| !has_owner("org.waybar.KeepAwake"), 5.0));
    assert!(wait_for(|| client.error() == "Keep Awake is not running in this session.", 5.0));
    assert!(!client.request_duration(60), "no owner: SetDuration reports unavailable");

    let calls = login.calls.borrow().len();
    let mut daemon = env.daemon();
    assert!(wait_for(|| client.active(), 5.0), "unexpired timer did not resume");
    assert_eq!(login.calls.borrow().len(), calls + 1);
    assert_eq!(client.duration(), 30);
    // "Until turned off" is not restored across restarts.
    let (code, _, _) = env.run(&["--disable"]);
    assert_eq!(code, 0);
    client.request_duration(0);
    assert!(wait_for(|| client.duration() == 0, 5.0));
    let (code, _, _) = env.run(&["--enable"]);
    assert_eq!(code, 0);
    assert!(wait_for(|| client.active(), 5.0));
    assert_eq!(env.timer(), serde_json::json!({"duration": 0, "until": null}));
    assert_eq!(terminate(&mut daemon), 0);
    let mut daemon = env.daemon();
    assert!(wait_for(|| client.ready(), 5.0));
    settle(0.3);
    assert!(!client.requested());
    assert_eq!(terminate(&mut daemon), 0);
    assert!(*updates.borrow() > 0);
    client.close();
    drop(login);
    let _ = std::fs::remove_dir_all(&state);
}
