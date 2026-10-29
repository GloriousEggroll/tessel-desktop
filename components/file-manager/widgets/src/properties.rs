//! `file_dialogs.PropertiesWindow`: nonblocking file properties, permissions,
//! ACLs, tags/notes, default-application association and checksums.

use crate::dialogs::{detail_row, escape_closes, size_text};
use files_ops::extras::MetadataStore;
use files_ops::tools;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use suite_gtk::utils::{accessible, headerbar, image, set_margins};

struct SendBox<T>(T);
// SAFETY: values (GFileInfo, store handles) are created on a worker and handed to the
// main thread once; the worker never touches them again.
unsafe impl<T> Send for SendBox<T> {}

/// A worker error rendered like Python's `str(exc)`.
fn error_text(error: &dyn std::fmt::Display) -> String {
    error.to_string()
}

type Annotations = Result<Option<files_ops::extras::Metadata>, String>;

struct Loaded {
    infos: Vec<gio::FileInfo>,
    metadata: Vec<tools::FileMetadata>,
    annotations: Annotations,
}

pub struct PropertiesInner {
    pub window: gtk::Window,
    pub stack: gtk::Stack,
    pub general: gtk::Box,
    pub annotations: gtk::Box,
    pub permissions: gtk::Box,
    pub open_with: gtk::Box,
    pub hash_page: gtk::Box,
    pub status: gtk::Label,
    pub files: Vec<gio::File>,
    pub paths: Vec<PathBuf>,
    on_changed: Option<Rc<dyn Fn()>>,
    closed: Cell<bool>,
    cancel: Arc<AtomicBool>,
    gio_cancel: gio::Cancellable,
    metadata: RefCell<Vec<tools::FileMetadata>>,
    metadata_store: Arc<Mutex<Option<MetadataStore>>>,
    apps: RefCell<Vec<gio::AppInfo>>,
    mime: RefCell<Option<String>>,
    busy: Cell<bool>,
    updating_mode: Cell<bool>,
    // Built after loading (Python attributes that `hasattr` checks).
    size_label: RefCell<Option<gtk::Label>>,
    mode: RefCell<Option<gtk::Entry>>,
    permission_checks: RefCell<Vec<(u32, gtk::CheckButton)>>,
    owner: RefCell<Option<gtk::Entry>>,
    group: RefCell<Option<gtk::Entry>>,
    acl: RefCell<Option<gtk::Box>>,
    acl_text: RefCell<Option<gtk::TextView>>,
    tags_entry: RefCell<Option<gtk::Entry>>,
    rating_choice: RefCell<Option<gtk::DropDown>>,
    comment_text: RefCell<Option<gtk::TextView>>,
    app_choice: RefCell<Option<gtk::DropDown>>,
    hash_button: RefCell<Option<gtk::Button>>,
    hash_result: RefCell<Option<gtk::Label>>,
}

/// The Properties window (non-modal, transient for the Files window).
#[derive(Clone)]
pub struct PropertiesWindow(Rc<PropertiesInner>);

impl std::ops::Deref for PropertiesWindow {
    type Target = PropertiesInner;
    fn deref(&self) -> &PropertiesInner {
        &self.0
    }
}

fn page(stack: &gtk::Stack, name: &str, title: &str) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    scroll.set_child(Some(&content));
    stack.add_titled(&scroll, Some(name), title);
    content
}

fn wrapped(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).wrap(true).build()
}

fn plain(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).build()
}

fn buffer_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string()
}

/// `owner or -1`: an empty field leaves the id unchanged.
fn principal(text: &str) -> tools::Principal {
    if text.is_empty() { tools::Principal::Id(-1) } else { tools::Principal::from(text) }
}

fn octal(text: &str) -> Option<u32> {
    (!text.is_empty() && text.chars().all(|c| ('0'..='7').contains(&c))).then(|| u32::from_str_radix(text, 8).ok()).flatten()
}

impl PropertiesWindow {
    /// `PropertiesWindow(parent, files, on_changed=None, metadata_store=None)`.
    pub fn new(parent: Option<&gtk::Window>, files: &[gio::File], on_changed: Option<Rc<dyn Fn()>>,
               metadata_store: Option<MetadataStore>) -> Self {
        let window = gtk::Window::builder().title("Properties").modal(false).build();
        window.set_transient_for(parent);
        if let Some(application) = parent.and_then(|p| p.application()) {
            window.set_application(Some(&application));
        }
        window.add_css_class("files-window");
        window.set_titlebar(Some(&headerbar("Properties")));
    crate::compat::ensure_search_css(&gtk::prelude::WidgetExt::display(&window));
        window.set_default_size(590, 560);
        window.set_size_request(420, 320);
        suite_gtk::theme::attach(&window, None);
        escape_closes(&window);
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 12);
        set_margins(&outer, 18);
        window.set_child(Some(&outer));
        let stack = gtk::Stack::builder().vexpand(true).build();
        let switcher = gtk::StackSwitcher::builder().stack(&stack).halign(gtk::Align::Center).build();
        outer.append(&switcher);
        outer.append(&stack);
        let general = page(&stack, "general", "General");
        let annotations = page(&stack, "metadata", "Tags & Notes");
        let permissions = page(&stack, "permissions", "Permissions");
        let open_with = page(&stack, "open-with", "Open With");
        let hash_page = page(&stack, "checksums", "Checksums");
        let status = wrapped("Loading file information…");
        outer.append(&status);
        let button = gtk::Button::builder().label("Close").halign(gtk::Align::End).build();
        outer.append(&button);
        let files = files.to_vec();
        let paths = files.iter().filter_map(|f| f.path()).collect();
        let this = Self(Rc::new(PropertiesInner {
            window, stack, general, annotations, permissions, open_with, hash_page, status, files, paths, on_changed,
            closed: Cell::new(false), cancel: Arc::new(AtomicBool::new(false)), gio_cancel: gio::Cancellable::new(),
            metadata: RefCell::default(), metadata_store: Arc::new(Mutex::new(metadata_store)), apps: RefCell::default(),
            mime: RefCell::new(None), busy: Cell::new(false), updating_mode: Cell::new(false),
            size_label: RefCell::default(), mode: RefCell::default(), permission_checks: RefCell::default(),
            owner: RefCell::default(), group: RefCell::default(), acl: RefCell::default(), acl_text: RefCell::default(),
            tags_entry: RefCell::default(), rating_choice: RefCell::default(), comment_text: RefCell::default(),
            app_choice: RefCell::default(), hash_button: RefCell::default(), hash_result: RefCell::default(),
        }));
        let weak = this.window.downgrade();
        button.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.close();
            }
        });
        // The window owns the handle until it closes (Python keeps the object alive).
        let owner = RefCell::new(Some(this.clone()));
        this.window.connect_close_request(move |_| {
            let this = owner.borrow_mut().take();
            if let Some(this) = this {
                this.closing();
            }
            glib::Propagation::Proceed
        });
        let (files, paths_count, cancel, store) = (this.files.clone(), this.paths.len(), this.gio_cancel.clone(), this.metadata_store.clone());
        let _ = paths_count;
        this.work(move || {
            let mut infos = Vec::new();
            let mut metadata = Vec::new();
            for file in &files {
                infos.push(file.query_info("standard::*,time::*,access::*", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, Some(&cancel))
                    .map_err(|e| suite_core::pyerr::python_error_text(&e))?);
                if let Some(path) = file.path() {
                    metadata.push(tools::file_metadata(&path).map_err(|e| error_text(&e))?);
                }
            }
            let annotations = if files.len() == 1 {
                let mut store = store.lock().unwrap();
                let store = store.get_or_insert_with(|| MetadataStore::new(None));
                Ok(Some(store.read(&files[0])))
            } else {
                Ok(None)
            };
            Ok(SendBox(Loaded { infos, metadata, annotations }))
        }, |this, result: Result<SendBox<Loaded>, String>| this.loaded(result.map(|b| b.0)));
        this
    }

    pub fn window(&self) -> &gtk::Window {
        &self.0.window
    }

    pub fn present(&self) {
        self.0.window.present();
    }

    pub fn close(&self) {
        self.0.window.close();
    }

    /// `window._busy`.
    pub fn busy(&self) -> bool {
        self.0.busy.get()
    }

    /// `window._cancel.is_set()`.
    pub fn cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::SeqCst)
    }

    /// `bool(window._metadata)`.
    pub fn metadata_loaded(&self) -> bool {
        !self.0.metadata.borrow().is_empty()
    }

    pub fn size_label(&self) -> Option<gtk::Label> { self.0.size_label.borrow().clone() }
    pub fn mode(&self) -> Option<gtk::Entry> { self.0.mode.borrow().clone() }
    pub fn permission_checks(&self) -> Vec<(u32, gtk::CheckButton)> { self.0.permission_checks.borrow().clone() }
    pub fn owner(&self) -> Option<gtk::Entry> { self.0.owner.borrow().clone() }
    pub fn group(&self) -> Option<gtk::Entry> { self.0.group.borrow().clone() }
    pub fn acl(&self) -> Option<gtk::Box> { self.0.acl.borrow().clone() }
    pub fn acl_text(&self) -> Option<gtk::TextView> { self.0.acl_text.borrow().clone() }
    pub fn tags_entry(&self) -> Option<gtk::Entry> { self.0.tags_entry.borrow().clone() }
    pub fn rating_choice(&self) -> Option<gtk::DropDown> { self.0.rating_choice.borrow().clone() }
    pub fn comment_text(&self) -> Option<gtk::TextView> { self.0.comment_text.borrow().clone() }
    pub fn app_choice(&self) -> Option<gtk::DropDown> { self.0.app_choice.borrow().clone() }
    pub fn hash_button(&self) -> Option<gtk::Button> { self.0.hash_button.borrow().clone() }
    pub fn hash_result(&self) -> Option<gtk::Label> { self.0.hash_result.borrow().clone() }

    /// `_work(function, callback)`: run on a worker; deliver on the main loop unless closed.
    fn work<T: Send + 'static>(&self, function: impl FnOnce() -> Result<T, String> + Send + 'static,
                               callback: impl FnOnce(&PropertiesWindow, Result<T, String>) + 'static) {
        if self.0.closed.get() {
            return;
        }
        let handle = gio::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(function))
                .unwrap_or_else(|_| Err("The operation failed".to_string()))
        });
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = handle.await.unwrap_or_else(|_| Err("The operation failed".to_string()));
            if let Some(inner) = weak.upgrade() {
                if !inner.closed.get() {
                    callback(&PropertiesWindow(inner), result);
                }
            }
        });
    }

    fn loaded(&self, result: Result<Loaded, String>) {
        let loaded = match result {
            Ok(loaded) => loaded,
            Err(error) => {
                self.status.set_text(&error);
                return;
            }
        };
        let Loaded { infos, metadata, annotations } = loaded;
        self.0.metadata.replace(metadata);
        self.status.set_text("");
        let count = self.files.len();
        if count == 0 {
            self.status.set_text("No files selected");
            return;
        }
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let icon = if count == 1 {
            infos[0].icon().map(|i| gtk::Image::from_gicon(&i)).unwrap_or_default()
        } else {
            image("folder-symbolic", 40)
        };
        icon.set_pixel_size(40);
        heading.append(&icon);
        let name = gtk::Label::builder()
            .label(if count == 1 { infos[0].display_name().to_string() } else { format!("{count} items") })
            .xalign(0.0).wrap(true).hexpand(true).selectable(true).build();
        name.add_css_class("title-2");
        heading.append(&name);
        self.general.append(&heading);
        if count == 1 {
            let (file, info) = (&self.files[0], &infos[0]);
            detail_row(&self.general, "Location", &file.parse_name());
            let mime = info.content_type().map(|s| s.to_string()).filter(|s| !s.is_empty())
                .unwrap_or_else(|| "application/octet-stream".into());
            detail_row(&self.general, "Type", &gio::content_type_get_description(&mime));
            self.0.mime.replace(Some(mime));
            if let Some(modified) = info.modification_date_time() {
                if let Ok(text) = modified.format("%x %X") {
                    detail_row(&self.general, "Modified", &text);
                }
            }
            if let Some(target) = self.0.metadata.borrow().first().and_then(|m| m.link_target.clone()).map(|t| t.display().to_string()) {
                detail_row(&self.general, "Link target", &target);
            }
        }
        let size = detail_row(&self.general, "Size", &size_text(infos.iter().map(|i| i.size().max(0) as u64).sum()));
        self.0.size_label.replace(Some(size.clone()));
        if self.paths.len() == count && infos.iter().any(|i| i.file_type() == gio::FileType::Directory) {
            size.set_text("Calculating…");
            let (paths, cancel) = (self.paths.clone(), self.0.cancel.clone());
            self.work(move || tools::recursive_size(&paths, Some(&*cancel)).map_err(|e| error_text(&e)),
                      |this, result| this.size_loaded(result));
        }
        self.build_permissions();
        self.build_annotations(annotations);
        self.build_associations(&infos);
        self.build_hashes(&infos);
    }

    fn size_loaded(&self, result: Result<tools::SizeSummary, String>) {
        let Some(label) = self.size_label() else { return };
        match result {
            Err(error) => label.set_text(&error),
            Ok(summary) => {
                let mut text = format!("{}\n{} files, {} folders", size_text(summary.bytes),
                                       crate::compat::thousands(summary.files as i128), crate::compat::thousands(summary.directories as i128));
                if summary.unreadable > 0 {
                    text += &format!("\n{} items could not be read", summary.unreadable);
                }
                label.set_text(&text);
            }
        }
    }

    fn build_permissions(&self) {
        if self.paths.len() != self.files.len() {
            self.permissions.append(&wrapped("Permission editing is available for local files and mounted folders."));
            return;
        }
        let metadata = self.0.metadata.borrow().clone();
        if metadata.iter().any(|m| m.is_symlink) {
            self.permissions.append(&wrapped("Symbolic-link permissions are managed by the system. Open the target to change its permissions."));
            return;
        }
        let mut modes: Vec<u32> = metadata.iter().map(|m| m.mode).collect();
        modes.sort();
        modes.dedup();
        let mut owners: Vec<String> = metadata.iter().map(|m| m.owner.clone()).collect();
        owners.sort();
        owners.dedup();
        let mut groups: Vec<String> = metadata.iter().map(|m| m.group.clone()).collect();
        groups.sort();
        groups.dedup();
        let single = (modes.len() == 1).then(|| modes[0]);
        let mode = gtk::Entry::builder().text(single.map(|m| format!("{m:04o}")).unwrap_or_default())
            .placeholder_text("Mixed — enter an octal mode, e.g. 0755").build();
        accessible(&mode, "Unix permissions in octal");
        self.0.mode.replace(Some(mode.clone()));
        let grid = gtk::Grid::builder().column_spacing(18).row_spacing(10).build();
        for (column, title) in ["", "Read", "Write", "Execute"].iter().enumerate() {
            grid.attach(&plain(title), column as i32, 0, 1, 1);
        }
        let mut checks = Vec::new();
        for (row, (title, shift)) in [("Owner", 6u32), ("Group", 3), ("Others", 0)].into_iter().enumerate() {
            let row = row as i32 + 1;
            grid.attach(&plain(title), 0, row, 1, 1);
            for (column, (label, bit)) in [("read", 4u32), ("write", 2), ("execute", 1)].into_iter().enumerate() {
                let check = gtk::CheckButton::builder().halign(gtk::Align::Center).build();
                accessible(&check, &format!("{title} {label} permission"));
                let flag = bit << shift;
                check.set_active(single.is_some_and(|m| m & flag != 0));
                check.set_inconsistent(single.is_none());
                let weak = Rc::downgrade(&self.0);
                check.connect_toggled(move |_| {
                    if let Some(inner) = weak.upgrade() {
                        PropertiesWindow(inner).permission_toggled();
                    }
                });
                checks.push((flag, check.clone()));
                grid.attach(&check, column as i32 + 1, row, 1, 1);
            }
        }
        self.0.permission_checks.replace(checks);
        self.permissions.append(&grid);
        self.permissions.append(&plain("Permissions (octal)"));
        self.permissions.append(&mode);
        let weak = Rc::downgrade(&self.0);
        mode.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                PropertiesWindow(inner).mode_text_changed();
            }
        });
        self.permissions.append(&wrapped("Example: 0644 gives the owner read/write access and others read access; 0755 also allows execution or entering a folder."));
        let mode_button = gtk::Button::builder().label("Apply permissions").halign(gtk::Align::Start).build();
        let weak = Rc::downgrade(&self.0);
        mode_button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                PropertiesWindow(inner).apply_mode();
            }
        });
        self.permissions.append(&mode_button);
        let owner_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let owner = gtk::Entry::builder().text(if owners.len() == 1 { owners[0].as_str() } else { "" }).placeholder_text("Owner").hexpand(true).build();
        let group = gtk::Entry::builder().text(if groups.len() == 1 { groups[0].as_str() } else { "" }).placeholder_text("Group").hexpand(true).build();
        accessible(&owner, "File owner");
        accessible(&group, "File group");
        owner_row.append(&owner);
        owner_row.append(&group);
        self.0.owner.replace(Some(owner));
        self.0.group.replace(Some(group));
        self.permissions.append(&plain("Owner and group"));
        self.permissions.append(&owner_row);
        let owner_button = gtk::Button::builder().label("Apply ownership").halign(gtk::Align::Start).build();
        let weak = Rc::downgrade(&self.0);
        owner_button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                PropertiesWindow(inner).apply_owner();
            }
        });
        self.permissions.append(&owner_button);
        let note = wrapped("Ownership changes may require administrator permissions. Empty fields are left unchanged.");
        note.add_css_class("dim-label");
        self.permissions.append(&note);
        if self.paths.len() == 1 {
            let acl = page(&self.stack, "acl", "Access Control");
            self.0.acl.replace(Some(acl.clone()));
            if !tools::acl_available() {
                acl.append(&wrapped("Install the acl package to view and edit access control lists."));
            } else {
                acl.append(&plain("Extended permissions (POSIX ACL)"));
                let text = gtk::TextView::builder().monospace(true).vexpand(true).build();
                accessible(&text, "Access control list");
                let scroll = gtk::ScrolledWindow::builder().min_content_height(160).vexpand(true).build();
                scroll.set_child(Some(&text));
                acl.append(&scroll);
                self.0.acl_text.replace(Some(text));
                acl.append(&wrapped("Use one entry per line, such as user::rw-, user:alice:r--, group::r--, mask::r--, other::---."));
                let apply = gtk::Button::builder().label("Apply access control list").halign(gtk::Align::Start).build();
                let weak = Rc::downgrade(&self.0);
                apply.connect_clicked(move |_| {
                    if let Some(inner) = weak.upgrade() {
                        PropertiesWindow(inner).apply_acl();
                    }
                });
                acl.append(&apply);
                let path = self.paths[0].clone();
                self.work(move || tools::read_acl(&path).map_err(|e| error_text(&e)), |this, result| match result {
                    Err(error) => this.status.set_text(&error),
                    Ok(text) => {
                        if let Some(view) = this.acl_text() {
                            view.buffer().set_text(&text);
                        }
                    }
                });
            }
        }
    }

    fn build_annotations(&self, values: Annotations) {
        if self.files.len() != 1 {
            self.annotations.append(&wrapped("Choose one item to edit its tags, rating and comment."));
            return;
        }
        if self.paths.len() != 1 {
            self.annotations.append(&wrapped("Tags, ratings and comments are available for local files and mounted folders."));
            return;
        }
        let values = match values {
            Err(error) => {
                self.annotations.append(&wrapped(&format!("File metadata could not be loaded: {error}")));
                return;
            }
            Ok(values) => values.unwrap_or_default(),
        };
        if self.0.metadata.borrow().first().is_some_and(|m| m.is_symlink) {
            self.annotations.append(&wrapped("Open the symbolic link's target to edit tags, rating and comments."));
            return;
        }
        self.annotations.append(&plain("Tags"));
        let tags = gtk::Entry::builder().text(values.tags.join(", ")).placeholder_text("Separate tags with commas").max_length(4096).build();
        accessible(&tags, "File tags, separated by commas");
        self.annotations.append(&tags);
        self.0.tags_entry.replace(Some(tags));
        self.annotations.append(&plain("Rating"));
        let rating = gtk::DropDown::from_strings(&["Not rated", "★", "★★", "★★★", "★★★★", "★★★★★"]);
        accessible(&rating, "File rating");
        rating.set_selected(values.rating as u32);
        self.annotations.append(&rating);
        self.0.rating_choice.replace(Some(rating));
        self.annotations.append(&plain("Comment"));
        let comment = gtk::TextView::builder().wrap_mode(gtk::WrapMode::WordChar).vexpand(true).build();
        comment.buffer().set_text(&values.comment);
        accessible(&comment, "File comment");
        let scroll = gtk::ScrolledWindow::builder().min_content_height(100).vexpand(true).build();
        scroll.set_child(Some(&comment));
        self.annotations.append(&scroll);
        self.0.comment_text.replace(Some(comment));
        let save = gtk::Button::builder().label("Save tags and notes").halign(gtk::Align::Start).build();
        save.add_css_class("suggested-action");
        let weak = Rc::downgrade(&self.0);
        save.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                PropertiesWindow(inner).apply_annotations();
            }
        });
        self.annotations.append(&save);
    }

    /// `_apply_annotations`: write tags, rating and comment explicitly.
    pub fn apply_annotations(&self) {
        let (Some(tags), Some(rating), Some(comment)) = (self.tags_entry(), self.rating_choice(), self.comment_text()) else { return };
        let tags: Vec<String> = tags.text().split(',').map(|v| suite_core::pyjson::py_strip(v).to_string()).filter(|v| !v.is_empty()).collect();
        let rating = rating.selected();
        let comment = buffer_text(&comment);
        let (store, file) = (self.0.metadata_store.clone(), self.files[0].clone());
        self.change(move || {
            let mut fields = serde_json::Map::new();
            fields.insert("tags".into(), serde_json::json!(tags));
            fields.insert("rating".into(), serde_json::json!(rating));
            fields.insert("comment".into(), serde_json::json!(comment));
            let mut store = store.lock().unwrap();
            let store = store.get_or_insert_with(|| MetadataStore::new(None));
            store.write(&file, &fields).map(|_| ()).map_err(|e| error_text(&e))
        });
    }

    fn set_pages_sensitive(&self, sensitive: bool) {
        self.permissions.set_sensitive(sensitive);
        self.annotations.set_sensitive(sensitive);
        if let Some(acl) = self.acl() {
            acl.set_sensitive(sensitive);
        }
    }

    fn change(&self, function: impl FnOnce() -> Result<(), String> + Send + 'static) {
        if self.0.busy.get() {
            return;
        }
        self.0.busy.set(true);
        self.set_pages_sensitive(false);
        self.status.set_text("Applying changes…");
        self.work(function, |this, result| {
            this.0.busy.set(false);
            this.set_pages_sensitive(true);
            this.status.set_text(match &result {
                Err(error) => error,
                Ok(()) => "Changes applied",
            });
            if let Some(changed) = &this.0.on_changed {
                changed();
            }
        });
    }

    /// `_apply_mode`: three or four octal digits applied to every path.
    pub fn apply_mode(&self) {
        let Some(entry) = self.mode() else { return };
        let text = suite_core::pyjson::py_strip(&entry.text()).to_string();
        let mode = match octal(&text) {
            Some(mode) if text.chars().count() <= 4 => mode,
            _ => {
                self.status.set_text("Enter three or four octal digits, for example 0644 or 0755");
                return;
            }
        };
        let paths = self.paths.clone();
        self.change(move || {
            for path in &paths {
                tools::set_permissions(path, mode as i64).map_err(|e| error_text(&e))?;
            }
            Ok(())
        });
    }

    fn permission_toggled(&self) {
        if self.0.updating_mode.get() {
            return;
        }
        let Some(entry) = self.mode() else { return };
        self.0.updating_mode.set(true);
        let text = suite_core::pyjson::py_strip(&entry.text()).to_string();
        let mut mode = octal(&text).map(|m| m & 0o7000).unwrap_or(0);
        for (bit, check) in self.0.permission_checks.borrow().iter() {
            check.set_inconsistent(false);
            if check.is_active() {
                mode |= bit;
            }
        }
        entry.set_text(&format!("{mode:04o}"));
        self.0.updating_mode.set(false);
    }

    fn mode_text_changed(&self) {
        if self.0.updating_mode.get() {
            return;
        }
        let Some(entry) = self.mode() else { return };
        let text = suite_core::pyjson::py_strip(&entry.text()).to_string();
        let Some(mode) = octal(&text).filter(|_| text.chars().count() <= 4) else { return };
        self.0.updating_mode.set(true);
        let checks = self.0.permission_checks.borrow().clone();
        for (bit, check) in checks {
            check.set_inconsistent(false);
            check.set_active(mode & bit != 0);
        }
        self.0.updating_mode.set(false);
    }

    /// `_apply_owner`: empty fields are left unchanged (-1).
    pub fn apply_owner(&self) {
        let (Some(owner), Some(group)) = (self.owner(), self.group()) else { return };
        let owner = suite_core::pyjson::py_strip(&owner.text()).to_string();
        let group = suite_core::pyjson::py_strip(&group.text()).to_string();
        let paths = self.paths.clone();
        self.change(move || {
            for path in &paths {
                tools::set_owner(path, principal(&owner), principal(&group))
                    .map_err(|e| error_text(&e))?;
            }
            Ok(())
        });
    }

    /// `_apply_acl`.
    pub fn apply_acl(&self) {
        let Some(view) = self.acl_text() else { return };
        let content = buffer_text(&view);
        let path = self.paths[0].clone();
        self.change(move || tools::write_acl(&path, &content).map(|_| ()).map_err(|e| error_text(&e)));
    }

    fn build_associations(&self, infos: &[gio::FileInfo]) {
        if self.files.len() != 1 || infos[0].file_type() != gio::FileType::Regular {
            self.open_with.append(&wrapped("Choose one regular file to configure its default application."));
            return;
        }
        let mime = self.0.mime.borrow().clone().unwrap_or_default();
        let apps = gio::AppInfo::all_for_type(&mime);
        if apps.is_empty() {
            self.open_with.append(&wrapped("No installed applications advertise support for this file type."));
            return;
        }
        self.open_with.append(&wrapped(&format!("Default application for {}", gio::content_type_get_description(&mime))));
        let names: Vec<String> = apps.iter().map(|a| a.display_name().to_string()).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let choice = gtk::DropDown::from_strings(&names);
        accessible(&choice, "Default application");
        if let Some(default) = gio::AppInfo::default_for_type(&mime, false) {
            if let Some(index) = apps.iter().position(|app| app.equal(&default)) {
                choice.set_selected(index as u32);
            }
        }
        self.open_with.append(&choice);
        self.0.app_choice.replace(Some(choice));
        self.0.apps.replace(apps);
        let button = gtk::Button::builder().label("Set as default").halign(gtk::Align::Start).build();
        let weak = Rc::downgrade(&self.0);
        button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                PropertiesWindow(inner).set_default();
            }
        });
        self.open_with.append(&button);
    }

    /// `_set_default`: explicit association change for this file's type.
    pub fn set_default(&self) {
        let Some(choice) = self.app_choice() else { return };
        let app = self.0.apps.borrow().get(choice.selected() as usize).cloned();
        let mime = self.0.mime.borrow().clone().unwrap_or_default();
        let Some(app) = app else { return };
        match app.set_as_default_for_type(&mime) {
            Ok(()) => {
                self.status.set_text(&format!("Default application set to {}", app.display_name()));
                if let Some(changed) = &self.0.on_changed {
                    changed();
                }
            }
            Err(error) => self.status.set_text(&suite_core::pyerr::python_error_text(&error)),
        }
    }

    fn build_hashes(&self, infos: &[gio::FileInfo]) {
        if self.paths.len() != 1 || self.files.len() != 1 || infos[0].file_type() != gio::FileType::Regular {
            self.hash_page.append(&wrapped("Select one local regular file to calculate checksums."));
            return;
        }
        self.hash_page.append(&wrapped("Calculate checksums to verify this file against a known value."));
        let button = gtk::Button::builder().label("Calculate SHA-256, SHA-512 and MD5").halign(gtk::Align::Start).build();
        let weak = Rc::downgrade(&self.0);
        button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                PropertiesWindow(inner).calculate_hashes();
            }
        });
        self.hash_page.append(&button);
        let result = gtk::Label::builder().label("").selectable(true).wrap(true).xalign(0.0).build();
        result.add_css_class("monospace");
        self.hash_page.append(&result);
        self.0.hash_button.replace(Some(button));
        self.0.hash_result.replace(Some(result));
    }

    /// `_calculate_hashes`: SHA-256, SHA-512 and MD5 on a worker.
    pub fn calculate_hashes(&self) {
        let (Some(button), Some(result)) = (self.hash_button(), self.hash_result()) else { return };
        button.set_sensitive(false);
        result.set_text("Calculating…");
        let (path, cancel) = (self.paths[0].clone(), self.0.cancel.clone());
        self.work(move || tools::checksums(&path, &["sha256", "sha512", "md5"], Some(&*cancel)).map_err(|e| error_text(&e)),
                  move |_, value| {
            button.set_sensitive(true);
            result.set_text(&match value {
                Err(error) => error,
                Ok(sums) => sums.iter().map(|(name, value)| format!("{}\n{value}", name.to_uppercase())).collect::<Vec<_>>().join("\n\n"),
            });
        });
    }

    fn closing(&self) {
        self.0.closed.set(true);
        self.0.cancel.store(true, Ordering::SeqCst);
        self.0.gio_cancel.cancel();
    }
}
