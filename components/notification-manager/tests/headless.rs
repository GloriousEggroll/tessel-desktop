//! Widget and daemon checks that need a compositor (ports of
//! test_notification_ui.py and test_notification_daemon.py). They run only
//! inside the private nested headless labwc of tests/run_headless.sh
//! (`NM_HEADLESS=1`, own XDG_RUNTIME_DIR, own dbus-run-session bus); under
//! plain `cargo test` this binary skips itself.
//!
//! `headless ui` runs the view tests, `headless daemon` the in-process daemon.

use glib::Variant;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use notification_manager::backend::WindowBackend;
use notification_manager::daemon::Daemon;
use notification_manager::ui::{self, Client, NotificationCenter, SettingsPage, ToastStack};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn settle_until(mut predicate: impl FnMut() -> bool, seconds: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    predicate()
}

fn settle(seconds: f64) {
    settle_until(|| false, seconds);
}

fn now() -> f64 {
    notification_manager::model::wall_clock()
}

fn notification(ident: u32, extra: Value) -> Value {
    let mut base = json!({"id": ident, "app_name": "Software", "summary": "Updates are ready",
        "body": "Review the available updates whenever you're ready.", "icon": "system-software-update-symbolic",
        "actions": [["default", "View updates"], ["later", "Later"]], "urgency": 1, "progress": null,
        "timestamp": now(), "active": true, "read": false});
    for (key, value) in extra.as_object().unwrap() {
        base[key] = value.clone();
    }
    base
}

#[derive(Default)]
struct FakeInner {
    state: RefCell<Map<String, Value>>,
    ready: std::cell::Cell<bool>,
    error: RefCell<String>,
    actions: RefCell<Vec<Value>>,
}

#[derive(Clone)]
struct FakeClient(Rc<FakeInner>);

impl FakeClient {
    fn new() -> Self {
        let client = Self(Rc::new(FakeInner::default()));
        client.0.ready.set(true);
        client.0.state.replace(json!({"notifications": [notification(1, json!({}))], "settings": {
            "do_not_disturb": false, "suppress_fullscreen": true, "timeout_ms": 6000}, "unread": 1}).as_object().unwrap().clone());
        client
    }
    fn actions(&self) -> Vec<Value> { self.0.actions.borrow().clone() }
    fn set(&self, key: &str, value: Value) { self.0.state.borrow_mut().insert(key.into(), value); }
    fn set_setting(&self, key: &str, value: Value) {
        self.0.state.borrow_mut().get_mut("settings").unwrap().as_object_mut().unwrap().insert(key.into(), value);
    }
}

impl Client for FakeClient {
    fn state(&self) -> Map<String, Value> { self.0.state.borrow().clone() }
    fn ready(&self) -> bool { self.0.ready.get() }
    fn error(&self) -> String { self.0.error.borrow().clone() }
    fn set_settings(&self, patch: Value) {
        self.0.actions.borrow_mut().push(json!(["settings", patch.clone()]));
        for (k, v) in patch.as_object().unwrap() {
            self.set_setting(k, v.clone());
        }
    }
    fn clear_history(&self) { self.0.actions.borrow_mut().push(json!(["clear"])); }
    fn clear_app(&self, key: &str) { self.0.actions.borrow_mut().push(json!(["clear-app", key])); }
    fn mark_read(&self) {
        self.0.actions.borrow_mut().push(json!(["read"]));
        let mut state = self.0.state.borrow_mut();
        if let Some(Value::Array(list)) = state.get_mut("notifications") {
            for entry in list {
                entry["read"] = json!(true);
            }
        }
        state.insert("unread".into(), json!(0));
    }
    fn dismiss(&self, ident: u32) { self.0.actions.borrow_mut().push(json!(["dismiss", ident])); }
    fn activate(&self, ident: u32, key: &str) { self.0.actions.borrow_mut().push(json!(["activate", ident, key])); }
    fn show_settings(&self) { self.0.actions.borrow_mut().push(json!(["show-settings"])); }
}

struct Ui {
    app: gtk::Application,
    client: FakeClient,
    toasts: Vec<ToastStack>,
    centers: Vec<NotificationCenter>,
    pages: Vec<SettingsPage>,
    windows: Vec<gtk::Window>,
}

impl Ui {
    fn new(sequence: usize) -> Self {
        let app = gtk::Application::new(Some(&format!("org.waydock.NotificationTest{sequence}")), gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gio::Cancellable>).unwrap();
        Self { app, client: FakeClient::new(), toasts: Vec::new(), centers: Vec::new(), pages: Vec::new(), windows: Vec::new() }
    }
    fn rc(&self) -> Rc<dyn Client> { Rc::new(self.client.clone()) }
    fn toast_with(&mut self, hover: Option<ui::Hover>) -> ToastStack {
        let monitor = gdk::Display::default().unwrap().monitors().item(0).and_downcast::<gdk::Monitor>();
        let (dismiss, activate) = (self.client.clone(), self.client.clone());
        let view = ToastStack::new(&self.app, monitor.as_ref(), Rc::new(move |id| dismiss.dismiss(id)),
                                   Rc::new(move |id, key| activate.activate(id, key)), hover, false);
        self.toasts.push(view.clone());
        view
    }
    fn toast(&mut self) -> ToastStack { self.toast_with(None) }
    fn center(&mut self) -> NotificationCenter {
        let view = NotificationCenter::new(&self.app, self.rc(), "bottom", 76);
        self.centers.push(view.clone());
        view
    }
    fn page(&mut self) -> SettingsPage {
        let page = SettingsPage::new(self.rc());
        self.pages.push(page.clone());
        page
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        for view in self.toasts.drain(..) { view.destroy() }
        for view in self.centers.drain(..) { view.destroy() }
        for page in self.pages.drain(..) { page.destroy() }
        for window in self.windows.drain(..) { window.destroy() }
        settle(0.07);
        self.app.quit();
    }
}

fn names(image: &gtk::Image) -> Vec<String> {
    image.gicon().and_then(|g| g.downcast::<gio::ThemedIcon>().ok())
        .map(|t| t.names().iter().map(|n| n.to_string()).collect()).unwrap_or_default()
}

#[allow(deprecated)]
fn center_has_a_themed_icon_before_the_title(ui: &mut Ui) {
    let view = ui.center();
    view.present();
    settle(0.2);
    let tile = &view.0.header_tile;
    assert_eq!(tile.parent().unwrap().first_child().as_ref(), Some(tile.upcast_ref::<gtk::Widget>()));
    assert_eq!((view.0.header_icon.width(), view.0.header_icon.height()), (24, 24));
    assert_eq!((tile.width(), tile.height()), (42, 42));
    assert!(tile.has_css_class("settings-tile"));
    let theme = gtk::IconTheme::for_display(&WidgetExt::display(view.window()));
    let gicon = view.0.header_icon.gicon().expect("gicon");
    let paintable = theme.lookup_by_gicon(&gicon, 24, 1, gtk::TextDirection::None, gtk::IconLookupFlags::PRELOAD);
    let name = paintable.icon_name().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    assert!(!name.contains("image-missing"), "{name}");
    assert!(paintable.is_symbolic());
}

fn toast_does_not_steal_focus_and_respects_bar_workarea(ui: &mut Ui) {
    let editor = gtk::ApplicationWindow::builder().application(&ui.app).title("Focused application").build();
    editor.set_default_size(300, 200);
    editor.set_child(Some(&gtk::Label::new(Some("Focused application"))));
    ui.windows.push(editor.clone().upcast());
    let backend = WindowBackend::new(|| {}, None);
    backend.start();
    editor.present();
    let focused = || backend.windows().values().any(|w| w.title == "Focused application" && w.activated);
    assert!(settle_until(focused, 3.0), "editor focused through the private compositor");
    let view = ui.toast();
    view.update(&[notification(1, json!({}))]);
    settle(0.2);
    assert!(view.window().is_mapped());
    assert!(focused(), "a toast never takes focus");
    assert!(!view.window().is_active());
    assert_eq!(view.window().layer(), Layer::Top);
    assert_eq!(view.window().exclusive_zone(), 0);
    assert_eq!(view.window().keyboard_mode(), KeyboardMode::None);
    assert_eq!(view.window().margin(Edge::Right), 12);
    assert_eq!(view.window().namespace().as_deref(), Some("waydock-notifications"));
    backend.close();
}

fn replacement_reuses_card_with_updated_actions_and_safe_labels(ui: &mut Ui) {
    let view = ui.toast();
    view.update(&[notification(1, json!({}))]);
    let card = view.card(1).unwrap();
    assert!(card.0.action_buttons.borrow().contains_key("default"));
    view.update(&[notification(1, json!({"summary": "<b>New & updated</b>", "body": "<script>literal</script>",
                                          "actions": [["install", "Install now"]], "progress": 74}))]);
    assert!(Rc::ptr_eq(&view.card(1).unwrap().0, &card.0));
    assert_eq!(card.0.summary_label.text(), "<b>New & updated</b>");
    assert!(!card.0.summary_label.uses_markup());
    assert!(!card.0.body_label.uses_markup());
    assert_eq!(card.0.body_label.text(), "<script>literal</script>");
    assert!((card.0.progress.fraction() - 0.74).abs() < 1e-9);
    assert_eq!(card.0.progress.text().as_deref(), Some("74%"));
    assert!(!card.0.action_buttons.borrow().contains_key("default"));
    let install = card.0.action_buttons.borrow()["install"].clone();
    install.emit_clicked();
    card.0.close_button.emit_clicked();
    assert_eq!(ui.client.actions(), [json!(["activate", 1, "install"]), json!(["dismiss", 1])]);
}

fn toasts_bound_visible_count_and_hide_without_destroying(ui: &mut Ui) {
    let view = ui.toast();
    let many: Vec<Value> = (1..12).map(|i| notification(i, json!({}))).collect();
    view.update(&many);
    settle(0.15);
    let mut ids = view.card_ids();
    ids.sort();
    assert_eq!(ids, [1, 2, 3]);
    assert!(view.window().height() <= 696);
    let card = view.card(1).unwrap();
    view.hide();
    assert!(!view.window().get_visible());
    view.update(&[notification(1, json!({}))]);
    assert!(Rc::ptr_eq(&card.0, &view.card(1).unwrap().0));
    assert!(view.window().get_visible());
    view.update(&[]);
    assert!(!view.window().get_visible());
    assert!(view.card_ids().is_empty());
}

fn hover_lifecycle_balanced_on_hide_and_replacement(ui: &mut Ui) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let sink = calls.clone();
    let view = ui.toast_with(Some(Rc::new(move |id, hovered| sink.borrow_mut().push((id, hovered)))));
    view.update(&[notification(1, json!({}))]);
    let card = view.card(1).unwrap();
    card.enter();
    card.enter();
    view.update(&[notification(1, json!({"summary": "Updated"}))]);
    view.hide();
    view.hide();
    assert_eq!(*calls.borrow(), [(1, true), (1, false)]);
}

fn center_settings_clear_and_dismissed_history(ui: &mut Ui) {
    let view = ui.center();
    view.present();
    settle(0.2);
    assert!(ui.client.actions().contains(&json!(["read"])));
    assert!(view.window().get_visible());
    view.0.dnd_switch.set_active(true);
    assert!(ui.client.actions().contains(&json!(["settings", {"do_not_disturb": true}])));
    view.refresh();
    assert!(view.0.status_label.get_visible());
    ui.client.0.state.borrow_mut().get_mut("notifications").unwrap()[0]["active"] = json!(false);
    view.refresh();
    let card = view.card(1).unwrap();
    assert!(!card.0.action_box.get_visible());
    assert!(card.0.close_button.get_visible());
    let before = ui.client.actions();
    let default = card.0.action_buttons.borrow()["default"].clone();
    default.emit_clicked();
    assert_eq!(ui.client.actions(), before, "inactive history never invokes actions");
    view.0.clear_button.emit_clicked();
    assert!(ui.client.actions().contains(&json!(["clear"])));
    view.0.settings_button.emit_clicked();
    assert!(!view.window().get_visible());
    assert!(ui.client.actions().contains(&json!(["show-settings"])));
}

fn center_escape_clickaway_and_empty_state(ui: &mut Ui) {
    let view = ui.center();
    ui.client.set("notifications", json!([]));
    view.present();
    settle(0.15);
    assert_eq!(view.0.stack.visible_child_name().as_deref(), Some("empty"));
    assert!(!view.0.clear_button.is_sensitive());
    let controller = view.controller().unwrap();
    assert!(!controller.backdrops().is_empty(), "click-away surfaces are shown");
    // Escape and outside clicks both end in PopupController::dismiss().
    controller.dismiss();
    assert!(!view.window().get_visible());
    view.present();
    settle(0.15);
    assert!(view.window().get_visible());
    controller.dismiss();
    assert!(!view.window().get_visible());
    assert!(controller.backdrops().iter().all(|b| !b.get_visible()));
}

fn embedded_settings_sync_avoids_spurious_writes(ui: &mut Ui) {
    let page = ui.page();
    assert!(ui.client.actions().is_empty());
    assert!(page.0.fullscreen_switch.is_active());
    page.0.timeout_spin.set_value(9.0);
    page.0.fullscreen_switch.set_active(false);
    assert_eq!(ui.client.actions(), [json!(["settings", {"timeout_ms": 9000}]), json!(["settings", {"suppress_fullscreen": false}])]);
    ui.client.set_setting("timeout_ms", json!(12000));
    ui.client.set_setting("do_not_disturb", json!(true));
    let before = ui.client.actions();
    page.refresh();
    assert_eq!(page.0.timeout_spin.value_as_int(), 12);
    assert!(page.0.dnd_switch.is_active());
    assert_eq!(ui.client.actions(), before);
    ui.client.0.ready.set(false);
    ui.client.0.error.replace("Service stopped".into());
    page.refresh();
    assert!(!page.0.dnd_switch.is_sensitive());
    assert!(!page.0.timeout_spin.is_sensitive());
    assert_eq!(page.0.status_label.text(), "Service stopped");
    assert!(page.0.status_label.has_css_class("error"));
}

fn history_groups_by_app_collapse_and_clear(ui: &mut Ui) {
    ui.client.set("notifications", json!([
        notification(1, json!({"app_name": "Chat", "desktop_entry": "org.chat", "summary": "Newest"})),
        notification(2, json!({"app_name": "Mail"})),
        notification(3, json!({"app_name": "Chat", "desktop_entry": "org.chat", "summary": "Older"}))]));
    let view = ui.center();
    view.present();
    settle(0.1);
    assert_eq!(view.group_keys(), ["org.chat", "Mail"]);
    let chat = view.group("org.chat").unwrap();
    assert_eq!(view.0.listing.first_child().as_ref(), Some(chat.0.root.upcast_ref::<gtk::Widget>()));
    assert_eq!(chat.0.name_label.text(), "Chat");
    assert_eq!(chat.0.count_label.text(), "2");
    let parent = |id| view.card(id).unwrap().widget().parent();
    assert_eq!(parent(1).as_ref(), Some(chat.0.cards.upcast_ref::<gtk::Widget>()));
    assert_eq!(parent(3).as_ref(), Some(chat.0.cards.upcast_ref::<gtk::Widget>()));
    assert_eq!(chat.0.cards.first_child().as_ref(), Some(view.card(1).unwrap().widget().upcast_ref::<gtk::Widget>()));
    assert!(chat.0.revealer.reveals_child());
    chat.0.toggle.emit_clicked();
    assert!(!chat.0.revealer.reveals_child());
    view.refresh();
    assert!(!view.group("org.chat").unwrap().0.revealer.reveals_child(), "collapse survives refresh");
    chat.0.clear_button.emit_clicked();
    assert!(ui.client.actions().contains(&json!(["clear-app", "org.chat"])));
    // A replacement that changes application moves its card to that group.
    ui.client.set("notifications", json!([notification(2, json!({"app_name": "Mail"})),
                                          notification(3, json!({"app_name": "Mail", "summary": "Moved"}))]));
    view.refresh();
    assert_eq!(view.group_keys(), ["Mail"]);
    assert_eq!(parent(3).as_ref(), Some(view.group("Mail").unwrap().0.cards.upcast_ref::<gtk::Widget>()));
    assert!(view.card(1).is_none());
}

fn center_and_settings_timed_dnd_options(ui: &mut Ui) {
    let view = ui.center();
    let before = now();
    view.0.dnd_buttons["hour"].emit_clicked();
    let patch = ui.client.actions().last().unwrap()[1].clone();
    assert_eq!(patch["do_not_disturb"], json!(true));
    assert!((patch["dnd_until"].as_f64().unwrap() - (before + 3600.0)).abs() < 5.0);
    view.refresh();
    let until = ui::dnd_until_text(ui.client.state()["settings"].as_object().unwrap());
    assert!(!until.is_empty() && view.0.status_label.text().contains(&until));
    view.0.dnd_buttons["off"].emit_clicked();
    assert_eq!(ui.client.actions().last().unwrap(), &json!(["settings", {"do_not_disturb": true, "dnd_until": 0}]));
    let page = ui.page();
    page.0.dnd_buttons["tomorrow"].emit_clicked();
    assert!(ui.client.actions().last().unwrap()[1]["dnd_until"].as_f64().unwrap() > now());
    page.refresh();
    assert!(page.0.dnd_until_label.get_visible());
}

fn settings_lists_seen_and_muted_apps_with_mute_switches(ui: &mut Ui) {
    ui.client.set("notifications", json!([notification(1, json!({"app_name": "Chat", "desktop_entry": "org.chat"})),
                                          notification(2, json!({"app_name": "Mail"}))]));
    ui.client.set_setting("muted_apps", json!(["Retired app"]));
    let page = ui.page();
    assert!(ui.client.actions().is_empty(), "building the list must not write settings");
    let mut keys: Vec<String> = page.0.app_switches.borrow().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["Mail", "Retired app", "org.chat"]);
    let switch = |key: &str| page.0.app_switches.borrow()[key].clone();
    assert!(switch("Retired app").is_active());
    assert!(!page.0.apps_empty.get_visible());
    switch("org.chat").set_active(true);
    assert_eq!(ui.client.actions().last().unwrap(), &json!(["settings", {"muted_apps": ["Retired app", "org.chat"]}]));
    page.refresh();
    assert!(switch("org.chat").is_active());
    switch("Retired app").set_active(false);
    assert_eq!(ui.client.actions().last().unwrap(), &json!(["settings", {"muted_apps": ["org.chat"]}]));
    ui.client.set("notifications", json!([]));
    ui.client.set_setting("muted_apps", json!([]));
    page.refresh();
    assert!(page.0.app_switches.borrow().is_empty());
    assert!(page.0.apps_empty.get_visible());
}

fn icon_images_stay_compact_and_real_images_use_the_picture(ui: &mut Ui) {
    let view = ui.toast();
    // An icon name in the image hint is an icon, not content.
    view.update(&[notification(1, json!({"icon": "", "app_name": "Nobody", "image_path": "dialog-warning-symbolic"}))]);
    let card = view.card(1).unwrap();
    assert!(!card.0.picture.get_visible());
    assert!(!card.0.avatar.get_visible());
    assert!(names(&card.0.icon).contains(&"dialog-warning-symbolic".to_string()));
    // The same icon as the application never repeats as an avatar.
    view.update(&[notification(1, json!({"image_path": "system-software-update-symbolic"}))]);
    assert!(!card.0.picture.get_visible());
    assert!(!card.0.avatar.get_visible());
    assert!(names(&card.0.icon).contains(&"system-software-update-symbolic".to_string()));
    // A different icon (a contact, say) becomes a small avatar.
    view.update(&[notification(1, json!({"image_path": "avatar-default-symbolic"}))]);
    assert!(!card.0.picture.get_visible());
    assert_eq!(card.0.avatar.get_visible(), card.0.avatar.storage_type() != gtk::ImageType::Empty);
    // Real image data keeps the large picture.
    let data = json!({"width": 2, "height": 2, "rowstride": 8, "channels": 4, "has_alpha": true,
                      "data": notification_manager::compat::base64_encode(&[0xff; 16])});
    view.update(&[notification(1, json!({"image": data}))]);
    assert!(card.0.picture.get_visible());
}

fn header_uses_the_application_icon_before_the_info_fallback(ui: &mut Ui) {
    let view = ui.toast();
    view.update(&[notification(1, json!({"icon": "", "app_name": "Nobody", "image_path": ""}))]);
    let card = view.card(1).unwrap();
    assert_eq!(card.0.icon.icon_name().as_deref(), Some("dialog-information-symbolic"));
    view.update(&[notification(1, json!({"icon": "", "app_name": "Nobody", "image_path": "emblem-system-symbolic"}))]);
    assert!(names(&card.0.icon).contains(&"emblem-system-symbolic".to_string()));
}

fn dnd_choices_wrap_instead_of_truncating(ui: &mut Ui) {
    let view = ui.center();
    for button in view.0.dnd_buttons.values() {
        let label = button.child().and_downcast::<gtk::Label>().unwrap();
        assert!(label.wraps());
        assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::None);
    }
}

fn images_are_bounded_and_never_remote(_ui: &mut Ui) {
    let texture = ui::notification_texture(&json!({"width": 1, "height": 2, "rowstride": 4, "channels": 3, "has_alpha": false,
        "data": notification_manager::compat::base64_encode(b"\xff\0\0\0\0\xff\0")})).unwrap();
    assert_eq!((texture.intrinsic_width(), texture.intrinsic_height()), (1, 2));
    assert!(ui::notification_texture(&json!("dialog-information-symbolic")).is_some());
    assert!(ui::notification_texture(&json!("https://example.invalid/notification.png")).is_none());
    assert!(ui::notification_texture(&json!("file://example.invalid/notification.png")).is_none());
    assert!(ui::notification_texture(&json!({"width": 20000, "height": 20000, "rowstride": 80000, "channels": 4,
                                             "has_alpha": true, "data": ""})).is_none());
    assert!(ui::notification_texture(&json!({"width": 1, "height": 1, "rowstride": 4, "channels": 4,
                                             "has_alpha": true, "data": "broken"})).is_none());
    assert_eq!(ui::local_path(&json!("file://localhost/tmp/a%20b.png")).unwrap(), std::path::PathBuf::from("/tmp/a b.png"));
    assert!(ui::local_path(&json!("file:///tmp/a.png?x=1")).is_none());
    assert!(ui::local_path(&json!("relative.png")).is_none());
}

fn palette_tracks_selected_monitor_and_tears_down(ui: &mut Ui) {
    // HOME is private (run_headless.sh); give monitor 0 a palette before the
    // first window attaches the shared palette watcher.
    let monitor = gdk::Display::default().unwrap().monitors().item(0).and_downcast::<gdk::Monitor>().unwrap();
    let g = monitor.geometry();
    let path = suite_gtk::theme::palette_path();
    assert!(path.starts_with(std::env::var("HOME").unwrap()) && std::env::var("HOME").unwrap() != "/home/tcrider");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, json!({"version": 1, "is_dark": true, "apps_transparency_enabled": false, "apps_transparency": 5,
        "apps_frosted": false, "apps_frost_strength": 25, "apps_rounded_corners": true, "colored_window_buttons": false,
        "monitors": [{"name": "Test monitor", "geometry": [g.x(), g.y(), g.width(), g.height()], "background": [39, 31, 47],
                      "foreground": [243, 239, 250], "accent": [186, 159, 235]}]}).to_string()).unwrap();
    let view = ui.toast();
    view.update(&[notification(1, json!({}))]);
    assert!(settle_until(|| view.0.theme.monitor_name().is_some(), 3.0), "palette applied");
    assert_eq!(view.0.theme.monitor_name().as_deref(), Some("Test monitor"));
    let css = view.0.theme.provider().unwrap().to_str().to_string();
    assert!(css.contains("rgb(39,31,47)"), "palette background in the provider");
    view.destroy();
    assert!(view.0.theme.is_closed());
}

type UiTest = fn(&mut Ui);

fn run_ui() -> Vec<&'static str> {
    let tests: &[(&'static str, UiTest)] = &[
        ("palette_tracks_selected_monitor_and_tears_down", palette_tracks_selected_monitor_and_tears_down),
        ("center_has_a_themed_icon_before_the_title", center_has_a_themed_icon_before_the_title),
        ("toast_does_not_steal_focus_and_respects_bar_workarea", toast_does_not_steal_focus_and_respects_bar_workarea),
        ("replacement_reuses_card_with_updated_actions_and_safe_labels", replacement_reuses_card_with_updated_actions_and_safe_labels),
        ("toasts_bound_visible_count_and_hide_without_destroying", toasts_bound_visible_count_and_hide_without_destroying),
        ("hover_lifecycle_balanced_on_hide_and_replacement", hover_lifecycle_balanced_on_hide_and_replacement),
        ("center_settings_clear_and_dismissed_history", center_settings_clear_and_dismissed_history),
        ("center_escape_clickaway_and_empty_state", center_escape_clickaway_and_empty_state),
        ("embedded_settings_sync_avoids_spurious_writes", embedded_settings_sync_avoids_spurious_writes),
        ("history_groups_by_app_collapse_and_clear", history_groups_by_app_collapse_and_clear),
        ("center_and_settings_timed_dnd_options", center_and_settings_timed_dnd_options),
        ("settings_lists_seen_and_muted_apps_with_mute_switches", settings_lists_seen_and_muted_apps_with_mute_switches),
        ("icon_images_stay_compact_and_real_images_use_the_picture", icon_images_stay_compact_and_real_images_use_the_picture),
        ("header_uses_the_application_icon_before_the_info_fallback", header_uses_the_application_icon_before_the_info_fallback),
        ("dnd_choices_wrap_instead_of_truncating", dnd_choices_wrap_instead_of_truncating),
        ("images_are_bounded_and_never_remote", images_are_bounded_and_never_remote),
    ];
    gtk::init().expect("GTK on the private compositor");
    let mut failed = Vec::new();
    for (index, (name, test)) in tests.iter().enumerate() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut ui = Ui::new(index + 1);
            test(&mut ui);
        }));
        report(name, result.is_ok(), &mut failed);
    }
    failed
}

fn report(name: &'static str, ok: bool, failed: &mut Vec<&'static str>) {
    println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
    if !ok {
        failed.push(name);
    }
}

// --- daemon (test_notification_daemon.py) ---

const TEST_TITLE: &str = "Notifications isolated test window";

struct DaemonCase {
    daemon: Daemon,
    client: gio::DBusConnection,
    window: gtk::ApplicationWindow,
}

impl DaemonCase {
    fn new(daemon: &Daemon) -> Self {
        assert!(daemon.failure().is_empty(), "{}", daemon.failure());
        let ready = || daemon.backend().is_some_and(|b| b.ready()) && daemon.guard().is_some_and(|g| !g.checking());
        assert!(settle_until(ready, 5.0), "window backend ready");
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
        let client = gio::DBusConnection::for_address_sync(&address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None, None::<&gio::Cancellable>).unwrap();
        let window = gtk::ApplicationWindow::builder().application(daemon.app()).title(TEST_TITLE).build();
        window.set_default_size(400, 240);
        window.set_child(Some(&gtk::Label::new(Some("Synthetic application"))));
        window.present();
        let case = Self { daemon: daemon.clone(), client, window };
        assert!(settle_until(|| case.test_window().is_some_and(|w| w.activated) && !daemon.guard().unwrap().checking(), 5.0));
        case
    }

    fn test_window(&self) -> Option<notification_manager::backend::Window> {
        self.daemon.backend()?.windows().values().find(|w| w.title == TEST_TITLE).cloned()
    }

    fn call(&self, method: &str, parameters: Option<Variant>, interface: &str) -> Variant {
        let replies = Rc::new(RefCell::new(None));
        let sink = replies.clone();
        self.client.call(Some("org.freedesktop.Notifications"), "/org/freedesktop/Notifications", interface, method,
                         parameters.as_ref(), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>,
                         move |reply| { sink.replace(Some(reply)); });
        assert!(settle_until(|| replies.borrow().is_some(), 3.0));
        replies.take().unwrap().expect("D-Bus call")
    }

    fn control(&self, method: &str, parameters: Option<Variant>) -> Variant {
        self.call(method, parameters, "org.waydock.Notifications1")
    }

    fn notify(&self, title: &str, timeout: i32, replaces: u32, app: &str, hints: Vec<(&str, Variant)>) -> u32 {
        let hints: std::collections::HashMap<String, Variant> = hints.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        let params = (app, replaces, "dialog-information", title, "Notification body", Vec::<String>::new(), hints, timeout).to_variant();
        self.call("Notify", Some(params), "org.freedesktop.Notifications").child_value(0).get::<u32>().unwrap()
    }

    fn simple(&self, title: &str) -> u32 { self.notify(title, 0, 0, "Isolated test", vec![]) }

    fn shown(&self) -> Vec<u32> {
        self.daemon.toast_stacks().iter().filter(|(_, s)| s.window().get_visible()).flat_map(|(_, s)| s.card_ids()).collect()
    }

    fn visible(&self) -> bool {
        self.daemon.toasts().is_some_and(|t| t.window().get_visible())
    }

    fn model(&self) -> notification_manager::model::SharedModel { self.daemon.model().unwrap() }

    fn active(&self, ident: u32) -> bool { self.model().with(|m| m.items.get(&ident).is_some_and(|i| i.active)) }
}

impl Drop for DaemonCase {
    fn drop(&mut self) {
        self.window.destroy();
        let _ = self.client.close_sync(None::<&gio::Cancellable>);
        settle(0.1);
    }
}

fn probe_config(value: Option<Value>) {
    let path = std::env::var("NM_FAKE_PROBE").unwrap();
    match value {
        Some(value) => std::fs::write(&path, value.to_string()).unwrap(),
        None => { let _ = std::fs::remove_file(&path); }
    }
}

fn probe_count() -> usize {
    let path = std::env::var("NM_FAKE_PROBE").unwrap() + ".log";
    std::fs::read_to_string(path).map(|t| t.lines().count()).unwrap_or(0)
}

fn critical() -> Vec<(&'static str, Variant)> { vec![("urgency", 2u8.to_variant())] }

fn idle_daemon_schedules_no_wakeups(case: &DaemonCase) {
    let d = &case.daemon;
    case.model().clear_history();
    assert!(settle_until(|| d.0.expiry_source.borrow().is_none(), 1.0));
    assert!(d.0.dnd_source.borrow().is_none());
    // Only the signal handlers remain; no periodic tick or fullscreen poll.
    assert_eq!(d.0.sources.borrow().len(), 2);
    let persistent = case.simple("Persistent");
    assert!(d.0.expiry_source.borrow().is_none(), "a persistent toast must not schedule a wakeup");
    let timed = case.notify("Expires", 150, 0, "Isolated test", vec![]);
    assert!(d.0.expiry_source.borrow().is_some());
    assert!(settle_until(|| !case.active(timed), 2.0));
    assert!(d.0.expiry_source.borrow().is_none());
    case.model().close(persistent, 2);
    let probes = probe_count();
    d.windows_changed();
    d.reset_focus_signature();
    d.windows_changed();
    settle(0.2);
    assert_eq!(probe_count(), probes, "cached focus must not probe");
    case.model().set_settings(&json!({"do_not_disturb": true, "dnd_until": now() + 1.0})).unwrap();
    assert!(d.0.dnd_source.borrow().is_some());
    assert!(settle_until(|| case.model().with(|m| m.settings["do_not_disturb"] == json!(false)), 3.0));
    assert!(d.0.dnd_source.borrow().is_none());
}

fn critical_bypasses_dnd_and_fullscreen_but_muted_apps_never_pop_up(case: &DaemonCase) {
    let model = case.model();
    model.clear_history();
    model.set_settings(&json!({"do_not_disturb": true})).unwrap();
    let normal = case.simple("Ordinary during DND");
    let crit = case.notify("Critical during DND", 0, 0, "Isolated test", critical());
    assert!(settle_until(|| case.shown().contains(&crit), 3.0));
    assert!(!case.shown().contains(&normal));
    assert!(case.active(crit));
    assert_eq!(model.with(|m| m.items[&crit].expire_timeout), 0, "critical defaults to persistent");
    model.set_settings(&json!({"do_not_disturb": false, "muted_apps": ["Muted app"]})).unwrap();
    assert!(settle_until(|| case.shown().contains(&crit), 1.0), "turning DND off keeps the critical toast");
    let muted = case.notify("Muted", 0, 0, "Muted app", critical());
    settle(0.3);
    assert!(!case.shown().contains(&muted));
    assert!(model.with(|m| m.items.contains_key(&muted)), "muted notifications stay in history");
    model.close(crit, 2);
    case.window.fullscreen();
    assert!(settle_until(|| case.daemon.guard().unwrap().blocked(), 5.0));
    let game_critical = case.notify("Critical over game", 0, 0, "Isolated test", critical());
    let ordinary = case.simple("Ordinary over game");
    assert!(settle_until(|| case.shown().contains(&game_critical), 3.0));
    assert!(!case.shown().contains(&ordinary));
    let stack = case.daemon.toast_stacks().into_iter().find(|(_, s)| s.card_ids().contains(&game_critical)).unwrap().1;
    assert_eq!(stack.window().layer(), Layer::Overlay);
    assert!(case.test_window().unwrap().activated, "the toast did not take focus from the game");
    case.window.unfullscreen();
    assert!(settle_until(|| { let g = case.daemon.guard().unwrap(); !g.blocked() && !g.checking() }, 5.0));
    model.close(game_critical, 2);
    model.set_settings(&json!({"do_not_disturb": false, "muted_apps": []})).unwrap();
}

fn fullscreen_history_no_backlog_timeout_center_and_headless_cli(case: &DaemonCase) {
    let model = case.model();
    let ident = case.simple("Normal");
    assert!(settle_until(|| case.visible(), 3.0));
    assert!(case.test_window().unwrap().activated, "passive toasts must not steal application focus");
    model.close(ident, 2);
    assert!(settle_until(|| !case.visible(), 2.0));
    case.window.fullscreen();
    assert!(settle_until(|| case.daemon.guard().unwrap().blocked(), 5.0));
    let suppressed = case.simple("Suppressed in fullscreen");
    assert!(!case.visible());
    let state: Value = serde_json::from_str(&case.control("GetState", None).child_value(0).get::<String>().unwrap()).unwrap();
    assert_eq!(state["presentation"]["fullscreen"], json!(true));
    assert!(state["notifications"].as_array().unwrap().iter().any(|e| e["id"] == json!(suppressed)));
    case.window.unfullscreen();
    assert!(settle_until(|| { let g = case.daemon.guard().unwrap(); !g.blocked() && !g.checking() }, 5.0));
    settle(0.1);
    assert!(!case.visible(), "leaving fullscreen must not replay notification backlog");
    let timed = case.notify("Expires", 180, 0, "Isolated test", vec![]);
    assert!(settle_until(|| case.visible(), 2.0));
    assert!(settle_until(|| !case.active(timed), 2.0));
    assert!(settle_until(|| !case.visible(), 2.0));
    case.control("ShowCenterAt", Some(("top", 24u32).to_variant()));
    assert!(settle_until(|| case.daemon.center().is_some_and(|c| c.window().get_visible()), 2.0));
    assert_eq!(model.with(|m| m.unread()), 0);
    case.control("ShowCenterAt", Some(("top", 24u32).to_variant()));
    assert!(case.daemon.center().is_none(), "a second ShowCenterAt closes the center");
    case.control("ShowCenter", None);
    assert!(settle_until(|| case.daemon.center().is_some_and(|c| c.window().get_visible()), 2.0));
    case.daemon.close_center();
    case.control("ShowSettings", None);
    assert!(settle_until(|| case.daemon.settings_window().is_some_and(|s| s.0.window.get_visible()), 2.0));
    assert_eq!(case.daemon.settings_window().unwrap().0.window.default_size(), (650, 410));
    case.daemon.settings_window().unwrap().0.window.close();
    settle(0.1);
    assert!(!case.daemon.settings_window().unwrap().0.window.get_visible(), "close hides the settings window");
    let binary = std::env::var("NM_BIN").unwrap();
    let mut child = std::process::Command::new(binary).arg("--check").stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()).spawn().unwrap();
    assert!(settle_until(|| child.try_wait().ok().flatten().is_some(), 3.0));
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["ready"], json!(true));
    assert_eq!(model.with(|m| m.directory.parent().map(|p| p.to_path_buf())),
               Some(std::path::PathBuf::from(std::env::var("XDG_STATE_HOME").unwrap())));
}

fn borderless_probe_holds_replacement_and_preserves_history(case: &DaemonCase) {
    let model = case.model();
    let guard = case.daemon.guard().unwrap();
    let ident = case.simple("Before game");
    assert!(settle_until(|| case.visible(), 3.0));
    let window = case.test_window().unwrap();
    let output = case.daemon.backend().unwrap().outputs().values().find(|o| window.outputs.contains(&o.name)).cloned().unwrap();
    probe_config(Some(json!({"delay": 0.05, "snapshot": {
        "focused": true, "classes": [window.app_id], "title": window.title, "fullscreen": false, "maximized": false,
        "geometry": [output.x, output.y, output.width, output.height], "border_width": 0, "extents": [0, 0, 0, 0],
        "motif": null, "clients": [[window.app_id]]}})));
    guard.cache().borrow_mut().invalidate();
    let replacement = case.notify("During borderless probe", 0, ident, "Isolated test", vec![]);
    assert_eq!(replacement, ident);
    assert!(guard.checking());
    assert!(!case.visible(), "a replacement must not leak through the fullscreen probe");
    assert!(settle_until(|| guard.blocked() && !guard.checking(), 3.0));
    assert!(!case.visible());
    assert_eq!(model.with(|m| m.items[&ident].summary.clone()), "During borderless probe");
    probe_config(Some(json!({"snapshot": {"focused": false, "clients": [[window.app_id]]}})));
    guard.cache().borrow_mut().invalidate();
    guard.refresh(false);
    assert!(settle_until(|| !guard.blocked() && !guard.checking(), 3.0));
    assert!(!case.visible(), "no backlog after the game");
    probe_config(None);
    model.close(ident, 2);
}

type DaemonTest = fn(&DaemonCase);

fn run_daemon() -> Vec<&'static str> {
    let tests: &'static [(&'static str, DaemonTest)] = &[
        ("idle_daemon_schedules_no_wakeups", idle_daemon_schedules_no_wakeups),
        ("critical_bypasses_dnd_and_fullscreen_but_muted_apps_never_pop_up", critical_bypasses_dnd_and_fullscreen_but_muted_apps_never_pop_up),
        ("fullscreen_history_no_backlog_timeout_center_and_headless_cli", fullscreen_history_no_backlog_timeout_center_and_headless_cli),
        ("borderless_probe_holds_replacement_and_preserves_history", borderless_probe_holds_replacement_and_preserves_history),
    ];
    let daemon = Daemon::new();
    let failed = Rc::new(RefCell::new(Vec::new()));
    let (sink, handle) = (failed.clone(), daemon.clone());
    glib::timeout_add_local_once(Duration::from_millis(100), move || {
        for (name, test) in tests {
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let case = DaemonCase::new(&handle);
                test(&case);
            })).is_ok();
            report(name, ok, &mut sink.borrow_mut());
        }
        handle.stop();
    });
    let code = daemon.run();
    let mut failed = failed.take();
    if code != 0 {
        failed.push("daemon exit status");
    }
    failed
}

fn main() {
    if std::env::var("NM_HEADLESS").as_deref() != Ok("1") {
        println!("headless tests skipped (run tests/run_headless.sh)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(!runtime.is_empty() && runtime != "/run/user/1000", "private runtime dir required");
    suite_core::i18n::initialize();
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ui".into());
    let failed = if mode == "daemon" { run_daemon() } else { run_ui() };
    println!("test result: {}. {} failed {:?}", if failed.is_empty() { "ok" } else { "FAILED" }, failed.len(), failed);
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
