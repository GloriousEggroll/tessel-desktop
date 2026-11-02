//! GTK4 screenshot and recorder tools, sharing the suite's live appearance
//! (capture_ui.py `CaptureWindow`).

use crate::app::AppInner;
use crate::area::{Rectangle, ScreenSelection};
use crate::media::{self, Crop, RecordEvent, Recorder, RecordingOptions, Sources};
use crate::native::{Feed, StreamProps};
use crate::portal::{self, PortalSession};
use crate::{settings, shortcuts};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Instant;
use tessel_core::tr;
use tessel_gtk::app_menu::{AboutDetails, MenuTarget, Shortcut, ShortcutSection};
use tessel_gtk::utils::{accessible, add_css, headerbar, image, set_margins};

pub const STYLE: &str = "
.capture-window .subtitle { opacity: .75; }
.capture-window .card { border-radius: 12px; padding: 14px; border: 1px solid alpha(@theme_fg_color,.12); }
.capture-window .record-status { font-size: 11pt; font-weight: 600; }
.capture-window .capture-mode button { padding: 6px 18px; }
.capture-window .capture-mode button:checked { background-color: alpha(currentColor, .16); font-weight: 600; }
.capture-window .preview { background: alpha(@theme_fg_color,.04); border-radius: 10px; }
.capture-window .error { color: #e97171; }
";

const SCREENSHOT_TARGETS: [&str; 4] = ["Current workspace · all screens", "Current screen", "Current window", "Custom area"];
const RECORD_TARGETS: [&str; 3] = ["Current screen", "Current window", "Custom area"];
const DROPDOWNS: [&str; 6] = ["codec", "color", "encoder", "quality", "container", "format"];
const NUMBERS: [&str; 5] = ["delay", "fps", "peak", "bitrate", "duration"];
const BACKENDS: [&str; 3] = ["auto", "cpu", "vulkan"];

/// Desktop capture bindings are user-configurable, so list the saved ones.
pub fn shortcut_help() -> Vec<ShortcutSection> {
    let bound: Vec<Shortcut> = shortcuts::load(None).into_iter().filter(|(_, v)| !v.is_empty())
        .map(|(name, value)| Shortcut::new(&shortcuts::display(&value), shortcuts::title(name))).collect();
    vec![("Desktop shortcuts".into(), bound), editor_shortcuts()]
}

pub fn editor_shortcuts() -> ShortcutSection {
    ("Screenshot editor".into(), vec![Shortcut::new("<Control>z", "Undo"), Shortcut::new("<Control>c", "Copy to clipboard"),
        Shortcut::new("<Control>s", "Save"), Shortcut::new("<Control><Shift>s", "Save As…"), Shortcut::new("Escape", "Close the editor")])
}

fn dropdown(values: &[&str]) -> gtk::DropDown {
    let strings: Vec<String> = values.iter().map(|v| tr!(*v)).collect();
    let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
    gtk::DropDown::from_strings(&refs)
}

fn set_strings(widget: &gtk::DropDown, values: &[String]) {
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    widget.set_model(Some(&gtk::StringList::new(&refs)));
}

fn number(value: f64, low: f64, high: f64, step: f64) -> gtk::SpinButton {
    let adjustment = gtk::Adjustment::new(value, low, high, step, step * 5.0, 0.0);
    gtk::SpinButton::builder().adjustment(&adjustment).digits(0).build()
}

/// Linked Screenshot / Screen recording toggle with DropDown's `selected` API.
pub struct ModeSwitch {
    pub root: gtk::Box,
    pub buttons: Vec<gtk::ToggleButton>,
    selected: Rc<Cell<u32>>,
    changed: Rc<RefCell<Option<Box<dyn Fn()>>>>,
}

impl ModeSwitch {
    fn new(labels: &[(&str, &str)]) -> Self {
        let root = gtk::Box::builder().halign(gtk::Align::Center).build();
        root.add_css_class("linked");
        root.add_css_class("capture-mode");
        let selected = Rc::new(Cell::new(0u32));
        let changed: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let mut buttons: Vec<gtk::ToggleButton> = Vec::new();
        for (index, (icon, label)) in labels.iter().enumerate() {
            let content = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
            content.append(&image(icon, 16));
            content.append(&gtk::Label::new(Some(&tr!(*label))));
            let button = gtk::ToggleButton::builder().child(&content).active(index == 0).build();
            accessible(&button, &tr!(*label));
            if let Some(first) = buttons.first() {
                button.set_group(Some(first));
            }
            let (selected, changed) = (selected.clone(), changed.clone());
            button.connect_toggled(move |button| {
                if button.is_active() && selected.get() != index as u32 {
                    selected.set(index as u32);
                    if let Some(callback) = changed.borrow().as_ref() {
                        callback();
                    }
                }
            });
            root.append(&button);
            buttons.push(button);
        }
        Self { root, buttons, selected, changed }
    }

    pub fn selected(&self) -> u32 {
        self.selected.get()
    }

    pub fn set_selected(&self, index: u32) {
        self.buttons[index as usize].set_active(true);
    }
}

fn row(container: &gtk::Box, title: &str, widget: &impl IsA<gtk::Widget>, detail: Option<&str>) -> gtk::Box {
    let line = gtk::Box::builder().spacing(16).build();
    let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(3).hexpand(true).valign(gtk::Align::Center).build();
    labels.append(&gtk::Label::builder().label(tr!(title)).xalign(0.0).build());
    if let Some(detail) = detail {
        let label = gtk::Label::builder().label(tr!(detail)).xalign(0.0).wrap(true).build();
        label.add_css_class("subtitle");
        label.set_max_width_chars(44);
        labels.append(&label);
    }
    line.append(&labels);
    line.append(widget);
    widget.set_valign(gtk::Align::Center);
    accessible(widget.upcast_ref::<gtk::Widget>(), &tr!(title));
    container.append(&line);
    line
}

fn card(parent: &gtk::Box) -> gtk::Box {
    let container = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    container.add_css_class("card");
    parent.append(&container);
    container
}

fn hint(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(tr!(text)).xalign(0.0).wrap(true).build();
    label.add_css_class("subtitle");
    label
}

/// `Path(text).expanduser()`.
pub fn expand_user(text: &str) -> PathBuf {
    if text == "~" {
        return crate::compat::home();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return crate::compat::home().join(rest);
    }
    PathBuf::from(text)
}

/// Pictures/Screenshots or Videos/Recordings (GLib special dirs, else `~`).
pub fn default_folder(recording: bool) -> String {
    let directory = glib::user_special_dir(if recording { glib::UserDirectory::Videos } else { glib::UserDirectory::Pictures })
        .unwrap_or_else(crate::compat::home);
    directory.join(if recording { "Recordings" } else { "Screenshots" }).to_string_lossy().into_owned()
}

/// `f'{stem}_{datetime.now():%Y-%m-%d_%H-%M-%S_%f}.{extension}'`.
pub fn filename(directory: &Path, stem: &str, extension: &str) -> PathBuf {
    let now = glib::DateTime::now_local().expect("local time");
    directory.join(format!("{stem}_{}_{:06}.{extension}", now.format("%Y-%m-%d_%H-%M-%S").unwrap(), now.microsecond()))
}

thread_local! {
    static WINDOWS: RefCell<HashMap<u64, Weak<Inner>>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

pub struct Widgets {
    pub header: gtk::HeaderBar,
    pub mode: ModeSwitch,
    pub options: gtk::Box,
    pub target: gtk::DropDown,
    pub delay: gtk::SpinButton,
    pub cursor: gtk::Switch,
    pub peak: gtk::SpinButton,
    pub screenshot_options: gtk::Box,
    pub format: gtk::DropDown,
    pub arm: gtk::Button,
    pub setting_switches: Vec<(&'static str, gtk::Switch)>,
    pub record_options: gtk::Box,
    pub codec: gtk::DropDown,
    pub color: gtk::DropDown,
    pub fps: gtk::SpinButton,
    pub encoder: gtk::DropDown,
    pub quality: gtk::DropDown,
    pub bitrate: gtk::SpinButton,
    pub container: gtk::DropDown,
    pub duration: gtk::SpinButton,
    pub system_audio: gtk::DropDown,
    pub mic_audio: gtk::DropDown,
    pub directory: gtk::Entry,
    pub start: gtk::Button,
    pub stop: gtk::Button,
    pub status: gtk::Label,
    pub preview: gtk::Picture,
    pub result_row: gtk::Box,
    pub copy: gtk::Button,
    pub edit: gtk::Button,
    pub open: gtk::Button,
    pub folder: gtk::Button,
    pub shortcut_entries: RefCell<Vec<(&'static str, gtk::Entry)>>,
    pub menu_button: RefCell<Option<gtk::MenuButton>>,
}

impl Widgets {
    fn dropdown(&self, name: &str) -> &gtk::DropDown {
        match name {
            "codec" => &self.codec,
            "color" => &self.color,
            "encoder" => &self.encoder,
            "quality" => &self.quality,
            "container" => &self.container,
            _ => &self.format,
        }
    }

    fn number(&self, name: &str) -> &gtk::SpinButton {
        match name {
            "delay" => &self.delay,
            "fps" => &self.fps,
            "peak" => &self.peak,
            "bitrate" => &self.bitrate,
            _ => &self.duration,
        }
    }
}

pub type ArmedSource = (PortalSession, u32, StreamProps);

pub struct Inner {
    pub id: u64,
    pub app: Weak<AppInner>,
    pub window: gtk::ApplicationWindow,
    pub w: Widgets,
    pub closed: Cell<bool>,
    pub sessions: RefCell<Vec<PortalSession>>,
    pub feeds: RefCell<Vec<Arc<dyn Feed>>>,
    pub armed_sessions: RefCell<Vec<PortalSession>>,
    pub armed_sources: RefCell<Vec<ArmedSource>>,
    pub generation: Cell<u64>,
    pub busy: Cell<bool>,
    pub lock_capture: Cell<bool>,
    pub recorder: RefCell<Option<Recorder>>,
    pub capture_recording: Cell<bool>,
    countdown_timer: RefCell<Option<glib::SourceId>>,
    pub elapsed_timer: RefCell<Option<glib::SourceId>>,
    pub last_file: RefCell<Option<PathBuf>>,
    pub last_kind: RefCell<Option<String>>,
    pub png: RefCell<Option<Vec<u8>>>,
    pub area: Cell<Option<Crop>>,
    pub area_logical: Cell<Option<(Rectangle, (i32, i32))>>,
    pub selector: RefCell<Option<ScreenSelection>>,
    pub quick_capture: Cell<bool>,
    dock_popup_held: Cell<bool>,
    pub saved_audio: RefCell<Map<String, Value>>,
    pub mode_preferences: RefCell<Map<String, Value>>,
    last_mode: RefCell<Option<String>>,
    pub config_path: RefCell<PathBuf>,
    pub arming: Cell<bool>,
    pub save_directory: RefCell<PathBuf>,
    pub capture_options: RefCell<RecordingOptions>,
    pub image_extension: RefCell<String>,
    pub kind: RefCell<String>,
    pub output_queue: RefCell<Vec<String>>,
    pub notification_output: RefCell<String>,
    notification_probe: RefCell<Option<tessel_gtk::placement::PointerMonitor>>,
    notification_pointer_resolved: Cell<bool>,
    record_started: Cell<Option<Instant>>,
    pub system_sources: RefCell<Sources>,
    pub mic_sources: RefCell<Sources>,
    _css: gtk::CssProvider,
    _theme: tessel_gtk::theme::WindowTheme,
}

#[derive(Clone)]
pub struct CaptureWindow(pub Rc<Inner>);

macro_rules! weak_call {
    ($this:expr, |$w:ident| $body:expr) => {{
        let weak = Rc::downgrade(&$this.0);
        move || {
            if let Some(inner) = weak.upgrade() {
                let $w = CaptureWindow(inner);
                $body;
            }
        }
    }};
}

impl CaptureWindow {
    pub fn new(app: &Rc<AppInner>) -> Self {
        let window = gtk::ApplicationWindow::builder().application(&app.gtk).title(tr!("Screenshot & Screen Recorder")).build();
        window.add_css_class("capture-window");
        window.set_default_size(650, 760);
        let header = headerbar(&tr!("Capture"));
        window.set_titlebar(Some(&header));
        // Above the palette stylesheet (APPLICATION + 30) so the chrome rules win.
        let css = add_css(STYLE, Some(&WidgetExt::display(&window)), tessel_gtk::utils::PRIORITY_APPLICATION + 31);
        let theme = tessel_gtk::theme::attach(&window, None);
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(16).build();
        set_margins(&root, 20);
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).build();
        scroll.set_child(Some(&root));
        let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        window.set_child(Some(&outer));
        scroll.set_vexpand(true);
        outer.append(&scroll);
        let footer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        set_margins(&footer, 20);
        footer.set_margin_top(12);
        outer.append(&footer);
        let mode = ModeSwitch::new(&[("camera-photo-symbolic", "Screenshot"), ("media-record-symbolic", "Screen recording")]);
        accessible(&mode.root, &tr!("Capture mode"));
        root.append(&mode.root);
        let options = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).build();
        root.append(&options);
        let common = card(&options);
        let target = dropdown(&SCREENSHOT_TARGETS);
        target.set_selected(1);
        row(&common, "Area", &target, None);
        common.append(&hint("Screen uses the monitor under the pointer. Workspace captures all screens. Drag to select a custom area; Escape cancels."));
        let delay = number(0.0, 0.0, 3600.0, 1.0);
        row(&common, "Delay (seconds)", &delay, None);
        let cursor = gtk::Switch::builder().active(true).build();
        row(&common, "Include pointer", &cursor, None);
        let peak = number(1000.0, 100.0, 10000.0, 100.0);
        row(&common, "HDR peak brightness (nits)", &peak, Some("Used when converting an HDR source to SDR."));
        let screenshot_options = card(&options);
        let format = dropdown(&["PNG", "JPEG"]);
        row(&screenshot_options, "Image format", &format, Some("Screenshots are always saved in SDR (sRGB)."));
        let arm = gtk::Button::with_label(&tr!("Enable Print Screen while locked"));
        screenshot_options.append(&arm);
        screenshot_options.append(&hint("Approve screen access before locking. Print Screen then saves the visible lock screen. Access ends when you disable it or quit Capture."));
        let config_path = settings::config_path();
        let mut setting_switches = Vec::new();
        for key in settings::KEYS {
            let (line, switch) = settings::switch_row(key, Some(config_path.clone()));
            screenshot_options.append(&line);
            setting_switches.push((key, switch));
        }
        let record_options = card(&options);
        let codec = dropdown(&["H.264", "H.265 / HEVC", "AV1"]);
        row(&record_options, "Video codec", &codec, None);
        let color = dropdown(&["SDR · convert HDR to SDR", "HDR · 10-bit PQ / BT.2020"]);
        row(&record_options, "Color", &color, None);
        let fps = number(30.0, 1.0, 120.0, 1.0);
        row(&record_options, "Frames per second", &fps, None);
        let encoder = dropdown(&["Automatic · Vulkan preferred", "Software", "Vulkan hardware"]);
        row(&record_options, "Encoder", &encoder, Some("Uses FFmpeg. H.264 software encoding uses OpenH264."));
        let quality = dropdown(&["High", "Balanced", "Small file"]);
        quality.set_selected(1);
        row(&record_options, "Quality", &quality, None);
        let bitrate = number(0.0, 0.0, 200.0, 1.0);
        row(&record_options, "Bitrate (Mbit/s)", &bitrate, Some("0 uses the selected quality."));
        let container = dropdown(&["Matroska (.mkv)", "MP4 (.mp4)"]);
        row(&record_options, "Container", &container, None);
        let duration = number(0.0, 0.0, 86400.0, 1.0);
        row(&record_options, "Stop after (seconds)", &duration, Some("0 records until you press Stop."));
        let system_audio = dropdown(&["No system audio"]);
        let mic_audio = dropdown(&["No microphone"]);
        row(&record_options, "System audio", &system_audio, None);
        row(&record_options, "Microphone", &mic_audio, None);
        let output = card(&options);
        let directory = gtk::Entry::builder().hexpand(true).build();
        directory.set_text(&default_folder(false));
        let browse = gtk::Button::builder().child(&image("folder-open-symbolic", 16)).tooltip_text(tr!("Choose save folder")).build();
        accessible(&browse, &tr!("Choose save folder"));
        accessible(&directory, &tr!("Save to"));
        let address = gtk::Box::builder().spacing(6).build();
        address.append(&directory);
        address.append(&browse);
        output.append(&gtk::Label::builder().label(tr!("Save to")).xalign(0.0).build());
        output.append(&address);
        let actions = gtk::Box::builder().spacing(10).build();
        let start = gtk::Button::builder().label(tr!("Take screenshot")).hexpand(true).build();
        start.add_css_class("suggested-action");
        let stop = gtk::Button::builder().label(tr!("Cancel")).visible(false).build();
        actions.append(&start);
        actions.append(&stop);
        footer.append(&actions);
        let status = gtk::Label::builder().label(tr!("Ready")).xalign(0.0).wrap(true).selectable(true).build();
        status.add_css_class("record-status");
        footer.append(&status);
        let preview = gtk::Picture::builder().can_shrink(true).height_request(180).visible(false).build();
        preview.set_content_fit(gtk::ContentFit::Contain);
        preview.add_css_class("preview");
        root.append(&preview);
        // Result actions appear once something has been captured.
        let result_row = gtk::Box::builder().spacing(8).visible(false).build();
        let copy = gtk::Button::builder().label(tr!("Copy image")).sensitive(false).build();
        let edit = gtk::Button::builder().label(tr!("Edit…")).sensitive(false).build();
        let open = gtk::Button::builder().label(tr!("Open file")).sensitive(false).build();
        let folder = gtk::Button::builder().label(tr!("Open folder")).sensitive(false).build();
        for button in [&copy, &edit, &open, &folder] {
            result_row.append(button);
        }
        footer.append(&result_row);
        let id = NEXT_ID.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        let this = Self(Rc::new(Inner {
            id, app: Rc::downgrade(app), window: window.clone(),
            w: Widgets { header: header.clone(), mode, options, target, delay, cursor, peak, screenshot_options, format, arm: arm.clone(),
                setting_switches, record_options, codec, color, fps, encoder, quality, bitrate, container, duration, system_audio, mic_audio,
                directory, start: start.clone(), stop: stop.clone(), status, preview, result_row, copy: copy.clone(), edit: edit.clone(),
                open: open.clone(), folder: folder.clone(), shortcut_entries: RefCell::default(), menu_button: RefCell::default() },
            closed: Cell::new(false), sessions: RefCell::default(), feeds: RefCell::default(), armed_sessions: RefCell::default(),
            armed_sources: RefCell::default(), generation: Cell::new(0), busy: Cell::new(false), lock_capture: Cell::new(false),
            recorder: RefCell::default(), capture_recording: Cell::new(false), countdown_timer: RefCell::default(),
            elapsed_timer: RefCell::default(), last_file: RefCell::default(), last_kind: RefCell::default(), png: RefCell::default(),
            area: Cell::new(None), area_logical: Cell::new(None), selector: RefCell::default(), quick_capture: Cell::new(false),
            dock_popup_held: Cell::new(false), saved_audio: RefCell::default(), mode_preferences: RefCell::default(),
            last_mode: RefCell::default(), config_path: RefCell::new(config_path), arming: Cell::new(false),
            save_directory: RefCell::default(), capture_options: RefCell::default(), image_extension: RefCell::new("png".into()),
            kind: RefCell::default(), output_queue: RefCell::default(), notification_output: RefCell::default(),
            notification_probe: RefCell::default(), notification_pointer_resolved: Cell::new(false), record_started: Cell::new(None),
            system_sources: RefCell::default(), mic_sources: RefCell::default(), _css: css, _theme: theme,
        }));
        WINDOWS.with(|map| map.borrow_mut().insert(id, Rc::downgrade(&this.0)));
        *this.0.w.mode.changed.borrow_mut() = Some(Box::new(weak_call!(this, |w| w.mode_changed())));
        this.0.w.color.connect_selected_notify({
            let f = weak_call!(this, |w| w.color_changed());
            move |_| f()
        });
        arm.connect_clicked({
            let f = weak_call!(this, |w| w.arm_clicked());
            move |_| f()
        });
        browse.connect_clicked({
            let f = weak_call!(this, |w| w.browse());
            move |_| f()
        });
        start.connect_clicked({
            let f = weak_call!(this, |w| w.prepare(false, None, true));
            move |_| f()
        });
        stop.connect_clicked({
            let f = weak_call!(this, |w| w.stop_clicked());
            move |_| f()
        });
        copy.connect_clicked({
            let f = weak_call!(this, |w| w.copy_image(false));
            move |_| f()
        });
        edit.connect_clicked({
            let f = weak_call!(this, |w| w.edit_image());
            move |_| f()
        });
        open.connect_clicked({
            let f = weak_call!(this, |w| w.open_file());
            move |_| f()
        });
        folder.connect_clicked({
            let f = weak_call!(this, |w| w.open_folder());
            move |_| f()
        });
        let weak = Rc::downgrade(&this.0);
        window.connect_close_request(move |_| match weak.upgrade() {
            Some(inner) if CaptureWindow(inner.clone()).close_requested() => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        let weak = Rc::downgrade(&this.0);
        this.worker(|| Ok(media::audio_sources()), move |window, sources| {
            if weak.upgrade().is_some() {
                window.audio_ready(sources);
            }
        }, false, None);
        this.load_preferences();
        this.mode_changed();
        this.add_shortcuts(&root);
        let quit: Rc<dyn Fn()> = Rc::new(weak_call!(this, |w| w.quit_requested()));
        let menu = tessel_gtk::app_menu::install_app_menu(&app.gtk, &window, MenuTarget::New, AboutDetails {
            name: "Screenshot & Screen Recorder".into(), icon: "camera-photo".into(), version: None,
            comments: Some("Screenshots, annotations and screen recordings".into()), website: None,
        }, shortcut_help(), Some(&header), Some(quit));
        *this.0.w.menu_button.borrow_mut() = Some(menu);
        window.connect_visible_notify({
            let f = weak_call!(this, |w| w.refresh_settings());
            move |_| f()
        });
        this
    }

    fn app(&self) -> Option<Rc<AppInner>> {
        self.0.app.upgrade()
    }

    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.0.window
    }

    fn add_shortcuts(&self, root: &gtk::Box) {
        let container = card(root);
        container.append(&gtk::Label::builder().label(tr!("Keyboard shortcuts")).xalign(0.0).build());
        container.append(&hint("Use Ctrl, Alt, Shift or Super plus a key (for example Alt+F9). Leave blank to disable. Capture shortcuts use the delay above."));
        let mut entries = Vec::new();
        for (name, value) in shortcuts::load(None) {
            let entry = gtk::Entry::builder().text(shortcuts::display(&value)).width_chars(18).build();
            row(&container, shortcuts::title(name), &entry, None);
            entries.push((name, entry));
        }
        *self.0.w.shortcut_entries.borrow_mut() = entries;
        let buttons = gtk::Box::builder().spacing(8).build();
        let reset = gtk::Button::with_label(&tr!("Defaults"));
        reset.connect_clicked({
            let f = weak_call!(self, |w| {
                for (key, entry) in w.0.w.shortcut_entries.borrow().iter() {
                    entry.set_text(&shortcuts::display(shortcuts::default(key)));
                }
            });
            move |_| f()
        });
        let apply = gtk::Button::with_label(&tr!("Apply shortcuts"));
        apply.connect_clicked({
            let f = weak_call!(self, |w| w.apply_shortcuts());
            move |_| f()
        });
        buttons.append(&reset);
        buttons.append(&apply);
        container.append(&buttons);
    }

    /// Global Settings may have changed these while the window was hidden.
    pub fn refresh_settings(&self) {
        let path = self.0.config_path.borrow().clone();
        for (key, switch) in &self.0.w.setting_switches {
            let value = settings::get(key, Some(&path));
            if switch.is_active() != value {
                switch.set_active(value);
            }
        }
    }

    pub fn quit_requested(&self) {
        if self.0.recorder.borrow().is_some() || self.0.busy.get() {
            self.0.window.present();
            self.message(&tr!("Stop the current capture before quitting."), true);
            return;
        }
        let Some(app) = self.app() else { return };
        for window in app.gtk.windows() {
            if &window != self.0.window.upcast_ref::<gtk::Window>() {
                window.close();
            }
        }
        if !self.0.armed_sources.borrow().is_empty() {
            self.disarm();
        }
        self.close_requested();
        app.gtk.quit();
    }

    pub fn apply_shortcuts(&self) {
        let values: shortcuts::Values = self.0.w.shortcut_entries.borrow().iter().map(|(k, e)| (*k, e.text().to_string())).collect();
        match shortcuts::apply(&values, None, None) {
            Ok(values) => {
                for (key, value) in values {
                    if let Some((_, entry)) = self.0.w.shortcut_entries.borrow().iter().find(|(k, _)| *k == key) {
                        entry.set_text(&shortcuts::display(&value));
                    }
                }
                self.save_preferences();
                if let Some(app) = self.app() {
                    tessel_gtk::app_menu::set_shortcuts(&app.gtk, shortcut_help());
                }
                self.message(&tr!("Keyboard shortcuts updated."), false);
            }
            Err(error) => self.message(&error, true),
        }
    }

    pub fn recording(&self) -> bool {
        self.0.w.mode.selected() == 1
    }

    pub fn mode_changed(&self) {
        self.remember_mode();
        let recording = self.recording();
        let key = if recording { "record" } else { "screenshot" };
        let saved = self.0.mode_preferences.borrow().get(key).cloned().unwrap_or_else(|| json!({}));
        *self.0.last_mode.borrow_mut() = Some(key.into());
        self.0.window.set_title(Some(&if recording { tr!("Screen Recorder") } else { tr!("Screenshot") }));
        self.0.w.record_options.set_visible(recording);
        self.0.w.screenshot_options.set_visible(!recording);
        self.0.w.start.set_label(&if recording { tr!("Start recording") } else { tr!("Take screenshot") });
        self.update_results();
        let directory = saved.get("directory").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| default_folder(recording));
        self.0.w.directory.set_text(&directory);
        // Workspace composition is a still-image operation; recording selects one stream.
        let values: Vec<String> = if recording { RECORD_TARGETS.iter().map(|v| tr!(*v)).collect() } else { SCREENSHOT_TARGETS.iter().map(|v| tr!(*v)).collect() };
        set_strings(&self.0.w.target, &values);
        let fallback = if recording { 0 } else { 1 };
        let target = match saved.get("target") {
            Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap(),
            Some(_) => -1,
            None => fallback,
        };
        self.0.w.target.set_selected(if (0..values.len() as i64).contains(&target) { target as u32 } else { 0 });
    }

    /// Show the result actions only for an existing capture of this mode.
    pub fn update_results(&self) {
        let kind = self.0.last_kind.borrow().clone();
        let current = if self.recording() { "record" } else { "screenshot" };
        self.0.w.result_row.set_visible(self.0.last_file.borrow().is_some() && kind.as_deref() == Some(current));
        // Images can be copied and edited; recordings only opened.
        self.0.w.copy.set_visible(current == "screenshot");
        self.0.w.edit.set_visible(current == "screenshot");
        self.0.w.preview.set_visible(current == "screenshot" && self.0.png.borrow().is_some() && kind.as_deref() == Some("screenshot")
                                     && self.0.w.preview.paintable().is_some());
    }

    fn remember_mode(&self) {
        let last = self.0.last_mode.borrow().clone();
        if let Some(last) = last {
            self.0.mode_preferences.borrow_mut().insert(last, json!({"directory": self.0.w.directory.text().as_str(),
                                                                     "target": self.0.w.target.selected()}));
        }
    }

    fn color_changed(&self) {
        if self.0.w.color.selected() == 1 && self.0.w.codec.selected() == 0 {
            self.0.w.codec.set_selected(1);
        }
    }

    pub fn audio_ready(&self, sources: Sources) {
        *self.0.system_sources.borrow_mut() = sources.iter().filter(|(n, _)| n.ends_with(".monitor")).cloned().collect();
        *self.0.mic_sources.borrow_mut() = sources.iter().filter(|(n, _)| !n.ends_with(".monitor")).cloned().collect();
        for (widget, entries, empty, key) in [(&self.0.w.system_audio, self.0.system_sources.borrow().clone(), "No system audio", "system"),
                                              (&self.0.w.mic_audio, self.0.mic_sources.borrow().clone(), "No microphone", "microphone")] {
            let mut values = vec![tr!(empty)];
            values.extend(entries.iter().map(|(_, d)| d.clone()));
            set_strings(widget, &values);
            let saved = self.0.saved_audio.borrow().get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
            widget.set_selected(entries.iter().position(|(name, _)| *name == saved).map(|i| i as u32 + 1).unwrap_or(0));
        }
    }

    fn selected_audio(&self) -> Map<String, Value> {
        let mut selected = self.0.saved_audio.borrow().clone();
        for (key, widget, sources) in [("system", &self.0.w.system_audio, self.0.system_sources.borrow().clone()),
                                       ("microphone", &self.0.w.mic_audio, self.0.mic_sources.borrow().clone())] {
            if !sources.is_empty() {
                let index = widget.selected() as usize;
                let value = if index > 0 && index <= sources.len() { sources[index - 1].0.clone() } else { String::new() };
                selected.insert(key.into(), Value::String(value));
            }
        }
        selected
    }

    pub fn start_recording(&self, preset: Option<String>, hide_window: bool) {
        if self.0.busy.get() {
            return;
        }
        self.0.w.mode.set_selected(1);
        self.0.arming.set(false);
        self.0.capture_recording.set(true);
        if hide_window {
            self.0.window.set_visible(false);
        }
        let selected = self.selected_audio();
        self.set_busy(true);
        let system = selected.get("system").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let microphone = selected.get("microphone").and_then(|v| v.as_str()).unwrap_or("").to_string();
        self.worker(move || media::recording_audio(preset.as_deref(), &system, &microphone), move |window, (sources, system, microphone)| {
            *window.0.saved_audio.borrow_mut() = Map::from_iter([("system".to_string(), Value::String(system)),
                                                                 ("microphone".to_string(), Value::String(microphone))]);
            window.audio_ready(sources);
            window.set_busy(false);
            window.prepare(false, None, hide_window);
        }, true, None);
    }

    /// Run `function` on a worker thread; deliver on the main thread unless the
    /// window closed or a newer capture started (then `dispose` the result).
    pub fn worker<T: Send + 'static>(&self, function: impl FnOnce() -> Result<T, String> + Send + 'static,
                                     ready: impl FnOnce(&CaptureWindow, T) + 'static, report_error: bool,
                                     dispose: Option<Box<dyn FnOnce(T)>>) {
        let generation = self.0.generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(function).await.unwrap_or_else(|_| Err("capture worker failed".into()));
            let window = weak.upgrade().map(CaptureWindow);
            let current = window.as_ref().is_some_and(|w| !w.0.closed.get() && w.0.generation.get() == generation);
            if !current {
                if let (Some(dispose), Ok(value)) = (dispose, result) {
                    dispose(value);
                }
                return;
            }
            let window = window.unwrap();
            match result {
                Ok(value) => ready(&window, value),
                Err(error) => {
                    if report_error {
                        window.fail(&error);
                    }
                }
            }
        });
    }

    pub fn preferences(&self) -> Map<String, Value> {
        self.remember_mode();
        let audio = self.selected_audio();
        let mut result = settings::values(Some(&self.0.config_path.borrow()));
        result.insert("mode_preferences".into(), Value::Object(self.0.mode_preferences.borrow().clone()));
        result.insert("audio".into(), Value::Object(audio));
        for name in DROPDOWNS {
            result.insert(name.into(), json!(self.0.w.dropdown(name).selected()));
        }
        for name in NUMBERS {
            result.insert(name.into(), json!(self.0.w.number(name).value_as_int()));
        }
        result.insert("cursor".into(), json!(self.0.w.cursor.is_active()));
        result.insert("encoder_backend".into(), json!(BACKENDS[(self.0.w.encoder.selected() as usize).min(2)]));
        result
    }

    pub fn load_preferences(&self) {
        let path = self.0.config_path.borrow().clone();
        let Ok(text) = std::fs::read(&path) else { return };
        let Ok(Value::Object(mut values)) = serde_json::from_slice::<Value>(&text) else { return };
        if let Some(Value::Object(modes)) = values.get("mode_preferences") {
            *self.0.mode_preferences.borrow_mut() = modes.iter()
                .filter(|(k, v)| matches!(k.as_str(), "record" | "screenshot") && v.is_object())
                .map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        if let Some(Value::Object(audio)) = values.get("audio") {
            *self.0.saved_audio.borrow_mut() = audio.iter().filter(|(_, v)| v.is_string()).map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        // Migrate both retired hardware choices without changing software or
        // Automatic preferences. Named values avoid future index drift.
        let backend = values.get("encoder_backend").and_then(|v| v.as_str()).and_then(|b| BACKENDS.iter().position(|x| *x == b));
        if let Some(index) = backend {
            values.insert("encoder".into(), json!(index));
        } else if matches!(values.get("encoder").and_then(Value::as_i64), Some(2 | 3)) && values.get("encoder").is_some_and(Value::is_i64) {
            values.insert("encoder".into(), json!(2));
        }
        for (name, value) in &values {
            let int = if value.is_i64() || value.is_u64() { value.as_i64() } else { None };
            if DROPDOWNS.contains(&name.as_str()) {
                if let Some(index) = int {
                    let widget = self.0.w.dropdown(name);
                    let count = widget.model().map(|m| m.n_items()).unwrap_or(0) as i64;
                    if (0..count).contains(&index) {
                        widget.set_selected(index as u32);
                    }
                }
            } else if NUMBERS.contains(&name.as_str()) {
                if let Some(number) = int {
                    self.0.w.number(name).set_value(number as f64);
                }
            } else if name == "cursor" {
                if let Value::Bool(flag) = value {
                    self.0.w.cursor.set_active(*flag);
                }
            }
        }
    }

    pub fn save_preferences(&self) {
        let path = self.0.config_path.borrow().clone();
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let temporary = path.with_extension("tmp");
        let text = crate::compat::json_indent2(&Value::Object(self.preferences())) + "\n";
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, &path);
        }
    }

    fn browse(&self) {
        let dialog = gtk::FileDialog::builder().title(tr!("Save captures in")).build();
        let weak = Rc::downgrade(&self.0);
        dialog.select_folder(Some(&self.0.window), None::<&gio::Cancellable>, move |result| {
            let (Some(inner), Ok(file)) = (weak.upgrade(), result) else { return };
            if let Some(path) = file.path() {
                inner.w.directory.set_text(&path.to_string_lossy());
            }
        });
    }

    pub fn options_snapshot(&self) -> Result<RecordingOptions, String> {
        let mut audio = Vec::new();
        for (widget, sources) in [(&self.0.w.system_audio, self.0.system_sources.borrow().clone()), (&self.0.w.mic_audio, self.0.mic_sources.borrow().clone())] {
            let index = widget.selected() as usize;
            if index > 0 && index <= sources.len() {
                audio.push(sources[index - 1].0.clone());
            }
        }
        let w = &self.0.w;
        let options = RecordingOptions {
            codec: ["h264", "h265", "av1"][(w.codec.selected() as usize).min(2)].into(), hdr: w.color.selected() == 1,
            fps: w.fps.value_as_int() as i64, encoder: BACKENDS[(w.encoder.selected() as usize).min(2)].into(),
            quality: [18, 23, 28][(w.quality.selected() as usize).min(2)], bitrate: w.bitrate.value_as_int() as i64,
            container: ["mkv", "mp4"][(w.container.selected() as usize).min(1)].into(), audio, peak: w.peak.value_as_int() as i64,
            duration: w.duration.value_as_int() as i64, crop: None,
        };
        options.validate()?;
        Ok(options)
    }

    fn tray_update(&self, recording: bool, armed: bool, busy: bool, stopping: bool) {
        if let Some(app) = self.app() {
            if let Some(tray) = app.tray.borrow().as_ref() {
                tray.update(recording, armed, busy, stopping, self.0.capture_recording.get());
            }
        }
    }

    pub fn set_busy(&self, busy: bool) {
        self.0.busy.set(busy);
        let w = &self.0.w;
        w.mode.root.set_sensitive(!busy);
        w.options.set_sensitive(!busy);
        w.start.set_sensitive(!busy);
        w.stop.set_visible(busy);
        let recording = self.0.recorder.borrow().is_some();
        w.stop.set_label(&if recording { tr!("Stop & save") } else { tr!("Cancel") });
        self.tray_update(recording, !self.0.armed_sources.borrow().is_empty(), busy, false);
    }

    pub fn message(&self, message: &str, error: bool) {
        self.0.w.status.set_text(message);
        if error {
            self.0.w.status.add_css_class("error");
        } else {
            self.0.w.status.remove_css_class("error");
        }
    }

    pub fn prepare(&self, arm: bool, target: Option<String>, hide_window: bool) {
        if self.0.busy.get() {
            return;
        }
        self.0.capture_recording.set(self.recording() && target.is_none() && !arm);
        let prepared = (|| -> Result<(), String> {
            let mut directory = self.0.w.directory.text().to_string();
            if target.is_some() && self.recording() {
                directory = self.0.mode_preferences.borrow().get("screenshot").and_then(|v| v.get("directory")).and_then(|v| v.as_str())
                    .map(String::from).unwrap_or_else(|| default_folder(false));
            }
            let folder = expand_user(&directory);
            if !folder.is_absolute() {
                return Err(tr!("Choose an absolute save folder."));
            }
            *self.0.save_directory.borrow_mut() = folder;
            let options = if self.0.capture_recording.get() {
                self.options_snapshot()?
            } else {
                RecordingOptions { peak: self.0.w.peak.value_as_int() as i64, ..Default::default() }
            };
            *self.0.capture_options.borrow_mut() = options;
            Ok(())
        })();
        if let Err(error) = prepared {
            self.message(&error, true);
            if self.0.quick_capture.get() {
                self.notify(&tr!("Capture failed"), &error, false, None);
            } else {
                self.0.window.present();
            }
            return;
        }
        self.save_preferences();
        *self.0.image_extension.borrow_mut() = if self.0.w.format.selected() != 0 { "jpg" } else { "png" }.into();
        self.0.arming.set(arm);
        self.set_busy(true);
        let kind = match target {
            Some(target) => target,
            None if arm => "workspace".into(),
            None => {
                let index = self.0.w.target.selected() as usize;
                if self.recording() { ["screen", "window", "area"][index.min(2)].into() } else { ["workspace", "screen", "window", "area"][index.min(3)].into() }
            }
        };
        *self.0.kind.borrow_mut() = kind;
        self.0.area.set(None);
        self.0.area_logical.set(None);
        self.0.output_queue.borrow_mut().clear();
        self.0.notification_output.borrow_mut().clear();
        self.0.notification_probe.borrow_mut().take();
        self.0.notification_pointer_resolved.set(false);
        if hide_window {
            self.0.window.set_visible(false);
        }
        // Popover grabs otherwise prevent the pointer probe and area selector
        // from receiving input until the user manually dismisses the popup.
        self.dismiss_popovers();
        self.message(&tr!("Preparing capture…"), false);
        let generation = self.0.generation.get();
        let weak = Rc::downgrade(&self.0);
        self.0.dock_popup_held.set(!self.0.capture_recording.get());
        let bus = self.app().and_then(|a| a.gtk.dbus_connection());
        crate::popups::prepare_dock_popup(move || {
            glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                if let Some(inner) = weak.upgrade() {
                    let window = CaptureWindow(inner);
                    if window.0.busy.get() && generation == window.0.generation.get() {
                        window.choose_target();
                    }
                }
            });
        }, bus, self.0.capture_recording.get());
    }

    pub fn dismiss_popovers(&self) {
        fn visit(widget: &gtk::Widget) {
            let mut child = widget.first_child();
            while let Some(current) = child {
                let following = current.next_sibling();
                match current.downcast_ref::<gtk::Popover>() {
                    Some(popover) if popover.get_visible() => popover.popdown(),
                    _ => visit(&current),
                }
                child = following;
            }
        }
        visit(self.0.window.upcast_ref());
    }

    pub fn choose_target(&self) {
        let kind = self.0.kind.borrow().clone();
        if !matches!(kind.as_str(), "screen" | "area") && !self.0.notification_pointer_resolved.get() {
            let generation = self.0.generation.get();
            let weak = Rc::downgrade(&self.0);
            let probe = tessel_gtk::placement::PointerMonitor::new(move |monitor| {
                let Some(inner) = weak.upgrade() else { return };
                let window = CaptureWindow(inner);
                window.0.notification_probe.borrow_mut().take();
                if !window.0.busy.get() || generation != window.0.generation.get() {
                    return;
                }
                window.0.notification_pointer_resolved.set(true);
                *window.0.notification_output.borrow_mut() = monitor.and_then(|m| m.connector()).map(|c| c.to_string()).unwrap_or_default();
                window.choose_target();
            });
            *self.0.notification_probe.borrow_mut() = Some(probe);
            return;
        }
        if kind == "workspace" {
            let monitors = WidgetExt::display(&self.0.window).monitors();
            let outputs: Vec<Option<String>> = (0..monitors.n_items())
                .map(|i| monitors.item(i).and_downcast::<gdk::Monitor>().and_then(|m| m.connector()).map(|c| c.to_string())).collect();
            if outputs.is_empty() || outputs.iter().any(|o| o.as_deref().is_none_or(str::is_empty)) {
                self.fail(&tr!("No connected screens are available."));
                return;
            }
            *self.0.output_queue.borrow_mut() = outputs.into_iter().flatten().collect();
            self.select_next();
        } else if matches!(kind.as_str(), "screen" | "area") && !self.0.arming.get() {
            let weak = Rc::downgrade(&self.0);
            match ScreenSelection::new(&WidgetExt::display(&self.0.window), move |result| {
                if let Some(inner) = weak.upgrade() {
                    CaptureWindow(inner).target_selected(result);
                }
            }, kind == "area") {
                Ok(selector) => {
                    if !selector.is_done() {
                        *self.0.selector.borrow_mut() = Some(selector);
                    }
                }
                Err(error) => self.fail(&error),
            }
        } else {
            self.select_next();
        }
    }

    pub fn selector(&self) -> Option<ScreenSelection> {
        self.0.selector.borrow().clone()
    }

    fn target_selected(&self, result: crate::area::Selection) {
        self.0.selector.borrow_mut().take();
        let Some((monitor, rectangle)) = result else {
            self.stop_clicked();
            return;
        };
        let Some(output) = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty()) else {
            self.fail(&tr!("The selected screen is no longer connected."));
            return;
        };
        *self.0.output_queue.borrow_mut() = vec![output];
        if let Some(rectangle) = rectangle {
            let geometry = monitor.geometry();
            self.0.area_logical.set(Some((rectangle, (geometry.width(), geometry.height()))));
        }
        self.select_next();
    }

    fn select_next(&self) {
        if !self.0.busy.get() {
            return;
        }
        let output = {
            let mut queue = self.0.output_queue.borrow_mut();
            if queue.is_empty() { None } else { Some(queue.remove(0)) }
        };
        let ready = {
            let weak = Rc::downgrade(&self.0);
            move |session: &PortalSession| {
                if let Some(inner) = weak.upgrade() {
                    CaptureWindow(inner).source_ready(session);
                }
            }
        };
        let failed = {
            let weak = Rc::downgrade(&self.0);
            move |session: &PortalSession, message: String| {
                if let Some(inner) = weak.upgrade() {
                    CaptureWindow(inner).session_failed(session, &message);
                }
            }
        };
        let bus = self.app().and_then(|a| a.hooks.portal_bus.clone());
        let session = PortalSession::new(ready, failed, portal::Options {
            types: if self.0.kind.borrow().as_str() == "window" { 2 } else { 1 }, output,
            cursor: self.0.w.cursor.is_active() && !self.0.arming.get(), multiple: false, bus,
        });
        if !session.closed() {
            self.0.sessions.borrow_mut().push(session);
        }
    }

    fn session_failed(&self, session: &PortalSession, message: &str) {
        if session.cancelled() {
            self.stop_clicked();
        } else if self.0.armed_sessions.borrow().contains(session) {
            self.disarm();
            self.message(message, true);
        } else {
            self.fail(message);
        }
    }

    fn source_ready(&self, session: &PortalSession) {
        if let Some(first) = session.outputs().into_iter().next() {
            *self.0.notification_output.borrow_mut() = first;
        }
        if !self.0.output_queue.borrow().is_empty() {
            self.select_next();
        } else if self.0.arming.get() {
            self.open_feeds();
        } else {
            self.countdown();
        }
    }

    fn feed_factory(&self) -> crate::native::FeedFactory {
        self.app().map(|a| a.hooks.feed_factory.clone()).unwrap_or_else(crate::native::NativeFeed::factory)
    }

    /// Consumers start AFTER the delay, so the initial frame cannot be a
    /// buffered pre-timer image or contain the selection overlay.
    pub fn open_feeds(&self) {
        let mut sources = Vec::new();
        for session in self.0.sessions.borrow().iter() {
            let Some(remote) = session.remote() else { continue };
            for (node, props) in session.streams() {
                sources.push((remote.clone(), node, props));
            }
        }
        let factory = self.feed_factory();
        self.worker(move || {
            let mut result: Vec<Arc<dyn Feed>> = Vec::new();
            for (remote, node, props) in sources {
                match remote.remote_fd().and_then(|fd| factory(fd, node, props)) {
                    Ok(feed) => result.push(feed),
                    Err(error) => {
                        for feed in &result {
                            feed.close();
                        }
                        return Err(error);
                    }
                }
            }
            Ok(result)
        }, |window, feeds| window.feeds_ready(feeds), true, Some(Box::new(|feeds: Vec<Arc<dyn Feed>>| feeds.iter().for_each(|f| f.close()))));
    }

    fn feeds_ready(&self, feeds: Vec<Arc<dyn Feed>>) {
        self.0.feeds.borrow_mut().extend(feeds);
        let feeds = self.0.feeds.borrow().clone();
        if self.0.arming.get() {
            self.worker(move || {
                for feed in &feeds {
                    feed.frame(5.0, None).map_err(|e| e.to_string())?;
                }
                Ok(())
            }, |window, ()| window.armed(), true, None);
            return;
        }
        let Some(first) = feeds.first().cloned() else {
            self.fail(&tr!("The portal returned no capture sources."));
            return;
        };
        self.worker(move || first.frame(5.0, None).map_err(|e| e.to_string()), |window, frame| {
            // Convert logical selection coordinates to this stream's native pixels.
            if let Some((rectangle, logical)) = window.0.area_logical.get() {
                match crate::area::pixel_rectangle(rectangle, logical, frame.width, frame.height) {
                    Ok(area) => {
                        window.0.area.set(Some(area));
                        window.0.capture_options.borrow_mut().crop = Some(area);
                    }
                    Err(error) => {
                        window.fail(&error);
                        return;
                    }
                }
            }
            if window.0.capture_recording.get() {
                let options = window.0.capture_options.borrow().clone();
                let destination = filename(&window.0.save_directory.borrow(), "Recording", &options.container);
                window.message(&tr!("Starting encoder…"), false);
                let feed = window.0.feeds.borrow()[0].clone();
                let id = window.0.id;
                let notify = Box::new(move |event: RecordEvent| {
                    glib::MainContext::default().invoke(move || {
                        let target = WINDOWS.with(|map| map.borrow().get(&id).and_then(Weak::upgrade));
                        if let Some(inner) = target {
                            CaptureWindow(inner).record_event(event);
                        }
                    });
                });
                let chooser = window.app().map(|a| a.hooks.encoder_chooser.clone());
                let recorder = match chooser {
                    Some(chooser) => Recorder::with_encoder_chooser(feed, options, destination, notify, Box::new(move |o, f| chooser(o, f))),
                    None => Recorder::new(feed, options, destination, notify),
                };
                *window.0.recorder.borrow_mut() = Some(recorder);
                window.set_busy(true);
            } else {
                let job = window.screenshot_job(None, None);
                window.worker(job, |window, result| window.shot_ready(result, false), true, None);
            }
        }, true, None);
    }

    fn countdown(&self) {
        // Use a monotonic deadline; always allow the overlays a repaint to disappear.
        let delay = (self.0.w.delay.value_as_int() as f64).max(0.35);
        let deadline = Instant::now() + std::time::Duration::from_secs_f64(delay);
        self.tick(deadline);
    }

    fn tick(&self, deadline: Instant) {
        self.0.countdown_timer.borrow_mut().take();
        if !self.0.busy.get() {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now()).as_secs_f64();
        if remaining > 0.0 {
            self.message(&tr!("Capturing in {seconds} seconds…", seconds = remaining.ceil() as i64), false);
            let wait = ((remaining * 1000.0) as u64 + 1).clamp(1, 100);
            let weak = Rc::downgrade(&self.0);
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(wait), move || {
                if let Some(inner) = weak.upgrade() {
                    inner.countdown_timer.borrow_mut().take();
                    CaptureWindow(inner).tick(deadline);
                }
            });
            *self.0.countdown_timer.borrow_mut() = Some(source);
        } else {
            self.message(&tr!("Capturing…"), false);
            self.open_feeds();
        }
    }

    fn cancel_countdown(&self) {
        if let Some(source) = self.0.countdown_timer.borrow_mut().take() {
            source.remove();
        }
    }

    /// A Send job taking a screenshot of `feeds` (default: the open feeds).
    pub fn screenshot_job(&self, feeds: Option<Vec<Arc<dyn Feed>>>, not_before: Option<f64>)
                          -> impl FnOnce() -> Result<(PathBuf, Vec<u8>), String> + Send + 'static {
        let feeds = feeds.unwrap_or_else(|| self.0.feeds.borrow().clone());
        let area = self.0.area.get();
        let peak = self.0.capture_options.borrow().peak;
        let extension = self.0.image_extension.borrow().clone();
        let directory = self.0.save_directory.borrow().clone();
        move || take_screenshot(&feeds, not_before, area, peak, &extension, &directory)
    }

    pub fn shot_ready(&self, result: (PathBuf, Vec<u8>), locked: bool) {
        let (path, png) = result;
        *self.0.last_file.borrow_mut() = Some(path.clone());
        *self.0.png.borrow_mut() = Some(png.clone());
        *self.0.last_kind.borrow_mut() = Some("screenshot".into());
        for button in [&self.0.w.copy, &self.0.w.edit, &self.0.w.open, &self.0.w.folder] {
            button.set_sensitive(true);
        }
        self.update_results();
        if !locked {
            if let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from(&png)) {
                self.0.w.preview.set_paintable(Some(&texture));
            }
            self.0.w.preview.set_visible(true);
            self.release_sources();
            if !self.0.quick_capture.get() {
                self.0.window.present();
            }
        }
        self.set_busy(false);
        self.0.lock_capture.set(false);
        self.message(&tr!("Saved {path}", path = path.to_string_lossy().into_owned()), false);
        if !locked {
            // Lock-screen captures stay off the clipboard of the locked session.
            let config = self.0.config_path.borrow().clone();
            let copied = settings::copy_enabled(Some(&config));
            if copied {
                self.copy_image(true);
            }
            let title = if copied { tr!("Screenshot saved and copied") } else { tr!("Screenshot saved") };
            self.notify(&title, &path.to_string_lossy(), false, Some(&path));
            if settings::editor_enabled(Some(&config)) {
                self.edit_image();
            }
        }
    }

    fn arm_clicked(&self) {
        if !self.0.armed_sources.borrow().is_empty() {
            self.disarm();
        } else {
            self.prepare(true, None, true);
        }
    }

    fn armed(&self) {
        self.disarm();
        let sessions: Vec<PortalSession> = self.0.sessions.borrow_mut().drain(..).collect();
        *self.0.armed_sources.borrow_mut() = sessions.iter()
            .flat_map(|session| session.streams().into_iter().map(move |(node, props)| (session.clone(), node, props))).collect();
        *self.0.armed_sessions.borrow_mut() = sessions;
        for feed in self.0.feeds.borrow_mut().drain(..) {
            feed.close();
        }
        self.0.w.arm.set_label(&tr!("Disable Print Screen while locked"));
        self.set_busy(false);
        self.message(&tr!("Ready for Print Screen on the lock screen. Keep Capture running; it can be minimized."), false);
        self.0.window.present();
    }

    pub fn disarm(&self) {
        for session in self.0.armed_sessions.borrow_mut().drain(..) {
            session.close();
        }
        self.0.armed_sources.borrow_mut().clear();
        self.0.w.arm.set_label(&tr!("Enable Print Screen while locked"));
        let recording = self.0.recorder.borrow().is_some();
        self.tray_update(recording, false, false, false);
        self.message(&tr!("Locked screenshot access is disabled."), false);
    }

    pub fn locked_shot(&self, not_before: Option<f64>) -> bool {
        if self.0.closed.get() || self.0.busy.get() || self.0.armed_sources.borrow().is_empty() {
            return false;
        }
        // Armed tessel-lock sources are a workspace snapshot, independent of the
        // most recent ordinary screenshot or recording target.
        *self.0.kind.borrow_mut() = "workspace".into();
        self.0.notification_output.borrow_mut().clear();
        self.0.capture_recording.set(false);
        *self.0.capture_options.borrow_mut() = RecordingOptions { peak: self.0.w.peak.value_as_int() as i64, ..Default::default() };
        *self.0.image_extension.borrow_mut() = if self.0.w.format.selected() != 0 { "jpg" } else { "png" }.into();
        self.0.area.set(None);
        self.0.area_logical.set(None);
        self.0.lock_capture.set(true);
        self.set_busy(true);
        let sources: Vec<_> = self.0.armed_sources.borrow().iter().filter_map(|(s, n, p)| Some((s.remote()?, *n, p.clone()))).collect();
        let factory = self.feed_factory();
        let (peak, extension, directory) = (self.0.capture_options.borrow().peak, self.0.image_extension.borrow().clone(),
                                            self.0.save_directory.borrow().clone());
        self.worker(move || {
            let mut feeds: Vec<Arc<dyn Feed>> = Vec::new();
            let result = (|| {
                for (remote, node, props) in sources {
                    feeds.push(factory(remote.remote_fd()?, node, props)?);
                }
                take_screenshot(&feeds, not_before, None, peak, &extension, &directory)
            })();
            for feed in &feeds {
                feed.close();
            }
            result
        }, |window, result| window.shot_ready(result, true), true, None);
        true
    }

    fn record_event(&self, event: RecordEvent) {
        if self.0.closed.get() {
            return;
        }
        match event {
            RecordEvent::Recording(detail) => {
                self.0.record_started.set(Some(Instant::now()));
                self.message(&tr!("Recording · {detail}", detail = detail), false);
                let weak = Rc::downgrade(&self.0);
                let source = glib::timeout_add_seconds_local(1, move || {
                    let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
                    let window = CaptureWindow(inner);
                    if window.update_elapsed() {
                        glib::ControlFlow::Continue
                    } else {
                        window.0.elapsed_timer.borrow_mut().take();
                        glib::ControlFlow::Break
                    }
                });
                if let Some(old) = self.0.elapsed_timer.borrow_mut().replace(source) {
                    old.remove();
                }
            }
            RecordEvent::Saved(value) => {
                let path = PathBuf::from(&value);
                *self.0.last_file.borrow_mut() = Some(path.clone());
                *self.0.last_kind.borrow_mut() = Some("record".into());
                self.0.w.open.set_sensitive(true);
                self.0.w.folder.set_sensitive(true);
                self.update_results();
                self.finish_recording();
                self.message(&tr!("Saved {path}", path = value.as_str()), false);
                self.notify(&tr!("Recording saved"), &value, true, Some(&path));
            }
            RecordEvent::Cancelled => self.finish_recording(),
            RecordEvent::Error(value) => {
                self.finish_recording();
                self.fail(&value);
            }
        }
    }

    fn update_elapsed(&self) -> bool {
        if self.0.recorder.borrow().is_none() {
            return false;
        }
        let elapsed = self.0.record_started.get().map(|s| s.elapsed().as_secs()).unwrap_or(0);
        self.message(&tr!("Recording · {detail}", detail = format!("{:02}:{:02}", elapsed / 60, elapsed % 60)), false);
        true
    }

    fn finish_recording(&self) {
        self.0.recorder.borrow_mut().take();
        if let Some(source) = self.0.elapsed_timer.borrow_mut().take() {
            source.remove();
        }
        if let Some(app) = self.app() {
            app.gtk.withdraw_notification("capture");
        }
        self.release_sources();
        self.set_busy(false);
    }

    pub fn stop_clicked(&self) {
        if let Some(recorder) = self.0.recorder.borrow().as_ref() {
            recorder.stop();
            let armed = !self.0.armed_sources.borrow().is_empty();
            self.tray_update(true, armed, true, true);
            self.0.w.stop.set_sensitive(false);
            self.message(&tr!("Finishing recording…"), false);
            return;
        }
        self.0.generation.set(self.0.generation.get() + 1);
        self.cancel_countdown();
        self.release_sources();
        self.set_busy(false);
        self.message(&tr!("Capture cancelled."), false);
    }

    pub fn release_sources(&self) {
        if let Some(probe) = self.0.notification_probe.borrow_mut().take() {
            probe.cancel();
        }
        if self.0.dock_popup_held.replace(false) {
            crate::popups::restore_dock_popup(self.app().and_then(|a| a.gtk.dbus_connection()));
        }
        let selector = self.0.selector.borrow_mut().take();
        if let Some(selector) = selector {
            selector.finish(None, false);
        }
        for feed in self.0.feeds.borrow_mut().drain(..) {
            feed.close();
        }
        let sessions: Vec<PortalSession> = self.0.sessions.borrow_mut().drain(..).collect();
        for session in sessions {
            session.close();
        }
        self.0.w.stop.set_sensitive(true);
    }

    pub fn fail(&self, message: &str) {
        if let Some(recorder) = self.0.recorder.borrow().as_ref() {
            recorder.stop();
            self.message(message, true);
            return;
        }
        self.0.generation.set(self.0.generation.get() + 1);
        self.cancel_countdown();
        self.release_sources();
        self.set_busy(false);
        self.message(message, true);
        if !self.0.lock_capture.get() {
            if self.0.quick_capture.get() {
                self.notify(&tr!("Capture failed"), message, false, None);
            } else {
                self.0.window.present();
            }
        }
        self.0.lock_capture.set(false);
    }

    pub fn notify(&self, title: &str, body: &str, recording: bool, saved_file: Option<&Path>) {
        let Some(app) = self.app() else { return };
        if let Some(saved) = saved_file {
            let output = self.0.notification_output.borrow().clone();
            let workspace = self.0.kind.borrow().as_str() == "workspace";
            if let Some(notifications) = app.notifications.borrow().as_ref() {
                notifications.saved(title, saved, &output, workspace, recording);
            }
            return;
        }
        let notification = gio::Notification::new(title);
        notification.set_body(Some(body));
        notification.set_icon(&gio::ThemedIcon::new(if recording { "media-record-symbolic" } else { "camera-photo-symbolic" }));
        notification.set_default_action("app.show");
        if recording {
            notification.add_button(&tr!("Stop & save"), "app.stop");
        }
        app.gtk.send_notification(Some("capture"), &notification);
    }

    pub fn copy_image(&self, quiet: bool) {
        let Some(png) = self.0.png.borrow().clone() else { return };
        let Some(app) = self.app() else { return };
        let weak = Rc::downgrade(&self.0);
        crate::clipboard::copy_image(&app.gtk, png, "image/png", Some(WidgetExt::display(&self.0.window)), Some(Box::new(move |error| {
            let Some(inner) = weak.upgrade() else { return };
            let window = CaptureWindow(inner);
            match error {
                Some(error) => window.message(&tr!("Could not copy the screenshot: {error}", error = error), true),
                None if !quiet => window.message(&tr!("Image copied to the clipboard."), false),
                None => {}
            }
        })));
    }

    pub fn edit_image(&self) {
        let path = self.0.last_file.borrow().clone();
        if let (Some(path), Some(app)) = (path, self.app()) {
            crate::editor::open_editor(&app.gtk, &path);
        }
    }

    pub fn open_file(&self) {
        let path = self.0.last_file.borrow().clone();
        if let (Some(path), Some(app)) = (path, self.app()) {
            (app.hooks.open_saved)(&app.gtk, &crate::results::file_uri(&path), false);
        }
    }

    pub fn open_folder(&self) {
        let path = self.0.last_file.borrow().clone();
        if let (Some(path), Some(app)) = (path, self.app()) {
            (app.hooks.open_saved)(&app.gtk, &crate::results::file_uri(&path), true);
        }
    }

    /// `close-request`: hide while busy/armed (returns true = keep), else tear down.
    pub fn close_requested(&self) -> bool {
        if self.0.busy.get() || !self.0.armed_sources.borrow().is_empty() {
            self.0.window.set_visible(false);
            return true;
        }
        self.save_preferences();
        self.0.closed.set(true);
        self.0.generation.set(self.0.generation.get() + 1);
        self.release_sources();
        self.disarm();
        false
    }
}

/// Worker: read one frame per feed, tone-map, compose and save without overwriting.
pub fn take_screenshot(feeds: &[Arc<dyn Feed>], not_before: Option<f64>, area: Option<Crop>, peak: i64, extension: &str,
                       directory: &Path) -> Result<(PathBuf, Vec<u8>), String> {
    let mut items = Vec::new();
    for feed in feeds {
        let frame = feed.frame(5.0, not_before).map_err(|e| e.to_string())?;
        let crop = if feeds.len() == 1 { area } else { None };
        items.push((media::screenshot(&frame, crop, peak)?, feed.properties().clone()));
    }
    let data = media::compose_screens(items)?;
    let encoded = if extension == "jpg" { crate::imaging::jpeg_with_icc(&crate::imaging::decode_rgb(&data)?)? } else { data.clone() };
    let path = filename(directory, "Screenshot", extension);
    let path = media::save_exclusive(&path, &encoded).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists { tessel_core::pyerr::os_error_path(&e, &path) } else { tessel_core::pyerr::os_error_path(&e, &path) }
    })?;
    Ok((path, data))
}
