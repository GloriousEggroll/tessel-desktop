//! Shared widgets and CSS of audio_ui.py (style, label, scales, combos).

#![allow(deprecated)] // Gtk.ComboBox/ListStore/StyleContext, as in the Python UI.

use crate::api::{AudioApi, AudioState};
use crate::model::{target_available, Node};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use tessel_core::tr;
use tessel_gtk::gtk;

pub const STYLE_CSS: &str = r#"
@define-color audio_accent @theme_selected_bg_color;
@define-color audio_surface mix(@theme_base_color, @theme_bg_color, 0.18);
@define-color audio_muted alpha(@theme_fg_color, 0.62);
@define-color audio_line alpha(@theme_fg_color, 0.10);
window.audiomanager { background-color: @theme_bg_color; }
.audiomanager headerbar {
    background-image: none; background-color: @theme_bg_color;
    border: none; box-shadow: none; padding: 8px 14px;
}
.audiomanager .title { font-size: 24pt; font-weight: 700; }
.audiomanager headerbar .title { font-size: inherit; }
.audiomanager .subtitle, .audiomanager .muted { color: @audio_muted; }
.audiomanager .subtitle { font-size: 10pt; }
.audiomanager .section-title { font-size: 12pt; font-weight: 600; }
.audiomanager .device-name { font-weight: 600; font-size: 11pt; }
.audiomanager .card {
    background-color: @audio_surface; border: 1px solid @audio_line;
    border-radius: 14px;
}
.audiomanager .device-icon {
    min-width: 42px; min-height: 42px; border-radius: 12px;
    color: @audio_accent; background-color: alpha(@audio_accent, 0.10);
}
.audiomanager button:not(.titlebutton) {
    background-image: none; background-color: @audio_surface;
    color: @theme_fg_color; border: 1px solid @audio_line;
    border-radius: 8px; box-shadow: none; text-shadow: none;
    min-height: 28px; padding: 4px 12px;
}
.audiomanager button:hover { background-color: mix(@audio_surface, @theme_fg_color, 0.06); }
.audiomanager button:checked { background-color: alpha(@audio_accent, 0.15); color: @audio_accent; }
.audiomanager button.primary { background-color: @audio_accent; color: @theme_selected_fg_color; border-color: transparent; }
.audiomanager button.icon-button { padding: 4px 7px; min-width: 24px; background-color: transparent; border-color: transparent; }
.audiomanager button.icon-button:hover { background-color: alpha(@theme_fg_color, 0.07); }
.audiomanager button:focus-visible { outline: 2px solid alpha(@audio_accent, 0.65); outline-offset: 2px; }
.audiomanager button:disabled { opacity: 0.45; }
.audiomanager .navigation { background: transparent; }
.audiomanager .navigation row { padding: 12px; border-radius: 9px; margin-bottom: 4px; }
.audiomanager .navigation row:selected { background-color: alpha(@audio_accent, 0.14); color: @audio_accent; }
.audiomanager .navigation row:hover { background-color: alpha(@theme_fg_color, 0.06); }
.audiomanager .volume-label { font-size: 10pt; font-weight: 600; min-width: 48px; }
.audiomanager scale trough { min-height: 5px; border-radius: 8px; }
.audiomanager scale highlight { background-color: @audio_accent; border-radius: 8px; }
.audiomanager .status { font-size: 9pt; color: @audio_muted; }
.audiomanager .error { color: #ed858c; }
.audiomanager .empty { color: @audio_muted; padding: 36px 20px; }
.audiomanager .channel-label { font-size: 9pt; color: @audio_muted; }
.audiomanager levelbar.input-level trough { min-height: 4px; border-radius: 4px; }
.audiomanager levelbar.input-level block { min-height: 4px; border-radius: 4px; }
.audiomanager levelbar.input-level block.filled { background-color: @audio_accent; }
window.audiomanager.tray-popup { background-color: transparent; }
.audiomanager .popup-surface {
    background-color: @theme_bg_color; border-radius: 12px;
    border: 1px solid alpha(@theme_fg_color, 0.14);
}
"#;

pub const INPUT_CLASSES: [&str; 2] = ["Audio/Source", "Audio/Source/Virtual"];

/// Add CSS classes; windows also follow the dock palette (`attach_theme`).
pub fn style<W: IsA<gtk::Widget>>(widget: &W, names: &[&str]) {
    for name in names {
        widget.add_css_class(name);
    }
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        tessel_gtk::theme::attach(window, None);
    }
}

pub fn margins<W: IsA<gtk::Widget>>(widget: &W, value: i32) {
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
    widget.set_margin_start(value);
    widget.set_margin_end(value);
}

pub fn icon(name: Option<&str>, size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name.filter(|n| !n.is_empty()).unwrap_or("audio-card-symbolic"));
    image.set_pixel_size(size);
    image
}

/// Keep device/application artwork, with themed fallbacks for missing names.
pub fn node_icon_names(node: &Node) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if !node.icon.is_empty() {
        names.push(node.icon.clone());
        names.push(match node.icon.strip_suffix("-symbolic") {
            Some(base) => base.to_owned(),
            None => format!("{}-symbolic", node.icon),
        });
    }
    if ["Audio/Source", "Audio/Source/Virtual", "Stream/Input/Audio"].contains(&node.media_class.as_str()) {
        names.extend(["audio-input-microphone-symbolic".into(), "audio-input-microphone".into()]);
    } else {
        names.extend(["audio-speakers-symbolic".into(), "audio-speakers".into()]);
    }
    names.extend(["audio-card-symbolic".into(), "audio-card".into()]);
    let mut unique = Vec::new();
    for name in names {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    unique
}

pub fn label(text: &str, css: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::new(Some(text));
    widget.set_xalign(0.0);
    widget.set_ellipsize(gtk::pango::EllipsizeMode::End);
    widget.set_max_width_chars(45);
    if let Some(css) = css {
        style(&widget, &[css]);
    }
    widget
}

pub fn volume_scale() -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 150.0, 1.0);
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    scale.add_mark(100.0, gtk::PositionType::Bottom, None);
    scale.set_tooltip_text(Some(&tr!("Volume (100% is normal; higher values amplify)")));
    scale
}

/// Primary-button press/release (and "stopped") on a widget, in the capture phase.
pub fn track_drag(widget: &impl IsA<gtk::Widget>, pressed: impl Fn() + 'static, released: impl Fn() + 'static) -> gtk::GestureClick {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(1);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    gesture.connect_pressed(move |_, _, _, _| pressed());
    let released = Rc::new(released);
    let on_stop = released.clone();
    gesture.connect_released(move |_, _, _, _| released());
    gesture.connect_stopped(move |_| on_stop());
    widget.add_controller(gesture.clone());
    gesture
}

pub type ChoiceList = Vec<(String, String, bool)>;

/// `choice_combo()`: a ComboBox over ListStore(str id, str label, bool sensitive).
#[derive(Clone)]
pub struct ChoiceCombo {
    pub combo: gtk::ComboBox,
    pub model: gtk::ListStore,
    choices: Rc<RefCell<Option<ChoiceList>>>,
}

impl ChoiceCombo {
    pub fn new() -> Self {
        let model = gtk::ListStore::new(&[glib::Type::STRING, glib::Type::STRING, glib::Type::BOOL]);
        let combo = gtk::ComboBox::with_model(&model);
        combo.set_id_column(0);
        let renderer = gtk::CellRendererText::new();
        renderer.set_ellipsize(gtk::pango::EllipsizeMode::End);
        renderer.set_width_chars(18);
        combo.pack_start(&renderer, true);
        combo.add_attribute(&renderer, "text", 1);
        combo.add_attribute(&renderer, "sensitive", 2);
        combo.set_hexpand(true);
        Self { combo, model, choices: Rc::default() }
    }

    /// `update_combo(combo, choices, active)`.
    pub fn update(&self, choices: &[(i64, String, bool)], active: Option<i64>) {
        let choices: ChoiceList = choices.iter().map(|(i, text, available)| (i.to_string(), text.clone(), *available)).collect();
        if self.choices.borrow().as_ref() != Some(&choices) {
            self.model.clear();
            for (id, text, available) in &choices {
                self.model.insert_with_values(None, &[(0, id), (1, text), (2, available)]);
            }
            *self.choices.borrow_mut() = Some(choices.clone());
        }
        match active {
            Some(active) => {
                self.combo.set_active_id(Some(&active.to_string()));
            }
            None => self.combo.set_active(None),
        }
        let current = active.and_then(|a| choices.iter().find(|(i, _, _)| *i == a.to_string())).map(|c| c.1.clone()).unwrap_or_default();
        self.combo.set_tooltip_text(Some(&current));
    }

    /// `choose_index(combo)`: the selected id if its row is sensitive.
    pub fn choose_index(&self) -> Option<i64> {
        let iter = self.combo.active_iter()?;
        if !self.model.get::<bool>(&iter, 2) {
            return None;
        }
        self.combo.active_id()?.parse().ok()
    }

    pub fn labels(&self) -> Vec<String> {
        self.choices.borrow().as_ref().map(|c| c.iter().map(|x| x.1.clone()).collect()).unwrap_or_default()
    }
}

impl Default for ChoiceCombo {
    fn default() -> Self {
        Self::new()
    }
}

pub fn device_class(node: &Node) -> &str {
    if node.media_class == "Audio/Source/Virtual" { "Audio/Source" } else { &node.media_class }
}

pub fn level_bar() -> gtk::LevelBar {
    let bar = gtk::LevelBar::new();
    bar.set_min_value(0.0);
    bar.set_max_value(1.0);
    style(&bar, &["input-level"]);
    bar.set_mode(gtk::LevelBarMode::Continuous);
    for name in ["low", "high", "full"] {
        bar.remove_offset_value(Some(name));
    }
    bar.set_tooltip_text(Some(&tr!("Input level")));
    tessel_gtk::utils::accessible(&bar, &tr!("Input level"));
    bar
}

pub fn casefold(text: &str) -> String {
    tessel_core::pyjson::casefold(text)
}

/// Selectable default-device choices: visible outputs or inputs.
pub fn device_choices(state: &AudioState, media_class: &str) -> Vec<(i64, String, bool)> {
    let mut nodes: Vec<&Node> = state.nodes.values().filter(|n| device_class(n) == media_class && !n.loopback_internal).collect();
    let name = |n: &Node| if n.description.is_empty() { n.name.clone() } else { n.description.clone() };
    nodes.sort_by_key(|n| (casefold(&name(n)), n.id));
    nodes.iter().map(|n| {
        let available = target_available(Some(n));
        (n.id, if available { name(n) } else { tr!("{device} (unavailable)", device = name(n)) }, available)
    }).collect()
}

/// A consistent copy of the backend state (never hold the backend's borrow
/// across calls that can report errors back into the views).
pub fn snapshot(backend: &dyn AudioApi) -> AudioState {
    backend.state().clone()
}

/// Something rows report back to: the settings window or the popup.
pub trait View {
    fn backend(&self) -> Rc<dyn AudioApi>;
    fn perform(&self, action: crate::api::Action);
    fn set_error(&self, message: &str);
}

/// Mute icon by state (shared by rows and the popup).
pub fn mute_icon_name(mute: bool, volume: f64) -> &'static str {
    if mute || volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 0.34 {
        "audio-volume-low-symbolic"
    } else if volume < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

/// Python `"%d%%" % round(value * 100)` / `f"{value:.0f}%"` (half-even).
pub fn percent(value: f64) -> String {
    format!("{}%", round_half_even(value))
}

pub fn round_half_even(value: f64) -> i64 {
    let rounded = value.round();
    if (value - value.trunc()).abs() == 0.5 {
        (2.0 * (value / 2.0).round()) as i64
    } else {
        rounded as i64
    }
}
