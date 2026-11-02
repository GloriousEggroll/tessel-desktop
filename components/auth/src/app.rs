//! Application + main (authentication_manager.py:302-366).

use crate::agent::AuthenticationAgent;
use crate::bluetooth::{BluetoothAgent, PairingFactory, PairingWindow};
use crate::listener::{session_pid, Registration, Subject, OBJECT_PATH};
use crate::session::{PolkitSession, SessionFactory};
use crate::window::AuthenticationWindow;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub const APPLICATION_ID: &str = "org.tessel.Authentication";

/// Injection points; production uses [`Services::default`].
pub struct Services {
    pub sessions: SessionFactory,
    pub pairing: PairingFactory,
    /// `None` = the system bus (`Gio.bus_get_sync(SYSTEM)`).
    pub bluetooth_bus: Option<gio::DBusConnection>,
}

impl Default for Services {
    fn default() -> Self {
        Self { sessions: PolkitSession::factory(), pairing: PairingWindow::factory(), bluetooth_bus: None }
    }
}

#[derive(Default)]
struct State {
    agent: Option<AuthenticationAgent>,
    registration: Option<Registration>,
    bluetooth: Option<BluetoothAgent>,
    hold: Option<gio::ApplicationHoldGuard>,
}

/// Type=notify: `READY=1` on `$NOTIFY_SOCKET` (leading `@` = abstract).
pub fn notify_ready(address: Option<&str>) -> std::io::Result<()> {
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let Some(address) = address.filter(|a| !a.is_empty()) else { return Ok(()) };
    let socket = UnixDatagram::unbound()?;
    if let Some(name) = address.strip_prefix('@') {
        use std::os::linux::net::SocketAddrExt;
        socket.connect_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)?;
    } else {
        socket.connect(address)?;
    }
    socket.send(b"READY=1")?;
    Ok(())
}

pub fn build(services: Services) -> gtk::Application {
    let app = gtk::Application::new(Some(APPLICATION_ID), gio::ApplicationFlags::empty());
    let state: Rc<RefCell<State>> = Rc::default();
    let services = Rc::new(services);
    let s = state.clone();
    app.connect_activate(move |app| activate(app, &s, &services));
    let s = state;
    app.connect_shutdown(move |_| {
        let (bluetooth, agent, registration) = {
            let mut state = s.borrow_mut();
            (state.bluetooth.take(), state.agent.take(), state.registration.take())
        };
        if let Some(bluetooth) = bluetooth {
            bluetooth.close();
        }
        if let Some(agent) = &agent {
            agent.close();
        }
        if let Some(mut registration) = registration {
            registration.unregister();
        }
        drop(agent);
    });
    app
}

fn activate(app: &gtk::Application, state: &Rc<RefCell<State>>, services: &Rc<Services>) {
    if state.borrow().agent.is_some() {
        return;
    }
    let hold = app.hold();
    let agent = AuthenticationAgent::new(Some(app.clone()), AuthenticationWindow::factory(services.sessions.clone()));
    // systemd user services belong to the user manager rather than a login
    // session. Register for the compositor's actual session.
    let registration = session_pid(std::env::var("LABWC_PID").ok().as_deref())
        .and_then(|pid| Subject::unix_session_for_process(pid).map_err(|e| e.message().to_owned()))
        .and_then(|subject| agent.listener.register(&subject, OBJECT_PATH).map_err(|e| e.message().to_owned()));
    {
        let mut s = state.borrow_mut();
        s.hold = Some(hold);
        s.agent = Some(agent);
    }
    match registration {
        Ok(registration) => state.borrow_mut().registration = Some(registration),
        Err(message) => {
            eprintln!("Could not register authentication agent: {message}");
            app.quit();
            return;
        }
    }
    // Bluetooth pairing prompts live here too: this service always runs in
    // the session, unlike the Bluetooth popup. A missing bluetoothd is fine.
    let bluetooth = BluetoothAgent::new(Some(app.clone()), services.bluetooth_bus.clone(), None, services.pairing.clone());
    match bluetooth.start() {
        Ok(()) => state.borrow_mut().bluetooth = Some(bluetooth),
        Err(error) => eprintln!("Bluetooth pairing agent unavailable: {}", error.message()),
    }
    // Type=notify reports readiness only after the session agent is registered.
    let _ = notify_ready(std::env::var("NOTIFY_SOCKET").ok().as_deref());
}

/// `main()`; returns the exit status.
pub fn main_with(services: Services) -> i32 {
    tessel_core::i18n::initialize();
    // Never register a headless agent after the old Wayland session exits:
    // it would report READY but could not present authentication dialogs.
    if gtk::init().is_err() || gdk::Display::default().is_none() {
        eprintln!("Authentication agent requires an active graphical session.");
        return 1;
    }
    let app = build(services);
    for number in [libc::SIGTERM, libc::SIGINT] {
        let app = app.clone();
        glib_unix::unix_signal_add_local(number, move || {
            app.quit();
            glib::ControlFlow::Break
        });
    }
    let args: Vec<String> = std::env::args().collect();
    i32::from(app.run_with_args(&args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_is_sent_to_path_and_abstract_sockets() {
        use std::os::unix::net::UnixDatagram;
        let dir = std::env::temp_dir().join(format!("auth-notify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify");
        let _ = std::fs::remove_file(&path);
        let server = UnixDatagram::bind(&path).unwrap();
        notify_ready(Some(path.to_str().unwrap())).unwrap();
        let mut buffer = [0u8; 32];
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"READY=1");
        use std::os::linux::net::SocketAddrExt;
        let name = format!("auth-notify-test-{}", std::process::id());
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let server = UnixDatagram::bind_addr(&addr).unwrap();
        notify_ready(Some(&format!("@{name}"))).unwrap();
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"READY=1");
        assert!(notify_ready(None).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
