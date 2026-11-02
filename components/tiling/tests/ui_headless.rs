//! The shortcut popup and TilingControls in a private headless labwc
//! (tests/run_headless.sh, one case per process). The compositor is a fake
//! `call`, rc.xml is a scratch file and reload is a no-op: no real labwc is
//! ever reconfigured. Under plain `cargo test` it skips itself.

use gio::prelude::*;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tessel_gtk::gtk;
use tessel_gtk::gtk::prelude::*;
use tessel_tiling::bindings::{BINDINGS, EXTRA_SHORTCUTS};
use tessel_tiling::client::{ClientError, Reply};
use tessel_tiling::config::{Reload, TilingConfig};
use tessel_tiling::gaps::with_gap;
use tessel_tiling::outline::Call;
use tessel_tiling::ui::popup::build_application;
use tessel_tiling::ui::settings::{Runner, StatusCall};

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

#[derive(Default)]
struct Compositor {
    enabled: bool,
    outline_supported: bool,
    parking_supported: bool,
    fail_enable: bool,
    commands: Vec<String>,
}

fn reply(state: &Compositor) -> Reply {
    json!({"version": 1, "ready": true, "compositor_pid": 0, "enabled": state.enabled, "parking_supported": state.parking_supported,
           "outline_supported": state.outline_supported, "outline_enabled": true, "error": ""}).as_object().unwrap().clone()
}

struct Fixture {
    root: PathBuf,
    state: Arc<Mutex<Compositor>>,
    config: Arc<TilingConfig>,
    status: StatusCall,
}

fn fixture(name: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("tessel-tiling-ui-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("rc.xml"), b"<labwc_config><core><gap>12</gap></core></labwc_config>\n").unwrap();
    let state = Arc::new(Mutex::new(Compositor { enabled: true, parking_supported: false, outline_supported: true, ..Default::default() }));
    let shared = state.clone();
    let call: Call = Arc::new(move |command: &str| {
        let mut state = shared.lock().unwrap();
        state.commands.push(command.into());
        match command {
            "enable" if state.fail_enable => return Err(ClientError::Runtime("Not applied".into())),
            "enable" => state.enabled = true,
            "disable" => state.enabled = false,
            _ => {}
        }
        Ok(reply(&state))
    });
    let reload: Reload = Arc::new(|| Ok(()));
    let config = Arc::new(TilingConfig::new(Some(root.join("rc.xml")), Some(root.clone()), Some(root.clone()), Some(call.clone()),
                                            Some(reload), Some(root.join("tessel-tiling"))));
    let status_call = call.clone();
    let status: StatusCall = Arc::new(move || tessel_tiling::client::status_with(&*status_call));
    Fixture { root, state, config, status }
}

fn label(widget: &gtk::Label) -> String { widget.text().to_string() }

fn shortcut_popup_controls_and_search(runner: Runner) {
    let f = fixture(if runner == Runner::Inline { "inline" } else { "thread" });
    let (app, current) = build_application("top", 30, Some(f.config.clone()), Some(f.status.clone()), runner);
    app.register(None::<&gio::Cancellable>).unwrap();
    app.activate();
    assert!(wait_for(|| current.borrow().as_ref().is_some_and(|p| p.window().is_mapped()), 5.0), "popup never mapped");
    let popup = current.borrow().clone().unwrap();
    let window = popup.window().clone();
    assert!(window.has_css_class("tiling-shortcuts") && window.has_css_class("tray-popup"));
    assert_eq!(window.title().as_deref(), Some("Window Tiling"));
    assert_eq!(window.default_size().1, 620);
    assert!(window.width() <= 420, "width {}", window.width());
    assert!(window.child().unwrap().has_css_class("popup-surface"));
    assert_eq!((popup.controller().edge().as_str(), popup.controller().offset()), ("top", 30));
    let controls = popup.controls().clone();
    assert!(wait_for(|| label(popup.state_label()) == "On · New windows tile automatically.", 3.0), "{}", label(popup.state_label()));
    assert!(popup.state_label().is_visible());
    assert!(controls.enabled_switch().is_active() && controls.enabled_switch().is_sensitive());
    assert!(!controls.gapless().switch().is_active());
    assert!(controls.outline_switch().is_active());
    assert!(!controls.shortcuts().is_visible(), "the popup hides its own launcher button");
    assert_eq!(label(controls.notice()), "Sign out and back in to load the window parking update.");
    assert!(controls.notice().is_visible() && controls.sign_out().button.is_visible());
    for widget in [controls.notice()] {
        for class in ["settings-muted", "detail", "gs-row-subtitle"] {
            assert!(widget.has_css_class(class));
        }
    }
    assert!(controls.error().has_css_class("error"));
    // Shortcut rows and search.
    let rows = popup.rows();
    assert_eq!(rows.len(), BINDINGS.len() + EXTRA_SHORTCUTS.len());
    assert!(rows[0].has_css_class("shortcut-row"));
    let key = rows[0].first_child().unwrap().downcast::<gtk::Label>().unwrap();
    assert!(key.has_css_class("key"));
    assert_eq!(label(&key), BINDINGS[0].label);
    popup.search().set_text("WORKSPACE 3");
    assert!(wait_for(|| rows.iter().filter(|r| r.is_visible()).count() < 10, 2.0));
    let visible: Vec<String> = rows.iter().filter(|r| r.is_visible())
        .map(|r| label(&r.last_child().unwrap().downcast::<gtk::Label>().unwrap())).collect();
    assert_eq!(visible, ["Switch to workspace 3", "Move window to workspace 3 and follow", "Move window to workspace 3 without following"]);
    popup.search().set_text("");
    assert!(wait_for(|| rows.iter().all(|r| r.is_visible()), 2.0));
    // Gapless: immediate apply on the scratch rc.xml.
    controls.gapless().switch().set_active(true);
    assert!(wait_for(|| !controls.gapless().busy(), 3.0));
    assert!(controls.gapless().switch().is_active() && controls.gapless().switch().state());
    let rc = std::fs::read(f.root.join("rc.xml")).unwrap();
    assert_eq!(with_gap(&rc, 0).unwrap().1, 0);
    let prefs: Value = serde_json::from_str(&std::fs::read_to_string(f.root.join("tessel-tiling/config.json")).unwrap()).unwrap();
    assert_eq!(prefs, json!({"gapless": true, "gapped_spacing": 12}));
    // Tiling off, then a failing enable reverts the switch and shows the error.
    controls.enabled_switch().set_active(false);
    assert!(wait_for(|| !controls.busy() && !f.state.lock().unwrap().enabled, 3.0));
    assert!(wait_for(|| label(popup.state_label()) == "Off · Windows move and resize normally.", 3.0));
    assert!(!controls.enabled_switch().is_active());
    f.state.lock().unwrap().fail_enable = true;
    controls.enabled_switch().set_active(true);
    assert!(wait_for(|| controls.error().is_visible(), 3.0));
    assert_eq!(label(controls.error()), "Not applied");
    assert!(!controls.enabled_switch().is_active() && !controls.enabled_switch().state(), "a failed change reverts");
    assert_eq!(std::fs::read(f.root.join("rc.xml")).unwrap(), rc, "rc.xml restored after the failure");
    // Outline: unsupported compositor → saved, sign-out notice.
    f.state.lock().unwrap().outline_supported = false;
    controls.refresh();
    assert!(wait_for(|| label(controls.outline_detail()).ends_with("Takes effect after you sign out and back in."), 3.0));
    controls.outline_switch().set_active(false);
    assert!(wait_for(|| controls.outline_pending_sign_out(), 3.0));
    assert!(label(controls.notice()).contains("Saved. Sign out and back in to load the compositor outline update."));
    let prefs: Value = serde_json::from_str(&std::fs::read_to_string(f.root.join("tessel-tiling/config.json")).unwrap()).unwrap();
    assert_eq!(prefs["focus_outline"], false);
    // Same-instance activation toggles the popup.
    app.activate();
    assert!(wait_for(|| !window.is_visible(), 3.0), "activate should dismiss the visible popup");
    app.activate();
    assert!(wait_for(|| current.borrow().as_ref().is_some_and(|p| p.window().is_visible()), 3.0));
    assert!(popup.has_timer() || current.borrow().as_ref().unwrap().has_timer());
    let commands = f.state.lock().unwrap().commands.clone();
    assert!(!commands.iter().any(|c| c == "outline on" || c == "outline off"), "unsupported outline sends nothing: {commands:?}");
    app.quit();
    settle(0.1);
}

fn second_launch_activates_the_first_instance() {
    let f = fixture("remote");
    let (app, current) = build_application("bottom", 76, Some(f.config.clone()), Some(f.status.clone()), Runner::Inline);
    app.register(None::<&gio::Cancellable>).unwrap();
    assert!(!app.is_remote());
    let launch = |args: &[&str]| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tessel-tiling")).args(args).stdout(Stdio::null()).stderr(Stdio::piped())
            .spawn().unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < end, "second tessel-tiling did not return");
            settle(0.01);
        }
        child.wait().unwrap().code()
    };
    assert_eq!(launch(&["--settings", "--popup-edge", "left", "--popup-offset", "9"]), Some(0));
    assert!(wait_for(|| current.borrow().as_ref().is_some_and(|p| p.window().is_mapped()), 5.0));
    // Python passes only argv[0]: the first instance keeps its own placement.
    let popup = current.borrow().clone().unwrap();
    assert_eq!((popup.controller().edge().as_str(), popup.controller().offset()), ("bottom", 76));
    assert_eq!(launch(&["--popup"]), Some(0));
    assert!(wait_for(|| !popup.window().is_visible(), 3.0), "a second launch toggles the popup closed");
    let _: Map<String, Value> = Map::new();
    app.quit();
    settle(0.1);
}

fn main() {
    let Ok(case) = std::env::var("TILING_UI") else {
        eprintln!("ui_headless: skipped (run tests/run_headless.sh)");
        return;
    };
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(!runtime.is_empty() && runtime != "/run/user/1000", "refusing to run on the live session");
    tessel_core::i18n::initialize();
    gtk::init().unwrap();
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    match case.as_str() {
        "shortcut_popup_controls_and_search" => shortcut_popup_controls_and_search(Runner::Inline),
        "worker_thread_controls" => shortcut_popup_controls_and_search(Runner::Thread),
        "second_launch_activates_the_first_instance" => second_launch_activates_the_first_instance(),
        other => panic!("unknown case {other}"),
    }
    println!("ok {case}");
}
