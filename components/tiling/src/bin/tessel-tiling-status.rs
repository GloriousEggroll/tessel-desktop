//! tiling_status.py: `main()` with argv forced to `--watch` (spawned by
//! Tessel Dock, resident). Calls the watch loop directly so this binary never
//! references (or dynamically links) GTK.

fn main() {
    tessel_core::i18n::initialize();
    std::process::exit(tessel_tiling::cli::watch(std::time::Duration::from_secs(1)));
}
