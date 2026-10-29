//! The `org.waydock.NetworkDiscovery` GTK application and its popup
//! (discovery_manager.py `create_application`).

use crate::backend::{DiscoveryBackend, Done};
use crate::browse::{Device, ServiceBrowser};
use crate::compat::{prog, ArgSpec, Outcome};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::gtk::prelude::*;
use suite_gtk::gtk::{self, gio, glib};
use suite_gtk::popup::{header_button, popup_header, set_popup_width, PopupController, POPUP_PADDING, POPUP_WIDTH};
use suite_gtk::utils::{accessible, add_css, PRIORITY_APPLICATION};

pub const APPLICATION_ID: &str = "org.waydock.NetworkDiscovery";

pub const SPEC: ArgSpec = ArgSpec {
    description: "Network device discovery controls, using the desktop's GTK4 popup styling.",
    modes: &["toggle", "popup", "settings", "status"],
};

pub const CSS: &str = "
              window.discovery-manager.tray-popup { background: transparent; }
              .discovery-manager .popup-surface { background: @theme_bg_color; color: @theme_fg_color;
                border-radius: 12px; border: 1px solid alpha(@theme_fg_color,.14); }
              .discovery-manager list.discovered { background: alpha(@theme_fg_color,.05); border-radius: 8px; }
              .discovery-manager list.discovered > row { background: transparent; }
            ";

/// What the application drives (the real [`DiscoveryBackend`] or a fake).
pub trait Backend {
    fn start(&self);
    fn close(&self);
    fn snapshot(&self) -> Map<String, Value>;
    fn ready(&self) -> bool;
    fn busy(&self) -> bool;
    fn set_enabled(&self, enabled: bool, done: Done) -> bool;
}

impl Backend for DiscoveryBackend {
    fn start(&self) { DiscoveryBackend::start(self) }
    fn close(&self) { DiscoveryBackend::close(self) }
    fn snapshot(&self) -> Map<String, Value> { DiscoveryBackend::snapshot(self) }
    fn ready(&self) -> bool { DiscoveryBackend::ready(self) }
    fn busy(&self) -> bool { DiscoveryBackend::busy(self) }
    fn set_enabled(&self, enabled: bool, done: Done) -> bool { DiscoveryBackend::set_enabled(self, enabled, done) }
}

/// The device browser (the real [`ServiceBrowser`] or a fake).
pub trait Browser {
    fn start(&self) -> bool;
    fn close(&self);
    fn running(&self) -> bool;
    fn devices(&self) -> Vec<Device>;
    fn error(&self) -> String;
}

impl Browser for ServiceBrowser {
    fn start(&self) -> bool { ServiceBrowser::start(self) }
    fn close(&self) { ServiceBrowser::close(self) }
    fn running(&self) -> bool { ServiceBrowser::running(self) }
    fn devices(&self) -> Vec<Device> { ServiceBrowser::devices(self) }
    fn error(&self) -> String { ServiceBrowser::error(self) }
}

pub type BackendFactory = Box<dyn FnOnce(Rc<dyn Fn()>) -> Rc<dyn Backend>>;
pub type BrowserFactory = Rc<dyn Fn(Rc<dyn Fn()>) -> Rc<dyn Browser>>;

pub fn default_backend() -> BackendFactory {
    Box::new(|changed| Rc::new(DiscoveryBackend::new(move || changed())) as Rc<dyn Backend>)
}

pub fn default_browser() -> BrowserFactory {
    Rc::new(|finished| Rc::new(ServiceBrowser::new(Rc::new(move |_| finished()))) as Rc<dyn Browser>)
}

fn flag(state: &Map<String, Value>, key: &str) -> bool { state.get(key) == Some(&Value::Bool(true)) }

struct AppInner {
    app: glib::WeakRef<gtk::Application>,
    backend_factory: RefCell<Option<BackendFactory>>,
    browser_factory: BrowserFactory,
    backend: RefCell<Option<Rc<dyn Backend>>>,
    popup: RefCell<Option<DiscoveryPopup>>,
    pending_toggle: Cell<bool>,
    message: RefCell<String>,
    browser: RefCell<Option<Rc<dyn Browser>>>,
    browsed_enabled: Cell<Option<bool>>,
    holds: RefCell<Vec<gio::ApplicationHoldGuard>>,
}

/// The application state shared by its signal handlers (Python `DiscoveryApplication`).
#[derive(Clone)]
pub struct Discovery(Rc<AppInner>);

impl Discovery {
    fn weak(&self) -> Weak<AppInner> { Rc::downgrade(&self.0) }

    pub fn application(&self) -> Option<gtk::Application> { self.0.app.upgrade() }
    pub fn backend(&self) -> Option<Rc<dyn Backend>> { self.0.backend.borrow().clone() }
    pub fn popup(&self) -> Option<DiscoveryPopup> { self.0.popup.borrow().clone() }
    pub fn browser(&self) -> Option<Rc<dyn Browser>> { self.0.browser.borrow().clone() }
    pub fn message(&self) -> String { self.0.message.borrow().clone() }

    fn snapshot(&self) -> Map<String, Value> { self.backend().map(|b| b.snapshot()).unwrap_or_default() }

    fn hold(&self) {
        if let Some(app) = self.application() {
            self.0.holds.borrow_mut().push(app.hold());
        }
    }

    fn release(&self) { drop(self.0.holds.borrow_mut().pop()); }

    fn startup(&self, app: &gtk::Application) {
        suite_gtk::theme::attach_application(app);
        add_css(CSS, None, PRIORITY_APPLICATION);
        let Some(factory) = self.0.backend_factory.take() else { return };
        let weak = self.weak();
        let backend = factory(Rc::new(move || {
            if let Some(inner) = weak.upgrade() {
                Discovery(inner).changed();
            }
        }));
        self.0.backend.replace(Some(backend.clone()));
        backend.start();
    }

    fn command_line(&self, command: &gio::ApplicationCommandLine) -> i32 {
        let argv: Vec<String> = command.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let name = prog(argv.first().map(String::as_str).unwrap_or("discovery_manager.py"));
        let args = match SPEC.parse(argv.get(1..).unwrap_or(&[]), true) {
            Outcome::Run(args) => args,
            Outcome::Help => {
                command.print_literal(&SPEC.help(&name));
                return 0;
            }
            Outcome::Error(message) => {
                command.printerr_literal(&SPEC.error_text(&name, &message));
                return 2;
            }
        };
        if args.mode == Some("toggle") {
            let busy = self.backend().is_some_and(|b| b.busy());
            if !self.0.pending_toggle.get() && !busy {
                self.hold();
                self.0.pending_toggle.set(true);
                self.changed();
            }
        } else {
            self.show(&args.popup_edge, args.popup_offset as i32);
        }
        0
    }

    pub fn changed(&self) {
        let Some(backend) = self.backend() else { return };
        if self.0.pending_toggle.get() && backend.ready() {
            self.0.pending_toggle.set(false);
            let state = backend.snapshot();
            let weak = self.weak();
            let done: Done = Box::new(move |success, error| {
                if let Some(inner) = weak.upgrade() {
                    Discovery(inner).finished(success, &error);
                }
            });
            if !backend.set_enabled(!flag(&state, "enabled"), done) {
                let error = state.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).map(String::from)
                    .unwrap_or_else(|| tr!("Avahi is not installed."));
                self.finished(false, &error);
            }
        }
        if let Some(popup) = self.popup() {
            let state = backend.snapshot();
            // Look for devices whenever the popup is open and discovery turns on.
            let enabled = flag(&state, "enabled");
            if flag(&state, "ready") && Some(enabled) != self.0.browsed_enabled.get() {
                self.0.browsed_enabled.set(Some(enabled));
                if enabled {
                    self.browse(false);
                }
            }
            popup.apply_state();
        }
    }

    pub fn browse(&self, update: bool) {
        let browser = self.browser().unwrap_or_else(|| {
            let weak = self.weak();
            let browser = (self.0.browser_factory)(Rc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    if let Some(popup) = Discovery(inner).popup() {
                        popup.apply_state();
                    }
                }
            }));
            self.0.browser.replace(Some(browser.clone()));
            browser
        });
        browser.start();
        if update {
            if let Some(popup) = self.popup() {
                popup.apply_state();
            }
        }
    }

    fn finished(&self, _success: bool, error: &str) {
        if !error.is_empty() {
            self.0.message.replace(error.into());
            self.show("bottom", 76);
        }
        self.release();
    }

    fn change(&self, enabled: bool) {
        self.0.message.replace(String::new());
        self.hold();
        // Dismiss the layer-shell keyboard grab before polkit asks for a
        // password; keep the operation alive even when the popup is gone.
        if let Some(popup) = self.popup() {
            popup.controller().dismiss();
        }
        let weak = self.weak();
        let done: Done = Box::new(move |success, error| {
            if let Some(inner) = weak.upgrade() {
                Discovery(inner).finished(success, &error);
            }
        });
        if !self.backend().is_some_and(|b| b.set_enabled(enabled, done)) {
            self.release();
        }
    }

    pub fn show(&self, edge: &str, offset: i32) {
        match self.popup() {
            None => {
                let Some(app) = self.application() else { return };
                let popup = DiscoveryPopup::new(self, &app, edge, offset);
                self.0.popup.replace(Some(popup.clone()));
                self.changed();
                popup.controller().show();
            }
            Some(popup) => {
                popup.controller().set_position(edge, offset);
                popup.controller().show();
            }
        }
    }

    fn hidden(&self, window: &gtk::Window) {
        // show_pending: set_position() hid it only to reappear on another edge.
        let Some(popup) = self.popup() else { return };
        if popup.window() != window || window.get_visible() || popup.controller().show_pending() {
            return;
        }
        self.0.popup.replace(None);
        self.0.browsed_enabled.set(None);
        if let Some(browser) = self.0.browser.take() {
            browser.close();
        }
        popup.controller().close();
        window.destroy();
    }

    fn shutdown(&self) {
        if let Some(backend) = self.backend() {
            backend.close();
        }
        if let Some(browser) = self.browser() {
            browser.close();
        }
        if let Some(popup) = self.popup() {
            popup.controller().close();
        }
    }
}

/// `create_application(backend_factory, browser_factory)`.
pub fn create_application(backend_factory: BackendFactory, browser_factory: BrowserFactory) -> (gtk::Application, Discovery) {
    let app = gtk::Application::builder().application_id(APPLICATION_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    let discovery = Discovery(Rc::new(AppInner {
        app: app.downgrade(), backend_factory: RefCell::new(Some(backend_factory)), browser_factory,
        backend: RefCell::new(None), popup: RefCell::new(None), pending_toggle: Cell::new(false),
        message: RefCell::default(), browser: RefCell::new(None), browsed_enabled: Cell::new(None),
        holds: RefCell::default(),
    }));
    let state = discovery.clone();
    app.connect_startup(move |app| state.startup(app));
    let state = discovery.clone();
    app.connect_command_line(move |_, command| glib::ExitCode::from(state.command_line(command) as u8));
    let state = discovery.clone();
    app.connect_activate(move |_| state.show("bottom", 76));
    let state = discovery.clone();
    app.connect_shutdown(move |_| state.shutdown());
    (app, discovery)
}

struct PopupInner {
    owner: Weak<AppInner>,
    window: gtk::ApplicationWindow,
    state_label: gtk::Label,
    switch: gtk::Switch,
    detail: gtk::Label,
    rescan: gtk::Button,
    found_status: gtk::Label,
    devices: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    syncing: Cell<bool>,
    controller: RefCell<Option<PopupController>>,
}

#[derive(Clone)]
pub struct DiscoveryPopup(Rc<PopupInner>);

fn wrapped_label(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).wrap(true).max_width_chars(40).css_classes([class]).build()
}

impl DiscoveryPopup {
    fn new(app_state: &Discovery, app: &gtk::Application, edge: &str, offset: i32) -> Self {
        let window = gtk::ApplicationWindow::builder().application(app).title(tr!("Discover Network Devices")).build();
        // Passing css_classes to the constructor would replace the classes
        // the theme adds, so add ours afterwards.
        window.add_css_class("discovery-manager");
        set_popup_width(window.upcast_ref(), POPUP_WIDTH);
        let surface = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["popup-surface"]).build();
        window.set_child(Some(&surface));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .margin_top(POPUP_PADDING).margin_bottom(POPUP_PADDING).margin_start(POPUP_PADDING).margin_end(POPUP_PADDING).build();
        surface.append(&content);
        let heading = popup_header(&["network-workgroup-symbolic"], &tr!("Network Discovery"), Some(" "), Vec::new());
        content.append(&heading.root);
        content.append(&wrapped_label(&tr!(
            "Network discovery lets this computer find printers, file shares, media players and other computers on your local network, and lets them find services this computer shares. It uses Avahi (mDNS/DNS-SD)."),
            "row-detail"));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        row.append(&gtk::Label::builder().label(tr!("Discover network devices")).xalign(0.0).hexpand(true)
            .css_classes(["row-title"]).build());
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).tooltip_text(tr!("Enable network device discovery")).build();
        accessible(&switch, &tr!("Discover network devices"));
        row.append(&switch);
        content.append(&row);
        let detail = wrapped_label("", "row-detail");
        content.append(&detail);
        let found = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        found.append(&gtk::Label::builder().label(tr!("Found on this network")).xalign(0.0).hexpand(true)
            .css_classes(["section-title"]).build());
        let weak = app_state.weak();
        let rescan = header_button("view-refresh-symbolic", &tr!("Search again"), Some(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                Discovery(inner).browse(true);
            }
        })), false);
        found.append(&rescan);
        content.append(&found);
        let found_status = wrapped_label("", "row-detail");
        content.append(&found_status);
        let devices = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list", "discovered"]).build();
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true)
            .max_content_height(260).build();
        scroll.set_child(Some(&devices));
        content.append(&scroll);
        content.append(&wrapped_label(&tr!(
            "Turning discovery on or off asks for administrator authentication. Changes are remembered after restart."),
            "row-detail"));
        let popup = Self(Rc::new(PopupInner {
            owner: app_state.weak(), window: window.clone(), state_label: heading.subtitle_label.clone(), switch: switch.clone(),
            detail, rescan, found_status, devices, scroll, syncing: Cell::new(false), controller: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&popup.0);
        switch.connect_state_set(move |_, enabled| {
            if let Some(inner) = weak.upgrade() {
                if !inner.syncing.get() {
                    if let Some(owner) = inner.owner.upgrade() {
                        Discovery(owner).change(enabled);
                    }
                }
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(&popup.0);
        let controller = PopupController::new(window.upcast_ref(), move || {
            if let Some(inner) = weak.upgrade() {
                DiscoveryPopup(inner).apply_state();
            }
        }, edge, offset, "service_apps");
        popup.0.controller.replace(Some(controller));
        let owner = app_state.weak();
        window.connect_visible_notify(move |window| {
            if !window.get_visible() {
                let (owner, window) = (owner.clone(), window.clone());
                glib::idle_add_local_once(move || {
                    if let Some(inner) = owner.upgrade() {
                        Discovery(inner).hidden(window.upcast_ref());
                    }
                });
            }
        });
        popup
    }

    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }
    pub fn controller(&self) -> PopupController { self.0.controller.borrow().clone().unwrap() }
    pub fn switch(&self) -> &gtk::Switch { &self.0.switch }
    pub fn state_label(&self) -> &gtk::Label { &self.0.state_label }
    pub fn detail(&self) -> &gtk::Label { &self.0.detail }
    pub fn rescan(&self) -> &gtk::Button { &self.0.rescan }
    pub fn found_status(&self) -> &gtk::Label { &self.0.found_status }
    pub fn devices(&self) -> &gtk::ListBox { &self.0.devices }
    pub fn scroll(&self) -> &gtk::ScrolledWindow { &self.0.scroll }

    pub fn apply_state(&self) {
        let Some(owner) = self.0.owner.upgrade().map(Discovery) else { return };
        let state = owner.snapshot();
        let (enabled, ready, installed, busy) = (flag(&state, "enabled"), flag(&state, "ready"), flag(&state, "installed"),
                                                 flag(&state, "busy"));
        self.0.syncing.set(true);
        self.0.switch.set_active(enabled);
        self.0.switch.set_state(enabled);
        self.0.switch.set_sensitive(ready && installed && !busy);
        self.0.syncing.set(false);
        let summary = if busy {
            "Waiting for authentication…"
        } else if !ready {
            "Checking…"
        } else if !installed {
            "Avahi is not installed."
        } else if enabled {
            "Discovery is on."
        } else {
            "Discovery is off."
        };
        self.0.state_label.set_text(&tr!(summary));
        let message = owner.message();
        let problem = if !message.is_empty() { message } else { state.get("error").and_then(Value::as_str).unwrap_or("").to_owned() };
        self.0.detail.set_text(&if problem.is_empty() { String::new() } else { tr!(problem) });
        self.0.detail.set_visible(!problem.is_empty());
        self.apply_devices(&owner, enabled);
    }

    fn apply_devices(&self, owner: &Discovery, enabled: bool) {
        let browser = owner.browser();
        let searching = browser.as_ref().is_some_and(|b| b.running());
        self.0.rescan.set_sensitive(enabled && !searching);
        while let Some(child) = self.0.devices.first_child() {
            self.0.devices.remove(&child);
        }
        let devices = match &browser {
            Some(browser) if enabled => browser.devices(),
            _ => Vec::new(),
        };
        for device in &devices {
            let entry = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2)
                .margin_top(8).margin_bottom(8).margin_start(12).margin_end(12).build();
            entry.append(&gtk::Label::builder().label(&device.name).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End)
                .tooltip_text(&device.name).css_classes(["row-title"]).build());
            let mut summary = device.services.iter().map(|s| tr!(s)).collect::<Vec<_>>().join(", ");
            if !device.address.is_empty() {
                summary.push_str(" · ");
                summary.push_str(&device.address);
            }
            entry.append(&wrapped_label(&summary, "row-detail"));
            self.0.devices.append(&entry);
        }
        self.0.scroll.set_visible(!devices.is_empty());
        let message = if !devices.is_empty() && !searching {
            String::new()
        } else if !enabled {
            "Turn on discovery to see devices on this network.".into()
        } else if searching {
            "Searching…".into()
        } else if let Some(browser) = browser.as_ref().filter(|b| !b.error().is_empty()) {
            browser.error()
        } else if browser.is_some() {
            "No devices or shared services were found.".into()
        } else {
            String::new()
        };
        self.0.found_status.set_text(&if message.is_empty() { String::new() } else { tr!(message) });
        self.0.found_status.set_visible(!message.is_empty());
    }
}
