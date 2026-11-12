//! The seam between the panel widgets (W-f: `indicators`, `tray`, `media`,
//! `capture_controls`) and their owners (W-e: `DockSurface` and the
//! application), plus the small GTK helpers of gtk4_support.py they use.
//!
//! # What the dock surface must provide — [`DockView`]
//! Python widgets call these attributes/methods on `self.dock` (same names):
//! `config`, `window`, `orientation`, `horizontal`, `edge_gap`, `monitor`,
//! `monitor_name`, `destroyed`, `is_app_dock`, `panel`, `popup_class`,
//! `enter(token)` / `leave(token)` (hover holds), `queue_panel_layout`,
//! `refresh_groups`, `add_group(name, parent, widget, available)`,
//! `toggle_class` (default provided), `context_menu`, `popup_menu(anchor,
//! position, entries)`, `close_preview`, `close_app_menu`,
//! `application_expander`, `indicators`, `screen_position(widget, x, y)`,
//! `dock_dimensions`, `panel_frost_geometry`.
//! Widgets keep a `Weak<dyn DockView>`; the surface owns them. Obligations of
//! the surface (as dock_ui.py): build `DockIndicators::new(&rc_self, app)` once
//! the surface is in an `Rc`; `refresh_groups` calls
//! `indicators.sync_apps_visibility()`, merges `group_availability()` and calls
//! `refresh_trays(false)`; every layout pass calls `tray_expander().sync()` and
//! `application_expander.sync()`; `popup_menu` first calls
//! `indicators.tray_menu_opening(anchor)` (keeps the system tray open for a
//! menu inside it, else collapses it) and `application_expander.menu_active(true)`,
//! and its `closed` handler calls `indicators.tray_menu_closed()` and
//! `application_expander.menu_active(false)`; `destroy` calls `indicators.close()`.
//!
//! # What the application must provide — [`DockApp`]
//! `application`, `docks` (every live clone), the status backend
//! (`status`, `status_action`, `audio_scroll`, `media_icon`), `tray_backend`
//! (a [`TrayBackend`]; implemented here for `dbus::sni::StatusTrayBackend`),
//! panel actions (`panel_open`, `panel_capture`), the workspace backend
//! (`workspaces(monitor)`, `activate_workspace`, `workspace_error`),
//! `fullscreen_active` (window backend: an activated, non-minimized fullscreen
//! window exists) and `clock_store` (defaults to `~/.config/tessel/clock.json`).
//!
//! # Helpers (gtk4_support.py)
//! [`click`] (+ [`ClickEvent`]), [`motion`], [`scroll`], [`keys`], [`remove`],
//! [`pack`], [`accessible_name`], [`accessible_description`], [`monitors`],
//! [`translate`], [`surface`] (dock_ui.surface), [`transparent_area`],
//! [`toggle_class`], [`round_half_even`].

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use std::rc::Rc;

use crate::core::config::DockConfig;
use crate::dbus::sni::{Action, StatusTrayBackend, TrayItem};
use crate::status::backend::Status;
use crate::wl::Workspace;

use super::indicators::DockIndicators;
use super::tray::TrayExpander;

/// One row of `DockSurface.popup_menu`: label and its activation.
pub type MenuEntry = (String, Rc<dyn Fn()>);

/// The dock surface (dock_ui.DockSurface) as seen by the panel widgets.
pub trait DockView {
    /// `dock.config` (the per-monitor DockConfig; replaced, never mutated in place).
    fn config(&self) -> Rc<DockConfig>;
    /// `dock.window`: the visual layer-shell window.
    fn window(&self) -> gtk::Window;
    /// `dock.orientation` (horizontal for top/bottom edges).
    fn orientation(&self) -> gtk::Orientation;
    /// `dock.horizontal`.
    fn horizontal(&self) -> bool { self.orientation() == gtk::Orientation::Horizontal }
    /// `dock.edge_gap`: the current float gap between panel and screen edge.
    fn edge_gap(&self) -> i32;
    /// `dock.monitor`.
    fn monitor(&self) -> gdk::Monitor;
    /// `dock.monitor_name` (wl_output name, e.g. "HEADLESS-1").
    fn monitor_name(&self) -> String;
    /// `dock.destroyed`.
    fn destroyed(&self) -> bool;
    /// `dock.is_app_dock` (the separate application dock).
    fn is_app_dock(&self) -> bool { false }
    /// `dock.panel` (its `tessel-dock-compact-bar` class selects 16 px tray icons).
    fn panel(&self) -> Option<gtk::Widget> { None }
    /// `dock.popup_class`: palette scope class for popovers.
    fn popup_class(&self) -> Option<String> { None }
    /// `dock.enter(token)`: hold the dock revealed/raised for `token`.
    fn enter(&self, token: &str);
    /// `dock.leave(token)`.
    fn leave(&self, token: &str);
    /// `dock.queue_panel_layout()`.
    fn queue_panel_layout(&self);
    /// `dock.refresh_groups()` (reads `DockIndicators::group_availability`).
    fn refresh_groups(&self);
    /// `dock.add_group(name, parent, widget, available=True)`: like dock_ui.py,
    /// pack `widget` into `parent` (after a group separator) and show it, then
    /// register the group. `available == false` means availability comes from
    /// `DockIndicators::group_availability()`.
    fn add_group(&self, name: &str, parent: &gtk::Box, widget: &gtk::Widget, available: bool);
    /// `dock.toggle_class(widget, css, enabled)`.
    fn toggle_class(&self, widget: &gtk::Widget, css: &str, enabled: bool) { toggle_class(widget, css, enabled) }
    /// `dock.context_menu`: the open `tessel-dock-context-menu` popover, if any.
    fn context_menu(&self) -> Option<gtk::Popover>;
    /// `dock.popup_menu(anchor, event, entries)`: open a context menu (a
    /// `gtk::Popover` whose child is a vertical `gtk::Box` of buttons) and make
    /// it `context_menu()`. `position` is the anchor-local pointer point.
    fn popup_menu(&self, anchor: &gtk::Widget, position: Option<(f64, f64)>, entries: Vec<MenuEntry>);
    /// `dock.close_preview()`.
    fn close_preview(&self);
    /// `dock.close_app_menu()`.
    fn close_app_menu(&self);
    /// `dock.application_expander` (dock-mode title display), if any.
    fn application_expander(&self) -> Option<TrayExpander> { None }
    /// `dock.indicators`, once constructed.
    fn indicators(&self) -> Option<DockIndicators>;
    /// `dock.screen_position(widget, x, y)`: widget-local point → global
    /// compositor (screen) coordinates; `None` when it cannot be computed.
    fn screen_position(&self, widget: &gtk::Widget, x: f64, y: f64) -> Option<(i32, i32)>;
    /// `dock.dock_dimensions()`: (width, height) of the visible panel.
    fn dock_dimensions(&self) -> (i32, i32);
    /// `dock._panel_frost_geometry()`: (width, height, x, y), x/y monitor-local.
    fn panel_frost_geometry(&self) -> (i32, i32, i32, i32);
}

/// The StatusNotifier host as seen by `StatusTray` (status_tray.StatusTrayBackend).
pub trait TrayBackend {
    /// `backend.items.values()` in registration order.
    fn items(&self) -> Vec<Rc<TrayItem>>;
    fn item(&self, key: &str) -> Option<Rc<TrayItem>> { self.items().into_iter().find(|item| item.key() == key) }
    fn subscribe(&self, changed: Rc<dyn Fn()>) -> u64;
    fn unsubscribe(&self, id: u64);
    /// `backend.action(item, method, x, y, delta, orientation, failed)`.
    fn action(&self, item: &Rc<TrayItem>, action: Action, failed: Option<Box<dyn FnOnce()>>) -> bool;
    /// `backend.connection` (for DBusMenu); `None` disables menus.
    fn connection(&self) -> Option<gio::DBusConnection>;
}

impl TrayBackend for StatusTrayBackend {
    fn items(&self) -> Vec<Rc<TrayItem>> { StatusTrayBackend::items(self) }
    fn item(&self, key: &str) -> Option<Rc<TrayItem>> { StatusTrayBackend::item(self, key) }
    fn subscribe(&self, changed: Rc<dyn Fn()>) -> u64 { StatusTrayBackend::subscribe(self, changed) }
    fn unsubscribe(&self, id: u64) { StatusTrayBackend::unsubscribe(self, id) }
    fn action(&self, item: &Rc<TrayItem>, action: Action, failed: Option<Box<dyn FnOnce()>>) -> bool {
        StatusTrayBackend::action(self, item, action, failed)
    }
    fn connection(&self) -> Option<gio::DBusConnection> { StatusTrayBackend::connection(self) }
}

/// The application (tessel-dock.TesselDockApplication) as seen by the panel widgets.
pub trait DockApp {
    /// The `gtk::Application` new surfaces belong to.
    fn application(&self) -> Option<gtk::Application>;
    /// `app.docks` / `app.surfaces`: every live dock clone (for the single
    /// dismissal set and capture controls).
    fn docks(&self) -> Vec<Rc<dyn DockView>>;
    /// `app.status_backend.status` (StatusBackend::status()).
    fn status(&self) -> Status;
    /// `app.status_backend.action(name)`.
    fn status_action(&self, name: &str) -> bool;
    /// `app.status_backend.audio_scroll(step)`.
    fn audio_scroll(&self, step: f64);
    /// `app.status_backend.media_status.icon` (StatusBackend::media_icon()).
    fn media_icon(&self) -> Option<gio::Icon>;
    /// `app.tray_backend` (None: no tray widgets are built).
    fn tray_backend(&self) -> Option<Rc<dyn TrayBackend>>;
    /// `app.panel_actions.open(name, settings=, edge=, offset=)`.
    fn panel_open(&self, name: &str, settings: bool, edge: &str, offset: i64) -> bool;
    /// `app.panel_actions.capture(action)`.
    fn panel_capture(&self, action: &str) -> bool;
    /// `app.workspace_backend.sorted_workspaces(monitor)`; `None` = no backend.
    fn workspaces(&self, monitor: &str) -> Option<Vec<Workspace>>;
    /// `app.workspace_backend.activate(id)`.
    fn activate_workspace(&self, id: &str) -> bool;
    /// `app.workspace_backend.error` ("" = none).
    fn workspace_error(&self) -> String;
    /// TrayExpander._fullscreen: an activated, non-minimized fullscreen window exists.
    fn fullscreen_active(&self) -> bool;
    /// `ClockPreferencesStore()` for the clock.
    fn clock_store(&self) -> tessel_core::clock::ClockPreferencesStore { tessel_core::clock::ClockPreferencesStore::default() }
}

// ---------------------------------------------------------------- helpers

/// gtk4_support.click's event namespace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickEvent {
    pub button: u32,
    pub x: f64,
    pub y: f64,
}

/// `click(widget, callback, button=, release=, capture=)`: a GestureClick
/// whose callback returning true claims the sequence.
pub fn click(widget: &impl IsA<gtk::Widget>, button: u32, release: bool, capture: bool,
             callback: impl Fn(&gtk::Widget, ClickEvent) -> bool + 'static) -> gtk::GestureClick {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(button);
    gesture.set_propagation_phase(if capture { gtk::PropagationPhase::Capture } else { gtk::PropagationPhase::Bubble });
    let received = move |controller: &gtk::GestureClick, _count: i32, x: f64, y: f64| {
        let event = ClickEvent { button: controller.current_button(), x, y };
        if let Some(widget) = controller.widget() {
            if callback(&widget, event) {
                controller.set_state(gtk::EventSequenceState::Claimed);
            }
        }
    };
    if release { gesture.connect_released(received); } else { gesture.connect_pressed(received); }
    widget.add_controller(gesture.clone());
    gesture
}

/// `motion(widget, enter, leave)`.
pub fn motion(widget: &impl IsA<gtk::Widget>, enter: impl Fn() + 'static, leave: impl Fn() + 'static)
              -> gtk::EventControllerMotion {
    let controller = gtk::EventControllerMotion::new();
    controller.connect_enter(move |_, _, _| enter());
    controller.connect_leave(move |_| leave());
    widget.add_controller(controller.clone());
    controller
}

/// `scroll(widget, callback)` (both axes; true stops propagation).
pub fn scroll(widget: &impl IsA<gtk::Widget>, callback: impl Fn(&gtk::Widget, f64, f64) -> bool + 'static)
              -> gtk::EventControllerScroll {
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    controller.connect_scroll(move |controller, dx, dy| match controller.widget() {
        Some(widget) if callback(&widget, dx, dy) => glib::Propagation::Stop,
        _ => glib::Propagation::Proceed,
    });
    widget.add_controller(controller.clone());
    controller
}

/// `keys(widget, callback)` (capture phase).
pub fn keys(widget: &impl IsA<gtk::Widget>, callback: impl Fn(&gtk::Widget, gdk::Key) -> bool + 'static)
            -> gtk::EventControllerKey {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |controller, key, _code, _state| match controller.widget() {
        Some(widget) if callback(&widget, key) => glib::Propagation::Stop,
        _ => glib::Propagation::Proceed,
    });
    widget.add_controller(controller.clone());
    controller
}

/// `remove(widget)`: detach from a Box/FlowBox, else unparent.
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

/// `pack(box, widget, expand, fill, padding)`.
pub fn pack(container: &gtk::Box, widget: &impl IsA<gtk::Widget>, expand: bool, _fill: bool, padding: i32) {
    let horizontal = container.orientation() == gtk::Orientation::Horizontal;
    if horizontal { widget.set_hexpand(expand) } else { widget.set_vexpand(expand) }
    if padding != 0 {
        if horizontal {
            widget.set_margin_start(padding);
            widget.set_margin_end(padding);
        } else {
            widget.set_margin_top(padding);
            widget.set_margin_bottom(padding);
        }
    }
    container.append(widget);
}

const NAME_KEY: &str = "tessel-dock-accessible-name";
const DESCRIPTION_KEY: &str = "tessel-dock-accessible-description";

fn cached_property(widget: &impl IsA<gtk::Widget>, key: &str, text: &str) -> bool {
    // SAFETY: the key is only ever used with `String` values in this module.
    unsafe {
        if widget.data::<String>(key).is_some_and(|known| known.as_ref() == text) {
            return false;
        }
        widget.set_data(key, text.to_owned());
    }
    true
}

/// gtk4_support.name: set the accessible label (only when it changed).
pub fn accessible_name(widget: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>), text: &str) {
    if cached_property(widget, NAME_KEY, text) {
        widget.update_property(&[gtk::accessible::Property::Label(text)]);
    }
}

/// gtk4_support.describe: full state text for screen readers.
pub fn accessible_description(widget: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>), text: &str) {
    if cached_property(widget, DESCRIPTION_KEY, text) {
        widget.update_property(&[gtk::accessible::Property::Description(text)]);
    }
}

/// The last accessible label set through [`accessible_name`].
pub fn accessible_name_of(widget: &impl IsA<gtk::Widget>) -> Option<String> {
    // SAFETY: see `cached_property`.
    unsafe { widget.data::<String>(NAME_KEY).map(|value| value.as_ref().clone()) }
}

/// `monitors(display)`.
pub fn monitors(display: Option<&gdk::Display>) -> Vec<gdk::Monitor> {
    let display = match display {
        Some(display) => display.clone(),
        None => match gdk::Display::default() { Some(display) => display, None => return Vec::new() },
    };
    let model = display.monitors();
    (0..model.n_items()).filter_map(|index| model.item(index).and_downcast::<gdk::Monitor>()).collect()
}

/// `translate(widget, target, x, y)`.
pub fn translate(widget: &impl IsA<gtk::Widget>, target: &impl IsA<gtk::Widget>, x: f64, y: f64) -> Option<(f64, f64)> {
    widget.compute_point(target, &gtk::graphene::Point::new(x as f32, y as f32))
        .map(|point| (point.x() as f64, point.y() as f64))
}

/// dock_ui.surface: an undecorated layer-shell window (`tessel-dock-surface`,
/// exclusive zone −1, keyboard NONE or ON_DEMAND).
pub fn surface(application: Option<&gtk::Application>, monitor: &gdk::Monitor, namespace: &str, layer: Layer,
               keyboard: bool) -> gtk::Window {
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

/// A DrawingArea painted fully transparent (gtk4_support.transparent_draw).
pub fn transparent_area() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_draw_func(|_area, context, _width, _height| {
        context.set_operator(gtk::cairo::Operator::Source);
        context.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = context.paint();
    });
    area
}

/// `toggle_class(widget, css, enabled)`.
pub fn toggle_class(widget: &impl IsA<gtk::Widget>, css: &str, enabled: bool) {
    if enabled { widget.add_css_class(css) } else { widget.remove_css_class(css) }
}

/// Python 3 `round()` (half to even) for floats.
pub fn round_half_even(value: f64) -> i64 { value.round_ties_even() as i64 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_rounding() {
        assert_eq!(round_half_even(0.5), 0);
        assert_eq!(round_half_even(1.5), 2);
        assert_eq!(round_half_even(-0.5), 0);
        assert_eq!(round_half_even(-1.5), -2);
        assert_eq!(round_half_even(2.6), 3);
    }
}
