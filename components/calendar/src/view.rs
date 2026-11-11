//! Reusable month calendar and shared styling (calendar_view.py).

use crate::backend::ZonedTime;
use crate::civil::{days_in_month, Date, Naive, MAX_ORDINAL};
use crate::preferences::CalendarPreferencesStore;
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use tessel_core::tr;

pub const STYLE_CSS: &str = r#"
@define-color calendar_accent @theme_selected_bg_color;
@define-color calendar_surface mix(@theme_base_color, @theme_bg_color, 0.18);
@define-color calendar_muted alpha(@theme_fg_color, 0.62);
@define-color calendar_line alpha(@theme_fg_color, 0.10);
window.calendarmanager { background-color: @theme_bg_color; }
.calendarmanager headerbar {
    background-image: none; background-color: @theme_bg_color;
    border: none; box-shadow: none; padding: 8px 14px;
}
.calendarmanager .title { font-size: 24pt; font-weight: 700; }
.calendarmanager .subtitle, .calendarmanager .muted { color: @calendar_muted; }
.calendarmanager .subtitle { font-size: 10pt; }
.calendarmanager .section-title { font-size: 12pt; font-weight: 600; }
.calendarmanager .card {
    background-color: @calendar_surface; border: 1px solid @calendar_line;
    border-radius: 8px;
}
.calendarmanager button:not(.titlebutton) {
    background-image: none; background-color: @calendar_surface;
    color: @theme_fg_color; border: 1px solid @calendar_line;
    border-radius: 8px; box-shadow: none; text-shadow: none;
    min-height: 28px; padding: 4px 12px;
}
.calendarmanager button:hover {
    background-color: mix(@calendar_surface, @theme_fg_color, 0.06);
}
.calendarmanager button.primary {
    background-color: @calendar_accent;
    color: @theme_selected_fg_color; border-color: transparent;
}
.calendarmanager button.icon-button {
    padding: 4px 7px; min-width: 24px;
    background-color: transparent; border-color: transparent;
}
.calendarmanager button.icon-button:hover {
    background-color: alpha(@theme_fg_color, 0.07);
}
.calendarmanager button:focus-visible {
    outline: 2px solid alpha(@calendar_accent, 0.65); outline-offset: -2px;
}
.calendarmanager button:disabled { opacity: 0.45; }
.calendarmanager entry {
    border-radius: 8px; border-color: @calendar_line; padding: 8px;
}
.calendarmanager .status { font-size: 9pt; color: @calendar_muted; }
.calendarmanager .error { color: #ed858c; }
.calendarmanager .calendar-month { font-size: 12pt; font-weight: 600; }
.calendarmanager .calendar-weekday { font-size: 9pt; color: @calendar_muted; padding: 6px 0; }
.calendarmanager button.calendar-day {
    border-radius: 8px; border: 1px solid transparent;
    background-color: transparent; min-width: 28px; min-height: 30px;
    padding: 3px 2px; font-size: 10pt;
}
.calendarmanager button.calendar-day:hover {
    background-color: alpha(@calendar_accent, 0.12);
}
.calendarmanager button.calendar-day.adjacent { color: alpha(@theme_fg_color, 0.36); }
.calendarmanager button.calendar-day.has-events {
    background-image: radial-gradient(circle 2px at 50% 88%, @calendar_accent 100%, transparent 100%);
}
.calendarmanager button.calendar-day.selected.has-events {
    background-image: radial-gradient(circle 2px at 50% 88%, @theme_selected_fg_color 100%, transparent 100%);
}
.calendarmanager .event-list { margin-top: 2px; }
.calendarmanager .event-row {
    background-color: @calendar_surface; border: 1px solid @calendar_line;
    border-radius: 8px; padding: 7px 10px;
}
.calendarmanager .event-time { font-size: 9pt; color: @calendar_muted; min-width: 64px; }
.calendarmanager .event-title { font-weight: 600; }
.calendarmanager .event-location { font-size: 9pt; color: @calendar_muted; }
.calendarmanager button.calendar-day.today {
    color: @calendar_accent; border-color: alpha(@calendar_accent, 0.6);
    font-weight: 700;
}
.calendarmanager button.calendar-day.selected {
    background-color: @calendar_accent; color: @theme_selected_fg_color;
    border-color: @calendar_accent; font-weight: 700;
}
.calendarmanager .calendar-footer { font-size: 9pt; }
window.calendarmanager.tray-popup { background-color: transparent; }
.calendarmanager .popup-surface {
    background-color: @theme_bg_color; border-radius: 8px;
    border: 1px solid alpha(@theme_fg_color, 0.14);
}
.calendarmanager .calendar-tile {
    min-width: 42px; min-height: 42px; border-radius: 8px;
    color: @calendar_accent; background-color: alpha(@calendar_accent, 0.12);
}
"#;

// ------------------------------------------------------- locale strftime

/// C `strftime` with the current locale (Python `date.strftime`).
pub fn strftime(date: Date, hour: i64, minute: i64, second: i64, format: &str) -> String {
    let (y, m, d) = date.parts();
    // SAFETY: a zeroed tm is valid; fields are set below.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = (y - 1900) as i32;
    tm.tm_mon = (m - 1) as i32;
    tm.tm_mday = d as i32;
    tm.tm_hour = hour as i32;
    tm.tm_min = minute as i32;
    tm.tm_sec = second as i32;
    tm.tm_wday = ((date.weekday() + 1) % 7) as i32;
    tm.tm_yday = (date.0 - Date::ymd(y, 1, 1).0) as i32;
    tm.tm_isdst = -1;
    let Ok(format) = std::ffi::CString::new(format) else { return String::new() };
    let mut buffer = vec![0u8; 512];
    // SAFETY: buffer and format are valid for the call.
    let length = unsafe { libc::strftime(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len(), format.as_ptr(), &tm) };
    String::from_utf8_lossy(&buffer[..length]).into_owned()
}

/// `calendar.day_name[weekday]` (Monday = 0).
pub fn day_name(weekday: i64) -> String {
    strftime(Date::ymd(2001, 1, 1 + weekday), 0, 0, 0, "%A")
}

pub fn day_abbr(weekday: i64) -> String {
    strftime(Date::ymd(2001, 1, 1 + weekday), 0, 0, 0, "%a")
}

pub fn month_name(month: i64) -> String {
    strftime(Date::ymd(2001, month, 1), 0, 0, 0, "%B")
}

pub fn month_abbr(month: i64) -> String {
    strftime(Date::ymd(2001, month, 1), 0, 0, 0, "%b")
}

/// `"%A, <month name> <day>"`.
pub fn full_date(value: Date) -> String {
    format!("{}, {} {}", strftime(value, 0, 0, 0, "%A"), month_name(value.month()), value.day())
}

impl tessel_core::clock::ClockTime for ZonedTime {
    fn hour(&self) -> i32 { self.wall.hour() as i32 }
    fn minute(&self) -> i32 { self.wall.minute() as i32 }
    fn day(&self) -> i32 { self.wall.date().day() as i32 }
    fn month(&self) -> i32 { self.wall.date().month() as i32 }
    fn year(&self) -> i32 { self.wall.date().year() as i32 }
    fn strftime(&self, format: &str) -> String {
        strftime(self.wall.date(), self.wall.hour(), self.wall.minute(), self.wall.second(), format)
    }
}

/// A naive local wall time for clock formatting.
pub struct Wall(pub Naive);

impl tessel_core::clock::ClockTime for Wall {
    fn hour(&self) -> i32 { self.0.hour() as i32 }
    fn minute(&self) -> i32 { self.0.minute() as i32 }
    fn day(&self) -> i32 { self.0.date().day() as i32 }
    fn month(&self) -> i32 { self.0.date().month() as i32 }
    fn year(&self) -> i32 { self.0.date().year() as i32 }
    fn strftime(&self, format: &str) -> String {
        strftime(self.0.date(), self.0.hour(), self.0.minute(), self.0.second(), format)
    }
}

// ----------------------------------------------------------- widget helpers

pub fn style<W: IsA<gtk::Widget>>(widget: &W, names: &[&str]) -> W {
    for name in names {
        widget.add_css_class(name);
    }
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        tessel_gtk::theme::attach(window, None);
    }
    widget.clone()
}

pub fn margins<W: IsA<gtk::Widget>>(widget: &W, value: i32) {
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
    widget.set_margin_start(value);
    widget.set_margin_end(value);
}

pub fn icon(name: &str, size: i32) -> gtk::Image {
    let widget = gtk::Image::from_icon_name(name);
    widget.set_pixel_size(size);
    widget
}

pub fn label(text: &str, css: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::builder().label(text).xalign(0.0).build();
    widget.set_ellipsize(pango::EllipsizeMode::End);
    if let Some(css) = css {
        widget.add_css_class(css);
    }
    widget
}

pub fn install_css(display: &gdk::Display, css: &str) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    gtk::style_context_add_provider_for_display(display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    provider
}

/// `GLib.source_remove` only when the source still exists.
pub fn remove_source(source: glib::SourceId) {
    if glib::MainContext::default().find_source_by_id(&source).is_some() {
        source.remove();
    }
}

pub type Marker = Rc<dyn Fn(Date, Date) -> HashSet<Date>>;

pub struct ViewInner {
    pub widget: gtk::Box,
    pub previous_button: gtk::Button,
    pub next_button: gtk::Button,
    pub today_button: gtk::Button,
    pub month_label: gtk::Label,
    pub grid: gtk::Grid,
    pub headings: Vec<gtk::Label>,
    pub selection_label: gtk::Label,
    slots: Vec<gtk::Button>,
    slot_dates: RefCell<Vec<Option<Date>>>,
    pub day_buttons: RefCell<HashMap<Date, gtk::Button>>,
    first_weekday: Cell<i64>,
    marker: RefCell<Option<Marker>>,
    pub marked_days: RefCell<HashSet<Date>>,
    today_provider: Rc<dyn Fn() -> Date>,
    on_selected: RefCell<Option<Rc<dyn Fn(Date)>>>,
    today: Cell<Date>,
    selected: Cell<Date>,
    display: Cell<(i64, i64)>,
}

/// A six-week calendar. Selecting or browsing dates never changes the clock.
#[derive(Clone)]
pub struct CalendarView(pub Rc<ViewInner>);

impl CalendarView {
    pub fn new(selected: Option<Date>, on_selected: Option<Rc<dyn Fn(Date)>>, today_provider: Rc<dyn Fn() -> Date>,
               first_weekday: Option<i64>, marker: Option<Marker>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 10);
        widget.add_css_class("calendar-view");
        let first_weekday = first_weekday.unwrap_or_else(|| CalendarPreferencesStore::default().load().first_weekday());
        let today = today_provider();
        let selected = selected.unwrap_or(today);
        let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let navigation_button = |name: &str, tooltip: &str| {
            let button = gtk::Button::new();
            button.add_css_class("icon-button");
            button.set_child(Some(&icon(name, 18)));
            button.set_tooltip_text(Some(tooltip));
            tessel_gtk::utils::accessible(&button, tooltip);
            button
        };
        let previous_button = navigation_button("go-previous-symbolic", &tr!("Previous month"));
        let next_button = navigation_button("go-next-symbolic", &tr!("Next month"));
        let month_label = gtk::Label::new(None);
        month_label.add_css_class("calendar-month");
        month_label.set_hexpand(true);
        tessel_gtk::utils::append(&navigation, &previous_button, false, false);
        tessel_gtk::utils::append(&navigation, &month_label, true, false);
        tessel_gtk::utils::append(&navigation, &next_button, false, false);
        tessel_gtk::utils::append(&widget, &navigation, false, false);

        let grid = gtk::Grid::builder().column_spacing(3).row_spacing(3).column_homogeneous(true).build();
        let mut headings = Vec::new();
        for column in 0..7 {
            let heading = gtk::Label::new(None);
            heading.add_css_class("calendar-weekday");
            grid.attach(&heading, column, 0, 1, 1);
            headings.push(heading);
        }
        let mut slots = Vec::new();
        for position in 0..42 {
            let button = gtk::Button::new();
            button.add_css_class("calendar-day");
            button.add_css_class("flat");
            button.set_hexpand(true);
            grid.attach(&button, position % 7, position / 7 + 1, 1, 1);
            slots.push(button);
        }
        tessel_gtk::utils::append(&widget, &grid, false, false);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let today_button = gtk::Button::with_label(&tr!("Today"));
        today_button.set_tooltip_text(Some(&tr!("Go to today")));
        tessel_gtk::utils::append(&footer, &today_button, false, false);
        let selection_label = gtk::Label::builder().xalign(1.0).build();
        selection_label.add_css_class("muted");
        selection_label.add_css_class("calendar-footer");
        selection_label.set_ellipsize(pango::EllipsizeMode::End);
        selection_label.set_hexpand(true);
        tessel_gtk::utils::append(&footer, &selection_label, true, false);
        tessel_gtk::utils::append(&widget, &footer, false, false);

        let view = CalendarView(Rc::new(ViewInner {
            widget, previous_button, next_button, today_button, month_label, grid, headings, selection_label,
            slot_dates: RefCell::new(vec![None; 42]), slots, day_buttons: RefCell::default(),
            first_weekday: Cell::new(first_weekday), marker: RefCell::new(marker), marked_days: RefCell::default(),
            today_provider, on_selected: RefCell::new(on_selected), today: Cell::new(today), selected: Cell::new(selected),
            display: Cell::new((selected.year(), selected.month())),
        }));
        let weak = view.downgrade();
        view.0.previous_button.connect_clicked(move |_| if let Some(v) = up(&weak) { v.shift_month(-1) });
        let weak = view.downgrade();
        view.0.next_button.connect_clicked(move |_| if let Some(v) = up(&weak) { v.shift_month(1) });
        let weak = view.downgrade();
        view.0.today_button.connect_clicked(move |_| if let Some(v) = up(&weak) { v.go_today() });
        for (index, button) in view.0.slots.iter().enumerate() {
            let weak = view.downgrade();
            button.connect_clicked(move |_| if let Some(v) = up(&weak) { v.day_clicked(index) });
        }
        let weak = view.downgrade();
        tessel_gtk::utils::key_controller(&view.0.widget, false, move |_, key, _, state| {
            up(&weak).map_or(glib::Propagation::Proceed, |v| v.on_key_pressed(key, state))
        });
        view.render();
        view
    }

    fn downgrade(&self) -> Weak<ViewInner> {
        Rc::downgrade(&self.0)
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.widget }
    pub fn selected_date(&self) -> Date { self.0.selected.get() }
    pub fn today(&self) -> Date { self.0.today.get() }
    pub fn first_weekday(&self) -> i64 { self.0.first_weekday.get() }
    pub fn display_year(&self) -> i64 { self.0.display.get().0 }
    pub fn display_month(&self) -> i64 { self.0.display.get().1 }
    pub fn day_button(&self, day: Date) -> Option<gtk::Button> { self.0.day_buttons.borrow().get(&day).cloned() }
    pub fn marked_days(&self) -> HashSet<Date> { self.0.marked_days.borrow().clone() }

    pub fn set_on_selected(&self, callback: Option<Rc<dyn Fn(Date)>>) {
        self.0.on_selected.replace(callback);
    }

    pub fn set_first_weekday(&self, value: i64) {
        assert!((0..7).contains(&value), "First weekday must be 0 (Monday) to 6 (Sunday)");
        if value != self.0.first_weekday.get() {
            self.0.first_weekday.set(value);
            self.render();
        }
    }

    pub fn set_marker(&self, marker: Option<Marker>) {
        self.0.marker.replace(marker);
        self.render();
    }

    fn first_ordinal(&self) -> i64 {
        let (year, month) = self.0.display.get();
        let first = Date::ymd(year, month, 1);
        first.0 - (first.weekday() - self.0.first_weekday.get()).rem_euclid(7)
    }

    pub fn visible_range(&self) -> (Date, Date) {
        let ordinal = self.first_ordinal().max(1);
        (Date(ordinal), Date((ordinal + 41).min(MAX_ORDINAL)))
    }

    fn render(&self) {
        let first_weekday = self.0.first_weekday.get();
        for (index, heading) in self.0.headings.iter().enumerate() {
            let day = (first_weekday + index as i64) % 7;
            heading.set_label(&day_abbr(day).chars().take(2).collect::<String>());
            heading.set_tooltip_text(Some(&day_name(day)));
        }
        let first_ordinal = self.first_ordinal();
        let marker = self.0.marker.borrow().clone();
        let marked = match marker {
            Some(marker) => {
                let (a, b) = self.visible_range();
                marker(a, b)
            }
            None => HashSet::new(),
        };
        self.0.marked_days.replace(marked.clone());
        let (year, month) = self.0.display.get();
        self.0.month_label.set_text(&format!("{} {year}", month_name(month)));
        self.0.previous_button.set_sensitive((year, month) != (1, 1));
        self.0.next_button.set_sensitive((year, month) != (9999, 12));
        let (today, selected) = (self.0.today.get(), self.0.selected.get());
        let mut buttons = HashMap::new();
        let mut dates = self.0.slot_dates.borrow_mut();
        for (position, button) in self.0.slots.iter().enumerate() {
            let ordinal = first_ordinal + position as i64;
            let value = (1..=MAX_ORDINAL).contains(&ordinal).then_some(Date(ordinal));
            dates[position] = value;
            for name in ["today", "selected", "adjacent", "has-events"] {
                button.remove_css_class(name);
            }
            button.set_sensitive(value.is_some());
            button.set_focusable(value.is_some());
            let Some(value) = value else {
                button.set_label("");
                button.set_tooltip_text(None);
                tessel_gtk::utils::accessible(button, &tr!("Outside calendar range"));
                continue;
            };
            button.set_label(&value.day().to_string());
            let mut description = format!("{}, {}", full_date(value), value.year());
            if value == today {
                button.add_css_class("today");
                description += ", today";
            }
            if value == selected {
                button.add_css_class("selected");
                description += ", selected";
            }
            if value.month() != month {
                button.add_css_class("adjacent");
            }
            if marked.contains(&value) {
                button.add_css_class("has-events");
                description += &format!(", {}", tr!("has events"));
            }
            button.set_tooltip_text(Some(&description));
            tessel_gtk::utils::accessible(button, &description);
            buttons.insert(value, button.clone());
        }
        drop(dates);
        self.0.day_buttons.replace(buttons);
        self.0.selection_label.set_text(&format!("{} {}, {}", month_abbr(selected.month()), selected.day(), selected.year()));
    }

    pub fn set_date(&self, value: Date) {
        let changed = value != self.0.selected.get();
        self.0.selected.set(value);
        self.0.display.set((value.year(), value.month()));
        self.render();
        if changed {
            let callback = self.0.on_selected.borrow().clone();
            if let Some(callback) = callback {
                callback(value);
            }
        }
    }

    /// Navigate months, retaining the day where possible and clamping bounds.
    pub fn shift_month(&self, offset: i64) {
        let selected = self.0.selected.get();
        let index = ((selected.year() - 1) * 12 + selected.month() - 1 + offset).clamp(0, 9999 * 12 - 1);
        let (year, month) = (index / 12 + 1, index % 12 + 1);
        let day = selected.day().min(days_in_month(year, month));
        self.set_date(Date::ymd(year, month, day));
    }

    pub fn go_today(&self) {
        self.0.today.set((self.0.today_provider)());
        self.set_date(self.0.today.get());
    }

    /// Update today's highlight without changing the user's selection.
    pub fn refresh_today(&self) {
        let value = (self.0.today_provider)();
        if value != self.0.today.get() {
            self.0.today.set(value);
            self.render();
        }
    }

    fn day_clicked(&self, index: usize) {
        let value = self.0.slot_dates.borrow()[index];
        if let Some(value) = value {
            self.set_date(value);
            if let Some(button) = self.day_button(value) {
                button.grab_focus();
            }
        }
    }

    pub fn on_key_pressed(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if state.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return glib::Propagation::Proceed;
        }
        let offset = match key {
            gdk::Key::Left => Some(-1),
            gdk::Key::Right => Some(1),
            gdk::Key::Up => Some(-7),
            gdk::Key::Down => Some(7),
            _ => None,
        };
        if let Some(offset) = offset {
            let ordinal = (self.0.selected.get().0 + offset).clamp(1, MAX_ORDINAL);
            self.set_date(Date(ordinal));
        } else if key == gdk::Key::Page_Up || key == gdk::Key::Page_Down {
            let offset = if key == gdk::Key::Page_Up { -1 } else { 1 };
            self.shift_month(offset * if state.contains(gdk::ModifierType::CONTROL_MASK) { 12 } else { 1 });
        } else if key == gdk::Key::Home {
            self.go_today();
        } else {
            return glib::Propagation::Proceed;
        }
        if let Some(button) = self.day_button(self.0.selected.get()) {
            button.grab_focus();
        }
        glib::Propagation::Stop
    }
}

fn up(weak: &Weak<ViewInner>) -> Option<CalendarView> {
    weak.upgrade().map(CalendarView)
}
