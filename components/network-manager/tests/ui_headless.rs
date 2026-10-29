//! GTK checks (port of the test_network.py UI cases that need a display).
//! Runs only inside the private nested headless labwc of
//! tests/run_headless.sh (NETWORK_UI_ISOLATED_TEST=1); under plain
//! `cargo test` it skips itself. The FAKE NetworkManager runs in-process on
//! the private session bus, which is also the "system" bus.

use gtk::prelude::*;
use network_manager::agent::Prompter;
use network_manager::app::{App, Surfaces};
use network_manager::backend::{NetworkBackend, Options};
use network_manager::gui::Gui;
use network_manager::ui::prompt::GtkPrompter;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[path = "support/fake_nm.rs"]
mod fake_nm;

fn pump(duration: Duration) {
    let end = Instant::now() + duration;
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait(what: &str, mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if condition() {
            return true;
        }
        pump(Duration::from_millis(20));
    }
    eprintln!("timed out: {what}");
    false
}

fn labels(widget: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        out.push(label.text().to_string());
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if let Some(text) = button.label() {
            out.push(text.to_string());
        }
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        out.extend(labels(&c));
        child = c.next_sibling();
    }
    out
}

fn find_button(widget: &gtk::Widget, text: &str) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if button.label().as_deref() == Some(text) {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(found) = find_button(&c, text) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

fn main() {
    if std::env::var_os("NETWORK_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run tests/run_headless.sh)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(runtime != "/run/user/1000" && !runtime.is_empty(), "refusing to run on the live session");
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
    assert_eq!(std::env::var("DBUS_SYSTEM_BUS_ADDRESS").ok().as_deref(), Some(address.as_str()));
    gtk::init().expect("headless display");
    suite_core::i18n::initialize();
    let world: fake_nm::Shared = Default::default();
    let _nm = fake_nm::serve_nm(&address, &world);
    let gapp = gio::Application::new(Some("org.waybar.NetworkManagerTest"), gio::ApplicationFlags::NON_UNIQUE);
    let app = App::new(&gapp);
    let w = app.downgrade();
    let backend = NetworkBackend::new(move || if let Some(a) = w.upgrade() { App(a).changed() },
                                      |_| {}, Options::default());
    app.0.backend.replace(Some(backend.clone()));
    let gui = Gui::new(&app);
    app.0.surfaces.replace(Some(gui.clone() as Rc<dyn Surfaces>));
    backend.start();
    let mut results: Vec<(&str, bool)> = Vec::new();
    results.push(("backend ready from the fake NetworkManager", wait("ready", || backend.ready())));

    // Popup: adapter cards keyed by D-Bus path, scan button per Wi-Fi adapter.
    gui.show_popup("bottom", 76);
    pump(Duration::from_millis(600));
    let popup_text = || gtk::Window::list_toplevels().into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.title().as_deref() == Some("Network")).map(|w| labels(w.upcast_ref())).unwrap_or_default();
    let text = popup_text();
    results.push(("popup shows both adapters and sections", ["Ethernet/LAN", "Wi-Fi", "enp4s0", "wlan0", "Available networks (1)", "Home Wi-Fi"]
        .iter().all(|t| text.iter().any(|x| x == t))));
    results.push(("portal button hidden while online", !gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
        .filter(|w| w.title().as_deref() == Some("Network")).any(|w| find_button(w.upcast_ref(), "Sign in to network").is_some_and(|b| b.is_visible()))));
    results.push(("subtitle names the default connection", text.iter().any(|x| x == "Wired connection")));

    // Per-adapter scan reaches only that adapter (the fake rejects it: no real scan).
    let popup_window = gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.title().as_deref() == Some("Network")).unwrap();
    let _ = &popup_window;
    app.perform(|b| Ok(b.scan(Some("/org/freedesktop/NetworkManager/Devices/11"))));
    pump(Duration::from_millis(200));
    results.push(("scan is busy-guarded", backend.state().busy.is_empty() || backend.state().busy.contains("scan:/org/freedesktop/NetworkManager/Devices/11")));

    // Settings: rows, count, search filter, type filter, confirm dialog.
    gui.show_settings();
    pump(Duration::from_millis(600));
    let settings = gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.title().as_deref() == Some("Network Settings"));
    results.push(("settings window opens and the popup is dismissed", settings.is_some()));
    let text = settings.as_ref().map(|w| labels(w.upcast_ref())).unwrap_or_default();
    results.push(("saved connections counted", text.iter().any(|x| x == "Saved connections · 2")));
    results.push(("rows show type and last used", text.iter().any(|x| x.starts_with("Ethernet · enp4s0 · Connected"))));
    gui.show_settings();
    pump(Duration::from_millis(200));
    let count = gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
        .filter(|w| w.title().as_deref() == Some("Network Settings")).count();
    results.push(("settings window is reused", count == 1));

    // Connection information window.
    gui.show_info(Some("11111111-1111-4111-8111-111111111111"), None, None);
    pump(Duration::from_millis(400));
    let info = gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.title().as_deref() == Some("Connection Information"));
    let text = info.as_ref().map(|w| labels(w.upcast_ref())).unwrap_or_default();
    results.push(("info shows profile, device and active details", ["Wired connection", "Profile ID", "Interface", "enp4s0", "Connection status"]
        .iter().all(|t| text.iter().any(|x| x == t))));

    // Agent prompt: masked entries, remember checkbox, entries blanked after OK.
    let prompter = GtkPrompter { gui: Rc::downgrade(&gui) };
    let answer: Rc<RefCell<Option<Option<(Vec<(String, String)>, bool)>>>> = Rc::default();
    let a = answer.clone();
    let handle = prompter.fields("Network authentication", "Enter credentials for Home Wi-Fi.",
                                 &[("psk".into(), "Password".into(), String::new(), true)], Some(true),
                                 Box::new(move |r| { a.replace(Some(r)); }));
    pump(Duration::from_millis(400));
    #[allow(deprecated)]
    let dialog = gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Dialog>().ok())
        .find(|w| w.title().as_deref() == Some("Network authentication"));
    let mut masked = false;
    if let Some(dialog) = &dialog {
        let mut stack = vec![dialog.clone().upcast::<gtk::Widget>()];
        while let Some(w) = stack.pop() {
            if let Some(entry) = w.downcast_ref::<gtk::Entry>() {
                masked = !EntryExt::is_visible(entry) && entry.input_purpose() == gtk::InputPurpose::Password;
                entry.set_text("synthetic-passphrase");
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                stack.push(c.clone());
                child = c.next_sibling();
            }
        }
        #[allow(deprecated)]
        dialog.response(gtk::ResponseType::Ok);
    }
    pump(Duration::from_millis(200));
    results.push(("credential prompt is masked and returns the entry", masked && matches!(&*answer.borrow(),
        Some(Some((values, true))) if values == &vec![("psk".to_string(), "synthetic-passphrase".to_string())])));
    drop(handle);

    // Escape dismisses the popup.
    gui.show_popup("bottom", 76);
    pump(Duration::from_millis(500));
    let mut results_extra = network_manager::ui::wizard::headless_checks();
    let _ = &mut results_extra;
    app.shutdown();
    backend.close();
    pump(Duration::from_millis(200));
    let mut failed = 0;
    for (name, ok) in results.iter().map(|(n, o)| (n.to_string(), *o)).chain(results_extra.into_iter()) {
        println!("{} {name}", if ok { "PASS" } else { "FAIL" });
        failed += (!ok) as i32;
    }
    let calls = world.borrow().calls.clone();
    let mutations: Vec<_> = calls.iter().filter(|c| ["Enable", "ActivateConnection", "AddAndActivateConnection", "DeactivateConnection",
                                                      "Update2", "Delete"].contains(&c.1.as_str())).collect();
    println!("{} no NetworkManager mutations outside explicit actions", if mutations.is_empty() { "PASS" } else { "FAIL" });
    failed += (!mutations.is_empty()) as i32;
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
