//! Fake system power services on `$DBUS_SESSION_BUS_ADDRESS` for measurements
//! in a private headless session (tests/measure_headless.sh). Records calls
//! only; never forwards anything to real logind/UPower/power-profiles.

#[path = "../tests/support/fake_services.rs"]
mod fake_services;

use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("private bus address");
    assert!(!address.contains("/run/user/1000/bus"), "refusing to serve fakes on the live session bus");
    let world: fake_services::Shared = Rc::new(RefCell::new(fake_services::World { active_profile: "balanced".into(), ..Default::default() }));
    let _services = fake_services::serve(&address, &world);
    if let Some(ready) = std::env::args().nth(1) {
        std::fs::write(ready, "ready\n").unwrap();
    }
    let main_loop = glib::MainLoop::new(None, false);
    let quit = main_loop.clone();
    suite_core::glib_compat::unix_signal_add_local(libc::SIGTERM, move || {
        quit.quit();
        glib::ControlFlow::Break
    });
    main_loop.run();
}
