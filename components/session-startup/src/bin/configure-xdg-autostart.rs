//! `configure_xdg_autostart.py`: preview or `--apply` labwc's XDG application autostart.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prog = session_startup::cli::prog("configure_xdg_autostart.py");
    let source = session_startup::session::Source::locate();
    let home = session_startup::fsops::home();
    match session_startup::xdg::main(&args, &prog, &home, &source.session) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
