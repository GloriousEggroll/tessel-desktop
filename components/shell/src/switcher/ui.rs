//! Port of tessel_shell_switcher_ui.py: the switcher/overview layer surface, the
//! click-away backdrops, cards and layout. Activation and keyboard policy live
//! in the controller (switcher/mod.rs).

use super::overview::{layout_limits, overview_layout, preview_key, unique_preview_target, CaptureFrame, Key, OverviewCapture, Stage};
use gtk::prelude::*;
use gtk::{gdk, gio, pango};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use tessel_core::tr;

pub const STYLE: &str = r#"
window.tessel_shell-switcher, window.tessel_shell-switcher-backdrop {
    background-image: none; background-color: transparent; box-shadow: none;
    border: none; border-radius: 0;
}
.tessel_shell-switcher .switcher-surface {
    background-color: @theme_bg_color; color: @theme_fg_color;
    border: 1px solid alpha(@theme_fg_color, .14); border-radius: 12px; padding: 20px;
}
.tessel_shell-switcher .switcher-heading { font-size: 13pt; font-weight: 700; }
.tessel_shell-switcher .switcher-count, .tessel_shell-switcher .switcher-hint {
    color: alpha(@theme_fg_color, .62); font-size: 9pt;
}
.tessel_shell-switcher .switcher-grid { background: transparent; padding: 0; }
.tessel_shell-switcher .switcher-grid > flowboxchild { padding: 0; margin: 0; }
.tessel_shell-switcher button.switcher-card {
    background-image: none; background-color: alpha(@theme_fg_color, .045);
    color: @theme_fg_color; border: 2px solid transparent; border-radius: 12px;
    padding: 10px 8px; box-shadow: none; text-shadow: none; outline: none;
    transition: none;
}
.tessel_shell-switcher button.switcher-card:hover {
    background-color: alpha(@theme_fg_color, .10);
}
.tessel_shell-switcher button.switcher-card.selected {
    background-color: alpha(@theme_selected_bg_color, .20);
    border-color: @theme_selected_bg_color;
}
.tessel_shell-switcher button.switcher-card-close {
    min-width: 20px; min-height: 20px; padding: 1px; border-radius: 8px;
    border: 1px solid alpha(@theme_fg_color, .12); box-shadow: none;
    background-image: none; background-color: alpha(@theme_fg_color, .05);
    color: @theme_fg_color;
}
.tessel_shell-switcher button.switcher-card-close:hover {
    background-color: alpha(@theme_fg_color, .14);
}
/* Card actions appear only for the card under the pointer or the selected card. */
.tessel_shell-switcher .switcher-tile button.switcher-card-close { opacity: 0; }
.tessel_shell-switcher .switcher-tile:hover button.switcher-card-close,
.tessel_shell-switcher .switcher-tile.selected button.switcher-card-close { opacity: 1; }
.tessel_shell-switcher .switcher-name { font-size: 10pt; font-weight: 600; }
.tessel_shell-switcher .switcher-title { color: alpha(@theme_fg_color, .68); font-size: 9pt; }
.tessel_shell-switcher .switcher-card.minimized image { opacity: .65; }
.tessel_shell-switcher scrolledwindow { background-color: transparent; border: none; }
.tessel_shell-switcher scrolledwindow scrollbar {
    min-width: 8px; min-height: 8px; opacity: 1; background: transparent;
}
.tessel_shell-switcher scrolledwindow scrollbar slider {
    min-width: 6px; min-height: 32px; border-radius: 6px;
    background-color: alpha(@theme_fg_color, .25);
}
.tessel_shell-switcher scrollbar slider { min-width: 6px; min-height: 6px; border: none; padding: 0; margin: 0; }
window.tessel_shell-switcher.activities { background-color: @theme_bg_color; }
.tessel_shell-switcher.activities .switcher-surface {
    background-image: none; background-color: alpha(@theme_bg_color, .40);
    border: none; border-radius: 0; padding: 40px;
}
.tessel_shell-switcher.activities .switcher-grid { padding: 10px; }
.tessel_shell-switcher.activities button.switcher-card { padding: 30px 10px 12px; }
.tessel_shell-switcher.activities .switcher-heading { font-size: 18pt; }
.tessel_shell-switcher .window-preview { background: alpha(@theme_fg_color, .025); border-radius: 8px; }
"#;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub title: String,
    pub icon: Option<gio::Icon>,
    pub minimized: bool,
    pub app_id: String,
    pub audio_available: bool,
    pub muted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Hidden,
    Keyboard,
    Activities,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Hidden => "hidden",
            Mode::Keyboard => "keyboard",
            Mode::Activities => "activities",
        }
    }
}

pub struct Card {
    pub button: gtk::Button,
    pub child: gtk::FlowBoxChild,
    pub tile: gtk::Overlay,
    pub close: gtk::Button,
    pub mute: gtk::Button,
    pub name: gtk::Label,
    pub title: gtk::Label,
    pub image: gtk::Image,
    pub preview: gtk::Picture,
    pub preview_layer: gtk::Overlay,
    pub fallback: gtk::Image,
    pub content: gtk::Box,
    pub caption: gtk::Box,
    icon: RefCell<Option<String>>,
    tooltip: RefCell<Option<String>>,
    preview_key: RefCell<Option<String>>,
}

pub type IdFn = Rc<dyn Fn(&str)>;
pub type WallpaperFn = Rc<dyn Fn(&str) -> Option<gdk::Paintable>>;

/// `update_mute_button` (application_audio.py).
pub fn update_mute_button(button: &gtk::Button, available: bool, muted: bool) {
    button.set_sensitive(available);
    if let Some(image) = button.child().and_downcast::<gtk::Image>() {
        image.set_icon_name(Some(if muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" }));
    }
    let tip = if available {
        if muted { tr!("Unmute application") } else { tr!("Mute application") }
    } else {
        tr!("No playback audio for this application")
    };
    button.set_tooltip_text(Some(&tip));
}

pub struct SwitcherView {
    pub window: gtk::Window,
    pub stage: Stage,
    pub surface: gtk::Box,
    pub grid: gtk::FlowBox,
    pub scroller: gtk::ScrolledWindow,
    pub count: gtk::Label,
    pub empty: gtk::Label,
    pub hint: gtk::Label,
    application: gtk::Application,
    on_activate: IdFn,
    on_cancel: Rc<dyn Fn()>,
    pub on_close: RefCell<Option<IdFn>>,
    pub on_mute: RefCell<Option<IdFn>>,
    wallpaper: WallpaperFn,
    pub mode: Cell<Mode>,
    pub capture: RefCell<Option<OverviewCapture>>,
    present_source: RefCell<Option<glib::SourceId>>,
    identities: RefCell<HashMap<Key, usize>>,
    desktop_texture: RefCell<Option<gdk::Texture>>,
    pub cards: RefCell<HashMap<String, Rc<Card>>>,
    items: RefCell<Vec<Item>>,
    order: RefCell<Vec<String>>,
    selected: RefCell<Option<String>>,
    closed: Cell<bool>,
    scroll_idle: RefCell<Option<glib::SourceId>>,
    pub monitor: RefCell<Option<gdk::Monitor>>,
    columns: Cell<i32>,
    size: Cell<Option<(i32, i32)>>,
    pub backdrops: RefCell<Vec<(gdk::Monitor, gtk::Window, gtk::GestureClick)>>,
    primary: RefCell<Option<(gtk::Window, gtk::GestureClick)>>,
    provider: gtk::CssProvider,
    monitors_handler: RefCell<Option<glib::SignalHandlerId>>,
    me: RefCell<Weak<SwitcherView>>,
}

fn all_edges() -> [Edge; 4] {
    [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right]
}

impl SwitcherView {
    pub fn new(application: &gtk::Application, on_activate: IdFn, on_cancel: Rc<dyn Fn()>, on_close: Option<IdFn>,
               on_mute: Option<IdFn>, wallpaper: WallpaperFn) -> Rc<Self> {
        let window = gtk::Window::builder().application(application).title(tr!("Open windows")).build();
        window.add_css_class("tessel-shell-switcher");
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_hide_on_close(true);
        window.init_layer_shell();
        window.set_namespace(Some("tessel-shell-switcher"));
        window.set_layer(Layer::Overlay);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        let surface = gtk::Box::new(gtk::Orientation::Vertical, 14);
        surface.add_css_class("switcher-surface");
        let stage = Stage::new();
        stage.set_child(&surface);
        window.set_child(Some(&stage));
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let heading = gtk::Label::builder().label(tr!("Open windows")).xalign(0.).hexpand(true).build();
        heading.add_css_class("switcher-heading");
        header.append(&heading);
        let count = gtk::Label::new(None);
        count.add_css_class("switcher-count");
        header.append(&count);
        surface.append(&header);

        let grid = gtk::FlowBox::new();
        grid.add_css_class("switcher-grid");
        grid.set_selection_mode(gtk::SelectionMode::None);
        grid.set_homogeneous(true);
        grid.set_row_spacing(10);
        grid.set_column_spacing(10);
        grid.set_valign(gtk::Align::Start);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_propagate_natural_height(true);
        scroller.set_child(Some(&grid));
        scroller.set_vexpand(true);
        surface.append(&scroller);
        let empty = gtk::Label::new(Some(&tr!("No open windows")));
        empty.set_margin_top(24);
        empty.set_margin_bottom(24);
        surface.append(&empty);
        let hint = gtk::Label::builder().label(tr!("Release Alt to switch  ·  Esc to cancel")).xalign(0.).build();
        hint.set_ellipsize(pango::EllipsizeMode::End);
        hint.add_css_class("switcher-hint");
        surface.append(&hint);

        let provider = gtk::CssProvider::new();
        provider.load_from_string(STYLE);
        gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);

        let view = Rc::new(Self {
            window, stage, surface, grid, scroller, count, empty, hint, application: application.clone(), on_activate, on_cancel,
            on_close: RefCell::new(on_close), on_mute: RefCell::new(on_mute), wallpaper, mode: Cell::new(Mode::Keyboard),
            capture: RefCell::new(None), present_source: RefCell::new(None), identities: RefCell::default(),
            desktop_texture: RefCell::new(None), cards: RefCell::default(), items: RefCell::default(), order: RefCell::default(),
            selected: RefCell::new(None), closed: Cell::new(false), scroll_idle: RefCell::new(None), monitor: RefCell::new(None),
            columns: Cell::new(0), size: Cell::new(None), backdrops: RefCell::default(), primary: RefCell::new(None), provider, monitors_handler: RefCell::new(None),
            me: RefCell::new(Weak::new()),
        });
        *view.me.borrow_mut() = Rc::downgrade(&view);

        let weak = Rc::downgrade(&view);
        view.window.connect_close_request(move |_| {
            if let Some(v) = weak.upgrade() {
                (v.on_cancel)();
            }
            glib::Propagation::Stop
        });
        let outside = gtk::GestureClick::new();
        let weak = Rc::downgrade(&view);
        outside.connect_released(move |_, _, x, y| {
            if let Some(v) = weak.upgrade() {
                v.overview_click(x, y);
            }
        });
        view.window.add_controller(outside);
        let monitors = WidgetExt::display(&view.window).monitors();
        let weak = Rc::downgrade(&view);
        let handler = monitors.connect_items_changed(move |_, _, _, _| {
            if let Some(v) = weak.upgrade() {
                if !v.closed.get() && v.window.get_visible() {
                    // Reopening resolves the new output set.
                    (v.on_cancel)();
                }
            }
        });
        *view.monitors_handler.borrow_mut() = Some(handler);
        view.sync_backdrops(None);
        view.layout();
        view
    }

    pub fn columns(&self) -> i32 {
        self.columns.get()
    }

    pub fn set_mode(&self, mode: Mode) {
        if mode != self.mode.get() {
            self.mode.set(mode);
            let activities = mode == Mode::Activities;
            if activities { self.window.add_css_class("activities") } else { self.window.remove_css_class("activities") }
            self.window.set_resizable(activities);
            for edge in all_edges() {
                self.window.set_anchor(edge, activities);
            }
            if !activities {
                self.stop_capture();
                self.stage.set_wallpaper(None);
            }
            self.size.set(None);
            self.layout();
        }
        self.hint.set_label(&if mode == Mode::Activities {
            tr!("Select a window  ·  Enter to switch  ·  Esc to close")
        } else {
            tr!("Release Alt to switch  ·  Esc to cancel")
        });
    }

    fn overview_click(&self, x: f64, y: f64) {
        if self.mode.get() != Mode::Activities {
            return;
        }
        let mut widget = self.window.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(w) = widget {
            if w.is::<gtk::Button>() || w.is::<gtk::Scrollbar>() {
                return;
            }
            widget = w.parent();
        }
        (self.on_cancel)();
    }

    fn sync_backdrops(&self, primary: Option<&gdk::Monitor>) {
        let current = crate::monitors::monitor_list(&WidgetExt::display(&self.window));
        let mut backdrops = self.backdrops.borrow_mut();
        backdrops.retain(|(monitor, window, _)| {
            let keep = current.contains(monitor);
            if !keep {
                window.destroy();
            }
            keep
        });
        for monitor in &current {
            let existing = backdrops.iter().find(|(m, _, _)| m == monitor).map(|(_, w, _)| w.clone());
            let backdrop = match existing {
                Some(b) => b,
                None => {
                    let backdrop = gtk::Window::builder().application(&self.application).title(tr!("Window switcher backdrop")).build();
                    backdrop.add_css_class("tessel-shell-switcher-backdrop");
                    backdrop.set_decorated(false);
                    // Fixed-size GTK4 windows ignore the full-output configure.
                    backdrop.set_resizable(true);
                    backdrop.init_layer_shell();
                    backdrop.set_namespace(Some("tessel-shell-switcher-backdrop"));
                    backdrop.set_layer(Layer::Overlay);
                    backdrop.set_exclusive_zone(-1);
                    backdrop.set_keyboard_mode(KeyboardMode::None);
                    for edge in all_edges() {
                        backdrop.set_anchor(edge, true);
                    }
                    backdrop.set_monitor(Some(monitor));
                    // A transparent Cairo node gives the surface a real buffer to catch input.
                    let catcher = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
                    catcher.set_draw_func(|_, cr, _, _| {
                        cr.set_source_rgba(0., 0., 0., 0.);
                        let _ = cr.paint();
                    });
                    backdrop.set_child(Some(&catcher));
                    let click = gtk::GestureClick::new();
                    click.set_button(0);
                    click.set_propagation_phase(gtk::PropagationPhase::Capture);
                    let weak = self.me.borrow().clone();
                    click.connect_released(move |_, _, _, _| {
                        if let Some(v) = weak.upgrade() {
                            (v.on_cancel)();
                        }
                    });
                    backdrop.add_controller(click.clone());
                    backdrops.push((monitor.clone(), backdrop.clone(), click));
                    backdrop
                }
            };
            let geometry = monitor.geometry();
            backdrop.set_default_size(geometry.width(), geometry.height());
        }
        let chosen = primary.and_then(|p| backdrops.iter().find(|(m, _, _)| m == p)).or_else(|| backdrops.first())
            .map(|(_, w, g)| (w.clone(), g.clone()));
        *self.primary.borrow_mut() = chosen;
    }

    /// The primary monitor's click-away backdrop (Python `view.backdrop`).
    pub fn backdrop(&self) -> Option<gtk::Window> {
        self.primary.borrow().as_ref().map(|(w, _)| w.clone())
    }

    /// Its outside-click gesture (Python `view._outside_click`).
    pub fn outside_click(&self) -> Option<gtk::GestureClick> {
        self.primary.borrow().as_ref().map(|(_, g)| g.clone())
    }

    fn new_card(&self, ident: &str) -> Rc<Card> {
        let button = gtk::Button::new();
        button.add_css_class("switcher-card");
        // Arrow/Tab semantics belong to the controller, not GTK focus traversal.
        button.set_focusable(false);
        button.set_hexpand(true);
        let weak = self.me.borrow().clone();
        let id = ident.to_string();
        button.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                (v.on_activate)(&id);
            }
        });
        let has_close = self.on_close.borrow().is_some();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        if has_close {
            content.set_margin_top(16);
        }
        let image = gtk::Image::new();
        image.set_pixel_size(44);
        image.set_margin_bottom(3);
        content.append(&image);
        // SCALE_DOWN keeps small dialogs at their real size.
        let preview = gtk::Picture::builder().can_shrink(true).content_fit(gtk::ContentFit::ScaleDown).build();
        preview.add_css_class("window-preview");
        preview.set_visible(false);
        let preview_layer = gtk::Overlay::new();
        preview_layer.set_child(Some(&preview));
        let fallback = gtk::Image::builder().pixel_size(72).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        preview_layer.add_overlay(&fallback);
        let fb = fallback.clone();
        preview.connect_paintable_notify(move |picture| fb.set_visible(picture.paintable().is_none()));
        preview_layer.set_visible(false);
        content.prepend(&preview_layer);
        let name = gtk::Label::new(None);
        name.add_css_class("switcher-name");
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_max_width_chars(18);
        name.set_width_chars(9);
        content.append(&name);
        let title = gtk::Label::new(None);
        title.add_css_class("switcher-title");
        title.set_ellipsize(pango::EllipsizeMode::End);
        title.set_max_width_chars(23);
        title.set_width_chars(9);
        content.append(&title);
        button.set_child(Some(&content));
        let tile = gtk::Overlay::new();
        tile.add_css_class("switcher-tile");
        tile.set_child(Some(&button));
        let close = gtk::Button::new();
        close.add_css_class("switcher-card-close");
        close.set_focusable(false);
        close.set_halign(gtk::Align::End);
        close.set_valign(gtk::Align::Start);
        close.set_margin_top(4);
        close.set_margin_end(4);
        close.set_tooltip_text(Some(&tr!("Close window")));
        let close_image = gtk::Image::from_icon_name("window-close-symbolic");
        close_image.set_pixel_size(12);
        close.set_child(Some(&close_image));
        let weak = self.me.borrow().clone();
        let id = ident.to_string();
        close.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.close_card(&id);
            }
        });
        close.set_visible(has_close);
        tile.add_overlay(&close);
        let mute = gtk::Button::new();
        mute.add_css_class("switcher-card-close");
        let mute_image = gtk::Image::from_icon_name("audio-volume-high-symbolic");
        mute_image.set_pixel_size(12);
        mute.set_child(Some(&mute_image));
        mute.set_focusable(false);
        mute.set_halign(gtk::Align::End);
        mute.set_valign(gtk::Align::Start);
        mute.set_margin_top(4);
        mute.set_margin_end(32);
        mute.set_visible(false);
        let weak = self.me.borrow().clone();
        let id = ident.to_string();
        mute.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                let cb = v.on_mute.borrow().clone();
                if let Some(cb) = cb {
                    cb(&id);
                }
            }
        });
        tile.add_overlay(&mute);
        let child = gtk::FlowBoxChild::new();
        child.set_focusable(false);
        child.set_child(Some(&tile));
        Rc::new(Card {
            button, child, tile, close, mute, name, title, image, preview, preview_layer, fallback, content,
            caption: gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build(),
            icon: RefCell::new(None), tooltip: RefCell::new(None), preview_key: RefCell::new(None),
        })
    }

    fn close_card(&self, ident: &str) {
        let cb = self.on_close.borrow().clone();
        if !self.closed.get() && self.cards.borrow().contains_key(ident) {
            if let Some(cb) = cb {
                cb(ident);
            }
        }
    }

    pub fn render(&self, items: &[Item], selected_id: Option<&str>) {
        if self.closed.get() {
            return;
        }
        let selection_changed = selected_id.map(str::to_string) != *self.selected.borrow();
        // Defensive deduplication (dict semantics: last value, first position).
        let mut unique: Vec<Item> = Vec::new();
        for item in items {
            match unique.iter_mut().find(|u| u.id == item.id) {
                Some(slot) => *slot = item.clone(),
                None => unique.push(item.clone()),
            }
        }
        let order: Vec<String> = unique.iter().map(|i| i.id.clone()).collect();
        *self.items.borrow_mut() = unique.clone();
        let gone: Vec<String> = self.cards.borrow().keys().filter(|k| !order.contains(k)).cloned().collect();
        for ident in gone {
            if let Some(card) = self.cards.borrow_mut().remove(&ident) {
                self.grid.remove(&card.child);
            }
        }
        let has_mute = self.on_mute.borrow().is_some();
        for item in &unique {
            let existing = self.cards.borrow().get(&item.id).cloned();
            let card = match existing {
                Some(c) => c,
                None => {
                    let c = self.new_card(&item.id);
                    self.cards.borrow_mut().insert(item.id.clone(), c.clone());
                    c
                }
            };
            update_mute_button(&card.mute, item.audio_available, item.muted);
            card.mute.set_visible(has_mute && item.audio_available);
            if card.preview_key.borrow().as_deref() != Some(item.app_id.as_str()) {
                card.preview.set_paintable(None::<&gdk::Paintable>);
                *card.preview_key.borrow_mut() = Some(item.app_id.clone());
            }
            let name = if item.name.is_empty() { tr!("Application") } else { item.name.clone() };
            let title = if item.title.is_empty() { tr!("Untitled window") } else { item.title.clone() };
            if card.name.label() != name {
                card.name.set_label(&name);
            }
            if card.title.label() != title {
                card.title.set_label(&title);
            }
            let icon_key = item.icon.as_ref().and_then(|i| IconExt::to_string(i)).map(|s| s.to_string())
                .unwrap_or_else(|| "application-x-executable".into());
            if card.icon.borrow().as_deref() != Some(icon_key.as_str()) {
                match &item.icon {
                    Some(icon) => {
                        card.image.set_from_gicon(icon);
                        card.fallback.set_from_gicon(icon);
                    }
                    None => {
                        card.image.set_icon_name(Some("application-x-executable"));
                        card.fallback.set_icon_name(Some("application-x-executable"));
                    }
                }
                *card.icon.borrow_mut() = Some(icon_key);
            }
            let selected = Some(item.id.as_str()) == selected_id;
            for widget in [card.button.upcast_ref::<gtk::Widget>(), card.tile.upcast_ref()] {
                if selected { widget.add_css_class("selected") } else { widget.remove_css_class("selected") }
            }
            if item.minimized { card.button.add_css_class("minimized") } else { card.button.remove_css_class("minimized") }
            let tooltip = format!("{name}\n{title}{}", if item.minimized { "\nMinimized" } else { "" });
            if card.tooltip.borrow().as_deref() != Some(tooltip.as_str()) {
                card.button.set_tooltip_text(Some(&tooltip));
                *card.tooltip.borrow_mut() = Some(tooltip);
            }
        }
        if order != *self.order.borrow() {
            // Reparent only when the actual ordering changes.
            for ident in &order {
                let child = self.cards.borrow()[ident].child.clone();
                if child.parent().as_ref() == Some(self.grid.upcast_ref()) {
                    self.grid.remove(&child);
                }
                self.grid.insert(&child, -1);
            }
            *self.order.borrow_mut() = order.clone();
        }
        *self.selected.borrow_mut() = selected_id.map(str::to_string);
        self.count.set_label(&order.len().to_string());
        self.scroller.set_visible(!order.is_empty());
        self.empty.set_visible(order.is_empty());
        self.layout();
        // Metadata updates must not pull the viewport back while browsing.
        if selection_changed {
            self.queue_scroll();
        }
    }

    pub fn layout(&self) {
        let geometry = self.monitor.borrow().as_ref().map(|m| m.geometry());
        let (width, height) = geometry.map(|g| (g.width(), g.height())).unwrap_or((1024, 768));
        let activities = self.mode.get() == Mode::Activities;
        let count = self.items.borrow().len();
        let limits = if activities { overview_layout(width, height, count) } else { layout_limits(width, height, count) };
        if limits.columns != self.columns.get() {
            self.columns.set(limits.columns);
            self.grid.set_min_children_per_line(limits.columns as u32);
            self.grid.set_max_children_per_line(limits.columns as u32);
        }
        self.scroller.set_max_content_height(limits.content_height);
        let has_close = self.on_close.borrow().is_some();
        for card in self.cards.borrow().values() {
            let (content, caption) = (&card.content, &card.caption);
            if activities && caption.parent().is_none() {
                for child in [card.image.upcast_ref::<gtk::Widget>(), card.title.upcast_ref()] {
                    content.remove(child);
                    caption.append(child);
                }
                content.append(caption);
            } else if !activities && caption.parent().is_some() {
                for child in [card.image.upcast_ref::<gtk::Widget>(), card.title.upcast_ref()] {
                    caption.remove(child);
                }
                content.remove(caption);
                content.insert_child_after(&card.image, Some(&card.preview_layer));
                content.append(&card.title);
            }
            card.name.set_visible(!activities);
            content.set_margin_top(if activities { 0 } else if has_close { 16 } else { 0 });
            card.preview.set_visible(activities);
            card.preview_layer.set_visible(activities);
            card.preview.set_size_request(-1, if activities { limits.preview_height } else { -1 });
            card.image.set_pixel_size(if activities { 24 } else { 44 });
        }
        self.grid.set_valign(if activities { gtk::Align::Center } else { gtk::Align::Start });
        let size = if activities { (width, height) } else { (limits.width, limits.height) };
        if Some(size) != self.size.get() {
            self.window.set_default_size(size.0, size.1);
            self.size.set(Some(size));
        }
    }

    fn queue_scroll(&self) {
        if self.scroll_idle.borrow().is_some() {
            return;
        }
        let weak = self.me.borrow().clone();
        let id = glib::idle_add_local_once(move || {
            if let Some(v) = weak.upgrade() {
                v.scroll_idle.borrow_mut().take();
                v.scroll_selected();
            }
        });
        *self.scroll_idle.borrow_mut() = Some(id);
    }

    fn scroll_selected(&self) {
        if self.closed.get() {
            return;
        }
        let selected = self.selected.borrow().clone();
        let card = selected.and_then(|s| self.cards.borrow().get(&s).cloned());
        if let Some(card) = card {
            if let Some(bounds) = card.child.compute_bounds(&self.grid) {
                self.scroller.vadjustment().clamp_page(bounds.y() as f64, (bounds.y() + bounds.height()) as f64);
            }
        }
    }

    pub fn show(&self, monitor: Option<gdk::Monitor>) {
        if self.closed.get() {
            return;
        }
        let monitor = monitor.or_else(|| crate::monitors::monitor_list(&WidgetExt::display(&self.window)).into_iter().next());
        if self.window.get_visible() && monitor != *self.monitor.borrow() {
            // A layer surface's output is immutable after its first configure.
            self.dismiss();
        }
        *self.monitor.borrow_mut() = monitor.clone();
        #[cfg(feature = "theme")]
        tessel_theme::set_window_monitor(&self.window, monitor.as_ref());
        if let Some(m) = &monitor {
            self.window.set_monitor(Some(m));
        }
        self.sync_backdrops(monitor.as_ref());
        self.layout();
        if self.mode.get() == Mode::Activities {
            self.stop_capture();
            let connector = monitor.as_ref().and_then(|m| m.connector()).map(|c| c.to_string()).unwrap_or_default();
            self.stage.set_wallpaper((self.wallpaper)(&connector));
            let weak = self.me.borrow().clone();
            let weak2 = weak.clone();
            let capture = OverviewCapture::new(
                move |frame| {
                    if let Some(v) = weak.upgrade() {
                        v.captured(frame);
                    }
                },
                move || {
                    if let Some(v) = weak2.upgrade() {
                        v.present();
                    }
                },
            );
            *self.capture.borrow_mut() = Some(capture.clone());
            // Capture the desktop before mapping, with a short deadline.
            let weak = self.me.borrow().clone();
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(220), move || {
                if let Some(v) = weak.upgrade() {
                    v.present_source.borrow_mut().take();
                    v.present();
                }
            });
            *self.present_source.borrow_mut() = Some(id);
            capture.start(&connector);
        } else {
            self.present();
        }
    }

    pub fn present(&self) {
        if let Some(source) = self.present_source.borrow_mut().take() {
            source.remove();
        }
        if self.closed.get() || (self.mode.get() == Mode::Activities && self.capture.borrow().is_none()) {
            return;
        }
        if self.window.get_visible() {
            return;
        }
        for (_, backdrop, _) in self.backdrops.borrow().iter() {
            backdrop.set_visible(true);
        }
        self.window.present();
        if self.mode.get() == Mode::Activities {
            let desktop = self.desktop_texture.borrow_mut().take();
            self.stage.animate(desktop);
        }
        self.queue_scroll();
    }

    fn captured(&self, frame: CaptureFrame) {
        if self.closed.get() || self.mode.get() != Mode::Activities || self.capture.borrow().is_none() {
            return;
        }
        match frame {
            CaptureFrame::Identities(keys) => {
                let mut counts = HashMap::new();
                for key in keys {
                    *counts.entry(key).or_insert(0) += 1;
                }
                *self.identities.borrow_mut() = counts;
            }
            CaptureFrame::Desktop(texture) => {
                if !self.window.get_visible() {
                    *self.desktop_texture.borrow_mut() = Some(texture);
                    self.present();
                }
            }
            CaptureFrame::Window { key, texture } => {
                let target = {
                    let items = self.items.borrow();
                    let identities = self.identities.borrow();
                    unique_preview_target(&key, items.iter().map(|i| (i.id.as_str(), i.app_id.as_str(), i.title.as_str())), &identities)
                };
                if let Some(card) = target.and_then(|t| self.cards.borrow().get(&t).cloned()) {
                    card.preview.set_paintable(Some(&texture));
                }
            }
        }
    }

    pub fn identities(&self) -> HashMap<Key, usize> {
        self.identities.borrow().clone()
    }

    fn stop_capture(&self) {
        if let Some(source) = self.present_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(capture) = self.capture.borrow_mut().take() {
            capture.close();
        }
        self.identities.borrow_mut().clear();
        self.desktop_texture.borrow_mut().take();
        self.stage.stop();
        for card in self.cards.borrow().values() {
            card.preview.set_paintable(None::<&gdk::Paintable>);
        }
        let _ = preview_key;
    }

    pub fn dismiss(&self) {
        self.stop_capture();
        self.stage.set_wallpaper(None);
        if let Some(source) = self.scroll_idle.borrow_mut().take() {
            source.remove();
        }
        self.window.set_visible(false);
        for (_, backdrop, _) in self.backdrops.borrow().iter() {
            backdrop.set_visible(false);
        }
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        if let Some(handler) = self.monitors_handler.borrow_mut().take() {
            WidgetExt::display(&self.window).monitors().disconnect(handler);
        }
        self.dismiss();
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.window), &self.provider);
        self.window.destroy();
        self.primary.borrow_mut().take();
        for (_, backdrop, _) in self.backdrops.borrow_mut().drain(..) {
            backdrop.destroy();
        }
        self.cards.borrow_mut().clear();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }
}
