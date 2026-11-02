//! `desktop_suite_failed.py UNIT.service`: offer a Restart once a suite unit gave up.

fn main() {
    tessel_core::i18n::initialize();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(tessel_session_startup::failed::main(&args));
}
