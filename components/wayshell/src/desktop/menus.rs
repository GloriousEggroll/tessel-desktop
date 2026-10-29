//! Desktop context menus (wayshell:889 background, 2973 multi-selection,
//! 3063 item). Each popover carries its own action group shadowing "win".

use super::view::{is_protected_name, DesktopView};
use super::config;
use gio::prelude::*;
use gtk::prelude::*;
use std::rc::Rc;
use suite_core::tr;

fn action(group: &gio::SimpleActionGroup, view: &Rc<DesktopView>, name: &str, f: impl Fn(&Rc<DesktopView>) + 'static) -> gio::SimpleAction {
    let action = gio::SimpleAction::new(name, None);
    let weak = Rc::downgrade(view);
    action.connect_activate(move |_, _| if let Some(v) = weak.upgrade() { f(&v) });
    group.add_action(&action);
    action
}

/// True when `way-displays-gui` is on PATH (`shutil.which`).
pub fn has_display_settings() -> bool {
    suite_core::shell::which("way-displays-gui").is_some()
}

/// The background menu model: four sections (Configure Displays only when
/// `way-displays-gui` exists).
pub fn background_model(configure_displays: bool) -> gio::Menu {
    let model = gio::Menu::new();
    let launch = gio::Menu::new();
    launch.append(Some(&tr!("Open Terminal")), Some("win.open_terminal"));
    launch.append(Some(&tr!("Open File Manager")), Some("win.open_file_manager"));
    launch.append(Some(&tr!("Open Web Browser")), Some("win.open_web_browser"));
    model.append_section(None, &launch);
    let view = gio::Menu::new();
    view.append(Some(&tr!("Hide Icons")), Some("win.toggle_hide_icons"));
    view.append(Some(&tr!("Align to Grid")), Some("win.toggle_align_grid"));
    view.append(Some(&tr!("Show Hidden")), Some("win.toggle_show_hidden"));
    let sort = gio::Menu::new();
    sort.append(Some(&tr!("By Name")), Some("win.sort_by_name"));
    sort.append(Some(&tr!("By Type")), Some("win.sort_by_type"));
    view.append_item(&gio::MenuItem::new_submenu(Some(&tr!("Sort by")), &sort));
    model.append_section(None, &view);
    let edit = gio::Menu::new();
    edit.append(Some(&tr!("Cut")), Some("win.cut"));
    edit.append(Some(&tr!("Copy")), Some("win.copy"));
    edit.append(Some(&tr!("Paste")), Some("win.paste"));
    edit.append(Some(&tr!("Move to Trash")), Some("win.delete_item"));
    model.append_section(None, &edit);
    let system = gio::Menu::new();
    system.append(Some(&tr!("System Monitor")), Some("win.open_system_monitor"));
    system.append(Some(&tr!("Settings")), Some("win.open_global_settings"));
    system.append(Some(&tr!("Change Wallpaper")), Some("win.open_wallpaper_manager"));
    if configure_displays {
        system.append(Some(&tr!("Configure Displays")), Some("win.configure_displays"));
    }
    model.append_section(None, &system);
    model
}

/// The background menu's action group (exposed for tests).
pub fn background_actions(view: &Rc<DesktopView>) -> gio::SimpleActionGroup {
    let any_protected = view.selected_names().iter().any(|key| is_protected_name(&config::desktop_dir().join(key).to_string_lossy()));
    let group = gio::SimpleActionGroup::new();
    // Shared view actions: every open menu reflects changes from other monitors.
    group.add_action(&view.act_hide_icons);
    group.add_action(&view.act_show_hidden);
    let align = gio::SimpleAction::new_stateful("toggle_align_grid", None, &view.state.borrow().align_to_grid.to_variant());
    let weak = Rc::downgrade(view);
    align.connect_activate(move |action, _| {
        let state = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
        action.set_state(&state.to_variant());
        if let Some(v) = weak.upgrade() {
            v.state.borrow_mut().align_to_grid = state;
        }
    });
    group.add_action(&align);
    let cut = action(&group, view, "cut", |v| v.multi_cut());
    let copy = action(&group, view, "copy", |v| v.multi_copy());
    action(&group, view, "paste", |v| v.multi_paste());
    let delete = action(&group, view, "delete_item", |v| v.delete_selected_item());
    if any_protected {
        for a in [&cut, &copy, &delete] {
            a.set_enabled(false);
        }
    }
    action(&group, view, "open_wallpaper_manager", |v| super::settings::show_wallpaper_manager(v));
    action(&group, view, "open_system_monitor", |v| v.launch_suite_app("org.waydock.SystemMonitor.desktop"));
    action(&group, view, "open_global_settings", |v| v.launch_suite_app("org.waydock.GlobalSettings.desktop"));
    action(&group, view, "open_terminal", |v| v.open_default_terminal());
    action(&group, view, "open_file_manager", |v| v.launch_file_manager());
    action(&group, view, "open_web_browser", |v| v.launch_web_browser());
    action(&group, view, "configure_displays", |v| v.launch_configure_displays());
    action(&group, view, "sort_by_name", |v| if let Some(d) = v.desktops() { d.sort_all_by_name() });
    action(&group, view, "sort_by_type", |v| if let Some(d) = v.desktops() { d.sort_all_by_type() });
    group
}

pub fn background_menu(view: &Rc<DesktopView>) {
    let popover = gtk::PopoverMenu::from_model(Some(&background_model(has_display_settings())));
    popover.set_has_arrow(false);
    popover.set_autohide(true);
    popover.insert_action_group("win", Some(&background_actions(view)));
    view.popup_menu(&popover);
}

/// Multi-selection menu: Cut / Copy / Move to Trash (disabled with a protected item).
pub fn multi_actions(view: &Rc<DesktopView>) -> (gio::Menu, gio::SimpleActionGroup) {
    let menu = gio::Menu::new();
    menu.append_item(&gio::MenuItem::new(Some(&tr!("Cut")), Some("win.cut_items")));
    menu.append_item(&gio::MenuItem::new(Some(&tr!("Copy")), Some("win.copy_items")));
    menu.append_item(&gio::MenuItem::new(Some(&tr!("Move to Trash")), Some("win.delete_items")));
    let group = gio::SimpleActionGroup::new();
    let allowed = !view.selected_contains_protected();
    for a in [action(&group, view, "cut_items", |v| v.multi_cut()), action(&group, view, "copy_items", |v| v.multi_copy()), action(&group, view, "delete_items", |v| v.delete_selected_item())] {
        a.set_enabled(allowed);
    }
    (menu, group)
}

pub fn multi_menu(view: &Rc<DesktopView>) {
    let (menu, group) = multi_actions(view);
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_autohide(true);
    popover.insert_action_group("win", Some(&group));
    view.popup_menu(&popover);
}

/// Item menu: Trash = Open / Empty Trash; otherwise Open / Rename / Cut /
/// Copy / Move to Trash (all but Open disabled for protected items; Rename
/// also unless exactly one item is selected).
pub fn item_actions(view: &Rc<DesktopView>, fullpath: &str) -> (gio::Menu, gio::SimpleActionGroup) {
    let basename = std::path::Path::new(fullpath).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let menu = gio::Menu::new();
    menu.append(Some(&tr!("Open")), Some("win.open_item"));
    if basename == "trash-can.desktop" {
        menu.append(Some(&tr!("Empty Trash")), Some("win.empty_trash"));
    } else {
        menu.append(Some(&tr!("Rename")), Some("ctx.rename"));
        menu.append(Some(&tr!("Cut")), Some("ctx.cut"));
        menu.append(Some(&tr!("Copy")), Some("ctx.copy"));
        menu.append(Some(&tr!("Move to Trash")), Some("ctx.delete"));
    }
    let group = gio::SimpleActionGroup::new();
    let rename = action(&group, view, "rename", |v| v.action_rename_item(None));
    let cut = action(&group, view, "cut", |v| v.multi_cut());
    let copy = action(&group, view, "copy", |v| v.multi_copy());
    let delete = action(&group, view, "delete", |v| v.delete_selected_item());
    let allowed = !is_protected_name(fullpath);
    for a in [&rename, &cut, &copy, &delete] {
        a.set_enabled(allowed);
    }
    if view.selected_names().len() != 1 {
        rename.set_enabled(false);
    }
    (menu, group)
}

pub fn item_menu(view: &Rc<DesktopView>, fullpath: &str) {
    let (menu, group) = item_actions(view, fullpath);
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_autohide(true);
    popover.insert_action_group("ctx", Some(&group));
    view.popup_menu(&popover);
}

/// Labels of a menu model, flattened (sections and submenus), for tests.
pub fn labels(model: &impl IsA<gio::MenuModel>) -> Vec<Vec<String>> {
    let model = model.as_ref();
    let mut sections = Vec::new();
    let flat = |m: &gio::MenuModel| -> Vec<String> {
        (0..m.n_items()).filter_map(|i| m.item_attribute_value(i, "label", None).and_then(|v| v.get::<String>())).collect()
    };
    let has_sections = (0..model.n_items()).any(|i| model.item_link(i, "section").is_some());
    if !has_sections {
        return vec![flat(model)];
    }
    for i in 0..model.n_items() {
        if let Some(section) = model.item_link(i, "section") {
            sections.push(flat(&section));
        }
    }
    sections
}
