//! Port of test_lock_ui.py (LockTests, preview, RecoveryTests) plus real
//! ext-session-lock checks. Runs ONLY inside the private nested headless labwc
//! started by tests/run_headless.sh (two outputs, private D-Bus as session and
//! system bus, private HOME/XDG dirs). Under plain `cargo test` it skips.
//!
//! In-process checks inject a FakeAuthentication (test_lock_ui.py) and unlock
//! only through the app's real PAM-success path (fake `done(true)`): there is
//! no test unlock API. Subprocess checks run the real `tessel-lock` binary
//! copied next to a `tessel-lock-pam` wrapper that loads a FAKE libpam.so.0
//! (verified with ld.so tracing first). The system PAM stack is never used.
use gtk::prelude::*;
use gtk::{gdk, glib};
use tessel_lock::app::{App, Options};
use tessel_lock::auth::{AuthFactory, Callbacks, Transaction};
use tessel_lock::window::{account_display_name, block_clipboard, secret_text, LockWindow};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_gtk::gtk;
use tessel_locking::config::ConfigStore;
use tessel_locking::voice_state::monotonic;

// ---------------------------------------------------------------- guard

fn isolated() -> bool {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    std::env::var("LOCK_UI_ISOLATED_TEST").as_deref() == Ok("1") && !runtime.is_empty() && runtime != "/run/user/1000"
        && !runtime.starts_with("/run/user/1000/wayland") && !display.is_empty()
        && home.starts_with("/run/user/1000/cbt/") && runtime.starts_with("/run/user/1000/cbt/")
}

fn scratch() -> PathBuf { PathBuf::from(std::env::var("LOCK_TEST_DIR").expect("LOCK_TEST_DIR")) }

// ---------------------------------------------------------------- helpers

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

fn wait(timeout: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.01);
        if predicate() {
            return true;
        }
    }
    predicate()
}

fn run(program: &str, args: &[&str]) {
    let output = Command::new(program).args(args).stdout(Stdio::null()).stderr(Stdio::piped()).output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(output.status.success(), "{program} {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
}

/// FakeAuthentication (test_lock_ui.py) + the Recording variant: fingerprint
/// transactions send no prompt.
struct Fake {
    cb: Callbacks,
    waiting: Cell<bool>,
    cancelled: Cell<bool>,
    responses: RefCell<Vec<String>>,
    password_only: Cell<bool>,
    fingerprint: Cell<bool>,
}

impl Fake {
    fn done(&self, success: bool) { (self.cb.complete)(success) }
    fn prompt(&self, message: &str, echo: bool) { (self.cb.prompt)(message, echo) }
}

impl Transaction for Fake {
    fn start(&self) {
        self.waiting.set(true);
        if !self.fingerprint.get() {
            (self.cb.prompt)("Password:", false);
        }
    }
    fn respond(&self, text: &str) {
        self.waiting.set(false);
        self.responses.borrow_mut().push(text.to_string());
    }
    fn cancel(&self) { self.cancelled.set(true) }
    fn waiting(&self) -> bool { self.waiting.get() }
    fn set_password_only(&self, v: bool) { self.password_only.set(v) }
    fn password_only(&self) -> bool { self.password_only.get() }
    fn set_fingerprint(&self, v: bool) { self.fingerprint.set(v) }
    fn fingerprint(&self) -> bool { self.fingerprint.get() }
    fn as_any(&self) -> &dyn std::any::Any { self }
}

thread_local!(static CREATED: RefCell<Vec<Rc<dyn Transaction>>> = const { RefCell::new(Vec::new()) });

fn fake_factory() -> AuthFactory {
    Rc::new(|cb| {
        let fake: Rc<dyn Transaction> = Rc::new(Fake { cb, waiting: false.into(), cancelled: false.into(),
            responses: RefCell::new(vec![]), password_only: false.into(), fingerprint: false.into() });
        CREATED.with(|c| c.borrow_mut().push(fake.clone()));
        fake
    })
}

fn fake(t: &Rc<dyn Transaction>) -> &Fake { t.as_any().downcast_ref::<Fake>().expect("fake transaction") }
fn created() -> Vec<Rc<dyn Transaction>> { CREATED.with(|c| c.borrow().clone()) }

static mut SEQ: u32 = 0;

fn folder(name: &str) -> PathBuf {
    let dir = scratch().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn update(store: &ConfigStore, changes: Value) -> tessel_locking::config::LockConfig {
    store.update(changes.as_object().unwrap()).unwrap()
}

struct Fixture {
    app: Rc<App>,
    store: ConfigStore,
    dir: PathBuf,
}

fn application(dir: &Path, preview: bool) -> (Rc<App>, ConfigStore) {
    let store = ConfigStore::new(Some(dir.join("lock.json")));
    let app = App::new(Options { preview, auth_factory: fake_factory(), store: ConfigStore::new(Some(dir.join("lock.json"))),
        has_assistant: Some(false), fingerprint_policy: dir.join("no-fingerprint-policy"),
        password_only_policy: dir.join("no-password-policy") });
    // SAFETY: single-threaded test runner.
    let n = unsafe { SEQ += 1; SEQ };
    app.application.set_application_id(Some(&format!("org.tessel.TestLock{n}x{}", std::process::id())));
    app.application.register(None::<&gtk::gio::Cancellable>).expect("register");
    (app, store)
}

thread_local!(static CURRENT: RefCell<Option<Rc<App>>> = const { RefCell::new(None) });

fn setup(name: &str) -> Fixture {
    let dir = folder(name);
    let (app, store) = application(&dir, false);
    CURRENT.with(|c| *c.borrow_mut() = Some(app.clone()));
    app.application.activate();
    assert!(wait(3.0, || app.is_locked() && app.windows.borrow().len() == 2), "locked with two lock surfaces");
    settle(0.3);
    Fixture { app, store, dir }
}

fn first(app: &App) -> Rc<LockWindow> { app.window_list()[0].clone() }

fn wake(app: &App) {
    first(app).key(None, gdk::Key::space);
    settle(0.4);
}

/// Unlock through the real PAM-success path (fake done(true)), then shut down.
fn finish(fixture: Fixture) { release(fixture.app) }

fn release(app: Rc<App>) {
    CURRENT.with(|c| c.borrow_mut().take());
    if app.is_locked() && !app.window_list().is_empty() {
        app.cancel_authentication();
        app.ready();
        let window = first(&app);
        app.activity(&window);
        window.entry.set_sensitive(true);
        window.entry.set_text("fixture");
        window.submit();
        let auth = app.current_auth().expect("password transaction");
        fake(&auth).done(true);
        assert!(wait(3.0, || !app.is_locked()), "PAM success unlocks");
    }
    app.shutdown();
    settle(0.2);
    for window in app.window_list() {
        window.window.destroy();
    }
    settle(0.1);
}

fn controller<T: IsA<gtk::EventController> + IsA<glib::Object>>(widget: &impl IsA<gtk::Widget>) -> T {
    let list = widget.observe_controllers();
    (0..list.n_items()).filter_map(|i| list.item(i).and_downcast::<T>()).next().expect("controller")
}

fn toplevels() -> u32 { gtk::Window::toplevels().n_items() }

// ---------------------------------------------------------------- checks (LockTests)

/// Regression: a revealed password (eye toggle) can never reach the
/// clipboard (copy/cut keybindings and context menu emit these signals), and
/// the submitted text is read without an un-zeroed GString copy.
fn revealed_password_never_reaches_the_clipboard() {
    let f = setup("clipboard");
    let app = &f.app;
    wake(app);
    let window = first(app);
    let clipboard = gtk::prelude::WidgetExt::clipboard(&window.entry);
    // Control: an ordinary visible entry does copy (so the check below is meaningful).
    let control = gtk::Entry::new();
    control.set_text("control text");
    control.select_region(0, -1);
    control.delegate().unwrap().emit_by_name::<()>("copy-clipboard", &[]);
    assert!(clipboard.content().is_some(), "control entry copies");
    clipboard.set_content(None::<&gdk::ContentProvider>).unwrap();
    // The control can also be blocked by the same helper.
    block_clipboard(&control);
    control.delegate().unwrap().emit_by_name::<()>("copy-clipboard", &[]);
    assert!(clipboard.content().is_none());
    window.show_password.set_active(true);
    assert!(gtk::prelude::EntryExt::is_visible(&window.entry), "password revealed");
    window.entry.set_text("fixture secret é");
    window.entry.select_region(0, -1);
    for signal in ["copy-clipboard", "cut-clipboard"] {
        window.entry.delegate().unwrap().emit_by_name::<()>(signal, &[]);
        settle(0.05);
        assert!(clipboard.content().is_none(), "{signal} put the password on the clipboard");
    }
    assert_eq!(window.entry.text(), "fixture secret é", "cut did not remove text either");
    assert_eq!(secret_text(&window.entry).as_str(), "fixture secret é");
    window.entry.set_text("");
    assert_eq!(secret_text(&window.entry).as_str(), "");
    window.show_password.set_active(false);
    finish(f);
}

fn all_monitors_covered_and_escape_cannot_unlock() {
    let f = setup("escape");
    for window in f.app.window_list() {
        let geometry = window.monitor.geometry();
        assert_eq!((window.window.width(), window.window.height()), (geometry.width(), geometry.height()));
        assert!(!window.assistant.get_visible());
        assert!(!window.voice.widget().get_visible());
        window.key(None, gdk::Key::Escape);
        window.window.close();
        settle(0.1);
        assert!(f.app.is_locked());
    }
    assert_eq!(f.app.windows.borrow().len(), 2);
    finish(f);
}

fn failure_cancellation_stale_success_and_valid_authentication() {
    let f = setup("failure");
    let app = &f.app;
    wake(app);
    let window = first(app);
    window.entry.set_text("fixture response");
    window.submit();
    let auth = app.current_auth().unwrap();
    assert_eq!(*fake(&auth).responses.borrow(), ["fixture response"]);
    assert_eq!(window.entry.text(), "");
    app.cancel_authentication();
    assert!(fake(&auth).cancelled.get());
    fake(&auth).done(true);
    assert!(app.is_locked(), "Stale authentication must not unlock");
    window.entry.set_text("wrong fixture");
    window.submit();
    fake(&app.current_auth().unwrap()).done(false);
    assert!(app.is_locked());
    assert!(window.status.text().contains("failed"));
    app.ready();
    window.entry.set_text("correct fixture");
    window.submit();
    fake(&app.current_auth().unwrap()).done(true);
    settle(0.1);
    assert!(!app.is_locked());
    assert!(app.authorized.get());
    finish(f);
}

fn print_screen_does_not_reveal_login_or_type_password() {
    let f = setup("print");
    assert!(!f.app.login_visible.get());
    for window in f.app.window_list() {
        assert!(window.key(None, gdk::Key::Print));
        assert_eq!(window.entry.text(), "");
    }
    assert_eq!(f.app.capture_requests.get(), 2);
    settle(0.2); // NO_AUTO_START: no Capture service on the private bus.
    assert!(f.app.is_locked());
    assert!(!f.app.login_visible.get());
    finish(f);
}

fn starts_with_wallpaper_and_native_input_works_on_either_monitor() {
    let f = setup("native");
    let app = &f.app;
    assert!(!app.login_visible.get(), "Mapping under the cursor must not reveal the login");
    for window in app.window_list() {
        assert_eq!(window.login_layer.progress(), 0.0);
        assert!(window.wallpaper_content.is_mapped());
        assert!(!window.entry.is_sensitive() || !window.entry.is_mapped());
    }
    run("wtype", &["-k", "space"]);
    assert!(wait(1.0, || app.login_visible.get()));
    settle(0.4);
    assert!(app.current_auth().is_none());
    for (index, window) in app.window_list().into_iter().enumerate() {
        let geometry = window.monitor.geometry();
        let bounds = window.entry.compute_bounds(&window.window).unwrap();
        let x = geometry.x() as f32 + bounds.x() + bounds.width() / 3.0;
        let y = geometry.y() as f32 + bounds.y() + bounds.height() / 2.0;
        run("wlrctl", &["pointer", "move", "-100000", "-100000"]);
        run("wlrctl", &["pointer", "move", &(x as i32).to_string(), &(y as i32).to_string()]);
        settle(0.1);
        run("wlrctl", &["pointer", "click"]);
        settle(0.1);
        let text = format!("fixture{index}");
        run("wtype", &[&text]);
        settle(0.15);
        assert_eq!(window.entry.text(), text, "monitor {index} receives keyboard input");
        assert!(!gtk::prelude::EntryExt::is_visible(&window.entry));
        assert!(app.window_list().iter().filter(|o| !Rc::ptr_eq(o, &window)).all(|o| o.entry.text().is_empty()));
        run("wtype", &["-k", "BackSpace", "X"]);
        settle(0.15);
        assert_eq!(window.entry.text(), format!("{}X", &text[..text.len() - 1]));
        let text = format!("{text}é!");
        run("wtype", &["-M", "ctrl", "-k", "a", "-m", "ctrl"]);
        run("wtype", &[&text]);
        settle(0.15);
        assert_eq!(window.entry.text(), text);
        run("wtype", &["-k", "Return"]);
        settle(0.15);
        assert!(app.auth_window().is_some_and(|w| Rc::ptr_eq(&w, &window)));
        assert_eq!(*fake(&app.current_auth().unwrap()).responses.borrow(), [text]);
        app.cancel_authentication();
        assert!(app.is_locked());
    }
    finish(f);
}

fn touch_keyboard_edits_the_selected_lock_field_without_ipc() {
    let f = setup("touch");
    let window = first(&f.app);
    f.app.activity(&window);
    let touch = window.keyboard().expect("touch keyboard");
    settle(0.4);
    assert_eq!(touch.button().parent().as_ref(), Some(window.actions.upcast_ref::<gtk::Widget>()));
    assert!(touch.button().is_mapped());
    touch.button().emit_clicked();
    settle(0.2);
    let widget = touch.widget().expect("keyboard widget");
    assert!(widget.root().is_mapped());
    assert!(window.scroll.margin_bottom() > 0);
    widget.press(30);
    widget.modifier(1);
    widget.press(48);
    widget.character("7");
    assert_eq!(window.entry.text(), "aB7");
    widget.press(14);
    assert_eq!(window.entry.text(), "aB");
    widget.hide_button().emit_clicked();
    settle(0.1);
    assert!(!widget.root().get_visible());
    assert_eq!(window.scroll.margin_bottom(), 0);
    assert!(f.app.is_locked());
    touch.show();
    window.entry.set_sensitive(false);
    widget.press(30);
    assert_eq!(window.entry.text(), "aB");
    finish(f);
}

fn touch_keyboard_dead_keys_remain_inside_lock_form() {
    let config = tessel_keyboard::config::default_path();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    tessel_keyboard::config::Config::new(&config).save(&json!({"layout": "fr"})).unwrap();
    let f = setup("deadkeys");
    let window = first(&f.app);
    f.app.activity(&window);
    let touch = window.keyboard().unwrap();
    touch.show();
    settle(0.2);
    let widget = touch.widget().unwrap();
    widget.press(26);
    assert_eq!(window.entry.text(), "");
    widget.press(18);
    assert_eq!(window.entry.text(), "ê");
    widget.press(26);
    widget.press(57);
    assert_eq!(window.entry.text(), "ê^");
    assert!(f.app.is_locked());
    let _ = std::fs::remove_file(&config);
    finish(f);
}

fn start_fingerprint(f: &Fixture) {
    for name in ["tessel-lock-fingerprint", "tessel-lock-password"] {
        std::fs::write(f.dir.join(name), "fixture\n").unwrap();
    }
    *f.app.fingerprint_policy.borrow_mut() = f.dir.join("tessel-lock-fingerprint");
    *f.app.password_only_policy.borrow_mut() = f.dir.join("tessel-lock-password");
    f.app.fingerprint_found("Touch the fingerprint reader");
}

fn fingerprint_is_a_parallel_pam_transaction() {
    let f = setup("fp-parallel");
    let app = &f.app;
    let window = first(app);
    assert!(app.current_fingerprint().is_none(), "no detector result on the private bus");
    let before = created().len();
    start_fingerprint(&f);
    let fingerprint = app.current_fingerprint().unwrap();
    assert!(Rc::ptr_eq(&created()[before], &fingerprint));
    assert!(fingerprint.fingerprint());
    assert!(window.status_row.fingerprint.get_visible());
    assert_eq!(window.fingerprint_hint.text(), "Touch the fingerprint reader");
    wake(app);
    window.entry.set_text("typed while reader waits");
    window.submit();
    let password = app.current_auth().unwrap();
    assert!(password.password_only());
    assert!(!password.fingerprint());
    assert_eq!(*fake(&password).responses.borrow(), ["typed while reader waits"]);
    fake(&fingerprint).done(true);
    settle(0.1);
    assert!(fake(&password).cancelled.get());
    assert!(!app.is_locked());
    assert!(app.authorized.get());
    finish(f);
}

fn password_success_cancels_fingerprint_and_failures_restart() {
    let f = setup("fp-restart");
    let app = &f.app;
    let window = first(app);
    let before = created().len();
    start_fingerprint(&f);
    let first_fp = app.current_fingerprint().unwrap();
    fake(&first_fp).done(false);
    assert_eq!(app.fingerprint_failures.get(), 1);
    assert_eq!(window.fingerprint_hint.text(), "Fingerprint not recognized. Try again.");
    assert!(app.is_locked());
    settle(1.3);
    let second = app.current_fingerprint().unwrap();
    assert!(!Rc::ptr_eq(&second, &first_fp));
    assert!(second.fingerprint());
    fake(&second).prompt("Password:", false);
    assert!(fake(&second).cancelled.get(), "a fingerprint prompt fails closed");
    assert!(app.current_fingerprint().is_none());
    app.fingerprint_begin();
    let third = app.current_fingerprint().unwrap();
    wake(app);
    window.entry.set_text("correct fixture");
    window.submit();
    fake(&app.current_auth().unwrap()).done(true);
    settle(0.1);
    assert!(fake(&third).cancelled.get());
    assert!(!app.is_locked());
    assert_eq!(created()[before..].iter().filter(|a| a.fingerprint()).count(), 3);
    finish(f);
}

fn fingerprint_stops_after_repeated_non_matches() {
    let f = setup("fp-stop");
    let app = &f.app;
    let window = first(app);
    start_fingerprint(&f);
    for _ in 0..tessel_lock::app::FINGERPRINT_ATTEMPTS {
        app.fingerprint_begin();
        fake(&app.current_fingerprint().unwrap()).done(false);
    }
    assert!(app.current_fingerprint().is_none());
    assert!(app.fingerprint_restart.borrow().is_none());
    assert_eq!(window.fingerprint_hint.text(), "Too many fingerprint attempts. Use your password.");
    assert!(app.is_locked());
    finish(f);
}

fn without_fingerprint_policy_password_uses_tessel_lock() {
    let f = setup("fp-none");
    let app = &f.app;
    let window = first(app);
    app.start_fingerprint();
    assert!(app.fingerprint.borrow().is_none());
    wake(app);
    window.entry.set_text("fixture");
    window.submit();
    assert!(!app.current_auth().unwrap().password_only());
    assert!(app.is_locked());
    finish(f);
}

fn battery_and_notification_count_are_privacy_safe() {
    let f = setup("status");
    let app = &f.app;
    let window = first(app);
    app.status.set_battery(Some(("battery-good-symbolic".into(), "64% · Charging".into())));
    app.status.set_notifications(json!({"unread": 2, "notifications": [{"app_name": "Mail", "summary": "Private subject", "read": false}]}));
    app.information_changed();
    assert_eq!(window.status_row.battery_label.text(), "64% · Charging");
    assert_eq!(window.status_row.notifications_label.text(), "2 new notifications");
    *app.config.borrow_mut() = update(&f.store, json!({"notification_app_names": true}));
    app.information_changed();
    assert_eq!(window.status_row.notifications_label.text(), "2 new notifications · Mail");
    *app.config.borrow_mut() = update(&f.store, json!({"show_battery": false, "show_notifications": false}));
    app.information_changed();
    assert!(!window.status_row.widget.get_visible());
    assert!(app.is_locked());
    finish(f);
}

fn appearance_changes_live_and_voice_is_conditional() {
    let f = setup("appearance");
    let app = &f.app;
    let window = first(app);
    update(&f.store, json!({"follow_appearance": false, "blur_strength": 70, "transparency": 25}));
    app.tick_windows();
    assert!(window.signature.borrow().contains("0.750"));
    app.has_assistant.set(true);
    window.refresh();
    assert!(window.assistant.get_visible());
    assert_eq!(window.assistant.text(), "KASUMI");
    update(&f.store, json!({"show_assistant_name": false, "show_voice": false}));
    app.tick_windows();
    assert!(!window.assistant.get_visible());
    assert!(!window.voice.widget().get_visible());
    finish(f);
}

fn idle_hides_only_login_cancels_auth_and_keyboard_reveals_all_monitors() {
    let f = setup("idle");
    let app = &f.app;
    wake(app);
    let window = first(app);
    window.entry.set_text("pending fixture");
    window.submit();
    let auth = app.current_auth().unwrap();
    app.last_activity.set(monotonic() - 29.0);
    app.tick_windows();
    assert!(app.login_visible.get());
    app.last_activity.set(monotonic() - 31.0);
    app.tick_windows();
    settle(0.5);
    assert!(!app.login_visible.get());
    assert!(fake(&auth).cancelled.get());
    fake(&auth).done(true);
    assert!(app.is_locked());
    for item in app.window_list() {
        assert_eq!(item.login_layer.progress(), 0.0);
        assert!(item.background.is_mapped());
        assert!(item.window.get_visible());
        assert!(!item.login_content.is_sensitive() || !item.login_content.is_mapped());
        assert_eq!(item.entry.text(), "");
        assert!(item.wallpaper_content.is_mapped());
        assert!(item.clock.is_mapped());
        assert!(item.date.is_mapped());
        assert!(!item.clock.is_ancestor(&item.login_content));
    }
    assert!(window.key(None, gdk::Key::Return));
    assert!(app.current_auth().is_none(), "The wake-up key must not submit a password");
    settle(0.5);
    assert!(app.login_visible.get());
    assert!(app.is_locked());
    let focused = gtk::prelude::GtkWindowExt::focus(&window.window);
    assert!(focused.is_some_and(|w| w == *window.entry.upcast_ref::<gtk::Widget>() || w.is_ancestor(&window.entry)));
    for item in app.window_list() {
        assert_eq!(item.login_layer.progress(), 1.0);
        assert!(item.login_content.is_sensitive());
        assert!(!item.wallpaper_content.is_mapped());
        assert!(!item.clock.is_mapped());
    }
    finish(f);
}

fn pointer_wake_and_hints_use_the_existing_secure_surface() {
    let f = setup("hints");
    let app = &f.app;
    wake(app);
    let window = first(app);
    app.can_sleep.set(true);
    *app.greeter.borrow_mut() = Some("/fixture/seat".into());
    window.tick();
    settle(0.1);
    assert_eq!(window.actions.first_child().as_ref(), Some(window.unlock_button.upcast_ref::<gtk::Widget>()));
    assert_eq!(window.unlock_button.next_sibling().as_ref(), Some(window.sleep.upcast_ref::<gtk::Widget>()));
    assert_eq!(window.sleep.next_sibling().as_ref(), Some(window.switch_user.upcast_ref::<gtk::Widget>()));
    let count = toplevels();
    let buttons: [(gtk::Widget, &str); 4] = [(window.unlock_button.clone().upcast(), "Unlock"), (window.sleep.clone().upcast(), "Sleep"),
        (window.switch_user.clone().upcast(), "Switch User"), (window.show_password.clone().upcast(), "Show password")];
    for (button, text) in buttons {
        let hover: gtk::EventControllerMotion = controller(&button);
        hover.emit_by_name::<()>("enter", &[&10.0f64, &10.0f64]);
        settle(0.25);
        assert_eq!(window.hint.text(), text);
        assert!(window.hint.is_mapped());
        assert!(!button.has_tooltip());
        assert_eq!(toplevels(), count, "hints never create a separate surface");
        hover.emit_by_name::<()>("leave", &[]);
        assert!(!window.hint.get_visible());
    }
    app.last_activity.set(monotonic() - 31.0);
    app.tick_windows();
    settle(0.5);
    window.pointer_activity(20.0, 20.0);
    settle(0.5);
    assert!(app.login_visible.get());
    assert!(app.is_locked());
    assert_eq!(window.login_layer.progress(), 1.0);
    finish(f);
}

fn action_sizes_and_native_mouse_wake() {
    let f = setup("actions");
    let app = &f.app;
    wake(app);
    app.can_sleep.set(true);
    *app.greeter.borrow_mut() = Some("/fixture/seat".into());
    for window in app.window_list() {
        window.tick();
    }
    settle(0.3);
    let window = first(app);
    let buttons: [gtk::Widget; 4] = [window.unlock_button.clone().upcast(), window.show_password.clone().upcast(),
                                     window.sleep.clone().upcast(), window.switch_user.clone().upcast()];
    let sizes: Vec<(i32, i32)> = buttons.iter().map(|b| (b.width(), b.height())).collect();
    let _ = Command::new("grim").arg(scratch().join("tessel-lock-action-sizes.png")).status();
    assert!(sizes.windows(2).all(|w| w[0] == w[1]), "{sizes:?}");
    assert!(window.avatar.pixel_size() >= 96);
    assert!(!window.prompt.get_visible());
    assert!(window.entry.placeholder_text().is_none());
    let bounds = window.show_password.compute_bounds(&window.password_field).unwrap();
    assert!(bounds.x() > window.password_field.width() as f32 / 2.0);
    assert!(bounds.y() >= 0.0);
    assert!(bounds.x() + bounds.width() <= window.password_field.width() as f32);
    assert!(bounds.y() + bounds.height() <= window.password_field.height() as f32);
    window.show_password.set_size_request(52, 52);
    settle(0.15);
    let grown: Vec<(i32, i32)> = buttons.iter().map(|b| (b.width(), b.height())).collect();
    assert!(grown.windows(2).all(|w| w[0] == w[1]), "{grown:?}");
    assert!(window.sleep.width() > sizes[0].0);
    window.show_password.set_size_request(44, 44);
    let mut geometry = window.monitor.geometry();
    for item in app.window_list() {
        geometry = item.monitor.geometry();
        run("wlrctl", &["pointer", "move", "-100000", "-100000"]);
        run("wlrctl", &["pointer", "move", &(geometry.x() + 40).to_string(), &(geometry.y() + 40).to_string()]);
        settle(0.2);
        app.last_activity.set(monotonic() - 31.0);
        app.tick_windows();
        settle(0.5);
        assert!(!app.login_visible.get());
        run("wlrctl", &["pointer", "move", "20", "10"]);
        assert!(wait(0.8, || app.login_visible.get()), "Real mouse movement must reveal the login");
        settle(0.5);
        assert!(app.is_locked());
        for other in app.window_list() {
            assert_eq!(other.login_layer.progress(), 1.0);
        }
        app.last_activity.set(monotonic() - 31.0);
        app.tick_windows();
        settle(0.5);
        assert!(!app.login_visible.get(), "A stationary pointer must allow idle hiding");
        run("wlrctl", &["pointer", "click"]);
        assert!(wait(0.8, || app.login_visible.get()));
        settle(0.5);
        assert!(app.current_auth().is_none());
        assert!(app.is_locked());
    }
    let window = app.window_list().last().unwrap().clone();
    let bounds = window.show_password.compute_bounds(&window.window).unwrap();
    let x = geometry.x() as f32 + bounds.x() + bounds.width() / 2.0;
    let y = geometry.y() as f32 + bounds.y() + bounds.height() / 2.0;
    run("wlrctl", &["pointer", "move", "-100000", "-100000"]);
    run("wlrctl", &["pointer", "move", &(x as i32).to_string(), &(y as i32).to_string()]);
    settle(0.1);
    run("wlrctl", &["pointer", "click"]);
    settle(0.1);
    assert!(window.show_password.is_active(), "capture controllers keep ordinary buttons working");
    finish(f);
}

fn account_fallback_and_extra_authentication_prompts() {
    let f = setup("prompts");
    let app = &f.app;
    wake(app);
    assert_eq!(account_display_name(Some("  Full Name  ,Office"), "login"), "Full Name");
    for missing in [Some(""), Some(" "), Some(",Office"), None] {
        assert_eq!(account_display_name(missing, "login"), "login");
    }
    let window = first(app);
    window.entry.set_text("fixture response");
    window.submit();
    assert!(!window.prompt.get_visible(), "Ordinary PAM password prompts stay visually hidden");
    fake(&app.current_auth().unwrap()).prompt("Verification code:", false);
    assert!(window.prompt.get_visible());
    assert_eq!(window.prompt.text(), "Verification code:");
    app.cancel_authentication();
    assert!(!window.prompt.get_visible());
    assert!(app.is_locked());
    finish(f);
}

fn solid_png(path: &Path, rgb: [u8; 3]) {
    let mut data = Vec::with_capacity(128 * 96 * 3);
    for _ in 0..128 * 96 {
        data.extend_from_slice(&rgb);
    }
    let texture = gdk::MemoryTexture::new(128, 96, gdk::MemoryFormat::R8g8b8, &glib::Bytes::from(&data), 128 * 3);
    texture.save_to_png(path).unwrap();
}

fn palette_follows_each_lockscreen_wallpaper_and_slideshow() {
    let f = setup("palette");
    let app = &f.app;
    let (red, green) = (f.dir.join("red.png"), f.dir.join("green.png"));
    solid_png(&red, [230, 20, 30]);
    solid_png(&green, [15, 210, 50]);
    let windows = app.window_list();
    let wallpapers = tessel_core::paths::component_config_dir("shell").join("wallpapers.json");
    std::fs::create_dir_all(wallpapers.parent().unwrap()).unwrap();
    let mut map = serde_json::Map::new();
    for (window, path) in windows.iter().zip([&red, &green]) {
        map.insert(window.monitor.connector().unwrap().to_string(), json!({"file_path": path}));
    }
    std::fs::write(&wallpapers, Value::Object(map).to_string()).unwrap();
    *app.config.borrow_mut() = update(&f.store, json!({"wallpaper_mode": "desktop"}));
    for window in &windows {
        window.refresh();
    }
    assert!(wait(3.0, || windows[0].background.palette_color() == (230, 20, 30) && windows[1].background.palette_color() == (15, 210, 50)),
            "{:?} {:?}", windows[0].background.palette_color(), windows[1].background.palette_color());
    settle(0.2);
    for window in &windows {
        window.refresh();
    }
    assert_ne!(*windows[0].signature.borrow(), *windows[1].signature.borrow());
    *app.config.borrow_mut() = update(&f.store, json!({"wallpaper_mode": "picture", "picture": green}));
    windows[0].refresh();
    assert!(wait(3.0, || windows[0].background.palette_color() == (15, 210, 50)));
    *app.config.borrow_mut() = update(&f.store, json!({"wallpaper_mode": "slideshow", "slideshow": [red, green]}));
    windows[0].refresh();
    settle(1.0);
    let before = windows[0].background.palette_color();
    windows[0].background.set_next_rotation(0.0);
    windows[0].refresh();
    assert!(wait(3.0, || windows[0].background.palette_color() != before));
    *app.config.borrow_mut() = update(&f.store, json!({"wallpaper_mode": "color", "background_color": "#235689"}));
    windows[0].refresh();
    assert!(wait(1.0, || windows[0].background.palette_color() == (35, 86, 137)));
    let _ = std::fs::remove_file(&wallpapers);
    finish(f);
}

fn monitor_hotplug_gets_a_lock_surface_and_removal_keeps_the_lock() {
    let f = setup("hotplug");
    let app = &f.app;
    let connector = app.window_list()[1].monitor.connector().unwrap().to_string();
    run("wlr-randr", &["--output", &connector, "--off"]);
    assert!(wait(3.0, || app.windows.borrow().len() == 1), "removed output's surface is destroyed");
    assert!(app.is_locked(), "removing an output never unlocks");
    run("wlr-randr", &["--output", &connector, "--on"]);
    assert!(wait(3.0, || app.windows.borrow().len() == 2), "a new output gets a lock surface");
    settle(0.3);
    let window = app.window_list()[1].clone();
    assert_eq!(window.monitor.connector().unwrap(), connector);
    let geometry = window.monitor.geometry();
    assert!(wait(2.0, || (window.window.width(), window.window.height()) == (geometry.width(), geometry.height())));
    assert!(app.is_locked());
    finish(f);
}

/// lock_dashboard.py: the media group follows the media row's own `visible`
/// flag (Python get_visible). Exercises crates/locking WallpaperDashboard.
fn dashboard_media_group_follows_media_row() {
    let f = setup("dashboard");
    let window = first(&f.app);
    window.media_row.set_visible(true);
    assert!(window.information_layout.media_group().get_visible(), "media group shown with the media row");
    window.media_row.set_visible(false);
    assert!(!window.information_layout.media_group().get_visible());
    finish(f);
}

// ---------------------------------------------------------------- preview

fn preview_is_explicitly_unlocked_and_has_no_authentication() {
    let dir = folder("preview");
    let (app, store) = application(&dir, true);
    update(&store, json!({"show_media": true, "pause_media": true}));
    *app.config.borrow_mut() = store.load().unwrap();
    app.application.activate();
    settle(1.0);
    let window = first(&app);
    assert!(!window.entry.is_sensitive());
    assert!(window.status.text().contains("not locked"));
    assert!(!app.has_lock());
    let close = window.preview_close.as_ref().expect("preview close");
    assert_eq!(close.label().as_deref(), Some("Preview · Close"));
    assert!(close.has_css_class("family-preview"));
    assert!(window.clock.has_css_class("family-clock"));
    assert!(window.entry.has_css_class("family-password"));
    assert_eq!(window.date.text(), tessel_locking::visuals::long_date(&glib::DateTime::now_local().unwrap()));
    assert_eq!(window.avatar.pixel_size(), tessel_locking::visuals::AVATAR_SIZE);
    assert!(!window.avatar.text().is_empty());
    assert!(!app.login_visible.get());
    assert!(window.wallpaper_content.is_mapped());
    window.toggle_media();
    let _ = Command::new("grim").arg(scratch().join("tessel-lock-preview.png")).status();
    assert!(window.window.width() > 500);
    app.last_activity.set(monotonic() - 31.0);
    app.tick_windows();
    settle(0.5);
    assert_eq!(window.login_layer.progress(), 0.0);
    window.key(None, gdk::Key::space);
    settle(0.5);
    assert!(!window.entry.is_sensitive());
    assert!(window.status.text().contains("not locked"));
    window.entry.set_text("x");
    window.submit();
    assert!(app.current_auth().is_none(), "preview never authenticates");
    app.can_sleep.set(true);
    *app.greeter.borrow_mut() = Some("/fixture/seat".into());
    window.tick();
    settle(0.1);
    window.request_hint(window.switch_user.upcast_ref());
    settle(0.25);
    assert_eq!(window.hint.text(), "Switch User");
    let _ = Command::new("grim").arg(scratch().join("tessel-lock-tooltip.png")).status();
    app.shutdown();
    window.window.destroy();
    settle(0.1);
}

// ---------------------------------------------------------------- real locker (subprocess)

const FAKE_PAM: &str = include_str!("support/fake_pam.c");

/// tessel-lock copied next to a tessel-lock-pam wrapper that loads the fake libpam.
fn locker_dir() -> PathBuf {
    let dir = scratch().join("lockbin");
    if dir.join("tessel-lock").is_file() {
        return dir;
    }
    std::fs::create_dir_all(&dir).unwrap();
    let pam = scratch().join("fakepam");
    std::fs::create_dir_all(&pam).unwrap();
    std::fs::write(pam.join("fake_pam.c"), FAKE_PAM).unwrap();
    std::fs::write(pam.join("libpam.map"), "LIBPAM_1.0 { global: pam_*; fake_pam_marker; local: *; };\n").unwrap();
    let status = Command::new("cc").args(["-shared", "-fPIC", "-Wall", "-Werror", "-Wl,-soname,libpam.so.0"])
        .arg(format!("-Wl,--version-script={}", pam.join("libpam.map").display())).arg("-o").arg(pam.join("libpam.so.0"))
        .arg(pam.join("fake_pam.c")).status().unwrap();
    assert!(status.success());
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_tessel-lock-pam"));
    // ld.so must resolve libpam.so.0 to the fake before anything else runs.
    let trace = Command::new(&helper).env("LD_TRACE_LOADED_OBJECTS", "1").env("LD_LIBRARY_PATH", &pam).output().unwrap();
    let text = String::from_utf8_lossy(&trace.stdout);
    assert!(text.lines().any(|l| l.contains("libpam.so.0") && l.contains(pam.to_str().unwrap())), "fake libpam not used: {text}");
    std::fs::write(dir.join("tessel-lock-pam"), format!("#!/bin/sh\nexport LD_LIBRARY_PATH='{}'\nexec '{}' \"$@\"\n",
                                                     pam.display(), helper.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.join("tessel-lock-pam"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let binary = std::env::var("LOCK_SCREEN_BIN").ok().filter(|b| !b.is_empty())
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_tessel-lock").to_string());
    std::fs::copy(binary, dir.join("tessel-lock")).unwrap();
    dir
}

struct Locker {
    child: Child,
    notify: UnixDatagram,
    started: Instant,
}

fn spawn_locker(program: &[String], name: &str, extra: &[(&str, &str)]) -> Locker {
    let address = scratch().join(format!("{name}.sock"));
    let _ = std::fs::remove_file(&address);
    let notify = UnixDatagram::bind(&address).unwrap();
    notify.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut command = Command::new(&program[0]);
    command.args(&program[1..]).env("NOTIFY_SOCKET", &address).stdout(Stdio::null()).stderr(Stdio::null());
    for (k, v) in extra {
        command.env(k, v);
    }
    let started = Instant::now();
    Locker { child: command.spawn().unwrap(), notify, started }
}

impl Locker {
    fn ready(&self) -> Option<Duration> {
        let mut buffer = [0u8; 64];
        let size = self.notify.recv(&mut buffer).ok()?;
        (&buffer[..size] == b"READY=1").then(|| self.started.elapsed())
    }
    fn alive(&mut self) -> bool { self.child.try_wait().unwrap().is_none() }
    fn wait_exit(&mut self, seconds: f64) -> Option<i32> {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < end {
            if let Some(status) = self.child.try_wait().unwrap() {
                use std::os::unix::process::ExitStatusExt;
                return Some(status.code().unwrap_or(128 + status.signal().unwrap_or(0)));
            }
            settle(0.02);
        }
        None
    }
    fn signal(&mut self, signal: i32) {
        // SAFETY: our own child process.
        unsafe { libc::kill(self.child.id() as i32, signal) };
    }
    /// VmRSS / RssAnon in kB from /proc/<pid>/status (smaps_rollup is not
    /// readable: the locker is PR_SET_DUMPABLE 0).
    fn rss(&self) -> (u64, u64) {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap_or_default();
        let field = |name: &str| status.lines().find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0);
        (field("VmRSS:"), field("RssAnon:"))
    }
}

/// Count clearly red pixels in a grim screenshot of every output.
fn red_pixels(name: &str) -> usize {
    let path = scratch().join(format!("{name}.png"));
    run("grim", &[path.to_str().unwrap()]);
    let texture = gdk::Texture::from_filename(&path).unwrap();
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    let mut count = 0;
    for y in 0..h {
        for x in 0..w {
            let p = &bytes[y * stride + x * 4..y * stride + x * 4 + 3];
            if p[0] > 200 && p[1] < 60 && p[2] < 60 {
                count += 1;
            }
        }
    }
    count
}

fn canary() -> gtk::Window {
    let window = gtk::Window::builder().title("canary").default_width(600).default_height(400).build();
    window.add_css_class("canary");
    tessel_gtk::utils::add_css("window.canary { background: rgb(255,0,0); }", None, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 100);
    window.present();
    settle(0.5);
    window
}

fn type_text(text: &str) {
    run("wtype", &["-d", "5", text]);
}

/// Type the fake policy's password and second factor into the real locker.
/// Retries (Escape + retry delay) if keystrokes raced the surface mapping.
fn unlock_with_fake_pam(locker: &mut Locker, label: &str) -> Option<i32> {
    for attempt in 1..=3 {
        run("wtype", &["-k", "space"]);
        settle(0.8);
        let _ = Command::new("grim").arg(scratch().join(format!("{label}-a{attempt}-woken.png"))).status();
        type_text("fixture password");
        run("wtype", &["-k", "Return"]);
        settle(1.2);
        let _ = Command::new("grim").arg(scratch().join(format!("{label}-a{attempt}-typed.png"))).status();
        if attempt == 1 {
            assert!(locker.alive(), "the fake policy still asks for a second factor");
            assert_eq!(red_pixels(&format!("{label}-second-factor")), 0);
        }
        type_text("123456");
        run("wtype", &["-k", "Return"]);
        if let Some(code) = locker.wait_exit(5.0) {
            println!("note {label}: unlocked on attempt {attempt}");
            return Some(code);
        }
        run("wtype", &["-k", "Escape"]);
        settle(2.5);
    }
    None
}

fn real_locker_survives_wrong_password_signals_and_crashes() {
    let bin = locker_dir();
    let rust = vec![bin.join("tessel-lock").to_string_lossy().into_owned()];
    let window = canary();
    assert!(red_pixels("00-unlocked") > 10_000, "canary visible before locking");
    let mut a = spawn_locker(&rust, "a", &[]);
    let ready = a.ready().expect("READY=1 only after the compositor confirmed the lock");
    println!("measure rust time_to_locked_ms={}", ready.as_millis());
    settle(1.0);
    let (rss, anon) = a.rss();
    println!("measure rust locked_rss_kb={rss} rss_anon_kb={anon}");
    assert_eq!(red_pixels("01-locked"), 0, "no client content while locked");
    // A second locker is rejected (exit 75) while the first holds the lock.
    let mut b = spawn_locker(&rust, "b", &[]);
    assert_eq!(b.wait_exit(10.0), Some(75));
    // (a) wrong password keeps it locked.
    run("wtype", &["-k", "space"]);
    settle(0.8);
    type_text("wrong");
    run("wtype", &["-k", "Return"]);
    settle(2.5);
    assert!(a.alive(), "wrong password: locker keeps running");
    assert_eq!(red_pixels("02-wrong"), 0);
    // Escape never unlocks.
    run("wtype", &["-k", "Escape"]);
    settle(0.5);
    assert!(a.alive());
    let mut b = spawn_locker(&rust, "b2", &[]);
    assert_eq!(b.wait_exit(10.0), Some(75), "still locked after a wrong password and Escape");
    // SIGTERM (systemd stop) and SIGINT exit without unlocking.
    a.signal(libc::SIGTERM);
    assert!(a.wait_exit(5.0).is_some());
    settle(0.5);
    assert_eq!(red_pixels("03-sigterm"), 0, "SIGTERM leaves the session locked");
    // (b) SIGKILL of the locker keeps the session locked; a fresh locker can lock again.
    let mut c = spawn_locker(&rust, "c", &[]);
    assert!(c.ready().is_some(), "replacement locker re-locks the crashed session");
    settle(0.5);
    c.signal(libc::SIGKILL);
    assert!(c.wait_exit(5.0).is_some());
    settle(0.5);
    assert_eq!(red_pixels("04-sigkill"), 0, "SIGKILL leaves the session locked");
    // (c) only fake-PAM success unlocks.
    let mut d = spawn_locker(&rust, "d", &[]);
    assert!(d.ready().is_some());
    settle(0.5);
    assert_eq!(unlock_with_fake_pam(&mut d, "05"), Some(0), "PAM success unlocks and exits 0");
    settle(0.8);
    assert!(red_pixels("06-unlocked") > 10_000, "desktop visible again after PAM success");
    window.destroy();
    settle(0.1);
}

fn check_reports_without_locking() {
    let bin = locker_dir();
    let output = Command::new(bin.join("tessel-lock")).arg("--check").output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["session_lock"], json!(true));
    assert_eq!(value["pam_helper"], json!(true));
    assert_eq!(text.split('"').filter(|s| s.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !s.is_empty()).collect::<Vec<_>>(),
               ["session_lock", "pam_helper", "pam_policy"]);
    assert_eq!(output.status.code(), Some(if value["pam_policy"] == json!(true) { 0 } else { 1 }));
    // Nothing was locked: a fresh locker can lock immediately.
    let mut e = spawn_locker(&[bin.join("tessel-lock").to_string_lossy().into_owned()], "e", &[]);
    assert!(e.ready().is_some());
    e.signal(libc::SIGKILL);
    let _ = e.wait_exit(5.0);
    // Unknown flags: argparse exit 2, no lock.
    let status = Command::new(bin.join("tessel-lock")).arg("--bogus").stderr(Stdio::null()).status().unwrap();
    assert_eq!(status.code(), Some(2));
    // Recover the session lock left by SIGKILL through fake PAM success.
    let mut d = spawn_locker(&[bin.join("tessel-lock").to_string_lossy().into_owned()], "e2", &[]);
    assert!(d.ready().is_some());
    settle(0.5);
    assert_eq!(unlock_with_fake_pam(&mut d, "e2"), Some(0));
}

// ---------------------------------------------------------------- runner

fn main() {
    if !isolated() {
        println!("ui_headless: skipped (requires the private compositor: tests/run_headless.sh)");
        return;
    }
    std::env::set_var("WAYKEYS_INPUT", tessel_keyboard::BUILT_HELPER);
    tessel_core::i18n::initialize();
    gtk::init().expect("gtk");
    // A persistent virtual keyboard so the seat has a keyboard (wtype -s).
    let mut keyboard = Command::new("wtype").args(["-s", "900000"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok();
    settle(0.1);
    let checks: Vec<(&str, fn())> = vec![
        ("all_monitors_covered_and_escape_cannot_unlock", all_monitors_covered_and_escape_cannot_unlock),
        ("revealed_password_never_reaches_the_clipboard", revealed_password_never_reaches_the_clipboard),
        ("failure_cancellation_stale_success_and_valid_authentication", failure_cancellation_stale_success_and_valid_authentication),
        ("print_screen_does_not_reveal_login_or_type_password", print_screen_does_not_reveal_login_or_type_password),
        ("starts_with_wallpaper_and_native_input_works_on_either_monitor", starts_with_wallpaper_and_native_input_works_on_either_monitor),
        ("touch_keyboard_edits_the_selected_lock_field_without_ipc", touch_keyboard_edits_the_selected_lock_field_without_ipc),
        ("touch_keyboard_dead_keys_remain_inside_lock_form", touch_keyboard_dead_keys_remain_inside_lock_form),
        ("fingerprint_is_a_parallel_pam_transaction", fingerprint_is_a_parallel_pam_transaction),
        ("password_success_cancels_fingerprint_and_failures_restart", password_success_cancels_fingerprint_and_failures_restart),
        ("fingerprint_stops_after_repeated_non_matches", fingerprint_stops_after_repeated_non_matches),
        ("without_fingerprint_policy_password_uses_tessel_lock", without_fingerprint_policy_password_uses_tessel_lock),
        ("battery_and_notification_count_are_privacy_safe", battery_and_notification_count_are_privacy_safe),
        ("appearance_changes_live_and_voice_is_conditional", appearance_changes_live_and_voice_is_conditional),
        ("idle_hides_only_login_cancels_auth_and_keyboard_reveals_all_monitors", idle_hides_only_login_cancels_auth_and_keyboard_reveals_all_monitors),
        ("pointer_wake_and_hints_use_the_existing_secure_surface", pointer_wake_and_hints_use_the_existing_secure_surface),
        ("action_sizes_and_native_mouse_wake", action_sizes_and_native_mouse_wake),
        ("account_fallback_and_extra_authentication_prompts", account_fallback_and_extra_authentication_prompts),
        ("palette_follows_each_lockscreen_wallpaper_and_slideshow", palette_follows_each_lockscreen_wallpaper_and_slideshow),
        ("monitor_hotplug_gets_a_lock_surface_and_removal_keeps_the_lock", monitor_hotplug_gets_a_lock_surface_and_removal_keeps_the_lock),
        ("dashboard_media_group_follows_media_row", dashboard_media_group_follows_media_row),
        ("preview_is_explicitly_unlocked_and_has_no_authentication", preview_is_explicitly_unlocked_and_has_no_authentication),
        ("check_reports_without_locking", check_reports_without_locking),
        ("real_locker_survives_wrong_password_signals_and_crashes", real_locker_survives_wrong_password_signals_and_crashes),
    ];
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let (mut passed, mut failed) = (0, Vec::new());
    for (name, check) in checks {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
        // A failed check must not leave its session lock held for the next one.
        if let Some(app) = CURRENT.with(|c| c.borrow_mut().take()) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| release(app)));
        }
        match result {
            Ok(()) => { passed += 1; println!("ok   {name}"); }
            Err(error) => {
                let text = error.downcast_ref::<String>().cloned().or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                println!("FAIL {name}: {text}");
                failed.push(name);
            }
        }
    }
    if let Some(keyboard) = keyboard.as_mut() {
        let _ = keyboard.kill();
        let _ = keyboard.wait();
    }
    println!("ui_headless: {passed} passed, {} failed {failed:?}", failed.len());
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
