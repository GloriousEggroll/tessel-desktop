//! The application menu (dock_ui.py `toggle_app_menu` … `close_app_menu`):
//! search with ranked results, categories, the Frequent row, pin stars,
//! keyboard navigation, drag-to-pin, outside-click dismissal.
//!
//! # Public API
//! - [`AppMenu::new(host)`] → `Rc<AppMenu>`, owned by the dock surface; one
//!   per clone. `toggle()` (= `toggle_app_menu`), `close()` (=
//!   `close_app_menu`), `dismiss()` (= `dismiss_app_menu`, returns true),
//!   `is_open()`, `window()` (= `menu_window`), `backdrops()`
//!   (= `menu_backdrops`), `widgets()` (handles for tests/hosts),
//!   `dragging()`.
//! - [`desktop_id(widget)`]/[`category_id(row)`]: the Python `row.desktop_id`
//!   / `row.category_id` attributes.
//! - [`MenuHost`]: what the dock surface (agent E) must implement (on top of
//!   [`PopupHost`]); see its docs.
//! - [`launch_app`]: `Waydock.launch` (catalog launch through core's
//!   `Launcher`, errors to `show_error`, then the launch history) for the
//!   application object to call from `MenuHost::launch`.
//! - Submodules: [`logic`] (pure: `app_menu_size`, grid, drop ids, popup
//!   arithmetic), [`popup`] (`PopupHost`, `surface`, `icon_image`, pin CSS,
//!   `position_popup`, gtk helpers), [`drag`] (`drag_content`, button drag
//!   source, `DropHost` + drop target for the dock's pinned strip).
//!
//! Surfaces: `waydock-applications` (OVERLAY, keyboard EXCLUSIVE while open,
//! class `<popup_class>-applications`), one `waydock-menu-dismiss` per
//! monitor (OVERLAY, all edges, dismiss on release).

pub mod drag;
pub mod logic;
pub mod popup;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use crate::core::app_catalog::{AppCatalog, AppEntry, LaunchContext, LaunchHistory, Launcher};
use logic::{app_menu_size, available_width, count_text, grid_cells, grid_move, tile_tooltip, GridMove, MenuMetrics};
pub use logic::{DRAG_PREFIX, MENU_MAX_COLUMNS, MENU_TILE_WIDTH};
use popup::{accessible_name, children, keys, menu_icon_image, key_name, named_image, pack, position_popup, surface, toggle_class};
pub use popup::{install_app_menu_pin_style, PopupAnchor, PopupHost, APP_MENU_PIN_CSS};
use suite_core::tr;

/// Settings and System both resolve to a gear in common icon themes.
pub const MENU_CATEGORY_ICONS: &[(&str, &str)] = &[("System", "computer-symbolic")];

/// What the application menu needs from its dock surface and the app
/// (Python `self` / `self.app` attributes of the same names).
pub trait MenuHost: PopupHost {
    /// `self.app.config.pins` (stars show pinned state).
    fn pins(&self) -> Vec<String>;
    /// `self.app.launch_history` (`None` hides the Frequent row).
    fn launch_history(&self) -> Option<Rc<RefCell<LaunchHistory>>>;
    /// `self.app.launch(desktop_id, context)`; see [`launch_app`].
    fn launch(&self, desktop_id: &str, context: Rc<dyn LaunchContext>);
    /// `self.app.toggle_pin(desktop_id)`.
    fn toggle_pin(&self, desktop_id: &str);
    /// `self.app.show_settings()`.
    fn show_settings(&self);
    /// Start of `toggle_app_menu`: every *other* dock clone closes its menu,
    /// the tray expander and the applications expander collapse.
    fn before_toggle(&self);
    /// `self.close_preview()`.
    fn close_preview(&self);
    /// Geometry for `app_menu_size`.
    fn menu_metrics(&self) -> MenuMetrics;
    /// `dock.enter(zone)` on every clone (`app.docks`, else self) — drags.
    fn enter_all(&self, zone: &str) {
        self.enter(zone)
    }
    fn leave_all(&self, zone: &str) {
        self.leave(zone)
    }
}

/// `Waydock.launch(desktop_id, context)`: launch through the catalog; errors
/// (immediate or reported by the service) go to `show_error`; a started
/// launch is recorded in the history.
pub fn launch_app(
    catalog: &AppCatalog,
    launcher: &Launcher,
    history: Option<&RefCell<LaunchHistory>>,
    desktop_id: &str,
    context: Option<Rc<dyn LaunchContext>>,
    show_error: Rc<dyn Fn(&str)>,
) {
    let report = show_error.clone();
    let done: crate::core::app_catalog::DoneFn = Box::new(move |error| {
        if let Some(error) = error {
            report(&error);
        }
    });
    if let Err(error) = catalog.launch(launcher, desktop_id, context, Some(done)) {
        show_error(&error.to_string());
        return;
    }
    if let Some(history) = history {
        history.borrow_mut().record(desktop_id);
    }
}

const DESKTOP_ID: &str = "waydock-desktop-id";
const CATEGORY_ID: &str = "waydock-category-id";

/// `row.desktop_id` (FlowBox tiles and Frequent buttons; padding cells: None).
pub fn desktop_id(widget: &impl IsA<glib::Object>) -> Option<String> {
    // SAFETY: the key only ever stores a String.
    unsafe { widget.data::<String>(DESKTOP_ID).map(|p| p.as_ref().clone()) }
}

fn set_desktop_id(widget: &impl IsA<glib::Object>, id: &str) {
    unsafe { widget.set_data(DESKTOP_ID, id.to_owned()) }
}

/// `row.category_id` of a category row: `Some(None)` = All Applications.
pub fn category_id(row: &impl IsA<glib::Object>) -> Option<Option<String>> {
    // SAFETY: the key only ever stores an Option<String>.
    unsafe { row.data::<Option<String>>(CATEGORY_ID).map(|p| p.as_ref().clone()) }
}

/// Widget handles of an open menu.
#[derive(Clone)]
pub struct MenuWidgets {
    pub popup: gtk::Window,
    pub content: gtk::Box,
    pub header: gtk::Box,
    pub count: gtk::Label,
    pub settings: gtk::Button,
    pub search: gtk::SearchEntry,
    pub categories: gtk::ListBox,
    pub section: gtk::Label,
    pub listing: gtk::FlowBox,
    pub scroll: gtk::ScrolledWindow,
    pub results: gtk::Stack,
    pub frequent: gtk::Box,
    pub frequent_list: gtk::Box,
    pub columns: i32,
}

struct MenuState {
    widgets: MenuWidgets,
    menu: Weak<AppMenu>,
    category_rows: Vec<(Option<String>, gtk::ListBoxRow, String)>,
    search_at_bottom: bool,
    selected_category: RefCell<Option<String>>,
    first_result: RefCell<Option<gtk::FlowBoxChild>>,
    last_query: RefCell<Option<String>>,
}

/// The application menu of one dock clone.
pub struct AppMenu {
    host: Weak<dyn MenuHost>,
    state: RefCell<Option<Rc<MenuState>>>,
    backdrops: RefCell<Vec<gtk::Window>>,
    dragging: Cell<bool>,
    this: Weak<AppMenu>,
}

impl AppMenu {
    pub fn new(host: Weak<dyn MenuHost>) -> Rc<Self> {
        Rc::new_cyclic(|this| AppMenu {
            host,
            state: RefCell::new(None),
            backdrops: RefCell::new(Vec::new()),
            dragging: Cell::new(false),
            this: this.clone(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.state.borrow().is_some()
    }

    /// `self.menu_window`.
    pub fn window(&self) -> Option<gtk::Window> {
        self.state.borrow().as_ref().map(|s| s.widgets.popup.clone())
    }

    pub fn widgets(&self) -> Option<MenuWidgets> {
        self.state.borrow().as_ref().map(|s| s.widgets.clone())
    }

    /// `self.menu_backdrops`.
    pub fn backdrops(&self) -> Vec<gtk::Window> {
        self.backdrops.borrow().clone()
    }

    /// `self._menu_dragging`.
    pub fn dragging(&self) -> bool {
        self.dragging.get()
    }

    /// `dismiss_app_menu`.
    pub fn dismiss(&self) -> bool {
        self.close();
        true
    }

    /// `close_app_menu`.
    pub fn close(&self) {
        let state = self.state.borrow_mut().take();
        let backdrops = std::mem::take(&mut *self.backdrops.borrow_mut());
        if let Some(state) = &state {
            popup::close_blur(&state.widgets.popup);
            state.widgets.popup.destroy();
        }
        for backdrop in backdrops {
            backdrop.destroy();
        }
        if state.is_some() {
            if let Some(host) = self.host.upgrade() {
                host.leave("menu");
            }
            // The menu's tiles, listing and render caches are freed now.
            suite_gtk::utils::trim_heap_after(std::time::Duration::from_millis(1500));
        }
    }

    fn dismiss_unfocused(&self, popup: &gtk::Window) {
        if self.dragging.get() {
            return;
        }
        if self.window().as_ref() == Some(popup) && !popup.is_active() {
            self.close();
        }
    }

    /// `toggle_app_menu`.
    pub fn toggle(&self) {
        let Some(host) = self.host.upgrade() else { return };
        host.before_toggle();
        if self.is_open() {
            self.close();
            return;
        }
        host.close_preview();
        host.enter("menu");
        let application = host.application();
        let monitor = host.monitor();
        let popup = surface(application.as_ref(), &monitor, "waydock-applications", Layer::Overlay, true);
        popup.add_css_class(&format!("{}-applications", host.popup_class()));
        install_app_menu_pin_style(Some(&WidgetExt::display(&popup)));
        // Layer surfaces cannot observe clicks outside their own input region.
        // Temporary transparent surfaces catch dismissal clicks on every output.
        // Map them first so the actual menu remains above them.
        let display = monitor.display();
        for each in popup::monitors(&display) {
            let backdrop = surface(application.as_ref(), &each, "waydock-menu-dismiss", Layer::Overlay, false);
            backdrop.set_resizable(true);
            backdrop.set_child(Some(&popup::transparent_area()));
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                backdrop.set_anchor(edge, true);
            }
            // Close on release so the rest of the click cannot reach a window
            // underneath the dismissed surface.
            let gesture = gtk::GestureClick::new();
            gesture.set_button(0);
            let menu = self.this.clone();
            gesture.connect_released(move |gesture, _n, _x, _y| {
                if let Some(menu) = menu.upgrade() {
                    if menu.dismiss() {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                }
            });
            backdrop.add_controller(gesture);
            self.backdrops.borrow_mut().push(backdrop.clone());
            backdrop.set_visible(true);
        }
        // ON_DEMAND waits for a click inside the menu before giving it keys.
        // This focus exists only for the lifetime of the open menu.
        popup.set_keyboard_mode(KeyboardMode::Exclusive);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.add_css_class("waydock-popup");
        content.add_css_class("waydock-app-menu");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.add_css_class("waydock-app-menu-header");
        let title = gtk::Label::builder().label(tr!("Applications")).xalign(0.0).build();
        title.add_css_class("waydock-title");
        pack(&header, &title, true);
        let count = gtk::Label::new(None);
        count.add_css_class("waydock-subtitle");
        pack(&header, &count, false);
        let settings = gtk::Button::from_icon_name("emblem-system-symbolic");
        settings.add_css_class("waydock-app-menu-settings");
        settings.set_tooltip_text(Some(&tr!("Dock settings")));
        accessible_name(&settings, &tr!("Dock settings"));
        let weak_host = self.host.clone();
        settings.connect_clicked(move |_| {
            if let Some(host) = weak_host.upgrade() {
                host.show_settings();
            }
        });
        header.append(&settings);
        pack(&content, &header, false);
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search all applications")).build();
        let search_at_bottom = host.edge() == "bottom";
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        // Leave space for the dock, header and search on smaller screens.
        // Keep long names ellipsized inside this width instead of enlarging it.
        let metrics = host.menu_metrics();
        let available = available_width(&metrics);
        #[allow(deprecated)]
        let chrome = {
            let context = content.style_context();
            let (padding, border) = (context.padding(), context.border());
            (padding.left() + padding.right() + border.left() + border.right()) as i32
        };
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let (body_width, body_height, columns) = app_menu_size(&metrics, available, chrome);
        body.set_size_request(body_width, body_height);
        let category_scroll = gtk::ScrolledWindow::new();
        category_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        category_scroll.set_size_request(180, -1);
        let categories = gtk::ListBox::new();
        categories.add_css_class("waydock-app-categories");
        categories.set_selection_mode(gtk::SelectionMode::Single);
        categories.set_activate_on_single_click(true);
        accessible_name(&categories, &tr!("Application categories"));
        let mut entries: Vec<(Option<String>, String, String)> = vec![(None, tr!("All Applications"), "view-app-grid-symbolic".into())];
        let catalog = host.catalog();
        entries.extend(catalog.borrow().menu_categories().into_iter().map(|category| {
            let icon = MENU_CATEGORY_ICONS.iter().find(|(id, _)| *id == category.id).map(|(_, icon)| *icon).unwrap_or(category.icon);
            (Some(category.id.to_owned()), suite_core::i18n::translate(category.name), icon.to_owned())
        }));
        let mut category_rows = Vec::new();
        for (id, name, icon) in entries {
            let row = gtk::ListBoxRow::new();
            unsafe { row.set_data(CATEGORY_ID, id.clone()) };
            row.set_tooltip_text(Some(&name));
            accessible_name(&row, &name);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            line.append(&named_image(&icon, 18));
            let label = gtk::Label::builder().label(&name).xalign(0.0).hexpand(true).build();
            label.set_ellipsize(pango::EllipsizeMode::End);
            line.append(&label);
            row.set_child(Some(&line));
            categories.append(&row);
            category_rows.push((id, row, name));
        }
        category_scroll.set_child(Some(&categories));
        body.append(&category_scroll);
        body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let browser = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).hexpand(true).build();
        let section = gtk::Label::builder().label(tr!("All Applications")).xalign(0.0).build();
        section.add_css_class("waydock-app-section");
        browser.append(&section);
        body.append(&browser);
        let listing = gtk::FlowBox::new();
        listing.set_homogeneous(true);
        listing.set_min_children_per_line(columns as u32);
        listing.set_max_children_per_line(columns as u32);
        listing.set_column_spacing(8);
        listing.set_row_spacing(4);
        listing.set_selection_mode(gtk::SelectionMode::Single);
        listing.set_activate_on_single_click(true);
        // Results always read top to bottom from A, so the menu opens on the
        // first applications even before any scroll adjustment is applied.
        listing.set_valign(gtk::Align::Start);
        scroll.set_child(Some(&listing));
        let results = gtk::Stack::new();
        results.set_vexpand(true);
        results.add_named(&scroll, Some("applications"));
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 10);
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        pack(&empty, &named_image("system-search-symbolic", 32), false);
        pack(&empty, &gtk::Label::new(Some(&tr!("No applications found"))), false);
        let hint = gtk::Label::new(Some(&tr!("Try a different search.")));
        hint.add_css_class("waydock-subtitle");
        pack(&empty, &hint, false);
        results.add_named(&empty, Some("empty"));
        // Most-launched applications, shown only while browsing everything.
        let frequent = gtk::Box::new(gtk::Orientation::Vertical, 4);
        frequent.add_css_class("waydock-app-frequent");
        let frequent_title = gtk::Label::builder().label(tr!("Frequent")).xalign(0.0).build();
        frequent_title.add_css_class("waydock-app-section");
        // A single row of buttons: Tab reaches each, Enter/click launches.
        let frequent_list = gtk::Box::builder().spacing(8).homogeneous(true).build();
        frequent_list.add_css_class("waydock-app-frequent-list");
        accessible_name(&frequent_list, &tr!("Frequent"));
        frequent.append(&frequent_title);
        frequent.append(&frequent_list);
        frequent.set_visible(false);
        if !search_at_bottom {
            pack(&content, &search, false);
        }
        browser.append(&frequent);
        browser.append(&results);
        pack(&content, &body, true);
        if search_at_bottom {
            pack(&content, &search, false);
        }
        listing.set_vadjustment(&scroll.vadjustment());

        let state = Rc::new(MenuState {
            widgets: MenuWidgets {
                popup: popup.clone(), content: content.clone(), header, count, settings, search: search.clone(),
                categories: categories.clone(), section, listing: listing.clone(), scroll, results, frequent, frequent_list,
                columns,
            },
            menu: self.this.clone(),
            category_rows,
            search_at_bottom,
            selected_category: RefCell::new(None),
            first_result: RefCell::new(None),
            last_query: RefCell::new(None),
        });
        *self.state.borrow_mut() = Some(state.clone());
        host.set_popup_blur(&popup, true);

        let weak = Rc::downgrade(&state);
        let on_category = move |row: Option<&gtk::ListBoxRow>| {
            if let (Some(state), Some(row)) = (weak.upgrade(), row) {
                state.select_category(row);
            }
        };
        let selected = on_category.clone();
        categories.connect_row_selected(move |_list, row| selected(row));
        categories.connect_row_activated(move |_list, row| on_category(Some(row)));
        let weak = Rc::downgrade(&state);
        listing.connect_child_activated(move |_list, child| {
            if let Some(state) = weak.upgrade() {
                state.launch(Some(child.upcast_ref()));
            }
        });
        // Clearing a query while choosing a category updates immediately.
        // Ignore SearchEntry's later notification for that same text so it
        // cannot replace an app tile while the user is clicking it.
        let weak = Rc::downgrade(&state);
        search.connect_search_changed(move |search| {
            if let Some(state) = weak.upgrade() {
                let text = suite_core::pyjson::py_strip(&search.text()).to_owned();
                if state.last_query.borrow().as_deref() != Some(text.as_str()) {
                    state.populate();
                }
            }
        });
        let weak = Rc::downgrade(&state);
        search.connect_activate(move |_| {
            if let Some(state) = weak.upgrade() {
                let row = state.selected_result();
                state.launch(row.as_ref().map(|r| r.upcast_ref()));
            }
        });
        let weak = Rc::downgrade(&state);
        keys(&search, move |_widget, key| weak.upgrade().is_some_and(|state| state.search_key(key)));
        let menu = self.this.clone();
        search.connect_stop_search(move |_| {
            if let Some(menu) = menu.upgrade() {
                menu.dismiss();
            }
        });
        popup.set_child(Some(&content));
        let menu = self.this.clone();
        keys(&popup, move |_widget, key| {
            key == gdk::Key::Escape && menu.upgrade().is_some_and(|menu| menu.dismiss())
        });
        let menu = self.this.clone();
        popup.connect_is_active_notify(move |popup| {
            let (menu, popup) = (menu.clone(), popup.clone());
            glib::idle_add_local_once(move || {
                if let Some(menu) = menu.upgrade() {
                    menu.dismiss_unfocused(&popup);
                }
            });
        });
        let menu = self.this.clone();
        popup.connect_close_request(move |_| {
            if let Some(menu) = menu.upgrade() {
                menu.dismiss();
            }
            glib::Propagation::Stop
        });
        state.populate();
        content.set_visible(true);
        position_popup(&*host, &popup, &PopupAnchor::Menu);
        popup.set_visible(true);
        search.grab_focus();
    }

    /// `pin_from_menu(desktop_id, refresh)`.
    fn pin_from_menu(&self, desktop_id: &str, state: &Rc<MenuState>) {
        if let Some(host) = self.host.upgrade() {
            host.toggle_pin(desktop_id);
        }
        state.populate();
    }

    /// `_menu_drag_source(row, desktop_id)`: menu tiles can be dragged onto the dock.
    fn menu_drag_source(&self, widget: &impl IsA<gtk::Widget>, desktop_id: &str) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::COPY);
        let id = desktop_id.to_owned();
        source.connect_prepare(move |_s, _x, _y| Some(drag::drag_content(&id)));
        let menu = self.this.clone();
        source.connect_drag_begin(move |_s, _d| {
            if let Some(menu) = menu.upgrade() {
                menu.drag_began();
            }
        });
        let menu = self.this.clone();
        source.connect_drag_end(move |_s, _d, _delete| {
            if let Some(menu) = menu.upgrade() {
                menu.drag_ended();
            }
        });
        widget.add_controller(source);
    }

    /// `_menu_drag_began`: let the dock underneath receive the drop (the
    /// dismissal backdrops sit above every dock surface while the menu is open).
    pub fn drag_began(&self) {
        self.dragging.set(true);
        for backdrop in self.backdrops.borrow().iter() {
            backdrop.set_visible(false);
        }
        if let Some(host) = self.host.upgrade() {
            host.enter_all("drag");
        }
    }

    /// `_menu_drag_ended`.
    pub fn drag_ended(&self) {
        self.dragging.set(false);
        if let Some(host) = self.host.upgrade() {
            host.leave_all("drag");
        }
        let menu = self.this.clone();
        glib::idle_add_local_once(move || {
            if let Some(menu) = menu.upgrade() {
                if menu.host.upgrade().is_some_and(|h| !h.destroyed()) {
                    menu.close();
                }
            }
        });
    }
}

impl MenuState {
    fn host(&self) -> Option<(Rc<AppMenu>, Rc<dyn MenuHost>)> {
        let menu = self.menu.upgrade()?;
        let host = menu.host.upgrade()?;
        Some((menu, host))
    }

    fn is_current(&self, menu: &AppMenu) -> bool {
        menu.window().as_ref() == Some(&self.widgets.popup)
    }

    fn selected_result(&self) -> Option<gtk::FlowBoxChild> {
        self.widgets.listing.selected_children().into_iter().next().or_else(|| self.first_result.borrow().clone())
    }

    fn select_category(self: &Rc<Self>, row: &gtk::ListBoxRow) {
        let Some(id) = category_id(row) else { return };
        let searching = !suite_core::pyjson::py_strip(&self.widgets.search.text()).is_empty();
        if id == *self.selected_category.borrow() && !searching {
            return;
        }
        *self.selected_category.borrow_mut() = id;
        self.widgets.search.set_text("");
        self.populate();
    }

    /// `launch(_list, row)`.
    fn launch(&self, row: Option<&gtk::Widget>) {
        let Some(ident) = row.and_then(desktop_id) else { return };
        let Some((menu, host)) = self.host() else { return };
        let context: Rc<dyn LaunchContext> = Rc::new(popup::launch_context(&WidgetExt::display(&self.widgets.popup)));
        host.launch(&ident, context);
        menu.close();
    }

    /// `search_key`: toward the results selects the current (or first) result.
    fn search_key(&self, key: gdk::Key) -> bool {
        let name = key_name(key);
        let toward_results: &[&str] = if self.search_at_bottom { &["Up", "KP_Up"] } else { &["Down", "KP_Down"] };
        if toward_results.contains(&name.as_str()) && self.first_result.borrow().is_some() {
            if let Some(row) = self.selected_result() {
                self.widgets.listing.select_child(&row);
                row.grab_focus();
            }
            return true;
        }
        false
    }

    /// `grid_key(row, event)`: arrows move between applications; Tab still
    /// reaches each tile's pin button without turning Enter on that button
    /// into a launch.
    fn grid_key(&self, row: &gtk::Widget, key: gdk::Key) -> bool {
        if GtkWindowExt::focus(&self.widgets.popup).as_ref() != Some(row) {
            return false;
        }
        let name = key_name(key);
        if matches!(name.as_str(), "Return" | "KP_Enter" | "space") {
            self.launch(Some(row));
            return true;
        }
        let Some(direction) = logic::arrow_direction(&name) else { return false };
        let cells = children(&self.widgets.listing);
        let has_app: Vec<bool> = cells.iter().map(|c| desktop_id(c).is_some()).collect();
        let index = row.downcast_ref::<gtk::FlowBoxChild>().map(|c| c.index()).unwrap_or(0).max(0) as usize;
        match grid_move(index, self.widgets.columns as usize, &has_app, direction, self.search_at_bottom) {
            GridMove::Search => {
                self.widgets.search.grab_focus();
            }
            GridMove::Select(target) => {
                if let Some(child) = cells[target].downcast_ref::<gtk::FlowBoxChild>() {
                    self.widgets.listing.select_child(child);
                    child.grab_focus();
                }
            }
            GridMove::Stay => {}
        }
        true
    }

    fn entry_content(entry: &AppEntry, monitor: &gdk::Monitor) -> gtk::Box {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        pack(&line, &menu_icon_image(entry.icon.as_ref(), 28, monitor), false);
        let label = gtk::Label::builder().label(&entry.name).xalign(0.0).build();
        label.add_css_class("waydock-app-name");
        label.set_single_line_mode(true);
        label.set_ellipsize(pango::EllipsizeMode::End);
        pack(&line, &label, true);
        line
    }

    /// `populate()`.
    fn populate(self: &Rc<Self>) {
        let Some((menu, host)) = self.host() else { return };
        let w = &self.widgets;
        *self.first_result.borrow_mut() = None;
        for child in children(&w.listing) {
            w.listing.remove(&child);
        }
        let query = suite_core::pyjson::py_strip(&w.search.text()).to_owned();
        *self.last_query.borrow_mut() = Some(query.clone());
        let catalog = host.catalog();
        let selected = self.selected_category.borrow().clone();
        let entries = if !query.is_empty() {
            let entries = catalog.borrow().search(&query);
            w.categories.unselect_all();
            w.section.set_text(&tr!("Search results"));
            entries
        } else {
            let entries = catalog.borrow().apps_in_category(selected.as_deref());
            if let Some((_, row, name)) = self.category_rows.iter().find(|(id, _, _)| *id == selected) {
                w.categories.select_row(Some(row));
                w.section.set_text(name);
            }
            entries
        };
        w.count.set_text(&count_text(entries.len()));
        for child in children(&w.frequent_list) {
            w.frequent_list.remove(&child);
        }
        let columns = w.columns as usize;
        let monitor = host.monitor();
        let common = match host.launch_history() {
            Some(history) if query.is_empty() && selected.is_none() => history.borrow().frequent(&catalog.borrow(), columns),
            _ => Vec::new(),
        };
        for entry in &common {
            let tile = gtk::Button::new();
            tile.add_css_class("waydock-app-frequent-tile");
            set_desktop_id(&tile, &entry.id);
            tile.set_tooltip_text(Some(&entry.name));
            accessible_name(&tile, &entry.name);
            tile.set_child(Some(&Self::entry_content(entry, &monitor)));
            menu.menu_drag_source(&tile, &entry.id);
            let weak = Rc::downgrade(self);
            tile.connect_clicked(move |button| {
                if let Some(state) = weak.upgrade() {
                    state.launch(Some(button.upcast_ref()));
                }
            });
            w.frequent_list.append(&tile);
        }
        // Keep tiles the width of one grid column.
        let target = if common.is_empty() { 0 } else { columns };
        for _ in common.len()..target {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_can_target(false);
            w.frequent_list.append(&spacer);
        }
        w.frequent.set_visible(!common.is_empty());
        let pins = host.pins();
        for (index, cell) in grid_cells(&entries, columns).into_iter().enumerate() {
            let row = gtk::FlowBoxChild::new();
            let Some(entry) = cell else {
                row.set_sensitive(false);
                row.set_focusable(false);
                row.set_can_focus(false);
                row.add_css_class("waydock-empty-cell");
                w.listing.append(&row);
                continue;
            };
            set_desktop_id(&row, &entry.id);
            let weak = Rc::downgrade(self);
            keys(&row, move |row, key| weak.upgrade().is_some_and(|state| state.grid_key(row, key)));
            if index == 0 {
                *self.first_result.borrow_mut() = Some(row.clone());
            }
            row.set_tooltip_text(Some(&tile_tooltip(&entry)));
            let line = Self::entry_content(&entry, &monitor);
            let pinned = pins.contains(&entry.id);
            let pin = gtk::Button::from_icon_name(if pinned { "starred-symbolic" } else { "non-starred-symbolic" });
            pin.add_css_class("waydock-app-pin");
            toggle_class(&pin, "pinned", pinned);
            let action = if pinned { tr!("Unpin application") } else { tr!("Pin application") };
            pin.set_tooltip_text(Some(&action));
            accessible_name(&pin, &format!("{action}: {}", entry.name));
            let (weak, key) = (Rc::downgrade(self), entry.id.clone());
            pin.connect_clicked(move |_| {
                if let Some(state) = weak.upgrade() {
                    if let Some(menu) = state.menu.upgrade() {
                        menu.pin_from_menu(&key, &state);
                    }
                }
            });
            line.append(&pin);
            row.set_child(Some(&line));
            menu.menu_drag_source(&row, &entry.id);
            w.listing.append(&row);
        }
        w.listing.unselect_all();
        w.listing.set_visible(true);
        w.results.set_visible_child_name(if entries.is_empty() { "empty" } else { "applications" });
        let (weak, menu) = (Rc::downgrade(self), Rc::downgrade(&menu));
        glib::idle_add_local_once(move || {
            if let (Some(state), Some(menu)) = (weak.upgrade(), menu.upgrade()) {
                if state.is_current(&menu) {
                    let adjustment = state.widgets.scroll.vadjustment();
                    adjustment.set_value(adjustment.lower());
                }
            }
        });
    }
}

/// Port of test_app_popup_style.py: application-menu/preview transparency
/// stays independent of panel styling (CSS from `appearance::style`).
#[cfg(test)]
mod popup_style_tests {
    use crate::appearance::style::{background_css, popup_css, BackgroundRequest};
    use suite_gtk::palette::color_palette;

    fn popup(dark: bool, wallpaper: Option<[u8; 3]>, transparency: i64, frosted: bool, rounded: bool, strength: i64) -> String {
        popup_css("test", dark, wallpaper, transparency, frosted, rounded, strength, None).unwrap()
    }

    #[test]
    fn background_alpha_changes_without_fading_content_or_styling_panel() {
        for dark in [false, true] {
            let colors = color_palette(dark, Some([80, 130, 200]));
            let join = |c: [u8; 3]| c.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
            let (background, foreground) = (join(colors.background), join(colors.foreground));
            // Light surfaces stay at least 85% opaque over dark wallpapers.
            for (amount, alpha) in [(0, "1.000"), (50, if dark { "0.500" } else { "0.850" }), (100, if dark { "0.000" } else { "0.850" })] {
                let css = popup(dark, Some([80, 130, 200]), amount, false, true, 50);
                assert!(css.contains(&format!("background-color: rgba({background}, {alpha});")), "{dark} {amount}");
                assert!(css.contains(&format!("color: rgb({foreground});")));
                assert!(css.contains("popover.test.waydock-context-menu { background-image: none; background-color: transparent;"));
                assert!(!css.contains("opacity:"));
                assert!(!css.contains(".waydock-panel"));
            }
            assert_eq!(popup(dark, None, 0, false, true, 50), popup_css("test", dark, None, 0, false, true, 50, None).unwrap());
        }
    }

    #[test]
    fn frosted_popup_and_corners_are_independent_of_dock() {
        for dark in [false, true] {
            for rounded in [false, true] {
                let css = popup(dark, None, 0, true, rounded, 100);
                assert!(!css.contains("url("));
                assert!(css.contains("background-color: transparent;"));
                assert!(css.contains("1.000)"));
                assert!(css.contains(&format!("border-radius: {}px;", if rounded { 12 } else { 0 })));
                assert!(!css.contains("opacity:"));
                assert!(!css.contains(".waydock-panel"));
                assert!(css.contains("popover.test.waydock-context-menu { background-image: none; background-color: transparent;"));
            }
        }
    }

    fn panel(request: BackgroundRequest) -> String {
        background_css(true, &request).unwrap()
    }

    #[test]
    fn edge_squared_bar_keeps_requested_workspace_corners() {
        let css = panel(BackgroundRequest { rounded_corners: false, inner_rounded_corners: Some(true), ..Default::default() });
        assert!(css.contains("border-radius: 0px;"));
        assert!(css.contains("button.waydock-workspace { border-radius: 8px; }"));
        let css = panel(BackgroundRequest { rounded_corners: true, inner_rounded_corners: Some(false), ..Default::default() });
        assert!(css.contains("button.waydock-workspace { border-radius: 0px; }"));
        assert!(panel(BackgroundRequest::default()).contains("button.waydock-workspace { border-radius: 8px; }"));
    }

    #[test]
    fn dock_shine_is_a_top_highlight_beside_bar_borders() {
        assert!(panel(BackgroundRequest::default()).contains("box-shadow: none;"));
        let shine = panel(BackgroundRequest { shine: true, ..Default::default() });
        assert!(shine.contains("inset 0 1px 0 0 rgba(255, 255, 255, 0.16)"));
        let both = panel(BackgroundRequest { shine: true, border_sides: vec!["bottom".into()], ..Default::default() });
        assert!(both.contains("rgba(255, 255, 255, 0.16), inset 0px -1px"));
    }

    #[test]
    fn frosted_panel_styles_only_the_panel() {
        for dark in [false, true] {
            for rounded in [false, true] {
                let css = background_css(dark, &BackgroundRequest { transparency: 50, frosted: true, rounded_corners: rounded, ..Default::default() }).unwrap();
                assert!(!css.contains("cross-fade("));
                assert!(css.contains(if dark { "0.500)" } else { "0.850)" }));
                assert!(!css.contains("background-size:"));
                assert!(!css.contains("url("));
                assert!(css.contains(&format!("border-radius: {}px;", if rounded { 12 } else { 0 })));
                assert!(!css.contains("opacity:"));
                assert!(!css.contains("window"));
                assert!(!css.contains(".waydock-popup"));
                assert!(!css.contains("popover"));
            }
        }
        let missing = panel(BackgroundRequest { transparency: 100, frosted: true, ..Default::default() });
        assert!(missing.contains("background-color: rgba(0,0,0,0.000);"));
    }

    #[test]
    fn invalid_percentages_are_rejected() {
        for invalid in [-1, 101] {
            assert!(popup_css("test", true, None, invalid, false, true, 50, None).is_err());
            assert!(popup_css("test", true, None, 0, false, true, invalid, None).is_err());
            assert!(background_css(true, &BackgroundRequest { frost_strength: invalid, ..Default::default() }).is_err());
        }
    }
}
