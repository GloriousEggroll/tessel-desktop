#![allow(deprecated)]
//! End-to-end port of test_brightness_app.py: the real `brightness-manager`
//! and `brightness-status` binaries on a private bus (gio::TestDBus) that is
//! also their "system" bus, with a FAKE logind exported by this process, a
//! FAKE way-displays IPC socket, a FAKE sysfs (`BRIGHTNESS_MANAGER_SYSFS`)
//! and shell fakes for wlr-randr / ddcutil / swayidle as the only PATH.
//! No real monitor, backlight, idle listener or display configuration is touched.
//!
//! Without a display (plain `cargo test`) GTK is unavailable in the child, so
//! the keyboard/OSD and `--settings` steps run only under tests/run_headless.sh
//! (`BRIGHTNESS_ISOLATED_DISPLAY=1`, private nested labwc).

use brightness_manager::compat::yaml;
use gio::prelude::*;
use glib::Variant;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MANAGER: &str = env!("CARGO_BIN_EXE_brightness-manager");
const STATUS: &str = env!("CARGO_BIN_EXE_brightness-status");
const BUS_NAME: &str = "org.waybar.BrightnessManager";
const PATH: &str = "/org/waybar/BrightnessManager";
const INTERFACE: &str = "org.waybar.BrightnessManager.Controls";

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

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn edid(number: u8) -> Vec<u8> {
    let mut data = b"\x00\xff\xff\xff\xff\xff\xff\x00".to_vec();
    data.extend(std::iter::repeat_n(number, 120));
    data
}

/// FAKE logind: ListInhibitors / GetSession / Session properties.
#[derive(Default)]
struct Logind {
    inhibited: bool,
    hold: bool,
    held: Vec<gio::DBusMethodInvocation>,
    calls: Vec<String>,
}

fn inhibitors(inhibited: bool) -> Variant {
    let rows: Vec<(String, String, String, String, u32, u32)> = if inhibited {
        vec![("idle".into(), "video".into(), "playing".into(), "block".into(), 1000, 42)]
    } else {
        Vec::new()
    };
    (rows,).to_variant()
}

fn serve_logind(address: &str, logind: &Rc<RefCell<Logind>>) -> gio::DBusConnection {
    let connection = gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>)
        .unwrap();
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                         Some(&("org.freedesktop.login1", 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                         None::<&gio::Cancellable>).unwrap();
    let manager = gio::DBusNodeInfo::for_xml(r#"<node><interface name="org.freedesktop.login1.Manager">
        <method name="ListInhibitors"><arg type="a(ssssuu)" direction="out"/></method>
        <method name="GetSession"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
      </interface></node>"#).unwrap();
    let l = logind.clone();
    connection.register_object("/org/freedesktop/login1", &manager.lookup_interface("org.freedesktop.login1.Manager").unwrap())
        .method_call(move |_, _, _, _, method, params, invocation| {
            let mut state = l.borrow_mut();
            state.calls.push(format!("{method}{}", params.print(false)));
            match method {
                "ListInhibitors" if state.hold => state.held.push(invocation),
                "ListInhibitors" => invocation.return_value(Some(&inhibitors(state.inhibited))),
                _ => invocation.return_value(Some(&(glib::variant::ObjectPath::try_from("/org/freedesktop/login1/session/c1").unwrap(),)
                    .to_variant())),
            }
        }).build().unwrap();
    let session = gio::DBusNodeInfo::for_xml(r#"<node><interface name="org.freedesktop.login1.Session">
        <property name="Active" type="b" access="read"/><property name="Remote" type="b" access="read"/>
        <property name="Type" type="s" access="read"/></interface></node>"#).unwrap();
    connection.register_object("/org/freedesktop/login1/session/c1", &session.lookup_interface("org.freedesktop.login1.Session").unwrap())
        .property(|_, _, _, _, name| match name {
            "Active" => true.to_variant(),
            "Remote" => false.to_variant(),
            _ => "wayland".to_variant(),
        }).build().unwrap();
    connection
}

/// FAKE way-displays daemon on `$XDG_RUNTIME_DIR/way-displays.sock`.
struct Daemon {
    data: Value,
    calls: Vec<String>,
    saved: Option<Value>,
}

fn serve_way_displays(socket: &Path, daemon: Arc<Mutex<Daemon>>) {
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            stream.set_read_timeout(Some(Duration::from_millis(60))).unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) if !request.is_empty() => break,
                    Ok(0) => break,
                    Ok(n) => request.extend_from_slice(&chunk[..n]),
                    Err(_) => continue,
                }
            }
            let request = yaml::load(&request).unwrap_or(Value::Null);
            let operation = request["OP"].as_str().unwrap_or("").to_owned();
            let mut d = daemon.lock().unwrap();
            d.calls.push(operation.clone());
            let config = request.get("CFG").cloned().unwrap_or(Value::Null);
            match operation.as_str() {
                "CFG_SET" => for (key, value) in config.as_object().unwrap() {
                    match value.as_object() {
                        Some(map) => for (k, v) in map {
                            d.data["CFG"][key.as_str()][k.as_str()] = v.clone();
                        },
                        None => d.data["CFG"][key.as_str()] = value.clone(),
                    }
                },
                "CFG_DEL" => for (key, values) in config.as_object().unwrap() {
                    for item in values.as_object().unwrap().keys() {
                        if let Some(map) = d.data["CFG"].get_mut(key).and_then(Value::as_object_mut) {
                            map.shift_remove(item);
                        }
                    }
                },
                "CFG_WRITE" => d.saved = Some(d.data["CFG"].clone()),
                _ => {}
            }
            let mut reply = d.data.clone();
            reply["DONE"] = true.into();
            reply["RC"] = 0.into();
            // JSON is valid YAML: exercises the real loader.
            let _ = stream.write_all(reply.to_string().as_bytes());
        }
    });
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

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join("log").join(name)).unwrap_or_default()
    }

    fn raw(&self) -> i64 {
        let text = std::fs::read_to_string(self.root.join("vcp/12/10")).unwrap();
        text.split_whitespace().next().unwrap().parse().unwrap()
    }

    fn setvcp(&self) -> usize {
        self.log("ddcutil").lines().filter(|l| l.contains("setvcp")).count()
    }

    fn swayidle(&self) -> (u32, Vec<String>) {
        let pids = self.log("swayidle.pids");
        let pid: u32 = pids.lines().last().expect("swayidle spawned").trim().parse().unwrap();
        (pid, self.log(&format!("swayidle.{pid}")).lines().map(String::from).collect())
    }

    fn journal(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.root.join("state/brightness-manager/restore.json")).unwrap()).unwrap()
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| !s.contains(") Z ")).unwrap_or(false)
}

struct Controls(gio::DBusConnection);

impl Controls {
    fn call(&self, method: &str, params: Option<Variant>) -> Result<Variant, String> {
        let result: Rc<RefCell<Option<Result<Variant, glib::Error>>>> = Rc::default();
        let r = result.clone();
        self.0.call(Some(BUS_NAME), PATH, INTERFACE, method, params.as_ref(), None, gio::DBusCallFlags::NO_AUTO_START, 5000,
                    None::<&gio::Cancellable>, move |reply| { r.replace(Some(reply)); });
        wait_for(method, Duration::from_secs(6), || result.borrow().is_some());
        let reply = result.take().unwrap();
        reply.map_err(|mut e| {
            gio::DBusError::strip_remote_error(&mut e);
            e.message().to_owned()
        })
    }

    fn state(&self) -> Value {
        let reply = self.call("GetState", None).unwrap();
        serde_json::from_str(reply.child_value(0).str().unwrap()).unwrap()
    }

    fn ok(&self, method: &str, params: Variant) -> bool {
        self.call(method, Some(params)).unwrap().child_value(0).get::<bool>().unwrap()
    }
}

fn rss(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default().lines()
        .find(|l| l.starts_with("VmRSS")).unwrap_or("").to_owned()
}

fn main() {
    let display = std::env::var("BRIGHTNESS_ISOLATED_DISPLAY").ok().as_deref() == Some("1");
    let wayland = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    // gio::TestDBus::up() unsets XDG_RUNTIME_DIR: resolve the socket first.
    let wayland_socket = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_default()).join(&wayland);
    if display {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        assert!(!runtime.starts_with("/run/user/1000") || runtime.contains("/cbt/"), "refusing the live session");
    }
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("bm-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["bin", "log", "config", "state", "cache", "data", "rt", "sys/drm", "sys/backlight", "vcp/12", "home"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::set_permissions(root.join("rt"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (r, l) = (root.display().to_string(), root.join("log").display().to_string());
    // Fake sysfs: one DP monitor with a local AUX adapter (bus 12).
    let connector = root.join("sys/drm/card1-DP-TEST");
    std::fs::create_dir_all(connector.join("i2c-12")).unwrap();
    std::fs::write(connector.join("status"), "connected\n").unwrap();
    std::fs::write(connector.join("edid"), edid(5)).unwrap();
    std::fs::write(root.join("vcp/12/10"), "75 100\n").unwrap();
    std::fs::write(root.join("vcp/12/12"), "50 100\n").unwrap();
    std::fs::write(root.join("outputs.json"), json!([{"name": "DP-TEST", "description": "Synthetic display", "enabled": true,
        "position": {"x": 0, "y": 0}, "scale": 1.0, "modes": [{"width": 1920, "height": 1080, "current": true}]}]).to_string()).unwrap();
    script(&root.join("bin/wlr-randr"), &format!("echo \"$*\" >> {l}/wlr-randr\nexec /usr/bin/cat {r}/outputs.json"));
    script(&root.join("bin/ddcutil"), &format!(r#"echo "$*" >> {l}/ddcutil
bus=""; op=""
while [ $# -gt 0 ]; do case "$1" in --bus) bus=$2; shift;; getvcp|setvcp) op=$1; shift; break;; esac; shift; done
dir={r}/vcp/$bus
if [ "$op" = getvcp ]; then
  [ -d "$dir" ] || {{ echo "DDC address 0x37 did not respond" >&2; exit 1; }}
  for code in "$@"; do [ "$code" = --terse ] && continue
    if [ -f "$dir/$code" ]; then read raw max < "$dir/$code"; echo "VCP $code C $raw $max"; else echo "VCP $code ERR"; fi
  done
elif [ "$op" = setvcp ]; then read raw max < "$dir/$1"; echo "$2 $max" > "$dir/$1"; fi"#));
    script(&root.join("bin/swayidle"), &format!("printf '%s\\n' \"$@\" > {l}/swayidle.$$\necho $$ >> {l}/swayidle.pids\nexec /usr/bin/sleep 300"));
    for fake in ["wlr-randr", "ddcutil", "swayidle"] {
        assert!(root.join("bin").join(fake).exists(), "fake {fake} must shadow the real tool");
    }

    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    let logind: Rc<RefCell<Logind>> = Rc::default();
    let _logind_connection = serve_logind(&address, &logind);
    let daemon = Arc::new(Mutex::new(Daemon {
        data: json!({"CFG": {"AUTO_SCALE": true, "SCALE": {"Disconnected": 2.0}},
                     "STATE": {"HEADS": [{"NAME": "DP-TEST", "DESCRIPTION": "Synthetic display", "MAKE": "Example", "MODEL": "Display",
                                          "SERIAL_NUMBER": "test-monitor", "CURRENT": {"ENABLED": true, "SCALE": 1.0}}]}}),
        calls: Vec::new(), saved: None,
    }));
    serve_way_displays(&root.join("rt/way-displays.sock"), daemon.clone());
    let mut vars: Vec<(String, String)> = vec![
        ("HOME".into(), root.join("home").display().to_string()),
        ("PATH".into(), root.join("bin").display().to_string()),
        ("XDG_CONFIG_HOME".into(), root.join("config").display().to_string()),
        ("XDG_STATE_HOME".into(), root.join("state").display().to_string()),
        ("XDG_CACHE_HOME".into(), root.join("cache").display().to_string()),
        ("XDG_DATA_HOME".into(), root.join("data").display().to_string()),
        ("XDG_DATA_DIRS".into(), root.join("data").display().to_string()),
        ("XDG_RUNTIME_DIR".into(), root.join("rt").display().to_string()),
        ("DBUS_SESSION_BUS_ADDRESS".into(), address.clone()),
        ("DBUS_SYSTEM_BUS_ADDRESS".into(), address.clone()),
        ("BRIGHTNESS_MANAGER_SYSFS".into(), root.join("sys").display().to_string()),
        ("GSETTINGS_BACKEND".into(), "memory".into()),
        ("GDK_DEBUG".into(), "no-portals".into()), ("GIO_USE_VFS".into(), "local".into()), ("GTK_USE_PORTAL".into(), "0".into()),
        ("GTK_A11Y".into(), "none".into()),
        ("NO_AT_BRIDGE".into(), "1".into()),
        ("GSK_RENDERER".into(), "cairo".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ];
    if display {
        // Only the private nested compositor of run_headless.sh (absolute socket path).
        vars.push(("WAYLAND_DISPLAY".into(), wayland_socket.display().to_string()));
    }
    let env = Env { root: root.clone(), vars };

    // Launch-only flags never instantiate an app when the owner is absent.
    for args in [&["--check"][..], &["--idle-dim", "old-token"], &["--idle-restore", "old-token"], &["--brightness-up"],
                 &["--brightness-down"]] {
        let (code, _, stderr) = env.run(args);
        assert_ne!(code, 0, "absent primary was launched by {args:?}: {stderr}");
    }
    assert_eq!(env.log("ddcutil"), "", "launchers never touch hardware");
    assert!(daemon.lock().unwrap().calls.is_empty());
    let (code, _, stderr) = env.run(&["--popup-offset", "-1"]);
    assert_eq!(code, 2);
    assert!(stderr.ends_with("error: Popup offset must be nonnegative.\n"), "{stderr}");

    let started = Instant::now();
    let mut manager: Child = env.command(MANAGER, &["--background"]).spawn().unwrap();
    let mut report = String::new();
    wait_for("--check ready", Duration::from_secs(10), || {
        let (code, stdout, _) = env.run(&["--check"]);
        report = stdout;
        code == 0
    });
    println!("time-to-ready (first passing --check) {:?}", started.elapsed());
    let health: Value = serde_json::from_str(report.trim()).unwrap();
    let keys: Vec<&String> = health.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["ready", "busy", "dimmed", "settings", "error", "displays", "scaling"]);
    assert_eq!(health["settings"], json!({"auto_dim": false, "dim_after": 120, "dim_percent": 30}));

    let client = gio::DBusConnection::for_address_sync(&address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>)
        .unwrap();
    let changed: Rc<RefCell<Vec<Value>>> = Rc::default();
    let c = changed.clone();
    client.signal_subscribe(Some(BUS_NAME), Some(INTERFACE), Some("Changed"), Some(PATH), None, gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, params| {
            if let Some((encoded,)) = params.get::<(String,)>() {
                c.borrow_mut().push(serde_json::from_str(&encoded).unwrap());
            }
        });
    let controls = Controls(client);
    wait_for("hardware probe", Duration::from_secs(5), || {
        controls.state()["displays"][0]["brightness"]["supported"] == true && controls.state()["scaling"]["ready"] == true
    });
    let state = controls.state();
    let shown = state["displays"][0].clone();
    let keys: Vec<&String> = shown.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["connector", "name", "id", "enabled", "method", "error", "geometry", "brightness", "contrast", "scale_id", "scale"]);
    assert_eq!(shown["method"], "ddc");
    assert_eq!(shown["geometry"], json!([0, 0, 1920, 1080]));
    assert_eq!(shown["brightness"], json!({"supported": true, "value": 75.0, "raw": 75, "max": 100, "busy": false, "error": ""}));
    let id = shown["id"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 64);
    assert_eq!(env.raw(), 75);
    assert_eq!(env.setvcp(), 0, "Startup must not adjust brightness");
    assert!(daemon.lock().unwrap().calls.iter().all(|c| c == "GET"), "Startup must not change display configuration");
    for _ in 0..2 {
        assert_eq!(env.run(&["--check"]).0, 0);
        let (code, _, stderr) = env.run(&["--background"]);
        assert_eq!(code, 0, "{stderr}");
    }
    assert_eq!(env.log("wlr-randr").lines().count() >= 1, true);

    // Indicator stream: one JSON object per change, never touching hardware.
    let mut status = env.command(STATUS, &[]).spawn().unwrap();
    let status_out = status.stdout.take().unwrap();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = lines.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(status_out).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    wait_for("status line", Duration::from_secs(5), || lines.lock().unwrap().iter().any(|l| l.contains("\"manual\"")));
    let first: Value = serde_json::from_str(&lines.lock().unwrap()[0]).unwrap();
    assert_eq!(first["class"], "unavailable");
    let manual: Value = serde_json::from_str(lines.lock().unwrap().iter().find(|l| l.contains("\"manual\"")).unwrap()).unwrap();
    assert_eq!(manual["tooltip"], "Displays\nDP-TEST: Brightness: 75% · Scale: 100%\nAuto-dim: Off\nClick for display controls · Right-click for display settings");

    // Scaling through the shared IPC, persisted with CFG_WRITE.
    let scale_id = shown["scale_id"].as_str().unwrap().to_owned();
    assert!(controls.ok("SetScale", ("DP-TEST", 1.5f64, scale_id.as_str()).to_variant()));
    wait_for("SetScale", Duration::from_secs(5), || {
        let s = controls.state();
        s["displays"][0]["scale"]["value"] == json!(1.5) && s["scaling"]["busy"] == false
    });
    assert_eq!(daemon.lock().unwrap().saved.as_ref().unwrap()["SCALE"]["Synthetic display"], json!(1.5));
    assert!(changed.borrow().iter().any(|s| s["displays"][0]["scale"]["value"] == json!(1.5)), "Changed signal");
    assert!(controls.ok("SetAutoScale", (false,).to_variant()));
    wait_for("SetAutoScale", Duration::from_secs(5), || {
        let s = controls.state();
        s["scaling"]["auto_scale"] == false && s["scaling"]["busy"] == false
    });
    assert!(daemon.lock().unwrap().saved.as_ref().unwrap()["SCALE"].get("Synthetic display").is_none());
    let saved = daemon.lock().unwrap().saved.clone();
    assert!(!controls.ok("SetScale", ("DP-TEST", 2.0f64, "removed-display").to_variant()));
    assert!(controls.state()["scaling"]["error"].as_str().unwrap().contains("no longer"));
    assert_eq!(daemon.lock().unwrap().saved, saved);
    assert_eq!(env.setvcp(), 0, "Scaling must not write brightness");

    if display {
        // Keyboard steps through the OSD; never opens windows.
        let (code, _, stderr) = env.run(&["--brightness-up"]);
        assert_eq!(code, 0, "{stderr}");
        wait_for("brightness up", Duration::from_secs(5), || env.raw() == 85);
        assert_eq!(env.run(&["--brightness-down"]).0, 0);
        wait_for("brightness down", Duration::from_secs(5), || env.raw() == 75);
        // --settings: way-displays-gui.desktop is not installed in this private XDG_DATA_HOME.
        assert_eq!(env.run(&["--settings"]).0, 0);
        wait_for("settings error", Duration::from_secs(5), || {
            controls.state()["error"].as_str().unwrap_or("").starts_with("Could not open display settings: ")
        });
    }

    // Shared SetValue and stale-identity rejection.
    assert!(controls.ok("SetValue", ("DP-TEST", "brightness", 61.0f64, id.as_str()).to_variant()));
    wait_for("SetValue", Duration::from_secs(5), || env.raw() == 61 && controls.state()["displays"][0]["brightness"]["raw"] == 61);
    let writes = env.setvcp();
    let error = controls.call("SetValue", Some(("DP-TEST", "brightness", 10.0f64, "removed-monitor").to_variant())).unwrap_err();
    assert_eq!(error, "This display is no longer connected.");
    assert_eq!(env.setvcp(), writes);
    let error = controls.call("SetSettings", Some(("not json",).to_variant())).unwrap_err();
    assert!(error.starts_with("expected"), "{error}");
    assert!(!controls.ok("SetSettings", (r#"{"auto_dim": true}"#,).to_variant()));

    // Auto-dim: one swayidle listener calling back with a policy token.
    assert!(controls.ok("SetAutoDim", (true,).to_variant()));
    wait_for("swayidle", Duration::from_secs(5), || !env.log("swayidle.pids").is_empty());
    wait_for("auto_dim setting", Duration::from_secs(5), || controls.state()["settings"]["auto_dim"] == true);
    let (pid, argv) = env.swayidle();
    assert_eq!(argv[..5], ["-w", "-C", "/dev/null", "timeout", "120"]);
    let dim = suite_core::shell::shell_split(&argv[5]);
    assert_eq!(dim[0], MANAGER);
    assert_eq!(dim[1], "--idle-dim");
    let token = dim[2].clone();
    assert_eq!(token.len(), 32);
    assert!(argv[7].ends_with(" </dev/null >/dev/null 2>&1 &"));
    assert!(lines.lock().unwrap().iter().any(|l| l.contains("\"auto\"") && l.contains("Auto-dim: On")));

    // Activity while the inhibitor check is pending cancels the dim.
    logind.borrow_mut().hold = true;
    assert_eq!(env.run(&["--idle-dim", &token]).0, 0);
    wait_for("held inhibitor check", Duration::from_secs(5), || logind.borrow().held.len() == 1);
    assert_eq!(env.run(&["--idle-restore", &token]).0, 0);
    let held = logind.borrow_mut().held.pop().unwrap();
    held.return_value(Some(&inhibitors(false)));
    logind.borrow_mut().hold = false;
    pump(Duration::from_millis(400));
    assert_eq!((env.raw(), env.setvcp()), (61, writes), "Activity must cancel pending inhibitor checks");

    assert_eq!(env.run(&["--idle-dim", "stale-policy"]).0, 0);
    pump(Duration::from_millis(300));
    assert_eq!(env.setvcp(), writes);
    logind.borrow_mut().inhibited = true;
    assert_eq!(env.run(&["--idle-dim", &token]).0, 0);
    pump(Duration::from_millis(400));
    assert_eq!(env.setvcp(), writes, "Inhibition must block automatic dimming");
    logind.borrow_mut().inhibited = false;
    assert_eq!(env.run(&["--idle-dim", &token]).0, 0);
    wait_for("synthetic dim", Duration::from_secs(5), || env.raw() == 30);
    wait_for("journal", Duration::from_secs(5), || env.journal().get(&id).is_some());
    assert_eq!(env.journal()[&id], json!({"connector": "DP-TEST", "raw": 61, "target": 30, "max": 100}));
    assert_eq!(controls.state()["dimmed"], true);
    assert!(logind.borrow().calls.iter().any(|c| c == "GetSession('auto',)"));
    assert_eq!(env.run(&["--idle-restore", &token]).0, 0);
    wait_for("exact brightness restoration", Duration::from_secs(5), || env.raw() == 61 && env.journal() == json!({}));

    assert_eq!(env.run(&["--idle-dim", &token]).0, 0);
    wait_for("second dim", Duration::from_secs(5), || env.raw() == 30);
    assert!(controls.ok("SetAutoDim", (false,).to_variant()));
    wait_for("disable restoration", Duration::from_secs(5), || env.raw() == 61);
    wait_for("listener stopped", Duration::from_secs(5), || !alive(pid));
    assert_eq!(env.run(&["--idle-dim", &token]).0, 0);
    pump(Duration::from_millis(300));
    assert_eq!(env.raw(), 61, "revoked token");
    assert!(controls.ok("SetSettings", (r#"{"auto_dim": true, "dim_after": 60, "dim_percent": 20}"#,).to_variant()));
    wait_for("SetSettings", Duration::from_secs(5), || env.swayidle().0 != pid);
    let (pid, argv) = env.swayidle();
    assert_eq!(argv[4], "60");
    let token = suite_core::shell::shell_split(&argv[5])[2].clone();
    assert_eq!(std::fs::read_to_string(root.join("config/brightness-manager/settings.json")).unwrap(),
               "{\n  \"auto_dim\": true,\n  \"dim_after\": 60,\n  \"dim_percent\": 20\n}\n");
    assert_eq!(env.run(&["--idle-dim", &token]).0, 0);
    wait_for("dim before orderly shutdown", Duration::from_secs(5), || env.raw() == 20);

    println!("idle RSS of brightness-manager: {}", rss(manager.id()));
    println!("idle RSS of brightness-status: {}", rss(status.id()));
    // SIGTERM: restore the exact brightness, empty the journal, stop the listener.
    unsafe { libc::kill(manager.id() as i32, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(status) = manager.try_wait().unwrap() {
            break status.code();
        }
        assert!(Instant::now() < end, "manager did not stop");
        pump(Duration::from_millis(20));
    };
    assert_eq!(code, Some(0));
    assert_eq!(env.raw(), 61, "Orderly shutdown must restore exact previous brightness");
    assert_eq!(env.journal(), json!({}));
    wait_for("listener stopped on shutdown", Duration::from_secs(5), || !alive(pid));
    wait_for("status stream sees the service leave", Duration::from_secs(5), || {
        lines.lock().unwrap().last().is_some_and(|l| l.contains("\"unavailable\""))
    });
    unsafe { libc::kill(status.id() as i32, libc::SIGTERM) };
    assert!(status.wait().unwrap().success());
    let ddc = env.log("ddcutil");
    assert!(ddc.lines().all(|l| l.starts_with("--noconfig --bus 12 ")), "{ddc}");
    bus.down();
    let _ = std::fs::remove_dir_all(&root);
    println!("brightness-manager private-bus integration passed{}", if display { " (with display: keys + settings)" } else { "" });
}
