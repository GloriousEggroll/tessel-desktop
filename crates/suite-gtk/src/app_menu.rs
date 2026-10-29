//! The suite's standard primary menu: Keyboard Shortcuts, About and Quit
//! (app_menu.py). Every desktop-suite application gets the same end-of-header-bar
//! hamburger button and the same final menu section.
//!
//! Actions: `app.shortcuts` (Ctrl+?), `app.about`, `app.quit` (Ctrl+Q). The
//! default quit closes every window through its normal close-request, so
//! unsaved-change prompts and background-hiding behaviour still apply.

use crate::utils::{accessible, add_css, headerbar, remove_css, set_margins};
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use suite_core::tr;

const STYLE: &str = r#"
.suite-shortcuts .shortcut-section { font-weight: bold; margin-top: 10px; }
.suite-shortcuts .shortcut-keys { font-feature-settings: "tnum"; padding: 2px 8px; border-radius: 6px;
 background: alpha(@theme_fg_color,.08); border: 1px solid alpha(@theme_fg_color,.14); }
"#;

/// One row: GTK accelerator strings (`"<Control>o"`, alternatives joined with
/// " / ") or free text such as `"Ctrl+Scroll"` shown unchanged; English label.
#[derive(Clone, Debug)]
pub struct Shortcut {
    pub keys: Vec<String>,
    pub label: String,
}

impl Shortcut {
    pub fn new(keys: &str, label: &str) -> Self { Self { keys: vec![keys.into()], label: label.into() } }
    pub fn any(keys: &[&str], label: &str) -> Self { Self { keys: keys.iter().map(|k| (*k).into()).collect(), label: label.into() } }
}

/// `(English section title, rows)`.
pub type ShortcutSection = (String, Vec<Shortcut>);

fn standard() -> ShortcutSection {
    ("General".into(), vec![Shortcut::new("<Control>question", "Keyboard shortcuts"), Shortcut::new("<Control>q", "Quit")])
}

/// Readable, localised key text for an accelerator; free text is kept.
pub fn accelerator_label(accelerator: &str) -> String {
    match gtk::accelerator_parse(accelerator) {
        Some((key, mods)) if key.into_glib() != 0 => gtk::accelerator_get_label(key, mods).to_string(),
        _ => accelerator.to_owned(),
    }
}

/// `accelerator_label` of each alternative, joined by " / ".
pub fn keys_label(keys: &[String]) -> String {
    keys.iter().map(|key| accelerator_label(key)).collect::<Vec<_>>().join(" / ")
}

fn theme(window: &gtk::Window) {
    // Plain GTK styling still works without the dock palette.
    crate::theme::attach(window, None);
}

/// The open shortcuts window; `rows` holds each row with its casefolded search text.
pub struct ShortcutsWindow {
    pub window: gtk::Window,
    pub search: gtk::SearchEntry,
    pub rows: Rc<Vec<(gtk::Box, String)>>,
}

pub fn shortcuts_window(parent: Option<&gtk::Window>, shortcuts: &[ShortcutSection], title: Option<&str>) -> ShortcutsWindow {
    let mut sections = shortcuts.to_vec();
    sections.push(standard());
    let title = title.map(str::to_owned).unwrap_or_else(|| tr!("Keyboard Shortcuts"));
    let window = gtk::Window::builder().title(&title).modal(true).default_width(520).default_height(620)
        .destroy_with_parent(true).build();
    window.set_transient_for(parent);
    window.add_css_class("suite-shortcuts");
    let css = add_css(STYLE, Some(&WidgetExt::display(&window)), crate::utils::PRIORITY_APPLICATION);
    window.set_titlebar(Some(&headerbar(&title)));
    theme(&window);
    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    set_margins(&content_box, 16);
    let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search shortcuts")).build();
    accessible(&search, &tr!("Search shortcuts"));
    content_box.append(&search);
    let scroll = gtk::ScrolledWindow::builder().vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    scroll.set_child(Some(&content));
    content_box.append(&scroll);
    window.set_child(Some(&content_box));
    let mut rows = Vec::new();
    let mut headings = Vec::new();
    for (section, entries) in &sections {
        let heading = gtk::Label::builder().label(tr!(section)).xalign(0.0).build();
        heading.add_css_class("shortcut-section");
        content.append(&heading);
        let mut members = Vec::new();
        for entry in entries {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let name = gtk::Label::builder().label(tr!(entry.label)).xalign(0.0).hexpand(true).wrap(true).build();
            let keys = gtk::Label::builder().label(keys_label(&entry.keys)).xalign(1.0).build();
            keys.add_css_class("shortcut-keys");
            row.append(&name);
            row.append(&keys);
            let (name, keys) = (name.label(), keys.label());
            row.update_property(&[gtk::accessible::Property::Label(&format!("{name}, {keys}"))]);
            content.append(&row);
            members.push(row.clone());
            rows.push((row, format!("{name} {keys}").to_lowercase()));
        }
        headings.push((heading, members));
    }
    let rows = Rc::new(rows);
    let filter_rows = rows.clone();
    search.connect_search_changed(move |search| {
        let query = search.text().trim().to_lowercase();
        for (row, text) in filter_rows.iter() {
            row.set_visible(query.is_empty() || text.contains(&query));
        }
        for (heading, members) in &headings {
            heading.set_visible(members.iter().any(|member| member.get_visible()));
        }
    });
    let keys = gtk::EventControllerKey::new();
    let closing = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let Some(window) = closing.upgrade() { window.close() }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window.connect_close_request(move |window| {
        remove_css(&WidgetExt::display(window), &css);
        glib::Propagation::Proceed
    });
    window.present();
    search.grab_focus();
    ShortcutsWindow { window, search, rows }
}

/// About dialog details; `name` and `comments` are English and translated here.
#[derive(Clone, Debug, Default)]
pub struct AboutDetails {
    pub name: String,
    pub icon: String,
    pub version: Option<String>,
    pub comments: Option<String>,
    pub website: Option<String>,
}

/// The suite has no published license, so the About dialog omits one.
pub fn about_dialog(parent: Option<&gtk::Window>, details: &AboutDetails) -> gtk::AboutDialog {
    let dialog = gtk::AboutDialog::builder().modal(true).destroy_with_parent(true).program_name(tr!(details.name))
        .logo_icon_name(&details.icon).build();
    dialog.set_transient_for(parent);
    if let Some(version) = details.version.as_deref().filter(|v| !v.is_empty()) {
        dialog.set_version(Some(version));
    }
    if let Some(comments) = details.comments.as_deref().filter(|c| !c.is_empty()) {
        dialog.set_comments(Some(&tr!(comments)));
    }
    if let Some(website) = details.website.as_deref().filter(|w| !w.is_empty()) {
        dialog.set_website(Some(website));
    }
    theme(dialog.upcast_ref());
    dialog.present();
    dialog
}

/// Icon themes disagree on "open-menu-symbolic": Papirus draws vertical dots,
/// Breeze and Adwaita a hamburger. Every suite menu uses the same hamburger file.
pub const MENU_ICON_FILES: [&str; 1] = ["/usr/share/icons/Adwaita/symbolic/actions/open-menu-symbolic.svg"];

/// The suite's primary-menu icon: always the three-line hamburger.
pub fn menu_gicon() -> gio::Icon {
    for path in MENU_ICON_FILES {
        if std::path::Path::new(path).exists() {
            return gio::FileIcon::new(&gio::File::for_path(path)).upcast();
        }
    }
    gio::ThemedIcon::new("open-menu-symbolic").upcast()
}

/// Show the hamburger in `button`; returns its image.
pub fn use_menu_icon(button: &gtk::MenuButton) -> gtk::Image {
    let image = gtk::Image::from_gicon(&menu_gicon());
    button.set_child(Some(&image));
    if let Some(inner) = button.first_child() {
        // Keep the compact square padding of an icon-only MenuButton.
        inner.add_css_class("image-button");
    }
    image
}

pub fn has_menu_icon(button: &gtk::MenuButton) -> bool {
    button.child().and_downcast::<gtk::Image>().and_then(|image| image.gicon()).is_some_and(|icon| icon.equal(Some(&menu_gicon())))
}

/// Re-validate the window's styles once it is shown.
///
/// The palette stylesheet is installed from the window's map handler, after
/// GTK has already computed that frame's styles, and GTK does not revisit
/// them; header tools then keep the theme's raised default fill until some
/// unrelated change restyles the window. Toggling a class on the next idle
/// forces that pass.
pub fn restyle_when_mapped(window: &gtk::Window) {
    if unsafe { window.data::<bool>("suite-restyle-signal") }.is_some() {
        return;
    }
    window.connect_map(|window| {
        let window = window.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(window) = window.upgrade() {
                window.remove_css_class("suite-restyled");
                window.add_css_class("suite-restyled");
            }
        });
    });
    unsafe { window.set_data("suite-restyle-signal", true) };
}

struct MenuState {
    window: glib::WeakRef<gtk::Window>,
    shortcuts: Vec<ShortcutSection>,
    details: AboutDetails,
    quit: Option<Rc<dyn Fn()>>,
}

type SharedState = Rc<RefCell<MenuState>>;
const STATE_KEY: &str = "suite-app-menu";

fn state(app: &gtk::Application) -> Option<SharedState> {
    unsafe { app.data::<SharedState>(STATE_KEY).map(|s| s.as_ref().clone()) }
}

fn parent(app: &gtk::Application, state: &SharedState) -> Option<gtk::Window> {
    app.active_window().or_else(|| state.borrow().window.upgrade())
}

/// Replace the listed shortcuts later (for example after the user rebinds keys).
pub fn set_shortcuts(app: &impl IsA<gtk::Application>, shortcuts: Vec<ShortcutSection>) {
    if let Some(state) = state(app.upcast_ref()) {
        state.borrow_mut().shortcuts = shortcuts;
    }
}

/// Where the standard section goes.
pub enum MenuTarget {
    /// A new `gio::Menu` and a new MenuButton.
    New,
    /// The application's own menu; the section is appended last.
    Model(gio::Menu),
    /// An existing MenuButton whose model is extended (or set when it has none).
    Button(gtk::MenuButton),
}

/// Install the standard menu. Actions are added to `app` once; calling again for
/// another window reuses them and updates the stored details. With `header` the
/// returned MenuButton is packed at its end. `quit` replaces the default quit.
pub fn install_app_menu(app: &impl IsA<gtk::Application>, window: &impl IsA<gtk::Window>, menu: MenuTarget, details: AboutDetails,
                        shortcuts: Vec<ShortcutSection>, header: Option<&gtk::HeaderBar>, quit: Option<Rc<dyn Fn()>>) -> gtk::MenuButton {
    let app: &gtk::Application = app.upcast_ref();
    let window: &gtk::Window = window.upcast_ref();
    let shared = match state(app) {
        Some(shared) => shared,
        None => {
            let shared: SharedState = Rc::new(RefCell::new(MenuState {
                window: window.downgrade(), shortcuts: Vec::new(), details: AboutDetails::default(), quit: None }));
            unsafe { app.set_data(STATE_KEY, shared.clone()) };
            for name in ["shortcuts", "about", "quit"] {
                if app.lookup_action(name).is_some() {
                    app.remove_action(name);
                }
                let action = gio::SimpleAction::new(name, None);
                let (app_weak, state_weak) = (app.downgrade(), Rc::downgrade(&shared));
                action.connect_activate(move |action, _| {
                    let (Some(app), Some(state)) = (app_weak.upgrade(), state_weak.upgrade()) else { return };
                    match action.name().as_str() {
                        "shortcuts" => {
                            let sections = state.borrow().shortcuts.clone();
                            shortcuts_window(parent(&app, &state).as_ref(), &sections, None);
                        }
                        "about" => {
                            let details = state.borrow().details.clone();
                            about_dialog(parent(&app, &state).as_ref(), &details);
                        }
                        _ => {
                            let quit = state.borrow().quit.clone();
                            match quit {
                                Some(quit) => quit(),
                                None => app.windows().iter().for_each(|w| w.close()),
                            }
                        }
                    }
                });
                app.add_action(&action);
            }
            app.set_accels_for_action("app.shortcuts", &["<Control>question"]);
            app.set_accels_for_action("app.quit", &["<Control>q"]);
            shared
        }
    };
    {
        let mut state = shared.borrow_mut();
        state.window = window.downgrade();
        state.shortcuts = shortcuts;
        state.details = details.clone();
        state.quit = quit;
    }
    let (button, model) = match menu {
        MenuTarget::Button(button) => {
            let model = button.menu_model();
            (Some(button), model)
        }
        MenuTarget::Model(model) => (None, Some(model.upcast())),
        MenuTarget::New => (None, None),
    };
    let model: gio::MenuModel = model.unwrap_or_else(|| gio::Menu::new().upcast());
    let section = gio::Menu::new();
    section.append(Some(&tr!("Keyboard Shortcuts")), Some("app.shortcuts"));
    section.append(Some(&tr!("About {name}", name = tr!(details.name))), Some("app.about"));
    section.append(Some(&tr!("Quit")), Some("app.quit"));
    if let Some(menu) = model.downcast_ref::<gio::Menu>() {
        menu.append_section(None, &section);
    }
    let button = button.unwrap_or_default();
    use_menu_icon(&button);
    // Header tools stay flat even before the palette stylesheet applies.
    button.add_css_class("flat");
    button.add_css_class("suite-app-menu");
    restyle_when_mapped(window);
    button.set_menu_model(Some(&model));
    button.set_primary(true);
    button.set_tooltip_text(Some(&tr!("Main menu")));
    accessible(&button, &tr!("Main menu"));
    if let Some(header) = header {
        header.pack_end(&button);
    }
    button
}
