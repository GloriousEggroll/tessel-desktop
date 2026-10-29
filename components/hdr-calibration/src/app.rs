//! hdr_calibration.py `main()`: a NON_UNIQUE `org.desktop.HDRCalibration` app.

use crate::args::{self, Outcome};
use crate::calibration::RealHost;
use crate::ui::{LiveBackend, Window};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::sync::Arc;

pub const APP_ID: &str = "org.desktop.HDRCalibration";

pub fn run() -> glib::ExitCode {
    suite_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = args::prog(argv.first().map(String::as_str).unwrap_or("hdr-calibration"));
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
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::NON_UNIQUE);
    app.connect_activate(move |application| {
        let window = Window::new(application, parsed.output.clone(), Arc::new(LiveBackend(RealHost)));
        window.window.present();
        if parsed.sdr_editing {
            window.preset.set_selected(1);
            window.preset.grab_focus();
        }
        if parsed.smoke_test {
            let application = application.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                window.close();
                application.quit();
            });
        }
    });
    app.run_with_args(&argv[..1.min(argv.len())])
}
