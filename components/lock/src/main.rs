//! tessel-lock: `lock_screen.py` (see contracts/tessel-lock.md).
use gtk::prelude::*;
use tessel_lock::{app, auth, cli, session_lock};
use std::path::Path;
use tessel_gtk::gtk;

fn main() {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = argv.first().map(|a| Path::new(a).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        .unwrap_or_else(|| "tessel-lock".into());
    let args = match cli::parse(&argv[1.min(argv.len())..]) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{}\n{prog}: error: {message}", cli::usage(&prog));
            std::process::exit(2);
        }
    };
    if args.help {
        print!("{}", cli::help(&prog));
        return;
    }
    if !cli::protect_process() {
        eprintln!("Unable to protect the authentication process");
        std::process::exit(1);
    }
    if args.check {
        if gtk::init().is_err() {
            eprintln!("Gtk couldn't be initialized");
            std::process::exit(1);
        }
        let checks = (session_lock::is_supported(), cli::executable(&auth::helper_path()),
                      Path::new("/etc/pam.d/tessel-lock").is_file());
        println!("{}", cli::check_json(checks.0, checks.1, checks.2));
        std::process::exit(if checks.0 && checks.1 && checks.2 { 0 } else { 1 });
    }
    let app = app::App::new(app::Options { preview: args.preview, ..Default::default() });
    let application = app.application.clone();
    application.run_with_args(&argv[..1.min(argv.len())]);
    std::process::exit(app.result.get());
}
