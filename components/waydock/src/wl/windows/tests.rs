//! Port of test_window_backend.py: synthetic helper events (EventTests) and a
//! scripted connection over suite-wayland's model (ProcessTests: the Python
//! fake helper process becomes a FakeLink, GLib timers a FakeScheduler).

use super::*;
use crate::wl::link::fake::{FakeConnector, FakeLink};
use serde_json::json;
use std::cell::Cell;
use suite_core::scheduler::FakeScheduler;
use suite_wayland::model::{toplevel_state, Model};
use suite_wayland::ActionError;

struct Harness {
    backend: WindowBackend,
    connector: Rc<FakeConnector>,
    scheduler: Rc<FakeScheduler>,
    errors: Rc<RefCell<Vec<String>>>,
    changes: Rc<Cell<usize>>,
}

fn harness() -> Harness {
    let connector = FakeConnector::new();
    let scheduler = Rc::new(FakeScheduler::default());
    let errors = Rc::new(RefCell::new(Vec::new()));
    let changes = Rc::new(Cell::new(0));
    let (e, c) = (errors.clone(), changes.clone());
    let backend = WindowBackend::with_parts(move || c.set(c.get() + 1), move |m| e.borrow_mut().push(m.to_owned()),
                                            connector.clone(), scheduler.clone());
    Harness { backend, connector, scheduler, errors, changes }
}

/// Run every zero-delay timer (GLib idle callbacks).
fn idle(scheduler: &FakeScheduler) {
    loop {
        let ids: Vec<u64> = scheduler.pending.borrow().iter().filter(|p| p.1 == 0).map(|p| p.0).collect();
        if ids.is_empty() {
            return;
        }
        for id in ids {
            scheduler.fire(id);
        }
    }
}

fn fire_delay(scheduler: &FakeScheduler, delay: u32) -> bool {
    let id = scheduler.pending.borrow().iter().find(|p| p.1 == delay).map(|p| p.0);
    id.is_some_and(|id| scheduler.fire(id))
}

// ---- EventTests -------------------------------------------------------------------------

fn event_backend() -> Harness {
    let h = harness();
    h.backend.set_session(4);
    h
}

fn window_event(changes: Value) -> Value {
    let mut value = json!({"event": "window", "id": "3", "app_id": "org.example.App",
        "title": "quoted \"title\"\nwith newline", "outputs": ["DP-1", "DP-2"],
        "activated": true, "fullscreen": true, "minimized": false});
    for (key, item) in changes.as_object().unwrap() {
        value[key] = item.clone();
    }
    value
}

#[test]
fn exact_ids_state_parent_and_outputs_survive_events() {
    let h = event_backend();
    h.backend.consume_event(&window_event(json!({"parent": "7"}))).unwrap();
    let window = h.backend.window("4:3").unwrap();
    assert_eq!(window.id, "4:3");
    assert_eq!(window.parent_id, "4:7");
    assert_eq!(window.outputs, ["DP-1", "DP-2"]);
    assert_eq!(window.title, "quoted \"title\"\nwith newline");
    assert!(window.activated && window.fullscreen);
    assert!(!window.minimized);
    assert!(!WindowBackend::WORKSPACE_VISIBILITY_KNOWN);
}

#[test]
fn windows_from_same_app_remain_separate_and_close_only_one() {
    let h = event_backend();
    h.backend.consume_event(&window_event(json!({}))).unwrap();
    h.backend.consume_event(&window_event(json!({"id": "8", "title": "Second window"}))).unwrap();
    h.backend.consume_event(&json!({"event": "closed", "id": "3"})).unwrap();
    assert_eq!(h.backend.windows().keys().collect::<Vec<_>>(), ["4:8"]);
}

#[test]
fn maximized_state_is_distinct_from_fullscreen_and_resets() {
    let h = event_backend();
    h.backend.consume_event(&window_event(json!({"maximized": true, "fullscreen": false}))).unwrap();
    let maximized = h.backend.window("4:3").unwrap();
    assert!(maximized.maximized && maximized.activated && !maximized.fullscreen);
    h.backend.consume_event(&window_event(json!({"maximized": false, "fullscreen": false}))).unwrap();
    assert!(!h.backend.window("4:3").unwrap().maximized);
    assert!(maximized.maximized, "published window snapshots remain immutable");
    h.backend.consume_event(&window_event(json!({"maximized": true}))).unwrap();
    // An older helper may omit the field.
    h.backend.consume_event(&window_event(json!({"fullscreen": true}))).unwrap();
    assert!(!h.backend.window("4:3").unwrap().maximized);
    assert!(h.backend.window("4:3").unwrap().fullscreen);
}

#[test]
fn window_constructor_defaults() {
    let window = Window::new("4:3", "org.example.App", "Title");
    assert!(window.outputs.is_empty() && window.parent_id.is_empty() && !window.maximized);
}

#[test]
fn logical_output_geometry_and_hot_unplug_are_tracked() {
    let h = event_backend();
    h.backend.consume_event(&json!({"event": "output", "id": 11, "name": "DP-1",
        "x": -2560, "y": 0, "width": 2560, "height": 1440, "scale": 2})).unwrap();
    let output = h.backend.outputs().get("DP-1").cloned().unwrap();
    assert_eq!((output.x, output.width, output.height, output.scale), (-2560, 2560, 1440, 2));
    h.backend.consume_event(&json!({"event": "output-removed", "id": 11})).unwrap();
    assert!(h.backend.outputs().is_empty());
}

#[test]
fn output_name_replacement_does_not_leave_stale_monitor() {
    let h = event_backend();
    for name in ["DP-1", "DP-2"] {
        h.backend.consume_event(&json!({"event": "output", "id": 11, "name": name})).unwrap();
    }
    assert_eq!(h.backend.outputs().keys().collect::<Vec<_>>(), ["DP-2"]);
    assert_eq!(h.backend.outputs()["DP-2"].scale, 1);
}

#[test]
fn malformed_ids_and_output_payloads_are_rejected() {
    let h = event_backend();
    for ident in [json!(""), json!("0"), json!("-3"), json!("3\nclose 9"), json!(3), Value::Null, json!("3:9"),
                  json!("123456789012345678901")] {
        assert!(h.backend.consume_event(&window_event(json!({"id": ident.clone()}))).is_err(), "{ident}");
    }
    assert!(h.backend.consume_event(&window_event(json!({"outputs": "DP-1"}))).is_err());
    assert!(h.backend.consume_event(&window_event(json!({"outputs": [1]}))).is_err());
    assert!(h.backend.consume_event(&window_event(json!({"title": 5}))).is_err());
    assert!(h.backend.windows().is_empty());
    for bad in [json!({"event": "output", "id": 1}), json!({"event": "output", "id": 1, "name": ""}),
                json!({"event": "output", "name": "DP-1"}), json!({"event": "output", "id": "x", "name": "DP-1"}),
                json!({"event": "output", "id": 1, "name": "DP-1", "x": null}), json!({"event": "nope"}),
                json!({"id": "3"}), json!([1]), json!({"event": "output-removed"})] {
        assert!(h.backend.consume_event(&bad).is_err(), "{bad}");
    }
    assert!(h.backend.outputs().is_empty());
}

#[test]
fn unavailable_or_stale_actions_never_send_requests() {
    let h = harness();
    h.backend.start();
    let link = FakeLink::toplevel();
    // Not ready yet: nothing is sent.
    h.backend.consume_event(&window_event(json!({}))).unwrap();
    assert!(!h.backend.activate("1:3"));
    assert_eq!(h.backend.error(), "Window management is unavailable.");
    assert!(h.connector.ready(&link));
    assert!(h.backend.ready());
    h.backend.consume_event(&window_event(json!({}))).unwrap();
    assert!(!h.backend.activate("0:3"));
    assert!(!h.backend.activate("1:9"));
    assert!(!h.backend.activate(""));
    assert!(!h.backend.minimize("1:3x"));
    assert_eq!(h.backend.error(), "That window is no longer available.");
    assert!(link.requests.borrow().is_empty());
    assert!(h.backend.activate("1:3"));
    assert_eq!(*link.requests.borrow(), [Request::Activate(3)]);
}

#[test]
fn missing_seat_refuses_activation() {
    let h = harness();
    h.backend.start();
    let link = FakeLink::toplevel();
    link.seat.set(false);
    h.connector.ready(&link);
    assert!(h.backend.ready() && !h.backend.can_activate());
    h.backend.consume_event(&window_event(json!({}))).unwrap();
    assert!(!h.backend.activate("1:3"));
    assert!(h.backend.error().contains("input seat"));
    assert!(h.backend.minimize("1:3"), "other actions need no seat");
    assert_eq!(*link.requests.borrow(), [Request::Minimize(3)]);
}

#[test]
fn error_event_reports_without_dropping_state() {
    let h = event_backend();
    h.backend.consume_event(&window_event(json!({}))).unwrap();
    h.backend.consume_event(&json!({"event": "error", "message": "That window is no longer available."})).unwrap();
    h.backend.consume_event(&json!({"event": "error"})).unwrap();
    assert_eq!(*h.errors.borrow(), ["That window is no longer available.", "The window request failed."]);
    assert_eq!(h.backend.windows().len(), 1);
}

// ---- ProcessTests (scripted connection) ---------------------------------------------------

/// The Python FAKE_HELPER: output DP-1 at -1920, one window, seat.
fn fake_helper() -> (Rc<FakeLink>, u64) {
    let link = FakeLink::toplevel();
    let id = {
        let mut m = link.model.borrow_mut();
        m.output_added(4);
        m.output_name(4, "DP-1");
        m.logical_position(4, -1920, 0);
        m.logical_size(4, 1920, 1080);
        m.output_scale(4, 1);
        m.output_done(4);
        let id = m.toplevel_new();
        m.toplevel_app_id(id, "org.example.Test");
        m.toplevel_title(id, "Synthetic window");
        m.toplevel_output_enter(id, 4);
        m.toplevel_state(id, &[]);
        m.toplevel_done(id);
        id
    };
    link.respond(move |m: &mut Model, request| {
        let state = |m: &mut Model, states: &[u32]| {
            m.toplevel_state(id, states);
            m.toplevel_done(id);
        };
        match request {
            Request::Activate(n) if n == id => state(m, &[toplevel_state::ACTIVATED]),
            Request::Minimize(n) if n == id => state(m, &[toplevel_state::MINIMIZED]),
            Request::Unminimize(n) if n == id => state(m, &[]),
            Request::Close(n) if n == id => m.toplevel_closed(id),
            _ => return Err(ActionError::NoWindow),
        }
        Ok(())
    });
    (link, id)
}

#[test]
fn async_discovery_and_selected_window_actions_use_only_the_fake_connection() {
    let h = harness();
    h.backend.start();
    assert_eq!(h.connector.attempts(), 1);
    assert_eq!(h.connector.protocols(0), Some(Protocols::TOPLEVELS));
    assert!(!h.backend.ready());
    assert_eq!(h.backend.pending_sources(), ["changed", "startup"]);
    let (link, _) = fake_helper();
    h.connector.ready(&link);
    assert!(h.backend.ready() && h.backend.can_activate());
    assert_eq!(h.backend.protocol_version(), 3);
    assert_eq!(h.backend.pending_sources(), ["changed"], "startup timeout cancelled");
    let ident = h.backend.windows().keys().next().cloned().unwrap();
    assert_eq!(ident, "1:1");
    assert_eq!(h.backend.outputs()["DP-1"].x, -1920);
    assert_eq!(h.backend.window(&ident).unwrap().outputs, ["DP-1"]);
    idle(&h.scheduler);
    assert_eq!(h.changes.get(), 1, "change callbacks are coalesced");
    assert!(h.backend.activate(&ident));
    link.emit();
    assert!(h.backend.window(&ident).unwrap().activated);
    assert!(h.backend.minimize(&ident));
    link.emit();
    assert!(h.backend.window(&ident).unwrap().minimized);
    assert!(h.backend.unminimize(&ident));
    link.emit();
    let window = h.backend.window(&ident).unwrap();
    assert!(!window.minimized);
    assert!(!window.activated, "restoring alone must not request focus");
    assert!(h.backend.close_window(&ident));
    link.emit();
    assert!(h.backend.windows().is_empty());
    assert!(!h.backend.close_window(&ident));
    assert_eq!(*h.errors.borrow(), ["That window is no longer available."]);
}

#[test]
fn connection_loss_clears_state_invalidates_ids_and_reconnects_with_backoff() {
    let h = harness();
    h.backend.start();
    let (link, _) = fake_helper();
    h.connector.ready(&link);
    let ident = h.backend.windows().keys().next().cloned().unwrap();
    link.disconnect("Broken pipe");
    assert!(!h.backend.ready());
    assert!(h.backend.windows().is_empty() && h.backend.outputs().is_empty());
    assert!(link.closed.get());
    assert!(!h.backend.activate(&ident));
    assert!(h.backend.pending_sources().contains(&"reconnect"));
    assert_eq!(h.errors.borrow()[0], "The compositor's window connection ended.");
    assert_eq!(h.backend.retry_delay(), 2000);
    assert!(fire_delay(&h.scheduler, 1000));
    assert_eq!(h.connector.attempts(), 2);
    // The second session fails at once (helper exit): backoff keeps doubling.
    h.connector.complete(Startup::Failed { lines: Vec::new(), message: Some("Cannot connect to the Wayland compositor.".into()) });
    assert_eq!(h.backend.error(), "Cannot connect to the Wayland compositor.");
    assert_eq!(h.backend.retry_delay(), 4000);
    assert!(fire_delay(&h.scheduler, 2000));
    let (second, _) = fake_helper();
    h.connector.ready(&second);
    assert!(h.backend.ready());
    assert_eq!(h.backend.retry_delay(), 1000, "a ready session resets the backoff");
    let new_ident = h.backend.windows().keys().next().cloned().unwrap();
    assert_ne!(new_ident, ident);
    assert!(!h.backend.activate(&ident), "old-session ids stay invalid");
    assert!(h.backend.activate(&new_ident));
    // Events from the lost link never reach the new session.
    link.model.borrow_mut().toplevel_closed(1);
    link.emit();
    assert_eq!(h.backend.windows().len(), 1);
}

#[test]
fn backoff_is_capped_at_fifteen_seconds() {
    let h = harness();
    h.backend.start();
    let mut delays = Vec::new();
    for _ in 0..6 {
        h.connector.complete(Startup::Failed { lines: Vec::new(), message: None });
        delays.push(h.scheduler.pending.borrow().iter().find(|p| p.1 > 0).map(|p| p.1).unwrap());
        let delay = *delays.last().unwrap();
        assert!(fire_delay(&h.scheduler, delay));
    }
    assert_eq!(delays, [1000, 2000, 4000, 8000, 15000, 15000]);
    assert_eq!(h.backend.error(), "The compositor's window connection ended.");
}

#[test]
fn startup_timeout_aborts_the_attempt() {
    let h = harness();
    h.backend.start();
    assert!(fire_delay(&h.scheduler, 8000));
    assert_eq!(h.connector.aborted.get(), 1);
    assert_eq!(h.backend.error(), "The compositor did not provide window information.");
    assert!(h.backend.pending_sources().contains(&"reconnect"));
    // A late completion of the aborted attempt is ignored.
    assert!(!h.connector.complete(Startup::NotStarted));
}

#[test]
fn not_started_and_unsupported_compositor() {
    let h = harness();
    h.backend.start();
    h.connector.complete(Startup::NotStarted);
    assert_eq!(h.backend.error(), "Waydock's window helper could not start. Build or reinstall it.");
    assert!(fire_delay(&h.scheduler, 1000));
    let link = FakeLink::toplevel();
    link.toplevels.set(false);
    h.connector.ready(&link);
    assert!(!h.backend.ready());
    assert!(link.closed.get());
    // The helper printed the error, then exited (Python reports both).
    assert_eq!(h.errors.borrow()[1..], ["This compositor does not support foreign-toplevel window management.",
                                         "This compositor does not support foreign-toplevel window management."]);
    assert!(h.backend.pending_sources().contains(&"reconnect"), "windows keep retrying like Python");
}

#[test]
fn fatal_protocol_error_ends_the_session() {
    let h = harness();
    h.backend.start();
    let (link, _) = fake_helper();
    h.connector.ready(&link);
    link.model.borrow_mut().error("The compositor stopped window management.", true);
    link.emit();
    assert!(!h.backend.ready());
    assert_eq!(h.backend.error(), "The compositor stopped window management.");
    assert!(h.backend.windows().is_empty());
}

#[test]
fn output_rename_and_unplug_through_the_model() {
    let h = harness();
    h.backend.start();
    let (link, id) = fake_helper();
    h.connector.ready(&link);
    {
        let mut m = link.model.borrow_mut();
        m.output_name(4, "DP-9");
        m.output_done(4);
    }
    link.emit();
    assert_eq!(h.backend.outputs().keys().collect::<Vec<_>>(), ["DP-9"]);
    assert_eq!(h.backend.window(&format!("1:{id}")).unwrap().outputs, ["DP-9"]);
    link.model.borrow_mut().output_removed(4);
    link.emit();
    assert!(h.backend.outputs().is_empty());
    assert!(h.backend.window(&format!("1:{id}")).unwrap().outputs.is_empty());
}

#[test]
fn close_suppresses_late_callbacks_and_reconnect() {
    let h = harness();
    h.backend.start();
    let (link, _) = fake_helper();
    h.connector.ready(&link);
    h.backend.close();
    let errors = h.errors.borrow().clone();
    idle(&h.scheduler);
    assert!(!h.backend.ready());
    assert!(h.backend.windows().is_empty());
    assert!(h.backend.pending_sources().is_empty());
    assert!(h.scheduler.pending.borrow().is_empty());
    assert!(link.closed.get());
    link.disconnect("gone");
    assert_eq!(*h.errors.borrow(), errors);
    assert_eq!(h.changes.get(), 0);
    h.backend.start();
    assert_eq!(h.connector.attempts(), 1, "a closed backend never restarts");
}

#[test]
fn invalid_helper_line_fails_cleanly_without_any_window_request() {
    let h = harness();
    h.backend.start();
    h.connector.complete(Startup::Failed { lines: vec!["not JSON".into()], message: None });
    assert!(!h.errors.borrow().is_empty());
    assert!(!h.backend.ready());
    assert!(h.backend.windows().is_empty());
    assert!(h.backend.error().contains("invalid"));
}

#[test]
fn request_errors_are_reported() {
    let h = harness();
    h.backend.start();
    let (link, _) = fake_helper();
    link.respond(|_, _| Err(ActionError::NoSeat));
    h.connector.ready(&link);
    assert!(!h.backend.minimize("1:1"));
    assert_eq!(h.backend.error(), "No input seat is available to focus windows.");
}
