//! Inhibitor FD lifetime (test_keep_awake.py InhibitorTests) with a recording
//! fake connection, then the real gio transport against a fake logind on a
//! private bus (test_keep_awake_ui.py real FD transfer). Never acquires a
//! real logind inhibitor.

mod common;

use gio::prelude::*;
use common::{pipe, read, wait_for, FakeLogin, PrivateBus, Read};
use tessel_keep_awake::backend::{is_cloexec, BusSource, InhibitReply, InhibitorBackend, LoginConnection, INHIBIT_ARGS, UNAVAILABLE};
use std::cell::{Cell, RefCell};
use std::os::fd::OwnedFd;
use std::rc::Rc;

struct Request {
    owner: String,
    cancel: gio::Cancellable,
    done: Option<Box<dyn FnOnce(InhibitReply)>>,
}

#[derive(Default)]
struct Recorder {
    requests: RefCell<Vec<Request>>,
    fail: RefCell<Option<String>>,
}

impl LoginConnection for Recorder {
    fn inhibit(&self, owner: &str, cancellable: &gio::Cancellable, done: Box<dyn FnOnce(InhibitReply)>) -> Result<(), String> {
        if let Some(error) = self.fail.borrow().clone() {
            return Err(error);
        }
        self.requests.borrow_mut().push(Request { owner: owner.into(), cancel: cancellable.clone(), done: Some(done) });
        Ok(())
    }
}

struct Fixture {
    backend: InhibitorBackend,
    connection: Rc<Recorder>,
    changes: Rc<Cell<u32>>,
}

fn fixture() -> Fixture {
    let changes = Rc::new(Cell::new(0));
    let counter = changes.clone();
    let backend = InhibitorBackend::new(Rc::new(move || counter.set(counter.get() + 1)));
    let connection = Rc::new(Recorder::default());
    backend.attach(connection.clone(), ":1.42", true);
    Fixture { backend, connection, changes }
}

impl Fixture {
    /// Answer request `index` (negative = from the end) with a pipe writer, or `error`.
    /// Returns the reader: EOF once the backend no longer holds its copy.
    fn reply(&self, index: isize, error: Option<&str>) -> Option<OwnedFd> {
        let count = self.connection.requests.borrow().len() as isize;
        let index = if index < 0 { count + index } else { index } as usize;
        let done = self.connection.requests.borrow_mut()[index].done.take().expect("request already answered");
        match error {
            Some(error) => {
                done(Err(error.into()));
                None
            }
            None => {
                let (reader, writer) = pipe();
                done(Ok(writer));
                Some(reader)
            }
        }
    }
    fn count(&self) -> usize { self.connection.requests.borrow().len() }
}

fn fd_is_held_only_after_reply_and_closed_on_disable() {
    let f = fixture();
    f.backend.set_enabled(true);
    assert!(f.backend.pending());
    assert!(!f.backend.active());
    assert_eq!(f.connection.requests.borrow()[0].owner, ":1.42");
    let reader = f.reply(-1, None).unwrap();
    assert!(f.backend.active());
    assert!(!f.backend.pending());
    assert!(is_cloexec(f.backend.descriptor().unwrap()));
    assert_eq!(read(&reader), Read::WouldBlock);
    f.backend.set_enabled(false);
    assert!(!f.backend.active());
    assert_eq!(read(&reader), Read::Eof);
    assert!(f.changes.get() >= 3);
    f.backend.close();
}

fn disabling_pending_request_cannot_reacquire_from_late_reply() {
    let f = fixture();
    f.backend.set_enabled(true);
    let cancel = f.connection.requests.borrow()[0].cancel.clone();
    f.backend.set_enabled(false);
    assert!(cancel.is_cancelled());
    let reader = f.reply(-1, None).unwrap();
    assert_eq!(read(&reader), Read::Eof, "late descriptor must be closed");
    assert!(!f.backend.active());
    assert!(!f.backend.requested());
    f.backend.close();
}

fn rapid_toggle_uses_only_latest_request() {
    let f = fixture();
    f.backend.toggle();
    f.backend.toggle();
    f.backend.toggle();
    assert_eq!(f.count(), 2);
    let stale = f.reply(0, None).unwrap();
    assert_eq!(read(&stale), Read::Eof);
    assert!(f.backend.pending());
    let latest = f.reply(1, None).unwrap();
    assert!(f.backend.active());
    assert!(f.backend.requested());
    assert_eq!(read(&latest), Read::WouldBlock);
    f.backend.close();
    assert_eq!(read(&latest), Read::Eof);
}

fn service_loss_closes_fd_and_reacquires_only_requested_state() {
    let f = fixture();
    f.backend.set_enabled(true);
    let reader = f.reply(-1, None).unwrap();
    f.backend.vanished();
    assert!(!f.backend.ready());
    assert!(!f.backend.active());
    assert_eq!(f.backend.error(), UNAVAILABLE);
    assert_eq!(read(&reader), Read::Eof);
    f.backend.appeared(":1.99");
    assert!(f.backend.pending());
    assert_eq!(f.connection.requests.borrow().last().unwrap().owner, ":1.99");
    let second = f.reply(-1, None).unwrap();
    assert!(f.backend.active());
    // An owner change drops the old handle and re-acquires from the new owner.
    f.backend.appeared(":1.77");
    assert_eq!(read(&second), Read::Eof);
    assert!(f.backend.pending());
    f.reply(-1, None).unwrap();
    f.backend.vanished();
    f.backend.set_enabled(false);
    let count = f.count();
    f.backend.appeared(":1.100");
    assert_eq!(f.count(), count);
    f.backend.close();
}

fn denied_enable_resets_toggle_and_surfaces_error() {
    let f = fixture();
    f.backend.set_enabled(true);
    f.reply(-1, Some("g-dbus-error-quark: GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: Not permitted (9)"));
    assert!(!f.backend.active());
    assert!(!f.backend.requested());
    assert!(!f.backend.pending());
    assert!(f.backend.error().contains("Not permitted"));
    assert!(f.backend.error().starts_with("Could not keep the desktop awake: "));
    f.backend.set_enabled(true);
    f.reply(-1, None).unwrap();
    assert!(f.backend.active());
    assert_eq!(f.backend.error(), "");
    f.backend.close();
}

fn starting_before_logind_is_ready_queues_intent() {
    let f = fixture();
    f.backend.attach(f.connection.clone(), ":1.42", false);
    f.backend.set_enabled(true);
    assert_eq!(f.count(), 0);
    f.backend.appeared(":1.42");
    f.reply(-1, None).unwrap();
    assert!(f.backend.active());
    f.backend.close();
}

fn immediate_bus_failure_does_not_leave_toggle_pending() {
    let f = fixture();
    f.connection.fail.replace(Some(
        "g-dbus-error-quark: GDBus.Error:org.freedesktop.DBus.Error.Disconnected: Bus disconnected (0)".into()));
    f.backend.set_enabled(true);
    assert!(!f.backend.pending());
    assert!(!f.backend.requested());
    assert!(!f.backend.active());
    assert!(f.backend.error().contains("disconnected"));
    f.backend.close();
}

fn shutdown_releases_fd_and_late_reply_without_persisting_state() {
    let f = fixture();
    f.backend.set_enabled(true);
    let reader = f.reply(-1, None).unwrap();
    // A second request still in flight when the service stops.
    f.backend.vanished();
    f.backend.appeared(":1.43");
    assert_eq!(read(&reader), Read::Eof);
    f.backend.close();
    let late = f.reply(-1, None).unwrap();
    assert_eq!(read(&late), Read::Eof);
    assert!(!f.backend.active());
    assert!(!f.backend.requested());
    assert!(!f.backend.set_enabled(true), "a closed backend ignores requests");
    let replacement = InhibitorBackend::new(Rc::new(|| {}));
    assert!(!replacement.requested());
    assert!(!replacement.active());
    replacement.close();
}

fn real_unix_fd_transfer_on_private_bus() {
    let bus = PrivateBus::up();
    let login = FakeLogin::new(&bus.address);
    let backend = InhibitorBackend::with_bus(Rc::new(|| {}), BusSource::Address(bus.address.clone()));
    backend.start();
    assert!(wait_for(|| backend.ready(), 3.0), "backend never saw the fake logind");
    assert_eq!(backend.owner().as_deref(), Some(login.unique_name().as_str()));
    backend.set_enabled(true);
    assert!(wait_for(|| backend.active(), 3.0), "{}", backend.error());
    let call = login.calls.borrow()[0].clone();
    assert_eq!((call.0.as_str(), call.1.as_str(), call.2.as_str(), call.3.as_str()), INHIBIT_ARGS);
    assert!(is_cloexec(backend.descriptor().unwrap()));
    assert_eq!(login.reader(0), Read::WouldBlock);
    backend.set_enabled(false);
    assert!(wait_for(|| login.reader(0) == Read::Eof, 3.0), "an inhibitor descriptor remained open after Disable");

    // Denied by logind: the toggle resets and the PyGObject-style error is shown.
    login.deny.set(true);
    backend.set_enabled(true);
    assert!(wait_for(|| !backend.pending(), 3.0));
    assert!(!backend.requested());
    assert_eq!(backend.error(),
        "Could not keep the desktop awake: g-dbus-error-quark: GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: Not permitted (9)");
    login.deny.set(false);

    // logind goes away: the FD closes; a new owner re-acquires while requested.
    backend.set_enabled(true);
    assert!(wait_for(|| backend.active(), 3.0));
    login.disown();
    assert!(wait_for(|| !backend.ready(), 3.0));
    assert_eq!(backend.error(), UNAVAILABLE);
    assert!(wait_for(|| login.reader(1) == Read::Eof, 3.0));
    let replacement = FakeLogin::new(&bus.address);
    assert!(wait_for(|| backend.active(), 3.0), "did not re-acquire from the new logind");
    assert_eq!(backend.owner().as_deref(), Some(replacement.unique_name().as_str()));
    assert_eq!(replacement.calls.borrow().len(), 1);
    backend.close();
    assert!(wait_for(|| replacement.reader(0) == Read::Eof, 3.0), "close() must release the handle");
    drop(replacement);
    drop(login);
}

#[test]
fn inhibitor() {
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    fd_is_held_only_after_reply_and_closed_on_disable();
    disabling_pending_request_cannot_reacquire_from_late_reply();
    rapid_toggle_uses_only_latest_request();
    service_loss_closes_fd_and_reacquires_only_requested_state();
    denied_enable_resets_toggle_and_surfaces_error();
    starting_before_logind_is_ready_queues_intent();
    immediate_bus_failure_does_not_leave_toggle_pending();
    shutdown_releases_fd_and_late_reply_without_persisting_state();
    real_unix_fd_transfer_on_private_bus();
}
