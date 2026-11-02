//! night_light_page.py: warmer colors in the evening, applied by tessel-night-light.service.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gs_base::widgets::{dropdown, label, label_with, status_text, Page};
use gs_base::SettingsPage;
use gtk::glib;
use gtk::prelude::*;
use tessel_core::tr;

use crate::about_page::{default_runner, Runner};
use crate::compat::Error;
use crate::night_light::{enabled_outputs, rc_text, split_outputs, NightLightConfig, NightLightStore, MAX_TEMPERATURE, MIN_TEMPERATURE, UNIT};

pub fn time_entry(value: &str) -> gtk::Entry {
    let entry = gtk::Entry::builder().text(value).max_length(5).width_chars(6).input_purpose(gtk::InputPurpose::Digits).build();
    entry.set_placeholder_text(Some("HH:MM"));
    entry
}

pub struct NightLightPage {
    pub page: Page,
    pub store: NightLightStore,
    runner: Runner,
    pub config: RefCell<NightLightConfig>,
    pub available: bool,
    loading: Cell<bool>,
    pub card: gtk::Box,
    pub enabled: gtk::Switch,
    pub schedule: gtk::DropDown,
    pub location_note: gtk::Label,
    pub sunset: gtk::Entry,
    pub sunrise: gtk::Entry,
    pub times_row: gtk::Box,
    pub temperature: gtk::Scale,
    pub hdr_note: gtk::Label,
    pub status: gtk::Label,
    save_source: RefCell<Option<glib::SourceId>>,
}

/// `outputs=(names, rc_text)` test injection; None reads them in the background.
pub type Outputs = Option<(Option<Vec<String>>, String)>;

impl NightLightPage {
    pub fn new(store: Option<NightLightStore>, runner: Option<Runner>, outputs: Outputs, available: Option<bool>) -> Rc<NightLightPage> {
        let page = Page::new("night-light-page", &tr!("Night Light"),
                             &tr!("Reduce blue light in the evening to make the screen easier on your eyes."), Some("weather-clear-night-symbolic"));
        let store = store.unwrap_or_default();
        let config = store.load();
        let available = available.unwrap_or_else(|| glib::find_program_in_path("wlsunset").is_some());
        let card = page.group(None, None, None);
        let enabled = gtk::Switch::builder().active(config.enabled).build();
        Page::row(&card, &tr!("Night light"), Some(enabled.upcast_ref()), None);
        let schedule = dropdown(&[tr!("Sunset to sunrise"), tr!("Manual schedule")], if config.schedule == "sun" { 0 } else { 1 });
        Page::row(&card, &tr!("Schedule"), Some(schedule.upcast_ref()), None);
        let location_note = label("", Some("detail"));
        card.append(&location_note);
        let times = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let sunset = time_entry(&config.sunset);
        let sunrise = time_entry(&config.sunrise);
        for (title, entry) in [(tr!("From"), &sunset), (tr!("To"), &sunrise)] {
            times.append(&label_with(&title, None, false));
            entry.update_property(&[gtk::accessible::Property::Label(&title)]);
            times.append(entry);
        }
        let times_row = Page::row(&card, &tr!("Times"), Some(times.upcast_ref()), None);
        let temperature = gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_TEMPERATURE as f64, MAX_TEMPERATURE as f64, 100.0);
        temperature.set_inverted(true); // left = cooler, right = warmer
        temperature.set_value(config.temperature as f64);
        temperature.set_size_request(280, -1);
        temperature.set_draw_value(true);
        temperature.set_format_value_func(|_, value| tr!("{value1} K", value1 = value as i64));
        Page::row(&card, &tr!("Color temperature"), Some(temperature.upcast_ref()), Some(&tr!("Lower values are warmer.")));
        let hdr_note = label("", Some("warning"));
        hdr_note.set_visible(false);
        page.content.append(&hdr_note);
        let status = label("", Some("detail"));
        page.content.append(&status);
        if !available {
            status_text(&status, &tr!("Install the wlsunset package to use Night Light."), Some("warning"));
            card.set_sensitive(false);
        }
        let this = Rc::new(NightLightPage {
            page, store, runner: runner.unwrap_or_else(default_runner), config: RefCell::new(config), available,
            loading: Cell::new(true), card, enabled, schedule, location_note, sunset, sunrise, times_row, temperature, hdr_note, status,
            save_source: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        this.enabled.connect_active_notify(move |_| if let Some(t) = weak.upgrade() { t.changed(true, false) });
        let weak = Rc::downgrade(&this);
        this.schedule.connect_selected_notify(move |_| if let Some(t) = weak.upgrade() { t.changed(false, false) });
        let weak = Rc::downgrade(&this);
        this.temperature.connect_value_changed(move |_| if let Some(t) = weak.upgrade() { t.changed(false, true) });
        for entry in [&this.sunset, &this.sunrise] {
            let weak = Rc::downgrade(&this);
            entry.connect_activate(move |_| if let Some(t) = weak.upgrade() { t.changed(false, false) });
            let weak = Rc::downgrade(&this);
            entry.connect_changed(move |_| if let Some(t) = weak.upgrade() { t.changed(false, true) });
        }
        this.update_schedule();
        this.loading.set(false);
        match outputs {
            None => {
                let weak = Rc::downgrade(&this);
                this.page.run(|| Ok((enabled_outputs(), rc_text())), move |result| if let Some(t) = weak.upgrade() {
                    if let Ok(result) = result { t.outputs_loaded(result.0, &result.1) }
                });
            }
            Some((outputs, text)) => this.outputs_loaded(outputs, &text),
        }
        this
    }

    pub fn outputs_loaded(&self, outputs: Option<Vec<String>>, text: &str) {
        let Some(outputs) = outputs.filter(|o| !o.is_empty()) else { return };
        let (sdr, hdr) = split_outputs(&outputs, text);
        if !hdr.is_empty() && sdr.is_empty() {
            status_text(&self.hdr_note, &tr!("Every display is using HDR. Night light only works on SDR displays; turn off HDR in Displays to use it."),
                        Some("warning"));
            if !self.enabled.is_active() {
                self.enabled.set_sensitive(false);
            }
        } else if !hdr.is_empty() {
            status_text(&self.hdr_note, &tr!("Night light is not applied to HDR displays: {value1}", value1 = hdr.join(", ")), Some("warning"));
        }
        self.hdr_note.set_visible(!hdr.is_empty());
    }

    fn update_schedule(&self) {
        let manual = self.schedule.selected() == 1;
        let location = self.store.location();
        self.times_row.set_visible(manual || location.is_none());
        if manual {
            self.location_note.set_text("");
        } else if let Some(location) = location {
            let name = if location.0.is_empty() { tr!("Unknown") } else { location.0 };
            self.location_note.set_text(&tr!("Using the location from Weather settings: {value1}", value1 = name));
        } else {
            self.location_note.set_text(&tr!("Set a location in Weather settings to follow sunset and sunrise. Until then, these times are used."));
        }
    }

    fn current(&self) -> NightLightConfig {
        NightLightConfig {
            enabled: self.enabled.is_active(),
            schedule: if self.schedule.selected() == 1 { "manual".into() } else { "sun".into() },
            sunset: crate::compat::strip(&self.sunset.text()).to_owned(),
            sunrise: crate::compat::strip(&self.sunrise.text()).to_owned(),
            // int(round(value / 100) * 100): Python rounds half to even.
            temperature: ((self.temperature.value() / 100.0).round_ties_even() * 100.0) as i64,
        }
    }

    fn changed(self: &Rc<Self>, service: bool, delay: bool) {
        if self.loading.get() {
            return;
        }
        self.update_schedule();
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
        }
        if delay {
            let weak = Rc::downgrade(self);
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
                if let Some(t) = weak.upgrade() {
                    t.save_source.borrow_mut().take();
                    t.save(false);
                }
            });
            *self.save_source.borrow_mut() = Some(id);
            return;
        }
        self.save(service);
    }

    /// `_delayed()`: flush a pending delayed save now.
    pub fn delayed(self: &Rc<Self>) {
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
        }
        self.save(false);
    }

    pub fn save(self: &Rc<Self>, service: bool) {
        let previous = self.config.borrow().clone();
        let config = match self.store.save(&self.current()) {
            Ok(config) => config,
            Err(Error::Value(message)) => {
                let text = if message.contains("HH:MM") { tr!("Times must use the 24-hour HH:MM format.") } else { message };
                status_text(&self.status, &text, Some("error"));
                return;
            }
            Err(error) => {
                status_text(&self.status, &tr!("Night light settings could not be saved: {value1}", value1 = error.to_string()), Some("error"));
                return;
            }
        };
        *self.config.borrow_mut() = config.clone();
        let argv: Vec<String> = if service || previous.enabled != config.enabled {
            vec!["systemctl".into(), "--user".into(), if config.enabled { "enable" } else { "disable" }.into(), "--now".into(), UNIT.into()]
        } else if config.enabled {
            vec!["systemctl".into(), "--user".into(), "restart".into(), UNIT.into()]
        } else {
            status_text(&self.status, &tr!("Saved."), None);
            return;
        };
        let weak = Rc::downgrade(self);
        (self.runner)(argv, Box::new(move |okay, output| {
            let Some(t) = weak.upgrade() else { return };
            if t.page.closed() {
                return;
            }
            if okay {
                let text = if t.config.borrow().enabled { tr!("Night light is on.") } else { tr!("Night light is off.") };
                status_text(&t.status, &text, None);
            } else {
                let detail = if crate::compat::strip(&output).is_empty() { tr!("Unknown error") } else { crate::compat::strip(&output).to_owned() };
                status_text(&t.status, &tr!("Night light could not be started: {value1}", value1 = detail), Some("error"));
            }
        }));
    }

    pub fn close(self: &Rc<Self>) {
        if self.save_source.borrow().is_some() {
            self.delayed();
        }
        self.page.close();
    }
}

pub fn build_page_with(store: Option<NightLightStore>, runner: Option<Runner>, outputs: Outputs, available: Option<bool>) -> SettingsPage {
    let controller = NightLightPage::new(store, runner, outputs, available);
    let weak = Rc::downgrade(&controller);
    let page = SettingsPage::new(controller.page.widget.clone()).with_close(move || if let Some(c) = weak.upgrade() { c.close() });
    page.add_target("night-enabled", &controller.enabled);
    page.add_target("night-schedule", &controller.schedule);
    page.add_target("night-temperature", &controller.temperature);
    page.add_target("night-times", &controller.sunset);
    page.set_controller(controller);
    page
}
