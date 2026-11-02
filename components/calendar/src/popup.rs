//! Compact, navigable calendar menu for the dock/Waybar clock (calendar_popup.py).

use crate::civil::Date;
use crate::events::Local;
use crate::store::EventStore;
use crate::view::{full_date, install_css, label, margins, month_abbr, strftime, style, CalendarView, Marker, Wall, STYLE_CSS};
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::clock::ClockPreferencesStore;
use tessel_core::tr;
use tessel_gtk::popup::{header_button, popup_header, set_popup_width, HeaderAction, PopupController, POPUP_PADDING, POPUP_WIDTH};

pub struct PopupInner {
    pub window: gtk::ApplicationWindow,
    pub settings_button: gtk::Button,
    pub today_label: gtk::Label,
    pub calendar_view: CalendarView,
    pub events_box: gtk::Box,
    pub events_heading: gtk::Label,
    pub event_list: gtk::Box,
    pub events_scroll: gtk::ScrolledWindow,
    pub events_status: gtk::Label,
    pub controller: RefCell<Option<PopupController>>,
    events: Option<EventStore>,
    today_provider: Rc<dyn Fn() -> Date>,
    open_settings: Rc<dyn Fn()>,
    css: gtk::CssProvider,
    destroyed: Cell<bool>,
    refresh_source: RefCell<Option<glib::SourceId>>,
    clock: RefCell<ClockPreferencesStore>,
}

#[derive(Clone)]
pub struct CalendarPopup(pub Rc<PopupInner>);

fn marker_for(events: &EventStore) -> Marker {
    let events = events.clone();
    Rc::new(move |first, last| events.range(first, last).keys().copied().collect())
}

impl CalendarPopup {
    pub fn new(application: &gtk::Application, open_settings: Rc<dyn Fn()>, edge: &str, offset: i32,
               today_provider: Rc<dyn Fn() -> Date>, events: Option<EventStore>, first_weekday: Option<i64>) -> Self {
        let window = gtk::ApplicationWindow::new(application);
        window.set_title(Some(&tr!("Calendar")));
        set_popup_width(window.upcast_ref(), POPUP_WIDTH);
        style(&window, &["calendarmanager"]);
        let css = install_css(&WidgetExt::display(&window), STYLE_CSS);
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("popup-surface");
        window.set_child(Some(&surface));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, POPUP_PADDING);
        surface.append(&content);

        let settings_button = header_button("emblem-system-symbolic", &tr!("Date settings"), None, false);
        let heading = popup_header(&["view-calendar-symbolic", "x-office-calendar-symbolic"], &tr!("Calendar"), Some(" "),
                                   vec![HeaderAction::Widget(settings_button.clone().upcast())]);
        let today_label = heading.subtitle_label.clone();
        tessel_gtk::utils::append(&content, &heading.root, false, false);

        let marker = events.as_ref().map(marker_for);
        let calendar_view = CalendarView::new(None, None, today_provider.clone(), first_weekday, marker);
        tessel_gtk::utils::append(&content, calendar_view.widget(), false, false);

        let events_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let events_heading = label("", Some("section-title"));
        tessel_gtk::utils::append(&events_box, &events_heading, false, false);
        let event_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        event_list.add_css_class("event-list");
        let events_scroll = gtk::ScrolledWindow::new();
        events_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        events_scroll.set_propagate_natural_height(true);
        events_scroll.set_max_content_height(220);
        events_scroll.set_child(Some(&event_list));
        tessel_gtk::utils::append(&events_box, &events_scroll, false, false);
        let events_status = label("", Some("muted"));
        events_status.set_ellipsize(pango::EllipsizeMode::None);
        events_status.set_wrap(true);
        events_status.set_wrap_mode(pango::WrapMode::WordChar);
        events_status.set_max_width_chars(40);
        tessel_gtk::utils::append(&events_box, &events_status, false, false);
        tessel_gtk::utils::append(&content, &events_box, false, false);

        let popup = CalendarPopup(Rc::new(PopupInner {
            window, settings_button, today_label, calendar_view, events_box, events_heading, event_list, events_scroll,
            events_status, controller: RefCell::new(None), events, today_provider, open_settings, css,
            destroyed: Cell::new(false), refresh_source: RefCell::new(None), clock: RefCell::new(ClockPreferencesStore::default()),
        }));
        let weak = popup.downgrade();
        popup.0.calendar_view.set_on_selected(Some(Rc::new(move |_| if let Some(p) = up(&weak) { p.show_events() })));
        let weak = popup.downgrade();
        popup.0.settings_button.connect_clicked(move |_| if let Some(p) = up(&weak) { p.on_settings() });
        let weak = popup.downgrade();
        let controller = PopupController::new(popup.0.window.upcast_ref(), move || if let Some(p) = up(&weak) { p.refresh() },
                                              edge, offset, "clock");
        popup.0.controller.replace(Some(controller));
        let weak = popup.downgrade();
        popup.0.window.connect_unrealize(move |_| if let Some(p) = up(&weak) { p.on_destroy() });
        let weak = popup.downgrade();
        let source = glib::timeout_add_seconds_local(30, move || match up(&weak) {
            Some(p) => {
                p.refresh();
                if p.0.destroyed.get() { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
            }
            None => glib::ControlFlow::Break,
        });
        popup.0.refresh_source.replace(Some(source));
        popup.refresh();
        popup
    }

    fn downgrade(&self) -> Weak<PopupInner> {
        Rc::downgrade(&self.0)
    }

    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }
    pub fn calendar_view(&self) -> &CalendarView { &self.0.calendar_view }
    pub fn controller(&self) -> Option<PopupController> { self.0.controller.borrow().clone() }

    /// Event sources finished loading or the configuration changed.
    pub fn events_changed(&self) {
        if !self.0.destroyed.get() {
            self.0.calendar_view.set_marker(self.0.events.as_ref().map(marker_for));
            self.show_events();
        }
    }

    pub fn set_first_weekday(&self, value: i64) {
        if !self.0.destroyed.get() {
            self.0.calendar_view.set_first_weekday(value);
        }
    }

    fn time_text(&self, value: crate::civil::Naive) -> String {
        // Follow the dock clock's 12/24-hour choice.
        self.0.clock.borrow_mut().load().format_time(&Wall(value))
    }

    fn event_row(&self, occurrence: &crate::events::Occurrence, day: Date) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("event-row");
        let when = if occurrence.all_day {
            tr!("All day")
        } else {
            match occurrence.start {
                Local::Time(start) if start.date() == day => self.time_text(start),
                _ => "…".to_owned(),
            }
        };
        let when_label = label(&when, Some("event-time"));
        when_label.set_valign(gtk::Align::Start);
        tessel_gtk::utils::append(&row, &when_label, false, false);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let untitled = tr!("Untitled event");
        let summary = if occurrence.summary.is_empty() { untitled.clone() } else { occurrence.summary.clone() };
        let title = label(&summary, Some("event-title"));
        title.set_tooltip_text((!occurrence.summary.is_empty()).then_some(occurrence.summary.as_str()));
        tessel_gtk::utils::append(&texts, &title, false, false);
        if !occurrence.location.is_empty() {
            let place = label(&occurrence.location, Some("event-location"));
            place.set_tooltip_text(Some(&occurrence.location));
            tessel_gtk::utils::append(&texts, &place, false, false);
        }
        tessel_gtk::utils::append(&row, &texts, true, false);
        tessel_gtk::utils::accessible(&row, &format!("{when}, {summary}"));
        row
    }

    pub fn show_events(&self) {
        if self.0.destroyed.get() {
            return;
        }
        let day = self.0.calendar_view.selected_date();
        self.0.events_box.set_visible(self.0.events.is_some());
        let Some(events) = self.0.events.as_ref() else { return };
        while let Some(child) = self.0.event_list.first_child() {
            self.0.event_list.remove(&child);
        }
        let today = (self.0.today_provider)();
        self.0.events_heading.set_text(&if day == today {
            tr!("Today")
        } else {
            format!("{}, {} {}", strftime(day, 0, 0, 0, "%A"), month_abbr(day.month()), day.day())
        });
        let items = events.on(day);
        for occurrence in &items {
            self.0.event_list.append(&self.event_row(occurrence, day));
        }
        self.0.events_scroll.set_visible(!items.is_empty());
        let mut status = if events.sources().is_empty() {
            tr!("No calendars added. Add one in Date settings.")
        } else if events.loading() && items.is_empty() {
            tr!("Loading events…")
        } else if items.is_empty() {
            tr!("No events")
        } else {
            String::new()
        };
        if !events.errors().is_empty() && !events.loading() {
            let failed = tr!("Some calendars could not be loaded.");
            status = crate::py::strip(&format!("{status}\n{failed}")).to_owned();
        }
        self.0.events_status.set_text(&status);
        self.0.events_status.set_visible(!status.is_empty());
    }

    pub fn refresh(&self) {
        if self.0.destroyed.get() {
            return;
        }
        let view = &self.0.calendar_view;
        let following_today = view.selected_date() == view.today();
        view.refresh_today();
        if following_today && view.selected_date() != view.today() {
            view.set_date(view.today());
        }
        self.0.today_label.set_text(&full_date((self.0.today_provider)()));
        self.show_events();
    }

    fn on_settings(&self) {
        self.dismiss();
        (self.0.open_settings)();
    }

    pub fn show(&self) {
        if !self.0.destroyed.get() {
            if let Some(c) = self.controller() { c.show() }
        }
    }

    pub fn activate(&self) {
        if !self.0.destroyed.get() {
            if let Some(c) = self.controller() { c.activate() }
        }
    }

    pub fn dismiss(&self) {
        if !self.0.destroyed.get() {
            if let Some(c) = self.controller() { c.dismiss() }
        }
    }

    pub fn destroy(&self) {
        if !self.0.destroyed.get() {
            self.0.window.destroy();
        }
    }

    pub fn is_destroyed(&self) -> bool {
        self.0.destroyed.get()
    }

    fn on_destroy(&self) {
        self.0.destroyed.set(true);
        if let Some(source) = self.0.refresh_source.take() {
            crate::view::remove_source(source);
        }
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.0.window), &self.0.css);
    }
}

fn up(weak: &Weak<PopupInner>) -> Option<CalendarPopup> {
    weak.upgrade().map(CalendarPopup)
}
