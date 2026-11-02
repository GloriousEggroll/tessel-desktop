//! The resident notification service (port of notification_manager.py's
//! `Application`): owns the bus name, presents toasts per output under the
//! fullscreen/DND policy, and opens the history center and settings.

use crate::backend::WindowBackend;
use crate::model::{app_key, Event, NotificationModel, SharedModel};
use crate::policy::FullscreenWatcher;
use crate::service::{Command, NotificationService};
use crate::ui::{Client, NotificationCenter, NotificationSettings, ToastStack};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Layer, LayerShell};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const APPLICATION_ID: &str = "org.tessel.Notifications";

type FocusSignature = Vec<(String, bool, bool, bool, Vec<String>)>;

pub struct Inner {
    pub app: gtk::Application,
    pub model: RefCell<Option<SharedModel>>,
    pub service: RefCell<Option<NotificationService>>,
    pub backend: RefCell<Option<WindowBackend>>,
    pub guard: RefCell<Option<FullscreenWatcher>>,
    pub center: RefCell<Option<NotificationCenter>>,
    pub settings: RefCell<Option<NotificationSettings>>,
    pub toast_stacks: RefCell<Vec<((gdk::Monitor, bool), ToastStack)>>,
    pub pending: RefCell<BTreeSet<u32>>,
    pub eligible: RefCell<BTreeSet<u32>>,
    /// The unix-signal sources (SIGINT, SIGTERM).
    pub sources: RefCell<Vec<glib::SourceId>>,
    pub failure: RefCell<String>,
    focus_signature: RefCell<Option<FocusSignature>>,
    pub presentation_hold: Cell<bool>,
    // One-shot timeouts for the next toast expiry and timed-DND end. Nothing
    // is scheduled while idle, so the daemon does not wake.
    pub expiry_source: RefCell<Option<glib::SourceId>>,
    pub dnd_source: RefCell<Option<glib::SourceId>>,
    monitors: RefCell<Option<gio::ListModel>>,
    monitor_signal: RefCell<Option<glib::SignalHandlerId>>,
}

#[derive(Clone)]
pub struct Daemon(pub Rc<Inner>);

/// The in-process client the daemon's own views use.
struct LocalClient(Weak<Inner>);

impl LocalClient {
    fn daemon(&self) -> Option<Daemon> { self.0.upgrade().map(Daemon) }
    fn model(&self) -> Option<SharedModel> { self.daemon().and_then(|d| d.model()) }
}

impl Client for LocalClient {
    fn state(&self) -> Map<String, Value> {
        self.model().and_then(|m| m.state().as_object().cloned()).unwrap_or_default()
    }
    fn ready(&self) -> bool { true }
    fn error(&self) -> String { self.model().map(|m| m.with(|m| m.storage_error.clone())).unwrap_or_default() }
    fn set_settings(&self, patch: Value) {
        if let Some(model) = self.model() {
            if let Err(error) = model.set_settings(&patch) {
                // Python raises out of the GTK callback (logged, state unchanged).
                eprintln!("Notifications: {error}");
            }
        }
    }
    fn clear_history(&self) { if let Some(m) = self.model() { m.clear_history() } }
    fn clear_app(&self, key: &str) { if let Some(m) = self.model() { m.clear_app(key); } }
    fn mark_read(&self) { if let Some(m) = self.model() { m.mark_read() } }
    fn dismiss(&self, ident: u32) { if let Some(m) = self.model() { m.dismiss(ident); } }
    fn activate(&self, ident: u32, key: &str) { if let Some(m) = self.model() { m.activate(ident, key); } }
    fn show_settings(&self) { if let Some(d) = self.daemon() { d.show_settings() } }
}

impl Daemon {
    pub fn new() -> Self {
        let app = gtk::Application::new(Some(APPLICATION_ID), gio::ApplicationFlags::NON_UNIQUE);
        let daemon = Self(Rc::new(Inner {
            app: app.clone(), model: RefCell::new(None), service: RefCell::new(None), backend: RefCell::new(None),
            guard: RefCell::new(None), center: RefCell::new(None), settings: RefCell::new(None),
            toast_stacks: RefCell::new(Vec::new()), pending: RefCell::new(BTreeSet::new()), eligible: RefCell::new(BTreeSet::new()),
            sources: RefCell::new(Vec::new()), failure: RefCell::new(String::new()), focus_signature: RefCell::new(None),
            presentation_hold: Cell::new(true), expiry_source: RefCell::new(None), dnd_source: RefCell::new(None),
            monitors: RefCell::new(None), monitor_signal: RefCell::new(None),
        }));
        let weak = daemon.weak();
        app.connect_startup(move |_| if let Some(d) = upgrade(&weak) { d.startup() });
        // A resident service starts without opening a settings window.
        app.connect_activate(|_| {});
        let weak = daemon.weak();
        app.connect_shutdown(move |_| if let Some(d) = upgrade(&weak) { d.shutdown() });
        daemon
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    pub fn app(&self) -> &gtk::Application { &self.0.app }
    pub fn model(&self) -> Option<SharedModel> { self.0.model.borrow().clone() }
    pub fn guard(&self) -> Option<FullscreenWatcher> { self.0.guard.borrow().clone() }
    pub fn backend(&self) -> Option<WindowBackend> { self.0.backend.borrow().clone() }
    pub fn center(&self) -> Option<NotificationCenter> { self.0.center.borrow().clone() }
    pub fn failure(&self) -> String { self.0.failure.borrow().clone() }
    pub fn toast_stacks(&self) -> Vec<((gdk::Monitor, bool), ToastStack)> { self.0.toast_stacks.borrow().clone() }
    /// The first stack (Python `app.toasts`).
    pub fn toasts(&self) -> Option<ToastStack> { self.0.toast_stacks.borrow().first().map(|e| e.1.clone()) }

    fn client(&self) -> Rc<dyn Client> { Rc::new(LocalClient(self.weak())) }

    /// Run like `app.run([argv0])`; 1 when startup failed.
    ///
    /// The bus name is owned (and both interfaces exported) before GTK
    /// initializes, so early `Notify` callers queue instead of failing; their
    /// calls are dispatched once the main loop runs after `startup`.
    pub fn run(&self) -> i32 {
        if self.model().is_none() {
            if let Err(error) = self.own_name() {
                self.0.failure.replace(error.clone());
                eprintln!("Notifications: {error}");
                return 1;
            }
        }
        let argv0 = std::env::args().next().unwrap_or_else(|| "tessel-notifications".into());
        let result = self.0.app.run_with_args(&[argv0]);
        if self.failure().is_empty() { i32::from(result) } else { 1 }
    }

    fn startup(&self) {
        // A resident service with no windows must not quit.
        std::mem::forget(self.0.app.hold());
        if let Err(error) = self.try_startup() {
            self.0.failure.replace(error.clone());
            eprintln!("Notifications: {error}");
            let weak = self.weak();
            glib::idle_add_local_once(move || if let Some(d) = upgrade(&weak) { d.stop() });
        }
    }

    /// Create the model and own `org.freedesktop.Notifications` (DO_NOT_QUEUE).
    pub fn own_name(&self) -> Result<(), String> {
        let model = SharedModel::new(NotificationModel::new(None));
        self.0.model.replace(Some(model.clone()));
        let weak = self.weak();
        model.subscribe(move |event| {
            if let (Some(daemon), Event::Changed(kind, ident)) = (upgrade(&weak), event) {
                daemon.changed(kind, *ident);
            }
        });
        let connection = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).map_err(|e| e.message().to_owned())?;
        let weak = self.weak();
        let service = NotificationService::new(&connection, &model, Some(Rc::new(move |command| {
            upgrade(&weak).map_or(Ok(()), |daemon| daemon.command(command))
        })));
        self.0.service.replace(Some(service.clone()));
        service.start()
    }

    fn try_startup(&self) -> Result<(), String> {
        if self.model().is_none() {
            self.own_name()?;
        }
        let weak = self.weak();
        let backend = WindowBackend::new(move || if let Some(d) = upgrade(&weak) { d.windows_changed() }, None);
        self.0.backend.replace(Some(backend.clone()));
        let weak = self.weak();
        let guard = FullscreenWatcher::new(&backend, move || if let Some(d) = upgrade(&weak) { d.policy_changed() }, None);
        self.0.guard.replace(Some(guard));
        backend.start();
        self.schedule_expiry();
        self.schedule_dnd();
        let display = gdk::Display::default().ok_or("No display")?;
        let monitors = display.monitors();
        let weak = self.weak();
        let signal = monitors.connect_items_changed(move |_, _, _, _| if let Some(d) = upgrade(&weak) { d.outputs_changed() });
        self.0.monitors.replace(Some(monitors));
        self.0.monitor_signal.replace(Some(signal));
        for signum in [libc::SIGINT, libc::SIGTERM] {
            let weak = self.weak();
            let id = tessel_core::glib_compat::unix_signal_add_local(signum, move || {
                if let Some(d) = upgrade(&weak) {
                    d.0.sources.borrow_mut().retain(|_| false);
                    d.stop();
                }
                glib::ControlFlow::Break
            });
            self.0.sources.borrow_mut().push(id);
        }
        Ok(())
    }

    fn monitor_list(&self) -> Vec<gdk::Monitor> {
        let monitors = self.0.monitors.borrow();
        let Some(monitors) = monitors.as_ref() else { return Vec::new() };
        (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>()).collect()
    }

    fn suppress_fullscreen(&self) -> bool {
        self.model().is_none_or(|m| m.with(|m| m.setting_bool("suppress_fullscreen", true)))
    }

    pub fn windows_changed(&self) {
        let Some(backend) = self.backend() else { return };
        let signature: FocusSignature = backend.windows().values()
            .map(|w| (w.id.clone(), w.activated, w.fullscreen, w.minimized, w.outputs.clone())).collect();
        if self.0.focus_signature.borrow().as_ref() == Some(&signature) {
            return;
        }
        self.0.focus_signature.replace(Some(signature));
        self.0.presentation_hold.set(true);
        // Hide immediately while deciding whether newly focused X11 content is
        // a borderless game. The bounded probe is off the main thread.
        if self.suppress_fullscreen() {
            for (_, stack) in self.toast_stacks() {
                stack.hide();
            }
        }
        // Event-driven: the guard probes X11 only for a new focus signature on
        // a window that may be Xwayland.
        if let Some(guard) = self.guard() {
            guard.refresh(false);
        }
    }

    /// Forget the cached focus signature (test hook, Python `focus_signature = None`).
    pub fn reset_focus_signature(&self) {
        self.0.focus_signature.replace(None);
    }

    pub fn schedule_expiry(&self) {
        if let Some(source) = self.0.expiry_source.take() {
            source.remove();
        }
        let Some(deadline) = self.model().and_then(|m| m.with(|m| m.next_deadline())) else { return };
        let delay = (((deadline - crate::model::monotonic()) * 1000.0).ceil() + 1.0).max(1.0) as u64;
        let weak = self.weak();
        let id = glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            if let Some(daemon) = upgrade(&weak) {
                daemon.0.expiry_source.replace(None);
                if let Some(model) = daemon.model() {
                    model.tick();
                }
                daemon.schedule_expiry();
            }
        });
        self.0.expiry_source.replace(Some(id));
    }

    pub fn schedule_dnd(&self) {
        if let Some(source) = self.0.dnd_source.take() {
            source.remove();
        }
        let Some(model) = self.model() else { return };
        let (dnd, until) = model.with(|m| (m.setting_bool("do_not_disturb", false), m.dnd_until()));
        if !(dnd && until != 0.0) {
            return;
        }
        // The main-loop clock pauses during suspend; cap each wait so a
        // wall-clock end is noticed within a minute of resuming.
        let delay = ((until - crate::model::wall_clock()).ceil().max(1.0)).min(60.0) as u32;
        let weak = self.weak();
        let id = glib::timeout_add_seconds_local_once(delay, move || {
            if let Some(daemon) = upgrade(&weak) {
                daemon.0.dnd_source.replace(None);
                let expired = daemon.model().is_some_and(|m| m.expire_dnd());
                if !expired {
                    daemon.schedule_dnd();
                }
            }
        });
        self.0.dnd_source.replace(Some(id));
    }

    pub fn hovered(&self, ident: u32, value: bool) {
        if let Some(model) = self.model() {
            model.set_hovered(ident, value);
        }
        self.schedule_expiry();
    }

    /// Whether ordinary popups are paused (DND or focused fullscreen).
    pub fn suppressed(&self) -> bool {
        let Some(model) = self.model() else { return false };
        model.with(|m| m.dnd_active())
            || (self.suppress_fullscreen() && self.guard().is_some_and(|g| g.blocked()))
    }

    /// Muted apps never pop up; critical urgency bypasses DND and fullscreen.
    pub fn may_present(&self, item: Option<&Value>) -> bool {
        let (Some(item), Some(model)) = (item, self.model()) else { return false };
        if model.with(|m| m.app_muted_key(&app_key(item))) {
            return false;
        }
        item.get("urgency").and_then(Value::as_i64) == Some(2) || !self.suppressed()
    }

    fn item_json(&self, ident: u32) -> Option<Value> {
        self.model()?.with(|m| m.items.get(&ident).map(|i| Value::Object(i.to_json())))
    }

    fn drop_suppressed(&self) {
        // Suppressed notifications remain in history; don't burst a backlog
        // of popups when leaving a game or turning DND off.
        for set in [&self.0.pending, &self.0.eligible] {
            let ids: Vec<u32> = set.borrow().iter().copied().collect();
            let keep: BTreeSet<u32> = ids.into_iter().filter(|id| self.may_present(self.item_json(*id).as_ref())).collect();
            set.replace(keep);
        }
    }

    fn center_visible(&self) -> bool {
        self.center().is_some_and(|c| c.window().get_visible())
    }

    pub fn changed(&self, kind: &str, ident: u32) {
        let Some(guard) = self.guard() else { return };
        let Some(model) = self.model() else { return };
        if matches!(kind, "notify" | "close" | "history" | "settings") {
            self.schedule_expiry();
        }
        if kind == "settings" {
            self.schedule_dnd();
        }
        if kind == "notify" {
            let muted = model.with(|m| m.items.get(&ident).is_some_and(|item| m.item_muted(item)));
            if self.center_visible() {
                model.mark_read();
            } else if muted {
                self.0.pending.borrow_mut().remove(&ident);
                self.0.eligible.borrow_mut().remove(&ident);
            } else {
                self.0.pending.borrow_mut().insert(ident);
                // A replacement may already be eligible. Do not paint its new
                // content until this fresh fullscreen check completes.
                self.0.presentation_hold.set(true);
                guard.refresh(true);
            }
        }
        if matches!(kind, "settings" | "history") {
            self.drop_suppressed();
        }
        if kind != "presentation" {
            self.render_toasts();
        }
        if let Some(center) = self.center() {
            center.refresh();
        }
        let settings = self.0.settings.borrow().clone();
        if let Some(settings) = settings {
            settings.refresh();
        }
    }

    pub fn policy_changed(&self) {
        let Some(guard) = self.guard() else { return };
        if !guard.checking() {
            self.0.presentation_hold.set(false);
        }
        if self.suppressed() {
            self.drop_suppressed();
            if guard.blocked() {
                if let Some(center) = self.0.center.take() {
                    center.destroy();
                }
            }
        }
        if !guard.checking() {
            let pending = std::mem::take(&mut *self.0.pending.borrow_mut());
            self.0.eligible.borrow_mut().extend(pending);
        }
        if let Some(model) = self.model() {
            model.set_presentation(guard.blocked(), guard.checking());
        }
        self.render_toasts();
    }

    fn selected_monitor(&self) -> Option<gdk::Monitor> {
        let monitors = self.monitor_list();
        let focused: Vec<String> = self.backend().map(|b| b.windows().values().filter(|w| w.activated && !w.minimized)
            .flat_map(|w| w.outputs.clone()).collect()).unwrap_or_default();
        monitors.iter().find(|m| m.connector().is_some_and(|c| focused.iter().any(|f| *f == c.as_str()))).cloned()
            .or_else(|| monitors.first().cloned())
    }

    pub fn render_toasts(&self) {
        let Some(guard) = self.guard() else { return };
        let Some(model) = self.model() else { return };
        let records = model.with(|m| m.notifications());
        let active: BTreeSet<u32> = records.iter().filter(|i| i.get("active") == Some(&Value::Bool(true)))
            .filter_map(|i| i.get("id").and_then(Value::as_u64)).map(|i| i as u32).collect();
        self.0.eligible.borrow_mut().retain(|id| active.contains(id));
        self.0.pending.borrow_mut().retain(|id| active.contains(id));
        let eligible = self.0.eligible.borrow().clone();
        let shown: Vec<Value> = records.into_iter()
            .filter(|i| i.get("id").and_then(Value::as_u64).is_some_and(|id| eligible.contains(&(id as u32))))
            .filter(|i| self.may_present(Some(i))).collect();
        let suppress = self.suppress_fullscreen();
        if shown.is_empty() || (self.0.presentation_hold.get() && suppress) {
            for (_, stack) in self.toast_stacks() {
                stack.hide();
            }
            return;
        }
        let monitors = self.monitor_list();
        let fallback = self.selected_monitor();
        let groups = crate::placement::group_notifications(&shown, &monitors, fallback.as_ref(), None);
        let stale: Vec<ToastStack> = {
            let mut stacks = self.0.toast_stacks.borrow_mut();
            let mut stale = Vec::new();
            stacks.retain(|(key, stack)| {
                let keep = groups.iter().any(|(k, _)| k == key);
                if !keep {
                    stale.push(stack.clone());
                }
                keep
            });
            stale
        };
        for stack in stale {
            stack.destroy();
        }
        let layer = if suppress && !guard.blocked() { Layer::Top } else { Layer::Overlay };
        for (key, items) in groups {
            let existing = self.0.toast_stacks.borrow().iter().find(|(k, _)| *k == key).map(|e| e.1.clone());
            let stack = match existing {
                Some(stack) => stack,
                None => {
                    let (dismiss, activate, hover) = (self.weak(), self.weak(), self.weak());
                    let stack = ToastStack::new(&self.0.app, Some(&key.0),
                        Rc::new(move |id| if let Some(d) = upgrade(&dismiss) { LocalClient(d.weak()).dismiss(id) }),
                        Rc::new(move |id, action| if let Some(d) = upgrade(&activate) { LocalClient(d.weak()).activate(id, action) }),
                        Some(Rc::new(move |id, value| if let Some(d) = upgrade(&hover) { d.hovered(id, value) })), key.1);
                    self.0.toast_stacks.borrow_mut().push((key.clone(), stack.clone()));
                    stack
                }
            };
            // While fullscreen blocks ordinary popups only critical ones are
            // shown, and they must rise above the fullscreen surface.
            if gtk4_layer_shell::is_supported() {
                stack.window().set_layer(layer);
            }
            stack.update(&items);
        }
    }

    fn outputs_changed(&self) {
        let stacks = std::mem::take(&mut *self.0.toast_stacks.borrow_mut());
        for (_, stack) in stacks {
            stack.destroy();
        }
        self.render_toasts();
    }

    pub fn command(&self, command: Command) -> Result<(), String> {
        match command {
            Command::ShowSettings => self.show_settings(),
            Command::ShowCenter => self.show_center("bottom", 76),
            Command::ShowCenterAt(edge, offset) => {
                if !matches!(edge.as_str(), "top" | "bottom" | "left" | "right") || offset > 4096 {
                    return Err("Invalid notification popup position".into());
                }
                self.show_center(&edge, offset as i32);
            }
        }
        Ok(())
    }

    pub fn show_center(&self, edge: &str, offset: i32) {
        let existing = self.0.center.take();
        if let Some(center) = existing {
            let visible = center.window().get_visible();
            center.destroy();
            if visible {
                return;
            }
        }
        self.0.pending.borrow_mut().clear();
        self.0.eligible.borrow_mut().clear();
        self.render_toasts();
        let center = NotificationCenter::new(&self.0.app, self.client(), edge, offset);
        self.0.center.replace(Some(center.clone()));
        center.present();
        if let Some(model) = self.model() {
            model.mark_read();
        }
    }

    /// Close the center without toggling (test hook, Python `center.destroy(); center = None`).
    pub fn close_center(&self) {
        if let Some(center) = self.0.center.take() {
            center.destroy();
        }
    }

    pub fn show_settings(&self) {
        if let Some(center) = self.0.center.take() {
            center.destroy();
        }
        if let Some(settings) = self.0.settings.take() {
            settings.destroy();
        }
        let settings = NotificationSettings::new(&self.0.app, self.client());
        self.0.settings.replace(Some(settings.clone()));
        settings.present();
    }

    pub fn settings_window(&self) -> Option<NotificationSettings> { self.0.settings.borrow().clone() }

    pub fn stop(&self) {
        self.0.app.quit();
    }

    fn shutdown(&self) {
        if let (Some(monitors), Some(signal)) = (self.0.monitors.borrow().as_ref(), self.0.monitor_signal.take()) {
            monitors.disconnect(signal);
        }
        for source in self.0.sources.take() {
            source.remove();
        }
        for source in [self.0.expiry_source.take(), self.0.dnd_source.take()].into_iter().flatten() {
            source.remove();
        }
        if let Some(guard) = self.0.guard.take() {
            guard.close();
        }
        if let Some(backend) = self.0.backend.take() {
            backend.close();
        }
        if let Some(center) = self.0.center.take() {
            center.destroy();
        }
        if let Some(settings) = self.0.settings.take() {
            settings.destroy();
        }
        for (_, stack) in self.0.toast_stacks.take() {
            stack.destroy();
        }
        if let Some(service) = self.0.service.take() {
            service.close();
        }
    }
}

impl Default for Daemon {
    fn default() -> Self { Self::new() }
}

fn upgrade(weak: &Weak<Inner>) -> Option<Daemon> {
    weak.upgrade().map(Daemon)
}
