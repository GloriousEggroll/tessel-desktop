fn main() -> glib::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--daemon") {
        return glib::ExitCode::from(tessel_displays::daemon::run() as u8);
    }
    tessel_core::i18n::initialize();
    tessel_displays::window::run()
}
