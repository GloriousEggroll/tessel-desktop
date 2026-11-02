//! GLib main-loop integration: a custom `GSource` on the wl_display fd.
//!
//! * prepare: flush, then dispatch whatever libwayland already queued for us
//!   (events another reader — GDK, on a shared display — pulled off the socket)
//!   and register a waker for events routed to our queue later. Only our own
//!   `Dispatch` impls run here; user handlers never do.
//! * check: the fd is readable, or events are waiting.
//! * dispatch: read the socket (non-blocking), dispatch, flush, then call the
//!   handler for each event with **no borrow held**, so handlers may call
//!   [`GlibClient::with`] / [`GlibClient::borrow_mut`] freely.
//!
//! Reading only in the dispatch phase is what makes sharing GTK's wl_display
//! safe: GDK prepares/reads/cancels within prepare→check, so no GDK read is
//! outstanding while any source dispatches (two outstanding reads on one
//! thread would deadlock in `wl_display_read_events`).

use crate::client::{Client, Event};
use glib::translate::{FromGlibPtrFull, ToGlibPtr};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What a [`raw_source`] drives.
pub(crate) trait Pump {
    /// Dispatch already-queued events into state; true if the handler should run.
    fn prepare(&self) -> bool;
    /// `revents` of the display fd; true to dispatch.
    fn check(&self, revents: u16) -> bool;
    /// Read + dispatch + deliver. false removes the source.
    fn dispatch(&self) -> bool;
}

#[repr(C)]
struct RawSource {
    base: glib::ffi::GSource,
    poll: glib::ffi::GPollFD,
    pump: *mut Box<dyn Pump>,
}

unsafe extern "C" fn prepare(source: *mut glib::ffi::GSource, timeout: *mut i32) -> glib::ffi::gboolean {
    *timeout = -1;
    let raw = source as *mut RawSource;
    (*(*raw).pump).prepare() as glib::ffi::gboolean
}

unsafe extern "C" fn check(source: *mut glib::ffi::GSource) -> glib::ffi::gboolean {
    let raw = source as *mut RawSource;
    (*(*raw).pump).check((*raw).poll.revents) as glib::ffi::gboolean
}

unsafe extern "C" fn dispatch(source: *mut glib::ffi::GSource, _: glib::ffi::GSourceFunc, _: glib::ffi::gpointer) -> glib::ffi::gboolean {
    let raw = source as *mut RawSource;
    (*(*raw).pump).dispatch() as glib::ffi::gboolean
}

unsafe extern "C" fn finalize(source: *mut glib::ffi::GSource) {
    let raw = source as *mut RawSource;
    let pump = std::mem::replace(&mut (*raw).pump, std::ptr::null_mut());
    if !pump.is_null() {
        drop(Box::from_raw(pump));
    }
}

static mut FUNCS: glib::ffi::GSourceFuncs = glib::ffi::GSourceFuncs {
    prepare: Some(prepare),
    check: Some(check),
    dispatch: Some(dispatch),
    finalize: Some(finalize),
    closure_callback: None,
    closure_marshal: None,
};

/// Create and attach a source polling `fd` (IN/ERR/HUP) that drives `pump`.
pub(crate) fn raw_source(fd: i32, pump: Box<dyn Pump>, context: &glib::MainContext, name: &str) -> glib::Source {
    unsafe {
        let source = glib::ffi::g_source_new(std::ptr::addr_of_mut!(FUNCS), std::mem::size_of::<RawSource>() as u32);
        let raw = source as *mut RawSource;
        (*raw).poll = glib::ffi::GPollFD {
            fd,
            events: (glib::ffi::G_IO_IN | glib::ffi::G_IO_ERR | glib::ffi::G_IO_HUP) as u16,
            revents: 0,
        };
        (*raw).pump = Box::into_raw(Box::new(pump));
        glib::ffi::g_source_add_poll(source, &mut (*raw).poll);
        glib::ffi::g_source_set_priority(source, glib::ffi::G_PRIORITY_DEFAULT);
        let name = std::ffi::CString::new(name).unwrap_or_default();
        glib::ffi::g_source_set_name(source, name.as_ptr());
        glib::ffi::g_source_attach(source, context.to_glib_none().0);
        glib::Source::from_glib_full(source)
    }
}

/// Wakes the main context when libwayland routes events to our queue from
/// another queue's dispatch (wayland-client calls the registered waker).
pub(crate) struct Wake {
    context: glib::MainContext,
    pub(crate) woken: AtomicBool,
}

impl std::task::Wake for Wake {
    fn wake(self: Arc<Self>) { self.wake_by_ref() }
    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
        self.context.wakeup();
    }
}

impl Wake {
    pub(crate) fn new(context: &glib::MainContext) -> Arc<Self> {
        Arc::new(Self { context: context.clone(), woken: AtomicBool::new(false) })
    }
    pub(crate) fn take(&self) -> bool { self.woken.swap(false, Ordering::AcqRel) }
}

type Handler = Box<dyn FnMut(&GlibClient, Event)>;

struct Inner {
    client: RefCell<Client>,
    handler: RefCell<Option<Handler>>,
    source: RefCell<Option<glib::Source>>,
    wake: Arc<Wake>,
    closed: Cell<bool>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.destroy();
        }
    }
}

/// A [`Client`] dispatched by the GLib main loop. Cheap to clone.
#[derive(Clone)]
pub struct GlibClient(Rc<Inner>);

struct ClientPump(Weak<Inner>);

impl ClientPump {
    fn pending(inner: &Inner) -> bool {
        let Ok(mut client) = inner.client.try_borrow_mut() else { return false };
        let _ = client.flush();
        let waker = std::task::Waker::from(inner.wake.clone());
        let mut cx = std::task::Context::from_waker(&waker);
        if client.poll_pending(&mut cx).is_err() {
            return true; // dispatch reports the failure
        }
        client.has_events()
    }
}

impl Pump for ClientPump {
    // A dropped client: report ready so dispatch removes the source.
    fn prepare(&self) -> bool {
        let Some(inner) = self.0.upgrade() else { return true };
        !inner.closed.get() && (inner.wake.take() | Self::pending(&inner))
    }
    fn check(&self, revents: u16) -> bool {
        let Some(inner) = self.0.upgrade() else { return true };
        !inner.closed.get() && (revents != 0 || inner.wake.woken.load(Ordering::Acquire)
                                || inner.client.try_borrow().is_ok_and(|c| c.has_events()))
    }
    fn dispatch(&self) -> bool {
        let Some(inner) = self.0.upgrade() else { return false };
        if inner.closed.get() {
            return false;
        }
        let (events, failure) = {
            let Ok(mut client) = inner.client.try_borrow_mut() else { return true };
            inner.wake.take();
            let waker = std::task::Waker::from(inner.wake.clone());
            let mut cx = std::task::Context::from_waker(&waker);
            let failure = client.read_socket().err().map(|e| e.to_string())
                .or_else(|| client.poll_pending(&mut cx).err().map(|e| e.to_string()))
                .or_else(|| client.connection().protocol_error().map(|e| e.to_string()));
            let _ = client.flush();
            (client.take_events(), failure)
        };
        let client = GlibClient(inner);
        for event in events {
            client.deliver(event);
        }
        if let Some(message) = failure {
            client.deliver(Event::Disconnected(message));
            client.close();
            return false;
        }
        !client.0.closed.get()
    }
}

impl GlibClient {
    /// Attach `client` to `context` (None = the thread-default context).
    /// `handler` runs for every event with no borrow of the client held.
    pub fn attach(client: Client, context: Option<&glib::MainContext>, handler: impl FnMut(&GlibClient, Event) + 'static) -> Self {
        let context = context.cloned().unwrap_or_else(glib::MainContext::ref_thread_default);
        let fd = client.fd();
        let inner = Rc::new(Inner {
            client: RefCell::new(client), handler: RefCell::new(Some(Box::new(handler))), source: RefCell::new(None),
            wake: Wake::new(&context), closed: Cell::new(false),
        });
        let source = raw_source(fd, Box::new(ClientPump(Rc::downgrade(&inner))), &context, "tessel-wayland");
        inner.source.replace(Some(source));
        Self(inner)
    }

    fn deliver(&self, event: Event) {
        let handler = self.0.handler.borrow_mut().take();
        if let Some(mut handler) = handler {
            handler(self, event);
            let mut slot = self.0.handler.borrow_mut();
            if slot.is_none() && !self.0.closed.get() {
                *slot = Some(handler);
            }
        }
    }

    /// Replace the event handler.
    pub fn set_handler(&self, handler: impl FnMut(&GlibClient, Event) + 'static) {
        self.0.handler.replace(Some(Box::new(handler)));
    }
    pub fn borrow(&self) -> std::cell::Ref<'_, Client> { self.0.client.borrow() }
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, Client> { self.0.client.borrow_mut() }
    /// Run `f` on the client and flush its requests.
    pub fn with<R>(&self, f: impl FnOnce(&mut Client) -> R) -> R {
        let mut client = self.0.client.borrow_mut();
        let result = f(&mut client);
        let _ = client.flush();
        result
    }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }
    /// Detach from the main loop and drop the handler. The connection closes
    /// when the last clone is dropped.
    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        if let Some(source) = self.0.source.take() {
            source.destroy();
        }
        self.0.handler.replace(None);
    }
}
