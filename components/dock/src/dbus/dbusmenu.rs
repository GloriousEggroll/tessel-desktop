//! dbusmenu.py `RemoteMenu`, model side: a `com.canonical.dbusmenu` client
//! rendered as a native `gio::Menu` + `gio::SimpleActionGroup` (insert it as
//! the `"remote"` action group). The widget (UI agent) owns the
//! `gtk::PopoverMenu` (`has_arrow=false`, CSS `tessel-dock-context-menu` + palette
//! class), the capture-phase `gtk::ShortcutController` built from
//! `shortcuts()`, and calls `popup_opened()` before `popover.popup()`.
//!
//! Protocol: `GetLayout(0, -1, [])` (coalesced: one in flight, one queued),
//! re-rendered only when the layout differs; `LayoutUpdated` /
//! `ItemsPropertiesUpdated` refresh; `AboutToShow(i)` → refresh when it returns
//! true; `Event(id, "clicked"|"opened"|"closed", <i 0>, 0)`; a click calls the
//! `on_clicked` hook (popdown). Rendering: checkmark (stateful `b`), radio
//! (stateful `s`, target = id), disabled, hidden, separators → sections,
//! submenus (`submenu<id>` stateful `b`, `submenu-action` attribute, opened /
//! closed events), `icon-name` / `icon-data`, `shortcut` → `accel` attribute.
//! Depth > 16 or more than 2048 items are cut. Action names are
//! `item<id>` with `-` → `n`.
//!
//! API: `RemoteMenu::new(&connection, bus_name, path)`, `.model()`, `.actions()`,
//! `.shortcuts()`, `.layout()`, `.set_on_render(cb)`, `.set_on_clicked(cb)`,
//! `.refresh()`, `.about_to_show(id)`, `.event(id, name)`, `.popup_opened()`,
//! `.activate_shortcut(id, mapped) -> bool`, `.close()`, `.closed()`.

use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

pub const INTERFACE: &str = "com.canonical.dbusmenu";

/// One keyboard shortcut of a leaf item: GTK trigger string (`<Control>q`) and the item id.
#[derive(Clone, Debug)]
pub struct MenuShortcut {
    pub accel: String,
    pub ident: i32,
    pub action: gio::SimpleAction,
}

struct Inner {
    connection: gio::DBusConnection,
    bus_name: String,
    path: String,
    closed: Cell<bool>,
    pending: Cell<bool>,
    again: Cell<bool>,
    layout: RefCell<Option<Variant>>,
    actions: RefCell<gio::SimpleActionGroup>,
    model: RefCell<gio::Menu>,
    shortcuts: RefCell<Vec<MenuShortcut>>,
    on_render: RefCell<Option<Rc<dyn Fn()>>>,
    on_clicked: RefCell<Option<Rc<dyn Fn()>>>,
    subscription: RefCell<Option<gio::SignalSubscription>>,
    cancellable: gio::Cancellable,
}

#[derive(Clone)]
pub struct RemoteMenu(Rc<Inner>);

fn properties(value: &Variant) -> HashMap<String, Variant> {
    let mut result = HashMap::new();
    for entry in value.iter() {
        if let (Some(key), Some(value)) = (entry.child_value(0).get::<String>(), entry.child_value(1).as_variant()) {
            result.insert(key, value);
        }
    }
    result
}

fn truthy(value: Option<&Variant>, default: bool) -> bool {
    value.map(|v| tessel_core::pyjson::truthy(&tessel_core::values::to_value(v))).unwrap_or(default)
}

fn text(value: Option<&Variant>) -> String {
    match value {
        None => String::new(),
        Some(value) => value.str().map(str::to_owned).unwrap_or_else(|| tessel_core::pyjson::py_str(&tessel_core::values::to_value(value))),
    }
}

fn string(value: Option<&Variant>) -> Option<String> { value.and_then(|v| v.str().map(str::to_owned)) }

fn integer(value: Option<&Variant>) -> Option<i64> {
    let value = value?;
    value.get::<i32>().map(i64::from).or_else(|| value.get::<i64>()).or_else(|| value.get::<u32>().map(i64::from))
        .or_else(|| value.get::<bool>().map(i64::from))
}

/// `(ident, properties, children)` of a `(ia{sv}av)` node (possibly boxed in `v`).
fn node(value: &Variant) -> Option<(i32, HashMap<String, Variant>, Vec<Variant>)> {
    let value = if value.type_() == glib::VariantTy::VARIANT { value.as_variant()? } else { value.clone() };
    if value.type_().as_str() != "(ia{sv}av)" {
        return None;
    }
    let ident = value.child_value(0).get::<i32>()?;
    let props = properties(&value.child_value(1));
    let children = value.child_value(2).iter().collect();
    Some((ident, props, children))
}

impl RemoteMenu {
    pub fn new(connection: &gio::DBusConnection, bus_name: &str, path: &str) -> Self {
        let menu = Self(Rc::new(Inner {
            connection: connection.clone(), bus_name: bus_name.into(), path: path.into(), closed: Cell::new(false),
            pending: Cell::new(false), again: Cell::new(false), layout: RefCell::new(None),
            actions: RefCell::new(gio::SimpleActionGroup::new()), model: RefCell::new(gio::Menu::new()),
            shortcuts: RefCell::default(), on_render: RefCell::new(None), on_clicked: RefCell::new(None),
            subscription: RefCell::new(None), cancellable: gio::Cancellable::new(),
        }));
        let weak = menu.weak();
        let subscription = connection.subscribe_to_signal(Some(bus_name), Some(INTERFACE), None, Some(path), None,
            gio::DBusSignalFlags::NONE, move |signal| {
                if matches!(signal.signal_name, "LayoutUpdated" | "ItemsPropertiesUpdated") {
                    if let Some(inner) = weak.upgrade() { RemoteMenu(inner).refresh() }
                }
            });
        menu.0.subscription.replace(Some(subscription));
        menu.refresh();
        menu
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    pub fn bus_name(&self) -> &str { &self.0.bus_name }
    pub fn path(&self) -> &str { &self.0.path }
    pub fn closed(&self) -> bool { self.0.closed.get() }
    pub fn model(&self) -> gio::Menu { self.0.model.borrow().clone() }
    pub fn actions(&self) -> gio::SimpleActionGroup { self.0.actions.borrow().clone() }
    pub fn shortcuts(&self) -> Vec<MenuShortcut> { self.0.shortcuts.borrow().clone() }
    /// The last rendered layout (`None` until the first GetLayout reply).
    pub fn layout(&self) -> Option<Variant> { self.0.layout.borrow().clone() }
    /// Called after every re-render: re-insert `actions()` as "remote", set `model()`, rebuild shortcuts.
    pub fn set_on_render(&self, callback: Option<Rc<dyn Fn()>>) { self.0.on_render.replace(callback); }
    /// Called after a "clicked" event (the popover pops down).
    pub fn set_on_clicked(&self, callback: Option<Rc<dyn Fn()>>) { self.0.on_clicked.replace(callback); }

    fn call(&self, method: &str, parameters: Variant, callback: Option<Box<dyn FnOnce(&RemoteMenu, Option<Variant>)>>) {
        if self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        self.0.connection.call(Some(&self.0.bus_name), &self.0.path, INTERFACE, method, Some(&parameters), None,
            gio::DBusCallFlags::NONE, 3000, Some(&self.0.cancellable), move |result| {
                let Some(inner) = weak.upgrade() else { return };
                let menu = RemoteMenu(inner);
                if !menu.0.closed.get() {
                    if let Some(callback) = callback {
                        callback(&menu, result.ok());
                    }
                }
            });
    }

    pub fn refresh(&self) {
        if self.0.closed.get() {
            return;
        }
        if self.0.pending.get() {
            self.0.again.set(true);
            return;
        }
        self.0.pending.set(true);
        self.call("GetLayout", (0i32, -1i32, Vec::<String>::new()).to_variant(), Some(Box::new(|menu, value| {
            menu.0.pending.set(false);
            if let Some(value) = value {
                if value.is_container() && value.n_children() == 2 {
                    let layout = value.child_value(1);
                    if node(&layout).is_some() && menu.0.layout.borrow().as_ref() != Some(&layout) {
                        menu.0.layout.replace(Some(layout));
                        menu.render();
                    }
                }
            }
            if menu.0.again.get() {
                menu.0.again.set(false);
                menu.refresh();
            }
        })));
    }

    pub fn about_to_show(&self, ident: i32) {
        self.call("AboutToShow", (ident,).to_variant(), Some(Box::new(|menu, value| {
            if value.and_then(|v| v.try_child_value(0)).and_then(|v| v.get::<bool>()).unwrap_or(false) {
                menu.refresh();
            }
        })));
    }

    pub fn event(&self, ident: i32, name: &str) {
        self.call("Event", (ident, name, 0i32.to_variant(), 0u32).to_variant(), None);
        if name == "clicked" {
            let callback = self.0.on_clicked.borrow().clone();
            if let Some(callback) = callback {
                callback();
            }
        }
    }

    /// `RemoteMenu.popup()` minus the popover: AboutToShow(0) + Event(0, "opened").
    pub fn popup_opened(&self) {
        if self.0.closed.get() {
            return;
        }
        self.about_to_show(0);
        self.event(0, "opened");
    }

    /// The shortcut callback: dispatches only while the menu is open (`mapped`) and the item enabled.
    pub fn activate_shortcut(&self, ident: i32, mapped: bool) -> bool {
        let action = self.0.shortcuts.borrow().iter().find(|s| s.ident == ident).map(|s| s.action.clone());
        match action {
            Some(action) if !self.0.closed.get() && mapped && action.is_enabled() => {
                self.event(ident, "clicked");
                true
            }
            _ => false,
        }
    }

    fn render(&self) {
        let Some(layout) = self.layout() else { return };
        let actions = gio::SimpleActionGroup::new();
        let mut shortcuts = Vec::new();
        let mut budget = 2048i32;
        let model = self.build(&layout, 0, &actions, &mut shortcuts, &mut budget);
        self.0.actions.replace(actions);
        self.0.model.replace(model);
        self.0.shortcuts.replace(shortcuts);
        let callback = self.0.on_render.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    fn build(&self, value: &Variant, depth: u32, actions: &gio::SimpleActionGroup, shortcuts: &mut Vec<MenuShortcut>,
             budget: &mut i32) -> gio::Menu {
        let result = gio::Menu::new();
        let mut section = gio::Menu::new();
        let Some((_ident, _props, children)) = node(value) else { return result };
        for child in children {
            if depth > 16 || *budget <= 0 {
                break;
            }
            *budget -= 1;
            let Some((key, props, descendants)) = node(&child) else { continue };
            if !truthy(props.get("visible"), true) {
                continue;
            }
            if string(props.get("type")).as_deref() == Some("separator") {
                if section.n_items() > 0 {
                    result.append_section(None, &section);
                    section = gio::Menu::new();
                }
                continue;
            }
            let item = gio::MenuItem::new(Some(&text(props.get("label"))), None);
            let suffix = key.to_string().replace('-', "n");
            let enabled = truthy(props.get("enabled"), true);
            let is_submenu = !descendants.is_empty() || string(props.get("children-display")).as_deref() == Some("submenu");
            let action;
            if is_submenu {
                item.set_submenu(Some(&self.build(&child, depth + 1, actions, shortcuts, budget)));
                let name = format!("submenu{suffix}");
                action = gio::SimpleAction::new_stateful(&name, None, &false.to_variant());
                let weak = self.weak();
                action.connect_change_state(move |action, value| {
                    let Some(value) = value else { return };
                    action.set_state(value);
                    let Some(inner) = weak.upgrade() else { return };
                    let menu = RemoteMenu(inner);
                    if value.get::<bool>().unwrap_or(false) {
                        menu.about_to_show(key);
                        menu.event(key, "opened");
                    } else {
                        menu.event(key, "closed");
                    }
                });
                item.set_attribute_value("submenu-action", Some(&format!("remote.{name}").to_variant()));
            } else {
                let name = format!("item{suffix}");
                let checked = integer(props.get("toggle-state")).unwrap_or(0) == 1;
                match string(props.get("toggle-type")).as_deref() {
                    Some("checkmark") => {
                        action = gio::SimpleAction::new_stateful(&name, None, &checked.to_variant());
                        item.set_detailed_action(&format!("remote.{name}"));
                    }
                    Some("radio") => {
                        let state = if checked { key.to_string() } else { String::new() };
                        action = gio::SimpleAction::new_stateful(&name, Some(glib::VariantTy::STRING), &state.to_variant());
                        item.set_action_and_target_value(Some(&format!("remote.{name}")), Some(&key.to_string().to_variant()));
                    }
                    _ => {
                        action = gio::SimpleAction::new(&name, None);
                        item.set_detailed_action(&format!("remote.{name}"));
                    }
                }
                let weak = self.weak();
                action.connect_activate(move |_, _| {
                    if let Some(inner) = weak.upgrade() { RemoteMenu(inner).event(key, "clicked") }
                });
            }
            action.set_enabled(enabled);
            actions.add_action(&action);
            if let Some(icon) = string(props.get("icon-name")).filter(|name| !name.is_empty()) {
                item.set_icon(&gio::ThemedIcon::new(&icon));
            } else if let Some(data) = props.get("icon-data").filter(|v| truthy(Some(v), false)) {
                if let Ok(bytes) = data.fixed_array::<u8>() {
                    item.set_icon(&gio::BytesIcon::new(&glib::Bytes::from(bytes)));
                }
            }
            if let Some(shortcut) = props.get("shortcut") {
                let first = shortcut.try_child_value(0).and_then(|v| v.get::<Vec<String>>()).filter(|s| !s.is_empty());
                if let Some(sequence) = first {
                    let (last, modifiers) = sequence.split_last().unwrap();
                    let accel = modifiers.iter().map(|m| format!("<{m}>")).collect::<String>() + last;
                    item.set_attribute_value("accel", Some(&accel.to_variant()));
                    if descendants.is_empty() {
                        shortcuts.push(MenuShortcut { accel, ident: key, action: action.clone() });
                    }
                }
            }
            section.append_item(&item);
        }
        if section.n_items() > 0 {
            result.append_section(None, &section);
        }
        result
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.cancellable.cancel();
        self.0.subscription.take();
        self.0.on_render.replace(None);
        self.0.on_clicked.replace(None);
    }
}
