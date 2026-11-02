//! End-to-end CLI tests against a FAKE compositor control socket in a
//! scratch XDG_RUNTIME_DIR. Nothing here talks to the live session: the
//! socket, rc.xml, preferences and `labwc` on PATH are all scratch copies,
//! and the "compositor" PID is a renamed `sleep`.

use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_tessel-tiling");
const STATUS_BIN: &str = env!("CARGO_BIN_EXE_tessel-tiling-status");

struct Compositor {
    enabled: bool,
    outline: bool,
    error: String,
    pid: u32,
    commands: Vec<String>,
}

struct Env {
    root: PathBuf,
    state: Arc<Mutex<Compositor>>,
    stop: Arc<Mutex<bool>>,
    sleeper: Option<Child>,
}

fn reply(state: &mut Compositor, command: &str) -> Value {
    state.commands.push(command.into());
    match command {
        "enable" => state.enabled = true,
        "disable" => state.enabled = false,
        "outline on" => state.outline = true,
        "outline off" => state.outline = false,
        _ => {}
    }
    json!({"version": 1, "ready": true, "compositor_pid": state.pid, "enabled": state.enabled,
           "parking_supported": true, "tile_bounds_supported": true, "outline_supported": true,
           "outline_enabled": state.outline, "error": state.error, "windows": [],
           "capture_output_supported": true, "capture_output": null})
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("tessel-tiling-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["rt", "home", "config", "state", "cache", "bin", "fakelabwc"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("rt"), std::fs::Permissions::from_mode(0o700)).unwrap();
        // `labwc --reconfigure` on PATH only records its call; it never signals anything.
        let script = root.join("bin/labwc");
        std::fs::write(&script, format!("#!/bin/sh\necho \"$* LABWC_PID=$LABWC_PID\" >> '{}'\n",
                                        root.join("reconfigure.log").display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A process whose executable is named labwc, standing in for the compositor PID.
        std::fs::copy("/usr/bin/sleep", root.join("fakelabwc/labwc")).unwrap();
        // Parallel tests fork while the copy is open for writing: retry ETXTBSY.
        let mut attempts = 0;
        let sleeper = loop {
            match Command::new(root.join("fakelabwc/labwc")).arg("120").spawn() {
                Ok(child) => break child,
                Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && attempts < 100 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("{error}"),
            }
        };
        let state = Arc::new(Mutex::new(Compositor { enabled: false, outline: true, error: String::new(), pid: sleeper.id(),
                                                     commands: Vec::new() }));
        Self { root, state, stop: Arc::new(Mutex::new(false)), sleeper: Some(sleeper) }
    }

    fn socket(&self) -> PathBuf { self.root.join("rt/tessel-tiling-test-0.sock") }

    fn serve(&self) {
        let server = UnixDatagram::bind(self.socket()).unwrap();
        server.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let (state, stop) = (self.state.clone(), self.stop.clone());
        std::thread::spawn(move || {
            let mut buffer = [0u8; 512];
            while !*stop.lock().unwrap() {
                let Ok((length, peer)) = server.recv_from(&mut buffer) else { continue };
                let command = String::from_utf8_lossy(&buffer[..length]).into_owned();
                let answer = reply(&mut state.lock().unwrap(), &command);
                if let Some(path) = peer.as_pathname() {
                    let _ = server.send_to(answer.to_string().as_bytes(), path);
                }
            }
        });
    }

    fn command(&self, binary: &str, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command.args(args).env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.root.join("bin").display()))
            .env("HOME", self.root.join("home"))
            .env("XDG_RUNTIME_DIR", self.root.join("rt"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_CONFIG_DIRS", self.root.join("nonexistent"))
            .env("WAYLAND_DISPLAY", "test-0")
            .env("DESKTOP_SUITE_LOCALE_DIR", "/nonexistent")
            .env("LANG", "C.UTF-8");
        command
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = self.command(BIN, args).output().unwrap();
        (output.status.code().unwrap_or(-1), String::from_utf8(output.stdout).unwrap(), String::from_utf8(output.stderr).unwrap())
    }

    fn status(&self) -> Map<String, Value> {
        let (code, out, _) = self.run(&["--status"]);
        assert_eq!(code, 0);
        assert_eq!(out.lines().count(), 1);
        serde_json::from_str::<Value>(&out).unwrap().as_object().unwrap().clone()
    }

    fn rc(&self) -> PathBuf { self.root.join("config/labwc/rc.xml") }

    fn commands(&self) -> Vec<String> { std::mem::take(&mut self.state.lock().unwrap().commands) }
}

impl Drop for Env {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
        if let Some(mut sleeper) = self.sleeper.take() {
            let _ = sleeper.kill();
            let _ = sleeper.wait();
        }
        std::thread::sleep(Duration::from_millis(60));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn status_reports_the_compositor_and_every_failure_kind() {
    let env = Env::new("status");
    let state = env.status();
    assert_eq!(state["ready"], false);
    assert_eq!(state["class"], "unavailable");
    assert_eq!(state["error"], "Sign out and back in to load the labwc tiling update.");
    assert_eq!(state["sign_out_required"], true);
    assert_eq!(state["tooltip"], "Window Tiling\nSign out and back in to load the labwc tiling update.");
    let (_, raw, _) = env.run(&["--status"]);
    assert_eq!(raw, "{\"ready\": false, \"enabled\": false, \"text\": \"\\u25a6\", \"class\": \"unavailable\", \"error\": \"Sign out and back in to load the labwc tiling update.\", \"tooltip\": \"Window Tiling\\nSign out and back in to load the labwc tiling update.\", \"sign_out_required\": true}\n");
    // A non-socket at the path.
    std::fs::write(env.socket(), b"").unwrap();
    assert_eq!(env.status()["error"], "The tiling control socket is not owned by this user.");
    std::fs::remove_file(env.socket()).unwrap();
    // Nobody listening on a real socket path: connect raises OSError (not Unavailable).
    drop(UnixDatagram::bind(env.socket()).unwrap());
    let state = env.status();
    assert_eq!(state["error"], "[Errno 111] Connection refused");
    assert_eq!(state["sign_out_required"], false);
    std::fs::remove_file(env.socket()).unwrap();
    env.serve();
    let state = env.status();
    assert_eq!(state["ready"], true);
    assert_eq!(state["class"], "inactive");
    assert_eq!(state["text"], "▦");
    assert_eq!(state["tooltip"], "Window Tiling: Off\nClick to toggle · Right-click for shortcuts");
    assert_eq!(state["sign_out_required"], false);
    let keys: Vec<&String> = state.keys().collect();
    assert_eq!(keys, ["version", "ready", "compositor_pid", "enabled", "parking_supported", "tile_bounds_supported",
                      "outline_supported", "outline_enabled", "error", "windows", "capture_output_supported",
                      "capture_output", "text", "class", "tooltip", "sign_out_required"]);
    env.state.lock().unwrap().error = "The session is locked.".into();
    let state = env.status();
    assert_eq!((state["ready"].clone(), state["error"].clone()), (json!(false), json!("The session is locked.")));
    // Invalid display names never reach a socket.
    let output = env.command(BIN, &["--status"]).env("WAYLAND_DISPLAY", "../x").output().unwrap();
    let state: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["error"], "Tiling requires a labwc Wayland session.");
    // Reply directories are cleaned up.
    let leftovers = std::fs::read_dir(env.root.join("rt")).unwrap().filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("wt-")).count();
    assert_eq!(leftovers, 0);
}

#[test]
fn enable_disable_toggle_and_restore_edit_a_scratch_rc_xml() {
    let env = Env::new("enable");
    let original = b"<?xml version=\"1.0\"?>\n<labwc_config>\n  <keyboard>\n    <default/>\n  </keyboard>\n</labwc_config>\n";
    std::fs::create_dir_all(env.rc().parent().unwrap()).unwrap();
    std::fs::write(env.rc(), original).unwrap();
    // Not ready: error on stderr, exit 1, nothing touched.
    let (code, _, err) = env.run(&["--toggle"]);
    assert_eq!((code, err.as_str()), (1, "Sign out and back in to load the labwc tiling update.\n"));
    env.serve();
    let (code, _, err) = env.run(&["--enable"]);
    assert_eq!(code, 0, "{err}");
    let on = String::from_utf8(std::fs::read(env.rc()).unwrap()).unwrap();
    assert!(on.contains("<!-- Tessel Tiling keyboard begin -->"));
    let home = env.root.join("home");
    assert!(on.contains(&format!("command=\"{}/.local/bin/tessel-tiling --settings\"", home.display())));
    let commands = env.commands();
    assert_eq!(&commands[..3], ["status", "status", "enable"]);
    let pid = env.state.lock().unwrap().pid;
    assert_eq!(std::fs::read_to_string(env.root.join("reconfigure.log")).unwrap(), format!("--reconfigure LABWC_PID={pid}\n"));
    let prefs = env.root.join("config/tessel-tiling/config.json");
    assert_eq!(std::fs::read_to_string(&prefs).unwrap(), "{\"enabled\": true}\n");
    assert!(std::fs::read_dir(env.root.join("state/tessel-tiling")).unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().starts_with("rc-")));
    let (code, _, _) = env.run(&["--toggle"]);
    assert_eq!(code, 0);
    assert_eq!(std::fs::read(env.rc()).unwrap(), original, "disabling restores rc.xml byte for byte");
    assert_eq!(std::fs::read_to_string(&prefs).unwrap(), "{\"enabled\": false}\n");
    env.commands();
    // --restore: nothing while the preference is off.
    assert_eq!(env.run(&["--restore"]).0, 0);
    assert!(env.commands().is_empty());
    std::fs::write(&prefs, "{\"enabled\": true, \"focus_outline\": false}").unwrap();
    assert_eq!(env.run(&["--restore"]).0, 0);
    let commands = env.commands();
    assert!(commands.contains(&"enable".to_owned()) && commands.contains(&"outline off".to_owned()), "{commands:?}");
    assert_eq!(std::fs::read_to_string(&prefs).unwrap(), "{\"enabled\": true, \"focus_outline\": false}",
               "restore never rewrites the preference");
    assert!(env.state.lock().unwrap().enabled);
    assert_eq!(env.run(&["--disable"]).0, 0);
    assert_eq!(std::fs::read(env.rc()).unwrap(), original);
}

#[test]
fn watch_prints_only_changes_and_ends_on_sigint() {
    let env = Env::new("watch");
    env.serve();
    for binary in [BIN, STATUS_BIN] {
        let args: &[&str] = if binary == BIN { &["--watch"] } else { &[] };
        let mut child = env.command(binary, args).stdout(Stdio::piped()).spawn().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let first: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(first["class"], "inactive");
        std::thread::sleep(Duration::from_millis(1200));
        let started = Instant::now();
        env.state.lock().unwrap().enabled = true;
        let second: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(second["class"], "active");
        assert!(started.elapsed() < Duration::from_secs(3));
        unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
        assert_eq!(child.wait().unwrap().code(), Some(0));
        env.state.lock().unwrap().enabled = false;
        // Without a palette, polls are status requests only.
        assert!(env.commands().iter().all(|c| c == "status"));
    }
    // Focus outline sync from the palette: colours once, then idle.
    std::fs::create_dir_all(env.root.join("cache/tessel-dock")).unwrap();
    std::fs::write(env.root.join("cache/tessel-dock/palette.json"), r#"{"monitors":[{"name":"HEADLESS-1","accent":[1,2,3]}]}"#).unwrap();
    let mut child = env.command(BIN, &["--watch"]).stdout(Stdio::piped()).spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    lines.next().unwrap().unwrap();
    std::thread::sleep(Duration::from_millis(2300));
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    child.wait().unwrap();
    let commands = env.commands();
    assert_eq!(commands.iter().filter(|c| c.starts_with("outline-color")).count(), 1, "{commands:?}");
    assert!(commands.contains(&"outline-color HEADLESS-1 1 2 3".to_owned()));
}

#[test]
fn argument_errors_match_argparse() {
    let env = Env::new("args");
    let (code, out, _) = env.run(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("usage: tessel-tiling [-h] [--toggle | --enable | --disable | --restore | --status |\n"), "{out}");
    let (code, _, err) = env.run(&["--status", "--toggle"]);
    assert_eq!(code, 2);
    assert!(err.ends_with("tessel-tiling: error: argument --toggle: not allowed with argument --status\n"));
    let (code, _, err) = env.run(&["--bogus"]);
    assert_eq!(code, 2);
    assert!(err.ends_with("error: unrecognized arguments: --bogus\n"));
}

#[test]
fn python_shim_argv_keeps_script_names() {
    use std::os::unix::process::CommandExt;
    let env = Env::new("shim");
    env.serve();
    let output = env.command(BIN, &["/x/tiling_manager.py", "--bogus"]).arg0("/usr/bin/python3").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.starts_with("usage: tiling_manager.py [-h]"), "{err}");
    assert!(err.ends_with("tiling_manager.py: error: unrecognized arguments: --bogus\n"));
    let mut child = env.command(BIN, &["/x/tiling_status.py"]).arg0("/usr/bin/python3").stdout(Stdio::piped()).spawn().unwrap();
    let line = BufReader::new(child.stdout.take().unwrap()).lines().next().unwrap().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["class"], "inactive");
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    assert_eq!(child.wait().unwrap().code(), Some(0));
}
