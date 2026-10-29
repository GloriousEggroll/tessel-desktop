//! Battery status and charge controls, separate from power and idle policies (`battery_ui.py`).

use crate::power::BatteryModel;
use crate::power_ui::battery_ui::{BatterySection, Bluetooth};
use crate::power_ui::style::{icon, install_style, label, margins, set_icon, style};
use crate::status::{valid_percentage, whole};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::gtk;
use suite_gtk::popup::{friendly_status, header_button, popup_header, set_popup_width, HeaderAction, PopupController, POPUP_PADDING, POPUP_WIDTH};
use suite_gtk::utils::{append, headerbar, key_controller, remove_css};

/// What the windows need from the application (`BatteryApplication` attributes).
pub trait AppHooks {
    fn error(&self) -> String;
    fn bluetooth_available(&self) -> bool;
    fn open_bluetooth(&self) -> bool;
}

pub fn source_status(backend: &dyn BatteryModel) -> String {
    if !backend.power_ready() || !backend.power_source_known() {
        return tr!("Power source unavailable");
    }
    let mut source = if backend.on_battery() { tr!("On battery") } else { tr!("Plugged in") };
    if backend.has_battery() {
        if let Some(value) = valid_percentage(backend.percentage()) {
            source += &format!(" · {}%", whole(value));
        }
    }
    source
}

pub fn source_icon(backend: &dyn BatteryModel) -> &'static str {
    if !backend.power_ready() || !backend.power_source_known() {
        return "battery-missing-symbolic";
    }
    if !backend.on_battery() {
        return "ac-adapter-symbolic";
    }
    match valid_percentage(backend.percentage()) {
        None => "battery-missing-symbolic",
        Some(value) if value <= 10.0 => "battery-caution-symbolic",
        Some(value) if value <= 25.0 => "battery-low-symbolic",
        Some(value) if value >= 95.0 => "battery-full-symbolic",
        Some(_) => "battery-good-symbolic",
    }
}

/// Local, application and backend messages, made friendly and de-duplicated.
pub fn status_message(application_error: &str, backend: &dyn BatteryModel, local: &str) -> String {
    let fallback = tr!("Some battery details could not be read. See the log for details.");
    let mut lines: Vec<String> = Vec::new();
    for item in [local.to_owned(), application_error.to_owned(), backend.error()] {
        if item.is_empty() {
            continue;
        }
        let friendly = friendly_status(&item, &fallback, &[]);
        if !lines.contains(&friendly) {
            lines.push(friendly);
        }
    }
    let message = lines.join("\n");
    if message.is_empty() && backend.busy() {
        return tr!("Updating battery settings…");
    }
    message
}

fn bluetooth_callback(hooks: &Rc<dyn AppHooks>, before: Option<Rc<dyn Fn()>>) -> Bluetooth {
    if !hooks.bluetooth_available() {
        return None;
    }
    let hooks = Rc::downgrade(hooks);
    Some(Rc::new(move || {
        if let Some(before) = &before {
            before();
        }
        if let Some(hooks) = hooks.upgrade() {
            hooks.open_bluetooth();
        }
    }))
}

pub struct BatteryPopup {
    pub window: gtk::ApplicationWindow,
    pub settings_button: gtk::Button,
    pub source_icon: gtk::Image,
    pub source_label: gtk::Label,
    pub batteries: Rc<BatterySection>,
    pub status_label: gtk::Label,
    pub controller: PopupController,
    hooks: Rc<dyn AppHooks>,
    backend: Rc<dyn BatteryModel>,
    destroyed: Cell<bool>,
    message: RefCell<String>,
}

impl BatteryPopup {
    pub fn new(application: Option<&gtk::Application>, hooks: Rc<dyn AppHooks>, backend: Rc<dyn BatteryModel>,
               open_settings: Rc<dyn Fn()>, edge: &str, offset: i32) -> Rc<Self> {
        Rc::new_cyclic(|weak: &Weak<Self>| {
            let window = gtk::ApplicationWindow::builder().title(tr!("Battery")).build();
            if let Some(application) = application {
                window.set_application(Some(application));
            }
            set_popup_width(window.upcast_ref(), POPUP_WIDTH);
            style(&window, &["powermanager", "batterymanager"]);
            let css = install_style(&window);
            let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
            style(&surface, &["popup-surface"]);
            window.set_child(Some(&surface));
            let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
            margins(&content, POPUP_PADDING);
            surface.append(&content);
            let settings_button = header_button("emblem-system-symbolic", &tr!("Battery settings"), None, false);
            let owner = weak.clone();
            settings_button.connect_clicked(move |_| {
                if let Some(popup) = owner.upgrade() {
                    popup.dismiss();
                    open_settings();
                }
            });
            let heading = popup_header(&["battery-good-symbolic"], &tr!("Battery"), Some(" "),
                                       vec![HeaderAction::Widget(settings_button.clone().upcast())]);
            append(&content, &heading.root, false, false);
            let refresh = refresher(weak);
            let owner = weak.clone();
            let failed: Rc<dyn Fn(&str)> = Rc::new(move |value: &str| {
                if let Some(popup) = owner.upgrade() {
                    popup.message.replace(value.to_owned());
                }
            });
            let owner = weak.clone();
            let dismiss: Rc<dyn Fn()> = Rc::new(move || {
                if let Some(popup) = owner.upgrade() {
                    popup.dismiss();
                }
            });
            let batteries = BatterySection::new(backend.clone(), refresh.clone(), failed, bluetooth_callback(&hooks, Some(dismiss)), false);
            append(&content, &batteries.root, false, false);
            let status_label = label("", Some("status"), true);
            append(&content, &status_label, false, false);
            let controller = PopupController::new(window.upcast_ref(), move || refresh(), edge, offset, "service_apps");
            let owner = weak.clone();
            window.connect_unrealize(move |window| {
                if let Some(popup) = owner.upgrade() {
                    popup.destroyed.set(true);
                }
                remove_css(&WidgetExt::display(window), &css);
            });
            Self { window, settings_button, source_icon: heading.icon.clone(), source_label: heading.subtitle_label.clone(),
                   batteries, status_label, controller, hooks, backend, destroyed: Cell::new(false), message: RefCell::new(String::new()) }
        })
        .tap_refresh()
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        self.source_label.set_text(&source_status(&*self.backend));
        set_icon(&self.source_icon, source_icon(&*self.backend), Some(24));
        self.batteries.refresh();
        let message = status_message(&self.hooks.error(), &*self.backend, &self.message.borrow().clone());
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
        }
    }

    pub fn destroy(&self) {
        if !self.destroyed.get() {
            self.window.destroy();
        }
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed.get()
    }
}

trait TapRefresh {
    fn tap_refresh(self) -> Self;
}

impl TapRefresh for Rc<BatteryPopup> {
    fn tap_refresh(self) -> Self {
        self.refresh();
        self
    }
}

impl TapRefresh for Rc<BatterySettingsWindow> {
    fn tap_refresh(self) -> Self {
        self.refresh();
        self
    }
}

trait Refresh {
    fn refresh_now(&self);
}

impl Refresh for BatteryPopup {
    fn refresh_now(&self) {
        self.refresh();
    }
}

impl Refresh for BatterySettingsWindow {
    fn refresh_now(&self) {
        self.refresh();
    }
}

fn refresher<T: Refresh + 'static>(weak: &Weak<T>) -> Rc<dyn Fn()> {
    let weak = weak.clone();
    Rc::new(move || {
        if let Some(owner) = weak.upgrade() {
            owner.refresh_now();
        }
    })
}

pub struct BatterySettingsWindow {
    pub window: gtk::ApplicationWindow,
    pub source_icon: gtk::Image,
    pub source_label: gtk::Label,
    pub batteries: Rc<BatterySection>,
    pub status_label: gtk::Label,
    pub close_button: gtk::Button,
    hooks: Rc<dyn AppHooks>,
    backend: Rc<dyn BatteryModel>,
    destroyed: Cell<bool>,
    message: RefCell<String>,
}

impl BatterySettingsWindow {
    /// `open_bluetooth` overrides the application's launcher (Global Settings embedding).
    pub fn new(application: Option<&gtk::Application>, hooks: Rc<dyn AppHooks>, backend: Rc<dyn BatteryModel>,
               open_bluetooth: Bluetooth) -> Rc<Self> {
        Rc::new_cyclic(|weak: &Weak<Self>| {
            let title = tr!("Battery Settings");
            let window = gtk::ApplicationWindow::builder().title(&title).build();
            if let Some(application) = application {
                window.set_application(Some(application));
            }
            window.set_default_size(550, 560);
            window.set_size_request(470, 360);
            style(&window, &["powermanager", "batterymanager"]);
            let css = install_style(&window);
            window.set_titlebar(Some(&headerbar(&title)));
            let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
            margins(&root, 22);
            window.set_child(Some(&root));
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let source_icon = icon("battery-good-symbolic", 32);
            append(&heading, &source_icon, false, false);
            let titles = gtk::Box::new(gtk::Orientation::Vertical, 4);
            append(&titles, &label(&tr!("Battery & Charging"), Some("title"), false), false, false);
            let source_label = label("", Some("subtitle"), false);
            append(&titles, &source_label, false, false);
            append(&heading, &titles, true, false);
            append(&root, &heading, false, false);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroll.set_vexpand(true);
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.set_margin_end(6);
            scroll.set_child(Some(&content));
            append(&root, &scroll, true, false);
            let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
            style(&card, &["card"]);
            let owner = weak.clone();
            let failed: Rc<dyn Fn(&str)> = Rc::new(move |value: &str| {
                if let Some(window) = owner.upgrade() {
                    window.message.replace(value.to_owned());
                }
            });
            let bluetooth = open_bluetooth.or_else(|| bluetooth_callback(&hooks, None));
            let batteries = BatterySection::new(backend.clone(), refresher(weak), failed, bluetooth, true);
            margins(&batteries.root, 16);
            card.append(&batteries.root);
            append(&content, &card, false, false);
            let status_label = label("", Some("status"), true);
            append(&root, &status_label, false, false);
            let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let close_button = gtk::Button::with_label(&tr!("Close"));
            let target = window.downgrade();
            close_button.connect_clicked(move |_| {
                if let Some(window) = target.upgrade() {
                    window.destroy();
                }
            });
            footer.set_halign(gtk::Align::End);
            append(&footer, &close_button, false, false);
            append(&root, &footer, false, false);
            key_controller(&window, false, |controller, key, _, _| {
                if key == gdk::Key::Escape {
                    if let Some(window) = controller.widget().and_downcast::<gtk::Window>() {
                        window.destroy();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            let owner = weak.clone();
            window.connect_unrealize(move |window| {
                if let Some(settings) = owner.upgrade() {
                    settings.destroyed.set(true);
                }
                remove_css(&WidgetExt::display(window), &css);
            });
            Self { window, source_icon, source_label, batteries, status_label, close_button, hooks, backend,
                   destroyed: Cell::new(false), message: RefCell::new(String::new()) }
        })
        .tap_refresh()
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        self.source_label.set_text(&source_status(&*self.backend));
        set_icon(&self.source_icon, source_icon(&*self.backend), Some(32));
        self.batteries.refresh();
        let message = status_message(&self.hooks.error(), &*self.backend, &self.message.borrow().clone());
        self.status_label.set_text(&message);
        self.status_label.set_visible(!message.is_empty());
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed.get()
    }
}
