//! Desktop Behavior "Snap layouts": per-screen grids of areas that windows
//! snap into. `$XDG_CONFIG_HOME/tessel/snap-layouts.json` is the source of
//! truth; rc.xml gets a managed `<regions>` block of labwc regions limited to
//! one output each, regenerated from it ([`sync_rc`]).
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use serde_json::{json, Map, Value};
use tessel_core::tr;
use tessel_displays::compat::fsx::{self, Replace};
use tessel_displays::labwc::{html_escape, labwc_config_path, lock_rc, rc_lock_path, reconfigure_labwc, BlockWrite};

pub const SNAP_BEGIN: &str = "<!-- tessel snap layouts begin -->";
pub const SNAP_END: &str = "<!-- tessel snap layouts end -->";
pub const MAX_COLUMNS: usize = 6;
pub const MAX_ROWS: usize = 4;
/// The smallest area, in percent of the screen.
pub const MIN_AREA: u32 = 10;
/// Keyboard divider moves are applied this long after the last key.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

// ------------------------------------------------------------------- model

/// `count` integer percentages summing to 100; the remainder goes to the
/// middle ones (33/34/33).
pub fn split(count: usize) -> Vec<u32> {
    let count = count.max(1);
    let base = 100 / count as u32;
    let extra = (100 % count as u32) as usize;
    let start = (count - extra) / 2;
    (0..count).map(|i| base + u32::from(i >= start && i < start + extra)).collect()
}

fn valid_sizes(sizes: &[u32], max: usize) -> bool {
    (1..=max).contains(&sizes.len()) && sizes.iter().all(|s| *s >= MIN_AREA || sizes.len() == 1)
        && sizes.iter().sum::<u32>() == 100
}

/// Move the divider after `sizes[index]` by `delta` percent; both areas
/// next to it stay at least [`MIN_AREA`]. False when nothing moved.
pub fn move_divider(sizes: &mut [u32], index: usize, delta: i32) -> bool {
    if index + 1 >= sizes.len() {
        return false;
    }
    let (before, after) = (sizes[index] as i32, sizes[index + 1] as i32);
    let (low, high) = (MIN_AREA as i32 - before, after - MIN_AREA as i32);
    if low > high {
        return false;
    }
    let delta = delta.clamp(low, high);
    if delta == 0 {
        return false;
    }
    sizes[index] = (before + delta) as u32;
    sizes[index + 1] = (after - delta) as u32;
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Columns,
    Rows,
}

/// One screen's areas: column widths and row heights in percent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub columns: Vec<u32>,
    pub rows: Vec<u32>,
}

impl Default for Layout {
    fn default() -> Self { Self::equal(1, 1) }
}

impl Layout {
    pub fn equal(columns: usize, rows: usize) -> Self {
        Self { columns: split(columns.clamp(1, MAX_COLUMNS)), rows: split(rows.clamp(1, MAX_ROWS)) }
    }

    /// One area: the screen keeps labwc's halves at the edges.
    pub fn is_default(&self) -> bool { self.columns.len() == 1 && self.rows.len() == 1 }

    pub fn valid(&self) -> bool { valid_sizes(&self.columns, MAX_COLUMNS) && valid_sizes(&self.rows, MAX_ROWS) }

    pub fn sizes(&self, axis: Axis) -> &[u32] {
        match axis { Axis::Columns => &self.columns, Axis::Rows => &self.rows }
    }

    pub fn sizes_mut(&mut self, axis: Axis) -> &mut Vec<u32> {
        match axis { Axis::Columns => &mut self.columns, Axis::Rows => &mut self.rows }
    }

    /// A saved `{"columns": […], "rows": […]}`; None when it is not valid.
    pub fn from_value(value: &Value) -> Option<Self> {
        let sizes = |key: &str| -> Option<Vec<u32>> {
            value.get(key)?.as_array()?.iter().map(|v| v.as_u64().filter(|n| *n <= 100).map(|n| n as u32)).collect()
        };
        let layout = Self { columns: sizes("columns")?, rows: sizes("rows")? };
        layout.valid().then_some(layout)
    }

    /// `<region …/>` lines for `output`, column by column.
    pub fn regions(&self, output: &str) -> Vec<String> {
        let name = html_escape(output);
        let mut lines = Vec::new();
        let mut x = 0;
        for (column, width) in self.columns.iter().enumerate() {
            let mut y = 0;
            for (row, height) in self.rows.iter().enumerate() {
                lines.push(format!("<region name=\"tessel-{}-{}\" output=\"{name}\" x=\"{x}%\" y=\"{y}%\" width=\"{width}%\" height=\"{height}%\"/>",
                                   column + 1, row + 1));
                y += height;
            }
            x += width;
        }
        lines
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Halves,
    Columns2,
    Columns3,
    Columns4,
    Rows2,
    Rows3,
    Grid2x2,
    Grid3x2,
    Custom,
}

/// The "Layout" drop-down, in order.
pub const PRESETS: [Preset; 9] = [Preset::Halves, Preset::Columns2, Preset::Columns3, Preset::Columns4, Preset::Rows2,
                                  Preset::Rows3, Preset::Grid2x2, Preset::Grid3x2, Preset::Custom];

impl Preset {
    pub fn label(self) -> String {
        match self {
            Self::Halves => tr!("Halves (default)"),
            Self::Columns2 => tr!("2 columns"),
            Self::Columns3 => tr!("3 columns"),
            Self::Columns4 => tr!("4 columns"),
            Self::Rows2 => tr!("2 rows"),
            Self::Rows3 => tr!("3 rows"),
            Self::Grid2x2 => tr!("2 × 2 grid"),
            Self::Grid3x2 => tr!("3 × 2 grid"),
            Self::Custom => tr!("Custom"),
        }
    }

    /// (columns, rows); None for Custom.
    pub fn shape(self) -> Option<(usize, usize)> {
        Some(match self {
            Self::Halves => (1, 1),
            Self::Columns2 => (2, 1),
            Self::Columns3 => (3, 1),
            Self::Columns4 => (4, 1),
            Self::Rows2 => (1, 2),
            Self::Rows3 => (1, 3),
            Self::Grid2x2 => (2, 2),
            Self::Grid3x2 => (3, 2),
            Self::Custom => return None,
        })
    }

    pub fn layout(self) -> Option<Layout> { self.shape().map(|(c, r)| Layout::equal(c, r)) }

    /// The preset that gives exactly `layout`, else Custom.
    pub fn of(layout: &Layout) -> Self {
        PRESETS.into_iter().find(|p| p.layout().as_ref() == Some(layout)).unwrap_or(Self::Custom)
    }
}

/// `snap-layouts.json`: unknown keys are kept, invalid entries read as the default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapLayouts {
    pub data: Map<String, Value>,
    /// Valid, non-default layouts by connector.
    pub outputs: BTreeMap<String, Layout>,
}

impl SnapLayouts {
    pub fn parse(text: &str) -> Self {
        let data = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(data)) => data,
            _ => Map::new(),
        };
        let outputs = data.get("outputs").and_then(Value::as_object).map(|outputs| {
            outputs.iter().filter(|(name, _)| !name.is_empty())
                .filter_map(|(name, value)| Some((name.clone(), Layout::from_value(value)?)))
                .filter(|(_, layout)| !layout.is_default()).collect()
        }).unwrap_or_default();
        Self { data, outputs }
    }

    /// A missing or unreadable file is no layouts.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map(|text| Self::parse(&text)).unwrap_or_default()
    }

    pub fn layout(&self, output: &str) -> Layout { self.outputs.get(output).cloned().unwrap_or_default() }

    /// Save `layout` for `output`; the default (or an invalid one) removes it.
    pub fn set(&mut self, output: &str, layout: &Layout) {
        if !self.data.get("outputs").is_some_and(Value::is_object) {
            self.data.insert("outputs".into(), json!({}));
        }
        let Some(Value::Object(outputs)) = self.data.get_mut("outputs") else { unreachable!() };
        if layout.is_default() || !layout.valid() || output.is_empty() {
            outputs.shift_remove(output);
            self.outputs.remove(output);
            return;
        }
        let entry = outputs.entry(output).or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        entry["columns"] = json!(layout.columns);
        entry["rows"] = json!(layout.rows);
        self.outputs.insert(output.to_owned(), layout.clone());
    }

    pub fn to_json(&self) -> String {
        let mut data = Map::new();
        if !self.data.contains_key("version") {
            data.insert("version".into(), json!(1));
        }
        data.extend(self.data.clone());
        if !data.get("outputs").is_some_and(Value::is_object) {
            data.insert("outputs".into(), json!({}));
        }
        let mut text = serde_json::to_string_pretty(&Value::Object(data)).unwrap_or_default();
        text.push('\n');
        text
    }

    /// Atomic, 0600.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        fsx::atomic_write_with(path, self.to_json().as_bytes(), 0o600, ".snap-layouts.json.", &fsx::os_replace)
    }
}

/// `$XDG_CONFIG_HOME/tessel/snap-layouts.json`.
pub fn config_path() -> PathBuf { tessel_core::paths::config_home().join("tessel").join("snap-layouts.json") }

// ------------------------------------------------------------------ rc.xml

/// The managed block for these layouts; None when every screen keeps the default.
pub fn block_text(outputs: &BTreeMap<String, Layout>) -> Option<String> {
    let regions: Vec<String> = outputs.iter().filter(|(name, layout)| !name.is_empty() && !layout.is_default() && layout.valid())
        .flat_map(|(name, layout)| layout.regions(name)).collect();
    if regions.is_empty() {
        return None;
    }
    let mut lines = vec![SNAP_BEGIN.to_owned(), "<regions>".to_owned()];
    lines.extend(regions.iter().map(|line| format!("  {line}")));
    lines.push("</regions>".into());
    lines.push(SNAP_END.into());
    Some(lines.join("\n"))
}

fn find_block(text: &str) -> Option<(usize, usize)> {
    let start = text.find(SNAP_BEGIN)?;
    let end = start + text[start..].find(SNAP_END)? + SNAP_END.len();
    Some((start, end))
}

/// The managed block in rc.xml text, markers included.
pub fn current_block(text: &str) -> Option<&str> { find_block(text).map(|(s, e)| &text[s..e]) }

fn closing_tag(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    ["</labwc_config", "</openbox_config"].iter().filter_map(|tag| {
        let index = lower.rfind(tag)?;
        lower[index + tag.len()..].trim_start().starts_with('>').then_some(index)
    }).max()
}

/// `<labwc_config …/>` opened into `<labwc_config …>\n</labwc_config>`.
fn open_empty_root(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for name in ["labwc_config", "openbox_config"] {
        let tag = format!("<{name}");
        let Some(start) = lower.match_indices(&tag).map(|(i, _)| i).find(|i| {
            lower[i + tag.len()..].chars().next().is_some_and(|c| c.is_ascii_whitespace() || c == '/' || c == '>')
        }) else { continue };
        let after = start + tag.len();
        let close = after + lower[after..].find('>')?;
        if !text[..close].ends_with('/') {
            return None;
        }
        let original = &text[start + 1..after];
        let attributes = text[after..close - 1].trim_end();
        return Some(format!("{}<{original}{attributes}>\n</{original}>{}", &text[..start], &text[close + 1..]));
    }
    None
}

/// rc.xml text with the managed block replaced, added before the closing
/// root element, or (None) removed. Everything else is kept byte for byte.
pub fn with_block(text: &str, block: Option<&str>) -> Result<String, String> {
    match (find_block(text), block) {
        (Some((start, end)), Some(block)) => Ok(format!("{}{block}{}", &text[..start], &text[end..])),
        (Some((start, end)), None) => Ok(format!("{}{}", text[..start].trim_end(), &text[end..])),
        (None, None) => Ok(text.to_owned()),
        (None, Some(block)) => {
            let opened;
            let source = if closing_tag(text).is_none() {
                match open_empty_root(text) {
                    Some(result) => {
                        opened = result;
                        &opened
                    }
                    None => text,
                }
            } else {
                text
            };
            let index = closing_tag(source).ok_or_else(|| "rc.xml has no closing labwc_config element".to_owned())?;
            Ok(format!("{}\n\n{block}\n{}", source[..index].trim_end(), &source[index..]))
        }
    }
}

/// Atomically make rc.xml's snap layouts block say `block` (None removes it)
/// under the shared rc.xml lock. A missing rc.xml is created only to add a block.
pub fn write_block_with(path: &Path, block: Option<&str>, replace: Replace) -> Result<BlockWrite, String> {
    // Write through a linked rc.xml to the file it points to.
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if block.is_none() && !path.exists() {
        return Ok(BlockWrite::NoFile);
    }
    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory).map_err(|error| tessel_core::pyerr::os_error_path(&error, directory))?;
    let _lock = lock_rc(&path).map_err(|error| tessel_core::pyerr::os_error_path(&error, &rc_lock_path(&path)))?;
    let (text, mode) = match std::fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes).map_err(|_| format!("'utf-8' codec can't decode file '{}'", path.display()))?;
            (text, fsx::mode_of(&path).map_err(|error| fsx::read_error(&error, &path))?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if block.is_none() {
                return Ok(BlockWrite::NoFile);
            }
            ("<?xml version=\"1.0\"?>\n<labwc_config>\n</labwc_config>\n".to_owned(), 0o600)
        }
        Err(error) => return Err(fsx::read_error(&error, &path)),
    };
    let mut updated = with_block(&text, block)?;
    if updated != text && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if updated == text {
        return Ok(BlockWrite::Unchanged);
    }
    fsx::atomic_write_with(&path, updated.as_bytes(), mode, ".rc.xml.", replace)?;
    Ok(BlockWrite::Written)
}

/// Regenerate rc.xml's block from `layouts`; only when that changed the
/// file, have labwc read it again.
pub fn sync_rc_with(rc: &Path, layouts: &SnapLayouts, replace: Replace,
                    reconfigure: &dyn Fn() -> (bool, Option<String>)) -> Result<BlockWrite, String> {
    let written = write_block_with(rc, block_text(&layouts.outputs).as_deref(), replace)?;
    if written == BlockWrite::Written {
        if let (false, error) = reconfigure() {
            return Err(format!("labwc did not reload rc.xml: {}", error.unwrap_or_default()));
        }
    }
    Ok(written)
}

/// The saved JSON written to the running session's rc.xml (for session start).
pub fn sync_rc() -> Result<BlockWrite, String> {
    sync_rc_with(&labwc_config_path(), &SnapLayouts::load(&config_path()), &fsx::os_replace, &reconfigure_labwc)
}

/// Whether rc.xml's block already says what `layouts` says.
pub fn rc_in_sync(rc: &Path, layouts: &SnapLayouts) -> bool {
    let text = std::fs::read_to_string(rc).unwrap_or_default();
    current_block(&text) == block_text(&layouts.outputs).as_deref()
}

// --------------------------------------------------------------------- GTK

/// A connected screen as the page shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub connector: String,
    pub label: String,
    pub width: i32,
    pub height: i32,
}

impl Screen {
    pub fn portrait(&self) -> bool { self.height > self.width }
}

/// GDK's monitors that have a connector name. GTK thread only.
pub fn display_screens() -> Vec<Screen> {
    let Some(display) = gdk::Display::default() else { return Vec::new() };
    let model = display.monitors();
    (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gdk::Monitor>()).filter_map(|monitor| {
        let connector = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty())?;
        let label = tessel_dock::core::monitors::monitor_label(&connector, monitor.description().as_deref().unwrap_or(""),
                                                              monitor.model().as_deref().unwrap_or(""));
        let geometry = monitor.geometry();
        Some(Screen { connector, label, width: geometry.width(), height: geometry.height() })
    }).collect()
}

pub type Reconfigure = Rc<dyn Fn() -> (bool, Option<String>)>;
pub type ScreenList = Rc<dyn Fn() -> Vec<Screen>>;

/// Test seams; each None is the real one.
#[derive(Clone, Default)]
pub struct SnapOptions {
    pub config: Option<PathBuf>,
    pub rc: Option<Rc<dyn Fn() -> PathBuf>>,
    pub reconfigure: Option<Reconfigure>,
    /// With a fake list the page does not watch GDK; call `refresh_screens`.
    pub screens: Option<ScreenList>,
}

const CSS: &str = "\
.tessel-snap-preview separator.tessel-snap-divider { background: transparent; min-width: 0; min-height: 0; border-radius: 6px; }
.tessel-snap-preview separator.tessel-snap-divider:hover { background-color: alpha(@theme_selected_bg_color, .35); }
.tessel-snap-preview separator.tessel-snap-divider:focus-visible { background-color: alpha(@theme_selected_bg_color, .6); outline: none; }
.tessel-snap-preview label.tessel-snap-size { font-size: 9.5pt; }
";
const HANDLE: i32 = 12;

/// The "Snap layouts" card of Desktop Behavior.
pub struct SnapLayoutsGroup {
    pub widget: gtk::Box,
    pub screen: gtk::DropDown,
    pub screen_caption: gtk::Label,
    pub layout: gtk::DropDown,
    pub columns: gtk::SpinButton,
    pub rows: gtk::SpinButton,
    pub preview: gtk::DrawingArea,
    pub overlay: gtk::Overlay,
    pub hint: gtk::Label,
    pub tiling_note: gtk::Label,
    pub error: gtk::Label,
    config: PathBuf,
    rc: Rc<dyn Fn() -> PathBuf>,
    reconfigure: Reconfigure,
    screen_list: ScreenList,
    data: RefCell<SnapLayouts>,
    screens: RefCell<Vec<Screen>>,
    /// The screen being edited and its layout.
    editing: RefCell<Option<String>>,
    current: RefCell<Layout>,
    dividers: RefCell<Vec<(Axis, usize, gtk::Separator)>>,
    labels: RefCell<Vec<(usize, usize, gtk::Label)>>,
    updating: Cell<bool>,
    rc_failed: Cell<bool>,
    pending: RefCell<Option<glib::SourceId>>,
    drag: RefCell<Option<(Axis, usize, Layout)>>,
    monitor_watch: RefCell<Option<(gio::ListModel, glib::SignalHandlerId)>>,
    tiling_watch: RefCell<Option<(gtk::Switch, glib::SignalHandlerId)>>,
    me: Weak<Self>,
}

fn caption(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).hexpand(true).wrap(true).valign(gtk::Align::Center).build()
}

fn accessible_label(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

fn spin(max: usize, name: &str) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(1.0, max as f64, 1.0);
    spin.set_numeric(true);
    spin.set_width_chars(3);
    spin.set_halign(gtk::Align::End);
    spin.set_valign(gtk::Align::Center);
    accessible_label(&spin, name);
    spin
}

fn running(sizes: &[u32], count: usize) -> u32 { sizes.iter().take(count).sum() }

impl SnapLayoutsGroup {
    pub fn new(options: SnapOptions) -> Rc<Self> {
        let watch_display = options.screens.is_none();
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).css_classes(["settings-card"]).build();
        tessel_gtk::utils::css_once(&WidgetExt::display(&widget), "tessel-snap-layouts", CSS, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        heading.add_css_class("settings-section-heading");
        heading.append(&gtk::Label::builder().label(tr!("Snap layouts")).xalign(0.0).css_classes(["settings-section-title"]).build());
        widget.append(&heading);
        widget.append(&gtk::Label::builder().label(tr!("Split a screen into areas that windows snap into."))
            .xalign(0.0).wrap(true).max_width_chars(60).css_classes(["settings-muted"]).build());
        let grid = gtk::Grid::builder().column_spacing(16).row_spacing(12).build();
        let screen = gtk::DropDown::from_strings(&[]);
        screen.set_valign(gtk::Align::Center);
        screen.set_halign(gtk::Align::End);
        let screen_caption = caption(&tr!("Screen"));
        accessible_label(&screen, &tr!("Screen"));
        let labels: Vec<String> = PRESETS.iter().map(|p| p.label()).collect();
        let layout = gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        layout.set_valign(gtk::Align::Center);
        layout.set_halign(gtk::Align::End);
        accessible_label(&layout, &tr!("Layout"));
        let columns = spin(MAX_COLUMNS, &tr!("Columns"));
        let rows = spin(MAX_ROWS, &tr!("Rows"));
        for (row, (label, control)) in [(screen_caption.clone(), screen.clone().upcast::<gtk::Widget>()),
                                        (caption(&tr!("Layout")), layout.clone().upcast()),
                                        (caption(&tr!("Columns")), columns.clone().upcast()),
                                        (caption(&tr!("Rows")), rows.clone().upcast())].into_iter().enumerate() {
            grid.attach(&label, 0, row as i32, 1, 1);
            grid.attach(&control, 1, row as i32, 1, 1);
        }
        widget.append(&grid);
        let preview = gtk::DrawingArea::new();
        accessible_label(&preview, &tr!("Preview"));
        let overlay = gtk::Overlay::builder().halign(gtk::Align::Center).css_classes(["tessel-snap-preview"]).build();
        overlay.set_child(Some(&preview));
        widget.append(&overlay);
        let hint = gtk::Label::builder()
            .label(tr!("Drag a window to a screen edge, or hold Shift while dragging, to snap it into an area. Super+Arrow keys move it between areas."))
            .xalign(0.0).wrap(true).max_width_chars(60).css_classes(["settings-muted"]).build();
        widget.append(&hint);
        let tiling_note = gtk::Label::builder().label(tr!("Snap layouts apply to windows that are not tiled."))
            .xalign(0.0).wrap(true).max_width_chars(60).visible(false).css_classes(["settings-muted"]).build();
        widget.append(&tiling_note);
        let error = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(60).visible(false).css_classes(["error"]).build();
        widget.append(&error);
        let config = options.config.unwrap_or_else(config_path);
        let this = Rc::new_cyclic(|me| Self {
            me: me.clone(), widget, screen, screen_caption, layout, columns, rows, preview, overlay, hint, tiling_note, error,
            data: RefCell::new(SnapLayouts::load(&config)), config,
            rc: options.rc.unwrap_or_else(|| Rc::new(labwc_config_path)),
            reconfigure: options.reconfigure.unwrap_or_else(|| Rc::new(reconfigure_labwc)),
            screen_list: options.screens.unwrap_or_else(|| Rc::new(display_screens)),
            screens: RefCell::default(), editing: RefCell::default(), current: RefCell::default(), dividers: RefCell::default(),
            labels: RefCell::default(), updating: Cell::new(false), rc_failed: Cell::new(false), pending: RefCell::default(),
            drag: RefCell::default(), monitor_watch: RefCell::default(), tiling_watch: RefCell::default(),
        });
        this.connect(watch_display);
        this.refresh_screens();
        this
    }

    fn connect(self: &Rc<Self>, watch_display: bool) {
        let weak = Rc::downgrade(self);
        self.screen.connect_selected_notify(move |_| {
            let Some(this) = weak.upgrade() else { return };
            if !this.updating.get() {
                this.flush();
                this.load_selected();
            }
        });
        let weak = Rc::downgrade(self);
        self.layout.connect_selected_notify(move |dropdown| {
            let Some(this) = weak.upgrade() else { return };
            if this.updating.get() {
                return;
            }
            if let Some(layout) = PRESETS.get(dropdown.selected() as usize).and_then(|p| p.layout()) {
                this.set_current(layout, When::Now);
            }
        });
        for spin in [&self.columns, &self.rows] {
            let weak = Rc::downgrade(self);
            spin.connect_value_changed(move |_| {
                let Some(this) = weak.upgrade() else { return };
                if !this.updating.get() {
                    let layout = Layout::equal(this.columns.value_as_int() as usize, this.rows.value_as_int() as usize);
                    this.set_current(layout, When::Now);
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.preview.set_draw_func(move |area, context, width, height| {
            if let Some(this) = weak.upgrade() {
                this.draw(area, context, width as f64, height as f64);
            }
        });
        let weak = Rc::downgrade(self);
        self.overlay.connect_get_child_position(move |_, child| weak.upgrade().and_then(|this| this.child_position(child)));
        let drag = gtk::GestureDrag::new();
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |gesture, x, y| {
            if !weak.upgrade().is_some_and(|this| this.begin_drag(x, y)) {
                gesture.set_state(gtk::EventSequenceState::Denied);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(this) = weak.upgrade() {
                this.update_drag(dx, dy);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            if let Some(this) = weak.upgrade() {
                this.end_drag();
            }
        });
        self.overlay.add_controller(drag);
        if watch_display {
            if let Some(display) = gdk::Display::default() {
                let model = display.monitors();
                let weak: Weak<Self> = Rc::downgrade(self);
                let id = model.connect_items_changed(move |_, _, _, _| {
                    let weak = weak.clone();
                    // A new monitor's connector arrives with it; read the list once settled.
                    glib::idle_add_local_once(move || {
                        if let Some(this) = weak.upgrade() {
                            this.refresh_screens();
                        }
                    });
                });
                self.monitor_watch.replace(Some((model, id)));
            }
        }
    }

    /// Show the note while `switch` (Window tiling) is on.
    pub fn watch_tiling(self: &Rc<Self>, switch: &gtk::Switch) {
        self.tiling_note.set_visible(switch.is_active());
        let note = self.tiling_note.clone();
        let id = switch.connect_active_notify(move |switch| note.set_visible(switch.is_active()));
        if let Some((old, id)) = self.tiling_watch.replace(Some((switch.clone(), id))) {
            old.disconnect(id);
        }
    }

    pub fn screens(&self) -> Vec<Screen> { self.screens.borrow().clone() }
    pub fn current(&self) -> Layout { self.current.borrow().clone() }
    pub fn editing(&self) -> Option<String> { self.editing.borrow().clone() }
    pub fn saved(&self) -> SnapLayouts { self.data.borrow().clone() }
    pub fn preset(&self) -> Preset { PRESETS.get(self.layout.selected() as usize).copied().unwrap_or(Preset::Custom) }
    pub fn dividers(&self) -> Vec<(Axis, usize, gtk::Separator)> { self.dividers.borrow().clone() }
    pub fn size_labels(&self) -> Vec<String> { self.labels.borrow().iter().map(|(_, _, l)| l.text().to_string()).collect() }
    pub fn has_pending(&self) -> bool { self.pending.borrow().is_some() }

    /// Read the JSON and the screens again (page activation).
    pub fn refresh(&self) {
        self.flush();
        *self.data.borrow_mut() = SnapLayouts::load(&self.config);
        self.refresh_screens();
    }

    /// Rebuild the screen list, keeping the selected screen when it is still there.
    pub fn refresh_screens(&self) {
        if self.drag.borrow().is_some() {
            return;
        }
        self.flush();
        let list = (self.screen_list)();
        let keep = self.editing.borrow().clone();
        let index = keep.and_then(|k| list.iter().position(|s| s.connector == k)).unwrap_or(0);
        let labels: Vec<&str> = list.iter().map(|s| s.label.as_str()).collect();
        self.updating.set(true);
        self.screen.set_model(Some(&gtk::StringList::new(&labels)));
        self.screen.set_selected(index as u32);
        self.updating.set(false);
        let several = list.len() > 1;
        self.screen.set_visible(several);
        self.screen_caption.set_visible(several);
        for widget in [self.layout.upcast_ref::<gtk::Widget>(), self.columns.upcast_ref(), self.rows.upcast_ref()] {
            widget.set_sensitive(!list.is_empty());
        }
        *self.screens.borrow_mut() = list;
        self.load_selected();
    }

    fn selected_screen(&self) -> Option<Screen> { self.screens.borrow().get(self.screen.selected() as usize).cloned() }

    fn load_selected(&self) {
        let screen = self.selected_screen();
        let layout = screen.as_ref().map(|s| self.data.borrow().layout(&s.connector)).unwrap_or_default();
        *self.editing.borrow_mut() = screen.as_ref().map(|s| s.connector.clone());
        let (width, height) = screen.map(|s| (s.width.max(1) as f64, s.height.max(1) as f64)).unwrap_or((16.0, 9.0));
        let (width, height) = if height > width {
            ((270.0 * width / height).round().max(90.0), 270.0)
        } else {
            (360.0, (360.0 * height / width).round().clamp(120.0, 270.0))
        };
        self.preview.set_content_width(width as i32);
        self.preview.set_content_height(height as i32);
        *self.current.borrow_mut() = layout;
        self.sync_controls(true);
    }

    /// What the preview draws: the default shows labwc's two halves.
    fn shown(&self) -> Layout {
        let current = self.current.borrow();
        if current.is_default() { Layout::equal(2, 1) } else { current.clone() }
    }

    fn sync_controls(&self, rebuild: bool) {
        let current = self.current();
        self.updating.set(true);
        let preset = Preset::of(&current);
        self.layout.set_selected(PRESETS.iter().position(|p| *p == preset).unwrap_or(PRESETS.len() - 1) as u32);
        self.columns.set_value(current.columns.len() as f64);
        self.rows.set_value(current.rows.len() as f64);
        self.updating.set(false);
        if rebuild {
            self.rebuild_children();
        }
        self.update_children();
        self.preview.queue_draw();
        self.overlay.queue_allocate();
    }

    fn rebuild_children(&self) {
        for (_, _, divider) in self.dividers.take() {
            self.overlay.remove_overlay(&divider);
        }
        for (_, _, label) in self.labels.take() {
            self.overlay.remove_overlay(&label);
        }
        let current = self.current();
        let shown = self.shown();
        for column in 0..shown.columns.len() {
            for row in 0..shown.rows.len() {
                let label = gtk::Label::builder().can_target(false).ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["tessel-snap-size"]).build();
                self.overlay.add_overlay(&label);
                self.labels.borrow_mut().push((column, row, label));
            }
        }
        if current.is_default() {
            return;
        }
        for axis in [Axis::Columns, Axis::Rows] {
            let count = current.sizes(axis).len();
            for index in 0..count.saturating_sub(1) {
                let orientation = if axis == Axis::Columns { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
                let divider = gtk::Separator::builder().orientation(orientation).focusable(true).focus_on_click(true)
                    .css_classes(["tessel-snap-divider"]).build();
                divider.set_cursor_from_name(Some(if axis == Axis::Columns { "col-resize" } else { "row-resize" }));
                let name = if axis == Axis::Columns {
                    tr!("Border between columns {value1} and {value2}", value1 = index + 1, value2 = index + 2)
                } else {
                    tr!("Border between rows {value1} and {value2}", value1 = index + 1, value2 = index + 2)
                };
                accessible_label(&divider, &name);
                let help = tr!("Drag or use the arrow keys to resize the areas.");
                divider.set_tooltip_text(Some(&help));
                divider.update_property(&[gtk::accessible::Property::Description(&help)]);
                let keys = gtk::EventControllerKey::new();
                let weak = self.me.clone();
                keys.connect_key_pressed(move |_, key, _, _| {
                    let delta = match (axis, key) {
                        (Axis::Columns, gdk::Key::Left | gdk::Key::KP_Left) | (Axis::Rows, gdk::Key::Up | gdk::Key::KP_Up) => -1,
                        (Axis::Columns, gdk::Key::Right | gdk::Key::KP_Right) | (Axis::Rows, gdk::Key::Down | gdk::Key::KP_Down) => 1,
                        _ => return glib::Propagation::Proceed,
                    };
                    if let Some(this) = weak.upgrade() {
                        this.move_divider(axis, index, delta);
                    }
                    glib::Propagation::Stop
                });
                divider.add_controller(keys);
                self.overlay.add_overlay(&divider);
                self.dividers.borrow_mut().push((axis, index, divider));
            }
        }
    }

    fn update_children(&self) {
        let current = self.current();
        let shown = self.shown();
        let grid = shown.columns.len() > 1 && shown.rows.len() > 1;
        for (column, row, label) in self.labels.borrow().iter() {
            let (width, height) = (shown.columns[*column], shown.rows[*row]);
            let text = if grid {
                format!("{} × {}", tr!("{value1}%", value1 = width), tr!("{value1}%", value1 = height))
            } else if shown.columns.len() > 1 {
                tr!("{value1}%", value1 = width)
            } else {
                tr!("{value1}%", value1 = height)
            };
            label.set_text(&text);
        }
        for (axis, index, divider) in self.dividers.borrow().iter() {
            let sizes = current.sizes(*axis);
            let position = running(sizes, index + 1);
            let low = running(sizes, *index) + MIN_AREA;
            let high = running(sizes, index + 2) - MIN_AREA;
            divider.update_property(&[gtk::accessible::Property::ValueNow(position as f64),
                                      gtk::accessible::Property::ValueMin(low as f64),
                                      gtk::accessible::Property::ValueMax(high as f64),
                                      gtk::accessible::Property::ValueText(&tr!("{value1}%", value1 = position))]);
        }
    }

    fn child_position(&self, child: &gtk::Widget) -> Option<gdk::Rectangle> {
        let (width, height) = (self.preview.width(), self.preview.height());
        if width <= 0 || height <= 0 {
            return None;
        }
        let current = self.current();
        let at = |sizes: &[u32], count: usize, length: i32| (length as f64 * running(sizes, count) as f64 / 100.0).round() as i32;
        if let Some((axis, index, _)) = self.dividers.borrow().iter().find(|(_, _, d)| d.upcast_ref::<gtk::Widget>() == child) {
            return Some(match axis {
                Axis::Columns => gdk::Rectangle::new(at(&current.columns, index + 1, width) - HANDLE / 2, 0, HANDLE, height),
                Axis::Rows => gdk::Rectangle::new(0, at(&current.rows, index + 1, height) - HANDLE / 2, width, HANDLE),
            });
        }
        let shown = self.shown();
        let labels = self.labels.borrow();
        let (column, row, label) = labels.iter().find(|(_, _, l)| l.upcast_ref::<gtk::Widget>() == child)?;
        let (x0, x1) = (at(&shown.columns, *column, width), at(&shown.columns, column + 1, width));
        let (y0, y1) = (at(&shown.rows, *row, height), at(&shown.rows, row + 1, height));
        let (minimum_width, natural_width, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
        let label_width = natural_width.min(x1 - x0 - 8).max(minimum_width);
        let (_, natural_height, _, _) = label.measure(gtk::Orientation::Vertical, label_width);
        Some(gdk::Rectangle::new(x0 + (x1 - x0 - label_width) / 2, y0 + (y1 - y0 - natural_height) / 2, label_width, natural_height))
    }

    #[allow(deprecated)]
    fn draw(&self, area: &gtk::DrawingArea, context: &cairo::Context, width: f64, height: f64) {
        let foreground = area.color();
        let accent = area.style_context().lookup_color("theme_selected_bg_color").unwrap_or(foreground);
        let rgba = |color: &gdk::RGBA, alpha: f64| (color.red() as f64, color.green() as f64, color.blue() as f64, alpha);
        let rounded = |x: f64, y: f64, w: f64, h: f64, r: f64| {
            let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
            context.new_sub_path();
            context.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
            context.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
            context.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
            context.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
            context.close_path();
        };
        let set = |(r, g, b, a): (f64, f64, f64, f64)| context.set_source_rgba(r, g, b, a);
        rounded(0.5, 0.5, width - 1.0, height - 1.0, 8.0);
        set(rgba(&foreground, 0.05));
        let _ = context.fill_preserve();
        set(rgba(&foreground, 0.35));
        context.set_line_width(1.0);
        let _ = context.stroke();
        let current = self.current();
        let default = current.is_default();
        let shown = self.shown();
        let at = |sizes: &[u32], count: usize, length: f64| length * running(sizes, count) as f64 / 100.0;
        for column in 0..shown.columns.len() {
            for row in 0..shown.rows.len() {
                let (x0, x1) = (at(&shown.columns, column, width), at(&shown.columns, column + 1, width));
                let (y0, y1) = (at(&shown.rows, row, height), at(&shown.rows, row + 1, height));
                rounded(x0 + 4.0, y0 + 4.0, x1 - x0 - 8.0, y1 - y0 - 8.0, 5.0);
                let tint = if default { 0.06 } else if (column + row) % 2 == 0 { 0.22 } else { 0.32 };
                set(rgba(&accent, tint));
                let _ = context.fill_preserve();
                set(rgba(&accent, if default { 0.45 } else { 0.7 }));
                if default {
                    context.set_dash(&[4.0, 3.0], 0.0);
                }
                let _ = context.stroke();
                context.set_dash(&[], 0.0);
            }
        }
        if default {
            return;
        }
        // A grip on each divider.
        set(rgba(&foreground, 0.55));
        context.set_line_width(3.0);
        context.set_line_cap(cairo::LineCap::Round);
        for index in 1..current.columns.len() {
            let x = at(&current.columns, index, width).round();
            context.move_to(x, height / 2.0 - 10.0);
            context.line_to(x, height / 2.0 + 10.0);
        }
        for index in 1..current.rows.len() {
            let y = at(&current.rows, index, height).round();
            context.move_to(width / 2.0 - 10.0, y);
            context.line_to(width / 2.0 + 10.0, y);
        }
        let _ = context.stroke();
    }

    fn divider_at(&self, x: f64, y: f64) -> Option<(Axis, usize, gtk::Separator)> {
        let (width, height) = (self.preview.width() as f64, self.preview.height() as f64);
        let current = self.current();
        self.dividers.borrow().iter().map(|(axis, index, divider)| {
            let (point, length) = if *axis == Axis::Columns { (x, width) } else { (y, height) };
            let distance = (point - length * running(current.sizes(*axis), index + 1) as f64 / 100.0).abs();
            (distance, (*axis, *index, divider.clone()))
        }).filter(|(distance, _)| *distance <= HANDLE as f64 / 2.0 + 2.0)
            .min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, found)| found)
    }

    fn begin_drag(&self, x: f64, y: f64) -> bool {
        let Some((axis, index, divider)) = self.divider_at(x, y) else { return false };
        divider.grab_focus();
        self.drag.replace(Some((axis, index, self.current())));
        true
    }

    fn update_drag(&self, dx: f64, dy: f64) {
        let Some((axis, index, start)) = self.drag.borrow().clone() else { return };
        let (offset, length) = if axis == Axis::Columns { (dx, self.preview.width()) } else { (dy, self.preview.height()) };
        if length <= 0 {
            return;
        }
        let mut layout = start;
        move_divider(layout.sizes_mut(axis), index, (offset * 100.0 / length as f64).round() as i32);
        if layout != self.current() {
            self.set_current(layout, When::Later);
        }
    }

    fn end_drag(&self) {
        if self.drag.take().is_some() {
            self.apply_now();
        }
    }

    /// Move a divider by `delta` percent as a key press does (applied after [`DEBOUNCE`]).
    pub fn move_divider(&self, axis: Axis, index: usize, delta: i32) -> bool {
        let mut layout = self.current();
        if !move_divider(layout.sizes_mut(axis), index, delta) {
            return false;
        }
        self.set_current(layout, When::Debounced);
        true
    }

    /// A mouse drag of a divider from start to end, `offset` in pixels.
    pub fn drag_divider(&self, axis: Axis, index: usize, offset: f64) -> bool {
        let Some((_, _, divider)) = self.dividers.borrow().iter().find(|(a, i, _)| *a == axis && *i == index).cloned() else {
            return false;
        };
        divider.grab_focus();
        self.drag.replace(Some((axis, index, self.current())));
        if axis == Axis::Columns { self.update_drag(offset, 0.0) } else { self.update_drag(0.0, offset) }
        self.end_drag();
        true
    }

    fn set_current(&self, layout: Layout, when: When) {
        let shape = {
            let current = self.current.borrow();
            current.columns.len() != layout.columns.len() || current.rows.len() != layout.rows.len()
                || current.is_default() != layout.is_default()
        };
        *self.current.borrow_mut() = layout;
        self.sync_controls(shape);
        match when {
            When::Now => self.apply_now(),
            When::Debounced => self.schedule(),
            When::Later => {}
        }
    }

    fn schedule(&self) {
        if let Some(source) = self.pending.take() {
            source.remove();
        }
        let weak = self.me.clone();
        let source = glib::timeout_add_local_once(DEBOUNCE, move || {
            if let Some(this) = weak.upgrade() {
                this.pending.replace(None);
                this.apply_now();
            }
        });
        self.pending.replace(Some(source));
    }

    /// Apply a change still waiting for its debounce.
    pub fn flush(&self) {
        if self.pending.borrow().is_some() {
            self.apply_now();
        }
    }

    /// Save the JSON, then regenerate rc.xml's block from it.
    pub fn apply_now(&self) {
        if let Some(source) = self.pending.take() {
            source.remove();
        }
        let Some(output) = self.editing() else { return };
        let layout = self.current();
        if self.data.borrow().layout(&output) == layout && !self.rc_failed.get() {
            return;
        }
        let mut data = self.data.borrow().clone();
        data.set(&output, &layout);
        if let Err(error) = data.save(&self.config) {
            self.show_error(Some(tr!("Could not save the snap layout: {error}", error = error)));
            return;
        }
        *self.data.borrow_mut() = data;
        let result = sync_rc_with(&(self.rc)(), &self.data.borrow(), &fsx::os_replace, &*self.reconfigure);
        self.rc_failed.set(result.is_err());
        self.show_error(result.err().map(|error| tr!("Could not apply the snap layout: {error}", error = error)));
    }

    fn show_error(&self, message: Option<String>) {
        self.error.set_text(message.as_deref().unwrap_or(""));
        self.error.set_visible(message.is_some());
    }

    /// Whether rc.xml already matches the saved JSON (checked, never rewritten, on load).
    pub fn rc_in_sync(&self) -> bool { rc_in_sync(&(self.rc)(), &self.data.borrow()) }
}

#[derive(Clone, Copy)]
enum When {
    Now,
    Debounced,
    Later,
}

impl Drop for SnapLayoutsGroup {
    fn drop(&mut self) {
        if self.pending.borrow().is_some() {
            self.apply_now();
        }
        if let Some((model, id)) = self.monitor_watch.take() {
            model.disconnect(id);
        }
        if let Some((switch, id)) = self.tiling_watch.take() {
            switch.disconnect(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-snap-unit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn example() -> SnapLayouts {
        let mut layouts = SnapLayouts::default();
        layouts.set("HDMI-A-1", &Layout { columns: vec![60, 40], rows: vec![100] });
        layouts.set("DP-2", &Preset::Rows3.layout().unwrap());
        layouts
    }

    const EXAMPLE: &str = "<!-- tessel snap layouts begin -->
<regions>
  <region name=\"tessel-1-1\" output=\"DP-2\" x=\"0%\" y=\"0%\" width=\"100%\" height=\"33%\"/>
  <region name=\"tessel-1-2\" output=\"DP-2\" x=\"0%\" y=\"33%\" width=\"100%\" height=\"34%\"/>
  <region name=\"tessel-1-3\" output=\"DP-2\" x=\"0%\" y=\"67%\" width=\"100%\" height=\"33%\"/>
  <region name=\"tessel-1-1\" output=\"HDMI-A-1\" x=\"0%\" y=\"0%\" width=\"60%\" height=\"100%\"/>
  <region name=\"tessel-2-1\" output=\"HDMI-A-1\" x=\"60%\" y=\"0%\" width=\"40%\" height=\"100%\"/>
</regions>
<!-- tessel snap layouts end -->";

    #[test]
    fn presets_give_equal_percentages() {
        let shapes: Vec<_> = PRESETS.iter().map(|p| p.layout()).collect();
        assert_eq!(shapes[0], Some(Layout { columns: vec![100], rows: vec![100] }));
        assert_eq!(Preset::Columns2.layout().unwrap().columns, [50, 50]);
        assert_eq!(Preset::Columns3.layout().unwrap().columns, [33, 34, 33]);
        assert_eq!(Preset::Columns4.layout().unwrap().columns, [25, 25, 25, 25]);
        assert_eq!(Preset::Rows3.layout().unwrap(), Layout { columns: vec![100], rows: vec![33, 34, 33] });
        assert_eq!(Preset::Grid2x2.layout().unwrap(), Layout { columns: vec![50, 50], rows: vec![50, 50] });
        assert_eq!(Preset::Grid3x2.layout().unwrap(), Layout { columns: vec![33, 34, 33], rows: vec![50, 50] });
        assert_eq!(Preset::Custom.layout(), None);
        assert_eq!(split(6), [16, 17, 17, 17, 17, 16]);
        for preset in PRESETS {
            if let Some(layout) = preset.layout() {
                assert_eq!(Preset::of(&layout), preset);
            }
        }
        assert_eq!(Preset::of(&Layout { columns: vec![60, 40], rows: vec![100] }), Preset::Custom);
        assert_eq!(Preset::of(&Layout::equal(5, 1)), Preset::Custom);
        assert_eq!(Preset::of(&Layout::default()), Preset::Halves);
    }

    #[test]
    fn every_shape_sums_to_100_and_tiles_without_gaps() {
        for columns in 1..=MAX_COLUMNS {
            for rows in 1..=MAX_ROWS {
                let layout = Layout::equal(columns, rows);
                assert!(layout.valid(), "{columns}x{rows}");
                assert_eq!(layout.columns.iter().sum::<u32>(), 100);
                assert_eq!(layout.rows.iter().sum::<u32>(), 100);
                let regions = layout.regions("X");
                assert_eq!(regions.len(), columns * rows);
                // Coverage of every percent cell exactly once.
                let mut cover = vec![0u8; 100 * 100];
                for line in &regions {
                    let value = |key: &str| -> usize {
                        let start = line.find(&format!(" {key}=\"")).unwrap() + key.len() + 3;
                        line[start..].split('%').next().unwrap().parse().unwrap()
                    };
                    let (x, y, w, h) = (value("x"), value("y"), value("width"), value("height"));
                    for yy in y..y + h {
                        for xx in x..x + w {
                            cover[yy * 100 + xx] += 1;
                        }
                    }
                }
                assert!(cover.iter().all(|c| *c == 1), "{columns}x{rows}");
            }
        }
        assert_eq!(Layout::equal(9, 9), Layout::equal(MAX_COLUMNS, MAX_ROWS));
    }

    #[test]
    fn divider_moves_clamp_at_the_smallest_area() {
        let mut sizes = vec![33, 34, 33];
        assert!(move_divider(&mut sizes, 0, 5));
        assert_eq!(sizes, [38, 29, 33]);
        assert!(move_divider(&mut sizes, 0, 50));
        assert_eq!(sizes, [57, 10, 33]);
        assert!(!move_divider(&mut sizes, 0, 1));
        assert!(!move_divider(&mut sizes, 1, -1));
        assert!(move_divider(&mut sizes, 1, 100));
        assert_eq!(sizes, [57, 33, 10]);
        assert!(move_divider(&mut sizes, 0, -100));
        assert_eq!(sizes, [10, 80, 10]);
        assert!(!move_divider(&mut sizes, 2, 1));
        assert!(!move_divider(&mut sizes, 0, 0));
        assert_eq!(sizes.iter().sum::<u32>(), 100);
        let mut broken = vec![5, 5, 90];
        assert!(!move_divider(&mut broken, 0, 1));
    }

    #[test]
    fn json_round_trip_keeps_unknown_keys_and_ignores_invalid_entries() {
        let text = r#"{"version": 1, "future": {"a": 1}, "outputs": {
            "DP-2": {"columns": [50, 50], "rows": [100], "note": "kept"},
            "BAD-1": {"columns": [50, 40], "rows": [100]},
            "BAD-2": {"columns": [95, 5], "rows": [100]},
            "BAD-3": {"columns": [10, 10, 10, 10, 10, 10, 40], "rows": [100]},
            "BAD-4": {"columns": "x"},
            "BAD-5": 7,
            "ONE-1": {"columns": [100], "rows": [100]}}}"#;
        let mut layouts = SnapLayouts::parse(text);
        assert_eq!(layouts.outputs.keys().collect::<Vec<_>>(), ["DP-2"]);
        assert_eq!(layouts.layout("BAD-1"), Layout::default());
        layouts.set("HDMI-A-1", &Preset::Columns3.layout().unwrap());
        layouts.set("DP-2", &Layout { columns: vec![60, 40], rows: vec![100] });
        let again = SnapLayouts::parse(&layouts.to_json());
        assert_eq!(again, layouts);
        assert_eq!(again.data["future"], json!({"a": 1}));
        assert_eq!(again.data["outputs"]["DP-2"], json!({"columns": [60, 40], "rows": [100], "note": "kept"}));
        assert_eq!(again.data["outputs"]["BAD-5"], json!(7));
        layouts.set("DP-2", &Layout::default());
        assert!(!layouts.data["outputs"].as_object().unwrap().contains_key("DP-2"));
        assert_eq!(layouts.layout("DP-2"), Layout::default());
        for junk in ["", "[]", "null", "{", "{\"outputs\": []}", "{\"outputs\": {\"DP-1\": null}}"] {
            let mut layouts = SnapLayouts::parse(junk);
            assert!(layouts.outputs.is_empty(), "{junk}");
            layouts.set("DP-1", &Preset::Rows2.layout().unwrap());
            assert_eq!(SnapLayouts::parse(&layouts.to_json()).layout("DP-1"), Preset::Rows2.layout().unwrap());
        }
        let fresh = SnapLayouts::default();
        assert_eq!(serde_json::from_str::<Value>(&fresh.to_json()).unwrap(), json!({"version": 1, "outputs": {}}));
    }

    #[test]
    fn json_is_saved_atomically_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("save");
        let path = dir.join("tessel/snap-layouts.json");
        assert_eq!(SnapLayouts::load(&path), SnapLayouts::default());
        let layouts = example();
        layouts.save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(SnapLayouts::load(&path).outputs, layouts.outputs);
        assert_eq!(SnapLayouts::load(&path).data["version"], json!(1));
        assert_eq!(std::fs::read_dir(dir.join("tessel")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn block_text_matches_the_labwc_contract() {
        assert_eq!(block_text(&example().outputs).unwrap(), EXAMPLE);
        let mut layouts = example();
        layouts.set("DP-2", &Layout::default());
        layouts.set("HDMI-A-1", &Preset::Halves.layout().unwrap());
        assert_eq!(block_text(&layouts.outputs), None);
        let mut odd = SnapLayouts::default();
        odd.set("A\"<&", &Preset::Columns2.layout().unwrap());
        assert!(block_text(&odd.outputs).unwrap().contains("output=\"A&quot;&lt;&amp;\""));
    }

    const USER_RC: &str = "<?xml version=\"1.0\"?>\n<labwc_config>\n  <core gap=\"4\"/>\n  <regions>\n    <region name=\"mine\" x=\"0%\" y=\"0%\" width=\"50%\" height=\"50%\"/>\n  </regions>\n  <!-- tessel-displays adaptive sync begin -->\n  <adaptiveSyncOutputs>\n    <output name=\"DP-2\" mode=\"yes\" />\n  </adaptiveSyncOutputs>\n  <!-- tessel-displays adaptive sync end -->\n</labwc_config>\n";

    #[test]
    fn block_is_added_replaced_and_removed_leaving_the_rest_alone() {
        let added = with_block(USER_RC, Some(EXAMPLE)).unwrap();
        let (start, end) = find_block(&added).unwrap();
        assert_eq!(&added[start..end], EXAMPLE);
        assert!(added.starts_with(USER_RC.trim_end_matches("</labwc_config>\n").trim_end()));
        assert!(added.ends_with(&format!("{EXAMPLE}\n</labwc_config>\n")));
        let other = block_text(&{
            let mut l = SnapLayouts::default();
            l.set("DP-1", &Preset::Columns2.layout().unwrap());
            l
        }.outputs).unwrap();
        let replaced = with_block(&added, Some(&other)).unwrap();
        assert_eq!(replaced, added.replace(EXAMPLE, &other));
        assert_eq!(with_block(&replaced, None).unwrap(), USER_RC);
        assert_eq!(with_block(USER_RC, None).unwrap(), USER_RC);
        assert_eq!(with_block("<?xml version=\"1.0\"?>\n<labwc_config/>\n", Some(EXAMPLE)).unwrap(),
                   format!("<?xml version=\"1.0\"?>\n<labwc_config>\n\n{EXAMPLE}\n</labwc_config>\n"));
        assert_eq!(with_block("<openbox_config xmlns=\"x\" />", Some("B")).unwrap(), "<openbox_config xmlns=\"x\">\n\nB\n</openbox_config>");
        assert!(with_block("<other/>", Some("B")).is_err());
    }

    #[test]
    fn rc_writes_only_on_change_and_reconfigures_once() {
        let dir = scratch("rc");
        let rc = dir.join("rc.xml");
        std::fs::write(&rc, USER_RC).unwrap();
        let calls = Cell::new(0);
        let reconfigure = || {
            calls.set(calls.get() + 1);
            (true, None)
        };
        assert!(!rc_in_sync(&rc, &example()));
        assert_eq!(sync_rc_with(&rc, &example(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Written);
        assert_eq!(calls.get(), 1);
        assert!(rc_in_sync(&rc, &example()));
        let written = std::fs::read(&rc).unwrap();
        assert_eq!(sync_rc_with(&rc, &example(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Unchanged);
        assert_eq!(calls.get(), 1);
        assert_eq!(std::fs::read(&rc).unwrap(), written);
        assert_eq!(sync_rc_with(&rc, &SnapLayouts::default(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Written);
        assert_eq!(calls.get(), 2);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), USER_RC);
        assert_eq!(sync_rc_with(&rc, &SnapLayouts::default(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Unchanged);
        assert_eq!(calls.get(), 2);
        // No rc.xml: nothing to remove; a layout creates one.
        let missing = dir.join("new/rc.xml");
        assert_eq!(sync_rc_with(&missing, &SnapLayouts::default(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::NoFile);
        assert!(!missing.exists());
        assert_eq!(sync_rc_with(&missing, &example(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Written);
        assert!(std::fs::read_to_string(&missing).unwrap().contains(EXAMPLE));
        // A failed reload is reported; the file is still written.
        let failing = || (false, Some("no compositor".to_owned()));
        let error = sync_rc_with(&rc, &example(), &fsx::os_replace, &failing).unwrap_err();
        assert!(error.contains("no compositor"));
        assert!(rc_in_sync(&rc, &example()));
        // A linked rc.xml stays a link.
        let target = dir.join("target.xml");
        std::fs::write(&target, USER_RC).unwrap();
        let link = dir.join("link.xml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        sync_rc_with(&link, &example(), &fsx::os_replace, &reconfigure).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(std::fs::read_to_string(&target).unwrap().contains(EXAMPLE));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unwritable_rc_is_an_error() {
        let dir = scratch("ro");
        let rc = dir.join("rc.xml");
        std::fs::write(&rc, USER_RC).unwrap();
        let fail: &dyn Fn(&Path, &Path) -> std::io::Result<()> = &|_, _| Err(std::io::Error::from_raw_os_error(libc::EACCES));
        assert!(sync_rc_with(&rc, &example(), fail, &|| (true, None)).is_err());
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), USER_RC);
        let _ = std::fs::remove_dir_all(dir);
    }
}
