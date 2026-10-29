//! Capture state for Waydock (capture_tray.py); deliberately no second tray icon.

use gio::prelude::*;
use std::cell::{Cell, RefCell};

pub const INTERFACE: &str = "org.waydock.Capture.Status";
pub const PATH: &str = "/org/waydock/Capture/Status";
pub const XML: &str = r#"<node><interface name="org.waydock.Capture.Status">
<method name="GetState"><arg type="s" direction="out"/><arg type="b" direction="out"/></method>
<signal name="Changed"><arg type="s"/><arg type="b"/></signal>
</interface></node>"#;

pub struct State {
    pub state: RefCell<String>,
    pub armed: Cell<bool>,
}

pub struct CaptureTray {
    bus: Option<gio::DBusConnection>,
    pub status: std::rc::Rc<State>,
    registration: RefCell<Option<gio::RegistrationId>>,
}

/// `('stopping' if stopping else 'recording' if recording else 'preparing' if busy and capture_recording else 'idle')`.
pub fn state_name(recording: bool, busy: bool, stopping: bool, capture_recording: bool) -> &'static str {
    if stopping {
        "stopping"
    } else if recording {
        "recording"
    } else if busy && capture_recording {
        "preparing"
    } else {
        "idle"
    }
}

impl CaptureTray {
    pub fn new(bus: Option<gio::DBusConnection>) -> Self {
        let status = std::rc::Rc::new(State { state: RefCell::new("idle".into()), armed: Cell::new(false) });
        let mut registration = None;
        if let Some(bus) = &bus {
            let info = gio::DBusNodeInfo::for_xml(XML).expect("status XML");
            let interface = info.lookup_interface(INTERFACE).expect("status interface");
            let state = status.clone();
            match bus.register_object(PATH, &interface).method_call(move |_, _, _, _, _, _, invocation| {
                invocation.return_value(Some(&(state.state.borrow().clone(), state.armed.get()).to_variant()));
            }).build() {
                Ok(id) => registration = Some(id),
                Err(error) => eprintln!("waycapture: could not register {PATH}: {}", error.message()),
            }
        }
        Self { bus, status, registration: RefCell::new(registration) }
    }

    pub fn state(&self) -> (String, bool) {
        (self.status.state.borrow().clone(), self.status.armed.get())
    }

    /// Emit `Changed(s, b)` only when the pair changes.
    pub fn update(&self, recording: bool, armed: bool, busy: bool, stopping: bool, capture_recording: bool) {
        let state = state_name(recording, busy, stopping, capture_recording);
        if (state, armed) != (self.status.state.borrow().as_str(), self.status.armed.get()) {
            *self.status.state.borrow_mut() = state.into();
            self.status.armed.set(armed);
            if let Some(bus) = &self.bus {
                let _ = bus.emit_signal(None, PATH, INTERFACE, "Changed", Some(&(state, armed).to_variant()));
            }
        }
    }

    pub fn close(&self) {
        if let (Some(bus), Some(id)) = (&self.bus, self.registration.borrow_mut().take()) {
            let _ = bus.unregister_object(id);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn states() {
        use super::state_name;
        assert_eq!(state_name(false, false, false, true), "idle");
        assert_eq!(state_name(false, true, false, true), "preparing");
        assert_eq!(state_name(false, true, false, false), "idle");
        assert_eq!(state_name(true, true, false, true), "recording");
        assert_eq!(state_name(true, true, true, true), "stopping");
    }
}
