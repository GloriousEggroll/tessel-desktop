//! On-demand per-application playback mute for window previews (port of
//! application_audio.py). No polling, no sink-wide writes, no microphones.
//! Call `close()` when the preview closes so nothing lives for the session.
//!
//! # Approach (gap vs Python)
//! Python used WirePlumber's GI mixer-api on its own PipeWire core. No
//! WirePlumber development files exist here, so the Rust port watches the
//! graph with **`pw-dump --monitor --no-colors`** (one child per open
//! preview, event-driven: PipeWire pushes changes, nothing polls) and mutes
//! with **`wpctl set-mute <id> 1|0`** (wpctl writes through the same
//! mixer-api). Differences: a mute write is reported successful once wpctl
//! *starts* (Python got the mixer's synchronous result); the reused-id guard
//! compares the PipeWire `object.serial` seen when the streams were listed
//! with the newest graph state (Python compared proxy identity); "ready"
//! means the first graph dump arrived (Python: mixer plugin activated).
//!
//! # API
//! - [`application_key`], [`matches_application`] (pure, as Python).
//! - [`ApplicationAudio::new(catalog, changed)`] (real source) or
//!   [`ApplicationAudio::with_source`]; `start`, `state(app_id) ->
//!   (available, muted)`, `toggle(app_id) -> bool`, `streams`, `close`,
//!   `ready`, `error`. Implements [`AudioControl`] (what previews use).
//! - [`AudioSource`] (injectable graph source), [`PwDumpSource`] (real; argv
//!   and spawner injectable), [`FakeAudioSource`] (tests), [`JsonStream`]
//!   (concatenated JSON documents from a pipe).
//! - [`update_mute_button`] (icon, sensitivity, translated tooltip).

use gtk::prelude::*;
use gtk::{gio, glib};
use indexmap::IndexMap;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::core::app_catalog::AppCatalog;
use crate::core::spawn::{GioSpawner, Spawner};

/// `application_key(value, catalog)`: the resolved desktop id (or the raw
/// value) without `.desktop`, casefolded.
pub fn application_key(value: &str, catalog: &AppCatalog) -> String {
    let value = suite_core::pyjson::py_strip(value);
    if value.is_empty() {
        return String::new();
    }
    let id = catalog.resolve(value, None).map(|app| app.id.clone()).unwrap_or_else(|| value.to_owned());
    suite_core::pyjson::casefold(id.strip_suffix(".desktop").unwrap_or(&id))
}

/// String properties of a stream node (client fallbacks already applied).
pub type Props = HashMap<String, String>;

fn prop<'a>(props: &'a Props, key: &str) -> &'a str {
    props.get(key).map(String::as_str).unwrap_or("")
}

/// `matches_application(app_id, props, catalog)`.
pub fn matches_application(app_id: &str, props: &Props, catalog: &AppCatalog) -> bool {
    let key = application_key(app_id, catalog);
    if key.is_empty() || prop(props, "media.class") != "Stream/Output/Audio" {
        return false;
    }
    // Desktop IDs are authoritative. Never guess from a window/media title or
    // a localized display name; generic interpreters are not app identities.
    let desktop = [prop(props, "application.id"), prop(props, "pipewire.access.portal.app_id")]
        .into_iter()
        .find(|v| !v.is_empty());
    if let Some(desktop) = desktop {
        return key == application_key(desktop, catalog);
    }
    let binary = std::path::Path::new(prop(props, "application.process.binary"))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if ["python", "python3", "wine", "wine64", "node", "java", ""].contains(&binary.as_str()) {
        return false;
    }
    key == application_key(&binary, catalog)
}

const KEYS: [&str; 5] = ["media.class", "application.id", "pipewire.access.portal.app_id", "application.process.binary", "client.id"];

/// What previews need from the audio controller (Python duck type).
pub trait AudioControl {
    fn start(&self);
    /// (playback streams exist, all of them are muted).
    fn state(&self, app_id: &str) -> (bool, bool);
    fn toggle(&self, app_id: &str) -> bool;
    fn close(&self);
}

/// Events from an [`AudioSource`].
#[derive(Clone, Debug, PartialEq)]
pub enum SourceEvent {
    /// One `pw-dump` document: an array of (changed) objects; an object with
    /// `"info": null` was removed.
    Objects(Vec<Value>),
    /// The graph connection ended.
    Disconnected,
}

/// Where the PipeWire graph comes from and how mute is written.
pub trait AudioSource {
    /// Begin delivering events (on the thread-default main context). `Err`
    /// is the error text.
    fn start(&self, events: Rc<dyn Fn(SourceEvent)>) -> Result<(), String>;
    /// Write `mute` on node `id`; false when the write could not be started.
    fn set_mute(&self, id: u32, mute: bool) -> bool;
    fn close(&self);
}

/// Splits a byte stream of concatenated JSON documents (pw-dump output).
#[derive(Default)]
pub struct JsonStream {
    buffer: Vec<u8>,
}

impl JsonStream {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.buffer.extend_from_slice(bytes);
        let mut values = Vec::new();
        let mut start = 0;
        loop {
            while start < self.buffer.len() && self.buffer[start].is_ascii_whitespace() {
                start += 1;
            }
            if start >= self.buffer.len() {
                break;
            }
            let mut iter = serde_json::Deserializer::from_slice(&self.buffer[start..]).into_iter::<Value>();
            match iter.next() {
                Some(Ok(value)) => {
                    start += iter.byte_offset();
                    values.push(value);
                }
                Some(Err(error)) if error.is_eof() => break,
                Some(Err(_)) => {
                    // Resynchronise on the next line.
                    match self.buffer[start..].iter().position(|b| *b == b'\n') {
                        Some(newline) => start += newline + 1,
                        None => {
                            start = self.buffer.len();
                        }
                    }
                }
                None => break,
            }
        }
        self.buffer.drain(..start);
        values
    }
}

/// The real graph source: `pw-dump --monitor --no-colors` + `wpctl set-mute`.
pub struct PwDumpSource {
    /// argv of the monitor (default `pw-dump --monitor --no-colors`).
    pub monitor: Vec<String>,
    /// argv prefix of the mute write (default `wpctl set-mute`).
    pub mute: Vec<String>,
    pub spawner: Rc<dyn Spawner>,
    process: RefCell<Option<gio::Subprocess>>,
    cancellable: gio::Cancellable,
}

impl Default for PwDumpSource {
    fn default() -> Self {
        Self::new(
            vec!["pw-dump".into(), "--monitor".into(), "--no-colors".into()],
            vec!["wpctl".into(), "set-mute".into()],
            Rc::new(GioSpawner),
        )
    }
}

impl PwDumpSource {
    pub fn new(monitor: Vec<String>, mute: Vec<String>, spawner: Rc<dyn Spawner>) -> Self {
        Self { monitor, mute, spawner, process: RefCell::new(None), cancellable: gio::Cancellable::new() }
    }

    fn read(stream: gio::InputStream, cancellable: gio::Cancellable, mut parser: JsonStream, events: Rc<dyn Fn(SourceEvent)>) {
        let next = stream.clone();
        let again = cancellable.clone();
        stream.read_bytes_async(65536, glib::Priority::DEFAULT, Some(&cancellable), move |result| match result {
            Ok(bytes) if !bytes.is_empty() => {
                for value in parser.push(&bytes) {
                    if let Value::Array(objects) = value {
                        events(SourceEvent::Objects(objects));
                    }
                }
                Self::read(next, again, parser, events);
            }
            Ok(_) => events(SourceEvent::Disconnected),
            Err(error) => {
                if !error.matches(gio::IOErrorEnum::Cancelled) {
                    events(SourceEvent::Disconnected);
                }
            }
        });
    }
}

impl AudioSource for PwDumpSource {
    fn start(&self, events: Rc<dyn Fn(SourceEvent)>) -> Result<(), String> {
        let argv: Vec<&std::ffi::OsStr> = self.monitor.iter().map(std::ffi::OsStr::new).collect();
        let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE)
            .map_err(|_| "PipeWire is unavailable".to_owned())?;
        let stdout = process.stdout_pipe().ok_or_else(|| "PipeWire is unavailable".to_owned())?;
        *self.process.borrow_mut() = Some(process);
        Self::read(stdout, self.cancellable.clone(), JsonStream::default(), events);
        Ok(())
    }

    fn set_mute(&self, id: u32, mute: bool) -> bool {
        let mut argv = self.mute.clone();
        argv.extend([id.to_string(), if mute { "1" } else { "0" }.to_owned()]);
        self.spawner.spawn(&argv, None, Box::new(|_| {})).is_ok()
    }

    fn close(&self) {
        self.cancellable.cancel();
        if let Some(process) = self.process.borrow_mut().take() {
            process.force_exit();
        }
    }
}

/// Scripted source for tests: `emit` delivers events, `mutes` records writes.
#[derive(Default)]
pub struct FakeAudioSource {
    pub events: RefCell<Option<Rc<dyn Fn(SourceEvent)>>>,
    pub mutes: RefCell<Vec<(u32, bool)>>,
    pub fail_start: RefCell<Option<String>>,
    pub refuse_mute: Cell<bool>,
    pub closed: Cell<bool>,
}

impl FakeAudioSource {
    pub fn emit(&self, event: SourceEvent) {
        let handler = self.events.borrow().clone();
        if let Some(handler) = handler {
            handler(event);
        }
    }
}

impl AudioSource for FakeAudioSource {
    fn start(&self, events: Rc<dyn Fn(SourceEvent)>) -> Result<(), String> {
        if let Some(error) = self.fail_start.borrow().clone() {
            return Err(error);
        }
        *self.events.borrow_mut() = Some(events);
        Ok(())
    }
    fn set_mute(&self, id: u32, mute: bool) -> bool {
        self.mutes.borrow_mut().push((id, mute));
        !self.refuse_mute.get()
    }
    fn close(&self) {
        self.closed.set(true);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Node,
    Client,
    Other,
}

#[derive(Clone, Debug)]
struct PwObject {
    kind: Kind,
    serial: String,
    props: Props,
    /// `mixer.get-volume(id)['mute']`; `None` = no volume controls.
    mute: Option<bool>,
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn volume_mute(params: &Value) -> Option<bool> {
    let props = params.get("Props")?.as_array()?;
    let control = props.iter().find(|p| p.get("mute").is_some() || p.get("channelVolumes").is_some() || p.get("volume").is_some())?;
    Some(control.get("mute").and_then(Value::as_bool).unwrap_or(false))
}

/// One audible stream: (node id, object serial, muted).
pub type Stream = (u32, String, bool);

struct Inner {
    catalog: Rc<RefCell<AppCatalog>>,
    changed: RefCell<Rc<dyn Fn()>>,
    source: Rc<dyn AudioSource>,
    objects: RefCell<IndexMap<u32, PwObject>>,
    started: Cell<bool>,
    ready: Cell<bool>,
    closed: Cell<bool>,
    idle: RefCell<Option<suite_core::timer::Timeout>>,
    error: RefCell<String>,
}

/// `ApplicationAudio(catalog, changed)`.
#[derive(Clone)]
pub struct ApplicationAudio {
    inner: Rc<Inner>,
}

impl ApplicationAudio {
    pub fn new(catalog: Rc<RefCell<AppCatalog>>, changed: Rc<dyn Fn()>) -> Self {
        Self::with_source(catalog, changed, Rc::new(PwDumpSource::default()))
    }

    pub fn with_source(catalog: Rc<RefCell<AppCatalog>>, changed: Rc<dyn Fn()>, source: Rc<dyn AudioSource>) -> Self {
        Self {
            inner: Rc::new(Inner {
                catalog,
                changed: RefCell::new(changed),
                source,
                objects: RefCell::new(IndexMap::new()),
                started: Cell::new(false),
                ready: Cell::new(false),
                closed: Cell::new(false),
                idle: RefCell::new(None),
                error: RefCell::new(String::new()),
            }),
        }
    }

    pub fn ready(&self) -> bool {
        self.inner.ready.get()
    }
    pub fn error(&self) -> String {
        self.inner.error.borrow().clone()
    }
    pub fn is_closed(&self) -> bool {
        self.inner.closed.get()
    }
    /// Graph objects currently known (Python `len(proxies)`).
    pub fn object_count(&self) -> usize {
        self.inner.objects.borrow().len()
    }

    pub fn start(&self) {
        let inner = &self.inner;
        if inner.started.get() || inner.closed.get() {
            return;
        }
        inner.started.set(true);
        let weak: Weak<Inner> = Rc::downgrade(inner);
        let events: Rc<dyn Fn(SourceEvent)> = Rc::new(move |event| {
            if let Some(inner) = weak.upgrade() {
                Inner::event(&inner, event);
            }
        });
        if let Err(error) = inner.source.start(events) {
            *inner.error.borrow_mut() = error;
            Inner::notify(inner);
        }
    }

    /// Feed one source event directly (tests).
    pub fn consume(&self, event: SourceEvent) {
        Inner::event(&self.inner, event);
    }

    /// `_props(proxy)` for node `id`.
    fn props(&self, id: u32) -> Option<Props> {
        let objects = self.inner.objects.borrow();
        let node = objects.get(&id)?;
        let mut props: Props = KEYS.iter().filter_map(|k| node.props.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        let client = props.get("client.id").filter(|v| !v.is_empty()).map(|v| v.trim().parse::<i64>());
        let client = match client {
            Some(Ok(value)) => u32::try_from(value).ok().and_then(|id| objects.get(&id)),
            Some(Err(_)) => None,
            None => None,
        };
        if let Some(client) = client {
            for key in &KEYS[1..4] {
                if props.get(*key).is_none_or(|v| v.is_empty()) {
                    if let Some(value) = client.props.get(*key) {
                        props.insert(key.to_string(), value.clone());
                    }
                }
            }
        }
        Some(props)
    }

    /// `streams(app_id)`: audible playback nodes of the application.
    pub fn streams(&self, app_id: &str) -> Vec<Stream> {
        if !self.inner.ready.get() || self.inner.closed.get() {
            return Vec::new();
        }
        let nodes: Vec<(u32, String, Option<bool>)> = self
            .inner
            .objects
            .borrow()
            .iter()
            .filter(|(_, o)| o.kind == Kind::Node)
            .map(|(id, o)| (*id, o.serial.clone(), o.mute))
            .collect();
        let catalog = self.inner.catalog.borrow();
        nodes
            .into_iter()
            .filter(|(id, _, _)| self.props(*id).is_some_and(|props| matches_application(app_id, &props, &catalog)))
            .filter_map(|(id, serial, mute)| mute.map(|m| (id, serial, m)))
            .collect()
    }

    pub fn state(&self, app_id: &str) -> (bool, bool) {
        let streams = self.streams(app_id);
        (!streams.is_empty(), !streams.is_empty() && streams.iter().all(|s| s.2))
    }

    pub fn toggle(&self, app_id: &str) -> bool {
        let streams = self.streams(app_id);
        self.toggle_streams(&streams)
    }

    /// The write half of `toggle` over already listed streams.
    pub fn toggle_streams(&self, streams: &[Stream]) -> bool {
        let muted = !streams.iter().all(|s| s.2);
        let mut success = !streams.is_empty();
        for (id, serial, _) in streams {
            // A removed/reused PipeWire ID must never mute a different client.
            let same = self.inner.objects.borrow().get(id).is_some_and(|o| o.kind == Kind::Node && &o.serial == serial);
            if same {
                success = self.inner.source.set_mute(*id, muted) && success;
            }
        }
        Inner::notify(&self.inner);
        success
    }

    pub fn close(&self) {
        let inner = &self.inner;
        inner.closed.set(true);
        if let Some(idle) = inner.idle.borrow_mut().take() {
            idle.cancel();
        }
        inner.objects.borrow_mut().clear();
        inner.source.close();
        *inner.changed.borrow_mut() = Rc::new(|| {});
    }
}

impl Inner {
    fn notify(inner: &Rc<Inner>) {
        if inner.closed.get() || inner.idle.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(inner);
        // Thread-default context (GTK's main context in the dock).
        let id = suite_core::timer::once(0, move || {
            if let Some(inner) = weak.upgrade() {
                inner.idle.borrow_mut().take();
                if !inner.closed.get() {
                    let changed = inner.changed.borrow().clone();
                    changed();
                }
            }
        });
        *inner.idle.borrow_mut() = Some(id);
    }

    fn event(inner: &Rc<Inner>, event: SourceEvent) {
        if inner.closed.get() {
            return;
        }
        match event {
            SourceEvent::Disconnected => {
                inner.ready.set(false);
                *inner.error.borrow_mut() = "PipeWire is unavailable".into();
                inner.objects.borrow_mut().clear();
            }
            SourceEvent::Objects(objects) => {
                let mut known = inner.objects.borrow_mut();
                for object in objects {
                    let Some(id) = object.get("id").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()) else { continue };
                    let info = object.get("info").cloned().unwrap_or(Value::Null);
                    if info.is_null() {
                        known.shift_remove(&id);
                        continue;
                    }
                    let kind = match object.get("type").and_then(Value::as_str) {
                        Some("PipeWire:Interface:Node") => Kind::Node,
                        Some("PipeWire:Interface:Client") => Kind::Client,
                        _ => Kind::Other,
                    };
                    let previous = known.get(&id).filter(|o| o.kind == kind).cloned();
                    let props: Option<Props> = info
                        .get("props")
                        .and_then(Value::as_object)
                        .map(|map| map.iter().filter_map(|(k, v)| text(v).map(|t| (k.clone(), t))).collect());
                    let props = props.or_else(|| previous.as_ref().map(|p| p.props.clone())).unwrap_or_default();
                    let mute = match info.get("params") {
                        Some(params) if params.get("Props").is_some() => volume_mute(params),
                        _ => previous.as_ref().and_then(|p| p.mute),
                    };
                    let serial = props.get("object.serial").cloned().unwrap_or_default();
                    known.insert(id, PwObject { kind, serial, props, mute });
                }
                drop(known);
                inner.ready.set(true);
            }
        }
        Inner::notify(inner);
    }
}

impl AudioControl for ApplicationAudio {
    fn start(&self) {
        ApplicationAudio::start(self)
    }
    fn state(&self, app_id: &str) -> (bool, bool) {
        ApplicationAudio::state(self, app_id)
    }
    fn toggle(&self, app_id: &str) -> bool {
        ApplicationAudio::toggle(self, app_id)
    }
    fn close(&self) {
        ApplicationAudio::close(self)
    }
}

/// The English tooltip `update_mute_button` translates.
pub fn mute_tooltip(available: bool, muted: bool) -> &'static str {
    if !available {
        "No playback audio for this application"
    } else if muted {
        "Unmute application"
    } else {
        "Mute application"
    }
}

/// `update_mute_button(button, available, muted, tr)`.
pub fn update_mute_button(button: &gtk::Button, available: bool, muted: bool) {
    button.set_sensitive(available);
    if let Some(image) = button.child().and_downcast::<gtk::Image>() {
        image.set_icon_name(Some(if muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" }));
    }
    button.set_tooltip_text(Some(&suite_core::i18n::translate(mute_tooltip(available, muted))));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::app_catalog::AppInfoData;
    use serde_json::json;

    fn catalog() -> AppCatalog {
        AppCatalog::from_infos(vec![
            AppInfoData::fake("spotify.desktop", "Spotify"),
            AppInfoData::fake("org.remmina.Remmina.desktop", "Remmina"),
        ])
    }

    fn props(pairs: &[(&str, &str)]) -> Props {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// test_playback_only_and_desktop_identity_is_authoritative
    #[test]
    fn playback_only_and_desktop_identity_is_authoritative() {
        let catalog = catalog();
        let base = [("media.class", "Stream/Output/Audio"), ("application.process.binary", "spotify")];
        assert!(matches_application("Spotify", &props(&base), &catalog));
        let mut other = props(&base);
        other.insert("application.id".into(), "org.other.Player".into());
        assert!(!matches_application("Spotify", &other, &catalog));
        for kind in ["Audio/Sink", "Audio/Source", "Stream/Input/Audio"] {
            let mut wrong = props(&base);
            wrong.insert("media.class".into(), kind.into());
            assert!(!matches_application("Spotify", &wrong, &catalog));
        }
        assert!(!matches_application("Spotify", &props(&[("media.class", "Stream/Output/Audio"), ("media.name", "Spotify")]), &catalog));
        assert!(!matches_application("", &props(&base), &catalog));
        let remmina = props(&[("media.class", "Stream/Output/Audio"), ("application.process.binary", "/usr/bin/remmina")]);
        assert!(matches_application("org.remmina.Remmina", &remmina, &catalog));
        // Generic interpreters never identify an app; a portal app id does.
        assert!(!matches_application("python3", &props(&[("media.class", "Stream/Output/Audio"), ("application.process.binary", "python3")]), &catalog));
        let portal = props(&[("media.class", "Stream/Output/Audio"), ("pipewire.access.portal.app_id", "spotify"), ("application.process.binary", "x")]);
        assert!(matches_application("spotify.desktop", &portal, &catalog));
        assert_eq!(application_key(" Spotify ", &catalog), "spotify");
        assert_eq!(application_key("Unknown.App.desktop", &catalog), "unknown.app");
    }

    fn node(id: u32, serial: u32, client: Option<u32>, binary: Option<&str>, mute: Option<bool>) -> Value {
        let mut props = json!({"media.class": "Stream/Output/Audio", "object.serial": serial});
        if let Some(client) = client {
            props["client.id"] = json!(client);
        }
        if let Some(binary) = binary {
            props["application.process.binary"] = json!(binary);
        }
        let params = match mute {
            Some(m) => json!({"Props": [{"volume": 1.0, "mute": m, "channelVolumes": [1.0, 1.0]}]}),
            None => json!({}),
        };
        json!({"id": id, "type": "PipeWire:Interface:Node", "info": {"props": props, "params": params}})
    }

    fn audio() -> (ApplicationAudio, Rc<FakeAudioSource>, Rc<Cell<usize>>) {
        let source = Rc::new(FakeAudioSource::default());
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        let audio = ApplicationAudio::with_source(Rc::new(RefCell::new(catalog())), Rc::new(move || counter.set(counter.get() + 1)), source.clone());
        (audio, source, calls)
    }

    fn settle() {
        let context = glib::MainContext::ref_thread_default();
        while context.iteration(false) {}
    }

    /// Run on a private main context (tests share the process).
    fn private<T>(f: impl FnOnce() -> T) -> T {
        glib::MainContext::new().with_thread_default(f).unwrap()
    }

    /// test_mixed_streams_mute_together_then_unmute_and_guard_reused_ids
    #[test]
    fn mixed_streams_mute_together_then_unmute_and_guard_reused_ids() { private(|| {
        let (audio, source, _calls) = audio();
        audio.start();
        assert!(!audio.ready());
        assert_eq!(audio.state("Spotify"), (false, false));
        source.emit(SourceEvent::Objects(vec![
            json!({"id": 30, "type": "PipeWire:Interface:Client", "info": {"props": {"application.process.binary": "spotify"}}}),
            node(4, 104, None, Some("spotify"), Some(false)),
            node(5, 105, Some(30), None, Some(true)),
            node(6, 106, None, Some("spotify"), None),
            node(7, 107, None, Some("remmina"), Some(true)),
        ]));
        assert!(audio.ready());
        assert_eq!(audio.streams("Spotify"), vec![(4, "104".into(), false), (5, "105".into(), true)]);
        assert_eq!(audio.state("Spotify"), (true, false));
        assert!(audio.toggle("Spotify"));
        assert_eq!(*source.mutes.borrow(), vec![(4, true), (5, true)]);
        source.mutes.borrow_mut().clear();
        // Id 5 is reused by another node before the write: never touched.
        let listed = vec![(4, "104".to_owned(), true), (5, "105".to_owned(), true)];
        source.emit(SourceEvent::Objects(vec![node(5, 999, None, Some("other"), Some(true))]));
        audio.toggle_streams(&listed);
        assert_eq!(*source.mutes.borrow(), vec![(4, false)]);
        // Removal.
        source.emit(SourceEvent::Objects(vec![json!({"id": 4, "info": null})]));
        assert_eq!(audio.state("Spotify"), (false, false));
        assert!(!audio.toggle("Spotify"));
        // Disconnect clears everything and reports the Python error text.
        source.emit(SourceEvent::Disconnected);
        assert!(!audio.ready());
        assert_eq!(audio.error(), "PipeWire is unavailable");
        assert_eq!(audio.object_count(), 0);
        source.emit(SourceEvent::Objects(vec![node(8, 108, None, Some("spotify"), Some(true))]));
        assert_eq!(audio.state("Spotify"), (true, true));
        audio.close();
        assert_eq!(audio.object_count(), 0);
        assert!(source.closed.get());
        source.emit(SourceEvent::Objects(vec![node(9, 109, None, Some("spotify"), Some(true))]));
        assert_eq!(audio.object_count(), 0);
    })}

    #[test]
    fn notifications_are_coalesced_and_stop_after_close() { private(|| {
        let (audio, source, calls) = audio();
        audio.start();
        source.emit(SourceEvent::Objects(vec![node(4, 104, None, Some("spotify"), Some(false))]));
        source.emit(SourceEvent::Objects(vec![node(4, 104, None, Some("spotify"), Some(true))]));
        settle();
        assert_eq!(calls.get(), 1);
        // A props-less update keeps known props; a params update changes mute.
        source.emit(SourceEvent::Objects(vec![json!({"id": 4, "type": "PipeWire:Interface:Node", "info": {"params": {"Props": [{"mute": false, "volume": 1.0}]}}})]));
        assert_eq!(audio.state("Spotify"), (true, false));
        audio.close();
        settle();
        assert_eq!(calls.get(), 1);
    })}

    #[test]
    fn start_failure_reports_error() { private(|| {
        let source = Rc::new(FakeAudioSource::default());
        *source.fail_start.borrow_mut() = Some("PipeWire is unavailable".into());
        let audio = ApplicationAudio::with_source(Rc::new(RefCell::new(catalog())), Rc::new(|| {}), source);
        audio.start();
        assert_eq!(audio.error(), "PipeWire is unavailable");
        assert_eq!(audio.state("Spotify"), (false, false));
    })}

    #[test]
    fn json_stream_splits_concatenated_documents() {
        let mut stream = JsonStream::default();
        assert!(stream.push(b"[\n  {\"id\": 1,").is_empty());
        let values = stream.push(b" \"info\": null }\n]\n[ {\"id\": 2} ]\n[");
        assert_eq!(values, vec![json!([{"id": 1, "info": null}]), json!([{"id": 2}])]);
        assert_eq!(stream.push(b"]"), vec![json!([])]);
        // Garbage resynchronises at the next line.
        assert_eq!(stream.push(b"garbage\n[1]"), vec![json!([1])]);
    }

    /// The real source over a fixture "monitor" (cat of a recorded dump):
    /// never touches PipeWire; mute writes go to a fake spawner.
    #[test]
    fn pw_dump_source_reads_a_fixture_stream() { private(|| {
        let dir = std::env::temp_dir().join(format!("waydock-f1-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("dump.json");
        let first = json!([node(4, 104, None, Some("spotify"), Some(false))]);
        let second = json!([node(4, 104, None, Some("spotify"), Some(true))]);
        std::fs::write(&fixture, format!("{}\n{}\n", serde_json::to_string_pretty(&first).unwrap(), second)).unwrap();
        let spawner = Rc::new(crate::core::spawn::FakeSpawner::new());
        let source = Rc::new(PwDumpSource::new(vec!["/usr/bin/cat".into(), fixture.to_string_lossy().into()],
                                               vec!["wpctl".into(), "set-mute".into()], spawner.clone()));
        let audio = ApplicationAudio::with_source(Rc::new(RefCell::new(catalog())), Rc::new(|| {}), source);
        audio.start();
        let context = glib::MainContext::ref_thread_default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !audio.error().contains("unavailable") && std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        // Both documents parse; EOF then reports the disconnection.
        assert_eq!(audio.error(), "PipeWire is unavailable");
        audio.consume(SourceEvent::Objects(vec![node(4, 104, None, Some("spotify"), Some(true))]));
        assert!(audio.toggle("Spotify"));
        assert_eq!(spawner.last_argv(), vec!["wpctl", "set-mute", "4", "0"]);
        audio.close();
        let _ = std::fs::remove_dir_all(&dir);
    })}

    #[test]
    fn tooltips_follow_state() {
        assert_eq!(mute_tooltip(false, true), "No playback audio for this application");
        assert_eq!(mute_tooltip(true, true), "Unmute application");
        assert_eq!(mute_tooltip(true, false), "Mute application");
    }
}
