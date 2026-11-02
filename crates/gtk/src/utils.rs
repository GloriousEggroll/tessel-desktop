//! Small native GTK4 utilities shared by the desktop applications (gtk4_utils.py).

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;

/// `Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION` (600); add offsets like Python does.
pub const PRIORITY_APPLICATION: u32 = gtk::STYLE_PROVIDER_PRIORITY_APPLICATION;
/// `Gtk.STYLE_PROVIDER_PRIORITY_USER` (800).
pub const PRIORITY_USER: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER;

thread_local! {
    static DISPLAY_CSS: RefCell<HashMap<(gdk::Display, &'static str), gtk::CssProvider>> = RefCell::new(HashMap::new());
}

/// Install `css` once per display under `key`; later calls return the same provider.
pub fn css_once(display: &gdk::Display, key: &'static str, css: &str, priority: u32) -> gtk::CssProvider {
    if let Some(provider) = DISPLAY_CSS.with(|map| map.borrow().get(&(display.clone(), key)).cloned()) {
        return provider;
    }
    let provider = load_css(display, css, priority);
    DISPLAY_CSS.with(|map| map.borrow_mut().insert((display.clone(), key), provider.clone()));
    provider
}

/// Whether `key` was installed on `display` by [`css_once`] (or the helpers using it).
pub fn has_css(display: &gdk::Display, key: &'static str) -> Option<gtk::CssProvider> {
    DISPLAY_CSS.with(|map| map.borrow().get(&(display.clone(), key)).cloned())
}

fn load_css(display: &gdk::Display, css: &str, priority: u32) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    gtk::style_context_add_provider_for_display(display, &provider, priority);
    provider
}

/// Load a stylesheet for `display` (default display when `None`). Every call
/// adds a new provider; remove it with [`remove_css`]. Also installs, once, the
/// suite's search-field text margins at APPLICATION+40.
pub fn add_css(css: &str, display: Option<&gdk::Display>, priority: u32) -> gtk::CssProvider {
    let display = display.cloned().or_else(gdk::Display::default).expect("a GDK display");
    // Every suite search field gets the same icon/text breathing room, even
    // with a GTK theme that gives SearchEntry's text node no default margin.
    css_once(&display, "suite-search", "searchentry > text, entry.search > text { margin-left: 8px; margin-right: 8px; }",
             PRIORITY_APPLICATION + 40);
    load_css(&display, css, priority)
}

pub fn remove_css(display: &gdk::Display, provider: &gtk::CssProvider) {
    gtk::style_context_remove_provider_for_display(display, provider);
}

pub fn children(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut result = Vec::new();
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        result.push(current);
    }
    result
}

/// Remove every child of a Box/ListBox/FlowBox (any widget with `remove`).
pub fn remove_children(container: &impl IsA<gtk::Widget>) {
    for child in children(container) {
        child.unparent_from(container.upcast_ref());
    }
}

trait UnparentFrom {
    fn unparent_from(&self, parent: &gtk::Widget);
}

impl UnparentFrom for gtk::Widget {
    fn unparent_from(&self, parent: &gtk::Widget) {
        if let Some(container) = parent.downcast_ref::<gtk::Box>() {
            container.remove(self);
        } else if let Some(container) = parent.downcast_ref::<gtk::ListBox>() {
            container.remove(self);
        } else if let Some(container) = parent.downcast_ref::<gtk::FlowBox>() {
            container.remove(self);
        } else if let Some(container) = parent.downcast_ref::<gtk::Grid>() {
            container.remove(self);
        } else {
            self.unparent();
        }
    }
}

pub fn set_margins<W: IsA<gtk::Widget>>(widget: &W, margin: i32) -> &W {
    widget.set_margin_top(margin);
    widget.set_margin_bottom(margin);
    widget.set_margin_start(margin);
    widget.set_margin_end(margin);
    widget
}

/// Symbolic alternatives so Settings, Welcome and the applets show an icon
/// under breeze (which lacks many GNOME names) and Papirus. Symbolic names come
/// first; colored names are a last resort for themes without either.
pub const ICON_FALLBACKS: &[(&str, &[&str])] = &[
    ("bluetooth-active-symbolic", &["bluetooth-symbolic", "preferences-system-bluetooth-symbolic",
                                    "preferences-system-bluetooth-activated-symbolic", "bluetooth"]),
    ("preferences-system-notifications-symbolic", &["notifications-symbolic", "notifications-applet-symbolic",
                                                    "preferences-desktop-notification-symbolic",
                                                    "preferences-desktop-notification-bell", "notifications",
                                                    "dialog-information-symbolic"]),
    ("power-profile-power-saver-symbolic", &["battery-low-symbolic", "battery-100-profile-powersave", "battery-low"]),
    ("power-profile-balanced-symbolic", &["battery-good-symbolic", "battery-100-profile-balanced",
                                          "preferences-system-power-management", "system-shutdown-symbolic"]),
    ("power-profile-performance-symbolic", &["utilities-system-monitor-symbolic", "battery-100-profile-performance",
                                             "utilities-system-monitor"]),
    ("preferences-system-power-symbolic", &["system-shutdown-symbolic", "preferences-system-power-management"]),
    ("view-column-symbolic", &["view-dual-symbolic", "view-columns-symbolic"]),
    // Night Light, and Welcome's Dark choice.
    ("night-light-symbolic", &["weather-clear-night-symbolic", "redshift-status-on-symbolic", "redshift-status-on"]),
    ("preferences-desktop-keyboard-shortcuts-symbolic", &["preferences-desktop-keyboard-shortcut-symbolic",
                                                          "input-keyboard-symbolic"]),
    ("preferences-desktop-keyboard-symbolic", &["input-keyboard-symbolic"]),
    // Light or Dark / Theme.
    ("preferences-desktop-appearance-symbolic", &["preferences-desktop-theme-global-symbolic", "contrast-symbolic",
                                                  "color-mode-black-white-symbolic", "preferences-desktop-theme"]),
    ("preferences-desktop-wallpaper-symbolic", &["viewimage-symbolic", "image-x-generic-symbolic",
                                                 "preferences-desktop-wallpaper"]),
    ("preferences-desktop-display-symbolic", &["video-display-symbolic", "monitor-symbolic", "computer-symbolic",
                                               "preferences-desktop-display"]),
    ("video-display-symbolic", &["monitor-symbolic", "computer-symbolic", "video-display"]),
    ("preferences-desktop-symbolic", &["preferences-other-symbolic", "window-duplicate-symbolic", "preferences-desktop"]),
    ("preferences-system-time-symbolic", &["clock-symbolic", "alarm-symbolic", "preferences-system-time"]),
    ("view-reveal-symbolic", &["view-visible-symbolic", "caffeine-cup-full-symbolic"]),
    ("dialog-information-symbolic", &["help-about-symbolic", "documentinfo-symbolic", "dialog-information"]),
    ("dialog-password-symbolic", &["system-lock-screen-symbolic", "lock-symbolic", "object-locked-symbolic",
                                   "dialog-password"]),
    ("avatar-default-symbolic", &["system-users-symbolic", "user-identity-symbolic"]),
    ("battery-good-symbolic", &["battery-symbolic", "battery-080-symbolic", "battery-good"]),
    ("preferences-desktop-locale-symbolic", &["globe-symbolic", "preferences-desktop-locale"]),
    ("dialog-warning-symbolic", &["emblem-important-symbolic", "dialog-warning"]),
    ("image-x-generic-symbolic", &["viewimage-symbolic", "image-x-generic"]),
];

/// `ICON_FALLBACKS.get(name, ())`.
pub fn icon_fallbacks(name: &str) -> &'static [&'static str] {
    ICON_FALLBACKS.iter().find(|(key, _)| *key == name).map_or(&[], |entry| entry.1)
}

/// `name` followed by its fallbacks.
pub fn themed_icon(name: &str) -> gio::ThemedIcon {
    let mut names = vec![name];
    names.extend_from_slice(icon_fallbacks(name));
    gio::ThemedIcon::from_names(&names)
}

/// A themed image with fallbacks at `size` px (Python default 16).
pub fn image(name: &str, size: i32) -> gtk::Image {
    let result = gtk::Image::from_gicon(&themed_icon(name));
    result.set_pixel_size(size);
    result
}

pub fn accessible<'a, W: IsA<gtk::Accessible>>(widget: &'a W, text: &str) -> &'a W {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
    widget
}

/// Append with expand (or `end` alignment) along the box's orientation.
pub fn append<'a, W: IsA<gtk::Widget>>(container: &gtk::Box, child: &'a W, expand: bool, end: bool) -> &'a W {
    let horizontal = container.orientation() == gtk::Orientation::Horizontal;
    if horizontal { child.set_hexpand(expand || end) } else { child.set_vexpand(expand || end) }
    if end {
        if horizontal { child.set_halign(gtk::Align::End) } else { child.set_valign(gtk::Align::End) }
    }
    container.append(child);
    child
}

const HEADERBAR_CSS: &str = "
headerbar.suite-headerbar windowcontrols button.image-button {
    min-width: 24px; min-height: 24px; padding: 2px; margin: 0;
    border: none; border-radius: 6px; background: transparent;
}
headerbar.suite-headerbar windowcontrols button image { color: @theme_fg_color; }
headerbar.suite-headerbar .title { font-weight: 600; }
";

/// A HeaderBar with the suite's `.title` label and compact window controls.
pub fn headerbar(title: &str) -> gtk::HeaderBar {
    let header = gtk::HeaderBar::new();
    header.add_css_class("suite-headerbar");
    // The "title" class gives every suite app the same title weight.
    let label = gtk::Label::builder().label(title).single_line_mode(true).build();
    label.add_css_class("title");
    header.set_title_widget(Some(&label));
    header.set_show_title_buttons(true);
    css_once(&WidgetExt::display(&header), "suite-headerbar", HEADERBAR_CSS, PRIORITY_APPLICATION + 10);
    header
}

pub const SIDEBAR_CLASS: &str = "suite-sidebar";
/// Flat navigation rows with 16px symbolic icons and a rounded selection pill in
/// the palette's accent. Colors are GTK's named colors, which Tessel Dock's live
/// palette (gtk_color_scheme / companion_theme) redefines per monitor.
pub const SIDEBAR_CSS: &str = "
.suite-sidebar, .suite-sidebar list, .suite-sidebar listview {
    background-color: transparent; background-image: none; border: none;
}
.suite-sidebar row {
    background-color: transparent; background-image: none; border: none; box-shadow: none;
    padding: 6px 10px; margin: 1px 6px; border-radius: 8px; min-height: 0;
}
.suite-sidebar row image { -gtk-icon-size: 16px; min-width: 16px; min-height: 16px; }
.suite-sidebar row:hover { background-color: alpha(@theme_fg_color, 0.07); }
.suite-sidebar row:selected, .suite-sidebar row:selected:hover {
    background-color: alpha(@theme_selected_bg_color, 0.22); color: @theme_fg_color;
}
.suite-sidebar row:selected image { color: @theme_selected_bg_color; }
.suite-sidebar row:selected label { font-weight: 600; }
.suite-sidebar row:focus-visible { outline: 2px solid alpha(@theme_selected_bg_color, 0.65); outline-offset: -2px; }
.suite-sidebar row label { font-size: 10.5pt; }
";

/// Give a ListBox (or its container) the shared `.suite-sidebar` look. Icons in
/// the rows should be `-symbolic` names; `image(name, 16)` produces matching ones.
pub fn sidebar_style<W: IsA<gtk::Widget>>(widget: &W) -> &W {
    widget.add_css_class(SIDEBAR_CLASS);
    css_once(&widget.display(), "suite-sidebar", SIDEBAR_CSS, PRIORITY_APPLICATION + 5);
    widget
}

pub fn key_controller(widget: &impl IsA<gtk::Widget>, capture: bool,
                      callback: impl Fn(&gtk::EventControllerKey, gdk::Key, u32, gdk::ModifierType) -> glib::Propagation + 'static)
                      -> gtk::EventControllerKey {
    let controller = gtk::EventControllerKey::new();
    if capture {
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    }
    controller.connect_key_pressed(callback);
    widget.add_controller(controller.clone());
    controller
}

thread_local! {
    static END_CHILDREN: RefCell<HashMap<gtk::Box, Vec<glib::WeakRef<gtk::Widget>>>> = RefCell::new(HashMap::new());
}

/// GTK3-style `pack_start`/`pack_end` for a GTK4 Box: `end` children stay
/// grouped after every start child (`expand`, `fill`, `padding` as in GTK3).
pub fn pack<'a, W: IsA<gtk::Widget>>(container: &gtk::Box, child: &'a W, expand: bool, fill: bool, padding: i32, end: bool) -> &'a W {
    let horizontal = container.orientation() == gtk::Orientation::Horizontal;
    let align = if fill { gtk::Align::Fill } else { gtk::Align::Center };
    if horizontal {
        child.set_hexpand(expand);
        child.set_halign(align);
    } else {
        child.set_vexpand(expand);
        child.set_valign(align);
    }
    if padding != 0 {
        if horizontal {
            child.set_margin_start(padding);
            child.set_margin_end(padding);
        } else {
            child.set_margin_top(padding);
            child.set_margin_bottom(padding);
        }
    }
    let parent: gtk::Widget = container.clone().upcast();
    let mut end_children: Vec<gtk::Widget> = END_CHILDREN.with(|map| map.borrow().get(container).cloned()).unwrap_or_default()
        .iter().filter_map(|weak| weak.upgrade()).filter(|item| item.parent().as_ref() == Some(&parent)).collect();
    match end_children.first() {
        Some(first) => container.insert_child_after(child, first.prev_sibling().as_ref()),
        None => container.append(child),
    }
    if end {
        end_children.insert(0, child.clone().upcast());
    }
    END_CHILDREN.with(|map| map.borrow_mut().insert(container.clone(), end_children.iter().map(|w| w.downgrade()).collect()));
    child
}

/// Present a window and ask labwc (via `wlrctl toplevel focus`) to focus *and
/// raise* it, matching app id and title so dialogs are not selected. A restored
/// surface may not be advertised until its first buffer: up to 4 retries 100 ms
/// apart. `still_valid` is re-checked before each attempt (Python's ownership checks).
pub fn raise_window(application_id: &str, window: &gtk::Window, still_valid: impl Fn(&gtk::Window) -> bool + 'static) {
    raise_attempt(application_id.to_owned(), window.downgrade(), std::rc::Rc::new(still_valid), 0);
}

fn raise_attempt(id: String, window: glib::WeakRef<gtk::Window>, valid: std::rc::Rc<dyn Fn(&gtk::Window) -> bool>, attempt: u32) {
    let Some(target) = window.upgrade() else { return };
    if !target.get_visible() || !valid(&target) {
        return;
    }
    let display = WidgetExt::display(&target);
    display.flush();
    if !display.type_().name().eq("GdkWaylandDisplay") {
        return;
    }
    let Some(helper) = glib::find_program_in_path("wlrctl") else { return };
    let title = target.title().map(|t| t.to_string()).unwrap_or_default();
    let (app_id, title) = (format!("app_id:{id}"), format!("title:{title}"));
    let argv = [helper.as_os_str(), "toplevel".as_ref(), "focus".as_ref(), app_id.as_ref(), title.as_ref()];
    let Ok(process) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE)
    else { return };
    let child = process.clone();
    process.wait_async(None::<&gio::Cancellable>, move |result| {
        if result.is_ok() && !child.is_successful() && attempt < 4 {
            glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || raise_attempt(id, window, valid, attempt + 1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn theme_names(theme: &str) -> HashSet<String> {
        let mut names = HashSet::new();
        let mut stack = vec![Path::new("/usr/share/icons").join(theme)];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    names.insert(name.rsplit_once('.').map_or(name, |(stem, _)| stem).to_owned());
                }
            }
        }
        names
    }

    // Icons named in Global Settings and Welcome resolve to a symbolic name in
    // breeze and Papirus through ICON_FALLBACKS (reads installed themes only).
    #[test]
    fn settings_and_welcome_icons_resolve_to_symbolic_names() {
        let source = tessel_core::paths::home().join("tessel-bluetooth");
        let chains: [(&str, &[&str]); 2] = [("breeze", &["breeze", "hicolor"]), ("Papirus", &["Papirus", "breeze", "hicolor"])];
        if !source.is_dir() || ["breeze", "Papirus", "hicolor"].iter().any(|t| !Path::new("/usr/share/icons").join(t).is_dir()) {
            eprintln!("skipping: sources or icon themes not installed");
            return;
        }
        let mut used = HashSet::new();
        for folder in ["tessel-settings", "welcome"] {
            let files: Vec<PathBuf> = std::fs::read_dir(source.join(folder)).unwrap().flatten().map(|e| e.path()).collect();
            for file in files {
                let name = file.file_name().unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".py") || name.starts_with("test_") {
                    continue;
                }
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                // re.findall(r"['\"]([a-z0-9-]+-symbolic)['\"]", text)
                let bytes = text.as_bytes();
                let quote = |b: u8| b == b'\'' || b == b'"';
                let mut i = 0;
                while i < bytes.len() {
                    if quote(bytes[i]) {
                        let mut j = i + 1;
                        while j < bytes.len() && (bytes[j].is_ascii_lowercase() || bytes[j].is_ascii_digit() || bytes[j] == b'-') {
                            j += 1;
                        }
                        let word = &text[i + 1..j];
                        if j < bytes.len() && quote(bytes[j]) && word.len() > 9 && word.ends_with("-symbolic") {
                            used.insert(word.to_owned());
                            i = j + 1;
                            continue;
                        }
                    }
                    i += 1;
                }
            }
        }
        for required in ["bluetooth-active-symbolic", "night-light-symbolic", "preferences-desktop-keyboard-shortcuts-symbolic",
                         "preferences-desktop-appearance-symbolic", "input-keyboard-symbolic"] {
            assert!(used.contains(required), "{required}");
        }
        let themes: std::collections::HashMap<&str, HashSet<String>> =
            ["breeze", "Papirus", "hicolor"].iter().map(|t| (*t, theme_names(t))).collect();
        for (theme, chain) in chains {
            for name in &used {
                let mut candidates = vec![name.as_str()];
                candidates.extend_from_slice(icon_fallbacks(name));
                let found = candidates.iter().find(|c| chain.iter().any(|part| themes[part].contains(**c)));
                let found = found.unwrap_or_else(|| panic!("{theme}: {name} has no icon"));
                assert!(found.ends_with("-symbolic"), "{theme}: {name} -> {found}");
            }
        }
    }
}

thread_local! {
    static TRIM_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Hand free heap pages back to the OS once `delay` has passed (coalesced:
/// at most one pending trim). Startup and popup teardown leave large freed
/// holes in glibc's heap (GPU shader compilation, icon-theme scans, menu
/// tiles) that glibc only returns on `malloc_trim`. Purely a memory
/// operation: nothing visible changes.
pub fn trim_heap_after(delay: std::time::Duration) {
    if TRIM_PENDING.with(|p| p.replace(true)) {
        return;
    }
    glib::timeout_add_local_once(delay, || {
        TRIM_PENDING.with(|p| p.set(false));
        // SAFETY: malloc_trim only releases free memory of the allocator.
        unsafe { libc::malloc_trim(0) };
    });
}
