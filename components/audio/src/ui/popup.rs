//! Compact system-volume menu for the panel audio button (audio_popup.py).

#![allow(deprecated)]

use super::common::*;
use super::rows::NodeRow;
use crate::api::{Action, AudioApi};
use crate::model::Node;
use gtk::prelude::*;
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::gtk;
use tessel_gtk::popup::{header_button, popup_header, section_title, set_popup_width, HeaderAction, PopupController, POPUP_PADDING, POPUP_WIDTH};
use tessel_gtk::utils::{accessible, append};

pub const POPUP_CSS: &str = r#"
window.audiomanager.tray-popup { background-color: transparent; }
.audiomanager.tray-popup .popup-surface {
    background-color: @theme_bg_color;
    border: 1px solid alpha(@theme_fg_color, 0.14);
    border-radius: 12px;
}
.audiomanager.tray-popup .popup-status { font-size: 9pt; }
.audiomanager.tray-popup .popup-status.error {
    color: mix(@theme_fg_color, #e05d65, 0.75);
}
.audiomanager.tray-popup button { min-height: 28px; }
.audiomanager.tray-popup scale { padding: 4px 8px; }
.audiomanager.tray-popup combobox button { min-height: 26px; padding: 2px 8px; }
.audiomanager.tray-popup scale mark { font-size: 8pt; }
.audiomanager.tray-popup scrollbar slider { min-width: 6px; min-height: 24px; margin: 0; padding: 0; border: none; }
"#;

pub struct VolumePopup {
    weak: RefCell<Weak<VolumePopup>>,
    backend: Rc<dyn AudioApi>,
    open_settings: Box<dyn Fn()>,
    syncing: Cell<bool>,
    dragging: Cell<bool>,
    volume_timeout: RefCell<Option<glib::SourceId>>,
    pending_volume: RefCell<Option<(Option<(i64, i64)>, f64)>>,
    node_id: Cell<Option<i64>>,
    node_identity: Cell<Option<(i64, i64)>>,
    error: RefCell<String>,
    destroyed: Cell<bool>,
    pub node_rows: RefCell<IndexMap<i64, Rc<NodeRow>>>,
    stream_height_limit: Cell<i32>,
    pub window: gtk::ApplicationWindow,
    css: RefCell<Vec<gtk::CssProvider>>,
    pub controller: RefCell<Option<PopupController>>,
    pub settings_button: gtk::Button,
    pub output_icon: gtk::Image,
    pub output_name: gtk::Label,
    pub output_combo: ChoiceCombo,
    pub input_combo: ChoiceCombo,
    pub volume_label: gtk::Label,
    pub volume_scale: gtk::Scale,
    pub stream_scroller: gtk::ScrolledWindow,
    pub stream_sections: IndexMap<&'static str, (gtk::Box, gtk::Label)>,
    pub mute_button: gtk::ToggleButton,
    pub mute_icon: gtk::Image,
    pub status_label: gtk::Label,
}

impl View for VolumePopup {
    fn backend(&self) -> Rc<dyn AudioApi> {
        self.backend.clone()
    }
    fn perform(&self, action: Action) {
        if self.destroyed.get() || !self.backend.state().ready {
            return;
        }
        self.error.borrow_mut().clear();
        match action.run(&*self.backend) {
            Ok(false) => {
                let error = self.backend.state().error.clone();
                self.set_error(&if error.is_empty() { tr!("Unable to change audio settings.") } else { error });
            }
            Err(error) => self.set_error(&error),
            Ok(true) => {}
        }
    }
    fn set_error(&self, message: &str) {
        *self.error.borrow_mut() = message.to_owned();
        self.refresh();
    }
}

impl VolumePopup {
    pub fn new(application: &gtk::Application, backend: Rc<dyn AudioApi>, open_settings: impl Fn() + 'static, edge: &str, offset: i32) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(application);
        window.set_title(Some(&tr!("Volume")));
        set_popup_width(window.upcast_ref(), POPUP_WIDTH);
        window.add_css_class("audiomanager");
        let mut css = Vec::new();
        for stylesheet in [STYLE_CSS, POPUP_CSS] {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(stylesheet);
            gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            css.push(provider);
        }
        tessel_gtk::theme::attach(&window, None);

        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("popup-surface");
        window.set_child(Some(&surface));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, POPUP_PADDING);
        surface.append(&content);

        let settings_button = header_button("emblem-system-symbolic", &tr!("Audio Settings"), None, false);
        let heading = popup_header(&["audio-volume-high-symbolic"], &tr!("Volume"), None, vec![HeaderAction::Widget(settings_button.clone().upcast())]);
        let output_icon = heading.icon.clone();
        let output_name = heading.subtitle_label.clone();
        output_name.set_max_width_chars(34);
        append(&content, &heading.root, false, false);
        // The device switcher sits below the header: choosing one sets the default.
        let output_combo = ChoiceCombo::new();
        output_combo.combo.set_tooltip_text(Some(&tr!("Default output device")));
        accessible(&output_combo.combo, &tr!("Default output device"));
        append(&content, &output_combo.combo, false, false);

        let volume_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let volume_heading = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        append(&volume_heading, &section_title(&tr!("Output volume")), true, false);
        let volume_label = gtk::Label::new(Some("0%"));
        volume_label.set_xalign(1.0);
        volume_label.add_css_class("volume-label");
        volume_label.set_width_chars(5);
        append(&volume_heading, &volume_label, false, false);
        append(&volume_box, &volume_heading, false, false);
        let volume_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 150.0, 1.0);
        volume_scale.set_draw_value(false);
        volume_scale.set_hexpand(true);
        volume_scale.set_has_origin(true);
        volume_scale.add_mark(100.0, gtk::PositionType::Bottom, Some("100%"));
        accessible(&volume_scale, &tr!("System output volume"));
        append(&content, &volume_box, false, false);

        // Reuse the settings mixer's guarded stream controls, with routing and
        // channel configuration left in the full settings window.
        let stream_scroller = gtk::ScrolledWindow::new();
        stream_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        stream_scroller.set_propagate_natural_height(true);
        stream_scroller.set_max_content_height(360);
        let sections = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let mut stream_sections = IndexMap::new();
        let input_combo = ChoiceCombo::new();
        for (key, title, empty_text) in [("playback", tr!("Applications"), tr!("No applications are playing audio")),
                                         ("input", tr!("Input"), tr!("No input devices"))] {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 6);
            section.append(&section_title(&title));
            if key == "input" {
                input_combo.combo.set_tooltip_text(Some(&tr!("Default input device")));
                accessible(&input_combo.combo, &tr!("Default input device"));
                section.append(&input_combo.combo);
            }
            let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
            section.append(&rows);
            let empty = gtk::Label::new(Some(&empty_text));
            empty.set_xalign(0.0);
            empty.add_css_class("muted");
            empty.set_wrap(true);
            section.append(&empty);
            stream_sections.insert(key, (rows, empty));
            sections.append(&section);
        }
        stream_scroller.set_child(Some(&sections));
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&stream_scroller);

        let mute_button = gtk::ToggleButton::new();
        mute_button.add_css_class("mute-button");
        mute_button.add_css_class("icon-button");
        let mute_icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
        mute_button.set_child(Some(&mute_icon));
        let output_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        output_controls.append(&mute_button);
        output_controls.append(&volume_scale);
        volume_box.append(&output_controls);

        let status_label = gtk::Label::new(None);
        status_label.set_xalign(0.0);
        status_label.add_css_class("popup-status");
        status_label.add_css_class("muted");
        status_label.set_wrap(true);
        status_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        status_label.set_max_width_chars(44);
        append(&content, &status_label, false, false);

        let this = Rc::new(Self {
            weak: RefCell::new(Weak::new()), backend, open_settings: Box::new(open_settings), syncing: Cell::new(false), dragging: Cell::new(false),
            volume_timeout: RefCell::new(None), pending_volume: RefCell::new(None), node_id: Cell::new(None), node_identity: Cell::new(None),
            error: RefCell::default(), destroyed: Cell::new(false), node_rows: RefCell::default(), stream_height_limit: Cell::new(360), window,
            css: RefCell::new(css), controller: RefCell::new(None), settings_button, output_icon, output_name, output_combo, input_combo, volume_label,
            volume_scale, stream_scroller, stream_sections, mute_button, mute_icon, status_label,
        });
        *this.weak.borrow_mut() = Rc::downgrade(&this);
        this.connect();
        let weak = Rc::downgrade(&this);
        let controller = PopupController::new(this.window.upcast_ref(), move || if let Some(p) = weak.upgrade() { p.refresh() }, edge, offset, "service_apps");
        *this.controller.borrow_mut() = Some(controller);
        let weak = Rc::downgrade(&this);
        this.window.connect_unrealize(move |_| if let Some(p) = weak.upgrade() { p.on_destroy() });
        this.refresh();
        this
    }

    fn connect(&self) {
        let weak = self.weak.borrow().clone();
        self.settings_button.connect_clicked(move |_| if let Some(p) = weak.upgrade() { p.dismiss(); (p.open_settings)() });
        let weak = self.weak.borrow().clone();
        self.output_combo.combo.connect_changed(move |_| if let Some(p) = weak.upgrade() { p.device_chosen("Audio/Sink") });
        let weak = self.weak.borrow().clone();
        self.input_combo.combo.connect_changed(move |_| if let Some(p) = weak.upgrade() { p.device_chosen("Audio/Source") });
        let weak = self.weak.borrow().clone();
        self.volume_scale.connect_value_changed(move |_| if let Some(p) = weak.upgrade() { p.volume_changed() });
        let (a, b) = (self.weak.borrow().clone(), self.weak.borrow().clone());
        track_drag(&self.volume_scale, move || if let Some(p) = a.upgrade() { p.dragging.set(true) },
                   move || if let Some(p) = b.upgrade() { p.scale_released() });
        let weak = self.weak.borrow().clone();
        self.mute_button.connect_toggled(move |_| if let Some(p) = weak.upgrade() { p.mute_toggled() });
    }

    fn view(&self) -> Weak<dyn View> {
        let weak: Weak<dyn View> = self.weak.borrow().clone();
        weak
    }

    fn default_output(&self) -> (Option<i64>, Option<Node>) {
        let state = self.backend.state();
        match state.default_output() {
            Some(node) => (Some(node.id), Some(node.clone())),
            None => (None, None),
        }
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        let (node_id, node) = self.default_output();
        let ready = self.backend.state().ready;
        let identity = node.as_ref().map(|n| (n.id, n.serial));
        if identity != self.node_identity.get() {
            self.cancel_volume();
            self.dragging.set(false);
            self.node_id.set(node_id);
            self.node_identity.set(identity);
        }
        let enabled = ready && node.as_ref().is_some_and(|n| n.volume_available);
        self.volume_scale.set_sensitive(enabled);
        self.mute_button.set_sensitive(enabled);
        self.syncing.set(true);
        self.refresh_device_choices();
        match &node {
            None => {
                self.output_name.set_text(&if ready { tr!("No output device") } else { tr!("Connecting to audio service…") });
                self.output_name.set_tooltip_text(None);
                self.volume_scale.set_value(0.0);
                self.volume_label.set_text("—");
                self.mute_button.set_active(false);
                self.set_mute_appearance(false, 0.0);
            }
            Some(node) => {
                self.output_name.set_text(&node.description);
                self.output_name.set_tooltip_text(Some(&node.description));
                if !self.dragging.get() && self.pending_volume.borrow().is_none() {
                    let volume = (node.volume * 100.0).clamp(0.0, 150.0);
                    self.volume_scale.set_value(volume);
                    self.volume_label.set_text(&percent(volume));
                }
                self.mute_button.set_active(node.mute);
                self.set_mute_appearance(node.mute, node.volume);
            }
        }
        self.syncing.set(false);
        self.refresh_streams();
        let own = self.error.borrow().clone();
        let mut message = if own.is_empty() { self.backend.state().error.clone() } else { own };
        let error = !message.is_empty();
        if message.is_empty() && ready && node.is_none() {
            message = tr!("Connect an audio device or choose an output in Audio Settings.");
        } else if message.is_empty() && ready && node.as_ref().is_some_and(|n| !n.volume_available) {
            message = tr!("This output does not expose software volume controls.");
        }
        self.status_label.set_text(&message);
        self.status_label.set_visible(!message.is_empty());
        if error { self.status_label.add_css_class("error") } else { self.status_label.remove_css_class("error") }
    }

    fn refresh_device_choices(&self) {
        let state = snapshot(&*self.backend);
        for (combo, media_class) in [(&self.output_combo, "Audio/Sink"), (&self.input_combo, "Audio/Source")] {
            let choices = device_choices(&state, media_class);
            let default = state.default_id(media_class).filter(|d| choices.iter().any(|c| c.0 == *d));
            combo.update(&choices, default);
            combo.combo.set_sensitive(state.ready && !choices.is_empty());
            combo.combo.set_visible(!choices.is_empty());
        }
        self.output_name.set_visible(!self.output_combo.combo.get_visible());
    }

    fn device_chosen(&self, media_class: &str) {
        if self.syncing.get() || self.destroyed.get() {
            return;
        }
        let combo = if media_class == "Audio/Sink" { &self.output_combo } else { &self.input_combo };
        let Some(id) = combo.choose_index() else { return };
        let (current, exists) = { let s = self.backend.state(); (s.default_id(media_class), s.nodes.contains_key(&id)) };
        if Some(id) == current {
            return;
        }
        if !exists {
            self.refresh();
            return;
        }
        self.perform(Action::SetDefault(id));
    }

    fn refresh_streams(&self) {
        let state = snapshot(&*self.backend);
        let mut playback = Vec::new();
        let mut input = Vec::new();
        for node in state.nodes.values().filter(|n| !n.loopback_internal) {
            if node.media_class == "Stream/Output/Audio" {
                playback.push(node.clone());
            } else if INPUT_CLASSES.contains(&node.media_class.as_str()) {
                input.push(node.clone());
            }
        }
        let live: IndexMap<i64, Node> = playback.iter().chain(&input).map(|n| (n.id, n.clone())).collect();
        self.stream_scroller.set_min_content_height(self.stream_height_limit.get().min(90 + 130 * live.len() as i32));
        let stale: Vec<i64> = self.node_rows.borrow().iter().filter(|(id, row)| {
            let old = row.node.borrow();
            live.get(*id).is_none_or(|n| (n.serial, &n.media_class) != (old.serial, &old.media_class))
        }).map(|(id, _)| *id).collect();
        for id in stale {
            if let Some(row) = self.node_rows.borrow_mut().shift_remove(&id) {
                row.destroyed();
                if let Some(parent) = row.root.parent().and_downcast::<gtk::Box>() {
                    parent.remove(&row.root);
                }
            }
        }
        let default_input = state.default_id("Audio/Source");
        for (key, mut nodes) in [("playback", playback), ("input", input)] {
            let (rows, empty) = &self.stream_sections[key];
            empty.set_visible(nodes.is_empty());
            nodes.sort_by_key(|n| {
                let name = if !n.application.is_empty() { &n.application } else if !n.description.is_empty() { &n.description } else { &n.name };
                (casefold(name), n.id)
            });
            let mut previous: Option<gtk::Widget> = None;
            for node in &nodes {
                let existing = self.node_rows.borrow().get(&node.id).cloned();
                let row = match existing {
                    Some(row) => {
                        row.refresh(node);
                        row
                    }
                    None => {
                        let row = NodeRow::new(self.view(), node, true);
                        row.name_label.set_max_width_chars(25);
                        row.detail_label.set_max_width_chars(32);
                        rows.append(&row.root);
                        self.node_rows.borrow_mut().insert(node.id, row.clone());
                        row
                    }
                };
                if key == "input" {
                    // Meter only the default input, and only while shown.
                    row.set_meter_enabled(Some(node.id) == default_input);
                }
                // Stable widgets keep slider drags and scroll position intact.
                if row.root.prev_sibling() != previous {
                    rows.reorder_child_after(&row.root, previous.as_ref());
                }
                previous = Some(row.root.clone().upcast());
            }
        }
    }

    fn set_mute_appearance(&self, mute: bool, volume: f64) {
        let name = mute_icon_name(mute, volume);
        self.output_icon.set_icon_name(Some(name));
        self.mute_icon.set_icon_name(Some(name));
        let action = if mute { tr!("Unmute output") } else { tr!("Mute output") };
        self.mute_button.set_tooltip_text(Some(&action));
        accessible(&self.mute_button, &action);
    }

    fn volume_changed(&self) {
        if self.syncing.get() || self.destroyed.get() || self.node_id.get().is_none() {
            return;
        }
        let value = self.volume_scale.value();
        self.volume_label.set_text(&percent(value));
        *self.pending_volume.borrow_mut() = Some((self.node_identity.get(), value / 100.0));
        if let Some(source) = self.volume_timeout.borrow_mut().take() {
            source.remove();
        }
        let weak = self.weak.borrow().clone();
        *self.volume_timeout.borrow_mut() = Some(glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
            if let Some(p) = weak.upgrade() {
                p.volume_timeout.borrow_mut().take();
                p.send_volume();
            }
        }));
    }

    fn send_volume(&self) {
        let pending = self.pending_volume.borrow_mut().take();
        let Some((identity, value)) = pending else { return };
        if self.destroyed.get() {
            return;
        }
        let (current_id, node) = self.default_output();
        let ready = self.backend.state().ready;
        let valid = node.as_ref().is_some_and(|n| identity == Some((n.id, n.serial)) && n.volume_available) && ready;
        if !valid {
            self.refresh();
            return;
        }
        self.error.borrow_mut().clear();
        if let Err(error) = self.backend.set_volume(current_id.unwrap(), value) {
            self.set_error(&error);
        }
    }

    fn scale_released(&self) {
        self.dragging.set(false);
        if let Some(source) = self.volume_timeout.borrow_mut().take() {
            source.remove();
        }
        self.send_volume();
    }

    fn mute_toggled(&self) {
        if self.syncing.get() || self.destroyed.get() {
            return;
        }
        let (node_id, node) = self.default_output();
        let ready = self.backend.state().ready;
        let Some(node) = node.filter(|n| ready && n.volume_available && self.node_identity.get() == Some((n.id, n.serial))) else {
            self.refresh();
            return;
        };
        self.error.borrow_mut().clear();
        let active = self.mute_button.is_active();
        self.set_mute_appearance(active, node.volume);
        if let Err(error) = self.backend.set_mute(node_id.unwrap(), active) {
            self.set_error(&error);
        }
    }

    fn cancel_volume(&self) {
        if let Some(source) = self.volume_timeout.borrow_mut().take() {
            source.remove();
        }
        *self.pending_volume.borrow_mut() = None;
    }

    fn controller(&self) -> Option<PopupController> {
        self.controller.borrow().clone()
    }

    pub fn show(&self) {
        if self.destroyed.get() {
            return;
        }
        let monitors = WidgetExt::display(&self.window).monitors();
        let heights: Vec<i32> = (0..monitors.n_items())
            .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>().map(|m| m.geometry().height())).collect();
        if let Some(height) = heights.into_iter().min() {
            // Remain usable on the smallest connected display; the shared
            // popup controller still chooses the pointer's actual output.
            self.stream_height_limit.set((height - 400).clamp(100, 420));
            self.stream_scroller.set_min_content_height(-1);
            self.stream_scroller.set_max_content_height(self.stream_height_limit.get());
            self.refresh_streams();
        }
        if let Some(controller) = self.controller() {
            controller.show();
        }
    }

    pub fn activate(&self) {
        if !self.destroyed.get() {
            if let Some(controller) = self.controller() {
                controller.activate();
            }
        }
    }

    pub fn dismiss(&self) {
        if !self.destroyed.get() {
            if let Some(controller) = self.controller() {
                controller.dismiss();
            }
        }
    }

    pub fn destroy(&self) {
        if !self.destroyed.get() {
            self.window.destroy();
        }
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed.get()
    }

    fn on_destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        self.cancel_volume();
        for row in self.node_rows.borrow().values() {
            row.destroyed();
        }
        self.node_rows.borrow_mut().clear();
        for provider in self.css.borrow_mut().drain(..) {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.window), &provider);
        }
    }
}

use tessel_gtk::gtk::gdk;
