//! InputBridge against a scripted fake helper (no Wayland), and the real
//! helper's offline `--labels` mode used by the lock screen.
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use waykeys::backend::{Command, InputBridge, CLOSED_MESSAGE};

fn settle_until(limit: f64, done: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs_f64(limit);
    let context = glib::MainContext::default();
    while Instant::now() < end && !done() {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn scratch() -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("waykeys-bridge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn bridge_and_labels() {
    let dir = scratch();
    let log = dir.join("stdin.log");
    let helper = dir.join("fake-helper");
    // Echo a ready event with the layout argument, record every command, quit on "key 1 0".
    std::fs::write(&helper, format!("#!/bin/sh\nprintf '{{\"event\":\"ready\",\"input_method\":true,\"layout\":\"%s\"}}\\n' \"$1\"\n\
        echo '[1]'\nwhile read -r line; do echo \"$line\" >> '{}'; [ \"$line\" = 'key 1 0' ] && exit 3; done\nexit 0\n", log.display())).unwrap();
    std::fs::set_permissions(&helper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let events = Rc::new(RefCell::new(Vec::new()));
    let failures = Rc::new(RefCell::new(Vec::<String>::new()));
    let (e, f) = (events.clone(), failures.clone());
    let bridge = InputBridge::new("de", move |event| e.borrow_mut().push(event), move |error| f.borrow_mut().push(error),
                                  Some(helper.clone())).unwrap();
    settle_until(3.0, || !failures.borrow().is_empty());
    assert_eq!(events.borrow().len(), 1);
    assert_eq!(events.borrow()[0]["layout"], "de");
    // A non-object line fails the bridge (and stops reading), like Python.
    assert_eq!(failures.borrow().as_slice(), ["Invalid input response."]);
    for command in [Command::Key(30, 1), Command::Text(0x20ac, 0), Command::Layout("fr".into())] {
        bridge.send(&command).unwrap();
    }
    assert!(bridge.send(&Command::Layout("us;rm".into())).is_err());
    // Queue cap: 128 more than fit are dropped silently.
    for _ in 0..200 {
        bridge.send(&Command::Key(2, 0)).unwrap();
    }
    settle_until(1.0, || false);
    let queued = std::fs::read_to_string(&log).unwrap().lines().count();
    assert!(queued <= 3 + 128, "queue is capped: {queued}");
    bridge.send(&Command::Key(1, 0)).unwrap();
    settle_until(5.0, || failures.borrow().len() >= 2);
    let lines = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = lines.lines().collect();
    assert_eq!(&lines[..3], ["key 30 1", "text 8364", "layout fr"]);
    assert_eq!(lines.last(), Some(&"key 1 0"));
    // The helper exited without being closed (it may die before reading "key 1 0").
    assert!(failures.borrow().iter().any(|f| f == CLOSED_MESSAGE), "{:?}", failures.borrow());
    bridge.close();

    // close(): EOF makes a well-behaved helper exit; no failure is reported afterwards.
    std::fs::remove_file(&log).unwrap();
    let (f2, ended) = (Rc::new(RefCell::new(Vec::<String>::new())), Rc::new(RefCell::new(false)));
    let f2c = f2.clone();
    let bridge = InputBridge::new("us", |_| {}, move |error| f2c.borrow_mut().push(error), Some(helper.clone())).unwrap();
    let process = bridge.process();
    let flag = ended.clone();
    process.wait_async(None::<&gio::Cancellable>, move |_| *flag.borrow_mut() = true);
    settle_until(1.0, || false);
    bridge.close();
    bridge.send(&Command::Key(30, 0)).unwrap();
    settle_until(3.0, || *ended.borrow());
    assert!(*ended.borrow());
    assert_eq!(process.exit_status(), 0);
    assert!(f2.borrow().iter().all(|f| f != CLOSED_MESSAGE), "{:?}", f2.borrow());
    assert!(!log.exists(), "nothing is written after close");

    // Missing helper → spawn error.
    assert!(InputBridge::new("us", |_| {}, |_| {}, Some(dir.join("missing"))).is_err());

    // The real helper's offline labels (no Wayland connection).
    let helper = PathBuf::from(waykeys::BUILT_HELPER);
    let us = waykeys::lock::labels(&helper, "us");
    assert_eq!(us["event"], "layout");
    assert_eq!(us["keys"]["30"][0], "a");
    let de = waykeys::lock::labels(&helper, "de");
    assert_eq!(de["keys"]["21"][0], "z");
    let fr = waykeys::lock::labels(&helper, "fr");
    assert_eq!(fr["dead"]["26"][0], "dead_circumflex");
    assert_eq!(waykeys::lock::labels(&helper, "nonexistentlayout"), serde_json::json!({}));
    std::fs::remove_dir_all(dir).unwrap();
}
