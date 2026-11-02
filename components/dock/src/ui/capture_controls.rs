//! panel_ui.register_capture_controls: the application actions Screen
//! Recorder uses while it captures.
//!
//! `register_capture_controls(&app, docks)` adds
//! - `app.prepare-capture` (parameter `b`): `true` (recording starts) pops the
//!   record preset menu down on every dock; `false` (a screenshot) keeps it
//!   open but non-modal — autohide off — and remembers it per panel;
//! - `app.finish-screenshot`: restores autohide on the remembered menu if it
//!   is still that panel's record menu.
//!
//! [`set_autohide`] changes a popover's autohide mode while it is mapped by
//! unmapping under the OLD mode first (so GTK releases its grab), then
//! realize + map again.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

use super::host::DockView;

pub type Docks = Rc<dyn Fn() -> Vec<Rc<dyn DockView>>>;

/// Change autohide on a (possibly mapped) popover without leaving a grab behind.
pub fn set_autohide(menu: &gtk::Popover, enabled: bool) {
    if menu.is_autohide() == enabled {
        return;
    }
    let mapped = menu.is_mapped();
    // Unmap while the OLD mode is in effect so GTK releases its grab.
    if mapped {
        menu.unmap();
    }
    menu.set_autohide(enabled);
    if mapped {
        WidgetExt::realize(menu);
        menu.map();
    }
}

/// `prepare-capture(recording)`.
pub fn prepare_capture(docks: &[Rc<dyn DockView>], recording: bool) {
    for dock in docks {
        let Some(panel) = dock.indicators() else { continue };
        let Some(menu) = panel.record_menu() else { continue };
        if recording {
            menu.popdown();
        } else {
            panel.set_capture_preserved_menu(Some(menu.clone()));
            set_autohide(&menu, false);
        }
    }
}

/// `finish-screenshot`.
pub fn finish_screenshot(docks: &[Rc<dyn DockView>]) {
    for dock in docks {
        let Some(panel) = dock.indicators() else { continue };
        if let Some(menu) = panel.capture_preserved_menu() {
            panel.set_capture_preserved_menu(None);
            if panel.record_menu().as_ref() == Some(&menu) {
                set_autohide(&menu, true);
            }
        }
    }
}

pub fn register_capture_controls(app: &impl IsA<gio::ActionMap>, docks: Docks) {
    let action = gio::SimpleAction::new("prepare-capture", Some(glib::VariantTy::BOOLEAN));
    let source = docks.clone();
    action.connect_activate(move |_, parameter| {
        let recording = parameter.and_then(|value| value.get::<bool>()).unwrap_or(false);
        prepare_capture(&source(), recording);
    });
    app.add_action(&action);
    let action = gio::SimpleAction::new("finish-screenshot", None);
    action.connect_activate(move |_, _| finish_screenshot(&docks()));
    app.add_action(&action);
}
