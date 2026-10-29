//! `session-manager [--popup] [--menu] [--popup-edge E] [--popup-offset N]`
//! (session_manager.py `__main__`). Every invocation is forwarded to the
//! primary `org.waydock.Session` instance; arguments are validated here first.

use gtk::prelude::*;
use session_manager::app::{session_backend_factory, SessionApplication, APPLICATION_ID};
use session_manager::args::{self, Outcome};

fn main() -> gtk::glib::ExitCode {
    suite_core::i18n::initialize();
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let prog = args::prog(argv.first().map(String::as_str).unwrap_or("session-manager"));
    match args::parse(argv.get(1..).unwrap_or(&[])) {
        Outcome::Run(_) => {}
        Outcome::Help => {
            print!("{}", args::help(&prog));
            return gtk::glib::ExitCode::SUCCESS;
        }
        Outcome::Error(message) => {
            eprint!("{}", args::error_text(&prog, &message));
            return gtk::glib::ExitCode::from(2);
        }
    }
    gtk::glib::set_prgname(Some(APPLICATION_ID));
    let app = SessionApplication::new(APPLICATION_ID, session_backend_factory());
    app.application().run_with_args(&argv)
}
