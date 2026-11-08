//! Displays (desktop_pages.DisplaysPage): the tessel-displays editor embedded
//! (`tessel_displays::window::TesselDisplaysApp`, embedded mode), HDR calibration
//! launch and an explicit "Identify displays" overlay.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gs_base::style::{empty_state, find_title, page_header, PageHeader};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use tessel_core::tr;
use tessel_displays::color_config::{self, Status};
use tessel_displays::ipc;
use tessel_displays::window::{ClientFactory, TesselDisplaysApp};

use crate::util::label;

/// Connectors whose monitor supports HDR, from the compositor's color status.
pub fn hdr_outputs(status: &Status) -> Vec<String> {
    let mut names: Vec<String> = status.iter()
        .filter(|(name, values)| name.as_str() != "compositor"
            && values.get("enabled").map(String::as_str) == Some("true")
            && values.get("hdr_supported").map(String::as_str) == Some("true"))
        .map(|(name, _)| name.clone()).collect();
    names.sort();
    names
}

/// The installed `tessel-hdr-calibration [--output X]`, else `None`.
pub fn hdr_calibration_command(connector: Option<&str>) -> Option<Vec<String>> {
    let program = tessel_core::paths::program("tessel-hdr-calibration");
    if !program.is_absolute() {
        return None;
    }
    let mut command = vec![program.to_string_lossy().into_owned()];
    if let Some(connector) = connector.filter(|c| !c.is_empty()) {
        command.extend(["--output".to_owned(), connector.to_owned()]);
    }
    Some(command)
}

pub type Launch = Rc<dyn Fn(&[String]) -> Result<(), String>>;

fn default_launch(command: &[String]) -> Result<(), String> {
    let args: Vec<&std::ffi::OsStr> = command.iter().map(std::ffi::OsStr::new).collect();
    gio::Subprocess::newv(&args, gio::SubprocessFlags::NONE).map(|_| ()).map_err(|e| e.message().to_owned())
}

/// Python keyword arguments of `DisplaysPage(...)`.
#[derive(Default)]
pub struct DisplaysOptions {
    pub brightness_client_factory: Option<ClientFactory>,
    /// `load_data=True` reads tessel-displays (`GET`) and the color status on a worker.
    pub load_data: Option<bool>,
    pub hdr_status: Option<Status>,
    pub launch: Option<Launch>,
}

pub struct DisplaysPage {
    pub host: Host,
    pub widget: gtk::Box,
    pub header: PageHeader,
    pub status: RefCell<gtk::Widget>,
    pub owner: RefCell<Option<Rc<TesselDisplaysApp>>>,
    pub hdr: RefCell<Vec<String>>,
    pub launch: RefCell<Launch>,
    pub calibrate: RefCell<Option<gtk::Button>>,
    pub identify_button: RefCell<Option<gtk::Button>>,
    pub closed: Cell<bool>,
    overlay_timer: RefCell<Option<glib::SourceId>>,
    factory: RefCell<Option<ClientFactory>>,
}

impl DisplaysPage {
    pub fn new(host: &Host, options: DisplaysOptions) -> Rc<Self> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).vexpand(true).build();
        widget.add_css_class("tessel-displays");
        let header = page_header(&tr!("Displays"), Some(&tr!("Arrange your screens and adjust their display settings.")),
                                 Some("video-display-symbolic"));
        header.root.set_margin_top(24);
        header.root.set_margin_start(24);
        header.root.set_margin_end(24);
        widget.append(&header.root);
        let status = label(&tr!("Loading display controls…"), Some("subtitle"));
        status.set_margin_start(24);
        widget.append(&status);
        let page = Rc::new(Self {
            host: host.clone(), widget, header, status: RefCell::new(status.upcast()), owner: RefCell::new(None),
            hdr: RefCell::new(options.hdr_status.as_ref().map(hdr_outputs).unwrap_or_default()),
            launch: RefCell::new(options.launch.unwrap_or_else(|| Rc::new(default_launch))),
            calibrate: RefCell::new(None), identify_button: RefCell::new(None), closed: Cell::new(false),
            overlay_timer: RefCell::new(None), factory: RefCell::new(options.brightness_client_factory),
        });
        if options.load_data.unwrap_or(true) {
            let weak = Rc::downgrade(&page);
            crate::util::Runner::Thread.run(|| {
                let status = color_config::read_status_file(color_config::status_path().as_deref()).unwrap_or_default();
                (hdr_outputs(&status), ipc::ipc_operation("GET", ""))
            }, move |result| {
                let Some(page) = weak.upgrade() else { return };
                let (hdr, reply) = result.unwrap_or_else(|| (Vec::new(), Err("worker failed".into())));
                page.hdr.replace(hdr);
                let error = match reply {
                    Ok(text) => (!ipc::get_config_data_from(|| Ok(text))).then(|| tr!("Display settings could not be loaded.")),
                    Err(error) => {
                        println!("Failed to load config data: {error}");
                        Some(tr!("Display settings could not be loaded."))
                    }
                };
                page.loaded(error);
            });
        } else {
            page.loaded(None);
        }
        page
    }

    fn loaded(self: &Rc<Self>, error: Option<String>) {
        if self.closed.get() {
            return;
        }
        let heads = ipc::config_data().get("STATE").and_then(|s| s.get("HEADS")).is_some_and(tessel_core::pyjson::truthy);
        if error.is_some() || !heads {
            self.failed(error.as_deref());
            return;
        }
        let Some(application) = self.host.application() else {
            self.failed(Some("No application"));
            return;
        };
        let owner = TesselDisplaysApp::new(&application, self.factory.take(), true);
        let content = owner.window.child();
        owner.window.set_child(gtk::Widget::NONE);
        let status = self.status.borrow().clone();
        if status.parent().as_ref() == Some(self.widget.upcast_ref()) {
            self.widget.remove(&status);
        }
        let toolbar = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).valign(gtk::Align::Center).build();
        // HDR Calibration is its own app; offer it wherever an HDR monitor is connected.
        let calibrate = gtk::Button::builder().label(tr!("Calibrate HDR…")).visible(!self.hdr.borrow().is_empty()).build();
        calibrate.set_tooltip_text(Some(&tr!("Set highlight brightness and SDR white for an HDR monitor.")));
        let weak = Rc::downgrade(self);
        calibrate.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.calibrate_hdr();
            }
        });
        toolbar.append(&calibrate);
        let identify = gtk::Button::with_label(&tr!("Identify displays"));
        let weak = Rc::downgrade(self);
        identify.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.identify();
            }
        });
        toolbar.append(&identify);
        // Page actions sit at the end of the shared header, which replaces
        // the editor's own title; its body lines up with the header.
        self.header.root.append(&toolbar);
        if let Some(content) = &content {
            if let Some(title) = find_title(content, 9) {
                if let Some(heading) = title.parent() {
                    heading.set_visible(false);
                    let inner = heading.parent().and_then(|body| body.parent());
                    if let Some(inner) = inner.filter(|inner| inner.margin_start() == 30) {
                        inner.set_margin_start(24);
                        inner.set_margin_end(24);
                        inner.set_margin_bottom(24);
                    }
                }
            }
            self.widget.append(content);
        }
        self.calibrate.replace(Some(calibrate));
        self.identify_button.replace(Some(identify));
        self.owner.replace(Some(owner));
    }

    /// Global Settings' shared error state instead of a bare label.
    fn failed(&self, error: Option<&str>) {
        let (title, detail, icon) = match error.filter(|e| !e.is_empty()) {
            Some(error) => {
                let mut detail = tr!("Display settings could not be loaded. Make sure tessel-displays is running, then try again.");
                if error != tr!("Display settings could not be loaded.") {
                    detail = format!("{detail}\n{error}");
                }
                (tr!("Displays are unavailable"), detail, "dialog-warning-symbolic")
            }
            None => (tr!("No displays found"), tr!("Connect a display, then try again."), "video-display-symbolic"),
        };
        if let Some(status) = self.status.borrow().downcast_ref::<gtk::Label>() {
            status.set_text(&detail);
        }
        self.host.show_page_error("displays", &detail, Some(&title), icon);
        // The host replaces this page; keep the same state here for hosts
        // that do not (Python: show_page_error(...) returned None).
        let status = self.status.borrow().clone();
        if status.parent().as_ref() == Some(self.widget.upcast_ref()) {
            self.widget.remove(&status);
        }
        let state = empty_state(&title, &detail, icon, None, None);
        self.widget.append(&state.root);
        self.status.replace(state.root.upcast());
    }

    pub fn calibrate_hdr(&self) {
        let connector = self.hdr.borrow().first().cloned();
        let result = match hdr_calibration_command(connector.as_deref()) {
            None => Err(tr!("Install HDR Calibration to use this control.")),
            Some(command) => {
                let launch = self.launch.borrow().clone();
                launch(&command)
            }
        };
        if let (Err(error), Some(button)) = (result, self.calibrate.borrow().as_ref()) {
            button.set_tooltip_text(Some(&error));
        }
    }

    pub fn identify(self: &Rc<Self>) {
        let owner = self.owner.borrow().clone();
        let Some(owner) = owner.filter(|_| !self.closed.get()) else { return };
        let Some(layout) = owner.layout_area.borrow().clone() else { return };
        self.deactivate();
        for meta in layout.entries() {
            if let Some(overlay) = layout.show_overlay(&meta.desc, meta.orig_width, meta.orig_height, &meta.name) {
                owner.push_overlay(overlay);
            }
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local(3, move || {
            if let Some(page) = weak.upgrade() {
                page.overlay_timer.take();
                page.hide_overlays();
            }
            glib::ControlFlow::Break
        });
        self.overlay_timer.replace(Some(id));
    }

    /// Python `page._overlay_timer != 0`.
    pub fn overlay_timer_active(&self) -> bool { self.overlay_timer.borrow().is_some() }

    fn hide_overlays(&self) {
        if let Some(owner) = self.owner.borrow().as_ref() {
            owner.hide_overlays();
        }
    }

    pub fn activate(&self) {
        if let Some(owner) = self.owner.borrow().as_ref() {
            owner.refresh_brightness();
        }
    }

    pub fn deactivate(&self) {
        if let Some(id) = self.overlay_timer.take() {
            id.remove();
        }
        self.hide_overlays();
    }

    pub fn close(&self) {
        self.closed.set(true);
        self.deactivate();
        if let Some(owner) = self.owner.take() {
            owner.close_brightness();
            if let Some(palette) = owner.palette.take() {
                palette.close();
            }
            if let Some(provider) = tessel_displays::style::display_style(&owner.window) {
                gtk::style_context_remove_provider_for_display(&WidgetExt::display(&owner.window), &provider);
            }
            owner.window.destroy();
        }
    }
}

/// `build_page("displays", host)` with injection.
pub fn build_displays_page(host: &Host, options: DisplaysOptions) -> SettingsPage {
    let controller = DisplaysPage::new(host, options);
    let page = SettingsPage::new(controller.widget.clone());
    let (c1, c2, c3) = (controller.clone(), controller.clone(), controller.clone());
    page.set_close(move || c1.close());
    page.set_activate(move || c2.activate());
    page.set_deactivate(move || c3.deactivate());
    page.set_controller(controller);
    page
}

