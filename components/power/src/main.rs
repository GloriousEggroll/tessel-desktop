//! `power_manager.py`: Waybar power profiles and persistent session idle
//! management. A unique GApplication (`org.tessel.PowerManager`); launchers
//! (`--check`, `--idle-action`, `--idle-resume`, `--power-button`) forward to
//! the primary and never become one.

use gio::prelude::*;
use tessel_power::alerts::BatteryAlerts;
use tessel_power::app::{App, Services, APPLICATION_ID, INTERFACE};
use tessel_power::args::{self, Args};
use tessel_power_core::backend::{Options, PowerBackend, PowerSystem};
use tessel_power::gui::{GtkDimmer, GtkSurfaces};
use tessel_power::idle::{self_command, IdleController, RealProcesses};
use tessel_power::logind::SessionSwitches;
use tessel_core::scheduler::{GlibScheduler, Scheduler};
use std::cell::RefCell;
use std::rc::Rc;
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::service_settings::SettingsService;

fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "tessel-power".into())
}

fn startup(gapp: &gio::Application, app: &App, service: &Rc<RefCell<Option<SettingsService>>>) {
    app.load_settings(None);
    let scheduler: Rc<dyn Scheduler> = Rc::new(GlibScheduler::default());
    let (w1, w2) = (app.downgrade(), app.downgrade());
    let backend = PowerBackend::new(move || if let Some(a) = w1.upgrade() { App(a).changed() },
                                    move |_| if let Some(a) = w2.upgrade() { App(a).changed() }, Options::default());
    let system: Rc<dyn PowerSystem> = Rc::new(backend);
    let w = app.downgrade();
    let idle = IdleController::new(system.clone(), move |m| if let Some(a) = w.upgrade() { App(a).idle_error(m) },
                                   Rc::new(RealProcesses), scheduler.clone(), self_command());
    app.0.backend.replace(Some(system.clone()));
    app.0.idle.replace(Some(Rc::new(idle)));
    let w = app.downgrade();
    let alerts = BatteryAlerts::new(system.clone(), move |action| if let Some(a) = w.upgrade() { App(a).critical_action(action) },
                                    None, scheduler);
    alerts.update(&app.settings());
    app.0.alerts.replace(Some(alerts));
    let (w1, w2) = (app.downgrade(), app.downgrade());
    let switches = SessionSwitches::new(move |docked| if let Some(a) = w1.upgrade() { App(a).lid_closed(docked) },
                                        move |m| if let Some(a) = w2.upgrade() { App(a).idle_error(m) }, None, None);
    app.0.switches.replace(Some(switches.clone()));
    app.0.dimmer.replace(Some(Rc::new(GtkDimmer::new())));
    app.0.surfaces.replace(Some(GtkSurfaces::new(app)));
    system.start();
    switches.start();
    if app.0.config_valid.get() {
        if let Some(idle) = app.idle() {
            idle.start(&app.settings());
        }
    }
    let (w1, w2) = (app.downgrade(), app.downgrade());
    match SettingsService::new(gapp, INTERFACE, "SetSettings", "s",
        move || w1.upgrade().map(|a| App(a).settings_state()).unwrap_or(serde_json::Value::Null),
        move |value: &glib::Variant| {
            let app = w2.upgrade().map(App).ok_or_else(|| "Power management is stopping.".to_owned())?;
            let encoded = value.get::<String>().ok_or_else(|| "Invalid power settings.".to_owned())?;
            app.remote_save(&encoded)
        }) {
        Ok(created) => {
            let notify = created.clone();
            app.0.notify_service.replace(Some(Box::new(move || notify.changed())));
            service.replace(Some(created));
        }
        Err(error) => glib::g_warning!("tessel-power", "Could not export the settings service: {}", error.message()),
    }
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
    let surfaces = app.0.surfaces.borrow().clone();
    if let Some(action) = &args.idle_action {
        app.idle_action(action, &args.policy_token);
    } else if let Some(action) = &args.idle_resume {
        app.idle_resume(action, &args.policy_token);
    } else if args.power_button {
        app.power_button();
    } else if args.settings {
        if let Some(s) = surfaces {
            s.show_settings();
        }
    } else if !args.background {
        if let Some(s) = surfaces {
            s.show_popup(&args.popup_edge, args.popup_offset);
        }
    }
    0
}

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = prog(argv.first().map(String::as_str).unwrap_or("tessel-power"));
    let parsed = match args::parse(&prog, &argv[1..]) {
        Ok(parsed) => parsed,
        Err((code, text, stdout)) => {
            if stdout { print!("{text}") } else { eprint!("{text}") }
            return glib::ExitCode::from(code as u8);
        }
    };
    glib::set_prgname(Some(APPLICATION_ID));
    // A delayed timer or health probe must never launch a new idle manager.
    let gapp = gio::Application::new(Some(APPLICATION_ID), tessel_core::single_instance::application_flags(parsed.launcher()));
    let app = App::new(Services::default());
    let service: Rc<RefCell<Option<SettingsService>>> = Rc::new(RefCell::new(None));
    let signals: Rc<RefCell<Vec<glib::SourceId>>> = Rc::new(RefCell::new(Vec::new()));
    let held: Rc<RefCell<Option<gio::ApplicationHoldGuard>>> = Rc::new(RefCell::new(None));
    {
        let (app, service, signals, held) = (app.clone(), service.clone(), signals.clone(), held.clone());
        gapp.connect_startup(move |gapp| {
            startup(gapp, &app, &service);
            held.replace(Some(gapp.hold()));
            for signal in [libc::SIGTERM, libc::SIGINT] {
                let weak = gapp.downgrade();
                signals.borrow_mut().push(tessel_core::glib_compat::unix_signal_add_local(signal, move || {
                    if let Some(gapp) = weak.upgrade() {
                        gapp.quit();
                    }
                    glib::ControlFlow::Break
                }));
            }
        });
    }
    {
        let app = app.clone();
        gapp.connect_activate(move |_| {
            if let Some(s) = app.0.surfaces.borrow().clone() {
                s.show_popup("bottom", 76);
            }
        });
    }
    {
        let (app, prog) = (app.clone(), prog.clone());
        gapp.connect_command_line(move |_, line| glib::ExitCode::from(command_line(&app, &prog, line) as u8));
    }
    {
        let (app, service) = (app.clone(), service.clone());
        gapp.connect_shutdown(move |_| {
            app.0.stopping.set(true);
            if let Some(service) = service.take() {
                service.close();
            }
            for source in signals.borrow_mut().drain(..) {
                if glib::MainContext::default().find_source_by_id(&source).is_some() {
                    source.remove();
                }
            }
            app.shutdown();
            held.take();
        });
    }
    gapp.run_with_args(&argv)
}
