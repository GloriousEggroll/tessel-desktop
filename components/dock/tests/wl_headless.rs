//! tessel_dock::wl against a real compositor: GTK test windows seen, activated,
//! minimized, maximized/fullscreened and closed through `WindowBackend`;
//! show desktop; `WorkspaceBackend` lists/activates the private desktops
//! (port of test_workspace_native.py); the fullscreen guard on native
//! fullscreen and — when Xwayland is available — on a borderless X11 window
//! (port of FullscreenGuardNativeTests).
//!
//! Runs only inside the private headless labwc of tests/run_wl_headless.sh
//! (TESSEL_DOCK_WL_HEADLESS=1, private XDG_RUNTIME_DIR); anywhere else it skips
//! without connecting to anything.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tessel_dock::wl::fullscreen_guard::{self, FullscreenCache, FullscreenGuard};
use tessel_dock::wl::show_desktop::ShowDesktop;
use tessel_dock::wl::{WindowBackend, WorkspaceBackend};

const APP_ID: &str = "org.tessel.WlTest";
const X11_TITLE: &str = "Fullscreen guard X11 fixture";

fn wait_for(seconds: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(seconds: f64) { wait_for(seconds, || false); }

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => { if !$cond { return Err(format!($($msg)+)); } };
}

struct World {
    windows: WindowBackend,
    workspaces: WorkspaceBackend,
    errors: Rc<RefCell<Vec<String>>>,
    changes: Rc<RefCell<usize>>,
}

impl World {
    fn id(&self, title: &str) -> Option<String> {
        self.windows.windows().values().find(|w| w.title == title).map(|w| w.id.clone())
    }
    fn window(&self, title: &str) -> Option<tessel_dock::wl::Window> {
        self.windows.windows().values().find(|w| w.title == title).cloned()
    }
    fn gtk_window(&self, title: &str) -> Result<(gtk::Window, String), String> {
        let window = gtk::Window::builder().title(title).default_width(360).default_height(240).build();
        window.set_child(Some(&gtk::Label::new(Some(title))));
        window.present();
        ensure!(wait_for(10.0, || self.id(title).is_some()), "{title} never appeared: {:?}", self.windows.windows());
        Ok((window, self.id(title).unwrap()))
    }
}

fn check_windows(w: &World) -> Result<(), String> {
    ensure!(w.windows.ready() && w.windows.can_activate(), "not ready: {}", w.windows.error());
    ensure!(w.windows.protocol_version() == 3, "version {}", w.windows.protocol_version());
    let outputs: Vec<_> = w.windows.outputs().values().cloned().collect();
    ensure!(outputs.len() == 2 && outputs.iter().all(|o| o.width > 0 && o.height > 0 && o.scale == 1),
            "outputs {outputs:?}");
    ensure!(outputs.iter().any(|o| o.name == "HEADLESS-1"), "names {outputs:?}");
    let (alpha, alpha_id) = w.gtk_window("Alpha")?;
    let (beta, beta_id) = w.gtk_window("Beta")?;
    let session = w.windows.session();
    ensure!(alpha_id.starts_with(&format!("{session}:")), "id {alpha_id}");
    let a = w.window("Alpha").unwrap();
    ensure!(a.app_id == APP_ID && a.outputs.len() == 1 && a.parent_id.is_empty(), "alpha {a:?}");
    ensure!(*w.changes.borrow() > 0, "on_changed never ran");

    for (id, title, other) in [(&alpha_id, "Alpha", "Beta"), (&beta_id, "Beta", "Alpha")] {
        ensure!(w.windows.activate(id), "activate refused: {}", w.windows.error());
        ensure!(wait_for(5.0, || w.window(title).is_some_and(|x| x.activated) && w.window(other).is_some_and(|x| !x.activated)),
                "{title} not activated: {:?}", w.window(title));
    }
    ensure!(w.windows.minimize(&beta_id), "minimize refused");
    ensure!(wait_for(5.0, || w.window("Beta").is_some_and(|x| x.minimized)), "Beta not minimized");
    ensure!(w.windows.unminimize(&beta_id), "unminimize refused");
    ensure!(wait_for(5.0, || w.window("Beta").is_some_and(|x| !x.minimized)), "Beta not restored");
    ensure!(w.windows.minimize(&beta_id) && wait_for(5.0, || w.window("Beta").is_some_and(|x| x.minimized)), "re-minimize");
    ensure!(w.windows.activate(&beta_id), "activate");
    ensure!(wait_for(5.0, || w.window("Beta").is_some_and(|x| !x.minimized && x.activated)), "activate did not restore");

    alpha.maximize();
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| x.maximized && !x.fullscreen)), "maximized ≠ fullscreen: {:?}", w.window("Alpha"));
    alpha.unmaximize();
    alpha.fullscreen();
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| x.fullscreen && !x.maximized)), "fullscreen: {:?}", w.window("Alpha"));
    alpha.unfullscreen();
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| !x.fullscreen)), "unfullscreen");

    ensure!(!w.windows.activate("0:1") && w.windows.error() == "That window is no longer available.", "stale id accepted");
    let closes = Rc::new(RefCell::new(0));
    let counter = closes.clone();
    beta.connect_close_request(move |_| {
        *counter.borrow_mut() += 1;
        glib::Propagation::Proceed
    });
    ensure!(w.windows.close_window(&beta_id), "close refused");
    ensure!(wait_for(5.0, || w.id("Beta").is_none()), "Beta not closed");
    ensure!(*closes.borrow() == 1, "close-request count {}", closes.borrow());
    ensure!(!w.windows.close_window(&beta_id), "closed id accepted");
    alpha.destroy();
    ensure!(wait_for(5.0, || w.id("Alpha").is_none()), "Alpha not gone");
    w.errors.borrow_mut().retain(|e| e != "That window is no longer available.");
    Ok(())
}

/// test_native_integration: show desktop preserves pre-existing minimization
/// and the maximized state.
fn show_desktop(w: &World) -> Result<(), String> {
    let (normal, normal_id) = w.gtk_window("Desktop toggle normal")?;
    let (maximized, maximized_id) = w.gtk_window("Desktop toggle maximized")?;
    let (minimized, minimized_id) = w.gtk_window("Desktop toggle already minimized")?;
    w.windows.minimize(&minimized_id);
    maximized.maximize();
    w.windows.activate(&maximized_id);
    let state = |id: &str| w.windows.window(id);
    ensure!(wait_for(5.0, || state(&minimized_id).is_some_and(|x| x.minimized)
                     && state(&maximized_id).is_some_and(|x| x.activated && x.maximized) && maximized.is_maximized()),
            "setup {:?}", w.windows.windows());
    let mut desktop = ShowDesktop::new();
    let mut statuses = Vec::new();
    statuses.extend(desktop.toggle(&w.windows, || {}));
    ensure!(wait_for(5.0, || [&normal_id, &maximized_id, &minimized_id].iter().all(|id| state(id).is_some_and(|x| x.minimized))),
            "not all minimized");
    ensure!(desktop.windows().len() == 2 && desktop.focus() == Some(maximized_id.as_str()), "remembered {desktop:?}");
    statuses.extend(desktop.toggle(&w.windows, || {}));
    ensure!(wait_for(5.0, || state(&normal_id).is_some_and(|x| !x.minimized)
                     && state(&maximized_id).is_some_and(|x| !x.minimized && x.activated)), "not restored");
    settle(0.2);
    ensure!(state(&minimized_id).is_some_and(|x| x.minimized), "pre-minimized window was restored");
    ensure!(maximized.is_maximized() && state(&maximized_id).is_some_and(|x| x.maximized), "maximized state lost");
    ensure!(statuses == [true, false], "statuses {statuses:?}");
    // Closing every hidden window resets the state.
    desktop.toggle(&w.windows, || {});
    ensure!(wait_for(5.0, || state(&normal_id).is_some_and(|x| x.minimized)), "second hide");
    for window in [&normal, &maximized, &minimized] {
        window.destroy();
    }
    ensure!(wait_for(5.0, || w.windows.windows().is_empty()), "windows left: {:?}", w.windows.windows());
    ensure!(desktop.windows_changed(&w.windows) && !desktop.active(), "not reset after close");
    Ok(())
}

/// test_workspace_native: read-only snapshot, then activate and restore.
fn check_workspaces(w: &World) -> Result<(), String> {
    ensure!(wait_for(5.0, || w.workspaces.ready()), "workspaces not ready: {}", w.workspaces.error());
    ensure!(w.workspaces.supported() == Some(true), "unsupported");
    let values = w.workspaces.sorted_workspaces(None);
    let names: Vec<_> = values.iter().map(|w| w.name.as_str()).collect();
    ensure!(names == ["1", "2", "3", "10"], "names {names:?}");
    ensure!(values.iter().all(|w| w.can_activate), "not activatable");
    ensure!(values.iter().filter(|w| w.active).count() == 1, "active count");
    ensure!(values.iter().all(|w| w.outputs.len() == 2), "outputs {values:?}");
    ensure!(w.workspaces.groups().len() == 1, "groups {:?}", w.workspaces.groups());
    ensure!(w.workspaces.sorted_workspaces(Some("HEADLESS-2")).len() == 4, "per-output filter");
    let original = values.iter().find(|w| w.active).unwrap().clone();
    let target = values.iter().find(|w| !w.active).unwrap().clone();
    ensure!(w.workspaces.activate(&target.id), "activate refused: {}", w.workspaces.error());
    ensure!(wait_for(5.0, || w.workspaces.workspace(&target.id).is_some_and(|x| x.active)), "target not active");
    ensure!(w.workspaces.workspace(&original.id).is_some_and(|x| !x.active), "original still active");
    ensure!(w.workspaces.workspaces().values().filter(|w| w.active).count() == 1, "double active");
    ensure!(w.workspaces.activate(&original.id), "restore refused");
    ensure!(wait_for(5.0, || w.workspaces.workspace(&original.id).is_some_and(|x| x.active)), "original not restored");
    ensure!(!w.workspaces.activate("0:1"), "stale id accepted");
    w.errors.borrow_mut().retain(|e| e != "That workspace is no longer available.");
    Ok(())
}

fn native_fullscreen(w: &World) -> Result<(), String> {
    let (window, id) = w.gtk_window("Native fullscreen")?;
    let source = w.windows.clone();
    let guard = FullscreenGuard::with_parts(move || Some(source.snapshot()), Arc::new(FullscreenCache::new()),
                                            Arc::new(|| panic!("native windows never need X11")));
    w.windows.activate(&id);
    ensure!(wait_for(5.0, || w.windows.window(&id).is_some_and(|x| x.activated)), "not activated");
    window.fullscreen();
    ensure!(wait_for(5.0, || w.windows.window(&id).is_some_and(|x| x.fullscreen)), "not fullscreen");
    ensure!(guard.state() && guard.menu_hotkey_blocked(false) && !guard.menu_hotkey_blocked(true), "guard missed native fullscreen");
    window.unfullscreen();
    ensure!(wait_for(5.0, || w.windows.window(&id).is_some_and(|x| !x.fullscreen)), "still fullscreen");
    window.destroy();
    ensure!(wait_for(5.0, || w.windows.window(&id).is_none()), "not closed");
    Ok(())
}

struct Fixture(Child);
impl Fixture {
    fn send(&mut self, command: &str) {
        if let Some(stdin) = self.0.stdin.as_mut() {
            let _ = writeln!(stdin, "{command}");
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// FullscreenGuardNativeTests: a borderless X11 window filling an output.
fn xwayland(w: &World) -> Result<(), String> {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    if display.is_empty() || !fullscreen_guard::is_local_display(&display) {
        println!("note: no Xwayland DISPLAY in the private compositor; X11 probe not exercised");
        return Ok(());
    }
    let output = w.windows.outputs().get("HEADLESS-1").cloned().ok_or("no HEADLESS-1")?;
    let mut fixture = Fixture(Command::new(std::env::current_exe().unwrap()).arg("--x11-fixture")
        .arg(output.width.to_string()).arg(output.height.to_string())
        .env("GDK_BACKEND", "x11").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().map_err(|e| e.to_string())?);
    ensure!(wait_for(15.0, || w.id(X11_TITLE).is_some()), "X11 fixture never appeared");
    ensure!(fullscreen_guard::x11_snapshot().is_some(), "probe of the private Xwayland failed");
    let ident = w.id(X11_TITLE).unwrap();
    w.windows.activate(&ident);
    ensure!(wait_for(5.0, || w.windows.window(&ident).is_some_and(|x| x.activated)), "fixture not activated");
    let fresh = |cache: &FullscreenCache| fullscreen_guard::focused_fullscreen(&w.windows.snapshot(), cache);
    let cache = FullscreenCache::new();
    ensure!(wait_for(5.0, || fresh(&cache)), "borderless X11 window not detected: {:?} window {:?} outputs {:?}",
            fullscreen_guard::x11_snapshot(), w.windows.window(&ident), w.windows.outputs());
    ensure!(w.windows.window(&ident).is_some_and(|x| !x.fullscreen), "fixture must not request compositor fullscreen");
    fixture.send("maximize");
    ensure!(wait_for(5.0, || fullscreen_guard::x11_snapshot().is_some_and(|s| s["maximized"] == true)), "not maximized in X11");
    ensure!(!fresh(&cache), "ordinary maximization must not block the shortcut");
    fixture.send("unmaximize");
    ensure!(wait_for(5.0, || fullscreen_guard::x11_snapshot().is_some_and(|s| s["maximized"] == false)), "not unmaximized");
    ensure!(wait_for(5.0, || fresh(&cache)), "borderless again: {:?}", fullscreen_guard::x11_snapshot());
    let (native, native_id) = w.gtk_window("Fullscreen guard Wayland fixture")?;
    w.windows.activate(&native_id);
    ensure!(wait_for(5.0, || w.windows.window(&native_id).is_some_and(|x| x.activated)), "native not activated");
    ensure!(!fresh(&cache), "unfocused Xwayland game must not affect a native client");
    ensure!(fullscreen_guard::x11_snapshot().is_some_and(|s| s["focused"] == false), "X focus must reject the old X app");
    let cache = FullscreenCache::new();
    for _ in 0..2 {
        ensure!(!fresh(&cache), "native verdict");
    }
    ensure!(!cache.needs_probe(&w.windows.snapshot(), true), "a native client is classified from real X11 client lists");
    w.windows.activate(&ident);
    ensure!(wait_for(5.0, || w.windows.window(&ident).is_some_and(|x| x.activated)), "fixture not re-activated");
    ensure!(cache.needs_probe(&w.windows.snapshot(), false), "the Xwayland fixture stays a probe candidate");
    ensure!(wait_for(5.0, || fresh(&cache)), "fixture fullscreen again");
    ensure!(cache.last_state(Some(&w.windows.snapshot())), "cached verdict");
    native.destroy();
    drop(fixture);
    ensure!(wait_for(5.0, || w.windows.windows().is_empty()), "fixtures left");
    Ok(())
}

fn close(w: &World) -> Result<(), String> {
    let errors = w.errors.borrow().clone();
    w.windows.close();
    w.workspaces.close();
    ensure!(!w.windows.ready() && w.windows.windows().is_empty() && w.windows.pending_sources().is_empty(), "windows not closed");
    ensure!(!w.workspaces.ready() && w.workspaces.workspaces().is_empty(), "workspaces not closed");
    let window = gtk::Window::builder().title("After close").build();
    window.present();
    settle(0.5);
    ensure!(w.windows.windows().is_empty(), "closed backend still tracks windows");
    ensure!(*w.errors.borrow() == errors, "late errors {:?}", w.errors.borrow());
    window.destroy();
    Ok(())
}

/// `--x11-fixture W H`: an undecorated X11 window of the output's size;
/// stdin `maximize` / `unmaximize`.
fn x11_fixture(width: i32, height: i32) {
    gtk::init().expect("gtk (x11)");
    let window = gtk::Window::builder().title(X11_TITLE).decorated(false).default_width(width).default_height(height).build();
    window.set_child(Some(&gtk::Label::new(Some("Private borderless fixture"))));
    window.present();
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
        std::process::exit(0);
    });
    glib::timeout_add_local(Duration::from_millis(20), move || {
        while let Ok(command) = receiver.try_recv() {
            match command.as_str() {
                "maximize" => window.maximize(),
                "unmaximize" => window.unmaximize(),
                _ => {}
            }
        }
        glib::ControlFlow::Continue
    });
    glib::MainLoop::new(None, false).run();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if std::env::var("TESSEL_DOCK_WL_HEADLESS").as_deref() != Ok("1") || runtime.is_empty() || runtime == "/run/user/1000" {
        println!("wl_headless: skipped (run tests/run_wl_headless.sh)");
        return;
    }
    glib::set_prgname(Some(APP_ID));
    if args.get(1).map(String::as_str) == Some("--x11-fixture") {
        x11_fixture(args[2].parse().unwrap_or(1280), args[3].parse().unwrap_or(720));
        return;
    }
    gtk::init().expect("gtk");
    let _ = gdk::Display::default();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let changes = Rc::new(RefCell::new(0));
    let (e1, e2, c) = (errors.clone(), errors.clone(), changes.clone());
    let windows = WindowBackend::new(move || *c.borrow_mut() += 1, move |m| e1.borrow_mut().push(m.to_owned()));
    let workspaces = WorkspaceBackend::new(|| {}, move |m| e2.borrow_mut().push(m.to_owned()));
    let started = Instant::now();
    windows.start();
    workspaces.start();
    let world = World { windows, workspaces, errors, changes };
    if !wait_for(8.0, || world.windows.ready() && world.workspaces.ready()) {
        println!("FAIL setup: windows ready={} ({}) workspaces ready={} ({})", world.windows.ready(), world.windows.error(),
                 world.workspaces.ready(), world.workspaces.error());
        std::process::exit(1);
    }
    println!("time to ready: {:?}", started.elapsed());
    let checks: &[(&str, fn(&World) -> Result<(), String>)] = &[
        ("windows: observe / activate / minimize / maximize / close", check_windows),
        ("show desktop", show_desktop),
        ("workspaces: list / activate / restore", check_workspaces),
        ("fullscreen guard: native", native_fullscreen),
        ("fullscreen guard: Xwayland borderless", xwayland),
        ("close", close),
    ];
    let mut failed = 0;
    for (name, check) in checks {
        match check(&world) {
            Ok(()) => println!("ok   {name}"),
            Err(error) => {
                failed += 1;
                println!("FAIL {name}: {error}");
            }
        }
    }
    let errors = world.errors.borrow().clone();
    if !errors.is_empty() {
        failed += 1;
        println!("FAIL unexpected backend errors: {errors:?}");
    }
    println!("wl_headless: {} passed, {failed} failed", checks.len() + 1 - failed);
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
