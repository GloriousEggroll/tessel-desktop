//! input_page.py: mouse and touchpad controls for the active labwc session.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gs_base::widgets::run_in_thread;
use gs_base::{Host, Setting, SettingsPage};
use gtk::glib;
use gtk::prelude::*;
use suite_core::tr;
use suite_gtk::utils::{accessible, add_css, remove_children, set_margins, PRIORITY_APPLICATION};

use crate::compat::casefold;
use crate::input_config::{keys, InputConfig, InputState, Updates};
use crate::input_devices::{discover, InputDevice};
use crate::mouse_polling::Polling;
use crate::mouse_polling_ui::PollingCard;

pub const SWITCHES: &[&str] = &["naturalScroll", "middleEmulation", "tap", "tapAndDrag", "disableWhileTyping"];
const BOOL: &[(Option<&str>, &str)] = &[(None, "Default"), (Some("yes"), "On"), (Some("no"), "Off")];

/// Map a GDK button number to the Linux input code labwc expects (BTN_LEFT = 272).
pub fn linux_button(gdk_button: u32) -> Option<u32> {
    match gdk_button {
        1 => Some(272),
        2 => Some(274),
        3 => Some(273),
        b if b >= 8 => Some(b + 267),
        _ => None,
    }
}

pub fn options(key: &str) -> &'static [(Option<&'static str>, &'static str)] {
    match key {
        "sendEventsMode" => &[(None, "Default"), (Some("yes"), "Enabled"), (Some("no"), "Disabled"),
                              (Some("disabledOnExternalMouse"), "Disable when a mouse is connected")],
        "accelProfile" => &[(None, "Default"), (Some("adaptive"), "Adaptive"), (Some("flat"), "Flat")],
        "leftHanded" => &[(None, "Default"), (Some("no"), "Left button"), (Some("yes"), "Right button")],
        "naturalScroll" | "middleEmulation" | "tap" | "tapAndDrag" | "disableWhileTyping" => BOOL,
        "scrollMethod" => &[(None, "Default"), (Some("twofinger"), "Two fingers"), (Some("edge"), "Edge"),
                            (Some("onbutton"), "Hold a button"), (Some("none"), "Disabled")],
        "clickMethod" => &[(None, "Default"), (Some("buttonAreas"), "Button areas"), (Some("clickfinger"), "Finger count"), (Some("none"), "Disabled")],
        "tapButtonMap" => &[(None, "Default (left / right / middle)"), (Some("lrm"), "Left / right / middle"), (Some("lmr"), "Left / middle / right")],
        "dragLock" => &[(None, "Default"), (Some("no"), "Off"), (Some("timeout"), "Release after a short pause"),
                        (Some("yes"), "Keep dragging until next tap")],
        "threeFingerDrag" => &[(None, "Default"), (Some("no"), "Off"), (Some("3"), "Three fingers"), (Some("4"), "Four fingers")],
        _ => &[],
    }
}

const CSS: &str = "
        .input-settings .input-detail { color: alpha(@theme_fg_color, .7); font-size: 9.5pt; }
        .input-settings .input-test { border: 1px dashed alpha(@theme_fg_color, .3);
            border-radius: 12px; padding: 20px; }
        .input-settings .input-test.capturing { border-color: @theme_selected_bg_color; }
        ";

/// “Press a button…”: records the next mouse button pressed on this control.
pub struct Capture {
    pub button: gtk::Button,
    listening: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    record: RefCell<Option<Box<dyn Fn(u32)>>>,
}

impl Capture {
    pub fn listening(&self) -> bool { self.listening.get() }

    pub fn record(&self, gdk_button: u32) {
        let record = self.record.borrow();
        if let Some(record) = record.as_ref() {
            record(gdk_button);
        }
    }

    fn stop(&self, text: Option<String>) {
        self.listening.set(false);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        self.button.remove_css_class("suggested-action");
        self.button.set_label(&text.unwrap_or_else(|| tr!("Press a button…")));
    }
}

/// The control of one setting (Python attached `.switch`, `.scale` … to the widget).
#[derive(Clone)]
pub enum Control {
    Switch { container: gtk::Box, switch: gtk::Switch, reset: gtk::Button },
    Choice(gtk::DropDown),
    Number { container: gtk::Box, scale: gtk::Scale, automatic: gtk::CheckButton },
    ScrollButton { container: gtk::Box, capture: Rc<Capture>, number: gtk::SpinButton, default: gtk::CheckButton },
}

impl Control {
    pub fn widget(&self) -> gtk::Widget {
        match self {
            Control::Switch { container, .. } | Control::Number { container, .. } | Control::ScrollButton { container, .. } => container.clone().upcast(),
            Control::Choice(d) => d.clone().upcast(),
        }
    }
    pub fn switch(&self) -> Option<&gtk::Switch> { if let Control::Switch { switch, .. } = self { Some(switch) } else { None } }
    pub fn reset(&self) -> Option<&gtk::Button> { if let Control::Switch { reset, .. } = self { Some(reset) } else { None } }
    pub fn dropdown(&self) -> Option<&gtk::DropDown> { if let Control::Choice(d) = self { Some(d) } else { None } }
    pub fn scale(&self) -> Option<&gtk::Scale> { if let Control::Number { scale, .. } = self { Some(scale) } else { None } }
    pub fn automatic(&self) -> Option<&gtk::CheckButton> { if let Control::Number { automatic, .. } = self { Some(automatic) } else { None } }
}

pub type DeviceLoader = Arc<dyn Fn() -> (Vec<InputDevice>, String) + Send + Sync>;
pub type Profile = (String, String);
type Changes = Vec<(String, Option<String>)>;

pub struct InputControls {
    pub widget: gtk::Box,
    pub backend: RefCell<Option<InputConfig>>,
    device_loader: DeviceLoader,
    polling_backend: Option<Arc<dyn Polling>>,
    pub polling_card: RefCell<Option<Rc<PollingCard>>>,
    /// The stable box the polling card is built in (search target `pollingRate`).
    pub polling_root: gtk::Box,
    pub state: RefCell<Option<InputState>>,
    pub devices: RefCell<Vec<InputDevice>>,
    /// `(name, kind, label)`.
    pub profiles: RefCell<Vec<(String, String, String)>>,
    /// `{(name, kind): changes | None}` in insertion order.
    pub pending: RefCell<Vec<(Profile, Option<Changes>)>>,
    pub selected: RefCell<Profile>,
    pub controls: RefCell<HashMap<String, Control>>,
    /// One row per setting, reused across renders (Python `setattr(self, key, row)`).
    pub rows: HashMap<String, gtk::Box>,
    pub closed: Rc<Cell<bool>>,
    pub busy: Cell<bool>,
    pub discovering: Cell<bool>,
    rendering: Cell<bool>,
    provider: gtk::CssProvider,
    pub selector: gtk::DropDown,
    pub refresh_button: gtk::Button,
    pub device_detail: gtk::Label,
    pub body: gtk::Box,
    pub scroll: gtk::ScrolledWindow,
    pub status: gtk::Label,
    pub shared_button: gtk::Button,
    pub reset_button: gtk::Button,
    pub discard_button: gtk::Button,
    pub apply_button: gtk::Button,
    pub test_notice: RefCell<Option<gtk::Label>>,
    pub discovery_detail: RefCell<String>,
    pub device: RefCell<InputDevice>,
    apply_done: RefCell<Option<Box<dyn FnOnce(bool)>>>,
}

fn detail_label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(if text.is_empty() { String::new() } else { tr!(text) }).xalign(0.0).wrap(true).max_width_chars(64).build();
    label.add_css_class("input-detail");
    label
}

impl InputControls {
    pub fn new(backend: Option<InputConfig>, device_loader: Option<DeviceLoader>, polling_backend: Option<Arc<dyn Polling>>) -> Rc<InputControls> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 16);
        widget.set_vexpand(true);
        widget.add_css_class("input-settings");
        set_margins(&widget, 22);
        // Title, card and heading sizes come from global_style.PAGE_CSS.
        let provider = add_css(CSS, None, PRIORITY_APPLICATION);
        widget.append(&gs_base::style::page_header(&tr!("Mouse & Touchpad"),
            Some(&tr!("Choose defaults for mice or touchpads, or customize an individual device. Apply saves your changes for future sessions.")),
            Some("input-mouse-symbolic")).root);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let selector = gtk::DropDown::builder().hexpand(true).build();
        accessible(&selector, &tr!("Input device"));
        toolbar.append(&selector);
        let refresh_button = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text(tr!("Refresh connected devices")).build();
        toolbar.append(&refresh_button);
        widget.append(&toolbar);
        let device_detail = detail_label("");
        widget.append(&device_detail);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&body));
        widget.append(&scroll);
        let status = detail_label("");
        status.set_selectable(true);
        widget.append(&status);
        let footer = gtk::Box::builder().spacing(10).halign(gtk::Align::End).build();
        let shared_button = gtk::Button::with_label(&tr!("Use shared defaults"));
        shared_button.set_tooltip_text(Some(&tr!("Remove this device override when you press Apply.")));
        footer.append(&shared_button);
        let reset_button = gtk::Button::with_label(&tr!("Use defaults"));
        reset_button.set_tooltip_text(Some(&tr!("Reset this profile to compositor and hardware defaults. Press Apply to save.")));
        footer.append(&reset_button);
        let discard_button = gtk::Button::with_label(&tr!("Discard changes"));
        footer.append(&discard_button);
        let apply_button = gtk::Button::with_label(&tr!("Apply"));
        apply_button.add_css_class("suggested-action");
        footer.append(&apply_button);
        widget.append(&footer);
        let polling_root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        polling_root.add_css_class("gs-card");
        let rows = keys().into_iter().map(|k| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            row.add_css_class("gs-row");
            (k.to_owned(), row)
        }).collect();
        let this = Rc::new(InputControls {
            widget, backend: RefCell::new(backend), device_loader: device_loader.unwrap_or_else(|| Arc::new(discover)), polling_backend,
            polling_card: RefCell::new(None), polling_root, state: RefCell::new(None), devices: Default::default(), profiles: Default::default(),
            pending: Default::default(), selected: RefCell::new(("non-touch".into(), "non-touch".into())), controls: Default::default(), rows,
            closed: Rc::new(Cell::new(false)), busy: Cell::new(false), discovering: Cell::new(false), rendering: Cell::new(false), provider,
            selector, refresh_button, device_detail, body, scroll, status, shared_button, reset_button, discard_button, apply_button,
            test_notice: RefCell::new(None), discovery_detail: Default::default(), device: Default::default(), apply_done: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        this.selector.connect_selected_notify(move |_| if let Some(t) = weak.upgrade() { t.selected_changed() });
        let weak = Rc::downgrade(&this);
        this.refresh_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.refresh_devices() });
        let weak = Rc::downgrade(&this);
        this.shared_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.shared() });
        let weak = Rc::downgrade(&this);
        this.reset_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.defaults() });
        let weak = Rc::downgrade(&this);
        this.discard_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.reload() });
        let weak = Rc::downgrade(&this);
        this.apply_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.apply(None) });
        this.reload();
        this.refresh_devices();
        this
    }

    /// `getattr(controller, key)` — the row of a setting.
    pub fn row(&self, key: &str) -> Option<&gtk::Box> { self.rows.get(key) }

    pub fn control(&self, key: &str) -> Option<Control> { self.controls.borrow().get(key).cloned() }

    pub fn polling_busy(&self) -> bool { self.polling_card.borrow().as_ref().is_some_and(|c| c.busy.get()) }

    fn selected(&self) -> Profile { self.selected.borrow().clone() }

    fn pending_of(&self, profile: &Profile) -> Option<Option<Changes>> {
        self.pending.borrow().iter().find(|(p, _)| p == profile).map(|(_, c)| c.clone())
    }

    pub fn has_pending(&self) -> bool { !self.pending.borrow().is_empty() }

    pub fn reload(self: &Rc<Self>) {
        if self.busy.get() || self.polling_busy() {
            return;
        }
        let result = (|| {
            if self.backend.borrow().is_none() {
                *self.backend.borrow_mut() = Some(InputConfig::new(None, None, None, None)?);
            }
            let backend = self.backend.borrow().clone().unwrap();
            backend.load()
        })();
        match result {
            Ok(state) => {
                *self.state.borrow_mut() = Some(state);
                self.pending.borrow_mut().clear();
                self.status.set_text("");
            }
            Err(error) => {
                *self.state.borrow_mut() = None;
                self.status.set_text(error.message());
            }
        }
        self.profiles_changed();
    }

    pub fn refresh_devices(self: &Rc<Self>) {
        if self.closed.get() || self.discovering.get() || self.polling_busy() {
            return;
        }
        self.discovering.set(true);
        self.refresh_button.set_sensitive(false);
        let loader = self.device_loader.clone();
        let weak = Rc::downgrade(self);
        run_in_thread(self.closed.clone(), move || Ok(loader()), move |result: Result<(Vec<InputDevice>, String), String>| {
            if let Some(t) = weak.upgrade() {
                let (devices, detail) = result.unwrap_or_else(|e| (Vec::new(), e));
                t.devices_ready(devices, detail);
            }
        });
    }

    fn devices_ready(self: &Rc<Self>, devices: Vec<InputDevice>, detail: String) {
        if self.closed.get() {
            return;
        }
        self.discovering.set(false);
        self.refresh_button.set_sensitive(!self.busy.get());
        *self.devices.borrow_mut() = devices;
        *self.discovery_detail.borrow_mut() = detail;
        self.profiles_changed();
    }

    fn profiles_changed(self: &Rc<Self>) {
        if self.polling_busy() {
            return;
        }
        self.rendering.set(true);
        let mut profiles = vec![("non-touch".to_owned(), "non-touch".to_owned(), tr!("Mouse defaults")),
                                ("touchpad".to_owned(), "touchpad".to_owned(), tr!("Touchpad defaults"))];
        let mut known: Vec<String> = ["default", "non-touch", "touchpad", "touch"].iter().map(|s| s.to_string()).collect();
        for device in self.devices.borrow().iter() {
            profiles.push((device.name.clone(), device.kind.clone(), device.name.clone()));
            known.push(casefold(&device.name));
        }
        if let Some(state) = self.state.borrow().as_ref() {
            for (key, profile) in &state.profiles {
                if !known.contains(key) {
                    let touch = ["tap", "tapAndDrag", "clickMethod"].iter().any(|k| profile.get(k).is_some());
                    profiles.push((profile.name.clone(), if touch { "touchpad" } else { "non-touch" }.into(),
                                   tr!("{value1} (not connected)", value1 = profile.name.clone())));
                }
            }
        }
        let selected = self.selected();
        let index = profiles.iter().position(|(n, k, _)| (n, k) == (&selected.0, &selected.1)).unwrap_or(0);
        let names: Vec<&str> = profiles.iter().map(|(_, _, l)| l.as_str()).collect();
        let model = gtk::StringList::new(&names);
        *self.profiles.borrow_mut() = profiles;
        self.selector.set_model(Some(&model));
        self.selector.set_selected(index as u32);
        self.rendering.set(false);
        self.selected_changed();
    }

    fn selected_changed(self: &Rc<Self>) {
        if self.rendering.get() || self.profiles.borrow().is_empty() {
            return;
        }
        let index = self.selector.selected() as usize;
        let Some((name, kind, _)) = self.profiles.borrow().get(index).cloned() else { return };
        *self.selected.borrow_mut() = (name, kind);
        self.render();
    }

    /// The values in effect for the selected profile, with pending edits.
    pub fn values(&self) -> Vec<(String, String)> {
        let state = self.state.borrow();
        let Some(state) = state.as_ref() else { return Vec::new() };
        let selected = self.selected();
        match self.pending_of(&selected) {
            Some(None) => {
                let without = InputState { profiles: state.profiles.iter().filter(|(k, _)| *k != casefold(&selected.0)).cloned().collect() };
                without.match_profile(&selected.0, &selected.1).values
            }
            pending => {
                let mut values = state.match_profile(&selected.0, &selected.1).values;
                for (key, value) in pending.flatten().unwrap_or_default() {
                    match value {
                        None => values.retain(|(k, _)| *k != key),
                        Some(v) => {
                            if let Some(slot) = values.iter_mut().find(|(k, _)| *k == key) { slot.1 = v } else { values.push((key, v)) }
                        }
                    }
                }
                values
            }
        }
    }

    pub fn value(&self, key: &str) -> Option<String> { self.values().into_iter().find(|(k, _)| k == key).map(|(_, v)| v) }

    pub fn changed(&self, key: &str, value: Option<String>) {
        if self.rendering.get() {
            return;
        }
        let selected = self.selected();
        let original = {
            let state = self.state.borrow();
            let Some(state) = state.as_ref() else { return };
            state.match_profile(&selected.0, &selected.1).get(key).map(str::to_owned)
        };
        {
            let mut pending = self.pending.borrow_mut();
            let index = match pending.iter().position(|(p, _)| *p == selected) {
                Some(index) => index,
                None => {
                    pending.push((selected.clone(), Some(Vec::new())));
                    pending.len() - 1
                }
            };
            let Some(changes) = pending[index].1.as_mut() else { return };
            if value == original {
                changes.retain(|(k, _)| k != key);
            } else if let Some(slot) = changes.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value;
            } else {
                changes.push((key.to_owned(), value));
            }
            if changes.is_empty() {
                pending.remove(index);
            }
        }
        self.buttons();
        self.status.set_text(&if self.has_pending() { tr!("Changes are not saved. Press Apply to use them.") } else { String::new() });
    }

    pub fn buttons(&self) {
        let working = self.busy.get() || self.polling_busy();
        let enabled = self.state.borrow().is_some() && !working;
        let selected = self.selected();
        let removing = matches!(self.pending_of(&selected), Some(None));
        let pending = self.has_pending();
        self.apply_button.set_sensitive(enabled && pending);
        self.discard_button.set_sensitive(enabled && pending);
        self.reset_button.set_sensitive(enabled && !removing);
        self.body.set_sensitive(enabled && !removing);
        let override_shown = self.state.borrow().as_ref().is_some_and(|s| !["non-touch", "touchpad"].contains(&selected.0.as_str())
            && s.contains(&casefold(&selected.0)));
        self.shared_button.set_visible(override_shown);
        self.shared_button.set_sensitive(enabled && !removing);
        if let Some(notice) = self.test_notice.borrow().as_ref() {
            notice.set_visible(pending);
        }
        self.selector.set_sensitive(!working);
        self.refresh_button.set_sensitive(!working && !self.discovering.get());
    }

    fn card(&self, title: &str, detail: &str) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("gs-card");
        let label = gtk::Label::builder().label(tr!(title)).xalign(0.0).build();
        label.add_css_class("gs-section-title");
        card.append(&label);
        if !detail.is_empty() {
            card.append(&detail_label(detail));
        }
        self.body.append(&card);
        card
    }

    fn add_row(&self, card: &gtk::Box, key: &str, title: &str, detail: &str, control: Control, note: Option<&str>) {
        let row = self.rows[key].clone();
        if let Some(parent) = row.parent().and_downcast::<gtk::Box>() {
            parent.remove(&row);
        }
        remove_children(&row);
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
        let label = gtk::Label::builder().label(tr!(title)).xalign(0.0).wrap(true).max_width_chars(28).build();
        label.add_css_class("gs-row-title");
        text.append(&label);
        if !detail.is_empty() {
            text.append(&detail_label(detail));
        }
        if let Some(note) = note {
            text.append(&detail_label(note));
        }
        row.append(&text);
        let widget = control.widget();
        widget.set_valign(gtk::Align::Center);
        accessible(&widget, &tr!(title));
        row.append(&widget);
        card.append(&row);
        let supported = self.device.borrow().supports(key);
        widget.set_sensitive(supported);
        if !supported {
            widget.set_tooltip_text(Some(&tr!("Not supported by this device.")));
        }
        self.controls.borrow_mut().insert(key.to_owned(), control);
    }

    fn switch_row(self: &Rc<Self>, card: &gtk::Box, key: &'static str, title: &str, detail: &str) {
        let value = self.value(key);
        let default = self.device.borrow().default_enabled(key);
        let effective = match &value { Some(v) => v == "yes", None => default.unwrap_or(false) };
        let control = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let reset = gtk::Button::builder().icon_name("edit-undo-symbolic").visible(value.is_some()).build();
        reset.add_css_class("flat");
        reset.set_tooltip_text(Some(&match default {
            Some(on) => tr!("Use the device default ({state})", state = if on { tr!("On") } else { tr!("Off") }),
            None => tr!("Use the device default"),
        }));
        accessible(&reset, &tr!("Use the device default"));
        let switch = gtk::Switch::builder().active(effective).valign(gtk::Align::Center).build();
        let weak = Rc::downgrade(self);
        let reset_button = reset.clone();
        switch.connect_active_notify(move |w| if let Some(t) = weak.upgrade() {
            if t.rendering.get() {
                return;
            }
            t.changed(key, Some(if w.is_active() { "yes" } else { "no" }.into()));
            reset_button.set_visible(true);
        });
        let weak = Rc::downgrade(self);
        reset.connect_clicked(move |_| if let Some(t) = weak.upgrade() {
            t.changed(key, None);
            t.render();
        });
        control.append(&reset);
        control.append(&switch);
        self.add_row(card, key, title, detail, Control::Switch { container: control, switch: switch.clone(), reset },
                     if value.is_none() { Some("Using the device default.") } else { None });
        accessible(&switch, &tr!(title));
    }

    fn choice(self: &Rc<Self>, card: &gtk::Box, key: &'static str, title: &str, detail: &str) {
        if SWITCHES.contains(&key) {
            return self.switch_row(card, key, title, detail);
        }
        let device = self.device.borrow().clone();
        let mut items: Vec<(Option<String>, String)> = options(key).iter().filter(|(v, _)| device.allowed(key, *v))
            .map(|(v, l)| (v.map(str::to_owned), tr!(*l))).collect();
        if key == "sendEventsMode" && self.selected().1 != "touchpad" {
            items.retain(|(v, _)| v.as_deref() != Some("disabledOnExternalMouse"));
        }
        let value = self.value(key);
        if let Some(v) = &value {
            if !items.iter().any(|(item, _)| item.as_ref() == Some(v)) {
                items.push((Some(v.clone()), tr!("Configured: {value1}", value1 = v.clone())));
            }
        }
        let names: Vec<&str> = items.iter().map(|(_, l)| l.as_str()).collect();
        let control = gtk::DropDown::from_strings(&names);
        control.set_selected(items.iter().position(|(v, _)| *v == value).unwrap_or(0) as u32);
        let weak = Rc::downgrade(self);
        control.connect_selected_notify(move |w| if let Some(t) = weak.upgrade() {
            if let Some((v, _)) = items.get(w.selected() as usize) {
                t.changed(key, v.clone());
            }
        });
        self.add_row(card, key, title, detail, Control::Choice(control), None);
    }

    #[allow(clippy::too_many_arguments)]
    fn number(self: &Rc<Self>, card: &gtk::Box, key: &'static str, title: &str, detail: &str, low: f64, high: f64, step: f64, default: f64, digits: usize) {
        let control = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let value = self.value(key);
        let number = value.as_deref().and_then(|v| crate::compat::py_float(v).ok()).unwrap_or(default);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, low, high, step);
        scale.set_size_request(180, -1);
        scale.set_digits(digits as i32);
        scale.set_value(number);
        scale.add_mark(default, gtk::PositionType::Bottom, None);
        scale.set_sensitive(value.is_some());
        accessible(&scale, &tr!(title));
        control.append(&scale);
        let automatic = gtk::CheckButton::with_label(&tr!("Use default"));
        automatic.set_active(value.is_none());
        let (weak, s) = (Rc::downgrade(self), scale.clone());
        automatic.connect_toggled(move |w| if let Some(t) = weak.upgrade() {
            s.set_sensitive(!w.is_active());
            t.changed(key, if w.is_active() { None } else { Some(format!("{:.*}", digits, s.value())) });
        });
        let (weak, a) = (Rc::downgrade(self), automatic.clone());
        scale.connect_value_changed(move |w| if let Some(t) = weak.upgrade() {
            if !a.is_active() {
                t.changed(key, Some(format!("{:.*}", digits, w.value())));
            }
        });
        control.append(&automatic);
        self.add_row(card, key, title, detail, Control::Number { container: control, scale, automatic }, None);
    }

    pub fn render(self: &Rc<Self>) {
        if self.polling_busy() {
            return;
        }
        if let Some(card) = self.polling_card.borrow_mut().take() {
            card.close();
        }
        self.rendering.set(true);
        remove_children(&self.body);
        self.controls.borrow_mut().clear();
        let (name, kind) = self.selected();
        let device = self.devices.borrow().iter().find(|d| casefold(&d.name) == casefold(&name)).cloned()
            .unwrap_or_else(|| InputDevice::new(&name, &kind));
        *self.device.borrow_mut() = device.clone();
        let description = if name == "non-touch" || name == "touchpad" {
            tr!("Applies to devices without their own custom profile. Individual device profiles take priority.")
        } else {
            let profile = self.state.borrow().as_ref().map(|s| s.match_profile(&name, &kind));
            let mut description = if device.paths.is_empty() { tr!("Device not connected") } else { device.paths.join(" · ") };
            if profile.is_some_and(|p| casefold(&p.name) != casefold(&name)) {
                description += &format!("\n{}", tr!("Currently using shared defaults. Your first edit creates a separate profile for this device."));
            }
            description
        };
        self.device_detail.set_text(&description);
        let pointer = self.card("Pointer", "");
        if name != "non-touch" {
            self.choice(&pointer, "sendEventsMode", "Device state", "Enable or disable input from this device.");
        } else {
            pointer.append(&detail_label("To disable a mouse, select the individual device above."));
        }
        self.number(&pointer, "pointerSpeed", "Pointer speed", "Slow to fast; zero is the normal speed.", -1.0, 1.0, 0.05, 0.0, 2);
        self.choice(&pointer, "accelProfile", "Acceleration", "Adaptive responds to movement speed. Flat uses a constant multiplier.");
        if kind != "touchpad" {
            let weak: Weak<InputControls> = Rc::downgrade(self);
            let busy_changed: Rc<dyn Fn()> = Rc::new(move || if let Some(t) = weak.upgrade() { t.buttons() });
            let card = PollingCard::with_root(self.polling_root.clone(), if name == "non-touch" { None } else { Some(device.clone()) },
                                              self.polling_backend.clone(), busy_changed);
            self.body.append(&self.polling_root);
            *self.polling_card.borrow_mut() = Some(card);
        }
        let buttons = self.card("Buttons", "");
        self.choice(&buttons, "leftHanded", "Primary button", "Swap the primary and secondary buttons for left-handed use.");
        self.choice(&buttons, "middleEmulation", "Emulate middle click", "Press the left and right buttons together to middle-click.");
        let scroll = self.card("Scrolling", "");
        self.choice(&scroll, "naturalScroll", "Natural scrolling", "Move content in the same direction as your fingers.");
        self.number(&scroll, "scrollFactor", "Scroll speed", "Multiplier for wheel and touchpad scrolling; 1 is normal.", 0.1, 5.0, 0.1, 1.0, 1);
        self.choice(&scroll, "scrollMethod", "Scroll method", "Use two fingers, the touchpad edge, or a held button.");
        if kind != "touchpad" {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let number = gtk::SpinButton::with_range(1.0, 767.0, 1.0);
            number.set_value(self.value("scrollButton").and_then(|v| crate::compat::py_float(&v).ok()).unwrap_or(274.0));
            let default = gtk::CheckButton::with_label(&tr!("Use default"));
            default.set_active(self.value("scrollButton").is_none());
            number.set_sensitive(!default.is_active());
            let (weak, n) = (Rc::downgrade(self), number.clone());
            default.connect_toggled(move |w| if let Some(t) = weak.upgrade() {
                n.set_sensitive(!w.is_active());
                t.changed("scrollButton", if w.is_active() { None } else { Some(n.value_as_int().to_string()) });
            });
            let (weak, d) = (Rc::downgrade(self), default.clone());
            number.connect_value_changed(move |w| if let Some(t) = weak.upgrade() {
                if !d.is_active() {
                    t.changed("scrollButton", Some(w.value_as_int().to_string()));
                }
            });
            let capture = self.button_capture(&number, &default);
            row.append(&capture.button);
            row.append(&number);
            row.append(&default);
            self.add_row(&scroll, "scrollButton", "Scroll button",
                "Hold this button and move the mouse to scroll. Used with Hold a button. Press “Press a button…”, then press the mouse button you want.",
                Control::ScrollButton { container: row, capture, number, default }, None);
        }
        if kind == "touchpad" {
            let taps = self.card("Taps & click areas", "Tapping is enabled by default in labwc. Options you have not changed use the device or compositor default.");
            self.choice(&taps, "tap", "Tap to click", "Tap with one finger to click; two and three fingers use the button map below.");
            self.choice(&taps, "tapButtonMap", "One / two / three-finger taps", "Choose which mouse button each finger count represents.");
            self.choice(&taps, "clickMethod", "Physical click method", "Button areas uses the bottom left/right regions. Finger count uses one finger for left-click, two for right-click, and three for middle-click.");
            taps.append(&detail_label("Button-area sizes are defined by the device and libinput; they are not custom screen coordinates."));
            let gestures = self.card("Drag gestures & palm protection", "");
            self.choice(&gestures, "tapAndDrag", "Tap and drag", "Tap, then touch again and move to drag an item.");
            self.choice(&gestures, "dragLock", "Drag lock", "Continue a tap-and-drag while repositioning your fingers.");
            self.choice(&gestures, "threeFingerDrag", "Multi-finger drag", "Drag with three or four fingers. Uses those fingers for dragging instead of swipe gestures; requires libinput 1.28 or newer.");
            self.choice(&gestures, "disableWhileTyping", "Disable while typing", "Ignore accidental touchpad movement while typing. Palm rejection is handled automatically.");
            gestures.append(&detail_label("Pinch-to-zoom and swipe gestures are passed to applications that support them."));
        }
        self.test_area();
        let detail = self.discovery_detail.borrow().clone();
        if !detail.is_empty() {
            self.body.append(&detail_label(&detail));
        }
        self.rendering.set(false);
        self.buttons();
    }

    fn button_capture(self: &Rc<Self>, number: &gtk::SpinButton, default: &gtk::CheckButton) -> Rc<Capture> {
        let button = gtk::Button::with_label(&tr!("Press a button…"));
        button.set_tooltip_text(Some(&tr!("Click here, then press the mouse button to use for scrolling.")));
        let capture = Rc::new(Capture { button, listening: Cell::new(false), timer: RefCell::new(None), record: RefCell::new(None) });
        let (weak, c, n, d) = (Rc::downgrade(self), Rc::downgrade(&capture), number.clone(), default.clone());
        *capture.record.borrow_mut() = Some(Box::new(move |gdk_button| {
            let Some(capture) = c.upgrade() else { return };
            let Some(code) = linux_button(gdk_button) else {
                capture.stop(Some(tr!("That button cannot be used. Try another.")));
                return;
            };
            d.set_active(false);
            n.set_value(code as f64);
            if let Some(t) = weak.upgrade() {
                t.changed("scrollButton", Some(code.to_string()));
            }
            capture.stop(Some(tr!("Button {code} selected", code = code as i64)));
        }));
        let c = Rc::downgrade(&capture);
        capture.button.connect_clicked(move |button| {
            let Some(capture) = c.upgrade() else { return };
            capture.listening.set(true);
            button.add_css_class("suggested-action");
            button.set_label(&tr!("Press the mouse button now…"));
            let c2 = Rc::downgrade(&capture);
            let id = glib::timeout_add_seconds_local_once(8, move || if let Some(capture) = c2.upgrade() {
                capture.timer.borrow_mut().take();
                capture.stop(None);
            });
            *capture.timer.borrow_mut() = Some(id);
        });
        let gesture = gtk::GestureClick::builder().button(0).build();
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let c = Rc::downgrade(&capture);
        gesture.connect_pressed(move |gesture, _, _, _| {
            let Some(capture) = c.upgrade() else { return };
            if !capture.listening.get() {
                return;
            }
            // Claimed in the capture phase, so the press does not also click.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            capture.record(gesture.current_button());
        });
        capture.button.add_controller(gesture);
        capture
    }

    fn test_area(&self) {
        let card = self.card("Try your settings",
            "This area uses the settings currently in effect. Unapplied changes are not active yet: press Apply, then click, scroll, or pinch here to try them.");
        let notice = detail_label("You have changes that are not applied yet, so this area still uses your previous settings.");
        notice.set_visible(self.has_pending());
        card.append(&notice);
        *self.test_notice.borrow_mut() = Some(notice);
        let area = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Fill).build();
        area.add_css_class("input-test");
        area.set_size_request(-1, 90);
        let output = gtk::Label::builder().label(tr!("Click, scroll, or pinch here")).wrap(true).build();
        area.append(&output);
        let click = gtk::GestureClick::builder().button(0).build();
        let o = output.clone();
        click.connect_pressed(move |w, count, _, _| o.set_text(&tr!("Button {value1} · {value2} click(s)", value1 = w.current_button() as i64, value2 = count as i64)));
        area.add_controller(click);
        let scrolling = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let o = output.clone();
        scrolling.connect_scroll(move |_, dx, dy| {
            o.set_text(&tr!("Scroll: horizontal {value1}, vertical {value2}", value1 = format!("{dx:.2}"), value2 = format!("{dy:.2}")));
            glib::Propagation::Stop
        });
        area.add_controller(scrolling);
        let zoom = gtk::GestureZoom::new();
        let o = output.clone();
        zoom.connect_scale_changed(move |_, value| o.set_text(&tr!("Pinch zoom: {value1}×", value1 = format!("{value:.2}"))));
        area.add_controller(zoom);
        card.append(&area);
    }

    pub fn defaults(self: &Rc<Self>) {
        for key in keys() {
            self.changed(key, None);
        }
        self.render();
    }

    pub fn shared(self: &Rc<Self>) {
        let selected = self.selected();
        {
            let mut pending = self.pending.borrow_mut();
            match pending.iter_mut().find(|(p, _)| *p == selected) {
                Some(slot) => slot.1 = None,
                None => pending.push((selected, None)),
            }
        }
        self.render();
        self.status.set_text(&tr!("Press Apply to remove this device override and use shared defaults."));
    }

    /// `_apply(done=…)`.
    pub fn apply(self: &Rc<Self>, done: Option<Box<dyn FnOnce(bool)>>) {
        if self.busy.get() || self.polling_busy() || !self.has_pending() || self.state.borrow().is_none() {
            if let Some(done) = done {
                done(!self.has_pending());
            }
            return;
        }
        *self.apply_done.borrow_mut() = done;
        self.busy.set(true);
        self.refresh_button.set_sensitive(false);
        self.buttons();
        let updates: Updates = self.pending.borrow().clone();
        let expected = self.state.borrow().clone().unwrap();
        self.status.set_text(&tr!("Applying input settings…"));
        let backend = self.backend.borrow().clone();
        let weak = Rc::downgrade(self);
        run_in_thread(self.closed.clone(), move || {
            let backend = backend.ok_or_else(|| "Input settings require a running labwc session.".to_string())?;
            backend.save(&updates, &expected).map_err(|e| e.to_string())
        }, move |result| if let Some(t) = weak.upgrade() { t.applied(result) });
    }

    fn applied(self: &Rc<Self>, result: Result<InputState, String>) {
        if self.closed.get() {
            return;
        }
        self.busy.set(false);
        self.refresh_button.set_sensitive(!self.discovering.get());
        let okay = result.is_ok();
        match result {
            Err(error) => {
                self.status.set_text(&error);
                self.buttons();
            }
            Ok(state) => {
                *self.state.borrow_mut() = Some(state);
                self.pending.borrow_mut().clear();
                self.render();
                self.status.set_text(&tr!("Input settings applied and saved."));
            }
        }
        if let Some(done) = self.apply_done.borrow_mut().take() {
            done(okay);
        }
    }

    /// Ask before closing with unapplied edits: Apply, Discard or Cancel.
    pub fn can_close(self: &Rc<Self>, close_host: Option<Rc<dyn Fn()>>) -> bool {
        if self.busy.get() || self.polling_busy() {
            return false;
        }
        let Some(close_host) = close_host.filter(|_| self.has_pending()) else { return true };
        let (apply, discard) = (Rc::downgrade(self), Rc::downgrade(self));
        !gs_base::style::confirm_leave(&self.widget, &tr!("Your mouse and touchpad changes have not been applied."),
            move |done| if let Some(t) = apply.upgrade() { t.apply(Some(done)) },
            move || if let Some(t) = discard.upgrade() { t.reload() }, close_host)
    }

    pub fn activate(self: &Rc<Self>) {
        if !self.has_pending() && !self.busy.get() && !self.polling_busy() {
            self.reload();
        }
        self.refresh_devices();
    }

    pub fn reveal_setting(&self, setting: &Setting) {
        let selected = self.selected();
        if setting.target == "pollingRate" && (selected.0 == "non-touch" || selected.1 == "touchpad") {
            let index = self.profiles.borrow().iter().position(|(n, k, _)| k == "non-touch" && n != "non-touch").unwrap_or(0);
            self.selector.set_selected(index as u32);
        }
        let selected = self.selected();
        if ["tap", "tapButtonMap", "clickMethod", "tapAndDrag", "dragLock", "threeFingerDrag", "disableWhileTyping"].contains(&setting.target)
            && selected.1 != "touchpad" {
            if let Some(index) = self.profiles.borrow().iter().position(|(n, _, _)| n == "touchpad") {
                self.selector.set_selected(index as u32);
            }
        }
    }

    pub fn close(&self) {
        self.closed.set(true);
        if let Some(card) = self.polling_card.borrow().as_ref() {
            card.close();
        }
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.widget), &self.provider);
    }
}

pub fn build_page_with(host: &Host, backend: Option<InputConfig>, loader: Option<DeviceLoader>, polling: Option<Arc<dyn Polling>>) -> SettingsPage {
    let controller = InputControls::new(backend, loader, polling);
    let (close, activate, can_close, reveal) = (Rc::downgrade(&controller), Rc::downgrade(&controller), Rc::downgrade(&controller),
                                                Rc::downgrade(&controller));
    let window = host.window();
    let page = SettingsPage::new(controller.widget.clone())
        .with_close(move || if let Some(c) = close.upgrade() { c.close() })
        .with_activate(move || if let Some(c) = activate.upgrade() { c.activate() })
        .with_can_close(move || {
            let window = window.clone();
            can_close.upgrade().map(|c| c.can_close(Some(Rc::new(move || window.close())))).unwrap_or(true)
        });
    *page.reveal_setting.borrow_mut() = Some(Rc::new(move |setting: &Setting| if let Some(c) = reveal.upgrade() { c.reveal_setting(setting) }));
    for (key, row) in &controller.rows {
        page.add_target(key, row);
    }
    page.add_target("pollingRate", &controller.polling_root);
    page.set_controller(controller);
    page
}
