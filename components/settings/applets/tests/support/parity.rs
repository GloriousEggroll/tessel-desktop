//! Fake-HOME parity: the Python page (tests/parity_driver.py) and the Rust
//! page (this binary with `--rust-driver`) perform the same actions in
//! separate fake homes; every file they leave under ~/.config and
//! ~/.local/state must be byte-identical.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use gs_applets::*;
use gs_base::Host;
use gtk::prelude::*;

use crate::{new_host, settle};

pub const SCENARIOS: &[&str] = &["clock", "capture", "weather", "bluetooth", "network"];

fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for top in [".config", ".local/state"] {
        let base = root.join(top);
        let mut pending = vec![base.clone()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(data) = std::fs::read(&path) {
                    out.insert(path.strip_prefix(root).unwrap().display().to_string(), data);
                }
            }
        }
    }
    out
}

fn home_env(command: &mut Command, home: &Path) {
    command.env("HOME", home).env("XDG_CONFIG_HOME", home.join(".config")).env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_DATA_HOME", home.join(".local/share")).env("XDG_CACHE_HOME", home.join(".cache"));
}

fn run(scenario: &str, home: &Path, python: bool) -> bool {
    let _ = std::fs::remove_dir_all(home);
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let mut command = if python {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/parity_driver.py");
        let mut c = Command::new("python3");
        c.arg("-I").arg(script).arg(scenario);
        c
    } else {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.arg("--rust-driver").arg(scenario);
        c
    };
    home_env(&mut command, home);
    match command.output() {
        Ok(output) => {
            if !output.status.success() {
                eprintln!("{} driver {scenario} failed: {}\n{}", if python { "python" } else { "rust" },
                          String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            }
            output.status.success()
        }
        Err(error) => {
            eprintln!("cannot run driver: {error}");
            false
        }
    }
}

/// Run every scenario; returns (passed, failed names).
pub fn run_all(root: &Path) -> (usize, Vec<String>) {
    let mut passed = 0;
    let mut failed = Vec::new();
    for scenario in SCENARIOS {
        let base = root.join("parity").join(scenario);
        let (py, rs) = (base.join("py"), base.join("rs"));
        let ok = run(scenario, &py, true) && run(scenario, &rs, false);
        let (a, b) = (files(&py), files(&rs));
        let name = format!("parity_{scenario}");
        if ok && !a.is_empty() && a == b {
            passed += 1;
            println!("ok   {name} ({} file(s): {:?})", a.len(), a.keys().collect::<Vec<_>>());
        } else {
            println!("FAIL {name}");
            for key in a.keys().chain(b.keys()).collect::<std::collections::BTreeSet<_>>() {
                if a.get(key) != b.get(key) {
                    println!("  {key}\n    python: {:?}\n    rust:   {:?}", a.get(key).map(|d| String::from_utf8_lossy(d).into_owned()),
                             b.get(key).map(|d| String::from_utf8_lossy(d).into_owned()));
                }
            }
            failed.push(name);
        }
    }
    (passed, failed)
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut child = widget.first_child();
    while let Some(c) = child {
        out.push(c.clone());
        out.extend(descendants(&c));
        child = c.next_sibling();
    }
    out
}

/// The Rust side of one scenario (same actions as parity_driver.py).
pub fn rust_driver(scenario: &str) -> i32 {
    let app = gtk::Application::new(None::<&str>, gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gio::Cancellable>).expect("register");
    let host = new_host(&app);
    let dyn_host: Host = host.clone();
    let mut factories = Factories::default();
    let page = match scenario {
        "clock" => {
            factories.datetime = Some(Box::new(|changed, failed| {
                tessel_calendar::fake::FakeDateBackend::new(move || changed(), move |m| failed(m))
            }));
            let page = build_page_with("datetime", &dyn_host, &factories).unwrap();
            host.window.set_child(Some(&page.widget));
            host.window.present();
            page.activate();
            settle(0.2);
            let c = page.controller::<DatePage>().unwrap().controller.0.clone();
            c.clock_stack_switch.set_active(false);
            c.clock_reverse_switch.set_active(true);
            c.clock_hour_combo.set_selected(1);
            c.clock_date_combo.set_selected(3);
            c.clock_show_year_switch.set_active(false);
            c.clock_show_weekday_switch.set_active(true);
            c.clock_show_date_switch.set_active(false);
            c.week_start_combo.set_selected(1);
            page
        }
        "capture" => {
            let page = build_page_with("capture", &dyn_host, &factories).unwrap();
            host.window.set_child(Some(&page.widget));
            host.window.present();
            settle(0.2);
            let targets = page.search_targets.borrow().clone();
            targets["copy_to_clipboard"].downcast_ref::<gtk::Switch>().unwrap().set_active(false);
            targets["open_editor"].downcast_ref::<gtk::Switch>().unwrap().set_active(true);
            page
        }
        "weather" => {
            let page = build_page_with("weather", &dyn_host, &factories).unwrap();
            host.window.set_child(Some(&page.widget));
            host.window.present();
            page.activate();
            settle(0.2);
            let c = page.controller::<WeatherPage>().unwrap().controller.clone();
            c.location_name.set_text("Springfield");
            c.latitude.set_value(39.7817);
            c.longitude.set_value(-89.6501);
            c.timezone.set_text("America/Chicago");
            c.manual_apply.emit_clicked();
            settle(0.2);
            c.units.set_selected(1);
            c.week.set_selected(1);
            c.refresh_minutes.set_value(45.0);
            page
        }
        "bluetooth" => {
            factories.bluetooth = Some(Box::new(|| Ok(tessel_bluetooth::testing::FakeBluez::new("multi-adapter", false).bt.clone())));
            let page = build_page_with("bluetooth", &dyn_host, &factories).unwrap();
            host.window.set_child(Some(&page.widget));
            host.window.present();
            page.activate();
            settle(0.2);
            let ui = page.controller::<BluetoothPage>().unwrap().controller.clone();
            ui.receive_combo.as_ref().unwrap().set_selected(0);
            page
        }
        "network" => {
            let page = build_page_with("network", &dyn_host, &factories).unwrap();
            host.window.set_child(Some(&page.widget));
            host.window.present();
            page.activate();
            settle(0.2);
            let net = page.controller::<NetworkPage>().unwrap();
            net.gui.show_preferences();
            settle(0.2);
            let dialog = net.gui.children().last().cloned().unwrap();
            let checks: Vec<gtk::CheckButton> = descendants(dialog.upcast_ref()).into_iter()
                .filter_map(|w| w.downcast::<gtk::CheckButton>().ok()).collect();
            checks[0].set_active(false);
            checks[3].set_active(false);
            page
        }
        other => {
            eprintln!("unknown scenario {other}");
            return 2;
        }
    };
    settle(0.3);
    page.close();
    host.window.destroy();
    settle(0.1);
    let _ = Rc::strong_count(&host);
    0
}
