//! Tessel Keyboard service entry point (osk_manager.py): `--background`, `--toggle`,
//! `--show`, `--hide`, `--settings`, `--quit`; a second launch forwards its
//! arguments to the running instance (GApplication `org.tessel.OnScreenKeyboard`).
use gtk::prelude::*;
use tessel_gtk::gtk;

fn main() -> glib::ExitCode {
    tessel_core::i18n::initialize();
    let manager = tessel_keyboard::manager::Manager::new(Default::default());
    let code = manager.app.run();
    drop(manager);
    code
}
