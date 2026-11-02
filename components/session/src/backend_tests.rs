//! test_session.py `ActionTests`: commands and D-Bus are fakes; nothing can
//! lock, log out, suspend, restart or power off.

use crate::backend::*;
use crate::bus::{FakeSystemBus, SystemBus};
use glib::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct FakeRunner {
    calls: RefCell<Vec<(Vec<String>, Option<Done>)>>,
}

impl FakeRunner {
    fn argv(&self, index: usize) -> Vec<String> { self.calls.borrow()[index].0.clone() }
    fn count(&self) -> usize { self.calls.borrow().len() }
    fn complete(&self, index: usize, ok: bool, error: &str) {
        let done = self.calls.borrow_mut()[index].1.take().expect("pending command");
        done(ok, error.into());
    }
}

impl CommandRunner for Rc<FakeRunner> {
    fn run(&self, argv: &[&str], done: Done) {
        self.calls.borrow_mut().push((argv.iter().map(|a| a.to_string()).collect(), Some(done)));
    }
}

type Results = Rc<RefCell<Vec<(bool, String)>>>;

fn done(results: &Results) -> Done {
    let results = results.clone();
    Box::new(move |ok, error| results.borrow_mut().push((ok, error)))
}

struct Fixture {
    backend: SessionBackend,
    runner: Rc<FakeRunner>,
    bus: Rc<FakeSystemBus>,
    results: Results,
}

fn fixture(capabilities: Capabilities) -> Fixture {
    let runner = Rc::new(FakeRunner::default());
    let connector: Connector = Box::new(|_, _| {});
    let backend = SessionBackend::with(Rc::new(|| {}), connector, Rc::new(runner.clone()), Some(None));
    let bus = FakeSystemBus::new();
    backend.set_bus(Some(Rc::new(bus.clone()) as Rc<dyn SystemBus>));
    backend.set_capabilities(capabilities);
    Fixture { backend, runner, bus, results: Rc::default() }
}

const LOCK: [&str; 4] = ["systemctl", "--user", "start", "tessel-lock.service"];

#[test]
fn sleep_locks_first_and_waits_for_readiness() {
    let f = fixture(Capabilities::all());
    f.backend.perform("sleep", done(&f.results));
    assert_eq!(f.runner.argv(0), LOCK);
    assert!(f.bus.calls.borrow().is_empty());
    f.runner.complete(0, true, "");
    let call = f.bus.last().expect("Suspend");
    assert_eq!((call.interface.as_str(), call.method.as_str()), ("org.freedesktop.login1.Manager", "Suspend"));
    assert_eq!(call.params, Some(Variant::from((true,))));
    assert_eq!(call.flags, gio::DBusCallFlags::NONE);
    assert_eq!(call.timeout, 120000);
}

#[test]
fn failed_lock_cancels_sleep() {
    let f = fixture(Capabilities::all());
    f.backend.perform("sleep", done(&f.results));
    f.runner.complete(0, false, "fixture failure");
    assert!(f.bus.calls.borrow().is_empty());
    assert_eq!(f.results.borrow()[0],
               (false, "The screen could not be locked. Sleep was cancelled. fixture failure".to_owned()));
}

#[test]
fn lock_is_manual_and_does_not_check_idle_inhibitors() {
    let f = fixture(Capabilities::all());
    f.backend.perform("lock", done(&f.results));
    assert_eq!(f.runner.count(), 1);
    assert_eq!(f.runner.argv(0), LOCK);
    assert!(f.bus.calls.borrow().is_empty());
    f.runner.complete(0, true, "");
    assert_eq!(f.results.borrow()[0], (true, String::new()));
}

#[test]
fn switch_user_locks_before_greeter() {
    let f = fixture(Capabilities::all());
    f.backend.perform("switch_user", done(&f.results));
    assert!(!f.results.borrow()[0].0); // No display manager seat.
    assert_eq!(f.runner.count(), 0);
    f.backend.set_capabilities(Capabilities { switch_user: true, ..Capabilities::all() });
    f.backend.set_greeter(Some("/org/freedesktop/DisplayManager/Seat0"));
    f.backend.perform("switch_user", done(&f.results));
    assert_eq!(f.runner.argv(0), LOCK);
    assert!(f.bus.calls.borrow().is_empty());
    f.runner.complete(0, false, "no lock");
    assert!(f.bus.calls.borrow().is_empty());
    assert!(!f.results.borrow()[1].0);
    f.backend.perform("switch_user", done(&f.results));
    f.runner.complete(1, true, "");
    let call = f.bus.last().expect("greeter call");
    assert_eq!((call.path.as_str(), call.interface.as_str(), call.method.as_str()),
               ("/org/freedesktop/DisplayManager/Seat0", "org.freedesktop.DisplayManager.Seat", "SwitchToGreeter"));
    assert_eq!((call.flags, call.timeout), (gio::DBusCallFlags::NO_AUTO_START, 5000));
    f.bus.reply("SwitchToGreeter", Ok(Variant::from(())));
    assert_eq!(f.results.borrow()[2], (true, String::new()));
}

#[test]
fn restart_dispatches_reboot() {
    let f = fixture(Capabilities::all());
    f.backend.perform("restart", done(&f.results));
    assert_eq!(f.bus.methods(), ["Reboot"]);
    assert_eq!(f.runner.count(), 0);
}

#[test]
fn restart_checks_its_own_capability_and_uses_normal_authorization() {
    let f = fixture(Capabilities { poweroff: true, ..Capabilities::default() });
    f.backend.perform("restart", done(&f.results));
    assert!(f.bus.calls.borrow().is_empty());
    assert_eq!(f.results.borrow()[0], (false, "That action is unavailable.".to_owned()));
    f.backend.set_capabilities(Capabilities { poweroff: true, restart: true, ..Capabilities::default() });
    f.backend.perform("restart", done(&f.results));
    let call = f.bus.last().unwrap();
    assert_eq!(call.method, "Reboot");
    assert_eq!(call.params.unwrap().get::<(bool,)>(), Some((true,)));
    f.bus.reply("Reboot", Err("GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: denied".into()));
    assert_eq!(f.results.borrow()[1], (false, "GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: denied".to_owned()));
}

#[test]
fn poweroff_and_unknown_actions() {
    let f = fixture(Capabilities::all());
    f.backend.perform("poweroff", done(&f.results));
    assert_eq!(f.bus.methods(), ["PowerOff"]);
    f.backend.perform("dance", done(&f.results));
    assert_eq!(f.results.borrow()[0], (false, "Unknown session action.".to_owned()));
    let f = fixture(Capabilities::default());
    f.backend.perform("sleep", done(&f.results));
    assert_eq!(f.results.borrow()[0], (false, "Sleep is unavailable.".to_owned()));
    assert_eq!(f.runner.count(), 0);
}

#[test]
fn restart_capability_is_queried_on_connect() {
    let f = fixture(Capabilities::default());
    let bus = FakeSystemBus::new();
    f.backend.connected(Ok(Rc::new(bus.clone()) as Rc<dyn SystemBus>));
    assert_eq!(bus.methods(), ["CanSuspend", "CanReboot", "CanPowerOff", "Get"]);
    for call in bus.calls.borrow().iter() {
        assert_eq!((call.flags, call.timeout), (gio::DBusCallFlags::NO_AUTO_START, 3000));
        assert!(call.cancellable.is_some());
    }
    bus.reply("CanSuspend", Ok(Variant::from(("challenge",))));
    bus.reply("CanReboot", Ok(Variant::from(("no",))));
    bus.reply("CanPowerOff", Ok(Variant::from(("yes",))));
    let seats = Variant::from_variant(&vec![glib::variant::ObjectPath::try_from("/org/freedesktop/DisplayManager/Seat0".to_owned()).unwrap()].to_variant());
    std::env::remove_var("XDG_SEAT_PATH");
    bus.reply("Get", Ok(Variant::tuple_from_iter([seats])));
    let caps = f.backend.capabilities();
    assert_eq!((caps.sleep, caps.restart, caps.poweroff, caps.switch_user), (true, false, true, true));
    assert_eq!(f.backend.greeter().as_deref(), Some("/org/freedesktop/DisplayManager/Seat0"));
}

#[test]
fn capability_errors_are_reported_and_missing_display_manager_hides_switch_user() {
    let f = fixture(Capabilities::default());
    let bus = FakeSystemBus::new();
    f.backend.connected(Ok(Rc::new(bus.clone()) as Rc<dyn SystemBus>));
    bus.reply("CanSuspend", Err("boom".into()));
    bus.reply("Get", Err("GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: none".into()));
    assert_eq!(f.backend.error(), "boom");
    assert!(!f.backend.capabilities().switch_user);
    let f = fixture(Capabilities::default());
    f.backend.connected(Err("no system bus".into()));
    assert_eq!(f.backend.error(), "no system bus");
}

#[test]
fn logout_requires_our_compositor_session() {
    let other = |_: &std::path::Path| Ok("other\n".to_owned());
    let labwc = |path: &std::path::Path| {
        assert_eq!(path, std::path::Path::new("/proc/123/comm"));
        Ok("labwc\n".to_owned())
    };
    assert!(!can_logout_with(Some("123"), other));
    assert!(can_logout_with(Some("123"), labwc));
    assert!(!can_logout_with(Some("1"), labwc));
    assert!(!can_logout_with(Some("x"), labwc));
    assert!(!can_logout_with(None, labwc));
    assert!(!can_logout_with(Some("123"), |_| Err(std::io::Error::other("gone"))));
    // Without our compositor (the test runner has no LABWC_PID labwc) logout refuses.
    let f = fixture(Capabilities::all());
    if !f.backend.can_logout() {
        f.backend.perform("logout", done(&f.results));
        assert_eq!(f.runner.count(), 0);
        assert_eq!(f.results.borrow()[0], (false, "The labwc session is unavailable.".to_owned()));
    }
}

#[test]
fn command_runner_reports_status_and_stderr() {
    let context = glib::MainContext::new();
    context.with_thread_default(|| {
        let results: Results = Rc::default();
        GioRunner.run(&["sh", "-c", "echo out; echo ' bad thing ' >&2; exit 3"], done(&results));
        GioRunner.run(&["true"], done(&results));
        GioRunner.run(&["/nonexistent/session-manager-test"], done(&results));
        assert!(tessel_core::timer::wait_for(&context, std::time::Duration::from_secs(5), || results.borrow().len() == 3));
        let mut got = results.borrow().clone();
        got.sort();
        assert_eq!(got[0].0, false);
        assert!(got.iter().any(|r| *r == (false, "bad thing".to_owned())));
        assert!(got.iter().any(|r| *r == (true, String::new())));
    }).unwrap();
}
