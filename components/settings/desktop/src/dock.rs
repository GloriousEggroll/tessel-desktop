//! Dock/Panel and Desktop Behavior (dock_pages.py): Tessel Dock's own Rust
//! `SettingsWindow` in embedded mode, driven by a [`DockSettingsBridge`];
//! Desktop Behavior adds workspaces, window tiling and the authentication
//! agent status ([`DesktopControls`]).
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use tessel_core::tr;
use tessel_dock::ui::settings::{ExtraControls, SettingsOptions, SettingsWindow};

use crate::bridge::{appearance_fields, desktop_fields, dock_fields, BridgeOptions, DockSettingsBridge};

/// `systemctl --user is-active tessel-auth.service`.
pub const AUTHENTICATION_COMMAND: [&str; 4] = ["systemctl", "--user", "is-active", "tessel-auth.service"];

/// Explain the desktop's password prompts; the agent has no window of its own.
pub struct AuthenticationStatus {
    pub command: Vec<String>,
    pub widget: gtk::Box,
    pub status: gtk::Label,
}

impl AuthenticationStatus {
    pub fn new(command: Option<Vec<String>>) -> Rc<Self> {
        let command = command.unwrap_or_else(|| AUTHENTICATION_COMMAND.iter().map(|s| s.to_string()).collect());
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).css_classes(["settings-card"]).build();
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        heading.add_css_class("settings-section-heading");
        heading.append(&gtk::Label::builder().label(tr!("Authentication")).xalign(0.0).css_classes(["settings-section-title"]).build());
        widget.append(&heading);
        widget.append(&gtk::Label::builder()
            .label(tr!("When an app needs administrator permission, the desktop asks for your password in its own authentication window. It starts with your session and has no settings to change."))
            .xalign(0.0).wrap(true).max_width_chars(60).css_classes(["settings-muted"]).build());
        let status = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(60).css_classes(["settings-muted"]).build();
        widget.append(&status);
        let this = Rc::new(Self { command, widget, status });
        this.refresh();
        this
    }

    pub fn refresh(&self) {
        let args: Vec<&std::ffi::OsStr> = self.command.iter().map(std::ffi::OsStr::new).collect();
        let process = match gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) {
            Ok(process) => process,
            Err(_) => {
                self.status.set_text(&tr!("Status: unknown"));
                return;
            }
        };
        let status = self.status.clone();
        process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
            let output = result.ok().and_then(|(out, _)| out).map(|s| s.to_string()).unwrap_or_default();
            let running = tessel_core::pyjson::py_strip(&output) == "active";
            status.set_text(&if running { tr!("Status: running") } else { tr!("Status: not running. Sign out and back in to start it.") });
        });
    }
}

/// `workspace_settings.WorkspaceControls` as Desktop Behavior uses it.
pub trait WorkspacePart {
    fn widget(&self) -> gtk::Widget;
    /// `.count` (the workspace count SpinButton).
    fn count(&self) -> gtk::SpinButton;
    fn limit(&self) -> gtk::Label;
    fn status(&self) -> gtk::Label;
    fn refresh(&self);
    fn apply(&self) -> bool;
}

/// `tiling_settings.TilingControls` / `GaplessControls` as Desktop Behavior uses it.
pub trait TilingPart {
    fn widget(&self) -> gtk::Widget;
    /// `.switch` (the Gapless switch).
    fn switch(&self) -> gtk::Switch;
    /// `getattr(tiling, 'enabled_switch', tiling.switch)`.
    fn enabled_switch(&self) -> gtk::Switch { self.switch() }
    /// `getattr(tiling, 'outline_switch', tiling.switch)`.
    fn outline_switch(&self) -> gtk::Switch { self.switch() }
    fn error(&self) -> gtk::Label;
    fn refresh(&self);
}

impl TilingPart for tessel_tiling::ui::settings::TilingControls {
    fn widget(&self) -> gtk::Widget { tessel_tiling::ui::settings::TilingControls::widget(self).clone().upcast() }
    fn switch(&self) -> gtk::Switch { self.gapless().switch().clone() }
    fn enabled_switch(&self) -> gtk::Switch { tessel_tiling::ui::settings::TilingControls::enabled_switch(self).clone() }
    fn outline_switch(&self) -> gtk::Switch { tessel_tiling::ui::settings::TilingControls::outline_switch(self).clone() }
    fn error(&self) -> gtk::Label { tessel_tiling::ui::settings::TilingControls::error(self).clone() }
    fn refresh(&self) { tessel_tiling::ui::settings::TilingControls::refresh(self) }
}

impl TilingPart for tessel_tiling::ui::settings::GaplessControls {
    fn widget(&self) -> gtk::Widget { tessel_tiling::ui::settings::GaplessControls::widget(self).clone().upcast() }
    fn switch(&self) -> gtk::Switch { tessel_tiling::ui::settings::GaplessControls::switch(self).clone() }
    fn error(&self) -> gtk::Label { tessel_tiling::ui::settings::GaplessControls::error(self).clone() }
    fn refresh(&self) { tessel_tiling::ui::settings::GaplessControls::refresh(self) }
}

pub type WorkspaceFactory = Rc<dyn Fn() -> Result<Rc<dyn WorkspacePart>, String>>;
pub type TilingFactory = Rc<dyn Fn() -> Rc<dyn TilingPart>>;
pub type AuthenticationFactory = Rc<dyn Fn() -> Rc<AuthenticationStatus>>;
pub type BridgeFactory = Rc<dyn Fn(Option<gtk::Application>, BTreeSet<String>) -> Result<DockSettingsBridge, String>>;

fn default_tiling() -> Rc<dyn TilingPart> {
    Rc::new(tessel_tiling::ui::settings::TilingControls::new(None, None, true, None,
        tessel_tiling::ui::settings::Runner::Thread))
}

/// Workspaces, Window Tiling and Authentication below the dock's own cards.
pub struct DesktopControls {
    pub workspace: Rc<dyn WorkspacePart>,
    pub tiling: Rc<dyn TilingPart>,
    pub authentication: Rc<AuthenticationStatus>,
    pub count: gtk::SpinButton,
    pub widget: gtk::Box,
    pub tiling_card: gtk::Box,
    pub snap_layouts: Rc<crate::snap_layouts::SnapLayoutsGroup>,
}

impl DesktopControls {
    pub fn new(workspace_factory: Option<&WorkspaceFactory>, tiling_factory: Option<&TilingFactory>,
               authentication_factory: Option<&AuthenticationFactory>,
               snap_options: Option<&crate::snap_layouts::SnapOptions>) -> Result<Rc<Self>, String> {
        let workspace = match workspace_factory {
            Some(factory) => factory()?,
            None => crate::workspaces::default_workspace_controls()?,
        };
        let tiling = tiling_factory.map(|f| f()).unwrap_or_else(default_tiling);
        let authentication = authentication_factory.map(|f| f()).unwrap_or_else(|| AuthenticationStatus::new(None));
        let count = workspace.count();
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 14);
        widget.append(&workspace.widget());
        let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).css_classes(["settings-card"]).build();
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        heading.add_css_class("settings-section-heading");
        heading.append(&gtk::Label::builder().label(tr!("Window Tiling")).xalign(0.0).css_classes(["settings-section-title"]).build());
        card.append(&heading);
        card.append(&tiling.widget());
        widget.append(&card);
        let snap_layouts = crate::snap_layouts::SnapLayoutsGroup::new(snap_options.cloned().unwrap_or_default());
        if tiling.enabled_switch() != tiling.switch() {
            snap_layouts.watch_tiling(&tiling.enabled_switch());
        }
        widget.append(&snap_layouts.widget);
        widget.append(&authentication.widget);
        Ok(Rc::new(Self { workspace, tiling, authentication, count, widget, tiling_card: card, snap_layouts }))
    }

    pub fn refresh(&self) {
        self.workspace.refresh();
        self.tiling.refresh();
        self.snap_layouts.refresh();
        self.authentication.refresh();
    }
}

impl ExtraControls for DesktopControls {
    fn widget(&self) -> gtk::Widget { self.widget.clone().upcast() }
    fn apply(&self) -> bool { self.workspace.apply() }
}

fn items(container: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = Vec::new();
    let mut item = container.first_child();
    while let Some(widget) = item {
        item = widget.next_sibling();
        result.push(widget);
    }
    result
}

/// Section headings inside cards carry no icon, as on every other page.
pub fn plain_section_headings(root: &gtk::Widget) {
    for child in items(root) {
        let is_heading = child.is::<gtk::Box>() && items(&child).iter().any(|item| {
            item.downcast_ref::<gtk::Label>().is_some_and(|l| l.has_css_class("settings-section-title"))
        });
        if is_heading {
            for item in items(&child) {
                if item.is::<gtk::Image>() {
                    item.set_visible(false);
                }
            }
        } else {
            plain_section_headings(&child);
        }
    }
}

/// Python keyword arguments of `_create_page(...)`.
#[derive(Default, Clone)]
pub struct DockPageOptions {
    pub bridge_factory: Option<BridgeFactory>,
    pub workspace_factory: Option<WorkspaceFactory>,
    pub tiling_factory: Option<TilingFactory>,
    pub authentication_factory: Option<AuthenticationFactory>,
    pub snap_layouts: Option<crate::snap_layouts::SnapOptions>,
}

/// One embedded page; `page.controller::<DockPage>()` gives the live
/// controller (Python `container._settings_controller` / `_settings_bridge`).
pub struct DockPage {
    pub section: String,
    pub application: Option<gtk::Application>,
    pub container: gtk::Box,
    provider: gtk::CssProvider,
    display: gtk::gdk::Display,
    options: DockPageOptions,
    controller: RefCell<Option<SettingsWindow>>,
    bridge: RefCell<Option<DockSettingsBridge>>,
    extra: RefCell<Option<Rc<DesktopControls>>>,
    closed: Cell<bool>,
}

impl DockPage {
    pub fn controller(&self) -> Option<SettingsWindow> { self.controller.borrow().clone() }
    pub fn bridge(&self) -> Option<DockSettingsBridge> { self.bridge.borrow().clone() }
    /// `controller.extra_controls`.
    pub fn extra_controls(&self) -> Option<Rc<DesktopControls>> { self.extra.borrow().clone() }

    fn dispose(&self) {
        if let Some(bridge) = self.bridge.take() {
            bridge.close();
        }
        if let Some(controller) = self.controller.take() {
            controller.destroy();
        }
        self.extra.take();
        if let Some(child) = self.container.first_child() {
            self.container.remove(&child);
        }
    }

    pub fn reset(self: &Rc<Self>) -> Result<(), String> {
        self.dispose();
        let fields = if self.section == "dock" {
            let mut fields = dock_fields();
            fields.remove("match_wallpaper");
            fields
        } else {
            desktop_fields()
        };
        let bridge = match &self.options.bridge_factory {
            Some(factory) => factory(self.application.clone(), fields)?,
            None => DockSettingsBridge::new(self.application.clone(), BridgeOptions { fields, ..BridgeOptions::default() }, None)?,
        };
        self.bridge.replace(Some(bridge.clone()));
        let extra = if self.section == "desktop" {
            Some(DesktopControls::new(self.options.workspace_factory.as_ref(), self.options.tiling_factory.as_ref(),
                                      self.options.authentication_factory.as_ref(), self.options.snap_layouts.as_ref())?)
        } else {
            None
        };
        self.extra.replace(extra.clone());
        let weak = Rc::downgrade(self);
        let reset: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(page) = weak.upgrade() {
                if let Err(error) = page.reset() {
                    eprintln!("tessel-settings: {error}");
                }
            }
        });
        let controller = SettingsWindow::new(Rc::new(bridge.clone()), SettingsOptions {
            section: self.section.clone(), embedded: true, reset: Some(reset),
            extra_controls: extra.clone().map(|e| e as Rc<dyn ExtraControls>),
        });
        let result_label = controller.controls().result_label.clone();
        bridge.set_on_error(Rc::new(move |message| {
            result_label.set_text(message);
            result_label.add_css_class("error");
            result_label.set_visible(true);
        }));
        if let Some(body) = controller.window().child() {
            controller.window().set_child(gtk::Widget::NONE);
            self.container.append(&body);
            plain_section_headings(&body);
        }
        self.controller.replace(Some(controller));
        Ok(())
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.dispose();
        gtk::style_context_remove_provider_for_display(&self.display, &self.provider);
    }

    pub fn deactivate(&self) {
        if let Some(bridge) = self.bridge() {
            bridge.restore_appearance();
        }
    }

    pub fn activate(self: &Rc<Self>) {
        if let Some(extra) = self.extra_controls() {
            extra.refresh();
        }
        let (Some(bridge), Some(controller)) = (self.bridge(), self.controller()) else { return };
        if self.section != "dock" {
            return;
        }
        let previous = bridge.config();
        let weak = Rc::downgrade(self);
        let target = bridge.clone();
        bridge.refresh_async(Some(Box::new(move |ok| {
            let Some(page) = weak.upgrade() else { return };
            let same = page.bridge().is_some_and(|b| Arc::ptr_eq(&b.core_rc(), &target.core_rc()));
            if !ok || page.closed.get() || !same {
                return;
            }
            let config = target.config();
            let c = controller.controls();
            controller.refresh_tray_rows();
            controller.set_switching_mode(true);
            c.match_wallpaper.set_active(config.match_wallpaper);
            // A saved change on Theme & Wallpaper wins over this page's older
            // value. If it did not change elsewhere, keep any local edit.
            if previous.transparency != config.transparency {
                c.transparency.set_value(config.transparency as f64);
            }
            if previous.transparency_enabled != config.transparency_enabled {
                c.transparency_enabled.set_active(config.transparency_enabled);
            }
            if previous.frosted != config.frosted {
                c.frosted.set_active(config.frosted);
            }
            if previous.frost_strength != config.frost_strength {
                c.frost_strength.set_value(config.frost_strength as f64);
            }
            c.transparency.set_sensitive(c.transparency_enabled.is_active());
            controller.update_material_controls();
            controller.set_switching_mode(false);
            controller.preview_appearance();
        })), Some(appearance_fields()));
    }

    /// Search spotlight targets (Python attributes of the controller).
    pub fn register_targets(&self, page: &SettingsPage) {
        let Some(controller) = self.controller() else { return };
        let c = controller.controls();
        if self.section == "dock" {
            page.add_target("mode", &c.mode);
            page.add_target("monitor", &c.monitor);
            page.add_target("size", &c.size);
            page.add_target("float_mode", &c.float_mode);
            page.add_target("rounded_corners", &c.rounded_corners);
            page.add_target("auto_hide", &c.auto_hide);
            page.add_target("fullscreen", &c.fullscreen);
            page.add_target("application_display", &c.application_display);
            page.add_target("separate_app_dock", &c.separate_app_dock);
            page.add_target("background_color", &c.background_color.button);
            page.add_target("bar_border_enabled", &c.bar_border_enabled);
            page.add_target("app_dock_layout_controls", c.app_dock_layout_controls.widget());
            page.add_target("tray_icon_size", &c.tray_icon_size);
            if let Some(media) = c.group_switches.get("media") {
                page.add_target("group_media", media);
            }
        } else {
            page.add_target("hot_corner_delay", &c.hot_corner_delay);
            page.add_target("menu_hotkey_fullscreen", &c.menu_hotkey_fullscreen);
            if let Some(extra) = self.extra_controls() {
                page.add_target("workspace_count", &extra.count);
                page.add_target("gapless_switch", &extra.tiling.switch());
                // Search targets for the other tiling and authentication rows.
                page.add_target("tiling_switch", &extra.tiling.enabled_switch());
                page.add_target("focus_outline_switch", &extra.tiling.outline_switch());
                page.add_target("authentication_status", &extra.authentication.widget);
                page.add_target("snap_layouts", &extra.snap_layouts.layout);
            }
        }
    }
}

impl DockSettingsBridge {
    fn core_rc(&self) -> Arc<crate::bridge::BridgeCore> { self.core() }
}

/// `_create_page(application, section, …)`: `section` is `"dock"` or `"desktop"`.
pub fn create_page(application: Option<gtk::Application>, section: &str, options: DockPageOptions) -> Result<SettingsPage, String> {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    container.add_css_class("tessel-dock-settings");
    container.set_hexpand(true);
    container.set_vexpand(true);
    let display = WidgetExt::display(&container);
    let provider = tessel_gtk::utils::add_css(tessel_dock::appearance::style::CSS, Some(&display), gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    let page = Rc::new(DockPage {
        section: section.to_owned(), application, container: container.clone(), provider, display, options,
        controller: RefCell::new(None), bridge: RefCell::new(None), extra: RefCell::new(None), closed: Cell::new(false),
    });
    if let Err(error) = page.reset() {
        page.close();
        return Err(error);
    }
    let settings = SettingsPage::new(container);
    let (c1, c2, c3) = (page.clone(), page.clone(), page.clone());
    settings.set_close(move || c1.close());
    settings.set_activate(move || c2.activate());
    settings.set_deactivate(move || c3.deactivate());
    page.register_targets(&settings);
    settings.set_controller(page);
    Ok(settings)
}

pub fn create_dock_page(application: Option<gtk::Application>, options: DockPageOptions) -> Result<SettingsPage, String> {
    create_page(application, "dock", options)
}

pub fn create_desktop_page(application: Option<gtk::Application>, options: DockPageOptions) -> Result<SettingsPage, String> {
    create_page(application, "desktop", options)
}

/// `dock_pages.build_page(section, host)`.
pub fn build_dock_page(section: &str, host: &Host) -> Result<SettingsPage, String> {
    match section {
        "dock" => create_dock_page(host.application(), DockPageOptions::default()),
        "behavior" => create_desktop_page(host.application(), DockPageOptions::default()),
        _ => Err("Unknown dock settings page".into()),
    }
}
