//! Copies to removable devices and network filesystems that finish when the
//! data is on the device, not when it is in memory.
//!
//! `write()` returns once the data is in the page cache; the kernel writes it
//! out later. On a USB stick or a slow share that is minutes later, so a plain
//! copy shows progress for filling memory, reports success while the device
//! is still writing, and a move deletes its sources before their copies
//! exist anywhere but in memory.
//!
//! [`slow_destination`] tells which folders need care, [`copy_file`] copies
//! one regular file there in writeback windows (so its progress follows the
//! device) and [`flush_filesystem`] writes out whatever is left at the end of
//! an operation. Fixed local disks need none of this and keep plain copies.

use std::fs::File;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Why a destination needs windowed copies and a flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slow {
    /// A device that can be unplugged: USB disks and sticks, memory cards.
    Removable,
    /// A network or FUSE filesystem.
    Network,
}

/// Set to `removable`, `network` or `none` to override detection (tests).
pub const POLICY_VARIABLE: &str = "TESSEL_COPY_POLICY";

const MIB: u64 = 1024 * 1024;
/// The first and smallest writeback window.
pub const WINDOW_MIN: u64 = MIB;
/// The largest writeback window; larger files are also fsynced at their end.
pub const WINDOW_MAX: u64 = 16 * MIB;
/// How much writeback time one window aims for.
const WINDOW_TIME: Duration = Duration::from_millis(200);
const CHUNK: usize = 256 * 1024;

/// Whether files copied into `directory` need [`copy_file`] and a final
/// [`flush_filesystem`]. Reads the mount table and sysfs only: nothing is
/// asked of the device or of a network server.
pub fn slow_destination(directory: &Path) -> Option<Slow> {
    match std::env::var(POLICY_VARIABLE).as_deref() {
        Ok("removable") => return Some(Slow::Removable),
        Ok("network") => return Some(Slow::Network),
        Ok("none") => return None,
        _ => {}
    }
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let directory = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    let mount = mount_of(&mounts, &directory)?;
    classify_mount(&mount, Path::new("/sys"), &|device| std::fs::metadata(device).ok().map(|m| device_numbers(m.rdev())))
}

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub point: PathBuf,
    pub filesystem: String,
    pub source: String,
}

/// The mount `path` (absolute, resolved) is on: the longest mount point that
/// contains it, the later line winning among equals (it covers the earlier).
pub fn mount_of(mountinfo: &str, path: &Path) -> Option<Mount> {
    let mut best: Option<Mount> = None;
    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else { continue };
        let Some(point) = left.split(' ').nth(4) else { continue };
        let mut right = right.split(' ');
        let (Some(filesystem), Some(source)) = (right.next(), right.next()) else { continue };
        let point = PathBuf::from(unescape(point));
        if !path.starts_with(&point) {
            continue;
        }
        if best.as_ref().is_none_or(|b| point.as_os_str().len() >= b.point.as_os_str().len()) {
            best = Some(Mount { point, filesystem: filesystem.to_owned(), source: unescape(source) });
        }
    }
    best
}

/// mountinfo writes space, tab, newline and backslash as `\ooo`.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
            let value = bytes[i + 1..i + 4].iter().fold(0u32, |v, b| v * 8 + (b - b'0') as u32);
            out.push(value as u8);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

const NETWORK: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "ceph", "glusterfs", "9p", "afs", "ncpfs", "lustre", "davfs", "fuse"];

/// The policy for a mount. `numbers` gives the (major, minor) of a device node.
pub fn classify_mount(mount: &Mount, sys: &Path, numbers: &dyn Fn(&Path) -> Option<(u32, u32)>) -> Option<Slow> {
    let filesystem = mount.filesystem.as_str();
    // fuseblk (ntfs-3g, exfat-fuse) sits on a block device like any other.
    if NETWORK.contains(&filesystem) || filesystem.starts_with("fuse.") {
        return Some(Slow::Network);
    }
    if !mount.source.starts_with("/dev/") {
        return None;
    }
    let (major, minor) = numbers(Path::new(&mount.source))?;
    let block = std::fs::canonicalize(sys.join(format!("dev/block/{major}:{minor}"))).ok()?;
    removable_block(&block, 0).then_some(Slow::Removable)
}

fn device_numbers(rdev: u64) -> (u32, u32) {
    (libc::major(rdev), libc::minor(rdev))
}

fn attribute(directory: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(directory.join(name)).ok().map(|text| text.trim().to_owned())
}

/// Whether the block device at the resolved sysfs path `block` can be
/// unplugged. Partitions answer for their disk, and encrypted, LVM and RAID
/// volumes for the disks under them. A disk counts when it reports removable
/// media, is a memory card, or hangs off a USB port or another port the
/// kernel marks as reachable from outside the machine. Most USB disks do not
/// report removable media, so the port is what usually decides.
fn removable_block(block: &Path, depth: u8) -> bool {
    if depth > 8 {
        return false;
    }
    let disk = if block.join("partition").exists() { block.parent().unwrap_or(block) } else { block };
    if let Ok(slaves) = std::fs::read_dir(disk.join("slaves")) {
        let slaves: Vec<PathBuf> = slaves.flatten().filter_map(|entry| std::fs::canonicalize(entry.path()).ok()).collect();
        if !slaves.is_empty() {
            return slaves.iter().any(|slave| removable_block(slave, depth + 1));
        }
    }
    if attribute(disk, "removable").as_deref() == Some("1") {
        return true;
    }
    // SD cards; eMMC ("MMC") is soldered on.
    if attribute(disk, "device/type").as_deref() == Some("SD") {
        return true;
    }
    for ancestor in disk.ancestors().skip(1) {
        let usb = std::fs::read_link(ancestor.join("subsystem")).is_ok_and(|target| target.file_name().is_some_and(|name| name == "usb"));
        match attribute(ancestor, "removable").as_deref() {
            Some("removable") => return true,
            Some("fixed") => return false,
            // "unknown" is what ports without firmware information say.
            _ if usb => return true,
            _ => {}
        }
    }
    false
}

/// Write everything the filesystem holding `directory` still has in memory to
/// its device (`syncfs`). Returns when it is there, or with the write error.
pub fn flush_filesystem(directory: &Path) -> Result<()> {
    let handle = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC).open(directory)?;
    if unsafe { libc::syncfs(handle.as_raw_fd()) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(())
}

/// The next window: about [`WINDOW_TIME`] of writeback at the speed just
/// measured, at most double the last one, within the fixed bounds.
pub fn next_window(current: u64, written: u64, elapsed: Duration) -> u64 {
    let seconds = elapsed.as_secs_f64().max(0.000_001);
    let aim = (written as f64 / seconds * WINDOW_TIME.as_secs_f64()) as u64;
    aim.clamp(WINDOW_MIN, (current * 2).min(WINDOW_MAX))
}

fn sync_range(file: &File, offset: u64, length: u64, flags: libc::c_uint) -> Result<()> {
    loop {
        if unsafe { libc::sync_file_range(file.as_raw_fd(), offset as libc::off64_t, length as libc::off64_t, flags) } == 0 {
            return Ok(());
        }
        let error = Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// A filesystem that cannot write back part of a file (the copy then relies
/// on the flush at its end), as opposed to a write that failed.
fn unsupported(error: &Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::ESPIPE | libc::EBADF))
}

/// Copy the regular file `source` to the new file `destination` (which must
/// not exist) on a [`slow_destination`], data only.
///
/// The data is written in windows. A full window is handed to the kernel for
/// writeback, and the copy goes on filling the next one; it only waits when
/// the window before is not on the device yet. So at most two windows are in
/// memory and not written, and `progress(written_to_device, total)` is never
/// more than that ahead of the truth. A file larger than [`WINDOW_MAX`] is
/// fsynced before this returns; smaller ones are left to the
/// [`flush_filesystem`] that must end the operation.
///
/// `cancelled` is polled between chunks. On any error, cancellation included
/// (`ErrorKind::Interrupted`), the partial destination is removed.
pub fn copy_file(source: &Path, destination: &Path, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let mut input = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(source)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    let output = std::fs::OpenOptions::new().write(true).create_new(true).mode(metadata.mode() & 0o777)
        .custom_flags(libc::O_CLOEXEC).open(destination)?;
    let outcome = copy_data(&mut input, output, metadata.len(), cancelled, progress);
    if outcome.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    outcome
}

fn copy_data(input: &mut File, output: File, total: u64, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let stop = || Error::new(ErrorKind::Interrupted, "cancelled");
    let mut output = WindowedWriter::new(output);
    let mut buffer = vec![0u8; CHUNK];
    let mut reported = 0u64;
    loop {
        if cancelled() {
            return Err(stop());
        }
        let count = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        output.write_all(&buffer[..count])?;
        if output.on_device() != reported {
            reported = output.on_device();
            progress(reported, total.max(output.written()));
        }
    }
    if cancelled() {
        return Err(stop());
    }
    let written = output.finish()?;
    progress(written, total.max(written));
    Ok(())
}

/// A new file on a [`slow_destination`], written from its start to its end
/// in writeback windows (see [`copy_file`]): for data that does not come
/// from another file, such as an archive member being extracted.
pub struct WindowedWriter {
    file: File,
    written: u64,
    /// Where the window being filled starts, and its size.
    start: u64,
    size: u64,
    /// The window handed to the kernel and not yet known to be on the device.
    previous: Option<(u64, u64, Instant)>,
    on_device: u64,
    /// False once the filesystem turned out not to write back parts of files.
    windows: bool,
}

impl WindowedWriter {
    /// `file` must be empty and positioned at its start.
    pub fn new(file: File) -> Self {
        WindowedWriter { file, written: 0, start: 0, size: WINDOW_MIN, previous: None, on_device: 0, windows: true }
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    /// How many bytes the device is known to hold. At most two windows
    /// behind [`written`](Self::written).
    pub fn on_device(&self) -> u64 {
        if self.windows { self.on_device } else { self.written }
    }

    fn window_filled(&mut self) -> Result<()> {
        let length = self.written - self.start;
        match sync_range(&self.file, self.start, length, libc::SYNC_FILE_RANGE_WRITE) {
            Ok(()) => {}
            Err(error) if unsupported(&error) => {
                self.windows = false;
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        if let Some((offset, length, began)) = self.previous.take() {
            sync_range(&self.file, offset, length, libc::SYNC_FILE_RANGE_WAIT_BEFORE | libc::SYNC_FILE_RANGE_WRITE | libc::SYNC_FILE_RANGE_WAIT_AFTER)?;
            self.size = next_window(self.size, length, began.elapsed());
            self.on_device = offset + length;
        }
        self.previous = Some((self.start, length, Instant::now()));
        self.start = self.written;
        Ok(())
    }

    /// End the file: one larger than [`WINDOW_MAX`] is fsynced, and close
    /// errors are returned (NFS reports failed writes there, and dropping a
    /// file hides them). Returns the bytes written.
    pub fn finish(self) -> Result<u64> {
        if self.written > WINDOW_MAX {
            self.file.sync_all()?;
        }
        let descriptor = std::os::fd::IntoRawFd::into_raw_fd(self.file);
        if unsafe { libc::close(descriptor) } != 0 {
            let error = Error::last_os_error();
            if error.kind() != ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(self.written)
    }
}

impl Write for WindowedWriter {
    fn write(&mut self, buffer: &[u8]) -> Result<usize> {
        let count = self.file.write(buffer)?;
        self.written += count as u64;
        if self.windows && self.written - self.start >= self.size {
            self.window_filled()?;
        }
        Ok(count)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::{symlink, PermissionsExt};

    /// Scratch under the repository's own `target/` (never `$TMPDIR`).
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch").join(format!("tessel-core-device-copy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    const MOUNTS: &str = "\
25 1 0:23 / / rw,relatime shared:1 - btrfs /dev/nvme0n1p3 rw,subvol=/root
60 25 8:17 / /run/media/u/USB\\040STICK rw,nosuid shared:30 - vfat /dev/sdb1 rw,fmask=0022
61 25 0:55 / /mnt/nas rw,relatime shared:31 - cifs //nas/share rw,vers=3.1.1
62 25 0:56 / /mnt/nas/deep rw shared:32 - nfs4 nas:/export rw
63 25 0:57 / /home/u/remote rw - fuse.sshfs u@host:/ rw
64 25 8:33 / /mnt/ntfs rw - fuseblk /dev/sdc1 rw
65 25 0:58 / /run/user/1000 rw - tmpfs tmpfs rw
66 25 8:49 / /mnt/over rw - ext4 /dev/sdd1 rw
67 25 8:65 / /mnt/over rw - ext4 /dev/sde1 rw
";

    #[test]
    fn the_mount_of_a_path() {
        let mount = |path: &str| mount_of(MOUNTS, Path::new(path)).unwrap();
        assert_eq!(mount("/home/u/file").source, "/dev/nvme0n1p3");
        assert_eq!(mount("/run/media/u/USB STICK/folder").source, "/dev/sdb1", "escaped spaces");
        assert_eq!(mount("/run/media/u/USB STICKY").source, "/dev/nvme0n1p3", "whole components only");
        assert_eq!(mount("/mnt/nas/x").filesystem, "cifs");
        assert_eq!(mount("/mnt/nas/deep/x").filesystem, "nfs4", "the longest mount point");
        assert_eq!(mount("/mnt/over").source, "/dev/sde1", "the later mount covers the earlier");
        assert!(mount_of("garbage\n", Path::new("/")).is_none());
    }

    /// A fake sysfs: `sdb` on a USB port that says "unknown", `sdc` on one
    /// marked fixed, `sdd` a SATA disk, `sde` reporting removable media,
    /// `mmcblk0` an SD card, `mmcblk1` eMMC, `dm-0` encrypted on `sdb1`,
    /// `dm-1` on `sdd1`, and `nvme1n1` in a Thunderbolt enclosure.
    fn fake_sys() -> PathBuf {
        let sys = scratch("sys");
        let dir = |path: &str| {
            std::fs::create_dir_all(sys.join(path)).unwrap();
            sys.join(path)
        };
        let write = |path: &str, text: &str| {
            dir(Path::new(path).parent().unwrap().to_str().unwrap());
            std::fs::write(sys.join(path), text).unwrap();
        };
        dir("bus/usb");
        dir("bus/pci");
        dir("dev/block");
        let number = Cell::new(0);
        let disk = |path: &str, removable: &str, partition: Option<&str>| {
            write(&format!("{path}/removable"), removable);
            number.set(number.get() + 16);
            symlink(sys.join(path), sys.join(format!("dev/block/8:{}", number.get()))).unwrap();
            if let Some(name) = partition {
                write(&format!("{path}/{name}/partition"), "1\n");
                symlink(sys.join(path).join(name), sys.join(format!("dev/block/8:{}", number.get() + 1))).unwrap();
            }
        };
        let usb = |path: &str, removable: &str| {
            write(&format!("{path}/removable"), removable);
            symlink(sys.join("bus/usb"), sys.join(path).join("subsystem")).unwrap();
        };
        usb("devices/pci0/usb2/2-1", "unknown\n");
        disk("devices/pci0/usb2/2-1/2-1:1.0/host6/target6/6:0:0:0/block/sdb", "0\n", Some("sdb1")); // 8:16, 8:17
        usb("devices/pci0/usb1/1-4", "fixed\n");
        disk("devices/pci0/usb1/1-4/1-4:1.0/host7/target7/7:0:0:0/block/sdc", "0\n", Some("sdc1")); // 8:32, 8:33
        disk("devices/pci0/ata1/host0/target0/0:0:0:0/block/sdd", "0\n", Some("sdd1")); // 8:48, 8:49
        disk("devices/pci0/ata2/host1/target1/1:0:0:0/block/sde", "1\n", Some("sde1")); // 8:64, 8:65
        disk("devices/platform/mmc_host/mmc0/mmc0:aaaa/block/mmcblk0", "0\n", Some("mmcblk0p1")); // 8:80, 8:81
        write("devices/platform/mmc_host/mmc0/mmc0:aaaa/type", "SD\n");
        symlink(sys.join("devices/platform/mmc_host/mmc0/mmc0:aaaa"), sys.join("devices/platform/mmc_host/mmc0/mmc0:aaaa/block/mmcblk0/device")).unwrap();
        disk("devices/platform/mmc_host/mmc1/mmc1:0001/block/mmcblk1", "0\n", Some("mmcblk1p1")); // 8:96, 8:97
        write("devices/platform/mmc_host/mmc1/mmc1:0001/type", "MMC\n");
        symlink(sys.join("devices/platform/mmc_host/mmc1/mmc1:0001"), sys.join("devices/platform/mmc_host/mmc1/mmc1:0001/block/mmcblk1/device")).unwrap();
        disk("devices/virtual/block/dm-0", "0\n", None); // 8:112
        dir("devices/virtual/block/dm-0/slaves");
        symlink(sys.join("devices/pci0/usb2/2-1/2-1:1.0/host6/target6/6:0:0:0/block/sdb/sdb1"), sys.join("devices/virtual/block/dm-0/slaves/sdb1")).unwrap();
        disk("devices/virtual/block/dm-1", "0\n", None); // 8:128
        dir("devices/virtual/block/dm-1/slaves");
        symlink(sys.join("devices/pci0/ata1/host0/target0/0:0:0:0/block/sdd/sdd1"), sys.join("devices/virtual/block/dm-1/slaves/sdd1")).unwrap();
        write("devices/pci0/0000:05:00.0/removable", "removable\n");
        symlink(sys.join("bus/pci"), sys.join("devices/pci0/0000:05:00.0/subsystem")).unwrap();
        disk("devices/pci0/0000:05:00.0/0000:06:00.0/nvme/nvme1/nvme1n1", "0\n", Some("nvme1n1p1")); // 8:144, 8:145
        dir("devices/virtual/block/dm-0/empty");
        sys
    }

    #[test]
    fn which_mounts_are_slow() {
        let sys = fake_sys();
        let numbers = |device: &Path| -> Option<(u32, u32)> {
            let minor = match device.to_str()? {
                "/dev/sdb1" => 17,
                "/dev/sdc1" => 33,
                "/dev/sdd1" => 49,
                "/dev/sde1" => 65,
                "/dev/mmcblk0p1" => 81,
                "/dev/mmcblk1p1" => 97,
                "/dev/mapper/usb" => 112,
                "/dev/mapper/home" => 128,
                "/dev/nvme1n1p1" => 145,
                "/dev/sdb" => 16,
                _ => return None,
            };
            Some((8, minor))
        };
        let policy = |filesystem: &str, source: &str| {
            classify_mount(&Mount { point: "/x".into(), filesystem: filesystem.into(), source: source.into() }, &sys, &numbers)
        };
        assert_eq!(policy("vfat", "/dev/sdb1"), Some(Slow::Removable), "a USB disk that does not report removable media");
        assert_eq!(policy("exfat", "/dev/sdb"), Some(Slow::Removable), "a whole disk without partitions");
        assert_eq!(policy("ext4", "/dev/sdc1"), None, "a USB disk on a port marked fixed");
        assert_eq!(policy("ext4", "/dev/sdd1"), None, "an internal disk");
        assert_eq!(policy("vfat", "/dev/sde1"), Some(Slow::Removable), "removable media");
        assert_eq!(policy("vfat", "/dev/mmcblk0p1"), Some(Slow::Removable), "an SD card");
        assert_eq!(policy("ext4", "/dev/mmcblk1p1"), None, "eMMC");
        assert_eq!(policy("ext4", "/dev/mapper/usb"), Some(Slow::Removable), "encrypted volume on a USB disk");
        assert_eq!(policy("ext4", "/dev/mapper/home"), None, "encrypted volume on an internal disk");
        assert_eq!(policy("btrfs", "/dev/nvme1n1p1"), Some(Slow::Removable), "a port the kernel marks removable");
        assert_eq!(policy("fuseblk", "/dev/sdb1"), Some(Slow::Removable), "ntfs-3g on a USB disk");
        assert_eq!(policy("fuseblk", "/dev/sdd1"), None);
        assert_eq!(policy("ext4", "/dev/unknown"), None);
        assert_eq!(policy("tmpfs", "tmpfs"), None);
        for filesystem in ["nfs", "nfs4", "cifs", "smb3", "9p", "fuse.sshfs", "fuse.rclone", "fuse"] {
            assert_eq!(policy(filesystem, "server:/x"), Some(Slow::Network), "{filesystem}");
        }
        std::fs::remove_dir_all(&sys).unwrap();
    }

    #[test]
    fn windows_follow_the_measured_speed() {
        let ms = Duration::from_millis;
        assert_eq!(next_window(MIB, MIB, ms(1000)), WINDOW_MIN, "1 MiB/s stays at the smallest window");
        assert_eq!(next_window(MIB, MIB, ms(1)), 2 * MIB, "at most double");
        assert_eq!(next_window(8 * MIB, 8 * MIB, ms(1)), WINDOW_MAX);
        assert_eq!(next_window(16 * MIB, 16 * MIB, ms(1)), WINDOW_MAX, "never above the cap");
        assert_eq!(next_window(8 * MIB, 8 * MIB, ms(400)), 4 * MIB, "20 MiB/s aims at 4 MiB");
        assert_eq!(next_window(4 * MIB, 4 * MIB, Duration::ZERO), 8 * MIB);
    }

    fn pattern(length: usize) -> Vec<u8> {
        (0..length).map(|i| (i % 251) as u8 ^ (i / 4099) as u8).collect()
    }

    #[test]
    fn copies_are_complete_and_progress_never_runs_backwards() {
        let dir = scratch("copy");
        for length in [0usize, 1, CHUNK, MIB as usize - 1, MIB as usize, 5 * MIB as usize + 12345, 40 * MIB as usize + 7] {
            let source = dir.join("source");
            let destination = dir.join("destination");
            std::fs::write(&source, pattern(length)).unwrap();
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
            let reports = RefCell::new(Vec::new());
            copy_file(&source, &destination, &|| false, &mut |done, total| reports.borrow_mut().push((done, total))).unwrap();
            assert_eq!(std::fs::read(&destination).unwrap(), pattern(length), "{length} bytes");
            assert_eq!(std::fs::metadata(&destination).unwrap().permissions().mode() & 0o777, 0o640);
            let reports = reports.into_inner();
            assert_eq!(reports.last(), Some(&(length as u64, length as u64)));
            assert!(reports.windows(2).all(|pair| pair[0].0 <= pair[1].0), "{reports:?}");
            assert!(reports.iter().all(|&(done, total)| done <= total && total == length as u64));
            if length as u64 > 4 * MIB {
                assert!(reports.len() > 2, "progress is reported while copying: {reports:?}");
            }
            std::fs::remove_file(&destination).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_windowed_writer_takes_writes_of_any_size() {
        let dir = scratch("writer");
        let data = pattern(20 * MIB as usize + 3);
        let mut writer = WindowedWriter::new(File::create(dir.join("out")).unwrap());
        let mut last = 0;
        for piece in data.chunks(70_001) {
            writer.write_all(piece).unwrap();
            assert!(writer.on_device() >= last && writer.on_device() <= writer.written());
            last = writer.on_device();
        }
        assert!(last > 0, "windows were written back along the way");
        assert_eq!(writer.finish().unwrap(), data.len() as u64);
        assert_eq!(std::fs::read(dir.join("out")).unwrap(), data);
        let empty = WindowedWriter::new(File::create(dir.join("empty")).unwrap());
        assert_eq!(empty.finish().unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failures_leave_nothing_behind() {
        let dir = scratch("failures");
        let source = dir.join("source");
        std::fs::write(&source, pattern(3 * MIB as usize)).unwrap();
        // Cancelled part way: the partial file is removed.
        let calls = Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() > 4
        };
        let error = copy_file(&source, &dir.join("partial"), &cancelled, &mut |_, _| {}).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
        assert!(!dir.join("partial").exists());
        // An existing destination is never overwritten or removed.
        std::fs::write(dir.join("existing"), b"keep").unwrap();
        let error = copy_file(&source, &dir.join("existing"), &|| false, &mut |_, _| {}).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(dir.join("existing")).unwrap(), b"keep");
        // Links and folders are not regular files.
        symlink(&source, dir.join("link")).unwrap();
        assert!(copy_file(&dir.join("link"), &dir.join("from-link"), &|| false, &mut |_, _| {}).is_err());
        assert!(copy_file(&dir, &dir.join("from-folder"), &|| false, &mut |_, _| {}).is_err());
        assert!(!dir.join("from-link").exists() && !dir.join("from-folder").exists());
        assert!(copy_file(&dir.join("missing"), &dir.join("from-missing"), &|| false, &mut |_, _| {}).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flushing_a_filesystem() {
        let dir = scratch("flush");
        std::fs::write(dir.join("file"), b"data").unwrap();
        flush_filesystem(&dir).unwrap();
        assert!(flush_filesystem(&dir.join("missing")).is_err());
        assert!(flush_filesystem(&dir.join("file")).is_err(), "a folder is required");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
