//! A native GTK4 source chooser for Tessel Shell's screen-capture portal helper
//! (capture_picker_ui.py).
//!
//! The portal's source tokens are opaque: this view only displays source
//! metadata and returns the original record after an explicit user action.

use crate::sources::{CaptureSource, Kind};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::theme::{PaletteClient, WindowTheme};
use tessel_gtk::utils::{accessible, headerbar, image, key_controller, set_margins};

pub const MONITOR_ICON: &[&str] = &["monitor-symbolic", "video-display-symbolic"];
pub const WINDOW_ICON: &[&str] = &["window-symbolic", "focus-windows-symbolic", "application-x-executable-symbolic"];

/// Breeze has no video-display-symbolic and GTK would fall back to its colour
/// "video-display" art; try each symbolic name first.
pub fn symbolic_image(names: &[&str], size: i32) -> gtk::Image {
    let result = gtk::Image::from_gicon(&gio::ThemedIcon::from_names(names));
    result.set_pixel_size(size);
    result
}

/// Above the palette stylesheet (APPLICATION + 30): the Screens/Windows switch
/// needs a selected state the palette's near-identical button fills hide.
pub const SWITCHER_STYLE: &str = "
.capture-picker stackswitcher button { background-color: transparent; }
.capture-picker stackswitcher button:hover { background-color: alpha(currentColor, .08); }
.capture-picker stackswitcher button:checked {
    background-color: alpha(currentColor, .16); border-color: alpha(currentColor, .32); font-weight: 600;
}
";

pub const STYLE: &str = "
window.capture-picker { background-color: @theme_bg_color; color: @theme_fg_color; }
.capture-picker headerbar { box-shadow: none; }
.capture-picker .capture-heading { font-size: 16pt; font-weight: 700; }
.capture-picker .capture-title { font-size: 10.5pt; font-weight: 600; }
.capture-picker .muted { color: alpha(@theme_fg_color, .65); font-size: 9pt; }
.capture-picker .card {
    background-color: alpha(@theme_fg_color, .04);
    border: 1px solid alpha(@theme_fg_color, .12); border-radius: 12px; padding: 6px;
}
.capture-picker .capture-sources { background: transparent; }
.capture-picker .capture-sources row {
    border-radius: 8px; margin: 2px; padding: 0;
    background-color: transparent;
}
.capture-picker .capture-sources row:hover { background-color: alpha(@theme_fg_color, .07); }
.capture-picker .capture-sources row:selected {
    background-color: alpha(@theme_selected_bg_color, .20);
}
.capture-picker .capture-sources row:focus { outline: 2px solid @theme_selected_bg_color; outline-offset: -2px; }
.capture-picker .device-icon { color: @theme_selected_bg_color; }
.capture-picker .capture-selection { min-width: 20px; }
.capture-picker button {
    background-image: none; background-color: alpha(@theme_fg_color, .07);
    border: 1px solid alpha(@theme_fg_color, .12); border-radius: 8px;
    padding: 9px 18px; box-shadow: none; text-shadow: none;
}
.capture-picker button:hover { background-color: alpha(@theme_fg_color, .12); }
.capture-picker button.primary {
    background-color: @theme_selected_bg_color; color: @theme_selected_fg_color;
    border-color: @theme_selected_bg_color; font-weight: 600;
}
.capture-picker button:disabled { opacity: .45; }
.capture-picker stackswitcher button { padding: 7px 20px; }
.capture-picker scrolledwindow { background: transparent; border: none; }
.capture-picker scrollbar slider { min-width: 6px; min-height: 6px; border: none; padding: 0; margin: 0; }
.capture-picker .capture-empty { padding: 38px 18px; }
.capture-picker .capture-remote {
    background-color: alpha(@warning_color, .14);
    border: 1px solid alpha(@warning_color, .45); border-radius: 8px; padding: 12px 14px;
}
.capture-picker .capture-remote image { color: @warning_color; }
";

/// One source row (Python sets `row.source`, `row.check`, `row.title_label`…).
#[derive(Clone)]
pub struct Row {
    pub row: gtk::ListBoxRow,
    pub source: usize,
    pub check: gtk::Image,
    pub title_label: gtk::Label,
    pub subtitle_label: gtk::Label,
}

pub type OnSelected = Box<dyn FnOnce(CaptureSource)>;
pub type OnCancel = Box<dyn FnOnce()>;

/// Options a caller may inject (tests): pre-resolved icons and a palette client.
#[derive(Default)]
pub struct Options {
    pub remote_control: u8,
    /// `None` runs `icons::window_icons` (the `tessel-dock-toplevel` snapshot).
    pub icons: Option<HashMap<String, gio::Icon>>,
    pub palette: Option<PaletteClient>,
}

pub struct Inner {
    pub window: gtk::ApplicationWindow,
    pub sources: Vec<CaptureSource>,
    on_selected: RefCell<Option<OnSelected>>,
    on_cancel: RefCell<Option<OnCancel>>,
    finished: Cell<bool>,
    selected: Cell<Option<usize>>,
    rows: RefCell<Vec<Row>>,
    lists: RefCell<Vec<(Kind, gtk::ListBox)>>,
    empty_labels: RefCell<Vec<(Kind, gtk::Box)>>,
    query: RefCell<String>,
    icons: HashMap<String, gio::Icon>,
    theme: RefCell<Option<WindowTheme>>,
    css: gtk::CssProvider,
    switcher_css: gtk::CssProvider,
    display: gtk::gdk::Display,
    pub stack: gtk::Stack,
    pub switcher: gtk::StackSwitcher,
    pub search: gtk::SearchEntry,
    pub selection_label: gtk::Label,
    pub cancel_button: gtk::Button,
    pub share_button: gtk::Button,
    pub keys: RefCell<Option<gtk::EventControllerKey>>,
    pub remote_notice: RefCell<Option<gtk::Box>>,
}

/// Select a source without ever implicitly granting screen-capture access.
///
/// `on_selected(source)` or `on_cancel()` is called at most once. The callback
/// owns process exit; the chooser closes its window and theme binding first.
#[derive(Clone)]
pub struct CapturePicker(Rc<Inner>);

impl std::ops::Deref for CapturePicker {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

fn weak_call(weak: &Weak<Inner>, f: impl Fn(&CapturePicker)) {
    if let Some(inner) = weak.upgrade() {
        f(&CapturePicker(inner));
    }
}

impl CapturePicker {
    pub fn new(application: &gtk::Application, sources: Vec<CaptureSource>, on_selected: OnSelected, on_cancel: OnCancel,
               options: Options) -> Self {
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Share your screen")).build();
        let icons = options.icons.unwrap_or_else(|| crate::icons::window_icons(&sources));
        let display = WidgetExt::display(&window);
        window.add_css_class("capture-picker");
        window.set_default_size(640, 570);
        window.set_size_request(380, 380);
        window.set_titlebar(Some(&headerbar(&tr!("Screen sharing"))));

        let css = gtk::CssProvider::new();
        css.load_from_string(STYLE);
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let switcher_css = gtk::CssProvider::new();
        switcher_css.load_from_string(SWITCHER_STYLE);
        gtk::style_context_add_provider_for_display(&display, &switcher_css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 31);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        set_margins(&root, 22);
        window.set_child(Some(&root));
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        heading.append(&symbolic_image(MONITOR_ICON, 24));
        let headings = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        let title = gtk::Label::builder().label(tr!("Choose what to share")).xalign(0.0).build();
        title.add_css_class("capture-heading");
        headings.append(&title);
        let subtitle = gtk::Label::builder().label(tr!("Select a screen or window to start sharing.")).xalign(0.0).wrap(true).build();
        subtitle.add_css_class("muted");
        headings.append(&subtitle);
        heading.append(&headings);
        root.append(&heading);
        let mut remote_notice = None;
        if options.remote_control != 0 {
            let notice = remote_control_notice(options.remote_control);
            root.append(&notice);
            remote_notice = Some(notice);
        }

        let stack = gtk::Stack::builder().vexpand(true).build();
        stack.set_transition_type(gtk::StackTransitionType::None);
        let kinds: Vec<Kind> = [Kind::Monitor, Kind::Window].into_iter().filter(|k| sources.iter().any(|s| s.kind == *k)).collect();
        let switcher = gtk::StackSwitcher::builder().stack(&stack).halign(gtk::Align::Center).build();
        switcher.set_visible(kinds.len() > 1);
        root.append(&switcher);
        let search = gtk::SearchEntry::builder().hexpand(true).build();
        search.set_placeholder_text(Some(&tr!("Search screens and windows")));
        accessible(&search, &tr!("Search screens and windows"));
        search.set_visible(sources.len() >= 8);
        root.append(&search);

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let selection_label = gtk::Label::builder().label(tr!("No source selected")).xalign(0.0).hexpand(true).build();
        selection_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        selection_label.set_max_width_chars(26);
        selection_label.add_css_class("muted");
        footer.append(&selection_label);
        let cancel_button = gtk::Button::with_label(&tr!("Cancel"));
        footer.append(&cancel_button);
        let share_button = gtk::Button::builder().label(tr!("Share")).sensitive(false).build();
        share_button.add_css_class("primary");
        footer.append(&share_button);

        let picker = CapturePicker(Rc::new(Inner {
            window: window.clone(), sources, on_selected: RefCell::new(Some(on_selected)), on_cancel: RefCell::new(Some(on_cancel)),
            finished: Cell::new(false), selected: Cell::new(None), rows: RefCell::new(Vec::new()), lists: RefCell::new(Vec::new()),
            empty_labels: RefCell::new(Vec::new()), query: RefCell::new(String::new()), icons, theme: RefCell::new(None),
            css, switcher_css, display, stack: stack.clone(), switcher, search: search.clone(), selection_label,
            cancel_button: cancel_button.clone(), share_button: share_button.clone(), keys: RefCell::new(None),
            remote_notice: RefCell::new(remote_notice),
        }));
        let weak = Rc::downgrade(&picker.0);

        window.connect_close_request({
            let weak = weak.clone();
            move |_| {
                weak_call(&weak, |p| p.cancel());
                glib::Propagation::Stop
            }
        });
        let keys = key_controller(&window, true, {
            let weak = weak.clone();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    weak_call(&weak, |p| p.cancel());
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        picker.keys.replace(Some(keys));
        search.connect_search_changed({
            let weak = weak.clone();
            move |_| weak_call(&weak, |p| p.search_changed())
        });

        for kind in &kinds {
            picker.add_page(*kind);
        }
        if kinds.is_empty() {
            let empty = empty_state(&tr!("No capture sources available"), &tr!("Close this window and try screen sharing again."));
            stack.add_named(&empty, Some("empty"));
        }
        stack.connect_visible_child_name_notify({
            let weak = weak.clone();
            move |_| weak_call(&weak, |p| p.page_changed())
        });
        root.append(&stack);
        cancel_button.connect_clicked({
            let weak = weak.clone();
            move |_| weak_call(&weak, |p| p.cancel())
        });
        share_button.connect_clicked({
            let weak = weak.clone();
            move |_| weak_call(&weak, |p| p.share())
        });
        root.append(&footer);
        // A portal-side or toolkit destroy disposes without a callback.
        window.connect_destroy({
            let weak = weak.clone();
            move |_| weak_call(&weak, |p| { p.dispose(); })
        });
        // A default button or focus on the first row can accidentally select a
        // screen on map. Cancel is safe, and Tab/arrow keys navigate the lists.
        GtkWindowExt::set_focus(&window, Some(&cancel_button));
        let theme = tessel_gtk::theme::attach(&window, options.palette.as_ref());
        picker.theme.replace(Some(theme));
        picker
    }

    pub fn present(&self) { self.window.present(); }

    pub fn theme(&self) -> Option<WindowTheme> { self.theme.borrow().clone() }

    pub fn is_finished(&self) -> bool { self.finished.get() }

    /// `self._selected`.
    pub fn selected(&self) -> Option<&CaptureSource> { self.selected.get().map(|i| &self.sources[i]) }

    pub fn list(&self, kind: Kind) -> Option<gtk::ListBox> {
        self.lists.borrow().iter().find(|(k, _)| *k == kind).map(|(_, l)| l.clone())
    }

    pub fn empty_label(&self, kind: Kind) -> Option<gtk::Box> {
        self.empty_labels.borrow().iter().find(|(k, _)| *k == kind).map(|(_, b)| b.clone())
    }

    pub fn rows(&self) -> Vec<Row> { self.rows.borrow().clone() }

    pub fn row_for(&self, row: &gtk::ListBoxRow) -> Option<Row> {
        self.rows.borrow().iter().find(|r| &r.row == row).cloned()
    }

    fn visible_list(&self) -> Option<gtk::ListBox> {
        let name = self.stack.visible_child_name()?;
        let kind = match name.as_str() {
            "monitor" => Kind::Monitor,
            "window" => Kind::Window,
            _ => return None,
        };
        self.list(kind)
    }

    fn add_page(&self, kind: Kind) {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
        page.add_css_class("card");
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_min_content_height(110);
        let listing = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        listing.add_css_class("capture-sources");
        listing.set_activate_on_single_click(false);
        listing.set_valign(gtk::Align::Start);
        let weak = Rc::downgrade(&self.0);
        listing.set_filter_func({
            let weak = weak.clone();
            move |row| weak.upgrade().is_none_or(|inner| CapturePicker(inner).row_matches(row))
        });
        listing.connect_row_selected({
            let weak = weak.clone();
            move |listing, row| weak_call(&weak, |p| p.row_selected(listing, row))
        });
        listing.connect_row_activated({
            let weak = weak.clone();
            move |listing, row| weak_call(&weak, |p| p.row_activated(listing, Some(row)))
        });
        self.lists.borrow_mut().push((kind, listing.clone()));
        let icon_theme = gtk::IconTheme::for_display(&self.display);
        for (index, source) in self.sources.iter().enumerate() {
            if source.kind != kind {
                continue;
            }
            let row = gtk::ListBoxRow::builder().activatable(true).selectable(true).build();
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 14);
            set_margins(&content, 14);
            let icon = match self.icons.get(&source.token) {
                Some(app_icon) => {
                    let icon = gtk::Image::from_gicon(app_icon);
                    icon.set_pixel_size(30);
                    icon
                }
                None => {
                    let name = source.icon_name.as_str();
                    let fallback = if kind == Kind::Monitor { MONITOR_ICON } else { WINDOW_ICON };
                    if name.ends_with("-symbolic") && icon_theme.has_icon(name) && !fallback.contains(&name) {
                        let mut names = vec![name];
                        names.extend_from_slice(fallback);
                        symbolic_image(&names, 30)
                    } else {
                        symbolic_image(fallback, 30)
                    }
                }
            };
            icon.add_css_class("device-icon");
            icon.set_valign(gtk::Align::Center);
            content.append(&icon);
            let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
            let title = gtk::Label::builder().label(&source.title).xalign(0.0).build();
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.set_max_width_chars(50);
            title.add_css_class("capture-title");
            labels.append(&title);
            let subtitle = gtk::Label::builder().label(&source.subtitle).xalign(0.0).build();
            subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
            subtitle.add_css_class("muted");
            subtitle.set_visible(!source.subtitle.is_empty());
            labels.append(&subtitle);
            content.append(&labels);
            let check = image("object-select-symbolic", 18);
            check.add_css_class("capture-selection");
            check.set_opacity(0.0);
            content.append(&check);
            row.set_child(Some(&content));
            let suffix = |separator: &str| if source.subtitle.is_empty() { String::new() } else { format!("{separator}{}", source.subtitle) };
            row.set_tooltip_text(Some(&format!("{}{}", source.title, suffix("\n"))));
            accessible(&row, &format!("{}{}", source.title, suffix(", ")));
            self.rows.borrow_mut().push(Row { row: row.clone(), source: index, check, title_label: title, subtitle_label: subtitle });
            listing.append(&row);
        }
        let empty = empty_state(&tr!("No matching sources"), &tr!("Try a different search."));
        listing.set_placeholder(Some(&empty));
        self.empty_labels.borrow_mut().push((kind, empty));
        scroller.set_child(Some(&listing));
        page.append(&scroller);
        let label = if kind == Kind::Monitor { tr!("Screens") } else { tr!("Windows") };
        self.stack.add_titled(&page, Some(kind.name()), &label);
    }

    pub fn row_matches(&self, row: &gtk::ListBoxRow) -> bool {
        let query = self.query.borrow();
        if query.is_empty() {
            return true;
        }
        let Some(entry) = self.rows.borrow().iter().find(|r| &r.row == row).map(|r| r.source) else { return true };
        let source = &self.sources[entry];
        tessel_core::pyjson::casefold(&format!("{}\n{}", source.title, source.subtitle)).contains(query.as_str())
    }

    /// `_search_changed`.
    pub fn search_changed(&self) {
        let text = self.search.text();
        self.query.replace(tessel_core::pyjson::casefold(tessel_core::pyjson::py_strip(&text)));
        let lists: Vec<gtk::ListBox> = self.lists.borrow().iter().map(|(_, l)| l.clone()).collect();
        for listing in lists {
            if let Some(selected) = listing.selected_row() {
                if !self.row_matches(&selected) {
                    listing.unselect_all();
                }
            }
            listing.invalidate_filter();
        }
    }

    fn page_changed(&self) {
        let lists: Vec<gtk::ListBox> = self.lists.borrow().iter().map(|(_, l)| l.clone()).collect();
        for listing in lists {
            listing.unselect_all();
        }
        self.set_selected(None);
    }

    fn row_selected(&self, listing: &gtk::ListBox, row: Option<&gtk::ListBoxRow>) {
        if self.visible_list().as_ref() == Some(listing) {
            self.set_selected(row.and_then(|r| self.row_for(r)).map(|r| r.source));
        }
    }

    fn set_selected(&self, source: Option<usize>) {
        self.selected.set(source);
        for row in self.rows.borrow().iter() {
            row.check.set_opacity(if Some(row.source) == source { 1.0 } else { 0.0 });
        }
        self.share_button.set_sensitive(source.is_some() && !self.finished.get());
        let label = match source {
            Some(index) => self.sources[index].title.clone(),
            None => tr!("No source selected"),
        };
        self.selection_label.set_label(&label);
    }

    fn row_activated(&self, listing: &gtk::ListBox, row: Option<&gtk::ListBoxRow>) {
        let Some(row) = row else { return };
        if !self.finished.get() && self.visible_list().as_ref() == Some(listing) && self.row_matches(row) {
            if let Some(entry) = self.row_for(row) {
                self.set_selected(Some(entry.source));
                self.share();
            }
        }
    }

    fn share(&self) {
        if self.finished.get() {
            return;
        }
        let Some(index) = self.selected.get() else { return };
        let source = self.sources[index].clone();
        self.destroy();
        if let Some(callback) = self.on_selected.borrow_mut().take() {
            self.on_cancel.borrow_mut().take();
            callback(source);
        }
    }

    pub fn cancel(&self) {
        if !self.finished.get() {
            self.destroy();
            if let Some(callback) = self.on_cancel.borrow_mut().take() {
                self.on_selected.borrow_mut().take();
                callback();
            }
        }
    }

    fn dispose(&self) -> bool {
        if self.finished.replace(true) {
            return false;
        }
        if let Some(theme) = self.theme.borrow().as_ref() {
            theme.close();
        }
        gtk::style_context_remove_provider_for_display(&self.display, &self.css);
        gtk::style_context_remove_provider_for_display(&self.display, &self.switcher_css);
        true
    }

    /// Dispose without a callback, including when the portal is cancelled.
    pub fn destroy(&self) {
        if self.dispose() {
            self.window.destroy();
        }
    }
}

fn remote_control_notice(devices: u8) -> gtk::Box {
    let message = if devices & 3 == 3 {
        tr!("Sharing also lets this application control your keyboard and mouse.")
    } else if devices & 1 != 0 {
        tr!("Sharing also lets this application control your keyboard.")
    } else {
        tr!("Sharing also lets this application control your mouse.")
    };
    let notice = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    notice.add_css_class("capture-remote");
    notice.append(&image("dialog-warning-symbolic", 22));
    let label = gtk::Label::builder().label(&message).xalign(0.0).wrap(true).hexpand(true).build();
    notice.append(&label);
    accessible(&notice, &message);
    notice
}

fn empty_state(title: &str, subtitle: &str) -> gtk::Box {
    let container = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).valign(gtk::Align::Center).build();
    container.add_css_class("capture-empty");
    let icon = symbolic_image(MONITOR_ICON, 40);
    icon.add_css_class("muted");
    container.append(&icon);
    container.append(&gtk::Label::builder().label(title).wrap(true).justify(gtk::Justification::Center).build());
    let label = gtk::Label::builder().label(subtitle).wrap(true).justify(gtk::Justification::Center).build();
    label.add_css_class("muted");
    container.append(&label);
    container
}
