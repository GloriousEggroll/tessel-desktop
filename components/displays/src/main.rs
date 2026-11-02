fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    way_displays::window::run()
}
