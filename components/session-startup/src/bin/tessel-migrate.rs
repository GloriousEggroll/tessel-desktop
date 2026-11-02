//! `tessel-migrate`: move a desktop-suite home to Tessel's layout once (autostart runs it).

fn main() {
    std::process::exit(tessel_session_startup::migrate::main());
}
