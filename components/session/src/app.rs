//! The `org.tessel.Session` GtkApplication, the session popup and its
//! centered `--menu` placement (session_manager.py).

use crate::args::{self, Outcome};
use crate::backend::{Backend, SessionBackend};
use crate::end::{Ending, Inhibitor};
use crate::windows::Window;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4_layer_shell::{Edge, KeyboardMode, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::placement::PointerMonitor;
use tessel_gtk::popup::PopupController;
use tessel_gtk::utils::{add_css, image, PRIORITY_APPLICATION};

pub const APPLICATION_ID: &str = "org.tessel.Session";

/// (key, title, icon, detail)
pub const ACTIONS: [(&str, &str, &str, &str); 6] = [
    ("lock", "Lock Screen", "changes-prevent-symbolic", "Keep your applications open and require authentication."),
    ("switch_user", "Switch User", "system-users-symbolic", "Lock this session and sign in as another user."),
    ("logout", "Log Out", "system-log-out-symbolic", "End this desktop session."),
    ("sleep", "Sleep", "system-suspend-symbolic", "Lock the screen and suspend the PC."),
    ("restart", "Restart", "system-reboot-symbolic", "Restart the PC."),
    ("poweroff", "Power Off", "system-shutdown-symbolic", "Shut down the PC."),
];

/// (action, button title, question)
pub const CONFIRMATIONS: [(&str, &str, &str); 3] = [
    ("logout", "Log Out", "Log out of this session? Save your work before continuing."),
    ("restart", "Restart", "Restart the PC? Save your work before continuing."),
    ("poweroff", "Power Off", "Power off the PC? Save your work before continuing."),
];

/// Continue button when windows remain after the close requests.
pub const REMAINING: [(&str, &str); 3] = [
    ("logout", "Log out anyway"),
    ("restart", "Restart anyway"),
    ("poweroff", "Power off anyway"),
];

/// The System Information row (like "About This Mac"), above the session actions.
pub const SYSTEM_INFO: (&str, &str, &str) = ("System Information", "computer-symbolic", "See this PC's hardware and software.");

/// argv opening Settings at System Information. Settings runs in its own user
/// unit: this popup's unit stops when the popup closes and would take a direct
/// child with it.
pub fn system_info_command() -> Vec<String> {
    let program = |name: &str| tessel_core::paths::program(name).to_string_lossy().into_owned();
    vec![program("systemd-run"), "--user".into(), "--collect".into(), "--quiet".into(), "--property=Type=exec".into(),
         program("tessel-settings"), "--section".into(), "systeminfo".into()]
}

/// One popup row: icon, title and a muted detail line.
fn action_button(title: &str, icon: &str, detail: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("session-action");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let icon_image = image(icon, 16);
    icon_image.set_valign(gtk::Align::Center);
    row.append(&icon_image);
    let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(3).hexpand(true).build();
    let name = gtk::Label::builder().label(tr!(title)).xalign(0.0).build();
    name.add_css_class("session-title");
    words.append(&name);
    let description = gtk::Label::builder().label(tr!(detail)).xalign(0.0).wrap(true).max_width_chars(34).build();
    description.add_css_class("subtitle");
    words.append(&description);
    row.append(&words);
    button.set_child(Some(&row));
    button
}

pub const CSS: &str = "
window.tessel-session.tray-popup { background: transparent; }
.tessel-session .popup-surface { background: @theme_bg_color; border-radius: 8px; border: 1px solid alpha(@theme_fg_color,.14); }
.tessel-session .title { font-size: 16pt; font-weight: 600; }
.tessel-session .subtitle { color: alpha(@theme_fg_color,.66); font-size: 9pt; }
.tessel-session .session-title { font-size: 10.5pt; font-weight: 600; }
.tessel-session .session-action { padding: 10px 12px; border-radius: 8px; background: transparent; border: none; box-shadow: none; }
.tessel-session .session-action:hover { background: alpha(@theme_fg_color,.08); }
.tessel-session .session-blocker { padding: 6px 4px; }
.tessel-session .session-separator { margin: 2px 12px; background: alpha(@theme_fg_color,.10); }
.tessel-session .session-tile { min-width: 40px; min-height: 40px; border-radius: 8px; background: alpha(@theme_selected_bg_color,.13); color: @theme_selected_bg_color; }
";

fn confirmation(action: &str) -> Option<(&'static str, &'static str)> {
    CONFIRMATIONS.iter().find(|(key, _, _)| *key == action).map(|(_, title, question)| (*title, *question))
}

fn remaining_label(action: &str) -> Option<&'static str> {
    REMAINING.iter().find(|(key, _)| *key == action).map(|(_, label)| *label)
}

/// Installed application icon for a Wayland app_id, or a generic icon.
pub fn window_icon(app_id: &str) -> gio::Icon {
    if !app_id.is_empty() {
        let last = app_id.rsplit('.').next().unwrap_or(app_id).to_lowercase();
        for name in [app_id.to_owned(), app_id.to_lowercase(), last] {
            if let Some(icon) = tessel_core::glib_compat::desktop_app_info(&format!("{name}.desktop")).and_then(|info| info.icon()) {
                return icon;
            }
        }
    }
    gio::ThemedIcon::new("application-x-executable-symbolic").upcast()
}

// ---------------------------------------------------------------------------
// Centered --menu placement (MenuPopupController)

struct MenuInner {
    base: PopupController,
    window: gtk::Window,
    apply_state: Box<dyn Fn()>,
    edge: RefCell<String>,
    offset: Cell<i32>,
    generation: Cell<u64>,
    pending: Cell<bool>,
    probe: RefCell<Option<PointerMonitor>>,
    backdrops: RefCell<Vec<(gdk::Monitor, gtk::Window)>>,
    backdrop_css: RefCell<Option<gtk::CssProvider>>,
    layer_shell: bool,
}

/// Keyboard shortcuts have no dock button: no anchors (layer-shell centers
/// the surface on the pointer's output) and EXCLUSIVE keyboard. Dismissal
/// (Escape, close request, click-away, monitor change) is the shared
/// [`PopupController`]'s.
#[derive(Clone)]
pub struct MenuController(Rc<MenuInner>);

impl MenuController {
    fn new(window: &gtk::Window, apply_state: impl Fn() + 'static, edge: &str, offset: i32, group: &str) -> Self {
        let base = PopupController::new(window, || {}, edge, offset, group);
        let this = Self(Rc::new(MenuInner {
            base, window: window.clone(), apply_state: Box::new(apply_state), edge: RefCell::new(edge.into()),
            offset: Cell::new(offset), generation: Cell::new(0), pending: Cell::new(false), probe: RefCell::new(None),
            backdrops: RefCell::default(), backdrop_css: RefCell::new(None), layer_shell: gtk4_layer_shell::is_supported(),
        }));
        if this.0.layer_shell {
            this.position_layer();
        }
        let weak = Rc::downgrade(&this.0);
        window.connect_visible_notify(move |window| {
            if !window.get_visible() {
                if let Some(inner) = weak.upgrade() {
                    for (_, backdrop) in inner.backdrops.borrow().iter() {
                        backdrop.set_visible(false);
                    }
                }
            }
        });
        this
    }

    pub fn base(&self) -> &PopupController { &self.0.base }

    fn position_layer(&self) {
        let window = &self.0.window;
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, false);
            window.set_margin(edge, 0);
        }
        window.set_keyboard_mode(KeyboardMode::Exclusive);
    }

    fn show(&self) {
        self.0.base.begin_show();
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        self.0.pending.set(true);
        if let Some(probe) = self.0.probe.take() {
            probe.cancel();
        }
        let weak = Rc::downgrade(&self.0);
        let probe = PointerMonitor::new(move |monitor| {
            let Some(inner) = weak.upgrade() else { return };
            let this = MenuController(inner);
            this.0.probe.replace(None);
            // dismiss()/a newer show() makes this probe stale.
            if generation != this.0.generation.get() || !this.0.base.show_pending() {
                return;
            }
            if this.0.layer_shell {
                if monitor.is_some() && this.0.window.monitor() != monitor {
                    this.0.window.set_visible(false);
                    this.0.window.set_monitor(monitor.as_ref());
                }
                this.position_layer();
            }
            this.show_now();
        });
        self.0.probe.replace(Some(probe));
    }

    fn ensure_backdrops(&self) {
        if !self.0.layer_shell {
            return;
        }
        let monitors = WidgetExt::display(&self.0.window).monitors();
        let current: Vec<gdk::Monitor> = (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast()).collect();
        self.0.backdrops.borrow_mut().retain(|(monitor, backdrop)| {
            let keep = current.contains(monitor);
            if !keep {
                backdrop.destroy();
            }
            keep
        });
        if self.0.backdrop_css.borrow().is_none() {
            let provider = add_css("window.desktop-applet-backdrop { background: transparent; border: none; box-shadow: none; }",
                                   Some(&WidgetExt::display(&self.0.window)), PRIORITY_APPLICATION + 40);
            self.0.backdrop_css.replace(Some(provider));
        }
        for monitor in current {
            if self.0.backdrops.borrow().iter().any(|(known, _)| *known == monitor) {
                continue;
            }
            let backdrop = gtk::Window::builder().decorated(false).resizable(true).build();
            backdrop.add_css_class("desktop-applet-backdrop");
            backdrop.init_layer_shell();
            backdrop.set_namespace(Some("desktop-applet-dismiss"));
            backdrop.set_layer(gtk4_layer_shell::Layer::Overlay);
            backdrop.set_exclusive_zone(-1);
            backdrop.set_monitor(Some(&monitor));
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                backdrop.set_anchor(edge, true);
            }
            backdrop.set_keyboard_mode(KeyboardMode::None);
            let area = monitor.geometry();
            backdrop.set_default_size(area.width(), area.height());
            let catcher = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
            catcher.set_draw_func(|_, cr, _, _| {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
                let _ = cr.paint();
            });
            backdrop.set_child(Some(&catcher));
            let click = gtk::GestureClick::new();
            click.set_button(0);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(&self.0);
            click.connect_released(move |_, _, _, _| if let Some(inner) = weak.upgrade() { inner.base.dismiss() });
            backdrop.add_controller(click);
            self.0.backdrops.borrow_mut().push((monitor, backdrop));
        }
    }

    fn show_now(&self) {
        self.0.pending.set(false);
        self.ensure_backdrops();
        for (_, backdrop) in self.0.backdrops.borrow().iter() {
            backdrop.set_visible(true);
        }
        (self.0.apply_state)();
        self.0.window.present();
    }

    fn dismiss(&self) {
        self.0.pending.set(false);
        self.0.generation.set(self.0.generation.get() + 1);
        if let Some(probe) = self.0.probe.take() {
            probe.cancel();
        }
        self.0.base.dismiss();
    }

    fn set_position(&self, edge: &str, offset: i32) {
        if *self.0.edge.borrow() == edge && self.0.offset.get() == offset {
            return;
        }
        self.dismiss();
        self.0.edge.replace(edge.into());
        self.0.offset.set(offset);
        if self.0.layer_shell {
            self.position_layer();
        }
    }

    fn activate(&self) {
        if self.0.window.get_visible() { self.dismiss() } else { self.show() }
    }

    fn close(&self) {
        self.dismiss();
        self.0.base.close();
        for (_, backdrop) in self.0.backdrops.take() {
            backdrop.destroy();
        }
        if let Some(provider) = self.0.backdrop_css.take() {
            tessel_gtk::utils::remove_css(&WidgetExt::display(&self.0.window), &provider);
        }
    }
}

/// Edge (dock button) or centered (`--menu`) placement.
#[derive(Clone)]
pub enum Controller {
    Edge(PopupController),
    Menu(MenuController),
}

impl Controller {
    pub fn show(&self) {
        match self { Self::Edge(c) => c.show(), Self::Menu(c) => c.show() }
    }
    pub fn activate(&self) {
        match self { Self::Edge(c) => c.activate(), Self::Menu(c) => c.activate() }
    }
    pub fn dismiss(&self) {
        match self { Self::Edge(c) => c.dismiss(), Self::Menu(c) => c.dismiss() }
    }
    pub fn set_position(&self, edge: &str, offset: i32) {
        match self { Self::Edge(c) => c.set_position(edge, offset), Self::Menu(c) => c.set_position(edge, offset) }
    }
    pub fn show_pending(&self) -> bool {
        match self { Self::Edge(c) => c.show_pending(), Self::Menu(c) => c.0.pending.get() && c.0.base.show_pending() }
    }
    pub fn edge(&self) -> String {
        match self { Self::Edge(c) => c.edge(), Self::Menu(c) => c.0.edge.borrow().clone() }
    }
    pub fn offset(&self) -> i32 {
        match self { Self::Edge(c) => c.offset(), Self::Menu(c) => c.0.offset.get() }
    }
    pub fn is_menu(&self) -> bool { matches!(self, Self::Menu(_)) }
    pub fn close(&self) {
        match self { Self::Edge(c) => c.close(), Self::Menu(c) => c.close() }
    }
}

// ---------------------------------------------------------------------------
// The popup

pub struct SessionPopup {
    pub menu: bool,
    pub pending: RefCell<Option<String>>,
    pub window: gtk::ApplicationWindow,
    pub system_info: gtk::Button,
    pub buttons: Vec<(String, gtk::Button)>,
    pub actions: gtk::Box,
    pub confirmation: gtk::Box,
    pub question: gtk::Label,
    pub confirm_button: gtk::Button,
    pub remaining: gtk::Box,
    pub remaining_title: gtk::Label,
    pub remaining_detail: gtk::Label,
    pub remaining_list: gtk::Box,
    pub remaining_cancel: gtk::Button,
    pub anyway_button: gtk::Button,
    pub status: gtk::Label,
    controller: RefCell<Option<Controller>>,
    app: Weak<AppInner>,
}

impl SessionPopup {
    fn new(app: &SessionApplication, edge: &str, offset: i32, menu: bool) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(&app.0.app).title(tr!("Session")).build();
        window.set_default_size(380, -1);
        window.add_css_class("tessel-session");
        // Above the palette stylesheet (APPLICATION + 30) so the chrome rules win.
        let style = add_css(CSS, Some(&WidgetExt::display(&window)), PRIORITY_APPLICATION + 31);
        window.connect_unrealize(move |window| tessel_gtk::utils::remove_css(&WidgetExt::display(window), &style));
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("popup-surface");
        window.set_child(Some(&surface));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .margin_top(16).margin_bottom(16).margin_start(16).margin_end(16).build();
        surface.append(&content);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let tile = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        tile.add_css_class("session-tile");
        let glyph = image("system-shutdown-symbolic", 24);
        // A homogeneous box gives its child the whole 40px tile to center in;
        // expanding the glyph instead would stretch the tile across the header.
        tile.set_homogeneous(true);
        glyph.set_halign(gtk::Align::Center);
        glyph.set_valign(gtk::Align::Center);
        tile.append(&glyph);
        header.append(&tile);
        let title = gtk::Label::builder().label(tr!("Session")).xalign(0.0).build();
        title.add_css_class("title");
        header.append(&title);
        content.append(&header);

        let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.append(&actions);
        let (title, icon, detail) = SYSTEM_INFO;
        let system_info = action_button(title, icon, detail);
        actions.append(&system_info);
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        separator.add_css_class("session-separator");
        actions.append(&separator);
        let mut buttons = Vec::new();
        for (key, title, icon, detail) in ACTIONS {
            let button = action_button(title, icon, detail);
            actions.append(&button);
            buttons.push((key.to_owned(), button));
        }

        let confirmation = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).visible(false).build();
        let question = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(34).build();
        confirmation.append(&question);
        let row = gtk::Box::builder().spacing(8).homogeneous(true).build();
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        row.append(&cancel);
        let confirm_button = gtk::Button::new();
        confirm_button.add_css_class("destructive-action");
        row.append(&confirm_button);
        confirmation.append(&row);
        content.append(&confirmation);

        let remaining = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).visible(false).build();
        let remaining_title = gtk::Label::builder().label(tr!("These apps are still open")).xalign(0.0).wrap(true)
            .max_width_chars(34).build();
        remaining_title.add_css_class("session-title");
        remaining.append(&remaining_title);
        let remaining_detail = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(36).build();
        remaining_detail.add_css_class("subtitle");
        remaining.append(&remaining_detail);
        let remaining_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let scroller = gtk::ScrolledWindow::builder().propagate_natural_height(true).max_content_height(320).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&remaining_list));
        remaining.append(&scroller);
        let row = gtk::Box::builder().spacing(8).homogeneous(true).build();
        let remaining_cancel = gtk::Button::with_label(&tr!("Cancel"));
        row.append(&remaining_cancel);
        let anyway_button = gtk::Button::new();
        anyway_button.add_css_class("destructive-action");
        row.append(&anyway_button);
        remaining.append(&row);
        content.append(&remaining);

        let status = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(36).visible(false).build();
        status.add_css_class("subtitle");
        // An empty status line would add a spacing gap above the bottom margin.
        status.connect_label_notify(|label| label.set_visible(!label.label().is_empty()));
        content.append(&status);

        let popup = Rc::new(Self {
            menu, pending: RefCell::new(None), window: window.clone(), system_info, buttons, actions, confirmation, question,
            confirm_button, remaining, remaining_title, remaining_detail, remaining_list, remaining_cancel,
            anyway_button, status, controller: RefCell::new(None), app: Rc::downgrade(&app.0),
        });
        for (key, button) in &popup.buttons {
            let weak = Rc::downgrade(&popup);
            let key = key.clone();
            button.connect_clicked(move |_| if let Some(popup) = weak.upgrade() { popup.choose(&key) });
        }
        let weak = Rc::downgrade(&popup);
        popup.system_info.connect_clicked(move |_| if let Some(popup) = weak.upgrade() { popup.open_system_info() });
        let weak = Rc::downgrade(&popup);
        cancel.connect_clicked(move |_| if let Some(popup) = weak.upgrade() { popup.cancel() });
        let weak = Rc::downgrade(&popup);
        popup.confirm_button.connect_clicked(move |_| if let Some(popup) = weak.upgrade() { popup.confirm() });
        let weak = Rc::downgrade(&popup.app.upgrade().unwrap());
        popup.remaining_cancel.connect_clicked(move |_| if let Some(app) = weak.upgrade() { SessionApplication(app).cancel_end(true) });
        let weak = Rc::downgrade(&popup.app.upgrade().unwrap());
        popup.anyway_button.connect_clicked(move |_| if let Some(app) = weak.upgrade() { SessionApplication(app).continue_end() });

        let weak = Rc::downgrade(&popup);
        let apply = move || if let Some(popup) = weak.upgrade() { popup.apply_state() };
        let gwindow = window.upcast_ref::<gtk::Window>();
        let controller = if menu {
            Controller::Menu(MenuController::new(gwindow, apply, edge, offset, "session"))
        } else {
            Controller::Edge(PopupController::new(gwindow, apply, edge, offset, "session"))
        };
        popup.controller.replace(Some(controller));
        let app_weak = popup.app.clone();
        window.connect_visible_notify(move |window| {
            if !window.get_visible() {
                let app_weak = app_weak.clone();
                let window = window.clone();
                glib::idle_add_local_once(move || {
                    if let Some(app) = app_weak.upgrade() {
                        SessionApplication(app).hidden(window.upcast_ref());
                    }
                });
            }
        });
        popup
    }

    pub fn controller(&self) -> Controller { self.controller.borrow().clone().expect("controller") }

    pub fn button(&self, action: &str) -> gtk::Button {
        self.buttons.iter().find(|(key, _)| key == action).map(|(_, b)| b.clone()).expect("action button")
    }

    fn app(&self) -> Option<SessionApplication> { self.app.upgrade().map(SessionApplication) }

    pub fn apply_state(&self) {
        let Some(app) = self.app() else { return };
        let Some(backend) = app.backend() else { return };
        let capabilities = backend.capabilities();
        for action in ["sleep", "restart", "poweroff"] {
            self.button(action).set_sensitive(capabilities.get(action));
        }
        self.button("switch_user").set_visible(capabilities.switch_user);
        self.button("logout").set_sensitive(backend.can_logout());
        let error = backend.error();
        if !error.is_empty() && self.status.text().is_empty() {
            self.status.set_text(&error);
        }
    }

    pub fn choose(&self, action: &str) {
        let Some(app) = self.app() else { return };
        if app.busy() {
            return;
        }
        if let Some((title, question)) = confirmation(action) {
            self.pending.replace(Some(action.into()));
            self.actions.set_visible(false);
            self.confirmation.set_visible(true);
            self.question.set_text(&tr!(question));
            self.confirm_button.set_label(&tr!(title));
        } else {
            app.perform(action);
        }
    }

    /// Open Settings at System Information and close the popup.
    pub fn open_system_info(&self) {
        let Some(app) = self.app() else { return };
        if app.busy() {
            return;
        }
        let argv = system_info_command();
        let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
        match gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
            Ok(_) => self.controller().dismiss(),
            Err(error) => self.status.set_text(&format!("{}: {}", tr!("System Information could not be opened"), error.message())),
        }
    }

    pub fn cancel(&self) {
        self.pending.replace(None);
        self.confirmation.set_visible(false);
        self.remaining.set_visible(false);
        self.actions.set_visible(true);
    }

    /// List the windows and logind inhibitors still holding the session.
    pub fn show_remaining(&self, action: &str, windows: &[Window], inhibitors: &[Inhibitor], note: &str) {
        self.actions.set_visible(false);
        self.confirmation.set_visible(false);
        self.remaining.set_visible(true);
        self.status.set_text("");
        self.anyway_button.set_label(&tr!(remaining_label(action).unwrap_or("")));
        self.remaining_title.set_text(&if !windows.is_empty() || !note.is_empty() {
            tr!("These apps are still open")
        } else {
            tr!("Some apps are delaying this action")
        });
        let mut details = Vec::new();
        if !note.is_empty() {
            details.push(tr!(note));
        }
        if !windows.is_empty() {
            details.push(tr!("Save your work in these apps, or continue and lose unsaved changes."));
        }
        self.remaining_detail.set_text(&details.join(" "));
        self.remaining_detail.set_visible(!details.is_empty());
        while let Some(child) = self.remaining_list.first_child() {
            self.remaining_list.remove(&child);
        }
        for window in windows {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("session-blocker");
            let icon = gtk::Image::from_gicon(&window_icon(&window.app_id));
            icon.set_pixel_size(28);
            row.append(&icon);
            let name = if !window.title.is_empty() {
                window.title.clone()
            } else if !window.app_id.is_empty() {
                window.app_id.clone()
            } else {
                tr!("Untitled window")
            };
            let label = gtk::Label::builder().label(&name).xalign(0.0).hexpand(true).ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(34).build();
            row.append(&label);
            row.update_property(&[gtk::accessible::Property::Label(&name)]);
            self.remaining_list.append(&row);
        }
        for item in inhibitors {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("session-blocker");
            let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
            icon.set_pixel_size(28);
            row.append(&icon);
            let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
            let who = if item.who.is_empty() { tr!("Unknown application") } else { item.who.clone() };
            words.append(&gtk::Label::builder().label(who).xalign(0.0).ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(34).build());
            let reason = if !item.why.is_empty() {
                item.why.clone()
            } else if item.mode == "block" {
                tr!("Blocking")
            } else {
                tr!("Delaying")
            };
            let why = gtk::Label::builder().label(reason).xalign(0.0).wrap(true).max_width_chars(36).build();
            why.add_css_class("subtitle");
            words.append(&why);
            row.append(&words);
            self.remaining_list.append(&row);
        }
        self.anyway_button.grab_focus();
    }

    pub fn confirm(&self) {
        let action = self.pending.borrow().clone();
        self.cancel();
        if let Some(action) = action.filter(|action| confirmation(action).is_some()) {
            if let Some(app) = self.app() {
                app.perform(&action);
            }
        }
    }

    pub fn destroy(&self) {
        if let Some(controller) = self.controller.take() {
            controller.close();
        }
        self.window.destroy();
    }
}

// ---------------------------------------------------------------------------
// The application

/// Builds the backend from the change callback (Python `backend_factory`).
pub type BackendFactory = Box<dyn FnOnce(Rc<dyn Fn()>) -> Rc<dyn Backend>>;

pub fn session_backend_factory() -> BackendFactory {
    Box::new(|changed| Rc::new(SessionBackend::new(changed)) as Rc<dyn Backend>)
}

pub struct AppInner {
    pub app: gtk::Application,
    popup: RefCell<Option<Rc<SessionPopup>>>,
    busy: Cell<bool>,
    ending: RefCell<Option<Rc<dyn Ending>>>,
    ending_action: RefCell<Option<String>>,
    backend: RefCell<Option<Rc<dyn Backend>>>,
    holds: RefCell<Vec<gio::ApplicationHoldGuard>>,
}

#[derive(Clone)]
pub struct SessionApplication(pub Rc<AppInner>);

impl SessionApplication {
    pub fn new(application_id: &str, backend_factory: BackendFactory) -> Self {
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        let this = Self(Rc::new(AppInner {
            app: app.clone(), popup: RefCell::new(None), busy: Cell::new(false), ending: RefCell::new(None),
            ending_action: RefCell::new(None), backend: RefCell::new(None), holds: RefCell::default(),
        }));
        let weak = Rc::downgrade(&this.0);
        let changed: Rc<dyn Fn()> = Rc::new(move || if let Some(inner) = weak.upgrade() { SessionApplication(inner).changed() });
        this.0.backend.replace(Some(backend_factory(changed)));
        let weak = Rc::downgrade(&this.0);
        app.connect_startup(move |app| {
            tessel_gtk::theme::attach_application(app);
            if let Some(inner) = weak.upgrade() {
                if let Some(backend) = SessionApplication(inner).backend() {
                    backend.start();
                }
            }
        });
        let weak = Rc::downgrade(&this.0);
        app.connect_command_line(move |_, command| {
            let Some(inner) = weak.upgrade() else { return glib::ExitCode::FAILURE };
            SessionApplication(inner).command_line(command)
        });
        let weak = Rc::downgrade(&this.0);
        app.connect_activate(move |_| if let Some(inner) = weak.upgrade() { SessionApplication(inner).show("bottom", 76, false) });
        let weak = Rc::downgrade(&this.0);
        app.connect_shutdown(move |_| if let Some(inner) = weak.upgrade() { SessionApplication(inner).shutdown() });
        this
    }

    pub fn application(&self) -> &gtk::Application { &self.0.app }
    pub fn backend(&self) -> Option<Rc<dyn Backend>> { self.0.backend.borrow().clone() }
    pub fn popup(&self) -> Option<Rc<SessionPopup>> { self.0.popup.borrow().clone() }
    pub fn busy(&self) -> bool { self.0.busy.get() }
    pub fn ending(&self) -> Option<Rc<dyn Ending>> { self.0.ending.borrow().clone() }

    fn hold(&self) { self.0.holds.borrow_mut().push(self.0.app.hold()); }
    fn release(&self) { self.0.holds.borrow_mut().pop(); }

    fn command_line(&self, command: &gio::ApplicationCommandLine) -> glib::ExitCode {
        let arguments: Vec<String> = command.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let prog = args::prog(arguments.first().map(String::as_str).unwrap_or("tessel-session"));
        match args::parse(arguments.get(1..).unwrap_or(&[])) {
            Outcome::Run(parsed) => {
                self.show(&parsed.popup_edge, parsed.popup_offset as i32, parsed.menu);
                glib::ExitCode::SUCCESS
            }
            Outcome::Help => {
                command.print_literal(&args::help(&prog));
                glib::ExitCode::SUCCESS
            }
            Outcome::Error(message) => {
                command.printerr_literal(&args::error_text(&prog, &message));
                glib::ExitCode::from(2)
            }
        }
    }

    pub fn show(&self, edge: &str, offset: i32, menu: bool) {
        if self.busy() {
            let done = self.ending().is_some_and(|ending| ending.done());
            if done {
                if let Some(popup) = self.popup() {
                    popup.controller().activate();
                }
            }
            return;
        }
        if let Some(popup) = self.popup().filter(|popup| popup.menu != menu) {
            self.0.popup.replace(None);
            popup.destroy();
        }
        match self.popup() {
            None => {
                let popup = SessionPopup::new(self, edge, offset, menu);
                self.0.popup.replace(Some(popup.clone()));
                popup.controller().show();
            }
            Some(popup) => {
                let controller = popup.controller();
                controller.set_position(edge, offset);
                controller.activate();
            }
        }
    }

    fn is_popup_window(&self, window: &gtk::Window) -> bool {
        self.popup().is_some_and(|popup| popup.window.upcast_ref::<gtk::Window>() == window)
    }

    pub fn hidden(&self, window: &gtk::Window) {
        if self.is_popup_window(window) && self.popup().is_some_and(|p| p.controller.borrow().as_ref().is_some_and(|c| c.show_pending())) {
            // set_position() hid it only to reappear on another edge.
            return;
        }
        if self.is_popup_window(window) && !window.get_visible() && self.ending().is_some_and(|e| e.done()) {
            // Dismissing the still-open apps list (Escape/outside click) cancels.
            self.cancel_end(false);
        }
        if self.is_popup_window(window) && !window.get_visible() && !self.busy() {
            if let Some(popup) = self.0.popup.take() {
                popup.destroy();
            }
        }
    }

    pub fn changed(&self) {
        if let Some(popup) = self.popup() {
            popup.apply_state();
        }
    }

    pub fn perform(&self, action: &str) {
        if self.busy() {
            return;
        }
        self.0.busy.set(true);
        self.hold(); // Finish the operation even after dismissing the popup.
        if let Some(popup) = self.popup() {
            popup.controller().dismiss();
        }
        if remaining_label(action).is_some() {
            // Hidden while closing so the apps' own save prompts stay usable.
            self.0.ending_action.replace(Some(action.into()));
            let weak = Rc::downgrade(&self.0);
            let finished = Rc::new(move |windows: Vec<Window>, inhibitors: Vec<Inhibitor>, note: String| {
                if let Some(inner) = weak.upgrade() {
                    SessionApplication(inner).end_checked(windows, inhibitors, note);
                }
            });
            let Some(backend) = self.backend() else { return };
            // Python assigns `self.ending` from end_session()'s return value;
            // a report delivered synchronously during it is dropped the same way.
            let ending = backend.end_session(action, finished);
            self.0.ending.replace(Some(ending.clone()));
            let weak = Rc::downgrade(&self.0);
            ending.set_on_update(Some(Rc::new(move || if let Some(inner) = weak.upgrade() { SessionApplication(inner).end_updated() })));
            return;
        }
        self.finish(action);
    }

    fn end_checked(&self, windows: Vec<Window>, inhibitors: Vec<Inhibitor>, note: String) {
        let Some(ending) = self.ending() else { return };
        if !ending.blocking() {
            self.continue_end();
            return;
        }
        if self.popup().is_none() {
            let popup = SessionPopup::new(self, "bottom", 76, false);
            self.0.popup.replace(Some(popup));
        }
        let popup = self.popup().expect("popup");
        let action = self.0.ending_action.borrow().clone().unwrap_or_default();
        popup.show_remaining(&action, &windows, &inhibitors, &note);
        popup.controller().show();
    }

    fn end_updated(&self) {
        let Some(ending) = self.ending() else { return };
        let Some(popup) = self.popup() else { return };
        if !ending.done() {
            return;
        }
        if !ending.blocking() {
            self.continue_end(); // Every remaining app closed itself.
        } else if popup.remaining.get_visible() {
            let action = self.0.ending_action.borrow().clone().unwrap_or_default();
            popup.show_remaining(&action, &ending.remaining(), &ending.inhibitors(), &ending.note());
        }
    }

    pub fn cancel_end(&self, show: bool) {
        let Some(ending) = self.0.ending.take() else { return };
        ending.close();
        self.0.ending_action.replace(None);
        self.0.busy.set(false);
        if let Some(popup) = self.popup() {
            popup.cancel();
            if show {
                popup.controller().show();
            }
        }
        self.release();
    }

    pub fn continue_end(&self) {
        let Some(ending) = self.0.ending.take() else { return };
        let action = self.0.ending_action.take().unwrap_or_default();
        ending.close();
        if let Some(popup) = self.popup() {
            popup.cancel();
            popup.controller().dismiss();
        }
        self.finish(&action);
    }

    fn finish(&self, action: &str) {
        let Some(backend) = self.backend() else { return };
        let weak = Rc::downgrade(&self.0);
        backend.perform(action, Box::new(move |ok, error| {
            let Some(inner) = weak.upgrade() else { return };
            let this = SessionApplication(inner);
            this.0.busy.set(false);
            if ok {
                this.0.app.quit();
            } else if let Some(popup) = this.popup() {
                popup.status.set_text(&if error.is_empty() { tr!("The action could not be completed.") } else { error });
                popup.controller().show();
            }
            this.release();
        }));
    }

    pub fn shutdown(&self) {
        if let Some(ending) = self.0.ending.take() {
            ending.close();
        }
        if let Some(backend) = self.backend() {
            backend.close();
        }
        if let Some(popup) = self.0.popup.take() {
            popup.destroy();
        }
        self.0.holds.borrow_mut().clear();
    }
}
