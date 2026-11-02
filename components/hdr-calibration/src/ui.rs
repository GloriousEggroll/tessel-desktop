//! hdr_calibration.py: visual setup and recorded meter readings.

use crate::calibration::{self, CalError, Host, Profile, Section, FIELDS, PRESETS};
use crate::correction;
use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use tessel_core::tr;
use way_displays::color_config::Status;

pub const STYLE: &str = r#"
.tessel-hdr-calibration .hdr-section { padding: 16px; border-radius: 12px; }
.tessel-hdr-calibration .hdr-section-title { font-size: 10pt; font-weight: 600; opacity: .7; }
.tessel-hdr-calibration .hdr-field-title { font-size: 10.5pt; font-weight: 600; }
.tessel-hdr-calibration .hdr-detail { font-size: 9pt; opacity: .7; }
.tessel-hdr-calibration .hdr-empty { padding: 48px 24px; }
.tessel-hdr-calibration .hdr-empty-title { font-size: 16pt; font-weight: 600; }
.tessel-hdr-calibration .hdr-empty-icon { opacity: .5; }
"#;

/// `(key, label, lower, upper, step, digits, help)`.
pub const FIELD_SPECS: [(&str, &str, f64, f64, f64, u32, &str); 4] = [
    ("black", "Shadow clipping", 0.0, 19.0, 0.001, 4,
     "Reduce the bright patch until it just disappears into black. This estimates a signal threshold; it does not measure emitted black luminance."),
    ("peak", "Small-highlight limit", 40.0, 10000.0, 10.0, 1,
     "Increase the 10% patch until its darker cross just disappears. If it never disappears, the monitor is tone mapping; use measured limits or its documented HDR-mode limits."),
    ("fullFrame", "Full-screen limit", 20.0, 10000.0, 10.0, 1,
     "Increase the full-screen pattern until the cross just disappears. Allow brightness to settle. Hide the instructions with H to view the full field."),
    ("white", "SDR reference white", 20.0, 500.0, 5.0, 1,
     "Choose a comfortable white level for SDR windows in your current room lighting. This changes SDR brightness while preserving absolute PQ HDR signal levels."),
];

/// What the window calls (Python tests patch `calibration.session`, `color_config.read_status`,
/// `calibration.read_profiles`, `calibration.apply`, `calibration.workspace_report`).
pub trait Backend: Send + Sync {
    fn session(&self) -> Result<Status, CalError>;
    fn read_status(&self) -> Status;
    fn read_profiles(&self) -> Result<Vec<Profile>, CalError>;
    fn apply(&self, connector: &str, profile: Option<&Profile>) -> Result<PathBuf, CalError>;
    fn workspace_report(&self, connector: &str, profile: &Profile) -> Result<PathBuf, CalError>;
}

/// The live backend over a [`Host`].
pub struct LiveBackend<H: Host>(pub H);

impl<H: Host> Backend for LiveBackend<H> {
    fn session(&self) -> Result<Status, CalError> { self.0.session() }
    fn read_status(&self) -> Status { self.0.read_status() }
    fn read_profiles(&self) -> Result<Vec<Profile>, CalError> { calibration::read_profiles() }
    fn apply(&self, connector: &str, profile: Option<&Profile>) -> Result<PathBuf, CalError> { calibration::apply(connector, profile, &self.0) }
    fn workspace_report(&self, connector: &str, profile: &Profile) -> Result<PathBuf, CalError> {
        calibration::workspace_report(connector, profile, &self.0)
    }
}

pub struct Inner {
    pub window: gtk::ApplicationWindow,
    pub requested: Option<String>,
    pub names: RefCell<Vec<String>>,
    pub monitor_identity: RefCell<Option<String>>,
    /// Python `self.worker is not None`.
    pub worker: Cell<bool>,
    pub child: RefCell<Option<std::process::Child>>,
    pub alive: Cell<bool>,
    pub loading: Cell<bool>,
    pub correction: RefCell<Option<Value>>,
    pub correction_unresolved: Cell<bool>,
    #[allow(deprecated)]
    pub chooser: RefCell<Option<gtk::FileChooserNative>>,
    pub menu_button: gtk::MenuButton,
    pub pages: gtk::Stack,
    pub outputs: gtk::DropDown,
    pub capability: gtk::Label,
    pub empty_detail: gtk::Label,
    pub controls: gtk::Box,
    pub mode: gtk::Entry,
    pub saved: gtk::DropDown,
    pub saved_profiles: RefCell<Vec<Profile>>,
    pub preset_keys: Vec<&'static str>,
    pub preset: gtk::DropDown,
    pub correction_label: gtk::Label,
    pub choose_correction: gtk::Button,
    pub method: gtk::DropDown,
    pub instrument: gtk::Entry,
    pub values: HashMap<&'static str, gtk::SpinButton>,
    pub pattern_buttons: Vec<gtk::Button>,
    pub report_button: gtk::Button,
    pub reset_button: gtk::Button,
    pub apply_button: gtk::Button,
    pub outcome: gtk::Label,
    pub reset_dialog: RefCell<Option<gtk::AlertDialog>>,
    pub timer: RefCell<Option<glib::SourceId>>,
    pub backend: RefCell<Arc<dyn Backend>>,
    _theme: tessel_gtk::theme::WindowTheme,
}

#[derive(Clone)]
pub struct Window(pub Rc<Inner>);

impl std::ops::Deref for Window {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

fn label(text: &str) -> gtk::Label {
    gtk::Label::builder().label(tr!(text)).wrap(true).xalign(0.0).build()
}

fn detail(text: &str) -> gtk::Label {
    let widget = label(text);
    widget.add_css_class("hdr-detail");
    widget
}

fn section(parent: &gtk::Box, title: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("settings-card");
    card.add_css_class("hdr-section");
    let heading = gtk::Label::builder().label(tr!(title)).xalign(0.0).build();
    heading.add_css_class("hdr-section-title");
    card.append(&heading);
    parent.append(&card);
    card
}

fn string_list(items: &[String]) -> gtk::StringList {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    gtk::StringList::new(&refs)
}

/// The pattern helper next to the executable (`hdr-pattern`, else `build/hdr-pattern`);
/// `HDR_CALIBRATION_PATTERN` overrides it (Rust-only, for development runs).
pub fn pattern_helper() -> PathBuf {
    if let Some(path) = std::env::var_os("HDR_CALIBRATION_PATTERN") {
        return PathBuf::from(path);
    }
    tessel_core::paths::program("hdr-pattern")
}

impl Window {
    pub fn new(app: &gtk::Application, requested: Option<String>, backend: Arc<dyn Backend>) -> Self {
        let window = gtk::ApplicationWindow::builder().application(app).title(tr!("HDR Calibration"))
            .default_width(760).default_height(840).build();
        let header = gtk::HeaderBar::new();
        window.set_titlebar(Some(&header));
        let theme = way_displays::style::style_window(&window, false);
        window.add_css_class("tessel-hdr-calibration");
        let css = gtk::CssProvider::new();
        css.load_from_string(STYLE);
        // Above the palette stylesheet (APPLICATION + 30) so the chrome rules win.
        gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 31);
        let details = tessel_gtk::app_menu::AboutDetails { name: "HDR Calibration".into(), icon: "preferences-desktop-display".into(),
            comments: Some("Set HDR brightness limits and SDR white for each monitor".into()), ..Default::default() };
        let menu_button = tessel_gtk::app_menu::install_app_menu(app, &window, tessel_gtk::app_menu::MenuTarget::New, details,
                                                                Vec::new(), Some(&header), None);
        let pages = gtk::Stack::builder().vexpand(true).build();
        window.set_child(Some(&pages));
        // Empty state.
        let empty = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).valign(gtk::Align::Center)
            .halign(gtk::Align::Center).build();
        empty.add_css_class("hdr-empty");
        let icon = gtk::Image::from_gicon(&gio::ThemedIcon::from_names(&["monitor-symbolic", "video-display-symbolic",
                                                                         "preferences-desktop-display-symbolic"]));
        icon.set_pixel_size(64);
        icon.add_css_class("hdr-empty-icon");
        empty.append(&icon);
        let title = gtk::Label::builder().label(tr!("HDR calibration is unavailable")).wrap(true).justify(gtk::Justification::Center).build();
        title.add_css_class("hdr-empty-title");
        empty.append(&title);
        let empty_detail = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).max_width_chars(48).build();
        empty_detail.add_css_class("hdr-detail");
        empty.append(&empty_detail);
        pages.add_named(&empty, Some("empty"));
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        pages.add_named(&scroll, Some("main"));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(16).margin_top(24).margin_bottom(24)
            .margin_start(24).margin_end(24).build();
        scroll.set_child(Some(&content));
        let heading = gtk::Label::builder().label(tr!("HDR Calibration")).xalign(0.0).build();
        heading.add_css_class("page-title");
        content.append(&heading);
        content.append(&detail("Set up one monitor and picture mode at a time. Keep the monitor’s HDR picture mode and your room lighting the same as when you normally use it."));
        let monitor = section(&content, "Monitor");
        let outputs = gtk::DropDown::from_strings(&[]);
        monitor.append(&outputs);
        let capability = detail("");
        monitor.append(&capability);
        let controls = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.append(&controls);
        let mode_section = section(&controls, "Monitor picture mode");
        let mode = gtk::Entry::builder().text("Current HDR mode").placeholder_text(tr!("For example: HDR True Black")).build();
        mode_section.append(&mode);
        let saved = gtk::DropDown::from_strings(&[&tr!("New setup")]);
        mode_section.append(&saved);
        let usage = section(&controls, "What you use this monitor for (HDR stays on)");
        let preset_keys: Vec<&'static str> = PRESETS.iter().map(|(k, _, _)| *k).collect();
        let preset_labels: Vec<String> = PRESETS.iter().map(|(_, l, _)| tr!(*l)).collect();
        let preset = gtk::DropDown::new(Some(string_list(&preset_labels)), None::<gtk::Expression>);
        usage.append(&preset);
        usage.append(&detail("Video: use an SDR Rec.709 preview in your editor. Photo: use its color-managed SDR preview. These presets set white brightness; the editor still controls its project and export color space."));
        let correction_section = section(&controls, "HDR output correction");
        let correction_label = label("HDR output correction: none");
        correction_section.append(&correction_label);
        let correction_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let choose_correction = gtk::Button::with_label(&tr!("Import HDR correction…"));
        correction_row.append(&choose_correction);
        let clear = gtk::Button::with_label(&tr!("Clear correction"));
        correction_row.append(&clear);
        correction_section.append(&correction_row);
        correction_section.append(&detail("A correction must have been prepared for this monitor in its selected HDR picture mode. Importing it does not verify physical accuracy."));
        let reference = section(&controls, "Reference patterns");
        let reference_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let mut reference_buttons = Vec::new();
        for (field, text) in [("sdr-srgb", "sRGB reference pattern"), ("sdr-709", "Rec.709 reference pattern")] {
            let button = gtk::Button::with_label(&tr!(text));
            reference_row.append(&button);
            reference_buttons.push((field, button));
        }
        reference.append(&reference_row);
        reference.append(&detail("Reference patterns show gray ramps and color patches so you can check ordinary (SDR) content by eye. They are not a measurement."));
        let limits = section(&controls, "Brightness limits");
        let method = gtk::DropDown::new(Some(string_list(&[tr!("Visual estimates"), tr!("Measured luminance — manual readings")])), None::<gtk::Expression>);
        limits.append(&method);
        let instrument = gtk::Entry::builder().placeholder_text(tr!("Instrument model and measurement notes")).visible(false).build();
        limits.append(&instrument);
        let mut values = HashMap::new();
        let mut pattern_buttons = Vec::new();
        for (key, text, lower, upper, step, digits, help) in FIELD_SPECS {
            let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(6).build();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let name = gtk::Label::builder().label(tr!(text)).xalign(0.0).hexpand(true).build();
            name.add_css_class("hdr-field-title");
            row.append(&name);
            let spin = gtk::SpinButton::with_range(lower, upper, step);
            spin.set_digits(digits);
            spin.set_numeric(true);
            spin.set_tooltip_text(Some(&tr!("Nits (cd/m²)")));
            row.append(&spin);
            row.append(&gtk::Label::new(Some(&tr!("nits"))));
            let button = gtk::Button::with_label(&tr!("Show pattern"));
            row.append(&button);
            pattern_buttons.push(button);
            card.append(&row);
            card.append(&detail(help));
            limits.append(&card);
            values.insert(key, spin);
        }
        limits.append(&gtk::Separator::builder().margin_top(6).build());
        limits.append(&detail("The limits are shared with HDR applications; SDR white changes desktop brightness. Visual setup does not correct color gamut, white balance, or the monitor’s HDR response. Color-accurate editing still requires measurements."));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let report_button = gtk::Button::builder().label(tr!("Save a calibration summary")).hexpand(true).halign(gtk::Align::Start).build();
        actions.append(&report_button);
        let reset_button = gtk::Button::with_label(&tr!("Restore display defaults"));
        actions.append(&reset_button);
        let apply_button = gtk::Button::with_label(&tr!("Save and apply"));
        apply_button.add_css_class("primary");
        actions.append(&apply_button);
        controls.append(&actions);
        let outcome = label("");
        outcome.set_selectable(true);
        content.append(&outcome);

        let inner = Rc::new(Inner {
            window, requested, names: RefCell::new(Vec::new()), monitor_identity: RefCell::new(None), worker: Cell::new(false),
            child: RefCell::new(None), alive: Cell::new(true), loading: Cell::new(false), correction: RefCell::new(None),
            correction_unresolved: Cell::new(false), chooser: RefCell::new(None), menu_button, pages, outputs, capability,
            empty_detail, controls, mode, saved, saved_profiles: RefCell::new(Vec::new()), preset_keys, preset,
            correction_label, choose_correction, method, instrument, values, pattern_buttons, report_button, reset_button,
            apply_button, outcome, reset_dialog: RefCell::new(None), timer: RefCell::new(None), backend: RefCell::new(backend),
            _theme: theme,
        });
        let this = Window(inner);
        this.connect(&clear, reference_buttons);
        this.refresh();
        let weak = this.weak();
        let timer = glib::timeout_add_seconds_local(2, move || match weak.upgrade() {
            Some(inner) if Window(inner.clone()).refresh() => glib::ControlFlow::Continue,
            _ => glib::ControlFlow::Break,
        });
        this.timer.replace(Some(timer));
        this
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn with(weak: &Weak<Inner>, f: impl FnOnce(&Window)) {
        if let Some(inner) = weak.upgrade() {
            f(&Window(inner));
        }
    }

    fn connect(&self, clear: &gtk::Button, reference_buttons: Vec<(&'static str, gtk::Button)>) {
        let w = self.weak();
        self.outputs.connect_selected_notify(move |_| Self::with(&w, |m| m.selected()));
        let w = self.weak();
        self.saved.connect_selected_notify(move |_| Self::with(&w, |m| m.load_saved()));
        let w = self.weak();
        self.preset.connect_selected_notify(move |_| Self::with(&w, |m| m.preset_changed()));
        let w = self.weak();
        self.choose_correction.connect_clicked(move |_| Self::with(&w, |m| m.import_correction()));
        let w = self.weak();
        clear.connect_clicked(move |_| Self::with(&w, |m| m.set_correction(None)));
        for (field, button) in reference_buttons {
            let w = self.weak();
            button.connect_clicked(move |_| Self::with(&w, |m| m.pattern(field)));
        }
        let w = self.weak();
        self.method.connect_selected_notify(move |_| Self::with(&w, |m| m.method_changed()));
        let w = self.weak();
        self.values["white"].connect_value_changed(move |_| Self::with(&w, |m| m.white_changed()));
        for ((key, ..), button) in FIELD_SPECS.iter().zip(&self.pattern_buttons) {
            let (w, key) = (self.weak(), *key);
            button.connect_clicked(move |_| Self::with(&w, |m| m.pattern(key)));
        }
        let w = self.weak();
        self.report_button.connect_clicked(move |_| Self::with(&w, |m| m.report()));
        let w = self.weak();
        self.reset_button.connect_clicked(move |_| Self::with(&w, |m| m.confirm_reset()));
        let w = self.weak();
        self.apply_button.connect_clicked(move |_| Self::with(&w, |m| m.apply(false)));
        let w = self.weak();
        // The window owns the state for its lifetime.
        let strong = RefCell::new(Some(self.0.clone()));
        self.window.connect_close_request(move |_| {
            let stop = w.upgrade().map(|inner| Window(inner).close_requested()).unwrap_or(false);
            if stop { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        self.window.connect_destroy(move |_| { strong.borrow_mut().take(); });
    }

    fn backend(&self) -> Arc<dyn Backend> {
        self.backend.borrow().clone()
    }

    fn show_unavailable(&self, reason: &str) {
        self.capability.set_text(reason);
        self.empty_detail.set_text(reason);
        self.pages.set_visible_child_name("empty");
    }

    pub fn connector(&self) -> Option<String> {
        let index = self.outputs.selected() as usize;
        self.names.borrow().get(index).cloned()
    }

    pub fn refresh(&self) -> bool {
        if !self.alive.get() {
            return false;
        }
        if let Err(error) = self.try_refresh() {
            self.show_unavailable(&error.to_string());
            self.controls.set_sensitive(false);
            if let Some(child) = self.child.borrow_mut().as_mut() {
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            }
        }
        true
    }

    fn try_refresh(&self) -> Result<(), CalError> {
        let status = self.backend().session()?;
        let names: Vec<String> = status.iter().filter(|(k, v)| *k != "compositor" && v.get("enabled").map(String::as_str) == Some("true"))
            .map(|(k, _)| k.clone()).collect();
        if names != *self.names.borrow() {
            let selected = self.connector().or_else(|| self.requested.clone());
            self.names.replace(names.clone());
            self.outputs.set_model(Some(&string_list(&names)));
            let index = selected.and_then(|s| names.iter().position(|n| *n == s)).unwrap_or(0);
            self.outputs.set_selected(index as u32);
            self.selected();
        }
        let connector = self.connector().unwrap_or_default();
        let monitor = calibration::check_monitor(&status, &connector, None)?;
        let identity = self.monitor_identity.borrow().clone();
        if identity.is_some_and(|i| !i.is_empty() && Some(&i) != monitor.get("display_id")) {
            self.selected();
        }
        let idle = !self.worker.get() && self.child.borrow().is_none();
        self.controls.set_sensitive(idle);
        self.outputs.set_sensitive(idle);
        self.choose_correction.set_sensitive(status.get("compositor").and_then(|c| c.get("hdr_correction_supported"))
            .map(String::as_str) == Some("true"));
        self.pages.set_visible_child_name("main");
        let active = monitor.get("hdr_calibration_active").map(String::as_str) == Some("true");
        let white = calibration::py_float_text(monitor.get("hdr_reference_white_nits").map(String::as_str).unwrap_or("0"))
            .ok_or_else(|| CalError::Value(format!("could not convert string to float: '{}'",
                                                  monitor.get("hdr_reference_white_nits").cloned().unwrap_or_default())))?;
        let display = monitor.get("description").cloned().unwrap_or_else(|| connector.clone());
        let state = if active { tr!("Your saved HDR setup is in use") } else { tr!("Using the monitor’s default HDR settings") };
        self.capability.set_text(&tr!("{display}\nHDR is on · SDR white is {white:g} nits · {state}", display = display, white = white, state = state));
        Ok(())
    }

    pub fn selected(&self) {
        if self.loading.get() {
            return;
        }
        self.loading.set(true);
        if let Err(error) = self.try_selected() {
            self.outcome.set_text(&error.to_string());
        }
        self.loading.set(false);
    }

    fn try_selected(&self) -> Result<(), CalError> {
        let status = self.backend().read_status();
        let connector = self.connector();
        let monitor: Section = connector.as_ref().and_then(|c| status.get(c)).cloned().unwrap_or_default();
        self.monitor_identity.replace(monitor.get("display_id").cloned());
        let positive = |key: &str, fallback: f64| match monitor.get(key) {
            None => fallback,
            Some(text) => calibration::py_float_text(text).filter(|v| *v > 0.0 && *v <= 10000.0).unwrap_or(fallback),
        };
        let peak = positive("hdr_target_peak_nits", positive("hdr_peak_nits", 400.0));
        let full = peak.min(positive("hdr_target_full_frame_nits", positive("hdr_max_frame_average_nits", 203.0)));
        let black_text = monitor.get("hdr_target_black_nits").cloned().unwrap_or_else(|| "0".into());
        let black = calibration::py_float_text(&black_text)
            .ok_or_else(|| CalError::Value(format!("could not convert string to float: '{black_text}'")))?;
        let defaults = [("black", black), ("peak", peak), ("fullFrame", full), ("white", full.min(positive("hdr_reference_white_nits", 203.0)))];
        for (key, value) in defaults {
            self.values[key].set_value(value);
        }
        self.preset.set_selected(0);
        self.set_correction(None);
        self.mode.set_text("Current HDR mode");
        self.method.set_selected(0);
        self.instrument.set_text("");
        let identity = self.monitor_identity.borrow().clone();
        let profiles: Vec<Profile> = self.backend().read_profiles()?.into_iter().filter(|p| {
            p.get("displayId").and_then(Value::as_str) == identity.as_deref() && p.get("connector").and_then(Value::as_str) == connector.as_deref()
        }).collect();
        let mut names = vec![tr!("New setup")];
        names.extend(profiles.iter().map(|p| format!("{} — {}", p.get("mode").and_then(Value::as_str).unwrap_or(""),
            tr!(calibration::preset_label(p.get("preset").and_then(Value::as_str).unwrap_or("custom"))))));
        self.saved_profiles.replace(profiles.clone());
        self.saved.set_model(Some(&string_list(&names)));
        self.saved.set_selected(0);
        let mut active_index = 0;
        for (index, profile) in profiles.iter().enumerate() {
            if calibration::matches(&monitor, Some(profile))? {
                active_index = index + 1;
                break;
            }
        }
        if active_index != 0 {
            self.loading.set(false);
            self.saved.set_selected(active_index as u32);
        } else if monitor.get("hdr_correction_path").is_some_and(|p| !p.is_empty()) {
            self.correction_unresolved.set(true);
            self.correction_label.set_text(&tr!("Existing HDR correction: load its saved profile, import a replacement, or explicitly clear it before applying."));
        }
        Ok(())
    }

    pub fn load_saved(&self) {
        let index = self.saved.selected() as usize;
        let profiles = self.saved_profiles.borrow().clone();
        if self.loading.get() || index == 0 || index > profiles.len() {
            return;
        }
        let profile = &profiles[index - 1];
        self.loading.set(true);
        let preset = profile.get("preset").and_then(Value::as_str).unwrap_or("custom");
        self.preset.set_selected(self.preset_keys.iter().position(|k| *k == preset).unwrap_or(0) as u32);
        self.set_correction(profile.get("correction").filter(|c| !c.is_null()).cloned());
        let mode = profile.get("mode").and_then(Value::as_str).unwrap_or("").to_owned();
        self.mode.set_text(&mode);
        self.method.set_selected((profile.get("method").and_then(Value::as_str) == Some("meter-readings")) as u32);
        self.instrument.set_text(profile.get("instrument").and_then(Value::as_str).unwrap_or(""));
        for key in FIELDS {
            self.values[key].set_value(profile.get(key).and_then(Value::as_f64).unwrap_or(0.0));
        }
        self.loading.set(false);
        self.outcome.set_text(&tr!("Loaded {mode}. Select that picture mode in the monitor’s own menu before applying.", mode = mode));
    }

    pub fn preset_changed(&self) {
        if self.loading.get() {
            return;
        }
        let key = self.preset_keys.get(self.preset.selected() as usize).copied().unwrap_or("custom");
        if let Some(Some(white)) = calibration::preset_white(key) {
            self.loading.set(true);
            self.values["white"].set_value(white);
            self.loading.set(false);
        }
        self.outcome.set_text(&tr!("Preset selected. Press Save and apply to change this monitor’s SDR brightness."));
    }

    fn white_changed(&self) {
        if !self.loading.get() {
            self.preset.set_selected(0);
        }
    }

    pub fn set_correction(&self, correction: Option<Value>) {
        self.correction_unresolved.set(false);
        let text = match correction.as_ref().and_then(|c| c.get("path")).and_then(Value::as_str) {
            Some(path) => tr!("HDR output correction: {name}",
                              name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            None => tr!("HDR output correction: none"),
        };
        self.correction.replace(correction);
        self.correction_label.set_text(&text);
    }

    #[allow(deprecated)]
    fn import_correction(&self) {
        let chooser = gtk::FileChooserNative::new(Some(&tr!("Import monitor HDR correction")), Some(&self.window),
            gtk::FileChooserAction::Open, Some(&tr!("Import")), Some(&tr!("Cancel")));
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&tr!("HDR output correction (.cube)")));
        filter.add_pattern("*.cube");
        chooser.add_filter(&filter);
        let w = self.weak();
        chooser.connect_response(move |dialog, response| {
            Self::with(&w, |m| {
                if response == gtk::ResponseType::Accept {
                    if let Some(path) = dialog.file().and_then(|f| f.path()) {
                        let identity = m.monitor_identity.borrow().clone().unwrap_or_default();
                        match correction::import_cube(&path, &identity, m.mode.text().trim()) {
                            Ok(result) => {
                                m.set_correction(Some(Value::Object(result)));
                                m.outcome.set_text(&tr!("Correction imported. Press Save and apply to use it. It has not been checked with a measuring device."));
                            }
                            Err(error) => m.outcome.set_text(&error.to_string()),
                        }
                    }
                }
                m.chooser.replace(None);
            });
            dialog.destroy();
        });
        self.chooser.replace(Some(chooser.clone()));
        chooser.show();
    }

    fn report(&self) {
        let result = self.profile().and_then(|profile| self.backend().workspace_report(&self.connector().unwrap_or_default(), &profile));
        match result {
            Ok(path) => self.outcome.set_text(&tr!("Calibration summary saved:\n{path}", path = path.display().to_string())),
            Err(error) => self.outcome.set_text(&error.to_string()),
        }
    }

    fn method_changed(&self) {
        let measured = self.method.selected() == 1;
        self.instrument.set_visible(measured);
        for button in &self.pattern_buttons {
            button.set_sensitive(!measured);
        }
    }

    pub fn profile(&self) -> Result<Profile, CalError> {
        if self.correction_unresolved.get() {
            return Err(CalError::Value(tr!("Load the existing correction’s saved profile, import a replacement, or clear it before applying.")));
        }
        for spin in self.values.values() {
            spin.update();
        }
        let mut profile = json!({
            "version": 1, "connector": self.connector(), "displayId": self.monitor_identity.borrow().clone(),
            "mode": self.mode.text().trim(), "method": if self.method.selected() == 1 { "meter-readings" } else { "visual" },
            "instrument": self.instrument.text().trim(),
            "preset": self.preset_keys.get(self.preset.selected() as usize).copied().unwrap_or("custom"),
            "correction": self.correction.borrow().clone(),
        }).as_object().cloned().unwrap();
        for key in FIELDS {
            profile.insert(key.into(), json!(self.values[key].value()));
        }
        calibration::validate(&profile)
    }

    pub fn pattern(&self, field: &'static str) {
        if let Err(error) = self.try_pattern(field) {
            self.outcome.set_text(&error);
        }
    }

    fn try_pattern(&self, field: &'static str) -> Result<(), String> {
        let connector = self.connector().unwrap_or_default();
        let identity = self.monitor_identity.borrow().clone();
        calibration::check_monitor(&self.backend().session().map_err(|e| e.to_string())?, &connector, identity.as_deref())
            .map_err(|e| e.to_string())?;
        let source = if field.starts_with("sdr-") { "white" } else { field };
        self.values[source].update();
        let helper = pattern_helper();
        let child = std::process::Command::new(&helper).args([connector.as_str(), field, &tessel_core::pyfmt::float_repr(self.values[source].value())])
            .stdin(std::process::Stdio::inherit()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().map_err(|e| tessel_core::pyerr::os_error_path(&e, &helper))?;
        self.child.replace(Some(child));
        self.outcome.set_text(&if field.starts_with("sdr-") {
            tr!("SDR reference open at the applied white target. H hides instructions; Enter closes and Escape cancels.")
        } else {
            tr!("Pattern open. Use arrows or the wheel to adjust; Enter accepts and Escape cancels.")
        });
        self.refresh();
        let w = self.weak();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let Some(inner) = w.upgrade() else { return glib::ControlFlow::Break };
            if Window(inner).pattern_finished(field) { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
        });
        Ok(())
    }

    fn pattern_finished(&self, field: &str) -> bool {
        use std::io::Read;
        let mut slot = self.child.borrow_mut();
        let Some(child) = slot.as_mut() else { return false };
        match child.try_wait() {
            Ok(None) => return true,
            Ok(Some(_)) | Err(_) => {}
        }
        let mut child = slot.take().unwrap();
        drop(slot);
        let (mut output, mut error) = (String::new(), String::new());
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut output);
        }
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut error);
        }
        let code = child.wait().ok().and_then(|s| s.code());
        if !self.alive.get() {
            return false;
        }
        if code == Some(0) {
            let result = (|| -> Result<Option<f64>, String> {
                let data: Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
                let expected = match field { "sdr-srgb" => "SRGB-10", "sdr-709" => "BT709-BT1886-10", _ => "BT2020-PQ-10" };
                if data.get("encoding").and_then(Value::as_str) != Some(expected) {
                    return Err("Unexpected pattern encoding.".into());
                }
                let identity = self.monitor_identity.borrow().clone();
                calibration::check_monitor(&self.backend().session().map_err(|e| e.to_string())?,
                                           &self.connector().unwrap_or_default(), identity.as_deref()).map_err(|e| e.to_string())?;
                if field.starts_with("sdr-") {
                    return Ok(None);
                }
                data.get("nits").and_then(Value::as_f64).map(Some).ok_or_else(|| "'nits'".to_owned())
            })();
            match result {
                Ok(None) => {
                    self.outcome.set_text(&tr!("Reference pattern closed."));
                    self.refresh();
                    return false;
                }
                Ok(Some(nits)) => {
                    self.values[field].set_value(nits);
                    self.outcome.set_text(&tr!("Pattern result recorded as a visual estimate. Save and apply when all values are ready."));
                }
                Err(failure) => self.outcome.set_text(&failure),
            }
        } else {
            let trimmed = error.trim();
            self.outcome.set_text(&if trimmed.is_empty() { tr!("Pattern cancelled.") } else { trimmed.to_owned() });
        }
        self.refresh();
        false
    }

    /// Ask before discarding this monitor's saved HDR setup.
    pub fn confirm_reset(&self) {
        if self.worker.get() || self.child.borrow().is_some() {
            return;
        }
        let dialog = gtk::AlertDialog::builder().message(tr!("Restore display defaults?"))
            .detail(tr!("This monitor goes back to its own HDR brightness settings. Your saved setups are kept and can be loaded again."))
            .buttons([tr!("Cancel"), tr!("Restore defaults")]).modal(true).cancel_button(0).default_button(0).build();
        let w = self.weak();
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            if result == Ok(1) {
                Self::with(&w, |m| m.apply(true));
            }
        });
        self.reset_dialog.replace(Some(dialog));
    }

    pub fn apply(&self, reset: bool) {
        if self.worker.get() || self.child.borrow().is_some() {
            return;
        }
        let profile = if reset { None } else {
            match self.profile() {
                Ok(profile) => Some(profile),
                Err(error) => {
                    self.outcome.set_text(&error.to_string());
                    return;
                }
            }
        };
        let connector = self.connector().unwrap_or_default();
        self.outcome.set_text(&tr!("Applying HDR setup…"));
        let backend = self.backend();
        let (applied, restored, summary_failed) = (tr!("HDR setup applied and saved for {mode}.",
            mode = profile.as_ref().and_then(|p| p.get("mode")).and_then(Value::as_str).unwrap_or("")),
            tr!("Display defaults restored."), tr!("Settings applied, but the calibration summary could not be saved: "));
        let work = move || -> String {
            match backend.apply(&connector, profile.as_ref()) {
                Ok(_backup) => {
                    let mut message = if reset { restored } else { applied };
                    if let Some(profile) = &profile {
                        match backend.workspace_report(&connector, profile) {
                            Ok(path) => message += &format!("\n{}", path.display()),
                            Err(error) => message += &format!("\n{summary_failed}{error}"),
                        }
                    }
                    message
                }
                Err(error) => error.to_string(),
            }
        };
        self.worker.set(true);
        let w = self.weak();
        glib::MainContext::default().spawn_local(async move {
            let message = gio::spawn_blocking(work).await.unwrap_or_else(|_| "The HDR configuration update failed.".into());
            Self::with(&w, |m| m.applied(&message));
        });
        self.refresh();
    }

    fn applied(&self, message: &str) {
        self.worker.set(false);
        if self.alive.get() {
            self.outcome.set_text(message);
            self.refresh();
        }
    }

    /// `close-request`: true keeps the window open (an apply is still running).
    #[allow(deprecated)]
    pub fn close_requested(&self) -> bool {
        if self.worker.get() {
            self.outcome.set_text(&tr!("Finishing the HDR configuration update…"));
            return true;
        }
        self.alive.set(false);
        if let Some(chooser) = self.chooser.take() {
            chooser.destroy();
        }
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        if let Some(mut child) = self.child.take() {
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let _ = child.try_wait();
        }
        false
    }

    pub fn close(&self) {
        self.window.close();
    }
}
