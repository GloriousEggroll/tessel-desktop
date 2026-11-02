//! `tessel-tiling [--toggle | --enable | --disable | --restore | --status | --watch |
//! --settings | --popup] [--popup-edge E] [--popup-offset N]`.

fn main() {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    std::process::exit(tessel_tiling::cli::main(&argv));
}
