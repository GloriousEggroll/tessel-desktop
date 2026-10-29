//! keyboard_page.py: physical layouts, XKB options, key repeat, and the on-screen keyboard.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gs_base::style::{find_title, TITLE_CLASSES};
use gs_base::widgets::{dropdown, label, status_text, Page};
use gs_base::SettingsPage;
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use suite_core::tr;
use suite_gtk::utils::{image, key_controller};

use crate::keyboard_config::{load_catalog, Catalog, KeyboardConfig, KeyboardState};

/// Searchable list of every XKB layout and variant.
pub struct LayoutChooser {
    pub window: gtk::Window,
    pub search: gtk::SearchEntry,
    pub list: gtk::ListBox,
    pub add_button: gtk::Button,
    values: RefCell<Vec<((String, String), String)>>,
    chosen: Box<dyn Fn((String, String))>,
}

impl LayoutChooser {
    pub fn new(parent: Option<&gtk::Window>, catalog: &Catalog, chosen: impl Fn((String, String)) + 'static) -> Rc<LayoutChooser> {
        let window = gtk::Window::builder().title(tr!("Add Keyboard Layout")).modal(true).build();
        window.set_transient_for(parent);
        window.set_default_size(460, 560);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10)
            .margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
        window.set_child(Some(&content));
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search layouts")).build();
        search.update_property(&[gtk::accessible::Property::Label(&tr!("Search layouts"))]);
        content.append(&search);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        scroll.set_child(Some(&list));
        content.append(&scroll);
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        let add_button = gtk::Button::with_label(&tr!("Add"));
        add_button.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&add_button);
        content.append(&buttons);
        let this = Rc::new(LayoutChooser { window, search, list, add_button, values: Default::default(), chosen: Box::new(chosen) });
        for (code, name) in &catalog.layouts {
            this.add(code, "", name, false);
            for (variant, variant_name) in catalog.variants.get(code).map(Vec::as_slice).unwrap_or(&[]) {
                this.add(code, variant, variant_name, true);
            }
        }
        let weak = Rc::downgrade(&this);
        this.list.set_filter_func(move |row| weak.upgrade().map(|t| t.filter(row)).unwrap_or(true));
        let list = this.list.clone();
        this.search.connect_search_changed(move |_| list.invalidate_filter());
        let weak = Rc::downgrade(&this);
        this.search.connect_activate(move |_| if let Some(t) = weak.upgrade() { t.choose(t.first_visible()) });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| if let Some(t) = weak.upgrade() { t.choose(Some(row.clone())) });
        let w = this.window.clone();
        cancel.connect_clicked(move |_| w.close());
        let weak = Rc::downgrade(&this);
        this.add_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.choose(t.list.selected_row()) });
        let w = this.window.clone();
        key_controller(&this.window, false, move |_, key, _, _| {
            if key == gdk::Key::Escape {
                w.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        let keep = RefCell::new(Some(this.clone()));
        this.window.connect_destroy(move |_| { keep.borrow_mut().take(); });
        this
    }

    fn add(&self, code: &str, variant: &str, name: &str, indent: bool) {
        let row = gtk::ListBoxRow::new();
        gs_base::tags::set_str(&row, "index", &self.values.borrow().len().to_string());
        let terms = crate::compat::casefold(&[code, variant, name].join(" "));
        self.values.borrow_mut().push(((code.into(), variant.into()), terms));
        let text = gtk::Label::builder().label(name).xalign(0.0).ellipsize(pango::EllipsizeMode::End)
            .margin_start(if indent { 24 } else { 6 }).margin_top(6).margin_bottom(6).build();
        row.set_child(Some(&text));
        self.list.append(&row);
    }

    fn index(row: &gtk::ListBoxRow) -> usize { gs_base::tags::get_str(row, "index").and_then(|s| s.parse().ok()).unwrap_or(0) }

    fn filter(&self, row: &gtk::ListBoxRow) -> bool {
        let query = crate::compat::casefold(&self.search.text());
        let values = self.values.borrow();
        let terms = &values[Self::index(row)].1;
        crate::compat::split_whitespace(&query).iter().all(|w| terms.contains(w))
    }

    fn first_visible(&self) -> Option<gtk::ListBoxRow> {
        let mut row = self.list.first_child();
        while let Some(item) = row {
            if item.is_child_visible() {
                return item.downcast().ok();
            }
            row = item.next_sibling();
        }
        None
    }

    fn choose(&self, row: Option<gtk::ListBoxRow>) {
        if let Some(row) = row {
            let value = self.values.borrow()[Self::index(&row)].0.clone();
            (self.chosen)(value);
            self.window.close();
        }
    }

    pub fn present(&self) { self.window.present() }
}

/// The embedded on-screen keyboard settings (`osk_settings.KeyboardSettings()`).
pub struct OskEmbed {
    pub widget: gtk::Widget,
    pub search_targets: Vec<(String, gtk::Widget)>,
    pub close: Box<dyn Fn()>,
}

pub type OskFactory = Box<dyn FnOnce() -> Result<OskEmbed, String>>;

pub fn default_osk() -> Result<OskEmbed, String> {
    let settings = on_screen_keyboard::settings::KeyboardSettings::new(None);
    let mut targets: Vec<(String, gtk::Widget)> = settings.search_targets().into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    targets.sort_by(|a, b| a.0.cmp(&b.0));
    let widget = settings.root().clone().upcast();
    Ok(OskEmbed { widget, search_targets: targets, close: Box::new(move || settings.close()) })
}

pub struct KeyboardPage {
    pub page: Page,
    pub config: KeyboardConfig,
    pub catalog: RefCell<Catalog>,
    pub state: RefCell<KeyboardState>,
    loading: Cell<bool>,
    save_source: RefCell<Option<glib::SourceId>>,
    pub layout_list: gtk::ListBox,
    pub add_button: gtk::Button,
    pub switch_options: RefCell<Vec<(String, String)>>,
    pub switch: gtk::DropDown,
    pub compose_options: RefCell<Vec<(String, String)>>,
    pub compose: gtk::DropDown,
    pub delay: gtk::Scale,
    pub rate: gtk::Scale,
    pub test: gtk::Entry,
    pub status: gtk::Label,
    pub osk: RefCell<Option<OskEmbed>>,
    pub search_targets: RefCell<Vec<(String, gtk::Widget)>>,
}

impl KeyboardPage {
    pub fn new(config: Option<KeyboardConfig>, catalog: Option<Catalog>, osk_factory: Option<OskFactory>) -> Rc<KeyboardPage> {
        let page = Page::new("keyboard-page", &tr!("Keyboard"), &tr!("Choose layouts and typing behavior for physical keyboards."),
                             Some("input-keyboard-symbolic"));
        let config = config.unwrap_or_else(|| KeyboardConfig::new(None, None, None));
        let provided = catalog.is_some();
        let (state, load_error) = match config.load() {
            Ok(state) => (state, None),
            Err(error) => (KeyboardState::default(), Some(error)),
        };
        let layouts = page.group(Some(&tr!("Layouts")),
                                 Some(&tr!("The first layout is used when you sign in. Use the switch shortcut to change layouts while typing.")), None);
        let layout_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        layouts.append(&layout_list);
        let add_button = gtk::Button::builder().label(tr!("Add Layout…")).halign(gtk::Align::Start).build();
        add_button.add_css_class("pill");
        layouts.append(&add_button);
        let switch = dropdown(&[tr!("None")], 0);
        Page::row(&layouts, &tr!("Switch layouts with"), Some(switch.upcast_ref()), Some(&tr!("Keys that change to the next keyboard layout.")));
        let compose = dropdown(&[tr!("None")], 0);
        Page::row(&layouts, &tr!("Compose key"), Some(compose.upcast_ref()),
                  Some(&tr!("Press the compose key, then a sequence such as e and ' to type é.")));
        let repeat = page.group(Some(&tr!("Key repeat")), None, None);
        let delay = gtk::Scale::with_range(gtk::Orientation::Horizontal, 150.0, 1500.0, 10.0);
        delay.set_size_request(260, -1);
        delay.set_value(state.repeat_delay as f64);
        delay.set_format_value_func(|_, value| tr!("{value1} ms", value1 = value as i64));
        delay.set_draw_value(true);
        Page::row(&repeat, &tr!("Repeat delay"), Some(delay.upcast_ref()), Some(&tr!("How long to hold a key before it repeats.")));
        let rate = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 100.0, 1.0);
        rate.set_size_request(260, -1);
        rate.set_value(state.repeat_rate as f64);
        rate.set_format_value_func(|_, value| tr!("{value1} per second", value1 = value as i64));
        rate.set_draw_value(true);
        Page::row(&repeat, &tr!("Repeat rate"), Some(rate.upcast_ref()), Some(&tr!("How quickly a held key repeats.")));
        let test = gtk::Entry::builder().placeholder_text(tr!("Type here to test")).build();
        repeat.append(&test);
        let status = label("", Some("detail"));
        page.content.append(&status);
        page.content.append(&gtk::Separator::builder().margin_top(8).build());
        let none = vec![(String::new(), tr!("None"))];
        let this = Rc::new(KeyboardPage {
            page, config, catalog: RefCell::new(catalog.unwrap_or_default()), state: RefCell::new(state), loading: Cell::new(true),
            save_source: RefCell::new(None), layout_list, add_button, switch_options: RefCell::new(none.clone()), switch,
            compose_options: RefCell::new(none), compose, delay, rate, test, status, osk: RefCell::new(None), search_targets: Default::default(),
        });
        match osk_factory.unwrap_or_else(|| Box::new(default_osk))() {
            Ok(osk) => {
                osk.widget.set_margin_top(0);
                osk.widget.set_margin_bottom(0);
                osk.widget.set_margin_start(0);
                osk.widget.set_margin_end(0);
                this.page.content.append(&osk.widget);
                // Its own page title is a section here; one page header per page.
                if let Some(title) = find_title(&osk.widget, 9) {
                    for name in TITLE_CLASSES {
                        title.remove_css_class(name);
                    }
                    title.add_css_class("group-title");
                    if let Some(row) = title.parent().filter(|r| *r != osk.widget) {
                        if let Some(child) = row.first_child() {
                            let image_child = child.is::<gtk::Image>()
                                || (child != *title.upcast_ref::<gtk::Widget>() && child.first_child().is_some_and(|c| c.is::<gtk::Image>()));
                            if image_child {
                                child.set_visible(false);
                            }
                        }
                    }
                }
                *this.osk.borrow_mut() = Some(osk);
            }
            Err(error) => {
                glib::g_info!("global-settings", "On-screen keyboard settings unavailable: {}", error);
                this.page.content.append(&label(&tr!("On-screen Keyboard"), Some("group-title")));
                this.page.content.append(&label(&tr!("The on-screen keyboard is not installed."), Some("detail")));
            }
        }
        {
            let mut targets = this.search_targets.borrow_mut();
            targets.extend([("keyboard-layouts".to_owned(), this.layout_list.clone().upcast::<gtk::Widget>()),
                            ("keyboard-switch".into(), this.switch.clone().upcast()), ("keyboard-compose".into(), this.compose.clone().upcast()),
                            ("keyboard-repeat-delay".into(), this.delay.clone().upcast()), ("keyboard-repeat-rate".into(), this.rate.clone().upcast())]);
            if let Some(osk) = this.osk.borrow().as_ref() {
                for (key, widget) in &osk.search_targets {
                    // Search metadata names on-screen keyboard controls without the prefix.
                    targets.push((key.clone(), widget.clone()));
                    targets.push((key.strip_prefix("keyboard-").unwrap_or(key).to_owned(), widget.clone()));
                }
            }
        }
        let weak = Rc::downgrade(&this);
        this.add_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.choose_layout() });
        let weak = Rc::downgrade(&this);
        this.switch.connect_selected_notify(move |w| if let Some(t) = weak.upgrade() { t.option_changed(w, "grp", false) });
        let weak = Rc::downgrade(&this);
        this.compose.connect_selected_notify(move |w| if let Some(t) = weak.upgrade() { t.option_changed(w, "compose", true) });
        let weak = Rc::downgrade(&this);
        this.delay.connect_value_changed(move |_| if let Some(t) = weak.upgrade() { t.repeat_changed() });
        let weak = Rc::downgrade(&this);
        this.rate.connect_value_changed(move |_| if let Some(t) = weak.upgrade() { t.repeat_changed() });
        this.render_layouts();
        this.fill_options();
        this.loading.set(false);
        if let Some(error) = load_error {
            status_text(&this.status, &tr!("Keyboard settings could not be read: {value1}", value1 = error.to_string()), Some("error"));
        }
        if !provided {
            this.add_button.set_sensitive(false);
            let weak = Rc::downgrade(&this);
            let language = suite_core::i18n::language();
            this.page.run(move || Ok(load_catalog(&language)), move |result| if let Some(t) = weak.upgrade() { t.catalog_loaded(result.ok()) });
        }
        this
    }

    fn catalog_loaded(self: &Rc<Self>, catalog: Option<Catalog>) {
        if let Some(catalog) = catalog {
            *self.catalog.borrow_mut() = catalog;
        }
        self.add_button.set_sensitive(true);
        self.loading.set(true);
        self.render_layouts();
        self.fill_options();
        self.loading.set(false);
    }

    fn fill_options(&self) {
        for (attribute, widget, group) in [(&self.switch_options, &self.switch, "grp"), (&self.compose_options, &self.compose, "compose")] {
            let mut options = vec![(String::new(), tr!("None"))];
            options.extend(self.catalog.borrow().options.get(group).cloned().unwrap_or_default());
            let current = self.state.borrow().option(group);
            if !current.is_empty() && !options.iter().any(|(v, _)| *v == current) {
                options.push((current.clone(), current.clone()));
            }
            let names: Vec<&str> = options.iter().map(|(_, n)| n.as_str()).collect();
            widget.set_model(Some(&gtk::StringList::new(&names)));
            // dict(options)[…]: the first matching value wins in next().
            widget.set_selected(options.iter().position(|(v, _)| *v == current).unwrap_or(0) as u32);
            *attribute.borrow_mut() = options;
        }
    }

    pub fn render_layouts(self: &Rc<Self>) {
        suite_gtk::utils::remove_children(&self.layout_list);
        let layouts = self.state.borrow().layouts.clone();
        let count = layouts.len();
        for (index, (layout, variant)) in layouts.iter().enumerate() {
            let row = gtk::Box::builder().spacing(8).margin_top(4).margin_bottom(4).build();
            row.append(&image("input-keyboard-symbolic", 16));
            let name = self.catalog.borrow().layout_name(layout, variant);
            row.append(&gtk::Label::builder().label(&name).xalign(0.0).hexpand(true).ellipsize(pango::EllipsizeMode::End).build());
            for (icon, tooltip, enabled, offset) in [("go-up-symbolic", tr!("Move up"), index > 0, -1i32),
                                                     ("go-down-symbolic", tr!("Move down"), index + 1 < count, 1)] {
                let button = gtk::Button::builder().icon_name(icon).sensitive(enabled).tooltip_text(&tooltip).build();
                button.add_css_class("flat");
                button.update_property(&[gtk::accessible::Property::Label(&format!("{tooltip} · {name}"))]);
                let weak = Rc::downgrade(self);
                button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.move_layout(index, offset) });
                row.append(&button);
            }
            let remove = gtk::Button::builder().icon_name("list-remove-symbolic").sensitive(count > 1).tooltip_text(tr!("Remove")).build();
            remove.add_css_class("flat");
            remove.update_property(&[gtk::accessible::Property::Label(&format!("{} · {name}", tr!("Remove")))]);
            let weak = Rc::downgrade(self);
            remove.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.remove_layout(index) });
            row.append(&remove);
            self.layout_list.append(&row);
        }
        self.add_button.set_visible(count < 4);
    }

    pub fn move_layout(self: &Rc<Self>, index: usize, offset: i32) {
        {
            let mut state = self.state.borrow_mut();
            let other = (index as i32 + offset) as usize;
            state.layouts.swap(index, other);
        }
        self.render_layouts();
        self.save(true);
    }

    pub fn remove_layout(self: &Rc<Self>, index: usize) {
        self.state.borrow_mut().layouts.remove(index);
        self.render_layouts();
        self.save(true);
    }

    pub fn add_layout(self: &Rc<Self>, value: (String, String)) {
        {
            let state = self.state.borrow();
            if state.layouts.contains(&value) || state.layouts.len() >= 4 {
                return;
            }
        }
        self.state.borrow_mut().layouts.push(value);
        self.render_layouts();
        self.save(true);
    }

    fn choose_layout(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let root = self.page.widget.root().and_downcast::<gtk::Window>();
        LayoutChooser::new(root.as_ref(), &self.catalog.borrow(), move |value| if let Some(t) = weak.upgrade() { t.add_layout(value) }).present();
    }

    fn option_changed(self: &Rc<Self>, widget: &gtk::DropDown, group: &str, compose: bool) {
        if self.loading.get() {
            return;
        }
        let options = if compose { self.compose_options.borrow().clone() } else { self.switch_options.borrow().clone() };
        let Some((value, _)) = options.get(widget.selected() as usize).cloned() else { return };
        let state = self.state.borrow().with_option(group, &value);
        *self.state.borrow_mut() = state;
        self.save(true);
    }

    fn repeat_changed(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.repeat_rate = self.rate.value() as i64;
            state.repeat_delay = self.delay.value() as i64;
        }
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(450), move || if let Some(t) = weak.upgrade() {
            t.save_source.borrow_mut().take();
            t.save(false);
        });
        *self.save_source.borrow_mut() = Some(id);
    }

    pub fn save(self: &Rc<Self>, layout: bool) {
        let closed = self.page.closed_flag();
        let status = self.status.clone();
        let done: crate::labwc_settings::Done = Box::new(move |okay, _message| {
            if closed.get() {
                return;
            }
            if !okay {
                status_text(&status, &tr!("Saved, but the desktop could not reload its configuration."), Some("warning"));
            } else if layout {
                status_text(&status, &tr!("Saved. Layout changes apply after you sign out and back in."), None);
            } else {
                status_text(&status, &tr!("Saved. Key repeat changes apply immediately."), None);
            }
        });
        let state = self.state.borrow().clone();
        if let Err(error) = self.config.save(&state, Some(done)) {
            status_text(&self.status, &tr!("Keyboard settings could not be saved: {value1}", value1 = error.to_string()), Some("error"));
        }
    }

    pub fn close(self: &Rc<Self>) {
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
            self.save(false);
        }
        if let Some(osk) = self.osk.borrow().as_ref() {
            (osk.close)();
        }
        self.page.close();
    }
}

pub fn build_page_with(config: Option<KeyboardConfig>, catalog: Option<Catalog>, osk_factory: Option<OskFactory>) -> SettingsPage {
    let controller = KeyboardPage::new(config, catalog, osk_factory);
    let weak = Rc::downgrade(&controller);
    let page = SettingsPage::new(controller.page.widget.clone()).with_close(move || if let Some(c) = weak.upgrade() { c.close() });
    let targets: HashMap<String, gtk::Widget> = controller.search_targets.borrow().iter().cloned().collect();
    for (key, widget) in targets {
        page.add_target(&key, &widget);
    }
    page.set_controller(controller);
    page
}
