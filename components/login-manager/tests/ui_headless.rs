//! Port of test_login_ui.py (+ end-to-end greetd, fake logind and --preview
//! checks). Runs ONLY inside the private nested headless labwc started by
//! tests/run_headless.sh: two outputs, a private D-Bus used as session *and*
//! system bus (no service activation), private HOME/XDG dirs, wtype/wlrctl/grim
//! talking to that compositor only. greetd is a FakeClient or a fake socket
//! server written here; logind is a fake object on the private bus. Under
//! plain `cargo test` this binary skips itself.
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};
use gtk4_layer_shell::{KeyboardMode, LayerShell};
use serde_json::{json, Value};
use suite_lock::login_model::{Appearance, Session, User};
use waylogin::desktop_fade::DesktopFade;
use waylogin::ui::{App, Authenticator, Changed, LoginWindow, Options, Reader};
use zeroize::Zeroizing;

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait(timeout: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    predicate()
}

fn run(program: &str, args: &[&str]) {
    let status = std::process::Command::new(program).args(args).status().unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

// ------------------------------------------------------------ fake greetd client

/// test_login_ui.FakeClient
struct FakeClient {
    callback: Changed,
    state: RefCell<String>,
    responses: RefCell<Vec<String>>,
    generation: Cell<u64>,
    starts: RefCell<Vec<u64>>,
    session: RefCell<Option<Session>>,
}

impl Authenticator for FakeClient {
    fn state(&self) -> String { self.state.borrow().clone() }
    fn generation(&self) -> u64 { self.generation.get() }
    fn begin(&self, _username: &str, session: Option<Session>) {
        *self.session.borrow_mut() = session;
        *self.state.borrow_mut() = "prompt".into();
        (self.callback)("secret", "Password:");
    }
    fn respond(&self, text: Zeroizing<String>) {
        self.responses.borrow_mut().push(text.to_string());
        *self.state.borrow_mut() = "authenticating".into();
        (self.callback)("busy", "Checking…");
    }
    fn cancel(&self) { *self.state.borrow_mut() = "idle".into() }
    fn start_session(&self, generation: u64) { self.starts.borrow_mut().push(generation) }
    fn close(&self) { *self.state.borrow_mut() = "closed".into() }
}

thread_local! {
    static FAKE: RefCell<Option<Rc<FakeClient>>> = const { RefCell::new(None) };
    static READER: Rc<RefCell<Vec<String>>> = Rc::default();
}

fn fake() -> Rc<FakeClient> {
    FAKE.with(|f| f.borrow().clone().expect("fake client"))
}

fn launched() -> Vec<String> {
    READER.with(|r| r.borrow().clone())
}

fn sessions() -> Vec<Session> {
    vec![Session { ident: "labwc".into(), name: "labwc".into(), argv: vec!["labwc".into()], desktops: "labwc".into() },
         Session { ident: "plasma".into(), name: "Plasma".into(), argv: vec!["/bin/true".into()], desktops: "KDE".into() }]
}

fn users() -> Vec<User> {
    vec![User { username: "fixture".into(), name: "Test User".into() }]
}

struct FakeReader(Rc<RefCell<Vec<String>>>);

impl Reader for FakeReader {
    fn running(&mut self) -> bool { true }
    fn terminate(&mut self) { self.0.borrow_mut().push("stopped".into()) }
}

struct Fixture {
    app: App,
    keyboard: Option<std::process::Child>,
}

impl Fixture {
    fn with(mut options: Options, windows: usize) -> Fixture {
        let log = READER.with(|r| r.clone());
        log.borrow_mut().clear();
        options.reader_launcher = Some(Box::new(move |argv| {
            log.borrow_mut().push(argv.join(" "));
            Some(Box::new(FakeReader(log.clone())) as Box<dyn Reader>)
        }));
        options.watch_devices = Some(false);
        // GtkApplication keeps a startup reference until run() returns, so a
        // registered test app stays exported: give each fixture its own id.
        static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let count = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count > 0 {
            options.application_id = Some(format!("{}.Test{count}", waylogin::ui::APPLICATION_ID));
        }
        // A persistent virtual keyboard so the seat has a keyboard (Python: wtype -s 25000).
        let keyboard = std::process::Command::new("wtype").args(["-s", "25000"]).spawn().ok();
        let app = App::new(options);
        app.application().register(None::<&gio::Cancellable>).unwrap();
        app.application().activate();
        settle(0.8);
        if count == 0 {
            assert_eq!(app.application().application_id().unwrap(), "org.wayshell.Login");
        }
        assert!(app.0.startup_error.borrow().is_none(), "{:?}", app.0.startup_error.borrow());
        assert_eq!(app.windows().len(), windows);
        Fixture { app, keyboard }
    }

    fn new() -> Fixture {
        Fixture::with(Options {
            appearance: Some(Appearance::default()), sessions: Some(sessions()), users: Some(users()),
            client_factory: Some(Box::new(|_path, changed| {
                let client = Rc::new(FakeClient { callback: changed, state: RefCell::new("idle".into()), responses: RefCell::default(),
                    generation: Cell::new(1), starts: RefCell::default(), session: RefCell::new(None) });
                FAKE.with(|f| *f.borrow_mut() = Some(client.clone()));
                client
            })),
            ..Options::default()
        }, 2)
    }

    fn first(&self) -> Rc<LoginWindow> {
        self.app.windows()[0].clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.shutdown();
        for window in self.app.windows() {
            window.window.destroy();
        }
        settle(0.1);
        if let Some(mut keyboard) = self.keyboard.take() {
            let _ = keyboard.kill();
            let _ = keyboard.wait();
        }
        settle(0.1);
    }
}

fn bounds(widget: &impl IsA<gtk::Widget>, target: &impl IsA<gtk::Widget>) -> (f64, f64, f64, f64) {
    let rect = widget.compute_bounds(target).expect("bounds");
    (rect.x() as f64, rect.y() as f64, rect.width() as f64, rect.height() as f64)
}

// ------------------------------------------------------------ checks (test_login_ui.py)

fn accessibility_menu_applies_to_every_monitor() {
    let f = Fixture::new();
    let windows = f.app.windows();
    for window in &windows {
        assert_eq!(window.accessibility.parent(), window.power_button.parent());
        assert!(window.a11y_switch("large_text").is_some());
        assert!(window.a11y_switch("keyboard").is_some());
        assert_eq!(window.a11y_switch("screen_reader").is_some(), f.app.0.screen_reader_available);
    }
    windows[0].a11y_switch("large_text").unwrap().set_active(true);
    assert!(windows[1].a11y_switch("large_text").unwrap().is_active());
    assert!(f.app.0.a11y_styles.borrow().contains_key("large_text"));
    windows[1].a11y_switch("high_contrast").unwrap().set_active(true);
    assert!(windows[0].a11y_switch("high_contrast").unwrap().is_active());
    windows[0].a11y_switch("large_text").unwrap().set_active(false);
    assert!(!f.app.0.a11y_styles.borrow().contains_key("large_text"));
    f.app.toggle_screen_reader(true);
    f.app.toggle_screen_reader(false);
    assert_eq!(launched(), ["orca --replace", "stopped"]);
    windows[0].a11y_switch("high_contrast").unwrap().set_active(false);
    settle(0.1);
}

fn mapped_labels(root: &gtk::Widget) -> Vec<String> {
    let mut labels = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            if label.is_mapped() {
                labels.push(label.text().to_string());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if !current.is::<gtk::Popover>() {
                stack.push(current.clone());
            }
            child = current.next_sibling();
        }
    }
    labels
}

fn lock_and_login_share_one_visual_family() {
    let f = Fixture::new();
    let window = f.first();
    f.app.activity(&window);
    settle(0.5);
    // The account name appears once: as the user selector.
    let labels = mapped_labels(window.form.upcast_ref());
    assert_eq!(labels.iter().filter(|l| *l == "Test User").count(), 1, "{labels:?}");
    assert!(window.user_choice.has_css_class("family-user-choice"));
    assert_eq!(window.avatar.text(), "TU");
    let size = suite_lock::visuals::AVATAR_SIZE;
    assert_eq!((window.avatar.widget().width(), window.avatar.widget().height()), (size, size));
    // No ragged "Session" caption: a centred pill below the password.
    assert!(window.session_choice.has_css_class("family-session"));
    let field = bounds(&window.entry, &window.form);
    let session = bounds(&window.session_choice, &window.form);
    assert!(session.1 > field.1 + field.3 - 1.0);
    assert!((session.0 + session.2 / 2.0 - window.form.width() as f64 / 2.0).abs() <= 1.0);
    assert!(window.entry.has_css_class("family-password"));
    let keyboard = window.keyboard();
    for button in [window.accessibility.upcast_ref::<gtk::Widget>(), window.power_button.upcast_ref(), window.sleep.upcast_ref(),
                   keyboard.button().upcast_ref()] {
        assert!(button.has_css_class("family-action"));
    }
    let inner = window.accessibility.first_child().unwrap();
    assert_eq!((inner.width(), inner.height()), (window.power_button.width(), window.power_button.height()));
    assert!(window.clock.has_css_class("family-clock"));
    assert_eq!(window.date.text(), suite_lock::visuals::long_date(&glib::DateTime::now_local().unwrap()));
    assert!(window.preview_close.is_none());
}

fn idle_wake_keys_password_and_cancel() {
    let f = Fixture::new();
    assert!(!f.app.0.visible.get());
    assert!(f.app.windows().iter().all(|w| w.idle.is_mapped()));
    run("wtype", &["-k", "space"]);
    settle(0.5);
    assert!(f.app.0.visible.get());
    let window = f.app.active().expect("active window");
    assert_eq!(window.entry.text(), "");
    assert!(!window.idle.is_visible());
    run("wtype", &["fixture secret"]);
    settle(0.1);
    assert_eq!(window.entry.text(), "fixture secret");
    run("wtype", &["-k", "Return"]);
    settle(0.1);
    assert_eq!(*fake().responses.borrow(), ["fixture secret"]);
    assert_eq!(window.entry.text(), "");
    f.app.hide_login();
    settle(0.5);
    assert!(!f.app.0.visible.get());
    assert!(window.idle.is_visible());
    assert_eq!(fake().state(), "idle");
}

fn pointer_wake_and_typing_on_each_monitor() {
    let f = Fixture::new();
    run("wlrctl", &["pointer", "move", "80", "50"]);
    settle(0.5);
    assert!(f.app.0.visible.get());
    for window in f.app.windows() {
        let geometry = window.monitor.geometry();
        let rect = bounds(&window.entry, &window.window);
        let (x, y) = (geometry.x() as f64 + rect.0 + 25.0, geometry.y() as f64 + rect.1 + 25.0);
        run("wlrctl", &["pointer", "move", "-100000", "-100000"]);
        run("wlrctl", &["pointer", "move", &(x as i64).to_string(), &(y as i64).to_string()]);
        settle(0.15);
        run("wlrctl", &["pointer", "click"]);
        settle(0.1);
        run("wtype", &["hello"]);
        settle(0.1);
        let active = f.app.active().expect("active");
        assert!(Rc::ptr_eq(&active, &window), "expected {:?} at {x},{y}", window.monitor.connector());
        assert_eq!(window.entry.text(), "hello");
        assert!(f.app.windows().iter().filter(|o| !Rc::ptr_eq(o, &window)).all(|o| o.entry.text().is_empty()));
    }
}

fn idle_timeout_clears_secret_and_session_selection() {
    let f = Fixture::new();
    let window = f.first();
    f.app.activity(&window);
    settle(0.5);
    window.entry.set_text("do not retain");
    window.session_choice.set_selected(1);
    settle(0.1);
    assert_eq!(fake().session.borrow().as_ref().unwrap().ident, "plasma");
    assert_eq!(window.entry.text(), "");
    window.entry.set_text("discard on idle");
    f.app.age_activity(31.0);
    f.app.tick();
    settle(0.5);
    assert!(!f.app.0.visible.get());
    assert_eq!(window.entry.text(), "");
}

fn preview_screenshots() {
    // These images contain a fake account and no user credentials.
    let directory = std::env::temp_dir();
    let f = Fixture::new();
    let idle = directory.join("waylogin-idle.png");
    run("grim", &["-o", "HEADLESS-1", idle.to_str().unwrap()]);
    let window = f.first();
    f.app.activity(&window);
    settle(0.8);
    let form = directory.join("waylogin-form.png");
    run("grim", &["-o", "HEADLESS-1", form.to_str().unwrap()]);
    assert!(std::fs::metadata(&idle).unwrap().len() > 1000 && std::fs::metadata(&form).unwrap().len() > 1000);
}

fn restart_and_shutdown_each_require_confirmation() {
    let f = Fixture::new();
    let window = f.first();
    f.app.activity(&window);
    let calls: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = calls.clone();
    f.app.set_power_hook(Some(Box::new(move |method| log.borrow_mut().push(method.into()))));
    for method in ["Suspend", "Reboot", "PowerOff"] {
        f.app.0.capabilities.borrow_mut().insert(method.into(), true);
    }
    window.tick();
    settle(0.5);
    for (button, method) in [(&window.restart_button, "Reboot"), (&window.power_button, "PowerOff")] {
        button.emit_clicked();
        assert!(window.power_panel.is_visible());
        assert!(calls.borrow().is_empty());
        window.power_cancel.emit_clicked();
        window.power_confirm.emit_clicked();
        assert!(calls.borrow().is_empty());
        button.emit_clicked();
        window.power_confirm.emit_clicked();
        assert_eq!(*calls.borrow(), [method]);
        calls.borrow_mut().clear();
    }
    f.app.0.capabilities.borrow_mut().insert("Reboot".into(), false);
    window.tick();
    assert!(!window.restart_button.is_sensitive());
    assert!(window.power_button.is_sensitive());
    f.app.0.preview.set(true);
    window.tick();
    assert!(!window.power_button.is_sensitive());
    assert!(!window.sleep.is_sensitive());
    f.app.0.preview.set(false);
}

fn submit_chevron_is_centered_and_action_buttons_are_equal() {
    let f = Fixture::new();
    let window = f.first();
    f.app.activity(&window);
    settle(0.5);
    let keyboard = window.keyboard();
    let buttons: [&gtk::Widget; 6] = [window.eye.upcast_ref(), window.enter.upcast_ref(), window.sleep.upcast_ref(),
                                      window.restart_button.upcast_ref(), window.power_button.upcast_ref(), keyboard.button().upcast_ref()];
    let sizes: Vec<(i32, i32)> = buttons.iter().map(|b| (b.width(), b.height())).collect();
    assert!(sizes.windows(2).all(|w| w[0] == w[1]), "{sizes:?}");
    assert_eq!(sizes[0].0, sizes[0].1);
    let icon = window.enter.child().unwrap();
    assert!(icon.is::<gtk::DrawingArea>());
    assert_eq!((icon.width(), icon.height()), (20, 20));
    let b = bounds(&icon, &window.enter);
    assert!((b.0 + b.2 / 2.0 - window.enter.width() as f64 / 2.0).abs() <= 1.0);
    assert!((b.1 + b.3 / 2.0 - window.enter.height() as f64 / 2.0).abs() <= 1.0);
}

fn keyboard_button_types_locally_on_either_monitor_and_can_hide() {
    let f = Fixture::new();
    for window in f.app.windows() {
        f.app.activity(&window);
        window.begin();
        settle(0.4);
        let touch = window.keyboard();
        assert!(touch.button().is_mapped());
        touch.button().emit_clicked();
        settle(0.2);
        let widget = touch.widget().expect("keyboard widget");
        assert!(widget.root().is_mapped());
        widget.press(30);
        widget.character("7");
        assert_eq!(window.entry.text(), "a7");
        assert!(fake().responses.borrow().is_empty());
        widget.hide_button().emit_clicked();
        assert!(!widget.root().is_visible());
        assert_eq!(window.content.margin_bottom(), 0);
        f.app.hide_login();
        settle(0.4);
        assert_eq!(window.entry.text(), "");
    }
}

fn success_fades_before_handoff_and_failure_restores_form() {
    let f = Fixture::new();
    let window = f.first();
    f.app.activity(&window);
    settle(0.5);
    *fake().state.borrow_mut() = "ready".into();
    f.app.authentication_event("authenticated", "Starting your session…");
    assert!(f.app.0.transitioning.get());
    assert!(!window.form.is_sensitive());
    // begin() is ignored while transitioning (Python: begin.assert_not_called()).
    *fake().state.borrow_mut() = "sentinel".into();
    window.begin();
    assert_eq!(fake().state(), "sentinel");
    *fake().state.borrow_mut() = "ready".into();
    assert!(fake().starts.borrow().is_empty());
    settle(0.12);
    let opacity = window.fade_cover.opacity();
    assert!(opacity > 0.0 && opacity < 1.0, "{opacity}");
    assert!(fake().starts.borrow().is_empty());
    f.app.hide_login();
    assert!(f.app.0.visible.get());
    settle(0.4);
    assert_eq!(*fake().starts.borrow(), [1]);
    assert_eq!(window.fade_cover.opacity(), 1.0);
    *fake().state.borrow_mut() = "error".into();
    f.app.set_active(None); // Unplugged authentication monitor during handoff.
    f.app.authentication_event("error", "Session failed");
    assert!(f.app.active().is_some());
    assert!(!f.app.0.transitioning.get());
    assert!(!window.fade_cover.is_visible());
    assert!(window.form.is_sensitive());
}

fn desktop_cover_expires_and_does_not_take_keyboard_focus() {
    let app = DesktopFade::new();
    let quits = Rc::new(Cell::new(0));
    let counter = quits.clone();
    app.set_quit(move || counter.set(counter.get() + 1));
    app.application().register(None::<&gio::Cancellable>).unwrap();
    app.application().activate();
    settle(0.2);
    assert_eq!(app.windows().len(), 2);
    for window in app.windows() {
        assert_eq!(window.keyboard_mode(), KeyboardMode::None);
        assert_eq!(window.exclusive_zone(), -1);
        assert_eq!(window.opacity(), 1.0);
    }
    settle(2.5);
    assert_eq!(quits.get(), 1);
    assert!(app.windows().iter().all(|w| w.opacity() == 0.0));
    app.shutdown();
}

// ------------------------------------------------------------ extra checks

fn preview_is_windowed_and_inert() {
    let f = Fixture::with(Options { preview: true, appearance: Some(Appearance::default()), sessions: Some(sessions()),
        users: Some(users()), ..Options::default() }, 1);
    assert!(f.app.client().is_none(), "preview never connects to greetd");
    let window = f.first();
    assert!(window.preview_close.is_some());
    assert!(!window.window.is_layer_window());
    assert_eq!(window.window.default_size().0, 1200); // height is clamped to the 720 px headless output
    f.app.activity(&window);
    settle(0.3);
    assert!(window.entry.is_sensitive());
    window.entry.set_text("not a secret");
    window.submit();
    assert_eq!(window.entry.text(), "");
    assert_eq!(window.status.text(), "Preview only — authentication and power actions are disabled.");
    f.app.0.capabilities.borrow_mut().insert("PowerOff".into(), true);
    window.tick();
    assert!(!window.power_button.is_sensitive() && !window.sleep.is_sensitive());
    f.app.toggle_screen_reader(true);
    assert!(launched().is_empty(), "no screen reader in preview");
}

/// A fake greetd socket server: records frames, answers from a script.
struct Greetd {
    path: PathBuf,
    thread: Option<std::thread::JoinHandle<Vec<Vec<u8>>>>,
}

impl Greetd {
    fn new(name: &str, answers: Vec<Value>) -> Greetd {
        let path = std::env::temp_dir().join(format!("{name}.sock"));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let thread = std::thread::spawn(move || {
            let mut answers = answers.into_iter();
            let mut frames = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(8);
            'accept: while Instant::now() < deadline {
                let Ok((mut channel, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                channel.set_nonblocking(false).unwrap();
                channel.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                loop {
                    let mut header = [0u8; 4];
                    if channel.read_exact(&mut header).is_err() {
                        if answers.len() == 0 {
                            return frames;
                        }
                        continue 'accept;
                    }
                    let mut payload = vec![0u8; u32::from_ne_bytes(header) as usize];
                    channel.read_exact(&mut payload).unwrap();
                    frames.push([&header[..], &payload].concat());
                    let Some(answer) = answers.next() else { return frames };
                    let data = serde_json::to_vec(&answer).unwrap();
                    channel.write_all(&[&(data.len() as u32).to_ne_bytes()[..], &data].concat()).unwrap();
                }
            }
            frames
        });
        Greetd { path, thread: Some(thread) }
    }

    fn finish(mut self) -> Vec<Vec<u8>> {
        let frames = self.thread.take().unwrap().join().unwrap();
        let _ = std::fs::remove_file(&self.path);
        frames
    }
}

fn greetd_fixture(path: &std::path::Path) -> Fixture {
    let dummy = std::env::var("GREETD_SOCK").unwrap();
    std::env::set_var("GREETD_SOCK", path);
    let fixture = Fixture::with(Options { appearance: Some(Appearance::default()), sessions: Some(sessions()), users: Some(users()),
        ..Options::default() }, 2);
    std::env::set_var("GREETD_SOCK", dummy);
    fixture
}

fn kinds(frames: &[Vec<u8>]) -> Vec<String> {
    frames.iter().map(|f| serde_json::from_slice::<Value>(&f[4..]).unwrap()["type"].as_str().unwrap().to_owned()).collect()
}

fn real_greetd_sign_in_starts_session_only_after_success() {
    let greetd = Greetd::new("greetd-ok", vec![json!({"type": "success"}),
        json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"}),
        json!({"type": "success"}), json!({"type": "success"})]);
    let f = greetd_fixture(&greetd.path);
    let window = f.first();
    f.app.activity(&window);
    assert!(wait(3.0, || window.entry.is_sensitive()), "password prompt");
    assert!(!window.prompt.is_visible(), "plain Password: prompt is not repeated");
    run("wtype", &["päss wörd"]);
    settle(0.1);
    assert!(f.app.client().is_some_and(|c| c.state() == "prompt"));
    run("wtype", &["-k", "Return"]);
    assert!(wait(3.0, || f.app.0.transitioning.get()), "authenticated");
    assert_eq!(window.entry.text(), "");
    assert!(f.app.client().is_some_and(|c| c.state() == "ready"), "no start_session before the fade completes");
    assert!(wait(3.0, || f.app.client().is_some_and(|c| c.state() == "started")), "session started");
    let frames = greetd.finish();
    assert_eq!(kinds(&frames), ["cancel_session", "create_session", "post_auth_message_response", "start_session"]);
    let expected = waylogin::protocol::Request::Respond(Zeroizing::new("päss wörd".into())).frame().unwrap();
    assert_eq!(frames[2], *expected, "password frame is byte-identical to Python's");
    let start: Value = serde_json::from_slice(&frames[3][4..]).unwrap();
    assert_eq!(start, Value::Object(sessions()[0].request()));
}

fn real_greetd_wrong_password_never_starts_session() {
    let greetd = Greetd::new("greetd-bad", vec![json!({"type": "success"}),
        json!({"type": "auth_message", "auth_message_type": "info", "auth_message": "Touch the key"}),
        json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "PIN:"}),
        json!({"type": "error", "error_type": "auth_error", "description": "denied"}), json!({"type": "success"})]);
    let f = greetd_fixture(&greetd.path);
    let window = f.first();
    f.app.activity(&window);
    assert!(wait(3.0, || window.entry.is_sensitive()), "PIN prompt");
    assert!(window.prompt.is_visible());
    assert_eq!(window.prompt.text(), "PIN:");
    window.entry.set_text("0000");
    window.submit();
    assert!(wait(3.0, || window.status.text() == "Sign-in failed. Check your credentials and try again."));
    assert!(!f.app.0.transitioning.get());
    assert!(!window.entry.is_sensitive() && window.entry.text().is_empty());
    assert!(window.enter.is_sensitive(), "retry is explicit");
    f.app.hide_login();
    settle(0.3);
    let frames = greetd.finish();
    let kinds = kinds(&frames);
    assert!(!kinds.contains(&"start_session".to_string()), "{kinds:?}");
    assert_eq!(&kinds[..4], ["cancel_session", "create_session", "post_auth_message_response", "post_auth_message_response"]);
    assert_eq!(serde_json::from_slice::<Value>(&frames[2][4..]).unwrap(), json!({"type": "post_auth_message_response"}));
}

fn fake_logind_controls_power_buttons() {
    const XML: &str = r#"<node><interface name="org.freedesktop.login1.Manager">
      <method name="CanSuspend"><arg type="s" direction="out"/></method>
      <method name="CanPowerOff"><arg type="s" direction="out"/></method>
      <method name="CanReboot"><arg type="s" direction="out"/></method>
      <method name="Suspend"><arg type="b" direction="in"/></method>
      <method name="PowerOff"><arg type="b" direction="in"/></method>
      <method name="Reboot"><arg type="b" direction="in"/></method>
    </interface></node>"#;
    let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap();
    let connection = gio::DBusConnection::for_address_sync(&address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>).unwrap();
    let calls: Rc<RefCell<Vec<(String, bool)>>> = Rc::default();
    let log = calls.clone();
    let info = gio::DBusNodeInfo::for_xml(XML).unwrap().lookup_interface("org.freedesktop.login1.Manager").unwrap();
    let registration = connection.register_object("/org/freedesktop/login1", &info)
        .method_call(move |_, _, _, _, method, parameters, invocation| match method {
            "CanSuspend" => invocation.return_value(Some(&("yes",).to_variant())),
            "CanPowerOff" => invocation.return_value(Some(&("challenge",).to_variant())),
            "CanReboot" => invocation.return_value(Some(&("na",).to_variant())),
            other => {
                log.borrow_mut().push((other.into(), parameters.child_value(0).get::<bool>().unwrap()));
                invocation.return_value(None);
            }
        }).build().unwrap();
    let reply = connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        Some(&("org.freedesktop.login1", 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).unwrap();
    assert_eq!(reply.child_value(0).get::<u32>(), Some(1));
    let f = Fixture::new();
    assert!(wait(3.0, || f.app.0.capabilities.borrow().len() == 3), "CanX answered");
    let window = f.first();
    f.app.activity(&window);
    settle(0.5);
    window.tick();
    assert!(window.sleep.is_sensitive());
    assert!(!window.power_button.is_sensitive() && !window.restart_button.is_sensitive());
    window.sleep.emit_clicked();
    assert!(wait(3.0, || !calls.borrow().is_empty()));
    assert_eq!(*calls.borrow(), [("Suspend".to_string(), false)]);
    assert!(!f.app.0.visible.get(), "suspend hides the form");
    // Never a power action while a session is launching.
    *fake().state.borrow_mut() = "launching".into();
    f.app.power("Suspend");
    settle(0.3);
    assert_eq!(calls.borrow().len(), 1);
    drop(f);
    let _ = connection.unregister_object(registration);
}

fn main() {
    let isolated = std::env::var("LOGIN_UI_ISOLATED_TEST").as_deref() == Ok("1");
    if !isolated {
        println!("ui_headless: skipped (run tests/run_headless.sh)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let session = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    let system = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_default();
    assert!(runtime.starts_with("/run/user/1000/cbt/") && runtime != "/run/user/1000", "not the private compositor");
    assert!(std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| std::path::Path::new(&runtime).join(&d).exists()), "private Wayland socket");
    assert!(session.contains("/run/user/1000/cbt/") && system == session, "private buses only");
    gtk::init().unwrap();
    let filter: Vec<String> = std::env::args().skip(1).collect();
    let checks: Vec<(&str, fn())> = vec![
        ("accessibility_menu_applies_to_every_monitor", accessibility_menu_applies_to_every_monitor),
        ("lock_and_login_share_one_visual_family", lock_and_login_share_one_visual_family),
        ("idle_wake_keys_password_and_cancel", idle_wake_keys_password_and_cancel),
        ("pointer_wake_and_typing_on_each_monitor", pointer_wake_and_typing_on_each_monitor),
        ("idle_timeout_clears_secret_and_session_selection", idle_timeout_clears_secret_and_session_selection),
        ("preview_screenshots", preview_screenshots),
        ("restart_and_shutdown_each_require_confirmation", restart_and_shutdown_each_require_confirmation),
        ("submit_chevron_is_centered_and_action_buttons_are_equal", submit_chevron_is_centered_and_action_buttons_are_equal),
        ("keyboard_button_types_locally_on_either_monitor_and_can_hide", keyboard_button_types_locally_on_either_monitor_and_can_hide),
        ("success_fades_before_handoff_and_failure_restores_form", success_fades_before_handoff_and_failure_restores_form),
        ("desktop_cover_expires_and_does_not_take_keyboard_focus", desktop_cover_expires_and_does_not_take_keyboard_focus),
        ("preview_is_windowed_and_inert", preview_is_windowed_and_inert),
        ("real_greetd_sign_in_starts_session_only_after_success", real_greetd_sign_in_starts_session_only_after_success),
        ("real_greetd_wrong_password_never_starts_session", real_greetd_wrong_password_never_starts_session),
        ("fake_logind_controls_power_buttons", fake_logind_controls_power_buttons),
    ];
    let mut failed = Vec::new();
    for (name, check) in checks {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let started = Instant::now();
        let result = std::panic::catch_unwind(check);
        println!("{} {name} ({:.1}s)", if result.is_ok() { "ok  " } else { "FAIL" }, started.elapsed().as_secs_f64());
        if result.is_err() {
            failed.push(name);
        }
    }
    println!("{} failed: {failed:?}", failed.len());
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
