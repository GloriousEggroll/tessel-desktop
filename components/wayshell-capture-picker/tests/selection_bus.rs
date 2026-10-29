//! CaptureSelection client against a fake SelectionBroker on a private
//! `gio::TestDBus` bus (never the session bus), plus the real binary's
//! preselection / Selected report path without any display.

use gio::prelude::*;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use wayshell_capture_picker::selection::*;
use wayshell_capture_picker::sources::parse_sources;

#[derive(Default)]
struct Broker {
    /// session → monitor name the requester wants (Python `targets`).
    targets: Vec<(String, String)>,
    hang: bool,
    selects: Vec<(String, Vec<String>)>,
    selected: Vec<(String, Vec<String>)>,
}

fn start_broker(address: String, state: Arc<Mutex<Broker>>) -> String {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let connection = gio::DBusConnection::for_address_sync(&address,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None, None::<&gio::Cancellable>).unwrap();
            let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
            let interface = node.lookup_interface(INTERFACE).unwrap();
            let held = Arc::new(Mutex::new(Vec::<gio::DBusMethodInvocation>::new()));
            let _registration = connection.register_object(PATH, &interface).method_call(move |_, _, _, _, method, parameters, invocation| {
                let mut state = state.lock().unwrap();
                if method == "Selected" {
                    let (session, outputs) = parameters.get::<(glib::variant::ObjectPath, Vec<String>)>().unwrap();
                    state.selected.push((session.as_str().to_owned(), outputs));
                    invocation.return_value(None);
                    return;
                }
                let (session, tokens) = parameters.get::<(glib::variant::ObjectPath, Vec<String>)>().unwrap();
                let session = session.as_str().to_owned();
                state.selects.push((session.clone(), tokens.clone()));
                if state.hang {
                    held.lock().unwrap().push(invocation);
                    return;
                }
                let position = state.targets.iter().position(|(s, _)| *s == session);
                let target = position.map(|i| state.targets.remove(i).1);
                let chosen = target.as_ref().and_then(|name| {
                    tokens.iter().find(|t| t.strip_prefix("Monitor: ").is_some_and(|r| r.split(' ').next() == Some(name.as_str())))
                }).cloned().unwrap_or_default();
                invocation.return_value(Some(&(target.is_some(), chosen).to_variant()));
            }).build().unwrap();
            tx.send(connection.unique_name().unwrap().to_string()).unwrap();
            glib::MainLoop::new(Some(&context), false).run();
        }).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10)).expect("broker did not start")
}

fn session_for(owner: &str, suffix: &str) -> String {
    format!("/org/freedesktop/portal/desktop/session/{}/capture_{suffix}", owner.trim_start_matches(':').replace('.', "_"))
}

fn check(name: &str, ok: bool) {
    assert!(ok, "{name}");
    println!("ok {name}");
}

fn main() {
    // Pure owner derivation first (no bus).
    check("owner_from_session", session_owner("/org/freedesktop/portal/desktop/session/1_42/capture_0af") == Some(":1.42".into()));
    check("owner_multi_part", session_owner("/org/freedesktop/portal/desktop/session/1_2_3/capture_a") == Some(":1.2.3".into()));
    for bad in ["", "/org/freedesktop/portal/desktop/session/1/capture_a", "/org/freedesktop/portal/desktop/session/1_9/s1",
                "/org/freedesktop/portal/desktop/session/1_9/capture_A", "/org/freedesktop/portal/desktop/session/1_9/capture_",
                "/org/freedesktop/portal/desktop/session/x_9/capture_a", "/org/freedesktop/portal/desktop/session/1_9/capture_a/x"] {
        check(&format!("rejects {bad:?}"), session_owner(bad).is_none());
    }

    let runtime = std::env::temp_dir().join(format!("capture-bus-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    assert!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap() == address, "TestDBus must own the session address");
    let state = Arc::new(Mutex::new(Broker::default()));
    let owner = start_broker(address.clone(), state.clone());
    let client = gio::DBusConnection::for_address_sync(&address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>).unwrap();
    let sources = parse_sources(b"Monitor: DP-1 Left\nMonitor: HDMI-A-1 Right\nWindow: Editor (ext-1)\n").unwrap();

    let session = session_for(&owner, "ab12");
    state.lock().unwrap().targets.push((session.clone(), "HDMI-A-1".into()));
    let (handled, chosen) = requested_selection_on(Some(&client), &session, &sources);
    check("preselects_requested_monitor", handled && chosen.as_ref() == Some(&sources[1]));
    check("select_receives_exact_tokens", state.lock().unwrap().selects.last().unwrap().1
          == sources.iter().map(|s| s.token.clone()).collect::<Vec<_>>());
    let (handled, chosen) = requested_selection_on(Some(&client), &session, &sources);
    check("one_shot_target", !handled && chosen.is_none());

    state.lock().unwrap().targets.push((session.clone(), "DP-9".into()));
    let (handled, chosen) = requested_selection_on(Some(&client), &session, &sources);
    check("vanished_output_cancels", handled && chosen.is_none());

    let calls = state.lock().unwrap().selects.len();
    let (handled, _) = requested_selection_on(Some(&client), "/org/freedesktop/portal/desktop/session/1_9/s1", &sources);
    check("invalid_session_makes_no_call", !handled && state.lock().unwrap().selects.len() == calls);
    let (handled, _) = requested_selection_on(Some(&client), &session_for(":1.9999", "ab"), &sources);
    check("unknown_owner_keeps_ui", !handled);

    report_selection_on(Some(&client), &session, &["HDMI-A-1".to_owned()]);
    check("selected_reported", state.lock().unwrap().selected.last() == Some(&(session.clone(), vec!["HDMI-A-1".to_owned()])));
    let reported = state.lock().unwrap().selected.len();
    report_selection_on(Some(&client), "/bad", &["x".to_owned()]);
    check("invalid_session_not_reported", state.lock().unwrap().selected.len() == reported);

    // A requester that never answers costs at most the 1200 ms timeout.
    state.lock().unwrap().hang = true;
    let started = Instant::now();
    let (handled, _) = requested_selection_on(Some(&client), &session, &sources);
    let elapsed = started.elapsed();
    check(&format!("hung_requester_times_out ({} ms)", elapsed.as_millis()),
          !handled && elapsed >= Duration::from_millis(1100) && elapsed < Duration::from_millis(2500));
    state.lock().unwrap().hang = false;

    // The real binary: preselection answers, nothing is shown, the exact
    // record is printed and the outputs reach the requester. No display at all.
    let binary = env!("CARGO_BIN_EXE_wayshell-capture-picker");
    let run = |session: &str, remote: Option<&str>, input: &[u8]| {
        let mut command = Command::new(binary);
        command.env_clear().env("HOME", std::env::var("HOME").unwrap()).env("PATH", std::env::var("PATH").unwrap())
            .env("DBUS_SESSION_BUS_ADDRESS", &address).env("XDG_RUNTIME_DIR", &runtime).env("XDPW_CHOOSER_SESSION", session)
            .env("DESKTOP_SUITE_LOCALE_DIR", "/nonexistent").env("GDK_BACKEND", "wayland")
            .env("GDK_DEBUG", "no-portals").env("GIO_USE_VFS", "local").env("GTK_USE_PORTAL", "0")
            .env("WAYLAND_DISPLAY", "no-such-display")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(remote) = remote {
            command.env("XDPW_CHOOSER_REMOTE_DESKTOP", remote);
        }
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    };
    let input = b"Monitor: DP-1 Left\nMonitor: HDMI-A-1 Right\n";
    let session = session_for(&owner, "cd34");
    state.lock().unwrap().targets.push((session.clone(), "DP-1".into()));
    let output = run(&session, None, input);
    check("binary_preselected_record", output.status.code() == Some(0) && output.stdout == b"Monitor: DP-1 Left\n");
    check("binary_reports_outputs", state.lock().unwrap().selected.last() == Some(&(session.clone(), vec!["DP-1".to_owned()])));

    // Remote control never asks the requester and needs the visible picker:
    // without a display that is an error (exit 1) and never output.
    let session = session_for(&owner, "ef56");
    state.lock().unwrap().targets.push((session.clone(), "DP-1".into()));
    let calls = state.lock().unwrap().selects.len();
    let output = run(&session, Some("3"), input);
    check("binary_remote_never_preselects", state.lock().unwrap().selects.len() == calls);
    check("binary_gui_error_is_no_consent", output.status.code() == Some(1) && output.stdout.is_empty()
          && String::from_utf8_lossy(&output.stderr).contains("Wayshell capture picker:"));
    let output = run(&session, None, b"Monitor: DP-1 Left\n");
    check("binary_preselect_consumes_target", output.status.code() == Some(0) && output.stdout == b"Monitor: DP-1 Left\n");
    let output = Command::new(binary).arg("extra").env("XDG_RUNTIME_DIR", &runtime).env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null()).output().unwrap();
    check("binary_rejects_arguments", output.status.code() == Some(1) && output.stdout.is_empty());

    bus.down();
    let _ = std::fs::remove_dir_all(&runtime);
}
