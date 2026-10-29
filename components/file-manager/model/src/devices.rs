//! `files_devices.py` data side: local disk discovery and capacity values for
//! Places (the `DiskUsage` widget belongs to files-widgets).

use std::collections::HashSet;

use gio::prelude::*;

/// The attributes `DiskUsage` queries (`query_filesystem_info_async`, LOW priority).
pub const CAPACITY_ATTRIBUTES: &str = "filesystem::size,filesystem::free,filesystem::used";

/// `Disk(path, name, device, icon)`.
#[derive(Clone, Debug)]
pub struct Disk {
    pub path: String,
    pub name: String,
    pub device: String,
    pub icon: gio::Icon,
}

impl PartialEq for Disk {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.name == other.name && self.device == other.device && self.icon.equal(Some(&other.icon))
    }
}

/// What `local_disks` reads from a `GUnixMountEntry`.
pub trait MountEntryInfo {
    fn mount_path(&self) -> String;
    fn device_path(&self) -> String;
    fn fs_type(&self) -> String;
    fn guess_name(&self) -> String;
    fn guess_symbolic_icon(&self) -> gio::Icon;
}

mod ffi {
    use std::ffi::c_char;
    #[repr(C)]
    pub struct GUnixMountEntry {
        _private: [u8; 0],
    }
    extern "C" {
        pub fn g_unix_mount_entries_get(time_read: *mut u64) -> *mut glib::ffi::GList;
        pub fn g_unix_mount_entry_free(entry: *mut GUnixMountEntry);
        pub fn g_unix_mount_entry_get_mount_path(entry: *mut GUnixMountEntry) -> *const c_char;
        pub fn g_unix_mount_entry_get_device_path(entry: *mut GUnixMountEntry) -> *const c_char;
        pub fn g_unix_mount_entry_get_fs_type(entry: *mut GUnixMountEntry) -> *const c_char;
        pub fn g_unix_mount_entry_guess_name(entry: *mut GUnixMountEntry) -> *mut c_char;
        pub fn g_unix_mount_entry_guess_symbolic_icon(entry: *mut GUnixMountEntry) -> *mut gio::ffi::GIcon;
    }
}

fn c_text(pointer: *const std::ffi::c_char) -> String {
    if pointer.is_null() {
        String::new()
    } else {
        // SAFETY: GIO returns a valid NUL-terminated string.
        unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned()
    }
}

/// `GioUnix.mount_entries_get()` (glib >= 2.84; read-only), as records.
pub fn mount_entries() -> Vec<MountRecord> {
    use glib::translate::from_glib_full;
    let mut records = Vec::new();
    // SAFETY: the list and its entries are owned by us and freed here; the
    // guessed name/icon are transferred (full), the paths are borrowed.
    unsafe {
        let list = ffi::g_unix_mount_entries_get(std::ptr::null_mut());
        let mut node = list;
        while !node.is_null() {
            let entry = (*node).data as *mut ffi::GUnixMountEntry;
            let name_ptr = ffi::g_unix_mount_entry_guess_name(entry);
            let name = c_text(name_ptr);
            glib::ffi::g_free(name_ptr as glib::ffi::gpointer);
            let icon: gio::Icon = from_glib_full(ffi::g_unix_mount_entry_guess_symbolic_icon(entry));
            records.push(MountRecord {
                mount_path: c_text(ffi::g_unix_mount_entry_get_mount_path(entry)),
                device_path: c_text(ffi::g_unix_mount_entry_get_device_path(entry)),
                fs_type: c_text(ffi::g_unix_mount_entry_get_fs_type(entry)),
                name,
                icon,
            });
            ffi::g_unix_mount_entry_free(entry);
            node = (*node).next;
        }
        glib::ffi::g_list_free(list);
    }
    records
}

/// A plain mount entry (tests, or callers with their own mount table).
#[derive(Clone, Debug)]
pub struct MountRecord {
    pub mount_path: String,
    pub device_path: String,
    pub fs_type: String,
    pub name: String,
    pub icon: gio::Icon,
}

impl MountEntryInfo for MountRecord {
    fn mount_path(&self) -> String {
        self.mount_path.clone()
    }
    fn device_path(&self) -> String {
        self.device_path.clone()
    }
    fn fs_type(&self) -> String {
        self.fs_type.clone()
    }
    fn guess_name(&self) -> String {
        self.name.clone()
    }
    fn guess_symbolic_icon(&self) -> gio::Icon {
        self.icon.clone()
    }
}

/// `local_disks(entries)`: fixed data disks which GVfs omits, excluding OS
/// pseudo mounts, loop devices, system trees and duplicate devices (Btrfs
/// subvolumes and bind mounts share one capacity). `/` comes first, as `System`.
pub fn local_disks_from<E: MountEntryInfo>(entries: &[E]) -> Vec<Disk> {
    let mut sorted: Vec<(String, &E)> = entries.iter().map(|e| (e.mount_path(), e)).collect();
    sorted.sort_by(|a, b| (a.0 != "/", &a.0).cmp(&(b.0 != "/", &b.0)));
    let mut disks = Vec::new();
    let mut devices = HashSet::new();
    for (path, entry) in sorted {
        let device = entry.device_path();
        if !device.starts_with("/dev/") || device.starts_with("/dev/loop") {
            continue;
        }
        if matches!(entry.fs_type().as_str(), "squashfs" | "tmpfs" | "devtmpfs") {
            continue;
        }
        if ["/boot", "/proc", "/sys", "/dev", "/var/lib", "/usr"]
            .iter()
            .any(|root| path == *root || path.starts_with(&format!("{root}/")))
        {
            continue;
        }
        if path.starts_with("/run/") && !path.starts_with("/run/media/") {
            continue;
        }
        if !devices.insert(device.clone()) {
            continue;
        }
        let name = if path == "/" {
            "System".to_owned()
        } else {
            let guessed = entry.guess_name();
            if guessed.is_empty() {
                path.rsplit('/').next().unwrap_or("").to_owned()
            } else {
                guessed
            }
        };
        disks.push(Disk { name, device, icon: entry.guess_symbolic_icon(), path });
    }
    disks
}

/// `local_disks()` from the system mount table (read-only).
pub fn local_disks() -> Vec<Disk> {
    local_disks_from(&mount_entries())
}

/// `capacity(info) -> (size, free, used)`, or `None` when size/free are
/// missing or the size is 0. Values are clamped to the size; `used` defaults
/// to `size - free`.
pub fn capacity(info: &gio::FileInfo) -> Option<(u64, u64, u64)> {
    if !info.has_attribute("filesystem::size") || !info.has_attribute("filesystem::free") {
        return None;
    }
    let size = info.attribute_uint64("filesystem::size");
    if size == 0 {
        return None;
    }
    let free = size.min(info.attribute_uint64("filesystem::free"));
    let used = if info.has_attribute("filesystem::used") { size.min(info.attribute_uint64("filesystem::used")) } else { size - free };
    Some((size, free, used))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(path: &str, device: &str, fs: &str) -> MountRecord {
        MountRecord {
            mount_path: path.into(),
            device_path: device.into(),
            fs_type: fs.into(),
            name: path.rsplit('/').next().unwrap().into(),
            icon: gio::ThemedIcon::new("drive-harddisk-symbolic").upcast(),
        }
    }

    // test_files_devices.test_fixed_storage_and_removable_mounts_without_pseudo_mounts_or_duplicates
    #[test]
    fn fixed_storage_and_removable_mounts() {
        crate::test_env();
        let entries = [
            mount("/home", "/dev/mapper/root", "btrfs"),
            mount("/", "/dev/mapper/root", "btrfs"),
            mount("/mnt/Storage", "/dev/nvme1n1p1", "ext4"),
            mount("/media/USB", "/dev/sda1", "ext4"),
            mount("/boot/efi", "/dev/nvme0n1p1", "vfat"),
            mount("/var/lib/snapd/app", "/dev/loop0", "squashfs"),
            mount("/run", "tmpfs", "tmpfs"),
            mount("/mnt/server", "server:/data", "nfs"),
        ];
        let disks = local_disks_from(&entries);
        let paths: Vec<&str> = disks.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["/", "/media/USB", "/mnt/Storage"]);
        assert_eq!(disks[0].name, "System");
        assert_eq!(disks[1].name, "USB");
        let more = [mount("/run/media/u/Stick", "/dev/sdb1", "vfat"), mount("/run/user/1000/doc", "/dev/sdc1", "ext4")];
        let paths: Vec<String> = local_disks_from(&more).into_iter().map(|d| d.path).collect();
        assert_eq!(paths, ["/run/media/u/Stick"]);
        // The real mount table is only read.
        let _ = local_disks();
    }

    // test_files_devices.test_unknown_empty_full_and_reserved_capacity
    #[test]
    fn capacity_bounds() {
        crate::test_env();
        let info = gio::FileInfo::new();
        assert_eq!(capacity(&info), None);
        info.set_attribute_uint64("filesystem::size", 0);
        info.set_attribute_uint64("filesystem::free", 0);
        assert_eq!(capacity(&info), None);
        info.set_attribute_uint64("filesystem::size", 1000);
        assert_eq!(capacity(&info), Some((1000, 0, 1000)));
        info.set_attribute_uint64("filesystem::free", 100);
        info.set_attribute_uint64("filesystem::used", 850);
        assert_eq!(capacity(&info), Some((1000, 100, 850)));
        info.set_attribute_uint64("filesystem::free", 1200);
        info.set_attribute_uint64("filesystem::used", 1200);
        assert_eq!(capacity(&info), Some((1000, 1000, 1000)));
    }
}
