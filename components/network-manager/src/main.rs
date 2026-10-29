//! `network_manager.py`: NetworkManager controls and authentication for the
//! desktop. A unique GApplication (`org.waybar.NetworkManager`) owning the
//! backend and the secret agent; `--check` probes never become the primary.

use gio::prelude::*;
use network_manager::agent::{AgentCore, AgentService};
use network_manager::app::{App, Surfaces, APPLICATION_ID};
use network_manager::args::{self, Args};
use network_manager::backend::{NetworkBackend, Options};
use network_manager::gui::Gui;
use network_manager::keyring::SecretService;
use network_manager::preferences::NotificationPreferences;
use network_manager::ui::prompt::GtkPrompter;
use std::cell::RefCell;
use std::rc::Rc;
use suite_core::pyfmt::{json_dumps, JsonStyle};

fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "network-manager".into())
}

fn startup(gapp: &gio::Application, app: &App) {
    app.0.preferences.replace(Some(NotificationPreferences::new()));
    let (w1, w2) = (app.downgrade(), app.downgrade());
    let backend = NetworkBackend::new(move || if let Some(a) = w1.upgrade() { App(a).changed() },
                                      move |m| if let Some(a) = w2.upgrade() { App(a).report_error(m) }, Options::default());
    app.0.backend.replace(Some(backend.clone()));
    let gui = Gui::new(app);
    app.0.surfaces.replace(Some(gui.clone() as Rc<dyn Surfaces>));
    backend.start();
    let w = app.downgrade();
    let core = AgentCore::new(Rc::new(SecretService::new()), Rc::new(GtkPrompter { gui: Rc::downgrade(&gui) }),
                              move |m| if let Some(a) = w.upgrade() { App(a).report_error(m) });
    let agent = AgentService::new(core);
    app.0.agent.replace(Some(agent.clone()));
    agent.start();
    let action = gio::SimpleAction::new("captive-portal", None);
    let w = app.downgrade();
    action.connect_activate(move |_, _| if let Some(a) = w.upgrade() { App(a).open_captive_portal() });
    gapp.add_action(&action);
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
        return if report["ready"] == true { 0 } else { 2 };
    }
    let Some(surfaces) = app.surfaces() else { return 0 };
    if args.settings {
        surfaces.show_settings();
    } else if !args.background {
        surfaces.show_popup(&args.popup_edge, args.popup_offset);
    }
    0
}

fn main() -> glib::ExitCode {
    suite_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = prog(argv.first().map(String::as_str).unwrap_or("network-manager"));
    // Bad arguments exit 2 before any D-Bus traffic.
    let parsed = match args::parse(&prog, &argv[1..]) {
        Ok(parsed) => parsed,
        Err((code, text, stdout)) => {
            if stdout { print!("{text}") } else { eprint!("{text}") }
            return glib::ExitCode::from(code as u8);
        }
    };
    glib::set_prgname(Some(APPLICATION_ID));
    // Health probes must never win the startup race, become the persistent
    // primary or register another secret agent.
    let gapp = gio::Application::new(Some(APPLICATION_ID), suite_core::single_instance::application_flags(parsed.check));
    let app = App::new(&gapp);
    let signals: Rc<RefCell<Vec<glib::SourceId>>> = Rc::default();
    let held: Rc<RefCell<Option<gio::ApplicationHoldGuard>>> = Rc::default();
    {
        let (app, signals, held) = (app.clone(), signals.clone(), held.clone());
        gapp.connect_startup(move |gapp| {
            startup(gapp, &app);
            held.replace(Some(gapp.hold()));
            for signal in [libc::SIGTERM, libc::SIGINT] {
                let weak = gapp.downgrade();
                signals.borrow_mut().push(suite_core::glib_compat::unix_signal_add_local(signal, move || {
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
        gapp.connect_activate(move |_| if let Some(s) = app.surfaces() { s.show_popup("bottom", 76) });
    }
    {
        let (app, prog) = (app.clone(), prog.clone());
        gapp.connect_command_line(move |_, line| glib::ExitCode::from(command_line(&app, &prog, line) as u8));
    }
    {
        let app = app.clone();
        gapp.connect_shutdown(move |_| {
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
