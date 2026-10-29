//! Port of test_dock_model.py and test_utility_groups.py.

use std::cell::RefCell;
use std::collections::HashMap;

use super::app_catalog::AppEntry;
use super::model::*;

struct Catalog {
    apps: RefCell<Vec<AppEntry>>,
    aliases: HashMap<&'static str, &'static str>,
}

impl Catalog {
    fn new() -> Self {
        Self {
            apps: RefCell::new(vec![
                AppEntry::new("browser.desktop", "Browser"),
                AppEntry::new("files.desktop", "Files"),
                AppEntry::new("terminal.desktop", "Terminal"),
            ]),
            aliases: HashMap::from([("BrowserClass", "browser.desktop")]),
        }
    }
    fn add(&self, id: &str, name: &str) {
        self.apps.borrow_mut().push(AppEntry::new(id, name));
    }
    fn remove(&self, id: &str) {
        self.apps.borrow_mut().retain(|a| a.id != id);
    }
}

impl CatalogLookup for Catalog {
    fn get(&self, id: &str) -> Option<AppEntry> {
        self.apps.borrow().iter().find(|a| a.id == id).cloned()
    }
    fn resolve(&self, key: &str) -> Option<AppEntry> {
        let key = self.aliases.get(key).copied().unwrap_or(key);
        self.get(key).or_else(|| self.get(&format!("{key}.desktop")))
    }
}

fn w(id: &str, app_id: &str) -> WindowData {
    WindowData::new(id, app_id, "")
}

fn titled(id: &str, app_id: &str, title: &str) -> WindowData {
    WindowData::new(id, app_id, title)
}

fn pins(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn ids(items: &[DockItem<WindowData>]) -> Vec<String> {
    items.iter().map(|i| i.id.clone()).collect()
}

fn window_ids(item: &DockItem<WindowData>) -> Vec<String> {
    item.windows.iter().map(|w| w.id.clone()).collect()
}

#[test]
fn pins_join_alias_windows_and_indicators_follow_snapshot() {
    let catalog = Catalog::new();
    let mut model = DockModel::new();
    let mut windows = vec![
        WindowData { activated: true, ..w("a", "browser") },
        WindowData { minimized: true, ..w("b", "BrowserClass") },
        w("c", "files"),
    ];
    let items = model.update(&pins(&["browser.desktop", "browser.desktop", "terminal.desktop"]), &windows, &catalog).to_vec();
    assert_eq!(ids(&items), pins(&["browser.desktop", "terminal.desktop", "files.desktop"]));
    let (browser, terminal, files) = (&items[0], &items[1], &items[2]);
    assert_eq!(browser.count(), 2);
    assert!(browser.focused() && browser.running());
    assert!(!terminal.running());
    assert_eq!(terminal.count(), 0);
    assert!(!files.pinned);
    windows[0].activated = false;
    windows[2].activated = true;
    let items = model.update(&pins(&["browser.desktop", "terminal.desktop"]), &windows, &catalog);
    assert!(!items[0].focused());
    assert!(items[2].focused());
}

#[test]
fn unpin_close_and_repin_do_not_duplicate_or_lose_running_windows() {
    let catalog = Catalog::new();
    let mut model = DockModel::new();
    let mut windows = vec![w("a", "browser"), w("b", "files")];
    model.update(&pins(&["browser.desktop"]), &windows, &catalog);
    let items = model.update(&[], &windows, &catalog).to_vec();
    assert_eq!(ids(&items), pins(&["browser.desktop", "files.desktop"]));
    assert!(!items[0].pinned);
    let items = model.update(&pins(&["files.desktop"]), &windows, &catalog).to_vec();
    assert_eq!(ids(&items), pins(&["files.desktop", "browser.desktop"]));
    windows.remove(0);
    let items = model.update(&pins(&["files.desktop"]), &windows, &catalog).to_vec();
    assert_eq!(ids(&items), pins(&["files.desktop"]));
    let items = model.update(&pins(&["files.desktop"]), &[], &catalog).to_vec();
    assert!(!items[0].running());
    assert!(model.update(&[], &[], &catalog).is_empty());
}

#[test]
fn window_and_group_order_survive_snapshot_reordering() {
    let catalog = Catalog::new();
    let mut model = DockModel::new();
    let initial = vec![w("a", "browser"), w("b", "files"), w("c", "browser")];
    model.update(&[], &initial, &catalog);
    let mut reversed: Vec<WindowData> = initial.iter().rev().cloned().collect();
    reversed.push(w("d", "terminal"));
    let items = model.update(&[], &reversed, &catalog).to_vec();
    assert_eq!(ids(&items), pins(&["browser.desktop", "files.desktop", "terminal.desktop"]));
    assert_eq!(window_ids(&items[0]), pins(&["a", "c"]));
    let items = model.update(&pins(&["terminal.desktop", "files.desktop"]), &initial, &catalog).to_vec();
    assert_eq!(ids(&items), pins(&["terminal.desktop", "files.desktop", "browser.desktop"]));
}

#[test]
fn unknown_exact_app_ids_group_but_empty_ids_stay_separate() {
    let catalog = Catalog::new();
    let mut model = DockModel::new();
    let windows = vec![w("a", "unknown"), w("b", "unknown"), w("c", "Unknown"), titled("d", "", "Document"), titled("e", "", "Document")];
    let items = model.update(&[], &windows, &catalog).to_vec();
    assert_eq!(ids(&items), pins(&["app:unknown", "app:Unknown", "window:d", "window:e"]));
    assert_eq!(items[0].count(), 2);
    assert_eq!(items[0].desktop_id(), "");
    assert_eq!(items.last().unwrap().name, "Document");
    let items = model.update(&[], &[w("x", "")], &catalog);
    assert_eq!(items[0].name, "Application");
}

#[test]
fn missing_pin_is_hidden_but_surviving_windows_remain_unpinned() {
    let catalog = Catalog::new();
    let mut model = DockModel::new();
    let pins = pins(&["uninstalled.desktop"]);
    assert!(model.update(&pins, &[], &catalog).is_empty());
    let items = model.update(&pins, &[w("a", "uninstalled")], &catalog).to_vec();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "app:uninstalled");
    assert_eq!(items[0].count(), 1);
    assert!(!items[0].pinned);
    assert!(items[0].app.is_none());
    assert_eq!(items[0].desktop_id(), "");
    catalog.add("uninstalled.desktop", "Reinstalled");
    let items = model.update(&pins, &[], &catalog);
    assert_eq!(items[0].id, "uninstalled.desktop");
    assert!(items[0].pinned);
    assert_eq!(items[0].name, "Reinstalled");
}

#[test]
fn unidentified_dialog_inherits_parent_identity_without_title_guessing() {
    let catalog = Catalog::new();
    let mut model = DockModel::new();
    let windows = vec![WindowData { parent_id: "a".into(), activated: true, ..w("dialog", "") }, titled("a", "browser", "Files")];
    let items = model.update(&[], &windows, &catalog).to_vec();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "browser.desktop");
    assert_eq!(items[0].count(), 2);
    assert!(items[0].focused());
    let cyclic = vec![WindowData { parent_id: "y".into(), ..w("x", "") }, WindowData { parent_id: "x".into(), ..w("y", "") }];
    let items = model.update(&[], &cyclic, &catalog);
    assert_eq!(items.iter().map(|i| i.count()).sum::<usize>(), 2);
}

#[test]
fn minimized_window_remains_running_but_does_not_claim_focus() {
    let catalog = Catalog::new();
    let mut model = DockModel::new();
    let items = model.update(&[], &[WindowData { activated: true, minimized: true, ..w("a", "browser") }], &catalog);
    assert!(items[0].running());
    assert!(!items[0].focused());
}

// ---- HoverState ----

#[test]
fn sensor_to_dock_gap_has_grace_period_then_demotes() {
    let mut state = HoverState::new(true, 0.25).unwrap();
    assert_eq!(state.layer(), "bottom");
    assert!(state.enter("sensor", 1.0));
    assert_eq!(state.layer(), "top");
    state.leave("sensor", 1.1);
    assert!((state.deadline.unwrap() - 1.35).abs() < 1e-9);
    assert!(state.tick(1.2));
    state.enter("dock", 1.21);
    assert_eq!(state.deadline, None);
    assert!(state.tick(2.0));
    state.leave("dock", 2.0);
    assert!(state.tick(2.249));
    assert!(!state.tick(2.25));
    assert_eq!(state.layer(), "bottom");
}

#[test]
fn menu_and_preview_hold_visible_independently_of_pointer() {
    let mut state = HoverState::default();
    state.enter("dock", 0.0);
    state.enter("menu", 0.1);
    state.enter("preview", 0.2);
    state.leave("dock", 0.3);
    assert_eq!(state.deadline, None);
    state.leave("menu", 0.4);
    assert!(state.tick(10.0));
    state.leave("preview", 10.0);
    assert!(!state.tick(11.0));
}

#[test]
fn stale_timer_cannot_hide_new_hover() {
    let mut state = HoverState::default();
    state.enter("dock", 0.0);
    state.leave("dock", 1.0);
    state.enter("dock", 1.1);
    assert!(state.tick(1.25));
    state.leave("unknown", 2.0);
    assert_eq!(state.deadline, None);
    assert!(state.promoted());
}

#[test]
fn outputs_have_independent_hover_state() {
    let (mut first, mut second) = (HoverState::default(), HoverState::default());
    first.enter("sensor", 0.0);
    assert_eq!(first.layer(), "top");
    assert_eq!(second.layer(), "bottom");
    first.leave("sensor", 1.0);
    second.enter("menu", 1.0);
    assert!(!first.tick(2.0));
    assert!(second.tick(2.0));
}

#[test]
fn configuration_rebuild_and_zero_delay() {
    let mut state = HoverState::new(false, 0.25).unwrap();
    assert_eq!(state.layer(), "top");
    assert!(!state.active());
    state.enter("preview", 0.0);
    assert!(state.active());
    state.leave("preview", 1.0);
    assert!(state.active());
    state.tick(1.25);
    assert!(!state.active());
    assert!(state.promoted());
    state.set_hide_on_fullscreen(true);
    assert_eq!(state.layer(), "bottom");
    state.enter("menu", 0.0);
    assert!(state.set_hide_on_fullscreen(false));
    assert!(state.clear());
    assert!(!state.set_hide_on_fullscreen(true));
    let mut state = HoverState::new(true, 0.0).unwrap();
    state.enter("dock", 0.0);
    assert!(!state.leave("dock", 1.0));
    assert_eq!(HoverState::new(true, -1.0).unwrap_err(), "The leave delay cannot be negative");
}

// ---- utility groups ----

fn utility_catalog() -> Catalog {
    let catalog = Catalog::new();
    catalog.add("org.waydock.GlobalSettings.desktop", "Global Settings");
    catalog
}

fn partition(
    model: &mut DockModel<WindowData>,
    catalog: &Catalog,
    windows: &[WindowData],
    pins: &[String],
    options: UtilityOptions,
) -> (Vec<DockItem<WindowData>>, Utilities<WindowData>) {
    let items = model.update(pins, windows, catalog).to_vec();
    utility_partition(&items, &UtilityOptions { file_managers: vec!["files.desktop".into()], ..options })
}

#[test]
fn windows_move_to_utilities_once_with_focus_and_counts() {
    let catalog = utility_catalog();
    let mut model = DockModel::new();
    let windows = vec![
        WindowData { activated: true, ..titled("folder", "files", "Downloads") },
        titled("trash", "files", "Trash — Files"),
        titled("settings", "org.waydock.GlobalSettings", "Audio — Global Settings"),
        titled("browser", "browser", "A webpage"),
    ];
    let (normal, utilities) = partition(&mut model, &catalog, &windows, &[], UtilityOptions::default());
    assert_eq!(ids(&normal), pins(&["browser.desktop"]));
    assert_eq!(utility(&utilities, "home").id, "utility:home");
    assert_eq!(window_ids(utility(&utilities, "home")), pins(&["folder"]));
    assert_eq!(window_ids(utility(&utilities, "trash")), pins(&["trash"]));
    assert_eq!(window_ids(utility(&utilities, "settings")), pins(&["settings"]));
    assert!(utility(&utilities, "home").focused());
    assert!(!utility(&utilities, "trash").focused());
    let total: usize = normal.iter().map(|i| i.count()).sum::<usize>() + utilities.iter().map(|(_, i)| i.count()).sum::<usize>();
    assert_eq!(total, windows.len());
    assert_eq!(utility(&utilities, "home").name, "Home folder");
}

#[test]
fn hidden_utilities_restore_original_pins_and_taskbar_windows() {
    let catalog = utility_catalog();
    let mut model = DockModel::new();
    let windows = vec![titled("folder", "files", "Downloads"), titled("trash", "files", "Trash")];
    let saved = pins(&["files.desktop", "terminal.desktop"]);
    let (normal, utilities) = partition(&mut model, &catalog, &windows, &saved, UtilityOptions { enabled: false, ..Default::default() });
    assert_eq!(ids(&normal), saved);
    assert!(normal[0].pinned);
    assert_eq!(normal[0].count(), 2);
    assert!(!utilities.iter().any(|(_, i)| i.running()));
    let (visible, _) = partition(&mut model, &catalog, &windows, &saved, UtilityOptions::default());
    assert_eq!(ids(&visible), pins(&["terminal.desktop"]));
    assert_eq!(saved, pins(&["files.desktop", "terminal.desktop"]));
}

#[test]
fn utility_placeholder_replaces_redundant_pin_without_windows() {
    let catalog = utility_catalog();
    let mut model = DockModel::new();
    let (normal, utilities) =
        partition(&mut model, &catalog, &[], &pins(&["files.desktop", "org.waydock.GlobalSettings.desktop"]), UtilityOptions::default());
    assert!(normal.is_empty());
    assert!(!utilities.iter().any(|(_, i)| i.running()));
    assert_eq!(utility(&utilities, "home").app.as_ref().map(|a| a.id.as_str()), Some("files.desktop"));
}

#[test]
fn title_match_is_conservative_and_foreign_apps_are_not_claimed() {
    let catalog = utility_catalog();
    let mut model = DockModel::new();
    let windows = vec![
        titled("document", "browser", "Trash"),
        titled("notes", "files", "trash notes"),
        titled("subfolder", "files", "my-trash — Files"),
        titled("trash", "files", "Trash — Files"),
        titled("localized", "files", "Papierkorb — Files"),
    ];
    let options = UtilityOptions { trash_titles: pins(&["Trash", "Papierkorb"]), ..Default::default() };
    let (normal, utilities) = partition(&mut model, &catalog, &windows, &[], options);
    assert_eq!(window_ids(&normal[0]), pins(&["document"]));
    assert_eq!(window_ids(utility(&utilities, "home")), pins(&["notes", "subfolder"]));
    assert_eq!(window_ids(utility(&utilities, "trash")), pins(&["trash", "localized"]));
}

#[test]
fn trash_child_dialog_tracks_its_parent() {
    let catalog = utility_catalog();
    let mut model = DockModel::new();
    let windows = vec![WindowData { parent_id: "trash".into(), activated: true, ..titled("confirm", "", "Confirm") }, titled("trash", "files", "Trash")];
    let (normal, utilities) = partition(&mut model, &catalog, &windows, &[], UtilityOptions::default());
    assert!(normal.is_empty());
    assert_eq!(utility(&utilities, "trash").count(), 2);
    assert!(utility(&utilities, "trash").focused());
    assert_eq!(utility(&utilities, "home").count(), 0);
}

#[test]
fn navigation_between_folder_and_trash_moves_same_window() {
    let catalog = utility_catalog();
    let mut model = DockModel::new();
    let window = WindowData { activated: true, ..titled("folder", "files", "Home") };
    let (_, before) = partition(&mut model, &catalog, &[window.clone()], &[], UtilityOptions::default());
    let (_, after) = partition(&mut model, &catalog, &[WindowData { title: "Trash".into(), ..window }], &[], UtilityOptions::default());
    assert_eq!(utility(&before, "home").count(), 1);
    assert_eq!(utility(&after, "home").count(), 0);
    assert_eq!(utility(&after, "trash").count(), 1);
    assert!(utility(&after, "trash").focused());
    let (_, empty) = partition(&mut model, &catalog, &[], &[], UtilityOptions::default());
    assert!(!empty.iter().any(|(_, i)| i.running()));
}

#[test]
fn distinct_trash_handler_keeps_other_windows_in_taskbar() {
    let catalog = utility_catalog();
    catalog.add("other-files.desktop", "Other Files");
    let mut model = DockModel::new();
    let windows = vec![titled("other", "other-files", "Downloads"), titled("trash", "other-files", "Trash")];
    let options = UtilityOptions { trash_managers: pins(&["other-files.desktop"]), ..Default::default() };
    let (normal, utilities) = partition(&mut model, &catalog, &windows, &[], options);
    assert_eq!(window_ids(&normal[0]), pins(&["other"]));
    assert_eq!(window_ids(utility(&utilities, "trash")), pins(&["trash"]));
    assert_eq!(utility(&utilities, "home").count(), 0);
}

#[test]
fn unknown_settings_launcher_still_groups_exact_app_identity() {
    let catalog = utility_catalog();
    catalog.remove("org.waydock.GlobalSettings.desktop");
    let mut model = DockModel::new();
    let (normal, utilities) = partition(&mut model, &catalog, &[titled("settings", "org.waydock.GlobalSettings", "Settings")], &[], UtilityOptions::default());
    assert!(normal.is_empty());
    assert_eq!(utility(&utilities, "settings").count(), 1);
    assert!(utility(&utilities, "settings").app.is_none());
}

#[test]
fn uri_metadata_and_missing_title() {
    let catalog = utility_catalog();
    let mut model = DockModel::new();
    let windows = vec![WindowData { uri: "trash:///".into(), ..titled("trash", "files", "") }, titled("folder", "files", "")];
    let (normal, utilities) = partition(&mut model, &catalog, &windows, &[], UtilityOptions::default());
    assert!(normal.is_empty());
    assert_eq!(utility(&utilities, "trash").count(), 1);
    assert_eq!(utility(&utilities, "home").count(), 1);
}
