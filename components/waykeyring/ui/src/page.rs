//! The Passwords settings page Global Settings embeds: whether saved passwords
//! are protected and unlocked by the login, the saved entries (search, copy,
//! delete), Lock, and removing GNOME's old unencrypted keyring files once
//! they were imported. Everything goes through waykeyring on the session bus.

use gio::prelude::*;
use glib::variant::ObjectPath;
use gtk::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use suite_core::tr;

const NAME: &str = "org.freedesktop.secrets";
const ROOT: &str = "/org/freedesktop/secrets";
const CONTROL_PATH: &str = "/org/waydock/Keyring";
const CONTROL: &str = "org.waydock.Keyring";
const TIMEOUT_MS: i32 = 5000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub set_up: bool,
    pub unlocked: bool,
    pub unlocked_by_login: bool,
    pub items: u32,
    pub imported: u32,
    pub legacy_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: String,
    pub label: String,
    pub attributes: HashMap<String, String>,
    pub locked: bool,
}

impl Entry {
    /// "me @ github.com" style summary of the attributes.
    pub fn description(&self) -> String {
        let get = |k: &str| self.attributes.get(k).filter(|v| !v.is_empty()).cloned();
        let who = get("user").or_else(|| get("username")).or_else(|| get("account")).or_else(|| get("email"));
        let place = get("server").or_else(|| get("origin")).or_else(|| get("url")).or_else(|| get("host"))
            .or_else(|| get("service")).or_else(|| get("application")).or_else(|| get("app_id"));
        match (who, place) {
            (Some(w), Some(p)) => format!("{w} — {p}"),
            (Some(w), None) => w,
            (None, Some(p)) => p,
            (None, None) => get("xdg:schema").unwrap_or_default(),
        }
    }

    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty() || self.label.to_lowercase().contains(&query)
            || self.attributes.values().any(|v| v.to_lowercase().contains(&query))
    }
}

/// Thin synchronous Secret Service client (waykeyring answers locally and fast).
#[derive(Clone)]
pub struct Client {
    pub conn: gio::DBusConnection,
}

impl Client {
    pub fn session() -> Option<Self> {
        gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok().map(|conn| Client { conn })
    }

    fn call(&self, path: &str, interface: &str, method: &str, args: Option<&glib::Variant>)
        -> Result<glib::Variant, String> {
        self.conn.call_sync(Some(NAME), path, interface, method, args, None, gio::DBusCallFlags::NONE, TIMEOUT_MS,
                            None::<&gio::Cancellable>).map_err(|e| e.message().to_owned())
    }

    fn property(&self, path: &str, interface: &str, name: &str) -> Option<glib::Variant> {
        self.call(path, "org.freedesktop.DBus.Properties", "Get", Some(&(interface, name).to_variant())).ok()
            .and_then(|v| v.child_value(0).as_variant())
    }

    /// `Err` when waykeyring is not the Secret Service in this session.
    pub fn status(&self) -> Result<Status, String> {
        let reply = self.call(CONTROL_PATH, CONTROL, "Status", None)?;
        let map = reply.child_value(0).get::<HashMap<String, glib::Variant>>().unwrap_or_default();
        let flag = |k: &str| map.get(k).and_then(|v| v.get::<bool>()).unwrap_or(false);
        let number = |k: &str| map.get(k).and_then(|v| v.get::<u32>()).unwrap_or(0);
        Ok(Status {
            set_up: flag("set_up"), unlocked: flag("unlocked"), unlocked_by_login: flag("unlocked_by_login"),
            items: number("items"), imported: number("imported"),
            legacy_files: map.get("legacy_files").and_then(|v| v.get::<Vec<String>>()).unwrap_or_default(),
        })
    }

    /// Every item of every stored collection (not the session one).
    pub fn entries(&self) -> Vec<Entry> {
        let collections = self.property(ROOT, "org.freedesktop.Secret.Service", "Collections")
            .and_then(|v| v.get::<Vec<ObjectPath>>()).unwrap_or_default();
        let mut entries = Vec::new();
        for collection in collections.iter().filter(|c| !c.as_str().ends_with("/session")) {
            let items = self.property(collection, "org.freedesktop.Secret.Collection", "Items")
                .and_then(|v| v.get::<Vec<ObjectPath>>()).unwrap_or_default();
            for item in items {
                let Ok(all) = self.call(&item, "org.freedesktop.DBus.Properties", "GetAll",
                                        Some(&("org.freedesktop.Secret.Item",).to_variant())) else { continue };
                let props = all.child_value(0).get::<HashMap<String, glib::Variant>>().unwrap_or_default();
                entries.push(Entry {
                    path: item.to_string(),
                    label: props.get("Label").and_then(|v| v.get::<String>()).unwrap_or_default(),
                    attributes: props.get("Attributes").and_then(|v| v.get::<HashMap<String, String>>()).unwrap_or_default(),
                    locked: props.get("Locked").and_then(|v| v.get::<bool>()).unwrap_or(true),
                });
            }
        }
        entries.sort_by_key(|e| (e.label.to_lowercase(), e.path.clone()));
        entries
    }

    pub fn secret(&self, item: &str) -> Option<Vec<u8>> {
        let opened = self.call(ROOT, "org.freedesktop.Secret.Service", "OpenSession",
                               Some(&("plain", glib::Variant::from_variant(&"".to_variant())).to_variant())).ok()?;
        let session = opened.child_value(1).get::<ObjectPath>()?;
        let reply = self.call(item, "org.freedesktop.Secret.Item", "GetSecret", Some(&(session.clone(),).to_variant()));
        let _ = self.call(&session, "org.freedesktop.Secret.Session", "Close", None);
        let (_, _, value, _) = reply.ok()?.child_value(0).get::<(ObjectPath, Vec<u8>, Vec<u8>, String)>()?;
        Some(value)
    }

    pub fn delete(&self, item: &str) -> Result<(), String> {
        self.call(item, "org.freedesktop.Secret.Item", "Delete", None).map(|_| ())
    }

    pub fn lock(&self) -> Result<(), String> {
        self.call(CONTROL_PATH, CONTROL, "Lock", None).map(|_| ())
    }

    /// Asks for the password if needed; `done(unlocked)`.
    pub fn unlock(&self, done: impl FnOnce(bool) + 'static) {
        self.conn.call(Some(NAME), CONTROL_PATH, CONTROL, "Unlock", None, None, gio::DBusCallFlags::NONE, 5 * 60 * 1000,
                       None::<&gio::Cancellable>, move |reply| {
            done(reply.ok().and_then(|v| v.child_value(0).get::<bool>()).unwrap_or(false));
        });
    }

    pub fn remove_legacy_files(&self) -> Vec<String> {
        self.call(CONTROL_PATH, CONTROL, "RemoveLegacyFiles", None).ok()
            .and_then(|v| v.child_value(0).get::<Vec<String>>()).unwrap_or_default()
    }

    /// Call `changed` when waykeyring reports a change.
    pub fn watch(&self, changed: impl Fn() + 'static) -> gio::SignalSubscriptionId {
        #[allow(deprecated)]
        self.conn.signal_subscribe(Some(NAME), Some(CONTROL), Some("Changed"), Some(CONTROL_PATH), None,
                                   gio::DBusSignalFlags::NONE, move |_, _, _, _, _, _| changed())
    }
}

/// Status line for the page (pure, tested).
pub fn status_text(status: &Result<Status, String>) -> (String, String) {
    match status {
        Err(_) => (tr!("Another keyring service is running"),
                   tr!("Sign out and sign back in to switch to the desktop's own saved-password storage.")),
        Ok(s) if !s.set_up => (tr!("Not set up yet"),
                               tr!("Saved passwords are set up the next time you sign in with your password.")),
        Ok(s) if s.unlocked => (tr!("Protected by your login password"),
                                tr!("Saved passwords are encrypted on disk and were unlocked when you signed in.")),
        Ok(_) => (tr!("Locked"), tr!("Applications will ask for your login password the next time they need a saved password.")),
    }
}

pub struct PasswordsPage {
    pub page: gtk::Box,
    pub status_title: gtk::Label,
    pub status_detail: gtk::Label,
    pub lock: gtk::Button,
    pub unlock: gtk::Button,
    pub legacy_row: gtk::Box,
    pub legacy_remove: gtk::Button,
    pub search: gtk::SearchEntry,
    pub list: gtk::Box,
    pub empty: gtk::Label,
    pub client: Option<Client>,
    entries: RefCell<Vec<Entry>>,
    watch: RefCell<Option<gio::SignalSubscriptionId>>,
}

fn row(title: &str, subtitle: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("gs-row");
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
    let heading = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    heading.add_css_class("gs-row-title");
    let detail = gtk::Label::builder().label(subtitle).xalign(0.0).wrap(true).max_width_chars(60)
        .visible(!subtitle.is_empty()).build();
    detail.add_css_class("gs-row-subtitle");
    text.append(&heading);
    text.append(&detail);
    row.append(&text);
    (row, heading, detail)
}

fn card(page: &gtk::Box) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("gs-card");
    card.add_css_class("card");
    page.append(&card);
    card
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button.add_css_class("flat");
    button.set_valign(gtk::Align::Center);
    button
}

impl PasswordsPage {
    pub fn refresh(self: &Rc<Self>) {
        let status = match &self.client {
            Some(client) => client.status(),
            None => Err("no session bus".into()),
        };
        let (title, detail) = status_text(&status);
        self.status_title.set_label(&title);
        self.status_detail.set_label(&detail);
        let ours = status.as_ref().ok();
        self.lock.set_visible(ours.is_some_and(|s| s.unlocked));
        self.unlock.set_visible(ours.is_some_and(|s| s.set_up && !s.unlocked));
        self.legacy_row.set_visible(ours.is_some_and(|s| !s.legacy_files.is_empty() && s.imported > 0));
        let entries = match (&self.client, ours) {
            (Some(client), Some(_)) => client.entries(),
            _ => Vec::new(),
        };
        *self.entries.borrow_mut() = entries;
        self.fill();
    }

    fn fill(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let query = self.search.text().to_string();
        let entries = self.entries.borrow().clone();
        let shown: Vec<Entry> = entries.into_iter().filter(|e| e.matches(&query)).collect();
        self.empty.set_label(&if self.entries.borrow().is_empty() { tr!("No saved passwords.") }
                             else { tr!("Nothing matches your search.") });
        self.empty.set_visible(shown.is_empty());
        for entry in shown {
            let label = if entry.locked { tr!("Locked entry") }
                        else if entry.label.is_empty() { tr!("Unnamed entry") } else { entry.label.clone() };
            let (r, _, _) = row(&label, &entry.description());
            r.set_widget_name(&entry.path);
            if !entry.locked {
                let copy = icon_button("edit-copy-symbolic", &tr!("Copy password"));
                let delete = icon_button("user-trash-symbolic", &tr!("Delete"));
                let (weak, path) = (Rc::downgrade(self), entry.path.clone());
                copy.connect_clicked(move |button| {
                    if let Some(this) = weak.upgrade() {
                        this.copy(button, &path);
                    }
                });
                let (weak, path, name) = (Rc::downgrade(self), entry.path.clone(), label.clone());
                delete.connect_clicked(move |button| {
                    if let Some(this) = weak.upgrade() {
                        this.confirm_delete(button, &path, &name);
                    }
                });
                r.append(&copy);
                r.append(&delete);
            }
            self.list.append(&r);
        }
    }

    /// Copy marked as a password: clipboard managers (wayclip) do not keep it.
    fn copy(&self, widget: &gtk::Button, path: &str) {
        let Some(secret) = self.client.as_ref().and_then(|c| c.secret(path)) else { return };
        let text = glib::Bytes::from_owned(secret);
        let provider = gtk::gdk::ContentProvider::new_union(&[
            gtk::gdk::ContentProvider::for_bytes("text/plain;charset=utf-8", &text),
            gtk::gdk::ContentProvider::for_bytes("text/plain", &text),
            gtk::gdk::ContentProvider::for_bytes("x-kde-passwordManagerHint", &glib::Bytes::from_static(b"secret")),
        ]);
        let _ = widget.clipboard().set_content(Some(&provider));
    }

    fn confirm_delete(self: &Rc<Self>, widget: &gtk::Button, path: &str, name: &str) {
        let dialog = gtk::AlertDialog::builder()
            .message(tr!("Delete “{name}”?", name = name))
            .detail(tr!("Applications that saved it will ask for it again."))
            .buttons([tr!("Cancel"), tr!("Delete")]).cancel_button(0).default_button(0).modal(true).build();
        let window = widget.root().and_downcast::<gtk::Window>();
        let (weak, path) = (Rc::downgrade(self), path.to_owned());
        dialog.choose(window.as_ref(), None::<&gio::Cancellable>, move |choice| {
            if choice == Ok(1) {
                if let Some(this) = weak.upgrade() {
                    this.delete(&path);
                }
            }
        });
    }

    pub fn delete(self: &Rc<Self>, path: &str) {
        if let Some(client) = &self.client {
            let _ = client.delete(path);
        }
        self.refresh();
    }

    pub fn close(&self) {
        if let (Some(client), Some(id)) = (&self.client, self.watch.borrow_mut().take()) {
            #[allow(deprecated)]
            client.conn.signal_unsubscribe(id);
        }
    }
}

pub fn build_page(client: Option<Client>) -> Rc<PasswordsPage> {
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    page.add_css_class("passwords-settings");
    let status_card = card(&page);
    let (status_row, status_title, status_detail) = row("", "");
    let lock = gtk::Button::with_label(&tr!("Lock"));
    lock.set_valign(gtk::Align::Center);
    let unlock = gtk::Button::with_label(&tr!("Unlock"));
    unlock.set_valign(gtk::Align::Center);
    status_row.append(&lock);
    status_row.append(&unlock);
    status_card.append(&status_row);
    let (legacy_row, _, _) = row(&tr!("Old unencrypted keyring files"),
                                 &tr!("Their passwords were copied into your protected saved passwords. Remove the unprotected copies from disk."));
    let legacy_remove = gtk::Button::with_label(&tr!("Remove"));
    legacy_remove.add_css_class("destructive-action");
    legacy_remove.set_valign(gtk::Align::Center);
    legacy_row.append(&legacy_remove);
    status_card.append(&legacy_row);

    let heading = gtk::Label::builder().label(tr!("Saved passwords")).xalign(0.0).margin_top(6).build();
    heading.add_css_class("gs-section-title");
    page.append(&heading);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&tr!("Search saved passwords")));
    page.append(&search);
    let list = card(&page);
    let empty = gtk::Label::builder().xalign(0.0).margin_top(6).visible(false).build();
    empty.add_css_class("dim-label");
    page.append(&empty);

    let this = Rc::new(PasswordsPage {
        page, status_title, status_detail, lock, unlock, legacy_row, legacy_remove, search, list, empty, client,
        entries: RefCell::new(Vec::new()), watch: RefCell::new(None),
    });
    let weak = Rc::downgrade(&this);
    this.search.connect_search_changed(move |_| if let Some(t) = weak.upgrade() { t.fill() });
    let weak = Rc::downgrade(&this);
    this.lock.connect_clicked(move |_| if let Some(t) = weak.upgrade() {
        if let Some(c) = &t.client {
            let _ = c.lock();
        }
        t.refresh();
    });
    let weak = Rc::downgrade(&this);
    this.unlock.connect_clicked(move |_| if let Some(t) = weak.upgrade() {
        if let Some(c) = &t.client {
            let weak = Rc::downgrade(&t);
            c.unlock(move |_| if let Some(t) = weak.upgrade() { t.refresh() });
        }
    });
    let weak = Rc::downgrade(&this);
    this.legacy_remove.connect_clicked(move |_| if let Some(t) = weak.upgrade() {
        if let Some(c) = &t.client {
            c.remove_legacy_files();
        }
        t.refresh();
    });
    if let Some(client) = &this.client {
        let weak = Rc::downgrade(&this);
        *this.watch.borrow_mut() = Some(client.watch(move || if let Some(t) = weak.upgrade() { t.refresh() }));
    }
    this.refresh();
    this
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, attrs: &[(&str, &str)]) -> Entry {
        Entry { path: "/p".into(), label: label.into(), locked: false,
                attributes: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    #[test]
    fn descriptions_and_search() {
        assert_eq!(entry("x", &[("user", "me"), ("server", "github.com")]).description(), "me — github.com");
        assert_eq!(entry("x", &[("service", "Slack")]).description(), "Slack");
        assert_eq!(entry("x", &[("xdg:schema", "org.gnome.keyring.Note")]).description(), "org.gnome.keyring.Note");
        let e = entry("GitHub token", &[("server", "github.com")]);
        assert!(e.matches("") && e.matches("github") && e.matches("TOKEN") && !e.matches("gitlab"));
    }

    #[test]
    fn status_lines() {
        assert!(status_text(&Err("x".into())).0.contains("Another"));
        let unlocked = Status { set_up: true, unlocked: true, ..Default::default() };
        assert!(status_text(&Ok(unlocked)).0.contains("login password"));
        assert_eq!(status_text(&Ok(Status::default())).0, "Not set up yet");
    }
}
