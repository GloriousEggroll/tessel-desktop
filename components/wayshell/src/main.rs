//! `wayshell`: the desktop daemon and its CLI client (contract §2.2).
fn main() {
    let argv: Vec<String> = std::env::args().collect();
    // systemd-run launch helper (Python: `python3 app_catalog.py --launch-* <target>`).
    #[cfg(feature = "desktop")]
    if wayshell::desktop::launch::is_helper_invocation(&argv[1..]) {
        std::process::exit(wayshell::desktop::launch::helper_main(&argv[1..]));
    }
    suite_core::i18n::initialize();
    if let Some(outcome) = wayshell::cli::client_main(&argv[1..], &wayshell::cli::runtime_sock_path()) {
        std::process::exit(wayshell::cli::emit(&outcome));
    }
    std::process::exit(wayshell::app::run(argv));
}
