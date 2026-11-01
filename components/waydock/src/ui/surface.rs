//! dock_ui.DockSurface: one dock/bar clone on one monitor (contract §6).
//!
//! Surfaces: `waydock` (visual, BOTTOM ↔ TOP with hover, keyboard NONE),
//! `waydock-reveal` (TOP hover sensor; in bar mode its exclusive zone is
//! thickness + gap even while auto-hidden), `waydock-app-reservation`
//! (BOTTOM, empty input region; expanded separate app dock). Popups are
//! F1's `AppMenu` (`waydock-applications`) and `WindowPreview`
//! (`waydock-windows`); context menus are `waydock-context-menu` popovers.
//!
//! API (Python names): `DockSurface::new(app, monitor, name, app_dock,
//! top_bar)` → `Rc<DockSurface>`; `render(items)`, `destroy()`,
//! `set_background(&Background)`, `set_float_mode(b)`, `set_trash_count(n)`,
//! `toggle_app_menu()`, `close_app_menu()`, `close_preview()`, `enter(zone)` /
//! `leave(zone)`, `wake(s)`, `tick()`, `sync_layer()`, `dock_dimensions()`,
//! `resize_sensor()`, `refresh_window_state()`, `refresh_groups()`,
//! `queue_panel_layout()`, `group_zones()`, `custom_group_zones()`,
//! `visible_groups()`, `item_button(id)`, `utility_button(role)`, accessors
//! for the layer windows (`window()`, `sensor()`, `reservation()`), `panel()`,
//! `edge_gap()`, `bar_mode`, `is_app_dock`, `monitor`, `monitor_name`.
//! Implements `ui::host::DockView`, `ui::app_menu::{PopupHost, MenuHost}`,
//! `ui::app_menu::drag::DropHost` and `ui::preview::PreviewHost<Window>`.
//!
//! Workarea rule (§6): a bar reserves thickness + gap on the always-mapped
//! sensor; while any non-minimized maximized window is on its monitor the
//! bar drops the float gap and rounding (reservation = thickness only).

mod groups;
mod items;
mod style;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use indexmap::IndexMap;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::app::App;
use crate::core::app_catalog::{AppCatalog, LaunchContext};
use crate::core::config::DockConfig;
use crate::core::group_layout::{default_app_dock_layout, default_layout, APP_DOCK_GROUPS, POSITIONS};
use crate::core::model::{DockItem, HoverState};
use crate::wl::Window;

use super::app_menu::drag::DropHost;
use super::app_menu::logic::{MenuMetrics, PopupPlacement};
use super::app_menu::{AppMenu, MenuHost, PopupAnchor, PopupHost};
use super::host::{monitors, surface, transparent_area, DockView, MenuEntry};
use super::indicators::DockIndicators;
use super::magnifier::Magnifier;
use super::preview::{PreviewHost, WindowActions, WindowPreview};
use super::tray::TrayExpander;

pub use style::{Background, Material};

pub type Item = DockItem<Window>;
/// Three positions (start, center, end) of group names.
pub type Zones = Vec<Vec<String>>;

fn now() -> f64 { glib::monotonic_time() as f64 / 1_000_000.0 }

const SIDES: [Edge; 4] = [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right];

fn side_index(side: Edge) -> usize { SIDES.iter().position(|s| *s == side).unwrap_or(0) }

fn opposite(side: Edge) -> Edge {
    match side {
        Edge::Top => Edge::Bottom,
        Edge::Bottom => Edge::Top,
        Edge::Left => Edge::Right,
        _ => Edge::Left,
    }
}

pub fn edge_of(name: &str) -> Edge {
    match name {
        "top" => Edge::Top,
        "left" => Edge::Left,
        "right" => Edge::Right,
        _ => Edge::Bottom,
    }
}

/// `preferred(widget, orientation)[1]` (natural size).
pub fn natural(widget: &impl IsA<gtk::Widget>, orientation: gtk::Orientation) -> i32 { widget.measure(orientation, -1).1 }

/// A dock button (`item_button`) and its Python attributes.
pub struct ItemButton {
    pub button: gtk::Button,
    pub icon_override: RefCell<Option<String>>,
    pub app_image: gtk::Image,
    pub item_box: gtk::Box,
    pub magnifier: Magnifier,
    pub count_badge: gtk::Label,
    pub indicator: gtk::Box,
    pub dock_item: RefCell<Option<Item>>,
}

/// Widgets made once by `_build_groups`.
pub struct Groups {
    pub menu_button: gtk::Button,
    pub app_scroll: gtk::ScrolledWindow,
    pub center_strip: gtk::Box,
    pub pinned_box: gtk::Box,
    pub running_box: gtk::Box,
    pub focused_title: gtk::Label,
    pub focused_title_button: gtk::Button,
    pub application_strip: gtk::Box,
    pub activities_button: gtk::Button,
}

pub struct DockSurface {
    app: Weak<App>,
    this: Weak<DockSurface>,
    pub monitor: gdk::Monitor,
    pub monitor_name: String,
    pub config: Rc<DockConfig>,
    pub is_app_dock: bool,
    top_bar: Option<Weak<DockSurface>>,
    pub bar_mode: bool,
    pub positioned_layout: bool,
    pub horizontal: bool,
    pub orientation: gtk::Orientation,
    pub edge: Edge,
    requested_float_mode: Cell<bool>,
    float_mode: Cell<bool>,
    has_maximized: Cell<bool>,
    hover: RefCell<HoverState>,
    preview: OnceCell<Rc<WindowPreview<Window>>>,
    menu: OnceCell<Rc<AppMenu>>,
    context_menu: RefCell<Option<gtk::Popover>>,
    buttons: RefCell<IndexMap<String, Rc<ItemButton>>>,
    utility_buttons: RefCell<IndexMap<String, Rc<ItemButton>>>,
    utility_items: RefCell<IndexMap<String, Item>>,
    render_items: RefCell<IndexMap<String, Item>>,
    source_items: RefCell<Rc<Vec<Item>>>,
    utility_enabled: Cell<Option<bool>>,
    utility_handlers: RefCell<Option<(u64, (Vec<String>, Vec<String>))>>,
    destroyed: Cell<bool>,
    signature: RefCell<Option<String>>,
    groups: RefCell<IndexMap<String, (gtk::Separator, Vec<gtk::Widget>)>>,
    group_available: RefCell<HashMap<String, bool>>,
    visible_groups: RefCell<Option<Vec<String>>>,
    building_groups: Cell<bool>,
    indicators: RefCell<Option<DockIndicators>>,
    utility_scroll: RefCell<Option<gtk::ScrolledWindow>>,
    start_scroll: RefCell<Option<gtk::ScrolledWindow>>,
    application_expander: RefCell<Option<TrayExpander>>,
    arranged: RefCell<Option<(bool, Option<Zones>)>>,
    title_window_id: RefCell<Option<String>>,
    layout_timer: RefCell<Option<glib::SourceId>>,
    styles: style::Styles,
    pub popup_class: String,
    sensor: gtk::Window,
    sensor_catcher: gtk::DrawingArea,
    window: gtk::Window,
    reservation: Option<gtk::Window>,
    panel: gtk::Widget,
    core: RefCell<gtk::Box>,
    magnify: bool,
    headroom: i32,
    /// Transparent room around the panel for its drop shadow (0: none).
    shadow_room: i32,
    /// Where the panel itself sits from each screen edge (top, bottom, left,
    /// right); the window's layer margins are these minus its shadow room.
    panel_margins: Cell<[i32; 4]>,
    input_bounds: Cell<Option<(i32, i32, i32, i32)>>,
    blur: suite_gtk::blur::BlurBinding,
    last_geometry: Cell<Option<(i32, i32)>>,
    timer: RefCell<Option<glib::SourceId>>,
    awake_until: Cell<f64>,
    geometry_signals: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    widgets: OnceCell<Groups>,
    drop_target: RefCell<Option<gtk::DropTarget>>,
}

impl DockSurface {
    pub fn new(app: &Rc<App>, monitor: &gdk::Monitor, monitor_name: &str, app_dock: bool,
               top_bar: Option<&Rc<DockSurface>>) -> Rc<Self> {
        let mut config = app.config().for_monitor(monitor_name);
        if app_dock {
            config = config.for_app_dock();
        }
        let config = Rc::new(config);
        let bar_mode = config.mode == "bar" || (app_dock && config.app_dock_expand);
        let positioned_layout = bar_mode || app_dock || config.group_layouts.get(&config.mode).is_some_and(|layout| !layout.is_empty());
        let horizontal = matches!(config.edge.as_str(), "top" | "bottom");
        let orientation = if horizontal { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
        let edge = edge_of(&config.edge);
        let float = config.effective_float_mode();
        let application = app.application();
        let hover = HoverState::new(config.hide_on_fullscreen, config.hide_delay_ms as f64 / 1000.0).unwrap_or_default();
        // Fullscreen applications must cover both the dock and its input
        // sensor, so game clicks cannot reveal or interact with hidden icons.
        let sensor = surface(Some(&application), monitor, "waydock-reveal", Layer::Top, false);
        sensor.set_resizable(true);
        sensor.set_default_size(1, 1);
        sensor.set_anchor(edge, true);
        let catcher = transparent_area();
        sensor.set_child(Some(&catcher));
        let window = surface(Some(&application), monitor, "waydock", Layer::Bottom, false);
        let reservation = if app_dock && bar_mode {
            // Reserve after the top bar in layer order, without allowing the
            // sidebar to shorten that bar.
            let reservation = surface(Some(&application), monitor, "waydock-app-reservation", Layer::Bottom, false);
            reservation.set_default_size(1, 1);
            reservation.set_child(Some(&transparent_area()));
            reservation.connect_realize(|window| {
                if let Some(surface) = window.surface() {
                    surface.set_input_region(Some(&gtk::cairo::Region::create()));
                }
            });
            reservation.set_anchor(edge, true);
            let sides = if horizontal { [Edge::Left, Edge::Right] } else { [Edge::Top, Edge::Bottom] };
            for side in sides {
                reservation.set_anchor(side, true);
            }
            Some(reservation)
        } else {
            None
        };
        let gap = if float { config.gap as i32 } else { 0 };
        window.set_anchor(edge, true);
        window.set_margin(edge, gap);
        if bar_mode {
            let sides = if horizontal { [Edge::Left, Edge::Right] } else { [Edge::Top, Edge::Bottom] };
            for win in [&window, &sensor] {
                win.set_resizable(true);
                for side in sides {
                    win.set_anchor(side, true);
                    win.set_margin(side, gap);
                }
            }
        } else if config.alignment != "center" {
            let start = config.alignment == "start";
            let side = if horizontal { if start { Edge::Left } else { Edge::Right } } else if start { Edge::Top } else { Edge::Bottom };
            for win in [&window, &sensor] {
                win.set_anchor(side, true);
                win.set_margin(side, gap);
            }
        }
        let panel: gtk::Widget = if positioned_layout {
            gtk::CenterBox::builder().orientation(orientation).build().upcast()
        } else {
            gtk::Box::new(orientation, 3).upcast()
        };
        panel.add_css_class("waydock-panel");
        let item_icon_size = if config.application_display == "title" { config.icon_size.min(22) } else { config.icon_size } as f64;
        let magnify = !bar_mode && config.magnification && config.hover_scale > 1.0;
        let headroom = if magnify { (item_icon_size * (config.hover_scale - 1.0)) as i32 + 4 } else { 0 };
        let shadow_room = if app_dock && !bar_mode && config.app_dock_shadow { suite_gtk::palette::SHADOW_ROOM } else { 0 };
        let core = panel.clone().downcast::<gtk::Box>().unwrap_or_else(|_| gtk::Box::new(orientation, 3));
        let dock = Rc::new_cyclic(|this: &Weak<DockSurface>| {
            let popup_class = format!("waydock-palette-{:x}", this.as_ptr() as usize);
            panel.add_css_class(&popup_class);
            DockSurface {
                app: Rc::downgrade(app), this: this.clone(), monitor: monitor.clone(), monitor_name: monitor_name.to_owned(),
                config: config.clone(), is_app_dock: app_dock, top_bar: top_bar.map(Rc::downgrade), bar_mode,
                positioned_layout, horizontal, orientation, edge, requested_float_mode: Cell::new(float),
                float_mode: Cell::new(float), has_maximized: Cell::new(false), hover: RefCell::new(hover),
                preview: OnceCell::new(), menu: OnceCell::new(), context_menu: RefCell::new(None),
                buttons: RefCell::default(), utility_buttons: RefCell::default(), utility_items: RefCell::default(),
                render_items: RefCell::default(), source_items: RefCell::new(Rc::new(Vec::new())),
                utility_enabled: Cell::new(None), utility_handlers: RefCell::new(None), destroyed: Cell::new(false),
                signature: RefCell::new(None), groups: RefCell::default(), group_available: RefCell::default(),
                visible_groups: RefCell::new(None), building_groups: Cell::new(false), indicators: RefCell::new(None),
                utility_scroll: RefCell::new(None), start_scroll: RefCell::new(None),
                application_expander: RefCell::new(None), arranged: RefCell::new(None),
                title_window_id: RefCell::new(None), layout_timer: RefCell::new(None), styles: style::Styles::default(),
                popup_class, sensor: sensor.clone(), sensor_catcher: catcher.clone(), window: window.clone(), reservation,
                panel: panel.clone(), core: RefCell::new(core), magnify, headroom, shadow_room,
                panel_margins: Cell::new([0; 4]), input_bounds: Cell::new(None),
                blur: suite_gtk::blur::attach(&window, Some("waydock-panel"), true, false), last_geometry: Cell::new(None),
                timer: RefCell::new(None), awake_until: Cell::new(0.0), geometry_signals: RefCell::default(),
                widgets: OnceCell::new(), drop_target: RefCell::new(None),
            }
        });
        dock.init();
        dock
    }

    fn init(self: &Rc<Self>) {
        let preview_host: Weak<dyn PreviewHost<Window>> = self.this.clone();
        let _ = self.preview.set(WindowPreview::new(preview_host));
        let menu_host: Weak<dyn MenuHost> = self.this.clone();
        let _ = self.menu.set(AppMenu::new(menu_host));
        let weak = self.this.clone();
        let weak2 = self.this.clone();
        super::host::motion(&self.sensor_catcher,
            move || if let Some(dock) = weak.upgrade() { dock.enter("sensor") },
            move || if let Some(dock) = weak2.upgrade() { dock.leave("sensor") });
        if self.magnify {
            let room = self.headroom;
            let headroom = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            headroom.set_size_request(if self.horizontal { -1 } else { room }, if self.horizontal { room } else { -1 });
            let holder = gtk::Box::new(if self.horizontal { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal }, 0);
            if matches!(self.config.edge.as_str(), "bottom" | "right") {
                holder.append(&headroom);
                holder.append(&self.panel);
            } else {
                holder.append(&self.panel);
                holder.append(&headroom);
            }
            self.window.set_child(Some(&holder));
            let pointer = gtk::EventControllerMotion::new();
            let weak = self.this.clone();
            pointer.connect_enter(move |_, x, y| if let Some(dock) = weak.upgrade() { dock.magnify_at(Some((x, y))) });
            let weak = self.this.clone();
            pointer.connect_motion(move |_, x, y| if let Some(dock) = weak.upgrade() { dock.magnify_at(Some((x, y))) });
            let weak = self.this.clone();
            pointer.connect_leave(move |_| if let Some(dock) = weak.upgrade() { dock.magnify_at(None) });
            self.panel.add_controller(pointer);
        } else {
            self.window.set_child(Some(&self.panel));
        }
        for side in SIDES {
            self.set_panel_margin(side, self.window.margin(side));
        }
        let (weak, weak2) = (self.this.clone(), self.this.clone());
        super::host::motion(&self.window,
            move || if let Some(dock) = weak.upgrade() { dock.enter("dock") },
            move || if let Some(dock) = weak2.upgrade() { dock.leave("dock") });
        let weak = self.this.clone();
        super::host::click(&self.window, 3, false, false, move |_widget, event| {
            weak.upgrade().is_some_and(|dock| dock.background_menu(event.button, event.x, event.y))
        });
        let weak = self.this.clone();
        super::host::click(&self.window, 1, true, false, move |widget, event| {
            weak.upgrade().is_some_and(|dock| dock.background_activate(widget, event.button, event.x, event.y))
        });
        let weak = self.this.clone();
        let handler = self.window.connect_realize(move |window| {
            if let Some(dock) = weak.upgrade() {
                dock.watch_surface(window);
            }
        });
        self.geometry_signals.borrow_mut().push((self.window.clone().upcast(), handler));
        let weak = self.this.clone();
        let handler = self.monitor.connect_geometry_notify(move |_| {
            if let Some(dock) = weak.upgrade() {
                dock.wake(1.0);
            }
        });
        self.geometry_signals.borrow_mut().push((self.monitor.clone().upcast(), handler));
        self.wake(2.0);
        self.render(Rc::new(Vec::new()));
        if self.bar_mode {
            self.resize_sensor();
        }
        if let Some(reservation) = &self.reservation {
            reservation.set_visible(true);
        }
        self.sync_app_dock_placement();
        self.sensor.set_visible(true);
        self.window.set_visible(true);
        self.sync_layer();
    }

    pub fn app(&self) -> Option<Rc<App>> { self.app.upgrade() }
    pub fn rc(&self) -> Option<Rc<DockSurface>> { self.this.upgrade() }
    pub fn window(&self) -> &gtk::Window { &self.window }
    pub fn sensor(&self) -> &gtk::Window { &self.sensor }
    /// The surface carrying the bar's workarea reservation.
    pub fn reservation(&self) -> &gtk::Window { self.reservation.as_ref().unwrap_or(&self.sensor) }
    pub fn panel(&self) -> &gtk::Widget { &self.panel }
    pub fn core(&self) -> gtk::Box { self.core.borrow().clone() }
    pub fn is_destroyed(&self) -> bool { self.destroyed.get() }
    pub fn headroom(&self) -> i32 { self.headroom }
    pub fn shadow_room(&self) -> i32 { self.shadow_room }

    fn panel_margin(&self, side: Edge) -> i32 { self.panel_margins.get()[side_index(side)] }

    /// The transparent room kept on `side` of the panel for its shadow: all of
    /// it where the window is free (less the headroom already there), and no
    /// more than the gap where the window is anchored to a screen edge.
    fn shadow_inset(&self, side: Edge) -> i32 {
        if self.shadow_room == 0 {
            0
        } else if self.window.is_anchor(side) {
            self.shadow_room.min(self.panel_margin(side).max(0))
        } else if side == opposite(self.edge) {
            (self.shadow_room - self.headroom).max(0)
        } else {
            self.shadow_room
        }
    }

    /// Place the panel `margin` px from `side`, moving the window out by the
    /// shadow room on that side so the panel itself does not move.
    fn set_panel_margin(&self, side: Edge, margin: i32) {
        let mut margins = self.panel_margins.get();
        margins[side_index(side)] = margin;
        self.panel_margins.set(margins);
        // Margins on free sides are ignored; keep them as given.
        let inset = if self.window.is_anchor(side) { self.shadow_inset(side) } else { 0 };
        self.window.set_margin(side, margin - inset);
        if self.shadow_room == 0 {
            return;
        }
        if let Some(content) = self.window.child() {
            content.set_margin_top(self.shadow_inset(Edge::Top));
            content.set_margin_bottom(self.shadow_inset(Edge::Bottom));
            content.set_margin_start(self.shadow_inset(Edge::Left));
            content.set_margin_end(self.shadow_inset(Edge::Right));
        }
    }

    /// The window's (horizontal, vertical) shadow room in total.
    fn shadow_insets(&self) -> (i32, i32) {
        (self.shadow_inset(Edge::Left) + self.shadow_inset(Edge::Right),
         self.shadow_inset(Edge::Top) + self.shadow_inset(Edge::Bottom))
    }
    pub fn magnify(&self) -> bool { self.magnify }
    pub fn hover_layer(&self) -> &'static str { self.hover.borrow().layer() }
    pub fn hover_active(&self) -> bool { self.hover.borrow().active() }
    pub fn timer_running(&self) -> bool { self.timer.borrow().is_some() }
    pub fn float_mode(&self) -> bool { self.float_mode.get() }
    pub fn has_maximized(&self) -> bool { self.has_maximized.get() }
    pub fn app_menu(&self) -> Rc<AppMenu> { self.menu.get().expect("menu").clone() }
    pub fn preview(&self) -> Rc<WindowPreview<Window>> { self.preview.get().expect("preview").clone() }
    pub fn menu_window(&self) -> Option<gtk::Window> { self.menu.get().and_then(|menu| menu.window()) }
    pub fn context_menu(&self) -> Option<gtk::Popover> { self.context_menu.borrow().clone() }
    pub fn indicators(&self) -> Option<DockIndicators> { self.indicators.borrow().clone() }
    pub fn application_expander(&self) -> Option<TrayExpander> { self.application_expander.borrow().clone() }
    pub fn widgets(&self) -> Option<&Groups> { self.widgets.get() }
    pub fn visible_groups(&self) -> Vec<String> { self.visible_groups.borrow().clone().unwrap_or_default() }
    pub fn start_scroll(&self) -> Option<gtk::ScrolledWindow> { self.start_scroll.borrow().clone() }
    pub fn utility_scroll(&self) -> Option<gtk::ScrolledWindow> { self.utility_scroll.borrow().clone() }
    pub fn item_button(&self, id: &str) -> Option<Rc<ItemButton>> { self.buttons.borrow().get(id).cloned() }
    pub fn item_buttons(&self) -> Vec<(String, Rc<ItemButton>)> {
        self.buttons.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
    pub fn utility_button(&self, role: &str) -> Option<Rc<ItemButton>> { self.utility_buttons.borrow().get(role).cloned() }
    pub fn group_widgets(&self, name: &str) -> Option<(gtk::Separator, Vec<gtk::Widget>)> { self.groups.borrow().get(name).cloned() }
    pub fn group_names(&self) -> Vec<String> { self.groups.borrow().keys().cloned().collect() }
    pub fn source_items(&self) -> Rc<Vec<Item>> { self.source_items.borrow().clone() }
    pub fn signature_reset(&self) { self.signature.replace(None); }

    /// `edge_gap`: the float gap, or 0.
    pub fn edge_gap(&self) -> i32 { if self.float_mode.get() { self.config.gap as i32 } else { 0 } }

    /// `item_icon_size`.
    pub fn item_icon_size(&self) -> i32 {
        (if self.config.application_display == "title" { self.config.icon_size.min(22) } else { self.config.icon_size }) as i32
    }

    pub fn custom_group_zones(&self) -> Option<Zones> {
        let to_zones = |layout: &crate::core::group_layout::Layout| -> Zones {
            POSITIONS.iter().map(|(position, _)| layout.get(*position).cloned().unwrap_or_default()).collect()
        };
        if self.is_app_dock {
            let layout = if self.config.app_dock_group_layout.is_empty() {
                default_app_dock_layout(self.bar_mode)
            } else {
                self.config.app_dock_group_layout.clone()
            };
            return Some(to_zones(&layout));
        }
        let layout = self.config.group_layouts.get(&self.config.mode).filter(|l| !l.is_empty()).cloned();
        if self.config.uses_separate_app_dock() {
            let layout = layout.unwrap_or_else(|| default_layout(&self.config.mode, &self.config.application_display));
            return Some(to_zones(&layout).into_iter()
                .map(|zone| zone.into_iter().filter(|name| !APP_DOCK_GROUPS.contains(&name.as_str())).collect())
                .collect());
        }
        layout.map(|layout| to_zones(&layout))
    }

    fn top_bar(&self) -> Option<Rc<DockSurface>> { self.top_bar.as_ref().and_then(Weak::upgrade).filter(|bar| !bar.destroyed.get()) }

    fn top_bar_span(&self) -> i32 { self.top_bar().map(|bar| bar.sensor.exclusive_zone().max(0)).unwrap_or(0) }

    fn sync_app_dock_placement(&self) {
        if !self.is_app_dock || self.horizontal {
            return;
        }
        let top = self.top_bar_span();
        let margin = if self.bar_mode {
            top + self.edge_gap()
        } else {
            let height = self.monitor.geometry().height();
            top + self.edge_gap().max((height - top - self.dock_dimensions().1).div_euclid(2))
        };
        let changed = self.panel_margin(Edge::Top) != margin;
        for window in [&self.window, &self.sensor] {
            window.set_anchor(Edge::Top, true);
        }
        self.sensor.set_margin(Edge::Top, margin);
        self.set_panel_margin(Edge::Top, margin);
        if changed {
            self.queue_panel_layout();
            self.reposition_popups();
        }
    }

    fn reposition_popups(&self) {
        if let Some(menu) = self.menu_window() {
            super::app_menu::popup::position_popup(self, &menu, &PopupAnchor::Menu);
        }
        if let (Some(popup), Some(id)) = (self.preview().window(), self.preview().preview_id()) {
            super::app_menu::popup::position_popup(self, &popup, &PopupAnchor::Item(id));
        }
    }

    pub fn set_float_mode(&self, enabled: bool) {
        if self.destroyed.get() {
            return;
        }
        self.requested_float_mode.set(enabled);
        self.refresh_window_state();
        self.wake(1.0);
    }

    pub fn refresh_window_state(&self) {
        // Maximization suppresses floating only for bars; compact docks
        // neither reserve space nor change their gap when windows maximize.
        if self.destroyed.get() {
            return;
        }
        let has_maximized = self.bar_mode && self.app().and_then(|app| app.backend()).is_some_and(|backend| {
            backend.windows().values().any(|w| !w.minimized && w.maximized && w.outputs.contains(&self.monitor_name))
        });
        let enabled = self.requested_float_mode.get() && !has_maximized;
        let float_changed = self.float_mode.get() != enabled;
        let appearance_changed = float_changed || self.has_maximized.get() != has_maximized;
        self.has_maximized.set(has_maximized);
        self.float_mode.set(enabled);
        if appearance_changed {
            self.restyle();
        }
        if !float_changed {
            return;
        }
        let gap = self.edge_gap();
        self.set_panel_margin(self.edge, gap);
        // The reveal sensor always reaches the screen edge; only its side
        // alignment follows the floating dock's inset.
        for side in SIDES {
            if side != self.edge && self.window.is_anchor(side) {
                self.sensor.set_margin(side, gap);
                self.set_panel_margin(side, gap);
            }
        }
        self.resize_sensor();
        self.queue_panel_layout();
    }

    /// `_panel_frost_geometry()`: (monitor width, height, panel x, y).
    pub fn panel_frost_geometry(&self) -> (i32, i32, i32, i32) {
        let geometry = self.monitor.geometry();
        let (width, height) = self.dock_dimensions();
        let position = |extent: i32, size: i32, start: Edge, end: Edge| {
            let (first, last) = (self.window.is_anchor(start), self.window.is_anchor(end));
            let (low, high) = (self.panel_margin(start), self.panel_margin(end));
            if first && last {
                low + (extent - low - high - size).div_euclid(2)
            } else if first {
                low
            } else if last {
                extent - high - size
            } else {
                (extent - size).div_euclid(2)
            }
        };
        (geometry.width(), geometry.height(), position(geometry.width(), width, Edge::Left, Edge::Right),
         position(geometry.height(), height, Edge::Top, Edge::Bottom))
    }

    /// `screen_position(widget, x, y)`: global logical screen coordinates.
    pub fn screen_position(&self, widget: &gtk::Widget, x: f64, y: f64) -> Option<(i32, i32)> {
        let point = super::host::translate(widget, &self.window, x, y)?;
        let geometry = self.monitor.geometry();
        let (_w, _h, origin_x, origin_y) = self.panel_frost_geometry();
        Some(((geometry.x() as f64 + origin_x as f64 + point.0).round_ties_even() as i32,
              (geometry.y() as f64 + origin_y as f64 + point.1).round_ties_even() as i32))
    }

    pub fn enter(&self, zone: &str) {
        self.hover.borrow_mut().enter(zone, now());
        self.window.set_visible(true);
        self.sync_layer();
        self.wake(1.0);
    }

    pub fn leave(&self, zone: &str) {
        self.hover.borrow_mut().leave(zone, now());
        self.wake(1.0);
    }

    /// Keep the state tick running for a short while after an event.
    pub fn wake(&self, seconds: f64) {
        if self.destroyed.get() {
            return;
        }
        self.awake_until.set(self.awake_until.get().max(now() + seconds));
        if self.timer.borrow().is_none() {
            let weak = self.this.clone();
            let source = glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
                let Some(dock) = weak.upgrade() else { return glib::ControlFlow::Break };
                if dock.tick() {
                    glib::ControlFlow::Continue
                } else {
                    dock.timer.borrow_mut().take();
                    glib::ControlFlow::Break
                }
            });
            self.timer.replace(Some(source));
        }
    }

    fn watch_surface(&self, window: &gtk::Window) {
        if let Some(native) = window.surface() {
            let weak = self.this.clone();
            let handler = native.connect_layout(move |_, _, _| {
                if let Some(dock) = weak.upgrade() {
                    dock.wake(0.5);
                }
            });
            self.geometry_signals.borrow_mut().push((native.upcast(), handler));
            self.wake(0.5);
        }
    }

    fn needs_tick(&self, geometry_changed: bool) -> bool {
        let expanded = self.indicators().and_then(|i| i.tray_expander()).is_some_and(|e| e.expanded())
            || self.application_expander().is_some_and(|e| e.expanded());
        geometry_changed || self.hover.borrow().active() || now() < self.awake_until.get() || self.preview().is_open()
            || self.menu_window().is_some() || self.context_menu.borrow().is_some() || expanded
    }

    pub fn sync_layer(&self) {
        let layer = if self.hover.borrow().layer() == "top" { Layer::Top } else { Layer::Bottom };
        if self.window.layer() == layer {
            return;
        }
        // Commit the new layer immediately without unmapping: hiding/showing
        // a visible bar here flashes the whole surface on hover changes.
        self.window.set_layer(layer);
        self.window.queue_draw();
        if let Some(surface) = self.window.surface() {
            surface.queue_render();
        }
        if layer == Layer::Top {
            if let Some(corners) = self.app().and_then(|app| app.hot_corners()) {
                corners.raise_for_monitor(&self.monitor_name);
            }
        }
    }

    /// Apply pending hover/geometry state; returns whether to tick again.
    pub fn tick(&self) -> bool {
        if self.destroyed.get() {
            return false;
        }
        self.refresh_window_state();
        self.sync_app_dock_placement();
        self.sync_input_region();
        if let Some(expander) = self.indicators().and_then(|i| i.tray_expander()) {
            expander.sync();
        }
        if let Some(expander) = self.application_expander() {
            expander.sync();
        }
        let geometry = self.dock_dimensions();
        let geometry_changed = Some(geometry) != self.last_geometry.get();
        if geometry_changed {
            self.last_geometry.set(Some(geometry));
            self.resize_sensor();
            self.queue_panel_layout();
        }
        self.hover.borrow_mut().tick(now());
        if !self.hover.borrow().active() {
            self.close_preview();
            if self.config.auto_hide {
                self.window.set_visible(false);
            }
        }
        self.sync_layer();
        self.needs_tick(geometry_changed)
    }

    /// `dock_dimensions()`: the panel's (width, height) without headroom.
    pub fn dock_dimensions(&self) -> (i32, i32) {
        let (pad_x, pad_y) = self.shadow_insets();
        let (width, height) = (self.window.width() - pad_x, self.window.height() - pad_y);
        let room = self.headroom;
        let size = if self.horizontal { (width, height - room) } else { (width - room, height) };
        if size.0 > 0 && size.1 > 0 {
            return size;
        }
        if let Some(last) = self.last_geometry.get().filter(|(w, h)| *w > 0 && *h > 0) {
            return last;
        }
        (natural(&self.panel, gtk::Orientation::Horizontal).max(1), natural(&self.panel, gtk::Orientation::Vertical).max(1))
    }

    pub fn resize_sensor(&self) {
        let (mut width, mut height) = self.dock_dimensions();
        if self.horizontal {
            height += self.edge_gap();
        } else {
            width += self.edge_gap();
        }
        self.sensor_catcher.set_content_width(if self.bar_mode && self.horizontal { 1 } else { width });
        self.sensor_catcher.set_content_height(if self.horizontal || !self.bar_mode { height } else { 1 });
        if self.bar_mode {
            // Opposite anchors stretch the long dimension; a stale explicit
            // size would prevent shrinking when Float changes.
            self.sensor.set_size_request(if self.horizontal { -1 } else { width }, if self.horizontal { height } else { -1 });
            self.sensor.set_default_size(if self.horizontal { 1 } else { width }, if self.horizontal { height } else { 1 });
        } else {
            self.sensor.set_size_request(width, height);
            self.sensor.set_default_size(width, height);
        }
        // A bar owns a strip of the monitor's usable workspace, reserved on
        // the always-mapped reveal surface (it reaches the edge, so this size
        // includes the floating gap). Compact docks never reserve.
        let zone = if self.bar_mode { if self.horizontal { height } else { width } } else { -1 };
        let reservation = self.reservation();
        if self.reservation.is_some() {
            reservation.set_size_request(1, 1);
        }
        if reservation.exclusive_zone() != zone {
            reservation.set_exclusive_zone(zone);
            if let Some(app) = self.app() {
                app.queue_workareas_changed();
                // A paired app dock follows this bar's reserved span.
                for dock in app.docks() {
                    if !std::ptr::eq(Rc::as_ptr(&dock), self as *const DockSurface) && dock.top_bar.as_ref().is_some_and(|bar| bar.as_ptr() == self as *const _) {
                        dock.wake(1.0);
                    }
                }
            }
            reservation.queue_draw();
            if let Some(surface) = reservation.surface() {
                surface.queue_render();
            }
        }
        self.sync_app_dock_placement();
        self.reposition_popups();
    }

    /// `launch_context()`: the display's launch context, timestamp 0.
    pub fn launch_context(&self) -> Rc<dyn LaunchContext> {
        let context = WidgetExt::display(&self.window).app_launch_context();
        context.set_timestamp(0);
        Rc::new(context.upcast::<gio::AppLaunchContext>())
    }

    pub fn close_preview(&self) {
        if let Some(preview) = self.preview.get() {
            preview.close();
        }
    }

    pub fn close_app_menu(&self) {
        if let Some(menu) = self.menu.get() {
            menu.close();
        }
    }

    pub fn toggle_app_menu(&self) { self.app_menu().toggle(); }

    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        self.blur.close();
        if let Some(source) = self.layout_timer.borrow_mut().take() {
            source.remove();
        }
        self.close_preview();
        self.close_app_menu();
        let context = self.context_menu.borrow().clone();
        if let Some(menu) = context {
            menu.popdown();
        }
        if let Some(expander) = self.application_expander() {
            expander.close();
        }
        if let Some(indicators) = self.indicators() {
            // Release timers and tray subscriptions, including Bar's second group.
            indicators.close();
        }
        self.styles.close(&WidgetExt::display(&self.window));
        if let Some(source) = self.timer.borrow_mut().take() {
            source.remove();
        }
        for (object, handler) in self.geometry_signals.borrow_mut().drain(..) {
            object.disconnect(handler);
        }
        self.sensor.destroy();
        if let Some(reservation) = &self.reservation {
            reservation.destroy();
        }
        self.window.destroy();
    }
}

// ------------------------------------------------------------ host traits

impl DockView for DockSurface {
    fn config(&self) -> Rc<DockConfig> { self.config.clone() }
    fn window(&self) -> gtk::Window { self.window.clone() }
    fn orientation(&self) -> gtk::Orientation { self.orientation }
    fn horizontal(&self) -> bool { self.horizontal }
    fn edge_gap(&self) -> i32 { DockSurface::edge_gap(self) }
    fn monitor(&self) -> gdk::Monitor { self.monitor.clone() }
    fn monitor_name(&self) -> String { self.monitor_name.clone() }
    fn destroyed(&self) -> bool { self.destroyed.get() }
    fn is_app_dock(&self) -> bool { self.is_app_dock }
    fn panel(&self) -> Option<gtk::Widget> { Some(self.panel.clone()) }
    fn popup_class(&self) -> Option<String> { Some(self.popup_class.clone()) }
    fn enter(&self, token: &str) { DockSurface::enter(self, token) }
    fn leave(&self, token: &str) { DockSurface::leave(self, token) }
    fn queue_panel_layout(&self) { DockSurface::queue_panel_layout(self) }
    fn refresh_groups(&self) { DockSurface::refresh_groups(self) }
    fn add_group(&self, name: &str, parent: &gtk::Box, widget: &gtk::Widget, available: bool) {
        self.add_group_widgets(name, parent, std::slice::from_ref(widget), available);
    }
    fn context_menu(&self) -> Option<gtk::Popover> { DockSurface::context_menu(self) }
    fn popup_menu(&self, anchor: &gtk::Widget, _position: Option<(f64, f64)>, entries: Vec<MenuEntry>) {
        let entries = entries.into_iter().map(|(title, action)| (title, Some(action))).collect();
        DockSurface::popup_menu(self, anchor, entries);
    }
    fn close_preview(&self) { DockSurface::close_preview(self) }
    fn close_app_menu(&self) { DockSurface::close_app_menu(self) }
    fn application_expander(&self) -> Option<TrayExpander> { DockSurface::application_expander(self) }
    fn indicators(&self) -> Option<DockIndicators> { DockSurface::indicators(self) }
    fn screen_position(&self, widget: &gtk::Widget, x: f64, y: f64) -> Option<(i32, i32)> {
        DockSurface::screen_position(self, widget, x, y)
    }
    fn dock_dimensions(&self) -> (i32, i32) { DockSurface::dock_dimensions(self) }
    fn panel_frost_geometry(&self) -> (i32, i32, i32, i32) { DockSurface::panel_frost_geometry(self) }
}

impl PopupHost for DockSurface {
    fn application(&self) -> Option<gtk::Application> { self.app().map(|app| app.application()) }
    fn monitor(&self) -> gdk::Monitor { self.monitor.clone() }
    fn edge(&self) -> String { self.config.edge.clone() }
    fn horizontal(&self) -> bool { self.horizontal }
    fn popup_class(&self) -> String { self.popup_class.clone() }
    fn popup_placement(&self, anchor: &PopupAnchor) -> PopupPlacement {
        let (width, height) = self.dock_dimensions();
        let geometry = self.monitor.geometry();
        let mut alignment = "center".to_owned();
        let mut widget: Option<gtk::Widget> = match anchor {
            PopupAnchor::Menu => None,
            PopupAnchor::Item(id) => self.item_anchor(id).map(|button| button.upcast()),
        };
        if self.is_app_dock && *anchor == PopupAnchor::Menu {
            if let Some(groups) = self.widgets.get().filter(|g| g.menu_button.get_visible()) {
                widget = Some(groups.menu_button.clone().upcast());
                alignment = "start".into();
            }
        }
        let anchor = widget.and_then(|widget| {
            super::host::translate(&widget, &self.panel, 0.0, 0.0).map(|point| {
                // Python's get_allocated_width/height: the margin box.
                #[allow(deprecated)]
                let extent = if self.horizontal { widget.allocated_width() } else { widget.allocated_height() };
                (if self.horizontal { point.0 } else { point.1 }, extent as f64)
            })
        });
        let frost = self.panel_frost_geometry();
        PopupPlacement {
            edge: self.config.edge.clone(),
            edge_gap: self.edge_gap(),
            thickness: if self.horizontal { height } else { width },
            headroom: self.headroom,
            total: if self.horizontal { geometry.width() } else { geometry.height() },
            dock_length: if self.horizontal { width } else { height },
            dock_alignment: self.config.alignment.clone(),
            app_dock_origin: self.is_app_dock.then(|| if self.horizontal { frost.2 } else { frost.3 } as f64),
            anchor,
            alignment,
            minimum: if self.is_app_dock && !self.horizontal { (self.top_bar_span() + 8) as f64 } else { 8.0 },
        }
    }
    fn set_popup_blur(&self, popup: &gtk::Window, menu: bool) { self.set_window_popup_blur(popup, menu) }
    fn enter(&self, zone: &str) { DockSurface::enter(self, zone) }
    fn leave(&self, zone: &str) { DockSurface::leave(self, zone) }
    fn destroyed(&self) -> bool { self.destroyed.get() }
    fn catalog(&self) -> Rc<RefCell<AppCatalog>> {
        self.app().map(|app| app.catalog()).unwrap_or_else(|| Rc::new(RefCell::new(AppCatalog::from_infos(Vec::new()))))
    }
}

impl MenuHost for DockSurface {
    fn pins(&self) -> Vec<String> { self.app().map(|app| app.config().pins.clone()).unwrap_or_default() }
    fn launch_history(&self) -> Option<Rc<RefCell<crate::core::app_catalog::LaunchHistory>>> {
        self.app().and_then(|app| app.launch_history())
    }
    fn launch(&self, desktop_id: &str, context: Rc<dyn LaunchContext>) {
        if let Some(app) = self.app() {
            app.launch(desktop_id, Some(context));
        }
    }
    fn toggle_pin(&self, desktop_id: &str) {
        if let Some(app) = self.app() {
            app.toggle_pin(desktop_id);
        }
    }
    fn show_settings(&self) {
        if let Some(app) = self.app() {
            app.show_settings();
        }
    }
    fn before_toggle(&self) {
        if let Some(app) = self.app() {
            for dock in app.docks() {
                if dock.as_ref() as *const _ != self as *const _ {
                    dock.close_app_menu();
                }
            }
        }
        if let Some(expander) = self.indicators().and_then(|i| i.tray_expander()) {
            expander.set_expanded(false);
        }
        if let Some(expander) = self.application_expander() {
            expander.set_expanded(false);
        }
    }
    fn close_preview(&self) { DockSurface::close_preview(self) }
    fn menu_metrics(&self) -> MenuMetrics {
        let geometry = self.monitor.geometry();
        let mut reserved = 0;
        if let Some(app) = self.app() {
            for dock in app.docks() {
                if dock.monitor == self.monitor && !dock.destroyed.get() {
                    let zone = dock.reservation().exclusive_zone();
                    if zone > 0 && dock.horizontal {
                        reserved += zone;
                    }
                }
            }
        }
        let (_width, height) = self.dock_dimensions();
        MenuMetrics {
            monitor_width: geometry.width(), monitor_height: geometry.height(), reserved, horizontal: self.horizontal,
            dock_height: height, edge_gap: self.edge_gap(), headroom: self.headroom, dock_window_width: self.window.width() - self.shadow_insets().0,
        }
    }
    fn enter_all(&self, zone: &str) {
        match self.app() {
            Some(app) => app.docks().iter().for_each(|dock| dock.enter(zone)),
            None => self.enter(zone),
        }
    }
    fn leave_all(&self, zone: &str) {
        match self.app() {
            Some(app) => app.docks().iter().for_each(|dock| dock.leave(zone)),
            None => self.leave(zone),
        }
    }
}

impl DropHost for DockSurface {
    fn catalog(&self) -> Rc<RefCell<AppCatalog>> { PopupHost::catalog(self) }
    fn panel(&self) -> gtk::Widget { self.panel.clone() }
    fn pinned_children(&self) -> Vec<(gtk::Widget, Option<String>)> {
        let Some(groups) = self.widgets.get() else { return Vec::new() };
        let buttons = self.buttons.borrow();
        super::app_menu::popup::children(&groups.pinned_box).into_iter().map(|child| {
            let id = buttons.values().find(|b| b.button.upcast_ref::<gtk::Widget>() == &child)
                .and_then(|b| b.dock_item.borrow().as_ref().map(|item| item.desktop_id()));
            (child, id)
        }).collect()
    }
    fn horizontal(&self) -> bool { self.horizontal }
    fn place_pin(&self, desktop_id: &str, before: Option<&str>) {
        if let Some(app) = self.app() {
            app.place_pin(desktop_id, before);
        }
    }
    fn enter(&self, zone: &str) { DockSurface::enter(self, zone) }
    fn leave(&self, zone: &str) { DockSurface::leave(self, zone) }
}

impl PreviewHost<Window> for DockSurface {
    fn tray_expanded(&self) -> bool { self.indicators().and_then(|i| i.tray_expander()).is_some_and(|e| e.expanded()) }
    fn menu_open(&self) -> bool { self.menu_window().is_some() || self.context_menu.borrow().is_some() }
    fn find_item(&self, item_id: &str) -> Option<Item> { DockSurface::find_item(self, item_id) }
    fn actions(&self) -> Rc<dyn WindowActions> {
        match self.app().and_then(|app| app.backend()) {
            Some(backend) => Rc::new(backend),
            None => Rc::new(NoWindows),
        }
    }
    fn show_error(&self, text: &str) {
        if let Some(app) = self.app() {
            app.show_error(text);
        }
    }
    fn clear_focused_title(&self) {
        if let Some(app) = self.app() {
            app.focused_title().borrow_mut().clear();
        }
    }
    fn collapse_application_expander(&self) {
        if let Some(expander) = self.application_expander() {
            expander.set_expanded(false);
        }
    }
    fn icon_size(&self) -> i32 { self.config.icon_size as i32 }
}

struct NoWindows;
impl WindowActions for NoWindows {
    fn window(&self, _id: &str) -> Option<Window> { None }
    fn activate(&self, _id: &str) -> bool { false }
    fn minimize(&self, _id: &str) -> bool { false }
    fn close_window(&self, _id: &str) -> bool { false }
}

/// Every GDK monitor (re-exported for the app).
pub fn display_monitors() -> Vec<gdk::Monitor> { monitors(None) }
