//! Private-bus checks without a display: the real binary's headless refusal,
//! and the BlueZ pairing agent against a fake `org.bluez` (AgentManager1 +
//! Device1.Alias) on a private dbus-daemon.

use tessel_auth::bluetooth::*;
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

mod support;
use support::*;

const BINARY: &str = env!("CARGO_BIN_EXE_tessel-auth");
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn headless_start_exits_before_registering_authentication() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("headless");
    let bus = PrivateBus::start(&dir);
    let authority = fake_authority(&bus.address);
    let runtime = dir.join("rt");
    std::fs::create_dir_all(&runtime).unwrap();
    let notify = dir.join("notify");
    let socket = std::os::unix::net::UnixDatagram::bind(&notify).unwrap();
    socket.set_nonblocking(true).unwrap();
    let output = Command::new(BINARY).env_clear()
        .env("HOME", std::env::var("HOME").unwrap()).env("PATH", std::env::var("PATH").unwrap())
        .env("XDG_RUNTIME_DIR", &runtime).env("XDG_CONFIG_HOME", dir.join("config"))
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address).env("DBUS_SYSTEM_BUS_ADDRESS", &bus.address)
        .env("NOTIFY_SOCKET", &notify).env("GDK_BACKEND", "wayland").env("NO_AT_BRIDGE", "1")
        .env("GDK_DEBUG", "no-portals").env("GIO_USE_VFS", "local").env("GTK_USE_PORTAL", "0")
        .output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Authentication agent requires an active graphical session.\n"), "{stderr}");
    assert!(!stderr.contains("Could not register"), "{stderr}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(authority.calls.lock().unwrap().is_empty(), "nothing registered");
    let mut buffer = [0u8; 16];
    assert!(socket.recv(&mut buffer).is_err(), "no READY");
}

struct Window {
    title: String,
    answer: RefCell<Option<Box<dyn FnOnce(Option<Answer>)>>>,
    done: Cell<bool>,
}

impl PairingPrompt for Window {
    fn set_application(&self, _application: Option<&gtk::Application>) {}
    fn present(&self) {}
    fn focus_entry(&self) {}
    fn set_validator(&self, _validator: Validator) {}
    fn set_message(&self, _text: &str) {}
    fn is_done(&self) -> bool {
        self.done.get()
    }
    fn finish(&self, value: Option<Answer>) {
        if !self.done.replace(true) {
            if let Some(answer) = self.answer.take() {
                answer(value);
            }
        }
    }
    fn displayed_device(&self) -> Option<String> {
        None
    }
    fn set_displayed_device(&self, _device: &str) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

type Outcome = Rc<RefCell<Option<Result<Variant, glib::Error>>>>;

fn call_agent(connection: &gio::DBusConnection, owner: &str, method: &str, params: Variant) -> Outcome {
    let outcome: Outcome = Rc::default();
    let out = outcome.clone();
    connection.call(Some(owner), AGENT_PATH, "org.bluez.Agent1", method, Some(&params), None, gio::DBusCallFlags::NONE,
                    5000, None::<&gio::Cancellable>, move |result| { out.replace(Some(result)); });
    outcome
}

#[test]
fn bluetooth_agent_registers_answers_and_reregisters_on_private_bus() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("bluez");
    let bus = PrivateBus::start(&dir);
    let bluez = fake_bluez(&bus.address, "Headphones");
    let connection = connect(&bus.address);
    let owner = connection.unique_name().unwrap().to_string();
    let created: Rc<RefCell<Vec<Rc<Window>>>> = Rc::default();
    let list = created.clone();
    let factory: PairingFactory = Rc::new(move |title, _message, answer, _options| {
        let window = Rc::new(Window { title: title.into(), answer: RefCell::new(Some(answer)), done: Cell::new(false) });
        list.borrow_mut().push(window.clone());
        window as Rc<dyn PairingPrompt>
    });
    let agent = BluetoothAgent::new(None, Some(connection.clone()), None, factory);
    agent.start().unwrap();
    wait_for("RequestDefaultAgent", Duration::from_secs(5), || !bluez.calls("RequestDefaultAgent").is_empty());
    let register = bluez.calls("RegisterAgent");
    assert_eq!(register.len(), 1);
    assert_eq!(register[0].params.child_value(0).str(), Some(AGENT_PATH));
    assert_eq!(register[0].params.child_value(1).str(), Some(CAPABILITY));
    assert_eq!(register[0].sender, owner);
    assert!(agent.is_registered());

    // BlueZ asks for confirmation; the alias comes from Device1.Alias.
    let device = glib::variant::ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap();
    let outcome = call_agent(&bluez.connection, &owner, "RequestConfirmation", (device.clone(), 4021u32).to_variant());
    wait_for("prompt", Duration::from_secs(5), || !created.borrow().is_empty());
    assert_eq!(created.borrow()[0].title, "Pair with Headphones");
    assert!(outcome.borrow().is_none(), "invocation kept until answered");
    created.borrow()[0].finish(Some(Answer::Accepted));
    wait_for("reply", Duration::from_secs(5), || outcome.borrow().is_some());
    assert!(outcome.borrow().as_ref().unwrap().is_ok());

    // BlueZ cancels an open prompt: its invocation is Rejected.
    let pending = call_agent(&bluez.connection, &owner, "RequestAuthorization", (device.clone(),).to_variant());
    wait_for("second prompt", Duration::from_secs(5), || created.borrow().len() == 2);
    let cancel = call_agent(&bluez.connection, &owner, "Cancel", ().to_variant());
    wait_for("cancel replies", Duration::from_secs(5), || pending.borrow().is_some() && cancel.borrow().is_some());
    let error = pending.borrow().as_ref().unwrap().as_ref().unwrap_err().clone();
    assert_eq!(gio::DBusError::remote_error(&error).as_deref(), Some(REJECTED));
    assert!(error.message().contains("Rejected by user"));

    // bluetoothd restarts: re-register.
    bluez.release("org.bluez");
    wait_for("vanished", Duration::from_secs(5), || !agent.is_registered());
    bluez.request("org.bluez");
    wait_for("re-registered", Duration::from_secs(5), || bluez.calls("RequestDefaultAgent").len() == 2);
    assert_eq!(bluez.calls("RegisterAgent").len(), 2);

    agent.close();
    let unregister = bluez.calls("UnregisterAgent");
    assert_eq!(unregister.len(), 1);
    assert_eq!(unregister[0].params.child_value(0).str(), Some(AGENT_PATH));
    // The object is gone.
    let gone = call_agent(&bluez.connection, &owner, "Cancel", ().to_variant());
    wait_for("error", Duration::from_secs(5), || gone.borrow().is_some());
    assert!(gone.borrow().as_ref().unwrap().is_err());
}
