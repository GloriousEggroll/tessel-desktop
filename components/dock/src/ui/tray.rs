//! status_tray.py widgets: `StatusTray` (one view per dock clone over the
//! shared StatusNotifier backend), the DBusMenu popover (`RemoteMenuPopover`,
//! dbusmenu.py's GTK half over `dbus::dbusmenu::RemoteMenu`) and
//! `TrayExpander` (inline expansion with outside-click dismissal surfaces):
//! the system tray's "More apps" expansion and the title-mode applications.
//!
//! API:
//! - `StatusTray::new(Rc<dyn TrayBackend>, TrayOptions)`; `.widget()` (the
//!   `tessel-dock-tray` box), `.buttons()` / `.button(key)` / `.keys()`,
//!   `.icon_size()`, `.set_icon_size(n) -> bool`, `.refresh()`, `.close()`,
//!   `.theme_changed()`, `.custom_theme_paths()`, `.pixbuf_calls()` (test
//!   counter of `_pixbuf` work), `.close_menu()`, `.menu_open()`,
//!   `.menu_deactivated(&menu)`, `.menu(key)`, `.set_menu_factory(f)` (test hook replacing
//!   `_prepare_menu`'s RemoteMenu construction), `.image(key)`.
//!   Clicks: left → `Activate(x, y)` (SCREEN coordinates from
//!   `TrayOptions::screen_position`; on failure the DBusMenu opens), middle →
//!   `SecondaryActivate`, right (or left with `ItemIsMenu`) → DBusMenu popover
//!   if the item has one, else `ContextMenu(x, y)` after releasing panel menus
//!   (`menu_state(true)` then `(false)`); scroll → `Scroll(∓120·steps, axis)`.
//! - `TrayMenu` trait (popup/popdown/close) and `RemoteMenuPopover::new`.
//! - `TrayExpander::new(dock, app, content, ExpanderOptions)`; `.widget()`, `.button()`,
//!   `.arrow()`, `.content()`, `.expanded()`, `.set_expanded(b)`,
//!   `.menu_active(b)`, `.menu_is_active()`, `.sync()`, `.close()`,
//!   `.backdrops()`, `.kind()`. With `ExpanderOptions::toggle` the expander
//!   uses that button (the system tray's "More apps") instead of its own arrow
//!   button: it toggles on click, leaves the button's look to its owner and
//!   reports every change to `on_change`. Namespaces `tessel-dock-tray-dismiss` /
//!   `tessel-dock-<kind>-dismiss` (TOP, keyboard NONE, exclusive −1, all edges, one
//!   per monitor), input region = monitor minus every visible clone's panel
//!   and `extra_holes`; one expander at a time across all clones; never while
//!   a fullscreen window is active; released when hidden/emptied/destroyed.
//!   A native menu on the content (`menu_active(true)`) keeps it expanded
//!   without catchers; the menu closing (`menu_active(false)`) retracts it.
//!   Expanded, it holds the dock's `tray-expanded` / `<kind>-expanded` hover
//!   token, released on every retraction and on `close()`.

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};

use crate::dbus::dbusmenu::RemoteMenu;
use crate::dbus::sni::{pixmap_rgba, pixmaps_from_variant, Action, IconSignature, TrayItem};

use super::host::{accessible_description, accessible_name, click, monitors, remove, round_half_even, scroll, surface,
                  transparent_area, ClickEvent, DockApp, DockView, TrayBackend};

// ------------------------------------------------------------- menus

/// A tray item's menu (RemoteMenu in Python; fakes in tests).
pub trait TrayMenu {
    fn popup(&self);
    fn popdown(&self);
    fn close(&self);
    /// The native popover, when there is one.
    fn popover(&self) -> Option<gtk::Popover> { None }
}

pub type MenuClosed = Rc<dyn Fn(&Rc<dyn TrayMenu>)>;

/// dbusmenu.RemoteMenu's widget half: a `gtk::PopoverMenu` (no arrow,
/// `tessel-dock-context-menu` + palette class, parented to the tray button) with a
/// capture-phase ShortcutController built from the menu's shortcuts.
pub struct RemoteMenuPopover {
    pub remote: RemoteMenu,
    pub popover: gtk::PopoverMenu,
    shortcuts: gtk::ShortcutController,
    shortcut_items: RefCell<Vec<gtk::Shortcut>>,
    closed: Cell<bool>,
}

impl RemoteMenuPopover {
    pub fn new(connection: &gio::DBusConnection, bus_name: &str, path: &str, anchor: &gtk::Widget, closed: MenuClosed,
               palette_class: Option<&str>) -> Rc<Self> {
        let remote = RemoteMenu::new(connection, bus_name, path);
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
        let popover = gtk::PopoverMenu::from_model(Some(&remote.model()));
        popover.set_has_arrow(false);
        popover.add_controller(shortcuts.clone());
        popover.set_parent(anchor);
        popover.add_css_class("tessel-dock-context-menu");
        if let Some(class) = palette_class.filter(|class| !class.is_empty()) {
            popover.add_css_class(class);
        }
        popover.insert_action_group("remote", Some(&remote.actions()));
        let menu = Rc::new(Self { remote, popover, shortcuts, shortcut_items: RefCell::default(), closed: Cell::new(false) });
        let weak = Rc::downgrade(&menu);
        menu.popover.connect_closed(move |_| {
            if let Some(menu) = weak.upgrade() {
                let menu: Rc<dyn TrayMenu> = menu;
                closed(&menu);
            }
        });
        let weak = Rc::downgrade(&menu);
        menu.remote.set_on_render(Some(Rc::new(move || {
            if let Some(menu) = weak.upgrade() { menu.render() }
        })));
        let weak = Rc::downgrade(&menu);
        menu.remote.set_on_clicked(Some(Rc::new(move || {
            if let Some(menu) = weak.upgrade() { menu.popover.popdown() }
        })));
        menu.render();
        menu
    }

    fn render(self: &Rc<Self>) {
        for shortcut in self.shortcut_items.take() {
            self.shortcuts.remove_shortcut(&shortcut);
        }
        let mut items = Vec::new();
        for entry in self.remote.shortcuts() {
            let Some(trigger) = gtk::ShortcutTrigger::parse_string(&entry.accel) else { continue };
            let weak = Rc::downgrade(self);
            let ident = entry.ident;
            let action = gtk::CallbackAction::new(move |_widget, _args| {
                let handled = weak.upgrade().is_some_and(|menu| {
                    !menu.closed.get() && menu.remote.activate_shortcut(ident, menu.popover.is_mapped())
                });
                if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
            });
            let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
            self.shortcuts.add_shortcut(shortcut.clone());
            items.push(shortcut);
        }
        self.shortcut_items.replace(items);
        self.popover.insert_action_group("remote", Some(&self.remote.actions()));
        self.popover.set_menu_model(Some(&self.remote.model()));
    }

    pub fn shortcut_count(&self) -> usize { self.shortcut_items.borrow().len() }
}

impl TrayMenu for RemoteMenuPopover {
    fn popup(&self) {
        if self.closed.get() {
            return;
        }
        self.remote.popup_opened();
        self.popover.popup();
    }
    fn popdown(&self) { self.popover.popdown() }
    fn popover(&self) -> Option<gtk::Popover> { Some(self.popover.clone().upcast()) }
    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.remote.close();
        self.popover.popdown();
        if self.popover.parent().is_some() {
            self.popover.unparent();
        }
    }
}

// ------------------------------------------------------------- StatusTray

pub type ItemFilter = Rc<dyn Fn(&TrayItem) -> bool>;
pub type ScreenPosition = Rc<dyn Fn(&gtk::Widget, f64, f64) -> Option<(i32, i32)>>;
pub type MenuFactory = Rc<dyn Fn(&Rc<TrayItem>, &gtk::Button, MenuClosed) -> Option<Rc<dyn TrayMenu>>>;

pub struct TrayOptions {
    pub orientation: gtk::Orientation,
    pub icon_size: i32,
    pub menu_state: Option<Rc<dyn Fn(bool)>>,
    pub palette_class: Option<String>,
    /// Default: `Status != "Passive"`.
    pub item_filter: Option<ItemFilter>,
    pub screen_position: Option<ScreenPosition>,
}

impl Default for TrayOptions {
    fn default() -> Self {
        Self { orientation: gtk::Orientation::Horizontal, icon_size: 22, menu_state: None, palette_class: None,
               item_filter: None, screen_position: None }
    }
}

struct TrayButton {
    button: gtk::Button,
    image: gtk::Image,
    signature: RefCell<Option<IconSignature>>,
}

struct TrayInner {
    root: gtk::Box,
    backend: Rc<dyn TrayBackend>,
    icon_size: Cell<i32>,
    menu_state: RefCell<Rc<dyn Fn(bool)>>,
    palette_class: Option<String>,
    screen_position: Option<ScreenPosition>,
    item_filter: ItemFilter,
    buttons: RefCell<Vec<(String, Rc<TrayButton>)>>,
    menus: RefCell<HashMap<String, (String, Rc<dyn TrayMenu>)>>,
    menu: RefCell<Option<Rc<dyn TrayMenu>>>,
    menu_key: RefCell<Option<String>>,
    closed: Cell<bool>,
    theme: gtk::IconTheme,
    theme_serial: Cell<u64>,
    /// Sized by the system tray's Medium or Large icon size (`set_resized`).
    resized: Cell<bool>,
    /// Private `IconThemePath`s of current items; a full theme is only built
    /// (and shared between views) when the directory has themed copies.
    custom_themes: RefCell<HashMap<String, Option<gtk::IconTheme>>>,
    theme_handler: RefCell<Option<glib::SignalHandlerId>>,
    subscription: Cell<Option<u64>>,
    pixbuf_calls: Cell<u64>,
    menu_factory: RefCell<Option<MenuFactory>>,
}

/// One view per output; item ownership and all subscriptions are shared.
#[derive(Clone)]
pub struct StatusTray(Rc<TrayInner>);

impl StatusTray {
    pub fn new(backend: Rc<dyn TrayBackend>, options: TrayOptions) -> Self {
        let root = gtk::Box::new(options.orientation, 1);
        root.set_homogeneous(true);
        let theme = gtk::IconTheme::for_display(&gdk::Display::default().expect("a GDK display"));
        let tray = Self(Rc::new(TrayInner {
            root, backend, icon_size: Cell::new(options.icon_size),
            menu_state: RefCell::new(options.menu_state.unwrap_or_else(|| Rc::new(|_| {}))),
            palette_class: options.palette_class, screen_position: options.screen_position,
            item_filter: options.item_filter.unwrap_or_else(|| Rc::new(|item: &TrayItem| !item.is_passive())),
            buttons: RefCell::default(), menus: RefCell::default(), menu: RefCell::new(None), menu_key: RefCell::new(None),
            closed: Cell::new(false), theme, theme_serial: Cell::new(0), resized: Cell::new(false), custom_themes: RefCell::default(),
            theme_handler: RefCell::new(None), subscription: Cell::new(None), pixbuf_calls: Cell::new(0),
            menu_factory: RefCell::new(None),
        }));
        let weak = tray.weak();
        let handler = tray.0.theme.connect_changed(move |_| {
            if let Some(tray) = Self::upgrade(&weak) { tray.theme_changed() }
        });
        tray.0.theme_handler.replace(Some(handler));
        tray.0.root.add_css_class("tessel-dock-tray");
        tray.0.root.set_overflow(gtk::Overflow::Hidden);
        let weak = tray.weak();
        let id = tray.0.backend.subscribe(Rc::new(move || {
            if let Some(tray) = Self::upgrade(&weak) { tray.refresh() }
        }));
        tray.0.subscription.set(Some(id));
        tray.refresh();
        tray
    }

    fn weak(&self) -> Weak<TrayInner> { Rc::downgrade(&self.0) }
    fn upgrade(weak: &Weak<TrayInner>) -> Option<Self> { weak.upgrade().map(Self) }

    pub fn widget(&self) -> gtk::Box { self.0.root.clone() }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }
    pub fn icon_size(&self) -> i32 { self.0.icon_size.get() }
    pub fn keys(&self) -> Vec<String> { self.0.buttons.borrow().iter().map(|(key, _)| key.clone()).collect() }
    pub fn buttons(&self) -> Vec<(String, gtk::Button)> {
        self.0.buttons.borrow().iter().map(|(key, entry)| (key.clone(), entry.button.clone())).collect()
    }
    pub fn button(&self, key: &str) -> Option<gtk::Button> { self.entry(key).map(|entry| entry.button.clone()) }
    pub fn image(&self, key: &str) -> Option<gtk::Image> { self.entry(key).map(|entry| entry.image.clone()) }
    pub fn is_empty(&self) -> bool { self.0.buttons.borrow().is_empty() }
    pub fn custom_theme_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.0.custom_themes.borrow().keys().cloned().collect();
        paths.sort();
        paths
    }
    /// The full private icon theme built for `path`, if one was needed.
    pub fn custom_theme(&self, path: &str) -> Option<gtk::IconTheme> {
        self.0.custom_themes.borrow().get(path).cloned().flatten()
    }
    /// How often icons were resolved/decoded (`_pixbuf` calls).
    pub fn pixbuf_calls(&self) -> u64 { self.0.pixbuf_calls.get() }
    /// Replace the `menu_state(active)` callback.
    pub fn set_menu_state(&self, callback: Rc<dyn Fn(bool)>) { self.0.menu_state.replace(callback); }
    fn menu_state(&self, active: bool) {
        let callback = self.0.menu_state.borrow().clone();
        callback(active);
    }
    pub fn menu_open(&self) -> bool { self.0.menu.borrow().is_some() }
    /// The prepared menu of `key` (its DBusMenu popover via `TrayMenu::popover`).
    pub fn menu(&self, key: &str) -> Option<Rc<dyn TrayMenu>> { self.0.menus.borrow().get(key).map(|(_, menu)| menu.clone()) }
    /// Replace `_prepare_menu`'s RemoteMenu construction (tests).
    pub fn set_menu_factory(&self, factory: Option<MenuFactory>) {
        self.0.menu_factory.replace(factory);
        for (_path, menu) in self.0.menus.take().into_values() {
            menu.close();
        }
    }

    fn entry(&self, key: &str) -> Option<Rc<TrayButton>> {
        self.0.buttons.borrow().iter().find(|(known, _)| known == key).map(|(_, entry)| entry.clone())
    }

    /// The icon theme changed: reload every icon, drop private themes.
    pub fn theme_changed(&self) {
        self.0.theme_serial.set(self.0.theme_serial.get() + 1);
        self.0.custom_themes.borrow_mut().clear();
        self.refresh();
    }

    /// The icon file `Gtk.IconTheme(theme name, [IconThemePath] + search path)`
    /// would resolve `name` to (`_pixbuf`'s private theme). A private theme
    /// costs a full icon-theme index (~13 MB for Papirus) per directory, so it
    /// is only built when the directory holds a copy of the icon below its top
    /// level; otherwise the result is derived from the display theme: themed
    /// icons win, then unthemed top-level files in search-path order.
    fn resolve_icon(&self, item: &TrayItem, name: &str, size: i32) -> Option<std::path::PathBuf> {
        let size = theme_lookup_size(size, self.0.resized.get());
        let lookup = |theme: &gtk::IconTheme| {
            theme.lookup_icon(name, &[], size, 1, gtk::TextDirection::Ltr, gtk::IconLookupFlags::empty())
                .file().and_then(|file| file.path())
        };
        let Some(path) = item.string("IconThemePath").filter(|path| !path.is_empty() && Path::new(path).is_dir()) else {
            return lookup(&self.0.theme);
        };
        match private_icon_layout(Path::new(&path), name) {
            PrivateIcons::Nested => {
                let existing = self.0.custom_themes.borrow().get(&path).cloned().flatten();
                let theme = existing.unwrap_or_else(|| shared_private_theme(&self.0.theme, &path));
                self.0.custom_themes.borrow_mut().insert(path, Some(theme.clone()));
                lookup(&theme)
            }
            PrivateIcons::None => {
                self.0.custom_themes.borrow_mut().entry(path).or_insert(None);
                lookup(&self.0.theme)
            }
            PrivateIcons::TopLevel(file) => {
                self.0.custom_themes.borrow_mut().entry(path).or_insert(None);
                // A miss yields the theme's image-missing icon, which the private
                // theme would not return here: only real hits count.
                let main = self.0.theme.has_icon(name).then(|| lookup(&self.0.theme)).flatten();
                let roots = self.0.theme.search_path();
                match main {
                    // A themed hit (below a theme directory) beats unthemed files.
                    Some(found) if !found.parent().is_some_and(|parent| roots.iter().any(|root| root == parent)) => Some(found),
                    // Unthemed: the private directory comes first in the search path.
                    _ => Some(file),
                }
            }
        }
    }

    /// `_pixbuf(properties, prefix, size)`.
    fn pixbuf(&self, item: &TrayItem, prefix: &str, size: i32) -> Option<gdk_pixbuf::Pixbuf> {
        self.0.pixbuf_calls.set(self.0.pixbuf_calls.get() + 1);
        let name = item.string(&format!("{prefix}Name"));
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            if Path::new(&name).is_absolute() {
                if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file_at_scale(&name, size, size, true) {
                    return Some(pixbuf);
                }
            } else if let Some(path) = self.resolve_icon(item, &name, size) {
                if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file_at_scale(&path, size, size, true) {
                    return Some(pixbuf);
                }
            }
        }
        let value = item.property(&format!("{prefix}Pixmap"))?;
        let (width, height, rgba) = pixmap_rgba(&pixmaps_from_variant(&value), size as i64)?;
        let (width, height) = (width as i32, height as i32);
        let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(&glib::Bytes::from_owned(rgba), gdk_pixbuf::Colorspace::Rgb, true, 8,
                                                   width, height, width * 4);
        let ratio = size as f64 / width.max(height) as f64;
        pixbuf.scale_simple((round_half_even(width as f64 * ratio) as i32).max(1),
                            (round_half_even(height as f64 * ratio) as i32).max(1), gdk_pixbuf::InterpType::Bilinear)
    }

    #[allow(deprecated)]
    fn update_icon(&self, entry: &TrayButton, item: &TrayItem) {
        let size = self.0.icon_size.get();
        // The lookup changes with `resized` too: count it like a theme change.
        let signature = item.icon_signature(size, self.0.theme_serial.get() * 2 + u64::from(self.0.resized.get()));
        if entry.signature.borrow().as_ref() == Some(&signature) {
            entry.signature.replace(Some(signature));
            return;
        }
        let attention = signature.attention;
        let mut pixbuf = self.pixbuf(item, if attention { "AttentionIcon" } else { "Icon" }, size);
        if pixbuf.is_none() && attention {
            pixbuf = self.pixbuf(item, "Icon", size);
        }
        let overlay = self.pixbuf(item, "OverlayIcon", (size / 2).max(8));
        match pixbuf {
            Some(mut pixbuf) => {
                if let Some(overlay) = overlay {
                    if let Some(copy) = pixbuf.copy() {
                        pixbuf = copy;
                        let (width, height) = (overlay.width(), overlay.height());
                        let x = (pixbuf.width() - width).max(0);
                        let y = (pixbuf.height() - height).max(0);
                        overlay.composite(&pixbuf, x, y, width.min(pixbuf.width()), height.min(pixbuf.height()),
                                          x as f64, y as f64, 1.0, 1.0, gdk_pixbuf::InterpType::Bilinear, 255);
                    }
                }
                entry.image.set_paintable(Some(&gdk::Texture::for_pixbuf(&pixbuf)));
            }
            None => entry.image.set_icon_name(Some("application-x-executable")),
        }
        entry.image.set_pixel_size(size);
        entry.image.set_size_request(size, size);
        entry.signature.replace(Some(signature));
    }

    /// Whether the system tray's Medium or Large icon size sizes this tray:
    /// themed icons are then looked up for an even scale ([`theme_lookup_size`]).
    pub fn set_resized(&self, resized: bool) -> bool {
        if resized == self.0.resized.replace(resized) {
            return false;
        }
        self.refresh();
        true
    }

    pub fn resized(&self) -> bool { self.0.resized.get() }

    /// Resize every tray icon; unchanged sizes cost nothing.
    pub fn set_icon_size(&self, size: i32) -> bool {
        if size == self.0.icon_size.get() {
            return false;
        }
        self.0.icon_size.set(size);
        self.refresh();
        true
    }

    pub fn refresh(&self) {
        if self.0.closed.get() {
            return;
        }
        let filter = self.0.item_filter.clone();
        let visible: Vec<Rc<TrayItem>> =
            self.0.backend.items().into_iter().filter(|item| item.ready() && filter(item)).collect();
        let paths: Vec<String> = visible.iter().filter_map(|item| item.string("IconThemePath")).collect();
        self.0.custom_themes.borrow_mut().retain(|path, _| paths.contains(path));
        let stale: Vec<String> = self.keys().into_iter().filter(|key| !visible.iter().any(|item| &item.key() == key)).collect();
        for key in stale {
            if self.0.menu_key.borrow().as_deref() == Some(key.as_str()) {
                self.close_menu();
            }
            let menu = self.0.menus.borrow_mut().remove(&key);
            if let Some((_path, menu)) = menu {
                menu.close();
            }
            let entry = {
                let mut buttons = self.0.buttons.borrow_mut();
                let index = buttons.iter().position(|(known, _)| *known == key);
                index.map(|index| buttons.remove(index).1)
            };
            if let Some(entry) = entry {
                remove(&entry.button);
            }
        }
        for item in visible {
            let key = item.key();
            let entry = match self.entry(&key) {
                Some(entry) => entry,
                None => {
                    let entry = self.create_button(&key);
                    self.0.buttons.borrow_mut().push((key.clone(), entry.clone()));
                    self.0.root.append(&entry.button);
                    entry
                }
            };
            let tooltip = item.tooltip_text();
            entry.button.set_tooltip_text(Some(&tooltip));
            self.prepare_menu(&item);
            accessible_name(&entry.button, tooltip.split('\n').next().unwrap_or(""));
            accessible_description(&entry.button, &tooltip);
            if item.needs_attention() {
                entry.button.add_css_class("needs-attention");
            } else {
                entry.button.remove_css_class("needs-attention");
            }
            self.update_icon(&entry, &item);
            entry.button.set_visible(true);
        }
    }

    fn create_button(&self, key: &str) -> Rc<TrayButton> {
        let button = gtk::Button::new();
        button.set_focusable(false);
        button.set_overflow(gtk::Overflow::Hidden);
        button.add_css_class("tessel-dock-tray-item");
        let image = gtk::Image::new();
        image.set_halign(gtk::Align::Center);
        image.set_valign(gtk::Align::Center);
        button.set_child(Some(&image));
        let (weak, ident) = (self.weak(), key.to_owned());
        click(&button, 0, false, true, move |widget, event| {
            Self::upgrade(&weak).is_some_and(|tray| tray.press(widget, event, &ident))
        });
        let (weak, ident) = (self.weak(), key.to_owned());
        scroll(&button, move |_widget, dx, dy| Self::upgrade(&weak).is_some_and(|tray| tray.scroll(dx, dy, &ident)));
        Rc::new(TrayButton { button, image, signature: RefCell::new(None) })
    }

    fn press(&self, button: &gtk::Widget, event: ClickEvent, key: &str) -> bool {
        let Some(item) = self.0.backend.item(key) else { return false };
        if !(1..=3).contains(&event.button) {
            return false;
        }
        let context = event.button == 3 || (event.button == 1 && item.boolean("ItemIsMenu"));
        if context && self.open_menu(&item) {
            return true;
        }
        // SNI methods take screen coordinates, not the point within the button.
        let position = match &self.0.screen_position {
            Some(position) => position(button, event.x, event.y),
            None => Some((0, 0)),
        };
        let Some((x, y)) = position else { return true };
        if context {
            // Application-drawn menus have no host-side close notification:
            // release panel menus and expansion catchers before handing off.
            self.close_menu();
            self.menu_state(true);
            self.menu_state(false);
            self.0.backend.action(&item, Action::ContextMenu { x, y }, None);
        } else if event.button == 2 {
            self.0.backend.action(&item, Action::SecondaryActivate { x, y }, None);
        } else {
            let (weak, target) = (self.weak(), item.clone());
            self.0.backend.action(&item, Action::Activate { x, y }, Some(Box::new(move || {
                if let Some(tray) = Self::upgrade(&weak) {
                    tray.open_menu(&target);
                }
            })));
        }
        true
    }

    fn scroll(&self, dx: f64, dy: f64, key: &str) -> bool {
        let Some(item) = self.0.backend.item(key) else { return false };
        let (delta, orientation) = if dy.abs() >= dx.abs() {
            (-round_half_even(dy * 120.0), "vertical")
        } else {
            (-round_half_even(dx * 120.0), "horizontal")
        };
        if delta != 0 {
            self.0.backend.action(&item, Action::Scroll { delta: delta as i32, orientation: orientation.into() }, None);
        }
        true
    }

    fn prepare_menu(&self, item: &Rc<TrayItem>) -> Option<Rc<dyn TrayMenu>> {
        let key = item.key();
        let path = item.string("Menu");
        let cached = self.0.menus.borrow().get(&key).cloned();
        if let Some((known, menu)) = &cached {
            if Some(known) == path.as_ref() {
                return Some(menu.clone());
            }
            if self.0.menu_key.borrow().as_deref() == Some(key.as_str()) {
                self.close_menu();
            }
            self.0.menus.borrow_mut().remove(&key);
            menu.close();
        }
        let path = path?;
        if self.0.closed.get() || path == "/" || !glib::Variant::is_object_path(&path) {
            return None;
        }
        let button = self.entry(&key)?.button.clone();
        let weak = self.weak();
        let closed: MenuClosed = Rc::new(move |menu| {
            if let Some(tray) = Self::upgrade(&weak) { tray.menu_deactivated(menu) }
        });
        let factory = self.0.menu_factory.borrow().clone();
        let menu: Rc<dyn TrayMenu> = match factory {
            Some(factory) => factory(item, &button, closed)?,
            None => {
                let connection = self.0.backend.connection()
                    .or_else(|| gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok())?;
                RemoteMenuPopover::new(&connection, &item.name, &path, button.upcast_ref(), closed,
                                       self.0.palette_class.as_deref())
            }
        };
        self.0.menus.borrow_mut().insert(key, (path, menu.clone()));
        Some(menu)
    }

    fn open_menu(&self, item: &Rc<TrayItem>) -> bool {
        let Some(menu) = self.prepare_menu(item) else { return false };
        self.close_menu();
        self.0.menu.replace(Some(menu.clone()));
        self.0.menu_key.replace(Some(item.key()));
        self.menu_state(true);
        menu.popup();
        true
    }

    pub fn close_menu(&self) {
        let menu = self.0.menu.take();
        if let Some(menu) = menu {
            self.0.menu_key.replace(None);
            menu.popdown();
            self.menu_state(false);
        }
    }

    /// A menu popover closed by itself (GTK grab, item click).
    pub fn menu_deactivated(&self, menu: &Rc<dyn TrayMenu>) {
        let current = self.0.menu.borrow().as_ref().is_some_and(|open| Rc::ptr_eq(open, menu));
        if current {
            self.0.menu.replace(None);
            self.0.menu_key.replace(None);
            self.menu_state(false);
        }
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.close_menu();
        for (_path, menu) in self.0.menus.take().into_values() {
            menu.close();
        }
        self.0.custom_themes.borrow_mut().clear();
        if let Some(id) = self.0.subscription.take() {
            self.0.backend.unsubscribe(id);
        }
        if let Some(handler) = self.0.theme_handler.take() {
            self.0.theme.disconnect(handler);
        }
    }
}

// ------------------------------------------------------------- TrayExpander

/// A hole in the dismissal surfaces: (monitor, x, y, width, height), monitor-local.
pub type Hole = (gdk::Monitor, i32, i32, i32, i32);

#[derive(Default)]
pub struct ExpanderOptions {
    /// "system_tray" (default: the `tessel-dock-tray-dismiss` namespace and the
    /// `tray-expanded` token) or e.g. "applications".
    pub kind: Option<String>,
    pub available: Option<Rc<dyn Fn() -> bool>>,
    /// Default: the group named `kind` is visible.
    pub enabled: Option<Rc<dyn Fn() -> bool>>,
    pub close_content: Option<Rc<dyn Fn()>>,
    pub extra_holes: Option<Rc<dyn Fn() -> Vec<Hole>>>,
    /// An existing button that toggles the expansion; the expander then has no
    /// arrow button of its own and does not change this button's look.
    pub toggle: Option<gtk::Button>,
    /// Called after every expansion change with the new state.
    pub on_change: Option<Rc<dyn Fn(bool)>>,
}

struct ExpanderInner {
    root: gtk::Box,
    dock: Weak<dyn DockView>,
    app: Rc<dyn DockApp>,
    content: gtk::Widget,
    kind: String,
    available: Rc<dyn Fn() -> bool>,
    enabled: Rc<dyn Fn() -> bool>,
    close_content: Rc<dyn Fn()>,
    extra_holes: Option<Rc<dyn Fn() -> Vec<Hole>>>,
    hover_token: String,
    namespace: String,
    show_title: String,
    hide_title: String,
    expanded: Cell<bool>,
    closed: Cell<bool>,
    backdrops: RefCell<Vec<(gtk::Window, gdk::Monitor)>>,
    menu_active: Cell<bool>,
    #[allow(clippy::type_complexity)]
    region_signature: RefCell<Option<(Vec<Hole>, Vec<(gdk::Monitor, i32, i32)>)>>,
    monitor_signature: RefCell<Vec<gdk::Monitor>>,
    button: gtk::Button,
    arrow: gtk::Image,
    /// The button belongs to the owner (`ExpanderOptions::toggle`).
    external: bool,
    on_change: Option<Rc<dyn Fn(bool)>>,
}

/// Inline icons with a temporary, non-keyboard outside-click catcher.
#[derive(Clone)]
pub struct TrayExpander(Rc<ExpanderInner>);

impl PartialEq for TrayExpander {
    fn eq(&self, other: &Self) -> bool { Rc::ptr_eq(&self.0, &other.0) }
}

impl TrayExpander {
    pub fn new(dock: &Rc<dyn DockView>, app: &Rc<dyn DockApp>, content: &gtk::Widget, options: ExpanderOptions) -> Self {
        let kind = options.kind.unwrap_or_else(|| "system_tray".into());
        let tray = kind == "system_tray";
        let enabled = options.enabled.unwrap_or_else(|| {
            let (weak, kind) = (Rc::downgrade(dock), kind.clone());
            Rc::new(move || weak.upgrade().is_some_and(|dock| dock.config().is_group_visible(&kind)))
        });
        let root = gtk::Box::new(dock.orientation(), 1);
        let external = options.toggle.is_some();
        let button = options.toggle.unwrap_or_default();
        let arrow = gtk::Image::new();
        arrow.set_pixel_size(16);
        // The arrow points toward the screen: physical, not text direction.
        arrow.set_direction(gtk::TextDirection::Ltr);
        if !external {
            button.set_child(Some(&arrow));
        }
        let show_title = if tray { tessel_core::tr!("Show system tray") } else { tessel_core::tr!("Show applications") };
        let hide_title = if tray { tessel_core::tr!("Hide system tray") } else { tessel_core::tr!("Hide applications") };
        let expander = Self(Rc::new(ExpanderInner {
            root, dock: Rc::downgrade(dock), app: app.clone(), content: content.clone(),
            available: options.available.unwrap_or_else(|| Rc::new(|| true)), enabled,
            close_content: options.close_content.unwrap_or_else(|| Rc::new(|| {})),
            extra_holes: options.extra_holes,
            hover_token: if tray { "tray-expanded".into() } else { format!("{kind}-expanded") },
            namespace: if tray { "tessel-dock-tray-dismiss".into() } else { format!("tessel-dock-{kind}-dismiss") },
            show_title: show_title.clone(), hide_title, kind, expanded: Cell::new(false), closed: Cell::new(false),
            backdrops: RefCell::default(), menu_active: Cell::new(false), region_signature: RefCell::new(None),
            monitor_signature: RefCell::default(), button, arrow, external, on_change: options.on_change,
        }));
        expander.update_icon();
        let inner = &expander.0;
        if !external {
            inner.button.add_css_class("tessel-dock-tray-expander");
            if !tray {
                inner.button.add_css_class("tessel-dock-applications-expander");
            }
            inner.button.set_tooltip_text(Some(&show_title));
            accessible_name(&inner.button, &show_title);
        }
        let weak = Rc::downgrade(&expander.0);
        inner.button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let expander = Self(inner);
                expander.set_expanded(!expander.expanded());
            }
        });
        if !external {
            inner.root.append(&inner.button);
            inner.root.append(content);
        }
        content.set_visible(false);
        expander
    }

    pub fn widget(&self) -> gtk::Box { self.0.root.clone() }
    pub fn button(&self) -> gtk::Button { self.0.button.clone() }
    pub fn arrow(&self) -> gtk::Image { self.0.arrow.clone() }
    pub fn content(&self) -> gtk::Widget { self.0.content.clone() }
    pub fn kind(&self) -> &str { &self.0.kind }
    pub fn expanded(&self) -> bool { self.0.expanded.get() }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }
    pub fn menu_is_active(&self) -> bool { self.0.menu_active.get() }
    pub fn backdrops(&self) -> Vec<gtk::Window> { self.0.backdrops.borrow().iter().map(|(w, _)| w.clone()).collect() }

    fn dock(&self) -> Option<Rc<dyn DockView>> { self.0.dock.upgrade() }

    fn update_icon(&self) {
        if self.0.external {
            return;
        }
        let edge = self.dock().map(|dock| dock.config().edge.clone()).unwrap_or_default();
        let (collapsed, expanded) = match edge.as_str() {
            "top" => ("pan-down-symbolic", "pan-up-symbolic"),
            "left" => ("pan-end-symbolic", "pan-start-symbolic"),
            "right" => ("pan-start-symbolic", "pan-end-symbolic"),
            _ => ("pan-up-symbolic", "pan-down-symbolic"),
        };
        self.0.arrow.set_icon_name(Some(if self.expanded() { expanded } else { collapsed }));
    }

    fn fullscreen(&self) -> bool { self.0.app.fullscreen_active() }

    pub fn set_expanded(&self, expanded: bool) {
        let dock = self.dock();
        let destroyed = dock.as_ref().is_none_or(|dock| dock.destroyed());
        let mut expanded = expanded && !self.0.closed.get() && !destroyed;
        if expanded && (!(self.0.available)() || self.fullscreen() || !(self.0.enabled)()) {
            expanded = false;
        }
        if expanded == self.0.expanded.get() {
            return;
        }
        let Some(dock) = dock else {
            // The owner is gone: only release what is still held.
            self.0.expanded.set(false);
            self.remove_backdrops();
            return;
        };
        if expanded {
            // One dismissal surface set at a time, including cloned docks.
            for other in self.0.app.docks() {
                let expanders = [other.indicators().and_then(|panel| panel.tray_expander()), other.application_expander()];
                for expander in expanders.into_iter().flatten() {
                    if expander != *self {
                        expander.set_expanded(false);
                    }
                }
            }
            dock.close_preview();
            dock.close_app_menu();
        }
        self.0.expanded.set(expanded);
        self.update_icon();
        if !self.0.external {
            let tooltip = if expanded { &self.0.hide_title } else { &self.0.show_title };
            self.0.button.set_tooltip_text(Some(tooltip));
            accessible_name(&self.0.button, tooltip);
        }
        self.0.content.set_visible(expanded);
        if expanded {
            dock.enter(&self.0.hover_token);
            self.create_backdrops();
        } else {
            self.remove_backdrops();
            (self.0.close_content)();
            dock.leave(&self.0.hover_token);
        }
        if let Some(on_change) = &self.0.on_change {
            on_change(expanded);
        }
        if !dock.destroyed() {
            dock.queue_panel_layout();
        }
    }

    fn create_backdrops(&self) {
        if self.0.closed.get() || !self.expanded() || self.0.menu_active.get() {
            return;
        }
        let Some(dock) = self.dock() else { return };
        self.remove_backdrops();
        let outputs = monitors(Some(&WidgetExt::display(&dock.window())));
        self.0.monitor_signature.replace(outputs.clone());
        let application = self.0.app.application();
        for monitor in outputs {
            let backdrop = surface(application.as_ref(), &monitor, &self.0.namespace, Layer::Top, false);
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                backdrop.set_anchor(edge, true);
            }
            let catcher = transparent_area();
            catcher.set_hexpand(true);
            catcher.set_vexpand(true);
            backdrop.set_child(Some(&catcher));
            // Consuming release matches popup dismissal; nothing is replayed below.
            let weak = Rc::downgrade(&self.0);
            click(&backdrop, 0, true, false, move |_, _| {
                if let Some(inner) = weak.upgrade() { Self(inner).set_expanded(false) }
                true
            });
            let weak = Rc::downgrade(&self.0);
            backdrop.connect_map(move |_| {
                if let Some(inner) = weak.upgrade() { Self(inner).refresh_regions(true) }
            });
            self.0.backdrops.borrow_mut().push((backdrop.clone(), monitor));
            backdrop.set_visible(true);
        }
        self.refresh_regions(true);
    }

    fn refresh_regions(&self, force: bool) {
        let mut holes: Vec<Hole> = Vec::new();
        for dock in self.0.app.docks() {
            if dock.destroyed() || !dock.window().get_visible() {
                continue;
            }
            let (_width, _height, x, y) = dock.panel_frost_geometry();
            let (width, height) = dock.dock_dimensions();
            holes.push((dock.monitor(), x, y, width, height));
        }
        if let Some(extra) = &self.0.extra_holes {
            holes.extend(extra());
        }
        let backdrops = self.0.backdrops.borrow().clone();
        let outputs: Vec<(gdk::Monitor, i32, i32)> = backdrops.iter()
            .map(|(_, monitor)| (monitor.clone(), monitor.geometry().width(), monitor.geometry().height())).collect();
        let signature = (holes.clone(), outputs);
        if !force && self.0.region_signature.borrow().as_ref() == Some(&signature) {
            return;
        }
        self.0.region_signature.replace(Some(signature));
        for (backdrop, monitor) in backdrops {
            let Some(native) = backdrop.surface() else { continue };
            let geometry = monitor.geometry();
            let region = gtk::cairo::Region::create_rectangle(
                &gtk::cairo::RectangleInt::new(0, 0, geometry.width(), geometry.height()));
            for (hole_monitor, x, y, width, height) in &holes {
                if *hole_monitor == monitor {
                    let _ = region.subtract_rectangle(&gtk::cairo::RectangleInt::new(*x, *y, *width, *height));
                }
            }
            native.set_input_region(Some(&region));
            // GDK sends the region with the next commit: draw one, or a dock
            // that grew after the catchers appeared stays partly covered.
            native.queue_render();
        }
    }

    fn remove_backdrops(&self) {
        let backdrops = self.0.backdrops.take();
        self.0.region_signature.replace(None);
        self.0.monitor_signature.replace(Vec::new());
        for (backdrop, _) in backdrops {
            backdrop.destroy();
        }
    }

    /// A native menu (its own grab) opened or closed on this expander's content.
    pub fn menu_active(&self, active: bool) {
        self.0.menu_active.set(active);
        if active {
            // Never place input catchers in front of native menus/submenus.
            self.remove_backdrops();
        } else if self.expanded() {
            self.set_expanded(false);
        }
    }

    /// Re-validate after layout/visibility/monitor changes.
    pub fn sync(&self) {
        if self.0.closed.get() || !self.expanded() {
            return;
        }
        let dock = self.dock();
        let gone = dock.as_ref().is_none_or(|dock| dock.destroyed() || !dock.window().get_visible());
        if gone || !(self.0.available)() || self.fullscreen() || !(self.0.enabled)() {
            self.set_expanded(false);
            return;
        }
        if self.0.menu_active.get() {
            return;
        }
        let dock = dock.unwrap();
        if monitors(Some(&WidgetExt::display(&dock.window()))) != *self.0.monitor_signature.borrow() {
            self.create_backdrops();
        } else {
            self.refresh_regions(false);
        }
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.set_expanded(false);
        self.0.closed.set(true);
        self.remove_backdrops();
    }
}

/// Where a private `IconThemePath` provides `name`.
enum PrivateIcons {
    /// Not at all: the private theme would resolve exactly like the display theme.
    None,
    /// Only as an unthemed top-level file (svg, png, symbolic png: GTK's order).
    TopLevel(std::path::PathBuf),
    /// Below a subdirectory (a themed copy), or the directory is too large to tell.
    Nested,
}

/// The icon theme size to look a tray icon up at for `size` px.
///
/// At the original sizes (`resized` false) the size itself. A Medium or
/// Large tray (`resized`) avoids the 22 and 24 px panel sizes, which themes
/// draw with wide margins: up to 24 px the icon comes from the 16 px size,
/// scaled up at most 1.5 times; above that from the next larger common size,
/// scaled down. So a bigger choice never shows a smaller picture, and none
/// shows a smaller one than the original 16 and 22 px sizes.
pub fn theme_lookup_size(size: i32, resized: bool) -> i32 {
    const LARGER: [i32; 6] = [32, 48, 64, 96, 128, 256];
    if !resized {
        size
    } else if size <= 24 {
        16.min(size)
    } else {
        LARGER.iter().copied().find(|known| *known >= size).unwrap_or(size)
    }
}

fn private_icon_layout(root: &Path, name: &str) -> PrivateIcons {
    const MAX_ENTRIES: usize = 20_000;
    let files = [format!("{name}.svg"), format!("{name}.png"), format!("{name}.symbolic.png")];
    let mut top = None;
    for file in &files {
        let candidate = root.join(file);
        if top.is_none() && candidate.is_file() {
            top = Some(candidate);
        }
    }
    let mut pending: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return PrivateIcons::Nested;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let is_dir = kind.is_dir() || (kind.is_symlink() && entry.path().is_dir());
            if is_dir {
                if depth < 6 {
                    pending.push((entry.path(), depth + 1));
                }
            } else if depth > 0 && files.iter().any(|file| entry.file_name() == file.as_str()) {
                return PrivateIcons::Nested;
            }
        }
    }
    top.map_or(PrivateIcons::None, PrivateIcons::TopLevel)
}

thread_local! {
    /// Private themes shared by every tray view (one per output), keyed by
    /// directory, theme name and the display theme's search path.
    static PRIVATE_THEMES: RefCell<HashMap<(String, Option<String>, Vec<std::path::PathBuf>), glib::WeakRef<gtk::IconTheme>>> =
        RefCell::new(HashMap::new());
}

/// `Gtk.IconTheme.new()` with the settings' theme name and `[path] + search path`.
fn shared_private_theme(display_theme: &gtk::IconTheme, path: &str) -> gtk::IconTheme {
    let name = gtk::Settings::default().and_then(|settings| settings.gtk_icon_theme_name()).map(|n| n.to_string());
    let mut search = vec![std::path::PathBuf::from(path)];
    search.extend(display_theme.search_path());
    let key = (path.to_owned(), name.clone(), search.clone());
    PRIVATE_THEMES.with(|themes| {
        let mut themes = themes.borrow_mut();
        themes.retain(|_, weak| weak.upgrade().is_some());
        if let Some(theme) = themes.get(&key).and_then(|weak| weak.upgrade()) {
            return theme;
        }
        let theme = gtk::IconTheme::new();
        if let Some(name) = &name {
            theme.set_theme_name(Some(name));
        }
        let refs: Vec<&Path> = search.iter().map(|p| p.as_path()).collect();
        theme.set_search_path(&refs);
        let weak = glib::WeakRef::new();
        weak.set(Some(&theme));
        themes.insert(key, weak);
        theme
    })
}
