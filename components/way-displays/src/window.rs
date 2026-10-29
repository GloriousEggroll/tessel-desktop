//! `WayDisplaysApp(Gtk.ApplicationWindow)`: the Displays window
//! (way-displays-gui:2233-3063).

use crate::color_config::{self, ColorSettings};
use crate::compat::brightness_client::{BrightnessClient, BrightnessModel};
use crate::ipc;
use crate::labwc::{self, HdrConfig};
use crate::layout::MonitorLayout;
use crate::model::{self, transform_text};
use crate::monitor_widget::{monitor_title, MonitorWidget};
use crate::overlay::MonitorOverlay;
use crate::primary::{self, WriteResult};
use crate::style::style_window;
use glib::ControlFlow;
use gtk::prelude::*;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;
use suite_core::pyjson::{py_str, truthy};
use suite_core::tr;
use suite_gtk::theme::WindowTheme;

pub const DEFAULT_CALLBACK: &str = "notify-send \"way-displays ${CALLBACK_LEVEL}\" \"${CALLBACK_MSG}\"";

type Ipc = Rc<dyn Fn(&str, &str) -> Result<String, String>>;
type IpcConfig = Rc<dyn Fn(&str, &Value) -> Result<String, String>>;

/// The module functions the Python tests patch (`mock.patch.object(gui, …)`).
#[derive(Clone)]
pub struct Hooks {
    pub ipc_operation: Ipc,
    pub ipc_config_operation: IpcConfig,
    pub get_config_data: Rc<dyn Fn() -> bool>,
    pub get_xwayland_primary_output: Rc<dyn Fn() -> Option<String>>,
    pub apply_xwayland_primary_output: Rc<dyn Fn(Option<&str>) -> (bool, Option<String>)>,
    pub read_labwc_primary_environment: Rc<dyn Fn(&Path) -> Option<String>>,
    pub write_labwc_primary_environment: Rc<dyn Fn(&Path, Option<&str>) -> WriteResult>,
    pub write_labwc_primary_xinitrc: Rc<dyn Fn(&Path, Option<&str>) -> WriteResult>,
    pub read_labwc_hdr_config: Rc<dyn Fn(&Path) -> HdrConfig>,
    pub write_labwc_hdr_overrides: Rc<dyn Fn(&Path, &BTreeMap<String, bool>) -> (bool, Option<String>)>,
    pub reconfigure_labwc: Rc<dyn Fn() -> (bool, Option<String>)>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            ipc_operation: Rc::new(ipc::ipc_operation),
            ipc_config_operation: Rc::new(ipc::ipc_config_operation),
            get_config_data: Rc::new(ipc::get_config_data),
            get_xwayland_primary_output: Rc::new(primary::get_xwayland_primary_output),
            apply_xwayland_primary_output: Rc::new(primary::apply_xwayland_primary_output),
            read_labwc_primary_environment: Rc::new(primary::read_labwc_primary_environment),
            write_labwc_primary_environment: Rc::new(primary::write_labwc_primary_environment),
            write_labwc_primary_xinitrc: Rc::new(primary::write_labwc_primary_xinitrc),
            read_labwc_hdr_config: Rc::new(labwc::read_labwc_hdr_config),
            write_labwc_hdr_overrides: Rc::new(labwc::write_labwc_hdr_overrides),
            reconfigure_labwc: Rc::new(labwc::reconfigure_labwc),
        }
    }
}

thread_local! {
    static DEFAULT_HOOKS: RefCell<Option<Hooks>> = const { RefCell::new(None) };
}

/// Hooks for windows created afterwards (tests: module-level patches).
pub fn set_default_hooks(hooks: Option<Hooks>) {
    DEFAULT_HOOKS.with(|h| h.replace(hooks));
}

fn default_hooks() -> Hooks {
    DEFAULT_HOOKS.with(|h| h.borrow().clone()).unwrap_or_default()
}

pub type ClientFactory = Box<dyn FnOnce(Rc<dyn Fn()>) -> Result<Rc<dyn BrightnessModel>, String>>;

/// `create_brightness_client(on_changed)` (refresh_on_connect=True).
pub fn create_brightness_client(on_changed: Rc<dyn Fn()>) -> Result<Rc<dyn BrightnessModel>, String> {
    Ok(Rc::new(BrightnessClient::new(move || on_changed(), true)))
}

pub struct WayDisplaysApp {
    pub window: gtk::ApplicationWindow,
    pub embedded: bool,
    initializing: Cell<bool>,
    pub palette: RefCell<Option<WindowTheme>>,
    overlays: RefCell<Vec<MonitorOverlay>>,
    pub new_configs: Cell<i32>,
    pub monitors_notebook: gtk::Notebook,
    pub monitor_widgets: RefCell<Vec<Rc<MonitorWidget>>>,
    monitor_tabs: RefCell<BTreeMap<String, (Rc<MonitorWidget>, gtk::Label)>>,
    monitor_tab_indices: RefCell<BTreeMap<String, u32>>,
    updating_primary: Cell<bool>,
    pub brightness_client: RefCell<Option<Rc<dyn BrightnessModel>>>,
    pub brightness_error: RefCell<String>,
    brightness_closed: Cell<bool>,
    pub labwc_environment_path: RefCell<PathBuf>,
    pub primary_output_name: RefCell<Option<String>>,
    pub labwc_config_path: RefCell<PathBuf>,
    labwc_hdr_global: Cell<bool>,
    labwc_hdr_overrides: RefCell<BTreeMap<String, bool>>,
    labwc_managed_hdr_overrides: RefCell<BTreeMap<String, bool>>,
    labwc_hdr_available: bool,
    pub layout_area: RefCell<Option<Rc<MonitorLayout>>>,
    pub layout_frame: gtk::Frame,
    pub notify_toggle: gtk::Switch,
    pub callback_entry: gtk::Entry,
    callback_handler: RefCell<Option<glib::SignalHandlerId>>,
    pub auto_scale_switch: gtk::Switch,
    auto_scale_enabled: Cell<bool>,
    scale_overrides: RefCell<Map<String, Value>>,
    pub save_btn: gtk::Button,
    pub hooks: RefCell<Hooks>,
    pub arrange_mode: RefCell<String>,
}

fn obj<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|v| truthy(v))
}

impl WayDisplaysApp {
    pub fn new(application: &gtk::Application, factory: Option<ClientFactory>, embedded: bool) -> Rc<Self> {
        let hooks = default_hooks();
        let window = gtk::ApplicationWindow::new(application);
        window.set_title(Some(&tr!("Displays")));
        let monitors = WidgetExt::display(&window).monitors();
        let geometry = monitors.item(0).and_downcast::<gtk::gdk::Monitor>().map(|m| m.geometry());
        window.set_default_size(geometry.map(|g| 980.min(g.width() - 60)).unwrap_or(980),
                                geometry.map(|g| 780.min(g.height() - 100)).unwrap_or(780));
        window.set_resizable(true);
        window.set_decorated(true);
        let palette = style_window(&window, false);
        let header = gtk::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some(&tr!("Displays")))));
        header.set_show_title_buttons(true);
        window.set_titlebar(Some(&header));

        let cfg = ipc::config_data().get("CFG").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
        let (callback_cmd, persisted_primary) = primary::split_primary_callback(
            &cfg.get("CALLBACK_CMD").map(py_str).unwrap_or_default());
        let environment_path = primary::labwc_environment_path();
        let environment_primary = (hooks.read_labwc_primary_environment)(&environment_path);
        let primary_name = persisted_primary.or(environment_primary).or_else(|| (hooks.get_xwayland_primary_output)());
        let config_path = labwc::labwc_config_path();
        let hdr = (hooks.read_labwc_hdr_config)(&config_path);
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
        let hdr_available = std::env::var("LABWC_PID").is_ok_and(|v| !v.is_empty()) || desktop.contains("labwc");

        let layout_frame = gtk::Frame::new(None);
        layout_frame.set_hexpand(true);
        // A fixed arrangement area keeps the display cards in view on tall windows.
        layout_frame.set_size_request(-1, 300);
        layout_frame.set_vexpand(false);

        let save_btn = gtk::Button::with_label(&tr!("Apply"));
        save_btn.add_css_class("primary");
        save_btn.set_halign(gtk::Align::End);
        save_btn.set_hexpand(true);
        save_btn.set_sensitive(false);

        let this = Rc::new(Self {
            window: window.clone(), embedded, initializing: Cell::new(true), palette: RefCell::new(Some(palette)),
            overlays: RefCell::new(Vec::new()), new_configs: Cell::new(1), monitors_notebook: gtk::Notebook::new(),
            monitor_widgets: RefCell::new(Vec::new()), monitor_tabs: RefCell::new(BTreeMap::new()),
            monitor_tab_indices: RefCell::new(BTreeMap::new()), updating_primary: Cell::new(false),
            brightness_client: RefCell::new(None), brightness_error: RefCell::new(String::new()), brightness_closed: Cell::new(false),
            labwc_environment_path: RefCell::new(environment_path), primary_output_name: RefCell::new(primary_name),
            labwc_config_path: RefCell::new(config_path), labwc_hdr_global: Cell::new(hdr.global),
            labwc_hdr_overrides: RefCell::new(hdr.overrides), labwc_managed_hdr_overrides: RefCell::new(hdr.managed),
            labwc_hdr_available: hdr_available, layout_area: RefCell::new(None), layout_frame,
            notify_toggle: gtk::Switch::new(), callback_entry: gtk::Entry::new(), callback_handler: RefCell::new(None),
            auto_scale_switch: gtk::Switch::new(), auto_scale_enabled: Cell::new(false), scale_overrides: RefCell::new(Map::new()),
            save_btn, hooks: RefCell::new(hooks), arrange_mode: RefCell::new(String::new()),
        });

        let weak = Rc::downgrade(&this);
        let refresh: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.refresh_brightness();
            }
        });
        let factory = factory.unwrap_or_else(|| Box::new(create_brightness_client));
        match factory(refresh) {
            Ok(client) => {
                this.brightness_client.replace(Some(client));
            }
            Err(error) => {
                this.brightness_error.replace(error);
            }
        }
        let weak = Rc::downgrade(&this);
        window.connect_close_request(move |_| {
            if let Some(this) = weak.upgrade() {
                this.close_brightness();
            }
            glib::Propagation::Proceed
        });

        // Notifications and global auto scale are not shown: the callback
        // keeps its saved command (it also carries the primary output), and
        // each display's Automatic scaling switch replaces the global toggle.
        let weak = Rc::downgrade(&this);
        let handler = this.callback_entry.connect_changed(move |entry| {
            if let Some(this) = weak.upgrade() {
                this.on_callback_entry_changed(entry);
            }
        });
        this.callback_handler.replace(Some(handler));
        let weak = Rc::downgrade(&this);
        this.notify_toggle.connect_active_notify(move |switch| {
            if let Some(this) = weak.upgrade() {
                this.on_notify_toggle(switch.is_active());
            }
        });
        if !callback_cmd.is_empty() {
            this.callback_entry.set_text(&callback_cmd);
            this.notify_toggle.set_active(true);
        }
        let weak = Rc::downgrade(&this);
        this.save_btn.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.save_config();
            }
        });

        let save_bar = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).margin_start(12).margin_end(12)
            .margin_top(6).margin_bottom(6).build();
        save_bar.append(&this.save_btn);
        let main_content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let page_heading = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let heading = gtk::Label::builder().label(tr!("Displays")).xalign(0.0).build();
        heading.add_css_class("page-title");
        let subtitle = gtk::Label::builder().label(tr!("Arrange your screens and adjust their display settings.")).xalign(0.0).build();
        subtitle.add_css_class("subtitle");
        page_heading.append(&heading);
        page_heading.append(&subtitle);
        main_content.append(&page_heading);
        main_content.append(&this.layout_frame);
        main_content.append(&this.monitors_notebook);
        main_content.append(&save_bar);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_widget_name("main-scroll");
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        let inner = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_start(30).margin_end(30).margin_bottom(30).build();
        inner.append(&main_content);
        scroll.set_child(Some(&inner));
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scroll.set_vexpand(true);
        outer.append(&scroll);
        window.set_child(Some(&outer));

        this.load_monitor_data();
        this.reload_layout();
        this.initializing.set(false);
        if !embedded {
            let weak = Rc::downgrade(&this);
            glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.apply_primary_output();
                }
            });
        }
        let client = this.brightness_client.borrow().clone();
        if let Some(client) = client {
            if let Err(error) = client.start() {
                this.brightness_error.replace(error);
                this.refresh_brightness();
            }
        }
        this
    }

    pub fn refresh_brightness(&self) {
        if self.brightness_closed.get() {
            return;
        }
        let widgets = self.monitor_widgets.borrow().clone();
        for widget in widgets {
            widget.brightness_controls.client_error.replace(self.brightness_error.borrow().clone());
            widget.brightness_controls.refresh();
        }
    }

    pub fn close_brightness(&self) {
        for overlay in self.overlays.take() {
            overlay.destroy();
        }
        if !self.brightness_closed.get() {
            self.brightness_closed.set(true);
            let widgets = self.monitor_widgets.borrow().clone();
            for widget in widgets {
                widget.brightness_controls.close();
                widget.color_controls.close();
            }
            let client = self.brightness_client.borrow().clone();
            if let Some(client) = client {
                client.close();
            }
        }
    }

    pub fn on_monitor_toggle_changed(&self) {
        if self.new_configs.get() == 0 {
            self.save_btn.set_sensitive(true);
        }
    }

    pub fn load_monitor_data(self: &Rc<Self>) {
        let data = ipc::config_data();
        let cfg = data.get("CFG").cloned().unwrap_or_else(|| json!({}));
        let global_auto_scale = cfg.get("AUTO_SCALE").is_some_and(truthy);
        self.auto_scale_switch.set_active(global_auto_scale);
        self.auto_scale_enabled.set(global_auto_scale);
        let scale_overrides = obj(&cfg, "SCALE").and_then(Value::as_object).cloned().unwrap_or_default();
        self.scale_overrides.replace(scale_overrides.clone());

        // Runtime monitor info; way-displays 2.0 keeps per-display overrides in keyed maps.
        let transform_overrides = obj(&cfg, "TRANSFORM").cloned().unwrap_or_else(|| json!({}));
        let full_monitors: Vec<Value> = ipc::with_config_data(|data| {
            let Some(heads) = data.get_mut("STATE").and_then(|s| s.get_mut("HEADS")).and_then(Value::as_array_mut) else {
                return Vec::new();
            };
            // state_by_desc = {DESCRIPTION: head}: first position, last head.
            let mut order: Vec<String> = Vec::new();
            let mut last: BTreeMap<String, usize> = BTreeMap::new();
            for (index, head) in heads.iter().enumerate() {
                let desc = head.get("DESCRIPTION").map(py_str).unwrap_or_default();
                if !order.contains(&desc) {
                    order.push(desc.clone());
                }
                last.insert(desc, index);
            }
            let mut result = Vec::new();
            for desc in order {
                let head = &mut heads[last[&desc]];
                if let Some(transform) = transform_overrides.get(&desc) {
                    if !head.get("CURRENT").is_some_and(Value::is_object) {
                        head["CURRENT"] = json!({});
                    }
                    head["CURRENT"]["TRANSFORM"] = transform.clone();
                }
                if let Some(scale) = scale_overrides.get(&desc).filter(|v| v.is_number()) {
                    if head.get("CURRENT").is_some_and(Value::is_object) {
                        head["CURRENT"]["SCALE"] = scale.clone();
                    }
                }
                if let Some(current) = head.get_mut("CURRENT").and_then(Value::as_object_mut) {
                    current.entry("TRANSFORM").or_insert_with(|| json!("NORMAL"));
                }
                result.push(head.clone());
            }
            result
        });

        // Rebuild UI.
        let old = self.monitor_widgets.borrow().clone();
        for widget in &old {
            widget.brightness_controls.close();
            widget.color_controls.close();
        }
        while self.monitors_notebook.n_pages() > 0 {
            self.monitors_notebook.remove_page(None);
        }
        self.monitor_widgets.borrow_mut().clear();
        self.monitor_tabs.borrow_mut().clear();
        self.monitor_tab_indices.borrow_mut().clear();

        for (index, monitor) in full_monitors.iter().enumerate() {
            let name = monitor.get("NAME").map(py_str);
            let hdr_enabled = name.as_ref().and_then(|n| self.labwc_hdr_overrides.borrow().get(n).copied())
                .unwrap_or(self.labwc_hdr_global.get());
            let desc = monitor.get("DESCRIPTION").map(py_str).unwrap_or_default();
            let primary = monitor.get("NAME").filter(|v| !v.is_null()).map(py_str) == *self.primary_output_name.borrow();
            let widget = MonitorWidget::new(monitor.clone(), Rc::downgrade(self), primary, hdr_enabled, self.labwc_hdr_available,
                self.layout_area.borrow().clone(), self.brightness_client.borrow().clone(), &self.brightness_error.borrow(),
                self.labwc_config_path.borrow().clone(),
                self.auto_scale_enabled.get() && !scale_overrides.contains_key(&desc));
            self.monitor_widgets.borrow_mut().push(widget.clone());
            let mut label_text = monitor_title(monitor);
            if !monitor.get("CURRENT").and_then(|c| c.get("ENABLED")).is_none_or(truthy) {
                label_text = tr!("{display} (Disabled)", display = label_text);
            }
            let label = gtk::Label::new(Some(&label_text));
            self.monitors_notebook.append_page(&widget.root, Some(&label));
            self.monitor_tabs.borrow_mut().insert(desc.clone(), (widget, label));
            self.monitor_tab_indices.borrow_mut().insert(desc, index as u32);
        }

        let enabled: Vec<Value> = full_monitors.iter()
            .filter(|m| m.get("CURRENT").and_then(|c| c.get("ENABLED")).is_none_or(truthy)).cloned().collect();
        let align = cfg.get("ALIGN").map(py_str).unwrap_or_else(|| "TOP".into());
        let arrange = cfg.get("ARRANGE").map(py_str).unwrap_or_else(|| "ROW".into());
        let layout = MonitorLayout::new(&enabled, &self.monitors_notebook, self.monitor_tab_indices.borrow().clone(),
                                        Some(&align), Some(&arrange), None, None, Rc::downgrade(self));
        self.layout_area.replace(Some(layout));
        for monitor in &enabled {
            let desc = monitor.get("DESCRIPTION").map(py_str).unwrap_or_default();
            let widget = self.monitor_tabs.borrow().get(&desc).map(|t| t.0.clone());
            if let Some(widget) = widget {
                let transform = widget.get_transform();
                self.on_transform_change(&desc, transform.as_deref());
            }
        }
    }

    fn on_callback_entry_changed(&self, entry: &gtk::Entry) {
        if self.initializing.get() {
            return;
        }
        let text = entry.text().trim().to_owned();
        if !text.is_empty() {
            let ipc = self.hooks.borrow().ipc_config_operation.clone();
            if let Err(error) = ipc("CFG_SET", &json!({"CALLBACK_CMD": self.get_callback_command()})) {
                eprintln!("{error}");
            }
        }
    }

    pub fn get_callback_command(&self) -> String {
        primary::compose_primary_callback(&self.callback_entry.text(), self.primary_output_name.borrow().as_deref())
    }

    fn on_notify_toggle(&self, enabled: bool) {
        if self.initializing.get() {
            return;
        }
        let current = self.callback_entry.text().trim().to_owned();
        let hooks = self.hooks.borrow().clone();
        let result = if enabled {
            self.callback_entry.set_sensitive(true);
            if current.is_empty() {
                self.callback_entry.set_text(DEFAULT_CALLBACK);
            }
            (hooks.ipc_config_operation)("CFG_SET", &json!({"CALLBACK_CMD": self.get_callback_command()}))
        } else {
            self.callback_entry.set_sensitive(false);
            self.callback_entry.set_text("");
            let callback = self.get_callback_command();
            if !callback.is_empty() {
                (hooks.ipc_config_operation)("CFG_SET", &json!({"CALLBACK_CMD": callback}))
            } else {
                (hooks.ipc_config_operation)("CFG_DEL", &json!({"CALLBACK_CMD": ""}))
            }
        };
        if let Err(error) = result {
            eprintln!("{error}");
            return;
        }
        (hooks.get_config_data)();
    }

    pub fn on_transform_change(&self, desc: &str, transform: Option<&str>) {
        let Some(layout) = self.layout_area.borrow().clone() else { return };
        if layout.shape(desc).is_none() {
            return;
        }
        let transform = transform.map(String::from).unwrap_or_else(|| transform_text(None));
        layout.update_widget_transform(desc, &transform);
        let tab = self.monitor_tabs.borrow().get(desc).map(|t| t.0.clone());
        if let Some(state) = tab.and_then(|t| t.get_desired_state()) {
            let width = (state.width.as_f64().unwrap_or(0.0) * model::SCALE) as i64;
            let height = (state.height.as_f64().unwrap_or(0.0) * model::SCALE) as i64;
            layout.with_meta(desc, |meta| {
                meta.width = width;
                meta.height = height;
            });
        }
        layout.apply_transform_to_shape(desc, &transform);
    }

    pub fn on_monitor_enabled_toggled(self: &Rc<Self>, monitor: &Value, enabled: bool) {
        let desc = monitor.get("DESCRIPTION").map(py_str).unwrap_or_else(|| tr!("Unknown"));
        println!("[TOGGLE] {desc} -> {}", if enabled { "ENABLED" } else { "DISABLED" });
        let label = self.monitor_tabs.borrow().get(&desc).map(|t| t.1.clone());
        if let Some(label) = label {
            label.set_text(&if enabled { desc.clone() } else { tr!("{value1} (Disabled)", value1 = desc.as_str()) });
        }
        let Some(layout) = self.layout_area.borrow().clone() else { return };
        if enabled {
            layout.remove_monitor(&desc);
            layout.add_monitor(monitor);
        } else {
            layout.add_monitor(monitor);
            layout.remove_monitor(&desc);
        }
    }

    pub fn on_primary_toggled(&self, widget: &Rc<MonitorWidget>, active: bool) {
        if self.updating_primary.get() {
            return;
        }
        let Some(output) = widget.name().filter(|n| !n.is_empty()) else { return };
        if active && !widget.enable_switch.is_active() {
            self.updating_primary.set(true);
            widget.primary_switch.set_active(false);
            self.updating_primary.set(false);
            return;
        }
        self.updating_primary.set(true);
        if active {
            self.primary_output_name.replace(Some(output));
            let others = self.monitor_widgets.borrow().clone();
            for other in others {
                if !Rc::ptr_eq(&other, widget) && other.primary_switch.is_active() {
                    other.primary_switch.set_active(false);
                }
            }
        } else if self.primary_output_name.borrow().as_deref() == Some(output.as_str()) {
            self.primary_output_name.replace(None);
        }
        self.updating_primary.set(false);
        self.on_monitor_toggle_changed();
    }

    pub fn apply_primary_output(&self) {
        let apply = self.hooks.borrow().apply_xwayland_primary_output.clone();
        let (success, error) = apply(self.primary_output_name.borrow().as_deref());
        if !success {
            println!("Failed to apply primary output: {}", error.unwrap_or_else(|| "None".into()));
        }
    }

    fn color_error(&self, error: &str) {
        let widgets = self.monitor_widgets.borrow().clone();
        for widget in widgets {
            widget.color_controls.error.replace(error.to_owned());
            widget.color_controls.refresh();
        }
    }

    pub fn save_config(self: &Rc<Self>) {
        let data = ipc::config_data();
        let hooks = self.hooks.borrow().clone();
        let widgets = self.monitor_widgets.borrow().clone();
        let mut color_changes: BTreeMap<String, ColorSettings> = BTreeMap::new();
        for widget in &widgets {
            if widget.color_controls.changed() {
                color_changes.insert(widget.name().unwrap_or_else(|| "None".into()), widget.color_controls.get_state());
            }
        }
        if !color_changes.is_empty() {
            let check = (|| {
                if color_config::read_status().is_empty() {
                    return Err("The running compositor does not support these color controls.".to_owned());
                }
                for value in color_changes.values() {
                    color_config::validate_profile(&value.profile).map_err(|e| e.to_string())?;
                }
                color_config::read_colors(&self.labwc_config_path.borrow()).map_err(|e| e.to_string())?;
                Ok(())
            })();
            if let Err(error) = check {
                self.color_error(&error);
                return;
            }
        }
        let states: Vec<_> = widgets.iter().map(|w| w.get_desired_state()).collect();
        let callback = self.get_callback_command();
        let layout = self.layout_area.borrow().clone();
        let layout_model = layout.as_ref().map(|l| l.model.borrow().clone());
        let plan = model::save_plan(data.get("CFG"), &states, &callback, layout_model.as_ref(),
                                    &self.labwc_managed_hdr_overrides.borrow());
        for index in &plan.enabled_changed {
            if let Some(state) = &states[*index] {
                widgets[*index].initial_enabled.set(state.enabled);
            }
        }
        if let Some(arrange) = plan.cfg.get("ARRANGE").and_then(Value::as_str) {
            self.arrange_mode.replace(arrange.to_owned());
        }

        let mut reconfigure = false;
        let environment = self.labwc_environment_path.borrow().clone();
        let primary_name = self.primary_output_name.borrow().clone();
        let (ok, error, _) = (hooks.write_labwc_primary_xinitrc)(&environment.with_file_name("xinitrc"), primary_name.as_deref());
        if !ok {
            println!("Failed to update Labwc primary-output xinitrc: {}", error.unwrap_or_default());
        }
        let (ok, error, changed) = (hooks.write_labwc_primary_environment)(&environment, primary_name.as_deref());
        if ok {
            reconfigure = changed;
        } else {
            println!("Failed to update Labwc primary-output environment: {}", error.unwrap_or_default());
        }
        if !plan.hdr_changed.is_empty() {
            let (ok, error) = (hooks.write_labwc_hdr_overrides)(&self.labwc_config_path.borrow(), &plan.hdr_overrides);
            if ok {
                self.labwc_managed_hdr_overrides.replace(plan.hdr_overrides.clone());
                self.labwc_hdr_overrides.borrow_mut().extend(plan.hdr_overrides.clone());
                for index in &plan.hdr_changed {
                    widgets[*index].initial_hdr.set(widgets[*index].hdr_switch.is_active());
                }
                reconfigure = true;
            } else {
                println!("Failed to apply Labwc HDR configuration: {}", error.unwrap_or_default());
            }
        }
        let mut transaction = None;
        if !color_changes.is_empty() {
            match color_config::write_colors(&self.labwc_config_path.borrow(), &color_changes) {
                Ok(tx) => transaction = Some(tx),
                Err(error) => {
                    self.color_error(&error.to_string());
                    return;
                }
            }
        }
        if reconfigure || transaction.is_some() {
            let (ok, error) = (hooks.reconfigure_labwc)();
            let mut error = error.unwrap_or_else(|| "None".into());
            if let Some(tx) = &transaction {
                if ok {
                    for widget in &widgets {
                        widget.color_controls.saved();
                    }
                } else {
                    if let Err(rollback) = color_config::rollback(tx) {
                        error = format!("{error} {rollback}");
                    }
                    self.color_error(&error);
                    return;
                }
            }
            if !ok {
                println!("Failed to reload Labwc configuration: {error}");
            }
        }

        let result = (|| {
            if plan.del_cfg.as_object().is_some_and(|m| !m.is_empty()) {
                (hooks.ipc_config_operation)("CFG_DEL", &plan.del_cfg)?;
            }
            (hooks.ipc_config_operation)("CFG_SET", &plan.cfg)?;
            (hooks.ipc_operation)("CFG_WRITE", "")?;
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            // Python raised here: the rest of the Apply did not run.
            eprintln!("Traceback (most recent call last):\n  save_config\n{error}");
            return;
        }
        for widget in &widgets {
            widget.initial_vrr.set(widget.vrr_switch.is_active());
        }
        self.apply_primary_output();
        // Output changes can make XWayland rebuild its RandR topology just
        // after the IPC request completes, so retry once after it has settled.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(500), move || {
            if let Some(this) = weak.upgrade() {
                this.apply_primary_output();
            }
            ControlFlow::Break
        });
        // Refresh only VRR after Apply. Other editors or a temporary compositor
        // state must not turn a stale switch value into a persisted preference.
        if (hooks.get_config_data)() {
            let data = ipc::config_data();
            let heads: Vec<Value> = data.get("STATE").and_then(|s| s.get("HEADS")).and_then(Value::as_array).cloned().unwrap_or_default();
            self.new_configs.set(1);
            for widget in &widgets {
                let name = widget.monitor.borrow().get("NAME").cloned().unwrap_or(Value::Null);
                let head = heads.iter().rev().find(|h| h.get("NAME").cloned().unwrap_or(Value::Null) == name);
                if let Some(head) = head {
                    let vrr = head.get("DESIRED").filter(|d| truthy(d)).and_then(|d| d.get("VRR").cloned())
                        .unwrap_or_else(|| head.get("CURRENT").and_then(|c| c.get("VRR")).cloned().unwrap_or(Value::Bool(false)));
                    widget.initial_vrr.set(truthy(&vrr));
                    widget.vrr_switch.set_active(truthy(&vrr));
                }
            }
        }
        self.new_configs.set(0);
        self.save_btn.set_sensitive(false);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(250), move || {
            if let Some(this) = weak.upgrade() {
                this.reload_layout();
            }
            ControlFlow::Break
        });
    }

    pub fn reload_layout(self: &Rc<Self>) {
        let cfg = ipc::cfg();
        let arrange = cfg.get("ARRANGE").map(py_str).unwrap_or_else(|| "ROW".into());
        let align = cfg.get("ALIGN").map(py_str).unwrap_or_else(|| "TOP".into());
        let order: Vec<String> = obj(&cfg, "ORDER").and_then(Value::as_array).map(|o| o.iter().map(py_str).collect()).unwrap_or_default();
        let transform_overrides = obj(&cfg, "TRANSFORM").and_then(Value::as_object).cloned().unwrap_or_default();
        if self.layout_frame.child().is_some() {
            self.layout_frame.set_child(gtk::Widget::NONE);
        }
        let widgets = self.monitor_widgets.borrow().clone();
        if widgets.is_empty() {
            println!("[ERROR] No monitor widgets found — skipping layout generation.");
            return;
        }
        let desc_to_tab_index: BTreeMap<String, u32> = widgets.iter().enumerate().map(|(i, w)| (w.desc(), i as u32)).collect();
        let monitors: Vec<Value> = widgets.iter().map(|w| w.monitor.borrow().clone()).collect();
        let layout = MonitorLayout::new(&monitors, &self.monitors_notebook, desc_to_tab_index, Some(&align), Some(&arrange),
                                        Some(&order), Some(transform_overrides), Rc::downgrade(self));
        self.layout_frame.set_child(Some(&layout.fixed));
        self.layout_area.replace(Some(layout.clone()));
        for widget in &widgets {
            widget.layout_area.replace(Some(layout.clone()));
        }
        for overlay in self.overlays.take() {
            overlay.destroy();
        }
        // Embedded pages identify displays only on an explicit request.
        if self.embedded {
            self.new_configs.set(0);
            return;
        }
        for meta in layout.entries() {
            let (weak, layout) = (Rc::downgrade(self), Rc::downgrade(&layout));
            glib::idle_add_local_once(move || {
                let (Some(this), Some(layout)) = (weak.upgrade(), layout.upgrade()) else { return };
                if let Some(overlay) = layout.show_overlay(&meta.desc, meta.orig_width, meta.orig_height, &meta.name) {
                    this.overlays.borrow_mut().push(overlay);
                }
            });
        }
        self.new_configs.set(0);
    }

    pub fn overlay_count(&self) -> usize {
        self.overlays.borrow().len()
    }

    /// Global Settings' "Identify displays" keeps the overlays it shows here
    /// (Python appends to `owner.overlays`).
    pub fn push_overlay(&self, overlay: MonitorOverlay) {
        self.overlays.borrow_mut().push(overlay);
    }

    /// Destroy every identify overlay (Python `for o in owner.overlays: o.destroy(); owner.overlays = []`).
    pub fn hide_overlays(&self) {
        for overlay in self.overlays.take() {
            overlay.destroy();
        }
    }
}

/// Python's `MainApp(Gtk.Application)` → `do_activate`.
pub fn run() -> glib::ExitCode {
    let app = gtk::Application::new(Some("com.nobara.WayDisplaysApp"), gio::ApplicationFlags::empty());
    let windows: Rc<RefCell<Vec<Rc<WayDisplaysApp>>>> = Rc::default();
    app.connect_activate(move |app| {
        ipc::get_config_data();
        let window = WayDisplaysApp::new(app, None, false);
        window.window.present();
        let list = windows.clone();
        let weak: Weak<WayDisplaysApp> = Rc::downgrade(&window);
        window.window.connect_destroy(move |_| {
            list.borrow_mut().retain(|w| !Weak::ptr_eq(&Rc::downgrade(w), &weak));
        });
        windows.borrow_mut().push(window);
    });
    app.run()
}
