//! Transport between the window/workspace backends and the compositor.
//!
//! The Python backends ran `waydock-toplevel` / `waydock-workspaces` and read
//! their JSON lines. The Rust backends keep that line protocol as their
//! *internal* event format, but produce it in-process: a
//! [`suite_wayland::Client`] with `model.render` set renders exactly the
//! helper's lines (`take_lines`) as protocol events arrive. Requests go
//! straight to the client instead of a stdin pipe.
//!
//! * [`WaylandConnector`] — the real thing: connects and runs the helpers'
//!   three startup roundtrips on a worker thread (so a stalled compositor
//!   cannot block the GTK main loop; the backend's 8 s startup timeout aborts
//!   it by shutting the socket down), then attaches a
//!   [`suite_wayland::GlibClient`] on the caller's thread-default context.
//! * [`fake::FakeConnector`] / [`fake::FakeLink`] — scripted connections over
//!   a real [`Model`] fed with mocked protocol events, for unit tests here and
//!   in other modules (no compositor needed).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use suite_wayland::model::{self, Model, Render};
use suite_wayland::{ActionError, Client, GlibClient, Protocols};

/// One request for the compositor (helper stdin `<action> <id>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Activate(u64),
    Minimize(u64),
    Unminimize(u64),
    Close(u64),
    ActivateWorkspace(u64),
}

/// What a live link reports after startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// The model changed; rendered helper lines are waiting in `take_lines`.
    Changed(model::Event),
    /// The connection is gone (the helper's stdout EOF).
    Disconnected(String),
}

/// A connected session (= one helper process lifetime).
pub trait Link {
    /// Helper JSON lines rendered since the last call.
    fn take_lines(&self) -> Vec<String>;
    /// Model events since the last call (only used once, right after startup).
    fn take_events(&self) -> Vec<model::Event>;
    fn has_toplevels(&self) -> bool;
    fn has_seat(&self) -> bool;
    fn toplevel_version(&self) -> u32;
    fn has_workspaces(&self) -> bool;
    /// Report the workspace state once (`publish_workspaces`), like the helper
    /// does right before `ready`.
    fn publish_workspaces(&self);
    fn request(&self, request: Request) -> Result<(), ActionError>;
    fn set_handler(&self, handler: Box<dyn FnMut(LinkEvent)>);
    fn close(&self);
}

/// Result of a connection attempt.
pub enum Startup {
    /// Startup roundtrips done; lines/events of the startup are still queued.
    Ready(Rc<dyn Link>),
    /// The helper ended during startup: it printed `lines`, and `message`
    /// is its error line (None = it just exited).
    Failed { lines: Vec<String>, message: Option<String> },
    /// The "helper" could not be started at all.
    NotStarted,
}

/// Starts connection attempts. `connect` returns an abort function; after
/// it was called, `done` must not run (it is ignored by the backends anyway).
pub trait Connector {
    fn connect(&self, protocols: Protocols, done: Box<dyn FnOnce(Startup)>) -> Box<dyn FnOnce()>;
}

// ---- the real connector ---------------------------------------------------------------

/// `wl_display_connect(NULL)` + the helpers' startup on a worker thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct WaylandConnector;

#[derive(Default)]
struct Handshake {
    fd: Option<i32>,
    finished: bool,
}

/// A freshly started [`Client`] crossing from the worker to the main thread.
struct SendClient(Client);
// SAFETY: the only !Send part of `Client` is the shared-memory mapping of an
// image capture, and these clients never bind capture protocols
// (Protocols::TOPLEVELS / WORKSPACES) nor start a capture before the hand-off.
// Every other part (Connection, EventQueue, proxies, Model) is Send.
unsafe impl Send for SendClient {}

type StartupResult = Result<SendClient, (Vec<String>, Option<String>)>;

fn finish(handshake: &Mutex<Handshake>) {
    if let Ok(mut state) = handshake.lock() {
        state.finished = true;
    }
}

fn startup(protocols: Protocols, handshake: &Mutex<Handshake>) -> StartupResult {
    let conn = match suite_wayland::connect() {
        Ok(conn) => conn,
        Err(_) => {
            finish(handshake);
            return Err((Vec::new(), Some("Cannot connect to the Wayland compositor.".to_owned())));
        }
    };
    {
        use std::os::fd::AsRawFd;
        if let Ok(mut state) = handshake.lock() {
            state.fd = Some(conn.backend().poll_fd().as_raw_fd());
        }
    }
    let mut client = Client::new(conn, protocols);
    if protocols.workspaces {
        client.model.workspaces_published = false;
        client.model.render = Some(Render::Workspaces);
    } else {
        client.model.render = Some(Render::Toplevel);
    }
    let mut ok = true;
    for _ in 0..3 {
        if client.roundtrip().is_err() {
            ok = false;
            break;
        }
    }
    // Mark finished before the client (and its fd) can be dropped, so an
    // abort never shuts down a recycled descriptor.
    finish(handshake);
    if ok {
        Ok(SendClient(client))
    } else {
        Err((client.model.take_lines(), None))
    }
}

impl Connector for WaylandConnector {
    fn connect(&self, protocols: Protocols, done: Box<dyn FnOnce(Startup)>) -> Box<dyn FnOnce()> {
        let context = glib::MainContext::ref_thread_default();
        let handshake = Arc::new(Mutex::new(Handshake::default()));
        let worker = handshake.clone();
        let handle = gio::spawn_blocking(move || startup(protocols, &worker));
        let aborted = Rc::new(Cell::new(false));
        let flag = aborted.clone();
        let attach_context = context.clone();
        context.spawn_local(async move {
            let result = handle.await;
            if flag.get() {
                return;
            }
            let outcome = match result {
                Ok(Ok(SendClient(client))) => Startup::Ready(Rc::new(GlibLink::attach(client, &attach_context))),
                Ok(Err((lines, message))) => Startup::Failed { lines, message },
                Err(_) => Startup::NotStarted,
            };
            done(outcome);
        });
        Box::new(move || {
            aborted.set(true);
            if let Ok(state) = handshake.lock() {
                if let (false, Some(fd)) = (state.finished, state.fd) {
                    // Wakes the worker's blocked roundtrip; it then returns an error.
                    unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
                }
            }
        })
    }
}

type Handler = Box<dyn FnMut(LinkEvent)>;

/// A started client on the GLib main loop.
struct GlibLink {
    client: GlibClient,
    handler: Rc<RefCell<Option<Handler>>>,
    closed: Rc<Cell<bool>>,
}

fn deliver(slot: &RefCell<Option<Handler>>, closed: &Cell<bool>, event: LinkEvent) {
    let taken = slot.borrow_mut().take();
    if let Some(mut handler) = taken {
        handler(event);
        let mut current = slot.borrow_mut();
        if current.is_none() && !closed.get() {
            *current = Some(handler);
        }
    }
}

impl GlibLink {
    fn attach(client: Client, context: &glib::MainContext) -> Self {
        let handler: Rc<RefCell<Option<Handler>>> = Rc::new(RefCell::new(None));
        let closed = Rc::new(Cell::new(false));
        let (slot, flag) = (handler.clone(), closed.clone());
        let client = GlibClient::attach(client, Some(context), move |_, event| {
            let event = match event {
                suite_wayland::Event::Changed(change) => LinkEvent::Changed(change),
                suite_wayland::Event::Disconnected(message) => LinkEvent::Disconnected(message),
                suite_wayland::Event::Captured(..) => return,
            };
            deliver(&slot, &flag, event);
        });
        Self { client, handler, closed }
    }
}

impl Link for GlibLink {
    fn take_lines(&self) -> Vec<String> { self.client.borrow_mut().model.take_lines() }
    fn take_events(&self) -> Vec<model::Event> { self.client.borrow_mut().model.take_events() }
    fn has_toplevels(&self) -> bool { self.client.borrow().has_toplevels() }
    fn has_seat(&self) -> bool { self.client.borrow().has_seat() }
    fn toplevel_version(&self) -> u32 { self.client.borrow().toplevel_version() }
    fn has_workspaces(&self) -> bool { self.client.borrow().has_workspaces() }
    fn publish_workspaces(&self) { self.client.borrow_mut().model.publish_workspaces() }
    fn request(&self, request: Request) -> Result<(), ActionError> {
        self.client.with(|client| match request {
            Request::Activate(id) => client.activate(id),
            Request::Minimize(id) => client.set_minimized(id, true),
            Request::Unminimize(id) => client.set_minimized(id, false),
            Request::Close(id) => client.close(id),
            Request::ActivateWorkspace(id) => client.activate_workspace(id),
        })
    }
    fn set_handler(&self, handler: Box<dyn FnMut(LinkEvent)>) {
        if !self.closed.get() {
            self.handler.replace(Some(handler));
        }
    }
    fn close(&self) {
        self.closed.set(true);
        self.handler.replace(None);
        self.client.close();
    }
}

// ---- fakes ----------------------------------------------------------------------------

/// Scripted connections for tests (no compositor, no threads).
pub mod fake {
    use super::*;

    type Responder = Box<dyn FnMut(&mut Model, Request) -> Result<(), ActionError>>;

    /// A connection over a real [`Model`]: drive it with the model's protocol
    /// event methods (`toplevel_new`, `output_added`, …), then [`FakeLink::emit`].
    pub struct FakeLink {
        pub model: RefCell<Model>,
        pub toplevels: Cell<bool>,
        pub seat: Cell<bool>,
        pub version: Cell<u32>,
        pub workspaces: Cell<bool>,
        pub requests: RefCell<Vec<Request>>,
        pub closed: Cell<bool>,
        responder: RefCell<Option<Responder>>,
        handler: RefCell<Option<Handler>>,
    }

    impl FakeLink {
        fn with_render(render: Render) -> Rc<Self> {
            let mut model = Model::new();
            model.render = Some(render);
            model.workspaces_published = render != Render::Workspaces;
            Rc::new(Self {
                model: RefCell::new(model), toplevels: Cell::new(true), seat: Cell::new(true), version: Cell::new(3),
                workspaces: Cell::new(true), requests: RefCell::new(Vec::new()), closed: Cell::new(false),
                responder: RefCell::new(None), handler: RefCell::new(None),
            })
        }
        /// A foreign-toplevel connection (helper `waydock-toplevel`).
        pub fn toplevel() -> Rc<Self> { Self::with_render(Render::Toplevel) }
        /// An ext-workspace connection (helper `waydock-workspaces`).
        pub fn workspaces() -> Rc<Self> { Self::with_render(Render::Workspaces) }
        /// Simulate the compositor's reaction to requests (called in `request`).
        pub fn respond(&self, responder: impl FnMut(&mut Model, Request) -> Result<(), ActionError> + 'static) {
            self.responder.replace(Some(Box::new(responder)));
        }
        /// Deliver the model's pending events (as the GLib source would).
        pub fn emit(&self) {
            let events = self.model.borrow_mut().take_events();
            for event in events {
                self.deliver(LinkEvent::Changed(event));
            }
        }
        /// The compositor connection broke.
        pub fn disconnect(&self, message: &str) { self.deliver(LinkEvent::Disconnected(message.to_owned())) }
        fn deliver(&self, event: LinkEvent) {
            let taken = self.handler.borrow_mut().take();
            if let Some(mut handler) = taken {
                handler(event);
                let mut slot = self.handler.borrow_mut();
                if slot.is_none() && !self.closed.get() {
                    *slot = Some(handler);
                }
            }
        }
        pub fn has_handler(&self) -> bool { self.handler.borrow().is_some() }
    }

    impl Link for FakeLink {
        fn take_lines(&self) -> Vec<String> { self.model.borrow_mut().take_lines() }
        fn take_events(&self) -> Vec<model::Event> { self.model.borrow_mut().take_events() }
        fn has_toplevels(&self) -> bool { self.toplevels.get() }
        fn has_seat(&self) -> bool { self.seat.get() }
        fn toplevel_version(&self) -> u32 { self.version.get() }
        fn has_workspaces(&self) -> bool { self.workspaces.get() }
        fn publish_workspaces(&self) { self.model.borrow_mut().publish_workspaces() }
        fn request(&self, request: Request) -> Result<(), ActionError> {
            self.requests.borrow_mut().push(request);
            let mut responder = self.responder.borrow_mut();
            match responder.as_mut() {
                Some(respond) => respond(&mut self.model.borrow_mut(), request),
                None => Ok(()),
            }
        }
        fn set_handler(&self, handler: Box<dyn FnMut(LinkEvent)>) {
            if !self.closed.get() {
                self.handler.replace(Some(handler));
            }
        }
        fn close(&self) {
            self.closed.set(true);
            self.handler.replace(None);
        }
    }

    type Pending = Rc<RefCell<Option<Box<dyn FnOnce(Startup)>>>>;

    /// Records connection attempts; complete them with [`FakeConnector::complete`].
    #[derive(Default)]
    pub struct FakeConnector {
        attempts: RefCell<Vec<(Protocols, Pending)>>,
        pub aborted: Rc<Cell<usize>>,
    }

    impl FakeConnector {
        pub fn new() -> Rc<Self> { Rc::new(Self::default()) }
        /// Number of `connect` calls so far.
        pub fn attempts(&self) -> usize { self.attempts.borrow().len() }
        pub fn protocols(&self, index: usize) -> Option<Protocols> { self.attempts.borrow().get(index).map(|a| a.0) }
        /// Whether the latest attempt is still waiting (not completed or aborted).
        pub fn waiting(&self) -> bool {
            self.attempts.borrow().last().is_some_and(|(_, pending)| pending.borrow().is_some())
        }
        /// Complete the latest attempt; false if none is waiting.
        pub fn complete(&self, startup: Startup) -> bool {
            let pending = self.attempts.borrow().last().map(|(_, p)| p.clone());
            let done = pending.and_then(|p| p.borrow_mut().take());
            match done {
                Some(done) => {
                    done(startup);
                    true
                }
                None => false,
            }
        }
        /// Complete the latest attempt with `link`.
        pub fn ready(&self, link: &Rc<FakeLink>) -> bool { self.complete(Startup::Ready(link.clone())) }
    }

    impl Connector for FakeConnector {
        fn connect(&self, protocols: Protocols, done: Box<dyn FnOnce(Startup)>) -> Box<dyn FnOnce()> {
            let pending: Pending = Rc::new(RefCell::new(Some(done)));
            self.attempts.borrow_mut().push((protocols, pending.clone()));
            let aborted = self.aborted.clone();
            Box::new(move || {
                if pending.borrow_mut().take().is_some() {
                    aborted.set(aborted.get() + 1);
                }
            })
        }
    }

    impl Connector for Rc<FakeConnector> {
        fn connect(&self, protocols: Protocols, done: Box<dyn FnOnce(Startup)>) -> Box<dyn FnOnce()> {
            (**self).connect(protocols, done)
        }
    }
}
