//! The keyboard controls, also embeddable in Global Settings (osk_settings.py).
use crate::config::{Config, Settings, LAYOUTS, SIZES};
use crate::devices::{discover, Device, DeviceMonitor};
use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::gtk;
use tessel_gtk::utils::{add_css, remove_children, remove_css, set_margins, PRIORITY_APPLICATION};

/// One title size and card radius with the other settings pages (global_style.PAGE_CSS
/// fallback used by standalone installs).
pub const CSS: &str = concat!(".gs-page-title { font-size: 24px; font-weight: 700; } ",
    ".gs-card { padding: 16px; border-radius: 12px; border: 1px solid alpha(@theme_fg_color,.12); ",
    "background-color: alpha(@theme_fg_color,.035); } .gs-row-subtitle { font-size: 9.5pt; }",
    " .keyboard-settings .detail { opacity: .72; }");

/// Breeze's 32px "input-keyboard-symbolic" is its colour art, which GTK recolours
/// into a solid square at header sizes; GTK's own symbolic file is used instead.
pub const KEYBOARD_ICON: &str = "/usr/share/icons/Adwaita/symbolic/devices/input-keyboard-symbolic.svg";

pub fn keyboard_icon(size: i32) -> gtk::Image {
    let icon = if std::path::Path::new(KEYBOARD_ICON).exists() {
        gtk::Image::from_gicon(&gio::FileIcon::new(&gio::File::for_path(KEYBOARD_ICON)))
    } else {
        gtk::Image::from_icon_name("input-keyboard-symbolic")
    };
    icon.set_pixel_size(size);
    icon
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(tr!(text)).xalign(0.0).wrap(true).max_width_chars(68).build();
    label.add_css_class("detail");
    label
}

/// A titled row; `detail = Some("")` adds an empty subtitle label.
fn row(title: &str, widget: &impl IsA<gtk::Widget>, detail: Option<&str>) -> (gtk::Box, Option<gtk::Label>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(3).hexpand(true).build();
    text.append(&gtk::Label::builder().label(tr!(title)).xalign(0.0).wrap(true).build());
    let subtitle = detail.map(|detail| {
        let subtitle = gtk::Label::builder().label(if detail.is_empty() { String::new() } else { tr!(detail) }).xalign(0.0)
            .wrap(true).max_width_chars(60).css_classes(["detail", "gs-row-subtitle"]).build();
        text.append(&subtitle);
        subtitle
    });
    row.append(&text);
    widget.set_valign(gtk::Align::Center);
    row.append(widget);
    (row, subtitle)
}

pub struct Inner {
    pub root: gtk::Box,
    pub automatic: gtk::Switch,
    pub layout: gtk::DropDown,
    pub size: gtk::DropDown,
    pub devices: gtk::Box,
    pub status: gtk::Label,
    layout_detail: gtk::Label,
    layouts: Vec<(&'static str, &'static str)>,
    store: Config,
    config: RefCell<Settings>,
    updating: Cell<bool>,
    provider: gtk::CssProvider,
    monitor: RefCell<Option<DeviceMonitor>>,
    environment: Option<std::path::PathBuf>,
}

#[derive(Clone)]
pub struct KeyboardSettings(Rc<Inner>);

impl KeyboardSettings {
    pub fn new(store: Option<Config>) -> Self { Self::with_devices(store, true, None) }

    /// `watch_devices = false` and a custom `environment` file are for tests.
    pub fn with_devices(store: Option<Config>, watch_devices: bool, environment: Option<std::path::PathBuf>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        set_margins(&root, 24);
        let store = store.unwrap_or_default();
        let config = store.load().unwrap_or_default();
        let provider = add_css(CSS, None, PRIORITY_APPLICATION);
        root.add_css_class("keyboard-settings");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let icon = keyboard_icon(24);
        icon.set_valign(gtk::Align::Center);
        heading.append(&icon);
        let title = gtk::Label::builder().label(tr!("On-screen Keyboard")).xalign(0.0).build();
        title.add_css_class("gs-page-title");
        heading.append(&title);
        root.append(&heading);
        root.append(&label("Show a touch keyboard when a touchscreen is connected and no physical keyboard is available. The dock icon can also show it manually."));
        let controls = gtk::Box::new(gtk::Orientation::Vertical, 16);
        controls.add_css_class("gs-card");
        root.append(&controls);
        let automatic = gtk::Switch::builder().active(config.automatic).valign(gtk::Align::Center).build();
        controls.append(&row("Show automatically", &automatic,
            Some("Open when you tap a text field on a touchscreen and no physical keyboard is connected.")).0);
        let layouts: Vec<(&str, &str)> = [("system", "System keyboard layout")].into_iter().chain(LAYOUTS).collect();
        let names: Vec<String> = layouts.iter().map(|(_, l)| tr!(*l)).collect();
        let layout = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
        layout.set_selected(layouts.iter().position(|(k, _)| *k == config.layout).unwrap_or(0) as u32);
        let (layout_row, layout_detail) = row("Keyboard layout", &layout, Some(""));
        controls.append(&layout_row);
        let sizes = [tr!("Small"), tr!("Medium"), tr!("Large")];
        let size = gtk::DropDown::from_strings(&sizes.iter().map(String::as_str).collect::<Vec<_>>());
        size.set_selected(SIZES.iter().position(|s| *s == config.size).unwrap_or(1) as u32);
        controls.append(&row("Key size", &size,
            Some("Larger keys are easier to tap; smaller keys leave more of the screen visible.")).0);
        let status = label("");
        root.append(&status);
        let devices = gtk::Box::new(gtk::Orientation::Vertical, 12);
        devices.add_css_class("gs-card");
        root.append(&devices);
        let settings = Self(Rc::new(Inner {
            root, automatic: automatic.clone(), layout: layout.clone(), size: size.clone(), devices, status,
            layout_detail: layout_detail.unwrap(), layouts, store, config: RefCell::new(config), updating: Cell::new(false),
            provider, monitor: RefCell::new(None), environment }));
        settings.update_layout_detail();
        let weak = settings.downgrade();
        automatic.connect_active_notify(move |switch| if let Some(s) = upgrade(&weak) { s.save("automatic", Value::Bool(switch.is_active())) });
        let weak = settings.downgrade();
        layout.connect_selected_notify(move |dropdown| if let Some(s) = upgrade(&weak) {
            let key = s.0.layouts.get(dropdown.selected() as usize).map_or("system", |(k, _)| *k);
            s.save("layout", Value::from(key));
            s.update_layout_detail();
        });
        let weak = settings.downgrade();
        size.connect_selected_notify(move |dropdown| if let Some(s) = upgrade(&weak) {
            let key = SIZES.get(dropdown.selected() as usize).copied().unwrap_or("medium");
            s.save("size", Value::from(key));
        });
        if watch_devices {
            let weak = settings.downgrade();
            *settings.0.monitor.borrow_mut() = Some(DeviceMonitor::new(move |devices| if let Some(s) = upgrade(&weak) {
                s.render_devices(&devices)
            }));
            settings.render_devices(&discover());
        } else {
            settings.render_devices(&[]);
        }
        settings
    }

    fn downgrade(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }
    pub fn root(&self) -> &gtk::Box { &self.0.root }
    pub fn automatic(&self) -> &gtk::Switch { &self.0.automatic }
    pub fn layout(&self) -> &gtk::DropDown { &self.0.layout }
    pub fn size(&self) -> &gtk::DropDown { &self.0.size }
    pub fn status(&self) -> &gtk::Label { &self.0.status }
    pub fn layout_detail(&self) -> &gtk::Label { &self.0.layout_detail }
    pub fn config(&self) -> Settings { self.0.config.borrow().clone() }

    /// Global Settings search anchors (settings_search.py).
    pub fn search_targets(&self) -> HashMap<&'static str, gtk::Widget> {
        HashMap::from([("keyboard-automatic", self.0.automatic.clone().upcast()), ("keyboard-layout", self.0.layout.clone().upcast()),
                       ("keyboard-size", self.0.size.clone().upcast()), ("keyboard-devices", self.0.devices.clone().upcast())])
    }

    fn update_layout_detail(&self) {
        let selected = self.0.layouts.get(self.0.layout.selected() as usize).map_or("system", |(k, _)| *k);
        if selected == "system" {
            let layouts = crate::config::system_layouts(self.0.environment.as_deref()).join(", ");
            self.0.layout_detail.set_text(&tr!("Follows your physical keyboard layout in Keyboard settings (now: {layout}).", layout = layouts));
        } else {
            self.0.layout_detail.set_text(&tr!("Use this layout on the on-screen keyboard, whatever your physical keyboard uses."));
        }
    }

    pub fn save(&self, key: &str, value: Value) {
        if self.0.updating.get() {
            return;
        }
        let result = self.0.store.load().and_then(|current| {
            let mut data = current.to_value();
            data[key] = value;
            self.0.store.save(&data)
        });
        match result {
            Ok(config) => {
                *self.0.config.borrow_mut() = config;
                self.0.status.set_text(&tr!("Changes apply immediately."));
            }
            Err(error) => self.0.status.set_text(&error),
        }
    }

    pub fn render_devices(&self, devices: &[Device]) {
        self.0.updating.set(true);
        let container = &self.0.devices;
        remove_children(container);
        let touches: Vec<&str> = devices.iter().filter(|d| d.kind == "touchscreen").map(|d| d.name.as_str()).collect();
        let keyboards: Vec<&Device> = devices.iter().filter(|d| d.kind == "keyboard").collect();
        container.append(&gtk::Label::builder().label(tr!("Connected input devices")).xalign(0.0).build());
        let names = if touches.is_empty() { tr!("None detected") } else { touches.join(", ") };
        // Python passes the already translated text through tr() again.
        container.append(&label(&tr!("Touchscreens: {value1}", value1 = names)));
        container.append(&label("Turn off a device below if it is a receiver or accessory that incorrectly reports itself as a keyboard."));
        let ignored = self.0.config.borrow().ignore_keyboards.clone();
        for device in &keyboards {
            let toggle = gtk::Switch::builder().active(!ignored.contains(&device.identity)).valign(gtk::Align::Center).build();
            toggle.set_tooltip_text(Some(&tr!("Count this device as a physical keyboard")));
            let (weak, identity) = (self.downgrade(), device.identity.clone());
            toggle.connect_active_notify(move |switch| if let Some(s) = upgrade(&weak) { s.ignore(&identity, !switch.is_active()) });
            container.append(&row(&device.name, &toggle, Some("On: counts as a physical keyboard, so the on-screen keyboard stays hidden.")).0);
        }
        if keyboards.is_empty() {
            container.append(&label("No physical keyboards detected."));
        }
        self.0.updating.set(false);
    }

    pub fn ignore(&self, identity: &str, ignored: bool) {
        if self.0.updating.get() {
            return;
        }
        let mut values: BTreeSet<String> = match self.0.store.load() {
            Ok(config) => config.ignore_keyboards.into_iter().collect(),
            Err(error) => return self.0.status.set_text(&error),
        };
        if ignored {
            values.insert(identity.into());
        } else {
            values.remove(identity);
        }
        self.save("ignore_keyboards", Value::from(values.into_iter().collect::<Vec<_>>()));
    }

    pub fn close(&self) {
        if let Some(monitor) = self.0.monitor.borrow_mut().take() {
            monitor.close();
        }
        remove_css(&self.0.root.display(), &self.0.provider);
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<KeyboardSettings> { weak.upgrade().map(KeyboardSettings) }

#[allow(unused_imports)]
use glib as _;
