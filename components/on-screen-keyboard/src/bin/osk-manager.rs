//! Waykeys service entry point (osk_manager.py): `--background`, `--toggle`,
//! `--show`, `--hide`, `--settings`, `--quit`; a second launch forwards its
//! arguments to the running instance (GApplication `org.waydock.OnScreenKeyboard`).
use gtk::prelude::*;
use suite_gtk::gtk;

fn main() -> glib::ExitCode {
    suite_core::i18n::initialize();
    let manager = waykeys::manager::Manager::new(Default::default());
    let code = manager.app.run();
    drop(manager);
    code
}
