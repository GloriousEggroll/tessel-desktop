//! Alt+Tab controller (wayshell_switcher.py), using native Wayland window
//! identities from the in-process foreign-toplevel backend.

pub mod audio;
pub mod backend;
pub mod model;
pub mod overview;
pub mod ui;

use crate::app_catalog::AppCatalog;
use audio::Audio;
use backend::Backend;
use gtk::gdk;
use gtk::prelude::*;
use model::MruSwitcher;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use ui::{Item, Mode, SwitcherView};

pub type BackendFactory = Box<dyn FnOnce(backend::Callback, backend::ErrorCallback) -> Rc<dyn Backend>>;
pub type AudioFactory = Rc<dyn Fn(Rc<dyn Fn()>) -> Rc<dyn Audio>>;
pub type ViewFactory = Rc<dyn Fn(&gtk::Application, ui::IdFn, Rc<dyn Fn()>, Option<ui::IdFn>, Option<ui::IdFn>) -> Rc<SwitcherView>>;

pub struct WindowSwitcher {
    application: gtk::Application,
    pub model: RefCell<MruSwitcher>,
    catalog: RefCell<Option<AppCatalog>>,
    pub view: RefCell<Option<Rc<SwitcherView>>>,
    audio: RefCell<Option<Rc<dyn Audio>>>,
    audio_factory: AudioFactory,
    view_factory: ViewFactory,
    pub backend: Rc<dyn Backend>,
    closed: Cell<bool>,
    mode: Cell<Mode>,
    alt_check: RefCell<Option<glib::SourceId>>,
    modifiers_idle: RefCell<Option<glib::SourceId>>,
    gesture_closed: Cell<bool>,
    last_alt: Cell<bool>,
    keyboard: RefCell<Option<(gdk::Device, glib::SignalHandlerId)>>,
    display: gdk::Display,
    seat_handler: RefCell<Option<glib::SignalHandlerId>>,
    /// Theme refresh hook (`application._waydock_theme.refresh()`).
    pub refresh_theme: RefCell<Option<Rc<dyn Fn()>>>,
    me: RefCell<Weak<WindowSwitcher>>,
}

impl WindowSwitcher {
    pub fn new(application: &gtk::Application, backend_factory: Option<BackendFactory>, audio_factory: Option<AudioFactory>,
               view_factory: ViewFactory, catalog: Option<AppCatalog>) -> Rc<Self> {
        let display = gdk::Display::default().expect("display");
        let cell: Rc<RefCell<Weak<WindowSwitcher>>> = Rc::default();
        let (c1, c2) = (cell.clone(), cell.clone());
        let on_changed: backend::Callback = Rc::new(move || {
            if let Some(s) = c1.borrow().upgrade() {
                s.changed();
            }
        });
        let on_error: backend::ErrorCallback = Rc::new(move |message: &str| {
            if let Some(s) = c2.borrow().upgrade() {
                s.error(message);
            }
        });
        let backend: Rc<dyn Backend> = match backend_factory {
            Some(factory) => factory(on_changed, on_error),
            None => backend::NativeBackend::new(on_changed, on_error),
        };
        let switcher = Rc::new(Self {
            application: application.clone(), model: RefCell::default(), catalog: RefCell::new(catalog), view: RefCell::new(None),
            audio: RefCell::new(None), audio_factory: audio_factory.unwrap_or_else(|| Rc::new(audio::default_audio)),
            view_factory, backend, closed: Cell::new(false), mode: Cell::new(Mode::Hidden), alt_check: RefCell::new(None),
            modifiers_idle: RefCell::new(None), gesture_closed: Cell::new(false), last_alt: Cell::new(false),
            keyboard: RefCell::new(None), display: display.clone(), seat_handler: RefCell::new(None),
            refresh_theme: RefCell::new(None), me: RefCell::new(Weak::new()),
        });
        *switcher.me.borrow_mut() = Rc::downgrade(&switcher);
        *cell.borrow_mut() = Rc::downgrade(&switcher);
        let weak = Rc::downgrade(&switcher);
        let handler = display.connect_seat_added(move |_, _| {
            if let Some(s) = weak.upgrade() {
                s.bind_keyboard();
            }
        });
        *switcher.seat_handler.borrow_mut() = Some(handler);
        switcher.bind_keyboard();
        switcher.backend.start();
        switcher
    }

    pub fn default_view_factory(wallpaper: ui::WallpaperFn) -> ViewFactory {
        Rc::new(move |app, activate, cancel, close, mute| SwitcherView::new(app, activate, cancel, close, mute, wallpaper.clone()))
    }

    fn weak(&self) -> Weak<WindowSwitcher> {
        self.me.borrow().clone()
    }

    pub fn mode(&self) -> Mode {
        self.mode.get()
    }

    fn bind_keyboard(&self) {
        let keyboard = self.display.default_seat().and_then(|s| s.keyboard());
        let same = match (&*self.keyboard.borrow(), &keyboard) {
            (Some((k, _)), Some(n)) => k == n,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if let Some((old, handler)) = self.keyboard.borrow_mut().take() {
            old.disconnect(handler);
        }
        if let Some(keyboard) = keyboard {
            let weak = self.weak();
            let handler = keyboard.connect_modifier_state_notify(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.modifiers_changed();
                }
            });
            *self.keyboard.borrow_mut() = Some((keyboard, handler));
        }
        self.last_alt.set(self.alt_down());
    }

    fn alt_down(&self) -> bool {
        self.keyboard.borrow().as_ref().is_some_and(|(k, _)| k.modifier_state().contains(gdk::ModifierType::ALT_MASK))
    }

    fn modifiers_changed(&self) {
        let alt = self.alt_down();
        if alt && !self.last_alt.get() {
            self.gesture_closed.set(false);
        }
        self.last_alt.set(alt);
        if self.mode.get() == Mode::Keyboard && self.model.borrow().active && !alt && self.modifiers_idle.borrow().is_none() {
            self.queue_alt_idle();
        }
    }

    fn queue_alt_idle(&self) {
        let weak = self.weak();
        let id = glib::idle_add_local_once(move || {
            if let Some(s) = weak.upgrade() {
                s.modifiers_idle.borrow_mut().take();
                s.finish_if_released();
            }
        });
        *self.modifiers_idle.borrow_mut() = Some(id);
    }

    fn view_active(&self) -> bool {
        self.view.borrow().as_ref().is_some_and(|v| v.window.is_active())
    }

    fn finish_if_released(&self) {
        if self.mode.get() == Mode::Keyboard && self.model.borrow().active && self.view_active() && !self.alt_down() {
            let selected = self.model.borrow().selected_id();
            if let Some(id) = selected {
                self.choose(&id);
            }
        }
    }

    fn focus_changed(&self) {
        if self.mode.get() == Mode::Keyboard && self.view_active() && self.model.borrow().active && self.modifiers_idle.borrow().is_none() {
            // wl_keyboard.enter is followed by current modifiers: wait for the batch.
            self.queue_alt_idle();
        }
    }

    fn update_model(&self) {
        let windows = self.backend.windows();
        self.model.borrow_mut().update(windows.iter().map(|w| (w.id.as_str(), w.activated)));
    }

    fn changed(&self) {
        if self.closed.get() {
            return;
        }
        let active = self.model.borrow().active;
        self.update_model();
        let now_active = self.model.borrow().active;
        if self.mode.get() == Mode::Activities {
            if !now_active {
                self.model.borrow_mut().begin(false, true);
            }
            self.render();
        } else if active && !now_active {
            self.cancel();
        } else if now_active {
            self.render();
        }
    }

    fn error(&self, message: &str) {
        if self.mode.get() != Mode::Hidden {
            self.cancel();
        }
        eprintln!("Wayshell window switcher: {message}");
    }

    pub fn step(&self, reverse: bool) -> bool {
        if self.closed.get() || !self.backend.ready() || !self.backend.can_activate() {
            return false;
        }
        self.bind_keyboard();
        // Ignore late repeats after Escape within the same held-Alt gesture.
        if self.gesture_closed.get() {
            return true;
        }
        self.update_model();
        if self.model.borrow().active {
            self.model.borrow_mut().advance(reverse);
            if self.mode.get() == Mode::Activities {
                self.mode.set(Mode::Keyboard);
                if let Some(v) = self.view.borrow().as_ref() {
                    v.set_mode(Mode::Keyboard);
                }
                self.watch_alt();
            }
            self.render();
            return true;
        }
        if self.model.borrow_mut().begin(reverse, false).is_none() {
            return true;
        }
        self.mode.set(Mode::Keyboard);
        let view = self.ensure_view();
        view.set_mode(Mode::Keyboard);
        self.render();
        view.show(self.active_monitor());
        self.do_refresh_theme();
        self.watch_alt();
        true
    }

    fn ensure_view(&self) -> Rc<SwitcherView> {
        if self.audio.borrow().is_none() {
            let weak = self.weak();
            let audio = (self.audio_factory)(Rc::new(move || {
                if let Some(s) = weak.upgrade() {
                    s.render();
                }
            }));
            *self.audio.borrow_mut() = Some(audio.clone());
            audio.start();
        }
        if let Some(view) = self.view.borrow().as_ref() {
            return view.clone();
        }
        let (w1, w2, w3, w4) = (self.weak(), self.weak(), self.weak(), self.weak());
        let view = (self.view_factory)(
            &self.application,
            Rc::new(move |id: &str| if let Some(s) = w1.upgrade() { s.choose(id) }),
            Rc::new(move || if let Some(s) = w2.upgrade() { s.cancel() }),
            Some(Rc::new(move |id: &str| if let Some(s) = w3.upgrade() { s.close_window(id); })),
            Some(Rc::new(move |id: &str| if let Some(s) = w4.upgrade() { s.mute_window(id); })),
        );
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.weak();
        keys.connect_key_pressed(move |_, keyval, _, state| match weak.upgrade() {
            Some(s) if s.key_pressed(keyval, state) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        let weak = self.weak();
        keys.connect_key_released(move |_, keyval, _, _| {
            if let Some(s) = weak.upgrade() {
                s.key_released(keyval);
            }
        });
        view.window.add_controller(keys);
        let weak = self.weak();
        view.window.connect_is_active_notify(move |_| {
            if let Some(s) = weak.upgrade() {
                s.focus_changed();
            }
        });
        *self.view.borrow_mut() = Some(view.clone());
        view
    }

    fn do_refresh_theme(&self) {
        let hook = self.refresh_theme.borrow().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn watch_alt(&self) {
        if self.alt_check.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(s) = weak.upgrade() else { return glib::ControlFlow::Break };
            if s.mode.get() != Mode::Keyboard || !s.model.borrow().active {
                s.alt_check.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            s.finish_if_released();
            if s.mode.get() == Mode::Keyboard && s.model.borrow().active {
                glib::ControlFlow::Continue
            } else {
                s.alt_check.borrow_mut().take();
                glib::ControlFlow::Break
            }
        });
        *self.alt_check.borrow_mut() = Some(id);
    }

    /// `show_activities(monitor_name)`; `monitor` is the raw request value.
    pub fn show_activities(&self, monitor: Option<&Value>) -> bool {
        if self.closed.get() || !self.backend.ready() || !self.backend.can_activate() {
            return false;
        }
        let monitor = match monitor {
            Some(value) => {
                let Some(name) = value.as_str() else { return false };
                if crate::cli::monitor_name(name).is_err() {
                    return false;
                }
                let found = crate::monitors::monitor_list(&self.display).into_iter()
                    .find(|m| m.connector().is_some_and(|c| c == name));
                match found {
                    Some(m) => Some(m),
                    None => return false,
                }
            }
            None if self.mode.get() != Mode::Hidden && self.view.borrow().is_some() => {
                self.view.borrow().as_ref().and_then(|v| v.monitor.borrow().clone())
            }
            None => self.active_monitor(),
        };
        self.stop_alt_watch();
        self.gesture_closed.set(false);
        self.mode.set(Mode::Activities);
        self.update_model();
        if !self.model.borrow().active {
            self.model.borrow_mut().begin(false, true);
        }
        let view = self.ensure_view();
        view.set_mode(Mode::Activities);
        self.render();
        if !view.window.is_visible() || *view.monitor.borrow() != monitor {
            view.show(monitor);
        }
        self.do_refresh_theme();
        true
    }

    fn active_monitor(&self) -> Option<gdk::Monitor> {
        let monitors = crate::monitors::monitor_list(&self.display);
        for window in self.backend.windows() {
            if window.activated {
                for name in &window.outputs {
                    if let Some(m) = monitors.iter().find(|m| m.connector().is_some_and(|c| c == name.as_str())) {
                        return Some(m.clone());
                    }
                }
            }
        }
        // Desktop icon interactions can be focused while no ordinary window is.
        let toplevels = gtk::Window::toplevels();
        for i in 0..toplevels.n_items() {
            if let Some(window) = toplevels.item(i).and_downcast::<gtk::Window>() {
                if window.is_active() {
                    if let Some(surface) = window.surface() {
                        return self.display.monitor_at_surface(&surface);
                    }
                }
            }
        }
        None
    }

    pub fn render(&self) {
        let Some(view) = self.view.borrow().clone() else { return };
        if self.catalog.borrow().is_none() {
            *self.catalog.borrow_mut() = Some(AppCatalog::new());
        }
        let audio = self.audio.borrow().clone();
        let windows = self.backend.windows();
        let mut items = Vec::new();
        {
            let catalog = self.catalog.borrow();
            let catalog = catalog.as_ref().unwrap();
            for ident in self.model.borrow().ordered_ids() {
                let Some(window) = windows.iter().find(|w| w.id == ident) else { continue };
                let app = catalog.resolve(Some(&window.app_id), None);
                let (available, muted) = audio.as_ref().map(|a| a.state(&window.app_id)).unwrap_or((false, false));
                items.push(Item {
                    id: ident.clone(),
                    name: app.map(|a| a.name.clone()).unwrap_or_else(|| if window.app_id.is_empty() { "Application".into() } else { window.app_id.clone() }),
                    title: if window.title.is_empty() { "Untitled window".into() } else { window.title.clone() },
                    icon: app.and_then(|a| a.icon.clone()),
                    minimized: window.minimized, app_id: window.app_id.clone(), audio_available: available, muted,
                });
            }
        }
        let selected = self.model.borrow().selected_id();
        view.render(&items, selected.as_deref());
    }

    fn key_pressed(&self, keyval: gdk::Key, state: gdk::ModifierType) -> bool {
        use gdk::Key;
        if keyval == Key::Escape {
            self.cancel();
            return true;
        }
        if keyval == Key::Return || keyval == Key::KP_Enter {
            let selected = self.model.borrow().selected_id();
            match selected {
                None => self.cancel(),
                Some(id) => self.choose(&id),
            }
            return true;
        }
        if [Key::Tab, Key::ISO_Left_Tab, Key::Left, Key::Right, Key::Up, Key::Down].contains(&keyval) {
            let reverse = [Key::Left, Key::Up, Key::ISO_Left_Tab].contains(&keyval) || state.contains(gdk::ModifierType::SHIFT_MASK);
            if self.mode.get() == Mode::Activities && (keyval == Key::Tab || keyval == Key::ISO_Left_Tab)
                && state.contains(gdk::ModifierType::ALT_MASK) {
                return self.step(reverse);
            }
            let steps = if keyval == Key::Up || keyval == Key::Down {
                self.view.borrow().as_ref().map(|v| v.columns()).unwrap_or(1)
            } else {
                1
            };
            for _ in 0..steps {
                self.model.borrow_mut().advance(reverse);
            }
            self.render();
            return true;
        }
        false
    }

    fn key_released(&self, keyval: gdk::Key) {
        if self.mode.get() == Mode::Keyboard && (keyval == gdk::Key::Alt_L || keyval == gdk::Key::Alt_R) && self.model.borrow().active {
            // The release event's state can still include Alt: inspect the device later.
            if self.modifiers_idle.borrow().is_none() {
                self.queue_alt_idle();
            }
        }
    }

    fn stop_alt_watch(&self) {
        for slot in [&self.alt_check, &self.modifiers_idle] {
            if let Some(source) = slot.borrow_mut().take() {
                source.remove();
            }
        }
    }

    fn hide(&self) {
        self.gesture_closed.set(self.mode.get() == Mode::Keyboard);
        self.mode.set(Mode::Hidden);
        self.stop_alt_watch();
        if let Some(audio) = self.audio.borrow_mut().take() {
            audio.close();
        }
        let view = self.view.borrow().clone();
        if let Some(view) = view {
            view.dismiss();
            // Overview thumbnails and capture buffers are freed now; hand the
            // memory back to the OS once the dismissal has rendered.
            glib::timeout_add_local_once(std::time::Duration::from_millis(500), || unsafe {
                libc::malloc_trim(0);
            });
            // Activation happens through another Wayland connection: complete
            // unmapping first so the exclusive layer cannot refuse client focus.
            self.display.sync();
        }
    }

    pub fn choose(&self, ident: &str) {
        if !self.model.borrow().active || !self.backend.contains(ident) {
            return;
        }
        self.model.borrow_mut().finish();
        self.hide();
        self.backend.activate(ident);
    }

    pub fn close_window(&self, ident: &str) -> bool {
        if self.closed.get() || self.mode.get() == Mode::Hidden || !self.backend.contains(ident) {
            return false;
        }
        // The app handles its close request (save prompts); the card stays until it is gone.
        self.backend.close_window(ident)
    }

    pub fn mute_window(&self, ident: &str) -> bool {
        let audio = self.audio.borrow().clone();
        match (audio, self.backend.window(ident)) {
            (Some(audio), Some(window)) if self.mode.get() != Mode::Hidden => audio.toggle(&window.app_id),
            _ => false,
        }
    }

    pub fn cancel(&self) {
        self.model.borrow_mut().cancel();
        self.hide();
    }

    pub fn status(&self) -> Map<String, Value> {
        let monitor = self.view.borrow().as_ref().and_then(|v| v.monitor.borrow().as_ref().and_then(|m| m.connector()).map(|c| c.to_string()));
        let value = json!({
            "ready": self.backend.ready(), "can_activate": self.backend.can_activate(),
            "visible": self.mode.get() != Mode::Hidden, "mode": self.mode.get().as_str(),
            "monitor": monitor, "windows": self.backend.windows().len(), "error": self.backend.error(),
        });
        value.as_object().cloned().unwrap_or_default()
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.cancel();
        self.backend.close();
        if let Some((keyboard, handler)) = self.keyboard.borrow_mut().take() {
            keyboard.disconnect(handler);
        }
        if let Some(handler) = self.seat_handler.borrow_mut().take() {
            self.display.disconnect(handler);
        }
        if let Some(view) = self.view.borrow_mut().take() {
            view.close();
        }
    }
}
