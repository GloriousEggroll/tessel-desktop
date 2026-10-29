//! test_session.py `EndSessionTests`: the window helper and logind are fakes;
//! close requests only update a map.

use crate::bus::{BusCall, ReplyFn, SystemBus};
use crate::end::*;
use crate::windows::{Changed, ErrorFn, Window, WindowFactory, WindowSource};
use glib::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use suite_core::timer;

struct FakeWindows {
    changed: Changed,
    ready: Cell<bool>,
    windows: RefCell<Vec<Window>>,
    initial: Vec<String>,
    closes: bool,
    closed_requests: RefCell<Vec<String>>,
    stopped: Cell<bool>,
}

impl FakeWindows {
    fn remove(&self, id: &str) {
        self.windows.borrow_mut().retain(|w| w.id != id);
    }
}

impl WindowSource for Rc<FakeWindows> {
    fn start(&self) {
        let this = self.clone();
        timer::once(0, move || {
            this.windows.replace(this.initial.iter().map(|id| Window::new(id, "app", &format!("Doc {id}"))).collect());
            this.ready.set(true);
            (this.changed)();
        });
    }
    fn ready(&self) -> bool { self.ready.get() }
    fn windows(&self) -> Vec<Window> { self.windows.borrow().clone() }
    fn close_window(&self, id: &str) -> bool {
        self.closed_requests.borrow_mut().push(id.into());
        if self.closes {
            let this = self.clone();
            let id = id.to_owned();
            timer::once(20, move || {
                this.remove(&id);
                (this.changed)();
            });
        }
        true
    }
    fn close(&self) { self.stopped.set(true) }
}

/// ListInhibitors answered on the next main-loop turn.
struct FakeBus(Vec<InhibitorRow>);

impl SystemBus for FakeBus {
    fn call(&self, call: BusCall, done: ReplyFn) {
        assert_eq!(call.method, "ListInhibitors");
        let reply = (self.0.clone(),).to_variant();
        timer::once(0, move || done(Ok(reply)));
    }
}

type Reports = Rc<RefCell<Vec<(Vec<Window>, Vec<Inhibitor>, String)>>>;

fn finished(reports: &Reports) -> Finished {
    let reports = reports.clone();
    Rc::new(move |windows, inhibitors, note| reports.borrow_mut().push((windows, inhibitors, note)))
}

fn factory(holder: &Rc<RefCell<Option<Rc<FakeWindows>>>>, windows: &[&str], closes: bool) -> WindowFactory {
    let holder = holder.clone();
    let initial: Vec<String> = windows.iter().map(|w| w.to_string()).collect();
    Rc::new(move |changed: Changed, _error: ErrorFn| {
        let fake = Rc::new(FakeWindows { changed, ready: Cell::new(false), windows: RefCell::default(),
            initial: initial.clone(), closes, closed_requests: RefCell::default(), stopped: Cell::new(false) });
        holder.replace(Some(fake.clone()));
        Rc::new(fake) as Rc<dyn WindowSource>
    })
}

fn on_context(test: impl FnOnce(&glib::MainContext)) {
    let context = glib::MainContext::new();
    context.with_thread_default(|| test(&context)).unwrap();
}

fn spin(context: &glib::MainContext, seconds: f64) {
    timer::settle(context, Duration::from_secs_f64(seconds));
}

struct Run {
    ending: EndSession,
    backend: Rc<FakeWindows>,
    reports: Reports,
}

fn run_end(context: &glib::MainContext, windows: &[&str], closes: bool, rows: Vec<InhibitorRow>, wait: f64) -> Run {
    let reports: Reports = Rc::default();
    let holder = Rc::new(RefCell::new(None));
    let ending = EndSession::new("logout", finished(&reports), Some(Rc::new(FakeBus(rows))),
                                 Some(factory(&holder, windows, closes)), 300, 500);
    ending.start();
    spin(context, wait);
    let backend = holder.borrow().clone().unwrap();
    Run { ending, backend, reports }
}

#[test]
fn every_window_gets_one_close_request_and_logout_proceeds() {
    on_context(|context| {
        let run = run_end(context, &["1:1", "1:2"], true, vec![], 0.6);
        let mut requests = run.backend.closed_requests.borrow().clone();
        requests.sort();
        assert_eq!(requests, ["1:1", "1:2"]);
        assert_eq!(*run.reports.borrow(), vec![(vec![], vec![], String::new())]);
        assert!(!run.ending.blocking());
    });
}

#[test]
fn windows_that_stay_open_are_reported_after_timeout() {
    on_context(|context| {
        let run = run_end(context, &["1:1", "1:2"], false, vec![], 1.0);
        let (windows, _, _) = run.reports.borrow()[0].clone();
        assert_eq!(windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), ["1:1", "1:2"]);
        assert!(run.ending.blocking());
        // A window closing later updates the list without a second close request.
        let updates = Rc::new(RefCell::new(Vec::new()));
        let (u, e) = (updates.clone(), run.ending.clone());
        run.ending.set_on_update(Some(Rc::new(move || u.borrow_mut().push(e.remaining().len()))));
        run.backend.remove("1:1");
        (run.backend.changed)();
        assert_eq!(*updates.borrow(), [1]);
        assert_eq!(run.backend.closed_requests.borrow().len(), 2);
    });
}

#[test]
fn no_windows_and_unavailable_helper() {
    on_context(|context| {
        let run = run_end(context, &[], true, vec![], 0.5);
        assert_eq!(*run.reports.borrow(), vec![(vec![], vec![], String::new())]);
        let reports: Reports = Rc::default();
        let ending = EndSession::new("restart", finished(&reports), Some(Rc::new(FakeBus(vec![]))), None,
                                     CLOSE_TIMEOUT, READY_TIMEOUT);
        ending.drop_backend();
        ending.start();
        spin(context, 0.2);
        assert_eq!(reports.borrow().last().unwrap().2, "Open apps could not be checked.");
        assert!(ending.blocking());
    });
}

#[test]
fn cancel_stops_without_reporting() {
    on_context(|context| {
        let reports: Reports = Rc::default();
        let holder = Rc::new(RefCell::new(None));
        let ending = EndSession::new("logout", finished(&reports), Some(Rc::new(FakeBus(vec![]))),
                                     Some(factory(&holder, &["1:1", "1:2"], false)), 200, READY_TIMEOUT);
        ending.start();
        spin(context, 0.25);
        ending.close();
        spin(context, 0.4);
        assert!(reports.borrow().is_empty());
        assert!(holder.borrow().as_ref().unwrap().stopped.get());
    });
}

#[test]
fn helper_that_never_becomes_ready_is_a_note() {
    on_context(|context| {
        let reports: Reports = Rc::default();
        struct Silent;
        impl WindowSource for Silent {
            fn start(&self) {}
            fn ready(&self) -> bool { false }
            fn windows(&self) -> Vec<Window> { vec![] }
            fn close_window(&self, _: &str) -> bool { false }
            fn close(&self) {}
        }
        let errors: Rc<RefCell<Option<ErrorFn>>> = Rc::default();
        let keep = errors.clone();
        let factory: WindowFactory = Rc::new(move |_, error| { keep.replace(Some(error)); Rc::new(Silent) as Rc<dyn WindowSource> });
        let ending = EndSession::new("logout", finished(&reports), Some(Rc::new(FakeBus(vec![]))), Some(factory), 300, 150);
        ending.start();
        spin(context, 0.3);
        assert_eq!(reports.borrow()[0].2, NOTE_UNCHECKED);
        // A helper error before "ready" reports the same note immediately.
        let reports2: Reports = Rc::default();
        let errors2: Rc<RefCell<Option<ErrorFn>>> = Rc::default();
        let keep = errors2.clone();
        let factory: WindowFactory = Rc::new(move |_, error| { keep.replace(Some(error)); Rc::new(Silent) as Rc<dyn WindowSource> });
        let ending = EndSession::new("logout", finished(&reports2), Some(Rc::new(FakeBus(vec![]))), Some(factory), 300, 5000);
        ending.start();
        (errors2.borrow().clone().unwrap())("helper failed".into());
        spin(context, 0.05);
        assert_eq!(reports2.borrow()[0].2, NOTE_UNCHECKED);
        drop(errors);
    });
}

#[test]
fn inhibitors_filtered_by_action() {
    let uid = unsafe { libc::getuid() };
    let row = |what: &str, who: &str, why: &str, mode: &str, owner: u32, pid: u32| -> InhibitorRow {
        (what.into(), who.into(), why.into(), mode.into(), owner, pid)
    };
    let rows = vec![row("shutdown:sleep", "Builder", "Saving project", "block", uid, 10),
                    row("sleep", "Player", "Playing", "block", uid, 11),
                    row("shutdown", "Updater", "Installing updates", "delay", uid.wrapping_add(1), 12),
                    row("idle", "Video", "Watching", "block", uid, 13)];
    let who = |items: Vec<Inhibitor>| items.into_iter().map(|i| i.who).collect::<Vec<_>>();
    assert_eq!(who(parse_inhibitors(&rows, "logout", Some(uid))), ["Builder"]);
    assert_eq!(who(parse_inhibitors(&rows, "restart", Some(uid))), ["Builder", "Updater"]);
    assert_eq!(parse_inhibitors(&rows, "restart", Some(uid))[1],
               Inhibitor { who: "Updater".into(), why: "Installing updates".into(), mode: "delay".into(), pid: 12 });
    on_context(|context| {
        let run = run_end(context, &["1:1", "1:2"], true, rows.clone(), 0.6);
        let whys: Vec<String> = run.reports.borrow()[0].1.iter().map(|i| i.why.clone()).collect();
        assert_eq!(whys, ["Saving project"]);
        // A delay-only inhibitor does not block; a block one does.
        assert!(run.ending.blocking());
    });
}
