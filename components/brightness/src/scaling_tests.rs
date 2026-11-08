//! Port of test_display_scaling.py with a fake daemon; never configures real
//! monitors. The IPC tests run against a private Unix socket.

use tessel_core::scheduler::FakeScheduler;
use crate::compat::yaml;
use crate::scaling::*;
use serde_json::{json, Value};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

pub fn head(connector: &str, description: &str, serial: &str) -> Value {
    json!({"NAME": connector, "DESCRIPTION": description, "MAKE": "Example", "MODEL": "Display", "SERIAL_NUMBER": serial,
           "CURRENT": {"ENABLED": true, "SCALE": 1.0}})
}

pub struct Daemon {
    pub data: Value,
    pub calls: Vec<(String, Option<Value>, ThreadId)>,
    pub saved: Option<Value>,
    pub fail: Option<String>,
}

impl Daemon {
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            data: json!({"CFG": {"AUTO_SCALE": true, "SCALE": {"First display": 1.25, "Disconnected": 2.0},
                                 "MODE": {"First display": "3840x2160@120"}, "CUSTOM": ["preserve"]},
                         "STATE": {"HEADS": [head("DP-1", "First display", "a"), head("HDMI-A-1", "UGREEN", "b")]}}),
            calls: Vec::new(), saved: None, fail: None,
        }))
    }

    pub fn requester(daemon: &Arc<Mutex<Self>>) -> Requester {
        let daemon = daemon.clone();
        Arc::new(move |operation: &str, config: Option<&Value>| {
            let mut d = daemon.lock().unwrap();
            d.calls.push((operation.into(), config.cloned(), std::thread::current().id()));
            if d.fail.as_deref() == Some(operation) {
                return Err("Synthetic service failure".into());
            }
            match operation {
                "CFG_SET" => {
                    for (key, value) in config.unwrap().as_object().unwrap() {
                        match value.as_object() {
                            Some(map) => {
                                let target = d.data["CFG"].as_object_mut().unwrap().entry(key.clone()).or_insert(json!({}));
                                for (k, v) in map {
                                    target[k] = v.clone();
                                }
                            }
                            None => d.data["CFG"][key] = value.clone(),
                        }
                    }
                }
                "CFG_DEL" => {
                    for (key, values) in config.unwrap().as_object().unwrap() {
                        for item in values.as_object().unwrap().keys() {
                            if let Some(map) = d.data["CFG"].get_mut(key).and_then(Value::as_object_mut) {
                                map.shift_remove(item);
                            }
                        }
                    }
                }
                "CFG_WRITE" => d.saved = Some(d.data["CFG"].clone()),
                _ => {}
            }
            Ok(d.data.clone())
        })
    }
}

struct Fx {
    daemon: Arc<Mutex<Daemon>>,
    backend: ScaleBackend,
    changes: Rc<std::cell::RefCell<Vec<ThreadId>>>,
}

impl Fx {
    fn new() -> Self {
        let daemon = Daemon::new();
        let changes: Rc<std::cell::RefCell<Vec<ThreadId>>> = Rc::default();
        let c = changes.clone();
        let backend = ScaleBackend::new(move || c.borrow_mut().push(std::thread::current().id()),
            ScaleOptions { request: Daemon::requester(&daemon), scheduler: Rc::new(FakeScheduler::default()), spawn_events: false });
        Self { daemon, backend, changes }
    }

    fn wait(&self, mut condition: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(3);
        while !condition() && Instant::now() < end {
            self.backend.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(condition());
    }

    fn start(&self) {
        self.backend.start();
        self.wait(|| self.backend.ready());
    }

    fn operations(&self) -> std::collections::BTreeSet<String> {
        self.daemon.lock().unwrap().calls.iter().map(|c| c.0.clone()).collect()
    }

    fn scale_id(&self, index: usize) -> String {
        self.backend.displays()[index]["scale_id"].as_str().unwrap().to_owned()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.backend.close();
    }
}

#[test]
fn startup_refresh_and_poll_do_not_write_configuration() {
    let fx = Fx::new();
    let before = fx.daemon.lock().unwrap().data.clone();
    fx.start();
    assert!(fx.backend.poll_timer().is_some(), "GET poll every 5 s");
    fx.backend.refresh();
    fx.wait(|| !fx.backend.refresh_pending());
    let daemon = fx.daemon.lock().unwrap();
    assert_eq!(daemon.data, before);
    assert_eq!(daemon.calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["GET", "GET"]);
    let main = std::thread::current().id();
    assert!(daemon.calls.iter().all(|c| c.2 != main));
    assert!(fx.changes.borrow().iter().all(|t| *t == main));
}

#[test]
fn explicit_scale_changes_only_target_and_persists_without_disabling_auto() {
    let fx = Fx::new();
    fx.start();
    assert!(fx.backend.set_scale("HDMI-A-1", 1.37, &fx.scale_id(1)));
    fx.wait(|| !fx.backend.busy());
    let daemon = fx.daemon.lock().unwrap();
    let saved = daemon.saved.clone().unwrap();
    assert_eq!(saved["SCALE"], json!({"First display": 1.25, "Disconnected": 2.0, "UGREEN": 1.375}));
    assert_eq!(saved["AUTO_SCALE"], true);
    assert_eq!(saved["MODE"], json!({"First display": "3840x2160@120"}));
    assert_eq!(saved["CUSTOM"], json!(["preserve"]));
    drop(daemon);
    assert_eq!(fx.backend.displays()[1]["scale"]["value"], json!(1.375));
}

#[test]
fn auto_scale_off_matches_manager_clears_attached_overrides_only() {
    let fx = Fx::new();
    fx.start();
    assert!(fx.backend.set_auto_scale(false));
    fx.wait(|| !fx.backend.busy());
    {
        let daemon = fx.daemon.lock().unwrap();
        let saved = daemon.saved.clone().unwrap();
        assert_eq!(saved["SCALE"], json!({"Disconnected": 2.0}));
        assert_eq!(saved["AUTO_SCALE"], false);
        let deleted = daemon.calls.iter().find(|c| c.0 == "CFG_DEL").unwrap().1.clone().unwrap();
        assert_eq!(deleted, json!({"SCALE": {"First display": 1.25, "UGREEN": 1.0}}));
    }
    assert!(fx.backend.set_auto_scale(true));
    fx.wait(|| !fx.backend.busy());
    let saved = fx.daemon.lock().unwrap().saved.clone().unwrap();
    assert_eq!(saved["AUTO_SCALE"], true);
    assert_eq!(saved["SCALE"], json!({"Disconnected": 2.0}));
}

#[test]
fn hotplug_replacement_between_request_and_write_is_rejected() {
    let fx = Fx::new();
    fx.start();
    let old = fx.scale_id(0);
    fx.daemon.lock().unwrap().data["STATE"]["HEADS"][0]["SERIAL_NUMBER"] = json!("replacement");
    assert!(fx.backend.set_scale("DP-1", 2.0, &old));
    fx.wait(|| !fx.backend.busy());
    assert!(fx.backend.error().contains("changed"));
    assert_eq!(fx.operations(), ["GET".to_owned()].into_iter().collect());
    assert_ne!(old, fx.scale_id(0));
}

#[test]
fn offline_disabled_and_duplicate_heads_cannot_be_scaled() {
    let fx = Fx::new();
    {
        let mut daemon = fx.daemon.lock().unwrap();
        daemon.data["STATE"]["HEADS"][1]["CURRENT"]["ENABLED"] = json!(false);
        daemon.data["STATE"]["HEADS"].as_array_mut().unwrap().push(head("DP-2", "First display", "c"));
    }
    fx.start();
    for display in fx.backend.displays() {
        assert_eq!(display["scale"]["supported"], false);
        assert!(!fx.backend.set_scale(display["connector"].as_str().unwrap(), 2.0, display["scale_id"].as_str().unwrap()));
    }
    assert_eq!(fx.backend.displays()[1]["scale"]["error"], "Enable this display to change its scale.");
    assert_eq!(fx.backend.displays()[0]["scale"]["error"], "This display cannot be identified uniquely.");
    assert_eq!(fx.operations(), ["GET".to_owned()].into_iter().collect());
}

#[test]
fn failed_save_reports_runtime_change_without_claiming_persistence() {
    let fx = Fx::new();
    fx.start();
    fx.daemon.lock().unwrap().fail = Some("CFG_WRITE".into());
    assert!(fx.backend.set_scale("DP-1", 2.0, &fx.scale_id(0)));
    fx.wait(|| !fx.backend.busy());
    assert!(fx.backend.ready());
    assert_eq!(fx.backend.displays()[0]["scale"]["value"], json!(2.0));
    assert!(fx.backend.error().contains("could not be saved"));
    assert!(fx.daemon.lock().unwrap().saved.is_none());
}

#[test]
fn service_loss_disables_stale_scale_and_recovers_on_refresh() {
    let fx = Fx::new();
    fx.start();
    fx.daemon.lock().unwrap().fail = Some("GET".into());
    fx.backend.refresh();
    fx.wait(|| !fx.backend.refresh_pending());
    assert!(!fx.backend.ready());
    let combined = merge_displays(&[], Some(&fx.backend));
    assert_eq!(combined[0]["scale"]["supported"], false);
    assert_eq!(combined[0]["scale"]["error"], "Synthetic service failure");
    assert!(!fx.backend.set_auto_scale(true));
    fx.daemon.lock().unwrap().fail = None;
    fx.backend.refresh();
    fx.wait(|| fx.backend.ready());
    assert_eq!(merge_displays(&[], Some(&fx.backend))[0]["scale"]["supported"], true);
}

#[test]
fn scaling_available_for_unsupported_hardware_without_changing_identity() {
    let fx = Fx::new();
    fx.start();
    let hardware = vec![json!({"connector": "HDMI-A-1", "id": "edid-123", "enabled": true,
                               "brightness": {"supported": false}, "contrast": {"supported": false}})];
    let combined = merge_displays(&hardware, Some(&fx.backend));
    assert_eq!(combined[0]["id"], "edid-123");
    assert_eq!(combined[0]["scale"]["supported"], true);
    assert_eq!(combined[0]["brightness"]["supported"], false);
    assert!(hardware[0].get("scale").is_none());
    assert_eq!(combined.len(), 2);
    assert_eq!(combined[1]["id"], format!("scale:{}", fx.scale_id(0)));
    let keys: Vec<&String> = combined[1].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["connector", "name", "enabled", "id", "method", "geometry", "error", "brightness", "contrast", "scale_id", "scale"]);
    let none = merge_displays(&hardware, None);
    assert_eq!(none[0]["scale"], json!({"supported": false, "value": null, "busy": false, "override": false, "error": UNAVAILABLE}));
    assert_eq!(none[0]["scale_id"], "");
}

#[test]
fn invalid_scale_values_and_unknown_identifiers_never_write() {
    let fx = Fx::new();
    fx.start();
    for value in [0.0, 0.5, 3.0, f64::INFINITY, f64::NAN] {
        assert!(!fx.backend.set_scale("DP-1", value, &fx.scale_id(0)));
    }
    assert!(!fx.backend.set_scale("DP-1", 1.5, "stale"));
    assert!(!fx.backend.reject_auto_scale());
    assert_eq!(fx.operations(), ["GET".to_owned()].into_iter().collect());
    assert_eq!(validated_scale(1.31).unwrap(), 1.25);
    assert_eq!(validated_scale(1.3125).unwrap(), 1.25, "Python round() is half to even");
}

#[test]
fn scale_identity_hash_matches_python() {
    let snapshot = snapshot(&json!({"CFG": {}, "STATE": {"HEADS": [head("DP-1", "First display", "a")]}})).unwrap();
    // hashlib.sha256(json.dumps(["DP-1","First display","Example","Display","a"], sort_keys=True).encode()).hexdigest()
    let expected = {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(br#"["DP-1", "First display", "Example", "Display", "a"]"#);
        digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    assert_eq!(snapshot.displays[0]["scale_id"], expected);
    assert!(!snapshot.auto_scale);
}

fn serve(reply: Vec<u8>) -> (tempdir::Dir, std::thread::JoinHandle<Vec<u8>>) {
    let dir = tempdir::Dir::new();
    let listener = std::os::unix::net::UnixListener::bind(dir.0.join("tessel-displays.sock")).unwrap();
    let handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut received = vec![0u8; 4096];
        let count = stream.read(&mut received).unwrap_or(0);
        received.truncate(count);
        let (a, b) = reply.split_at(reply.len().min(9));
        let _ = stream.write_all(a);
        let _ = stream.write_all(b);
        received
    });
    (dir, handle)
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Dir {
        pub fn new() -> Self {
            Self(crate::backend_tests::temp_root("ipc"))
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// `XDG_RUNTIME_DIR` is process-global: the IPC exchanges run in one test.
#[test]
fn ipc_complete_response_structured_payload_and_errors() {
    let response = json!({"DONE": true, "RC": 0, "CFG": {}, "STATE": {"HEADS": []}});
    let (dir, handle) = serve(b"DONE: true\nRC: 0\nCFG: {}\nSTATE:\n  HEADS: []\n".to_vec());
    std::env::set_var("XDG_RUNTIME_DIR", &dir.0);
    std::env::remove_var("XDG_VTNR");
    let value = ipc_request("CFG_SET", Some(&json!({"SCALE": {"A: quoted display": 1.25}}))).unwrap();
    assert_eq!(value, response);
    let payload = handle.join().unwrap();
    assert_eq!(yaml::load(&payload).unwrap(), json!({"OP": "CFG_SET", "CFG": {"SCALE": {"A: quoted display": 1.25}}}));
    drop(dir);
    for reply in ["", "{}", "DONE: false\nRC: 0\n", "DONE: true\nRC: false\n", "DONE: true\nRC: 1\nMESSAGES: [Unsupported scale]\n"] {
        let (dir, handle) = serve(reply.as_bytes().to_vec());
        std::env::set_var("XDG_RUNTIME_DIR", &dir.0);
        let error = ipc_request("GET", None).unwrap_err();
        if reply.contains("MESSAGES") {
            assert_eq!(error, "['Unsupported scale']");
        } else {
            assert_eq!(error, "The display service returned an invalid response.");
        }
        handle.join().unwrap();
    }
    std::env::set_var("XDG_RUNTIME_DIR", "/nonexistent-brightness-test");
    assert!(ipc_request("GET", None).unwrap_err().starts_with("[Errno 2]"));
}
