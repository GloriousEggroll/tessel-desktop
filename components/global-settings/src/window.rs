//! `global_ui.py`: a single, lazy-loaded home for the suite's settings pages.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gs_base::style::{align_page_gutter, empty_state, install, page_header, symbolic_icon, unify_header, HeaderIcon};
use gs_base::{tags, Host, PageHost, Setting, SettingsPage};
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use suite_core::tr;
use suite_gtk::utils::{headerbar, image, key_controller};

use crate::search::{normalize, SearchIndex, Section};

pub const SECTIONS: &[Section] = &[
    ("audio", "Audio", "audio-volume-high-symbolic", "applet_pages"),
    ("bluetooth", "Bluetooth", "bluetooth-active-symbolic", "applet_pages"),
    ("network", "Network", "network-wired-symbolic", "applet_pages"),
    ("displays", "Displays", "video-display-symbolic", "desktop_pages"),
    ("input", "Mouse & Touchpad", "input-mouse-symbolic", "input_page"),
    ("keyboard", "Keyboard", "input-keyboard-symbolic", "keyboard_page"),
    ("shortcuts", "Keyboard Shortcuts", "preferences-desktop-keyboard-shortcuts-symbolic", "shortcuts_page"),
    ("power", "Power", "power-profile-balanced-symbolic", "applet_pages"),
    ("battery", "Battery", "battery-good-symbolic", "applet_pages"),
    ("keepawake", "Keep Awake", "view-reveal-symbolic", "applet_pages"),
    ("notifications", "Notifications", "preferences-system-notifications-symbolic", "applet_pages"),
    ("weather", "Weather", "weather-clear-symbolic", "weather_page"),
    ("theme", "Theme & Wallpaper", "preferences-desktop-wallpaper-symbolic", "desktop_pages"),
    ("layout", "Desktop Layout", "view-dual-symbolic", "layout_page"),
    ("behavior", "Desktop Behavior", "preferences-desktop-symbolic", "dock_pages"),
    ("lock", "Lock Screen", "changes-prevent-symbolic", "lock_page"),
    ("login", "Login Screen", "system-users-symbolic", "login_page"),
    ("dock", "Dock/Panel", "preferences-desktop-display-symbolic", "dock_pages"),
    ("datetime", "Date & Time", "preferences-system-time-symbolic", "applet_pages"),
    ("language", "Language", "preferences-desktop-locale-symbolic", "language_page"),
    ("accessibility", "Accessibility", "preferences-desktop-accessibility-symbolic", "accessibility_page"),
    ("defaultapps", "Default Apps", "applications-system-symbolic", "default_apps_page"),
    ("nightlight", "Night Light", "weather-clear-night-symbolic", "night_light_page"),
    ("users", "Users", "avatar-default-symbolic", "users_page"),
    ("about", "About", "help-about-symbolic", "about_page"),
    ("capture", "Screenshots", "camera-photo-symbolic", "capture_page"),
];

pub fn section_keys() -> Vec<&'static str> { SECTIONS.iter().map(|s| s.0).collect() }

/// `SECTION_ALIASES`.
pub fn alias(section: &str) -> &str {
    if section == "brightness" { "displays" } else { section }
}

/// A symbolic-only icon for a section (sidebar, page header, search).
pub fn section_icon(section: &str, size: i32) -> gtk::Image {
    let icon = SECTIONS.iter().find(|s| s.0 == section).map(|s| s.2).unwrap_or("dialog-warning-symbolic");
    symbolic_icon(icon, size)
}

/// `_build_page`: dispatch to the page crate owning the section.
#[cfg(feature = "pages")]
pub fn default_page(section: &str, host: &Host) -> Result<SettingsPage, String> {
    if gs_applets::SECTIONS.contains(&section) {
        gs_applets::build_page(section, host)
    } else if gs_desktop::SECTIONS.contains(&section) {
        gs_desktop::build_page(section, host)
    } else if gs_system::SECTIONS.contains(&section) {
        gs_system::build_page(section, host)
    } else {
        Err(format!("No module named '{}'", SECTIONS.iter().find(|s| s.0 == section).map(|s| s.3).unwrap_or(section)))
    }
}

#[cfg(not(feature = "pages"))]
pub fn default_page(section: &str, _host: &Host) -> Result<SettingsPage, String> {
    Err(format!("No module named '{}'", SECTIONS.iter().find(|s| s.0 == section).map(|s| s.3).unwrap_or(section)))
}

pub type PageFactory = Rc<dyn Fn(&str, &Host) -> Result<SettingsPage, String>>;

pub struct Inner {
    pub window: gtk::ApplicationWindow,
    pub pages: RefCell<HashMap<String, Rc<SettingsPage>>>,
    pub rows: RefCell<HashMap<String, gtk::ListBoxRow>>,
    pub current_section: RefCell<Option<String>>,
    pub closed: Cell<bool>,
    pub search_index: SearchIndex,
    pub search: gtk::SearchEntry,
    pub navigation: gtk::ListBox,
    pub stack: gtk::Stack,
    pub search_page: gtk::Box,
    pub search_summary: gtk::Label,
    pub search_scroll: gtk::ScrolledWindow,
    pub search_results: gtk::ListBox,
    pub search_empty: gtk::Box,
    search_active: Cell<bool>,
    search_query: RefCell<String>,
    search_updating: Cell<bool>,
    spotlight_source: RefCell<Option<glib::SourceId>>,
    highlight_source: RefCell<Option<glib::SourceId>>,
    pub highlight: RefCell<Option<gtk::Widget>>,
    page_factory: PageFactory,
    style_close: RefCell<Option<Box<dyn Fn()>>>,
}

#[derive(Clone)]
pub struct GlobalSettingsWindow(pub Rc<Inner>);

impl std::ops::Deref for GlobalSettingsWindow {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

/// The page-facing view of the window (`host` in Python).
struct WindowHost(Weak<Inner>);

impl PageHost for WindowHost {
    fn show_section(&self, section: &str) {
        if let Some(inner) = self.0.upgrade() {
            let _ = GlobalSettingsWindow(inner).show_section(section);
        }
    }
    fn application(&self) -> Option<gtk::Application> {
        self.0.upgrade().and_then(|inner| inner.window.application())
    }
    fn window(&self) -> gtk::Window {
        self.0.upgrade().map(|inner| inner.window.clone().upcast()).unwrap_or_else(gtk::Window::new)
    }
    fn show_page_error(&self, section: &str, error: &str, title: Option<&str>, icon: &str) {
        if let Some(inner) = self.0.upgrade() {
            GlobalSettingsWindow(inner).show_page_error(section, error, title, icon);
        }
    }
    fn closed(&self) -> bool {
        self.0.upgrade().map(|inner| inner.closed.get()).unwrap_or(true)
    }
}

fn walk(root: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(root.clone());
    let mut child = root.first_child();
    while let Some(item) = child {
        walk(&item, out);
        child = item.next_sibling();
    }
}

fn widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn translate(text: &str) -> String { suite_core::i18n::translate(text) }

impl GlobalSettingsWindow {
    pub fn new(application: &gtk::Application, page_factory: Option<PageFactory>) -> GlobalSettingsWindow {
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Settings")).build();
        window.set_default_size(1090, 810);
        window.set_size_request(830, 580);
        window.set_titlebar(Some(&headerbar(&tr!("Settings"))));
        let style_close = install(&window);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        window.set_child(Some(&root));
        let navigation_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        navigation_box.add_css_class("settings-navigation");
        navigation_box.add_css_class("suite-sidebar");
        navigation_box.set_size_request(230, -1);
        navigation_box.set_vexpand(true);
        let title = gtk::Label::builder().label(tr!("Settings")).xalign(0.0).build();
        title.add_css_class("navigation-title");
        navigation_box.append(&title);
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search settings")).build();
        search.add_css_class("settings-search");
        search.update_property(&[gtk::accessible::Property::Label(&tr!("Search settings"))]);
        navigation_box.append(&search);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let navigation = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        let mut rows = HashMap::new();
        // Alphabetical by the name shown, so the order holds in every language.
        let mut sorted: Vec<&Section> = SECTIONS.iter().collect();
        sorted.sort_by_key(|s| suite_core::pyjson::casefold(&translate(s.1)));
        for (key, name, _icon, _module) in sorted {
            let row = gtk::ListBoxRow::new();
            tags::set_str(&row, "section", key);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            line.add_css_class("navigation-item");
            line.append(&section_icon(key, 16));
            line.append(&gtk::Label::builder().label(translate(name)).xalign(0.0).build());
            row.set_child(Some(&line));
            navigation.append(&row);
            rows.insert(key.to_string(), row);
        }
        scroll.set_child(Some(&navigation));
        navigation_box.append(&scroll);
        root.append(&navigation_box);
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);
        // Live controls can own timers and popovers; avoid animating two pages at once.
        stack.set_transition_type(gtk::StackTransitionType::None);

        // _build_search_page
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.add_css_class("settings-search-page");
        let heading = page_header(&tr!("Search results"), Some(" "), Some("system-search-symbolic"));
        page.append(&heading.root);
        let search_summary = heading.subtitle.clone().expect("subtitle");
        let search_scroll = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).build();
        search_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let search_results = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        search_results.set_activate_on_single_click(true);
        search_results.add_css_class("settings-search-results");
        search_scroll.set_child(Some(&search_results));
        page.append(&search_scroll);
        let empty = empty_state(&tr!("No settings found"), &tr!("Try another word, such as volume, wallpaper, or sleep."),
                                "system-search-symbolic", None, None);
        empty.root.set_visible(false);
        page.append(&empty.root);
        stack.add_named(&page, Some("__search__"));
        root.append(&stack);

        let inner = Rc::new(Inner {
            window: window.clone(),
            pages: RefCell::new(HashMap::new()),
            rows: RefCell::new(rows),
            current_section: RefCell::new(None),
            closed: Cell::new(false),
            search_index: SearchIndex::new(SECTIONS, &translate),
            search: search.clone(),
            navigation: navigation.clone(),
            stack,
            search_page: page,
            search_summary,
            search_scroll,
            search_results: search_results.clone(),
            search_empty: empty.root,
            search_active: Cell::new(false),
            search_query: RefCell::new(String::new()),
            search_updating: Cell::new(false),
            spotlight_source: RefCell::new(None),
            highlight_source: RefCell::new(None),
            highlight: RefCell::new(None),
            page_factory: page_factory.unwrap_or_else(|| Rc::new(default_page)),
            style_close: RefCell::new(Some(Box::new(style_close))),
        });
        let this = GlobalSettingsWindow(inner);
        let weak = Rc::downgrade(&this.0);
        let get = move || weak.upgrade().map(GlobalSettingsWindow);
        let with = {
            let get = get.clone();
            move |f: &dyn Fn(&GlobalSettingsWindow)| if let Some(this) = get() { f(&this) }
        };
        let w = with.clone();
        let get_c = get.clone();
        window.connect_close_request(move |_| {
            let _ = &w;
            if get_c().is_some_and(|this| this.close_requested()) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        let w = with.clone();
        window.connect_unrealize(move |_| w(&|this| this.cleanup()));
        let w = with.clone();
        let get_k = get.clone();
        key_controller(&window, true, move |_c, key, _code, state| {
            let _ = &w;
            if get_k().is_some_and(|this| this.key(key, state)) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        let w = with.clone();
        search.connect_search_changed(move |_| w(&|this| this.search_changed()));
        let w = with.clone();
        search.connect_activate(move |_| w(&|this| this.activate_search()));
        let w = with.clone();
        search.connect_stop_search(move |_| w(&|this| { this.leave_search(true); }));
        let w = with.clone();
        let get_s = get.clone();
        key_controller(&search, false, move |_c, key, _code, _state| {
            let _ = &w;
            if get_s().is_some_and(|this| this.search_key(key)) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        let w = with.clone();
        navigation.connect_row_selected(move |_, row| {
            let row = row.cloned();
            w(&|this| this.selected(row.as_ref()));
        });
        let w = with.clone();
        navigation.connect_row_activated(move |_, row| {
            let row = row.clone();
            w(&|this| this.selected(Some(&row)));
        });
        let w = with;
        search_results.connect_row_activated(move |_, row| {
            let row = row.clone();
            w(&|this| this.search_result_activated(Some(&row)));
        });
        this
    }

    pub fn host(&self) -> Host { Rc::new(WindowHost(Rc::downgrade(&self.0))) }

    pub fn current_section(&self) -> Option<String> { self.current_section.borrow().clone() }

    pub fn page(&self, section: &str) -> Option<Rc<SettingsPage>> { self.pages.borrow().get(section).cloned() }

    fn current_page(&self) -> Option<Rc<SettingsPage>> {
        self.current_section().and_then(|s| self.page(&s))
    }

    fn search_changed(&self) {
        if self.search_updating.get() || self.closed.get() {
            return;
        }
        let query = normalize(&self.search.text());
        if query == *self.search_query.borrow() {
            return;
        }
        if query.is_empty() {
            self.leave_search(true);
            return;
        }
        self.clear_highlight();
        if !self.search_active.get() {
            if let Some(page) = self.current_page() {
                page.deactivate();
            }
        }
        self.search_active.set(true);
        *self.search_query.borrow_mut() = query.clone();
        while let Some(child) = self.search_results.first_child() {
            self.search_results.remove(&child);
        }
        let results = self.search_index.search_indices(&query);
        for index in &results {
            let setting = &self.search_index.items[*index];
            let row = gtk::ListBoxRow::new();
            tags::set_str(&row, "setting", &index.to_string());
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let section_name = self.search_index.sections[setting.section].0.clone();
            let picture = section_icon(setting.section, 16);
            picture.set_valign(gtk::Align::Start);
            line.append(&picture);
            let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
            let title = gtk::Label::builder().label(translate(setting.title)).xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End).build();
            title.add_css_class("search-setting-title");
            labels.append(&title);
            let section = gtk::Label::builder().label(&section_name).xalign(0.0).build();
            section.add_css_class("search-section");
            labels.append(&section);
            let detail = gtk::Label::builder().label(translate(self.search_index.detail(*index))).xalign(0.0)
                .wrap(true).max_width_chars(66).build();
            detail.add_css_class("search-detail");
            labels.append(&detail);
            line.append(&labels);
            let arrow = image("go-next-symbolic", 16);
            arrow.set_valign(gtk::Align::Center);
            line.append(&arrow);
            row.set_child(Some(&line));
            row.set_tooltip_text(Some(&format!("{} · {}", section_name, translate(setting.title))));
            self.search_results.append(&row);
        }
        self.search_results.unselect_all();
        let count = results.len() as i64;
        self.search_summary.set_text(&if count == 1 {
            tr!("{value1} result", value1 = count)
        } else {
            tr!("{value1} results", value1 = count)
        });
        self.search_empty.set_visible(results.is_empty());
        self.search_scroll.set_visible(!results.is_empty());
        self.stack.set_visible_child_name("__search__");
    }

    fn leave_search(&self, restore: bool) -> bool {
        let active = self.search_active.get();
        self.search_active.set(false);
        self.search_query.borrow_mut().clear();
        self.search_updating.set(true);
        self.search.set_text("");
        self.search_updating.set(false);
        if restore {
            if let Some(section) = self.current_section() {
                self.stack.set_visible_child_name(&section);
                if active {
                    if let Some(page) = self.page(&section) {
                        page.activate();
                    }
                }
            }
        }
        active
    }

    fn activate_search(&self) {
        let row = self.search_results.selected_row().or_else(|| self.search_results.row_at_index(0));
        if self.search_active.get() {
            if let Some(row) = row {
                self.search_result_activated(Some(&row));
            }
        }
    }

    fn search_key(&self, key: gdk::Key) -> bool {
        if key == gdk::Key::Down && self.search_active.get() {
            if let Some(row) = self.search_results.row_at_index(0) {
                self.search_results.select_row(Some(&row));
                row.grab_focus();
                return true;
            }
        }
        false
    }

    /// Activate a search result row (tests call this directly).
    pub fn search_result_activated(&self, row: Option<&gtk::ListBoxRow>) {
        let Some(row) = row else { return };
        let Some(index) = tags::get_str(row, "setting").and_then(|s| s.parse::<usize>().ok()) else { return };
        let setting = self.search_index.items[index].clone();
        let _ = self.show_section(setting.section);
        if setting.key != "__section__" {
            if let Some(page) = self.page(setting.section) {
                let reveal = page.reveal_setting.borrow().clone();
                if let Some(reveal) = reveal {
                    reveal(&setting);
                }
            }
            self.clear_highlight();
            let this = Rc::downgrade(&self.0);
            let id = glib::idle_add_local(move || {
                if let Some(inner) = this.upgrade() {
                    inner.spotlight_source.borrow_mut().take();
                    GlobalSettingsWindow(inner).spotlight(setting.clone(), 0);
                }
                glib::ControlFlow::Break
            });
            *self.spotlight_source.borrow_mut() = Some(id);
        }
    }

    fn spotlight(&self, setting: Setting, attempt: u32) {
        if self.closed.get() || self.search_active.get() || self.current_section().as_deref() != Some(setting.section) {
            return;
        }
        let Some(page) = self.page(setting.section) else { return };
        if !setting.subpage.is_empty() {
            if let Some(tabs) = page.tabs.borrow().as_ref() {
                tabs.set_visible_child_name(setting.subpage);
            }
            for widget in widgets(&page.widget) {
                if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                    if tags::get_str(row, tags::PAGE_NAME).as_deref() == Some(setting.subpage) {
                        if let Some(list) = row.parent().and_then(|p| p.downcast::<gtk::ListBox>().ok()) {
                            list.select_row(Some(row));
                        }
                        break;
                    }
                }
            }
        }
        let mut target = page.search_target(&setting).filter(|w| w.is_mapped());
        if target.is_none() {
            let mut names: Vec<String> = setting.labels.iter().map(|l| normalize(&translate(l))).collect();
            names.push(normalize(&translate(setting.title)));
            for widget in widgets(&page.widget) {
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    if label.is_mapped() && names.contains(&normalize(&label.text())) {
                        target = Some(label.parent().unwrap_or_else(|| label.clone().upcast()));
                        break;
                    }
                }
            }
        }
        let target = match target {
            Some(t) if t.height() > 0 => t,
            _ => {
                // Pages may be mapped before their first allocation, and device
                // controls can populate after asynchronous discovery.
                if attempt < 14 {
                    let this = Rc::downgrade(&self.0);
                    let id = glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                        if let Some(inner) = this.upgrade() {
                            inner.spotlight_source.borrow_mut().take();
                            GlobalSettingsWindow(inner).spotlight(setting.clone(), attempt + 1);
                        }
                        glib::ControlFlow::Break
                    });
                    *self.spotlight_source.borrow_mut() = Some(id);
                }
                return;
            }
        };
        target.add_css_class("settings-search-target");
        *self.highlight.borrow_mut() = Some(target.clone());
        let this = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local(std::time::Duration::from_millis(3500), move || {
            if let Some(inner) = this.upgrade() {
                inner.highlight_source.borrow_mut().take();
                GlobalSettingsWindow(inner).clear_highlight();
            }
            glib::ControlFlow::Break
        });
        *self.highlight_source.borrow_mut() = Some(id);
        for widget in widgets(&target) {
            let focusable = widget.is::<gtk::Switch>() || widget.is::<gtk::Scale>() || widget.is::<gtk::Entry>()
                || widget.is::<gtk::DropDown>() || widget.is::<gtk::SpinButton>() || widget.is::<gtk::CheckButton>()
                || widget.is::<gtk::Button>();
            if focusable && widget.is_sensitive() && widget.is_mapped() {
                widget.grab_focus();
                break;
            }
        }
        let mut parent = target.parent();
        while let Some(item) = parent {
            if let Some(scrolled) = item.downcast_ref::<gtk::ScrolledWindow>() {
                let mut content = scrolled.child();
                if let Some(viewport) = content.as_ref().and_then(|c| c.downcast_ref::<gtk::Viewport>()) {
                    content = viewport.child();
                }
                if let Some(bounds) = content.and_then(|c| target.compute_bounds(&c)) {
                    let adjustment = scrolled.vadjustment();
                    let value = bounds.y() as f64 - adjustment.page_size() * 0.3;
                    adjustment.set_value(adjustment.lower().max(value.min(adjustment.upper() - adjustment.page_size())));
                }
                break;
            }
            parent = item.parent();
        }
    }

    pub fn clear_highlight(&self) {
        if let Some(id) = self.spotlight_source.borrow_mut().take() {
            suite_core::glib_compat::source_remove(id);
        }
        if let Some(id) = self.highlight_source.borrow_mut().take() {
            suite_core::glib_compat::source_remove(id);
        }
        if let Some(widget) = self.highlight.borrow_mut().take() {
            widget.remove_css_class("settings-search-target");
        }
    }

    /// `show_section(section)`; Err for an unknown key (Python ValueError).
    pub fn show_section(&self, section: &str) -> Result<(), String> {
        let section = alias(section);
        let row = self.rows.borrow().get(section).cloned()
            .ok_or_else(|| format!("Unknown settings section: {section}"))?;
        if self.navigation.selected_row().as_ref() == Some(&row) {
            self.selected(Some(&row));
        } else {
            self.navigation.select_row(Some(&row));
        }
        Ok(())
    }

    fn selected(&self, row: Option<&gtk::ListBoxRow>) {
        let Some(row) = row else { return };
        let was_search = self.leave_search(false);
        let Some(section) = tags::get_str(row, "section") else { return };
        if Some(&section) == self.current_section.borrow().as_ref() {
            self.stack.set_visible_child_name(&section);
            if was_search {
                if let Some(page) = self.page(&section) {
                    page.activate();
                }
            }
            return;
        }
        self.clear_highlight();
        if !was_search {
            if let Some(page) = self.current_page() {
                page.deactivate();
            }
        }
        if !self.pages.borrow().contains_key(&section) {
            let page = match (self.page_factory)(&section, &self.host()) {
                Ok(page) => page,
                Err(error) => {
                    eprintln!("ERROR:root:Could not load {section} settings: {error}");
                    self.error_page(&section, &error, None, "dialog-warning-symbolic")
                }
            };
            self.unify(&section, &page);
            self.stack.add_named(&page.widget, Some(&section));
            self.pages.borrow_mut().insert(section.clone(), Rc::new(page));
        }
        *self.current_section.borrow_mut() = Some(section.clone());
        self.stack.set_visible_child_name(&section);
        if let Some(page) = self.page(&section) {
            page.activate();
        }
    }

    /// Give pages built by other components the shared page header.
    fn unify(&self, section: &str, page: &SettingsPage) {
        let header = unify_header(&page.widget, Some(HeaderIcon::Image(section_icon(section, 24))));
        align_page_gutter(&page.widget, header.as_ref());
    }

    fn error_page(&self, section: &str, error: &str, title: Option<&str>, icon: &str) -> SettingsPage {
        let weak = Rc::downgrade(&self.0);
        let key = section.to_owned();
        let again = move || {
            let Some(inner) = weak.upgrade() else { return };
            let this = GlobalSettingsWindow(inner);
            let previous = this.pages.borrow_mut().remove(&key);
            if let Some(previous) = previous {
                previous.close();
                this.stack.remove(&previous.widget);
            }
            *this.current_section.borrow_mut() = None;
            let row = this.rows.borrow().get(&key).cloned();
            this.selected(row.as_ref());
        };
        let title = title.map(str::to_owned).unwrap_or_else(|| tr!("Settings are unavailable"));
        let state = empty_state(&title, error, icon, Some(&tr!("Try Again")), Some(Box::new(again)));
        state.root.add_css_class("settings-error");
        // The section keeps its usual header above the error.
        let name = SECTIONS.iter().find(|s| s.0 == section).map(|s| s.1).unwrap_or(section);
        let header = page_header(&translate(name), None, None);
        header.root.prepend(&section_icon(section, 24));
        let first = header.root.first_child().unwrap();
        first.add_css_class("gs-page-icon");
        first.set_valign(gtk::Align::Center);
        header.root.set_margin_top(24);
        header.root.set_margin_start(24);
        header.root.set_margin_end(24);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&header.root);
        root.append(&state.root);
        let page = SettingsPage::new(root);
        *page.error_state.borrow_mut() = Some(state.root);
        page
    }

    /// Replace a loaded page with the shared error state.
    pub fn show_page_error(&self, section: &str, error: &str, title: Option<&str>, icon: &str) -> Option<Rc<SettingsPage>> {
        let previous = self.page(section)?;
        if self.closed.get() {
            return None;
        }
        let page = Rc::new(self.error_page(section, error, title, icon));
        let visible = self.stack.visible_child().as_ref() == Some(&previous.widget);
        previous.close();
        self.stack.remove(&previous.widget);
        self.pages.borrow_mut().insert(section.to_owned(), page.clone());
        self.stack.add_named(&page.widget, Some(section));
        if visible {
            self.stack.set_visible_child_name(section);
        }
        Some(page)
    }

    /// `_close_requested`: true = refuse the close.
    fn close_requested(&self) -> bool {
        let pages: Vec<(String, Rc<SettingsPage>)> = self.pages.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        // Python iterates dict insertion order; keep the order pages were opened.
        let order = self.opened_order(pages);
        for (key, page) in order {
            if !page.can_close() {
                let _ = self.show_section(&key);
                return true;
            }
        }
        self.cleanup();
        false
    }

    fn opened_order(&self, pages: Vec<(String, Rc<SettingsPage>)>) -> Vec<(String, Rc<SettingsPage>)> {
        // Stack children are in the order pages were added (after __search__).
        let mut ordered = Vec::new();
        let mut child = self.stack.first_child();
        while let Some(item) = child {
            if let Some((key, page)) = pages.iter().find(|(_, p)| p.widget == item) {
                ordered.push((key.clone(), page.clone()));
            }
            child = item.next_sibling();
        }
        ordered
    }

    pub fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if (key == gdk::Key::f || key == gdk::Key::F) && state.contains(gdk::ModifierType::CONTROL_MASK) {
            self.search.grab_focus();
            self.search.select_region(0, -1);
            return true;
        }
        if key == gdk::Key::Escape {
            if self.search_active.get() || !self.search.text().is_empty() {
                self.leave_search(true);
                return true;
            }
            self.window.close();
            return true;
        }
        false
    }

    pub fn cleanup(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.clear_highlight();
        let pages: Vec<(String, Rc<SettingsPage>)> = self.pages.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (_key, page) in self.opened_order(pages) {
            page.close();
        }
        if let Some(close) = self.style_close.borrow_mut().take() {
            close();
        }
    }

    /// Submit a completed query (tests): like Python's tests, the entry's
    /// intermediate empty text while replacing is not seen as a search.
    pub fn set_search(&self, text: &str) {
        self.search_updating.set(true);
        self.search.set_text(text);
        self.search_updating.set(false);
        self.search_changed();
    }

    pub fn search_active(&self) -> bool { self.search_active.get() }
}
