//! NodeRow, DeviceRow, LoopbackRow and LoopbackDialog (audio_ui.py:178-767).

#![allow(deprecated)]

use super::common::*;
use crate::api::{Action, LevelMeter};
use crate::model::{target_available, Device, Loopback, Node};
use crate::timer::monotonic;
use gtk::prelude::*;
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::gtk;
use suite_gtk::utils::{accessible, append, children};

type Source = RefCell<Option<glib::SourceId>>;

fn cancel(source: &Source) {
    if let Some(id) = source.borrow_mut().take() {
        id.remove();
    }
}

pub struct NodeRow {
    weak: RefCell<Weak<NodeRow>>,
    view: Weak<dyn View>,
    pub root: gtk::Box,
    pub node: RefCell<Node>,
    pub compact: bool,
    updating: Cell<bool>,
    volume_source: Source,
    volume_pending: Cell<Option<(f64, f64)>>,
    channel_sources: RefCell<HashMap<usize, glib::SourceId>>,
    channel_pending: RefCell<HashMap<usize, (f64, f64)>>,
    channel_dragging: RefCell<HashSet<usize>>,
    dragging: Cell<bool>,
    pub image: gtk::Image,
    icon_names: RefCell<Vec<String>>,
    pub name_label: gtk::Label,
    pub detail_label: gtk::Label,
    pub mute_button: gtk::ToggleButton,
    pub mute_icon: gtk::Image,
    pub volume_scale: gtk::Scale,
    pub percent_label: gtk::Label,
    pub level_bar: Option<gtk::LevelBar>,
    meter: RefCell<Option<Box<dyn LevelMeter>>>,
    meter_target: Cell<Option<(i64, i64)>>,
    meter_failed: Cell<Option<(i64, i64)>>,
    meter_value: Cell<f64>,
    pub meter_enabled: Cell<bool>,
    pub target_combo: Option<ChoiceCombo>,
    pub targets_box: Option<gtk::Box>,
    pub route_combo: Option<ChoiceCombo>,
    pub default_button: Option<gtk::Button>,
    pub device_controls: Option<gtk::Box>,
    pub channels_controls: gtk::Box,
    pub channels_expander: gtk::Expander,
    pub channels_box: gtk::Box,
    pub channel_lock_button: gtk::ToggleButton,
    pub channel_lock_icon: gtk::Image,
    pub channel_scales: RefCell<IndexMap<usize, gtk::Scale>>,
    channels: RefCell<Option<Vec<String>>>,
    destroyed: Cell<bool>,
}

impl NodeRow {
    pub fn new(view: Weak<dyn View>, node: &Node, compact: bool) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 9);
        style(&root, &["card"]);
        root.set_margin_bottom(if compact { 6 } else { 12 });
        let content = gtk::Box::new(gtk::Orientation::Vertical, 9);
        margins(&content, if compact { 10 } else { 16 });
        append(&root, &content, true, false);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let image = icon(None, 24);
        style(&image, &["device-icon"]);
        append(&heading, &image, false, false);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let name_label = label("", Some("device-name"));
        let detail_label = label("", Some("muted"));
        append(&names, &name_label, false, false);
        append(&names, &detail_label, false, false);
        append(&heading, &names, true, false);
        let mute_button = gtk::ToggleButton::new();
        style(&mute_button, &["mute-button", "icon-button"]);
        let mute_icon = icon(Some("audio-volume-high-symbolic"), 18);
        mute_button.set_child(Some(&mute_icon));
        mute_button.set_valign(gtk::Align::Center);
        append(&heading, &mute_button, false, false);
        append(&content, &heading, false, false);

        let volumes = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let volume_scale = volume_scale();
        accessible(&volume_scale, &tr!("Volume"));
        let percent_label = gtk::Label::new(None);
        style(&percent_label, &["volume-label"]);
        append(&volumes, &volume_scale, true, false);
        append(&volumes, &percent_label, false, false);
        append(&content, &volumes, false, false);

        // Microphones get a peak meter that only captures while it is shown.
        let level_bar = INPUT_CLASSES.contains(&node.media_class.as_str()).then(|| {
            let bar = level_bar();
            bar.set_visible(!compact);
            append(&content, &bar, false, false);
            bar
        });

        let (mut target_combo, mut targets_box, mut route_combo, mut default_button, mut device_controls) = (None, None, None, None, None);
        if node.media_class.starts_with("Stream/") {
            let targets = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let text = if node.media_class == "Stream/Output/Audio" { tr!("Output") } else { tr!("Input") };
            append(&targets, &label(&text, Some("muted")), false, false);
            let combo = ChoiceCombo::new();
            accessible(&combo.combo, &tr!("Application audio device"));
            append(&targets, &combo.combo, true, false);
            append(&content, &targets, false, false);
            targets.set_visible(!compact);
            target_combo = Some(combo);
            targets_box = Some(targets);
        } else {
            let controls = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let combo = ChoiceCombo::new();
            accessible(&combo.combo, &tr!("Port"));
            append(&controls, &combo.combo, true, false);
            let button = gtk::Button::with_label(&tr!("Set as default"));
            append(&controls, &button, false, false);
            append(&content, &controls, false, false);
            controls.set_visible(!compact);
            route_combo = Some(combo);
            default_button = Some(button);
            device_controls = Some(controls);
        }

        let channels_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let channels_expander = gtk::Expander::new(Some(&tr!("Individual channels")));
        if let Some(widget) = channels_expander.label_widget() {
            widget.set_size_request(-1, 38);
        }
        let channels_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
        channels_expander.set_child(Some(&channels_box));
        let channel_lock_button = gtk::ToggleButton::new();
        style(&channel_lock_button, &["icon-button"]);
        let channel_lock_icon = icon(Some("changes-prevent-symbolic"), 18);
        channel_lock_button.set_child(Some(&channel_lock_icon));
        channel_lock_button.set_valign(gtk::Align::Start);
        accessible(&channel_lock_button, &tr!("Lock channels together"));
        channel_lock_button.set_active(true);
        append(&channels_controls, &channels_expander, true, false);
        append(&channels_controls, &channel_lock_button, false, false);
        append(&content, &channels_controls, false, false);

        let row = Rc::new(Self {
            weak: RefCell::new(Weak::new()), view, root, node: RefCell::new(node.clone()), compact, updating: Cell::new(true),
            volume_source: RefCell::new(None), volume_pending: Cell::new(None), channel_sources: RefCell::default(),
            channel_pending: RefCell::default(), channel_dragging: RefCell::default(), dragging: Cell::new(false), image,
            icon_names: RefCell::default(), name_label, detail_label, mute_button, mute_icon, volume_scale, percent_label, level_bar,
            meter: RefCell::new(None), meter_target: Cell::new(None), meter_failed: Cell::new(None), meter_value: Cell::new(0.0),
            meter_enabled: Cell::new(!compact), target_combo, targets_box, route_combo, default_button, device_controls,
            channels_controls, channels_expander, channels_box, channel_lock_button, channel_lock_icon,
            channel_scales: RefCell::default(), channels: RefCell::new(None), destroyed: Cell::new(false),
        });
        *row.weak.borrow_mut() = Rc::downgrade(&row);
        row.connect();
        row.channel_lock_changed();
        row.refresh(node);
        row
    }

    fn weak(&self) -> Weak<NodeRow> {
        self.weak.borrow().clone()
    }

    fn connect(&self) {
        macro_rules! on {
            ($body:expr) => {{
                let weak = self.weak();
                move |_: &_| if let Some(row) = weak.upgrade() { $body(&row) }
            }};
        }
        self.mute_button.connect_toggled(on!(|row: &Rc<NodeRow>| row.mute_changed()));
        self.volume_scale.connect_value_changed(on!(|row: &Rc<NodeRow>| row.volume_changed()));
        let (a, b) = (self.weak(), self.weak());
        track_drag(&self.volume_scale, move || if let Some(r) = a.upgrade() { r.dragging.set(true) },
                   move || if let Some(r) = b.upgrade() { r.dragging.set(false) });
        if let Some(bar) = &self.level_bar {
            bar.connect_map(on!(|row: &Rc<NodeRow>| row.update_meter()));
            bar.connect_unmap(on!(|row: &Rc<NodeRow>| row.update_meter()));
        }
        if let Some(combo) = &self.target_combo {
            combo.combo.connect_changed(on!(|row: &Rc<NodeRow>| row.target_changed()));
        }
        if let Some(combo) = &self.route_combo {
            combo.combo.connect_changed(on!(|row: &Rc<NodeRow>| row.route_changed()));
        }
        if let Some(button) = &self.default_button {
            button.connect_clicked(on!(|row: &Rc<NodeRow>| {
                let id = row.node.borrow().id;
                row.send(Action::SetDefault(id));
            }));
        }
        self.channel_lock_button.connect_toggled(on!(|row: &Rc<NodeRow>| row.channel_lock_changed()));
        self.root.connect_unrealize(on!(|row: &Rc<NodeRow>| row.destroyed()));
    }

    fn view(&self) -> Option<Rc<dyn View>> {
        self.view.upgrade()
    }

    fn send(&self, action: Action) {
        let Some(view) = self.view() else { return };
        let (id, serial) = { let n = self.node.borrow(); (n.id, n.serial) };
        let current = view.backend().state().nodes.get(&id).map(|n| n.serial);
        if current != Some(serial) {
            view.set_error(&tr!("This audio device is no longer available."));
            return;
        }
        view.perform(action);
    }

    fn mute_changed(&self) {
        if self.updating.get() {
            return;
        }
        let active = self.mute_button.is_active();
        let (id, volume) = { let n = self.node.borrow(); (n.id, n.volume) };
        self.set_mute_appearance(active, volume);
        self.send(Action::SetMute(id, active));
    }

    fn set_mute_appearance(&self, mute: bool, volume: f64) {
        self.mute_icon.set_icon_name(Some(mute_icon_name(mute, volume)));
        let action = if mute { tr!("Unmute") } else { tr!("Mute") };
        self.mute_button.set_tooltip_text(Some(&action));
        accessible(&self.mute_button, &action);
    }

    fn volume_changed(&self) {
        if self.updating.get() {
            return;
        }
        self.cancel_channel_edits();
        let value = self.volume_scale.value() / 100.0;
        self.percent_label.set_text(&percent(value * 100.0));
        self.volume_pending.set(Some((value, monotonic() + 1.5)));
        cancel(&self.volume_source);
        let weak = self.weak();
        *self.volume_source.borrow_mut() = Some(glib::timeout_add_local_once(std::time::Duration::from_millis(70), move || {
            if let Some(row) = weak.upgrade() {
                row.volume_source.borrow_mut().take();
                if let Some((value, _)) = row.volume_pending.get() {
                    let id = row.node.borrow().id;
                    row.send(Action::SetVolume(id, value));
                }
            }
        }));
    }

    fn target_changed(&self) {
        let Some(value) = self.target_combo.as_ref().and_then(|c| c.choose_index()) else { return };
        if !self.updating.get() {
            let id = self.node.borrow().id;
            self.send(Action::MoveStream(id, value));
        }
    }

    fn route_changed(&self) {
        let Some(value) = self.route_combo.as_ref().and_then(|c| c.choose_index()) else { return };
        if !self.updating.get() {
            let id = self.node.borrow().id;
            self.send(Action::SetRoute(id, value));
        }
    }

    fn channel_changed(&self, index: usize) {
        if self.updating.get() {
            return;
        }
        cancel(&self.volume_source);
        let Some(scale) = self.channel_scales.borrow().get(&index).cloned() else { return };
        let value = scale.value() / 100.0;
        let indices: Vec<usize> = if self.channel_lock_button.is_active() { self.channel_scales.borrow().keys().copied().collect() } else { vec![index] };
        let identity = { let n = self.node.borrow(); (n.id, n.serial, self.channels.borrow().clone().unwrap_or_default()) };
        let deadline = monotonic() + 1.5;
        self.updating.set(true);
        for channel in indices {
            let scale = self.channel_scales.borrow().get(&channel).cloned();
            if let Some(scale) = scale {
                scale.set_value(value * 100.0);
            }
            self.channel_pending.borrow_mut().insert(channel, (value, deadline));
            if let Some(source) = self.channel_sources.borrow_mut().remove(&channel) {
                source.remove();
            }
            let weak = self.weak();
            let identity = identity.clone();
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(70), move || {
                if let Some(row) = weak.upgrade() {
                    row.write_channel(channel, value, identity);
                }
            });
            self.channel_sources.borrow_mut().insert(channel, source);
        }
        let maximum = self.channel_scales.borrow().values().map(|s| s.value() / 100.0).reduce(f64::max).unwrap_or(0.0);
        self.volume_pending.set(Some((maximum, deadline)));
        self.volume_scale.set_value(maximum * 100.0);
        self.percent_label.set_text(&percent(maximum * 100.0));
        self.updating.set(false);
    }

    fn write_channel(&self, index: usize, value: f64, identity: (i64, i64, Vec<String>)) {
        self.channel_sources.borrow_mut().remove(&index);
        let Some(view) = self.view() else { return };
        let matches = view.backend().state().nodes.get(&identity.0).is_some_and(|n| (n.id, n.serial, &n.channels) == (identity.0, identity.1, &identity.2));
        if matches {
            self.send(Action::SetChannelVolume(identity.0, index as i64, value));
        }
    }

    fn channel_lock_changed(&self) {
        let locked = self.channel_lock_button.is_active();
        self.channel_lock_icon.set_icon_name(Some(if locked { "changes-prevent-symbolic" } else { "changes-allow-symbolic" }));
        self.channel_lock_button.set_tooltip_text(Some(&if locked { tr!("Unlock channels to adjust them separately") } else { tr!("Lock channels to adjust them together") }));
    }

    pub fn set_meter_enabled(&self, enabled: bool) {
        let Some(bar) = &self.level_bar else { return };
        self.meter_enabled.set(enabled);
        bar.set_visible(enabled);
        self.update_meter();
    }

    pub fn meter_active(&self) -> bool {
        self.meter.borrow().is_some()
    }

    fn update_meter(&self) {
        let Some(bar) = &self.level_bar else { return };
        let target = { let n = self.node.borrow(); (n.id, n.serial) };
        let Some(view) = self.view() else { return };
        let backend = view.backend();
        let wanted = self.meter_enabled.get() && bar.is_mapped() && backend.state().ready && backend.has_level_monitor();
        if self.meter.borrow().is_some() && (!wanted || self.meter_target.get() != Some(target)) {
            self.stop_meter();
        }
        if wanted && self.meter.borrow().is_none() && self.meter_failed.get() != Some(target) {
            let weak = self.weak();
            let meter = backend.level_monitor(target.0, Box::new(move |value| {
                if let Some(row) = weak.upgrade() {
                    row.meter_level(value);
                }
            }));
            let started = meter.is_some();
            *self.meter.borrow_mut() = meter;
            self.meter_target.set(Some(target));
            self.meter_failed.set(if started { None } else { Some(target) });
            bar.set_sensitive(started);
            bar.set_tooltip_text(Some(&if started { tr!("Input level") } else { tr!("The input level meter is unavailable.") }));
        }
    }

    fn meter_level(&self, value: Option<f64>) {
        if self.meter.borrow().is_none() {
            return;
        }
        // Fast attack, gentle release, like a hardware peak meter.
        self.meter_value.set(value.unwrap_or(0.0).max(self.meter_value.get() * 0.72));
        if let Some(bar) = &self.level_bar {
            bar.set_value(self.meter_value.get());
        }
    }

    fn stop_meter(&self) {
        let meter = self.meter.borrow_mut().take();
        if let Some(meter) = meter {
            meter.stop();
        }
        self.meter_value.set(0.0);
        if let Some(bar) = &self.level_bar {
            bar.set_value(0.0);
        }
    }

    pub fn level_value(&self) -> f64 {
        self.level_bar.as_ref().map_or(0.0, |b| b.value())
    }

    fn cancel_channel_edits(&self) {
        for (_, source) in self.channel_sources.borrow_mut().drain() {
            source.remove();
        }
        self.channel_pending.borrow_mut().clear();
        self.channel_dragging.borrow_mut().clear();
    }

    pub fn refresh(&self, node: &Node) {
        *self.node.borrow_mut() = node.clone();
        let Some(view) = self.view() else { return };
        let state = snapshot(&*view.backend());
        self.updating.set(true);
        let names = node_icon_names(node);
        if *self.icon_names.borrow() != names {
            // GTK re-resolves themed names when the icon theme changes.
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.image.set_from_gicon(&gio::ThemedIcon::from_names(&refs));
            *self.icon_names.borrow_mut() = names;
        }
        let stream = node.media_class.starts_with("Stream/");
        let or = |a: &str, b: &str| if a.is_empty() { b.to_owned() } else { a.to_owned() };
        let name = if stream { or(&or(&node.application, &node.description), &node.name) } else { or(&node.description, &node.name) };
        self.name_label.set_text(&name);
        self.name_label.set_tooltip_text(Some(&name));
        let detail = if stream { or(&node.media_name, &node.description) } else { node.name.clone() };
        self.detail_label.set_text(&detail);
        self.detail_label.set_tooltip_text(Some(&detail));
        self.mute_button.set_active(node.mute);
        self.set_mute_appearance(node.mute, node.volume);
        if let Some((value, deadline)) = self.volume_pending.get() {
            if (node.volume - value).abs() < 0.005 || monotonic() > deadline {
                self.volume_pending.set(None);
            }
        }
        if !self.dragging.get() && self.volume_pending.get().is_none() {
            self.volume_scale.set_value(node.volume * 100.0);
            self.percent_label.set_text(&percent(node.volume * 100.0));
        }

        if let Some(combo) = &self.target_combo {
            let media_class = if node.media_class == "Stream/Output/Audio" { "Audio/Sink" } else { "Audio/Source" };
            let recording = node.media_class == "Stream/Input/Audio";
            let mut targets: Vec<&Node> = state.nodes.values()
                .filter(|n| device_class(n) == media_class || (recording && n.media_class == "Audio/Sink")).collect();
            targets.sort_by_key(|n| (casefold(&n.description), n.id));
            let choices: Vec<(i64, String, bool)> = targets.iter().map(|n| {
                let name = or(&n.description, &n.name);
                (n.id, if recording && n.media_class == "Audio/Sink" { tr!("Monitor of {device}", device = name) } else { name }, true)
            }).collect();
            let selected = node.target_id.or_else(|| state.default_id(media_class));
            combo.update(&choices, selected);
            combo.combo.set_sensitive(!choices.is_empty());
        }
        if let (Some(button), Some(combo)) = (&self.default_button, &self.route_combo) {
            let is_default = state.default_id(device_class(node)) == Some(node.id);
            button.set_label(&if is_default { tr!("Default") } else { tr!("Set as default") });
            button.set_sensitive(!is_default);
            if is_default { button.add_css_class("primary") } else { button.remove_css_class("primary") }
            let routes: Vec<(i64, String, bool)> = node.routes.iter()
                .map(|r| (r.index, format!("{}{}", r.label, if r.available { "" } else { " (unavailable)" }), r.available)).collect();
            combo.update(&routes, node.route_index);
            combo.combo.set_sensitive(!node.routes.is_empty());
            combo.combo.set_visible(!node.routes.is_empty());
        }

        if self.channels.borrow().as_ref() != Some(&node.channels) {
            self.cancel_channel_edits();
            for child in children(&self.channels_box) {
                self.channels_box.remove(&child);
            }
            self.channel_scales.borrow_mut().clear();
            *self.channels.borrow_mut() = Some(node.channels.clone());
            for (i, channel) in node.channels.iter().enumerate() {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let channel_label = label(channel, Some("channel-label"));
                channel_label.set_size_request(70, -1);
                append(&row, &channel_label, false, false);
                let scale = volume_scale();
                accessible(&scale, &tr!("{value1} volume", value1 = channel.as_str()));
                let weak = self.weak();
                scale.connect_value_changed(move |_| if let Some(r) = weak.upgrade() { r.channel_changed(i) });
                let (a, b) = (self.weak(), self.weak());
                track_drag(&scale, move || if let Some(r) = a.upgrade() { r.channel_dragging.borrow_mut().insert(i); },
                           move || if let Some(r) = b.upgrade() { r.channel_dragging.borrow_mut().remove(&i); });
                append(&row, &scale, true, false);
                append(&self.channels_box, &row, false, false);
                self.channel_scales.borrow_mut().insert(i, scale);
            }
            self.channels_box.set_visible(true);
        }
        for (i, value) in node.channel_volumes.iter().enumerate() {
            let pending = self.channel_pending.borrow().get(&i).copied();
            if let Some((target, deadline)) = pending {
                if !self.channel_sources.borrow().contains_key(&i) && ((value - target).abs() < 0.005 || monotonic() > deadline) {
                    self.channel_pending.borrow_mut().remove(&i);
                }
            }
            let dragging = self.channel_dragging.borrow().contains(&i)
                || (!self.channel_dragging.borrow().is_empty() && self.channel_lock_button.is_active());
            let scale = self.channel_scales.borrow().get(&i).cloned();
            if let Some(scale) = scale {
                if !self.channel_pending.borrow().contains_key(&i) && !dragging {
                    scale.set_value(value * 100.0);
                }
            }
        }
        self.channels_controls.set_visible(!self.compact && node.channels.len() > 1);
        self.channel_lock_button.set_visible(true);
        self.channels_expander.set_visible(node.channels.len() > 1);
        self.root.set_sensitive(state.ready);
        let controllable = node.volume_available;
        self.volume_scale.set_sensitive(controllable);
        self.mute_button.set_sensitive(controllable);
        self.channels_expander.set_sensitive(controllable);
        self.channel_lock_button.set_sensitive(controllable);
        self.updating.set(false);
        self.update_meter();
    }

    pub fn destroyed(&self) {
        if self.destroyed.replace(true) {
            // unrealize may repeat; cleanup is idempotent anyway.
        }
        cancel(&self.volume_source);
        self.cancel_channel_edits();
        if self.level_bar.is_some() {
            self.stop_meter();
        }
    }

    /// Test hooks: whether a master/channel write is waiting.
    pub fn volume_pending(&self) -> bool {
        self.volume_source.borrow().is_some() || self.volume_pending.get().is_some()
    }
    pub fn channel_writes_pending(&self) -> usize {
        self.channel_sources.borrow().len()
    }
    pub fn set_dragging(&self, dragging: bool) {
        self.dragging.set(dragging);
    }
}

pub struct DeviceRow {
    weak: RefCell<Weak<DeviceRow>>,
    view: Weak<dyn View>,
    pub root: gtk::Box,
    pub device: RefCell<Device>,
    updating: Cell<bool>,
    pub name_label: gtk::Label,
    pub detail_label: gtk::Label,
    pub profile_combo: ChoiceCombo,
}

impl DeviceRow {
    pub fn new(view: Weak<dyn View>, device: &Device) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        style(&root, &["card"]);
        root.set_margin_bottom(12);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, 18);
        append(&root, &content, true, false);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let image = icon(Some("audio-card-symbolic"), 26);
        style(&image, &["device-icon"]);
        append(&heading, &image, false, false);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let name_label = label("", Some("device-name"));
        let detail_label = label("", Some("muted"));
        append(&names, &name_label, false, false);
        append(&names, &detail_label, false, false);
        append(&heading, &names, true, false);
        append(&content, &heading, false, false);
        let profiles = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        append(&profiles, &label(&tr!("Profile"), Some("muted")), false, false);
        let profile_combo = ChoiceCombo::new();
        accessible(&profile_combo.combo, &tr!("Device profile"));
        append(&profiles, &profile_combo.combo, true, false);
        append(&content, &profiles, false, false);
        let row = Rc::new(Self { weak: RefCell::new(Weak::new()), view, root, device: RefCell::new(device.clone()), updating: Cell::new(true), name_label, detail_label, profile_combo });
        *row.weak.borrow_mut() = Rc::downgrade(&row);
        let weak = Rc::downgrade(&row);
        row.profile_combo.combo.connect_changed(move |_| if let Some(r) = weak.upgrade() { r.profile_changed() });
        row.refresh(device);
        row
    }

    fn profile_changed(&self) {
        let Some(index) = self.profile_combo.choose_index() else { return };
        if self.updating.get() {
            return;
        }
        let Some(view) = self.view.upgrade() else { return };
        let (id, serial) = { let d = self.device.borrow(); (d.id, d.serial) };
        let current = view.backend().state().devices.get(&id).map(|d| d.serial);
        if current == Some(serial) {
            view.perform(Action::SetProfile(id, index));
        }
    }

    pub fn refresh(&self, device: &Device) {
        *self.device.borrow_mut() = device.clone();
        self.updating.set(true);
        let name = if device.description.is_empty() { device.name.clone() } else { device.description.clone() };
        self.name_label.set_text(&name);
        self.name_label.set_tooltip_text(Some(&name));
        self.detail_label.set_text(&device.name);
        let profiles: Vec<(i64, String, bool)> = device.profiles.iter()
            .map(|p| (p.index, format!("{}{}", p.label, if p.available { "" } else { " (unavailable)" }), p.available)).collect();
        self.profile_combo.update(&profiles, device.profile_index);
        let ready = self.view.upgrade().is_some_and(|v| v.backend().state().ready);
        self.profile_combo.combo.set_sensitive(!device.profiles.is_empty() && ready);
        self.updating.set(false);
    }
}

pub struct LoopbackRow {
    view: Weak<dyn View>,
    pub root: gtk::Box,
    pub loopback: RefCell<Loopback>,
    pub name_label: gtk::Label,
    pub target_label: gtk::Label,
    pub state_label: gtk::Label,
    pub remove_button: gtk::Button,
}

impl LoopbackRow {
    pub fn new(view: Weak<dyn View>, loopback: &Loopback) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        style(&root, &["card"]);
        root.set_margin_bottom(12);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        margins(&content, 16);
        append(&root, &content, true, false);
        let image = icon(Some("audio-card-symbolic"), 24);
        style(&image, &["device-icon"]);
        append(&content, &image, false, false);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let name_label = label("", Some("device-name"));
        let target_label = label("", Some("muted"));
        let state_label = label("", Some("muted"));
        append(&names, &name_label, false, false);
        append(&names, &target_label, false, false);
        append(&names, &state_label, false, false);
        append(&content, &names, true, false);
        let remove_button = gtk::Button::with_label(&tr!("Remove"));
        remove_button.set_valign(gtk::Align::Center);
        remove_button.set_tooltip_text(Some(&tr!("Remove this loopback and its saved configuration")));
        append(&content, &remove_button, false, false);
        let row = Rc::new(Self { view, root, loopback: RefCell::new(loopback.clone()), name_label, target_label, state_label, remove_button });
        let weak = Rc::downgrade(&row);
        row.remove_button.connect_clicked(move |_| {
            if let Some(row) = weak.upgrade() {
                let id = row.loopback.borrow().id.clone();
                if let Some(view) = row.view.upgrade() {
                    view.perform(Action::RemoveLoopback(id));
                }
            }
        });
        row.refresh(loopback);
        row
    }

    pub fn refresh(&self, loopback: &Loopback) {
        *self.loopback.borrow_mut() = loopback.clone();
        let Some(view) = self.view.upgrade() else { return };
        let state = snapshot(&*view.backend());
        self.name_label.set_text(&loopback.name);
        self.name_label.set_tooltip_text(Some(&loopback.name));
        self.target_label.set_text(&tr!("Output: {value1}", value1 = loopback.target_description.as_str()));
        self.target_label.set_tooltip_text(Some(&loopback.target_name));
        let target = state.nodes.values().find(|n| n.name == loopback.target_name && n.media_class == "Audio/Sink");
        let available = state.ready && target.is_some() && target_available(target);
        let active = available && state.nodes.values().any(|n| n.loopback_id.as_deref() == Some(loopback.id.as_str()) && !n.loopback_internal);
        let text = if active { tr!("Active · Saved across reboots") }
            else if available { tr!("Starting… · Saved across reboots") }
            else if state.ready { tr!("Disabled · Output device unavailable") }
            else { tr!("Disabled · Audio service unavailable") };
        self.state_label.set_text(&text);
        self.name_label.set_sensitive(available);
        self.remove_button.set_sensitive(true);
    }
}

pub struct LoopbackDialog {
    weak: RefCell<Weak<LoopbackDialog>>,
    view: Weak<dyn View>,
    pub dialog: gtk::Dialog,
    selection: RefCell<Option<(i64, String)>>,
    updating: Cell<bool>,
    pub name_entry: gtk::Entry,
    pub target_combo: ChoiceCombo,
    pub error_label: gtk::Label,
}

impl LoopbackDialog {
    pub fn new(view: Weak<dyn View>, parent: &gtk::Window) -> Rc<Self> {
        let dialog = gtk::Dialog::new();
        dialog.set_title(Some(&tr!("Add Loopback")));
        dialog.set_transient_for(Some(parent));
        dialog.set_modal(true);
        dialog.set_destroy_with_parent(true);
        dialog.set_default_size(460, -1);
        dialog.set_resizable(false);
        style(&dialog, &["audiomanager"]);
        dialog.add_button(&tr!("Cancel"), gtk::ResponseType::Cancel);
        dialog.add_button(&tr!("Add Loopback"), gtk::ResponseType::Ok);
        dialog.set_default_response(gtk::ResponseType::Ok);
        if let Some(button) = dialog.widget_for_response(gtk::ResponseType::Ok) {
            style(&button, &["primary"]);
        }
        let content = dialog.content_area();
        content.set_spacing(12);
        margins(&content, 20);
        let explanation = label(&tr!("Create a virtual output that forwards audio to a device."), Some("muted"));
        explanation.set_ellipsize(gtk::pango::EllipsizeMode::None);
        explanation.set_wrap(true);
        append(&content, &explanation, false, false);
        append(&content, &label(&tr!("Name"), Some("device-name")), false, false);
        let name_entry = gtk::Entry::new();
        name_entry.set_placeholder_text(Some(&tr!("For example, Headset Loopback")));
        name_entry.set_max_length(80);
        name_entry.set_activates_default(true);
        append(&content, &name_entry, false, false);
        append(&content, &label(&tr!("Output device"), Some("device-name")), false, false);
        let target_combo = ChoiceCombo::new();
        accessible(&target_combo.combo, &tr!("Loopback output device"));
        append(&content, &target_combo.combo, false, false);
        let note = label(&tr!("Saved across reboots. Disabled while the selected device is unavailable; reconnects automatically when it returns."), Some("muted"));
        note.set_ellipsize(gtk::pango::EllipsizeMode::None);
        note.set_wrap(true);
        append(&content, &note, false, false);
        let error_label = label("", Some("error"));
        error_label.set_wrap(true);
        append(&content, &error_label, false, false);
        let this = Rc::new(Self { weak: RefCell::new(Weak::new()), view, dialog, selection: RefCell::new(None), updating: Cell::new(false), name_entry, target_combo, error_label });
        *this.weak.borrow_mut() = Rc::downgrade(&this);
        let weak = Rc::downgrade(&this);
        this.name_entry.connect_changed(move |_| if let Some(d) = weak.upgrade() { d.validate() });
        let weak = Rc::downgrade(&this);
        this.target_combo.combo.connect_changed(move |_| if let Some(d) = weak.upgrade() { d.target_changed() });
        let weak = Rc::downgrade(&this);
        this.dialog.connect_response(move |_, response| if let Some(d) = weak.upgrade() { d.response(response) });
        this.refresh();
        this
    }

    fn targets(state: &crate::api::AudioState) -> Vec<Node> {
        state.nodes.values().filter(|n| n.media_class == "Audio/Sink" && n.loopback_id.is_none()).cloned().collect()
    }

    fn selected_node(&self) -> Option<Node> {
        let serial = self.target_combo.choose_index()?;
        let view = self.view.upgrade()?;
        let state = snapshot(&*view.backend());
        Self::targets(&state).into_iter().find(|n| n.serial == serial)
    }

    fn target_changed(&self) {
        if self.updating.get() {
            return;
        }
        let node = self.selected_node();
        *self.selection.borrow_mut() = node.map(|n| (n.serial, if n.description.is_empty() { n.name } else { n.description }));
        self.validate();
    }

    fn validate(&self) {
        let ready = self.view.upgrade().is_some_and(|v| v.backend().state().ready);
        let node = self.selected_node();
        let enabled = !crate::pyjson::py_strip(&self.name_entry.text()).is_empty() && ready && node.is_some() && target_available(node.as_ref());
        self.dialog.set_response_sensitive(gtk::ResponseType::Ok, enabled);
    }

    pub fn refresh(&self) {
        let Some(view) = self.view.upgrade() else { return };
        let state = snapshot(&*view.backend());
        self.updating.set(true);
        let mut nodes = Self::targets(&state);
        nodes.sort_by_key(|n| (casefold(&n.description), n.serial));
        let name = |n: &Node| if n.description.is_empty() { n.name.clone() } else { n.description.clone() };
        if self.selection.borrow().is_none() {
            let available: Vec<&Node> = nodes.iter().filter(|n| target_available(Some(n))).collect();
            let default = available.iter().find(|n| Some(n.id) == state.default_id("Audio/Sink")).or(available.first());
            if let Some(default) = default {
                *self.selection.borrow_mut() = Some((default.serial, name(default)));
            }
        }
        let mut choices: Vec<(i64, String, bool)> = nodes.iter()
            .map(|n| (n.serial, format!("{}{}", name(n), if target_available(Some(n)) { "" } else { " (unavailable)" }), target_available(Some(n)))).collect();
        let selection = self.selection.borrow().clone();
        if let Some((serial, text)) = &selection {
            if !nodes.iter().any(|n| n.serial == *serial) {
                choices.push((*serial, format!("{text} (unavailable)"), false));
            }
        }
        self.target_combo.update(&choices, selection.map(|s| s.0));
        self.updating.set(false);
        self.validate();
    }

    pub fn response(&self, response: gtk::ResponseType) {
        if response != gtk::ResponseType::Ok {
            self.dialog.destroy();
            return;
        }
        let Some(view) = self.view.upgrade() else { return };
        let backend = view.backend();
        let node = self.selected_node();
        let ready = backend.state().ready;
        let Some(node) = node.filter(|n| target_available(Some(n)) && ready) else {
            self.error_label.set_text(&tr!("The selected output is no longer available."));
            self.refresh();
            return;
        };
        let name = crate::pyjson::py_strip(&self.name_entry.text()).to_owned();
        match backend.add_loopback(&name, node.id, node.serial) {
            Err(error) => self.error_label.set_text(&error),
            Ok(false) => {
                let error = backend.state().error.clone();
                self.error_label.set_text(&if error.is_empty() { tr!("Could not add this loopback.") } else { error });
            }
            Ok(true) => self.dialog.destroy(),
        }
    }
}
