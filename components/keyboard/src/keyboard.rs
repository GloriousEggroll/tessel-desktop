//! Touch-friendly keyboard widget, also usable inside a secure lock surface (osk_keyboard.py).
use gtk::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use tessel_core::tr;
use tessel_gtk::gtk;
use tessel_gtk::utils::{accessible, add_css, image, remove_children, remove_css, PRIORITY_APPLICATION};

pub const STYLE: &str = "
.tessel-keyboard { padding: 12px; border-radius: 8px; background: @theme_bg_color; color: @theme_fg_color; }
.tessel-keyboard .keyboard-heading { font-weight: 600; font-size: 15px; }
.tessel-keyboard .keyboard-key { min-height: 42px; min-width: 22px; padding: 4px 5px;
    border-radius: 8px; border: 1px solid alpha(@theme_fg_color,.13);
    background: alpha(@theme_fg_color,.065); box-shadow: 0 2px 0 alpha(black,.12); font-size: 18px; }
.tessel-keyboard .keyboard-key:hover { background: alpha(@theme_fg_color,.12); }
.tessel-keyboard .keyboard-key:active, .tessel-keyboard .keyboard-key.latched { background: alpha(@accent_bg_color,.35); }
.tessel-keyboard .keyboard-utility { min-height: 30px; padding: 4px 9px; border-radius: 8px; }
.tessel-keyboard .keyboard-status { opacity: .72; font-size: 12px; }
.tessel-keyboard.keyboard-small .keyboard-key { min-height: 34px; font-size: 16px; }
.tessel-keyboard.keyboard-large .keyboard-key { min-height: 54px; font-size: 21px; }
.tessel-keyboard.keyboard-narrow { padding: 6px; }
.tessel-keyboard.keyboard-narrow .keyboard-key { min-width: 0; padding: 3px 1px; font-size: 15px; }
";

/// Evdev codes of the four character rows.
pub fn rows() -> [Vec<u32>; 4] {
    [(2..14).collect(), (16..28).collect(), (30..41).chain([43]).collect(), (44..54).collect()]
}
const BASE: [&str; 4] = ["1234567890-=", "qwertyuiop[]", "asdfghjkl;'\\", "zxcvbnm,./"];
const SHIFT: [&str; 4] = ["!@#$%^&*()_+", "QWERTYUIOP{}", "ASDFGHJKL:\"|", "ZXCVBNM<>?"];

pub type Labels = HashMap<String, [String; 4]>;

/// US labels until the helper's `layout` event arrives.
pub fn default_labels() -> Labels {
    let mut labels = Labels::new();
    for ((codes, normal), shift) in rows().iter().zip(BASE).zip(SHIFT) {
        for ((code, a), b) in codes.iter().zip(normal.chars()).zip(shift.chars()) {
            let (a, b) = (a.to_string(), b.to_string());
            labels.insert(code.to_string(), [a.clone(), b.clone(), a, b]);
        }
    }
    labels.insert("57".into(), std::array::from_fn(|_| " ".into()));
    labels
}

/// Keep only well-formed entries of a helper `layout.keys` object.
pub fn merge_labels(labels: &mut Labels, value: &serde_json::Value) {
    let Some(map) = value.as_object() else { return };
    for (key, value) in map {
        // Python str.isdecimal(): non-empty, all decimal digits.
        if key.is_empty() || !key.chars().all(|c| c.is_numeric() && c.to_digit(10).is_some()) {
            continue;
        }
        let Some(list) = value.as_array().filter(|l| l.len() == 4) else { continue };
        let strings: Vec<&str> = list.iter().filter_map(|s| s.as_str().filter(|s| s.chars().count() < 32)).collect();
        if strings.len() == 4 {
            labels.insert(key.clone(), std::array::from_fn(|i| strings[i].to_string()));
        }
    }
}

/// Shift (1) and AltGr (16) select the label level.
pub fn level(flags: u32) -> usize { (if flags & 1 != 0 { 1 } else { 0 }) + (if flags & 16 != 0 { 2 } else { 0 }) }

/// Input purposes 2,3,4,9 (digits, number, phone, PIN) open the numeric pad.
pub fn mode_for_purpose(purpose: u64) -> Mode { if matches!(purpose, 2 | 3 | 4 | 9) { Mode::Numbers } else { Mode::Letters } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Letters,
    Numbers,
    Symbols,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self { Mode::Letters => "letters", Mode::Numbers => "numbers", Mode::Symbols => "symbols" }
    }
}

#[derive(Clone)]
enum Item {
    Key(u32),
    Text(String),
    Modifier(u32),
    Mode(Mode),
}

fn tooltip_for(code: u32) -> Option<&'static str> {
    Some(match code {
        14 => "Backspace", 28 => "Enter", 15 => "Tab", 1 => "Escape", 105 => "Left", 106 => "Right", 103 => "Up",
        108 => "Down", 111 => "Delete", _ => return None,
    })
}

pub type Send = Box<dyn Fn(&str, u32, u32)>;

struct State {
    labels: Labels,
    flags: u32,
    shift_locked: bool,
    last_shift: Option<Instant>,
    mode: Mode,
    character_buttons: Vec<(u32, gtk::Button)>,
    modifier_buttons: Vec<(u32, gtk::Button)>,
    repeat: Option<glib::SourceId>,
}

pub struct Inner {
    pub root: gtk::Box,
    pub body: gtk::Box,
    pub status: gtk::Label,
    pub hide_button: gtk::Button,
    provider: gtk::CssProvider,
    secure: bool,
    send: Send,
    hide: Box<dyn Fn()>,
    state: RefCell<State>,
}

/// The widget; `root()` is the `Gtk.Box` with classes `tessel-keyboard popup-surface`.
#[derive(Clone)]
pub struct Keyboard(Rc<Inner>);

impl Keyboard {
    pub fn new(send: impl Fn(&str, u32, u32) + 'static, hide: impl Fn() + 'static, settings: Option<Box<dyn Fn()>>, secure: bool) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("tessel-keyboard");
        root.add_css_class("popup-surface");
        let provider = add_css(STYLE, None, PRIORITY_APPLICATION);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        heading.append(&image("input-keyboard-symbolic", 22));
        let title = gtk::Label::builder().label(tr!("On-screen Keyboard")).xalign(0.0).hexpand(true).build();
        title.add_css_class("keyboard-heading");
        heading.append(&title);
        if let Some(settings) = settings {
            let button = gtk::Button::builder().icon_name("emblem-system-symbolic").tooltip_text(tr!("Keyboard settings"))
                .focusable(false).build();
            button.add_css_class("keyboard-utility");
            button.connect_clicked(move |_| settings());
            heading.append(&button);
        }
        let hide_button = gtk::Button::builder().focusable(false).tooltip_text(tr!("Hide keyboard")).build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        content.append(&image("pan-down-symbolic", 18));
        content.append(&gtk::Label::new(Some(&tr!("Hide"))));
        hide_button.set_child(Some(&content));
        hide_button.add_css_class("keyboard-utility");
        hide_button.set_has_tooltip(!secure);
        accessible(&hide_button, &tr!("Hide keyboard"));
        heading.append(&hide_button);
        root.append(&heading);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 7);
        root.append(&body);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        status.add_css_class("keyboard-status");
        root.append(&status);
        let keyboard = Self(Rc::new(Inner {
            root: root.clone(), body, status, hide_button: hide_button.clone(), provider, secure, send: Box::new(send),
            hide: Box::new(hide), state: RefCell::new(State {
                labels: default_labels(), flags: 0, shift_locked: false, last_shift: None, mode: Mode::Letters,
                character_buttons: Vec::new(), modifier_buttons: Vec::new(), repeat: None }) }));
        let weak = keyboard.downgrade();
        hide_button.connect_clicked(move |_| if let Some(k) = upgrade(&weak) { k.dismiss() });
        keyboard.render();
        let weak = keyboard.downgrade();
        root.connect_unmap(move |_| if let Some(k) = upgrade(&weak) { k.reset() });
        let weak = keyboard.downgrade();
        root.connect_unrealize(move |_| if let Some(k) = upgrade(&weak) { k.stop_repeat() });
        keyboard
    }

    fn downgrade(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }
    pub fn root(&self) -> &gtk::Box { &self.0.root }
    pub fn body(&self) -> &gtk::Box { &self.0.body }
    pub fn status_label(&self) -> &gtk::Label { &self.0.status }
    pub fn hide_button(&self) -> &gtk::Button { &self.0.hide_button }
    pub fn flags(&self) -> u32 { self.0.state.borrow().flags }
    pub fn shift_locked(&self) -> bool { self.0.state.borrow().shift_locked }
    pub fn mode(&self) -> Mode { self.0.state.borrow().mode }
    pub fn labels(&self) -> Labels { self.0.state.borrow().labels.clone() }
    pub fn label(&self, code: &str) -> Option<[String; 4]> { self.0.state.borrow().labels.get(code).cloned() }
    pub fn character_button(&self, code: u32) -> Option<gtk::Button> {
        self.0.state.borrow().character_buttons.iter().find(|(c, _)| *c == code).map(|(_, b)| b.clone())
    }
    pub fn modifier_button(&self, flag: u32) -> Option<gtk::Button> {
        self.0.state.borrow().modifier_buttons.iter().find(|(c, _)| *c == flag).map(|(_, b)| b.clone())
    }

    pub fn message(&self, text: &str) {
        self.0.status.set_text(text);
        self.0.status.set_visible(!text.is_empty());
    }

    pub fn set_labels(&self, labels: &serde_json::Value) {
        if !labels.is_object() {
            return;
        }
        merge_labels(&mut self.0.state.borrow_mut().labels, labels);
        self.update_labels();
    }

    pub fn set_size(&self, size: &str) {
        for value in ["small", "medium", "large"] {
            let class = format!("keyboard-{value}");
            if size == value { self.0.root.add_css_class(&class) } else { self.0.root.remove_css_class(&class) }
        }
    }

    pub fn set_purpose(&self, purpose: u64) {
        let mode = mode_for_purpose(purpose);
        if mode != self.mode() {
            self.0.state.borrow_mut().mode = mode;
            self.render();
        }
    }

    fn button(&self, label: &str, callback: Rc<dyn Fn()>, tooltip: Option<&str>, repeat: bool) -> gtk::Button {
        let button = gtk::Button::builder().label(label).focusable(false).hexpand(true).build();
        button.add_css_class("keyboard-key");
        accessible(&button, tooltip.unwrap_or(label));
        if let Some(tooltip) = tooltip.filter(|_| !self.0.secure) {
            button.set_tooltip_text(Some(&tr!(tooltip)));
        }
        if repeat {
            let gesture = gtk::GestureClick::builder().button(1).build();
            gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = self.downgrade();
            gesture.connect_pressed(move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                callback();
                let Some(keyboard) = upgrade(&weak) else { return };
                keyboard.stop_repeat();
                let (weak, callback) = (weak.clone(), callback.clone());
                let id = glib::timeout_add_local_once(Duration::from_millis(420), move || {
                    let Some(keyboard) = upgrade(&weak) else { return };
                    let callback = callback.clone();
                    let id = glib::timeout_add_local(Duration::from_millis(65), move || {
                        callback();
                        glib::ControlFlow::Continue
                    });
                    // The one-shot source is finishing; only the interval is live now.
                    keyboard.0.state.borrow_mut().repeat = Some(id);
                });
                keyboard.0.state.borrow_mut().repeat = Some(id);
            });
            let weak = self.downgrade();
            gesture.connect_released(move |_, _, _, _| if let Some(k) = upgrade(&weak) { k.stop_repeat() });
            let weak = self.downgrade();
            gesture.connect_cancel(move |_, _| if let Some(k) = upgrade(&weak) { k.stop_repeat() });
            button.add_controller(gesture);
        } else {
            button.connect_clicked(move |_| callback());
        }
        button
    }

    fn row(&self, items: &[(String, Item, i32)]) {
        let grid = gtk::Grid::builder().column_spacing(4).column_homogeneous(true).build();
        let mut column = 0;
        for (label, item, span) in items {
            let weak = self.downgrade();
            let item_for_callback = item.clone();
            let callback: Rc<dyn Fn()> = Rc::new(move || {
                let Some(keyboard) = upgrade(&weak) else { return };
                match &item_for_callback {
                    Item::Key(code) => keyboard.press(*code),
                    Item::Text(text) => keyboard.character(text),
                    Item::Modifier(flag) => keyboard.modifier(*flag),
                    Item::Mode(mode) => keyboard.change_mode(*mode),
                }
            });
            let (tooltip, repeat) = match item {
                Item::Key(code) => (tooltip_for(*code), matches!(code, 14 | 111 | 105 | 106)),
                _ => (None, false),
            };
            let button = self.button(label, callback, tooltip, repeat);
            grid.attach(&button, column, 0, *span, 1);
            column += span;
            let mut state = self.0.state.borrow_mut();
            match item {
                Item::Key(code) if state.labels.contains_key(&code.to_string()) => state.character_buttons.push((*code, button)),
                Item::Modifier(flag) => state.modifier_buttons.push((*flag, button)),
                _ => {}
            }
        }
        self.0.body.append(&grid);
    }

    fn render(&self) {
        self.stop_repeat();
        remove_children(&self.0.body);
        let mode = {
            let mut state = self.0.state.borrow_mut();
            state.character_buttons.clear();
            state.modifier_buttons.clear();
            state.mode
        };
        let key = |code: u32, span| (code.to_string(), Item::Key(code), span);
        let named = |label: &str, code: u32, span| (label.to_string(), Item::Key(code), span);
        let text = |c: &str, span| (c.to_string(), Item::Text(c.to_string()), span);
        let modifier = |label: &str, flag: u32, span| (label.to_string(), Item::Modifier(flag), span);
        let mode_item = |label: &str, mode: Mode, span| (label.to_string(), Item::Mode(mode), span);
        let rows = rows();
        match mode {
            Mode::Letters => {
                self.row(&rows[0].iter().map(|c| key(*c, 2)).chain([named("⌫", 14, 3)]).collect::<Vec<_>>());
                self.row(&[named("Tab", 15, 3)].into_iter().chain(rows[1].iter().map(|c| key(*c, 2))).collect::<Vec<_>>());
                self.row(&rows[2].iter().map(|c| key(*c, 2)).chain([named("↵", 28, 3)]).collect::<Vec<_>>());
                self.row(&[modifier("⇧", 1, 3)].into_iter().chain(rows[3].iter().map(|c| key(*c, 2))).chain([named("↑", 103, 2)])
                    .collect::<Vec<_>>());
                self.row(&[mode_item("123", Mode::Numbers, 2), mode_item("#+=", Mode::Symbols, 2), modifier("Ctrl", 2, 2),
                           modifier("Alt", 4, 2), modifier("Super", 8, 2), named("Space", 57, 7), modifier("AltGr", 16, 2),
                           named("←", 105, 2), named("↓", 108, 2), named("→", 106, 2)]);
            }
            Mode::Numbers => {
                for line in ["789", "456", "123"] {
                    let mut items: Vec<_> = line.chars().map(|c| text(&c.to_string(), 2)).collect();
                    items.push(match line {
                        "789" => named("⌫", 14, 2),
                        "456" => text("-", 2),
                        _ => text("+", 2),
                    });
                    self.row(&items);
                }
                self.row(&[mode_item("ABC", Mode::Letters, 2), text("0", 2), text(".", 2), named("↵", 28, 2)]);
            }
            Mode::Symbols => {
                for line in ["!@#$%^&*()", "[]{}<>_=+\\", "€£¥¢°±×÷µ§", ".,;:!?'\"`~"] {
                    self.row(&line.chars().map(|c| text(&c.to_string(), 1)).collect::<Vec<_>>());
                }
                self.row(&[mode_item("ABC", Mode::Letters, 2), named("Esc", 1, 2), named("Space", 57, 4), named("Delete", 111, 2),
                           named("⌫", 14, 2), named("↵", 28, 2)]);
            }
        }
        self.update_labels();
    }

    fn update_labels(&self) {
        let state = self.0.state.borrow();
        let level = level(state.flags);
        for (code, button) in &state.character_buttons {
            let empty: [String; 4] = Default::default();
            let values = state.labels.get(&code.to_string()).unwrap_or(&empty);
            let label = if values[level].is_empty() { values[0].as_str() } else { values[level].as_str() };
            if *code == 57 {
                button.set_label(&tr!("Space"));
            } else {
                button.set_label(if label.is_empty() { "·" } else { label });
            }
            button.set_sensitive(*code == 57 || !label.is_empty());
        }
        for (flag, button) in &state.modifier_buttons {
            if state.flags & flag != 0 { button.add_css_class("latched") } else { button.remove_css_class("latched") }
            if *flag == 1 {
                button.set_label(if state.shift_locked { "⇪" } else { "⇧" });
            }
        }
    }

    /// One-shot modifiers; Shift tapped twice within 0.4 s locks (⇪).
    pub fn modifier(&self, flag: u32) {
        {
            let mut state = self.0.state.borrow_mut();
            if flag == 1 {
                let stamp = Instant::now();
                let recent = state.last_shift.is_some_and(|last| stamp.duration_since(last) < Duration::from_millis(400));
                if !state.shift_locked && state.flags & 1 != 0 && recent {
                    state.shift_locked = true;
                } else {
                    state.flags ^= flag;
                    state.shift_locked = false;
                }
                state.last_shift = Some(stamp);
            } else {
                state.flags ^= flag;
            }
        }
        self.update_labels();
    }

    fn after_key(&self) {
        {
            let mut state = self.0.state.borrow_mut();
            state.flags = if state.shift_locked { 1 } else { 0 };
        }
        self.update_labels();
    }

    pub fn press(&self, code: u32) {
        let flags = self.flags();
        (self.0.send)("key", code, flags);
        self.after_key();
    }

    pub fn character(&self, text: &str) {
        for c in text.chars() {
            (self.0.send)("text", c as u32, 0);
        }
        self.after_key();
    }

    pub fn change_mode(&self, mode: Mode) {
        self.0.state.borrow_mut().mode = mode;
        self.render();
    }

    pub fn stop_repeat(&self) {
        if let Some(id) = self.0.state.borrow_mut().repeat.take() {
            id.remove();
        }
    }

    pub fn reset(&self) {
        self.stop_repeat();
        {
            let mut state = self.0.state.borrow_mut();
            state.flags = 0;
            state.shift_locked = false;
        }
        self.update_labels();
    }

    pub fn dismiss(&self) {
        self.reset();
        (self.0.hide)();
    }

    pub fn close(&self) {
        self.stop_repeat();
        remove_css(&gtk::prelude::WidgetExt::display(&self.0.root), &self.0.provider);
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<Keyboard> { weak.upgrade().map(Keyboard) }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_labels_match_us_layout() {
        let labels = default_labels();
        assert_eq!(labels["30"], ["a", "A", "a", "A"]);
        assert_eq!(labels["43"], ["\\", "|", "\\", "|"]);
        assert_eq!(labels["13"], ["=", "+", "=", "+"]);
        assert_eq!(labels["57"], [" ", " ", " ", " "]);
        assert_eq!(labels.len(), 12 + 12 + 12 + 10 + 1);
    }

    #[test]
    fn merging_helper_labels_validates_entries() {
        let mut labels = default_labels();
        merge_labels(&mut labels, &json!({"21": ["z", "Z", "←", "¥"], "x": ["a","a","a","a"], "22": ["u"],
                                          "23": ["i", "I", 1, ""], "24": ["o", "O", "", "x".repeat(32)], "100": ["", "", "", ""]}));
        assert_eq!(labels["21"], ["z", "Z", "←", "¥"]);
        assert_eq!(labels["22"][0], "u");
        assert_eq!(labels["23"][0], "i");
        assert_eq!(labels["24"][0], "o");
        assert!(labels.contains_key("100"));
        assert!(!labels.contains_key("x"));
    }

    #[test]
    fn levels_and_purposes() {
        assert_eq!([level(0), level(1), level(16), level(17), level(2)], [0, 1, 2, 3, 0]);
        for purpose in [2, 3, 4, 9] {
            assert_eq!(mode_for_purpose(purpose), Mode::Numbers);
        }
        for purpose in [0, 1, 5, 8, 10] {
            assert_eq!(mode_for_purpose(purpose), Mode::Letters);
        }
    }
}
