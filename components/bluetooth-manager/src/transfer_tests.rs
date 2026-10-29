//! Port of test_transfer.py: OBEX sender/receiver lifecycles on a fake
//! connection (no radio, no bus). Each test runs on a private main context.

use super::*;
use crate::dbus::{props_reply, FakeConn};

const SESSION_PATH: &str = "/org/bluez/obex/client/session1";
const FIRST: &str = "/org/bluez/obex/client/session1/transfer1";
const SECOND: &str = "/org/bluez/obex/client/session1/transfer2";
const SOURCE: &str = "12:34:56:78:9A:BC";
const DESTINATION: &str = "AB:CD:EF:12:34:56";

enum T {
    U(u64),
    S(&'static str),
}

fn variants(props: &[(&'static str, T)]) -> Vec<(&'static str, Variant)> {
    props.iter().map(|(k, v)| (*k, match v { T::U(n) => n.to_variant(), T::S(s) => s.to_variant() })).collect()
}

fn on_context(test: impl FnOnce(&glib::MainContext)) {
    let context = glib::MainContext::new();
    context.with_thread_default(|| test(&context)).unwrap();
}

fn drain(context: &glib::MainContext) {
    while context.pending() {
        context.iteration(false);
    }
}

type Results = Rc<RefCell<Vec<(Option<BtError>, bool)>>>;

struct Fx {
    dir: PathBuf,
    files: Vec<String>,
    conn: Rc<FakeConn>,
    progress: Rc<RefCell<Vec<Progress>>>,
    results: Results,
    sender: RefCell<Option<ObexSender>>,
}

fn temp_dir(prefix: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("{prefix}{}-{}", std::process::id(), suite_core::secrets::token_urlsafe(6)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl Fx {
    fn new() -> Self {
        let dir = temp_dir("btmanager-test-");
        let mut files = Vec::new();
        for (name, data) in [("one.txt", "hello"), ("two.txt", "bye")] {
            let path = dir.join(name);
            std::fs::write(&path, data).unwrap();
            files.push(path.to_string_lossy().into_owned());
        }
        Self { dir, files, conn: FakeConn::new(), progress: Rc::default(), results: Rc::default(), sender: RefCell::new(None) }
    }

    fn make(&self, source: &str, files: &[String], connection: Option<Rc<dyn Conn>>, connector: Option<Connector>) -> ObexSender {
        let progress = self.progress.clone();
        let results = self.results.clone();
        let sender = ObexSender::new(source, DESTINATION, files, Some(Rc::new(move |p: &Progress| progress.borrow_mut().push(p.clone()))),
                                     Some(Box::new(move |e, c| results.borrow_mut().push((e, c)))), connection, connector);
        self.sender.replace(Some(sender.clone()));
        sender
    }

    fn start(&self, ctx: &glib::MainContext, files: Option<Vec<String>>) -> ObexSender {
        let files = files.unwrap_or_else(|| self.files[..1].to_vec());
        let sender = self.make(SOURCE, &files, Some(self.conn.clone()), None);
        sender.start();
        drain(ctx);
        sender
    }

    fn sender(&self) -> ObexSender {
        self.sender.borrow().clone().unwrap()
    }

    fn session_ready(&self) {
        self.conn.reply("CreateSession", Ok(Variant::tuple_from_iter([crate::dbus::object_path(SESSION_PATH)])));
    }

    fn transfer_ready(&self, path: &str, props: &[(&'static str, T)]) {
        self.conn.reply("SendFile", Ok(Variant::tuple_from_iter([crate::dbus::object_path(path), vardict(&variants(props))])));
    }

    fn ready(&self, ctx: &glib::MainContext) {
        self.start(ctx, None);
        self.session_ready();
        self.transfer_ready(FIRST, &[("Status", T::S("queued")), ("Size", T::U(5))]);
    }

    fn properties(&self, path: &str, props: &[(&'static str, T)]) {
        self.conn.emit(OBEX, PROPERTIES, "PropertiesChanged", path,
                       Variant::tuple_from_iter([TRANSFER.to_variant(), vardict(&variants(props)), Vec::<String>::new().to_variant()]));
    }

    fn removed(&self, path: &str) {
        self.conn.emit(OBEX, OBJECT_MANAGER, "InterfacesRemoved", ROOT,
                       Variant::tuple_from_iter([crate::dbus::object_path(path), vec![TRANSFER].to_variant()]));
    }

    fn assert_clean(&self) {
        assert!(self.sender().done());
        assert_eq!(self.conn.subscription_count(), 0);
        assert_eq!(self.results.borrow().len(), 1);
    }

    fn result(&self) -> (Option<BtError>, bool) {
        self.results.borrow()[0].clone()
    }

    fn count(&self, method: &str) -> usize {
        self.conn.count(method)
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn explicit_source_adapter_and_destination() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None);
        let call = f.conn.calls.borrow()[0].clone();
        assert_eq!((call.destination.as_str(), call.path.as_str(), call.interface.as_str(), call.method.as_str()),
                   (OBEX, ROOT, CLIENT, "CreateSession"));
        let params = call.params.unwrap();
        assert_eq!(params.child_value(0).str(), Some(DESTINATION));
        let args = crate::backend::props_of(&params.child_value(1));
        assert_eq!(args.get("Target").and_then(|v| v.as_str()), Some("opp"));
        assert_eq!(args.get("Source").and_then(|v| v.as_str()), Some(SOURCE));
        assert_eq!(call.timeout, 60000);
        f.session_ready();
        let send = f.conn.calls.borrow()[1].clone();
        assert_eq!((send.path.as_str(), send.interface.as_str(), send.method.as_str()), (SESSION_PATH, PUSH, "SendFile"));
        assert_eq!(send.arg_str(0).as_deref(), Some(f.files[0].as_str()));
    });
}

#[test]
fn sequence_reports_progress_and_cleans_once() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, Some(f.files.clone()));
        f.session_ready();
        f.transfer_ready(FIRST, &[("Status", T::S("active")), ("Size", T::U(5))]);
        f.properties(FIRST, &[("Transferred", T::U(3))]);
        assert_eq!(f.progress.borrow().last().unwrap().transferred, 3);
        assert_eq!(f.progress.borrow().last().unwrap().total, 8);
        f.properties(FIRST, &[("Status", T::S("complete")), ("Transferred", T::U(5))]);
        assert_eq!(f.conn.last_call().arg_str(0).as_deref(), Some(f.files[1].as_str()));
        f.transfer_ready(SECOND, &[("Status", T::S("active")), ("Size", T::U(3))]);
        assert_eq!(f.progress.borrow().last().unwrap().index, 2);
        assert_eq!(f.progress.borrow().last().unwrap().completed, 5);
        f.properties(SECOND, &[("Status", T::S("complete")), ("Transferred", T::U(3))]);
        let last = f.conn.last_call();
        assert_eq!((last.method.as_str(), last.arg_str(0).as_deref()), ("RemoveSession", Some(SESSION_PATH)));
        assert!(f.results.borrow().is_empty());
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        assert_eq!(f.result(), (None, false));
    });
}

#[test]
fn fast_completion_before_send_reply_is_not_lost() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None);
        f.session_ready();
        f.properties(FIRST, &[("Status", T::S("complete")), ("Transferred", T::U(5))]);
        f.removed(FIRST);
        f.transfer_ready(FIRST, &[("Status", T::S("queued"))]);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        assert_eq!(f.result(), (None, false));
    });
}

#[test]
fn pre_reply_progress_overrides_older_snapshot() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None);
        f.session_ready();
        f.properties(FIRST, &[("Status", T::S("active")), ("Transferred", T::U(4))]);
        f.transfer_ready(FIRST, &[("Status", T::S("queued")), ("Transferred", T::U(0))]);
        let last = f.progress.borrow().last().cloned().unwrap();
        assert_eq!((last.transferred, last.status.as_str()), (4, "active"));
    });
}

#[test]
fn old_file_signals_cannot_finish_next_file() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, Some(f.files.clone()));
        f.session_ready();
        f.transfer_ready(FIRST, &[("Status", T::S("active"))]);
        f.properties(FIRST, &[("Status", T::S("complete"))]);
        f.properties(FIRST, &[("Status", T::S("complete"))]);
        f.removed(FIRST);
        f.transfer_ready(SECOND, &[("Status", T::S("active"))]);
        assert_eq!(f.progress.borrow().last().unwrap().index, 2);
        assert_eq!(f.count("RemoveSession"), 0);
    });
}

#[test]
fn other_session_properties_are_ignored() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.properties("/org/bluez/obex/client/session10/transfer1", &[("Status", T::S("complete"))]);
        assert_eq!(f.sender().index(), 0);
    });
}

#[test]
fn cancel_before_idle_never_opens_bus() {
    on_context(|ctx| {
        let f = Fx::new();
        let opened = Rc::new(Cell::new(0));
        let o = opened.clone();
        let connector: Connector = Rc::new(move |_done| o.set(o.get() + 1));
        let sender = f.make(SOURCE, &f.files, None, Some(connector));
        sender.start();
        sender.cancel();
        drain(ctx);
        assert_eq!(opened.get(), 0);
        f.assert_clean();
        assert_eq!(f.result(), (None, true));
    });
}

#[test]
fn cancel_create_waits_for_late_session_and_removes_it() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None).cancel();
        assert!(f.results.borrow().is_empty());
        f.session_ready();
        assert_eq!(f.count("SendFile"), 0);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        assert_eq!(f.result(), (None, true));
    });
}

#[test]
fn cancel_pending_send_aborts_session_and_ignores_late_reply() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None);
        f.session_ready();
        f.sender().cancel();
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.transfer_ready(FIRST, &[("Status", T::S("active"))]);
        f.assert_clean();
        assert_eq!(f.result(), (None, true));
    });
}

#[test]
fn cancel_active_transfer_idempotently_removes_session() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.sender().cancel();
        f.sender().cancel();
        f.properties(FIRST, &[("Status", T::S("complete"))]);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.sender().cancel();
        f.assert_clean();
        assert_eq!(f.result(), (None, true));
        assert_eq!(f.count("RemoveSession"), 1);
    });
}

#[test]
fn remote_transfer_failure_aborts_remainder() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, Some(f.files.clone()));
        f.session_ready();
        f.transfer_ready(FIRST, &[("Status", T::S("error"))]);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        let (error, cancelled) = f.result();
        assert!(error.is_some_and(|e| !e.glib) && !cancelled);
        assert_eq!(f.count("SendFile"), 1);
    });
}

#[test]
fn missing_service_keeps_original_error_and_setup_hint() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None);
        let error = BtError::dbus(gio::DBusError::ServiceUnknown, "No obex daemon");
        f.conn.reply("CreateSession", Err(error.clone()));
        f.assert_clean();
        assert_eq!(f.result().0, Some(error.clone()));
        assert!(is_unavailable_error(&error));
        assert!(is_unavailable_error(&BtError::remote("org.freedesktop.DBus.Error.ServiceUnknown", "x")));
    });
}

#[test]
fn create_failure_after_cancel_reports_cancellation() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None).cancel();
        f.conn.reply("CreateSession", Err(BtError::dbus(gio::DBusError::TimedOut, "Timed out")));
        f.assert_clean();
        assert_eq!(f.result(), (None, true));
    });
}

#[test]
fn send_call_error_cleans_session_and_preserves_error() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None);
        f.session_ready();
        let error = BtError::dbus(gio::DBusError::Failed, "Failed");
        f.conn.reply("SendFile", Err(error.clone()));
        assert!(f.results.borrow().is_empty());
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        assert_eq!(f.result().0, Some(error.clone()));
        assert!(!is_unavailable_error(&error));
    });
}

#[test]
fn cleanup_failure_is_not_reported_as_success() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.properties(FIRST, &[("Status", T::S("complete"))]);
        let error = BtError::dbus(gio::DBusError::Failed, "Cleanup failed");
        f.conn.reply("RemoveSession", Err(error.clone()));
        f.assert_clean();
        assert_eq!(f.result().0, Some(error));
    });
}

#[test]
fn transfer_disappearing_is_an_error() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.removed(FIRST);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        assert!(f.result().0.is_some());
    });
}

#[test]
fn transfer_disappearing_before_reply_is_an_error() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, None);
        f.session_ready();
        f.removed(FIRST);
        f.transfer_ready(FIRST, &[("Status", T::S("queued"))]);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        assert_eq!(f.result().0.map(|e| e.message), Some("The file transfer disappeared".into()));
    });
}

#[test]
fn session_vanishing_is_an_error() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.conn.emit(OBEX, OBJECT_MANAGER, "InterfacesRemoved", ROOT,
                    Variant::tuple_from_iter([crate::dbus::object_path(SESSION_PATH), vec![SESSION].to_variant()]));
        f.assert_clean();
        assert_eq!(f.count("RemoveSession"), 0);
        assert_eq!(f.result().0.map(|e| e.message), Some("The Bluetooth file transfer session closed".into()));
    });
}

#[test]
fn service_shutdown_releases_watchers_and_fails() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.conn.emit(DBUS, DBUS, "NameOwnerChanged", DBUS_PATH, (OBEX, ":1.12", "").to_variant());
        f.assert_clean();
        assert!(f.result().0.is_some());
        assert_eq!(f.count("RemoveSession"), 0);
    });
}

#[test]
fn invalidated_status_is_refreshed() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.conn.emit(OBEX, PROPERTIES, "PropertiesChanged", FIRST,
                    Variant::tuple_from_iter([TRANSFER.to_variant(), vardict(&[]), vec!["Status"].to_variant()]));
        f.conn.reply("GetAll", Ok(props_reply(&variants(&[("Status", T::S("complete"))]))));
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
        assert_eq!(f.result(), (None, false));
    });
}

#[test]
fn unreadable_or_missing_file_never_creates_session() {
    on_context(|ctx| {
        let f = Fx::new();
        let missing = f.dir.join("missing").to_string_lossy().into_owned();
        f.start(ctx, Some(vec![f.files[0].clone(), missing.clone()]));
        f.assert_clean();
        assert!(f.conn.calls.borrow().is_empty());
        assert_eq!(f.result().0.map(|e| e.message), Some(format!("Not a regular file: {missing}")));
    });
}

#[test]
fn empty_selection_never_creates_session() {
    on_context(|ctx| {
        let f = Fx::new();
        f.start(ctx, Some(vec![]));
        f.assert_clean();
        assert!(f.conn.calls.borrow().is_empty());
    });
}

#[test]
fn missing_source_is_rejected_instead_of_defaulting_adapter() {
    on_context(|ctx| {
        let f = Fx::new();
        f.make("", &f.files, Some(f.conn.clone()), None).start();
        drain(ctx);
        f.assert_clean();
        assert!(f.conn.calls.borrow().is_empty());
    });
}

#[test]
fn bus_unavailable_preserves_error_and_has_setup_hint() {
    on_context(|ctx| {
        let f = Fx::new();
        let error = BtError::glib("No session bus").into_unavailable();
        let e = error.clone();
        let connector: Connector = Rc::new(move |done| done(Err(e.clone())));
        f.make(SOURCE, &f.files, None, Some(connector)).start();
        drain(ctx);
        f.assert_clean();
        assert_eq!(f.result().0, Some(error.clone()));
        assert!(is_unavailable_error(&error));
        assert!(!is_unavailable_error(&BtError::runtime("Failed")));
    });
}

#[test]
fn real_connector_marks_missing_session_bus_unavailable() {
    on_context(|ctx| {
        let f = Fx::new();
        let saved = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/btmanager-test-bus");
        f.make(SOURCE, &f.files, None, None).start();
        let ok = suite_core::timer::wait_for(ctx, std::time::Duration::from_secs(5), || !f.results.borrow().is_empty());
        match saved { Some(v) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", v), None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS") }
        assert!(ok);
        let error = f.result().0.unwrap();
        assert!(is_unavailable_error(&error), "{error:?}");
    });
}

#[test]
fn private_connection_closed_before_payload_released() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.sender().set_owns_connection(true);
        f.properties(FIRST, &[("Status", T::S("complete"))]);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        assert!(f.results.borrow().is_empty());
        assert_eq!(f.conn.subscription_count(), 0);
        assert_eq!(f.conn.close_requests.get(), 1);
        f.conn.finish_close();
        f.assert_clean();
    });
}

#[test]
fn private_connection_closed_even_if_session_removal_fails() {
    on_context(|ctx| {
        let f = Fx::new();
        f.ready(ctx);
        f.sender().set_owns_connection(true);
        f.sender().cancel();
        f.conn.reply("RemoveSession", Err(BtError::dbus(gio::DBusError::Failed, "Removal failed")));
        assert!(f.results.borrow().is_empty());
        assert_eq!(f.conn.close_requests.get(), 1);
        f.conn.finish_close();
        f.assert_clean();
        assert_eq!(f.result(), (None, true));
    });
}

#[test]
fn late_connection_after_cancellation_is_closed_without_sending() {
    on_context(|ctx| {
        let f = Fx::new();
        type Slot = Rc<RefCell<Option<Box<dyn FnOnce(Result<Rc<dyn Conn>, BtError>)>>>>;
        let slot: Slot = Rc::default();
        let s = slot.clone();
        let connector: Connector = Rc::new(move |done| { s.replace(Some(done)); });
        f.make(SOURCE, &f.files, None, Some(connector)).start();
        drain(ctx);
        f.sender().cancel();
        let late = FakeConn::new();
        (slot.take().unwrap())(Ok(late.clone()));
        f.assert_clean();
        assert_eq!(late.close_requests.get(), 1);
        assert!(late.calls.borrow().is_empty());
    });
}

#[test]
fn connection_closing_mid_transfer_fails() {
    on_context(|ctx| {
        let f = Fx::new();
        let conn = f.conn.clone();
        let connector: Connector = Rc::new(move |done| done(Ok(conn.clone())));
        f.make(SOURCE, &f.files[..1].to_vec(), None, Some(connector)).start();
        drain(ctx);
        f.session_ready();
        f.conn.fire_closed(None);
        f.assert_clean();
        assert_eq!(f.result().0.map(|e| e.message), Some("The session bus connection closed".into()));
    });
}

#[test]
fn cancel_from_progress_callback_does_not_send_file() {
    on_context(|ctx| {
        let f = Fx::new();
        let sender = f.start(ctx, None);
        let s = sender.clone();
        sender.set_on_progress(Some(Rc::new(move |_| s.cancel())));
        f.session_ready();
        assert_eq!(f.count("SendFile"), 0);
        f.conn.reply("RemoveSession", Ok(().to_variant()));
        f.assert_clean();
    });
}

#[test]
fn cancel_from_initial_progress_does_not_open_bus() {
    on_context(|ctx| {
        let f = Fx::new();
        let opened = Rc::new(Cell::new(0));
        let o = opened.clone();
        let connector: Connector = Rc::new(move |_| o.set(o.get() + 1));
        let sender = f.make(SOURCE, &f.files, None, Some(connector));
        let s = sender.clone();
        sender.set_on_progress(Some(Rc::new(move |_| s.cancel())));
        sender.start();
        drain(ctx);
        assert_eq!(opened.get(), 0);
        f.assert_clean();
    });
}

#[test]
fn start_twice_is_refused() {
    on_context(|ctx| {
        let f = Fx::new();
        let sender = f.start(ctx, None);
        assert_eq!(sender.start_checked(), Err("This transfer has already been started"));
    });
}

// ------------------------------------------------------------- receiving

const INCOMING: &str = "/org/bluez/obex/server/session3/transfer4";
const INCOMING_SESSION: &str = "/org/bluez/obex/server/session3";

type Events = Rc<RefCell<Vec<(String, String)>>>;

struct Rx {
    dir: PathBuf,
    conn: Rc<FakeConn>,
    events: Events,
    requests: Rc<RefCell<Vec<Rc<ReceiveRequest>>>>,
    receiver: ObexReceiver,
}

impl Rx {
    fn new() -> Self {
        let conn = FakeConn::new();
        let events: Events = Rc::default();
        let requests: Rc<RefCell<Vec<Rc<ReceiveRequest>>>> = Rc::default();
        let push = |kind: &'static str, e: &Events| {
            let e = e.clone();
            move |detail: String| e.borrow_mut().push((kind.into(), detail))
        };
        let (on_req, on_cancel, on_progress, on_done, on_status) =
            (push("request", &events), push("cancel", &events), push("progress", &events), push("done", &events), push("status", &events));
        let rq = requests.clone();
        let callbacks = ReceiverCallbacks {
            on_request: Some(Rc::new(move |r| { rq.borrow_mut().push(r.clone()); on_req(r.transfer.clone()) })),
            on_cancel: Some(Rc::new(move |r| on_cancel(r.state()))),
            on_progress: Some(Rc::new(move |r| on_progress(r.transferred.get().to_string()))),
            on_done: Some(Rc::new(move |r| on_done(r.state()))),
            on_status: Some(Rc::new(move |registered, _| on_status(registered.to_string()))),
        };
        let receiver = ObexReceiver::new(callbacks, Some(conn.clone()), None);
        Self { dir: temp_dir("btmanager-receive-"), conn, events, requests, receiver }
    }

    fn pop(&self) -> (String, String) {
        self.events.borrow_mut().pop().expect("an event")
    }

    fn register(&self) {
        self.receiver.start();
        let last = self.conn.last_call();
        assert_eq!((last.interface.as_str(), last.method.as_str(), last.arg_str(0).as_deref()),
                   (AGENT_MANAGER, "RegisterAgent", Some(AGENT_PATH)));
        self.conn.reply("RegisterAgent", Ok(().to_variant()));
        let lookup = self.conn.reply("GetNameOwner", Ok((":1.5",).to_variant()));
        assert_eq!((lookup.destination.as_str(), lookup.path.as_str()), (DBUS, DBUS_PATH));
        assert_eq!(self.pop(), ("status".into(), "true".into()));
    }

    fn invoke(&self, method: &str, params: Variant, sender: &str) -> Option<Result<Option<Variant>, (String, String)>> {
        self.conn.invoke(method, params, sender)
    }

    fn push(&self, name: &str, size: u64) -> (crate::dbus::InvocationSlot, Rc<ReceiveRequest>) {
        let slot = self.conn.invoke_slot("AuthorizePush", Variant::tuple_from_iter([crate::dbus::object_path(INCOMING)]), ":1.5");
        assert!(slot.borrow().is_none(), "AuthorizePush must wait for the user");
        self.conn.reply("GetAll", Ok(props_reply(&[("Name", name.to_variant()), ("Size", size.to_variant()),
                                                   ("Session", crate::dbus::object_path(INCOMING_SESSION))])));
        self.conn.reply("GetAll", Ok(props_reply(&[("Destination", DESTINATION.to_variant())])));
        assert_eq!(self.pop().0, "request");
        let request = self.requests.borrow().last().cloned().unwrap();
        (slot, request)
    }

    fn properties(&self, props: &[(&'static str, T)]) {
        self.conn.emit(OBEX, PROPERTIES, "PropertiesChanged", INCOMING,
                       Variant::tuple_from_iter([TRANSFER.to_variant(), vardict(&variants(props)), Vec::<String>::new().to_variant()]));
    }
}

impl Drop for Rx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn registers_agent_and_unregisters_on_stop() {
    let rx = Rx::new();
    rx.register();
    assert_eq!(rx.conn.objects.borrow().len(), 1);
    assert_eq!(rx.conn.objects.borrow()[0].1, AGENT_PATH);
    rx.receiver.stop();
    assert_eq!(rx.conn.last_call().method, "UnregisterAgent");
    assert!(rx.conn.objects.borrow().is_empty());
    assert_eq!(rx.conn.subscription_count(), 0);
}

#[test]
fn rejects_calls_from_other_bus_clients() {
    let rx = Rx::new();
    rx.register();
    let reply = rx.invoke("AuthorizePush", Variant::tuple_from_iter([crate::dbus::object_path(INCOMING)]), ":1.99");
    assert_eq!(reply.unwrap().unwrap_err().0, REJECTED);
    assert!(rx.events.borrow().is_empty());
}

#[test]
fn unknown_methods_are_refused() {
    let rx = Rx::new();
    rx.register();
    let reply = rx.invoke("Bogus", ().to_variant(), ":1.5").unwrap().unwrap_err();
    assert_eq!(reply, ("org.freedesktop.DBus.Error.UnknownMethod".into(), "Bogus".into()));
}

#[test]
fn accept_saves_to_unique_sanitized_path_and_reports_progress() {
    let rx = Rx::new();
    rx.register();
    std::fs::write(rx.dir.join("evil.txt"), "").unwrap();
    let (slot, request) = rx.push("../evil.txt", 10);
    assert_eq!((request.name.borrow().as_str(), request.size.get(), request.address.borrow().as_str()), ("evil.txt", 10, DESTINATION));
    let path = request.accept(&rx.dir).unwrap();
    assert_eq!(path, rx.dir.join("evil (1).txt"));
    let reply = slot.borrow().clone().unwrap().unwrap().unwrap();
    assert_eq!(reply.child_value(0).str(), Some(path.to_str().unwrap()));
    assert!(path.exists());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    rx.properties(&[("Transferred", T::U(4))]);
    assert_eq!(rx.pop(), ("progress".into(), "4".into()));
    rx.properties(&[("Status", T::S("complete"))]);
    assert_eq!(rx.pop(), ("done".into(), "complete".into()));
    assert_eq!(request.transferred.get(), 10);
    assert!(path.exists());
}

#[test]
fn failed_transfer_removes_partial_file() {
    let rx = Rx::new();
    rx.register();
    let (_slot, request) = rx.push("photo.jpg", 5);
    let path = request.accept(&rx.dir).unwrap();
    rx.properties(&[("Status", T::S("error"))]);
    assert_eq!(rx.pop(), ("done".into(), "failed".into()));
    assert!(!path.exists());
}

#[test]
fn decline_and_obex_cancel() {
    let rx = Rx::new();
    rx.register();
    let (slot, request) = rx.push("photo.jpg", 5);
    request.reject();
    assert_eq!(slot.borrow().clone().unwrap().unwrap_err().0, REJECTED);
    assert_eq!(request.state(), "rejected");
    assert!(request.accept(&rx.dir).is_none());
    assert!(rx.receiver.requests().is_empty());
    let (_slot, request) = rx.push("photo.jpg", 5);
    assert_eq!(rx.invoke("Cancel", ().to_variant(), ":1.5"), Some(Ok(None)));
    assert_eq!(rx.pop(), ("cancel".into(), "cancelled".into()));
    assert_eq!(request.state(), "cancelled");
}

#[test]
fn failed_transfer_lookup_rejects_and_unsaveable_accept_rejects() {
    let rx = Rx::new();
    rx.register();
    rx.invoke("AuthorizePush", Variant::tuple_from_iter([crate::dbus::object_path(INCOMING)]), ":1.5");
    rx.conn.reply("GetAll", Err(BtError::glib("gone")));
    assert!(rx.events.borrow().is_empty());
    assert!(rx.receiver.requests().is_empty());
    let (_slot, request) = rx.push("photo.jpg", 5);
    let blocked = rx.dir.join("file-not-dir");
    std::fs::write(&blocked, "").unwrap();
    assert!(request.accept(&blocked).is_none());
    assert_eq!(request.state(), "rejected");
    assert!(request.error.borrow().is_some());
}

#[test]
fn release_unregisters_and_reports_status() {
    let rx = Rx::new();
    rx.register();
    assert_eq!(rx.invoke("Release", ().to_variant(), ":1.5"), Some(Ok(None)));
    assert!(!rx.receiver.0.registered.get());
    assert_eq!(rx.pop(), ("status".into(), "false".into()));
}

#[test]
fn obexd_restart_fails_active_transfer_and_registers_again() {
    let rx = Rx::new();
    rx.register();
    let (_slot, request) = rx.push("photo.jpg", 5);
    request.accept(&rx.dir);
    rx.conn.emit(DBUS, DBUS, "NameOwnerChanged", DBUS_PATH, (OBEX, ":1.5", "").to_variant());
    assert!(rx.events.borrow().contains(&("done".into(), "failed".into())));
    rx.conn.emit(DBUS, DBUS, "NameOwnerChanged", DBUS_PATH, (OBEX, "", ":1.7").to_variant());
    assert_eq!(rx.conn.last_call().method, "RegisterAgent");
}

#[test]
fn stop_rejects_pending_requests_and_restart_race_unregisters() {
    let rx = Rx::new();
    rx.register();
    let (_slot, request) = rx.push("photo.jpg", 5);
    rx.receiver.stop();
    assert_eq!(request.state(), "rejected");
    // A RegisterAgent reply that arrives after stop() is undone.
    rx.receiver.start();
    rx.receiver.stop();
    rx.conn.reply("RegisterAgent", Ok(().to_variant()));
    assert_eq!(rx.conn.last_call().method, "UnregisterAgent");
}

#[test]
fn safe_filename_cases() {
    for (raw, expected) in [("../../x", "x"), ("", "file"), ("..", "file"), (".bashrc", "_bashrc"), ("a\x00b\nc", "abc"),
                            ("dir\\name.txt", "name.txt"), ("  spaced  ", "spaced")] {
        assert_eq!(safe_filename(raw), expected, "{raw:?}");
    }
    assert_eq!(safe_filename(&"x".repeat(300)).chars().count(), 200);
    assert_eq!(splitext("archive.tar.gz"), ("archive.tar", ".gz"));
    assert_eq!(splitext("_bashrc"), ("_bashrc", ""));
    assert_eq!(splitext("..a"), ("..a", ""));
    assert_eq!(splitext("a."), ("a", "."));
    assert_eq!(abspath("/a/b/../c/./d"), "/a/c/d");
}

// ------------------------------------------------------------- notifications

#[test]
fn notifier_sends_hints_and_routes_actions() {
    let conn = FakeConn::new();
    let notifier = Notifier::new(conn.clone(), "Bluetooth", None);
    assert!(notifier.supports_actions());
    conn.reply("GetCapabilities", Ok((vec!["body"],).to_variant()));
    assert!(!notifier.supports_actions());
    let actions: Rc<RefCell<Vec<String>>> = Rc::default();
    let shown = Rc::new(Cell::new(0u32));
    let (a, a2, s) = (actions.clone(), actions.clone(), shown.clone());
    let mut note = Notification::new("Incoming file", "body");
    note.actions = vec![("accept".into(), "Accept".into())];
    note.on_action = Some(Rc::new(move |k| a.borrow_mut().push(k.into())));
    note.on_closed = Some(Rc::new(move |r| a2.borrow_mut().push(format!("closed {r}"))));
    note.urgency = 2;
    note.timeout = 0;
    note.value = Some(50);
    note.on_shown = Some(Box::new(move |id| s.set(id)));
    notifier.notify(note);
    let call = conn.reply("Notify", Ok((7u32,).to_variant()));
    let params = call.params.unwrap();
    assert_eq!(params.type_().as_str(), "(susssasa{sv}i)");
    assert_eq!(params.child_value(0).str(), Some("Bluetooth"));
    assert_eq!(params.child_value(2).str(), Some("bluetooth-symbolic"));
    let hints = crate::backend::props_of(&params.child_value(6));
    assert_eq!(hints.get("urgency"), Some(&serde_json::json!(2)));
    assert_eq!(hints.get("desktop-entry"), Some(&serde_json::json!("btmanager")));
    assert_eq!(hints.get("value"), Some(&serde_json::json!(50)));
    assert_eq!(params.child_value(7).get::<i32>(), Some(0));
    assert_eq!(shown.get(), 7);
    conn.emit(NOTIFICATIONS, NOTIFICATIONS, "ActionInvoked", NOTIFICATIONS_PATH, (7u32, "accept").to_variant());
    conn.emit(NOTIFICATIONS, NOTIFICATIONS, "ActionInvoked", NOTIFICATIONS_PATH, (8u32, "accept").to_variant());
    conn.emit(NOTIFICATIONS, NOTIFICATIONS, "NotificationClosed", NOTIFICATIONS_PATH, (7u32, 2u32).to_variant());
    conn.emit(NOTIFICATIONS, NOTIFICATIONS, "ActionInvoked", NOTIFICATIONS_PATH, (7u32, "accept").to_variant());
    assert_eq!(*actions.borrow(), vec!["accept".to_owned(), "closed 2".to_owned()]);
    notifier.close(7);
    assert_eq!(conn.last_call().method, "CloseNotification");
    notifier.close(0);
    assert_eq!(conn.count("CloseNotification"), 1);
    notifier.destroy();
    assert_eq!(conn.subscription_count(), 0);
}

#[test]
fn notifier_reports_errors_and_unknown_capabilities_assume_actions() {
    let conn = FakeConn::new();
    let notifier = Notifier::new(conn.clone(), "Bluetooth", None);
    conn.reply("GetCapabilities", Err(BtError::glib("no server")));
    assert!(!notifier.supports_actions());
    let failed = Rc::new(Cell::new(false));
    let f = failed.clone();
    let mut note = Notification::new("a", "b");
    note.on_error = Some(Box::new(move |_| f.set(true)));
    notifier.notify(note);
    let call = conn.reply("Notify", Err(BtError::glib("x")));
    assert_eq!(call.params.unwrap().child_value(7).get::<i32>(), Some(-1));
    assert!(failed.get());
}
