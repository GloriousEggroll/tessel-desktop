//! Compact port of `~/btmanager/file-manager/media_chooser.py` (the Browse/Add
//! dialog): `.files-window`, Back/Forward/Up/Refresh, location entry, Places
//! sidebar, a folder + media list filtered by type, single or multiple
//! selection, worker-side validation, Escape/close/parent-unrealize cancel.
//! The Files view modes (icons/compact/details), zoom, thumbnails toggle,
//! bookmarks and devices belong to the file-manager port and are not here.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use suite_core::tr;

use super::pool::Pool;
use super::Validator;
use crate::fileops::mailbox::Mailbox;

pub struct Config {
    pub title: String,
    pub initial_folder: PathBuf,
    /// `(label, extensions without dot)`; the first is active.
    pub filters: Vec<(String, Vec<&'static str>)>,
    pub validate: Option<Validator>,
    pub multiple: bool,
    pub accept_label: Option<String>,
}

enum Message {
    Listed(u64, Result<Vec<(String, bool)>, String>),
    Checked(Result<(), String>),
}

struct Inner {
    window: gtk::Window,
    callback: RefCell<Option<Box<dyn FnOnce(Option<Vec<gio::File>>)>>>,
    closed: Cell<bool>,
    busy: Cell<bool>,
    loading: Cell<bool>,
    multiple: bool,
    validate: Option<Validator>,
    filters: Vec<(String, Vec<&'static str>)>,
    extensions: RefCell<Vec<&'static str>>,
    current: RefCell<PathBuf>,
    history: RefCell<Vec<PathBuf>>,
    history_index: Cell<usize>,
    generation: Cell<u64>,
    kinds: RefCell<HashMap<String, bool>>,
    model: gio::ListStore,
    filter: gtk::CustomFilter,
    selection: gtk::SelectionModel,
    list: gtk::ListView,
    status: gtk::Label,
    content: gtk::Box,
    location: gtk::Entry,
    back_button: gtk::Button,
    forward_button: gtk::Button,
    up_button: gtk::Button,
    type_filter: gtk::DropDown,
    accept_button: gtk::Button,
    pool: Pool,
    mailbox: RefCell<Option<Mailbox<Message>>>,
    pending_files: RefCell<Vec<gio::File>>,
    parent: gtk::Window,
    parent_handler: RefCell<Option<glib::SignalHandlerId>>,
}

/// Returns local `gio::File`s (one in single mode) or `None` on cancel.
#[derive(Clone)]
pub struct MediaChooser(Rc<Inner>);

fn suffix(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => name[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

impl MediaChooser {
    pub fn new(parent: &gtk::Window, callback: impl FnOnce(Option<Vec<gio::File>>) + 'static, config: Config) -> Self {
        let window = gtk::Window::builder().title(&config.title).transient_for(parent).modal(true).destroy_with_parent(true).build();
        window.set_default_size(980, 660);
        window.set_titlebar(Some(&suite_gtk::utils::headerbar(&config.title)));
        window.add_css_class("files-window");
        suite_gtk::theme::attach(&window, None);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.set_child(Some(&root));
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        toolbar.add_css_class("files-toolbar");
        let tool = |icon: &str, tip: String| gtk::Button::builder().icon_name(icon).tooltip_text(tip).build();
        let back_button = tool("go-previous-symbolic", tr!("Back"));
        let forward_button = tool("go-next-symbolic", tr!("Forward"));
        let up_button = tool("go-up-symbolic", tr!("Parent folder"));
        let refresh = tool("view-refresh-symbolic", tr!("Refresh"));
        let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        navigation.add_css_class("files-navigation-buttons");
        for b in [&back_button, &forward_button, &up_button, &refresh] {
            navigation.append(b);
        }
        toolbar.append(&navigation);
        let location = gtk::Entry::builder().hexpand(true).build();
        toolbar.append(&location);
        root.append(&toolbar);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        status.add_css_class("files-error");
        root.append(&status);
        let content = gtk::Box::builder().hexpand(true).vexpand(true).build();
        root.append(&content);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("files-sidebar");
        suite_gtk::utils::sidebar_style(&sidebar);
        let sidebar_scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).min_content_width(200).hexpand(false).build();
        sidebar_scroll.set_child(Some(&sidebar));
        content.append(&sidebar_scroll);

        let model = gio::ListStore::new::<gtk::StringObject>();
        let filter = gtk::CustomFilter::new(|_| true);
        let filtered = gtk::FilterListModel::new(Some(model.clone()), Some(filter.clone()));
        let selection: gtk::SelectionModel = if config.multiple {
            gtk::MultiSelection::new(Some(filtered)).upcast()
        } else {
            let single = gtk::SingleSelection::new(Some(filtered));
            single.set_autoselect(false);
            single.set_can_unselect(true);
            single.upcast()
        };
        let factory = gtk::SignalListItemFactory::new();
        let list = gtk::ListView::builder().model(&selection).factory(&factory).vexpand(true).hexpand(true).build();
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).build();
        scroll.set_child(Some(&list));
        content.append(&scroll);

        let footer = gtk::Box::builder().spacing(10).margin_top(10).margin_bottom(12).margin_start(12).margin_end(12).build();
        let names: Vec<&str> = config.filters.iter().map(|(n, _)| n.as_str()).collect();
        let type_filter = gtk::DropDown::from_strings(&names);
        type_filter.set_tooltip_text(Some(&tr!("File types")));
        footer.append(&type_filter);
        footer.append(&gtk::Box::builder().hexpand(true).build());
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        let accept_button = gtk::Button::builder().label(config.accept_label.clone().unwrap_or_else(|| tr!("Select"))).sensitive(false).build();
        accept_button.add_css_class("suggested-action");
        footer.append(&cancel);
        footer.append(&accept_button);
        root.append(&footer);

        let extensions = config.filters.first().map(|(_, e)| e.clone()).unwrap_or_default();
        let chooser = MediaChooser(Rc::new(Inner {
            window: window.clone(),
            callback: RefCell::new(Some(Box::new(callback))),
            closed: Cell::new(false),
            busy: Cell::new(false),
            loading: Cell::new(false),
            multiple: config.multiple,
            validate: config.validate.clone(),
            filters: config.filters,
            extensions: RefCell::new(extensions),
            current: RefCell::new(PathBuf::new()),
            history: RefCell::new(Vec::new()),
            history_index: Cell::new(0),
            generation: Cell::new(0),
            kinds: RefCell::new(HashMap::new()),
            model,
            filter: filter.clone(),
            selection: selection.clone(),
            list: list.clone(),
            status,
            content,
            location: location.clone(),
            back_button: back_button.clone(),
            forward_button: forward_button.clone(),
            up_button: up_button.clone(),
            type_filter: type_filter.clone(),
            accept_button: accept_button.clone(),
            pool: Pool::new(1, "media-validation"),
            mailbox: RefCell::new(None),
            pending_files: RefCell::new(Vec::new()),
            parent: parent.clone(),
            parent_handler: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&chooser.0);
        let on = move |f: fn(&MediaChooser)| {
            let weak = weak.clone();
            move || {
                if let Some(inner) = weak.upgrade() {
                    f(&MediaChooser(inner));
                }
            }
        };
        let w = Rc::downgrade(&chooser.0);
        *chooser.0.mailbox.borrow_mut() = Some(Mailbox::new(move |m| match w.upgrade() {
            Some(inner) => {
                MediaChooser(inner).deliver(m);
                true
            }
            None => false,
        }));
        let w = Rc::downgrade(&chooser.0);
        filter.set_filter_func(move |object| {
            let Some(inner) = w.upgrade() else { return true };
            let Some(name) = object.downcast_ref::<gtk::StringObject>().map(|s| s.string().to_string()) else { return false };
            let is_dir = inner.kinds.borrow().get(&name).copied().unwrap_or(false);
            if is_dir {
                return true;
            }
            let matched = inner.extensions.borrow().contains(&suffix(&name).as_str());
            matched
        });
        // Places.
        let heading = gtk::Label::builder().label(tr!("Places")).xalign(0.0).build();
        heading.add_css_class("section-heading");
        sidebar.append(&heading);
        let mut places: Vec<(String, &str, PathBuf)> = vec![(tr!("Home"), "user-home-symbolic", suite_core::paths::home())];
        for (title, dir, icon) in [
            ("Desktop", glib::UserDirectory::Desktop, "user-desktop-symbolic"),
            ("Downloads", glib::UserDirectory::Downloads, "folder-download-symbolic"),
            ("Pictures", glib::UserDirectory::Pictures, "folder-pictures-symbolic"),
            ("Videos", glib::UserDirectory::Videos, "folder-videos-symbolic"),
        ] {
            if let Some(path) = glib::user_special_dir(dir) {
                places.push((tr!(title), icon, path));
            }
        }
        if let Some(folder) = ["/usr/share/wallpapers", "/usr/share/applications/wallpapers"].iter().find(|f| Path::new(f).is_dir()) {
            places.push((tr!("Wallpapers"), "preferences-desktop-wallpaper-symbolic", PathBuf::from(folder)));
        }
        places.push((tr!("File System"), "drive-harddisk-symbolic", PathBuf::from("/")));
        for (title, icon, path) in places {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
            row.append(&suite_gtk::utils::image(icon, 18));
            row.append(&gtk::Label::builder().label(&title).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(20).build());
            let button = gtk::Button::builder().child(&row).tooltip_text(path.to_string_lossy()).build();
            button.add_css_class("profile-row");
            let w = Rc::downgrade(&chooser.0);
            button.connect_clicked(move |_| {
                if let Some(inner) = w.upgrade() {
                    MediaChooser(inner).navigate(&path);
                }
            });
            sidebar.append(&button);
        }
        let f = on(|c| c.back());
        back_button.connect_clicked(move |_| f());
        let f = on(|c| c.forward());
        forward_button.connect_clicked(move |_| f());
        let f = on(|c| c.up());
        up_button.connect_clicked(move |_| f());
        let f = on(|c| c.refresh());
        refresh.connect_clicked(move |_| f());
        let w = Rc::downgrade(&chooser.0);
        location.connect_activate(move |entry| {
            if let Some(inner) = w.upgrade() {
                MediaChooser(inner).navigate(Path::new(entry.text().as_str()));
            }
        });
        let f = on(|c| c.close());
        cancel.connect_clicked(move |_| f());
        let f = on(|c| c.accept());
        accept_button.connect_clicked(move |_| f());
        let f = on(|c| c.selection_changed());
        selection.connect_selection_changed(move |_, _, _| f());
        let f = on(|c| c.filter_changed());
        type_filter.connect_selected_notify(move |_| f());
        let w = Rc::downgrade(&chooser.0);
        list.connect_activate(move |_, position| {
            if let Some(inner) = w.upgrade() {
                let c = MediaChooser(inner);
                if let Some(name) = c.visible_names().get(position as usize).cloned() {
                    c.activate_name(&name);
                }
            }
        });
        let w = Rc::downgrade(&chooser.0);
        factory.connect_setup(|_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else { return };
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&gtk::Image::builder().pixel_size(24).build());
            row.append(&gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::Middle).build());
            item.set_child(Some(&row));
        });
        factory.connect_bind(move |_, object| {
            let (Some(inner), Some(item)) = (w.upgrade(), object.downcast_ref::<gtk::ListItem>()) else { return };
            let Some(name) = item.item().and_downcast::<gtk::StringObject>().map(|s| s.string().to_string()) else { return };
            let Some(row) = item.child().and_downcast::<gtk::Box>() else { return };
            let is_dir = inner.kinds.borrow().get(&name).copied().unwrap_or(false);
            let icon = if is_dir {
                "folder-symbolic"
            } else if super::VIDEO_SUFFIXES.contains(&suffix(&name).as_str()) {
                "video-x-generic-symbolic"
            } else {
                "image-x-generic-symbolic"
            };
            if let Some(image) = row.first_child().and_downcast::<gtk::Image>() {
                image.set_icon_name(Some(icon));
            }
            if let Some(label) = row.last_child().and_downcast::<gtk::Label>() {
                label.set_text(&name);
            }
        });
        let f = on(|c| c.close());
        *chooser.0.parent_handler.borrow_mut() = Some(parent.connect_unrealize(move |_| f()));
        let f = on(|c| c.finish(None));
        window.connect_close_request(move |_| {
            f();
            glib::Propagation::Stop
        });
        let f = on(|c| c.finish(None));
        window.connect_unrealize(move |_| f());
        let w = Rc::downgrade(&chooser.0);
        suite_gtk::utils::key_controller(&window, false, move |_, key, _, state| match w.upgrade() {
            Some(inner) => MediaChooser(inner).key_pressed(key, state),
            None => glib::Propagation::Proceed,
        });
        chooser.navigate(&config.initial_folder);
        chooser
    }

    pub fn window(&self) -> &gtk::Window {
        &self.0.window
    }
    pub fn present(&self) {
        self.0.window.present();
    }
    pub fn is_closed(&self) -> bool {
        self.0.closed.get()
    }
    pub fn is_busy(&self) -> bool {
        self.0.busy.get()
    }
    pub fn is_loading(&self) -> bool {
        self.0.loading.get()
    }
    pub fn is_multiple(&self) -> bool {
        self.0.multiple
    }
    pub fn status(&self) -> &gtk::Label {
        &self.0.status
    }
    pub fn accept_button(&self) -> &gtk::Button {
        &self.0.accept_button
    }
    pub fn back_button(&self) -> &gtk::Button {
        &self.0.back_button
    }
    pub fn type_filter(&self) -> &gtk::DropDown {
        &self.0.type_filter
    }
    pub fn selection(&self) -> &gtk::SelectionModel {
        &self.0.selection
    }
    pub fn list(&self) -> &gtk::ListView {
        &self.0.list
    }
    pub fn current(&self) -> PathBuf {
        self.0.current.borrow().clone()
    }

    /// Names shown after filtering, in order (folders first).
    pub fn visible_names(&self) -> Vec<String> {
        let model = &self.0.selection;
        (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gtk::StringObject>()).map(|s| s.string().to_string()).collect()
    }

    /// Select (adding to the selection in multiple mode) the entry `name`.
    pub fn select_name(&self, name: &str, exclusive: bool) -> bool {
        match self.visible_names().iter().position(|n| n == name) {
            Some(index) => self.0.selection.select_item(index as u32, exclusive),
            None => false,
        }
    }

    pub fn selected(&self) -> Vec<String> {
        let bits = self.0.selection.selection();
        let names = self.visible_names();
        (0..names.len()).filter(|i| bits.contains(*i as u32)).map(|i| names[i].clone()).collect()
    }

    fn deliver(&self, message: Message) {
        if self.0.closed.get() {
            return;
        }
        match message {
            Message::Listed(generation, result) => {
                if generation != self.0.generation.get() {
                    return;
                }
                self.0.loading.set(false);
                match result {
                    Ok(entries) => {
                        *self.0.kinds.borrow_mut() = entries.iter().cloned().collect();
                        let objects: Vec<gtk::StringObject> = entries.iter().map(|(n, _)| gtk::StringObject::new(n)).collect();
                        self.0.model.splice(0, self.0.model.n_items(), &objects);
                        self.0.filter.changed(gtk::FilterChange::Different);
                    }
                    Err(error) => self.message(&error),
                }
                self.selection_changed();
            }
            Message::Checked(result) => {
                self.0.busy.set(false);
                self.0.content.set_sensitive(true);
                match result {
                    Ok(()) => {
                        let files = std::mem::take(&mut *self.0.pending_files.borrow_mut());
                        self.finish(Some(files));
                    }
                    Err(error) => {
                        self.message(&error);
                        self.selection_changed();
                    }
                }
            }
        }
    }

    pub fn message(&self, text: &str) {
        self.0.status.set_text(text);
        self.0.status.set_visible(!text.is_empty());
    }

    pub fn navigate(&self, location: &Path) {
        if self.0.closed.get() {
            return;
        }
        let folder = super::import::abspath(location);
        {
            let mut history = self.0.history.borrow_mut();
            let index = self.0.history_index.get();
            if history.get(index) != Some(&folder) {
                if !history.is_empty() {
                    history.truncate(index + 1);
                }
                history.push(folder.clone());
                self.0.history_index.set(history.len() - 1);
            }
        }
        self.load(folder);
    }

    fn load(&self, folder: PathBuf) {
        *self.0.current.borrow_mut() = folder.clone();
        self.0.location.set_text(&folder.to_string_lossy());
        let index = self.0.history_index.get();
        self.0.back_button.set_sensitive(index > 0);
        self.0.forward_button.set_sensitive(index + 1 < self.0.history.borrow().len());
        self.0.up_button.set_sensitive(folder.parent().is_some());
        self.0.loading.set(true);
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        let Some(mailbox) = self.0.mailbox.borrow().clone() else { return };
        // Listing is cheap (no decoding); a plain thread keeps validation's queue free.
        std::thread::spawn(move || {
            let result = std::fs::read_dir(&folder)
                .map(|entries| {
                    let mut rows: Vec<(String, bool)> = entries
                        .filter_map(Result::ok)
                        .filter_map(|e| {
                            let name = e.file_name().to_string_lossy().into_owned();
                            (!name.starts_with('.')).then(|| (name, e.path().is_dir()))
                        })
                        .collect();
                    rows.sort_by_key(|(name, is_dir)| (!*is_dir, suite_core::pyjson::casefold(name)));
                    rows
                })
                .map_err(|e| suite_core::pyerr::os_error_path(&e, &folder));
            mailbox.send(Message::Listed(generation, result));
        });
    }

    pub fn back(&self) {
        let index = self.0.history_index.get();
        if index > 0 {
            self.0.history_index.set(index - 1);
            let folder = self.0.history.borrow()[index - 1].clone();
            self.load(folder);
        }
    }

    pub fn forward(&self) {
        let index = self.0.history_index.get();
        let folder = self.0.history.borrow().get(index + 1).cloned();
        if let Some(folder) = folder {
            self.0.history_index.set(index + 1);
            self.load(folder);
        }
    }

    pub fn up(&self) {
        if let Some(parent) = self.current().parent() {
            self.navigate(&parent.to_path_buf());
        }
    }

    pub fn refresh(&self) {
        self.load(self.current());
    }

    fn selection_changed(&self) {
        self.0.accept_button.set_sensitive(!self.selected().is_empty() && !self.0.busy.get());
    }

    fn filter_changed(&self) {
        let index = self.0.type_filter.selected() as usize;
        if let Some((_, extensions)) = self.0.filters.get(index) {
            *self.0.extensions.borrow_mut() = extensions.clone();
        }
        self.0.filter.changed(gtk::FilterChange::Different);
        self.selection_changed();
    }

    fn key_pressed(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let control = state.contains(gdk::ModifierType::CONTROL_MASK);
        let alt = state.contains(gdk::ModifierType::ALT_MASK);
        if key == gdk::Key::Escape {
            self.close();
        } else if control && key == gdk::Key::l {
            self.0.location.grab_focus();
        } else if alt && key == gdk::Key::Left {
            self.back();
        } else if alt && key == gdk::Key::Right {
            self.forward();
        } else if alt && key == gdk::Key::Up {
            self.up();
        } else if key == gdk::Key::F5 {
            self.refresh();
        } else {
            return glib::Propagation::Proceed;
        }
        glib::Propagation::Stop
    }

    fn is_dir_name(&self, name: &str) -> bool {
        self.0.kinds.borrow().get(name).copied().unwrap_or(false)
    }

    fn activate_name(&self, name: &str) {
        let path = self.current().join(name);
        if self.is_dir_name(name) {
            self.navigate(&path);
        } else {
            self.open_files(vec![gio::File::for_path(path)]);
        }
    }

    pub fn accept(&self) {
        let selected = self.selected();
        if selected.is_empty() || self.0.busy.get() {
            return;
        }
        let files: Vec<gio::File> =
            selected.iter().filter(|n| !self.is_dir_name(n)).map(|n| gio::File::for_path(self.current().join(n))).collect();
        if self.0.multiple && !files.is_empty() {
            self.open_files(files);
        } else {
            self.activate_name(&selected[0]);
        }
    }

    /// Validate on a worker, then return the files.
    pub fn open_files(&self, files: Vec<gio::File>) {
        if self.0.closed.get() || self.0.busy.get() || files.is_empty() {
            return;
        }
        let files: Vec<gio::File> = if self.0.multiple { files } else { files.into_iter().take(1).collect() };
        let paths: Vec<Option<PathBuf>> = files.iter().map(|f| f.path()).collect();
        let extensions = self.0.extensions.borrow().clone();
        let supported = paths.iter().all(|p| p.as_ref().is_some_and(|p| extensions.contains(&super::media::suffix_of(p).as_str())));
        if !supported {
            self.message(&tr!("Choose a supported image or video."));
            return;
        }
        let paths: Vec<PathBuf> = paths.into_iter().flatten().collect();
        self.0.busy.set(true);
        self.0.accept_button.set_sensitive(false);
        self.0.content.set_sensitive(false);
        self.message(&tr!("Checking media…"));
        *self.0.pending_files.borrow_mut() = files;
        let validate = self.0.validate.clone();
        let Some(mailbox) = self.0.mailbox.borrow().clone() else { return };
        // Translations are resolved on the main thread.
        let missing_template = tr!("This file is no longer available: {name}", name = "\u{0}");
        self.0.pool.submit(None, move || {
            let check = || -> Result<(), String> {
                for path in &paths {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if !path.is_file() {
                        return Err(missing_template.replace('\u{0}', &name));
                    }
                    if let Some(validate) = &validate {
                        validate(path).map_err(|error| format!("{name}: {error}"))?;
                    }
                }
                Ok(())
            };
            mailbox.send(Message::Checked(check()));
        });
    }

    pub fn close(&self) {
        self.finish(None);
    }

    fn finish(&self, value: Option<Vec<gio::File>>) {
        let inner = &self.0;
        if inner.closed.replace(true) {
            return;
        }
        if let Some(id) = inner.parent_handler.borrow_mut().take() {
            inner.parent.disconnect(id);
        }
        inner.pool.shutdown();
        let mailbox = inner.mailbox.borrow_mut().take();
        if let Some(mailbox) = mailbox {
            mailbox.close();
        }
        let callback = inner.callback.borrow_mut().take();
        inner.window.destroy();
        if let Some(callback) = callback {
            callback(value);
        }
    }
}
