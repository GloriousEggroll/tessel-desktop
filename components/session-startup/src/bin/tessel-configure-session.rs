//! `tessel-configure-session`: preview (`--check`, default) or `--apply` the generic labwc session.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prog = tessel_session_startup::cli::prog("tessel-configure-session");
    let options = tessel_session_startup::session::Options::from_environment();
    match tessel_session_startup::session::main(&args, &prog, &options) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
