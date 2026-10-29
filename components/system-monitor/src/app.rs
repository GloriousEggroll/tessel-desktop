//! system_monitor.py: the `org.waydock.SystemMonitor` application.

use crate::args::{self, Outcome};
use crate::backend::{Options, Sampler};
use crate::ui::MonitorWindow;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub const APP_ID: &str = "org.waydock.SystemMonitor";

/// `--check`: a read-only backend sample as one JSON line.
pub fn check_text() -> String {
    let result = Sampler::new(Options::default()).sample();
    let summary = serde_json::json!({
        "ready": true, "processes": result["processes"].as_array().map(Vec::len).unwrap_or(0),
        "logical_cores": result["cpu"]["logical_cores"], "memory_total": result["memory"]["total"],
        "gpus": result["gpus"].as_array().map(Vec::len).unwrap_or(0),
    });
    suite_core::pyfmt::json_dumps(&summary, &suite_core::pyfmt::JsonStyle::DEFAULT)
}

/// `wlrctl toplevel focus app_id:org.waydock.SystemMonitor` (output silenced).
fn raise_window(window: &glib::WeakRef<MonitorWindowHandle>) {
    let Some(window) = window.upgrade() else { return };
    if !window.is_visible() || glib::find_program_in_path("wlrctl").is_none() {
        return;
    }
    let argv = ["wlrctl", "toplevel", "focus", &format!("app_id:{APP_ID}")].map(std::ffi::OsString::from);
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
    if let Ok(process) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        process.wait_async(None::<&gio::Cancellable>, |_| {});
    }
}

type MonitorWindowHandle = gtk::ApplicationWindow;

pub fn run() -> glib::ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let prog = args::prog(argv.first().map(String::as_str).unwrap_or("system-monitor"));
    let parsed = match args::parse(argv.get(1..).unwrap_or(&[])) {
        Outcome::Run(parsed) => parsed,
        Outcome::Help => {
            print!("{}", args::help(&prog));
            return glib::ExitCode::SUCCESS;
        }
        Outcome::Error(message) => {
            eprint!("{}", args::error_text(&prog, &message));
            return glib::ExitCode::from(2);
        }
    };
    if parsed.check {
        println!("{}", check_text());
        return glib::ExitCode::SUCCESS;
    }
    suite_core::i18n::initialize();
    glib::set_prgname(Some(APP_ID));
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
    suite_gtk::theme::attach_application(&app);
    let window: Rc<RefCell<Option<MonitorWindow>>> = Rc::new(RefCell::new(None));
    let show = {
        let window = window.clone();
        Rc::new(move |app: &gtk::Application, page: Option<String>| {
            let current = window.borrow().clone();
            let current = match current {
                Some(existing) if !existing.closed.get() => existing,
                _ => {
                    let created = MonitorWindow::new(app, Some(Sampler::new(Options::default())), true);
                    window.replace(Some(created.clone()));
                    created
                }
            };
            if let Some(page) = page {
                current.show_page(&page);
            }
            current.window.unminimize();
            current.window.present();
            let weak = current.window.downgrade();
            glib::idle_add_local_once(move || raise_window(&weak));
        })
    };
    let activate_show = show.clone();
    app.connect_activate(move |app| activate_show(app, None));
    app.connect_command_line(move |app, command_line| {
        let arguments: Vec<String> = command_line.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let options = match args::parse(arguments.get(1..).unwrap_or(&[])) {
            Outcome::Run(options) => options,
            Outcome::Help => {
                command_line.print_literal(&args::help(&prog));
                return glib::ExitCode::SUCCESS;
            }
            Outcome::Error(message) => {
                command_line.printerr_literal(&args::error_text(&prog, &message));
                return glib::ExitCode::from(2);
            }
        };
        if options.quit {
            if let Some(current) = window.borrow().clone() {
                current.close();
            }
            app.quit();
        } else {
            show(app, options.page);
        }
        glib::ExitCode::SUCCESS
    });
    app.run_with_args(&argv)
}
