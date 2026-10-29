//! test_popup.py: GTK4 controls on a private headless labwc with fake
//! privileged operations (tests/run_headless.sh runs one case per process,
//! because each registers the real application id on the private bus).
//! Under plain `cargo test` it skips itself.

use gio::prelude::*;
use network_discovery::app::{create_application, Backend, BackendFactory, Browser, BrowserFactory, Discovery, APPLICATION_ID};
use network_discovery::backend::{summarize, Done, SOCKET};
use network_discovery::browse::Device;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use suite_gtk::gtk;
use suite_gtk::gtk::prelude::*;

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(predicate: impl Fn() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.01);
        if predicate() {
            return true;
        }
    }
    false
}

struct FakeBackend {
    changed: Rc<dyn Fn()>,
    enabled: Cell<bool>,
    installed: Cell<bool>,
    commands: RefCell<Vec<bool>>,
}

impl Backend for FakeBackend {
    fn start(&self) {}
    fn close(&self) {}
    fn snapshot(&self) -> Map<String, Value> {
        let load = if self.installed.get() { "loaded" } else { "not-found" };
        let active = if self.enabled.get() { "active" } else { "inactive" };
        let units = json!({SOCKET: {"LoadState": load, "ActiveState": active}});
        summarize(units.as_object().unwrap(), true, "", false)
    }
    fn ready(&self) -> bool { true }
    fn busy(&self) -> bool { false }
    fn set_enabled(&self, enabled: bool, done: Done) -> bool {
        if !self.installed.get() {
            return false;
        }
        self.commands.borrow_mut().push(enabled);
        self.enabled.set(enabled);
        let changed = self.changed.clone();
        glib::idle_add_local_once(move || {
            done(true, String::new());
            changed();
        });
        true
    }
}

thread_local!(static BACKEND: RefCell<Option<Rc<FakeBackend>>> = const { RefCell::new(None) });
thread_local!(static STARTED: Cell<usize> = const { Cell::new(0) });

fn backend() -> Rc<FakeBackend> { BACKEND.with(|b| b.borrow().clone().unwrap()) }

fn backend_factory(installed: bool) -> BackendFactory {
    Box::new(move |changed| {
        let fake = Rc::new(FakeBackend { changed, enabled: Cell::new(true), installed: Cell::new(installed), commands: RefCell::default() });
        BACKEND.with(|b| b.replace(Some(fake.clone())));
        fake as Rc<dyn Backend>
    })
}

struct FakeBrowser {
    finished: Rc<dyn Fn()>,
    devices: RefCell<Vec<Device>>,
}

impl Browser for FakeBrowser {
    fn start(&self) -> bool {
        STARTED.with(|s| s.set(s.get() + 1));
        self.devices.replace(vec![Device { name: "Office Printer".into(), host: "brw".into(), address: "192.0.2.2".into(),
                                           services: vec!["Printer".into(), "Web page".into()] }]);
        let finished = self.finished.clone();
        glib::idle_add_local_once(move || finished());
        true
    }
    fn close(&self) {}
    fn running(&self) -> bool { false }
    fn devices(&self) -> Vec<Device> { self.devices.borrow().clone() }
    fn error(&self) -> String { String::new() }
}

fn browser_factory() -> BrowserFactory {
    Rc::new(|finished| Rc::new(FakeBrowser { finished, devices: RefCell::default() }) as Rc<dyn Browser>)
}

fn label_text(widget: Option<gtk::Widget>) -> String {
    widget.and_then(|w| w.downcast::<gtk::Label>().ok()).map(|l| l.text().to_string()).unwrap_or_default()
}

fn popup_style_state_and_dismissal_before_authentication() {
    let (app, state) = create_application(backend_factory(true), browser_factory());
    app.register(None::<&gio::Cancellable>).unwrap();
    app.activate();
    assert!(wait_for(|| state.popup().is_some_and(|p| p.window().is_mapped()), 5.0), "popup never mapped");
    settle(0.3);
    let popup = state.popup().unwrap();
    let window = popup.window().clone();
    assert!(window.is_visible());
    assert!(window.has_css_class("discovery-manager"));
    assert!(window.has_css_class("tray-popup"));
    assert_eq!(window.title().as_deref(), Some("Discover Network Devices"));
    assert!(window.child().unwrap().has_css_class("popup-surface"));
    assert!(popup.switch().is_active());
    assert!(popup.switch().is_sensitive());
    assert_eq!(popup.state_label().text(), "Discovery is on.");
    assert!(!popup.detail().is_visible(), "Only problems use the detail line");
    // The palette theme adds its own window class; ours must not replace it.
    let theme = window.css_classes().iter().any(|c| c.starts_with("waydock-companion-"));
    let palette = suite_core::paths::cache_home().join("waydock/palette.json");
    assert!(theme || !palette.exists() || !window.is_realized(), "theme class missing");
    assert!(window.width() <= 420, "width {}", window.width());
    assert_eq!(STARTED.with(Cell::get), 1, "Opening the popup lists discovered devices");
    assert!(popup.scroll().is_visible());
    assert!(popup.devices().has_css_class("discovered") && popup.devices().has_css_class("boxed-list"));
    let row = popup.devices().row_at_index(0).expect("a device row");
    let entry = row.child().unwrap();
    assert_eq!(label_text(entry.first_child()), "Office Printer");
    assert_eq!(label_text(entry.last_child()), "Printer, Web page · 192.0.2.2");
    assert!(!popup.found_status().is_visible());
    assert!(popup.rescan().is_sensitive());
    assert!(backend().commands.borrow().is_empty(), "Opening the popup must not toggle discovery");
    // Search again re-runs the browser.
    popup.rescan().emit_clicked();
    settle(0.1);
    assert_eq!(STARTED.with(Cell::get), 2);
    popup.switch().emit_by_name::<bool>("state-set", &[&false]);
    assert!(!window.is_visible(), "Polkit must not compete with the popup keyboard grab");
    assert_eq!(*backend().commands.borrow(), [false]);
    settle(0.2);
    assert!(state.popup().is_none(), "dismissed for authentication, the popup closes");
    app.quit();
    settle(0.1);
}

fn moving_to_another_edge_reshows_instead_of_closing() {
    let (app, state) = create_application(backend_factory(true), browser_factory());
    app.register(None::<&gio::Cancellable>).unwrap();
    state.show("top", 40);
    assert!(wait_for(|| state.popup().is_some_and(|p| p.window().is_visible()), 5.0));
    settle(0.3);
    let popup = state.popup().unwrap();
    assert_eq!((popup.controller().edge().as_str(), popup.controller().offset()), ("top", 40));
    // set_position() hides before the asynchronous re-show; that hide must
    // not be taken for a dismissal that destroys the popup.
    state.show("bottom", 76);
    assert!(wait_for(|| popup.window().is_visible(), 3.0));
    settle(0.1);
    assert!(state.popup().is_some_and(|p| p.window() == popup.window()));
    assert!(popup.window().is_visible());
    assert_eq!(popup.controller().edge(), "bottom");
    popup.controller().dismiss();
    settle(0.1);
    assert!(state.popup().is_none(), "A real dismissal still closes the popup");
    app.quit();
    settle(0.1);
}

/// A pure remote (IS_LAUNCHER) forwarding argv to the primary under test.
fn remote(args: &[&str]) -> i32 {
    let mut child = Command::new(std::env::current_exe().unwrap()).env("DISCOVERY_REMOTE", args.join("\x1f"))
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "remote {args:?} did not return");
        settle(0.01);
    }
    child.wait().unwrap().code().unwrap_or(-1)
}

fn toggle_and_command_line(state: &Discovery, app: &gtk::Application) {
    app.register(None::<&gio::Cancellable>).unwrap();
    assert!(!app.is_remote());
    assert_eq!(remote(&["--toggle"]), 0);
    assert!(wait_for(|| backend().commands.borrow().len() == 1, 3.0));
    assert_eq!(*backend().commands.borrow(), [false], "toggle flips the current state");
    settle(0.2);
    assert!(state.popup().is_none(), "a successful toggle opens nothing");
    assert_eq!(remote(&["--popup", "--popup-edge", "left", "--popup-offset", "12"]), 0);
    assert!(wait_for(|| state.popup().is_some_and(|p| p.window().is_mapped()), 5.0));
    let popup = state.popup().unwrap();
    assert_eq!((popup.controller().edge().as_str(), popup.controller().offset()), ("left", 12));
    assert_eq!(popup.state_label().text(), "Discovery is off.");
    assert_eq!(popup.found_status().text(), "Turn on discovery to see devices on this network.");
    assert!(!popup.rescan().is_sensitive());
    assert_eq!(remote(&["--popup-offset", "-1"]), 2);
    app.quit();
    settle(0.1);
}

fn toggle_without_avahi_shows_the_message() {
    let (app, state) = create_application(backend_factory(false), browser_factory());
    app.register(None::<&gio::Cancellable>).unwrap();
    assert_eq!(remote(&["--toggle"]), 0);
    assert!(wait_for(|| state.popup().is_some_and(|p| p.window().is_mapped()), 5.0), "the failure opens the popup");
    let popup = state.popup().unwrap();
    assert_eq!(state.message(), "Avahi is not installed.");
    assert!(popup.detail().is_visible());
    assert_eq!(popup.detail().text(), "Avahi is not installed.");
    assert_eq!(popup.state_label().text(), "Avahi is not installed.");
    assert!(!popup.switch().is_sensitive());
    app.quit();
    settle(0.1);
}

fn main() {
    if let Ok(args) = std::env::var("DISCOVERY_REMOTE") {
        let mut argv = vec!["discovery_manager.py".to_owned()];
        argv.extend(args.split('\x1f').map(String::from));
        let app = gio::Application::new(Some(APPLICATION_ID), suite_core::single_instance::application_flags(true));
        std::process::exit(i32::from(app.run_with_args(&argv)));
    }
    let Ok(case) = std::env::var("DISCOVERY_UI") else {
        eprintln!("ui_headless: skipped (run tests/run_headless.sh)");
        return;
    };
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(!runtime.is_empty() && runtime != "/run/user/1000", "refusing to run on the live session");
    suite_core::i18n::initialize();
    gtk::init().unwrap();
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    match case.as_str() {
        "popup_style_state_and_dismissal_before_authentication" => popup_style_state_and_dismissal_before_authentication(),
        "moving_to_another_edge_reshows_instead_of_closing" => moving_to_another_edge_reshows_instead_of_closing(),
        "toggle_and_command_line" => {
            let (app, state) = create_application(backend_factory(true), browser_factory());
            toggle_and_command_line(&state, &app);
        }
        "toggle_without_avahi_shows_the_message" => toggle_without_avahi_shows_the_message(),
        other => panic!("unknown case {other}"),
    }
    println!("ok {case}");
}
