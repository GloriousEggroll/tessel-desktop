//! Serve the FAKE NetworkManager / Secret Service / notifications fixture on
//! the session bus of the current (private, headless) session until killed.
//! Used by tests/measure_headless.sh; refuses to run on the real session.



#[path = "../tests/support/fake_nm.rs"]
mod fake_nm;

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || runtime.is_empty() {
        eprintln!("refusing to run outside a private session");
        std::process::exit(1);
    }
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("private session bus");
    if std::env::var("DBUS_SYSTEM_BUS_ADDRESS").ok().as_deref() != Some(address.as_str()) {
        eprintln!("DBUS_SYSTEM_BUS_ADDRESS must point at the private session bus");
        std::process::exit(1);
    }
    let world: fake_nm::Shared = Default::default();
    let _nm = fake_nm::serve_nm(&address, &world);
    let _secrets = fake_nm::serve_secrets(&address, &world);
    if std::env::var_os("FAKE_NOTIFICATIONS").is_some() {
        let _notifications = fake_nm::serve_notifications(&address, &world);
        std::mem::forget(_notifications);
    }
    if let Some(ready) = std::env::args().nth(1) {
        let _ = std::fs::write(ready, "ready");
    }
    let log = std::env::var("FAKE_NM_LOG").ok();
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        if let Some(log) = &log {
            let text: String = world.borrow().calls.iter().map(|c| format!("{} {} {}\n", c.0, c.1, c.2)).collect();
            let _ = std::fs::write(log, text);
        }
        glib::ControlFlow::Continue
    });
    glib::MainLoop::new(None, false).run();
}
