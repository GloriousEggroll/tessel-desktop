//! `color_controls.py`: GTK4 colour controls shared by Displays and
//! Settings → Displays (HDR details, SDR ICC profile, calibration, depth).

use crate::color_config::{self, ColorSettings, Status};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use tessel_core::pyfmt::{format_value, Arg};
use tessel_core::tr;

type Section = BTreeMap<String, String>;

/// `nits(key)`: `f"{value:,.1f}"` without trailing zeros, for finite values > 0.
pub fn nits(active: &Section, key: &str) -> Option<String> {
    let raw = active.get(key).cloned().unwrap_or_else(|| "0".into());
    let value = tessel_core::pyjson::py_float(&serde_json::Value::String(raw))?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let text = format_value(&Arg::Float(value), ",.1f").ok()?;
    Some(text.trim_end_matches('0').trim_end_matches('.').to_owned())
}

/// `(hdr box visible, HDR detail rows, correction error)` of `refresh()`.
pub fn hdr_report(active: &Section, hdr: bool) -> (bool, Vec<(String, String)>, String) {
    let yes = |key: &str| active.get(key).map(String::as_str) == Some("true");
    let (peak, average, white) = (nits(active, "hdr_peak_nits"), nits(active, "hdr_max_frame_average_nits"),
                                  nits(active, "hdr_reference_white_nits"));
    let hdr_active = yes("hdr") && active.get("enabled").map(String::as_str).unwrap_or("true") == "true";
    let visible = hdr || hdr_active || peak.is_some() || yes("hdr_supported");
    let unavailable = tr!("Unavailable");
    let shown = |value: &Option<String>| match value {
        Some(n) => tr!("{nits} nits", nits = n.as_str()),
        None => unavailable.clone(),
    };
    let mut rows = vec![(tr!("Peak brightness"), shown(&peak))];
    if average.is_some() {
        rows.push((tr!("Frame-average limit"), shown(&average)));
    }
    if hdr_active {
        rows.push((tr!("Reference white"), shown(&white)));
        if yes("hdr_calibration_active") {
            if let Some(peak) = nits(active, "hdr_target_peak_nits") {
                rows.push((tr!("Calibrated peak"), tr!("{nits} nits", nits = peak)));
            }
            if let Some(full) = nits(active, "hdr_target_full_frame_nits") {
                rows.push((tr!("Calibrated full-screen limit"), tr!("{nits} nits", nits = full)));
            }
        }
        rows.push((tr!("Output correction"), if yes("hdr_correction_active") { tr!("Active") } else { tr!("None") }));
    }
    let correction = if hdr_active { active.get("hdr_correction_error").cloned().unwrap_or_default() } else { String::new() };
    (visible, rows, correction)
}

/// The status label lines of `refresh()`.
pub fn status_messages(status: &Status, connector: &str, error: &str, hdr: bool, profile: &str, changed: bool) -> Vec<String> {
    let empty = Section::new();
    let supported = status.get("compositor").and_then(|c| c.get("icc_supported")).map(String::as_str) == Some("true");
    let active = status.get(connector).unwrap_or(&empty);
    let mut messages = Vec::new();
    if !error.is_empty() {
        messages.push(error.to_owned());
    }
    if status.is_empty() {
        messages.push(tr!("Color controls require the updated labwc compositor. Start a session with that build to use them."));
    } else if !supported {
        messages.push(tr!("ICC profiles require the Vulkan renderer. SDR color depth can still be configured."));
    }
    if !active.is_empty() {
        let get = |key: &str| active.get(key).cloned().unwrap_or_default();
        if !get("error").is_empty() {
            messages.push(get("error"));
        }
        if get("icc_active") == "true" {
            messages.push(tr!("Active profile: {profile}", profile = get("profile")));
        } else if get("hdr") == "true" && !get("profile").is_empty() {
            messages.push(tr!("HDR is active. The saved SDR profile is suspended until HDR is turned off."));
        } else {
            messages.push(tr!(if get("hdr") == "true" { "Active color: HDR" } else { "Active color: built-in SDR" }));
        }
        let depth = active.get("bit_depth").cloned().unwrap_or_else(|| "?".into());
        messages.push(tr!("Active output depth: {depth}-bit", depth = depth));
    }
    if hdr && !profile.is_empty() {
        messages.push(tr!("The SDR profile and calibration curves apply only with HDR off."));
    }
    if changed {
        messages.push(tr!("Color changes are pending until Apply."));
    }
    messages
}

pub struct ColorControls {
    pub root: gtk::Box,
    pub connector: String,
    pub config_path: PathBuf,
    changed_callback: Option<Rc<dyn Fn()>>,
    pub hdr: Cell<bool>,
    pub error: RefCell<String>,
    pub initial: RefCell<ColorSettings>,
    pub hdr_box: gtk::Box,
    pub hdr_calibrate: gtk::Button,
    pub hdr_grid: gtk::Grid,
    pub hdr_error: gtk::Label,
    hdr_rows: RefCell<Vec<(String, String)>>,
    pub profile: gtk::Entry,
    pub browse: gtk::Button,
    pub reset: gtk::Button,
    pub calibration: gtk::Switch,
    pub depth: gtk::DropDown,
    pub status: gtk::Label,
    #[allow(deprecated)]
    chooser: RefCell<Option<gtk::FileChooserNative>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

fn row(parent: &gtk::Box, title: &str, control: &impl IsA<gtk::Widget>, expand: bool) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("settings-row");
    let label = gtk::Label::builder().label(title).xalign(0.0).hexpand(!expand).valign(gtk::Align::Center).build();
    label.set_size_request(if expand { 180 } else { -1 }, -1);
    row.append(&label);
    control.set_valign(gtk::Align::Center);
    control.set_halign(if expand { gtk::Align::Fill } else { gtk::Align::End });
    row.append(control);
    parent.append(&row);
    row
}

/// The installed `tessel-hdr-calibration` program, or `None` when it is not
/// installed (`$TESSEL_DISPLAYS_HDR_CALIBRATION` overrides).
pub fn hdr_calibration_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TESSEL_DISPLAYS_HDR_CALIBRATION").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    let program = tessel_core::paths::program("tessel-hdr-calibration");
    if program.is_absolute() { Some(program) } else { glib::find_program_in_path(&program) }
}

impl ColorControls {
    pub fn new(connector: &str, config_path: PathBuf, changed: Option<Rc<dyn Fn()>>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let mut error = String::new();
        let initial = match color_config::read_colors(&config_path) {
            Ok(colors) => colors.get(connector).cloned().unwrap_or_default(),
            Err(e) => {
                error = e.to_string();
                ColorSettings::default()
            }
        };
        // HDR details: a two-column table of what the monitor reports and what
        // the compositor targets, with the calibration button beside its title.
        let hdr_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        hdr_box.add_css_class("hdr-details");
        hdr_box.set_tooltip_text(Some(&tr!("Peak and frame-average limits come from the monitor’s HDR metadata. Reference white is the compositor’s white target. Measuring emitted screen luminance requires a colorimeter.")));
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let title = gtk::Label::builder().label(tr!("HDR details")).xalign(0.0).hexpand(true).valign(gtk::Align::Center).build();
        title.add_css_class("hdr-details-title");
        heading.append(&title);
        let hdr_calibrate = gtk::Button::builder().label(tr!("Calibrate HDR…")).valign(gtk::Align::Center).build();
        heading.append(&hdr_calibrate);
        hdr_box.append(&heading);
        let hdr_grid = gtk::Grid::builder().column_spacing(24).row_spacing(6).build();
        hdr_box.append(&hdr_grid);
        let hdr_error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        hdr_error.add_css_class("error");
        hdr_box.append(&hdr_error);
        root.append(&hdr_box);

        let profile = gtk::Entry::builder().text(initial.profile.as_str()).editable(false).hexpand(true).build();
        profile.set_placeholder_text(Some(&tr!("Built-in display color")));
        profile.set_tooltip_text(Some(&tr!("ICC profile measured for this monitor and its current picture mode.")));
        let browse = gtk::Button::with_label(&tr!("Choose profile…"));
        let reset = gtk::Button::with_label(&tr!("Clear"));
        let chooser_box = gtk::Box::builder().spacing(8).hexpand(true).build();
        chooser_box.append(&profile);
        chooser_box.append(&browse);
        chooser_box.append(&reset);
        row(&root, &tr!("SDR display profile"), &chooser_box, true);
        let calibration = gtk::Switch::builder().active(initial.calibration).build();
        calibration.set_tooltip_text(Some(&tr!("Apply the profile’s video card calibration curves once, after color conversion.")));
        row(&root, &tr!("Apply calibration curves"), &calibration, false);
        let depth = gtk::DropDown::from_strings(&[&tr!("8-bit"), &tr!("10-bit")]);
        depth.set_selected(if initial.sdr_bit_depth == 10 { 1 } else { 0 });
        depth.set_tooltip_text(Some(&tr!("10-bit SDR reduces quantization without enabling HDR. Hardware support is required.")));
        row(&root, &tr!("SDR color depth"), &depth, false);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).selectable(true).build();
        status.add_css_class("dim-label");
        root.append(&status);

        let this = Rc::new(Self {
            root, connector: connector.to_owned(), config_path, changed_callback: changed, hdr: Cell::new(false),
            error: RefCell::new(error), initial: RefCell::new(initial), hdr_box, hdr_calibrate, hdr_grid, hdr_error,
            hdr_rows: RefCell::new(Vec::new()), profile, browse, reset, calibration, depth, status,
            chooser: RefCell::new(None), monitor: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        this.hdr_calibrate.connect_clicked(Self::weak_fn(&weak, |c| c.calibrate_hdr()));
        this.browse.connect_clicked(Self::weak_fn(&weak, |c| c.choose()));
        this.reset.connect_clicked(Self::weak_fn(&weak, |c| c.set_profile("")));
        let w = weak.clone();
        this.calibration.connect_active_notify(move |_| { if let Some(c) = w.upgrade() { c.changed_signal() } });
        let w = weak.clone();
        this.depth.connect_selected_notify(move |_| { if let Some(c) = w.upgrade() { c.changed_signal() } });
        if let Some(path) = color_config::status_path() {
            if path.parent().is_some_and(|p| p.is_dir()) {
                let directory = gio::File::for_path(path.parent().unwrap());
                if let Ok(monitor) = directory.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
                    let w = weak.clone();
                    monitor.connect_changed(move |_, file, other, _| {
                        let matches = file.path().as_deref() == Some(path.as_path())
                            || other.and_then(|o| o.path()).as_deref() == Some(path.as_path());
                        if matches {
                            if let Some(c) = w.upgrade() {
                                c.refresh();
                            }
                        }
                    });
                    this.monitor.replace(Some(monitor));
                }
            }
        }
        this.refresh();
        this
    }

    fn weak_fn(weak: &Weak<Self>, f: impl Fn(&Rc<Self>) + 'static) -> impl Fn(&gtk::Button) + 'static {
        let weak = weak.clone();
        move |_| {
            if let Some(c) = weak.upgrade() {
                f(&c);
            }
        }
    }

    /// The HDR details shown, as (name, value) pairs.
    pub fn hdr_rows(&self) -> Vec<(String, String)> {
        self.hdr_rows.borrow().clone()
    }

    fn show_hdr_rows(&self, rows: Vec<(String, String)>) {
        if *self.hdr_rows.borrow() == rows {
            return;
        }
        while let Some(child) = self.hdr_grid.first_child() {
            self.hdr_grid.remove(&child);
        }
        for (index, (name, value)) in rows.iter().enumerate() {
            let key = gtk::Label::builder().label(name.as_str()).xalign(0.0).build();
            key.add_css_class("dim-label");
            let shown = gtk::Label::builder().label(value.as_str()).xalign(1.0).hexpand(true).selectable(true).build();
            shown.add_css_class("hdr-value");
            self.hdr_grid.attach(&key, 0, index as i32, 1, 1);
            self.hdr_grid.attach(&shown, 1, index as i32, 1, 1);
        }
        self.hdr_rows.replace(rows);
    }

    fn calibrate_hdr(&self) {
        let result = if let Some(app) = hdr_calibration_path() {
            use std::os::unix::process::CommandExt;
            std::process::Command::new(app).arg("--output").arg(&self.connector).process_group(0)
                .spawn().map(|_| ()).map_err(|e| tessel_core::pyerr::os_error_text(&e))
        } else {
            Err(tr!("Install HDR Calibration to use this control."))
        };
        if let Err(error) = result {
            self.error.replace(error);
            self.refresh();
        }
    }

    pub fn get_state(&self) -> ColorSettings {
        ColorSettings { profile: self.profile.text().to_string(), calibration: self.calibration.is_active(),
                        sdr_bit_depth: if self.depth.selected() == 1 { 10 } else { 8 } }
    }

    pub fn changed(&self) -> bool {
        self.get_state() != *self.initial.borrow()
    }

    pub fn saved(&self) {
        self.initial.replace(self.get_state());
        self.error.replace(String::new());
        self.refresh();
    }

    fn changed_signal(&self) {
        self.refresh();
        if let Some(callback) = &self.changed_callback {
            callback();
        }
    }

    pub fn set_profile(&self, path: &str) {
        if let Err(error) = color_config::validate_profile(path) {
            self.error.replace(error.to_string());
            self.refresh();
            return;
        }
        self.error.replace(String::new());
        self.profile.set_text(path);
        self.changed_signal();
    }

    #[allow(deprecated)]
    fn choose(self: &Rc<Self>) {
        let parent = self.root.root().and_downcast::<gtk::Window>();
        let chooser = gtk::FileChooserNative::new(Some(&tr!("Choose display ICC profile")), parent.as_ref(),
                                                  gtk::FileChooserAction::Open, Some(&tr!("Choose")), Some(&tr!("Cancel")));
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&tr!("ICC display profiles")));
        for pattern in ["*.icc", "*.icm", "*.ICC", "*.ICM"] {
            filter.add_pattern(pattern);
        }
        chooser.add_filter(&filter);
        let weak = Rc::downgrade(self);
        chooser.connect_response(move |chooser, response| {
            let Some(this) = weak.upgrade() else { return };
            if response == gtk::ResponseType::Accept {
                if let Some(path) = chooser.file().and_then(|f| f.path()) {
                    this.set_profile(&path.to_string_lossy());
                }
            }
            chooser.destroy();
            this.chooser.replace(None);
        });
        chooser.show();
        self.chooser.replace(Some(chooser));
    }

    pub fn refresh(&self) {
        let status = color_config::read_status();
        let supported = status.get("compositor").and_then(|c| c.get("icc_supported")).map(String::as_str) == Some("true");
        let has_profile = !self.profile.text().is_empty();
        self.browse.set_sensitive(supported);
        self.reset.set_sensitive(supported && has_profile);
        self.calibration.set_sensitive(supported && has_profile);
        self.depth.set_sensitive(!status.is_empty());
        let empty = Section::new();
        let active = status.get(&self.connector).unwrap_or(&empty);
        let (visible, rows, correction) = hdr_report(active, self.hdr.get());
        self.hdr_box.set_visible(visible);
        self.show_hdr_rows(rows);
        self.hdr_error.set_text(&correction);
        self.hdr_error.set_visible(!correction.is_empty());
        let messages = status_messages(&status, &self.connector, &self.error.borrow(), self.hdr.get(),
                                       &self.profile.text(), self.changed());
        self.status.set_text(&messages.join("\n"));
    }

    #[allow(deprecated)]
    pub fn close(&self) {
        if let Some(monitor) = self.monitor.take() {
            monitor.cancel();
        }
        if let Some(chooser) = self.chooser.take() {
            chooser.destroy();
        }
    }
}
