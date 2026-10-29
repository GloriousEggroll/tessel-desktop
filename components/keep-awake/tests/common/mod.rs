//! Shared test helpers: main-loop pumping, pipes standing in for inhibitor
//! FDs, and a fake logind on a private bus (never the real system bus).
#![allow(dead_code)]

use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn wait_for(predicate: impl Fn() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.01);
        if predicate() {
            return true;
        }
    }
    false
}

/// A pipe: (non-blocking reader, writer).
pub fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

#[derive(Debug, PartialEq)]
pub enum Read {
    Eof,
    WouldBlock,
    Data,
}

/// `os.read(reader, 1)` outcome: EOF proves every writer copy was closed.
pub fn read(reader: &OwnedFd) -> Read {
    let mut byte = [0u8; 1];
    let n = unsafe { libc::read(reader.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
    match n {
        0 => Read::Eof,
        n if n > 0 => Read::Data,
        _ => {
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EAGAIN));
            Read::WouldBlock
        }
    }
}

const LOGIN_XML: &str = r#"<node><interface name="org.freedesktop.login1.Manager"><method name="Inhibit">
  <arg type="s" direction="in"/><arg type="s" direction="in"/>
  <arg type="s" direction="in"/><arg type="s" direction="in"/>
  <arg type="h" direction="out"/></method></interface></node>"#;

/// A fake `org.freedesktop.login1` on a private bus connection: each
/// `Inhibit` returns the writer end of a fresh pipe and keeps the reader.
pub struct FakeLogin {
    pub connection: gio::DBusConnection,
    pub calls: Rc<RefCell<Vec<(String, String, String, String)>>>,
    pub readers: Rc<RefCell<Vec<OwnedFd>>>,
    pub deny: Rc<Cell<bool>>,
    pub owned: Rc<Cell<bool>>,
    owner: RefCell<Option<gio::OwnerId>>,
    registration: RefCell<Option<gio::RegistrationId>>,
}

impl FakeLogin {
    pub fn new(address: &str) -> Self {
        let connection = gio::DBusConnection::for_address_sync(address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None::<&gio::DBusAuthObserver>, None::<&gio::Cancellable>).unwrap();
        let calls: Rc<RefCell<Vec<(String, String, String, String)>>> = Rc::default();
        let readers: Rc<RefCell<Vec<OwnedFd>>> = Rc::default();
        let deny = Rc::new(Cell::new(false));
        let info = gio::DBusNodeInfo::for_xml(LOGIN_XML).unwrap();
        let interface = info.lookup_interface("org.freedesktop.login1.Manager").unwrap();
        let (c, r, d) = (calls.clone(), readers.clone(), deny.clone());
        let registration = connection.register_object("/org/freedesktop/login1", &interface)
            .method_call(move |_, _, _, _, _, params, invocation| {
                c.borrow_mut().push(params.get::<(String, String, String, String)>().unwrap());
                if d.get() {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "Not permitted");
                    return;
                }
                let (reader, writer) = pipe();
                let list = gio::UnixFDList::new();
                let index = list.append(&writer).unwrap();
                drop(writer);
                r.borrow_mut().push(reader);
                invocation.return_value_with_unix_fd_list(Some(&(glib::variant::Handle(index),).to_variant()), Some(&list));
            })
            .build().unwrap();
        let login = Self { connection, calls, readers, deny, owned: Rc::new(Cell::new(false)),
                           owner: RefCell::new(None), registration: RefCell::new(Some(registration)) };
        login.own();
        login
    }

    pub fn unique_name(&self) -> String { self.connection.unique_name().unwrap().to_string() }

    pub fn own(&self) {
        let (acquired, lost) = (self.owned.clone(), self.owned.clone());
        let id = gio::bus_own_name_on_connection(&self.connection, "org.freedesktop.login1",
            gio::BusNameOwnerFlags::NONE, move |_, _| acquired.set(true), move |_, _| lost.set(false));
        self.owner.replace(Some(id));
        assert!(wait_for(|| self.owned.get(), 3.0), "fake logind could not own its name");
    }

    pub fn disown(&self) {
        if let Some(id) = self.owner.take() {
            gio::bus_unown_name(id);
        }
        self.owned.set(false);
    }

    pub fn reader(&self, index: usize) -> Read { read(&self.readers.borrow()[index]) }
}

impl Drop for FakeLogin {
    fn drop(&mut self) {
        self.disown();
        if let Some(id) = self.registration.take() {
            let _ = self.connection.unregister_object(id);
        }
    }
}

/// A private dbus-daemon serving as both the session and the "system" bus.
pub struct PrivateBus {
    pub bus: gio::TestDBus,
    pub address: String,
}

impl PrivateBus {
    pub fn up() -> Self {
        // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
        // and in spawned children must not look up portals or gvfs.
        for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
            std::env::set_var(key, value);
        }
        let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
        bus.up();
        let address = bus.bus_address().unwrap().to_string();
        assert!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap() == address, "TestDBus did not take over the session bus");
        // SAFETY: tests touching the environment run single-threaded per binary.
        unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };
        Self { bus, address }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) { self.bus.down() }
}
