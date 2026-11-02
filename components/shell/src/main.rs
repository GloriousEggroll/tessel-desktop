//! `tessel-shell`: the desktop daemon and its CLI client (contract §2.2).
fn main() {
    let argv: Vec<String> = std::env::args().collect();
    // systemd-run launch helper (Python: `python3 app_catalog.py --launch-* <target>`).
    #[cfg(feature = "desktop")]
    if tessel_shell::desktop::launch::is_helper_invocation(&argv[1..]) {
        std::process::exit(tessel_shell::desktop::launch::helper_main(&argv[1..]));
    }
    tessel_core::i18n::initialize();
    if let Some(outcome) = tessel_shell::cli::client_main(&argv[1..], &tessel_shell::cli::runtime_sock_path()) {
        std::process::exit(tessel_shell::cli::emit(&outcome));
    }
    std::process::exit(tessel_shell::app::run(argv));
}
