//! `tessel-network-discovery [--toggle | --popup | --settings | --status]
//! [--popup-edge E] [--popup-offset N]` (discovery_manager.py).

use gio::prelude::*;
use tessel_network_discovery::app::{create_application, default_backend, default_browser, SPEC};
use tessel_network_discovery::compat::{prog, Outcome};

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let mut argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    // The Python shim execs `[python, discovery_manager.py, *args]`: drop the
    // interpreter so argv[0] is the script path (argparse's prog name).
    if argv.len() > 1 && argv[1].ends_with(".py") && std::path::Path::new(&argv[0]).file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("python")) {
        argv.remove(0);
    }
    let name = prog(argv.first().map(String::as_str).unwrap_or("tessel-network-discovery"));
    let args = match SPEC.parse(argv.get(1..).unwrap_or(&[]), true) {
        Outcome::Run(args) => args,
        Outcome::Help => {
            print!("{}", SPEC.help(&name));
            return glib::ExitCode::SUCCESS;
        }
        Outcome::Error(message) => {
            eprint!("{}", SPEC.error_text(&name, &message));
            return glib::ExitCode::from(2);
        }
    };
    if args.mode == Some("status") {
        println!("{}", tessel_network_discovery::read_status());
        return glib::ExitCode::SUCCESS;
    }
    let (app, _state) = create_application(default_backend(), default_browser());
    app.run_with_args(&argv)
}
