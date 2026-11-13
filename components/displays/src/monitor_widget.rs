//! `MonitorWidget`: one notebook tab (Display card, brightness, Color card)
//! (tessel-displays:1000-1377).

use crate::brightness::BrightnessControls;
use crate::color_controls::ColorControls;
use crate::compat::brightness_client::BrightnessModel;
use crate::daemon::config::VrrMode;
use crate::layout::MonitorLayout;
use crate::model::DesiredState;
use crate::window::TesselDisplaysApp;
use gtk::prelude::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use tessel_core::pyfmt::float_repr;
use tessel_core::pyjson::{py_str, truthy};
use tessel_core::tr;

pub const ROTATIONS: [(&str, &str); 4] = [
    ("STANDARD", "view-fullscreen-symbolic"),
    ("90", "object-rotate-right-symbolic"),
    ("180", "view-refresh-symbolic"),
    ("270", "object-rotate-left-symbolic"),
];

pub struct MonitorWidget {
    pub root: gtk::Box,
    pub monitor: RefCell<Value>,
    app: Weak<TesselDisplaysApp>,
    pub layout_area: RefCell<Option<Rc<MonitorLayout>>>,
    pub initial_enabled: Cell<bool>,
    pub initial_hdr: Cell<bool>,
    pub hdr_available: bool,
    /// The compositor's renderer can show HDR (`color_config::renderer_supports_color`).
    pub hdr_renderer: bool,
    pub initial_vrr: Cell<VrrMode>,
    pub initial_scale_auto: bool,
    pub enable_switch: gtk::Switch,
    pub primary_switch: gtk::Switch,
    pub res_combo: gtk::DropDown,
    pub scale_slider: gtk::Scale,
    pub scale_auto_switch: gtk::Switch,
    pub rotation_buttons: Vec<(&'static str, gtk::ToggleButton)>,
    rotation_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    pub flip_switch: gtk::Switch,
    /// Adaptive sync: Automatic, Always, Off (`VRR_MODES` order).
    pub vrr_combo: gtk::DropDown,
    /// The dim line under the Adaptive Sync row.
    pub vrr_hint: gtk::Label,
    pub vrr_capable: Cell<bool>,
    pub hdr_switch: gtk::Switch,
    pub brightness_controls: Rc<BrightnessControls>,
    pub color_controls: Rc<ColorControls>,
}

/// The Adaptive Sync drop-down's items, in order.
pub const VRR_MODES: [VrrMode; 3] = [VrrMode::Auto, VrrMode::Always, VrrMode::Off];

/// `strftime`-free Python `f"{x:g}"` for the scale marks/values.
pub fn general(value: f64) -> String {
    tessel_core::pyfmt::format_value(&tessel_core::pyfmt::Arg::Float(value), "g").unwrap_or_else(|_| float_repr(value))
}

fn labels(title: &str, detail: Option<&str>) -> gtk::Box {
    let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
    labels.append(&gtk::Label::builder().label(title).xalign(0.0).build());
    if let Some(detail) = detail.filter(|d| !d.is_empty()) {
        let hint = gtk::Label::builder().label(detail).xalign(0.0).wrap(true).build();
        hint.add_css_class("dim-label");
        labels.append(&hint);
    }
    labels
}

fn card(parent: &gtk::Box, title: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("settings-card");
    let heading = gtk::Label::builder().label(title).xalign(0.0).build();
    heading.add_css_class("heading");
    card.append(&heading);
    parent.append(&card);
    card
}

fn row(card: &gtk::Box, title: &str, control: &impl IsA<gtk::Widget>, detail: Option<&str>, expand: bool) -> gtk::Box {
    let row = gtk::Box::builder().spacing(16).build();
    row.add_css_class("settings-row");
    let labels = labels(title, detail);
    labels.set_hexpand(!expand);
    labels.set_size_request(if expand { 180 } else { -1 }, -1);
    row.append(&labels);
    control.set_valign(gtk::Align::Center);
    control.set_halign(if expand { gtk::Align::Fill } else { gtk::Align::End });
    row.append(control);
    card.append(&row);
    row
}

/// Tab / title text: `"{make} {model} ({name})"` or `Unknown ({name})`.
pub fn monitor_title(monitor: &Value) -> String {
    let text = |key: &str| monitor.get(key).filter(|v| truthy(v)).map(py_str);
    match (text("MAKE"), text("MODEL")) {
        (Some(make), Some(model)) => format!("{make} {model} ({})", monitor.get("NAME").map(py_str).unwrap_or_else(|| "None".into())),
        _ => tr!("Unknown ({value1})", value1 = monitor.get("NAME").map(py_str).unwrap_or_else(|| "None".into())),
    }
}

/// `f"{width}x{height} @ {refresh:.2f}Hz"`.
pub fn mode_label(mode: &Value) -> String {
    let refresh = mode.get("REFRESH_MHZ").and_then(Value::as_f64).unwrap_or(60000.0) / 1000.0;
    format!("{}x{} @ {refresh:.2}Hz", mode.get("WIDTH").map(py_str).unwrap_or_else(|| "None".into()),
            mode.get("HEIGHT").map(py_str).unwrap_or_else(|| "None".into()))
}

impl MonitorWidget {
    #[allow(clippy::too_many_arguments)]
    pub fn new(monitor: Value, app: Weak<TesselDisplaysApp>, primary: bool, hdr: bool, hdr_available: bool,
               layout_area: Option<Rc<MonitorLayout>>, brightness_client: Option<Rc<dyn BrightnessModel>>,
               brightness_error: &str, color_config_path: PathBuf, scale_auto: bool) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let name = monitor.get("NAME").map(py_str).unwrap_or_default();
        let empty = json!({});
        let current = monitor.get("CURRENT").unwrap_or(&empty).clone();

        let title = gtk::Label::builder().label(monitor_title(&monitor)).xalign(0.0).build();
        title.add_css_class("monitor-title");
        root.append(&title);
        let display_card = card(&root, &tr!("Display"));
        let brightness_controls = BrightnessControls::new(&name, brightness_client, brightness_error);
        root.append(&brightness_controls.root);
        let color_card = card(&root, &tr!("Color"));

        let enabled = current.get("ENABLED").is_some_and(truthy);
        let enable_switch = gtk::Switch::new();
        enable_switch.set_active(enabled);
        let primary_switch = gtk::Switch::new();
        primary_switch.set_active(primary);
        primary_switch.set_sensitive(enabled);
        primary_switch.set_tooltip_text(Some(&tr!("Sets the XRandR primary output for XWayland and configures the SDL, Wine, and Proton Wayland monitor preference for applications launched after Apply. Wayland itself has no global primary output.")));

        let res_combo = gtk::DropDown::builder().build();
        let model = gtk::StringList::new(&[]);
        let modes = monitor.get("MODES").and_then(Value::as_array).cloned().unwrap_or_default();
        let current_mode = current.get("MODE").cloned().unwrap_or_else(|| json!({}));
        let mut current_index = 0;
        for (i, mode) in modes.iter().enumerate() {
            model.append(&mode_label(mode));
            if mode.get("WIDTH") == current_mode.get("WIDTH") && mode.get("HEIGHT") == current_mode.get("HEIGHT")
                && mode.get("REFRESH_MHZ") == current_mode.get("REFRESH_MHZ") {
                current_index = i as u32;
            }
        }
        res_combo.set_model(Some(&model));
        res_combo.set_selected(current_index);

        let scale_value = current.get("SCALE").and_then(Value::as_f64).unwrap_or(1.0);
        let adjustment = gtk::Adjustment::new(scale_value, 1.0, 2.5, 0.125, 0.125, 0.0);
        let scale_slider = gtk::Scale::new(gtk::Orientation::Horizontal, Some(&adjustment));
        scale_slider.add_css_class("scale-slider");
        scale_slider.set_draw_value(true);
        scale_slider.set_value_pos(gtk::PositionType::Right);
        scale_slider.set_format_value_func(|_, value| format!("{}×", general(value)));
        scale_slider.set_hexpand(true);
        scale_slider.set_size_request(280, -1);
        for mark in [1.0, 1.5, 2.0, 2.5] {
            scale_slider.add_mark(mark, gtk::PositionType::Bottom, Some(&format!("{}×", general(mark))));
        }
        let scale_auto_switch = gtk::Switch::builder().active(scale_auto).build();

        let rotation_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        rotation_box.add_css_class("linked");
        let mut rotation_buttons = Vec::new();
        for (rotation, icon) in ROTATIONS {
            let button = gtk::ToggleButton::new();
            button.set_child(Some(&gtk::Image::from_icon_name(icon)));
            button.set_tooltip_text(Some(&if rotation != "STANDARD" { tr!("Rotate {value1}°", value1 = rotation) } else { tr!("No rotation") }));
            button.add_css_class("orientation-button");
            rotation_box.append(&button);
            rotation_buttons.push((rotation, button));
        }
        let flip_switch = gtk::Switch::new();
        let vrr_mode = crate::model::vrr_mode_of(&monitor, &crate::ipc::cfg(), &crate::model::state_heads(&crate::ipc::config_data()));
        let vrr_capable = crate::model::vrr_capable_of(&monitor);
        let vrr_combo = gtk::DropDown::from_strings(&[&tr!("Automatic"), &tr!("Always"), &tr!("Off")]);
        vrr_combo.set_selected(VRR_MODES.iter().position(|mode| *mode == vrr_mode).unwrap_or(0) as u32);
        let vrr_hint = gtk::Label::builder().xalign(0.0).wrap(true).build();
        vrr_hint.add_css_class("dim-label");
        let hdr_switch = gtk::Switch::new();
        hdr_switch.set_active(hdr);
        // Without the Vulkan renderer labwc refuses HDR: the switch can only
        // turn a saved HDR setting off, and says why.
        let hdr_renderer = crate::color_config::renderer_supports_color(&crate::color_config::read_status());
        hdr_switch.set_sensitive(enabled && hdr_available && (hdr_renderer || hdr));
        hdr_switch.set_tooltip_text(Some(&if hdr_renderer {
            tr!("Requests HDR for this output through Labwc. Unsupported outputs remain in SDR. The Vulkan renderer is required.")
        } else {
            tr!(crate::color_config::NEEDS_VULKAN)
        }));

        let app_for_color = app.clone();
        let color_controls = ColorControls::new(&name, color_config_path, Some(Rc::new(move || {
            if let Some(app) = app_for_color.upgrade() {
                app.on_monitor_toggle_changed();
            }
        })));

        let this = Rc::new(Self {
            root, monitor: RefCell::new(monitor), app, layout_area: RefCell::new(layout_area), initial_enabled: Cell::new(enabled),
            initial_hdr: Cell::new(hdr), hdr_available, hdr_renderer, initial_vrr: Cell::new(vrr_mode), initial_scale_auto: scale_auto,
            enable_switch, primary_switch, res_combo, scale_slider, scale_auto_switch, rotation_buttons,
            rotation_handlers: RefCell::new(Vec::new()), flip_switch, vrr_combo, vrr_hint, vrr_capable: Cell::new(vrr_capable), hdr_switch,
            brightness_controls, color_controls,
        });
        let apply = |this: &Rc<Self>| {
            let weak = Rc::downgrade(this);
            move || {
                if let Some(this) = weak.upgrade() {
                    if let Some(app) = this.app.upgrade() {
                        app.on_monitor_toggle_changed();
                    }
                }
            }
        };

        // Signal order follows the Python constructor.
        let weak = Rc::downgrade(&this);
        this.enable_switch.connect_active_notify(move |switch| {
            if let Some(this) = weak.upgrade() {
                this.on_enabled_toggled(switch.is_active());
            }
        });
        let a = apply(&this);
        this.enable_switch.connect_active_notify(move |_| a());
        row(&display_card, &tr!("Enabled"), &this.enable_switch, None, false);
        let weak = Rc::downgrade(&this);
        this.primary_switch.connect_active_notify(move |switch| {
            if let Some(this) = weak.upgrade() {
                if let Some(app) = this.app.upgrade() {
                    app.on_primary_toggled(&this, switch.is_active());
                }
            }
        });
        let a = apply(&this);
        this.primary_switch.connect_active_notify(move |_| a());
        row(&display_card, &tr!("Primary"), &this.primary_switch, None, false);

        let weak = Rc::downgrade(&this);
        this.res_combo.connect_selected_notify(move |dropdown| {
            if let Some(this) = weak.upgrade() {
                this.on_resolution_changed(dropdown.selected());
            }
        });
        let a = apply(&this);
        this.res_combo.connect_selected_notify(move |_| a());
        row(&display_card, &tr!("Resolution"), &this.res_combo, None, false);

        this.scale_slider.connect_value_changed(|scale| {
            let value = scale.value();
            let snapped = (value * 8.0).round() / 8.0;
            println!("{}", float_repr(snapped));
            if (snapped - value).abs() > 0.0001 {
                scale.set_value(snapped);
            }
        });
        let a = apply(&this);
        this.scale_slider.connect_value_changed(move |_| a());
        let slider = this.scale_slider.clone();
        this.scale_auto_switch.connect_active_notify(move |switch| slider.set_sensitive(!switch.is_active()));
        let a = apply(&this);
        this.scale_auto_switch.connect_active_notify(move |_| a());
        this.scale_slider.set_sensitive(!scale_auto);
        row(&display_card, &tr!("Automatic scaling"), &this.scale_auto_switch,
            Some(&tr!("Choose the scale from this screen's size and resolution.")), false);
        row(&display_card, &tr!("Scale"), &this.scale_slider, None, true);

        let mut handlers = Vec::new();
        for (rotation, button) in &this.rotation_buttons {
            let weak = Rc::downgrade(&this);
            let rotation = *rotation;
            handlers.push(button.connect_toggled(move |button| {
                if let Some(this) = weak.upgrade() {
                    this.on_rotation_selected(button, rotation);
                }
            }));
            let a = apply(&this);
            button.connect_active_notify(move |_| a());
        }
        this.rotation_handlers.replace(handlers);
        this.rotation_buttons[0].1.set_active(true);
        row(&display_card, &tr!("Orientation"), &rotation_box, None, false);

        let weak = Rc::downgrade(&this);
        this.flip_switch.connect_active_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.transform_changed();
            }
        });
        let a = apply(&this);
        this.flip_switch.connect_active_notify(move |_| a());
        row(&display_card, &tr!("Flip"), &this.flip_switch, None, false);

        if let Some(transform) = current.get("TRANSFORM").filter(|t| truthy(t)) {
            let text = py_str(transform);
            let rot = if text.to_uppercase().contains("FLIPPED") {
                this.flip_switch.set_active(true);
                if text.contains('-') { text.split('-').nth(1).unwrap_or("").to_uppercase() } else { "STANDARD".into() }
            } else {
                text.to_uppercase()
            };
            let key = if matches!(rot.as_str(), "90" | "180" | "270") { rot.as_str() } else { "STANDARD" };
            if let Some((_, button)) = this.rotation_buttons.iter().find(|(r, _)| *r == key) {
                button.set_active(true);
            }
        }
        this.transform_changed();

        let weak = Rc::downgrade(&this);
        this.vrr_combo.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.update_vrr_hint();
            }
        });
        let a = apply(&this);
        this.vrr_combo.connect_selected_notify(move |_| a());
        let vrr_row = row(&color_card, &tr!("Adaptive Sync (VRR)"), &this.vrr_combo, None, false);
        if let Some(labels) = vrr_row.first_child().and_downcast::<gtk::Box>() {
            labels.append(&this.vrr_hint);
        }
        this.update_vrr_hint();
        let a = apply(&this);
        this.hdr_switch.connect_active_notify(move |_| a());
        row(&color_card, &tr!("HDR"), &this.hdr_switch, None, false);
        this.color_controls.hdr.set(hdr);
        this.color_controls.refresh();
        let weak = Rc::downgrade(&this);
        this.hdr_switch.connect_active_notify(move |switch| {
            if let Some(this) = weak.upgrade() {
                this.color_controls.hdr.set(switch.is_active());
                this.color_controls.refresh();
            }
        });
        color_card.append(&this.color_controls.root);
        this
    }

    pub fn vrr_mode(&self) -> VrrMode {
        VRR_MODES.get(self.vrr_combo.selected() as usize).copied().unwrap_or_default()
    }

    pub fn set_vrr_mode(&self, mode: VrrMode) {
        self.vrr_combo.set_selected(VRR_MODES.iter().position(|m| *m == mode).unwrap_or(0) as u32);
    }

    /// A screen the compositor refused adaptive sync on cannot be set.
    pub fn set_vrr_capable(&self, capable: bool) {
        self.vrr_capable.set(capable);
        self.update_vrr_hint();
    }

    fn update_vrr_hint(&self) {
        let capable = self.vrr_capable.get();
        self.vrr_combo.set_sensitive(capable);
        let hint = if !capable {
            tr!("Adaptive Sync is not available on this screen.")
        } else if self.vrr_mode() == VrrMode::Auto {
            tr!("On only while an app is fullscreen on this screen.")
        } else {
            String::new()
        };
        self.vrr_hint.set_visible(!hint.is_empty());
        self.vrr_hint.set_text(&hint);
    }

    pub fn desc(&self) -> String {
        self.monitor.borrow().get("DESCRIPTION").map(py_str).unwrap_or_default()
    }

    pub fn name(&self) -> Option<String> {
        self.monitor.borrow().get("NAME").filter(|v| !v.is_null()).map(py_str)
    }

    fn transform_changed(&self) {
        if let Some(app) = self.app.upgrade() {
            app.on_transform_change(&self.desc(), self.get_transform().as_deref());
        }
    }

    fn on_resolution_changed(&self, selected: u32) {
        let modes = self.monitor.borrow().get("MODES").and_then(Value::as_array).cloned().unwrap_or_default();
        let Some(mode) = modes.get(selected as usize) else { return };
        let width = mode.get("WIDTH").cloned().unwrap_or(json!(1920));
        let height = mode.get("HEIGHT").cloned().unwrap_or(json!(1080));
        let refresh_mhz = mode.get("REFRESH_MHZ").cloned().unwrap_or(json!(60000));
        let refresh_hz = refresh_mhz.as_f64().unwrap_or(60000.0) / 1000.0;
        {
            let mut monitor = self.monitor.borrow_mut();
            if !monitor.get("CURRENT").is_some_and(Value::is_object) {
                monitor["CURRENT"] = json!({});
            }
            monitor["CURRENT"]["MODE"] = json!({"WIDTH": width, "HEIGHT": height, "REFRESH_MHZ": refresh_mhz});
        }
        let layout = self.layout_area.borrow().clone();
        if let Some(layout) = layout {
            let desc = self.desc();
            if let Some(shape) = layout.shape(&desc) {
                let (mut w, mut h, mut position) = (0, 0, (0, 0));
                layout.with_meta(&desc, |meta| {
                    let transform = meta.transform.clone();
                    meta.set_mode(width.as_i64().unwrap_or(0), height.as_i64().unwrap_or(0), &transform);
                    (w, h, position) = (meta.width, meta.height, (meta.x, meta.y));
                });
                if shape.rotatable.parent().is_some_and(|p| p == *layout.fixed.upcast_ref::<gtk::Widget>()) {
                    layout.fixed.move_(&shape.rotatable, position.0 as f64, position.1 as f64);
                    shape.rotatable.set_size_request(w as i32, h as i32);
                }
                shape.label.set_text(&tr!("{value1}\n{value2}x{value3} @ {value4:.0f}Hz", value1 = desc.as_str(),
                    value2 = py_str(&width), value3 = py_str(&height), value4 = refresh_hz));
            }
            layout.snap_monitors_to_edges();
        }
    }

    fn on_enabled_toggled(&self, enabled: bool) {
        self.primary_switch.set_sensitive(enabled);
        self.hdr_switch.set_sensitive(enabled && self.hdr_available && (self.hdr_renderer || self.hdr_switch.is_active()));
        if !enabled && self.primary_switch.is_active() {
            self.primary_switch.set_active(false);
        }
        {
            let mut monitor = self.monitor.borrow_mut();
            if !monitor.get("CURRENT").is_some_and(Value::is_object) {
                monitor["CURRENT"] = json!({});
            }
            monitor["CURRENT"]["ENABLED"] = Value::Bool(enabled);
        }
        if let Some(app) = self.app.upgrade() {
            let monitor = self.monitor.borrow().clone();
            app.on_monitor_enabled_toggled(&monitor, enabled);
        }
    }

    fn on_rotation_selected(&self, button: &gtk::ToggleButton, _rotation: &str) {
        if !button.is_active() {
            return;
        }
        let handlers = self.rotation_handlers.borrow();
        for ((_, other), handler) in self.rotation_buttons.iter().zip(handlers.iter()) {
            if other != button {
                other.block_signal(handler);
                other.set_active(false);
                other.unblock_signal(handler);
            }
        }
        drop(handlers);
        self.transform_changed();
    }

    /// `"90"`, `"FLIPPED-180"`, `"STANDARD"`…; `None` when no rotation button
    /// is active (flipped then gives Python's `"FLIPPED-None"`).
    pub fn get_transform(&self) -> Option<String> {
        let rotation = self.rotation_buttons.iter().find(|(_, b)| b.is_active()).map(|(r, _)| *r);
        if self.flip_switch.is_active() {
            return Some(format!("FLIPPED-{}", rotation.unwrap_or("None")));
        }
        rotation.map(String::from)
    }

    pub fn get_desired_state(&self) -> Option<DesiredState> {
        let selected = self.res_combo.selected();
        let monitor = self.monitor.borrow();
        let modes = monitor.get("MODES").and_then(Value::as_array)?;
        let mode = modes.get(selected as usize)?;
        Some(DesiredState {
            desc: monitor.get("DESCRIPTION").map(py_str).unwrap_or_default(),
            name: monitor.get("NAME").filter(|v| !v.is_null()).map(py_str),
            width: mode["WIDTH"].clone(),
            height: mode["HEIGHT"].clone(),
            refresh: mode["REFRESH_MHZ"].clone(),
            enabled: self.enable_switch.is_active(),
            enabled_changed: self.enable_switch.is_active() != self.initial_enabled.get(),
            primary: self.primary_switch.is_active(),
            vrr: self.vrr_mode(),
            vrr_changed: self.vrr_mode() != self.initial_vrr.get(),
            hdr: self.hdr_switch.is_active(),
            hdr_changed: self.hdr_switch.is_active() != self.initial_hdr.get(),
            scale: self.scale_slider.value(),
            scale_auto: self.scale_auto_switch.is_active(),
            transform: self.get_transform(),
        })
    }
}
