//! Apply's 10 second question against the real `tessel-displays --daemon`, with
//! the real IPC, in the private nested labwc of `run_trial_headless.sh`. Output
//! state comes from wlr-randr, not from what the GUI believes.
use gtk::glib;
use gtk::prelude::*;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_displays::window::{self, ClientFactory, Hooks, TesselDisplaysApp};

fn settle(seconds: f64) {
    let context = glib::MainContext::default();
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn outputs() -> Vec<Value> {
    let out = Command::new("wlr-randr").arg("--json").output().expect("wlr-randr");
    serde_json::from_slice(&out.stdout).expect("wlr-randr json")
}

fn output(name: &str) -> Value {
    outputs().into_iter().find(|o| o["name"] == name).unwrap_or_else(|| panic!("no {name}"))
}

/// Wait in the main loop, so timers run while the outputs change.
fn wait_for(what: &str, seconds: f64, mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        if check() {
            return;
        }
        settle(0.1);
    }
    panic!("timed out: {what}");
}

fn saved() -> String {
    let path = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tessel/displays/cfg.yaml");
    std::fs::read_to_string(path).unwrap_or_default()
}

fn screenshot(name: &str) {
    if let Ok(folder) = std::env::var("TRIAL_SHOTS") {
        let _ = Command::new("grim").arg(format!("{folder}/{name}.png")).status();
    }
}

fn widget(window: &TesselDisplaysApp, connector: &str) -> Rc<tessel_displays::monitor_widget::MonitorWidget> {
    window.monitor_widgets.borrow().iter().find(|w| w.name().as_deref() == Some(connector)).unwrap().clone()
}

fn scale(name: &str) -> f64 {
    output(name)["scale"].as_f64().unwrap_or(0.0)
}

fn main() {
    assert_eq!(std::env::var("TESSEL_DISPLAYS_TRIAL_ISOLATED").as_deref(), Ok("1"), "run tests/run_trial_headless.sh");
    gtk::init().unwrap();
    let application = gtk::Application::builder().application_id("org.tessel.TrialTest").build();
    application.register(None::<&gtk::gio::Cancellable>).unwrap();
    let folder = PathBuf::from(std::env::var("TMPDIR").unwrap());
    std::fs::write(folder.join("rc.xml"), "<labwc_config>\n</labwc_config>\n").unwrap();

    let hooks = Hooks { confirm_seconds: 3, ..Hooks::default() };
    window::set_default_hooks(Some(hooks));
    assert!(tessel_displays::ipc::get_config_data(), "the daemon answers");
    let factory: ClientFactory = Box::new(|_| Err("no brightness in this test".to_owned()));
    let window = TesselDisplaysApp::new(&application, Some(factory), false);
    window.labwc_environment_path.replace(folder.join("environment"));
    window.labwc_config_path.replace(folder.join("rc.xml"));
    window.window.present();
    settle(0.4);
    let file_before = saved();
    assert_eq!(scale("HEADLESS-1"), 1.0);

    // Nobody answers: the scale goes live, then comes back, and is never saved.
    let target = widget(&window, "HEADLESS-1");
    target.scale_auto_switch.set_active(false);
    target.scale_slider.set_value(1.5);
    window.save_config();
    wait_for("scale goes live", 2.0, || scale("HEADLESS-1") == 1.5);
    assert!(window.trial_dialog().is_some());
    assert_eq!(output("HEADLESS-2")["enabled"], true, "Apply leaves a display under the lid rule on");
    settle(0.3);
    screenshot("question");
    assert_eq!(saved(), file_before, "nothing saved during the trial");
    wait_for("the countdown reverts the scale", 6.0, || scale("HEADLESS-1") == 1.0);
    assert!(window.trial_dialog().is_none());
    assert_eq!(saved(), file_before, "a reverted setting is not saved");
    println!("ok no answer: scale reverted, not saved");

    // A screen turned off, as in a real black screen: nobody can answer. This
    // one is under the lid rule, so it is switched by name, without a saved list.
    let target = widget(&window, "HEADLESS-2");
    target.enable_switch.set_active(false);
    window.save_config();
    wait_for("the output turns off", 2.0, || output("HEADLESS-2")["enabled"] == false);
    wait_for("the countdown turns it back on", 6.0, || output("HEADLESS-2")["enabled"] == true);
    assert!(window.trial_dialog().is_none());
    assert_eq!(saved(), file_before);
    println!("ok no answer: output turned off and back on");

    // Revert button path.
    let target = widget(&window, "HEADLESS-1");
    target.scale_auto_switch.set_active(false);
    target.scale_slider.set_value(1.25);
    window.save_config();
    wait_for("scale goes live", 2.0, || scale("HEADLESS-1") == 1.25);
    window.revert_display_changes();
    wait_for("Revert brings it back", 2.0, || scale("HEADLESS-1") == 1.0);
    assert_eq!(saved(), file_before);
    println!("ok Revert");

    // Keep Changes: stays live, past the countdown, and is saved.
    let target = widget(&window, "HEADLESS-1");
    target.scale_auto_switch.set_active(false);
    target.scale_slider.set_value(1.5);
    window.save_config();
    wait_for("scale goes live", 2.0, || scale("HEADLESS-1") == 1.5);
    window.keep_display_changes();
    settle(4.0);
    assert_eq!(scale("HEADLESS-1"), 1.5, "kept past the countdown");
    assert!(saved().contains("Headless output 1: 1.5"), "saved: {}", saved());
    assert!(saved().contains("!^HEADLESS-2"), "the lid rule stays saved: {}", saved());
    println!("ok Keep Changes: live and saved");
    println!("trial_real_daemon: all checks passed");
}
