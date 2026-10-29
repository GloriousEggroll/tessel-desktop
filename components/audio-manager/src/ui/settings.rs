//! The Audio Settings window (audio_ui.py:770-942).

#![allow(deprecated)]

use super::common::*;
use super::rows::{DeviceRow, LoopbackDialog, LoopbackRow, NodeRow};
use crate::api::{Action, AudioApi};
use crate::model::Node;
use gtk::prelude::*;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::gtk;
use suite_gtk::utils::{append, children, headerbar};

/// (key, title, icon, media class / "loopbacks" / None = devices, empty text).
/// Some empty texts are untranslated literals in Python; kept for parity.
pub fn pages() -> Vec<(&'static str, String, &'static str, Option<&'static str>, String)> {
    vec![
        ("playback", tr!("Playback"), "audio-volume-high-symbolic", Some("Stream/Output/Audio"), "No applications are playing audio.".into()),
        ("recording", tr!("Recording"), "audio-input-microphone-symbolic", Some("Stream/Input/Audio"), "No applications are recording audio.".into()),
        ("outputs", tr!("Output Devices"), "audio-speakers-symbolic", Some("Audio/Sink"), "No output devices available.".into()),
        ("inputs", tr!("Input Devices"), "audio-input-microphone-symbolic", Some("Audio/Source"), "No input devices available.".into()),
        ("configuration", tr!("Configuration"), "audio-card-symbolic", None, "No audio devices found.".into()),
        ("loopbacks", tr!("Loopbacks"), "media-playlist-repeat-symbolic", Some("loopbacks"), tr!("No loopbacks yet. Add a virtual output for a speaker or headset.")),
    ]
}

pub struct AudioSettingsWindow {
    weak: RefCell<Weak<AudioSettingsWindow>>,
    backend: Rc<dyn AudioApi>,
    pub window: gtk::ApplicationWindow,
    pub stack: gtk::Stack,
    pub navigation: gtk::ListBox,
    pub node_rows: RefCell<IndexMap<i64, Rc<NodeRow>>>,
    pub device_rows: RefCell<IndexMap<i64, Rc<DeviceRow>>>,
    pub loopback_rows: RefCell<IndexMap<String, Rc<LoopbackRow>>>,
    pub loopback_dialog: RefCell<Option<Rc<LoopbackDialog>>>,
    pub pages: RefCell<IndexMap<&'static str, (gtk::Box, gtk::Label)>>,
    pub add_loopback_button: gtk::Button,
    pub status_label: gtk::Label,
    /// `window._settings_host`: the Global Settings window that embeds these
    /// controls; dialogs are transient for it instead of the hidden window.
    pub settings_host: RefCell<Option<gtk::Window>>,
    css: gtk::CssProvider,
}

impl View for AudioSettingsWindow {
    fn backend(&self) -> Rc<dyn AudioApi> {
        self.backend.clone()
    }
    /// `perform(action, *arguments)`: exceptions become the status error.
    fn perform(&self, action: Action) {
        if let Err(error) = action.run(&*self.backend) {
            self.set_error(&error);
        }
    }
    fn set_error(&self, message: &str) {
        self.status_label.set_text(message);
        self.status_label.set_tooltip_text(Some(message));
        style(&self.status_label, &["error"]);
    }
}

impl AudioSettingsWindow {
    pub fn new(application: &gtk::Application, backend: Rc<dyn AudioApi>) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(application);
        window.set_title(Some(&tr!("Audio Settings")));
        window.set_default_size(980, 740);
        window.set_size_request(780, 560);
        style(&window, &["audiomanager"]);
        let css = gtk::CssProvider::new();
        css.load_from_string(STYLE_CSS);
        gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let header = headerbar(&tr!("Audio Settings"));
        window.set_titlebar(Some(&header));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 20);
        margins(&root, 24);
        window.set_child(Some(&root));
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 6);
        append(&heading, &label(&tr!("Sound"), Some("title")), false, false);
        append(&heading, &label(&tr!("Manage devices and application audio."), Some("subtitle")), false, false);
        append(&root, &heading, false, false);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        body.set_vexpand(true);
        append(&root, &body, true, false);
        let navigation = gtk::ListBox::new();
        style(&navigation, &["navigation"]);
        navigation.set_size_request(190, -1);
        navigation.set_valign(gtk::Align::Start);
        append(&body, &navigation, false, false);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);
        append(&body, &stack, true, false);
        let add_loopback_button = gtk::Button::with_label(&tr!("Add Loopback"));
        style(&add_loopback_button, &["primary"]);
        add_loopback_button.set_valign(gtk::Align::Start);
        let mut page_map = IndexMap::new();
        let mut names = Vec::new();
        for (key, title, symbol, _class, empty_text) in pages() {
            let nav = gtk::ListBoxRow::new();
            let nav_content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            append(&nav_content, &icon(Some(symbol), 18), false, false);
            append(&nav_content, &label(&title, None), false, false);
            nav.set_child(Some(&nav_content));
            navigation.append(&nav);
            names.push(key);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            page.set_margin_end(6);
            let title_label = label(&title, Some("section-title"));
            title_label.set_margin_bottom(14);
            if key == "loopbacks" {
                let page_heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                append(&page_heading, &title_label, true, false);
                append(&page_heading, &add_loopback_button, false, false);
                append(&page, &page_heading, false, false);
                let helper = label(&tr!("Virtual outputs that reconnect to their saved devices."), Some("muted"));
                helper.set_margin_bottom(16);
                append(&page, &helper, false, false);
            } else {
                append(&page, &title_label, false, false);
            }
            let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
            append(&page, &rows, false, false);
            let empty = gtk::Label::new(Some(&empty_text));
            style(&empty, &["empty"]);
            empty.set_wrap(true);
            empty.set_justify(gtk::Justification::Center);
            append(&page, &empty, true, false);
            scroll.set_child(Some(&page));
            stack.add_named(&scroll, Some(key));
            page_map.insert(key, (rows, empty));
        }
        let stack_ref = stack.clone();
        navigation.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                if let Some(name) = names.get(row.index() as usize) {
                    stack_ref.set_visible_child_name(name);
                }
            }
        });
        navigation.select_row(navigation.row_at_index(0).as_ref());
        let status_label = label("", Some("status"));
        append(&root, &status_label, false, false);
        let this = Rc::new(Self {
            weak: RefCell::new(Weak::new()), backend, window, stack, navigation, node_rows: RefCell::default(), device_rows: RefCell::default(),
            loopback_rows: RefCell::default(), loopback_dialog: RefCell::new(None), pages: RefCell::new(page_map), add_loopback_button,
            status_label, settings_host: RefCell::new(None), css,
        });
        *this.weak.borrow_mut() = Rc::downgrade(&this);
        let weak = Rc::downgrade(&this);
        this.add_loopback_button.connect_clicked(move |_| if let Some(w) = weak.upgrade() { w.add_loopback() });
        let weak = Rc::downgrade(&this);
        this.window.connect_unrealize(move |window| {
            if let Some(this) = weak.upgrade() {
                gtk::style_context_remove_provider_for_display(&WidgetExt::display(window), &this.css);
            }
        });
        this.refresh();
        this
    }

    fn view(&self) -> Weak<dyn View> {
        let weak: Weak<dyn View> = self.weak.borrow().clone();
        weak
    }

    pub fn add_loopback(&self) {
        if self.loopback_dialog.borrow().is_none() {
            let host = self.settings_host.borrow().clone();
            let parent = host.unwrap_or_else(|| self.window.clone().upcast());
            let dialog = LoopbackDialog::new(self.view(), &parent);
            let weak = self.weak.borrow().clone();
            dialog.dialog.connect_unrealize(move |widget| {
                if let Some(this) = weak.upgrade() {
                    let current = this.loopback_dialog.borrow().as_ref().map(|d| d.dialog.clone());
                    if current.as_ref() == Some(widget) {
                        this.loopback_dialog.replace(None);
                    }
                }
            });
            *self.loopback_dialog.borrow_mut() = Some(dialog);
        }
        let dialog = self.loopback_dialog.borrow().as_ref().map(|d| d.dialog.clone());
        if let Some(dialog) = dialog {
            dialog.set_visible(true);
            dialog.present();
        }
    }

    fn place(rows: &gtk::Box, widget: &gtk::Box, position: usize) {
        let previous = if position > 0 { children(rows).get(position - 1).cloned() } else { None };
        rows.reorder_child_after(widget, previous.as_ref());
    }

    pub fn refresh(&self) {
        let state = snapshot(&*self.backend);
        let live_nodes: IndexMap<i64, Node> = state.nodes.iter().filter(|(_, n)| !n.loopback_internal).map(|(k, v)| (*k, v.clone())).collect();
        let stale: Vec<i64> = self.node_rows.borrow().iter().filter(|(id, row)| {
            let old = row.node.borrow();
            live_nodes.get(*id).is_none_or(|n| n.serial != old.serial || n.media_class != old.media_class)
        }).map(|(id, _)| *id).collect();
        for id in stale {
            if let Some(row) = self.node_rows.borrow_mut().shift_remove(&id) {
                row.destroyed();
                if let Some(parent) = row.root.parent().and_downcast::<gtk::Box>() {
                    parent.remove(&row.root);
                }
            }
        }
        let stale: Vec<i64> = self.device_rows.borrow().iter()
            .filter(|(id, row)| state.devices.get(*id).is_none_or(|d| d.serial != row.device.borrow().serial)).map(|(id, _)| *id).collect();
        for id in stale {
            if let Some(row) = self.device_rows.borrow_mut().shift_remove(&id) {
                if let Some(parent) = row.root.parent().and_downcast::<gtk::Box>() {
                    parent.remove(&row.root);
                }
            }
        }
        let stale: Vec<String> = self.loopback_rows.borrow().keys().filter(|id| !state.loopbacks.contains_key(*id)).cloned().collect();
        for id in stale {
            if let Some(row) = self.loopback_rows.borrow_mut().shift_remove(&id) {
                if let Some(parent) = row.root.parent().and_downcast::<gtk::Box>() {
                    parent.remove(&row.root);
                }
            }
        }
        let page_widgets = self.pages.borrow().clone();
        for (key, _title, _symbol, media_class, _empty) in pages() {
            let (rows, empty) = &page_widgets[key];
            let count;
            if key == "loopbacks" {
                let mut objects: Vec<_> = state.loopbacks.values().cloned().collect();
                objects.sort_by_key(|l| (casefold(&l.name), l.id.clone()));
                count = objects.len();
                for (position, loopback) in objects.iter().enumerate() {
                    let existing = self.loopback_rows.borrow().get(&loopback.id).cloned();
                    let row = match existing {
                        Some(row) => {
                            row.refresh(loopback);
                            row
                        }
                        None => {
                            let row = LoopbackRow::new(self.view(), loopback);
                            self.loopback_rows.borrow_mut().insert(loopback.id.clone(), row.clone());
                            append(rows, &row.root, false, false);
                            row.root.set_visible(true);
                            row.refresh(loopback);
                            row
                        }
                    };
                    // Preserve widgets, focus and open menus across state changes.
                    Self::place(rows, &row.root, position);
                }
            } else if media_class.is_none() {
                let mut objects: Vec<_> = state.devices.values().cloned().collect();
                objects.sort_by_key(|d| (casefold(&d.description), d.id));
                count = objects.len();
                for (position, device) in objects.iter().enumerate() {
                    let existing = self.device_rows.borrow().get(&device.id).cloned();
                    let row = match existing {
                        Some(row) => {
                            row.refresh(device);
                            row
                        }
                        None => {
                            let row = DeviceRow::new(self.view(), device);
                            self.device_rows.borrow_mut().insert(device.id, row.clone());
                            append(rows, &row.root, false, false);
                            row.root.set_visible(true);
                            row.refresh(device);
                            row
                        }
                    };
                    Self::place(rows, &row.root, position);
                }
            } else {
                let class = media_class.unwrap();
                let mut objects: Vec<_> = live_nodes.values().filter(|n| device_class(n) == class).cloned().collect();
                objects.sort_by_key(|n| (casefold(if n.application.is_empty() { &n.description } else { &n.application }), n.id));
                count = objects.len();
                for (position, node) in objects.iter().enumerate() {
                    let existing = self.node_rows.borrow().get(&node.id).cloned();
                    let row = match existing {
                        Some(row) => {
                            row.refresh(node);
                            row
                        }
                        None => {
                            let row = NodeRow::new(self.view(), node, false);
                            self.node_rows.borrow_mut().insert(node.id, row.clone());
                            append(rows, &row.root, false, false);
                            row.root.set_visible(true);
                            row.refresh(node);
                            row
                        }
                    };
                    Self::place(rows, &row.root, position);
                }
            }
            empty.set_visible(count == 0);
        }
        self.add_loopback_button.set_sensitive(state.ready);
        let dialog = self.loopback_dialog.borrow().clone();
        if let Some(dialog) = dialog {
            dialog.refresh();
        }
        let error = self.backend.state().error.clone();
        if !error.is_empty() {
            self.set_error(&error);
        } else {
            self.status_label.remove_css_class("error");
            let text = if state.ready { tr!("Changes take effect immediately.") } else { tr!("Connecting to the audio service…") };
            self.status_label.set_text(&text);
            self.status_label.set_tooltip_text(Some(&text));
        }
    }

    /// `window._css` (released by Global Settings when the embedded page closes).
    pub fn css_provider(&self) -> &gtk::CssProvider {
        &self.css
    }

    pub fn page_titles(&self) -> Vec<String> {
        pages().into_iter().map(|p| p.1).collect()
    }
}
