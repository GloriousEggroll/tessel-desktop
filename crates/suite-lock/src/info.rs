//! `lock-screen/lock_info.py`: weather content drawn inside the lock surface,
//! without opening other windows.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use suite_core::tr;
use suite_gtk::utils::image;
use weather_manager::backend::{format_temperature, weather_description, weather_icon, WeatherConfig, WeatherSnapshot};
use weather_manager::dates::Date;

use crate::weather::LockWeather;

struct Inner {
    widget: gtk::Box,
    location_group: gtk::Box,
    current_group: gtk::Box,
    forecast_group: gtk::Box,
    forecast_columns: Cell<u32>,
    days: RefCell<Option<gtk::FlowBox>>,
    compact: Cell<bool>,
    signature: RefCell<Option<(Option<WeatherConfig>, Option<WeatherSnapshot>, bool, bool)>>,
}

/// `WeatherView`: location + current conditions (`widget()`, a vertical box
/// with class `lock-information`) and a separately placed 7-day forecast
/// (`forecast_group()`, also `lock-information`).
#[derive(Clone)]
pub struct WeatherView(Rc<Inner>);

impl Default for WeatherView {
    fn default() -> Self { Self::new() }
}

fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(suite_core::i18n::translate(text)).xalign(0.0).build();
    label.add_css_class("lock-group-heading");
    label
}

fn local_date(date: Date) -> Option<glib::DateTime> {
    glib::DateTime::from_local(date.year, date.month as i32, date.day as i32, 12, 0, 0.0).ok()
}

fn strftime(date: Date, format: &str) -> String {
    local_date(date).and_then(|d| d.format(format).ok()).map(|s| s.to_string()).unwrap_or_default()
}

impl WeatherView {
    pub fn new() -> Self {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(30).visible(false).build();
        widget.add_css_class("lock-information");
        let location_group = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let current_group = gtk::Box::new(gtk::Orientation::Vertical, 12);
        widget.append(&location_group);
        widget.append(&current_group);
        // The dashboard places the forecast independently from the location
        // and current conditions, so it can use the lower part of the screen.
        let forecast_group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).visible(false).build();
        forecast_group.add_css_class("lock-information");
        Self(Rc::new(Inner { widget, location_group, current_group, forecast_group, forecast_columns: Cell::new(1),
                             days: RefCell::new(None), compact: Cell::new(false), signature: RefCell::new(None) }))
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.widget }
    pub fn forecast_group(&self) -> &gtk::Box { &self.0.forecast_group }
    pub fn location_group(&self) -> &gtk::Box { &self.0.location_group }
    pub fn current_group(&self) -> &gtk::Box { &self.0.current_group }
    pub fn days(&self) -> Option<gtk::FlowBox> { self.0.days.borrow().clone() }
    pub fn forecast_columns(&self) -> u32 { self.0.forecast_columns.get() }
    pub fn is_compact(&self) -> bool { self.0.compact.get() }
    /// `get_visible()` of the view (the property, like Python).
    pub fn get_visible(&self) -> bool { self.0.widget.get_visible() }
    pub fn is_visible(&self) -> bool { self.0.widget.get_visible() }

    pub fn set_compact(&self, compact: bool) {
        self.0.compact.set(compact);
        self.0.forecast_group.set_spacing(if compact { 12 } else { 18 });
        if let Some(days) = self.0.days.borrow().as_ref() {
            let mut child = days.first_child();
            while let Some(item) = child {
                if let Some(entry) = item.downcast_ref::<gtk::FlowBoxChild>().and_then(|c| c.child()) {
                    if let Some(column) = entry.downcast_ref::<gtk::Box>() {
                        column.set_spacing(if compact { 4 } else { 8 });
                        if let Some(icon) = column.first_child().and_then(|w| w.next_sibling()).and_downcast::<gtk::Image>() {
                            icon.set_pixel_size(if compact { 28 } else { 36 });
                        }
                    }
                }
                child = item.next_sibling();
            }
        }
    }

    pub fn set_forecast_columns(&self, columns: u32) {
        self.0.forecast_columns.set(columns);
        if let Some(days) = self.0.days.borrow().as_ref() {
            days.set_min_children_per_line(columns);
        }
    }

    /// `update(feed, current, forecast)`: rebuilds only when the feed's
    /// config/snapshot or the two switches changed.
    pub fn update(&self, feed: &LockWeather, current: bool, forecast: bool) {
        self.update_with(feed.config(), feed.snapshot(), current, forecast);
    }

    pub fn update_with(&self, config: Option<WeatherConfig>, snapshot: Option<WeatherSnapshot>, current: bool, forecast: bool) {
        let inner = &self.0;
        let signature = (config.clone(), snapshot.clone(), current, forecast);
        if inner.signature.borrow().as_ref() == Some(&signature) {
            return;
        }
        inner.signature.replace(Some(signature));
        inner.days.replace(None);
        for group in [&inner.location_group, &inner.current_group, &inner.forecast_group] {
            while let Some(child) = group.first_child() {
                group.remove(&child);
            }
        }
        let configured = config.is_some();
        inner.widget.set_visible(configured && (current || forecast));
        inner.current_group.set_visible(configured && current);
        inner.forecast_group.set_visible(configured && forecast);
        let Some(config) = config.filter(|_| inner.widget.get_visible()) else { return };
        inner.location_group.append(&heading("Location"));
        let location = gtk::Label::builder().label(&config.name).wrap(true).max_width_chars(32).xalign(0.0).build();
        location.add_css_class("lock-location");
        inner.location_group.append(&location);
        let units = &config.units;
        if current {
            inner.current_group.append(&heading("Current weather"));
            let row = gtk::Box::builder().spacing(18).halign(gtk::Align::Start).build();
            row.add_css_class("lock-current-weather");
            if let Some(weather) = snapshot.as_ref().and_then(|s| s.current.as_ref()) {
                row.append(&image(weather_icon(Some(weather.code), weather.is_day), 64));
                let temperature = gtk::Label::builder().label(format_temperature(Some(weather.temp), units)).xalign(0.0).build();
                temperature.add_css_class("lock-temperature");
                let description = gtk::Box::new(gtk::Orientation::Vertical, 4);
                description.append(&temperature);
                let condition = gtk::Label::builder().label(suite_core::i18n::translate(weather_description(Some(weather.code))))
                    .wrap(true).max_width_chars(30).xalign(0.0).build();
                condition.add_css_class("lock-condition");
                description.append(&condition);
                row.append(&description);
            } else {
                row.append(&gtk::Label::new(Some(&if snapshot.is_some() { tr!("Weather unavailable") } else { tr!("Loading weather…") })));
            }
            inner.current_group.append(&row);
        }
        if forecast {
            inner.forecast_group.append(&heading("7-day forecast"));
        }
        match snapshot.as_ref() {
            Some(snapshot) if forecast => {
                let days = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).homogeneous(true)
                    .min_children_per_line(inner.forecast_columns.get()).max_children_per_line(7).column_spacing(18)
                    .row_spacing(18).halign(gtk::Align::Start).build();
                for day in &snapshot.days {
                    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
                    column.add_css_class("lock-forecast-day");
                    column.append(&gtk::Label::new(Some(&format!("{} {}", strftime(day.date, "%a"), day.date.day))));
                    column.append(&image(weather_icon(day.code, true), 36));
                    let high = gtk::Label::new(Some(&format_temperature(day.temp_max, units)));
                    high.add_css_class("lock-forecast-high");
                    column.append(&high);
                    let low = gtk::Label::new(Some(&format_temperature(day.temp_min, units)));
                    low.add_css_class("lock-caption");
                    column.append(&low);
                    let label = format!("{}, {}", strftime(day.date, "%A"), suite_core::i18n::translate(weather_description(day.code)));
                    column.update_property(&[gtk::accessible::Property::Label(&label)]);
                    let child = gtk::FlowBoxChild::builder().child(&column).focusable(false).build();
                    days.append(&child);
                }
                inner.forecast_group.append(&days);
                inner.days.replace(Some(days));
            }
            _ if forecast => {
                inner.forecast_group.append(&gtk::Label::builder().label(tr!("Loading forecast…")).xalign(0.0).build());
            }
            _ => {}
        }
        if let Some(snapshot) = snapshot.as_ref().filter(|s| s.stale || s.error.is_some()) {
            let message = if snapshot.current.is_none() { "Weather unavailable" } else { "Saved weather · waiting for an update" };
            let label = gtk::Label::builder().label(suite_core::i18n::translate(message)).xalign(0.0).wrap(true).max_width_chars(40).build();
            label.add_css_class("lock-caption");
            if current { inner.current_group.append(&label) } else { inner.forecast_group.append(&label) }
        }
        self.set_compact(inner.compact.get());
    }
}
