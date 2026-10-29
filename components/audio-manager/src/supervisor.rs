//! Loopback worker supervision (loopback_service.py:22-148): own only our
//! `pw-loopback` children; unavailable targets never fall back to speakers.

use crate::graph::Nodes;
use crate::model::{target_available, Loopback, Node};
use crate::store::{entry_json, validate_entry, Entries};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Log like Python's `logging.basicConfig(format="%(levelname)s: %(message)s")`.
#[macro_export]
macro_rules! log {
    ($level:literal, $($arg:tt)*) => {
        eprintln!(concat!($level, ": {}"), format_args!($($arg)*))
    };
}

/// Direct argv: descriptions and target names are never shell code.
pub fn loopback_argv(entry: &Loopback, executable: &str) -> Result<Vec<String>, String> {
    let entry = validate_entry(&entry_json(entry)).map_err(|e| e.0)?;
    let node_name = format!("audio_manager.loopback.{}", entry.id);
    let common = |map: &mut Map<String, Value>| {
        map.insert("audio.manager.loopback.id".into(), entry.id.clone().into());
        map.insert("node.link-group".into(), node_name.clone().into());
        map.insert("node.virtual".into(), true.into());
    };
    let mut capture = Map::new();
    common(&mut capture);
    capture.insert("node.name".into(), node_name.clone().into());
    capture.insert("node.description".into(), entry.name.clone().into());
    capture.insert("media.class".into(), "Audio/Sink".into());
    capture.insert("priority.session".into(), (-100).into());
    let mut playback = Map::new();
    common(&mut playback);
    playback.insert("node.name".into(), format!("{node_name}.output").into());
    playback.insert("node.description".into(), format!("{} output", entry.name).into());
    playback.insert("audio.manager.loopback.internal".into(), true.into());
    playback.insert("target.object".into(), entry.target_name.clone().into());
    playback.insert("node.dont-fallback".into(), true.into());
    playback.insert("node.linger".into(), true.into());
    playback.insert("node.dont-move".into(), true.into());
    playback.insert("state.restore-target".into(), false.into());
    playback.insert("node.passive".into(), true.into());
    let style = suite_core::pyfmt::JsonStyle { ensure_ascii: false, ..suite_core::pyfmt::JsonStyle::DEFAULT };
    Ok(vec![
        executable.to_owned(), "--name".into(), node_name,
        "--capture-props".into(), suite_core::pyfmt::json_dumps(&Value::Object(capture), &style),
        "--playback-props".into(), suite_core::pyfmt::json_dumps(&Value::Object(playback), &style),
    ])
}

/// The subset of `subprocess.Popen` the supervisor uses.
pub trait Child {
    fn poll(&mut self) -> Option<i32>;
    fn terminate(&mut self);
    fn kill(&mut self);
    /// `wait(timeout)`: Err on timeout.
    fn wait(&mut self, timeout: Duration) -> Result<i32, ()>;
}

pub struct ProcessChild {
    child: std::process::Child,
    code: Option<i32>,
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().unwrap_or_else(|| -status.signal().unwrap_or(0))
}

impl Child for ProcessChild {
    fn poll(&mut self) -> Option<i32> {
        if self.code.is_none() {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.code = Some(exit_code(status));
            }
        }
        self.code
    }
    fn terminate(&mut self) {
        // Popen.send_signal: never signal a reaped (possibly recycled) pid.
        if self.poll().is_none() {
            unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        }
    }
    fn kill(&mut self) {
        if self.poll().is_none() {
            let _ = self.child.kill();
        }
    }
    fn wait(&mut self, timeout: Duration) -> Result<i32, ()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(code) = self.poll() {
                return Ok(code);
            }
            if std::time::Instant::now() >= deadline {
                return Err(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// `spawn_loopback(entry, executable)`: stdin/stdout devnull, stderr inherited.
pub fn spawn_loopback(entry: &Loopback, executable: &str) -> io::Result<Box<dyn Child>> {
    let argv = loopback_argv(entry, executable).map_err(io::Error::other)?;
    let child = Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).spawn()?;
    Ok(Box::new(ProcessChild { child, code: None }))
}

pub struct Worker {
    pub entry: Loopback,
    pub target_serial: i64,
    pub process: Box<dyn Child>,
    pub started: f64,
}

pub type Spawn = Box<dyn Fn(&Loopback) -> io::Result<Box<dyn Child>>>;
pub type Clock = Box<dyn Fn() -> f64>;

pub struct LoopbackSupervisor {
    spawn: Spawn,
    clock: Clock,
    pub workers: IndexMap<String, Worker>,
    stopping: IndexMap<String, (Box<dyn Child>, f64)>,
    retry_after: HashMap<String, f64>,
    failures: HashMap<String, u32>,
}

impl LoopbackSupervisor {
    pub fn new(spawn: Spawn, clock: Clock) -> Self {
        Self { spawn, clock, workers: IndexMap::new(), stopping: IndexMap::new(), retry_after: HashMap::new(), failures: HashMap::new() }
    }

    pub fn targets<'a>(entries: &Entries, nodes: &'a Nodes, ready: bool) -> IndexMap<String, &'a Node> {
        if !ready {
            return IndexMap::new();
        }
        let outputs: HashMap<&str, &Node> = nodes
            .values()
            .filter(|n| n.media_class == "Audio/Sink" && n.loopback_id.is_none() && !n.loopback_internal && target_available(Some(n)))
            .map(|n| (n.name.as_str(), n))
            .collect();
        entries.iter().filter_map(|(id, e)| outputs.get(e.target_name.as_str()).map(|n| (id.clone(), *n))).collect()
    }

    fn stop(&mut self, identifier: String, mut worker: Worker, now: f64) {
        worker.process.terminate();
        crate::log!("INFO", "Disabling loopback {}", worker.entry.name);
        self.stopping.insert(identifier, (worker.process, now + 2.0));
    }

    fn failed(&mut self, identifier: &str, now: f64) {
        let failures = self.failures.get(identifier).copied().unwrap_or(0) + 1;
        self.failures.insert(identifier.to_owned(), failures);
        self.retry_after.insert(identifier.to_owned(), now + 30f64.min(2f64.powi((failures - 1).min(5) as i32)));
    }

    pub fn reconcile(&mut self, entries: &Entries, nodes: &Nodes, ready: bool) {
        let now = (self.clock)();
        let targets: IndexMap<String, (i64,)> = Self::targets(entries, nodes, ready).into_iter().map(|(k, n)| (k, (n.serial,))).collect();
        let stopping: Vec<String> = self.stopping.keys().cloned().collect();
        for identifier in stopping {
            let (process, deadline) = self.stopping.get_mut(&identifier).unwrap();
            if process.poll().is_some() {
                self.stopping.shift_remove(&identifier);
            } else if now >= *deadline {
                process.kill();
            }
        }
        let identifiers: Vec<String> = self.workers.keys().cloned().collect();
        for identifier in identifiers {
            let worker = self.workers.get_mut(&identifier).unwrap();
            if worker.process.poll().is_some() {
                let worker = self.workers.shift_remove(&identifier).unwrap();
                crate::log!("WARNING", "Loopback {} exited; retrying shortly", worker.entry.name);
                self.failed(&identifier, now);
                continue;
            }
            let target = targets.get(&identifier);
            if target.is_none() || entries.get(&identifier) != Some(&worker.entry) || target.map(|t| t.0) != Some(worker.target_serial) {
                let worker = self.workers.shift_remove(&identifier).unwrap();
                self.stop(identifier, worker, now);
            } else if now - worker.started >= 30.0 {
                self.failures.remove(&identifier);
                self.retry_after.remove(&identifier);
            }
        }
        for (identifier, (serial,)) in &targets {
            if self.workers.contains_key(identifier) || self.stopping.contains_key(identifier)
                || self.retry_after.get(identifier).copied().unwrap_or(0.0) > now
            {
                continue;
            }
            let entry = &entries[identifier];
            match (self.spawn)(entry) {
                Ok(process) => {
                    self.workers.insert(identifier.clone(), Worker { entry: entry.clone(), target_serial: *serial, process, started: now });
                    crate::log!("INFO", "Enabled loopback {} → {}", entry.name, entry.target_description);
                }
                Err(error) => {
                    crate::log!("ERROR", "Cannot start loopback {}: {}", entry.name, python_os_error(&error));
                    self.failed(identifier, now);
                }
            }
        }
        let stale: Vec<String> = self.retry_after.keys().filter(|k| !entries.contains_key(*k)).cloned().collect();
        for identifier in stale {
            self.retry_after.remove(&identifier);
            self.failures.remove(&identifier);
        }
    }

    pub fn close(&mut self) {
        let mut processes: Vec<Box<dyn Child>> = self.workers.drain(..).map(|(_, w)| w.process).collect();
        processes.extend(self.stopping.drain(..).map(|(_, (p, _))| p));
        for process in &mut processes {
            if process.poll().is_none() {
                process.terminate();
            }
        }
        let deadline = (self.clock)() + 2.0;
        for process in &mut processes {
            let remaining = (deadline - (self.clock)()).max(0.0);
            if process.wait(Duration::from_secs_f64(remaining)).is_err() {
                process.kill();
                let _ = process.wait(Duration::from_secs(1));
            }
        }
    }
}


use suite_core::pyerr::os_error_text as python_os_error;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Choice;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    const IDENTIFIER: &str = "532aa4b1-bd34-4d0c-8a4e-34580ebf1228";

    fn entry(name: &str, target: &str) -> Loopback {
        Loopback::new(IDENTIFIER, name, target, "Bluetooth Headset")
    }

    fn sink(id: i64, serial: i64, name: &str) -> Node {
        Node::new(id, serial, name, "Bluetooth Headset", "Audio/Sink")
    }

    #[derive(Default)]
    struct State {
        code: Option<i32>,
        uncooperative: bool,
        terminated: u32,
        killed: u32,
    }

    struct FakeProcess(Rc<RefCell<State>>);

    impl Child for FakeProcess {
        fn poll(&mut self) -> Option<i32> { self.0.borrow().code }
        fn terminate(&mut self) {
            let mut s = self.0.borrow_mut();
            s.terminated += 1;
            if !s.uncooperative {
                s.code = Some(-15);
            }
        }
        fn kill(&mut self) {
            let mut s = self.0.borrow_mut();
            s.killed += 1;
            s.code = Some(-9);
        }
        fn wait(&mut self, _timeout: Duration) -> Result<i32, ()> { self.0.borrow().code.ok_or(()) }
    }

    struct Harness {
        now: Rc<Cell<f64>>,
        processes: Rc<RefCell<Vec<Rc<RefCell<State>>>>>,
        spawned: Rc<RefCell<Vec<Loopback>>>,
        supervisor: LoopbackSupervisor,
        saved: Entries,
        nodes: Nodes,
    }

    impl Harness {
        fn new() -> Self {
            let now = Rc::new(Cell::new(0.0));
            let processes: Rc<RefCell<Vec<Rc<RefCell<State>>>>> = Rc::default();
            let spawned: Rc<RefCell<Vec<Loopback>>> = Rc::default();
            let (p, s, n) = (processes.clone(), spawned.clone(), now.clone());
            let supervisor = LoopbackSupervisor::new(
                Box::new(move |entry| {
                    s.borrow_mut().push(entry.clone());
                    let state = Rc::new(RefCell::new(State::default()));
                    p.borrow_mut().push(state.clone());
                    Ok(Box::new(FakeProcess(state)) as Box<dyn Child>)
                }),
                Box::new(move || n.get()),
            );
            let saved = Entries::from([(IDENTIFIER.to_owned(), entry("Headset Loopback", "bluez_output.headset"))]);
            let nodes = Nodes::from([(42, sink(42, 4242, "bluez_output.headset"))]);
            Self { now, processes, spawned, supervisor, saved, nodes }
        }
        fn reconcile(&mut self) {
            self.supervisor.reconcile(&self.saved, &self.nodes, true);
        }
        fn count(&self) -> usize { self.processes.borrow().len() }
        fn process(&self, i: usize) -> Rc<RefCell<State>> { self.processes.borrow()[i].clone() }
    }

    #[test]
    fn missing_target_never_falls_back_and_return_restarts() {
        let mut h = Harness::new();
        h.supervisor.reconcile(&h.saved, &Nodes::from([(43, sink(43, 4343, "speakers"))]), true);
        assert_eq!(h.count(), 0);
        h.reconcile();
        assert_eq!(h.count(), 1);
        h.supervisor.reconcile(&h.saved, &Nodes::new(), true);
        assert_eq!(h.process(0).borrow().terminated, 1);
        assert!(h.supervisor.workers.is_empty());
        h.reconcile();
        assert_eq!(h.count(), 2);
    }

    #[test]
    fn unavailable_route_disables_but_suspended_output_is_available() {
        let mut h = Harness::new();
        let node = h.nodes.get_mut(&42).unwrap();
        node.state = "suspended".into();
        node.route_index = Some(4);
        node.routes = vec![Choice::new(4, "Headphones", true)];
        h.reconcile();
        assert_eq!(h.count(), 1);
        h.nodes[&42].routes[0].available = false;
        h.reconcile();
        assert!(h.supervisor.workers.is_empty());
        assert_eq!(h.process(0).borrow().terminated, 1);
        h.nodes[&42].routes[0].available = true;
        h.reconcile();
        assert_eq!(h.count(), 2);
    }

    #[test]
    fn removal_only_stops_owned_child_and_does_not_recreate_it() {
        let mut h = Harness::new();
        let foreign = Rc::new(RefCell::new(State::default()));
        h.reconcile();
        h.supervisor.reconcile(&Entries::new(), &h.nodes, true);
        h.supervisor.reconcile(&Entries::new(), &h.nodes, true);
        assert_eq!(h.count(), 1);
        assert_eq!(h.process(0).borrow().terminated, 1);
        assert_eq!(foreign.borrow().terminated, 0);
    }

    #[test]
    fn recycled_device_serial_and_connection_loss_replace_worker() {
        let mut h = Harness::new();
        h.reconcile();
        h.nodes[&42].serial = 9000;
        h.reconcile();
        assert!(h.supervisor.workers.is_empty());
        h.reconcile();
        assert_eq!(h.supervisor.workers[IDENTIFIER].target_serial, 9000);
        h.supervisor.reconcile(&h.saved, &h.nodes, false);
        assert_eq!(h.process(1).borrow().terminated, 1);
        assert!(h.supervisor.workers.is_empty());
    }

    #[test]
    fn managed_outputs_cannot_be_targets_for_loopback_cycles() {
        let mut h = Harness::new();
        h.nodes[&42].loopback_id = Some(IDENTIFIER.into());
        h.reconcile();
        assert_eq!(h.count(), 0);
        h.nodes[&42].loopback_id = None;
        h.nodes[&42].loopback_internal = true;
        h.reconcile();
        assert_eq!(h.count(), 0);
    }

    #[test]
    fn child_failure_backoff_prevents_restart_spin() {
        let mut h = Harness::new();
        h.reconcile();
        h.process(0).borrow_mut().code = Some(1);
        h.reconcile();
        h.now.set(0.5);
        h.reconcile();
        assert_eq!(h.count(), 1);
        h.now.set(1.0);
        h.reconcile();
        assert_eq!(h.count(), 2);
        h.process(1).borrow_mut().code = Some(1);
        h.reconcile();
        h.now.set(2.0);
        h.reconcile();
        assert_eq!(h.count(), 2);
        h.now.set(3.0);
        h.reconcile();
        assert_eq!(h.count(), 3);
    }

    #[test]
    fn healthy_worker_resets_backoff_after_30_seconds() {
        let mut h = Harness::new();
        h.reconcile();
        h.process(0).borrow_mut().code = Some(1);
        h.reconcile();
        h.now.set(1.0);
        h.reconcile();
        h.now.set(31.0);
        h.reconcile();
        assert!(h.supervisor.failures.is_empty() && h.supervisor.retry_after.is_empty());
    }

    #[test]
    fn hung_child_is_killed_before_replacement_is_started() {
        let mut h = Harness::new();
        h.reconcile();
        h.process(0).borrow_mut().uncooperative = true;
        let renamed = Entries::from([(IDENTIFIER.to_owned(), entry("Renamed loopback", "bluez_output.headset"))]);
        h.supervisor.reconcile(&renamed, &h.nodes, true);
        h.now.set(1.0);
        h.supervisor.reconcile(&renamed, &h.nodes, true);
        assert_eq!(h.count(), 1);
        h.now.set(2.0);
        h.supervisor.reconcile(&renamed, &h.nodes, true);
        assert_eq!(h.process(0).borrow().killed, 1);
        h.supervisor.reconcile(&renamed, &h.nodes, true);
        assert_eq!(h.count(), 2);
        assert_eq!(h.spawned.borrow().last().unwrap().name, "Renamed loopback");
    }

    #[test]
    fn shutdown_cleans_all_owned_children() {
        let mut h = Harness::new();
        h.reconcile();
        h.process(0).borrow_mut().uncooperative = true;
        h.supervisor.close();
        assert_eq!(h.process(0).borrow().terminated, 1);
        assert_eq!(h.process(0).borrow().killed, 1);
        assert!(h.supervisor.workers.is_empty());
    }

    #[test]
    fn spawn_failure_is_logged_and_backed_off() {
        let now = Rc::new(Cell::new(0.0));
        let n = now.clone();
        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        let mut supervisor = LoopbackSupervisor::new(
            Box::new(move |_| { c.set(c.get() + 1); Err(io::Error::from_raw_os_error(2)) }),
            Box::new(move || n.get()));
        let saved = Entries::from([(IDENTIFIER.to_owned(), entry("Headset Loopback", "bluez_output.headset"))]);
        let nodes = Nodes::from([(42, sink(42, 4242, "bluez_output.headset"))]);
        supervisor.reconcile(&saved, &nodes, true);
        supervisor.reconcile(&saved, &nodes, true);
        assert_eq!(calls.get(), 1);
        now.set(1.0);
        supervisor.reconcile(&saved, &nodes, true);
        assert_eq!(calls.get(), 2);
        assert_eq!(python_os_error(&io::Error::from_raw_os_error(2)), "[Errno 2] No such file or directory");
    }

    #[test]
    fn target_pinning_and_ownership_properties_are_explicit() {
        let argv = loopback_argv(&entry("Headset Loopback", "bluez_output.headset"), "pw-loopback").unwrap();
        let at = |flag: &str| -> Value { serde_json::from_str(&argv[argv.iter().position(|a| a == flag).unwrap() + 1]).unwrap() };
        let (capture, playback) = (at("--capture-props"), at("--playback-props"));
        assert_eq!(capture["media.class"], "Audio/Sink");
        assert_eq!(capture["audio.manager.loopback.id"], IDENTIFIER);
        assert_eq!(capture["node.link-group"], playback["node.link-group"]);
        assert!(capture["priority.session"].as_i64().unwrap() < 0);
        assert_eq!(playback["target.object"], "bluez_output.headset");
        for key in ["node.dont-fallback", "node.linger", "node.dont-move", "audio.manager.loopback.internal"] {
            assert_eq!(playback[key], true);
        }
        assert_eq!(playback["state.restore-target"], false);
        assert!(playback.get("node.dont-reconnect").is_none());
        // Exact Python json.dumps(..., ensure_ascii=False) text and key order.
        assert_eq!(argv[..3], ["pw-loopback", "--name", &format!("audio_manager.loopback.{IDENTIFIER}")]);
        assert_eq!(argv[4], format!("{{\"audio.manager.loopback.id\": \"{IDENTIFIER}\", \"node.link-group\": \"audio_manager.loopback.{IDENTIFIER}\", \"node.virtual\": true, \"node.name\": \"audio_manager.loopback.{IDENTIFIER}\", \"node.description\": \"Headset Loopback\", \"media.class\": \"Audio/Sink\", \"priority.session\": -100}}"));
        assert_eq!(argv[6], format!("{{\"audio.manager.loopback.id\": \"{IDENTIFIER}\", \"node.link-group\": \"audio_manager.loopback.{IDENTIFIER}\", \"node.virtual\": true, \"node.name\": \"audio_manager.loopback.{IDENTIFIER}.output\", \"node.description\": \"Headset Loopback output\", \"audio.manager.loopback.internal\": true, \"target.object\": \"bluez_output.headset\", \"node.dont-fallback\": true, \"node.linger\": true, \"node.dont-move\": true, \"state.restore-target\": false, \"node.passive\": true}}"));
    }

    #[test]
    fn names_remain_literal_argv_without_shell_interpolation() {
        let malicious = entry("Headset $(touch /tmp/no) `id` \"quoted\" — ü", "target.\"strange\"");
        let argv = loopback_argv(&malicious, "pw-loopback").unwrap();
        let capture: Value = serde_json::from_str(&argv[4]).unwrap();
        let playback: Value = serde_json::from_str(&argv[6]).unwrap();
        assert_eq!(capture["node.description"], malicious.name);
        assert_eq!(playback["target.object"], malicious.target_name);
        assert!(argv[4].contains("— ü"), "ensure_ascii=False keeps the literal text");
    }
}
