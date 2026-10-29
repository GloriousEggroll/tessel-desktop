fn main() -> glib::ExitCode {
    suite_core::i18n::initialize();
    way_displays::window::run()
}
