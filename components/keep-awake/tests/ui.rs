//! test_keep_awake_ui.py: singleton popup controls in a private headless
//! labwc (tests/run_headless.sh). Under plain `cargo test` it skips itself.

mod common;

use common::{settle, wait_for};
use gio::prelude::*;
use tessel_keep_awake::app::{BackendFactory, KeepAwakeApplication, APPLICATION_ID};
use tessel_keep_awake::timed::Inhibit;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_gtk::gtk;
use tessel_gtk::gtk::prelude::*;

const BIN: &str = env!("CARGO_BIN_EXE_tessel-keep-awake");

#[derive(Default)]
struct Fake {
    changed: RefCell<Option<Rc<dyn Fn()>>>,
    ready: Cell<bool>,
    active: Cell<bool>,
    requested: Cell<bool>,
    error: RefCell<String>,
    writes: RefCell<Vec<bool>>,
}

impl Fake {
    fn notify(&self) {
        let changed = self.changed.borrow().clone();
        if let Some(changed) = changed { changed() }
    }
}

impl Inhibit for Fake {
    fn start(&self) { self.ready.set(true); self.notify() }
    fn set_enabled(&self, enabled: bool) -> bool {
        self.active.set(enabled);
        self.requested.set(enabled);
        self.writes.borrow_mut().push(enabled);
        self.notify();
        true
    }
    fn requested(&self) -> bool { self.requested.get() }
    fn active(&self) -> bool { self.active.get() }
    fn ready(&self) -> bool { self.ready.get() }
    fn error(&self) -> String { self.error.borrow().clone() }
    fn on_changed(&self) { self.notify() }
    fn close(&self) { self.active.set(false); self.requested.set(false); self.ready.set(false) }
}

fn client(args: &[&str]) -> String {
    let mut child = Command::new(BIN).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "secondary command {args:?} did not return");
        settle(0.01);
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

fn status() -> serde_json::Value { serde_json::from_str(&client(&["--status"])).unwrap() }

fn singleton_controls_popup_and_closing_popup_preserves_state(state: &PathBuf) {
    let fake = Rc::new(Fake::default());
    let handle = fake.clone();
    let factory: BackendFactory = Box::new(move |changed| {
        handle.changed.replace(Some(changed));
        handle as Rc<dyn Inhibit>
    });
    let app = KeepAwakeApplication::new(APPLICATION_ID, factory, false, Some(state.join("timer.json")));
    app.register(None::<&gio::Cancellable>).unwrap();
    assert!(!app.is_remote());
    assert_eq!(status()["active"], false);
    client(&["--enable"]);
    assert!(fake.active.get());
    assert!(app.popup().is_none(), "--enable must not open the popup");
    assert_eq!(status()["active"], true);
    client(&["--popup", "--popup-edge", "top", "--popup-offset", "45"]);
    assert!(wait_for(|| app.popup().is_some_and(|p| p.window().is_mapped()), 5.0), "popup never mapped");
    let popup = app.popup().unwrap();
    let window = popup.window().clone();
    for class in ["tessel-keep-awake", "tray-popup"] {
        assert!(window.has_css_class(class), "{class}");
    }
    assert_eq!(window.title().as_deref(), Some("Keep Awake"));
    let controller = popup.controller().unwrap();
    assert_eq!((controller.edge().as_str(), controller.offset()), ("top", 45));
    assert!(popup.toggle().is_active());
    assert_eq!(popup.subtitle().text(), "Your session will stay awake");
    assert_eq!(popup.status().text(), "Active for this session. Closing this popup keeps it on.");
    let surface = window.child().unwrap();
    assert!(surface.has_css_class("popup-surface"));
    assert_eq!(popup.duration().model().unwrap().n_items(), 4);
    popup.dismiss();
    settle(0.1);
    assert!(!window.is_visible());
    assert!(fake.active.get(), "closing the popup must keep Keep Awake on");
    client(&["--disable"]);
    assert!(!fake.active.get());
    assert!(!popup.toggle().is_active());
    assert_eq!(popup.status().text(), "Off · Your power and brightness settings apply.");
    popup.show();
    assert!(wait_for(|| window.is_mapped(), 5.0));
    popup.toggle().set_active(true);
    assert!(fake.active.get());
    assert_eq!(*fake.writes.borrow(), vec![true, false, true]);
    // Durations: choosing one sets an end time; the popup shows it.
    popup.duration().set_selected(0);
    settle(0.05);
    let backend = app.backend().unwrap();
    assert_eq!(backend.duration(), 30);
    assert!(backend.until().is_some());
    assert_eq!(status()["duration"], 30);
    assert!(popup.status().text().contains("On until"), "{}", popup.status().text());
    popup.duration().set_selected(3);
    assert_eq!(backend.until(), None);
    // Same position: activate() toggles the existing popup; a new one replaces it.
    client(&["--popup", "--popup-edge", "top", "--popup-offset", "45"]);
    assert!(wait_for(|| !window.is_visible(), 3.0), "same-position --popup should toggle it closed");
    assert!(app.popup().is_some_and(|p| p.window() == &window));
    client(&["--popup", "--popup-edge", "bottom", "--popup-offset", "76"]);
    assert!(wait_for(|| app.popup().is_some_and(|p| p.window() != &window && p.window().is_mapped()), 5.0));
    let popup = app.popup().unwrap();
    fake.ready.set(false);
    fake.active.set(false);
    fake.requested.set(false);
    fake.error.replace("System power management unavailable".into());
    popup.refresh();
    assert!(!popup.toggle().is_sensitive());
    assert!(popup.status().text().contains("unavailable"));
    assert!(popup.status().has_css_class("error"));
    app.shutdown_now();
    settle(0.1);
    assert!(app.popup().is_none());
    // A registered GApplication cannot be finalized outside run() (GIO would
    // unregister from a half-destroyed object); the process exits soon anyway.
    std::mem::forget(app);
}

fn launcher_without_primary_never_creates_a_backend() {
    let started = Rc::new(Cell::new(false));
    let flag = started.clone();
    let factory: BackendFactory = Box::new(move |_| {
        flag.set(true);
        panic!("A launcher-only client started the inhibitor");
    });
    let app = KeepAwakeApplication::new("org.tessel.KeepAwakeMissingTest", factory, true, None);
    assert_ne!(app.run_with_args(&["tessel-keep-awake", "--toggle"]), glib::ExitCode::SUCCESS);
    assert!(!started.get());
}

fn main() {
    if std::env::var("KEEP_AWAKE_UI").as_deref() != Ok("1") {
        eprintln!("ui: skipped (run tests/run_headless.sh)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(!runtime.is_empty() && runtime != "/run/user/1000", "refusing to run on the live session");
    tessel_core::i18n::initialize();
    gtk::init().unwrap();
    let state = PathBuf::from(std::env::var("TMPDIR").unwrap()).join("keep-awake-state");
    let _ = std::fs::create_dir_all(&state);
    // Never the real timer file.
    unsafe { std::env::set_var("XDG_STATE_HOME", &state) };
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    singleton_controls_popup_and_closing_popup_preserves_state(&state);
    println!("ok singleton_controls_popup_and_closing_popup_preserves_state");
    launcher_without_primary_never_creates_a_backend();
    println!("ok launcher_without_primary_never_creates_a_backend");
    println!("ui: all passed");
}
