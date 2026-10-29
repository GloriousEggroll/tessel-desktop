//! `BrightnessApplication` logic (`brightness_manager.py`) without GTK: the
//! D-Bus controls, state JSON, auto-dim tokens, inhibitor checks, keyboard
//! steps and orderly shutdown. Windows live behind [`Surfaces`].

use crate::backend::Hardware;
use crate::client::{LocalClient, Request};
use suite_core::scheduler::Scheduler;
use crate::core::BrightnessCore;
use crate::guard::IdleGuard;
use crate::idle::IdleListener;
use crate::keys::{BrightnessKeys, Osd};
use crate::scaling::{merge_displays, ScaleBackend, ScaleView};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::tr;

/// GTK side of the application (popup, OSD, settings launcher).
pub trait Surfaces {
    fn show_popup(&self, edge: &str, offset: i32);
    fn dismiss_popup(&self);
    fn has_popup(&self) -> bool;
    /// `AdjustmentOSD(app)` (created lazily on the first key press).
    fn osd(&self) -> Rc<dyn Osd>;
    /// Launch `way-displays-gui.desktop`; `done(Some(error))` on asynchronous failure.
    fn launch_settings(&self, done: Box<dyn FnOnce(Option<String>)>) -> Result<(), String>;
    fn close(&self);
}

pub type HardwareFactory = Box<dyn FnOnce(Box<dyn Fn()>, Box<dyn Fn(&str)>) -> Rc<dyn Hardware>>;
pub type ScalingFactory = Box<dyn FnOnce(Box<dyn Fn()>) -> ScaleBackend>;
pub type IdleFactory = Box<dyn FnOnce(Box<dyn Fn(&str)>) -> Rc<dyn IdleListener>>;
pub type GuardFactory = Box<dyn FnOnce() -> Rc<dyn IdleGuard>>;

pub struct Services {
    pub hardware: HardwareFactory,
    pub scaling: ScalingFactory,
    pub idle: IdleFactory,
    pub guard: GuardFactory,
    pub scheduler: Rc<dyn Scheduler>,
    pub config_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
}

pub struct Inner {
    pub backend: RefCell<Option<Rc<dyn Hardware>>>,
    pub core: RefCell<Option<BrightnessCore>>,
    pub scaling: RefCell<Option<ScaleBackend>>,
    pub guard: RefCell<Option<Rc<dyn IdleGuard>>>,
    pub idle: RefCell<Option<Rc<dyn IdleListener>>>,
    pub keyboard: RefCell<Option<BrightnessKeys>>,
    pub osd: RefCell<Option<Rc<dyn Osd>>>,
    pub surfaces: RefCell<Option<Rc<dyn Surfaces>>>,
    pub client: LocalClient,
    scheduler: RefCell<Option<Rc<dyn Scheduler>>>,
    emit: RefCell<Option<Box<dyn Fn(&str)>>>,
    quit: RefCell<Option<Rc<dyn Fn()>>>,
    pub closing: Cell<bool>,
    updating: Cell<bool>,
    refresh_source: RefCell<Option<glib::SourceId>>,
    last_state: RefCell<Option<String>>,
    pub activity: Cell<u64>,
    poll: Cell<Option<u64>>,
    inhibit_pending: Cell<bool>,
    pub error: RefCell<String>,
}

#[derive(Clone)]
pub struct App(pub Rc<Inner>);

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self(Rc::new(Inner {
            backend: RefCell::new(None), core: RefCell::new(None), scaling: RefCell::new(None), guard: RefCell::new(None),
            idle: RefCell::new(None), keyboard: RefCell::new(None), osd: RefCell::new(None), surfaces: RefCell::new(None),
            client: LocalClient::new(|| {}), scheduler: RefCell::new(None), emit: RefCell::new(None), quit: RefCell::new(None),
            closing: Cell::new(false), updating: Cell::new(false), refresh_source: RefCell::new(None), last_state: RefCell::new(None),
            activity: Cell::new(0), poll: Cell::new(None), inhibit_pending: Cell::new(false), error: RefCell::new(String::new()),
        }))
    }

    /// The in-process client must notify the popup; build it with that hook.
    pub fn with_client(client: LocalClient) -> Self {
        let app = Self::new();
        let inner = Rc::try_unwrap(app.0).ok().expect("fresh app");
        Self(Rc::new(Inner { client, ..inner }))
    }

    pub fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub fn core(&self) -> Option<BrightnessCore> {
        self.0.core.borrow().clone()
    }

    pub fn backend(&self) -> Option<Rc<dyn Hardware>> {
        self.0.backend.borrow().clone()
    }

    pub fn idle(&self) -> Option<Rc<dyn IdleListener>> {
        self.0.idle.borrow().clone()
    }

    pub fn guard(&self) -> Option<Rc<dyn IdleGuard>> {
        self.0.guard.borrow().clone()
    }

    pub fn scaling(&self) -> Option<ScaleBackend> {
        self.0.scaling.borrow().clone()
    }

    pub fn set_surfaces(&self, surfaces: Rc<dyn Surfaces>) {
        self.0.surfaces.replace(Some(surfaces));
    }

    fn surfaces(&self) -> Option<Rc<dyn Surfaces>> {
        self.0.surfaces.borrow().clone()
    }

    /// `do_startup` (after the D-Bus object is registered by the caller).
    pub fn startup(&self, services: Services, emit: Box<dyn Fn(&str)>, quit: Rc<dyn Fn()>) {
        self.0.emit.replace(Some(emit));
        self.0.quit.replace(Some(quit));
        self.0.scheduler.replace(Some(services.scheduler.clone()));
        let (w1, w2) = (self.downgrade(), self.downgrade());
        let backend = (services.hardware)(Box::new(move || if let Some(a) = w1.upgrade() { App(a).changed() }),
                                          Box::new(move |m| if let Some(a) = w2.upgrade() { App(a).backend_error(m) }));
        let w = self.downgrade();
        let core = BrightnessCore::new(backend.clone(), move || if let Some(a) = w.upgrade() { App(a).changed() },
                                       services.config_dir.as_deref(), services.state_dir.as_deref());
        let w = self.downgrade();
        let scaling = (services.scaling)(Box::new(move || if let Some(a) = w.upgrade() { App(a).changed() }));
        let guard = (services.guard)();
        let w = self.downgrade();
        let idle = (services.idle)(Box::new(move |m| if let Some(a) = w.upgrade() { App(a).idle_error(m) }));
        self.0.backend.replace(Some(backend.clone()));
        self.0.core.replace(Some(core.clone()));
        self.0.scaling.replace(Some(scaling.clone()));
        self.0.guard.replace(Some(guard.clone()));
        self.0.idle.replace(Some(idle.clone()));
        guard.start();
        backend.start();
        scaling.start();
        let _ = idle.start(core.auto_dim(), core.setting_i64("dim_after"));
        let w = self.downgrade();
        let dispatch: crate::client::Dispatch = Rc::new(move |request| match w.upgrade() {
            Some(inner) => App(inner).dispatch(request),
            None => Err(tr!("Brightness controls are restarting. Try again shortly.")),
        });
        self.0.client.connect(dispatch);
        self.0.client.state_changed(&self.encoded());
        self.arm_inhibition_check();
    }

    fn arm_inhibition_check(&self) {
        let Some(scheduler) = self.0.scheduler.borrow().clone() else { return };
        let w = self.downgrade();
        let id = scheduler.timeout(5000, Box::new(move || {
            if let Some(inner) = w.upgrade() {
                let app = App(inner);
                app.0.poll.set(None);
                if app.check_inhibition() {
                    app.arm_inhibition_check();
                }
            }
        }));
        self.0.poll.set(Some(id));
    }

    pub fn state(&self) -> Value {
        let backend = self.backend();
        let scaling = self.scaling();
        let core = self.core();
        let ready = self.idle().is_some() && (backend.as_ref().is_some_and(|b| b.ready()) || scaling.as_ref().is_some_and(|s| s.ready()));
        let hardware = backend.as_ref().map(|b| b.displays()).unwrap_or_default();
        let displays = merge_displays(&hardware, scaling.as_ref().map(|s| s as &dyn ScaleView));
        let scaling_status = scaling.as_ref().map(|s| s.status())
            .unwrap_or_else(|| json!({"ready": false, "busy": false, "auto_scale": false, "error": ""}));
        let settings = core.as_ref().map(|c| c.settings()).unwrap_or_else(|| json!({}));
        let dimmed = core.as_ref().is_some_and(|c| c.dimmed());
        let busy = backend.as_ref().is_some_and(|b| b.busy()) || core.as_ref().is_some_and(|c| c.busy());
        let error = [self.0.error.borrow().clone(), core.as_ref().map(|c| c.error()).unwrap_or_default(),
                     backend.as_ref().map(|b| b.error()).unwrap_or_default()].into_iter().find(|e| !e.is_empty()).unwrap_or_default();
        json!({"ready": ready, "displays": displays, "scaling": scaling_status, "settings": settings, "dimmed": dimmed,
               "busy": busy, "error": error})
    }

    pub fn encoded(&self) -> String {
        json_dumps(&self.state(), &JsonStyle::DEFAULT)
    }

    /// The `--check` report.
    pub fn health(&self) -> Value {
        let state = self.state();
        let displays: Vec<Value> = state["displays"].as_array().cloned().unwrap_or_default().iter().map(|item| {
            json!({"connector": item["connector"], "name": item.get("name").cloned().unwrap_or(Value::Null),
                   "brightness": item.get("brightness").cloned().unwrap_or(Value::Null),
                   "contrast": item.get("contrast").cloned().unwrap_or(Value::Null),
                   "scale": item.get("scale").cloned().unwrap_or(Value::Null)})
        }).collect();
        json!({"ready": state["ready"], "busy": state["busy"], "dimmed": state["dimmed"], "settings": state["settings"],
               "error": state["error"], "displays": displays, "scaling": state["scaling"]})
    }

    /// `_dbus_method` minus GetState: `Ok(accepted)` or the `.Error` message.
    pub fn dispatch(&self, request: Request) -> Result<bool, String> {
        if self.0.closing.get() {
            return Err(tr!("Brightness controls are restarting. Try again shortly."));
        }
        let (Some(core), Some(backend), Some(scaling), Some(idle)) = (self.core(), self.backend(), self.scaling(), self.idle()) else {
            return Err(tr!("Brightness controls are restarting. Try again shortly."));
        };
        match request {
            Request::SetValue(connector, feature, value, expected_id) => {
                self.0.activity.set(self.0.activity.get() + 1);
                if let Some(keyboard) = self.0.keyboard.borrow().clone() {
                    keyboard.cancel();
                }
                core.set_value(&connector, &feature, value, &expected_id, None)
            }
            Request::SetSettings(_) | Request::SetAutoDim(_) => {
                let settings = match &request {
                    Request::SetSettings(encoded) => serde_json::from_str::<Value>(encoded).map_err(|e| e.to_string())?,
                    Request::SetAutoDim(enabled) => {
                        let mut settings = core.settings();
                        settings["auto_dim"] = (*enabled).into();
                        settings
                    }
                    _ => unreachable!(),
                };
                let accepted = core.set_settings(&settings);
                if accepted {
                    self.0.activity.set(self.0.activity.get() + 1);
                    self.0.error.replace(String::new());
                    let (enabled, delay) = (core.auto_dim(), core.setting_i64("dim_after"));
                    idle.update(enabled, delay)?;
                    self.changed();
                }
                Ok(accepted)
            }
            Request::SetScale(connector, value, expected_id) => Ok(scaling.set_scale(&connector, value, &expected_id)),
            Request::SetAutoScale(enabled) => Ok(scaling.set_auto_scale(enabled)),
            Request::Refresh => {
                backend.refresh();
                scaling.refresh();
                Ok(true)
            }
        }
    }

    pub fn changed(&self) {
        if self.core().is_none() || self.0.closing.get() {
            return;
        }
        if self.0.updating.get() {
            if self.0.refresh_source.borrow().is_none() {
                let w = self.downgrade();
                let source = glib::idle_add_local_once(move || {
                    if let Some(inner) = w.upgrade() {
                        inner.refresh_source.replace(None);
                        App(inner).changed();
                    }
                });
                self.0.refresh_source.replace(Some(source));
            }
            return;
        }
        self.0.updating.set(true);
        if let Some(core) = self.core() {
            core.pump();
        }
        let encoded = self.encoded();
        if self.0.last_state.borrow().as_deref() != Some(encoded.as_str()) {
            self.0.last_state.replace(Some(encoded.clone()));
            if let Some(emit) = self.0.emit.borrow().as_ref() {
                emit(&encoded);
            }
            self.0.client.state_changed(&encoded);
        }
        self.0.updating.set(false);
    }

    /// Backend errors live in backend.error and clear on successful requests.
    fn backend_error(&self, _message: &str) {
        self.changed();
    }

    fn idle_error(&self, message: &str) {
        self.0.error.replace(message.into());
        self.changed();
        if let Some(core) = self.core() {
            core.restore_all(None);
        }
    }

    /// `--idle-dim TOKEN`.
    pub fn dim(&self, token: &str) {
        let (Some(idle), Some(guard), Some(core)) = (self.idle(), self.guard(), self.core()) else { return };
        if !idle.allows(token) || self.0.closing.get() {
            return;
        }
        self.0.activity.set(self.0.activity.get() + 1);
        let activity = self.0.activity.get();
        let (w, token) = (self.downgrade(), token.to_owned());
        let current: Rc<dyn Fn() -> bool> = Rc::new(move || {
            let Some(inner) = w.upgrade() else { return false };
            let app = App(inner);
            !app.0.closing.get() && activity == app.0.activity.get() && app.idle().is_some_and(|i| i.allows(&token))
        });
        guard.idle_inhibited(Box::new(move |inhibited| {
            if !inhibited && current() {
                core.dim(current);
            }
        }));
    }

    /// `--idle-restore TOKEN`.
    pub fn idle_restore(&self, token: &str) {
        if self.idle().is_some_and(|i| i.allows(token)) {
            self.0.activity.set(self.0.activity.get() + 1);
            if let Some(core) = self.core() {
                core.restore_all(None);
            }
        }
    }

    /// Every 5 s while dimmed: restore if an inhibitor appeared.
    pub fn check_inhibition(&self) -> bool {
        if self.0.closing.get() {
            return false;
        }
        let (Some(core), Some(guard)) = (self.core(), self.guard()) else { return false };
        if core.dimmed() && !self.0.inhibit_pending.get() {
            self.0.inhibit_pending.set(true);
            let w = self.downgrade();
            guard.idle_inhibited(Box::new(move |blocked| {
                let Some(inner) = w.upgrade() else { return };
                let app = App(inner);
                app.0.inhibit_pending.set(false);
                if blocked && !app.0.closing.get() {
                    app.0.activity.set(app.0.activity.get() + 1);
                    if let Some(core) = app.core() {
                        core.restore_all(None);
                    }
                }
            }));
        }
        true
    }

    pub fn adjust_brightness(&self, step: f64) -> bool {
        if self.0.closing.get() {
            return false;
        }
        self.0.activity.set(self.0.activity.get() + 1);
        if self.0.keyboard.borrow().is_none() {
            let (Some(surfaces), Some(core)) = (self.surfaces(), self.core()) else { return false };
            let osd = surfaces.osd();
            self.0.osd.replace(Some(osd.clone()));
            self.0.keyboard.replace(Some(BrightnessKeys::new(core, osd, None)));
        }
        let keyboard = self.0.keyboard.borrow().clone();
        keyboard.is_some_and(|k| k.adjust(step))
    }

    /// Opening the controls is the only non-hotplug hardware re-read.
    pub fn show_popup(&self, edge: &str, offset: i32) {
        if let Some(backend) = self.backend() {
            backend.refresh();
        }
        if let Some(surfaces) = self.surfaces() {
            surfaces.show_popup(edge, offset);
        }
    }

    /// `--settings`: the full display manager (way-displays-gui.desktop).
    pub fn show_settings(&self) {
        let Some(surfaces) = self.surfaces() else { return };
        surfaces.dismiss_popup();
        let w = self.downgrade();
        let launched = surfaces.launch_settings(Box::new(move |error| {
            if let (Some(error), Some(inner)) = (error, w.upgrade()) {
                let app = App(inner);
                app.0.error.replace(error);
                app.changed();
            }
        }));
        if let Err(error) = launched {
            self.0.error.replace(tr!("Could not open display settings: {error}", error = error));
            self.changed();
        }
    }

    /// SIGTERM/SIGINT: restore dimmed hardware, then quit.
    pub fn stop(&self) {
        if self.0.closing.get() {
            return;
        }
        self.0.closing.set(true);
        self.0.activity.set(self.0.activity.get() + 1);
        if let Some(keyboard) = self.0.keyboard.borrow().clone() {
            keyboard.close();
        }
        if let Some(osd) = self.0.osd.borrow().clone() {
            osd.close();
        }
        if let Some(idle) = self.idle() {
            idle.close();
        }
        let quit = self.0.quit.borrow().clone();
        match self.core() {
            Some(core) => core.restore_all(Some(Box::new(move || if let Some(quit) = quit { quit() }))),
            None => if let Some(quit) = quit { quit() },
        }
    }

    /// `do_shutdown`.
    pub fn shutdown(&self) {
        self.0.closing.set(true);
        if let Some(keyboard) = self.0.keyboard.borrow().clone() {
            keyboard.close();
        }
        if let Some(osd) = self.0.osd.borrow().clone() {
            osd.close();
        }
        if let (Some(id), Some(scheduler)) = (self.0.poll.take(), self.0.scheduler.borrow().clone()) {
            scheduler.cancel(id);
        }
        if let Some(source) = self.0.refresh_source.take() {
            source.remove();
        }
        self.0.client.close();
        if let Some(idle) = self.idle() {
            idle.close();
        }
        if let Some(backend) = self.backend() {
            backend.close();
        }
        if let Some(scaling) = self.scaling() {
            scaling.close();
        }
        if let Some(guard) = self.guard() {
            guard.close();
        }
        if let Some(surfaces) = self.surfaces() {
            surfaces.close();
        }
    }
}
