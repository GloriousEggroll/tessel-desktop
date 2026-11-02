//! Release the dock preset grab while preserving the menu for screenshots
//! (capture_popups.py): Tessel Dock's `org.gtk.Actions` `prepare-capture(b)` and
//! `finish-screenshot`.

use gio::prelude::*;

pub const NAME: &str = "org.tessel.Dock";
pub const PATH: &str = "/org/tessel/Dock";

pub fn prepare_dock_popup(done: impl FnOnce() + 'static, bus: Option<gio::DBusConnection>, recording: bool) {
    call_action(bus, "prepare-capture", vec![recording.to_variant()], done);
}

pub fn restore_dock_popup(bus: Option<gio::DBusConnection>) {
    call_action(bus, "finish-screenshot", Vec::new(), || {});
}

pub fn call_action(bus: Option<gio::DBusConnection>, name: &str, values: Vec<glib::Variant>, done: impl FnOnce() + 'static) {
    let Some(bus) = bus else {
        done();
        return;
    };
    let values = glib::Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, values.iter().map(|v| v.to_variant()));
    let platform = glib::VariantDict::new(None).end();
    let parameters = glib::Variant::tuple_from_iter([name.to_variant(), values, platform]);
    // Capture also works when the dock is absent or an older version.
    bus.call(Some(NAME), PATH, "org.gtk.Actions", "Activate", Some(&parameters), None, gio::DBusCallFlags::NO_AUTO_START, 1000,
             None::<&gio::Cancellable>, move |_| done());
}
