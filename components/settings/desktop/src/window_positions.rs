//! Desktop Behavior "Window positions": the user saves where an app's window
//! is, and labwc opens that app's first window there.
//! `$XDG_CONFIG_HOME/tessel/window-positions.json` is the source of truth;
//! rc.xml gets a managed `<windowRules>` block of `PlaceOnOutput` rules,
//! regenerated from it ([`sync_rc`]). Open windows come from the compositor's
//! `windows` reply (labwc 0.20.2-29 adds `positions_supported` and the keys
//! read here).
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk::prelude::*;
use serde_json::{json, Map, Value};
use tessel_core::tr;
use tessel_displays::compat::fsx::{self, Replace};
use tessel_displays::labwc::{html_escape, labwc_config_path, lock_rc, rc_lock_path, reconfigure_labwc, BlockWrite};
use tessel_dock::core::app_catalog::AppCatalog;
use tessel_tiling::client::{ClientError, Reply};

use crate::snap_layouts::{closing_tag, display_screens, open_empty_root, Reconfigure, ScreenList};
use crate::util::Runner;

pub const POSITIONS_BEGIN: &str = "<!-- tessel window positions begin -->";
pub const POSITIONS_END: &str = "<!-- tessel window positions end -->";
/// Global Settings' own windows are never offered.
pub const OWN_APP_ID: &str = "org.tessel.GlobalSettings";

// ------------------------------------------------------------------- model

fn int(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value.as_i64().or_else(|| value.as_f64().filter(|f| f.is_finite() && f.abs() < 1e12).map(|f| f.round() as i64))
}

fn text(value: Option<&Value>) -> String { value.and_then(Value::as_str).unwrap_or_default().to_owned() }

/// labwc matches identifiers case-insensitively.
pub fn same_app(a: &str, b: &str) -> bool { a.to_lowercase() == b.to_lowercase() }

/// One saved place: where `app_id`'s first window opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub app_id: String,
    /// The app's name when it was saved (shown when no desktop entry is found).
    pub name: String,
    /// Connector name, e.g. `DP-2`.
    pub output: String,
    /// Relative to the output's top-left corner, as the compositor reported it.
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub maximized: bool,
}

impl Position {
    /// A saved entry; None when it lacks an app, a screen or a number.
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let app_id = text(object.get("app_id"));
        let output = text(object.get("output"));
        if app_id.is_empty() || output.is_empty() {
            return None;
        }
        let (width, height) = (int(object.get("width"))?, int(object.get("height"))?);
        if width < 0 || height < 0 {
            return None;
        }
        Some(Self {
            name: object.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(&app_id).to_owned(),
            x: int(object.get("x"))?, y: int(object.get("y"))?, width, height,
            maximized: object.get("maximized").and_then(Value::as_bool).unwrap_or(false),
            app_id, output,
        })
    }

    pub fn to_value(&self) -> Value {
        json!({"app_id": self.app_id, "name": self.name, "output": self.output, "x": self.x, "y": self.y,
               "width": self.width, "height": self.height, "maximized": self.maximized})
    }

    /// Where `window` is now.
    pub fn from_window(window: &OpenWindow, name: &str) -> Self {
        Self { app_id: window.app_id.clone(), name: if name.is_empty() { window.app_id.clone() } else { name.to_owned() },
               output: window.output.clone(), x: window.x, y: window.y, width: window.width, height: window.height,
               maximized: window.maximized }
    }

    /// The `<windowRule>` for this entry, indented for the block.
    pub fn rule(&self) -> String {
        format!("  <windowRule identifier=\"{}\" type=\"normal\" matchOnce=\"true\">\n    \
                 <action name=\"PlaceOnOutput\" output=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" maximized=\"{}\"/>\n  \
                 </windowRule>",
                html_escape(&glob_escape(&self.app_id)), html_escape(&self.output), self.x, self.y, self.width, self.height,
                if self.maximized { "yes" } else { "no" })
    }
}

/// `window-positions.json`: unknown top-level keys are kept (in `data`, without
/// `positions`); invalid entries and later duplicates of an app are left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowPositions {
    pub data: Map<String, Value>,
    pub positions: Vec<Position>,
}

impl WindowPositions {
    pub fn parse(text: &str) -> Self {
        let mut data = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(data)) => data,
            _ => Map::new(),
        };
        let mut positions: Vec<Position> = Vec::new();
        let saved = data.shift_remove("positions");
        for position in saved.as_ref().and_then(Value::as_array).into_iter().flatten().filter_map(Position::from_value) {
            if !positions.iter().any(|p| same_app(&p.app_id, &position.app_id)) {
                positions.push(position);
            }
        }
        Self { data, positions }
    }

    /// A missing or unreadable file is no positions.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map(|text| Self::parse(&text)).unwrap_or_default()
    }

    pub fn get(&self, app_id: &str) -> Option<&Position> { self.positions.iter().find(|p| same_app(&p.app_id, app_id)) }

    /// Save `position`, replacing the app's earlier entry in its place.
    pub fn set(&mut self, position: Position) {
        match self.positions.iter_mut().find(|p| same_app(&p.app_id, &position.app_id)) {
            Some(existing) => *existing = position,
            None => self.positions.push(position),
        }
    }

    /// False when the app had no entry.
    pub fn remove(&mut self, app_id: &str) -> bool {
        let before = self.positions.len();
        self.positions.retain(|p| !same_app(&p.app_id, app_id));
        self.positions.len() != before
    }

    pub fn to_json(&self) -> String {
        let mut data = Map::new();
        if !self.data.contains_key("version") {
            data.insert("version".into(), json!(1));
        }
        data.extend(self.data.clone());
        data.insert("positions".into(), Value::Array(self.positions.iter().map(Position::to_value).collect()));
        let mut text = serde_json::to_string_pretty(&Value::Object(data)).unwrap_or_default();
        text.push('\n');
        text
    }

    /// Atomic, 0600.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        fsx::atomic_write_with(path, self.to_json().as_bytes(), 0o600, ".window-positions.json.", &fsx::os_replace)
    }
}

/// `$XDG_CONFIG_HOME/tessel/window-positions.json`.
pub fn config_path() -> PathBuf { tessel_core::paths::config_home().join("tessel").join("window-positions.json") }

// ------------------------------------------------------------------ rc.xml

/// labwc's identifier is an fnmatch glob: `* ? [ ] \` match only themselves.
pub fn glob_escape(app_id: &str) -> String {
    let mut escaped = String::with_capacity(app_id.len());
    for c in app_id.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// The managed block for these positions; None when there are none.
pub fn block_text(positions: &[Position]) -> Option<String> {
    let rules: Vec<String> = positions.iter().filter(|p| !p.app_id.is_empty() && !p.output.is_empty()).map(Position::rule).collect();
    if rules.is_empty() {
        return None;
    }
    let mut lines = vec![POSITIONS_BEGIN.to_owned(), "<windowRules>".to_owned()];
    lines.extend(rules);
    lines.push("</windowRules>".into());
    lines.push(POSITIONS_END.into());
    Some(lines.join("\n"))
}

fn find_block(text: &str) -> Option<(usize, usize)> {
    let start = text.find(POSITIONS_BEGIN)?;
    let end = start + text[start..].find(POSITIONS_END)? + POSITIONS_END.len();
    Some((start, end))
}

/// The managed block in rc.xml text, markers included.
pub fn current_block(text: &str) -> Option<&str> { find_block(text).map(|(s, e)| &text[s..e]) }

/// rc.xml text with the managed block replaced, added before the closing
/// root element, or (None) removed. Everything else is kept byte for byte.
pub fn with_block(text: &str, block: Option<&str>) -> Result<String, String> {
    match (find_block(text), block) {
        (Some((start, end)), Some(block)) => Ok(format!("{}{block}{}", &text[..start], &text[end..])),
        (Some((start, end)), None) => Ok(format!("{}{}", text[..start].trim_end(), &text[end..])),
        (None, None) => Ok(text.to_owned()),
        (None, Some(block)) => {
            let opened;
            let source = match closing_tag(text).is_none().then(|| open_empty_root(text)).flatten() {
                Some(result) => {
                    opened = result;
                    &opened
                }
                None => text,
            };
            let index = closing_tag(source).ok_or_else(|| "rc.xml has no closing labwc_config element".to_owned())?;
            Ok(format!("{}\n\n{block}\n{}", source[..index].trim_end(), &source[index..]))
        }
    }
}

/// Atomically make rc.xml's window positions block say `block` (None removes
/// it) under the shared rc.xml lock. A missing rc.xml is created only to add a block.
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

/// Regenerate rc.xml's block from `positions`; only when that changed the
/// file, have labwc read it again.
pub fn sync_rc_with(rc: &Path, positions: &WindowPositions, replace: Replace,
                    reconfigure: &dyn Fn() -> (bool, Option<String>)) -> Result<BlockWrite, String> {
    let written = write_block_with(rc, block_text(&positions.positions).as_deref(), replace)?;
    if written == BlockWrite::Written {
        if let (false, error) = reconfigure() {
            return Err(format!("labwc did not reload rc.xml: {}", error.unwrap_or_default()));
        }
    }
    Ok(written)
}

/// The saved JSON written to the running session's rc.xml (for session start).
pub fn sync_rc() -> Result<BlockWrite, String> {
    sync_rc_with(&labwc_config_path(), &WindowPositions::load(&config_path()), &fsx::os_replace, &reconfigure_labwc)
}

/// Whether rc.xml's block already says what `positions` says.
pub fn rc_in_sync(rc: &Path, positions: &WindowPositions) -> bool {
    let text = std::fs::read_to_string(rc).unwrap_or_default();
    current_block(&text) == block_text(&positions.positions).as_deref()
}

// --------------------------------------------------------------- compositor

/// An open window that can be saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenWindow {
    pub app_id: String,
    pub title: String,
    pub output: String,
    /// `place`: relative to the output, the size it returns to when restored.
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub maximized: bool,
    pub active: bool,
}

/// What the compositor said about its windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Windows {
    /// No `positions_supported` (labwc before 0.20.2-29).
    Unsupported,
    /// No answer; the reason.
    Unavailable(String),
    /// Normal windows with an app and a screen, in the compositor's order.
    Open(Vec<OpenWindow>),
}

impl Windows {
    pub fn open(&self) -> &[OpenWindow] {
        match self {
            Windows::Open(windows) => windows,
            _ => &[],
        }
    }

    /// The app's window to save: the active one, else its first.
    pub fn of_app(&self, app_id: &str) -> Option<&OpenWindow> {
        let open = self.open();
        open.iter().find(|w| w.active && same_app(&w.app_id, app_id)).or_else(|| open.iter().find(|w| same_app(&w.app_id, app_id)))
    }
}

/// A `windows` reply, leaving out windows of `own` (app_ids).
pub fn parse_windows(reply: &Reply, own: &[String]) -> Windows {
    if reply.get("positions_supported").and_then(Value::as_bool) != Some(true) {
        return Windows::Unsupported;
    }
    let windows = reply.get("windows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    Windows::Open(windows.iter().filter_map(|window| {
        let object = window.as_object()?;
        if object.get("type_normal").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let (app_id, output) = (text(object.get("app_id")), text(object.get("output_name")));
        if app_id.is_empty() || output.is_empty() || own.iter().any(|o| same_app(o, &app_id)) {
            return None;
        }
        let place = object.get("place")?.as_object()?;
        let (width, height) = (int(place.get("width"))?, int(place.get("height"))?);
        Some(OpenWindow {
            title: text(object.get("title")), x: int(place.get("x"))?, y: int(place.get("y"))?,
            width: width.max(0), height: height.max(0),
            maximized: object.get("maximized").and_then(Value::as_bool).unwrap_or(false),
            active: object.get("active").and_then(Value::as_bool).unwrap_or(false),
            app_id, output,
        })
    }).collect())
}

pub type WindowSource = Arc<dyn Fn() -> Result<Reply, ClientError> + Send + Sync>;

/// Ask the compositor (blocks up to the client's 1.5 s timeout).
pub fn read_windows(source: &dyn Fn() -> Result<Reply, ClientError>, own: &[String]) -> Windows {
    match source() {
        Ok(reply) => parse_windows(&reply, own),
        Err(error) => Windows::Unavailable(error.message().to_owned()),
    }
}

/// The real control socket.
pub fn socket_source() -> WindowSource { Arc::new(|| tessel_tiling::client::request("windows")) }

// --------------------------------------------------------------------- apps

/// An app as the page shows it.
#[derive(Clone, Debug, Default)]
pub struct AppLabel {
    pub name: String,
    pub icon: Option<gio::Icon>,
}

/// The installed app for `app_id` (app_id or WM_CLASS); the app_id itself when none.
pub fn app_label(catalog: &AppCatalog, app_id: &str) -> AppLabel {
    match catalog.resolve(app_id, None) {
        Some(entry) if !entry.name.is_empty() => AppLabel { name: entry.name.clone(), icon: entry.icon.clone() },
        Some(entry) => AppLabel { name: app_id.to_owned(), icon: entry.icon.clone() },
        None => AppLabel { name: app_id.to_owned(), icon: None },
    }
}

pub type AppLookup = Rc<dyn Fn(&str) -> AppLabel>;

/// Installed apps, read once on first use.
pub fn catalog_lookup() -> AppLookup {
    let catalog: RefCell<Option<AppCatalog>> = RefCell::default();
    Rc::new(move |app_id| app_label(catalog.borrow_mut().get_or_insert_with(AppCatalog::new), app_id))
}

// --------------------------------------------------------------------- GTK

/// Test seams; each None is the real one.
#[derive(Clone, Default)]
pub struct PositionOptions {
    pub config: Option<PathBuf>,
    pub rc: Option<Rc<dyn Fn() -> PathBuf>>,
    pub reconfigure: Option<Reconfigure>,
    pub windows: Option<WindowSource>,
    /// Connected screens (for names and the "not connected" note).
    pub screens: Option<ScreenList>,
    pub apps: Option<AppLookup>,
    /// Where the compositor is asked; the default is a worker thread.
    pub runner: Option<Runner>,
}

/// One saved entry's row.
#[derive(Clone)]
pub struct PositionRow {
    pub app_id: String,
    pub widget: gtk::Box,
    pub name: gtk::Label,
    pub detail: gtk::Label,
    pub note: gtk::Label,
    pub update: gtk::Button,
    pub remove: gtk::Button,
}

/// The "Window positions" card of Desktop Behavior.
pub struct WindowPositionsGroup {
    pub widget: gtk::Box,
    pub rows: gtk::Box,
    pub empty: gtk::Label,
    pub add: gtk::Button,
    pub picker: gtk::Box,
    pub picker_list: gtk::Box,
    pub cancel: gtk::Button,
    pub status: gtk::Label,
    pub error: gtk::Label,
    config: PathBuf,
    rc: Rc<dyn Fn() -> PathBuf>,
    reconfigure: Reconfigure,
    source: WindowSource,
    screens: ScreenList,
    apps: AppLookup,
    runner: Runner,
    own: Vec<String>,
    data: RefCell<WindowPositions>,
    windows: RefCell<Option<Windows>>,
    row_list: RefCell<Vec<PositionRow>>,
    choices: RefCell<Vec<(OpenWindow, gtk::Button)>>,
    loading: Cell<bool>,
    requests: Cell<u64>,
    applied: Cell<u64>,
    me: Weak<Self>,
}

fn muted(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).wrap(true).max_width_chars(60).css_classes(["settings-muted"]).build()
}

fn app_image(icon: Option<&gio::Icon>, size: i32) -> gtk::Image {
    let image = match icon {
        Some(icon) => gtk::Image::from_gicon(icon),
        None => gtk::Image::from_icon_name("application-x-executable"),
    };
    image.set_pixel_size(size);
    image.set_valign(gtk::Align::Center);
    image
}

impl WindowPositionsGroup {
    pub fn new(options: PositionOptions) -> Rc<Self> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).css_classes(["settings-card"]).build();
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        heading.add_css_class("settings-section-heading");
        heading.append(&gtk::Label::builder().label(tr!("Window positions")).xalign(0.0).css_classes(["settings-section-title"]).build());
        widget.append(&heading);
        widget.append(&muted(&tr!("Save where an app's window is, and the app opens there the next time it starts.")));
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 12);
        widget.append(&rows);
        let empty = muted(&tr!("No window positions saved yet."));
        widget.append(&empty);
        let add = gtk::Button::builder().label(tr!("Add a window…")).halign(gtk::Align::Start).build();
        widget.append(&add);
        let picker = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).visible(false).build();
        picker.append(&muted(&tr!("Choose a window to save where it is now.")));
        let picker_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        picker.append(&picker_list);
        let cancel = gtk::Button::builder().label(tr!("Cancel")).halign(gtk::Align::Start).build();
        picker.append(&cancel);
        let status = muted("");
        status.set_visible(false);
        widget.append(&status);
        widget.append(&picker);
        let error = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(60).visible(false).css_classes(["error"]).build();
        widget.append(&error);
        let config = options.config.unwrap_or_else(config_path);
        let mut own = vec![OWN_APP_ID.to_owned()];
        if let Some(id) = gio::Application::default().and_then(|a| a.application_id()) {
            own.push(id.to_string());
        }
        let this = Rc::new_cyclic(|me| Self {
            me: me.clone(), widget, rows, empty, add, picker, picker_list, cancel, status, error,
            data: RefCell::new(WindowPositions::load(&config)), config,
            rc: options.rc.unwrap_or_else(|| Rc::new(labwc_config_path)),
            reconfigure: options.reconfigure.unwrap_or_else(|| Rc::new(reconfigure_labwc)),
            source: options.windows.unwrap_or_else(socket_source),
            screens: options.screens.unwrap_or_else(|| Rc::new(display_screens)),
            apps: options.apps.unwrap_or_else(catalog_lookup),
            runner: options.runner.unwrap_or_default(), own,
            windows: RefCell::default(), row_list: RefCell::default(), choices: RefCell::default(),
            loading: Cell::new(false), requests: Cell::new(0), applied: Cell::new(0),
        });
        let weak = Rc::downgrade(&this);
        this.add.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.open_picker();
            }
        });
        let weak = Rc::downgrade(&this);
        this.cancel.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.close_picker();
            }
        });
        this.rebuild_rows();
        this.read_windows(None);
        this
    }

    pub fn saved(&self) -> WindowPositions { self.data.borrow().clone() }
    pub fn rows(&self) -> Vec<PositionRow> { self.row_list.borrow().clone() }
    /// The last answer from the compositor; None before the first one.
    pub fn windows(&self) -> Option<Windows> { self.windows.borrow().clone() }
    /// The add-window list: each open window and its button.
    pub fn choices(&self) -> Vec<(OpenWindow, gtk::Button)> { self.choices.borrow().clone() }
    pub fn is_loading(&self) -> bool { self.loading.get() }
    pub fn picker_open(&self) -> bool { self.picker.is_visible() }

    /// Read the JSON and the open windows again (page activation).
    pub fn refresh(&self) {
        *self.data.borrow_mut() = WindowPositions::load(&self.config);
        self.rebuild_rows();
        self.read_windows(None);
    }

    /// Ask the compositor for its windows off the GTK thread; `then` runs
    /// with the answer after the page took it.
    pub fn read_windows(&self, then: Option<Box<dyn FnOnce(&Self, &Windows)>>) {
        let request = self.requests.get() + 1;
        self.requests.set(request);
        self.loading.set(true);
        self.sync_status();
        let (source, own, weak) = (self.source.clone(), self.own.clone(), self.me.clone());
        self.runner.run(move || read_windows(&*source, &own), move |result| {
            let Some(this) = weak.upgrade() else { return };
            let windows = result.unwrap_or_else(|| Windows::Unavailable(tr!("The compositor did not respond to the tiling request.")));
            // An older request finishing late does not replace a newer answer.
            if request >= this.applied.get() {
                this.applied.set(request);
                *this.windows.borrow_mut() = Some(windows.clone());
            }
            if request == this.requests.get() {
                this.loading.set(false);
            }
            this.sync_windows();
            if let Some(then) = then {
                then(&this, &windows);
            }
        });
    }

    /// Show the list of open windows (read now).
    pub fn open_picker(&self) {
        self.show_error(None);
        self.picker.set_visible(true);
        self.add.set_sensitive(false);
        self.fill_picker(&[]);
        self.read_windows(None);
    }

    pub fn close_picker(&self) {
        self.picker.set_visible(false);
        self.add.set_sensitive(true);
        self.fill_picker(&[]);
        self.sync_status();
    }

    /// Save where `window` is now, replacing its app's earlier entry.
    pub fn choose(&self, window: &OpenWindow) -> bool {
        let name = (self.apps)(&window.app_id).name;
        let mut data = self.saved();
        data.set(Position::from_window(window, &name));
        let ok = self.apply(data);
        if ok {
            self.close_picker();
        }
        ok
    }

    /// Read the app's open window again and save where it is.
    pub fn update(&self, app_id: &str) {
        self.show_error(None);
        let app_id = app_id.to_owned();
        self.read_windows(Some(Box::new(move |this, windows| {
            match windows {
                Windows::Unsupported => this.show_error(Some(unsupported_text())),
                Windows::Unavailable(error) => this.show_error(Some(tr!("Could not read the open windows: {error}", error = error))),
                Windows::Open(_) => match windows.of_app(&app_id) {
                    Some(window) => {
                        this.choose(window);
                    }
                    None => {
                        let name = this.display_name(&app_id);
                        this.show_error(Some(tr!("{app} has no open window now.", app = name)));
                    }
                },
            }
        })));
    }

    /// Forget the app's position.
    pub fn remove(&self, app_id: &str) -> bool {
        let mut data = self.saved();
        if !data.remove(app_id) {
            return false;
        }
        self.apply(data)
    }

    /// Save the JSON and rc.xml's block; on failure the previous state stays.
    fn apply(&self, data: WindowPositions) -> bool {
        let previous = self.saved();
        if let Err(error) = data.save(&self.config) {
            self.show_error(Some(tr!("Could not save the window positions: {error}", error = error)));
            return false;
        }
        let rc = (self.rc)();
        let written = match write_block_with(&rc, block_text(&data.positions).as_deref(), &fsx::os_replace) {
            Ok(written) => written,
            Err(error) => {
                let _ = previous.save(&self.config);
                self.show_error(Some(tr!("Could not apply the window positions: {error}", error = error)));
                return false;
            }
        };
        *self.data.borrow_mut() = data;
        self.rebuild_rows();
        let mut message = None;
        if written == BlockWrite::Written {
            if let (false, error) = (self.reconfigure)() {
                let error = format!("labwc did not reload rc.xml: {}", error.unwrap_or_default());
                message = Some(tr!("Could not apply the window positions: {error}", error = error));
            }
        }
        self.show_error(message);
        true
    }

    fn show_error(&self, message: Option<String>) {
        self.error.set_text(message.as_deref().unwrap_or(""));
        self.error.set_visible(message.is_some());
    }

    fn display_name(&self, app_id: &str) -> String {
        let label = (self.apps)(app_id);
        if label.name != app_id {
            return label.name;
        }
        self.data.borrow().get(app_id).map(|p| p.name.clone()).unwrap_or(label.name)
    }

    fn rebuild_rows(&self) {
        crate::util::clear_box(&self.rows);
        let screens = (self.screens)();
        let mut list = Vec::new();
        for position in self.data.borrow().positions.iter() {
            let label = (self.apps)(&position.app_id);
            let name_text = if label.name != position.app_id { label.name.clone() } else { position.name.clone() };
            let widget = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(12)
                .css_classes(["tessel-window-position"]).build();
            widget.append(&app_image(label.icon.as_ref(), 32));
            let text_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true)
                .valign(gtk::Align::Center).build();
            let name = gtk::Label::builder().label(&name_text).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            let screen = screens.iter().find(|s| s.connector == position.output);
            let place = screen.map(|s| s.label.clone()).unwrap_or_else(|| position.output.clone());
            let detail_text = if position.maximized {
                tr!("{screen}, maximized", screen = place)
            } else {
                tr!("{screen}, {width} × {height}", screen = place, width = position.width, height = position.height)
            };
            let detail = gtk::Label::builder().label(&detail_text).xalign(0.0).wrap(true).css_classes(["settings-muted"]).build();
            let note = muted(&tr!("This screen is not connected now. The position is kept for when it is back."));
            note.set_visible(screen.is_none());
            text_box.append(&name);
            text_box.append(&detail);
            text_box.append(&note);
            widget.append(&text_box);
            let update = gtk::Button::builder().label(tr!("Update")).valign(gtk::Align::Center).visible(false)
                .tooltip_text(tr!("Save where its open window is now")).build();
            crate::util::accessible_label(&update, &format!("{} – {}", tr!("Update"), name_text));
            let remove = gtk::Button::builder().label(tr!("Remove")).valign(gtk::Align::Center).build();
            crate::util::accessible_label(&remove, &format!("{} – {}", tr!("Remove"), name_text));
            widget.append(&update);
            widget.append(&remove);
            let (weak, app_id) = (self.me.clone(), position.app_id.clone());
            update.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.update(&app_id);
                }
            });
            let (weak, app_id) = (self.me.clone(), position.app_id.clone());
            remove.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.remove(&app_id);
                }
            });
            self.rows.append(&widget);
            list.push(PositionRow { app_id: position.app_id.clone(), widget, name, detail, note, update, remove });
        }
        self.empty.set_visible(list.is_empty());
        self.rows.set_visible(!list.is_empty());
        *self.row_list.borrow_mut() = list;
        self.sync_windows();
    }

    /// Update buttons and the open list follow the last answer.
    fn sync_windows(&self) {
        let windows = self.windows();
        for row in self.row_list.borrow().iter() {
            row.update.set_visible(windows.as_ref().is_some_and(|w| w.of_app(&row.app_id).is_some()));
        }
        if self.picker.is_visible() && !self.loading.get() {
            self.fill_picker(windows.as_ref().map(Windows::open).unwrap_or_default());
        }
        self.sync_status();
    }

    fn fill_picker(&self, windows: &[OpenWindow]) {
        crate::util::clear_box(&self.picker_list);
        let mut choices = Vec::new();
        for window in windows {
            let label = (self.apps)(&window.app_id);
            let button = gtk::Button::builder().css_classes(["flat"]).build();
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            content.append(&app_image(label.icon.as_ref(), 24));
            let text_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(1).valign(gtk::Align::Center).build();
            text_box.append(&gtk::Label::builder().label(&label.name).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build());
            if !window.title.is_empty() && window.title != label.name {
                text_box.append(&gtk::Label::builder().label(&window.title).xalign(0.0).max_width_chars(50)
                    .ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["settings-muted"]).build());
            }
            content.append(&text_box);
            button.set_child(Some(&content));
            let accessible = if window.title.is_empty() { label.name.clone() } else { format!("{} – {}", label.name, window.title) };
            crate::util::accessible_label(&button, &accessible);
            let (weak, chosen) = (self.me.clone(), window.clone());
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.choose(&chosen);
                }
            });
            self.picker_list.append(&button);
            choices.push((window.clone(), button));
        }
        self.picker_list.set_visible(!choices.is_empty());
        *self.choices.borrow_mut() = choices;
    }

    /// The one-line state under the button: while choosing, or when labwc is too old.
    fn sync_status(&self) {
        let windows = self.windows();
        let message = match (self.picker.is_visible(), self.loading.get(), windows.as_ref()) {
            (_, _, Some(Windows::Unsupported)) => Some(unsupported_text()),
            (true, true, _) => Some(tr!("Looking for open windows…")),
            (true, false, Some(Windows::Unavailable(error))) => Some(tr!("Could not read the open windows: {error}", error = error)),
            (true, false, Some(Windows::Open(open))) if open.is_empty() =>
                Some(tr!("No open windows to add. Open the app and place its window first.")),
            _ => None,
        };
        self.status.set_text(message.as_deref().unwrap_or(""));
        self.status.set_visible(message.is_some());
    }

    /// Whether rc.xml already matches the saved JSON (checked, never rewritten, on load).
    pub fn rc_in_sync(&self) -> bool { rc_in_sync(&(self.rc)(), &self.data.borrow()) }
}

/// Shown when the compositor cannot place windows yet.
pub fn unsupported_text() -> String { tr!("Window positions need a newer labwc; after updating it, sign out and back in.") }
