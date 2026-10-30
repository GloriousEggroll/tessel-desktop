//! The main window (Ark's part + shell): entry tree with sortable columns,
//! information panel, filter bar, status bar with job progress, every
//! archive action, context menu, drag and drop in both directions.

use super::app::App;
use super::dialogs;
use super::dnd::{self, ArkContent, Exported};
use super::forms;
use super::model::{self, EntryObject};
use super::progress::{conflict_handler, with_password, Work};
use crate::archive::{AddItem, CreateOptions, Error, Listing};
use crate::backend::process::TempDir;
use crate::job::{JobHandle, JobUi};
use crate::ops::{self, Archive, ExtractOptions};
use archive_settings::plugins::Kind;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use suite_core::tr;

pub const COLUMNS: &[(&str, &str)] = &[
    ("name", "Name"), ("size", "Size"), ("compressed", "Compressed"), ("ratio", "Ratio"), ("type", "Type"), ("modified", "Modified"),
    ("permissions", "Permissions"), ("owner", "Owner"), ("group", "Group"), ("crc", "CRC"), ("method", "Method"), ("version", "Version"),
];

fn column_titles() -> [String; 12] {
    [tr!("Name"), tr!("Size"), tr!("Compressed"), tr!("Ratio"), tr!("Type"), tr!("Modified"), tr!("Permissions"), tr!("Owner"),
     tr!("Group"), tr!("CRC"), tr!("Method"), tr!("Version")]
}

pub struct InfoPanel {
    pub root: gtk::Box,
    pub icon: gtk::Image,
    pub name: gtk::Label,
    pub details: gtk::Label,
    pub comment_title: gtk::Label,
    pub comment: gtk::TextView,
    pub comment_scroll: gtk::ScrolledWindow,
}

/// A new archive that has no file yet (Ark writes it with the first files added).
#[derive(Clone)]
pub struct Pending {
    pub path: PathBuf,
    pub kind: Kind,
    pub options: CreateOptions,
}

pub struct Inner {
    pub app: App,
    pub window: gtk::ApplicationWindow,
    pub title: gtk::Label,
    pub archive: RefCell<Option<Archive>>,
    pub pending: RefCell<Option<Pending>>,
    pub stack: gtk::Stack,
    pub view: gtk::ColumnView,
    pub selection: gtk::MultiSelection,
    pub sort_model: gtk::SortListModel,
    pub columns: Vec<(&'static str, gtk::ColumnViewColumn)>,
    pub search_bar: gtk::SearchBar,
    pub search: gtk::SearchEntry,
    pub paned: gtk::Paned,
    pub info: InfoPanel,
    pub status: gtk::Label,
    pub progress: gtk::ProgressBar,
    pub cancel: gtk::Button,
    pub progress_box: gtk::Box,
    pub job: Rc<RefCell<Option<JobHandle>>>,
    pub busy: Cell<bool>,
    pub clipboard: RefCell<Option<(Vec<String>, bool)>>,
    pub exports: RefCell<Vec<Rc<Exported>>>,
    pub scratch: RefCell<Vec<TempDir>>,
    pub monitors: RefCell<Vec<gio::FileMonitor>>,
    pub drag: RefCell<Option<ArkContent>>,
    pub drop_highlight: Cell<bool>,
    pub recent_menu: gio::Menu,
    pub empty_hint: gtk::Label,
    pub closed: Cell<bool>,
    /// Ark D-Bus extraction requests received (drag and drop).
    pub dnd_calls: Cell<u32>,
}

#[derive(Clone)]
pub struct ArchiveWindow(pub Rc<Inner>);

impl PartialEq for ArchiveWindow {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::ops::Deref for ArchiveWindow {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

#[derive(Clone)]
pub struct WeakWindow(Weak<Inner>);

impl WeakWindow {
    pub fn upgrade(&self) -> Option<ArchiveWindow> {
        self.0.upgrade().map(ArchiveWindow)
    }
}

const ENTRY_KEY: &str = "wayarchive-entry";

fn set_widget_entry(widget: &impl IsA<gtk::Widget>, entry: Option<&EntryObject>) {
    unsafe {
        match entry {
            Some(e) => widget.as_ref().set_data(ENTRY_KEY, e.clone()),
            None => {
                let _ = widget.as_ref().steal_data::<EntryObject>(ENTRY_KEY);
            }
        }
    }
}

fn widget_entry(widget: &gtk::Widget) -> Option<EntryObject> {
    let mut current = Some(widget.clone());
    while let Some(w) = current {
        if let Some(e) = unsafe { w.data::<EntryObject>(ENTRY_KEY) } {
            return Some(unsafe { e.as_ref() }.clone());
        }
        if w.is::<gtk::ColumnView>() {
            return None;
        }
        current = w.parent();
    }
    None
}

fn text_for(entry: &crate::archive::Entry, key: &str) -> String {
    match key {
        "size" => if entry.is_dir { String::new() } else { super::format_size(entry.size) },
        "compressed" => entry.compressed.map(super::format_size).unwrap_or_default(),
        "ratio" => entry.ratio().map(|r| format!("{:.0} %", r * 100.0)).unwrap_or_default(),
        "type" => super::type_description(entry),
        "modified" => entry.mtime.map(super::format_time).unwrap_or_default(),
        "permissions" => entry.mode.map(|m| super::permissions(m, entry.is_dir)).unwrap_or_default(),
        "owner" => entry.owner.clone().unwrap_or_default(),
        "group" => entry.group.clone().unwrap_or_default(),
        "crc" => entry.crc.clone().unwrap_or_default(),
        "method" => entry.method.clone().unwrap_or_default(),
        "version" => entry.version.clone().unwrap_or_default(),
        _ => entry.name().to_string(),
    }
}

fn compare(a: &EntryObject, b: &EntryObject, key: &str) -> std::cmp::Ordering {
    let (a, b) = (a.node(), b.node());
    let (x, y) = (&a.entry, &b.entry);
    let folders = y.is_dir.cmp(&x.is_dir);
    if folders != std::cmp::Ordering::Equal {
        return folders;
    }
    let order = match key {
        "size" => x.size.cmp(&y.size),
        "compressed" => x.compressed.cmp(&y.compressed),
        "ratio" => x.ratio().partial_cmp(&y.ratio()).unwrap_or(std::cmp::Ordering::Equal),
        "modified" => x.mtime.cmp(&y.mtime),
        "permissions" => x.mode.cmp(&y.mode),
        "name" => model::natural_cmp(x.name(), y.name()),
        _ => text_for(x, key).cmp(&text_for(y, key)),
    };
    order.then_with(|| model::natural_cmp(x.name(), y.name()))
}

fn column(key: &'static str, title: &str) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if key == "name" {
            let expander = gtk::TreeExpander::new();
            expander.set_indent_for_icon(true);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let image = gtk::Image::new();
            let label = gtk::Label::builder().xalign(0.0).ellipsize(pango::EllipsizeMode::Middle).build();
            row.append(&image);
            row.append(&label);
            expander.set_child(Some(&row));
            item.set_child(Some(&expander));
        } else {
            let label = gtk::Label::builder().xalign(if matches!(key, "size" | "compressed" | "ratio") { 1.0 } else { 0.0 })
                .ellipsize(pango::EllipsizeMode::End).build();
            if matches!(key, "crc" | "permissions") {
                label.add_css_class("monospace");
            }
            item.set_child(Some(&label));
        }
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else { return };
        let Some(object) = row.item().and_downcast::<EntryObject>() else { return };
        let entry = object.entry();
        let Some(child) = item.child() else { return };
        set_widget_entry(&child, Some(&object));
        if key == "name" {
            let expander = child.downcast_ref::<gtk::TreeExpander>().expect("expander");
            expander.set_list_row(Some(&row));
            if let Some(b) = expander.child() {
                let image = b.first_child().and_downcast::<gtk::Image>().expect("icon");
                image.set_from_gicon(&super::icon_for(&entry));
                let label = image.next_sibling().and_downcast::<gtk::Label>().expect("label");
                label.set_label(entry.name());
                if entry.encrypted {
                    label.set_tooltip_text(Some(&tr!("Encrypted")));
                } else {
                    label.set_tooltip_text(None);
                }
            }
        } else if let Some(label) = child.downcast_ref::<gtk::Label>() {
            label.set_label(&text_for(&entry, key));
        }
    });
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let Some(child) = item.child() {
            set_widget_entry(&child, None);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_resizable(true);
    if key == "name" {
        column.set_expand(true);
        column.set_fixed_width(260);
    } else {
        column.set_fixed_width(match key {
            "type" | "modified" => 150,
            "permissions" => 110,
            "ratio" | "version" => 70,
            _ => 95,
        });
    }
    let sorter = gtk::CustomSorter::new(move |a, b| {
        match (a.downcast_ref::<EntryObject>(), b.downcast_ref::<EntryObject>()) {
            (Some(a), Some(b)) => compare(a, b, key).into(),
            _ => gtk::Ordering::Equal,
        }
    });
    column.set_sorter(Some(&sorter));
    column
}

impl ArchiveWindow {
    pub fn downgrade(&self) -> WeakWindow {
        WeakWindow(Rc::downgrade(&self.0))
    }

    pub fn new(app: &App) -> ArchiveWindow {
        let window = gtk::ApplicationWindow::builder().application(&app.app).title(tr!("Archive Manager")).default_width(980).default_height(640).build();
        window.add_css_class("archive-window");
        window.set_widget_name("archive-window");
        let header = suite_gtk::utils::headerbar(&tr!("Archive Manager"));
        let title = header.title_widget().and_downcast::<gtk::Label>()
            .or_else(|| header.title_widget().and_then(|w| w.first_child()).and_downcast::<gtk::Label>())
            .unwrap_or_else(|| {
                let l = gtk::Label::new(Some(&tr!("Archive Manager")));
                header.set_title_widget(Some(&l));
                l
            });
        title.set_ellipsize(pango::EllipsizeMode::Middle);
        window.set_titlebar(Some(&header));
        let icon_button = |icon: &str, tip: &str, action: &str| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b.set_action_name(Some(action));
            b.update_property(&[gtk::accessible::Property::Label(tip)]);
            b
        };
        header.pack_start(&icon_button("document-open-symbolic", &tr!("Open an archive (Ctrl+O)"), "win.open"));
        let extract_button = gtk::Button::builder().label(tr!("Extract")).action_name("win.extract").tooltip_text(tr!("Extract files (Ctrl+E)")).build();
        extract_button.set_widget_name("extract-button");
        header.pack_start(&extract_button);
        let add_menu = gio::Menu::new();
        add_menu.append(Some(&tr!("Add Files…")), Some("win.add-files"));
        add_menu.append(Some(&tr!("Add Folder…")), Some("win.add-folder"));
        add_menu.append(Some(&tr!("New Folder…")), Some("win.new-folder"));
        let add_button = gtk::MenuButton::builder().icon_name("list-add-symbolic").menu_model(&add_menu).tooltip_text(tr!("Add files to the archive")).build();
        add_button.set_widget_name("add-button");
        header.pack_start(&add_button);

        let menu = gio::Menu::new();
        let file_section = gio::Menu::new();
        file_section.append(Some(&tr!("New Archive…")), Some("win.new"));
        file_section.append(Some(&tr!("Open…")), Some("win.open"));
        let recent_menu = gio::Menu::new();
        file_section.append_submenu(Some(&tr!("Open Recent")), &recent_menu);
        menu.append_section(None, &file_section);
        let archive_section = gio::Menu::new();
        archive_section.append(Some(&tr!("Extract…")), Some("win.extract"));
        archive_section.append(Some(&tr!("Extract Here")), Some("win.extract-here"));
        archive_section.append(Some(&tr!("Test Integrity")), Some("win.test"));
        archive_section.append(Some(&tr!("Edit Comment…")), Some("win.comment"));
        archive_section.append(Some(&tr!("Properties")), Some("win.properties"));
        menu.append_section(None, &archive_section);
        let view_section = gio::Menu::new();
        view_section.append(Some(&tr!("Find")), Some("win.find"));
        view_section.append(Some(&tr!("Information Panel")), Some("win.info-panel"));
        view_section.append(Some(&tr!("Settings…")), Some("win.settings"));
        menu.append_section(None, &view_section);
        let menu_button = suite_gtk::app_menu::install_app_menu(&app.app, &window, suite_gtk::app_menu::MenuTarget::Model(menu),
            suite_gtk::app_menu::AboutDetails { name: archive_settings::n_!("Archive Manager").into(), icon: "package-x-generic".into(), version: Some(env!("CARGO_PKG_VERSION").into()),
                comments: Some(archive_settings::n_!("Create, open, extract and edit archives").into()), website: None }, shortcuts(), Some(&header), None);
        let _ = menu_button;
        let info_toggle = gtk::ToggleButton::builder().icon_name("view-dual-symbolic").action_name("win.info-panel").tooltip_text(tr!("Information panel (F9)")).build();
        header.pack_end(&info_toggle);
        let search_toggle = gtk::ToggleButton::builder().icon_name("system-search-symbolic").tooltip_text(tr!("Filter entries (Ctrl+F)")).build();
        search_toggle.set_widget_name("search-toggle");
        header.pack_end(&search_toggle);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Filter entries by name")).hexpand(true).build();
        search.set_widget_name("filter-entry");
        let search_bar = gtk::SearchBar::builder().child(&search).show_close_button(true).build();
        search_bar.connect_entry(&search);
        search_toggle.bind_property("active", &search_bar, "search-mode-enabled").bidirectional().build();
        root.append(&search_bar);

        let tree = model::tree_model(&[], false);
        let view = gtk::ColumnView::new(None::<gtk::MultiSelection>);
        view.set_widget_name("entries");
        view.set_show_column_separators(false);
        view.set_reorderable(true);
        let titles = column_titles();
        let mut columns = Vec::new();
        for (i, (key, _)) in COLUMNS.iter().enumerate() {
            let c = column(key, &titles[i]);
            view.append_column(&c);
            columns.push((*key, c));
        }
        // Folders first in both directions, then the column the user sorts by.
        let folders_first = gtk::CustomSorter::new(|a, b| match (a.downcast_ref::<EntryObject>(), b.downcast_ref::<EntryObject>()) {
            (Some(a), Some(b)) => b.is_dir().cmp(&a.is_dir()).into(),
            _ => gtk::Ordering::Equal,
        });
        let both = gtk::MultiSorter::new();
        both.append(folders_first);
        if let Some(columns) = view.sorter() {
            both.append(columns);
        }
        let sorter = gtk::TreeListRowSorter::new(Some(both));
        let sort_model = gtk::SortListModel::new(Some(tree), Some(sorter));
        let selection = gtk::MultiSelection::new(Some(sort_model.clone()));
        view.set_model(Some(&selection));
        view.sort_by_column(Some(&columns[0].1), gtk::SortType::Ascending);
        let scroll = gtk::ScrolledWindow::builder().child(&view).hexpand(true).vexpand(true).build();

        let info_root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        info_root.add_css_class("info-panel");
        info_root.set_widget_name("info-panel");
        info_root.set_size_request(240, -1);
        let icon = gtk::Image::builder().pixel_size(64).margin_top(8).build();
        let name = gtk::Label::builder().wrap(true).wrap_mode(pango::WrapMode::WordChar).justify(gtk::Justification::Center).selectable(true).build();
        name.add_css_class("info-name");
        let details = gtk::Label::builder().wrap(true).xalign(0.0).selectable(true).build();
        details.set_widget_name("info-details");
        let comment_title = gtk::Label::builder().label(tr!("Comment")).xalign(0.0).margin_top(8).build();
        comment_title.add_css_class("dim-label");
        let comment = gtk::TextView::builder().editable(false).wrap_mode(gtk::WrapMode::WordChar).cursor_visible(false).build();
        comment.set_widget_name("info-comment");
        let comment_scroll = gtk::ScrolledWindow::builder().child(&comment).vexpand(true).min_content_height(80).build();
        comment_scroll.add_css_class("frame");
        info_root.append(&icon);
        info_root.append(&name);
        info_root.append(&details);
        info_root.append(&comment_title);
        info_root.append(&comment_scroll);
        let info_scroll = gtk::ScrolledWindow::builder().child(&info_root).hscrollbar_policy(gtk::PolicyType::Never).build();
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&scroll));
        paned.set_end_child(Some(&info_scroll));
        paned.set_resize_end_child(false);
        paned.set_shrink_end_child(false);
        paned.set_position(700);

        let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty.add_css_class("empty-state");
        empty.set_valign(gtk::Align::Center);
        empty.set_halign(gtk::Align::Center);
        let big = gtk::Image::from_icon_name("package-x-generic");
        big.set_pixel_size(96);
        let empty_title = gtk::Label::new(Some(&tr!("No archive open")));
        empty_title.add_css_class("empty-title");
        let empty_hint = gtk::Label::builder().label(tr!("Open an archive, create a new one, or drop files here.")).wrap(true).justify(gtk::Justification::Center).build();
        empty_hint.add_css_class("dim-label");
        let empty_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        empty_buttons.set_halign(gtk::Align::Center);
        let open_b = gtk::Button::builder().label(tr!("Open…")).action_name("win.open").build();
        open_b.add_css_class("suggested-action");
        let new_b = gtk::Button::builder().label(tr!("New Archive…")).action_name("win.new").build();
        empty_buttons.append(&open_b);
        empty_buttons.append(&new_b);
        empty.append(&big);
        empty.append(&empty_title);
        empty.append(&empty_hint);
        empty.append(&empty_buttons);
        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&paned, Some("archive"));
        stack.set_vexpand(true);
        root.append(&stack);

        let status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        status_bar.add_css_class("status-bar");
        let status = gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(pango::EllipsizeMode::End).build();
        status.set_widget_name("status");
        let progress_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let progress = gtk::ProgressBar::builder().valign(gtk::Align::Center).width_request(180).build();
        progress.set_widget_name("job-progress");
        let cancel = gtk::Button::from_icon_name("process-stop-symbolic");
        cancel.set_tooltip_text(Some(&tr!("Cancel")));
        cancel.set_widget_name("job-cancel");
        cancel.add_css_class("flat");
        progress_box.append(&progress);
        progress_box.append(&cancel);
        progress_box.set_visible(false);
        status_bar.append(&status);
        status_bar.append(&progress_box);
        root.append(&status_bar);
        window.set_child(Some(&root));

        let this = ArchiveWindow(Rc::new(Inner {
            app: app.clone(), window: window.clone(), title, archive: RefCell::new(None), pending: RefCell::new(None), stack, view, selection,
            sort_model, columns, search_bar, search, paned, info: InfoPanel { root: info_root, icon, name, details, comment_title, comment, comment_scroll },
            status, progress, cancel, progress_box, job: Rc::default(), busy: Cell::new(false), clipboard: RefCell::new(None),
            exports: RefCell::new(Vec::new()), scratch: RefCell::new(Vec::new()), monitors: RefCell::new(Vec::new()), drag: RefCell::new(None),
            drop_highlight: Cell::new(false), recent_menu, empty_hint, closed: Cell::new(false), dnd_calls: Cell::new(0),
        }));
        this.install_actions();
        this.connect_signals();
        this.install_dnd();
        this.refresh_recent();
        this.apply_settings();
        let weak = this.downgrade();
        app.settings_listeners.borrow_mut().push(Rc::new(move || {
            if let Some(w) = weak.upgrade() {
                w.apply_settings();
                w.refresh_recent();
            }
        }));
        this.show_archive();
        this
    }

    pub fn archive(&self) -> Option<Archive> {
        self.archive.borrow().clone()
    }

    pub fn busy(&self) -> bool {
        self.busy.get()
    }

    pub fn parent(&self) -> Option<gtk::Window> {
        Some(self.window.clone().upcast())
    }

    fn apply_settings(&self) {
        let settings = self.app.settings();
        if let Some(action) = self.window.lookup_action("info-panel").and_downcast::<gio::SimpleAction>() {
            action.set_state(&settings.show_info_panel.to_variant());
        }
        if let Some(end) = self.paned.end_child() {
            end.set_visible(settings.show_info_panel);
        }
    }

    fn refresh_recent(&self) {
        self.recent_menu.remove_all();
        for path in self.app.settings().recent.iter().take(10) {
            let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
            let item = gio::MenuItem::new(Some(&name), None);
            item.set_action_and_target_value(Some("win.open-recent"), Some(&path.to_variant()));
            self.recent_menu.append_item(&item);
        }
    }

    fn add_action(&self, name: &str, f: impl Fn(&ArchiveWindow) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = self.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(w) = weak.upgrade() {
                f(&w)
            }
        });
        self.window.add_action(&action);
    }

    fn install_actions(&self) {
        self.add_action("open", |w| w.choose_and_open());
        let recent = gio::SimpleAction::new("open-recent", Some(glib::VariantTy::STRING));
        let weak = self.downgrade();
        recent.connect_activate(move |_, p| {
            if let (Some(w), Some(path)) = (weak.upgrade(), p.and_then(|p| p.str().map(PathBuf::from))) {
                w.open(&path);
            }
        });
        self.window.add_action(&recent);
        self.add_action("new", |w| w.new_archive(Vec::new()));
        self.add_action("add-files", |w| w.choose_and_add(false));
        self.add_action("add-folder", |w| w.choose_and_add(true));
        self.add_action("extract", |w| w.extract_dialog());
        self.add_action("extract-here", |w| w.extract_here());
        self.add_action("delete", |w| w.delete_selected());
        self.add_action("rename", |w| w.rename_selected());
        self.add_action("cut", |w| w.cut_copy(true));
        self.add_action("copy", |w| w.cut_copy(false));
        self.add_action("paste", |w| w.paste());
        self.add_action("new-folder", |w| w.new_folder());
        self.add_action("preview", |w| w.preview_selected());
        self.add_action("open-entry", |w| w.open_selected(None));
        self.add_action("open-with", |w| w.open_with_selected());
        self.add_action("test", |w| w.test());
        self.add_action("properties", |w| w.properties());
        self.add_action("comment", |w| w.edit_comment());
        self.add_action("find", |w| {
            w.search_bar.set_search_mode(true);
            w.search.grab_focus();
        });
        self.add_action("settings", |w| w.show_settings());
        self.add_action("close", |w| w.window.close());
        self.add_action("select-all", |w| {
            w.selection.select_all();
        });
        let info = gio::SimpleAction::new_stateful("info-panel", None, &true.to_variant());
        let weak = self.downgrade();
        info.connect_activate(move |action, _| {
            let Some(w) = weak.upgrade() else { return };
            let now = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(true);
            action.set_state(&now.to_variant());
            w.app.update_settings(|s| s.show_info_panel = now);
        });
        self.window.add_action(&info);
        // Keys that must not fire while typing in the filter: on the list only.
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Local);
        for (keys, action) in [("Delete", "win.delete"), ("F2", "win.rename"), ("<Control>x", "win.cut"), ("<Control>c", "win.copy"),
                               ("<Control>v", "win.paste"), ("<Control>a", "win.select-all"), ("space", "win.preview")] {
            if let Some(trigger) = gtk::ShortcutTrigger::parse_string(keys) {
                shortcuts.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(gtk::NamedAction::new(action))));
            }
        }
        self.view.add_controller(shortcuts);
    }

    fn connect_signals(&self) {
        let weak = self.downgrade();
        self.selection.connect_selection_changed(move |_, _, _| {
            if let Some(w) = weak.upgrade() {
                w.update_actions();
                w.update_info();
                w.update_status();
            }
        });
        let weak = self.downgrade();
        self.search.connect_search_changed(move |_| {
            if let Some(w) = weak.upgrade() {
                w.refresh_tree();
            }
        });
        let weak = self.downgrade();
        self.view.connect_activate(move |_, position| {
            let Some(w) = weak.upgrade() else { return };
            let Some(row) = w.selection.item(position).and_downcast::<gtk::TreeListRow>() else { return };
            let Some(object) = row.item().and_downcast::<EntryObject>() else { return };
            if object.is_dir() {
                row.set_expanded(!row.is_expanded());
            } else {
                w.selection.select_item(position, true);
                w.preview_selected();
            }
        });
        let click = gtk::GestureClick::builder().button(3).build();
        let weak = self.downgrade();
        click.connect_pressed(move |gesture, _, x, y| {
            let Some(w) = weak.upgrade() else { return };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            w.context_menu(x, y);
        });
        self.view.add_controller(click);
        let weak = self.downgrade();
        self.cancel.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                if let Some(job) = w.job.borrow().as_ref() {
                    job.cancel();
                }
            }
        });
        let weak = self.downgrade();
        self.window.connect_close_request(move |_| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            if let Some(job) = w.job.borrow().as_ref().filter(|j| j.running()) {
                job.cancel();
            }
            w.closed.set(true);
            w.monitors.borrow_mut().clear();
            for e in w.exports.borrow_mut().drain(..) {
                e.unregister();
            }
            w.scratch.borrow_mut().clear();
            w.app.forget(&w);
            glib::Propagation::Proceed
        });
    }

    // ---------------------------------------------------------------- state

    pub fn listing(&self) -> Listing {
        self.archive.borrow().as_ref().map(|a| a.listing.clone()).unwrap_or_default()
    }

    pub fn read_only(&self) -> bool {
        match self.archive.borrow().as_ref() {
            Some(a) => a.read_only(),
            None => self.pending.borrow().is_none(),
        }
    }

    pub fn set_archive(&self, archive: Option<Archive>) {
        *self.archive.borrow_mut() = archive;
        self.show_archive();
    }

    fn show_archive(&self) {
        let has = self.archive.borrow().is_some() || self.pending.borrow().is_some();
        self.stack.set_visible_child_name(if has { "archive" } else { "empty" });
        let name = self.archive.borrow().as_ref().map(|a| a.name())
            .or_else(|| self.pending.borrow().as_ref().and_then(|p| p.path.file_name().map(|n| n.to_string_lossy().into_owned())));
        let title = match &name {
            Some(n) => n.clone(),
            None => tr!("Archive Manager"),
        };
        self.title.set_label(&title);
        self.window.set_title(Some(&match &name {
            Some(n) => format!("{n} — {}", tr!("Archive Manager")),
            None => tr!("Archive Manager"),
        }));
        let columns = self.archive.borrow().as_ref().map(|a| a.listing.columns).unwrap_or_default();
        for (key, column) in &self.columns {
            let visible = match *key {
                "name" | "size" | "type" | "modified" => true,
                "compressed" | "ratio" => columns.compressed,
                "permissions" => columns.permissions,
                "owner" | "group" => columns.owner,
                "crc" => columns.crc,
                "method" => columns.method,
                "version" => columns.version,
                _ => true,
            };
            column.set_visible(visible);
        }
        self.refresh_tree();
        self.update_actions();
        self.update_info();
        self.update_status();
    }

    pub fn refresh_tree(&self) {
        let listing = self.listing();
        let filter = self.search.text().to_string();
        let filter = Some(filter.as_str()).filter(|f| !f.is_empty());
        let nodes = model::build_tree(&listing, filter);
        let tree = model::tree_model(&nodes, filter.is_some());
        self.sort_model.set_model(Some(&tree));
    }

    pub fn selected_objects(&self) -> Vec<EntryObject> {
        let set = self.selection.selection();
        (0..set.size()).filter_map(|i| self.selection.item(set.nth(i as u32))).filter_map(|o| model::row_entry(&o)).collect()
    }

    /// Selected paths without those already covered by a selected folder.
    pub fn selected_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.selected_objects().iter().map(|o| o.path()).collect();
        paths.sort();
        let mut out: Vec<String> = Vec::new();
        for p in paths {
            if !out.iter().any(|o| p.starts_with(&format!("{o}/"))) {
                out.push(p);
            }
        }
        out
    }

    /// Ark: new files go into the selected folder (or the selected file's folder).
    pub fn current_folder(&self) -> String {
        let objects = self.selected_objects();
        match objects.as_slice() {
            [one] if one.is_dir() => one.path(),
            [one] => one.entry().parent().to_string(),
            _ => String::new(),
        }
    }

    pub fn select_paths(&self, paths: &[String]) {
        self.selection.unselect_all();
        for i in 0..self.selection.n_items() {
            if let Some(o) = self.selection.item(i).as_ref().and_then(model::row_entry) {
                if paths.contains(&o.path()) {
                    self.selection.select_item(i, false);
                }
            }
        }
    }

    pub fn update_actions(&self) {
        let has = self.archive.borrow().is_some();
        let pending = self.pending.borrow().is_some();
        let busy = self.busy.get();
        let writable = !self.read_only() && !busy;
        let selected = self.selected_objects();
        let n = selected.len();
        let one_file = n == 1 && !selected[0].is_dir();
        let kind = self.archive.borrow().as_ref().map(|a| a.kind);
        let enable = |name: &str, on: bool| {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        };
        enable("extract", has && !busy);
        enable("extract-here", has && !busy);
        enable("test", has && !busy);
        enable("properties", has);
        enable("comment", has && writable && kind == Some(Kind::Zip));
        enable("add-files", (has || pending) && writable);
        enable("add-folder", (has || pending) && writable);
        enable("new-folder", has && writable);
        enable("delete", has && writable && n > 0);
        enable("rename", has && writable && n == 1);
        enable("cut", has && writable && n > 0);
        enable("copy", has && writable && n > 0);
        enable("paste", has && writable && self.clipboard.borrow().is_some());
        enable("preview", has && !busy && one_file);
        enable("open-entry", has && !busy && one_file);
        enable("open-with", has && !busy && one_file);
        enable("find", has);
        enable("select-all", has);
    }

    fn update_info(&self) {
        let info = &self.info;
        let selected = self.selected_objects();
        let archive = self.archive.borrow();
        let buffer = info.comment.buffer();
        match (selected.as_slice(), archive.as_ref()) {
            ([one], _) => {
                let e = one.entry();
                info.icon.set_from_gicon(&super::icon_for(&e));
                info.name.set_label(e.name());
                let mut lines = vec![format!("{} {}", tr!("Type:"), super::type_description(&e))];
                if !e.is_dir {
                    lines.push(format!("{} {}", tr!("Size:"), super::format_size(e.size)));
                    if let Some(c) = e.compressed {
                        lines.push(format!("{} {}", tr!("Compressed:"), super::format_size(c)));
                    }
                }
                if let Some(t) = e.mtime {
                    lines.push(format!("{} {}", tr!("Modified:"), super::format_time(t)));
                }
                if let Some(o) = &e.owner {
                    lines.push(format!("{} {}{}", tr!("Owner:"), o, e.group.as_ref().map(|g| format!(" / {g}")).unwrap_or_default()));
                }
                if let Some(m) = e.mode {
                    lines.push(format!("{} {}", tr!("Permissions:"), super::permissions(m, e.is_dir)));
                }
                if e.encrypted {
                    lines.push(tr!("Password protected"));
                }
                if let Some(l) = &e.link {
                    lines.push(format!("{} {}", tr!("Link to:"), l));
                }
                info.details.set_label(&lines.join("\n"));
            }
            (many, Some(a)) => {
                info.icon.set_from_gicon(&gio::content_type_get_icon(&a.mime));
                info.name.set_label(&a.name());
                let mut lines = vec![
                    format!("{} {}", tr!("Type:"), tr!(a.kind.label())),
                    format!("{} {}", tr!("Size:"), super::format_size(std::fs::metadata(&a.path).map(|m| m.len()).unwrap_or(0))),
                    tr!("{files} files, {folders} folders", files = a.listing.files() as i64, folders = a.listing.folders() as i64),
                ];
                if !many.is_empty() {
                    lines.push(tr!("{count} selected", count = many.len() as i64));
                }
                if let Some(reason) = a.read_only_reason() {
                    lines.push(reason);
                }
                info.details.set_label(&lines.join("\n"));
            }
            _ => {
                info.icon.set_icon_name(Some("package-x-generic"));
                info.name.set_label(&self.title.label());
                info.details.set_label("");
            }
        }
        let comment = archive.as_ref().and_then(|a| a.listing.comment.clone());
        buffer.set_text(comment.as_deref().unwrap_or(""));
        info.comment_title.set_visible(comment.is_some());
        info.comment_scroll.set_visible(comment.is_some());
    }

    pub fn update_status(&self) {
        if self.busy.get() {
            return;
        }
        let archive = self.archive.borrow();
        let text = match archive.as_ref() {
            Some(a) => {
                let mut s = tr!("{files} files, {folders} folders, {size}", files = a.listing.files() as i64,
                                folders = a.listing.folders() as i64, size = super::format_size(a.listing.total_size()));
                let n = self.selected_objects().len();
                if n > 0 {
                    s.push_str(" · ");
                    s.push_str(&tr!("{count} selected", count = n as i64));
                }
                if let Some(reason) = a.read_only_reason() {
                    s.push_str(" · ");
                    s.push_str(&reason);
                }
                s
            }
            None if self.pending.borrow().is_some() => tr!("Add files to create the archive."),
            None => String::new(),
        };
        self.status.set_label(&text);
    }

    fn set_busy(&self, busy: bool, label: &str) {
        self.busy.set(busy);
        self.progress_box.set_visible(busy);
        self.progress.set_fraction(0.0);
        if busy {
            self.status.set_label(label);
        }
        self.update_actions();
        self.update_status();
    }

    fn job_ui(&self) -> Rc<dyn Fn() -> JobUi> {
        let weak = self.downgrade();
        Rc::new(move || {
            let w = weak.clone();
            JobUi {
                progress: Rc::new(move |fraction, _item| {
                    if let Some(w) = w.upgrade() {
                        if fraction < 0.0 {
                            w.progress.pulse();
                        } else {
                            w.progress.set_fraction(fraction.clamp(0.0, 1.0));
                        }
                    }
                }),
                conflict: conflict_handler(weak.upgrade().and_then(|w| w.parent())),
            }
        })
    }

    /// Run an operation on the open archive in the background (with password retry).
    pub fn run<T: Send + 'static>(&self, label: &str, work: Work<T>, done: impl FnOnce(&ArchiveWindow, crate::archive::Result<T>) + 'static) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true, label);
        let name = self.archive.borrow().as_ref().map(|a| a.name()).unwrap_or_default();
        let password = self.archive.borrow().as_ref().and_then(|a| a.password.clone());
        let weak = self.downgrade();
        with_password(self.parent(), name, password, self.job_ui(), self.job.clone(), work, Box::new(move |result, password| {
            let Some(w) = weak.upgrade() else { return };
            if let (Some(pw), Some(a)) = (password, w.archive.borrow_mut().as_mut()) {
                if result.is_ok() {
                    a.password = Some(pw);
                }
            }
            w.set_busy(false, "");
            done(&w, result);
        }));
    }

    pub fn show_error(&self, error: &Error) {
        if *error != Error::Cancelled {
            dialogs::error(self.parent().as_ref(), &error.to_string());
        }
    }

    // ------------------------------------------------------------- opening

    pub fn open(&self, path: &Path) {
        self.open_with(path, None, false);
    }

    /// Open with a known password (`header`: it also protects the file list).
    pub fn open_with(&self, path: &Path, known: Option<String>, header: bool) {
        let path = path.to_path_buf();
        if !path.is_file() {
            dialogs::error(self.parent().as_ref(), &tr!("The file {name} does not exist.", name = path.display().to_string()));
            return;
        }
        if self.busy.get() {
            return;
        }
        self.pending.borrow_mut().take();
        self.set_busy(true, &tr!("Opening {name}…", name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        let settings = self.app.settings();
        let p = path.clone();
        let work: Work<Archive> = Arc::new(move |ctx, _| ops::open(&p, &settings, ctx));
        let weak = self.downgrade();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let given = known.clone();
        with_password(self.parent(), name, known, self.job_ui(), self.job.clone(), work, Box::new(move |result, password| {
            let Some(w) = weak.upgrade() else { return };
            w.set_busy(false, "");
            match result {
                Ok(mut archive) => {
                    if password.is_some() {
                        // Asked for (not given): the list itself needed the password.
                        let asked = password != given;
                        archive.password = password;
                        if asked || header {
                            archive.listing.header_encrypted = true;
                            archive.listing.encrypted = true;
                        }
                    }
                    w.app.update_settings(|s| s.add_recent(&path.display().to_string()));
                    w.search.set_text("");
                    w.set_archive(Some(archive));
                }
                Err(Error::Cancelled) => {}
                Err(e) => {
                    let message = tr!("Could not open {name}: {error}", name = path.display().to_string(), error = e.to_string());
                    dialogs::error(w.parent().as_ref(), &message);
                }
            }
        }));
    }

    /// Re-read the archive after a change, keeping its password.
    pub fn reload(&self, select: Vec<String>) {
        let Some(old) = self.archive() else { return };
        let settings = self.app.settings();
        let path = old.path.clone();
        let work: Work<Archive> = Arc::new(move |ctx, _| ops::open(&path, &settings, ctx));
        self.run(&tr!("Reloading…"), work, move |w, result| match result {
            Ok(mut archive) => {
                archive.password = old.password.clone().or(archive.password);
                archive.listing.header_encrypted |= old.listing.header_encrypted;
                w.set_archive(Some(archive));
                if !select.is_empty() {
                    w.select_paths(&select);
                }
            }
            Err(e) => {
                w.show_error(&e);
                w.set_archive(None);
            }
        });
    }

    fn file_filter() -> gtk::FileFilter {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&tr!("Archives")));
        for mime in Kind::all_mimes() {
            filter.add_mime_type(mime);
        }
        filter
    }

    fn start_folder(&self) -> PathBuf {
        self.archive.borrow().as_ref().and_then(|a| a.path.parent().map(Path::to_path_buf)).unwrap_or_else(suite_core::paths::home)
    }

    fn choose_and_open(&self) {
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&Self::file_filter());
        let all = gtk::FileFilter::new();
        all.set_name(Some(&tr!("All files")));
        all.add_pattern("*");
        filters.append(&all);
        let chooser = gtk::FileDialog::builder().title(tr!("Open Archive")).modal(true).filters(&filters)
            .initial_folder(&gio::File::for_path(self.start_folder())).build();
        let weak = self.downgrade();
        chooser.open(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            if let (Some(w), Ok(Some(path))) = (weak.upgrade(), result.map(|f| f.path())) {
                if w.archive().is_some() {
                    let other = w.app.new_window();
                    other.window.present();
                    other.open(&path);
                } else {
                    w.open(&path);
                }
            }
        });
    }

    // -------------------------------------------------------------- create

    pub fn new_archive(&self, files: Vec<PathBuf>) {
        let settings = self.app.settings();
        let kinds = archive_settings::plugins::writable_kinds(&|id| settings.plugin_enabled(id), &|p| p.available());
        let kinds: Vec<Kind> = if files.len() == 1 && files[0].is_file() { kinds } else { kinds.into_iter().filter(|k| !k.is_single_file()).collect() };
        let folder = files.first().and_then(|f| f.parent().map(Path::to_path_buf))
            .or_else(|| glib::user_special_dir(glib::UserDirectory::Documents)).unwrap_or_else(suite_core::paths::home);
        let name = match files.as_slice() {
            [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            _ => tr!("New Archive"),
        };
        let weak = self.downgrade();
        forms::create_dialog(self.parent().as_ref(), &folder, &name, kinds, None, move |request| {
            let (Some(w), Some(request)) = (weak.upgrade(), request) else { return };
            let target = if w.archive().is_some() || w.pending.borrow().is_some() {
                let other = w.app.new_window();
                other.window.present();
                other
            } else {
                w
            };
            *target.pending.borrow_mut() = Some(Pending { path: request.path.clone(), kind: request.kind, options: request.options.clone() });
            target.set_archive(None);
            if !files.is_empty() {
                target.add_files(files, String::new());
            }
        });
    }

    // ----------------------------------------------------------------- add

    fn choose_and_add(&self, folders: bool) {
        let chooser = gtk::FileDialog::builder().title(if folders { tr!("Add Folder") } else { tr!("Add Files") }).modal(true)
            .initial_folder(&gio::File::for_path(self.start_folder())).build();
        let weak = self.downgrade();
        let folder = self.current_folder();
        let done = move |result: Result<gio::ListModel, glib::Error>| {
            let (Some(w), Ok(list)) = (weak.upgrade(), result) else { return };
            let files: Vec<PathBuf> = (0..list.n_items()).filter_map(|i| list.item(i).and_downcast::<gio::File>()).filter_map(|f| f.path()).collect();
            if !files.is_empty() {
                w.add_files(files, folder.clone());
            }
        };
        if folders {
            chooser.select_multiple_folders(Some(&self.window), None::<&gio::Cancellable>, done);
        } else {
            chooser.open_multiple(Some(&self.window), None::<&gio::Cancellable>, done);
        }
    }

    /// Add files below `folder`; creates a pending new archive.
    pub fn add_files(&self, files: Vec<PathBuf>, folder: String) {
        if let Some(pending) = self.pending.borrow().clone() {
            let settings = self.app.settings();
            let p = pending.clone();
            let work: Work<()> = Arc::new(move |ctx, _| {
                ops::create_items(&p.path, p.kind, &ops::items_for(&files, ""), &p.options, &settings, ctx)
            });
            self.run(&tr!("Creating {name}…", name = pending.path.display().to_string()), work, move |w, result| match result {
                Ok(()) => {
                    w.pending.borrow_mut().take();
                    w.open_with(&pending.path, pending.options.password.clone(), pending.options.encrypt_header);
                }
                Err(e) => w.show_error(&e),
            });
            return;
        }
        let Some(archive) = self.archive() else { return };
        let names: Vec<String> = ops::items_for(&files, &folder).into_iter().map(|i| i.dest).collect();
        let existing: Vec<String> = names.iter().filter(|n| archive.listing.find(n).is_some()).cloned().collect();
        let go = {
            let weak = self.downgrade();
            move || {
                let Some(w) = weak.upgrade() else { return };
                let a = archive.clone();
                let (files, folder) = (files.clone(), folder.clone());
                let work: Work<()> = Arc::new(move |ctx, _| ops::add(&a, &files, &folder, &CreateOptions::default(), ctx));
                let select = names.clone();
                w.run(&tr!("Adding files…"), work, move |w, result| match result {
                    Ok(()) => w.reload(select),
                    Err(e) => w.show_error(&e),
                });
            }
        };
        if existing.is_empty() {
            go();
        } else {
            dialogs::confirm(self.parent().as_ref(), &tr!("Replace Entries?"),
                             &tr!("The archive already contains {names}. Replace them with the added files?", names = existing.join(", ")),
                             &tr!("Replace"), true, move |yes| if yes { go() });
        }
    }

    // ------------------------------------------------------------- extract

    fn extract_dialog(&self) {
        let Some(archive) = self.archive() else { return };
        let selected = self.selected_paths();
        let settings = self.app.settings();
        let destination = archive.path.parent().map(Path::to_path_buf).unwrap_or_else(suite_core::paths::home);
        let weak = self.downgrade();
        forms::extract_dialog(self.parent().as_ref(), &archive, selected.len(), &destination, &settings, true, move |request| {
            let (Some(w), Some(request)) = (weak.upgrade(), request) else { return };
            w.app.update_settings(|s| {
                s.preserve_paths = request.preserve_paths;
                s.open_destination = request.open_destination;
                s.close_after_extraction = request.close_after;
            });
            let selection = request.selected_only.then(|| selected.clone());
            w.extract(selection, request.destination.clone(), ExtractOptions {
                preserve_paths: request.preserve_paths, strip: None, subfolder: request.subfolder.clone(),
            }, request.open_destination, request.close_after);
        });
    }

    fn extract_here(&self) {
        let Some(archive) = self.archive() else { return };
        let settings = self.app.settings();
        let destination = archive.path.parent().map(Path::to_path_buf).unwrap_or_else(suite_core::paths::home);
        let subfolder = (settings.extract_subfolder && ops::needs_subfolder(&archive.listing)).then(|| ops::subfolder_name(&archive.path));
        self.extract(None, destination, ExtractOptions { preserve_paths: true, strip: None, subfolder }, settings.open_destination, false);
    }

    pub fn extract(&self, selection: Option<Vec<String>>, destination: PathBuf, options: ExtractOptions, open_after: bool, close_after: bool) {
        let Some(archive) = self.archive() else { return };
        let label = tr!("Extracting {name}…", name = archive.name());
        let work: Work<ops::ExtractReport> = Arc::new(move |ctx, resolve| {
            ops::extract(&archive, selection.as_deref(), &destination, &options, ctx, resolve)
        });
        self.run(&label, work, move |w, result| match result {
            Ok(report) => {
                if open_after {
                    w.app.open_folder(&report.folder);
                }
                if close_after {
                    w.window.close();
                } else {
                    w.status.set_label(&tr!("Extracted to {folder}", folder = report.folder.display().to_string()));
                }
            }
            Err(e) => w.show_error(&e),
        });
    }

    /// The Ark D-Bus drop: extract the dragged entries into `destination`.
    pub fn extract_dropped(&self, entries: Vec<String>, strip: Option<String>, destination: Result<PathBuf, String>) {
        self.dnd_calls.set(self.dnd_calls.get() + 1);
        let destination = match destination {
            Ok(d) if d.is_dir() => d,
            Ok(d) => {
                dialogs::error(self.parent().as_ref(), &tr!("The folder {folder} does not exist.", folder = d.display().to_string()));
                return;
            }
            Err(text) => {
                dialogs::error(self.parent().as_ref(), &tr!("Files can only be extracted to a local folder, not {destination}.", destination = text));
                return;
            }
        };
        self.extract(Some(entries), destination, ExtractOptions { preserve_paths: true, strip, subfolder: None }, false, false);
    }

    // ---------------------------------------------------------------- edits

    fn delete_selected(&self) {
        let Some(archive) = self.archive() else { return };
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let message = if paths.len() == 1 {
            tr!("Delete {name} from the archive? This cannot be undone.", name = paths[0].clone())
        } else {
            tr!("Delete {count} entries from the archive? This cannot be undone.", count = paths.len() as i64)
        };
        let weak = self.downgrade();
        dialogs::confirm(self.parent().as_ref(), &tr!("Delete Entries"), &message, &tr!("Delete"), true, move |yes| {
            let (Some(w), true) = (weak.upgrade(), yes) else { return };
            let p = paths.clone();
            let work: Work<()> = Arc::new(move |ctx, _| ops::delete(&archive, &p, ctx));
            w.run(&tr!("Deleting…"), work, |w, result| match result {
                Ok(()) => w.reload(Vec::new()),
                Err(e) => w.show_error(&e),
            });
        });
    }

    fn rename_selected(&self) {
        let Some(archive) = self.archive() else { return };
        let objects = self.selected_objects();
        let [object] = objects.as_slice() else { return };
        let entry = object.entry();
        let weak = self.downgrade();
        let current = entry.name().to_string();
        dialogs::prompt(self.parent().as_ref(), &tr!("Rename"), &tr!("New name for {name}:", name = current.clone()), &current, &tr!("Rename"),
                        move |name| {
            let (Some(w), Some(name)) = (weak.upgrade(), name) else { return };
            if name == entry.name() {
                return;
            }
            let (path, n) = (entry.path.clone(), name.clone());
            let work: Work<()> = Arc::new(move |ctx, _| ops::rename(&archive, &path, &n, ctx));
            let parent = entry.parent().to_string();
            let select = if parent.is_empty() { name.clone() } else { format!("{parent}/{name}") };
            w.run(&tr!("Renaming…"), work, move |w, result| match result {
                Ok(()) => w.reload(vec![select]),
                Err(e) => w.show_error(&e),
            });
        });
    }

    fn cut_copy(&self, cut: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        *self.clipboard.borrow_mut() = Some((paths, cut));
        self.status.set_label(&if cut { tr!("{count} entries cut", count = n as i64) } else { tr!("{count} entries copied", count = n as i64) });
        self.update_actions();
    }

    fn paste(&self) {
        let Some(archive) = self.archive() else { return };
        let Some((paths, cut)) = self.clipboard.borrow().clone() else { return };
        let folder = self.current_folder();
        if cut && paths.iter().any(|p| folder == *p || folder.starts_with(&format!("{p}/"))) {
            dialogs::error(self.parent().as_ref(), &tr!("A folder cannot be moved into itself."));
            return;
        }
        let names: Vec<String> = paths.iter().map(|p| {
            let n = p.rsplit('/').next().unwrap_or(p);
            if folder.is_empty() { n.to_string() } else { format!("{folder}/{n}") }
        }).collect();
        let f = folder.clone();
        let work: Work<()> = Arc::new(move |ctx, _| if cut {
            ops::move_into_folder(&archive, &paths, &f, ctx)
        } else {
            ops::copy_into_folder(&archive, &paths, &f, ctx)
        });
        self.run(&if cut { tr!("Moving…") } else { tr!("Copying…") }, work, move |w, result| match result {
            Ok(()) => {
                if cut {
                    w.clipboard.borrow_mut().take();
                }
                w.reload(names);
            }
            Err(e) => w.show_error(&e),
        });
    }

    /// Move entries inside the archive (internal drag and drop).
    pub fn move_entries(&self, paths: Vec<String>, folder: String) {
        let Some(archive) = self.archive() else { return };
        if paths.iter().any(|p| folder == *p || folder.starts_with(&format!("{p}/"))) {
            return;
        }
        if paths.iter().all(|p| p.rsplit_once('/').map(|(d, _)| d).unwrap_or("") == folder) {
            return;
        }
        let f = folder.clone();
        let names: Vec<String> = paths.iter().map(|p| {
            let n = p.rsplit('/').next().unwrap_or(p);
            if folder.is_empty() { n.to_string() } else { format!("{folder}/{n}") }
        }).collect();
        let work: Work<()> = Arc::new(move |ctx, _| ops::move_into_folder(&archive, &paths, &f, ctx));
        self.run(&tr!("Moving…"), work, move |w, result| match result {
            Ok(()) => w.reload(names),
            Err(e) => w.show_error(&e),
        });
    }

    fn new_folder(&self) {
        let Some(archive) = self.archive() else { return };
        let parent = self.current_folder();
        let weak = self.downgrade();
        dialogs::prompt(self.parent().as_ref(), &tr!("New Folder"), &tr!("Name of the new folder:"), &tr!("New Folder"), &tr!("Create"), move |name| {
            let (Some(w), Some(name)) = (weak.upgrade(), name) else { return };
            let (p, n) = (parent.clone(), name.clone());
            let work: Work<()> = Arc::new(move |ctx, _| ops::create_folder(&archive, &p, &n, ctx));
            let select = if parent.is_empty() { name } else { format!("{parent}/{name}") };
            w.run(&tr!("Creating folder…"), work, move |w, result| match result {
                Ok(()) => w.reload(vec![select]),
                Err(e) => w.show_error(&e),
            });
        });
    }

    fn edit_comment(&self) {
        let Some(archive) = self.archive() else { return };
        let current = archive.listing.comment.clone().unwrap_or_default();
        let weak = self.downgrade();
        dialogs::comment(self.parent().as_ref(), &current, move |text| {
            let (Some(w), Some(text)) = (weak.upgrade(), text) else { return };
            let work: Work<()> = Arc::new(move |_, _| ops::set_comment(&archive, &text));
            w.run(&tr!("Saving the comment…"), work, |w, result| match result {
                Ok(()) => w.reload(Vec::new()),
                Err(e) => w.show_error(&e),
            });
        });
    }

    fn test(&self) {
        let Some(archive) = self.archive() else { return };
        let work: Work<()> = Arc::new(move |ctx, _| ops::test(&archive, ctx));
        self.run(&tr!("Testing…"), work, |w, result| match result {
            Ok(()) => {
                dialogs::message(w.parent().as_ref(), &tr!("Test Results"), &tr!("The archive passed the integrity test."));
            }
            Err(Error::Cancelled) => {}
            Err(e) => {
                dialogs::message(w.parent().as_ref(), &tr!("Test Results"), &tr!("The archive failed the integrity test: {error}", error = e.to_string()));
            }
        });
    }

    fn properties(&self) {
        if let Some(archive) = self.archive() {
            forms::properties_dialog(self.parent().as_ref(), &archive);
        }
    }

    fn show_settings(&self) {
        let (window, body, buttons) = dialogs::dialog(self.parent().as_ref(), &tr!("Settings"), 620);
        window.set_widget_name("settings-dialog");
        window.set_default_height(620);
        let page = archive_settings::page::build_page(Some(self.app.store.path.clone()));
        let scroll = gtk::ScrolledWindow::builder().child(&page.page).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        body.append(&scroll);
        let close = dialogs::button(&buttons, &tr!("Close"), "settings-close", Some("suggested-action"));
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
        let keep = RefCell::new(Some(page));
        window.connect_close_request(move |_| {
            if let Some(page) = keep.borrow_mut().take() {
                page.close();
            }
            glib::Propagation::Proceed
        });
        window.present();
    }

    // ---------------------------------------------- preview / open / open with

    fn single_selected(&self) -> Option<crate::archive::Entry> {
        let objects = self.selected_objects();
        match objects.as_slice() {
            [one] if !one.is_dir() => Some(one.entry()),
            _ => None,
        }
    }

    /// Extract one entry to a private folder, then `done(path)`.
    fn extract_temp(&self, entry: &crate::archive::Entry, done: impl FnOnce(&ArchiveWindow, PathBuf) + 'static) {
        let Some(archive) = self.archive() else { return };
        let scratch = match TempDir::private("open-") {
            Ok(t) => t,
            Err(e) => return self.show_error(&e),
        };
        let folder = scratch.path().to_path_buf();
        self.scratch.borrow_mut().push(scratch);
        let path = entry.path.clone();
        let name = entry.name().to_string();
        let f = folder.clone();
        let work: Work<ops::ExtractReport> = Arc::new(move |ctx, resolve| {
            ops::extract(&archive, Some(std::slice::from_ref(&path)), &f, &ExtractOptions { preserve_paths: false, ..Default::default() }, ctx, resolve)
        });
        self.run(&tr!("Extracting {name}…", name = name.clone()), work, move |w, result| match result {
            Ok(_) => done(w, folder.join(&name)),
            Err(e) => w.show_error(&e),
        });
    }

    pub fn preview_selected(&self) {
        let Some(entry) = self.single_selected() else { return };
        let settings = self.app.settings();
        if !settings.internal_preview {
            return self.open_selected(None);
        }
        if let Some(limit) = settings.preview_limit_bytes() {
            if entry.size > limit {
                dialogs::error(self.parent().as_ref(), &tr!("{name} is larger than the preview size limit ({size}).",
                                                              name = entry.name(), size = super::format_size(limit)));
                return;
            }
        }
        let e = entry.clone();
        self.extract_temp(&entry, move |w, path| {
            if super::preview::show(w.parent().as_ref(), &path, e.name()).is_none() {
                w.launch_and_watch(&e, &path, None);
            }
        });
    }

    pub fn open_selected(&self, app: Option<gio::AppInfo>) {
        let Some(entry) = self.single_selected() else { return };
        let e = entry.clone();
        self.extract_temp(&entry, move |w, path| w.launch_and_watch(&e, &path, app.as_ref()));
    }

    fn open_with_selected(&self) {
        let Some(entry) = self.single_selected() else { return };
        let content = super::content_type(&entry);
        #[allow(deprecated)]
        let chooser = gtk::AppChooserDialog::for_content_type(self.parent().as_ref(), gtk::DialogFlags::MODAL, &content);
        chooser.set_widget_name("open-with-dialog");
        let weak = self.downgrade();
        #[allow(deprecated)]
        chooser.connect_response(move |dialog, response| {
            let app = (response == gtk::ResponseType::Ok).then(|| dialog.app_info()).flatten();
            dialog.destroy();
            if let (Some(w), Some(app)) = (weak.upgrade(), app) {
                w.open_selected(Some(app));
            }
        });
        chooser.present();
    }

    /// Launch the extracted file; offer to put a saved change back into the archive.
    fn launch_and_watch(&self, entry: &crate::archive::Entry, path: &Path, app: Option<&gio::AppInfo>) {
        let file = gio::File::for_path(path);
        if let Err(e) = self.app.launch(&file, app) {
            dialogs::error(self.parent().as_ref(), &tr!("Could not open {name}: {error}", name = entry.name(), error = e));
            return;
        }
        if self.read_only() {
            return;
        }
        let weak = self.downgrade();
        let (entry_path, file_path) = (entry.path.clone(), path.to_path_buf());
        let asking = Rc::new(Cell::new(false));
        if let Some(monitor) = super::preview::watch(path, move || {
            let Some(w) = weak.upgrade() else { return };
            if asking.get() || !file_path.is_file() {
                return;
            }
            asking.set(true);
            let (ep, fp, asking) = (entry_path.clone(), file_path.clone(), asking.clone());
            let name = ep.rsplit('/').next().unwrap_or(&ep).to_string();
            let weak = w.downgrade();
            dialogs::confirm(w.parent().as_ref(), &tr!("File Modified"),
                             &tr!("The file {name} was modified. Do you want to update the archive?", name = name), &tr!("Update"), false, move |yes| {
                asking.set(false);
                let (Some(w), true) = (weak.upgrade(), yes) else { return };
                let Some(archive) = w.archive() else { return };
                let (ep2, fp2) = (ep.clone(), fp.clone());
                let work: Work<()> = Arc::new(move |ctx, _| ops::add_items(&archive, &[AddItem { source: fp2.clone(), dest: ep2.clone() }], &CreateOptions::default(), ctx));
                w.run(&tr!("Updating the archive…"), work, move |w, result| match result {
                    Ok(()) => w.reload(vec![ep]),
                    Err(e) => w.show_error(&e),
                });
            });
        }) {
            self.monitors.borrow_mut().push(monitor);
        }
    }

    // ---------------------------------------------------------- context menu

    fn row_at(&self, x: f64, y: f64) -> Option<EntryObject> {
        let widget = self.view.pick(x, y, gtk::PickFlags::DEFAULT)?;
        widget_entry(&widget)
    }

    fn select_under(&self, x: f64, y: f64) -> Option<EntryObject> {
        let object = self.row_at(x, y)?;
        let selected = self.selected_objects();
        if !selected.iter().any(|o| o.path() == object.path()) {
            self.select_paths(&[object.path()]);
        }
        Some(object)
    }

    fn context_menu(&self, x: f64, y: f64) {
        if self.archive.borrow().is_none() {
            return;
        }
        if self.select_under(x, y).is_none() {
            self.selection.unselect_all();
        }
        let menu = gio::Menu::new();
        let open = gio::Menu::new();
        open.append(Some(&tr!("Preview")), Some("win.preview"));
        open.append(Some(&tr!("Open")), Some("win.open-entry"));
        open.append(Some(&tr!("Open With…")), Some("win.open-with"));
        menu.append_section(None, &open);
        let extract = gio::Menu::new();
        extract.append(Some(&tr!("Extract…")), Some("win.extract"));
        menu.append_section(None, &extract);
        let edit = gio::Menu::new();
        edit.append(Some(&tr!("Cut")), Some("win.cut"));
        edit.append(Some(&tr!("Copy")), Some("win.copy"));
        edit.append(Some(&tr!("Paste")), Some("win.paste"));
        edit.append(Some(&tr!("Rename…")), Some("win.rename"));
        edit.append(Some(&tr!("Delete")), Some("win.delete"));
        menu.append_section(None, &edit);
        let add = gio::Menu::new();
        add.append(Some(&tr!("New Folder…")), Some("win.new-folder"));
        add.append(Some(&tr!("Add Files…")), Some("win.add-files"));
        menu.append_section(None, &add);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_widget_name("entry-menu");
        popover.set_parent(&self.view);
        popover.set_has_arrow(false);
        popover.set_halign(if suite_core::i18n::is_rtl() { gtk::Align::End } else { gtk::Align::Start });
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    // ------------------------------------------------------------ drag & drop

    fn install_dnd(&self) {
        // COPY only: other applications must never delete anything; a drop on this window moves.
        let source = gtk::DragSource::builder().actions(gdk::DragAction::COPY).build();
        let weak = self.downgrade();
        source.connect_prepare(move |_, x, y| {
            let w = weak.upgrade()?;
            w.drag_content(x, y).map(|c| c.upcast())
        });
        let weak = self.downgrade();
        source.connect_drag_begin(move |source, _| {
            let Some(w) = weak.upgrade() else { return };
            let n = w.selected_paths().len();
            let icon = if n == 1 { w.selected_objects().first().map(|o| super::icon_for(&o.entry())) } else { None };
            let theme = gtk::IconTheme::for_display(&WidgetExt::display(&w.window));
            let gicon = icon.unwrap_or_else(|| gio::ThemedIcon::new("edit-copy").upcast());
            let paintable = theme.lookup_by_gicon(&gicon, 32, 1, gtk::TextDirection::None, gtk::IconLookupFlags::empty());
            source.set_icon(Some(&paintable), 8, 8);
        });
        let weak = self.downgrade();
        source.connect_drag_end(move |_, _, _| {
            // Keep the object briefly: an Ark-aware target calls it right after the drop.
            let weak = weak.clone();
            glib::timeout_add_seconds_local_once(60, move || {
                if let Some(w) = weak.upgrade() {
                    let mut exports = w.exports.borrow_mut();
                    if exports.len() > 1 {
                        let old = exports.remove(0);
                        old.unregister();
                    }
                }
            });
        });
        self.view.add_controller(source);

        let formats = gdk::ContentFormats::new(&["text/uri-list", dnd::SERVICE]).union(&gdk::ContentFormats::for_type(gdk::FileList::static_type()));
        let target = gtk::DropTargetAsync::new(Some(formats), gdk::DragAction::COPY);
        let weak = self.downgrade();
        let action_for = move |drop: &gdk::Drop| -> gdk::DragAction {
            let Some(w) = weak.upgrade() else { return gdk::DragAction::empty() };
            if w.busy.get() {
                return gdk::DragAction::empty();
            }
            match w.local_drag(drop) {
                Some(true) if !w.read_only() => gdk::DragAction::COPY,
                Some(_) => gdk::DragAction::empty(),
                None if w.archive.borrow().is_some() && w.read_only() => gdk::DragAction::empty(),
                None => gdk::DragAction::COPY,
            }
        };
        let a = Rc::new(action_for);
        let a1 = a.clone();
        target.connect_accept(move |_, drop| !a1(drop).is_empty());
        let (a2, weak) = (a.clone(), self.downgrade());
        target.connect_drag_enter(move |_, drop, _, _| {
            if let Some(w) = weak.upgrade() {
                w.stack.add_css_class("drop-highlight");
            }
            a2(drop)
        });
        let a3 = a.clone();
        target.connect_drag_motion(move |_, drop, _, _| a3(drop));
        let weak = self.downgrade();
        target.connect_drag_leave(move |_, _| {
            if let Some(w) = weak.upgrade() {
                w.stack.remove_css_class("drop-highlight");
            }
        });
        let weak = self.downgrade();
        target.connect_drop(move |target, drop, x, y| {
            let Some(w) = weak.upgrade() else { return false };
            w.stack.remove_css_class("drop-highlight");
            if a(drop).is_empty() {
                return false;
            }
            let point = target.widget().and_then(|t| t.compute_point(&w.view, &gtk::graphene::Point::new(x as f32, y as f32)));
            let folder = point.and_then(|p| w.row_at(p.x() as f64, p.y() as f64)).map(|o| if o.is_dir() { o.path() } else { o.entry().parent().to_string() })
                .unwrap_or_default();
            if w.local_drag(drop) == Some(true) {
                let paths = w.selected_paths();
                drop.finish(gdk::DragAction::COPY);
                w.move_entries(paths, folder);
                return true;
            }
            let d = drop.clone();
            let weak = w.downgrade();
            glib::spawn_future_local(async move {
                let value = d.read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT).await;
                let files: Vec<PathBuf> = value.ok().and_then(|v| v.get::<gdk::FileList>().ok()).map(|l| l.files())
                    .unwrap_or_default().iter().filter_map(|f| f.path()).collect();
                d.finish(if files.is_empty() { gdk::DragAction::empty() } else { gdk::DragAction::COPY });
                if let (Some(w), false) = (weak.upgrade(), files.is_empty()) {
                    w.dropped_files(files, folder);
                }
            });
            true
        });
        self.window.add_controller(target);
    }

    /// Some(true): a drag from this window; Some(false): from another window of ours.
    fn local_drag(&self, drop: &gdk::Drop) -> Option<bool> {
        let drag = drop.drag()?;
        let content = drag.content();
        let ours = self.drag.borrow().as_ref().is_some_and(|c| c.upcast_ref::<gdk::ContentProvider>() == &content);
        Some(ours)
    }

    /// Files dropped from outside: open an archive, start a new one, or add them.
    pub fn dropped_files(&self, files: Vec<PathBuf>, folder: String) {
        if self.archive.borrow().is_none() && self.pending.borrow().is_none() {
            if let [one] = files.as_slice() {
                if one.is_file() && ops::detect(one).is_ok() {
                    return self.open(one);
                }
            }
            return self.new_archive(files);
        }
        self.add_files(files, folder);
    }

    /// The drag content for the entries under (x, y) / the selection.
    pub fn drag_content(&self, x: f64, y: f64) -> Option<ArkContent> {
        let archive = self.archive()?;
        if self.busy.get() {
            return None;
        }
        self.select_under(x, y)?;
        let paths = self.selected_paths();
        if paths.is_empty() {
            return None;
        }
        Some(self.drag_content_for(archive, paths))
    }

    pub fn drag_content_for(&self, archive: Archive, paths: Vec<String>) -> ArkContent {
        // Ark extracts dragged entries without their parent folders.
        let parents: Vec<&str> = paths.iter().map(|p| p.rsplit_once('/').map(|(d, _)| d).unwrap_or("")).collect();
        let strip = parents.first().filter(|first| !first.is_empty() && parents.iter().all(|p| p == *first)).map(|s| s.to_string());
        let connection = self.app.app.dbus_connection().or_else(|| gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok());
        let weak = self.downgrade();
        let (entries, s) = (paths.clone(), strip.clone());
        let exported = connection.and_then(|c| dnd::export(&c, Rc::new(move |destination| {
            if let Some(w) = weak.upgrade() {
                w.extract_dropped(entries.clone(), s.clone(), destination);
            }
        })).ok());
        let (service, object) = exported.as_ref().map(|e| (e.service.clone(), e.path.clone())).unwrap_or_default();
        if let Some(e) = exported {
            self.exports.borrow_mut().push(e);
        }
        let weak = self.downgrade();
        let stager: dnd::Stager = Rc::new(move |done| {
            let Some(w) = weak.upgrade() else { return done(Err("closed".into())) };
            let scratch = match TempDir::private("drag-") {
                Ok(t) => t,
                Err(e) => return done(Err(e.to_string())),
            };
            let folder = scratch.path().to_path_buf();
            w.scratch.borrow_mut().push(scratch);
            let (a, p, s) = (archive.clone(), paths.clone(), strip.clone());
            let f = folder.clone();
            let work: Work<ops::ExtractReport> = Arc::new(move |ctx, resolve| {
                ops::extract(&a, Some(&p), &f, &ExtractOptions { preserve_paths: true, strip: s.clone(), subfolder: None }, ctx, resolve)
            });
            let done = RefCell::new(Some(done));
            w.run(&tr!("Extracting {name}…", name = archive.name()), work, move |_, result| {
                if let Some(done) = done.borrow_mut().take() {
                    done(result.map(|r| r.top).map_err(|e| e.to_string()));
                }
            });
        });
        let content = ArkContent::new(&service, &object, Some(stager));
        *self.drag.borrow_mut() = Some(content.clone());
        content
    }
}

fn shortcuts() -> Vec<suite_gtk::app_menu::ShortcutSection> {
    use suite_gtk::app_menu::Shortcut;
    vec![
        (archive_settings::n_!("Archive").to_string(), vec![
            Shortcut::new("<Control>n", archive_settings::n_!("New archive")), Shortcut::new("<Control>o", archive_settings::n_!("Open an archive")),
            Shortcut::new("<Control>e", archive_settings::n_!("Extract")), Shortcut::new("<Control>t", archive_settings::n_!("Test integrity")),
            Shortcut::new("<Alt>Return", archive_settings::n_!("Properties")), Shortcut::new("<Control>w", archive_settings::n_!("Close the window")),
        ]),
        (archive_settings::n_!("Entries").to_string(), vec![
            Shortcut::new("<Control><Shift>a", archive_settings::n_!("Add files")), Shortcut::new("<Control><Shift>n", archive_settings::n_!("New folder")),
            Shortcut::new("Delete", archive_settings::n_!("Delete")), Shortcut::new("F2", archive_settings::n_!("Rename")),
            Shortcut::new("<Control>x", archive_settings::n_!("Cut")), Shortcut::new("<Control>c", archive_settings::n_!("Copy")), Shortcut::new("<Control>v", archive_settings::n_!("Paste")),
            Shortcut::new("space", archive_settings::n_!("Preview")), Shortcut::new("<Control>f", archive_settings::n_!("Filter entries")),
            Shortcut::new("F9", archive_settings::n_!("Information panel")),
        ]),
    ]
}
