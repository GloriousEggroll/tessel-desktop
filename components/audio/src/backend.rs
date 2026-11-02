//! Native PipeWire discovery and WirePlumber 0.5 audio controls
//! (audio_backend.py:245-806).
//!
//! WirePlumber owns the mixer, default-device policy and stream routing. One
//! long-running `pw-dump --monitor` streams incremental object updates that are
//! merged into a local graph; WirePlumber registry and mixer events supply
//! proxies and live volumes. The WirePlumber side is behind `Connector` so the
//! state machine is testable without PipeWire (see `wp` for the real one).

use crate::api::{AudioApi, AudioState, LevelCallback, LevelMeter, Outcome};
use crate::graph::{merge_update, parse_snapshot, split_batches, Graph};
use crate::model::target_available;
use crate::store::{LoopbackMonitor, LoopbackStore};
use crate::timer::{self, monotonic, Timeout};
use gio::prelude::*;
use indexmap::IndexMap;
use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

/// Own volume writes are echoed by the mixer and the graph monitor; within
/// this window echoes update the model only and views reconcile afterwards.
pub const ECHO_WINDOW: f64 = 0.35;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Bool(bool),
}

/// A bound WirePlumber proxy (Node, Device, Link or Metadata).
pub trait NativeProxy {
    fn bound_id(&self) -> i64;
    /// `Wp.PipewireObject.get_property(proxy, key)`.
    fn property(&self, key: &str) -> Option<String>;
    /// `set_param(id, flags, Spa:Pod:Object:Param:<id> {values})`.
    fn set_param(&self, id: &str, flags: u32, values: &[(&str, ParamValue)]) -> bool;
    /// `Wp.Metadata.set(subject, key, type, value)` (metadata proxies only).
    fn metadata_set(&self, _subject: i64, _key: &str, _type: &str, _value: &str) {}
    /// Identity of the underlying object (Python `==` on proxies).
    fn key(&self) -> usize;
}

/// `get-volume` data of the mixer-api plugin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixerVolume {
    pub volume: Option<f64>,
    pub mute: Option<bool>,
    /// (`"<index>"`, volume, channel name) of `channelVolumes`.
    pub channels: Vec<(String, Option<f64>, Option<String>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PendingValue {
    Volume(f64),
    Mute(bool),
    /// `{"<index>": {"volume": d}}`
    Channels(IndexMap<String, f64>),
}

/// The `a{sv}` given to mixer `set-volume`, in Python dict order.
pub type PendingValues = IndexMap<&'static str, PendingValue>;

pub trait NativeMixer {
    fn get_volume(&self, id: i64) -> Option<MixerVolume>;
    fn set_volume(&self, id: i64, values: &PendingValues) -> bool;
}

pub trait NativeDefaults {
    fn set_default_configured_node_name(&self, media_class: &str, name: &str) -> bool;
}

pub enum Plugin {
    Mixer(Rc<dyn NativeMixer>),
    Defaults(Rc<dyn NativeDefaults>),
}

pub enum Event {
    /// `_component_loaded`: plugin found and configured, activation started.
    ComponentLoaded(&'static str, Result<Plugin, String>),
    PluginActivated(&'static str, Result<(), String>),
    /// (proxy, is the "default" metadata)
    ObjectAdded(Rc<dyn NativeProxy>, bool),
    ObjectRemoved(usize),
    Installed,
    MixerChanged(i64),
    DefaultsChanged,
    Disconnected,
}

pub type EventSink = Rc<dyn Fn(Event)>;

pub trait Connection {
    fn disconnect(&self);
}

/// `Wp.init` … `install_object_manager`: events arrive asynchronously.
pub trait Connector {
    fn connect(&self, events: EventSink) -> Result<Box<dyn Connection>, String>;
}

pub type MeterFactory = Rc<dyn Fn(i64, LevelCallback) -> Result<Box<dyn LevelMeter>, String>>;

struct Pending {
    serial: i64,
    values: PendingValues,
}

pub struct BackendOptions {
    pub monitor_loopbacks: bool,
    pub loopback_store: LoopbackStore,
    pub connector: Rc<dyn Connector>,
    pub dump_path: Option<PathBuf>,
    pub meter: Option<MeterFactory>,
}

impl BackendOptions {
    /// The production configuration (WirePlumber + `pw-dump` from PATH).
    pub fn native(monitor_loopbacks: bool) -> Self {
        Self {
            monitor_loopbacks,
            loopback_store: LoopbackStore::default(),
            connector: Rc::new(crate::wp::WpConnector),
            dump_path: glib::find_program_in_path("pw-dump"),
            meter: Some(Rc::new(|serial, callback| crate::level::PeakMonitor::start(serial, callback).map(|m| Box::new(m) as Box<dyn LevelMeter>))),
        }
    }
}

pub struct Inner {
    weak: RefCell<Weak<Inner>>,
    state: RefCell<AudioState>,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    connector: Rc<dyn Connector>,
    connection: RefCell<Option<Box<dyn Connection>>>,
    mixer: RefCell<Option<Rc<dyn NativeMixer>>>,
    default_api: RefCell<Option<Rc<dyn NativeDefaults>>>,
    metadata: RefCell<Option<Rc<dyn NativeProxy>>>,
    proxies: RefCell<IndexMap<i64, Rc<dyn NativeProxy>>>,
    plugins: RefCell<HashSet<&'static str>>,
    closed: Cell<bool>,
    generation: Cell<u64>,
    sources: RefCell<HashMap<&'static str, Timeout>>,
    pending: RefCell<IndexMap<i64, Pending>>,
    monitor_process: RefCell<Option<gio::Subprocess>>,
    monitor_cancel: RefCell<Option<gio::Cancellable>>,
    monitor_buffer: RefCell<Vec<u8>>,
    monitor_failures: Cell<u32>,
    graph: RefCell<Graph>,
    echo: RefCell<HashMap<i64, f64>>,
    echo_dirty: Cell<bool>,
    snapshot_received: Cell<bool>,
    dump_path: Option<PathBuf>,
    loopback_store: LoopbackStore,
    monitor_loopbacks: bool,
    loopback_monitor: RefCell<Option<LoopbackMonitor>>,
    meter: Option<MeterFactory>,
    /// Tests: record `_later` names instead of scheduling (Python mocks `_later`).
    pub(crate) record_later: Cell<bool>,
    /// `pw-dump --monitor` processes started (tests: one per connection).
    pub snapshots_spawned: Cell<u32>,
    pub(crate) later_log: RefCell<Vec<&'static str>>,
}

/// The shared native backend (`AudioBackend`). Cheap to clone.
#[derive(Clone)]
pub struct AudioBackend(pub(crate) Rc<Inner>);

type Fallible<T> = Result<T, String>;

fn int(text: Option<String>) -> Option<i64> {
    tessel_core::pyjson::py_int(text.map(serde_json::Value::String).as_ref())
}

impl AudioBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static, options: BackendOptions) -> Self {
        let inner = Rc::new(Inner {
            weak: RefCell::new(Weak::new()),
            state: RefCell::default(),
            on_changed: Box::new(on_changed),
            on_error: Box::new(on_error),
            connector: options.connector,
            connection: RefCell::new(None),
            mixer: RefCell::new(None),
            default_api: RefCell::new(None),
            metadata: RefCell::new(None),
            proxies: RefCell::default(),
            plugins: RefCell::default(),
            closed: Cell::new(false),
            generation: Cell::new(0),
            sources: RefCell::default(),
            pending: RefCell::default(),
            monitor_process: RefCell::new(None),
            monitor_cancel: RefCell::new(None),
            monitor_buffer: RefCell::default(),
            monitor_failures: Cell::new(0),
            graph: RefCell::default(),
            echo: RefCell::default(),
            echo_dirty: Cell::new(false),
            snapshot_received: Cell::new(false),
            dump_path: options.dump_path,
            loopback_store: options.loopback_store,
            monitor_loopbacks: options.monitor_loopbacks,
            loopback_monitor: RefCell::new(None),
            meter: options.meter,
            record_later: Cell::new(false),
            snapshots_spawned: Cell::new(0),
            later_log: RefCell::default(),
        });
        *inner.weak.borrow_mut() = Rc::downgrade(&inner);
        Self(inner)
    }

    fn weak(&self) -> Weak<Inner> {
        self.0.weak.borrow().clone()
    }

    fn with_self(weak: &Weak<Inner>, f: impl FnOnce(AudioBackend)) {
        if let Some(inner) = weak.upgrade() {
            f(AudioBackend(inner));
        }
    }

    pub fn ready(&self) -> bool {
        self.0.state.borrow().ready
    }

    pub fn error(&self) -> String {
        self.0.state.borrow().error.clone()
    }

    pub fn state_mut(&self) -> std::cell::RefMut<'_, AudioState> {
        self.0.state.borrow_mut()
    }

    /// Number of `pw-dump --monitor` processes started so far.
    pub fn snapshots_spawned(&self) -> u32 {
        self.0.snapshots_spawned.get()
    }

    /// The merged raw graph object (tests).
    pub fn raw_object(&self, id: i64) -> Option<serde_json::Value> {
        self.0.graph.borrow().get(&id).cloned()
    }

    pub fn store(&self) -> &LoopbackStore {
        &self.0.loopback_store
    }

    fn later(&self, name: &'static str, delay: u32, callback: impl FnOnce(&AudioBackend) + 'static) {
        if self.0.closed.get() || self.0.sources.borrow().contains_key(name) {
            return;
        }
        if self.0.record_later.get() {
            self.0.later_log.borrow_mut().push(name);
            return;
        }
        let weak = self.weak();
        let source = timer::once(delay, move || {
            Self::with_self(&weak, |backend| {
                backend.0.sources.borrow_mut().remove(name);
                if !backend.0.closed.get() {
                    callback(&backend);
                }
            })
        });
        self.0.sources.borrow_mut().insert(name, source);
    }

    fn cancel_source(&self, name: &str) {
        if let Some(source) = self.0.sources.borrow_mut().remove(name) {
            source.cancel();
        }
    }

    fn changed(&self) {
        self.later("notify", 25, |b| (b.0.on_changed)());
    }

    fn fail(&self, message: &str) {
        if self.0.closed.get() {
            return;
        }
        self.0.state.borrow_mut().error = message.to_owned();
        (self.0.on_error)(message);
        self.changed();
    }

    // ----- connection lifecycle -------------------------------------------

    pub fn start(&self) {
        if self.0.connection.borrow().is_some() || self.0.closed.get() {
            return;
        }
        if self.0.monitor_loopbacks && self.0.loopback_monitor.borrow().is_none() {
            let weak = self.weak();
            match LoopbackMonitor::new(&self.0.loopback_store, move || Self::with_self(&weak, |b| b.reload_loopbacks())) {
                Ok(monitor) => {
                    *self.0.loopback_monitor.borrow_mut() = Some(monitor);
                    self.reload_loopbacks();
                }
                Err(error) => {
                    let message = format!("Cannot monitor saved loopbacks: {error}");
                    self.0.state.borrow_mut().loopback_error = message.clone();
                    self.fail(&message);
                }
            }
        }
        if self.0.dump_path.is_none() {
            self.fail("The PipeWire tool pw-dump is missing. Install pipewire-utils and reopen Audio Settings.");
            return;
        }
        self.0.generation.set(self.0.generation.get() + 1);
        let generation = self.0.generation.get();
        self.0.plugins.borrow_mut().clear();
        self.0.snapshot_received.set(false);
        self.0.graph.borrow_mut().clear();
        let weak = self.weak();
        let sink: EventSink = Rc::new(move |event| Self::with_self(&weak, |b| b.event(generation, event)));
        match self.0.connector.connect(sink) {
            Ok(connection) => {
                *self.0.connection.borrow_mut() = Some(connection);
                self.later("startup", 5000, |b| b.startup_timeout());
            }
            Err(error) => {
                self.fail(&error);
                self.disconnect_core();
                self.later("reconnect", 2500, |b| b.start());
            }
        }
    }

    fn startup_timeout(&self) {
        if !self.ready() {
            self.fail("Audio services did not become ready. Check PipeWire and WirePlumber; reconnecting…");
            self.disconnect_core();
            self.later("reconnect", 2500, |b| b.start());
        }
    }

    /// Dispatch a connector event of `generation`; retired connections are ignored.
    pub fn event(&self, generation: u64, event: Event) {
        if self.0.closed.get() || generation != self.0.generation.get() {
            return;
        }
        match event {
            Event::ComponentLoaded(name, result) => match result {
                Ok(Plugin::Mixer(mixer)) => *self.0.mixer.borrow_mut() = Some(mixer),
                Ok(Plugin::Defaults(api)) => *self.0.default_api.borrow_mut() = Some(api),
                Err(error) => {
                    let _ = name;
                    self.fail(&format!("Cannot initialize native audio controls: {error}"));
                }
            },
            Event::PluginActivated(name, result) => match result {
                Ok(()) => {
                    self.0.plugins.borrow_mut().insert(name);
                    if self.0.snapshot_received.get() {
                        self.apply_graph();
                    }
                    self.request_snapshot();
                }
                Err(error) => self.fail(&format!("Cannot activate WirePlumber audio controls: {error}")),
            },
            Event::ObjectAdded(proxy, default_metadata) => self.object_added(proxy, default_metadata),
            Event::ObjectRemoved(key) => self.object_removed(key),
            Event::Installed | Event::DefaultsChanged => self.request_snapshot(),
            Event::MixerChanged(id) => self.mixer_changed(id),
            Event::Disconnected => {
                self.fail("Connection to PipeWire was lost. Reconnecting…");
                self.disconnect_core();
                self.later("reconnect", 2000, |b| b.start());
            }
        }
    }

    fn object_added(&self, proxy: Rc<dyn NativeProxy>, default_metadata: bool) {
        let id = proxy.bound_id();
        if default_metadata {
            *self.0.metadata.borrow_mut() = Some(proxy.clone());
        }
        self.0.proxies.borrow_mut().insert(id, proxy);
        // Info, params and metadata changes arrive through the graph monitor;
        // the registry only provides proxies for writes.
        self.request_snapshot();
    }

    fn object_removed(&self, key: usize) {
        let ids: Vec<i64> = self.0.proxies.borrow().iter().filter(|(_, p)| p.key() == key).map(|(id, _)| *id).collect();
        for id in ids {
            self.0.proxies.borrow_mut().shift_remove(&id);
            let mut state = self.0.state.borrow_mut();
            state.nodes.shift_remove(&id);
            state.devices.shift_remove(&id);
            drop(state);
            self.0.pending.borrow_mut().shift_remove(&id);
        }
        let is_metadata = self.0.metadata.borrow().as_ref().is_some_and(|m| m.key() == key);
        if is_metadata {
            *self.0.metadata.borrow_mut() = None;
        }
        self.changed();
        self.request_snapshot();
    }

    fn mixer_changed(&self, id: i64) {
        if self.0.mixer.borrow().is_none() || !self.0.state.borrow().nodes.contains_key(&id) {
            return;
        }
        self.read_volume(id);
        if self.echoing(id) {
            self.0.echo_dirty.set(true);
            self.later("echo", (ECHO_WINDOW * 1000.0) as u32, |b| b.echo_settled());
        } else {
            self.changed();
        }
    }

    fn read_volume(&self, id: i64) {
        let mixer = self.0.mixer.borrow().clone();
        let Some(mixer) = mixer.filter(|_| self.0.plugins.borrow().contains("mixer-api")) else { return };
        let data = mixer.get_volume(id);
        let mut state = self.0.state.borrow_mut();
        let Some(node) = state.nodes.get_mut(&id) else { return };
        node.volume_available = data.is_some();
        let Some(data) = data else { return };
        let mut ordered = data.channels.clone();
        ordered.sort_by_key(|(key, _, _)| tessel_core::pyjson::py_int(Some(&serde_json::Value::String(key.clone()))).unwrap_or(0));
        node.channel_volumes = ordered.iter().map(|(_, v, _)| v.unwrap_or(0.0)).collect();
        node.channels = ordered
            .iter()
            .enumerate()
            .map(|(i, (_, _, c))| c.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| format!("Channel {}", i + 1)))
            .collect();
        node.volume = node.channel_volumes.iter().copied().reduce(f64::max).unwrap_or(data.volume.unwrap_or(1.0));
        node.mute = data.mute.unwrap_or(false);
    }

    fn echoing(&self, id: i64) -> bool {
        let deadline = self.0.echo.borrow().get(&id).copied();
        match deadline {
            None => false,
            Some(deadline) if monotonic() < deadline => true,
            Some(_) => {
                self.0.echo.borrow_mut().remove(&id);
                false
            }
        }
    }

    fn echo_settled(&self) {
        let now = monotonic();
        self.0.echo.borrow_mut().retain(|_, deadline| *deadline > now);
        let latest = self.0.echo.borrow().values().copied().reduce(f64::max);
        if let Some(latest) = latest {
            let delay = 1.max(((latest - now) * 1000.0) as i64 + 1) as u32;
            self.later("echo", delay, |b| b.echo_settled());
        } else if self.0.echo_dirty.get() {
            self.0.echo_dirty.set(false);
            self.apply_graph();
        }
    }

    /// Ensure the graph monitor runs; it already streams every change.
    fn request_snapshot(&self) {
        if self.0.closed.get() || self.0.connection.borrow().is_none() || self.0.monitor_process.borrow().is_some() {
            return;
        }
        self.later("snapshot", 120, |b| b.snapshot());
    }

    fn snapshot(&self) {
        if self.0.monitor_process.borrow().is_some() || self.0.connection.borrow().is_none() {
            return;
        }
        let Some(dump) = self.0.dump_path.clone() else { return };
        let generation = self.0.generation.get();
        let argv = [dump.as_os_str(), "--monitor".as_ref(), "--no-colors".as_ref()];
        let process = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) {
            Ok(process) => process,
            Err(error) => {
                self.fail(&format!("Cannot read PipeWire devices: {}", crate::wp::gerror_str(&error)));
                return;
            }
        };
        self.0.snapshots_spawned.set(self.0.snapshots_spawned.get() + 1);
        *self.0.monitor_process.borrow_mut() = Some(process.clone());
        self.0.monitor_buffer.borrow_mut().clear();
        *self.0.monitor_cancel.borrow_mut() = Some(gio::Cancellable::new());
        self.0.graph.borrow_mut().clear();
        self.0.snapshot_received.set(false);
        let watched = process.clone();
        self.later("snapshot_timeout", 5000, move |b| b.terminate_snapshot(&watched));
        self.read_monitor(process, generation);
    }

    fn read_monitor(&self, process: gio::Subprocess, generation: u64) {
        let Some(stdout) = process.stdout_pipe() else { return };
        let cancel = self.0.monitor_cancel.borrow().clone();
        let weak = self.weak();
        stdout.read_bytes_async(1 << 16, glib::Priority::DEFAULT, cancel.as_ref(), move |result| {
            let data = match result {
                Ok(bytes) => bytes.to_vec(),
                Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => return,
                Err(_) => Vec::new(),
            };
            Self::with_self(&weak, |b| b.monitor_data(process, generation, data));
        });
    }

    fn terminate_snapshot(&self, process: &gio::Subprocess) {
        if self.0.monitor_process.borrow().as_ref() == Some(process) && !self.0.snapshot_received.get() {
            process.force_exit();
        }
    }

    fn monitor_data(&self, process: gio::Subprocess, generation: u64, data: Vec<u8>) {
        if self.0.closed.get() || generation != self.0.generation.get() || self.0.monitor_process.borrow().as_ref() != Some(&process) {
            return;
        }
        if data.is_empty() {
            self.monitor_exited();
            return;
        }
        match self.feed(&data) {
            Ok(()) => self.read_monitor(process, generation),
            Err(error) => {
                self.0.monitor_buffer.borrow_mut().clear();
                self.fail(&format!("Cannot read PipeWire devices: {error}"));
                process.force_exit();
            }
        }
    }

    /// Append monitor output and apply every complete batch.
    pub(crate) fn feed(&self, data: &[u8]) -> Fallible<()> {
        let mut buffer = std::mem::take(&mut *self.0.monitor_buffer.borrow_mut());
        buffer.extend_from_slice(data);
        let (batches, rest) = split_batches(buffer);
        *self.0.monitor_buffer.borrow_mut() = rest;
        for batch in batches {
            let objects: serde_json::Value = serde_json::from_slice(&batch).map_err(|e| e.to_string())?;
            self.apply_batch(&objects);
        }
        Ok(())
    }

    pub(crate) fn apply_batch(&self, objects: &serde_json::Value) {
        let (changed, props_only) = merge_update(&mut self.0.graph.borrow_mut(), objects);
        let first = !self.0.snapshot_received.get();
        if first {
            self.0.snapshot_received.set(true);
            self.0.monitor_failures.set(0);
            self.cancel_source("snapshot_timeout");
        } else if changed.is_empty() {
            return;
        } else if changed.iter().all(|id| props_only.contains(id)) && changed.iter().all(|id| self.echoing(*id)) {
            // Our own slider writes: keep the merged graph, defer the views.
            self.0.echo_dirty.set(true);
            self.later("echo", (ECHO_WINDOW * 1000.0) as u32, |b| b.echo_settled());
            return;
        }
        if first {
            self.apply_graph();
        } else {
            self.later("graph", 20, |b| b.apply_graph());
        }
    }

    fn apply_graph(&self) {
        if self.0.closed.get() || self.0.connection.borrow().is_none() || !self.0.snapshot_received.get() {
            return;
        }
        let (nodes, devices, defaults) = parse_snapshot(self.0.graph.borrow().values());
        let ids: Vec<i64> = nodes.keys().copied().collect();
        {
            let mut state = self.0.state.borrow_mut();
            state.nodes = nodes;
            state.devices = devices;
            state.defaults = defaults;
        }
        for id in ids {
            self.read_volume(id);
        }
        let ready = self.0.plugins.borrow().len() == 2;
        {
            let mut state = self.0.state.borrow_mut();
            state.ready = ready;
            if ready {
                state.error = state.loopback_error.clone();
            }
        }
        if ready {
            self.cancel_source("startup");
        }
        self.changed();
    }

    fn monitor_exited(&self) {
        *self.0.monitor_process.borrow_mut() = None;
        self.cancel_source("snapshot_timeout");
        if let Some(cancel) = self.0.monitor_cancel.borrow_mut().take() {
            cancel.cancel();
        }
        let failures = self.0.monitor_failures.get() + 1;
        self.0.monitor_failures.set(failures);
        if failures >= 3 {
            self.0.state.borrow_mut().ready = false;
            self.fail("Cannot read PipeWire devices: the PipeWire graph monitor stopped.");
        }
        // Keep the last known graph visible while the monitor restarts.
        let delay = 10000u64.min(500u64.saturating_mul(1u64 << failures.min(20)));
        self.later("snapshot", delay as u32, |b| b.snapshot());
    }

    pub(crate) fn disconnect_core(&self) {
        self.0.generation.set(self.0.generation.get() + 1);
        for name in ["startup", "snapshot", "snapshot_timeout", "graph", "echo", "volume"] {
            self.cancel_source(name);
        }
        {
            let mut state = self.0.state.borrow_mut();
            state.ready = false;
            state.nodes.clear();
            state.devices.clear();
            state.defaults.clear();
        }
        self.0.echo.borrow_mut().clear();
        self.0.echo_dirty.set(false);
        self.0.graph.borrow_mut().clear();
        self.0.snapshot_received.set(false);
        self.0.pending.borrow_mut().clear();
        self.0.proxies.borrow_mut().clear();
        *self.0.metadata.borrow_mut() = None;
        *self.0.mixer.borrow_mut() = None;
        *self.0.default_api.borrow_mut() = None;
        self.0.plugins.borrow_mut().clear();
        if let Some(cancel) = self.0.monitor_cancel.borrow_mut().take() {
            cancel.cancel();
        }
        if let Some(process) = self.0.monitor_process.borrow_mut().take() {
            process.force_exit();
        }
        self.0.monitor_buffer.borrow_mut().clear();
        let connection = self.0.connection.borrow_mut().take();
        if let Some(connection) = connection {
            connection.disconnect();
        }
        self.changed();
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        // Deliver the final slider position while the registered object is
        // still alive; never retain a write across reconnects.
        self.flush_volumes();
        self.0.closed.set(true);
        if let Some(monitor) = self.0.loopback_monitor.borrow_mut().take() {
            monitor.close();
        }
        let names: Vec<&'static str> = self.0.sources.borrow().keys().copied().collect();
        for name in names {
            self.cancel_source(name);
        }
        self.disconnect_core();
    }

    // ----- writes -----------------------------------------------------------

    fn checked_proxy(&self, id: Option<i64>, model_serial: Option<i64>) -> Fallible<Rc<dyn NativeProxy>> {
        let state = self.0.state.borrow();
        if !state.ready || self.0.closed.get() {
            return Err("Audio controls are not connected yet.".into());
        }
        let serial = model_serial.or_else(|| {
            let id = id?;
            state.nodes.get(&id).map(|n| n.serial).or_else(|| state.devices.get(&id).map(|d| d.serial))
        });
        drop(state);
        let proxy = id.and_then(|id| self.0.proxies.borrow().get(&id).cloned());
        let (Some(serial), Some(proxy)) = (serial, proxy) else {
            return Err("This audio device or stream is no longer available.".into());
        };
        if int(proxy.property("object.serial")) != Some(serial) || Some(proxy.bound_id()) != id {
            return Err("This audio device changed. Please select it again.".into());
        }
        Ok(proxy)
    }

    fn action(&self, body: impl FnOnce() -> Fallible<()>) -> Outcome {
        match body() {
            Ok(()) => Ok(true),
            Err(error) => {
                self.fail(&error);
                Ok(false)
            }
        }
    }

    fn validate_volume(value: f64) -> Fallible<f64> {
        if !value.is_finite() || !(0.0..=1.5).contains(&value) {
            return Err("Volume must be between 0% and 150%.".into());
        }
        Ok(value)
    }

    fn queue_volume(&self, id: i64, field: &'static str, value: PendingValue) -> Fallible<()> {
        self.checked_proxy(Some(id), None)?;
        let (serial, available) = {
            let state = self.0.state.borrow();
            let node = state.nodes.get(&id).ok_or("This audio device or stream is no longer available.")?;
            (node.serial, node.volume_available)
        };
        if !available {
            return Err("This audio stream does not expose a volume control.".into());
        }
        {
            let mut pending = self.0.pending.borrow_mut();
            let entry = pending.entry(id).or_insert_with(|| Pending { serial, values: PendingValues::new() });
            if field == "volume" {
                // Master adjustment links channels; later individual channel
                // writes can override a channel without losing the master value.
                entry.values.shift_remove("channelVolumes");
            }
            entry.values.insert(field, value);
        }
        self.later("volume", 45, |b| b.flush_volumes());
        Ok(())
    }

    pub fn set_volume(&self, id: i64, value: f64) -> Outcome {
        self.action(|| self.queue_volume(id, "volume", PendingValue::Volume(Self::validate_volume(value)?)))
    }

    pub fn set_mute(&self, id: i64, muted: bool) -> Outcome {
        self.action(|| {
            self.queue_volume(id, "mute", PendingValue::Mute(muted))?;
            self.flush_volumes();
            Ok(())
        })
    }

    pub fn set_channel_volume(&self, id: i64, index: i64, value: f64) -> Outcome {
        self.action(|| {
            let value = Self::validate_volume(value)?;
            self.checked_proxy(Some(id), None)?;
            let count = self.0.state.borrow().nodes.get(&id).map_or(0, |n| n.channel_volumes.len());
            if index < 0 || index as usize >= count {
                return Err("The selected audio channel is no longer available.".into());
            }
            let mut channels = match self.0.pending.borrow().get(&id).and_then(|p| p.values.get("channelVolumes")) {
                Some(PendingValue::Channels(current)) => current.clone(),
                _ => IndexMap::new(),
            };
            channels.insert(index.to_string(), value);
            self.queue_volume(id, "channelVolumes", PendingValue::Channels(channels))
        })
    }

    pub(crate) fn flush_volumes(&self) {
        let pending = std::mem::take(&mut *self.0.pending.borrow_mut());
        for (id, command) in pending {
            let _ = self.action(|| {
                let serial = self.0.state.borrow().nodes.get(&id).map(|n| n.serial);
                if serial != Some(command.serial) {
                    return Ok(());
                }
                self.checked_proxy(Some(id), serial)?;
                // The mixer may report the change before set-volume returns.
                self.0.echo.borrow_mut().insert(id, monotonic() + ECHO_WINDOW);
                let mixer = self.0.mixer.borrow().clone();
                if !mixer.is_some_and(|m| m.set_volume(id, &command.values)) {
                    return Err("WirePlumber could not change this volume. The device may have disconnected.".into());
                }
                Ok(())
            });
        }
    }

    pub fn set_default(&self, id: i64) -> Outcome {
        self.action(|| {
            self.checked_proxy(Some(id), None)?;
            let (media_class, name) = {
                let state = self.0.state.borrow();
                let node = state.nodes.get(&id).ok_or("Only an output or input device can be the system default.")?;
                let class = if node.media_class == "Audio/Source/Virtual" { "Audio/Source".to_owned() } else { node.media_class.clone() };
                (class, node.name.clone())
            };
            if media_class != "Audio/Sink" && media_class != "Audio/Source" {
                return Err("Only an output or input device can be the system default.".into());
            }
            let api = self.0.default_api.borrow().clone();
            if !api.is_some_and(|api| api.set_default_configured_node_name(&media_class, &name)) {
                return Err("WirePlumber could not change the default device.".into());
            }
            self.request_snapshot();
            Ok(())
        })
    }

    pub fn move_stream(&self, stream_id: i64, target_id: i64) -> Outcome {
        self.action(|| {
            self.checked_proxy(Some(stream_id), None)?;
            self.checked_proxy(Some(target_id), None)?;
            let (stream_class, target_class, serial) = {
                let state = self.0.state.borrow();
                let missing = || "This audio device or stream is no longer available.".to_owned();
                let stream = state.nodes.get(&stream_id).ok_or_else(missing)?;
                let target = state.nodes.get(&target_id).ok_or_else(missing)?;
                (stream.media_class.clone(), target.media_class.clone(), target.serial)
            };
            let allowed: &[&str] = match stream_class.as_str() {
                "Stream/Output/Audio" => &["Audio/Sink"],
                "Stream/Input/Audio" => &["Audio/Source", "Audio/Source/Virtual", "Audio/Sink"],
                _ => &[],
            };
            if !allowed.contains(&target_class.as_str()) {
                return Err("That device cannot be used for this audio stream.".into());
            }
            let metadata = self.0.metadata.borrow().clone();
            let Some(metadata) = metadata else {
                return Err("WirePlumber routing metadata is unavailable. Check that WirePlumber is running.".into());
            };
            metadata.metadata_set(stream_id, "target.object", "Spa:Id", &serial.to_string());
            self.request_snapshot();
            Ok(())
        })
    }

    pub fn set_profile(&self, device_id: i64, index: i64) -> Outcome {
        self.action(|| {
            let device = self.0.state.borrow().devices.get(&device_id).cloned();
            let proxy = self.checked_proxy(Some(device_id), device.as_ref().map(|d| d.serial))?;
            if !device.is_some_and(|d| d.profiles.iter().any(|c| c.index == index && c.available)) {
                return Err("This audio profile is unavailable.".into());
            }
            if !proxy.set_param("Profile", 0, &[("index", ParamValue::Int(index)), ("save", ParamValue::Bool(true))]) {
                return Err("PipeWire could not change the device profile.".into());
            }
            self.request_snapshot();
            Ok(())
        })
    }

    pub fn set_route(&self, node_id: i64, index: i64) -> Outcome {
        self.action(|| {
            let node_proxy = self.checked_proxy(Some(node_id), None)?;
            let node = self.0.state.borrow().nodes.get(&node_id).cloned().ok_or("This audio device or stream is no longer available.")?;
            let device_serial = node.device_id.and_then(|d| self.0.state.borrow().devices.get(&d).map(|d| d.serial));
            let device_proxy = self.checked_proxy(node.device_id, device_serial)?;
            let profile_device = int(node_proxy.property("card.profile.device"));
            let Some(profile_device) = profile_device.filter(|_| node.routes.iter().any(|c| c.index == index && c.available)) else {
                return Err("This audio port is unavailable for the current device profile.".into());
            };
            let values = [("index", ParamValue::Int(index)), ("device", ParamValue::Int(profile_device)), ("save", ParamValue::Bool(true))];
            if !device_proxy.set_param("Route", 0, &values) {
                return Err("PipeWire could not change the audio port.".into());
            }
            self.request_snapshot();
            Ok(())
        })
    }

    /// Start a peak meter for an input device; None when unsupported.
    pub fn level_monitor(&self, id: i64, callback: LevelCallback) -> Option<Box<dyn LevelMeter>> {
        let serial = {
            let state = self.0.state.borrow();
            let node = state.nodes.get(&id)?;
            if self.0.closed.get() || !state.ready || !matches!(node.media_class.as_str(), "Audio/Source" | "Audio/Source/Virtual") {
                return None;
            }
            node.serial
        };
        (self.0.meter.as_ref()?)(serial, callback).ok()
    }

    // ----- loopbacks --------------------------------------------------------

    pub(crate) fn reload_loopbacks(&self) {
        match self.0.loopback_store.load() {
            Err(error) => {
                self.0.state.borrow_mut().loopback_error = error.0.clone();
                self.fail(&error.0);
            }
            Ok(entries) => {
                {
                    let mut state = self.0.state.borrow_mut();
                    state.loopbacks = entries;
                    if state.error == state.loopback_error {
                        state.error.clear();
                    }
                    state.loopback_error.clear();
                }
                self.changed();
            }
        }
    }

    pub fn add_loopback(&self, name: &str, target_id: i64, target_serial: i64) -> Outcome {
        self.action(|| {
            self.checked_proxy(Some(target_id), None)?;
            let node = self.0.state.borrow().nodes.get(&target_id).cloned();
            let Some(node) = node.filter(|n| n.serial == target_serial) else {
                return Err("The selected output changed. Please select it again.".into());
            };
            if node.media_class != "Audio/Sink" || node.name.is_empty() || node.loopback_id.is_some() || node.loopback_internal || !target_available(Some(&node)) {
                return Err("Choose an available output device for this loopback.".into());
            }
            self.0.loopback_store.add(name, &node.name, &node.description).map_err(|e| e.0)?;
            self.reload_loopbacks();
            Ok(())
        })
    }

    pub fn remove_loopback(&self, identifier: &str) -> Outcome {
        self.action(|| {
            if self.0.closed.get() {
                return Err("Audio Settings is closing.".into());
            }
            self.0.loopback_store.remove(identifier).map_err(|e| e.0)?;
            self.reload_loopbacks();
            Ok(())
        })
    }
}

impl AudioApi for AudioBackend {
    fn state(&self) -> Ref<'_, AudioState> {
        self.0.state.borrow()
    }
    fn start(&self) {
        AudioBackend::start(self)
    }
    fn close(&self) {
        AudioBackend::close(self)
    }
    fn set_volume(&self, id: i64, volume: f64) -> Outcome {
        AudioBackend::set_volume(self, id, volume)
    }
    fn set_mute(&self, id: i64, muted: bool) -> Outcome {
        AudioBackend::set_mute(self, id, muted)
    }
    fn set_channel_volume(&self, id: i64, index: i64, volume: f64) -> Outcome {
        AudioBackend::set_channel_volume(self, id, index, volume)
    }
    fn set_default(&self, id: i64) -> Outcome {
        AudioBackend::set_default(self, id)
    }
    fn move_stream(&self, stream: i64, target: i64) -> Outcome {
        AudioBackend::move_stream(self, stream, target)
    }
    fn set_profile(&self, device: i64, index: i64) -> Outcome {
        AudioBackend::set_profile(self, device, index)
    }
    fn set_route(&self, node: i64, index: i64) -> Outcome {
        AudioBackend::set_route(self, node, index)
    }
    fn level_monitor(&self, id: i64, callback: LevelCallback) -> Option<Box<dyn LevelMeter>> {
        AudioBackend::level_monitor(self, id, callback)
    }
    fn add_loopback(&self, name: &str, target: i64, serial: i64) -> Outcome {
        AudioBackend::add_loopback(self, name, target, serial)
    }
    fn remove_loopback(&self, id: &str) -> Outcome {
        AudioBackend::remove_loopback(self, id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::graph::tests::fixture;
    use crate::model::Node;
    use serde_json::json;

    pub struct FakeProxy {
        pub id: i64,
        pub serial: Cell<i64>,
        pub params: RefCell<Vec<(String, u32, Vec<(String, ParamValue)>)>>,
        pub metadata: RefCell<Vec<(i64, String, String, String)>>,
    }

    impl FakeProxy {
        pub fn new(id: i64, serial: i64) -> Rc<Self> {
            Rc::new(Self { id, serial: Cell::new(serial), params: RefCell::default(), metadata: RefCell::default() })
        }
    }

    impl NativeProxy for FakeProxy {
        fn bound_id(&self) -> i64 { self.id }
        fn property(&self, key: &str) -> Option<String> {
            Some(if key == "object.serial" { self.serial.get().to_string() } else { "7".into() })
        }
        fn set_param(&self, id: &str, flags: u32, values: &[(&str, ParamValue)]) -> bool {
            self.params.borrow_mut().push((id.into(), flags, values.iter().map(|(k, v)| (k.to_string(), *v)).collect()));
            true
        }
        fn metadata_set(&self, subject: i64, key: &str, kind: &str, value: &str) {
            self.metadata.borrow_mut().push((subject, key.into(), kind.into(), value.into()));
        }
        fn key(&self) -> usize { self as *const Self as usize }
    }

    #[derive(Default)]
    pub struct FakeMixer {
        pub writes: RefCell<Vec<(i64, PendingValues)>>,
        pub volume: RefCell<Option<MixerVolume>>,
        pub reads: Cell<u32>,
    }

    impl NativeMixer for FakeMixer {
        fn get_volume(&self, _id: i64) -> Option<MixerVolume> {
            self.reads.set(self.reads.get() + 1);
            self.volume.borrow().clone()
        }
        fn set_volume(&self, id: i64, values: &PendingValues) -> bool {
            self.writes.borrow_mut().push((id, values.clone()));
            true
        }
    }

    #[derive(Default)]
    pub struct FakeDefaults(pub RefCell<Vec<(String, String)>>);

    impl NativeDefaults for FakeDefaults {
        fn set_default_configured_node_name(&self, class: &str, name: &str) -> bool {
            self.0.borrow_mut().push((class.into(), name.into()));
            true
        }
    }

    pub struct NoConnector;
    impl Connector for NoConnector {
        fn connect(&self, _events: EventSink) -> Result<Box<dyn Connection>, String> {
            Err("PipeWire is not available. Check that PipeWire and WirePlumber are running.".into())
        }
    }

    pub struct FakeConnection;
    impl Connection for FakeConnection {
        fn disconnect(&self) {}
    }

    pub fn options(store: LoopbackStore) -> BackendOptions {
        BackendOptions { monitor_loopbacks: false, loopback_store: store, connector: Rc::new(NoConnector), dump_path: None, meter: None }
    }

    pub struct Controls {
        pub backend: AudioBackend,
        pub errors: Rc<RefCell<Vec<String>>>,
        pub mixer: Rc<FakeMixer>,
        pub defaults: Rc<FakeDefaults>,
        pub metadata: Rc<FakeProxy>,
        pub proxies: IndexMap<i64, Rc<FakeProxy>>,
    }

    /// ControlTests.setUp
    pub fn controls() -> Controls {
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = errors.clone();
        let backend = AudioBackend::new(|| {}, move |m| sink.borrow_mut().push(m.into()), options(LoopbackStore::new(Some("/nonexistent/loopbacks.json".into()))));
        backend.0.record_later.set(true);
        let (nodes, devices, defaults) = parse_snapshot(&fixture());
        let mut proxies = IndexMap::new();
        for (id, serial) in nodes.values().map(|n| (n.id, n.serial)).chain(devices.values().map(|d| (d.id, d.serial))) {
            let proxy = FakeProxy::new(id, serial);
            backend.0.proxies.borrow_mut().insert(id, proxy.clone());
            proxies.insert(id, proxy);
        }
        {
            let mut state = backend.state_mut();
            state.ready = true;
            state.nodes = nodes;
            state.devices = devices;
            state.defaults = defaults;
        }
        let mixer = Rc::new(FakeMixer::default());
        let defaults = Rc::new(FakeDefaults::default());
        let metadata = FakeProxy::new(50, 500);
        *backend.0.mixer.borrow_mut() = Some(mixer.clone());
        *backend.0.default_api.borrow_mut() = Some(defaults.clone());
        *backend.0.metadata.borrow_mut() = Some(metadata.clone());
        Controls { backend, errors, mixer, defaults, metadata, proxies }
    }

    #[test]
    fn volume_coalesces_latest_value_and_preserves_mute() {
        let c = controls();
        assert_eq!(c.backend.set_volume(20, 0.2), Ok(true));
        assert_eq!(c.backend.set_volume(20, 0.6), Ok(true));
        assert_eq!(c.backend.set_mute(20, true), Ok(true));
        let writes = c.mixer.writes.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (20, PendingValues::from([("volume", PendingValue::Volume(0.6)), ("mute", PendingValue::Mute(true))])));
    }

    #[test]
    fn channel_changes_merge_without_touching_other_channels() {
        let c = controls();
        c.backend.set_channel_volume(20, 0, 0.4).unwrap();
        c.backend.set_channel_volume(20, 1, 0.8).unwrap();
        c.backend.flush_volumes();
        let writes = c.mixer.writes.borrow();
        assert_eq!(writes.last().unwrap().1, PendingValues::from([("channelVolumes", PendingValue::Channels(IndexMap::from([("0".into(), 0.4), ("1".into(), 0.8)])))]));
    }

    #[test]
    fn master_volume_drops_pending_channels() {
        let c = controls();
        c.backend.set_channel_volume(20, 0, 0.4).unwrap();
        c.backend.set_volume(20, 0.5).unwrap();
        c.backend.flush_volumes();
        assert_eq!(c.mixer.writes.borrow()[0].1, PendingValues::from([("volume", PendingValue::Volume(0.5))]));
    }

    #[test]
    fn invalid_volume_channel_and_unavailable_volume_are_rejected() {
        let c = controls();
        for value in [-1.0, 1.51, f64::NAN, f64::INFINITY] {
            assert_eq!(c.backend.set_volume(20, value), Ok(false));
        }
        assert_eq!(c.errors.borrow()[0], "Volume must be between 0% and 150%.");
        assert_eq!(c.backend.set_channel_volume(20, 7, 0.5), Ok(false));
        assert_eq!(c.errors.borrow().last().unwrap(), "The selected audio channel is no longer available.");
        assert_eq!(c.backend.set_volume(31, 0.5), Ok(false));
        assert_eq!(c.errors.borrow().last().unwrap(), "This audio stream does not expose a volume control.");
        c.backend.flush_volumes();
        assert!(c.mixer.writes.borrow().is_empty());
    }

    #[test]
    fn queued_volume_cannot_touch_replacement_node() {
        let c = controls();
        c.backend.set_volume(20, 0.3).unwrap();
        c.backend.state_mut().nodes[&20].serial = 999;
        c.proxies[&20].serial.set(999);
        c.backend.flush_volumes();
        assert!(c.mixer.writes.borrow().is_empty());
    }

    #[test]
    fn stale_snapshot_serial_is_rejected_before_write() {
        let c = controls();
        c.proxies[&20].serial.set(999);
        assert_eq!(c.backend.set_mute(20, true), Ok(false));
        assert!(c.mixer.writes.borrow().is_empty());
        assert!(c.errors.borrow().last().unwrap().contains("changed"));
    }

    #[test]
    fn default_uses_stable_node_name() {
        let c = controls();
        assert_eq!(c.backend.set_default(20), Ok(true));
        assert_eq!(*c.defaults.0.borrow(), [("Audio/Sink".to_owned(), "speakers".to_owned())]);
        assert_eq!(c.backend.set_default(30), Ok(false));
    }

    #[test]
    fn stream_move_uses_target_serial_and_allows_monitor_capture() {
        let c = controls();
        assert_eq!(c.backend.move_stream(30, 20), Ok(true));
        assert_eq!(c.metadata.metadata.borrow().last().unwrap(), &(30, "target.object".into(), "Spa:Id".into(), "200".into()));
        assert_eq!(c.backend.move_stream(31, 20), Ok(true));
        assert_eq!(c.metadata.metadata.borrow().last().unwrap(), &(31, "target.object".into(), "Spa:Id".into(), "200".into()));
        assert_eq!(c.backend.move_stream(30, 21), Ok(false));
    }

    #[test]
    fn routes_and_profiles_build_native_saved_parameters() {
        let c = controls();
        assert_eq!(c.backend.set_route(20, 5), Ok(true));
        let params = c.proxies[&10].params.borrow().last().unwrap().clone();
        assert_eq!(params, ("Route".into(), 0, vec![("index".into(), ParamValue::Int(5)), ("device".into(), ParamValue::Int(7)), ("save".into(), ParamValue::Bool(true))]));
        assert_eq!(c.backend.set_route(20, 6), Ok(false));
        assert_eq!(c.backend.set_profile(10, 3), Ok(false));
        assert_eq!(c.backend.set_profile(10, 2), Ok(true));
        assert_eq!(c.proxies[&10].params.borrow().last().unwrap().0, "Profile");
        assert_eq!(c.proxies[&10].params.borrow().last().unwrap().2, vec![("index".into(), ParamValue::Int(2)), ("save".into(), ParamValue::Bool(true))]);
    }

    #[test]
    fn connection_loss_discards_queued_actions() {
        let c = controls();
        c.backend.set_volume(20, 0.4).unwrap();
        c.backend.disconnect_core();
        assert!(!c.backend.ready());
        assert!(c.backend.state().nodes.is_empty());
        c.backend.flush_volumes();
        let mixer = Rc::new(FakeMixer::default());
        *c.backend.0.mixer.borrow_mut() = Some(mixer.clone());
        assert_eq!(c.backend.set_volume(20, 0.4), Ok(false));
        assert!(mixer.writes.borrow().is_empty() && c.mixer.writes.borrow().is_empty());
    }

    #[test]
    fn retired_registry_and_mixer_callbacks_cannot_touch_new_connection() {
        let c = controls();
        let current = c.backend.0.generation.get();
        let replacement_key = c.proxies[&20].key();
        c.backend.event(current.wrapping_sub(1), Event::ObjectAdded(FakeProxy::new(20, 1), false));
        c.backend.event(current.wrapping_sub(1), Event::ObjectRemoved(replacement_key));
        c.backend.event(current.wrapping_sub(1), Event::MixerChanged(20));
        assert_eq!(c.backend.0.proxies.borrow()[&20].key(), replacement_key);
        assert!(c.backend.state().nodes.contains_key(&20));
        assert_eq!(c.mixer.reads.get(), 0);
        assert!(c.backend.0.later_log.borrow().is_empty());
    }

    #[test]
    fn disconnect_cancels_connection_timers_and_snapshot_work() {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let c = controls();
            c.backend.0.record_later.set(false);
            c.backend.0.echo_dirty.set(true);
            c.backend.0.graph.borrow_mut().insert(1, json!({}));
            let fired = Rc::new(Cell::new(0));
            for name in ["startup", "snapshot", "snapshot_timeout", "graph", "echo", "volume"] {
                let fired = fired.clone();
                c.backend.later(name, 1, move |_| fired.set(fired.get() + 1));
            }
            let process = gio::Subprocess::newv(&["sleep".as_ref(), "30".as_ref()], gio::SubprocessFlags::NONE).unwrap();
            *c.backend.0.monitor_process.borrow_mut() = Some(process.clone());
            let cancel = gio::Cancellable::new();
            *c.backend.0.monitor_cancel.borrow_mut() = Some(cancel.clone());
            c.backend.disconnect_core();
            // Only the "notify" of the disconnect itself remains scheduled.
            assert_eq!(c.backend.0.sources.borrow().keys().copied().collect::<Vec<_>>(), ["notify"]);
            assert!(!c.backend.0.echo_dirty.get());
            assert!(c.backend.0.graph.borrow().is_empty());
            assert!(c.backend.0.monitor_process.borrow().is_none());
            assert!(cancel.is_cancelled());
            process.wait(None::<&gio::Cancellable>).unwrap();
            assert!(!process.is_successful());
            timer::settle(&context, std::time::Duration::from_millis(20));
            assert_eq!(fired.get(), 0);
        }).unwrap();
    }

    #[test]
    fn volume_echo_updates_model_without_refreshing_views() {
        let c = controls();
        c.backend.0.plugins.borrow_mut().extend(["mixer-api", "default-nodes-api"]);
        *c.backend.0.connection.borrow_mut() = Some(Box::new(FakeConnection));
        c.backend.0.snapshot_received.set(true);
        *c.backend.0.graph.borrow_mut() = fixture().into_iter().map(|o| (o["id"].as_i64().unwrap(), o)).collect();
        assert_eq!(c.backend.set_volume(20, 0.3), Ok(true));
        c.backend.flush_volumes();
        assert!(c.backend.0.echo.borrow().contains_key(&20));
        *c.mixer.volume.borrow_mut() = Some(MixerVolume { volume: Some(0.3), ..Default::default() });
        c.backend.0.later_log.borrow_mut().clear();
        c.backend.event(c.backend.0.generation.get(), Event::MixerChanged(20));
        let echo = json!([{"id": 20, "type": "PipeWire:Interface:Node", "info": {
            "change-mask": ["params"], "params": {"Props": [{"channelVolumes": [0.027, 0.027]}]}}}]);
        c.backend.apply_batch(&echo);
        let later = c.backend.0.later_log.borrow().clone();
        assert!(!later.contains(&"notify") && !later.contains(&"graph") && later.contains(&"echo"));
        assert!((c.backend.state().nodes[&20].volume - 0.3).abs() < 1e-9);
        assert!(c.backend.0.echo_dirty.get());
        // Other changes, and the same node after the echo window, refresh.
        c.backend.0.echo.borrow_mut().insert(20, 0.0);
        c.backend.apply_batch(&echo);
        assert!(c.backend.0.later_log.borrow().contains(&"graph"));
        c.backend.0.later_log.borrow_mut().clear();
        c.backend.echo_settled();
        assert!(!c.backend.0.echo_dirty.get());
        assert_eq!(c.backend.0.later_log.borrow().last(), Some(&"notify"));
    }

    #[test]
    fn mixer_volume_is_read_in_channel_order() {
        let c = controls();
        c.backend.0.plugins.borrow_mut().insert("mixer-api");
        *c.mixer.volume.borrow_mut() = Some(MixerVolume { volume: Some(0.9), mute: Some(true), channels: vec![
            ("10".into(), Some(0.2), Some("RL".into())), ("1".into(), Some(0.7), None), ("0".into(), Some(0.4), Some("FL".into()))] });
        c.backend.read_volume(20);
        let node = c.backend.state().nodes[&20].clone();
        assert_eq!(node.channels, ["FL", "Channel 2", "RL"]);
        assert_eq!(node.channel_volumes, [0.4, 0.7, 0.2]);
        assert_eq!((node.volume, node.mute, node.volume_available), (0.7, true, true));
        *c.mixer.volume.borrow_mut() = None;
        c.backend.read_volume(20);
        assert!(!c.backend.state().nodes[&20].volume_available);
    }

    #[test]
    fn level_monitor_only_for_inputs() {
        let calls: Rc<RefCell<Vec<i64>>> = Rc::default();
        let sink = calls.clone();
        struct Meter;
        impl LevelMeter for Meter { fn stop(&self) {} }
        let mut opts = options(LoopbackStore::new(Some("/nonexistent/x.json".into())));
        opts.meter = Some(Rc::new(move |serial, _| { sink.borrow_mut().push(serial); Ok(Box::new(Meter) as Box<dyn LevelMeter>) }));
        let backend = AudioBackend::new(|| {}, |_| {}, opts);
        let (nodes, devices, defaults) = parse_snapshot(&fixture());
        {
            let mut s = backend.state_mut();
            s.nodes = nodes; s.devices = devices; s.defaults = defaults; s.ready = true;
        }
        assert!(backend.level_monitor(20, Box::new(|_| {})).is_none());
        assert!(backend.level_monitor(21, Box::new(|_| {})).is_some());
        assert_eq!(*calls.borrow(), [210]);
        backend.close();
    }

    #[test]
    fn startup_failure_reports_and_schedules_reconnect() {
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = errors.clone();
        let mut opts = options(LoopbackStore::new(Some("/nonexistent/x.json".into())));
        opts.dump_path = Some("/usr/bin/true".into());
        let backend = AudioBackend::new(|| {}, move |m| sink.borrow_mut().push(m.into()), opts);
        backend.0.record_later.set(true);
        backend.start();
        assert_eq!(errors.borrow().as_slice(), ["PipeWire is not available. Check that PipeWire and WirePlumber are running."]);
        assert!(backend.0.later_log.borrow().contains(&"reconnect"));
        let missing = AudioBackend::new(|| {}, |_| {}, options(LoopbackStore::new(Some("/nonexistent/x.json".into()))));
        missing.0.record_later.set(true);
        missing.start();
        assert_eq!(missing.error(), "The PipeWire tool pw-dump is missing. Install pipewire-utils and reopen Audio Settings.");
    }

    // ----- BackendLoopbackTests ------------------------------------------

    fn loopback_backend(tag: &str) -> (PathBuf, AudioBackend, Rc<RefCell<Vec<String>>>) {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("am-backend-{tag}-{}-{}", std::process::id(), crate::store::uuid4()));
        std::fs::create_dir_all(&dir).unwrap();
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = errors.clone();
        let backend = AudioBackend::new(|| {}, move |m| sink.borrow_mut().push(m.into()), options(LoopbackStore::new(Some(dir.join("loopbacks.json")))));
        backend.0.record_later.set(true);
        backend.0.proxies.borrow_mut().insert(42, FakeProxy::new(42, 4242));
        {
            let mut state = backend.state_mut();
            state.nodes.insert(42, Node::new(42, 4242, "bluez_output.headset", "Bluetooth Headset", "Audio/Sink"));
            state.ready = true;
        }
        (dir, backend, errors)
    }

    #[test]
    fn add_persists_stable_name_and_remove_works_with_audio_offline() {
        let (dir, backend, _) = loopback_backend("add");
        assert_eq!(backend.add_loopback("Headset", 42, 4242), Ok(true));
        let saved = backend.state().loopbacks.values().next().unwrap().clone();
        assert_eq!(saved.target_name, "bluez_output.headset");
        assert_eq!(backend.store().load().unwrap(), backend.state().loopbacks);
        {
            let mut state = backend.state_mut();
            state.ready = false;
            state.nodes.clear();
        }
        assert_eq!(backend.remove_loopback(&saved.id), Ok(true));
        assert!(backend.store().load().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_unavailable_or_managed_target_is_rejected() {
        let (dir, backend, errors) = loopback_backend("stale");
        assert_eq!(backend.add_loopback("Headset", 42, 1111), Ok(false));
        assert_eq!(errors.borrow().last().unwrap(), "The selected output changed. Please select it again.");
        backend.state_mut().nodes[&42].state = "error".into();
        assert_eq!(backend.add_loopback("Headset", 42, 4242), Ok(false));
        assert_eq!(errors.borrow().last().unwrap(), "Choose an available output device for this loopback.");
        backend.state_mut().nodes[&42].state = "suspended".into();
        backend.state_mut().nodes[&42].loopback_id = Some("532aa4b1-bd34-4d0c-8a4e-34580ebf1228".into());
        assert_eq!(backend.add_loopback("Headset", 42, 4242), Ok(false));
        assert!(!backend.store().path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_external_config_keeps_last_valid_view_then_recovers() {
        let (dir, backend, _) = loopback_backend("invalid");
        backend.add_loopback("Headset", 42, 4242).unwrap();
        let saved = backend.state().loopbacks.clone();
        let good = std::fs::read(&backend.store().path).unwrap();
        std::fs::write(&backend.store().path, "{").unwrap();
        backend.reload_loopbacks();
        assert_eq!(backend.state().loopbacks, saved);
        assert!(!backend.state().loopback_error.is_empty());
        assert_eq!(backend.error(), backend.state().loopback_error);
        std::fs::write(&backend.store().path, good).unwrap();
        backend.reload_loopbacks();
        assert!(backend.state().loopback_error.is_empty());
        assert!(backend.error().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The recorded real pw-dump stream gives exactly the recorded reference models.
    #[test]
    fn recorded_pw_dump_stream_matches_reference() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let stream = std::fs::read(dir.join("monitor.json")).unwrap();
        let reference: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("reference.json")).unwrap()).unwrap();
        let (batches, rest) = split_batches(stream);
        assert!(rest.is_empty());
        let mut graph = Graph::new();
        let first: serde_json::Value = serde_json::from_slice(&batches[0]).unwrap();
        crate::testing::assert_model(&parse_snapshot(first.as_array().unwrap()), &reference["snapshot"]);
        for (batch, expected) in batches.iter().zip(reference["monitor"].as_array().unwrap()) {
            let (changed, props_only) = merge_update(&mut graph, &serde_json::from_slice(batch).unwrap());
            let mut changed: Vec<i64> = changed.into_iter().collect();
            changed.sort();
            let mut props_only: Vec<i64> = props_only.into_iter().collect();
            props_only.sort();
            assert_eq!(json!(changed), expected["changed"]);
            assert_eq!(json!(props_only), expected["props_only"]);
            crate::testing::assert_model(&parse_snapshot(graph.values()), &expected["model"]);
        }
    }

    /// Feeding the stream in odd-sized chunks through the backend's buffer gives the same graph.
    #[test]
    fn chunked_monitor_feed_applies_complete_batches_only() {
        let c = controls();
        *c.backend.0.connection.borrow_mut() = Some(Box::new(FakeConnection));
        c.backend.0.plugins.borrow_mut().extend(["mixer-api", "default-nodes-api"]);
        c.backend.0.snapshot_received.set(false);
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let stream = std::fs::read(dir.join("monitor.json")).unwrap();
        for chunk in stream.chunks(4093) {
            c.backend.feed(chunk).unwrap();
        }
        assert!(c.backend.0.monitor_buffer.borrow().is_empty());
        let reference: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("reference.json")).unwrap()).unwrap();
        let expected = reference["monitor"].as_array().unwrap().last().unwrap()["model"].clone();
        crate::testing::assert_model(&parse_snapshot(c.backend.0.graph.borrow().values()), &expected);
        assert!(c.backend.ready());
    }
}
