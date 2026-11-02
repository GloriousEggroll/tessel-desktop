//! GTK helpers shared by the application menu and the window preview:
//! [`PopupHost`] (what both need from the dock surface), `surface()`,
//! `icon_image()`, the pin-star CSS, `position_popup` application, the
//! transparent dismissal catcher and the small gtk4_support.py helpers.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::collections::HashSet;

use super::logic::{popup_margins, PopupPlacement};

/// Which popup `position_popup` places (the host resolves the anchor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PopupAnchor {
    /// The application menu (`position_popup(menu_window)`; a separate app
    /// dock anchors it at its visible menu button with "start" alignment).
    Menu,
    /// A window preview over the item's button (`item_anchor(item_id)`).
    Item(String),
}

/// What the app menu and preview read from the dock surface (`DockSurface`).
pub trait PopupHost {
    /// `self.app` (the `Gtk.Application` new popups belong to).
    fn application(&self) -> Option<gtk::Application>;
    /// `self.monitor`.
    fn monitor(&self) -> gdk::Monitor;
    /// `config.edge`: "top" | "bottom" | "left" | "right".
    fn edge(&self) -> String;
    /// `self.horizontal`.
    fn horizontal(&self) -> bool {
        matches!(self.edge().as_str(), "top" | "bottom")
    }
    /// `self.popup_class` (`tessel-dock-palette-<id>`); the menu adds `-applications`.
    fn popup_class(&self) -> String;
    /// Geometry for `position_popup`; see [`PopupPlacement`].
    fn popup_placement(&self, anchor: &PopupAnchor) -> PopupPlacement;
    /// `_set_popup_blur(popup)` (menu: dock frost/rounding; preview: app popups').
    fn set_popup_blur(&self, popup: &gtk::Window, menu: bool);
    /// `self.enter(zone)` / `self.leave(zone)` hover holds ("menu", "preview", "drag", "drop").
    fn enter(&self, zone: &str);
    fn leave(&self, zone: &str);
    /// `self.destroyed`.
    fn destroyed(&self) -> bool;
    /// `self.app.catalog`.
    fn catalog(&self) -> std::rc::Rc<RefCell<crate::core::app_catalog::AppCatalog>>;
}

/// Pin stars stay out of the way until a row is hovered, selected or
/// focused; pinned applications always show their filled star.
pub const APP_MENU_PIN_CSS: &str = "
.tessel_dock-app-menu flowboxchild button.tessel_dock-app-pin:not(.pinned) { opacity: 0; }
.tessel_dock-app-menu flowboxchild:hover button.tessel_dock-app-pin,
.tessel_dock-app-menu flowboxchild:selected button.tessel_dock-app-pin,
.tessel_dock-app-menu flowboxchild:focus-within button.tessel_dock-app-pin { opacity: 1; }
";

thread_local! {
    static PIN_DISPLAYS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// `install_app_menu_pin_style(display)` (once per display, APPLICATION+3).
pub fn install_app_menu_pin_style(display: Option<&gdk::Display>) {
    let Some(display) = display else { return };
    let key = display.as_ptr() as usize;
    if PIN_DISPLAYS.with(|set| !set.borrow_mut().insert(key)) {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(APP_MENU_PIN_CSS);
    gtk::style_context_add_provider_for_display(display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 3);
}

/// `icon_image(icon, size)`: a gicon (fallback `application-x-executable`).
pub fn icon_image(icon: Option<&gio::Icon>, size: i32) -> gtk::Image {
    let fallback: gio::Icon = gio::ThemedIcon::new("application-x-executable").upcast();
    let image = gtk::Image::from_gicon(icon.unwrap_or(&fallback));
    image.set_pixel_size(size);
    image
}

type MenuIconKey = (usize, String, i32, i32, bool);

thread_local! {
    /// Application-menu icon paintables, keyed by (display, serialized gicon,
    /// size, scale, RTL). In GTK 4.22 every fresh `GtkImage` gicon lookup
    /// re-loads the icon and the process keeps ~0.5 MB per menu of that
    /// churn, so rebuilding the menu on each open grew the dock by several
    /// MB per ten menus; reusing the looked-up paintables keeps repeated
    /// menus at a constant footprint. Cleared when the icon theme changes.
    static MENU_ICONS: RefCell<std::collections::HashMap<MenuIconKey, gtk::IconPaintable>> =
        RefCell::new(std::collections::HashMap::new());
    /// Icons shown uncached, looked up later at low priority (so the first
    /// open stays as fast as GtkImage's lazy gicon path).
    static MENU_ICON_PENDING: RefCell<Vec<(MenuIconKey, gio::Icon)>> = const { RefCell::new(Vec::new()) };
    static MENU_ICON_FILL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static MENU_ICON_THEMES: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// Look up queued menu icons a few at a time while the main loop is idle.
fn schedule_menu_icon_fill() {
    if MENU_ICON_FILL.with(|f| f.replace(true)) {
        return;
    }
    // Start after the menu's first frames, then continue whenever idle.
    glib::timeout_add_local_once(std::time::Duration::from_millis(200), || {
        glib::idle_add_local_full(glib::Priority::LOW, fill_menu_icons);
    });
}

fn fill_menu_icons() -> glib::ControlFlow {
    let batch: Vec<(MenuIconKey, gio::Icon)> = MENU_ICON_PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let at = pending.len().saturating_sub(8);
        pending.split_off(at)
    });
    for (key, icon) in batch {
        if MENU_ICONS.with(|c| c.borrow().contains_key(&key)) {
            continue;
        }
        let Some(display) = gdk::DisplayManager::get().list_displays().into_iter().find(|d| d.as_ptr() as usize == key.0) else { continue };
        let direction = if key.4 { gtk::TextDirection::Rtl } else { gtk::TextDirection::Ltr };
        let paintable = gtk::IconTheme::for_display(&display).lookup_by_gicon(&icon, key.2, key.3, direction, gtk::IconLookupFlags::empty());
        MENU_ICONS.with(|c| c.borrow_mut().insert(key, paintable));
    }
    if MENU_ICON_PENDING.with(|p| p.borrow().is_empty()) {
        MENU_ICON_FILL.with(|f| f.set(false));
        glib::ControlFlow::Break
    } else {
        glib::ControlFlow::Continue
    }
}

/// `icon_image(icon, size)` for the application menu, which is rebuilt on
/// every open. An icon seen before reuses the paintable GtkImage's own
/// lookup would produce (`IconTheme::lookup_by_gicon` with the size, the
/// monitor's scale, the default direction and no forced style); a new one
/// uses the lazy gicon image and is queued for a low-priority lookup.
/// Icons without a string form (bytes icons) are never cached.
pub fn menu_icon_image(icon: Option<&gio::Icon>, size: i32, monitor: &gdk::Monitor) -> gtk::Image {
    let fallback: gio::Icon = gio::ThemedIcon::new("application-x-executable").upcast();
    let icon = icon.unwrap_or(&fallback);
    let Some(serialized) = IconExt::to_string(icon) else { return icon_image(Some(icon), size) };
    let display = monitor.display();
    let display_key = display.as_ptr() as usize;
    let scale = monitor.scale_factor().max(1);
    let rtl = gtk::Widget::default_direction() == gtk::TextDirection::Rtl;
    let key: MenuIconKey = (display_key, serialized.to_string(), size, scale, rtl);
    if MENU_ICON_THEMES.with(|set| set.borrow_mut().insert(display_key)) {
        gtk::IconTheme::for_display(&display).connect_changed(move |_| {
            MENU_ICONS.with(|cache| cache.borrow_mut().retain(|k, _| k.0 != display_key));
            MENU_ICON_PENDING.with(|p| p.borrow_mut().retain(|(k, _)| k.0 != display_key));
        });
    }
    if let Some(paintable) = MENU_ICONS.with(|cache| cache.borrow().get(&key).cloned()) {
        let image = gtk::Image::from_paintable(Some(&paintable));
        image.set_pixel_size(size);
        return image;
    }
    MENU_ICON_PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        if !pending.iter().any(|(k, _)| *k == key) {
            pending.push((key, icon.clone()));
        }
    });
    schedule_menu_icon_fill();
    icon_image(Some(icon), size)
}

/// Number of cached application-menu icon paintables (tests).
pub fn menu_icon_cache_len() -> usize {
    MENU_ICONS.with(|cache| cache.borrow().len())
}

/// `icon_image("name", size)` (a themed icon by name).
pub fn named_image(name: &str, size: i32) -> gtk::Image {
    icon_image(Some(&gio::ThemedIcon::new(name).upcast()), size)
}

/// `dock_ui.surface(application, monitor, namespace, layer, keyboard)`.
pub fn surface(application: Option<&gtk::Application>, monitor: &gdk::Monitor, namespace: &str, layer: Layer, keyboard: bool) -> gtk::Window {
    let window = gtk::Window::new();
    if let Some(application) = application {
        window.set_application(Some(application));
    }
    window.set_decorated(false);
    window.set_resizable(true);
    window.add_css_class("tessel-dock-surface");
    window.init_layer_shell();
    window.set_namespace(Some(namespace));
    window.set_monitor(Some(monitor));
    window.set_layer(layer);
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(if keyboard { KeyboardMode::OnDemand } else { KeyboardMode::None });
    window
}

/// The transparent full-output catcher of a `tessel-dock-menu-dismiss` surface.
pub fn transparent_area() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
    area.set_draw_func(|_area, context, _w, _h| {
        context.set_operator(gtk::cairo::Operator::Source);
        context.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = context.paint();
    });
    area
}

/// `position_popup(popup, anchor, alignment)` with the host's geometry.
pub fn position_popup(host: &dyn PopupHost, popup: &gtk::Window, anchor: &PopupAnchor) {
    let placement = host.popup_placement(anchor);
    let horizontal = placement.horizontal();
    let edge = match placement.edge.as_str() {
        "top" => Edge::Top,
        "left" => Edge::Left,
        "right" => Edge::Right,
        _ => Edge::Bottom,
    };
    let orientation = if horizontal { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
    let length = popup.measure(orientation, -1).1;
    let (edge_margin, side_margin) = popup_margins(&placement, length);
    popup.set_anchor(edge, true);
    // Popups clear the magnified icons as well as the panel.
    popup.set_margin(edge, edge_margin);
    let side = if horizontal { Edge::Left } else { Edge::Top };
    popup.set_anchor(side, true);
    popup.set_margin(side, side_margin);
}

/// Close the popup's blur binding (`attach_blur(popup).close()`).
pub fn close_blur(popup: &gtk::Window) {
    tessel_gtk::blur::attach(popup, Some("tessel-dock-popup"), true, false).close();
}

/// `gtk4_support.pack(box, widget, expand, fill, padding)` (padding 0).
pub fn pack(container: &gtk::Box, widget: &impl IsA<gtk::Widget>, expand: bool) {
    if container.orientation() == gtk::Orientation::Horizontal {
        widget.set_hexpand(expand);
    } else {
        widget.set_vexpand(expand);
    }
    container.append(widget);
}

/// `gtk4_support.name(widget, text)` (accessible label, updated only on change).
pub fn accessible_name(widget: &impl IsA<gtk::Accessible>, text: &str) {
    let object = widget.upcast_ref::<gtk::Accessible>();
    // SAFETY: this key only ever stores a String.
    let current = unsafe { object.data::<String>("tessel-dock-accessible-name").map(|p| p.as_ref().clone()) };
    if current.as_deref() != Some(text) {
        object.update_property(&[gtk::accessible::Property::Label(text)]);
        unsafe { object.set_data("tessel-dock-accessible-name", text.to_owned()) };
    }
}

/// The accessible label last set with [`accessible_name`].
pub fn accessible_label(widget: &impl IsA<gtk::Accessible>) -> Option<String> {
    unsafe { widget.upcast_ref::<gtk::Accessible>().data::<String>("tessel-dock-accessible-name").map(|p| p.as_ref().clone()) }
}

/// `DockSurface.toggle_class(widget, name, enabled)`.
pub fn toggle_class(widget: &impl IsA<gtk::Widget>, name: &str, enabled: bool) {
    if widget.has_css_class(name) != enabled {
        if enabled {
            widget.add_css_class(name);
        } else {
            widget.remove_css_class(name);
        }
    }
}

/// `gtk4_support.children(widget)`.
pub fn children(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut result = Vec::new();
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        result.push(current);
    }
    result
}

/// `gtk4_support.translate(widget, target, x, y)`.
pub fn translate(widget: &impl IsA<gtk::Widget>, target: &impl IsA<gtk::Widget>, x: f64, y: f64) -> Option<(f64, f64)> {
    widget
        .compute_point(target, &gtk::graphene::Point::new(x as f32, y as f32))
        .map(|p| (p.x() as f64, p.y() as f64))
}

/// `gtk4_support.keys(widget, callback)`: CAPTURE-phase key presses; the
/// callback gets the widget and the key.
pub fn keys(widget: &impl IsA<gtk::Widget>, callback: impl Fn(&gtk::Widget, gdk::Key) -> bool + 'static) -> gtk::EventControllerKey {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |controller, key, _code, _state| {
        let Some(widget) = controller.widget() else { return glib::Propagation::Proceed };
        if callback(&widget, key) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
    });
    widget.add_controller(controller.clone());
    controller
}

/// `gtk4_support.motion(widget, enter, leave)` (dock_ui.crossing).
pub fn crossing(widget: &impl IsA<gtk::Widget>, enter: impl Fn() + 'static, leave: impl Fn() + 'static) {
    let controller = gtk::EventControllerMotion::new();
    controller.connect_enter(move |_c, _x, _y| enter());
    controller.connect_leave(move |_c| leave());
    widget.add_controller(controller);
}

/// `gtk4_support.remove(widget)`.
pub fn remove(widget: &impl IsA<gtk::Widget>) {
    let Some(parent) = widget.parent() else { return };
    if let Some(container) = parent.downcast_ref::<gtk::Box>() {
        container.remove(widget);
    } else if let Some(flow) = parent.downcast_ref::<gtk::FlowBox>() {
        flow.remove(widget);
    } else {
        widget.unparent();
    }
}

/// The key name `grid_key`/`search_key` compare (`Gdk.KEY_*`).
pub fn key_name(key: gdk::Key) -> String {
    key.name().map(|n| n.to_string()).unwrap_or_default()
}

/// `self.window.get_display().get_app_launch_context()` with timestamp 0.
pub fn launch_context(display: &gdk::Display) -> gio::AppLaunchContext {
    let context = display.app_launch_context();
    context.set_timestamp(0);
    context.upcast()
}

/// Every monitor of a display (`gtk4_support.monitors`).
pub fn monitors(display: &gdk::Display) -> Vec<gdk::Monitor> {
    let model = display.monitors();
    (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gdk::Monitor>()).collect()
}
