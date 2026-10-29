//! `media_manager.py` / `waymedia`: on-demand media controls using the lock
//! screen's MPRIS and icon backends.

use gio::prelude::*;
use media_manager::app::{arguments, MediaApp, APPLICATION_ID};
use media_manager::compat::argparse::prog_name;

fn main() -> glib::ExitCode {
    suite_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = prog_name(argv.first().map(String::as_str).unwrap_or(""), "media-manager");
    if let Err((code, text, stdout)) = arguments(&prog, &argv[1..]) {
        if stdout { print!("{text}") } else { eprint!("{text}") }
        return glib::ExitCode::from(code as u8);
    }
    let app = MediaApp::new(APPLICATION_ID, &prog);
    app.app.run_with_args(&argv)
}
