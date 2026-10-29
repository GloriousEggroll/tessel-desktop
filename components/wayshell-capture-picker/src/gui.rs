//! The picker application (capture_picker.py `choose`).

use crate::sources::CaptureSource;
use crate::ui::{CapturePicker, Options};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const APPLICATION_ID: &str = "org.wayshell.CapturePicker";

struct State {
    sources: Vec<CaptureSource>,
    window: RefCell<Option<CapturePicker>>,
    selected: RefCell<Option<CaptureSource>>,
    finished: Cell<bool>,
    app: gtk::Application,
}

impl State {
    fn select(&self, source: CaptureSource) {
        if !self.finished.get() && self.sources.contains(&source) {
            self.selected.replace(Some(source));
            self.finish();
        }
    }

    fn cancel(&self) {
        if !self.finished.get() {
            self.selected.replace(None);
            self.finish();
        }
    }

    fn finish(&self) {
        self.finished.set(true);
        let window = self.window.borrow().clone();
        if let Some(window) = window {
            window.destroy();
        }
        self.app.quit();
    }
}

/// Show the picker and return the deliberately chosen source (`None` = cancel).
/// The caller has already hardened the portal environment and loaded i18n.
pub fn choose(sources: &[CaptureSource], remote_control: u8) -> Result<Option<CaptureSource>, String> {
    gtk::init().map_err(|_| "Failed to open display".to_owned())?;
    let app = gtk::Application::new(Some(APPLICATION_ID), gio::ApplicationFlags::NON_UNIQUE);
    let state = Rc::new(State {
        sources: sources.to_vec(), window: RefCell::new(None), selected: RefCell::new(None), finished: Cell::new(false),
        app: app.clone(),
    });
    app.connect_activate({
        let state = Rc::downgrade(&state);
        move |app| {
            let Some(state) = state.upgrade() else { return };
            if state.window.borrow().is_none() {
                let on_selected = {
                    let state = Rc::downgrade(&state);
                    Box::new(move |source| if let Some(s) = state.upgrade() { s.select(source) })
                };
                let on_cancel = {
                    let state = Rc::downgrade(&state);
                    Box::new(move || if let Some(s) = state.upgrade() { s.cancel() })
                };
                let picker = CapturePicker::new(app, state.sources.clone(), on_selected, on_cancel,
                                                Options { remote_control, ..Default::default() });
                state.window.replace(Some(picker));
            }
            let window = state.window.borrow().clone();
            if let Some(window) = window {
                window.present();
            }
        }
    });
    let parent = crate::entry::parent_pid();
    let mut watches = vec![glib::timeout_add_seconds_local(1, {
        let state = Rc::downgrade(&state);
        move || {
            if unsafe { libc::getppid() } != parent {
                if let Some(state) = state.upgrade() {
                    state.cancel();
                }
            }
            glib::ControlFlow::Continue
        }
    })];
    for signum in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let state = Rc::downgrade(&state);
        watches.push(suite_core::glib_compat::unix_signal_add_local(signum, move || {
            if let Some(state) = state.upgrade() {
                state.cancel();
            }
            glib::ControlFlow::Continue
        }));
    }
    let argv0 = std::env::args().next().unwrap_or_else(|| "wayshell-capture-picker".into());
    let result = app.run_with_args(&[argv0]);
    for watch in watches {
        suite_core::glib_compat::source_remove(watch);
    }
    let window = state.window.borrow().clone();
    if let Some(window) = window {
        window.destroy();
    }
    if result == glib::ExitCode::SUCCESS {
        Ok(state.selected.borrow().clone())
    } else {
        Ok(None)
    }
}
