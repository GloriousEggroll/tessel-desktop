//! `connection_wizard.py`: application-owned connection chooser and profile
//! editor (our forms; never nm-connection-editor or plugin GUI widgets).

use crate::backend::{NetworkBackend, Remote};
use crate::gui::Gui;
use crate::model::friendly_text;
use crate::profiles::{connection_types, edit_draft, new_draft, ProfileDraft, ProfileStore};
use crate::ui::forms::{self, available_optional_settings, build_pages, Page};
use crate::ui::style::{expand, icon, label, margins, remove_children, style};
use crate::ui::vpn_editor::{VpnPluginCatalog, VpnSettingsForm};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use suite_core::tr;

#[derive(Default)]
pub struct WizardOptions {
    pub remote: Option<Remote>,
    pub connection_type: Option<String>,
    pub draft: Option<ProfileDraft>,
    pub activate: bool,
    pub device_id: Option<String>,
    pub ap_path: Option<String>,
}

pub struct ConnectionWizard {
    pub window: gtk::Window,
    pub title: gtk::Label,
    pub subtitle: gtk::Label,
    pub save_button: gtk::Button,
    pub action_label: RefCell<String>,
    gui: Weak<Gui>,
    backend: NetworkBackend,
    pub draft: RefCell<Option<Rc<RefCell<ProfileDraft>>>>,
    activate: bool,
    device_id: Rc<RefCell<Option<String>>>,
    ap_path: Option<String>,
    pub pages: RefCell<Vec<(String, Rc<dyn Page>)>>,
    closed: Cell<bool>,
    saving: Cell<bool>,
    choice: RefCell<Option<(String, String)>>,
    catalog: VpnPluginCatalog,
    store: ProfileStore,
    stack: gtk::Stack,
    pub search: gtk::SearchEntry,
    pub choices: gtk::ListBox,
    editor: gtk::Box,
    editor_toolbar: gtk::Box,
    page_box: gtk::Box,
    pub page_stack: RefCell<Option<gtk::Stack>>,
    pub error_label: gtk::Label,
    pub note: gtk::Label,
    pub import_button: gtk::Button,
    pub export_button: gtk::Button,
    pub back_button: gtk::Button,
    pub cancel_button: gtk::Button,
    weak: Weak<ConnectionWizard>,
}

fn choice_of(row: &gtk::ListBoxRow) -> Option<(String, String)> {
    unsafe { row.data::<(String, String)>("choice").map(|c| c.as_ref().clone()) }
}

fn search_text_of(row: &gtk::ListBoxRow) -> String {
    unsafe { row.data::<String>("search-text").map(|c| c.as_ref().clone()) }.unwrap_or_default()
}

impl ConnectionWizard {
    pub fn new(gui: &Gui, options: WizardOptions) -> Result<Rc<Self>, String> {
        let app = gui.app().ok_or("The network service is closing.")?;
        let backend = app.backend();
        let action_label = if options.activate { tr!("Connect") } else { tr!("Save connection") };
        let window = gtk::Window::new();
        window.set_title(Some(&if options.remote.is_some() { tr!("Edit Connection") } else { tr!("Add Connection") }));
        window.set_default_size(820, 680);
        window.set_size_request(660, 460);
        style(&window, &["networkmanager"]);
        window.set_titlebar(Some(&gtk::HeaderBar::builder().show_title_buttons(true).build()));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        margins(&root, 22);
        window.set_child(Some(&root));
        let title = label(&tr!("Add a connection"), Some("title"));
        root.append(&title);
        let subtitle = label(&tr!("Choose the type of connection you want to set up."), Some("subtitle"));
        subtitle.set_wrap(true);
        root.append(&subtitle);
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
        stack.set_vexpand(true);
        root.append(&expand(&stack, &root));
        let chooser = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(&tr!("Search connection types")));
        chooser.append(&search);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let choices = style(&gtk::ListBox::new(), &["device-list", "connection-types"]);
        scroll.set_child(Some(&choices));
        chooser.append(&expand(&scroll, &chooser));
        stack.add_named(&chooser, Some("choose"));
        let editor = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let editor_toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        editor.append(&editor_toolbar);
        let page_box = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        editor.append(&expand(&page_box, &editor));
        stack.add_named(&editor, Some("edit"));
        let error_label = label("", Some("error"));
        error_label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        error_label.set_wrap(true);
        error_label.set_max_width_chars(85);
        error_label.set_visible(false);
        root.append(&error_label);
        let note = label("", Some("muted"));
        note.set_wrap(true);
        note.set_max_width_chars(85);
        root.append(&note);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let import_button = gtk::Button::with_label(&tr!("Import VPN…"));
        import_button.set_visible(false);
        actions.append(&import_button);
        let export_button = gtk::Button::with_label(&tr!("Export VPN…"));
        export_button.set_visible(false);
        actions.append(&export_button);
        let back_button = gtk::Button::with_label(&tr!("Back"));
        back_button.set_visible(false);
        actions.append(&back_button);
        let save_button = style(&gtk::Button::with_label(&tr!("Continue")), &["primary"]);
        let cancel_button = gtk::Button::with_label(&tr!("Cancel"));
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        actions.append(&spacer);
        actions.append(&cancel_button);
        actions.append(&save_button);
        root.append(&actions);
        let wizard = Rc::new_cyclic(|weak: &Weak<ConnectionWizard>| {
            let (w1, w2) = (weak.clone(), weak.clone());
            let store = ProfileStore::new(&backend, move || if let Some(w) = w1.upgrade() { w.store_changed() },
                                          move |m| if let Some(w) = w2.upgrade() { w.show_error(m) });
            Self { window: window.clone(), title, subtitle, save_button: save_button.clone(), action_label: RefCell::new(action_label),
                   gui: gui.weak(), backend: backend.clone(), draft: RefCell::new(None), activate: options.activate,
                   device_id: Rc::new(RefCell::new(options.device_id.clone())), ap_path: options.ap_path.clone(),
                   pages: RefCell::default(), closed: Cell::new(false), saving: Cell::new(false), choice: RefCell::new(None),
                   catalog: VpnPluginCatalog::installed(), store, stack, search: search.clone(), choices: choices.clone(), editor,
                   editor_toolbar, page_box, page_stack: RefCell::new(None), error_label, note, import_button: import_button.clone(),
                   export_button: export_button.clone(), back_button: back_button.clone(), cancel_button: cancel_button.clone(),
                   weak: weak.clone() }
        });
        wizard.populate_choices();
        let w = Rc::downgrade(&wizard);
        choices.set_filter_func(move |row| {
            w.upgrade().is_none_or(|wz| search_text_of(row).contains(&suite_core::pyjson::casefold(&wz.search.text())))
        });
        let w = Rc::downgrade(&wizard);
        search.connect_search_changed(move |_| if let Some(wz) = w.upgrade() { wz.choices.invalidate_filter() });
        let w = Rc::downgrade(&wizard);
        choices.connect_row_selected(move |_, row| if let Some(wz) = w.upgrade() {
            wz.choice.replace(row.and_then(choice_of));
            wz.save_button.set_sensitive(wz.choice.borrow().is_some());
        });
        let w = Rc::downgrade(&wizard);
        choices.connect_row_activated(move |_, _| if let Some(wz) = w.upgrade() { wz.continue_choice() });
        let w = Rc::downgrade(&wizard);
        import_button.connect_clicked(move |_| if let Some(wz) = w.upgrade() { wz.import() });
        let w = Rc::downgrade(&wizard);
        export_button.connect_clicked(move |_| if let Some(wz) = w.upgrade() { wz.export() });
        let w = Rc::downgrade(&wizard);
        back_button.connect_clicked(move |_| if let Some(wz) = w.upgrade() { wz.show_chooser() });
        let w = Rc::downgrade(&wizard);
        save_button.connect_clicked(move |_| if let Some(wz) = w.upgrade() { wz.next_or_save() });
        let w = Rc::downgrade(&wizard);
        cancel_button.connect_clicked(move |_| if let Some(wz) = w.upgrade() { wz.close() });
        let w = Rc::downgrade(&wizard);
        window.connect_close_request(move |_| {
            if w.upgrade().is_some_and(|wz| wz.saving.get()) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        let w = Rc::downgrade(&wizard);
        window.connect_destroy(move |_| if let Some(wz) = w.upgrade() { wz.destroyed() });
        // A Stack can only select a visible child: prepare before choosing.
        root.set_visible(true);
        if let Some(remote) = &options.remote {
            wizard.set_draft(edit_draft(remote)?);
            wizard.show_editor();
        } else if let Some(draft) = options.draft {
            wizard.set_draft(draft);
            wizard.show_editor();
        } else if let Some(kind) = &options.connection_type {
            let kind = match kind.as_str() { "wired" => "802-3-ethernet", "wifi" => "802-11-wireless", "mobile" => "gsm", other => other };
            wizard.set_draft(new_draft(kind, None)?);
            wizard.show_editor();
        } else {
            wizard.show_chooser();
        }
        Ok(wizard)
    }

    fn set_draft(&self, draft: ProfileDraft) {
        self.draft.replace(Some(Rc::new(RefCell::new(draft))));
    }

    fn connection(&self) -> Option<crate::libnm::Connection> {
        self.draft.borrow().as_ref().map(|d| d.borrow().connection.clone())
    }

    fn populate_choices(&self) {
        let mut choices: Vec<(String, String, String, String)> = Vec::new();
        for item in connection_types() {
            if item.setting == "vpn" {
                continue;
            }
            choices.push((item.name, item.category, item.id, String::new()));
        }
        for plugin in &self.catalog.plugins {
            choices.push((plugin.name.clone(), "VPN".into(), "vpn".into(), plugin.service.clone()));
        }
        for (name, category, kind, service) in choices {
            let row = gtk::ListBoxRow::new();
            let shown = if service.is_empty() { suite_core::i18n::translate(&name) } else { name.clone() };
            let search = suite_core::pyjson::casefold(&format!("{shown} {} {name}", suite_core::i18n::translate(&category)));
            unsafe {
                row.set_data("choice", (kind.clone(), service.clone()));
                row.set_data("search-text", search);
            }
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 14);
            margins(&content, 12);
            let symbol = if category == "VPN" || kind == "wireguard" { "network-vpn-symbolic" }
                else if kind == "802-11-wireless" { "network-wireless-symbolic" }
                else if kind == "gsm" || kind == "cdma" { "network-cellular-symbolic" } else { "network-wired-symbolic" };
            content.append(&icon(symbol, 22));
            let titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
            titles.append(&label(&shown, Some("device-name")));
            titles.append(&label(&suite_core::i18n::translate(&category), Some("device-detail")));
            content.append(&expand(&titles, &content));
            content.append(&icon("go-next-symbolic", 16));
            row.set_child(Some(&content));
            self.choices.append(&row);
        }
    }

    pub fn show_chooser(&self) {
        self.stack.set_visible_child_name("choose");
        self.title.set_text(&tr!("Add a connection"));
        self.subtitle.set_text(&tr!("Choose the type of connection you want to set up."));
        self.back_button.set_visible(false);
        self.import_button.set_visible(true);
        self.export_button.set_visible(false);
        self.save_button.set_label(&tr!("Continue"));
        self.save_button.set_sensitive(self.choice.borrow().is_some());
        self.note.set_text("");
        self.show_error("");
    }

    fn continue_choice(&self) {
        let Some((kind, service)) = self.choice.borrow().clone() else { return };
        match new_draft(&kind, None) {
            Ok(draft) => {
                if !service.is_empty() {
                    if let Some(vpn) = draft.connection.setting("vpn") {
                        vpn.set_str("service-type", Some(&service));
                    }
                }
                self.set_draft(draft);
                self.show_editor();
            }
            Err(error) => self.show_error(&friendly_text(&error, false)),
        }
    }

    pub fn show_editor(&self) {
        let Some(connection) = self.connection() else { return };
        let original = self.draft.borrow().as_ref().is_some_and(|d| d.borrow().original.is_some());
        self.stack.set_visible_child_name("edit");
        self.title.set_text(&if self.activate { tr!("Connect to Wi-Fi") } else if original { tr!("Edit connection") }
                            else { tr!("Set up your connection") });
        let id = connection.id();
        self.subtitle.set_text(&if id.is_empty() { tr!("Connection settings") } else { id });
        self.import_button.set_visible(false);
        let plugin = connection.setting("vpn").and_then(|v| self.catalog.find(&v.vpn_service_type()).cloned());
        self.export_button.set_visible(plugin.is_some_and(|p| p.can_export));
        self.back_button.set_visible(!original && !self.activate);
        self.save_button.set_label(&self.action_label.borrow());
        self.save_button.set_sensitive(true);
        self.note.set_text(&if self.activate { String::new() } else { tr!("Save this profile without changing your current connection.") });
        self.show_error("");
        self.build_toolbar();
        self.build_pages();
        if self.activate {
            if let Some(stack) = self.page_stack.borrow().as_ref() {
                stack.set_visible_child_name("802-11-wireless");
            }
        }
    }

    #[allow(deprecated)]
    fn build_toolbar(&self) {
        remove_children(&self.editor_toolbar);
        if self.activate {
            self.editor_toolbar.append(&label(&tr!("Wi-Fi adapter"), Some("section-title")));
            let combo = gtk::ComboBoxText::new();
            combo.set_hexpand(true);
            for device in self.backend.state().devices.values() {
                if device.kind == "wifi" && device.enabled() {
                    combo.append(Some(&device.id), &format!("{} ({})", device.name, device.interface));
                }
            }
            let current = self.device_id.borrow().clone();
            match current {
                Some(id) => { combo.set_active_id(Some(&id)); }
                None => {
                    combo.set_active(Some(0));
                    self.device_id.replace(combo.active_id().map(|s| s.to_string()));
                }
            }
            combo.set_sensitive(self.ap_path.is_none());
            let device_id = self.device_id.clone();
            combo.connect_changed(move |c| { device_id.replace(c.active_id().map(|s| s.to_string())); });
            self.editor_toolbar.append(&expand(&combo, &self.editor_toolbar));
        }
        let options = self.connection().map(|c| available_optional_settings(&c)).unwrap_or_default();
        if !options.is_empty() {
            let combo = gtk::ComboBoxText::new();
            combo.append(Some(""), &tr!("Additional settings…"));
            for (name, title) in &options {
                combo.append(Some(name), title);
            }
            combo.set_active(Some(0));
            let weak = self.self_weak();
            combo.connect_changed(move |c| {
                let name = c.active_id().map(|s| s.to_string()).unwrap_or_default();
                if let Some(wz) = weak.upgrade() {
                    if !name.is_empty() {
                        // Rebuilding inside the handler would destroy this combo.
                        glib::idle_add_local_once(move || wz.add_optional(&name));
                    }
                }
            });
            self.editor_toolbar.append(&combo);
        }
        self.editor_toolbar.set_visible(true);
    }

    fn self_weak(&self) -> Weak<ConnectionWizard> {
        self.weak.clone()
    }

    fn add_optional(&self, name: &str) {
        let result: Result<(), String> = (|| {
            self.apply_pages()?;
            let connection = self.connection().ok_or("The connection settings are invalid.")?;
            forms::add_optional_setting(&connection, name)?;
            self.build_toolbar();
            self.build_pages();
            if let Some(stack) = self.page_stack.borrow().as_ref() {
                stack.set_visible_child_name(name);
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.show_error(&friendly_text(&error, false));
        }
    }

    fn build_pages(&self) {
        remove_children(&self.page_box);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        let sidebar = style(&gtk::StackSidebar::new(), &["editor-sidebar"]);
        sidebar.set_stack(&stack);
        sidebar.set_size_request(165, -1);
        self.page_box.append(&sidebar);
        self.page_box.append(&expand(&stack, &self.page_box));
        self.page_stack.replace(Some(stack.clone()));
        self.pages.borrow_mut().clear();
        let Some(connection) = self.connection() else { return };
        for (id, title, page) in build_pages(&connection) {
            self.add_page(&stack, &id, &title, page);
        }
        if connection.setting("vpn").is_some() {
            match VpnSettingsForm::new(&connection, None) {
                Ok(form) => self.add_page(&stack, "vpn", "VPN", form),
                Err(error) => self.show_error(&error),
            }
        }
        self.page_box.set_visible(true);
    }

    fn add_page(&self, stack: &gtk::Stack, id: &str, title: &str, page: Rc<dyn Page>) {
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_overlay_scrolling(false);
        scroll.set_child(Some(&page.widget()));
        stack.add_titled(&scroll, Some(id), title);
        self.pages.borrow_mut().push((id.to_owned(), page));
    }

    pub fn apply_pages(&self) -> Result<(), String> {
        let pages: Vec<Rc<dyn Page>> = self.pages.borrow().iter().map(|(_, p)| p.clone()).collect();
        for page in pages {
            page.apply()?;
        }
        Ok(())
    }

    pub fn next_or_save(&self) {
        if self.stack.visible_child_name().as_deref() == Some("choose") {
            self.continue_choice();
            return;
        }
        if self.saving.get() {
            return;
        }
        let result: Result<(), String> = (|| {
            self.apply_pages()?;
            let draft = self.draft.borrow().clone().ok_or("The connection settings are invalid.")?;
            if self.activate {
                let device_id = self.device_id.borrow().clone();
                let usable = device_id.as_ref().and_then(|id| self.backend.state().devices.get(id).cloned())
                    .is_some_and(|d| d.kind == "wifi" && d.enabled());
                if !usable {
                    return Err(tr!("Choose an available Wi-Fi adapter."));
                }
                let connection = draft.borrow().connection.clone();
                connection.normalize().map_err(|e| e.message().to_owned())?;
                connection.verify().map_err(|e| e.message().to_owned())?;
                connection.verify_secrets().map_err(|e| e.message().to_owned())?;
                let app = self.gui.upgrade().and_then(|g| g.app()).ok_or("The network service is closing.")?;
                let ap = self.ap_path.clone();
                if app.perform(|backend| Ok(backend.activate_connection(&connection, device_id.as_deref(), ap.as_deref()))) {
                    self.close();
                } else {
                    let message = app.error();
                    self.show_error(&if message.is_empty() { self.backend.error() } else { message });
                }
                return Ok(());
            }
            self.set_saving(true);
            let weak = self.self_weak();
            let saved: crate::profiles::Saved = Rc::new(move |_remote: &Remote| if let Some(wz) = weak.upgrade() { wz.saved() });
            if !self.store.save(&draft, saved) {
                self.set_saving(false);
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.set_saving(false);
            self.show_error(&friendly_text(&error, false));
        }
    }

    fn set_saving(&self, saving: bool) {
        if self.closed.get() {
            return;
        }
        self.saving.set(saving);
        for widget in [self.editor.upcast_ref::<gtk::Widget>(), self.save_button.upcast_ref(), self.back_button.upcast_ref(),
                       self.cancel_button.upcast_ref(), self.export_button.upcast_ref()] {
            widget.set_sensitive(!saving);
        }
        self.save_button.set_label(&if saving { tr!("Saving…") } else { self.action_label.borrow().clone() });
    }

    fn store_changed(&self) {
        if !self.closed.get() && !self.store.busy() && self.saving.get() {
            self.set_saving(false);
        }
    }

    fn show_error(&self, message: &str) {
        if self.closed.get() {
            return;
        }
        self.error_label.set_text(message);
        self.error_label.set_visible(!message.is_empty());
    }

    fn saved(&self) {
        if !self.closed.get() {
            self.backend.refresh();
            self.close();
        }
    }

    #[allow(deprecated)]
    fn file_dialog(&self, title: &str, action: gtk::FileChooserAction, accept: &str, name: Option<String>,
                   done: impl Fn(&ConnectionWizard, String) + 'static) {
        let dialog = gtk::FileChooserDialog::new(Some(title), Some(&self.window), action,
                                                 &[(&tr!("Cancel"), gtk::ResponseType::Cancel), (accept, gtk::ResponseType::Accept)]);
        dialog.set_modal(true);
        dialog.set_destroy_with_parent(true);
        if let Some(name) = name {
            dialog.set_current_name(&name);
        }
        let weak = self.self_weak();
        dialog.connect_response(move |window, response| {
            let path = if response == gtk::ResponseType::Accept { window.file().and_then(|f| f.path()) } else { None };
            window.destroy();
            if let (Some(path), Some(wz)) = (path, weak.upgrade()) {
                done(&wz, path.to_string_lossy().into_owned());
            }
        });
        if let Some(gui) = self.gui.upgrade() {
            gui.prepare_dialog(Some(dialog.upcast_ref()));
        }
    }

    fn import(&self) {
        self.file_dialog(&tr!("Import VPN configuration"), gtk::FileChooserAction::Open, &tr!("Import"), None, |wz, path| {
            match wz.catalog.import_connection(&path, None) {
                Ok(connection) => {
                    if let Some(general) = connection.setting("connection") {
                        general.set_str("uuid", Some(&crate::libnm::uuid_generate()));
                    }
                    wz.set_draft(ProfileDraft::new(connection));
                    wz.show_editor();
                }
                Err(error) => wz.show_error(&friendly_text(&error, false)),
            }
        });
    }

    fn export(&self) {
        if let Err(error) = self.apply_pages() {
            self.show_error(&friendly_text(&error, false));
            return;
        }
        let id = self.connection().map(|c| c.id()).filter(|i| !i.is_empty()).unwrap_or_else(|| "VPN".into());
        self.file_dialog(&tr!("Export VPN configuration"), gtk::FileChooserAction::Save, &tr!("Export"),
                         Some(format!("{}.conf", id.replace('/', "-"))), |wz, path| {
            let Some(connection) = wz.connection() else { return };
            match wz.catalog.export_connection(std::path::Path::new(&path), &connection, true) {
                Ok(_) => wz.note.set_text(&tr!("VPN configuration exported. Passwords were not included.")),
                Err(error) => wz.show_error(&friendly_text(&error, false)),
            }
        });
    }

    fn destroyed(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        self.store.close();
    }

    pub fn set_action_label(&self, label: &str) {
        self.action_label.replace(label.to_owned());
        self.save_button.set_label(label);
    }

    /// A draft can close before it was ever realized: release its pending operations too.
    pub fn close(&self) {
        self.destroyed();
        self.window.destroy();
    }
}

/// GTK checks of test_connection_forms.py / test_vpn_editor.py FormTests,
/// for a private headless display: `(name, passed)` per check.
pub fn headless_checks() -> Vec<(String, bool)> {
    use crate::libnm::Setting;
    use crate::ui::forms::*;
    let mut results: Vec<(String, bool)> = Vec::new();
    let mut check = |name: &str, f: &dyn Fn() -> Result<bool, String>| {
        let ok = f().unwrap_or_else(|e| { eprintln!("{name}: {e}"); false });
        results.push((name.to_owned(), ok));
    };
    check("all_connection_types_construct_and_preserve_unchanged_drafts", &|| {
        for item in connection_types() {
            let draft = new_draft(&item.id, None)?;
            let before = draft.connection.to_dbus(crate::libnm::ALL);
            for (_, _, page) in build_pages(&draft.connection) {
                page.apply()?;
            }
            if draft.connection.to_dbus(crate::libnm::ALL) != before {
                return Err(format!("{} changed", item.id));
            }
        }
        Ok(true)
    });
    #[allow(deprecated)]
    check("wifi_security_is_staged_and_existing_password_is_not_displayed_or_erased", &|| {
        let draft = new_draft("802-11-wireless", None)?;
        let c = &draft.connection;
        let form = WirelessForm::new(c, &c.setting("802-11-wireless").unwrap());
        form.network.field("ssid").unwrap().control.set_text("Example network");
        form.security_combo.set_active_id(Some("wpa-psk"));
        if c.setting("802-11-wireless-security").is_some() {
            return Err("staged too early".into());
        }
        form.security_form.borrow().as_ref().unwrap().field("psk").unwrap().control.set_text("example-test-password");
        form.apply()?;
        let ok1 = c.setting("802-11-wireless-security").and_then(|s| s.get_string("psk")).as_deref() == Some("example-test-password")
            && c.setting("802-11-wireless").unwrap().get_bytes("ssid").as_deref() == Some(b"Example network".as_slice())
            && c.verify().is_ok();
        let reopened = WirelessForm::new(c, &c.setting("802-11-wireless").unwrap());
        let psk = reopened.security_form.borrow().as_ref().unwrap().field("psk").unwrap().control.entry().unwrap();
        let ok2 = psk.text().is_empty() && !gtk::prelude::EntryExt::is_visible(&psk);
        reopened.apply()?;
        let ok3 = c.setting("802-11-wireless-security").and_then(|s| s.get_string("psk")).as_deref() == Some("example-test-password");
        reopened.security_combo.set_active_id(Some("open"));
        reopened.apply()?;
        Ok(ok1 && ok2 && ok3 && c.setting("802-11-wireless-security").is_none())
    });
    #[allow(deprecated)]
    check("manual_ip_addresses_and_routes_validate_before_mutating", &|| {
        let draft = new_draft("802-3-ethernet", None)?;
        let c = &draft.connection;
        let page = IpPage::new(&c.setting("ipv4").unwrap());
        page.method.set_active_id(Some("manual"));
        let row = page.addresses.add_row(Some(&["192.0.2.10".into(), "24".into()]), None, true);
        let route = page.routes.add_row(Some(&["198.51.100.0/24".into(), "192.0.2.1".into(), "20".into()]), None, true);
        page.options.field("dns").unwrap().control.set_text("192.0.2.53, 192.0.2.54");
        page.apply()?;
        let s = c.setting("ipv4").unwrap();
        let ok = s.ip_method().as_deref() == Some("manual") && s.addresses()[0].address() == "192.0.2.10"
            && s.routes()[0].metric() == 20 && s.get_strv("dns").get(1).map(String::as_str) == Some("192.0.2.54") && c.verify().is_ok();
        let before = c.to_dbus(crate::libnm::ALL);
        row.entries[0].set_text("invalid address");
        route.entries[2].set_text("30");
        Ok(ok && page.apply().is_err() && c.to_dbus(crate::libnm::ALL) == before)
    });
    check("wireguard_peer_edits_preserve_unshown_secrets_and_allow_empty_endpoint", &|| {
        let draft = new_draft("wireguard", None)?;
        draft.connection.setting("connection").unwrap().set_str("interface-name", Some("wg-test"));
        let setting = draft.connection.setting("wireguard").unwrap();
        let secret = suite_core::base64::encode(&(32u8..64).collect::<Vec<_>>());
        let peer = crate::libnm::Peer::new();
        peer.set_public_key(&suite_core::base64::encode(&(0u8..32).collect::<Vec<_>>()));
        peer.set_preshared_key(&secret);
        peer.set_endpoint("example.test:51820");
        peer.append_allowed_ip("10.0.0.0/8");
        setting.append_peer(&peer);
        let page = WireGuardPage::new(&setting);
        let editor = page.peers.borrow()[0].clone();
        let hidden = editor.entries["preshared-key"].text().is_empty();
        editor.entries["persistent-keepalive"].set_text("25");
        editor.entries["endpoint"].set_text("");
        page.apply()?;
        let updated = &setting.peers()[0];
        Ok(hidden && updated.endpoint().is_empty() && updated.preshared_key() == Some(secret) && updated.persistent_keepalive() == 25
           && updated.allowed_ips() == ["10.0.0.0/8"])
    });
    #[allow(deprecated)]
    check("ethernet_link_options_use_native_types_and_validate_before_apply", &|| {
        let draft = new_draft("802-3-ethernet", None)?;
        let setting = add_optional_setting(&draft.connection, "ethtool")?;
        let page = EthtoolPage::new(&setting);
        let set = |name: &str, value: &str| match &page.options[name] {
            EthtoolControl::Choice(c) => { c.set_active_id(Some(value)); }
            EthtoolControl::Entry(e) => e.set_text(value),
        };
        set("feature-rx", "false");
        set("ring-rx", "512");
        page.apply()?;
        let ok = setting.option("feature-rx").and_then(|v| v.get::<bool>()) == Some(false)
            && setting.option("ring-rx").and_then(|v| v.get::<u32>()) == Some(512) && setting.verify().is_ok();
        set("feature-rx", "true");
        set("ring-rx", "-1");
        Ok(ok && page.apply().is_err() && setting.option("feature-rx").and_then(|v| v.get::<bool>()) == Some(false))
    });
    #[allow(deprecated)]
    check("enterprise_defaults_to_system_ca_and_skip_requires_explicit_acknowledgment", &|| {
        let draft = new_draft("802-11-wireless", None)?;
        let c = &draft.connection;
        let form = WirelessForm::new(c, &c.setting("802-11-wireless").unwrap());
        form.network.field("ssid").unwrap().control.set_text("Enterprise");
        form.security_combo.set_active_id(Some("wpa-eap"));
        let auth = form.authentication_form.borrow().clone().unwrap();
        let system = auth.ca_mode.active_id().as_deref() == Some("system");
        auth.options.field("identity").unwrap().control.set_text("example");
        form.apply()?;
        let ok1 = c.setting("802-1x").unwrap().get_bool("system-ca-certs");
        auth.ca_mode.set_active_id(Some("skip"));
        let rejected = form.apply().is_err() && c.setting("802-1x").unwrap().get_bool("system-ca-certs");
        auth.skip_ack.set_active(true);
        form.apply()?;
        let ok2 = !c.setting("802-1x").unwrap().get_bool("system-ca-certs");
        auth.ca_mode.set_active_id(Some("custom"));
        Ok(system && ok1 && rejected && ok2 && form.apply().is_err())
    });
    check("unchanged_embedded_ca_is_preserved", &|| {
        let draft = new_draft("802-11-wireless", None)?;
        let c = &draft.connection;
        let security = Setting::new("802-11-wireless-security").unwrap();
        security.set_str("key-mgmt", Some("wpa-eap"));
        c.add_setting(&security);
        let authentication = Setting::new("802-1x").unwrap();
        authentication.set_strv("eap", &["peap".into()]);
        let embedded = b"\x30\x03\x02\x01\x00";
        authentication.set_bytes("ca-cert", Some(embedded));
        c.add_setting(&authentication);
        let form = WirelessForm::new(c, &c.setting("802-11-wireless").unwrap());
        let auth = form.authentication_form.borrow().clone().unwrap();
        let blank = auth.options.field("ca-cert").unwrap().control.entry().unwrap().text().is_empty();
        form.apply()?;
        Ok(blank && authentication.get_bytes("ca-cert").as_deref() == Some(embedded.as_slice()))
    });
    check("vpn_form_preserves_unknown_multiline_data_and_untouched_password", &|| {
        let c = crate::libnm::Connection::new().map_err(|e| e.to_string())?;
        let general = Setting::new("connection").unwrap();
        general.set_str("id", Some("VPN fixture"));
        general.set_str("uuid", Some("11111111-1111-4111-8111-111111111111"));
        general.set_str("type", Some("vpn"));
        c.add_setting(&general);
        let vpn = Setting::new("vpn").unwrap();
        vpn.set_str("service-type", Some("org.freedesktop.NetworkManager.openvpn"));
        vpn.vpn_add_data_item("remote", "vpn.example.invalid");
        vpn.vpn_add_data_item("custom", "one\ntwo\\three  ");
        vpn.vpn_add_secret("password", "fixture-only");
        c.add_setting(&vpn);
        let form = VpnSettingsForm::new(&c, None)?;
        let blank = form.secret_fields.iter().find(|(k, _)| k == "password").is_some_and(|(_, e)| e.text().is_empty());
        if let Some((_, crate::ui::vpn_editor::VpnControl::Entry(e))) = form.fields.iter().find(|(k, _)| k == "remote") {
            e.set_text("changed.example.invalid");
        }
        form.apply()?;
        let after = c.setting("vpn").unwrap();
        let ok = blank && after.vpn_data_item("custom").as_deref() == Some("one\ntwo\\three  ")
            && after.vpn_secret("password").as_deref() == Some("fixture-only");
        form.add_option("remote", "second.example.invalid");
        let duplicate = form.apply().is_err() && c.setting("vpn").unwrap().vpn_data_item("remote").as_deref() == Some("changed.example.invalid");
        Ok(ok && duplicate)
    });
    results
}

