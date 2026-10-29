//! Port of test_workspace_backend.py (EventTests + ProcessTests over a
//! scripted connection).

use super::*;
use crate::wl::link::fake::{FakeConnector, FakeLink};
use serde_json::json;
use std::cell::Cell;
use suite_core::scheduler::FakeScheduler;
use suite_wayland::model::{workspace_bits, Model};
use suite_wayland::ActionError;

struct Harness {
    backend: WorkspaceBackend,
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
    let backend = WorkspaceBackend::with_parts(move || c.set(c.get() + 1), move |m| e.borrow_mut().push(m.to_owned()),
                                               connector.clone(), scheduler.clone());
    Harness { backend, connector, scheduler, errors, changes }
}

fn fire_delay(scheduler: &FakeScheduler, delay: u32) -> bool {
    let id = scheduler.pending.borrow().iter().find(|p| p.1 == delay).map(|p| p.0);
    id.is_some_and(|id| scheduler.fire(id))
}

fn workspace(ident: &str, name: &str, changes: Value) -> Value {
    let mut value = json!({"id": ident, "name": name, "active": false, "can_activate": true,
                           "coordinates": [], "group_id": "1", "outputs": ["DP-1"]});
    for (key, item) in changes.as_object().unwrap() {
        value[key] = item.clone();
    }
    value
}

fn snapshot(values: Vec<Value>) -> Value {
    json!({"event": "state", "workspaces": values, "groups": [{"id": "1", "outputs": ["DP-1"]}]})
}

fn names(values: &[Workspace]) -> Vec<&str> { values.iter().map(|w| w.name.as_str()).collect() }

fn event_backend() -> Harness {
    let h = harness();
    h.backend.set_session(4);
    h
}

#[test]
fn active_state_updates_atomically_and_removed_workspaces_disappear() {
    let h = event_backend();
    let b = &h.backend;
    b.consume_event(&snapshot(vec![workspace("3", "1", json!({"active": true})), workspace("4", "2", json!({}))])).unwrap();
    assert!(b.workspace("4:3").unwrap().active);
    b.consume_event(&snapshot(vec![workspace("3", "1", json!({})), workspace("4", "2", json!({"active": true}))])).unwrap();
    assert!(!b.workspace("4:3").unwrap().active);
    assert!(b.workspace("4:4").unwrap().active);
    b.consume_event(&snapshot(vec![workspace("4", "2", json!({"active": true}))])).unwrap();
    assert_eq!(b.workspaces().keys().collect::<Vec<_>>(), ["4:4"]);
}

#[test]
fn coordinate_order_overrides_names_and_fallback_is_natural() {
    let h = event_backend();
    let b = &h.backend;
    b.consume_event(&snapshot(vec![workspace("3", "10", json!({})), workspace("4", "2", json!({})),
                                   workspace("5", "1", json!({}))])).unwrap();
    assert_eq!(names(&b.sorted_workspaces(None)), ["1", "2", "10"]);
    b.consume_event(&snapshot(vec![workspace("3", "10", json!({"coordinates": [0]})),
        workspace("4", "2", json!({"coordinates": [2]})), workspace("5", "1", json!({"coordinates": [1]}))])).unwrap();
    assert_eq!(names(&b.sorted_workspaces(None)), ["10", "1", "2"]);
}

#[test]
fn monitor_filter_honors_groups_shared_outputs_and_hidden_states() {
    let h = event_backend();
    let b = &h.backend;
    b.consume_event(&snapshot(vec![workspace("3", "1", json!({"outputs": ["DP-1", "DP-2"]})),
        workspace("4", "2", json!({"outputs": ["DP-2"]})), workspace("5", "3", json!({"hidden": true})),
        workspace("6", "4", json!({"group_id": "", "outputs": []}))])).unwrap();
    assert_eq!(names(&b.sorted_workspaces(Some("DP-1"))), ["1", "4"]);
    assert_eq!(names(&b.sorted_workspaces(Some("DP-2"))), ["1", "2", "4"]);
    assert_eq!(names(&b.sorted_workspaces(Some(""))), ["1", "2", "4"]);
}

#[test]
fn malformed_snapshots_do_not_publish_partial_changes() {
    let h = event_backend();
    let b = &h.backend;
    b.consume_event(&snapshot(vec![workspace("3", "1", json!({}))])).unwrap();
    let previous = b.workspaces().clone();
    for invalid in [workspace("3\nactivate 9", "1", json!({})), workspace("3", "1", json!({"active": 1})),
                    workspace("3", "1", json!({"coordinates": [true]})), workspace("3", "1", json!({"coordinates": [-1]})),
                    workspace("3", "1", json!({"coordinates": [4294967296u64]})),
                    workspace("3", "1", json!({"coordinates": [1.0]})),
                    workspace("3", "1", json!({"group_id": "999"})), workspace("3", "1", json!({"outputs": "DP-1"})),
                    workspace("3", "1", json!({"outputs": [""]})), workspace("3", "1", json!({"name": null})),
                    workspace("3", &"x".repeat(16385), json!({})), json!("3")] {
        assert!(b.consume_event(&snapshot(vec![workspace("4", "2", json!({})), invalid.clone()])).is_err(), "{invalid}");
        assert_eq!(*b.workspaces(), previous);
    }
    assert!(b.consume_event(&json!({"event": "state", "workspaces": [], "groups": "x"})).is_err());
    assert!(b.consume_event(&json!({"event": "state", "groups": []})).is_err());
    assert!(b.consume_event(&json!({"event": "state", "workspaces": [],
        "groups": [{"id": "1"}, {"id": "1"}]})).is_err());
    assert!(b.consume_event(&json!({"event": "bogus"})).is_err());
    assert!(b.consume_event(&snapshot(vec![workspace("3", &"é".repeat(16384), json!({}))])).is_ok(),
            "the name limit counts characters like Python len()");
}

#[test]
fn duplicate_ids_rejected() {
    let h = event_backend();
    assert!(h.backend.consume_event(&snapshot(vec![workspace("3", "1", json!({})), workspace("3", "1", json!({}))])).is_err());
}

#[test]
fn natural_names_compare_numbers_by_value_and_text_casefolded() {
    let mut values = vec!["Desk 10", "desk 9", "Desk 010a", "٣", "2", "beta", "Alpha", "", "100000000000000000000000"];
    values.sort_by_key(|v| natural_name(v));
    assert_eq!(values, ["", "2", "100000000000000000000000", "Alpha", "beta", "desk 9", "Desk 10", "Desk 010a", "٣"]);
    assert_eq!(natural_name("a1b"), [NamePart::Text("a".into()), NamePart::Number("1".into()), NamePart::Text("b".into())]);
}

#[test]
fn natural_name_matches_python() {
    // Same order as Python's sorted(key=natural_name) on these names.
    let names = ["10", "9", "ß", "SS", "a01", "a1", "a", "١٢", "x٣y", "Z", "z2", "z10"];
    let python = std::process::Command::new("python3").arg("-I").arg("-c").arg(format!(
        "import sys,json,re; sys.path.insert(0,'{}/btmanager/waydock')\n\
         from workspace_backend import natural_name\n\
         print(json.dumps(sorted({:?}, key=natural_name)))",
        std::env::var("HOME").unwrap(), names)).output();
    let Ok(output) = python else { return };
    if !output.status.success() {
        return; // PyGObject missing: the import needs gi
    }
    let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    let mut ours: Vec<&str> = names.to_vec();
    ours.sort_by_key(|v| natural_name(v));
    assert_eq!(ours, expected);
}

#[test]
fn disabled_hidden_and_stale_actions_are_never_sent() {
    let h = harness();
    h.backend.start();
    assert!(!h.backend.activate("1:3"));
    let link = FakeLink::workspaces();
    h.connector.ready(&link);
    assert!(h.backend.ready());
    h.backend.consume_event(&snapshot(vec![workspace("3", "1", json!({"can_activate": false})),
                                           workspace("4", "2", json!({"hidden": true}))])).unwrap();
    for ident in ["1:3", "1:4", "0:3", "1:9", "", "1:"] {
        assert!(!h.backend.activate(ident), "{ident}");
    }
    assert!(link.requests.borrow().is_empty());
    assert_eq!(h.backend.error(), "That workspace is no longer available.");
    assert!(!h.backend.activate("1:3"));
    assert_eq!(h.backend.error(), "This workspace cannot be activated.");
}

/// The Python FAKE_HELPER: one group on DP-1, workspaces "1" (active) and "2".
fn fake_helper() -> (Rc<FakeLink>, [u64; 2]) {
    let link = FakeLink::workspaces();
    let ids = {
        let mut m = link.model.borrow_mut();
        m.output_added(7);
        m.output_name(7, "DP-1");
        m.output_done(7);
        let group = m.group_new();
        m.group_output_enter(group, 7);
        let mut ids = [0; 2];
        for (index, slot) in ids.iter_mut().enumerate() {
            let id = m.workspace_new();
            m.workspace_name(id, &(index + 1).to_string());
            m.workspace_coordinates(id, &(index as u32).to_ne_bytes());
            m.workspace_capabilities(id, workspace_bits::CAN_ACTIVATE);
            m.workspace_state(id, if index == 0 { workspace_bits::ACTIVE } else { 0 });
            m.group_workspace_enter(group, id);
            *slot = id;
        }
        m.workspaces_done();
        ids
    };
    link.respond(move |m: &mut Model, request| {
        let Request::ActivateWorkspace(target) = request else { return Err(ActionError::NoWorkspace) };
        if !ids.contains(&target) {
            return Err(ActionError::NoWorkspace);
        }
        for id in ids {
            m.workspace_state(id, if id == target { workspace_bits::ACTIVE } else { 0 });
        }
        m.workspaces_done();
        Ok(())
    });
    (link, ids)
}

#[test]
fn discovery_and_serialized_rapid_activation() {
    let h = harness();
    h.backend.start();
    assert_eq!(h.connector.protocols(0), Some(Protocols::WORKSPACES));
    let (link, _) = fake_helper();
    h.connector.ready(&link);
    assert!(h.backend.ready(), "{:?}", h.errors.borrow());
    assert_eq!(h.backend.supported(), Some(true));
    assert_eq!(h.backend.protocol_version(), 1);
    let sorted = h.backend.sorted_workspaces(None);
    let [first, second] = [&sorted[0], &sorted[1]];
    assert!(first.active);
    assert_eq!(first.outputs, ["DP-1"]);
    assert_eq!(first.group_id, "1:1");
    assert_eq!(h.backend.groups().len(), 1);
    for _ in 0..16 {
        assert!(h.backend.activate(&second.id));
        assert!(h.backend.activate(&first.id));
    }
    assert!(h.backend.activate(&second.id));
    link.emit();
    assert!(h.backend.workspace(&second.id).unwrap().active);
    assert!(!h.backend.workspace(&first.id).unwrap().active);
    assert_eq!(link.requests.borrow().len(), 33);
    assert!(h.errors.borrow().is_empty());
}

#[test]
fn loss_invalidates_ids_and_recovers() {
    let h = harness();
    h.backend.start();
    let (link, _) = fake_helper();
    h.connector.ready(&link);
    let old = h.backend.sorted_workspaces(None)[0].id.clone();
    link.disconnect("gone");
    assert_eq!(*h.errors.borrow(), ["The compositor's workspace connection ended."]);
    assert!(!h.backend.activate(&old));
    assert!(fire_delay(&h.scheduler, 1000));
    let (second, _) = fake_helper();
    h.connector.ready(&second);
    assert!(h.backend.ready());
    assert!(h.backend.workspace(&old).is_none());
    assert_eq!(h.backend.workspaces().len(), 2);
}

#[test]
fn unsupported_compositor_does_not_restart_or_error_spam() {
    let h = harness();
    h.backend.start();
    let link = FakeLink::workspaces();
    link.workspaces.set(false);
    h.connector.ready(&link);
    assert_eq!(h.backend.supported(), Some(false));
    assert!(!h.backend.ready());
    assert!(h.backend.workspaces().is_empty());
    assert!(!h.backend.pending_sources().contains(&"startup"));
    assert!(!h.backend.pending_sources().contains(&"reconnect"));
    assert!(h.errors.borrow().is_empty());
    assert_eq!(h.backend.error(), "This compositor does not expose desktop workspaces.");
    assert!(link.closed.get());
    link.disconnect("gone");
    assert!(h.scheduler.pending.borrow().iter().all(|p| p.1 == 0), "no reconnect after unsupported");
    // A JSON `unsupported` without a message gets the backend's own text.
    let other = event_backend();
    other.backend.consume_event(&json!({"event": "unsupported"})).unwrap();
    assert_eq!(other.backend.error(), "Desktop workspace navigation is unavailable.");
    assert!(other.errors.borrow().is_empty());
}

#[test]
fn malformed_output_fails_and_close_suppresses_reconnect() {
    let h = harness();
    h.backend.start();
    h.connector.complete(Startup::Failed { lines: vec!["not JSON".into()], message: None });
    assert!(!h.errors.borrow().is_empty());
    assert!(h.backend.error().contains("invalid"));
    h.backend.close();
    let errors = h.errors.borrow().clone();
    assert!(h.backend.pending_sources().is_empty());
    assert!(h.scheduler.pending.borrow().is_empty());
    assert_eq!(*h.errors.borrow(), errors);
    assert_eq!(h.changes.get(), 0);
}

#[test]
fn startup_timeout_and_not_started_messages() {
    let h = harness();
    h.backend.start();
    assert!(fire_delay(&h.scheduler, 8000));
    assert_eq!(h.backend.error(), "The compositor did not provide workspace information.");
    assert!(fire_delay(&h.scheduler, 1000));
    h.connector.complete(Startup::NotStarted);
    assert_eq!(h.backend.error(), "Waydock's workspace helper could not start. Build or reinstall it.");
}

#[test]
fn fatal_coordinates_error_ends_the_session() {
    let h = harness();
    h.backend.start();
    let (link, ids) = fake_helper();
    h.connector.ready(&link);
    link.model.borrow_mut().workspace_coordinates(ids[0], &[1, 2, 3]);
    link.emit();
    assert!(!h.backend.ready());
    assert_eq!(h.backend.error(), "Invalid workspace coordinates.");
    assert!(h.backend.pending_sources().contains(&"reconnect"));
}
