//! System Information (KDE's "About this System", GNOME's About panel): the
//! Tessel version, the software it runs on and the hardware, read from
//! os-release, /proc and /sys. Nothing is changed; the device name is shown
//! read-only because renaming it needs privileges.
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gs_base::widgets::{label, label_with, status_text, Page};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tessel_core::tr;

use crate::Factories;

/// The Tessel version shown in the header and the Desktop row.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// DMI values firmware vendors leave in place of a real name.
const PLACEHOLDERS: &[&str] = &["to be filled by o.e.m.", "system product name", "system manufacturer", "system version",
                                "default string", "not applicable", "not specified", "none", "o.e.m.", "oem",
                                "type1productconfigid", "0123456789", "x.x"];

/// Block devices that are not disks of their own (loop files, RAM, swap in
/// RAM, device-mapper and md volumes stacked on the real disks).
const VIRTUAL_DISKS: &[&str] = &["loop", "ram", "zram", "dm-", "md"];

/// Starts System Monitor (default: a gio subprocess, as the Displays page
/// starts HDR calibration).
pub type Launch = Rc<dyn Fn(&[String]) -> Result<(), String>>;

fn default_launch(command: &[String]) -> Result<(), String> {
    let args: Vec<&std::ffi::OsStr> = command.iter().map(std::ffi::OsStr::new).collect();
    gio::Subprocess::newv(&args, gio::SubprocessFlags::NONE).map(|_| ()).map_err(|e| e.message().to_owned())
}

pub const MONITOR_COMMAND: &[&str] = &["tessel-system-monitor"];

/// Everything `gather` reads, before translation and formatting.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    /// os-release `LOGO=`, an icon name.
    pub os_logo: String,
    pub kernel: String,
    pub machine: String,
    pub vendor: String,
    pub model: String,
    pub cpu: String,
    pub cores: usize,
    /// Bytes.
    pub memory: u64,
    pub gpus: Vec<String>,
    /// Bytes, every non-removable disk together.
    pub disk: u64,
    pub firmware: String,
    /// `labwc --version` (empty if it did not answer).
    pub labwc: String,
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).map(|text| text.trim().to_owned()).unwrap_or_default()
}

fn join_words(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" ")
}

/// `KEY=value` lines; quotes and shell escapes removed.
pub fn os_release(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        if key.is_empty() || key.starts_with('#') {
            return None;
        }
        let value = value.trim();
        let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(value);
        Some((key.to_owned(), value.replace("\\\"", "\"").replace("\\$", "$").replace("\\`", "`").replace("\\\\", "\\")))
    }).collect()
}

fn dmi(root: &Path, name: &str) -> String {
    let value = read(&root.join("sys/class/dmi/id").join(name));
    if PLACEHOLDERS.contains(&value.to_lowercase().as_str()) { String::new() } else { value }
}

/// `(model name, logical processors)` from /proc/cpuinfo.
pub fn cpu(text: &str) -> (String, usize) {
    let mut name = String::new();
    let mut count = 0;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if key == "processor" {
            count += 1;
        } else if name.is_empty() && ["model name", "Hardware", "Processor", "cpu model"].contains(&key) && !value.is_empty() {
            name = value.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    (name, count)
}

/// `MemTotal` in bytes.
pub fn memory(text: &str) -> u64 {
    text.lines().find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok()).map_or(0, |kib| kib * 1024)
}

/// Every non-removable disk in `/sys/block`, in bytes.
pub fn disk_capacity(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root.join("sys/block")) else { return 0 };
    entries.flatten().filter(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        !VIRTUAL_DISKS.iter().any(|prefix| name.starts_with(prefix)) && read(&entry.path().join("removable")) != "1"
    }).filter_map(|entry| read(&entry.path().join("size")).parse::<u64>().ok()).map(|sectors| sectors * 512).sum()
}

/// One GPU: `(vendor id, device id)` from sysfs, or the driver of a non-PCI one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gpu {
    Pci(u16, u16),
    Driver(String),
}

fn hex_id(text: &str) -> Option<u16> {
    u16::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// The GPUs behind `/sys/class/drm/card*/device`, each device once.
pub fn gpu_ids(root: &Path) -> Vec<Gpu> {
    let drm = root.join("sys/class/drm");
    let mut cards: Vec<(u32, PathBuf)> = std::fs::read_dir(&drm).map(|entries| entries.flatten().filter_map(|entry| {
        // card0, card1…; card0-DP-1 and the like are its connectors.
        let number = entry.file_name().to_string_lossy().strip_prefix("card")?.parse::<u32>().ok()?;
        Some((number, entry.path().join("device")))
    }).collect()).unwrap_or_default();
    cards.sort();
    let mut seen = Vec::new();
    let mut gpus = Vec::new();
    for (_, device) in cards {
        let Ok(real) = device.canonicalize() else { continue };
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        let gpu = match (hex_id(&read(&device.join("vendor"))), hex_id(&read(&device.join("device")))) {
            (Some(vendor), Some(id)) => Gpu::Pci(vendor, id),
            _ => {
                let driver = read(&device.join("uevent")).lines().find_map(|l| l.strip_prefix("DRIVER=").map(str::to_owned));
                // simpledrm is the firmware framebuffer, not a GPU.
                match driver.filter(|d| !d.is_empty() && d != "simpledrm") {
                    Some(driver) => Gpu::Driver(driver),
                    None => continue,
                }
            }
        };
        gpus.push(gpu);
    }
    gpus
}

/// `(vendor, device)` names from a pci.ids database.
pub fn pci_names(text: &str, vendor: u16, device: u16) -> (Option<String>, Option<String>) {
    let (vendor_key, device_key) = (format!("{vendor:04x}"), format!("{device:04x}"));
    let mut vendor_name = None;
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('\t') {
            if vendor_name.is_some() && !rest.starts_with('\t') {
                if let Some(name) = rest.strip_prefix(device_key.as_str()).and_then(|n| n.strip_prefix("  ")) {
                    return (vendor_name, Some(name.to_owned()));
                }
            }
        } else if vendor_name.is_some() || line.starts_with("C ") {
            break;
        } else if let Some(name) = line.strip_prefix(vendor_key.as_str()).and_then(|n| n.strip_prefix("  ")) {
            vendor_name = Some(name.to_owned());
        }
    }
    (vendor_name, None)
}

fn bracketed(text: &str) -> Option<&str> {
    let start = text.rfind('[')?;
    text[start + 1..].strip_suffix(']').filter(|inner| !inner.is_empty())
}

/// "Advanced Micro Devices, Inc. [AMD/ATI]" → "AMD", "NVIDIA Corporation" → "NVIDIA".
pub fn short_vendor(vendor: &str) -> String {
    if let Some(inner) = bracketed(vendor) {
        return inner.split('/').next().unwrap_or(inner).to_owned();
    }
    let mut name = vendor.trim();
    for suffix in [" Corporation", " Corp.", " Corp", ", Inc.", " Inc.", " Inc", " Co., Ltd.", " Ltd.", " Limited"] {
        name = name.strip_suffix(suffix).unwrap_or(name);
    }
    name.trim_end_matches(',').to_owned()
}

/// "AMD" + "Navi 48 [Radeon RX 9070/9070 XT/9070 GRE]" → "AMD Radeon RX 9070/9070 XT/9070 GRE".
pub fn gpu_name(vendor: Option<&str>, device: Option<&str>, ids: (u16, u16)) -> String {
    let Some(device) = device else {
        let prefix = vendor.map(short_vendor).unwrap_or_default();
        return join_words(&[&prefix, &format!("{:04x}:{:04x}", ids.0, ids.1)]);
    };
    let model = bracketed(device).unwrap_or(device);
    let vendor = vendor.map(short_vendor).unwrap_or_default();
    if model.to_lowercase().starts_with(&vendor.to_lowercase()) { model.to_owned() } else { join_words(&[&vendor, model]) }
}

/// The installed pci.ids (hwdata, else the older misc location).
pub fn pci_ids(root: &Path) -> String {
    ["usr/share/hwdata/pci.ids", "usr/share/misc/pci.ids"].iter()
        .find_map(|path| std::fs::read_to_string(root.join(path)).ok()).unwrap_or_default()
}

impl SystemInfo {
    /// Read everything below `root` (`/` for this computer). Blocking: the
    /// page calls it on a worker thread; pci.ids alone is a few megabytes.
    pub fn gather(root: &Path) -> SystemInfo {
        let release = ["etc/os-release", "usr/lib/os-release"].iter().map(|p| read(&root.join(p)))
            .find(|text| !text.is_empty()).map(|text| os_release(&text)).unwrap_or_default();
        let value = |key: &str| release.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
        let os = match value("PRETTY_NAME") {
            pretty if !pretty.is_empty() => pretty,
            _ => join_words(&[&value("NAME"), &value("VERSION")]),
        };
        let hostname = match read(&root.join("proc/sys/kernel/hostname")) {
            name if !name.is_empty() => name,
            _ => read(&root.join("etc/hostname")),
        };
        let machine = match read(&root.join("proc/sys/kernel/arch")) {
            arch if !arch.is_empty() => arch,
            _ if root == Path::new("/") => std::env::consts::ARCH.to_owned(),
            _ => String::new(),
        };
        // Desktop boards often leave the product empty; the board names it then.
        let (mut vendor, mut model) = (dmi(root, "sys_vendor"), join_words(&[&dmi(root, "product_name"), &dmi(root, "product_version")]));
        if model.is_empty() {
            // (board_version is mostly "Rev 1.xx" or similar filler.)
            model = dmi(root, "board_name");
            if vendor.is_empty() {
                vendor = dmi(root, "board_vendor");
            }
        }
        let (cpu, cores) = cpu(&read(&root.join("proc/cpuinfo")));
        let gpus = gpu_ids(root);
        let database = if gpus.iter().any(|g| matches!(g, Gpu::Pci(..))) { pci_ids(root) } else { String::new() };
        let gpus = gpus.into_iter().map(|gpu| match gpu {
            Gpu::Pci(vendor, device) => {
                let (vendor_name, device_name) = pci_names(&database, vendor, device);
                gpu_name(vendor_name.as_deref(), device_name.as_deref(), (vendor, device))
            }
            Gpu::Driver(driver) => driver,
        }).collect();
        SystemInfo {
            hostname, os, os_logo: value("LOGO"), kernel: read(&root.join("proc/sys/kernel/osrelease")), machine,
            vendor, model, cpu, cores, memory: memory(&read(&root.join("proc/meminfo"))), gpus,
            disk: disk_capacity(root), firmware: join_words(&[&dmi(root, "bios_vendor"), &dmi(root, "bios_version")]),
            labwc: String::new(),
        }
    }
}

/// "labwc 0.20.2 (+xwayland …)" → "0.20.2".
pub fn labwc_version_from(output: &str) -> String {
    let mut words = output.split_whitespace();
    while let Some(word) = words.next() {
        if word == "labwc" {
            return words.next().unwrap_or_default().to_owned();
        }
    }
    String::new()
}

/// `labwc --version` (blocking; worker thread only).
pub fn labwc_version() -> String {
    std::process::Command::new("labwc").arg("--version").stdin(std::process::Stdio::null()).output().ok()
        .map(|out| labwc_version_from(&String::from_utf8_lossy(&out.stdout))).unwrap_or_default()
}

pub fn gtk_version() -> String {
    format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version())
}

fn word_size(machine: &str) -> Option<String> {
    match machine {
        "x86_64" | "aarch64" | "arm64" | "ppc64" | "ppc64le" | "riscv64" | "s390x" | "loongarch64" | "mips64"
        | "sparc64" => Some(tr!("64-bit")),
        "i386" | "i486" | "i586" | "i686" | "x86" | "arm" | "armv6l" | "armv7l" | "armv8l" | "riscv32" | "mips" => Some(tr!("32-bit")),
        _ => None,
    }
}

/// The two cards of the page (the first field of each [`rows`] entry).
pub const SOFTWARE: &str = "Software";
pub const HARDWARE: &str = "Hardware";

/// `[(group, English title, value)]`, in page order. Rows the computer does
/// not report (a VM without DMI names) are left out; the rest say Unknown.
pub fn rows(info: &SystemInfo, gtk: &str) -> Vec<(&'static str, &'static str, String)> {
    let unknown = |text: String| if text.is_empty() { tr!("Unknown") } else { text };
    let kernel = match word_size(&info.machine) {
        Some(bits) if !info.kernel.is_empty() => format!("{} ({bits})", info.kernel),
        _ if !info.machine.is_empty() && !info.kernel.is_empty() => format!("{} ({})", info.kernel, info.machine),
        _ => info.kernel.clone(),
    };
    let labwc = if info.labwc.is_empty() { "labwc".to_owned() } else { format!("labwc {}", info.labwc) };
    let cpu = match (info.cpu.as_str(), info.cores) {
        ("", _) => String::new(),
        (name, 0) => name.to_owned(),
        (name, cores) => format!("{name} × {cores}"),
    };
    let memory = if info.memory > 0 { glib::format_size_full(info.memory, glib::FormatSizeFlags::IEC_UNITS).to_string() } else { String::new() };
    let disk = if info.disk > 0 { glib::format_size(info.disk).to_string() } else { String::new() };
    let mut rows = vec![
        (SOFTWARE, "Operating system", unknown(info.os.clone())),
        (SOFTWARE, "Kernel version", unknown(kernel)),
        (SOFTWARE, "Desktop environment", format!("Tessel {VERSION}")),
        (SOFTWARE, "Windowing system", format!("Wayland ({labwc})")),
        (SOFTWARE, "GTK version", gtk.to_owned()),
    ];
    if !info.vendor.is_empty() {
        rows.push((HARDWARE, "Manufacturer", info.vendor.clone()));
    }
    if !info.model.is_empty() {
        rows.push((HARDWARE, "Model", info.model.clone()));
    }
    rows.push((HARDWARE, "Processor", unknown(cpu)));
    rows.push((HARDWARE, "Memory", unknown(memory)));
    rows.push((HARDWARE, "Graphics", unknown(info.gpus.join("\n"))));
    rows.push((HARDWARE, "Disk capacity", unknown(disk)));
    if !info.firmware.is_empty() {
        rows.push((HARDWARE, "Firmware", info.firmware.clone()));
    }
    rows
}

/// The plain text "Copy to Clipboard" puts on the clipboard (KDE style:
/// one translated "Title: value" line each).
pub fn plain_text(info: &SystemInfo, gtk: &str) -> String {
    let mut lines = vec![format!("{}: {}", tr!("Device name"), if info.hostname.is_empty() { tr!("Unknown") } else { info.hostname.clone() })];
    lines.extend(rows(info, gtk).into_iter().map(|(_, title, value)| format!("{}: {}", tr!(title), value.replace('\n', "; "))));
    lines.join("\n") + "\n"
}

/// The Tessel logo: the data file when installed (or in the source tree),
/// else the `tessel` icon, else a computer.
pub fn logo_files() -> Vec<PathBuf> {
    vec![tessel_core::paths::data_home().join("tessel-settings/tessel-logo.svg"),
         PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/tessel-logo.svg"))]
}

fn has_icon(name: &str) -> bool {
    !name.is_empty() && gdk::Display::default().is_some_and(|display| gtk::IconTheme::for_display(&display).has_icon(name))
}

fn logo() -> gtk::Image {
    let image = match logo_files().into_iter().find(|path| path.is_file()) {
        Some(path) => gtk::Image::from_file(path),
        None => gtk::Image::from_icon_name(if has_icon("tessel") { "tessel" } else { "computer-symbolic" }),
    };
    image.set_pixel_size(96);
    image
}

/// `page.controller` of the System Information page.
pub struct SystemInfoPage {
    pub page: Page,
    pub info: std::cell::RefCell<Option<SystemInfo>>,
    pub logo: gtk::Image,
    pub device_name: gtk::Label,
    pub device_card: gtk::Box,
    pub software: gtk::Box,
    pub hardware: gtk::Box,
    pub copy: gtk::Button,
    pub monitor: gtk::Button,
    pub status: gtk::Label,
    launch: Launch,
}

fn value_label(text: &str) -> gtk::Label {
    let value = label_with(text, None, true);
    value.set_xalign(1.0);
    value.set_justify(gtk::Justification::Right);
    value.set_selectable(true);
    // Shares the row with its title instead of shrinking to the longest word.
    value.set_hexpand(true);
    value
}

impl SystemInfoPage {
    pub fn new(root: PathBuf, launch: Option<Launch>) -> Rc<SystemInfoPage> {
        let page = Page::new("system-info-page", &tr!("System Information"), &tr!("Information about this computer and its software."),
                             Some("computer-symbolic"));
        let hero = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).halign(gtk::Align::Center)
            .margin_top(6).margin_bottom(6).build();
        let logo = logo();
        hero.append(&logo);
        let name = gtk::Label::new(Some("Tessel"));
        name.add_css_class("title-1");
        hero.append(&name);
        hero.append(&gtk::Label::builder().label(tr!("Version {value1}", value1 = VERSION)).css_classes(["detail"]).build());
        page.content.append(&hero);
        let device_card = page.group(None, None, None);
        let device_name = value_label(&tr!("Loading…"));
        Page::row(&device_card, &tr!("Device name"), Some(device_name.upcast_ref()), None);
        let software = page.group(Some(&tr!(SOFTWARE)), None, None);
        let hardware = page.group(Some(&tr!(HARDWARE)), None, None);
        for card in [&software, &hardware] {
            card.append(&label(&tr!("Loading…"), Some("detail")));
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let copy = gtk::Button::builder().label(tr!("Copy to Clipboard")).sensitive(false).build();
        let monitor = gtk::Button::builder().label(tr!("Open System Monitor")).build();
        for button in [&copy, &monitor] {
            button.add_css_class("pill");
            actions.append(button);
        }
        page.content.append(&actions);
        let status = label("", Some("detail"));
        page.content.append(&status);
        let this = Rc::new(SystemInfoPage { page, info: Default::default(), logo, device_name, device_card, software, hardware,
                                            copy, monitor, status, launch: launch.unwrap_or_else(|| Rc::new(default_launch)) });
        let weak = Rc::downgrade(&this);
        this.copy.connect_clicked(move |_| if let Some(this) = weak.upgrade() { this.copy_text() });
        let weak = Rc::downgrade(&this);
        this.monitor.connect_clicked(move |_| if let Some(this) = weak.upgrade() { this.open_monitor() });
        let weak = Rc::downgrade(&this);
        // labwc --version only describes the running compositor, not `root`.
        let live = root == Path::new("/");
        this.page.run(move || {
            let mut info = SystemInfo::gather(&root);
            if live {
                info.labwc = labwc_version();
            }
            Ok(info)
        }, move |result| if let Some(this) = weak.upgrade() { this.loaded(result) });
        this
    }

    fn loaded(&self, result: Result<SystemInfo, String>) {
        for card in [&self.software, &self.hardware] {
            tessel_gtk::utils::remove_children(card);
        }
        let info = match result {
            Ok(info) => info,
            Err(error) => {
                self.software.append(&label(&tr!("System information could not be read: {value1}", value1 = error), Some("error")));
                return;
            }
        };
        self.device_name.set_text(&if info.hostname.is_empty() { tr!("Unknown") } else { info.hostname.clone() });
        for (group, title, value) in rows(&info, &gtk_version()) {
            let card = if group == SOFTWARE { &self.software } else { &self.hardware };
            let value_widget: gtk::Widget = if title == "Operating system" && has_icon(&info.os_logo) {
                let line = gtk::Box::builder().spacing(8).hexpand(true).halign(gtk::Align::End).build();
                line.append(&gtk::Image::builder().icon_name(info.os_logo.as_str()).pixel_size(24).build());
                line.append(&value_label(&value));
                line.upcast()
            } else {
                value_label(&value).upcast()
            };
            let row = Page::row(card, &tr!(title), Some(&value_widget), None);
            row.set_widget_name(title);
        }
        self.info.replace(Some(info));
        self.copy.set_sensitive(true);
    }

    pub fn text(&self) -> String {
        self.info.borrow().as_ref().map(|info| plain_text(info, &gtk_version())).unwrap_or_default()
    }

    pub fn copy_text(&self) {
        self.page.widget.clipboard().set_text(&self.text());
        status_text(&self.status, &tr!("Copied to the clipboard."), None);
    }

    pub fn open_monitor(&self) {
        let command: Vec<String> = MONITOR_COMMAND.iter().map(|s| s.to_string()).collect();
        match (self.launch)(&command) {
            Ok(()) => status_text(&self.status, "", None),
            Err(error) => status_text(&self.status, &tr!("Could not open System Monitor: {value1}", value1 = error), Some("error")),
        }
    }
}

pub(crate) fn build(_host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let root = factories.system_root.clone().unwrap_or_else(|| PathBuf::from("/"));
    let controller = SystemInfoPage::new(root, factories.system_launch.clone());
    let weak = Rc::downgrade(&controller);
    let page = SettingsPage::new(controller.page.widget.clone()).with_close(move || if let Some(c) = weak.upgrade() { c.page.close() });
    page.add_target("device_name", &controller.device_card);
    page.add_target("software", &controller.software);
    page.add_target("hardware", &controller.hardware);
    page.add_target("copy", &controller.copy);
    page.add_target("monitor", &controller.monitor);
    page.set_controller(controller);
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-applets-unit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const PCI_IDS: &str = "# pci.ids\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t13c0  Granite Ridge [Radeon Graphics]\n\
        \t7550  Navi 48 [Radeon RX 9070/9070 XT/9070 GRE]\n\t\t1002 0b36  Reference\n10de  NVIDIA Corporation\n\
        \t2882  AD107 [GeForce RTX 4060]\n8086  Intel Corporation\n\t7d55  Meteor Lake-P [Intel Arc Graphics]\nC 03  Display controller\n";

    /// A desktop: two AMD GPUs (one twice, through a connector-less alias),
    /// three NVMe disks among loop/zram/dm volumes and a USB stick, and an
    /// ASUS board whose product names are placeholders.
    fn fake_root(name: &str) -> PathBuf {
        let root = scratch(name);
        put(&root, "etc/os-release", "NAME=\"Nobara Linux\"\nVERSION=\"44 (KDE)\"\nPRETTY_NAME=\"Nobara Linux 44 (KDE Plasma Desktop Edition)\"\nLOGO=nobara-logo-icon\n# c\n");
        put(&root, "proc/sys/kernel/hostname", "lalatina\n");
        put(&root, "proc/sys/kernel/osrelease", "7.3.0-203.nobara.fc44.x86_64\n");
        put(&root, "proc/sys/kernel/arch", "x86_64\n");
        put(&root, "proc/cpuinfo", "processor\t: 0\nmodel name\t: AMD  Ryzen 9 9950X3D 16-Core Processor\n\nprocessor\t: 1\nmodel name\t: AMD Ryzen 9 9950X3D 16-Core Processor\n");
        put(&root, "proc/meminfo", "MemTotal:       65536000 kB\nMemFree:  1 kB\n");
        for (file, value) in [("sys_vendor", "ASUS"), ("product_name", "System Product Name"), ("product_version", "System Version"),
                              ("board_vendor", "ASUSTeK COMPUTER INC."), ("board_name", "ROG STRIX X870-A GAMING WIFI"),
                              ("board_version", "Rev 1.xx"), ("bios_vendor", "American Megatrends Inc."), ("bios_version", "2503")] {
            put(&root, &format!("sys/class/dmi/id/{file}"), &format!("{value}\n"));
        }
        put(&root, "sys/devices/pci/0000:03:00.0/vendor", "0x1002\n");
        put(&root, "sys/devices/pci/0000:03:00.0/device", "0x7550\n");
        put(&root, "sys/devices/pci/0000:75:00.0/vendor", "0x1002\n");
        put(&root, "sys/devices/pci/0000:75:00.0/device", "0x13c0\n");
        put(&root, "sys/devices/platform/simple-framebuffer.0/uevent", "DRIVER=simpledrm\n");
        for (card, device) in [("card1", "pci/0000:03:00.0"), ("card0", "pci/0000:75:00.0"), ("card2", "pci/0000:03:00.0"),
                               ("card3", "platform/simple-framebuffer.0"), ("card0-DP-1", "pci/0000:75:00.0")] {
            std::fs::create_dir_all(root.join("sys/class/drm").join(card)).unwrap();
            std::os::unix::fs::symlink(root.join("sys/devices").join(device), root.join("sys/class/drm").join(card).join("device")).unwrap();
        }
        put(&root, "usr/share/hwdata/pci.ids", PCI_IDS);
        for (disk, removable, sectors) in [("nvme0n1", "0", "1000"), ("nvme1n1", "0", "1000"), ("sda", "0", "48"), ("sdb", "1", "9999"),
                                           ("loop0", "0", "777"), ("zram0", "0", "777"), ("dm-0", "0", "777"), ("ram0", "0", "777")] {
            put(&root, &format!("sys/block/{disk}/removable"), removable);
            put(&root, &format!("sys/block/{disk}/size"), sectors);
        }
        root
    }

    #[test]
    fn gather_reads_every_source_below_the_root() {
        let root = fake_root("gather");
        let info = SystemInfo::gather(&root);
        assert_eq!(info, SystemInfo {
            hostname: "lalatina".into(), os: "Nobara Linux 44 (KDE Plasma Desktop Edition)".into(), os_logo: "nobara-logo-icon".into(),
            kernel: "7.3.0-203.nobara.fc44.x86_64".into(), machine: "x86_64".into(), vendor: "ASUS".into(),
            model: "ROG STRIX X870-A GAMING WIFI".into(), cpu: "AMD Ryzen 9 9950X3D 16-Core Processor".into(), cores: 2,
            memory: 65536000 * 1024, gpus: vec!["AMD Radeon Graphics".into(), "AMD Radeon RX 9070/9070 XT/9070 GRE".into()],
            disk: (1000 + 1000 + 48) * 512, firmware: "American Megatrends Inc. 2503".into(), labwc: String::new(),
        });
    }

    #[test]
    fn missing_sources_leave_values_empty_and_rows_unknown() {
        let root = scratch("empty");
        put(&root, "usr/lib/os-release", "NAME=Fedora\nVERSION=44\n");
        put(&root, "etc/hostname", "box\n");
        let info = SystemInfo::gather(&root);
        assert_eq!(info, SystemInfo { os: "Fedora 44".into(), hostname: "box".into(), ..Default::default() });
        let rows = rows(&info, "4.20.1");
        let titles: Vec<&str> = rows.iter().map(|r| r.1).collect();
        // No DMI names, no firmware: those rows are left out rather than Unknown.
        assert_eq!(titles, ["Operating system", "Kernel version", "Desktop environment", "Windowing system", "GTK version",
                            "Processor", "Memory", "Graphics", "Disk capacity"]);
        assert_eq!(rows[1].2, "Unknown");
        assert_eq!(rows[3].2, "Wayland (labwc)");
        assert!(rows[5..].iter().all(|r| r.2 == "Unknown"));
    }

    #[test]
    fn pci_ids_lookup_and_names() {
        assert_eq!(pci_names(PCI_IDS, 0x1002, 0x7550),
                   (Some("Advanced Micro Devices, Inc. [AMD/ATI]".into()), Some("Navi 48 [Radeon RX 9070/9070 XT/9070 GRE]".into())));
        // A device id of another vendor, or the subsystem line, never matches.
        assert_eq!(pci_names(PCI_IDS, 0x10de, 0x7550), (Some("NVIDIA Corporation".into()), None));
        assert_eq!(pci_names(PCI_IDS, 0x1002, 0x0b36).1, None);
        assert_eq!(pci_names(PCI_IDS, 0xffff, 0x0001), (None, None));
        assert_eq!(gpu_name(Some("NVIDIA Corporation"), Some("AD107 [GeForce RTX 4060]"), (0x10de, 0x2882)), "NVIDIA GeForce RTX 4060");
        assert_eq!(gpu_name(Some("Intel Corporation"), Some("Meteor Lake-P [Intel Arc Graphics]"), (0x8086, 0x7d55)), "Intel Arc Graphics");
        assert_eq!(gpu_name(Some("Intel Corporation"), None, (0x8086, 0x1234)), "Intel 8086:1234");
        assert_eq!(gpu_name(None, None, (0x1af4, 0x1050)), "1af4:1050");
        assert_eq!(short_vendor("Silicon Motion, Inc."), "Silicon Motion");
        // Without a database the ids are shown.
        let root = fake_root("no-pci-ids");
        std::fs::remove_file(root.join("usr/share/hwdata/pci.ids")).unwrap();
        assert_eq!(SystemInfo::gather(&root).gpus, ["1002:13c0", "1002:7550"]);
        put(&root, "usr/share/misc/pci.ids", PCI_IDS);
        assert_eq!(SystemInfo::gather(&root).gpus[1], "AMD Radeon RX 9070/9070 XT/9070 GRE");
    }

    #[test]
    fn parsers() {
        assert_eq!(os_release("A=\"x \\\"y\\\"\"\nB='z'\n#C=1\nD=plain\n"),
                   [("A".to_owned(), "x \"y\"".to_owned()), ("B".into(), "z".into()), ("D".into(), "plain".into())]);
        assert_eq!(cpu("Processor\t: ARMv8\nprocessor : 0\nprocessor : 1\nprocessor : 2\n"), ("ARMv8".into(), 3));
        assert_eq!(memory("MemFree: 1 kB\nMemTotal: 2048 kB\n"), 2048 * 1024);
        assert_eq!(memory(""), 0);
        assert_eq!(labwc_version_from("labwc 0.20.2 (+xwayland +nls +rsvg +libsfdo) wlroots-0.20.2\n"), "0.20.2");
        assert_eq!(labwc_version_from(""), "");
    }

    #[test]
    fn rows_and_plain_text() {
        let mut info = SystemInfo::gather(&fake_root("rows"));
        info.labwc = "0.20.2".into();
        let rows = rows(&info, "4.20.1");
        let find = |title: &str| rows.iter().find(|r| r.1 == title).map(|r| r.2.clone()).unwrap();
        assert_eq!(find("Kernel version"), "7.3.0-203.nobara.fc44.x86_64 (64-bit)");
        assert_eq!(find("Desktop environment"), format!("Tessel {VERSION}"));
        assert_eq!(find("Windowing system"), "Wayland (labwc 0.20.2)");
        assert_eq!(find("GTK version"), "4.20.1");
        assert_eq!(find("Processor"), "AMD Ryzen 9 9950X3D 16-Core Processor × 2");
        assert_eq!(find("Memory"), glib::format_size_full(65536000 * 1024, glib::FormatSizeFlags::IEC_UNITS).to_string());
        assert!(find("Memory").ends_with("GiB"));
        assert_eq!(find("Graphics"), "AMD Radeon Graphics\nAMD Radeon RX 9070/9070 XT/9070 GRE");
        assert_eq!(find("Firmware"), "American Megatrends Inc. 2503");
        assert_eq!(rows.iter().filter(|r| r.0 == SOFTWARE).count(), 5);
        let text = plain_text(&info, "4.20.1");
        assert!(text.starts_with("Device name: lalatina\nOperating system: Nobara Linux 44 (KDE Plasma Desktop Edition)\n"), "{text}");
        assert!(text.contains("\nGraphics: AMD Radeon Graphics; AMD Radeon RX 9070/9070 XT/9070 GRE\n"), "{text}");
        assert!(text.ends_with("Firmware: American Megatrends Inc. 2503\n"));
        assert_eq!(text.lines().count(), rows.len() + 1);
        info.machine = "riscv128".into();
        assert_eq!(super::rows(&info, "4").iter().find(|r| r.1 == "Kernel version").unwrap().2, "7.3.0-203.nobara.fc44.x86_64 (riscv128)");
    }

    #[test]
    fn placeholders_are_skipped() {
        let root = scratch("oem");
        for (file, value) in [("sys_vendor", "To Be Filled By O.E.M."), ("product_name", "Default string"), ("board_vendor", "Framework"),
                              ("board_name", "FRANMECP01"), ("bios_vendor", "INSYDE Corp."), ("bios_version", "03.05")] {
            put(&root, &format!("sys/class/dmi/id/{file}"), value);
        }
        let info = SystemInfo::gather(&root);
        assert_eq!((info.vendor.as_str(), info.model.as_str(), info.firmware.as_str()), ("Framework", "FRANMECP01", "INSYDE Corp. 03.05"));
        put(&root, "sys/class/dmi/id/sys_vendor", "LENOVO");
        put(&root, "sys/class/dmi/id/product_name", "21KC");
        put(&root, "sys/class/dmi/id/product_version", "ThinkPad X1 Carbon Gen 12");
        let info = SystemInfo::gather(&root);
        assert_eq!((info.vendor.as_str(), info.model.as_str()), ("LENOVO", "21KC ThinkPad X1 Carbon Gen 12"));
    }
}
