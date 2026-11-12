//! shortcuts_page.py: browse, search and rebind, plus the Super+/ cheat sheet.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gs_base::widgets::{label, label_with, status_text, Page, CSS};
use gs_base::SettingsPage;
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use tessel_core::tr;
use tessel_gtk::utils::{add_css, headerbar, key_controller, PRIORITY_APPLICATION};

use crate::shortcuts_model::{conflicts, display_key, display_key_with, key_from_event, normalize, Shortcut, ShortcutStore, Shortcuts, GROUPS};

pub const SOURCE_NOTES: &[(&str, &str)] = &[("default", "Default"), ("tiling", "Tiling")];

pub fn describe(shortcut: &Shortcut) -> String { shortcut.translated() }

fn translate(text: &str) -> String { tessel_core::i18n::translate(text) }

pub fn keycaps(key: &str) -> gtk::Box {
    let container = gtk::Box::builder().spacing(4).valign(gtk::Align::Center).build();
    let shown = display_key_with(key, &translate);
    for part in shown.split(" + ") {
        let cap = gtk::Label::new(Some(part));
        cap.add_css_class("keycap");
        container.append(&cap);
    }
    container.update_property(&[gtk::accessible::Property::Label(&shown)]);
    container
}

/// `apply(shortcut, key)` of the rebind dialog.
pub type Apply = Rc<dyn Fn(&Shortcut, &str)>;

/// Capture a new key combination and explain any conflict before saving.
pub struct RebindDialog {
    pub window: gtk::Window,
    pub shortcut: Shortcut,
    pub shortcuts: Vec<Shortcut>,
    apply: Apply,
    pub key: RefCell<String>,
    pub preview: gtk::Box,
    pub entry: gtk::Entry,
    pub message: gtk::Label,
    pub save: gtk::Button,
}

impl RebindDialog {
    pub fn new(parent: Option<&gtk::Window>, shortcut: &Shortcut, shortcuts: &[Shortcut], apply: Apply) -> Rc<RebindDialog> {
        let window = gtk::Window::builder().title(tr!("Change Shortcut")).modal(true).build();
        window.set_transient_for(parent);
        window.add_css_class("system-page");
        window.set_default_size(440, -1);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
        window.set_child(Some(&content));
        content.append(&label(&tr!("Press the new shortcut for “{value1}”.", value1 = describe(shortcut)), Some("group-title")));
        content.append(&label(&tr!("Press Escape to cancel or Backspace to clear."), Some("detail")));
        let preview = gtk::Box::builder().halign(gtk::Align::Center).margin_top(6).margin_bottom(6).build();
        content.append(&preview);
        let entry = gtk::Entry::builder().placeholder_text(tr!("Or type it, for example W-S-e")).build();
        entry.update_property(&[gtk::accessible::Property::Label(&tr!("Shortcut"))]);
        content.append(&entry);
        let message = label("", Some("detail"));
        content.append(&message);
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        let save = gtk::Button::builder().label(tr!("Set Shortcut")).sensitive(false).build();
        save.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&save);
        content.append(&buttons);
        let this = Rc::new(RebindDialog { window, shortcut: shortcut.clone(), shortcuts: shortcuts.to_vec(), apply, key: RefCell::new(String::new()),
                                          preview, entry, message, save });
        let weak = Rc::downgrade(&this);
        this.entry.connect_changed(move |entry| if let Some(t) = weak.upgrade() { t.set(crate::compat::strip(&entry.text()), true) });
        let w = this.window.clone();
        cancel.connect_clicked(move |_| w.close());
        let weak = Rc::downgrade(&this);
        this.save.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.apply_key() });
        let weak = Rc::downgrade(&this);
        this.entry.connect_activate(move |_| if let Some(t) = weak.upgrade() { t.apply_key() });
        let weak = Rc::downgrade(&this);
        key_controller(&this.window, true, move |_, keyval, keycode, state| {
            weak.upgrade().map(|t| t.key_pressed(keyval, keycode, state)).unwrap_or(glib::Propagation::Proceed)
        });
        this.window.connect_map(|w| inhibit(w, true));
        this.window.connect_unmap(|w| inhibit(w, false));
        this.set(&shortcut.key, false);
        let keep = RefCell::new(Some(this.clone()));
        this.window.connect_destroy(move |_| { keep.borrow_mut().take(); });
        this
    }

    pub fn present(&self) { self.window.present() }

    fn key_pressed(&self, keyval: gdk::Key, keycode: u32, state: gdk::ModifierType) -> glib::Propagation {
        let plain = !state.intersects(gdk::ModifierType::from_bits_truncate(0x1c00_1fff));
        if keyval == gdk::Key::Escape && plain {
            self.window.close();
            return glib::Propagation::Stop;
        }
        let held = gdk::ModifierType::SUPER_MASK | gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK;
        if self.entry_focused() && !state.intersects(held) && edits_text(keyval) {
            return glib::Propagation::Proceed;
        }
        if keyval == gdk::Key::BackSpace && plain {
            self.set("", false);
            return glib::Propagation::Stop;
        }
        let key = key_from_event(keyval, state, Some(&WidgetExt::display(&self.window)), Some(keycode));
        if !key.is_empty() {
            self.set(&key, false);
        }
        glib::Propagation::Stop
    }

    /// The focus sits on the entry's inner text widget, never on the entry itself.
    fn entry_focused(&self) -> bool {
        GtkWindowExt::focus(&self.window).is_some_and(|w| &w == self.entry.upcast_ref::<gtk::Widget>() || w.is_ancestor(&self.entry))
    }

    /// `_set(key, from_entry)`.
    pub fn set(&self, key: &str, from_entry: bool) {
        *self.key.borrow_mut() = key.to_owned();
        if let Some(child) = self.preview.first_child() {
            self.preview.remove(&child);
        }
        if !key.is_empty() {
            self.preview.append(&keycaps(key));
        }
        if !from_entry {
            // Emits 'changed', which re-runs set(text.strip(), from_entry=True) first.
            self.entry.set_text(key);
        }
        let (allowed, text, css) = self.check(key);
        status_text(&self.message, &text, css);
        self.save.set_sensitive(allowed);
    }

    pub fn check(&self, key: &str) -> (bool, String, Option<&'static str>) {
        if key.is_empty() {
            return (false, tr!("Press a key combination."), None);
        }
        if key.contains(['<', '>', '&', '"', '\'']) || (key.ends_with('-') && !key.ends_with("--")) {
            return (false, tr!("This is not a valid shortcut."), Some("error"));
        }
        if normalize(key) == normalize(&self.shortcut.key) {
            return (false, tr!("This is the current shortcut."), None);
        }
        let found = conflicts(&self.shortcuts, key, Some(&self.shortcut));
        if let Some(blocking) = found.iter().find(|i| i.active && i.source != "default") {
            return (false, tr!("Already used by “{value1}”.", value1 = describe(blocking)), Some("error"));
        }
        if let Some(default) = found.iter().find(|i| i.source == "default") {
            return (true, tr!("Replaces the default shortcut “{value1}”.", value1 = describe(default)), Some("warning"));
        }
        if let Some(first) = found.first() {
            return (true, tr!("Conflicts with “{value1}” when tiling is turned on.", value1 = describe(first)), Some("warning"));
        }
        (true, tr!("This shortcut is available."), None)
    }

    pub fn apply_key(&self) {
        let key = self.key.borrow().clone();
        if self.check(&key).0 {
            (self.apply)(&self.shortcut, &key);
            self.window.close();
        }
    }
}

/// Keys the entry needs for typing; everything else (Print, F9, …) is captured.
fn edits_text(keyval: gdk::Key) -> bool {
    use gdk::Key;
    keyval.to_unicode().is_some_and(|c| !c.is_control())
        || [Key::BackSpace, Key::Delete, Key::KP_Delete, Key::Left, Key::Right, Key::KP_Left, Key::KP_Right, Key::Home, Key::End,
            Key::KP_Home, Key::KP_End, Key::Tab, Key::ISO_Left_Tab, Key::Return, Key::KP_Enter].contains(&keyval)
}

/// Let compositor-bound combinations reach the dialog while it is open.
fn inhibit(window: &gtk::Window, active: bool) {
    if let Some(toplevel) = window.surface().and_downcast::<gdk::Toplevel>() {
        if active {
            toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
        } else {
            toplevel.restore_system_shortcuts();
        }
    }
}

/// Grouped, searchable shortcut rows shared by the page and the cheat sheet.
pub struct ShortcutList {
    pub parent: gtk::Box,
    edit: Option<Rc<dyn Fn(&Shortcut)>>,
    include_inactive: bool,
    /// `(row, shortcut, terms)`.
    pub rows: RefCell<Vec<(gtk::ListBoxRow, Shortcut, String)>>,
    pub groups: RefCell<Vec<(&'static str, gtk::Label, gtk::ListBox)>>,
}

impl ShortcutList {
    pub fn new(parent: &gtk::Box, edit: Option<Rc<dyn Fn(&Shortcut)>>, include_inactive: bool) -> ShortcutList {
        ShortcutList { parent: parent.clone(), edit, include_inactive, rows: Default::default(), groups: Default::default() }
    }

    pub fn render(&self, shortcuts: &[Shortcut]) {
        tessel_gtk::utils::remove_children(&self.parent);
        self.rows.borrow_mut().clear();
        self.groups.borrow_mut().clear();
        for group in GROUPS {
            let items: Vec<&Shortcut> = shortcuts.iter().filter(|i| i.group == *group && (self.include_inactive || i.active)).collect();
            if items.is_empty() {
                continue;
            }
            let heading = label(&tr!(*group), Some("group-title"));
            let card = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
            card.add_css_class("card");
            self.parent.append(&heading);
            self.parent.append(&card);
            for item in items {
                card.append(&self.row(item));
            }
            self.groups.borrow_mut().push((group, heading, card));
        }
    }

    fn row(&self, item: &Shortcut) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::builder().activatable(false).build();
        row.add_css_class("gs-row");
        let text = describe(item);
        let terms = crate::compat::casefold(&[text.as_str(), &item.description, &display_key_with(&item.key, &translate), &display_key(&item.key),
                                             &item.key, &tr!(&item.group)].join(" "));
        let line = gtk::Box::builder().spacing(10).margin_top(5).margin_bottom(5).build();
        let title = gtk::Label::builder().label(&text).xalign(0.0).hexpand(true).wrap(true).build();
        title.set_wrap_mode(pango::WrapMode::WordChar);
        if !item.active {
            title.add_css_class("detail");
        }
        line.append(&title);
        let note = if item.source == "tiling" && !item.active {
            tr!("When tiling is on")
        } else if item.description == "Disabled" {
            String::new()
        } else if let (Some((_, note)), true) = (SOURCE_NOTES.iter().find(|(s, _)| *s == item.source), self.edit.is_some()) {
            tr!(*note)
        } else {
            String::new()
        };
        if !note.is_empty() {
            line.append(&label_with(&note, Some("detail"), false));
        }
        line.append(&keycaps(&item.key));
        if let Some(edit) = &self.edit {
            let button = gtk::Button::builder().icon_name("document-edit-symbolic").tooltip_text(tr!("Change Shortcut"))
                .sensitive(item.editable()).build();
            button.add_css_class("flat");
            button.update_property(&[gtk::accessible::Property::Label(&format!("{} · {text}", tr!("Change Shortcut")))]);
            if !item.editable() {
                button.set_tooltip_text(Some(&tr!("Default and tiling shortcuts cannot be changed here.")));
            }
            let (edit, item) = (edit.clone(), item.clone());
            button.connect_clicked(move |_| edit(&item));
            line.append(&button);
        }
        row.set_child(Some(&line));
        self.rows.borrow_mut().push((row.clone(), item.clone(), terms));
        row
    }

    pub fn filter(&self, query: &str) -> usize {
        let query = crate::compat::casefold(query);
        let words = crate::compat::split_whitespace(&query);
        let mut visible = 0;
        for (row, _, terms) in self.rows.borrow().iter() {
            let show = words.iter().all(|w| terms.contains(w));
            row.set_visible(show);
            visible += show as usize;
        }
        for (_, heading, card) in self.groups.borrow().iter() {
            let shown = self.rows.borrow().iter().any(|(row, _, _)| row.is_visible() && row.parent().as_ref() == Some(card.upcast_ref()));
            heading.set_visible(shown);
            card.set_visible(shown);
        }
        visible
    }

    /// The shortcuts of the currently visible rows (tests).
    pub fn visible(&self) -> Vec<Shortcut> {
        self.rows.borrow().iter().filter(|(r, _, _)| r.is_visible()).map(|(_, s, _)| s.clone()).collect()
    }
}

pub struct ShortcutsPage {
    pub page: Page,
    pub store: Rc<dyn Shortcuts>,
    pub shortcuts: RefCell<Vec<Shortcut>>,
    pub search: gtk::SearchEntry,
    pub status: gtk::Label,
    pub empty: gtk::Label,
    pub container: gtk::Box,
    pub list: RefCell<Option<ShortcutList>>,
}

impl ShortcutsPage {
    pub fn new(store: Option<Rc<dyn Shortcuts>>) -> Rc<ShortcutsPage> {
        let page = Page::new("shortcuts-page", &tr!("Keyboard Shortcuts"),
                             &tr!("Every shortcut available on this desktop. Your own shortcuts can be changed; press Super + / to see this list at any time."),
                             Some("preferences-desktop-keyboard-shortcuts-symbolic"));
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search shortcuts")).build();
        search.update_property(&[gtk::accessible::Property::Label(&tr!("Search shortcuts"))]);
        page.content.append(&search);
        let status = label("", Some("detail"));
        page.content.append(&status);
        let empty = label(&tr!("No shortcuts found"), Some("detail"));
        empty.set_visible(false);
        page.content.append(&empty);
        let container = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.content.append(&container);
        let this = Rc::new(ShortcutsPage { page, store: store.unwrap_or_else(|| Rc::new(ShortcutStore::new(None, None))),
                                           shortcuts: Default::default(), search, status, empty, container, list: RefCell::new(None) });
        let weak = Rc::downgrade(&this);
        let edit: Rc<dyn Fn(&Shortcut)> = Rc::new(move |shortcut| if let Some(t) = weak.upgrade() { t.edit(shortcut) });
        *this.list.borrow_mut() = Some(ShortcutList::new(&this.container, Some(edit), true));
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| if let Some(t) = weak.upgrade() { t.filter() });
        this.reload();
        this
    }

    pub fn reload(&self) {
        let shortcuts = match self.store.load() {
            Ok(items) => items,
            Err(error) => {
                status_text(&self.status, &tr!("Shortcuts could not be read: {value1}", value1 = error.to_string()), Some("error"));
                Vec::new()
            }
        };
        if let Some(list) = self.list.borrow().as_ref() {
            list.render(&shortcuts);
        }
        *self.shortcuts.borrow_mut() = shortcuts;
        self.filter();
    }

    fn filter(&self) {
        let visible = self.list.borrow().as_ref().map(|l| l.filter(&self.search.text())).unwrap_or(0);
        self.empty.set_visible(visible == 0);
    }

    pub fn edit(self: &Rc<Self>, shortcut: &Shortcut) {
        let weak = Rc::downgrade(self);
        let apply: Apply = Rc::new(move |shortcut, key| if let Some(t) = weak.upgrade() { t.rebind(shortcut, key) });
        let root = self.page.widget.root().and_downcast::<gtk::Window>();
        RebindDialog::new(root.as_ref(), shortcut, &self.shortcuts.borrow(), apply).present();
    }

    /// The `apply` callback for a RebindDialog (tests build their own dialog).
    pub fn apply_callback(self: &Rc<Self>) -> Apply {
        let weak = Rc::downgrade(self);
        Rc::new(move |shortcut, key| if let Some(t) = weak.upgrade() { t.rebind(shortcut, key) })
    }

    pub fn rebind(&self, shortcut: &Shortcut, key: &str) {
        let (closed, status, shown) = (self.page.closed_flag(), self.status.clone(), display_key_with(key, &translate));
        let done: crate::labwc_settings::Done = Box::new(move |okay, _| {
            if closed.get() {
                return;
            }
            if okay {
                status_text(&status, &tr!("Shortcut changed to {value1}.", value1 = shown), None);
            } else {
                status_text(&status, &tr!("Saved, but the desktop could not reload its configuration."), Some("warning"));
            }
        });
        if let Err(error) = self.store.rebind(shortcut, key, Some(done)) {
            status_text(&self.status, &tr!("The shortcut could not be changed: {value1}", value1 = error.to_string()), Some("error"));
        }
        self.reload();
    }
}

struct SheetInner {
    window: gtk::ApplicationWindow,
    search: gtk::SearchEntry,
    lists: Vec<(gtk::Box, ShortcutList)>,
    provider: gtk::CssProvider,
    style_close: RefCell<Option<Box<dyn Fn()>>>,
    closed: Cell<bool>,
}

/// Compact read-only overview opened with Super+/ (`tessel-settings --shortcuts`).
#[derive(Clone)]
pub struct CheatSheet(Rc<SheetInner>);

impl CheatSheet {
    pub fn new(app: &gtk::Application) -> Self { Self::with_store(app, None) }

    pub fn with_store(app: &gtk::Application, store: Option<Rc<dyn Shortcuts>>) -> Self {
        let window = gtk::ApplicationWindow::builder().application(app).title(tr!("Keyboard Shortcuts")).build();
        window.add_css_class("system-page");
        window.add_css_class("shortcut-sheet");
        window.set_default_size(980, 720);
        // The same headerbar, palette and page styles as the Settings window.
        window.set_titlebar(Some(&headerbar(&tr!("Keyboard Shortcuts"))));
        let style_close = gs_base::style::install(&window);
        let provider = add_css(&format!("{CSS}.shortcut-sheet {{ background: @theme_bg_color; }}"), Some(&WidgetExt::display(&window)),
                               PRIORITY_APPLICATION);
        let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .margin_top(16).margin_bottom(16).margin_start(24).margin_end(24).build();
        window.set_child(Some(&outer));
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search shortcuts")).hexpand(true).build();
        search.update_property(&[gtk::accessible::Property::Label(&tr!("Search shortcuts"))]);
        outer.append(&search);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).homogeneous(false).max_children_per_line(2)
            .min_children_per_line(1).column_spacing(18).row_spacing(10).valign(gtk::Align::Start).build();
        scroll.set_child(Some(&flow));
        outer.append(&scroll);
        outer.append(&label(&tr!("Press Escape to close. Change shortcuts in Settings › Keyboard Shortcuts."), Some("gs-footer")));
        let store = store.unwrap_or_else(|| Rc::new(ShortcutStore::new(None, None)));
        let shortcuts = match store.load() {
            Ok(items) => items,
            Err(error) => {
                outer.append(&label(&error.to_string(), Some("error")));
                Vec::new()
            }
        };
        let mut lists = Vec::new();
        for group in GROUPS {
            let items: Vec<Shortcut> = shortcuts.iter().filter(|i| i.group == *group && i.active).cloned().collect();
            if items.is_empty() {
                continue;
            }
            let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).valign(gtk::Align::Start).build();
            column.set_size_request(440, -1);
            let list = ShortcutList::new(&column, None, false);
            list.render(&items);
            flow.append(&column);
            lists.push((column, list));
        }
        let sheet = CheatSheet(Rc::new(SheetInner { window, search, lists, provider, style_close: RefCell::new(Some(Box::new(style_close))),
                                                    closed: Cell::new(false) }));
        let weak = Rc::downgrade(&sheet.0);
        sheet.0.search.connect_search_changed(move |_| if let Some(inner) = weak.upgrade() { CheatSheet(inner).filter() });
        let weak = Rc::downgrade(&sheet.0);
        key_controller(&sheet.0.window, true, move |_, key, _, _| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            if key == gdk::Key::Escape {
                if !inner.search.text().is_empty() {
                    inner.search.set_text("");
                } else {
                    inner.window.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        // The window keeps the sheet alive until it closes.
        let keep = RefCell::new(Some(sheet.clone()));
        sheet.0.window.connect_close_request(move |_| {
            if let Some(sheet) = keep.borrow_mut().take() {
                sheet.cleanup();
            }
            glib::Propagation::Proceed
        });
        let search = sheet.0.search.clone();
        glib::idle_add_local_once(move || { search.grab_focus(); });
        sheet
    }

    fn filter(&self) {
        let query = self.0.search.text();
        for (column, list) in &self.0.lists {
            if let Some(parent) = column.parent() {
                parent.set_visible(list.filter(&query) > 0);
            }
        }
    }

    fn cleanup(&self) {
        if !self.0.closed.replace(true) {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.0.window), &self.0.provider);
            if let Some(close) = self.0.style_close.borrow_mut().take() {
                close();
            }
        }
    }

    pub fn present(&self) { self.0.window.present() }

    pub fn close(&self) { self.0.window.close() }

    pub fn closed(&self) -> bool { self.0.closed.get() }

    pub fn window(&self) -> gtk::Window { self.0.window.clone().upcast() }

    pub fn search(&self) -> &gtk::SearchEntry { &self.0.search }

    /// `sheet.lists`: each group column and its list.
    pub fn lists(&self) -> Vec<gtk::Box> { self.0.lists.iter().map(|(c, _)| c.clone()).collect() }
}

pub fn build_page_with(store: Option<Rc<dyn Shortcuts>>) -> SettingsPage {
    let controller = ShortcutsPage::new(store);
    let (close, activate) = (Rc::downgrade(&controller), Rc::downgrade(&controller));
    let page = SettingsPage::new(controller.page.widget.clone())
        .with_close(move || if let Some(c) = close.upgrade() { c.page.close() })
        .with_activate(move || if let Some(c) = activate.upgrade() { c.reload() });
    page.add_target("shortcuts-search", &controller.search);
    page.add_target("shortcuts-list", &controller.container);
    // Python: getattr(controller, 'search') for "shortcuts-overview".
    page.add_target("search", &controller.search);
    page.set_controller(controller);
    page
}
