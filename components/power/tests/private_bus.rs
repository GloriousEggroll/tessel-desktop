//! End-to-end checks of the real `tessel-power` and `tessel-power-status` binaries on
//! a private dbus-daemon that hosts FAKE logind / UPower / power-profiles /
//! notification services (in this process). Both the session and the "system"
//! bus of the children point at that private daemon. swayidle, wlopm,
//! systemctl and systemd-run are logging shell fakes first on PATH. Nothing
//! here can suspend, lock, power off or touch a display (no WAYLAND_DISPLAY).

use gio::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

const MANAGER: &str = env!("CARGO_BIN_EXE_tessel-power");
const STATUS: &str = env!("CARGO_BIN_EXE_tessel-power-status");

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

#[path = "support/fake_services.rs"]
mod fake_services;
use fake_services::{serve, Shared, World};

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Env {
    root: PathBuf,
    vars: Vec<(String, String)>,
}

impl Env {
    fn command(&self, binary: &str, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command.args(args).env_clear().envs(self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        command
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join("log").join(name)).unwrap_or_default()
    }

    /// Run a launcher while the in-process fake services keep answering.
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

    fn swayidle_argv(&self) -> Vec<String> {
        let pids = self.log("swayidle.pids");
        let last = pids.lines().last().expect("swayidle spawned").trim().to_owned();
        self.log(&format!("swayidle.{last}")).lines().map(String::from).collect()
    }
}

/// Enough shlex for the generated commands (single-quoted and plain words).
fn split(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut in_word) = (false, false);
    for c in text.chars() {
        match c {
            '\'' => { quoted = !quoted; in_word = true; }
            ' ' if !quoted => if in_word { words.push(std::mem::take(&mut current)); in_word = false; },
            c => { current.push(c); in_word = true; }
        }
    }
    if in_word { words.push(current); }
    words
}

fn controls(connection: &gio::DBusConnection, method: &str, params: Option<Variant>) -> Result<Variant, glib::Error> {
    let result: Rc<RefCell<Option<Result<Variant, glib::Error>>>> = Rc::default();
    let r = result.clone();
    connection.call(Some("org.tessel.PowerManager"), "/org/tessel/PowerManager", "org.tessel.PowerManager.Controls", method,
                    params.as_ref(), None, gio::DBusCallFlags::NO_AUTO_START, 5000, None::<&gio::Cancellable>,
                    move |reply| { r.replace(Some(reply)); });
    wait_for(method, Duration::from_secs(6), || result.borrow().is_some());
    let reply = result.take().unwrap();
    reply
}

fn rss(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default().lines()
        .find(|l| l.starts_with("VmRSS")).unwrap_or("").to_owned()
}

#[test]
fn manager_and_status_stream_on_private_bus() {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("pm-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["bin", "log", "config/waybar/scripts", "config/tessel-power", "config/labwc", "runtime"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::set_permissions(root.join("runtime"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let l = root.join("log").display().to_string();
    script(&root.join("bin/swayidle"), &format!("printf '%s\\n' \"$@\" > {l}/swayidle.$$\necho $$ >> {l}/swayidle.pids\nexec sleep 120"));
    script(&root.join("bin/wlopm"), &format!("echo \"$*\" >> {l}/wlopm.log"));
    script(&root.join("bin/systemctl"), &format!("echo \"$*\" >> {l}/systemctl.log"));
    script(&root.join("bin/systemd-run"), &format!("echo \"$*\" >> {l}/systemd-run.log"));
    script(&root.join("config/waybar/scripts/screenlock.sh"), "exit 0");
    std::fs::write(root.join("config/tessel-power/settings.json"),
                   r#"{"version": 1, "ac": {"display_off": 600, "sleep_after": 900, "sleep_action": "suspend"}, "power_button": "suspend"}"#).unwrap();
    std::fs::write(root.join("config/labwc/rc.xml"), "<labwc_config/>").unwrap();

    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    let world: Shared = Rc::new(RefCell::new(World { active_profile: "balanced".into(), ..Default::default() }));
    let services = serve(&address, &world);
    let env = Env {
        root: root.clone(),
        vars: vec![
            ("HOME".into(), std::env::var("HOME").unwrap_or_default()),
            ("PATH".into(), format!("{}:/usr/bin:/bin", root.join("bin").display())),
            ("XDG_CONFIG_HOME".into(), root.join("config").display().to_string()),
            ("XDG_RUNTIME_DIR".into(), root.join("runtime").display().to_string()),
            ("DBUS_SESSION_BUS_ADDRESS".into(), address.clone()),
            ("DBUS_SYSTEM_BUS_ADDRESS".into(), address.clone()),
            ("POWER_MANAGER_EXECUTABLE".into(), MANAGER.into()),
            ("POWER_MANAGER_SCRIPTS_DIR".into(), root.join("config/waybar/scripts").display().to_string()),
            ("GSETTINGS_BACKEND".into(), "memory".into()),
            ("GDK_DEBUG".into(), "no-portals".into()), ("GIO_USE_VFS".into(), "local".into()), ("GTK_USE_PORTAL".into(), "0".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ],
    };

    // No primary: --check fails without becoming one.
    let (code, _, stderr) = env.run(&["--check"]);
    assert_ne!(code, 0, "{stderr}");
    let (code, _, stderr) = env.run(&["--idle-action", "lock"]);
    assert_eq!(code, 2);
    assert!(stderr.ends_with("error: Idle events require --policy-token\n"), "{stderr}");

    let started = Instant::now();
    let mut manager: Child = env.command(MANAGER, &["--background"]).spawn().unwrap();
    let mut report = String::new();
    wait_for("--check ready", Duration::from_secs(10), || {
        let (code, stdout, _) = env.run(&["--check"]);
        report = stdout;
        code == 0
    });
    println!("time-to-ready (first passing --check) {:?}", started.elapsed());
    let health: serde_json::Value = serde_json::from_str(report.trim()).unwrap();
    assert_eq!(health["active_profile"], "balanced");
    assert_eq!(health["lid_handled"], true, "{health}");
    assert_eq!(health["power_key_handled"], false, "no XF86PowerOff keybind");

    // swayidle argv: absolute fakes and token-bound callbacks into this binary.
    pump(Duration::from_millis(300));
    let argv = env.swayidle_argv();
    assert_eq!(argv[..3], ["-w", "-C", "/dev/null"]);
    let timeout = |action: &str| argv.iter().map(|w| split(w)).find(|w| w.len() == 5 && w[1] == "--idle-action" && w[2] == action);
    let lock = timeout("lock").expect("lock timer");
    assert_eq!(lock[0], MANAGER);
    let token = lock[4].clone();
    assert_eq!(token.len(), 32);
    assert!(timeout("suspend").is_some(), "AC sleep is armed: source known, CanSuspend=yes");
    let lock_index = argv.iter().position(|w| w == "lock").unwrap();
    assert_eq!(split(&argv[lock_index + 1]), [root.join("bin/systemctl").display().to_string(), "--user".into(), "start".into(),
                                               "tessel-lock.service".into()]);
    let resume = argv.iter().find(|w| w.contains("--idle-resume display-off")).unwrap();
    assert!(resume.ends_with(&format!("</dev/null >/dev/null 2>&1 & {}/bin/wlopm --on '*'", root.display())), "{resume}");

    // Stale token: nothing. Current token: lock via the independent unit.
    let (code, _, stderr) = env.run(&["--idle-action", "lock", "--policy-token", "stale"]);
    assert_eq!(code, 0, "{stderr}");
    pump(Duration::from_millis(300));
    assert_eq!(env.log("systemctl.log"), "");
    let (code, _, _) = env.run(&["--idle-action", "lock", "--policy-token", &token]);
    assert_eq!(code, 0);
    wait_for("lock unit start", Duration::from_secs(5), || env.log("systemctl.log").contains("--user start tessel-lock.service"));
    assert!(world.borrow().calls.iter().any(|c| c.1 == "ListInhibitors"), "inhibitors checked first");
    assert!(world.borrow().calls.iter().any(|c| c.1 == "GetSession" && c.2 == "('auto',)"), "session checked first");

    // Display off, then wake on resume.
    env.run(&["--idle-action", "display-off", "--policy-token", &token]);
    wait_for("wlopm --off", Duration::from_secs(5), || env.log("wlopm.log").contains("--off *"));
    env.run(&["--idle-resume", "display-off", "--policy-token", &token]);
    wait_for("wlopm --on", Duration::from_secs(5), || env.log("wlopm.log").contains("--on *"));

    // Automatic suspend: a non-interactive logind call after fresh checks (fake logind).
    env.run(&["--idle-action", "suspend", "--policy-token", &token]);
    wait_for("Suspend(false)", Duration::from_secs(5), || world.borrow().calls.iter().any(|c| c.1 == "Suspend" && c.2 == "(false,)"));

    // Power button = suspend: lock first, then interactive Suspend(true).
    let before = env.log("systemctl.log").lines().count();
    env.run(&["--power-button"]);
    wait_for("Suspend(true)", Duration::from_secs(5), || world.borrow().calls.iter().any(|c| c.1 == "Suspend" && c.2 == "(true,)"));
    assert_eq!(env.log("systemctl.log").lines().count(), before + 1, "locked before sleeping");

    // Lid close edge (not docked) follows lid_close.ac = suspend.
    let suspends = |w: &Shared| w.borrow().calls.iter().filter(|c| c.1 == "Suspend").count();
    let count = suspends(&world);
    world.borrow_mut().lid_closed = true;
    let changed = glib::VariantDict::new(None);
    changed.insert_value("LidClosed", &true.to_variant());
    services[0].emit_signal(None, "/org/freedesktop/login1", "org.freedesktop.DBus.Properties", "PropertiesChanged",
        Some(&Variant::tuple_from_iter(["org.freedesktop.login1.Manager".to_variant(), changed.end(), Vec::<String>::new().to_variant()]))).unwrap();
    wait_for("lid suspend", Duration::from_secs(5), || suspends(&world) > count);

    // Settings transport (what Global Settings and tessel-lock use).
    let client = gio::DBusConnection::for_address_sync(&address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>).unwrap();
    let state: String = controls(&client, "GetState", None).unwrap().child_value(0).get().unwrap();
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(state["ready"], true);
    assert_eq!(state["settings"]["ac"]["sleep_after"], 900);
    let reply = controls(&client, "SetSettings", Some(("{\"lock_after\": 120, \"power_button\": \"nothing\"}",).to_variant())).unwrap();
    assert_eq!(reply.get::<(bool, String)>(), Some((true, String::new())));
    let saved = std::fs::read_to_string(root.join("config/tessel-power/settings.json")).unwrap();
    assert!(saved.contains("\"lock_after\": 120") && saved.starts_with("{\n  \"version\": 1,"), "{saved}");
    wait_for("swayidle respawn", Duration::from_secs(5), || env.swayidle_argv().iter().any(|w| w == "120"));
    let rejected = controls(&client, "SetSettings", Some(("{\"lock_after\": -5}",).to_variant())).unwrap();
    assert!(!rejected.get::<(bool, String)>().unwrap().0);
    let invalid = controls(&client, "SetSettings", Some(("[]",).to_variant()));
    assert!(invalid.unwrap_err().message().contains("Invalid power settings."));
    let locks = env.log("systemctl.log").lines().count();
    env.run(&["--idle-action", "lock", "--policy-token", &token]);
    pump(Duration::from_millis(300));
    assert_eq!(env.log("systemctl.log").lines().count(), locks, "a revoked token never acts");
    assert!(!world.borrow().calls.iter().any(|c| c.0 == "ppd"), "startup and refresh never write profiles");

    // Battery alerts: discharging at 9 % → one "Battery low" notification.
    {
        let mut w = world.borrow_mut();
        w.on_battery = true;
        w.battery = Some(HashMap::from([("Type", 2u32.to_variant()), ("PowerSupply", true.to_variant()), ("IsPresent", true.to_variant()),
                                        ("Percentage", 9.0f64.to_variant()), ("WarningLevel", 1u32.to_variant()), ("State", 2u32.to_variant()),
                                        ("NativePath", "BAT0".to_variant()), ("Model", "Fake battery".to_variant())]));
    }
    services[1].emit_signal(None, "/org/freedesktop/UPower", "org.freedesktop.UPower", "DeviceAdded",
        Some(&(glib::variant::ObjectPath::try_from("/org/freedesktop/UPower/devices/battery_BAT0").unwrap(),).to_variant())).unwrap();
    wait_for("low battery notification", Duration::from_secs(5), || !world.borrow().notifications.is_empty());
    assert_eq!(world.borrow().notifications[0], ("Battery low".into(), "9% remaining. Consider plugging in.".into(), 1));

    // Status stream: gio-only, deduplicated JSON lines.
    let mut status = env.command(STATUS, &[]).spawn().unwrap();
    let stdout = status.stdout.take().unwrap();
    let lines: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let sink = lines.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    wait_for("status line", Duration::from_secs(5), || lines.lock().unwrap().iter().any(|l| l.contains("balanced")));
    assert_eq!(lines.lock().unwrap()[0],
               r#"{"text": "\uf135", "tooltip": "Power profile: Unavailable\nClick for power profiles \u00b7 Right-click for settings", "class": ["unavailable"]}"#);
    pump(Duration::from_millis(500));
    println!("tessel-power-status idle {}", rss(status.id()));
    println!("tessel-power --background idle {}", rss(manager.id()));
    unsafe { libc::kill(status.id() as i32, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_secs(5);
    while status.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "status stream ignores SIGTERM");
        pump(Duration::from_millis(10));
    }

    // SIGTERM: clean exit and the swayidle child is terminated.
    let swayidle_pid: i32 = env.log("swayidle.pids").lines().last().unwrap().trim().parse().unwrap();
    unsafe { libc::kill(manager.id() as i32, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_secs(5);
    let exit = loop {
        if let Some(exit) = manager.try_wait().unwrap() {
            break exit;
        }
        assert!(Instant::now() < end, "manager ignores SIGTERM");
        pump(Duration::from_millis(10));
    };
    assert!(exit.success(), "{exit:?}");
    let end = Instant::now() + Duration::from_secs(3);
    let gone = || std::fs::read_to_string(format!("/proc/{swayidle_pid}/stat")).map(|s| s.contains(") Z ")).unwrap_or(true);
    while !gone() && Instant::now() < end {
        pump(Duration::from_millis(20));
    }
    assert!(gone(), "swayidle child was terminated");
    assert!(!env.log("systemd-run.log").contains("session_manager"));
    drop(services);
    bus.down();
    let _ = std::fs::remove_dir_all(&root);
}
