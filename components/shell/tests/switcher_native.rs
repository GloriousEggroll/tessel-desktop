//! Port of test_switcher_native.py: real Alt+Tab input and activation in a
//! private nested compositor (tests/run_switcher_native.sh: headless labwc →
//! private Xwayland → X11-backend labwc driven by xdotool XTEST, so held
//! modifiers travel through labwc's real keyboard group). One test per process.
//! Under plain `cargo test` this binary skips itself.

use gtk::prelude::*;
use gtk::{gdk, gio};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_shell::server::{Server, Shell};
use tessel_shell::switcher::audio::{Audio, NoAudio};
use tessel_shell::switcher::ui::Mode;
use tessel_shell::switcher::WindowSwitcher;

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

fn until(mut predicate: impl FnMut() -> bool, seconds: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        settle(0.015);
        if predicate() {
            return true;
        }
    }
    false
}

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            panic!("check failed: {} (line {})", stringify!($cond), line!());
        }
    };
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            panic!("check failed: {} (line {}): {}", stringify!($cond), line!(), format!($($msg)+));
        }
    };
}

struct Recorder {
    switcher: RefCell<Option<Rc<WindowSwitcher>>>,
    requests: RefCell<Vec<Value>>,
}

impl Shell for Recorder {
    fn switch_window(&self, reverse: bool) -> bool {
        self.requests.borrow_mut().push(json!({"cmd": "switch-window", "reverse": reverse}));
        self.switcher.borrow().as_ref().is_some_and(|s| s.step(reverse))
    }
    fn switcher_status(&self) -> Option<Map<String, Value>> {
        self.switcher.borrow().as_ref().map(|s| s.status())
    }
    fn show_activities(&self, monitor: Option<&Value>) -> bool {
        self.requests.borrow_mut().push(json!({"cmd": "show-activities", "monitor": monitor}));
        self.switcher.borrow().as_ref().is_some_and(|s| s.show_activities(monitor))
    }
    fn monitor_keys(&self) -> Vec<String> {
        Vec::new()
    }
    fn apply_wallpaper(&self, _: &[String], _: Option<&str>, _: &str, _: &gdk::RGBA) {}
}

struct Fixture {
    app: gtk::Application,
    switcher: Rc<WindowSwitcher>,
    recorder: Rc<Recorder>,
    server: Rc<Server>,
    windows: Vec<gtk::ApplicationWindow>,
    clients: RefCell<Vec<Child>>,
    keys: RefCell<Vec<Child>>,
}

fn exe() -> String {
    std::env::current_exe().unwrap().to_string_lossy().into_owned()
}

impl Fixture {
    fn new() -> Self {
        let app = gtk::Application::new(Some("org.tessel.SwitcherFixture"), gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gio::Cancellable>).unwrap();
        let audio: tessel_shell::switcher::AudioFactory = Rc::new(|_| Rc::new(NoAudio) as Rc<dyn Audio>);
        let switcher = WindowSwitcher::new(&app, None, Some(audio), WindowSwitcher::default_view_factory(Rc::new(|_| None)), None);
        check!(until(|| switcher.backend.ready(), 5.0), "backend not ready: {}", switcher.backend.error());
        let recorder = Rc::new(Recorder { switcher: RefCell::new(Some(switcher.clone())), requests: RefCell::default() });
        let path = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap()).join("tessel-shell.sock");
        let server = Server::start(path, recorder.clone()).unwrap();
        let mut fixture = Fixture { app, switcher, recorder, server, windows: Vec::new(), clients: RefCell::default(), keys: RefCell::default() };
        for title in ["First window", "Second window", "Third window"] {
            if title == "Second window" {
                // A separate process/Wayland connection, like a real application.
                let child = Command::new(exe()).args(["--client-window", title]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
                fixture.clients.borrow_mut().push(child);
            } else {
                let window = gtk::ApplicationWindow::builder().application(&fixture.app).title(title).default_width(360).default_height(220).build();
                window.set_child(Some(&gtk::Label::new(Some(title))));
                window.present();
                fixture.windows.push(window);
            }
            let s = fixture.switcher.clone();
            check!(until(|| s.backend.windows().iter().any(|w| w.title == title), 5.0), "{title} never appeared");
        }
        // Exercise real focus history, independent of compositor startup order.
        for title in ["First window", "Second window", "Third window"] {
            let id = fixture.ident(title);
            check!(fixture.switcher.backend.activate(&id));
            let s = fixture.switcher.clone();
            check!(until(|| s.backend.window(&id).is_some_and(|w| w.activated), 3.0));
        }
        fixture
    }

    fn ident(&self, title: &str) -> String {
        self.switcher.backend.windows().into_iter().find(|w| w.title == title).map(|w| w.id).unwrap_or_default()
    }

    fn activated(&self, id: &str) -> bool {
        self.switcher.backend.window(id).is_some_and(|w| w.activated)
    }

    fn selected(&self) -> Option<String> {
        self.switcher.model.borrow().selected_id()
    }

    fn active(&self) -> bool {
        self.switcher.model.borrow().active
    }

    fn requests(&self) -> usize {
        self.recorder.requests.borrow().len()
    }

    /// The Python test's compact vocabulary, driven through XTEST.
    fn type_async(&self, args: &[&str]) -> usize {
        let mut command = Command::new("xdotool");
        for pair in args.chunks(2) {
            let modifier = |v: &str| match v { "alt" => "Alt_L", "shift" => "Shift_L", _ => "Control_L" }.to_string();
            match pair[0] {
                "-M" => { command.args(["keydown", &modifier(pair[1])]); }
                "-m" => { command.args(["keyup", &modifier(pair[1])]); }
                "-k" => { command.args(["key", "--delay", "0", pair[1]]); }
                "-s" => { command.args(["sleep", &format!("{}", pair[1].parse::<f64>().unwrap() / 1000.)]); }
                other => panic!("{other}"),
            }
        }
        let child = command.stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        self.keys.borrow_mut().push(child);
        self.keys.borrow().len() - 1
    }

    fn done(&self, index: usize) -> bool {
        self.keys.borrow_mut()[index].try_wait().ok().flatten().is_some()
    }

    fn view_active(&self) -> bool {
        self.switcher.view.borrow().as_ref().is_some_and(|v| v.window.is_active())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.switcher.close();
        self.server.close();
        for child in self.keys.borrow_mut().iter_mut().chain(self.clients.borrow_mut().iter_mut()) {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("xdotool").args(["keyup", "Alt_L", "Shift_L", "Control_L"]).status();
        for window in &self.windows {
            window.destroy();
        }
        settle(0.05);
    }
}

fn hold_tab_repeat_reverse_release_and_fast_tap() {
    let f = Fixture::new();
    let (third, second, first) = (f.ident("Third window"), f.ident("Second window"), f.ident("First window"));
    let keyboard = f.type_async(&["-M", "alt", "-k", "Tab", "-s", "600", "-k", "Tab", "-s", "400", "-M", "shift", "-k", "Tab", "-m", "shift", "-s", "400", "-m", "alt"]);
    check!(until(|| f.active(), 3.0));
    check!(f.selected() == Some(second.clone()));
    check!(until(|| f.view_active(), 3.0));
    let card = f.switcher.view.borrow().as_ref().unwrap().window.clone();
    check!(until(|| f.requests() >= 2, 3.0));
    check!(f.selected() == Some(first.clone()));
    check!(f.switcher.view.borrow().as_ref().unwrap().window == card);
    check!(until(|| f.requests() >= 3, 3.0));
    check!(f.selected() == Some(second.clone()));
    check!(until(|| f.done(keyboard) && !f.active(), 3.0));
    check!(until(|| f.activated(&second), 3.0));
    // Alt is released before the shortcut client can map the popup.
    let fast = f.type_async(&["-M", "alt", "-k", "Tab", "-m", "alt"]);
    check!(until(|| f.done(fast) && f.requests() == 4, 3.0));
    check!(until(|| !f.active() && f.activated(&third), 3.0));
}

fn escape_cancels_and_ignores_more_tabs_until_alt_repressed() {
    let f = Fixture::new();
    let third = f.ident("Third window");
    let keyboard = f.type_async(&["-M", "alt", "-k", "Tab", "-s", "500", "-k", "Escape", "-s", "250", "-k", "Tab", "-s", "250", "-m", "alt"]);
    check!(until(|| f.active(), 3.0));
    check!(until(|| f.requests() == 2, 3.0));
    check!(!f.active());
    check!(until(|| f.activated(&third), 3.0));
    check!(until(|| f.done(keyboard), 3.0));
    let next = f.type_async(&["-M", "alt", "-k", "Tab", "-s", "300", "-m", "alt"]);
    check!(until(|| f.active(), 3.0));
    check!(until(|| f.done(next) && !f.active(), 3.0));
    check!(!f.activated(&third));
}

fn minimized_window_restores_and_closed_selection_is_safe() {
    let f = Fixture::new();
    let second = f.ident("Second window");
    check!(f.switcher.backend.minimize(&second));
    check!(until(|| f.switcher.backend.window(&second).is_some_and(|w| w.minimized), 3.0));
    let keyboard = f.type_async(&["-M", "alt", "-k", "Tab", "-s", "500", "-m", "alt"]);
    check!(until(|| f.active(), 3.0));
    check!(f.selected() == Some(second.clone()));
    check!(until(|| f.done(keyboard) && !f.active(), 3.0));
    check!(until(|| f.switcher.backend.window(&second).is_some_and(|w| w.activated && !w.minimized), 3.0));
    let keyboard = f.type_async(&["-M", "alt", "-k", "Tab", "-s", "700", "-m", "alt"]);
    check!(until(|| f.active(), 3.0));
    let closing = f.selected().unwrap();
    f.switcher.backend.close_window(&closing);
    check!(until(|| !f.switcher.backend.contains(&closing), 3.0));
    check!(f.selected() != Some(closing.clone()));
    let survivor = f.selected().unwrap();
    check!(until(|| f.done(keyboard) && !f.active(), 3.0));
    check!(until(|| f.activated(&survivor), 3.0));
}

fn cli_command() -> Vec<String> {
    std::env::var("TESSEL_SHELL_TEST_CLI").unwrap_or_default().split_whitespace().map(str::to_string).collect()
}

fn activities_stays_open_then_alt_tab_uses_normal_release() {
    let f = Fixture::new();
    let monitor = tessel_shell::monitors::monitor_list(&gdk::Display::default().unwrap())[0].connector().unwrap().to_string();
    let cli = cli_command();
    let mut command = Command::new(&cli[0]);
    command.args(&cli[1..]).args(["show-activities", "--monitor", &monitor]).stdout(Stdio::null());
    let child = command.spawn().unwrap();
    f.keys.borrow_mut().push(child);
    let index = f.keys.borrow().len() - 1;
    check!(until(|| f.done(index) && f.switcher.mode() == Mode::Activities, 3.0));
    let (third, second) = (f.ident("Third window"), f.ident("Second window"));
    check!(f.selected() == Some(third.clone()));
    settle(0.3);
    check!(f.view_active());
    check!(f.switcher.mode() == Mode::Activities);
    let keyboard = f.type_async(&["-M", "alt", "-k", "Tab", "-s", "500", "-m", "alt"]);
    check!(until(|| f.switcher.mode() == Mode::Keyboard, 3.0));
    check!(f.selected() == Some(second.clone()));
    check!(f.switcher.view.borrow().as_ref().unwrap().hint.label().contains("Release Alt"));
    check!(until(|| f.done(keyboard) && f.switcher.mode() == Mode::Hidden, 3.0));
    check!(until(|| f.activated(&second), 3.0));
    // Activities does not require a fresh Alt press after a prior gesture.
    check!(f.switcher.show_activities(Some(&Value::String(monitor))));
    // The Python original sends Escape before the overlay maps and fails this
    // line the same way (keys typed before the surface exists go to the app);
    // wait for the overlay's keyboard focus first.
    check!(until(|| f.view_active(), 3.0));
    let escape = f.type_async(&["-k", "Escape"]);
    check!(until(|| f.done(escape) && f.switcher.mode() == Mode::Hidden, 3.0));
    check!(until(|| f.activated(&second), 3.0));
}

fn activities_close_targets_windows_without_activation_and_respects_refusal() {
    let f = Fixture::new();
    let (first, second, third) = (f.ident("First window"), f.ident("Second window"), f.ident("Third window"));
    check!(f.switcher.show_activities(None));
    check!(until(|| f.view_active(), 3.0));
    check!(f.selected() == Some(third.clone()));
    let activated_before: Vec<String> = Vec::new();
    let assert_overview = |selected: Option<&str>| {
        check!(f.switcher.mode() == Mode::Activities);
        let view = f.switcher.view.borrow().clone().unwrap();
        check!(view.window.is_visible());
        check!(view.window.is_active());
        check!(f.selected().as_deref() == selected, "{:?} != {:?}", f.selected(), selected);
        let _ = &activated_before;
    };
    let click_close = |id: &str| {
        let view = f.switcher.view.borrow().clone().unwrap();
        let close = view.cards.borrow().get(id).map(|c| c.close.clone()).unwrap();
        close.emit_clicked();
    };
    click_close(&second);
    check!(until(|| !f.switcher.backend.contains(&second), 3.0));
    check!(until(|| f.clients.borrow_mut()[0].try_wait().ok().flatten().is_some(), 3.0));
    check!(!f.switcher.view.borrow().as_ref().unwrap().cards.borrow().contains_key(&second));
    assert_overview(Some(&third));
    click_close(&third);
    check!(until(|| !f.switcher.backend.contains(&third), 3.0));
    assert_overview(Some(&first));
    // A refused close (unsaved work prompt) keeps the window and its card.
    let refusal = Rc::new(RefCell::new(false));
    let window = f.windows.iter().find(|w| w.title().as_deref() == Some("First window")).unwrap().clone();
    let r = refusal.clone();
    let handler = window.connect_close_request(move |_| {
        *r.borrow_mut() = true;
        glib::Propagation::Stop
    });
    click_close(&first);
    check!(until(|| *refusal.borrow(), 3.0));
    settle(0.1);
    check!(f.switcher.backend.contains(&first));
    check!(f.switcher.view.borrow().as_ref().unwrap().cards.borrow().contains_key(&first));
    assert_overview(Some(&first));
    window.disconnect(handler);
    click_close(&first);
    check!(until(|| !f.switcher.backend.contains(&first), 3.0));
    check!(f.switcher.view.borrow().as_ref().unwrap().cards.borrow().is_empty());
    check!(f.switcher.view.borrow().as_ref().unwrap().empty.is_visible());
    assert_overview(None);
}

fn fullscreen_geometry_survives_alt_tab() {
    // The adaptive top-bar half of this test belongs to tessel-dock; the switcher
    // half (exclusive_zone -1, no reservation) is checked here.
    let f = Fixture::new();
    let monitor = tessel_shell::monitors::monitor_list(&gdk::Display::default().unwrap())[0].clone();
    let game = f.windows.last().unwrap().clone();
    let (third, first) = (f.ident("Third window"), f.ident("First window"));
    f.switcher.backend.activate(&third);
    check!(until(|| f.activated(&third), 3.0));
    game.fullscreen_on_monitor(&monitor);
    check!(until(|| f.switcher.backend.window(&third).is_some_and(|w| w.fullscreen), 3.0));
    let expected = (monitor.geometry().width(), monitor.geometry().height());
    check!(until(|| (game.width(), game.height()) == expected, 3.0));
    let keyboard = f.type_async(&["-M", "alt", "-k", "Tab", "-s", "400", "-m", "alt"]);
    check!(until(|| f.active(), 3.0));
    check!(until(|| f.done(keyboard) && !f.active(), 3.0));
    check!(until(|| f.activated(&first) || f.activated(&f.ident("Second window")), 3.0));
    settle(0.3);
    check!(f.switcher.backend.window(&third).is_some_and(|w| w.fullscreen));
    check!((game.width(), game.height()) == expected);
}

/// Alt+Tab open latency: XTEST Alt+Tab → (labwc keybind → CLI process → socket →
/// step → present) → the switcher has keyboard focus. Prints `latency_ms=…`.
fn latency() {
    let f = Fixture::new();
    let mut samples = Vec::new();
    for _ in 0..12 {
        let t0 = Instant::now();
        let index = f.type_async(&["-M", "alt", "-k", "Tab"]);
        check!(until(|| f.view_active(), 3.0));
        samples.push(t0.elapsed().as_secs_f64() * 1000.);
        let up = f.type_async(&["-m", "alt"]);
        check!(until(|| f.done(index) && f.done(up) && !f.active(), 3.0));
        settle(0.25);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("latency_ms median={:.1} min={:.1} max={:.1} samples={:?}", samples[samples.len() / 2], samples[0],
             samples[samples.len() - 1], samples.iter().map(|s| (s * 10.).round() / 10.).collect::<Vec<_>>());
}

fn client_window(title: &str) -> i32 {
    let app = gtk::Application::new(Some("org.tessel.ExternalWindowFixture"), gio::ApplicationFlags::NON_UNIQUE);
    let title = title.to_string();
    app.connect_activate(move |app| {
        let window = gtk::ApplicationWindow::builder().application(app).title(&title).default_width(360).default_height(220).build();
        window.set_child(Some(&gtk::Label::new(Some(&title))));
        window.present();
    });
    let code = app.run_with_args(&["fixture"]);
    i32::from(code)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--test-threads") && a != "--nocapture").collect();
    if std::env::var("TESSEL_DOCK_TEST_ISOLATED").as_deref() != Ok("1") {
        println!("switcher_native: skipped (run tests/run_switcher_native.sh)");
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--client-window") {
        std::process::exit(client_window(&args[i + 1]));
    }
    gtk::init().unwrap();
    let tests: Vec<(&str, fn())> = vec![
        ("hold_tab_repeat_reverse_release_and_fast_tap", hold_tab_repeat_reverse_release_and_fast_tap),
        ("escape_cancels_and_ignores_more_tabs_until_alt_repressed", escape_cancels_and_ignores_more_tabs_until_alt_repressed),
        ("minimized_window_restores_and_closed_selection_is_safe", minimized_window_restores_and_closed_selection_is_safe),
        ("activities_stays_open_then_alt_tab_uses_normal_release", activities_stays_open_then_alt_tab_uses_normal_release),
        ("activities_close_targets_windows_without_activation_and_respects_refusal", activities_close_targets_windows_without_activation_and_respects_refusal),
        ("fullscreen_geometry_survives_alt_tab", fullscreen_geometry_survives_alt_tab),
        ("latency", latency),
    ];
    let name = args.first().cloned().unwrap_or_default();
    match tests.iter().find(|(n, _)| *n == name) {
        Some((n, test)) => {
            test();
            println!("test {n} ... ok");
        }
        None => {
            for (n, _) in &tests {
                println!("{n}");
            }
        }
    }
}
