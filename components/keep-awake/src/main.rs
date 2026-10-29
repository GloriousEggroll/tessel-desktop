//! `keep-awake [--background | --popup | --status | --enable | --disable | --toggle]
//! [--popup-edge E] [--popup-offset N]` (keep_awake.py `main`).

use gio::prelude::*;
use keep_awake::app::{login_factory, KeepAwakeApplication, APPLICATION_ID};
use keep_awake::args::{self, Outcome};

fn main() -> glib::ExitCode {
    suite_core::i18n::initialize();
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let prog = args::prog(argv.first().map(String::as_str).unwrap_or("keep-awake"));
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
    // Reading status or clicking a stale icon in another desktop cannot create
    // a new inhibitor daemon. Only labwc's explicit --background starts it.
    let app = KeepAwakeApplication::new(APPLICATION_ID, login_factory(), !parsed.background(), None);
    app.run_with_args(&argv)
}
