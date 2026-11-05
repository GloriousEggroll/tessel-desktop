//! Port of test_session_ui.py (UITests): every session action is a fake.
//! Runs only inside the private nested headless labwc (tests/run_headless.sh);
//! under a plain `cargo test` it skips itself.

use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, LayerShell};
use tessel_session::app::{BackendFactory, Controller, SessionApplication};
use tessel_session::backend::{Backend, Capabilities, Done};
use tessel_session::end::{Ending, Finished, Inhibitor};
use tessel_session::windows::Window;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

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

#[derive(Default)]
struct FakeEnding {
    finished: RefCell<Option<Finished>>,
    done: Cell<bool>,
    windows: RefCell<Vec<Window>>,
    inhibitors: RefCell<Vec<Inhibitor>>,
    note: RefCell<String>,
    on_update: RefCell<Option<Rc<dyn Fn()>>>,
    closed: Cell<bool>,
}

impl FakeEnding {
    fn report(&self, windows: Vec<Window>, inhibitors: Vec<Inhibitor>, note: &str) {
        self.done.set(true);
        self.windows.replace(windows.clone());
        self.inhibitors.replace(inhibitors.clone());
        self.note.replace(note.into());
        let finished = self.finished.borrow().clone().unwrap();
        finished(windows, inhibitors, note.into());
    }
    fn update(&self) {
        let update = self.on_update.borrow().clone().expect("on_update");
        update();
    }
}

struct EndingHandle(Rc<FakeEnding>);

impl Ending for EndingHandle {
    fn done(&self) -> bool { self.0.done.get() }
    fn remaining(&self) -> Vec<Window> { self.0.windows.borrow().clone() }
    fn inhibitors(&self) -> Vec<Inhibitor> { self.0.inhibitors.borrow().clone() }
    fn note(&self) -> String { self.0.note.borrow().clone() }
    fn set_on_update(&self, on_update: Option<Rc<dyn Fn()>>) { self.0.on_update.replace(on_update); }
    fn close(&self) { self.0.closed.set(true) }
}

#[derive(Default)]
struct FakeBackend {
    capabilities: Cell<Capabilities>,
    can_logout: Cell<bool>,
    performed: RefCell<Vec<(String, Option<Done>)>>,
    endings: RefCell<Vec<Rc<FakeEnding>>>,
}

impl FakeBackend {
    fn last_action(&self) -> Option<String> { self.performed.borrow().last().map(|p| p.0.clone()) }
    fn complete(&self, ok: bool, error: &str) {
        let done = self.performed.borrow_mut().last_mut().unwrap().1.take().unwrap();
        done(ok, error.into());
    }
    fn ending(&self) -> Rc<FakeEnding> { self.endings.borrow().last().unwrap().clone() }
}

struct BackendHandle(Rc<FakeBackend>);

impl Backend for BackendHandle {
    fn start(&self) {}
    fn capabilities(&self) -> Capabilities { self.0.capabilities.get() }
    fn can_logout(&self) -> bool { self.0.can_logout.get() }
    fn error(&self) -> String { String::new() }
    fn perform(&self, action: &str, done: Done) { self.0.performed.borrow_mut().push((action.into(), Some(done))) }
    fn end_session(&self, _action: &str, finished: Finished) -> Rc<dyn Ending> {
        let ending = Rc::new(FakeEnding::default());
        ending.finished.replace(Some(finished));
        self.0.endings.borrow_mut().push(ending.clone());
        Rc::new(EndingHandle(ending))
    }
    fn close(&self) {}
}

struct Fx {
    app: SessionApplication,
    backend: Rc<FakeBackend>,
}

thread_local!(static COUNTER: Cell<u32> = const { Cell::new(0) });

fn setup() -> Fx {
    let backend = Rc::new(FakeBackend::default());
    backend.capabilities.set(Capabilities::all());
    backend.can_logout.set(true);
    let b = backend.clone();
    let factory: BackendFactory = Box::new(move |_changed| Rc::new(BackendHandle(b)) as Rc<dyn Backend>);
    let n = COUNTER.with(|c| { c.set(c.get() + 1); c.get() });
    let app = SessionApplication::new(&format!("org.tessel.SessionTest{n}"), factory);
    app.application().register(None::<&gtk::gio::Cancellable>).unwrap();
    app.show("bottom", 76, false);
    settle(0.12);
    Fx { app, backend }
}

impl Fx {
    fn popup(&self) -> Rc<tessel_session::app::SessionPopup> { self.app.popup().expect("popup") }
    fn confirm(&self, action: &str) -> Rc<FakeEnding> {
        self.popup().button(action).emit_clicked();
        self.popup().confirm();
        settle(0.12);
        self.backend.ending()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.app.shutdown();
        settle(0.12);
    }
}

fn window(app_id: &str, title: &str) -> Window { Window::new("1:1", app_id, title) }

fn children(widget: &gtk::Box) -> usize {
    let mut count = 0;
    let mut child = widget.first_child();
    while let Some(c) = child {
        count += 1;
        child = c.next_sibling();
    }
    count
}

fn logout_closes_apps_first_and_proceeds_when_all_closed() {
    let fx = setup();
    let ending = fx.confirm("logout");
    assert!(fx.backend.last_action().is_none());
    assert!(fx.app.busy());
    ending.report(vec![], vec![], "");
    assert_eq!(fx.backend.last_action().as_deref(), Some("logout"));
    assert!(ending.closed.get());
}

fn remaining_apps_listed_with_cancel_and_anyway() {
    let fx = setup();
    let popup = fx.popup();
    let ending = fx.confirm("restart");
    let editor = window("org.gnome.TextEditor", "notes.txt — unsaved");
    let inhibitor = Inhibitor { who: "Updater".into(), why: "Installing updates".into(), mode: "block".into(), pid: 1 };
    ending.report(vec![editor.clone()], vec![inhibitor], "");
    settle(0.12);
    assert!(popup.remaining.get_visible());
    assert_eq!(popup.anyway_button.label().as_deref(), Some("Restart anyway"));
    assert_eq!(children(&popup.remaining_list), 2);
    assert!(fx.backend.last_action().is_none());
    popup.remaining_cancel.emit_clicked();
    settle(0.12);
    assert!(!fx.app.busy());
    assert!(ending.closed.get());
    assert!(fx.backend.last_action().is_none());
    assert!(popup.actions.get_visible());
    let ending = fx.confirm("logout");
    ending.report(vec![editor], vec![], "");
    settle(0.12);
    popup.anyway_button.emit_clicked();
    assert_eq!(fx.backend.last_action().as_deref(), Some("logout"));
}

fn list_updates_and_continues_when_last_app_closes() {
    let fx = setup();
    let ending = fx.confirm("poweroff");
    ending.report(vec![window("", "Draft")], vec![], "");
    settle(0.12);
    assert!(fx.backend.last_action().is_none());
    ending.windows.replace(vec![]);
    ending.update();
    assert_eq!(fx.backend.last_action().as_deref(), Some("poweroff"));
}

fn dismissing_remaining_list_cancels() {
    let fx = setup();
    let ending = fx.confirm("logout");
    ending.report(vec![window("x", "y")], vec![], "");
    settle(0.12);
    fx.popup().controller().dismiss();
    settle(0.12);
    assert!(ending.closed.get());
    assert!(!fx.app.busy());
    assert!(fx.backend.last_action().is_none());
}

fn menu_mode_is_centered_and_keyboard_exclusive() {
    let fx = setup();
    assert!(matches!(tessel_session::args::parse(&["--menu"]), tessel_session::args::Outcome::Run(a) if a.menu));
    fx.app.show("bottom", 76, true);
    settle(1.2);
    let popup = fx.popup();
    assert!(matches!(popup.controller(), Controller::Menu(_)));
    let window = popup.window.upcast_ref::<gtk::Window>();
    if gtk4_layer_shell::is_supported() {
        assert!(![Edge::Top, Edge::Bottom, Edge::Left, Edge::Right].iter().any(|e| window.is_anchor(*e)));
        assert_eq!(window.keyboard_mode(), KeyboardMode::Exclusive);
        assert_eq!(window.namespace().as_deref(), Some("desktop-applet"));
    }
    assert!(window.get_visible());
    // Escape/outside click still dismiss and close the menu.
    popup.controller().dismiss();
    settle(0.12);
    assert!(fx.app.popup().is_none());
}

fn moving_to_another_edge_reshows_instead_of_closing() {
    let fx = setup();
    let popup = fx.popup();
    assert!(popup.window.get_visible());
    // set_position() hides before the asynchronous re-show; that hide must
    // not be taken for a dismissal that destroys the popup.
    fx.app.show("top", 35, false);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !popup.window.get_visible() && Instant::now() < deadline {
        settle(0.02);
    }
    settle(0.12);
    assert!(Rc::ptr_eq(&fx.app.popup().unwrap(), &popup));
    assert!(popup.window.get_visible());
    assert_eq!((popup.controller().edge(), popup.controller().offset()), ("top".to_owned(), 35));
    popup.controller().dismiss();
    settle(0.12);
    assert!(fx.app.popup().is_none(), "A real dismissal still closes the popup");
}

fn switch_user_hidden_without_display_manager() {
    let fx = setup();
    let popup = fx.popup();
    assert!(!popup.button("switch_user").get_visible());
    fx.backend.capabilities.set(Capabilities { switch_user: true, ..Capabilities::all() });
    fx.app.changed();
    assert!(popup.button("switch_user").get_visible());
    popup.button("switch_user").emit_clicked();
    assert_eq!(fx.backend.last_action().as_deref(), Some("switch_user"));
}

fn destructive_actions_require_confirmation_and_cancel_is_safe() {
    let fx = setup();
    let popup = fx.popup();
    for action in ["logout", "restart", "poweroff"] {
        popup.button(action).emit_clicked();
        assert!(popup.confirmation.get_visible());
        assert!(fx.backend.last_action().is_none());
        popup.cancel();
        assert!(popup.pending.borrow().is_none());
    }
    popup.button("poweroff").emit_clicked();
    popup.confirm();
    fx.backend.ending().report(vec![], vec![], "");
    assert_eq!(fx.backend.last_action().as_deref(), Some("poweroff"));
    assert!(fx.app.busy());
    settle(0.12); // Real failures arrive after the dismissal has been processed.
    fx.backend.complete(false, "Fixture denied");
    assert_eq!(popup.status.text(), "Fixture denied");
    assert!(popup.status.get_visible());
    settle(1.8); // Re-showing waits for the pointer probe and dock lookup.
    assert!(popup.window.get_visible());
}

fn capabilities_and_outside_click() {
    let fx = setup();
    let popup = fx.popup();
    fx.backend.capabilities.set(Capabilities { sleep: false, restart: false, ..Capabilities::all() });
    fx.app.changed();
    assert!(!popup.button("sleep").is_sensitive());
    assert!(!popup.button("restart").is_sensitive());
    assert!(popup.button("poweroff").is_sensitive());
    fx.backend.can_logout.set(false);
    fx.app.changed();
    assert!(!popup.button("logout").is_sensitive());
    popup.controller().dismiss(); // = _on_outside_click
    settle(0.12);
    assert!(fx.backend.last_action().is_none());
    assert!(fx.app.popup().is_none());
}

fn restart_confirmation_runs_only_restart() {
    let fx = setup();
    let popup = fx.popup();
    popup.button("restart").emit_clicked();
    assert_eq!(popup.confirm_button.label().as_deref(), Some("Restart"));
    assert_eq!(popup.question.text(), "Restart the PC? Save your work before continuing.");
    assert!(fx.backend.last_action().is_none());
    popup.confirm_button.emit_clicked();
    fx.backend.ending().report(vec![], vec![], "");
    assert_eq!(fx.backend.last_action().as_deref(), Some("restart"));
    assert_eq!(fx.backend.performed.borrow().len(), 1);
    fx.backend.complete(false, "Restart denied");
    assert_eq!(popup.status.text(), "Restart denied");
}

fn lock_button_and_error_recovery() {
    let fx = setup();
    let popup = fx.popup();
    popup.button("lock").emit_clicked();
    assert_eq!(fx.backend.last_action().as_deref(), Some("lock"));
    assert!(!popup.window.get_visible());
    settle(0.12);
    fx.backend.complete(false, "Fixture lock error");
    settle(1.8);
    assert!(popup.window.get_visible());
    assert!(!fx.app.busy());
}

fn remaining_view_texts_and_note() {
    let fx = setup();
    let popup = fx.popup();
    let ending = fx.confirm("logout");
    ending.report(vec![], vec![Inhibitor { who: "".into(), why: "".into(), mode: "delay".into(), pid: 2 },
                               Inhibitor { who: "Builder".into(), why: "".into(), mode: "block".into(), pid: 3 }], "");
    settle(0.12);
    assert_eq!(popup.remaining_title.text(), "Some apps are delaying this action");
    assert!(!popup.remaining_detail.get_visible());
    assert_eq!(popup.anyway_button.label().as_deref(), Some("Log out anyway"));
    let labels = collect_labels(popup.remaining_list.upcast_ref());
    assert_eq!(labels, ["Unknown application", "Delaying", "Builder", "Blocking"]);
    popup.remaining_cancel.emit_clicked();
    settle(0.12);
    let ending = fx.confirm("restart");
    ending.report(vec![window("", "")], vec![], "Open apps could not be checked.");
    settle(0.12);
    assert_eq!(popup.remaining_title.text(), "These apps are still open");
    assert_eq!(popup.remaining_detail.text(),
               "Open apps could not be checked. Save your work in these apps, or continue and lose unsaved changes.");
    assert_eq!(collect_labels(popup.remaining_list.upcast_ref()), ["Untitled window"]);
    // The CSS classes of the Python popup.
    assert!(popup.window.has_css_class("tessel-session") && popup.window.has_css_class("tray-popup"));
    assert!(popup.button("lock").has_css_class("session-action"));
    assert!(popup.confirm_button.has_css_class("destructive-action"));
    assert_eq!(popup.window.title().as_deref(), Some("Session"));
}

fn collect_labels(widget: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(label) = c.downcast_ref::<gtk::Label>() {
            out.push(label.label().to_string());
        }
        out.extend(collect_labels(&c));
        child = c.next_sibling();
    }
    out
}

/// "About This Mac": System Information is the first row, set apart from the
/// session actions, and opens Settings there without touching the backend.
fn system_information_row_opens_settings() {
    use std::os::unix::fs::PermissionsExt;
    let f = setup();
    let popup = f.popup();
    let first = popup.actions.first_child().expect("rows");
    assert_eq!(first, popup.system_info.clone().upcast::<gtk::Widget>(), "System Information comes first");
    assert!(first.next_sibling().is_some_and(|w| w.is::<gtk::Separator>()), "a separator follows it");
    assert!(popup.system_info.has_css_class("session-action"));
    assert_eq!(collect_labels(popup.system_info.upcast_ref()), ["System Information", "See this PC's hardware and software."]);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("session-ui-programs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("settings.log");
    let fake = dir.join("tessel-settings");
    std::fs::write(&fake, format!("#!/bin/sh\nprintf '%s ' \"$@\" > '{}'\n", log.display())).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("TESSEL_PROGRAM_DIR", &dir);
    popup.system_info.emit_clicked();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !log.exists() && std::time::Instant::now() < end {
        settle(0.05);
    }
    std::env::remove_var("TESSEL_PROGRAM_DIR");
    assert_eq!(std::fs::read_to_string(&log).unwrap_or_default().trim(), "--section systeminfo");
    settle(0.2);
    assert!(!popup.window.is_visible(), "the popup closes");
    assert!(f.backend.last_action().is_none(), "no session action ran");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The power glyph sits in the middle of its header tile.
fn header_glyph_is_centered() {
    let f = setup();
    let popup = f.popup();
    let tile = find_class(popup.window.upcast_ref(), "session-tile").expect("tile");
    let glyph = tile.first_child().expect("glyph");
    let bounds = glyph.compute_bounds(&tile).expect("bounds");
    let (cx, cy) = (bounds.x() + bounds.width() / 2.0, bounds.y() + bounds.height() / 2.0);
    assert!((cx - tile.width() as f32 / 2.0).abs() <= 1.0 && (cy - tile.height() as f32 / 2.0).abs() <= 1.0,
            "glyph center ({cx},{cy}) in a {}x{} tile", tile.width(), tile.height());
}

fn find_class(widget: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if widget.has_css_class(class) {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(found) = find_class(&c, class) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

fn main() {
    if std::env::var_os("SESSION_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run tests/run_headless.sh for the private headless labwc)");
        return;
    }
    gtk::init().expect("gtk init");
    let tests: [(&str, fn()); 14] = [
        ("system_information_row_opens_settings", system_information_row_opens_settings),
        ("header_glyph_is_centered", header_glyph_is_centered),
        ("logout_closes_apps_first_and_proceeds_when_all_closed", logout_closes_apps_first_and_proceeds_when_all_closed),
        ("remaining_apps_listed_with_cancel_and_anyway", remaining_apps_listed_with_cancel_and_anyway),
        ("list_updates_and_continues_when_last_app_closes", list_updates_and_continues_when_last_app_closes),
        ("dismissing_remaining_list_cancels", dismissing_remaining_list_cancels),
        ("menu_mode_is_centered_and_keyboard_exclusive", menu_mode_is_centered_and_keyboard_exclusive),
        ("moving_to_another_edge_reshows_instead_of_closing", moving_to_another_edge_reshows_instead_of_closing),
        ("switch_user_hidden_without_display_manager", switch_user_hidden_without_display_manager),
        ("destructive_actions_require_confirmation_and_cancel_is_safe", destructive_actions_require_confirmation_and_cancel_is_safe),
        ("capabilities_and_outside_click", capabilities_and_outside_click),
        ("restart_confirmation_runs_only_restart", restart_confirmation_runs_only_restart),
        ("lock_button_and_error_recovery", lock_button_and_error_recovery),
        ("remaining_view_texts_and_note", remaining_view_texts_and_note),
    ];
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let mut failed = 0;
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let result = std::panic::catch_unwind(test);
        println!("test {name} ... {}", if result.is_ok() { "ok" } else { "FAILED" });
        failed += result.is_err() as i32;
    }
    println!("ui_headless: {failed} failed");
    std::process::exit(failed.min(1));
}
