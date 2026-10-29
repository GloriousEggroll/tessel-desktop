//! DesktopApp (wayshell:3766): the unique GApplication `com.nobaraDesktopView`
//! (owns that session-bus name), `do_command_line`, the socket server, the
//! switcher and the theme; the desktop layer lives in `crate::desktop`.

use crate::cli::{self, Command};
use crate::server::{self, Server, Shell};
use crate::switcher::WindowSwitcher;
use gio::prelude::*;
use gtk::gdk;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::rc::Rc;

pub const APP_ID: &str = "com.nobaraDesktopView";

#[derive(Default)]
pub struct DesktopApp {
    pub app: RefCell<Option<gtk::Application>>,
    #[cfg(feature = "desktop")]
    pub desktops: RefCell<Option<Rc<crate::desktop::Desktops>>>,
    pub switcher: RefCell<Option<Rc<WindowSwitcher>>>,
    pub server: RefCell<Option<Rc<Server>>>,
    #[cfg(feature = "theme")]
    pub theme: RefCell<Option<Rc<wayshell_theme::WayshellTheme>>>,
    activated: std::cell::Cell<bool>,
}

impl DesktopApp {
    fn app(&self) -> gtk::Application {
        self.app.borrow().clone().expect("application")
    }

    fn display(&self) -> Option<gdk::Display> {
        gdk::Display::default()
    }

    pub fn monitor_keys(&self) -> Vec<String> {
        self.display().map(|d| crate::monitors::monitors_by_key(&d).into_iter().map(|(k, _)| k).collect()).unwrap_or_default()
    }

    /// `do_activate` (idempotent: GApplication activation can repeat).
    pub fn activate(self: &Rc<Self>) {
        if self.activated.replace(true) {
            return;
        }
        // The daemon lives until SIGTERM/shutdown even with no desktop windows
        // (Python stays alive through its desktop layers).
        std::mem::forget(self.app().hold());
        #[cfg(feature = "theme")]
        if self.theme.borrow().is_none() {
            *self.theme.borrow_mut() = Some(wayshell_theme::WayshellTheme::new(None));
        }
        #[cfg(feature = "desktop")]
        if let Some(desktops) = self.desktops.borrow().clone() {
            desktops.start();
        }
        self.ensure_cli_socket();
        // Keep window identities and focus history warm before the first shortcut.
        let weak = Rc::downgrade(self);
        let wallpaper: crate::switcher::ui::WallpaperFn = Rc::new(move |connector: &str| {
            let _ = (&weak, connector);
            #[cfg(feature = "desktop")]
            if let Some(this) = weak.upgrade() {
                return this.desktops.borrow().as_ref().and_then(|d| d.wallpaper_paintable(connector));
            }
            None
        });
        let switcher = WindowSwitcher::new(&self.app(), None, None, WindowSwitcher::default_view_factory(wallpaper), None);
        #[cfg(feature = "theme")]
        {
            let weak = Rc::downgrade(self);
            *switcher.refresh_theme.borrow_mut() = Some(Rc::new(move || {
                if let Some(theme) = weak.upgrade().and_then(|t| t.theme.borrow().clone()) {
                    theme.refresh();
                }
            }));
        }
        *self.switcher.borrow_mut() = Some(switcher);
        // Return the startup transients (wallpaper decode, GPU pipeline
        // compilation, icon-theme scans) to the OS once the desktops are drawn.
        suite_gtk::utils::trim_heap_after(std::time::Duration::from_secs(4));
        glib::timeout_add_local_once(std::time::Duration::from_secs(30), || {
            suite_gtk::utils::trim_heap_after(std::time::Duration::ZERO);
        });
    }

    fn ensure_cli_socket(self: &Rc<Self>) {
        if self.server.borrow().is_some() {
            return;
        }
        let shell: Rc<dyn Shell> = Rc::new(AppShell(Rc::downgrade(self)));
        match Server::start(server::socket_path(), shell) {
            Ok(s) => *self.server.borrow_mut() = Some(s),
            Err(e) => eprintln!("CLI socket error: {e}"),
        }
    }

    pub fn apply_wallpaper(&self, keys: &[String], file_path: Option<&str>, mode: &str, color: &gdk::RGBA) {
        #[cfg(feature = "desktop")]
        if let Some(d) = self.desktops.borrow().clone() {
            d.apply_wallpaper_to_keys(keys, file_path, mode, color);
            return;
        }
        let _ = (keys, file_path, mode, color);
    }

    /// `do_command_line`: messages go to the daemon's own stdout like Python's print().
    pub fn command_line(self: &Rc<Self>, argv: &[String]) -> i32 {
        if argv.iter().any(|a| a == "-h" || a == "--help") {
            print!("{}", cli::HELP);
            return 0;
        }
        let command = match cli::parse_wayshell(argv, None) {
            Ok(c) => c,
            Err(e) => {
                eprint!("{}", e.text);
                print!("Invalid arguments.\n\nRun with --help for usage.\n\n");
                return 2;
            }
        };
        let keys = self.monitor_keys();
        let resolve = |monitor: &str| -> Result<Vec<String>, String> {
            let wanted = if monitor == "all" { keys.clone() } else { vec![monitor.to_string()] };
            let missing: Vec<String> = wanted.iter().filter(|k| !keys.contains(k)).cloned().collect();
            if missing.is_empty() { Ok(wanted) } else { Err(missing.join(", ")) }
        };
        match command {
            None => {
                self.app().activate();
                0
            }
            Some(Command::SwitchWindow { reverse }) => {
                self.ensure_switcher();
                self.with_switcher(|s| s.step(reverse)) as i32 ^ 1
            }
            Some(Command::SwitcherStatus) => {
                self.ensure_switcher();
                let mut out = Map::new();
                out.insert("ok".into(), Value::Bool(true));
                out.extend(self.switcher.borrow().as_ref().map(|s| s.status()).unwrap_or_default());
                println!("{}", cli::dumps(&Value::Object(out)));
                0
            }
            Some(Command::ShowActivities { monitor }) => {
                self.ensure_switcher();
                let value = monitor.map(Value::String);
                self.with_switcher(|s| s.show_activities(value.as_ref())) as i32 ^ 1
            }
            Some(Command::ListMonitors) => {
                for k in &keys {
                    print!("{k}\n\n");
                }
                0
            }
            Some(Command::Set { monitor, file, mode, color }) => {
                let keys = match resolve(&monitor) {
                    Ok(k) => k,
                    Err(m) => {
                        print!("Unknown monitor(s): {m}\n\n");
                        return 1;
                    }
                };
                let Ok(rgba) = gdk::RGBA::parse(color.as_str()) else {
                    print!("Invalid color '{color}'\n\n");
                    return 1;
                };
                let file = if mode == "Color Only" { None } else { file.filter(|f| !f.is_empty()) };
                if let Some(f) = &file {
                    if !std::path::Path::new(f).is_file() {
                        print!("File not found: {f}\n\n");
                        return 1;
                    }
                }
                self.apply_wallpaper(&keys, file.as_deref(), &mode, &rgba);
                print!("OK\n\n");
                0
            }
            Some(Command::SetColor { monitor, color, mode }) => {
                let keys = match resolve(&monitor) {
                    Ok(k) => k,
                    Err(m) => {
                        print!("Unknown monitor(s): {m}\n\n");
                        return 1;
                    }
                };
                let Ok(rgba) = gdk::RGBA::parse(color.as_str()) else {
                    print!("Invalid color '{color}'\n\n");
                    return 1;
                };
                self.apply_wallpaper(&keys, None, &mode, &rgba);
                print!("OK\n\n");
                0
            }
            Some(Command::ApplyJson { json_path }) => {
                let cfg: Value = match std::fs::read_to_string(&json_path).map_err(|e| suite_core::pyerr::os_error_path(&e, std::path::Path::new(&json_path)))
                    .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
                    Ok(v) => v,
                    Err(e) => {
                        print!("Failed to read JSON: {e}\n\n");
                        return 1;
                    }
                };
                if let Value::Object(cfg) = cfg {
                    for (key, entry) in cfg {
                        if !keys.contains(&key) {
                            print!("Skipping unknown monitor '{key}'\n\n");
                            continue;
                        }
                        let mode = entry.get("mode").map(suite_core::pyjson::py_str).unwrap_or_else(|| "Fill".into());
                        let color = entry.get("color").map(suite_core::pyjson::py_str).unwrap_or_else(|| "black".into());
                        let path = entry.get("file_path").and_then(Value::as_str).map(str::to_string);
                        let Ok(rgba) = gdk::RGBA::parse(color.as_str()) else {
                            print!("Skipping '{key}': bad color '{color}'\n\n");
                            continue;
                        };
                        if mode != "Color Only" {
                            if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
                                if !std::path::Path::new(p).is_file() {
                                    print!("Skipping '{key}': missing file '{p}'\n\n");
                                    continue;
                                }
                            }
                        }
                        let file = if mode == "Color Only" { None } else { path.as_deref() };
                        self.apply_wallpaper(&[key.clone()], file, &mode, &rgba);
                    }
                }
                print!("OK\n\n");
                0
            }
        }
    }

    fn ensure_switcher(self: &Rc<Self>) {
        if self.switcher.borrow().is_none() {
            self.app().activate();
        }
    }

    fn with_switcher(&self, f: impl FnOnce(&WindowSwitcher) -> bool) -> bool {
        let s = self.switcher.borrow().clone();
        s.map(|s| f(&s)).unwrap_or(false)
    }

    /// `do_shutdown`. The socket file is deliberately left behind (Python parity).
    pub fn shutdown(&self) {
        #[cfg(feature = "desktop")]
        if let Some(d) = self.desktops.borrow_mut().take() {
            d.shutdown();
        }
        if let Some(s) = self.switcher.borrow_mut().take() {
            s.close();
        }
        #[cfg(feature = "theme")]
        if let Some(t) = self.theme.borrow_mut().take() {
            t.close();
        }
        if let Some(server) = self.server.borrow_mut().take() {
            server.close();
        }
    }
}

struct AppShell(std::rc::Weak<DesktopApp>);

impl Shell for AppShell {
    fn switch_window(&self, reverse: bool) -> bool {
        self.0.upgrade().is_some_and(|a| a.with_switcher(|s| s.step(reverse)))
    }
    fn switcher_status(&self) -> Option<Map<String, Value>> {
        self.0.upgrade().and_then(|a| a.switcher.borrow().as_ref().map(|s| s.status()))
    }
    fn show_activities(&self, monitor: Option<&Value>) -> bool {
        self.0.upgrade().is_some_and(|a| a.with_switcher(|s| s.show_activities(monitor)))
    }
    fn monitor_keys(&self) -> Vec<String> {
        self.0.upgrade().map(|a| a.monitor_keys()).unwrap_or_default()
    }
    fn apply_wallpaper(&self, keys: &[String], file_path: Option<&str>, mode: &str, color: &gdk::RGBA) {
        if let Some(a) = self.0.upgrade() {
            a.apply_wallpaper(keys, file_path, mode, color);
        }
    }
}

/// Swallow "GtkGizmo" warnings like the Python log filter.
fn install_log_filter() {
    glib::log_set_handler(Some("Gtk"), glib::LogLevels::LEVEL_WARNING, false, false, |domain, level, message| {
        if !message.contains("GtkGizmo") {
            glib::log_default_handler(domain, level, Some(message));
        }
    });
}

/// Run the daemon GApplication with `argv` (program name first).
pub fn run(argv: Vec<String>) -> i32 {
    install_log_filter();
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
    let state = Rc::new(DesktopApp::default());
    *state.app.borrow_mut() = Some(app.clone());
    let s = state.clone();
    app.connect_startup(move |app| {
        let _ = (&s, app);
        #[cfg(feature = "desktop")]
        {
            *s.desktops.borrow_mut() = Some(crate::desktop::Desktops::new(app));
        }
    });
    let s = state.clone();
    app.connect_activate(move |_| s.activate());
    let s = state.clone();
    app.connect_command_line(move |_, command_line| {
        let args: Vec<String> = command_line.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        glib::ExitCode::from(s.command_line(&args) as u8)
    });
    let s = state.clone();
    app.connect_shutdown(move |_| s.shutdown());
    let code = app.run_with_args(&argv);
    i32::from(code)
}
