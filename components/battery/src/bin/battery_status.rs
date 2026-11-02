//! `tessel-battery-status` (replaces `battery_status.py`): JSON lines for Waybar/Tessel Dock,
//! printed only when changed; exits on SIGTERM/SIGINT or a closed stdout.
//! Built from the GTK-free modules only (not the library crate, which links
//! GTK), so the stream needs just GLib/GIO.
#![allow(dead_code)]

#[path = "../status.rs"]
mod status;

use tessel_power_core::{PowerBackend, PowerSystem};
use status::encode;
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

fn main() {
    tessel_core::i18n::initialize();
    let main_loop = glib::MainLoop::new(None, false);
    let previous: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let slot: Rc<RefCell<Option<PowerBackend>>> = Rc::new(RefCell::new(None));
    let changed: Rc<dyn Fn()> = {
        let (main_loop, previous, slot) = (main_loop.clone(), previous.clone(), slot.clone());
        Rc::new(move || {
            let Some(backend) = slot.borrow().clone() else { return };
            let encoded = encode(&backend);
            if previous.borrow().as_deref() == Some(encoded.as_str()) {
                return;
            }
            let mut stdout = std::io::stdout().lock();
            if writeln!(stdout, "{encoded}").and_then(|_| stdout.flush()).is_ok() {
                previous.replace(Some(encoded));
            } else {
                main_loop.quit();
            }
        })
    };
    let (on_changed, on_error) = (changed.clone(), changed.clone());
    let backend = PowerBackend::batteries_only(move || on_changed(), move |_| on_error());
    slot.replace(Some(backend.clone()));
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let main_loop = main_loop.clone();
        tessel_core::glib_compat::unix_signal_add_local(signal, move || {
            main_loop.quit();
            glib::ControlFlow::Break
        });
    }
    changed();
    backend.start();
    main_loop.run();
    backend.close();
    slot.replace(None);
}
