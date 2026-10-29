//! DockSurface groups and layout: `_build_groups`, `_arrange_application_groups`,
//! `group_zones`, `_arrange_custom_groups`, `add_group`, `refresh_groups`,
//! `_refresh_legacy_zone_visibility`, `_panel_layout` (+ custom/bar limits),
//! `status_scroll`, `separator`, `scroll_horizontal`.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

use super::{natural, DockSurface, Groups, Zones};
use crate::core::model::utility_id;
use crate::ui::app_menu::popup::children;
use crate::ui::host::{accessible_name, pack, remove, scroll as scroll_controller, DockApp, DockView};
use crate::ui::indicators::DockIndicators;
use crate::ui::tray::{ExpanderOptions, TrayExpander};
use suite_core::tr;

fn zones(lists: &[&[&str]]) -> Zones { lists.iter().map(|zone| zone.iter().map(|s| s.to_string()).collect()).collect() }

/// `group_zones()` without custom layouts (pure).
pub fn legacy_group_zones(title_mode: bool, bar_mode: bool) -> Zones {
    if title_mode {
        if bar_mode {
            return zones(&[&["app_menu", "weather", "workspaces", "files", "pinned", "activities", "focused_title"], &["clock"],
                           &["media", "system_tray", "service_apps", "session"]]);
        }
        return zones(&[&["app_menu", "workspaces", "files", "applications", "focused_title", "media", "system_tray",
                         "service_apps", "weather", "clock", "session"], &["pinned", "running"]]);
    }
    if bar_mode {
        return zones(&[&["weather", "workspaces"], &["app_menu", "pinned", "running", "files"],
                       &["media", "system_tray", "service_apps", "clock", "session"]]);
    }
    zones(&[&["app_menu", "pinned", "running", "files", "workspaces", "media", "system_tray", "service_apps", "weather", "clock",
              "session"]])
}

/// `_custom_layout_limits` arithmetic (sides, center) for three requests.
pub fn custom_limits(available: i32, requests: [i32; 3]) -> [i32; 3] {
    let available = (available - 6).max(1);
    let center_min = 60.min(requests[1]);
    let side_span = requests[0].max(requests[2]).min(((available - center_min).div_euclid(2)).max(1));
    let center_limit = (available - 2 * side_span).max(1);
    let center_span = requests[1].min(center_limit);
    let side_limit = ((available - center_span).div_euclid(2)).max(1);
    [side_limit, center_limit, side_limit]
}

/// The menu icon file (`icons/waydock-app-menu-symbolic.svg` of the runtime
/// directory; an embedded copy is extracted to `$XDG_RUNTIME_DIR/waydock`).
pub fn menu_icon_path() -> std::path::PathBuf {
    const NAME: &str = "waydock-app-menu-symbolic.svg";
    let installed = suite_core::paths::data_home().join("waydock/icons").join(NAME);
    if installed.is_file() {
        return installed;
    }
    let extracted = glib::user_runtime_dir().join("waydock").join(NAME);
    let data: &[u8] = include_bytes!("../../app/waydock-app-menu-symbolic.svg");
    if std::fs::read(&extracted).ok().as_deref() != Some(data) {
        let _ = std::fs::create_dir_all(extracted.parent().expect("parent"));
        let _ = std::fs::write(&extracted, data);
    }
    extracted
}

impl DockSurface {
    fn set_policy_strip(&self, scroll: &gtk::ScrolledWindow) {
        scroll.set_policy(if self.horizontal { gtk::PolicyType::External } else { gtk::PolicyType::Never },
                          if self.horizontal { gtk::PolicyType::Never } else { gtk::PolicyType::External });
    }

    pub(super) fn action_button(&self, icon: &gio::Icon, tooltip: &str, callback: impl Fn() + 'static) -> gtk::Button {
        let button = gtk::Button::new();
        let image = gtk::Image::from_gicon(icon);
        image.set_pixel_size(self.item_icon_size());
        button.set_child(Some(&image));
        button.set_tooltip_text(Some(tooltip));
        accessible_name(&button, tooltip);
        button.connect_clicked(move |_| callback());
        button
    }

    /// `_build_groups()`: every section is created once.
    pub(super) fn build_groups(self: &Rc<Self>) {
        self.building_groups.set(true);
        if self.positioned_layout {
            let core = gtk::Box::new(self.orientation, 3);
            core.add_css_class("waydock-applications");
            if let Some(center) = self.panel.downcast_ref::<gtk::CenterBox>() {
                center.set_center_widget(Some(&core));
            }
            self.core.replace(core);
        }
        let core = self.core();
        let menu_icon: gio::Icon = gio::FileIcon::new(&gio::File::for_path(menu_icon_path())).upcast();
        let weak = self.this.clone();
        let menu_button = self.action_button(&menu_icon, &tr!("Applications"), move || {
            if let Some(dock) = weak.upgrade() {
                dock.toggle_app_menu();
            }
        });
        self.add_group_widgets("app_menu", &core, &[menu_button.clone().upcast()], true);
        let app_scroll = gtk::ScrolledWindow::new();
        self.set_policy_strip(&app_scroll);
        app_scroll.set_propagate_natural_width(true);
        app_scroll.set_propagate_natural_height(true);
        if self.horizontal {
            self.install_wheel(&app_scroll);
        }
        let strip = gtk::Box::new(self.orientation, 3);
        app_scroll.set_child(Some(&strip));
        pack(&core, &app_scroll, false, false, 0);
        let pinned_box = gtk::Box::new(self.orientation, 3);
        let running_box = gtk::Box::new(self.orientation, 3);
        let focused_title = gtk::Label::new(None);
        focused_title.set_xalign(0.5);
        focused_title.add_css_class("waydock-focused-title");
        focused_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        focused_title.set_max_width_chars(if self.horizontal { 28 } else { 6 });
        focused_title.set_wrap(!self.horizontal);
        focused_title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        focused_title.set_lines(if self.horizontal { 1 } else { 3 });
        focused_title.set_valign(gtk::Align::Center);
        let focused_title_button = gtk::Button::new();
        focused_title_button.add_css_class("waydock-focused-title-button");
        focused_title_button.set_child(Some(&focused_title));
        let weak = self.this.clone();
        focused_title_button.connect_clicked(move |_| {
            if let Some(dock) = weak.upgrade() {
                dock.toggle_focused_title();
            }
        });
        self.add_group_widgets("focused_title", &strip, &[focused_title_button.clone().upcast()], false);
        self.add_group_widgets("pinned", &strip, &[pinned_box.clone().upcast()], false);
        // Drops anywhere on the dock pin or reorder relative to pinned icons.
        let host: std::rc::Weak<dyn crate::ui::app_menu::drag::DropHost> = self.this.clone();
        self.drop_target.replace(Some(crate::ui::app_menu::drag::install_drop_target(&self.panel, host)));
        self.add_group_widgets("running", &strip, &[running_box.clone().upcast()], false);
        let application_strip = gtk::Box::new(self.orientation, 3);
        let activities_button = gtk::Button::new();
        let _ = self.widgets.set(Groups {
            menu_button, app_scroll: app_scroll.clone(), center_strip: strip, pinned_box, running_box, focused_title,
            focused_title_button, application_strip: application_strip.clone(), activities_button: activities_button.clone(),
        });
        let mut utility = Vec::new();
        for (role, icon) in [("home", "user-home"), ("settings", "preferences-system"), ("trash", "user-trash")] {
            let item = self.utility_items.borrow().get(&utility_id(role)).cloned();
            let item = item.unwrap_or_else(|| crate::core::model::DockItem {
                id: utility_id(role), app: None, app_id: String::new(), name: String::new(), pinned: false, windows: Vec::new() });
            let button = self.new_item_button(&item, Some(icon));
            utility.push(button.button.clone().upcast::<gtk::Widget>());
            self.utility_buttons.borrow_mut().insert(role.to_owned(), button);
        }
        self.add_group_widgets("files", &core, &utility, true);
        let app = self.app();
        if !self.is_app_dock && app.as_ref().is_some_and(|app| app.status_backend().is_some()) {
            let utility_scroll = self.status_scroll();
            self.utility_scroll.replace(Some(utility_scroll.clone()));
            let view: Rc<dyn DockView> = self.clone();
            let dock_app: Rc<dyn DockApp> = app.as_ref().expect("app").dock_app();
            let indicators = DockIndicators::new(&view, dock_app);
            self.indicators.replace(Some(indicators.clone()));
            utility_scroll.set_child(Some(&indicators.widget()));
            if self.positioned_layout {
                if indicators.bar_start().is_none() {
                    indicators.set_bar_start(Some(gtk::Box::new(self.orientation, 3)));
                }
                let start_scroll = self.status_scroll();
                start_scroll.set_child(indicators.bar_start().as_ref());
                self.start_scroll.replace(Some(start_scroll.clone()));
                if let Some(center) = self.panel.downcast_ref::<gtk::CenterBox>() {
                    center.set_start_widget(Some(&start_scroll));
                    center.set_end_widget(Some(&utility_scroll));
                }
            } else if let Some(panel) = self.panel.downcast_ref::<gtk::Box>() {
                pack(panel, &utility_scroll, false, false, 0);
            }
        }
        if let Some(app) = &app {
            let view: Rc<dyn DockView> = self.clone();
            let dock_app: Rc<dyn DockApp> = app.dock_app();
            let (w1, w2, w3, w4) = (self.this.clone(), self.this.clone(), self.this.clone(), self.this.clone());
            let expander = TrayExpander::new(&view, &dock_app, application_strip.upcast_ref(), ExpanderOptions {
                kind: Some("applications".into()),
                available: Some(Rc::new(move || w1.upgrade().is_some_and(|d| d.application_icons_available()))),
                enabled: Some(Rc::new(move || w2.upgrade().is_some_and(|d| {
                    d.config.application_display == "title" && !d.bar_mode && d.custom_group_zones().is_none()
                }))),
                close_content: Some(Rc::new(move || if let Some(d) = w3.upgrade() { d.close_application_content() })),
                extra_holes: Some(Rc::new(move || w4.upgrade().map(|d| d.application_preview_regions()).unwrap_or_default())),
            });
            self.application_expander.replace(Some(expander.clone()));
            let parent = self.indicators().map(|i| i.widget()).unwrap_or_else(|| self.core());
            self.add_group_widgets("applications", &parent, &[expander.widget().upcast()], false);
        }
        let image = gtk::Image::from_icon_name("focus-windows-symbolic");
        image.set_pixel_size(20);
        image.add_css_class("waydock-activities-icon");
        activities_button.set_child(Some(&image));
        activities_button.set_tooltip_text(Some(&tr!("Activities")));
        let weak = self.this.clone();
        activities_button.connect_clicked(move |_| {
            if let Some(dock) = weak.upgrade() {
                if let Some(app) = dock.app() {
                    app.open_activities(&dock.monitor_name);
                }
            }
        });
        activities_button.add_css_class("waydock-compact-item");
        activities_button.set_valign(gtk::Align::Center);
        activities_button.set_halign(gtk::Align::Center);
        accessible_name(&activities_button, &tr!("Activities"));
        let parent = self.indicators().map(|i| i.widget()).unwrap_or_else(|| self.core());
        self.add_group_widgets("activities", &parent, &[activities_button.upcast()], false);
        self.panel.set_visible(true);
        self.building_groups.set(false);
    }

    fn install_wheel(&self, scroll: &gtk::ScrolledWindow) {
        let weak = self.this.clone();
        scroll_controller(scroll, move |widget, dx, dy| {
            let (Some(dock), Some(scroll)) = (weak.upgrade(), widget.downcast_ref::<gtk::ScrolledWindow>()) else { return false };
            dock.scroll_horizontal(scroll, dx, dy)
        });
    }

    pub(super) fn application_icons_available(&self) -> bool {
        let available = self.group_available.borrow();
        ["pinned", "running"].iter().any(|name| self.config.is_group_visible(name) && available.get(*name).copied().unwrap_or(false))
    }

    fn close_application_content(&self) {
        self.close_preview();
        if let Some(menu) = self.context_menu() {
            menu.popdown();
        }
    }

    fn application_preview_regions(&self) -> Vec<crate::ui::tray::Hole> {
        use gtk4_layer_shell::{Edge, LayerShell};
        let Some(popup) = self.preview().window().filter(|p| p.is_visible()) else { return Vec::new() };
        let geometry = self.monitor.geometry();
        let (width, height) = (popup.width(), popup.height());
        let x = if popup.is_anchor(Edge::Left) { popup.margin(Edge::Left) } else { geometry.width() - popup.margin(Edge::Right) - width };
        let y = if popup.is_anchor(Edge::Top) { popup.margin(Edge::Top) } else { geometry.height() - popup.margin(Edge::Bottom) - height };
        vec![(self.monitor.clone(), x, y, width, height)]
    }

    fn move_group(&self, name: &str, parent: &gtk::Box) {
        let Some((line, widgets)) = self.groups.borrow().get(name).cloned() else { return };
        for widget in std::iter::once(line.upcast::<gtk::Widget>()).chain(widgets) {
            remove(&widget);
            parent.append(&widget);
        }
    }

    pub(super) fn arrange_application_groups(&self) {
        let title_mode = self.config.application_display == "title";
        let custom = self.custom_group_zones();
        let signature = (title_mode, custom.clone());
        if self.arranged.borrow().as_ref() == Some(&signature) {
            return;
        }
        let Some(groups) = self.widgets.get() else { return };
        if let Some(expander) = self.application_expander() {
            expander.set_expanded(false);
        }
        self.close_preview();
        self.arranged.replace(Some(signature));
        groups.app_scroll.set_policy(
            if title_mode || !self.horizontal { gtk::PolicyType::Never } else { gtk::PolicyType::External },
            if title_mode || self.horizontal { gtk::PolicyType::Never } else { gtk::PolicyType::External });
        self.toggle_class(&self.panel, "waydock-title-mode", title_mode);
        // A full-length app dock also counts as a bar; Compact bar is for the bar itself.
        self.toggle_class(&self.panel, "waydock-compact-bar",
                          self.bar_mode && (title_mode || (self.config.compact_bar && !self.is_app_dock)));
        self.toggle_class(&self.panel, "waydock-vertical", !self.horizontal);
        if let Some(image) = groups.menu_button.child().and_downcast::<gtk::Image>() {
            image.set_pixel_size(self.item_icon_size());
        }
        self.toggle_class(&groups.menu_button, "waydock-compact-item", title_mode);
        let align = if title_mode { gtk::Align::Center } else { gtk::Align::Fill };
        if self.horizontal { groups.menu_button.set_valign(align) } else { groups.menu_button.set_halign(align) }
        if let Some(custom) = custom {
            self.arrange_custom_groups(&custom);
            return;
        }
        let core = self.core();
        let indicators = self.indicators();
        let start = match (&indicators, self.bar_mode) {
            (Some(indicators), true) => indicators.bar_start().unwrap_or_else(|| core.clone()),
            _ => core.clone(),
        };
        let end = indicators.as_ref().map(|i| i.widget()).unwrap_or_else(|| core.clone());
        let mv = |widget: &gtk::Widget, parent: &gtk::Box| {
            remove(widget);
            parent.append(widget);
        };
        let app_scroll: gtk::Widget = groups.app_scroll.clone().upcast();
        if title_mode {
            self.move_group("app_menu", &start);
            if self.bar_mode {
                self.move_group("weather", &start);
            }
            self.move_group("workspaces", &start);
            self.move_group("files", &start);
            if self.bar_mode {
                self.move_group("pinned", &start);
                self.move_group("activities", &start);
                self.move_group("focused_title", &start);
            } else {
                self.move_group("applications", &start);
            }
            mv(&app_scroll, &core);
            if !self.bar_mode {
                self.move_group("focused_title", &groups.center_strip);
                self.move_group("pinned", &groups.application_strip);
            }
            self.move_group("running", &groups.application_strip);
        } else {
            self.move_group("app_menu", &core);
            mv(&app_scroll, &core);
            for name in ["pinned", "running", "focused_title"] {
                self.move_group(name, &groups.center_strip);
            }
            self.move_group("files", &core);
            if self.bar_mode {
                self.move_group("weather", &start);
            }
            self.move_group("workspaces", if self.bar_mode { &start } else { &end });
        }
        if !title_mode {
            self.move_group("applications", &end);
        }
        for name in ["media", "system_tray", "service_apps", "weather", "clock", "session"] {
            if name != "weather" || !self.bar_mode {
                self.move_group(name, if name == "clock" && title_mode && self.bar_mode { &groups.center_strip } else { &end });
            }
        }
        groups.focused_title.set_xalign(if title_mode && self.bar_mode { 0.0 } else { 0.5 });
        if !self.bar_mode {
            if let Some(scroll) = self.utility_scroll() {
                mv(scroll.upcast_ref(), &core);
            }
        }
    }

    pub fn group_zones(&self) -> Zones {
        if let Some(custom) = self.custom_group_zones() {
            return custom;
        }
        legacy_group_zones(self.config.application_display == "title", self.bar_mode)
    }

    fn scroll_content(scroll: &gtk::ScrolledWindow) -> Option<gtk::Box> {
        let child = scroll.child()?;
        match child.downcast::<gtk::Viewport>() {
            Ok(viewport) => viewport.child().and_downcast::<gtk::Box>(),
            Err(child) => child.downcast::<gtk::Box>().ok(),
        }
    }

    fn arrange_custom_groups(&self, zones: &Zones) {
        let Some(groups) = self.widgets.get() else { return };
        let center = self.panel.downcast_ref::<gtk::CenterBox>();
        if self.start_scroll.borrow().is_none() {
            let scroll = self.status_scroll();
            scroll.set_child(Some(&gtk::Box::new(self.orientation, 3)));
            if let Some(center) = center {
                center.set_start_widget(Some(&scroll));
            }
            self.start_scroll.replace(Some(scroll));
        }
        if self.utility_scroll.borrow().is_none() {
            let scroll = self.status_scroll();
            scroll.set_child(Some(&gtk::Box::new(self.orientation, 3)));
            if let Some(center) = center {
                center.set_end_widget(Some(&scroll));
            }
            self.utility_scroll.replace(Some(scroll));
        }
        let (start, utility) = (self.start_scroll().expect("start"), self.utility_scroll().expect("end"));
        let parents = [Self::scroll_content(&start), Some(groups.center_strip.clone()), Self::scroll_content(&utility)];
        for scroll in [&start, &groups.app_scroll, &utility] {
            self.set_policy_strip(scroll);
        }
        for (parent, names) in parents.iter().zip(zones) {
            let Some(parent) = parent else { continue };
            for name in names {
                self.move_group(name, parent);
            }
        }
        // Activities replaces the legacy combined application drawer when
        // groups have individually assigned positions, so pins can move alone.
        if let Some((line, widgets)) = self.groups.borrow().get("applications").cloned() {
            for widget in std::iter::once(line.upcast::<gtk::Widget>()).chain(widgets) {
                remove(&widget);
                groups.center_strip.append(&widget);
                widget.set_visible(false);
            }
        }
        groups.focused_title.set_xalign(if zones.get(1).is_some_and(|z| z.iter().any(|n| n == "focused_title")) { 0.5 } else { 0.0 });
    }

    pub(super) fn add_group_widgets(&self, name: &str, container: &gtk::Box, widgets: &[gtk::Widget], available: bool) {
        let line = self.separator(container);
        for widget in widgets {
            pack(container, widget, false, false, 0);
            widget.set_visible(true);
        }
        self.groups.borrow_mut().insert(name.to_owned(), (line, widgets.to_vec()));
        self.group_available.borrow_mut().insert(name.to_owned(), available);
    }

    pub fn refresh_groups(&self) {
        if self.destroyed.get() || self.building_groups.get() {
            return;
        }
        let config = self.config.clone();
        let enabled = |name: &str| config.is_group_visible(name);
        if self.utility_enabled.get().is_some_and(|known| known != enabled("files")) {
            // A hidden utility group must immediately return its windows to
            // the taskbar, including when changed in a live settings preview.
            if let Some(this) = self.rc() {
                this.render(self.source_items());
            }
            return;
        }
        if let Some(indicators) = self.indicators() {
            indicators.sync_apps_visibility();
            let availability = indicators.group_availability();
            {
                let mut available = self.group_available.borrow_mut();
                for (name, value) in availability {
                    available.insert(name.to_owned(), value);
                }
            }
            indicators.refresh_trays(false);
        }
        let title_mode = config.application_display == "title";
        let custom = self.custom_group_zones();
        // A top bar beside a separate app dock offers Activities and the
        // focused window title on their own (GNOME and macOS menu bars).
        let paired = self.bar_mode && !self.is_app_dock && config.uses_separate_app_dock();
        let applications = title_mode && custom.is_none() && !self.bar_mode && self.application_icons_available();
        {
            let mut available = self.group_available.borrow_mut();
            available.insert("focused_title".into(), self.names_focused_window() && enabled("running"));
            available.insert("applications".into(), applications);
            available.insert("activities".into(), paired || (title_mode && (self.bar_mode || custom.is_some()) && enabled("running")));
        }
        let zones = self.group_zones();
        let visible: Vec<String> = {
            let groups = self.groups.borrow();
            let available = self.group_available.borrow();
            zones.iter().flatten().filter(|name| {
                groups.contains_key(*name) && enabled(name) && available.get(*name).copied().unwrap_or(false)
                    && !(custom.is_some() && title_mode && *name == "running")
            }).cloned().collect()
        };
        if let Some(preview_id) = self.preview().preview_id() {
            let group = if self.utility_items.borrow().contains_key(&preview_id) {
                "files"
            } else if self.find_item(&preview_id).is_some_and(|item| item.pinned) {
                "pinned"
            } else {
                "running"
            };
            if !visible.iter().any(|name| name == group) {
                self.close_preview();
            }
        }
        let first: Vec<Option<&String>> = zones.iter().map(|zone| zone.iter().find(|name| visible.contains(name))).collect();
        let entries: Vec<(String, gtk::Separator, Vec<gtk::Widget>)> =
            self.groups.borrow().iter().map(|(k, (l, w))| (k.clone(), l.clone(), w.clone())).collect();
        for (name, line, widgets) in entries {
            let shown = visible.contains(&name);
            line.set_visible(shown && !first.contains(&Some(&name)));
            for widget in widgets {
                widget.set_visible(shown);
            }
        }
        let groups = self.widgets.get().expect("groups");
        if let Some(custom) = &custom {
            let scrolls = [self.start_scroll(), Some(groups.app_scroll.clone()), self.utility_scroll()];
            for (scroll, zone) in scrolls.iter().zip(custom) {
                if let Some(scroll) = scroll {
                    scroll.set_visible(zone.iter().any(|name| visible.contains(name)));
                }
            }
            self.core().set_visible(groups.app_scroll.is_visible());
        } else {
            self.refresh_legacy_zone_visibility(&visible, title_mode);
        }
        if let Some(expander) = self.application_expander() {
            expander.sync();
        }
        if self.visible_groups.borrow().as_ref() != Some(&visible) {
            let empty = visible.is_empty();
            self.visible_groups.replace(Some(visible));
            // Keep a right-click target available when all groups are hidden.
            let size = if empty { self.config.icon_size as i32 + 18 } else { -1 };
            self.panel.set_size_request(size, size);
            let tooltip = empty.then(|| tr!("All dock groups are hidden · Right-click for settings"));
            self.panel.set_tooltip_text(tooltip.as_deref());
            self.queue_panel_layout();
        }
    }

    fn refresh_legacy_zone_visibility(&self, visible: &[String], title_mode: bool) {
        let any = |names: &[&str]| names.iter().any(|name| visible.iter().any(|v| v == name));
        let groups = self.widgets.get().expect("groups");
        groups.app_scroll.set_visible(any(if title_mode && self.bar_mode {
            &["clock"]
        } else if title_mode {
            &["focused_title"]
        } else {
            &["pinned", "running"]
        }));
        if let Some(scroll) = self.utility_scroll() {
            let mut utilities: Vec<&str> = if title_mode && self.bar_mode {
                vec!["media", "system_tray", "service_apps", "session"]
            } else {
                vec!["media", "system_tray", "service_apps", "clock", "session"]
            };
            if !self.bar_mode {
                if title_mode { utilities.push("weather") } else { utilities.extend(["workspaces", "weather"]) }
            }
            scroll.set_visible(any(&utilities));
        }
        if self.bar_mode {
            self.core().set_visible(any(if title_mode { &["clock"] } else { &["app_menu", "pinned", "running", "files"] }));
            if let Some(start) = self.start_scroll() {
                start.set_visible(any(if title_mode {
                    &["app_menu", "weather", "workspaces", "files", "pinned", "activities", "focused_title"]
                } else {
                    &["weather", "workspaces"]
                }));
            }
        }
    }

    pub fn queue_panel_layout(&self) {
        if !self.destroyed.get() && self.layout_timer.borrow().is_none() {
            let weak = self.this.clone();
            let source = glib::idle_add_local_once(move || {
                if let Some(dock) = weak.upgrade() {
                    dock.layout_timer.borrow_mut().take();
                    dock.panel_layout();
                }
            });
            self.layout_timer.replace(Some(source));
        }
        self.wake(0.5);
    }

    pub(super) fn status_scroll(&self) -> gtk::ScrolledWindow {
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(if self.horizontal { gtk::PolicyType::Automatic } else { gtk::PolicyType::Never },
                          if self.horizontal { gtk::PolicyType::Never } else { gtk::PolicyType::Automatic });
        scroll.set_propagate_natural_width(true);
        scroll.set_propagate_natural_height(true);
        if self.horizontal {
            self.install_wheel(&scroll);
        }
        scroll
    }

    fn preferred_span(&self, widget: &impl IsA<gtk::Widget>) -> i32 { natural(widget, self.orientation) }

    fn child_span(&self, scroll: &gtk::ScrolledWindow) -> i32 { scroll.child().map(|c| self.preferred_span(&c)).unwrap_or(0) }

    #[allow(deprecated)]
    fn panel_layout(&self) {
        let Some(groups) = self.widgets.get() else { return };
        if self.destroyed.get() {
            return;
        }
        self.refresh_groups();
        let geometry = self.monitor.geometry();
        let mut span = (if self.horizontal { geometry.width() } else { geometry.height() }) - 2 * self.edge_gap();
        if self.is_app_dock && !self.horizontal {
            span -= self.top_bar_span();
        }
        // Reserve the actual menu/Home/Trash sizes, including CSS.
        let context = self.panel.style_context();
        let (padding, border) = (context.padding(), context.border());
        let mut fixed = if self.horizontal {
            (padding.left() + padding.right() + border.left() + border.right()) as i32
        } else {
            (padding.top() + padding.bottom() + border.top() + border.bottom()) as i32
        };
        if self.custom_group_zones().is_some() {
            self.custom_layout_limits((span - fixed).max(1));
            return;
        }
        if self.bar_mode {
            self.bar_layout((span - fixed).max(1));
            return;
        }
        let visible_children: Vec<gtk::Widget> = children(&self.panel).into_iter().filter(|c| c.is_visible()).collect();
        let spacing = self.panel.downcast_ref::<gtk::Box>().map(|b| b.spacing()).unwrap_or(3);
        fixed += spacing * (visible_children.len() as i32 - 1).max(0);
        let utility: Option<gtk::Widget> = self.utility_scroll().map(|s| s.upcast());
        let app_scroll: gtk::Widget = groups.app_scroll.clone().upcast();
        for child in &visible_children {
            if *child != app_scroll && Some(child) != utility.as_ref() {
                fixed += natural(child, self.orientation);
            }
        }
        let mut available = (span - fixed).max(1);
        let mut limits = Vec::new();
        if let (Some(indicators), Some(scroll)) = (self.indicators(), self.utility_scroll()) {
            if scroll.is_visible() {
                let request = natural(&indicators.widget(), self.orientation);
                let utility_max = (available - 60.min(available.div_euclid(2))).max(1);
                let utility_span = request.min(utility_max);
                limits.push((scroll, utility_max));
                available -= utility_span;
            }
        }
        limits.push((groups.app_scroll.clone(), available.max(1)));
        for (scroll, maximum) in limits {
            self.set_scroll_limit(&scroll, maximum);
        }
    }

    fn custom_layout_limits(&self, available: i32) {
        let groups = self.widgets.get().expect("groups");
        let (start, end) = (self.start_scroll().expect("start"), self.utility_scroll().expect("end"));
        if self.is_app_dock && self.bar_mode && !groups.app_scroll.is_visible() {
            // With no center group, either side can use the space left by the other.
            let start_request = if start.is_visible() { self.child_span(&start) } else { 0 };
            let end_request = if end.is_visible() { self.child_span(&end) } else { 0 };
            let available = (available - 6).max(1);
            let start_limit = start_request.min((available - end_request).max(available.div_euclid(2)));
            self.set_scroll_limit(&end, (available - start_limit).max(1));
            self.set_scroll_limit(&start, start_limit.max(1));
            self.set_scroll_limit(&groups.app_scroll, 1);
            return;
        }
        // Reserve symmetric side space to keep the center at the midpoint.
        let scrolls = [start, groups.app_scroll.clone(), end];
        let requests = [0, 1, 2].map(|i| if scrolls[i].is_visible() { self.child_span(&scrolls[i]) } else { 0 });
        let limits = custom_limits(available, requests);
        for (scroll, limit) in scrolls.iter().zip(limits) {
            self.set_scroll_limit(scroll, limit);
        }
    }

    fn bar_layout(&self, available: i32) {
        let groups = self.widgets.get().expect("groups");
        let (start, end) = (self.start_scroll(), self.utility_scroll());
        if self.config.application_display == "title" {
            // Equal limits keep the clock at the monitor midpoint.
            let clock_request = if groups.app_scroll.is_visible() { self.preferred_span(&groups.center_strip) } else { 0 };
            let clock_span = clock_request.min((available - 100).max(1));
            let side_limit = ((available - clock_span - 6).div_euclid(2)).max(1);
            for scroll in [&start, &end].into_iter().flatten() {
                self.set_scroll_limit(scroll, side_limit);
            }
            self.set_scroll_limit(&groups.app_scroll, clock_span.max(1));
            return;
        }
        let core = self.core();
        let visible_children: Vec<gtk::Widget> = children(&core).into_iter().filter(|c| c.is_visible()).collect();
        let app_scroll: gtk::Widget = groups.app_scroll.clone().upcast();
        let mut fixed = core.spacing() * (visible_children.len() as i32 - 1).max(0);
        fixed += visible_children.iter().filter(|c| **c != app_scroll).map(|c| self.preferred_span(c)).sum::<i32>();
        // Reserve equal space on each side of the application group.
        let zones: [Option<gtk::Widget>; 3] = [start.clone().map(|s| s.upcast()), Some(core.clone().upcast()), end.clone().map(|s| s.upcast())];
        let visible_zones = zones.iter().flatten().filter(|w| w.is_visible()).count() as i32;
        let spacing = 3 * (visible_zones - 1).max(0);
        let side_request = [&start, &end].into_iter().flatten().filter(|s| s.is_visible()).map(|s| self.child_span(s)).max().unwrap_or(0);
        let app_request = if groups.app_scroll.is_visible() { self.child_span(&groups.app_scroll) } else { 0 };
        let app_minimum = 60.min(app_request);
        let side_limit = ((available - fixed - app_minimum - spacing).div_euclid(2)).max(1);
        let side_span = side_request.min(side_limit);
        let app_limit = (available - fixed - 2 * side_span - spacing).max(1);
        self.set_scroll_limit(&groups.app_scroll, app_limit);
        let app_span = app_request.min(app_limit);
        // If fewer apps are open, offer the remaining space to both ends.
        let side_limit = ((available - fixed - app_span - spacing).div_euclid(2)).max(1);
        for scroll in [&start, &end].into_iter().flatten() {
            self.set_scroll_limit(scroll, side_limit);
        }
    }

    pub(super) fn set_scroll_limit(&self, scroll: &gtk::ScrolledWindow, maximum: i32) {
        if self.horizontal {
            if scroll.max_content_width() != maximum {
                scroll.set_max_content_width(maximum);
            }
        } else if scroll.max_content_height() != maximum {
            scroll.set_max_content_height(maximum);
        }
    }

    pub(super) fn separator(&self, container: &gtk::Box) -> gtk::Separator {
        let line = gtk::Separator::new(if self.horizontal { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        line.add_css_class("waydock-separator");
        pack(container, &line, false, false, 0);
        line
    }

    pub fn scroll_horizontal(&self, scroll: &gtk::ScrolledWindow, dx: f64, dy: f64) -> bool {
        let adjustment = scroll.hadjustment();
        match crate::ui::app_menu::logic::scroll_value(adjustment.lower(), adjustment.upper(), adjustment.page_size(),
                                                        adjustment.value(), dx, dy, self.config.icon_size as i32) {
            Some(value) => {
                adjustment.set_value(value);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_zones_match_python() {
        assert_eq!(legacy_group_zones(false, false).len(), 1);
        assert_eq!(legacy_group_zones(false, true)[1], vec!["app_menu", "pinned", "running", "files"]);
        assert_eq!(legacy_group_zones(true, true)[1], vec!["clock"]);
        assert_eq!(legacy_group_zones(true, false)[1], vec!["pinned", "running"]);
    }

    #[test]
    fn custom_limits_keep_center_symmetric() {
        // 1000 px, sides 200/100, center 300.
        assert_eq!(custom_limits(1000, [200, 300, 100]), [347, 594, 347]);
        // Tiny space never goes below 1.
        assert_eq!(custom_limits(3, [50, 50, 50]), [1, 1, 1]);
    }
}
