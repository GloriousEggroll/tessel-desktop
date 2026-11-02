//! AuthenticationAgent (authentication_manager.py:200-300): FIFO request queue,
//! one window at a time, caller cancellation through idle callbacks, shutdown.

use crate::identity::Identity;
use crate::listener::{AuthenticateCall, Completion, Listener};
use crate::secret::Secret;
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const CANCELLED: &str = "Authentication cancelled";
pub const UNAVAILABLE: &str = "Authentication is unavailable";

/// One authentication request (Python's request dict).
pub struct Request {
    pub action_id: String,
    pub message: String,
    /// Wiped as soon as the request finishes.
    pub cookie: RefCell<Secret>,
    pub identities: Vec<Identity>,
    pub cancellable: Option<gio::Cancellable>,
    task: RefCell<Option<Box<dyn Completion>>>,
    handler: RefCell<Option<gio::CancelledHandlerId>>,
    window: RefCell<Option<Rc<dyn Prompt>>>,
    done: Cell<bool>,
}

impl Request {
    pub fn is_done(&self) -> bool {
        self.done.get()
    }

    pub fn window(&self) -> Option<Rc<dyn Prompt>> {
        self.window.borrow().clone()
    }
}

/// What the agent needs from a request window.
pub trait Prompt {
    fn set_application(&self, application: Option<&gtk::Application>);
    fn present(&self);
    fn start(&self);
    /// Idempotent; calls the window's `finished(cancelled)` once.
    fn finish(&self, cancelled: bool);
    fn as_any(&self) -> &dyn std::any::Any;
}

/// `window_factory(request, finished)`; an `Err` is "Could not display".
pub type WindowFactory = Rc<dyn Fn(Rc<Request>, Box<dyn FnOnce(bool)>) -> Result<Rc<dyn Prompt>, String>>;

pub struct AuthenticationAgent {
    inner: Rc<Inner>,
    pub listener: Listener,
}

struct Inner {
    application: Option<gtk::Application>,
    factory: WindowFactory,
    requests: RefCell<Vec<Rc<Request>>>,
    window: RefCell<Option<Rc<dyn Prompt>>>,
    closed: Cell<bool>,
}

impl AuthenticationAgent {
    pub fn new(application: Option<gtk::Application>, factory: WindowFactory) -> Self {
        let inner = Rc::new(Inner { application, factory, requests: RefCell::default(), window: RefCell::default(),
                                    closed: Cell::new(false) });
        let weak = Rc::downgrade(&inner);
        let listener = Listener::new(move |call: AuthenticateCall| match weak.upgrade() {
            Some(inner) => Inner::authenticate(&inner, Box::new(call.task), call.action_id, call.message, call.cookie,
                                               &call.identities, call.cancellable),
            None => Box::new(call.task).cancel(UNAVAILABLE),
        });
        Self { inner, listener }
    }

    /// The listener's `authenticate` handler (also usable directly in tests).
    pub fn authenticate(&self, task: Box<dyn Completion>, action_id: &str, message: &str, cookie: Secret,
                        identities: &[String], cancellable: Option<gio::Cancellable>) {
        Inner::authenticate(&self.inner, task, action_id.into(), message.into(), cookie, identities, cancellable);
    }

    pub fn requests(&self) -> Vec<Rc<Request>> {
        self.inner.requests.borrow().clone()
    }

    pub fn window(&self) -> Option<Rc<dyn Prompt>> {
        self.inner.window.borrow().clone()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.get()
    }

    /// Cancel every request (each completes exactly once).
    pub fn close(&self) {
        self.inner.closed.set(true);
        let requests = self.inner.requests.borrow().clone();
        for request in requests {
            Inner::cancel(&self.inner, &request);
        }
    }
}

impl Drop for AuthenticationAgent {
    fn drop(&mut self) {
        self.listener.clear_handler();
    }
}

impl Inner {
    fn authenticate(this: &Rc<Self>, task: Box<dyn Completion>, action_id: String, message: String, cookie: Secret,
                    names: &[String], cancellable: Option<gio::Cancellable>) {
        let identities: Vec<Identity> = names.iter().map(|name| Identity::parse(name)).collect();
        if this.closed.get() || identities.is_empty() {
            task.cancel(UNAVAILABLE);
            return;
        }
        let request = Rc::new(Request {
            action_id, message, cookie: RefCell::new(cookie), identities, cancellable: cancellable.clone(),
            task: RefCell::new(Some(task)), handler: RefCell::new(None), window: RefCell::new(None), done: Cell::new(false),
        });
        this.requests.borrow_mut().push(request.clone());
        if let Some(cancellable) = cancellable {
            // Complete on the GTK main loop; never disconnect inside the
            // Cancellable callback (g_cancellable_disconnect would deadlock).
            let weak = Rc::downgrade(this);
            let pending = Rc::downgrade(&request);
            let id = cancellable.connect_cancelled_local(move |_| {
                glib::idle_add_local_once(move || {
                    if let (Some(inner), Some(request)) = (weak.upgrade(), pending.upgrade()) {
                        Inner::cancel(&inner, &request);
                    }
                });
            });
            request.handler.replace(id);
        }
        Self::schedule(this);
    }

    fn schedule(this: &Rc<Self>) {
        let weak: Weak<Self> = Rc::downgrade(this);
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade() {
                Inner::show_next(&inner);
            }
        });
    }

    fn cancel(this: &Rc<Self>, request: &Rc<Request>) {
        if !request.done.get() {
            let window = request.window.borrow().clone();
            match window {
                Some(window) => window.finish(true),
                None => Self::complete(this, request, true),
            }
        }
    }

    fn show_next(this: &Rc<Self>) {
        if this.closed.get() || this.window.borrow().is_some() {
            return;
        }
        let Some(request) = this.requests.borrow().first().cloned() else { return };
        if request.cancellable.as_ref().is_some_and(|c| c.is_cancelled()) {
            Self::cancel(this, &request);
            return;
        }
        let weak = Rc::downgrade(this);
        let done_request = Rc::downgrade(&request);
        let finished: Box<dyn FnOnce(bool)> = Box::new(move |cancelled| {
            if let (Some(inner), Some(request)) = (weak.upgrade(), done_request.upgrade()) {
                Inner::complete(&inner, &request, cancelled);
            }
        });
        match (this.factory)(request.clone(), finished) {
            Ok(window) => {
                this.window.replace(Some(window.clone()));
                request.window.replace(Some(window.clone()));
                window.set_application(this.application.as_ref());
                window.present();
                window.start();
            }
            Err(_) => {
                // Do not log request data or PAM conversation details.
                eprintln!("Could not display authentication request");
                Self::cancel(this, &request);
            }
        }
    }

    fn complete(this: &Rc<Self>, request: &Rc<Request>, cancelled: bool) {
        if request.done.replace(true) {
            return;
        }
        if let (Some(id), Some(cancellable)) = (request.handler.take(), request.cancellable.as_ref()) {
            cancellable.disconnect_cancelled(id);
        }
        request.cookie.borrow_mut().clear();
        let window = request.window.borrow().clone();
        let same = match (this.window.borrow().as_ref(), window.as_ref()) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        };
        if same {
            this.window.replace(None);
        }
        this.requests.borrow_mut().retain(|item| !Rc::ptr_eq(item, request));
        if let Some(task) = request.task.take() {
            if cancelled {
                task.cancel(CANCELLED);
            } else {
                // Completing the conversation is not granting permission. Polkit's
                // trusted helper alone reports successful authorization to polkitd.
                task.succeed();
            }
        }
        request.window.replace(None);
        Self::schedule(this);
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
