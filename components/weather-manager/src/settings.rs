//! weather_settings.py: one Weather settings widget shared by Global Settings
//! and the app window. Every control change saves immediately, merged with
//! the latest file.

use crate::backend::{search_locations, ConfigStore, Fetch, Location, Num};
use crate::location::{self, LocationCallback, PendingLocation};
use crate::ui::{install_style, label, margins, wrapped};
use crate::worker::Worker;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use suite_core::i18n::translate;
use suite_core::tr;
use suite_gtk::utils::{accessible, headerbar, image};

pub type SearchFn = Arc<dyn Fn(&str) -> Result<Vec<Location>, String> + Send + Sync>;
pub type ProbeFn = Arc<dyn Fn() -> (bool, String) + Send + Sync>;
pub type RequestFactory = Rc<dyn Fn(LocationCallback) -> Rc<dyn PendingLocation>>;
pub type ResolveFn = Arc<dyn Fn(f64, f64) -> Result<Location, String> + Send + Sync>;

/// Injectable services (Python's keyword arguments); `None` = the real ones.
#[derive(Default, Clone)]
pub struct Services {
    pub search: Option<SearchFn>,
    pub location_probe: Option<ProbeFn>,
    pub location_request: Option<RequestFactory>,
    pub location_resolve: Option<ResolveFn>,
    pub fetch: Option<Fetch>,
}

/// Fields of a `replace(config, **changes)` call.
#[derive(Default, Clone)]
pub struct Changes {
    pub name: Option<String>,
    pub latitude: Option<Num>,
    pub longitude: Option<Num>,
    pub timezone: Option<String>,
    pub units: Option<String>,
    pub week: Option<String>,
    pub refresh_minutes: Option<i64>,
}

pub struct WeatherSettings {
    pub root: gtk::Box,
    pub store: Arc<ConfigStore>,
    search: SearchFn,
    location_probe: ProbeFn,
    location_factory: RequestFactory,
    location_resolve: ResolveFn,
    worker: Worker,
    probe_worker: Worker,
    location_worker: Worker,
    location_request: RefCell<Option<Rc<dyn PendingLocation>>>,
    pub location_supported: Cell<bool>,
    pub locating: Cell<bool>,
    locate_pending: Cell<bool>,
    pub closed: Cell<bool>,
    syncing: Cell<bool>,
    style_close: RefCell<Option<Box<dyn Fn()>>>,
    pub config: RefCell<crate::backend::WeatherConfig>,
    pub location_label: gtk::Label,
    pub current_location: gtk::Button,
    pub cancel_location: gtk::Button,
    pub location_status: gtk::Label,
    pub location_search: gtk::SearchEntry,
    pub search_button: gtk::Button,
    pub search_status: gtk::Label,
    pub results: gtk::ListBox,
    pub results_scroll: gtk::ScrolledWindow,
    pub result_locations: RefCell<Vec<(gtk::ListBoxRow, Location)>>,
    pub location_name: gtk::Entry,
    pub latitude: gtk::SpinButton,
    pub longitude: gtk::SpinButton,
    pub timezone: gtk::Entry,
    pub manual_apply: gtk::Button,
    pub units: gtk::DropDown,
    pub week: gtk::DropDown,
    pub refresh_minutes: gtk::SpinButton,
    pub status: gtk::Label,
}

fn card(parent: &gtk::Box, title: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    margins(&body, 16);
    body.append(&label(title, Some("section-title")));
    card.append(&body);
    parent.append(&card);
    body
}

fn row(title: &str, control: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>)) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let text = wrapped(title, None, -1);
    text.set_hexpand(true);
    row.append(&text);
    control.set_valign(gtk::Align::Center);
    control.update_property(&[gtk::accessible::Property::Label(title)]);
    row.append(control);
    row
}

impl WeatherSettings {
    pub fn new(store: Option<Arc<ConfigStore>>, services: Services) -> Rc<Self> {
        let store = store.unwrap_or_else(|| Arc::new(ConfigStore::new(None)));
        let fetch = services.fetch.clone().unwrap_or_else(crate::backend::real_fetch);
        let search = services.search.clone().unwrap_or_else(|| {
            let fetch = fetch.clone();
            Arc::new(move |query: &str| search_locations(query, &fetch).map_err(|e| e.0))
        });
        let location_probe = services.location_probe.clone().unwrap_or_else(|| Arc::new(location::available));
        let location_factory = services.location_request.clone().unwrap_or_else(|| Rc::new(location::new_request));
        let location_resolve = services.location_resolve.clone().unwrap_or_else(|| {
            let fetch = fetch.clone();
            Arc::new(move |lat, lon| location::weather_location(lat, lon, &fetch))
        });
        let config = store.load();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let style_close = install_style(&root);
        margins(&root, 24);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        heading.append(&image("weather-few-clouds-symbolic", 30));
        heading.append(&label(&tr!("Weather"), Some("title")));
        root.append(&heading);
        root.append(&wrapped(&tr!("Choose a location and the forecast shown in your panel."), Some("subtitle"), -1));

        let location = card(&root, &tr!("Location"));
        let location_label = wrapped("", Some("section-title"), -1);
        location.append(&location_label);
        let current_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let current_location = gtk::Button::new();
        current_location.set_sensitive(false);
        let current_content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        current_content.append(&image("find-location-symbolic", 18));
        current_content.append(&label(&tr!("Use current location"), None));
        current_location.set_child(Some(&current_content));
        current_row.append(&current_location);
        let cancel_location = gtk::Button::with_label(&tr!("Cancel"));
        cancel_location.set_visible(false);
        current_row.append(&cancel_location);
        location.append(&current_row);
        let location_status = wrapped(&tr!("Checking system location services…"), Some("subtitle"), -1);
        location.append(&location_status);
        let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let location_search = gtk::SearchEntry::new();
        location_search.set_hexpand(true);
        location_search.set_placeholder_text(Some(&tr!("Search for a city")));
        accessible(&location_search, &tr!("Search for a weather location"));
        search_row.append(&location_search);
        let search_button = gtk::Button::with_label(&tr!("Search"));
        search_row.append(&search_button);
        location.append(&search_row);
        let search_status = wrapped("", Some("subtitle"), -1);
        location.append(&search_status);
        let results = gtk::ListBox::new();
        results.set_selection_mode(gtk::SelectionMode::None);
        results.add_css_class("weather-search-results");
        let results_scroll = gtk::ScrolledWindow::new();
        results_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        results_scroll.set_propagate_natural_height(true);
        results_scroll.set_max_content_height(210);
        results_scroll.set_child(Some(&results));
        results_scroll.set_visible(false);
        location.append(&results_scroll);

        let manual = gtk::Expander::new(Some(&tr!("Enter coordinates manually")));
        location.append(&manual);
        let manual_content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        margins(&manual_content, 10);
        manual.set_child(Some(&manual_content));
        let location_name = gtk::Entry::new();
        location_name.set_hexpand(true);
        let latitude = gtk::SpinButton::with_range(-90.0, 90.0, 0.0001);
        let longitude = gtk::SpinButton::with_range(-180.0, 180.0, 0.0001);
        latitude.set_digits(4);
        longitude.set_digits(4);
        let timezone = gtk::Entry::new();
        timezone.set_hexpand(true);
        timezone.set_placeholder_text(Some(&tr!("For example America/Denver")));
        manual_content.append(&row(&tr!("Location name"), &location_name));
        manual_content.append(&row(&tr!("Latitude"), &latitude));
        manual_content.append(&row(&tr!("Longitude"), &longitude));
        manual_content.append(&row(&tr!("Time zone"), &timezone));
        let manual_apply = gtk::Button::with_label(&tr!("Use this location"));
        manual_apply.set_halign(gtk::Align::End);
        manual_content.append(&manual_apply);

        let forecast = card(&root, &tr!("Forecast"));
        let units = gtk::DropDown::from_strings(&[&tr!("Fahrenheit (°F)"), &tr!("Celsius (°C)")]);
        forecast.append(&row(&tr!("Temperature units"), &units));
        let week = gtk::DropDown::from_strings(&[&tr!("Upcoming Sunday–Saturday"), &tr!("Current Sunday–Saturday")]);
        forecast.append(&row(&tr!("Forecast week"), &week));
        forecast.append(&wrapped(&tr!("Seven days, ordered Sunday through Saturday. Days without a forecast are marked unavailable."),
                                 Some("subtitle"), -1));
        let refresh_minutes = gtk::SpinButton::with_range(15.0, 1440.0, 5.0);
        forecast.append(&row(&tr!("Refresh interval (minutes)"), &refresh_minutes));
        let status = wrapped("", Some("subtitle"), -1);
        root.append(&status);
        root.append(&wrapped(&tr!("Weather and location search provided by Open-Meteo. No account is required."), Some("subtitle"), -1));

        let settings = Rc::new(Self {
            root, store, search, location_probe, location_factory, location_resolve, worker: Worker::new(),
            probe_worker: Worker::new(), location_worker: Worker::new(), location_request: RefCell::new(None),
            location_supported: Cell::new(false), locating: Cell::new(false), locate_pending: Cell::new(false),
            closed: Cell::new(false), syncing: Cell::new(false), style_close: RefCell::new(Some(style_close)),
            config: RefCell::new(config), location_label, current_location, cancel_location, location_status, location_search,
            search_button, search_status, results, results_scroll, result_locations: RefCell::new(Vec::new()), location_name,
            latitude, longitude, timezone, manual_apply, units, week, refresh_minutes, status,
        });
        let w = Rc::downgrade(&settings);
        settings.current_location.connect_clicked(move |_| if let Some(s) = w.upgrade() { s.use_current_location() });
        let w = Rc::downgrade(&settings);
        settings.cancel_location.connect_clicked(move |_| if let Some(s) = w.upgrade() { s.cancel_location(true) });
        let w = Rc::downgrade(&settings);
        settings.location_search.connect_activate(move |_| if let Some(s) = w.upgrade() { s.search() });
        let w = Rc::downgrade(&settings);
        settings.location_search.connect_text_notify(move |_| if let Some(s) = w.upgrade() { s.query_changed() });
        let w = Rc::downgrade(&settings);
        settings.search_button.connect_clicked(move |_| if let Some(s) = w.upgrade() { s.search() });
        let w = Rc::downgrade(&settings);
        settings.results.connect_row_activated(move |_, row| if let Some(s) = w.upgrade() { s.selected_location(row) });
        let w = Rc::downgrade(&settings);
        settings.manual_apply.connect_clicked(move |_| if let Some(s) = w.upgrade() { s.manual() });
        let w = Rc::downgrade(&settings);
        settings.units.connect_selected_notify(move |units| if let Some(s) = w.upgrade() {
            s.preference(Changes { units: Some(if units.selected() == 0 { "fahrenheit" } else { "celsius" }.into()), ..Changes::default() })
        });
        let w = Rc::downgrade(&settings);
        settings.week.connect_selected_notify(move |week| if let Some(s) = w.upgrade() {
            s.preference(Changes { week: Some(if week.selected() == 0 { "upcoming" } else { "current" }.into()), ..Changes::default() })
        });
        let w = Rc::downgrade(&settings);
        settings.refresh_minutes.connect_value_changed(move |spin| if let Some(s) = w.upgrade() {
            s.preference(Changes { refresh_minutes: Some(spin.value_as_int() as i64), ..Changes::default() })
        });
        settings.sync();
        if let Some(error) = settings.store.error() {
            settings.status.set_text(&error);
        }
        settings.probe_location();
        settings
    }

    /// Global Settings search targets (same keys as Python).
    pub fn search_targets(&self) -> BTreeMap<&'static str, gtk::Widget> {
        BTreeMap::from([("location", self.location_search.clone().upcast()), ("units", self.units.clone().upcast()),
                        ("forecast", self.week.clone().upcast()), ("refresh", self.refresh_minutes.clone().upcast()),
                        ("current_location", self.current_location.clone().upcast())])
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> { Rc::downgrade(self) }

    fn probe_location(self: &Rc<Self>) {
        let weak = self.weak();
        let probe = self.location_probe.clone();
        self.probe_worker.run(move || Ok(probe()), move |result: Option<(bool, String)>, _error| {
            let Some(this) = weak.upgrade() else { return };
            this.location_supported.set(result.as_ref().is_some_and(|r| r.0));
            this.current_location.set_sensitive(this.location_supported.get() && !this.locating.get());
            if !this.locating.get() {
                this.location_status.set_text(&translate(result.as_ref().map(|r| r.1.as_str())
                    .unwrap_or("System location services are unavailable. You can still choose a city manually.")));
            }
            if this.locate_pending.get() {
                this.locate_pending.set(false);
                if this.location_supported.get() {
                    this.use_current_location();
                }
            }
        });
    }

    /// Request the current location (asked for explicitly) once availability is known.
    pub fn locate_when_ready(self: &Rc<Self>) {
        if self.closed.get() || self.locating.get() {
            return;
        }
        if self.location_supported.get() { self.use_current_location() } else { self.locate_pending.set(true) }
    }

    pub fn cancel_location(&self, message: bool) {
        let was_locating = self.locating.get();
        self.locating.set(false);
        if let Some(request) = self.location_request.take() {
            request.cancel();
        }
        self.location_worker.cancel();
        self.cancel_location.set_visible(false);
        self.current_location.set_sensitive(self.location_supported.get());
        if message || was_locating {
            self.location_status.set_text(&tr!("Location request cancelled."));
        }
    }

    pub fn use_current_location(self: &Rc<Self>) {
        if self.closed.get() || self.locating.get() || !self.location_supported.get() {
            return;
        }
        self.worker.cancel();
        self.clear_results();
        self.search_button.set_sensitive(true);
        self.search_status.set_text("");
        self.locating.set(true);
        self.current_location.set_sensitive(false);
        self.cancel_location.set_visible(true);
        self.location_status.set_text(&tr!("Finding your location… Allow access if the system asks."));
        let weak = self.weak();
        let located: LocationCallback = Box::new(move |position, error| {
            let Some(this) = weak.upgrade() else { return };
            if this.closed.get() || !this.locating.get() {
                return;
            }
            match (position, error) {
                (_, Some(error)) => this.resolved(None, Some(translate(&error))),
                (Some((lat, lon)), None) => {
                    this.location_status.set_text(&tr!("Preparing weather for your location…"));
                    let resolve = this.location_resolve.clone();
                    let weak = this.weak();
                    this.location_worker.run(move || resolve(lat, lon), move |place, error| {
                        if let Some(this) = weak.upgrade() { this.resolved(place, error) }
                    });
                }
                (None, None) => {}
            }
        });
        let request = (self.location_factory)(located);
        // A synchronous callback may already have finished (and cleared) the request.
        if self.locating.get() {
            self.location_request.replace(Some(request));
        }
    }

    fn resolved(self: &Rc<Self>, place: Option<Location>, error: Option<String>) {
        if self.closed.get() || !self.locating.get() {
            return;
        }
        self.cancel_location(false);
        if let Some(error) = error {
            self.location_status.set_text(&(tr!("Could not use current location: ") + &error));
        } else if let Some(place) = place {
            if self.save(Changes { name: Some(translate(&place.name)), latitude: Some(place.latitude), longitude: Some(place.longitude),
                                   timezone: Some(place.timezone), ..Changes::default() }) {
                self.location_status.set_text(&tr!("Current location saved. Use this button again to update it."));
            }
        }
    }

    pub fn sync(&self) {
        self.syncing.set(true);
        let config = self.config.borrow().clone();
        let configured = self.store.configured().is_some();
        self.location_label.set_text(&if configured { config.name.clone() } else { tr!("Choose a location") });
        self.location_name.set_text(if configured { &config.name } else { "" });
        self.latitude.set_value(config.latitude.value());
        self.longitude.set_value(config.longitude.value());
        self.timezone.set_text(if configured { &config.timezone } else { "" });
        self.units.set_selected(if config.units == "fahrenheit" { 0 } else { 1 });
        self.week.set_selected(if config.week == "upcoming" { 0 } else { 1 });
        self.refresh_minutes.set_value(config.refresh_minutes as f64);
        self.units.set_sensitive(configured);
        self.week.set_sensitive(configured);
        self.refresh_minutes.set_sensitive(configured);
        self.syncing.set(false);
    }

    pub fn save(&self, changes: Changes) -> bool {
        // Merge only the changed fields with the latest file so another
        // settings window cannot silently overwrite unrelated preferences.
        let mut config = self.store.load();
        if let Some(v) = changes.name { config.name = v; }
        if let Some(v) = changes.latitude { config.latitude = v; }
        if let Some(v) = changes.longitude { config.longitude = v; }
        if let Some(v) = changes.timezone { config.timezone = v; }
        if let Some(v) = changes.units { config.units = v; }
        if let Some(v) = changes.week { config.week = v; }
        if let Some(v) = changes.refresh_minutes { config.refresh_minutes = v; }
        if let Err(error) = self.store.save(&config) {
            self.status.set_text(&(tr!("Could not save weather settings: ") + &error.0));
            return false;
        }
        self.config.replace(config);
        self.sync();
        self.status.set_text(&tr!("Weather settings saved."));
        true
    }

    fn preference(&self, changes: Changes) {
        if !self.syncing.get() && !self.closed.get() {
            if self.store.configured().is_none() {
                return;
            }
            if !self.save(changes) {
                self.sync();
            }
        }
    }

    pub fn manual(&self) {
        self.cancel_location(false);
        if self.save(Changes { name: Some(self.location_name.text().trim().to_owned()),
                               latitude: Some(Num::Float(self.latitude.value())), longitude: Some(Num::Float(self.longitude.value())),
                               timezone: Some(self.timezone.text().trim().to_owned()), ..Changes::default() }) {
            self.worker.cancel();
            self.search_status.set_text("");
            self.clear_results();
        }
    }

    fn clear_results(&self) {
        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
        self.result_locations.borrow_mut().clear();
        self.results_scroll.set_visible(false);
    }

    fn query_changed(&self) {
        if self.locating.get() {
            self.cancel_location(true);
        }
        self.worker.cancel();
        self.search_button.set_sensitive(true);
        self.search_status.set_text("");
        self.clear_results();
    }

    pub fn search(self: &Rc<Self>) {
        let query = self.location_search.text().trim().to_owned();
        if query.is_empty() {
            self.search_status.set_text(&tr!("Enter a city name to search."));
            return;
        }
        self.clear_results();
        self.search_status.set_text(&tr!("Searching…"));
        self.search_button.set_sensitive(false);
        let weak = self.weak();
        let search = self.search.clone();
        let asked = query.clone();
        self.worker.run(move || search(&asked), move |locations: Option<Vec<Location>>, error| {
            let Some(this) = weak.upgrade() else { return };
            this.search_button.set_sensitive(true);
            if this.location_search.text().trim() != query {
                return;
            }
            if let Some(error) = error {
                this.search_status.set_text(&(tr!("Location search failed: ") + &error));
                return;
            }
            let locations = locations.unwrap_or_default();
            this.search_status.set_text(&if locations.is_empty() { tr!("No matching locations found.") } else { tr!("Select a location below.") });
            for place in &locations {
                let row = gtk::ListBoxRow::new();
                row.set_activatable(true);
                let line = gtk::Box::new(gtk::Orientation::Vertical, 3);
                line.append(&wrapped(&place.name, None, -1));
                line.append(&label(&format!("{:.4}, {:.4} · {}", place.latitude.value(), place.longitude.value(), place.timezone),
                                   Some("subtitle")));
                row.set_child(Some(&line));
                this.results.append(&row);
                this.result_locations.borrow_mut().push((row, place.clone()));
            }
            this.results_scroll.set_visible(!locations.is_empty());
        });
    }

    pub fn location_of(&self, row: &gtk::ListBoxRow) -> Option<Location> {
        self.result_locations.borrow().iter().find(|(r, _)| r == row).map(|(_, l)| l.clone())
    }

    fn selected_location(&self, row: &gtk::ListBoxRow) {
        self.cancel_location(false);
        let Some(place) = self.location_of(row) else { return };
        if self.save(Changes { name: Some(place.name), latitude: Some(place.latitude), longitude: Some(place.longitude),
                               timezone: Some(place.timezone), ..Changes::default() }) {
            self.worker.cancel();
            self.search_status.set_text(&tr!("Location selected."));
            self.clear_results();
        }
    }

    pub fn activate(self: &Rc<Self>) {
        if !self.closed.get() {
            self.config.replace(self.store.load());
            self.sync();
            self.probe_location();
        }
    }

    pub fn deactivate(&self) {
        self.locate_pending.set(false);
        self.cancel_location(false);
        self.probe_worker.cancel();
        self.worker.cancel();
        self.search_button.set_sensitive(true);
        self.search_status.set_text("");
    }

    pub fn close(&self) {
        if !self.closed.get() {
            self.closed.set(true);
            self.cancel_location(false);
            self.probe_worker.close();
            self.location_worker.close();
            self.worker.close();
            if let Some(close) = self.style_close.take() {
                close();
            }
        }
    }
}

pub struct WeatherSettingsWindow {
    pub window: gtk::ApplicationWindow,
    pub settings: Rc<WeatherSettings>,
}

impl WeatherSettingsWindow {
    pub fn new(application: &gtk::Application, store: Option<Arc<ConfigStore>>, services: Services) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Weather Settings")).build();
        window.set_default_size(620, 700);
        window.set_titlebar(Some(&headerbar(&tr!("Weather Settings"))));
        let settings = WeatherSettings::new(store, services);
        window.add_css_class("weathermanager");
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&settings.root));
        window.set_child(Some(&scroll));
        let weak = Rc::downgrade(&settings);
        window.connect_unrealize(move |_| if let Some(s) = weak.upgrade() { s.close() });
        Rc::new(Self { window, settings })
    }
}
