//! `luna_clock.py`: the Kasumi idle dashboard (`local.luna.Clock`).
use gtk::prelude::*;

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let config = match kasumi_screen::clock::load_config(&kasumi_screen::clock::default_config_path()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let clock = match kasumi_screen::clock::LunaClock::new(kasumi_screen::clock::Options::new(config)) {
        Ok(clock) => clock,
        Err(error) => {
            eprintln!("{error}");
            return glib::ExitCode::FAILURE;
        }
    };
    clock.application().run()
}
