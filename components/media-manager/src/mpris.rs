//! Event-driven MPRIS client: lock-screen/lock_media.py (`LockMedia`) plus the
//! popup's extensions from media_session.py (`MediaSession`: player choice,
//! interpolated position, seeking and play modes). Never launches players or
//! resumes them; commands address the player's *unique* owner so a
//! replacement player never receives a stale action. Designed to move into a
//! shared crate for waydock and the lock screen (`readonly`, `pause_playing`).

use gio::prelude::*;
use glib::variant::ToVariant;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::Instant;

pub const PREFIX: &str = "org.mpris.MediaPlayer2.";
pub const PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
pub const ROOT: &str = "org.mpris.MediaPlayer2";
const DBUS: &str = "org.freedesktop.DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
pub const LOOP_ORDER: [&str; 3] = ["None", "Playlist", "Track"];
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";
const TIMEOUT: i32 = 1500;

/// A D-Bus value unpacked the way PyGObject's `Variant.unpack()` does.
#[derive(Clone, Debug, PartialEq)]
pub enum Py {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Py>),
    Dict(Vec<(String, Py)>),
}

impl Py {
    pub fn str(&self) -> Option<&str> {
        match self { Py::Str(s) => Some(s), _ => None }
    }
    /// `isinstance(value, int)` (bool included, like Python).
    pub fn int(&self) -> Option<i64> {
        match self { Py::Int(v) => Some(*v), Py::Bool(b) => Some(*b as i64), _ => None }
    }
    pub fn is_true(&self) -> bool { matches!(self, Py::Bool(true)) }
    pub fn get(&self, key: &str) -> Option<&Py> {
        match self { Py::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v), _ => None }
    }
}

pub fn py(value: &Variant) -> Py {
    use glib::VariantClass as C;
    match value.classify() {
        C::Boolean => Py::Bool(value.get::<bool>().unwrap_or(false)),
        C::Byte => Py::Int(value.get::<u8>().unwrap_or(0) as i64),
        C::Int16 => Py::Int(value.get::<i16>().unwrap_or(0) as i64),
        C::Uint16 => Py::Int(value.get::<u16>().unwrap_or(0) as i64),
        C::Int32 => Py::Int(value.get::<i32>().unwrap_or(0) as i64),
        C::Uint32 => Py::Int(value.get::<u32>().unwrap_or(0) as i64),
        C::Int64 => Py::Int(value.get::<i64>().unwrap_or(0)),
        C::Uint64 => Py::Int(value.get::<u64>().unwrap_or(0).min(i64::MAX as u64) as i64),
        C::Handle => Py::Int(value.get::<i32>().unwrap_or(0) as i64),
        C::Double => Py::Float(value.get::<f64>().unwrap_or(0.0)),
        C::String | C::ObjectPath | C::Signature => Py::Str(value.str().unwrap_or("").to_owned()),
        C::Variant => value.as_variant().map(|v| py(&v)).unwrap_or(Py::None),
        C::Maybe => value.as_maybe().map(|v| py(&v)).unwrap_or(Py::None),
        C::Array if value.type_().element().is_dict_entry() => Py::Dict((0..value.n_children()).map(|i| {
            let entry = value.child_value(i);
            let key = match py(&entry.child_value(0)) { Py::Str(s) => s, other => format!("{other:?}") };
            (key, py(&entry.child_value(1)))
        }).collect()),
        C::Array | C::Tuple | C::DictEntry => Py::List((0..value.n_children()).map(|i| py(&value.child_value(i))).collect()),
        _ => Py::None,
    }
}

/// `text()`: collapse whitespace, cap 300 characters; non-strings are empty.
pub fn text(value: Option<&Py>) -> String {
    match value {
        Some(Py::Str(s)) => s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect(),
        _ => String::new(),
    }
}

/// Minimal `urllib.parse.urlsplit` (scheme, netloc, path) plus `hostname`.
pub struct Url {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
}

impl Url {
    pub fn split(url: &str) -> Self {
        let mut rest = url;
        let mut scheme = String::new();
        if let Some(colon) = url.find(':') {
            let candidate = &url[..colon];
            if !candidate.is_empty() && candidate.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && candidate.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
                scheme = candidate.to_ascii_lowercase();
                rest = &url[colon + 1..];
            }
        }
        let mut netloc = String::new();
        if let Some(after) = rest.strip_prefix("//") {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            netloc = after[..end].to_owned();
            rest = &after[end..];
        }
        let end = rest.find(['?', '#']).unwrap_or(rest.len());
        Self { scheme, netloc, path: rest[..end].to_owned() }
    }

    pub fn has_userinfo(&self) -> bool { self.netloc.contains('@') }

    pub fn hostname(&self) -> String {
        let host = self.netloc.rsplit('@').next().unwrap_or("");
        let host = if let Some(stripped) = host.strip_prefix('[') { stripped.split(']').next().unwrap_or("") } else {
            host.split(':').next().unwrap_or("")
        };
        host.to_lowercase()
    }
}

/// `urllib.parse.unquote` (UTF-8, invalid sequences replaced).
pub fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `PurePosixPath(path).suffix`.
fn suffix(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or("");
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => name[i..].to_owned(),
        _ => String::new(),
    }
}

/// Prefer reported media types, then recognizable track URLs. An empty result
/// means unknown; known videos must never become album cards.
pub fn track_kind(metadata: &Py, url: &str) -> &'static str {
    let mut types: Vec<String> = Vec::new();
    for key in ["xesam:contentType", "xesam:mimeType", "mpris:mediaType", "kde:mediaType"] {
        match metadata.get(key) {
            Some(Py::Str(s)) => types.push(s.clone()),
            Some(Py::List(items)) => types.extend(items.iter().take(16).filter_map(|v| v.str().map(String::from))),
            _ => {}
        }
    }
    let normalized: Vec<String> = types.iter().map(|v| {
        let lowered = v.to_lowercase();
        let trimmed = lowered.trim_end_matches('/');
        trimmed.rsplit('#').next().unwrap_or("").to_owned()
    }).collect();
    let is = |kind: &str| normalized.iter().any(|v| v == kind || v.starts_with(&format!("{kind}/")) || v.ends_with(&format!("/{kind}")));
    if is("video") {
        return "video";
    }
    if is("audio") {
        return "audio";
    }
    let parts = Url::split(url);
    let suffix = suffix(&unquote(&parts.path)).to_lowercase();
    let host = parts.hostname();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    const VIDEO: [&str; 13] = [".mkv", ".mp4", ".m4v", ".avi", ".webm", ".mov", ".ogv", ".flv", ".wmv", ".mpeg", ".mpg", ".ts", ".m2ts"];
    const AUDIO: [&str; 11] = [".mp3", ".flac", ".wav", ".m4a", ".aac", ".ogg", ".oga", ".opus", ".wma", ".aiff", ".alac"];
    if VIDEO.contains(&suffix.as_str()) {
        return "video";
    }
    if AUDIO.contains(&suffix.as_str()) {
        return "audio";
    }
    if ["youtube.com", "m.youtube.com", "youtu.be", "vimeo.com", "twitch.tv", "netflix.com"].contains(&host) {
        return "video";
    }
    if ["open.spotify.com", "soundcloud.com", "music.apple.com", "listen.tidal.com"].contains(&host) || parts.scheme == "spotify" {
        return "audio";
    }
    ""
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    pub name: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub controllable: bool,
    pub busy: bool,
    pub error: String,
    pub desktop_entry: String,
    pub identity: String,
    pub url: String,
    pub can_previous: bool,
    pub can_next: bool,
    pub album: String,
    pub art_url: String,
    pub kind: String,
    pub can_raise: bool,
}

/// Seek and play-mode capabilities; `None` = unsupported.
#[derive(Clone, Debug, PartialEq)]
pub struct Details {
    pub length: i64,
    pub can_seek: bool,
    pub shuffle: Option<bool>,
    pub loop_status: Option<String>,
    pub can_control: bool,
}

struct Player {
    proxy: gio::DBusProxy,
    handler: Option<glib::SignalHandlerId>,
}

pub struct Inner {
    changed: RefCell<Rc<dyn Fn()>>,
    pub readonly: bool,
    bus: RefCell<Option<gio::DBusConnection>>,
    players: RefCell<BTreeMap<String, Player>>,
    pending: RefCell<HashMap<String, u64>>,
    tokens: Cell<u64>,
    selected: RefCell<Option<String>>,
    busy: RefCell<HashSet<String>>,
    errors: RefCell<HashMap<String, String>>,
    identities: RefCell<HashMap<String, (String, String)>>,
    can_raise: RefCell<HashMap<String, bool>>,
    pub closed: Cell<bool>,
    started: Cell<bool>,
    subscriptions: RefCell<Vec<gio::SignalSubscriptionId>>,
    cancel: gio::Cancellable,
    pause_initial: Cell<bool>,
    pinned: RefCell<Option<String>>,
    positions: RefCell<HashMap<String, (i64, Instant)>>,
}

#[derive(Clone)]
pub struct MediaSession(pub Rc<Inner>);

fn up(weak: &Weak<Inner>) -> Option<MediaSession> { weak.upgrade().map(MediaSession) }

fn property(proxy: &gio::DBusProxy, key: &str) -> Py {
    proxy.cached_property(key).map(|v| py(&v)).unwrap_or(Py::None)
}

impl MediaSession {
    pub fn new(changed: impl Fn() + 'static, readonly: bool) -> Self {
        Self(Rc::new(Inner {
            changed: RefCell::new(Rc::new(changed)), readonly, bus: RefCell::new(None), players: RefCell::new(BTreeMap::new()),
            pending: RefCell::new(HashMap::new()), tokens: Cell::new(0), selected: RefCell::new(None),
            busy: RefCell::new(HashSet::new()), errors: RefCell::new(HashMap::new()), identities: RefCell::new(HashMap::new()),
            can_raise: RefCell::new(HashMap::new()), closed: Cell::new(false), started: Cell::new(false),
            subscriptions: RefCell::new(Vec::new()), cancel: gio::Cancellable::new(), pause_initial: Cell::new(false),
            pinned: RefCell::new(None), positions: RefCell::new(HashMap::new()),
        }))
    }

    fn changed(&self) {
        let callback = self.0.changed.borrow().clone();
        callback();
    }

    pub fn player_names(&self) -> Vec<String> { self.0.players.borrow().keys().cloned().collect() }

    fn proxy(&self, name: &str) -> Option<gio::DBusProxy> { self.0.players.borrow().get(name).map(|p| p.proxy.clone()) }

    fn same_proxy(&self, name: &str, proxy: &gio::DBusProxy) -> bool { self.proxy(name).is_some_and(|p| &p == proxy) }

    pub fn start(&self, pause_playing: bool) {
        if self.0.started.get() || self.0.closed.get() {
            return;
        }
        self.0.started.set(true);
        self.0.pause_initial.set(pause_playing && !self.0.readonly);
        let weak = Rc::downgrade(&self.0);
        gio::bus_get(gio::BusType::Session, Some(&self.0.cancel), move |result| {
            if let (Some(this), Ok(bus)) = (up(&weak), result) { this.connected(bus) }
        });
    }

    #[allow(deprecated)]
    fn connected(&self, bus: gio::DBusConnection) {
        if self.0.closed.get() {
            return;
        }
        self.0.bus.replace(Some(bus.clone()));
        let weak = Rc::downgrade(&self.0);
        let owner = bus.signal_subscribe(Some(DBUS), Some(DBUS), Some("NameOwnerChanged"), Some("/org/freedesktop/DBus"), None,
            gio::DBusSignalFlags::NONE, move |_, _, _, _, _, parameters| {
                let Some(this) = up(&weak) else { return };
                let Some((name, _old, new)) = parameters.get::<(String, String, String)>() else { return };
                if this.0.closed.get() || !name.starts_with(PREFIX) {
                    return;
                }
                this.remove(&name);
                if !new.is_empty() {
                    this.discover(&name, false);
                }
            });
        let weak = Rc::downgrade(&self.0);
        let root = bus.signal_subscribe(None, Some(PROPERTIES), Some("PropertiesChanged"), Some(PATH), Some(ROOT),
            gio::DBusSignalFlags::NONE, move |_, sender, _, _, _, _| {
                let Some(this) = up(&weak) else { return };
                if this.0.closed.get() {
                    return;
                }
                let owned: Vec<(String, gio::DBusProxy)> = this.0.players.borrow().iter()
                    .filter(|(_, p)| p.proxy.name_owner().as_deref() == Some(sender))
                    .map(|(n, p)| (n.clone(), p.proxy.clone())).collect();
                for (name, proxy) in owned {
                    this.identity(&name, &proxy);
                }
            });
        let weak = Rc::downgrade(&self.0);
        let seeked = bus.signal_subscribe(None, Some(PLAYER), Some("Seeked"), Some(PATH), None, gio::DBusSignalFlags::NONE,
            move |_, sender, _, _, _, parameters| if let Some(this) = up(&weak) { this.seeked(sender, parameters) });
        self.0.subscriptions.borrow_mut().extend([owner, root, seeked]);
        let weak = Rc::downgrade(&self.0);
        bus.call(Some(DBUS), "/org/freedesktop/DBus", DBUS, "ListNames", None, Some(glib::VariantTy::new("(as)").unwrap()),
                 gio::DBusCallFlags::NO_AUTO_START, TIMEOUT, Some(&self.0.cancel), move |result| {
            let (Some(this), Ok(reply)) = (up(&weak), result) else { return };
            if this.0.closed.get() {
                return;
            }
            let pause = this.0.pause_initial.get();
            for name in reply.child_value(0).get::<Vec<String>>().unwrap_or_default() {
                if name.starts_with(PREFIX) {
                    this.discover(&name, pause);
                }
            }
        });
    }

    fn remove(&self, name: &str) {
        self.0.positions.borrow_mut().remove(name);
        if self.0.pinned.borrow().as_deref() == Some(name) {
            self.0.pinned.replace(None);
        }
        self.0.pending.borrow_mut().remove(name);
        let removed = self.0.players.borrow_mut().remove(name);
        if let Some(Player { proxy, handler: Some(handler) }) = removed {
            proxy.disconnect(handler);
        }
        self.0.busy.borrow_mut().remove(name);
        self.0.errors.borrow_mut().remove(name);
        self.0.identities.borrow_mut().remove(name);
        self.0.can_raise.borrow_mut().remove(name);
        self.changed();
    }

    fn discover(&self, name: &str, pause: bool) {
        if self.0.players.borrow().contains_key(name) || self.0.pending.borrow().contains_key(name) {
            return;
        }
        let Some(bus) = self.0.bus.borrow().clone() else { return };
        let token = self.0.tokens.get() + 1;
        self.0.tokens.set(token);
        self.0.pending.borrow_mut().insert(name.to_owned(), token);
        let weak = Rc::downgrade(&self.0);
        let (name, bus_name) = (name.to_owned(), name.to_owned());
        gio::DBusProxy::new(&bus, gio::DBusProxyFlags::DO_NOT_AUTO_START | gio::DBusProxyFlags::GET_INVALIDATED_PROPERTIES, None,
                            Some(&bus_name), PATH, PLAYER, Some(&self.0.cancel), move |result| {
            let Some(this) = up(&weak) else { return };
            if this.0.closed.get() || this.0.pending.borrow().get(&name) != Some(&token) {
                return;
            }
            this.0.pending.borrow_mut().remove(&name);
            let Ok(proxy) = result else { return };
            if proxy.name_owner().is_none() {
                return;
            }
            proxy.set_default_timeout(TIMEOUT);
            let weak = Rc::downgrade(&this.0);
            let handler = proxy.connect_local("g-properties-changed", false, move |values| {
                let (Some(this), Some(proxy), Some(changed)) = (up(&weak), values[0].get::<gio::DBusProxy>().ok(),
                                                                 values[1].get::<Variant>().ok()) else { return None };
                this.properties(&proxy, &changed);
                None
            });
            this.0.players.borrow_mut().insert(name.clone(), Player { proxy: proxy.clone(), handler: Some(handler) });
            this.identity(&name, &proxy);
            if pause && property(&proxy, "PlaybackStatus").str() == Some("Playing") {
                this.command(&name, &proxy, "Pause", None);
            }
            this.changed();
        });
    }

    fn identity(&self, name: &str, proxy: &gio::DBusProxy) {
        let (Some(bus), Some(owner)) = (self.0.bus.borrow().clone(), proxy.name_owner()) else { return };
        let weak = Rc::downgrade(&self.0);
        let (name, proxy) = (name.to_owned(), proxy.clone());
        bus.call(Some(&owner), PATH, PROPERTIES, "GetAll", Some(&(ROOT,).to_variant()), Some(glib::VariantTy::new("(a{sv})").unwrap()),
                 gio::DBusCallFlags::NO_AUTO_START, TIMEOUT, Some(&self.0.cancel), move |result| {
            let (Some(this), Ok(reply)) = (up(&weak), result) else { return };
            if this.0.closed.get() || !this.same_proxy(&name, &proxy) {
                return;
            }
            let values = py(&reply.child_value(0));
            this.0.identities.borrow_mut().insert(name.clone(), (text(values.get("DesktopEntry")), text(values.get("Identity"))));
            this.0.can_raise.borrow_mut().insert(name.clone(), values.get("CanRaise").is_some_and(Py::is_true));
            this.changed();
        });
    }

    fn properties(&self, proxy: &gio::DBusProxy, changed: &Variant) {
        let keys: Vec<String> = match py(changed) { Py::Dict(items) => items.into_iter().map(|(k, _)| k).collect(), _ => Vec::new() };
        let has = |key: &str| keys.iter().any(|k| k == key);
        let name = proxy.name().map(|n| n.to_string()).unwrap_or_default();
        if self.0.positions.borrow().contains_key(&name) && (has("PlaybackStatus") || has("Rate") || has("Metadata")) {
            // Re-anchor the interpolation at the current estimate.
            let estimate = if has("Metadata") { 0 } else { self.position(&name) };
            self.0.positions.borrow_mut().insert(name.clone(), (estimate, Instant::now()));
            self.refresh_position(&name);
        }
        if self.0.closed.get() {
            return;
        }
        if has("PlaybackStatus") || has("Metadata") {
            self.0.errors.borrow_mut().remove(&name);
        }
        self.changed();
    }

    fn build(&self, name: &str, proxy: &gio::DBusProxy) -> Option<Media> {
        let state = property(proxy, "PlaybackStatus");
        let state = state.str()?;
        if !matches!(state, "Playing" | "Paused") || proxy.name_owner().is_none() {
            return None;
        }
        let metadata = match property(proxy, "Metadata") { m @ Py::Dict(_) => m, _ => Py::Dict(Vec::new()) };
        let title = Some(text(metadata.get("xesam:title"))).filter(|t| !t.is_empty()).unwrap_or_else(|| "Media playback".into());
        let artist = match metadata.get("xesam:artist") {
            Some(Py::List(items)) => {
                let joined = items.iter().take(10).filter_map(|v| v.str()).collect::<Vec<_>>().join(", ");
                text(Some(&Py::Str(joined)))
            }
            _ => String::new(),
        };
        let is = |key: &str| property(proxy, key).is_true();
        let allowed = is("CanControl") && is(if state == "Playing" { "CanPause" } else { "CanPlay" });
        let (desktop, identity) = self.0.identities.borrow().get(name).cloned().unwrap_or_default();
        let bounded = |key: &str| match metadata.get(key) {
            Some(Py::Str(s)) if s.chars().count() <= 8192 => s.clone(),
            _ => String::new(),
        };
        let url = bounded("xesam:url");
        let art_url = bounded("mpris:artUrl");
        let controls = is("CanControl") && !self.0.readonly;
        Some(Media {
            name: name.to_owned(), title, artist, playing: state == "Playing", controllable: allowed && !self.0.readonly,
            busy: self.0.busy.borrow().contains(name), error: self.0.errors.borrow().get(name).cloned().unwrap_or_default(),
            desktop_entry: desktop, identity, can_previous: controls && is("CanGoPrevious"), can_next: controls && is("CanGoNext"),
            album: text(metadata.get("xesam:album")), kind: track_kind(&metadata, &url).into(), url, art_url,
            can_raise: self.0.can_raise.borrow().get(name).copied().unwrap_or(false) && !self.0.readonly,
        })
    }

    /// The shared Media model of one player (None unless playing/paused).
    pub fn media_for(&self, name: &str) -> Option<Media> {
        let proxy = self.proxy(name)?;
        self.build(name, &proxy)
    }

    /// LockMedia.current: playing first, then the previous selection, then bus name.
    fn base_current(&self) -> Option<Media> {
        let players: Vec<(String, gio::DBusProxy)> = self.0.players.borrow().iter().map(|(n, p)| (n.clone(), p.proxy.clone())).collect();
        let selected = self.0.selected.borrow().clone();
        let mut choices: Vec<Media> = players.iter().filter_map(|(name, proxy)| self.build(name, proxy)).collect();
        choices.sort_by(|a, b| (!a.playing, Some(&a.name) != selected.as_ref(), &a.name)
            .cmp(&(!b.playing, Some(&b.name) != selected.as_ref(), &b.name)));
        let media = choices.into_iter().next();
        self.0.selected.replace(media.as_ref().map(|m| m.name.clone()));
        media
    }

    pub fn current(&self) -> Option<Media> {
        let pinned = self.0.pinned.borrow().clone();
        if let Some(pinned) = pinned {
            if let Some(media) = self.media_for(&pinned) {
                self.0.selected.replace(Some(media.name.clone()));
                return Some(media);
            }
        }
        self.base_current()
    }

    pub fn available(&self) -> Vec<Media> {
        let mut choices: Vec<Media> = self.player_names().iter().filter_map(|n| self.media_for(n)).collect();
        let key = |m: &Media| ((if m.identity.is_empty() { &m.name } else { &m.identity }).to_lowercase(), m.name.clone());
        choices.sort_by_key(key);
        choices
    }

    pub fn choose(&self, name: &str) {
        if self.0.players.borrow().contains_key(name) && self.0.pinned.borrow().as_deref() != Some(name) {
            self.0.pinned.replace(Some(name.to_owned()));
            self.refresh_position(name);
            self.changed();
        }
    }

    fn value(&self, name: &str, key: &str) -> Py { self.proxy(name).map(|p| property(&p, key)).unwrap_or(Py::None) }

    pub fn details(&self, media: Option<&Media>) -> Option<Details> {
        let media = media?;
        let name = &media.name;
        if !self.0.players.borrow().contains_key(name) {
            return None;
        }
        let metadata = self.value(name, "Metadata");
        let length = metadata.get("mpris:length").and_then(Py::int).filter(|l| *l > 0).unwrap_or(0);
        let control = self.value(name, "CanControl").is_true() && !self.0.readonly;
        let shuffle = match self.value(name, "Shuffle") { Py::Bool(b) => Some(b), _ => None };
        let loop_status = self.value(name, "LoopStatus").str().filter(|l| LOOP_ORDER.contains(l)).map(String::from);
        Some(Details { length, can_seek: control && length != 0 && self.value(name, "CanSeek").is_true(), shuffle, loop_status,
                       can_control: control })
    }

    /// Interpolated position in microseconds.
    pub fn position(&self, name: &str) -> i64 {
        let anchor = self.0.positions.borrow().get(name).copied();
        let Some((position, at)) = anchor else {
            return self.value(name, "Position").int().unwrap_or(0);
        };
        let mut position = position;
        if self.value(name, "PlaybackStatus").str() == Some("Playing") {
            let rate = match self.value(name, "Rate") {
                Py::Float(r) if r > 0.0 => r,
                Py::Int(r) if r > 0 => r as f64,
                Py::Bool(true) => 1.0,
                _ => 1.0,
            };
            position += (at.elapsed().as_secs_f64() * 1_000_000.0 * rate) as i64;
        }
        position.max(0)
    }

    pub fn refresh_position(&self, name: &str) {
        let Some(proxy) = self.proxy(name) else { return };
        let (Some(bus), Some(owner), false) = (self.0.bus.borrow().clone(), proxy.name_owner(), self.0.closed.get()) else { return };
        let weak = Rc::downgrade(&self.0);
        let name = name.to_owned();
        bus.call(Some(&owner), PATH, PROPERTIES, "Get", Some(&(PLAYER, "Position").to_variant()), Some(glib::VariantTy::new("(v)").unwrap()),
                 gio::DBusCallFlags::NO_AUTO_START, TIMEOUT, Some(&self.0.cancel), move |result| {
            let (Some(this), Ok(reply)) = (up(&weak), result) else { return };
            let value = py(&reply.child_value(0));
            if !this.0.closed.get() && this.same_proxy(&name, &proxy) {
                if let Py::Int(value) = value {
                    this.0.positions.borrow_mut().insert(name.clone(), (value.max(0), Instant::now()));
                    this.changed();
                }
            }
        });
    }

    fn seeked(&self, sender: &str, parameters: &Variant) {
        if self.0.closed.get() {
            return;
        }
        let Some((position,)) = parameters.get::<(i64,)>() else { return };
        let owned: Vec<String> = self.0.players.borrow().iter()
            .filter(|(_, p)| p.proxy.name_owner().as_deref() == Some(sender)).map(|(n, _)| n.clone()).collect();
        for name in owned {
            self.0.positions.borrow_mut().insert(name, (position.max(0), Instant::now()));
            self.changed();
        }
    }

    fn call(&self, name: &str, interface: &str, method: &str, parameters: Variant) -> bool {
        let Some(proxy) = self.proxy(name) else { return false };
        let (Some(bus), Some(owner)) = (self.0.bus.borrow().clone(), proxy.name_owner()) else { return false };
        if self.0.closed.get() || self.0.readonly {
            return false;
        }
        let weak = Rc::downgrade(&self.0);
        let name = name.to_owned();
        // Address the unique owner, as the shared client does for commands.
        bus.call(Some(&owner), PATH, interface, method, Some(&parameters), None, gio::DBusCallFlags::NO_AUTO_START, TIMEOUT,
                 Some(&self.0.cancel), move |result| {
            let Some(this) = up(&weak) else { return };
            if result.is_err() && !this.0.closed.get() && this.same_proxy(&name, &proxy) {
                this.0.errors.borrow_mut().insert(name.clone(), "Playback control is unavailable.".into());
                this.changed();
            }
        });
        true
    }

    pub fn set_position(&self, media: Option<&Media>, microseconds: f64) -> bool {
        let Some(details) = self.details(media).filter(|d| d.can_seek) else { return false };
        let media = media.unwrap();
        let target = (microseconds as i64).clamp(0, details.length);
        let metadata = self.value(&media.name, "Metadata");
        let track = metadata.get("mpris:trackid").and_then(Py::str).map(String::from);
        let sent = match track.filter(|t| Variant::is_object_path(t) && t != NO_TRACK) {
            Some(track) => {
                let path = glib::variant::ObjectPath::try_from(track).expect("object path");
                self.call(&media.name, PLAYER, "SetPosition", (path, target).to_variant())
            }
            None => self.call(&media.name, PLAYER, "Seek", (target - self.position(&media.name),).to_variant()),
        };
        if sent {
            self.0.positions.borrow_mut().insert(media.name.clone(), (target, Instant::now()));
            self.changed();
        }
        sent
    }

    pub fn set_shuffle(&self, media: Option<&Media>, enabled: bool) -> bool {
        let Some(details) = self.details(media) else { return false };
        if !details.can_control || details.shuffle.is_none() {
            return false;
        }
        self.call(&media.unwrap().name, PROPERTIES, "Set", (PLAYER, "Shuffle", enabled.to_variant()).to_variant())
    }

    pub fn cycle_loop(&self, media: Option<&Media>) -> bool {
        let Some(details) = self.details(media) else { return false };
        let (true, Some(current)) = (details.can_control, details.loop_status) else { return false };
        let index = LOOP_ORDER.iter().position(|l| *l == current).unwrap_or(0);
        let following = LOOP_ORDER[(index + 1) % LOOP_ORDER.len()];
        self.call(&media.unwrap().name, PROPERTIES, "Set", (PLAYER, "LoopStatus", following.to_variant()).to_variant())
    }

    pub fn toggle(&self) {
        if let Some(media) = self.current().filter(|m| m.controllable && !m.busy) {
            if let Some(proxy) = self.proxy(&media.name) {
                self.command(&media.name, &proxy, if media.playing { "Pause" } else { "Play" }, None);
            }
        }
    }

    pub fn previous(&self) {
        if let Some(media) = self.current().filter(|m| m.can_previous && !m.busy) {
            if let Some(proxy) = self.proxy(&media.name) {
                self.command(&media.name, &proxy, "Previous", None);
            }
        }
    }

    pub fn next(&self) {
        if let Some(media) = self.current().filter(|m| m.can_next && !m.busy) {
            if let Some(proxy) = self.proxy(&media.name) {
                self.command(&media.name, &proxy, "Next", None);
            }
        }
    }

    /// Ask the current player to activate its own UI/tab, without launching a URL.
    pub fn raise_player(&self, finished: Option<Box<dyn FnOnce(bool)>>) -> bool {
        if let Some(media) = self.current().filter(|m| m.can_raise && !m.busy) {
            if let Some(proxy) = self.proxy(&media.name) {
                return self.command(&media.name, &proxy, "Raise", finished);
            }
        }
        false
    }

    fn command(&self, name: &str, proxy: &gio::DBusProxy, method: &str, finished: Option<Box<dyn FnOnce(bool)>>) -> bool {
        let capability = match method { "Pause" => Some("CanPause"), "Play" => Some("CanPlay"), "Previous" => Some("CanGoPrevious"),
                                        "Next" => Some("CanGoNext"), _ => None };
        let allowed = if method == "Raise" {
            self.0.can_raise.borrow().get(name).copied().unwrap_or(false)
        } else {
            capability.is_some_and(|c| property(proxy, "CanControl").is_true() && property(proxy, c).is_true())
        };
        let owner = proxy.name_owner();
        let bus = self.0.bus.borrow().clone();
        let (Some(owner), Some(bus)) = (owner, bus) else { return false };
        if !allowed || self.0.closed.get() || self.0.readonly || self.0.busy.borrow().contains(name) {
            return false;
        }
        self.0.busy.borrow_mut().insert(name.to_owned());
        self.0.errors.borrow_mut().remove(name);
        self.changed();
        let weak = Rc::downgrade(&self.0);
        let (name, proxy, method_name) = (name.to_owned(), proxy.clone(), method.to_owned());
        bus.call(Some(&owner), PATH, if method == "Raise" { ROOT } else { PLAYER }, method, None, None,
                 gio::DBusCallFlags::NO_AUTO_START, TIMEOUT, Some(&self.0.cancel), move |result| {
            let error = match result {
                Ok(_) => None,
                Err(_) if method_name == "Raise" => Some("Could not open the media player."),
                Err(_) => Some("Playback control is unavailable."),
            };
            if let Some(this) = up(&weak) {
                if !this.0.closed.get() && this.same_proxy(&name, &proxy) {
                    this.0.busy.borrow_mut().remove(&name);
                    if let Some(error) = error {
                        this.0.errors.borrow_mut().insert(name.clone(), error.into());
                    }
                    this.changed();
                }
            }
            if let Some(finished) = finished {
                finished(error.is_none());
            }
        });
        true
    }

    #[allow(deprecated)]
    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.cancel.cancel();
        if let Some(bus) = self.0.bus.borrow().clone() {
            for id in self.0.subscriptions.borrow_mut().drain(..) {
                bus.signal_unsubscribe(id);
            }
        }
        self.0.changed.replace(Rc::new(|| {}));
        for name in self.player_names() {
            self.remove(&name);
        }
        self.0.pending.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(items: &[(&str, Py)]) -> Py { Py::Dict(items.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()) }

    #[test]
    fn track_kind_prefers_types_then_urls_then_hosts() {
        let s = |v: &str| Py::Str(v.into());
        assert_eq!(track_kind(&meta(&[("xesam:contentType", s("Video/"))]), "file:///a.mp3"), "video");
        assert_eq!(track_kind(&meta(&[("kde:mediaType", Py::List(vec![s("x#audio")]))]), ""), "audio");
        assert_eq!(track_kind(&meta(&[]), "file:///tmp/movie.MKV"), "video");
        assert_eq!(track_kind(&meta(&[]), "file:///tmp/a%20song.flac"), "audio");
        assert_eq!(track_kind(&meta(&[]), "https://www.youtube.com/watch?v=1"), "video");
        assert_eq!(track_kind(&meta(&[]), "spotify:track:fixture"), "audio");
        assert_eq!(track_kind(&meta(&[]), "https://example.test/watch?id=1"), "");
        assert_eq!(track_kind(&meta(&[]), "file:///tmp/.hidden"), "");
    }

    #[test]
    fn text_collapses_whitespace_and_caps_length() {
        assert_eq!(text(Some(&Py::Str(" An album\nname ".into()))), "An album name");
        assert_eq!(text(Some(&Py::Str("x".repeat(400)))).len(), 300);
        assert_eq!(text(Some(&Py::Int(3))), "");
    }

    #[test]
    fn variants_unpack_like_pygobject() {
        let dict = glib::VariantDict::new(None);
        dict.insert_value("mpris:length", &180_000_000i64.to_variant());
        dict.insert_value("mpris:trackid", &glib::variant::ObjectPath::try_from("/fixture/track/1".to_owned()).unwrap().to_variant());
        dict.insert_value("xesam:artist", &vec!["A".to_owned(), "B".to_owned()].to_variant());
        let value = py(&dict.end());
        assert_eq!(value.get("mpris:length"), Some(&Py::Int(180_000_000)));
        assert_eq!(value.get("mpris:trackid").and_then(Py::str), Some("/fixture/track/1"));
        assert_eq!(value.get("xesam:artist"), Some(&Py::List(vec![Py::Str("A".into()), Py::Str("B".into())])));
    }
}
