//! Date, time and timezone settings using the desktop's authorized time
//! service, plus the dock clock and calendar preferences (date_settings.py).
#![allow(deprecated)] // ComboBoxText/EntryCompletion/ListStore: kept for parity with the GTK4 Python UI.

use crate::backend::{Clock, DateSystem};
use crate::civil::Date;
use crate::preferences::{locale_week_start, os_error_text, CalendarPreferencesStore, PreferencesError, MONDAY, SATURDAY, SUNDAY, WEEK_STARTS};
use crate::view::{day_name, install_css, label, margins, style, CalendarView, STYLE_CSS};
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::clock::{ClockPreferencesStore, StoreError, DATE_ORDERS, DATE_SEPARATORS};
use tessel_core::tr;

pub const SETTINGS_CSS: &str = r#"
.calendarmanager spinbutton.time-field button {
    min-width: 18px; padding: 4px 6px;
}
.calendarmanager .clock-preview-time { font-size: 17pt; font-weight: 600; }
/* Even-width digits keep the line still as the seconds change. */
.calendarmanager .current-time { font-size: 14pt; font-weight: 600; font-feature-settings: "tnum"; }
.calendarmanager .clock-preview-date, .calendarmanager .clock-preview-weekday { font-size: 10pt; }
.calendarmanager .clock-preview-inline .clock-preview-time,
.calendarmanager .clock-preview-inline .clock-preview-date,
.calendarmanager .clock-preview-inline .clock-preview-weekday { font-size: 12pt; font-weight: 400; }
.calendarmanager .calendar-source-row {
    border-radius: 8px; border: 1px solid @calendar_line; padding: 4px 4px 4px 10px;
}
"#;

fn casefold(text: &str) -> String {
    text.to_lowercase().replace('ß', "ss")
}

pub struct SettingsInner {
    pub window: gtk::ApplicationWindow,
    pub backend: Rc<dyn DateSystem>,
    pub current_label: gtk::Label,
    pub ntp_switch: gtk::Switch,
    pub timezone_combo: gtk::ComboBoxText,
    pub timezone_entry: gtk::Entry,
    pub zone_model: gtk::ListStore,
    pub zone_completion: gtk::EntryCompletion,
    pub timezone_apply_button: gtk::Button,
    pub hour_spin: gtk::SpinButton,
    pub minute_spin: gtk::SpinButton,
    pub second_spin: gtk::SpinButton,
    pub manual_hint: gtk::Label,
    pub calendar_view: CalendarView,
    pub timezone_label: gtk::Label,
    pub apply_button: gtk::Button,
    pub status_label: gtk::Label,
    // Dock clock card.
    pub clock_preview_row: gtk::Box,
    pub clock_weekday_preview: gtk::Label,
    pub clock_preview: gtk::Box,
    pub clock_time_preview: gtk::Label,
    pub clock_date_preview: gtk::Label,
    pub clock_stack_switch: gtk::Switch,
    pub clock_show_date_switch: gtk::Switch,
    pub clock_show_year_switch: gtk::Switch,
    pub clock_show_weekday_switch: gtk::Switch,
    pub clock_reverse_switch: gtk::Switch,
    pub clock_hour_combo: gtk::DropDown,
    pub clock_date_combo: gtk::DropDown,
    pub clock_separator_combo: gtk::DropDown,
    pub clock_month_name_switch: gtk::Switch,
    pub clock_short_names_switch: gtk::Switch,
    pub clock_error_label: gtk::Label,
    // Calendar card.
    pub week_start_combo: gtk::DropDown,
    pub calendar_source_list: gtk::Box,
    pub calendar_source_entry: gtk::Entry,
    pub calendar_browse_button: gtk::Button,
    pub calendar_add_button: gtk::Button,
    pub calendar_error_label: gtk::Label,
    pub clock_preferences: RefCell<ClockPreferencesStore>,
    pub calendar_preferences: RefCell<CalendarPreferencesStore>,
    css: gtk::CssProvider,
    constructed: Cell<bool>,
    syncing: Cell<bool>,
    requested_date: Cell<Option<Date>>,
    requested_zone: RefCell<Option<String>>,
    pending_action: RefCell<Option<&'static str>>,
    saw_busy: Cell<bool>,
    date_dirty: Cell<bool>,
    time_dirty: Cell<bool>,
    formatting_time: Cell<bool>,
    timezone_dirty: Cell<bool>,
    zone_choices: RefCell<Option<Vec<String>>>,
    message: RefCell<String>,
    calendar_syncing: Cell<bool>,
    calendar_error: RefCell<String>,
    calendar_signature: RefCell<Option<Vec<crate::events::Source>>>,
    clock_syncing: Cell<bool>,
    clock_error: RefCell<String>,
    /// Updates the current time line each second while it is shown.
    seconds: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct DateSettingsWindow(pub Rc<SettingsInner>);

fn time_spin(maximum: f64, name: &str) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(0.0, maximum, 1.0);
    spin.add_css_class("time-field");
    spin.set_numeric(true);
    spin.set_wrap(true);
    spin.set_width_chars(2);
    spin.set_max_width_chars(2);
    tessel_gtk::utils::accessible(&spin, name);
    spin.set_tooltip_text(Some(name));
    spin
}

fn switch_row(content: &gtk::Box, title: &str, widget: &impl IsA<gtk::Widget>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let heading = label(title, None);
    heading.set_wrap(true);
    heading.set_ellipsize(pango::EllipsizeMode::None);
    tessel_gtk::utils::append(&row, &heading, true, false);
    widget.set_valign(gtk::Align::Center);
    tessel_gtk::utils::accessible(widget.upcast_ref::<gtk::Widget>(), title);
    tessel_gtk::utils::append(&row, widget, false, false);
    tessel_gtk::utils::append(content, &row, false, false);
}

fn card() -> (gtk::Box, gtk::Box) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    margins(&content, 14);
    tessel_gtk::utils::append(&card, &content, false, false);
    (card, content)
}

fn error_label() -> gtk::Label {
    let widget = label("", Some("error"));
    widget.set_wrap(true);
    widget.set_ellipsize(pango::EllipsizeMode::None);
    widget.set_visible(false);
    widget
}

fn dropdown(items: &[String]) -> gtk::DropDown {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    gtk::DropDown::from_strings(&refs)
}

impl DateSettingsWindow {
    pub fn new(application: &gtk::Application, backend: Rc<dyn DateSystem>) -> Self {
        Self::with_stores(application, backend, ClockPreferencesStore::default(), CalendarPreferencesStore::default())
    }

    pub fn with_stores(application: &gtk::Application, backend: Rc<dyn DateSystem>, clock: ClockPreferencesStore,
                       mut calendar: CalendarPreferencesStore) -> Self {
        let window = gtk::ApplicationWindow::new(application);
        window.set_title(Some(&tr!("Date Settings")));
        window.set_default_size(510, 790);
        window.set_size_request(470, 560);
        style(&window, &["calendarmanager"]);
        let css = install_css(&WidgetExt::display(&window), &format!("{STYLE_CSS}{SETTINGS_CSS}"));
        window.set_titlebar(Some(&tessel_gtk::utils::headerbar(&tr!("Date Settings"))));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        margins(&root, 22);
        window.set_child(Some(&root));
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 5);
        tessel_gtk::utils::append(&heading, &label(&tr!("Date & Time"), Some("title")), false, false);
        let current_label = label("", Some("current-time"));
        tessel_gtk::utils::append(&heading, &current_label, false, false);
        tessel_gtk::utils::append(&root, &heading, false, false);

        let automatic_card = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        automatic_card.add_css_class("card");
        let automatic_content = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        margins(&automatic_content, 14);
        tessel_gtk::utils::append(&automatic_card, &automatic_content, true, false);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 4);
        tessel_gtk::utils::append(&titles, &label(&tr!("Automatic date & time"), Some("section-title")), false, false);
        tessel_gtk::utils::append(&titles, &label(&tr!("Keep the clock synchronized online."), Some("muted")), false, false);
        tessel_gtk::utils::append(&automatic_content, &titles, true, false);
        let ntp_switch = gtk::Switch::new();
        ntp_switch.set_valign(gtk::Align::Center);
        tessel_gtk::utils::accessible(&ntp_switch, &tr!("Automatic date and time"));
        tessel_gtk::utils::append(&automatic_content, &ntp_switch, false, false);
        tessel_gtk::utils::append(&root, &automatic_card, false, false);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        let manual_content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        manual_content.set_margin_end(6);
        scroll.set_child(Some(&manual_content));
        tessel_gtk::utils::append(&root, &scroll, true, false);

        // Dock clock card.
        let (clock_card, content) = card();
        tessel_gtk::utils::append(&content, &label(&tr!("Dock clock"), Some("section-title")), false, false);
        let hint = label(&tr!("Changes apply immediately to the dock."), Some("muted"));
        hint.set_wrap(true);
        tessel_gtk::utils::append(&content, &hint, false, false);
        let clock_preview_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        clock_preview_row.set_halign(gtk::Align::Center);
        let clock_weekday_preview = gtk::Label::new(None);
        clock_weekday_preview.add_css_class("clock-preview-weekday");
        clock_weekday_preview.set_valign(gtk::Align::Center);
        clock_preview_row.append(&clock_weekday_preview);
        let clock_preview = gtk::Box::new(gtk::Orientation::Vertical, 0);
        clock_preview_row.append(&clock_preview);
        let clock_time_preview = gtk::Label::new(None);
        clock_time_preview.add_css_class("clock-preview-time");
        let clock_date_preview = gtk::Label::new(None);
        clock_date_preview.add_css_class("clock-preview-date");
        clock_preview.append(&clock_time_preview);
        clock_preview.append(&clock_date_preview);
        tessel_gtk::utils::append(&content, &clock_preview_row, false, false);
        let clock_stack_switch = gtk::Switch::new();
        switch_row(&content, &tr!("Stack time and date"), &clock_stack_switch);
        let clock_show_date_switch = gtk::Switch::new();
        switch_row(&content, &tr!("Show date"), &clock_show_date_switch);
        let clock_show_year_switch = gtk::Switch::new();
        switch_row(&content, &tr!("Show year"), &clock_show_year_switch);
        let clock_show_weekday_switch = gtk::Switch::new();
        switch_row(&content, &tr!("Show day of the week"), &clock_show_weekday_switch);
        let clock_reverse_switch = gtk::Switch::new();
        switch_row(&content, &tr!("Date before time"), &clock_reverse_switch);
        let clock_hour_combo = dropdown(&[tr!("12-hour"), tr!("24-hour")]);
        switch_row(&content, &tr!("Time format"), &clock_hour_combo);
        let clock_date_combo = dropdown(&[tr!("Month / Day / Year"), tr!("Day / Month / Year"), tr!("Year / Month / Day"),
                                          tr!("Year / Day / Month"), tr!("Month / Year / Day"), tr!("Day / Year / Month")]);
        switch_row(&content, &tr!("Date order"), &clock_date_combo);
        let clock_separator_combo = dropdown(&[tr!("Slash (/)"), tr!("Dash (-)"), tr!("Dot (.)")]);
        switch_row(&content, &tr!("Date separator"), &clock_separator_combo);
        let clock_month_name_switch = gtk::Switch::new();
        switch_row(&content, &tr!("Month as a name"), &clock_month_name_switch);
        let clock_short_names_switch = gtk::Switch::new();
        switch_row(&content, &tr!("Abbreviated names"), &clock_short_names_switch);
        let clock_error_label = error_label();
        tessel_gtk::utils::append(&content, &clock_error_label, false, false);
        tessel_gtk::utils::append(&manual_content, &clock_card, false, false);

        // Calendar card.
        let (calendar_card, content) = card();
        tessel_gtk::utils::append(&content, &label(&tr!("Calendar"), Some("section-title")), false, false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let week_heading = label(&tr!("First day of the week"), None);
        week_heading.set_wrap(true);
        week_heading.set_ellipsize(pango::EllipsizeMode::None);
        tessel_gtk::utils::append(&row, &week_heading, true, false);
        let automatic = tr!("Automatic ({day})", day = day_name(locale_week_start()));
        let week_start_combo = dropdown(&[automatic, day_name(SUNDAY), day_name(MONDAY), day_name(SATURDAY)]);
        week_start_combo.set_valign(gtk::Align::Center);
        tessel_gtk::utils::accessible(&week_start_combo, &tr!("First day of the week"));
        tessel_gtk::utils::append(&row, &week_start_combo, false, false);
        tessel_gtk::utils::append(&content, &row, false, false);
        tessel_gtk::utils::append(&content, &label(&tr!("Calendars"), Some("section-title")), false, false);
        let hint = label(&tr!("Show events from iCalendar (.ics) files or subscription addresses. Calendars are read-only."), Some("muted"));
        hint.set_wrap(true);
        hint.set_ellipsize(pango::EllipsizeMode::None);
        tessel_gtk::utils::append(&content, &hint, false, false);
        let calendar_source_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        tessel_gtk::utils::append(&content, &calendar_source_list, false, false);
        let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let calendar_source_entry = gtk::Entry::new();
        calendar_source_entry.set_placeholder_text(Some(&tr!("File path or https:// address")));
        calendar_source_entry.set_hexpand(true);
        tessel_gtk::utils::accessible(&calendar_source_entry, &tr!("Calendar file or address"));
        tessel_gtk::utils::append(&add_row, &calendar_source_entry, true, false);
        let calendar_browse_button = gtk::Button::new();
        calendar_browse_button.add_css_class("icon-button");
        calendar_browse_button.set_child(Some(&gtk::Image::from_icon_name("document-open-symbolic")));
        calendar_browse_button.set_tooltip_text(Some(&tr!("Choose a calendar file")));
        tessel_gtk::utils::accessible(&calendar_browse_button, &tr!("Choose a calendar file"));
        tessel_gtk::utils::append(&add_row, &calendar_browse_button, false, false);
        let calendar_add_button = gtk::Button::with_label(&tr!("Add"));
        calendar_add_button.set_tooltip_text(Some(&tr!("Add calendar")));
        tessel_gtk::utils::append(&add_row, &calendar_add_button, false, false);
        tessel_gtk::utils::append(&content, &add_row, false, false);
        let calendar_error_label = error_label();
        tessel_gtk::utils::append(&content, &calendar_error_label, false, false);
        tessel_gtk::utils::append(&manual_content, &calendar_card, false, false);

        tessel_gtk::utils::append(&manual_content, &label(&tr!("Time zone"), Some("section-title")), false, false);
        let zone_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let timezone_combo = gtk::ComboBoxText::with_entry();
        timezone_combo.set_hexpand(true);
        let timezone_entry: gtk::Entry = timezone_combo.child().and_downcast().expect("combo entry");
        timezone_entry.set_placeholder_text(Some(&tr!("Search by city or region")));
        timezone_entry.set_width_chars(20);
        tessel_gtk::utils::accessible(&timezone_entry, &tr!("Time zone"));
        let zone_model = gtk::ListStore::new(&[glib::Type::STRING]);
        let zone_completion = gtk::EntryCompletion::new();
        zone_completion.set_model(Some(&zone_model));
        zone_completion.set_text_column(0);
        zone_completion.set_minimum_key_length(1);
        zone_completion.set_match_func(|completion, key, iter| {
            let Some(model) = completion.model() else { return false };
            let text: String = model.get(iter, 0);
            casefold(&text).replace('_', " ").contains(&casefold(key).replace('_', " "))
        });
        timezone_entry.set_completion(Some(&zone_completion));
        let timezone_apply_button = gtk::Button::with_label(&tr!("Apply Zone"));
        timezone_apply_button.set_tooltip_text(Some(&tr!("Apply the selected time zone")));
        tessel_gtk::utils::append(&zone_row, &timezone_combo, true, false);
        tessel_gtk::utils::append(&zone_row, &timezone_apply_button, false, false);
        tessel_gtk::utils::append(&manual_content, &zone_row, false, false);
        tessel_gtk::utils::append(&manual_content, &label(&tr!("Time (24-hour)"), Some("section-title")), false, false);
        let clock_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let hour_spin = time_spin(23.0, &tr!("Hours"));
        let minute_spin = time_spin(59.0, &tr!("Minutes"));
        let second_spin = time_spin(59.0, &tr!("Seconds"));
        for (index, spin) in [&hour_spin, &minute_spin, &second_spin].into_iter().enumerate() {
            if index > 0 {
                tessel_gtk::utils::append(&clock_row, &gtk::Label::new(Some(":")), false, false);
            }
            tessel_gtk::utils::append(&clock_row, spin, false, false);
        }
        tessel_gtk::utils::append(&manual_content, &clock_row, false, false);
        let manual_hint = label("", Some("muted"));
        manual_hint.set_ellipsize(pango::EllipsizeMode::None);
        manual_hint.set_wrap(true);
        manual_hint.set_max_width_chars(48);
        tessel_gtk::utils::append(&manual_content, &manual_hint, false, false);
        tessel_gtk::utils::append(&manual_content, &label(&tr!("System date"), Some("section-title")), false, false);
        let today_backend = backend.clone();
        let calendar_view = CalendarView::new(Some(backend.now().date()), None, Rc::new(move || today_backend.now().date()),
                                              Some(calendar.load().first_weekday()), None);
        tessel_gtk::utils::append(&manual_content, calendar_view.widget(), false, false);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let timezone_label = label("", Some("muted"));
        tessel_gtk::utils::append(&footer, &timezone_label, true, false);
        let apply_button = gtk::Button::with_label(&tr!("Apply Date & Time"));
        apply_button.add_css_class("primary");
        tessel_gtk::utils::append(&footer, &apply_button, false, false);
        tessel_gtk::utils::append(&root, &footer, false, false);
        let status_label = label("", Some("muted"));
        status_label.set_ellipsize(pango::EllipsizeMode::None);
        status_label.set_wrap(true);
        status_label.set_max_width_chars(48);
        tessel_gtk::utils::append(&root, &status_label, false, false);

        let this = DateSettingsWindow(Rc::new(SettingsInner {
            window, backend, current_label, ntp_switch, timezone_combo, timezone_entry, zone_model, zone_completion,
            timezone_apply_button, hour_spin, minute_spin, second_spin, manual_hint, calendar_view, timezone_label, apply_button,
            status_label, clock_preview_row, clock_weekday_preview, clock_preview, clock_time_preview, clock_date_preview,
            clock_stack_switch, clock_show_date_switch, clock_show_year_switch, clock_show_weekday_switch, clock_reverse_switch,
            clock_hour_combo, clock_date_combo, clock_separator_combo, clock_month_name_switch, clock_short_names_switch,
            clock_error_label, week_start_combo, calendar_source_list, calendar_source_entry, calendar_browse_button,
            calendar_add_button, calendar_error_label, clock_preferences: RefCell::new(clock),
            calendar_preferences: RefCell::new(calendar), css, constructed: Cell::new(false), syncing: Cell::new(false),
            requested_date: Cell::new(None), requested_zone: RefCell::new(None), pending_action: RefCell::new(None),
            saw_busy: Cell::new(false), date_dirty: Cell::new(false), time_dirty: Cell::new(false),
            formatting_time: Cell::new(false), timezone_dirty: Cell::new(false), zone_choices: RefCell::new(None),
            message: RefCell::new(String::new()), calendar_syncing: Cell::new(false), calendar_error: RefCell::new(String::new()),
            calendar_signature: RefCell::new(None), clock_syncing: Cell::new(false), clock_error: RefCell::new(String::new()),
            seconds: RefCell::new(None),
        }));
        // The label is shown in this window or embedded in Global Settings.
        let weak = this.downgrade();
        this.0.current_label.connect_map(move |_| if let Some(w) = up(&weak) { w.tick_seconds() });
        let weak = this.downgrade();
        this.0.current_label.connect_unmap(move |_| if let Some(w) = up(&weak) { w.stop_seconds() });
        this.connect_signals();
        this.0.constructed.set(true);
        this.refresh();
        this
    }

    fn downgrade(&self) -> Weak<SettingsInner> {
        Rc::downgrade(&self.0)
    }

    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }

    /// `_destroyed(window)`: release the window's style provider. Global
    /// Settings embeds the controls of a window that is never realized, so
    /// its unrealize handler never runs.
    /// Show the current time and wake again just after the next second starts:
    /// a fixed one-second timer drifts and skips seconds.
    fn tick_seconds(&self) {
        self.stop_seconds();
        let current = self.0.backend.now();
        self.show_current_time(&current);
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_millis();
        let wait = std::time::Duration::from_millis(u64::from(1000 - since.min(999)) + 5);
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(wait, move || {
            if let Some(w) = up(&weak) {
                w.0.seconds.replace(None);
                w.tick_seconds();
            }
        });
        self.0.seconds.replace(Some(source));
    }

    fn stop_seconds(&self) {
        if let Some(source) = self.0.seconds.take() {
            source.remove();
        }
    }

    fn show_current_time(&self, current: &crate::backend::ZonedTime) {
        self.0.current_label.set_text(&crate::view::strftime(current.date(), current.hour(), current.minute(), current.second(),
                                                             "%A, %B %d, %Y · %H:%M:%S"));
    }

    pub fn destroyed(&self) {
        self.stop_seconds();
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.0.window), &self.0.css);
    }

    fn connect_signals(&self) {
        let s = &self.0;
        let weak = self.downgrade();
        s.ntp_switch.connect_state_set(move |_, enabled| up(&weak).map_or(glib::Propagation::Proceed, |w| w.ntp_requested(enabled)));
        let bools: [(&gtk::Switch, &'static str); 7] = [
            (&s.clock_stack_switch, "stacked"), (&s.clock_show_date_switch, "show_date"), (&s.clock_show_year_switch, "show_year"),
            (&s.clock_show_weekday_switch, "show_weekday"), (&s.clock_reverse_switch, "date_first"),
            (&s.clock_month_name_switch, "month_name"), (&s.clock_short_names_switch, "short_names"),
        ];
        for (switch, key) in bools {
            let weak = self.downgrade();
            switch.connect_active_notify(move |widget| if let Some(w) = up(&weak) { w.clock_changed(key, Value::Bool(widget.is_active())) });
        }
        let weak = self.downgrade();
        s.clock_hour_combo.connect_selected_notify(move |widget| if let Some(w) = up(&weak) {
            w.clock_changed("use_24_hour", Value::Bool(widget.selected() == 1))
        });
        let weak = self.downgrade();
        s.clock_date_combo.connect_selected_notify(move |widget| if let Some(w) = up(&weak) {
            if let Some(order) = DATE_ORDERS.get(widget.selected() as usize) { w.clock_changed("date_order", Value::from(*order)) }
        });
        let weak = self.downgrade();
        s.clock_separator_combo.connect_selected_notify(move |widget| if let Some(w) = up(&weak) {
            if let Some(separator) = DATE_SEPARATORS.get(widget.selected() as usize) { w.clock_changed("date_separator", Value::from(*separator)) }
        });
        let weak = self.downgrade();
        s.week_start_combo.connect_selected_notify(move |widget| if let Some(w) = up(&weak) { w.week_start_changed(widget.selected()) });
        let weak = self.downgrade();
        s.calendar_source_entry.connect_activate(move |_| if let Some(w) = up(&weak) { w.add_calendar_source() });
        let weak = self.downgrade();
        s.calendar_add_button.connect_clicked(move |_| if let Some(w) = up(&weak) { w.add_calendar_source() });
        let weak = self.downgrade();
        s.calendar_browse_button.connect_clicked(move |_| if let Some(w) = up(&weak) { w.browse_calendar_file() });
        let weak = self.downgrade();
        s.timezone_entry.connect_changed(move |_| if let Some(w) = up(&weak) { w.timezone_changed() });
        let weak = self.downgrade();
        s.timezone_apply_button.connect_clicked(move |_| if let Some(w) = up(&weak) { w.apply_timezone() });
        for spin in [&s.hour_spin, &s.minute_spin, &s.second_spin] {
            let weak = self.downgrade();
            spin.connect_output(move |spin| up(&weak).map_or(glib::Propagation::Proceed, |w| w.time_output(spin)));
            let weak = self.downgrade();
            spin.connect_changed(move |_| if let Some(w) = up(&weak) { w.time_changed() });
            let weak = self.downgrade();
            spin.connect_value_changed(move |_| if let Some(w) = up(&weak) { w.time_changed() });
        }
        let weak = self.downgrade();
        s.calendar_view.set_on_selected(Some(Rc::new(move |_| if let Some(w) = up(&weak) { w.date_selected() })));
        let weak = self.downgrade();
        s.apply_button.connect_clicked(move |_| if let Some(w) = up(&weak) { w.apply_date() });
        let weak = self.downgrade();
        s.window.connect_unrealize(move |window| if let Some(w) = up(&weak) {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(window), &w.0.css);
        });
    }

    // ------------------------------------------------------------ calendar

    fn week_start_changed(&self, selected: u32) {
        if self.0.calendar_syncing.get() {
            return;
        }
        let Some(value) = WEEK_STARTS.get(selected as usize) else { return };
        self.calendar_action(|store| store.set_week_start(value).map(|_| ()));
    }

    fn calendar_action(&self, action: impl FnOnce(&mut CalendarPreferencesStore) -> Result<(), PreferencesError>) -> bool {
        let result = action(&mut self.0.calendar_preferences.borrow_mut());
        let ok = match result {
            Ok(()) => {
                self.0.calendar_error.borrow_mut().clear();
                true
            }
            Err(PreferencesError::Invalid(message)) => {
                self.0.calendar_error.replace(tessel_core::i18n::translate(&message));
                false
            }
            Err(PreferencesError::Io(message)) => {
                self.0.calendar_error.replace(tr!("Could not save calendar preferences: {error}", error = message));
                false
            }
        };
        self.refresh_calendar_card();
        ok
    }

    pub fn add_calendar_source(&self) {
        let text = self.0.calendar_source_entry.text().to_string();
        if self.calendar_action(|store| store.add_source(&text, "").map(|_| ())) {
            self.0.calendar_source_entry.set_text("");
        }
    }

    fn remove_calendar_source(&self, location: &str) {
        self.calendar_action(|store| store.remove_source(location).map(|_| ()));
    }

    fn browse_calendar_file(&self) {
        let dialog = gtk::FileDialog::builder().title(tr!("Choose a calendar file")).modal(true).build();
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        let ics = gtk::FileFilter::new();
        ics.set_name(Some(&tr!("iCalendar files")));
        ics.add_pattern("*.ics");
        ics.add_mime_type("text/calendar");
        filters.append(&ics);
        dialog.set_filters(Some(&filters));
        let weak = self.downgrade();
        dialog.open(Some(&self.0.window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return }; // Dismissed.
            if let (Some(w), Some(path)) = (up(&weak), file.path()) {
                w.0.calendar_source_entry.set_text(&path.to_string_lossy());
                w.add_calendar_source();
            }
        });
    }

    pub fn refresh_calendar_card(&self) {
        let preferences = self.0.calendar_preferences.borrow_mut().load();
        self.0.calendar_syncing.set(true);
        let index = WEEK_STARTS.iter().position(|w| *w == preferences.week_start).unwrap_or(0);
        self.0.week_start_combo.set_selected(index as u32);
        self.0.calendar_syncing.set(false);
        self.0.calendar_view.set_first_weekday(preferences.first_weekday());
        let changed = self.0.calendar_signature.borrow().as_ref() != Some(&preferences.sources);
        if changed {
            self.0.calendar_signature.replace(Some(preferences.sources.clone()));
            let list = &self.0.calendar_source_list;
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            if preferences.sources.is_empty() {
                list.append(&label(&tr!("No calendars added."), Some("muted")));
            }
            for source in &preferences.sources {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.add_css_class("calendar-source-row");
                let image = gtk::Image::from_icon_name(if source.kind == "url" { "network-server-symbolic" } else { "x-office-calendar-symbolic" });
                tessel_gtk::utils::append(&row, &image, false, false);
                let text = label(if source.name.is_empty() { &source.location } else { &source.name }, None);
                text.set_ellipsize(pango::EllipsizeMode::Middle);
                text.set_tooltip_text(Some(&source.location));
                text.set_valign(gtk::Align::Center);
                tessel_gtk::utils::append(&row, &text, true, false);
                let remove = gtk::Button::new();
                remove.add_css_class("icon-button");
                remove.set_child(Some(&gtk::Image::from_icon_name("user-trash-symbolic")));
                remove.set_tooltip_text(Some(&tr!("Remove calendar")));
                tessel_gtk::utils::accessible(&remove, &tr!("Remove calendar"));
                let weak = self.downgrade();
                let location = source.location.clone();
                remove.connect_clicked(move |_| if let Some(w) = up(&weak) { w.remove_calendar_source(&location) });
                tessel_gtk::utils::append(&row, &remove, false, false);
                list.append(&row);
            }
        }
        let error = self.0.calendar_error.borrow().clone();
        self.0.calendar_error_label.set_text(&error);
        self.0.calendar_error_label.set_visible(!error.is_empty());
    }

    // --------------------------------------------------------------- clock

    fn clock_changed(&self, key: &str, value: Value) {
        if self.0.clock_syncing.get() {
            return;
        }
        let mut change = Map::new();
        change.insert(key.into(), value);
        let result = self.0.clock_preferences.borrow_mut().update(&change);
        match result {
            Ok(_) => self.0.clock_error.borrow_mut().clear(),
            Err(error) => {
                let path = self.0.clock_preferences.borrow().path.clone();
                let text = match &error {
                    StoreError::Io(io) => os_error_text(io, &path),
                    StoreError::Invalid(message) => message.clone(),
                };
                self.0.clock_error.replace(tr!("Could not save clock preferences: {error}", error = text));
            }
        }
        self.refresh_clock_display(&self.0.backend.now());
    }

    fn refresh_clock_display(&self, current: &crate::backend::ZonedTime) {
        let s = &self.0;
        let settings = s.clock_preferences.borrow_mut().load();
        s.clock_syncing.set(true);
        s.clock_stack_switch.set_active(settings.stacked);
        s.clock_show_date_switch.set_active(settings.show_date);
        s.clock_show_year_switch.set_active(settings.show_year);
        s.clock_show_weekday_switch.set_active(settings.show_weekday);
        s.clock_reverse_switch.set_active(settings.date_first);
        s.clock_hour_combo.set_selected(u32::from(settings.use_24_hour));
        s.clock_date_combo.set_selected(DATE_ORDERS.iter().position(|o| *o == settings.date_order).unwrap_or(0) as u32);
        s.clock_separator_combo.set_selected(DATE_SEPARATORS.iter().position(|o| *o == settings.date_separator).unwrap_or(0) as u32);
        s.clock_month_name_switch.set_active(settings.month_name);
        s.clock_short_names_switch.set_active(settings.short_names);
        s.clock_syncing.set(false);
        for control in [s.clock_show_year_switch.upcast_ref::<gtk::Widget>(), s.clock_reverse_switch.upcast_ref(),
                        s.clock_date_combo.upcast_ref(), s.clock_separator_combo.upcast_ref(), s.clock_month_name_switch.upcast_ref()] {
            control.set_sensitive(settings.show_date);
        }
        s.clock_separator_combo.set_sensitive(settings.show_date && !settings.month_name);
        s.clock_short_names_switch.set_sensitive(settings.show_weekday || (settings.show_date && settings.month_name));
        s.clock_preview.set_orientation(if settings.stacked { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        s.clock_preview.set_spacing(if settings.stacked { 0 } else { 8 });
        for widget in [s.clock_preview.upcast_ref::<gtk::Widget>(), s.clock_preview_row.upcast_ref()] {
            if settings.stacked { widget.remove_css_class("clock-preview-inline") } else { widget.add_css_class("clock-preview-inline") }
        }
        let (first, second) = if settings.date_first { (&s.clock_date_preview, &s.clock_time_preview) } else { (&s.clock_time_preview, &s.clock_date_preview) };
        s.clock_preview.reorder_child_after(first, None::<&gtk::Widget>);
        s.clock_preview.reorder_child_after(second, Some(first));
        s.clock_weekday_preview.set_text(&settings.format_weekday(current));
        s.clock_weekday_preview.set_visible(settings.show_weekday);
        s.clock_time_preview.set_text(&settings.format_time(current));
        s.clock_date_preview.set_text(&settings.format_date(current));
        s.clock_date_preview.set_visible(settings.show_date);
        let error = s.clock_error.borrow().clone();
        s.clock_error_label.set_text(&error);
        s.clock_error_label.set_visible(!error.is_empty());
    }

    // ---------------------------------------------------------- date/time

    fn date_selected(&self) {
        if self.0.syncing.get() {
            return;
        }
        self.0.date_dirty.set(true);
        self.0.message.borrow_mut().clear();
        if self.0.constructed.get() {
            self.refresh();
        }
    }

    fn time_output(&self, spin: &gtk::SpinButton) -> glib::Propagation {
        self.0.formatting_time.set(true);
        spin.set_text(&format!("{:02}", spin.value_as_int()));
        self.0.formatting_time.set(false);
        glib::Propagation::Stop
    }

    fn time_changed(&self) {
        if !self.0.syncing.get() && !self.0.formatting_time.get() && self.0.constructed.get() {
            self.0.time_dirty.set(true);
            self.0.message.borrow_mut().clear();
            self.refresh();
        }
    }

    pub fn chosen_time(&self) -> Option<Clock> {
        let s = &self.0;
        let mut fields = Vec::new();
        for spin in [&s.hour_spin, &s.minute_spin, &s.second_spin] {
            let text = spin.text().to_string();
            let field = crate::py::strip(&text);
            if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            fields.push(field.parse::<i64>().ok()?);
        }
        Clock::new(fields[0], fields[1], fields[2])
    }

    fn timezone_changed(&self) {
        if !self.0.syncing.get() && self.0.constructed.get() {
            let backend_zone = self.0.backend.state().timezone;
            self.0.timezone_dirty.set(self.chosen_zone().as_deref() != Some(backend_zone.as_str()));
            self.0.message.borrow_mut().clear();
            self.refresh();
        }
    }

    pub fn chosen_zone(&self) -> Option<String> {
        let text = casefold(crate::py::strip(&self.0.timezone_entry.text()));
        self.0.zone_choices.borrow().as_ref()?.iter().find(|z| casefold(z) == text).cloned()
    }

    fn update_zones(&self) {
        let state = self.0.backend.state();
        let mut choices: Vec<String> = state.timezones.clone();
        if !state.timezone.is_empty() {
            choices.push(state.timezone.clone());
        }
        choices.sort();
        choices.dedup();
        if self.0.zone_choices.borrow().as_ref() != Some(&choices) {
            let text = self.0.timezone_entry.text();
            self.0.timezone_combo.remove_all();
            self.0.zone_model.clear();
            for zone in &choices {
                self.0.timezone_combo.append(Some(zone), zone);
                self.0.zone_model.insert_with_values(None, &[(0, zone)]);
            }
            self.0.zone_choices.replace(Some(choices));
            self.0.timezone_entry.set_text(&text);
        }
        if !self.0.timezone_dirty.get() {
            self.0.timezone_combo.set_active_id(Some(&state.timezone));
            self.0.timezone_entry.set_text(&state.timezone);
        }
    }

    pub fn apply_timezone(&self) {
        let state = self.0.backend.state();
        if !state.ready || state.busy || state.ntp {
            return;
        }
        let Some(zone) = self.chosen_zone() else { return };
        if zone == state.timezone {
            return;
        }
        self.0.message.borrow_mut().clear();
        self.0.requested_zone.replace(Some(zone.clone()));
        self.0.pending_action.replace(Some("timezone"));
        self.0.saw_busy.set(false);
        if !self.0.backend.set_timezone(&zone) {
            self.0.pending_action.replace(None);
            self.0.requested_zone.replace(None);
        }
        self.refresh();
    }

    fn ntp_requested(&self, enabled: bool) -> glib::Propagation {
        if self.0.syncing.get() {
            return glib::Propagation::Proceed;
        }
        self.0.message.borrow_mut().clear();
        self.0.requested_date.set(None);
        self.0.pending_action.replace(None);
        let state = self.0.backend.state();
        if state.ready && state.can_ntp && !state.busy {
            self.0.backend.set_ntp(enabled);
        }
        self.refresh();
        glib::Propagation::Stop
    }

    pub fn apply_date(&self) {
        let state = self.0.backend.state();
        if !state.ready || state.busy || state.ntp {
            return;
        }
        let chosen_time = self.chosen_time();
        if self.chosen_zone().as_deref() != Some(state.timezone.as_str()) || chosen_time.is_none() {
            return;
        }
        self.0.message.borrow_mut().clear();
        let selected = self.0.calendar_view.selected_date();
        self.0.requested_date.set(Some(selected));
        let time_dirty = self.0.time_dirty.get();
        self.0.pending_action.replace(Some(if time_dirty { "datetime" } else { "date" }));
        self.0.saw_busy.set(false);
        let result = if time_dirty { self.0.backend.set_datetime(selected, chosen_time.unwrap()) } else { self.0.backend.set_date(selected) };
        if !result {
            self.0.requested_date.set(None);
            self.0.pending_action.replace(None);
        }
        self.refresh();
    }

    pub fn refresh(&self) {
        let s = &self.0;
        let backend = &s.backend;
        let current = backend.now();
        self.refresh_clock_display(&current);
        self.refresh_calendar_card();
        let state = backend.state();
        let error = state.error.clone();
        let pending = *s.pending_action.borrow();
        if let Some(action) = pending {
            s.saw_busy.set(s.saw_busy.get() || state.busy);
            if !state.busy && !error.is_empty() {
                s.pending_action.replace(None);
                s.requested_date.set(None);
                s.requested_zone.replace(None);
            } else if !state.busy && s.saw_busy.get() {
                if action == "timezone" && Some(&state.timezone) == s.requested_zone.borrow().as_ref() {
                    s.message.replace(tr!("Time zone updated."));
                    s.timezone_dirty.set(false);
                    s.pending_action.replace(None);
                    s.requested_zone.replace(None);
                } else if action == "date" || action == "datetime" {
                    s.message.replace(if action == "datetime" { tr!("System date and time updated.") } else { tr!("System date updated.") });
                    s.time_dirty.set(false);
                    s.date_dirty.set(false);
                    s.pending_action.replace(None);
                    s.requested_date.set(None);
                }
            }
        }
        self.show_current_time(&current);
        s.calendar_view.refresh_today();
        let ready = state.ready && !state.busy;
        let manual = ready && !state.ntp;
        s.syncing.set(true);
        self.update_zones();
        if !s.date_dirty.get() && s.calendar_view.selected_date() != current.date() {
            s.calendar_view.set_date(current.date());
        }
        if !s.time_dirty.get() {
            for (spin, value) in [(&s.hour_spin, current.hour()), (&s.minute_spin, current.minute()), (&s.second_spin, current.second())] {
                spin.set_value(value as f64);
                self.time_output(spin);
            }
        }
        s.ntp_switch.set_active(state.ntp);
        s.ntp_switch.set_state(state.ntp);
        s.syncing.set(false);
        s.ntp_switch.set_sensitive(ready && state.can_ntp);
        s.calendar_view.widget().set_sensitive(manual);
        s.timezone_combo.set_sensitive(manual);
        for spin in [&s.hour_spin, &s.minute_spin, &s.second_spin] {
            spin.set_sensitive(manual);
        }
        let chosen_zone = self.chosen_zone();
        let pending_zone = chosen_zone.as_deref() != Some(state.timezone.as_str());
        let chosen_time = self.chosen_time();
        s.timezone_apply_button.set_sensitive(manual && chosen_zone.is_some() && pending_zone);
        let selected = s.calendar_view.selected_date();
        s.apply_button.set_sensitive(manual && !pending_zone && chosen_time.is_some() && selected.year() >= 1970
                                     && (selected != current.date() || s.time_dirty.get()));
        s.manual_hint.set_text(&if state.ntp {
            tr!("Turn off automatic date & time to edit the time zone, date, or time.")
        } else if chosen_zone.is_none() {
            tr!("Choose a time zone from the list.")
        } else if pending_zone {
            tr!("Apply the selected time zone before setting its local date and time.")
        } else if chosen_time.is_none() {
            tr!("Enter a valid time: hours 00–23, minutes and seconds 00–59.")
        } else {
            tr!("Changes take effect when you click Apply.")
        });
        let tzname = current.tzname();
        s.timezone_label.set_text(&if !state.timezone.is_empty() { state.timezone.clone() } else if !tzname.is_empty() { tzname } else { tr!("Local time") });
        let own_message = s.message.borrow().clone();
        let mut message = if state.busy {
            tr!("Waiting for authorization or applying the change…")
        } else if !error.is_empty() {
            error.clone()
        } else if !own_message.is_empty() {
            own_message
        } else if !state.ready {
            tr!("Connecting to the date and time service…")
        } else if state.ntp && state.synchronized {
            tr!("Automatic time is synchronized.")
        } else if state.ntp {
            tr!("Automatic time is enabled.")
        } else {
            tr!("Your desktop may request authentication when you apply a change.")
        };
        if manual && selected.year() < 1970 {
            message = tr!("Choose a date in 1970 or later.");
        }
        s.status_label.set_text(&message);
        s.status_label.set_tooltip_text(Some(&message));
        if error.is_empty() { s.status_label.remove_css_class("error") } else { s.status_label.add_css_class("error") }
    }
}

fn up(weak: &Weak<SettingsInner>) -> Option<DateSettingsWindow> {
    weak.upgrade().map(DateSettingsWindow)
}
