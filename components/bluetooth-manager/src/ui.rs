//! The GTK half of `class App` (btmanager.py): the window or tray popup, the
//! device rows (reused while their `RowView` is unchanged, like the Python
//! `_state_signature`), the Add device window and the Info / Send File /
//! Send Note / transfer / incoming-file dialogs. All state decisions live in
//! [`Manager`]; this module renders its views and forwards input.
#![allow(deprecated)]

use crate::backend::{Bt, Device, DEFAULT_TIMEOUT, DEVICE_IFACE, PROPS_IFACE};
use crate::dbus::BtError;
use crate::env::Env;
use crate::i18n::{tr, tr1, P};
use crate::manager::*;
use crate::settings::RECEIVE_MODES;
use crate::transfer::{is_unavailable_error, DoneFn, ProgressFn, PromptHandle, ReceiveRequest, Sender};
use glib::Variant;
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use suite_gtk::utils::{accessible, headerbar, key_controller, pack};

pub const POPUP_WIDTH: i32 = 400;
pub const POPUP_LIST_MAX_HEIGHT: i32 = 420;

/// STYLE_CSS of btmanager.py, unchanged.
pub const STYLE_CSS: &str = include_str!("style.css");

/// Creates an outgoing transfer (`ObexSender(...)`; fakes in tests).
pub type SenderFactory = Rc<dyn Fn(&str, &str, &[String], ProgressFn, DoneFn) -> Rc<dyn Sender>>;

pub fn obex_sender_factory() -> SenderFactory {
    Rc::new(|source, destination, files, on_progress, on_done| {
        Rc::new(crate::transfer::ObexSender::new(source, destination, files, Some(on_progress), Some(on_done), None, None))
    })
}

pub struct Options {
    pub application: Option<gtk::Application>,
    pub popup: bool,
    pub popup_edge: String,
    pub popup_offset: i32,
    pub lang: String,
    pub bt: Rc<Bt>,
    pub env: Rc<dyn Env>,
    pub services: Services,
    pub sender_factory: SenderFactory,
    /// Install SIGINT → destroy (not when embedded or in tests).
    pub handle_sigint: bool,
}

/// One device section (`_make_section`).
pub struct Section {
    pub root: gtk::Box,
    pub listbox: gtk::ListBox,
    pub label: gtk::Label,
    pub badge: gtk::Label,
    pub show_trust: bool,
    empty: RefCell<Option<(gtk::ListBoxRow, String)>>,
}

/// A rendered device row and its controls.
#[derive(Clone)]
pub struct Row {
    pub row: gtk::ListBoxRow,
    pub view: RowView,
    pub connect_button: gtk::Button,
    pub forget_button: Option<gtk::Button>,
    pub advanced_button: gtk::MenuButton,
    pub advanced_items: Vec<(&'static str, gtk::Button)>,
    pub trust_switch: Option<gtk::Switch>,
    pub battery_label: Option<gtk::Label>,
}

impl Row {
    pub fn item(&self, key: &str) -> Option<gtk::Button> {
        self.advanced_items.iter().find(|(k, _)| *k == key).map(|(_, b)| b.clone())
    }
}

/// An icon + caption button (`_button_icon`).
#[derive(Clone)]
pub struct IconButton {
    pub button: gtk::Button,
    pub icon: gtk::Image,
    pub caption: gtk::Label,
}

pub struct AddWidgets {
    pub window: gtk::Window,
    pub adapter_lbl: gtk::Label,
    pub scan: IconButton,
    pub spinner: gtk::Spinner,
    pub status_lbl: gtk::Label,
    pub nearby: Rc<Section>,
    pub event_lbl: gtk::Label,
}

/// `Gtk.Dialog` + the Python attributes tests look at.
#[derive(Clone)]
pub struct Tracked {
    pub window: gtk::Window,
    alive: Rc<Cell<bool>>,
}

impl Tracked {
    pub fn alive(&self) -> bool { self.alive.get() }
}

impl PromptHandle for Tracked {
    fn alive(&self) -> bool { self.alive.get() }
    fn destroy(&self) { self.window.destroy() }
}

#[derive(Clone)]
pub struct InfoDialog {
    pub dialog: gtk::Dialog,
    pub view: gtk::TextView,
    pub error: gtk::Label,
    pub refresh: gtk::Button,
}

#[derive(Clone)]
pub struct NoteDialog {
    pub dialog: gtk::Dialog,
    pub view: gtk::TextView,
    pub error: gtk::Label,
    pub send: gtk::Button,
}

#[derive(Clone)]
pub struct TransferDialog {
    pub dialog: gtk::Dialog,
    pub progress: gtk::ProgressBar,
    pub status: gtk::Label,
    pub error: gtk::Label,
    pub filename: gtk::Label,
    pub button: gtk::Button,
    pub sender: Rc<RefCell<Option<Rc<dyn Sender>>>>,
}

pub struct Ui {
    pub manager: Rc<Manager>,
    pub window: gtk::Window,
    pub popup: RefCell<Option<suite_gtk::popup::PopupController>>,
    pub popup_mode: bool,
    pub lang: String,
    applying: Cell<bool>,
    pub add_btn: gtk::Button,
    pub adapter_combo: gtk::ComboBox,
    adapter_model: gtk::ListStore,
    pub receive_combo: Option<gtk::DropDown>,
    pub power_switch: gtk::Switch,
    pub power_tile: gtk::Box,
    pub power_icon: gtk::Image,
    pub ctrl_lbl: gtk::Label,
    pub status_lbl: gtk::Label,
    pub content_stack: gtk::Stack,
    pub devices_scroll: gtk::ScrolledWindow,
    pub list_connected: Rc<Section>,
    pub list_known: Rc<Section>,
    pub off_title: gtk::Label,
    pub off_description: gtk::Label,
    pub event_bar: gtk::Box,
    pub event_icon: gtk::Image,
    pub event_lbl: gtk::Label,
    pub add: RefCell<Option<AddWidgets>>,
    pub rows: RefCell<HashMap<String, Row>>,
    pub dialogs: RefCell<Vec<Tracked>>,
    pub transfers: RefCell<Vec<TransferDialog>>,
    trust_columns: gtk::SizeGroup,
    sender_factory: SenderFactory,
    weak: RefCell<Weak<Ui>>,
    /// `App._settings_host`: the Global Settings window embedding this UI
    /// (dialogs are transient for it instead of the never-shown window).
    pub settings_host: RefCell<Option<gtk::Window>>,
}

fn style<W: IsA<gtk::Widget>>(widget: W, classes: &[&str]) -> W {
    for class in classes {
        widget.add_css_class(class);
    }
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        suite_gtk::theme::attach(window, None);
    }
    widget
}

fn image(name: &str, size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(size);
    image
}

fn margins(widget: &impl IsA<gtk::Widget>, vertical: i32, horizontal: i32) {
    widget.set_margin_top(vertical);
    widget.set_margin_bottom(vertical);
    widget.set_margin_start(horizontal);
    widget.set_margin_end(horizontal);
}

fn label(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).build()
}

fn button_icon(label_text: &str, icon: gtk::Image) -> IconButton {
    let button = gtk::Button::with_label(label_text);
    let caption = gtk::Label::new(Some(label_text));
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Center);
    content.append(&icon);
    content.append(&caption);
    button.set_child(Some(&content));
    IconButton { button, icon, caption }
}

fn describe(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Description(text)]);
}

fn listbox_children(listbox: &gtk::ListBox) -> Vec<gtk::Widget> {
    suite_gtk::utils::children(listbox)
}

/// Python `str.splitlines()`.
pub fn splitlines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut current));
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                lines.push(std::mem::take(&mut current))
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The vNote payload `_on_send_note_clicked` writes.
pub fn vnote(body: &str, date: &str) -> String {
    format!("BEGIN:VNOTE\r\nVERSION:1.1\r\nBODY;CHARSET=UTF-8:{}\r\nDCREATED:{date}\r\nLAST-MODIFIED:{date}\r\nCLASS:PUBLIC\r\nX-IRMC-LUID:000001000000\r\nEND:VNOTE\r\n",
            splitlines(body).join(" "))
}

/// `tempfile.TemporaryDirectory(prefix=…)` + `note.vnt` (`"x"`, 0600).
pub fn write_note(body: &str) -> Result<(PathBuf, PathBuf), String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let base = std::env::var_os("TMPDIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let mut directory = None;
    for _ in 0..100 {
        let candidate = base.join(format!("btmanager-note-{}", suite_core::secrets::token_urlsafe(6).replace('-', "_")));
        match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => {
                directory = Some(candidate);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(suite_core::pyerr::os_error_path(&e, &candidate)),
        }
    }
    let directory = directory.ok_or_else(|| "[Errno 17] No usable temporary directory name found".to_owned())?;
    let path = directory.join("note.vnt");
    let date = glib::DateTime::now_local().and_then(|d| d.format("%Y%m%dT%H%M%S")).map(|s| s.to_string()).unwrap_or_default();
    let written = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, vnote(body, &date).as_bytes()));
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(suite_core::pyerr::os_error_path(&e, &path));
    }
    Ok((directory, path))
}

/// Python `str()` of a PyGObject-unpacked value (for the Info text).
fn py_text(value: &Variant, top: bool) -> String {
    use glib::VariantClass as C;
    let repr = |v: &Variant| py_text(v, false);
    match value.classify() {
        C::Variant => value.as_variant().map(|v| py_text(&v, top)).unwrap_or_default(),
        C::Boolean => {
            let b = value.get::<bool>().unwrap_or(false);
            if top { tr(if b { "Yes" } else { "No" }) } else if b { "True".into() } else { "False".into() }
        }
        C::String | C::ObjectPath | C::Signature => {
            let text = value.str().unwrap_or_default();
            if top { text.to_owned() } else { suite_core::pyjson::py_repr_value(&serde_json::Value::String(text.into())) }
        }
        C::Double => suite_core::pyfmt::float_repr(value.get::<f64>().unwrap_or(0.0)),
        C::Array if value.type_().as_str() == "ay" => {
            let bytes: Vec<u8> = value.fixed_array::<u8>().map(|b| b.to_vec()).unwrap_or_default();
            let mut out = String::from("b'");
            for b in bytes {
                match b {
                    b'\\' => out.push_str("\\\\"),
                    b'\'' => out.push_str("\\'"),
                    b'\t' => out.push_str("\\t"),
                    b'\n' => out.push_str("\\n"),
                    b'\r' => out.push_str("\\r"),
                    32..=126 => out.push(b as char),
                    _ => out.push_str(&format!("\\x{b:02x}")),
                }
            }
            out.push('\'');
            out
        }
        C::Array if value.type_().element().is_dict_entry() => {
            let items: Vec<String> = value.iter().map(|e| format!("{}: {}", repr(&e.child_value(0)), repr(&e.child_value(1)))).collect();
            format!("{{{}}}", items.join(", "))
        }
        C::Array | C::Tuple => {
            let items: Vec<String> = value.iter().map(|v| if top { py_text(&v, true) } else { repr(&v) }).collect();
            if top { items.join("\n") } else if value.classify() == C::Tuple { format!("({})", items.join(", ")) } else { format!("[{}]", items.join(", ")) }
        }
        _ => suite_core::values::to_value(value).to_string(),
    }
}

fn int_of(value: &Variant) -> Option<i64> {
    use glib::VariantClass as C;
    let inner = if value.classify() == C::Variant { value.as_variant()? } else { value.clone() };
    match inner.classify() {
        C::Byte | C::Int16 | C::Uint16 | C::Int32 | C::Uint32 | C::Int64 | C::Uint64 => suite_core::values::to_value(&inner).as_i64(),
        C::Boolean => Some(inner.get::<bool>().unwrap_or(false) as i64),
        _ => None,
    }
}

/// `_info_text(properties)`.
pub fn info_text(properties: &[(String, Variant)]) -> String {
    let mut sorted: Vec<&(String, Variant)> = properties.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut lines = Vec::new();
    for (key, raw) in sorted {
        let value = match (key.as_str(), int_of(raw)) {
            ("RSSI" | "TxPower", Some(n)) => format!("{n} dBm"),
            ("Class" | "Appearance", Some(n)) => format!("0x{n:04x}"),
            _ => py_text(raw, true),
        };
        lines.push(format!("{key}: {}", value.replace('\n', "\n    ")));
    }
    lines.join("\n\n")
}

impl Ui {
    pub fn new(options: Options) -> Rc<Ui> {
        Self::build(options, None)
    }

    /// `App(embedded=True, host_window=host)`: the controls are built but the
    /// window is never shown (Global Settings moves its content into a page),
    /// no SIGINT handler is installed and dialogs are transient for `host`.
    pub fn new_embedded(mut options: Options, host: &gtk::Window) -> Rc<Ui> {
        options.handle_sigint = false;
        options.popup = false;
        Self::build(options, Some(host.clone()))
    }

    /// `_settings_host or self.window`.
    pub fn dialog_parent(&self) -> gtk::Window {
        self.settings_host.borrow().clone().unwrap_or_else(|| self.window.clone())
    }

    fn build(options: Options, host: Option<gtk::Window>) -> Rc<Ui> {
        let embedded = host.is_some();
        crate::i18n::set_language(&options.lang);
        let manager = Manager::new(options.bt.clone(), options.env.clone(), options.popup, &options.lang, options.services.clone());
        let popup = options.popup;
        let window: gtk::Window = match &options.application {
            Some(app) => gtk::ApplicationWindow::new(app).upcast(),
            None => gtk::Window::new(),
        };
        window.set_title(Some(&tr("Bluetooth")));
        if popup {
            // Match the suite's tray popups: fixed width, natural height.
            suite_gtk::popup::set_popup_width(&window, POPUP_WIDTH);
        } else {
            window.set_default_size(820, 710);
            window.set_size_request(560, 440);
        }
        window.add_css_class("btmanager");
        suite_gtk::utils::css_once(&WidgetExt::display(&window), "btmanager-style", STYLE_CSS, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        suite_gtk::theme::attach(&window, None);

        let trust_columns = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        gtk::Widget::set_default_direction(if ["ar", "he", "fa"].contains(&options.lang.as_str()) { gtk::TextDirection::Rtl }
                                           else { gtk::TextDirection::Ltr });
        if !popup {
            let hb = match window.titlebar().and_then(|t| t.downcast::<gtk::HeaderBar>().ok()) {
                Some(hb) => hb,
                None => {
                    let hb = headerbar(&tr("Bluetooth"));
                    hb.set_show_title_buttons(true);
                    window.set_titlebar(Some(&hb));
                    hb
                }
            };
            hb.set_title_widget(Some(&gtk::Label::new(Some(&tr("Bluetooth")))));
        }

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        margins(&root, if popup { 16 } else { 20 }, if popup { 16 } else { 28 });
        if popup {
            let surface = style(gtk::Box::new(gtk::Orientation::Vertical, 0), &["popup-surface"]);
            pack(&surface, &root, true, true, 0, false);
            window.set_child(Some(&surface));
        } else {
            window.set_child(Some(&root));
        }

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        heading.set_margin_bottom(24);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let title = style(label(&tr("Bluetooth")), &["page-title"]);
        let subtitle = style(label(&tr("Manage your devices and connections.")), &["page-subtitle"]);
        subtitle.set_wrap(true);
        pack(&titles, &title, false, false, 0, false);
        pack(&titles, &subtitle, false, false, 0, false);
        pack(&heading, &titles, true, true, 0, false);

        let add = button_icon(&tr("Add device"), image("list-add-symbolic", 16));
        let add_btn = style(add.button, &["suggested-action"]);
        add_btn.set_valign(gtk::Align::Center);
        add_btn.set_sensitive(false);
        if !popup {
            pack(&heading, &add_btn, false, false, 0, true);
            pack(&root, &heading, false, false, 0, false);
        }

        let adapters = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        adapters.set_margin_bottom(16);
        if !popup {
            pack(&adapters, &style(gtk::Label::new(Some(&tr("Adapter"))), &["section-title"]), false, false, 0, false);
        }
        let adapter_model = gtk::ListStore::new(&[String::static_type(), String::static_type()]);
        let adapter_combo = gtk::ComboBox::with_model(&adapter_model);
        adapter_combo.set_id_column(0);
        let renderer = gtk::CellRendererText::new();
        renderer.set_ellipsize(pango::EllipsizeMode::End);
        renderer.set_width_chars(if popup { 16 } else { 20 });
        if popup {
            // Long "name · hci0 · address" entries must not widen the popup.
            renderer.set_fixed_size(160, -1);
        }
        adapter_combo.pack_start(&renderer, true);
        adapter_combo.add_attribute(&renderer, "text", 1);
        accessible(&adapter_combo, &tr("Adapter"));
        adapter_combo.set_sensitive(false);
        pack(&adapters, &adapter_combo, true, true, 0, false);

        let receive_combo = (!popup).then(|| gtk::DropDown::from_strings(&[&tr("Off"), &tr("Ask")]));
        if !popup {
            pack(&root, &adapters, false, false, 0, false);
        }

        let power_switch = gtk::Switch::new();
        power_switch.set_valign(gtk::Align::Center);
        power_switch.set_sensitive(false);
        power_switch.set_tooltip_text(Some(&tr("Bluetooth")));
        accessible(&power_switch, &tr("Bluetooth"));

        let probe = gtk::IconTheme::for_display(&WidgetExt::display(&window));
        let header_icon = dev_icon(&probe, None);
        let (card, power_tile, power_icon, ctrl_lbl, status_lbl): (gtk::Widget, gtk::Box, gtk::Image, gtk::Label, gtk::Label);
        if popup {
            // The shared tray header: tinted tile, title, state and the power switch.
            let header = suite_gtk::popup::popup_header(&[&header_icon], &tr("Bluetooth"), Some(" "),
                vec![suite_gtk::popup::HeaderAction::Widget(power_switch.clone().upcast())]);
            header.root.set_margin_bottom(12);
            header.subtitle_label.set_max_width_chars(26);
            power_tile = header.tile.clone();
            power_icon = header.icon.clone();
            ctrl_lbl = header.subtitle_label.clone();
            // The adapter selector already supplies the controller identity.
            status_lbl = gtk::Label::builder().xalign(0.0).visible(false).build();
            card = header.root.clone().upcast();
        } else {
            let power_card = style(gtk::Box::new(gtk::Orientation::Horizontal, 0), &["power-card"]);
            power_card.set_margin_bottom(24);
            let card_box = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            margins(&card_box, 18, 20);
            pack(&power_card, &card_box, true, true, 0, false);
            power_tile = style(gtk::Box::new(gtk::Orientation::Horizontal, 0), &["bluetooth-tile"]);
            power_tile.set_valign(gtk::Align::Center);
            power_tile.set_overflow(gtk::Overflow::Hidden);
            power_icon = image(&header_icon, 30);
            power_icon.set_halign(gtk::Align::Center);
            power_icon.set_valign(gtk::Align::Center);
            pack(&power_tile, &power_icon, true, false, 0, false);
            pack(&card_box, &power_tile, false, false, 0, false);
            let titlebox = gtk::Box::new(gtk::Orientation::Vertical, 5);
            titlebox.set_valign(gtk::Align::Center);
            ctrl_lbl = style(gtk::Label::builder().xalign(0.0).build(), &["power-title"]);
            ctrl_lbl.set_ellipsize(pango::EllipsizeMode::End);
            ctrl_lbl.set_max_width_chars(42);
            status_lbl = style(gtk::Label::builder().xalign(0.0).build(), &["secondary-label"]);
            status_lbl.set_ellipsize(pango::EllipsizeMode::End);
            status_lbl.set_max_width_chars(44);
            pack(&titlebox, &ctrl_lbl, false, false, 0, false);
            pack(&titlebox, &status_lbl, false, false, 0, false);
            pack(&card_box, &titlebox, true, true, 0, false);
            pack(&card_box, &power_switch, false, false, 0, true);
            card = power_card.upcast();
        }

        let content_stack = gtk::Stack::new();
        content_stack.set_vexpand(true);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        if popup {
            // Size to the device lists, up to a cap; scroll beyond it.
            content_stack.set_vhomogeneous(false);
            content_stack.set_hhomogeneous(false);
            content_stack.set_interpolate_size(false);
            scroll.set_propagate_natural_height(true);
            scroll.set_max_content_height(POPUP_LIST_MAX_HEIGHT);
        } else {
            scroll.set_propagate_natural_height(false);
        }
        let inner = gtk::Box::new(gtk::Orientation::Vertical, if popup { 16 } else { 20 });
        inner.set_margin_end(2);
        inner.set_margin_bottom(8);
        scroll.set_child(Some(&inner));
        content_stack.add_named(&scroll, Some("devices"));
        let list_connected = make_section(&inner, Some("connected-count"), true, popup, &trust_columns);
        let list_known = make_section(&inner, None, true, popup, &trust_columns);

        let off_state = gtk::Box::new(gtk::Orientation::Vertical, 10);
        off_state.set_valign(gtk::Align::Center);
        off_state.set_halign(gtk::Align::Center);
        margins(&off_state, if popup { 16 } else { 28 }, 16);
        let off_icon = style(image("bluetooth-disabled-symbolic", if popup { 40 } else { 56 }), &["off-symbol"]);
        pack(&off_state, &off_icon, false, false, 0, false);
        let off_title = style(gtk::Label::new(None), &["off-title"]);
        off_title.set_wrap(true);
        off_title.set_justify(gtk::Justification::Center);
        pack(&off_state, &off_title, false, false, 0, false);
        let off_description = style(gtk::Label::new(Some(&tr("Turn on Bluetooth to discover and connect your devices."))),
                                    &["secondary-label"]);
        off_description.set_wrap(true);
        off_description.set_max_width_chars(if popup { 32 } else { 46 });
        off_description.set_justify(gtk::Justification::Center);
        pack(&off_state, &off_description, false, false, 0, false);
        content_stack.add_named(&off_state, Some("off"));

        let event_bar = style(gtk::Box::new(gtk::Orientation::Horizontal, 8), &["event-bar"]);
        event_bar.set_margin_top(if popup { 10 } else { 16 });
        let event_icon = image("dialog-information-symbolic", 14);
        pack(&event_bar, &event_icon, false, false, 0, false);
        let event_lbl = label("");
        // Error advice is a sentence: wrap it over a few lines, not one.
        event_lbl.set_wrap(true);
        event_lbl.set_wrap_mode(pango::WrapMode::WordChar);
        event_lbl.set_lines(3);
        event_lbl.set_ellipsize(pango::EllipsizeMode::End);
        event_lbl.set_max_width_chars(if popup { 46 } else { 60 });
        pack(&event_bar, &event_lbl, true, true, 0, false);

        let ui = Rc::new(Ui {
            manager: manager.clone(), window: window.clone(), popup: RefCell::new(None), popup_mode: popup, lang: options.lang.clone(),
            applying: Cell::new(false), add_btn: add_btn.clone(), adapter_combo: adapter_combo.clone(), adapter_model,
            receive_combo: receive_combo.clone(), power_switch: power_switch.clone(), power_tile, power_icon, ctrl_lbl, status_lbl,
            content_stack: content_stack.clone(), devices_scroll: scroll, list_connected, list_known, off_title, off_description,
            event_bar: event_bar.clone(), event_icon, event_lbl, add: RefCell::new(None), rows: RefCell::default(),
            dialogs: RefCell::default(), transfers: RefCell::default(), trust_columns, sender_factory: options.sender_factory.clone(),
            weak: RefCell::new(Weak::new()), settings_host: RefCell::new(host),
        });
        ui.weak.replace(Rc::downgrade(&ui));

        if let Some(combo) = &receive_combo {
            pack(&root, &ui.build_receive_setting(combo), false, false, 0, false);
        }
        pack(&root, &card, false, false, 0, false);
        if popup {
            pack(&root, &adapters, false, false, 0, false);
        }
        pack(&root, &content_stack, true, true, 0, false);
        let footer = popup.then(|| {
            add_btn.add_css_class("add-device-action");
            let footer = suite_gtk::popup::popup_footer(&[Some(add_btn.clone().upcast())]);
            footer.set_margin_top(12);
            footer
        });
        pack(&root, &event_bar, false, false, 0, false);
        if let Some(footer) = &footer {
            pack(&root, footer, false, false, 0, false);
        }

        let weak = Rc::downgrade(&ui);
        adapter_combo.connect_changed(move |combo| {
            if let Some(ui) = weak.upgrade() {
                if !ui.applying.get() {
                    ui.manager.adapter_changed(combo.active_id().as_deref());
                }
            }
        });
        let weak = Rc::downgrade(&ui);
        power_switch.connect_active_notify(move |switch| {
            if let Some(ui) = weak.upgrade() {
                if !ui.applying.get() {
                    ui.manager.power_toggled(switch.is_active());
                }
            }
        });
        let weak = Rc::downgrade(&ui);
        add_btn.connect_clicked(move |_| if let Some(ui) = weak.upgrade() { ui.on_add_device_clicked() });
        let weak = Rc::downgrade(&ui);
        window.connect_unrealize(move |_| if let Some(ui) = weak.upgrade() { ui.on_window_destroyed() });
        if options.handle_sigint {
            let weak = window.downgrade();
            suite_core::glib_compat::unix_signal_add_local(libc::SIGINT, move || {
                if let Some(window) = weak.upgrade() {
                    window.destroy();
                }
                glib::ControlFlow::Continue
            });
        }
        if popup {
            let weak = Rc::downgrade(&ui);
            let controller = suite_gtk::popup::PopupController::new(&window, move || {
                if let Some(ui) = weak.upgrade() { ui.manager.apply_state(); }
            }, &options.popup_edge, options.popup_offset, "service_apps");
            if let Some(app) = &options.application {
                let app = app.clone();
                controller.set_raise_window(move |child| crate::app::raise_child(&app, child));
            }
            ui.popup.replace(Some(controller));
        }

        let weak = Rc::downgrade(&ui);
        manager.set_mode_listener(Rc::new(move || if let Some(ui) = weak.upgrade() { ui.sync_receive_combo() }));
        manager.set_view(ui.clone());
        manager.start();
        let controller = ui.popup.borrow().clone();
        match controller {
            Some(controller) => controller.show(),
            None if embedded => {}
            None => window.set_visible(true),
        }
        manager.apply_state();
        ui
    }

    fn me(&self) -> Weak<Ui> {
        self.weak.borrow().clone()
    }

    fn build_receive_setting(&self, combo: &gtk::DropDown) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        row.set_margin_bottom(16);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        titles.set_valign(gtk::Align::Center);
        let title = style(label(&tr("Receive files")), &["section-title"]);
        let hint = style(label(&tr("Ask before saving files that nearby devices send to Downloads.")), &["secondary-label"]);
        hint.set_wrap(true);
        pack(&titles, &title, false, false, 0, false);
        pack(&titles, &hint, false, false, 0, false);
        pack(&row, &titles, true, true, 0, false);
        combo.set_valign(gtk::Align::Center);
        combo.set_selected(RECEIVE_MODES.iter().position(|m| *m == self.manager.receive_mode()).unwrap_or(1) as u32);
        accessible(combo, &tr("Receive files"));
        let weak = self.me();
        combo.connect_selected_notify(move |combo| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.applying.get() {
                return;
            }
            if ui.manager.receive_mode_selected(combo.selected()).is_err() {
                ui.sync_receive_combo();
            }
        });
        pack(&row, combo, false, false, 0, true);
        row
    }

    pub fn sync_receive_combo(&self) {
        let Some(combo) = &self.receive_combo else { return };
        self.applying.set(true);
        combo.set_selected(RECEIVE_MODES.iter().position(|m| *m == self.manager.receive_mode()).unwrap_or(1) as u32);
        self.applying.set(false);
    }

    fn icon_theme(&self) -> gtk::IconTheme {
        gtk::IconTheme::for_display(&WidgetExt::display(&self.window))
    }

    // ------------------------------------------------------------ add device
    pub fn on_add_device_clicked(&self) {
        let existing = self.add.borrow().as_ref().map(|a| a.window.clone());
        if let Some(window) = existing {
            if let Some(popup) = self.popup.borrow().as_ref() {
                popup.suspend_for_child(&window);
            }
            window.present();
            return;
        }
        if !self.manager.can_open_add() {
            return;
        }
        let window = style(gtk::Window::new(), &["btmanager"]);
        if !self.popup_mode {
            window.set_transient_for(Some(&self.dialog_parent()));
        }
        window.set_modal(false);
        window.set_destroy_with_parent(true);
        window.set_default_size(if self.popup_mode { 480 } else { 600 }, 460);
        window.set_size_request(480, 320);
        let weak = self.me();
        window.connect_unrealize(move |w| if let Some(ui) = weak.upgrade() { ui.on_add_window_destroyed(w) });
        key_controller(&window, false, |controller, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(widget) = controller.widget() {
                    if let Ok(window) = widget.downcast::<gtk::Window>() {
                        window.destroy();
                    }
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        let widgets = self.build_add_device_ui(&window);
        self.add.replace(Some(widgets));
        if let Some(popup) = self.popup.borrow().as_ref() {
            popup.suspend_for_child(&window);
        }
        window.set_visible(true);
        self.manager.add_window_opened();
        window.present();
    }

    fn build_add_device_ui(&self, window: &gtk::Window) -> AddWidgets {
        window.set_title(Some(&tr("Add device")));
        let header = match window.titlebar().and_then(|t| t.downcast::<gtk::HeaderBar>().ok()) {
            Some(header) => header,
            None => {
                let header = headerbar(&tr("Add device"));
                header.set_show_title_buttons(true);
                header.set_decoration_layout(Some(":close"));
                window.set_titlebar(Some(&header));
                header
            }
        };
        header.set_title_widget(Some(&gtk::Label::new(Some(&tr("Add device")))));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        margins(&root, 20, 24);
        window.set_child(Some(&root));
        let adapter_lbl = style(label(""), &["secondary-label"]);
        adapter_lbl.set_ellipsize(pango::EllipsizeMode::End);
        adapter_lbl.set_max_width_chars(48);
        pack(&root, &adapter_lbl, false, false, 0, false);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let spinner = gtk::Spinner::new();
        spinner.set_valign(gtk::Align::Center);
        pack(&controls, &spinner, false, false, 0, false);
        let status_lbl = style(label(""), &["section-title"]);
        status_lbl.set_wrap(true);
        status_lbl.set_max_width_chars(32);
        pack(&controls, &status_lbl, true, true, 0, false);
        let scan = button_icon(&tr("Start scan"), image("view-refresh-symbolic", 16));
        scan.button.add_css_class("suggested-action");
        scan.button.set_valign(gtk::Align::Center);
        let weak = self.me();
        scan.button.connect_clicked(move |_| if let Some(ui) = weak.upgrade() { ui.manager.scan_clicked() });
        pack(&controls, &scan.button, false, false, 0, true);
        pack(&root, &controls, false, false, 0, false);
        let hint = style(label(&tr("Make sure your device is in pairing mode.")), &["secondary-label"]);
        hint.set_wrap(true);
        hint.set_max_width_chars(55);
        pack(&root, &hint, false, false, 0, false);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inner.set_margin_end(2);
        inner.set_margin_bottom(4);
        scroll.set_child(Some(&inner));
        let nearby = make_section(&inner, None, false, self.popup_mode, &self.trust_columns);
        pack(&root, &scroll, true, true, 0, false);
        let event_lbl = style(label(""), &["event-bar"]);
        event_lbl.set_wrap(true);
        event_lbl.set_max_width_chars(55);
        pack(&root, &event_lbl, false, false, 0, false);
        AddWidgets { window: window.clone(), adapter_lbl, scan, spinner, status_lbl, nearby, event_lbl }
    }

    fn on_add_window_destroyed(&self, window: &gtk::Window) {
        let current = self.add.borrow().as_ref().map(|a| a.window.clone());
        if current.as_ref() != Some(window) {
            return;
        }
        let main: [gtk::Widget; 2] = [self.list_connected.listbox.clone().upcast(), self.list_known.listbox.clone().upcast()];
        self.rows.borrow_mut().retain(|_, row| row.row.parent().is_some_and(|p| main.contains(&p)));
        self.add.replace(None);
        self.manager.add_window_destroyed();
    }

    /// `_on_window_destroyed(window)` (also called by an embedding page).
    pub fn on_window_destroyed(&self) {
        self.manager.window_destroyed();
        let dialogs: Vec<Tracked> = self.dialogs.borrow().clone();
        for dialog in dialogs {
            dialog.window.destroy();
        }
        let add = self.add.borrow().as_ref().map(|a| a.window.clone());
        if let Some(window) = add {
            window.destroy();
        }
    }

    pub fn nearby(&self) -> Option<Rc<Section>> {
        self.add.borrow().as_ref().map(|a| a.nearby.clone())
    }

    // ------------------------------------------------------------ rows
    fn make_row(&self, view: &RowView) -> Row {
        let popup = view.popup;
        let d = &view.dev;
        let name_text = if d.alias.is_empty() { d.addr.clone() } else { d.alias.clone() };
        let row = gtk::ListBoxRow::builder().activatable(false).selectable(false).build();
        let busy = !view.busy.is_empty();
        let can_act = view.can_act;
        let line = gtk::Box::new(gtk::Orientation::Horizontal, if popup { 10 } else { 12 });
        let (container, actions) = if popup {
            let container = gtk::Box::new(gtk::Orientation::Vertical, 7);
            margins(&container, 11, 12);
            pack(&container, &line, false, false, 0, false);
            let actions = style(gtk::Box::new(gtk::Orientation::Horizontal, 7), &["device-actions"]);
            actions.set_margin_start(44);
            pack(&container, &actions, false, false, 0, false);
            (container, actions)
        } else {
            margins(&line, 15, 16);
            (line.clone(), line.clone())
        };
        let icon = style(image(&dev_icon(&self.icon_theme(), Some(d)), if popup { 20 } else { 23 }), &["device-icon"]);
        icon.set_valign(gtk::Align::Center);
        if d.connected {
            icon.add_css_class("connected");
        }
        pack(&line, &icon, false, false, 0, false);
        let vb = gtk::Box::new(gtk::Orientation::Vertical, if popup { 3 } else { 5 });
        vb.set_valign(gtk::Align::Center);
        let name = style(label(&name_text), &["device-name"]);
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_width_chars(if popup { 8 } else { 10 });
        name.set_max_width_chars(if popup { 30 } else { 38 });
        name.set_tooltip_text(Some(&name_text));
        pack(&vb, &name, false, false, 0, false);
        let metadata = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let status = if view.forgetting { Some(tr("Removing…")) } else if d.pairing { Some(tr("Pairing…")) }
                     else if d.connected { Some(tr("Connected")) } else { None };
        if let Some(status) = status {
            let state = style(label(&status), &[if d.connected { "connection-status" } else { "secondary-label" }]);
            pack(&metadata, &state, false, false, 0, false);
            pack(&metadata, &style(gtk::Label::new(Some("·")), &["secondary-label"]), false, false, 0, false);
        }
        let address = style(label(&d.addr), &["device-address"]);
        address.set_ellipsize(pango::EllipsizeMode::End);
        address.set_max_width_chars(20);
        pack(&metadata, &address, false, true, 0, false);
        let mut battery_label = None;
        if let Some(level) = d.battery {
            pack(&metadata, &style(gtk::Label::new(Some("·")), &["secondary-label"]), false, false, 0, false);
            let battery = style(gtk::Box::new(gtk::Orientation::Horizontal, 3), &["battery-level"]);
            pack(&battery, &image(&battery_icon(&self.icon_theme(), level), 12), false, false, 0, false);
            let text = format!("{level}%");
            let level_label = style(label(&text), &["device-address"]);
            pack(&battery, &level_label, false, false, 0, false);
            let tooltip = tr1("Battery: %s", text);
            battery.set_tooltip_text(Some(&tooltip));
            accessible(&battery, &tooltip);
            pack(&metadata, &battery, false, false, 0, false);
            battery_label = Some(level_label);
        }
        pack(&vb, &metadata, false, false, 0, false);
        pack(&line, &vb, true, true, 0, false);

        let action_label = if view.busy.iter().any(|b| b == "connect") { tr("Connecting…") }
            else if view.busy.iter().any(|b| b == "disconnect") { tr("Disconnecting…") }
            else if d.connected { tr("Disconnect") } else { tr("Connect") };
        let cbtn = gtk::Button::with_label(&action_label);
        cbtn.set_valign(gtk::Align::Center);
        cbtn.set_size_request(if popup { 96 } else { 104 }, -1);
        if !d.connected {
            cbtn.add_css_class("connect-action");
        }
        cbtn.set_sensitive(can_act && !busy);
        let (weak, path, dev, connected) = (self.me(), view.path.clone(), d.clone(), d.connected);
        cbtn.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if connected { ui.manager.disconnect_clicked(&path, &dev) } else { ui.manager.connect_clicked(&path, &dev) }
            }
        });
        describe(&cbtn, &name_text);
        pack(&actions, &cbtn, false, false, 0, false);

        let mut forget_button = None;
        if d.paired || d.bonded || d.trusted {
            let forget = style(gtk::Button::new(), &["destructive-action"]);
            if popup {
                forget.set_child(Some(&image("edit-delete-symbolic", 16)));
                forget.set_tooltip_text(Some(&tr("Forget device")));
                accessible(&forget, &tr("Forget device"));
                forget.add_css_class("icon-button");
            } else {
                forget.set_label(&tr("Forget device"));
            }
            forget.set_valign(gtk::Align::Center);
            forget.set_sensitive(can_act && !busy);
            describe(&forget, &name_text);
            let (weak, path, dev) = (self.me(), view.path.clone(), d.clone());
            forget.connect_clicked(move |_| if let Some(ui) = weak.upgrade() { ui.manager.forget_clicked(&path, &dev) });
            pack(&actions, &forget, false, false, 0, false);
            forget_button = Some(forget);
        }

        let (advanced_button, advanced_items) = self.make_advanced_menu(view, can_act, busy, &name_text);
        pack(&actions, &advanced_button, false, false, 0, popup);

        let mut trust_switch = None;
        if view.show_trust {
            let trust = style(gtk::Box::new(gtk::Orientation::Vertical, 0), &["trust-column"]);
            trust.set_valign(gtk::Align::Center);
            self.trust_columns.add_widget(&trust);
            let tsw = gtk::Switch::new();
            tsw.set_halign(gtk::Align::Center);
            tsw.set_valign(gtk::Align::Center);
            tsw.set_active(d.trusted);
            tsw.set_sensitive(can_act && !busy);
            tsw.set_tooltip_text(Some(&tr("When trusted, the device can connect and bond automatically.")));
            accessible(&tsw, &tr("Trust device"));
            describe(&tsw, &name_text);
            let (weak, path, dev) = (self.me(), view.path.clone(), d.clone());
            tsw.connect_active_notify(move |switch| {
                let Some(ui) = weak.upgrade() else { return };
                if ui.applying.get() {
                    return;
                }
                let target = switch.downgrade();
                let revert_ui = ui.me();
                ui.manager.trust_toggled(&path, &dev, switch.is_active(), Box::new(move |value| {
                    if let (Some(switch), Some(ui)) = (target.upgrade(), revert_ui.upgrade()) {
                        ui.applying.set(true);
                        switch.set_active(value);
                        ui.applying.set(false);
                    }
                }));
            });
            pack(&trust, &tsw, false, false, 0, false);
            pack(&line, &trust, false, false, 0, true);
            trust_switch = Some(tsw);
        }
        row.set_child(Some(&container));
        Row { row, view: view.clone(), connect_button: cbtn, forget_button, advanced_button, advanced_items, trust_switch, battery_label }
    }

    fn make_advanced_menu(&self, view: &RowView, can_act: bool, busy: bool, name: &str) -> (gtk::MenuButton, Vec<(&'static str, gtk::Button)>) {
        let button = style(gtk::MenuButton::new(), &["icon-button"]);
        button.set_child(Some(&image("view-more-symbolic", 16)));
        button.set_valign(gtk::Align::Center);
        button.set_tooltip_text(Some(&tr("Advanced")));
        accessible(&button, &tr("Advanced"));
        describe(&button, name);
        let popover = gtk::Popover::new();
        button.set_popover(Some(&popover));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&content, 10, 10);
        popover.set_child(Some(&content));
        let mut items = Vec::new();
        for (key, text, icon) in [("info", "Info", "dialog-information-symbolic"), ("file", "Send File", "document-send-symbolic"),
                                  ("note", "Send Note", "document-edit-symbolic")] {
            let item = button_icon(&tr(text), image(icon, 16)).button;
            item.set_halign(gtk::Align::Fill);
            if key != "info" {
                item.set_sensitive(can_act && !busy && view.supports_push);
                if !view.supports_push {
                    item.set_tooltip_text(Some(&tr("This device does not support file transfers.")));
                }
            }
            let (weak, path, dev, pop) = (self.me(), view.path.clone(), view.dev.clone(), popover.downgrade());
            item.connect_clicked(move |_| {
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
                let Some(ui) = weak.upgrade() else { return };
                let dev = ui.manager.bt.device(&path).unwrap_or_else(|| dev.clone());
                match key {
                    "info" => { ui.on_info_clicked(&path, &dev); }
                    "file" => { ui.on_send_file_clicked(&path, &dev); }
                    _ => { ui.on_send_note_clicked(&path, &dev); }
                }
            });
            pack(&content, &item, false, false, 0, false);
            items.push((key, item));
        }
        content.set_visible(true);
        (button, items)
    }

    fn render_section(&self, section: &Section, view: &SectionView, old_rows: &HashMap<String, Row>, rows: &mut HashMap<String, Row>) {
        section.label.set_text(&view.title);
        section.badge.set_text(&view.count.to_string());
        let mut target: Vec<gtk::Widget> = Vec::new();
        for row_view in &view.rows {
            let row = match old_rows.get(&row_view.path) {
                Some(old) if old.view == *row_view => old.clone(),
                _ => self.make_row(row_view),
            };
            target.push(row.row.clone().upcast());
            rows.insert(row_view.path.clone(), row);
        }
        if target.is_empty() {
            // Reuse placeholders: unrelated updates should not disturb focus,
            // open menus or the scroll position.
            let mut empty = section.empty.borrow_mut();
            let reuse = empty.as_ref().filter(|(_, text)| *text == view.empty_text).map(|(row, _)| row.clone());
            let row = reuse.unwrap_or_else(|| {
                let row = gtk::ListBoxRow::builder().activatable(false).selectable(false).build();
                let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                margins(&content, 23, 20);
                pack(&content, &style(image(view.empty_icon, 22), &["empty-icon"]), false, false, 0, false);
                let text = style(label(&view.empty_text), &["empty-label"]);
                text.set_wrap(true);
                text.set_max_width_chars(46);
                pack(&content, &text, true, true, 0, false);
                row.set_child(Some(&content));
                *empty = Some((row.clone(), view.empty_text.clone()));
                row
            });
            target.push(row.upcast());
        }
        let listbox = &section.listbox;
        for old in listbox_children(listbox) {
            if !target.contains(&old) {
                listbox.remove(&old);
            }
        }
        for (index, row) in target.iter().enumerate() {
            let current = listbox_children(listbox);
            if current.get(index) == Some(row) {
                continue;
            }
            if let Some(parent) = row.parent() {
                if let Ok(lb) = parent.clone().downcast::<gtk::ListBox>() {
                    lb.remove(row);
                } else {
                    row.unparent();
                }
            }
            listbox.insert(row, index as i32);
            row.set_visible(true);
        }
    }

    // ------------------------------------------------------------ dialogs
    fn track_dialog(&self, window: &gtk::Window) -> Tracked {
        match self.popup.borrow().as_ref() {
            Some(popup) => popup.suspend_for_child(window),
            None => window.set_transient_for(Some(&self.dialog_parent())),
        }
        window.set_destroy_with_parent(true);
        window.add_css_class("btmanager");
        suite_gtk::theme::attach(window, None);
        let tracked = Tracked { window: window.clone(), alive: Rc::new(Cell::new(true)) };
        self.dialogs.borrow_mut().push(tracked.clone());
        let (weak, alive) = (self.me(), tracked.alive.clone());
        window.connect_unrealize(move |w| {
            alive.set(false);
            if let Some(ui) = weak.upgrade() {
                ui.dialogs.borrow_mut().retain(|t| t.window != *w);
            }
        });
        tracked
    }

    pub fn is_tracked(&self, window: &impl IsA<gtk::Window>) -> bool {
        let window = window.upcast_ref::<gtk::Window>();
        self.dialogs.borrow().iter().any(|t| t.window == *window)
    }

    fn device_dialog(&self, title: &str, path: &str, dev: &Device, width: i32, height: i32) -> (gtk::Dialog, Tracked) {
        let dialog = gtk::Dialog::builder().title(title).build();
        let tracked = self.track_dialog(dialog.upcast_ref());
        dialog.set_default_size(width, height);
        let content = dialog.content_area();
        content.set_spacing(14);
        margins(&content, 20, 24);
        let text = if dev.alias.is_empty() { dev.addr.clone() } else { dev.alias.clone() };
        let name = style(label(&text), &["power-title"]);
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_max_width_chars(44);
        name.set_tooltip_text(Some(&text));
        pack(&content, &name, false, false, 0, false);
        let owner_text = tr1("Controller: %s", self.manager.adapter_label(Some(&device_adapter(dev, path))));
        let owner = style(label(&owner_text), &["secondary-label"]);
        owner.set_ellipsize(pango::EllipsizeMode::End);
        owner.set_max_width_chars(48);
        owner.set_tooltip_text(Some(&owner_text));
        pack(&content, &owner, false, false, 0, false);
        (dialog, tracked)
    }

    fn error_label(content: &gtk::Box) -> gtk::Label {
        let error = style(label(""), &["event-bar", "error"]);
        error.set_wrap(true);
        error.set_max_width_chars(60);
        pack(content, &error, false, false, 0, false);
        error
    }

    pub fn on_info_clicked(&self, path: &str, dev: &Device) -> InfoDialog {
        let (dialog, tracked) = self.device_dialog(&tr("Info"), path, dev, 600, 480);
        let content = dialog.content_area();
        let view = gtk::TextView::builder().editable(false).cursor_visible(true).build();
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_top_margin(12);
        view.set_bottom_margin(12);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&view));
        pack(&content, &scroll, true, true, 0, false);
        let cached: Vec<(String, Variant)> = vec![
            ("Address".into(), dev.addr.to_variant()), ("Alias".into(), dev.alias.to_variant()),
            ("Adapter".into(), device_adapter(dev, path).to_variant()), ("Connected".into(), dev.connected.to_variant()),
            ("Paired".into(), dev.paired.to_variant()), ("Trusted".into(), dev.trusted.to_variant()),
            ("UUIDs".into(), dev.uuids.to_variant()),
        ];
        view.buffer().set_text(&info_text(&cached));
        let error = Self::error_label(&content);
        let refresh: gtk::Button = dialog.add_button(&tr("Refresh"), gtk::ResponseType::Apply).downcast().unwrap();
        dialog.add_button(&tr("Close"), gtk::ResponseType::Close);
        let info = InfoDialog { dialog: dialog.clone(), view, error, refresh };
        let load = {
            let (info, bt, path, alive) = (info.clone(), self.manager.bt.clone(), path.to_owned(), tracked.alive.clone());
            Rc::new(move || {
                info.refresh.set_sensitive(false);
                info.refresh.set_label(&tr("Loading…"));
                info.error.set_visible(false);
                let (info, alive) = (info.clone(), alive.clone());
                bt.call_async(&path, PROPS_IFACE, "GetAll", Some((DEVICE_IFACE,).to_variant()), DEFAULT_TIMEOUT, move |reply| {
                    if !alive.get() {
                        return;
                    }
                    info.refresh.set_sensitive(true);
                    info.refresh.set_label(&tr("Refresh"));
                    match reply {
                        Err(err) => {
                            let (advice, details) = crate::backend::friendly_err(&err);
                            info.error.set_text(&advice);
                            info.error.set_tooltip_text(Some(&details));
                            info.error.set_visible(true);
                        }
                        Ok(result) => {
                            let props: Vec<(String, Variant)> = if result.n_children() > 0 {
                                result.child_value(0).iter().map(|e| (e.child_value(0).str().unwrap_or_default().to_owned(),
                                                                       e.child_value(1))).collect()
                            } else { Vec::new() };
                            info.view.buffer().set_text(&info_text(&props));
                        }
                    }
                });
            })
        };
        let reload = load.clone();
        dialog.connect_response(move |w, response| {
            if response == gtk::ResponseType::Apply { reload() } else { w.destroy() }
        });
        dialog.set_visible(true);
        load();
        info
    }

    pub fn on_send_file_clicked(&self, path: &str, dev: &Device) -> gtk::FileChooserDialog {
        let target = self.manager.send_snapshot(path, dev);
        let dialog = gtk::FileChooserDialog::new(Some(&tr("Send File")), None::<&gtk::Window>, gtk::FileChooserAction::Open,
                                                 &[(&tr("Cancel"), gtk::ResponseType::Cancel), (&tr("Send"), gtk::ResponseType::Ok)]);
        self.track_dialog(dialog.upcast_ref());
        dialog.set_default_response(gtk::ResponseType::Ok);
        dialog.set_select_multiple(true);
        let recipient = gtk::Label::new(Some(&tr1("To: %s", if dev.alias.is_empty() { &dev.addr } else { &dev.alias })));
        recipient.set_wrap(true);
        dialog.content_area().append(&recipient);
        let weak = self.me();
        dialog.connect_response(move |window, response| {
            let paths: Vec<String> = if response == gtk::ResponseType::Ok {
                let files = window.files();
                (0..files.n_items()).filter_map(|i| files.item(i).and_downcast::<gio::File>())
                    .filter_map(|f| f.path()).map(|p| p.to_string_lossy().into_owned()).collect()
            } else { Vec::new() };
            window.destroy();
            if let (false, Some(ui)) = (paths.is_empty(), weak.upgrade()) {
                ui.start_transfer(&target, &paths, &tr("Send File"), None);
            }
        });
        dialog.set_visible(true);
        dialog
    }

    pub fn on_send_note_clicked(&self, path: &str, dev: &Device) -> NoteDialog {
        let target = self.manager.send_snapshot(path, dev);
        let (dialog, _tracked) = self.device_dialog(&tr("Send Note"), path, dev, 520, 360);
        let content = dialog.content_area();
        let hint = style(label(&tr("Write a note to send to this device.")), &["secondary-label"]);
        hint.set_wrap(true);
        pack(&content, &hint, false, false, 0, false);
        let note = gtk::TextView::new();
        note.set_wrap_mode(gtk::WrapMode::WordChar);
        note.set_left_margin(12);
        note.set_right_margin(12);
        note.set_top_margin(12);
        note.set_bottom_margin(12);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&note));
        pack(&content, &scroll, true, true, 0, false);
        let error = Self::error_label(&content);
        dialog.add_button(&tr("Cancel"), gtk::ResponseType::Cancel);
        let send: gtk::Button = dialog.add_button(&tr("Send"), gtk::ResponseType::Ok).downcast().unwrap();
        send.add_css_class("suggested-action");
        send.set_sensitive(false);
        let buffer = note.buffer();
        let text = |buffer: &gtk::TextBuffer| buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let sb = send.downgrade();
        buffer.connect_changed(move |b| if let Some(s) = sb.upgrade() { s.set_sensitive(!text(b).trim().is_empty()) });
        let (weak, err_label, buf) = (self.me(), error.clone(), buffer.clone());
        dialog.connect_response(move |window, response| {
            if response != gtk::ResponseType::Ok {
                window.destroy();
                return;
            }
            let body = text(&buf);
            if body.trim().is_empty() {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            if let Some(problem) = ui.manager.send_error(&target) {
                err_label.set_text(&problem);
                err_label.set_visible(true);
                return;
            }
            match write_note(&body) {
                Err(text) => {
                    let short = text.rsplit_once(": ").map(|(_, t)| t.to_owned()).unwrap_or(text);
                    err_label.set_text(&tr1("Error: %s", short));
                    err_label.set_visible(true);
                }
                Ok((directory, path)) => {
                    window.destroy();
                    ui.start_transfer(&target, &[path.to_string_lossy().into_owned()], &tr("Send Note"), Some(directory));
                }
            }
        });
        dialog.set_visible(true);
        note.grab_focus();
        NoteDialog { dialog, view: note, error, send }
    }

    pub fn start_transfer(&self, target: &SendTarget, files: &[String], title: &str, temporary: Option<PathBuf>) -> TransferDialog {
        let (dialog, tracked) = self.device_dialog(title, &target.path, &target.dev, 520, 260);
        let content = dialog.content_area();
        let filename = label("");
        filename.set_ellipsize(pango::EllipsizeMode::Middle);
        filename.set_max_width_chars(48);
        pack(&content, &filename, false, false, 0, false);
        let progress = gtk::ProgressBar::builder().show_text(true).build();
        pack(&content, &progress, false, false, 0, false);
        let status = label(&tr("Connecting…"));
        status.set_wrap(true);
        pack(&content, &status, false, false, 0, false);
        let error = Self::error_label(&content);
        let button: gtk::Button = dialog.add_button(&tr("Cancel"), gtk::ResponseType::Cancel).downcast().unwrap();
        let transfer = TransferDialog { dialog: dialog.clone(), progress, status, error, filename, button, sender: Rc::default() };
        self.transfers.borrow_mut().push(transfer.clone());
        let finished = Rc::new(Cell::new(false));
        let cancelled = Rc::new(Cell::new(false));
        let owner = device_adapter(&target.dev, &target.path);

        let done: Rc<dyn Fn(Option<BtError>, bool)> = {
            let (weak, t, finished, alive, path, owner) = (self.me(), transfer.clone(), finished.clone(), tracked.alive.clone(),
                                                           target.path.clone(), owner.clone());
            let temporary = RefCell::new(temporary);
            Rc::new(move |err: Option<BtError>, was_cancelled: bool| {
                if finished.get() {
                    return;
                }
                finished.set(true);
                let Some(ui) = weak.upgrade() else { return };
                if t.sender.borrow().is_some() {
                    ui.manager.transfers.set(ui.manager.transfers.get().saturating_sub(1));
                    ui.manager.unmark_busy(&path, "send");
                }
                if let Some(directory) = temporary.borrow_mut().take() {
                    let _ = std::fs::remove_dir_all(directory);
                }
                ui.manager.refresh_ui();
                if !alive.get() {
                    return;
                }
                t.button.set_label(&tr("Close"));
                t.button.set_sensitive(true);
                let message = if was_cancelled {
                    tr("Transfer cancelled")
                } else if let Some(err) = &err {
                    t.error.set_text(&tr1("Error: %s", crate::backend::short_err(err)));
                    t.error.set_visible(true);
                    tr(if is_unavailable_error(err) {
                        "Bluetooth file transfer is unavailable. Install or enable the BlueZ OBEX service."
                    } else {
                        "File transfer failed. Make sure the receiving device accepts Bluetooth files."
                    })
                } else {
                    t.progress.set_fraction(1.0);
                    t.progress.set_text(Some("100%"));
                    tr("Transfer complete")
                };
                t.status.set_text(&message);
                ui.manager.set_event(&message, err.is_some(), Some(&owner), None);
                ui.manager.refresh_ui();
            })
        };
        let update: ProgressFn = {
            let (t, finished, cancelled, alive) = (transfer.clone(), finished.clone(), cancelled.clone(), tracked.alive.clone());
            Rc::new(move |info| {
                if !alive.get() || finished.get() || cancelled.get() {
                    return;
                }
                let base = std::path::Path::new(&info.filename).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                t.filename.set_text(&format!("{base} ({}/{})", info.index, info.count));
                t.filename.set_tooltip_text(Some(&info.filename));
                let transferred = info.completed + info.transferred;
                if info.total > 0 {
                    let fraction = (transferred as f64 / info.total as f64).clamp(0.0, 1.0);
                    t.progress.set_fraction(fraction);
                    t.progress.set_text(Some(&format!("{}%", (fraction * 100.0) as i64)));
                } else {
                    t.progress.pulse();
                }
                t.status.set_text(&tr(if info.status == "connecting" { "Connecting…" } else { "Sending…" }));
            })
        };
        let cancel = {
            let (t, finished, cancelled, alive) = (transfer.clone(), finished.clone(), cancelled.clone(), tracked.alive.clone());
            Rc::new(move || {
                let sender = t.sender.borrow().clone();
                if let (false, Some(sender)) = (finished.get(), sender) {
                    cancelled.set(true);
                    if alive.get() {
                        t.status.set_text(&tr("Cancelling…"));
                        t.button.set_sensitive(false);
                    }
                    sender.cancel();
                }
            })
        };
        let (f2, c2) = (finished.clone(), cancel.clone());
        dialog.connect_response(move |w, _| if f2.get() { w.destroy() } else { c2() });
        let c3 = cancel.clone();
        dialog.connect_unrealize(move |_| c3());
        dialog.set_visible(true);
        let mut problem = self.manager.send_error(target);
        if problem.is_none() && (files.is_empty() || files.iter().any(|p| !std::path::Path::new(p).is_file())) {
            problem = Some(tr("Select a regular file to send."));
        }
        if let Some(problem) = problem {
            done(Some(BtError::runtime(&problem)), false);
            return transfer;
        }
        self.manager.mark_busy(&target.path, "send");
        let source = self.manager.bt.adapter(Some(&owner)).map(|a| a.address).unwrap_or_default();
        let d = done.clone();
        let sender = (self.sender_factory)(&source, &target.dev.addr, files, update, Box::new(move |e, c| d(e, c)));
        transfer.sender.replace(Some(sender.clone()));
        self.manager.transfers.set(self.manager.transfers.get() + 1);
        sender.start();
        transfer
    }

    pub fn applying(&self) -> bool {
        self.applying.get()
    }
}

fn make_section(parent: &gtk::Box, count_class: Option<&str>, show_trust: bool, popup: bool, columns: &gtk::SizeGroup) -> Rc<Section> {
    let root = gtk::Box::new(gtk::Orientation::Vertical, if popup { 8 } else { 10 });
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_start(2);
    // The row's end padding plus the list border, centring the heading
    // above the rightmost column of trust switches.
    header.set_margin_end(if popup { 13 } else { 17 });
    let title = style(label(""), &["section-title"]);
    let badge = style(gtk::Label::new(Some("0")), &["count-badge"]);
    if let Some(class) = count_class {
        badge.add_css_class(class);
    }
    pack(&header, &title, false, false, 0, false);
    pack(&header, &badge, false, false, 0, false);
    if show_trust {
        let trusted = style(gtk::Label::new(Some(&tr("Trust"))), &["section-title", "trust-column"]);
        columns.add_widget(&trusted);
        pack(&header, &trusted, false, false, 0, true);
    }
    pack(&root, &header, false, false, 0, false);
    let listbox = style(gtk::ListBox::new(), &["device-list"]);
    listbox.set_selection_mode(gtk::SelectionMode::None);
    pack(&root, &listbox, false, false, 0, false);
    pack(parent, &root, false, false, 0, false);
    Rc::new(Section { root, listbox, label: title, badge, show_trust, empty: RefCell::new(None) })
}

/// `_battery_icon(level)`.
pub fn battery_icon(theme: &gtk::IconTheme, level: i64) -> String {
    let step = ((level.clamp(0, 100) as f64 / 10.0).round_ties_even() as i64) * 10;
    let legacy = if level >= 80 { "battery-full-symbolic" } else if level >= 50 { "battery-good-symbolic" }
                 else if level >= 20 { "battery-low-symbolic" } else { "battery-caution-symbolic" };
    for name in [format!("battery-level-{step}-symbolic"), legacy.into(), "battery-symbolic".into()] {
        if theme.has_icon(&name) {
            return name;
        }
    }
    "battery-symbolic".into()
}

/// `_dev_icon(device)`.
pub fn dev_icon(theme: &gtk::IconTheme, device: Option<&Device>) -> String {
    let mut names: Vec<String> = Vec::new();
    if let Some(device) = device {
        let icon = device.icon.as_str();
        if icon == "input-keyboard" || icon == "input-keyboard-symbolic" {
            // A compact keyboard glyph also supplied by GTK's fallback theme.
            names.push("preferences-desktop-keyboard-symbolic".into());
        }
        if !icon.is_empty() {
            names.push(if icon.ends_with("-symbolic") { icon.to_owned() } else { format!("{icon}-symbolic") });
            names.push(icon.to_owned());
        }
        let fallback = match (device.class >> 8) & 0x1f {
            1 => Some("computer-symbolic"),
            2 => Some("phone-symbolic"),
            4 => Some("audio-headphones-symbolic"),
            5 => Some("input-keyboard-symbolic"),
            _ => None,
        };
        if let Some(fallback) = fallback {
            names.push(fallback.into());
        }
    }
    names.extend(["bluetooth-symbolic", "bluetooth", "network-wireless-symbolic", "network-wireless"].map(String::from));
    names.into_iter().find(|n| theme.has_icon(n)).unwrap_or_else(|| "image-missing".into())
}

impl View for Ui {
    fn apply(&self, state: &StateView) {
        self.applying.set(true);
        if state.choices_changed {
            self.adapter_model.clear();
            for (path, text) in &state.adapter_choices {
                let iter = self.adapter_model.append();
                self.adapter_model.set(&iter, &[(0, path), (1, text)]);
            }
        }
        self.adapter_combo.set_active_id(state.active_adapter.as_deref());
        self.adapter_combo.set_sensitive(state.adapter_sensitive);
        self.adapter_combo.set_tooltip_text(Some(&state.adapter_tooltip));
        self.power_switch.set_active(state.power_active);
        self.power_switch.set_sensitive(state.power_sensitive);
        self.add_btn.set_sensitive(state.add_sensitive);
        self.applying.set(false);
        if let (Some(add), Some(view)) = (self.add.borrow().as_ref(), state.add.as_ref()) {
            add.adapter_lbl.set_text(&view.owner_label);
            add.adapter_lbl.set_tooltip_text(Some(&view.owner_label));
            add.scan.button.set_sensitive(view.scan_sensitive);
            add.scan.caption.set_label(&tr(if view.scanning { "Stop scan" } else { "Start scan" }));
            add.scan.icon.set_icon_name(Some(if view.scanning { "media-playback-stop-symbolic" } else { "view-refresh-symbolic" }));
            add.spinner.set_visible(view.scanning);
            if view.scanning { add.spinner.start() } else { add.spinner.stop() }
            add.status_lbl.set_text(&view.status);
        }
        self.ctrl_lbl.set_text(&state.ctrl_text);
        self.status_lbl.set_text(&state.status_text);
        self.status_lbl.set_tooltip_text(Some(&state.status_text));
        describe(&self.power_switch, &state.ctrl_text);
        if state.powered { self.power_tile.remove_css_class("off") } else { self.power_tile.add_css_class("off") }
        self.content_stack.set_visible_child_name(state.page);
        self.off_title.set_text(&state.off_title);
        self.off_description.set_visible(state.off_description_visible);
    }

    fn event(&self, event: &EventView) {
        self.event_lbl.set_text(&event.text);
        self.event_lbl.set_tooltip_text(Some(&event.tooltip));
        self.event_bar.remove_css_class("error");
        if event.error {
            self.event_bar.add_css_class("error");
        }
        self.event_icon.set_icon_name(Some(if event.add_error { "dialog-warning-symbolic" } else { "dialog-information-symbolic" }));
        self.event_icon.set_pixel_size(14);
        if let Some(add) = self.add.borrow().as_ref() {
            add.event_lbl.set_text(&event.text);
            add.event_lbl.set_tooltip_text(Some(&event.tooltip));
            add.event_lbl.set_visible(!event.text.is_empty());
            if event.add_error { add.event_lbl.add_css_class("error") } else { add.event_lbl.remove_css_class("error") }
        }
    }

    fn hide_add_event(&self) {
        if let Some(add) = self.add.borrow().as_ref() {
            add.event_lbl.set_visible(false);
        }
    }

    fn lists(&self, lists: &ListsView) {
        let old_rows = std::mem::take(&mut *self.rows.borrow_mut());
        let mut rows = HashMap::new();
        self.render_section(&self.list_connected, &lists.connected, &old_rows, &mut rows);
        self.render_section(&self.list_known, &lists.known, &old_rows, &mut rows);
        if let (Some(nearby), Some(view)) = (self.nearby(), lists.nearby.as_ref()) {
            self.render_section(&nearby, view, &old_rows, &mut rows);
        }
        *self.rows.borrow_mut() = rows;
    }

    fn sync_switches(&self, trusted: &[(String, bool)]) {
        self.applying.set(true);
        let rows = self.rows.borrow();
        for (path, value) in trusted {
            if let Some(switch) = rows.get(path).and_then(|r| r.trust_switch.as_ref()) {
                switch.set_active(*value);
            }
        }
        self.applying.set(false);
    }

    fn receive_dialog(&self, request: &Rc<ReceiveRequest>, summary: &str, body: &str) {
        let dialog = gtk::Dialog::builder().title(summary).build();
        let tracked = self.track_dialog(dialog.upcast_ref());
        dialog.set_default_size(420, -1);
        let content = dialog.content_area();
        content.set_spacing(10);
        margins(&content, 20, 24);
        pack(&content, &style(label(summary), &["power-title"]), false, false, 0, false);
        let text = label(body);
        text.set_wrap(true);
        text.set_max_width_chars(48);
        pack(&content, &text, false, false, 0, false);
        dialog.add_button(&tr("Decline"), gtk::ResponseType::Reject);
        dialog.add_button(&tr("Accept"), gtk::ResponseType::Accept).add_css_class("suggested-action");
        let (weak, r) = (Rc::downgrade(&self.manager), request.clone());
        dialog.connect_response(move |_, response| {
            if let Some(m) = weak.upgrade() {
                m.answer_receive(&r, response == gtk::ResponseType::Accept);
            }
        });
        self.manager.set_receive_prompt(request, Rc::new(tracked));
        dialog.set_visible(true);
        dialog.present();
    }
}

/// `P` re-export for callers formatting text with the UI's catalogs.
pub type Arg = P;
