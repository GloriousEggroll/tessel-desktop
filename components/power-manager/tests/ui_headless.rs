//! Port of test_power_ui.py (PowerUITests) with a fake backend. Runs only
#![allow(deprecated)]
//! inside the private nested headless labwc (tests/run_headless.sh); under a
//! plain `cargo test` it skips itself. No real power/idle modules are used.

use gtk::glib;
use gtk::prelude::*;
use suite_power::backend::PowerSystem;
use power_manager::dim::DimOverlay;
use power_manager::fakes::FakeSystem;
use suite_power::ui::style;
use power_manager::ui::{Control, PowerPopup, PowerSettingsWindow};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while !predicate() && Instant::now() < end {
        settle(0.02);
    }
    predicate()
}

fn default_settings() -> Value {
    json!({"version": 1, "lock_after": 300,
           "ac": {"display_off": 600, "sleep_after": 0, "sleep_action": "suspend"},
           "battery": {"display_off": 600, "sleep_after": 0, "sleep_action": "suspend"}})
}

struct Fx {
    backend: Rc<FakeSystem>,
    app_error: Rc<RefCell<String>>,
    opened: Rc<RefCell<Vec<String>>>,
    saved: Rc<RefCell<Vec<Value>>>,
    windows: RefCell<Vec<gtk::Window>>,
}

impl Fx {
    fn new() -> Self {
        Self { backend: FakeSystem::ready(), app_error: Rc::default(), opened: Rc::default(), saved: Rc::default(), windows: RefCell::default() }
    }

    fn error_fn(&self) -> Rc<dyn Fn() -> String> {
        let e = self.app_error.clone();
        Rc::new(move || e.borrow().clone())
    }

    fn popup(&self) -> Rc<PowerPopup> {
        let opened = self.opened.clone();
        let popup = PowerPopup::new(self.backend.clone(), self.error_fn(), Rc::new(move || opened.borrow_mut().push("settings".into())),
                                    "bottom", 76);
        self.windows.borrow_mut().push(popup.window.clone());
        popup.show();
        settle(0.06);
        popup
    }

    fn settings(&self, initial: &Value, save: Option<Rc<dyn Fn(Value) -> bool>>) -> Rc<PowerSettingsWindow> {
        let saved = self.saved.clone();
        let save = save.unwrap_or_else(|| Rc::new(move |v: Value| { saved.borrow_mut().push(v); true }));
        let window = PowerSettingsWindow::new(self.backend.clone(), self.error_fn(), initial, save);
        self.windows.borrow_mut().push(window.window.clone());
        window.window.set_visible(true);
        window.refresh();
        settle(0.06);
        window
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for window in self.windows.borrow().iter() {
            window.destroy();
        }
        settle(0.06);
    }
}

fn header_icons_resolve_and_fit_in_light_and_dark_icon_themes() {
    let f = Fx::new();
    let popup = f.popup();
    assert!(wait_until(|| popup.window.is_mapped(), 2.0), "popup maps");
    let settings = gtk::Settings::default().unwrap();
    let original = settings.gtk_icon_theme_name();
    let theme = gtk::IconTheme::for_display(&WidgetExt::display(&popup.window));
    let installed = |name: &str| std::path::Path::new("/usr/share/icons").join(name).is_dir();
    for name in ["Adwaita", "Papirus", "Papirus-Dark"].into_iter().filter(|n| installed(n)) {
        settings.set_gtk_icon_theme_name(Some(name));
        settle(0.05);
        for symbol in ["preferences-system-power-symbolic", "ac-adapter-symbolic", "battery-missing-symbolic", "battery-caution-symbolic",
                       "battery-low-symbolic", "battery-good-symbolic", "battery-full-symbolic"] {
            let image = style::icon(symbol, 18);
            let paintable = theme.lookup_by_gicon(&image.gicon().unwrap(), 24, 1, gtk::TextDirection::None, gtk::IconLookupFlags::PRELOAD);
            let resolved = paintable.icon_name().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            assert!(!resolved.contains("image-missing"), "{name}: {symbol}");
            assert!(paintable.is_symbolic(), "{name}: {symbol} -> {resolved}");
        }
        settle(0.05);
        assert_eq!((popup.source_icon.width(), popup.source_icon.height()), (24, 24));
        assert!(popup.source_icon.parent().unwrap().width() >= 42);
    }
    settings.set_gtk_icon_theme_name(original.as_deref());
    assert!(f.backend.history.borrow().is_empty());
    println!("ok header_icons_resolve_and_fit_in_light_and_dark_icon_themes");
}

fn popup_refresh_is_read_only_and_selection_is_explicit() {
    let f = Fx::new();
    let popup = f.popup();
    for _ in 0..3 {
        popup.refresh();
    }
    assert!(f.backend.history.borrow().is_empty());
    assert!(popup.selector.checks["balanced"].is_visible());
    popup.selector.buttons["performance"].emit_clicked();
    assert_eq!(*f.backend.history.borrow(), ["performance"]);
    assert!(popup.selector.checks["performance"].is_visible());
    assert!(!popup.selector.checks["balanced"].is_visible());
    popup.selector.buttons["performance"].emit_clicked();
    assert_eq!(*f.backend.history.borrow(), ["performance"]);
    assert!(popup.window.width() <= 430);
    assert!(popup.window.has_css_class("powermanager") && popup.window.has_css_class("tray-popup"));
    assert_eq!(popup.window.title().as_deref(), Some("Power"));
    println!("ok popup_refresh_is_read_only_and_selection_is_explicit");
}

fn profile_unavailable_busy_and_rejection_do_not_fake_success() {
    let f = Fx::new();
    let popup = f.popup();
    f.backend.set(|s| s.busy = true);
    popup.refresh();
    assert!(!popup.selector.buttons["performance"].is_sensitive());
    popup.selector.buttons["performance"].emit_clicked();
    f.backend.set(|s| { s.busy = false; s.profiles = vec!["power-saver".into(), "balanced".into()]; });
    popup.refresh();
    popup.selector.buttons["performance"].emit_clicked();
    f.backend.set(|s| s.profiles_ready = false);
    popup.refresh();
    popup.selector.buttons["power-saver"].emit_clicked();
    assert!(f.backend.history.borrow().is_empty());
    f.backend.set(|s| s.profiles_ready = true);
    f.backend.reject.set(true);
    popup.selector.buttons["power-saver"].emit_clicked();
    assert_eq!(f.backend.state().active_profile, "balanced");
    assert!(popup.selector.checks["balanced"].is_visible());
    assert!(popup.status_label.text().contains("cancelled"), "{}", popup.status_label.text());
    f.backend.set(|s| { s.busy = true; s.error.clear(); });
    popup.set_message("");
    popup.refresh();
    assert_eq!(popup.status_label.text(), "Updating power settings…");
    println!("ok profile_unavailable_busy_and_rejection_do_not_fake_success");
}

fn power_degraded_and_application_errors_are_visible_without_battery_widgets() {
    let f = Fx::new();
    let popup = f.popup();
    assert_eq!(popup.source_label.text(), "Profiles & sleep settings");
    f.backend.set(|s| s.performance_degraded = "high-operating-temperature".into());
    f.app_error.replace("Idle settings could not be loaded.".into());
    popup.refresh();
    assert!(popup.status_label.text().contains("high operating temperature"));
    assert!(popup.status_label.text().contains("Idle settings"));
    // Raw paths never reach the popup (friendly_message substring table).
    f.app_error.replace("The screen lock command is missing or not executable: /home/x/.config/waybar/scripts/screenlock.sh".into());
    popup.refresh();
    assert!(popup.status_label.text().contains("Automatic screen locking is not set up yet."));
    assert!(!popup.status_label.text().contains("/home/x"));
    println!("ok power_degraded_and_application_errors_are_visible_without_battery_widgets");
}

fn popup_gear_hides_menu_and_opens_settings() {
    let f = Fx::new();
    let popup = f.popup();
    assert!(wait_until(|| popup.window.is_visible(), 2.0));
    popup.settings_button.emit_clicked();
    assert_eq!(*f.opened.borrow(), ["settings"]);
    assert!(!popup.window.is_visible());
    if let Some(controller) = popup.controller.borrow().as_ref() {
        assert!(controller.backdrops().iter().all(|b| !b.is_visible()));
    }
    popup.activate();
    assert!(wait_until(|| popup.window.is_visible(), 2.0), "Activating a dismissed popup must show it again");
    popup.activate();
    assert!(!popup.window.is_visible());
    println!("ok popup_gear_hides_menu_and_opens_settings");
}

fn idle_edits_are_local_until_save_and_close_discards() {
    let f = Fx::new();
    let initial = default_settings();
    let window = f.settings(&initial, None);
    assert!(wait_until(|| window.save_button.width() > 0, 2.0));
    let origin = |b: &gtk::Button| b.compute_point(&window.window, &gtk::graphene::Point::new(0.0, 0.0)).unwrap().x();
    let (close_x, save_x) = (origin(&window.close_button), origin(&window.save_button));
    assert!(save_x > close_x);
    let (close, save) = (window.close_button.allocation(), window.save_button.allocation());
    assert!(save.x() - close.x() - close.width() <= 12, "Footer actions must remain together on the right");
    assert!(!window.save_button.is_sensitive());
    window.controls["ac"]["display_off"].set_active_id("900");
    window.controls["ac"]["sleep_after"].set_active_id("1800");
    window.controls["ac"]["sleep_action"].set_active_id("hibernate");
    window.lock_choice.set_active_id(Some("600"));
    assert_eq!(initial, default_settings());
    assert!(f.saved.borrow().is_empty());
    assert!(f.backend.history.borrow().is_empty());
    assert!(window.save_button.is_sensitive());
    window.save_button.emit_clicked();
    assert_eq!(f.saved.borrow().len(), 1);
    assert_eq!(f.saved.borrow()[0]["ac"], json!({"display_off": 900, "sleep_after": 1800, "sleep_action": "hibernate"}));
    assert_eq!(f.saved.borrow()[0]["lock_after"], 600);
    assert!(!window.save_button.is_sensitive());
    assert!(window.status_label.text().contains("Settings saved."));
    window.lock_choice.set_active_id(Some("120"));
    window.close_button.emit_clicked();
    settle(0.05);
    assert_eq!(f.saved.borrow().len(), 1);
    assert!(window.destroyed());
    println!("ok idle_edits_are_local_until_save_and_close_discards");
}

fn hidden_battery_policy_and_custom_seconds_are_preserved() {
    let f = Fx::new();
    let mut initial = default_settings();
    initial["battery"] = json!({"display_off": 75, "sleep_after": 1800, "sleep_action": "hibernate"});
    f.backend.set(|s| { s.capabilities.insert("hibernate".into(), "no".into()); });
    let window = f.settings(&initial, None);
    assert!(!window.policy_cards["battery"].is_visible());
    assert!(window.policy_cards["ac"].is_visible());
    assert!(wait_until(|| window.controls["ac"]["display_off"].combo().is_mapped(), 2.0));
    window.lock_choice.set_active_id(Some("600"));
    window.save_button.emit_clicked();
    assert_eq!(f.saved.borrow()[0]["battery"], initial["battery"]);
    f.backend.set(|s| s.has_battery = true);
    window.refresh();
    assert!(window.policy_cards["battery"].is_visible());
    assert_eq!(window.controls["battery"]["display_off"].active_id().as_deref(), Some("75"));
    assert_eq!(window.controls["battery"]["sleep_action"].active_id().as_deref(), Some("hibernate"));
    assert!(window.policy_hints["battery"].text().contains("saved sleep mode"));
    println!("ok hidden_battery_policy_and_custom_seconds_are_preserved");
}

fn unsupported_and_challenge_sleep_modes_stay_disabled_without_normalizing() {
    let f = Fx::new();
    let mut initial = default_settings();
    initial["ac"]["sleep_action"] = "hibernate".into();
    initial["ac"]["sleep_after"] = 900.into();
    f.backend.set(|s| {
        s.capabilities.insert("hibernate".into(), "challenge".into());
        s.capabilities.insert("suspend-then-hibernate".into(), "no".into());
    });
    let window = f.settings(&initial, None);
    let mode = &window.controls["ac"]["sleep_action"];
    assert_eq!(mode.active_id().as_deref(), Some("hibernate"));
    let Control::Sleep(choice) = mode else { panic!("sleep mode control") };
    let rows: Vec<(String, bool)> = choice.rows().into_iter().map(|r| (r.0, r.2)).collect();
    assert_eq!(rows, [("suspend".into(), true), ("hibernate".into(), false), ("suspend-then-hibernate".into(), false)]);
    assert!(choice.rows()[1].1.ends_with(" — authorization required"));
    assert!(choice.rows()[2].1.ends_with(" — unavailable"));
    assert!(window.policy_hints["ac"].text().contains("authorization"));
    window.lock_choice.set_active_id(Some("600"));
    assert!(window.save_button.is_sensitive(), "Unchanged unsupported saved modes must survive unrelated edits");
    window.controls["ac"]["sleep_after"].set_active_id("1800");
    assert!(!window.save_button.is_sensitive());
    window.save_button.emit_clicked();
    assert!(f.saved.borrow().is_empty());
    mode.set_active_id("suspend-then-hibernate");
    assert_eq!(mode.active_id().as_deref(), Some("hibernate"));
    window.controls["ac"]["sleep_after"].set_active_id("0");
    assert!(window.save_button.is_sensitive());
    window.save_button.emit_clicked();
    assert_eq!(f.saved.borrow()[0]["ac"]["sleep_action"], "hibernate");
    println!("ok unsupported_and_challenge_sleep_modes_stay_disabled_without_normalizing");
}

fn immediate_profile_changes_preserve_unsaved_idle_values() {
    let f = Fx::new();
    let window = f.settings(&default_settings(), None);
    window.controls["ac"]["display_off"].set_active_id("1800");
    window.selector.buttons["power-saver"].emit_clicked();
    assert_eq!(*f.backend.history.borrow(), ["power-saver"]);
    assert!(f.saved.borrow().is_empty());
    assert_eq!(window.controls["ac"]["display_off"].active_id().as_deref(), Some("1800"));
    f.backend.set(|s| s.has_battery = true);
    window.refresh();
    assert!(window.save_button.is_sensitive());
    println!("ok immediate_profile_changes_preserve_unsaved_idle_values");
}

fn save_failure_preserves_edits_and_combined_mode_explains_system_delay() {
    let f = Fx::new();
    let error = f.app_error.clone();
    let window = f.settings(&default_settings(), Some(Rc::new(move |_| {
        error.replace("Could not write the configuration file.".into());
        false
    })));
    window.controls["ac"]["sleep_action"].set_active_id("suspend-then-hibernate");
    assert!(window.policy_hints["ac"].text().contains("system controls"));
    window.save_button.emit_clicked();
    assert!(window.save_button.is_sensitive());
    assert!(window.status_label.text().contains("configuration file"));
    assert!(window.status_label.text().contains("Could not save settings."));
    assert_eq!(window.settings.borrow()["ac"]["sleep_action"], "suspend-then-hibernate");
    println!("ok save_failure_preserves_edits_and_combined_mode_explains_system_delay");
}

fn lid_button_dim_and_low_battery_options_save_together() {
    let f = Fx::new();
    let window = f.settings(&default_settings(), None);
    assert!(!window.battery_card.is_visible(), "Desktops have no battery actions");
    assert!(!window.extra(&["lid_close", "ac"]).is_visible());
    assert!(window.extra(&["power_button"]).is_visible());
    assert!(window.dim_switch.is_active());
    f.backend.set(|s| s.has_battery = true);
    window.refresh();
    assert!(window.battery_card.is_visible());
    window.extra(&["lid_close", "battery"]).set_active_id(Some("hibernate"));
    window.extra(&["power_button"]).set_active_id(Some("suspend"));
    window.extra(&["critical_action"]).set_active_id(Some("poweroff"));
    window.extra(&["critical_level"]).set_active_id(Some("5"));
    window.dim_switch.set_active(false);
    assert!(f.saved.borrow().is_empty());
    window.save_button.emit_clicked();
    let saved = f.saved.borrow().last().unwrap().clone();
    assert_eq!(saved["lid_close"], json!({"ac": "suspend", "battery": "hibernate"}));
    assert_eq!((saved["power_button"].clone(), saved["critical_action"].clone(), saved["critical_level"].clone(), saved["dim_before_lock"].clone()),
               (json!("suspend"), json!("poweroff"), json!(5), json!(false)));
    f.backend.set(|s| { s.capabilities.insert("hibernate".into(), "no".into()); });
    window.refresh();
    assert!(window.buttons_hint.text().contains("Hibernate is not available"));
    window.extra(&["critical_action"]).set_active_id(Some("nothing"));
    assert!(window.battery_hint.text().contains("shut down by itself"));
    println!("ok lid_button_dim_and_low_battery_options_save_together");
}

fn dim_overlay_covers_outputs_passes_input_and_restores() {
    use gtk4_layer_shell::LayerShell;
    let overlay = DimOverlay::default();
    if !overlay.available() {
        println!("skip dim overlay: layer shell unavailable");
        return;
    }
    assert!(overlay.dim());
    assert!(!overlay.dim(), "Dimming twice must not stack overlays");
    settle(0.2);
    let window = overlay.windows.borrow()[0].clone();
    assert!(window.is_mapped());
    assert!(window.width() > 200);
    assert!(window.has_css_class("power-dim-overlay"));
    assert_eq!(window.title().as_deref(), Some("Dim screen"));
    assert_eq!(window.namespace().as_deref(), Some("power-manager-dim"));
    assert_eq!(window.layer(), gtk4_layer_shell::Layer::Overlay);
    assert_eq!(window.keyboard_mode(), gtk4_layer_shell::KeyboardMode::None);
    overlay.restore();
    settle(0.06);
    assert!(!overlay.dimmed());
    overlay.close();
    println!("ok dim_overlay_covers_outputs_passes_input_and_restores");
}

fn remote_reload_shows_options_from_older_saved_files() {
    let f = Fx::new();
    let window = f.settings(&default_settings(), None);
    window.settings.borrow_mut()["power_button"] = "nothing".into();
    window.settings.borrow_mut()["dim_before_lock"] = false.into();
    window.sync_extra_controls();
    assert_eq!(window.extra(&["power_button"]).active_id().as_deref(), Some("nothing"));
    assert!(!window.dim_switch.is_active());
    assert_ne!(*window.settings.borrow(), *window.saved.borrow());
    println!("ok remote_reload_shows_options_from_older_saved_files");
}

fn css_parses_and_settings_window_contract() {
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = errors.clone();
    provider.connect_parsing_error(move |_, _, e| sink.borrow_mut().push(e.to_string()));
    provider.load_from_string(style::STYLE_CSS);
    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    let f = Fx::new();
    let window = f.settings(&default_settings(), None);
    assert_eq!(window.window.title().as_deref(), Some("Power Settings"));
    assert_eq!(window.window.default_size().0, 580, "580x760 default (height clamps to the 720 px headless output)");
    assert!(window.window.has_css_class("powermanager"));
    window.window.destroy();
    settle(0.05);
    assert!(window.destroyed());
    println!("ok css_parses_and_settings_window_contract");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var_os("POWER_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/power-manager/tests/run_headless.sh for the private compositor)");
        return;
    }
    gtk::init().expect("display");
    gtk::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
    header_icons_resolve_and_fit_in_light_and_dark_icon_themes();
    popup_refresh_is_read_only_and_selection_is_explicit();
    profile_unavailable_busy_and_rejection_do_not_fake_success();
    power_degraded_and_application_errors_are_visible_without_battery_widgets();
    popup_gear_hides_menu_and_opens_settings();
    idle_edits_are_local_until_save_and_close_discards();
    hidden_battery_policy_and_custom_seconds_are_preserved();
    unsupported_and_challenge_sleep_modes_stay_disabled_without_normalizing();
    immediate_profile_changes_preserve_unsaved_idle_values();
    save_failure_preserves_edits_and_combined_mode_explains_system_delay();
    lid_button_dim_and_low_battery_options_save_together();
    dim_overlay_covers_outputs_passes_input_and_restores();
    remote_reload_shows_options_from_older_saved_files();
    css_parses_and_settings_window_contract();
    println!("ui_headless: all checks passed");
}
