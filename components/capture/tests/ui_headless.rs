#![allow(deprecated)]
//! Port of test_capture_results.py and test_capture_portal.py. Runs only inside
//! the private nested headless labwc started by tests/run_headless.sh (two
//! outputs, private D-Bus as session *and* system bus, private HOME/XDG dirs,
//! logging fakes for pactl/wpctl/wl-copy/labwc first on PATH, an ffmpeg wrapper
//! that refuses PulseAudio). Under plain `cargo test` it skips itself.
//!
//! No xdg-desktop-portal, PipeWire or notification daemon is started: this file
//! provides a FAKE ScreenCast portal (`org.freedesktop.portal.Desktop`, which
//! also plays the patched xdpw + Tessel Shell chooser by calling the requester's
//! CaptureSelection broker), a fake `org.freedesktop.Notifications`, a fake
//! Tessel Dock `org.gtk.Actions`, a fake `org.freedesktop.FileManager1`, all on the
//! private bus, and a fake feed factory producing synthetic frames.

use tessel_capture::app::{CaptureApp, Hooks};
use tessel_capture::args::Options;
use tessel_capture::media::{self, Frame, RecordingOptions};
use tessel_capture::native::{Feed, FeedError, StreamProps};
use tessel_capture::portal::{self, PortalSession};
use tessel_capture::ui::CaptureWindow;
use tessel_capture::{area, compat, editor, results, settings};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- helpers

fn pump(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait(timeout: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    predicate()
}

fn private_dir(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("capture-ui-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                                          None, None::<&gio::Cancellable>).unwrap()
}

fn own(connection: &gio::DBusConnection, name: &str) {
    let reply = connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        Some(&(name, 4u32).to_variant()), Some(glib::VariantTy::new("(u)").unwrap()), gio::DBusCallFlags::NONE, 2000,
        None::<&gio::Cancellable>).unwrap();
    assert_eq!(reply.child_value(0).get::<u32>(), Some(1), "could not own {name}");
}

fn interface(xml: &str, name: &str) -> gio::DBusInterfaceInfo {
    gio::DBusNodeInfo::for_xml(xml).unwrap().lookup_interface(name).unwrap()
}

fn png_size(path: &Path) -> (u32, u32) {
    let data = std::fs::read(path).unwrap();
    tessel_capture::imaging::decode_rgb(&data).unwrap().dimensions()
}

fn option(f: impl FnOnce(&mut Options)) -> Options {
    let mut options = Options::default();
    f(&mut options);
    options
}

fn uri(path: &Path) -> String {
    results::file_uri(path)
}

// ---------------------------------------------------------------- fake feed

#[derive(Default)]
struct FeedLog {
    opened: Vec<(u32, Instant)>,
    not_before: Vec<Option<f64>>,
    closed: usize,
}

struct FakeFeed {
    props: StreamProps,
    log: Arc<Mutex<FeedLog>>,
    closed: AtomicBool,
}

impl Feed for FakeFeed {
    fn frame(&self, _timeout: f64, not_before: Option<f64>) -> Result<Frame, FeedError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(FeedError::Failed("Capture has ended.".into()));
        }
        self.log.lock().unwrap().not_before.push(not_before);
        std::thread::sleep(Duration::from_millis(5));
        let (w, h) = self.props.size.unwrap_or((320, 200));
        // bgr0: pure green, like the Python lock-surface checks.
        Ok(Frame::new(w as u32, h as u32, [0u8, 255, 0, 0].repeat((w * h) as usize)).format("bgr0"))
    }

    fn properties(&self) -> &StreamProps {
        &self.props
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.log.lock().unwrap().closed += 1;
        }
    }
}

// ---------------------------------------------------------------- fake portal

const PORTAL_XML: &str = r#"<node>
<interface name="org.freedesktop.portal.ScreenCast">
<method name="CreateSession"><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
<method name="SelectSources"><arg type="o" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
<method name="Start"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
<method name="OpenPipeWireRemote"><arg type="o" direction="in"/><arg type="a{sv}" direction="in"/><arg type="h" direction="out"/></method>
</interface>
<interface name="org.freedesktop.portal.Session"><method name="Close"/></interface>
</node>"#;

#[derive(Clone, Default)]
struct PortalSessionState {
    types: u32,
    cursor_mode: u32,
}

#[derive(Default)]
struct PortalState {
    cancel: Cell<bool>,
    chooser_count: Cell<u32>,
    unhandled: Cell<u32>,
    starts: Cell<u32>,
    remotes: Cell<u32>,
    session_closes: RefCell<Vec<String>>,
    sessions: RefCell<HashMap<String, PortalSessionState>>,
    /// Output answered by the CaptureSelection broker per session.
    chosen: RefCell<HashMap<String, String>>,
}

struct FakePortal {
    connection: gio::DBusConnection,
    state: Rc<PortalState>,
    monitors: Vec<(String, gdk::Rectangle)>,
}

fn request_path(sender: &str, options: &glib::VariantDict) -> String {
    let token = options.lookup::<String>("handle_token").ok().flatten().unwrap_or_default();
    format!("/org/freedesktop/portal/desktop/request/{}/{token}", sender.trim_start_matches(':').replace('.', "_"))
}

fn object_path(path: &str) -> glib::Variant {
    (glib::variant::ObjectPath::try_from(path.to_string()).unwrap(),).to_variant()
}

impl FakePortal {
    fn new(connection: &gio::DBusConnection, monitors: Vec<(String, gdk::Rectangle)>) -> Rc<Self> {
        let portal = Rc::new(Self { connection: connection.clone(), state: Rc::new(PortalState::default()), monitors });
        let weak = Rc::downgrade(&portal);
        connection.register_object(portal::PATH, &interface(PORTAL_XML, portal::INTERFACE))
            .method_call(move |_, sender, _, _, method, parameters, invocation| {
                if let Some(portal) = weak.upgrade() {
                    portal.call(sender.unwrap_or(""), method, &parameters, invocation);
                }
            }).build().unwrap();
        own(connection, portal::NAME);
        portal
    }

    fn respond(&self, path: String, code: u32, results: glib::Variant) {
        let connection = self.connection.clone();
        glib::idle_add_local_once(move || {
            let _ = connection.emit_signal(None, &path, "org.freedesktop.portal.Request", "Response",
                                           Some(&glib::Variant::tuple_from_iter([code.to_variant(), results])));
        });
    }

    fn call(self: &Rc<Self>, sender: &str, method: &str, parameters: &glib::Variant, invocation: gio::DBusMethodInvocation) {
        match method {
            "CreateSession" => {
                let options = glib::VariantDict::new(Some(&parameters.child_value(0)));
                let token = options.lookup::<String>("session_handle_token").ok().flatten().unwrap_or_default();
                let session = format!("/org/freedesktop/portal/desktop/session/{}/{token}", sender.trim_start_matches(':').replace('.', "_"));
                let (state, closed) = (self.state.clone(), session.clone());
                let _ = self.connection.register_object(&session, &interface(PORTAL_XML, "org.freedesktop.portal.Session"))
                    .method_call(move |_, _, _, _, _, _, invocation| {
                        state.session_closes.borrow_mut().push(closed.clone());
                        invocation.return_value(None);
                    }).build();
                self.state.sessions.borrow_mut().insert(session.clone(), PortalSessionState::default());
                let request = request_path(sender, &options);
                invocation.return_value(Some(&object_path(&request)));
                let results = glib::VariantDict::new(None);
                results.insert("session_handle", session);
                self.respond(request, 0, results.end());
            }
            "SelectSources" => {
                let session = parameters.child_value(0).str().unwrap().to_string();
                let options = glib::VariantDict::new(Some(&parameters.child_value(1)));
                if let Some(entry) = self.state.sessions.borrow_mut().get_mut(&session) {
                    entry.types = options.lookup::<u32>("types").ok().flatten().unwrap_or(1);
                    entry.cursor_mode = options.lookup::<u32>("cursor_mode").ok().flatten().unwrap_or(0);
                }
                let request = request_path(sender, &options);
                invocation.return_value(Some(&object_path(&request)));
                self.respond(request, 0, glib::VariantDict::new(None).end());
            }
            "Start" => {
                self.state.starts.set(self.state.starts.get() + 1);
                let session = parameters.child_value(0).str().unwrap().to_string();
                let options = glib::VariantDict::new(Some(&parameters.child_value(2)));
                let request = request_path(sender, &options);
                invocation.return_value(Some(&object_path(&request)));
                if self.state.cancel.get() {
                    self.respond(request, 1, glib::VariantDict::new(None).end());
                    return;
                }
                self.choose(sender.to_string(), session, request);
            }
            "OpenPipeWireRemote" => {
                self.state.remotes.set(self.state.remotes.get() + 1);
                let file = std::fs::File::open("/dev/null").unwrap();
                let list = gio::UnixFDList::new();
                let index = list.append(&file).unwrap();
                invocation.return_value_with_unix_fd_list(Some(&(glib::variant::Handle(index),).to_variant()), Some(&list));
            }
            _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
        }
    }

    /// The patched xdpw + Tessel Shell chooser: offer the session to the requester's
    /// CaptureSelection broker (async: the requester runs on this main loop).
    fn choose(self: &Rc<Self>, sender: String, session: String, request: String) {
        self.state.chooser_count.set(self.state.chooser_count.get() + 1);
        let types = self.state.sessions.borrow().get(&session).map(|s| s.types).unwrap_or(1);
        let mut tokens: Vec<String> = self.monitors.iter().map(|(name, _)| format!("Monitor: {name} Headless output")).collect();
        tokens.push("Window: 7 org.example.Window Example window".into());
        let path = glib::variant::ObjectPath::try_from(session.clone()).unwrap();
        let this = self.clone();
        let owner = sender.clone();
        self.connection.call(Some(&owner), compat::capture_selection::PATH, compat::capture_selection::INTERFACE, "Select",
            Some(&(path.clone(), tokens.clone()).to_variant()), Some(glib::VariantTy::new("(bs)").unwrap()), gio::DBusCallFlags::NO_AUTO_START,
            1200, None::<&gio::Cancellable>, move |result| {
                let (handled, token) = result.ok().and_then(|r| r.get::<(bool, String)>()).unwrap_or((false, String::new()));
                let chosen = if handled {
                    if token.is_empty() {
                        this.respond(request, 1, glib::VariantDict::new(None).end());
                        return;
                    }
                    token
                } else {
                    if types == 1 {
                        this.state.unhandled.set(this.state.unhandled.get() + 1);
                    }
                    // The user's choice in the normal picker.
                    if types == 2 { tokens.last().unwrap().clone() } else { tokens[0].clone() }
                };
                let props = glib::VariantDict::new(None);
                let output = if let Some(name) = chosen.strip_prefix("Monitor: ").and_then(|r| r.split(' ').next()) {
                    let (_, geometry) = this.monitors.iter().find(|(n, _)| n == name).unwrap();
                    props.insert_value("position", &(geometry.x(), geometry.y()).to_variant());
                    props.insert_value("size", &(geometry.width(), geometry.height()).to_variant());
                    props.insert("source_type", 1u32);
                    name.to_string()
                } else {
                    props.insert_value("size", &(300i32, 200i32).to_variant());
                    props.insert("source_type", 2u32);
                    this.monitors[0].0.clone()
                };
                this.state.chosen.borrow_mut().insert(session.clone(), output.clone());
                let node = 100 + this.state.starts.get();
                let streams = glib::Variant::array_from_iter_with_type(glib::VariantTy::new("(ua{sv})").unwrap(),
                    [glib::Variant::tuple_from_iter([node.to_variant(), props.end()])]);
                let results = glib::VariantDict::new(None);
                results.insert_value("streams", &streams);
                let results = results.end();
                let after = this.clone();
                this.connection.call(Some(&sender), compat::capture_selection::PATH, compat::capture_selection::INTERFACE, "Selected",
                    Some(&(path, vec![output]).to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 1000, None::<&gio::Cancellable>,
                    move |_| after.respond(request, 0, results));
            });
    }
}

// ---------------------------------------------------------------- fake services

const NOTIFY_XML: &str = r#"<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/>
<arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method>
<method name="GetCapabilities"><arg type="as" direction="out"/></method>
<method name="GetServerInformation"><arg type="s" direction="out"/><arg type="s" direction="out"/><arg type="s" direction="out"/><arg type="s" direction="out"/></method>
<signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
<signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
</interface></node>"#;

#[derive(Clone)]
struct Notice {
    id: u32,
    app: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: glib::VariantDict,
}

impl std::fmt::Debug for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Notice({} {:?} {:?} {:?} {:?} {})", self.id, self.app, self.summary, self.body, self.actions, self.hints.end().print(true))
    }
}

struct FakeNotifications {
    connection: gio::DBusConnection,
    items: RefCell<Vec<Notice>>,
}

impl FakeNotifications {
    fn new(connection: &gio::DBusConnection) -> Rc<Self> {
        let this = Rc::new(Self { connection: connection.clone(), items: RefCell::default() });
        let weak = Rc::downgrade(&this);
        connection.register_object("/org/freedesktop/Notifications", &interface(NOTIFY_XML, "org.freedesktop.Notifications"))
            .method_call(move |_, _, _, _, method, parameters, invocation| {
                let Some(this) = weak.upgrade() else { return };
                match method {
                    "Notify" => {
                        let id = this.items.borrow().len() as u32 + 1;
                        this.items.borrow_mut().push(Notice {
                            id, app: parameters.child_value(0).str().unwrap().into(), summary: parameters.child_value(3).str().unwrap().into(),
                            body: parameters.child_value(4).str().unwrap().into(), actions: parameters.child_value(5).get().unwrap(),
                            hints: glib::VariantDict::new(Some(&parameters.child_value(6))),
                        });
                        invocation.return_value(Some(&(id,).to_variant()));
                    }
                    "GetCapabilities" => invocation.return_value(Some(&(vec!["actions", "body"],).to_variant())),
                    "GetServerInformation" => invocation.return_value(Some(&("fake", "test", "1", "1.2").to_variant())),
                    _ => invocation.return_value(None),
                }
            }).build().unwrap();
        own(connection, "org.freedesktop.Notifications");
        this
    }

    fn count(&self) -> usize {
        self.items.borrow().len()
    }

    fn last(&self) -> Notice {
        self.items.borrow().last().cloned().expect("a notification")
    }

    fn since(&self, start: usize) -> Vec<Notice> {
        self.items.borrow()[start..].to_vec()
    }

    fn invoke(&self, id: u32, action: &str) {
        self.connection.emit_signal(None, "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "ActionInvoked",
                                    Some(&(id, action).to_variant())).unwrap();
    }

    fn closed(&self, id: u32) {
        self.connection.emit_signal(None, "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "NotificationClosed",
                                    Some(&(id, 2u32).to_variant())).unwrap();
    }
}

const ACTIONS_XML: &str = r#"<node><interface name="org.gtk.Actions">
<method name="Activate"><arg type="s" direction="in"/><arg type="av" direction="in"/><arg type="a{sv}" direction="in"/></method>
</interface></node>"#;

const FILE_MANAGER_XML: &str = r#"<node><interface name="org.freedesktop.FileManager1"><method name="ShowItems">
<arg type="as" direction="in"/><arg type="s" direction="in"/></method></interface></node>"#;

#[derive(Clone)]
struct Fakes {
    connection: gio::DBusConnection,
    portal: Rc<FakePortal>,
    notifications: Rc<FakeNotifications>,
    dock: Rc<RefCell<Vec<(String, Vec<glib::Variant>)>>>,
    shown: Rc<RefCell<Vec<Vec<String>>>>,
    feeds: Arc<Mutex<FeedLog>>,
    opened: Rc<RefCell<Vec<(String, bool)>>>,
    intercept_open: Rc<Cell<bool>>,
    scratch: PathBuf,
}

// ---------------------------------------------------------------- checks

struct Context {
    app: CaptureApp,
    fakes: Fakes,
    directory: PathBuf,
    file: PathBuf,
}

impl Context {
    fn window(&self) -> CaptureWindow {
        self.app.ensure_window()
    }

    fn reset_window(&self) {
        if let Some(window) = self.app.window() {
            if window.0.recorder.borrow().is_some() {
                window.stop_clicked();
                wait(20.0, || window.0.recorder.borrow().is_none());
            }
            if window.0.busy.get() {
                window.stop_clicked();
            }
            window.disarm();
            window.window().close();
            pump(0.05);
            window.window().destroy();
        }
        *self.app.0.window.borrow_mut() = None;
        self.fakes.portal.state.cancel.set(false);
        pump(0.05);
    }

    fn wl_copies(&self) -> Vec<String> {
        std::fs::read_to_string(self.fakes.scratch.join("wl-copy.log")).unwrap_or_default().lines().map(String::from).collect()
    }

    fn default_app(&self, ident: &str, executable: &str, mime: &str) {
        let directory = PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap()).join("applications");
        std::fs::create_dir_all(&directory).unwrap();
        let desktop = directory.join(ident);
        std::fs::write(&desktop, format!("[Desktop Entry]\nType=Application\nName=Private capture test\nExec={executable} %u\nMimeType={mime};\n")).unwrap();
        let info = gio_unix::DesktopAppInfo::from_filename(&desktop).unwrap();
        info.set_as_default_for_type(mime).unwrap();
        assert_eq!(gio::AppInfo::default_for_type(mime, false).and_then(|a| a.id()).as_deref(), Some(ident));
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.directory.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

type Check = fn(&Context) -> Result<(), String>;

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => { if !$cond { return Err(format!($($arg)+)); } };
}

fn test_routed_capture_notice_keeps_output_and_both_file_actions(c: &Context) -> Result<(), String> {
    let notifications = c.app.0.notifications.borrow().clone().unwrap();
    notifications.saved("Recording saved", &c.file, "HDMI-A-1", false, true);
    ensure!(wait(5.0, || !notifications.pending().is_empty()), "no pending notice");
    let notice = c.fakes.notifications.last();
    ensure!(notice.hints.lookup::<String>("x-tessel-dock-output").unwrap() == Some("HDMI-A-1".into()), "output hint");
    ensure!(notice.hints.lookup::<bool>("x-tessel-dock-workspace").unwrap() == Some(false), "workspace hint");
    ensure!(notice.hints.lookup::<String>("desktop-entry").unwrap() == Some("org.tessel.Capture".into()), "desktop-entry hint");
    ensure!(notice.actions == ["default", "Open", "folder", "Open Folder"], "actions {:?}", notice.actions);
    ensure!(notice.app == "Screen Recorder" && notice.summary == "Recording saved" && notice.body == c.file.to_string_lossy(), "{notice:?}");
    c.fakes.opened.borrow_mut().clear();
    c.fakes.notifications.invoke(notice.id, "folder");
    ensure!(wait(5.0, || !c.fakes.opened.borrow().is_empty()), "folder action not routed");
    ensure!(c.fakes.opened.borrow()[0] == (uri(&c.file), true), "{:?}", c.fakes.opened.borrow());
    ensure!(c.app.window().is_none(), "the capture window was opened");
    c.fakes.notifications.closed(notice.id);
    ensure!(wait(5.0, || notifications.pending().is_empty()), "closing did not release the notice");
    Ok(())
}

fn test_notice_has_buttons_and_routes_saved_uri_without_opening_capture(c: &Context) -> Result<(), String> {
    for (label, reveal) in [("Open", false), ("Open Folder", true)] {
        let start = c.fakes.notifications.count();
        c.app.0.gtk.send_notification(Some("capture"), &results::saved_notification("Screenshot saved", &c.file.to_string_lossy(), &c.file, false));
        ensure!(wait(5.0, || c.fakes.notifications.count() > start), "GNotification not delivered");
        let notice = c.fakes.notifications.last();
        let labels: Vec<&String> = notice.actions.iter().skip(1).step_by(2).collect();
        for expected in ["Open Folder", "Copy", "Edit"] {
            ensure!(labels.iter().any(|l| l.as_str() == expected), "missing {expected}: {:?}", notice.actions);
        }
        let key = if label == "Open" {
            "default".to_string()
        } else {
            notice.actions[notice.actions.iter().position(|a| a == label).unwrap() - 1].clone()
        };
        c.fakes.opened.borrow_mut().clear();
        c.fakes.notifications.invoke(notice.id, &key);
        ensure!(wait(5.0, || !c.fakes.opened.borrow().is_empty()), "{label} not routed ({key}) {:?}", notice.actions);
        ensure!(c.fakes.opened.borrow()[0] == (uri(&c.file), reveal), "{:?}", c.fakes.opened.borrow());
        ensure!(c.app.window().is_none(), "the capture window was opened");
    }
    Ok(())
}

fn test_open_uses_real_default_image_handler(c: &Context) -> Result<(), String> {
    let marker = c.directory.join("opened");
    let viewer = c.script("viewer", &format!("for a in \"$@\"; do last=\"$a\"; done; printf '%s' \"$last\" > '{}'", marker.display()));
    c.default_app("org.tessel.TestImageViewer.desktop", &viewer.to_string_lossy(), "image/png");
    results::open_saved(&c.app.0.gtk, &uri(&c.file), false);
    ensure!(wait(5.0, || marker.exists()), "viewer not started");
    let value = std::fs::read_to_string(&marker).unwrap();
    let opened = if value.starts_with("file:") { gio::File::for_uri(&value) } else { gio::File::for_path(&value) };
    ensure!(opened.path().as_deref() == Some(c.file.as_path()), "{value}");
    ensure!(c.app.window().is_none(), "the capture window was opened");
    Ok(())
}

fn test_screenshot_completion_and_preview_buttons_use_saved_file(c: &Context) -> Result<(), String> {
    let window = c.window();
    window.0.quick_capture.set(true);
    let start = c.fakes.notifications.count();
    let copies = c.wl_copies().len();
    let data = std::fs::read(&c.file).unwrap();
    window.shot_ready((c.file.clone(), data.clone()), false);
    ensure!(wait(5.0, || c.fakes.notifications.count() > start), "no saved notice");
    let notice = c.fakes.notifications.last();
    ensure!(notice.summary == "Screenshot saved and copied" && notice.body == c.file.to_string_lossy(), "{notice:?}");
    // Copying is on by default and needs no visible window.
    ensure!(wait(5.0, || c.wl_copies().len() > copies), "wl-copy not called");
    ensure!(c.wl_copies().last().unwrap() == &format!("--type image/png {}", data.len()), "{:?}", c.wl_copies());
    ensure!(!window.window().is_visible(), "quick capture showed the window");
    c.fakes.opened.borrow_mut().clear();
    window.0.w.open.emit_clicked();
    window.0.w.folder.emit_clicked();
    ensure!(*c.fakes.opened.borrow() == [(uri(&c.file), false), (uri(&c.file), true)], "{:?}", c.fakes.opened.borrow());
    ensure!(window.0.w.preview.get_visible() && window.0.w.result_row.get_visible(), "preview/result row hidden");
    Ok(())
}

fn test_folder_starts_files_and_selects_screenshot(c: &Context) -> Result<(), String> {
    let marker = c.directory.join("files-args");
    let files = c.script("tessel-files", &format!("printf '%s\\n' \"$@\" > '{}'", marker.display()));
    c.default_app("org.tessel.Files.desktop", &files.to_string_lossy(), "inode/directory");
    results::open_saved(&c.app.0.gtk, &uri(&c.file), true);
    ensure!(wait(5.0, || std::fs::read_to_string(&marker).map(|t| t.lines().count() == 2).unwrap_or(false)), "Files not started");
    let args = std::fs::read_to_string(&marker).unwrap();
    ensure!(args == format!("--select\n{}\n", uri(&c.file)), "{args:?}");
    ensure!(c.app.window().is_none(), "the capture window was opened");
    Ok(())
}

fn test_other_file_manager_receives_show_items(c: &Context) -> Result<(), String> {
    c.default_app("org.tessel.OtherFiles.desktop", "/usr/bin/false", "inode/directory");
    c.fakes.shown.borrow_mut().clear();
    results::open_saved(&c.app.0.gtk, &uri(&c.file), true);
    ensure!(wait(5.0, || !c.fakes.shown.borrow().is_empty()), "ShowItems not called");
    ensure!(c.fakes.shown.borrow()[0] == [uri(&c.file)], "{:?}", c.fakes.shown.borrow());
    ensure!(c.app.window().is_none(), "the capture window was opened");
    Ok(())
}

fn test_missing_capture_reports_error_without_opening_controls(c: &Context) -> Result<(), String> {
    let missing = c.directory.join("missing.png");
    c.fakes.intercept_open.set(false);
    let start = c.fakes.notifications.count();
    c.app.0.gtk.activate_action("open-saved", Some(&uri(&missing).to_variant()));
    ensure!(wait(5.0, || c.fakes.notifications.count() > start), "no error notice");
    let notice = c.fakes.notifications.last();
    ensure!(notice.summary == "Could not open saved capture", "{notice:?}");
    ensure!(notice.body == format!("The saved capture no longer exists: {}", missing.display()), "{notice:?}");
    ensure!(c.app.window().is_none(), "the capture window was opened");
    Ok(())
}

fn test_screenshot_notice_copies_and_edits_saved_file(c: &Context) -> Result<(), String> {
    let notifications = c.app.0.notifications.borrow().clone().unwrap();
    notifications.saved("Screenshot saved", &c.file, "", false, false);
    ensure!(wait(5.0, || c.fakes.notifications.last().summary == "Screenshot saved"), "no notice");
    let notice = c.fakes.notifications.last();
    ensure!(notice.actions == ["default", "Open", "folder", "Open Folder", "copy", "Copy", "edit", "Edit"], "{:?}", notice.actions);
    let copies = c.wl_copies().len();
    c.fakes.notifications.invoke(notice.id, "copy");
    ensure!(wait(5.0, || c.wl_copies().len() > copies), "copy not routed");
    let copied = std::fs::read(c.fakes.scratch.join("wl-copy.last")).unwrap();
    ensure!(copied.starts_with(b"\x89PNG") && tessel_capture::imaging::decode_rgb(&copied).map(|i| i.dimensions()) == Ok((4, 4)), "copied data");
    ensure!(c.app.window().is_none(), "copying opened the capture window");
    c.app.0.gtk.activate_action("edit-saved", Some(&uri(&c.file).to_variant()));
    let editors = editor::editors();
    ensure!(editors.len() == 1, "{} editors", editors.len());
    ensure!(c.app.window().is_none(), "editing opened the capture controls");
    c.fakes.notifications.invoke(notice.id, "edit");
    pump(0.2);
    ensure!(editor::editors().len() == 1, "a second editor opened for the same file");
    editors[0].0.window.destroy();
    pump(0.05);
    ensure!(editor::editors().is_empty(), "editor not unregistered");
    c.fakes.notifications.closed(notice.id);
    Ok(())
}

fn test_settings_merge_into_shared_config_and_editor_renders(c: &Context) -> Result<(), String> {
    let config = c.directory.join("config").join("config.json");
    ensure!(settings::copy_enabled(Some(&config)) && !settings::editor_enabled(Some(&config)), "defaults");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, r#"{"delay": 3}"#).unwrap();
    settings::set_value("copy_to_clipboard", false, Some(&config))?;
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    ensure!(value == serde_json::json!({"delay": 3, "copy_to_clipboard": false}), "{value}");
    let page = settings::build_page(Some(config.clone()));
    ensure!(!page.switches["copy_to_clipboard"].is_active(), "switch state");
    page.switches["open_editor"].set_active(true);
    ensure!(settings::editor_enabled(Some(&config)), "switch did not write");
    let white = image::RgbImage::from_pixel(40, 30, image::Rgb([255, 255, 255]));
    let file = c.directory.join("edit.png");
    std::fs::write(&file, tessel_capture::imaging::png_rgb_with_icc(&white)?).unwrap();
    let mut text = editor::Operation::new("text", (0.0, 0.0, 0.0, 1.0), 2, vec![(4.0, 28.0)]);
    text.text = "Hi".into();
    let operations = vec![
        editor::Operation::new("rectangle", (1.0, 0.0, 0.0, 1.0), 2, vec![(2.0, 2.0), (20.0, 20.0)]),
        editor::Operation::new("arrow", (0.0, 0.0, 1.0, 1.0), 2, vec![(5.0, 25.0), (35.0, 5.0)]),
        editor::Operation::new("pen", (0.0, 1.0, 0.0, 1.0), 3, vec![(1.0, 1.0), (3.0, 9.0), (9.0, 3.0)]),
        text,
        editor::Operation::new("crop", (0.0, 0.0, 0.0, 0.0), 0, vec![(1.0, 1.0), (31.0, 21.0)]),
    ];
    let window = editor::EditorWindow::new(&c.app.0.gtk, &file)?;
    window.set_operations(operations);
    ensure!(window.save(None), "save failed: {}", window.0.status.text());
    ensure!(png_size(&file) == (30, 20), "{:?}", png_size(&file));
    use std::os::unix::fs::PermissionsExt;
    ensure!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777 == 0o600, "mode");
    window.undo();
    ensure!(window.0.operations.borrow().len() == 4, "undo");
    window.0.window.present();
    pump(0.2);
    window.set_tool("rectangle");
    window.begin(10.0, 10.0);
    window.update(30.0, 20.0);
    window.end(30.0, 20.0);
    ensure!(window.0.operations.borrow().len() == 5, "drag did not add an operation");
    window.set_tool("text");
    window.begin(5.0, 5.0);
    ensure!(window.0.status.text().contains("Type the text first"), "{}", window.0.status.text());
    window.0.window.destroy();
    pump(0.05);
    Ok(())
}

fn test_capture_window_has_standard_menu_and_quit(c: &Context) -> Result<(), String> {
    let window = c.window();
    let menu = window.0.w.menu_button.borrow().clone().unwrap();
    ensure!(menu.ancestor(gtk::HeaderBar::static_type()).as_ref() == Some(window.0.w.header.upcast_ref()), "menu not in header");
    ensure!(window.preferences().contains_key("copy_to_clipboard"), "preferences");
    ensure!(c.app.0.gtk.lookup_action("quit").is_some(), "quit action");
    window.0.busy.set(true);
    window.quit_requested();
    ensure!(window.0.w.status.text().contains("Stop the current capture"), "{}", window.0.w.status.text());
    window.0.busy.set(false);
    ensure!(window.window().has_css_class("capture-window"), "class");
    ensure!(window.0.w.mode.root.has_css_class("capture-mode") && window.0.w.mode.root.has_css_class("linked"), "mode classes");
    ensure!(window.0.w.status.has_css_class("record-status") && window.0.w.preview.has_css_class("preview"), "status/preview classes");
    ensure!(window.window().title().as_deref() == Some("Screenshot"), "{:?}", window.window().title());
    window.0.w.mode.set_selected(1);
    ensure!(window.window().title().as_deref() == Some("Screen Recorder") && window.0.w.start.label().as_deref() == Some("Start recording"), "mode");
    ensure!(window.0.w.target.model().unwrap().n_items() == 3 && window.0.w.target.selected() == 0, "recording targets");
    window.0.w.mode.set_selected(0);
    ensure!(window.0.w.target.model().unwrap().n_items() == 4 && window.0.w.target.selected() == 1, "screenshot targets");
    window.0.w.color.set_selected(1);
    ensure!(window.0.w.codec.selected() == 1, "HDR must switch H.264 to H.265");
    window.0.w.color.set_selected(0);
    Ok(())
}

fn test_preferences_round_trip_and_legacy_encoder_migration(c: &Context) -> Result<(), String> {
    let window = c.window();
    let config = c.directory.join("preferences.json");
    *window.0.config_path.borrow_mut() = config.clone();
    for old in [2, 3] {
        std::fs::write(&config, serde_json::json!({"encoder": old, "fps": 60}).to_string()).unwrap();
        window.load_preferences();
        ensure!(window.0.w.encoder.selected() == 2 && window.0.w.fps.value_as_int() == 60, "legacy encoder {old}");
        ensure!(window.preferences()["encoder_backend"] == "vulkan", "backend name");
    }
    std::fs::write(&config, serde_json::json!({"encoder": 3, "encoder_backend": "cpu"}).to_string()).unwrap();
    window.load_preferences();
    ensure!(window.0.w.encoder.selected() == 1, "A named backend must take precedence over old indices");
    window.save_preferences();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    let keys: Vec<&String> = saved.as_object().unwrap().keys().collect();
    ensure!(keys == ["copy_to_clipboard", "open_editor", "mode_preferences", "audio", "codec", "color", "encoder", "quality", "container",
                     "format", "delay", "fps", "peak", "bitrate", "duration", "cursor", "encoder_backend"], "{keys:?}");
    ensure!(saved["mode_preferences"]["screenshot"]["target"] == 1, "{saved}");
    ensure!(std::fs::read_to_string(&config).unwrap().starts_with("{\n  \"copy_to_clipboard\": "), "indent");
    // pactl fake: one monitor and one microphone source.
    ensure!(wait(3.0, || window.0.system_sources.borrow().len() == 1), "audio sources");
    ensure!(window.0.w.mic_audio.model().unwrap().n_items() == 2, "microphones");
    Ok(())
}

fn test_shortcuts_apply_in_private_config_reloads_labwc(c: &Context) -> Result<(), String> {
    let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap());
    let rc = root.join("labwc/rc.xml");
    std::fs::create_dir_all(rc.parent().unwrap()).unwrap();
    std::fs::write(&rc, "<labwc_config><keyboard><default/></keyboard></labwc_config>\n").unwrap();
    let window = c.window();
    let entries = window.0.w.shortcut_entries.borrow().clone();
    let texts: Vec<String> = entries.iter().map(|(_, e)| e.text().to_string()).collect();
    ensure!(texts == ["Alt+F9", "Print", "Ctrl+Print", "Shift+Print", "Alt+Print"], "{texts:?}");
    entries[0].1.set_text("ctrl+alt+r");
    window.apply_shortcuts();
    ensure!(window.0.w.status.text() == "Keyboard shortcuts updated.", "{}", window.0.w.status.text());
    ensure!(entries[0].1.text() == "Ctrl+Alt+r", "{}", entries[0].1.text());
    let text = std::fs::read_to_string(&rc).unwrap();
    ensure!(text.contains("<keybind key=\"C-A-r\"><action name=\"Execute\"><command>/usr/local/bin/tessel-capture --toggle-recording</command>"), "{text}");
    let log = std::fs::read_to_string(c.fakes.scratch.join("calls.log")).unwrap_or_default();
    ensure!(log.lines().any(|l| l == "labwc --reconfigure"), "{log}");
    let help = tessel_capture::ui::shortcut_help();
    ensure!(help[0].1.iter().any(|s| s.keys[0] == "Ctrl+Alt+r" && s.label == "Start / stop recording"), "menu shortcuts");
    entries[1].1.set_text("Ctrl+Alt+r");
    window.apply_shortcuts();
    ensure!(window.0.w.status.has_css_class("error") && window.0.w.status.text() == "Each capture action needs a different shortcut.", "{}",
            window.0.w.status.text());
    ensure!(std::fs::read_to_string(&rc).unwrap() == text, "a rejected apply changed rc.xml");
    let _ = std::fs::remove_file(root.join("desktop-capture/shortcuts.json"));
    Ok(())
}

fn run_binary(args: &[&str]) -> (i32, String, String) {
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_tessel-capture")).args(args).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()).spawn().unwrap();
    let shared = Rc::new(RefCell::new(Some(child)));
    let mut status = None;
    wait(10.0, || {
        status = shared.borrow_mut().as_mut().unwrap().try_wait().unwrap();
        status.is_some()
    });
    let mut child = shared.borrow_mut().take().unwrap();
    if status.is_none() {
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into(), String::from_utf8_lossy(&output.stderr).into())
}

fn test_command_line_binary_contract(_c: &Context) -> Result<(), String> {
    let (code, stdout, _) = run_binary(&["--check"]);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| format!("{e}: {stdout}"))?;
    ensure!(value.get("ready").is_some_and(|v| v.is_boolean()) && stdout.starts_with("{\"ready\": "), "{stdout}");
    ensure!(code == if value["ready"] == true { 0 } else { 1 }, "exit {code}");
    let (code, _, stderr) = run_binary(&["--capture", "nowhere"]);
    ensure!(code == 2 && stderr.contains("invalid choice: 'nowhere'"), "{code} {stderr}");
    let (code, stdout, _) = run_binary(&["--help"]);
    ensure!(code == 0 && stdout.contains("Portal-backed Screenshot and Screen Recorder."), "{stdout}");
    let (code, _, _) = run_binary(&["--stop"]);
    ensure!(code == 1, "--stop while idle (forwarded to this primary) must fail: {code}");
    Ok(())
}

/// Run a worker-thread call while this thread keeps serving the fake portal.
fn in_worker<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let thread = std::thread::spawn(f);
    wait(10.0, || thread.is_finished());
    thread.join().unwrap()
}

fn authorized(c: &Context, types: u32) -> Result<PortalSession, String> {
    let ready = Rc::new(Cell::new(false));
    let errors = Rc::new(RefCell::new(Vec::new()));
    let (r, e) = (ready.clone(), errors.clone());
    let session = PortalSession::new(move |_| r.set(true), move |_, message| e.borrow_mut().push(message),
                                     portal::Options { types, bus: c.app.0.gtk.dbus_connection(), ..Default::default() });
    ensure!(wait(10.0, || ready.get() || !errors.borrow().is_empty()), "portal did not answer");
    ensure!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    Ok(session)
}

fn test_portal_cancel_and_immediate_close(c: &Context) -> Result<(), String> {
    c.fakes.portal.state.cancel.set(true);
    let ready = Rc::new(Cell::new(false));
    let errors = Rc::new(RefCell::new(Vec::<String>::new()));
    let (r, e) = (ready.clone(), errors.clone());
    let session = PortalSession::new(move |_| r.set(true), move |_, m| e.borrow_mut().push(m),
                                     portal::Options { bus: c.app.0.gtk.dbus_connection(), ..Default::default() });
    ensure!(wait(5.0, || !errors.borrow().is_empty()), "no cancel");
    ensure!(*errors.borrow() == ["Screen access was cancelled."], "{:?}", errors.borrow());
    ensure!(!ready.get() && session.closed() && session.cancelled() && !session.has_fd(), "cancelled state");
    let handle = session.session_handle().unwrap();
    ensure!(wait(2.0, || c.fakes.portal.state.session_closes.borrow().contains(&handle)), "Session.Close not called");
    let (r, e) = (ready.clone(), errors.clone());
    let second = PortalSession::new(move |_| r.set(true), move |_, m| e.borrow_mut().push(m),
                                    portal::Options { bus: c.app.0.gtk.dbus_connection(), ..Default::default() });
    second.close();
    pump(0.3);
    ensure!(second.closed() && !second.has_fd() && !ready.get() && errors.borrow().len() == 1, "immediate close");
    c.fakes.portal.state.cancel.set(false);
    Ok(())
}

fn test_portal_frame_and_window_source(c: &Context) -> Result<(), String> {
    let session = authorized(c, 1)?;
    ensure!(session.streams()[0].1.source_type == Some(1) && session.has_fd(), "{:?}", session.streams());
    let remote = session.remote().unwrap();
    let before = c.fakes.portal.state.remotes.get();
    let fd = in_worker(move || remote.remote_fd())?;
    ensure!(c.fakes.portal.state.remotes.get() == before + 1, "remote_fd must request a fresh fd");
    drop(fd);
    let state = c.fakes.portal.state.sessions.borrow().get(&session.session_handle().unwrap()).cloned().unwrap();
    ensure!(state.types == 1 && state.cursor_mode == 2, "SelectSources options {} {}", state.types, state.cursor_mode);
    let frame = Frame::new(32, 24, [0u8, 0, 255, 0].repeat(32 * 24)).format("bgr0");
    ensure!(media::screenshot(&frame, None, 1000)?.starts_with(b"\x89PNG"), "png");
    session.close();
    let window = authorized(c, 2)?;
    ensure!(window.streams()[0].1.source_type == Some(2) && window.streams()[0].1.size.unwrap().0 < 500, "{:?}", window.streams());
    window.close();
    let remote = window.remote().unwrap();
    ensure!(in_worker(move || remote.remote_fd()).is_err(), "closed sessions must refuse new fds");
    Ok(())
}

fn test_shortcut_dispatch_never_presents_controls(c: &Context) -> Result<(), String> {
    let window = c.window();
    c.fakes.portal.state.cancel.set(true);
    let start = c.fakes.notifications.count();
    for mode in ["workspace", "screen", "window", "area"] {
        let status = c.app.command_line(&option(|o| o.capture = Some(mode.into())));
        ensure!(status == 0, "{mode}: status {status}");
        ensure!(window.0.quick_capture.get() && window.0.busy.get(), "{mode}: not started");
        ensure!(*window.0.kind.borrow() == mode, "{mode}: kind {}", window.0.kind.borrow());
        ensure!(!window.window().is_visible(), "{mode}: shown");
        ensure!(c.app.command_line(&option(|o| o.capture = Some(mode.into()))) == 1, "busy must refuse");
        window.stop_clicked();
        ensure!(!window.window().is_visible(), "Cancelling a shortcut must keep the app hidden");
        window.fail("Test capture error");
        ensure!(!window.window().is_visible(), "Shortcut errors must not open the full app");
    }
    ensure!(wait(3.0, || c.fakes.notifications.count() >= start + 4), "notifications");
    let failures: Vec<Notice> = c.fakes.notifications.since(start).into_iter().filter(|n| n.summary == "Capture failed").collect();
    ensure!(failures.len() == 4 && failures.iter().all(|n| n.body == "Test capture error"), "{failures:?}");
    let dock = c.fakes.dock.borrow().clone();
    ensure!(dock.iter().any(|(name, values)| name == "prepare-capture" && values[0].get::<bool>() == Some(false)), "{dock:?}");
    c.app.command_line(&option(|o| o.record = true));
    ensure!(window.window().is_visible(), "The settings command must open the full controls");
    ensure!(!window.0.quick_capture.get() && window.recording(), "--record");
    c.app.command_line(&option(|o| o.screenshot = true));
    ensure!(!window.recording(), "--screenshot");
    Ok(())
}

fn test_screenshot_shortcut_preserves_open_recorder_and_cancel_is_silent(c: &Context) -> Result<(), String> {
    c.app.activate();
    let window = c.app.window().unwrap();
    let shots = c.directory.join("shots");
    window.0.w.directory.set_text(&shots.to_string_lossy());
    window.0.w.mode.set_selected(1);
    window.0.w.directory.set_text(&c.directory.join("videos").to_string_lossy());
    pump(0.2);
    let _ = std::process::Command::new("wlrctl").args(["pointer", "move", "5", "5"]).status();
    let menu = gtk::Popover::builder().autohide(true).build();
    menu.set_parent(&window.0.w.start);
    menu.set_child(Some(&gtk::Label::new(Some("Open capture controls"))));
    menu.popup();
    pump(0.2);
    ensure!(menu.is_visible(), "popover not shown");
    let notices = c.fakes.notifications.count();
    ensure!(c.app.command_line(&option(|o| o.capture = Some("screen".into()))) == 0, "status");
    ensure!(window.window().is_visible(), "window hidden");
    ensure!(window.recording(), "The displayed recorder settings must not change to screenshot mode");
    ensure!(!window.0.capture_recording.get(), "This operation must take a screenshot, not start video");
    ensure!(!menu.is_visible(), "Capture must release its own popup grab too");
    ensure!(wait(15.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    let last = window.0.last_file.borrow().clone();
    ensure!(last.as_ref().and_then(|p| p.parent()) == Some(shots.as_path()), "{last:?} / {}", window.0.w.status.text());
    ensure!(window.window().is_visible() && window.0.w.mode.selected() == 1, "mode/visibility changed");
    ensure!(window.0.w.directory.text().as_str() == c.directory.join("videos").to_string_lossy(), "directory changed");
    ensure!(c.fakes.dock.borrow().iter().any(|(n, _)| n == "finish-screenshot"), "dock popup not restored");
    ensure!(wait(3.0, || c.fakes.notifications.count() > notices), "no saved notice");
    let notice = c.fakes.notifications.last();
    ensure!(notice.summary == "Screenshot saved and copied", "{notice:?}");
    let screen = notice.hints.lookup::<String>("x-tessel-dock-output").unwrap().unwrap_or_default();
    ensure!(c.fakes.portal.monitors.iter().any(|(n, _)| *n == screen), "output hint {screen:?}");
    menu.unparent();
    // The portal's Cancel response is distinct from an actual capture error.
    window.window().set_visible(false);
    c.fakes.portal.state.cancel.set(true);
    let notices = c.fakes.notifications.count();
    c.app.command_line(&option(|o| o.capture = Some("window".into())));
    ensure!(wait(15.0, || !window.0.busy.get()), "window capture did not end");
    ensure!(c.fakes.notifications.count() == notices, "cancel notified");
    ensure!(!window.window().is_visible(), "cancel showed the window");
    ensure!(window.0.w.status.text().to_lowercase().contains("cancelled"), "{}", window.0.w.status.text());
    // Cancellation from the main capture button must also stay silent.
    c.app.activate();
    window.0.w.mode.set_selected(0);
    window.0.w.target.set_selected(2);
    window.0.w.start.emit_clicked();
    ensure!(wait(15.0, || !window.0.busy.get()), "button capture did not end");
    ensure!(!window.window().is_visible() && c.fakes.notifications.count() == notices, "cancel from the button");
    Ok(())
}

fn test_ui_screenshot_and_area_selection(c: &Context) -> Result<(), String> {
    c.app.activate();
    let window = c.app.window().unwrap();
    pump(0.2);
    window.0.w.directory.set_text(&c.directory.to_string_lossy());
    window.0.w.target.set_selected(1);
    let _ = std::process::Command::new("wlrctl").args(["pointer", "move", "5", "5"]).status();
    window.prepare(false, None, true);
    ensure!(wait(12.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    let last = window.0.last_file.borrow().clone().ok_or_else(|| window.0.w.status.text().to_string())?;
    ensure!(last.is_file() && window.0.w.preview.get_visible() && window.0.sessions.borrow().is_empty(), "result state");
    use std::os::unix::fs::PermissionsExt;
    ensure!(std::fs::metadata(&last).unwrap().permissions().mode() & 0o777 == 0o600, "private mode");
    let data = std::fs::read(&last).unwrap();
    ensure!(tessel_capture::imaging::icc_profile(&data).is_some_and(|p| p.len() == 588), "ICC profile");
    ensure!(window.window().is_visible(), "the window must be presented after a normal capture");
    ensure!(window.0.w.status.text() == format!("Saved {}", last.display()), "{}", window.0.w.status.text());
    let display = WidgetExt::display(window.window());
    let monitor = display.monitors().item(0).and_downcast::<gdk::Monitor>().unwrap();
    let selected = Rc::new(RefCell::new(None));
    let sink = selected.clone();
    let picker = area::ScreenSelection::new(&display, move |r| *sink.borrow_mut() = Some(r), true)?;
    pump(0.2);
    ensure!(picker.windows().len() == 2 && picker.windows().iter().all(|w| w.is_visible() && w.has_css_class("capture-selection")), "overlays");
    picker.begin(&monitor, 100.0, 100.0);
    picker.drag(-60.0, 80.0);
    picker.end(-60.0, 80.0);
    let result = selected.borrow_mut().take().flatten();
    ensure!(result.as_ref().and_then(|r| r.1) == Some((40.0, 100.0, 60.0, 80.0)), "{:?}", result.map(|r| r.1));
    ensure!(picker.windows().iter().all(|w| !w.is_visible()), "overlays not removed");
    // JPEG format embeds the same profile.
    window.0.w.format.set_selected(1);
    window.prepare(false, None, true);
    ensure!(wait(12.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    let jpeg = window.0.last_file.borrow().clone().unwrap();
    ensure!(jpeg.extension().unwrap() == "jpg", "{jpeg:?}");
    ensure!(tessel_capture::imaging::icc_profile(&std::fs::read(&jpeg).unwrap()).is_some_and(|p| p.len() == 588), "JPEG ICC");
    window.0.w.format.set_selected(0);
    Ok(())
}

fn test_direct_monitor_area_and_delay(c: &Context) -> Result<(), String> {
    c.app.activate();
    let window = c.app.window().unwrap();
    window.0.w.directory.set_text(&c.directory.to_string_lossy());
    window.0.w.delay.set_value(1.0);
    let (second, geometry) = c.fakes.portal.monitors[1].clone();
    let _ = std::process::Command::new("wlrctl").args(["pointer", "move", "-100000", "-100000"]).status();
    let _ = std::process::Command::new("wlrctl").args(["pointer", "move", &(geometry.x() + 80).to_string(), &(geometry.y() + 90).to_string()]).status();
    // Current screen (saved preferences of earlier checks may have chosen another target).
    window.0.w.target.set_selected(1);
    pump(0.1);
    let unhandled = c.fakes.portal.state.unhandled.get();
    let opened = c.fakes.feeds.lock().unwrap().opened.len();
    c.fakes.portal.state.chosen.borrow_mut().clear();
    let started = Instant::now();
    window.prepare(false, None, true);
    ensure!(wait(12.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    ensure!(window.0.last_file.borrow().is_some(), "{}", window.0.w.status.text());
    let first_open = c.fakes.feeds.lock().unwrap().opened[opened].1;
    ensure!(first_open.duration_since(started) >= Duration::from_secs(1), "feeds opened before the delay");
    let chosen: Vec<String> = c.fakes.portal.state.chosen.borrow().values().cloned().collect();
    ensure!(chosen == [second.clone()], "{chosen:?} vs {second}");
    ensure!(c.fakes.portal.state.unhandled.get() == unhandled, "the chooser was not preselected");
    ensure!(c.fakes.notifications.last().hints.lookup::<String>("x-tessel-dock-output").unwrap() == Some(second.clone()), "output hint");
    let previous = window.0.last_file.borrow().clone();
    window.0.w.target.set_selected(3);
    window.prepare(false, None, true);
    ensure!(wait(3.0, || window.selector().is_some()), "no area selector");
    let picker = window.selector().unwrap();
    let monitor = picker.0.windows.borrow().iter().map(|(_, _, m)| m.clone()).find(|m| m.connector().as_deref() == Some(second.as_str())).unwrap();
    picker.begin(&monitor, 20.0, 30.0);
    picker.end(90.0, 70.0);
    let released = Instant::now();
    pump(0.6);
    ensure!(*window.0.last_file.borrow() == previous, "Area capture must wait after mouse release");
    ensure!(wait(12.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    ensure!(released.elapsed() >= Duration::from_secs(1), "delay after release");
    let last = window.0.last_file.borrow().clone();
    ensure!(last != previous && last.is_some(), "{}", window.0.w.status.text());
    ensure!(png_size(last.as_ref().unwrap()) == (90, 70), "{:?}", png_size(last.as_ref().unwrap()));
    ensure!(c.fakes.portal.state.unhandled.get() == unhandled, "area chooser not preselected");
    // Escape/cancel discards the outline without capturing.
    let previous = window.0.last_file.borrow().clone();
    window.prepare(false, None, true);
    ensure!(wait(3.0, || window.selector().is_some()), "no selector");
    window.selector().unwrap().key(gdk::Key::Escape);
    ensure!(!window.0.busy.get() && *window.0.last_file.borrow() == previous, "escape");
    window.0.w.delay.set_value(0.0);
    Ok(())
}

fn test_workspace_and_armed_capture_reopen_without_new_consent(c: &Context) -> Result<(), String> {
    c.app.activate();
    let window = c.app.window().unwrap();
    window.0.w.directory.set_text(&c.directory.to_string_lossy());
    window.0.w.target.set_selected(0);
    window.prepare(false, None, true);
    ensure!(wait(20.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    let last = window.0.last_file.borrow().clone().ok_or_else(|| window.0.w.status.text().to_string())?;
    let monitors = &c.fakes.portal.monitors;
    let width = monitors.iter().map(|(_, g)| g.x() + g.width()).max().unwrap() - monitors.iter().map(|(_, g)| g.x()).min().unwrap();
    let height = monitors.iter().map(|(_, g)| g.y() + g.height()).max().unwrap() - monitors.iter().map(|(_, g)| g.y()).min().unwrap();
    ensure!(png_size(&last) == (width as u32, height as u32), "{:?} vs {width}x{height}", png_size(&last));
    let notice = c.fakes.notifications.last();
    ensure!(notice.hints.lookup::<bool>("x-tessel-dock-workspace").unwrap() == Some(true), "workspace hint");
    window.prepare(true, None, true);
    ensure!(wait(15.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    ensure!(window.0.armed_sources.borrow().len() == 2, "{}", window.0.w.status.text());
    ensure!(window.0.feeds.borrow().is_empty(), "Armed capture must not keep processing full-resolution video");
    ensure!(window.0.w.arm.label().as_deref() == Some("Disable Print Screen while locked"), "arm label");
    let state = c.app.0.tray.borrow().as_ref().unwrap().state();
    ensure!(state == ("idle".into(), true), "{state:?}");
    let cursor_modes: Vec<u32> = c.fakes.portal.state.sessions.borrow().values().map(|s| s.cursor_mode).collect();
    ensure!(cursor_modes.contains(&1), "arming must hide the pointer: {cursor_modes:?}");
    let choices = c.fakes.portal.state.chooser_count.get();
    let remotes = c.fakes.portal.state.remotes.get();
    let old = last;
    *window.0.kind.borrow_mut() = "window".into();
    *window.0.notification_output.borrow_mut() = "previous-screen".into();
    let notices = c.fakes.notifications.count();
    let copies = c.wl_copies().len();
    // Lock screen contract: org.gtk.Actions.Activate("lock-shot", [<d>], {}) on /org/tessel/Capture.
    let locked_at = compat::monotonic();
    let parameters = glib::Variant::tuple_from_iter(["lock-shot".to_variant(),
        glib::Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, [locked_at.to_variant().to_variant()]),
        glib::VariantDict::new(None).end()]);
    c.fakes.connection.call(Some("org.tessel.Capture"), "/org/tessel/Capture", "org.gtk.Actions", "Activate", Some(&parameters), None,
                            gio::DBusCallFlags::NO_AUTO_START, 1500, None::<&gio::Cancellable>, |_| {});
    ensure!(wait(3.0, || window.0.busy.get()), "lock-shot not received");
    ensure!(*window.0.kind.borrow() == "workspace" && window.0.notification_output.borrow().is_empty(), "lock-shot state");
    ensure!(wait(15.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    let last = window.0.last_file.borrow().clone();
    ensure!(last.is_some() && last.as_ref() != Some(&old), "{}", window.0.w.status.text());
    ensure!(c.fakes.portal.state.chooser_count.get() == choices, "Reopen must not launch a chooser while locked");
    ensure!(c.fakes.portal.state.remotes.get() == remotes + 2, "each armed source needs a fresh restricted fd");
    let not_before: Vec<Option<f64>> = c.fakes.feeds.lock().unwrap().not_before.iter().rev().take(2).cloned().collect();
    ensure!(not_before.iter().all(|n| *n == Some(locked_at)), "{not_before:?} vs {locked_at}");
    pump(0.3);
    ensure!(c.fakes.notifications.count() == notices && c.wl_copies().len() == copies, "lock shots stay off notifications/clipboard");
    // The CLI from a second process reaches this primary instance.
    let (code, _, _) = run_binary(&["--lock-shot"]);
    ensure!(code == 0, "--lock-shot exit {code}");
    ensure!(wait(15.0, || !window.0.busy.get()), "cli lock shot");
    let sessions: Vec<String> = window.0.armed_sessions.borrow().iter().filter_map(|s| s.session_handle()).collect();
    window.disarm();
    ensure!(window.0.armed_sources.borrow().is_empty(), "disarm");
    ensure!(wait(2.0, || sessions.iter().all(|s| c.fakes.portal.state.session_closes.borrow().contains(s))), "armed sessions not closed");
    let (code, _, _) = run_binary(&["--lock-shot"]);
    ensure!(code == 1, "--lock-shot when disarmed: {code}");
    Ok(())
}

fn get_state(c: &Context) -> Option<(String, bool)> {
    let result = Rc::new(RefCell::new(None));
    let sink = result.clone();
    c.fakes.connection.call(Some("org.tessel.Capture"), "/org/tessel/Capture/Status", "org.tessel.Capture.Status", "GetState", None,
        Some(glib::VariantTy::new("(sb)").unwrap()), gio::DBusCallFlags::NO_AUTO_START, 2000, None::<&gio::Cancellable>, move |reply| {
            *sink.borrow_mut() = Some(reply.ok().and_then(|r| r.get::<(String, bool)>()));
        });
    wait(3.0, || result.borrow().is_some());
    let value = result.borrow().clone().flatten();
    value
}

fn test_ui_recording_finishes_a_portal_stream(c: &Context) -> Result<(), String> {
    let states = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = states.clone();
    let _subscription = c.fakes.connection.subscribe_to_signal(None, Some("org.tessel.Capture.Status"), Some("Changed"),
        Some("/org/tessel/Capture/Status"), None, gio::DBusSignalFlags::NONE, move |signal| {
            sink.borrow_mut().push(signal.parameters.child_value(0).str().unwrap().to_string());
        });
    c.app.activate();
    let window = c.app.window().unwrap();
    window.0.w.mode.set_selected(1);
    window.0.w.encoder.set_selected(1);
    window.0.w.duration.set_value(0.0);
    window.0.w.fps.set_value(10.0);
    let videos = c.directory.join("videos");
    window.0.w.directory.set_text(&videos.to_string_lossy());
    let notices = c.fakes.notifications.count();
    window.0.w.start.emit_clicked();
    ensure!(wait(20.0, || window.0.elapsed_timer.borrow().is_some() && states.borrow().contains(&"recording".into())),
            "{} {:?}", window.0.w.status.text(), states.borrow());
    ensure!(!window.window().is_visible(), "recording must hide the controls");
    ensure!(states.borrow().contains(&"preparing".into()), "{:?}", states.borrow());
    // Restarting the panel must discover an already running recording.
    ensure!(get_state(c).map(|s| s.0) == Some("recording".into()), "GetState");
    ensure!(window.0.w.status.text().starts_with("Recording · "), "{}", window.0.w.status.text());
    window.window().close();
    ensure!(window.0.recorder.borrow().is_some(), "Closing the controls must not stop recording");
    pump(1.2);
    ensure!(c.app.command_line(&option(|o| o.toggle_recording = true)) == 0, "toggle");
    ensure!(wait(3.0, || states.borrow().contains(&"stopping".into())), "{:?}", states.borrow());
    ensure!(wait(20.0, || !window.0.busy.get() && states.borrow().last().map(String::as_str) == Some("idle")),
            "{} {:?}", window.0.w.status.text(), states.borrow());
    ensure!(!window.window().is_visible(), "Saving must not reopen the controls");
    let last = window.0.last_file.borrow().clone().ok_or_else(|| window.0.w.status.text().to_string())?;
    ensure!(last.is_file() && last.extension().unwrap() == "mkv" && last.parent() == Some(videos.as_path()), "{last:?}");
    ensure!(!std::fs::read_dir(&videos).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".recording-")), "temp left");
    use std::os::unix::fs::PermissionsExt;
    ensure!(std::fs::metadata(&last).unwrap().permissions().mode() & 0o077 == 0, "recordings are private");
    ensure!(wait(3.0, || c.fakes.notifications.since(notices).iter().any(|n| n.summary == "Recording saved")), "no notice");
    let saved: Vec<Notice> = c.fakes.notifications.since(notices).into_iter().filter(|n| n.summary == "Recording saved").collect();
    ensure!(saved.len() == 1 && saved[0].body == last.to_string_lossy() && saved[0].actions.len() == 4, "{saved:?}");
    ensure!(window.0.recorder.borrow().is_none() && window.0.sessions.borrow().is_empty(), "cleanup");
    let first = last;
    // The quick preset uses the saved output settings and stays hidden too.
    window.0.w.duration.set_value(1.0);
    let status = c.app.command_line(&option(|o| {
        o.start_recording = true;
        o.audio_preset = Some("none".into());
    }));
    ensure!(status == 0, "start-recording");
    ensure!(wait(3.0, || window.0.busy.get()), "not started");
    ensure!(wait(20.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    ensure!(window.0.last_file.borrow().as_ref() != Some(&first), "{}", window.0.w.status.text());
    ensure!(window.0.capture_options.borrow().audio.is_empty() && !window.window().is_visible(), "preset");
    c.app.activate();
    let notices = c.fakes.notifications.count();
    ensure!(c.app.command_line(&option(|o| o.toggle_recording = true)) == 0, "toggle start");
    ensure!(window.window().is_visible(), "The recording hotkey preserves an already-open window");
    ensure!(wait(3.0, || window.0.busy.get()), "not started");
    ensure!(wait(20.0, || !window.0.busy.get()), "{}", window.0.w.status.text());
    ensure!(window.window().is_visible(), "still visible");
    ensure!(wait(3.0, || c.fakes.notifications.since(notices).iter().any(|n| n.summary == "Recording saved")), "no saved notice");
    Ok(())
}

// ---------------------------------------------------------------- main

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if std::env::var("CAPTURE_UI_ISOLATED_TEST").as_deref() != Ok("1") || runtime == "/run/user/1000" || runtime.is_empty()
        || std::env::var("WAYLAND_DISPLAY").map(|d| d.is_empty()).unwrap_or(true) {
        println!("ui_headless: skipped (run tests/run_headless.sh for the private headless compositor)");
        return;
    }
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    tessel_core::i18n::initialize();
    gtk::init().expect("GTK in the private compositor");
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("private session bus");
    let connection = connect(&address);
    let display = gdk::Display::default().unwrap();
    let monitors: Vec<(String, gdk::Rectangle)> = (0..display.monitors().n_items())
        .filter_map(|i| display.monitors().item(i).and_downcast::<gdk::Monitor>())
        .map(|m| (m.connector().unwrap().to_string(), m.geometry())).collect();
    assert_eq!(monitors.len(), 2, "run_headless.sh starts two outputs");
    let portal = FakePortal::new(&connection, monitors);
    let notifications = FakeNotifications::new(&connection);
    let dock = Rc::new(RefCell::new(Vec::new()));
    let sink = dock.clone();
    connection.register_object("/org/tessel/Tessel Dock", &interface(ACTIONS_XML, "org.gtk.Actions"))
        .method_call(move |_, _, _, _, _, parameters, invocation| {
            let name = parameters.child_value(0).str().unwrap().to_string();
            let list = parameters.child_value(1);
            let values: Vec<glib::Variant> = (0..list.n_children()).map(|i| list.child_value(i).as_variant().unwrap()).collect();
            sink.borrow_mut().push((name, values));
            invocation.return_value(None);
        }).build().unwrap();
    own(&connection, "org.tessel.Tessel Dock");
    let shown = Rc::new(RefCell::new(Vec::new()));
    let sink = shown.clone();
    connection.register_object("/org/freedesktop/FileManager1", &interface(FILE_MANAGER_XML, "org.freedesktop.FileManager1"))
        .method_call(move |_, _, _, _, _, parameters, invocation| {
            sink.borrow_mut().push(parameters.child_value(0).get::<Vec<String>>().unwrap());
            invocation.return_value(None);
        }).build().unwrap();
    own(&connection, "org.freedesktop.FileManager1");
    let feeds = Arc::new(Mutex::new(FeedLog::default()));
    let log = feeds.clone();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let intercept_open = Rc::new(Cell::new(true));
    let (sink, intercept) = (opened.clone(), intercept_open.clone());
    let hooks = Hooks {
        feed_factory: Arc::new(move |fd, node, props| {
            drop(fd);
            log.lock().unwrap().opened.push((node, Instant::now()));
            Ok(Arc::new(FakeFeed { props, log: log.clone(), closed: AtomicBool::new(false) }) as Arc<dyn Feed>)
        }),
        // No GPU access in tests: Vulkan enumeration reports no devices.
        encoder_chooser: Arc::new(|options: &RecordingOptions, frame: &Frame| {
            media::choose_encoder_with(options, frame, &|_| Ok(Vec::new()), &tessel_capture::process::run)
        }),
        open_saved: Rc::new(move |app: &gtk::Application, uri: &str, reveal: bool| {
            sink.borrow_mut().push((uri.to_string(), reveal));
            if !intercept.get() {
                results::open_saved(app, uri, reveal);
            }
        }),
        portal_bus: None,
    };
    let app = CaptureApp::new(hooks);
    app.0.gtk.register(None::<&gio::Cancellable>).expect("register org.tessel.Capture on the private bus");
    let _hold = app.0.gtk.hold();
    let fakes = Fakes { connection, portal, notifications, dock, shown, feeds, opened, intercept_open, scratch: std::env::temp_dir() };
    let checks: Vec<(&str, Check)> = vec![
        ("test_routed_capture_notice_keeps_output_and_both_file_actions", test_routed_capture_notice_keeps_output_and_both_file_actions),
        ("test_notice_has_buttons_and_routes_saved_uri_without_opening_capture", test_notice_has_buttons_and_routes_saved_uri_without_opening_capture),
        ("test_open_uses_real_default_image_handler", test_open_uses_real_default_image_handler),
        ("test_screenshot_completion_and_preview_buttons_use_saved_file", test_screenshot_completion_and_preview_buttons_use_saved_file),
        ("test_folder_starts_files_and_selects_screenshot", test_folder_starts_files_and_selects_screenshot),
        ("test_other_file_manager_receives_show_items", test_other_file_manager_receives_show_items),
        ("test_missing_capture_reports_error_without_opening_controls", test_missing_capture_reports_error_without_opening_controls),
        ("test_screenshot_notice_copies_and_edits_saved_file", test_screenshot_notice_copies_and_edits_saved_file),
        ("test_settings_merge_into_shared_config_and_editor_renders", test_settings_merge_into_shared_config_and_editor_renders),
        ("test_capture_window_has_standard_menu_and_quit", test_capture_window_has_standard_menu_and_quit),
        ("test_preferences_round_trip_and_legacy_encoder_migration", test_preferences_round_trip_and_legacy_encoder_migration),
        ("test_shortcuts_apply_in_private_config_reloads_labwc", test_shortcuts_apply_in_private_config_reloads_labwc),
        ("test_command_line_binary_contract", test_command_line_binary_contract),
        ("test_portal_cancel_and_immediate_close", test_portal_cancel_and_immediate_close),
        ("test_portal_frame_and_window_source", test_portal_frame_and_window_source),
        ("test_shortcut_dispatch_never_presents_controls", test_shortcut_dispatch_never_presents_controls),
        ("test_screenshot_shortcut_preserves_open_recorder_and_cancel_is_silent", test_screenshot_shortcut_preserves_open_recorder_and_cancel_is_silent),
        ("test_ui_screenshot_and_area_selection", test_ui_screenshot_and_area_selection),
        ("test_direct_monitor_area_and_delay", test_direct_monitor_area_and_delay),
        ("test_workspace_and_armed_capture_reopen_without_new_consent", test_workspace_and_armed_capture_reopen_without_new_consent),
        ("test_ui_recording_finishes_a_portal_stream", test_ui_recording_finishes_a_portal_stream),
    ];
    let (mut failures, mut ran) = (0, 0);
    for (name, check) in checks {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let directory = private_dir(name);
        let file = directory.join("Screenshot café #50% \"one\".png");
        let blue = image::RgbImage::from_pixel(4, 4, image::Rgb([0, 0, 255]));
        std::fs::write(&file, tessel_capture::imaging::png_rgb_with_icc(&blue).unwrap()).unwrap();
        let context = Context { app: app.clone(), fakes: fakes.clone(), directory, file };
        fakes.intercept_open.set(true);
        ran += 1;
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&context)));
        let outcome = match result {
            Ok(Ok(())) => "ok".to_string(),
            Ok(Err(error)) => {
                failures += 1;
                format!("FAILED: {error}")
            }
            Err(panic) => {
                failures += 1;
                format!("PANICKED: {:?}", panic.downcast_ref::<String>().cloned().or(panic.downcast_ref::<&str>().map(|s| s.to_string())))
            }
        };
        println!("test {name} ... {outcome} ({:.1}s)", started.elapsed().as_secs_f64());
        context.reset_window();
        for editor in editor::editors() {
            editor.0.window.destroy();
        }
        pump(0.1);
    }
    println!("ui_headless: {} passed, {failures} failed", ran - failures);
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
