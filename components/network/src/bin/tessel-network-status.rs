//! `network_status.py`: stream NetworkManager status as safely escaped
//! Waybar JSON lines (deduplicated, flushed; exits on EPIPE/SIGTERM/SIGINT).
//! gio only: no GTK and no libnm.

use tessel_network::backend::{NetworkBackend, Options};
use std::cell::RefCell;
use std::io::Write;
use std::rc::{Rc, Weak};

fn main() {
    tessel_core::i18n::initialize();
    let main_loop = glib::MainLoop::new(None, false);
    let previous: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let slot: Rc<RefCell<Weak<tessel_network::backend::Inner>>> = Rc::new(RefCell::new(Weak::new()));
    let emit = {
        let (previous, slot, main_loop) = (previous.clone(), slot.clone(), main_loop.clone());
        Rc::new(move || {
            let line = match slot.borrow().upgrade() {
                Some(inner) => tessel_network::status::line(&NetworkBackend(inner).state()),
                None => tessel_network::status::line(&tessel_network::backend::State::new()),
            };
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
    let backend = NetworkBackend::new(move || changed(), move |_| failed(), Options::status());
    slot.replace(backend.downgrade());
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let main_loop = main_loop.clone();
        tessel_core::glib_compat::unix_signal_add_local(signal, move || {
            main_loop.quit();
            glib::ControlFlow::Break
        });
    }
    // SIGPIPE must surface as EPIPE (BrokenPipeError) rather than kill us.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    emit();
    backend.start();
    main_loop.run();
    backend.close();
}
