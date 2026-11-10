//! Port of test_calendar.py (isolated GUI tests with a fake clock/backend).
//! Runs only inside the private nested headless labwc started by
//! tests/run_headless.sh (own XDG dirs and D-Bus); under a plain `cargo test`
//! it skips itself. The real system clock and timedated are never used.
#![allow(deprecated)]

use tessel_calendar::app::{BackendFactory, CalendarApp, APPLICATION_ID};
use tessel_calendar::backend::{zoned, Clock, DateSystem};
use tessel_calendar::civil::{days_in_month, Date};
use tessel_calendar::fake::{FakeDateBackend, FakeOp};
use tessel_calendar::preferences::{preferences_path, CalendarPreferencesStore, MONDAY, SATURDAY};
use tessel_calendar::store::EventStore;
use tessel_calendar::view::{day_abbr, strftime, CalendarView};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_core::clock::{ClockPreferencesStore, DATE_ORDERS, DATE_SEPARATORS};

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn drain() {
    let context = glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool, what: &str, timeout: f64) {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        drain();
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("Timed out waiting for {what}");
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var("TMPDIR").unwrap();
    let path = PathBuf::from(base).join(format!("ui-{}-{name}-{}", std::process::id(), glib::random_int()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

thread_local!(static SEQUENCE: Cell<u32> = const { Cell::new(0) });

struct Fx {
    app: CalendarApp,
    backend: Rc<RefCell<Option<Rc<FakeDateBackend>>>>,
    count: Rc<Cell<u32>>,
    cleanup: RefCell<Vec<PathBuf>>,
}

impl Fx {
    fn new() -> Self {
        let n = SEQUENCE.with(|s| { s.set(s.get() + 1); s.get() });
        Self::with_id(&format!("org.calendar_manager.Test{}N{n}", std::process::id()))
    }

    fn with_id(id: &str) -> Self {
        let backend: Rc<RefCell<Option<Rc<FakeDateBackend>>>> = Rc::default();
        let count = Rc::new(Cell::new(0));
        let (b, c) = (backend.clone(), count.clone());
        let factory: BackendFactory = Rc::new(move |changed, error| {
            c.set(c.get() + 1);
            let fake = FakeDateBackend::new(move || changed(), move |m| error(m));
            b.replace(Some(fake.clone()));
            fake as Rc<dyn DateSystem>
        });
        let app = CalendarApp::new(id, "calendar_manager.py", factory, Rc::new(|changed| EventStore::with_defaults(changed)),
                                   CalendarPreferencesStore::default());
        app.0.app.register(gtk::gio::Cancellable::NONE).expect("register on the private session bus");
        Self { app, backend, count, cleanup: RefCell::default() }
    }

    fn backend(&self) -> Rc<FakeDateBackend> {
        self.backend.borrow().clone().unwrap()
    }

    fn history(&self) -> Vec<FakeOp> {
        self.backend().history.borrow().clone()
    }

    fn popup(&self) -> tessel_calendar::popup::CalendarPopup {
        self.app.show_popup("bottom", 76);
        let popup = self.app.popup().unwrap();
        let window = popup.window().clone();
        wait_for(|| window.is_mapped(), "calendar popup mapping", 5.0);
        popup
    }

    fn settings(&self) -> tessel_calendar::settings::DateSettingsWindow {
        self.app.show_settings();
        let settings = self.app.settings().unwrap();
        let window = settings.window().clone();
        wait_for(|| window.is_mapped(), "date settings mapping", 5.0);
        settings
    }

    fn calendar_config(&self, values: serde_json::Value) {
        let path = preferences_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, values.to_string()).unwrap();
        self.cleanup.borrow_mut().push(path);
        self.app.preferences_changed();
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for window in self.app.0.app.windows() {
            window.destroy();
        }
        if let Some(backend) = self.backend.borrow().as_ref() {
            backend.close();
        }
        self.app.0.app.quit();
        drain();
        for path in self.cleanup.borrow().iter() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn choose(view: &CalendarView, selected: Date) {
    view.set_date(selected);
    assert_eq!(view.selected_date(), selected);
    view.day_button(selected).expect("visible day").emit_clicked();
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = Vec::new();
    let mut child = widget.first_child();
    while let Some(c) = child {
        result.push(c.clone());
        result.extend(descendants(&c));
        child = c.next_sibling();
    }
    result
}

fn children(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut result = Vec::new();
    let mut child = widget.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        result.push(c);
    }
    result
}

fn labels(widget: &gtk::Widget) -> Vec<String> {
    descendants(widget).into_iter().filter_map(|w| w.downcast::<gtk::Label>().ok()).map(|l| l.label().to_string()).collect()
}

fn set_time(w: &tessel_calendar::settings::DateSettingsWindow, h: f64, m: f64, s: f64) {
    w.0.hour_spin.set_value(h);
    w.0.minute_spin.set_value(m);
    w.0.second_spin.set_value(s);
}

fn shown_time(w: &tessel_calendar::settings::DateSettingsWindow) -> (i32, i32, i32) {
    (w.0.hour_spin.value_as_int(), w.0.minute_spin.value_as_int(), w.0.second_spin.value_as_int())
}

fn d(y: i64, m: i64, day: i64) -> Date {
    Date::ymd(y, m, day)
}

fn clock(h: i64, m: i64, s: i64) -> Clock {
    Clock::new(h, m, s).unwrap()
}

// ------------------------------------------------------------------ tests

fn month_grid_handles_leap_years_and_december_rollover() {
    let f = Fx::new();
    let view = f.popup().calendar_view().clone();
    for (year, month) in [(2024, 2), (2023, 2), (2000, 2), (2100, 2), (2026, 9)] {
        view.set_date(d(year, month, 15));
        let mut days: Vec<i64> = view.0.day_buttons.borrow().keys().filter(|x| x.year() == year && x.month() == month).map(|x| x.day()).collect();
        days.sort();
        assert_eq!(days, (1..=days_in_month(year, month)).collect::<Vec<_>>());
    }
    choose(&view, d(2025, 12, 31));
    view.0.next_button.emit_clicked();
    assert_eq!((view.display_year(), view.display_month()), (2026, 1));
    view.0.previous_button.emit_clicked();
    assert_eq!((view.display_year(), view.display_month()), (2025, 12));
    assert!(f.history().is_empty());
}

fn popup_calendar_browsing_and_selection_never_change_system_date() {
    let f = Fx::new();
    let popup = f.popup();
    let view = popup.calendar_view().clone();
    let original = f.backend().now();
    view.0.next_button.emit_clicked();
    choose(&view, d(2026, 10, 8));
    view.0.previous_button.emit_clicked();
    view.0.today_button.emit_clicked();
    assert_eq!(view.selected_date(), original.date());
    assert_eq!((view.display_year(), view.display_month()), (original.date().year(), original.date().month()));
    assert_eq!(f.backend().now(), original);
    assert!(f.history().is_empty());
    assert!(!popup.window().is_decorated());
    assert!(popup.window().width() <= 460);
    // Layer-shell surface, CSS classes and shared popup controller.
    assert!(popup.window().has_css_class("calendarmanager") && popup.window().has_css_class("tray-popup"));
    assert_eq!(popup.controller().unwrap().group(), "clock");
    assert_eq!(popup.window().title().as_deref(), Some("Calendar"));
}

fn keyboard_navigation_moves_days_months_and_years() {
    let f = Fx::new();
    let view = f.popup().calendar_view().clone();
    let press = |key: gtk::gdk::Key, state: gtk::gdk::ModifierType| view.on_key_pressed(key, state);
    let none = gtk::gdk::ModifierType::empty();
    view.set_date(d(2026, 9, 18));
    press(gtk::gdk::Key::Right, none);
    assert_eq!(view.selected_date(), d(2026, 9, 19));
    press(gtk::gdk::Key::Up, none);
    assert_eq!(view.selected_date(), d(2026, 9, 12));
    press(gtk::gdk::Key::Page_Down, none);
    assert_eq!(view.selected_date(), d(2026, 10, 12));
    press(gtk::gdk::Key::Page_Up, gtk::gdk::ModifierType::CONTROL_MASK);
    assert_eq!(view.selected_date(), d(2025, 10, 12));
    assert_eq!(press(gtk::gdk::Key::Left, gtk::gdk::ModifierType::ALT_MASK), glib::Propagation::Proceed);
    press(gtk::gdk::Key::Home, none);
    assert_eq!(view.selected_date(), d(2026, 9, 18));
    view.set_date(d(2026, 1, 31));
    view.shift_month(1);
    assert_eq!(view.selected_date(), d(2026, 2, 28));
    assert!(f.history().is_empty());
}

fn today_tracks_clock_changes_without_discarding_browsed_selection() {
    let f = Fx::new();
    let view = f.popup().calendar_view().clone();
    let chosen = d(2026, 8, 12);
    choose(&view, chosen);
    let now = f.backend().now();
    f.backend().set_now(now.with_date(d(2027, 1, 1)));
    view.refresh_today();
    assert_eq!(view.selected_date(), chosen);
    assert_eq!((view.display_year(), view.display_month()), (2026, 8));
    view.0.today_button.emit_clicked();
    assert_eq!(view.selected_date(), d(2027, 1, 1));
    assert_eq!((view.display_year(), view.display_month()), (2027, 1));
    assert!(f.history().is_empty());
}

fn private_clock_store(window: &tessel_calendar::settings::DateSettingsWindow) -> PathBuf {
    let path = scratch("clock").join("clock.json");
    window.0.clock_preferences.replace(ClockPreferencesStore::new(&path));
    window.refresh();
    path
}

fn broken_clock_store(window: &tessel_calendar::settings::DateSettingsWindow) {
    // A file where the directory should be: every save fails (Python: OSError).
    let blocker = scratch("blocked").join("file");
    std::fs::write(&blocker, "x").unwrap();
    window.0.clock_preferences.replace(ClockPreferencesStore::new(blocker.join("clock.json")));
    window.refresh();
}

fn clock_display_controls_save_live_without_changing_system_time() {
    let f = Fx::new();
    let w = f.settings();
    let path = private_clock_store(&w);
    assert!(f.backend().state().ntp);
    assert!(w.0.clock_stack_switch.is_sensitive());
    w.0.clock_stack_switch.set_active(false);
    w.0.clock_reverse_switch.set_active(true);
    w.0.clock_hour_combo.set_selected(1);
    w.0.clock_date_combo.set_selected(DATE_ORDERS.iter().position(|o| *o == "ydm").unwrap() as u32);
    w.0.clock_separator_combo.set_selected(DATE_SEPARATORS.iter().position(|o| *o == "-").unwrap() as u32);
    let settings = ClockPreferencesStore::new(&path).load();
    assert!(!settings.stacked && settings.date_first && settings.use_24_hour);
    assert_eq!((settings.date_order.as_str(), settings.date_separator.as_str()), ("ydm", "-"));
    assert_eq!(w.0.clock_preview.orientation(), gtk::Orientation::Horizontal);
    assert_eq!(w.0.clock_preview.first_child().as_ref(), Some(w.0.clock_date_preview.upcast_ref()));
    assert!(w.0.clock_preview.has_css_class("clock-preview-inline"));
    let now = f.backend().now();
    assert_eq!(w.0.clock_time_preview.text(), settings.format_time(&now));
    assert_eq!(w.0.clock_date_preview.text(), settings.format_date(&now));
    w.0.clock_stack_switch.set_active(true);
    assert_eq!(w.0.clock_preview.orientation(), gtk::Orientation::Vertical);
    assert_eq!(w.0.clock_preview.first_child().as_ref(), Some(w.0.clock_date_preview.upcast_ref()));
    assert!(!w.0.clock_preview.has_css_class("clock-preview-inline"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("\n") && text.contains("\"version\": 1"), "{text}");
    assert!(f.history().is_empty());
}

fn clock_display_failed_save_reverts_control_and_reports_error() {
    let f = Fx::new();
    let w = f.settings();
    broken_clock_store(&w);
    w.0.clock_stack_switch.set_active(false);
    assert!(w.0.clock_stack_switch.is_active());
    assert!(w.0.clock_error_label.text().contains("Could not save clock preferences: [Errno 20]"), "{}", w.0.clock_error_label.text());
    assert!(w.0.clock_error_label.is_visible());
    assert!(f.history().is_empty());
}

fn clock_date_year_and_weekday_toggles_preserve_format_and_preview() {
    let f = Fx::new();
    let w = f.settings();
    let path = private_clock_store(&w);
    w.0.clock_show_year_switch.set_active(false);
    w.0.clock_show_weekday_switch.set_active(true);
    assert_eq!(w.0.clock_date_preview.text(), "9/18");
    let now = f.backend().now();
    assert_eq!(w.0.clock_weekday_preview.text(), strftime(now.date(), 0, 0, 0, "%A"));
    assert!(w.0.clock_weekday_preview.is_visible());
    assert_eq!(w.0.clock_preview_row.first_child().as_ref(), Some(w.0.clock_weekday_preview.upcast_ref()));
    assert_eq!(w.0.clock_weekday_preview.next_sibling().as_ref(), Some(w.0.clock_preview.upcast_ref()));
    for stacked in [false, true] {
        w.0.clock_stack_switch.set_active(stacked);
        for date_first in [false, true] {
            w.0.clock_reverse_switch.set_active(date_first);
            let expected: &gtk::Widget = if date_first { w.0.clock_date_preview.upcast_ref() } else { w.0.clock_time_preview.upcast_ref() };
            assert_eq!(w.0.clock_preview.first_child().as_ref(), Some(expected));
            assert_eq!(w.0.clock_preview_row.first_child().as_ref(), Some(w.0.clock_weekday_preview.upcast_ref()));
            assert_eq!(w.0.clock_preview_row.orientation(), gtk::Orientation::Horizontal);
        }
    }
    w.0.clock_show_date_switch.set_active(false);
    assert!(!w.0.clock_date_preview.is_visible());
    assert_eq!(w.0.clock_date_preview.text(), "");
    assert!(w.0.clock_weekday_preview.is_visible());
    for control in [w.0.clock_show_year_switch.upcast_ref::<gtk::Widget>(), w.0.clock_reverse_switch.upcast_ref(),
                    w.0.clock_date_combo.upcast_ref(), w.0.clock_separator_combo.upcast_ref()] {
        assert!(!control.is_sensitive());
    }
    let settings = ClockPreferencesStore::new(&path).load();
    assert!(!settings.show_date && !settings.show_year && settings.show_weekday);
    w.0.clock_show_date_switch.set_active(true);
    assert_eq!(w.0.clock_date_preview.text(), "9/18");
    assert!(w.0.clock_date_preview.is_visible());
    assert!(w.0.clock_show_year_switch.is_sensitive());
    w.0.clock_show_weekday_switch.set_active(false);
    assert!(!w.0.clock_weekday_preview.is_visible());
    assert_eq!(w.0.clock_weekday_preview.text(), "");
    assert!(f.history().is_empty());
}

fn clock_failed_visibility_save_restores_switch_and_preview() {
    let f = Fx::new();
    let w = f.settings();
    broken_clock_store(&w);
    w.0.clock_show_date_switch.set_active(false);
    assert!(w.0.clock_show_date_switch.is_active());
    assert!(w.0.clock_date_preview.is_visible());
    assert!(w.0.clock_show_year_switch.is_sensitive());
    assert!(w.0.clock_error_label.text().contains("Not a directory"));
    assert!(f.history().is_empty());
}

fn automatic_time_blocks_manual_apply_until_disabled() {
    let f = Fx::new();
    let w = f.settings();
    let selected = d(2026, 9, 22);
    choose(&w.0.calendar_view, selected);
    assert!(w.0.ntp_switch.is_active());
    assert!(!w.0.apply_button.is_sensitive());
    for widget in [w.0.hour_spin.upcast_ref::<gtk::Widget>(), w.0.minute_spin.upcast_ref(), w.0.second_spin.upcast_ref(),
                   w.0.timezone_entry.upcast_ref(), w.0.timezone_apply_button.upcast_ref()] {
        assert!(!widget.is_sensitive());
    }
    w.0.apply_button.emit_clicked();
    w.0.timezone_apply_button.emit_clicked();
    assert!(f.history().is_empty());
    w.0.ntp_switch.set_active(false);
    let b = f.backend();
    wait_for(|| !b.state().ntp, "automatic time disabled", 5.0);
    assert_eq!(f.history(), [FakeOp::Ntp(false)]);
    assert!(w.0.apply_button.is_sensitive());
    for widget in [w.0.hour_spin.upcast_ref::<gtk::Widget>(), w.0.minute_spin.upcast_ref(), w.0.second_spin.upcast_ref(),
                   w.0.timezone_entry.upcast_ref()] {
        assert!(widget.is_sensitive());
    }
    assert_eq!(w.0.calendar_view.selected_date(), selected);
    w.0.apply_button.emit_clicked();
    assert_eq!(f.history(), [FakeOp::Ntp(false), FakeOp::Date(selected)]);
    let now = f.backend().now();
    assert_eq!(now.date(), selected);
    assert_eq!((now.hour(), now.minute()), (14, 35));
    assert!(!w.0.apply_button.is_sensitive());
}

fn browsing_manual_date_still_requires_explicit_apply() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    let w = f.settings();
    let original = f.backend().now();
    w.0.calendar_view.0.next_button.emit_clicked();
    choose(&w.0.calendar_view, d(2026, 10, 15));
    assert!(w.0.apply_button.is_sensitive());
    assert!(f.history().is_empty());
    assert_eq!(f.backend().now(), original);
}

fn manual_time_edit_requires_apply_and_updates_selected_date_together() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    let w = f.settings();
    let selected = d(2026, 10, 9);
    let original = f.backend().now();
    choose(&w.0.calendar_view, selected);
    set_time(&w, 19.0, 42.0, 13.0);
    assert!(w.0.apply_button.is_sensitive());
    assert_eq!(f.backend().now(), original);
    assert!(f.history().is_empty());
    w.0.apply_button.emit_clicked();
    assert_eq!(f.history(), [FakeOp::DateTime(selected, clock(19, 42, 13))]);
    let now = f.backend().now();
    assert_eq!((now.date(), now.hour(), now.minute(), now.second()), (selected, 19, 42, 13));
    assert!(!w.0.apply_button.is_sensitive());
    f.backend().set_now(now.add_micros(7_000_000).unwrap());
    assert_eq!(shown_time(&w), (19, 42, 20), "Successful apply must resume following the live clock");
    assert_eq!(f.history().len(), 1);
}

fn unedited_time_tracks_clock_but_unsaved_time_survives_refreshes() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    let w = f.settings();
    assert_eq!(shown_time(&w), (14, 35, 0));
    let now = f.backend().now();
    f.backend().set_now(now.add_micros(19_000_000).unwrap());
    assert_eq!(shown_time(&w), (14, 35, 19));
    let selected = d(2026, 12, 8);
    choose(&w.0.calendar_view, selected);
    set_time(&w, 8.0, 12.0, 34.0);
    let now = f.backend().now();
    f.backend().set_now(now.add_micros(3_613_000_000).unwrap());
    f.backend().update(|s| s.synchronized = false);
    assert_eq!(shown_time(&w), (8, 12, 34));
    assert_eq!(w.0.calendar_view.selected_date(), selected);
    assert!(f.history().is_empty());
}

fn current_time_line_shows_every_second() {
    let f = Fx::new();
    let w = f.settings();
    let label = w.0.current_label.clone();
    assert!(label.has_css_class("current-time") && !label.has_css_class("muted"), "the line stands out");
    let text = |seconds: &str| label.text().ends_with(&format!(":{seconds}"));
    assert!(text("00"), "{}", label.text());
    for second in ["01", "02", "03"] {
        let now = f.backend().now();
        f.backend().tick_to(now.add_micros(1_000_000).unwrap());
        wait_for(|| text(second), &format!("second {second} without a page refresh"), 1.3);
    }
}

fn typed_time_rejects_invalid_fields_and_applies_uncommitted_valid_text() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    let w = f.settings();
    let selected = d(2026, 10, 20);
    choose(&w.0.calendar_view, selected);
    let spins = [w.0.hour_spin.clone(), w.0.minute_spin.clone(), w.0.second_spin.clone()];
    for (index, invalid) in [(0, "24"), (1, "60"), (2, "-1"), (0, "")] {
        for (spin, valid) in spins.iter().zip(["08", "09", "10"]) {
            spin.set_text(valid);
        }
        spins[index].set_text(invalid);
        assert!(!w.0.apply_button.is_sensitive(), "{index} {invalid}");
        w.0.apply_button.emit_clicked();
        assert!(f.history().is_empty());
    }
    for (spin, valid) in spins.iter().zip(["08", "09", "10"]) {
        spin.set_text(valid);
    }
    assert!(w.0.apply_button.is_sensitive());
    w.0.apply_button.emit_clicked();
    assert_eq!(f.history(), [FakeOp::DateTime(selected, clock(8, 9, 10))]);
}

fn timezone_choice_validates_supported_names_and_preserves_instant() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    let w = f.settings();
    let original = f.backend().now();
    assert_eq!(w.0.timezone_entry.text(), "America/Denver");
    assert!(!w.0.timezone_apply_button.is_sensitive());
    let completion = w.0.timezone_entry.completion().expect("completion");
    let model = completion.model().unwrap();
    let mut zones = Vec::new();
    if let Some(mut iter) = model.iter_first() {
        loop {
            zones.push(model.get::<String>(&iter, completion.text_column()));
            if !model.iter_next(&mut iter) { break }
        }
    }
    let mut expected = f.backend().state().timezones;
    expected.sort();
    zones.sort();
    assert_eq!(zones, expected);
    w.0.timezone_entry.set_text("new york");
    assert!(!w.0.timezone_apply_button.is_sensitive(), "A search fragment is not a selected timezone");
    w.0.timezone_entry.set_text("Not/A_Real_Timezone");
    assert!(!w.0.timezone_apply_button.is_sensitive());
    assert!(!w.0.apply_button.is_sensitive());
    w.0.timezone_apply_button.emit_clicked();
    assert!(f.history().is_empty());
    w.0.timezone_entry.set_text("Asia/Tokyo");
    assert!(w.0.timezone_apply_button.is_sensitive());
    assert!(!w.0.apply_button.is_sensitive());
    w.0.apply_button.emit_clicked();
    assert!(f.history().is_empty());
    assert_eq!(f.backend().state().timezone, "America/Denver");
    w.0.timezone_apply_button.emit_clicked();
    assert_eq!(f.history(), [FakeOp::Timezone("Asia/Tokyo".into())]);
    assert_eq!(f.backend().state().timezone, "Asia/Tokyo");
    assert_eq!(f.backend().now().timestamp_micros(), original.timestamp_micros());
    assert_eq!(w.0.timezone_entry.text(), "Asia/Tokyo");
    assert_eq!(shown_time(&w), (5, 35, 0));
    assert!(!w.0.timezone_apply_button.is_sensitive());
}

fn pending_timezone_blocks_time_apply_and_authorization_failure_can_retry() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    f.backend().defer_changes.set(true);
    let w = f.settings();
    let selected = d(2026, 10, 13);
    choose(&w.0.calendar_view, selected);
    let original = f.backend().now();
    w.0.timezone_entry.set_text("Europe/London");
    assert!(!w.0.apply_button.is_sensitive());
    w.0.timezone_apply_button.emit_clicked();
    assert_eq!(*f.backend().pending.borrow(), Some(FakeOp::Timezone("Europe/London".into())));
    for widget in [w.0.apply_button.upcast_ref::<gtk::Widget>(), w.0.timezone_apply_button.upcast_ref(), w.0.timezone_entry.upcast_ref(),
                   w.0.hour_spin.upcast_ref(), w.0.minute_spin.upcast_ref(), w.0.second_spin.upcast_ref()] {
        assert!(!widget.is_sensitive());
    }
    f.backend().complete_pending(Some("Authentication was cancelled."));
    assert_eq!(f.backend().state().timezone, "America/Denver");
    assert_eq!(f.backend().now(), original);
    assert_eq!(w.0.timezone_entry.text(), "Europe/London");
    assert_eq!(w.0.calendar_view.selected_date(), selected);
    assert!(w.0.timezone_apply_button.is_sensitive());
    assert!(!w.0.apply_button.is_sensitive());
    assert!(w.0.status_label.text().contains("Authentication was cancelled"));
    w.0.timezone_apply_button.emit_clicked();
    f.backend().complete_pending(None);
    assert_eq!(f.history(), [FakeOp::Timezone("Europe/London".into()), FakeOp::Timezone("Europe/London".into())]);
    assert_eq!(f.backend().now().timestamp_micros(), original.timestamp_micros());
    assert_eq!(f.backend().state().timezone, "Europe/London");
    assert_eq!(shown_time(&w), (21, 35, 0));
    assert!(w.0.apply_button.is_sensitive());
    assert!(!w.0.status_label.text().contains("cancelled"));
}

fn pending_time_and_date_survive_failed_authorization_and_retry() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    f.backend().defer_changes.set(true);
    let w = f.settings();
    let selected = d(2026, 12, 14);
    let original = f.backend().now();
    choose(&w.0.calendar_view, selected);
    set_time(&w, 7.0, 8.0, 9.0);
    w.0.apply_button.emit_clicked();
    assert_eq!(*f.backend().pending.borrow(), Some(FakeOp::DateTime(selected, clock(7, 8, 9))));
    f.backend().complete_pending(Some("Permission denied to set the clock."));
    f.backend().set_now(original.add_micros(120_000_000).unwrap());
    assert_eq!(shown_time(&w), (7, 8, 9));
    assert_eq!(w.0.calendar_view.selected_date(), selected);
    assert!(w.0.status_label.text().contains("Permission denied"));
    assert!(w.0.apply_button.is_sensitive());
    w.0.apply_button.emit_clicked();
    f.backend().complete_pending(None);
    assert_eq!(f.history(), [FakeOp::DateTime(selected, clock(7, 8, 9)), FakeOp::DateTime(selected, clock(7, 8, 9))]);
    let now = f.backend().now();
    assert_eq!((now.date(), now.hour(), now.minute(), now.second()), (selected, 7, 8, 9));
    assert!(!w.0.status_label.text().contains("Permission denied"));
}

fn unapplied_timezone_survives_external_changes_and_can_be_cancelled() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    let w = f.settings();
    choose(&w.0.calendar_view, d(2026, 10, 3));
    w.0.timezone_entry.set_text("Europe/London");
    f.backend().update(|s| s.timezone = "America/New_York".into());
    assert_eq!(w.0.timezone_entry.text(), "Europe/London");
    assert!(!w.0.apply_button.is_sensitive());
    assert!(f.history().is_empty());
    w.0.timezone_entry.set_text("America/New_York");
    assert!(!w.0.timezone_apply_button.is_sensitive());
    assert!(w.0.apply_button.is_sensitive());
    assert!(f.history().is_empty());
    f.backend().update(|s| s.timezone = "Etc/UTC".into());
    assert_eq!(w.0.timezone_entry.text(), "Etc/UTC", "Cancelling an unapplied choice must resume following the actual timezone");
    assert!(!w.0.timezone_apply_button.is_sensitive());
    assert!(w.0.apply_button.is_sensitive());
    assert!(f.history().is_empty());
}

fn async_apply_disables_changes_and_preserves_selection_until_complete() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    f.backend().defer_changes.set(true);
    let w = f.settings();
    let selected = d(2026, 10, 9);
    choose(&w.0.calendar_view, selected);
    w.0.apply_button.emit_clicked();
    assert!(f.backend().state().busy);
    assert_eq!(*f.backend().pending.borrow(), Some(FakeOp::Date(selected)));
    assert!(!w.0.apply_button.is_sensitive());
    assert!(!w.0.ntp_switch.is_sensitive());
    assert!(w.0.status_label.text().contains("Waiting"));
    f.backend().changed();
    w.0.apply_button.emit_clicked();
    assert_eq!(f.history(), [FakeOp::Date(selected)]);
    assert_eq!(w.0.calendar_view.selected_date(), selected);
    f.backend().complete_pending(None);
    assert!(!f.backend().state().busy);
    assert_eq!(f.backend().now().date(), selected);
    assert_eq!(w.0.calendar_view.selected_date(), selected);
    assert!(w.0.status_label.text().contains("updated"));
    assert!(w.0.ntp_switch.is_sensitive());
}

fn authorization_failure_keeps_actual_date_and_allows_retry() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    f.backend().defer_changes.set(true);
    let w = f.settings();
    let selected = d(2026, 11, 3);
    let actual = f.backend().now().date();
    choose(&w.0.calendar_view, selected);
    w.0.apply_button.emit_clicked();
    f.backend().complete_pending(Some("Authentication was cancelled."));
    assert_eq!(f.backend().now().date(), actual);
    assert_eq!(w.0.calendar_view.selected_date(), selected);
    assert!(w.0.apply_button.is_sensitive());
    assert!(w.0.status_label.text().contains("Authentication was cancelled"));
    w.0.apply_button.emit_clicked();
    assert_eq!(f.history(), [FakeOp::Date(selected), FakeOp::Date(selected)]);
    f.backend().complete_pending(None);
    assert_eq!(f.backend().now().date(), selected);
    assert!(!w.0.status_label.text().contains("cancelled"));
}

fn ntp_authorization_failure_restores_toggle_and_manual_guard() {
    let f = Fx::new();
    let w = f.settings();
    f.backend().defer_changes.set(true);
    w.0.ntp_switch.set_active(false);
    assert!(f.backend().state().busy);
    assert!(!w.0.ntp_switch.is_sensitive());
    f.backend().complete_pending(Some("Permission denied to change automatic time."));
    assert!(f.backend().state().ntp);
    assert!(w.0.ntp_switch.is_active());
    assert!(!w.0.apply_button.is_sensitive());
    assert!(w.0.status_label.text().contains("Permission denied"));
    assert!(w.0.status_label.has_css_class("error"));
    assert_eq!(f.history(), [FakeOp::Ntp(false)]);
}

fn external_updates_preserve_selection_and_disable_unavailable_controls() {
    let f = Fx::new();
    f.backend().update(|s| s.ntp = false);
    let w = f.settings();
    let chosen = d(2027, 3, 12);
    choose(&w.0.calendar_view, chosen);
    f.backend().update(|s| {
        s.synchronized = true;
        s.timezone = "Etc/UTC".into();
    });
    assert_eq!(w.0.calendar_view.selected_date(), chosen);
    assert_eq!((w.0.calendar_view.display_year(), w.0.calendar_view.display_month()), (2027, 3));
    f.backend().update(|s| {
        s.ready = false;
        s.error = "The system date service is unavailable.".into();
    });
    assert!(!w.0.ntp_switch.is_sensitive());
    assert!(!w.0.apply_button.is_sensitive());
    assert!(w.0.status_label.text().contains("unavailable"));
    w.0.apply_button.emit_clicked();
    assert!(f.history().is_empty());
    f.backend().update(|s| {
        s.ready = true;
        s.error.clear();
        s.can_ntp = false;
    });
    assert!(!w.0.ntp_switch.is_sensitive());
    assert!(w.0.apply_button.is_sensitive());
    assert_eq!(w.0.calendar_view.selected_date(), chosen);
}

fn popup_settings_button_reuses_existing_window_and_backend() {
    let f = Fx::new();
    let popup = f.popup();
    assert!(gtk::test_accessible_has_property(&popup.0.settings_button, gtk::AccessibleProperty::Label));
    popup.0.settings_button.emit_clicked();
    wait_for(|| f.app.settings().is_some_and(|s| s.window().is_mapped()), "Date Settings button", 5.0);
    let settings = f.app.settings().unwrap();
    assert!(!popup.window().is_visible());
    f.app.show_popup("bottom", 76);
    let window = popup.window().clone();
    wait_for(|| window.is_mapped(), "popup reopened", 5.0);
    popup.0.settings_button.emit_clicked();
    assert!(Rc::ptr_eq(&f.app.settings().unwrap().0, &settings.0));
    assert_eq!(f.count.get(), 1);
    assert!(f.history().is_empty());
    assert_eq!(settings.window().title().as_deref(), Some("Date Settings"));
}

fn popup_without_calendars_explains_how_to_add_them() {
    let f = Fx::new();
    let popup = f.popup();
    assert!(popup.0.events_box.is_visible());
    assert!(popup.0.events_status.text().contains("Date settings"));
    assert!(!popup.0.events_scroll.is_visible());
}

fn week_start_preference_and_selected_day_events() {
    let f = Fx::new();
    let directory = scratch("events");
    let ics = directory.join("work.ics");
    std::fs::write(&ics, "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:1\nSUMMARY:Planning\nLOCATION:Room 2\n\
        DTSTART:20260918T100000\nDTEND:20260918T110000\nRRULE:FREQ=WEEKLY;COUNT=2\n\
        END:VEVENT\nBEGIN:VEVENT\nUID:2\nSUMMARY:Holiday\nDTSTART;VALUE=DATE:20260921\n\
        END:VEVENT\nEND:VCALENDAR\n").unwrap();
    let popup = f.popup();
    f.calendar_config(serde_json::json!({"week_start": "monday",
                                         "sources": [{"kind": "file", "location": ics.to_string_lossy(), "name": "Work"}]}));
    let events = f.app.events().unwrap();
    wait_for(|| !events.loading() && !events.events().is_empty(), "calendar events loading", 5.0);
    let view = popup.calendar_view().clone();
    assert_eq!(view.first_weekday(), MONDAY);
    assert_eq!(view.0.headings[0].label(), day_abbr(MONDAY).chars().take(2).collect::<String>());
    assert_eq!(view.visible_range().0.weekday(), MONDAY);
    let mut marked: Vec<Date> = view.0.day_buttons.borrow().iter().filter(|(_, b)| b.has_css_class("has-events")).map(|(d, _)| *d).collect();
    marked.sort();
    assert_eq!(marked, [d(2026, 9, 18), d(2026, 9, 21), d(2026, 9, 25)]);
    assert_eq!(view.selected_date(), d(2026, 9, 18));
    let rows = children(&popup.0.event_list);
    assert_eq!(rows.len(), 1);
    let texts = labels(&rows[0]);
    assert!(texts.contains(&"Planning".to_owned()) && texts.contains(&"Room 2".to_owned()), "{texts:?}");
    assert!(texts.contains(&"10:00 AM".to_owned()) || texts.iter().any(|t| t.starts_with("10:00")), "{texts:?}");
    choose(&view, d(2026, 9, 21));
    let texts: Vec<String> = children(&popup.0.event_list).iter().flat_map(labels).collect();
    assert!(texts.contains(&"All day".to_owned()), "{texts:?}");
    choose(&view, d(2026, 9, 22));
    assert!(children(&popup.0.event_list).is_empty());
    assert_eq!(popup.0.events_status.text(), "No events");
    f.calendar_config(serde_json::json!({"week_start": "saturday", "sources": []}));
    assert_eq!(view.first_weekday(), SATURDAY);
    wait_for(|| view.marked_days().is_empty(), "event marks cleared", 5.0);
    assert!(f.history().is_empty());
    let _ = std::fs::remove_dir_all(directory);
}

fn calendar_settings_week_start_and_sources() {
    let f = Fx::new();
    let w = f.settings();
    let directory = scratch("prefs");
    w.0.calendar_preferences.replace(CalendarPreferencesStore::new(directory.join("calendar.json")));
    w.refresh();
    w.0.week_start_combo.set_selected(3);
    assert_eq!(w.0.calendar_preferences.borrow_mut().load().week_start, "saturday");
    assert_eq!(w.0.calendar_view.first_weekday(), SATURDAY);
    w.0.calendar_source_entry.set_text("relative.ics");
    w.0.calendar_add_button.emit_clicked();
    assert!(w.0.calendar_error_label.is_visible());
    assert!(w.0.calendar_preferences.borrow_mut().load().sources.is_empty());
    w.0.calendar_source_entry.set_text("webcal://calendar.example/feed.ics");
    w.0.calendar_add_button.emit_clicked();
    assert!(!w.0.calendar_error_label.is_visible());
    assert_eq!(w.0.calendar_source_entry.text(), "");
    let sources = w.0.calendar_preferences.borrow_mut().load().sources;
    assert_eq!(sources.iter().map(|s| s.location.as_str()).collect::<Vec<_>>(), ["https://calendar.example/feed.ics"]);
    let remove: Vec<gtk::Button> = descendants(w.0.calendar_source_list.upcast_ref()).into_iter().filter_map(|w| w.downcast().ok()).collect();
    assert_eq!(remove.len(), 1);
    remove[0].emit_clicked();
    assert!(w.0.calendar_preferences.borrow_mut().load().sources.is_empty());
    assert!(f.history().is_empty());
    let _ = std::fs::remove_dir_all(directory);
}

fn remote_activate(view: &str) {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_tessel-calendar")).arg(format!("--{view}"))
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        drain();
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "secondary launcher failed: {status:?}");
            return;
        }
        assert!(Instant::now() < end, "secondary launcher exit");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn remote_activation_reuses_application_and_routes_between_views() {
    // The private bus has no real calendar instance, so the real id is free.
    let f = Fx::with_id(APPLICATION_ID);
    let settings = f.settings();
    let backend = f.backend();
    remote_activate("popup");
    wait_for(|| f.app.popup().is_some_and(|p| p.window().is_mapped()), "remote calendar popup", 5.0);
    let popup = f.app.popup().unwrap();
    remote_activate("settings");
    let window = settings.window().clone();
    wait_for(|| window.is_mapped(), "remote date settings", 5.0);
    assert!(Rc::ptr_eq(&f.app.settings().unwrap().0, &settings.0));
    assert!(Rc::ptr_eq(&f.app.popup().unwrap().0, &popup.0));
    assert!(Rc::ptr_eq(&f.backend(), &backend));
    assert_eq!(f.count.get(), 1);
    wait_for(|| !popup.window().is_visible(), "popup dismissed", 5.0);
    // A repeated click with the same position toggles; a new position rebuilds.
    remote_activate("popup");
    wait_for(|| f.app.popup().is_some_and(|p| p.window().is_mapped()), "popup again", 5.0);
    f.app.show_popup("bottom", 76);
    wait_for(|| !popup.window().is_visible(), "toggle hides", 5.0);
    f.app.show_popup("top", 40);
    let rebuilt = f.app.popup().unwrap();
    assert!(!Rc::ptr_eq(&rebuilt.0, &popup.0));
    assert_eq!((rebuilt.controller().unwrap().edge(), rebuilt.controller().unwrap().offset()), ("top".to_owned(), 40));
    assert!(f.history().is_empty());
}

fn fake_backend_preview_clock() {
    // The deterministic fake clock used by every case above.
    let fake = FakeDateBackend::new(|| {}, |_| {});
    let now = fake.now();
    assert_eq!(now.wall, zoned(2026, 9, 18, 14, 35, 0, 0, &now.zone).wall);
    assert_eq!(now.zone.key, "America/Denver");
}

fn main() {
    if std::env::var_os("CALENDAR_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run tests/run_headless.sh)");
        return;
    }
    gtk::init().expect("headless display");
    tessel_core::i18n::initialize();
    let tests: &[(&str, fn())] = &[
        ("month_grid_handles_leap_years_and_december_rollover", month_grid_handles_leap_years_and_december_rollover),
        ("popup_calendar_browsing_and_selection_never_change_system_date", popup_calendar_browsing_and_selection_never_change_system_date),
        ("keyboard_navigation_moves_days_months_and_years", keyboard_navigation_moves_days_months_and_years),
        ("today_tracks_clock_changes_without_discarding_browsed_selection", today_tracks_clock_changes_without_discarding_browsed_selection),
        ("current_time_line_shows_every_second", current_time_line_shows_every_second),
        ("clock_display_controls_save_live_without_changing_system_time", clock_display_controls_save_live_without_changing_system_time),
        ("clock_display_failed_save_reverts_control_and_reports_error", clock_display_failed_save_reverts_control_and_reports_error),
        ("clock_date_year_and_weekday_toggles_preserve_format_and_preview", clock_date_year_and_weekday_toggles_preserve_format_and_preview),
        ("clock_failed_visibility_save_restores_switch_and_preview", clock_failed_visibility_save_restores_switch_and_preview),
        ("automatic_time_blocks_manual_apply_until_disabled", automatic_time_blocks_manual_apply_until_disabled),
        ("browsing_manual_date_still_requires_explicit_apply", browsing_manual_date_still_requires_explicit_apply),
        ("manual_time_edit_requires_apply_and_updates_selected_date_together", manual_time_edit_requires_apply_and_updates_selected_date_together),
        ("unedited_time_tracks_clock_but_unsaved_time_survives_refreshes", unedited_time_tracks_clock_but_unsaved_time_survives_refreshes),
        ("typed_time_rejects_invalid_fields_and_applies_uncommitted_valid_text", typed_time_rejects_invalid_fields_and_applies_uncommitted_valid_text),
        ("timezone_choice_validates_supported_names_and_preserves_instant", timezone_choice_validates_supported_names_and_preserves_instant),
        ("pending_timezone_blocks_time_apply_and_authorization_failure_can_retry", pending_timezone_blocks_time_apply_and_authorization_failure_can_retry),
        ("pending_time_and_date_survive_failed_authorization_and_retry", pending_time_and_date_survive_failed_authorization_and_retry),
        ("unapplied_timezone_survives_external_changes_and_can_be_cancelled", unapplied_timezone_survives_external_changes_and_can_be_cancelled),
        ("async_apply_disables_changes_and_preserves_selection_until_complete", async_apply_disables_changes_and_preserves_selection_until_complete),
        ("authorization_failure_keeps_actual_date_and_allows_retry", authorization_failure_keeps_actual_date_and_allows_retry),
        ("ntp_authorization_failure_restores_toggle_and_manual_guard", ntp_authorization_failure_restores_toggle_and_manual_guard),
        ("external_updates_preserve_selection_and_disable_unavailable_controls", external_updates_preserve_selection_and_disable_unavailable_controls),
        ("popup_settings_button_reuses_existing_window_and_backend", popup_settings_button_reuses_existing_window_and_backend),
        ("popup_without_calendars_explains_how_to_add_them", popup_without_calendars_explains_how_to_add_them),
        ("week_start_preference_and_selected_day_events", week_start_preference_and_selected_day_events),
        ("calendar_settings_week_start_and_sources", calendar_settings_week_start_and_sources),
        ("remote_activation_reuses_application_and_routes_between_views", remote_activation_reuses_application_and_routes_between_views),
        ("fake_backend_preview_clock", fake_backend_preview_clock),
    ];
    let only = std::env::var("CALENDAR_UI_ONLY").ok();
    let mut failed = 0;
    for (name, test) in tests {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        let result = std::panic::catch_unwind(test);
        settle(0.05);
        match result {
            Ok(()) => println!("ok {name}"),
            Err(error) => {
                failed += 1;
                let text = error.downcast_ref::<String>().cloned().or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                println!("FAILED {name}: {text}");
            }
        }
    }
    println!("{} headless calendar tests; {failed} failed", tests.len());
    std::process::exit(i32::from(failed > 0));
}
