//! `configure_xdg_autostart.py`: preview or `--apply` labwc's XDG application autostart.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prog = tessel_session_startup::cli::prog("configure_xdg_autostart.py");
    let source = tessel_session_startup::session::Source::locate();
    let home = tessel_session_startup::fsops::home();
    match tessel_session_startup::xdg::main(&args, &prog, &home, &source.session) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
