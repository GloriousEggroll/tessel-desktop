//! `welcome_ui.WelcomeWindow`: language, keyboard, layout, light/dark and
//! shortcuts. Each settings step is the Global Settings page that owns it.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use gs_base::style::{card, install, page_header, symbolic_icon, unify_header, HeaderIcon};
use gs_base::{tags, Host, PageHost, SettingsPage};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use suite_core::tr;
use suite_gtk::utils::{headerbar, image, key_controller};

use crate::state::{self, InstallStatus};

pub const APPLICATION_ID: &str = "org.waydock.Welcome";

/// (key, step name shown in the sidebar, icon)
pub const STEPS: &[(&str, &str, &str)] = &[
    ("welcome", "Welcome", "user-home-symbolic"),
    ("language", "Language", "preferences-desktop-locale-symbolic"),
    ("keyboard", "Keyboard", "input-keyboard-symbolic"),
    ("layout", "Desktop Layout", "view-dual-symbolic"),
    ("style", "Light or Dark", "preferences-desktop-appearance-symbolic"),
    ("shortcuts", "Shortcuts", "preferences-desktop-keyboard-shortcuts-symbolic"),
    ("done", "Done", "emblem-ok-symbolic"),
];

pub fn step_keys() -> Vec<&'static str> { STEPS.iter().map(|s| s.0).collect() }

/// Pages embedded from Global Settings; the section matches its --section name.
pub const SETTINGS_SECTIONS: &[(&str, &str)] = &[("language", "language_page"), ("keyboard", "keyboard_page"), ("layout", "layout_page")];

/// The suite's default shortcuts (session-startup and capture-manager).
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Super_L", "Open the application menu"),
    ("W-l", "Lock the screen"),
    ("W-slash", "Show all keyboard shortcuts"),
    ("C-A-Delete", "Session menu: lock, sign out, restart or shut down"),
    ("Print", "Take a screenshot"),
    ("A-Tab", "Switch between windows"),
    ("W-Return", "Open a terminal"),
];

/// Standard symbolic names present in both Adwaita and Breeze.
pub const COLOR_SCHEMES: &[(&str, &str, &str)] = &[
    ("prefer-light", "Light", "weather-clear-symbolic"),
    ("prefer-dark", "Dark", "weather-clear-night-symbolic"),
    ("default", "Automatic", "view-refresh-symbolic"),
];

pub const CSS: &str = "
.welcome .welcome-steps { background-color: alpha(@theme_fg_color, .025);
    border-right: 1px solid alpha(@theme_fg_color, .10); padding: 18px 0; }
.welcome .welcome-steps row { border-radius: 8px; margin: 2px 9px; padding: 9px 12px; }
.welcome .welcome-steps row:selected { background-color: alpha(@theme_selected_bg_color, .17); color: @theme_fg_color; }
.welcome .welcome-steps row:selected image { color: @theme_selected_bg_color; }
.welcome .welcome-step-done { color: alpha(@theme_fg_color, .5); }
.welcome .welcome-hero { font-size: 22pt; font-weight: 700; }
.welcome .welcome-lead { font-size: 13pt; color: alpha(@theme_fg_color, .78); }
.welcome .welcome-actions { padding: 12px 18px; border-top: 1px solid alpha(@theme_fg_color, .10); }
.welcome .welcome-actions button { padding: 8px 18px; border-radius: 8px; }
.welcome .welcome-key { font-family: monospace; font-weight: 600; padding: 4px 10px; border-radius: 6px;
    background-color: alpha(@theme_fg_color, .08); border: 1px solid alpha(@theme_fg_color, .14); }
.welcome .welcome-scheme { padding: 18px 26px; border-radius: 12px; }
.welcome .welcome-scheme:checked { background-color: alpha(@theme_selected_bg_color, .22); }
.welcome .welcome-warning { border-color: alpha(@warning_color, .6); }
";

/// The shared `org.gnome.desktop.interface color-scheme` (fakeable).
pub trait ColorSettings {
    fn get(&self) -> String;
    fn set(&self, value: &str) -> bool;
    fn connect_changed(&self, callback: Rc<dyn Fn()>) -> u64;
    fn disconnect(&self, handler: u64);
}

pub struct GioColorSettings {
    settings: gio::Settings,
    handlers: RefCell<HashMap<u64, glib::SignalHandlerId>>,
    next: Cell<u64>,
}

impl GioColorSettings {
    pub fn new(settings: gio::Settings) -> Self {
        GioColorSettings { settings, handlers: RefCell::default(), next: Cell::new(1) }
    }
}

impl ColorSettings for GioColorSettings {
    fn get(&self) -> String { self.settings.string("color-scheme").to_string() }
    fn set(&self, value: &str) -> bool { self.settings.set_string("color-scheme", value).is_ok() }
    fn connect_changed(&self, callback: Rc<dyn Fn()>) -> u64 {
        let id = self.settings.connect_changed(Some("color-scheme"), move |_, _| callback());
        let n = self.next.get();
        self.next.set(n + 1);
        self.handlers.borrow_mut().insert(n, id);
        n
    }
    fn disconnect(&self, handler: u64) {
        if let Some(id) = self.handlers.borrow_mut().remove(&handler) {
            self.settings.disconnect(id);
        }
    }
}

pub type PageFactory = Rc<dyn Fn(&str, &Host) -> Result<SettingsPage, String>>;
pub type Launch = Rc<dyn Fn(Vec<String>)>;

/// Build the Global Settings page for this step.
#[cfg(feature = "pages")]
pub fn default_page_factory(section: &str, host: &Host) -> Result<SettingsPage, String> {
    match section {
        "language" => gs_applets::build_page(section, host),
        "keyboard" => gs_system::build_page(section, host),
        "layout" => gs_desktop::build_page(section, host),
        other => Err(format!("No module named '{other}'")),
    }
}

#[cfg(not(feature = "pages"))]
pub fn default_page_factory(section: &str, _host: &Host) -> Result<SettingsPage, String> {
    let module = SETTINGS_SECTIONS.iter().find(|s| s.0 == section).map(|s| s.1).unwrap_or(section);
    Err(format!("No module named '{module}'"))
}

#[cfg(feature = "pages")]
fn interface_settings() -> Option<Rc<dyn ColorSettings>> {
    gs_desktop::interface_settings().map(|s| Rc::new(GioColorSettings::new(s)) as Rc<dyn ColorSettings>)
}

#[cfg(not(feature = "pages"))]
fn interface_settings() -> Option<Rc<dyn ColorSettings>> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup("org.gnome.desktop.interface", true)?;
    schema.has_key("color-scheme").then(|| {
        Rc::new(GioColorSettings::new(gio::Settings::new("org.gnome.desktop.interface"))) as Rc<dyn ColorSettings>
    })
}

#[cfg(feature = "pages")]
fn display_key(key: &str) -> String {
    gs_system::shortcuts_model::display_key_with(key, &|t: &str| suite_core::i18n::translate(t))
}

/// `shortcuts_model.display_key(key, tr)` (fallback copy without gs-system).
#[cfg(not(feature = "pages"))]
fn display_key(key: &str) -> String {
    let translate = |t: &str| suite_core::i18n::translate(t);
    let modifiers = |part: &str| -> String {
        match part.to_uppercase().as_str() {
            "W" => "Super", "C" => "Ctrl", "A" => "Alt", "S" => "Shift", "H" => "Hyper", "M" => "Meta",
            _ => return part.to_owned(),
        }.to_owned()
    };
    let parts: Vec<&str> = key.split('-').collect();
    let mut names: Vec<String> = parts[..parts.len() - 1].iter().map(|p| translate(&modifiers(p))).collect();
    let symbol = parts.last().copied().unwrap_or(key);
    let pretty = match symbol {
        "Super_L" | "Super_R" => Some("Super"), "Return" => Some("Enter"), "slash" => Some("/"),
        "Print" => Some("Print Screen"), "Tab" => None, _ => None,
    };
    let translated = pretty.map(|p| p.chars().count() > 1).unwrap_or(false);
    let mut symbol = pretty.unwrap_or(symbol).to_owned();
    if symbol.chars().count() == 1 {
        symbol = symbol.to_uppercase();
    }
    names.push(if translated { translate(&symbol) } else { symbol });
    names.join(" + ")
}

/// Options of `WelcomeWindow(application, page_factory=, color_settings=, status=, done_path=, launch=, first_run=)`.
#[derive(Default)]
pub struct Options {
    pub page_factory: Option<PageFactory>,
    /// `Some(None)` = explicitly no colour settings.
    pub color_settings: Option<Option<Rc<dyn ColorSettings>>>,
    /// `Some(None)` = no install status file.
    pub status: Option<Option<InstallStatus>>,
    pub done_path: Option<PathBuf>,
    pub launch: Option<Launch>,
    pub first_run: bool,
}

pub struct Inner {
    pub window: gtk::ApplicationWindow,
    pub pages: RefCell<Vec<(String, Rc<SettingsPage>)>>,
    pub rows: HashMap<&'static str, gtk::ListBoxRow>,
    pub current: RefCell<Option<&'static str>>,
    pub closed: Cell<bool>,
    pub finished: Cell<bool>,
    updating: Cell<bool>,
    scheme_handler: Cell<u64>,
    pub step_list: gtk::ListBox,
    pub stack: gtk::Stack,
    pub back_button: gtk::Button,
    pub next_button: gtk::Button,
    pub progress: gtk::Label,
    page_factory: PageFactory,
    pub color_settings: RefCell<Option<Rc<dyn ColorSettings>>>,
    pub status: Option<InstallStatus>,
    done_path: Option<PathBuf>,
    launch: Launch,
    pub first_run: bool,
    provider: gtk::CssProvider,
    style_close: RefCell<Option<Box<dyn Fn()>>>,
    pub scheme_buttons: RefCell<HashMap<&'static str, gtk::ToggleButton>>,
    pub scheme_message: RefCell<Option<gtk::Label>>,
    pub shortcut_rows: RefCell<Vec<(String, String)>>,
    pub warning: RefCell<Option<gtk::Box>>,
    fallbacks: RefCell<Vec<String>>,
}

#[derive(Clone)]
pub struct WelcomeWindow(pub Rc<Inner>);

impl std::ops::Deref for WelcomeWindow {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

struct WelcomeHost(Weak<Inner>);

impl PageHost for WelcomeHost {
    fn show_section(&self, section: &str) {
        // Python pages would call WelcomeWindow.show_section, which does not exist.
        eprintln!("WARNING:root:Welcome cannot show the {section} settings section");
    }
    fn application(&self) -> Option<gtk::Application> { self.0.upgrade().and_then(|i| i.window.application()) }
    fn window(&self) -> gtk::Window {
        self.0.upgrade().map(|i| i.window.clone().upcast()).unwrap_or_else(gtk::Window::new)
    }
    fn show_page_error(&self, section: &str, error: &str, _title: Option<&str>, _icon: &str) {
        eprintln!("ERROR:root:Could not load the {section} page: {error}");
    }
    fn closed(&self) -> bool { self.0.upgrade().map(|i| i.closed.get()).unwrap_or(true) }
}

fn label(text: &str, css: Option<&str>, wrap: bool) -> gtk::Label {
    let widget = gtk::Label::builder().label(text).xalign(0.0).wrap(wrap).build();
    if wrap {
        widget.set_wrap_mode(pango::WrapMode::WordChar);
    }
    for name in css.unwrap_or("").split_whitespace() {
        widget.add_css_class(name);
    }
    widget
}

fn scrolled(content: &gtk::Box) -> gtk::ScrolledWindow {
    let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(content));
    scroll
}

fn content(spacing: i32) -> gtk::Box {
    gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(spacing)
        .margin_top(24).margin_bottom(24).margin_start(24).margin_end(24).build()
}

#[cfg(feature = "pages")]
type Wizard = language_manager::ui::LanguageWizard;

impl WelcomeWindow {
    pub fn new(application: &gtk::Application, options: Options) -> WelcomeWindow {
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Welcome")).build();
        window.set_default_size(1000, 740);
        window.set_size_request(760, 560);
        window.set_titlebar(Some(&headerbar(&tr!("Welcome"))));
        let style_close = install(&window);
        window.add_css_class("welcome");
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &provider,
                                                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let color_settings = match options.color_settings {
            Some(value) => value,
            None => interface_settings(),
        };
        let status = match options.status {
            Some(value) => value,
            None => state::install_status(None),
        };
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        window.set_child(Some(&root));
        let step_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        step_list.add_css_class("welcome-steps");
        step_list.set_size_request(220, -1);
        let mut rows = HashMap::new();
        for (key, name, icon) in STEPS {
            let row = gtk::ListBoxRow::new();
            tags::set_str(&row, "step", key);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            line.append(&symbolic_icon(icon, 16));
            line.append(&gtk::Label::builder().label(suite_core::i18n::translate(name)).xalign(0.0).build());
            row.set_child(Some(&line));
            step_list.append(&row);
            rows.insert(*key, row);
        }
        root.append(&step_list);
        let main = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).vexpand(true).build();
        root.append(&main);
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        stack.set_transition_type(gtk::StackTransitionType::None);
        main.append(&stack);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        actions.add_css_class("welcome-actions");
        let back_button = gtk::Button::with_label(&tr!("Back"));
        actions.append(&back_button);
        let progress = label("", Some("gs-muted"), true);
        progress.set_hexpand(true);
        progress.set_xalign(0.5);
        actions.append(&progress);
        let next_button = gtk::Button::with_label(&tr!("Next"));
        next_button.add_css_class("suggested-action");
        actions.append(&next_button);
        main.append(&actions);
        let inner = Rc::new(Inner {
            window: window.clone(), pages: RefCell::default(), rows, current: RefCell::new(None),
            closed: Cell::new(false), finished: Cell::new(false), updating: Cell::new(false), scheme_handler: Cell::new(0),
            step_list: step_list.clone(), stack, back_button: back_button.clone(), next_button: next_button.clone(), progress,
            page_factory: options.page_factory.unwrap_or_else(|| Rc::new(default_page_factory)),
            color_settings: RefCell::new(color_settings), status, done_path: options.done_path,
            launch: options.launch.unwrap_or_else(|| Rc::new(|argv: Vec<String>| { state::spawn(&argv); })),
            first_run: options.first_run, provider, style_close: RefCell::new(Some(Box::new(style_close))),
            scheme_buttons: RefCell::default(), scheme_message: RefCell::new(None), shortcut_rows: RefCell::default(),
            warning: RefCell::new(None), fallbacks: RefCell::default(),
        });
        let this = WelcomeWindow(inner);
        let weak = Rc::downgrade(&this.0);
        let get = move || weak.upgrade().map(WelcomeWindow);
        let g = get.clone();
        window.connect_close_request(move |_| {
            if g().is_some_and(|w| w.close_requested()) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        let g = get.clone();
        key_controller(&window, false, move |_c, key, _code, state| {
            if g().is_some_and(|w| w.key(key, state)) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        let g = get.clone();
        back_button.connect_clicked(move |_| if let Some(w) = g() { w.go(-1) });
        let g = get.clone();
        next_button.connect_clicked(move |_| if let Some(w) = g() { w.go(1) });
        let g = get;
        step_list.connect_row_selected(move |_, row| {
            if let (Some(w), Some(row)) = (g(), row) {
                if !w.updating.get() {
                    if let Some(step) = tags::get_str(row, "step") {
                        w.show_step(&step);
                    }
                }
            }
        });
        this.show_step("welcome");
        this
    }

    fn host(&self) -> Host { Rc::new(WelcomeHost(Rc::downgrade(&self.0))) }

    pub fn page(&self, key: &str) -> Option<Rc<SettingsPage>> {
        self.pages.borrow().iter().find(|(k, _)| k == key).map(|(_, p)| p.clone())
    }

    pub fn current(&self) -> Option<&'static str> { *self.current.borrow() }

    pub fn is_fallback(&self, key: &str) -> bool { self.fallbacks.borrow().iter().any(|k| k == key) }

    pub fn show_step(&self, key: &str) {
        let Some(key) = STEPS.iter().map(|s| s.0).find(|k| *k == key) else { return };
        if self.closed.get() {
            return;
        }
        if self.current() == Some(key) {
            self.sync_actions();
            return;
        }
        if let Some(previous) = self.current().and_then(|c| self.page(c)) {
            previous.deactivate();
        }
        let page = match self.page(key) {
            Some(page) => page,
            None => {
                let page = Rc::new(self.build(key));
                self.pages.borrow_mut().push((key.to_owned(), page.clone()));
                self.stack.add_named(&page.widget, Some(key));
                page
            }
        };
        *self.current.borrow_mut() = Some(key);
        self.stack.set_visible_child_name(key);
        page.activate();
        if self.step_list.selected_row().as_ref() != Some(&self.rows[key]) {
            self.updating.set(true);
            self.step_list.select_row(Some(&self.rows[key]));
            self.updating.set(false);
        }
        self.sync_actions();
    }

    #[cfg(feature = "pages")]
    fn wizard(&self) -> Option<Wizard> {
        if self.current() != Some("language") {
            return None;
        }
        self.page("language")?.controller::<Wizard>().map(|w| (*w).clone())
    }

    pub fn go(&self, offset: i32) {
        #[cfg(feature = "pages")]
        if let Some(wizard) = self.wizard() {
            if wizard.wizard_step(offset > 0) {
                self.sync_actions();
                return;
            }
        }
        let keys = step_keys();
        let index = keys.iter().position(|k| Some(*k) == self.current()).unwrap_or(0) as i32 + offset;
        if index >= keys.len() as i32 {
            self.finish();
        } else if index >= 0 {
            self.show_step(keys[index as usize]);
        }
    }

    fn sync_actions(&self) {
        let keys = step_keys();
        let index = keys.iter().position(|k| Some(*k) == self.current()).unwrap_or(0);
        self.back_button.set_sensitive(index > 0);
        let last = index == keys.len() - 1;
        self.next_button.set_label(&if last { tr!("Start Using Your Desktop") } else { tr!("Next") });
        self.next_button.set_sensitive(true);
        #[cfg(feature = "pages")]
        if let Some(wizard) = self.wizard() {
            // Welcome's own buttons drive the language wizard's steps.
            if let Some(action) = wizard.wizard_action().filter(|a| !a.is_empty()) {
                self.next_button.set_label(&action);
                self.next_button.set_sensitive(!wizard.busy.get() && wizard.next_button.is_sensitive());
            }
            if wizard.busy.get() {
                self.next_button.set_sensitive(false);
                self.back_button.set_sensitive(false);
            } else if wizard.page.get() == "review" {
                self.back_button.set_sensitive(true);
            }
        }
        self.progress.set_label(&tr!("Step {number} of {total}", number = (index + 1) as i64, total = keys.len() as i64));
        for (number, key) in keys.iter().enumerate() {
            if number < index {
                self.rows[key].add_css_class("welcome-step-done");
            } else {
                self.rows[key].remove_css_class("welcome-step-done");
            }
        }
    }

    pub fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if key == gdk::Key::Escape {
            self.window.close();
            return true;
        }
        if state.contains(gdk::ModifierType::ALT_MASK) && (key == gdk::Key::Left || key == gdk::Key::Right) {
            self.go(if key == gdk::Key::Left { -1 } else { 1 });
            return true;
        }
        false
    }

    fn build(&self, key: &'static str) -> SettingsPage {
        if SETTINGS_SECTIONS.iter().any(|s| s.0 == key) {
            return match (self.page_factory)(key, &self.host()) {
                Ok(page) => {
                    self.embed_settings_page(key, &page);
                    page
                }
                Err(error) => {
                    // A missing component must not stop the walkthrough.
                    eprintln!("ERROR:root:Could not load the {key} page: {error}");
                    self.fallbacks.borrow_mut().push(key.to_owned());
                    self.fallback(key, &error)
                }
            };
        }
        match key {
            "welcome" => self.build_welcome(),
            "style" => self.build_style(),
            "shortcuts" => self.build_shortcuts(),
            _ => self.build_done(),
        }
    }

    /// The shared page header, and no second set of wizard buttons.
    fn embed_settings_page(&self, key: &'static str, page: &SettingsPage) {
        let icon = STEPS.iter().find(|s| s.0 == key).map(|s| s.2).unwrap();
        unify_header(&page.widget, Some(HeaderIcon::Image(symbolic_icon(icon, 24))));
        #[cfg(feature = "pages")]
        if let Some(wizard) = page.controller::<Wizard>() {
            wizard.hide_wizard_chrome();
            let weak = Rc::downgrade(&self.0);
            *wizard.on_wizard_changed.borrow_mut() = Some(Rc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    let this = WelcomeWindow(inner);
                    if !this.closed.get() && this.current() == Some(key) {
                        this.sync_actions();
                    }
                }
            }));
        }
        let _ = (key, page);
    }

    fn settings_button(&self, section: &'static str, text: &str) -> gtk::Button {
        let button = gtk::Button::builder().label(text).halign(gtk::Align::Start).build();
        let launch = self.launch.clone();
        button.connect_clicked(move |_| launch(state::settings_command(&["--section", section], None)));
        button
    }

    fn fallback(&self, key: &'static str, error: &str) -> SettingsPage {
        let body = content(18);
        let (_, name, icon) = STEPS.iter().find(|s| s.0 == key).copied().unwrap();
        body.append(&page_header(&suite_core::i18n::translate(name),
            Some(&tr!("This step could not be shown here. You can make the same choice in Settings.")), Some(icon)).root);
        let detail = label(error, Some("gs-muted"), true);
        detail.set_selectable(true);
        body.append(&detail);
        body.append(&self.settings_button(key, &tr!("Open Settings")));
        SettingsPage::new(scrolled(&body))
    }

    fn build_welcome(&self) -> SettingsPage {
        let body = content(18);
        body.append(&image("user-home-symbolic", 64));
        body.append(&label(&tr!("Welcome to your new desktop"), Some("welcome-hero"), true));
        body.append(&label(&tr!("A few quick choices make it yours: your language, keyboard, how the dock and panel are arranged, and light or dark colors. Every choice can be changed later in Settings."), Some("welcome-lead"), true));
        if let Some(status) = self.status.as_ref().filter(|s| !s.ok) {
            let warning = card(8);
            warning.add_css_class("welcome-warning");
            warning.append(&label(&tr!("Some parts of the desktop could not be set up"), Some("gs-section-title"), true));
            warning.append(&label(&tr!("Not installed: {names}", names = status.failed.join(", ")), Some("gs-muted"), true));
            let log = state::state_directory().join("first-login.log");
            let details = label(&tr!("Details are in {path}. Your administrator can run the installation again with install_all.py --apply.",
                                     path = log.to_string_lossy().into_owned()), Some("gs-muted"), true);
            details.set_selectable(true);
            warning.append(&details);
            body.append(&warning);
            *self.warning.borrow_mut() = Some(warning);
        }
        SettingsPage::new(scrolled(&body))
    }

    fn build_style(&self) -> SettingsPage {
        let body = content(18);
        body.append(&page_header(&tr!("Light or Dark"),
            Some(&tr!("Choose the colors for the dock, our apps and other apps that support it. Automatic follows the GTK theme.")),
            Some("preferences-desktop-appearance-symbolic")).root);
        let row = gtk::Box::builder().spacing(14).halign(gtk::Align::Start).homogeneous(true).build();
        let mut first: Option<gtk::ToggleButton> = None;
        for (value, name, icon) in COLOR_SCHEMES {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
            b.append(&symbolic_icon(icon, 32));
            b.append(&gtk::Label::new(Some(&suite_core::i18n::translate(name))));
            let button = gtk::ToggleButton::builder().child(&b).build();
            button.add_css_class("welcome-scheme");
            button.update_property(&[gtk::accessible::Property::Label(&suite_core::i18n::translate(name))]);
            match &first {
                None => first = Some(button.clone()),
                Some(first) => button.set_group(Some(first)),
            }
            let weak = Rc::downgrade(&self.0);
            let value: &'static str = value;
            button.connect_toggled(move |button| {
                if let Some(inner) = weak.upgrade() {
                    WelcomeWindow(inner).scheme_toggled(button, value);
                }
            });
            row.append(&button);
            self.scheme_buttons.borrow_mut().insert(value, button);
        }
        body.append(&row);
        let message = label("", Some("gs-muted"), true);
        body.append(&message);
        *self.scheme_message.borrow_mut() = Some(message.clone());
        let settings = self.color_settings.borrow().clone();
        match settings {
            None => {
                row.set_sensitive(false);
                message.set_label(&tr!("The shared color scheme setting is unavailable on this system."));
            }
            Some(settings) => {
                let weak = Rc::downgrade(&self.0);
                let id = settings.connect_changed(Rc::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        WelcomeWindow(inner).sync_scheme();
                    }
                }));
                self.scheme_handler.set(id);
                self.sync_scheme();
            }
        }
        SettingsPage::new(scrolled(&body))
    }

    fn sync_scheme(&self) {
        let settings = self.color_settings.borrow().clone();
        let Some(settings) = settings else { return };
        if self.closed.get() {
            return;
        }
        let value = settings.get();
        self.updating.set(true);
        let buttons = self.scheme_buttons.borrow().clone();
        if let Some(button) = buttons.get(value.as_str()).or_else(|| buttons.get("default")) {
            button.set_active(true);
        }
        self.updating.set(false);
    }

    fn scheme_toggled(&self, button: &gtk::ToggleButton, value: &str) {
        let settings = self.color_settings.borrow().clone();
        if self.updating.get() || !button.is_active() {
            return;
        }
        let Some(settings) = settings else { return };
        if settings.get() != value && !settings.set(value) {
            if let Some(message) = self.scheme_message.borrow().as_ref() {
                message.set_label(&tr!("The color scheme could not be changed."));
            }
            self.sync_scheme();
        }
    }

    fn build_shortcuts(&self) -> SettingsPage {
        let body = content(18);
        body.append(&page_header(&tr!("Keyboard Shortcuts"),
            Some(&tr!("These keys work everywhere on the desktop. You can change them in Settings.")),
            Some("preferences-desktop-keyboard-shortcuts-symbolic")).root);
        let b = card(0);
        let keys_column = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        for (key, description) in SHORTCUTS {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            row.add_css_class("gs-row");
            let keys = label(&display_key(key), Some("welcome-key"), false);
            keys_column.add_widget(&keys);
            row.append(&keys);
            let text = label(&suite_core::i18n::translate(description), None, true);
            text.set_hexpand(true);
            row.append(&text);
            b.append(&row);
            self.shortcut_rows.borrow_mut().push((keys.label().to_string(), text.label().to_string()));
        }
        body.append(&b);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let show = gtk::Button::with_label(&tr!("Show All Shortcuts"));
        let launch = self.launch.clone();
        show.connect_clicked(move |_| launch(state::settings_command(&["--shortcuts"], None)));
        buttons.append(&show);
        buttons.append(&self.settings_button("shortcuts", &tr!("Change Shortcuts")));
        body.append(&buttons);
        SettingsPage::new(scrolled(&body))
    }

    fn build_done(&self) -> SettingsPage {
        let body = content(18);
        body.append(&image("emblem-ok-symbolic", 64));
        body.append(&label(&tr!("You're all set"), Some("welcome-hero"), true));
        body.append(&label(&tr!("Open Welcome again at any time from the application menu. Everything else is in Settings."),
                           Some("welcome-lead"), true));
        body.append(&self.settings_button("about", &tr!("Open Settings")));
        SettingsPage::new(scrolled(&body))
    }

    /// Record that setup is complete, unless the installation itself is unfinished.
    pub fn finish(&self) {
        if self.status.as_ref().map(|s| s.ok).unwrap_or(true) {
            if let Err(error) = state::mark_setup_done(self.done_path.as_deref()) {
                eprintln!("ERROR:root:Could not record that setup is complete: {error}");
            }
        }
        self.finished.set(true);
        self.window.close();
    }

    fn close_requested(&self) -> bool {
        let pages = self.pages.borrow().clone();
        if let Some((key, _)) = pages.iter().find(|(_, page)| !page.can_close()) {
            self.show_step(key);
            return true;
        }
        self.cleanup();
        false
    }

    pub fn cleanup(&self) {
        if self.closed.replace(true) {
            return;
        }
        let pages = self.pages.borrow().clone();
        for (_key, page) in pages {
            page.close();
        }
        let handler = self.scheme_handler.replace(0);
        if handler != 0 {
            if let Some(settings) = self.color_settings.borrow().as_ref() {
                settings.disconnect(handler);
            }
        }
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.window), &self.provider);
        if let Some(close) = self.style_close.borrow_mut().take() {
            close();
        }
    }
}
