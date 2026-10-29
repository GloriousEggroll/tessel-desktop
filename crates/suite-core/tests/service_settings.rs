//! Real D-Bus transport tests on a private dbus-daemon (gio::TestDBus) with
//! memory-only settings; never touches the user's session bus.

use gio::prelude::*;
use glib::Variant;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use suite_core::service_settings::{SettingsClient, SettingsService};

fn wait(condition: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

struct Fixture {
    app: gio::Application,
    state: Rc<RefCell<Value>>,
    writes: Rc<RefCell<Vec<Value>>>,
    service: SettingsService,
    client: SettingsClient,
}

fn fixture(index: usize) -> Fixture {
    let name = format!("org.test.Settings{index}x{}", std::process::id());
    let app = gio::Application::new(Some(&name), gio::ApplicationFlags::FLAGS_NONE);
    app.register(None::<&gio::Cancellable>).unwrap();
    let state = Rc::new(RefCell::new(json!({"ready": true, "settings": {"timeout": 10}})));
    let writes = Rc::new(RefCell::new(Vec::new()));
    let (snapshot_state, apply_state, apply_writes) = (state.clone(), state.clone(), writes.clone());
    let service = SettingsService::new(&app, "org.test.Controls", "SetSettings", "s",
        move || snapshot_state.borrow().clone(),
        move |value: &Variant| {
            let updated: Value = serde_json::from_str(&value.get::<String>().unwrap()).map_err(|e| e.to_string())?;
            if updated["timeout"].as_i64().unwrap_or(0) < 0 {
                return Ok((false, "Rejected".into()));
            }
            apply_state.borrow_mut()["settings"] = updated.clone();
            apply_writes.borrow_mut().push(updated);
            Ok((true, String::new()))
        }).unwrap();
    let client = SettingsClient::new(|| {}, &name, "org.test.Controls", "Not running");
    client.start();
    assert!(wait(|| client.ready()));
    Fixture { app, state, writes, service, client }
}

fn teardown(fixture: Fixture) {
    fixture.client.close();
    fixture.service.close();
    drop(fixture.app);
}

#[test]
fn transport_on_private_bus() {
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();

    // Read-only start and live updates.
    let f = fixture(1);
    assert!(f.writes.borrow().is_empty());
    assert_eq!(f.client.state()["settings"], json!({"timeout": 10}));
    f.state.borrow_mut()["settings"] = json!({"timeout": 20});
    f.service.changed();
    assert!(wait(|| f.client.state()["settings"]["timeout"] == 20));
    teardown(f);

    // Write acknowledgement and rejection.
    let f = fixture(2);
    let replies = Rc::new(RefCell::new(Vec::new()));
    let sink = replies.clone();
    f.client.call("SetSettings", Some(&(r#"{"timeout": 30}"#,).to_variant()),
                  Some(Box::new(move |ok, error, _| sink.borrow_mut().push((ok, error)))));
    assert!(wait(|| !replies.borrow().is_empty()));
    assert_eq!(replies.borrow()[0], (true, String::new()));
    assert_eq!(*f.writes.borrow(), vec![json!({"timeout": 30})]);
    let sink = replies.clone();
    f.client.call("SetSettings", Some(&(r#"{"timeout": -1}"#,).to_variant()),
                  Some(Box::new(move |ok, error, _| sink.borrow_mut().push((ok, error)))));
    assert!(wait(|| replies.borrow().len() == 2));
    assert_eq!(replies.borrow()[1], (false, "Rejected".to_owned()));
    assert_eq!(f.state.borrow()["settings"]["timeout"], 30);
    teardown(f);

    // A missing service is not started.
    let absent = SettingsClient::new(|| {}, "org.test.NeverStarted", "org.test.Controls", "Not running");
    absent.start();
    assert!(wait(|| !absent.error().is_empty()));
    assert!(!absent.ready());
    assert_eq!(absent.error(), "Not running");
    absent.close();

    // Closing the client does not change the service.
    let f = fixture(3);
    f.client.close();
    f.service.changed();
    let settled = Instant::now() + Duration::from_millis(150);
    wait(|| Instant::now() > settled);
    assert_eq!(f.state.borrow()["settings"], json!({"timeout": 10}));
    assert!(f.writes.borrow().is_empty());
    teardown(f);

    bus.down();
}
