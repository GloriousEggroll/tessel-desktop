//! Native GTK4 layer-shell popup placement, click-away dismissal and the shared
//! popup layout (tray_popup.py).

use crate::placement::{dock_placement, PointerMonitor};
use crate::utils::{css_once, PRIORITY_APPLICATION};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use regex::Regex;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::LazyLock;
use std::time::Duration;

struct Inner {
    window: gtk::Window,
    apply_state: Box<dyn Fn()>,
    edge: RefCell<String>,
    offset: Cell<i32>,
    group: RefCell<String>,
    align: RefCell<String>,
    probe: RefCell<Option<PointerMonitor>>,
    show_generation: Cell<i64>,
    requested_generation: Cell<i64>,
    layer_shell: bool,
    backdrops: RefCell<Vec<(gdk::Monitor, gtk::Window)>>,
    focus_check: RefCell<Option<glib::SourceId>>,
    children: RefCell<Vec<gtk::Window>>,
    backdrop_css: RefCell<Option<gtk::CssProvider>>,
    monitors: gio::ListModel,
    monitor_handler: RefCell<Option<glib::SignalHandlerId>>,
    raise_window: RefCell<Option<Rc<dyn Fn(&gtk::Window)>>>,
}

/// Places `window` as an OVERLAY layer popup (namespace `desktop-applet`) and
/// dismisses it on Escape, close-request, a click on the per-monitor
/// transparent backdrops (namespace `desktop-applet-dismiss`) or a monitor
/// change. `show` follows the pointer's output and Tessel Dock's placement.
#[derive(Clone)]
pub struct PopupController(Rc<Inner>);

fn init_layer(window: &gtk::Window, namespace: &str) {
    window.init_layer_shell();
    window.set_namespace(Some(namespace));
    window.set_layer(Layer::Overlay);
    window.set_exclusive_zone(-1);
}

impl PopupController {
    /// `apply_state` is the owner's `_apply_state` (refresh contents before present).
    /// Python defaults: `edge="bottom"`, `offset=76`, `group="service_apps"`.
    pub fn new(window: &gtk::Window, apply_state: impl Fn() + 'static, edge: &str, offset: i32, group: &str) -> Self {
        let monitors = WidgetExt::display(window).monitors();
        let controller = Self(Rc::new(Inner {
            window: window.clone(), apply_state: Box::new(apply_state), edge: RefCell::new(edge.into()),
            offset: Cell::new(offset), group: RefCell::new(group.into()), align: RefCell::new("end".into()),
            probe: RefCell::new(None), show_generation: Cell::new(0), requested_generation: Cell::new(-1),
            layer_shell: gtk4_layer_shell::is_supported(), backdrops: RefCell::new(Vec::new()),
            focus_check: RefCell::new(None), children: RefCell::new(Vec::new()), backdrop_css: RefCell::new(None),
            monitors, monitor_handler: RefCell::new(None), raise_window: RefCell::new(None),
        }));
        window.set_decorated(false);
        window.set_resizable(false);
        window.add_css_class("tray-popup");
        if controller.0.layer_shell {
            init_layer(window, "desktop-applet");
            window.set_keyboard_mode(KeyboardMode::OnDemand);
            controller.position_layer();
        }
        let weak = controller.downgrade();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(this) = upgrade(&weak) { this.dismiss() }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(keys);
        let weak = controller.downgrade();
        window.connect_is_active_notify(move |_| if let Some(this) = upgrade(&weak) { this.on_focus_changed() });
        let weak = controller.downgrade();
        window.connect_close_request(move |_| {
            if let Some(this) = upgrade(&weak) { this.dismiss() }
            glib::Propagation::Stop
        });
        let weak = controller.downgrade();
        window.connect_unrealize(move |_| if let Some(this) = upgrade(&weak) { this.on_destroy(true) });
        controller.watch_monitors();
        let weak = controller.downgrade();
        window.connect_realize(move |_| if let Some(this) = upgrade(&weak) { this.watch_monitors() });
        controller
    }

    fn downgrade(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    pub fn window(&self) -> &gtk::Window { &self.0.window }
    pub fn edge(&self) -> String { self.0.edge.borrow().clone() }
    pub fn offset(&self) -> i32 { self.0.offset.get() }
    pub fn align(&self) -> String { self.0.align.borrow().clone() }
    pub fn group(&self) -> String { self.0.group.borrow().clone() }
    /// Tessel Dock placement group (`service_apps`, `weather`, `clock`, …) used by the next `show`.
    pub fn set_group(&self, group: &str) { self.0.group.replace(group.into()); }
    /// The per-monitor click-away windows (for tests).
    pub fn backdrops(&self) -> Vec<gtk::Window> { self.0.backdrops.borrow().iter().map(|e| e.1.clone()).collect() }
    /// Hook used by `activate` to raise a restored child window (Python: `application._raise_window`).
    pub fn set_raise_window(&self, raise: impl Fn(&gtk::Window) + 'static) {
        self.0.raise_window.replace(Some(Rc::new(raise)));
    }

    fn watch_monitors(&self) {
        if self.0.monitor_handler.borrow().is_none() {
            let weak = self.downgrade();
            let id = self.0.monitors.connect_items_changed(move |_, _, _, _| {
                if let Some(this) = upgrade(&weak) {
                    if this.0.window.get_visible() {
                        this.dismiss();
                    }
                }
            });
            self.0.monitor_handler.replace(Some(id));
        }
    }

    fn ensure_backdrops(&self) {
        if !self.0.layer_shell {
            return;
        }
        let monitors = &self.0.monitors;
        let current: Vec<gdk::Monitor> = (0..monitors.n_items())
            .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>()).collect();
        self.0.backdrops.borrow_mut().retain(|(monitor, backdrop)| {
            let keep = current.contains(monitor);
            if !keep {
                backdrop.destroy();
            }
            keep
        });
        if self.0.backdrop_css.borrow().is_none() {
            let provider = gtk::CssProvider::new();
            provider.load_from_string("window.desktop-applet-backdrop { background: transparent; border: none; box-shadow: none; }");
            gtk::style_context_add_provider_for_display(&WidgetExt::display(&self.0.window), &provider, PRIORITY_APPLICATION + 40);
            self.0.backdrop_css.replace(Some(provider));
        }
        for monitor in current {
            if self.0.backdrops.borrow().iter().any(|(known, _)| *known == monitor) {
                continue;
            }
            let backdrop = gtk::Window::builder().decorated(false).resizable(true).build();
            backdrop.add_css_class("desktop-applet-backdrop");
            init_layer(&backdrop, "desktop-applet-dismiss");
            backdrop.set_monitor(Some(&monitor));
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                backdrop.set_anchor(edge, true);
            }
            backdrop.set_keyboard_mode(KeyboardMode::None);
            let area = monitor.geometry();
            backdrop.set_default_size(area.width(), area.height());
            // An empty transparent GTK4 Box does not attach a Wayland buffer and
            // cannot receive pointer events. A Cairo node creates a real surface.
            let catcher = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
            catcher.set_draw_func(|_, cr, _, _| {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
                let _ = cr.paint();
            });
            backdrop.set_child(Some(&catcher));
            let click = gtk::GestureClick::new();
            click.set_button(0);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = self.downgrade();
            click.connect_released(move |_, _, _, _| if let Some(this) = upgrade(&weak) { this.dismiss() });
            backdrop.add_controller(click);
            self.0.backdrops.borrow_mut().push((monitor, backdrop));
        }
    }

    fn position_layer(&self) {
        crate::placement::anchor_popup(&self.0.window, &self.edge(), self.offset(), &self.align());
    }

    /// True while an asynchronous show is placing the popup.
    ///
    /// Owners that close a popup when it hides must not close it for the
    /// hide that `set_position()` or a monitor move does just before it.
    pub fn show_pending(&self) -> bool {
        self.0.requested_generation.get() == self.0.show_generation.get()
    }

    /// Start a show generation: any older pending show/probe becomes stale.
    pub fn begin_show(&self) -> i64 {
        self.cancel_focus_check();
        let generation = self.0.show_generation.get() + 1;
        self.0.show_generation.set(generation);
        self.0.requested_generation.set(generation);
        if let Some(probe) = self.0.probe.borrow().as_ref() {
            probe.cancel();
        }
        generation
    }

    /// Resolve the pointer's output, ask Tessel Dock for placement, then present.
    pub fn show(&self) {
        let generation = self.begin_show();
        let weak = self.downgrade();
        let probe = PointerMonitor::new(move |monitor| {
            let Some(this) = upgrade(&weak) else { return };
            this.0.probe.replace(None);
            let weak = this.downgrade();
            let group = this.group();
            let probed = monitor.clone();
            dock_placement(probed.as_ref(), &group, move |placement| {
                let Some(this) = upgrade(&weak) else { return };
                if generation != this.0.show_generation.get() {
                    return;
                }
                if this.0.layer_shell {
                    let window = &this.0.window;
                    if window.monitor() != monitor {
                        window.set_visible(false);
                        window.set_monitor(monitor.as_ref());
                    }
                    if let Some(placement) = placement {
                        this.0.edge.replace(placement.edge);
                        this.0.offset.set(placement.offset);
                        this.0.align.replace(placement.align);
                    }
                    this.position_layer();
                }
                this.show_now();
            });
        });
        self.0.probe.replace(Some(probe));
    }

    fn show_now(&self) {
        self.0.requested_generation.set(-1);
        self.ensure_backdrops();
        for backdrop in self.backdrops() {
            backdrop.set_visible(true);
        }
        (self.0.apply_state)();
        self.0.window.present();
    }

    /// Tray click: raise a suspended child window, else toggle the popup.
    pub fn activate(&self) {
        let child = self.0.children.borrow().iter().rev().find(|w| w.get_visible()).cloned();
        if let Some(child) = child {
            self.dismiss();
            child.unminimize();
            child.present();
            if let Some(raise) = self.0.raise_window.borrow().clone() {
                glib::idle_add_local_once(move || raise(&child));
            }
        } else if self.0.window.get_visible() {
            self.dismiss();
        } else {
            self.show();
        }
    }

    pub fn set_position(&self, edge: &str, offset: i32) {
        if *self.0.edge.borrow() == edge && self.offset() == offset {
            return;
        }
        self.dismiss();
        self.0.edge.replace(edge.into());
        self.0.offset.set(offset);
        if self.0.layer_shell {
            self.position_layer();
        }
    }

    pub fn dismiss(&self) {
        self.0.show_generation.set(self.0.show_generation.get() + 1);
        if let Some(probe) = self.0.probe.take() {
            probe.cancel();
        }
        self.cancel_focus_check();
        self.0.window.set_visible(false);
        for backdrop in self.backdrops() {
            backdrop.set_visible(false);
        }
    }

    /// Hide while `window` (a dialog opened from the popup) is open; `activate`
    /// raises it instead of reopening the popup.
    pub fn suspend_for_child(&self, window: &gtk::Window) {
        self.dismiss();
        if !self.0.children.borrow().contains(window) {
            self.0.children.borrow_mut().push(window.clone());
            let weak = self.downgrade();
            window.connect_unrealize(move |window| {
                if let Some(this) = upgrade(&weak) {
                    this.0.children.borrow_mut().retain(|child| child != window);
                }
            });
        }
    }

    fn on_focus_changed(&self) {
        self.cancel_focus_check();
        // A layer-shell popup can lose keyboard focus just by crossing outputs.
        // Its click-away surfaces and Escape handle dismissal without letting
        // that focus change unmap the input catcher before an outside click.
        if self.0.layer_shell {
            return;
        }
        if self.0.window.get_visible() && !self.0.window.is_active() {
            let weak = self.downgrade();
            let id = glib::timeout_add_local_once(Duration::from_millis(150), move || {
                let Some(this) = upgrade(&weak) else { return };
                this.0.focus_check.replace(None);
                let toplevels = gtk::Window::toplevels();
                let active = (0..toplevels.n_items()).filter_map(|i| toplevels.item(i).and_downcast::<gtk::Window>())
                    .any(|w| w.get_visible() && w.is_active());
                if !active {
                    this.dismiss();
                }
            });
            self.0.focus_check.replace(Some(id));
        }
    }

    fn cancel_focus_check(&self) {
        if let Some(id) = self.0.focus_check.take() {
            id.remove();
        }
    }

    fn on_destroy(&self, from_signal: bool) {
        self.0.show_generation.set(self.0.show_generation.get() + 1);
        if let Some(probe) = self.0.probe.take() {
            probe.cancel();
        }
        self.cancel_focus_check();
        if let Some(id) = self.0.monitor_handler.take() {
            self.0.monitors.disconnect(id);
        }
        let backdrops: Vec<gtk::Window> = self.0.backdrops.take().into_iter().map(|e| e.1).collect();
        let destroy = move || backdrops.iter().for_each(|b| b.destroy());
        if from_signal {
            // Do not recursively tear down layer-shell windows from another
            // surface's unrealize handler: finish its Wayland dispatch first.
            glib::idle_add_local_once(destroy);
        } else {
            destroy();
        }
        if let Some(provider) = self.0.backdrop_css.take() {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.0.window), &provider);
        }
    }

    pub fn close(&self) {
        self.dismiss();
        self.on_destroy(false);
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<PopupController> {
    weak.upgrade().map(PopupController)
}

// Shared popup layout. Every tray popup uses the same width, header, section
// titles, row text and footer so the applets read as one family.
pub const POPUP_WIDTH: i32 = 400;
pub const POPUP_MAX_WIDTH: i32 = 420;
pub const POPUP_PADDING: i32 = 16;

pub const POPUP_LAYOUT_CSS: &str = "
window.tray-popup .popup-header { min-height: 42px; }
window.tray-popup .popup-icon-tile {
    min-width: 42px; min-height: 42px; border-radius: 8px;
    color: @theme_selected_bg_color;
    background-color: alpha(@theme_selected_bg_color, 0.14);
}
window.tray-popup .popup-title { font-size: 16pt; font-weight: 600; }
window.tray-popup .popup-subtitle { font-size: 9pt; opacity: 0.7; }
window.tray-popup .section-title { font-size: 10pt; font-weight: 600; opacity: 0.7; }
window.tray-popup .row-title { font-size: 10.5pt; }
window.tray-popup .row-detail { font-size: 9pt; opacity: 0.7; }
window.tray-popup .popup-surface { border-radius: 8px; }
window.tray-popup .card, window.tray-popup .popup-card, window.tray-popup .power-card,
window.tray-popup .device-list { border-radius: 8px; }
window.tray-popup .device-name { font-size: 10.5pt; font-weight: 600; }
window.tray-popup .device-detail { font-size: 9pt; }
window.tray-popup .popup-footer { margin-top: 4px; }
";

/// The palette theme paints every button with a field color above the
/// application priority, so the flat chrome buttons need a slightly higher one.
pub const POPUP_FLAT_CSS: &str = "
window.tray-popup .popup-icon-tile { color: inherit; }
window.tray-popup .popup-icon-tile image { color: inherit; opacity: 1; -gtk-icon-filter: none; }
window.tray-popup button.popup-header-button {
    min-width: 32px; min-height: 32px; padding: 0; border-radius: 9999px;
    background: none; border: none; box-shadow: none;
}
window.tray-popup button.popup-header-button:focus:not(:focus-visible) { outline: none; }
window.tray-popup button.popup-flat-icon,
window.tray-popup button.popup-flat-icon:focus,
window.tray-popup button.popup-flat-icon:backdrop {
    background: none; border: none; box-shadow: none;
}
window.tray-popup button.popup-flat-icon:hover,
window.tray-popup button.popup-header-button:hover,
window.tray-popup button.popup-flat:not(.suggested-action):hover {
    background-color: alpha(currentColor, 0.10);
}
window.tray-popup button.popup-header-button:checked { background-color: alpha(currentColor, 0.16); }
window.tray-popup button.popup-flat:not(.suggested-action) {
    background: none; border: none; box-shadow: none;
}
window.tray-popup button.popup-flat:focus:not(:focus-visible) { outline: none; }
window.tray-popup button.popup-flat { min-height: 30px; padding: 2px 12px; border-radius: 8px; }
window.tray-popup button.popup-chip {
    min-height: 26px; padding: 3px 8px; border-radius: 8px; font-size: 9pt;
    background-image: none; background-color: alpha(currentColor, 0.07); border: none; box-shadow: none;
}
window.tray-popup button.popup-chip:hover { background-color: alpha(currentColor, 0.13); }
window.tray-popup button.popup-chip:active { background-color: alpha(currentColor, 0.18); }
window.tray-popup button.popup-chip:focus:not(:focus-visible) { outline: none; }
";

/// Load the shared popup stylesheets once per display (APPLICATION+5 and +35).
pub fn install_popup_css(display: Option<&gdk::Display>) {
    let Some(display) = display.cloned().or_else(gdk::Display::default) else { return };
    css_once(&display, "popup-layout", POPUP_LAYOUT_CSS, PRIORITY_APPLICATION + 5);
    css_once(&display, "popup-flat", POPUP_FLAT_CSS, PRIORITY_APPLICATION + 35);
}

/// Give a popup window the shared fixed width (never above the cap). Python default: `POPUP_WIDTH`.
pub fn set_popup_width(window: &gtk::Window, width: i32) -> i32 {
    let width = width.min(POPUP_MAX_WIDTH);
    window.set_default_size(width, -1);
    window.set_size_request(width, -1);
    install_popup_css(Some(&WidgetExt::display(window)));
    width
}

/// A flat, round icon button for the right side of a popup header. With
/// `toggle` it is a `gtk::ToggleButton` and `callback` runs on "toggled".
pub fn header_button(icon_name: &str, tooltip: &str, callback: Option<Box<dyn Fn()>>, toggle: bool) -> gtk::Button {
    let button: gtk::Button = if toggle { gtk::ToggleButton::new().upcast() } else { gtk::Button::new() };
    let image = gtk::Image::from_icon_name(icon_name);
    image.set_pixel_size(16);
    button.set_child(Some(&image));
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    for name in ["popup-header-button", "icon-button", "flat"] {
        button.add_css_class(name);
    }
    if let Some(callback) = callback {
        if let Some(toggle) = button.downcast_ref::<gtk::ToggleButton>() {
            toggle.connect_toggled(move |_| callback());
        } else {
            button.connect_clicked(move |_| callback());
        }
    }
    button
}

const ADWAITA_SYMBOLIC: &str = "/usr/share/icons/Adwaita/symbolic";

thread_local!(static ADWAITA_FILES: RefCell<Option<HashMap<String, String>>> = const { RefCell::new(None) });

/// Path of Adwaita's recolorable glyph for `name`, if installed.
pub fn adwaita_symbolic(name: &str) -> Option<String> {
    ADWAITA_FILES.with(|files| {
        let mut files = files.borrow_mut();
        let files = files.get_or_insert_with(|| {
            let mut found = HashMap::new();
            let mut stack = vec![std::path::PathBuf::from(ADWAITA_SYMBOLIC)];
            while let Some(directory) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&directory) else { continue };
                let mut entries: Vec<_> = entries.flatten().collect();
                entries.sort_by_key(|e| e.file_name());
                for entry in entries {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if let Some(file) = path.file_name().and_then(|f| f.to_str()) {
                        if let Some(stem) = file.strip_suffix(".svg").filter(|s| s.ends_with("-symbolic")) {
                            found.entry(stem.to_owned()).or_insert_with(|| path.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            found
        });
        files.get(name).cloned()
    })
}

/// Show the first available icon of `names`. With `glyph` the icon is chrome
/// that must follow the text color: some themes (breeze) ship "-symbolic"
/// files with fixed dark colors, so the always-recolorable Adwaita glyph is
/// preferred when it exists.
pub fn set_symbolic_icon(image: &gtk::Image, names: &[&str], glyph: bool) {
    if glyph {
        for name in names {
            if let Some(path) = adwaita_symbolic(name) {
                image.set_from_gicon(&gio::FileIcon::new(&gio::File::for_path(path)));
                return;
            }
        }
    }
    image.set_from_gicon(&gio::ThemedIcon::from_names(names));
}

/// An entry of `popup_header(actions=…)`.
pub enum HeaderAction {
    Widget(gtk::Widget),
    /// `(icon_name, tooltip, callback)`: becomes a [`header_button`].
    Button(String, String, Option<Box<dyn Fn()>>),
}

/// The shared header: tinted icon tile, title and subtitle, flat actions.
#[derive(Clone)]
pub struct PopupHeader {
    pub root: gtk::Box,
    pub tile: gtk::Box,
    pub icon: gtk::Image,
    pub title_label: gtk::Label,
    pub subtitle_label: gtk::Label,
    pub actions_box: gtk::Box,
}

impl PopupHeader {
    pub fn set_subtitle(&self, text: &str) {
        self.subtitle_label.set_text(text);
        self.subtitle_label.set_visible(!text.is_empty());
    }
}

/// `icon_names` is a symbolic icon name and optional alternatives.
pub fn popup_header(icon_names: &[&str], title: &str, subtitle: Option<&str>, actions: Vec<HeaderAction>) -> PopupHeader {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.add_css_class("popup-header");
    // Explicitly not expanding: the centered icon inside must not make the
    // tile take a share of the header width.
    let tile = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).hexpand(false).build();
    tile.add_css_class("popup-icon-tile");
    tile.add_css_class("settings-tile");
    let icon = gtk::Image::new();
    set_symbolic_icon(&icon, icon_names, true);
    icon.set_pixel_size(24);
    icon.set_hexpand(true);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    tile.append(&icon);
    root.append(&tile);
    let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true)
        .valign(gtk::Align::Center).build();
    let title_label = gtk::Label::builder().label(title).xalign(0.0).build();
    title_label.add_css_class("popup-title");
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    titles.append(&title_label);
    let subtitle = subtitle.unwrap_or("");
    let subtitle_label = gtk::Label::builder().label(subtitle).xalign(0.0).build();
    subtitle_label.add_css_class("popup-subtitle");
    subtitle_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle_label.set_visible(!subtitle.is_empty());
    titles.append(&subtitle_label);
    root.append(&titles);
    let actions_box = gtk::Box::builder().spacing(4).valign(gtk::Align::Center).build();
    let has_actions = !actions.is_empty();
    for action in actions {
        let widget: gtk::Widget = match action {
            HeaderAction::Widget(widget) => widget,
            HeaderAction::Button(icon, tooltip, callback) => header_button(&icon, &tooltip, callback, false).upcast(),
        };
        actions_box.append(&widget);
    }
    actions_box.set_visible(has_actions);
    root.append(&actions_box);
    PopupHeader { root, tile, icon, title_label, subtitle_label, actions_box }
}

pub fn section_title(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).build();
    label.add_css_class("section-title");
    label
}

pub fn footer_button(label: &str, callback: Option<Box<dyn Fn()>>, primary: bool, icon_name: Option<&str>) -> gtk::Button {
    let button = gtk::Button::new();
    if let Some(icon_name) = icon_name.filter(|name| !name.is_empty()) {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&gtk::Image::from_icon_name(icon_name));
        content.append(&gtk::Label::new(Some(label)));
        button.set_child(Some(&content));
    } else {
        button.set_label(label);
    }
    button.add_css_class("popup-flat");
    button.add_css_class(if primary { "suggested-action" } else { "flat" });
    if let Some(callback) = callback {
        button.connect_clicked(move |_| callback());
    }
    button
}

/// A flat button row aligned to the end; give at most one `primary`.
pub fn popup_footer(buttons: &[Option<gtk::Widget>]) -> gtk::Box {
    let footer = gtk::Box::builder().spacing(6).halign(gtk::Align::End).build();
    footer.add_css_class("popup-footer");
    for button in buttons.iter().flatten() {
        if !button.has_css_class("popup-flat") {
            button.add_css_class("popup-flat");
            if !button.has_css_class("suggested-action") {
                button.add_css_class("flat");
            }
        }
        footer.append(button);
    }
    footer
}

static RAW_DETAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\[Errno -?\d+\]|Traceback|GDBus\.Error|\b\w*(?:Error|Exception):|(?:^|[\s:'"(])(?:~|/)[\w.+-]+/[\w./+-]+"#).unwrap()
});

/// Keep raw exceptions and file paths out of popups.
///
/// `mapping` holds `(substring, friendly text)` pairs checked first. Any other
/// message with an errno, exception name or path becomes `fallback`. The
/// original text is logged (domain `tray-popup`) so the details stay available.
pub fn friendly_status(message: &str, fallback: &str, mapping: &[(&str, &str)]) -> String {
    let text = message.trim();
    if text.is_empty() {
        return String::new();
    }
    for (needle, friendly) in mapping {
        if text.contains(needle) {
            glib::g_warning!("tray-popup", "{}", text);
            return (*friendly).to_owned();
        }
    }
    if RAW_DETAIL.is_match(text) {
        glib::g_warning!("tray-popup", "{}", text);
        return fallback.to_owned();
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_status_hides_raw_details() {
        assert_eq!(friendly_status("  ", "x", &[]), "");
        assert_eq!(friendly_status("Connected", "x", &[]), "Connected");
        assert_eq!(friendly_status("[Errno 2] No such file", "Failed", &[]), "Failed");
        assert_eq!(friendly_status("GDBus.Error:org.x: nope", "Failed", &[]), "Failed");
        assert_eq!(friendly_status("ValueError: bad", "Failed", &[]), "Failed");
        assert_eq!(friendly_status("Cannot open /etc/foo/bar", "Failed", &[]), "Failed");
        assert_eq!(friendly_status("Cannot open ~/x/y", "Failed", &[]), "Cannot open ~/x/y");
        assert_eq!(friendly_status("Cannot open ~/.config/x", "Failed", &[]), "Cannot open ~/.config/x");
        assert_eq!(friendly_status("Use a/b paths", "Failed", &[]), "Use a/b paths");
        assert_eq!(friendly_status("Timeout was reached", "Failed", &[("Timeout", "Took too long")]), "Took too long");
    }
}
