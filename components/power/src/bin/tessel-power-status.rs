//! `power_status.py`: stream read-only power status as Waybar JSON lines.
//! gio only (no GTK); a profiles-only backend keeps it tiny.

use tessel_power_core::backend::{Mode, Options, PowerBackend, PowerSystem};
use std::cell::RefCell;
use std::io::Write;
use std::rc::{Rc, Weak};

fn main() {
    tessel_core::i18n::initialize();
    let main_loop = glib::MainLoop::new(None, false);
    let previous: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let slot: Rc<RefCell<Weak<tessel_power_core::backend::Inner>>> = Rc::new(RefCell::new(Weak::new()));
    let emit = {
        let (previous, slot, main_loop) = (previous.clone(), slot.clone(), main_loop.clone());
        Rc::new(move || {
            let Some(inner) = slot.borrow().upgrade() else {
                return;
            };
            let line = tessel_power::status::line(&PowerBackend(inner).state());
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
    let (changed, failed) = (emit.clone(), emit.clone());
    let options = Options { mode: Mode::ProfilesOnly, ..Options::default() };
    let backend = PowerBackend::new(move || changed(), move |_| failed(), options);
    slot.replace(Rc::downgrade(&backend.0));
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let main_loop = main_loop.clone();
        tessel_core::glib_compat::unix_signal_add_local(signal, move || {
            main_loop.quit();
            glib::ControlFlow::Break
        });
    }
    emit();
    backend.start();
    main_loop.run();
    backend.close();
}
