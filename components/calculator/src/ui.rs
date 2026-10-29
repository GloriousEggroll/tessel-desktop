//! Port of calculator_ui.py: the GTK4 calculator window using the suite's live
//! per-monitor palette.

use crate::backend::{evaluate, format_number, CalculatorStore, HistoryEntry, MAX_EXPRESSION};
use crate::decimal::Decimal;
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::app_menu::{self, AboutDetails, MenuTarget, Shortcut, ShortcutSection};
use suite_gtk::theme::{self, WindowTheme};
use suite_gtk::utils::{accessible, add_css, children, headerbar, image, remove_css, set_margins, PRIORITY_APPLICATION};

pub const STYLE: &str = "
.calculator { color: @theme_fg_color; background: @theme_bg_color; }
.calculator .calculator-display { border-radius: 14px; padding: 12px 14px; }
.calculator entry.expression { font-size: 15pt; background: transparent; border: none; box-shadow: none; }
.calculator .result { font-size: 26pt; font-weight: 650; }
.calculator .muted { opacity: .74; }
.calculator .calculator-key { min-height: 24px; min-width: 34px; padding: 4px 7px; border-radius: 9px; font-size: 13pt; }
.calculator .scientific-key { font-size: 10pt; min-height: 18px; padding: 3px 5px; }
.calculator .memory-key { padding: 5px 7px; border-radius: 7px; min-height: 22px; }
.calculator button.icon-button { min-width: 24px; min-height: 24px; padding: 3px; border-radius: 7px; }
.calculator .history-title { font-size: 13pt; font-weight: 650; }
.calculator .history-card { border-radius: 10px; border: 1px solid alpha(@theme_fg_color, .12); padding: 8px; }
.calculator .history-expression { font-size: 10pt; }
.calculator .history-result { font-size: 14pt; font-weight: 600; }
.calculator .error { color: #df7b85; }
";

pub fn shortcuts() -> Vec<ShortcutSection> {
    vec![
        ("Calculation".into(), vec![Shortcut::any(&["Return", "equal"], "Calculate"), Shortcut::new("BackSpace", "Delete the last character"),
                                    Shortcut::any(&["Escape", "Delete"], "Clear"), Shortcut::new("<Control>c", "Copy result"),
                                    Shortcut::new("<Control>v", "Paste expression")]),
        ("View".into(), vec![Shortcut::new("<Control>h", "Show calculation history")]),
    ]
}

pub const SCIENTIFIC_KEYS: [(&str, &str); 18] = [
    ("sin", "sin("), ("cos", "cos("), ("tan", "tan("), ("(", "("), (")", ")"), ("π", "π"),
    ("sin⁻¹", "asin("), ("cos⁻¹", "acos("), ("tan⁻¹", "atan("), ("ln", "ln("), ("log", "log("), ("e", "e"),
    ("xʸ", "^"), ("x!", "!"), ("|x|", "abs("), ("eˣ", "exp("), ("10ˣ", "10^("), ("1/x", "reciprocal"),
];

pub const KEYS: [(&str, &str); 24] = [
    ("%", "%"), ("x²", "^2"), ("√x", "sqrt("), ("⌫", "backspace"),
    ("7", "7"), ("8", "8"), ("9", "9"), ("÷", "/"),
    ("4", "4"), ("5", "5"), ("6", "6"), ("×", "*"),
    ("1", "1"), ("2", "2"), ("3", "3"), ("−", "-"),
    ("±", "negate"), ("0", "0"), (".", "."), ("+", "+"),
    ("C", "clear"), ("(", "("), (")", ")"), ("=", "evaluate"),
];

pub const MEMORY_KEYS: [(&str, &str); 6] = [
    ("MC", "Clear memory"), ("MR", "Recall memory"), ("M+", "Add to memory"),
    ("M−", "Subtract from memory"), ("MS", "Store in memory"), ("Ans", "Insert previous answer"),
];

/// Characters typed straight into the expression.
const TYPED: &str = "0123456789.+-*/^%()!abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZπ×÷−";

fn key_tooltip(value: &str, title: &str) -> String {
    match value {
        "%" => "Percent (divide by 100)", "backspace" => "Backspace", "clear" => "Clear (Esc)",
        "negate" => "Change sign", "evaluate" => "Calculate (Enter)", "^2" => "Square", "^" => "Power",
        "sqrt(" => "Square root", "!" => "Factorial", "reciprocal" => "Reciprocal", _ => title,
    }.to_string()
}

fn icon_button(icon: &str, tooltip: &str, callback: impl Fn() + 'static) -> gtk::Button {
    let button = gtk::Button::builder().child(&image(icon, 16)).tooltip_text(tr!(tooltip)).build();
    button.add_css_class("icon-button");
    accessible(&button, &tr!(tooltip));
    button.connect_clicked(move |_| callback());
    button
}

pub struct Widgets {
    pub window: gtk::ApplicationWindow,
    pub header: gtk::HeaderBar,
    pub menu_button: gtk::MenuButton,
    pub keys: gtk::EventControllerKey,
    pub panes: gtk::Box,
    pub mode_dropdown: gtk::DropDown,
    pub angle_dropdown: gtk::DropDown,
    pub history_button: gtk::ToggleButton,
    pub expression: gtk::Entry,
    pub result_label: gtk::Label,
    pub copy_button: gtk::Button,
    pub status_label: gtk::Label,
    pub memory_buttons: HashMap<&'static str, gtk::Button>,
    pub scientific_box: gtk::Grid,
    pub keypad: gtk::Grid,
    pub buttons: HashMap<&'static str, gtk::Button>,
    pub memory_status: gtk::Label,
    pub history_panel: gtk::Box,
    pub history_list: gtk::Box,
    pub history_empty: gtk::Label,
    pub history_scroll: gtk::ScrolledWindow,
    pub clear_history_button: gtk::Button,
}

pub struct Inner {
    pub w: Widgets,
    pub store: RefCell<CalculatorStore>,
    pub answer: RefCell<Decimal>,
    pub just_evaluated: Cell<bool>,
    pub closed: Cell<bool>,
    changing: Cell<bool>,
    paste_serial: Cell<u64>,
    status_timer: RefCell<Option<glib::SourceId>>,
    css: gtk::CssProvider,
    pub theme: RefCell<Option<WindowTheme>>,
}

/// `CalculatorWindow` (cheap clone handle).
#[derive(Clone)]
pub struct CalculatorWindow(pub Rc<Inner>);

impl std::ops::Deref for CalculatorWindow {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

impl std::ops::Deref for Inner {
    type Target = Widgets;
    fn deref(&self) -> &Widgets { &self.w }
}

/// A signal handler holding the window weakly: `on!(this, |w| w.method())`.
macro_rules! on {
    ($this:expr, |$w:ident| $body:expr) => {{
        let weak: Weak<Inner> = Rc::downgrade(&$this.0);
        move |_| {
            if let Some(inner) = weak.upgrade() {
                let $w = CalculatorWindow(inner);
                $body;
            }
        }
    }};
}

impl CalculatorWindow {
    pub fn new(application: &gtk::Application, store: Option<CalculatorStore>) -> Self {
        let store = store.unwrap_or_else(|| CalculatorStore::new(None));
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Calculator")).build();
        window.add_css_class("calculator");
        window.set_default_size(440, if store.scientific { 585 } else { 480 });
        window.set_size_request(365, -1);
        // Above the palette stylesheet (APPLICATION + 30) so the chrome rules win.
        let css = add_css(STYLE, Some(&WidgetExt::display(&window)), PRIORITY_APPLICATION + 31);
        let header = headerbar(&tr!("Calculator"));
        window.set_titlebar(Some(&header));
        let menu_button = app_menu::install_app_menu(application, &window, MenuTarget::New, AboutDetails {
            name: "Calculator".into(), icon: "accessories-calculator".into(), version: None,
            comments: Some("Standard and scientific calculations".into()), website: None,
        }, shortcuts(), Some(&header), None);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        window.add_controller(keys.clone());

        let panes = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        set_margins(&panes, 14);
        window.set_child(Some(&panes));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).hexpand(true).vexpand(true).build();
        panes.append(&content);

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let mode_dropdown = gtk::DropDown::from_strings(&[&tr!("Standard"), &tr!("Scientific")]);
        mode_dropdown.set_selected(if store.scientific { 1 } else { 0 });
        accessible(&mode_dropdown, &tr!("Calculator mode"));
        toolbar.append(&mode_dropdown);
        let angle_dropdown = gtk::DropDown::from_strings(&[&tr!("Degrees"), &tr!("Radians")]);
        angle_dropdown.set_selected(if store.degrees { 0 } else { 1 });
        angle_dropdown.set_visible(store.scientific);
        accessible(&angle_dropdown, &tr!("Angle units"));
        toolbar.append(&angle_dropdown);
        toolbar.append(&gtk::Box::builder().hexpand(true).build());
        let history_button = gtk::ToggleButton::builder().child(&image("document-open-recent-symbolic", 18))
            .tooltip_text(tr!("History (Ctrl+H)")).build();
        history_button.add_css_class("icon-button");
        accessible(&history_button, &tr!("Show calculation history"));
        toolbar.append(&history_button);
        content.append(&toolbar);

        let display = gtk::Box::new(gtk::Orientation::Vertical, 8);
        display.add_css_class("card");
        display.add_css_class("calculator-display");
        let expression = gtk::Entry::builder().xalign(1.0).placeholder_text("0").build();
        expression.add_css_class("expression");
        expression.set_max_length(MAX_EXPRESSION as i32);
        accessible(&expression, &tr!("Expression"));
        display.append(&expression);
        let result_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let result_label = gtk::Label::builder().label("0").xalign(1.0).hexpand(true).selectable(true).build();
        result_label.add_css_class("result");
        result_label.set_ellipsize(pango::EllipsizeMode::End);
        result_label.set_width_chars(1);
        result_label.set_max_width_chars(28);
        accessible(&result_label, &tr!("Result"));
        result_row.append(&result_label);
        let copy_button = icon_button("edit-copy-symbolic", "Copy result (Ctrl+C)", || {});
        result_row.append(&copy_button);
        display.append(&result_row);
        let status_label = gtk::Label::builder().xalign(1.0).wrap(true).build();
        status_label.set_wrap_mode(pango::WrapMode::WordChar);
        status_label.set_max_width_chars(42);
        status_label.add_css_class("muted");
        status_label.set_visible(false);
        display.append(&status_label);
        content.append(&display);

        let memory_row = gtk::Box::builder().spacing(5).homogeneous(true).build();
        let mut memory_buttons = HashMap::new();
        for (action, tooltip) in MEMORY_KEYS {
            let button = gtk::Button::builder().label(action).tooltip_text(tr!(tooltip)).build();
            accessible(&button, &tr!(tooltip));
            button.add_css_class("memory-key");
            memory_row.append(&button);
            memory_buttons.insert(action, button);
        }
        content.append(&memory_row);

        let grid = |spacing| gtk::Grid::builder().column_spacing(spacing).row_spacing(spacing).column_homogeneous(true)
            .row_homogeneous(true).build();
        let scientific_box = grid(6);
        let mut key_buttons: Vec<(gtk::Button, &'static str)> = Vec::new();
        for (index, (title, value)) in SCIENTIFIC_KEYS.iter().enumerate() {
            let button = Self::key(title, value);
            button.add_css_class("scientific-key");
            scientific_box.attach(&button, index as i32 % 6, index as i32 / 6, 1, 1);
            key_buttons.push((button, value));
        }
        scientific_box.set_visible(store.scientific);
        content.append(&scientific_box);

        let keypad = grid(7);
        keypad.set_vexpand(true);
        let mut buttons = HashMap::new();
        for (index, (title, value)) in KEYS.iter().enumerate() {
            let button = Self::key(title, value);
            if *value == "evaluate" {
                button.add_css_class("primary");
            }
            keypad.attach(&button, index as i32 % 4, index as i32 / 4, 1, 1);
            buttons.insert(*value, button.clone());
            key_buttons.push((button, value));
        }
        content.append(&keypad);
        let memory_status = gtk::Label::builder().xalign(0.0).build();
        memory_status.add_css_class("muted");
        memory_status.set_ellipsize(pango::EllipsizeMode::End);
        memory_status.set_max_width_chars(44);
        content.append(&memory_status);

        // _build_history
        let history_panel = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).width_request(260).build();
        history_panel.set_hexpand(false);
        let history_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::builder().label(tr!("History")).xalign(0.0).hexpand(true).build();
        title.add_css_class("history-title");
        history_header.append(&title);
        let clear_history_button = icon_button("edit-clear-all-symbolic", "Clear history", || {});
        history_header.append(&clear_history_button);
        history_panel.append(&history_header);
        let history_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        history_list.set_valign(gtk::Align::Start);
        let history_empty = gtk::Label::builder().label(tr!("Your calculations will appear here.")).wrap(true).vexpand(true)
            .valign(gtk::Align::Center).justify(gtk::Justification::Center).build();
        history_empty.add_css_class("muted");
        history_empty.set_max_width_chars(25);
        let history_scroll = gtk::ScrolledWindow::builder().vexpand(true).min_content_width(260).build();
        history_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        history_scroll.set_child(Some(&history_list));
        history_panel.append(&history_scroll);
        history_panel.append(&history_empty);
        history_panel.set_visible(false);
        panes.append(&history_panel);

        let this = Self(Rc::new(Inner {
            w: Widgets { window: window.clone(), header, menu_button, keys: keys.clone(), panes, mode_dropdown, angle_dropdown,
                         history_button, expression, result_label, copy_button, status_label, memory_buttons, scientific_box,
                         keypad, buttons, memory_status, history_panel, history_list, history_empty, history_scroll,
                         clear_history_button },
            store: RefCell::new(store), answer: RefCell::new(Decimal::zero()), just_evaluated: Cell::new(false),
            closed: Cell::new(false), changing: Cell::new(false), paste_serial: Cell::new(0),
            status_timer: RefCell::new(None), css, theme: RefCell::new(None),
        }));
        // The window owns its handle; destroy() releases it.
        unsafe { window.set_data("calculator-window", this.clone()) };
        this.copy_button.connect_clicked(on!(this, |w| w.copy_result()));
        this.clear_history_button.connect_clicked(on!(this, |w| w.clear_history()));
        let weak = Rc::downgrade(&this.0);
        window.connect_close_request(move |_| {
            if let Some(inner) = weak.upgrade() {
                CalculatorWindow(inner).destroy();
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(&this.0);
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            if CalculatorWindow(inner).key_pressed(keyval, state) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        this.mode_dropdown.connect_selected_notify(on!(this, |w| w.mode_changed()));
        this.angle_dropdown.connect_selected_notify(on!(this, |w| w.angle_changed()));
        this.history_button.connect_toggled(on!(this, |w| w.history_toggled()));
        this.expression.connect_changed(on!(this, |w| w.expression_changed()));
        for (action, button) in &this.memory_buttons {
            let action = *action;
            button.connect_clicked(on!(this, |w| w.memory_clicked(action)));
        }
        for (button, value) in key_buttons {
            button.connect_clicked(on!(this, |w| w.input(value)));
        }
        this.refresh_history();
        this.refresh_memory();
        this.theme.replace(Some(theme::attach(&window, None)));
        this.expression.grab_focus_without_selecting();
        let error = this.store.borrow().error.clone();
        if !error.is_empty() {
            this.status(&(tr!("Could not load saved history: ") + &error), true);
        }
        this
    }

    fn key(title: &str, value: &str) -> gtk::Button {
        let button = gtk::Button::builder().label(title).hexpand(true).vexpand(true).build();
        if value == "backspace" {
            button.set_child(Some(&image("edit-clear-symbolic", 20)));
        } else if value == "sqrt(" {
            // A math-font fallback can give the radical an enormous line box,
            // stretching every row of a homogeneous keypad. Draw its outline
            // at icon size so typography never changes the grid's dimensions.
            let radical = gtk::DrawingArea::builder().content_width(26).content_height(20)
                .halign(gtk::Align::Center).valign(gtk::Align::Center).build();
            radical.set_draw_func(|widget, context, _, _| {
                let color = widget.color();
                context.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, color.alpha() as f64);
                context.set_line_width(1.8);
                context.move_to(2.0, 10.0);
                context.line_to(6.0, 16.0);
                context.line_to(11.0, 4.0);
                context.line_to(24.0, 4.0);
                let _ = context.stroke();
            });
            button.set_child(Some(&radical));
        }
        button.add_css_class("calculator-key");
        let tooltip = tr!(key_tooltip(value, title));
        button.set_tooltip_text(Some(&tooltip));
        accessible(&button, &tooltip);
        button
    }

    pub fn refresh_history(&self) {
        for child in children(&self.history_list) {
            self.history_list.remove(&child);
        }
        let entries = self.store.borrow().history.clone();
        for entry in &entries {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
            card.add_css_class("card");
            card.add_css_class("history-card");
            let recall = gtk::Button::new();
            recall.add_css_class("profile-row");
            let content = gtk::Box::new(gtk::Orientation::Vertical, 5);
            let expression = gtk::Label::builder().label(&entry.expression).xalign(1.0).build();
            expression.set_ellipsize(pango::EllipsizeMode::Start);
            expression.set_max_width_chars(26);
            expression.add_css_class("history-expression");
            expression.add_css_class("muted");
            content.append(&expression);
            let result = gtk::Label::builder().label(format!("= {}", entry.result)).xalign(1.0).build();
            result.set_ellipsize(pango::EllipsizeMode::Start);
            result.set_max_width_chars(26);
            result.add_css_class("history-result");
            content.append(&result);
            recall.set_child(Some(&content));
            recall.set_tooltip_text(Some(&format!("{} = {}", entry.expression, entry.result)));
            accessible(&recall, &(tr!("Reuse calculation: ") + &entry.expression));
            let item = entry.clone();
            recall.connect_clicked(on!(self, |w| w.use_history(&item)));
            card.append(&recall);
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let units = gtk::Label::builder().label(if entry.degrees { tr!("Degrees") } else { tr!("Radians") })
                .hexpand(true).xalign(0.0).build();
            units.add_css_class("muted");
            actions.append(&units);
            let (copy_value, insert_value) = (entry.result.clone(), entry.result.clone());
            let weak = Rc::downgrade(&self.0);
            actions.append(&icon_button("edit-copy-symbolic", "Copy result", move || {
                if let Some(inner) = weak.upgrade() { CalculatorWindow(inner).copy(&copy_value) }
            }));
            let weak = Rc::downgrade(&self.0);
            actions.append(&icon_button("go-up-symbolic", "Insert result", move || {
                if let Some(inner) = weak.upgrade() { CalculatorWindow(inner).insert(&insert_value) }
            }));
            card.append(&actions);
            self.history_list.append(&card);
        }
        let visible = !entries.is_empty();
        self.history_empty.set_visible(!visible);
        self.history_scroll.set_visible(visible);
        self.clear_history_button.set_sensitive(visible);
    }

    pub fn use_history(&self, entry: &HistoryEntry) {
        self.angle_dropdown.set_selected(if entry.degrees { 0 } else { 1 });
        self.set_expression(&entry.expression);
        self.answer.replace(Decimal::parse(&entry.result).unwrap_or_else(Decimal::zero));
        self.result_label.set_text(&entry.result);
        self.result_label.set_tooltip_text(Some(&entry.result));
        self.just_evaluated.set(false);
        self.status("", false);
        self.expression.grab_focus_without_selecting();
    }

    pub fn clear_history(&self) {
        self.store.borrow_mut().clear_history();
        self.refresh_history();
        let error = self.store.borrow().error.clone();
        if !error.is_empty() {
            self.status(&(tr!("Could not save history: ") + &error), true);
        }
    }

    fn history_toggled(&self) {
        let visible = self.history_button.is_active();
        self.history_panel.set_visible(visible);
        let scientific = self.store.borrow().scientific;
        self.window.set_default_size(if visible { 735 } else { 440 }, if scientific { 620 } else { 530 });
    }

    fn mode_changed(&self) {
        let scientific = self.mode_dropdown.selected() == 1;
        self.store.borrow_mut().scientific = scientific;
        self.scientific_box.set_visible(scientific);
        self.angle_dropdown.set_visible(scientific);
        self.store.borrow_mut().save();
        self.history_toggled();
    }

    fn angle_changed(&self) {
        self.store.borrow_mut().degrees = self.angle_dropdown.selected() == 0;
        self.store.borrow_mut().save();
    }

    fn expression_changed(&self) {
        if !self.changing.get() {
            self.just_evaluated.set(false);
            self.status("", false);
        }
    }

    pub fn set_expression(&self, value: &str) {
        self.changing.set(true);
        let value: String = value.chars().take(MAX_EXPRESSION).collect();
        self.expression.set_text(&value);
        self.expression.set_position(-1);
        self.changing.set(false);
    }

    pub fn insert(&self, text: &str) {
        if self.just_evaluated.get() {
            let operator = text.chars().next().is_none_or(|c| "+-*/^%!".contains(c));
            let value = if operator { format_number(&self.answer.borrow()) } else { String::new() };
            self.set_expression(&value);
            self.just_evaluated.set(false);
        }
        let current: Vec<char> = self.expression.text().chars().collect();
        let (start, end) = match self.expression.selection_bounds() {
            Some((a, b)) => (a.min(b) as usize, a.max(b) as usize),
            None => {
                let position = self.expression.position().max(0) as usize;
                (position, position)
            }
        };
        let (start, end) = (start.min(current.len()), end.min(current.len()));
        let updated: String = current[..start].iter().copied().chain(text.chars()).chain(current[end..].iter().copied()).collect();
        if updated.chars().count() > MAX_EXPRESSION {
            self.status(&tr!("This expression is too long."), true);
            return;
        }
        self.set_expression(&updated);
        self.expression.set_position((start + text.chars().count()) as i32);
        self.status("", false);
        self.expression.grab_focus_without_selecting();
    }

    /// A keypad value: `evaluate`, `clear`, `backspace`, `negate`, `reciprocal` or text.
    pub fn input(&self, value: &str) -> bool {
        match value {
            "evaluate" => return self.calculate(),
            "clear" => {
                self.set_expression("");
                self.result_label.set_text("0");
                self.result_label.set_tooltip_text(None);
                self.just_evaluated.set(false);
                self.status("", false);
            }
            "backspace" => {
                self.just_evaluated.set(false);
                if let Some((a, b)) = self.expression.selection_bounds() {
                    self.expression.delete_text(a.min(b), a.max(b));
                } else {
                    let position = self.expression.position();
                    if position > 0 {
                        self.expression.delete_text(position - 1, position);
                    }
                }
            }
            "negate" | "reciprocal" => {
                let text = if self.just_evaluated.get() { format_number(&self.answer.borrow()) } else { self.expression.text().to_string() };
                let text = if text.is_empty() { "0".to_string() } else { text };
                self.set_expression(&format!("{}{text})", if value == "negate" { "-(" } else { "1/(" }));
                self.just_evaluated.set(false);
                self.status("", false);
            }
            _ => self.insert(value),
        }
        self.expression.grab_focus_without_selecting();
        false
    }

    pub fn calculate(&self) -> bool {
        let text = self.expression.text().to_string();
        let expression = suite_core::pyjson::py_strip(&text).to_string();
        if expression.is_empty() {
            return false;
        }
        let (degrees, answer) = (self.store.borrow().degrees, self.answer.borrow().clone());
        let result = match evaluate(&expression, degrees, &answer) {
            Ok(result) => result,
            Err(error) => {
                self.status(&tr!(error.0), true);
                self.just_evaluated.set(false);
                return false;
            }
        };
        let replacement = format!("({})", format_number(&answer));
        let ans = regex::Regex::new(r"(?i)\bans\b").unwrap();
        let history_expression = ans.replace_all(&expression, regex::NoExpand(&replacement)).into_owned();
        let formatted = format_number(&result);
        self.answer.replace(result.clone());
        self.result_label.set_text(&formatted);
        self.result_label.set_tooltip_text(Some(&formatted));
        self.store.borrow_mut().record(&history_expression, &result);
        self.refresh_history();
        self.just_evaluated.set(true);
        let error = self.store.borrow().error.clone();
        if error.is_empty() {
            self.status("", false);
        } else {
            self.status(&(tr!("Could not save history: ") + &error), true);
        }
        true
    }

    pub fn memory_clicked(&self, action: &str) {
        if action == "Ans" {
            self.insert("ans");
            return;
        }
        let action = action.replace('−', "-");
        let outcome = (|| -> Result<(), crate::backend::CalculatorError> {
            if action == "MR" {
                let memory = self.store.borrow().memory.clone();
                if let Some(memory) = memory {
                    self.insert(&format_number(&memory));
                }
                return Ok(());
            }
            if action == "MC" {
                self.store.borrow_mut().memory_action("MC", None)?;
                self.refresh_memory();
                return Ok(());
            }
            let text = self.expression.text().to_string();
            let value = if self.just_evaluated.get() || suite_core::pyjson::py_strip(&text).is_empty() {
                self.answer.borrow().clone()
            } else {
                let (degrees, answer) = (self.store.borrow().degrees, self.answer.borrow().clone());
                evaluate(&text, degrees, &answer)?
            };
            self.store.borrow_mut().memory_action(&action, Some(&value))?;
            self.refresh_memory();
            let error = self.store.borrow().error.clone();
            if !error.is_empty() {
                self.status(&(tr!("Could not save memory: ") + &error), true);
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            self.status(&tr!(error.0), true);
        }
    }

    pub fn refresh_memory(&self) {
        let memory = self.store.borrow().memory.clone();
        let available = memory.is_some();
        self.memory_buttons["MR"].set_sensitive(available);
        self.memory_buttons["MC"].set_sensitive(available);
        self.memory_status.set_text(&memory.map(|m| tr!("Memory: ") + &format_number(&m)).unwrap_or_default());
        self.memory_status.set_visible(available);
    }

    pub fn status(&self, text: &str, error: bool) {
        if let Some(timer) = self.status_timer.take() {
            timer.remove();
        }
        self.status_label.set_text(text);
        self.status_label.set_visible(!text.is_empty());
        if error {
            self.status_label.add_css_class("error");
        } else {
            self.status_label.remove_css_class("error");
        }
    }

    pub fn copy(&self, value: &str) {
        self.window.clipboard().set_text(value);
        self.status(&tr!("Copied to clipboard"), false);
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(1800), move || {
            if let Some(inner) = weak.upgrade() {
                inner.status_timer.replace(None);
                if !inner.closed.get() {
                    CalculatorWindow(inner).status("", false);
                }
            }
        });
        self.status_timer.replace(Some(id));
    }

    pub fn copy_result(&self) { self.copy(&self.result_label.text()); }

    pub fn paste(&self) {
        self.paste_serial.set(self.paste_serial.get() + 1);
        let serial = self.paste_serial.get();
        let weak = Rc::downgrade(&self.0);
        self.window.clipboard().read_text_async(None::<&gio::Cancellable>, move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let window = CalculatorWindow(inner);
            match result {
                Err(error) => {
                    if !window.closed.get() {
                        window.status(&suite_core::pyerr::python_error_text(&error), true);
                    }
                }
                Ok(value) => {
                    if let Some(value) = value.filter(|v| !v.is_empty()) {
                        if !window.closed.get() && serial == window.paste_serial.get() {
                            window.insert(suite_core::pyjson::py_strip(&value));
                        }
                    }
                }
            }
        });
    }

    pub fn key_pressed(&self, keyval: gdk::Key, state: gdk::ModifierType) -> bool {
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            if keyval == gdk::Key::h || keyval == gdk::Key::H {
                self.history_button.set_active(!self.history_button.is_active());
                return true;
            }
            if (keyval == gdk::Key::c || keyval == gdk::Key::C) && self.expression.selection_bounds().is_none() {
                self.copy_result();
                return true;
            }
            if keyval == gdk::Key::v || keyval == gdk::Key::V {
                self.paste();
                return true;
            }
            if keyval == gdk::Key::q || keyval == gdk::Key::Q {
                self.window.close();
                return true;
            }
            return false;
        }
        if state.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return false;
        }
        if let Some(focus) = GtkWindowExt::focus(&self.window) {
            if focus.is::<gtk::DropDown>() || focus.ancestor(gtk::DropDown::static_type()).is_some() {
                return false;
            }
        }
        if [gdk::Key::Return, gdk::Key::KP_Enter, gdk::Key::equal].contains(&keyval) {
            self.calculate();
            return true;
        }
        if [gdk::Key::Escape, gdk::Key::Delete, gdk::Key::KP_Delete].contains(&keyval) {
            self.input("clear");
            return true;
        }
        if keyval == gdk::Key::BackSpace {
            self.input("backspace");
            return true;
        }
        if let Some(c) = keyval.to_unicode().filter(|c| *c != '\0') {
            if TYPED.contains(c) {
                self.insert(&c.to_string());
                return true;
            }
        }
        false
    }

    /// `destroy()`: stop timers and pastes, release the theme and stylesheet.
    pub fn destroy(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        self.paste_serial.set(self.paste_serial.get() + 1);
        if let Some(timer) = self.status_timer.take() {
            timer.remove();
        }
        if let Some(theme) = self.theme.borrow().as_ref() {
            theme.close();
        }
        remove_css(&WidgetExt::display(&self.window), &self.css);
        self.window.destroy();
        let _: Option<CalculatorWindow> = unsafe { self.window.steal_data("calculator-window") };
    }
}

use gtk::gio;
