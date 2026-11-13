//! Port of picture_ui.py: browsing, viewing and editing interface for Pictures.
#![allow(deprecated)] // Gtk.ColorButton, as in the Python version.

use crate::canvas::{PictureCanvas, Zoom};
use crate::compat::{Pool, Ticket};
use crate::dialogs::{ask, compare, form, number};
use crate::model::{self, Annotation, AnnotationKind, Entry, Operation, PictureDocument, Signature};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tessel_core::tr;
use tessel_gtk::app_menu::{self, AboutDetails, MenuTarget, Shortcut, ShortcutSection};
use tessel_gtk::theme::{self, WindowTheme};
use tessel_gtk::utils::{accessible, add_css, headerbar, image, remove_css, set_margins, sidebar_style, PRIORITY_APPLICATION};

pub const STYLE: &str = "
.pictures-window .toolbar { padding: 4px 10px; border-spacing: 5px; background: transparent; border-top: 1px solid alpha(@theme_fg_color,.10); }
.pictures-window .toolbar button { background: transparent; border-color: transparent; box-shadow: none; }
.pictures-window .toolbar button:hover { background: alpha(@theme_fg_color,.09); }
.pictures-window .toolbar button:checked { background: alpha(@theme_selected_bg_color,.3); }
.pictures-window .pictures-sidebar { padding: 12px 8px; border-right: 1px solid alpha(@theme_fg_color,.12); }
.pictures-window .section-title { font-size: 10pt; font-weight: 600; opacity: .7; margin-top: 8px; padding: 0 10px; }
.pictures-window .pictures-sidebar .picture-info { padding: 0 10px; }
.pictures-window .muted { opacity: .7; }
.pictures-window .thumbnail { padding: 7px; border-radius: 8px; }
.pictures-window gridview child:selected, .pictures-window listview row:selected {
 background: alpha(@theme_selected_bg_color,.28); border-radius: 8px; }
.pictures-window .status { padding: 6px 12px; }
.pictures-window .edit-bar { padding: 6px 12px; background: alpha(@theme_selected_bg_color,.15); }
.pictures-window .tools { padding: 12px; border-left: 1px solid alpha(@theme_fg_color,.12); }
.pictures-window .tools button.edit-tool {
 background: transparent; background-image: none; box-shadow: none;
 border: 1px solid transparent; border-radius: 6px; padding: 7px 9px;
 color: @theme_fg_color; }
.pictures-window .tools button.edit-tool:hover,
.pictures-window .tools button.edit-tool:focus-visible {
 background: alpha(@theme_fg_color,.06); border-color: alpha(@theme_fg_color,.25); }
.pictures-window .tools button.edit-tool:checked {
 background: alpha(@theme_selected_bg_color,.12); border-color: alpha(@theme_selected_bg_color,.65); }
";

pub const ANNOTATION_TOOLS: [&str; 10] = ["", "pan", "red-eye", "arrow", "line", "rectangle", "ellipse", "pen", "text", "stamp"];

pub fn shortcuts() -> Vec<ShortcutSection> {
    vec![
        ("Files".into(), vec![Shortcut::new("<Control>o", "Open pictures"), Shortcut::new("<Control><Shift>o", "Open folder"),
            Shortcut::new("<Alt>Up", "Parent folder"), Shortcut::new("Return", "Browse folder"), Shortcut::new("<Control>s", "Save"),
            Shortcut::new("<Control><Shift>s", "Save As…"), Shortcut::new("<Control>c", "Copy Image"), Shortcut::new("<Control>p", "Print…"),
            Shortcut::new("F2", "Rename…"), Shortcut::new("Delete", "Move to Trash…")]),
        ("Viewing".into(), vec![Shortcut::any(&["Right", "Page_Down", "space"], "Next picture"),
            Shortcut::any(&["Left", "Page_Up", "BackSpace"], "Previous picture"), Shortcut::new("Home", "First picture"),
            Shortcut::new("End", "Last picture"), Shortcut::new("<Alt>Left", "Previous picture while zoomed"),
            Shortcut::new("<Alt>Right", "Next picture while zoomed"), Shortcut::any(&["<Control>plus", "plus"], "Zoom in"),
            Shortcut::any(&["<Control>minus", "minus"], "Zoom out"), Shortcut::new("<Control>0", "Best fit"),
            Shortcut::new("f", "Toggle fit / 100%"), Shortcut::new("<Shift>f", "Fill window"),
            Shortcut::new("Ctrl+Scroll", "Zoom with the mouse wheel"), Shortcut::new("F11", "Fullscreen"),
            Shortcut::new("F5", "Start or stop slideshow"), Shortcut::new("Escape", "Leave fullscreen or stop slideshow")]),
        ("Editing".into(), vec![Shortcut::new("<Control>z", "Undo"), Shortcut::any(&["<Control><Shift>z", "<Control>y"], "Redo"),
            Shortcut::new("<Control>r", "Rotate right"), Shortcut::new("<Control><Shift>r", "Rotate left"),
            Shortcut::new("<Shift>c", "Crop"), Shortcut::new("<Shift>r", "Resize…")]),
    ]
}

/// What the UI shows about the open document (cached so the UI thread never waits on a worker).
#[derive(Clone, Debug)]
pub struct DocInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub comment: String,
    pub original_signature: Signature,
}

pub type SharedDoc = Arc<Mutex<PictureDocument>>;
pub type Action = Rc<dyn Fn(&PicturesWindow)>;

pub struct ItemWidgets {
    pub picture: gtk::Picture,
    pub fallback: gtk::Image,
    pub stack: gtk::Stack,
    pub label: gtk::Label,
    pub job: RefCell<Option<(Ticket, u64)>>,
}

pub struct Widgets {
    pub window: gtk::ApplicationWindow,
    pub header: gtk::HeaderBar,
    pub toolbar: gtk::HeaderBar,
    pub caption: gtk::Label,
    pub browse_button: gtk::Button,
    pub previous: gtk::Button,
    pub next: gtk::Button,
    pub undo_button: gtk::Button,
    pub redo_button: gtk::Button,
    pub slide_button: gtk::Button,
    pub menu_button: gtk::MenuButton,
    pub edit_bar: gtk::Box,
    pub sidebar: gtk::ScrolledWindow,
    pub info: gtk::Label,
    pub comment_button: gtk::Button,
    pub stack: gtk::Stack,
    pub search: gtk::SearchEntry,
    pub sort: gtk::DropDown,
    pub thumbnail_size: gtk::DropDown,
    pub location: gtk::Entry,
    pub store: gio::ListStore,
    pub selection: gtk::MultiSelection,
    pub grid: gtk::GridView,
    pub view_stack: gtk::Stack,
    pub image_scroll: gtk::ScrolledWindow,
    pub canvas: PictureCanvas,
    pub video: gtk::Video,
    pub zoom_bar: gtk::Box,
    pub animation_button: gtk::Button,
    pub selection_label: gtk::Label,
    pub thumb_store: gio::ListStore,
    pub thumb_selection: gtk::SingleSelection,
    pub strip: gtk::ListView,
    pub thumb_scroll: gtk::ScrolledWindow,
    pub tools: gtk::ScrolledWindow,
    pub crop_button: gtk::ToggleButton,
    pub tool: gtk::DropDown,
    pub crop_ratio: gtk::DropDown,
    pub selection_options: gtk::Box,
    pub coordinates: [gtk::SpinButton; 4],
    pub annotation_options: gtk::Box,
    pub annotation_color: gtk::ColorButton,
    pub annotation_width: gtk::SpinButton,
    pub annotation_text: gtk::Entry,
    pub status_row: gtk::Box,
    pub spinner: gtk::Spinner,
    pub status: gtk::Label,
}

pub struct Inner {
    pub w: Widgets,
    pub closed: Cell<bool>,
    pub busy: Cell<bool>,
    pub doc: RefCell<Option<SharedDoc>>,
    doc_info: RefCell<Option<DocInfo>>,
    pub current_path: RefCell<Option<PathBuf>>,
    pub entries: RefCell<Vec<Entry>>,
    pub folder: RefCell<PathBuf>,
    pub generation: Cell<u64>,
    pub last_trashed: RefCell<Vec<PathBuf>>,
    pub confirm: RefCell<Option<gtk::Window>>,
    pub executor: Pool,
    pub thumbnail_executor: Pool,
    pub thumbnails: RefCell<Vec<((String, u64, u64), gdk::Texture)>>,
    pub thumbnail_pending: RefCell<HashSet<u64>>,
    thumbnail_sequence: Cell<u64>,
    pub context_menu: RefCell<Option<gtk::PopoverMenu>>,
    pub monitor: RefCell<Option<gio::FileMonitor>>,
    pub monitor_timer: RefCell<Option<glib::SourceId>>,
    pub slideshow_timer: RefCell<Option<glib::SourceId>>,
    pub animation_timer: RefCell<Option<glib::SourceId>>,
    pub inhibit_cookie: Cell<u32>,
    pub animation_index: Cell<usize>,
    pub displayed_frame: Cell<usize>,
    pub animation_playing: Cell<bool>,
    pub animation_generation: Cell<u64>,
    fullscreen_controls: RefCell<Option<Vec<(gtk::Widget, bool)>>>,
    pub fullscreen_reveal_timer: RefCell<Option<glib::SourceId>>,
    pub config_path: PathBuf,
    pub preferences: RefCell<Map<String, Value>>,
    pub stamp_path: RefCell<Option<PathBuf>>,
    changing_tool: Cell<bool>,
    pub transfer_results: RefCell<Option<gtk::Window>>,
    css: gtk::CssProvider,
    pub theme: RefCell<Option<WindowTheme>>,
}

#[derive(Clone)]
pub struct PicturesWindow(pub Rc<Inner>);

impl std::ops::Deref for PicturesWindow {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

impl std::ops::Deref for Inner {
    type Target = Widgets;
    fn deref(&self) -> &Widgets { &self.w }
}

/// A closure holding the window weakly: `weak!(self, |w| w.method())` → `Fn()`.
macro_rules! weak {
    ($this:expr, |$w:ident| $body:expr) => {{
        let weak = Rc::downgrade(&$this.0);
        move || {
            if let Some(inner) = weak.upgrade() {
                let $w = PicturesWindow(inner);
                $body;
            }
        }
    }};
}


pub fn entry_of(object: &glib::Object) -> Option<Entry> {
    object.downcast_ref::<glib::BoxedAnyObject>().map(|o| o.borrow::<Entry>().clone())
}

fn item(entry: &Entry) -> glib::BoxedAnyObject { glib::BoxedAnyObject::new(entry.clone()) }

/// Python `int(value)` for a preference, with a default.
pub fn pref_int(preferences: &Map<String, Value>, key: &str, default: i64) -> i64 {
    match preferences.get(key) {
        None => default,
        Some(value) => tessel_core::pyjson::py_int(Some(value)).unwrap_or(default),
    }
}

pub fn pref_bool(preferences: &Map<String, Value>, key: &str, default: bool) -> bool {
    preferences.get(key).map(tessel_core::pyjson::truthy).unwrap_or(default)
}

/// `~/.config/tessel/pictures/settings.json`.
pub fn config_path() -> PathBuf {
    tessel_core::paths::component_config_dir("pictures").join("settings.json")
}

fn button(container: Option<&gtk::Box>, icon: &str, tooltip: &str, callback: impl Fn() + 'static) -> gtk::Button {
    let widget = gtk::Button::builder().child(&image(icon, 16)).tooltip_text(tooltip).build();
    accessible(&widget, tooltip);
    widget.connect_clicked(move |_| callback());
    if let Some(container) = container {
        container.append(&widget);
    }
    widget
}

fn edit_tool_button(container: &gtk::Box, icon: &str, title: &str, toggle: bool) -> gtk::Button {
    let button: gtk::Button = if toggle { gtk::ToggleButton::new().upcast() } else { gtk::Button::new() };
    button.add_css_class("edit-tool");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.append(&image(icon, 20));
    content.append(&gtk::Label::builder().label(title).xalign(0.0).hexpand(true).build());
    button.set_child(Some(&content));
    button.set_tooltip_text(Some(title));
    accessible(&button, title);
    container.append(&button);
    button
}

impl PicturesWindow {
    pub fn new(app: &gtk::Application) -> Self {
        let config_path = config_path();
        let preferences = std::fs::read_to_string(&config_path).ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value.as_object().cloned()).unwrap_or_default();
        let pictures = glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(|| tessel_core::paths::home().join("Pictures"));
        let folder = if pictures.is_dir() { pictures } else { tessel_core::paths::home() };
        let window = gtk::ApplicationWindow::builder().application(app).title(tr!("Pictures")).build();
        window.add_css_class("pictures-window");
        window.set_default_size(1180, 800);
        window.set_size_request(740, 460);
        // Above the palette stylesheet (APPLICATION + 30) so the chrome rules win.
        let css = add_css(STYLE, Some(&WidgetExt::display(&window)), PRIORITY_APPLICATION + 31);
        let header = headerbar(&tr!("Pictures"));
        window.set_titlebar(Some(&header));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.set_child(Some(&root));
        let placeholder = || {};
        // The header bar is the only toolbar: navigation at the start, the
        // picture name as its title, and document tools before the menu.
        let open = button(None, "document-open-symbolic", &tr!("Open pictures (Ctrl+O)"), placeholder);
        let open_folder = button(None, "folder-open-symbolic", &tr!("Open folder (Ctrl+Shift+O)"), placeholder);
        let parent = button(None, "go-up-symbolic", &tr!("Parent folder (Alt+Up)"), placeholder);
        let browse_button = button(None, "view-grid-symbolic", &tr!("Browse folder (Enter)"), placeholder);
        let previous = button(None, "go-previous-symbolic", &tr!("Previous picture (Left)"), placeholder);
        let next = button(None, "go-next-symbolic", &tr!("Next picture (Right)"), placeholder);
        for widget in [&open, &open_folder, &parent, &browse_button, &previous, &next] {
            header.pack_start(widget);
        }
        let caption = gtk::Label::builder().label(tr!("Pictures")).ellipsize(pango::EllipsizeMode::Middle).max_width_chars(40).build();
        caption.add_css_class("title");
        header.set_title_widget(Some(&caption));
        let save = button(None, "document-save-symbolic", &tr!("Save (Ctrl+S)"), placeholder);
        let undo_button = button(None, "edit-undo-symbolic", &tr!("Undo (Ctrl+Z)"), placeholder);
        let redo_button = button(None, "edit-redo-symbolic", &tr!("Redo (Ctrl+Shift+Z)"), placeholder);
        let edit_tools = button(None, "document-edit-symbolic", &tr!("Editing tools"), placeholder);
        let slide_button = button(None, "media-playback-start-symbolic", &tr!("Start or stop slideshow (F5)"), placeholder);
        let fullscreen = button(None, "view-fullscreen-symbolic", &tr!("Fullscreen (F11)"), placeholder);
        let end = [&save, &undo_button, &redo_button, &edit_tools, &slide_button, &fullscreen];
        let menu = gio::Menu::new();
        const MENU: [(&str, &str); 15] = [
            ("Save As…", "save-as"), ("Copy Image", "copy-image"), ("Compare Selected Pictures", "compare"), ("Open With…", "open-with"),
            ("Open Containing Folder", "folder"), ("Rename…", "rename"), ("Copy To…", "copy"), ("Move To…", "move"),
            ("Move to Trash…", "trash"), ("Restore Last Trashed Pictures", "restore-trash"), ("Print…", "print"),
            ("Information Sidebar", "sidebar"), ("Thumbnail Strip", "thumbnails"), ("Set as Default Image Viewer", "set-default"),
            ("Settings…", "preferences")];
        for (label, name) in MENU {
            menu.append(Some(&tr!(label)), Some(&format!("win.{name}")));
        }
        // The suite's single primary menu sits at the end of the header bar.
        let menu_button = app_menu::install_app_menu(app, &window, MenuTarget::Model(menu), AboutDetails {
            name: "Pictures".into(), icon: "image-x-generic".into(), version: None,
            comments: Some("Browse, compare and edit images".into()), website: None,
        }, shortcuts(), Some(&header), None);
        for widget in end.iter().rev() {
            header.pack_end(*widget);
        }
        let edit_bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        edit_bar.add_css_class("edit-bar");
        edit_bar.append(&gtk::Label::builder().label(tr!("Unsaved changes")).hexpand(true).xalign(0.0).build());
        let save_changes = button(Some(&edit_bar), "document-save-symbolic", &tr!("Save changes"), placeholder);
        let save_copy = button(Some(&edit_bar), "document-save-as-symbolic", &tr!("Save a copy"), placeholder);
        let reload = button(Some(&edit_bar), "document-revert-symbolic", &tr!("Reload original"), placeholder);
        edit_bar.set_visible(false);
        root.append(&edit_bar);
        let body = gtk::Box::builder().vexpand(true).build();
        root.append(&body);
        let sidebar = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).min_content_width(200)
            .max_content_width(240).build();
        let side = gtk::Box::new(gtk::Orientation::Vertical, 8);
        // Not GTK's "sidebar" class: Adwaita paints that grey.
        side.add_css_class("pictures-sidebar");
        sidebar_style(&side);
        sidebar.set_child(Some(&side));
        body.append(&sidebar);
        let section = |text: &str| {
            let label = gtk::Label::builder().label(text).xalign(0.0).build();
            label.add_css_class("section-title");
            label
        };
        side.append(&section(&tr!("Places")));
        let downloads = glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(tessel_core::paths::home);
        let mut places = Vec::new();
        for (title, path, icon) in [(tr!("Pictures"), folder.clone(), "folder-pictures-symbolic"),
                                    (tr!("Home"), tessel_core::paths::home(), "user-home-symbolic"),
                                    (tr!("Downloads"), downloads, "folder-download-symbolic")] {
            let row = gtk::Button::new();
            row.add_css_class("profile-row");
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.append(&image(icon, 16));
            content.append(&gtk::Label::builder().label(&title).xalign(0.0).build());
            row.set_child(Some(&content));
            accessible(&row, &title);
            side.append(&row);
            places.push((row, path));
        }
        side.append(&section(&tr!("Information")));
        let info = gtk::Label::builder().xalign(0.0).yalign(0.0).selectable(true).wrap(true).max_width_chars(24).build();
        info.add_css_class("picture-info");
        side.append(&info);
        let comment_button = gtk::Button::with_label(&tr!("Edit description…"));
        side.append(&comment_button);
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        body.append(&stack);
        let browser = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        set_margins(&controls, 8);
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search pictures")).hexpand(true).build();
        accessible(&search, &tr!("Search pictures"));
        controls.append(&search);
        let sort = gtk::DropDown::from_strings(&[&tr!("Name A–Z"), &tr!("Name Z–A"), &tr!("Newest first"), &tr!("Oldest first"),
                                                  &tr!("Largest first")]);
        accessible(&sort, &tr!("Sort order"));
        controls.append(&sort);
        let thumbnail_size = gtk::DropDown::from_strings(&[&tr!("Small thumbnails"), &tr!("Medium thumbnails"), &tr!("Large thumbnails")]);
        accessible(&thumbnail_size, &tr!("Thumbnail size"));
        thumbnail_size.set_selected(pref_int(&preferences, "thumbnail_size", 1).clamp(0, 2) as u32);
        controls.append(&thumbnail_size);
        browser.append(&controls);
        let location = gtk::Entry::builder().text(folder.to_string_lossy()).placeholder_text(tr!("Folder path")).build();
        accessible(&location, &tr!("Folder path"));
        browser.append(&location);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let grid = gtk::GridView::builder().model(&selection).min_columns(1).max_columns(12).build();
        let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        scroll.set_child(Some(&grid));
        browser.append(&scroll);
        stack.add_named(&browser, Some("browse"));
        let viewer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let view_stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        viewer.append(&view_stack);
        let image_scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        let canvas = PictureCanvas::new(&image_scroll);
        image_scroll.set_child(Some(&canvas.area));
        view_stack.add_named(&image_scroll, Some("image"));
        let video = gtk::Video::builder().autoplay(false).loop_(false).hexpand(true).vexpand(true).build();
        // A large video in a small window is drawn far below its size; see plain_scale.
        view_stack.add_named(&tessel_gtk::plain_scale::PlainScaleBin::new(&video), Some("video"));
        let zoom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        zoom_bar.add_css_class("toolbar");
        viewer.append(&zoom_bar);
        let mut zoom_buttons = Vec::new();
        for (label, value) in [(tr!("Fit"), "fit"), (tr!("Fill"), "fill"), ("100%".to_string(), "actual")] {
            let button = gtk::Button::with_label(&label);
            zoom_bar.append(&button);
            zoom_buttons.push((button, value));
        }
        let zoom_out = button(Some(&zoom_bar), "zoom-out-symbolic", &tr!("Zoom out (Ctrl+−)"), placeholder);
        let zoom_in = button(Some(&zoom_bar), "zoom-in-symbolic", &tr!("Zoom in (Ctrl++)"), placeholder);
        let animation_button = gtk::Button::with_label(&tr!("Play animation"));
        zoom_bar.append(&animation_button);
        let selection_label = gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(pango::EllipsizeMode::End).build();
        zoom_bar.append(&selection_label);
        let thumb_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let thumb_selection = gtk::SingleSelection::new(Some(thumb_store.clone()));
        let strip = gtk::ListView::builder().model(&thumb_selection).single_click_activate(true).build();
        strip.set_orientation(gtk::Orientation::Horizontal);
        let thumb_scroll = gtk::ScrolledWindow::builder().height_request(115).vscrollbar_policy(gtk::PolicyType::Never).build();
        thumb_scroll.set_child(Some(&strip));
        viewer.append(&thumb_scroll);
        stack.add_named(&viewer, Some("view"));

        // build_tools
        let tools = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).width_request(245).build();
        let tools_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        tools_box.add_css_class("tools");
        tools.set_child(Some(&tools_box));
        tools_box.append(&section(&tr!("Edit Picture")));
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 2);
        tools_box.append(&actions);
        let mut operation_buttons: Vec<(gtk::Button, &'static str)> = Vec::new();
        for (title, icon, op) in [(tr!("Rotate left"), "object-rotate-left-symbolic", "left"),
                                  (tr!("Rotate right"), "object-rotate-right-symbolic", "right"),
                                  (tr!("Mirror"), "object-flip-horizontal-symbolic", "mirror"),
                                  (tr!("Flip"), "object-flip-vertical-symbolic", "flip")] {
            operation_buttons.push((edit_tool_button(&actions, icon, &title, false), op));
        }
        operation_buttons.push((edit_tool_button(&actions, "image-resize-symbolic", &tr!("Resize…"), false), "resize"));
        let crop_button = edit_tool_button(&actions, "image-crop", &tr!("Crop"), true).downcast::<gtk::ToggleButton>().unwrap();
        for (title, icon, op) in [(tr!("Brightness / Color…"), "color-select-symbolic", "adjust"),
                                  (tr!("Auto contrast"), "contrast", "autocontrast"), (tr!("Grayscale"), "image-filter", "grayscale")] {
            operation_buttons.push((edit_tool_button(&actions, icon, &title, false), op));
        }
        tools_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        tools_box.append(&gtk::Label::builder().label(tr!("Selection / Annotation")).xalign(0.0).build());
        let tool_names: Vec<String> = ["Choose a tool…", "Pan", "Reduce red eye", "Arrow", "Line", "Rectangle", "Ellipse", "Freehand",
                                       "Text", "Image stamp"].iter().map(|n| tr!(n)).collect();
        let tool = gtk::DropDown::from_strings(&tool_names.iter().map(String::as_str).collect::<Vec<_>>());
        accessible(&tool, &tr!("Selection or annotation tool"));
        tools_box.append(&tool);
        let crop_ratio = gtk::DropDown::from_strings(&[&tr!("Free aspect ratio"), &tr!("Original ratio"), "1:1", "4:3", "3:2", "16:9"]);
        accessible(&crop_ratio, &tr!("Crop aspect ratio"));
        let selection_options = gtk::Box::new(gtk::Orientation::Vertical, 8);
        tools_box.append(&selection_options);
        selection_options.append(&crop_ratio);
        let coordinates = [number(0.0, 0.0, 100000.0, 1.0), number(0.0, 0.0, 100000.0, 1.0), number(1.0, 1.0, 100000.0, 1.0),
                           number(1.0, 1.0, 100000.0, 1.0)];
        for (label, widget) in ["X".to_string(), "Y".to_string(), tr!("Width"), tr!("Height")].iter().zip(&coordinates) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            accessible(widget, label);
            row.append(&gtk::Label::builder().label(label).xalign(0.0).hexpand(true).build());
            row.append(widget);
            selection_options.append(&row);
        }
        let apply_selection = edit_tool_button(&selection_options, "object-select-symbolic", &tr!("Apply selection"), false);
        selection_options.set_visible(false);
        let annotation_options = gtk::Box::new(gtk::Orientation::Vertical, 8);
        tools_box.append(&annotation_options);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Label::builder().label(tr!("Color")).hexpand(true).xalign(0.0).build());
        let annotation_color = gtk::ColorButton::new();
        accessible(&annotation_color, &tr!("Annotation color"));
        annotation_color.set_rgba(&gdk::RGBA::parse("#ef4f5f").unwrap());
        row.append(&annotation_color);
        annotation_options.append(&row);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Label::builder().label(tr!("Line / text size")).hexpand(true).xalign(0.0).build());
        let annotation_width = number(4.0, 1.0, 100.0, 1.0);
        accessible(&annotation_width, &tr!("Line / text size"));
        row.append(&annotation_width);
        annotation_options.append(&row);
        let annotation_text = gtk::Entry::builder().placeholder_text(tr!("Annotation text")).build();
        accessible(&annotation_text, &tr!("Annotation text"));
        annotation_options.append(&annotation_text);
        let choose_stamp = edit_tool_button(&annotation_options, "insert-image-symbolic", &tr!("Choose image stamp…"), false);
        annotation_options.set_visible(false);
        let hint = gtk::Label::builder()
            .label(tr!("Drag on the picture to crop, mark an eye, or draw. Click to place text. Every edit can be undone."))
            .wrap(true).xalign(0.0).max_width_chars(28).build();
        hint.add_css_class("muted");
        tools_box.append(&hint);
        tools.set_visible(false);
        body.append(&tools);

        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_row.add_css_class("status");
        let spinner = gtk::Spinner::new();
        let status = gtk::Label::builder().label(tr!("Open a picture or browse a folder.")).xalign(0.0).hexpand(true)
            .ellipsize(pango::EllipsizeMode::End).build();
        status_row.append(&spinner);
        status_row.append(&status);
        root.append(&status_row);

        let this = Self(Rc::new(Inner {
            w: Widgets {
                window: window.clone(), toolbar: header.clone(), header, caption, browse_button, previous, next, undo_button, redo_button,
                slide_button, menu_button, edit_bar, sidebar, info, comment_button, stack, search, sort, thumbnail_size, location, store,
                selection, grid, view_stack, image_scroll, canvas, video, zoom_bar, animation_button, selection_label, thumb_store,
                thumb_selection, strip, thumb_scroll, tools, crop_button, tool, crop_ratio, selection_options, coordinates,
                annotation_options, annotation_color, annotation_width, annotation_text, status_row, spinner, status,
            },
            closed: Cell::new(false), busy: Cell::new(false), doc: RefCell::new(None), doc_info: RefCell::new(None),
            current_path: RefCell::new(None), entries: RefCell::new(Vec::new()), folder: RefCell::new(folder),
            generation: Cell::new(0), last_trashed: RefCell::new(Vec::new()), confirm: RefCell::new(None),
            executor: Pool::new("pictures", 1), thumbnail_executor: Pool::new("picture-thumbnails", 2),
            thumbnails: RefCell::new(Vec::new()), thumbnail_pending: RefCell::new(HashSet::new()), thumbnail_sequence: Cell::new(0),
            context_menu: RefCell::new(None), monitor: RefCell::new(None), monitor_timer: RefCell::new(None),
            slideshow_timer: RefCell::new(None), animation_timer: RefCell::new(None), inhibit_cookie: Cell::new(0),
            animation_index: Cell::new(0), displayed_frame: Cell::new(0), animation_playing: Cell::new(false),
            animation_generation: Cell::new(0), fullscreen_controls: RefCell::new(None), fullscreen_reveal_timer: RefCell::new(None),
            config_path, preferences: RefCell::new(preferences), stamp_path: RefCell::new(None), changing_tool: Cell::new(false),
            transfer_results: RefCell::new(None), css, theme: RefCell::new(None),
        }));
        this.theme.replace(Some(theme::attach(&window, None)));
        unsafe { window.set_data("pictures-window", this.clone()) };

        // Signal wiring (all handlers hold the window weakly).
        open.connect_clicked({ let f = weak!(this, |w| w.choose_open(false)); move |_| f() });
        open_folder.connect_clicked({ let f = weak!(this, |w| w.choose_open(true)); move |_| f() });
        parent.connect_clicked({ let f = weak!(this, |w| { let p = w.folder_parent(); w.browse_folder(&p, false) }); move |_| f() });
        this.browse_button.connect_clicked({ let f = weak!(this, |w| { let p = w.folder.borrow().clone(); w.browse_folder(&p, false) }); move |_| f() });
        this.previous.connect_clicked({ let f = weak!(this, |w| w.navigate(-1)); move |_| f() });
        this.next.connect_clicked({ let f = weak!(this, |w| w.navigate(1)); move |_| f() });
        save.connect_clicked({ let f = weak!(this, |w| w.save(false, None)); move |_| f() });
        this.undo_button.connect_clicked({ let f = weak!(this, |w| w.undo(false)); move |_| f() });
        this.redo_button.connect_clicked({ let f = weak!(this, |w| w.undo(true)); move |_| f() });
        edit_tools.connect_clicked({ let f = weak!(this, |w| w.tools.set_visible(!w.tools.get_visible())); move |_| f() });
        this.slide_button.connect_clicked({ let f = weak!(this, |w| w.toggle_slideshow()); move |_| f() });
        fullscreen.connect_clicked({ let f = weak!(this, |w| w.toggle_fullscreen()); move |_| f() });
        save_changes.connect_clicked({ let f = weak!(this, |w| w.save(false, None)); move |_| f() });
        save_copy.connect_clicked({ let f = weak!(this, |w| w.save(true, None)); move |_| f() });
        reload.connect_clicked({ let f = weak!(this, |w| w.reload_original()); move |_| f() });
        for (row, path) in places {
            row.connect_clicked({ let f = weak!(this, |w| w.browse_folder(&path, false)); move |_| f() });
        }
        this.comment_button.connect_clicked({ let f = weak!(this, |w| w.edit_comment()); move |_| f() });
        this.search.connect_search_changed({ let f = weak!(this, |w| w.filter_entries()); move |_| f() });
        this.sort.connect_selected_notify({ let f = weak!(this, |w| w.refresh_folder()); move |_| f() });
        this.thumbnail_size.connect_selected_notify({ let f = weak!(this, |w| w.resize_thumbnails()); move |_| f() });
        this.location.connect_activate({
            let f = weak!(this, |w| { let p = model_expand(&w.location.text()); w.browse_folder(&p, false) });
            move |_| f()
        });
        this.grid.set_factory(Some(&this.thumbnail_factory(false)));
        this.strip.set_factory(Some(&this.thumbnail_factory(true)));
        let weak = Rc::downgrade(&this.0);
        this.grid.connect_activate(move |_, position| {
            if let Some(w) = weak.upgrade().map(PicturesWindow) {
                if let Some(entry) = w.store.item(position).and_then(|o| entry_of(&o)) {
                    w.open_entry(&entry);
                }
            }
        });
        let weak = Rc::downgrade(&this.0);
        this.strip.connect_activate(move |_, position| {
            if let Some(w) = weak.upgrade().map(PicturesWindow) {
                if let Some(entry) = w.thumb_store.item(position).and_then(|o| entry_of(&o)) {
                    w.open_entry(&entry);
                }
            }
        });
        let canvas_area = this.canvas.area.clone();
        this.add_context_menu(canvas_area.upcast_ref(), None, false);
        this.canvas.connect(
            { let weak = Rc::downgrade(&this.0); Rc::new(move |r| if let Some(i) = weak.upgrade() { PicturesWindow(i).selection_changed(r) }) },
            { let weak = Rc::downgrade(&this.0);
              Rc::new(move |kind: &str, points| if let Some(i) = weak.upgrade() { PicturesWindow(i).annotate(kind, points) }) },
            { let weak = Rc::downgrade(&this.0); Rc::new(move || if let Some(i) = weak.upgrade() { PicturesWindow(i).toggle_fullscreen() }) },
        );
        for (button, value) in zoom_buttons {
            button.connect_clicked({ let f = weak!(this, |w| w.canvas.set_zoom(Zoom::Mode(value))); move |_| f() });
        }
        zoom_out.connect_clicked({ let f = weak!(this, |w| w.canvas.set_zoom(Zoom::Value(w.canvas.scale() / 1.25))); move |_| f() });
        zoom_in.connect_clicked({ let f = weak!(this, |w| w.canvas.set_zoom(Zoom::Value(w.canvas.scale() * 1.25))); move |_| f() });
        this.animation_button.connect_clicked({ let f = weak!(this, |w| w.toggle_animation()); move |_| f() });
        for (button, op) in operation_buttons {
            button.connect_clicked({
                let f = weak!(this, |w| match op {
                    "left" => w.edit(Operation::Left),
                    "right" => w.edit(Operation::Right),
                    "mirror" => w.edit(Operation::Mirror),
                    "flip" => w.edit(Operation::Flip),
                    "resize" => w.resize_picture(),
                    "adjust" => w.adjust_picture(),
                    "autocontrast" => w.edit(Operation::Autocontrast),
                    _ => w.edit(Operation::Grayscale),
                });
                move |_| f()
            });
        }
        this.crop_button.connect_toggled({ let f = weak!(this, |w| w.crop_toggled()); move |_| f() });
        this.tool.connect_selected_notify({ let f = weak!(this, |w| w.tool_changed()); move |_| f() });
        this.crop_ratio.connect_selected_notify({ let f = weak!(this, |w| w.update_ratio()); move |_| f() });
        apply_selection.connect_clicked({ let f = weak!(this, |w| w.apply_selection()); move |_| f() });
        choose_stamp.connect_clicked({ let f = weak!(this, |w| w.choose_stamp()); move |_| f() });

        for (_, name) in MENU {
            let action = gio::SimpleAction::new(name, None);
            let f = weak!(this, |w| w.menu_action(name));
            action.connect_activate(move |_, _| f());
            window.add_action(&action);
        }
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&this.0);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let handled = weak.upgrade().is_some_and(|i| PicturesWindow(i).key_pressed(key, modifiers));
            if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        window.add_controller(keys);
        // Viewing keys run before the window's arrow-key focus movement.
        let navigation = gtk::EventControllerKey::new();
        navigation.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&this.0);
        navigation.connect_key_pressed(move |_, key, _, modifiers| {
            let handled = weak.upgrade().is_some_and(|i| PicturesWindow(i).navigation_key(key, modifiers));
            if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        window.add_controller(navigation);
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(&this.0);
        drop.connect_drop(move |_, value, _, _| {
            if let (Some(w), Ok(files)) = (weak.upgrade().map(PicturesWindow), value.get::<gdk::FileList>()) {
                let paths: Vec<PathBuf> = files.files().iter().filter_map(|f| f.path()).collect();
                w.open_paths(&paths, false);
            }
            true
        });
        window.add_controller(drop);
        let weak = Rc::downgrade(&this.0);
        window.connect_close_request(move |_| match weak.upgrade() {
            Some(inner) => PicturesWindow(inner).close_requested(),
            None => glib::Propagation::Proceed,
        });
        let weak = Rc::downgrade(&this.0);
        window.connect_fullscreened_notify(move |_| if let Some(i) = weak.upgrade() { PicturesWindow(i).fullscreen_changed() });
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&this.0);
        motion.connect_motion(move |_, _, y| if let Some(i) = weak.upgrade() { PicturesWindow(i).fullscreen_motion(y) });
        root.add_controller(motion);
        this.update_actions();
        let weak = Rc::downgrade(&this.0);
        glib::idle_add_local_once(move || {
            if let Some(w) = weak.upgrade().map(PicturesWindow) {
                if !w.busy.get() && w.current_path.borrow().is_none() && w.entries.borrow().is_empty() {
                    let folder = w.folder.borrow().clone();
                    w.browse_folder(&folder, false);
                }
            }
        });
        this
    }

    fn folder_parent(&self) -> PathBuf {
        let folder = self.folder.borrow().clone();
        folder.parent().map(Path::to_path_buf).unwrap_or(folder)
    }

    fn menu_action(&self, name: &str) {
        match name {
            "save-as" => self.save(true, None),
            "compare" => self.compare_selected(),
            "restore-trash" => self.restore_trash(),
            "print" => self.print_picture(),
            "sidebar" => self.sidebar.set_visible(!self.sidebar.get_visible()),
            "thumbnails" => self.thumb_scroll.set_visible(!self.thumb_scroll.get_visible()),
            "set-default" => self.set_default_viewer(),
            "preferences" => self.show_preferences(),
            other => self.file_action(other),
        }
    }

    pub fn message(&self, text: &str) {
        self.status.set_text(text);
        self.status.set_tooltip_text(Some(text));
    }

    pub fn doc_info(&self) -> Option<DocInfo> { self.doc_info.borrow().clone() }

    /// Re-read the cached document summary (only while no worker owns the document).
    pub fn refresh_doc(&self) {
        if self.busy.get() {
            return;
        }
        let info = self.doc.borrow().as_ref().map(|doc| {
            let doc = doc.lock().unwrap();
            DocInfo { path: doc.path.clone(), width: doc.image.width(), height: doc.image.height(), frames: doc.frames, dirty: doc.dirty(),
                      can_undo: doc.can_undo(), can_redo: doc.can_redo(), comment: doc.comment.clone(),
                      original_signature: doc.original_signature }
        });
        self.doc_info.replace(info);
    }

    pub fn set_doc(&self, doc: Option<PictureDocument>) {
        self.doc.replace(doc.map(|d| Arc::new(Mutex::new(d))));
        self.refresh_doc();
    }

    /// `run(work, ready, dispose)`: one background job at a time; results arrive on the main loop.
    pub fn run<T: Send + 'static>(&self, work: impl FnOnce() -> Result<T, String> + Send + 'static,
                                  ready: impl FnOnce(&PicturesWindow, T) + 'static, dispose: Option<Box<dyn FnOnce(T)>>) {
        if self.closed.get() || self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.spinner.start();
        self.update_actions();
        let generation = self.generation.get();
        let (_, receiver) = self.executor.submit(work);
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = receiver.await;
            let Some(inner) = weak.upgrade() else { return };
            let window = PicturesWindow(inner);
            let Ok(result) = result else { return };
            if window.closed.get() || generation != window.generation.get() {
                if let (Some(dispose), Ok(value)) = (dispose, result) {
                    dispose(value);
                }
                return;
            }
            window.busy.set(false);
            window.spinner.stop();
            window.refresh_doc();
            match result {
                Ok(value) => ready(&window, value),
                Err(error) => window.message(&error),
            }
            window.update_actions();
        });
    }

    pub fn update_actions(&self) {
        self.refresh_doc();
        let info = self.doc_info();
        let busy = self.busy.get();
        let editable = !busy && info.as_ref().is_some_and(|i| i.frames == 1);
        self.undo_button.set_sensitive(editable && info.as_ref().is_some_and(|i| i.can_undo));
        self.redo_button.set_sensitive(editable && info.as_ref().is_some_and(|i| i.can_redo));
        self.tools.set_sensitive(editable);
        self.comment_button.set_sensitive(editable);
        self.previous.set_sensitive(!busy);
        self.next.set_sensitive(!busy);
        self.edit_bar.set_visible(info.as_ref().is_some_and(|i| i.dirty));
        let has_doc = self.doc.borrow().is_some();
        for name in self.window.list_actions() {
            if let Some(action) = self.window.lookup_action(&name).and_then(|a| a.downcast::<gio::SimpleAction>().ok()) {
                action.set_enabled(!busy && (has_doc || !matches!(name.as_str(), "save-as" | "copy-image" | "print")));
            }
        }
    }

    /// `guard(action)`: ask about unsaved edits before an action that replaces the document.
    pub fn guard(&self, action: Action) {
        if self.busy.get() || self.confirm.borrow().as_ref().is_some_and(|c| c.get_visible()) {
            return;
        }
        let Some(info) = self.doc_info().filter(|i| i.dirty) else {
            action(self);
            return;
        };
        self.stop_slideshow();
        let name = info.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let weak = Rc::downgrade(&self.0);
        let confirm = ask(&self.window, &tr!("Save changes?"), &tr!("Save changes to {name} before continuing?", name = name.as_str()),
            &[(tr!("Cancel"), "cancel"), (tr!("Discard"), "discard"), (tr!("Save"), "save")], move |value| {
                let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
                window.confirm.replace(None);
                match value {
                    "save" => window.save(false, Some(action.clone())),
                    "discard" => action(&window),
                    _ => {}
                }
            });
        self.confirm.replace(Some(confirm));
    }

    fn thumbnail_factory(&self, compact: bool) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(&self.0);
        factory.connect_setup(move |_, object| {
            let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let size = [116, 168, 236][pref_int(&window.preferences.borrow(), "thumbnail_size", 1).clamp(0, 2) as usize];
            let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5)
                .width_request(if compact { 108 } else { size }).build();
            content.add_css_class("thumbnail");
            let picture = gtk::Picture::builder().can_shrink(true).content_fit(gtk::ContentFit::Contain)
                .height_request(if compact { 65 } else { (size as f64 * 0.72).round_ties_even() as i32 }).build();
            let fallback = gtk::Image::builder().pixel_size(if compact { 48 } else { 80 }).build();
            let stack = gtk::Stack::builder().height_request(if compact { 65 } else { 122 }).build();
            stack.add_named(&fallback, Some("icon"));
            stack.add_named(&picture, Some("picture"));
            content.append(&stack);
            let label = gtk::Label::builder().ellipsize(pango::EllipsizeMode::Middle).max_width_chars(if compact { 16 } else { 22 }).build();
            content.append(&label);
            item.set_child(Some(&content));
            unsafe { item.set_data("pictures-item", ItemWidgets { picture, fallback, stack, label, job: RefCell::new(None) }) };
            window.add_context_menu(content.upcast_ref(), Some(item), compact);
        });
        let weak = Rc::downgrade(&self.0);
        factory.connect_bind(move |_, object| {
            let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(entry) = item.item().and_then(|o| entry_of(&o)) else { return };
            let Some(widgets) = (unsafe { item.data::<ItemWidgets>("pictures-item") }) else { return };
            let widgets = unsafe { widgets.as_ref() };
            widgets.label.set_text(&entry.path.file_name().unwrap_or_default().to_string_lossy());
            if let Some(child) = item.child() {
                child.set_tooltip_text(Some(&entry.path.to_string_lossy()));
            }
            widgets.picture.set_paintable(None::<&gdk::Paintable>);
            widgets.stack.set_visible_child_name("icon");
            let video = model::is_video(&entry.path);
            widgets.fallback.set_icon_name(Some(if entry.folder { "folder-pictures" } else if video { "video-x-generic" } else { "image-x-generic" }));
            if entry.folder || video {
                return;
            }
            let key = (entry.path.to_string_lossy().into_owned(), entry.size, entry.modified.to_bits());
            let cached = {
                let mut cache = window.thumbnails.borrow_mut();
                cache.iter().position(|(k, _)| *k == key).map(|index| {
                    let pair = cache.remove(index);
                    let texture = pair.1.clone();
                    cache.push(pair);
                    texture
                })
            };
            if let Some(texture) = cached {
                widgets.picture.set_paintable(Some(&texture));
                widgets.stack.set_visible_child_name("picture");
                return;
            }
            if window.thumbnail_pending.borrow().len() >= 100 {
                return;
            }
            let id = window.thumbnail_sequence.get() + 1;
            window.thumbnail_sequence.set(id);
            let path = entry.path.clone();
            let (ticket, receiver) = window.thumbnail_executor.submit(move || model::thumbnail(&path, 192).ok());
            window.thumbnail_pending.borrow_mut().insert(id);
            widgets.job.replace(Some((ticket, id)));
            let (weak_window, weak_item) = (Rc::downgrade(&window.0), item.downgrade());
            glib::spawn_future_local(async move {
                let result = receiver.await;
                let Some(window) = weak_window.upgrade().map(PicturesWindow) else { return };
                window.thumbnail_pending.borrow_mut().remove(&id);
                let (Ok(Some(pixels)), false) = (result, window.closed.get()) else { return };
                let (w, h) = pixels.dimensions();
                let texture: gdk::Texture = gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8,
                    &glib::Bytes::from_owned(pixels.into_raw()), w as usize * 4).upcast();
                {
                    let mut cache = window.thumbnails.borrow_mut();
                    cache.push((key.clone(), texture.clone()));
                    while cache.len() > 96 {
                        cache.remove(0);
                    }
                }
                let Some(item) = weak_item.upgrade() else { return };
                let current = item.item().and_then(|o| entry_of(&o));
                if current.is_some_and(|e| e.path == entry.path && e.size == entry.size) {
                    if let Some(widgets) = unsafe { item.data::<ItemWidgets>("pictures-item") } {
                        let widgets = unsafe { widgets.as_ref() };
                        widgets.picture.set_paintable(Some(&texture));
                        widgets.stack.set_visible_child_name("picture");
                    }
                }
            });
        });
        let weak = Rc::downgrade(&self.0);
        factory.connect_unbind(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(widgets) = (unsafe { item.data::<ItemWidgets>("pictures-item") }) else { return };
            let widgets = unsafe { widgets.as_ref() };
            if let Some((ticket, id)) = widgets.job.take() {
                if ticket.cancel() {
                    if let Some(window) = weak.upgrade() {
                        window.thumbnail_pending.borrow_mut().remove(&id);
                    }
                }
            }
            widgets.picture.set_paintable(None::<&gdk::Paintable>);
        });
        factory
    }

    fn resize_thumbnails(&self) {
        self.preferences.borrow_mut().insert("thumbnail_size".into(), Value::from(self.thumbnail_size.selected()));
        self.save_preferences();
        self.grid.set_factory(Some(&self.thumbnail_factory(false)));
    }

    fn add_context_menu(&self, widget: &gtk::Widget, item: Option<&gtk::ListItem>, compact: bool) {
        let gesture = gtk::GestureClick::builder().button(3).build();
        let weak = Rc::downgrade(&self.0);
        let item = item.map(|i| i.downgrade());
        let target = widget.downgrade();
        gesture.connect_pressed(move |controller, _, x, y| {
            let (Some(window), Some(widget)) = (weak.upgrade().map(PicturesWindow), target.upgrade()) else { return };
            if window.busy.get() {
                return;
            }
            if let Some(item) = item.as_ref().and_then(|i| i.upgrade()) {
                let Some(entry) = item.item().and_then(|o| entry_of(&o)).filter(|e| !e.folder) else { return };
                if compact {
                    // Strip actions apply to its selected picture only after
                    // it has opened, keeping unsaved-change prompts intact.
                    if Some(&entry.path) != window.current_path.borrow().as_ref() {
                        window.open_entry(&entry);
                        return;
                    }
                } else if !window.selection.is_selected(item.position()) {
                    window.selection.select_item(item.position(), true);
                }
            }
            let open_menu = window.context_menu.borrow().clone();
            if let Some(menu) = open_menu {
                menu.popdown();
            }
            let menu = gio::Menu::new();
            for (title, action) in [("Open With…", "open-with"), ("Compare Selected Pictures", "compare"), ("Save As…", "save-as"),
                                    ("Copy Image", "copy-image"), ("Rename…", "rename"), ("Copy To…", "copy"), ("Move To…", "move"),
                                    ("Move to Trash…", "trash"), ("Open Containing Folder", "folder")] {
                menu.append(Some(&tr!(title)), Some(&format!("win.{action}")));
            }
            let popup = gtk::PopoverMenu::from_model(Some(&menu));
            popup.set_parent(&widget);
            popup.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            window.context_menu.replace(Some(popup.clone()));
            let weak = Rc::downgrade(&window.0);
            popup.connect_closed(move |menu| {
                if let Some(window) = weak.upgrade() {
                    if window.context_menu.borrow().as_ref() == Some(menu) {
                        window.context_menu.replace(None);
                    }
                }
                let menu = menu.clone();
                glib::idle_add_local_once(move || if menu.parent().is_some() { menu.unparent() });
            });
            controller.set_state(gtk::EventSequenceState::Claimed);
            popup.popup();
        });
        widget.add_controller(gesture);
    }

    pub fn browse_folder(&self, folder: &Path, force: bool) {
        let folder = folder.to_path_buf();
        let browse: Action = Rc::new(move |w: &PicturesWindow| {
            w.stop_animation();
            w.stop_slideshow();
            w.video.set_file(None::<&gio::File>);
            let target = std::fs::canonicalize(model_expand(&folder.to_string_lossy())).unwrap_or_else(|_| folder.clone());
            let (order, reverse) = [("name", false), ("name", true), ("date", true), ("date", false), ("size", true)]
                [w.sort.selected().min(4) as usize];
            let hidden = pref_bool(&w.preferences.borrow(), "hidden", false);
            let scan_target = target.clone();
            w.run(move || model::scan_folder(&scan_target, hidden, order, reverse)
                      .map_err(|e| tessel_core::pyerr::os_error_path(&e, &scan_target)),
                  move |w, entries| {
                      w.set_doc(None);
                      w.current_path.replace(None);
                      w.canvas.clear();
                      let count = entries.iter().filter(|e| !e.folder).count();
                      w.folder.replace(target.clone());
                      w.entries.replace(entries);
                      w.location.set_text(&target.to_string_lossy());
                      let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| target.to_string_lossy().into_owned());
                      w.caption.set_text(&name);
                      w.window.set_title(Some(&tr!("Pictures — {folder}", folder = target.to_string_lossy().as_ref())));
                      w.info.set_text(&target.to_string_lossy());
                      w.stack.set_visible_child_name("browse");
                      w.filter_entries();
                      w.watch_folder();
                      w.message(&tr!("{count} pictures and videos", count = count as i64));
                  }, None);
        });
        if force { browse(self) } else { self.guard(browse) }
    }

    pub fn filter_entries(&self) {
        let query = tessel_core::pyjson::casefold(&self.search.text());
        let items: Vec<glib::BoxedAnyObject> = self.entries.borrow().iter()
            .filter(|e| tessel_core::pyjson::casefold(&e.path.file_name().unwrap_or_default().to_string_lossy()).contains(&query))
            .map(item).collect();
        self.store.splice(0, self.store.n_items(), &items);
    }

    fn refresh_folder(&self) {
        if self.stack.visible_child_name().as_deref() == Some("browse") {
            let folder = self.folder.borrow().clone();
            self.browse_folder(&folder, false);
        }
    }

    pub fn watch_folder(&self) {
        if let Some(monitor) = self.monitor.take() {
            monitor.cancel();
        }
        let folder = self.folder.borrow().clone();
        if let Ok(monitor) = gio::File::for_path(&folder).monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            let weak = Rc::downgrade(&self.0);
            monitor.connect_changed(move |_, file, _, event| {
                if let Some(window) = weak.upgrade() {
                    PicturesWindow(window).folder_changed(file, event);
                }
            });
            self.monitor.replace(Some(monitor));
        }
    }

    pub fn folder_changed(&self, file: &gio::File, event: gio::FileMonitorEvent) {
        if self.closed.get() || event == gio::FileMonitorEvent::ChangesDoneHint {
            return;
        }
        // Avoid redraw storms from unrelated files or our atomic-save temporary.
        let name = file.basename().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
        let path = PathBuf::from(&name);
        if name.starts_with(".tessel-pictures-") || !(model::is_image(&path) || model::is_video(&path)) {
            return;
        }
        if let Some(timer) = self.monitor_timer.take() {
            timer.remove();
        }
        let changed = file.path().unwrap_or_else(|| PathBuf::from("/"));
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(450), move || {
            let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
            window.monitor_timer.replace(None);
            if !window.busy.get() && !window.closed.get() && window.stack.visible_child_name().as_deref() == Some("browse") {
                let folder = window.folder.borrow().clone();
                window.browse_folder(&folder, true);
            } else if window.current_path.borrow().as_ref() == Some(&changed) {
                if let Some(info) = window.doc_info() {
                    if model::signature(&changed).is_ok_and(|s| s == info.original_signature) {
                        return;
                    }
                }
                window.message(&tr!("The file changed on disk. Reload to see the new version; unsaved edits are preserved."));
            }
        });
        self.monitor_timer.replace(Some(id));
    }

    pub fn monitor_pending(&self) -> bool { self.monitor_timer.borrow().is_some() }

    pub fn open_paths(&self, paths: &[PathBuf], slideshow: bool) {
        let paths: Vec<PathBuf> = paths.iter().filter(|p| !p.as_os_str().is_empty()).map(|p| model_expand(&p.to_string_lossy())).collect();
        let Some(first) = paths.first().cloned() else { return };
        if first.is_dir() {
            self.browse_folder(&first, false);
        } else if paths.len() > 1 {
            self.guard(Rc::new(move |w: &PicturesWindow| { compare(&w.window, &paths); }));
        } else {
            self.guard(Rc::new(move |w: &PicturesWindow| w.load_picture(&first, slideshow)));
        }
    }

    pub fn open_entry(&self, entry: &Entry) { self.open_paths(std::slice::from_ref(&entry.path), false); }

    pub fn load_picture(&self, path: &Path, slideshow: bool) {
        if self.busy.get() {
            return;
        }
        self.stop_animation();
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let video = model::is_video(&path);
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.message(&tr!("Opening {name}…", name = name.as_str()));
        let hidden = pref_bool(&self.preferences.borrow(), "hidden", false);
        let work_path = path.clone();
        self.run(move || {
            let parent = work_path.parent().unwrap_or(Path::new("/"));
            let entries = model::scan_folder(parent, hidden, "name", false).map_err(|e| tessel_core::pyerr::os_error_path(&e, parent))?;
            let doc = if video { None } else { Some(PictureDocument::open(&work_path).map_err(|e| e.to_string())?) };
            Ok((doc, entries))
        }, move |w, (doc, entries)| {
            w.set_doc(doc);
            w.entries.replace(entries);
            w.displayed_frame.set(0);
            w.current_path.replace(Some(path.clone()));
            w.folder.replace(path.parent().map(Path::to_path_buf).unwrap_or_default());
            w.caption.set_text(&name);
            w.window.set_title(Some(&tr!("{name} — Pictures", name = name.as_str())));
            w.stack.set_visible_child_name("view");
            w.refresh_strip();
            w.video.set_file(None::<&gio::File>);
            if video {
                w.canvas.clear();
                w.video.set_file(Some(&gio::File::for_path(&path)));
                w.view_stack.set_visible_child_name("video");
                w.info.set_text(&format!("{name}\n\n{}\n{}", tr!("Video"), path.parent().unwrap_or(Path::new("/")).display()));
                w.animation_button.set_visible(false);
            } else {
                w.view_stack.set_visible_child_name("image");
                if !pref_bool(&w.preferences.borrow(), "keep_zoom", false) {
                    w.canvas.set_zoom(Zoom::Mode("fit"));
                }
                w.update_document();
            }
            w.watch_folder();
            w.message(&path.to_string_lossy());
            if slideshow {
                w.toggle_slideshow();
            }
        }, Some(Box::new(|(doc, _)| drop(doc))));
    }

    pub fn refresh_strip(&self) {
        let items: Vec<glib::BoxedAnyObject> = self.entries.borrow().iter().filter(|e| !e.folder).map(item).collect();
        self.thumb_store.splice(0, self.thumb_store.n_items(), &items);
        let current = self.current_path.borrow().clone();
        for index in 0..self.thumb_store.n_items() {
            if self.thumb_store.item(index).and_then(|o| entry_of(&o)).map(|e| e.path) == current {
                self.thumb_selection.set_selected(index);
                self.strip.scroll_to(index, gtk::ListScrollFlags::NONE, None);
                break;
            }
        }
    }

    pub fn update_document(&self) {
        self.refresh_doc();
        let Some(doc) = self.doc.borrow().clone() else { return };
        let doc = doc.lock().unwrap();
        self.current_path.replace(Some(doc.path.clone()));
        self.canvas.set_image(&doc.image);
        let name = doc.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.caption.set_text(&name);
        self.window.set_title(Some(&format!("{}{}", if doc.dirty() { "• " } else { "" }, tr!("{name} — Pictures", name = name.as_str()))));
        let thousands = |n: u32| {
            let digits = n.to_string();
            let mut out = String::new();
            for (i, c) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i) % 3 == 0 {
                    out.push(',');
                }
                out.push(c);
            }
            out
        };
        let mut details = vec![name.clone(),
            tr!("{width} × {height} pixels", width = thousands(doc.image.width()).as_str(), height = thousands(doc.image.height()).as_str()),
            doc.metadata.format.clone().unwrap_or_else(|| model::suffix(&doc.path).trim_start_matches('.').to_uppercase())];
        if let Ok(meta) = std::fs::metadata(&doc.path) {
            use std::os::unix::fs::MetadataExt;
            details.push(format!("{:.2} MB", meta.len() as f64 / 1024.0 / 1024.0));
            if let Ok(time) = glib::DateTime::from_unix_local(meta.mtime()) {
                details.push(time.format("%x %X").map(|s| s.to_string()).unwrap_or_default());
            }
        }
        if doc.frames > 1 {
            details.push(tr!("{count} frames / pages", count = doc.frames as i64));
        }
        for (tag, label) in [(271, tr!("Camera")), (272, tr!("Model")), (36867, tr!("Taken")), (33434, tr!("Exposure")),
                             (33437, tr!("Aperture")), (34855, tr!("ISO")), (37386, tr!("Focal length")), (42036, tr!("Lens"))] {
            if let Some(value) = doc.exif.display(tag) {
                details.push(format!("{label}: {value}"));
            }
        }
        if !doc.comment.is_empty() {
            details.extend([String::new(), doc.comment.clone()]);
        }
        self.info.set_text(&details.join("\n\n"));
        self.animation_button.set_visible(doc.frames > 1);
        self.animation_button.set_label(&tr!("Play animation / pages"));
        drop(doc);
        self.update_actions();
    }

    pub fn navigate(&self, step: i64) {
        let pictures: Vec<PathBuf> = self.entries.borrow().iter().filter(|e| !e.folder).map(|e| e.path.clone()).collect();
        if pictures.is_empty() {
            return;
        }
        let current = self.current_path.borrow().clone();
        let index = match current.as_ref().and_then(|c| pictures.iter().position(|p| p == c)) {
            Some(index) => index as i64,
            None => if step > 0 { -1 } else { 0 },
        };
        let mut target = index + step;
        if self.slideshow_timer.borrow().is_some() && pref_bool(&self.preferences.borrow(), "shuffle", false) {
            let alternatives: Vec<&PathBuf> = pictures.iter().filter(|p| Some(*p) != current.as_ref()).collect();
            if !alternatives.is_empty() {
                let choice = glib::random_int_range(0, alternatives.len() as i32) as usize;
                self.open_paths(&[alternatives[choice].clone()], false);
            }
            return;
        }
        if !(0..pictures.len() as i64).contains(&target) {
            if self.slideshow_timer.borrow().is_some() && !pref_bool(&self.preferences.borrow(), "loop", true) {
                self.stop_slideshow();
                return;
            }
            target = target.rem_euclid(pictures.len() as i64);
        }
        self.open_paths(&[pictures[target as usize].clone()], false);
    }

    pub fn toggle_fullscreen(&self) {
        if self.window.is_fullscreen() { self.window.unfullscreen() } else { self.window.fullscreen() }
    }

    fn fullscreen_widgets(&self) -> Vec<gtk::Widget> {
        vec![self.toolbar.clone().upcast(), self.sidebar.clone().upcast(), self.tools.clone().upcast(), self.thumb_scroll.clone().upcast(),
             self.zoom_bar.clone().upcast(), self.status_row.clone().upcast()]
    }

    fn fullscreen_changed(&self) {
        if self.window.is_fullscreen() {
            let controls: Vec<(gtk::Widget, bool)> = self.fullscreen_widgets().into_iter().map(|w| { let v = w.get_visible(); (w, v) }).collect();
            self.fullscreen_controls.replace(Some(controls));
            self.header.set_visible(false);
            for widget in self.fullscreen_widgets() {
                widget.set_visible(false);
            }
        } else {
            self.header.set_visible(true);
            for (widget, visible) in self.fullscreen_controls.take().unwrap_or_default() {
                widget.set_visible(visible);
            }
            if let Some(timer) = self.fullscreen_reveal_timer.take() {
                timer.remove();
            }
        }
    }

    pub fn fullscreen_motion(&self, y: f64) {
        if !self.window.is_fullscreen() {
            return;
        }
        if y < 65.0 {
            self.toolbar.set_visible(true);
        }
        if let Some(timer) = self.fullscreen_reveal_timer.take() {
            timer.remove();
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(2000), move || {
            let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
            window.fullscreen_reveal_timer.replace(None);
            let menu_open = window.menu_button.popover().is_some_and(|m| m.get_visible());
            if window.window.is_fullscreen() && !menu_open {
                window.toolbar.set_visible(false);
            }
        });
        self.fullscreen_reveal_timer.replace(Some(id));
    }

    pub fn toggle_slideshow(&self) {
        if self.slideshow_timer.borrow().is_some() {
            self.stop_slideshow();
            return;
        }
        if let Some(info) = self.doc_info().filter(|i| i.dirty) {
            let path = info.path.clone();
            self.guard(Rc::new(move |w: &PicturesWindow| w.load_picture(&path, true)));
            return;
        }
        if self.current_path.borrow().is_none() {
            let first = self.entries.borrow().iter().find(|e| !e.folder).cloned();
            if let Some(first) = first {
                self.load_picture(&first.path, true);
            }
            return;
        }
        let interval = pref_int(&self.preferences.borrow(), "interval", 5).max(1) as u32;
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_seconds_local(interval, move || {
            let Some(window) = weak.upgrade().map(PicturesWindow) else { return glib::ControlFlow::Break };
            if window.closed.get() {
                return glib::ControlFlow::Break;
            }
            if !window.busy.get() {
                window.navigate(1);
            }
            glib::ControlFlow::Continue
        });
        self.slideshow_timer.replace(Some(id));
        self.slide_button.set_child(Some(&image("media-playback-pause-symbolic", 16)));
        if let Some(app) = self.window.application() {
            self.inhibit_cookie.set(app.inhibit(Some(&self.window), gtk::ApplicationInhibitFlags::IDLE, Some(&tr!("Picture slideshow"))));
        }
        self.message(&tr!("Slideshow playing · F5 stops playback"));
    }

    pub fn stop_slideshow(&self) {
        if let Some(timer) = self.slideshow_timer.take() {
            timer.remove();
        }
        let cookie = self.inhibit_cookie.replace(0);
        if cookie != 0 {
            if let Some(app) = self.window.application() {
                app.uninhibit(cookie);
            }
        }
        self.slide_button.set_child(Some(&image("media-playback-start-symbolic", 16)));
    }

    pub fn slideshow_running(&self) -> bool { self.slideshow_timer.borrow().is_some() }

    pub fn toggle_animation(&self) {
        if self.animation_playing.get() {
            self.stop_animation();
            return;
        }
        if !self.doc_info().is_some_and(|i| i.frames > 1) {
            return;
        }
        self.animation_index.set(0);
        self.animation_playing.set(true);
        self.animation_button.set_label(&tr!("Pause animation / pages"));
        self.schedule_animation(20);
    }

    fn schedule_animation(&self, delay: u64) {
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            if let Some(window) = weak.upgrade() {
                window.animation_timer.replace(None);
                PicturesWindow(window).animation_frame();
            }
        });
        self.animation_timer.replace(Some(id));
    }

    pub fn animation_timer_pending(&self) -> bool { self.animation_timer.borrow().is_some() }

    fn animation_frame(&self) {
        let Some(info) = self.doc_info() else { return };
        if self.closed.get() || !self.animation_playing.get() || info.frames <= 1 {
            return;
        }
        let (path, index, generation) = (info.path.clone(), self.animation_index.get(), self.animation_generation.get());
        let (_, receiver) = self.executor.submit(move || {
            model::load_pixels(&path, None, index).map(|l| (l.image, l.metadata.duration.map(|d| d as u64).unwrap_or(500).max(20)))
                .map_err(|e| e.to_string())
        });
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.await else { return };
            let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
            match result {
                Ok((pixels, delay)) => {
                    if !window.closed.get() && window.animation_playing.get() && generation == window.animation_generation.get() {
                        window.canvas.set_image(&pixels);
                        window.displayed_frame.set(index);
                        window.animation_index.set((index + 1) % info.frames);
                        window.schedule_animation(delay);
                    }
                }
                Err(error) => {
                    if !window.closed.get() {
                        window.stop_animation();
                        window.message(&error);
                    }
                }
            }
        });
    }

    pub fn stop_animation(&self) {
        self.animation_playing.set(false);
        self.animation_generation.set(self.animation_generation.get() + 1);
        if let Some(timer) = self.animation_timer.take() {
            timer.remove();
        }
        self.animation_button.set_label(&tr!("Play animation / pages"));
    }

    fn tool_changed(&self) {
        if self.changing_tool.get() {
            return;
        }
        let kind = ANNOTATION_TOOLS[self.tool.selected().min(9) as usize];
        if !kind.is_empty() {
            self.set_edit_tool(kind);
        }
    }

    fn crop_toggled(&self) {
        if !self.changing_tool.get() {
            self.set_edit_tool(if self.crop_button.is_active() { "crop" } else { "pan" });
        }
    }

    pub fn set_edit_tool(&self, kind: &str) {
        self.changing_tool.set(true);
        self.crop_button.set_active(kind == "crop");
        self.tool.set_selected(if kind == "crop" { 0 } else { ANNOTATION_TOOLS.iter().position(|t| *t == kind).unwrap_or(0) as u32 });
        self.changing_tool.set(false);
        self.canvas.set_tool(kind);
        self.selection_options.set_visible(matches!(kind, "crop" | "red-eye"));
        self.crop_ratio.set_visible(kind == "crop");
        self.annotation_options.set_visible(!matches!(kind, "pan" | "crop" | "red-eye"));
        self.update_ratio();
    }

    fn update_ratio(&self) {
        let original = self.doc_info().map(|i| i.width as f64 / i.height as f64).unwrap_or(1.0);
        self.canvas.ratio.set([0.0, original, 1.0, 4.0 / 3.0, 3.0 / 2.0, 16.0 / 9.0][self.crop_ratio.selected().min(5) as usize]);
    }

    fn selection_changed(&self, rectangle: [f64; 4]) {
        for (widget, value) in self.coordinates.iter().zip(rectangle) {
            widget.set_value(value.round_ties_even());
        }
        self.selection_label.set_text(&tr!("{width} × {height} pixels", width = rectangle[2].round_ties_even() as i64,
                                           height = rectangle[3].round_ties_even() as i64));
    }

    pub fn apply_selection(&self) {
        let tool = self.canvas.tool();
        if tool != "crop" && tool != "red-eye" {
            self.message(&tr!("Choose Crop or Reduce red eye first."));
            return;
        }
        let rect = [0, 1, 2, 3].map(|i| self.coordinates[i].value_as_int() as f64);
        self.edit(if tool == "crop" { Operation::Crop(rect) } else { Operation::RedEye(rect) });
    }

    fn annotate(&self, kind: &str, points: Vec<(f64, f64)>) {
        let rgba = self.annotation_color.rgba();
        let channel = |v: f32| (v as f64 * 255.0).round_ties_even() as u8;
        self.edit(Operation::Annotate(Annotation {
            kind: AnnotationKind::from_name(kind), points,
            color: [channel(rgba.red()), channel(rgba.green()), channel(rgba.blue()), channel(rgba.alpha())],
            width: self.annotation_width.value_as_int() as i64, text: self.annotation_text.text().to_string(),
            stamp: self.stamp_path.borrow().clone(),
        }));
    }

    fn choose_stamp(&self) {
        let chooser = gtk::FileDialog::builder().title(tr!("Choose Image Stamp")).build();
        let weak = Rc::downgrade(&self.0);
        chooser.open(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            if let (Some(window), Ok(file)) = (weak.upgrade().map(PicturesWindow), result) {
                window.stamp_path.replace(file.path());
                window.set_edit_tool("stamp");
            }
        });
    }

    pub fn edit(&self, operation: Operation) {
        if self.busy.get() {
            return;
        }
        let Some(doc) = self.doc.borrow().clone() else { return };
        self.stop_animation();
        self.stop_slideshow();
        self.run(move || doc.lock().unwrap().apply(operation).map_err(|e| e.to_string()), |w, _| {
            w.update_document();
            w.message(&tr!("Edited · Save to write changes, or Undo to revert."));
        }, None);
    }

    pub fn undo(&self, redo: bool) {
        if self.busy.get() {
            return;
        }
        let Some(doc) = self.doc.borrow().clone() else { return };
        self.run(move || doc.lock().unwrap().undo(redo).map_err(|e| e.to_string()), |w, _| w.update_document(), None);
    }

    fn reload_original(&self) {
        if let Some(info) = self.doc_info() {
            self.open_paths(&[info.path], false);
        }
    }

    pub fn resize_picture(&self) {
        let Some(info) = self.doc_info() else { return };
        let width = number(info.width as f64, 1.0, 100000.0, 1.0);
        let height = number(info.height as f64, 1.0, 100000.0, 1.0);
        let aspect = info.width as f64 / info.height as f64;
        let linked = gtk::Switch::builder().active(true).build();
        let updating = Rc::new(Cell::new(false));
        let link = |source: &gtk::SpinButton, other: &gtk::SpinButton, ratio: f64| {
            let (other, linked, updating) = (other.clone(), linked.clone(), updating.clone());
            source.connect_value_changed(move |widget| {
                if linked.is_active() && !updating.get() {
                    updating.set(true);
                    other.set_value((widget.value() * ratio).round_ties_even().max(1.0));
                    updating.set(false);
                }
            });
        };
        link(&width, &height, 1.0 / aspect);
        link(&height, &width, aspect);
        let (w, h) = (width.clone(), height.clone());
        let weak = Rc::downgrade(&self.0);
        form(&self.window, &tr!("Resize Picture"), vec![(tr!("Width"), width.upcast()), (tr!("Height"), height.upcast()),
             (tr!("Keep proportions"), linked.upcast())], &tr!("Resize"), move || {
            if let Some(window) = weak.upgrade().map(PicturesWindow) {
                window.edit(Operation::Resize(w.value_as_int() as f64, h.value_as_int() as f64));
            }
            Ok(())
        });
    }

    pub fn adjust_picture(&self) {
        if self.doc.borrow().is_none() {
            return;
        }
        let names = ["Brightness", "Contrast", "Saturation", "Gamma"];
        let widgets: Vec<gtk::SpinButton> = names.iter().map(|n| {
            let widget = number(1.0, 0.1, if *n == "Gamma" { 5.0 } else { 3.0 }, 0.05);
            widget.set_digits(2);
            widget
        }).collect();
        let rows = names.iter().zip(&widgets).map(|(n, w)| (tr!(n), w.clone().upcast())).collect();
        let weak = Rc::downgrade(&self.0);
        form(&self.window, &tr!("Adjust Colors"), rows, &tr!("Apply"), move || {
            if let Some(window) = weak.upgrade().map(PicturesWindow) {
                window.edit(Operation::Adjust { brightness: widgets[0].value(), contrast: widgets[1].value(),
                                                saturation: widgets[2].value(), gamma: widgets[3].value() });
            }
            Ok(())
        });
    }

    fn edit_comment(&self) {
        let Some(info) = self.doc_info() else { return };
        let entry = gtk::Entry::builder().text(&info.comment).hexpand(true).build();
        let weak = Rc::downgrade(&self.0);
        let text = entry.clone();
        form(&self.window, &tr!("Picture Description"), vec![(tr!("Description"), entry.upcast())], &tr!("Apply"), move || {
            if let Some(window) = weak.upgrade().map(PicturesWindow) {
                window.edit(Operation::Comment(text.text().to_string()));
            }
            Ok(())
        });
    }

    fn compare_selected(&self) {
        let paths = self.selected_paths();
        if paths.len() >= 2 {
            compare(&self.window, &paths);
        } else if let Some(current) = { let current = self.current_path.borrow().clone(); current } {
            let chooser = gtk::FileDialog::builder().title(tr!("Compare With")).build();
            let weak = Rc::downgrade(&self.0);
            chooser.open(Some(&self.window), None::<&gio::Cancellable>, move |result| {
                if let (Some(window), Ok(file)) = (weak.upgrade().map(PicturesWindow), result) {
                    if let Some(path) = file.path() {
                        compare(&window.window, &[current.clone(), path]);
                    }
                }
            });
        } else {
            self.message(&tr!("Select two to four pictures to compare."));
        }
    }

    /// Whether a viewing key belongs to the picture rather than a control.
    pub fn navigation_focus(&self, key: gdk::Key) -> bool {
        if self.stack.visible_child_name().as_deref() != Some("view") {
            return false;
        }
        let Some(focus) = GtkWindowExt::focus(&self.window) else { return true };
        if focus.is::<gtk::Editable>() || focus.is::<gtk::TextView>() || focus.ancestor(gtk::Popover::static_type()).is_some() {
            return false;
        }
        if focus == self.canvas.area.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&self.image_scroll) {
            return true;
        }
        // The strip's own Left/Right keys move its selection; arrows still
        // navigate pictures there, while activating keys stay with the list.
        if focus.is_ancestor(&self.thumb_scroll) {
            return [gdk::Key::Left, gdk::Key::Right, gdk::Key::Page_Up, gdk::Key::Page_Down, gdk::Key::Home, gdk::Key::End].contains(&key);
        }
        false
    }

    pub fn can_pan(&self, horizontal: bool) -> bool {
        if !matches!(self.canvas.zoom_mode(), "custom" | "actual") {
            return false;
        }
        let adjustment = if horizontal { self.image_scroll.hadjustment() } else { self.image_scroll.vadjustment() };
        adjustment.upper() - adjustment.page_size() > 1.0
    }

    fn pan(&self, horizontal: bool, step: f64) {
        let adjustment = if horizontal { self.image_scroll.hadjustment() } else { self.image_scroll.vadjustment() };
        adjustment.set_value(adjustment.value() + step * (adjustment.page_size() / 10.0).max(24.0));
    }

    fn navigate_to(&self, last: bool) {
        let pictures: Vec<PathBuf> = self.entries.borrow().iter().filter(|e| !e.folder).map(|e| e.path.clone()).collect();
        let target = if last { pictures.last() } else { pictures.first() };
        if let Some(target) = target {
            self.open_paths(&[target.clone()], false);
        }
    }

    /// Loupe-style viewing keys: arrows navigate, and pan only when zoomed.
    pub fn navigation_key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SUPER_MASK) || !self.navigation_focus(key) {
            return false;
        }
        let alt = modifiers.contains(gdk::ModifierType::ALT_MASK);
        let video = self.view_stack.visible_child_name().as_deref() == Some("video");
        if key == gdk::Key::Left || key == gdk::Key::Right {
            let step = if key == gdk::Key::Left { -1 } else { 1 };
            if !alt && !video && self.can_pan(true) {
                self.pan(true, step as f64);
            } else {
                self.navigate(step);
            }
            return true;
        }
        if alt {
            return false;
        }
        if (key == gdk::Key::Up || key == gdk::Key::Down) && !video && self.can_pan(false) {
            self.pan(false, if key == gdk::Key::Up { -1.0 } else { 1.0 });
            return true;
        }
        let step = match key {
            gdk::Key::Page_Up | gdk::Key::BackSpace => Some(-1),
            gdk::Key::Page_Down | gdk::Key::space => Some(1),
            _ => None,
        };
        if let Some(step) = step {
            if !(video && (key == gdk::Key::space || key == gdk::Key::BackSpace)) {
                self.navigate(step);
                return true;
            }
        }
        if key == gdk::Key::Home || key == gdk::Key::End {
            self.navigate_to(key == gdk::Key::End);
            return true;
        }
        false
    }

    fn zoom_key(&self, key: gdk::Key) -> bool {
        if [gdk::Key::plus, gdk::Key::equal, gdk::Key::KP_Add].contains(&key) {
            self.canvas.set_zoom(Zoom::Value(self.canvas.scale() * 1.25));
        } else if [gdk::Key::minus, gdk::Key::underscore, gdk::Key::KP_Subtract].contains(&key) {
            self.canvas.set_zoom(Zoom::Value(self.canvas.scale() / 1.25));
        } else if key == gdk::Key::_0 || key == gdk::Key::KP_0 {
            self.canvas.set_zoom(Zoom::Mode("fit"));
        } else {
            return false;
        }
        true
    }

    pub fn key_pressed(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        let original = key;
        let key = key.to_lower();
        let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        if control {
            if self.stack.visible_child_name().as_deref() == Some("view") && self.zoom_key(key) {
                return true;
            }
            // Ctrl+Q and Ctrl+? are application accelerators (app_menu).
            match key {
                gdk::Key::o => self.choose_open(shift),
                gdk::Key::s => self.save(shift, None),
                gdk::Key::z => self.undo(shift),
                gdk::Key::y => self.undo(true),
                gdk::Key::p => self.print_picture(),
                gdk::Key::r => self.edit(if shift { Operation::Left } else { Operation::Right }),
                gdk::Key::c => self.file_action("copy-image"),
                _ => return false,
            }
            return true;
        }
        if self.navigation_key(key, modifiers) {
            return true;
        }
        // Do not consume navigation or editing keys while typing in a field.
        if let Some(focus) = GtkWindowExt::focus(&self.window) {
            if focus.is::<gtk::Editable>() || focus.is::<gtk::TextView>() {
                return false;
            }
        }
        match key {
            gdk::Key::Escape => {
                self.stop_slideshow();
                self.stop_animation();
                if self.window.is_fullscreen() { self.window.unfullscreen() } else { self.set_edit_tool("pan") }
                return true;
            }
            gdk::Key::F11 => self.toggle_fullscreen(),
            gdk::Key::F5 => self.toggle_slideshow(),
            gdk::Key::Delete => self.file_action("trash"),
            gdk::Key::F2 => self.file_action("rename"),
            gdk::Key::f => self.canvas.set_zoom(Zoom::Mode(if shift { "fill" } else if self.canvas.zoom_mode() == "fit" { "actual" } else { "fit" })),
            gdk::Key::plus => self.canvas.set_zoom(Zoom::Value(self.canvas.scale() * 1.25)),
            gdk::Key::minus => self.canvas.set_zoom(Zoom::Value(self.canvas.scale() / 1.25)),
            gdk::Key::Return if self.stack.visible_child_name().as_deref() == Some("view") => {
                let folder = self.folder.borrow().clone();
                self.browse_folder(&folder, false);
            }
            gdk::Key::Up if modifiers.contains(gdk::ModifierType::ALT_MASK) => {
                let parent = self.folder_parent();
                self.browse_folder(&parent, false);
            }
            gdk::Key::c if shift => {
                self.tools.set_visible(true);
                self.set_edit_tool("crop");
            }
            gdk::Key::r if shift => self.resize_picture(),
            _ => {
                let _ = original;
                return false;
            }
        }
        true
    }

    fn set_default_viewer(&self) {
        let Some(info) = tessel_core::glib_compat::desktop_app_info("org.tessel.Pictures.desktop") else {
            self.message(&tr!("Pictures is not installed as an application, so it cannot be the default viewer."));
            return;
        };
        for mime in crate::files::IMAGE_MIME_TYPES {
            if let Err(error) = info.set_as_default_for_type(mime) {
                self.message(&tr!("Could not set the default image viewer: {error}", error = error.message()));
                return;
            }
        }
        self.message(&tr!("Pictures is now the default image viewer."));
    }

    fn close_requested(&self) -> glib::Propagation {
        if self.closed.get() {
            return glib::Propagation::Proceed;
        }
        if self.busy.get() {
            self.message(&tr!("Please wait for the current image operation to finish."));
            return glib::Propagation::Stop;
        }
        self.guard(Rc::new(|w: &PicturesWindow| w.shutdown()));
        glib::Propagation::Stop
    }

    pub fn shutdown(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        if let Some(timer) = self.fullscreen_reveal_timer.take() {
            timer.remove();
        }
        let open_menu = self.context_menu.borrow().clone();
        if let Some(menu) = open_menu {
            menu.popdown();
        }
        self.generation.set(self.generation.get() + 1);
        self.stop_animation();
        self.stop_slideshow();
        if let Some(monitor) = self.monitor.take() {
            monitor.cancel();
        }
        if let Some(timer) = self.monitor_timer.take() {
            timer.remove();
        }
        self.video.set_file(None::<&gio::File>);
        self.canvas.clear();
        self.thumbnails.borrow_mut().clear();
        self.doc.replace(None);
        self.doc_info.replace(None);
        self.executor.shutdown();
        self.thumbnail_executor.shutdown();
        let theme = self.theme.borrow().clone();
        if let Some(theme) = theme {
            theme.close();
        }
        remove_css(&WidgetExt::display(&self.window), &self.css);
        self.window.destroy();
        let _: Option<PicturesWindow> = unsafe { self.window.steal_data("pictures-window") };
    }
}

/// `Path(text).expanduser()`.
pub fn model_expand(text: &str) -> PathBuf {
    if text == "~" {
        tessel_core::paths::home()
    } else if let Some(rest) = text.strip_prefix("~/") {
        tessel_core::paths::home().join(rest)
    } else {
        PathBuf::from(text)
    }
}
