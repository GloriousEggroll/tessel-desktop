//! weather_ui.py: current weather and a seven-day forecast in the suite's
//! native GTK4 popup (shared PopupController, group `weather`).

use crate::backend::{format_temperature, has_location, weather_description, weather_icon, ConfigStore, WeatherClient,
                     WeatherConfig, WeatherSnapshot};
use crate::dates::{self, Date};
use crate::worker::Worker;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use tessel_core::pyfmt::{format_value, Arg};
use tessel_core::tr;
use tessel_gtk::popup::{header_button, popup_header, set_popup_width, HeaderAction, PopupController, POPUP_PADDING, POPUP_WIDTH};
use tessel_gtk::utils::image;

pub const CSS: &str = "
.weathermanager .subtitle, .weathermanager .muted { color: alpha(@theme_fg_color, .65); }
.weathermanager .weather-temperature { font-size: 34pt; font-weight: 600; }
.weathermanager .card { background-color: alpha(@theme_fg_color, .045);
    border: 1px solid alpha(@theme_fg_color, .10); border-radius: 8px; }
.weathermanager .weather-day { padding: 9px 12px; }
.weathermanager .weather-day + .weather-day { border-top: 1px solid alpha(@theme_fg_color, .08); }
.weathermanager button.icon-button { padding: 6px; min-width: 24px;
    background: transparent; border-color: transparent; box-shadow: none; }
.weathermanager button.icon-button:hover { background-color: alpha(@theme_fg_color, .08); }
window.weathermanager.tray-popup { background: transparent; }
.weathermanager .popup-surface { background-color: @theme_bg_color;
    border: 1px solid alpha(@theme_fg_color, .14); border-radius: 8px; }
.weathermanager .weather-search-results row { padding: 9px; border-radius: 7px; }
.weathermanager .weather-search-results row:hover { background-color: alpha(@theme_fg_color, .07); }
";

/// What the popup and tray read forecasts from (the real client or a test fake).
pub trait WeatherSource: Send + Sync {
    fn read(&self, config: &WeatherConfig, force: bool) -> Result<WeatherSnapshot, String>;
    fn cache_path(&self, _config: &WeatherConfig) -> Option<PathBuf> { None }
    fn cache_dir(&self) -> Option<PathBuf> { None }
}

impl WeatherSource for WeatherClient {
    fn read(&self, config: &WeatherConfig, force: bool) -> Result<WeatherSnapshot, String> {
        WeatherClient::read(self, Some(config), force).map_err(|e| e.0)
    }
    fn cache_path(&self, config: &WeatherConfig) -> Option<PathBuf> { Some(WeatherClient::cache_path(self, config)) }
    fn cache_dir(&self) -> Option<PathBuf> { Some(self.cache_dir.clone()) }
}

pub fn margins(widget: &impl IsA<gtk::Widget>, size: i32) {
    widget.set_margin_top(size);
    widget.set_margin_bottom(size);
    widget.set_margin_start(size);
    widget.set_margin_end(size);
}

pub fn label(text: &str, css: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::new(Some(text));
    widget.set_xalign(0.0);
    if let Some(css) = css {
        widget.add_css_class(css);
    }
    widget
}

pub fn wrapped(text: &str, css: Option<&str>, max_width_chars: i32) -> gtk::Label {
    let widget = label(text, css);
    widget.set_wrap(true);
    if max_width_chars >= 0 {
        widget.set_max_width_chars(max_width_chars);
    }
    widget
}

/// Adds `weathermanager` and the module CSS; the returned closure removes the provider.
pub fn install_style(widget: &impl IsA<gtk::Widget>) -> Box<dyn Fn()> {
    widget.add_css_class("weathermanager");
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    let display = widget.display();
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    Box::new(move || gtk::style_context_remove_provider_for_display(&display, &provider))
}

/// `"{value:.0f}"`.
pub fn fixed0(value: f64) -> String { format_value(&Arg::Float(value), ".0f").unwrap_or_default() }

/// `date.strftime(format)` in the current LC_TIME locale.
pub fn strftime(date: Date, format: &str) -> String {
    glib::DateTime::from_local(date.year, date.month as i32, date.day as i32, 12, 0, 0.0)
        .ok().and_then(|d| d.format(format).ok()).map(|s| s.to_string()).unwrap_or_default()
}

pub struct WeatherPopup {
    pub window: gtk::ApplicationWindow,
    pub controller: RefCell<Option<PopupController>>,
    client: Arc<dyn WeatherSource>,
    store: Arc<ConfigStore>,
    open_settings: Rc<dyn Fn()>,
    locate: Option<Rc<dyn Fn()>>,
    pub closed: Cell<bool>,
    worker: Worker,
    pub config: RefCell<WeatherConfig>,
    pub snapshot: RefCell<Option<WeatherSnapshot>>,
    loading: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    last_check: Cell<f64>,
    style_close: RefCell<Option<Box<dyn Fn()>>>,
    pub refresh_button: gtk::Button,
    pub settings_button: gtk::Button,
    pub location_label: gtk::Label,
    pub setup_card: gtk::Box,
    pub locate_button: Option<gtk::Button>,
    pub choose_button: gtk::Button,
    pub current_card: gtk::Box,
    pub current_icon: gtk::Image,
    pub temperature: gtk::Label,
    pub condition: gtk::Label,
    pub details: gtk::Label,
    pub forecast_title: gtk::Label,
    pub forecast_scroll: gtk::ScrolledWindow,
    pub forecast: gtk::Box,
    pub day_rows: RefCell<Vec<gtk::Box>>,
    pub message: gtk::Label,
    pub attribution: gtk::Label,
}

fn monotonic() -> f64 { glib::monotonic_time() as f64 / 1_000_000.0 }

impl WeatherPopup {
    #[allow(clippy::too_many_arguments)]
    pub fn new(application: &gtk::Application, client: Arc<dyn WeatherSource>, store: Arc<ConfigStore>,
               open_settings: Rc<dyn Fn()>, edge: &str, offset: i32, locate: Option<Rc<dyn Fn()>>) -> Rc<Self> {
        let config = store.load();
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Weather")).build();
        set_popup_width(window.upcast_ref(), POPUP_WIDTH);
        let style_close = install_style(&window);
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("popup-surface");
        window.set_child(Some(&surface));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, POPUP_PADDING);
        surface.append(&content);
        let refresh_button = header_button("view-refresh-symbolic", &tr!("Refresh weather"), None, false);
        let settings_button = header_button("emblem-system-symbolic", &tr!("Weather settings"), None, false);
        let heading = popup_header(&["weather-few-clouds-symbolic"], &tr!("Weather"), Some(&location_text(&config)),
                                   vec![HeaderAction::Widget(refresh_button.clone().upcast()),
                                        HeaderAction::Widget(settings_button.clone().upcast())]);
        let location_label = heading.subtitle_label.clone();
        location_label.set_max_width_chars(28);
        content.append(&heading.root);
        let setup_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        setup_card.set_visible(false);
        setup_card.add_css_class("card");
        let setup = gtk::Box::new(gtk::Orientation::Vertical, 10);
        margins(&setup, 14);
        setup_card.append(&setup);
        setup.append(&label(&tr!("Choose a location"), Some("row-title")));
        setup.append(&wrapped(&tr!("Weather needs a location. Search for a city, or use your current location if you allow it."),
                              Some("row-detail"), 40));
        // Stacked full-width buttons keep each label on one line.
        let setup_buttons = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let locate_button = locate.as_ref().map(|_| {
            let button = gtk::Button::with_label(&tr!("Use current location"));
            setup_buttons.append(&button);
            button
        });
        let choose_button = gtk::Button::with_label(&tr!("Choose location…"));
        choose_button.add_css_class("suggested-action");
        setup_buttons.append(&choose_button);
        setup.append(&setup_buttons);
        content.append(&setup_card);
        let current_card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        current_card.add_css_class("card");
        let current = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&current, 14);
        current_card.append(&current);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let current_icon = image("weather-few-clouds-symbolic", 48);
        line.append(&current_icon);
        let temperature = label("—", Some("weather-temperature"));
        line.append(&temperature);
        let condition = wrapped(&tr!("Loading weather…"), None, 16);
        condition.set_hexpand(true);
        line.append(&condition);
        current.append(&line);
        let details = wrapped("", Some("row-detail"), 40);
        current.append(&details);
        content.append(&current_card);
        let forecast_title = label(&tr!("Seven-day forecast · Sunday–Saturday"), Some("section-title"));
        content.append(&forecast_title);
        let forecast_scroll = gtk::ScrolledWindow::new();
        forecast_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        forecast_scroll.set_propagate_natural_height(true);
        forecast_scroll.set_max_content_height(390);
        let forecast = gtk::Box::new(gtk::Orientation::Vertical, 0);
        forecast.add_css_class("card");
        forecast_scroll.set_child(Some(&forecast));
        content.append(&forecast_scroll);
        let message = wrapped(&tr!("Loading weather…"), Some("row-detail"), 40);
        content.append(&message);
        let attribution = label(&tr!("Weather by Open-Meteo"), Some("row-detail"));
        content.append(&attribution);
        let popup = Rc::new(Self {
            window, controller: RefCell::new(None), client, store, open_settings, locate, closed: Cell::new(false),
            worker: Worker::new(), config: RefCell::new(config), snapshot: RefCell::new(None), loading: Cell::new(false),
            timer: RefCell::new(None), last_check: Cell::new(0.0), style_close: RefCell::new(Some(style_close)),
            refresh_button, settings_button, location_label, setup_card, locate_button, choose_button, current_card,
            current_icon, temperature, condition, details, forecast_title, forecast_scroll, forecast,
            day_rows: RefCell::new(Vec::new()), message, attribution,
        });
        let weak = Rc::downgrade(&popup);
        popup.refresh_button.connect_clicked(move |_| if let Some(p) = weak.upgrade() { p.refresh(true) });
        let weak = Rc::downgrade(&popup);
        popup.settings_button.connect_clicked(move |_| if let Some(p) = weak.upgrade() { p.settings() });
        let weak = Rc::downgrade(&popup);
        popup.choose_button.connect_clicked(move |_| if let Some(p) = weak.upgrade() { p.settings() });
        if let Some(button) = &popup.locate_button {
            let weak = Rc::downgrade(&popup);
            button.connect_clicked(move |_| if let Some(p) = weak.upgrade() { p.use_current_location() });
        }
        let weak = Rc::downgrade(&popup);
        let controller = PopupController::new(popup.window.upcast_ref(), move || if let Some(p) = weak.upgrade() { p.refresh(false) },
                                              edge, offset, "weather");
        popup.controller.replace(Some(controller));
        let weak = Rc::downgrade(&popup);
        popup.window.connect_visible_notify(move |_| if let Some(p) = weak.upgrade() { p.visibility() });
        let weak = Rc::downgrade(&popup);
        popup.window.connect_unrealize(move |_| if let Some(p) = weak.upgrade() { p.close() });
        popup
    }

    pub fn controller(&self) -> PopupController { self.controller.borrow().clone().expect("controller") }

    fn settings(&self) {
        self.dismiss();
        (self.open_settings)();
    }

    fn use_current_location(&self) {
        self.dismiss();
        if let Some(locate) = &self.locate {
            locate();
        }
    }

    fn show_setup(&self, needed: bool) {
        self.setup_card.set_visible(needed);
        self.current_card.set_visible(!needed);
        self.forecast_title.set_visible(!needed);
        self.forecast_scroll.set_visible(!needed);
        self.message.set_visible(!needed);
        self.attribution.set_visible(!needed);
        self.refresh_button.set_sensitive(!needed && !self.loading.get());
    }

    fn visibility(self: &Rc<Self>) {
        if self.window.get_visible() {
            if self.timer.borrow().is_none() {
                let weak: Weak<Self> = Rc::downgrade(self);
                let id = glib::timeout_add_seconds_local(2, move || match weak.upgrade() {
                    Some(popup) => popup.tick(),
                    None => glib::ControlFlow::Break,
                });
                self.timer.replace(Some(id));
            }
        } else {
            self.worker.cancel();
            self.loading.set(false);
            if let Some(id) = self.timer.take() {
                id.remove();
            }
        }
    }

    /// The 2 s tick (public for tests, like Python's `_tick`).
    pub fn tick(self: &Rc<Self>) -> glib::ControlFlow {
        if self.closed.get() {
            self.timer.replace(None);
            return glib::ControlFlow::Break;
        }
        let config = self.store.load();
        if config != *self.config.borrow() {
            self.snapshot.replace(None);
            self.refresh(false);
        } else if !self.loading.get() && self.snapshot.borrow().is_some() {
            // The backend cache controls network frequency; poll once per minute.
            if self.last_check.get() + 60.0 <= monotonic() {
                self.refresh(false);
            }
        }
        glib::ControlFlow::Continue
    }

    pub fn refresh(self: &Rc<Self>, force: bool) {
        if self.closed.get() {
            return;
        }
        let config = self.store.load();
        if self.loading.get() && config == *self.config.borrow() && !force {
            return;
        }
        let changed = config != *self.config.borrow();
        self.config.replace(config.clone());
        self.last_check.set(monotonic());
        self.location_label.set_text(&location_text(&config));
        if !has_location(&config) {
            // Nothing to fetch: prompt for setup instead of loading forever.
            self.worker.cancel();
            self.loading.set(false);
            self.snapshot.replace(None);
            self.clear_days();
            self.show_setup(true);
            return;
        }
        self.show_setup(false);
        if changed {
            self.snapshot.replace(None);
            self.temperature.set_text("—");
            self.condition.set_text(&tr!("Loading weather…"));
            self.details.set_text("");
            self.clear_days();
        }
        self.loading.set(true);
        self.refresh_button.set_sensitive(false);
        self.message.set_text(&if self.snapshot.borrow().is_some() { tr!("Updating weather…") } else { tr!("Loading weather…") });
        let weak = Rc::downgrade(self);
        let client = self.client.clone();
        let requested = config.clone();
        self.worker.run(move || client.read(&requested, force), move |snapshot, error| {
            let Some(popup) = weak.upgrade() else { return };
            popup.loading.set(false);
            popup.refresh_button.set_sensitive(true);
            if popup.store.load() != config {
                popup.refresh(false);
                return;
            }
            if let Some(error) = error {
                if popup.snapshot.borrow().is_none() {
                    popup.condition.set_text(&tr!("Weather unavailable"));
                }
                popup.message.set_text(&(tr!("Could not update weather: ") + &error));
                return;
            }
            if let Some(snapshot) = snapshot {
                popup.render(&snapshot);
                popup.snapshot.replace(Some(snapshot));
            }
        });
    }

    fn clear_days(&self) {
        while let Some(child) = self.forecast.first_child() {
            self.forecast.remove(&child);
        }
        self.day_rows.borrow_mut().clear();
    }

    pub fn render(&self, snapshot: &WeatherSnapshot) {
        let units = snapshot.config.units.as_str();
        let current = snapshot.current.as_ref();
        self.temperature.set_text(&current.map(|c| format_temperature(Some(c.temp), units)).unwrap_or_else(|| "—".into()));
        self.condition.set_text(&match current {
            Some(c) => tessel_core::i18n::translate(weather_description(Some(c.code))),
            None => tr!("Weather unavailable"),
        });
        self.current_icon.set_icon_name(Some(current.map(|c| weather_icon(Some(c.code), c.is_day)).unwrap_or("weather-severe-alert-symbolic")));
        let mut details: Vec<String> = Vec::new();
        if let Some(current) = current {
            if current.apparent.is_some() {
                details.push(tr!("Feels like ") + &format_temperature(current.apparent, units));
            }
            if let Some(humidity) = current.humidity {
                details.push(tr!("Humidity ") + &fixed0(humidity) + "%");
            }
            if let Some(wind) = current.wind {
                details.push(tr!("Wind ") + &fixed0(wind) + " " + if units == "fahrenheit" { "mph" } else { "km/h" });
            }
        }
        self.details.set_text(&details.join(" · "));
        self.clear_days();
        let days = &snapshot.days;
        if let (Some(first), Some(last)) = (days.first(), days.last()) {
            self.forecast_title.set_text(&format!("{} · {}–{}", tr!("Sunday–Saturday"), strftime(first.date, "%b %d"),
                                                  strftime(last.date, "%b %d")));
        }
        for day in days {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("weather-day");
            let date_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
            date_box.set_size_request(60, -1);
            date_box.append(&label(&strftime(day.date, "%a"), Some("row-title")));
            date_box.append(&label(&strftime(day.date, "%b %d"), Some("row-detail")));
            row.append(&date_box);
            row.append(&image(if day.code.is_some() { weather_icon(day.code, true) } else { "weather-severe-alert-symbolic" }, 24));
            let description = match day.code {
                Some(_) => tessel_core::i18n::translate(weather_description(day.code)),
                None => tr!("Forecast unavailable"),
            };
            let description_label = label(&description, None);
            description_label.set_hexpand(true);
            description_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            description_label.set_max_width_chars(18);
            description_label.set_tooltip_text(Some(&description));
            row.append(&description_label);
            row.append(&label(&format!("{} / {}", format_temperature(day.temp_max, units), format_temperature(day.temp_min, units)), None));
            if let Some(probability) = day.precip_probability {
                row.set_tooltip_text(Some(&format!("{description} · {}{}%", tr!("Chance of precipitation: "), fixed0(probability))));
            }
            self.forecast.append(&row);
            self.day_rows.borrow_mut().push(row);
        }
        let mut parts: Vec<String> = Vec::new();
        if snapshot.stale {
            parts.push(tr!("Showing saved weather"));
        }
        if let Some(fetched) = snapshot.fetched_at.filter(|f| *f != 0.0) {
            if let Some(local) = dates::machine_local(fetched) {
                parts.push(tr!("Updated ") + &local.format("%a %H:%M").map(|s| s.to_string()).unwrap_or_default());
            }
        }
        if let Some(error) = &snapshot.error {
            parts.push(error.clone());
        }
        self.message.set_text(&parts.join(" · "));
    }

    pub fn show(&self) { self.controller().show(); }
    pub fn activate(&self) { self.controller().activate(); }
    pub fn dismiss(&self) { self.controller().dismiss(); }

    pub fn destroy(&self) {
        self.close();
        self.window.destroy();
    }

    pub fn close(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        self.worker.close();
        if let Some(id) = self.timer.take() {
            id.remove();
        }
        if let Some(close) = self.style_close.take() {
            close();
        }
    }
}

fn location_text(config: &WeatherConfig) -> String {
    if has_location(config) { config.name.clone() } else { tr!("No location chosen") }
}
