//! ScreenCast portal client (capture_portal.py). Only the portal's restricted
//! PipeWire remote is used.
#![allow(deprecated)]

use crate::compat::capture_selection;
use crate::native::StreamProps;
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tessel_core::pyerr::python_error_text;
use tessel_core::tr;

pub const NAME: &str = "org.freedesktop.portal.Desktop";
pub const PATH: &str = "/org/freedesktop/portal/desktop";
pub const INTERFACE: &str = "org.freedesktop.portal.ScreenCast";

/// One portal stream: PipeWire node id and its properties.
pub type Stream = (u32, StreamProps);

type Ready = Box<dyn Fn(&PortalSession)>;
type Failed = Box<dyn Fn(&PortalSession, String)>;
type Step = Box<dyn FnOnce(&PortalSession, &glib::VariantDict) -> Result<(), String>>;

pub struct Inner {
    ready: Ready,
    failed: Failed,
    pub types: u32,
    pub cursor: bool,
    pub multiple: bool,
    pub output: Option<String>,
    pub bus: RefCell<Option<gio::DBusConnection>>,
    /// Outputs the chooser reported (`Selected`), initially `[output]`.
    pub outputs: RefCell<Vec<String>>,
    pub session: RefCell<Option<String>>,
    pub request: RefCell<Option<String>>,
    pub fd: RefCell<Option<OwnedFd>>,
    pub streams: RefCell<Vec<Stream>>,
    pub closed: Arc<AtomicBool>,
    pub cancelled: Cell<bool>,
    subscriptions: RefCell<Vec<gio::SignalSubscriptionId>>,
    pending: RefCell<HashMap<String, Step>>,
    timer: RefCell<Option<glib::SourceId>>,
    selection: RefCell<Option<Rc<capture_selection::SelectionBroker>>>,
}

#[derive(Clone)]
pub struct PortalSession(pub Rc<Inner>);

impl PartialEq for PortalSession {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// A Send handle for worker threads: fresh restricted connections on demand.
#[derive(Clone)]
pub struct RemoteHandle {
    bus: gio::DBusConnection,
    session: String,
    closed: Arc<AtomicBool>,
}

// SAFETY: GDBusConnection is thread-safe; the handle holds only it and strings.
unsafe impl Send for RemoteHandle {}
unsafe impl Sync for RemoteHandle {}

impl RemoteHandle {
    /// Worker-only: a fresh restricted connection for each consumer. Duplicating
    /// an already-used socket does not create a new PipeWire connection.
    pub fn remote_fd(&self) -> Result<OwnedFd, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(tr!("Screen access has ended."));
        }
        let path = glib::variant::ObjectPath::try_from(self.session.clone()).map_err(|e| e.to_string())?;
        let empty = glib::VariantDict::new(None).end();
        let parameters = glib::Variant::tuple_from_iter([path.to_variant(), empty]);
        let (reply, list) = self.bus.call_with_unix_fd_list_sync(Some(NAME), PATH, INTERFACE, "OpenPipeWireRemote", Some(&parameters),
            Some(glib::VariantTy::new("(h)").unwrap()), gio::DBusCallFlags::NONE, 5000, None::<&gio::UnixFDList>, None::<&gio::Cancellable>)
            .map_err(|e| python_error_text(&e))?;
        let handle = reply.child_value(0).get::<glib::variant::Handle>().map(|h| h.0).unwrap_or(-1);
        let fd = list.ok_or_else(|| "No file descriptor".to_string())?.get(handle).map_err(|e| python_error_text(&e))?;
        if self.closed.load(Ordering::SeqCst) {
            drop(fd);
            return Err(tr!("Screen access has ended."));
        }
        Ok(fd)
    }
}

fn token() -> String {
    format!("capture_{}", glib::uuid_string_random().replace('-', ""))
}

fn sender(bus: &gio::DBusConnection) -> String {
    bus.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default()
}

pub struct Options {
    pub types: u32,
    pub cursor: bool,
    pub multiple: bool,
    pub bus: Option<gio::DBusConnection>,
    pub output: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self { types: 1, cursor: true, multiple: false, bus: None, output: None }
    }
}

impl PortalSession {
    pub fn new(ready: impl Fn(&PortalSession) + 'static, failed: impl Fn(&PortalSession, String) + 'static, options: Options) -> Self {
        let output = options.output.filter(|o| !o.is_empty());
        let inner = Rc::new(Inner {
            ready: Box::new(ready), failed: Box::new(failed), types: options.types, cursor: options.cursor, multiple: options.multiple,
            outputs: RefCell::new(output.iter().cloned().collect()), output, bus: RefCell::new(options.bus.clone()),
            session: RefCell::default(), request: RefCell::default(), fd: RefCell::default(), streams: RefCell::default(),
            closed: Arc::new(AtomicBool::new(false)), cancelled: Cell::new(false), subscriptions: RefCell::default(),
            pending: RefCell::default(), timer: RefCell::default(), selection: RefCell::default(),
        });
        let session = PortalSession(inner);
        let weak = Rc::downgrade(&session.0);
        *session.0.timer.borrow_mut() = Some(glib::timeout_add_seconds_local(120, move || {
            if let Some(inner) = weak.upgrade() {
                let session = PortalSession(inner);
                session.0.timer.borrow_mut().take();
                session.fail(tr!("Screen selection timed out."));
            }
            glib::ControlFlow::Break
        }));
        if options.bus.is_none() {
            let weak = Rc::downgrade(&session.0);
            gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |result| {
                let Some(inner) = weak.upgrade() else { return };
                let session = PortalSession(inner);
                match result {
                    Ok(bus) => {
                        *session.0.bus.borrow_mut() = Some(bus);
                        if !session.closed() {
                            session.create();
                        }
                    }
                    Err(error) => session.fail(python_error_text(&error)),
                }
            });
        } else {
            session.create();
        }
        session
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub fn closed(&self) -> bool {
        self.0.closed.load(Ordering::SeqCst)
    }

    pub fn cancelled(&self) -> bool {
        self.0.cancelled.get()
    }

    pub fn streams(&self) -> Vec<Stream> {
        self.0.streams.borrow().clone()
    }

    pub fn outputs(&self) -> Vec<String> {
        self.0.outputs.borrow().clone()
    }

    pub fn session_handle(&self) -> Option<String> {
        self.0.session.borrow().clone()
    }

    pub fn has_fd(&self) -> bool {
        self.0.fd.borrow().is_some()
    }

    pub fn remote(&self) -> Option<RemoteHandle> {
        Some(RemoteHandle { bus: self.0.bus.borrow().clone()?, session: self.session_handle()?, closed: self.0.closed.clone() })
    }

    fn bus(&self) -> gio::DBusConnection {
        self.0.bus.borrow().clone().expect("portal bus")
    }

    fn create(&self) {
        let bus = self.bus();
        let weak = self.weak();
        let response = bus.signal_subscribe(Some(NAME), Some("org.freedesktop.portal.Request"), Some("Response"), None, None,
            gio::DBusSignalFlags::NONE, move |_, _, path, _, _, parameters| {
                if let Some(inner) = weak.upgrade() {
                    PortalSession(inner).response(path, parameters);
                }
            });
        let weak = self.weak();
        let closed = bus.signal_subscribe(Some(NAME), Some("org.freedesktop.portal.Session"), Some("Closed"), None, None,
            gio::DBusSignalFlags::NONE, move |_, _, path, _, _, _| {
                if let Some(inner) = weak.upgrade() {
                    let session = PortalSession(inner);
                    if session.session_handle().as_deref() == Some(path) {
                        session.fail(tr!("Screen sharing ended. The portal session was closed."));
                    }
                }
            });
        self.0.subscriptions.borrow_mut().extend([response, closed]);
        let session_token = token();
        let handle = format!("{PATH}/session/{}/{session_token}", sender(&bus));
        *self.0.session.borrow_mut() = Some(handle.clone());
        let broker = capture_selection::broker(&bus);
        let weak = self.weak();
        broker.observers.borrow_mut().insert(handle.clone(), Box::new(move |outputs| {
            if let Some(inner) = weak.upgrade() {
                *inner.outputs.borrow_mut() = outputs;
            }
        }));
        if let Some(output) = &self.0.output {
            broker.targets.borrow_mut().insert(handle, output.clone());
        }
        *self.0.selection.borrow_mut() = Some(broker);
        let options = glib::VariantDict::new(None);
        options.insert("session_handle_token", session_token);
        self.request("CreateSession", Vec::new(), options, Box::new(|session, results| session.created(results)));
    }

    fn request(&self, method: &str, positional: Vec<glib::Variant>, options: glib::VariantDict, callback: Step) {
        let bus = self.bus();
        let request_token = token();
        let expected = format!("{PATH}/request/{}/{request_token}", sender(&bus));
        *self.0.request.borrow_mut() = Some(expected.clone());
        self.0.pending.borrow_mut().insert(expected.clone(), callback);
        options.insert("handle_token", request_token);
        let mut items = positional;
        items.push(options.end());
        let parameters = glib::Variant::tuple_from_iter(items);
        let weak = self.weak();
        bus.call(Some(NAME), PATH, INTERFACE, method, Some(&parameters), None, gio::DBusCallFlags::NONE, 10000, None::<&gio::Cancellable>,
            move |result| {
                let Some(inner) = weak.upgrade() else { return };
                let session = PortalSession(inner);
                match result {
                    Ok(reply) => {
                        let actual = reply.child_value(0).str().unwrap_or("").to_string();
                        if session.closed() {
                            session.close_handle(Some(&actual), "Request");
                            let handle = session.session_handle();
                            session.close_handle(handle.as_deref(), "Session");
                        } else if actual != expected {
                            let moved = session.0.pending.borrow_mut().remove(&expected);
                            if let Some(step) = moved {
                                session.0.pending.borrow_mut().insert(actual.clone(), step);
                                *session.0.request.borrow_mut() = Some(actual);
                            }
                        }
                    }
                    Err(error) => session.fail(python_error_text(&error)),
                }
            });
    }

    fn response(&self, path: &str, parameters: &glib::Variant) {
        let callback = self.0.pending.borrow_mut().remove(path);
        let Some(callback) = callback else { return };
        if self.closed() {
            return;
        }
        *self.0.request.borrow_mut() = None;
        let code = parameters.child_value(0).get::<u32>().unwrap_or(2);
        let results = glib::VariantDict::new(Some(&parameters.child_value(1)));
        if code != 0 {
            self.0.cancelled.set(code == 1);
            self.fail(if code == 1 { tr!("Screen access was cancelled.") } else { tr!("The portal could not start capture.") });
            return;
        }
        if let Err(error) = callback(self, &results) {
            self.fail(error);
        }
    }

    fn created(&self, results: &glib::VariantDict) -> Result<(), String> {
        let handle = results.lookup::<String>("session_handle").ok().flatten()
            .or_else(|| results.lookup_value("session_handle", None).and_then(|v| v.str().map(String::from)))
            .ok_or_else(|| "'session_handle'".to_string())?;
        *self.0.session.borrow_mut() = Some(handle.clone());
        let path = glib::variant::ObjectPath::try_from(handle).map_err(|e| e.to_string())?;
        let options = glib::VariantDict::new(None);
        options.insert("types", self.0.types);
        options.insert("multiple", self.0.multiple);
        options.insert("cursor_mode", if self.0.cursor { 2u32 } else { 1u32 });
        options.insert("persist_mode", 0u32);
        self.request("SelectSources", vec![path.to_variant()], options, Box::new(|session, results| session.selected(results)));
        Ok(())
    }

    fn selected(&self, _results: &glib::VariantDict) -> Result<(), String> {
        let path = glib::variant::ObjectPath::try_from(self.session_handle().unwrap_or_default()).map_err(|e| e.to_string())?;
        self.request("Start", vec![path.to_variant(), "".to_variant()], glib::VariantDict::new(None),
                     Box::new(|session, results| session.started(results)));
        Ok(())
    }

    fn started(&self, results: &glib::VariantDict) -> Result<(), String> {
        let mut streams = Vec::new();
        if let Some(value) = results.lookup_value("streams", None) {
            for index in 0..value.n_children() {
                let item = value.child_value(index);
                let node = item.child_value(0).get::<u32>().unwrap_or(0);
                let props = glib::VariantDict::new(Some(&item.child_value(1)));
                streams.push((node, StreamProps::from_variant(&props)));
            }
        }
        if streams.is_empty() {
            return Err(tr!("The portal returned no capture sources."));
        }
        *self.0.streams.borrow_mut() = streams;
        let path = glib::variant::ObjectPath::try_from(self.session_handle().unwrap_or_default()).map_err(|e| e.to_string())?;
        let parameters = glib::Variant::tuple_from_iter([path.to_variant(), glib::VariantDict::new(None).end()]);
        let weak = self.weak();
        self.bus().call_with_unix_fd_list(Some(NAME), PATH, INTERFACE, "OpenPipeWireRemote", Some(&parameters),
            Some(glib::VariantTy::new("(h)").unwrap()), gio::DBusCallFlags::NONE, 10000, None::<&gio::UnixFDList>, None::<&gio::Cancellable>,
            move |result| {
                let Some(inner) = weak.upgrade() else { return };
                let session = PortalSession(inner);
                let outcome = result.map_err(|e| python_error_text(&e)).and_then(|(reply, list)| {
                    let handle = reply.child_value(0).get::<glib::variant::Handle>().map(|h| h.0).unwrap_or(-1);
                    list.ok_or_else(|| "No file descriptor".to_string())?.get(handle).map_err(|e| python_error_text(&e))
                });
                match outcome {
                    Ok(fd) => {
                        if session.closed() {
                            drop(fd);
                            return;
                        }
                        *session.0.fd.borrow_mut() = Some(fd);
                        if let Some(timer) = session.0.timer.borrow_mut().take() {
                            timer.remove();
                        }
                        (session.0.ready)(&session);
                    }
                    Err(error) => session.fail(error),
                }
            });
        Ok(())
    }

    fn fail(&self, message: String) {
        if !self.closed() {
            self.close();
            (self.0.failed)(self, message);
        }
    }

    fn close_handle(&self, path: Option<&str>, kind: &str) {
        let Some(bus) = self.0.bus.borrow().clone() else { return };
        let Some(path) = path.filter(|p| !p.is_empty()) else { return };
        bus.call(Some(NAME), path, &format!("org.freedesktop.portal.{kind}"), "Close", None, None, gio::DBusCallFlags::NONE, 2000,
                 None::<&gio::Cancellable>, |_| {});
    }

    pub fn close(&self) {
        if self.0.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let handle = self.session_handle();
        if let (Some(broker), Some(handle)) = (self.0.selection.borrow().as_ref(), handle.as_ref()) {
            broker.targets.borrow_mut().remove(handle);
            broker.observers.borrow_mut().remove(handle);
        }
        if let Some(timer) = self.0.timer.borrow_mut().take() {
            timer.remove();
        }
        let request = self.0.request.borrow().clone();
        self.close_handle(request.as_deref(), "Request");
        self.close_handle(handle.as_deref(), "Session");
        if let Some(bus) = self.0.bus.borrow().clone() {
            for subscription in self.0.subscriptions.take() {
                bus.signal_unsubscribe(subscription);
            }
        }
        self.0.pending.borrow_mut().clear();
        self.0.fd.borrow_mut().take();
    }
}
