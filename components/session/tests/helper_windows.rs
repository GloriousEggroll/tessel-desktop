//! HelperWindows (window_backend.py port) and EndSession against a fake
//! `tessel-dock-toplevel` shell script: JSON-lines events out, `close N` in.
//! No compositor and no real windows are involved.

use tessel_session::bus::{BusCall, ReplyFn, SystemBus};
use tessel_session::end::{EndSession, Ending, Inhibitor, NOTE_UNCHECKED};
use tessel_session::windows::{HelperWindows, Window, WindowFactory, WindowSource};
use glib::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tessel_core::timer;

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("session-manager-helper-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fake helper. `keep` = native ids that ignore close requests.
fn helper(dir: &PathBuf, events: &str, keep: &str) -> (Vec<String>, PathBuf) {
    let log = dir.join("stdin.log");
    let script = dir.join("tessel-dock-toplevel");
    std::fs::write(&script, format!(r#"#!/bin/sh
cat <<'EVENTS'
{events}
EVENTS
while read cmd id; do
  echo "$cmd $id" >> '{log}'
  case " {keep} " in *" $id "*) ;; *) echo "{{\"event\":\"closed\",\"id\":\"$id\"}}";; esac
done
"#, log = log.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (vec![script.to_string_lossy().into_owned()], log)
}

const TWO_WINDOWS: &str = r#"{"event":"output","id":7,"name":"HEADLESS-1","width":1920,"height":1080,"scale":1}
{"event":"window","id":"1","app_id":"org.gnome.TextEditor","title":"notes.txt","outputs":["HEADLESS-1","HEADLESS-1"],"activated":true}
{"event":"window","id":"2","app_id":"foot","title":"term"}
{"event":"ready","seat":true,"protocol_version":3}"#;

fn on_context(test: impl FnOnce(&glib::MainContext)) {
    let context = glib::MainContext::new();
    context.with_thread_default(|| test(&context)).unwrap();
}

#[test]
fn helper_events_windows_and_close_requests() {
    on_context(|context| {
        let dir = scratch("events");
        let (command, log) = helper(&dir, TWO_WINDOWS, "2");
        let changes = Rc::new(RefCell::new(0));
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let (c, e) = (changes.clone(), errors.clone());
        let backend = HelperWindows::new(command, Rc::new(move || *c.borrow_mut() += 1),
                                         Rc::new(move |m| e.borrow_mut().push(m)));
        backend.start();
        assert!(timer::wait_for(context, Duration::from_secs(5), || backend.ready()));
        timer::settle(context, Duration::from_millis(50));
        let windows = backend.windows();
        assert_eq!(windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), ["1:1", "1:2"]);
        assert_eq!(windows[0], Window { id: "1:1".into(), app_id: "org.gnome.TextEditor".into(), title: "notes.txt".into(),
            activated: true, outputs: vec!["HEADLESS-1".into()], ..Window::default() });
        assert!(backend.can_activate());
        assert_eq!(backend.protocol_version(), 3);
        assert_eq!(backend.outputs()["HEADLESS-1"].width, 1920);
        assert!(*changes.borrow() >= 1);
        assert!(backend.close_window("1:1"));
        assert!(backend.close_window("1:2"));
        assert!(!backend.close_window("9:9"));
        assert_eq!(errors.borrow().last().unwrap(), "That window is no longer available.");
        assert!(timer::wait_for(context, Duration::from_secs(5), || backend.windows().len() == 1));
        assert_eq!(backend.windows()[0].id, "1:2");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "close 1\nclose 2\n");
        backend.close();
        assert!(!backend.ready());
        assert!(backend.windows().is_empty());
    });
}

#[test]
fn invalid_helper_output_relaunches_with_new_ids() {
    on_context(|context| {
        let dir = scratch("invalid");
        let events = r#"{"event":"window","id":"1","app_id":"a","title":"t"}
{"event":"ready"}
not json"#;
        let (command, _) = helper(&dir, events, "");
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let e = errors.clone();
        let backend = HelperWindows::new(command, Rc::new(|| {}), Rc::new(move |m| e.borrow_mut().push(m)));
        backend.start();
        assert!(timer::wait_for(context, Duration::from_secs(5), || !errors.borrow().is_empty()));
        assert_eq!(errors.borrow()[0], "The window helper returned invalid window information.");
        assert!(!backend.ready());
        // Relaunched after 1 s as helper session 3 (ids "3:<n>"), which fails the same way.
        assert_eq!(backend.session(), 2);
        assert!(timer::wait_for(context, Duration::from_secs(5), || errors.borrow().len() >= 2));
        assert_eq!(backend.session(), 4);
        backend.close();
    });
}

#[test]
fn missing_helper_reports_start_failure() {
    on_context(|context| {
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let e = errors.clone();
        let backend = HelperWindows::new(vec!["/nonexistent/tessel-dock-toplevel".into()], Rc::new(|| {}),
                                         Rc::new(move |m| e.borrow_mut().push(m)));
        backend.start();
        timer::settle(context, Duration::from_millis(50));
        assert_eq!(errors.borrow()[0], "The dock's window helper could not start. Build or reinstall it.");
        backend.close();
    });
}

struct NoInhibitors;

impl SystemBus for NoInhibitors {
    fn call(&self, _call: BusCall, done: ReplyFn) {
        let reply = (Vec::<(String, String, String, String, u32, u32)>::new(),).to_variant();
        timer::once(0, move || done(Ok(reply)));
    }
}

type Reports = Rc<RefCell<Vec<(Vec<Window>, Vec<Inhibitor>, String)>>>;

fn end_with(command: Vec<String>, close_timeout: u32) -> (EndSession, Reports) {
    let reports: Reports = Rc::default();
    let r = reports.clone();
    let factory: WindowFactory = Rc::new(move |changed, error| {
        HelperWindows::new(command.clone(), changed, error) as Rc<dyn WindowSource>
    });
    let ending = EndSession::new("logout", Rc::new(move |w, i, n| r.borrow_mut().push((w, i, n))),
                                 Some(Rc::new(NoInhibitors)), Some(factory), close_timeout, 3000);
    ending.start();
    (ending, reports)
}

#[test]
fn end_session_through_the_helper() {
    on_context(|context| {
        // Every window closes: proceeds with nothing remaining.
        let dir = scratch("end-all");
        let (command, log) = helper(&dir, TWO_WINDOWS, "");
        let (ending, reports) = end_with(command, 2000);
        assert!(timer::wait_for(context, Duration::from_secs(5), || !reports.borrow().is_empty()));
        assert_eq!(reports.borrow()[0], (vec![], vec![], String::new()));
        assert!(!ending.blocking());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "close 1\nclose 2\n");
        ending.close();

        // A window that stays open (an unsaved-document prompt) is reported after the timeout.
        let dir = scratch("end-stuck");
        let (command, log) = helper(&dir, TWO_WINDOWS, "2");
        let (ending, reports) = end_with(command, 400);
        assert!(timer::wait_for(context, Duration::from_secs(5), || !reports.borrow().is_empty()));
        let (windows, _, note) = reports.borrow()[0].clone();
        assert_eq!(windows.iter().map(|w| w.title.as_str()).collect::<Vec<_>>(), ["term"]);
        assert_eq!(note, "");
        assert!(ending.blocking());
        timer::settle(context, Duration::from_millis(200));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "close 1\nclose 2\n", "one close request per window");
        ending.close();

        // No helper: the note.
        let (ending, reports) = end_with(vec!["/nonexistent/tessel-dock-toplevel".into()], 400);
        assert!(timer::wait_for(context, Duration::from_secs(5), || !reports.borrow().is_empty()));
        assert_eq!(reports.borrow()[0].2, NOTE_UNCHECKED);
        assert!(ending.blocking());
        ending.close();
    });
}
