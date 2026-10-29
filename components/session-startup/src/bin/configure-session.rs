//! `configure_session.py`: preview (`--check`, default) or `--apply` the generic labwc session.
//! Shipped files are read from `$DESKTOP_SUITE_SOURCE` (the suite source tree).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prog = session_startup::cli::prog("configure_session.py");
    let options = session_startup::session::Options::from_environment();
    match session_startup::session::main(&args, &prog, &options) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
