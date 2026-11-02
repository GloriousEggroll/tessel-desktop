//! Display quick controls (`brightness_ui.py`): the "Displays" popup with
//! per-display brightness/contrast/scaling rows, automatic scaling and
//! auto-dim switches. `BrightnessSettingsWindow` is not ported: it is
//! unreachable from the application (`--settings` opens tessel-displays).

use crate::client::UiClient;
use crate::model::{self, number};
use crate::style::{icon, install_style, label, margins, style};
use gtk::glib;
use gtk::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;
use tessel_core::pyfmt::{format_value, Arg};
use tessel_core::tr;
use tessel_gtk::gtk;
use tessel_gtk::popup::{friendly_status, header_button, popup_header, set_popup_width, HeaderAction, PopupController, POPUP_PADDING,
                       POPUP_WIDTH};
use tessel_gtk::utils::{accessible, append, children, remove_css};

pub type Client = Rc<dyn UiClient>;

/// Map raw errors (paths, errno text) to plain status text; log the details.
pub fn friendly_message(message: &str, fallback: Option<&str>) -> String {
    let default = tr!("Some display settings are unavailable. See the log for details.");
    friendly_status(message, fallback.unwrap_or(&default), &[])
}

fn display_for(state: &Value, connector: &str, identity: &str) -> Option<Value> {
    state.get("displays").and_then(Value::as_array)?.iter()
        .find(|d| model::text(d, "connector") == connector && model::text(d, "id") == identity).cloned()
}

/// `percent(feature)`: clamped float or `None`.
pub fn percent(feature: &Value) -> Option<f64> {
    let value = feature.get("valuepercent").or_else(|| feature.get("value"))?;
    number(value).map(|v| v.clamp(0.0, 100.0))
}

fn fmt(value: f64, spec: &str) -> String {
    format_value(&Arg::Float(value), spec).unwrap_or_default()
}

/// `f"{value * 100:g}%"`.
pub fn scale_text(value: f64) -> String {
    format!("{}%", fmt(value * 100.0, "g"))
}

fn card() -> (gtk::Box, gtk::Box) {
    let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
    style(&surface, &["card"]);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 13);
    margins(&body, 15);
    surface.append(&body);
    (surface, body)
}

fn schedule(timer: &RefCell<Option<glib::SourceId>>, callback: impl FnOnce() + 'static) {
    if let Some(source) = timer.take() {
        source.remove();
    }
    timer.replace(Some(glib::timeout_add_local_once(Duration::from_millis(200), callback)));
}

fn unschedule(timer: &RefCell<Option<glib::SourceId>>) {
    if let Some(source) = timer.take() {
        source.remove();
    }
}

/// Keep local gestures stable while a different control is refreshing.
pub struct FeatureRowInner {
    pub root: gtk::Box,
    pub value_label: gtk::Label,
    pub scale: gtk::Scale,
    pub hint: gtk::Label,
    client: Client,
    connector: String,
    identity: String,
    feature: String,
    syncing: Cell<bool>,
    dragging: Cell<bool>,
    destroyed: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    pending: Cell<Option<i64>>,
    submitted: Cell<Option<f64>>,
    submitted_busy: Cell<bool>,
    message: RefCell<String>,
}

#[derive(Clone)]
pub struct FeatureRow(pub Rc<FeatureRowInner>);

impl FeatureRow {
    pub fn new(client: Client, connector: &str, identity: &str, feature: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let title = if feature == "brightness" { tr!("Brightness") } else { tr!("Contrast") };
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        append(&heading, &icon(if feature == "brightness" { "display-brightness-symbolic" } else { "weather-clear-night-symbolic" }, 16),
               false, false);
        append(&heading, &label(&title, None, false), true, false);
        let value_label = label("—", Some("value"), false);
        value_label.set_width_chars(5);
        value_label.set_xalign(1.0);
        append(&heading, &value_label, false, false);
        append(&root, &heading, false, false);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        scale.set_draw_value(false);
        scale.set_hexpand(true);
        accessible(&scale, &format!("{connector} {}", title.to_lowercase()));
        append(&root, &scale, false, false);
        let hint = label("", Some("status"), true);
        append(&root, &hint, false, false);
        let row = Self(Rc::new(FeatureRowInner {
            root, value_label, scale, hint, client, connector: connector.into(), identity: identity.into(), feature: feature.into(),
            syncing: Cell::new(false), dragging: Cell::new(false), destroyed: Cell::new(false), timer: RefCell::new(None),
            pending: Cell::new(None), submitted: Cell::new(None), submitted_busy: Cell::new(false), message: RefCell::new(String::new()),
        }));
        let weak = row.weak();
        row.0.scale.connect_value_changed(move |_| if let Some(r) = weak.upgrade().map(FeatureRow) { r.changed() });
        let gesture = gtk::GestureClick::new();
        gesture.set_button(1);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = row.weak();
        gesture.connect_pressed(move |_, _, _, _| if let Some(r) = weak.upgrade().map(FeatureRow) { r.pressed() });
        let weak = row.weak();
        gesture.connect_released(move |_, _, _, _| if let Some(r) = weak.upgrade().map(FeatureRow) { r.released() });
        let weak = row.weak();
        gesture.connect_stopped(move |_| if let Some(r) = weak.upgrade().map(FeatureRow) { r.released() });
        row.0.scale.add_controller(gesture);
        let weak = row.weak();
        row.0.root.connect_unrealize(move |_| if let Some(r) = weak.upgrade().map(FeatureRow) { r.destroy() });
        row.refresh();
        row
    }

    fn weak(&self) -> Weak<FeatureRowInner> {
        Rc::downgrade(&self.0)
    }

    fn available(&self) -> (Option<Value>, Value, bool) {
        let display = display_for(&self.0.client.state(), &self.0.connector, &self.0.identity);
        let feature = display.as_ref().and_then(|d| d.get(&self.0.feature)).cloned().unwrap_or(Value::Null);
        let available = self.0.client.ready() && !self.0.identity.is_empty()
            && display.as_ref().is_some_and(|d| model::enabled(d, false))
            && feature.get("supported") == Some(&Value::Bool(true)) && percent(&feature).is_some();
        (display, feature, available)
    }

    pub fn dragging(&self) -> bool {
        self.0.dragging.get()
    }

    pub fn pressed(&self) {
        self.0.dragging.set(true);
    }

    pub fn released(&self) {
        self.0.dragging.set(false);
        if self.0.pending.get().is_some() {
            self.schedule();
        }
    }

    fn changed(&self) {
        if self.0.syncing.get() || self.0.destroyed.get() || !self.available().2 {
            return;
        }
        let pending = self.0.scale.value().round_ties_even() as i64;
        self.0.pending.set(Some(pending));
        self.0.message.replace(String::new());
        self.0.value_label.set_text(&format!("{pending}%"));
        self.schedule();
    }

    fn schedule(&self) {
        let weak = self.weak();
        schedule(&self.0.timer, move || if let Some(r) = weak.upgrade().map(FeatureRow) { r.0.timer.replace(None); r.send() });
    }

    fn send(&self) {
        let Some(value) = self.0.pending.get().filter(|_| !self.0.destroyed.get()) else { return };
        let (_display, feature, available) = self.available();
        if !available {
            self.cancel();
            self.refresh();
            return;
        }
        if feature.get("busy").is_some_and(model::truthy) {
            self.schedule();
            return;
        }
        self.0.pending.set(None);
        if percent(&feature) == Some(value as f64) {
            self.refresh();
            return;
        }
        self.0.submitted.set(Some(value as f64));
        self.0.submitted_busy.set(false);
        if !self.0.client.set_value(&self.0.connector, &self.0.feature, value as f64, Some(&self.0.identity)) {
            self.0.submitted.set(None);
            let error = self.0.client.error();
            self.0.message.replace(if error.is_empty() { tr!("The display could not be adjusted.") } else { error });
        }
        self.refresh();
    }

    pub fn refresh(&self) {
        if self.0.destroyed.get() {
            return;
        }
        let (display, feature, available) = self.available();
        let observed = percent(&feature);
        if !available {
            self.cancel();
        }
        if let Some(submitted) = self.0.submitted.get() {
            if feature.get("busy").is_some_and(model::truthy) {
                self.0.submitted_busy.set(true);
            } else if observed == Some(submitted) || self.0.submitted_busy.get() || feature.get("error").is_some_and(model::truthy) {
                self.0.submitted.set(None);
            }
        }
        self.0.scale.set_sensitive(available);
        let local = self.0.pending.get().map(|v| v as f64).or(self.0.submitted.get());
        if available && !self.0.dragging.get() && local.is_none() {
            self.0.syncing.set(true);
            self.0.scale.set_value(observed.unwrap_or(0.0));
            self.0.syncing.set(false);
        }
        let shown = local.or(if self.0.dragging.get() && available { Some(self.0.scale.value()) } else { observed });
        self.0.value_label.set_text(&match shown.filter(|_| available) {
            Some(shown) => format!("{}%", fmt(shown, ".0f")),
            None => "—".into(),
        });
        let raw = {
            let message = self.0.message.borrow().clone();
            if message.is_empty() { feature.get("error").and_then(Value::as_str).unwrap_or("").to_owned() } else { message }
        };
        let mut hint = friendly_message(&raw, Some(&tr!("This display could not be adjusted.")));
        if hint.is_empty() && !available {
            hint = if !self.0.client.ready() {
                tr!("Loading display controls…")
            } else if !display.as_ref().is_some_and(|d| model::enabled(d, false)) {
                tr!("Display is off or disconnected.")
            } else if feature.get("supported") != Some(&Value::Bool(true)) {
                tr!("Not supported by this display.")
            } else {
                tr!("The current value is unavailable.")
            };
        }
        self.0.hint.set_text(&hint);
        self.0.hint.set_visible(!hint.is_empty());
        self.0.scale.set_tooltip_text((!hint.is_empty()).then_some(hint.as_str()));
    }

    pub fn cancel(&self) {
        unschedule(&self.0.timer);
        self.0.pending.set(None);
        self.0.submitted.set(None);
        self.0.dragging.set(false);
    }

    pub fn destroy(&self) {
        self.cancel();
        self.0.destroyed.set(true);
    }
}

/// Notched display scaling, committed after a drag or keyboard adjustment.
pub struct ScalingRowInner {
    pub root: gtk::Box,
    pub value_label: gtk::Label,
    pub scale: gtk::Scale,
    pub hint: gtk::Label,
    client: Client,
    connector: String,
    identity: String,
    syncing: Cell<bool>,
    destroyed: Cell<bool>,
    submitted_busy: Cell<bool>,
    dragging: Cell<bool>,
    pending: Cell<Option<f64>>,
    submitted: Cell<Option<f64>>,
    scale_identity: RefCell<Option<String>>,
    timer: RefCell<Option<glib::SourceId>>,
    message: RefCell<String>,
}

#[derive(Clone)]
pub struct ScalingRow(pub Rc<ScalingRowInner>);

struct ScaleState {
    display: Option<Value>,
    scale: Value,
    service: Value,
    value: Option<f64>,
    available: bool,
}

impl ScalingRow {
    pub fn new(client: Client, connector: &str, identity: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        append(&heading, &icon("zoom-fit-best-symbolic", 16), false, false);
        append(&heading, &label(&tr!("Scaling"), None, false), true, false);
        let value_label = label("—", Some("value"), false);
        value_label.set_width_chars(7);
        value_label.set_xalign(1.0);
        append(&heading, &value_label, false, false);
        append(&root, &heading, false, false);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 2.5, 0.125);
        scale.set_draw_value(false);
        scale.set_digits(3);
        scale.set_hexpand(true);
        scale.set_increments(0.125, 0.125);
        for step in 8..=20 {
            let value = step as f64 / 8.0;
            let text = (step % 4 == 0).then(|| scale_text(value));
            scale.add_mark(value, gtk::PositionType::Bottom, text.as_deref());
        }
        accessible(&scale, &tr!("{display} scaling", display = connector));
        append(&root, &scale, false, false);
        let hint = label("", Some("status"), true);
        append(&root, &hint, false, false);
        let row = Self(Rc::new(ScalingRowInner {
            root, value_label, scale, hint, client, connector: connector.into(), identity: identity.into(), syncing: Cell::new(false),
            destroyed: Cell::new(false), submitted_busy: Cell::new(false), dragging: Cell::new(false), pending: Cell::new(None),
            submitted: Cell::new(None), scale_identity: RefCell::new(None), timer: RefCell::new(None), message: RefCell::new(String::new()),
        }));
        let weak = row.weak();
        row.0.scale.connect_value_changed(move |_| if let Some(r) = weak.upgrade().map(ScalingRow) { r.changed() });
        // "stopped" fires as GtkScale claims a drag, not when it ends. The
        // unpaired release still reports its actual button release afterward.
        let pointer = gtk::GestureClick::new();
        pointer.set_button(1);
        pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = row.weak();
        pointer.connect_pressed(move |_, _, _, _| if let Some(r) = weak.upgrade().map(ScalingRow) { r.pressed() });
        let weak = row.weak();
        pointer.connect_released(move |_, _, _, _| if let Some(r) = weak.upgrade().map(ScalingRow) { r.released() });
        let weak = row.weak();
        pointer.connect_unpaired_release(move |_, _, _, _, _| if let Some(r) = weak.upgrade().map(ScalingRow) { r.released() });
        row.0.scale.add_controller(pointer);
        let weak = row.weak();
        row.0.root.connect_unrealize(move |_| if let Some(r) = weak.upgrade().map(ScalingRow) { r.destroy() });
        row.refresh();
        row
    }

    fn weak(&self) -> Weak<ScalingRowInner> {
        Rc::downgrade(&self.0)
    }

    fn state(&self) -> ScaleState {
        let state = self.0.client.state();
        let display = display_for(&state, &self.0.connector, &self.0.identity);
        let scale = display.as_ref().and_then(|d| d.get("scale")).cloned().unwrap_or(Value::Null);
        let service = state.get("scaling").cloned().unwrap_or(Value::Null);
        let value = scale.get("value").and_then(number).filter(|v| (0.25..=8.0).contains(v));
        let available = self.0.client.ready() && service.get("ready") == Some(&Value::Bool(true))
            && display.as_ref().is_some_and(|d| model::enabled(d, false)) && scale.get("supported") == Some(&Value::Bool(true))
            && value.is_some() && display.as_ref().is_some_and(|d| !model::text(d, "scale_id").is_empty());
        ScaleState { display, scale, service, value, available }
    }

    fn busy(state: &ScaleState) -> bool {
        state.scale.get("busy").is_some_and(model::truthy) || state.service.get("busy").is_some_and(model::truthy)
    }

    fn scale_id(state: &ScaleState) -> Option<String> {
        state.display.as_ref().map(|d| model::text(d, "scale_id").to_owned())
    }

    pub fn dragging(&self) -> bool {
        self.0.dragging.get()
    }

    pub fn pressed(&self) {
        self.0.dragging.set(true);
        unschedule(&self.0.timer);
    }

    pub fn released(&self) {
        self.0.dragging.set(false);
        if self.0.pending.get().is_some() {
            self.schedule();
        }
    }

    fn changed(&self) {
        if self.0.syncing.get() || self.0.destroyed.get() {
            return;
        }
        let state = self.state();
        if !state.available || Self::busy(&state) || self.0.submitted.get().is_some()
            || *self.0.scale_identity.borrow() != Self::scale_id(&state) {
            self.refresh();
            return;
        }
        let selected = ((self.0.scale.value() * 8.0).round_ties_even() / 8.0).clamp(1.0, 2.5);
        self.0.syncing.set(true);
        self.0.scale.set_value(selected);
        self.0.syncing.set(false);
        self.0.pending.set(Some(selected));
        self.0.message.replace(String::new());
        self.0.value_label.set_text(&scale_text(selected));
        if !self.0.dragging.get() {
            self.schedule();
        }
    }

    fn schedule(&self) {
        let weak = self.weak();
        schedule(&self.0.timer, move || if let Some(r) = weak.upgrade().map(ScalingRow) { r.0.timer.replace(None); r.send() });
    }

    fn send(&self) {
        if self.0.destroyed.get() || self.0.dragging.get() {
            return;
        }
        let Some(value) = self.0.pending.get() else { return };
        let state = self.state();
        if !state.available || *self.0.scale_identity.borrow() != Self::scale_id(&state) || Self::busy(&state)
            || self.0.submitted.get().is_some() {
            self.cancel();
            self.refresh();
            return;
        }
        self.0.pending.set(None);
        if Some(value) == state.value {
            self.refresh();
            return;
        }
        self.0.submitted.set(Some(value));
        self.0.submitted_busy.set(false);
        let identity = self.0.scale_identity.borrow().clone();
        if !self.0.client.set_scale(&self.0.connector, value, identity.as_deref()) {
            self.0.submitted.set(None);
            let error = self.0.client.error();
            self.0.message.replace(if error.is_empty() { tr!("The display scale could not be changed.") } else { error });
        }
        self.refresh();
    }

    pub fn refresh(&self) {
        if self.0.destroyed.get() {
            return;
        }
        let state = self.state();
        let busy = Self::busy(&state);
        let error = [state.scale.get("error"), state.service.get("error")].into_iter().flatten()
            .find(|e| model::truthy(e)).map(|e| e.as_str().map(String::from).unwrap_or_else(|| e.to_string())).unwrap_or_default();
        let identity = Self::scale_id(&state);
        if !state.available || identity != *self.0.scale_identity.borrow() {
            self.cancel();
        }
        if self.0.pending.get().is_some() && busy {
            self.cancel();
        }
        if let Some(submitted) = self.0.submitted.get() {
            if busy {
                self.0.submitted_busy.set(true);
            } else if state.value == Some(submitted) || self.0.submitted_busy.get() || !error.is_empty() || !self.0.client.error().is_empty() {
                self.0.submitted.set(None);
            }
        }
        self.0.scale_identity.replace(identity);
        let shown = self.0.pending.get().or(self.0.submitted.get()).or(state.value);
        self.0.syncing.set(true);
        // Preserve an existing custom scale without snapping it on refresh.
        // New adjustments still use the same 100–250% stops as way-displays.
        self.0.scale.set_range(shown.map_or(1.0, |s| s.min(1.0)), shown.map_or(2.5, |s| s.max(2.5)));
        if let Some(shown) = shown {
            self.0.scale.set_value(shown);
        }
        self.0.scale.set_sensitive(state.available && !busy && self.0.submitted.get().is_none());
        self.0.syncing.set(false);
        self.0.value_label.set_text(&match shown.filter(|_| state.available) {
            Some(shown) => scale_text(shown),
            None => "—".into(),
        });
        let raw = {
            let message = self.0.message.borrow().clone();
            if message.is_empty() { error.clone() } else { message }
        };
        let mut hint = friendly_message(&raw, Some(&tr!("Display scaling is unavailable.")));
        if hint.is_empty() && !state.available {
            hint = if !self.0.client.ready() || !state.service.get("ready").is_some_and(model::truthy) {
                tr!("Display scaling is unavailable.")
            } else if !state.display.as_ref().is_some_and(|d| model::enabled(d, false)) {
                tr!("Display is off or disconnected.")
            } else {
                tr!("Scaling is not supported by this display.")
            };
        }
        self.0.hint.set_text(&hint);
        self.0.hint.set_visible(!hint.is_empty());
        self.0.scale.set_tooltip_text((!hint.is_empty()).then_some(hint.as_str()));
    }

    pub fn cancel(&self) {
        unschedule(&self.0.timer);
        self.0.pending.set(None);
        self.0.submitted.set(None);
        self.0.dragging.set(false);
    }

    pub fn destroy(&self) {
        self.cancel();
        self.0.destroyed.set(true);
    }
}

#[derive(Clone)]
pub enum Row {
    Feature(FeatureRow),
    Scaling(ScalingRow),
}

impl Row {
    pub fn refresh(&self) {
        match self {
            Row::Feature(r) => r.refresh(),
            Row::Scaling(r) => r.refresh(),
        }
    }

    pub fn cancel(&self) {
        match self {
            Row::Feature(r) => r.cancel(),
            Row::Scaling(r) => r.cancel(),
        }
    }

    pub fn destroy(&self) {
        match self {
            Row::Feature(r) => r.destroy(),
            Row::Scaling(r) => r.destroy(),
        }
    }

    pub fn root(&self) -> gtk::Box {
        match self {
            Row::Feature(r) => r.0.root.clone(),
            Row::Scaling(r) => r.0.root.clone(),
        }
    }

    pub fn scale(&self) -> gtk::Scale {
        match self {
            Row::Feature(r) => r.0.scale.clone(),
            Row::Scaling(r) => r.0.scale.clone(),
        }
    }

    pub fn value_label(&self) -> gtk::Label {
        match self {
            Row::Feature(r) => r.0.value_label.clone(),
            Row::Scaling(r) => r.0.value_label.clone(),
        }
    }

    pub fn hint(&self) -> gtk::Label {
        match self {
            Row::Feature(r) => r.0.hint.clone(),
            Row::Scaling(r) => r.0.hint.clone(),
        }
    }

    pub fn dragging(&self) -> bool {
        match self {
            Row::Feature(r) => r.dragging(),
            Row::Scaling(r) => r.dragging(),
        }
    }

    pub fn pressed(&self) {
        match self {
            Row::Feature(r) => r.pressed(),
            Row::Scaling(r) => r.pressed(),
        }
    }

    pub fn released(&self) {
        match self {
            Row::Feature(r) => r.released(),
            Row::Scaling(r) => r.released(),
        }
    }
}

pub type RowKey = (String, String, String);

pub struct DisplayControls {
    pub root: gtk::Box,
    client: Client,
    features: Vec<&'static str>,
    pub empty: gtk::Label,
    cards: RefCell<HashMap<(String, String), (gtk::Box, gtk::Label, gtk::Label)>>,
    pub rows: RefCell<HashMap<RowKey, Row>>,
}

impl DisplayControls {
    pub fn new(client: Client, scaling: bool) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let empty = label(&tr!("Looking for displays…"), Some("status"), true);
        append(&root, &empty, false, false);
        let features = if scaling { vec!["brightness", "contrast", "scale"] } else { vec!["brightness", "contrast"] };
        Rc::new(Self { root, client, features, empty, cards: RefCell::new(HashMap::new()), rows: RefCell::new(HashMap::new()) })
    }

    pub fn row(&self, connector: &str, identity: &str, feature: &str) -> Option<Row> {
        self.rows.borrow().get(&(connector.into(), identity.into(), feature.into())).cloned()
    }

    pub fn refresh(&self) {
        let state = self.client.state();
        let displays: Vec<Value> = state.get("displays").and_then(Value::as_array).cloned().unwrap_or_default();
        let keys: Vec<(String, String)> = displays.iter().map(|d| (model::text(d, "connector").to_owned(), model::text(d, "id").to_owned()))
            .collect();
        let stale: Vec<(String, String)> = self.cards.borrow().keys().filter(|k| !keys.contains(k)).cloned().collect();
        for key in stale {
            if let Some((surface, _, _)) = self.cards.borrow_mut().remove(&key) {
                self.root.remove(&surface);
            }
            for feature in &self.features {
                if let Some(row) = self.rows.borrow_mut().remove(&(key.0.clone(), key.1.clone(), feature.to_string())) {
                    row.destroy();
                }
            }
        }
        for (index, display) in displays.iter().enumerate() {
            let key = keys[index].clone();
            if !self.cards.borrow().contains_key(&key) {
                let (surface, body) = card();
                let heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
                append(&heading, &icon("video-display-symbolic", 21), false, false);
                let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
                let name = label("", Some("row-title"), false);
                let connector = label("", Some("status"), false);
                append(&titles, &name, false, false);
                append(&titles, &connector, false, false);
                append(&heading, &titles, true, false);
                append(&body, &heading, false, false);
                for feature in &self.features {
                    let row = if *feature == "scale" {
                        Row::Scaling(ScalingRow::new(self.client.clone(), &key.0, &key.1))
                    } else {
                        Row::Feature(FeatureRow::new(self.client.clone(), &key.0, &key.1, feature))
                    };
                    append(&body, &row.root(), false, false);
                    self.rows.borrow_mut().insert((key.0.clone(), key.1.clone(), feature.to_string()), row);
                }
                append(&self.root, &surface, false, false);
                surface.set_visible(true);
                self.cards.borrow_mut().insert(key.clone(), (surface, name, connector));
            }
            let (surface, name, connector) = self.cards.borrow()[&key].clone();
            let title = [display.get("name"), display.get("connector")].into_iter().flatten().find(|v| model::truthy(v))
                .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_else(|| tr!("Display"));
            name.set_text(&title);
            let output = model::text(display, "connector");
            connector.set_text(&if !model::enabled(display, false) { tr!("{display} · Off", display = output) } else { output.to_owned() });
            let previous = if index > 0 { children(&self.root).get(index - 1).cloned() } else { None };
            self.root.reorder_child_after(&surface, previous.as_ref());
            for feature in &self.features {
                let row = self.rows.borrow().get(&(key.0.clone(), key.1.clone(), feature.to_string())).cloned();
                if let Some(row) = row {
                    row.refresh();
                }
            }
        }
        self.empty.set_text(&if self.client.ready() { tr!("No displays found.") } else { tr!("Looking for displays…") });
        self.empty.set_visible(displays.is_empty());
    }

    pub fn cancel(&self) {
        let rows: Vec<Row> = self.rows.borrow().values().cloned().collect();
        for row in rows {
            row.cancel();
        }
    }
}

pub struct AutoDimRow {
    pub root: gtk::Box,
    pub toggle: gtk::Switch,
    pub hint: gtk::Label,
    client: Client,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
    syncing: Cell<bool>,
    pending: Cell<Option<bool>>,
    pub message: RefCell<String>,
}

impl AutoDimRow {
    pub fn new(client: Client) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
        append(&titles, &label(&tr!("Auto-dim"), Some("row-title"), false), false, false);
        let hint = label(&tr!("Dim displays while inactive."), Some("status"), true);
        append(&titles, &hint, false, false);
        append(&root, &titles, true, false);
        let toggle = gtk::Switch::new();
        toggle.set_valign(gtk::Align::Center);
        accessible(&toggle, &tr!("Automatically dim displays"));
        append(&root, &toggle, false, false);
        let row = Rc::new(Self { root, toggle, hint, client, changed: RefCell::new(None), syncing: Cell::new(false),
                                 pending: Cell::new(None), message: RefCell::new(String::new()) });
        let weak = Rc::downgrade(&row);
        row.toggle.connect_active_notify(move |toggle| if let Some(r) = weak.upgrade() { r.toggled(toggle.is_active()) });
        row
    }

    fn toggled(&self, desired: bool) {
        if self.syncing.get() || !self.client.ready() {
            return;
        }
        self.pending.set(Some(desired));
        self.message.replace(String::new());
        if !self.client.set_auto_dim(desired) {
            self.pending.set(None);
            let error = self.client.error();
            self.message.replace(if error.is_empty() { tr!("The automatic dimming setting could not be saved.") } else { error });
        }
        let changed = self.changed.borrow().clone();
        if let Some(changed) = changed {
            changed();
        }
    }

    pub fn refresh(&self) {
        let state = self.client.state();
        let value = state.get("settings").and_then(|s| s.get("auto_dim")).and_then(Value::as_bool);
        let available = self.client.ready() && value.is_some();
        if self.pending.get() == value || !self.client.error().is_empty() || state.get("error").is_some_and(model::truthy) {
            self.pending.set(None);
        }
        self.syncing.set(true);
        self.toggle.set_active(self.pending.get().unwrap_or(value.unwrap_or(false)));
        self.toggle.set_sensitive(available);
        self.syncing.set(false);
        self.hint.set_text(&if state.get("dimmed").is_some_and(model::truthy) {
            tr!("Displays are dimmed. Move the pointer to restore.")
        } else {
            tr!("Dim displays while inactive.")
        });
    }
}

pub struct AutoScaleRow {
    pub root: gtk::Box,
    pub toggle: gtk::Switch,
    client: Client,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
    syncing: Cell<bool>,
    pending: Cell<Option<bool>>,
    submitted_busy: Cell<bool>,
    pub message: RefCell<String>,
}

impl AutoScaleRow {
    pub fn new(client: Client) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
        append(&titles, &label(&tr!("Automatic scaling"), Some("row-title"), false), false, false);
        append(&titles, &label(&tr!("Manual scales override automatic scaling for that display."), Some("status"), true), false, false);
        append(&root, &titles, true, false);
        let toggle = gtk::Switch::new();
        toggle.set_valign(gtk::Align::Center);
        accessible(&toggle, &tr!("Automatic display scaling"));
        append(&root, &toggle, false, false);
        let row = Rc::new(Self { root, toggle, client, changed: RefCell::new(None), syncing: Cell::new(false), pending: Cell::new(None),
                                 submitted_busy: Cell::new(false), message: RefCell::new(String::new()) });
        let weak = Rc::downgrade(&row);
        row.toggle.connect_active_notify(move |toggle| if let Some(r) = weak.upgrade() { r.toggled(toggle.is_active()) });
        row
    }

    fn service(&self) -> Value {
        self.client.state().get("scaling").cloned().unwrap_or(Value::Null)
    }

    fn toggled(&self, desired: bool) {
        if self.syncing.get() {
            return;
        }
        let service = self.service();
        if !self.client.ready() || !service.get("ready").is_some_and(model::truthy) || service.get("busy").is_some_and(model::truthy) {
            self.refresh();
            return;
        }
        self.pending.set(Some(desired));
        self.submitted_busy.set(false);
        self.message.replace(String::new());
        if !self.client.set_auto_scale(desired) {
            self.pending.set(None);
            let error = self.client.error();
            self.message.replace(if error.is_empty() { tr!("Automatic scaling could not be changed.") } else { error });
        }
        let changed = self.changed.borrow().clone();
        if let Some(changed) = changed {
            changed();
        }
    }

    pub fn refresh(&self) {
        let service = self.service();
        let value = service.get("auto_scale").and_then(Value::as_bool);
        let busy = service.get("busy").is_some_and(model::truthy);
        let available = self.client.ready() && service.get("ready").is_some_and(model::truthy) && value.is_some();
        if self.pending.get().is_some() {
            if busy {
                self.submitted_busy.set(true);
            } else if !available || self.pending.get() == value || self.submitted_busy.get()
                || service.get("error").is_some_and(model::truthy) || !self.client.error().is_empty() {
                self.pending.set(None);
            }
        }
        self.syncing.set(true);
        self.toggle.set_active(self.pending.get().unwrap_or(value.unwrap_or(false)));
        self.toggle.set_sensitive(available && !busy && self.pending.get().is_none());
        self.syncing.set(false);
    }
}

/// Local, application, client and service messages, friendly and de-duplicated.
pub fn status_message(application_error: &str, client: &dyn UiClient, local: &str) -> String {
    let state_error = model::text(&client.state(), "error").to_owned();
    let mut lines: Vec<String> = Vec::new();
    for message in [local.to_owned(), application_error.to_owned(), client.error(), state_error] {
        if message.is_empty() {
            continue;
        }
        let friendly = friendly_message(&message, None);
        if !lines.contains(&friendly) {
            lines.push(friendly);
        }
    }
    lines.join("\n")
}

pub struct BrightnessPopup {
    pub window: gtk::Window,
    pub settings_button: gtk::Button,
    pub controls: Rc<DisplayControls>,
    pub auto_scale: Rc<AutoScaleRow>,
    pub auto_dim: Rc<AutoDimRow>,
    pub status_label: gtk::Label,
    pub controller: PopupController,
    client: Client,
    application_error: Rc<dyn Fn() -> String>,
    destroyed: Cell<bool>,
    css: RefCell<Option<gtk::CssProvider>>,
}

impl BrightnessPopup {
    pub fn new(client: Client, application_error: Rc<dyn Fn() -> String>, open_settings: Rc<dyn Fn()>, edge: &str, offset: i32)
               -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_title(Some(&tr!("Displays")));
        set_popup_width(&window, POPUP_WIDTH);
        style(&window, &["brightnessmanager"]);
        let css = install_style(&window);
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        style(&surface, &["popup-surface"]);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, POPUP_PADDING);
        surface.append(&content);
        window.set_child(Some(&surface));
        let settings_button = header_button("emblem-system-symbolic", &tr!("Display settings"), None, false);
        let heading = popup_header(&["video-display-symbolic"], &tr!("Displays"), Some(&tr!("Brightness, contrast & scaling")),
                                   vec![HeaderAction::Widget(settings_button.clone().upcast())]);
        append(&content, &heading.root, false, false);
        let controls = DisplayControls::new(client.clone(), true);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(440);
        scroll.set_child(Some(&controls.root));
        append(&content, &scroll, true, false);
        let auto_scale = AutoScaleRow::new(client.clone());
        append(&content, &auto_scale.root, false, false);
        let auto_dim = AutoDimRow::new(client.clone());
        append(&content, &auto_dim.root, false, false);
        let status_label = label("", Some("status"), true);
        append(&content, &status_label, false, false);
        let popup = Rc::new_cyclic(|weak: &Weak<BrightnessPopup>| {
            let apply = weak.clone();
            let controller = PopupController::new(&window, move || if let Some(p) = apply.upgrade() { p.refresh() }, edge, offset,
                                                  "service_apps");
            Self { window: window.clone(), settings_button: settings_button.clone(), controls, auto_scale, auto_dim, status_label,
                   controller, client, application_error, destroyed: Cell::new(false), css: RefCell::new(Some(css)) }
        });
        for row_changed in [&popup.auto_scale.changed, &popup.auto_dim.changed] {
            let weak = Rc::downgrade(&popup);
            row_changed.replace(Some(Rc::new(move || if let Some(p) = weak.upgrade() { p.refresh() })));
        }
        let weak = Rc::downgrade(&popup);
        popup.settings_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.dismiss();
                open_settings();
            }
        });
        let weak = Rc::downgrade(&popup);
        popup.window.connect_visible_notify(move |window| {
            if !window.get_visible() {
                if let Some(p) = weak.upgrade() {
                    p.controls.cancel();
                }
            }
        });
        let weak = Rc::downgrade(&popup);
        popup.window.connect_unrealize(move |window| {
            if let Some(p) = weak.upgrade() {
                p.destroyed.set(true);
                p.controls.cancel();
                if let Some(css) = p.css.take() {
                    remove_css(&WidgetExt::display(window), &css);
                }
            }
        });
        popup.refresh();
        popup
    }

    pub fn destroyed(&self) -> bool {
        self.destroyed.get()
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        self.controls.refresh();
        self.auto_scale.refresh();
        self.auto_dim.refresh();
        let scaling_error = self.client.state().get("scaling").and_then(|s| s.get("error")).and_then(Value::as_str).unwrap_or("").to_owned();
        let local = [self.auto_scale.message.borrow().clone(), self.auto_dim.message.borrow().clone(), scaling_error]
            .into_iter().find(|m| !m.is_empty()).unwrap_or_default();
        let message = status_message(&(self.application_error)(), self.client.as_ref(), &local);
        self.status_label.set_text(&message);
        self.status_label.set_visible(!message.is_empty());
    }

    pub fn show(&self) {
        if !self.destroyed.get() {
            self.controller.show();
        }
    }

    pub fn activate(&self) {
        if !self.destroyed.get() {
            self.controller.activate();
        }
    }

    pub fn dismiss(&self) {
        if !self.destroyed.get() {
            self.controller.dismiss();
            // notify::visible only fires for a mapped popup; a dismiss that
            // races an asynchronous show must still drop unsent slider values.
            self.controls.cancel();
        }
    }

    pub fn destroy(&self) {
        if !self.destroyed.get() {
            self.window.destroy();
        }
    }
}
