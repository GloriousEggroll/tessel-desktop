//! DockSurface items: `render`, `item_button`/`update_item_button`, the
//! magnifier, window click rules, focused title, context menus
//! (`popup_menu`, item/background/trash menus), trash count.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

use super::{DockSurface, Item, ItemButton};
use crate::core::model::{utility_id, utility_partition, UtilityOptions, WindowRecord, UTILITY_IDS};
use crate::ui::host::{accessible_description, accessible_name, click, pack, remove};
use crate::ui::magnifier::Magnifier;
use crate::wl::Window;
use suite_core::tr;

/// One context-menu row: title and action (`None` = insensitive).
pub type Entry = (String, Option<Rc<dyn Fn()>>);

fn reorder(container: &gtk::Box, widget: &impl IsA<gtk::Widget>, index: usize) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    let others: Vec<gtk::Widget> =
        crate::ui::app_menu::popup::children(container).into_iter().filter(|c| c != widget).collect();
    container.reorder_child_after(widget, if index > 0 { others.get(index - 1) } else { None });
}

fn role_of(id: &str) -> Option<&'static str> { UTILITY_IDS.iter().find(|(_, ident)| *ident == id).map(|(role, _)| *role) }

fn same_app(a: &Option<crate::core::app_catalog::AppEntry>, b: &Option<crate::core::app_catalog::AppEntry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b && match (&a.icon, &b.icon) {
            (None, None) => true,
            (Some(x), Some(y)) => x.equal(Some(y)),
            _ => false,
        },
        _ => false,
    }
}

impl DockSurface {
    /// `names_focused_window()`: title display, or a top bar paired with a
    /// separate app dock (it has no application icons, like a macOS menu bar).
    pub fn names_focused_window(&self) -> bool {
        self.config.application_display == "title" || (self.bar_mode && !self.is_app_dock && self.config.uses_separate_app_dock())
    }

    /// `utility_handlers()`: desktop defaults, cached until the catalog refreshes.
    fn utility_handlers(&self) -> (Vec<String>, Vec<String>) {
        let Some(app) = self.app() else { return (Vec::new(), Vec::new()) };
        let generation = app.catalog_generation();
        if let Some((known, handlers)) = self.utility_handlers.borrow().as_ref() {
            if *known == generation {
                return handlers.clone();
            }
        }
        let catalog = app.catalog();
        let aliases = |info: Option<gio::AppInfo>| -> Vec<String> {
            let Some(desktop_id) = info.and_then(|info| info.id()).map(|id| id.to_string()).filter(|id| !id.is_empty()) else {
                return Vec::new();
            };
            let catalog = catalog.borrow();
            let entry = catalog.get(&desktop_id).or_else(|| catalog.resolve(&desktop_id, None)).cloned();
            let mut values = vec![desktop_id];
            if let Some(entry) = entry {
                values.push(entry.id.clone());
                values.push(entry.startup_wm_class.clone());
            }
            values.into_iter().filter(|v| !v.is_empty()).collect()
        };
        let files = aliases(gio::AppInfo::default_for_type("inode/directory", false));
        let mut trash = aliases(gio::AppInfo::default_for_uri_scheme("trash"));
        if trash.is_empty() {
            trash = files.clone();
        }
        let handlers = (files, trash);
        self.utility_handlers.replace(Some((generation, handlers.clone())));
        handlers
    }

    pub fn find_item(&self, item_id: &str) -> Option<Item> {
        self.utility_items.borrow().get(item_id).cloned().or_else(|| self.render_items.borrow().get(item_id).cloned())
    }

    pub(super) fn item_anchor(&self, item_id: &str) -> Option<gtk::Button> {
        match role_of(item_id) {
            Some(role) => self.utility_buttons.borrow().get(role).map(|b| b.button.clone()),
            None => self.buttons.borrow().get(item_id).map(|b| b.button.clone()),
        }
    }

    /// `render(items)`.
    pub fn render(self: &Rc<Self>, items: Rc<Vec<Item>>) {
        // Maximization can change without changing app titles or grouping.
        self.refresh_window_state();
        self.wake(1.0);
        self.source_items.replace(items.clone());
        let enabled = self.config.is_group_visible("files");
        let (file_managers, trash_managers) = self.utility_handlers();
        let options = UtilityOptions {
            file_managers, trash_managers, enabled,
            trash_titles: ["Trash", "Wastebin", "Rubbish Bin", "Recycle Bin"].iter().map(|s| s.to_string()).chain([tr!("Trash")]).collect(),
            names: Some(vec![("home".into(), tr!("Home folder")), ("settings".into(), tr!("Global Settings")), ("trash".into(), tr!("Trash"))]),
        };
        let (items, utilities) = utility_partition(&items, &options);
        self.utility_enabled.set(Some(enabled));
        self.utility_items.replace(utilities.iter().map(|(_, item)| (item.id.clone(), item.clone())).collect());
        self.render_items.replace(items.iter().map(|item| (item.id.clone(), item.clone())).collect());
        let title_mode = self.names_focused_window();
        let app = self.app();
        let source = self.source_items();
        let mut title_windows: Vec<Window> = Vec::new();
        for window in source.iter().flat_map(|item| item.windows.iter()) {
            match title_windows.iter_mut().find(|w| w.id == window.id) {
                Some(existing) => *existing = window.clone(),
                None => title_windows.push(window.clone()),
            }
        }
        let (title_window, restore_id) = match &app {
            Some(app) => {
                let state = app.focused_title();
                let mut state = state.borrow_mut();
                let resolved = if title_mode { state.resolve(&title_windows).cloned() } else { None };
                if !title_mode && !self.is_app_dock {
                    state.clear();
                }
                (resolved, state.restore_id.clone())
            }
            None => (None, None),
        };
        let signature = format!("{:?}", (
            title_mode, self.custom_group_zones(), enabled, &restore_id,
            items.iter().chain(utilities.iter().map(|(_, i)| i)).map(|i| (
                &i.id, i.pinned, &i.name, i.focused(),
                i.windows.iter().map(|w| (&w.id, &w.title, w.activated, w.minimized)).collect::<Vec<_>>(),
            )).collect::<Vec<_>>(),
        ));
        if self.signature.borrow().as_deref() == Some(signature.as_str()) {
            return;
        }
        self.signature.replace(Some(signature));
        if self.widgets.get().is_none() {
            self.build_groups();
        }
        self.arrange_application_groups();
        let preview = self.preview();
        for (role, item) in &utilities {
            let button = self.utility_buttons.borrow().get(role).cloned();
            if let Some(button) = button {
                self.update_item_button(&button, item);
            }
            if preview.preview_id().as_deref() == Some(item.id.as_str()) {
                preview.update(item);
            }
        }
        let ids: Vec<&String> = items.iter().map(|item| &item.id).collect();
        let stale: Vec<String> = self.buttons.borrow().keys().filter(|id| !ids.contains(id)).cloned().collect();
        for ident in stale {
            if preview.preview_id().as_deref() == Some(ident.as_str()) {
                self.close_preview();
            }
            if let Some(button) = self.buttons.borrow_mut().shift_remove(&ident) {
                remove(&button.button);
            }
        }
        let groups = self.widgets.get().expect("groups");
        groups.focused_title.set_visible(title_mode);
        if title_mode {
            let current = title_window.as_ref().and_then(|tw| {
                source.iter().find_map(|item| item.windows.iter().find(|w| w.id == tw.id).map(|w| (item.clone(), w.clone())))
            });
            self.title_window_id.replace(current.as_ref().map(|(_, w)| w.id.clone()));
            let title = match &current {
                Some((item, window)) => if window.title.is_empty() { item.name.clone() } else { window.title.clone() },
                None => tr!("Desktop"),
            };
            // Keep multi-line application captions within the dock's bounds.
            groups.focused_title.set_text(&title.split_whitespace().collect::<Vec<_>>().join(" "));
            groups.focused_title.set_tooltip_text(Some(&title));
            let accessible = if current.is_some() { tr!("Focused window: {title}", title = title.as_str()) } else { tr!("Desktop") };
            accessible_name(&groups.focused_title, &accessible);
            groups.focused_title_button.set_sensitive(current.is_some());
            let action = match &current {
                Some((_, window)) if window.minimized || restore_id.as_deref() == Some(window.id.as_str()) => {
                    tr!("Restore {title}", title = title.as_str())
                }
                Some(_) => tr!("Minimize {title}", title = title.as_str()),
                None => tr!("Desktop"),
            };
            groups.focused_title_button.set_tooltip_text(Some(&action));
            accessible_name(&groups.focused_title_button, &action);
        }
        let mut positions = [0usize, 0usize];
        for item in &items {
            let container = if item.pinned { &groups.pinned_box } else { &groups.running_box };
            let existing = self.buttons.borrow().get(&item.id).cloned();
            let button = match existing {
                Some(button) => {
                    if let Some(parent) = button.button.parent().filter(|p| p != container.upcast_ref::<gtk::Widget>()) {
                        if let Some(parent) = parent.downcast_ref::<gtk::Box>() {
                            parent.remove(&button.button);
                        }
                    }
                    button
                }
                None => {
                    let button = self.new_item_button(item, None);
                    self.buttons.borrow_mut().insert(item.id.clone(), button.clone());
                    button
                }
            };
            if button.button.parent().is_none() {
                pack(container, &button.button, false, false, 0);
            }
            let slot = if item.pinned { 0 } else { 1 };
            reorder(container, &button.button, positions[slot]);
            positions[slot] += 1;
            self.update_item_button(&button, item);
            button.button.set_visible(true);
            if preview.preview_id().as_deref() == Some(item.id.as_str()) {
                preview.update(item);
            }
        }
        {
            let mut available = self.group_available.borrow_mut();
            available.insert("pinned".into(), positions[0] > 0);
            available.insert("running".into(), positions[1] > 0);
        }
        self.refresh_groups();
        self.queue_panel_layout();
        self.set_trash_count(app.as_ref().and_then(|app| app.trash_count()));
    }

    /// `item_button(item, icon)`.
    pub(super) fn new_item_button(self: &Rc<Self>, item: &Item, icon: Option<&str>) -> Rc<ItemButton> {
        let button = gtk::Button::new();
        let item_box = gtk::Box::new(if self.horizontal { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal }, 3);
        let overlay = gtk::Overlay::new();
        let gicon: Option<gio::Icon> = match icon {
            Some(name) => Some(gio::ThemedIcon::new(name).upcast()),
            None => item.app.as_ref().and_then(|app| app.icon.clone()),
        };
        let app_image = crate::ui::app_menu::popup::icon_image(gicon.as_ref(), self.item_icon_size());
        overlay.set_child(Some(&app_image));
        let magnifier = Magnifier::new(&overlay, &self.config.edge);
        let badge = gtk::Label::new(None);
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::Start);
        badge.add_css_class("waydock-count");
        overlay.add_overlay(&badge);
        badge.set_can_target(false);
        pack(&item_box, &magnifier, false, false, 0);
        let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        indicator.set_halign(gtk::Align::Center);
        indicator.set_valign(gtk::Align::Center);
        indicator.set_size_request(if self.horizontal { 6 } else { 3 }, if self.horizontal { 3 } else { 6 });
        pack(&item_box, &indicator, false, false, 0);
        button.set_child(Some(&item_box));
        let entry = Rc::new(ItemButton {
            button: button.clone(), icon_override: std::cell::RefCell::new(icon.map(str::to_owned)), app_image, item_box,
            magnifier, count_badge: badge, indicator, dock_item: std::cell::RefCell::new(None),
        });
        self.update_item_button(&entry, item);
        let (dock, weak) = (self.this.clone(), Rc::downgrade(&entry));
        button.connect_clicked(move |_| {
            let (Some(dock), Some(entry)) = (dock.upgrade(), weak.upgrade()) else { return };
            let item = entry.dock_item.borrow().clone();
            if let Some(item) = item {
                dock.activate_item(&item);
            }
        });
        let (dock, weak) = (self.this.clone(), Rc::downgrade(&entry));
        click(&button, 3, false, true, move |widget, event| {
            let (Some(dock), Some(entry)) = (dock.upgrade(), weak.upgrade()) else { return false };
            let item = entry.dock_item.borrow().clone();
            item.is_some_and(|item| dock.item_menu(widget, event.button, &item))
        });
        let hover = gtk::EventControllerMotion::new();
        let (dock, weak) = (self.this.clone(), Rc::downgrade(&entry));
        hover.connect_enter(move |_, _, _| {
            let (Some(dock), Some(entry)) = (dock.upgrade(), weak.upgrade()) else { return };
            let item = entry.dock_item.borrow().clone();
            if let Some(item) = item {
                dock.preview().queue(&item);
            }
        });
        let dock = self.this.clone();
        hover.connect_leave(move |_| if let Some(dock) = dock.upgrade() { dock.preview().cancel_timer() });
        button.add_controller(hover);
        if icon.is_none() {
            let weak = Rc::downgrade(&entry);
            let (begin, end) = (self.this.clone(), self.this.clone());
            crate::ui::app_menu::drag::install_button_drag_source(&button,
                move |_| weak.upgrade().and_then(|e| e.dock_item.borrow().as_ref().map(|i| i.desktop_id())).unwrap_or_default(),
                move || if let Some(dock) = begin.upgrade() {
                    let preview = dock.preview();
                    preview.cancel_timer();
                    preview.close();
                    dock.enter("drag");
                },
                move || if let Some(dock) = end.upgrade() { dock.leave("drag") });
        }
        entry
    }

    pub(super) fn update_item_button(&self, entry: &ItemButton, item: &Item) {
        let previous = entry.dock_item.replace(Some(item.clone()));
        if previous.as_ref().is_none_or(|p| !same_app(&p.app, &item.app)) {
            let gicon: gio::Icon = match entry.icon_override.borrow().as_deref() {
                Some(name) => gio::ThemedIcon::new(name).upcast(),
                None => item.app.as_ref().and_then(|a| a.icon.clone())
                    .unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast()),
            };
            entry.app_image.set_from_gicon(&gicon);
        }
        entry.app_image.set_pixel_size(self.item_icon_size());
        let compact = self.config.application_display == "title";
        self.toggle_class(&entry.button, "waydock-compact-item", compact);
        entry.item_box.set_spacing(if compact { 1 } else { 3 });
        let align = if compact { gtk::Align::Center } else { gtk::Align::Fill };
        if self.horizontal { entry.button.set_valign(align) } else { entry.button.set_halign(align) }
        if previous.as_ref().is_none_or(|p| p.count() != item.count()) {
            entry.count_badge.set_text(&item.count().to_string());
            // The running badge shows only for more than one window.
            entry.count_badge.set_visible(item.count() > 1);
        }
        self.toggle_class(&entry.button, "focused", item.focused());
        self.toggle_class(&entry.indicator, "waydock-indicator", item.running());
        self.toggle_class(&entry.indicator, "vertical", !self.horizontal && item.running());
        self.toggle_class(&entry.indicator, "focused", item.focused());
        if previous.as_ref().is_none_or(|p| p.name != item.name || p.running() != item.running()) {
            entry.button.set_tooltip_text(Some(&item.name));
        }
        if previous.as_ref().is_none_or(|p| p.name != item.name || p.count() != item.count()) {
            let text = if item.count() > 0 {
                tr!("{application}, {count} windows", application = item.name.as_str(), count = item.count() as i64)
            } else {
                item.name.clone()
            };
            accessible_name(&entry.button, &text);
        }
    }

    pub(super) fn toggle_class(&self, widget: &impl IsA<gtk::Widget>, name: &str, enabled: bool) {
        if widget.has_css_class(name) != enabled {
            if enabled { widget.add_css_class(name) } else { widget.remove_css_class(name) }
        }
    }

    /// `_magnify_at(x, y)`: scale icons by distance from the pointer.
    pub(super) fn magnify_at(&self, point: Option<(f64, f64)>) {
        let radius = self.item_icon_size() as f64 * 2.5;
        let buttons: Vec<Rc<ItemButton>> = self.buttons.borrow().values().cloned().collect();
        for button in buttons {
            let magnifier = &button.magnifier;
            if !magnifier.is_mapped() {
                continue;
            }
            let Some((x, y)) = point else {
                magnifier.set_target(1.0);
                continue;
            };
            // Magnified icons draw past the clipping scroller into the headroom.
            let mut widget = magnifier.parent();
            while let Some(current) = widget.filter(|w| *w != self.panel) {
                if current.overflow() != gtk::Overflow::Visible {
                    current.set_overflow(gtk::Overflow::Visible);
                }
                widget = current.parent();
            }
            let center = gtk::graphene::Point::new(magnifier.width() as f32 / 2.0, magnifier.height() as f32 / 2.0);
            if let Some(p) = magnifier.compute_point(&self.panel, &center) {
                let distance = if self.horizontal { (p.x() as f64 - x).abs() } else { (p.y() as f64 - y).abs() };
                let closeness = ((distance / radius).min(1.0) * std::f64::consts::PI / 2.0).cos();
                magnifier.set_target(1.0 + (self.config.hover_scale - 1.0) * closeness);
            }
        }
    }

    pub(super) fn sync_input_region(&self) {
        if (!self.magnify && self.shadow_room == 0) || !self.panel.is_mapped() {
            return;
        }
        let Some(surface) = self.window.surface() else { return };
        let Some(bounds) = self.panel.compute_bounds(&self.window) else { return };
        let (offset_x, offset_y) = self.window.surface_transform();
        let rectangle = ((bounds.x() as f64 + offset_x) as i32, (bounds.y() as f64 + offset_y) as i32,
                         bounds.width().ceil() as i32, bounds.height().ceil() as i32);
        if Some(rectangle) != self.input_bounds.get() {
            self.input_bounds.set(Some(rectangle));
            let region = gtk::cairo::Region::create_rectangle(&gtk::cairo::RectangleInt::new(
                rectangle.0, rectangle.1, rectangle.2, rectangle.3));
            surface.set_input_region(Some(&region));
        }
    }

    pub fn activate_item(&self, item: &Item) {
        if item.windows.is_empty() {
            if let Some(expander) = self.application_expander() {
                expander.set_expanded(false);
            }
        }
        if item.windows.is_empty() && role_of(&item.id).is_some() {
            self.launch_utility(&item.id);
        } else if item.windows.is_empty() && item.app.is_some() {
            if let Some(app) = self.app() {
                app.launch(&item.app.as_ref().expect("app").id, Some(self.launch_context()));
            }
        } else if item.windows.len() == 1 {
            self.activate_window(&item.windows[0].id);
        } else {
            self.preview().show(item);
        }
    }

    pub fn launch_utility(&self, item_id: &str) {
        let Some(app) = self.app() else { return };
        if item_id == utility_id("home") {
            app.desktop_actions().open_home(Some(self.launch_context()), None);
        } else if item_id == utility_id("settings") {
            app.open_global_settings();
        } else if item_id == utility_id("trash") {
            app.desktop_actions().open_trash(Some(self.launch_context()), None);
        }
    }

    pub fn activate_window(&self, window_id: &str) -> bool { self.preview().activate_window(window_id) }

    pub fn toggle_focused_title(&self) {
        let Some(app) = self.app() else { return };
        let Some(backend) = app.backend() else { return };
        let Some(id) = self.title_window_id.borrow().clone() else { return };
        let Some(window) = backend.window(&id) else { return };
        let state = app.focused_title();
        let restore = state.borrow().restore_id.as_deref() == Some(window.id.as_str()) || window.minimized || !window.activated;
        let succeeded = if restore {
            // Activate directly, even before a previous minimize is
            // acknowledged, so rapid double-clicks cannot minimize twice.
            state.borrow_mut().clear();
            backend.activate(&window.id)
        } else {
            state.borrow_mut().remember(&window.id);
            backend.minimize(&window.id)
        };
        if !succeeded {
            state.borrow_mut().clear();
            app.show_error(&tr!("This window is no longer available."));
        }
        for dock in app.docks() {
            let items = dock.source_items();
            dock.render(items);
        }
    }

    pub fn close_all_windows(&self, item_id: &str) { self.preview().close_all_windows(item_id) }

    /// `popup_menu(widget, event, entries)`.
    pub fn popup_menu(&self, widget: &gtk::Widget, entries: Vec<Entry>) {
        if let Some(expander) = self.indicators().and_then(|i| i.tray_expander()) {
            expander.set_expanded(false);
        }
        if let Some(expander) = self.application_expander() {
            expander.menu_active(true);
        }
        self.close_preview();
        self.close_app_menu();
        self.enter("menu");
        let menu = gtk::Popover::new();
        menu.set_autohide(true);
        menu.set_has_arrow(false);
        menu.add_css_class(&self.popup_class);
        menu.add_css_class("waydock-context-menu");
        menu.set_parent(widget);
        menu.set_position(match self.config.edge.as_str() {
            "bottom" => gtk::PositionType::Top,
            "top" => gtk::PositionType::Bottom,
            "left" => gtk::PositionType::Right,
            _ => gtk::PositionType::Left,
        });
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 2);
        menu.set_child(Some(&rows));
        self.set_popover_blur(&menu);
        self.context_menu.replace(Some(menu.clone()));
        for (title, callback) in entries {
            let row = gtk::Button::with_label(&title);
            row.set_halign(gtk::Align::Fill);
            row.set_sensitive(callback.is_some());
            if let Some(action) = callback {
                let menu = menu.downgrade();
                row.connect_clicked(move |_| {
                    if let Some(menu) = menu.upgrade() {
                        menu.popdown();
                    }
                    action();
                });
            }
            rows.append(&row);
        }
        let weak = self.this.clone();
        menu.connect_closed(move |menu| {
            let Some(dock) = weak.upgrade() else { return };
            if dock.context_menu.borrow().as_ref() == Some(menu) {
                dock.context_menu.replace(None);
                dock.leave("menu");
            }
            if let Some(indicators) = dock.indicators() {
                indicators.schedule_apps_collapse();
            }
            if let Some(expander) = dock.application_expander() {
                expander.menu_active(false);
            }
            let menu = menu.clone();
            glib::idle_add_local_once(move || {
                if menu.parent().is_some() {
                    menu.unparent();
                }
            });
        });
        menu.popup();
    }

    fn common_menu_entries(&self) -> Vec<Entry> {
        let weak = self.app.clone();
        vec![(tr!("Dock settings"), Some(Rc::new(move || if let Some(app) = weak.upgrade() { app.show_settings() })))]
    }

    fn action(&self, f: impl Fn(&DockSurface) + 'static) -> Option<Rc<dyn Fn()>> {
        let weak = self.this.clone();
        Some(Rc::new(move || if let Some(dock) = weak.upgrade() { f(&dock) }))
    }

    pub fn item_menu(&self, button: &gtk::Widget, event_button: u32, item: &Item) -> bool {
        if event_button != 3 {
            return false;
        }
        if role_of(&item.id).is_some() {
            if item.id == utility_id("trash") {
                return self.trash_menu(button, event_button);
            }
            let title = if item.id == utility_id("home") { tr!("Open Home Folder") } else { tr!("Open Global Settings") };
            let key = item.id.clone();
            let mut entries = vec![(title, self.action(move |dock| dock.launch_utility(&key)))];
            if item.running() {
                let key = item.id.clone();
                entries.push((tr!("Close All"), self.action(move |dock| dock.close_all_windows(&key))));
            }
            entries.extend(self.common_menu_entries());
            self.popup_menu(button, entries);
            return true;
        }
        let mut entries: Vec<Entry> = Vec::new();
        let Some(app) = self.app() else { return false };
        if let Some(entry) = &item.app {
            let id = entry.id.clone();
            entries.push((tr!("Launch application"), self.action(move |dock| {
                if let Some(app) = dock.app() {
                    app.launch(&id, Some(dock.launch_context()));
                }
            })));
            let id = entry.id.clone();
            let title = if item.pinned { tr!("Unpin application") } else { tr!("Pin application") };
            entries.push((title, self.action(move |dock| if let Some(app) = dock.app() { app.toggle_pin(&id) })));
        } else if item.pinned {
            let id = item.desktop_id();
            entries.push((tr!("Remove missing application pin"), self.action(move |dock| {
                if let Some(app) = dock.app() {
                    app.toggle_pin(&id);
                }
            })));
        }
        if item.running() {
            let key = item.id.clone();
            entries.push((tr!("Close All"), self.action(move |dock| dock.close_all_windows(&key))));
        }
        if item.pinned {
            let pins = app.config().pins.clone();
            let desktop_id = item.desktop_id();
            let catalog = app.catalog();
            let pin = pins.iter().find(|pin| **pin == desktop_id || item.app.as_ref().is_some_and(|entry| {
                catalog.borrow().resolve(pin, None).is_some_and(|resolved| resolved == entry)
            })).cloned().unwrap_or(desktop_id);
            let index = pins.iter().position(|p| *p == pin).unwrap_or(0);
            let (earlier, later) = (pin.clone(), pin.clone());
            entries.push((tr!("Move earlier"), if index > 0 {
                self.action(move |dock| if let Some(app) = dock.app() { app.move_pin(&earlier, -1) })
            } else {
                None
            }));
            entries.push((tr!("Move later"), if index + 1 < pins.len() {
                self.action(move |dock| if let Some(app) = dock.app() { app.move_pin(&later, 1) })
            } else {
                None
            }));
        }
        entries.extend(self.common_menu_entries());
        self.popup_menu(button, entries);
        true
    }

    pub(super) fn background_menu(&self, button: u32, _x: f64, _y: f64) -> bool {
        if button == 3 {
            self.popup_menu(&self.panel.clone(), self.common_menu_entries());
            return true;
        }
        false
    }

    pub(super) fn background_activate(&self, window: &gtk::Widget, button: u32, x: f64, y: f64) -> bool {
        if button != 1 || !self.bar_mode || self.visible_groups.borrow().as_ref().is_some_and(|v| v.iter().any(|n| n == "app_menu")) {
            return false;
        }
        // Treat every group's content (including insensitive controls) as an
        // occupied area; only the panel's empty gaps activate the hidden menu.
        let Some(target) = window.pick(x, y, gtk::PickFlags::INSENSITIVE) else { return false };
        let groups = self.groups.borrow().clone();
        for (_, (_line, widgets)) in groups.iter() {
            if widgets.iter().any(|widget| target == *widget || target.is_ancestor(widget)) {
                return false;
            }
        }
        self.toggle_app_menu();
        true
    }

    fn trash_menu(&self, button: &gtk::Widget, event_button: u32) -> bool {
        if event_button != 3 {
            return false;
        }
        let Some(app) = self.app() else { return false };
        let mut entries: Vec<Entry> = vec![
            (tr!("Open Trash"), self.action(|dock| {
                if let Some(app) = dock.app() {
                    app.desktop_actions().open_trash(Some(dock.launch_context()), None);
                }
            })),
            (tr!("Empty Trash…"), if app.desktop_actions().busy() {
                None
            } else {
                self.action(|dock| if let Some(app) = dock.app() { app.confirm_empty_trash() })
            }),
        ];
        if self.utility_items.borrow().get(&utility_id("trash")).is_some_and(|item| item.running()) {
            entries.push((tr!("Close All"), self.action(|dock| dock.close_all_windows(&utility_id("trash")))));
        }
        entries.extend(self.common_menu_entries());
        self.popup_menu(button, entries);
        true
    }

    pub fn set_trash_count(&self, count: Option<u32>) {
        let Some(trash) = self.utility_buttons.borrow().get("trash").cloned() else { return };
        let icon = if count.is_some_and(|c| c > 0) { "user-trash-full" } else { "user-trash" };
        if trash.icon_override.borrow().as_deref() != Some(icon) {
            trash.icon_override.replace(Some(icon.to_owned()));
            trash.app_image.set_icon_name(Some(icon));
            trash.app_image.set_pixel_size(self.item_icon_size());
        }
        let tooltip = match count {
            Some(count) => tr!("Trash · {value1} items", value1 = count as i64),
            None => tr!("Trash"),
        };
        if trash.button.tooltip_text().as_deref() != Some(tooltip.as_str()) {
            trash.button.set_tooltip_text(Some(&tooltip));
        }
        accessible_description(&trash.button, &tooltip);
    }
}

impl WindowRecord for Window {
    fn id(&self) -> String { self.id.clone() }
    fn app_id(&self) -> String { self.app_id.clone() }
    fn title(&self) -> String { self.title.clone() }
    fn activated(&self) -> bool { self.activated }
    fn minimized(&self) -> bool { self.minimized }
    fn parent_id(&self) -> String { self.parent_id.clone() }
}
