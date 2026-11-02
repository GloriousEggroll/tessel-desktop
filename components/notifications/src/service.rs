//! Freedesktop notification protocol and the shell notification-center API
//! (port of notification_service.py), on GDBus so calls run on the GTK thread.

use crate::model::{hints_from_variant, Event, NotifyArgs, SharedModel};
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

pub const BUS_NAME: &str = "org.freedesktop.Notifications";
pub const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
pub const INTERFACE: &str = BUS_NAME;
pub const CONTROL_INTERFACE: &str = "org.tessel.Notifications1";
pub const CAPABILITIES: [&str; 4] = ["actions", "body", "icon-static", "persistence"];
pub const ALREADY_RUNNING: &str = "Another notification server is already running";

pub const XML: &str = r#"<node>
  <interface name="org.freedesktop.Notifications">
    <method name="GetCapabilities"><arg direction="out" type="as"/></method>
    <method name="GetServerInformation">
      <arg direction="out" type="s"/><arg direction="out" type="s"/>
      <arg direction="out" type="s"/><arg direction="out" type="s"/>
    </method>
    <method name="Notify">
      <arg direction="in" type="s"/><arg direction="in" type="u"/>
      <arg direction="in" type="s"/><arg direction="in" type="s"/>
      <arg direction="in" type="s"/><arg direction="in" type="as"/>
      <arg direction="in" type="a{sv}"/><arg direction="in" type="i"/>
      <arg direction="out" type="u"/>
    </method>
    <method name="CloseNotification"><arg direction="in" type="u"/></method>
    <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
    <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
  </interface>
  <interface name="org.tessel.Notifications1">
    <method name="GetState"><arg direction="out" type="s"/></method>
    <method name="SetSettings"><arg direction="in" type="s"/></method>
    <method name="ClearHistory"/><method name="MarkRead"/>
    <method name="Dismiss"><arg direction="in" type="u"/></method>
    <method name="ClearApp"><arg direction="in" type="s"/></method>
    <method name="Activate"><arg direction="in" type="u"/><arg direction="in" type="s"/></method>
    <method name="ShowCenter"/>
    <method name="ShowCenterAt"><arg direction="in" type="s"/><arg direction="in" type="u"/></method>
    <method name="ShowSettings"/>
    <signal name="StateChanged"><arg type="s"/></signal>
  </interface>
</node>"#;

/// `ShowCenter`, `ShowCenterAt(edge, offset)` and `ShowSettings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    ShowCenter,
    ShowCenterAt(String, u32),
    ShowSettings,
}

/// Errors are `ValueError`-like (InvalidArgs) unless `Failed`.
#[derive(Clone, Debug)]
pub enum MethodError {
    /// `org.freedesktop.Notifications.InvalidId` "Notification does not exist".
    InvalidId,
    InvalidArgs(String),
    Failed(String),
}

pub type CommandHandler = Rc<dyn Fn(Command) -> Result<(), String>>;

struct Inner {
    connection: gio::DBusConnection,
    model: SharedModel,
    on_command: CommandHandler,
    registrations: RefCell<Vec<gio::RegistrationId>>,
    owned: Cell<bool>,
    subscription: Cell<Option<u64>>,
    senders: RefCell<HashMap<u32, String>>,
}

#[derive(Clone)]
pub struct NotificationService(Rc<Inner>);

/// Compact `json.dumps(state, ensure_ascii=False, separators=(",", ":"))`.
pub fn state_json(model: &SharedModel) -> String {
    tessel_core::pyfmt::json_dumps(&model.state(), &tessel_core::pyfmt::JsonStyle { ensure_ascii: false,
        ..tessel_core::pyfmt::JsonStyle::COMPACT })
}

impl NotificationService {
    pub fn new(connection: &gio::DBusConnection, model: &SharedModel, on_command: Option<CommandHandler>) -> Self {
        Self(Rc::new(Inner {
            connection: connection.clone(), model: model.clone(),
            on_command: on_command.unwrap_or_else(|| Rc::new(|_| Ok(()))),
            registrations: RefCell::new(Vec::new()), owned: Cell::new(false), subscription: Cell::new(None),
            senders: RefCell::new(HashMap::new()),
        }))
    }

    pub fn owned(&self) -> bool { self.0.owned.get() }

    /// Own the name (DO_NOT_QUEUE, never replacing) and export both interfaces.
    pub fn start(&self) -> Result<(), String> {
        if self.0.owned.get() {
            return Ok(());
        }
        // Do not displace an existing desktop's server or join its wait queue.
        let reply = self.0.connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "RequestName", Some(&(BUS_NAME, 4u32).to_variant()),
            Some(glib::VariantTy::new("(u)").unwrap()), gio::DBusCallFlags::NONE, 3000, None::<&gio::Cancellable>)
            .map_err(|e| e.message().to_owned())?;
        if reply.child_value(0).get::<u32>() != Some(1) {
            return Err(ALREADY_RUNNING.into());
        }
        self.0.owned.set(true);
        if let Err(error) = self.register() {
            self.close();
            return Err(error);
        }
        Ok(())
    }

    fn register(&self) -> Result<(), String> {
        let info = gio::DBusNodeInfo::for_xml(XML).map_err(|e| e.message().to_owned())?;
        for interface in info.interfaces() {
            let weak = Rc::downgrade(&self.0);
            let id = self.0.connection.register_object(OBJECT_PATH, interface)
                .method_call(move |_, sender, _, interface, method, params, invocation| {
                    let Some(inner) = weak.upgrade() else { return };
                    let service = NotificationService(inner);
                    match service.method(sender.unwrap_or(""), interface.unwrap_or(""), method, &params) {
                        Ok(result) => invocation.return_value(result.as_ref()),
                        Err(MethodError::InvalidId) =>
                            invocation.return_dbus_error("org.freedesktop.Notifications.InvalidId", INVALID_ID),
                        Err(MethodError::InvalidArgs(message)) =>
                            invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", &message),
                        Err(MethodError::Failed(message)) =>
                            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &message),
                    }
                })
                .build().map_err(|e| e.message().to_owned())?;
            self.0.registrations.borrow_mut().push(id);
        }
        let weak: Weak<Inner> = Rc::downgrade(&self.0);
        let id = self.0.model.subscribe(move |event| {
            if let Some(inner) = weak.upgrade() {
                NotificationService(inner).event(event);
            }
        });
        self.0.subscription.set(Some(id));
        Ok(())
    }

    pub fn close(&self) {
        if let Some(id) = self.0.subscription.take() {
            self.0.model.unsubscribe(id);
        }
        for id in self.0.registrations.take() {
            let _ = self.0.connection.unregister_object(id);
        }
        if self.0.owned.get() && !self.0.connection.is_closed() {
            let _ = self.0.connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus",
                "org.freedesktop.DBus", "ReleaseName", Some(&(BUS_NAME,).to_variant()),
                Some(glib::VariantTy::new("(u)").unwrap()), gio::DBusCallFlags::NONE, 3000, None::<&gio::Cancellable>);
        }
        self.0.owned.set(false);
        self.0.senders.borrow_mut().clear();
    }

    fn emit(&self, destination: Option<&str>, interface: &str, name: &str, value: Variant) {
        let _ = self.0.connection.emit_signal(destination.filter(|d| !d.is_empty()), OBJECT_PATH, interface, name, Some(&value));
    }

    fn event(&self, event: &Event) {
        match event {
            Event::Changed(kind, ident) => {
                if *kind == "notify" {
                    if let Some(sender) = self.0.model.with(|m| m.items.get(ident).map(|i| i.sender.clone())) {
                        self.0.senders.borrow_mut().insert(*ident, sender);
                    }
                }
                if self.0.owned.get() {
                    self.emit(None, CONTROL_INTERFACE, "StateChanged", (state_json(&self.0.model),).to_variant());
                }
            }
            Event::Closed(ident, reason) => {
                let sender = self.0.senders.borrow_mut().remove(ident);
                if self.0.owned.get() {
                    self.emit(sender.as_deref(), INTERFACE, "NotificationClosed", (*ident, *reason).to_variant());
                }
            }
            Event::Action(ident, key) => {
                if self.0.owned.get() {
                    let sender = self.0.senders.borrow().get(ident).cloned();
                    self.emit(sender.as_deref(), INTERFACE, "ActionInvoked", (*ident, key.as_str()).to_variant());
                }
            }
        }
    }

    /// Dispatch one call; `Ok(None)` replies with no values.
    pub fn method(&self, sender: &str, interface: &str, method: &str, params: &Variant) -> Result<Option<Variant>, MethodError> {
        let model = &self.0.model;
        let string = |index: usize| params.try_child_value(index).and_then(|v| v.get::<String>()).unwrap_or_default();
        let uint = |index: usize| params.try_child_value(index).and_then(|v| v.get::<u32>()).unwrap_or(0);
        let invalid = |message: &str| Err(MethodError::InvalidArgs(message.into()));
        if interface == INTERFACE {
            return match method {
                "GetCapabilities" => Ok(Some((CAPABILITIES.to_vec(),).to_variant())),
                "GetServerInformation" => Ok(Some(("Tessel Notifications", "Tessel", "1.0", "1.2").to_variant())),
                "Notify" => {
                    let args = NotifyArgs {
                        app_name: string(0), replaces_id: uint(1), app_icon: string(2), summary: string(3), body: string(4),
                        actions: params.try_child_value(5).and_then(|v| v.get::<Vec<String>>()).unwrap_or_default(),
                        hints: params.try_child_value(6).map(|v| hints_from_variant(&v)).unwrap_or_default(),
                        expire_timeout: params.try_child_value(7).and_then(|v| v.get::<i32>()).unwrap_or(-1),
                    };
                    Ok(Some((model.notify(args, sender),).to_variant()))
                }
                "CloseNotification" => {
                    if !model.close(uint(0), 3) {
                        return Err(MethodError::InvalidId);
                    }
                    Ok(None)
                }
                _ => invalid("Unknown notification method"),
            };
        }
        if interface != CONTROL_INTERFACE {
            return invalid("Unknown interface");
        }
        match method {
            "GetState" => Ok(Some((state_json(model),).to_variant())),
            "SetSettings" => {
                let text = string(0);
                if text.chars().count() > 16384 {
                    return invalid("Settings are too large");
                }
                let patch: serde_json::Value = serde_json::from_str(&text).map_err(|e| MethodError::InvalidArgs(e.to_string()))?;
                model.set_settings(&patch).map_err(MethodError::InvalidArgs)?;
                Ok(None)
            }
            "ClearHistory" => { model.clear_history(); Ok(None) }
            "MarkRead" => { model.mark_read(); Ok(None) }
            "Dismiss" => { model.dismiss(uint(0)); Ok(None) }
            "ClearApp" => {
                let key = string(0);
                if key.chars().count() > 256 {
                    return invalid("Application name is too long");
                }
                model.clear_app(&key);
                Ok(None)
            }
            "Activate" => { model.activate(uint(0), &string(1)); Ok(None) }
            "ShowCenter" | "ShowCenterAt" | "ShowSettings" => {
                let command = match method {
                    "ShowCenter" => Command::ShowCenter,
                    "ShowSettings" => Command::ShowSettings,
                    _ => Command::ShowCenterAt(string(0), uint(1)),
                };
                (self.0.on_command)(command).map_err(MethodError::InvalidArgs)?;
                Ok(None)
            }
            _ => invalid("Unknown notification-center method"),
        }
    }
}

/// Marker message: the dispatcher replies `org.freedesktop.Notifications.InvalidId`.
pub const INVALID_ID: &str = "Notification does not exist";
