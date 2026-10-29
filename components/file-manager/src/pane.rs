//! files_pane.py: virtualized GTK4 folder views with asynchronous browsing,
//! search, live directory monitoring, Git labels, folder peek and drops.

use crate::bridge::{self, Prefs};
use crate::columns::MillerColumns;
use crate::completion::LocationCompletion;
use crate::keys::latin_keyval;
use crate::thumbnails::Thumbnails;
use crate::tile::{self, FileTile};
use files_model::model::{self as fm, DirectoryLoader, FileItem, SearchLoader};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::Instant;
use suite_core::tr;

/// Files and folders share one icon size in the list views.
pub const LIST_ICON_SIZE: i32 = 16;
pub const PEEK_DELAY: u32 = 700;
pub const PEEK_LIMIT: usize = 40;
pub const TYPE_AHEAD_SECONDS: f64 = 1.2;

/// What a pane needs from its window (Python reaches `self.window.*`).
pub trait PaneHost {
    fn message(&self, text: &str);
    fn selection_changed(&self, pane: &BrowserPane);
    fn pane_changed(&self, pane: &BrowserPane);
    fn focus_pane(&self, pane: &BrowserPane);
    fn context_menu(&self, pane: &BrowserPane, widget: &gtk::Widget, x: f64, y: f64);
    /// `run_operation`: true when a job started (Python returns the entry).
    fn run_operation(&self, kind: &str, files: Vec<gio::File>, destination: Option<gio::File>) -> bool;
    fn new_tab(&self, location: &gio::File);
    fn open_files(&self, files: Vec<gio::File>);
    fn mount_location(&self, location: &gio::File, done: Box<dyn Fn()>);
    /// `window.quick_look(pane)`; false when the window has none (media picker).
    fn quick_look(&self, pane: &BrowserPane) -> bool;
    fn bind_location_menu(&self, button: &gtk::Button, location: &gio::File, pane: &BrowserPane);
    fn action(&self, name: &str);
    fn mount_operation(&self) -> Option<gtk::MountOperation>;
}

/// `tool(icon, title, callback)`.
pub fn tool(icon: impl IsA<gtk::Widget>, title: &str, callback: impl Fn() + 'static) -> gtk::Button {
    let button = gtk::Button::builder().child(&icon).tooltip_text(title).build();
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    button.connect_clicked(move |_| callback());
    button
}

pub fn tool_icon(icon: &str, title: &str, callback: impl Fn() + 'static) -> gtk::Button {
    tool(suite_gtk::utils::image(icon, 16), title, callback)
}

/// A slim, palette-colored chevron independent of icon-theme arrow shapes.
pub fn chevron(direction: &str, size: i32) -> gtk::DrawingArea {
    let points: [(f64, f64); 3] = match direction {
        "left" => [(10., 4.), (6., 8.), (10., 12.)],
        "up" => [(4., 10.), (8., 6.), (12., 10.)],
        _ => [(6., 4.), (10., 8.), (6., 12.)],
    };
    let glyph = gtk::DrawingArea::builder().content_width(size).content_height(size)
        .halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    glyph.set_draw_func(move |widget, context, width, height| {
        let color = widget.color();
        context.translate(width as f64 / 2., height as f64 / 2.);
        context.scale(size as f64 / 16., size as f64 / 16.);
        context.translate(-8., -8.);
        context.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, color.alpha() as f64);
        context.set_line_width(1.7);
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.set_line_join(gtk::cairo::LineJoin::Round);
        context.move_to(points[0].0, points[0].1);
        for point in &points[1..] {
            context.line_to(point.0, point.1);
        }
        let _ = context.stroke();
    });
    glyph
}

/// Grid cell width: room for about 14 characters so names such as
/// "preview.png" stay on one line instead of breaking at the dot.
pub fn tile_width(icon_size: i64) -> i32 {
    132.max(icon_size as i32 + 56)
}

/// `as_file(value)`: `Gio.File.new_for_commandline_arg(os.path.expanduser(str(value)))`.
pub fn as_file(value: &str) -> gio::File {
    gio::File::for_commandline_arg(expanduser(value))
}

pub fn expanduser(value: &str) -> String {
    if value == "~" {
        return suite_core::paths::home().to_string_lossy().into_owned();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return suite_core::paths::home().join(rest).to_string_lossy().into_owned();
    }
    value.to_string()
}

pub fn home_file() -> gio::File {
    gio::File::for_path(suite_core::paths::home())
}

/// `configure_file_drop`: only advertise MOVE when this process controls both
/// endpoints. An external source must never receive permission to delete its
/// originals; a move chosen for an external drop is performed by our own job.
pub fn drop_actions(internal: bool) -> gdk::DragAction {
    let mut actions = gdk::DragAction::COPY;
    if internal {
        actions |= gdk::DragAction::MOVE | gdk::DragAction::LINK;
    }
    actions
}

pub fn drop_acceptable(offered: gdk::DragAction, actions: gdk::DragAction, has_file_list: bool, busy: bool) -> bool {
    !busy && offered.intersects(actions) && has_file_list
}

pub fn configure_file_drop(target: &gtk::DropTarget, drop: &gdk::Drop, busy: bool) -> bool {
    let actions = drop_actions(drop.drag().is_some());
    target.set_actions(actions);
    let formats = drop.formats().union_deserialize_types();
    drop_acceptable(drop.actions(), actions, formats.contains_type(gdk::FileList::static_type()), busy)
}

/// `external_drop_choice`: 'copy', 'move', 'link' or 'ask' for a drop from another app.
pub fn external_drop_choice(offered: gdk::DragAction, modifiers: gdk::ModifierType) -> &'static str {
    let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
    if shift && control {
        return "link";
    }
    if !offered.contains(gdk::DragAction::MOVE) {
        return "copy";
    }
    if shift {
        return "move";
    }
    if control {
        return "copy";
    }
    "ask"
}

/// The kind for a drop made inside Files: the drag's selected action.
pub fn internal_drop_kind(action: gdk::DragAction) -> &'static str {
    if action == gdk::DragAction::MOVE {
        "move"
    } else if action == gdk::DragAction::LINK {
        "link"
    } else {
        "copy"
    }
}

pub fn drop_modifiers(target: &gtk::DropTarget) -> gdk::ModifierType {
    let mut state = target.current_event_state();
    if let Some(display) = gdk::Display::default() {
        if let Some(keyboard) = display.default_seat().and_then(|seat| seat.keyboard()) {
            state |= keyboard.modifier_state();
        }
    }
    state
}

/// A drop choice menu (`drop_choice_menu`): Copy/Move/Link/Cancel at the drop point.
#[derive(Clone)]
pub struct DropMenu {
    pub popover: gtk::Popover,
    chosen: Rc<RefCell<Option<Option<&'static str>>>>,
}

impl DropMenu {
    /// `popover.choose(kind)`.
    pub fn choose(&self, kind: Option<&'static str>) {
        if self.chosen.borrow().is_none() {
            *self.chosen.borrow_mut() = Some(kind);
        }
        self.popover.popdown();
    }
}

pub fn drop_choice_menu(widget: &gtk::Widget, x: f64, y: f64, callback: Box<dyn Fn(Option<&'static str>)>) -> DropMenu {
    let popover = gtk::Popover::builder().has_arrow(true).autohide(true).build();
    popover.add_css_class("menu");
    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let menu = DropMenu { popover: popover.clone(), chosen: Rc::new(RefCell::new(None)) };
    let mut buttons = Vec::new();
    for (label, kind) in [(tr!("Copy Here"), Some("copy")), (tr!("Move Here"), Some("move")), (tr!("Link Here"), Some("link")),
                          (tr!("Cancel"), None)] {
        if kind.is_none() {
            column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        let control = gtk::Button::with_label(&label);
        control.add_css_class("flat");
        if let Some(label) = control.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.);
        }
        let m = menu.clone();
        control.connect_clicked(move |_| m.choose(kind));
        column.append(&control);
        buttons.push(control);
    }
    popover.set_child(Some(&column));
    popover.set_parent(widget);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    let chosen = menu.chosen.clone();
    let callback = Rc::new(callback);
    popover.connect_closed(move |popover| {
        if chosen.borrow().is_none() {
            *chosen.borrow_mut() = Some(None);
        }
        let (popover, chosen, callback) = (popover.clone(), chosen.clone(), callback.clone());
        glib::idle_add_local_once(move || {
            popover.unparent();
            let kind = chosen.borrow().flatten();
            callback(kind);
        });
    });
    popover.popup();
    buttons[0].grab_focus();
    menu
}

/// `BrowserPane._item(value)`: a TreeListRow's item, or the item itself.
pub fn item_of(object: &glib::Object) -> Option<FileItem> {
    if let Some(row) = object.downcast_ref::<gtk::TreeListRow>() {
        return row.item().and_downcast::<FileItem>();
    }
    object.clone().downcast::<FileItem>().ok()
}

fn model_item(model: &impl IsA<gio::ListModel>, index: u32) -> Option<FileItem> {
    model.item(index).as_ref().and_then(item_of)
}

pub struct PaneState {
    pub current: gio::File,
    pub history: Vec<String>,
    pub history_index: i64,
    pub loading: bool,
    pub searching: bool,
    pub closed: bool,
    pub options: Map<String, Value>,
    pub git_status: HashMap<String, String>,
    pub git_rendered: HashMap<String, String>,
    pub extras_generation: u64,
    pub git_generation: u64,
    pub git_running: bool,
    pub git_update_pending: bool,
    pub git_last_started: Option<Instant>,
    pub git_update_source: Option<glib::SourceId>,
    pub type_prefix: String,
    pub type_time: Option<Instant>,
    pub pending_selection: HashSet<String>,
    pub removal: Option<(HashSet<String>, Option<String>)>,
    pub reveal_selection: bool,
    pub select_first: bool,
    pub monitor: Option<gio::FileMonitor>,
    pub monitor_handler: Option<glib::SignalHandlerId>,
    pub reload_source: Option<glib::SourceId>,
    pub monitor_pending: Vec<(String, gio::File)>,
    pub monitor_generation: u64,
    pub monitor_cancellable: Option<gio::Cancellable>,
    pub monitor_busy: bool,
    pub monitor_rescan: bool,
    pub child_loaders: Vec<DirectoryLoader>,
    pub scroll_positions: HashMap<String, (f64, f64)>,
    pub scroll_restore: Option<gtk::TickCallbackId>,
    pub peek_source: Option<glib::SourceId>,
    pub peek_cancellable: Option<gio::Cancellable>,
    pub peek_popover: Option<gtk::Popover>,
    pub syncing_header_sort: bool,
    pub connections: Vec<(glib::Object, glib::SignalHandlerId)>,
    /// Test counters for `patch.object(pane, 'navigate'/'refresh', wraps=...)`.
    pub navigate_calls: u32,
    pub refresh_calls: u32,
}

pub struct PaneInner {
    pub root: gtk::Box,
    pub host: RefCell<Option<Weak<dyn PaneHost>>>,
    pub preferences: Prefs,
    pub thumbnails: Thumbnails,
    pub state: RefCell<PaneState>,
    pub loader: DirectoryLoader,
    pub searcher: SearchLoader,
    pub monitor_loader: DirectoryLoader,
    pub store: gio::ListStore,
    pub filter: gtk::CustomFilter,
    pub filtered: gtk::FilterListModel,
    pub sorter: gtk::CustomSorter,
    pub sorted: gtk::SortListModel,
    pub flat_selection: gtk::MultiSelection,
    pub tree: gtk::TreeListModel,
    pub tree_selection: gtk::MultiSelection,
    pub selection: RefCell<gtk::MultiSelection>,
    pub location_box: gtk::Box,
    pub breadcrumbs: gtk::Box,
    pub location_blank: gtk::Box,
    pub location: gtk::Entry,
    pub location_stack: gtk::Stack,
    pub location_completion: RefCell<Option<LocationCompletion>>,
    pub search_box: gtk::Box,
    pub search_entry: gtk::SearchEntry,
    pub contents_check: gtk::CheckButton,
    pub filter_entry: gtk::SearchEntry,
    pub views: gtk::Stack,
    pub icons: gtk::GridView,
    pub compact: gtk::GridView,
    pub details: gtk::ColumnView,
    pub sort_columns: RefCell<Vec<(String, gtk::ColumnViewColumn)>>,
    pub git_column: RefCell<Option<gtk::ColumnViewColumn>>,
    pub columns: RefCell<Option<MillerColumns>>,
    pub empty: gtk::Label,
    pub footer: gtk::Box,
    pub spinner: gtk::Spinner,
    pub summary: gtk::Label,
    pub stop_button: RefCell<Option<gtk::Button>>,
}

#[derive(Clone)]
pub struct BrowserPane(pub Rc<PaneInner>);

impl PartialEq for BrowserPane {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::ops::Deref for BrowserPane {
    type Target = PaneInner;
    fn deref(&self) -> &PaneInner {
        &self.0
    }
}

#[derive(Clone)]
pub struct WeakPane(Weak<PaneInner>);

impl WeakPane {
    pub fn upgrade(&self) -> Option<BrowserPane> {
        self.0.upgrade().map(BrowserPane)
    }
}

fn opt_bool(options: &Map<String, Value>, key: &str, default: bool) -> bool {
    options.get(key).map(suite_core::pyjson::truthy).unwrap_or(default)
}

fn opt_str<'a>(options: &'a Map<String, Value>, key: &str, default: &'a str) -> &'a str {
    options.get(key).and_then(Value::as_str).unwrap_or(default)
}

fn opt_int(options: &Map<String, Value>, key: &str, default: i64) -> i64 {
    options.get(key).and_then(Value::as_i64).unwrap_or(default)
}

impl BrowserPane {
    pub fn new(host: Weak<dyn PaneHost>, start: &gio::File, preferences: Prefs, thumbnails: Thumbnails) -> Self {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).vexpand(true).build();
        root.add_css_class("files-pane");
        let store = gio::ListStore::new::<FileItem>();
        let filter = gtk::CustomFilter::new(|_| true);
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorter = gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal);
        let sorted = gtk::SortListModel::new(Some(filtered.clone()), Some(sorter.clone()));
        let flat_selection = gtk::MultiSelection::new(Some(sorted.clone()));
        let slot: Rc<RefCell<Weak<PaneInner>>> = Rc::new(RefCell::new(Weak::new()));
        let tree_slot = slot.clone();
        let tree = gtk::TreeListModel::new(sorted.clone(), false, false, move |object| {
            let pane = tree_slot.borrow().upgrade().map(BrowserPane)?;
            if pane.state.borrow().closed {
                return None;
            }
            pane.children_model(&item_of(object)?)
        });
        let tree_selection = gtk::MultiSelection::new(Some(tree.clone()));
        let location_box = gtk::Box::builder().hexpand(true).build();
        location_box.add_css_class("files-address");
        let breadcrumbs = gtk::Box::builder().hexpand(true).build();
        let location_blank = gtk::Box::builder().hexpand(true).focusable(true).width_request(48).build();
        location_blank.set_cursor_from_name(Some("text"));
        location_blank.set_tooltip_text(Some("Edit location — Ctrl+L"));
        location_blank.update_property(&[gtk::accessible::Property::Label("Edit location")]);
        let location = gtk::Entry::builder().hexpand(true).build();
        location.set_tooltip_text(Some("Location — Ctrl+L"));
        location.update_property(&[gtk::accessible::Property::Label("Location")]);
        let location_stack = gtk::Stack::builder().hexpand(true).vhomogeneous(true).build();
        let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        search_box.add_css_class("files-location");
        let search_entry = gtk::SearchEntry::builder().hexpand(true).placeholder_text("Search files…").build();
        let contents_check = gtk::CheckButton::with_label("Contents");
        contents_check.set_tooltip_text(Some("Also search the first 1 MiB of regular text files. Binary files and symbolic links are skipped."));
        let filter_entry = gtk::SearchEntry::builder().placeholder_text("Filter this folder…").build();
        let views = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        let icons = gtk::GridView::builder().min_columns(1).max_columns(30).enable_rubberband(true).build();
        let compact = gtk::GridView::builder().min_columns(1).max_columns(12).enable_rubberband(true).build();
        let details = gtk::ColumnView::builder().enable_rubberband(true).build();
        let empty = gtk::Label::builder().label("This folder is empty.").vexpand(false).build();
        empty.add_css_class("files-empty");
        empty.set_visible(false);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer.add_css_class("files-status");
        let spinner = gtk::Spinner::new();
        let summary = gtk::Label::builder().xalign(0.).hexpand(true).build();
        let state = PaneState {
            current: start.clone(), history: Vec::new(), history_index: -1, loading: false, searching: false, closed: false,
            options: Map::new(), git_status: HashMap::new(), git_rendered: HashMap::new(), extras_generation: 0, git_generation: 0,
            git_running: false, git_update_pending: false, git_last_started: None, git_update_source: None, type_prefix: String::new(),
            type_time: None, pending_selection: HashSet::new(), removal: None, reveal_selection: false, select_first: false,
            monitor: None, monitor_handler: None, reload_source: None, monitor_pending: Vec::new(), monitor_generation: 0,
            monitor_cancellable: None, monitor_busy: false, monitor_rescan: false, child_loaders: Vec::new(),
            scroll_positions: HashMap::new(), scroll_restore: None, peek_source: None, peek_cancellable: None, peek_popover: None,
            syncing_header_sort: false, connections: Vec::new(), navigate_calls: 0, refresh_calls: 0,
        };
        let inner = Rc::new(PaneInner {
            root, host: RefCell::new(Some(host)), preferences, thumbnails, state: RefCell::new(state), loader: DirectoryLoader::default(),
            searcher: SearchLoader::default(), monitor_loader: DirectoryLoader::default(), store, filter, filtered, sorter, sorted,
            selection: RefCell::new(flat_selection.clone()), flat_selection, tree, tree_selection, location_box, breadcrumbs,
            location_blank, location, location_stack, location_completion: RefCell::new(None), search_box, search_entry,
            contents_check, filter_entry, views, icons, compact, details, sort_columns: RefCell::new(Vec::new()),
            git_column: RefCell::new(None), columns: RefCell::new(None), empty, footer, spinner, summary,
            stop_button: RefCell::new(None),
        });
        *slot.borrow_mut() = Rc::downgrade(&inner);
        let pane = BrowserPane(inner);
        pane.state.borrow_mut().options = pane.compute_options();
        pane.setup();
        pane.navigate(start, true, None);
        pane
    }

    pub fn downgrade(&self) -> WeakPane {
        WeakPane(Rc::downgrade(&self.0))
    }

    pub fn host(&self) -> Option<Rc<dyn PaneHost>> {
        self.host.borrow().as_ref().and_then(Weak::upgrade)
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    fn connect<O: IsA<glib::Object>>(&self, object: &O, id: glib::SignalHandlerId) {
        self.state.borrow_mut().connections.push((object.clone().upcast(), id));
    }

    fn pane_tool(&self, icon: &str, title: &str, callback: impl Fn(&BrowserPane) + 'static) -> gtk::Button {
        let weak = self.downgrade();
        tool_icon(icon, title, move || {
            if let Some(pane) = weak.upgrade() {
                callback(&pane)
            }
        })
    }

    fn setup(&self) {
        let weak = self.downgrade();
        {
            let w = weak.clone();
            self.filter.set_filter_func(move |object| match (w.upgrade(), item_of(object)) {
                (Some(pane), Some(item)) => pane.filter_item(&item),
                _ => false,
            });
            let w = weak.clone();
            self.sorter.set_sort_func(move |a, b| match (w.upgrade(), item_of(a), item_of(b)) {
                (Some(pane), Some(a), Some(b)) => pane.compare(&a, &b),
                _ => gtk::Ordering::Equal,
            });
        }
        let w = weak.clone();
        let id = self.flat_selection.connect_selection_changed(move |_, _, _| {
            if let Some(pane) = w.upgrade() {
                pane.selection_changed_cb()
            }
        });
        self.connect(&self.flat_selection, id);
        let w = weak.clone();
        let id = self.tree_selection.connect_selection_changed(move |_, _, _| {
            if let (Some(pane), Some(host)) = (w.upgrade(), w.upgrade().and_then(|p| p.host())) {
                host.selection_changed(&pane)
            }
        });
        self.connect(&self.tree_selection, id);
        // Address: breadcrumbs (click the blank to edit) or an entry.
        let click_location = gtk::GestureClick::builder().button(1).build();
        let w = weak.clone();
        click_location.connect_released(move |_, _, _, _| {
            if let Some(pane) = w.upgrade() {
                pane.edit_location()
            }
        });
        self.location_blank.add_controller(click_location);
        let location_keys = gtk::EventControllerKey::new();
        let w = weak.clone();
        location_keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space) {
                if let Some(pane) = w.upgrade() {
                    pane.edit_location();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.location_blank.add_controller(location_keys);
        let breadcrumb_scroll = gtk::ScrolledWindow::builder().hexpand(true).min_content_width(100)
            .hscrollbar_policy(gtk::PolicyType::External).vscrollbar_policy(gtk::PolicyType::Never).build();
        breadcrumb_scroll.set_child(Some(&self.breadcrumbs));
        breadcrumb_scroll.hadjustment().connect_changed(|adjustment| {
            adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.));
        });
        let w = weak.clone();
        let id = self.location.connect_activate(move |entry| {
            if let Some(pane) = w.upgrade() {
                pane.activate_location(entry)
            }
        });
        self.connect(&self.location, id);
        let focus = gtk::EventControllerFocus::new();
        let w = weak.clone();
        focus.connect_leave(move |_| {
            if let Some(pane) = w.upgrade() {
                pane.finish_location_edit(false)
            }
        });
        self.location.add_controller(focus);
        self.location_stack.add_named(&breadcrumb_scroll, Some("breadcrumbs"));
        self.location_stack.add_named(&self.location, Some("entry"));
        self.location_box.append(&self.location_stack);
        let (wc, wh) = (weak.clone(), weak.clone());
        *self.location_completion.borrow_mut() = Some(LocationCompletion::new(
            &self.location,
            Rc::new(move || wc.upgrade().map(|p| p.current()).unwrap_or_else(home_file)),
            Rc::new(move || wh.upgrade().is_some_and(|p| opt_bool(&p.state.borrow().options, "show_hidden", false))),
        ));
        // Search and filter.
        let w = weak.clone();
        let id = self.search_entry.connect_activate(move |_| {
            if let Some(pane) = w.upgrade() {
                pane.search()
            }
        });
        self.connect(&self.search_entry, id);
        let w = weak.clone();
        let id = self.search_entry.connect_stop_search(move |_| {
            if let Some(pane) = w.upgrade() {
                pane.stop_search()
            }
        });
        self.connect(&self.search_entry, id);
        self.search_box.append(&self.search_entry);
        self.search_box.append(&self.contents_check);
        self.search_box.append(&self.pane_tool("system-search-symbolic", "Search this folder and subfolders", |p| p.search()));
        self.search_box.append(&self.pane_tool("bookmark-new-symbolic", "Save this search", |p| {
            if let Some(host) = p.host() {
                host.action("save-search")
            }
        }));
        self.search_box.append(&self.pane_tool("window-close-symbolic", "Close search", |p| p.stop_search()));
        self.search_box.set_visible(false);
        self.root.append(&self.search_box);
        let w = weak.clone();
        let id = self.filter_entry.connect_search_changed(move |_| {
            if let Some(pane) = w.upgrade() {
                pane.filter.changed(gtk::FilterChange::Different)
            }
        });
        self.connect(&self.filter_entry, id);
        let w = weak.clone();
        let id = self.filter_entry.connect_stop_search(move |_| {
            if let Some(pane) = w.upgrade() {
                pane.show_filter(false)
            }
        });
        self.connect(&self.filter_entry, id);
        self.filter_entry.set_visible(false);
        self.root.append(&self.filter_entry);
        // Views. Only the displayed layout binds the shared models.
        self.icons.set_factory(Some(&self.factory("icons")));
        self.compact.set_factory(Some(&self.factory("compact")));
        let mut sort_columns = Vec::new();
        for (title, key, expand) in [("Name", "name", true), ("Size", "size", false), ("Type", "type", false),
                                     ("Modified", "modified", false), ("Git", "git", false)] {
            let column = gtk::ColumnViewColumn::new(Some(title), Some(self.factory(key)));
            column.set_expand(expand);
            column.set_resizable(true);
            if key != "git" {
                let k = key.to_string();
                column.set_sorter(Some(&gtk::CustomSorter::new(move |a, b| match (item_of(a), item_of(b)) {
                    (Some(a), Some(b)) => fm::compare_items(&a, &b, &k, false, true, true).into(),
                    _ => gtk::Ordering::Equal,
                })));
                sort_columns.push((key.to_string(), column.clone()));
            } else {
                *self.git_column.borrow_mut() = Some(column.clone());
            }
            self.details.append_column(&column);
        }
        *self.sort_columns.borrow_mut() = sort_columns;
        if let Some(header_sorter) = self.details.sorter() {
            let w = weak.clone();
            let id = header_sorter.connect_changed(move |_, _| {
                if let Some(pane) = w.upgrade() {
                    pane.header_sort_changed()
                }
            });
            self.connect(&header_sorter, id);
        }
        *self.columns.borrow_mut() = Some(MillerColumns::new(self, &self.factory("compact")));
        let single_click = opt_bool(&self.state.borrow().options, "single_click", false);
        for (name, view) in self.view_widgets() {
            let w = weak.clone();
            let activate = move |position: u32, model: Option<gtk::SelectionModel>| {
                if let (Some(pane), Some(model)) = (w.upgrade(), model) {
                    pane.activate(model_item(&model, position));
                }
            };
            if let Some(grid) = view.downcast_ref::<gtk::GridView>() {
                let id = grid.connect_activate(move |v, pos| activate(pos, v.model()));
                self.connect(grid, id);
                grid.set_single_click_activate(single_click);
            } else if let Some(column) = view.downcast_ref::<gtk::ColumnView>() {
                let id = column.connect_activate(move |v, pos| activate(pos, v.model()));
                self.connect(column, id);
                column.set_single_click_activate(single_click);
            } else if let Some(list) = view.downcast_ref::<gtk::ListView>() {
                let id = list.connect_activate(move |v, pos| activate(pos, v.model()));
                self.connect(list, id);
                list.set_single_click_activate(single_click);
            }
            if name == "columns" {
                let columns = self.columns.borrow().as_ref().unwrap().root.clone();
                self.views.add_named(&columns, Some(name));
            } else {
                let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
                scroll.set_child(Some(&view));
                self.views.add_named(&scroll, Some(name));
            }
            self.controllers(&view, None);
            let keys = gtk::EventControllerKey::new();
            let w = weak.clone();
            keys.connect_key_pressed(move |_, key, _, state| match w.upgrade() {
                Some(pane) if pane.type_ahead(key, state) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            view.add_controller(keys);
            // Item widgets bind Space and arrows themselves, so preview and
            // navigation keys are handled before the event reaches them.
            let capture = gtk::EventControllerKey::new();
            capture.set_propagation_phase(gtk::PropagationPhase::Capture);
            let w = weak.clone();
            capture.connect_key_pressed(move |controller, key, code, state| {
                let (Some(pane), Some(widget)) = (w.upgrade(), controller.widget()) else { return glib::Propagation::Proceed };
                if pane.view_key_pressed(&widget, key, code, state) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            view.add_controller(capture);
        }
        self.root.append(&self.views);
        self.root.append(&self.empty);
        self.footer.append(&self.spinner);
        self.footer.append(&self.summary);
        let stop = self.pane_tool("process-stop-symbolic", "Stop loading", |p| p.cancel_loading());
        stop.set_visible(false);
        self.footer.append(&stop);
        *self.stop_button.borrow_mut() = Some(stop);
        self.root.append(&self.footer);
        let w = weak.clone();
        let id = self.filtered.connect_items_changed(move |_, _, _, _| {
            if let Some(pane) = w.upgrade() {
                pane.update_summary()
            }
        });
        self.connect(&self.filtered, id);
        let focus = gtk::EventControllerFocus::new();
        let w = weak.clone();
        focus.connect_enter(move |_| {
            if let Some(pane) = w.upgrade() {
                pane.focus_pane()
            }
        });
        self.root.add_controller(focus);
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = weak.clone();
        click.connect_pressed(move |_, _, _, _| {
            if let Some(pane) = w.upgrade() {
                pane.focus_pane()
            }
        });
        self.root.add_controller(click);
        for signal in ["map", "unmap"] {
            let w = weak.clone();
            let id = self.root.connect_local(signal, false, move |_| {
                if let Some(pane) = w.upgrade() {
                    pane.sync_view_models()
                }
                None
            });
            self.connect(&self.root, id);
        }
        let w = weak.clone();
        let id = self.views.connect_visible_child_name_notify(move |_| {
            if let Some(pane) = w.upgrade() {
                pane.sync_view_models()
            }
        });
        self.connect(&self.views, id);
    }

    pub fn current(&self) -> gio::File {
        self.state.borrow().current.clone()
    }
    pub fn loading(&self) -> bool {
        self.state.borrow().loading
    }
    pub fn searching(&self) -> bool {
        self.state.borrow().searching
    }
    pub fn closed(&self) -> bool {
        self.state.borrow().closed
    }
    pub fn options(&self) -> Map<String, Value> {
        self.state.borrow().options.clone()
    }
    pub fn option_bool(&self, key: &str) -> bool {
        opt_bool(&self.state.borrow().options, key, false)
    }
    pub fn option_str(&self, key: &str, default: &str) -> String {
        opt_str(&self.state.borrow().options, key, default).to_string()
    }
    pub fn option_int(&self, key: &str, default: i64) -> i64 {
        opt_int(&self.state.borrow().options, key, default)
    }
    pub fn history_index(&self) -> i64 {
        self.state.borrow().history_index
    }
    pub fn history_len(&self) -> usize {
        self.state.borrow().history.len()
    }
    pub fn selection(&self) -> gtk::MultiSelection {
        self.selection.borrow().clone()
    }
    pub fn columns(&self) -> MillerColumns {
        self.columns.borrow().clone().expect("columns")
    }
    pub fn stop_button(&self) -> gtk::Button {
        self.stop_button.borrow().clone().expect("stop button")
    }
    pub fn sort_column(&self, key: &str) -> Option<gtk::ColumnViewColumn> {
        self.sort_columns.borrow().iter().find(|(k, _)| k == key).map(|(_, c)| c.clone())
    }
    pub fn git_status(&self) -> HashMap<String, String> {
        self.state.borrow().git_status.clone()
    }
    pub fn set_pending_selection(&self, uris: impl IntoIterator<Item = String>) {
        self.state.borrow_mut().pending_selection = uris.into_iter().collect();
    }
    pub fn extend_pending_selection(&self, uris: impl IntoIterator<Item = String>) {
        self.state.borrow_mut().pending_selection.extend(uris);
    }
    pub fn location_completion(&self) -> LocationCompletion {
        self.location_completion.borrow().clone().expect("completion")
    }

    fn view_widgets(&self) -> Vec<(&'static str, gtk::Widget)> {
        let columns = self.columns.borrow().as_ref().map(|c| c.current.clone().upcast::<gtk::Widget>());
        let mut views = vec![("icons", self.icons.clone().upcast()), ("compact", self.compact.clone().upcast()),
                             ("details", self.details.clone().upcast())];
        if let Some(columns) = columns {
            views.push(("columns", columns));
        }
        views
    }

    /// The list widget of a layout; the displayed one by default.
    pub fn view_widget(&self, name: Option<&str>) -> gtk::Widget {
        let visible = self.views.visible_child_name().map(|s| s.to_string());
        let name = name.map(str::to_string).or(visible).unwrap_or_else(|| "icons".into());
        self.view_widgets().into_iter().find(|(n, _)| *n == name).map(|(_, v)| v).unwrap_or_else(|| self.icons.clone().upcast())
    }

    fn view_scroll(&self, name: &str) -> Option<gtk::ScrolledWindow> {
        if name == "columns" {
            return self.columns.borrow().as_ref().map(|c| c.current_scroll.clone());
        }
        self.views.child_by_name(name).and_downcast::<gtk::ScrolledWindow>()
    }

    fn view_model(view: &gtk::Widget) -> Option<gtk::SelectionModel> {
        if let Some(v) = view.downcast_ref::<gtk::GridView>() {
            return v.model();
        }
        if let Some(v) = view.downcast_ref::<gtk::ColumnView>() {
            return v.model();
        }
        view.downcast_ref::<gtk::ListView>().and_then(|v| v.model())
    }

    fn set_view_model(view: &gtk::Widget, model: Option<&gtk::MultiSelection>) {
        if let Some(v) = view.downcast_ref::<gtk::GridView>() {
            v.set_model(model);
        } else if let Some(v) = view.downcast_ref::<gtk::ColumnView>() {
            v.set_model(model);
        } else if let Some(v) = view.downcast_ref::<gtk::ListView>() {
            v.set_model(model);
        }
    }

    fn set_single_click(view: &gtk::Widget, value: bool) {
        if let Some(v) = view.downcast_ref::<gtk::GridView>() {
            v.set_single_click_activate(value);
        } else if let Some(v) = view.downcast_ref::<gtk::ColumnView>() {
            v.set_single_click_activate(value);
        } else if let Some(v) = view.downcast_ref::<gtk::ListView>() {
            v.set_single_click_activate(value);
        }
    }

    pub fn single_click(view: &gtk::Widget) -> bool {
        if let Some(v) = view.downcast_ref::<gtk::GridView>() {
            return v.is_single_click_activate();
        }
        if let Some(v) = view.downcast_ref::<gtk::ColumnView>() {
            return v.is_single_click_activate();
        }
        view.downcast_ref::<gtk::ListView>().is_some_and(|v| v.is_single_click_activate())
    }

    fn rubberband(view: &gtk::Widget) -> bool {
        if let Some(v) = view.downcast_ref::<gtk::GridView>() {
            return v.enables_rubberband();
        }
        if let Some(v) = view.downcast_ref::<gtk::ColumnView>() {
            return v.enables_rubberband();
        }
        view.downcast_ref::<gtk::ListView>().is_some_and(|v| v.enables_rubberband())
    }

    fn set_view_rubberband(view: &gtk::Widget, enabled: bool) {
        if let Some(v) = view.downcast_ref::<gtk::GridView>() {
            v.set_enable_rubberband(enabled);
        } else if let Some(v) = view.downcast_ref::<gtk::ColumnView>() {
            v.set_enable_rubberband(enabled);
        } else if let Some(v) = view.downcast_ref::<gtk::ListView>() {
            v.set_enable_rubberband(enabled);
        }
    }

    fn scroll_view_to(view: &gtk::Widget, index: u32, flags: gtk::ListScrollFlags) {
        if let Some(v) = view.downcast_ref::<gtk::GridView>() {
            v.scroll_to(index, flags, None);
        } else if let Some(v) = view.downcast_ref::<gtk::ColumnView>() {
            v.scroll_to(index, None::<&gtk::ColumnViewColumn>, flags, None);
        } else if let Some(v) = view.downcast_ref::<gtk::ListView>() {
            v.scroll_to(index, flags, None);
        }
    }

    pub fn sync_view_models(&self) {
        if let Some(tick) = self.state.borrow_mut().scroll_restore.take() {
            tick.remove();
        }
        let active = if self.root.is_mapped() && !self.closed() { self.views.visible_child_name().map(|s| s.to_string()) } else { None };
        let columns = self.columns();
        if active.as_deref() == Some("columns") {
            columns.sync();
        } else {
            columns.clear();
        }
        let mut restored: Option<(gtk::ScrolledWindow, (f64, f64))> = None;
        for (name, view) in self.view_widgets() {
            let model = if Some(name) == active.as_deref() {
                Some(if name == "details" { self.tree_selection.clone() } else { self.flat_selection.clone() })
            } else {
                None
            };
            let current = Self::view_model(&view);
            if current.as_ref().map(|m| m.as_ptr() as usize) == model.as_ref().map(|m| m.upcast_ref::<gtk::SelectionModel>().as_ptr() as usize) {
                continue;
            }
            let scroll = self.view_scroll(name);
            if let (Some(_), Some(scroll)) = (&current, &scroll) {
                self.state.borrow_mut().scroll_positions.insert(name.into(), (scroll.hadjustment().value(), scroll.vadjustment().value()));
            }
            Self::set_view_model(&view, model.as_ref());
            if model.is_some() {
                if let (Some(scroll), Some(position)) = (scroll, self.state.borrow().scroll_positions.get(name).copied()) {
                    restored = Some((scroll, position));
                }
            }
        }
        let reveal = active.is_some() && std::mem::take(&mut self.state.borrow_mut().reveal_selection);
        if reveal {
            self.scroll_to_selection(&self.view_widget(active.as_deref()));
        } else if let Some((scroll, (x, y))) = restored {
            // Restored rows need an allocation before adjustment ranges exist.
            let frames = Cell::new(0);
            let weak = self.downgrade();
            let id = self.root.add_tick_callback(move |_, _| {
                frames.set(frames.get() + 1);
                if frames.get() < 2 {
                    return glib::ControlFlow::Continue;
                }
                scroll.hadjustment().set_value(x);
                scroll.vadjustment().set_value(y);
                if let Some(pane) = weak.upgrade() {
                    pane.state.borrow_mut().scroll_restore = None;
                }
                glib::ControlFlow::Break
            });
            self.state.borrow_mut().scroll_restore = Some(id);
        }
    }

    fn scroll_to_selection(&self, view: &gtk::Widget) {
        let selected = self.selection().selection();
        if selected.size() > 0 {
            Self::scroll_view_to(view, selected.nth(0), gtk::ListScrollFlags::FOCUS);
        }
    }

    fn compute_options(&self) -> Map<String, Value> {
        let prefs = &self.preferences;
        let mut options = bridge::pdata(prefs);
        if bridge::pbool(prefs, "remember_folders") {
            let uri = self.current().uri().to_string();
            options.extend(bridge::pfolder_options(prefs, &uri));
        }
        options
    }

    fn children_model(&self, item: &FileItem) -> Option<gio::ListModel> {
        if !item.is_dir() {
            return None;
        }
        let store = gio::ListStore::new::<FileItem>();
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(self.filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), Some(self.sorter.clone()));
        let loader = DirectoryLoader::default();
        self.state.borrow_mut().child_loaders.push(loader.clone());
        let weak = self.downgrade();
        loader.start(&item.file(), move |batch: Vec<FileItem>| store.splice(store.n_items(), 0, &batch), move |error| {
            if let (Some(error), Some(pane)) = (error, weak.upgrade()) {
                if !pane.closed() {
                    if let Some(host) = pane.host() {
                        host.message(&suite_core::pyerr::python_error_text(&error));
                    }
                }
            }
        });
        Some(sorted.upcast())
    }

    fn filter_item(&self, item: &FileItem) -> bool {
        let show_hidden = opt_bool(&self.state.borrow().options, "show_hidden", false);
        (show_hidden || !item.is_hidden()) && fm::name_matches(&item.display_name(), self.filter_entry.text().as_str())
    }

    fn typing(&self) -> bool {
        let s = self.state.borrow();
        !s.type_prefix.is_empty() && s.type_time.is_some_and(|t| t.elapsed().as_secs_f64() < TYPE_AHEAD_SECONDS)
    }

    /// Test hook for `pane.type_prefix, pane.type_time = 'my', monotonic()`.
    pub fn set_type_prefix(&self, prefix: &str) {
        let mut s = self.state.borrow_mut();
        s.type_prefix = prefix.into();
        s.type_time = Some(Instant::now());
    }

    /// `_view_key_pressed` (capture phase on every view).
    pub fn view_key_pressed(&self, widget: &gtk::Widget, keyval: gdk::Key, keycode: u32, state: gdk::ModifierType) -> bool {
        if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return false;
        }
        let typing = self.typing();
        let shifted = state.contains(gdk::ModifierType::SHIFT_MASK);
        if matches!(keyval, gdk::Key::space | gdk::Key::KP_Space) && !typing && !shifted {
            if !self.selected().is_empty() {
                if let Some(host) = self.host() {
                    if host.quick_look(self) {
                        return true;
                    }
                }
            }
            return false;
        }
        let key = latin_keyval(Some(&widget.display()), keyval, keycode);
        let vim = bridge::pbool(&self.preferences, "vim_keys") && !typing && !shifted;
        let columns = self.views.visible_child_name().as_deref() == Some("columns");
        let arrow = matches!(keyval, gdk::Key::Up | gdk::Key::Down | gdk::Key::Left | gdk::Key::Right | gdk::Key::KP_Up
            | gdk::Key::KP_Down | gdk::Key::KP_Left | gdk::Key::KP_Right);
        if arrow && self.selection().selection().size() == 0 {
            // With nothing selected, any arrow starts at the first item.
            self.move_selection(1);
        } else if vim && (key == gdk::Key::j || key == gdk::Key::k) {
            self.move_selection(if key == gdk::Key::j { 1 } else { -1 });
        } else if (vim && key == gdk::Key::h) || (columns && matches!(keyval, gdk::Key::Left | gdk::Key::KP_Left)) {
            self.up();
        } else if (vim && key == gdk::Key::l) || (columns && matches!(keyval, gdk::Key::Right | gdk::Key::KP_Right)) {
            let selected = self.selected();
            if selected.len() != 1 || !selected[0].is_dir() {
                return vim;
            }
            self.state.borrow_mut().select_first = true;
            self.navigate(&selected[0].file(), true, None);
        } else {
            return false;
        }
        true
    }

    /// Select the next/previous item (no wrapping) and give it focus.
    pub fn move_selection(&self, delta: i64) -> Option<FileItem> {
        let selection = self.selection();
        let count = selection.n_items();
        if count == 0 {
            return None;
        }
        let bits = selection.selection();
        let index = if bits.size() > 0 {
            let anchor = if delta > 0 { bits.nth(bits.size() as u32 - 1) } else { bits.nth(0) } as i64;
            (anchor + delta).clamp(0, count as i64 - 1) as u32
        } else if delta > 0 {
            0
        } else {
            count - 1
        };
        self.select_index(index);
        model_item(&selection, index)
    }

    /// Select only the row at `index`, scroll to it and give it focus.
    pub fn select_index(&self, index: u32) {
        let view = self.view_widget(None);
        if Self::view_model(&view).is_none() {
            self.selection().select_item(index, true);
        } else {
            Self::scroll_view_to(&view, index, gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT);
        }
    }

    fn item_uris(&self) -> Vec<String> {
        let selection = self.selection();
        (0..selection.n_items()).filter_map(|i| model_item(&selection, i)).map(|item| item.uri()).collect()
    }

    /// Select the row before `items` once they have left this folder.
    pub fn expect_removal(&self, items: &[FileItem]) {
        let uris: HashSet<String> = items.iter().map(|i| i.uri()).collect();
        let rows = self.item_uris();
        let Some(first) = rows.iter().position(|u| uris.contains(u)) else { return };
        let target = first.checked_sub(1).map(|before| rows[before].clone());
        self.state.borrow_mut().removal = Some((uris, target));
    }

    fn select_after_removal(&self, complete: bool) {
        let Some((uris, target)) = self.state.borrow().removal.clone() else { return };
        let rows: HashMap<String, u32> = self.item_uris().into_iter().enumerate().map(|(i, u)| (u, i as u32)).collect();
        let remaining: HashSet<&String> = uris.iter().filter(|u| rows.contains_key(*u)).collect();
        // Monitor events can remove rows one batch at a time; a full load is
        // final, and leaves failed items in place.
        if !remaining.is_empty() && !complete {
            return;
        }
        self.state.borrow_mut().removal = None;
        if remaining.len() == uris.len() || rows.is_empty() {
            return;
        }
        // With nothing before the deleted rows, start at the top.
        let index = target.and_then(|t| rows.get(&t).copied()).unwrap_or(0);
        self.select_index(index);
    }

    fn type_ahead(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return false;
        }
        let Some(c) = key.to_unicode() else { return false };
        if !is_printable(c) {
            return false;
        }
        let prefix = {
            let mut s = self.state.borrow_mut();
            let keep = s.type_time.is_some_and(|t| t.elapsed().as_secs_f64() < TYPE_AHEAD_SECONDS);
            if !keep {
                s.type_prefix.clear();
            }
            s.type_prefix.push(c);
            s.type_time = Some(Instant::now());
            s.type_prefix.clone()
        };
        let folded = suite_core::pyjson::casefold(&prefix);
        let selection = self.selection();
        for index in 0..selection.n_items() {
            let Some(item) = model_item(&selection, index) else { continue };
            if suite_core::pyjson::casefold(&item.display_name()).starts_with(&folded) {
                selection.select_item(index, true);
                Self::scroll_view_to(&self.view_widget(None), index, gtk::ListScrollFlags::FOCUS);
                self.summary.set_text(&format!("Jump to: {prefix}"));
                return true;
            }
        }
        false
    }

    fn compare(&self, a: &FileItem, b: &FileItem) -> gtk::Ordering {
        let s = self.state.borrow();
        let o = &s.options;
        fm::compare_items(a, b, opt_str(o, "sort", "name"), opt_bool(o, "descending", false), opt_bool(o, "folders_first", true),
                          opt_bool(o, "natural_sort", true)).into()
    }

    fn header_sort_changed(&self) {
        if self.state.borrow().syncing_header_sort || self.closed() {
            return;
        }
        let Some(header_sorter) = self.details.sorter().and_downcast::<gtk::ColumnViewSorter>() else { return };
        let column = header_sorter.primary_sort_column();
        let key = self.sort_columns.borrow().iter().find(|(_, c)| Some(c) == column.as_ref()).map(|(k, _)| k.clone());
        let Some(key) = key else {
            self.sync_header_sort();
            return;
        };
        let descending = header_sorter.primary_sort_order() == gtk::SortType::Descending;
        {
            let s = self.state.borrow();
            if opt_str(&s.options, "sort", "") == key && s.options.get("descending") == Some(&Value::Bool(descending)) {
                return;
            }
        }
        let changes = bridge::map(&[("sort", Value::String(key)), ("descending", Value::Bool(descending))]);
        let result = if bridge::pbool(&self.preferences, "remember_folders") {
            bridge::pset_folder_options(&self.preferences, self.current().uri().as_str(), changes.clone())
        } else {
            bridge::pupdate(&self.preferences, changes.clone())
        };
        if let Err(error) = result {
            if let Some(host) = self.host() {
                host.message(&error);
            }
            self.sync_header_sort();
            return;
        }
        self.state.borrow_mut().options.extend(changes);
        // Roots and all expanded child models share this sorter, so folder-first
        // and natural-sort policy applies at every tree depth.
        self.sorter.changed(gtk::SorterChange::Different);
        if let Some(host) = self.host() {
            host.pane_changed(self);
        }
    }

    fn sync_header_sort(&self) {
        let (key, descending) = {
            let s = self.state.borrow();
            (opt_str(&s.options, "sort", "name").to_string(), opt_bool(&s.options, "descending", false))
        };
        let order = if descending { gtk::SortType::Descending } else { gtk::SortType::Ascending };
        let Some(header_sorter) = self.details.sorter().and_downcast::<gtk::ColumnViewSorter>() else { return };
        let column = self.sort_column(&key).or_else(|| self.sort_column("name"));
        if header_sorter.primary_sort_column() == column && header_sorter.primary_sort_order() == order {
            return;
        }
        self.state.borrow_mut().syncing_header_sort = true;
        self.details.sort_by_column(column.as_ref(), order);
        self.state.borrow_mut().syncing_header_sort = false;
    }

    fn factory(&self, mode: &'static str) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        let weak = self.downgrade();
        factory.connect_setup(move |_, object| {
            let Some(row) = object.downcast_ref::<gtk::ListItem>() else { return };
            let Some(pane) = weak.upgrade() else { return };
            let child: gtk::Widget = if matches!(mode, "icons" | "compact" | "name") {
                let orientation = if mode == "icons" { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
                let tile = FileTile::new(orientation, 6);
                tile.add_css_class("files-tile");
                let picture = gtk::Image::new();
                let label = gtk::Label::builder().xalign(if mode == "icons" { 0.5 } else { 0. }).ellipsize(pango::EllipsizeMode::End).build();
                label.set_max_width_chars(if mode == "icons" { 20 } else { 40 });
                // The cap sets only the column's default width; a widened Name
                // column gives the label its extra space instead of ellipsizing.
                label.set_hexpand(mode == "name");
                if mode == "icons" {
                    label.set_wrap(true);
                    label.set_wrap_mode(pango::WrapMode::WordChar);
                    label.set_lines(2);
                    label.set_justify(gtk::Justification::Center);
                    // Measure the unwrapped name so the tile decides where a long name wraps.
                    label.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
                    let attributes = pango::AttrList::new();
                    attributes.insert(pango::AttrInt::new_insert_hyphens(false));
                    label.set_attributes(Some(&attributes));
                }
                tile.append(&picture);
                tile.append(&label);
                tile.set_parts(Some(picture), label);
                if mode == "icons" {
                    tile.set_size_request(tile_width(pane.option_int("icon_size", 64)), -1);
                }
                if mode == "name" {
                    let expander = gtk::TreeExpander::new();
                    expander.set_child(Some(&tile));
                    // Files without an expander keep the indent, so every icon lines up.
                    expander.set_indent_for_icon(true);
                    expander.upcast()
                } else {
                    tile.upcast()
                }
            } else {
                let cell = FileTile::new(gtk::Orientation::Horizontal, 0);
                let label = gtk::Label::builder().xalign(0.).ellipsize(pango::EllipsizeMode::End).build();
                cell.append(&label);
                cell.set_parts(None, label);
                cell.upcast()
            };
            row.set_child(Some(&child));
            // GTK may clear ListItem.child before teardown; keep the owner.
            unsafe { row.set_data("wayfiles-widget", child.clone()) };
            pane.controllers(&child, Some(row));
        });
        let weak = self.downgrade();
        factory.connect_bind(move |_, object| {
            let Some(row) = object.downcast_ref::<gtk::ListItem>() else { return };
            let (Some(pane), Some(item)) = (weak.upgrade(), row.item().as_ref().and_then(item_of)) else { return };
            let Some(mut widget) = row.child() else { return };
            if mode == "name" {
                if let Some(expander) = widget.downcast_ref::<gtk::TreeExpander>() {
                    expander.set_list_row(row.item().and_downcast::<gtk::TreeListRow>().as_ref());
                    widget = expander.child().unwrap();
                }
            }
            let Some(tile) = widget.downcast_ref::<FileTile>() else { return };
            if matches!(mode, "icons" | "compact" | "name") {
                tile.cancel_thumbnail();
                let label = tile.label().unwrap();
                label.set_text(&item.display_name());
                let path = item.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let status = pane.state.borrow().git_status.get(&path).cloned().unwrap_or_default();
                // Hovering a folder peeks into it; a path tooltip would cover the peek.
                let peeking = item.is_dir() && pref_default_true(&pane.preferences, "folder_peek");
                let tooltip = if peeking {
                    None
                } else {
                    let mut text = item.file().parse_name().to_string();
                    if !status.is_empty() {
                        text.push_str(&format!("\nGit: {status}"));
                    }
                    Some(text)
                };
                tile.set_tooltip_text(tooltip.as_deref());
                let size = if mode == "icons" { pane.option_int("icon_size", 64) as i32 } else { LIST_ICON_SIZE };
                if mode == "icons" {
                    tile.set_size_request(tile_width(size as i64), -1);
                }
                let picture = tile.picture().unwrap();
                picture.set_pixel_size(size);
                picture.set_from_gicon(&item.icon());
                tile.set_bound_uri(Some(item.uri()));
                if opt_bool(&pane.state.borrow().options, "show_previews", true) && !item.is_dir() {
                    let (row_ref, item_ref, picture_ref) = (row.downgrade(), item.clone(), picture.downgrade());
                    let cancel = pane.thumbnails.request(&item, size, Rc::new(move |texture: Option<gdk::Texture>| {
                        let (Some(texture), Some(row), Some(picture)) = (texture, row_ref.upgrade(), picture_ref.upgrade()) else { return };
                        if row.item().as_ref().and_then(item_of).as_ref() == Some(&item_ref) {
                            picture.set_paintable(Some(&texture));
                        }
                    }));
                    tile.set_cancel(cancel);
                }
            } else {
                let path = item.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let text = match mode {
                    "git" => pane.state.borrow().git_status.get(&path).cloned().unwrap_or_default(),
                    "size" => if item.is_dir() { "—".into() } else { fm::format_size(item.size()) },
                    "modified" => fm::format_modified(item.modified()),
                    _ => gio::content_type_get_description(&item.content_type()).to_string(),
                };
                tile.label().unwrap().set_text(&text);
            }
        });
        factory.connect_unbind(move |_, object| {
            let Some(row) = object.downcast_ref::<gtk::ListItem>() else { return };
            let Some(mut widget) = row.child() else { return };
            if mode == "name" {
                if let Some(expander) = widget.downcast_ref::<gtk::TreeExpander>() {
                    expander.set_list_row(None);
                    widget = expander.child().unwrap();
                }
            }
            let Some(tile) = widget.downcast_ref::<FileTile>() else { return };
            tile.cancel_thumbnail();
            if matches!(mode, "icons" | "compact" | "name") {
                if let Some(picture) = tile.picture() {
                    picture.clear();
                }
                tile.set_bound_uri(None);
                tile.set_tooltip_text(None);
            }
        });
        factory.connect_teardown(move |_, object| {
            let Some(row) = object.downcast_ref::<gtk::ListItem>() else { return };
            // Row gestures close over their ListItem. Detach them before dropping the child.
            if let Some(widget) = unsafe { row.steal_data::<gtk::Widget>("wayfiles-widget") } {
                tile::remove_controllers(&widget);
            }
            row.set_child(None::<&gtk::Widget>);
        });
        factory
    }

    /// `_controllers(widget, row)`: context menu, press/rubberband, peek hover,
    /// middle click, drag source, file drop and Ark drop on a row or a view.
    pub fn controllers(&self, widget: &gtk::Widget, row: Option<&gtk::ListItem>) {
        let mut controllers: Vec<gtk::EventController> = Vec::new();
        let weak = self.downgrade();
        let row_ref = row.map(|r| r.downgrade());
        let right = gtk::GestureClick::builder().button(3).build();
        {
            let (weak, row_ref, target) = (weak.clone(), row_ref.clone(), widget.downgrade());
            right.connect_pressed(move |gesture, _, x, y| {
                let (Some(pane), Some(widget)) = (weak.upgrade(), target.upgrade()) else { return };
                pane.focus_pane();
                // The location/search field may still own keyboard focus after a
                // secondary click. Move focus into the file view first so
                // Copy/Cut/Paste route to files, not editable text.
                pane.view_widget(None).grab_focus();
                let selection = pane.selection();
                match row_ref.as_ref().and_then(|r| r.upgrade()) {
                    Some(row) => {
                        let position = row.position();
                        if position != gtk::INVALID_LIST_POSITION && !selection.is_selected(position) {
                            selection.select_item(position, true);
                        }
                    }
                    None if row_ref.is_none() => {
                        selection.unselect_all();
                    }
                    None => {}
                }
                if let Some(host) = pane.host() {
                    host.context_menu(&pane, &widget, x, y);
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });
        }
        widget.add_controller(right.clone());
        controllers.push(right.upcast());
        if let Some(row_ref) = row_ref.clone() {
            // GtkDragSource waits before it may start, while the view's
            // rubberband claims any press that moves sooner (GTK issue 5670),
            // so rubberband is only offered while no item is pressed.
            let primary = gtk::GestureClick::builder().button(1).build();
            let w = weak.clone();
            primary.connect_pressed(move |_, _, _, _| {
                if let Some(pane) = w.upgrade() {
                    pane.peek_cancel();
                    pane.set_rubberband(false);
                }
            });
            let w = weak.clone();
            primary.connect_released(move |_, _, _, _| {
                if let Some(pane) = w.upgrade() {
                    pane.set_rubberband(true)
                }
            });
            let w = weak.clone();
            primary.connect_stopped(move |_| {
                if let Some(pane) = w.upgrade() {
                    pane.set_rubberband(true)
                }
            });
            widget.add_controller(primary.clone());
            controllers.push(primary.upcast());
            let hover = gtk::EventControllerMotion::new();
            let (w, r, target) = (weak.clone(), row_ref.clone(), widget.downgrade());
            hover.connect_enter(move |_, _, _| {
                if let (Some(pane), Some(row), Some(widget)) = (w.upgrade(), r.upgrade(), target.upgrade()) {
                    if let Some(item) = row.item().as_ref().and_then(item_of) {
                        pane.peek_schedule(&widget, &item);
                    }
                }
            });
            let w = weak.clone();
            hover.connect_leave(move |_| {
                if let Some(pane) = w.upgrade() {
                    pane.peek_cancel()
                }
            });
            widget.add_controller(hover.clone());
            controllers.push(hover.upcast());
            let middle = gtk::GestureClick::builder().button(2).build();
            let (w, r) = (weak.clone(), row_ref.clone());
            middle.connect_pressed(move |gesture, _, _, _| {
                if let (Some(pane), Some(row)) = (w.upgrade(), r.upgrade()) {
                    if let Some(item) = row.item().as_ref().and_then(item_of) {
                        if item.is_dir() {
                            if let Some(host) = pane.host() {
                                host.new_tab(&item.file());
                            }
                        }
                    }
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });
            widget.add_controller(middle.clone());
            controllers.push(middle.upcast());
            let drag = gtk::DragSource::builder().actions(gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK).build();
            let (w, r) = (weak.clone(), row_ref.clone());
            drag.connect_prepare(move |_, _, _| {
                let (pane, row) = (w.upgrade()?, r.upgrade()?);
                let position = row.position();
                let selection = pane.selection();
                if position != gtk::INVALID_LIST_POSITION && !selection.is_selected(position) {
                    selection.select_item(position, true);
                }
                let files: Vec<gio::File> = pane.selected().iter().map(|i| i.file()).collect();
                if files.is_empty() {
                    return None;
                }
                Some(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()))
            });
            widget.add_controller(drag.clone());
            controllers.push(drag.upcast());
        }
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_accept(|target, incoming| configure_file_drop(target, incoming, false));
        {
            let (w, r, target_widget) = (weak.clone(), row_ref.clone(), widget.downgrade());
            let is_row = row.is_some();
            drop.connect_drop(move |target, value, x, y| {
                let Some(pane) = w.upgrade() else { return false };
                let Ok(list) = value.get::<gdk::FileList>() else { return false };
                let current = target.current_drop();
                let source_action = current.as_ref().and_then(|d| d.drag()).map(|drag| drag.selected_action());
                let offered = current.as_ref().map(|d| d.actions()).unwrap_or(gdk::DragAction::COPY);
                let item = if is_row { r.as_ref().and_then(|r| r.upgrade()).and_then(|row| row.item()).as_ref().and_then(item_of) } else { None };
                let modifiers = drop_modifiers(target);
                pane.drop_files(item.as_ref(), list.files(), source_action, offered, modifiers, target_widget.upgrade(), x, y)
            });
        }
        widget.add_controller(drop.clone());
        controllers.push(drop.upcast());
        let (w, r) = (weak.clone(), row_ref.clone());
        let is_row = row.is_some();
        let destination: Rc<dyn Fn() -> Option<gio::File>> = Rc::new(move || {
            let pane = w.upgrade()?;
            let item = if is_row { r.as_ref()?.upgrade()?.item().as_ref().and_then(item_of) } else { None };
            match item {
                Some(item) if !item.is_dir() => None,
                Some(item) => Some(item.file()),
                None => Some(pane.current()),
            }
        });
        let w = weak.clone();
        let report: Rc<dyn Fn(String)> = Rc::new(move |text| {
            if let Some(host) = w.upgrade().and_then(|p| p.host()) {
                host.message(&text)
            }
        });
        let ark = crate::ark_drop::attach_ark_drop(widget, destination, report);
        controllers.push(ark.upcast());
        tile::set_controllers(widget, controllers);
    }

    /// The `drop` handler body: files dropped onto a row (`item`) or the view.
    #[allow(clippy::too_many_arguments)]
    pub fn drop_files(&self, item: Option<&FileItem>, files: Vec<gio::File>, internal_action: Option<gdk::DragAction>,
                      offered: gdk::DragAction, modifiers: gdk::ModifierType, widget: Option<gtk::Widget>, x: f64, y: f64) -> bool {
        if item.is_some_and(|item| !item.is_dir()) {
            return false;
        }
        let destination = item.map(|i| i.file()).unwrap_or_else(|| self.current());
        let Some(host) = self.host() else { return false };
        match internal_action {
            None => {
                // Another application: the protocol only ever says COPY, so the
                // source never deletes; a chosen move is our own job.
                let kind = external_drop_choice(offered, modifiers);
                if kind == "ask" {
                    if files.is_empty() {
                        return false;
                    }
                    let Some(widget) = widget else { return false };
                    let weak_host = Rc::downgrade(&host);
                    let menu = drop_choice_menu(&widget, x, y, Box::new(move |kind| {
                        if let (Some(kind), Some(host)) = (kind, weak_host.upgrade()) {
                            host.run_operation(kind, files.clone(), Some(destination.clone()));
                        }
                    }));
                    LAST_DROP_MENU.with(|m| *m.borrow_mut() = Some(menu));
                    true
                } else {
                    host.run_operation(kind, files, Some(destination))
                }
            }
            Some(action) => host.run_operation(internal_drop_kind(action), files, Some(destination)),
        }
    }

    pub fn peek_schedule(&self, widget: &gtk::Widget, item: &FileItem) {
        self.peek_cancel();
        if self.closed() || !item.is_dir() || !pref_default_true(&self.preferences, "folder_peek") {
            return;
        }
        let (weak, target, item) = (self.downgrade(), widget.downgrade(), item.clone());
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(PEEK_DELAY as u64), move || {
            if let (Some(pane), Some(widget)) = (weak.upgrade(), target.upgrade()) {
                pane.state.borrow_mut().peek_source = None;
                pane.peek_open(&widget, &item);
            }
        });
        self.state.borrow_mut().peek_source = Some(source);
    }

    pub fn peek_source_pending(&self) -> bool {
        self.state.borrow().peek_source.is_some()
    }

    pub fn peek_popover(&self) -> Option<gtk::Popover> {
        self.state.borrow().peek_popover.clone()
    }

    pub fn peek_cancel(&self) {
        let (source, cancellable, popover) = {
            let mut s = self.state.borrow_mut();
            (s.peek_source.take(), s.peek_cancellable.take(), s.peek_popover.take())
        };
        if let Some(source) = source {
            source.remove();
        }
        if let Some(cancellable) = cancellable {
            cancellable.cancel();
        }
        if let Some(popover) = popover {
            popover.popdown();
            popover.unparent();
        }
    }

    /// List the hovered folder's first entries; nothing is retained after leaving.
    pub fn peek_open(&self, widget: &gtk::Widget, item: &FileItem) {
        self.state.borrow_mut().peek_source = None;
        if self.closed() || !widget.is_mapped() {
            return;
        }
        let cancellable = gio::Cancellable::new();
        self.state.borrow_mut().peek_cancellable = Some(cancellable.clone());
        let (weak, target, item2) = (self.downgrade(), widget.downgrade(), item.clone());
        item.file().enumerate_children_async("standard::display-name,standard::icon,standard::is-hidden,standard::type",
            gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT, Some(&cancellable.clone()), move |result| {
                let Ok(enumerator) = result else { return };
                let c = cancellable.clone();
                enumerator.clone().next_files_async(PEEK_LIMIT as i32 + 1, glib::Priority::DEFAULT, Some(&cancellable), move |result| {
                    let infos = result.ok();
                    enumerator.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
                    let (Some(infos), Some(pane), Some(widget)) = (infos, weak.upgrade(), target.upgrade()) else { return };
                    if !c.is_cancelled() && widget.is_mapped() {
                        pane.peek_show(&widget, &item2, infos);
                    }
                });
            });
    }

    fn peek_show(&self, widget: &gtk::Widget, item: &FileItem, infos: Vec<gio::FileInfo>) {
        let more = infos.len() > PEEK_LIMIT;
        let show_hidden = opt_bool(&self.state.borrow().options, "show_hidden", false);
        let mut infos: Vec<gio::FileInfo> = infos.into_iter().filter(|i| show_hidden || !i.is_hidden()).take(PEEK_LIMIT).collect();
        infos.sort_by_key(|i| (i.file_type() != gio::FileType::Directory, suite_core::pyjson::casefold(&i.display_name())));
        let popover = gtk::Popover::builder().autohide(false).can_focus(false).position(gtk::PositionType::Right).build();
        popover.add_css_class("files-peek");
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let title = gtk::Label::builder().label(item.display_name()).xalign(0.).ellipsize(pango::EllipsizeMode::End).max_width_chars(32).build();
        title.add_css_class("heading");
        column.append(&title);
        for info in &infos {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let icon = gtk::Image::new();
            if let Some(gicon) = info.icon() {
                icon.set_from_gicon(&gicon);
            }
            icon.set_pixel_size(16);
            row.append(&icon);
            row.append(&gtk::Label::builder().label(info.display_name()).xalign(0.).ellipsize(pango::EllipsizeMode::Middle).max_width_chars(32).build());
            column.append(&row);
        }
        if more || infos.is_empty() {
            let note = gtk::Label::builder().label(if more { "More items…" } else { "Empty folder" }).xalign(0.).build();
            note.add_css_class("muted");
            column.append(&note);
        }
        popover.set_child(Some(&column));
        popover.set_parent(widget);
        self.state.borrow_mut().peek_popover = Some(popover.clone());
        popover.popup();
    }

    fn selection_changed_cb(&self) {
        if self.views.visible_child_name().as_deref() == Some("columns") {
            self.columns().sync_trail();
        }
        if let Some(host) = self.host() {
            host.selection_changed(self);
        }
    }

    pub fn set_rubberband(&self, enabled: bool) {
        for (_, view) in self.view_widgets() {
            if Self::rubberband(&view) != enabled {
                Self::set_view_rubberband(&view, enabled);
            }
        }
    }

    pub fn focus_pane(&self) {
        if let Some(host) = self.host() {
            host.focus_pane(self);
        }
    }

    pub fn selected(&self) -> Vec<FileItem> {
        let selection = self.selection();
        let bits = selection.selection();
        (0..bits.size() as u32).filter_map(|i| model_item(&selection, bits.nth(i))).collect()
    }

    pub fn navigate(&self, location: &gio::File, history: bool, select: Option<Vec<String>>) {
        if self.closed() {
            return;
        }
        self.state.borrow_mut().navigate_calls += 1;
        if let Some(tick) = self.state.borrow_mut().scroll_restore.take() {
            tick.remove();
        }
        {
            let mut s = self.state.borrow_mut();
            s.scroll_positions.clear();
            s.reveal_selection = false;
        }
        self.peek_cancel();
        // Opening a folder by double-click tears down the pressed row before
        // its gesture can release, so restore rubberband selection here.
        self.set_rubberband(true);
        {
            let mut s = self.state.borrow_mut();
            if !location.equal(&s.current) {
                s.removal = None;
            }
            s.current = location.clone();
            s.extras_generation += 1;
            s.git_status.clear();
        }
        self.cancel_loading();
        self.stop_monitor();
        {
            let mut s = self.state.borrow_mut();
            let uri = s.current.uri().to_string();
            if history && (s.history_index < 0 || s.history[s.history_index as usize] != uri) {
                let keep = (s.history_index + 1) as usize;
                s.history.truncate(keep);
                s.history.push(uri);
                s.history_index = s.history.len() as i64 - 1;
            }
            s.searching = false;
        }
        let options = self.compute_options();
        let details = opt_str(&options, "view", "icons") == "details";
        self.state.borrow_mut().options = options.clone();
        *self.selection.borrow_mut() = if details { self.tree_selection.clone() } else { self.flat_selection.clone() };
        self.state.borrow_mut().pending_selection = select.unwrap_or_default().into_iter().collect();
        self.store.remove_all();
        self.filter.changed(gtk::FilterChange::Different);
        self.sorted.set_sorter(Some(&self.sorter));
        self.sorter.changed(gtk::SorterChange::Different);
        self.sync_header_sort();
        self.views.set_visible_child_name(opt_str(&options, "view", "icons"));
        if self.views.visible_child_name().as_deref() == Some("columns") && self.root.is_mapped() {
            self.columns().sync();
        }
        let single_click = opt_bool(&options, "single_click", false);
        for (_, view) in self.view_widgets() {
            Self::set_single_click(&view, single_click);
        }
        self.footer.set_visible(opt_bool(&options, "show_statusbar", true));
        self.set_location();
        self.begin_loading();
        let current = self.current();
        let (wb, wd) = (self.downgrade(), self.downgrade());
        self.loader.start(&current, move |batch: Vec<FileItem>| {
            if let Some(pane) = wb.upgrade() {
                pane.batch(batch)
            }
        }, move |error| {
            if let Some(pane) = wd.upgrade() {
                pane.loaded(error)
            }
        });
        if let Ok(monitor) = current.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>) {
            let weak = self.downgrade();
            let handler = monitor.connect_changed(move |monitor, file, other, _event| {
                if let Some(pane) = weak.upgrade() {
                    pane.changed(monitor, file, other);
                }
            });
            let mut s = self.state.borrow_mut();
            s.monitor = Some(monitor);
            s.monitor_handler = Some(handler);
        }
        if let Some(host) = self.host() {
            host.pane_changed(self);
        }
        self.load_git();
    }

    fn stop_monitor(&self) {
        let (monitor, handler) = {
            let mut s = self.state.borrow_mut();
            (s.monitor.take(), s.monitor_handler.take())
        };
        if let Some(monitor) = monitor {
            monitor.cancel();
            if let Some(handler) = handler {
                monitor.disconnect(handler);
            }
        }
    }

    fn set_git_column_visible(&self, visible: bool) {
        if let Some(column) = self.git_column.borrow().as_ref() {
            if column.is_visible() != visible {
                column.set_visible(visible);
            }
        }
    }

    pub fn git_column(&self) -> Option<gtk::ColumnViewColumn> {
        self.git_column.borrow().clone()
    }

    /// `_load_git`: one `git status` at a time on a worker; changes only
    /// update tooltips and the Git column (bound cells are preserved).
    pub fn load_git(&self) {
        if let Some(source) = self.state.borrow_mut().git_update_source.take() {
            source.remove();
        }
        if self.closed() {
            return;
        }
        let Some(path) = self.current().path() else {
            self.set_git_column_visible(false);
            self.state.borrow_mut().git_rendered.clear();
            return;
        };
        let generation = {
            let mut s = self.state.borrow_mut();
            if s.git_running {
                s.git_update_pending = true;
                return;
            }
            s.git_update_pending = false;
            s.git_last_started = Some(Instant::now());
            s.git_running = true;
            (s.extras_generation, s.git_generation, s.current.uri().to_string())
        };
        let weak = self.downgrade();
        let path = path.to_string_lossy().into_owned();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || bridge::status_for_directory(&path)).await;
            let Some(pane) = weak.upgrade() else { return };
            pane.state.borrow_mut().git_running = false;
            let (statuses, valid) = match result {
                Ok(Ok(statuses)) => (statuses, true),
                _ => (HashMap::new(), false),
            };
            let current = {
                let s = pane.state.borrow();
                (s.extras_generation, s.git_generation, s.current.uri().to_string())
            };
            if !pane.closed() && valid && generation == current {
                pane.apply_git(statuses);
            }
            let pending = pane.state.borrow().git_update_pending;
            if pending && !pane.closed() {
                pane.schedule_git_update();
            }
        });
    }

    fn apply_git(&self, statuses: HashMap<String, String>) {
        let changed = {
            let s = self.state.borrow();
            statuses != s.git_status || statuses != s.git_rendered
        };
        self.state.borrow_mut().git_status = statuses.clone();
        self.set_git_column_visible(!statuses.is_empty());
        if changed {
            // Preserve the bound file cells, their selection and hover state;
            // only update their existing tooltips.
            let by_uri: HashMap<String, String> = statuses.iter().map(|(p, s)| (gio::File::for_path(p).uri().to_string(), s.clone())).collect();
            let mut pending: Vec<gtk::Widget> = vec![self.icons.clone().upcast(), self.compact.clone().upcast(), self.details.clone().upcast()];
            while let Some(widget) = pending.pop() {
                if let Some(tile) = widget.downcast_ref::<FileTile>() {
                    if let Some(uri) = tile.bound_uri() {
                        let mut tooltip = gio::File::for_uri(&uri).parse_name().to_string();
                        if let Some(state) = by_uri.get(&uri) {
                            tooltip.push_str(&format!("\nGit: {state}"));
                        }
                        tile.set_tooltip_text(Some(&tooltip));
                    }
                }
                let mut child = widget.first_child();
                while let Some(current) = child {
                    child = current.next_sibling();
                    pending.push(current);
                }
            }
            if let Some(column) = self.git_column.borrow().as_ref() {
                column.set_factory(Some(&self.factory("git")));
            }
        }
        self.state.borrow_mut().git_rendered = statuses;
    }

    /// `_schedule_git_update`: at most one refresh every two seconds.
    pub fn schedule_git_update(&self) {
        if self.closed() || self.searching() || self.current().path().is_none() {
            return;
        }
        let remaining = {
            let mut s = self.state.borrow_mut();
            s.git_update_pending = true;
            if s.git_running || s.git_update_source.is_some() {
                return;
            }
            let elapsed = s.git_last_started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(f64::MAX);
            (2.0 - elapsed).max(0.)
        };
        let weak = self.downgrade();
        let delay = ((remaining * 1000.) as u64 + 1).max(1);
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let Some(pane) = weak.upgrade() else { return };
            pane.state.borrow_mut().git_update_source = None;
            let pending = pane.state.borrow().git_update_pending;
            if !pane.closed() && pending {
                pane.load_git();
            }
        });
        self.state.borrow_mut().git_update_source = Some(source);
    }

    fn cancel_git_update(&self) {
        let source = {
            let mut s = self.state.borrow_mut();
            s.git_generation += 1;
            s.git_update_pending = false;
            s.git_update_source.take()
        };
        if let Some(source) = source {
            source.remove();
        }
        // A running subprocess is bounded by status_for_directory's timeout;
        // its slot stays taken until its result arrives.
    }

    fn set_location(&self) {
        let current = self.current();
        self.location_completion().reset(&current.parse_name());
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }
        let home = home_file();
        let mut ancestors = Vec::new();
        let mut node = Some(current);
        while let Some(folder) = node {
            if ancestors.len() >= 6 {
                break;
            }
            ancestors.push(folder.clone());
            if folder.equal(&home) {
                break;
            }
            node = folder.parent();
        }
        for node in ancestors.iter().rev() {
            let mut text = node.basename().map(|b| b.to_string_lossy().into_owned()).filter(|t| !t.is_empty())
                .unwrap_or_else(|| node.parse_name().to_string());
            if node.equal(&home) {
                text = "Home".into();
            }
            let separator = chevron("right", 12);
            separator.add_css_class("files-crumb-arrow");
            self.breadcrumbs.append(&separator);
            let button = gtk::Button::with_label(&text);
            button.add_css_class("files-crumb");
            if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                label.set_ellipsize(pango::EllipsizeMode::End);
                label.set_max_width_chars(18);
            }
            button.set_tooltip_text(Some(&node.parse_name()));
            let (weak, location) = (self.downgrade(), node.clone());
            button.connect_clicked(move |_| {
                if let Some(pane) = weak.upgrade() {
                    pane.navigate(&location, true, None)
                }
            });
            if let Some(host) = self.host() {
                host.bind_location_menu(&button, node, self);
            }
            self.breadcrumbs.append(&button);
        }
        self.breadcrumbs.append(&self.location_blank);
        let editable = self.option_bool("location_editable");
        self.location_stack.set_visible_child_name(if editable { "entry" } else { "breadcrumbs" });
    }

    pub fn edit_location(&self) {
        self.focus_pane();
        self.location_stack.set_visible_child_name("entry");
        self.location.grab_focus();
        self.location.select_region(0, -1);
    }

    pub fn finish_location_edit(&self, focus_view: bool) {
        if self.closed() {
            return;
        }
        self.location_completion().reset(&self.current().parse_name());
        if !self.option_bool("location_editable") {
            self.location_stack.set_visible_child_name("breadcrumbs");
        }
        if focus_view {
            self.view_widget(None).grab_focus();
        }
    }

    fn activate_location(&self, entry: &gtk::Entry) {
        let path = entry.text();
        if !path.is_empty() {
            let target = files_model::completion::location_file(&path, &self.current());
            self.navigate(&target, true, None);
        }
        self.finish_location_edit(true);
    }

    pub fn back(&self) {
        let target = {
            let mut s = self.state.borrow_mut();
            if s.history_index <= 0 {
                return;
            }
            s.history_index -= 1;
            s.history[s.history_index as usize].clone()
        };
        self.navigate(&as_file(&target), false, None);
    }

    pub fn forward(&self) {
        let target = {
            let mut s = self.state.borrow_mut();
            if s.history_index + 1 >= s.history.len() as i64 {
                return;
            }
            s.history_index += 1;
            s.history[s.history_index as usize].clone()
        };
        self.navigate(&as_file(&target), false, None);
    }

    pub fn up(&self) {
        let current = self.current();
        if let Some(parent) = current.parent() {
            self.navigate(&parent, true, Some(vec![current.uri().to_string()]));
        }
    }

    pub fn refresh(&self) {
        self.state.borrow_mut().refresh_calls += 1;
        if self.searching() {
            self.search();
        } else {
            let selected = self.selected().iter().map(|i| i.uri()).collect();
            self.navigate(&self.current(), false, Some(selected));
        }
    }

    pub fn navigate_calls(&self) -> u32 {
        self.state.borrow().navigate_calls
    }
    pub fn refresh_calls(&self) -> u32 {
        self.state.borrow().refresh_calls
    }

    fn begin_loading(&self) {
        self.state.borrow_mut().loading = true;
        self.spinner.start();
        self.stop_button().set_visible(true);
        self.empty.set_visible(false);
        self.update_summary();
    }

    fn batch(&self, entries: Vec<FileItem>) {
        if !self.closed() {
            self.store.splice(self.store.n_items(), 0, &entries);
        }
    }

    fn loaded(&self, error: Option<glib::Error>) {
        if self.closed() {
            return;
        }
        self.state.borrow_mut().loading = false;
        self.spinner.stop();
        self.stop_button().set_visible(false);
        if let Some(error) = error {
            if let Some(host) = self.host() {
                if error.matches(gio::IOErrorEnum::NotMounted) {
                    let weak = self.downgrade();
                    host.mount_location(&self.current(), Box::new(move || {
                        if let Some(pane) = weak.upgrade() {
                            pane.refresh()
                        }
                    }));
                } else {
                    host.message(&suite_core::pyerr::python_error_text(&error));
                }
            }
        }
        if self.searching() && self.searcher.truncated() {
            if let Some(host) = self.host() {
                host.message("Search reached its result limit. Search a smaller folder or use a more specific query.");
            }
        }
        // Details uses a tree model; its row positions can differ from the
        // flat sorted model when a directory has been expanded.
        let pending = std::mem::take(&mut self.state.borrow_mut().pending_selection);
        let selection = self.selection();
        let mut first = None;
        for index in 0..selection.n_items() {
            if model_item(&selection, index).is_some_and(|item| pending.contains(&item.uri())) {
                selection.select_item(index, false);
                first.get_or_insert(index);
            }
        }
        if first.is_some() {
            let view = self.view_widget(None);
            if Self::view_model(&view).is_none() {
                self.state.borrow_mut().reveal_selection = true;
            } else {
                self.scroll_to_selection(&view);
            }
        }
        let select_first = std::mem::take(&mut self.state.borrow_mut().select_first);
        if select_first && first.is_none() && selection.n_items() > 0 {
            self.move_selection(1);
        }
        self.select_after_removal(true);
        self.update_summary();
        self.schedule_monitor_update();
    }

    fn changed(&self, monitor: &gio::FileMonitor, file: &gio::File, other: Option<&gio::File>) {
        {
            let s = self.state.borrow();
            if s.closed || s.searching || s.monitor.as_ref() != Some(monitor) {
                return;
            }
        }
        let current = self.current();
        let show_hidden = self.option_bool("show_hidden");
        for target in std::iter::once(file).chain(other) {
            if target.equal(&current) {
                self.state.borrow_mut().monitor_rescan = true;
                continue;
            }
            if !target.parent().is_some_and(|p| p.equal(&current)) {
                continue;
            }
            if !show_hidden && target.basename().is_some_and(|b| b.to_string_lossy().starts_with('.')) {
                continue;
            }
            let mut s = self.state.borrow_mut();
            if !s.monitor_rescan {
                let uri = target.uri().to_string();
                if !s.monitor_pending.iter().any(|(u, _)| *u == uri) {
                    s.monitor_pending.push((uri, target.clone()));
                }
            }
            if s.monitor_pending.len() > 1024 {
                // A bulk external operation gets one async reconciliation,
                // rather than an unbounded queue of individual metadata reads.
                s.monitor_pending.clear();
                s.monitor_rescan = true;
            }
        }
        self.schedule_monitor_update();
    }

    fn schedule_monitor_update(&self) {
        {
            let s = self.state.borrow();
            if s.closed || s.searching || s.loading || s.monitor_busy || s.reload_source.is_some()
                || (s.monitor_pending.is_empty() && !s.monitor_rescan) {
                return;
            }
        }
        // Coalesce bursts, but don't keep postponing a hot log indefinitely.
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(pane) = weak.upgrade() {
                pane.state.borrow_mut().reload_source = None;
                pane.update_monitored_entries();
            }
        });
        self.state.borrow_mut().reload_source = Some(source);
    }

    fn monitor_current(&self, generation: u64) -> bool {
        let s = self.state.borrow();
        !s.closed && generation == s.monitor_generation && !s.searching
    }

    fn monitor_finish(&self, generation: u64, entries: Vec<FileItem>, removed: HashSet<String>, complete: bool) {
        if !self.monitor_current(generation) {
            return;
        }
        let selected: HashSet<String> = self.selected().iter().map(|i| i.uri()).collect();
        let mut changed = false;
        for index in (0..self.store.n_items()).rev() {
            if self.store.item(index).and_downcast::<FileItem>().is_some_and(|item| removed.contains(&item.uri())) {
                self.store.remove(index);
                changed = true;
            }
        }
        changed = fm::reconcile_items(&self.store, &entries, complete) || changed;
        if changed {
            let selection = self.selection();
            for index in 0..selection.n_items() {
                if model_item(&selection, index).is_some_and(|item| selected.contains(&item.uri())) {
                    selection.select_item(index, false);
                }
            }
            self.select_after_removal(complete);
            if let Some(host) = self.host() {
                host.selection_changed(self);
            }
            self.schedule_git_update();
        }
        {
            let mut s = self.state.borrow_mut();
            s.monitor_cancellable = None;
            s.monitor_busy = false;
        }
        self.schedule_monitor_update();
    }

    fn update_monitored_entries(&self) {
        {
            let s = self.state.borrow();
            if s.closed || s.searching || s.loading || s.monitor_busy {
                return;
            }
        }
        let (generation, rescan) = {
            let mut s = self.state.borrow_mut();
            s.monitor_busy = true;
            let rescan = std::mem::take(&mut s.monitor_rescan);
            if rescan {
                s.monitor_pending.clear();
            }
            (s.monitor_generation, rescan)
        };
        if rescan {
            let entries: Rc<RefCell<Vec<FileItem>>> = Rc::default();
            let (collect, weak) = (entries.clone(), self.downgrade());
            self.monitor_loader.start(&self.current(), move |batch: Vec<FileItem>| collect.borrow_mut().extend(batch), move |error| {
                let Some(pane) = weak.upgrade() else { return };
                if !pane.monitor_current(generation) {
                    return;
                }
                match error {
                    Some(error) => {
                        if let Some(host) = pane.host() {
                            host.message(&suite_core::pyerr::python_error_text(&error));
                        }
                        pane.monitor_finish(generation, Vec::new(), HashSet::new(), false);
                    }
                    None => {
                        let items = std::mem::take(&mut *entries.borrow_mut());
                        pane.monitor_finish(generation, items, HashSet::new(), true);
                    }
                }
            });
            return;
        }
        let targets: Vec<gio::File> = {
            let mut s = self.state.borrow_mut();
            let take = s.monitor_pending.len().min(32);
            s.monitor_pending.drain(..take).map(|(_, f)| f).collect()
        };
        if targets.is_empty() {
            self.monitor_finish(generation, Vec::new(), HashSet::new(), false);
            return;
        }
        let cancellable = gio::Cancellable::new();
        self.state.borrow_mut().monitor_cancellable = Some(cancellable.clone());
        let collected: Rc<RefCell<(Vec<FileItem>, HashSet<String>, usize)>> = Rc::new(RefCell::new((Vec::new(), HashSet::new(), targets.len())));
        for target in targets {
            let (weak, collected, cancellable2) = (self.downgrade(), collected.clone(), cancellable.clone());
            let source = target.clone();
            target.query_info_async(fm::FILE_ATTRIBUTES, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT, Some(&cancellable),
                move |result| {
                    let Some(pane) = weak.upgrade() else { return };
                    match result {
                        Ok(info) => {
                            if pane.monitor_current(generation) {
                                collected.borrow_mut().0.push(FileItem::new(&source, &info));
                            }
                        }
                        Err(error) => {
                            if pane.monitor_current(generation) && error.matches(gio::IOErrorEnum::NotFound) {
                                collected.borrow_mut().1.insert(source.uri().to_string());
                            }
                            // Transient permission/backend errors must not remove a row.
                        }
                    }
                    if !pane.monitor_current(generation) {
                        return;
                    }
                    let done = {
                        let mut c = collected.borrow_mut();
                        c.2 -= 1;
                        c.2 == 0
                    };
                    if done {
                        let (entries, removed, _) = std::mem::take(&mut *collected.borrow_mut());
                        let weak = pane.downgrade();
                        fm::load_desktop_icons_async(entries, Some(&cancellable2), move |items: Vec<FileItem>| {
                            if let Some(pane) = weak.upgrade() {
                                pane.monitor_finish(generation, items, removed, false);
                            }
                        });
                    }
                });
        }
    }

    pub fn update_summary(&self) {
        let count = self.sorted.n_items();
        let selected = self.selected();
        let mut text = if selected.is_empty() {
            fm::item_count(count as i64)
        } else {
            let bytes: u64 = selected.iter().filter(|i| !i.is_dir()).map(|i| i.size()).sum();
            format!("{} selected · {}", selected.len(), fm::format_size(bytes))
        };
        let (loading, searching) = {
            let s = self.state.borrow();
            (s.loading, s.searching)
        };
        if loading {
            text.push_str(if searching { " · Searching…" } else { " · Loading…" });
        }
        self.summary.set_text(&text);
        self.empty.set_text(if searching || !self.filter_entry.text().is_empty() { "No matching files." } else { "This folder is empty." });
        self.empty.set_visible(!loading && count == 0);
    }

    pub fn activate(&self, item: Option<FileItem>) {
        let Some(item) = item else { return };
        if item.info().file_type() == gio::FileType::Mountable {
            let weak = self.downgrade();
            let operation = self.host().and_then(|h| h.mount_operation());
            item.file().mount_mountable(gio::MountMountFlags::NONE, operation.as_ref(), None::<&gio::Cancellable>, move |result| {
                let Some(pane) = weak.upgrade() else { return };
                match result {
                    Ok(location) => {
                        if !pane.closed() {
                            pane.navigate(&location, true, None)
                        }
                    }
                    Err(error) => {
                        if let Some(host) = pane.host() {
                            host.message(error.message())
                        }
                    }
                }
            });
        } else if item.is_dir() {
            self.navigate(&item.file(), true, None);
        } else if let Some(host) = self.host() {
            host.open_files(vec![item.file()]);
        }
    }

    pub fn show_search(&self) {
        self.search_box.set_visible(true);
        self.search_entry.grab_focus();
    }

    pub fn search(&self) {
        let query = self.search_entry.text().trim().to_string();
        if query.is_empty() {
            return;
        }
        self.cancel_loading();
        self.state.borrow_mut().searching = true;
        self.store.remove_all();
        self.begin_loading();
        let (wb, wd) = (self.downgrade(), self.downgrade());
        self.searcher.start(&self.current(), &query, move |batch: Vec<FileItem>| {
            if let Some(pane) = wb.upgrade() {
                pane.batch(batch)
            }
        }, move |error| {
            if let Some(pane) = wd.upgrade() {
                pane.loaded(error)
            }
        }, fm::SearchOptions { contents: self.contents_check.is_active(), include_hidden: self.option_bool("show_hidden"), ..Default::default() });
    }

    pub fn stop_search(&self) {
        self.search_box.set_visible(false);
        if !self.searching() {
            self.refresh();
        } else {
            self.navigate(&self.current(), false, None);
        }
    }

    pub fn show_filter(&self, show: bool) {
        self.filter_entry.set_visible(show);
        if show {
            self.filter_entry.grab_focus();
        } else {
            self.filter_entry.set_text("");
        }
    }

    pub fn cancel_loading(&self) {
        self.cancel_git_update();
        let (cancellable, loaders, reload) = {
            let mut s = self.state.borrow_mut();
            s.monitor_generation += 1;
            s.monitor_pending.clear();
            s.monitor_rescan = false;
            s.monitor_busy = false;
            s.loading = false;
            (s.monitor_cancellable.take(), std::mem::take(&mut s.child_loaders), s.reload_source.take())
        };
        if let Some(cancellable) = cancellable {
            cancellable.cancel();
        }
        self.monitor_loader.cancel();
        self.loader.cancel();
        self.searcher.cancel();
        for loader in loaders {
            loader.cancel();
        }
        self.spinner.stop();
        if let Some(stop) = self.stop_button.borrow().as_ref() {
            stop.set_visible(false);
        }
        if let Some(source) = reload {
            source.remove();
        }
    }

    pub fn close(&self) {
        if self.closed() {
            return;
        }
        self.state.borrow_mut().closed = true;
        self.peek_cancel();
        self.sync_view_models();
        let connections = std::mem::take(&mut self.state.borrow_mut().connections);
        for (object, id) in connections {
            object.disconnect(id);
        }
        if let Some(completion) = self.location_completion.borrow().as_ref() {
            completion.close();
        }
        self.state.borrow_mut().extras_generation += 1;
        self.cancel_loading();
        self.stop_monitor();
        self.filter.set_filter_func(|_| false);
        self.sorter.set_sort_func(|_, _| gtk::Ordering::Equal);
        self.store.remove_all();
        for (_, view) in self.view_widgets() {
            tile::remove_controllers(&view);
        }
        self.columns().close();
        self.icons.set_factory(None::<&gtk::ListItemFactory>);
        self.compact.set_factory(None::<&gtk::ListItemFactory>);
        let columns = self.details.columns();
        for index in 0..columns.n_items() {
            if let Some(column) = columns.item(index).and_downcast::<gtk::ColumnViewColumn>() {
                column.set_factory(None::<&gtk::ListItemFactory>);
            }
        }
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }
        *self.host.borrow_mut() = None;
    }
}

thread_local! {
    /// The most recent drop choice menu (test hook; Python patches drop_choice_menu).
    pub static LAST_DROP_MENU: RefCell<Option<DropMenu>> = const { RefCell::new(None) };
}

pub fn last_drop_menu() -> Option<DropMenu> {
    LAST_DROP_MENU.with(|m| m.borrow().clone())
}

/// `prefs.get(key, True)` for options that default to on.
pub fn pref_default_true(prefs: &Prefs, key: &str) -> bool {
    match bridge::pget(prefs, key) {
        Value::Null => true,
        value => suite_core::pyjson::truthy(&value),
    }
}

/// Python `str.isprintable()` for one character.
pub fn is_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !(c.is_control() || c.is_whitespace() || matches!(c as u32, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        | 0xE000..=0xF8FF | 0xD800..=0xDFFF))
}
