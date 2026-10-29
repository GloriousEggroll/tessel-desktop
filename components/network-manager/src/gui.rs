//! GTK glue for network_manager.py: lazily initialized display, popup and
//! settings ownership, child dialogs (`prepare_dialog` + `wlrctl` raise),
//! connection information windows, editors keyed by uuid/"new", the
//! notification preferences dialog and the agent's credential prompts.
//! `--background` never touches the display until a window is needed.

use crate::app::{App, AppInner, Surfaces, APPLICATION_ID};
use crate::profiles::{new_draft, ProfileDraft};
use crate::ui::info::ConnectionInfo;
use crate::ui::popup::NetworkPopup;
use crate::ui::settings::NetworkSettingsWindow;
use crate::ui::style::{install_style, margins, style};
use crate::ui::wizard::{ConnectionWizard, WizardOptions};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use suite_core::tr;

thread_local!(static GTK_STATE: Cell<u8> = const { Cell::new(0) });

/// Initialize GTK once; false when no display is available.
pub fn ensure_gtk() -> bool {
    GTK_STATE.with(|state| match state.get() {
        1 => true,
        2 => false,
        _ => {
            let ok = gtk::init().is_ok();
            if !ok {
                glib::g_warning!("network-manager", "No display is available for network windows.");
            }
            state.set(if ok { 1 } else { 2 });
            ok
        }
    })
}

/// The display's launch context, when GTK is up (portal sign-in page).
pub fn launch_context() -> Option<gio::AppLaunchContext> {
    if GTK_STATE.with(|s| s.get()) != 1 {
        return None;
    }
    gtk::gdk::Display::default().map(|d| d.app_launch_context().upcast())
}

/// Global Settings' facade (`NetworkPageApplication`): dialogs belong to the
/// settings host window, and "show settings"/"show popup" switch the page's
/// tabs instead of opening windows. The popup is a page and never dismissed.
pub struct Embedding {
    pub host: gtk::Window,
    pub show_settings: Rc<dyn Fn()>,
    pub show_popup: Rc<dyn Fn()>,
}

pub struct Gui {
    embedding: RefCell<Option<Embedding>>,
    app: Weak<AppInner>,
    popup: RefCell<Option<Rc<NetworkPopup>>>,
    position: RefCell<Option<(String, i64)>>,
    settings: RefCell<Option<Rc<NetworkSettingsWindow>>>,
    children: RefCell<Vec<gtk::Window>>,
    infos: RefCell<Vec<Rc<ConnectionInfo>>>,
    editors: RefCell<HashMap<String, Rc<ConnectionWizard>>>,
    css: RefCell<Option<gtk::CssProvider>>,
    weak: RefCell<Weak<Gui>>,
}

impl Gui {
    pub fn new(app: &App) -> Rc<Self> {
        let gui = Rc::new(Self { embedding: RefCell::new(None), app: app.downgrade(), popup: RefCell::new(None), position: RefCell::new(None), settings: RefCell::new(None),
                                 children: RefCell::default(), infos: RefCell::default(), editors: RefCell::default(),
                                 css: RefCell::new(None), weak: RefCell::new(Weak::new()) });
        gui.weak.replace(Rc::downgrade(&gui));
        gui
    }

    /// Switch to the embedded facade behaviour (see [`Embedding`]).
    pub fn set_embedding(&self, embedding: Embedding) {
        self.embedding.replace(Some(embedding));
    }

    pub fn embedded(&self) -> bool {
        self.embedding.borrow().is_some()
    }

    /// The facade's `settings` / `popup`: views created by the embedder.
    pub fn adopt(&self, settings: Option<Rc<NetworkSettingsWindow>>, popup: Option<Rc<NetworkPopup>>) {
        if settings.is_some() {
            self.settings.replace(settings);
        }
        if popup.is_some() {
            self.popup.replace(popup);
        }
    }

    /// `_children`: dialogs prepared by [`Gui::prepare_dialog`].
    pub fn children(&self) -> Vec<gtk::Window> {
        self.children.borrow().clone()
    }

    /// `_editors` (keyed by profile uuid or "new").
    pub fn editors(&self) -> Vec<(String, Rc<ConnectionWizard>)> {
        self.editors.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// The facade's page close: destroy every child dialog, forget editors and
    /// release the shared style (the embedder closes its own views).
    pub fn close_children(&self) {
        let windows: Vec<gtk::Window> = self.children.take();
        for window in windows {
            window.destroy();
        }
        self.editors.borrow_mut().clear();
        self.infos.borrow_mut().clear();
        if let Some(css) = self.css.take() {
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_remove_provider_for_display(&display, &css);
            }
        }
    }

    pub fn weak(&self) -> Weak<Gui> {
        self.weak.borrow().clone()
    }

    pub fn app(&self) -> Option<App> {
        self.app.upgrade().map(App)
    }

    fn stopping(&self) -> bool {
        self.app().is_none_or(|a| a.0.stopping.get())
    }

    pub fn ensure_style(&self, window: &impl IsA<gtk::Widget>) {
        style(window, &["networkmanager"]);
        if self.css.borrow().is_none() {
            self.css.replace(Some(install_style(window)));
        }
    }

    pub fn popup_destroyed(&self, popup: &NetworkPopup) {
        let current = self.popup.borrow().as_ref().is_some_and(|p| std::ptr::eq(p.as_ref(), popup));
        if current {
            self.popup.replace(None);
        }
    }

    pub fn settings_destroyed(&self) {
        self.settings.replace(None);
    }

    /// `prepare_dialog(window)`: dismiss the popup; `None` only dismisses.
    pub fn prepare_dialog(&self, window: Option<&gtk::Window>) {
        let host = self.embedding.borrow().as_ref().map(|e| e.host.clone());
        if let Some(host) = host {
            return self.prepare_embedded_dialog(window, &host);
        }
        self.dismiss_popup();
        let Some(window) = window else { return };
        self.ensure_style(window);
        if let Some(settings) = self.settings.borrow().as_ref() {
            if settings.window.get_visible() && window.transient_for().is_none() && window != &settings.window {
                window.set_transient_for(Some(&settings.window));
            }
        }
        let known = self.children.borrow().iter().any(|w| w == window);
        if !known {
            self.children.borrow_mut().push(window.clone());
            let weak = self.weak();
            window.connect_destroy(move |w| {
                if let Some(gui) = weak.upgrade() {
                    gui.children.borrow_mut().retain(|c| c != w);
                    gui.infos.borrow_mut().retain(|i| &i.window != w);
                    gui.editors.borrow_mut().retain(|_, e| &e.window != w);
                }
            });
        }
        window.set_visible(true);
        self.present(window);
    }

    /// `NetworkPageApplication.prepare_dialog`: the dialog belongs to the host.
    fn prepare_embedded_dialog(&self, window: Option<&gtk::Window>, host: &gtk::Window) {
        let Some(window) = window else { return };
        self.ensure_style(window);
        if window.application().is_none() {
            if let Some(application) = self.app().and_then(|a| a.0.gapp.clone().downcast::<gtk::Application>().ok()) {
                window.set_application(Some(&application));
            }
        }
        let settings = self.settings.borrow().as_ref().map(|s| s.window.clone());
        let parent = window.transient_for();
        if parent.is_none() || parent == settings {
            window.set_transient_for(Some(host));
        }
        window.set_destroy_with_parent(true);
        let known = self.children.borrow().iter().any(|w| w == window);
        if !known {
            self.children.borrow_mut().push(window.clone());
            let weak = self.weak();
            window.connect_destroy(move |w| {
                if let Some(gui) = weak.upgrade() {
                    gui.children.borrow_mut().retain(|c| c != w);
                    gui.infos.borrow_mut().retain(|i| &i.window != w);
                    gui.editors.borrow_mut().retain(|_, e| &e.window != w);
                }
            });
        }
        window.unminimize();
        window.present();
    }

    pub fn present(&self, window: &gtk::Window) {
        if self.embedded() {
            window.present();
            return;
        }
        window.unminimize();
        window.present();
        let weak = self.weak();
        let target = window.downgrade();
        glib::idle_add_local_once(move || {
            if let (Some(gui), Some(window)) = (weak.upgrade(), target.upgrade()) {
                if !gui.stopping() {
                    let weak = gui.weak();
                    suite_gtk::utils::raise_window(APPLICATION_ID, &window, move |_| weak.upgrade().is_some_and(|g| !g.stopping()));
                }
            }
        });
    }

    pub fn show_info(&self, profile: Option<&str>, active: Option<&str>, device: Option<&str>) {
        let Some(app) = self.app() else { return };
        if !ensure_gtk() {
            return;
        }
        let ids = (profile.map(String::from), active.map(String::from), device.map(String::from));
        let existing = self.infos.borrow().iter().find(|i| i.identifiers == ids).cloned();
        let info = existing.unwrap_or_else(|| {
            let info = ConnectionInfo::new(&app.backend(), ids.clone());
            self.infos.borrow_mut().push(info.clone());
            info
        });
        self.prepare_dialog(Some(&info.window));
    }

    pub fn launch_editor(&self, uuid: Option<&str>, connection_type: Option<&str>) {
        let Some(app) = self.app() else { return };
        self.dismiss_popup();
        if !ensure_gtk() {
            return;
        }
        let backend = app.backend();
        if let Some(uuid) = uuid {
            let profile = backend.state().profiles.get(uuid).cloned();
            match profile {
                None => return app.report_error(&tr!("That connection is no longer available.")),
                Some(p) if p.external => return app.report_error(&tr!("Edit this externally managed connection in the application that owns it.")),
                _ => {}
            }
        }
        let key = uuid.unwrap_or("new").to_owned();
        let existing = self.editors.borrow().get(&key).cloned();
        if let Some(existing) = existing {
            self.prepare_dialog(Some(&existing.window));
            return;
        }
        let remote = match uuid {
            Some(uuid) => match backend.remote(uuid) {
                Some(remote) => Some(remote),
                None => return app.report_error(&tr!("That connection is no longer available.")),
            },
            None => None,
        };
        let options = WizardOptions { remote, connection_type: connection_type.map(String::from), ..Default::default() };
        match ConnectionWizard::new(self, options) {
            Ok(window) => {
                self.editors.borrow_mut().insert(key, window.clone());
                self.prepare_dialog(Some(&window.window));
            }
            Err(message) => app.report_error(&crate::model::friendly_text(&message, false)),
        }
    }

    pub fn show_preferences(&self) {
        let Some(app) = self.app() else { return };
        if !ensure_gtk() {
            return;
        }
        let title = tr!("Network Notifications");
        let existing = self.children.borrow().iter().find(|w| w.title().as_deref() == Some(title.as_str())).cloned();
        if let Some(existing) = existing {
            self.prepare_dialog(Some(&existing));
            return;
        }
        #[allow(deprecated)]
        let dialog = gtk::Dialog::new();
        dialog.set_title(Some(&title));
        dialog.set_default_size(410, -1);
        #[allow(deprecated)]
        {
            dialog.add_button(&tr!("Close"), gtk::ResponseType::Close);
            let content = dialog.content_area();
            margins(&content, 20);
            content.set_spacing(14);
            for (event, title) in [("connected", tr!("Network connected")), ("disconnected", tr!("Network disconnected")),
                                   ("vpn", "VPN connections".to_owned()), ("portal", tr!("Network sign-in required"))] {
                let control = gtk::CheckButton::with_label(&title);
                control.set_active(app.notifications_enabled(event));
                control.set_sensitive(app.0.preferences.borrow().is_some());
                let weak = app.downgrade();
                control.connect_toggled(move |button| {
                    if let Some(app) = weak.upgrade() {
                        if let Some(preferences) = app.preferences.borrow().as_ref() {
                            let _ = preferences.set_boolean(&format!("disable-{event}-notifications"), !button.is_active());
                        }
                    }
                });
                content.append(&control);
            }
            dialog.connect_response(|window, _| window.destroy());
        }
        self.prepare_dialog(Some(dialog.upcast_ref()));
    }

    /// `show_wifi_dialog(mode, network_id)`: hidden / hotspot / one network.
    pub fn wifi_dialog(&self, mode: &str, network_id: Option<&str>) {
        let Some(app) = self.app() else { return };
        let backend = app.backend();
        let (ready, busy, networking, wireless, hardware) = {
            let s = backend.state();
            (s.ready, !s.busy.is_empty(), s.networking_enabled, s.wireless_enabled, s.wireless_hardware_enabled)
        };
        if !ready || busy {
            return;
        }
        if !networking || !wireless || !hardware {
            return app.report_error(&tr!("Enable networking and Wi-Fi before configuring a wireless connection."));
        }
        self.dismiss_popup();
        if !ensure_gtk() {
            return;
        }
        let result: Result<(), String> = (|| {
            let (mut device_id, mut ap_path) = (None, None);
            let draft = if mode == "hidden" || mode == "hotspot" {
                let draft = new_draft("802-11-wireless", None)?;
                let wireless = draft.connection.setting("802-11-wireless").ok_or("The connection settings are invalid.")?;
                wireless.set_bool("hidden", mode == "hidden");
                if mode == "hotspot" {
                    wireless.set_str("mode", Some("ap"));
                    draft.connection.setting("ipv4").unwrap().set_str("method", Some("shared"));
                    draft.connection.setting("ipv6").unwrap().set_str("method", Some("disabled"));
                    let security = crate::libnm::Setting::new("802-11-wireless-security").ok_or("The connection settings are invalid.")?;
                    security.set_str("key-mgmt", Some("wpa-psk"));
                    draft.connection.add_setting(&security);
                }
                draft
            } else {
                let id = network_id.unwrap_or_default();
                if !backend.state().networks.contains_key(id) {
                    return Err(tr!("That wireless network is no longer available."));
                }
                let Some(connection) = backend.build_wifi_connection(id) else { return Ok(()) };
                let (device, ap) = backend.get_wifi_objects(id);
                if device.is_none() || ap.is_none() {
                    return Err(tr!("That wireless adapter or access point is no longer available."));
                }
                device_id = device;
                ap_path = ap;
                ProfileDraft::new(connection)
            };
            let options = WizardOptions { draft: Some(draft), activate: true, device_id, ap_path, ..Default::default() };
            let wizard = ConnectionWizard::new(self, options)?;
            let title = match mode {
                "hotspot" => tr!("Create Wi-Fi Network"),
                "hidden" => tr!("Hidden Wi-Fi Network"),
                _ => tr!("Connect to Wi-Fi"),
            };
            wizard.window.set_title(Some(&title));
            if let Some(header) = wizard.window.titlebar().and_then(|t| t.downcast::<gtk::HeaderBar>().ok()) {
                header.set_title_widget(Some(&gtk::Label::new(Some(&title))));
            }
            if mode == "hidden" || mode == "hotspot" {
                wizard.title.set_text(&if mode == "hotspot" { tr!("Create a Wi-Fi network") } else { tr!("Join a hidden network") });
                wizard.subtitle.set_text(&if mode == "hotspot" { tr!("Choose a network name, password, and adapter.") }
                                          else { tr!("Enter the hidden network's name and security settings.") });
            }
            if mode == "hotspot" {
                wizard.set_action_label(&tr!("Create network"));
            }
            self.prepare_dialog(Some(&wizard.window));
            Ok(())
        })();
        if let Err(message) = result {
            app.report_error(&crate::model::friendly_text(&message, false));
        }
    }
}

impl Surfaces for Gui {
    fn refresh(&self) {
        let popup = self.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.refresh();
        }
        let settings = self.settings.borrow().clone();
        if let Some(settings) = settings {
            settings.refresh();
        }
        let infos = self.infos.borrow().clone();
        for info in infos {
            info.refresh();
        }
    }

    fn set_settings_error(&self, message: &str) {
        let settings = self.settings.borrow().clone();
        if let Some(settings) = settings {
            settings.set_error(message);
        }
    }

    fn show_popup(&self, edge: &str, offset: i64) {
        let hook = self.embedding.borrow().as_ref().map(|e| e.show_popup.clone());
        if let Some(hook) = hook {
            return hook();
        }
        let Some(app) = self.app() else { return };
        if !ensure_gtk() {
            return;
        }
        let position = (edge.to_owned(), offset);
        let current = self.popup.borrow().clone();
        if let Some(popup) = &current {
            if self.position.borrow().as_ref() != Some(&position) {
                popup.destroy();
                self.popup.replace(None);
            }
        }
        let current = self.popup.borrow().clone();
        match current {
            Some(popup) => popup.activate(),
            None => {
                let popup = NetworkPopup::new(self, &app, edge, offset as i32, false);
                self.popup.replace(Some(popup.clone()));
                self.position.replace(Some(position));
                popup.show();
            }
        }
    }

    fn show_settings(&self) {
        let hook = self.embedding.borrow().as_ref().map(|e| e.show_settings.clone());
        if let Some(hook) = hook {
            return hook();
        }
        let Some(app) = self.app() else { return };
        self.dismiss_popup();
        if !ensure_gtk() {
            return;
        }
        let existing = self.settings.borrow().clone();
        let settings = existing.unwrap_or_else(|| {
            let window = NetworkSettingsWindow::new(self, &app.backend());
            self.settings.replace(Some(window.clone()));
            window
        });
        self.ensure_style(&settings.window);
        settings.window.set_visible(true);
        settings.set_error(&app.error());
        settings.refresh();
        self.present(&settings.window);
    }

    fn dismiss_popup(&self) {
        if self.embedded() {
            // The connections overview is a page, not a dismissible surface.
            return;
        }
        let popup = self.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.dismiss();
        }
    }

    fn show_wifi_dialog(&self, mode: &str, network_id: Option<&str>) {
        self.wifi_dialog(mode, network_id);
    }

    fn close(&self) {
        let popup = self.popup.take();
        if let Some(popup) = popup {
            popup.destroy();
        }
        let windows: Vec<gtk::Window> = self.children.borrow().clone();
        for window in windows {
            window.destroy();
        }
        let editors: Vec<Rc<ConnectionWizard>> = self.editors.borrow_mut().drain().map(|(_, e)| e).collect();
        for editor in editors {
            editor.close();
        }
        if let Some(settings) = self.settings.take() {
            settings.window.destroy();
        }
    }
}
