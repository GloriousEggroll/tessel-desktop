//! The real gio `DateBackend` against a FAKE timedate1 on a private
//! dbus-daemon (gio::TestDBus) that also serves as the "system" bus. Nothing
//! here can change the real clock, timezone or NTP setting. Also checks the
//! binary's local argument handling (no display or bus needed).

use tessel_calendar::backend::{Clock, DateBackend, DateSystem};
use tessel_calendar::civil::Date;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[path = "support/fake_timedate.rs"]
mod fake_timedate;
use fake_timedate::{own, release, serve, Shared, Timedate};

fn pump(duration: Duration) {
    let end = Instant::now() + duration;
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait_for(what: &str, timeout: f64, mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        if condition() {
            return;
        }
        pump(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn writes(state: &Shared) -> Vec<(String, String, bool)> {
    state.borrow().calls.iter().filter(|c| c.0 != "ListTimezones").cloned().collect()
}

#[test]
fn backend_against_fake_timedated_on_private_bus() {
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address);
    let state: Shared = Rc::new(RefCell::new(Timedate::default()));
    let service = serve(&address, &state);

    let errors: Rc<RefCell<Vec<String>>> = Rc::default();
    let e = errors.clone();
    let changes = Rc::new(std::cell::Cell::new(0u32));
    let c = changes.clone();
    let backend = DateBackend::new(move || c.set(c.get() + 1), move |m| e.borrow_mut().push(m.into()));
    backend.start();
    wait_for("ready with time zones", 5.0, || { let s = backend.state(); s.ready && s.timezones_ready });
    let s = backend.state();
    assert_eq!((s.timezone.as_str(), s.ntp, s.can_ntp, s.synchronized), ("America/Denver", false, true, false));
    assert_eq!(s.timezones, ["America/Denver", "Asia/Tokyo", "Europe/London", "UTC"]);
    assert!(writes(&state).is_empty(), "start() never writes");
    assert_eq!(backend.now().zone.key, "America/Denver");

    // SetNTP: interactive authorization, no optimistic state, refreshed afterwards.
    assert!(backend.set_ntp(true));
    assert!(backend.state().busy);
    assert!(!backend.state().ntp);
    wait_for("SetNTP applied", 5.0, || { let s = backend.state(); !s.busy && s.ntp });
    assert_eq!(writes(&state), [("SetNTP".to_owned(), "(true, true)".to_owned(), true)]);
    // Manual writes are refused while automatic time is on.
    assert!(!backend.set_date(Date::ymd(2026, 9, 19)));
    assert!(errors.borrow().last().unwrap().contains("Turn off automatic date and time"));
    assert!(backend.set_ntp(false));
    wait_for("SetNTP off", 5.0, || { let s = backend.state(); !s.busy && !s.ntp });

    // SetTimezone after a fresh GetAll preflight.
    assert!(backend.set_timezone("Europe/London"));
    wait_for("timezone applied", 5.0, || { let s = backend.state(); !s.busy && s.timezone == "Europe/London" });
    assert_eq!(writes(&state).last().unwrap(), &("SetTimezone".to_owned(), "('Europe/London', true)".to_owned(), true));
    assert!(!backend.set_timezone("Mars/Olympus"));

    // SetTime with (xbb) = (usec, relative=false, interactive=true).
    assert!(backend.set_datetime(Date::ymd(2026, 12, 20), Clock::new(4, 5, 6).unwrap()));
    wait_for("SetTime", 5.0, || !backend.state().busy);
    let (method, params, interactive) = writes(&state).last().unwrap().clone();
    assert_eq!((method.as_str(), interactive), ("SetTime", true));
    // 2026-12-20 04:05:06 Europe/London (GMT) = 1797739506 s.
    assert_eq!(params, "(1797739506000000, false, true)");

    // Denied authorization and timedated's NTP error are mapped and change nothing.
    state.borrow_mut().fail_next = Some("org.freedesktop.DBus.Error.AccessDenied".into());
    assert!(backend.set_date(Date::ymd(2026, 9, 19)));
    wait_for("denied", 5.0, || !backend.state().busy);
    assert_eq!(backend.state().error, "Authorization was canceled or denied. No change was applied.");
    state.borrow_mut().fail_next = Some("org.freedesktop.timedate1.AutomaticTimeSyncEnabled".into());
    assert!(backend.set_date(Date::ymd(2026, 9, 19)));
    wait_for("refused", 5.0, || !backend.state().busy);
    assert_eq!(backend.state().error, "Turn off automatic date and time before changing the date.");
    assert_eq!(backend.state().timezone, "Europe/London");

    // An external change arrives through the explicit GetAll refresh.
    state.borrow_mut().synchronized = true;
    backend.refresh();
    wait_for("external change", 5.0, || backend.state().synchronized);

    // Losing the service fails closed and reconnects after 2.5 s.
    release(&service);
    wait_for("service loss", 5.0, || !backend.state().ready);
    let s = backend.state();
    assert!(s.ntp && !s.can_ntp && !s.busy);
    assert!(s.error.contains("unavailable"), "{}", s.error);
    assert!(!backend.set_ntp(true));
    own(&service);
    wait_for("reconnected", 8.0, || backend.state().ready);
    assert_eq!(backend.state().timezone, "Europe/London");
    backend.close();
    let before = changes.get();
    pump(Duration::from_millis(100));
    assert_eq!(changes.get(), before, "a closed backend never notifies");
    drop(service);
    bus.down();
}

#[test]
fn binary_validates_arguments_locally() {
    let binary = env!("CARGO_BIN_EXE_tessel-calendar");
    let run = |args: &[&str]| {
        let output = std::process::Command::new(binary).args(args).env_remove("DBUS_SESSION_BUS_ADDRESS").env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY").output().unwrap();
        (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned(),
         String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let (code, out, _) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("usage: tessel-calendar [-h] [--popup | --settings]"), "{out}");
    let (code, _, err) = run(&["--popup-offset", "-1"]);
    assert_eq!(code, 2);
    assert!(err.ends_with("tessel-calendar: error: --popup-offset must be nonnegative\n"), "{err}");
    let (code, _, err) = run(&["--popup", "--settings"]);
    assert_eq!(code, 2);
    assert!(err.contains("argument --settings: not allowed with argument --popup"));
}
