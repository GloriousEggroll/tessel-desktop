//! Queue volume keys and report only the output state confirmed by the
//! backend (volume_keys.py). Failure details are English, untranslated.

use crate::api::AudioApi;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const STEP: f64 = 0.05;
pub const MAXIMUM: f64 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Up,
    Down,
    Mute,
}

impl KeyAction {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "mute" => Ok(Self::Mute),
            _ => Err("Unknown volume action".into()),
        }
    }
}

/// One OSD update: `show("volume", value, muted=…, detail=…)`; `muted` is
/// None where Python omitted the keyword (failures).
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub value: Option<f64>,
    pub muted: Option<bool>,
    pub detail: String,
}

use suite_core::scheduler::Scheduler;

#[derive(Clone, Debug)]
struct Target {
    serial: u64,
    identity: (i64, i64),
    volume: f64,
    muted: bool,
    observed: (f64, bool),
}

/// `round(value, 6)` exactly as Python (correctly rounded, half-even).
pub fn round6(value: f64) -> f64 {
    format!("{value:.6}").parse().unwrap_or(value)
}

pub struct Inner {
    weak: RefCell<Weak<Inner>>,
    backend: Rc<dyn AudioApi>,
    show: Box<dyn Fn(Shown)>,
    scheduler: Rc<dyn Scheduler>,
    clock: Box<dyn Fn() -> f64>,
    queued: RefCell<Vec<KeyAction>>,
    target: RefCell<Option<Target>>,
    targets: Cell<u64>,
    timer: Cell<u64>,
    deadline: Cell<f64>,
    sending: Cell<bool>,
    closed: Cell<bool>,
}

#[derive(Clone)]
pub struct VolumeKeys(Rc<Inner>);

struct Output {
    id: i64,
    serial: i64,
    volume: f64,
    mute: bool,
    available: bool,
    description: String,
}

impl VolumeKeys {
    pub fn new(backend: Rc<dyn AudioApi>, show: impl Fn(Shown) + 'static, scheduler: Rc<dyn Scheduler>, clock: impl Fn() -> f64 + 'static) -> Self {
        let inner = Rc::new(Inner {
            weak: RefCell::new(Weak::new()), backend, show: Box::new(show), scheduler, clock: Box::new(clock),
            queued: RefCell::default(), target: RefCell::new(None), targets: Cell::new(0), timer: Cell::new(0),
            deadline: Cell::new(0.0), sending: Cell::new(false), closed: Cell::new(false),
        });
        *inner.weak.borrow_mut() = Rc::downgrade(&inner);
        Self(inner)
    }

    pub fn pending(&self) -> bool {
        !self.0.queued.borrow().is_empty() || self.0.target.borrow().is_some()
    }

    fn ready(&self) -> bool {
        self.0.backend.state().ready
    }

    fn backend_error(&self) -> String {
        self.0.backend.state().error.clone()
    }

    fn output(&self) -> Option<Output> {
        let state = self.0.backend.state();
        state.default_output().map(|n| Output {
            id: n.id, serial: n.serial, volume: n.volume, mute: n.mute, available: n.volume_available, description: n.description.clone(),
        })
    }

    fn target_serial(&self) -> Option<u64> {
        self.0.target.borrow().as_ref().map(|t| t.serial)
    }

    pub fn request(&self, action: KeyAction) {
        if self.0.closed.get() {
            return;
        }
        self.0.queued.borrow_mut().push(action);
        if !self.ready() {
            if self.0.deadline.get() == 0.0 {
                self.0.deadline.set((self.0.clock)() + 9.0);
            }
            self.watch();
            let error = self.backend_error();
            if !error.is_empty() {
                self.error(&error);
            }
            return;
        }
        // Keep one native write in flight. Comparing a reversed pending target
        // with the old cached state would mistake it for an acknowledgement.
        if self.0.target.borrow().is_some() {
            return;
        }
        let actions = std::mem::take(&mut *self.0.queued.borrow_mut());
        self.apply(actions);
    }

    fn apply(&self, actions: Vec<KeyAction>) {
        let Some(node) = self.output().filter(|n| n.available) else {
            let message = if self.output().is_none() { "No output device available" } else { "This output has no volume control" };
            self.fail(message);
            return;
        };
        let (mut volume, mut muted) = (node.volume, node.mute);
        for action in actions {
            match action {
                KeyAction::Mute => muted = !muted,
                KeyAction::Up => volume = round6(volume + STEP).clamp(0.0, MAXIMUM),
                KeyAction::Down => volume = round6(volume - STEP).clamp(0.0, MAXIMUM),
            }
        }
        let serial = self.0.targets.get() + 1;
        self.0.targets.set(serial);
        *self.0.target.borrow_mut() = Some(Target { serial, identity: (node.id, node.serial), volume, muted, observed: (node.volume, node.mute) });
        self.0.deadline.set((self.0.clock)() + 2.0);
        self.watch();
        self.0.sending.set(true);
        let result = (|| -> Result<(), String> {
            if (volume - node.volume).abs() > 0.0001 {
                if self.0.backend.set_volume(node.id, volume)? == false {
                    if self.target_serial() == Some(serial) {
                        let error = self.backend_error();
                        self.fail(if error.is_empty() { "Could not change output volume" } else { &error });
                    }
                    return Err(String::new());
                }
            }
            if self.target_serial() == Some(serial) && muted != node.mute && self.0.backend.set_mute(node.id, muted)? == false {
                if self.target_serial() == Some(serial) {
                    let error = self.backend_error();
                    self.fail(if error.is_empty() { "Could not change output mute state" } else { &error });
                }
                return Err(String::new());
            }
            Ok(())
        })();
        self.0.sending.set(false);
        match result {
            Ok(()) => self.changed(),
            Err(message) if !message.is_empty() => self.fail(&message),
            Err(_) => {}
        }
    }

    pub fn changed(&self) {
        if self.0.closed.get() || self.0.sending.get() || !self.pending() {
            return;
        }
        if !self.ready() {
            if self.0.target.borrow().is_some() {
                let error = self.backend_error();
                self.fail(if error.is_empty() { "Audio output disconnected" } else { &error });
            }
            return;
        }
        if self.0.target.borrow().is_none() && !self.0.queued.borrow().is_empty() {
            let queued = std::mem::take(&mut *self.0.queued.borrow_mut());
            self.apply(queued);
            return;
        }
        let Some(target) = self.0.target.borrow().clone() else { return };
        let node = self.output();
        let Some(node) = node.filter(|n| (n.id, n.serial) == target.identity) else {
            self.fail("The default audio output changed");
            return;
        };
        if !node.available {
            self.fail("This output has no volume control");
            return;
        }
        let observed = (node.volume, node.mute);
        let complete = (node.volume - target.volume).abs() < 0.005 && node.mute == target.muted;
        if complete || observed != target.observed {
            if let Some(t) = self.0.target.borrow_mut().as_mut() {
                t.observed = observed;
            }
            (self.0.show)(Shown { value: Some(node.volume * 100.0), muted: Some(node.mute), detail: node.description.clone() });
        }
        if complete {
            *self.0.target.borrow_mut() = None;
            self.stop_watch();
            // Collapse the next burst against this confirmed state; action
            // order is preserved at 0/150% and when mute is interleaved.
            let queued = std::mem::take(&mut *self.0.queued.borrow_mut());
            if !queued.is_empty() {
                self.apply(queued);
            }
        }
    }

    pub fn error(&self, message: &str) {
        if self.pending() {
            self.fail(message);
        }
    }

    fn fail(&self, message: &str) {
        self.0.queued.borrow_mut().clear();
        *self.0.target.borrow_mut() = None;
        self.stop_watch();
        (self.0.show)(Shown { value: None, muted: None, detail: message.to_owned() });
    }

    fn watch(&self) {
        if self.0.timer.get() == 0 {
            let weak = self.0.weak.borrow().clone();
            let id = self.0.scheduler.timeout(100, Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    VolumeKeys(inner).tick();
                }
            }));
            self.0.timer.set(id);
        }
    }

    fn tick(&self) {
        self.0.timer.set(0);
        self.changed();
        if self.pending() && (self.0.clock)() >= self.0.deadline.get() {
            let message = if !self.ready() {
                let error = self.backend_error();
                if error.is_empty() { "Audio services are unavailable".to_owned() } else { error }
            } else {
                "Audio output did not confirm the change".to_owned()
            };
            self.fail(&message);
        }
        if self.pending() {
            self.watch();
        }
    }

    fn stop_watch(&self) {
        let timer = self.0.timer.replace(0);
        if timer != 0 {
            self.0.scheduler.cancel(timer);
        }
        self.0.deadline.set(0.0);
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.queued.borrow_mut().clear();
        *self.0.target.borrow_mut() = None;
        self.stop_watch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AudioState, LevelCallback, LevelMeter, Outcome};
    use crate::model::Node;
    use std::cell::Ref;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Sched {
        time: Cell<f64>,
        callbacks: RefCell<BTreeMap<u64, Box<dyn FnOnce()>>>,
        serial: Cell<u64>,
    }

    impl Scheduler for Sched {
        fn timeout(&self, _ms: u32, callback: Box<dyn FnOnce()>) -> u64 {
            self.serial.set(self.serial.get() + 1);
            self.callbacks.borrow_mut().insert(self.serial.get(), callback);
            self.serial.get()
        }
        fn cancel(&self, id: u64) {
            self.callbacks.borrow_mut().remove(&id);
        }
    }

    impl Sched {
        fn tick(&self, elapsed: f64) {
            self.time.set(self.time.get() + elapsed);
            let callbacks = std::mem::take(&mut *self.callbacks.borrow_mut());
            for (_, callback) in callbacks {
                callback();
            }
        }
    }

    struct Backend {
        state: RefCell<AudioState>,
        writes: RefCell<Vec<(&'static str, i64, f64)>>,
        pending: RefCell<Vec<(&'static str, f64)>>,
        fail_volume: Cell<bool>,
    }

    impl Backend {
        fn new() -> Rc<Self> {
            let mut node = Node::new(20, 200, "speakers", "Speakers", "Audio/Sink");
            node.volume = 0.5;
            let mut state = AudioState { ready: true, ..Default::default() };
            state.nodes.insert(20, node);
            state.defaults.insert("Audio/Sink".into(), 20);
            Rc::new(Self { state: RefCell::new(state), writes: RefCell::default(), pending: RefCell::default(), fail_volume: Cell::new(false) })
        }
        fn node(&self) -> std::cell::RefMut<'_, Node> {
            std::cell::RefMut::map(self.state.borrow_mut(), |s| s.nodes.get_mut(&20).unwrap())
        }
        fn confirm(&self) {
            for (key, value) in self.pending.borrow_mut().drain(..) {
                let mut node = self.node();
                if key == "volume" { node.volume = value } else { node.mute = value != 0.0 }
            }
        }
    }

    impl AudioApi for Backend {
        fn state(&self) -> Ref<'_, AudioState> { self.state.borrow() }
        fn start(&self) {}
        fn close(&self) {}
        fn set_volume(&self, id: i64, volume: f64) -> Outcome {
            if self.fail_volume.get() {
                return Ok(false);
            }
            self.writes.borrow_mut().push(("volume", id, volume));
            self.pending.borrow_mut().push(("volume", volume));
            Ok(true)
        }
        fn set_mute(&self, id: i64, muted: bool) -> Outcome {
            self.writes.borrow_mut().push(("mute", id, muted as i32 as f64));
            self.pending.borrow_mut().push(("mute", muted as i32 as f64));
            Ok(true)
        }
        fn set_channel_volume(&self, _: i64, _: i64, _: f64) -> Outcome { Ok(true) }
        fn set_default(&self, _: i64) -> Outcome { Ok(true) }
        fn move_stream(&self, _: i64, _: i64) -> Outcome { Ok(true) }
        fn set_profile(&self, _: i64, _: i64) -> Outcome { Ok(true) }
        fn set_route(&self, _: i64, _: i64) -> Outcome { Ok(true) }
        fn level_monitor(&self, _: i64, _: LevelCallback) -> Option<Box<dyn LevelMeter>> { None }
        fn add_loopback(&self, _: &str, _: i64, _: i64) -> Outcome { Ok(true) }
        fn remove_loopback(&self, _: &str) -> Outcome { Ok(true) }
    }

    struct T {
        backend: Rc<Backend>,
        sched: Rc<Sched>,
        shown: Rc<RefCell<Vec<Shown>>>,
        keys: VolumeKeys,
    }

    impl T {
        fn new() -> Self {
            let backend = Backend::new();
            let sched = Rc::new(Sched::default());
            let shown: Rc<RefCell<Vec<Shown>>> = Rc::default();
            let (sink, clock) = (shown.clone(), sched.clone());
            let keys = VolumeKeys::new(backend.clone(), move |s| sink.borrow_mut().push(s), sched.clone(), move || clock.time.get());
            Self { backend, sched, shown, keys }
        }
        fn confirm(&self) {
            self.backend.confirm();
            self.keys.changed();
        }
        fn request(&self, action: &str) {
            self.keys.request(KeyAction::parse(action).unwrap());
        }
        fn last(&self) -> Shown { self.shown.borrow().last().unwrap().clone() }
        fn volumes(&self) -> Vec<f64> { self.backend.writes.borrow().iter().map(|w| w.2).collect() }
    }

    fn shown(value: Option<f64>, muted: Option<bool>, detail: &str) -> Shown {
        Shown { value, muted, detail: detail.into() }
    }

    #[test]
    fn startup_burst_waits_and_uses_actual_initial_volume() {
        let t = T::new();
        t.backend.state.borrow_mut().ready = false;
        for _ in 0..3 { t.request("up"); }
        assert!(t.backend.writes.borrow().is_empty() && t.shown.borrow().is_empty());
        t.backend.node().volume = 0.25;
        t.backend.state.borrow_mut().ready = true;
        t.keys.changed();
        assert_eq!(*t.backend.writes.borrow(), [("volume", 20, 0.4)]);
        assert!(t.shown.borrow().is_empty());
        t.confirm();
        assert_eq!(t.last(), shown(Some(40.0), Some(false), "Speakers"));
        assert!(!t.keys.pending());
        assert!(t.sched.callbacks.borrow().is_empty());
    }

    #[test]
    fn repeated_keys_accumulate_before_acknowledgement_and_show_confirmed_state() {
        let t = T::new();
        for _ in 0..3 { t.request("up"); }
        assert_eq!(t.volumes(), [0.55]);
        assert!(t.shown.borrow().is_empty());
        t.confirm();
        assert!((t.last().value.unwrap() - 55.0).abs() < 1e-9);
        assert_eq!(t.volumes(), [0.55, 0.65]);
        assert!(t.keys.pending());
        t.confirm();
        assert_eq!(t.last().value, Some(65.0));
        assert!(!t.keys.pending());
    }

    #[test]
    fn opposite_keys_wait_for_each_native_ack_before_returning_to_initial_value() {
        let t = T::new();
        t.request("up");
        t.request("down");
        assert!(t.keys.pending());
        assert_eq!(*t.backend.writes.borrow(), [("volume", 20, 0.55)]);
        t.confirm();
        assert_eq!(*t.backend.writes.borrow(), [("volume", 20, 0.55), ("volume", 20, 0.5)]);
        assert!(t.keys.pending());
        t.confirm();
        assert_eq!(t.backend.node().volume, 0.5);
        assert_eq!(t.last().value, Some(50.0));
    }

    #[test]
    fn mute_burst_toggles_pending_state_and_volume_keys_preserve_mute() {
        let t = T::new();
        t.request("mute");
        t.request("mute");
        assert_eq!(*t.backend.writes.borrow(), [("mute", 20, 1.0)]);
        t.confirm();
        assert_eq!(*t.backend.writes.borrow(), [("mute", 20, 1.0), ("mute", 20, 0.0)]);
        t.confirm();
        t.request("mute");
        t.request("up");
        t.confirm();
        t.confirm();
        assert_eq!(t.last().muted, Some(true));
        assert!((t.last().value.unwrap() - 55.0).abs() < 1e-9);
    }

    #[test]
    fn third_mute_uses_queued_intent_after_delayed_first_ack() {
        let t = T::new();
        t.request("mute");
        t.request("mute");
        assert!(t.keys.pending());
        t.confirm();
        t.request("mute");
        t.confirm();
        t.confirm();
        assert_eq!(*t.backend.writes.borrow(), [("mute", 20, 1.0), ("mute", 20, 0.0), ("mute", 20, 1.0)]);
        assert!(t.backend.node().mute);
        assert_eq!(t.last().muted, Some(true));
        assert!(!t.keys.pending());
    }

    #[test]
    fn up_down_up_after_delayed_ack_does_not_add_an_extra_step() {
        let t = T::new();
        t.request("up");
        t.request("down");
        t.confirm();
        t.request("up");
        t.confirm();
        t.confirm();
        assert_eq!(t.volumes(), [0.55, 0.5, 0.55]);
        assert_eq!(t.backend.node().volume, 0.55);
        assert!((t.last().value.unwrap() - 55.0).abs() < 1e-9);
        assert!(!t.keys.pending());
    }

    #[test]
    fn partial_volume_mute_ack_keeps_next_key_queued() {
        let t = T::new();
        t.request("up");
        t.request("mute");
        t.request("up");
        t.confirm();
        t.backend.node().volume = 0.6;
        t.keys.changed();
        assert!(t.keys.pending());
        let count = t.backend.writes.borrow().len();
        t.request("down");
        assert_eq!(t.backend.writes.borrow().len(), count);
        t.confirm();
        t.confirm();
        assert_eq!(t.backend.node().volume, 0.55);
        assert!(t.backend.node().mute);
        assert!(!t.keys.pending());
    }

    #[test]
    fn limits_match_ui_and_order_is_preserved_at_the_limit() {
        let t = T::new();
        t.backend.node().volume = 1.48;
        t.request("up");
        t.request("down");
        t.confirm();
        assert_eq!(*t.backend.writes.borrow().last().unwrap(), ("volume", 20, 1.45));
        t.confirm();
        t.backend.node().volume = 0.02;
        t.request("down");
        t.confirm();
        assert_eq!(t.backend.node().volume, 0.0);
        t.request("down");
        assert_eq!(t.last().value, Some(0.0));
    }

    #[test]
    fn unavailable_device_and_failed_write_do_not_show_requested_success() {
        let t = T::new();
        t.backend.node().volume_available = false;
        t.request("up");
        assert!(t.backend.writes.borrow().is_empty());
        assert_eq!(t.last(), shown(None, None, "This output has no volume control"));
        t.backend.node().volume_available = true;
        t.backend.state.borrow_mut().error = "Device disappeared".into();
        t.backend.fail_volume.set(true);
        t.request("up");
        assert_eq!(t.last(), shown(None, None, "Device disappeared"));
        assert!(!t.keys.pending());
        t.backend.state.borrow_mut().defaults.clear();
        t.request("up");
        assert_eq!(t.last().detail, "No output device available");
    }

    #[test]
    fn default_switch_or_reused_id_does_not_confirm_wrong_device() {
        let t = T::new();
        t.request("up");
        t.backend.node().serial = 201;
        t.backend.node().volume = 0.55;
        t.keys.changed();
        assert_eq!(t.last().value, None);
        assert!(t.last().detail.contains("changed"));
        assert!(!t.keys.pending());
    }

    #[test]
    fn startup_and_ack_timeout_cancel_without_late_replay() {
        let t = T::new();
        t.backend.state.borrow_mut().ready = false;
        t.request("up");
        t.sched.tick(10.0);
        assert_eq!(t.last(), shown(None, None, "Audio services are unavailable"));
        t.backend.state.borrow_mut().ready = true;
        t.keys.changed();
        assert!(t.backend.writes.borrow().is_empty());
        t.request("up");
        t.sched.tick(3.0);
        assert_eq!(t.last().value, None);
        assert!(t.last().detail.contains("confirm"));
        assert!(!t.keys.pending());
    }

    #[test]
    fn backend_disconnect_cancels_pending_burst() {
        let t = T::new();
        t.request("up");
        t.backend.state.borrow_mut().ready = false;
        t.backend.state.borrow_mut().error = "PipeWire disconnected".into();
        t.keys.changed();
        assert!(!t.keys.pending());
        assert_eq!(t.last(), shown(None, None, "PipeWire disconnected"));
    }

    #[test]
    fn python_round_matches() {
        assert_eq!(round6(0.25 + 0.05), 0.3);
        assert_eq!(round6(0.5 + 0.05), 0.55);
        assert_eq!(round6(1.48 + 0.05), 1.53);
        assert_eq!(KeyAction::parse("x").unwrap_err(), "Unknown volume action");
    }
}
