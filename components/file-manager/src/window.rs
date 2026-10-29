//! files_ui.FolderTab and files_ui.FilesWindow: tabs, split panes, Places,
//! file actions, context menus and background job controls.

use crate::app::{FilesApplication, WeakApp};
use crate::bridge;
use crate::goto::GoAnywhere;
use crate::keys::latin_keyval;
use crate::pane::{self, as_file, chevron, home_file, tool, tool_icon, BrowserPane, PaneHost};
use crate::tile;
use files_model::model::{DirectoryLoader, FileItem};
use files_ops::operations::{Callbacks, Conflict, OperationResult, Progress, Resolve};
use files_widgets::compat::Item;
use files_widgets::dialogs::{self, ChooserMode, PropertiesWindow};
use files_widgets::information::InformationPane;
use files_widgets::jobs::{self as jobs, JobEntry, JobHandle, JobsButton, KeepAnswer};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, graphene, pango};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use suite_core::tr;

pub const ACTIONS: &str = "new-tab new-window close-tab reopen-tab split focus-other location back forward up refresh home search \
filter hidden copy cut paste select-all invert-selection trash delete rename new-folder new-file \
undo properties bookmark zoom-in zoom-out information sidebar icons compact details previews \
open open-with duplicate copy-to move-to link restore empty-trash connect preferences \
compress extract templates sort-name sort-size sort-type sort-modified descending \
folder-first remember-folders save-search browse-archive open-new-tab open-terminal \
columns go-anywhere print quick-look";

const CHECKS: [(&str, &str); 6] = [("hidden", "show_hidden"), ("previews", "show_previews"), ("information", "show_information"),
                                   ("sidebar", "show_sidebar"), ("descending", "descending"), ("folder-first", "folders_first")];

pub const ARCHIVE_SUFFIXES: [&str; 9] = [".zip", ".tar", ".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".iso", ".7z", ".rar"];

fn file_label(file: &gio::File) -> String {
    file.basename().map(|b| b.to_string_lossy().into_owned()).filter(|b| !b.is_empty()).unwrap_or_else(|| file.parse_name().to_string())
}

// ------------------------------------------------------------------ FolderTab

pub struct TabInner {
    pub paned: gtk::Paned,
    window: Weak<WindowInner>,
    pub left: BrowserPane,
    pub right: RefCell<Option<BrowserPane>>,
    pub label: gtk::Label,
}

#[derive(Clone)]
pub struct FolderTab(pub Rc<TabInner>);

impl PartialEq for FolderTab {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for FolderTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FolderTab({})", self.left.current().uri())
    }
}

impl std::ops::Deref for FolderTab {
    type Target = TabInner;
    fn deref(&self) -> &TabInner {
        &self.0
    }
}

impl FolderTab {
    fn new(window: &FilesWindow, location: &gio::File) -> Self {
        let paned = gtk::Paned::builder().orientation(gtk::Orientation::Horizontal).wide_handle(true).hexpand(true).vexpand(true).build();
        let host: Weak<dyn PaneHost> = Rc::downgrade(&window.0) as Weak<dyn PaneHost>;
        let app = window.app();
        let left = BrowserPane::new(host, location, app.preferences(), app.thumbnails.clone());
        paned.set_start_child(Some(left.widget()));
        paned.set_resize_start_child(true);
        paned.set_shrink_start_child(false);
        let label = gtk::Label::builder().label(location.basename().map(|b| b.to_string_lossy().into_owned()).filter(|b| !b.is_empty())
            .unwrap_or_else(|| "Files".into())).ellipsize(pango::EllipsizeMode::End).width_chars(12).max_width_chars(24).xalign(0.).build();
        FolderTab(Rc::new(TabInner { paned, window: Rc::downgrade(&window.0), left, right: RefCell::new(None), label }))
    }

    pub fn right(&self) -> Option<BrowserPane> {
        self.right.borrow().clone()
    }

    pub fn set_split(&self, enabled: bool, location: Option<&gio::File>) {
        let Some(window) = self.window.upgrade().map(FilesWindow) else { return };
        if enabled && self.right().is_none() {
            let host: Weak<dyn PaneHost> = Rc::downgrade(&window.0) as Weak<dyn PaneHost>;
            let app = window.app();
            let start = location.cloned().unwrap_or_else(|| self.left.current());
            let right = BrowserPane::new(host, &start, app.preferences(), app.thumbnails.clone());
            self.paned.add_css_class("files-split");
            self.paned.set_end_child(Some(right.widget()));
            self.paned.set_resize_end_child(true);
            self.paned.set_shrink_end_child(false);
            self.paned.set_position(300.max(self.paned.width() / 2));
            *self.right.borrow_mut() = Some(right.clone());
            window.set_active_pane(&right);
        } else if !enabled {
            if let Some(right) = self.right.borrow_mut().take() {
                right.close();
                self.paned.set_end_child(None::<&gtk::Widget>);
                self.paned.remove_css_class("files-split");
                window.set_active_pane(&self.left);
            }
        }
        window.save_session(false);
    }

    pub fn close(&self) {
        self.left.close();
        if let Some(right) = self.right() {
            right.close();
        }
    }

    pub fn panes(&self) -> Vec<BrowserPane> {
        let mut panes = vec![self.left.clone()];
        panes.extend(self.right());
        panes
    }
}

// ---------------------------------------------------------------- FilesWindow

pub struct WindowInner {
    pub window: gtk::ApplicationWindow,
    app: WeakApp,
    pub active_pane: RefCell<Option<BrowserPane>>,
    pub closed_tabs: RefCell<Vec<(String, Option<String>)>>,
    pub alive: Cell<bool>,
    pub popover: RefCell<Option<gtk::Popover>>,
    popover_connections: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    popover_cleanup: RefCell<Option<glib::SourceId>>,
    pub quicklook: RefCell<Option<files_widgets::quicklook::QuickLook>>,
    pub goto: RefCell<Option<GoAnywhere>>,
    pub custom_actions: files_ops::extras::CustomActions,
    custom_action_ids: RefCell<Vec<String>>,
    information_width_source: RefCell<Option<glib::SourceId>>,
    restoring_information_width: Cell<bool>,
    session_source: RefCell<Option<glib::SourceId>>,
    pub close_confirmed: Cell<bool>,
    pub tabs: RefCell<Vec<FolderTab>>,
    pub header: gtk::HeaderBar,
    pub sidebar_toggle: gtk::ToggleButton,
    pub menu_button: RefCell<Option<gtk::MenuButton>>,
    pub jobs_button: JobsButton,
    pub root: gtk::Box,
    pub location_host: gtk::Box,
    pub view_buttons: RefCell<Vec<(String, gtk::ToggleButton)>>,
    syncing_views: Cell<bool>,
    pub information_toggle: gtk::ToggleButton,
    pub error_row: gtk::Box,
    pub error_label: gtk::Label,
    pub sidebar: gtk::Box,
    pub sidebar_scroll: gtk::ScrolledWindow,
    pub navigation_size: gtk::SizeGroup,
    pub notebook: gtk::Notebook,
    pub content_split: gtk::Paned,
    pub information: InformationPane,
    pub volumes: gio::VolumeMonitor,
    pub disk_usage: RefCell<Vec<files_widgets::devices::DiskUsage>>,
    pub mount_monitor: RefCell<Option<glib::Object>>,
    mount_signal: RefCell<Option<glib::SignalHandlerId>>,
    volume_signals: RefCell<Vec<glib::SignalHandlerId>>,
    disk_usage_timer: RefCell<Option<glib::SourceId>>,
    trash_loader: RefCell<Option<DirectoryLoader>>,
    switch_signal: RefCell<Option<glib::SignalHandlerId>>,
}

#[derive(Clone)]
pub struct FilesWindow(pub Rc<WindowInner>);

impl PartialEq for FilesWindow {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::ops::Deref for FilesWindow {
    type Target = WindowInner;
    fn deref(&self) -> &WindowInner {
        &self.0
    }
}

pub const SHORTCUTS: &[(&str, &[(&[&str], &str)])] = &[
    ("Windows and tabs", &[(&["<Control>n"], "New window"), (&["<Control>t"], "New tab"), (&["<Control>w"], "Close tab"),
                           (&["<Control><Shift>t"], "Reopen closed tab"), (&["F3"], "Split view"), (&["<Control>Tab"], "Switch pane")]),
    ("Navigation", &[(&["<Alt>Left"], "Back"), (&["<Alt>Right"], "Forward"), (&["<Alt>Up"], "Parent folder"), (&["<Alt>Home"], "Home folder"),
                     (&["<Control>l"], "Edit location"), (&["<Control>k"], "Go anywhere"), (&["F5"], "Reload"),
                     (&["<Control>f"], "Search"), (&["<Control>i"], "Filter"), (&["<Control>d"], "Bookmark this folder")]),
    ("Files", &[(&["<Control>c"], "Copy"), (&["<Control>x"], "Cut"), (&["<Control>v"], "Paste"), (&["<Control>z"], "Undo"),
                (&["<Control>a"], "Select all"), (&["<Control><Shift>a"], "Invert selection"), (&["F2"], "Rename"),
                (&["Delete"], "Move to Trash"), (&["<Shift>Delete"], "Delete permanently"), (&["<Control><Shift>n"], "New folder"),
                (&["<Alt>Return"], "Properties"), (&["<Control>p"], "Print")]),
    ("View", &[(&["<Control>h", "<Alt>period"], "Show hidden files"), (&["<Control>plus", "<Control>minus"], "Zoom in / out"),
               (&["<Control>1"], "Icons"), (&["<Control>2"], "Compact"), (&["<Control>3"], "Details"), (&["<Control>4"], "Columns"),
               (&["F9"], "Places pane"), (&["F11"], "Preview & information pane")]),
];

fn shortcut_sections() -> Vec<suite_gtk::app_menu::ShortcutSection> {
    SHORTCUTS.iter().map(|(title, entries)| {
        (title.to_string(), entries.iter().map(|(keys, label)| {
            if keys.len() == 1 {
                suite_gtk::app_menu::Shortcut::new(keys[0], label)
            } else {
                suite_gtk::app_menu::Shortcut::any(keys, label)
            }
        }).collect())
    }).collect()
}

impl FilesWindow {
    pub fn new(app: &FilesApplication) -> Self {
        let prefs = app.preferences();
        let size = bridge::pget(&prefs, "window_size");
        let width = size.get(0).and_then(Value::as_i64).unwrap_or(1100) as i32;
        let height = size.get(1).and_then(Value::as_i64).unwrap_or(760) as i32;
        let window = gtk::ApplicationWindow::builder().application(&app.app).title("Files").default_width(width).default_height(height).build();
        window.add_css_class("files-window");
        // Retain the live per-window binding even when this window is created
        // by an already-running application's remote command-line activation.
        let theme = suite_gtk::theme::attach(&window, None);
        unsafe { window.set_data("wayfiles-theme", theme) };
        let header = suite_gtk::utils::headerbar("Files");
        window.set_titlebar(Some(&header));
        let sidebar_toggle = gtk::ToggleButton::builder().child(&suite_gtk::utils::image("sidebar-show-symbolic", 16))
            .tooltip_text("Show or hide Places pane — F9").action_name("win.sidebar").build();
        let jobs_button = JobsButton::new(&app.jobs);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let location_host = gtk::Box::builder().hexpand(true).build();
        let information_toggle = gtk::ToggleButton::builder().child(&suite_gtk::utils::image("sidebar-show-right-symbolic", 16))
            .tooltip_text("Show or hide preview pane — F11").action_name("win.information").build();
        let error_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        error_row.add_css_class("files-error");
        let error_label = gtk::Label::builder().xalign(0.).wrap(true).hexpand(true).selectable(true).build();
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("files-sidebar");
        suite_gtk::utils::sidebar_style(&sidebar);
        let sidebar_scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).min_content_width(200).hexpand(false).build();
        sidebar_scroll.set_child(Some(&sidebar));
        sidebar_scroll.set_visible(bridge::pbool(&prefs, "show_sidebar"));
        let navigation_size = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let notebook = gtk::Notebook::builder().scrollable(true).hexpand(true).vexpand(true).show_tabs(false).show_border(false).build();
        notebook.add_css_class("files-tabs");
        let content_split = gtk::Paned::builder().orientation(gtk::Orientation::Horizontal).hexpand(true).vexpand(true).wide_handle(true).build();
        let weak_slot: Rc<RefCell<Weak<WindowInner>>> = Rc::new(RefCell::new(Weak::new()));
        let (hide_slot, print_slot) = (weak_slot.clone(), weak_slot.clone());
        let information = InformationPane::new(move || {
            if let Some(window) = hide_slot.borrow().upgrade() {
                FilesWindow(window).action("information");
            }
        }, Some(Rc::new(move |item: Item| {
            if let Some(window) = print_slot.borrow().upgrade() {
                FilesWindow(window).print_file(Some(item));
            }
        })));
        let inner = Rc::new(WindowInner {
            window, app: app.downgrade(), active_pane: RefCell::new(None), closed_tabs: RefCell::new(Vec::new()), alive: Cell::new(true),
            popover: RefCell::new(None), popover_connections: RefCell::new(Vec::new()), popover_cleanup: RefCell::new(None),
            quicklook: RefCell::new(None), goto: RefCell::new(None), custom_actions: files_ops::extras::CustomActions::new(None),
            custom_action_ids: RefCell::new(Vec::new()), information_width_source: RefCell::new(None),
            restoring_information_width: Cell::new(false), session_source: RefCell::new(None), close_confirmed: Cell::new(false),
            tabs: RefCell::new(Vec::new()), header, sidebar_toggle, menu_button: RefCell::new(None), jobs_button, root, location_host,
            view_buttons: RefCell::new(Vec::new()), syncing_views: Cell::new(false), information_toggle, error_row, error_label, sidebar,
            sidebar_scroll, navigation_size, notebook, content_split, information, volumes: gio::VolumeMonitor::get(),
            disk_usage: RefCell::new(Vec::new()), mount_monitor: RefCell::new(None), mount_signal: RefCell::new(None),
            volume_signals: RefCell::new(Vec::new()), disk_usage_timer: RefCell::new(None), trash_loader: RefCell::new(None),
            switch_signal: RefCell::new(None),
        });
        *weak_slot.borrow_mut() = Rc::downgrade(&inner);
        let window = FilesWindow(inner);
        jobs::set_window_alive(&window.window, true);
        app.register_window(&window);
        window.build(app);
        window
    }

    pub fn downgrade(&self) -> Weak<WindowInner> {
        Rc::downgrade(&self.0)
    }

    pub fn app(&self) -> FilesApplication {
        self.app.upgrade().expect("application")
    }

    pub fn gtk_window(&self) -> gtk::Window {
        self.window.clone().upcast()
    }

    fn weak_tool(&self, icon: impl IsA<gtk::Widget>, title: &str, name: &'static str) -> gtk::Button {
        let weak = self.downgrade();
        tool(icon, title, move || {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).action(name)
            }
        })
    }

    fn build(&self, app: &FilesApplication) {
        self.actions(app);
        self.header.pack_start(&self.weak_tool(suite_gtk::utils::image("list-add-symbolic", 16), "New tab", "new-tab"));
        self.header.pack_start(&self.sidebar_toggle);
        self.header.pack_end(&self.weak_tool(suite_gtk::utils::image("view-dual-symbolic", 16), "Split view — F3", "split"));
        let menu = gtk::MenuButton::builder().icon_name("open-menu-symbolic").tooltip_text("File manager menu").build();
        menu.set_menu_model(Some(&main_menu()));
        let weak_app = app.downgrade();
        let menu = suite_gtk::app_menu::install_app_menu(&app.app, &self.window, suite_gtk::app_menu::MenuTarget::Button(menu),
            suite_gtk::app_menu::AboutDetails { name: "Files".into(), icon: "system-file-manager".into(),
                comments: Some("Browse, organize and share files".into()), ..Default::default() },
            shortcut_sections(), None, Some(Rc::new(move || {
                if let Some(app) = weak_app.upgrade() {
                    crate::app::quit_files(&app);
                }
            })));
        self.header.pack_end(&menu);
        *self.menu_button.borrow_mut() = Some(menu);
        self.header.pack_end(self.jobs_button.widget());
        self.window.set_child(Some(&self.root));
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        toolbar.add_css_class("files-toolbar");
        let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        navigation.add_css_class("files-navigation-buttons");
        navigation.append(&self.weak_tool(chevron("left", 16), "Back", "back"));
        navigation.append(&self.weak_tool(chevron("right", 16), "Forward", "forward"));
        navigation.append(&self.weak_tool(chevron("up", 16), "Parent folder", "up"));
        navigation.append(&self.weak_tool(suite_gtk::utils::image("view-refresh-symbolic", 16), "Reload", "refresh"));
        toolbar.append(&navigation);
        toolbar.append(&self.location_host);
        let view_controls = gtk::Box::builder().spacing(4).margin_start(8).margin_end(12).build();
        toolbar.append(&view_controls);
        for (icon, title, action) in [("view-grid-symbolic", "Icons", "icons"), ("view-list-symbolic", "Compact", "compact"),
                                      ("view-list-details-symbolic", "Details", "details"), ("view-column-symbolic", "Columns", "columns"),
                                      ("system-search-symbolic", "Search", "search")] {
            if action == "search" {
                view_controls.append(&self.weak_tool(suite_gtk::utils::image(icon, 16), title, "search"));
                continue;
            }
            let button = gtk::ToggleButton::builder().child(&suite_gtk::utils::image(icon, 16)).tooltip_text(title).build();
            button.update_property(&[gtk::accessible::Property::Label(title)]);
            if let Some((_, first)) = self.view_buttons.borrow().first() {
                button.set_group(Some(first));
            }
            let weak = self.downgrade();
            button.connect_toggled(move |widget| {
                if let Some(window) = weak.upgrade().map(FilesWindow) {
                    if widget.is_active() && !window.syncing_views.get() {
                        window.action(action);
                    }
                }
            });
            self.view_buttons.borrow_mut().push((action.to_string(), button.clone()));
            view_controls.append(&button);
        }
        view_controls.append(&self.information_toggle);
        self.root.append(&toolbar);
        self.error_row.append(&suite_gtk::utils::image("dialog-warning-symbolic", 16));
        self.error_row.append(&self.error_label);
        let row = self.error_row.downgrade();
        self.error_row.append(&tool_icon("window-close-symbolic", "Dismiss", move || {
            if let Some(row) = row.upgrade() {
                row.set_visible(false)
            }
        }));
        self.error_row.set_visible(false);
        self.root.append(&self.error_row);
        let body = gtk::Box::builder().hexpand(true).vexpand(true).build();
        self.root.append(&body);
        // Keep the address field flush with the folder column.
        self.navigation_size.add_widget(&navigation);
        self.navigation_size.add_widget(&self.sidebar_scroll);
        body.append(&self.sidebar_scroll);
        let weak = self.downgrade();
        let id = self.notebook.connect_switch_page(move |_, page, _| {
            if let Some(window) = weak.upgrade().map(FilesWindow) {
                if let Some(tab) = window.tab_for_widget(page) {
                    window.set_active_pane(&tab.left);
                }
                window.save_session(false);
            }
        });
        *self.switch_signal.borrow_mut() = Some(id);
        let weak = self.downgrade();
        self.notebook.connect_page_reordered(move |notebook, _, _| {
            if let Some(window) = weak.upgrade().map(FilesWindow) {
                window.sync_tab_order(notebook);
                window.save_session(false);
            }
        });
        self.content_split.set_start_child(Some(&self.notebook));
        self.content_split.set_resize_start_child(true);
        self.content_split.set_shrink_start_child(false);
        self.content_split.set_end_child(Some(self.information.widget()));
        self.content_split.set_resize_end_child(false);
        self.content_split.set_shrink_end_child(false);
        body.append(&self.content_split);
        self.information.widget().set_visible(bridge::pbool(&app.preferences(), "show_information"));
        let weak = self.downgrade();
        self.content_split.connect_map(move |_| {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).restore_information_width()
            }
        });
        let weak = self.downgrade();
        self.content_split.connect_position_notify(move |_| {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).information_resized()
            }
        });
        let monitor = crate::app::unix_mount_monitor();
        if let Some(monitor) = &monitor {
            let weak = self.downgrade();
            let id = monitor.connect_local("mounts-changed", false, move |_| {
                if let Some(window) = weak.upgrade() {
                    FilesWindow(window).refresh_sidebar()
                }
                None
            });
            *self.mount_signal.borrow_mut() = Some(id);
        }
        *self.mount_monitor.borrow_mut() = monitor;
        let mut signals = Vec::new();
        for signal in ["mount-added", "mount-removed", "mount-changed", "volume-added", "volume-removed", "volume-changed"] {
            let weak = self.downgrade();
            signals.push(self.volumes.connect_local(signal, false, move |_| {
                if let Some(window) = weak.upgrade() {
                    FilesWindow(window).refresh_sidebar()
                }
                None
            }));
        }
        *self.volume_signals.borrow_mut() = signals;
        self.refresh_sidebar();
        let weak = self.downgrade();
        let timer = glib::timeout_add_seconds_local(15, move || match weak.upgrade() {
            Some(window) => FilesWindow(window).refresh_disk_usage(),
            None => glib::ControlFlow::Break,
        });
        *self.disk_usage_timer.borrow_mut() = Some(timer);
        let weak = self.downgrade();
        self.window.connect_close_request(move |_| match weak.upgrade() {
            Some(window) => FilesWindow(window).close_requested(),
            None => glib::Propagation::Proceed,
        });
        let keys = gtk::EventControllerKey::new();
        let weak = self.downgrade();
        keys.connect_key_pressed(move |_, key, code, state| match weak.upgrade() {
            Some(window) if FilesWindow(window.clone()).key_pressed(key, code, state) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        self.window.add_controller(keys);
    }

    fn actions(&self, app: &FilesApplication) {
        let prefs = app.preferences();
        for name in ACTIONS.split_whitespace() {
            let action = match CHECKS.iter().find(|(n, _)| *n == name) {
                Some((_, key)) => gio::SimpleAction::new_stateful(name, None, &bridge::pbool(&prefs, key).to_variant()),
                None => gio::SimpleAction::new(name, None),
            };
            let (weak, key) = (self.downgrade(), name.to_string());
            action.connect_activate(move |_, _| {
                if let Some(window) = weak.upgrade() {
                    FilesWindow(window).action(&key)
                }
            });
            self.window.add_action(&action);
        }
    }

    pub fn lookup_action(&self, name: &str) -> gio::SimpleAction {
        self.window.lookup_action(name).and_downcast::<gio::SimpleAction>().expect("window action")
    }

    pub fn pages(&self) -> Vec<FolderTab> {
        let tabs = self.tabs.borrow();
        (0..self.notebook.n_pages())
            .filter_map(|i| self.notebook.nth_page(Some(i)))
            .filter_map(|page| tabs.iter().find(|t| t.paned.upcast_ref::<gtk::Widget>() == &page).cloned())
            .collect()
    }

    fn sync_tab_order(&self, _notebook: &gtk::Notebook) {
        let ordered = self.pages();
        *self.tabs.borrow_mut() = ordered;
    }

    fn tab_for_widget(&self, widget: &gtk::Widget) -> Option<FolderTab> {
        self.tabs.borrow().iter().find(|t| t.paned.upcast_ref::<gtk::Widget>() == widget).cloned()
    }

    pub fn current_page(&self) -> Option<FolderTab> {
        let index = self.notebook.current_page()?;
        self.tab_for_widget(&self.notebook.nth_page(Some(index))?)
    }

    pub fn active_pane(&self) -> Option<BrowserPane> {
        self.active_pane.borrow().clone()
    }

    pub fn new_tab(&self, location: Option<&gio::File>, select: Option<Vec<String>>) -> FolderTab {
        let position = self.notebook.current_page().map(|p| p as i32).unwrap_or(-1) + 1;
        let location = location.cloned().or_else(|| self.active_pane().map(|p| p.current())).unwrap_or_else(home_file);
        let page = FolderTab::new(self, &location);
        self.tabs.borrow_mut().push(page.clone());
        let label = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        label.add_css_class("files-tab-label");
        label.append(&page.label);
        // Find the page from its tab label when clicked.
        let label_ref = label.downgrade();
        let close = tool_icon("window-close-symbolic", "Close tab", move || {
            let Some(label) = label_ref.upgrade() else { return };
            let Some(window) = crate::app::files_window_for(&label) else { return };
            let page = window.pages().into_iter().find(|p| window.notebook.tab_label(&p.paned).as_ref() == Some(label.upcast_ref()));
            if let Some(page) = page {
                window.close_tab(Some(&page));
            }
        });
        close.add_css_class("files-tab-close");
        if let Some(image) = close.child().and_downcast::<gtk::Image>() {
            image.set_pixel_size(14);
        }
        label.append(&close);
        let index = self.notebook.insert_page(&page.paned, Some(&label), Some(position as u32));
        self.notebook.set_show_tabs(self.notebook.n_pages() > 1);
        self.notebook.set_tab_reorderable(&page.paned, true);
        self.notebook.set_current_page(Some(index));
        self.sync_tab_order(&self.notebook);
        self.set_active_pane(&page.left);
        if let Some(select) = select {
            page.left.set_pending_selection(select);
        }
        self.save_session(false);
        page
    }

    pub fn close_tab(&self, page: Option<&FolderTab>) {
        let Some(page) = page.cloned().or_else(|| self.current_page()) else { return };
        if self.notebook.n_pages() == 1 {
            self.window.close();
            return;
        }
        {
            let mut closed = self.closed_tabs.borrow_mut();
            closed.push((page.left.current().uri().to_string(), page.right().map(|r| r.current().uri().to_string())));
            let excess = closed.len().saturating_sub(20);
            closed.drain(..excess);
        }
        page.close();
        if let Some(number) = self.notebook.page_num(&page.paned) {
            self.notebook.remove_page(Some(number));
        }
        self.tabs.borrow_mut().retain(|t| t != &page);
        self.notebook.set_show_tabs(self.notebook.n_pages() > 1);
        self.save_session(false);
    }

    pub fn set_active_pane(&self, pane: &BrowserPane) {
        if self.active_pane().as_ref() == Some(pane) {
            return;
        }
        if let Some(previous) = self.active_pane() {
            previous.widget().remove_css_class("active-pane");
        }
        if let Some(previous) = self.location_host.first_child() {
            self.location_host.remove(&previous);
        }
        *self.active_pane.borrow_mut() = Some(pane.clone());
        if let Some(parent) = pane.location_box.parent() {
            if let Some(parent) = parent.downcast_ref::<gtk::Box>() {
                parent.remove(&pane.location_box);
            }
        }
        self.location_host.append(&pane.location_box);
        pane.widget().add_css_class("active-pane");
        self.pane_changed_impl(pane);
    }

    fn pane_changed_impl(&self, pane: &BrowserPane) {
        for page in self.pages() {
            if pane == &page.left || page.right().as_ref() == Some(pane) {
                page.label.set_text(&file_label(&page.left.current()));
            }
        }
        if self.active_pane().as_ref() == Some(pane) {
            self.window.set_title(Some(&format!("{} — Files", file_label(&pane.current()))));
            self.syncing_views.set(true);
            let view = pane.option_str("view", "icons");
            let buttons = self.view_buttons.borrow().clone();
            let button = buttons.iter().find(|(n, _)| *n == view).or_else(|| buttons.iter().find(|(n, _)| n == "icons"));
            if let Some((_, button)) = button {
                button.set_active(true);
            }
            self.syncing_views.set(false);
            for (name, key) in [("hidden", "show_hidden"), ("previews", "show_previews"), ("descending", "descending"), ("folder-first", "folders_first")] {
                self.lookup_action(name).set_state(&pane.option_bool(key).to_variant());
            }
            self.selection_changed_impl(pane);
        }
        self.save_session(false);
    }

    fn selection_changed_impl(&self, pane: &BrowserPane) {
        pane.update_summary();
        if self.active_pane().as_ref() == Some(pane) {
            let selected = pane.selected();
            let has_selection = !selected.is_empty();
            let directories = has_selection && selected.iter().all(|i| i.is_dir());
            self.lookup_action("open-new-tab").set_enabled(directories);
            self.lookup_action("open-terminal").set_enabled(!has_selection || directories);
            for name in ["open", "open-with", "rename", "copy", "cut", "trash", "delete", "duplicate", "copy-to", "move-to", "link",
                         "compress", "extract"] {
                self.lookup_action(name).set_enabled(has_selection);
            }
            self.lookup_action("restore").set_enabled(has_selection && pane.current().has_uri_scheme("trash"));
            self.lookup_action("back").set_enabled(pane.history_index() > 0);
            self.lookup_action("forward").set_enabled(pane.history_index() + 1 < pane.history_len() as i64);
        }
        if self.active_pane().as_ref() != Some(pane) || !self.information.widget().is_visible() {
            return;
        }
        let items: Vec<Item> = pane.selected().into_iter().map(|i| Rc::new(i) as Item).collect();
        self.information.show_selection(&items, &pane.current());
    }

    pub fn restore_information_width(&self) {
        if !self.information.widget().is_visible() || self.restoring_information_width.get() {
            return;
        }
        self.restoring_information_width.set(true);
        // Tick callbacks run before layout: let newly shown/hidden side panes
        // allocate once before using the content width to restore the divider.
        let allocated = Cell::new(false);
        let weak = self.downgrade();
        self.content_split.add_tick_callback(move |_, _| {
            if !allocated.get() {
                allocated.set(true);
                return glib::ControlFlow::Continue;
            }
            if let Some(window) = weak.upgrade().map(FilesWindow) {
                if window.alive.get() && window.information.widget().is_visible() {
                    let width = bridge::pget(&window.app().preferences(), "information_width").as_i64().unwrap_or(360) as i32;
                    window.content_split.set_position(160.max(window.content_split.width() - width - 5));
                }
                window.restoring_information_width.set(false);
            }
            glib::ControlFlow::Break
        });
    }

    fn information_resized(&self) {
        if self.restoring_information_width.get() || !self.information.widget().is_visible() {
            return;
        }
        if let Some(source) = self.information_width_source.borrow_mut().take() {
            source.remove();
        }
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(window) = weak.upgrade().map(FilesWindow) {
                *window.information_width_source.borrow_mut() = None;
                window.save_information_width();
            }
        });
        *self.information_width_source.borrow_mut() = Some(source);
    }

    pub fn save_information_width(&self) {
        if let Some(source) = self.information_width_source.borrow_mut().take() {
            source.remove();
        }
        if self.alive.get() && self.information.widget().is_visible() && !self.restoring_information_width.get() {
            let width = self.information.widget().width();
            let prefs = self.app().preferences();
            if width >= 300 && bridge::pget(&prefs, "information_width").as_i64() != Some(width as i64) {
                let _ = bridge::pset(&prefs, "information_width", Value::from(width));
            }
        }
    }

    pub fn message(&self, text: &str) {
        if !text.is_empty() && self.alive.get() {
            self.error_label.set_text(text);
            self.error_row.set_visible(true);
        }
    }

    pub fn key_pressed(&self, key: gdk::Key, code: u32, state: gdk::ModifierType) -> bool {
        if key != gdk::Key::Escape {
            return self.layout_shortcut(key, code, state);
        }
        if let Some(popover) = self.popover.borrow().clone() {
            popover.popdown();
        }
        if let Some(pane) = self.active_pane() {
            let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
            let location: &gtk::Widget = pane.location.upcast_ref();
            if focus.as_ref().is_some_and(|f| f == location || f.is_ancestor(location)) {
                pane.finish_location_edit(true);
            } else if pane.search_box.is_visible() {
                pane.stop_search();
            } else if pane.filter_entry.is_visible() {
                pane.show_filter(false);
            } else {
                pane.selection().unselect_all();
            }
        }
        true
    }

    /// Run a Ctrl/Alt shortcut by key position when the active layout has no Latin letters.
    pub fn layout_shortcut(&self, key: gdk::Key, code: u32, state: gdk::ModifierType) -> bool {
        let modifiers = state & gtk::accelerator_get_default_mod_mask();
        if !modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            return false;
        }
        let latin = latin_keyval(Some(&WidgetExt::display(&self.window)), key, code);
        if latin == key.to_lower() {
            return false; // GTK already matched (or rejected) the typed key itself.
        }
        let accelerator = gtk::accelerator_name(latin, modifiers);
        for detailed in self.app().app.actions_for_accel(&accelerator) {
            if let Some(name) = detailed.strip_prefix("win.") {
                if self.window.lookup_action(name).is_some() {
                    let _ = WidgetExt::activate_action(&self.window, &detailed, None);
                    return true;
                }
            }
        }
        false
    }
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    for entries in [
        vec![("New Tab", "new-tab"), ("New Window", "new-window"), ("Reopen Closed Tab", "reopen-tab")],
        vec![("New Folder…", "new-folder"), ("New Text File…", "new-file"), ("New from Template…", "templates")],
        vec![("Split View", "split"), ("Show Hidden Files", "hidden"), ("Show Previews", "previews"), ("Places Pane", "sidebar"),
             ("Preview & Information Pane", "information")],
        vec![("Bookmark This Folder", "bookmark"), ("Connect to Server…", "connect"), ("Settings…", "preferences")],
    ] {
        let section = gio::Menu::new();
        for (title, action) in entries {
            section.append(Some(title), Some(&format!("win.{action}")));
        }
        menu.append_section(None, &section);
    }
    let sort = gio::Menu::new();
    for (title, action) in [("Name", "sort-name"), ("Size", "sort-size"), ("Type", "sort-type"), ("Modified", "sort-modified"),
                            ("Reverse Order", "descending"), ("Folders First", "folder-first")] {
        sort.append(Some(title), Some(&format!("win.{action}")));
    }
    menu.append_submenu(Some("Sort By"), &sort);
    menu
}

// Actions, menus and file operations.
impl FilesWindow {
    pub fn quick_look(&self, pane: Option<&BrowserPane>) {
        let selected = pane.map(|p| p.selected()).unwrap_or_default();
        if let Some(look) = self.quicklook.borrow().clone() {
            look.close();
            return;
        }
        let (Some(pane), Some(first)) = (pane, selected.first()) else { return };
        let weak = self.downgrade();
        let print: Rc<dyn Fn(Item)> = Rc::new(move |item| {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).print_file(Some(item))
            }
        });
        let look = files_widgets::quicklook::QuickLook::new(&self.window, Rc::new(QuickLookAdapter(pane.downgrade())), print);
        *self.quicklook.borrow_mut() = Some(look.clone());
        let (weak, window) = (self.downgrade(), look.window().downgrade());
        look.window().connect_close_request(move |_| {
            if let Some(me) = weak.upgrade().map(FilesWindow) {
                let current = me.quicklook.borrow().as_ref().map(|l| l.window().clone());
                if current.is_some() && current == window.upgrade() {
                    *me.quicklook.borrow_mut() = None;
                }
            }
            glib::Propagation::Proceed
        });
        look.show_item(&(Rc::new(first.clone()) as Item));
        look.present();
    }

    pub fn go_anywhere(&self, pane: Option<&BrowserPane>) {
        let app = self.app();
        let home = suite_core::paths::home();
        let mut places = vec![home.to_string_lossy().into_owned()];
        for page in self.pages() {
            for side in page.panes() {
                if let Some(path) = side.current().path() {
                    places.push(path.to_string_lossy().into_owned());
                }
            }
        }
        for bookmark in app.bookmarks().borrow().list() {
            if let Some(path) = as_file(&bookmark.uri).path() {
                places.push(path.to_string_lossy().into_owned());
            }
        }
        let mut roots = vec![home.clone()];
        if let Some(current) = pane.and_then(|p| p.current().path()) {
            if !current.starts_with(&home) {
                roots.insert(0, current);
            }
        }
        let include_hidden = pane.is_some_and(|p| p.option_bool("show_hidden"));
        let index = match app.hooks.folder_index.borrow().clone() {
            Some(make) => make(),
            None => files_model::goto::FolderIndex::new(&roots, include_hidden),
        };
        let weak = self.downgrade();
        let chosen: crate::goto::OnOpen = Rc::new(move |location: gio::File, is_dir: bool| {
            let Some(window) = weak.upgrade().map(FilesWindow) else { return };
            let Some(target) = window.active_pane() else { return };
            if target.closed() {
                return;
            }
            if is_dir {
                target.navigate(&location, true, None);
            } else {
                let parent = location.parent().unwrap_or_else(|| location.clone());
                target.navigate(&parent, true, Some(vec![location.uri().to_string()]));
            }
            target.view_widget(None).grab_focus();
        });
        let palette = GoAnywhere::new(self.window.upcast_ref(), &places, index, chosen);
        let weak = self.downgrade();
        palette.window.connect_close_request(move |_| {
            if let Some(window) = weak.upgrade() {
                *window.goto.borrow_mut() = None;
            }
            glib::Propagation::Proceed
        });
        *self.goto.borrow_mut() = Some(palette.clone());
        palette.window.present();
    }

    pub fn print_file(&self, item: Option<Item>) {
        let Some(item) = item else { return };
        let weak = self.downgrade();
        let report: Rc<dyn Fn(String)> = Rc::new(move |text| {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).message(&text)
            }
        });
        files_widgets::print::print_file(Some(self.window.upcast_ref()), &item, report, None);
    }

    pub fn option(&self, key: &str, value: Value) {
        let prefs = self.app().preferences();
        let pane = self.active_pane();
        let result = match &pane {
            Some(pane) if bridge::pbool(&prefs, "remember_folders")
                && ["view", "icon_size", "sort", "descending", "folders_first", "show_hidden", "show_previews"].contains(&key) => {
                bridge::pset_folder_options(&prefs, pane.current().uri().as_str(), bridge::map(&[(key, value)]))
            }
            _ => bridge::pset(&prefs, key, value),
        };
        if let Err(error) = result {
            self.message(&error);
        }
        if let Some(pane) = pane {
            pane.refresh();
        }
    }

    /// `FilesWindow.action(name)`.
    pub fn action(&self, name: &str) {
        let app = self.app();
        if let Some(hook) = app.hooks.action.borrow().clone() {
            if hook(name) {
                return;
            }
        }
        // Window accelerators run before some editable widget bindings. Route
        // shared shortcuts to the text field, never to the selected files.
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        if let Some(editable) = focus.as_ref().and_then(|f| f.dynamic_cast_ref::<gtk::Editable>().cloned()) {
            if ["copy", "cut", "paste", "select-all", "invert-selection", "undo", "trash", "delete"].contains(&name) {
                let delegate: gtk::Widget = editable.delegate().map(|d| d.upcast()).unwrap_or_else(|| editable.clone().upcast());
                if name == "trash" {
                    delegate.emit_by_name::<()>("delete-from-cursor", &[&gtk::DeleteType::Chars, &1i32]);
                } else if name == "invert-selection" {
                    let position = editable.position();
                    editable.select_region(position, position);
                } else {
                    let target = match name {
                        "copy" => "clipboard.copy",
                        "cut" | "delete" => "clipboard.cut",
                        "paste" => "clipboard.paste",
                        "select-all" => "selection.select-all",
                        _ => "text.undo",
                    };
                    let _ = delegate.activate_action(target, None);
                }
                return;
            }
        }
        let pane = self.active_pane();
        match name {
            "new-window" => {
                let window = FilesWindow::new(&app);
                let location = pane.as_ref().map(|p| p.current()).unwrap_or_else(home_file);
                window.new_tab(Some(&location), None);
                window.window.present();
                return;
            }
            "new-tab" => {
                self.new_tab(None, None);
                return;
            }
            "close-tab" => {
                self.close_tab(None);
                return;
            }
            "reopen-tab" => {
                let last = self.closed_tabs.borrow_mut().pop();
                if let Some((uri, split)) = last {
                    let page = self.new_tab(Some(&as_file(&uri)), None);
                    if let Some(split) = split {
                        page.set_split(true, Some(&as_file(&split)));
                    }
                }
                return;
            }
            "preferences" => {
                self.settings();
                return;
            }
            _ => {}
        }
        let Some(pane) = pane else { return };
        match name {
            "back" => pane.back(),
            "forward" => pane.forward(),
            "up" => pane.up(),
            "refresh" => pane.refresh(),
            "home" => pane.navigate(&home_file(), true, None),
            "location" => pane.edit_location(),
            "split" => {
                if let Some(page) = self.current_page() {
                    page.set_split(page.right().is_none(), None);
                }
            }
            "focus-other" => {
                if let Some(page) = self.current_page() {
                    if page.right().is_none() {
                        page.set_split(true, None);
                    }
                    let other = if page.right().as_ref() == self.active_pane().as_ref() { page.left.clone() } else { page.right().unwrap() };
                    self.set_active_pane(&other);
                    other.view_widget(None).grab_focus();
                }
            }
            "search" => pane.show_search(),
            "filter" => pane.show_filter(true),
            "icons" | "compact" | "details" | "columns" => self.option("view", Value::String(name.into())),
            "go-anywhere" => self.go_anywhere(Some(&pane)),
            "quick-look" => self.quick_look(Some(&pane)),
            "print" => {
                let selected = pane.selected();
                if selected.len() == 1 {
                    let item: Item = Rc::new(selected[0].clone());
                    if files_widgets::print::printable(Some(&item)) {
                        self.print_file(Some(item));
                    }
                }
            }
            "sort-name" | "sort-size" | "sort-type" | "sort-modified" => self.option("sort", Value::String(name[5..].into())),
            "hidden" | "previews" | "descending" | "folder-first" => {
                let key = CHECKS.iter().find(|(n, _)| *n == name).unwrap().1;
                self.option(key, Value::Bool(!pane.option_bool(key)));
            }
            "information" | "sidebar" => {
                let widget: gtk::Widget = if name == "information" { self.information.widget().clone().upcast() } else { self.sidebar_scroll.clone().upcast() };
                if name == "information" && widget.is_visible() {
                    self.save_information_width();
                    self.information.clear();
                }
                widget.set_visible(!widget.is_visible());
                self.lookup_action(name).set_state(&widget.is_visible().to_variant());
                let key = if name == "information" { "show_information" } else { "show_sidebar" };
                let _ = bridge::pset(&app.preferences(), key, Value::Bool(widget.is_visible()));
                if name == "information" && widget.is_visible() {
                    self.restore_information_width();
                }
                self.selection_changed_impl(&pane);
            }
            "zoom-in" | "zoom-out" => {
                let size = pane.option_int("icon_size", 64) + if name == "zoom-in" { 16 } else { -16 };
                self.option("icon_size", Value::from(size.clamp(32, 160)));
            }
            "select-all" => {
                pane.selection().select_all();
            }
            "invert-selection" => {
                let selection = pane.selection();
                for i in 0..selection.n_items() {
                    if selection.is_selected(i) {
                        selection.unselect_item(i);
                    } else {
                        selection.select_item(i, false);
                    }
                }
            }
            "open" => {
                for item in pane.selected() {
                    pane.activate(Some(item));
                }
            }
            "open-new-tab" => {
                for item in pane.selected() {
                    if item.is_dir() {
                        self.new_tab(Some(&item.file()), None);
                    }
                }
            }
            "open-terminal" => self.open_terminal(),
            "open-with" => self.open_with(),
            "copy" | "cut" => self.copy_selection(name == "cut"),
            "paste" => self.paste(),
            "new-folder" | "new-file" => self.create(name == "new-folder"),
            "rename" => self.rename(),
            "trash" | "delete" => {
                let items = pane.selected();
                let files: Vec<gio::File> = items.iter().map(|i| i.file()).collect();
                if files.is_empty() {
                    return;
                }
                let count = items.len();
                let (weak, kind) = (self.downgrade(), name.to_string());
                let remove = move || {
                    if let Some(window) = weak.upgrade().map(FilesWindow) {
                        pane.expect_removal(&items);
                        window.run_operation(&kind, files, None);
                    }
                };
                if name == "delete" {
                    self.confirm("Permanently Delete", &format!("Permanently delete {count} selected item(s)? This cannot be undone."),
                                 Box::new(move |yes| if yes { remove() }));
                } else {
                    remove();
                }
            }
            "duplicate" | "restore" => {
                let files = pane.selected().iter().map(|i| i.file()).collect();
                self.run_operation(name, files, None);
            }
            "undo" => {
                self.undo();
            }
            "properties" => {
                let mut files: Vec<gio::File> = pane.selected().iter().map(|i| i.file()).collect();
                if files.is_empty() {
                    files.push(pane.current());
                }
                let weak = self.downgrade();
                let window = PropertiesWindow::new(Some(self.window.upcast_ref()), &files, Some(Rc::new(move || {
                    if let Some(window) = weak.upgrade() {
                        FilesWindow(window).refresh_all()
                    }
                })), None);
                window.present();
            }
            "copy-to" | "move-to" | "link" => self.choose_destination(name),
            "bookmark" => {
                let current = pane.current();
                if let Err(error) = app.bookmarks().borrow().add(current.uri().as_str(), &file_label(&current)) {
                    self.message(&error.to_string());
                }
                self.refresh_sidebar();
            }
            "connect" => self.connect_server(),
            "empty-trash" => self.empty_trash(),
            "compress" | "extract" => self.archive(name),
            "browse-archive" => {
                let selected = pane.selected();
                if selected.len() == 1 {
                    pane.navigate(&as_file(&files_model::model::archive_uri_for_file(&selected[0].file())), true, None);
                }
            }
            "templates" => self.from_template(),
            "save-search" => self.save_search(),
            _ => {}
        }
    }

    pub fn refresh_all(&self) {
        if self.alive.get() {
            for page in self.pages() {
                for pane in page.panes() {
                    pane.refresh();
                }
            }
        }
    }

    pub fn dismiss_context_menu(&self) {
        if let Some(source) = self.popover_cleanup.borrow_mut().take() {
            source.remove();
        }
        let popover = self.popover.borrow_mut().take();
        let connections = std::mem::take(&mut *self.popover_connections.borrow_mut());
        for (object, handler) in connections {
            object.disconnect(handler);
        }
        if let Some(popover) = popover {
            popover.popdown();
            if popover.parent().is_some() {
                popover.unparent();
            }
        }
    }

    fn context_menu_closed(&self, popover: &gtk::Popover) {
        // GTK closes a menu before dispatching the chosen action. Keep its
        // parent/action lookup intact until that dispatch has returned.
        if self.popover_cleanup.borrow().is_some() {
            return;
        }
        let (weak, closed) = (self.downgrade(), popover.downgrade());
        let source = glib::idle_add_local_once(move || {
            let Some(window) = weak.upgrade().map(FilesWindow) else { return };
            *window.popover_cleanup.borrow_mut() = None;
            let current = window.popover.borrow().clone();
            if current.is_some() && current == closed.upgrade() {
                window.dismiss_context_menu();
            }
        });
        *self.popover_cleanup.borrow_mut() = Some(source);
    }

    /// `present_context_menu`: parented to the window child (so view styles
    /// don't resize measured items), opening to the RIGHT of the pointer with
    /// its top at the pointer — the compositor slides it vertically instead of
    /// GTK shrinking it into a scrolled list.
    pub fn present_context_menu(&self, popover: &gtk::Popover, widget: &gtk::Widget, x: f64, y: f64) {
        self.dismiss_context_menu();
        *self.popover.borrow_mut() = Some(popover.clone());
        popover.set_has_arrow(false);
        let mut anchor: gtk::Widget = self.window.child().unwrap_or_else(|| widget.clone());
        let (mut x, mut y) = (x, y);
        match widget.compute_point(&anchor, &graphene::Point::new(x as f32, y as f32)) {
            Some(point) => {
                x = point.x() as f64;
                y = point.y() as f64;
            }
            None => anchor = widget.clone(),
        }
        popover.set_parent(&anchor);
        // Detach on close, and before its anchor disappears: virtualized rows
        // and breadcrumbs can be destroyed while the window remains alive.
        let weak = self.downgrade();
        let closed = popover.connect_closed(move |popover| {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).context_menu_closed(popover)
            }
        });
        let weak = self.downgrade();
        let unmapped = widget.connect_unmap(move |_| {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).dismiss_context_menu()
            }
        });
        *self.popover_connections.borrow_mut() = vec![(popover.clone().upcast(), closed), (widget.clone().upcast(), unmapped)];
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        // The menu builds its items on popup, so measure it after that.
        popover.set_position(gtk::PositionType::Right);
        popover.popup();
        let (_, natural, _, _) = popover.measure(gtk::Orientation::Vertical, -1);
        popover.set_offset(0, natural / 2);
    }

    pub fn context_menu(&self, pane: &BrowserPane, widget: &gtk::Widget, x: f64, y: f64) {
        self.dismiss_context_menu();
        for ident in self.custom_action_ids.borrow_mut().drain(..) {
            self.window.remove_action(&ident);
        }
        let menu = gio::Menu::new();
        let selected = pane.selected();
        let trash = pane.current().has_uri_scheme("trash");
        if !selected.is_empty() {
            let first = gio::Menu::new();
            first.append(Some("Open"), Some("win.open"));
            if selected.iter().all(|i| i.is_dir()) {
                first.append(Some("Open in New Tab"), Some("win.open-new-tab"));
                first.append(Some("Open in Terminal"), Some("win.open-terminal"));
            }
            first.append(Some("Open With…"), Some("win.open-with"));
            first.append(Some("Rename…"), Some("win.rename"));
            if selected.len() == 1 && !selected[0].is_dir() {
                first.append(Some("Quick Preview"), Some("win.quick-look"));
                let item: Item = Rc::new(selected[0].clone());
                if files_widgets::print::printable(Some(&item)) {
                    first.append(Some("Print…"), Some("win.print"));
                }
            }
            menu.append_section(None, &first);
            let edits = gio::Menu::new();
            for (text, name) in [("Cut", "cut"), ("Copy", "copy"), ("Duplicate", "duplicate"), ("Copy To…", "copy-to"),
                                 ("Move To…", "move-to"), ("Create Link…", "link")] {
                edits.append(Some(text), Some(&format!("win.{name}")));
            }
            menu.append_section(None, &edits);
            let archives = gio::Menu::new();
            archives.append(Some("Compress…"), Some("win.compress"));
            if selected.len() == 1 {
                archives.append(Some("Extract…"), Some("win.extract"));
                let lower = selected[0].name().to_lowercase();
                if ARCHIVE_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
                    archives.append(Some("Browse Archive"), Some("win.browse-archive"));
                }
            }
            menu.append_section(None, &archives);
            let remove = gio::Menu::new();
            if trash {
                remove.append(Some("Restore"), Some("win.restore"));
                remove.append(Some("Permanently Delete…"), Some("win.delete"));
            } else {
                remove.append(Some("Move to Trash"), Some("win.trash"));
            }
            menu.append_section(None, &remove);
            let actions = gio::Menu::new();
            for (index, custom) in self.custom_actions.list().into_iter().enumerate() {
                if selected.iter().any(|i| (i.is_dir() && !custom.directories) || (!i.is_dir() && !custom.files)) {
                    continue;
                }
                let ident = format!("custom-file-action-{index}");
                let action = gio::SimpleAction::new(&ident, None);
                let files: Vec<gio::File> = selected.iter().map(|i| i.file()).collect();
                let directory: Option<files_ops::Target> = pane.current().path().map(Into::into);
                let (weak, entry) = (self.downgrade(), custom.clone());
                action.connect_activate(move |_, _| {
                    let Some(window) = weak.upgrade().map(FilesWindow) else { return };
                    let targets: Vec<files_ops::Target> = files.iter().map(|f| f.clone().into()).collect();
                    if let Err(error) = files_ops::extras::CustomActions::invoke(&entry, &targets, directory.as_ref()) {
                        window.message(&error.to_string());
                    }
                });
                self.window.add_action(&action);
                self.custom_action_ids.borrow_mut().push(ident.clone());
                actions.append(Some(&custom.name), Some(&format!("win.{ident}")));
            }
            if actions.n_items() > 0 {
                menu.append_section(Some("Custom Actions"), &actions);
            }
        } else {
            menu.append(Some("Open in Terminal"), Some("win.open-terminal"));
            for (text, name) in [("New Folder…", "new-folder"), ("New Text File…", "new-file"), ("Paste", "paste"), ("Undo", "undo"),
                                 ("Reload", "refresh"), ("Bookmark This Folder", "bookmark")] {
                menu.append(Some(text), Some(&format!("win.{name}")));
            }
            if trash {
                menu.append(Some("Empty Trash…"), Some("win.empty-trash"));
            }
        }
        let end = gio::Menu::new();
        end.append(Some("Properties…"), Some("win.properties"));
        menu.append_section(None, &end);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        self.present_context_menu(popover.upcast_ref(), widget, x, y);
    }

    /// The test hook or `file_launch.launch_terminal`.
    fn launch_terminal(&self, directory: &gio::File) -> Result<(), String> {
        if let Some(hook) = self.app().hooks.launch_terminal.borrow().clone() {
            return hook(directory);
        }
        let weak = self.downgrade();
        let context = WidgetExt::display(&self.window).app_launch_context();
        files_model::launch::launch_terminal(directory, Some(Rc::new(context.upcast::<gio::AppLaunchContext>()) as Rc<dyn waydock::core::app_catalog::LaunchContext>), Some(Box::new(move |error: Option<String>| {
            if let (Some(error), Some(window)) = (error, weak.upgrade()) {
                FilesWindow(window).message(&error);
            }
        }))).map(|_| ()).map_err(|e| e.to_string())
    }

    fn open_terminal(&self) {
        let Some(pane) = self.active_pane() else { return };
        let selected = pane.selected();
        if selected.iter().any(|i| !i.is_dir()) {
            return;
        }
        let mut directories: Vec<gio::File> = selected.iter().map(|i| i.file()).collect();
        if directories.is_empty() {
            directories.push(pane.current());
        }
        for directory in directories {
            if let Err(error) = self.launch_terminal(&directory) {
                self.message(&error);
            }
        }
    }

    pub fn open_files(&self, files: Vec<gio::File>) {
        let app = self.app();
        for file in files {
            if let Some(hook) = app.hooks.open_files.borrow().clone() {
                hook(&file);
                continue;
            }
            let weak = self.downgrade();
            let context = WidgetExt::display(&self.window).app_launch_context();
            let mut launcher = waydock::core::app_catalog::Launcher::default();
            launcher.helper = vec![crate::app::helper_program().to_string_lossy().into_owned(), "--launch-helper".into(), "--app-catalog".into()];
            let context: Rc<dyn waydock::core::app_catalog::LaunchContext> = Rc::new(context.upcast::<gio::AppLaunchContext>());
            let result = launcher.launch_uri(file.uri().as_str(), Some(context),
                Some(Box::new(move |error: Option<String>| {
                    if let (Some(error), Some(window)) = (error, weak.upgrade()) {
                        FilesWindow(window).message(&error);
                    }
                })));
            if let Err(error) = result {
                self.message(&error.to_string());
            }
        }
    }

    fn open_with(&self) {
        let Some(pane) = self.active_pane() else { return };
        let selected = pane.selected();
        if selected.is_empty() {
            return;
        }
        let weak = self.downgrade();
        let files: Vec<gio::File> = selected.iter().map(|i| i.file()).collect();
        dialogs::choose_application(Some(self.window.upcast_ref()), &selected[0].file(), move |info| {
            let (Some(info), Some(window)) = (info, weak.upgrade().map(FilesWindow)) else { return };
            let context = WidgetExt::display(&window.window).app_launch_context();
            let w = window.downgrade();
            let result = files_model::launch::launch_with(&info, &files, Some(Rc::new(context.upcast::<gio::AppLaunchContext>()) as Rc<dyn waydock::core::app_catalog::LaunchContext>), Some(Box::new(move |error: Option<String>| {
                if let (Some(error), Some(window)) = (error, w.upgrade()) {
                    FilesWindow(window).message(&error);
                }
            })));
            if let Err(error) = result {
                window.message(&error.to_string());
            }
        });
    }

    pub fn copy_selection(&self, cut: bool) {
        let app = self.app();
        let Some(pane) = self.active_pane() else { return };
        let files: Vec<gio::File> = pane.selected().iter().map(|i| i.file()).collect();
        if files.is_empty() {
            return;
        }
        let copied = format!("{}\n{}", if cut { "cut" } else { "copy" }, files.iter().map(|f| f.uri().to_string()).collect::<Vec<_>>().join("\n"));
        let provider = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()),
            gdk::ContentProvider::for_bytes("x-special/gnome-copied-files", &glib::Bytes::from_owned(copied.into_bytes())),
        ]);
        *app.clipboard_files.borrow_mut() = files;
        app.clipboard_cut.set(cut);
        *app.clipboard_provider.borrow_mut() = Some(provider.clone());
        self.window.clipboard().set_content(Some(&provider)).ok();
    }

    pub fn paste(&self) {
        let app = self.app();
        let Some(destination) = self.active_pane().map(|p| p.current()) else { return };
        let clipboard = self.window.clipboard();
        let ours = clipboard.content().is_some() && clipboard.content() == *app.clipboard_provider.borrow();
        if ours && !app.clipboard_files.borrow().is_empty() {
            let files = app.clipboard_files.borrow().clone();
            let cut = app.clipboard_cut.get();
            if let Some(entry) = self.run_operation(if cut { "move" } else { "copy" }, files, Some(destination)) {
                if cut {
                    entry.cut_generation.set(Some(app.clipboard_generation.get()));
                }
            }
            return;
        }
        if clipboard.formats().contain_mime_type("x-special/gnome-copied-files") {
            let generation = app.clipboard_generation.get();
            let weak = self.downgrade();
            clipboard.read_async(&["x-special/gnome-copied-files"], glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |result| {
                let Some(window) = weak.upgrade().map(FilesWindow) else { return };
                match result {
                    Ok((stream, _mime)) => window.read_copied_files(stream, Vec::new(), generation, destination),
                    Err(error) => window.message(error.message()),
                }
            });
            return;
        }
        let weak = self.downgrade();
        clipboard.read_value_async(gdk::FileList::static_type(), glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |result| {
            let Some(window) = weak.upgrade().map(FilesWindow) else { return };
            match result {
                Ok(value) => {
                    if let Ok(list) = value.get::<gdk::FileList>() {
                        let files = list.files();
                        if !files.is_empty() {
                            window.run_operation("copy", files, Some(destination));
                        }
                    }
                }
                Err(error) => window.message(&format!("Clipboard does not contain files: {}", error.message())),
            }
        });
    }

    fn read_copied_files(&self, stream: gio::InputStream, mut chunks: Vec<u8>, generation: u64, destination: gio::File) {
        let weak = self.downgrade();
        stream.clone().read_bytes_async(16384, glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |result| {
            let Some(window) = weak.upgrade().map(FilesWindow) else { return };
            let fail = |text: String| {
                stream.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
                window.message(&text);
            };
            let chunk = match result {
                Ok(chunk) => chunk,
                Err(error) => return fail(suite_core::pyerr::python_error_text(&error)),
            };
            if chunks.len() + chunk.len() > 1024 * 1024 {
                return fail("Too many clipboard files.".into());
            }
            chunks.extend_from_slice(&chunk);
            if !chunk.is_empty() {
                return window.read_copied_files(stream.clone(), chunks, generation, destination);
            }
            stream.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
            let Ok(text) = String::from_utf8(chunks) else { return window.message("Invalid file clipboard data.") };
            let lines: Vec<&str> = text.lines().collect();
            if lines.is_empty() || !matches!(lines[0], "copy" | "cut") {
                return window.message("Invalid file clipboard data.");
            }
            let files: Vec<gio::File> = lines[1..].iter().filter(|uri| glib::Uri::peek_scheme(uri).is_some()).map(|uri| gio::File::for_uri(uri)).collect();
            let cut = lines[0] == "cut";
            if let Some(entry) = window.run_operation(if cut { "move" } else { "copy" }, files, Some(destination)) {
                if cut {
                    entry.cut_generation.set(Some(generation));
                }
            }
        });
    }

    fn create(&self, folder: bool) {
        let Some(destination) = self.active_pane().map(|p| p.current()) else { return };
        let weak = self.downgrade();
        let (title, initial) = if folder { ("New Folder", "New Folder") } else { ("New Text File", "Untitled.txt") };
        dialogs::prompt_text(Some(self.window.upcast_ref()), title, "Name", Some(initial), move |name| {
            let (Some(name), Some(window)) = (name, weak.upgrade().map(FilesWindow)) else { return };
            if name.trim().is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
                window.message("Enter a file name without slashes.");
                return;
            }
            window.run_operation(if folder { "mkdir" } else { "touch" }, Vec::new(), Some(destination.child(&name)));
        });
    }

    fn rename(&self) {
        let Some(pane) = self.active_pane() else { return };
        let selected = pane.selected();
        if selected.is_empty() {
            return;
        }
        let weak = self.downgrade();
        if selected.len() == 1 {
            let source = selected[0].file();
            dialogs::prompt_text(Some(self.window.upcast_ref()), "Rename", "New name", Some(&selected[0].name()), move |name| {
                let (Some(name), Some(window)) = (name, weak.upgrade().map(FilesWindow)) else { return };
                if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
                    window.message("Enter a file name without slashes.");
                    return;
                }
                let target = source.parent().map(|p| p.child(&name)).unwrap_or_else(|| gio::File::for_path(&name));
                window.run_operation("rename", vec![source.clone()], Some(target));
            });
            return;
        }
        dialogs::prompt_text(Some(self.window.upcast_ref()), "Rename Selected Files", "Use # for the sequence number. Original extensions are retained.",
            Some("File #"), move |value| {
                let (Some(value), Some(window)) = (value, weak.upgrade().map(FilesWindow)) else { return };
                if selected.iter().any(|i| i.path().is_none()) {
                    window.message("Batch rename currently requires local files.");
                    return;
                }
                let paths: Vec<PathBuf> = selected.iter().filter_map(|i| i.path()).collect();
                let weak = window.downgrade();
                glib::spawn_future_local(async move {
                    let result = gio::spawn_blocking(move || files_ops::tools::plan_batch_rename(&paths, &value, 1).map_err(|e| e.to_string())).await;
                    let Some(window) = weak.upgrade().map(FilesWindow) else { return };
                    if !window.alive.get() {
                        return;
                    }
                    let pairs = match result {
                        Ok(Ok(pairs)) => pairs,
                        Ok(Err(error)) => return window.message(&error),
                        Err(_) => return window.message("Batch rename failed."),
                    };
                    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let mut summary = pairs.iter().take(12).map(|(old, new)| format!("{} → {}", name(old), name(new))).collect::<Vec<_>>().join("\n");
                    if pairs.len() > 12 {
                        summary.push_str(&format!("\n… and {} more", pairs.len() - 12));
                    }
                    let w = window.downgrade();
                    window.confirm("Rename Selected Files", &summary, Box::new(move |yes| {
                        if let (true, Some(window)) = (yes, w.upgrade().map(FilesWindow)) {
                            let files = pairs.iter().map(|(a, b)| (gio::File::for_path(a), gio::File::for_path(b))).collect();
                            window.rename_many(files);
                        }
                    }));
                });
            });
    }

    fn choose_destination(&self, action: &str) {
        let Some(pane) = self.active_pane() else { return };
        let files: Vec<gio::File> = pane.selected().iter().map(|i| i.file()).collect();
        if files.is_empty() {
            return;
        }
        let kind = match action {
            "copy-to" => "copy",
            "move-to" => "move",
            _ => "link",
        };
        let weak = self.downgrade();
        dialogs::choose_file(Some(self.window.upcast_ref()), "Choose Destination", move |destination| {
            if let (Some(destination), Some(window)) = (destination, weak.upgrade().map(FilesWindow)) {
                window.run_operation(kind, files, Some(destination));
            }
        }, ChooserMode::Folder, Some(&pane.current()), "");
    }

    /// `window.job`: the oldest running job of the application, or None when idle.
    pub fn job_running(&self) -> bool {
        !self.app().jobs.entries().is_empty()
    }

    fn job_start(&self, title: &str) -> Rc<JobEntry> {
        // Operations run concurrently; drops and new jobs are never refused.
        self.app().jobs.add(title, Some(self.window.upcast_ref()), None)
    }

    fn callbacks(&self, entry: &Rc<JobEntry>) -> Callbacks {
        let app = self.app();
        let (a1, a2, a3) = (app.downgrade(), app.downgrade(), app.downgrade());
        let (e1, e2, e3) = (entry.clone(), entry.clone(), entry.clone());
        Callbacks::new()
            .on_progress(move |progress: &Progress| {
                if let Some(app) = a1.upgrade() {
                    progress_update(&app, &e1, progress)
                }
            })
            .on_conflict(move |conflict: &Conflict, resolve: Resolve| match a2.upgrade() {
                Some(app) => conflict_dialog(&app, &e2, conflict, resolve),
                None => resolve("cancel", None),
            })
            .on_done(move |result: &OperationResult| {
                if let Some(app) = a3.upgrade() {
                    job_done(&app, &e3, result)
                }
            })
    }

    /// `run_operation(kind, sources, destination)`: the job entry, or None.
    pub fn run_operation(&self, kind: &str, sources: Vec<gio::File>, destination: Option<gio::File>) -> Option<Rc<JobEntry>> {
        if let Some(hook) = self.app().hooks.run_operation.borrow().clone() {
            return hook(kind, &sources, destination.as_ref());
        }
        if sources.is_empty() && !matches!(kind, "touch" | "mkdir") {
            return None;
        }
        let title = job_title(kind);
        let entry = self.job_start(&title);
        let callbacks = self.callbacks(&entry);
        let job = self.app().operations().start(kind, &sources, destination.as_ref(), callbacks);
        entry.set_job(Some(Rc::new(job)));
        Some(entry)
    }

    pub fn rename_many(&self, pairs: Vec<(gio::File, gio::File)>) -> Option<Rc<JobEntry>> {
        let entry = self.job_start(&tr!("Renaming"));
        let job = self.app().operations().rename_many(pairs, self.callbacks(&entry));
        entry.set_job(Some(Rc::new(job)));
        Some(entry)
    }

    pub fn undo(&self) -> Option<Rc<JobEntry>> {
        let operations = self.app().operations();
        if !operations.can_undo() {
            self.message("No reversible file operation to undo.");
            return None;
        }
        let entry = self.job_start(&tr!("Undo"));
        match operations.undo(self.callbacks(&entry)) {
            Some(job) => entry.set_job(Some(Rc::new(job))),
            None => self.app().jobs.finish(&entry, None, false),
        }
        Some(entry)
    }

    /// Cancel the operations started in this window.
    pub fn cancel_job(&self) {
        let jobs = &self.app().jobs;
        jobs.cancel_all(Some(&jobs.owned_by(self.window.upcast_ref())));
    }

    /// `run_task(title, task)`: a blocking task on a worker thread with a cancel flag.
    pub fn run_task(&self, title: &str, task: impl FnOnce(Arc<AtomicBool>) -> Result<(), String> + Send + 'static) -> Rc<JobEntry> {
        let app = self.app();
        let entry = self.job_start(title);
        let cancel = Arc::new(AtomicBool::new(false));
        entry.set_job(Some(Rc::new(TaskHandle(cancel.clone()))));
        let (weak_app, e, title) = (app.downgrade(), entry.clone(), title.to_string());
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || task(cancel)).await;
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some("The task failed.".to_string()),
            };
            let Some(app) = weak_app.upgrade() else { return };
            if let Some(window) = e.window().and_then(|w| crate::app::files_window_for(&w)) {
                if let Some(error) = &error {
                    window.message(error);
                }
                window.refresh_all();
            }
            let summary = error.clone().unwrap_or_else(|| tr!("{title} finished.", title = title.as_str()));
            app.jobs.finish(&e, Some(&summary), error.is_some());
        });
        entry
    }

    fn archive(&self, action: &str) {
        let Some(pane) = self.active_pane() else { return };
        let selected = pane.selected();
        if selected.is_empty() {
            return;
        }
        if selected.iter().any(|i| i.path().is_none()) {
            self.message("Copy remote items locally before archiving them.");
            return;
        }
        let paths: Vec<PathBuf> = selected.iter().filter_map(|i| i.path()).collect();
        if action == "extract" && paths.len() != 1 {
            self.message("Select one archive to extract.");
            return;
        }
        let weak = self.downgrade();
        if action == "compress" {
            dialogs::choose_file(Some(self.window.upcast_ref()), "Create Archive (.zip, .tar, .tar.gz, .tar.bz2, .tar.xz)", move |file| {
                let (Some(file), Some(window)) = (file, weak.upgrade().map(FilesWindow)) else { return };
                let Some(output) = file.path() else { return window.message("Choose a local destination for the archive.") };
                window.run_task("Creating archive", move |cancel| {
                    files_ops::tools::create_archive(&paths, &output, Some(&*cancel), None).map(|_| ()).map_err(|e| e.to_string())
                });
            }, ChooserMode::Save, Some(&pane.current()), "Archive.zip");
        } else {
            dialogs::choose_file(Some(self.window.upcast_ref()), "Extract Archive Into", move |file| {
                let (Some(file), Some(window)) = (file, weak.upgrade().map(FilesWindow)) else { return };
                let Some(output) = file.path() else { return window.message("Choose a local destination for extraction.") };
                let archive = paths[0].clone();
                window.run_task("Extracting archive", move |cancel| {
                    files_ops::tools::extract_archive(&archive, &output, Some(&*cancel), None).map(|_| ()).map_err(|e| e.to_string())
                });
            }, ChooserMode::Folder, Some(&pane.current()), "");
        }
    }

    fn from_template(&self) {
        let Some(destination) = self.active_pane().map(|p| p.current()) else { return };
        let folder = glib::user_special_dir(glib::UserDirectory::Templates).unwrap_or_else(|| suite_core::paths::home().join("Templates"));
        let weak = self.downgrade();
        dialogs::choose_file(Some(self.window.upcast_ref()), "Choose a Template", move |template| {
            if let (Some(template), Some(window)) = (template, weak.upgrade().map(FilesWindow)) {
                window.run_operation("copy", vec![template], Some(destination));
            }
        }, ChooserMode::Open, Some(&gio::File::for_path(folder)), "");
    }

    fn save_search(&self) {
        let Some(pane) = self.active_pane() else { return };
        let query = pane.search_entry.text().to_string();
        if query.trim().is_empty() {
            return;
        }
        let value = serde_json::json!({"uri": pane.current().uri().as_str(), "query": query, "contents": pane.contents_check.is_active()});
        let weak = self.downgrade();
        dialogs::prompt_text(Some(self.window.upcast_ref()), "Save Search", "Name", Some(&query), move |name| {
            let (Some(name), Some(window)) = (name, weak.upgrade().map(FilesWindow)) else { return };
            if name.is_empty() {
                return;
            }
            let prefs = window.app().preferences();
            let mut saved = bridge::pget(&prefs, "search_saved").as_array().cloned().unwrap_or_default();
            let mut entry = value.as_object().cloned().unwrap_or_default();
            entry.insert("name".into(), Value::String(name));
            saved.push(Value::Object(entry));
            if let Err(error) = bridge::pset(&prefs, "search_saved", Value::Array(saved)) {
                window.message(&error);
            }
            window.refresh_sidebar();
        });
    }

    /// `confirm(...)` through the test hook when set.
    pub fn confirm(&self, title: &str, message: &str, callback: Box<dyn FnOnce(bool)>) {
        if let Some(hook) = self.app().hooks.confirm.borrow().clone() {
            hook(title, message, callback);
            return;
        }
        dialogs::confirm(Some(self.window.upcast_ref()), title, message, callback);
    }
}

/// A run_task job: `cancel()` sets the task's flag.
struct TaskHandle(Arc<AtomicBool>);

impl JobHandle for TaskHandle {
    fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub fn job_title(kind: &str) -> String {
    match kind {
        "copy" => tr!("Copying"),
        "move" => tr!("Moving"),
        "link" => tr!("Linking"),
        "trash" => tr!("Moving to Trash"),
        "delete" => tr!("Deleting"),
        "duplicate" => tr!("Duplicating"),
        "restore" => tr!("Restoring"),
        "rename" => tr!("Renaming"),
        "mkdir" => tr!("Creating folder"),
        "touch" => tr!("Creating file"),
        // kind.title()
        other => {
            let mut chars = other.chars();
            chars.next().map(|c| c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()).unwrap_or_default()
        }
    }
}

fn progress_update(app: &FilesApplication, entry: &Rc<JobEntry>, progress: &Progress) {
    let name = if progress.source.is_empty() { String::new() } else { file_label(&as_file(&progress.source)) };
    let mut fraction = if progress.total_bytes > 0 { Some((progress.current_bytes as f64 / progress.total_bytes as f64).min(1.)) } else { None };
    if fraction.is_none() && progress.total_items > 1 {
        fraction = Some((progress.completed_items as f64 / progress.total_items as f64).min(1.));
    }
    app.jobs.update(entry, Some(&name), fraction);
}

fn conflict_dialog(app: &FilesApplication, entry: &Rc<JobEntry>, conflict: &Conflict, resolve: Resolve) {
    let parent = entry.window().or_else(|| app.jobs.windows().into_iter().next());
    #[allow(deprecated)]
    let dialog = gtk::Dialog::builder().title(tr!("File Already Exists")).modal(parent.is_some()).application(&app.app).build();
    #[allow(deprecated)]
    {
        dialog.set_transient_for(parent.as_ref());
        dialog.set_default_size(440, -1);
        let label = gtk::Label::builder().label(format!("{}\n{}", tr!("A file or folder already exists at:"), conflict.destination.parse_name()))
            .wrap(true).selectable(true).build();
        suite_gtk::utils::set_margins(&label, 18);
        dialog.content_area().append(&label);
        let choices: Vec<(&str, &str)> = [("Cancel", "cancel"), ("Skip", "skip"), ("Keep Both", "rename"), ("Merge Folders", "merge"), ("Replace", "replace")]
            .into_iter().filter(|(_, c)| conflict.choices.contains(c)).collect();
        for (index, (text, _)) in choices.iter().enumerate() {
            dialog.add_button(&tr!(*text), gtk::ResponseType::Other(index as u16 + 1));
        }
        let answered = Cell::new(false);
        dialog.connect_response(move |dialog, response| {
            let decision = match response {
                gtk::ResponseType::Other(n) if n >= 1 && (n as usize) <= choices.len() => choices[n as usize - 1].1,
                _ => "cancel",
            };
            dialog.destroy();
            if !answered.replace(true) {
                resolve(decision, None);
            }
        });
        dialog.present();
    }
}

fn job_summary(result: &OperationResult) -> Option<String> {
    if !result.failures.is_empty() {
        return Some(result.failures.iter().take(8).map(|f| format!("{}: {}", f.source, f.message)).collect::<Vec<_>>().join("\n"));
    }
    if result.cancelled {
        return Some(tr!("Operation cancelled. Completed items have been retained."));
    }
    if !result.skipped.is_empty() {
        return Some(tr!("{count} item(s) skipped.", count = result.skipped.len() as i64));
    }
    None
}

fn job_done(app: &FilesApplication, entry: &Rc<JobEntry>, result: &OperationResult) {
    if let Some(generation) = entry.cut_generation.get() {
        if result.failures.is_empty() && !result.cancelled && result.skipped.is_empty() && app.clipboard_generation.get() == generation {
            app.clipboard_files.borrow_mut().clear();
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_content(None::<&gdk::ContentProvider>).ok();
            }
        }
    }
    let summary = job_summary(result);
    if let Some(window) = entry.window().and_then(|w| crate::app::files_window_for(&w)) {
        if let Some(summary) = &summary {
            window.message(summary);
        }
        window.refresh_all();
    }
    let text = summary.unwrap_or_else(|| tr!("{title} finished.", title = entry.title.as_str()));
    app.jobs.finish(entry, Some(&text), !result.failures.is_empty());
}

/// Quick Look's view of the pane.
struct QuickLookAdapter(pane::WeakPane);

impl files_widgets::quicklook::QuickLookPane for QuickLookAdapter {
    fn closed(&self) -> bool {
        self.0.upgrade().is_none_or(|p| p.closed())
    }
    fn move_selection(&self, delta: i32) -> Option<Item> {
        self.0.upgrade()?.move_selection(delta as i64).map(|i| Rc::new(i) as Item)
    }
    fn activate(&self, item: &Item) {
        if let (Some(pane), Some(item)) = (self.0.upgrade(), item.as_any().downcast_ref::<FileItem>()) {
            pane.activate(Some(item.clone()));
        }
    }
}

// Places, location menus, mounts, settings, session and closing.
impl FilesWindow {
    pub fn refresh_sidebar(&self) {
        for usage in self.disk_usage.borrow_mut().drain(..) {
            usage.close();
        }
        let in_sidebar = self.popover.borrow().as_ref().is_some_and(|p| p.is_ancestor(&self.sidebar));
        if in_sidebar {
            self.dismiss_context_menu();
        }
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        let heading = |text: &str| {
            let label = gtk::Label::builder().label(text).xalign(0.).build();
            label.add_css_class("section-heading");
            self.sidebar.append(&label);
        };
        let app = self.app();
        heading("Places");
        let home = suite_core::paths::home().to_string_lossy().into_owned();
        self.place("Home", PlaceIcon::Name("user-home-symbolic"), Visit::Uri(home.clone()), None, Some(as_file(&home)), None, None, false);
        for (name, kind, icon) in [("Desktop", glib::UserDirectory::Desktop, "user-desktop-symbolic"),
                                   ("Downloads", glib::UserDirectory::Downloads, "folder-download-symbolic"),
                                   ("Documents", glib::UserDirectory::Documents, "folder-documents-symbolic"),
                                   ("Pictures", glib::UserDirectory::Pictures, "folder-pictures-symbolic"),
                                   ("Music", glib::UserDirectory::Music, "folder-music-symbolic"),
                                   ("Videos", glib::UserDirectory::Videos, "folder-videos-symbolic")] {
            if let Some(path) = glib::user_special_dir(kind) {
                let path = path.to_string_lossy().into_owned();
                self.place(name, PlaceIcon::Name(icon), Visit::Uri(path.clone()), None, Some(as_file(&path)), None, None, false);
            }
        }
        self.place("Trash", PlaceIcon::Name("user-trash-symbolic"), Visit::Uri("trash:///".into()), None, Some(as_file("trash:///")), None, None, false);
        self.place("Network", PlaceIcon::Name("network-workgroup-symbolic"), Visit::Uri("network:///".into()), None, Some(as_file("network:///")),
                   None, None, false);
        let bookmarks = app.bookmarks().borrow().list();
        if !bookmarks.is_empty() {
            heading("Bookmarks");
            for bookmark in bookmarks {
                let title = if bookmark.label.is_empty() {
                    as_file(&bookmark.uri).basename().map(|b| b.to_string_lossy().into_owned()).filter(|b| !b.is_empty()).unwrap_or_else(|| bookmark.uri.clone())
                } else {
                    bookmark.label.clone()
                };
                let icon: gio::Icon = gio::ThemedIcon::from_names(&["bookmarks-symbolic", "user-bookmarks-symbolic"]).upcast();
                self.place(&title, PlaceIcon::Gicon(icon), Visit::Uri(bookmark.uri.clone()), None, None, Some(bookmark.uri.clone()), None, false);
            }
        }
        let mounts = self.volumes.mounts();
        let volumes: Vec<gio::Volume> = self.volumes.volumes().into_iter().filter(|v| v.get_mount().is_none()).collect();
        heading("Devices");
        let mut shown_paths: Vec<String> = Vec::new();
        let mut shown_devices: Vec<String> = Vec::new();
        for mount in &mounts {
            let root = mount.root();
            if let Some(path) = root.path() {
                shown_paths.push(path.to_string_lossy().into_owned());
                if let Some(device) = mount.volume().and_then(|v| v.identifier(gio::VOLUME_IDENTIFIER_KIND_UNIX_DEVICE)) {
                    shown_devices.push(device.to_string());
                }
                let unmount = (mount.can_unmount() || mount.can_eject()).then(|| mount.clone());
                self.place(&mount.name(), PlaceIcon::Gicon(mount.icon()), Visit::Uri(root.uri().to_string()), unmount, Some(root.clone()), None, None, true);
            }
        }
        for disk in files_model::devices::local_disks() {
            let path = disk.path.clone();
            if shown_paths.contains(&path) || (!disk.device.is_empty() && shown_devices.contains(&disk.device)) {
                continue;
            }
            shown_paths.push(path.clone());
            self.place(&disk.name, PlaceIcon::Gicon(disk.icon.clone()), Visit::Uri(path.clone()), None, Some(as_file(&path)), None, None, true);
        }
        if !shown_paths.iter().any(|p| p == "/") {
            self.place("System", PlaceIcon::Name("drive-harddisk-symbolic"), Visit::Uri("/".into()), None, Some(as_file("/")), None, None, true);
        }
        for volume in volumes {
            self.place(&volume.name(), PlaceIcon::Gicon(volume.icon()), Visit::Volume(volume.clone()), None, None, None, Some(volume.clone()), true);
        }
        let remote: Vec<&gio::Mount> = mounts.iter().filter(|m| m.root().path().is_none()).collect();
        if !remote.is_empty() {
            heading("Network Locations");
            for mount in remote {
                let unmount = (mount.can_unmount() || mount.can_eject()).then(|| mount.clone());
                self.place(&mount.name(), PlaceIcon::Gicon(mount.icon()), Visit::Uri(mount.root().uri().to_string()), unmount, Some(mount.root()),
                           None, None, false);
            }
        }
        let searches = bridge::pget(&app.preferences(), "search_saved").as_array().cloned().unwrap_or_default();
        if !searches.is_empty() {
            heading("Saved Searches");
            for value in searches {
                let name = value.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                let uri = value.get("uri").and_then(Value::as_str).unwrap_or_default().to_string();
                self.place(&name, PlaceIcon::Name("system-search-symbolic"), Visit::Search(value.clone()), None, Some(as_file(&uri)), None, None, false);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn place(&self, title: &str, icon: PlaceIcon, visit: Visit, unmount: Option<gio::Mount>, location: Option<gio::File>,
             bookmark_uri: Option<String>, volume: Option<gio::Volume>, disk: bool) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        // 16px picks each theme's 16px symbolic art.
        let picture = match icon {
            PlaceIcon::Name(name) => suite_gtk::utils::image(name, 16),
            PlaceIcon::Gicon(icon) => gtk::Image::from_gicon(&icon),
        };
        picture.set_pixel_size(16);
        content.append(&picture);
        let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        words.append(&gtk::Label::builder().label(title).xalign(0.).hexpand(true).ellipsize(pango::EllipsizeMode::End).max_width_chars(20).build());
        content.append(&words);
        if disk {
            match &location {
                Some(location) => {
                    let usage = files_widgets::devices::DiskUsage::new(location);
                    words.append(usage.widget());
                    self.disk_usage.borrow_mut().push(usage);
                }
                None => {
                    let label = gtk::Label::builder().label("Not mounted").xalign(0.).build();
                    label.add_css_class("muted");
                    words.append(&label);
                }
            }
        }
        let button = gtk::Button::builder().child(&content).hexpand(true).tooltip_text(title).build();
        button.add_css_class("profile-row");
        let weak = self.downgrade();
        button.connect_clicked(move |_| {
            let Some(window) = weak.upgrade().map(FilesWindow) else { return };
            match &visit {
                Visit::Uri(uri) => {
                    if let Some(pane) = window.active_pane() {
                        pane.navigate(&as_file(uri), true, None);
                    }
                }
                Visit::Volume(volume) => window.mount_volume(volume, None),
                Visit::Search(saved) => {
                    let Some(pane) = window.active_pane() else { return };
                    pane.navigate(&as_file(saved.get("uri").and_then(Value::as_str).unwrap_or_default()), true, None);
                    pane.show_search();
                    pane.search_entry.set_text(saved.get("query").and_then(Value::as_str).unwrap_or_default());
                    pane.contents_check.set_active(saved.get("contents").is_some_and(suite_core::pyjson::truthy));
                    pane.search();
                }
            }
        });
        row.append(&button);
        let target = location.clone().or_else(|| bookmark_uri.as_deref().map(as_file));
        self.bind_location_menu_full(&button, target.as_ref(), bookmark_uri, volume, None);
        if let Some(mount) = unmount {
            let weak = self.downgrade();
            let remove = tool_icon("media-eject-symbolic", "Unmount", move || {
                if let Some(window) = weak.upgrade() {
                    FilesWindow(window).unmount(&mount)
                }
            });
            remove.add_css_class("files-sidebar-action");
            remove.set_valign(gtk::Align::Center);
            row.append(&remove);
        }
        self.sidebar.append(&row);
    }

    fn refresh_disk_usage(&self) -> glib::ControlFlow {
        if !self.alive.get() {
            return glib::ControlFlow::Break;
        }
        if self.window.is_visible() && self.sidebar_scroll.is_visible() {
            for usage in self.disk_usage.borrow().iter() {
                usage.refresh();
            }
        }
        glib::ControlFlow::Continue
    }

    /// Capture the clicked folder, independent of file-view selection/focus.
    pub fn bind_location_menu_full(&self, button: &gtk::Button, location: Option<&gio::File>, bookmark_uri: Option<String>,
                                   volume: Option<gio::Volume>, pane: Option<&BrowserPane>) {
        tile::set_location_uri(button, location.map(|l| l.uri().to_string()));
        // Controllers belong to the button; retaining it (or its pane/window)
        // in their closures keeps discarded breadcrumbs and tabs alive.
        let target = location.cloned();
        let window_ref = self.downgrade();
        let pane_ref = pane.map(|p| p.downgrade());
        let show: Rc<dyn Fn(&gtk::Widget, f64, f64)> = Rc::new(move |button: &gtk::Widget, x: f64, y: f64| {
            let Some(window) = window_ref.upgrade().map(FilesWindow) else { return };
            if !window.alive.get() {
                return;
            }
            if let Some(pane) = pane_ref.as_ref().and_then(|p| p.upgrade()) {
                window.set_active_pane(&pane);
            }
            window.location_context_menu(button, target.as_ref(), x, y, bookmark_uri.clone(), volume.clone());
        });
        let click = gtk::GestureClick::builder().button(3).build();
        let s = show.clone();
        click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(widget) = gesture.widget() {
                s(&widget, x, y);
            }
        });
        button.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |controller, key, _, modifiers| {
            if key == gdk::Key::Menu || (key == gdk::Key::F10 && modifiers.contains(gdk::ModifierType::SHIFT_MASK)) {
                if let Some(button) = controller.widget() {
                    show(&button, 0., button.height() as f64);
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        button.add_controller(keys);
    }

    pub fn bookmark_context_menu(&self, button: &gtk::Widget, uri: &str, x: f64, y: f64) {
        self.location_context_menu(button, Some(&as_file(uri)), x, y, Some(uri.to_string()), None);
    }

    pub fn location_context_menu(&self, button: &gtk::Widget, location: Option<&gio::File>, x: f64, y: f64, bookmark_uri: Option<String>,
                                 volume: Option<gio::Volume>) {
        self.dismiss_context_menu();
        let menu = gio::Menu::new();
        for (label, action) in [("Open in New Tab", "tab"), ("Open in New Window", "window"), ("Open in Terminal", "terminal")] {
            menu.append(Some(label), Some(&format!("location.{action}")));
        }
        if location.is_some_and(|l| l.equal(&gio::File::for_uri("trash:///"))) {
            let section = gio::Menu::new();
            section.append(Some("Empty Trash…"), Some("location.empty-trash"));
            menu.append_section(None, &section);
        }
        if bookmark_uri.is_some() {
            let section = gio::Menu::new();
            section.append(Some("Remove Bookmark"), Some("bookmark.remove"));
            menu.append_section(None, &section);
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        let folders = gio::SimpleActionGroup::new();
        let empty = gio::SimpleAction::new("empty-trash", None);
        let weak = self.downgrade();
        empty.connect_activate(move |_, _| {
            if let Some(window) = weak.upgrade().map(FilesWindow) {
                window.dismiss_context_menu();
                window.empty_trash();
            }
        });
        folders.add_action(&empty);
        for mode in ["tab", "window", "terminal"] {
            let action = gio::SimpleAction::new(mode, None);
            let enabled = (volume.is_some() || location.is_some()) && (mode != "terminal" || volume.is_some() || location.is_some_and(|l| l.path().is_some()));
            action.set_enabled(enabled);
            let (weak, location, volume) = (self.downgrade(), location.cloned(), volume.clone());
            action.connect_activate(move |_, _| {
                let Some(window) = weak.upgrade().map(FilesWindow) else { return };
                window.dismiss_context_menu();
                let w = window.downgrade();
                let opened = move |target: gio::File| {
                    let Some(window) = w.upgrade().map(FilesWindow) else { return };
                    if !window.alive.get() {
                        return;
                    }
                    match mode {
                        "tab" => {
                            window.new_tab(Some(&target), None);
                        }
                        "window" => {
                            let other = FilesWindow::new(&window.app());
                            other.new_tab(Some(&target), None);
                            other.window.present();
                        }
                        _ => {
                            if let Err(error) = window.launch_terminal(&target) {
                                window.message(&error);
                            }
                        }
                    }
                };
                if let Some(volume) = &volume {
                    window.mount_volume(volume, Some(Box::new(opened)));
                } else if let Some(location) = &location {
                    opened(location.clone());
                }
            });
            folders.add_action(&action);
        }
        popover.insert_action_group("location", Some(&folders));
        let actions = gio::SimpleActionGroup::new();
        let remove = gio::SimpleAction::new("remove", None);
        let weak = self.downgrade();
        remove.connect_activate(move |_, _| {
            let Some(window) = weak.upgrade().map(FilesWindow) else { return };
            window.dismiss_context_menu();
            let app = window.app();
            if let Some(uri) = &bookmark_uri {
                if let Err(error) = app.bookmarks().borrow().remove(uri) {
                    window.message(&error.to_string());
                }
            }
            for other in app.files_windows() {
                if other.alive.get() {
                    other.refresh_sidebar();
                }
            }
        });
        actions.add_action(&remove);
        popover.insert_action_group("bookmark", Some(&actions));
        self.present_context_menu(popover.upcast_ref(), button, x, y);
    }

    pub fn mount_operation(&self) -> gtk::MountOperation {
        gtk::MountOperation::new(Some(self.window.upcast_ref::<gtk::Window>()))
    }

    pub fn mount_location(&self, location: &gio::File, callback: Box<dyn Fn()>) {
        let weak = self.downgrade();
        location.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&self.mount_operation()), None::<&gio::Cancellable>, move |result| {
            match result {
                Ok(()) => callback(),
                Err(error) if error.matches(gio::IOErrorEnum::AlreadyMounted) => callback(),
                Err(error) => {
                    if let Some(window) = weak.upgrade() {
                        FilesWindow(window).message(error.message());
                    }
                }
            }
        });
    }

    pub fn mount_volume(&self, volume: &gio::Volume, callback: Option<Box<dyn Fn(gio::File)>>) {
        let weak = self.downgrade();
        let callback: Box<dyn Fn(gio::File)> = callback.unwrap_or_else(|| Box::new(move |root| {
            if let Some(window) = weak.upgrade().map(FilesWindow) {
                if let (Some(pane), true) = (window.active_pane(), window.alive.get()) {
                    pane.navigate(&root, true, None);
                }
            }
        }));
        if let Some(mount) = volume.get_mount() {
            callback(mount.root());
            return;
        }
        let weak = self.downgrade();
        let v = volume.clone();
        volume.mount(gio::MountMountFlags::NONE, Some(&self.mount_operation()), None::<&gio::Cancellable>, move |result| match result {
            Ok(()) => {
                if let Some(mount) = v.get_mount() {
                    callback(mount.root());
                }
            }
            Err(error) => {
                if let Some(window) = weak.upgrade() {
                    FilesWindow(window).message(error.message());
                }
            }
        });
    }

    pub fn unmount(&self, mount: &gio::Mount) {
        let weak = self.downgrade();
        let done = move |result: Result<(), glib::Error>| {
            if let (Err(error), Some(window)) = (result, weak.upgrade()) {
                FilesWindow(window).message(error.message());
            }
        };
        if mount.can_eject() {
            mount.eject_with_operation(gio::MountUnmountFlags::NONE, Some(&self.mount_operation()), None::<&gio::Cancellable>, done);
        } else {
            mount.unmount_with_operation(gio::MountUnmountFlags::NONE, Some(&self.mount_operation()), None::<&gio::Cancellable>, done);
        }
    }

    fn connect_server(&self) {
        let weak = self.downgrade();
        dialogs::prompt_text(Some(self.window.upcast_ref()), "Connect to Server", "Server address (smb://, sftp://, ftp://, davs://)", Some("sftp://"),
            move |uri| {
                let (Some(uri), Some(window)) = (uri, weak.upgrade().map(FilesWindow)) else { return };
                if uri.is_empty() {
                    return;
                }
                let target = as_file(&uri);
                let (w, t) = (window.downgrade(), target.clone());
                window.mount_location(&target, Box::new(move || {
                    if let Some(pane) = w.upgrade().and_then(|w| FilesWindow(w).active_pane()) {
                        pane.navigate(&t, true, None);
                    }
                }));
            });
    }

    pub fn empty_trash(&self) {
        let weak = self.downgrade();
        self.confirm("Empty Trash", "Permanently delete all items in the Trash? This cannot be undone.", Box::new(move |yes| {
            let (true, Some(window)) = (yes, weak.upgrade().map(FilesWindow)) else { return };
            let loader = DirectoryLoader::default();
            let files: Rc<RefCell<Vec<gio::File>>> = Rc::default();
            let (collect, w) = (files.clone(), window.downgrade());
            loader.start(&gio::File::for_uri("trash:///"), move |items: Vec<FileItem>| collect.borrow_mut().extend(items.iter().map(|i| i.file())),
                move |error| {
                    let Some(window) = w.upgrade().map(FilesWindow) else { return };
                    match error {
                        Some(error) => window.message(&suite_core::pyerr::python_error_text(&error)),
                        None => {
                            let files = std::mem::take(&mut *files.borrow_mut());
                            window.run_operation("delete", files, None);
                        }
                    }
                });
            *window.trash_loader.borrow_mut() = Some(loader);
        }));
    }

    pub fn settings(&self) {
        let app = self.app();
        let dialog = gtk::Window::builder().title("Files Settings").transient_for(&self.window).modal(true).default_width(450).application(&app.app).build();
        dialog.set_titlebar(Some(&suite_gtk::utils::headerbar("Files Settings")));
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        suite_gtk::utils::set_margins(&body, 20);
        dialog.set_child(Some(&body));
        let prefs = app.preferences();
        for (title, key) in [("Restore tabs on launch", "restore_session"), ("Remember views for each folder", "remember_folders"),
                             ("Show hidden files", "show_hidden"), ("Show previews", "show_previews"), ("Folders first", "folders_first"),
                             ("Single click to open items", "single_click"), ("Show status bar", "show_statusbar"),
                             ("Peek into folders on hover", "folder_peek"), ("Vim-style keys (h j k l)", "vim_keys")] {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
            row.append(&gtk::Label::builder().label(title).hexpand(true).xalign(0.).build());
            let toggle = gtk::Switch::builder().active(bridge::pbool(&prefs, key)).valign(gtk::Align::Center).build();
            let (weak, prefs) = (self.downgrade(), prefs.clone());
            toggle.connect_active_notify(move |widget| {
                let _ = bridge::pset(&prefs, key, Value::Bool(widget.is_active()));
                if let Some(window) = weak.upgrade() {
                    FilesWindow(window).refresh_all();
                }
            });
            row.append(&toggle);
            body.append(&row);
        }
        let default_button = gtk::Button::with_label("Use Files as the Default File Manager");
        let d = dialog.downgrade();
        default_button.connect_clicked(move |button| {
            let result = match waydock::core::app_catalog::desktop_info("org.waydock.Files.desktop") {
                None => Err("Install Files before selecting it as the default file manager.".to_string()),
                Some(info) => info.set_as_default_for_type("inode/directory")
                    .and_then(|_| info.set_as_default_for_type("x-scheme-handler/trash")).map_err(|e| suite_core::pyerr::python_error_text(&e)),
            };
            match result {
                Ok(()) => {
                    button.set_label("Files Is the Default File Manager");
                    button.set_sensitive(false);
                }
                Err(error) => {
                    if let Some(dialog) = d.upgrade() {
                        dialogs::show_error(Some(&dialog), &error);
                    }
                }
            }
        });
        body.append(&default_button);
        let done = gtk::Button::with_label("Done");
        let d = dialog.downgrade();
        done.connect_clicked(move |_| {
            if let Some(dialog) = d.upgrade() {
                dialog.destroy();
            }
        });
        body.append(&done);
        dialog.present();
    }

    pub fn save_session(&self, now: bool) {
        if self.pages().is_empty() || !self.alive.get() {
            return;
        }
        if let Some(source) = self.session_source.borrow_mut().take() {
            source.remove();
        }
        if now {
            self.save_session_now();
        } else {
            let weak = self.downgrade();
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                if let Some(window) = weak.upgrade().map(FilesWindow) {
                    *window.session_source.borrow_mut() = None;
                    window.save_session_now();
                }
            });
            *self.session_source.borrow_mut() = Some(source);
        }
    }

    fn save_session_now(&self) {
        if !self.alive.get() {
            return;
        }
        let pages = self.pages();
        let tabs: Vec<Value> = pages.iter().map(|p| Value::String(p.left.current().uri().to_string())).collect();
        let splits: Vec<Value> = pages.iter().map(|p| Value::String(p.right().map(|r| r.current().uri().to_string()).unwrap_or_default())).collect();
        let active = self.notebook.current_page().map(|p| p as i64).unwrap_or(0).max(0);
        let size = vec![Value::from(720.max(self.window.width())), Value::from(480.max(self.window.height()))];
        let prefs = self.app().preferences();
        let mut changes = serde_json::Map::new();
        for (key, value) in [("tabs", Value::Array(tabs)), ("tab_splits", Value::Array(splits)), ("active_tab", Value::from(active)),
                             ("window_size", Value::Array(size))] {
            if bridge::pget(&prefs, key) != value {
                changes.insert(key.into(), value);
            }
        }
        if !changes.is_empty() {
            let _ = bridge::pupdate(&prefs, changes);
        }
    }

    fn close_requested(&self) -> glib::Propagation {
        let app = self.app();
        let jobs = &app.jobs;
        let me: gtk::Window = self.window.clone().upcast();
        let others = jobs.windows().into_iter().any(|w| w != me);
        // Jobs keep running after their window closes (the application holds
        // itself); ask only about this window's jobs, or all of them when it is
        // the last window.
        let owned = if others { jobs.owned_by(&me) } else { jobs.entries() };
        if !owned.is_empty() && !self.close_confirmed.get() {
            let weak = self.downgrade();
            let answered = Box::new(move |choice: Option<KeepAnswer>| {
                let (Some(choice), Some(window)) = (choice, weak.upgrade().map(FilesWindow)) else { return };
                if choice == KeepAnswer::Cancel {
                    window.app().jobs.cancel_all(Some(&owned));
                }
                window.close_confirmed.set(true);
                let w = window.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(window) = w.upgrade() {
                        window.window.close();
                    }
                });
            });
            app.ask_keep_running(Some(&me), answered);
            return glib::Propagation::Stop;
        }
        self.save_session(true);
        self.jobs_button.close();
        if let Some(look) = self.quicklook.borrow_mut().take() {
            look.close();
        }
        self.save_information_width();
        self.information.close();
        self.alive.set(false);
        jobs::set_window_alive(&self.window, false);
        // A background job can keep the process alive after its last window
        // closes; the next window should still reopen this session.
        if !app.files_windows().iter().any(|w| w != self && w.alive.get()) {
            app.session_restored.set(false);
        }
        if let Some(timer) = self.disk_usage_timer.borrow_mut().take() {
            timer.remove();
        }
        if let (Some(monitor), Some(id)) = (self.mount_monitor.borrow().clone(), self.mount_signal.borrow_mut().take()) {
            monitor.disconnect(id);
        }
        for usage in self.disk_usage.borrow_mut().drain(..) {
            usage.close();
        }
        if let Some(loader) = self.trash_loader.borrow_mut().take() {
            loader.cancel();
        }
        self.dismiss_context_menu();
        for page in self.pages() {
            page.close();
        }
        for id in self.volume_signals.borrow_mut().drain(..) {
            self.volumes.disconnect(id);
        }
        if let Some(id) = self.switch_signal.borrow_mut().take() {
            self.notebook.disconnect(id);
        }
        app.unregister_window(self);
        glib::Propagation::Proceed
    }
}

enum PlaceIcon {
    Name(&'static str),
    Gicon(gio::Icon),
}

enum Visit {
    Uri(String),
    Volume(gio::Volume),
    Search(Value),
}

impl PaneHost for WindowInner {
    fn message(&self, text: &str) {
        if let Some(w) = self.me() {
            w.message(text)
        }
    }
    fn selection_changed(&self, pane: &BrowserPane) {
        if let Some(w) = self.me() {
            w.selection_changed_impl(pane)
        }
    }
    fn pane_changed(&self, pane: &BrowserPane) {
        if let Some(w) = self.me() {
            w.pane_changed_impl(pane)
        }
    }
    fn focus_pane(&self, pane: &BrowserPane) {
        if let Some(w) = self.me() {
            if w.active_pane().as_ref() != Some(pane) {
                w.set_active_pane(pane);
            }
        }
    }
    fn context_menu(&self, pane: &BrowserPane, widget: &gtk::Widget, x: f64, y: f64) {
        if let Some(w) = self.me() {
            w.context_menu(pane, widget, x, y)
        }
    }
    fn run_operation(&self, kind: &str, files: Vec<gio::File>, destination: Option<gio::File>) -> bool {
        self.me().and_then(|w| w.run_operation(kind, files, destination)).is_some()
    }
    fn new_tab(&self, location: &gio::File) {
        if let Some(w) = self.me() {
            w.new_tab(Some(location), None);
        }
    }
    fn open_files(&self, files: Vec<gio::File>) {
        if let Some(w) = self.me() {
            w.open_files(files)
        }
    }
    fn mount_location(&self, location: &gio::File, done: Box<dyn Fn()>) {
        if let Some(w) = self.me() {
            w.mount_location(location, done)
        }
    }
    fn quick_look(&self, pane: &BrowserPane) -> bool {
        if let Some(w) = self.me() {
            w.quick_look(Some(pane));
        }
        true
    }
    fn bind_location_menu(&self, button: &gtk::Button, location: &gio::File, pane: &BrowserPane) {
        if let Some(w) = self.me() {
            w.bind_location_menu_full(button, Some(location), None, None, Some(pane));
        }
    }
    fn action(&self, name: &str) {
        if let Some(w) = self.me() {
            w.action(name)
        }
    }
    fn mount_operation(&self) -> Option<gtk::MountOperation> {
        self.me().map(|w| w.mount_operation())
    }
}

impl WindowInner {
    fn me(&self) -> Option<FilesWindow> {
        crate::app::files_window_for(&self.window)
    }
}
