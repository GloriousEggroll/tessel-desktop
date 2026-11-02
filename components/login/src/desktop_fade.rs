//! session_transition.py: one-shot, input-transparent desktop fade for labwc
//! sessions started from Tessel Login. Starts no services; exits after fading.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use gtk::gdk;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::transition::Fade;

pub const APPLICATION_ID: &str = "org.tessel.LoginTransition";
pub const CSS: &str = "window.login-transition { background: black; border-radius: 0; box-shadow: none; }";
pub const WATCHED: [&str; 2] = ["org.tessel.Tessel Dock", "com.nobaraDesktopView"];

struct Inner {
    app: gtk::Application,
    windows: RefCell<Vec<(gdk::Monitor, gtk::ApplicationWindow)>>,
    watches: RefCell<Vec<Box<dyn FnOnce()>>>,
    ready: RefCell<Vec<String>>,
    fade: RefCell<Option<Fade>>,
    timer: RefCell<Option<glib::SourceId>>,
    started: Cell<Instant>,
    monitors: RefCell<Option<(gio::ListModel, glib::SignalHandlerId)>>,
    style: RefCell<Option<gtk::CssProvider>>,
    quit: RefCell<Rc<dyn Fn()>>,
}

/// `DesktopFade(Gtk.Application)`.
#[derive(Clone)]
pub struct DesktopFade(Rc<Inner>);

impl Default for DesktopFade {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopFade {
    pub fn new() -> DesktopFade {
        let app = gtk::Application::new(Some(APPLICATION_ID), gio::ApplicationFlags::NON_UNIQUE);
        let quit_app = app.clone();
        let fade = DesktopFade(Rc::new(Inner {
            app, windows: RefCell::default(), watches: RefCell::default(), ready: RefCell::default(), fade: RefCell::default(),
            timer: RefCell::default(), started: Cell::new(Instant::now()), monitors: RefCell::default(), style: RefCell::default(),
            quit: RefCell::new(Rc::new(move || quit_app.quit())),
        }));
        let weak = Rc::downgrade(&fade.0);
        fade.0.app.connect_activate(move |_| if let Some(inner) = weak.upgrade() { DesktopFade(inner).activate() });
        let weak = Rc::downgrade(&fade.0);
        fade.0.app.connect_shutdown(move |_| if let Some(inner) = weak.upgrade() { DesktopFade(inner).shutdown() });
        fade
    }

    pub fn application(&self) -> &gtk::Application {
        &self.0.app
    }

    /// Test seam for Python's `app.quit = Mock()`.
    pub fn set_quit(&self, quit: impl Fn() + 'static) {
        *self.0.quit.borrow_mut() = Rc::new(quit);
    }

    pub fn windows(&self) -> Vec<gtk::ApplicationWindow> {
        self.0.windows.borrow().iter().map(|(_, w)| w.clone()).collect()
    }

    fn quit(&self) {
        let quit = self.0.quit.borrow().clone();
        quit();
    }

    fn activate(&self) {
        if !gtk4_layer_shell::is_supported() {
            self.quit();
            return;
        }
        *self.0.style.borrow_mut() = Some(tessel_gtk::utils::add_css(CSS, None, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION));
        let Some(display) = gdk::Display::default() else { return self.quit() };
        let monitors = display.monitors();
        let weak = Rc::downgrade(&self.0);
        let signal = monitors.connect_items_changed(move |_, _, _, _| if let Some(inner) = weak.upgrade() { DesktopFade(inner).update_monitors() });
        *self.0.monitors.borrow_mut() = Some((monitors, signal));
        self.update_monitors();
        for name in WATCHED {
            let (appeared, vanished) = (Rc::downgrade(&self.0), Rc::downgrade(&self.0));
            let id = gio::bus_watch_name(gio::BusType::Session, name, gio::BusNameWatcherFlags::NONE,
                move |_, name, _| if let Some(inner) = appeared.upgrade() {
                    let mut ready = inner.ready.borrow_mut();
                    if !ready.iter().any(|n| n == name) { ready.push(name.to_owned()) }
                },
                move |_, name| if let Some(inner) = vanished.upgrade() { inner.ready.borrow_mut().retain(|n| n != name) });
            self.0.watches.borrow_mut().push(Box::new(move || gio::bus_unwatch_name(id)));
        }
        let weak = Rc::downgrade(&self.0);
        let timer = glib::timeout_add_local(std::time::Duration::from_millis(100), move || match weak.upgrade() {
            Some(inner) => DesktopFade(inner).wait_ready(),
            None => glib::ControlFlow::Break,
        });
        *self.0.timer.borrow_mut() = Some(timer);
    }

    fn update_monitors(&self) {
        if self.0.fade.borrow().is_some() {
            return;
        }
        let Some(model) = self.0.monitors.borrow().as_ref().map(|(m, _)| m.clone()) else { return };
        let current: Vec<gdk::Monitor> = (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gdk::Monitor>()).collect();
        let removed: Vec<gtk::ApplicationWindow> = {
            let mut windows = self.0.windows.borrow_mut();
            let (keep, gone): (Vec<_>, Vec<_>) = windows.drain(..).partition(|(m, _)| current.contains(m));
            *windows = keep;
            gone.into_iter().map(|(_, w)| w).collect()
        };
        for window in removed {
            window.destroy();
        }
        for monitor in current {
            if self.0.windows.borrow().iter().any(|(m, _)| *m == monitor) {
                continue;
            }
            let window = gtk::ApplicationWindow::builder().application(&self.0.app).decorated(false).css_classes(["login-transition"]).build();
            window.init_layer_shell();
            window.set_namespace(Some("tessel-login-transition"));
            window.set_monitor(Some(&monitor));
            window.set_layer(Layer::Overlay);
            window.set_exclusive_zone(-1);
            window.set_keyboard_mode(KeyboardMode::None);
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
            window.connect_realize(|widget| {
                if let Some(surface) = widget.surface() {
                    surface.set_input_region(Some(&gtk::cairo::Region::create()));
                }
            });
            self.0.windows.borrow_mut().push((monitor, window.clone()));
            window.present();
        }
    }

    fn wait_ready(&self) -> glib::ControlFlow {
        let elapsed = self.0.started.get().elapsed().as_secs_f64();
        // Give initial wallpaper uploads a moment, but never hide a failed or
        // customized desktop indefinitely. No services are started here.
        if elapsed < 2.0 && (elapsed < 0.8 || self.0.ready.borrow().len() < 2) {
            return glib::ControlFlow::Continue;
        }
        self.0.timer.borrow_mut().take();
        let widgets = self.windows().into_iter().map(|w| w.upcast()).collect();
        let weak = Rc::downgrade(&self.0);
        let fade = Fade::new(widgets, 1.0, 0.0, 400, move || if let Some(inner) = weak.upgrade() { DesktopFade(inner).quit() });
        *self.0.fade.borrow_mut() = Some(fade);
        glib::ControlFlow::Break
    }

    pub fn shutdown(&self) {
        if let Some(timer) = self.0.timer.borrow_mut().take() {
            timer.remove();
        }
        if let Some(fade) = self.0.fade.borrow().as_ref() {
            fade.cancel();
        }
        if let Some((model, signal)) = self.0.monitors.borrow_mut().take() {
            model.disconnect(signal);
        }
        let watches: Vec<_> = self.0.watches.borrow_mut().drain(..).collect();
        for unwatch in watches {
            unwatch();
        }
        for window in self.windows() {
            window.destroy();
        }
        if let (Some(provider), Some(display)) = (self.0.style.borrow_mut().take(), gdk::Display::default()) {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
    }
}
