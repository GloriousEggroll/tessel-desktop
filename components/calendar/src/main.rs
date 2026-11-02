//! `calendar_manager.py`: a Waybar/Tessel Dock calendar popup and system date
//! settings. Argument errors are reported locally (exit 2) before the unique
//! GApplication forwards the command line to the primary instance.

use tessel_calendar::app::{real_backend_factory, real_store_factory, CalendarApp, APPLICATION_ID};
use tessel_calendar::args;
use tessel_calendar::preferences::CalendarPreferencesStore;

fn main() -> gtk::glib::ExitCode {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = argv.first().and_then(|a| std::path::Path::new(a).file_name()).map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tessel-calendar".into());
    if let Err((code, text, stdout)) = args::parse(&prog, &argv[1..]) {
        if stdout { print!("{text}") } else { eprint!("{text}") }
        return gtk::glib::ExitCode::from(code as u8);
    }
    let app = CalendarApp::new(APPLICATION_ID, &prog, real_backend_factory(), real_store_factory(), CalendarPreferencesStore::default());
    app.run(&argv)
}
