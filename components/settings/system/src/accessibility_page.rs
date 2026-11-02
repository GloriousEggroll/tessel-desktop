//! accessibility_page.py: text size, contrast, motion, pointer size and the screen reader.
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gs_base::widgets::{dropdown, label, status_text, Page};
use gs_base::SettingsPage;
use gtk::prelude::*;
use gtk::{gio, glib};
use tessel_core::tr;

use crate::about_page::{default_runner, Runner};
use crate::compat::Result;
use crate::labwc_settings;

pub const INTERFACE: &str = "org.gnome.desktop.interface";
pub const A11Y_INTERFACE: &str = "org.gnome.desktop.a11y.interface";
pub const A11Y_APPLICATIONS: &str = "org.gnome.desktop.a11y.applications";
pub const CURSOR_SIZES: &[i32] = &[24, 32, 48, 64, 96];
pub const LARGE_CURSOR: i32 = 48;
pub const TEXT_SCALES: &[f64] = &[1.0, 1.1, 1.25, 1.5, 1.75, 2.0];
pub const ORCA_UNIT: &str = "orca-screen-reader";

/// `settings(schema_id)`: None when the schema is not installed.
pub fn settings(schema_id: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(schema_id, true)?;
    Some(gio::Settings::new(schema_id))
}

pub struct AccessibilityBackend {
    pub interface: Option<gio::Settings>,
    pub a11y: Option<gio::Settings>,
    pub applications: Option<gio::Settings>,
    pub environment: Option<PathBuf>,
    pub runner: Runner,
    /// `orca_available()` override (tests).
    pub orca: Option<bool>,
}

impl AccessibilityBackend {
    pub fn new(environment: Option<PathBuf>, runner: Option<Runner>) -> Self {
        AccessibilityBackend { interface: settings(INTERFACE), a11y: settings(A11Y_INTERFACE), applications: settings(A11Y_APPLICATIONS),
                               environment, runner: runner.unwrap_or_else(default_runner), orca: None }
    }

    pub fn orca_available(&self) -> bool { self.orca.unwrap_or_else(|| glib::find_program_in_path("orca").is_some()) }

    pub fn set_cursor_size(&self, size: i32) -> Result<bool> {
        if !CURSOR_SIZES.contains(&size) {
            return crate::compat::value("Unsupported cursor size.");
        }
        if let Some(interface) = &self.interface {
            let _ = interface.set_int("cursor-size", size);
        }
        // labwc and X11 clients read the size from the session environment.
        labwc_settings::update_environment(&labwc_settings::env_values(&[("XCURSOR_SIZE", Some(&size.to_string()))]), self.environment.as_deref())
    }

    pub fn set_screen_reader(&self, enabled: bool, done: Option<Box<dyn FnOnce(bool, String)>>) {
        if let Some(applications) = &self.applications {
            let _ = applications.set_boolean("screen-reader-enabled", enabled);
        }
        let command: Vec<String> = if enabled {
            ["systemd-run", "--user", "--quiet", "--collect", &format!("--unit={ORCA_UNIT}"), "orca", "--replace"].iter().map(|s| s.to_string()).collect()
        } else {
            vec!["systemctl".into(), "--user".into(), "stop".into(), format!("{ORCA_UNIT}.service")]
        };
        (self.runner)(command, done.unwrap_or_else(|| Box::new(|_, _| {})));
    }
}

pub struct AccessibilityPage {
    pub page: Page,
    pub backend: AccessibilityBackend,
    loading: Cell<bool>,
    pub text_scales: Vec<f64>,
    pub text_size: gtk::DropDown,
    pub contrast: gtk::Switch,
    pub motion: gtk::Switch,
    pub cursor_sizes: Vec<i32>,
    pub cursor_size: gtk::DropDown,
    pub large_cursor: gtk::Switch,
    pub screen_reader: gtk::Switch,
    pub status: gtk::Label,
}

fn closest<T: Copy + Into<f64>>(values: &[T], target: f64) -> i32 {
    // min(range(len)), key=abs distance): the first of equal distances.
    let mut best = 0;
    for (index, value) in values.iter().enumerate() {
        if ((*value).into() - target).abs() < (values[best].into() - target).abs() {
            best = index;
        }
    }
    best as i32
}

impl AccessibilityPage {
    pub fn new(backend: Option<AccessibilityBackend>) -> Rc<AccessibilityPage> {
        let page = Page::new("accessibility-page", &tr!("Accessibility"), &tr!("Make the desktop easier to see, read and use."),
                             Some("preferences-desktop-accessibility-symbolic"));
        let backend = backend.unwrap_or_else(|| AccessibilityBackend::new(None, None));
        let (interface, a11y, applications) = (backend.interface.clone(), backend.a11y.clone(), backend.applications.clone());
        let seeing = page.group(Some(&tr!("Seeing")), None, None);
        let scale = interface.as_ref().map(|i| i.double("text-scaling-factor")).unwrap_or(1.0);
        let mut text_scales = TEXT_SCALES.to_vec();
        if !text_scales.iter().any(|v| (v - scale).abs() < 0.001) {
            text_scales.push((scale * 100.0).round_ties_even() / 100.0);
            text_scales.sort_by(|a, b| a.partial_cmp(b).unwrap());
        }
        let names: Vec<String> = text_scales.iter().map(|v| tr!("{value1}%", value1 = (v * 100.0).round_ties_even() as i64)).collect();
        let text_size = dropdown(&names, closest(&text_scales, scale));
        text_size.set_sensitive(interface.is_some());
        Page::row(&seeing, &tr!("Text size"), Some(text_size.upcast_ref()), Some(&tr!("Scale text in applications.")));
        let contrast = gtk::Switch::builder().active(a11y.as_ref().is_some_and(|a| a.boolean("high-contrast"))).sensitive(a11y.is_some()).build();
        Page::row(&seeing, &tr!("High contrast"), Some(contrast.upcast_ref()), Some(&tr!("Increase the contrast of controls and text.")));
        let motion = gtk::Switch::builder().active(interface.as_ref().is_some_and(|i| !i.boolean("enable-animations")))
            .sensitive(interface.is_some()).build();
        Page::row(&seeing, &tr!("Reduce motion"), Some(motion.upcast_ref()), Some(&tr!("Turn off animations in applications and the shell.")));
        let pointer = page.group(Some(&tr!("Pointer")), None, None);
        let size = interface.as_ref().map(|i| i.int("cursor-size")).unwrap_or(24);
        let cursor_sizes = CURSOR_SIZES.to_vec();
        let cursor_size = dropdown(&cursor_sizes.iter().map(|v| tr!("{value1} pixels", value1 = *v as i64)).collect::<Vec<_>>(),
                                   closest(&cursor_sizes, size as f64));
        Page::row(&pointer, &tr!("Cursor size"), Some(cursor_size.upcast_ref()), None);
        let large_cursor = gtk::Switch::builder().active(size >= LARGE_CURSOR).build();
        Page::row(&pointer, &tr!("Large cursor"), Some(large_cursor.upcast_ref()), Some(&tr!("Use a pointer that is easier to find.")));
        let reader = page.group(Some(&tr!("Screen reader")), None, None);
        let available = backend.orca_available();
        let screen_reader = gtk::Switch::builder().active(applications.as_ref().is_some_and(|a| a.boolean("screen-reader-enabled")))
            .sensitive(available && applications.is_some()).build();
        let detail = if available { tr!("Read the screen aloud with Orca.") } else { tr!("Install Orca to use a screen reader.") };
        Page::row(&reader, &tr!("Screen reader"), Some(screen_reader.upcast_ref()), Some(&detail));
        let status = label("", Some("detail"));
        page.content.append(&status);
        let this = Rc::new(AccessibilityPage { page, backend, loading: Cell::new(true), text_scales, text_size, contrast, motion, cursor_sizes,
                                               cursor_size, large_cursor, screen_reader, status });
        let weak = Rc::downgrade(&this);
        this.text_size.connect_selected_notify(move |_| if let Some(t) = weak.upgrade() {
            t.apply(|t| {
                let value = t.text_scales[t.text_size.selected() as usize];
                t.backend.interface.as_ref().ok_or_else(|| "'NoneType' object has no attribute 'set_double'".to_string())?
                    .set_double("text-scaling-factor", value).map(|_| ()).or(Ok(()))
            })
        });
        let weak = Rc::downgrade(&this);
        this.contrast.connect_active_notify(move |_| if let Some(t) = weak.upgrade() {
            t.apply(|t| t.backend.a11y.as_ref().ok_or_else(|| "'NoneType' object has no attribute 'set_boolean'".to_string())?
                .set_boolean("high-contrast", t.contrast.is_active()).map(|_| ()).or(Ok(())))
        });
        let weak = Rc::downgrade(&this);
        this.motion.connect_active_notify(move |_| if let Some(t) = weak.upgrade() {
            t.apply(|t| t.backend.interface.as_ref().ok_or_else(|| "'NoneType' object has no attribute 'set_boolean'".to_string())?
                .set_boolean("enable-animations", !t.motion.is_active()).map(|_| ()).or(Ok(())))
        });
        let weak = Rc::downgrade(&this);
        this.cursor_size.connect_selected_notify(move |w| if let Some(t) = weak.upgrade() {
            let size = t.cursor_sizes[w.selected() as usize];
            t.cursor(size)
        });
        let weak = Rc::downgrade(&this);
        this.large_cursor.connect_active_notify(move |w| if let Some(t) = weak.upgrade() {
            t.cursor(if w.is_active() { LARGE_CURSOR } else { CURSOR_SIZES[0] })
        });
        let weak = Rc::downgrade(&this);
        this.screen_reader.connect_active_notify(move |_| if let Some(t) = weak.upgrade() { t.apply(|t| { t.screen_reader_changed(); Ok(()) }) });
        this.loading.set(false);
        this
    }

    fn apply(self: &Rc<Self>, change: impl FnOnce(&Rc<Self>) -> std::result::Result<(), String>) {
        if self.loading.get() {
            return;
        }
        match change(self) {
            Ok(()) => status_text(&self.status, &tr!("Changes apply immediately."), None),
            Err(error) => status_text(&self.status, &tr!("The setting could not be changed: {value1}", value1 = error), Some("error")),
        }
    }

    fn cursor(&self, size: i32) {
        if self.loading.get() {
            return;
        }
        self.loading.set(true);
        if let Some(index) = self.cursor_sizes.iter().position(|s| *s == size) {
            self.cursor_size.set_selected(index as u32);
        }
        self.large_cursor.set_active(size >= LARGE_CURSOR);
        self.loading.set(false);
        match self.backend.set_cursor_size(size) {
            Ok(_) => status_text(&self.status, &tr!("Applications use the new size now; the desktop uses it after you sign out and back in."), None),
            Err(error) => status_text(&self.status, &tr!("The setting could not be changed: {value1}", value1 = error.to_string()), Some("error")),
        }
    }

    fn screen_reader_changed(self: &Rc<Self>) {
        let active = self.screen_reader.is_active();
        let weak = Rc::downgrade(self);
        let done: Option<Box<dyn FnOnce(bool, String)>> = active.then(|| Box::new(move |okay: bool, output: String| {
            let Some(t) = weak.upgrade() else { return };
            if !okay && !t.page.closed() {
                let detail = if crate::compat::strip(&output).is_empty() { tr!("Unknown error") } else { crate::compat::strip(&output).to_owned() };
                status_text(&t.status, &tr!("The screen reader could not be started: {value1}", value1 = detail), Some("error"));
            }
        }) as Box<dyn FnOnce(bool, String)>);
        self.backend.set_screen_reader(active, done);
    }
}

pub fn build_page_with(backend: Option<AccessibilityBackend>) -> SettingsPage {
    let controller = AccessibilityPage::new(backend);
    let weak = Rc::downgrade(&controller);
    let page = SettingsPage::new(controller.page.widget.clone()).with_close(move || if let Some(c) = weak.upgrade() { c.page.close() });
    page.add_target("a11y-text", &controller.text_size);
    page.add_target("a11y-contrast", &controller.contrast);
    page.add_target("a11y-motion", &controller.motion);
    page.add_target("a11y-cursor-size", &controller.cursor_size);
    page.add_target("a11y-large-cursor", &controller.large_cursor);
    page.add_target("a11y-screen-reader", &controller.screen_reader);
    page.set_controller(controller);
    page
}
