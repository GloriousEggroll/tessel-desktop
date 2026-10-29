//! Private D-Bus daemon + fake system services (PolicyKit1 Authority, BlueZ),
//! each served from its own thread/main context so the code under test can
//! make synchronous calls to them. Never touches the real session/system bus.
#![allow(dead_code)]

use gio::prelude::*;
use glib::Variant;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub fn scratch(name: &str) -> PathBuf {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/run/user/1000/cbt/rust-authentication-manager".into());
    let dir = Path::new(&base).join(format!("t-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn pump(duration: Duration) {
    let end = Instant::now() + duration;
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if condition() {
            return;
        }
        pump(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

/// A dbus-daemon with the session policy listening in a private directory.
pub struct PrivateBus {
    pub address: String,
    child: Child,
}

impl PrivateBus {
    pub fn start(dir: &Path) -> Self {
        let config = std::fs::read_to_string("/usr/share/dbus-1/session.conf").unwrap();
        let start = config.find("<listen>").unwrap();
        let end = config.find("</listen>").unwrap() + "</listen>".len();
        let config = format!("{}<listen>unix:tmpdir={}</listen>{}", &config[..start], dir.display(), &config[end..]);
        let path = dir.join("bus.conf");
        std::fs::write(&path, config).unwrap();
        let mut child = Command::new("dbus-daemon").arg(format!("--config-file={}", path.display()))
            .args(["--nofork", "--print-address=1"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut line).unwrap();
        let address = line.trim().to_owned();
        assert!(address.contains(dir.to_str().unwrap()), "private bus address {address}");
        Self { address, child }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Debug)]
pub struct Call {
    pub sender: String,
    pub path: String,
    pub interface: String,
    pub method: String,
    pub params: Variant,
    pub at: Instant,
}

pub type Reply = Result<Option<Variant>, (String, String)>;
pub type Handler = Box<dyn Fn(&Call) -> Reply + Send>;

pub struct Object {
    pub path: String,
    pub xml: String,
    pub handler: Handler,
    pub properties: Vec<(String, Variant)>,
}

/// A fake service owning `name` on `address`, served from its own thread.
pub struct Service {
    pub connection: gio::DBusConnection,
    pub calls: Arc<Mutex<Vec<Call>>>,
    main_loop: glib::MainLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Service {
    pub fn spawn(address: &str, name: &str, objects: Vec<Object>) -> Self {
        let calls: Arc<Mutex<Vec<Call>>> = Arc::default();
        let (tx, rx) = mpsc::channel();
        let (address, name, log) = (address.to_owned(), name.to_owned(), calls.clone());
        let thread = std::thread::spawn(move || {
            let context = glib::MainContext::new();
            context.with_thread_default(|| {
                let connection = connect(&address);
                for object in objects {
                    let info = gio::DBusNodeInfo::for_xml(&object.xml).unwrap();
                    let handler = Arc::new(object.handler);
                    let properties = Arc::new(object.properties);
                    for interface in info.interfaces() {
                        let (log, handler, properties) = (log.clone(), handler.clone(), properties.clone());
                        connection.register_object(&object.path, interface)
                            .method_call(move |_, sender, path, interface, method, params, invocation| {
                                let call = Call { sender: sender.unwrap_or("").into(), path: path.into(),
                                                  interface: interface.unwrap_or("").into(), method: method.into(),
                                                  params, at: Instant::now() };
                                log.lock().unwrap().push(call.clone());
                                match handler(&call) {
                                    Ok(value) => invocation.return_value(value.as_ref()),
                                    Err((name, message)) => invocation.return_dbus_error(&name, &message),
                                }
                            })
                            .property(move |_, _, _, _, property| {
                                properties.iter().find(|(n, _)| n == property).map(|(_, v)| v.clone())
                                    .unwrap_or_else(|| "".to_variant())
                            })
                            .build().unwrap();
                    }
                }
                request_name(&connection, &name);
                let main_loop = glib::MainLoop::new(Some(&context), false);
                tx.send((connection.clone(), main_loop.clone())).unwrap();
                main_loop.run();
            }).unwrap();
        });
        let (connection, main_loop) = rx.recv().unwrap();
        Self { connection, calls, main_loop, thread: Some(thread) }
    }

    pub fn calls(&self, method: &str) -> Vec<Call> {
        self.calls.lock().unwrap().iter().filter(|c| c.method == method).cloned().collect()
    }

    pub fn release(&self, name: &str) {
        self.connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                  "ReleaseName", Some(&(name,).to_variant()), None, gio::DBusCallFlags::NONE, 5000,
                                  None::<&gio::Cancellable>).unwrap();
    }

    pub fn request(&self, name: &str) {
        request_name(&self.connection, name);
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.main_loop.quit();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>).unwrap()
}

fn request_name(connection: &gio::DBusConnection, name: &str) {
    let reply = connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                     "RequestName", Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE,
                                     5000, None::<&gio::Cancellable>).unwrap();
    assert_eq!(reply.get::<(u32,)>(), Some((1,)), "own {name}");
}

pub const AUTHORITY_XML: &str = r#"<node><interface name="org.freedesktop.PolicyKit1.Authority">
<method name="RegisterAuthenticationAgent"><arg type="(sa{sv})" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
<method name="RegisterAuthenticationAgentWithOptions"><arg type="(sa{sv})" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
<method name="UnregisterAuthenticationAgent"><arg type="(sa{sv})" direction="in"/><arg type="s" direction="in"/></method>
<property name="BackendName" type="s" access="read"/>
<property name="BackendVersion" type="s" access="read"/>
<property name="BackendFeatures" type="u" access="read"/>
</interface></node>"#;

/// A fake `org.freedesktop.PolicyKit1` Authority that accepts registrations.
pub fn fake_authority(address: &str) -> Service {
    Service::spawn(address, "org.freedesktop.PolicyKit1", vec![Object {
        path: "/org/freedesktop/PolicyKit1/Authority".into(),
        xml: AUTHORITY_XML.into(),
        handler: Box::new(|_| Ok(None)),
        properties: vec![("BackendName".into(), "fake".to_variant()), ("BackendVersion".into(), "0".to_variant()),
                         ("BackendFeatures".into(), 0u32.to_variant())],
    }])
}

pub const AGENT_MANAGER_XML: &str = r#"<node><interface name="org.bluez.AgentManager1">
<method name="RegisterAgent"><arg type="o" direction="in"/><arg type="s" direction="in"/></method>
<method name="UnregisterAgent"><arg type="o" direction="in"/></method>
<method name="RequestDefaultAgent"><arg type="o" direction="in"/></method>
</interface></node>"#;

pub const DEVICE_XML: &str = r#"<node><interface name="org.bluez.Device1">
<property name="Alias" type="s" access="read"/>
</interface></node>"#;

pub fn fake_bluez(address: &str, alias: &str) -> Service {
    Service::spawn(address, "org.bluez", vec![
        Object { path: "/org/bluez".into(), xml: AGENT_MANAGER_XML.into(), handler: Box::new(|_| Ok(None)),
                 properties: vec![] },
        Object { path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF".into(), xml: DEVICE_XML.into(),
                 handler: Box::new(|_| Ok(None)), properties: vec![("Alias".into(), alias.to_variant())] },
    ])
}

/// (kind, details) of a polkit subject `(sa{sv})`.
pub fn subject(value: &Variant) -> (String, std::collections::HashMap<String, Variant>) {
    let kind = value.child_value(0).str().unwrap_or("").to_owned();
    let mut details = std::collections::HashMap::new();
    let dict = value.child_value(1);
    for i in 0..dict.n_children() {
        let entry = dict.child_value(i);
        let key = entry.child_value(0).str().unwrap_or("").to_owned();
        details.insert(key, entry.child_value(1).as_variant().unwrap());
    }
    (kind, details)
}
