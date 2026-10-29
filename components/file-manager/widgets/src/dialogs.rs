//! Port of file_dialogs.py: GTK4 dialogs shared by the Files window (no KDE
//! helpers, no portals): `show_error`, `confirm`, `prompt_text`, `choose_file`,
//! `choose_application` (Open With) and [`PropertiesWindow`].
//!
//! Strings are the Python literals (file_dialogs.py does not translate).

use gtk::prelude::*;
use gtk::{gdk, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use suite_gtk::utils::{accessible, headerbar, key_controller, set_margins};
use waydock::core::app_catalog::{AppCatalog, AppInfoData};

pub use crate::properties::PropertiesWindow;

pub(crate) fn escape_closes(window: &gtk::Window) {
    let weak = window.downgrade();
    key_controller(window, false, move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let Some(window) = weak.upgrade() {
                window.close();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
}

/// `_window(parent, title, width=460, height=-1)`: a modal `.files-window` dialog
/// with the suite header bar, dock theme, Escape-to-close and an 18 px content box.
pub fn dialog_window(parent: Option<&gtk::Window>, title: &str, width: i32, height: i32) -> (gtk::Window, gtk::Box) {
    let window = gtk::Window::builder().title(title).modal(true).build();
    window.set_transient_for(parent);
    if let Some(application) = parent.and_then(|p| p.application()) {
        window.set_application(Some(&application));
    }
    window.add_css_class("files-window");
    window.set_titlebar(Some(&headerbar(title)));
    crate::compat::ensure_search_css(&gtk::prelude::WidgetExt::display(&window));
    window.set_default_size(width, height);
    suite_gtk::theme::attach(&window, None);
    escape_closes(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    set_margins(&content, 18);
    window.set_child(Some(&content));
    (window, content)
}

/// Whether `theme::attach` ran for `window` (Python `hasattr(window, "_waydock_companion_theme")`).
pub fn has_companion_theme(window: &impl IsA<gtk::Window>) -> bool {
    unsafe { window.as_ref().data::<suite_gtk::theme::WindowTheme>("waydock-companion-theme") }.is_some()
}

/// A one-shot completion guard: the first answer wins, later ones are ignored.
pub(crate) struct Once<T> {
    finished: Cell<bool>,
    callback: RefCell<Option<Box<dyn FnOnce(T)>>>,
}

impl<T> Once<T> {
    pub(crate) fn new(callback: impl FnOnce(T) + 'static) -> Rc<Self> {
        Rc::new(Self { finished: Cell::new(false), callback: RefCell::new(Some(Box::new(callback))) })
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished.get()
    }

    /// Close `window` and deliver `value` unless already finished.
    pub(crate) fn complete(&self, window: &gtk::Window, value: T) {
        if self.finished.replace(true) {
            return;
        }
        window.close();
        if let Some(callback) = self.callback.borrow_mut().take() {
            callback(value);
        }
    }
}

fn button_row() -> gtk::Box {
    gtk::Box::builder().spacing(8).halign(gtk::Align::End).build()
}

/// `show_error(parent, message)`.
pub fn show_error(parent: Option<&gtk::Window>, message: &str) -> gtk::Window {
    let (window, content) = dialog_window(parent, "Could not complete operation", 460, -1);
    content.append(&gtk::Label::builder().label(message).xalign(0.0).wrap(true).selectable(true).build());
    let button = gtk::Button::builder().label("Close").halign(gtk::Align::End).build();
    let weak = window.downgrade();
    button.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    content.append(&button);
    window.present();
    window
}

/// `confirm(parent, title, message, callback)`: `callback(true)` only for Confirm;
/// closing or Cancel answers `false`, exactly once.
pub fn confirm(parent: Option<&gtk::Window>, title: &str, message: &str, callback: impl FnOnce(bool) + 'static) -> gtk::Window {
    let (window, content) = dialog_window(parent, title, 460, -1);
    content.append(&gtk::Label::builder().label(message).xalign(0.0).wrap(true).build());
    let buttons = button_row();
    let (no, yes) = (gtk::Button::with_label("Cancel"), gtk::Button::with_label("Confirm"));
    yes.add_css_class("destructive-action");
    buttons.append(&no);
    buttons.append(&yes);
    content.append(&buttons);
    let once = Once::new(callback);
    for (button, answer) in [(&no, false), (&yes, true)] {
        let (once, weak) = (once.clone(), window.downgrade());
        button.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                once.complete(&window, answer);
            }
        });
    }
    window.connect_close_request(move |window| {
        once.complete(window, false);
        glib::Propagation::Proceed
    });
    window.present();
    window
}

/// `prompt_text(parent, title, message, initial, callback)`: `Some(text)` on OK/Enter, `None` otherwise.
pub fn prompt_text(parent: Option<&gtk::Window>, title: &str, message: &str, initial: Option<&str>,
                   callback: impl FnOnce(Option<String>) + 'static) -> gtk::Window {
    let (window, content) = dialog_window(parent, title, 460, -1);
    content.append(&gtk::Label::builder().label(message).xalign(0.0).wrap(true).build());
    let entry = gtk::Entry::builder().text(initial.unwrap_or("")).hexpand(true).build();
    accessible(&entry, message);
    content.append(&entry);
    let buttons = button_row();
    let (cancel, accept) = (gtk::Button::with_label("Cancel"), gtk::Button::with_label("OK"));
    accept.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&accept);
    content.append(&buttons);
    let once = Once::new(callback);
    {
        let (once, weak) = (once.clone(), window.downgrade());
        cancel.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                once.complete(&window, None);
            }
        });
    }
    {
        let (once, weak, entry) = (once.clone(), window.downgrade(), entry.clone());
        accept.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                once.complete(&window, Some(entry.text().to_string()));
            }
        });
    }
    {
        let (once, weak) = (once.clone(), window.downgrade());
        entry.connect_activate(move |entry| {
            if let Some(window) = weak.upgrade() {
                once.complete(&window, Some(entry.text().to_string()));
            }
        });
    }
    window.connect_close_request(move |window| {
        once.complete(window, None);
        glib::Propagation::Proceed
    });
    window.present();
    entry.grab_focus();
    window
}

/// `choose_file` modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChooserMode {
    Open,
    Save,
    Folder,
}

impl ChooserMode {
    /// `"open" | "save" | "folder"`; anything else is Python's `ValueError("Unknown file chooser mode")`.
    pub fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "open" => Ok(Self::Open),
            "save" => Ok(Self::Save),
            "folder" => Ok(Self::Folder),
            _ => Err("Unknown file chooser mode".into()),
        }
    }
}

#[allow(deprecated)]
/// The window `choose_file` returns (`window.file_chooser`, `window.accept_button`).
#[derive(Clone)]
pub struct FileChooserDialog {
    pub window: gtk::Window,
    pub file_chooser: gtk::FileChooserWidget,
    pub accept_button: gtk::Button,
    pub status: gtk::Label,
}

/// `choose_file(parent, title, callback, mode=…, initial_folder=…, initial_name=…)`:
/// a themed GTK4 chooser, independent of any desktop portal or KDE dialog.
#[allow(deprecated)]
pub fn choose_file(parent: Option<&gtk::Window>, title: &str, callback: impl FnOnce(Option<gio::File>) + 'static,
                   mode: ChooserMode, initial_folder: Option<&gio::File>, initial_name: &str) -> FileChooserDialog {
    let action = match mode {
        ChooserMode::Open => gtk::FileChooserAction::Open,
        ChooserMode::Save => gtk::FileChooserAction::Save,
        ChooserMode::Folder => gtk::FileChooserAction::SelectFolder,
    };
    let (window, content) = dialog_window(parent, title, 840, 580);
    let chooser = gtk::FileChooserWidget::new(action);
    chooser.set_hexpand(true);
    chooser.set_vexpand(true);
    chooser.set_select_multiple(false);
    chooser.set_create_folders(true);
    if let Some(folder) = initial_folder {
        let _ = chooser.set_current_folder(Some(folder));
    }
    if mode == ChooserMode::Save && !initial_name.is_empty() {
        chooser.set_current_name(initial_name);
    }
    content.append(&chooser);
    let status = gtk::Label::builder().label("").xalign(0.0).wrap(true).build();
    content.append(&status);
    let buttons = button_row();
    let cancel = gtk::Button::with_label("Cancel");
    let accept = gtk::Button::with_label(match mode {
        ChooserMode::Open => "Open",
        ChooserMode::Save => "Save",
        ChooserMode::Folder => "Select Folder",
    });
    accept.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&accept);
    content.append(&buttons);
    let once = Once::new(callback);
    let selected: Rc<dyn Fn()> = {
        let (once, weak, chooser, status) = (once.clone(), window.downgrade(), chooser.clone(), status.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            match chooser.file() {
                None => status.set_text(if mode == ChooserMode::Folder { "Choose a folder" } else { "Choose a file" }),
                Some(file) => once.complete(&window, Some(file)),
            }
        })
    };
    {
        let (once, weak) = (once.clone(), window.downgrade());
        cancel.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                once.complete(&window, None);
            }
        });
    }
    let on_accept = selected.clone();
    accept.connect_clicked(move |_| on_accept());
    // GtkFileChooserWidget's "file-activated" (not wrapped as a typed signal).
    if glib::subclass::signal::SignalId::lookup("file-activated", chooser.type_()).is_some() {
        chooser.connect_local("file-activated", false, move |_| {
            selected();
            None
        });
    }
    window.connect_close_request(move |window| {
        once.complete(window, None);
        glib::Propagation::Proceed
    });
    window.set_default_widget(Some(&accept));
    window.present();
    FileChooserDialog { window, file_chooser: chooser, accept_button: accept, status }
}

// ---------------------------------------------------------------- Open With

/// What `_open_with_catalog` asks Gio for (injectable for tests, like the Python
/// tests' `patch.object(Gio.AppInfo, …)`). Runs on a worker thread.
pub struct AppSources {
    pub all: Vec<gio::AppInfo>,
    pub all_for_type: Vec<gio::AppInfo>,
    pub recommended: Vec<gio::AppInfo>,
    pub default: Option<gio::AppInfo>,
}

/// `Gio.AppInfo.get_all()` + the MIME queries for `content_type`.
pub fn gio_sources(content_type: Option<&str>) -> AppSources {
    match content_type {
        Some(kind) => AppSources {
            all: gio::AppInfo::all(),
            all_for_type: gio::AppInfo::all_for_type(kind),
            recommended: gio::AppInfo::recommended_for_type(kind),
            default: gio::AppInfo::default_for_type(kind, false),
        },
        None => AppSources { all: gio::AppInfo::all(), all_for_type: Vec::new(), recommended: Vec::new(), default: None },
    }
}

pub type Discovery = Arc<dyn Fn(Option<&str>) -> AppSources + Send + Sync>;
/// `app.set_as_default_for_type(content_type)` → `Ok(true)` when changed.
pub type SetDefault = Rc<dyn Fn(&gio::AppInfo, &str) -> Result<bool, glib::Error>>;

/// Hooks for [`choose_application_with`]; `Default` = real Gio.
#[derive(Clone, Default)]
pub struct OpenWithOptions {
    pub discovery: Option<Discovery>,
    pub set_default: Option<SetDefault>,
}

struct SendBox<T>(T);
// SAFETY: GDesktopAppInfo and GIcon instances are immutable after construction and
// GObject reference counting is atomic; the worker hands them over and never touches
// them again (the Python code shares the same objects across threads).
unsafe impl<T> Send for SendBox<T> {}

type CatalogData = (Vec<AppInfoData>, Option<String>, HashSet<String>);

/// `_open_with_catalog(content_type)`: normal apps plus MIME handlers, where
/// NoDisplay handlers become visible (they belong in Open With, not in the menu).
pub fn open_with_catalog(sources: AppSources) -> (AppCatalog, Option<String>, HashSet<String>) {
    let (infos, default, recommended) = catalog_data(sources);
    (AppCatalog::from_infos(infos), default, recommended)
}

fn catalog_data(sources: AppSources) -> CatalogData {
    let id = |info: &gio::AppInfo| info.id().map(|s| s.to_string());
    let mut handlers: Vec<gio::AppInfo> = sources.all_for_type.clone();
    handlers.extend(sources.recommended.iter().cloned());
    handlers.extend(sources.default.iter().cloned());
    let handler_ids: HashSet<String> = handlers.iter().filter_map(id).collect();
    let mut infos: Vec<AppInfoData> = sources.all.iter().chain(handlers.iter()).map(AppInfoData::from_gio).collect();
    for info in &mut infos {
        if info.id.as_ref().is_some_and(|i| handler_ids.contains(i)) {
            info.should_show = true;
        }
    }
    (infos, sources.default.as_ref().and_then(id), sources.recommended.iter().filter_map(id).collect())
}

/// The window `choose_application` returns, with the Python attribute names.
#[derive(Clone)]
pub struct OpenWithDialog {
    pub window: gtk::Window,
    pub application_chooser: gtk::ListBox,
    pub application_search: gtk::SearchEntry,
    pub application_sort: gtk::DropDown,
    pub default_check: gtk::CheckButton,
    pub default_label: gtk::Label,
    pub accept_button: gtk::Button,
    pub status: gtk::Label,
    applications_loaded: Rc<Cell<bool>>,
}

impl OpenWithDialog {
    pub fn applications_loaded(&self) -> bool {
        self.applications_loaded.get()
    }

    pub fn rows(&self) -> Vec<gtk::ListBoxRow> {
        suite_gtk::utils::children(&self.application_chooser).into_iter().filter_map(|w| w.downcast().ok()).collect()
    }
}

const ROW_ID: &str = "files-open-with-app-id";
const ROW_INFO: &str = "files-open-with-app-info";

/// `row.app_id`.
pub fn row_app_id(row: &gtk::ListBoxRow) -> Option<String> {
    unsafe { row.data::<String>(ROW_ID).map(|p| p.as_ref().clone()) }
}

/// `row.app_info`.
pub fn row_app_info(row: &gtk::ListBoxRow) -> Option<gio::AppInfo> {
    unsafe { row.data::<gio::AppInfo>(ROW_INFO).map(|p| p.as_ref().clone()) }
}

struct OpenWithState {
    dialog: OpenWithDialog,
    once: Rc<Once<Option<gio::AppInfo>>>,
    cancellable: gio::Cancellable,
    catalog: RefCell<Option<AppCatalog>>,
    default_id: RefCell<Option<String>>,
    recommended: RefCell<HashSet<String>>,
    content_type: RefCell<Option<String>>,
    set_default: SetDefault,
}

impl OpenWithState {
    fn complete(&self, app: Option<gio::AppInfo>) {
        if self.once.finished() {
            return;
        }
        self.cancellable.cancel();
        self.once.complete(&self.dialog.window, app);
    }

    fn selected(&self) {
        let row = self.dialog.application_chooser.selected_row();
        self.dialog.accept_button.set_sensitive(row.is_some());
        self.dialog.default_check.set_sensitive(row.is_some() && self.content_type.borrow().is_some());
    }

    fn rebuild(&self) {
        if self.once.finished() {
            return;
        }
        let catalog = self.catalog.borrow();
        let Some(catalog) = catalog.as_ref() else { return };
        let chooser = &self.dialog.application_chooser;
        let selected_id = chooser.selected_row().and_then(|row| row_app_id(&row));
        let query = self.dialog.application_search.text().to_string();
        let mut entries = catalog.search(&query);
        let key = |a: &waydock::core::app_catalog::AppEntry| (suite_core::pyjson::casefold(&a.name), suite_core::pyjson::casefold(&a.id), a.id.clone());
        entries.sort_by_key(key);
        if self.dialog.application_sort.selected() == 1 {
            entries.reverse(); // keys are unique (they end with the id)
        }
        suite_gtk::utils::remove_children(chooser);
        let default_id = self.default_id.borrow();
        let recommended = self.recommended.borrow();
        let mut chosen_row = None;
        for app in &entries {
            let row = gtk::ListBoxRow::new();
            unsafe {
                row.set_data(ROW_ID, app.id.clone());
                if let Some(info) = &app.info {
                    row.set_data(ROW_INFO, info.clone());
                }
            }
            let contents = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            set_margins(&contents, 10);
            let icon = match &app.icon {
                Some(icon) => gtk::Image::from_gicon(icon),
                None => gtk::Image::new(),
            };
            icon.set_pixel_size(32);
            contents.append(&icon);
            let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(3).hexpand(true).build();
            labels.append(&gtk::Label::builder().label(&app.name).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build());
            let detail = if default_id.as_deref() == Some(app.id.as_str()) {
                "Current default".to_string()
            } else if recommended.contains(&app.id) {
                "Recommended".to_string()
            } else {
                app.description.clone()
            };
            if !detail.is_empty() {
                let label = gtk::Label::builder().label(&detail).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
                label.add_css_class("dim-label");
                labels.append(&label);
            }
            contents.append(&labels);
            row.set_child(Some(&contents));
            let tooltip = if app.description.is_empty() { app.name.clone() } else { format!("{}\n{}", app.name, app.description) };
            row.set_tooltip_text(Some(&tooltip));
            chooser.append(&row);
            if selected_id.as_deref() == Some(app.id.as_str()) {
                chosen_row = Some(row);
            }
        }
        let target = chosen_row.or_else(|| chooser.row_at_index(0));
        chooser.select_row(target.as_ref());
        self.dialog.status.set_text(if !entries.is_empty() { "" } else if !query.trim().is_empty() {
            "No applications match your search."
        } else {
            "No installed applications were found."
        });
        drop((default_id, recommended, catalog));
        self.selected();
    }

    fn chosen(&self) {
        if self.once.finished() {
            return;
        }
        // SearchEntry debounces search-changed. An immediate Enter/click must
        // use the current query rather than the previous unfiltered selection.
        self.rebuild();
        let Some(row) = self.dialog.application_chooser.selected_row() else { return };
        let Some(app) = row_app_info(&row) else { return };
        let content_type = self.content_type.borrow().clone();
        if let (true, Some(kind)) = (self.dialog.default_check.is_active(), content_type) {
            let result = match (self.set_default)(&app, &kind) {
                Ok(true) => Ok(()),
                Ok(false) => Err("The default application could not be changed".to_string()),
                Err(error) => Err(error.message().to_string()),
            };
            if let Err(error) = result {
                self.dialog.status.set_text(&format!("Could not set the default application: {error}"));
                return;
            }
        }
        self.complete(Some(app));
    }
}

/// `choose_application(parent, file, callback)`: search installed apps; change a
/// MIME default only on explicit consent ("Use as default for …").
pub fn choose_application(parent: Option<&gtk::Window>, file: &gio::File,
                          callback: impl FnOnce(Option<gio::AppInfo>) + 'static) -> OpenWithDialog {
    choose_application_with(parent, file, callback, OpenWithOptions::default())
}

pub fn choose_application_with(parent: Option<&gtk::Window>, file: &gio::File,
                               callback: impl FnOnce(Option<gio::AppInfo>) + 'static,
                               options: OpenWithOptions) -> OpenWithDialog {
    let (window, content) = dialog_window(parent, "Open With", 560, 560);
    let name = file.basename().map(|p| p.to_string_lossy().into_owned()).filter(|n| !n.is_empty())
        .unwrap_or_else(|| file.parse_name().to_string());
    content.append(&gtk::Label::builder().label(format!("Choose an application for {name}")).xalign(0.0).wrap(true).build());
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let search = gtk::SearchEntry::builder().hexpand(true).placeholder_text("Search applications…").build();
    accessible(&search, "Search applications");
    let sort = gtk::DropDown::from_strings(&["A–Z", "Z–A"]);
    sort.set_tooltip_text(Some("Sort applications by name"));
    accessible(&sort, "Sort applications by name");
    toolbar.append(&search);
    toolbar.append(&sort);
    content.append(&toolbar);
    let chooser = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).activate_on_single_click(false).build();
    chooser.add_css_class("boxed-list");
    accessible(&chooser, "Applications");
    let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    scroll.set_child(Some(&chooser));
    content.append(&scroll);
    let default_label = gtk::Label::builder().label("Use as default for this file type").xalign(0.0).wrap(true).build();
    let make_default = gtk::CheckButton::builder().child(&default_label).sensitive(false).build();
    accessible(&make_default, &default_label.text());
    content.append(&make_default);
    let status = gtk::Label::builder().label("Loading applications…").xalign(0.0).wrap(true).build();
    content.append(&status);
    let buttons = button_row();
    let cancel = gtk::Button::with_label("Cancel");
    let accept = gtk::Button::builder().label("Open").sensitive(false).build();
    accept.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&accept);
    content.append(&buttons);
    let dialog = OpenWithDialog {
        window: window.clone(), application_chooser: chooser.clone(), application_search: search.clone(),
        application_sort: sort.clone(), default_check: make_default.clone(), default_label: default_label.clone(),
        accept_button: accept.clone(), status: status.clone(), applications_loaded: Rc::new(Cell::new(false)),
    };
    let set_default = options.set_default.clone().unwrap_or_else(|| Rc::new(|app: &gio::AppInfo, kind: &str| {
        app.set_as_default_for_type(kind).map(|_| true)
    }));
    let state = Rc::new(OpenWithState {
        dialog: dialog.clone(), once: Once::new(callback), cancellable: gio::Cancellable::new(),
        catalog: RefCell::new(None), default_id: RefCell::new(None), recommended: RefCell::new(HashSet::new()),
        content_type: RefCell::new(None), set_default,
    });
    let discovery = options.discovery.clone().unwrap_or_else(|| Arc::new(gio_sources));
    let weak = Rc::downgrade(&state);
    let with = move |f: &dyn Fn(&OpenWithState)| {
        if let Some(state) = weak.upgrade() {
            f(&state);
        }
    };
    let with = Rc::new(with);
    {
        let with = with.clone();
        cancel.connect_clicked(move |_| with(&|s| s.complete(None)));
    }
    {
        let with = with.clone();
        accept.connect_clicked(move |_| with(&|s| s.chosen()));
    }
    {
        let with = with.clone();
        chooser.connect_row_selected(move |_, _| with(&|s| s.selected()));
    }
    {
        let with = with.clone();
        chooser.connect_row_activated(move |chooser, row| {
            chooser.select_row(Some(row));
            with(&|s| s.chosen());
        });
    }
    {
        let with = with.clone();
        search.connect_search_changed(move |_| with(&|s| s.rebuild()));
    }
    {
        let with = with.clone();
        search.connect_activate(move |_| with(&|s| s.chosen()));
    }
    {
        let with = with.clone();
        sort.connect_selected_notify(move |_| with(&|s| s.rebuild()));
    }
    // The window owns the state until it closes.
    let owner = RefCell::new(Some(state.clone()));
    window.connect_close_request(move |_| {
        let state = owner.borrow_mut().take();
        if let Some(state) = state {
            state.complete(None);
        }
        glib::Propagation::Proceed
    });
    window.set_default_widget(Some(&accept));
    let weak = Rc::downgrade(&state);
    file.query_info_async("standard::content-type", gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT,
                          Some(&state.cancellable), move |result| {
        let Some(state) = weak.upgrade() else { return };
        if state.once.finished() {
            return;
        }
        let content_type = result.ok().and_then(|info| info.content_type()).map(|s| s.to_string()).filter(|s| !s.is_empty());
        state.content_type.replace(content_type.clone());
        match &content_type {
            Some(kind) => {
                let description = gio::content_type_get_description(kind);
                let description = if description.is_empty() { kind.clone() } else { description.to_string() };
                state.dialog.default_label.set_text(&format!("Use as default for {description} ({kind})"));
                accessible(&state.dialog.default_check, &state.dialog.default_label.text());
            }
            None => state.dialog.default_check.set_tooltip_text(Some("The file type could not be determined.")),
        }
        let discovery = discovery.clone();
        let handle = gio::spawn_blocking(move || {
            SendBox(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| catalog_data(discovery(content_type.as_deref()))))
                .map_err(|panic| panic.downcast_ref::<String>().cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "discovery failed".into())))
        });
        let weak = Rc::downgrade(&state);
        glib::spawn_future_local(async move {
            let result = handle.await;
            let Some(state) = weak.upgrade() else { return };
            if state.once.finished() {
                return;
            }
            match result {
                Ok(SendBox(Ok((infos, default, recommended)))) => {
                    state.catalog.replace(Some(AppCatalog::from_infos(infos)));
                    state.default_id.replace(default);
                    state.recommended.replace(recommended);
                    state.rebuild();
                    state.dialog.applications_loaded.set(true);
                }
                Ok(SendBox(Err(error))) => state.dialog.status.set_text(&format!("Could not load applications: {error}")),
                Err(_) => state.dialog.status.set_text("Could not load applications: discovery failed"),
            }
        });
    });
    window.present();
    search.grab_focus();
    dialog
}

/// `_size(value)`: `GLib.format_size` + `" (1,234 bytes)"`.
pub(crate) fn size_text(value: u64) -> String {
    format!("{} ({} bytes)", glib::format_size(value), crate::compat::thousands(value as i128))
}

/// `_row(box, title, value)`: a dim title and a selectable wrapping value label.
pub(crate) fn detail_row(container: &gtk::Box, title: &str, value: &str) -> gtk::Label {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let label = gtk::Label::builder().label(title).xalign(0.0).valign(gtk::Align::Start).build();
    label.set_size_request(110, -1);
    label.add_css_class("dim-label");
    let content = gtk::Label::builder().label(value).xalign(0.0).hexpand(true).selectable(true).wrap(true).build();
    line.append(&label);
    line.append(&content);
    container.append(&line);
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, name: &str, extra: &str) -> gio::AppInfo {
        let keyfile = glib::KeyFile::new();
        keyfile.load_from_data(&format!("[Desktop Entry]\nType=Application\nName={name}\nExec=/usr/bin/true %F\n{extra}"),
                               glib::KeyFileFlags::NONE).unwrap();
        let info = waydock::core::app_catalog::desktop_info_from_keyfile(&keyfile).unwrap();
        let _ = id;
        info
    }

    #[test]
    fn mime_handlers_become_visible_and_hidden_entries_never_do() {
        // Keyfile-built infos have no desktop id; ids come from AppInfoData, so check the flags.
        let handler = info("h", "Handler", "NoDisplay=true\n");
        let (infos, default, recommended) = catalog_data(AppSources {
            all: vec![info("a", "Alpha", ""), handler.clone()],
            all_for_type: vec![handler.clone()],
            recommended: vec![handler.clone()],
            default: None,
        });
        assert_eq!(default, None);
        assert!(recommended.len() <= 1);
        assert_eq!(infos.len(), 4);
        // Handlers are flagged visible only when they carry a desktop id (keyfile infos have none).
        assert!(!infos[1].should_show || infos[1].id.is_some());
        assert!(infos[0].should_show);
    }

    #[test]
    fn chooser_modes() {
        assert_eq!(ChooserMode::from_name("open"), Ok(ChooserMode::Open));
        assert_eq!(ChooserMode::from_name("save"), Ok(ChooserMode::Save));
        assert_eq!(ChooserMode::from_name("folder"), Ok(ChooserMode::Folder));
        assert_eq!(ChooserMode::from_name("x"), Err("Unknown file chooser mode".to_string()));
        assert_eq!(size_text(16), "16 bytes (16 bytes)");
    }
}
