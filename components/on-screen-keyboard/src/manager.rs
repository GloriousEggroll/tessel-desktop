//! Waykeys service: a themed automatic touch keyboard for the labwc session (osk_manager.py).
use suite_core::values::truthy;
use crate::backend::{Command, InputBridge};
use crate::config::{environment_path, layout_name, Config, Settings};
use crate::devices::{automatic_available, discover, Device, DeviceMonitor};
use crate::keyboard::Keyboard;
use crate::session::LockMonitor;
use crate::settings::KeyboardSettings;
use crate::state::Visibility;
use crate::status::state_path;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::tr;
use suite_gtk::app_menu::{install_app_menu, AboutDetails, MenuTarget};
use suite_gtk::gtk;
use suite_gtk::placement::PointerMonitor;
use suite_gtk::theme::attach;
use suite_gtk::utils::headerbar;

pub const APPLICATION_ID: &str = "org.waydock.OnScreenKeyboard";
pub const FLAGS: [&str; 6] = ["--background", "--toggle", "--show", "--hide", "--settings", "--quit"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    Settings,
    Background,
    Hide,
    Show,
    Toggle,
}

/// `do_command_line`: unknown arguments → `None` (exit status 2). Precedence:
/// quit > settings > background > hide > show > toggle (also with no arguments).
pub fn parse_arguments<S: AsRef<str>>(args: &[S]) -> Option<Action> {
    let has = |flag: &str| args.iter().any(|a| a.as_ref() == flag);
    if args.iter().any(|a| !FLAGS.contains(&a.as_ref())) {
        return None;
    }
    Some(if has("--quit") { Action::Quit } else if has("--settings") { Action::Settings } else if has("--background") {
        Action::Background } else if has("--hide") { Action::Hide } else if has("--show") { Action::Show } else { Action::Toggle })
}

/// Panel width for a monitor: `min(1100, max(300, width - 24))`, narrow below 700.
pub fn panel_width(monitor_width: i32) -> (i32, bool) {
    let width = 1100.min(300.max(monitor_width - 24));
    (width, width < 700)
}

pub struct Options {
    pub application_id: String,
    pub device_loader: Rc<dyn Fn() -> Vec<Device>>,
    /// Udev hotplug monitor (tests turn it off, like patching DeviceMonitor).
    pub watch_devices: bool,
    pub config: Config,
    pub helper: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self { application_id: APPLICATION_ID.into(), device_loader: Rc::new(discover), watch_devices: true,
               config: Config::default(), helper: None }
    }
}

pub struct Manager {
    pub app: gtk::Application,
    options: Options,
    config: RefCell<Settings>,
    policy: RefCell<Visibility>,
    devices: RefCell<Vec<Device>>,
    bridge: RefCell<Option<InputBridge>>,
    panel: RefCell<Option<gtk::ApplicationWindow>>,
    launcher: RefCell<Option<gtk::ApplicationWindow>>,
    keyboard: RefCell<Option<Keyboard>>,
    settings_window: RefCell<Option<(gtk::ApplicationWindow, KeyboardSettings)>>,
    error: RefCell<String>,
    keys: RefCell<Value>,
    purpose: Cell<u64>,
    bridge_ready: Cell<bool>,
    bridge_failed: Cell<bool>,
    suspended: Cell<bool>,
    closed: Cell<bool>,
    started: Cell<bool>,
    monitor: RefCell<Option<gdk::Monitor>>,
    reload_id: RefCell<Option<glib::SourceId>>,
    status: RefCell<Option<Value>>,
    sent_layout: RefCell<Option<String>>,
    geometry_watch: RefCell<Vec<(gdk::Monitor, glib::SignalHandlerId)>>,
    monitors: RefCell<Option<(gio::ListModel, glib::SignalHandlerId)>>,
    pointer_probe: RefCell<Option<PointerMonitor>>,
    file_watch: RefCell<Option<gio::FileMonitor>>,
    environment_watch: RefCell<Option<gio::FileMonitor>>,
    lock_watch: RefCell<Option<LockMonitor>>,
    device_watch: RefCell<Option<DeviceMonitor>>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
}

impl Manager {
    pub fn new(options: Options) -> Rc<Self> {
        let app = gtk::Application::new(Some(&options.application_id), suite_core::single_instance::application_flags(false));
        let manager = Rc::new(Self {
            app: app.clone(), options, config: RefCell::new(Settings::default()), policy: RefCell::new(Visibility::default()),
            devices: RefCell::default(), bridge: RefCell::default(), panel: RefCell::default(), launcher: RefCell::default(),
            keyboard: RefCell::default(), settings_window: RefCell::default(), error: RefCell::default(),
            keys: RefCell::new(Value::Object(Map::new())), purpose: Cell::new(0), bridge_ready: Cell::new(false),
            bridge_failed: Cell::new(false), suspended: Cell::new(false), closed: Cell::new(false), started: Cell::new(false),
            monitor: RefCell::default(), reload_id: RefCell::default(), status: RefCell::default(), sent_layout: RefCell::default(),
            geometry_watch: RefCell::default(), monitors: RefCell::default(), pointer_probe: RefCell::default(),
            file_watch: RefCell::default(), environment_watch: RefCell::default(), lock_watch: RefCell::default(),
            device_watch: RefCell::default(), hold: RefCell::default() });
        let weak = Rc::downgrade(&manager);
        app.connect_startup(move |_| if let Some(m) = weak.upgrade() { m.startup() });
        let weak = Rc::downgrade(&manager);
        app.connect_command_line(move |_, command| {
            let args: Vec<String> = command.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
            match weak.upgrade() {
                Some(m) => m.command_line(&args),
                None => glib::ExitCode::from(1),
            }
        });
        let weak = Rc::downgrade(&manager);
        app.connect_shutdown(move |_| if let Some(m) = weak.upgrade() { m.shutdown() });
        manager
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> { Rc::downgrade(self) }

    // ---- accessors (tests, Global Settings style embedding) ----
    pub fn panel(&self) -> Option<gtk::ApplicationWindow> { self.panel.borrow().clone() }
    pub fn launcher(&self) -> Option<gtk::ApplicationWindow> { self.launcher.borrow().clone() }
    pub fn keyboard(&self) -> Option<Keyboard> { self.keyboard.borrow().clone() }
    pub fn bridge(&self) -> Option<InputBridge> { self.bridge.borrow().clone() }
    pub fn bridge_ready(&self) -> bool { self.bridge_ready.get() }
    pub fn bridge_failed(&self) -> bool { self.bridge_failed.get() }
    pub fn policy(&self) -> Visibility { self.policy.borrow().clone() }
    pub fn error(&self) -> String { self.error.borrow().clone() }
    pub fn config(&self) -> Settings { self.config.borrow().clone() }
    pub fn monitor(&self) -> Option<gdk::Monitor> { self.monitor.borrow().clone() }
    pub fn suspended(&self) -> bool { self.suspended.get() }
    pub fn settings_window(&self) -> Option<gtk::ApplicationWindow> { self.settings_window.borrow().as_ref().map(|(w, _)| w.clone()) }
    pub fn settings_controls(&self) -> Option<KeyboardSettings> { self.settings_window.borrow().as_ref().map(|(_, c)| c.clone()) }

    fn startup(self: &Rc<Self>) {
        self.started.set(true);
        *self.hold.borrow_mut() = Some(self.app.hold());
        match self.options.config.load() {
            Ok(config) => *self.config.borrow_mut() = config,
            Err(error) => *self.error.borrow_mut() = error,
        }
        if let Some(directory) = self.options.config.path.parent() {
            let _ = std::fs::create_dir_all(directory);
            if let Ok(watch) = gio::File::for_path(directory).monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
                let weak = self.weak();
                watch.connect_changed(move |_, _, _, _| if let Some(m) = weak.upgrade() { m.preferences_changed() });
                *self.file_watch.borrow_mut() = Some(watch);
            }
        }
        // Follow the physical keyboard layouts chosen in Global Settings.
        if let Ok(watch) = gio::File::for_path(environment_path()).monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            let weak = self.weak();
            watch.connect_changed(move |_, _, _, _| if let Some(m) = weak.upgrade() { m.preferences_changed() });
            *self.environment_watch.borrow_mut() = Some(watch);
        }
        let weak = self.weak();
        *self.lock_watch.borrow_mut() = Some(LockMonitor::new(move |locked| if let Some(m) = weak.upgrade() { m.session_locked(locked) }));
        if self.options.watch_devices {
            let weak = self.weak();
            *self.device_watch.borrow_mut() = Some(DeviceMonitor::new(move |d| if let Some(m) = weak.upgrade() { m.hardware_changed(d) }));
        }
        self.hardware_changed((self.options.device_loader)());
        if let Some(display) = gdk::Display::default() {
            let monitors = display.monitors();
            let weak = self.weak();
            let handler = monitors.connect_items_changed(move |_, _, _, _| if let Some(m) = weak.upgrade() { m.monitor_changed() });
            *self.monitors.borrow_mut() = Some((monitors, handler));
        }
        self.monitor_changed();
    }

    fn command_line(self: &Rc<Self>, args: &[String]) -> glib::ExitCode {
        match parse_arguments(args) {
            None => return glib::ExitCode::from(2),
            Some(Action::Quit) => self.app.quit(),
            Some(Action::Settings) => self.show_settings(),
            Some(Action::Background) => {}
            Some(Action::Hide) => self.hide_keyboard(),
            Some(Action::Show) => self.show_keyboard(),
            Some(Action::Toggle) => self.toggle(),
        }
        glib::ExitCode::SUCCESS
    }

    pub fn session_locked(self: &Rc<Self>, locked: bool) {
        if locked == self.suspended.get() {
            return;
        }
        self.suspended.set(locked);
        {
            let mut policy = self.policy.borrow_mut();
            policy.hide();
            policy.active = false;
            policy.epoch = -1;
        }
        self.reconcile();
    }

    pub fn hardware_changed(self: &Rc<Self>, devices: Vec<Device>) {
        let eligible = automatic_available(&devices, &self.config.borrow());
        *self.devices.borrow_mut() = devices;
        self.policy.borrow_mut().hardware(eligible);
        self.reconcile();
    }

    fn preferences_changed(self: &Rc<Self>) {
        if self.reload_id.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let id = glib::timeout_add_local_once(Duration::from_millis(120), move || if let Some(m) = weak.upgrade() {
            m.reload_id.borrow_mut().take();
            m.reload_config();
        });
        *self.reload_id.borrow_mut() = Some(id);
    }

    fn reload_config(self: &Rc<Self>) {
        let result = (|| -> Result<(), String> {
            let config = self.options.config.load()?;
            *self.config.borrow_mut() = config.clone();
            let layout = layout_name(&config, None);
            let bridge = self.bridge();
            if let Some(bridge) = bridge {
                if self.sent_layout.borrow().as_deref() != Some(layout.as_str()) {
                    bridge.send(&Command::Layout(layout.clone()))?;
                    *self.sent_layout.borrow_mut() = Some(layout);
                }
            }
            if let Some(keyboard) = self.keyboard() {
                keyboard.set_size(&config.size);
            }
            let devices = self.devices.borrow().clone();
            self.hardware_changed(devices);
            Ok(())
        })();
        if let Err(error) = result {
            self.failed(error);
        }
    }

    pub fn show_keyboard(self: &Rc<Self>) {
        if self.bridge_failed.get() {
            if let Some(bridge) = self.bridge.borrow_mut().take() {
                bridge.close();
            }
            self.bridge_failed.set(false);
            let mut policy = self.policy.borrow_mut();
            policy.active = false;
            policy.epoch = -1;
        }
        self.error.borrow_mut().clear();
        self.policy.borrow_mut().show();
        self.reconcile();
    }

    pub fn hide_keyboard(self: &Rc<Self>) {
        self.policy.borrow_mut().hide();
        if let Some(keyboard) = self.keyboard() {
            keyboard.reset();
        }
        self.reconcile();
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.policy.borrow().visible() { self.hide_keyboard() } else { self.show_keyboard() }
    }

    pub fn input_event(self: &Rc<Self>, event: Map<String, Value>) {
        match event.get("event").and_then(Value::as_str) {
            Some("ready") => {
                self.bridge_ready.set(true);
                if !truthy(event.get("input_method")) {
                    *self.error.borrow_mut() = tr!("Automatic text-field detection is unavailable. Use the keyboard button to open it.");
                }
            }
            Some("layout") => {
                let keys = event.get("keys").cloned().unwrap_or_else(|| json!({}));
                *self.keys.borrow_mut() = keys.clone();
                if let Some(keyboard) = self.keyboard() {
                    keyboard.set_labels(&keys);
                }
            }
            Some("context") => {
                let epoch = event.get("epoch").and_then(Value::as_i64);
                let purpose = event.get("purpose").and_then(Value::as_u64).unwrap_or(0);
                let changed = epoch != Some(self.policy.borrow().epoch) || purpose != self.purpose.get();
                self.policy.borrow_mut().context(truthy(event.get("active")), epoch.unwrap_or(0));
                self.purpose.set(purpose);
                if let Some(keyboard) = self.keyboard().filter(|_| changed) {
                    keyboard.reset();
                    keyboard.set_purpose(purpose);
                }
            }
            Some("ime-unavailable") => {
                *self.error.borrow_mut() = tr!("Another input method is active. The keyboard still works manually.");
            }
            Some("error") => {
                let message = match event.get("message") {
                    None => tr!("Keyboard input failed."),
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                };
                self.failed(message);
                return;
            }
            _ => {}
        }
        self.reconcile();
    }

    pub fn failed(self: &Rc<Self>, error: String) {
        *self.error.borrow_mut() = error.clone();
        self.bridge_ready.set(false);
        self.bridge_failed.set(true);
        if let Some(keyboard) = self.keyboard() {
            keyboard.message(&error);
            keyboard.body().set_sensitive(false);
        }
        self.publish();
    }

    pub fn reconcile(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        if !gtk4_layer_shell::is_supported() {
            *self.error.borrow_mut() = tr!("On-screen keyboard requires a compatible Wayland session.");
            self.publish();
            return;
        }
        let needed = self.policy.borrow().needed() && !self.suspended.get();
        let has_bridge = self.bridge.borrow().is_some();
        if needed && !has_bridge {
            self.error.borrow_mut().clear();
            self.bridge_failed.set(false);
            let layout = layout_name(&self.config.borrow(), None);
            *self.sent_layout.borrow_mut() = Some(layout.clone());
            let (changed, failed) = (self.weak(), self.weak());
            match InputBridge::new(&layout, move |event| if let Some(m) = changed.upgrade() { m.input_event(event) },
                                   move |error| if let Some(m) = failed.upgrade() { m.failed(error) }, self.options.helper.clone()) {
                Ok(bridge) => *self.bridge.borrow_mut() = Some(bridge),
                Err(error) => self.failed(error),
            }
        } else if !needed && has_bridge {
            if let Some(bridge) = self.bridge.borrow_mut().take() {
                bridge.close();
            }
            self.bridge_ready.set(false);
            self.policy.borrow_mut().active = false;
        }
        if needed {
            self.ensure_windows();
        }
        let (panel, launcher, keyboard) = (self.panel(), self.launcher(), self.keyboard());
        if let (Some(panel), Some(launcher), Some(keyboard)) = (panel, launcher, keyboard) {
            let show = self.policy.borrow().visible() && !self.suspended.get();
            if show {
                keyboard.body().set_sensitive(self.bridge_ready.get());
                keyboard.message(&self.error.borrow());
                if !panel.get_visible() && self.pointer_probe.borrow().is_none() {
                    let weak = self.weak();
                    let probe = PointerMonitor::new(move |monitor| if let Some(m) = weak.upgrade() { m.pointer_ready(monitor) });
                    *self.pointer_probe.borrow_mut() = Some(probe);
                }
            } else {
                if let Some(probe) = self.pointer_probe.borrow_mut().take() {
                    probe.cancel();
                }
                panel.set_visible(false);
                panel.set_exclusive_zone(0);
            }
            launcher.set_visible(self.policy.borrow().eligible && !show && !self.suspended.get());
        }
        self.publish();
    }

    fn ensure_windows(self: &Rc<Self>) {
        if self.panel.borrow().is_some() {
            return;
        }
        let panel = gtk::ApplicationWindow::builder().application(&self.app).decorated(false).title(tr!("On-screen Keyboard"))
            .resizable(false).build();
        panel.add_css_class("waykeys-window");
        panel.init_layer_shell();
        panel.set_namespace(Some("waykeys-keyboard"));
        panel.set_layer(Layer::Overlay);
        panel.set_keyboard_mode(KeyboardMode::None);
        panel.set_anchor(Edge::Bottom, true);
        panel.set_margin(Edge::Bottom, 8);
        panel.auto_exclusive_zone_enable();
        let (send, hide, settings) = (self.weak(), self.weak(), self.weak());
        let keyboard = Keyboard::new(move |kind, value, flags| if let Some(m) = send.upgrade() { m.send(kind, value, flags) },
                                     move || if let Some(m) = hide.upgrade() { m.hide_keyboard() },
                                     Some(Box::new(move || if let Some(m) = settings.upgrade() { m.show_settings() })), false);
        keyboard.set_labels(&self.keys.borrow());
        keyboard.set_size(&self.config.borrow().size);
        keyboard.set_purpose(self.purpose.get());
        panel.set_child(Some(keyboard.root()));
        attach(&panel, None);
        let weak = self.weak();
        panel.connect_close_request(move |_| {
            if let Some(m) = weak.upgrade() {
                m.hide_keyboard();
            }
            glib::Propagation::Stop
        });
        let launcher = gtk::ApplicationWindow::builder().application(&self.app).decorated(false).resizable(false)
            .title(tr!("Show keyboard")).build();
        launcher.add_css_class("waykeys-window");
        launcher.init_layer_shell();
        launcher.set_namespace(Some("waykeys-launcher"));
        launcher.set_layer(Layer::Top);
        launcher.set_keyboard_mode(KeyboardMode::None);
        launcher.set_anchor(Edge::Bottom, true);
        launcher.set_anchor(Edge::Right, true);
        launcher.set_margin(Edge::Right, 12);
        launcher.set_margin(Edge::Bottom, 12);
        launcher.set_exclusive_zone(0);
        let button = gtk::Button::builder().icon_name("input-keyboard-symbolic").focusable(false).tooltip_text(tr!("Show keyboard")).build();
        button.set_size_request(52, 48);
        let weak = self.weak();
        button.connect_clicked(move |_| if let Some(m) = weak.upgrade() { m.show_keyboard() });
        launcher.set_child(Some(&button));
        attach(&launcher, None);
        *self.panel.borrow_mut() = Some(panel);
        *self.launcher.borrow_mut() = Some(launcher);
        *self.keyboard.borrow_mut() = Some(keyboard);
        self.position();
    }

    fn send(&self, kind: &str, value: u32, flags: u32) {
        if !self.bridge_ready.get() {
            return;
        }
        let bridge = self.bridge();
        if let (Some(bridge), Ok(command)) = (bridge, Command::from_kind(kind, value as i64, flags as i64)) {
            if let Err(error) = bridge.send(&command) {
                eprintln!("waykeys: {error}");
            }
        }
    }

    /// Keyboard focus must never move a keyboard opened on another screen: the
    /// panel goes to the output under the pointer when it is shown.
    fn pointer_ready(self: &Rc<Self>, monitor: Option<gdk::Monitor>) {
        self.pointer_probe.borrow_mut().take();
        if self.closed.get() || self.suspended.get() || !self.policy.borrow().visible() {
            return;
        }
        *self.monitor.borrow_mut() = monitor;
        self.position();
        if let Some(panel) = self.panel() {
            panel.set_visible(true);
        }
        self.publish();
    }

    fn monitor_changed(self: &Rc<Self>) {
        let Some(display) = gdk::Display::default() else { return };
        let monitors = display.monitors();
        for (monitor, handler) in self.geometry_watch.borrow_mut().drain(..) {
            monitor.disconnect(handler);
        }
        let items: Vec<gdk::Monitor> = (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast()).collect();
        for monitor in &items {
            let weak = self.weak();
            let handler = monitor.connect_geometry_notify(move |_| if let Some(m) = weak.upgrade() { m.position() });
            self.geometry_watch.borrow_mut().push((monitor.clone(), handler));
        }
        let current = self.monitor.borrow().clone();
        if current.is_some_and(|c| !items.contains(&c)) {
            *self.monitor.borrow_mut() = None;
        }
        self.position();
    }

    fn position(self: &Rc<Self>) {
        let (Some(panel), Some(launcher), Some(keyboard)) = (self.panel(), self.launcher(), self.keyboard()) else { return };
        let Some(display) = gdk::Display::default() else { return };
        let monitors = display.monitors();
        if monitors.n_items() == 0 {
            return;
        }
        let monitor = self.monitor().or_else(|| monitors.item(0).and_downcast()).unwrap();
        if panel.monitor().as_ref() != Some(&monitor) {
            let shown = panel.get_visible();
            panel.set_visible(false);
            panel.set_monitor(Some(&monitor));
            launcher.set_monitor(Some(&monitor));
            panel.set_visible(shown);
        }
        let (width, narrow) = panel_width(monitor.geometry().width());
        panel.set_default_size(width, -1);
        keyboard.root().set_size_request(width, -1);
        if narrow { keyboard.root().add_css_class("keyboard-narrow") } else { keyboard.root().remove_css_class("keyboard-narrow") }
        if self.policy.borrow().visible() {
            panel.auto_exclusive_zone_enable();
        }
    }

    /// `$XDG_RUNTIME_DIR/waykeys/state.json`, written only when it changes.
    fn publish(&self) {
        let value = json!({"pid": std::process::id(), "visible": self.panel().is_some_and(|p| p.get_visible()),
                           "automatic_available": self.policy.borrow().eligible, "ready": self.bridge_ready.get(),
                           "error": self.error.borrow().clone()});
        if self.status.borrow().as_ref() == Some(&value) {
            return;
        }
        *self.status.borrow_mut() = Some(value.clone());
        let path = state_path();
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
            }
            let temporary = path.with_extension("tmp");
            std::fs::write(&temporary, json_dumps(&value, &JsonStyle::DEFAULT) + "\n")?;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&temporary, &path)
        })();
        if let Err(error) = result {
            eprintln!("waykeys: cannot write {}: {error}", path.display());
        }
    }

    pub fn show_settings(self: &Rc<Self>) {
        if let Some(window) = self.settings_window() {
            window.present();
            return;
        }
        let window = gtk::ApplicationWindow::builder().application(&self.app).title(tr!("On-screen Keyboard Settings"))
            .default_width(640).default_height(580).build();
        let header = headerbar(&tr!("On-screen Keyboard"));
        window.set_titlebar(Some(&header));
        let weak = self.weak();
        // The keyboard service keeps running: Quit closes this window only.
        let quit: Rc<dyn Fn()> = Rc::new(move || if let Some(window) = weak.upgrade().and_then(|m| m.settings_window()) { window.close() });
        install_app_menu(&self.app, &window, MenuTarget::New, AboutDetails {
            name: "On-screen Keyboard".into(), icon: "input-keyboard".into(),
            comments: Some("A touch keyboard for tablets and touchscreens".into()), ..Default::default() },
            Vec::new(), Some(&header), Some(quit));
        let controls = KeyboardSettings::with_devices(Some(self.options.config.clone()), self.options.watch_devices, None);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(controls.root()));
        window.set_child(Some(&scroll));
        attach(&window, None);
        *self.settings_window.borrow_mut() = Some((window.clone(), controls));
        let weak = self.weak();
        window.connect_close_request(move |_| {
            if let Some(m) = weak.upgrade() {
                if let Some((_, controls)) = m.settings_window.borrow_mut().take() {
                    controls.close();
                }
            }
            glib::Propagation::Proceed
        });
        window.present();
    }

    /// `do_shutdown`: stop helpers and watchers, remove our state file.
    pub fn shutdown(&self) {
        if self.closed.replace(true) || !self.started.get() {
            return;
        }
        if let Some(probe) = self.pointer_probe.borrow_mut().take() {
            probe.cancel();
        }
        if let Some(bridge) = self.bridge.borrow().clone() {
            bridge.close();
        }
        if let Some(keyboard) = self.keyboard() {
            keyboard.close();
        }
        if let Some(watch) = self.device_watch.borrow_mut().take() {
            watch.close();
        }
        if let Some(watch) = self.file_watch.borrow_mut().take() {
            watch.cancel();
        }
        if let Some(watch) = self.lock_watch.borrow_mut().take() {
            watch.close();
        }
        if let Some(watch) = self.environment_watch.borrow_mut().take() {
            watch.cancel();
        }
        if let Some(id) = self.reload_id.borrow_mut().take() {
            id.remove();
        }
        if let Some((monitors, handler)) = self.monitors.borrow_mut().take() {
            monitors.disconnect(handler);
        }
        for (monitor, handler) in self.geometry_watch.borrow_mut().drain(..) {
            monitor.disconnect(handler);
        }
        let path = state_path();
        let ours = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("pid").and_then(Value::as_u64)) == Some(std::process::id() as u64);
        if ours {
            let _ = std::fs::remove_file(&path);
        }
        self.hold.borrow_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_and_precedence() {
        assert_eq!(parse_arguments::<&str>(&[]), Some(Action::Toggle));
        assert_eq!(parse_arguments(&["--toggle"]), Some(Action::Toggle));
        assert_eq!(parse_arguments(&["--show", "--hide"]), Some(Action::Hide));
        assert_eq!(parse_arguments(&["--show", "--background"]), Some(Action::Background));
        assert_eq!(parse_arguments(&["--background", "--settings"]), Some(Action::Settings));
        assert_eq!(parse_arguments(&["--settings", "--quit"]), Some(Action::Quit));
        assert_eq!(parse_arguments(&["--show"]), Some(Action::Show));
        assert_eq!(parse_arguments(&["--show", "--bogus"]), None);
        assert_eq!(parse_arguments(&["-h"]), None);
    }

    #[test]
    fn widths() {
        assert_eq!(panel_width(1920), (1100, false));
        assert_eq!(panel_width(1024), (1000, false));
        assert_eq!(panel_width(700), (676, true));
        assert_eq!(panel_width(200), (300, true));
    }
}
