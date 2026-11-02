//! Touch keys inside the lock surface (osk_lock.py). Secrets never enter the
//! input helper: the keyboard edits the lock's GtkEntry directly and only the
//! helper's offline `--labels` mode is used (no Wayland connection).
use crate::config::{layout_name, Config, Settings};
use crate::devices::{automatic_available, discover, Device, DeviceMonitor};
use crate::keyboard::{level, Keyboard, Labels};
use gtk::prelude::*;
use gtk::{gdk, glib};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use tessel_gtk::gtk;
use unicode_normalization::UnicodeNormalization;

thread_local! {
    static CACHE: RefCell<VecDeque<(PathBuf, String, Value)>> = const { RefCell::new(VecDeque::new()) };
}

/// `tessel-keyboard-input --labels <layout>` (2 s timeout, LRU cache of 8). `{}` on failure.
pub fn labels(helper: &Path, layout: &str) -> Value {
    if let Some(hit) = CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let index = cache.iter().position(|(h, l, _)| h == helper && l == layout)?;
        let entry = cache.remove(index)?;
        let value = entry.2.clone();
        cache.push_back(entry);
        Some(value)
    }) {
        return hit;
    }
    let Some(value) = run_labels(helper, layout) else { return Value::Object(Default::default()) };
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.len() >= 8 {
            cache.pop_front();
        }
        cache.push_back((helper.to_path_buf(), layout.to_string(), value.clone()));
    });
    value
}

fn run_labels(helper: &Path, layout: &str) -> Option<Value> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(helper).args(["--labels", layout]).stdin(Stdio::null()).stdout(Stdio::piped())
        .stderr(Stdio::piped()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).map(|_| text)
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let text = reader.join().ok()?.ok()?;
    if !status.success() {
        return None;
    }
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
}

/// Combining mark for an xkb dead-key name.
pub fn combining(dead: &str) -> Option<char> {
    Some(match dead {
        "dead_grave" => '\u{300}', "dead_acute" => '\u{301}', "dead_circumflex" => '\u{302}', "dead_tilde" => '\u{303}',
        "dead_macron" => '\u{304}', "dead_breve" => '\u{306}', "dead_abovedot" => '\u{307}', "dead_diaeresis" => '\u{308}',
        "dead_abovering" => '\u{30a}', "dead_doubleacute" => '\u{30b}', "dead_caron" => '\u{30c}', "dead_cedilla" => '\u{327}',
        "dead_ogonek" => '\u{328}', _ => return None,
    })
}

#[derive(Debug, Default, Clone)]
pub struct Compose {
    pub pending_accent: Option<char>,
    pub spacing_accent: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    Submit,
    Hide,
}

/// Apply one keyboard command to the lock entry (osk_lock.LockKeyboard.key after
/// its visibility/focus checks). Enter submits, Esc hides, arrows/Home/End move,
/// Ctrl+A selects all, other Ctrl/Alt/Super are ignored (no clipboard), dead keys
/// compose through combining marks + NFC and dead+dead yields the spacing accent.
pub fn apply_key(entry: &impl IsA<gtk::Editable>, labels: &Labels, dead: &Value, compose: &mut Compose, kind: &str, value: u32,
                 flags: u32) -> Outcome {
    let entry = entry.as_ref();
    let mut text: String;
    if kind == "text" {
        text = char::from_u32(value).map(String::from).unwrap_or_default();
    } else if value == 28 {
        return Outcome::Submit;
    } else if value == 1 {
        return Outcome::Hide;
    } else if matches!(value, 105 | 106 | 102 | 107) {
        let position = entry.position();
        entry.set_position(match value { 102 => 0, 107 => -1, 105 => (position - 1).max(0), _ => (position + 1).max(0) });
        return Outcome::None;
    } else if value == 30 && flags & 2 != 0 {
        entry.select_region(0, -1);
        return Outcome::None;
    } else if flags & (2 | 4 | 8) != 0 {
        return Outcome::None;
    } else if value == 14 || value == 111 {
        if let Some((start, end)) = entry.selection_bounds() {
            entry.delete_text(start, end);
        } else if value == 14 && entry.position() > 0 {
            let position = entry.position();
            entry.delete_text(position - 1, position);
            entry.set_position(position - 1);
        } else if value == 111 {
            let position = entry.position();
            entry.delete_text(position, position + 1);
        }
        compose.pending_accent = None;
        return Outcome::None;
    } else {
        let level = level(flags);
        let empty: [String; 4] = Default::default();
        let alternatives = labels.get(&value.to_string()).unwrap_or(&empty);
        text = if alternatives[level].is_empty() { alternatives[0].clone() } else { alternatives[level].clone() };
        let dead_name = dead.get(value.to_string()).and_then(|v| v.get(level)).and_then(Value::as_str).unwrap_or("");
        let mark = combining(dead_name);
        if mark.is_some() && compose.pending_accent.is_none() {
            compose.pending_accent = mark;
            compose.spacing_accent = text;
            return Outcome::None;
        }
        if mark.is_some() && mark == compose.pending_accent {
            text = " ".into();
        }
    }
    if text.is_empty() {
        return Outcome::None;
    }
    if let Some(mark) = compose.pending_accent.take() {
        text = if text == " " { compose.spacing_accent.clone() } else { format!("{text}{mark}").nfc().collect() };
    }
    if let Some((start, end)) = entry.selection_bounds() {
        entry.delete_text(start, end);
        entry.set_position(start);
    }
    let mut position = entry.position();
    entry.insert_text(&text, &mut position);
    entry.set_position(position);
    Outcome::None
}

/// What the lock screen provides (its window/owner in the Python version).
pub struct LockHost {
    /// The entry the keys edit (the selected lock field).
    pub entry: Rc<dyn Fn() -> gtk::Entry>,
    pub submit: Rc<dyn Fn()>,
    /// `window.owner.login_visible` (or the custom `is_visible`).
    pub is_visible: Rc<dyn Fn() -> bool>,
    /// Records activity and focuses the field (`focus`, else `entry.grab_focus_without_selecting()`).
    pub focus: Rc<dyn Fn()>,
    pub overlay: gtk::Overlay,
    pub scroll: gtk::Widget,
    pub monitor: Option<gdk::Monitor>,
    pub button_box: gtk::Box,
    pub size_group: Option<gtk::SizeGroup>,
    pub button_class: String,
    pub scale_factor: i32,
    pub direction: gtk::TextDirection,
    pub helper: Option<PathBuf>,
    pub store: Option<Config>,
    /// false in tests: no udev monitor.
    pub watch_devices: bool,
}

struct LockInner {
    host: LockHost,
    button: gtk::Button,
    widget: RefCell<Option<Keyboard>>,
    dead: RefCell<Value>,
    compose: RefCell<Compose>,
    dismissed: std::cell::Cell<bool>,
    available: std::cell::Cell<bool>,
    monitor: RefCell<Option<DeviceMonitor>>,
}

#[derive(Clone)]
pub struct LockKeyboard(Rc<LockInner>);

impl LockKeyboard {
    pub fn new(host: LockHost) -> Self {
        let icon = gtk::Image::builder().pixel_size(20).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        let standard = gtk::IconTheme::new();
        standard.set_theme_name(Some("Adwaita"));
        icon.set_paintable(Some(&standard.lookup_icon("input-keyboard-symbolic", &[], 20, host.scale_factor.max(1), host.direction,
                                                      gtk::IconLookupFlags::FORCE_SYMBOLIC)));
        let button = gtk::Button::builder().child(&icon).focusable(false).tooltip_text("Show keyboard").halign(gtk::Align::Center)
            .valign(gtk::Align::Center).build();
        button.set_has_tooltip(false);
        button.add_css_class(&host.button_class);
        button.set_size_request(44, 44);
        button.update_property(&[gtk::accessible::Property::Label("Show keyboard")]);
        host.button_box.append(&button);
        if let Some(group) = &host.size_group {
            group.add_widget(&button);
        }
        let watch = host.watch_devices;
        let keyboard = Self(Rc::new(LockInner {
            host, button: button.clone(), widget: RefCell::new(None), dead: RefCell::new(Value::Object(Default::default())),
            compose: RefCell::new(Compose::default()), dismissed: false.into(), available: false.into(), monitor: RefCell::new(None) }));
        let weak = Rc::downgrade(&keyboard.0);
        button.connect_clicked(move |_| if let Some(k) = up(&weak) { k.toggle() });
        if watch {
            let weak = Rc::downgrade(&keyboard.0);
            *keyboard.0.monitor.borrow_mut() = Some(DeviceMonitor::new(move |d| if let Some(k) = up(&weak) { k.devices_changed(&d) }));
            keyboard.devices_changed(&discover());
        }
        keyboard
    }

    pub fn button(&self) -> &gtk::Button { &self.0.button }
    pub fn widget(&self) -> Option<Keyboard> { self.0.widget.borrow().clone() }
    pub fn available(&self) -> bool { self.0.available.get() }
    fn store(&self) -> Config { self.0.host.store.clone().unwrap_or_default() }

    pub fn devices_changed(&self, devices: &[Device]) {
        let config = self.store().load().unwrap_or(Settings { automatic: false, ignore_keyboards: vec![], ..Settings::default() });
        let previous = self.0.available.get();
        let available = automatic_available(devices, &config);
        self.0.available.set(available);
        if previous && !available {
            self.hide();
        } else if available && !previous {
            self.0.dismissed.set(false);
            self.reveal((self.0.host.is_visible)());
        }
    }

    fn ensure(&self) {
        if self.0.widget.borrow().is_some() {
            return;
        }
        let config = self.store().load().unwrap_or(Settings { layout: "us".into(), size: "medium".into(), ..Settings::default() });
        let weak = Rc::downgrade(&self.0);
        let hide_weak = weak.clone();
        let widget = Keyboard::new(move |kind, value, flags| if let Some(k) = up(&weak) { k.key(kind, value, flags) },
                                   move || if let Some(k) = up(&hide_weak) { k.hide() }, None, true);
        let helper = self.0.host.helper.clone().unwrap_or_else(crate::backend::helper_path);
        let layout = labels(&helper, &layout_name(&config, None));
        *self.0.dead.borrow_mut() = layout.get("dead").cloned().unwrap_or(Value::Object(Default::default()));
        widget.set_labels(layout.get("keys").unwrap_or(&Value::Object(Default::default())));
        widget.set_size(&config.size);
        let width = self.0.host.monitor.as_ref().map_or(1280, |m| m.geometry().width());
        widget.root().set_size_request(1050.min(300.max(width - 24)), -1);
        if width < 724 {
            widget.root().add_css_class("keyboard-narrow");
        }
        widget.root().set_halign(gtk::Align::Center);
        widget.root().set_valign(gtk::Align::End);
        widget.root().set_margin_bottom(10);
        let weak = Rc::downgrade(&self.0);
        widget.root().connect_notify_local(Some("height"), move |_, _| if let Some(k) = up(&weak) { k.reserve() });
        self.0.host.overlay.add_overlay(widget.root());
        *self.0.widget.borrow_mut() = Some(widget);
    }

    fn reserve(&self) {
        let widget = self.widget();
        match widget.filter(|w| w.root().get_visible()) {
            Some(widget) => {
                let (minimum, natural, _, _) = widget.root().measure(gtk::Orientation::Vertical, -1);
                self.0.host.scroll.set_margin_bottom(minimum.max(natural) + 16);
            }
            None => self.0.host.scroll.set_margin_bottom(0),
        }
    }

    pub fn show(&self) {
        if !(self.0.host.is_visible)() {
            return;
        }
        self.ensure();
        self.0.dismissed.set(false);
        if let Some(widget) = self.widget() {
            widget.root().set_visible(true);
        }
        self.reserve();
        (self.0.host.focus)();
    }

    pub fn hide(&self) {
        self.0.dismissed.set(true);
        self.0.compose.borrow_mut().pending_accent = None;
        if let Some(widget) = self.widget() {
            widget.reset();
            widget.root().set_visible(false);
        }
        self.reserve();
    }

    pub fn toggle(&self) {
        if self.widget().is_some_and(|w| w.root().get_visible()) { self.hide() } else { self.show() }
    }

    pub fn reveal(&self, visible: bool) {
        if !visible {
            self.hide();
            self.0.dismissed.set(false);
        } else if self.0.available.get() && !self.0.dismissed.get() {
            self.show();
        }
    }

    pub fn key(&self, kind: &str, value: u32, flags: u32) {
        let entry = (self.0.host.entry)();
        if !(self.0.host.is_visible)() || !entry.is_sensitive() {
            return;
        }
        (self.0.host.focus)();
        let labels = self.widget().map(|w| w.labels()).unwrap_or_default();
        let dead = self.0.dead.borrow().clone();
        let outcome = apply_key(&entry, &labels, &dead, &mut self.0.compose.borrow_mut(), kind, value, flags);
        match outcome {
            Outcome::Submit => (self.0.host.submit)(),
            Outcome::Hide => self.hide(),
            Outcome::None => {}
        }
    }

    pub fn close(&self) {
        if let Some(monitor) = self.0.monitor.borrow_mut().take() {
            monitor.close();
        }
        self.0.compose.borrow_mut().pending_accent = None;
        if let Some(widget) = self.widget() {
            widget.reset();
            widget.close();
        }
    }
}

fn up(weak: &Weak<LockInner>) -> Option<LockKeyboard> { weak.upgrade().map(LockKeyboard) }

#[allow(unused_imports)]
use glib as _;
