//! `notification-manager`: CLI of notification_manager.py (`--daemon`,
//! `--check`, `--settings`, `--popup` [default], `--popup-edge`, `--popup-offset`).

fn main() {
    std::process::exit(notification_manager::cli::main(std::env::args().collect()));
}
