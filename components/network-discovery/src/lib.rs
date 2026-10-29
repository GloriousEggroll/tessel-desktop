//! Discover Network Devices: the Avahi discovery toggle and device list
//! (port of btmanager/network-discovery, adapted from NDDT, MIT).
//!
//! `backend` (systemd D-Bus state + pkexec toggle, no GTK) is the library
//! Waydock embeds; `browse` parses `avahi-browse`; `app` is the GTK popup.

pub mod app;
pub mod backend;
pub mod browse;
pub mod compat;

/// `--status`: run a main loop until the backend is ready, print one JSON
/// line (no GTK, no single instance; the installer's health check).
pub fn read_status() -> String {
    let context = glib::MainContext::default();
    let _guard = context.acquire().expect("main context");
    let ready = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let slot = ready.clone();
    let holder: std::rc::Rc<std::cell::RefCell<Option<backend::DiscoveryBackend>>> = Default::default();
    let weak = std::rc::Rc::downgrade(&holder);
    let backend = backend::DiscoveryBackend::new(move || {
        let Some(holder) = weak.upgrade() else { return };
        let Some(backend) = holder.borrow().clone() else { return };
        if backend.ready() && slot.borrow().is_none() {
            slot.replace(Some(backend::snapshot_text(&backend.snapshot())));
        }
    });
    holder.replace(Some(backend.clone()));
    backend.start();
    while ready.borrow().is_none() {
        context.iteration(true);
    }
    backend.close();
    // Let the fire-and-forget Unsubscribe go out.
    while context.iteration(false) {}
    let text = ready.borrow().clone().unwrap_or_default();
    text
}
