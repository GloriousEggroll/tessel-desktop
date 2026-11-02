//! `weather_manager.py`: weather popup and settings for Tessel Dock; no
//! background service required. `--tray` runs a StatusNotifierItem instead.

use gio::prelude::*;
use tessel_weather::app::{arguments, WeatherApp, APPLICATION_ID};
use tessel_weather::compat::argparse::prog_name;
use tessel_weather::settings::Services;

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = prog_name(argv.first().map(String::as_str).unwrap_or(""), "tessel-weather");
    if let Err((code, text, stdout)) = arguments(&prog, &argv[1..]) {
        if stdout { print!("{text}") } else { eprint!("{text}") }
        return glib::ExitCode::from(code as u8);
    }
    let app = WeatherApp::new(APPLICATION_ID, None, None, Services::default(), &prog);
    app.app.run_with_args(&argv)
}
