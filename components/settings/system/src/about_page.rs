//! about_page.py: operating system and hardware summary, plus a safe shell restart.
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::LazyLock;

use gs_base::widgets::{label, label_with, status_text, Page};
use gs_base::SettingsPage;
use gtk::prelude::*;
use gtk::{gio, glib};
use regex::Regex;
use tessel_core::tr;

use crate::compat;

/// Only the shell's own services restart; applications run in their own
/// systemd units (systemd-run from Tessel Dock/Tessel Shell), so they keep running.
pub const SHELL_SERVICES: &[&str] = &["tessel-dock.service", "tessel-shell.service", "tessel-notifications.service"];
pub const RESTART_COMMAND: &[&str] = &["systemctl", "--user", "--no-block", "try-restart",
                                       "tessel-dock.service", "tessel-shell.service", "tessel-notifications.service"];

/// `runner(argv, done)` (page_widgets.spawn or a test fake).
pub type Runner = Rc<dyn Fn(Vec<String>, Box<dyn FnOnce(bool, String)>)>;

pub fn default_runner() -> Runner {
    Rc::new(|argv, done| {
        gs_base::widgets::spawn(&argv, Some(done), None);
    })
}

pub fn suite_version_files() -> Vec<PathBuf> {
    let data = tessel_core::paths::data_home();
    let mut files = vec![data.join("tessel-settings/VERSION"), data.join("VERSION")];
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        files.push(dir.join("VERSION"));
    }
    files
}

pub fn os_release(paths: &[PathBuf]) -> Vec<(String, String)> {
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let mut values: Vec<(String, String)> = Vec::new();
        for line in compat::splitlines(&text, false) {
            let Some((name, item)) = line.split_once('=') else { continue };
            if compat::strip(name).is_empty() {
                continue;
            }
            let item = if compat::strip(item).is_empty() {
                String::new()
            } else {
                match compat::shlex_split(item) {
                    Ok(words) => words.into_iter().next().unwrap_or_default(),
                    Err(_) => item.trim_matches(['"', '\'']).to_owned(),
                }
            };
            let name = compat::strip(name).to_owned();
            if let Some(slot) = values.iter_mut().find(|(k, _)| *k == name) { slot.1 = item } else { values.push((name, item)) }
        }
        return values;
    }
    Vec::new()
}

fn get<'a>(values: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

pub fn uname() -> (String, String, String) {
    // SAFETY: uname fills a zeroed struct.
    let mut info: libc::utsname = unsafe { std::mem::zeroed() };
    unsafe { libc::uname(&mut info) };
    let field = |f: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(f.as_ptr()) }.to_string_lossy().into_owned();
    (field(&info.nodename), field(&info.release), field(&info.machine))
}

pub fn cpu_name(text: Option<&str>) -> String {
    let owned;
    let text = match text {
        Some(t) => t,
        None => {
            owned = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
            &owned
        }
    };
    for line in compat::splitlines(text, false) {
        let (name, item) = line.split_once(':').unwrap_or((&line, ""));
        if ["model name", "Hardware", "Processor", "cpu model"].contains(&compat::strip(name)) && !compat::strip(item).is_empty() {
            return SPACES.replace_all(compat::strip(item), " ").into_owned();
        }
    }
    uname().2 // platform.processor() is empty on Linux; platform.machine().
}

pub fn memory_bytes(text: Option<&str>) -> u64 {
    let owned;
    let text = match text {
        Some(t) => t,
        None => match std::fs::read_to_string("/proc/meminfo") {
            Ok(t) => {
                owned = t;
                &owned
            }
            Err(_) => return 0,
        },
    };
    Regex::new(r"(?m)^MemTotal:\s+(\d+)\s*kB").unwrap().captures(text)
        .and_then(|c| c[1].parse::<u64>().ok()).map(|v| v * 1024).unwrap_or(0)
}

pub fn short_vendor(vendor: &str) -> String {
    if let Some(found) = Regex::new(r"\[([^\]]+)\]").unwrap().captures(vendor) {
        return found[1].to_owned();
    }
    compat::strip(&Regex::new(r",? (Inc|Corporation|Corp|Co|Ltd|Limited)\.?$").unwrap().replace(vendor, "")).to_owned()
}

pub fn gpus_from_lspci(text: &str) -> Vec<String> {
    let class = Regex::new(r"VGA|3D|Display").unwrap();
    compat::splitlines(text, false).iter().filter_map(|line| {
        let fields = compat::shlex_split(line).ok()?;
        (fields.len() >= 4 && class.is_match(&fields[1])).then(|| format!("{} {}", short_vendor(&fields[2]), fields[3]))
    }).collect()
}

pub fn gpus_from_sysfs(root: &Path) -> Vec<String> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir(root).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    cards.retain(|c| c.file_name().map(|n| n.to_string_lossy()).is_some_and(|n| n.starts_with("card") && n[4..].starts_with(|c: char| c.is_ascii_digit())));
    cards.sort();
    let mut result = Vec::new();
    for card in cards {
        if card.file_name().unwrap().to_string_lossy().contains('-') {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(card.join("device/uevent")) else { continue };
        let uevent: Vec<(String, String)> = compat::splitlines(&text, false).iter()
            .filter_map(|l| l.split_once('=')).map(|(a, b)| (a.to_owned(), b.to_owned())).collect();
        let parts: Vec<&str> = [get(&uevent, "DRIVER"), get(&uevent, "PCI_ID")].into_iter().flatten().filter(|s| !s.is_empty()).collect();
        result.push(parts.join(" "));
    }
    result.into_iter().filter(|s| !s.is_empty()).collect()
}

fn command_output(argv: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new(argv[0]);
    command.args(&argv[1..]);
    crate::input_devices::run_timeout(command, 5).map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn gpus() -> Vec<String> {
    if let Some(output) = command_output(&["lspci", "-mm"]) {
        let found = gpus_from_lspci(&output);
        if !found.is_empty() {
            return found;
        }
    }
    gpus_from_sysfs(Path::new("/sys/class/drm"))
}

pub fn labwc_version_from(output: Option<String>) -> String {
    let Some(output) = output else { return String::new() };
    match Regex::new(r"labwc\s+(\S+)").unwrap().captures(&output) {
        Some(found) => found[1].to_owned(),
        None => compat::strip(&output).to_owned(),
    }
}

pub fn labwc_version() -> String { labwc_version_from(command_output(&["labwc", "--version"])) }

pub fn suite_version(files: &[PathBuf]) -> String {
    for path in files {
        if let Ok(text) = std::fs::read_to_string(path) {
            let value = compat::strip(&text);
            if !value.is_empty() {
                return compat::splitlines(value, false)[0].chars().take(64).collect();
            }
        }
    }
    String::new()
}

pub fn disk_usage(path: &str) -> (u64, u64) {
    let Ok(c) = std::ffi::CString::new(path) else { return (0, 0) };
    // SAFETY: zeroed statvfs filled by the call.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stats) } != 0 {
        return (0, 0);
    }
    (stats.f_blocks as u64 * stats.f_frsize as u64, stats.f_bavail as u64 * stats.f_frsize as u64)
}

/// The values `collect()` gathers.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AboutValues {
    pub os: String,
    pub hostname: String,
    pub kernel: String,
    pub cpu: String,
    pub memory: u64,
    pub gpu: Vec<String>,
    pub disk: (u64, u64),
    pub desktop: String,
    pub labwc: String,
    pub session: String,
}

/// Gather every value (blocking; run off the GTK thread).
pub fn collect() -> AboutValues {
    let release = os_release(&[PathBuf::from("/etc/os-release"), PathBuf::from("/usr/lib/os-release")]);
    let (hostname, kernel, machine) = uname();
    let pretty = get(&release, "PRETTY_NAME").filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| {
        [get(&release, "NAME"), get(&release, "VERSION")].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    });
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    AboutValues {
        os: pretty, hostname, kernel: format!("{kernel} ({machine})"), cpu: format!("{} × {cpus}", cpu_name(None)),
        memory: memory_bytes(None), gpu: gpus(), disk: disk_usage("/"), desktop: suite_version(&suite_version_files()),
        labwc: labwc_version(), session: std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
    }
}

pub const ROWS: &[(&str, &str)] = &[("os", "Operating system"), ("hostname", "Device name"), ("kernel", "Kernel"),
    ("cpu", "Processor"), ("memory", "Memory"), ("gpu", "Graphics"), ("disk", "Disk"),
    ("desktop", "Desktop version"), ("labwc", "Compositor"), ("session", "Windowing system")];

/// `[(English title, text)]` for display and the clipboard.
pub fn format_values(values: &AboutValues) -> Vec<(&'static str, String)> {
    ROWS.iter().map(|(key, title)| {
        let text = match *key {
            "os" => values.os.clone(),
            "hostname" => values.hostname.clone(),
            "kernel" => values.kernel.clone(),
            "cpu" => values.cpu.clone(),
            "memory" if values.memory > 0 => glib::format_size_full(values.memory, glib::FormatSizeFlags::IEC_UNITS).to_string(),
            "gpu" => values.gpu.join("\n"),
            "disk" if values.disk.0 > 0 => tr!("{value1} total, {value2} available", value1 = glib::format_size(values.disk.0).to_string(),
                                                value2 = glib::format_size(values.disk.1).to_string()),
            "desktop" => if values.desktop.is_empty() { tr!("Development build") } else { values.desktop.clone() },
            "labwc" if !values.labwc.is_empty() => format!("labwc {}", values.labwc),
            "session" => match values.session.as_str() { "wayland" => "Wayland".into(), "x11" => "X11".into(), other => other.into() },
            _ => String::new(),
        };
        (*title, if text.is_empty() { tr!("Unknown") } else { text })
    }).collect()
}

pub struct AboutPage {
    pub page: Page,
    pub values: std::cell::RefCell<Vec<(&'static str, String)>>,
    pub card: gtk::Box,
    pub grid: gtk::Grid,
    pub copy: gtk::Button,
    pub copy_status: gtk::Label,
    pub restart: gtk::Button,
    pub restart_status: gtk::Label,
    runner: Runner,
}

pub type Collector = Box<dyn FnOnce() -> AboutValues + Send>;

impl AboutPage {
    pub fn new(collector: Option<Collector>, runner: Option<Runner>) -> Rc<AboutPage> {
        let page = Page::new("about-page", &tr!("About"), &tr!("Information about this computer and its software."), Some("help-about-symbolic"));
        let card = page.group(None, None, None);
        let grid = gtk::Grid::builder().column_spacing(24).row_spacing(10).build();
        card.append(&grid);
        grid.attach(&label(&tr!("Loading…"), Some("detail")), 0, 0, 2, 1);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let copy = gtk::Button::builder().label(tr!("Copy to Clipboard")).sensitive(false).build();
        copy.add_css_class("pill");
        actions.append(&copy);
        page.content.append(&actions);
        let copy_status = label("", Some("detail"));
        page.content.append(&copy_status);
        let shell = page.group(Some(&tr!("Desktop shell")),
                               Some(&tr!("Restart the dock, the desktop shell and notifications if they stop responding. Open applications keep running.")), None);
        let restart = gtk::Button::builder().label(tr!("Restart Desktop Shell…")).halign(gtk::Align::Start).build();
        restart.add_css_class("pill");
        shell.append(&restart);
        let restart_status = label("", Some("detail"));
        shell.append(&restart_status);
        let this = Rc::new(AboutPage { page, values: Default::default(), card, grid, copy, copy_status, restart, restart_status,
                                       runner: runner.unwrap_or_else(default_runner) });
        let weak = Rc::downgrade(&this);
        this.copy.connect_clicked(move |_| if let Some(this) = weak.upgrade() { this.copy_text() });
        let weak = Rc::downgrade(&this);
        this.restart.connect_clicked(move |_| if let Some(this) = weak.upgrade() { this.confirm_restart() });
        let collector = collector.unwrap_or_else(|| Box::new(collect));
        let weak: Weak<AboutPage> = Rc::downgrade(&this);
        this.page.run(move || Ok(collector()), move |result| if let Some(this) = weak.upgrade() { this.loaded(result) });
        this
    }

    fn loaded(&self, result: Result<AboutValues, String>) {
        tessel_gtk::utils::remove_children(&self.grid);
        let values = match result {
            Err(error) => {
                self.grid.attach(&label(&tr!("System information could not be read: {value1}", value1 = error), Some("error")), 0, 0, 2, 1);
                return;
            }
            Ok(values) => format_values(&values),
        };
        for (index, (title, text)) in values.iter().enumerate() {
            let name = label_with(&tr!(title), Some("detail"), false);
            name.set_valign(gtk::Align::Start);
            self.grid.attach(&name, 0, index as i32, 1, 1);
            let value = label(text, None);
            value.set_selectable(true);
            value.set_hexpand(true);
            self.grid.attach(&value, 1, index as i32, 1, 1);
        }
        *self.values.borrow_mut() = values;
        self.copy.set_sensitive(true);
    }

    pub fn text(&self) -> String {
        self.values.borrow().iter().map(|(title, value)| format!("{}: {}", tr!(title), value.replace('\n', "; ")))
            .collect::<Vec<_>>().join("\n") + "\n"
    }

    pub fn copy_text(&self) {
        self.page.widget.clipboard().set_text(&self.text());
        status_text(&self.copy_status, &tr!("Copied to the clipboard."), None);
    }

    pub fn confirm_restart(self: &Rc<Self>) {
        let dialog = gtk::AlertDialog::builder().message(tr!("Restart the desktop shell?"))
            .detail(tr!("The dock, panel and notifications will briefly disappear. Open applications and unsaved work are not affected."))
            .buttons([tr!("Cancel"), tr!("Restart")]).cancel_button(0).default_button(0).build();
        let weak = Rc::downgrade(self);
        let root = self.page.widget.root().and_downcast::<gtk::Window>();
        dialog.choose(root.as_ref(), gio::Cancellable::NONE, move |result| {
            if result == Ok(1) {
                if let Some(this) = weak.upgrade() {
                    this.restart_shell();
                }
            }
        });
    }

    pub fn restart_shell(self: &Rc<Self>) {
        self.restart.set_sensitive(false);
        status_text(&self.restart_status, &tr!("Restarting the desktop shell…"), None);
        let weak = Rc::downgrade(self);
        (self.runner)(RESTART_COMMAND.iter().map(|s| s.to_string()).collect(), Box::new(move |okay, output| {
            let Some(this) = weak.upgrade() else { return };
            if this.page.closed() {
                return;
            }
            this.restart.set_sensitive(true);
            if okay {
                status_text(&this.restart_status, &tr!("The desktop shell was restarted."), None);
            } else {
                let detail = if compat::strip(&output).is_empty() { tr!("Unknown error") } else { compat::strip(&output).to_owned() };
                status_text(&this.restart_status, &tr!("The desktop shell could not be restarted: {value1}", value1 = detail), Some("error"));
            }
        }));
    }
}

pub fn build_page_with(collector: Option<Collector>, runner: Option<Runner>) -> SettingsPage {
    let controller = AboutPage::new(collector, runner);
    let weak = Rc::downgrade(&controller);
    let page = SettingsPage::new(controller.page.widget.clone()).with_close(move || if let Some(c) = weak.upgrade() { c.page.close() });
    page.add_target("about-info", &controller.card);
    page.add_target("about-copy", &controller.copy);
    page.add_target("about-restart", &controller.restart);
    page.set_controller(controller);
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    #[test]
    fn parsers() {
        let lspci = "03:00.0 \"VGA compatible controller\" \"Advanced Micro Devices, Inc. [AMD/ATI]\" \"Navi 48\" -rc0 \"Sapphire\" \"Pulse\"\n00:1f.3 \"Audio device\" \"Intel Corporation\" \"HDA\"\n01:00.0 \"3D controller\" \"NVIDIA Corporation\" \"AD107M [GeForce RTX 4060 Max-Q]\" \"\" \"\"\n";
        assert_eq!(gpus_from_lspci(lspci), ["AMD/ATI Navi 48", "NVIDIA AD107M [GeForce RTX 4060 Max-Q]"]);
        assert_eq!(cpu_name(Some("processor: 0\nmodel name\t: AMD  Ryzen 7\n")), "AMD Ryzen 7");
        assert_eq!(memory_bytes(Some("MemTotal:       1024 kB\n")), 1024 * 1024);
        let folder = scratch("about");
        let path = folder.join("os-release");
        std::fs::write(&path, "NAME=\"Nobara Linux\"\nPRETTY_NAME=\"Nobara Linux 44 (KDE)\"\n# x\n").unwrap();
        assert_eq!(get(&os_release(&[path]), "PRETTY_NAME"), Some("Nobara Linux 44 (KDE)"));
        assert_eq!(labwc_version_from(Some("labwc 0.20.2 (+xwayland)\n".into())), "0.20.2");
        assert_eq!(short_vendor("Intel Corporation"), "Intel");
        let version = folder.join("VERSION");
        std::fs::write(&version, "  1.2.3\nnext\n").unwrap();
        assert_eq!(suite_version(&[folder.join("missing"), version]), "1.2.3");
    }

    #[test]
    fn restart_only_targets_shell_services_without_blocking() {
        assert_eq!(&RESTART_COMMAND[..4], ["systemctl", "--user", "--no-block", "try-restart"]);
        let mut services: Vec<&str> = RESTART_COMMAND[4..].to_vec();
        services.sort();
        assert_eq!(services, ["tessel-notifications.service", "tessel-dock.service", "tessel-shell.service"]);
        assert_eq!(&RESTART_COMMAND[4..], SHELL_SERVICES);
    }

    #[test]
    fn format_values_matches_python_layout() {
        let values = AboutValues { os: "Test OS 1".into(), memory: 0, labwc: "0.20.2".into(), session: "wayland".into(), gpu: vec!["A".into(), "B".into()], ..Default::default() };
        let rows = format_values(&values);
        assert_eq!(rows[0], ("Operating system", "Test OS 1".into()));
        assert_eq!(rows[4].1, "Unknown");
        assert_eq!(rows[5].1, "A\nB");
        assert_eq!(rows[7].1, "Development build");
        assert_eq!(rows[8].1, "labwc 0.20.2");
        assert_eq!(rows[9].1, "Wayland");
    }
}
