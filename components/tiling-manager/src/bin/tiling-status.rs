//! tiling_status.py: `main()` with argv forced to `--watch` (spawned by
//! Waydock, resident). Calls the watch loop directly so this binary never
//! references (or dynamically links) GTK.

fn main() {
    suite_core::i18n::initialize();
    std::process::exit(tiling_manager::cli::watch(std::time::Duration::from_secs(1)));
}
