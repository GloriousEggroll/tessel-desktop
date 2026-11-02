//! Result notifications with explicit capture-output hints and file actions
//! (capture_notifications.py).
#![allow(deprecated)]

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use tessel_core::tr;

pub const BUS: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

/// Notification buttons; screenshots also offer Copy and Edit.
pub fn actions(recording: bool) -> Vec<String> {
    let mut result = vec!["default".to_string(), tr!("Open"), "folder".into(), tr!("Open Folder")];
    if !recording {
        result.extend(["copy".to_string(), tr!("Copy"), "edit".into(), tr!("Edit")]);
    }
    result
}

struct Pending {
    uri: String,
    timer: glib::SourceId,
    _hold: gio::ApplicationHoldGuard,
}

pub type OpenSaved = Rc<dyn Fn(&gtk::Application, &str, bool)>;

pub struct Inner {
    app: glib::WeakRef<gtk::Application>,
    open_saved: OpenSaved,
    bus: Option<gio::DBusConnection>,
    pending: RefCell<HashMap<u32, Pending>>,
    signals: RefCell<Vec<gio::SignalSubscriptionId>>,
}

#[derive(Clone)]
pub struct CaptureNotifications(pub Rc<Inner>);

impl CaptureNotifications {
    pub fn new(application: &gtk::Application, open_saved: OpenSaved) -> Self {
        let bus = application.dbus_connection();
        let this = Self(Rc::new(Inner { app: application.downgrade(), open_saved, bus: bus.clone(), pending: RefCell::default(), signals: RefCell::default() }));
        if let Some(bus) = bus {
            for name in ["ActionInvoked", "NotificationClosed"] {
                let weak = Rc::downgrade(&this.0);
                let id = bus.signal_subscribe(Some(BUS), Some(BUS), Some(name), Some(PATH), None, gio::DBusSignalFlags::NONE,
                    move |_, _, _, _, signal, args| {
                        if let Some(inner) = weak.upgrade() {
                            CaptureNotifications(inner).signal(signal, args);
                        }
                    });
                this.0.signals.borrow_mut().push(id);
            }
        }
        this
    }

    /// Ids of notifications whose actions are still routed (Python `pending`).
    pub fn pending(&self) -> Vec<u32> {
        self.0.pending.borrow().keys().copied().collect()
    }

    pub fn saved(&self, title: &str, path: &Path, output: &str, workspace: bool, recording: bool) {
        let (Some(application), Some(bus)) = (self.0.app.upgrade(), self.0.bus.clone()) else { return };
        let uri = crate::results::file_uri(path);
        let hints = glib::VariantDict::new(None);
        hints.insert("desktop-entry", "org.tessel.Capture");
        hints.insert("x-tessel-dock-output", output);
        hints.insert("x-tessel-dock-workspace", workspace);
        let hold = application.hold();
        let body = path.to_string_lossy().into_owned();
        let parameters = glib::Variant::tuple_from_iter([
            (if recording { tr!("Screen Recorder") } else { tr!("Screenshot") }).to_variant(), 0u32.to_variant(),
            (if recording { "media-record-symbolic" } else { "camera-photo-symbolic" }).to_variant(), title.to_variant(),
            body.to_variant(), actions(recording).to_variant(), hints.end(), 10000i32.to_variant()]);
        let weak = Rc::downgrade(&self.0);
        let (title, path) = (title.to_string(), path.to_path_buf());
        bus.call(Some(BUS), PATH, BUS, "Notify", Some(&parameters), Some(glib::VariantTy::new("(u)").unwrap()), gio::DBusCallFlags::NONE,
                 3000, None::<&gio::Cancellable>, move |result| {
            let Some(inner) = weak.upgrade() else { return };
            match result.ok().and_then(|r| r.child_value(0).get::<u32>()) {
                Some(ident) => {
                    let weak = Rc::downgrade(&inner);
                    let timer = glib::timeout_add_seconds_local(60, move || {
                        if let Some(inner) = weak.upgrade() {
                            CaptureNotifications(inner).expire(ident);
                        }
                        glib::ControlFlow::Break
                    });
                    if let Some(old) = inner.pending.borrow_mut().insert(ident, Pending { uri, timer, _hold: hold }) {
                        old.timer.remove();
                    }
                }
                None => {
                    drop(hold);
                    // Keep normal desktop notification behaviour with another daemon.
                    if let Some(application) = inner.app.upgrade() {
                        application.send_notification(Some("capture"), &crate::results::saved_notification(&title, &body, &path, recording));
                    }
                }
            }
        });
    }

    fn signal(&self, name: &str, args: &glib::Variant) {
        let ident = args.child_value(0).get::<u32>().unwrap_or(0);
        let uri = match self.0.pending.borrow().get(&ident) {
            Some(entry) => entry.uri.clone(),
            None => return,
        };
        if name == "ActionInvoked" {
            let value = args.child_value(1).str().unwrap_or("").to_string();
            if let Some(application) = self.0.app.upgrade() {
                match value.as_str() {
                    "default" | "folder" => (self.0.open_saved)(&application, &uri, value == "folder"),
                    "copy" => crate::results::copy_saved(&application, &uri),
                    "edit" => crate::results::edit_saved(&application, &uri),
                    _ => {}
                }
            }
        }
        if name == "NotificationClosed" {
            self.expire_entry(ident, false);
        }
    }

    fn expire(&self, ident: u32) {
        // Called from the timer itself: the source is finishing, do not remove it.
        self.expire_entry(ident, true);
    }

    fn expire_entry(&self, ident: u32, from_timer: bool) {
        let entry = self.0.pending.borrow_mut().remove(&ident);
        if let Some(entry) = entry {
            if !from_timer {
                entry.timer.remove();
            }
        }
    }

    pub fn close(&self) {
        for (_, entry) in self.0.pending.borrow_mut().drain() {
            entry.timer.remove();
        }
        if let Some(bus) = &self.0.bus {
            for id in self.0.signals.take() {
                bus.signal_unsubscribe(id);
            }
        }
    }
}
