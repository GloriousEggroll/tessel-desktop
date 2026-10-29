//! test_loopback_integration.py: loopbacks in a *private* PipeWire graph that
//! contains only dummy sinks. Every process gets a private HOME, runtime,
//! configuration, state and D-Bus bus, and `PIPEWIRE_REMOTE` names the private
//! daemon, so no command can reach the desktop's PipeWire or session bus.
//! Skips (exit 0) when pipewire/wireplumber/pw-cli/wpctl/dbus-daemon are missing.

use audio_manager::api::AudioApi;
use audio_manager::backend::{AudioBackend, BackendOptions};
use audio_manager::model::Loopback;
use std::cell::Cell;
use std::rc::Rc;
use audio_manager::store::LoopbackStore;
use audio_manager::supervisor::loopback_argv;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const REMOTE: &str = "audio-manager-loopback-test";

const PIPEWIRE_CONFIG: &str = r#"
context.properties = {
    core.daemon = true
    core.name = audio-manager-loopback-test
    support.dbus = false
}
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-spa-node-factory }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-client-device }
    { name = libpipewire-module-access }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-link-factory }
    { name = libpipewire-module-session-manager }
]
context.objects = [
    { factory = spa-node-factory
      args = {
          factory.name = support.node.driver
          node.name = Dummy-Driver
          node.group = pipewire.dummy
          priority.driver = 20000
      }
    }
]
"#;

struct Isolated {
    dir: PathBuf,
    env: Vec<(String, String)>,
    processes: Vec<(String, Child)>,
}

fn which(name: &str) -> bool {
    suite_core::shell::which(name).is_some()
}

impl Isolated {
    fn new() -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("am-pw-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in ["", "home", "config", "data", "cache", "state"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let d = dir.display().to_string();
        let env = vec![
            ("PATH".into(), std::env::var("PATH").unwrap()),
            ("HOME".into(), format!("{d}/home")),
            ("PIPEWIRE_RUNTIME_DIR".into(), d.clone()),
            ("PIPEWIRE_REMOTE".into(), REMOTE.into()),
            ("PIPEWIRE_CONFIG_DIR".into(), "/usr/share/pipewire".into()),
            ("WIREPLUMBER_CONFIG_DIR".into(), "/usr/share/wireplumber".into()),
            ("WIREPLUMBER_DATA_DIR".into(), "/usr/share/wireplumber".into()),
            ("XDG_RUNTIME_DIR".into(), d.clone()),
            ("XDG_CONFIG_HOME".into(), format!("{d}/config")),
            ("XDG_DATA_HOME".into(), format!("{d}/data")),
            ("XDG_CACHE_HOME".into(), format!("{d}/cache")),
            ("XDG_STATE_HOME".into(), format!("{d}/state")),
            ("DBUS_SESSION_BUS_ADDRESS".into(), format!("unix:path={d}/bus")),
            ("DBUS_SYSTEM_BUS_ADDRESS".into(), format!("unix:path={d}/no-system-bus")),
            ("PULSE_SERVER".into(), format!("unix:{d}/no-pulse-server")),
            ("WAYLAND_DISPLAY".into(), "no-test-display".into()),
            ("GDK_DEBUG".into(), "no-portals".into()), ("GIO_USE_VFS".into(), "local".into()), ("GTK_USE_PORTAL".into(), "0".into()),
            ("TMPDIR".into(), d.clone()),
        ];
        let mut graph = Self { dir, env, processes: Vec::new() };
        fs::write(graph.dir.join("bus.conf"), format!(
            "<busconfig><type>session</type><listen>unix:path={d}/bus</listen><policy context=\"default\"><allow own=\"*\"/>\
             <allow send_destination=\"*\"/><allow receive_sender=\"*\"/></policy></busconfig>")).unwrap();
        graph.spawn(&["dbus-daemon", "--nofork", "--nopidfile", &format!("--config-file={d}/bus.conf")], "bus");
        graph.wait_for(|g| g.dir.join("bus").exists(), "private D-Bus", 8.0);
        fs::write(graph.dir.join("isolated.conf"), PIPEWIRE_CONFIG).unwrap();
        graph.spawn(&["pipewire", "--config", &format!("{d}/isolated.conf")], "pipewire");
        graph.wait_for(|g| g.dir.join(REMOTE).exists(), "private PipeWire", 8.0);
        graph.spawn(&["wireplumber", "--profile", "policy"], "wireplumber");
        graph.wait_for(|g| g.snapshot().iter().any(|o| o["type"].as_str().unwrap_or("").ends_with(":Client")
            && o["info"]["props"].to_string().contains("WirePlumber")), "WirePlumber policy", 8.0);
        graph
    }

    fn command(&self, argv: &[&str]) -> Command {
        let mut command = Command::new(argv[0]);
        command.args(&argv[1..]).env_clear().envs(self.env.iter().map(|(k, v)| (k, v)));
        command
    }

    fn spawn(&mut self, argv: &[&str], label: &str) -> usize {
        let log = fs::File::create(self.dir.join(format!("{label}-{}.log", self.processes.len()))).unwrap();
        let child = self.command(argv).stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap();
        self.processes.push((label.into(), child));
        self.processes.len() - 1
    }

    fn run(&self, argv: &[&str]) -> String {
        let output = self.command(argv).stdin(Stdio::null()).output().unwrap();
        assert!(output.status.success(), "{argv:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn snapshot(&self) -> Vec<Value> {
        let output = self.command(&["pw-dump", "--remote", REMOTE]).stdin(Stdio::null()).stderr(Stdio::null()).output();
        output.ok().and_then(|o| serde_json::from_slice::<Vec<Value>>(&o.stdout).ok()).unwrap_or_default()
    }

    fn nodes(&self) -> HashMap<String, Value> {
        self.snapshot().into_iter().filter(|o| o["type"].as_str().unwrap_or("").ends_with(":Node"))
            .filter_map(|o| Some((o["info"]["props"]["node.name"].as_str()?.to_owned(), o))).collect()
    }

    fn links(&self) -> Vec<(i64, i64)> {
        self.snapshot().iter().filter(|o| o["type"].as_str().unwrap_or("").ends_with(":Link"))
            .map(|o| (o["info"]["output-node-id"].as_i64().unwrap(), o["info"]["input-node-id"].as_i64().unwrap())).collect()
    }

    fn add_sink(&self, name: &str) {
        let props = serde_json::json!({"factory.name": "support.null-audio-sink", "node.name": name, "node.description": name,
            "media.class": "Audio/Sink", "audio.position": ["FL", "FR"], "object.linger": true});
        self.run(&["pw-cli", "create-node", "adapter", &props.to_string()]);
        self.wait_for(|g| g.nodes().contains_key(name), &format!("dummy sink {name}"), 8.0);
    }

    fn remove_node(&self, name: &str) {
        let id = self.nodes()[name]["id"].to_string();
        self.run(&["pw-cli", "destroy", &id]);
        self.wait_for(|g| !g.nodes().contains_key(name), &format!("removing {name}"), 8.0);
    }

    fn wait_for(&self, mut predicate: impl FnMut(&Self) -> bool, description: &str, timeout: f64) {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        while Instant::now() < deadline {
            if predicate(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut logs = String::new();
        for entry in fs::read_dir(&self.dir).unwrap().flatten() {
            if entry.path().extension().is_some_and(|e| e == "log") {
                let text = fs::read_to_string(entry.path()).unwrap_or_default();
                logs += &format!("{}: {}\n", entry.file_name().to_string_lossy(), &text[text.len().saturating_sub(2500)..]);
            }
        }
        panic!("Timed out waiting for {description}\n{logs}");
    }

    fn running(&mut self, index: usize) -> bool {
        self.processes[index].1.try_wait().unwrap().is_none()
    }

    fn terminate(&mut self, index: usize) {
        let child = &mut self.processes[index].1;
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "process did not stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        for (_, child) in self.processes.iter_mut().rev() {
            if child.try_wait().ok().flatten().is_none() {
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let deadline = Instant::now() + Duration::from_secs(3);
                while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn names(entry: &Loopback) -> (String, String) {
    let name = format!("audio_manager.loopback.{}", entry.id);
    (name.clone(), name + ".output")
}

fn assert_no_alternate_link(graph: &Isolated, playback: &str, duration: f64) {
    let deadline = Instant::now() + Duration::from_secs_f64(duration);
    while Instant::now() < deadline {
        let nodes = graph.nodes();
        if let (Some(p), Some(a)) = (nodes.get(playback), nodes.get("test.alternate")) {
            let pair = (p["id"].as_i64().unwrap(), a["id"].as_i64().unwrap());
            assert!(!graph.links().contains(&pair), "loopback fell back to test.alternate");
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn wait_for_target_link(graph: &Isolated, playback: &str) {
    graph.wait_for(|g| {
        let nodes = g.nodes();
        match (nodes.get(playback), nodes.get("test.target")) {
            (Some(p), Some(t)) => g.links().contains(&(p["id"].as_i64().unwrap(), t["id"].as_i64().unwrap())),
            _ => false,
        }
    }, "loopback linked to its selected target", 8.0);
}

fn private_policy_graph() {
    let graph = Isolated::new();
    graph.add_sink("test.target");
    graph.add_sink("test.alternate");
    assert!(!graph.snapshot().iter().any(|o| o["type"].as_str().unwrap_or("").ends_with(":Device")));
    assert!(graph.nodes().contains_key("test.target") && graph.nodes().contains_key("test.alternate"));
}

fn builder_never_falls_back_and_recovers_after_hotplug() {
    let mut graph = Isolated::new();
    graph.add_sink("test.alternate");
    let store = LoopbackStore::new(Some(graph.dir.join("loopbacks.json")));
    let entry = store.add("Test \"loopback\" — $name", "test.target", "Target").unwrap();
    let (virtual_name, playback) = names(&entry);
    let argv = loopback_argv(&entry, "pw-loopback").unwrap();
    let worker = graph.spawn(&argv.iter().map(String::as_str).collect::<Vec<_>>(), "loopback");
    graph.wait_for(|g| { let n = g.nodes(); n.contains_key(&virtual_name) && n.contains_key(&playback) }, "native loopback with temporarily missing target", 8.0);
    assert_no_alternate_link(&graph, &playback, 0.35);
    assert!(graph.running(worker));
    graph.add_sink("test.target");
    wait_for_target_link(&graph, &playback);
    assert_eq!(graph.nodes()[&virtual_name]["info"]["props"]["node.description"], entry.name.as_str());
    let alternate = graph.nodes()["test.alternate"]["id"].to_string();
    graph.run(&["wpctl", "set-default", &alternate]);
    assert_no_alternate_link(&graph, &playback, 0.35);
    wait_for_target_link(&graph, &playback);
    graph.remove_node("test.target");
    assert_no_alternate_link(&graph, &playback, 0.35);
    assert!(graph.running(worker));
    graph.add_sink("test.target");
    wait_for_target_link(&graph, &playback);
}

fn service_restores_saved_loopback_disables_missing_target_and_removes() {
    let mut graph = Isolated::new();
    graph.add_sink("test.alternate");
    let store = LoopbackStore::new(Some(graph.dir.join("loopbacks.json")));
    let entry = store.add("Persistent test output", "test.target", "Target").unwrap();
    let (virtual_name, playback) = names(&entry);
    let binary = env!("CARGO_BIN_EXE_audio-manager-loopbacks");
    let config = store.path.display().to_string();
    let argv = [binary, "--config", config.as_str()];
    let mut service = graph.spawn(&argv, "loopback-service");
    // No worker may be created while only the unrelated default is present.
    assert_no_alternate_link(&graph, &playback, 0.6);
    assert!(graph.running(service));
    assert!(!graph.nodes().contains_key(&virtual_name));
    assert_eq!(store.load().unwrap()[&entry.id], entry);

    graph.add_sink("test.target");
    wait_for_target_link(&graph, &playback);
    let first_serial = graph.nodes()[&virtual_name]["info"]["props"]["object.serial"].clone();
    graph.remove_node("test.target");
    assert_no_alternate_link(&graph, &playback, 0.35);
    graph.wait_for(|g| { let n = g.nodes(); !n.contains_key(&virtual_name) && !n.contains_key(&playback) }, "disabling loopback after target removal", 8.0);
    assert_eq!(store.load().unwrap()[&entry.id], entry);

    graph.add_sink("test.target");
    wait_for_target_link(&graph, &playback);
    assert_ne!(graph.nodes()[&virtual_name]["info"]["props"]["object.serial"], first_serial);
    graph.terminate(service);
    graph.wait_for(|g| !g.nodes().contains_key(&virtual_name), "service shutdown cleanup", 8.0);
    // A second instance for the same configuration exits 0 immediately.
    service = graph.spawn(&argv, "loopback-service-restored");
    wait_for_target_link(&graph, &playback);
    let duplicate = graph.command(&argv).output().unwrap();
    assert!(duplicate.status.success());
    assert_eq!(String::from_utf8_lossy(&duplicate.stderr), "INFO: The loopback service is already running for this configuration.\n");
    assert_eq!(store.load().unwrap()[&entry.id], entry);

    store.remove(&entry.id).unwrap();
    graph.wait_for(|g| { let n = g.nodes(); !n.contains_key(&virtual_name) && !n.contains_key(&playback) }, "removing saved loopback without restarting audio", 8.0);
    assert!(store.load().unwrap().is_empty());
    assert!(graph.running(service));
    let nodes = graph.nodes();
    assert!(nodes.contains_key("test.target") && nodes.contains_key("test.alternate"));
    let log = fs::read_to_string(graph.dir.join(format!("loopback-service-restored-{service}.log"))).unwrap();
    assert!(log.contains("INFO: Enabled loopback Persistent test output → Target"), "{log}");
}

// ----- test_native_monitor.py: the real backend in-process on the private graph ----

const ISOLATED_KEYS: [&str; 7] = ["PIPEWIRE_RUNTIME_DIR", "PIPEWIRE_REMOTE", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS",
                                  "PULSE_SERVER", "XDG_STATE_HOME", "XDG_CONFIG_HOME"];

struct Native {
    graph: Isolated,
    backend: AudioBackend,
    changes: Rc<Cell<u32>>,
}

fn wait_main(mut predicate: impl FnMut() -> bool, description: &str, timeout: f64) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < deadline {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("Timed out waiting for {description}");
}

fn settle_main(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    wait_main(|| Instant::now() >= end, "settling", seconds + 1.0);
}

impl Native {
    fn new() -> Self {
        let graph = Isolated::new();
        for key in ISOLATED_KEYS {
            let value = graph.env.iter().find(|(k, _)| k == key).unwrap().1.clone();
            std::env::set_var(key, value);
        }
        assert_eq!(std::env::var("PIPEWIRE_REMOTE").unwrap(), REMOTE);
        graph.add_sink("test.target");
        let props = serde_json::json!({"factory.name": "support.null-audio-sink", "node.name": "test.mic", "node.description": "test.mic",
            "media.class": "Audio/Source/Virtual", "audio.position": ["FL", "FR"], "object.linger": true});
        graph.run(&["pw-cli", "create-node", "adapter", &props.to_string()]);
        graph.wait_for(|g| g.nodes().contains_key("test.mic"), "dummy source test.mic", 8.0);
        let changes = Rc::new(Cell::new(0));
        let counter = changes.clone();
        let mut options = BackendOptions::native(false);
        options.loopback_store = LoopbackStore::new(Some(graph.dir.join("unused.json")));
        let backend = AudioBackend::new(move || counter.set(counter.get() + 1), |_| {}, options);
        backend.start();
        let native = Self { graph, backend, changes };
        wait_main(|| native.backend.ready() && native.node("test.target").is_some() && native.node("test.mic").is_some(), "native backend ready", 8.0);
        native
    }

    fn node(&self, name: &str) -> Option<audio_manager::model::Node> {
        self.backend.state().nodes.values().find(|n| n.name == name).cloned()
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        self.backend.close();
    }
}

fn single_monitor_process_tracks_writes_and_external_changes() {
    let native = Native::new();
    assert_eq!(native.backend.snapshots_spawned(), 1);
    let target = native.node("test.target").unwrap();
    let before = native.changes.get();
    for step in 0..20 {
        assert_eq!(native.backend.set_volume(target.id, 0.30 + step as f64 * 0.01), Ok(true));
        settle_main(0.05);
    }
    let drag_changes = native.changes.get() - before;
    wait_main(|| (native.node("test.target").unwrap().volume - 0.49).abs() < 0.01, "final drag volume", 8.0);
    settle_main(0.5);
    // No pw-dump per volume step; self-echoes refresh views only once settled.
    assert_eq!(native.backend.snapshots_spawned(), 1);
    assert!(drag_changes <= 3, "{drag_changes} view refreshes during a drag");
    let raw = native.backend.raw_object(target.id).unwrap();
    assert!(raw["info"]["params"]["Props"].as_array().unwrap().iter().any(|i| i.get("channelVolumes").is_some()));
    // mute goes through the mixer immediately
    assert_eq!(native.backend.set_mute(target.id, true), Ok(true));
    wait_main(|| native.node("test.target").unwrap().mute, "mute", 8.0);
    assert_eq!(native.backend.set_mute(target.id, false), Ok(true));
    wait_main(|| !native.node("test.target").unwrap().mute, "unmute", 8.0);
    native.graph.run(&["wpctl", "set-volume", &target.id.to_string(), "0.2"]);
    wait_main(|| { let v = native.node("test.target").unwrap().volume; (v - 0.2f64.cbrt()).abs() < 0.02 || (v - 0.2).abs() < 0.02 }, "external volume change", 8.0);
    native.graph.add_sink("test.hotplug");
    wait_main(|| native.node("test.hotplug").is_some(), "hotplugged sink", 8.0);
    native.graph.remove_node("test.hotplug");
    wait_main(|| native.node("test.hotplug").is_none(), "removed sink", 8.0);
    assert_eq!(native.backend.snapshots_spawned(), 1);
    // default-nodes-api: the configured default follows by node name.
    assert_eq!(native.backend.set_default(target.id), Ok(true));
    wait_main(|| native.backend.state().default_id("Audio/Sink") == Some(target.id), "default output", 8.0);
}

fn input_level_meter_is_hidden_and_stops() {
    if !audio_manager::level::available() {
        println!("skipped input_level_meter_is_hidden_and_stops: GStreamer pipewiresrc/level unavailable");
        return;
    }
    let native = Native::new();
    let mic = native.node("test.mic").unwrap();
    let levels: Rc<std::cell::RefCell<Vec<Option<f64>>>> = Rc::default();
    let sink = levels.clone();
    let meter = native.backend.level_monitor(mic.id, Box::new(move |v| sink.borrow_mut().push(v))).expect("meter");
    wait_main(|| !levels.borrow().is_empty(), "level messages", 8.0);
    assert!(levels.borrow().iter().all(|v| v.is_none_or(|v| (0.0..=1.0).contains(&v))));
    wait_main(|| native.backend.state().nodes.values().any(|n| n.loopback_internal && n.media_class == "Stream/Input/Audio"), "hidden meter stream", 8.0);
    let meter_node = native.graph.nodes().into_values().find(|n| n["info"]["props"]["media.class"] == "Stream/Input/Audio");
    let props = meter_node.expect("meter stream")["info"]["props"].clone();
    assert_eq!(props["node.description"], "Audio Settings level meter");
    assert!(props["audio.manager.meter"] == "true" || props["audio.manager.meter"] == true, "{props}");
    meter.stop();
    wait_main(|| !native.backend.state().nodes.values().any(|n| n.media_class == "Stream/Input/Audio"), "meter stream removal", 8.0);
    let count = levels.borrow().len();
    settle_main(0.3);
    assert_eq!(levels.borrow().len(), count);
    let target = native.node("test.target").unwrap();
    let sink = levels.clone();
    assert!(native.backend.level_monitor(target.id, Box::new(move |v| sink.borrow_mut().push(v))).is_none());
}

fn main() {
    if std::env::args().any(|a| a == "--list") {
        return;
    }
    let missing: Vec<&str> = ["pipewire", "wireplumber", "pw-loopback", "pw-dump", "pw-cli", "wpctl", "dbus-daemon"]
        .into_iter().filter(|n| !which(n)).collect();
    if !missing.is_empty() {
        println!("skipped: missing native test dependencies: {}", missing.join(", "));
        return;
    }
    for (name, test) in [("private_policy_graph", private_policy_graph as fn()),
                         ("builder_never_falls_back_and_recovers_after_hotplug", builder_never_falls_back_and_recovers_after_hotplug),
                         ("service_restores_saved_loopback_disables_missing_target_and_removes", service_restores_saved_loopback_disables_missing_target_and_removes),
                         ("single_monitor_process_tracks_writes_and_external_changes", single_monitor_process_tracks_writes_and_external_changes),
                         ("input_level_meter_is_hidden_and_stops", input_level_meter_is_hidden_and_stops)] {
        let started = Instant::now();
        test();
        println!("test {name} ... ok ({:.1}s)", started.elapsed().as_secs_f64());
    }
}
