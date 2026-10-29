//! Compact Keep Awake controls in the shared desktop popup style
//! (port of keep_awake_ui.py `KeepAwakePopup`).

use crate::timed::TimedBackend;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use suite_core::tr;
use suite_gtk::gtk::prelude::*;
use suite_gtk::gtk::{self, glib};
use suite_gtk::popup::{popup_header, set_popup_width, PopupController, POPUP_PADDING, POPUP_WIDTH};
use suite_gtk::utils::{accessible, append, PRIORITY_APPLICATION};

/// (minutes, label); 0 keeps the session awake until turned off.
pub const DURATION_CHOICES: [(u32, &str); 4] =
    [(30, "For 30 minutes"), (60, "For 1 hour"), (120, "For 2 hours"), (0, "Until turned off")];

pub const CSS: &str = "
window.keep-awake.tray-popup { background: transparent; }
.keep-awake .popup-surface {
    background-color: @theme_bg_color; color: @theme_fg_color;
    border: 1px solid alpha(@theme_fg_color, 0.14); border-radius: 12px;
}
.keep-awake .muted { color: alpha(@theme_fg_color, 0.65); }
.keep-awake .card {
    border-radius: 8px; padding: 12px;
    background-color: alpha(@theme_fg_color, 0.045);
    border: 1px solid alpha(@theme_fg_color, 0.10);
}
.keep-awake .status { font-size: 9pt; }
.keep-awake .error { color: mix(@theme_fg_color, #e05d65, 0.75); }
";

/// What the popup drives: the daemon's [`TimedBackend`], or (embedded in
/// Global Settings) a [`crate::client::KeepAwakeClient`].
pub trait Controls {
    fn ready(&self) -> bool;
    fn active(&self) -> bool;
    fn requested(&self) -> bool;
    fn error(&self) -> String;
    fn duration(&self) -> u32;
    fn until(&self) -> Option<f64>;
    fn set_enabled(&self, enabled: bool);
    fn set_duration(&self, minutes: u32);
    /// Python's `hasattr(backend, "set_duration")`.
    fn has_set_duration(&self) -> bool { true }
}

impl Controls for TimedBackend {
    fn ready(&self) -> bool { TimedBackend::ready(self) }
    fn active(&self) -> bool { TimedBackend::active(self) }
    fn requested(&self) -> bool { TimedBackend::requested(self) }
    fn error(&self) -> String { TimedBackend::error(self) }
    fn duration(&self) -> u32 { TimedBackend::duration(self) }
    fn until(&self) -> Option<f64> { TimedBackend::until(self) }
    fn set_enabled(&self, enabled: bool) { TimedBackend::set_enabled(self, enabled); }
    fn set_duration(&self, minutes: u32) { let _ = TimedBackend::set_duration(self, minutes); }
}

/// The local time Keep Awake ends, following the desktop clock's 12/24-hour choice.
pub fn end_time_text(until: f64) -> String {
    let use_24_hour = suite_core::clock::ClockPreferencesStore::default().load().use_24_hour;
    format_end_time(until, use_24_hour)
}

pub fn format_end_time(until: f64, use_24_hour: bool) -> String {
    let Ok(moment) = glib::DateTime::from_unix_local(until.floor() as i64) else { return String::new() };
    if use_24_hour {
        moment.format("%H:%M").map(String::from).unwrap_or_default()
    } else {
        moment.format("%I:%M %p").map(|s| s.trim_start_matches('0').to_owned()).unwrap_or_default()
    }
}

/// The popup's subtitle and status texts for a state (`refresh()` rules).
pub fn texts(controls: &dyn Controls, end_time: &dyn Fn(f64) -> String) -> (String, String) {
    let subtitle = if controls.active() {
        tr!("Your session will stay awake")
    } else if controls.requested() {
        tr!("Turning on…")
    } else {
        tr!("Your normal power settings apply")
    };
    let error = controls.error();
    let until = controls.until().filter(|u| *u != 0.0);
    let status = if !error.is_empty() {
        error
    } else if controls.active() && until.is_some() {
        tr!("On until {time}. Closing this popup keeps it on.", time = end_time(until.unwrap()))
    } else if controls.active() {
        tr!("Active for this session. Closing this popup keeps it on.")
    } else if controls.requested() {
        tr!("Waiting for system power management…")
    } else {
        tr!("Off · Your power and brightness settings apply.")
    };
    (subtitle, status)
}

pub fn duration_index(minutes: u32) -> u32 {
    DURATION_CHOICES.iter().position(|(value, _)| *value == minutes).unwrap_or(DURATION_CHOICES.len() - 1) as u32
}

struct Inner {
    backend: Rc<dyn Controls>,
    syncing: Cell<bool>,
    window: gtk::Window,
    content: gtk::Box,
    subtitle: gtk::Label,
    toggle: gtk::Switch,
    duration: gtk::DropDown,
    status: gtk::Label,
    css: gtk::CssProvider,
    css_added: Cell<bool>,
    controller: RefCell<Option<PopupController>>,
}

#[derive(Clone)]
pub struct KeepAwakePopup(Rc<Inner>);

impl KeepAwakePopup {
    /// `embedded` (Global Settings) skips the layer-shell [`PopupController`].
    pub fn new(backend: Rc<dyn Controls>, edge: &str, offset: i32, embedded: bool) -> Self {
        let window = gtk::Window::builder().title(tr!("Keep Awake")).build();
        set_popup_width(&window, POPUP_WIDTH);
        window.add_css_class("keep-awake");
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("popup-surface");
        window.set_child(Some(&surface));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .margin_top(POPUP_PADDING).margin_bottom(POPUP_PADDING)
            .margin_start(POPUP_PADDING).margin_end(POPUP_PADDING).build();
        surface.append(&content);
        let heading = popup_header(&["view-reveal-symbolic", "preferences-desktop-screensaver-symbolic"],
                                   &tr!("Keep Awake"), Some(" "), Vec::new());
        append(&content, &heading.root, false, false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        row.add_css_class("card");
        let session_label = gtk::Label::builder().label(tr!("Keep this session awake")).xalign(0.0).wrap(true)
            .max_width_chars(28).build();
        session_label.add_css_class("row-title");
        append(&row, &session_label, true, false);
        let toggle = gtk::Switch::new();
        toggle.set_valign(gtk::Align::Center);
        accessible(&toggle, &tr!("Keep this session awake"));
        append(&row, &toggle, false, false);
        append(&content, &row, false, false);
        let duration_row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        duration_row.add_css_class("card");
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let title = gtk::Label::builder().label(tr!("Keep awake")).xalign(0.0).build();
        title.add_css_class("row-title");
        append(&labels, &title, false, false);
        let detail = gtk::Label::builder().label(tr!("Turns off by itself after this time.")).xalign(0.0).wrap(true)
            .max_width_chars(22).build();
        detail.add_css_class("row-detail");
        append(&labels, &detail, false, false);
        append(&duration_row, &labels, true, false);
        let choices: Vec<String> = DURATION_CHOICES.iter().map(|(_, label)| tr!(label)).collect();
        let choices: Vec<&str> = choices.iter().map(String::as_str).collect();
        let duration = gtk::DropDown::from_strings(&choices);
        duration.set_valign(gtk::Align::Center);
        accessible(&duration, &tr!("Keep awake"));
        append(&duration_row, &duration, false, false);
        append(&content, &duration_row, false, false);
        let detail = gtk::Label::builder()
            .label(tr!("Pause automatic dimming, display off, and sleep for the time you choose.")).xalign(0.0).build();
        detail.add_css_class("row-detail");
        detail.set_wrap(true);
        detail.set_max_width_chars(40);
        append(&content, &detail, false, false);
        let status = gtk::Label::builder().xalign(0.0).build();
        status.add_css_class("status");
        status.add_css_class("row-detail");
        status.set_wrap(true);
        status.set_max_width_chars(40);
        append(&content, &status, false, false);
        let popup = Self(Rc::new(Inner {
            backend, syncing: Cell::new(false), window: window.clone(), content, subtitle: heading.subtitle_label.clone(),
            toggle: toggle.clone(), duration: duration.clone(), status, css, css_added: Cell::new(false),
            controller: RefCell::new(None),
        }));
        popup.add_css();
        let weak = Rc::downgrade(&popup.0);
        toggle.connect_active_notify(move |widget| {
            if let Some(inner) = weak.upgrade() {
                if !inner.syncing.get() {
                    inner.backend.set_enabled(widget.is_active());
                }
            }
        });
        let weak = Rc::downgrade(&popup.0);
        duration.connect_selected_notify(move |widget| {
            if let Some(inner) = weak.upgrade() {
                if inner.syncing.get() || !inner.backend.has_set_duration() {
                    return;
                }
                if let Some((minutes, _)) = DURATION_CHOICES.get(widget.selected() as usize) {
                    inner.backend.set_duration(*minutes);
                }
            }
        });
        suite_gtk::theme::attach(&window, None);
        if !embedded {
            let weak = Rc::downgrade(&popup.0);
            let controller = PopupController::new(&window, move || {
                if let Some(inner) = weak.upgrade() {
                    KeepAwakePopup(inner).refresh();
                }
            }, edge, offset, "service_apps");
            popup.0.controller.replace(Some(controller));
        }
        // Python removes the provider on unrealize; it is re-added if the same
        // popup is realized again (a hidden layer surface may be re-realized).
        let weak = Rc::downgrade(&popup.0);
        window.connect_unrealize(move |_| {
            if let Some(inner) = weak.upgrade() {
                KeepAwakePopup(inner).remove_css();
            }
        });
        let weak = Rc::downgrade(&popup.0);
        window.connect_realize(move |_| {
            if let Some(inner) = weak.upgrade() {
                KeepAwakePopup(inner).add_css();
            }
        });
        popup.refresh();
        popup
    }

    fn add_css(&self) {
        if !self.0.css_added.replace(true) {
            gtk::style_context_add_provider_for_display(&WidgetExt::display(&self.0.window), &self.0.css,
                                                        PRIORITY_APPLICATION);
        }
    }

    fn remove_css(&self) {
        if self.0.css_added.replace(false) {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.0.window), &self.0.css);
        }
    }

    pub fn window(&self) -> &gtk::Window { &self.0.window }
    /// The controls box (what Global Settings embeds).
    pub fn content(&self) -> &gtk::Box { &self.0.content }
    pub fn toggle(&self) -> &gtk::Switch { &self.0.toggle }
    pub fn duration(&self) -> &gtk::DropDown { &self.0.duration }
    pub fn status(&self) -> &gtk::Label { &self.0.status }
    pub fn subtitle(&self) -> &gtk::Label { &self.0.subtitle }
    pub fn controller(&self) -> Option<PopupController> { self.0.controller.borrow().clone() }

    pub fn refresh(&self) {
        let inner = &self.0;
        inner.syncing.set(true);
        let backend = &inner.backend;
        inner.duration.set_selected(duration_index(backend.duration()));
        inner.duration.set_sensitive(backend.has_set_duration());
        inner.toggle.set_active(backend.requested());
        inner.toggle.set_sensitive(backend.ready() || backend.requested());
        let (subtitle, status) = texts(backend.as_ref(), &end_time_text);
        inner.subtitle.set_text(&subtitle);
        inner.status.set_text(&status);
        if backend.error().is_empty() {
            inner.status.remove_css_class("error");
        } else {
            inner.status.add_css_class("error");
        }
        inner.syncing.set(false);
    }

    pub fn show(&self) {
        if let Some(controller) = self.controller() { controller.show() }
    }

    pub fn activate(&self) {
        if let Some(controller) = self.controller() { controller.activate() }
    }

    pub fn dismiss(&self) {
        if let Some(controller) = self.controller() { controller.dismiss() }
    }

    pub fn destroy(&self) {
        if let Some(controller) = self.0.controller.take() {
            controller.close();
        }
        self.remove_css();
        self.0.window.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct State { ready: bool, active: bool, requested: bool, error: String, until: Option<f64> }

    impl Controls for State {
        fn ready(&self) -> bool { self.ready }
        fn active(&self) -> bool { self.active }
        fn requested(&self) -> bool { self.requested }
        fn error(&self) -> String { self.error.clone() }
        fn duration(&self) -> u32 { 0 }
        fn until(&self) -> Option<f64> { self.until }
        fn set_enabled(&self, _: bool) {}
        fn set_duration(&self, _: u32) {}
    }

    #[test]
    fn subtitle_and_status_texts() {
        let time = |_: f64| "10:30".to_owned();
        let mut state = State { ready: true, ..Default::default() };
        assert_eq!(texts(&state, &time), ("Your normal power settings apply".into(),
                                          "Off · Your power and brightness settings apply.".into()));
        state.requested = true;
        assert_eq!(texts(&state, &time), ("Turning on…".into(), "Waiting for system power management…".into()));
        state.active = true;
        assert_eq!(texts(&state, &time).1, "Active for this session. Closing this popup keeps it on.");
        state.until = Some(5000.0);
        assert_eq!(texts(&state, &time), ("Your session will stay awake".into(),
                                          "On until 10:30. Closing this popup keeps it on.".into()));
        state.error = "Could not keep the desktop awake: x".into();
        assert_eq!(texts(&state, &time).1, "Could not keep the desktop awake: x");
    }

    #[test]
    fn durations_and_end_time_format() {
        assert_eq!([30, 60, 120, 0, 45].map(duration_index), [0, 1, 2, 3, 3]);
        let local = glib::DateTime::from_unix_local(1_730_000_000).unwrap();
        assert_eq!(format_end_time(1_730_000_000.7, true), local.format("%H:%M").unwrap().as_str());
        let twelve = format_end_time(1_730_000_000.0, false);
        assert!(!twelve.starts_with('0'));
        assert_eq!(twelve, local.format("%I:%M %p").unwrap().trim_start_matches('0'));
    }
}
