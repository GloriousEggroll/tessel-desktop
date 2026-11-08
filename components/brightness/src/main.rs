//! `brightness_manager.py`: shared monitor controls for Waybar and
//! tessel-displays. A unique GApplication (`org.tessel.BrightnessManager`);
//! `--check`, `--idle-dim`, `--idle-restore`, `--brightness-up/down` forward
//! to the primary and never become one.

use tessel_brightness::app::{App, Services, Surfaces};
use tessel_brightness::args::{self, Args};
use tessel_brightness::backend::{DisplayBackend, Hardware, MonitorWatch, Options, RealSystem, System};
use tessel_brightness::client::{LocalClient, Request, INTERFACE, INTERFACE_XML, OBJECT_PATH, BUS_NAME};
use tessel_core::scheduler::{GlibScheduler, Scheduler};
use tessel_brightness::guard::{IdleGuard, LogindGuard};
use tessel_brightness::gui::{GdkMonitors, GtkSurfaces, Watch};
use tessel_brightness::idle::{self_command, IdleController, IdleListener, RealSpawner};
use tessel_brightness::scaling::{ipc_request, ScaleBackend, ScaleOptions};
use gio::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::tr;

fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "tessel-brightness".into())
}

/// The machine, optionally with a test sysfs (`$BRIGHTNESS_MANAGER_SYSFS/{drm,backlight}`).
struct Machine(Option<PathBuf>);

impl System for Machine {
    fn which(&self, name: &str) -> Option<String> { RealSystem.which(name) }
    fn run(&self, argv: &[String], timeout: std::time::Duration) -> Result<tessel_brightness::backend::CommandOutput, String> {
        RealSystem.run(argv, timeout)
    }
    fn drm_root(&self) -> PathBuf { self.0.as_ref().map(|r| r.join("drm")).unwrap_or_else(|| RealSystem.drm_root()) }
    fn backlight_root(&self) -> PathBuf { self.0.as_ref().map(|r| r.join("backlight")).unwrap_or_else(|| RealSystem.backlight_root()) }
    fn set_backlight(&self, name: &str, raw: u32, guard: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        RealSystem.set_backlight(name, raw, guard)
    }
    // A test sysfs never reads the session's labwc config or writes remembered locks.
    fn hdr_enabled(&self, connector: &str) -> bool { self.0.is_none() && RealSystem.hdr_enabled(connector) }
    fn load_hdr_locks(&self) -> Vec<String> { if self.0.is_none() { RealSystem.load_hdr_locks() } else { Vec::new() } }
    fn save_hdr_locks(&self, locks: &[String]) {
        if self.0.is_none() {
            RealSystem.save_hdr_locks(locks)
        }
    }
}

fn services(gtk_ready: bool) -> Services {
    let scheduler: Rc<dyn Scheduler> = Rc::new(GlibScheduler::default());
    let sysfs = std::env::var_os("BRIGHTNESS_MANAGER_SYSFS").filter(|v| !v.is_empty()).map(PathBuf::from);
    let (s1, s2, s3) = (scheduler.clone(), scheduler.clone(), scheduler.clone());
    Services {
        hardware: Box::new(move |changed, failed| {
            let monitors: Option<Box<dyn MonitorWatch>> = gtk_ready.then(|| Box::new(Watch(Rc::new(GdkMonitors::default()))) as _);
            let system: Arc<dyn System> = Arc::new(Machine(sysfs));
            Rc::new(DisplayBackend::new(changed, failed, Options { system, scheduler: s1, monitors, spawn_events: true })) as Rc<dyn Hardware>
        }),
        scaling: Box::new(move |changed| {
            ScaleBackend::new(changed, ScaleOptions { request: Arc::new(ipc_request), scheduler: s2, spawn_events: true })
        }),
        idle: Box::new(move |failed| {
            Rc::new(IdleController::new(failed, Rc::new(RealSpawner), s3, self_command())) as Rc<dyn IdleListener>
        }),
        guard: Box::new(|| Rc::new(LogindGuard::system()) as Rc<dyn IdleGuard>),
        scheduler,
        config_dir: None,
        state_dir: None,
    }
}

fn register(connection: &gio::DBusConnection, app: &App) -> Option<gio::RegistrationId> {
    let info = gio::DBusNodeInfo::for_xml(INTERFACE_XML).ok()?;
    let interface = info.lookup_interface(INTERFACE)?;
    let weak = app.downgrade();
    connection.register_object(OBJECT_PATH, &interface)
        .method_call(move |_, _, _, _, method, params, invocation| {
            let Some(inner) = weak.upgrade() else {
                invocation.return_dbus_error(&format!("{INTERFACE}.Error"), &tr!("Brightness controls are restarting. Try again shortly."));
                return;
            };
            let app = App(inner);
            if method == "GetState" {
                invocation.return_value(Some(&(app.encoded(),).into()));
                return;
            }
            let reply = match Request::from_call(method, &params) {
                Some(request) if !app.0.closing.get() => app.dispatch(request),
                Some(_) => Err(tr!("Brightness controls are restarting. Try again shortly.")),
                None => Err(tr!("Unknown brightness command.")),
            };
            match reply {
                Ok(accepted) => invocation.return_value(Some(&(accepted,).into())),
                Err(error) => invocation.return_dbus_error(&format!("{INTERFACE}.Error"), &error),
            }
        })
        .build()
        .map_err(|e| glib::g_warning!("tessel-brightness", "Could not export the controls: {}", e.message()))
        .ok()
}

fn command_line(app: &App, prog: &str, line: &gio::ApplicationCommandLine) -> i32 {
    let words: Vec<String> = line.arguments().iter().skip(1).map(|w| w.to_string_lossy().into_owned()).collect();
    let args: Args = match args::parse(prog, &words) {
        Ok(args) => args,
        Err((code, text, stdout)) => {
            if stdout { line.print_literal(&text) } else { line.printerr_literal(&text) }
            return code;
        }
    };
    if args.check {
        let report = app.health();
        line.print_literal(&(json_dumps(&report, &JsonStyle::DEFAULT) + "\n"));
        return if report["ready"] == true { 0 } else { 1 };
    }
    if args.brightness_up || args.brightness_down {
        return if app.adjust_brightness(if args.brightness_up { 10.0 } else { -10.0 }) { 0 } else { 1 };
    }
    if let Some(token) = args.idle_dim_token() {
        app.dim(token);
    } else if let Some(token) = args.idle_restore_token() {
        app.idle_restore(token);
    } else if args.settings {
        app.show_settings();
    } else if !args.background {
        app.show_popup(&args.popup_edge, args.popup_offset as i32);
    }
    0
}

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = prog(argv.first().map(String::as_str).unwrap_or("tessel-brightness"));
    let parsed = match args::parse(&prog, &argv[1..]) {
        Ok(parsed) => parsed,
        Err((code, text, stdout)) => {
            if stdout { print!("{text}") } else { eprint!("{text}") }
            return glib::ExitCode::from(code as u8);
        }
    };
    glib::set_prgname(Some(BUS_NAME));
    let gapp = gio::Application::new(Some(BUS_NAME), tessel_core::single_instance::application_flags(parsed.launcher()));
    let surfaces_slot: Rc<RefCell<Option<GtkSurfaces>>> = Rc::default();
    let client = {
        let slot = surfaces_slot.clone();
        LocalClient::new(move || {
            let surfaces = slot.borrow().clone();
            if let Some(surfaces) = surfaces {
                surfaces.refresh();
            }
        })
    };
    let app = App::with_client(client.clone());
    let registration: Rc<RefCell<Option<gio::RegistrationId>>> = Rc::default();
    let signals: Rc<RefCell<Vec<glib::SourceId>>> = Rc::default();
    let held: Rc<RefCell<Option<gio::ApplicationHoldGuard>>> = Rc::default();
    {
        let (app, registration, signals, held, slot) = (app.clone(), registration.clone(), signals.clone(), held.clone(), surfaces_slot.clone());
        gapp.connect_startup(move |gapp| {
            held.replace(Some(gapp.hold()));
            if let Some(connection) = gapp.dbus_connection() {
                registration.replace(register(&connection, &app));
            }
            // GDK supplies monitor hotplug signals and the popup/OSD surfaces;
            // no widget or theme is created until one is needed.
            let gtk_ready = gtk::init().is_ok();
            if gtk_ready {
                let (w1, w2) = (app.downgrade(), app.downgrade());
                let surfaces = GtkSurfaces::new(client.clone(),
                    Rc::new(move || w1.upgrade().map(|a| a.error.borrow().clone()).unwrap_or_default()),
                    Rc::new(move || if let Some(a) = w2.upgrade() { App(a).show_settings() }));
                slot.replace(Some(surfaces.clone()));
                app.set_surfaces(Rc::new(surfaces) as Rc<dyn Surfaces>);
            }
            let connection = gapp.dbus_connection();
            let emit = Box::new(move |encoded: &str| {
                if let Some(connection) = &connection {
                    let _ = connection.emit_signal(None, OBJECT_PATH, INTERFACE, "Changed", Some(&(encoded,).into()));
                }
            });
            let weak = gapp.downgrade();
            let quit: Rc<dyn Fn()> = Rc::new(move || if let Some(gapp) = weak.upgrade() { gapp.quit() });
            app.startup(services(gtk_ready), emit, quit);
            for signal in [libc::SIGTERM, libc::SIGINT] {
                let weak = app.downgrade();
                signals.borrow_mut().push(glib_unix::unix_signal_add_local(signal, move || {
                    if let Some(inner) = weak.upgrade() {
                        App(inner).stop();
                    }
                    glib::ControlFlow::Continue
                }));
            }
        });
    }
    {
        let app = app.clone();
        gapp.connect_activate(move |_| app.show_popup("bottom", 76));
    }
    {
        let (app, prog) = (app.clone(), prog.clone());
        gapp.connect_command_line(move |_, line| glib::ExitCode::from(command_line(&app, &prog, line) as u8));
    }
    {
        let app = app.clone();
        gapp.connect_shutdown(move |gapp| {
            for source in signals.borrow_mut().drain(..) {
                source.remove();
            }
            app.shutdown();
            if let (Some(connection), Some(id)) = (gapp.dbus_connection(), registration.take()) {
                let _ = connection.unregister_object(id);
            }
            surfaces_slot.replace(None);
            held.take();
        });
    }
    gapp.run_with_args(&argv)
}
