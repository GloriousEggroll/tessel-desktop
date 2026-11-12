//! At login: replace an icon theme that is not installed, or that is KDE's
//! default and was not chosen in Global Settings, with the chosen theme or
//! the default (Papirus-Dark). `--check` only reports.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prog = tessel_session_startup::cli::prog("tessel-icon-theme");
    let environment = tessel_session_startup::icon_theme::Environment::from_session();
    std::process::exit(tessel_session_startup::icon_theme::main(&args, &prog, &environment));
}
