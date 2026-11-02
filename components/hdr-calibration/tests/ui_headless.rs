//! Port of test_gui.py. Runs only inside the private nested headless labwc
//! (tests/run_headless.sh); under plain `cargo test` it skips itself. Every
//! backend call (status, profiles, apply, report) is a recording fake: no
//! rc.xml, profile, report or compositor signal is ever produced.

use gtk::prelude::*;
use gtk::{gio, glib};
use tessel_hdr_calibration::calibration::{CalError, Profile, STATUS_FIELDS};
use tessel_hdr_calibration::ui::{Backend, Window};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use way_displays::color_config::Status;

fn settle() {
    let end = Instant::now() + Duration::from_millis(200);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn profile() -> Profile {
    json!({"version": 1, "connector": "DP-1", "displayId": "0123456789abcdef", "mode": "HDR True Black", "method": "visual",
           "black": 0, "peak": 1000, "fullFrame": 300, "white": 160}).as_object().unwrap().clone()
}

fn base_status() -> Status {
    let mut status = Status::new();
    status.insert("compositor".into(), [("hdr_calibration_supported", "true"), ("hdr_correction_supported", "true"), ("pid", "1234")]
        .into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect());
    status.insert("DP-1".into(), [("enabled", "true"), ("hdr", "true"), ("bit_depth", "10"), ("display_id", "0123456789abcdef"),
        ("description", "Example HDR display (DP-1)"), ("hdr_reference_white_nits", "203"), ("hdr_peak_nits", "1000"),
        ("hdr_max_frame_average_nits", "300")].into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect());
    status
}

#[derive(Default)]
struct Fake {
    status: Mutex<Status>,
    session_error: Mutex<Option<String>>,
    profiles: Mutex<Vec<Profile>>,
    applied: Mutex<Vec<(String, Option<Profile>)>>,
    reports: Mutex<usize>,
}

impl Backend for Fake {
    fn session(&self) -> Result<Status, CalError> {
        match self.session_error.lock().unwrap().clone() {
            Some(error) => Err(CalError::Value(error)),
            None => Ok(self.status.lock().unwrap().clone()),
        }
    }
    fn read_status(&self) -> Status { self.status.lock().unwrap().clone() }
    fn read_profiles(&self) -> Result<Vec<Profile>, CalError> { Ok(self.profiles.lock().unwrap().clone()) }
    fn apply(&self, connector: &str, profile: Option<&Profile>) -> Result<PathBuf, CalError> {
        self.applied.lock().unwrap().push((connector.to_owned(), profile.cloned()));
        Ok(PathBuf::from("/example-backup"))
    }
    fn workspace_report(&self, _connector: &str, _profile: &Profile) -> Result<PathBuf, CalError> {
        *self.reports.lock().unwrap() += 1;
        Ok(PathBuf::from("/example-report"))
    }
}

fn worker(app: &gtk::Application) {
    let fake = Arc::new(Fake::default());
    *fake.status.lock().unwrap() = base_status();
    *fake.profiles.lock().unwrap() = vec![profile()];
    let window = Window::new(app, Some("DP-1".into()), fake.clone());
    window.window.present();
    settle();
    assert_eq!(window.connector().as_deref(), Some("DP-1"));
    assert!(window.controls.is_sensitive());
    let capability = window.capability.text();
    assert!(capability.contains("HDR is on") && !capability.contains("Meter"), "{capability}");
    assert_eq!(capability, "Example HDR display (DP-1)\nHDR is on · SDR white is 203 nits · Using the monitor’s default HDR settings");
    window.confirm_reset();
    settle();
    assert!(fake.applied.lock().unwrap().is_empty(), "waits for the confirmation dialog");
    window.reset_dialog.replace(None);
    window.saved.set_selected(1);
    assert_eq!(window.values["white"].value(), 160.0);
    assert_eq!(window.profile().unwrap()["displayId"], json!("0123456789abcdef"));
    window.preset.set_selected(1);
    assert_eq!(window.profile().unwrap()["white"], json!(100.0));
    assert_eq!(window.profile().unwrap()["preset"], json!("sdr-video"));
    window.values["white"].set_value(110.0);
    assert_eq!(window.profile().unwrap()["preset"], json!("custom"));
    window.preset.set_selected(2);
    assert_eq!(window.profile().unwrap()["white"], json!(120.0));
    assert!(window.choose_correction.is_sensitive());
    window.method.set_selected(1);
    assert!(WidgetExt::is_visible(&window.instrument));
    assert!(!window.pattern_buttons[0].is_sensitive());
    window.method.set_selected(0);
    window.apply(false);
    settle();
    assert!(!window.worker.get());
    assert_eq!(fake.applied.lock().unwrap().len(), 1);
    assert!(window.outcome.text().contains("applied"), "{}", window.outcome.text());
    assert_eq!(*fake.reports.lock().unwrap(), 1);
    if let Some(path) = std::env::var_os("HDR_TEST_SCREENSHOT") {
        let _ = std::process::Command::new("grim").arg(path).status();
    }
    fake.status.lock().unwrap().get_mut("DP-1").unwrap().insert("hdr".into(), "false".into());
    window.refresh();
    assert!(!window.controls.is_sensitive());
    window.close();
    settle();
    println!("ok monitor selection, saved modes, presets, measurement provenance, apply, SDR gating");

    let correction = json!({"path": "/example/example.cube", "sha256": "a".repeat(64), "size": 2, "encoding": "BT2020-PQ"});
    let mut corrected = profile();
    corrected.insert("correction".into(), correction.clone());
    {
        let mut status = fake.status.lock().unwrap();
        let monitor = status.get_mut("DP-1").unwrap();
        for (key, value) in [("hdr", "true"), ("hdr_calibration_active", "true"), ("hdr_correction_active", "true"),
                             ("hdr_correction_path", "/example/example.cube")] {
            monitor.insert(key.into(), value.into());
        }
        monitor.insert("hdr_correction_sha256".into(), "a".repeat(64));
        for (key, value) in STATUS_FIELDS.iter().zip(["0", "1000", "300", "160"]) {
            monitor.insert((*key).into(), value.into());
        }
    }
    *fake.profiles.lock().unwrap() = vec![corrected];
    let window = Window::new(app, Some("DP-1".into()), fake.clone());
    window.preset.set_selected(1);
    assert_eq!(window.profile().unwrap()["correction"], correction);
    assert_eq!(window.profile().unwrap()["mode"], json!("HDR True Black"));
    window.close();
    println!("ok active saved correction is kept");

    fake.profiles.lock().unwrap().clear();
    let window = Window::new(app, Some("DP-1".into()), fake.clone());
    assert!(window.profile().is_err(), "An unknown active correction must not be silently removed");
    window.set_correction(None);
    assert_eq!(window.profile().unwrap()["correction"], Value::Null);
    window.close();
    println!("ok unknown active correction blocks apply until cleared");

    *fake.session_error.lock().unwrap() = Some("Updated labwc required".into());
    let window = Window::new(app, None, fake.clone());
    assert!(!window.controls.is_sensitive());
    assert!(window.capability.text().contains("Updated labwc"));
    assert_eq!(window.pages.visible_child_name().as_deref(), Some("empty"));
    window.close();
    settle();
    println!("ok old compositor gating");
}

fn window_contract(app: &gtk::Application) {
    let fake = Arc::new(Fake::default());
    *fake.status.lock().unwrap() = base_status();
    let window = Window::new(app, None, fake.clone());
    assert_eq!(window.window.title().as_deref(), Some("HDR Calibration"));
    assert_eq!(window.window.default_size(), (760, 840));
    for class in ["way-displays", "tessel-hdr-calibration"] {
        assert!(window.window.has_css_class(class));
    }
    assert!(window.apply_button.has_css_class("primary"));
    assert!(tessel_gtk::app_menu::has_menu_icon(&window.menu_button));
    assert_eq!(window.mode.text(), "Current HDR mode");
    // Defaults from the status: peak 1000, full 300, white 203.
    let values: BTreeMap<&str, f64> = window.values.iter().map(|(k, s)| (*k, s.value())).collect();
    assert_eq!(values, BTreeMap::from([("black", 0.0), ("fullFrame", 300.0), ("peak", 1000.0), ("white", 203.0)]));
    // Closing is refused while an apply is running.
    window.worker.set(true);
    assert!(window.close_requested());
    assert!(window.outcome.text().contains("Finishing"));
    window.worker.set(false);
    window.close();
    settle();
    println!("ok window_contract");
}

/// `hdr_calibration.py --smoke-test` equivalent: the real binary (live backend,
/// read-only unless Apply is pressed) starts and quits by itself in this compositor.
fn smoke_test() {
    let Some(binary) = std::env::var_os("HDR_CALIBRATION_BIN") else {
        println!("skip smoke_test (HDR_CALIBRATION_BIN unset)");
        return;
    };
    let mut child = std::process::Command::new(&binary).arg("--smoke-test").spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > end {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.is_none() {
        let _ = child.kill(); // our own child only
        let _ = child.wait();
    }
    assert_eq!(status.and_then(|s| s.code()), Some(0));
    println!("ok smoke_test");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none()
        || std::env::var_os("HDR_CALIBRATION_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/hdr-calibration/tests/run_headless.sh for the private compositor)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("display");
    let app = gtk::Application::new(Some("org.desktop.HDRCalibration.Test"), gio::ApplicationFlags::NON_UNIQUE);
    app.register(gio::Cancellable::NONE).unwrap();
    worker(&app);
    window_contract(&app);
    smoke_test();
    println!("ui_headless: all checks passed");
}
