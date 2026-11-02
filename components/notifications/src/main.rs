//! `tessel-notifications`: CLI of notification_manager.py (`--daemon`,
//! `--check`, `--settings`, `--popup` [default], `--popup-edge`, `--popup-offset`).

fn main() {
    std::process::exit(tessel_notifications::cli::main(std::env::args().collect()));
}
