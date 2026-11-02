//! At login: use the default icon theme (Papirus-Dark) when the configured one
//! is not installed. `--check` only reports.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prog = tessel_session_startup::cli::prog("tessel-icon-theme");
    let environment = tessel_session_startup::icon_theme::Environment::from_session();
    std::process::exit(tessel_session_startup::icon_theme::main(&args, &prog, &environment));
}
