//! Ports of test_notification_model.py.

use super::*;
use glib::prelude::*;
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    dir: PathBuf,
    now: Rc<Cell<f64>>,
    model: NotificationModel,
    events: Vec<Event>,
}

fn temp(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("nm-model-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str) -> Fixture {
    let dir = temp(name);
    let now = Rc::new(Cell::new(1000.0));
    let (a, b) = (now.clone(), now.clone());
    let model = NotificationModel::with_clocks(Some(&dir), Box::new(move || a.get()), Box::new(move || b.get()));
    Fixture { dir, now, model, events: Vec::new() }
}

#[derive(Default)]
struct N {
    app_name: Option<&'static str>,
    replaces_id: u32,
    summary: Option<&'static str>,
    body: Option<&'static str>,
    actions: Vec<&'static str>,
    hints: Vec<(&'static str, Hint)>,
    expire_timeout: Option<i32>,
    sender: Option<&'static str>,
}

impl Fixture {
    fn notify(&mut self, n: N) -> u32 {
        let args = NotifyArgs {
            app_name: n.app_name.unwrap_or("Test").into(), replaces_id: n.replaces_id, app_icon: "test-symbolic".into(),
            summary: n.summary.unwrap_or("Hello").into(), body: n.body.unwrap_or("Body").into(),
            actions: n.actions.iter().map(|s| s.to_string()).collect(),
            hints: n.hints.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
            expire_timeout: n.expire_timeout.unwrap_or(-1),
        };
        let ident = self.model.notify(args, n.sender.unwrap_or(":1.7"));
        self.drain();
        ident
    }

    fn drain(&mut self) {
        self.events.extend(self.model.take_events());
    }

    fn advance(&self, seconds: f64) {
        self.now.set(self.now.get() + seconds);
    }

    fn reload(&self) -> NotificationModel {
        NotificationModel::new(Some(&self.dir))
    }
}

fn int(value: i64) -> Hint { Hint::Int(value) }

#[test]
fn replace_keeps_id_and_resets_expiry_and_new_id_never_zero() {
    let mut f = fixture("replace");
    let ident = f.notify(N { expire_timeout: Some(1000), ..Default::default() });
    f.advance(0.8);
    assert_eq!(f.notify(N { replaces_id: ident, summary: Some("Updated"), expire_timeout: Some(1000), ..Default::default() }), ident);
    f.advance(0.3);
    f.model.tick();
    assert!(f.model.items[&ident].active);
    f.advance(0.8);
    f.model.tick();
    f.drain();
    assert!(!f.model.items[&ident].active);
    assert!(f.events.contains(&Event::Closed(ident, 1)));
    let fresh = f.notify(N { replaces_id: ident, ..Default::default() });
    assert_ne!(ident, fresh);
    assert!(fresh > 0);
    // IDs wrap at 0xffffffff, never 0, and skip ones in use.
    let mut f = fixture("wrap");
    assert_eq!((f.notify(N::default()), f.notify(N::default())), (1, 2));
    f.model.next_id = 0xffff_ffff;
    let last = f.notify(N::default());
    assert_eq!(last, 0xffff_ffff);
    assert_eq!(f.notify(N::default()), 3, "1 and 2 are taken");
}

#[test]
fn critical_default_persists_explicit_timeout_respected_and_hover_pauses() {
    let mut f = fixture("critical");
    let critical = f.notify(N { hints: vec![("urgency", int(2))], ..Default::default() });
    let persistent = f.notify(N { expire_timeout: Some(0), ..Default::default() });
    let timed = f.notify(N { hints: vec![("urgency", int(2))], expire_timeout: Some(2000), ..Default::default() });
    let hovered = f.notify(N { expire_timeout: Some(2000), ..Default::default() });
    f.advance(1.0);
    f.model.set_hovered(hovered, true);
    f.advance(60.0);
    f.model.tick();
    assert!(f.model.items[&critical].active);
    assert!(f.model.items[&persistent].active);
    assert!(!f.model.items[&timed].active);
    assert!(f.model.items[&hovered].active);
    f.model.set_hovered(hovered, false);
    f.advance(1.1);
    f.model.tick();
    assert!(!f.model.items[&hovered].active);
}

#[test]
fn actions_only_valid_on_live_notifications_resident_preserved() {
    let mut f = fixture("actions");
    let ident = f.notify(N { actions: vec!["default", "Open", "save", "Save"], ..Default::default() });
    assert!(!f.model.activate(ident, "invented"));
    assert!(f.model.activate(ident, "save"));
    f.drain();
    let tail = &f.events[f.events.len() - 3..];
    assert_eq!(tail, [Event::Action(ident, "save".into()), Event::Closed(ident, 2), Event::Changed("close", ident)]);
    assert!(!f.model.activate(ident, "default"));
    let resident = f.notify(N { actions: vec!["default", "Open"], hints: vec![("resident", Hint::Bool(true))], ..Default::default() });
    f.model.activate(resident, "default");
    assert!(f.model.items[&resident].active);
    assert!(f.model.items[&resident].read);
}

#[test]
fn action_list_is_bounded_and_deduplicated() {
    let mut f = fixture("action-bounds");
    let mut actions = vec!["", "Empty", "a", "A", "a", "Again"];
    let keys: Vec<String> = (0..40).map(|i| format!("k{i}")).collect();
    let leaked: &'static [String] = Box::leak(keys.into_boxed_slice());
    for key in leaked {
        actions.push(key.as_str());
        actions.push("Label");
    }
    let ident = f.notify(N { actions, ..Default::default() });
    let pairs = &f.model.items[&ident].actions;
    assert_eq!(pairs[0], ("a".to_string(), "A".to_string()));
    assert!(pairs.len() <= 16);
    assert_eq!(pairs.len(), 14, "16 pairs scanned, one empty and one duplicate dropped");
}

#[test]
fn close_is_idempotent_and_callback_observes_invalidation() {
    let mut f = fixture("close");
    let ident = f.notify(N::default());
    let shared = SharedModel::new(std::mem::replace(&mut f.model, NotificationModel::new(Some(&temp("close-spare")))));
    let observed = Rc::new(RefCell::new(Vec::new()));
    let (sink, reader) = (observed.clone(), shared.clone());
    shared.subscribe(move |event| {
        if let Event::Closed(id, _) = event {
            sink.borrow_mut().push(reader.with(|m| m.items[id].active));
        }
    });
    let closes = Rc::new(Cell::new(0));
    let counter = closes.clone();
    shared.subscribe(move |event| if matches!(event, Event::Closed(_, 3)) { counter.set(counter.get() + 1) });
    assert!(shared.close(ident, 3));
    assert!(!shared.close(ident, 3));
    assert_eq!(*observed.borrow(), [false]);
    assert_eq!(closes.get(), 1);
}

#[test]
fn stack_tag_replaces_per_application_and_desktop_entry() {
    let mut f = fixture("stack");
    let ident = f.notify(N { hints: vec![("x-dunst-stack-tag", Hint::Str("volume".into())), ("value", int(5))], ..Default::default() });
    let updated = f.notify(N { sender: Some(":1.8"), hints: vec![("x-dunst-stack-tag", Hint::Str("volume".into())), ("value", int(200))], ..Default::default() });
    let other = f.notify(N { app_name: Some("Other"), hints: vec![("x-dunst-stack-tag", Hint::Str("volume".into()))], ..Default::default() });
    let entry = f.notify(N { hints: vec![("x-canonical-private-synchronous", Hint::Str("volume".into())),
                                       ("desktop-entry", Hint::Str("org.other".into()))], ..Default::default() });
    assert_eq!(ident, updated);
    assert_ne!(ident, other);
    assert_ne!(ident, entry, "desktop entry scopes the tag");
    assert_eq!(f.model.items[&ident].progress, Some(100));
}

#[test]
fn history_private_persisted_bounded_and_transient_not_journaled() {
    let mut f = fixture("history");
    f.model.set_settings(&json!({"history_limit": 2, "do_not_disturb": true})).unwrap();
    let mut ids = Vec::new();
    for index in 0..3 {
        f.advance(1.0);
        let summary: &'static str = Box::leak(index.to_string().into_boxed_str());
        let ident = f.notify(N { summary: Some(summary), actions: vec!["default", "Open"], ..Default::default() });
        f.model.close(ident, 1);
        ids.push(ident);
    }
    let transient = f.notify(N { summary: Some("Never journal this"), hints: vec![("transient", Hint::Bool(true))], ..Default::default() });
    assert!(!f.model.items.contains_key(&ids[0]));
    assert_eq!(f.model.unread(), 2);
    let path = f.dir.join("state.json");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(!std::fs::read_to_string(&path).unwrap().contains("Never journal this"));
    let mut reloaded = f.reload();
    assert_eq!(reloaded.settings["do_not_disturb"], json!(true));
    let mut keys: Vec<u32> = reloaded.items.keys().copied().collect();
    keys.sort();
    assert_eq!(keys, ids[1..]);
    assert!(reloaded.items.values().all(|i| !i.active && i.actions.is_empty()));
    f.model.close(transient, 2);
    assert!(!f.model.items.contains_key(&transient));
    reloaded.mark_read();
    assert_eq!(reloaded.unread(), 0);
    reloaded.clear_history();
    assert!(reloaded.notifications().is_empty());
}

#[test]
fn journal_format_matches_python() {
    let mut f = fixture("journal");
    let ident = f.notify(N { app_name: Some("notify-send"), summary: Some("Hi"), body: Some(""), ..Default::default() });
    f.model.close(ident, 1);
    let text = std::fs::read_to_string(f.dir.join("state.json")).unwrap();
    assert!(text.starts_with("{\"version\": 1, \"next_id\": 2, \"settings\": {\"do_not_disturb\": false, \"dnd_until\": 0, \
                              \"suppress_fullscreen\": true, \"timeout_ms\": 6000, \"history_limit\": 100, \"muted_apps\": []}, \
                              \"history\": [{\"id\": 1, \"app_name\": \"notify-send\", \"summary\": \"Hi\", \"body\": \"\", \
                              \"icon\": \"test-symbolic\", \"image_path\": \"\", \"image\": null, \"actions\": [], \"urgency\": 1, \
                              \"progress\": null, \"timestamp\": 1000.0, \"active\": false, \"read\": false, \"transient\": false, \
                              \"resident\": false, \"desktop_entry\": \"\", \"category\": \"\", \"stack_tag\": \"\", \"output\": \"\", \
                              \"workspace\": false, \"expire_timeout\": 6000, \"close_reason\": 1}]}"), "{text}");
    // ensure_ascii=False keeps UTF-8 text.
    f.notify(N { summary: Some("Café ☕"), ..Default::default() });
    assert!(std::fs::read_to_string(f.dir.join("state.json")).unwrap().contains("Café ☕"));
}

#[test]
fn raw_images_reject_oversized_short_wrong_format_and_copy_valid_padding() {
    let image = |w: i32, h: i32, stride: i32, alpha: bool, bits: i32, ch: i32, data: Vec<u8>| {
        Hint::Other((w, h, stride, alpha, bits, ch, data).to_variant())
    };
    let data = image_data(&image(2, 2, 8, false, 8, 3, (0..14).collect())).unwrap();
    assert_eq!(tessel_core::base64::decode_strict(&data.data).unwrap(), (0..14).collect::<Vec<u8>>());
    for raw in [image(2, 2, 8, false, 8, 3, b"short".to_vec()), image(2048, 2048, 8192, true, 8, 4, vec![]),
                image(2, 2, 5, false, 8, 3, b"0123456789".to_vec()), image(2, 2, 8, true, 8, 3, vec![0; 16]),
                image(1, 1, 3, false, 16, 3, vec![0; 3]), image(1, 1, 3, false, 8, 3, vec![0; MAX_IMAGE_BYTES + 1])] {
        assert!(image_data(&raw).is_none());
    }
    // Signed elements cannot be bytes.
    let signed = Hint::Other((1i32, 1i32, 3i32, false, 8i32, 3i32, vec![-1i32, 0, 0]).to_variant());
    assert!(image_data(&signed).is_none());
    let mut f = fixture("images");
    let ident = f.notify(N { hints: vec![("image-data", image(1, 1, 3, false, 8, 3, b"rgb".to_vec()))], ..Default::default() });
    assert!(f.model.items[&ident].image.is_some());
    let payload: Value = serde_json::from_str(&std::fs::read_to_string(f.dir.join("state.json")).unwrap()).unwrap();
    assert!(payload["history"][0]["image"].is_null());
    // icon_data is only a fallback when neither image-data nor image-path is given.
    let fallback = f.notify(N { hints: vec![("icon_data", image(1, 1, 3, false, 8, 3, b"rgb".to_vec()))], ..Default::default() });
    assert!(f.model.items[&fallback].image.is_some());
    let path = f.notify(N { hints: vec![("image-path", Hint::Str("/x.png".into())),
                                      ("icon_data", image(1, 1, 3, false, 8, 3, b"rgb".to_vec()))], ..Default::default() });
    assert!(f.model.items[&path].image.is_none());
}

#[test]
fn hints_parse_from_variants() {
    let dict = glib::VariantDict::new(None);
    dict.insert_value("urgency", &2u8.to_variant());
    dict.insert_value("transient", &true.to_variant());
    dict.insert_value("x-tessel-dock-output", &"DP-1".to_variant());
    dict.insert_value("image-data", &(1i32, 1i32, 3i32, false, 8i32, 3i32, vec![1u8, 2, 3]).to_variant());
    let hints = hints_from_variant(&dict.end());
    assert_eq!(hints["urgency"], Hint::Int(2));
    assert_eq!(hints["transient"], Hint::Bool(true));
    assert_eq!(hints["x-tessel-dock-output"], Hint::Str("DP-1".into()));
    assert_eq!(image_data(&hints["image-data"]).unwrap().data, "AQID");
}

#[test]
fn plain_text_and_json_are_never_interpreted_as_markup() {
    let mut f = fixture("markup");
    let body = "<b>Unsafe & plain</b> <img src=\"file:///secret\">";
    let ident = f.notify(N { body: Some(body), ..Default::default() });
    assert_eq!(f.model.items[&ident].body, body);
    let state: Value = serde_json::from_str(&serde_json::to_string(&f.model.state()).unwrap()).unwrap();
    assert_eq!(state["notifications"][0]["body"], body);
}

#[test]
fn active_notifications_bounded_and_presentation_does_not_rewrite_prefs() {
    let mut f = fixture("bounded");
    for _ in 0..MAX_ACTIVE + 3 {
        f.notify(N { expire_timeout: Some(0), hints: vec![("transient", Hint::Bool(true))], ..Default::default() });
    }
    assert_eq!(f.model.items.len(), MAX_ACTIVE);
    assert_eq!(f.events.iter().filter(|e| matches!(e, Event::Closed(_, 4))).count(), 3);
    let path = f.dir.join("state.json");
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    f.events.clear();
    f.model.set_presentation(true, false);
    f.model.set_presentation(true, false);
    f.drain();
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
    assert_eq!(f.events, [Event::Changed("presentation", 0)]);
    assert_eq!(f.model.state()["presentation"]["fullscreen"], json!(true));
}

#[test]
fn invalid_settings_rejected_atomically_corrupt_history_does_not_prevent_start() {
    let mut f = fixture("invalid");
    let original = f.model.settings.clone();
    assert!(f.model.set_settings(&json!({"do_not_disturb": true, "timeout_ms": 0})).is_err());
    assert_eq!(f.model.settings, original);
    assert_eq!(f.model.set_settings(&json!({"bogus": 1})).unwrap_err(), "Unknown notification setting");
    assert_eq!(f.model.set_settings(&json!([1])).unwrap_err(), "Settings must be an object");
    assert_eq!(f.model.set_settings(&json!({"suppress_fullscreen": 1})).unwrap_err(), "suppress_fullscreen must be true or false");
    assert!(f.model.set_settings(&json!({"timeout_ms": 6000.0})).is_err(), "floats are not ints");
    assert!(f.model.set_settings(&json!({"history_limit": 501})).is_err());
    let path = f.dir.join("state.json");
    std::fs::write(&path, "{\"version\": 1, \"history\": \"broken\"}").unwrap();
    assert!(f.reload().items.is_empty());
    std::fs::write(&path, "not json").unwrap();
    assert!(f.reload().items.is_empty());
    std::fs::write(&path, "{\"version\": 2, \"settings\": {\"timeout_ms\": 9000}}").unwrap();
    assert_eq!(f.reload().timeout_ms(), 6000, "other versions are ignored");
    std::fs::write(&path, "{\"version\": 1, \"next_id\": 7, \"history\": [{\"id\": 3, \"timestamp\": 5}, \
                           {\"id\": 4, \"timestamp\": \"x\"}, {\"id\": 5, \"timestamp\": 6}]}").unwrap();
    let partial = f.reload();
    assert_eq!(partial.items.keys().copied().collect::<Vec<_>>(), [3], "a bad entry stops the load like Python's except");
    assert_eq!(partial.next_id, 7);
    assert_eq!(partial.items[&3].timestamp, 5.0);
}

#[test]
fn clear_history_invalidates_all_and_emits_one_change() {
    let mut f = fixture("clear");
    let ids: Vec<u32> = (0..5).map(|_| f.notify(N::default())).collect();
    f.events.clear();
    f.model.clear_history();
    f.drain();
    assert!(f.model.items.is_empty());
    let mut expected: Vec<Event> = ids.iter().map(|id| Event::Closed(*id, 2)).collect();
    expected.push(Event::Changed("history", 0));
    assert_eq!(f.events, expected);
}

#[test]
fn next_deadline_is_idle_without_expiring_notifications() {
    let mut f = fixture("deadline");
    assert!(f.model.next_deadline().is_none());
    f.notify(N { expire_timeout: Some(0), ..Default::default() });
    f.notify(N { hints: vec![("urgency", int(2))], ..Default::default() });
    assert!(f.model.next_deadline().is_none(), "persistent and critical toasts never schedule wakeups");
    let later = f.notify(N { expire_timeout: Some(5000), ..Default::default() });
    let sooner = f.notify(N { expire_timeout: Some(1500), ..Default::default() });
    assert_eq!(f.model.next_deadline(), Some(f.now.get() + 1.5));
    f.model.set_hovered(sooner, true);
    assert_eq!(f.model.next_deadline(), Some(f.now.get() + 5.0));
    f.model.close(later, 2);
    assert!(f.model.next_deadline().is_none());
}

#[test]
fn timed_do_not_disturb_expires_and_plain_toggle_is_indefinite() {
    let mut f = fixture("dnd");
    let now = f.now.get();
    f.model.set_settings(&json!({"do_not_disturb": true, "dnd_until": now + 60.0})).unwrap();
    assert!(f.model.dnd_active());
    assert!(!f.model.expire_dnd());
    f.advance(61.0);
    assert!(!f.model.dnd_active());
    assert!(f.model.expire_dnd());
    assert_eq!((&f.model.settings["do_not_disturb"], &f.model.settings["dnd_until"]), (&json!(false), &json!(0)));
    let now = f.now.get();
    f.model.set_settings(&json!({"do_not_disturb": true, "dnd_until": now - 5.0})).unwrap();
    f.model.set_settings(&json!({"do_not_disturb": true})).unwrap();
    assert_eq!(f.model.settings["dnd_until"], json!(0));
    f.advance(1e6);
    assert!(f.model.dnd_active());
    assert_eq!(f.reload().settings["do_not_disturb"], json!(true));
    for value in [json!(-1), json!("soon"), json!(true), json!(2u64.pow(41)), json!(1e300)] {
        assert!(f.model.set_settings(&json!({"dnd_until": value})).is_err(), "{value}");
    }
}

#[test]
fn dnd_deadline_choices() {
    let now = glib::DateTime::from_local(2026, 10, 27, 23, 30, 0.0).unwrap().to_unix() as f64;
    assert_eq!(dnd_deadline("hour", Some(now)).unwrap(), now + 3600.0);
    assert_eq!(dnd_deadline("off", Some(now)).unwrap(), 0.0);
    let tomorrow = glib::DateTime::from_unix_local(dnd_deadline("tomorrow", Some(now)).unwrap() as i64).unwrap();
    assert_eq!((tomorrow.year(), tomorrow.month(), tomorrow.day_of_month(), tomorrow.hour(), tomorrow.minute()), (2026, 10, 28, 8, 0));
    assert!(dnd_deadline("forever", Some(now)).is_err());
}

#[test]
fn muted_apps_validated_persisted_and_copied() {
    let mut f = fixture("muted");
    let ident = f.notify(N { app_name: Some("Chat"), hints: vec![("desktop-entry", Hint::Str("org.chat.App".into()))], ..Default::default() });
    let item = f.model.items[&ident].clone();
    assert_eq!(app_key_of(&item.desktop_entry, &item.app_name), "org.chat.App");
    assert_eq!(app_key(&json!({"app_name": "Mail"})), "Mail");
    assert_eq!(app_key(&json!({})), "Application");
    assert!(!f.model.item_muted(&item));
    f.model.set_settings(&json!({"muted_apps": ["org.chat.App"]})).unwrap();
    assert!(f.model.item_muted(&item));
    let mut state = f.model.state();
    state["settings"]["muted_apps"].as_array_mut().unwrap().push(json!("Mutated"));
    assert_eq!(f.model.muted_apps(), ["org.chat.App"]);
    assert_eq!(f.reload().muted_apps(), ["org.chat.App"]);
    assert!(NotificationModel::new(Some(&temp("muted-empty"))).muted_apps().is_empty());
    let long = "x".repeat(257);
    for value in [json!("Chat"), json!(["a", "a"]), json!([""]), json!([1]), json!([long]), json!(vec!["ok"; 501])] {
        assert!(f.model.set_settings(&json!({"muted_apps": value})).is_err());
    }
}

#[test]
fn app_groups_and_clear_app() {
    let mut f = fixture("groups");
    let chat: Vec<u32> = (0..2).map(|_| f.notify(N { app_name: Some("Chat"), hints: vec![("desktop-entry", Hint::Str("chat".into()))], ..Default::default() })).collect();
    let mail = f.notify(N { app_name: Some("Mail"), ..Default::default() });
    f.advance(1.0);
    let newest = f.notify(N { app_name: Some("Chat Beta"), hints: vec![("desktop-entry", Hint::Str("chat".into()))], ..Default::default() });
    let groups = app_groups(&f.model.notifications());
    let summary: Vec<(String, usize)> = groups.iter().map(|g| (g.key.clone(), g.items.len())).collect();
    assert_eq!(summary, [("chat".to_string(), 3), ("Mail".to_string(), 1)]);
    assert_eq!(groups[0].name, "Chat Beta");
    f.events.clear();
    assert!(f.model.clear_app("chat"));
    f.drain();
    assert_eq!(f.model.items.keys().copied().collect::<Vec<_>>(), [mail]);
    let mut closed: Vec<u32> = f.events.iter().filter_map(|e| if let Event::Closed(id, _) = e { Some(*id) } else { None }).collect();
    closed.sort();
    let mut expected = chat.clone();
    expected.push(newest);
    expected.sort();
    assert_eq!(closed, expected);
    assert_eq!(f.events.iter().filter(|e| matches!(e, Event::Changed(..))).collect::<Vec<_>>(), [&Event::Changed("history", 0)]);
    assert!(!f.model.clear_app("chat"));
}

#[test]
fn storage_errors_are_reported_not_fatal() {
    let dir = temp("readonly");
    let blocker = dir.join("file");
    std::fs::write(&blocker, "x").unwrap();
    let mut model = NotificationModel::new(Some(&blocker.join("sub")));
    model.notify(NotifyArgs { summary: "x".into(), ..Default::default() }, "");
    assert!(model.storage_error.starts_with("[Errno 20] Not a directory"), "{}", model.storage_error);
    assert_eq!(model.state()["storage_error"], json!(model.storage_error));
}
