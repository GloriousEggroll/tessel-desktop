//! `GLib.idle_add` from worker threads to non-`Send` main-loop handlers.
//!
//! A [`Mailbox`] is created on the main thread with a (non-`Send`) handler,
//! which lives in a thread-local registry; the mailbox itself is `Send` and
//! only carries an id plus the main context it was created on (the
//! *thread-default* context, so tests can use private contexts).  Each
//! [`Mailbox::send`] attaches an idle source (`PRIORITY_DEFAULT_IDLE`, like
//! Python's `GLib.idle_add`) whose callback looks the handler up on the thread
//! that iterates the context.  No non-`Send` value ever crosses threads.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

type Handler = Box<dyn FnMut(Box<dyn Any + Send>) -> bool>;

thread_local! {
    static HANDLERS: RefCell<HashMap<u64, Handler>> = RefCell::new(HashMap::new());
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A `Send + Sync` handle to a main-thread handler for messages of type `T`.
pub struct Mailbox<T: Send + 'static> {
    id: u64,
    context: glib::MainContext,
    _marker: PhantomData<fn(T)>,
}

impl<T: Send + 'static> Clone for Mailbox<T> {
    fn clone(&self) -> Self {
        Self { id: self.id, context: self.context.clone(), _marker: PhantomData }
    }
}

impl<T: Send + 'static> Mailbox<T> {
    /// Register `handler` on the calling (main) thread. The handler returns
    /// `false` to unregister itself (later messages are dropped).
    pub fn new(mut handler: impl FnMut(T) -> bool + 'static) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let boxed: Handler = Box::new(move |message: Box<dyn Any + Send>| match message.downcast::<T>() {
            Ok(message) => handler(*message),
            Err(_) => true,
        });
        HANDLERS.with(|handlers| handlers.borrow_mut().insert(id, boxed));
        Self { id, context: glib::MainContext::ref_thread_default(), _marker: PhantomData }
    }

    /// A one-shot mailbox: the handler runs for the first message only.
    pub fn once(handler: impl FnOnce(T) + 'static) -> Self {
        let mut handler = Some(handler);
        Self::new(move |message| {
            if let Some(handler) = handler.take() {
                handler(message);
            }
            false
        })
    }

    /// Deliver `message` on the mailbox's main context (any thread).
    pub fn send(&self, message: T) {
        let id = self.id;
        let mut message = Some(message);
        let source = glib::idle_source_new(Some("wayshell-fileops"), glib::Priority::DEFAULT_IDLE, move || {
            if let Some(message) = message.take() {
                dispatch(id, Box::new(message));
            }
            glib::ControlFlow::Break
        });
        source.attach(Some(&self.context));
    }

    /// Unregister the handler (call on the main thread).
    pub fn close(&self) {
        HANDLERS.with(|handlers| handlers.borrow_mut().remove(&self.id));
    }

    pub fn is_open(&self) -> bool {
        HANDLERS.with(|handlers| handlers.borrow().contains_key(&self.id))
    }
}

fn dispatch(id: u64, message: Box<dyn Any + Send>) {
    // Take the handler out while it runs so it may send or register others.
    let Some(mut handler) = HANDLERS.with(|handlers| handlers.borrow_mut().remove(&id)) else {
        return;
    };
    if handler(message) {
        HANDLERS.with(|handlers| {
            handlers.borrow_mut().entry(id).or_insert(handler);
        });
    }
}
