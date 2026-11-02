//! monitor_ui.py: task-manager style GTK4 UI; all sampling and process
//! operations run off the GTK thread.

use crate::backend::{self, ProcessActionError, Sampler};
use crate::format::{self, num, percent, rate, size, total_rates, DASH};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, pango};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use tessel_core::tr;
use tessel_gtk::app_menu::{install_app_menu, AboutDetails, MenuTarget, Shortcut, ShortcutSection};
use tessel_gtk::utils::{accessible, add_css, headerbar, set_margins};

pub const STYLE: &str = r#"
window.tessel_system_monitor { font-size: 10pt; }
.monitor-nav { padding: 12px 8px; border-right: 1px solid alpha(currentColor,.12); }
.monitor-page { padding: 20px; }
.monitor-heading { font-size: 16pt; font-weight: 600; }
.monitor-summary { padding: 12px 16px; border-radius: 10px; border: 1px solid alpha(currentColor,.14); }
.monitor-value { font-size: 16pt; font-weight: 650; font-feature-settings: "tnum"; }
.monitor-caption { font-size: 9pt; opacity: .75; }
.monitor-toolbar { padding: 10px 0; }
.monitor-status { padding: 8px 20px; font-size: 9pt; }
.monitor-card { padding: 16px; border-radius: 12px; border: 1px solid alpha(currentColor,.15); }
.monitor-card-title { font-size: 11pt; font-weight: 600; }
.monitor-storage { padding: 4px 0; }
.monitor-storage-heading { font-size: 10pt; font-weight: 600; opacity: .7; padding: 4px 16px 2px; }
.monitor-storage-row { padding: 7px 16px; }
.monitor-storage-row + .monitor-storage-row { border-top: 1px solid alpha(currentColor,.08); }
.monitor-storage-row progressbar trough, .monitor-storage-row progressbar progress { min-height: 4px; }
.monitor-metric { font-feature-settings: "tnum"; }
columnview { background: transparent; }
columnview cell { padding: 7px 10px; }
columnview header button { padding: 9px 10px; font-weight: 650; }
.monitor-error { color: #dc6a73; }
"#;

/// Symbolic names first: Breeze lacks several of GTK's and would otherwise
/// substitute its colour application/device art in the navigation.
pub const PAGE_ICONS: &[(&str, &[&str])] = &[
    ("processes", &["view-list-symbolic", "view-list-details-symbolic"]),
    ("performance", &["speedometer-symbolic", "utilities-system-monitor-symbolic"]),
    ("storage", &["drive-harddisk-symbolic"]),
    ("network", &["network-wired-symbolic"]),
    ("gpu", &["monitor-symbolic", "video-display-symbolic"]),
    ("system", &["computer-symbolic"]),
];

pub const PAGES: &[(&str, &str, &str)] = &[
    ("processes", "Processes", "view-list-symbolic"),
    ("performance", "Performance", "utilities-system-monitor-symbolic"),
    ("storage", "Storage", "drive-harddisk-symbolic"),
    ("network", "Network", "network-wired-symbolic"),
    ("gpu", "Graphics", "video-display-symbolic"),
    ("system", "System", "computer-symbolic"),
];

pub const PROCESS_ACTIONS: &[(&str, &str)] = &[("end", "End Task"), ("kill", "Force Quit"), ("suspend", "Suspend"),
    ("resume", "Resume"), ("priority", "Set Priority…"), ("open-location", "Open File Location"), ("details", "Properties")];

pub fn shortcuts() -> Vec<ShortcutSection> {
    vec![
        ("Processes".into(), vec![
            Shortcut::any(&["Delete", "<Control>e"], "End the selected processes"),
            Shortcut::new("<Control>k", "Force quit the selected processes"),
            Shortcut::any(&["Menu", "<Shift>F10"], "Open the process menu"),
            Shortcut::new("Return", "Show process details"),
            Shortcut::new("<Control>a", "Select all processes"),
            Shortcut::new("<Control>f", "Search processes"),
        ]),
        ("View".into(), vec![Shortcut::new("F5", "Refresh now")]),
    ]
}

fn graph_name(key: &str) -> &'static str {
    match key {
        "cpu" => "CPU usage history",
        "memory" => "Memory usage history",
        "disk" => "Disk activity history",
        _ => "Network activity history",
    }
}

// ---------------------------------------------------------------- ProcessRow

mod imp {
    use super::*;
    use std::sync::OnceLock;

    #[derive(Default)]
    pub struct ProcessRow {
        pub data: RefCell<Value>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProcessRow {
        const NAME: &'static str = "SystemMonitorProcessRow";
        type Type = super::ProcessRow;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for ProcessRow {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("changed").run_first().build()])
        }
    }
}

glib::wrapper! {
    pub struct ProcessRow(ObjectSubclass<imp::ProcessRow>);
}

impl ProcessRow {
    pub fn new(data: Value) -> Self {
        let row: Self = glib::Object::new();
        row.imp().data.replace(data);
        row
    }

    pub fn data(&self) -> Value {
        self.imp().data.borrow().clone()
    }

    pub fn get(&self, key: &str) -> Value {
        self.imp().data.borrow().get(key).cloned().unwrap_or(Value::Null)
    }

    /// `(pid, create_time bits)`.
    pub fn identity(&self) -> (i64, u64) {
        let data = self.imp().data.borrow();
        (data["pid"].as_i64().unwrap_or(0), data["create_time"].as_f64().unwrap_or(f64::NAN).to_bits())
    }

    pub fn pid(&self) -> i64 {
        self.get("pid").as_i64().unwrap_or(0)
    }

    pub fn create_time(&self) -> Option<f64> {
        self.get("create_time").as_f64()
    }

    pub fn update(&self, data: Value) {
        if *self.imp().data.borrow() != data {
            self.imp().data.replace(data);
            self.emit_by_name::<()>("changed", &[]);
        }
    }
}

// --------------------------------------------------------------------- Graph

pub struct Graph {
    pub area: gtk::DrawingArea,
    pub values: Rc<RefCell<VecDeque<f64>>>,
    pub maximum: Option<f64>,
    formatter: fn(Option<f64>) -> String,
    pub described: Rc<RefCell<Option<String>>>,
}

impl Graph {
    pub fn new(maximum: Option<f64>, name: &str, formatter: fn(Option<f64>) -> String) -> Rc<Self> {
        let area = gtk::DrawingArea::builder().hexpand(true).accessible_role(gtk::AccessibleRole::Img).build();
        area.set_content_height(130);
        area.add_css_class("profile-check");
        if !name.is_empty() {
            accessible(&area, name);
        }
        let graph = Rc::new(Self { area, values: Rc::new(RefCell::new(VecDeque::with_capacity(60))), maximum, formatter,
                                   described: Rc::new(RefCell::new(None)) });
        let values = graph.values.clone();
        graph.area.set_draw_func(move |area, cr, width, height| draw_graph(area, cr, width as f64, height as f64, &values.borrow(), maximum));
        let weak = Rc::downgrade(&graph);
        graph.area.connect_map(move |_| {
            if let Some(graph) = weak.upgrade() {
                graph.describe();
            }
        });
        graph
    }

    pub fn push(&self, value: Option<f64>) {
        {
            let mut values = self.values.borrow_mut();
            if values.len() == 60 {
                values.pop_front();
            }
            values.push_back(value.unwrap_or(0.0).max(0.0));
        }
        // Screen readers read the description; refresh it only while shown.
        if self.area.is_mapped() {
            self.describe();
        }
        self.area.queue_draw();
    }

    pub fn describe(&self) {
        let values = self.values.borrow();
        let Some(last) = values.back() else { return };
        let average = values.iter().sum::<f64>() / values.len() as f64;
        let peak = values.iter().cloned().fold(f64::MIN, f64::max);
        let f = self.formatter;
        let text = tr!("Current {current}, average {average}, peak {peak} over the last {count} samples",
                       current = f(Some(*last)), average = f(Some(average)), peak = f(Some(peak)), count = values.len() as i64);
        if self.described.borrow().as_deref() != Some(text.as_str()) {
            self.area.update_property(&[gtk::accessible::Property::Description(&text)]);
            self.described.replace(Some(text));
        }
    }
}

#[allow(deprecated)]
fn draw_graph(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: f64, height: f64, values: &VecDeque<f64>, maximum: Option<f64>) {
    let color = area.color();
    let (r, g, b) = (color.red() as f64, color.green() as f64, color.blue() as f64);
    cr.set_source_rgba(r, g, b, 0.12);
    cr.set_line_width(1.0);
    for part in 0..5 {
        let y = 1.0 + (height - 2.0) * part as f64 / 4.0;
        cr.move_to(0.0, y);
        cr.line_to(width, y);
    }
    let _ = cr.stroke();
    if values.is_empty() {
        return;
    }
    let peak = values.iter().cloned().fold(f64::MIN, f64::max);
    let maximum = maximum.filter(|m| *m != 0.0).unwrap_or_else(|| (peak * 1.15).max(1.0));
    let count = values.len();
    let points: Vec<(f64, f64)> = values.iter().enumerate().map(|(i, v)| {
        ((60 - count + i) as f64 * width / 59.0, height - 2.0 - (v / maximum).min(1.0) * (height - 4.0))
    }).collect();
    cr.move_to(points[0].0, height);
    for (x, y) in &points {
        cr.line_to(*x, *y);
    }
    cr.line_to(width, height);
    cr.close_path();
    cr.set_source_rgba(r, g, b, 0.14);
    let _ = cr.fill();
    cr.move_to(points[0].0, points[0].1);
    for (x, y) in &points[1..] {
        cr.line_to(*x, *y);
    }
    cr.set_source_rgba(r, g, b, 1.0);
    cr.set_line_width(2.0);
    let _ = cr.stroke();
}

// ------------------------------------------------------------- test seams

/// Process operations (Python tests patch `monitor_ui.backend.*`).
pub trait ProcessBackend: Send + Sync {
    fn end_process(&self, pid: i64, created: Option<f64>, force: bool) -> Result<bool, ProcessActionError>;
    fn suspend_process(&self, pid: i64, created: Option<f64>) -> Result<bool, ProcessActionError>;
    fn resume_process(&self, pid: i64, created: Option<f64>) -> Result<bool, ProcessActionError>;
    fn set_process_priority(&self, pid: i64, created: Option<f64>, nice: i64) -> Result<bool, ProcessActionError>;
    fn process_details(&self, pid: i64, created: Option<f64>) -> Result<Map<String, Value>, ProcessActionError>;
}

/// The live backend (direct /proc reads, signals to the selected process only).
pub struct LiveBackend(Mutex<crate::procfs::ProcFs>);

impl Default for LiveBackend {
    fn default() -> Self { Self(Mutex::new(crate::procfs::ProcFs::new("/"))) }
}

impl ProcessBackend for LiveBackend {
    fn end_process(&self, pid: i64, created: Option<f64>, force: bool) -> Result<bool, ProcessActionError> {
        backend::end_process(pid, created, force, &*self.0.lock().unwrap())
    }
    fn suspend_process(&self, pid: i64, created: Option<f64>) -> Result<bool, ProcessActionError> {
        backend::suspend_process(pid, created, &*self.0.lock().unwrap())
    }
    fn resume_process(&self, pid: i64, created: Option<f64>) -> Result<bool, ProcessActionError> {
        backend::resume_process(pid, created, &*self.0.lock().unwrap())
    }
    fn set_process_priority(&self, pid: i64, created: Option<f64>, nice: i64) -> Result<bool, ProcessActionError> {
        backend::set_process_priority(pid, created, nice, &*self.0.lock().unwrap())
    }
    fn process_details(&self, pid: i64, created: Option<f64>) -> Result<Map<String, Value>, ProcessActionError> {
        backend::process_details(pid, created, &*self.0.lock().unwrap())
    }
}

/// A confirmation request (Python `Gtk.AlertDialog(message, detail, buttons, …)`).
#[derive(Clone, Debug)]
pub struct Prompt {
    pub message: String,
    pub detail: String,
    pub buttons: Vec<String>,
}

/// `(prompt, answer)`: `answer(Some(index))`, or `None` for a dismissed dialog.
pub type PromptHook = Rc<dyn Fn(&Prompt, Box<dyn FnOnce(Option<i32>)>)>;

// -------------------------------------------------------------------- Window

pub struct Inner {
    pub window: gtk::ApplicationWindow,
    pub header: gtk::HeaderBar,
    pub navigation: gtk::ListBox,
    pub page_rows: HashMap<&'static str, gtk::ListBoxRow>,
    pub stack: gtk::Stack,
    pub metrics: HashMap<&'static str, gtk::Label>,
    pub search: gtk::SearchEntry,
    pub all_users: gtk::CheckButton,
    pub details_button: gtk::Button,
    pub end_button: gtk::Button,
    pub store: gio::ListStore,
    pub filter: gtk::CustomFilter,
    pub sorted: gtk::SortListModel,
    pub selection: gtk::MultiSelection,
    pub table: gtk::ColumnView,
    pub columns: HashMap<&'static str, gtk::ColumnViewColumn>,
    pub graphs: HashMap<&'static str, Rc<Graph>>,
    pub graph_values: HashMap<&'static str, gtk::Label>,
    pub cpu_info: gtk::Label,
    pub core_grid: gtk::FlowBox,
    pub core_labels: RefCell<Vec<gtk::Label>>,
    pub info_boxes: HashMap<&'static str, gtk::Box>,
    pub status: gtk::Label,
    pub interval: gtk::DropDown,
    pub pause: gtk::ToggleButton,
    pub process_model: gio::Menu,
    pub menu_button: RefCell<Option<gtk::MenuButton>>,
    pub context_menu: RefCell<Option<gtk::PopoverMenu>>,
    pub snapshot: RefCell<Option<Rc<Value>>>,
    row_map: RefCell<HashMap<(i64, u64), ProcessRow>>,
    pub closed: Cell<bool>,
    pub paused: Cell<bool>,
    pub busy: Cell<bool>,
    pub timer: RefCell<Option<glib::SourceId>>,
    pub worker_pending: Cell<bool>,
    pub current_user: String,
    css: gtk::CssProvider,
    theme: tessel_gtk::theme::WindowTheme,
    pub sampler: Option<Arc<Mutex<Sampler>>>,
    pub backend: RefCell<Arc<dyn ProcessBackend>>,
    pub prompt: RefCell<Option<PromptHook>>,
    /// Python tests replace `run_worker` with a synchronous call.
    pub synchronous: Cell<bool>,
    /// Python tests replace `request_sample` with a no-op.
    pub sample_requests: Cell<u32>,
    pub reset_samples: Cell<bool>,
}

#[derive(Clone)]
pub struct MonitorWindow(pub Rc<Inner>);

impl std::ops::Deref for MonitorWindow {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

fn label(text: &str, style: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::builder().label(text).xalign(0.0).build();
    if let Some(style) = style {
        widget.add_css_class(style);
    }
    widget
}

fn button(text: &str, callback: impl Fn() + 'static) -> gtk::Button {
    let widget = gtk::Button::with_label(&tr!(text));
    widget.connect_clicked(move |_| callback());
    widget
}

fn text_of(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => format::display(other),
    }
}

fn protected(pid: i64) -> bool {
    pid == 0 || pid == 1 || pid == std::process::id() as i64
}

impl MonitorWindow {
    /// `sampler=None` is the Python tests' `sampler=object(), autostart=False`.
    pub fn new(application: &gtk::Application, sampler: Option<Sampler>, autostart: bool) -> Self {
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("System Monitor")).build();
        window.set_default_size(1120, 760);
        window.set_size_request(820, 540);
        window.add_css_class("tessel-system-monitor");
        // Above the palette stylesheet (APPLICATION + 30) so the chrome rules win.
        let css = add_css(STYLE, Some(&WidgetExt::display(&window)), gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 31);
        let theme = tessel_gtk::theme::attach(&window, None);
        let header = headerbar(&tr!("System Monitor"));
        window.set_titlebar(Some(&header));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.set_child(Some(&root));
        let content = gtk::Box::builder().vexpand(true).build();
        root.append(&content);
        let navigation = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        navigation.add_css_class("monitor-nav");
        tessel_gtk::utils::sidebar_style(&navigation);
        navigation.set_size_request(170, -1);
        content.append(&navigation);
        let mut page_rows = HashMap::new();
        for (key, title, icon_name) in PAGES {
            let row = gtk::ListBoxRow::new();
            unsafe { row.set_data("page-key", *key) };
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let names = PAGE_ICONS.iter().find(|(k, _)| k == key).map(|(_, n)| n.to_vec()).unwrap_or_else(|| vec![*icon_name]);
            let icon = gtk::Image::from_gicon(&gio::ThemedIcon::from_names(&names));
            icon.set_pixel_size(16);
            line.append(&icon);
            line.append(&label(&tr!(title), None));
            row.set_child(Some(&line));
            navigation.append(&row);
            page_rows.insert(*key, row);
        }
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).hhomogeneous(false).vhomogeneous(false).build();
        content.append(&stack);

        // Processes page.
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.add_css_class("monitor-page");
        page.append(&label(&tr!("Processes"), Some("monitor-heading")));
        let summaries = gtk::Box::builder().spacing(10).homogeneous(true).build();
        let mut metrics = HashMap::new();
        for (key, name) in [("cpu", "CPU"), ("memory", "Memory"), ("disk", "Disk I/O"), ("network", "Network")] {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
            card.add_css_class("monitor-summary");
            card.add_css_class("card");
            card.append(&label(&tr!(name), Some("monitor-caption")));
            let value = label(DASH, Some("monitor-value"));
            card.append(&value);
            metrics.insert(key, value);
            summaries.append(&card);
        }
        page.append(&summaries);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        toolbar.add_css_class("monitor-toolbar");
        let search = gtk::SearchEntry::builder().hexpand(true).placeholder_text(tr!("Search name, PID, user or status")).build();
        accessible(&search, &tr!("Search processes"));
        toolbar.append(&search);
        let all_users = gtk::CheckButton::builder().label(tr!("All users")).active(true).build();
        toolbar.append(&all_users);
        let details_button = gtk::Button::with_label(&tr!("Details"));
        toolbar.append(&details_button);
        let end_button = gtk::Button::with_label(&tr!("End Task"));
        end_button.add_css_class("destructive-action");
        toolbar.append(&end_button);
        // Further process actions live in the header's primary menu and the
        // row context menu (right-click, Menu or Shift+F10).
        page.append(&toolbar);
        let store = gio::ListStore::new::<ProcessRow>();
        let current_user = crate::procfs::current_user();
        let (search_weak, users_weak, user) = (search.downgrade(), all_users.downgrade(), current_user.clone());
        let filter = gtk::CustomFilter::new(move |item| {
            let row = item.downcast_ref::<ProcessRow>().unwrap();
            let (Some(search), Some(all_users)) = (search_weak.upgrade(), users_weak.upgrade()) else { return true };
            let data = row.imp().data.borrow();
            if !all_users.is_active() && data.get("user").and_then(Value::as_str) != Some(user.as_str()) {
                return false;
            }
            let query = tessel_core::pyjson::casefold(search.text().trim());
            if query.is_empty() {
                return true;
            }
            let haystack: Vec<String> = ["name", "pid", "user", "status"].iter()
                .map(|k| data.get(*k).map(format::display).unwrap_or_default()).collect();
            tessel_core::pyjson::casefold(&haystack.join(" ")).contains(&query)
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), None::<gtk::Sorter>);
        let selection = gtk::MultiSelection::new(Some(sorted.clone()));
        let table = gtk::ColumnView::builder().model(&selection).show_row_separators(true).show_column_separators(false).build();
        accessible(&table, &tr!("Processes"));
        let mut columns = HashMap::new();
        let column_specs: [(&str, &'static str, fn(&Value) -> String, bool); 8] = [
            ("Name", "name", format::display, true), ("PID", "pid", format::display, false),
            ("CPU", "cpu_percent", |v| percent(v.as_f64()), false), ("Memory", "rss", |v| size(v.as_f64()), false),
            ("Disk read", "read_bytes_per_sec", |v| rate(v.as_f64()), false),
            ("Disk write", "write_bytes_per_sec", |v| rate(v.as_f64()), false),
            ("User", "user", format::display, false), ("Status", "status", format::display, false)];
        for (title, key, formatter, expand) in column_specs {
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let text = matches!(key, "name" | "user" | "status");
                let child = gtk::Label::builder().xalign(if text { 0.0 } else { 1.0 }).ellipsize(pango::EllipsizeMode::End)
                    .max_width_chars(if key == "name" { 34 } else { 20 }).build();
                child.add_css_class("monitor-metric");
                unsafe { child.set_data("list-item", item.downgrade()) };
                item.set_child(Some(&child));
            });
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(row) = item.item().and_downcast::<ProcessRow>() else { return };
                let child = item.child().and_downcast::<gtk::Label>().unwrap();
                let update = move |row: &ProcessRow| {
                    let value = row.get(key);
                    if value.is_null() {
                        child.set_label(DASH);
                        child.set_tooltip_text(Some(&tr!("Unavailable")));
                    } else {
                        child.set_label(&formatter(&value));
                        child.set_tooltip_text(Some(&format::display(&value)));
                    }
                };
                update(&row);
                let handler = row.connect_local("changed", false, move |args| {
                    update(&args[0].get::<ProcessRow>().unwrap());
                    None
                });
                unsafe { item.set_data("binding", (row, handler)) };
            });
            factory.connect_unbind(|_, item| {
                if let Some((row, handler)) = unsafe { item.steal_data::<(ProcessRow, glib::SignalHandlerId)>("binding") } {
                    row.disconnect(handler);
                }
            });
            let column = gtk::ColumnViewColumn::builder().title(tr!(title)).factory(&factory).expand(expand).resizable(true).build();
            column.set_sorter(Some(&gtk::CustomSorter::new(move |a, b| compare(a.downcast_ref().unwrap(), b.downcast_ref().unwrap(), key))));
            table.append_column(&column);
            columns.insert(key, column);
        }
        sorted.set_sorter(table.sorter().as_ref());
        table.sort_by_column(Some(&columns["cpu_percent"]), gtk::SortType::Descending);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).child(&table).build();
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        page.append(&scroll);
        page.append(&label(&tr!("CPU is shown as a share of total processor capacity."), Some("monitor-caption")));
        stack.add_named(&page, Some("processes"));

        // Performance page.
        let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
        page.add_css_class("monitor-page");
        page.append(&label(&tr!("Performance"), Some("monitor-heading")));
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let grid = gtk::Grid::builder().column_spacing(14).row_spacing(14).column_homogeneous(true).build();
        let (mut graphs, mut graph_values) = (HashMap::new(), HashMap::new());
        for (index, (key, title, maximum)) in [("cpu", "CPU", Some(100.0)), ("memory", "Memory", Some(100.0)),
                                               ("disk", "Disk I/O", None), ("network", "Network", None)].into_iter().enumerate() {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
            card.add_css_class("monitor-card");
            card.add_css_class("card");
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let title_label = label(&tr!(title), Some("monitor-card-title"));
            title_label.set_hexpand(true);
            heading.append(&title_label);
            let value = label(DASH, Some("monitor-value"));
            heading.append(&value);
            card.append(&heading);
            let formatter: fn(Option<f64>) -> String = if matches!(key, "cpu" | "memory") { percent } else { rate };
            let graph = Graph::new(maximum, &tr!(graph_name(key)), formatter);
            card.append(&graph.area);
            card.append(&label(&tr!("Last 60 samples"), Some("monitor-caption")));
            graphs.insert(key, graph);
            graph_values.insert(key, value);
            grid.attach(&card, (index % 2) as i32, (index / 2) as i32, 1, 1);
        }
        let cpu_info = label("", Some("muted"));
        cpu_info.set_wrap(true);
        grid.attach(&cpu_info, 0, 2, 2, 1);
        let core_grid = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).homogeneous(true)
            .min_children_per_line(2).max_children_per_line(8).row_spacing(8).column_spacing(8).build();
        grid.attach(&core_grid, 0, 3, 2, 1);
        scroll.set_child(Some(&grid));
        page.append(&scroll);
        stack.add_named(&page, Some("performance"));

        let mut info_boxes = HashMap::new();
        for (key, title, _) in &PAGES[2..] {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
            page.add_css_class("monitor-page");
            page.append(&label(&tr!(title), Some("monitor-heading")));
            let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            let cards = gtk::Box::new(gtk::Orientation::Vertical, 12);
            scroll.set_child(Some(&cards));
            page.append(&scroll);
            stack.add_named(&page, Some(key));
            info_boxes.insert(*key, cards);
        }
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        footer.add_css_class("monitor-status");
        let status = label(&tr!("Reading system information…"), Some("muted"));
        status.set_hexpand(true);
        footer.append(&status);
        footer.append(&label(&tr!("Refresh"), Some("muted")));
        let interval = gtk::DropDown::from_strings(&["0.5 s", "1 s", "2 s", "5 s"]);
        accessible(&interval, &tr!("Refresh interval"));
        interval.set_selected(1);
        footer.append(&interval);
        let pause = gtk::ToggleButton::with_label(&tr!("Pause"));
        footer.append(&pause);
        root.append(&footer);

        let process_model = gio::Menu::new();
        let sampler = sampler.map(|s| Arc::new(Mutex::new(s)));
        let inner = Rc::new(Inner {
            window, header, navigation, page_rows, stack, metrics, search, all_users, details_button, end_button, store,
            filter, sorted, selection, table, columns, graphs, graph_values, cpu_info, core_grid,
            core_labels: RefCell::new(Vec::new()), info_boxes, status, interval, pause, process_model,
            menu_button: RefCell::new(None), context_menu: RefCell::new(None), snapshot: RefCell::new(None),
            row_map: RefCell::new(HashMap::new()), closed: Cell::new(false), paused: Cell::new(false), busy: Cell::new(false),
            timer: RefCell::new(None), worker_pending: Cell::new(false), current_user, css, theme, sampler,
            backend: RefCell::new(Arc::new(LiveBackend::default())), prompt: RefCell::new(None),
            synchronous: Cell::new(false), sample_requests: Cell::new(0), reset_samples: Cell::new(false),
        });
        let this = MonitorWindow(inner);
        this.connect_signals(application);
        this.show_page("processes");
        if autostart {
            this.request_sample();
            this.schedule();
        }
        this
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn with(weak: &Weak<Inner>, f: impl FnOnce(&MonitorWindow)) {
        if let Some(inner) = weak.upgrade() {
            f(&MonitorWindow(inner));
        }
    }

    fn connect_signals(&self, application: &gtk::Application) {
        let w = self.weak();
        self.search.connect_search_changed(move |_| Self::with(&w, |m| m.refilter()));
        let w = self.weak();
        self.all_users.connect_toggled(move |_| Self::with(&w, |m| m.refilter()));
        let w = self.weak();
        self.details_button.connect_clicked(move |_| Self::with(&w, |m| m.show_details()));
        let w = self.weak();
        self.end_button.connect_clicked(move |_| Self::with(&w, |m| m.confirm_action("end")));
        let w = self.weak();
        self.selection.connect_selection_changed(move |_, _, _| Self::with(&w, |m| m.selection_changed()));
        let w = self.weak();
        self.table.connect_activate(move |_, _| Self::with(&w, |m| m.show_details()));
        let context = gtk::GestureClick::builder().button(3).build();
        let w = self.weak();
        context.connect_pressed(move |gesture, _, x, y| Self::with(&w, |m| m.context_pressed(gesture, x, y)));
        self.table.add_controller(context);
        let w = self.weak();
        self.interval.connect_selected_notify(move |_| Self::with(&w, |m| m.schedule()));
        let w = self.weak();
        self.pause.connect_toggled(move |button| Self::with(&w, |m| m.pause_toggled(button)));
        let w = self.weak();
        self.navigation.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let key: &'static str = unsafe { *row.data::<&'static str>("page-key").unwrap().as_ref() };
            Self::with(&w, |m| {
                m.stack.set_visible_child_name(key);
                if m.snapshot.borrow().is_some() {
                    m.render_information(key);
                }
            });
        });
        let keys = gtk::EventControllerKey::new();
        let w = self.weak();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let handled = w.upgrade().map(|inner| MonitorWindow(inner).key(key, modifiers)).unwrap_or(false);
            if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        self.window.add_controller(keys);
        let w = self.weak();
        // Keep the Rc alive for the window's lifetime; cleanup breaks it.
        let strong = RefCell::new(Some(self.0.clone()));
        self.window.connect_close_request(move |_| {
            Self::with(&w, |m| m.cleanup());
            glib::Propagation::Proceed
        });
        let w = self.weak();
        self.window.connect_unrealize(move |_| Self::with(&w, |m| m.cleanup()));
        self.window.connect_destroy(move |_| { strong.borrow_mut().take(); });
        self.install_actions(application);
    }

    fn install_actions(&self, application: &gtk::Application) {
        let menu = gio::Menu::new();
        for (name, title) in PROCESS_ACTIONS {
            let action = gio::SimpleAction::new(name, None);
            let w = self.weak();
            action.connect_activate(move |_, _| Self::with(&w, |m| m.process_action(name)));
            self.window.add_action(&action);
            self.process_model.append(Some(&tr!(title)), Some(&format!("win.{name}")));
        }
        menu.append_section(Some(&tr!("Selected processes")), &self.process_model);
        let view = gio::Menu::new();
        view.append(Some(&tr!("Refresh Now")), Some("win.refresh"));
        let refresh = gio::SimpleAction::new("refresh", None);
        let w = self.weak();
        refresh.connect_activate(move |_, _| Self::with(&w, |m| { m.request_sample(); }));
        self.window.add_action(&refresh);
        menu.append_section(None, &view);
        let details = AboutDetails { name: "System Monitor".into(), icon: "utilities-system-monitor".into(),
                                     comments: Some("Processes, performance and system information".into()), ..Default::default() };
        let button = install_app_menu(application, &self.window, MenuTarget::Model(menu), details, shortcuts(), Some(&self.header), None);
        self.menu_button.replace(Some(button));
        self.selection_changed();
    }

    pub fn process_action(&self, name: &str) {
        match name {
            "details" => self.show_details(),
            "open-location" => self.open_location(),
            other => self.confirm_action(other),
        }
    }

    pub fn refilter(&self) {
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn selected_rows(&self) -> Vec<ProcessRow> {
        let selected = self.selection.selection();
        (0..selected.size() as u32).filter_map(|i| self.sorted.item(selected.nth(i)).and_downcast::<ProcessRow>()).collect()
    }

    pub fn select_positions(&self, positions: &[u32]) {
        self.selection.unselect_all();
        for position in positions {
            self.selection.select_item(*position, false);
        }
    }

    pub fn selection_changed(&self) {
        let rows = self.selected_rows();
        let busy = self.busy.get();
        let allowed = !rows.is_empty() && rows.iter().all(|r| !protected(r.pid())) && !busy;
        let single = rows.len() == 1 && !busy;
        self.details_button.set_sensitive(single);
        self.end_button.set_sensitive(allowed);
        let executable = single && rows[0].get("executable").as_str().is_some_and(|e| !e.is_empty());
        for (name, enabled) in [("end", allowed), ("kill", allowed), ("suspend", allowed), ("resume", allowed),
                                ("priority", allowed && single), ("details", single), ("open-location", executable)] {
            if let Some(action) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }
    }

    pub fn open_location(&self) {
        let rows = self.selected_rows();
        let Some(executable) = (rows.len() == 1).then(|| rows[0].get("executable")).and_then(|e| e.as_str().map(str::to_owned))
            .filter(|e| !e.is_empty()) else { return };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&executable)));
        let w = self.weak();
        launcher.open_containing_folder(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            if let Err(error) = result {
                Self::with(&w, |m| if !m.closed.get() { m.show_error(error.message()) });
            }
        });
    }

    fn row_at(&self, x: f64, y: f64) -> Option<u32> {
        let mut widget = self.table.pick(x, y, gtk::PickFlags::DEFAULT);
        let table: gtk::Widget = self.table.clone().upcast();
        while let Some(current) = widget {
            if current == table {
                break;
            }
            for candidate in [Some(current.clone()), current.first_child()].into_iter().flatten() {
                if let Some(item) = unsafe { candidate.data::<glib::WeakRef<gtk::ListItem>>("list-item") } {
                    if let Some(item) = unsafe { item.as_ref() }.upgrade() {
                        if item.item().is_some() {
                            return Some(item.position());
                        }
                    }
                }
            }
            widget = current.parent();
        }
        None
    }

    fn context_pressed(&self, gesture: &gtk::GestureClick, x: f64, y: f64) {
        let Some(position) = self.row_at(x, y).filter(|p| *p != gtk::INVALID_LIST_POSITION) else { return };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if !self.selection.is_selected(position) {
            self.selection.select_item(position, true);
        }
        self.popup_context_menu(Some((x, y)));
    }

    pub fn popup_context_menu(&self, at: Option<(f64, f64)>) -> bool {
        if self.selected_rows().is_empty() {
            return false;
        }
        if let Some(menu) = self.context_menu.borrow().as_ref() {
            menu.popdown();
        }
        let popup = gtk::PopoverMenu::from_model(Some(&self.process_model));
        popup.set_parent(&self.table);
        popup.set_has_arrow(false);
        if let Some((x, y)) = at {
            popup.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        }
        let w = self.weak();
        popup.connect_closed(move |menu| {
            Self::with(&w, |m| {
                let current = m.context_menu.borrow().clone();
                if current.as_ref() == Some(menu) {
                    m.context_menu.replace(None);
                }
            });
            let menu = menu.clone();
            glib::idle_add_local_once(move || if menu.parent().is_some() { menu.unparent() });
        });
        self.context_menu.replace(Some(popup.clone()));
        popup.popup();
        true
    }

    pub fn show_page(&self, page: &str) {
        if let Some(row) = self.page_rows.get(page) {
            self.navigation.select_row(Some(row));
        }
    }

    pub fn schedule(&self) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        if !self.closed.get() && !self.paused.get() {
            let delay = [500, 1000, 2000, 5000][(self.interval.selected() as usize).min(3)];
            let w = self.weak();
            let id = glib::timeout_add_local(std::time::Duration::from_millis(delay), move || {
                let Some(inner) = w.upgrade() else { return glib::ControlFlow::Break };
                if MonitorWindow(inner).request_sample() { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
            });
            self.timer.replace(Some(id));
        }
    }

    pub fn timer_active(&self) -> bool {
        self.timer.borrow().is_some()
    }

    fn pause_toggled(&self, widget: &gtk::ToggleButton) {
        self.paused.set(widget.is_active());
        widget.set_label(&if self.paused.get() { tr!("Resume") } else { tr!("Pause") });
        self.schedule();
        if self.paused.get() {
            self.status.set_label(&tr!("Paused — displayed values are frozen"));
        } else {
            self.request_sample();
        }
    }

    /// Run `work` off the GTK thread; `callback` runs on it unless the window closed.
    pub fn run_worker<T: Send + 'static>(&self, work: impl FnOnce() -> Result<T, String> + Send + 'static,
                                         callback: impl FnOnce(&MonitorWindow, Option<T>, Option<String>) + 'static) -> bool {
        if self.closed.get() {
            return false;
        }
        if self.synchronous.get() {
            let (value, error) = match work() { Ok(v) => (Some(v), None), Err(e) => (None, Some(e)) };
            callback(self, value, error);
            return true;
        }
        let w = self.weak();
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(work).await;
            let (value, error) = match result {
                Ok(Ok(v)) => (Some(v), None),
                Ok(Err(e)) => (None, Some(e)),
                Err(_) => (None, Some("worker failed".to_owned())),
            };
            Self::with(&w, |m| if !m.closed.get() { callback(m, value, error) });
        });
        true
    }

    pub fn request_sample(&self) -> bool {
        if self.closed.get() {
            return false;
        }
        self.sample_requests.set(self.sample_requests.get() + 1);
        if !self.paused.get() && !self.worker_pending.get() {
            if let Some(sampler) = self.sampler.clone() {
                self.worker_pending.set(true);
                self.run_worker(move || Ok(sampler.lock().map_err(|e| e.to_string())?.sample()),
                                |m, value, error| m.sampled(value, error));
            }
        }
        true
    }

    pub fn sampled(&self, value: Option<Value>, error: Option<String>) {
        self.worker_pending.set(false);
        if let Some(error) = error {
            self.status.set_label(&(tr!("Could not refresh: ") + &error));
        } else if !self.paused.get() {
            if let Some(value) = value {
                self.render_snapshot(value);
            }
        }
    }

    pub fn render_snapshot(&self, data: Value) {
        let data = Rc::new(data);
        self.snapshot.replace(Some(data.clone()));
        let (cpu, memory, disks, network) = (&data["cpu"], &data["memory"], &data["disks"], &data["network"]);
        let values = [
            ("cpu", num(cpu.get("percent"))), ("memory", num(memory.get("percent"))),
            ("disk", total_rates(&[num(disks.get("read_bytes_per_sec")), num(disks.get("write_bytes_per_sec"))])),
            ("network", total_rates(&[num(network.get("recv_bytes_per_sec")), num(network.get("sent_bytes_per_sec"))])),
        ];
        for (key, value) in values {
            let text = if matches!(key, "cpu" | "memory") { percent(value) } else { rate(value) };
            self.metrics[key].set_label(&text);
            self.graph_values[key].set_label(&text);
            self.graphs[key].push(value);
        }
        let identities: HashSet<(i64, u64)> = self.selected_rows().iter().map(ProcessRow::identity).collect();
        let mut rows = Vec::new();
        let mut active = HashMap::new();
        {
            let map = self.row_map.borrow();
            for entry in data["processes"].as_array().cloned().unwrap_or_default() {
                let key = (entry["pid"].as_i64().unwrap_or(0), entry["create_time"].as_f64().unwrap_or(f64::NAN).to_bits());
                let row = match map.get(&key) {
                    Some(row) => { row.update(entry); row.clone() }
                    None => ProcessRow::new(entry),
                };
                rows.push(row.clone());
                active.insert(key, row);
            }
        }
        self.row_map.replace(active);
        self.store.splice(0, self.store.n_items(), &rows);
        if let Some(sorter) = self.table.sorter() {
            sorter.changed(gtk::SorterChange::Different);
        }
        self.filter.changed(gtk::FilterChange::Different);
        if !identities.is_empty() {
            // Rebind by (PID, start time), so PID reuse never inherits a selection.
            let positions: Vec<u32> = (0..self.sorted.n_items()).filter(|i| self.sorted.item(*i)
                .and_downcast::<ProcessRow>().is_some_and(|r| identities.contains(&r.identity()))).collect();
            self.select_positions(&positions);
        }
        self.selection_changed();
        let per_core: Vec<Option<f64>> = cpu.get("per_core").and_then(Value::as_array).map(|a| a.iter().map(Value::as_f64).collect()).unwrap_or_default();
        if self.core_labels.borrow().len() != per_core.len() {
            tessel_gtk::utils::remove_children(&self.core_grid);
            let mut labels = self.core_labels.borrow_mut();
            labels.clear();
            for _ in &per_core {
                let widget = label("", Some("monitor-metric"));
                set_margins(&widget, 8);
                self.core_grid.append(&widget);
                labels.push(widget);
            }
        }
        for (index, (widget, value)) in self.core_labels.borrow().iter().zip(&per_core).enumerate() {
            widget.set_label(&format!("CPU {}  {}", index + 1, percent(*value)));
        }
        let frequency = cpu.get("frequency").and_then(|f| f.get("current")).and_then(Value::as_f64).filter(|f| *f != 0.0);
        let cores = |key: &str| match cpu.get(key) {
            Some(v) if !v.is_null() && v.as_f64() != Some(0.0) => format::display(v),
            _ => DASH.to_owned(),
        };
        let mut info = tr!("{physical} cores · {logical} logical processors", physical = cores("physical_cores"), logical = cores("logical_cores"));
        if let Some(frequency) = frequency {
            info += &format!(" · {:.2} GHz", frequency / 1000.0);
        }
        info += &format!("\n{} / {} ", size(num(memory.get("used"))), size(num(memory.get("total"))));
        info += &tr!("memory in use");
        self.cpu_info.set_label(&info);
        let time = glib::DateTime::now_local().and_then(|t| t.format("%H:%M:%S")).map(|t| t.to_string()).unwrap_or_default();
        self.status.set_label(&tr!("{count} processes · Updated {time}", count = rows.len() as i64, time = time));
        let warnings: Vec<String> = data.get("warnings").and_then(Value::as_array)
            .map(|w| w.iter().map(text_of).collect()).unwrap_or_default();
        self.status.set_tooltip_text(Some(&warnings.join("\n")));
        if let Some(page) = self.stack.visible_child_name() {
            self.render_information(&page);
        }
    }

    pub fn render_information(&self, key: &str) {
        let Some(container) = self.info_boxes.get(key) else { return };
        let Some(data) = self.snapshot.borrow().clone() else { return };
        tessel_gtk::utils::remove_children(container);
        match key {
            "storage" => self.storage_list(container, &data["disks"]),
            "network" => {
                for interface in data["network"].get("interfaces").and_then(Value::as_array).cloned().unwrap_or_default() {
                    let up = interface.get("up").cloned().unwrap_or(Value::Null);
                    let status = match up { Value::Null => tr!("Unavailable"), Value::Bool(true) => tr!("Connected"),
                                            ref v if tessel_core::pyjson::truthy(v) => tr!("Connected"), _ => tr!("Disconnected") };
                    let speed = interface.get("speed_mbps").filter(|s| s.as_f64().unwrap_or(0.0) > 0.0)
                        .map(|s| format!("{} Mbps", format::display(s))).unwrap_or_else(|| DASH.into());
                    info_card(container, &text_of(&interface["name"]), &[
                        ("Status", status), ("Receive", rate(num(interface.get("recv_bytes_per_sec")))),
                        ("Send", rate(num(interface.get("sent_bytes_per_sec")))), ("Link speed", speed)], None);
                }
            }
            "gpu" => {
                let gpus = data["gpus"].as_array().cloned().unwrap_or_default();
                for gpu in &gpus {
                    let temperature = num(gpu.get("temp_c")).map(|t| format!("{t:.0} °C")).unwrap_or_else(|| DASH.into());
                    let name = gpu.get("name").map(text_of).unwrap_or_else(|| "Graphics".into());
                    info_card(container, &name, &[
                        ("Utilization", percent(num(gpu.get("utilization_percent")))),
                        ("Video memory used", size(num(gpu.get("memory_used")))),
                        ("Video memory total", size(num(gpu.get("memory_total")))),
                        ("Temperature", temperature)], num(gpu.get("utilization_percent")));
                }
                if gpus.is_empty() {
                    let message = label(&tr!("Graphics telemetry is not available from the installed driver."), Some("muted"));
                    message.set_wrap(true);
                    container.append(&message);
                }
            }
            "system" => {
                let system = &data["system"];
                let uptime = num(system.get("uptime"));
                let mut seconds = uptime.unwrap_or(0.0) as i64;
                let days = seconds.div_euclid(86400);
                seconds = seconds.rem_euclid(86400);
                let hours = seconds.div_euclid(3600);
                seconds = seconds.rem_euclid(3600);
                let minutes = seconds / 60;
                let get = |k: &str| system.get(k).map(text_of).unwrap_or_else(|| DASH.into());
                let title = system.get("hostname").map(text_of).unwrap_or_else(|| tr!("This computer"));
                info_card(container, &title, &[
                    ("Operating system", get("os")), ("Kernel", get("kernel")),
                    ("Uptime", if uptime.is_some() { format!("{days}d {hours}h {minutes}m") } else { DASH.into() }),
                    ("Memory", size(num(data["memory"].get("total")))),
                    ("Available memory", size(num(data["memory"].get("available")))),
                    ("Swap used", size(num(data["swap"].get("used")))),
                    ("Swap total", size(num(data["swap"].get("total"))))], None);
            }
            _ => {}
        }
    }

    /// Compact rows: disk activity, then mounted volumes with usage.
    fn storage_list(&self, parent: &gtk::Box, disks: &Value) {
        let devices = format::storage_devices(disks.get("devices").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]));
        let volumes = format::storage_volumes(disks.get("partitions").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]));
        for (title, items, is_devices) in [(tr!("Disks"), &devices, true), (tr!("Volumes"), &volumes, false)] {
            if items.is_empty() {
                continue;
            }
            let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
            card.add_css_class("card");
            card.add_css_class("monitor-storage");
            card.append(&label(&title, Some("monitor-storage-heading")));
            for item in items {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
                row.add_css_class("monitor-storage-row");
                if is_devices {
                    let name = label(&item.get("name").map(text_of).unwrap_or_else(|| tr!("Disk")), None);
                    name.set_selectable(true);
                    name.set_hexpand(true);
                    row.append(&name);
                    for (name, key) in [("Read", "read_bytes_per_sec"), ("Write", "write_bytes_per_sec")] {
                        row.append(&label(&tr!(name), Some("muted")));
                        let value = label(&rate(num(item.get(key))), Some("monitor-metric"));
                        value.set_width_chars(11);
                        value.set_xalign(1.0);
                        row.append(&value);
                    }
                } else {
                    let names = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
                    let mount = item.get("mountpoint").map(text_of).filter(|m| !m.is_empty())
                        .unwrap_or_else(|| item.get("device").map(text_of).unwrap_or_else(|| tr!("Volume")));
                    let first = label(&mount, None);
                    first.set_selectable(true);
                    first.set_ellipsize(pango::EllipsizeMode::Middle);
                    names.append(&first);
                    let parts: Vec<String> = ["device", "fstype"].iter().map(|k| item.get(*k).map(text_of).unwrap_or_default())
                        .filter(|p| !p.is_empty()).collect();
                    let second = label(&parts.join(" · "), Some("muted"));
                    second.set_ellipsize(pango::EllipsizeMode::Middle);
                    names.append(&second);
                    row.append(&names);
                    let usage = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).valign(gtk::Align::Center).build();
                    usage.set_size_request(180, -1);
                    let total = num(item.get("total")).filter(|t| *t != 0.0);
                    let text = if total.is_some() {
                        tr!("{used} of {total}", used = size(num(item.get("used"))), total = size(total))
                    } else {
                        item.get("error").map(text_of).filter(|e| !e.is_empty()).unwrap_or_else(|| DASH.into())
                    };
                    let value = label(&text, Some("monitor-metric"));
                    value.set_xalign(1.0);
                    usage.append(&value);
                    if let Some(percent) = num(item.get("percent")) {
                        usage.append(&gtk::ProgressBar::builder().fraction((percent / 100.0).clamp(0.0, 1.0)).build());
                    }
                    row.append(&usage);
                }
                card.append(&row);
            }
            parent.append(&card);
        }
    }

    pub fn show_details(&self) {
        if self.closed.get() {
            return;
        }
        let rows = self.selected_rows();
        if rows.len() != 1 || self.busy.get() {
            return;
        }
        let (pid, created) = (rows[0].pid(), rows[0].create_time());
        self.busy.set(true);
        self.selection_changed();
        let backend = self.backend.borrow().clone();
        self.run_worker(move || backend.process_details(pid, created).map_err(|e| e.to_string()), move |m, data, error| {
            m.busy.set(false);
            m.selection_changed();
            if let Some(error) = error {
                m.show_error(&error);
                return;
            }
            let data = data.unwrap_or_default();
            let dialog = gtk::Window::builder().title(tr!("Process Details")).modal(true).destroy_with_parent(true)
                .transient_for(&m.window).build();
            if let Some(app) = m.window.application() {
                dialog.set_application(Some(&app));
            }
            dialog.set_default_size(580, 500);
            dialog.set_titlebar(Some(&headerbar(&tr!("Process Details"))));
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
            set_margins(&content, 18);
            let mut details = Vec::new();
            for (key, value) in &data {
                let text = match value {
                    Value::Array(items) if key == "command_line" => {
                        let words: Vec<String> = items.iter().map(format::display).collect();
                        tessel_core::shell::shell_join(&words)
                    }
                    Value::Null => DASH.into(),
                    other => format::display(other),
                };
                details.push((format::capitalize(&key.replace('_', " ")), text));
            }
            let title = data.get("name").map(text_of).unwrap_or_else(|| pid.to_string());
            let entries: Vec<(&str, String)> = details.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            info_card(&content, &title, &entries, None);
            scroll.set_child(Some(&content));
            dialog.set_child(Some(&scroll));
            tessel_gtk::theme::attach(&dialog, None);
            dialog.present();
        });
    }

    pub fn confirm_action(&self, action: &str) {
        if self.closed.get() || self.busy.get() {
            return;
        }
        let rows: Vec<ProcessRow> = self.selected_rows().into_iter().filter(|r| !protected(r.pid())).collect();
        if rows.is_empty() {
            return;
        }
        // Bind confirmation to these processes, not a future selection/PID reuse.
        let targets: Vec<(i64, Option<f64>, String)> = rows.iter().map(|r| {
            let name = r.get("name").as_str().filter(|n| !n.is_empty()).map(str::to_owned).unwrap_or_else(|| r.pid().to_string());
            (r.pid(), r.create_time(), name)
        }).collect();
        if action == "priority" {
            if targets.len() == 1 {
                let (pid, created, name) = targets[0].clone();
                self.priority_dialog(pid, created, &name);
            }
            return;
        }
        let (title, detail, plural) = match action {
            "end" => ("End Task", "The process will be asked to terminate. Unsaved work may be lost.", "End {count} processes?"),
            "kill" => ("Force Quit", "The process will stop immediately. Unsaved work will be lost.", "Force quit {count} processes?"),
            "suspend" => ("Suspend", "The process will pause until you resume it.", "Suspend {count} processes?"),
            "resume" => ("Resume", "The suspended process will continue running.", "Resume {count} processes?"),
            _ => return,
        };
        let (message, detail) = if targets.len() == 1 {
            let (pid, _, name) = &targets[0];
            (tr!(title) + &format!(" — {name} ({pid})"), tr!(detail))
        } else {
            let mut listed: Vec<String> = targets.iter().take(12).map(|(pid, _, name)| format!("{name} ({pid})")).collect();
            if targets.len() > 12 {
                listed.push(tr!("…and {count} more", count = (targets.len() - 12) as i64));
            }
            (tr!(plural, count = targets.len() as i64), tr!(detail) + "\n\n" + &listed.join("\n"))
        };
        let prompt = Prompt { message, detail, buttons: vec![tr!("Cancel"), tr!(title)] };
        let w = self.weak();
        let action = action.to_owned();
        let answer: Box<dyn FnOnce(Option<i32>)> = Box::new(move |choice: Option<i32>| {
            let Some(inner) = w.upgrade() else { return };
            let m = MonitorWindow(inner);
            if choice != Some(1) || m.closed.get() {
                return;
            }
            let backend = m.backend.borrow().clone();
            m.perform(move || {
                let mut failures = Vec::new();
                for (pid, created, name) in &targets {
                    let result = match action.as_str() {
                        "end" => backend.end_process(*pid, *created, false),
                        "kill" => backend.end_process(*pid, *created, true),
                        "suspend" => backend.suspend_process(*pid, *created),
                        _ => backend.resume_process(*pid, *created),
                    };
                    if let Err(error) = result {
                        failures.push(format!("{name} ({pid}): {error}"));
                    }
                }
                if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) }
            });
        });
        let hook = self.prompt.borrow().clone();
        match hook {
            Some(hook) => hook(&prompt, answer),
            None => {
                let buttons: Vec<&str> = prompt.buttons.iter().map(String::as_str).collect();
                let dialog = gtk::AlertDialog::builder().message(&prompt.message).detail(&prompt.detail).buttons(buttons)
                    .cancel_button(0).default_button(0).build();
                dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |result| answer(result.ok()));
            }
        }
    }

    pub fn priority_dialog(&self, pid: i64, created: Option<f64>, name: &str) -> Option<gtk::Window> {
        if self.closed.get() {
            return None;
        }
        let dialog = gtk::Window::builder().transient_for(&self.window).modal(true).title(tr!("Process Priority"))
            .destroy_with_parent(true).build();
        if let Some(app) = self.window.application() {
            dialog.set_application(Some(&app));
        }
        dialog.set_titlebar(Some(&headerbar(&tr!("Process Priority"))));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        set_margins(&content, 20);
        content.append(&label(&format!("{name} ({pid})"), None));
        let hint = label(&tr!("Lower values give higher priority. Raising priority may require privileges."), None);
        hint.set_wrap(true);
        hint.set_max_width_chars(48);
        content.append(&hint);
        let spin = gtk::SpinButton::with_range(-20.0, 19.0, 1.0);
        spin.set_value(0.0);
        content.append(&spin);
        let actions = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let weak_dialog = dialog.downgrade();
        actions.append(&button("Cancel", move || if let Some(d) = weak_dialog.upgrade() { d.close() }));
        let (w, weak_dialog) = (self.weak(), dialog.downgrade());
        actions.append(&button("Apply", move || {
            let value = spin.value_as_int() as i64;
            if let Some(d) = weak_dialog.upgrade() {
                d.close();
            }
            Self::with(&w, |m| {
                let backend = m.backend.borrow().clone();
                m.perform(move || backend.set_process_priority(pid, created, value).map(|_| ()).map_err(|e| e.to_string()));
            });
        }));
        content.append(&actions);
        dialog.set_child(Some(&content));
        tessel_gtk::theme::attach(&dialog, None);
        dialog.present();
        Some(dialog)
    }

    pub fn perform(&self, action: impl FnOnce() -> Result<(), String> + Send + 'static) -> bool {
        if self.closed.get() || self.busy.get() {
            return false;
        }
        self.busy.set(true);
        self.selection_changed();
        self.run_worker(action, |m, _, error| {
            m.busy.set(false);
            m.selection_changed();
            match error {
                Some(error) => m.show_error(&error),
                None => {
                    m.status.set_label(&tr!("Process action completed"));
                    m.request_sample();
                }
            }
        })
    }

    pub fn show_error(&self, message: &str) {
        gtk::AlertDialog::builder().message(tr!("Unable to complete the request")).detail(message).build().show(Some(&self.window));
    }

    /// The window's key handler (`_key`).
    pub fn key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        if control && (key == gdk::Key::f || key == gdk::Key::F) {
            self.show_page("processes");
            self.search.grab_focus();
            return true;
        }
        if key == gdk::Key::F5 {
            self.request_sample();
            return true;
        }
        let editing = gtk::prelude::GtkWindowExt::focus(&self.window).is_some_and(|f| f.is::<gtk::Editable>());
        if self.stack.visible_child_name().as_deref() != Some("processes") || editing {
            return false;
        }
        if (control && (key == gdk::Key::e || key == gdk::Key::E)) || ((key == gdk::Key::Delete || key == gdk::Key::KP_Delete) && !control) {
            self.confirm_action("end");
            return true;
        }
        if control && (key == gdk::Key::k || key == gdk::Key::K) {
            self.confirm_action("kill");
            return true;
        }
        if key == gdk::Key::Menu || (key == gdk::Key::F10 && modifiers.contains(gdk::ModifierType::SHIFT_MASK)) {
            return self.popup_context_menu(None);
        }
        false
    }

    pub fn cleanup(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        // Close native dialogs before the parent's surface is unrealized.
        if let Some(application) = self.window.application() {
            let me: gtk::Window = self.window.clone().upcast();
            for child in application.windows() {
                if child != me && child.transient_for().as_ref() == Some(&me) {
                    child.destroy();
                }
            }
        }
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        self.theme.close();
        tessel_gtk::utils::remove_css(&WidgetExt::display(&self.window), &self.css);
    }

    pub fn close(&self) {
        self.window.close();
    }
}

fn compare(first: &ProcessRow, second: &ProcessRow, key: &str) -> gtk::Ordering {
    let (a, b) = (first.get(key), second.get(key));
    let ordering = if matches!(key, "name" | "user" | "status") {
        let text = |v: &Value| if tessel_core::pyjson::truthy(v) { tessel_core::pyjson::casefold(&format::display(v)) } else { String::new() };
        text(&a).cmp(&text(&b))
    } else {
        let number = |v: &Value| v.as_f64().unwrap_or(0.0);
        number(&a).partial_cmp(&number(&b)).unwrap_or(std::cmp::Ordering::Equal)
    };
    ordering.into()
}

pub fn info_card(parent: &gtk::Box, title: &str, entries: &[(&str, String)], progress: Option<f64>) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("monitor-card");
    card.add_css_class("card");
    let heading = label(title, Some("monitor-card-title"));
    heading.set_selectable(true);
    heading.set_wrap(true);
    card.append(&heading);
    let grid = gtk::Grid::builder().column_spacing(24).row_spacing(8).build();
    for (index, (name, value)) in entries.iter().enumerate() {
        grid.attach(&label(&tr!(*name), Some("muted")), 0, index as i32, 1, 1);
        let text = label(value, None);
        text.set_selectable(true);
        text.set_wrap(true);
        text.set_hexpand(true);
        grid.attach(&text, 1, index as i32, 1, 1);
    }
    card.append(&grid);
    if let Some(progress) = progress {
        card.append(&gtk::ProgressBar::builder().fraction((progress / 100.0).clamp(0.0, 1.0)).build());
    }
    parent.append(&card);
}
