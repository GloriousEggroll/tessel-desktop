//! Serve a FAKE org.freedesktop.timedate1 on the bus in $DBUS_SYSTEM_BUS_ADDRESS
//! (a private daemon) until killed; `argv[1]` is touched once the name is owned.
//! Used by tests/measure_headless.sh. Never talks to the real timedated.

#[path = "../tests/support/fake_timedate.rs"]
mod fake_timedate;

fn main() {
    let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").expect("private DBUS_SYSTEM_BUS_ADDRESS");
    assert!(!address.contains("/run/dbus/system_bus_socket"), "refusing to serve on the real system bus");
    let state = std::rc::Rc::new(std::cell::RefCell::new(fake_timedate::Timedate::default()));
    let _connection = fake_timedate::serve(&address, &state);
    if let Some(ready) = std::env::args().nth(1) {
        std::fs::write(ready, "").unwrap();
    }
    glib::MainLoop::new(None, false).run();
}
