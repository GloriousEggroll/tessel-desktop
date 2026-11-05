//! Simulated MPRIS players on the private bus (port of tessel-lock
//! test_lock_content.FakePlayer and test_media.SeekablePlayer). Refuses to
//! run unless the session bus is a private test bus.

#![allow(dead_code)]


use glib::variant::ToVariant;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub const PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

const BASE: &str = r#"<method name="Pause"/><method name="Play"/><method name="PlayPause"/><method name="Previous"/><method name="Next"/>
<property name="PlaybackStatus" type="s" access="read"/>
<property name="Metadata" type="a{sv}" access="read"/>
<property name="CanControl" type="b" access="read"/>
<property name="CanPause" type="b" access="read"/>
<property name="CanPlay" type="b" access="read"/>
<property name="CanGoPrevious" type="b" access="read"/>
<property name="CanGoNext" type="b" access="read"/>"#;
const SEEK: &str = r#"<method name="SetPosition"><arg name="TrackId" type="o" direction="in"/><arg name="Position" type="x" direction="in"/></method>
<method name="Seek"><arg name="Offset" type="x" direction="in"/></method>
<signal name="Seeked"><arg name="Position" type="x"/></signal>
<property name="Position" type="x" access="read"/>
<property name="Rate" type="d" access="read"/>
<property name="CanSeek" type="b" access="read"/>
<property name="Shuffle" type="b" access="readwrite"/>
<property name="LoopStatus" type="s" access="readwrite"/>"#;
const ROOT: &str = r#"<interface name="org.mpris.MediaPlayer2"><method name="Raise"/>
<property name="CanRaise" type="b" access="read"/>
<property name="DesktopEntry" type="s" access="read"/>
<property name="Identity" type="s" access="read"/></interface>"#;

pub fn private_address() -> String {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("private bus");
    assert!(std::env::var("MEDIA_PRIVATE_BUS").as_deref() == Ok("1"), "Private bus only");
    address
}

pub fn s(text: &str) -> Variant { text.to_variant() }

pub fn metadata(items: &[(&str, Variant)]) -> Variant {
    let dict = glib::VariantDict::new(None);
    for (key, value) in items {
        dict.insert_value(key, value);
    }
    dict.end()
}

pub struct State {
    pub properties: HashMap<String, Variant>,
    pub root: HashMap<String, Variant>,
    pub calls: Vec<String>,
    pub fail: bool,
    pub position: i64,
}

pub struct FakePlayer {
    pub name: String,
    pub connection: gio::DBusConnection,
    pub state: Rc<RefCell<State>>,
    registrations: RefCell<Vec<gio::RegistrationId>>,
    closed: Cell<bool>,
}

pub struct Options<'a> {
    pub state: &'a str,
    pub allowed: bool,
    pub desktop_entry: &'a str,
    pub url: &'a str,
    pub seekable: bool,
}

impl Default for Options<'_> {
    fn default() -> Self { Self { state: "Playing", allowed: true, desktop_entry: "", url: "", seekable: false } }
}

impl FakePlayer {
    pub fn new(name: &str, options: Options) -> Rc<Self> {
        let address = private_address();
        let connection = gio::DBusConnection::for_address_sync(&address, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, gio::Cancellable::NONE).unwrap();
        let full = format!("org.mpris.MediaPlayer2.{name}");
        let mut properties: HashMap<String, Variant> = HashMap::new();
        properties.insert("PlaybackStatus".into(), s(options.state));
        let mut meta = vec![("xesam:title", s("A track <with> symbols"))];
        if options.url.is_empty() {
            meta.push(("xesam:artist", vec!["An artist".to_owned()].to_variant()));
        } else {
            meta.push(("xesam:url", s(options.url)));
        }
        properties.insert("Metadata".into(), metadata(&meta));
        for key in ["CanControl", "CanPause", "CanPlay", "CanGoPrevious", "CanGoNext"] {
            properties.insert(key.into(), options.allowed.to_variant());
        }
        let mut root: HashMap<String, Variant> = HashMap::new();
        root.insert("DesktopEntry".into(), s(options.desktop_entry));
        root.insert("Identity".into(), s("Fixture player"));
        root.insert("CanRaise".into(), true.to_variant());
        if options.seekable {
            properties.insert("Position".into(), 42_000_000i64.to_variant());
            properties.insert("Rate".into(), 1.0f64.to_variant());
            properties.insert("CanSeek".into(), true.to_variant());
            properties.insert("Shuffle".into(), false.to_variant());
            properties.insert("LoopStatus".into(), s("None"));
            properties.insert("Metadata".into(), metadata(&[("xesam:title", s(&format!("{name} song"))),
                ("mpris:trackid", glib::variant::ObjectPath::try_from("/fixture/track/1".to_owned()).unwrap().to_variant()),
                ("mpris:length", 180_000_000i64.to_variant())]));
            root.insert("Identity".into(), s(&format!("{name} player")));
        }
        let state = Rc::new(RefCell::new(State { properties, root, calls: Vec::new(), fail: false, position: 42_000_000 }));
        let xml = format!("<node><interface name=\"{PLAYER}\">{BASE}{}</interface>{ROOT}</node>", if options.seekable { SEEK } else { "" });
        let info = gio::DBusNodeInfo::for_xml(&xml).unwrap();
        let player = Rc::new(Self { name: full.clone(), connection: connection.clone(), state: state.clone(),
                                    registrations: RefCell::new(Vec::new()), closed: Cell::new(false) });
        for interface in info.interfaces() {
            let root_iface = interface.name() == "org.mpris.MediaPlayer2";
            let (weak_m, weak_g, weak_s) = (Rc::downgrade(&player), Rc::downgrade(&player), Rc::downgrade(&player));
            let id = connection.register_object(PATH, interface)
                .method_call(move |_, _, _, _, method, args, invocation| {
                    if let Some(player) = weak_m.upgrade() { player.method(method, &args, invocation) }
                })
                .property(move |_, _, _, _, name| {
                    let Some(player) = weak_g.upgrade() else { return false.to_variant() };
                    let state = player.state.borrow();
                    if root_iface { return state.root[name].clone() }
                    if name == "Position" { return state.position.to_variant() }
                    state.properties[name].clone()
                })
                .set_property(move |_, _, _, _, name, value| {
                    let Some(player) = weak_s.upgrade() else { return false };
                    let shown = value.str().map(String::from).unwrap_or_else(|| value.print(false).to_string());
                    player.state.borrow_mut().calls.push(format!("Set {name} {shown}"));
                    player.change(&[(name, value)]);
                    true
                })
                .build().unwrap();
            player.registrations.borrow_mut().push(id);
        }
        connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                             Some(&(full.as_str(), 0u32).to_variant()), None, gio::DBusCallFlags::NONE, 1000, gio::Cancellable::NONE).unwrap();
        player
    }

    pub fn simple(name: &str) -> Rc<Self> { Self::new(name, Options::default()) }

    fn method(&self, method: &str, args: &Variant, invocation: gio::DBusMethodInvocation) {
        if method == "SetPosition" || method == "Seek" {
            let position = if method == "SetPosition" {
                let (track, position) = (args.child_value(0), args.child_value(1).get::<i64>().unwrap());
                self.state.borrow_mut().calls.push(format!("SetPosition {} {position}", track.str().unwrap()));
                position
            } else {
                let offset = args.child_value(0).get::<i64>().unwrap();
                self.state.borrow_mut().calls.push(format!("Seek {offset}"));
                self.state.borrow().position + offset
            };
            self.state.borrow_mut().position = position;
            let _ = self.connection.emit_signal(None, PATH, PLAYER, "Seeked", Some(&(position,).to_variant()));
            invocation.return_value(None);
            return;
        }
        self.state.borrow_mut().calls.push(method.to_owned());
        if self.state.borrow().fail {
            invocation.return_dbus_error("org.mpris.MediaPlayer2.Error.Failed", "Fixture rejection");
            return;
        }
        let playing = self.state.borrow().properties["PlaybackStatus"].str() == Some("Playing");
        if method == "Pause" || method == "Play" || method == "PlayPause" {
            let pause = method == "Pause" || (method == "PlayPause" && playing);
            self.change(&[("PlaybackStatus", s(if pause { "Paused" } else { "Playing" }))]);
        }
        invocation.return_value(None);
    }

    pub fn change(&self, values: &[(&str, Variant)]) {
        let dict = glib::VariantDict::new(None);
        for (key, value) in values {
            self.state.borrow_mut().properties.insert((*key).to_owned(), value.clone());
            dict.insert_value(key, value);
        }
        let parameters = Variant::tuple_from_iter([PLAYER.to_variant(), dict.end(), Vec::<String>::new().to_variant()]);
        let _ = self.connection.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&parameters));
    }

    pub fn change_root(&self, key: &str, value: Variant) {
        self.state.borrow_mut().root.insert(key.to_owned(), value.clone());
        let dict = glib::VariantDict::new(None);
        dict.insert_value(key, &value);
        let parameters = Variant::tuple_from_iter(["org.mpris.MediaPlayer2".to_variant(), dict.end(), Vec::<String>::new().to_variant()]);
        let _ = self.connection.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&parameters));
    }

    pub fn calls(&self) -> Vec<String> { self.state.borrow().calls.clone() }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        for id in self.registrations.borrow_mut().drain(..) {
            let _ = self.connection.unregister_object(id);
        }
        let _ = self.connection.close_sync(gio::Cancellable::NONE);
    }
}

impl Drop for FakePlayer {
    fn drop(&mut self) { self.close(); }
}

pub fn settle(seconds: f64) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while std::time::Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
