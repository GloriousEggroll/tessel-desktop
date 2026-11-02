//! GTK side of the service: the popup lifecycle, the keyboard OSD adapter,
//! GDK monitor hotplug signals for the hardware backend, and the
//! tessel-displays launcher.

use crate::app::Surfaces;
use crate::backend::MonitorWatch;
use crate::client::LocalClient;
use crate::keys::{Monitor, Osd};
use crate::launch;
use crate::ui::{BrightnessPopup, Client};
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use tessel_gtk::gtk;
use tessel_gtk::osd::{Adjustment, AdjustmentOsd};

/// `GdkMonitor` → the keyboard controller's monitor description.
pub fn describe(monitor: &gdk::Monitor) -> Monitor {
    let g = monitor.geometry();
    Monitor { geometry: [g.x() as i64, g.y() as i64, g.width() as i64, g.height() as i64], model: monitor.model().map(|m| m.to_string()),
              handle: Some(Rc::new(monitor.clone())) }
}

/// The shared adjustment OSD (namespace `desktop-adjustment-osd`, 1500 ms).
pub struct GtkOsd(pub AdjustmentOsd);

impl Osd for GtkOsd {
    fn show(&self, value: Option<f64>, monitor: Option<&Monitor>, detail: &str) {
        let monitor = monitor.and_then(|m| m.handle.as_ref()).and_then(|h| h.downcast_ref::<gdk::Monitor>()).cloned();
        self.0.show(Adjustment::Brightness, value, false, monitor.as_ref(), detail);
    }

    fn resolve_monitor(&self, callback: Box<dyn FnOnce(Option<Monitor>)>) {
        self.0.resolve_monitor(move |monitor| callback(monitor.as_ref().map(describe)));
    }

    fn close(&self) {
        self.0.close();
    }
}

/// Re-discover outputs only when GDK reports a monitor change.
#[derive(Default)]
pub struct GdkMonitors {
    signals: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    list: RefCell<Option<gio::ListModel>>,
}

impl GdkMonitors {
    fn connect_items(self: &Rc<Self>, changed: &Rc<dyn Fn()>) {
        let Some(list) = self.list.borrow().clone() else { return };
        for index in 0..list.n_items() {
            let Some(item) = list.item(index) else { continue };
            if self.signals.borrow().iter().any(|(owner, _)| *owner == item) {
                continue;
            }
            let Ok(monitor) = item.clone().downcast::<gdk::Monitor>() else { continue };
            for property in ["geometry", "scale-factor"] {
                let changed = changed.clone();
                let id = monitor.connect_notify_local(Some(property), move |_, _| changed());
                self.signals.borrow_mut().push((item.clone(), id));
            }
        }
    }
}

pub struct Watch(pub Rc<GdkMonitors>);

impl MonitorWatch for Watch {
    fn watch(&self, changed: Rc<dyn Fn()>) {
        let Some(display) = gdk::Display::default() else { return };
        let list = display.monitors();
        self.0.list.replace(Some(list.clone()));
        let (weak, on_change) = (Rc::downgrade(&self.0), changed.clone());
        let id = list.connect_items_changed(move |_, _, _, _| {
            if let Some(monitors) = weak.upgrade() {
                monitors.connect_items(&on_change);
                on_change();
            }
        });
        self.0.signals.borrow_mut().push((list.upcast(), id));
        self.0.connect_items(&changed);
    }

    fn close(&self) {
        for (owner, id) in self.0.signals.take() {
            owner.disconnect(id);
        }
        self.0.list.replace(None);
    }
}

pub struct Inner {
    client: LocalClient,
    application_error: Rc<dyn Fn() -> String>,
    open_settings: Rc<dyn Fn()>,
    popup: RefCell<Option<Rc<BrightnessPopup>>>,
    position: RefCell<Option<(String, i32)>>,
    osd: RefCell<Option<Rc<GtkOsd>>>,
}

#[derive(Clone)]
pub struct GtkSurfaces(pub Rc<Inner>);

impl GtkSurfaces {
    pub fn new(client: LocalClient, application_error: Rc<dyn Fn() -> String>, open_settings: Rc<dyn Fn()>) -> Self {
        Self(Rc::new(Inner { client, application_error, open_settings, popup: RefCell::new(None), position: RefCell::new(None),
                             osd: RefCell::new(None) }))
    }

    /// `_ui_changed`: the in-process client saw a new state.
    pub fn refresh(&self) {
        let popup = self.0.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.refresh();
        }
    }

    pub fn popup(&self) -> Option<Rc<BrightnessPopup>> {
        self.0.popup.borrow().clone()
    }

    pub fn adjustment_osd(&self) -> Option<AdjustmentOsd> {
        self.0.osd.borrow().as_ref().map(|o| o.0.clone())
    }
}

impl Surfaces for GtkSurfaces {
    fn show_popup(&self, edge: &str, offset: i32) {
        let position = (edge.to_owned(), offset);
        let existing = self.0.popup.borrow().clone();
        if let Some(popup) = existing {
            if self.0.position.borrow().as_ref() != Some(&position) {
                self.0.popup.replace(None);
                popup.destroy();
            }
        }
        let current = self.0.popup.borrow().clone();
        match current {
            Some(popup) => popup.activate(),
            None => {
                let client: Client = Rc::new(self.0.client.clone());
                let popup = BrightnessPopup::new(client, self.0.application_error.clone(), self.0.open_settings.clone(), edge, offset);
                self.0.popup.replace(Some(popup.clone()));
                self.0.position.replace(Some(position));
                let (weak, window) = (Rc::downgrade(&self.0), popup.window.downgrade());
                popup.window.connect_unrealize(move |unrealized| {
                    let (Some(inner), Some(window)) = (weak.upgrade(), window.upgrade()) else { return };
                    let ours = inner.popup.borrow().as_ref().is_some_and(|p| p.window == window && &window == unrealized);
                    if ours {
                        inner.popup.replace(None);
                    }
                });
                popup.show();
            }
        }
    }

    fn dismiss_popup(&self) {
        let popup = self.0.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.dismiss();
        }
    }

    fn has_popup(&self) -> bool {
        self.0.popup.borrow().is_some()
    }

    fn osd(&self) -> Rc<dyn Osd> {
        let osd = self.0.osd.borrow().clone();
        let osd = osd.unwrap_or_else(|| {
            let created = Rc::new(GtkOsd(AdjustmentOsd::new(None, 1500)));
            self.0.osd.replace(Some(created.clone()));
            created
        });
        osd
    }

    fn launch_settings(&self, done: Box<dyn FnOnce(Option<String>)>) -> Result<(), String> {
        let info = tessel_core::glib_compat::desktop_app_info(launch::SETTINGS_DESKTOP_ID)
            .ok_or_else(|| "That application is no longer installed.".to_owned())?;
        let mut helper = launch::helper_command(tessel_core::paths::program("tessel-files"))
            .ok_or_else(|| "The application launcher (tessel-files) is not installed.".to_owned())?;
        let context = gdk::Display::default().map(|d| d.app_launch_context());
        let base: Vec<(String, String)> = match &context {
            Some(context) => context.environment().iter().filter_map(|item| {
                item.to_str().and_then(|s| s.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned()))
            }).collect(),
            None => std::env::vars().collect(),
        };
        let token = match &context {
            Some(context) if crate::compat::desktop_boolean(&info, "StartupNotify") => context.startup_notify_id(Some(&info), &[]).map(|t| t.to_string()),
            _ => None,
        };
        let environment = launch::launch_environment(base, token.clone());
        helper.extend(["--launch-desktop".into(), launch::SETTINGS_DESKTOP_ID.into()]);
        let command = launch::launch_command(&helper, &environment, &launch::unit_name());
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE);
        let environ: Vec<std::ffi::OsString> = environment.iter().map(|(k, v)| format!("{k}={v}").into()).collect();
        launcher.set_environ(&environ);
        let argv: Vec<&std::ffi::OsStr> = command.iter().map(|w| w.as_ref()).collect();
        let fail_token = |context: &Option<gdk::AppLaunchContext>, token: &Option<String>| {
            if let (Some(context), Some(token)) = (context, token) {
                context.launch_failed(token);
            }
        };
        let process = match launcher.spawn(&argv) {
            Ok(process) => process,
            Err(error) => {
                fail_token(&context, &token);
                return Err(error.message().to_owned());
            }
        };
        let finished = process.clone();
        // Not cancelled on shutdown: the service belongs to the app.
        process.communicate_utf8_async(None, None::<&gio::Cancellable>, move |result| {
            let error = match result {
                Ok(_) if finished.is_successful() => None,
                Ok((_, stderr)) => {
                    let detail = stderr.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "The application service could not start.".into());
                    Some(format!("Could not launch the application: {detail}"))
                }
                Err(error) => Some(format!("Could not launch the application: {}", error.message())),
            };
            if error.is_some() {
                fail_token(&context, &token);
            }
            done(error);
        });
        Ok(())
    }

    fn close(&self) {
        let popup = self.0.popup.take();
        if let Some(popup) = popup {
            popup.destroy();
        }
    }
}

