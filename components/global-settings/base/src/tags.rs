//! Python pages attach ad-hoc attributes to widgets (`row.page_name = …`,
//! `widget._gs_leave_prompt = True`). These helpers store them as GObject data.
use gtk::glib;
use gtk::prelude::*;

/// Store a string attribute on `object`.
pub fn set_str(object: &impl IsA<glib::Object>, key: &str, value: &str) {
    // SAFETY: the value type stored under a key is always String for *_str.
    unsafe { object.as_ref().set_data::<String>(&format!("gs-str-{key}"), value.to_owned()) }
}

pub fn get_str(object: &impl IsA<glib::Object>, key: &str) -> Option<String> {
    // SAFETY: only set_str writes this key, always with a String.
    unsafe { object.as_ref().data::<String>(&format!("gs-str-{key}")).map(|v| v.as_ref().clone()) }
}

pub fn set_flag(object: &impl IsA<glib::Object>, key: &str, value: bool) {
    // SAFETY: the value type stored under a key is always bool for *_flag.
    unsafe { object.as_ref().set_data::<bool>(&format!("gs-flag-{key}"), value) }
}

pub fn flag(object: &impl IsA<glib::Object>, key: &str) -> bool {
    // SAFETY: only set_flag writes this key, always with a bool.
    unsafe { object.as_ref().data::<bool>(&format!("gs-flag-{key}")).map(|v| *v.as_ref()).unwrap_or(false) }
}

/// `row.page_name` — sub-page rows the search spotlight selects.
pub const PAGE_NAME: &str = "page_name";
