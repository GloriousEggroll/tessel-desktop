//! users_page.py: your name, picture and password; other accounts for administrators.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gs_base::widgets::{label, label_with, run_in_thread, status_text, Page};
use gs_base::SettingsPage;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tessel_core::tr;
use tessel_gtk::utils::{image, key_controller};

use crate::users_backend::{change_password, Account, Accounts, AccountsService, PASSWD};

/// `changer(current, new) -> (okay, message)`; runs on a worker thread.
pub type Changer = Arc<dyn Fn(&str, &str) -> (bool, String) + Send + Sync>;

pub fn default_changer() -> Changer { Arc::new(|current, new| change_password(current, new, PASSWD, 45.0)) }

pub fn avatar(account: &Account, size: i32) -> gtk::Image {
    if !account.icon_file.is_empty() {
        if let Ok(texture) = gdk::Texture::from_filename(&account.icon_file) {
            let picture = gtk::Image::from_paintable(Some(&texture));
            picture.set_pixel_size(size);
            return picture;
        }
    }
    image("avatar-default-symbolic", size)
}

pub struct PasswordDialog {
    pub window: gtk::Window,
    pub current: gtk::PasswordEntry,
    pub new: gtk::PasswordEntry,
    pub confirm: gtk::PasswordEntry,
    pub message: gtk::Label,
    pub cancel: gtk::Button,
    pub change: gtk::Button,
    pub busy: Rc<Cell<bool>>,
    closed: Rc<Cell<bool>>,
    changer: Changer,
}

impl PasswordDialog {
    /// `PasswordDialog(parent, run, changer)`; `closed` is the page's closed flag (Page.run).
    pub fn new(parent: Option<&gtk::Window>, closed: Rc<Cell<bool>>, changer: Changer) -> Rc<PasswordDialog> {
        let window = gtk::Window::builder().title(tr!("Change Password")).modal(true).build();
        window.set_transient_for(parent);
        window.add_css_class("system-page");
        window.set_default_size(400, -1);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10)
            .margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
        window.set_child(Some(&content));
        let field = |title: String| {
            content.append(&label(&title, None));
            let entry = gtk::PasswordEntry::builder().show_peek_icon(true).build();
            entry.update_property(&[gtk::accessible::Property::Label(&title)]);
            content.append(&entry);
            entry
        };
        let current = field(tr!("Current password"));
        let new = field(tr!("New password"));
        let confirm = field(tr!("Confirm new password"));
        let message = label("", Some("detail"));
        content.append(&message);
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        let change = gtk::Button::builder().label(tr!("Change Password")).sensitive(false).build();
        change.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&change);
        content.append(&buttons);
        let this = Rc::new(PasswordDialog { window, current, new, confirm, message, cancel, change, busy: Rc::new(Cell::new(false)),
                                            closed, changer });
        let w = this.window.clone();
        this.cancel.connect_clicked(move |_| w.close());
        let weak = Rc::downgrade(&this);
        this.change.connect_clicked(move |_| if let Some(d) = weak.upgrade() { d.submit() });
        for entry in [&this.current, &this.new, &this.confirm] {
            let weak = Rc::downgrade(&this);
            entry.connect_changed(move |_| if let Some(d) = weak.upgrade() { d.validate() });
        }
        let weak = Rc::downgrade(&this);
        this.confirm.connect_activate(move |_| if let Some(d) = weak.upgrade() { if d.change.is_sensitive() { d.submit() } });
        let busy = this.busy.clone();
        this.window.connect_close_request(move |_| if busy.get() { glib::Propagation::Stop } else { glib::Propagation::Proceed });
        let (busy, w) = (this.busy.clone(), this.window.clone());
        key_controller(&this.window, false, move |_, key, _, _| {
            if key == gdk::Key::Escape && !busy.get() {
                w.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        // The dialog owns itself until it is destroyed.
        let keep = RefCell::new(Some(this.clone()));
        this.window.connect_destroy(move |_| { keep.borrow_mut().take(); });
        this
    }

    pub fn present(&self) { self.window.present() }

    pub fn validate(&self) {
        let (new, confirm) = (self.new.text(), self.confirm.text());
        if !confirm.is_empty() && new != confirm {
            status_text(&self.message, &tr!("The new passwords do not match."), Some("error"));
        } else {
            status_text(&self.message, "", None);
        }
        self.change.set_sensitive(!self.current.text().is_empty() && !new.is_empty() && new == confirm && !self.busy.get());
    }

    pub fn submit(self: &Rc<Self>) {
        let (current, new) = (self.current.text().to_string(), self.new.text().to_string());
        self.busy.set(true);
        for widget in [self.current.upcast_ref::<gtk::Widget>(), self.new.upcast_ref(), self.confirm.upcast_ref(),
                       self.change.upcast_ref(), self.cancel.upcast_ref()] {
            widget.set_sensitive(false);
        }
        status_text(&self.message, &tr!("Changing password…"), None);
        let changer = self.changer.clone();
        let this = self.clone();
        run_in_thread(self.closed.clone(), move || Ok(changer(&current, &new)), move |result| {
            this.busy.set(false);
            for widget in [this.current.upcast_ref::<gtk::Widget>(), this.new.upcast_ref(), this.confirm.upcast_ref(), this.cancel.upcast_ref()] {
                widget.set_sensitive(true);
            }
            let (okay, message) = result.unwrap_or_else(|error| (false, error));
            if okay {
                this.window.close();
                return;
            }
            this.validate();
            status_text(&this.message, &tr!("The password was not changed: {value1}", value1 = message), Some("error"));
        });
    }
}

pub struct UsersPage {
    pub page: Page,
    pub accounts: Arc<dyn Accounts>,
    pub changer: Changer,
    pub account: RefCell<Option<Account>>,
    pub avatar_button: gtk::Button,
    pub name_entry: gtk::Entry,
    pub user_label: gtk::Label,
    pub password: gtk::Button,
    pub status: gtk::Label,
    pub others_title: gtk::Label,
    pub others: gtk::ListBox,
}

impl UsersPage {
    pub fn new(accounts: Option<Arc<dyn Accounts>>, changer: Option<Changer>) -> Rc<UsersPage> {
        let page = Page::new("users-page", &tr!("Users"), &tr!("Manage your account."), Some("avatar-default-symbolic"));
        let card = page.group(None, None, None);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let avatar_button = gtk::Button::builder().tooltip_text(tr!("Change picture")).build();
        avatar_button.add_css_class("flat");
        avatar_button.update_property(&[gtk::accessible::Property::Label(&tr!("Change picture"))]);
        avatar_button.set_child(Some(&image("avatar-default-symbolic", 72)));
        top.append(&avatar_button);
        let names = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).hexpand(true).valign(gtk::Align::Center).build();
        let name_entry = gtk::Entry::builder().placeholder_text(tr!("Full name")).build();
        name_entry.update_property(&[gtk::accessible::Property::Label(&tr!("Full name"))]);
        names.append(&name_entry);
        let user_label = label("", Some("detail"));
        names.append(&user_label);
        top.append(&names);
        card.append(&top);
        let password = gtk::Button::builder().label(tr!("Change Password…")).halign(gtk::Align::Start).build();
        password.add_css_class("pill");
        Page::row(&card, &tr!("Password"), Some(password.upcast_ref()), None);
        let status = label("", Some("detail"));
        page.content.append(&status);
        let others_title = label(&tr!("Other users"), Some("group-title"));
        others_title.set_visible(false);
        page.content.append(&others_title);
        let others = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).visible(false).build();
        others.add_css_class("card");
        page.content.append(&others);
        let accounts = accounts.unwrap_or_else(|| Arc::new(AccountsService::default()));
        let this = Rc::new(UsersPage { page, accounts, changer: changer.unwrap_or_else(default_changer), account: RefCell::new(None),
                                       avatar_button, name_entry, user_label, password, status, others_title, others });
        let weak = Rc::downgrade(&this);
        this.avatar_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.choose_picture() });
        let weak = Rc::downgrade(&this);
        this.name_entry.connect_activate(move |_| if let Some(t) = weak.upgrade() { t.save_name() });
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&this);
        focus.connect_leave(move |_| if let Some(t) = weak.upgrade() { t.save_name() });
        this.name_entry.add_controller(focus);
        let weak = Rc::downgrade(&this);
        this.password.connect_clicked(move |_| if let Some(t) = weak.upgrade() {
            let root = t.page.widget.root().and_downcast::<gtk::Window>();
            PasswordDialog::new(root.as_ref(), t.page.closed_flag(), t.changer.clone()).present();
        });
        this.refresh();
        this
    }

    pub fn refresh(self: &Rc<Self>) {
        let accounts = self.accounts.clone();
        let weak = Rc::downgrade(self);
        self.page.run(move || accounts.current().map_err(|e| e.to_string()),
                      move |result| if let Some(t) = weak.upgrade() { t.current_loaded(result) });
    }

    fn current_loaded(self: &Rc<Self>, result: Result<Account, String>) {
        let account = match result {
            Ok(account) => account,
            Err(error) => {
                status_text(&self.status, &tr!("Account details could not be read: {value1}", value1 = error), Some("error"));
                return;
            }
        };
        self.name_entry.set_text(&account.real_name);
        let role = if account.administrator { tr!("Administrator") } else { tr!("Standard user") };
        self.user_label.set_text(&format!("{} · {role}", account.user_name));
        self.avatar_button.set_child(Some(&avatar(&account, 72)));
        let editable = !account.path.is_empty();
        self.name_entry.set_sensitive(editable);
        self.avatar_button.set_sensitive(editable);
        let load_others = account.administrator && !account.path.is_empty();
        *self.account.borrow_mut() = Some(account);
        if load_others {
            let accounts = self.accounts.clone();
            let weak = Rc::downgrade(self);
            self.page.run(move || accounts.others().map_err(|e| e.to_string()),
                          move |result| if let Some(t) = weak.upgrade() { t.others_loaded(result) });
        }
    }

    fn others_loaded(&self, result: Result<Vec<Account>, String>) {
        let Ok(accounts) = result else { return };
        if accounts.is_empty() {
            return;
        }
        tessel_gtk::utils::remove_children(&self.others);
        for account in &accounts {
            let row = gtk::Box::builder().spacing(12).margin_top(6).margin_bottom(6).build();
            row.append(&avatar(account, 32));
            let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).build();
            text.append(&label(account.display_name(), None));
            text.append(&label(&account.user_name, Some("detail")));
            row.append(&text);
            row.append(&label_with(&if account.administrator { tr!("Administrator") } else { tr!("Standard user") }, Some("detail"), false));
            self.others.append(&row);
        }
        self.others_title.set_visible(true);
        self.others.set_visible(true);
    }

    pub fn save_name(self: &Rc<Self>) {
        let Some(account) = self.account.borrow().clone().filter(|a| !a.path.is_empty()) else { return };
        let name = crate::compat::strip(&self.name_entry.text()).to_owned();
        if name == account.real_name {
            return;
        }
        *self.account.borrow_mut() = None;
        let accounts = self.accounts.clone();
        let weak = Rc::downgrade(self);
        let original = account.clone();
        self.page.run(move || accounts.set_real_name(&account, &name).map_err(|e| e.to_string()), move |result| {
            let Some(t) = weak.upgrade() else { return };
            match result {
                Err(error) => {
                    status_text(&t.status, &tr!("Your name could not be changed: {value1}", value1 = error), Some("error"));
                    t.name_entry.set_text(&original.real_name);
                }
                Ok(()) => {
                    status_text(&t.status, &tr!("Your name was changed."), None);
                    t.refresh();
                }
            }
        });
    }

    pub fn choose_picture(self: &Rc<Self>) {
        let Some(account) = self.account.borrow().clone() else { return };
        let dialog = gtk::FileDialog::builder().title(tr!("Choose a Picture")).modal(true).build();
        let images = gtk::FileFilter::new();
        images.set_name(Some(&tr!("Images")));
        images.add_mime_type("image/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&images);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&images));
        let weak = Rc::downgrade(self);
        let root = self.page.widget.root().and_downcast::<gtk::Window>();
        dialog.open(root.as_ref(), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            let Some(t) = weak.upgrade() else { return };
            let accounts = t.accounts.clone();
            let path = path.to_string_lossy().into_owned();
            let weak = Rc::downgrade(&t);
            t.page.run(move || accounts.set_icon(&account, &path).map_err(|e| e.to_string()), move |result| {
                let Some(t) = weak.upgrade() else { return };
                match result {
                    Err(error) => status_text(&t.status, &tr!("Your picture could not be changed: {value1}", value1 = error), Some("error")),
                    Ok(()) => t.refresh(),
                }
            });
        });
    }
}

pub fn build_page_with(accounts: Option<Arc<dyn Accounts>>, changer: Option<Changer>) -> SettingsPage {
    let controller = UsersPage::new(accounts, changer);
    let weak = Rc::downgrade(&controller);
    let page = SettingsPage::new(controller.page.widget.clone()).with_close(move || if let Some(c) = weak.upgrade() { c.page.close() });
    page.add_target("users-name", &controller.name_entry);
    page.add_target("users-picture", &controller.avatar_button);
    page.add_target("users-password", &controller.password);
    page.add_target("users-others", &controller.others);
    page.set_controller(controller);
    page
}
