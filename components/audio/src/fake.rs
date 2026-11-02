//! In-memory audio graph for UI tests and previews; never opens PipeWire
//! (fake_audio.py, plus the asynchronous key-test backend of test_volume_app.py).

use crate::api::{AudioApi, AudioState, LevelCallback, LevelMeter, Outcome};
use crate::model::{target_available, Choice, Device, Loopback, Node};
use std::cell::{Cell, Ref, RefCell};
use std::rc::{Rc, Weak};

pub const SPEAKERS: i64 = 10;
pub const HEADPHONES: i64 = 11;
pub const MICROPHONE: i64 = 20;
pub const USB_MICROPHONE: i64 = 21;
pub const BROWSER: i64 = 30;
pub const MUSIC: i64 = 31;
pub const RECORDER: i64 = 40;
pub const BUILTIN: i64 = 1;
pub const USB: i64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeMode {
    /// fake_audio.FakeAudioBackend(state="populated"): synchronous writes.
    Populated,
    /// state="empty"
    Empty,
    /// state="unavailable": start() reports an error and never becomes ready.
    Unavailable,
    /// test_volume_app.FakeBackend: one sink, ready after 350 ms, writes acknowledged after 45 ms.
    Keys,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Volume(i64, f64),
    Mute(i64, bool),
    Default(i64),
    Target(i64, i64),
    Profile(i64, i64),
    Route(i64, i64),
    Channel(i64, i64, f64),
    AddLoopback(String, i64, i64),
    RemoveLoopback(String),
}

pub struct FakeLevelMonitor {
    pub identifier: i64,
    pub active: Rc<Cell<bool>>,
    callback: LevelCallback,
}

struct MeterHandle(Rc<Cell<bool>>);

impl LevelMeter for MeterHandle {
    fn stop(&self) {
        self.0.set(false);
    }
}

pub struct FakeAudioBackend {
    weak: RefCell<Weak<FakeAudioBackend>>,
    pub state: RefCell<AudioState>,
    on_changed: RefCell<Option<Rc<dyn Fn()>>>,
    on_error: RefCell<Option<Rc<dyn Fn(&str)>>>,
    pub history: RefCell<Vec<Call>>,
    pub level_monitors: RefCell<Vec<FakeLevelMonitor>>,
    pub started: Cell<bool>,
    pub closed: Cell<bool>,
    pub mode: FakeMode,
    sources: RefCell<Vec<glib::SourceId>>,
}

fn node(id: i64, description: &str, media_class: &str, volume: f64) -> Node {
    let mut node = Node::new(id, 1000 + id, &format!("test.audio.{id}"), description, media_class);
    node.volume = volume;
    node.channels = vec!["FL".into(), "FR".into()];
    node.channel_volumes = vec![volume, volume];
    node.state = "running".into();
    node
}

impl FakeAudioBackend {
    pub fn new(mode: FakeMode) -> Rc<Self> {
        let backend = Rc::new(Self {
            weak: RefCell::new(Weak::new()), state: RefCell::default(), on_changed: RefCell::new(None), on_error: RefCell::new(None),
            history: RefCell::default(), level_monitors: RefCell::default(), started: Cell::new(false), closed: Cell::new(false),
            mode, sources: RefCell::default(),
        });
        *backend.weak.borrow_mut() = Rc::downgrade(&backend);
        match mode {
            FakeMode::Populated => backend.populate(),
            FakeMode::Keys => {
                let mut speakers = Node::new(20, 200, "synthetic.speakers", "Synthetic speakers", "Audio/Sink");
                speakers.volume = 0.5;
                let mut state = backend.state.borrow_mut();
                state.nodes.insert(20, speakers);
                state.defaults.insert("Audio/Sink".into(), 20);
            }
            _ => {}
        }
        backend
    }

    pub fn set_callbacks(&self, on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) {
        *self.on_changed.borrow_mut() = Some(Rc::new(on_changed));
        *self.on_error.borrow_mut() = Some(Rc::new(on_error));
    }

    fn populate(&self) {
        let builtin_routes = vec![Choice::new(0, "Speakers", true), Choice::new(1, "Headphones", true)];
        let profiles = vec![Choice::new(1, "Analog Stereo Duplex", true), Choice::new(2, "Digital Stereo Output", true), Choice::new(0, "Off", true)];
        let mut state = self.state.borrow_mut();
        state.devices.insert(BUILTIN, Device { id: BUILTIN, serial: 1001, name: "alsa_card.builtin".into(), description: "Built-in Audio".into(), profiles: profiles.clone(), profile_index: Some(1) });
        state.devices.insert(USB, Device { id: USB, serial: 1002, name: "alsa_card.usb_headset".into(), description: "USB Headset".into(), profiles: vec![profiles[0].clone(), profiles[2].clone()], profile_index: Some(1) });
        let mut speakers = node(SPEAKERS, "Built-in Speakers", "Audio/Sink", 0.62);
        speakers.device_id = Some(BUILTIN);
        speakers.route_index = Some(0);
        speakers.routes = builtin_routes;
        let mut headphones = node(HEADPHONES, "USB Headphones", "Audio/Sink", 0.45);
        headphones.device_id = Some(USB);
        headphones.route_index = Some(0);
        headphones.routes = vec![Choice::new(0, "Headphones", true)];
        let mut microphone = node(MICROPHONE, "Built-in Microphone", "Audio/Source", 0.7);
        microphone.device_id = Some(BUILTIN);
        microphone.route_index = Some(2);
        microphone.routes = vec![Choice::new(2, "Internal Microphone", true)];
        let mut usb_microphone = node(USB_MICROPHONE, "USB Microphone", "Audio/Source", 0.5);
        usb_microphone.device_id = Some(USB);
        usb_microphone.route_index = Some(1);
        usb_microphone.routes = vec![Choice::new(1, "Headset Microphone", true)];
        let stream = |id, description: &str, volume, application: &str, media_name: &str, icon: &str, target| {
            let mut n = node(id, description, if id == RECORDER { "Stream/Input/Audio" } else { "Stream/Output/Audio" }, volume);
            n.application = application.into();
            n.media_name = media_name.into();
            n.icon = icon.into();
            n.target_id = Some(target);
            n
        };
        for n in [speakers, headphones, microphone, usb_microphone,
                  stream(BROWSER, "Firefox", 0.8, "Firefox", "Video playback", "firefox", SPEAKERS),
                  stream(MUSIC, "Music", 0.55, "Music", "Evening playlist", "multimedia-player-symbolic", HEADPHONES),
                  stream(RECORDER, "Voice Recorder", 0.9, "Voice Recorder", "Recording", "audio-input-microphone-symbolic", MICROPHONE)] {
            state.nodes.insert(n.id, n);
        }
        state.defaults.insert("Audio/Sink".into(), SPEAKERS);
        state.defaults.insert("Audio/Source".into(), MICROPHONE);
    }

    pub fn changed(&self) {
        let callback = self.on_changed.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// `backend.on_error(message)` (tests: simulate a native failure report).
    pub fn report_error(&self, message: &str) {
        let callback = self.on_error.borrow().clone();
        if let Some(callback) = callback {
            callback(message);
        }
    }

    fn later(&self, delay: u64, callback: impl FnOnce(&FakeAudioBackend) + 'static) {
        let weak = self.weak.borrow().clone();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            if let Some(backend) = weak.upgrade() {
                callback(&backend);
            }
        });
        self.sources.borrow_mut().push(id);
    }

    /// Simulate an external control's change without recording a command.
    pub fn update(&self, id: i64, edit: impl FnOnce(&mut Node)) {
        if let Some(node) = self.state.borrow_mut().nodes.get_mut(&id) {
            edit(node);
        }
        self.changed();
    }

    pub fn remove(&self, id: i64) {
        {
            let mut state = self.state.borrow_mut();
            state.nodes.shift_remove(&id);
            state.defaults.retain(|_, default| *default != id);
        }
        self.changed();
    }

    pub fn active_meters(&self) -> Vec<i64> {
        let mut active: Vec<i64> = self.level_monitors.borrow().iter().filter(|m| m.active.get()).map(|m| m.identifier).collect();
        active.sort();
        active
    }

    /// Push a level to every active meter (tests: `monitor.emit(value)`).
    pub fn emit_level(&self, value: Option<f64>) {
        for monitor in self.level_monitors.borrow().iter().filter(|m| m.active.get()) {
            (monitor.callback)(value);
        }
    }

    fn record(&self, call: Call) {
        self.history.borrow_mut().push(call);
    }
}

impl AudioApi for FakeAudioBackend {
    fn state(&self) -> Ref<'_, AudioState> {
        self.state.borrow()
    }

    fn start(&self) {
        self.started.set(true);
        match self.mode {
            FakeMode::Unavailable => {
                self.state.borrow_mut().error = "PipeWire is unavailable".into();
                let callback = self.on_error.borrow().clone();
                if let Some(callback) = callback {
                    callback("PipeWire is unavailable");
                }
            }
            // The first key and forwarded repeat commands must survive startup.
            FakeMode::Keys => self.later(350, |b| {
                b.state.borrow_mut().ready = true;
                b.changed();
            }),
            _ => self.state.borrow_mut().ready = true,
        }
        self.changed();
    }

    fn close(&self) {
        self.closed.set(true);
        for id in self.sources.borrow_mut().drain(..) {
            // Already-fired one-shot sources are gone; removing them would warn.
            if glib::MainContext::default().find_source_by_id(&id).is_some() {
                id.remove();
            }
        }
        if self.mode == FakeMode::Keys {
            self.state.borrow_mut().ready = false;
        }
    }

    fn set_volume(&self, id: i64, volume: f64) -> Outcome {
        self.record(Call::Volume(id, volume));
        if self.mode == FakeMode::Keys {
            self.later(45, move |b| {
                if let Some(n) = b.state.borrow_mut().nodes.get_mut(&id) {
                    n.volume = volume;
                }
                b.changed();
            });
            return Ok(true);
        }
        {
            let mut state = self.state.borrow_mut();
            let node = state.nodes.get_mut(&id).ok_or("KeyError")?;
            node.volume = volume;
            node.channel_volumes = vec![volume; node.channels.len()];
        }
        self.changed();
        Ok(true)
    }

    fn set_mute(&self, id: i64, muted: bool) -> Outcome {
        self.record(Call::Mute(id, muted));
        if self.mode == FakeMode::Keys {
            self.later(45, move |b| {
                if let Some(n) = b.state.borrow_mut().nodes.get_mut(&id) {
                    n.mute = muted;
                }
                b.changed();
            });
            return Ok(true);
        }
        self.state.borrow_mut().nodes.get_mut(&id).ok_or("KeyError")?.mute = muted;
        self.changed();
        Ok(true)
    }

    fn set_channel_volume(&self, id: i64, index: i64, volume: f64) -> Outcome {
        self.record(Call::Channel(id, index, volume));
        {
            let mut state = self.state.borrow_mut();
            let node = state.nodes.get_mut(&id).ok_or("KeyError")?;
            if let Some(slot) = node.channel_volumes.get_mut(index as usize) {
                *slot = volume;
            }
            node.volume = node.channel_volumes.iter().copied().reduce(f64::max).unwrap_or(0.0);
        }
        self.changed();
        Ok(true)
    }

    fn set_default(&self, id: i64) -> Outcome {
        self.record(Call::Default(id));
        {
            let mut state = self.state.borrow_mut();
            let class = state.nodes.get(&id).ok_or("KeyError")?.media_class.clone();
            let class = if class == "Audio/Source/Virtual" { "Audio/Source".to_owned() } else { class };
            state.defaults.insert(class, id);
        }
        self.changed();
        Ok(true)
    }

    fn move_stream(&self, id: i64, target: i64) -> Outcome {
        self.record(Call::Target(id, target));
        self.state.borrow_mut().nodes.get_mut(&id).ok_or("KeyError")?.target_id = Some(target);
        self.changed();
        Ok(true)
    }

    fn set_profile(&self, id: i64, index: i64) -> Outcome {
        self.record(Call::Profile(id, index));
        self.state.borrow_mut().devices.get_mut(&id).ok_or("KeyError")?.profile_index = Some(index);
        self.changed();
        Ok(true)
    }

    fn set_route(&self, id: i64, index: i64) -> Outcome {
        self.record(Call::Route(id, index));
        self.state.borrow_mut().nodes.get_mut(&id).ok_or("KeyError")?.route_index = Some(index);
        self.changed();
        Ok(true)
    }

    /// Record meter lifetimes; tests push levels through `emit_level`.
    fn level_monitor(&self, id: i64, callback: LevelCallback) -> Option<Box<dyn LevelMeter>> {
        let state = self.state.borrow();
        let node = state.nodes.get(&id)?;
        if !state.ready || !matches!(node.media_class.as_str(), "Audio/Source" | "Audio/Source/Virtual") {
            return None;
        }
        let active = Rc::new(Cell::new(true));
        self.level_monitors.borrow_mut().push(FakeLevelMonitor { identifier: id, active: active.clone(), callback });
        Some(Box::new(MeterHandle(active)))
    }

    fn add_loopback(&self, name: &str, id: i64, serial: i64) -> Outcome {
        let target = {
            let state = self.state.borrow();
            let node = state.nodes.get(&id).cloned();
            if !state.ready || node.as_ref().is_none_or(|n| n.serial != serial) || !target_available(node.as_ref()) {
                return Err("The selected output is no longer available.".into());
            }
            node.unwrap()
        };
        self.record(Call::AddLoopback(name.into(), id, serial));
        let identifier = format!("test-loopback-{}", self.history.borrow().len());
        self.state.borrow_mut().loopbacks.insert(identifier.clone(), Loopback::new(&identifier, name, &target.name, &target.description));
        self.changed();
        Ok(true)
    }

    fn remove_loopback(&self, id: &str) -> Outcome {
        self.record(Call::RemoveLoopback(id.into()));
        self.state.borrow_mut().loopbacks.shift_remove(id);
        self.changed();
        Ok(true)
    }
}
