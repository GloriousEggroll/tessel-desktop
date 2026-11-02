//! PAM conversations. Production uses `PolkitAgentSession` (libpolkit spawns /
//! connects to polkit's setuid `polkit-agent-helper-1`, which alone talks to
//! PAM and reports the result to polkitd). Tests inject a fake helper.

use crate::ffi;
use crate::identity::Identity;
use crate::secret::Secret;
use glib::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    /// `request(prompt, echo_on)`
    Request { prompt: String, echo_on: bool },
    /// `show-info(text)`
    Info(String),
    /// `show-error(text)`
    Error(String),
    /// `completed(gained_authorization)`
    Completed(bool),
}

pub type EventHandler = Rc<dyn Fn(SessionEvent)>;

pub trait Session {
    fn initiate(&self);
    /// Send a PAM response. Implementations must not log or keep it.
    fn response(&self, response: &CStr);
    fn cancel(&self);
    /// Disconnect the event handler (Python disconnects every signal handler
    /// before cancelling, so a cancel's `completed` is never seen).
    fn disconnect(&self);
}

/// `session_factory(identity, cookie)` + the handler to connect.
pub type SessionFactory = Rc<dyn Fn(&Identity, &Secret, EventHandler) -> Result<Rc<dyn Session>, String>>;

/// `PolkitAgent.Session.new(identity, cookie)`.
pub struct PolkitSession {
    object: glib::Object,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl PolkitSession {
    pub fn new(identity: &Identity, cookie: &Secret, handler: EventHandler) -> Result<Rc<dyn Session>, String> {
        let text = CString::new(identity.text.as_str()).map_err(|_| "invalid identity".to_owned())?;
        let mut error: *mut glib::ffi::GError = std::ptr::null_mut();
        let identity = unsafe { ffi::polkit_identity_from_string(text.as_ptr(), &mut error) };
        if identity.is_null() {
            let error: glib::Error = unsafe { from_glib_full(error) };
            return Err(error.message().to_owned());
        }
        let session = unsafe { ffi::polkit_agent_session_new(identity, cookie.as_c_str().as_ptr()) };
        unsafe { glib::gobject_ffi::g_object_unref(identity as *mut _) };
        if session.is_null() {
            return Err("could not create a polkit session".into());
        }
        let object: glib::Object = unsafe { from_glib_full(session as *mut glib::gobject_ffi::GObject) };
        let mut handlers = Vec::new();
        let h = handler.clone();
        handlers.push(object.connect_local("request", false, move |values| {
            let prompt = values[1].get::<Option<String>>().ok().flatten().unwrap_or_default();
            let echo_on = values[2].get::<bool>().unwrap_or(false);
            h(SessionEvent::Request { prompt, echo_on });
            None
        }));
        let h = handler.clone();
        handlers.push(object.connect_local("show-info", false, move |values| {
            h(SessionEvent::Info(values[1].get::<Option<String>>().ok().flatten().unwrap_or_default()));
            None
        }));
        let h = handler.clone();
        handlers.push(object.connect_local("show-error", false, move |values| {
            h(SessionEvent::Error(values[1].get::<Option<String>>().ok().flatten().unwrap_or_default()));
            None
        }));
        let h = handler;
        handlers.push(object.connect_local("completed", false, move |values| {
            h(SessionEvent::Completed(values[1].get::<bool>().unwrap_or(false)));
            None
        }));
        Ok(Rc::new(Self { object, handlers: RefCell::new(handlers) }))
    }

    pub fn factory() -> SessionFactory {
        Rc::new(PolkitSession::new)
    }

    fn ptr(&self) -> *mut ffi::PolkitAgentSession {
        self.object.as_ptr() as *mut _
    }
}

impl Session for PolkitSession {
    fn initiate(&self) {
        unsafe { ffi::polkit_agent_session_initiate(self.ptr()) }
    }

    fn response(&self, response: &CStr) {
        // Goes straight to libpolkit's PAM helper pipe; never logged.
        unsafe { ffi::polkit_agent_session_response(self.ptr(), response.as_ptr()) }
    }

    fn cancel(&self) {
        unsafe { ffi::polkit_agent_session_cancel(self.ptr()) }
    }

    fn disconnect(&self) {
        for id in self.handlers.take() {
            self.object.disconnect(id);
        }
    }
}

impl Drop for PolkitSession {
    fn drop(&mut self) {
        for id in self.handlers.take() {
            self.object.disconnect(id);
        }
    }
}

/// Test helper modelled on test_authentication.py's FakeSession: `initiate`
/// asks `Password:` (hidden), `cancel` emits `completed(false)`, responses are
/// recorded. Never talks to PAM or polkitd.
pub struct FakeSession {
    pub identity: Identity,
    pub cookie: String,
    pub responses: RefCell<Vec<String>>,
    pub cancelled: std::cell::Cell<bool>,
    handler: RefCell<Option<EventHandler>>,
}

impl FakeSession {
    pub fn emit(&self, event: SessionEvent) {
        let handler = self.handler.borrow().clone();
        if let Some(handler) = handler {
            handler(event);
        }
    }

    /// A factory that also records every session it creates.
    pub fn factory(created: Rc<RefCell<Vec<Rc<FakeSession>>>>) -> SessionFactory {
        Rc::new(move |identity: &Identity, cookie: &Secret, handler: EventHandler| {
            let session = Rc::new(FakeSession {
                identity: identity.clone(),
                cookie: cookie.as_str().to_owned(),
                responses: RefCell::default(),
                cancelled: Default::default(),
                handler: RefCell::new(Some(handler)),
            });
            created.borrow_mut().push(session.clone());
            Ok(session as Rc<dyn Session>)
        })
    }
}

impl Session for FakeSession {
    fn initiate(&self) {
        self.emit(SessionEvent::Request { prompt: "Password:".into(), echo_on: false });
    }

    fn response(&self, response: &CStr) {
        self.responses.borrow_mut().push(response.to_string_lossy().into_owned());
    }

    fn cancel(&self) {
        self.cancelled.set(true);
        self.emit(SessionEvent::Completed(false));
    }

    fn disconnect(&self) {
        self.handler.replace(None);
    }
}
