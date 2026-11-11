//! Port of `~/tessel-bluetooth/shared/wallpaper_chooser.py` (+ `wallpaper_import.py`
//! in [`import`]): a modal GTK4 media chooser with large, lazily decoded
//! 224×126 wallpaper previews, shared by Tessel Shell's Wallpaper Settings and
//! Global Settings (candidate for tessel-gtk).
//!
//! ```ignore
//! let chooser = WallpaperChooser::new(Some(&parent), move |path: Option<PathBuf>| { /* stage it */ });
//! chooser.present();          // keep `chooser` to re-present instead of opening a second one
//! ```
//! `on_selected(path_or_none)` is called exactly once: Select/activate/Browse
//! or a single-file Add choose a path; Cancel, Escape, close, parent unrealize
//! or the optional cancellable give `None`. Nothing is applied here.

pub mod import;
pub mod media;
pub mod media_chooser;
pub mod pool;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tessel_core::tr;

use crate::fileops::mailbox::Mailbox;
pub use import::{Identity, ImportError, ImportOutcome, Importer, RemovalError};
pub use media::{
    default_wallpaper_folder, list_directory, preview, usable_video, validate_media, CACHE_LIMIT, IMAGE_SUFFIXES, PENDING_LIMIT,
    PREVIEW_HEIGHT, PREVIEW_WIDTH, VIDEO_SUFFIXES,
};
use media_chooser::MediaChooser;

pub const CSS: &str = "
.wallpaper-chooser gridview { background: transparent; }
.wallpaper-chooser gridview child { padding: 7px; border-radius: 8px; }
.wallpaper-chooser .wallpaper-preview {
    border-radius: 8px; background: alpha(@theme_fg_color, 0.06);
}
.wallpaper-chooser .wallpaper-filename { font-weight: 500; }
.wallpaper-chooser .wallpaper-remove {
    min-width: 26px; min-height: 26px; padding: 3px;
    border-radius: 7px; color: white;
    background: rgba(0, 0, 0, 0.65); border: none;
}
.wallpaper-chooser .wallpaper-remove:hover { background: #b3261e; }
";

/// Media validation used by Add/Browse (worker thread).
pub type Validator = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// Construction options (Python keyword arguments plus test seams).
#[derive(Default)]
pub struct Options {
    pub initial_folder: Option<PathBuf>,
    pub cancellable: Option<gio::Cancellable>,
    /// Overrides [`default_wallpaper_folder`] (the import collection).
    pub default_folder: Option<PathBuf>,
    pub importer: Option<Importer>,
    pub validate: Option<Validator>,
}

enum Message {
    Directory(u64, Option<PathBuf>, Option<String>, Result<Vec<(PathBuf, String)>, String>),
    Preview(u64, PathBuf, Result<Option<media::PreviewImage>, String>),
    Import(usize, ImportOutcome),
    Removal(PathBuf, Result<PathBuf, RemovalError>),
    Cancelled,
}

/// One grid cell's widgets (Python stores them on the list item).
pub struct Tile {
    pub child: gtk::Box,
    pub stack: gtk::Stack,
    pub icon: gtk::Image,
    pub picture: gtk::Picture,
    pub frame: gtk::Overlay,
    pub filename: gtk::Label,
    pub remove_button: gtk::Button,
    pub bound_path: RefCell<Option<PathBuf>>,
    pub preview_finished: Cell<bool>,
}

struct Widgets {
    navigation: gtk::Box,
    up_button: gtk::Button,
    location: gtk::Entry,
    browse_button: gtk::Button,
    add_button: gtk::Button,
    model: gio::ListStore,
    selection: gtk::SingleSelection,
    grid: gtk::GridView,
    scroll: gtk::ScrolledWindow,
    message: gtk::Label,
    select_button: gtk::Button,
}

struct Inner {
    window: gtk::Window,
    widgets: Widgets,
    callback: RefCell<Option<Box<dyn FnOnce(Option<PathBuf>)>>>,
    closed: Cell<bool>,
    generation: Arc<AtomicU64>,
    directory_loading: Cell<bool>,
    preview_timer: RefCell<Option<tessel_core::timer::Timeout>>,
    pending: RefCell<HashSet<PathBuf>>,
    tiles: RefCell<HashMap<gtk::ListItem, Rc<Tile>>>,
    bound: RefCell<Vec<(PathBuf, Rc<Tile>)>>,
    cache: RefCell<Vec<(PathBuf, Option<gdk::Texture>)>>,
    pool: pool::Pool,
    browse_dialog: RefCell<Option<MediaChooser>>,
    import_busy: Cell<bool>,
    import_cancel: Arc<AtomicBool>,
    removal_busy: Cell<bool>,
    removal_dialog: RefCell<Option<RemovalConfirmation>>,
    cancellable: Option<gio::Cancellable>,
    cancel_handler: RefCell<Option<gio::CancelledHandlerId>>,
    parent: Option<gtk::Window>,
    parent_handler: RefCell<Option<glib::SignalHandlerId>>,
    folder: RefCell<PathBuf>,
    wallpaper_folder: PathBuf,
    importer: RefCell<Importer>,
    validate: RefCell<Validator>,
    mailbox: RefCell<Option<Mailbox<Message>>>,
}

/// The wallpaper gallery window. Cheap to clone.
#[derive(Clone)]
pub struct WallpaperChooser(Rc<Inner>);

fn bound_get(bound: &[(PathBuf, Rc<Tile>)], path: &Path) -> Option<Rc<Tile>> {
    bound.iter().find(|(p, _)| p == path).map(|(_, t)| t.clone())
}

impl WallpaperChooser {
    /// `WallpaperChooser(parent, on_selected)`.
    pub fn new(parent: Option<&gtk::Window>, on_selected: impl FnOnce(Option<PathBuf>) + 'static) -> Self {
        Self::with_options(parent, on_selected, Options::default())
    }

    pub fn with_options(parent: Option<&gtk::Window>, on_selected: impl FnOnce(Option<PathBuf>) + 'static, options: Options) -> Self {
        let window = gtk::Window::builder().title(tr!("Choose Wallpaper")).modal(true).destroy_with_parent(true).build();
        if let Some(parent) = parent {
            window.set_transient_for(Some(parent));
        }
        window.set_default_size(800, 610);
        window.add_css_class("wallpaper-chooser");
        window.set_titlebar(Some(&tessel_gtk::utils::headerbar(&tr!("Choose Wallpaper"))));
        tessel_gtk::theme::attach(&window, None);
        tessel_gtk::utils::css_once(&WidgetExt::display(&window), "wallpaper-chooser", CSS, tessel_gtk::utils::PRIORITY_APPLICATION);

        let default_folder = options.default_folder.clone().unwrap_or_else(default_wallpaper_folder);
        let mut folder = default_folder.clone();
        if let Some(initial) = &options.initial_folder {
            let candidate = expand(initial);
            if candidate.is_dir() {
                folder = import::abspath(&candidate);
            }
        }

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(14)
            .margin_bottom(14)
            .margin_start(14)
            .margin_end(14)
            .build();
        window.set_child(Some(&body));
        let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let up_button = gtk::Button::builder().icon_name("go-up-symbolic").tooltip_text(tr!("Parent folder")).build();
        navigation.append(&up_button);
        let location = gtk::Entry::builder().hexpand(true).placeholder_text(tr!("Folder")).build();
        navigation.append(&location);
        let browse_button = gtk::Button::builder().label(tr!("Browse…")).tooltip_text(tr!("Choose another image or video")).build();
        navigation.append(&browse_button);
        let add_button = gtk::Button::builder()
            .label(tr!("Add"))
            .tooltip_text(tr!("Add images or videos to {folder}", folder = default_folder.display().to_string()))
            .build();
        navigation.append(&add_button);
        body.append(&navigation);

        let model = gio::ListStore::new::<gtk::StringObject>();
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let factory = gtk::SignalListItemFactory::new();
        let grid = gtk::GridView::builder()
            .model(&selection)
            .factory(&factory)
            .min_columns(2)
            .max_columns(4)
            .single_click_activate(false)
            .build();
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        scroll.set_child(Some(&grid));
        body.append(&scroll);
        let message = gtk::Label::builder().xalign(0.0).wrap(true).build();
        message.add_css_class("dim-label");
        body.append(&message);
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        buttons.append(&cancel);
        let select_button = gtk::Button::builder().label(tr!("Select")).sensitive(false).build();
        select_button.add_css_class("suggested-action");
        buttons.append(&select_button);
        body.append(&buttons);

        let validate: Validator = options.validate.clone().unwrap_or_else(|| Arc::new(validate_media));
        let chooser = WallpaperChooser(Rc::new(Inner {
            window: window.clone(),
            widgets: Widgets {
                navigation,
                up_button: up_button.clone(),
                location: location.clone(),
                browse_button: browse_button.clone(),
                add_button: add_button.clone(),
                model,
                selection: selection.clone(),
                grid: grid.clone(),
                scroll: scroll.clone(),
                message,
                select_button: select_button.clone(),
            },
            callback: RefCell::new(Some(Box::new(on_selected))),
            closed: Cell::new(false),
            generation: Arc::new(AtomicU64::new(0)),
            directory_loading: Cell::new(false),
            preview_timer: RefCell::new(None),
            pending: RefCell::new(HashSet::new()),
            tiles: RefCell::new(HashMap::new()),
            bound: RefCell::new(Vec::new()),
            cache: RefCell::new(Vec::new()),
            pool: pool::Pool::new(2, "wallpaper-preview"),
            browse_dialog: RefCell::new(None),
            import_busy: Cell::new(false),
            import_cancel: Arc::new(AtomicBool::new(false)),
            removal_busy: Cell::new(false),
            removal_dialog: RefCell::new(None),
            cancellable: options.cancellable.clone(),
            cancel_handler: RefCell::new(None),
            parent: parent.cloned(),
            parent_handler: RefCell::new(None),
            folder: RefCell::new(folder.clone()),
            wallpaper_folder: default_folder,
            importer: RefCell::new(options.importer.clone().unwrap_or_default()),
            validate: RefCell::new(validate),
            mailbox: RefCell::new(None),
        }));

        let weak = Rc::downgrade(&chooser.0);
        let on = move |f: fn(&WallpaperChooser)| {
            let weak = weak.clone();
            move || {
                if let Some(inner) = weak.upgrade() {
                    f(&WallpaperChooser(inner));
                }
            }
        };
        let mailbox_weak = Rc::downgrade(&chooser.0);
        *chooser.0.mailbox.borrow_mut() = Some(Mailbox::new(move |message| {
            if let Some(inner) = mailbox_weak.upgrade() {
                WallpaperChooser(inner).deliver(message);
                true
            } else {
                false
            }
        }));
        if let Some(parent) = parent {
            let f = on(|c| c.finish(None));
            *chooser.0.parent_handler.borrow_mut() = Some(parent.connect_unrealize(move |_| f()));
        }
        {
            let w = Rc::downgrade(&chooser.0);
            up_button.connect_clicked(move |_| {
                if let Some(inner) = w.upgrade() {
                    let c = WallpaperChooser(inner);
                    let parent = c.folder().parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
                    c.navigate(&parent, None, None);
                }
            });
            let w = Rc::downgrade(&chooser.0);
            location.connect_activate(move |entry| {
                if let Some(inner) = w.upgrade() {
                    WallpaperChooser(inner).navigate(Path::new(&entry.text().to_string()), None, None);
                }
            });
            let f = on(|c| c.browse(false));
            browse_button.connect_clicked(move |_| f());
            let f = on(|c| c.browse(true));
            add_button.connect_clicked(move |_| f());
            let f = on(|c| c.selection_changed());
            selection.connect_selected_item_notify(move |_| f());
            let f = on(|c| c.finish(None));
            cancel.connect_clicked(move |_| f());
            let f = on(|c| c.select());
            select_button.connect_clicked(move |_| f());
            let w = Rc::downgrade(&chooser.0);
            grid.connect_activate(move |_, position| {
                if let Some(inner) = w.upgrade() {
                    WallpaperChooser(inner).activate(position);
                }
            });
            for adjustment in [scroll.vadjustment()] {
                let f = on(|c| c.queue_previews());
                adjustment.connect_value_changed(move |_| f());
                let f = on(|c| c.queue_previews());
                adjustment.connect_changed(move |_| f());
            }
            let f = on(|c| c.queue_previews());
            grid.connect_map(move |_| f());
            let f = on(|c| c.finish(None));
            window.connect_close_request(move |_| {
                f();
                glib::Propagation::Stop
            });
            let f = on(|c| c.finish(None));
            window.connect_unrealize(move |_| f());
            let f = on(|c| c.finish(None));
            tessel_gtk::utils::key_controller(&window, false, move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    f();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            let w = Rc::downgrade(&chooser.0);
            factory.connect_setup(move |_, object| {
                if let (Some(inner), Some(item)) = (w.upgrade(), object.downcast_ref::<gtk::ListItem>()) {
                    WallpaperChooser(inner).setup(item);
                }
            });
            let w = Rc::downgrade(&chooser.0);
            factory.connect_bind(move |_, object| {
                if let (Some(inner), Some(item)) = (w.upgrade(), object.downcast_ref::<gtk::ListItem>()) {
                    WallpaperChooser(inner).bind(item);
                }
            });
            let w = Rc::downgrade(&chooser.0);
            factory.connect_unbind(move |_, object| {
                if let (Some(inner), Some(item)) = (w.upgrade(), object.downcast_ref::<gtk::ListItem>()) {
                    WallpaperChooser(inner).unbind(item);
                }
            });
            let w = Rc::downgrade(&chooser.0);
            factory.connect_teardown(move |_, object| {
                if let (Some(inner), Some(item)) = (w.upgrade(), object.downcast_ref::<gtk::ListItem>()) {
                    inner.tiles.borrow_mut().remove(item);
                }
            });
        }
        if let Some(cancellable) = &options.cancellable {
            let mailbox = chooser.0.mailbox.borrow().clone().unwrap();
            let id = cancellable.connect_cancelled(move |_| mailbox.send(Message::Cancelled));
            *chooser.0.cancel_handler.borrow_mut() = id;
        }
        chooser.navigate(&folder, None, None);
        chooser
    }

    // ------------------------------------------------------------ accessors

    pub fn window(&self) -> &gtk::Window {
        &self.0.window
    }
    pub fn present(&self) {
        if !self.0.closed.get() {
            self.0.window.present();
        }
    }
    pub fn close(&self) {
        self.finish(None);
    }
    pub fn is_closed(&self) -> bool {
        self.0.closed.get()
    }
    /// Python `_directory_future is not None`.
    pub fn is_loading(&self) -> bool {
        self.0.directory_loading.get()
    }
    pub fn is_import_busy(&self) -> bool {
        self.0.import_busy.get()
    }
    pub fn is_removal_busy(&self) -> bool {
        self.0.removal_busy.get()
    }
    pub fn folder(&self) -> PathBuf {
        self.0.folder.borrow().clone()
    }
    pub fn wallpaper_folder(&self) -> &Path {
        &self.0.wallpaper_folder
    }
    pub fn model(&self) -> &gio::ListStore {
        &self.0.widgets.model
    }
    pub fn selection(&self) -> &gtk::SingleSelection {
        &self.0.widgets.selection
    }
    pub fn grid(&self) -> &gtk::GridView {
        &self.0.widgets.grid
    }
    pub fn scroll(&self) -> &gtk::ScrolledWindow {
        &self.0.widgets.scroll
    }
    pub fn navigation(&self) -> &gtk::Box {
        &self.0.widgets.navigation
    }
    pub fn up_button(&self) -> &gtk::Button {
        &self.0.widgets.up_button
    }
    pub fn location(&self) -> &gtk::Entry {
        &self.0.widgets.location
    }
    pub fn browse_button(&self) -> &gtk::Button {
        &self.0.widgets.browse_button
    }
    pub fn add_button(&self) -> &gtk::Button {
        &self.0.widgets.add_button
    }
    pub fn select_button(&self) -> &gtk::Button {
        &self.0.widgets.select_button
    }
    pub fn message(&self) -> &gtk::Label {
        &self.0.widgets.message
    }
    /// Paths in model order.
    pub fn paths(&self) -> Vec<PathBuf> {
        let model = &self.0.widgets.model;
        (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gtk::StringObject>()).map(|s| PathBuf::from(s.string().as_str())).collect()
    }
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.0.widgets.selection.selected_item().and_downcast::<gtk::StringObject>().map(|s| PathBuf::from(s.string().as_str()))
    }
    pub fn tile(&self, path: &Path) -> Option<Rc<Tile>> {
        bound_get(&self.0.bound.borrow(), path)
    }
    pub fn bound_paths(&self) -> Vec<PathBuf> {
        self.0.bound.borrow().iter().map(|(p, _)| p.clone()).collect()
    }
    pub fn cache_paths(&self) -> Vec<PathBuf> {
        self.0.cache.borrow().iter().map(|(p, _)| p.clone()).collect()
    }
    pub fn pending_len(&self) -> usize {
        self.0.pending.borrow().len()
    }
    pub fn browse_dialog(&self) -> Option<MediaChooser> {
        self.0.browse_dialog.borrow().clone()
    }
    pub fn removal_dialog(&self) -> Option<RemovalConfirmation> {
        self.0.removal_dialog.borrow().clone()
    }
    pub fn set_importer(&self, importer: Importer) {
        *self.0.importer.borrow_mut() = importer;
    }
    pub fn set_validate(&self, validate: Validator) {
        *self.0.validate.borrow_mut() = validate;
    }

    fn send_job(&self, token: bool, job: impl FnOnce() -> Message + Send + 'static) {
        let Some(mailbox) = self.0.mailbox.borrow().clone() else { return };
        let token = token.then(|| (self.0.generation.clone(), self.0.generation.load(Ordering::SeqCst)));
        self.0.pool.submit(token, move || mailbox.send(job()));
    }

    fn blocked(&self) -> bool {
        self.0.closed.get() || self.0.import_busy.get() || self.0.removal_busy.get() || self.0.removal_dialog.borrow().is_some()
    }

    // ------------------------------------------------------------ lifecycle

    fn finish(&self, path: Option<PathBuf>) {
        let inner = &self.0;
        if inner.closed.get() {
            return;
        }
        inner.closed.set(true);
        inner.import_cancel.store(true, Ordering::SeqCst);
        let dialog = inner.removal_dialog.borrow_mut().take();
        if let Some(dialog) = dialog {
            dialog.window.destroy();
        }
        inner.generation.fetch_add(1, Ordering::SeqCst);
        let timer = inner.preview_timer.borrow_mut().take();
        if let Some(timer) = timer {
            if timer.is_pending() {
                timer.cancel();
            }
        }
        if let (Some(cancellable), Some(id)) = (&inner.cancellable, inner.cancel_handler.borrow_mut().take()) {
            cancellable.disconnect_cancelled(id);
        }
        if let (Some(parent), Some(id)) = (&inner.parent, inner.parent_handler.borrow_mut().take()) {
            parent.disconnect(id);
        }
        let browse = inner.browse_dialog.borrow_mut().take();
        if let Some(browse) = browse {
            browse.close();
        }
        inner.directory_loading.set(false);
        inner.pending.borrow_mut().clear();
        inner.bound.borrow_mut().clear();
        inner.cache.borrow_mut().clear();
        inner.pool.shutdown();
        let mailbox = inner.mailbox.borrow_mut().take();
        if let Some(mailbox) = mailbox {
            mailbox.close();
        }
        let callback = inner.callback.borrow_mut().take();
        inner.window.destroy();
        if let Some(callback) = callback {
            callback(path);
        }
    }

    fn deliver(&self, message: Message) {
        if self.0.closed.get() {
            return;
        }
        match message {
            Message::Directory(generation, select, message, result) => self.directory_loaded(generation, select, message, result),
            Message::Preview(generation, path, result) => self.preview_loaded(generation, path, result),
            Message::Import(count, outcome) => self.import_done(count, outcome),
            Message::Removal(path, result) => self.removal_done(path, result),
            Message::Cancelled => self.finish(None),
        }
    }

    // ------------------------------------------------------------ navigation

    /// Show `folder` (relative to the current one), optionally selecting a
    /// path and replacing the count message once loaded.
    pub fn navigate(&self, folder: &Path, select: Option<PathBuf>, message: Option<String>) {
        if self.blocked() {
            return;
        }
        let inner = &self.0;
        let mut folder = expand(folder);
        if !folder.is_absolute() {
            folder = self.folder().join(folder);
        }
        let folder = import::abspath(&folder);
        let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        inner.pending.borrow_mut().clear();
        inner.bound.borrow_mut().clear();
        inner.widgets.model.remove_all();
        *inner.folder.borrow_mut() = folder.clone();
        inner.widgets.location.set_text(&folder.display().to_string());
        inner.widgets.up_button.set_sensitive(folder.parent().is_some());
        inner.widgets.message.set_text(&tr!("Loading wallpapers…"));
        inner.directory_loading.set(true);
        self.send_job(true, move || {
            let result = list_directory(&folder).map_err(|e| tessel_core::pyerr::os_error_path(&e, &folder));
            Message::Directory(generation, select, message, result)
        });
    }

    fn directory_loaded(&self, generation: u64, select: Option<PathBuf>, message: Option<String>, result: Result<Vec<(PathBuf, String)>, String>) {
        let inner = &self.0;
        if generation != inner.generation.load(Ordering::SeqCst) {
            return;
        }
        inner.directory_loading.set(false);
        let entries = match result {
            Ok(entries) => entries,
            Err(error) => {
                inner.widgets.message.set_text(&tr!("Cannot open this folder: {error}", error = error));
                return;
            }
        };
        let objects: Vec<gtk::StringObject> = entries.iter().map(|(p, _)| gtk::StringObject::new(&p.to_string_lossy())).collect();
        inner.widgets.model.splice(0, 0, &objects);
        self.update_count();
        if let Some(select) = select {
            if let Some(index) = self.paths().iter().position(|p| *p == select) {
                inner.widgets.selection.set_selected(index as u32);
            }
        }
        if let Some(message) = message.filter(|m| !m.is_empty()) {
            inner.widgets.message.set_text(&message);
        }
    }

    fn update_count(&self) {
        if self.0.import_busy.get() || self.0.removal_busy.get() {
            return;
        }
        let count = self.0.widgets.model.n_items();
        self.0.widgets.message.set_text(&if count > 0 {
            tr!("{count} wallpapers and videos", count = count as i64)
        } else {
            tr!("No supported wallpapers found. Use Browse or choose another folder.")
        });
    }

    // ------------------------------------------------------------ tiles

    fn setup(&self, item: &gtk::ListItem) {
        let child = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let stack = gtk::Stack::new();
        stack.set_size_request(PREVIEW_WIDTH, PREVIEW_HEIGHT);
        let icon = gtk::Image::builder().pixel_size(56).build();
        let picture = gtk::Picture::builder().can_shrink(true).content_fit(gtk::ContentFit::Contain).build();
        stack.add_named(&icon, Some("icon"));
        stack.add_named(&picture, Some("preview"));
        // Keep the frame (and its trash button) centered at the preview size.
        let frame = gtk::Overlay::builder().halign(gtk::Align::Center).valign(gtk::Align::Start).build();
        frame.set_size_request(PREVIEW_WIDTH, PREVIEW_HEIGHT);
        frame.set_overflow(gtk::Overflow::Hidden);
        frame.add_css_class("wallpaper-preview");
        frame.set_child(Some(&stack));
        let remove_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .margin_start(6)
            .margin_bottom(6)
            .build();
        remove_button.add_css_class("wallpaper-remove");
        frame.add_overlay(&remove_button);
        child.append(&frame);
        let filename = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::Middle).max_width_chars(24).width_chars(24).build();
        filename.add_css_class("wallpaper-filename");
        child.append(&filename);
        item.set_child(Some(&child));
        let tile = Rc::new(Tile {
            child,
            stack,
            icon,
            picture,
            frame,
            filename,
            remove_button: remove_button.clone(),
            bound_path: RefCell::new(None),
            preview_finished: Cell::new(false),
        });
        let weak = Rc::downgrade(&self.0);
        let tile_weak: Weak<Tile> = Rc::downgrade(&tile);
        remove_button.connect_clicked(move |_| {
            if let (Some(inner), Some(tile)) = (weak.upgrade(), tile_weak.upgrade()) {
                let path = tile.bound_path.borrow().clone();
                WallpaperChooser(inner).confirm_removal(path);
            }
        });
        self.0.tiles.borrow_mut().insert(item.clone(), tile);
    }

    fn bind(&self, item: &gtk::ListItem) {
        let Some(tile) = self.0.tiles.borrow().get(item).cloned() else { return };
        let Some(object) = item.item().and_downcast::<gtk::StringObject>() else { return };
        let path = PathBuf::from(object.string().as_str());
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        *tile.bound_path.borrow_mut() = Some(path.clone());
        tile.preview_finished.set(false);
        tile.filename.set_text(&name);
        tile.remove_button.set_tooltip_text(Some(&tr!("Remove {name}", name = name.as_str())));
        tile.child.set_tooltip_text(Some(&path.to_string_lossy()));
        tile.picture.set_paintable(gdk::Paintable::NONE);
        tile.icon.set_icon_name(Some(if media::is_video(&path) { "video-x-generic-symbolic" } else { "image-x-generic-symbolic" }));
        tile.stack.set_visible_child_name("icon");
        {
            let mut bound = self.0.bound.borrow_mut();
            bound.retain(|(p, _)| *p != path);
            bound.push((path.clone(), tile.clone()));
        }
        let cached = {
            let mut cache = self.0.cache.borrow_mut();
            cache.iter().position(|(p, _)| *p == path).map(|index| {
                let entry = cache.remove(index);
                let texture = entry.1.clone();
                cache.push(entry);
                texture
            })
        };
        if let Some(texture) = cached {
            show_preview(&tile, texture.as_ref());
            tile.preview_finished.set(true);
        }
        self.queue_previews();
    }

    fn unbind(&self, item: &gtk::ListItem) {
        let Some(tile) = self.0.tiles.borrow().get(item).cloned() else { return };
        let path = tile.bound_path.borrow_mut().take();
        if let Some(path) = path {
            self.0.bound.borrow_mut().retain(|(p, t)| !(*p == path && Rc::ptr_eq(t, &tile)));
        }
        tile.picture.set_paintable(gdk::Paintable::NONE);
    }

    fn queue_previews(&self) {
        let inner = &self.0;
        if inner.closed.get() || inner.preview_timer.borrow().as_ref().is_some_and(|t| t.is_pending()) {
            return;
        }
        // Run after GTK allocates the newly bound or scrolled cells.
        let weak = Rc::downgrade(&self.0);
        let timer = tessel_core::timer::once(40, move || {
            if let Some(inner) = weak.upgrade() {
                inner.preview_timer.borrow_mut().take();
                WallpaperChooser(inner).pump_previews();
            }
        });
        *inner.preview_timer.borrow_mut() = Some(timer);
    }

    fn pump_previews(&self) {
        let inner = &self.0;
        if inner.closed.get() {
            return;
        }
        let viewport_height = inner.widgets.scroll.height() as f32;
        let bound = inner.bound.borrow().clone();
        for (path, tile) in bound {
            if inner.pending.borrow().len() >= PENDING_LIMIT {
                break;
            }
            if tile.preview_finished.get() || inner.pending.borrow().contains(&path) {
                continue;
            }
            // Decode only the viewport plus one row.
            let Some(bounds) = tile.child.compute_bounds(&inner.widgets.scroll) else { continue };
            let row = PREVIEW_HEIGHT as f32;
            if viewport_height <= 0.0 || bounds.height() <= 0.0 || bounds.y() + bounds.height() < -row || bounds.y() > viewport_height + row {
                continue;
            }
            inner.pending.borrow_mut().insert(path.clone());
            let generation = inner.generation.load(Ordering::SeqCst);
            self.send_job(true, move || {
                let result = preview(&path);
                Message::Preview(generation, path, result)
            });
        }
    }

    fn preview_loaded(&self, generation: u64, path: PathBuf, result: Result<Option<media::PreviewImage>, String>) {
        let inner = &self.0;
        if generation != inner.generation.load(Ordering::SeqCst) {
            return;
        }
        inner.pending.borrow_mut().remove(&path);
        let Some(index) = self.paths().iter().position(|p| *p == path) else {
            // A finished preview must not repopulate a deleted tile's cache.
            return;
        };
        let texture = result.ok().flatten().map(|image| image.to_texture());
        if texture.is_none() && media::is_image(&path) {
            // Damaged image data or a vanished file: remove the tile.
            inner.bound.borrow_mut().retain(|(p, _)| *p != path);
            inner.widgets.model.remove(index as u32);
            inner.cache.borrow_mut().retain(|(p, _)| *p != path);
            self.update_count();
            self.queue_previews();
            return;
        }
        {
            let mut cache = inner.cache.borrow_mut();
            cache.retain(|(p, _)| *p != path);
            cache.push((path.clone(), texture.clone()));
            while cache.len() > CACHE_LIMIT {
                cache.remove(0);
            }
        }
        if let Some(tile) = bound_get(&inner.bound.borrow(), &path) {
            show_preview(&tile, texture.as_ref());
            tile.preview_finished.set(true);
        }
        self.queue_previews();
    }

    // ------------------------------------------------------------ selection

    fn selection_changed(&self) {
        let inner = &self.0;
        let has = inner.widgets.selection.selected_item().is_some();
        inner.widgets.select_button.set_sensitive(has && !inner.import_busy.get() && !inner.removal_busy.get());
    }

    fn activate(&self, position: u32) {
        if self.blocked() {
            return;
        }
        self.0.widgets.selection.set_selected(position);
        self.select();
    }

    /// The Select button.
    pub fn select(&self) {
        if self.blocked() {
            return;
        }
        let Some(path) = self.selected_path() else { return };
        if path.is_file() {
            self.finish(Some(path));
        } else {
            self.0.widgets.message.set_text(&tr!("This file is no longer available. Choose another wallpaper."));
        }
    }

    // ------------------------------------------------------------ browse / add

    /// Browse… (`add = false`) or Add (`add = true`, multiple, imports).
    pub fn browse(&self, add: bool) {
        if self.0.browse_dialog.borrow().is_some() || self.blocked() {
            return;
        }
        let start = if add {
            glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(tessel_core::paths::home)
        } else {
            self.folder()
        };
        let weak = Rc::downgrade(&self.0);
        let filters = vec![
            (tr!("Images & Videos"), media::media_suffixes()),
            (tr!("Images"), IMAGE_SUFFIXES.to_vec()),
            (tr!("Videos"), VIDEO_SUFFIXES.to_vec()),
        ];
        let dialog = MediaChooser::new(
            &self.0.window,
            move |files| {
                if let Some(inner) = weak.upgrade() {
                    WallpaperChooser(inner).browse_done(files, add);
                }
            },
            media_chooser::Config {
                title: if add { tr!("Add Wallpapers") } else { tr!("Choose Image or Video") },
                initial_folder: start,
                filters,
                validate: Some(self.0.validate.borrow().clone()),
                multiple: add,
                accept_label: add.then(|| tr!("Add")),
            },
        );
        *self.0.browse_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present();
    }

    fn browse_done(&self, files: Option<Vec<gio::File>>, add: bool) {
        self.0.browse_dialog.borrow_mut().take();
        let Some(files) = files else { return };
        if self.0.closed.get() || files.is_empty() {
            return;
        }
        let paths: Vec<Option<PathBuf>> = files.iter().map(|f| f.path()).collect();
        if paths.iter().any(Option::is_none) {
            self.0.widgets.message.set_text(&tr!("Choose an image or video stored on this computer."));
            return;
        }
        let paths: Vec<PathBuf> = paths.into_iter().flatten().collect();
        if add {
            self.start_import(paths);
        } else {
            self.finish(paths.into_iter().next());
        }
    }

    /// Import `paths` into the wallpaper collection on a worker.
    pub fn start_import(&self, paths: Vec<PathBuf>) {
        if self.blocked() || paths.is_empty() {
            return;
        }
        let inner = &self.0;
        inner.import_busy.set(true);
        inner.import_cancel.store(false, Ordering::SeqCst);
        inner.widgets.navigation.set_sensitive(false);
        inner.widgets.grid.set_sensitive(false);
        inner.widgets.select_button.set_sensitive(false);
        inner.widgets.message.set_text(&if paths.len() == 1 {
            tr!("Adding wallpaper…")
        } else {
            tr!("Adding {count} wallpapers…", count = paths.len() as i64)
        });
        let importer = inner.importer.borrow().clone();
        let validate = inner.validate.borrow().clone();
        let cancel = inner.import_cancel.clone();
        let folder = inner.wallpaper_folder.clone();
        let count = paths.len();
        self.send_job(false, move || {
            let cancelled = move || cancel.load(Ordering::SeqCst);
            let validate = move |p: &Path| validate(p);
            Message::Import(count, importer.import_wallpapers(&paths, &folder, Some(&cancelled), Some(&validate)))
        });
    }

    fn import_done(&self, count: usize, (imported, errors, cancelled): ImportOutcome) {
        let inner = &self.0;
        inner.import_busy.set(false);
        inner.widgets.navigation.set_sensitive(true);
        inner.widgets.grid.set_sensitive(true);
        self.selection_changed();
        if count == 1 && imported.len() == 1 && errors.is_empty() && !cancelled {
            self.finish(imported.into_iter().next());
            return;
        }
        let message = if cancelled {
            tr!("Adding wallpapers was cancelled. {count} files were added.", count = imported.len() as i64)
        } else if !errors.is_empty() {
            let mut message = tr!(
                "Added {count} wallpapers. Could not add wallpaper: {error}",
                count = imported.len() as i64,
                error = errors.iter().take(3).cloned().collect::<Vec<_>>().join("\n")
            );
            if errors.len() > 3 {
                message.push('\n');
                message.push_str(&tr!("{count} more files could not be added.", count = (errors.len() - 3) as i64));
            }
            message
        } else {
            tr!("Added {count} wallpapers. Choose one to use.", count = imported.len() as i64)
        };
        if let Some(first) = imported.first().cloned() {
            let folder = inner.wallpaper_folder.clone();
            self.navigate(&folder, Some(first), Some(message));
        } else {
            inner.widgets.message.set_text(&message);
        }
    }

    // ------------------------------------------------------------ removal

    fn confirm_removal(&self, path: Option<PathBuf>) {
        let Some(path) = path else { return };
        if self.blocked() {
            return;
        }
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) => {
                let text = tessel_core::pyerr::os_error_path(&error, &path);
                self.0.widgets.message.set_text(&tr!("Could not remove wallpaper: {error}", error = text));
                return;
            }
        };
        use std::os::unix::fs::MetadataExt;
        let identity = (meta.dev(), meta.ino());
        let folder = self.folder();
        let weak = Rc::downgrade(&self.0);
        let target = path.clone();
        let dialog = RemovalConfirmation::new(&self.0.window, &path, move |dialog, response| {
            let Some(inner) = weak.upgrade() else {
                dialog.window.destroy();
                return;
            };
            let chooser = WallpaperChooser(inner);
            let current = chooser.0.removal_dialog.borrow().as_ref().is_some_and(|d| d.window == dialog.window);
            if current {
                chooser.0.removal_dialog.borrow_mut().take();
            }
            dialog.window.destroy();
            if response == gtk::ResponseType::Accept && !chooser.0.closed.get() {
                chooser.start_removal(target.clone(), folder.clone(), identity);
            }
        });
        *self.0.removal_dialog.borrow_mut() = Some(dialog.clone());
        dialog.window.present();
    }

    fn start_removal(&self, path: PathBuf, folder: PathBuf, identity: Identity) {
        let inner = &self.0;
        inner.removal_busy.set(true);
        inner.widgets.navigation.set_sensitive(false);
        inner.widgets.grid.set_sensitive(false);
        inner.widgets.select_button.set_sensitive(false);
        inner.widgets.message.set_text(&tr!("Removing wallpaper…"));
        let importer = inner.importer.borrow().clone();
        let cancel = inner.import_cancel.clone();
        self.send_job(false, move || {
            let cancelled = move || cancel.load(Ordering::SeqCst);
            let result = importer.remove_wallpaper(&path, &folder, Some(identity), Some(&cancelled));
            Message::Removal(path, result)
        });
    }

    fn removal_done(&self, path: PathBuf, result: Result<PathBuf, RemovalError>) {
        let inner = &self.0;
        inner.removal_busy.set(false);
        inner.widgets.navigation.set_sensitive(true);
        inner.widgets.grid.set_sensitive(true);
        match result {
            Err(RemovalError::Cancelled(_)) => inner.widgets.message.set_text(&tr!("Wallpaper was not removed.")),
            Err(RemovalError::Error(error)) => inner.widgets.message.set_text(&tr!("Could not remove wallpaper: {error}", error = error)),
            Ok(_) => {
                if self.selected_path().as_deref() == Some(path.as_path()) {
                    inner.widgets.selection.set_selected(gtk::INVALID_LIST_POSITION);
                }
                inner.pending.borrow_mut().remove(&path);
                inner.cache.borrow_mut().retain(|(p, _)| *p != path);
                inner.bound.borrow_mut().retain(|(p, _)| *p != path);
                if let Some(index) = self.paths().iter().position(|p| *p == path) {
                    inner.widgets.model.remove(index as u32);
                }
                self.update_count();
            }
        }
        self.selection_changed();
    }
}

fn show_preview(tile: &Tile, texture: Option<&gdk::Texture>) {
    if let Some(texture) = texture {
        tile.picture.set_paintable(Some(texture));
        tile.stack.set_visible_child_name("preview");
    }
}

fn expand(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return tessel_core::paths::home();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return tessel_core::paths::home().join(rest);
    }
    path.to_path_buf()
}

// ---------------------------------------------------------------- confirmation

struct ConfirmInner {
    responded: Cell<bool>,
    on_response: RefCell<Option<Box<dyn FnOnce(&RemovalConfirmation, gtk::ResponseType)>>>,
}

/// Suite-styled modal prompt with a safe default (Cancel) and one response.
#[derive(Clone)]
pub struct RemovalConfirmation {
    pub window: gtk::Window,
    pub message: gtk::Label,
    pub cancel_button: gtk::Button,
    pub remove_button: gtk::Button,
    inner: Rc<ConfirmInner>,
}

impl RemovalConfirmation {
    pub fn new(parent: &gtk::Window, path: &Path, on_response: impl FnOnce(&RemovalConfirmation, gtk::ResponseType) + 'static) -> Self {
        let window = gtk::Window::builder()
            .title(tr!("Remove Wallpaper"))
            .transient_for(parent)
            .modal(true)
            .destroy_with_parent(true)
            .resizable(false)
            .build();
        window.set_titlebar(Some(&tessel_gtk::utils::headerbar(&tr!("Remove Wallpaper"))));
        window.set_default_size(420, -1);
        window.add_css_class("wallpaper-chooser");
        tessel_gtk::theme::attach(&window, None);
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        window.set_child(Some(&body));
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let message = gtk::Label::builder()
            .label(tr!("Permanently remove “{name}” from this folder?\n\nThis cannot be undone.", name = name.as_str()))
            .wrap(true)
            .xalign(0.0)
            .max_width_chars(48)
            .build();
        body.append(&message);
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel_button = gtk::Button::with_label(&tr!("Cancel"));
        let remove_button = gtk::Button::with_label(&tr!("Remove"));
        remove_button.add_css_class("destructive-action");
        buttons.append(&cancel_button);
        buttons.append(&remove_button);
        body.append(&buttons);
        window.set_default_widget(Some(&cancel_button));
        cancel_button.grab_focus();
        let dialog = RemovalConfirmation {
            window: window.clone(),
            message,
            cancel_button: cancel_button.clone(),
            remove_button: remove_button.clone(),
            inner: Rc::new(ConfirmInner { responded: Cell::new(false), on_response: RefCell::new(Some(Box::new(on_response))) }),
        };
        let d = dialog.downgrade();
        cancel_button.connect_clicked(move |_| {
            if let Some(d) = RemovalConfirmation::upgrade(&d) {
                d.response(gtk::ResponseType::Cancel);
            }
        });
        let d = dialog.downgrade();
        remove_button.connect_clicked(move |_| {
            if let Some(d) = RemovalConfirmation::upgrade(&d) {
                d.response(gtk::ResponseType::Accept);
            }
        });
        let d = dialog.downgrade();
        window.connect_close_request(move |_| {
            if let Some(d) = RemovalConfirmation::upgrade(&d) {
                d.response(gtk::ResponseType::Cancel);
            }
            glib::Propagation::Stop
        });
        let d = dialog.downgrade();
        tessel_gtk::utils::key_controller(&window, true, move |_, key, _, _| match RemovalConfirmation::upgrade(&d) {
            Some(d) => d.key_pressed(key),
            None => glib::Propagation::Proceed,
        });
        dialog
    }

    fn downgrade(&self) -> (glib::WeakRef<gtk::Window>, glib::WeakRef<gtk::Label>, glib::WeakRef<gtk::Button>, glib::WeakRef<gtk::Button>, Weak<ConfirmInner>) {
        (self.window.downgrade(), self.message.downgrade(), self.cancel_button.downgrade(), self.remove_button.downgrade(), Rc::downgrade(&self.inner))
    }

    #[allow(clippy::type_complexity)]
    fn upgrade(
        weak: &(glib::WeakRef<gtk::Window>, glib::WeakRef<gtk::Label>, glib::WeakRef<gtk::Button>, glib::WeakRef<gtk::Button>, Weak<ConfirmInner>),
    ) -> Option<Self> {
        Some(RemovalConfirmation { window: weak.0.upgrade()?, message: weak.1.upgrade()?, cancel_button: weak.2.upgrade()?, remove_button: weak.3.upgrade()?, inner: weak.4.upgrade()? })
    }

    /// Python `_key_pressed`: Escape cancels (and returns handled).
    pub fn key_pressed(&self, key: gdk::Key) -> glib::Propagation {
        if key == gdk::Key::Escape {
            self.response(gtk::ResponseType::Cancel);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    pub fn response(&self, response: gtk::ResponseType) {
        if self.inner.responded.replace(true) {
            return;
        }
        let callback = self.inner.on_response.borrow_mut().take();
        if let Some(callback) = callback {
            callback(self, response);
        }
    }

    pub fn close(&self) {
        self.window.close();
    }
}
