//! The window chooser popup (dock_ui.py `cancel_preview_timer` …
//! `close_preview`, `activate_window`, `close_window`, `close_all_windows`)
//! with per-application playback mute (ui::app_audio).
//!
//! # Public API
//! - [`WindowPreview::new(host)`] → `Rc<WindowPreview<W>>` (one per dock
//!   clone): `queue(&item)` (220 ms hover delay), `cancel_timer()`,
//!   `show(&item)`, `update(&item)` (call on every render while open),
//!   `close()`, `is_open()`, `window()` (= `self.preview`), `preview_id()`,
//!   `rows()` (= `preview_buttons`, id → [`PreviewRow`]), `preview_box()`,
//!   `audio()`.
//! - Window rules (contract §5): `activate_window(id)` — focused & not
//!   minimized → minimize, else activate (state read at click time); closes
//!   the preview first; `close_window(id)` — only that window, a vanished
//!   window needs no action; `close_all_windows(item_id)` — snapshots ids.
//!   The dock's own single-window click must also call `activate_window`.
//! - [`WindowActions`] (implemented here for `wl::WindowBackend`) and
//!   [`PreviewHost`] — what the dock surface (agent E) implements.
//!
//! Surface: `tessel-dock-windows` (TOP, keyboard NONE), class `popup_class`;
//! rows are `Gtk.Overlay` tiles: activation button (icon 32 + ellipsized
//! `tessel-dock-window-title`) with sibling `tessel-dock-window-close` close (×) and
//! mute buttons. Previews show icons and titles (no thumbnails), so there is
//! no content-fit to choose.

use gtk::glib;
use gtk::prelude::*;
use gtk::{gio, pango};
use gtk4_layer_shell::Layer;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

use crate::core::model::{DockItem, WindowRecord};
use crate::ui::app_audio::{update_mute_button, ApplicationAudio, AudioControl};
use crate::ui::app_menu::logic::scroll_value;
use crate::ui::app_menu::popup::{
    accessible_name, children, close_blur, crossing, icon_image, named_image, pack, position_popup, remove, surface, toggle_class, PopupAnchor,
    PopupHost,
};
use crate::wl::{Window, WindowBackend};
use tessel_core::tr;

/// Window operations previews perform (Python `self.app.backend`).
pub trait WindowActions {
    /// `backend.windows.get(id)` (current state).
    fn window(&self, id: &str) -> Option<Window>;
    fn activate(&self, id: &str) -> bool;
    fn minimize(&self, id: &str) -> bool;
    fn close_window(&self, id: &str) -> bool;
}

impl WindowActions for WindowBackend {
    fn window(&self, id: &str) -> Option<Window> {
        WindowBackend::window(self, id)
    }
    fn activate(&self, id: &str) -> bool {
        WindowBackend::activate(self, id)
    }
    fn minimize(&self, id: &str) -> bool {
        WindowBackend::minimize(self, id)
    }
    fn close_window(&self, id: &str) -> bool {
        WindowBackend::close_window(self, id)
    }
}

/// What the preview needs from its dock surface (beyond [`PopupHost`]).
pub trait PreviewHost<W: WindowRecord>: PopupHost {
    /// `indicators.tray_expander.expanded` (an open tray expander blocks previews).
    fn tray_expanded(&self) -> bool;
    /// `self.menu_window or self.context_menu` (either blocks previews).
    fn menu_open(&self) -> bool;
    /// `find_item(item_id)`: current utility or rendered item.
    fn find_item(&self, item_id: &str) -> Option<DockItem<W>>;
    /// `self.app.backend`.
    fn actions(&self) -> Rc<dyn WindowActions>;
    /// `self.app.show_error(text)` (already translated).
    fn show_error(&self, text: &str);
    /// `self.app.focused_title_state.clear()`.
    fn clear_focused_title(&self);
    /// `self.application_expander.set_expanded(False)` when it exists.
    fn collapse_application_expander(&self);
    /// `config.icon_size` (horizontal wheel scrolling step).
    fn icon_size(&self) -> i32;
    /// `ApplicationAudio(self.app.catalog, changed)` (tests inject fakes).
    fn create_audio(&self, changed: Rc<dyn Fn()>) -> Rc<dyn AudioControl> {
        Rc::new(ApplicationAudio::new(self.catalog(), changed))
    }
}

/// One window tile (`preview_buttons[id]` and its attributes).
pub struct PreviewRow {
    pub button: gtk::Button,
    pub tile: gtk::Overlay,
    pub close_button: gtk::Button,
    pub mute_button: gtk::Button,
    pub title_label: gtk::Label,
    pub app_image: gtk::Image,
    app_entry: RefCell<Option<(String, Option<gio::Icon>)>>,
    audio_app_id: RefCell<String>,
}

impl PreviewRow {
    /// `button.audio_app_id` (the window's app id).
    pub fn audio_app_id(&self) -> String {
        self.audio_app_id.borrow().clone()
    }
}

/// `reorder(box, widget, index)`.
fn reorder(container: &gtk::Box, widget: &impl IsA<gtk::Widget>, index: usize) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    let others: Vec<gtk::Widget> = children(container).into_iter().filter(|c| c != widget).collect();
    let sibling = if index > 0 { others.get(index - 1) } else { None };
    container.reorder_child_after(widget, sibling);
}

pub struct WindowPreview<W: WindowRecord + 'static> {
    host: Weak<dyn PreviewHost<W>>,
    timer: RefCell<Option<glib::SourceId>>,
    popup: RefCell<Option<gtk::Window>>,
    preview_id: RefCell<Option<String>>,
    rows: RefCell<IndexMap<String, Rc<PreviewRow>>>,
    preview_box: RefCell<Option<gtk::Box>>,
    audio: RefCell<Option<Rc<dyn AudioControl>>>,
    this: Weak<WindowPreview<W>>,
}

impl<W: WindowRecord + 'static> WindowPreview<W> {
    pub fn new(host: Weak<dyn PreviewHost<W>>) -> Rc<Self> {
        Rc::new_cyclic(|this| WindowPreview {
            host,
            timer: RefCell::new(None),
            popup: RefCell::new(None),
            preview_id: RefCell::new(None),
            rows: RefCell::new(IndexMap::new()),
            preview_box: RefCell::new(None),
            audio: RefCell::new(None),
            this: this.clone(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.popup.borrow().is_some()
    }
    /// `self.preview`.
    pub fn window(&self) -> Option<gtk::Window> {
        self.popup.borrow().clone()
    }
    pub fn preview_id(&self) -> Option<String> {
        self.preview_id.borrow().clone()
    }
    /// `self.preview_buttons` (insertion order).
    pub fn rows(&self) -> IndexMap<String, Rc<PreviewRow>> {
        self.rows.borrow().clone()
    }
    pub fn row(&self, window_id: &str) -> Option<Rc<PreviewRow>> {
        self.rows.borrow().get(window_id).cloned()
    }
    /// `self.preview_box`.
    pub fn preview_box(&self) -> Option<gtk::Box> {
        self.preview_box.borrow().clone()
    }
    /// `self.preview_audio`.
    pub fn audio(&self) -> Option<Rc<dyn AudioControl>> {
        self.audio.borrow().clone()
    }
    /// Whether the hover delay is pending (`self.preview_timer`).
    pub fn timer_pending(&self) -> bool {
        self.timer.borrow().is_some()
    }

    /// `cancel_preview_timer`.
    pub fn cancel_timer(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
    }

    /// `queue_preview(item)`.
    pub fn queue(&self, item: &DockItem<W>) {
        self.cancel_timer();
        if item.windows.is_empty() {
            self.close();
            return;
        }
        let (this, id) = (self.this.clone(), item.id.clone());
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(220), move || {
            let Some(this) = this.upgrade() else { return };
            this.timer.borrow_mut().take();
            let Some(host) = this.host.upgrade() else { return };
            if !host.destroyed() {
                if let Some(current) = host.find_item(&id) {
                    this.show(&current);
                }
            }
        });
        *self.timer.borrow_mut() = Some(source);
    }

    /// `show_preview(item)`.
    pub fn show(&self, item: &DockItem<W>) {
        let Some(host) = self.host.upgrade() else { return };
        if host.tray_expanded() {
            return;
        }
        self.cancel_timer();
        if item.windows.is_empty() || host.menu_open() {
            return;
        }
        if self.is_open() && self.preview_id().as_deref() == Some(item.id.as_str()) {
            self.update(item);
            return;
        }
        self.close();
        *self.preview_id.borrow_mut() = Some(item.id.clone());
        let horizontal = host.horizontal();
        let popup = surface(host.application().as_ref(), &host.monitor(), "tessel-dock-windows", Layer::Top, false);
        popup.add_css_class(&host.popup_class());
        *self.popup.borrow_mut() = Some(popup.clone());
        let orientation = if horizontal { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
        let content = gtk::Box::new(orientation, 6);
        content.add_css_class("tessel-dock-popup");
        *self.preview_box.borrow_mut() = Some(content.clone());
        let scroll = gtk::ScrolledWindow::new();
        if horizontal {
            scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        } else {
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        }
        scroll.set_propagate_natural_width(true);
        scroll.set_propagate_natural_height(true);
        let geometry = host.monitor().geometry();
        scroll.set_max_content_width(220.max(geometry.width() - 32));
        scroll.set_max_content_height(160.max(geometry.height() - 140));
        if horizontal {
            let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
            let weak = self.host.clone();
            controller.connect_scroll(move |controller, dx, dy| {
                let (Some(host), Some(widget)) = (weak.upgrade(), controller.widget()) else { return glib::Propagation::Proceed };
                let Some(scroll) = widget.downcast_ref::<gtk::ScrolledWindow>() else { return glib::Propagation::Proceed };
                let adjustment = scroll.hadjustment();
                match scroll_value(adjustment.lower(), adjustment.upper(), adjustment.page_size(), adjustment.value(), dx, dy, host.icon_size()) {
                    Some(value) => {
                        adjustment.set_value(value);
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                }
            });
            scroll.add_controller(controller);
        }
        scroll.set_child(Some(&content));
        popup.set_child(Some(&scroll));
        host.set_popup_blur(&popup, false);
        let (enter, leave) = (self.host.clone(), self.host.clone());
        crossing(
            &popup,
            move || {
                if let Some(host) = enter.upgrade() {
                    host.enter("preview");
                }
            },
            move || {
                if let Some(host) = leave.upgrade() {
                    host.leave("preview");
                }
            },
        );
        let this = self.this.clone();
        let audio = host.create_audio(Rc::new(move || {
            if let Some(this) = this.upgrade() {
                this.audio_changed();
            }
        }));
        *self.audio.borrow_mut() = Some(audio.clone());
        audio.start();
        self.update(item);
        content.set_visible(true);
        scroll.set_visible(true);
        position_popup(&*host, &popup, &PopupAnchor::Item(item.id.clone()));
        popup.set_visible(true);
    }

    fn new_row(&self, window_id: &str, horizontal: bool) -> Rc<PreviewRow> {
        let button = gtk::Button::new();
        let tile = gtk::Overlay::new();
        tile.set_child(Some(&button));
        let content = gtk::Box::new(if horizontal { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal }, 8);
        // Reserve space for the independent close control so it never
        // overlaps the application icon or abbreviated window title.
        if horizontal {
            content.set_margin_top(16);
        } else {
            content.set_margin_end(50);
        }
        let app_image = icon_image(None, 32);
        pack(&content, &app_image, false);
        let label = gtk::Label::new(None);
        label.set_single_line_mode(true);
        label.set_ellipsize(pango::EllipsizeMode::End);
        let chars = if horizontal { 19 } else { 28 };
        label.set_width_chars(chars);
        label.set_max_width_chars(chars);
        label.add_css_class("tessel-dock-window-title");
        pack(&content, &label, false);
        button.set_child(Some(&content));
        let (this, id) = (self.this.clone(), window_id.to_owned());
        button.connect_clicked(move |_| {
            if let Some(this) = this.upgrade() {
                this.activate_window(&id);
            }
        });
        let close = gtk::Button::new();
        close.add_css_class("tessel-dock-window-close");
        close.set_child(Some(&named_image("window-close-symbolic", 12)));
        close.set_halign(gtk::Align::End);
        close.set_valign(gtk::Align::Start);
        close.set_margin_end(2);
        close.set_margin_top(2);
        close.set_tooltip_text(Some(&tr!("Close window")));
        let (this, id) = (self.this.clone(), window_id.to_owned());
        close.connect_clicked(move |_| {
            if let Some(this) = this.upgrade() {
                this.close_window(&id);
            }
        });
        tile.add_overlay(&close);
        close.set_can_target(true);
        let mute = gtk::Button::new();
        mute.add_css_class("tessel-dock-window-close");
        mute.set_child(Some(&named_image("audio-volume-high-symbolic", 12)));
        mute.set_halign(gtk::Align::End);
        mute.set_valign(gtk::Align::Start);
        mute.set_margin_end(30);
        mute.set_margin_top(2);
        let (this, id) = (self.this.clone(), window_id.to_owned());
        mute.connect_clicked(move |_| {
            if let Some(this) = this.upgrade() {
                this.mute(&id);
            }
        });
        tile.add_overlay(&mute);
        Rc::new(PreviewRow {
            button,
            tile,
            close_button: close,
            mute_button: mute,
            title_label: label,
            app_image,
            app_entry: RefCell::new(None),
            audio_app_id: RefCell::new(String::new()),
        })
    }

    /// `update_preview(item)`.
    pub fn update(&self, item: &DockItem<W>) {
        let Some(host) = self.host.upgrade() else { return };
        if !self.is_open() || self.preview_id().as_deref() != Some(item.id.as_str()) {
            return;
        }
        if item.windows.is_empty() {
            self.close();
            return;
        }
        let Some(container) = self.preview_box() else { return };
        let horizontal = host.horizontal();
        let ids: Vec<String> = item.windows.iter().map(|w| w.id()).collect();
        let stale: Vec<Rc<PreviewRow>> = {
            let mut rows = self.rows.borrow_mut();
            let gone: Vec<String> = rows.keys().filter(|id| !ids.contains(id)).cloned().collect();
            gone.iter().filter_map(|id| rows.shift_remove(id)).collect()
        };
        for row in stale {
            remove(&row.tile);
        }
        let audio = self.audio();
        for (index, window) in item.windows.iter().enumerate() {
            let window_id = window.id();
            let existing = self.row(&window_id);
            let row = match existing {
                Some(row) => row,
                None => {
                    let row = self.new_row(&window_id, horizontal);
                    pack(&container, &row.tile, false);
                    self.rows.borrow_mut().insert(window_id.clone(), row.clone());
                    row.tile.set_visible(true);
                    row
                }
            };
            let entry = item.app.as_ref().map(|app| (app.id.clone(), app.icon.clone()));
            let same = match (&*row.app_entry.borrow(), &entry) {
                (None, None) => true,
                (Some((a, ai)), Some((b, bi))) => a == b && ai.as_ref().map(|i| i.to_string()) == bi.as_ref().map(|i| i.to_string()),
                _ => false,
            };
            if !same || row.app_image.gicon().is_none() {
                let fallback: gio::Icon = gio::ThemedIcon::new("application-x-executable").upcast();
                let icon = entry.as_ref().and_then(|(_, icon)| icon.clone()).unwrap_or(fallback);
                row.app_image.set_from_gicon(&icon);
                row.app_image.set_pixel_size(32);
                *row.app_entry.borrow_mut() = entry;
            }
            let title = if window.title().is_empty() { item.name.clone() } else { window.title() };
            if row.title_label.text() != title {
                row.title_label.set_text(&title);
                row.button.set_tooltip_text(Some(&title));
                accessible_name(&row.close_button, &tr!("Close {value1}", value1 = title.as_str()));
            }
            toggle_class(&row.button, "focused", window.activated() && !window.minimized());
            *row.audio_app_id.borrow_mut() = window.app_id();
            let (available, muted) = audio.as_ref().map(|a| a.state(&window.app_id())).unwrap_or((false, false));
            update_mute_button(&row.mute_button, available, muted);
            reorder(&container, &row.tile, index);
        }
    }

    /// `_preview_audio_changed`.
    fn audio_changed(&self) {
        let (Some(_), Some(audio)) = (self.window(), self.audio()) else { return };
        for row in self.rows().values() {
            let (available, muted) = audio.state(&row.audio_app_id());
            update_mute_button(&row.mute_button, available, muted);
        }
    }

    /// `_mute_preview(window_id)`.
    pub fn mute(&self, window_id: &str) {
        if let (Some(row), Some(audio)) = (self.row(window_id), self.audio()) {
            audio.toggle(&row.audio_app_id());
        }
    }

    /// `close_preview`.
    pub fn close(&self) {
        self.cancel_timer();
        if let Some(audio) = self.audio.borrow_mut().take() {
            audio.close();
        }
        let popup = self.popup.borrow_mut().take();
        if let Some(popup) = popup {
            close_blur(&popup);
            popup.destroy();
            if let Some(host) = self.host.upgrade() {
                host.leave("preview");
            }
        }
        *self.preview_id.borrow_mut() = None;
        *self.preview_box.borrow_mut() = None;
        self.rows.borrow_mut().clear();
    }

    /// `activate_window(window_id)`: the contract §5 click rule.
    pub fn activate_window(&self, window_id: &str) -> bool {
        let Some(host) = self.host.upgrade() else { return false };
        host.clear_focused_title();
        self.close();
        host.collapse_application_expander();
        // Resolve current state at click time: a preview can outlive a focus
        // change, and a minimized window should always restore on selection.
        let actions = host.actions();
        let focused = actions.window(window_id).is_some_and(|w| w.activated && !w.minimized);
        let succeeded = if focused { actions.minimize(window_id) } else { actions.activate(window_id) };
        if !succeeded {
            host.show_error(&tr!("This window is no longer available."));
        }
        succeeded
    }

    /// `close_window(window_id)`: a window can disappear between pointer-down
    /// and release; never focus/minimize a neighbour.
    pub fn close_window(&self, window_id: &str) {
        let Some(host) = self.host.upgrade() else { return };
        let actions = host.actions();
        if actions.window(window_id).is_some() && !actions.close_window(window_id) {
            host.show_error(&tr!("This window could not be closed."));
        }
    }

    /// `close_all_windows(item_id)` ("Close All"): resolve the group when the
    /// action runs, then snapshot ids (closing changes groups immediately).
    pub fn close_all_windows(&self, item_id: &str) {
        self.close();
        let Some(host) = self.host.upgrade() else { return };
        let Some(item) = host.find_item(item_id) else { return };
        let ids: Vec<String> = item.windows.iter().map(|w| w.id()).collect();
        let actions = host.actions();
        let mut failed = false;
        for id in ids {
            if actions.window(&id).is_some() {
                failed = !actions.close_window(&id) || failed;
            }
        }
        if failed {
            host.show_error(&tr!("Some windows could not be closed."));
        }
    }
}
