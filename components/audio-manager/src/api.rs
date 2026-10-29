//! What the views and volume keys need from an audio backend: the native
//! backend (`backend::AudioBackend`) and the in-memory `fake::FakeAudioBackend`.

use crate::graph::{Defaults, Devices, Nodes};
use crate::model::Node;
use crate::store::Entries;
use std::cell::Ref;

#[derive(Default, Debug, Clone)]
pub struct AudioState {
    pub nodes: Nodes,
    pub devices: Devices,
    pub defaults: Defaults,
    pub loopbacks: Entries,
    pub loopback_error: String,
    pub ready: bool,
    pub error: String,
}

impl AudioState {
    pub fn default_id(&self, media_class: &str) -> Option<i64> {
        self.defaults.get(media_class).copied()
    }
    /// The default output if it is a sink (`_default_output`).
    pub fn default_output(&self) -> Option<&Node> {
        self.default_id("Audio/Sink").and_then(|id| self.nodes.get(&id)).filter(|n| n.media_class == "Audio/Sink")
    }
}

/// A running input peak meter.
pub trait LevelMeter {
    fn stop(&self);
}

/// Callback receiving 0..1 levels, or None on capture errors.
pub type LevelCallback = Box<dyn Fn(Option<f64>)>;

/// Errors: `Ok(false)` = the backend reported a failure through `on_error`
/// (Python returned False); `Err(text)` = Python raised an exception.
pub type Outcome = Result<bool, String>;

pub trait AudioApi {
    fn state(&self) -> Ref<'_, AudioState>;
    fn start(&self);
    fn close(&self);
    fn set_volume(&self, id: i64, volume: f64) -> Outcome;
    fn set_mute(&self, id: i64, muted: bool) -> Outcome;
    fn set_channel_volume(&self, id: i64, index: i64, volume: f64) -> Outcome;
    fn set_default(&self, id: i64) -> Outcome;
    fn move_stream(&self, stream: i64, target: i64) -> Outcome;
    fn set_profile(&self, device: i64, index: i64) -> Outcome;
    fn set_route(&self, node: i64, index: i64) -> Outcome;
    fn level_monitor(&self, id: i64, callback: LevelCallback) -> Option<Box<dyn LevelMeter>>;
    fn add_loopback(&self, name: &str, target: i64, serial: i64) -> Outcome;
    fn remove_loopback(&self, id: &str) -> Outcome;
    /// The UI only meters when the backend can (`hasattr(backend, "level_monitor")`).
    fn has_level_monitor(&self) -> bool {
        true
    }
}

/// UI actions dispatched by name (`perform("set_default", id)`).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    SetVolume(i64, f64),
    SetMute(i64, bool),
    SetChannelVolume(i64, i64, f64),
    SetDefault(i64),
    MoveStream(i64, i64),
    SetProfile(i64, i64),
    SetRoute(i64, i64),
    RemoveLoopback(String),
}

impl Action {
    pub fn run(&self, api: &dyn AudioApi) -> Outcome {
        match self {
            Action::SetVolume(id, v) => api.set_volume(*id, *v),
            Action::SetMute(id, m) => api.set_mute(*id, *m),
            Action::SetChannelVolume(id, i, v) => api.set_channel_volume(*id, *i, *v),
            Action::SetDefault(id) => api.set_default(*id),
            Action::MoveStream(s, t) => api.move_stream(*s, *t),
            Action::SetProfile(d, i) => api.set_profile(*d, *i),
            Action::SetRoute(n, i) => api.set_route(*n, *i),
            Action::RemoveLoopback(id) => api.remove_loopback(id),
        }
    }
}
