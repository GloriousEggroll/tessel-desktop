//! End-to-end checks of the real `network-manager` and `network-status`
//! binaries on a private dbus-daemon (gio::TestDBus) hosting a FAKE
//! NetworkManager, Secret Service and notification server in this process.
//! The children's session AND "system" bus are that private daemon; there is
//! no display, so `--background` never initializes GTK. No real
//! NetworkManager method is ever called and no real secret is read.

use gio::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

const MANAGER: &str = env!("CARGO_BIN_EXE_network-manager");
const STATUS: &str = env!("CARGO_BIN_EXE_network-status");

#[path = "support/fake_nm.rs"]
mod fake_nm;
use fake_nm::*;

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

fn wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if condition() {
            return;
        }
        pump(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

struct Env {
    vars: Vec<(String, String)>,
}

impl Env {
    fn command(&self, binary: &str, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command.args(args).env_clear().envs(self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        command
    }

    /// Run a launcher while the in-process fakes keep answering.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let mut child = self.command(MANAGER, args).spawn().unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                return (status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into(),
                        String::from_utf8_lossy(&output.stderr).into());
            }
            assert!(Instant::now() < end, "launcher {args:?} did not exit");
            pump(Duration::from_millis(10));
        }
    }
}

fn agent_call(connection: &gio::DBusConnection, destination: &str, method: &str, params: Variant) -> Result<Variant, glib::Error> {
    let result: Rc<RefCell<Option<Result<Variant, glib::Error>>>> = Rc::default();
    let r = result.clone();
    connection.call(Some(destination), "/org/freedesktop/NetworkManager/SecretAgent", "org.freedesktop.NetworkManager.SecretAgent",
                    method, Some(&params), None, gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>,
                    move |reply| { r.replace(Some(reply)); });
    wait_for(method, Duration::from_secs(6), || result.borrow().is_some());
    let reply = result.take().unwrap();
    reply
}

fn rss(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default().lines()
        .find(|l| l.starts_with("VmRSS")).unwrap_or("").to_owned()
}

fn agent_owner(connection: &gio::DBusConnection) -> String {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner",
                         Some(&("org.waybar.NetworkManager",).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>)
        .unwrap().child_value(0).str().unwrap().to_owned()
}

fn count(world: &Shared, method: &str) -> usize {
    world.borrow().calls.iter().filter(|c| c.1 == method).count()
}

#[test]
fn manager_agent_and_status_stream_on_private_bus() {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("nm-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["config", "runtime"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::set_permissions(root.join("runtime"), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    let world: Shared = Rc::default();
    let nm = serve_nm(&address, &world);
    let _secrets = serve_secrets(&address, &world);
    let _notifications = serve_notifications(&address, &world);
    let env = Env { vars: vec![
        ("HOME".into(), std::env::var("HOME").unwrap_or_default()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("XDG_CONFIG_HOME".into(), root.join("config").display().to_string()),
        ("XDG_RUNTIME_DIR".into(), root.join("runtime").display().to_string()),
        ("DBUS_SESSION_BUS_ADDRESS".into(), address.clone()),
        ("DBUS_SYSTEM_BUS_ADDRESS".into(), address.clone()),
        ("GSETTINGS_BACKEND".into(), "memory".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ] };

    // Bad arguments exit 2 before D-Bus; a probe without a primary never becomes one.
    let (code, _, stderr) = env.run(&["--popup-offset", "-3"]);
    assert_eq!(code, 2);
    assert!(stderr.ends_with("error: --popup-offset must be nonnegative\n"), "{stderr}");
    let (code, _, _) = env.run(&["--check"]);
    assert_ne!(code, 0);
    pump(Duration::from_millis(200));
    assert_eq!(count(&world, "RegisterWithCapabilities"), 0, "a --check probe never registers an agent");
    assert_eq!(count(&world, "GetManagedObjects"), 0, "a --check probe never builds a backend");

    let started = Instant::now();
    let mut manager: Child = env.command(MANAGER, &["--background"]).spawn().unwrap();
    let mut report = String::new();
    wait_for("--check ready", Duration::from_secs(10), || {
        let (code, stdout, _) = env.run(&["--check"]);
        report = stdout;
        code == 0
    });
    println!("time-to-ready (first passing --check) {:?}", started.elapsed());
    assert_eq!(report, "{\"ready\": true, \"network_ready\": true, \"agent_ready\": true, \"devices\": 2, \"profiles\": 2}\n");
    let registration = world.borrow().calls.iter().find(|c| c.1 == "RegisterWithCapabilities").cloned().unwrap();
    assert_eq!(registration.2, "('org.waybar.NetworkManager', 1)");

    // Remote launch with a real flag reuses the primary (exit 0, still one agent).
    let (code, _, stderr) = env.run(&["--background"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(count(&world, "RegisterWithCapabilities"), 1);

    // The agent only answers NetworkManager's unique name.
    let owner = world.borrow().agent.clone();
    assert!(owner.starts_with(':'));
    let _ = agent_owner;
    let stranger = connect(&address);
    let wifi = wifi_settings();
    let request = Variant::tuple_from_iter([wifi.clone(), o("/org/freedesktop/NetworkManager/Settings/2"),
        "802-11-wireless-security".to_variant(), Vec::<String>::new().to_variant(), 0u32.to_variant()]);
    let error = agent_call(&stranger, &owner, "GetSecrets", request.clone()).unwrap_err();
    assert!(gio::DBusError::remote_error(&error).unwrap().ends_with("SecretAgent.PermissionDenied"), "{error}");
    // Non-interactive request, nothing saved: NoSecrets, keyring never unlocked.
    let error = agent_call(&nm.connection, &owner, "GetSecrets", request.clone()).unwrap_err();
    assert!(gio::DBusError::remote_error(&error).unwrap().ends_with("SecretAgent.NoSecrets"), "{error}");
    assert_eq!(count(&world, "Unlock"), 0);
    // A saved (synthetic) secret is returned without a prompt.
    world.borrow_mut().secrets.push(([("connection-uuid", "22222222-2222-4222-8222-222222222222"),
        ("setting-name", "802-11-wireless-security"), ("setting-key", "psk")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        "synthetic-passphrase".into()));
    let reply = agent_call(&nm.connection, &owner, "GetSecrets", request).unwrap();
    assert_eq!(reply.print(false).as_str(), "({'802-11-wireless-security': {'psk': <'synthetic-passphrase'>}},)");

    // NetworkManager restarts: the agent registers again.
    release(&nm.connection, NM);
    pump(Duration::from_millis(300));
    own(&nm.connection, NM);
    wait_for("re-registration", Duration::from_secs(5), || count(&world, "RegisterWithCapabilities") == 2);
    wait_for("backend reload", Duration::from_secs(5), || count(&world, "GetManagedObjects") >= 2);

    // Captive portal: one notification with the Sign in action, withdrawn when online again.
    nm.root_changed("Connectivity", 2u32.to_variant());
    wait_for("portal notification", Duration::from_secs(5), || world.borrow().notifications.iter().any(|n| n.starts_with("Sign in to network|")));
    assert!(world.borrow().notifications.iter().any(|n| n.contains("Sign in")), "{:?}", world.borrow().notifications);
    nm.root_changed("Connectivity", 4u32.to_variant());
    wait_for("portal withdrawn", Duration::from_secs(5), || world.borrow().notifications.iter().any(|n| n == "closed"));
    let portals = world.borrow().notifications.iter().filter(|n| n.starts_with("Sign in to network|")).count();
    assert_eq!(portals, 1);

    // Status stream: one JSON line per change.
    let mut status = env.command(STATUS, &[]).spawn().unwrap();
    let stdout = status.stdout.take().unwrap();
    let lines: Rc<RefCell<Vec<String>>> = Rc::default();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let collect = |lines: &Rc<RefCell<Vec<String>>>| while let Ok(line) = rx.try_recv() { lines.borrow_mut().push(line) };
    wait_for("connected status", Duration::from_secs(5), || {
        collect(&lines);
        lines.borrow().iter().any(|l| l.contains("\"class\": [\"connected\", \"wired\"]"))
    });
    assert!(lines.borrow()[0].contains("\"unavailable\""), "first line before the snapshot: {:?}", lines.borrow());
    let connected = lines.borrow().last().unwrap().clone();
    assert!(connected.starts_with("{\"text\": \"\\udb80\\ude00\", \"tooltip\": \"Connected: Wired connection\\nenp4s0: Connected\\nwlan0: Disconnected\\n"),
            "{connected}");
    *nm.networking.borrow_mut() = false;
    nm.root_changed("NetworkingEnabled", false.to_variant());
    wait_for("disabled status", Duration::from_secs(5), || {
        collect(&lines);
        lines.borrow().last().is_some_and(|l| l.contains("[\"disabled\"]"))
    });
    let maps = std::fs::read_to_string(format!("/proc/{}/maps", status.id())).unwrap_or_default();
    assert!(!maps.contains("libnm.so"), "the status stream stays libnm-free");
    let manager_maps = std::fs::read_to_string(format!("/proc/{}/maps", manager.id())).unwrap_or_default();
    assert!(manager_maps.contains("libnm.so"), "the daemon uses libnm for compatibility/AP checks");
    unsafe { libc::kill(status.id() as i32, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_secs(5);
    while status.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "status stream ignores SIGTERM");
        pump(Duration::from_millis(10));
    }

    // A connection going away: one "Network disconnected" notification.
    nm.root_changed("ActiveConnections", Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, Vec::<Variant>::new()));
    wait_for("disconnected notification", Duration::from_secs(5),
             || world.borrow().notifications.iter().any(|n| n.starts_with("Network disconnected|")));

    // Startup and every refresh only read: no mutation, no profile secret read.
    let calls = world.borrow().calls.clone();
    for forbidden in ["Enable", "ActivateConnection", "AddAndActivateConnection", "DeactivateConnection", "Update2", "Delete",
                      "Set", "SetManaged", "RequestScan", "CreateItem"] {
        assert!(!calls.iter().any(|c| c.1 == forbidden), "{forbidden} was called");
    }
    assert!(!calls.iter().any(|c| c.0 == "Settings.Connection" && c.1 == "GetSecrets"));
    println!("manager {}", rss(manager.id()));

    // SIGTERM: agent unregisters before exit.
    unsafe { libc::kill(manager.id() as i32, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = manager.try_wait().unwrap() {
            assert!(status.success(), "{status:?}");
            break;
        }
        assert!(Instant::now() < end, "manager ignores SIGTERM");
        pump(Duration::from_millis(10));
    }
    wait_for("Unregister", Duration::from_secs(3), || count(&world, "Unregister") == 1);
    let _ = std::fs::remove_dir_all(&root);
    bus.down();
}
