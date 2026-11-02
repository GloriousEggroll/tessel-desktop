//! GTK glue: lazily initialized display, popup/settings ownership and the
//! dim overlay. `--background` never touches the display until a window or
//! the dim step is needed (the Python service imported GTK at startup).

use crate::app::{App, AppInner, Dimmer, Surfaces, APPLICATION_ID};
use crate::dim::DimOverlay;
use crate::ui::{PowerPopup, PowerSettingsWindow};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

thread_local!(static GTK_STATE: Cell<u8> = const { Cell::new(0) });

/// Initialize GTK once; false when no display is available.
pub fn ensure_gtk() -> bool {
    GTK_STATE.with(|state| match state.get() {
        1 => true,
        2 => false,
        _ => {
            let ok = gtk::init().is_ok();
            if !ok {
                glib::g_warning!("tessel-power", "No display is available for power windows.");
            }
            state.set(if ok { 1 } else { 2 });
            ok
        }
    })
}

pub struct GtkDimmer(RefCell<Option<DimOverlay>>);

impl GtkDimmer {
    pub fn new() -> Self {
        Self(RefCell::new(None))
    }
}

impl Default for GtkDimmer {
    fn default() -> Self {
        Self::new()
    }
}

impl Dimmer for GtkDimmer {
    fn dim(&self) -> bool {
        if !ensure_gtk() {
            return false;
        }
        self.0.borrow_mut().get_or_insert_with(DimOverlay::default).dim()
    }
    fn restore(&self) {
        if let Some(overlay) = self.0.borrow().as_ref() {
            overlay.restore();
        }
    }
    fn close(&self) {
        if let Some(overlay) = self.0.borrow().as_ref() {
            overlay.close();
        }
    }
}

pub struct GtkSurfaces {
    app: Weak<AppInner>,
    popup: RefCell<Option<Rc<PowerPopup>>>,
    position: RefCell<Option<(String, i64)>>,
    settings: RefCell<Option<Rc<PowerSettingsWindow>>>,
    weak_self: RefCell<Weak<GtkSurfaces>>,
}

impl GtkSurfaces {
    pub fn new(app: &App) -> Rc<Self> {
        let surfaces = Rc::new(Self { app: app.downgrade(), popup: RefCell::new(None), position: RefCell::new(None),
                                      settings: RefCell::new(None), weak_self: RefCell::new(Weak::new()) });
        surfaces.weak_self.replace(Rc::downgrade(&surfaces));
        surfaces
    }

    fn app(&self) -> Option<App> {
        self.app.upgrade().map(App)
    }

    pub fn popup(&self) -> Option<Rc<PowerPopup>> {
        self.popup.borrow().clone()
    }

    pub fn settings_window(&self) -> Option<Rc<PowerSettingsWindow>> {
        self.settings.borrow().clone()
    }
}

impl Surfaces for GtkSurfaces {
    fn refresh(&self) {
        if let Some(popup) = self.popup() {
            popup.refresh();
        }
        if let Some(settings) = self.settings_window() {
            settings.refresh();
        }
    }

    fn has_popup(&self) -> bool {
        self.popup.borrow().is_some()
    }

    fn has_settings(&self) -> bool {
        self.settings.borrow().is_some()
    }

    fn show_popup(&self, edge: &str, offset: i64) {
        let Some(app) = self.app() else { return };
        let Some(backend) = app.backend() else { return };
        if !ensure_gtk() {
            return;
        }
        let position = (edge.to_owned(), offset);
        if self.popup.borrow().is_some() && self.position.borrow().as_ref() != Some(&position) {
            if let Some(popup) = self.popup.take() {
                popup.destroy();
            }
        }
        let existing = self.popup();
        match existing {
            Some(popup) => popup.activate(),
            None => {
                let weak_app = app.downgrade();
                let error = Rc::new(move || weak_app.upgrade().map(|a| App(a).error()).unwrap_or_default());
                let weak = self.weak_self.borrow().clone();
                let open_settings = Rc::new(move || if let Some(s) = weak.upgrade() { s.show_settings() });
                let popup = PowerPopup::new(backend, error, open_settings, edge, offset as i32);
                self.position.replace(Some(position));
                let weak = self.weak_self.borrow().clone();
                let window = popup.window.clone();
                popup.window.connect_unrealize(move |w| {
                    if let Some(s) = weak.upgrade() {
                        let current = s.popup.borrow().as_ref().is_some_and(|p| &p.window == w);
                        if current {
                            s.popup.replace(None);
                        }
                    }
                });
                let _ = window;
                self.popup.replace(Some(popup.clone()));
                popup.show();
            }
        }
    }

    fn show_settings(&self) {
        let Some(app) = self.app() else { return };
        let Some(backend) = app.backend() else { return };
        if !ensure_gtk() {
            return;
        }
        if let Some(popup) = self.popup() {
            popup.dismiss();
        }
        if self.settings.borrow().is_none() {
            let weak_app = app.downgrade();
            let error = Rc::new(move || weak_app.upgrade().map(|a| App(a).error()).unwrap_or_default());
            let weak_app = app.downgrade();
            let save = Rc::new(move |settings: serde_json::Value| weak_app.upgrade().is_some_and(|a| App(a).save(&settings)));
            let window = PowerSettingsWindow::new(backend, error, &app.settings(), save);
            let weak = self.weak_self.borrow().clone();
            window.window.connect_unrealize(move |w| {
                if let Some(s) = weak.upgrade() {
                    let current = s.settings.borrow().as_ref().is_some_and(|p| &p.window == w);
                    if current {
                        s.settings.replace(None);
                    }
                }
            });
            self.settings.replace(Some(window));
        }
        let Some(settings) = self.settings_window() else { return };
        settings.window.set_visible(true);
        settings.refresh();
        settings.window.unminimize();
        settings.window.present();
        let weak = self.weak_self.borrow().clone();
        let target = settings.window.downgrade();
        glib::idle_add_local_once(move || {
            let Some(window) = target.upgrade() else { return };
            let weak = weak.clone();
            tessel_gtk::utils::raise_window(APPLICATION_ID, &window, move |w| {
                weak.upgrade().and_then(|s| s.settings_window()).is_some_and(|s| &s.window == w)
            });
        });
    }

    fn close(&self) {
        if let Some(popup) = self.popup.take() {
            popup.destroy();
        }
    }
}
