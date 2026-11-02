//! `tessel-language [--status] [--quit]` (language_manager.py `main`).

use gio::prelude::*;
use tessel_language::app::{default_factory, LanguageApplication, APPLICATION_ID};
use tessel_language::args::{self, Outcome};

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let prog = args::prog(argv.first().map(String::as_str).unwrap_or("tessel-language"));
    match args::parse(argv.get(1..).unwrap_or(&[])) {
        Outcome::Run(_) => {}
        Outcome::Help => {
            print!("{}", args::help(&prog));
            return glib::ExitCode::SUCCESS;
        }
        Outcome::Error(message) => {
            eprint!("{}", args::error_text(&prog, &message));
            return glib::ExitCode::from(2);
        }
    }
    LanguageApplication::new(APPLICATION_ID, default_factory()).run_with_args(&argv)
}
