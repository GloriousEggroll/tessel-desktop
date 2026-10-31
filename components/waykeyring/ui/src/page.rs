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
    /// KDE wallets not imported yet.
    pub kwallets: Vec<String>,
}

/// Outcome of an import: (added, already saved), or the D-Bus error name and message.
pub type ImportResult = Result<(u32, u32), (String, String)>;

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
            kwallets: map.get("kwallets").and_then(|v| v.get::<Vec<String>>()).unwrap_or_default(),
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

    fn import(&self, method: &str, args: glib::Variant, done: impl FnOnce(ImportResult) + 'static) {
        self.conn.call(Some(NAME), CONTROL_PATH, CONTROL, method, Some(&args), None, gio::DBusCallFlags::NONE,
                       5 * 60 * 1000, None::<&gio::Cancellable>, move |reply| {
            done(match reply {
                Ok(v) => Ok(v.get::<(u32, u32)>().unwrap_or((0, 0))),
                Err(e) => Err((gio::DBusError::remote_error(&e).map(|s| s.to_string()).unwrap_or_default(),
                               e.message().to_owned())),
            });
        });
    }

    /// Import a KDE wallet (`.kwl`) with its password.
    pub fn import_kwallet(&self, path: &str, password: &[u8], done: impl FnOnce(ImportResult) + 'static) {
        self.import("ImportKWallet", (path, password.to_vec()).to_variant(), done);
    }

    /// Import an unencrypted GNOME keyring file.
    pub fn import_gnome(&self, path: &str, done: impl FnOnce(ImportResult) + 'static) {
        self.import("ImportGnomeKeyring", (path,).to_variant(), done);
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

/// The message shown after an import (pure, tested).
pub fn import_message(result: &ImportResult) -> String {
    match result {
        Ok((0, 0)) => tr!("There was nothing to import."),
        Ok((added, 0)) => tr!("Imported {n} entries.", n = *added),
        Ok((added, skipped)) => tr!("Imported {n} entries; {m} were already saved.", n = *added, m = *skipped),
        Err((name, _)) if name.ends_with("WrongPassword") => tr!("That wallet password is not correct."),
        Err((name, _)) if name.ends_with("Locked") => tr!("Unlock your saved passwords first."),
        Err((_, message)) => tr!("Could not import: {error}", error = message.as_str()),
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
    pub import_card: gtk::Box,
    pub import_file: gtk::Button,
    pub import_status: gtk::Label,
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
        self.fill_imports(ours.map(|s| s.kwallets.clone()).unwrap_or_default(), ours.is_some());
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

    /// One row per KDE wallet still to import, above the "from a file" row.
    fn fill_imports(self: &Rc<Self>, kwallets: Vec<String>, available: bool) {
        let mut child = self.import_card.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.widget_name().starts_with("kwallet:") {
                self.import_card.remove(&widget);
            }
        }
        self.import_file.set_sensitive(available);
        for path in kwallets.iter().rev() {
            let name = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let (r, _, _) = row(&tr!("KDE Wallet “{name}”", name = name.as_str()),
                                &tr!("Copy its entries into your saved passwords. It asks for the wallet's password."));
            r.set_widget_name(&format!("kwallet:{path}"));
            let button = gtk::Button::with_label(&tr!("Import…"));
            button.set_valign(gtk::Align::Center);
            let (weak, path) = (Rc::downgrade(self), path.clone());
            button.connect_clicked(move |b| if let Some(t) = weak.upgrade() { t.ask_wallet_password(b, &path); });
            r.append(&button);
            self.import_card.prepend(&r);
        }
    }

    fn show_import_result(self: &Rc<Self>, result: &ImportResult) {
        self.import_status.set_label(&import_message(result));
        self.import_status.set_visible(true);
        self.refresh();
    }

    /// Small dialog: the wallet's password, retried in place when wrong.
    pub fn ask_wallet_password(self: &Rc<Self>, anchor: &impl IsA<gtk::Widget>, path: &str) -> gtk::Window {
        let name = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let title = tr!("Import KDE Wallet");
        let window = gtk::Window::builder().title(&title).modal(true).resizable(false).default_width(420).build();
        if let Some(parent) = anchor.root().and_downcast::<gtk::Window>() {
            window.set_transient_for(Some(&parent));
        }
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
        body.append(&gtk::Label::builder().label(tr!("Enter the password of the KDE wallet “{name}”. It is often your old login password.",
                                                     name = name.as_str())).xalign(0.0).wrap(true).max_width_chars(44).build());
        let entry = gtk::Entry::with_buffer(&gtk::PasswordEntryBuffer::new());
        entry.set_visibility(false);
        entry.set_input_purpose(gtk::InputPurpose::Password);
        let label = tr!("Wallet password");
        entry.set_placeholder_text(Some(&label));
        entry.update_property(&[gtk::accessible::Property::Label(&label)]);
        body.append(&entry);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        status.add_css_class("error");
        body.append(&status);
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        let import = gtk::Button::with_label(&tr!("Import"));
        import.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&import);
        body.append(&buttons);
        window.set_child(Some(&body));
        let w = window.clone();
        cancel.connect_clicked(move |_| w.close());
        let submit = {
            let (weak, path, window, entry, status, import) =
                (Rc::downgrade(self), path.to_owned(), window.clone(), entry.clone(), status.clone(), import.clone());
            Rc::new(move || {
                let Some(this) = weak.upgrade() else { return };
                let Some(client) = this.client.clone() else { return };
                import.set_sensitive(false);
                let password = entry.text().to_string();
                let (weak, window, entry, status, import) =
                    (Rc::downgrade(&this), window.clone(), entry.clone(), status.clone(), import.clone());
                client.import_kwallet(&path, password.as_bytes(), move |result| {
                    import.set_sensitive(true);
                    if matches!(&result, Err((name, _)) if name.ends_with("WrongPassword")) {
                        status.set_label(&import_message(&result));
                        status.set_visible(true);
                        entry.set_text("");
                        entry.grab_focus();
                        return;
                    }
                    entry.set_text("");
                    window.close();
                    if let Some(this) = weak.upgrade() {
                        this.show_import_result(&result);
                    }
                });
            })
        };
        let s = submit.clone();
        entry.connect_activate(move |_| s());
        import.connect_clicked(move |_| submit());
        window.present();
        entry.grab_focus();
        window
    }

    /// Import a chosen file: KDE wallets ask for their password, GNOME keyrings import directly.
    pub fn import_path(self: &Rc<Self>, anchor: &impl IsA<gtk::Widget>, path: &str) {
        if path.ends_with(".kwl") {
            self.ask_wallet_password(anchor, path);
        } else if let Some(client) = self.client.clone() {
            let weak = Rc::downgrade(self);
            client.import_gnome(path, move |result| if let Some(t) = weak.upgrade() { t.show_import_result(&result) });
        }
    }

    fn choose_file(self: &Rc<Self>, button: &gtk::Button) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&tr!("KDE wallets and GNOME keyrings")));
        filter.add_pattern("*.kwl");
        filter.add_pattern("*.keyring");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let start = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(".local/share");
        let dialog = gtk::FileDialog::builder().title(tr!("Import Saved Passwords")).modal(true).filters(&filters)
            .initial_folder(&gio::File::for_path(start)).build();
        let window = button.root().and_downcast::<gtk::Window>();
        let (weak, anchor) = (Rc::downgrade(self), button.clone());
        dialog.open(window.as_ref(), None::<&gio::Cancellable>, move |file| {
            let (Some(this), Ok(file)) = (weak.upgrade(), file) else { return };
            if let Some(path) = file.path() {
                this.import_path(&anchor, &path.to_string_lossy());
            }
        });
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

    let import_heading = gtk::Label::builder().label(tr!("Import")).xalign(0.0).margin_top(6).build();
    import_heading.add_css_class("gs-section-title");
    page.append(&import_heading);
    let import_card = card(&page);
    let (file_row, _, _) = row(&tr!("Import from a file"),
                               &tr!("A KDE wallet (.kwl) or an unencrypted GNOME keyring (.keyring)."));
    let import_file = gtk::Button::with_label(&tr!("Choose File…"));
    import_file.set_valign(gtk::Align::Center);
    file_row.append(&import_file);
    import_card.append(&file_row);
    let import_status = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    import_status.add_css_class("dim-label");
    page.append(&import_status);

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
        page, status_title, status_detail, lock, unlock, legacy_row, legacy_remove, import_card, import_file, import_status,
        search, list, empty, client,
        entries: RefCell::new(Vec::new()), watch: RefCell::new(None),
    });
    let weak = Rc::downgrade(&this);
    this.import_file.connect_clicked(move |b| if let Some(t) = weak.upgrade() { t.choose_file(b) });
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
    fn import_messages() {
        assert_eq!(import_message(&Ok((3, 0))), "Imported 3 entries.");
        assert_eq!(import_message(&Ok((1, 2))), "Imported 1 entries; 2 were already saved.");
        assert_eq!(import_message(&Err(("org.waydock.Keyring.Error.WrongPassword".into(), "x".into()))),
                   "That wallet password is not correct.");
        assert!(import_message(&Err(("org.waydock.Keyring.Error.Failed".into(), "bad".into()))).contains("bad"));
    }

    #[test]
    fn status_lines() {
        assert!(status_text(&Err("x".into())).0.contains("Another"));
        let unlocked = Status { set_up: true, unlocked: true, ..Default::default() };
        assert!(status_text(&Ok(unlocked)).0.contains("login password"));
        assert_eq!(status_text(&Ok(Status::default())).0, "Not set up yet");
    }
}
