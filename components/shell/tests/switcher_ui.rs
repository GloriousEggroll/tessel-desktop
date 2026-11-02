//! Port of test_switcher_ui.py (SwitcherUITests), test_overview.py
//! (OverviewTests) and test_shell_integration.py (DaemonTests) for a private
//! headless labwc with two outputs (tests/run_headless_switcher.sh). Under a
//! plain `cargo test` this binary skips itself.

use gtk::prelude::*;
use gtk::{gdk, gio};
use gtk4_layer_shell::{KeyboardMode, LayerShell};
use serde_json::Value;
use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_shell::switcher::overview::{CaptureFrame, OverviewCapture};
use tessel_shell::switcher::ui::{Item, Mode, SwitcherView, STYLE};

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

macro_rules! check {
    ($cond:expr) => { if !$cond { panic!("check failed: {} (line {})", stringify!($cond), line!()); } };
    ($cond:expr, $($msg:tt)+) => { if !$cond { panic!("check failed: {} (line {}): {}", stringify!($cond), line!(), format!($($msg)+)); } };
}

type Actions = Rc<RefCell<Vec<(String, String)>>>;

struct Ui {
    app: gtk::Application,
    view: Rc<SwitcherView>,
    actions: Actions,
    monitors: Vec<gdk::Monitor>,
}

fn view(app: &gtk::Application, actions: &Actions, close: bool, mute: bool) -> Rc<SwitcherView> {
    let (a1, a2, a3, a4) = (actions.clone(), actions.clone(), actions.clone(), actions.clone());
    SwitcherView::new(app, Rc::new(move |id: &str| a1.borrow_mut().push(("activate".into(), id.into()))),
                      Rc::new(move || a2.borrow_mut().push(("cancel".into(), String::new()))),
                      close.then(|| Rc::new(move |id: &str| a3.borrow_mut().push(("close".into(), id.into()))) as Rc<dyn Fn(&str)>),
                      mute.then(|| Rc::new(move |id: &str| a4.borrow_mut().push(("mute".into(), id.into()))) as Rc<dyn Fn(&str)>),
                      Rc::new(|_| None))
}

impl Ui {
    fn new(sequence: usize) -> Self {
        let app = gtk::Application::new(Some(&format!("org.tessel.SwitcherTest{sequence}")), gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gio::Cancellable>).unwrap();
        let actions: Actions = Rc::default();
        let view = view(&app, &actions, false, false);
        let mut monitors = tessel_shell::monitors::monitor_list(&gdk::Display::default().unwrap());
        monitors.sort_by_key(|m| m.connector().map(|c| c.to_string()));
        Ui { app, view, actions, monitors }
    }

    fn replace(&mut self, close: bool, mute: bool) {
        self.view.close();
        self.view = view(&self.app, &self.actions, close, mute);
    }

    fn actions(&self) -> Vec<(String, String)> {
        self.actions.borrow().clone()
    }

    fn point_at(&self, widget: &impl IsA<gtk::Widget>) {
        let bounds = widget.compute_bounds(&self.view.window).expect("bounds");
        let geometry = self.view.monitor.borrow().as_ref().unwrap().geometry();
        let x = geometry.x() as f32 + (geometry.width() - self.view.window.width()) as f32 / 2. + bounds.x() + bounds.width() / 2.;
        let y = geometry.y() as f32 + (geometry.height() - self.view.window.height()) as f32 / 2. + bounds.y() + bounds.height() / 2.;
        wlrctl(&["pointer", "move", "-100000", "-100000"]);
        wlrctl(&["pointer", "move", &(x as i32).to_string(), &(y as i32).to_string()]);
        settle(0.08);
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.view.close();
        settle(0.08);
    }
}

fn wlrctl(args: &[&str]) {
    let status = Command::new("wlrctl").args(args).status().unwrap();
    assert!(status.success());
}

fn items(count: usize) -> Vec<Item> {
    (0..count).map(|i| Item {
        id: i.to_string(), name: ["Terminal", "Web Browser", "Files"][i % 3].into(),
        title: format!("Project {i} — a long window title that should be ellipsized"),
        icon: Some(gio::ThemedIcon::new(["utilities-terminal", "web-browser", "system-file-manager"][i % 3]).upcast()),
        minimized: i == 1, ..Default::default()
    }).collect()
}

fn card<F: Fn(&tessel_shell::switcher::ui::Card) -> R, R>(v: &SwitcherView, id: &str, f: F) -> R {
    f(&v.cards.borrow()[id])
}

fn title_updates_keep_widgets_and_callbacks_alive(ui: &mut Ui) {
    let mut list = items(8);
    ui.view.render(&list, Some("0"));
    ui.view.show(Some(ui.monitors[0].clone()));
    settle(0.08);
    let cards: Vec<(String, gtk::Button, gtk::FlowBoxChild)> = ui.view.cards.borrow().iter().map(|(k, c)| (k.clone(), c.button.clone(), c.child.clone())).collect();
    check!(ui.view.window.keyboard_mode() == KeyboardMode::Exclusive);
    check!(ui.view.backdrop().unwrap().keyboard_mode() == KeyboardMode::None);
    check!(ui.view.window.is_visible());
    check!(ui.view.backdrop().unwrap().is_visible());
    check!(card(&ui.view, "0", |c| c.button.has_css_class("selected")));
    check!(card(&ui.view, "1", |c| c.button.has_css_class("minimized")));
    check!(ui.view.cards.borrow().values().all(|c| !c.close.property::<bool>("visible")));
    for index in 0..8 {
        list[0].title = format!("Build step {index} in progress");
        ui.view.render(&list, Some("2"));
    }
    settle(0.08);
    for (id, button, child) in &cards {
        check!(card(&ui.view, id, |c| c.button == *button && c.child == *child));
        check!(child.parent().as_ref() == Some(ui.view.grid.upcast_ref()));
    }
    check!(card(&ui.view, "0", |c| c.title.label() == "Build step 7 in progress"));
    check!(!card(&ui.view, "0", |c| c.button.has_css_class("selected")));
    check!(card(&ui.view, "2", |c| c.button.has_css_class("selected")));
    card(&ui.view, "3", |c| c.button.emit_clicked());
    ui.view.outside_click().unwrap().emit_by_name::<()>("released", &[&1i32, &10f64, &10f64]);
    check!(ui.actions() == vec![("activate".into(), "3".into()), ("cancel".into(), String::new())], "{:?}", ui.actions());
    let mut reordered: Vec<Item> = list[2..].to_vec();
    reordered.push(list[0].clone());
    ui.view.render(&reordered, Some("2"));
    check!(!ui.view.cards.borrow().contains_key("1"));
    check!(card(&ui.view, "0", |c| c.button == cards.iter().find(|x| x.0 == "0").unwrap().1));
}

fn card_actions_follow_audio_and_selection(ui: &mut Ui) {
    ui.replace(true, true);
    let mut list = items(3);
    list[1].audio_available = true;
    ui.view.render(&list, Some("0"));
    check!(!card(&ui.view, "0", |c| c.mute.property::<bool>("visible")), "No stream: no mute control");
    check!(card(&ui.view, "1", |c| c.mute.property::<bool>("visible")), "on_mute={} sensitive={}", ui.view.on_mute.borrow().is_some(), card(&ui.view, "1", |c| c.mute.is_sensitive()));
    check!(card(&ui.view, "0", |c| c.tile.has_css_class("selected")));
    check!(!card(&ui.view, "1", |c| c.tile.has_css_class("selected")));
    list[1].audio_available = false;
    ui.view.render(&list, Some("1"));
    check!(!card(&ui.view, "1", |c| c.mute.property::<bool>("visible")));
    check!(card(&ui.view, "1", |c| c.tile.has_css_class("selected")));
    check!(!card(&ui.view, "0", |c| c.tile.has_css_class("selected")));
    check!(card(&ui.view, "0", |c| c.preview.content_fit() == gtk::ContentFit::ScaleDown));
}

fn real_close_click_is_independent_and_survives_title_updates(ui: &mut Ui) {
    ui.replace(true, false);
    let mut list = items(8);
    ui.view.render(&list, Some("0"));
    ui.view.show(Some(ui.monitors[0].clone()));
    settle(0.08);
    let (button, close, tile) = card(&ui.view, "0", |c| (c.button.clone(), c.close.clone(), c.tile.clone()));
    check!(button.parent().as_ref() == Some(tile.upcast_ref()));
    check!(close.parent().as_ref() == Some(tile.upcast_ref()), "Close must be a sibling of the activation button");
    let image = close.child().and_downcast::<gtk::Image>().unwrap();
    check!(image.icon_name().as_deref() == Some("window-close-symbolic"));
    check!(image.pixel_size() == 12);
    check!(close.tooltip_text().as_deref() == Some("Close window"));
    check!(close.width() <= 28 && close.height() <= 28);
    let bounds = close.compute_bounds(&tile).unwrap();
    check!(bounds.x() + bounds.width() >= tile.width() as f32 - 5.);
    check!(bounds.y() <= 5.);
    ui.point_at(&close);
    for index in 0..12 {
        list[0].title = format!("Build step {index} is running");
        list[0].minimized = index % 2 == 1;
        ui.view.render(&list, Some("1"));
        settle(0.012);
    }
    check!(card(&ui.view, "0", |c| c.button == button && c.close == close && c.tile == tile));
    check!(close.state_flags().contains(gtk::StateFlags::PRELIGHT));
    wlrctl(&["pointer", "click"]);
    settle(0.12);
    check!(ui.actions() == vec![("close".into(), "0".into())], "{:?}", ui.actions());
    check!(ui.view.window.is_visible());
    ui.view.render(&list[1..], Some("1"));
    close.emit_clicked();
    check!(ui.actions() == vec![("close".into(), "0".into())], "Stale removed cards must not issue another close");
    settle(0.08);
    let other = card(&ui.view, "1", |c| c.button.clone());
    ui.point_at(&other);
    wlrctl(&["pointer", "click"]);
    settle(0.12);
    check!(ui.actions() == vec![("close".into(), "0".into()), ("activate".into(), "1".into())], "{:?}", ui.actions());
}

fn many_windows_scroll_and_surfaces_follow_selected_monitor(ui: &mut Ui) {
    check!(ui.monitors.len() >= 2, "needs two outputs");
    ui.view.render(&items(120), Some("119"));
    ui.view.show(Some(ui.monitors[1].clone()));
    settle(0.25);
    let geometry = ui.monitors[1].geometry();
    check!(ui.view.window.monitor().as_ref() == Some(&ui.monitors[1]));
    check!(ui.view.backdrop().unwrap().monitor().as_ref() == Some(&ui.monitors[1]));
    check!(ui.view.window.width() <= 800.min(geometry.width() - 48));
    check!(ui.view.window.height() <= geometry.height() - 48);
    let adjustment = ui.view.scroller.vadjustment();
    check!(adjustment.upper() > adjustment.page_size());
    check!(adjustment.value() > 0., "{}", adjustment.value());
    ui.view.dismiss();
    check!(!ui.view.window.is_visible());
    check!(!ui.view.backdrop().unwrap().is_visible());
    ui.view.render(&items(1), Some("0"));
    ui.view.show(Some(ui.monitors[0].clone()));
    settle(0.08);
    check!(ui.view.window.monitor().as_ref() == Some(&ui.monitors[0]));
    check!(ui.view.window.height() <= 300, "{}", ui.view.window.height());
}

fn manual_scroll_survives_window_updates_but_keyboard_selection_is_revealed(ui: &mut Ui) {
    let mut list = items(120);
    ui.view.set_mode(Mode::Activities);
    ui.view.render(&list, Some("0"));
    ui.view.show(Some(ui.monitors[0].clone()));
    settle(0.25);
    let adjustment = ui.view.scroller.vadjustment();
    adjustment.set_value(450.);
    settle(0.08);
    for index in 0..8 {
        list[0].title = format!("Working {index}");
        list[1].minimized = index % 2 == 1;
        ui.view.render(&list, Some("0"));
        settle(0.025);
        check!((adjustment.value() - 450.).abs() <= 1., "{}", adjustment.value());
    }
    ui.view.render(&list, Some("119"));
    settle(0.08);
    check!(adjustment.value() > 450.);
}

fn fallback_css_parses(_ui: &mut Ui) {
    let errors = Rc::new(RefCell::new(Vec::<String>::new()));
    let provider = gtk::CssProvider::new();
    let e = errors.clone();
    provider.connect_parsing_error(move |_, _, error| e.borrow_mut().push(error.to_string()));
    provider.load_from_string(STYLE);
    check!(errors.borrow().is_empty(), "{:?}", errors.borrow());
}

fn mute_click_does_not_activate_or_close(ui: &mut Ui) {
    ui.replace(true, true);
    let mut list = items(2);
    list[0].audio_available = true;
    list[0].muted = true;
    ui.view.render(&list, Some("0"));
    ui.view.show(Some(ui.monitors[0].clone()));
    settle(0.15);
    let mute = card(&ui.view, "0", |c| c.mute.clone());
    check!(mute.child().and_downcast::<gtk::Image>().unwrap().icon_name().as_deref() == Some("audio-volume-muted-symbolic"));
    check!(!card(&ui.view, "1", |c| c.mute.is_sensitive()));
    ui.point_at(&mute);
    wlrctl(&["pointer", "click"]);
    settle(0.08);
    check!(ui.actions() == vec![("mute".into(), "0".into())], "{:?}", ui.actions());
}

fn target_window(app: &gtk::Application, title: &str, w: i32, h: i32) -> gtk::Window {
    let target = gtk::Window::builder().application(app).title(title).default_width(w).default_height(h).build();
    target.set_child(Some(&gtk::Label::new(Some("A real native window preview"))));
    target.present();
    target
}

fn real_window_capture_and_overview_cleanup(ui: &mut Ui) {
    let target = target_window(&ui.app, "Unique overview native capture target", 640, 400);
    settle(0.15);
    let frames: Rc<RefCell<Vec<(String, String, i32)>>> = Rc::default();
    let done = Rc::new(RefCell::new(false));
    let (f, d) = (frames.clone(), done.clone());
    let capture = OverviewCapture::new(move |frame| {
        let entry = match frame {
            CaptureFrame::Desktop(t) => ("desktop".into(), String::new(), t.width()),
            CaptureFrame::Identities(_) => ("identities".into(), String::new(), 0),
            CaptureFrame::Window { key, texture } => (key.0, key.1, texture.width()),
        };
        f.borrow_mut().push(entry);
    }, move || *d.borrow_mut() = true);
    let started = Instant::now();
    capture.start(&ui.monitors[0].connector().unwrap());
    for _ in 0..60 {
        settle(0.05);
        if *done.borrow() {
            break;
        }
    }
    println!("capture_complete_ms={:.0}", started.elapsed().as_secs_f64() * 1000.);
    capture.close();
    check!(*done.borrow());
    check!(frames.borrow().iter().any(|f| f.0 == "desktop"));
    let native = frames.borrow().iter().find(|f| f.1 == "Unique overview native capture target").cloned();
    check!(native.is_some(), "Compositor must produce a window preview: {:?}", frames.borrow());
    let native = native.unwrap();
    check!(native.2 > 100 && native.2 <= 640, "{}", native.2);
    let item = Item { id: "native".into(), name: "Test".into(), title: target.title().unwrap().to_string(), app_id: native.0.clone(), ..Default::default() };
    ui.view.set_mode(Mode::Activities);
    ui.view.render(&[item], Some("native"));
    let t0 = Instant::now();
    ui.view.show(Some(ui.monitors[0].clone()));
    let mut mapped = None;
    let mut previewed = None;
    while t0.elapsed() < Duration::from_millis(1500) {
        settle(0.005);
        if mapped.is_none() && ui.view.window.is_mapped() {
            mapped = Some(t0.elapsed());
        }
        if previewed.is_none() && card(&ui.view, "native", |c| c.preview.paintable().is_some()) {
            previewed = Some(t0.elapsed());
        }
        if mapped.is_some() && previewed.is_some() && t0.elapsed() > Duration::from_millis(800) {
            break;
        }
    }
    println!("overview_open_ms mapped={:?} first_preview={:?}", mapped.map(|d| d.as_millis()), previewed.map(|d| d.as_millis()));
    let geometry = ui.monitors[0].geometry();
    check!(card(&ui.view, "native", |c| c.preview.paintable().is_some()));
    check!((ui.view.window.width(), ui.view.window.height()) == (geometry.width(), geometry.height()));
    check!(ui.view.stage.progress() == 1.);
    check!(!ui.view.stage.has_desktop());
    ui.view.dismiss();
    check!(card(&ui.view, "native", |c| c.preview.paintable().is_none()));
    check!(ui.view.capture.borrow().is_none());
    check!(!ui.view.stage.tick_active());
    target.destroy();
}

fn overview_visual_layout(ui: &mut Ui) {
    ui.replace(true, true);
    let names = ["Project notes", "Music library", "Files and folders", "Desktop settings"];
    let icons = ["accessories-text-editor", "multimedia-player", "system-file-manager", "preferences-system"];
    let targets: Vec<gtk::Window> = names.iter().enumerate().map(|(i, n)| target_window(&ui.app, n, 620 - i as i32 * 50, 390 + i as i32 * 20)).collect();
    settle(0.3);
    let app_id = ui.app.application_id().unwrap().to_string();
    let list: Vec<Item> = names.iter().enumerate().map(|(i, n)| Item {
        id: i.to_string(), name: n.to_string(), title: n.to_string(), app_id: app_id.clone(),
        icon: Some(gio::ThemedIcon::new(icons[i]).upcast()), ..Default::default()
    }).collect();
    ui.view.set_mode(Mode::Activities);
    ui.view.render(&list, Some("0"));
    ui.view.show(Some(ui.monitors[0].clone()));
    settle(1.2);
    check!(ui.view.columns() == 2, "{}", ui.view.columns());
    let adj = ui.view.scroller.vadjustment();
    check!(adj.upper() <= adj.page_size() + 1.);
    let _ = Command::new("grim").args(["-o", &ui.monitors[0].connector().unwrap(), &format!("{}/tessel-overview-review.png", std::env::var("TMPDIR").unwrap())]).status();
    for c in ui.view.cards.borrow().values() {
        check!(c.preview.paintable().is_some());
    }
    for t in targets {
        t.destroy();
    }
}

// ----------------------------------------------------------- DaemonTests

fn run_cli(cli: &[String], args: &[&str]) -> (i32, String, String) {
    let out = Command::new(&cli[0]).args(&cli[1..]).args(args).stdin(Stdio::null()).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// test_shell_integration: the real daemon in a private HOME, driven by both
/// the keyboard launcher and the daemon binary's own CLI. `DAEMON` and the
/// CLIs come from the runner (Rust, or Python for cross-checks).
fn real_daemon_starts_switcher_and_forwards_socket_commands(ui: &mut Ui) {
    let words = |v: &str| std::env::var(v).unwrap().split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let (daemon, key_cli, own_cli) = (words("WAYSHELL_DAEMON"), words("WAYSHELL_KEY_CLI"), words("WAYSHELL_DAEMON"));
    let home = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap()).join("home");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    let socket = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap()).join("tessel-shell.sock");
    // A stale socket from a previous daemon must not break startup.
    let _ = std::fs::remove_file(&socket);
    let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    drop(stale);
    let log = std::fs::File::create(home.join("daemon.log")).unwrap();
    let t0 = Instant::now();
    let mut process = Command::new(&daemon[0]).args(&daemon[1..]).env("HOME", &home).env("XDG_CONFIG_HOME", home.join(".config"))
        .stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap();
    let mut ready: Option<Value> = None;
    while t0.elapsed() < Duration::from_secs(8) && process.try_wait().unwrap().is_none() {
        let (code, out, _) = run_cli(&key_cli, &["switcher-status"]);
        if code == 0 {
            let v: Value = serde_json::from_str(&out).unwrap();
            if v["ready"] == Value::Bool(true) {
                ready = Some(v);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("daemon_ready_ms={}", t0.elapsed().as_millis());
    check!(process.try_wait().unwrap().is_none(), "{}", std::fs::read_to_string(home.join("daemon.log")).unwrap_or_default());
    let ready = ready.expect("ready");
    check!(ready["visible"] == Value::Bool(false));
    check!(ready["windows"] == 0, "Desktop layers must not become window-switcher cards");
    let pid = process.id();
    let rss = |pid: u32| {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let smaps = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).unwrap_or_default();
        let field = |text: &str, key: &str| text.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).unwrap_or("?").to_string();
        format!("rss_kb={} pss_kb={}", field(&status, "VmRSS:"), field(&smaps, "Pss:"))
    };
    std::thread::sleep(Duration::from_secs(3));
    println!("daemon_idle {}", rss(pid));
    for args in [&["switch-window"][..], &["switch-window", "--reverse"], &["list-monitors"]] {
        let launchers: Vec<&Vec<String>> = if args[0] == "switch-window" { vec![&key_cli, &own_cli] } else { vec![&own_cli] };
        for launcher in launchers {
            let (code, out, err) = run_cli(launcher, args);
            check!(code == 0, "{launcher:?} {args:?}: {out}{err}");
        }
    }
    let (_, out, _) = run_cli(&own_cli, &["list-monitors"]);
    check!(out.lines().count() == ui.monitors.len(), "{out}");
    let geometry = ui.monitors[1].geometry();
    for (launcher, monitor) in [(&key_cli, "HEADLESS-2"), (&own_cli, "HEADLESS-1")] {
        let (code, out, err) = run_cli(launcher, &["show-activities", "--monitor", monitor]);
        check!(code == 0, "{out}{err}");
        std::thread::sleep(Duration::from_millis(250));
        let (_, out, _) = run_cli(&key_cli, &["switcher-status"]);
        let status: Value = serde_json::from_str(&out).unwrap();
        check!(status["visible"] == Value::Bool(true));
        check!(status["mode"] == "activities");
        check!(status["monitor"] == monitor);
        check!(status["windows"] == 0);
        for cmd in [vec!["pointer", "move", "-100000", "-100000"], vec!["pointer", "move", &(geometry.x() + 30).to_string(), &(geometry.y() + 30).to_string()], vec!["pointer", "click"]] {
            let cmd: Vec<&str> = cmd.iter().map(|s| s.as_ref()).collect();
            wlrctl(&cmd);
            std::thread::sleep(Duration::from_millis(30));
        }
        let (_, out, _) = run_cli(&key_cli, &["switcher-status"]);
        let status: Value = serde_json::from_str(&out).unwrap();
        check!(status["visible"] == Value::Bool(false), "Outside click did not dismiss real Activities daemon");
    }
    println!("daemon_after_activities {}", rss(pid));
    let (code, _, _) = run_cli(&key_cli, &["show-activities", "--monitor", "disconnected-output"]);
    check!(code == 1);
    // `set` with "file": null (Global Settings, Color Only) over the raw socket.
    let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    use std::io::{Read, Write};
    stream.write_all(b"{\"cmd\": \"set\", \"monitor\": \"HEADLESS-1\", \"file\": null, \"mode\": \"Color Only\", \"color\": \"rgb(10,20,30)\"}\n").unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    check!(reply.trim() == r#"{"ok": true, "msg": "OK"}"#, "{reply}");
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    let _ = process.wait();
    let log = std::fs::read_to_string(home.join("daemon.log")).unwrap_or_default();
    check!(!log.contains("Traceback") && !log.contains("panicked"), "{log}");
    // Shutdown leaves the socket file behind (Python parity).
    check!(socket.exists());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    if std::env::var("WAYDOCK_TEST_ISOLATED").as_deref() != Ok("1") {
        println!("switcher_ui: skipped (run tests/run_headless_switcher.sh)");
        return;
    }
    gtk::init().unwrap();
    let tests: Vec<(&str, fn(&mut Ui))> = vec![
        ("title_updates_keep_widgets_and_callbacks_alive", title_updates_keep_widgets_and_callbacks_alive),
        ("card_actions_follow_audio_and_selection", card_actions_follow_audio_and_selection),
        ("real_close_click_is_independent_and_survives_title_updates", real_close_click_is_independent_and_survives_title_updates),
        ("many_windows_scroll_and_surfaces_follow_selected_monitor", many_windows_scroll_and_surfaces_follow_selected_monitor),
        ("manual_scroll_survives_window_updates_but_keyboard_selection_is_revealed", manual_scroll_survives_window_updates_but_keyboard_selection_is_revealed),
        ("fallback_css_parses", fallback_css_parses),
        ("mute_click_does_not_activate_or_close", mute_click_does_not_activate_or_close),
        ("real_window_capture_and_overview_cleanup", real_window_capture_and_overview_cleanup),
        ("overview_visual_layout", overview_visual_layout),
        ("real_daemon_starts_switcher_and_forwards_socket_commands", real_daemon_starts_switcher_and_forwards_socket_commands),
    ];
    let mut failed = 0;
    for (i, (name, test)) in tests.iter().enumerate() {
        if !args.is_empty() && !args.iter().any(|a| a == name) {
            continue;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut ui = Ui::new(i);
            test(&mut ui);
        }));
        match result {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failed += 1;
            }
        }
    }
    println!("switcher_ui: {failed} failed");
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
