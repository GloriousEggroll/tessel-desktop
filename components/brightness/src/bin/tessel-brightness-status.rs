//! `brightness_status.py`: stream the shared service's state as Waybar JSON
//! lines. gio only (no GTK); never touches monitor hardware.

use tessel_brightness::client::{BrightnessClient, UiClient};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

fn main() {
    tessel_core::i18n::initialize();
    let main_loop = glib::MainLoop::new(None, false);
    let previous: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let slot: Rc<RefCell<Option<BrightnessClient>>> = Rc::new(RefCell::new(None));
    let emit = {
        let (previous, slot, main_loop) = (previous.clone(), slot.clone(), main_loop.clone());
        Rc::new(move || {
            let (state, ready) = match slot.borrow().as_ref() {
                Some(client) => (client.state(), client.ready()),
                None => (tessel_brightness::client::initial_state(), false),
            };
            let line = tessel_brightness::status::line(&state, ready);
            if previous.borrow().as_deref() == Some(line.as_str()) {
                return;
            }
            let mut stdout = std::io::stdout().lock();
            if writeln!(stdout, "{line}").and_then(|_| stdout.flush()).is_err() {
                main_loop.quit();
                return;
            }
            previous.replace(Some(line));
        })
    };
    let changed = emit.clone();
    // The client calls back synchronously from its own methods; defer to idle
    // so the slot is never borrowed re-entrantly.
    let client = BrightnessClient::new(move || {
        let changed = changed.clone();
        glib::idle_add_local_once(move || changed());
    }, false);
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let main_loop = main_loop.clone();
        glib_unix::unix_signal_add_local(signal, move || {
            main_loop.quit();
            glib::ControlFlow::Break
        });
    }
    emit();
    slot.replace(Some(client.clone()));
    client.start();
    main_loop.run();
    client.close();
}
