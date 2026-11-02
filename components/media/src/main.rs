//! `media_manager.py` / `waymedia`: on-demand media controls using the lock
//! screen's MPRIS and icon backends.

use gio::prelude::*;
use tessel_media::app::{arguments, MediaApp, APPLICATION_ID};
use tessel_media::compat::argparse::prog_name;

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = prog_name(argv.first().map(String::as_str).unwrap_or(""), "tessel-media");
    if let Err((code, text, stdout)) = arguments(&prog, &argv[1..]) {
        if stdout { print!("{text}") } else { eprint!("{text}") }
        return glib::ExitCode::from(code as u8);
    }
    let app = MediaApp::new(APPLICATION_ID, &prog);
    app.app.run_with_args(&argv)
}
