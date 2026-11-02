//! files_columns.py: Miller columns — parent folders, the current folder, and
//! the selected folder side by side. The interactive column is the pane's own
//! folder model (a ListView on its flat selection).

use crate::pane::{BrowserPane, WeakPane};
use tessel_files_model::model::{DirectoryLoader, FileItem};
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const COLUMN_WIDTH: i32 = 260;
pub const MAX_PARENTS: usize = 6;

/// Parent folders shown left of `location`, nearest last; stops at home or the root.
pub fn column_chain(location: &gio::File, home: Option<&gio::File>, limit: usize) -> Vec<gio::File> {
    let home = home.cloned().unwrap_or_else(crate::pane::home_file);
    if location.equal(&home) {
        return Vec::new();
    }
    let stop_at_home = location.has_prefix(&home);
    let mut chain = Vec::new();
    let mut folder = location.parent();
    while let Some(current) = folder {
        if chain.len() >= limit {
            break;
        }
        chain.insert(0, current.clone());
        if stop_at_home && current.equal(&home) {
            break;
        }
        folder = current.parent();
    }
    chain
}

pub struct ColumnInner {
    pub root: gtk::ScrolledWindow,
    pub folder: gio::File,
    pub uri: String,
    pub highlight: RefCell<Option<String>>,
    pub syncing: Cell<bool>,
    pub loaded: Cell<bool>,
    pub store: gio::ListStore,
    pub filter: gtk::CustomFilter,
    pub sorted: gtk::SortListModel,
    pub selection: gtk::SingleSelection,
    pub view: gtk::ListView,
    pub loader: DirectoryLoader,
}

/// A read-only listing of one folder that highlights one child.
#[derive(Clone)]
pub struct FolderColumn(pub Rc<ColumnInner>);

impl std::ops::Deref for FolderColumn {
    type Target = ColumnInner;
    fn deref(&self) -> &ColumnInner {
        &self.0
    }
}

impl FolderColumn {
    pub fn new(folder: &gio::File, pane: &BrowserPane, on_pick: Rc<dyn Fn(&FolderColumn, &FileItem)>) -> Self {
        let root = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).width_request(COLUMN_WIDTH).vexpand(true).build();
        root.add_css_class("files-column");
        let store = gio::ListStore::new::<FileItem>();
        let weak_pane = pane.downgrade();
        let filter = gtk::CustomFilter::new(move |object| {
            let (Some(pane), Some(item)) = (weak_pane.upgrade(), object.downcast_ref::<FileItem>()) else { return false };
            pane.option_bool("show_hidden") || !item.is_hidden()
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), Some(pane.sorter.clone()));
        let selection = gtk::SingleSelection::new(Some(sorted.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, object| {
            let Some(row) = object.downcast_ref::<gtk::ListItem>() else { return };
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let icon = gtk::Image::builder().pixel_size(20).build();
            let label = gtk::Label::builder().xalign(0.).hexpand(true).ellipsize(pango::EllipsizeMode::End).build();
            let chevron = gtk::Image::builder().icon_name("go-next-symbolic").pixel_size(12).build();
            line.append(&icon);
            line.append(&label);
            line.append(&chevron);
            row.set_child(Some(&line));
        });
        factory.connect_bind(|_, object| {
            let Some(row) = object.downcast_ref::<gtk::ListItem>() else { return };
            let (Some(line), Some(item)) = (row.child(), row.item().and_downcast::<FileItem>()) else { return };
            let icon = line.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let chevron = label.next_sibling().unwrap();
            icon.set_from_gicon(&item.icon());
            label.set_text(&item.display_name());
            chevron.set_visible(item.is_dir());
        });
        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.add_css_class("files-column-list");
        root.set_child(Some(&view));
        let column = FolderColumn(Rc::new(ColumnInner {
            root, folder: folder.clone(), uri: folder.uri().to_string(), highlight: RefCell::new(None), syncing: Cell::new(false),
            loaded: Cell::new(false), store, filter, sorted, selection, view, loader: DirectoryLoader::default(),
        }));
        let weak = Rc::downgrade(&column.0);
        column.selection.connect_selection_changed(move |selection, _, _| {
            let Some(inner) = weak.upgrade() else { return };
            let column = FolderColumn(inner);
            if column.syncing.get() {
                return;
            }
            if let Some(item) = selection.selected_item().and_downcast::<FileItem>() {
                on_pick(&column, &item);
            }
        });
        let (store, weak) = (column.store.clone(), Rc::downgrade(&column.0));
        column.loader.start(folder, move |batch: Vec<FileItem>| store.splice(store.n_items(), 0, &batch), move |_error| {
            if let Some(inner) = weak.upgrade() {
                let column = FolderColumn(inner);
                column.loaded.set(true);
                let highlight = column.highlight.borrow().clone();
                column.set_highlight(highlight.as_deref());
            }
        });
        column
    }

    pub fn set_highlight(&self, uri: Option<&str>) {
        *self.highlight.borrow_mut() = uri.map(str::to_string);
        if !self.loaded.get() {
            return;
        }
        self.syncing.set(true);
        let found = (0..self.selection.n_items())
            .find(|&i| self.selection.item(i).and_downcast::<FileItem>().is_some_and(|item| Some(item.uri().as_str()) == uri));
        match found {
            Some(index) => {
                self.selection.set_selected(index);
                self.view.scroll_to(index, gtk::ListScrollFlags::NONE, None);
            }
            None => self.selection.set_selected(gtk::INVALID_LIST_POSITION),
        }
        self.syncing.set(false);
    }

    pub fn refilter(&self) {
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn close(&self) {
        self.loader.cancel();
        self.view.set_model(None::<&gtk::SelectionModel>);
        self.view.set_factory(None::<&gtk::ListItemFactory>);
        self.filter.set_filter_func(|_| false);
        self.store.remove_all();
    }

    /// Item names in display order (tests).
    pub fn names(&self) -> Vec<String> {
        (0..self.selection.n_items()).filter_map(|i| self.selection.item(i).and_downcast::<FileItem>()).map(|i| i.name()).collect()
    }
}

pub struct MillerInner {
    pub root: gtk::ScrolledWindow,
    pane: WeakPane,
    pub row: gtk::Box,
    pub current: gtk::ListView,
    pub current_scroll: gtk::ScrolledWindow,
    pub parents: RefCell<Vec<FolderColumn>>,
    pub trail: RefCell<Option<FolderColumn>>,
    scroll_source: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct MillerColumns(pub Rc<MillerInner>);

impl std::ops::Deref for MillerColumns {
    type Target = MillerInner;
    fn deref(&self) -> &MillerInner {
        &self.0
    }
}

impl MillerColumns {
    pub fn new(pane: &BrowserPane, factory: &gtk::SignalListItemFactory) -> Self {
        let root = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Automatic).vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true).vexpand(true).build();
        root.add_css_class("files-columns");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_child(Some(&row));
        let current = gtk::ListView::builder().factory(factory).enable_rubberband(true).build();
        let current_scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).width_request(COLUMN_WIDTH + 40)
            .vexpand(true).build();
        current_scroll.add_css_class("files-column");
        current_scroll.add_css_class("files-column-current");
        current_scroll.set_child(Some(&current));
        row.append(&current_scroll);
        MillerColumns(Rc::new(MillerInner {
            root, pane: pane.downgrade(), row, current, current_scroll, parents: RefCell::new(Vec::new()), trail: RefCell::new(None),
            scroll_source: RefCell::new(None),
        }))
    }

    fn picker(&self) -> Rc<dyn Fn(&FolderColumn, &FileItem)> {
        let pane = self.pane.clone();
        // Navigating replaces the column emitting this signal; leave its handler first.
        Rc::new(move |column: &FolderColumn, item: &FileItem| {
            let (folder, uri, pane) = (column.folder.clone(), item.uri(), pane.clone());
            glib::idle_add_local_once(move || {
                if let Some(pane) = pane.upgrade() {
                    if !pane.closed() {
                        pane.navigate(&folder, true, Some(vec![uri]));
                    }
                }
            });
        })
    }

    /// Rebuild the parent columns for the pane's folder, reusing loaded ones.
    pub fn sync(&self) {
        let Some(pane) = self.pane.upgrade() else { return };
        if pane.closed() {
            return;
        }
        let current = pane.current();
        let chain = column_chain(&current, None, MAX_PARENTS);
        let mut existing: Vec<FolderColumn> = self.parents.borrow().clone();
        let mut columns = Vec::new();
        for (index, folder) in chain.iter().enumerate() {
            let uri = folder.uri().to_string();
            let column = match existing.iter().position(|c| c.uri == uri) {
                Some(position) => {
                    let column = existing.remove(position);
                    column.refilter();
                    column
                }
                None => FolderColumn::new(folder, &pane, self.picker()),
            };
            let following = chain.get(index + 1).cloned().unwrap_or_else(|| current.clone());
            column.set_highlight(Some(following.uri().as_str()));
            columns.push(column);
        }
        for column in existing {
            if column.root.parent().as_ref() == Some(self.row.upcast_ref()) {
                self.row.remove(&column.root);
            }
            column.close();
        }
        for column in self.parents.borrow().iter() {
            if column.root.parent().as_ref() == Some(self.row.upcast_ref()) {
                self.row.remove(&column.root);
            }
        }
        for column in columns.iter().rev() {
            self.row.prepend(&column.root);
        }
        *self.parents.borrow_mut() = columns;
        self.sync_trail();
    }

    /// Show the single selected folder's contents right of the current column.
    pub fn sync_trail(&self) {
        let Some(pane) = self.pane.upgrade() else { return };
        if pane.closed() {
            return;
        }
        let selected = pane.selected();
        let folder = if selected.len() == 1 && selected[0].is_dir() { Some(selected[0].file()) } else { None };
        if let (Some(trail), Some(folder)) = (self.trail.borrow().as_ref(), folder.as_ref()) {
            if trail.folder.equal(folder) {
                return;
            }
        }
        if let Some(trail) = self.trail.borrow_mut().take() {
            self.row.remove(&trail.root);
            trail.close();
        }
        if let Some(folder) = folder {
            let trail = FolderColumn::new(&folder, &pane, self.picker());
            self.row.append(&trail.root);
            *self.trail.borrow_mut() = Some(trail);
        }
        self.reveal_end();
    }

    pub fn refilter(&self) {
        for column in self.parents.borrow().iter().chain(self.trail.borrow().iter()) {
            column.refilter();
        }
    }

    pub fn reveal_end(&self) {
        if self.scroll_source.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        // Newly appended columns need one allocation before the range grows.
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(30), move || {
            if let Some(inner) = weak.upgrade() {
                *inner.scroll_source.borrow_mut() = None;
                let adjustment = inner.root.hadjustment();
                adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.));
            }
        });
        *self.scroll_source.borrow_mut() = Some(source);
    }

    /// Drop parent and trail listings while another view is shown.
    pub fn clear(&self) {
        let parents = std::mem::take(&mut *self.parents.borrow_mut());
        let trail = self.trail.borrow_mut().take();
        for column in parents.into_iter().chain(trail) {
            if column.root.parent().is_some() {
                self.row.remove(&column.root);
            }
            column.close();
        }
    }

    pub fn close(&self) {
        if let Some(source) = self.scroll_source.borrow_mut().take() {
            source.remove();
        }
        self.clear();
        self.current.set_model(None::<&gtk::SelectionModel>);
        self.current.set_factory(None::<&gtk::ListItemFactory>);
    }

    pub fn parents(&self) -> Vec<FolderColumn> {
        self.parents.borrow().clone()
    }
    pub fn trail(&self) -> Option<FolderColumn> {
        self.trail.borrow().clone()
    }
}
