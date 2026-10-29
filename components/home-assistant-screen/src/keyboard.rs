//! `screen_keyboard.py`: the shared Waykeys widget, embedded below Kasumi's
//! local web controls.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use serde_json::{json, Map, Value};
use waykeys::backend::{Command, InputBridge};
use waykeys::config::{layout_name, Config, Settings};
use waykeys::devices::{automatic_available, discover, Device, DeviceMonitor};
use waykeys::keyboard::Keyboard;
use waykeys::state::Visibility;

pub type BridgeFactory = Rc<dyn Fn(&str, Box<dyn Fn(Map<String, Value>)>, Box<dyn Fn(String)>) -> Result<InputBridge, String>>;

struct Inner {
    panel: gtk::Window,
    bridge_factory: BridgeFactory,
    bridge: RefCell<Option<InputBridge>>,
    closed: Cell<bool>,
    ready: Cell<bool>,
    failed: Cell<bool>,
    error: RefCell<String>,
    purpose: Cell<Option<u64>>,
    policy: RefCell<Visibility>,
    config: Settings,
    widget: Keyboard,
    button: gtk::Button,
    monitor: RefCell<Option<DeviceMonitor>>,
}

/// `PanelKeyboard(panel, container, device_loader=discover, bridge_factory=InputBridge)`.
#[derive(Clone)]
pub struct PanelKeyboard(Rc<Inner>);

fn up(weak: &Weak<Inner>) -> Option<PanelKeyboard> { weak.upgrade().map(PanelKeyboard) }

pub fn default_bridge() -> BridgeFactory {
    Rc::new(|layout, changed, failed| InputBridge::new(layout, move |event| changed(event), move |message| failed(message), None))
}

impl PanelKeyboard {
    pub fn new(panel: &gtk::Window, container: &gtk::Box, devices: Option<Vec<Device>>, bridge_factory: Option<BridgeFactory>,
               watch_devices: bool) -> Self {
        let config = Config::default().load().unwrap_or_default();
        let cell: Rc<RefCell<Weak<Inner>>> = Rc::new(RefCell::new(Weak::new()));
        let (send_cell, hide_cell) = (cell.clone(), cell.clone());
        let widget = Keyboard::new(move |kind, value, flags| if let Some(k) = up(&send_cell.borrow()) { k.send(kind, value, flags) },
                                   move || if let Some(k) = up(&hide_cell.borrow()) { k.hide() }, None, false);
        widget.set_size(&config.size);
        widget.root().set_visible(false);
        // The web viewport shrinks with the keyboard, keeping focused fields
        // reachable even though the admin window itself is fullscreen.
        container.append(widget.root());
        let button = gtk::Button::builder().icon_name("input-keyboard-symbolic").focusable(false)
            .tooltip_text("Show or hide the on-screen keyboard").build();
        button.add_css_class("panel-keyboard-toggle");
        let keyboard = Self(Rc::new(Inner {
            panel: panel.clone(), bridge_factory: bridge_factory.unwrap_or_else(default_bridge), bridge: RefCell::new(None),
            closed: Cell::new(false), ready: Cell::new(false), failed: Cell::new(false), error: RefCell::new(String::new()),
            purpose: Cell::new(None), policy: RefCell::new(Visibility::default()), config, widget, button: button.clone(),
            monitor: RefCell::new(None),
        }));
        cell.replace(Rc::downgrade(&keyboard.0));
        let weak = Rc::downgrade(&keyboard.0);
        button.connect_clicked(move |_| if let Some(k) = up(&weak) { k.toggle() });
        let weak = Rc::downgrade(&keyboard.0);
        panel.connect_visible_notify(move |_| if let Some(k) = up(&weak) { k.visibility_changed() });
        if watch_devices {
            let weak = Rc::downgrade(&keyboard.0);
            keyboard.0.monitor.replace(Some(DeviceMonitor::new(move |d| if let Some(k) = up(&weak) { k.devices_changed(&d) })));
        }
        keyboard.devices_changed(&devices.unwrap_or_else(discover));
        keyboard
    }

    pub fn button(&self) -> &gtk::Button { &self.0.button }
    pub fn widget(&self) -> &Keyboard { &self.0.widget }
    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn bridge(&self) -> Option<InputBridge> { self.0.bridge.borrow().clone() }
    pub fn policy(&self) -> Visibility { self.0.policy.borrow().clone() }

    fn resize(&self) {
        let width = self.0.panel.width();
        let root = self.0.widget.root();
        if 0 < width && width < 724 { root.add_css_class("keyboard-narrow") } else { root.remove_css_class("keyboard-narrow") }
    }

    pub fn devices_changed(&self, devices: &[Device]) {
        self.0.policy.borrow_mut().hardware(automatic_available(devices, &self.0.config));
        self.reconcile();
    }

    fn visibility_changed(&self) {
        {
            let mut policy = self.0.policy.borrow_mut();
            if !self.0.panel.get_visible() {
                policy.hide();
                policy.active = false;
                policy.epoch = -1;
            } else {
                policy.dismissed = false;
            }
        }
        self.reconcile();
    }

    pub fn toggle(&self) {
        if self.0.widget.root().get_visible() {
            self.hide();
        } else {
            if self.0.failed.get() {
                self.stop_bridge();
            }
            self.0.failed.set(false);
            self.0.error.replace(String::new());
            self.0.policy.borrow_mut().show();
            self.reconcile();
        }
    }

    pub fn hide(&self) {
        self.0.policy.borrow_mut().hide();
        self.0.widget.reset();
        self.reconcile();
    }

    fn event(&self, event: Map<String, Value>) {
        let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
        match kind {
            "ready" => {
                self.0.ready.set(true);
                // On a touch-only seat this may be its first keyboard capability.
                // Restore the control panel after the compositor assigns key focus.
                if self.0.panel.get_visible() {
                    self.0.panel.present();
                }
                if !event.get("input_method").is_some_and(suite_core::pyjson::truthy) {
                    self.0.error.replace("Use the keyboard button to show the keyboard manually.".into());
                }
            }
            "layout" => self.0.widget.set_labels(event.get("keys").unwrap_or(&json!({}))),
            "context" => {
                let epoch = event.get("epoch").and_then(Value::as_i64);
                let purpose = event.get("purpose").and_then(Value::as_u64).unwrap_or(0);
                let changed = epoch != Some(self.0.policy.borrow().epoch) || Some(purpose) != self.0.purpose.get();
                if changed {
                    self.0.widget.reset();
                }
                let active = event.get("active").is_some_and(suite_core::pyjson::truthy);
                self.0.policy.borrow_mut().context(active, epoch.unwrap_or(0));
                self.0.purpose.set(Some(purpose));
                if changed {
                    self.0.widget.set_purpose(purpose);
                }
            }
            "ime-unavailable" => {
                self.0.error.replace("Another input method is active. Manual keyboard input is still available.".into());
            }
            "error" => {
                let message = event.get("message").map(suite_core::pyjson::py_str).unwrap_or_else(|| "Keyboard input failed.".into());
                self.failure(message);
                return;
            }
            _ => {}
        }
        self.reconcile();
    }

    fn failure(&self, message: String) {
        self.0.error.replace(message.clone());
        self.0.ready.set(false);
        self.0.failed.set(true);
        self.0.widget.body().set_sensitive(false);
        self.0.widget.message(&message);
    }

    fn reconcile(&self) {
        let inner = &self.0;
        if inner.closed.get() {
            return;
        }
        let needed = inner.panel.get_visible() && inner.policy.borrow().needed();
        if needed && inner.bridge.borrow().is_none() {
            let weak = Rc::downgrade(&self.0);
            let event = Box::new(move |e| if let Some(k) = up(&weak) { k.event(e) });
            let weak = Rc::downgrade(&self.0);
            let failed = Box::new(move |m| if let Some(k) = up(&weak) { k.failure(m) });
            let layout = layout_name(&inner.config, None);
            match (inner.bridge_factory)(&layout, event, failed) {
                Ok(bridge) => {
                    inner.bridge.replace(Some(bridge));
                }
                Err(error) => self.failure(error),
            }
        } else if !needed {
            self.stop_bridge();
        }
        let visible = inner.panel.get_visible() && inner.policy.borrow().visible();
        inner.widget.root().set_visible(visible);
        inner.widget.body().set_sensitive(inner.ready.get());
        inner.widget.message(&inner.error.borrow());
        if visible { inner.button.add_css_class("suggested-action") } else { inner.button.remove_css_class("suggested-action") }
        self.resize();
    }

    fn send(&self, kind: &str, value: u32, flags: u32) {
        if !self.0.ready.get() {
            return;
        }
        if let Some(bridge) = self.0.bridge.borrow().clone() {
            if let Ok(command) = Command::from_kind(kind, value as i64, flags as i64) {
                let _ = bridge.send(&command);
            }
        }
    }

    fn stop_bridge(&self) {
        if let Some(bridge) = self.0.bridge.borrow_mut().take() {
            bridge.close();
        }
        self.0.ready.set(false);
        let mut policy = self.0.policy.borrow_mut();
        policy.active = false;
        policy.epoch = -1;
    }

    pub fn status(&self) -> Value {
        json!({"visible": self.0.widget.root().get_visible(), "automatic_available": self.0.policy.borrow().eligible,
               "ready": self.0.ready.get(), "error": *self.0.error.borrow()})
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.stop_bridge();
        if let Some(monitor) = self.0.monitor.borrow_mut().take() {
            monitor.close();
        }
        self.0.widget.close();
    }
}
