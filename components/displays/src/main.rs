fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    if std::env::args().nth(1).as_deref() == Some("--daemon") {
        return glib::ExitCode::from(tessel_displays::daemon::run() as u8);
    }
    tessel_displays::window::run()
}
