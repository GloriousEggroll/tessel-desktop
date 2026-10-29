//! `waytile [--toggle | --enable | --disable | --restore | --status | --watch |
//! --settings | --popup] [--popup-edge E] [--popup-offset N]` (tiling_manager.py).

/// The Python shims exec `[python, <script>.py, *args]`: drop the
/// interpreter so argv[0] is the script path (argparse's prog name).
pub fn shim_argv(mut argv: Vec<String>) -> Vec<String> {
    if argv.len() > 1 && argv[1].ends_with(".py") && std::path::Path::new(&argv[0]).file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("python")) {
        argv.remove(0);
    }
    argv
}

fn main() {
    suite_core::i18n::initialize();
    let argv = shim_argv(std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect());
    // tiling_status.py: `main()` with argv forced to `--watch`.
    let code = if argv.first().is_some_and(|a| a.ends_with("tiling_status.py")) {
        tiling_manager::cli::main(&[argv[0].clone(), "--watch".into()])
    } else {
        tiling_manager::cli::main(&argv)
    };
    std::process::exit(code);
}
