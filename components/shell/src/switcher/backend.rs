//! Window backend for the switcher (tessel-dock/window_backend.py semantics) on an
//! in-process wlr-foreign-toplevel client instead of the `tessel-shell-toplevel`
//! helper process: same ready/can_activate/error/windows state, session-scoped
//! ids `"<session>:<native>"`, coalesced change notifications, 8 s startup
//! timeout and reconnect with 1 s → 15 s backoff.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_wayland::model::Event as ModelEvent;
use tessel_wayland::{Client, Event, GlibClient, Protocols};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WinInfo {
    pub id: String,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub outputs: Vec<String>,
    pub parent_id: String,
}

/// What the switcher needs from a window source (Python `WindowBackend`).
pub trait Backend {
    fn start(&self);
    fn ready(&self) -> bool;
    fn can_activate(&self) -> bool;
    fn error(&self) -> String;
    /// Windows in backend (dict insertion) order.
    fn windows(&self) -> Vec<WinInfo>;
    fn activate(&self, id: &str) -> bool;
    fn close_window(&self, id: &str) -> bool;
    fn minimize(&self, _id: &str) -> bool {
        false
    }
    fn close(&self);

    fn window(&self, id: &str) -> Option<WinInfo> {
        self.windows().into_iter().find(|w| w.id == id)
    }
    fn contains(&self, id: &str) -> bool {
        self.window(id).is_some()
    }
}

pub type Callback = Rc<dyn Fn()>;
pub type ErrorCallback = Rc<dyn Fn(&str)>;

#[derive(Default)]
struct State {
    ready: bool,
    can_activate: bool,
    error: String,
    windows: Vec<WinInfo>,
}

pub struct NativeBackend {
    state: RefCell<State>,
    on_changed: Callback,
    on_error: ErrorCallback,
    session: Cell<u64>,
    started: Cell<bool>,
    closed: Cell<bool>,
    client: RefCell<Option<GlibClient>>,
    changed_source: RefCell<Option<glib::SourceId>>,
    startup_source: RefCell<Option<glib::SourceId>>,
    reconnect_source: RefCell<Option<glib::SourceId>>,
    retry_delay: Cell<u32>,
    me: RefCell<Weak<NativeBackend>>,
}

impl NativeBackend {
    pub fn new(on_changed: Callback, on_error: ErrorCallback) -> Rc<Self> {
        let backend = Rc::new(Self {
            state: RefCell::default(), on_changed, on_error, session: Cell::new(0), started: Cell::new(false),
            closed: Cell::new(false), client: RefCell::new(None), changed_source: RefCell::new(None),
            startup_source: RefCell::new(None), reconnect_source: RefCell::new(None), retry_delay: Cell::new(1000),
            me: RefCell::new(Weak::new()),
        });
        *backend.me.borrow_mut() = Rc::downgrade(&backend);
        backend
    }

    fn weak(&self) -> Weak<NativeBackend> {
        self.me.borrow().clone()
    }

    fn changed(&self) {
        if self.closed.get() || self.changed_source.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let id = glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.changed_source.borrow_mut().take();
                if !this.closed.get() {
                    (this.on_changed)();
                }
            }
        });
        *self.changed_source.borrow_mut() = Some(id);
    }

    fn fail(&self, message: &str) -> bool {
        self.state.borrow_mut().error = message.to_string();
        if !self.closed.get() {
            (self.on_error)(message);
            self.changed();
        }
        false
    }

    fn clear(&self) {
        let mut state = self.state.borrow_mut();
        state.ready = false;
        state.can_activate = false;
        state.windows.clear();
    }

    fn launch(&self) {
        if self.closed.get() {
            return;
        }
        self.session.set(self.session.get() + 1);
        let session = self.session.get();
        self.clear();
        let client = match Client::connect(Protocols::TOPLEVELS) {
            Ok(c) => c,
            Err(_) => {
                self.fail("Cannot connect to the Wayland compositor.");
                self.lost(session, None);
                return;
            }
        };
        let mut client = client;
        let mut running = true;
        for _ in 0..3 {
            if client.roundtrip().is_err() {
                running = false;
                break;
            }
        }
        let weak = self.weak();
        let events = client.take_events();
        let has_toplevels = client.has_toplevels();
        let has_seat = client.has_seat();
        let glib_client = GlibClient::attach(client, None, move |c, event| {
            if let Some(this) = weak.upgrade() {
                this.event(session, c, event);
            }
        });
        *self.client.borrow_mut() = Some(glib_client.clone());
        for event in events {
            self.event(session, &glib_client, event);
        }
        if !running {
            self.lost(session, Some("The compositor's window connection ended."));
            return;
        }
        if !has_toplevels {
            self.fail("This compositor does not support foreign-toplevel window management.");
            self.lost(session, None);
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.ready = true;
            state.can_activate = has_seat;
            state.error.clear();
        }
        self.retry_delay.set(1000);
        self.changed();
    }

    fn native(&self, id: &str) -> Option<u64> {
        let (session, native) = id.split_once(':')?;
        (session == self.session.get().to_string()).then(|| native.parse().ok()).flatten()
    }

    fn info(&self, session: u64, client: &Client, id: u64) -> Option<WinInfo> {
        let window = client.model.window(id)?;
        if !window.published {
            return None;
        }
        let mut outputs: Vec<String> = Vec::new();
        for name in client.model.window_outputs(window) {
            if !outputs.iter().any(|o| o == name) {
                outputs.push(name.to_string());
            }
        }
        Some(WinInfo {
            id: format!("{session}:{id}"), app_id: window.app_id.clone(), title: window.title.clone(),
            activated: window.activated, fullscreen: window.fullscreen, minimized: window.minimized,
            maximized: window.maximized, outputs,
            parent_id: if window.parent != 0 { format!("{session}:{}", window.parent) } else { String::new() },
        })
    }

    fn event(&self, session: u64, client: &GlibClient, event: Event) {
        if self.closed.get() || session != self.session.get() {
            return;
        }
        match event {
            Event::Changed(ModelEvent::Window(id)) => {
                let info = { let c = client.borrow(); self.info(session, &c, id) };
                if let Some(info) = info {
                    let mut state = self.state.borrow_mut();
                    match state.windows.iter_mut().find(|w| w.id == info.id) {
                        Some(slot) => *slot = info,
                        None => state.windows.push(info),
                    }
                }
                self.changed();
            }
            Event::Changed(ModelEvent::WindowClosed(id)) => {
                let key = format!("{session}:{id}");
                self.state.borrow_mut().windows.retain(|w| w.id != key);
                self.changed();
            }
            Event::Changed(ModelEvent::Error { message, fatal }) => {
                self.fail(&message);
                if fatal {
                    self.lost(session, None);
                }
            }
            Event::Disconnected(_) => self.lost(session, Some("The compositor's window connection ended.")),
            Event::Changed(_) => self.changed(),
            Event::Captured(..) => {}
        }
    }

    /// `_lost`: invalidate every id, report, reconnect with backoff.
    fn lost(&self, session: u64, message: Option<&str>) {
        if self.closed.get() || session != self.session.get() {
            return;
        }
        self.session.set(self.session.get() + 1);
        self.clear();
        if let Some(client) = self.client.borrow_mut().take() {
            client.close();
        }
        if let Some(source) = self.startup_source.borrow_mut().take() {
            source.remove();
        }
        let error = self.state.borrow().error.clone();
        let message = if !error.is_empty() { error } else { message.unwrap_or("The compositor's window connection ended.").to_string() };
        self.fail(&message);
        if self.reconnect_source.borrow().is_none() {
            let weak = self.weak();
            let delay = self.retry_delay.get();
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(delay as u64), move || {
                if let Some(this) = weak.upgrade() {
                    this.reconnect_source.borrow_mut().take();
                    this.launch();
                }
            });
            *self.reconnect_source.borrow_mut() = Some(id);
        }
        self.retry_delay.set((self.retry_delay.get() * 2).min(15000));
    }

    fn send(&self, action: &str, id: &str) -> bool {
        let state_ok = self.state.borrow().ready;
        let client = self.client.borrow().clone();
        let (Some(client), true) = (client, state_ok) else {
            return self.fail("Window management is unavailable.");
        };
        let native = match self.native(id) {
            Some(n) if self.state.borrow().windows.iter().any(|w| w.id == id) => n,
            _ => return self.fail("That window is no longer available."),
        };
        if action == "activate" && !self.state.borrow().can_activate {
            return self.fail("No input seat is available to focus windows.");
        }
        self.state.borrow_mut().error.clear();
        let result = client.with(|c| match action {
            "activate" => c.activate(native),
            "minimize" => c.set_minimized(native, true),
            "unminimize" => c.set_minimized(native, false),
            _ => c.close(native),
        });
        match result {
            Ok(()) => true,
            // The helper reported these asynchronously; the request itself was accepted.
            Err(error) => {
                self.fail(&error.to_string());
                true
            }
        }
    }
}

impl Backend for NativeBackend {
    fn start(&self) {
        if self.started.get() || self.closed.get() {
            return;
        }
        self.started.set(true);
        self.launch();
        if !self.state.borrow().ready && self.reconnect_source.borrow().is_none() {
            let weak = self.weak();
            let session = self.session.get();
            let id = glib::timeout_add_local_once(std::time::Duration::from_secs(8), move || {
                if let Some(this) = weak.upgrade() {
                    this.startup_source.borrow_mut().take();
                    if !this.state.borrow().ready {
                        this.lost(session, Some("The compositor did not provide window information."));
                    }
                }
            });
            *self.startup_source.borrow_mut() = Some(id);
        }
    }
    fn ready(&self) -> bool {
        self.state.borrow().ready
    }
    fn can_activate(&self) -> bool {
        self.state.borrow().can_activate
    }
    fn error(&self) -> String {
        self.state.borrow().error.clone()
    }
    fn windows(&self) -> Vec<WinInfo> {
        self.state.borrow().windows.clone()
    }
    fn window(&self, id: &str) -> Option<WinInfo> {
        self.state.borrow().windows.iter().find(|w| w.id == id).cloned()
    }
    fn contains(&self, id: &str) -> bool {
        self.state.borrow().windows.iter().any(|w| w.id == id)
    }
    fn activate(&self, id: &str) -> bool {
        self.send("activate", id)
    }
    fn close_window(&self, id: &str) -> bool {
        self.send("close", id)
    }
    fn minimize(&self, id: &str) -> bool {
        self.send("minimize", id)
    }
    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.session.set(self.session.get() + 1);
        for slot in [&self.changed_source, &self.startup_source, &self.reconnect_source] {
            if let Some(source) = slot.borrow_mut().take() {
                source.remove();
            }
        }
        if let Some(client) = self.client.borrow_mut().take() {
            client.close();
        }
        self.clear();
    }
}
