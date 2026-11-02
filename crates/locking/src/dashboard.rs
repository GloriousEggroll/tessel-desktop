//! `tessel-lock/lock_dashboard.py`: centred wallpaper clock, status, media
//! and weather groups.
use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use tessel_core::tr;

use crate::info::WeatherView;

struct Inner {
    widget: gtk::Box,
    mode: RefCell<&'static str>,
    time_group: gtk::Box,
    hero: gtk::Box,
    media_group: gtk::Box,
    weather_group: gtk::Box,
    weather: WeatherView,
    status: Option<gtk::Widget>,
}

/// `WallpaperDashboard(clock, date, weather, media, assistant, status=None)`.
/// `widget()` is the outer vertical box; `hero()` holds `time_group()`,
/// `media_group()` and `weather_group()` (callers may insert more children
/// into `hero()`, as luna_clock does for timers/alarms).
#[derive(Clone)]
pub struct WallpaperDashboard(Rc<Inner>);

impl WallpaperDashboard {
    pub fn new(clock: &gtk::Label, date: &gtk::Label, weather: &WeatherView, media: &impl IsA<gtk::Widget>,
               assistant: &gtk::Label, status: Option<&gtk::Widget>) -> Self {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).vexpand(true).build();
        let time_group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).halign(gtk::Align::Center).build();
        for label in [clock, date, assistant] {
            label.set_xalign(0.5);
            label.set_halign(gtk::Align::Center);
            time_group.append(label);
        }
        assistant.set_margin_top(20);
        if let Some(status) = status {
            // Battery, notification count and fingerprint hint under the date.
            status.set_margin_top(14);
            time_group.insert_child_after(status, Some(date));
        }
        let hero = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).vexpand(true).build();
        hero.append(&time_group);
        let media_group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).halign(gtk::Align::Center)
            .visible(media.get_visible()).build();
        media_group.add_css_class("lock-information");
        let heading = gtk::Label::new(Some(&tr!("Media")));
        heading.add_css_class("lock-group-heading");
        media_group.append(&heading);
        media.set_halign(gtk::Align::Center);
        media_group.append(media);
        let group = media_group.downgrade();
        media.connect_visible_notify(move |media| {
            if let Some(group) = group.upgrade() {
                group.set_visible(media.get_visible());
            }
        });
        hero.append(&media_group);
        widget.append(&hero);
        let view = weather.widget();
        view.set_halign(gtk::Align::Start);
        view.set_orientation(gtk::Orientation::Horizontal);
        view.set_spacing(40);
        let forecast = weather.forecast_group();
        forecast.set_halign(gtk::Align::Start);
        let weather_group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).halign(gtk::Align::Center)
            .visible(view.get_visible()).build();
        weather_group.append(view);
        weather_group.append(forecast);
        let group = weather_group.downgrade();
        view.connect_visible_notify(move |view| {
            if let Some(group) = group.upgrade() {
                group.set_visible(view.get_visible());
            }
        });
        hero.append(&weather_group);
        let dashboard = Self(Rc::new(Inner { widget, mode: RefCell::new("wide"), time_group, hero, media_group, weather_group,
                                             weather: weather.clone(), status: status.cloned() }));
        dashboard.configure(1280, 720);
        dashboard
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.widget }
    pub fn hero(&self) -> &gtk::Box { &self.0.hero }
    pub fn time_group(&self) -> &gtk::Box { &self.0.time_group }
    pub fn media_group(&self) -> &gtk::Box { &self.0.media_group }
    pub fn weather_group(&self) -> &gtk::Box { &self.0.weather_group }
    pub fn forecast(&self) -> &gtk::Box { self.0.weather.forecast_group() }
    pub fn weather(&self) -> &WeatherView { &self.0.weather }
    pub fn status(&self) -> Option<&gtk::Widget> { self.0.status.as_ref() }
    /// "wide" (≥1200 px), "landscape" (≥800) or "portrait".
    pub fn mode(&self) -> &'static str { *self.0.mode.borrow() }

    pub fn configure(&self, width: i32, height: i32) {
        let inner = &self.0;
        inner.mode.replace(if width >= 1200 { "wide" } else if width >= 800 { "landscape" } else { "portrait" });
        let compact = height < 900;
        let mut margin = 28.max(80.min((width.min(height) as f64 * 0.05).round_ties_even() as i32));
        if compact {
            // Keep the group spacing intact while fitting the whole centred
            // stack on a shorter display.
            margin = 22;
        }
        inner.widget.set_margin_top(margin);
        inner.widget.set_margin_bottom(margin);
        inner.widget.set_margin_start(margin);
        inner.widget.set_margin_end(margin);
        inner.hero.set_spacing(if compact { 36 } else { 56 });
        inner.weather_group.set_spacing(if compact { 20 } else { 28 });
        let view = inner.weather.widget();
        view.set_orientation(if width < 600 { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        view.set_spacing(if width < 600 { 24 } else { 40 });
        inner.weather.set_forecast_columns(if width >= 800 { 7 } else { 1 });
        inner.weather.set_compact(compact);
    }
}
