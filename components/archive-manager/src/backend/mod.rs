//! Backends (Ark's kerfuffle plugins). Each one is synchronous and runs on a
//! worker thread; the table of which backend handles which format at what
//! priority lives in `archive_settings::plugins`.

pub mod cli7z;
pub mod clizip;
pub mod libarchive;
pub mod process;
pub mod unar;
pub mod zipcomment;

use crate::archive::{AddItem, CreateOptions, Ctx, Entry, Error, Listing, Result};
use archive_settings::plugins::Kind;
use std::path::{Path, PathBuf};

pub trait Backend: Send + Sync {
    fn id(&self) -> &'static str;
    /// `volumes`: every volume of a split archive, first one first (else just `archive`).
    fn list(&self, archive: &Path, kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<Listing>;
    /// Extract `entries` (exact listing paths, folders already expanded; None = all)
    /// into the empty folder `dest`, keeping their paths.
    fn extract(&self, archive: &Path, kind: Kind, volumes: &[PathBuf], entries: Option<&[String]>, dest: &Path, ctx: &Ctx) -> Result<()>;
    fn test(&self, archive: &Path, kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<()>;
    /// Add files (creating the archive when it does not exist).
    fn add(&self, _archive: &Path, kind: Kind, _items: &[AddItem], _options: &CreateOptions, _ctx: &Ctx) -> Result<()> {
        Err(read_only(kind))
    }
    /// Delete entries (folders already expanded to every entry below them).
    fn delete(&self, _archive: &Path, kind: Kind, _entries: &[String], _ctx: &Ctx) -> Result<()> {
        Err(read_only(kind))
    }
    /// Rename/move: (old path, new path) pairs; a folder pair also moves everything below it.
    fn rename(&self, _archive: &Path, kind: Kind, _renames: &[(String, String)], _ctx: &Ctx) -> Result<()> {
        Err(read_only(kind))
    }
}

fn read_only(kind: Kind) -> Error {
    Error::Unsupported(suite_core::tr!("{format} files cannot be changed.", format = suite_core::tr!(kind.label())))
}

pub fn get(id: &str) -> Option<&'static dyn Backend> {
    static LIBARCHIVE: libarchive::LibArchive = libarchive::LibArchive { single: false };
    static SINGLE: libarchive::LibArchive = libarchive::LibArchive { single: true };
    static SEVENZIP: cli7z::SevenZip = cli7z::SevenZip;
    static ZIP: clizip::InfoZip = clizip::InfoZip;
    static UNAR: unar::Unarchiver = unar::Unarchiver;
    Some(match id {
        "libarchive" => &LIBARCHIVE,
        "libsinglefile" => &SINGLE,
        "cli7z" => &SEVENZIP,
        "clizip" => &ZIP,
        "cliunarchiver" => &UNAR,
        _ => return None,
    })
}

/// Map a path through (old, new) renames; the first matching pair wins and
/// also moves everything below a renamed folder.
pub fn apply_renames(path: &str, renames: &[(String, String)]) -> String {
    for (old, new) in renames {
        if path == old {
            return new.clone();
        }
        if let Some(rest) = path.strip_prefix(old.as_str()).and_then(|r| r.strip_prefix('/')) {
            return format!("{new}/{rest}");
        }
    }
    path.to_string()
}

/// Every volume of a split/multi-volume archive, first one first.
/// `x.7z.001`, `x.zip.001`, `x.part1.rar` (`x.part01.rar`), `x.rar` + `x.r00`, `x.zip` + `x.z01`.
pub fn volumes(archive: &Path) -> Vec<PathBuf> {
    let Some(name) = archive.file_name().map(|n| n.to_string_lossy().into_owned()) else { return vec![archive.into()] };
    let dir = archive.parent().map(Path::to_path_buf).unwrap_or_default();
    let exists = |n: &str| dir.join(n).is_file();
    // name.001, name.002 ...
    if let Some((stem, number)) = name.rsplit_once('.') {
        if number.len() == 3 && number.bytes().all(|b| b.is_ascii_digit()) {
            let mut out = Vec::new();
            for i in 1..1000 {
                let candidate = format!("{stem}.{i:03}");
                if !exists(&candidate) {
                    break;
                }
                out.push(dir.join(candidate));
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    let lower = name.to_lowercase();
    // name.partN.rar
    if let Some(pos) = lower.rfind(".part") {
        let digits = &lower[pos + 5..];
        if let Some(number) = digits.strip_suffix(".rar") {
            if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
                let width = number.len();
                let stem = &name[..pos];
                let mut out = Vec::new();
                for i in 1..10000 {
                    let candidate = format!("{stem}.part{i:0width$}.rar");
                    if !exists(&candidate) {
                        break;
                    }
                    out.push(dir.join(candidate));
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }
    // name.rar + name.r00 ..., name.zip + name.z01 ...
    for (ext, letter, first) in [(".rar", 'r', 0), (".zip", 'z', 1)] {
        if lower.ends_with(ext) {
            let stem = &name[..name.len() - 4];
            let mut parts = Vec::new();
            for i in first..100 {
                let candidate = format!("{stem}.{letter}{i:02}");
                if !exists(&candidate) {
                    break;
                }
                parts.push(dir.join(candidate));
            }
            if !parts.is_empty() {
                if letter == 'r' {
                    let mut out = vec![archive.to_path_buf()];
                    out.extend(parts);
                    return out;
                }
                parts.push(archive.to_path_buf());
                return parts;
            }
        }
    }
    vec![archive.to_path_buf()]
}

/// Writes extracted entries below a root folder, refusing to follow links
/// out of it (an archive may contain `a -> /etc` followed by `a/passwd`).
pub struct DiskWriter {
    root: PathBuf,
    dirs: Vec<(PathBuf, u32, Option<i64>)>,
}

impl DiskWriter {
    pub fn new(root: &Path) -> Self {
        DiskWriter { root: root.to_path_buf(), dirs: Vec::new() }
    }

    /// The target path, with its parent folders created (never through a link).
    pub fn target(&self, relative: &str) -> Result<PathBuf> {
        let clean = crate::archive::clean_path(relative).ok_or_else(|| Error::Failed(format!("unsafe path: {relative}")))?;
        let mut current = self.root.clone();
        let parts: Vec<&str> = clean.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            current.push(part);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(Error::Failed(format!("unsafe path: {relative}"))),
                Err(_) => std::fs::create_dir(&current)?,
            }
        }
        current.push(parts[parts.len() - 1]);
        if let Ok(meta) = std::fs::symlink_metadata(&current) {
            if !meta.is_dir() {
                std::fs::remove_file(&current)?;
            }
        }
        Ok(current)
    }

    pub fn dir(&mut self, entry: &Entry) -> Result<()> {
        let path = self.target(&entry.path)?;
        if !path.is_dir() {
            std::fs::create_dir(&path)?;
        }
        self.dirs.push((path, entry.mode.unwrap_or(0o755), entry.mtime));
        Ok(())
    }

    pub fn file(&mut self, entry: &Entry) -> Result<std::fs::File> {
        let path = self.target(&entry.path)?;
        use std::os::unix::fs::OpenOptionsExt;
        Ok(std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path)?)
    }

    pub fn finish_file(&mut self, entry: &Entry) {
        if let Ok(path) = self.resolved(&entry.path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.mode.unwrap_or(0o644) & 0o777;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode | 0o400));
            set_mtime(&path, entry.mtime);
        }
    }

    fn resolved(&self, relative: &str) -> Result<PathBuf> {
        let clean = crate::archive::clean_path(relative).ok_or_else(|| Error::Failed(format!("unsafe path: {relative}")))?;
        Ok(self.root.join(clean))
    }

    pub fn symlink(&mut self, entry: &Entry, target: &str) -> Result<()> {
        let path = self.target(&entry.path)?;
        std::os::unix::fs::symlink(target, &path)?;
        Ok(())
    }

    pub fn hardlink(&mut self, entry: &Entry, existing: &str) -> Result<()> {
        let source = self.resolved(existing)?;
        let path = self.target(&entry.path)?;
        let meta = std::fs::symlink_metadata(&source).ok().filter(|m| m.is_file());
        if meta.is_some() && std::fs::hard_link(&source, &path).is_err() {
            std::fs::copy(&source, &path)?;
        }
        Ok(())
    }

    /// Apply folder modes and times last (a read-only folder must not block its contents).
    pub fn finish(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        for (path, mode, mtime) in self.dirs.drain(..).rev() {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode((mode & 0o777) | 0o700));
            set_mtime(&path, mtime);
        }
    }
}

pub fn set_mtime(path: &Path, mtime: Option<i64>) {
    use std::os::unix::ffi::OsStrExt;
    let Some(seconds) = mtime else { return };
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return };
    let times = [libc::timespec { tv_sec: seconds as libc::time_t, tv_nsec: 0 }, libc::timespec { tv_sec: seconds as libc::time_t, tv_nsec: 0 }];
    unsafe { libc::utimensat(libc::AT_FDCWD, name.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
}

/// Lay out `items` below `root` as symlinks at their archive paths, for tools
/// that add files relative to their working folder (7-Zip follows the links).
pub fn stage_links(root: &Path, items: &[AddItem]) -> Result<Vec<String>> {
    let mut tops: Vec<String> = Vec::new();
    for item in items {
        let dest = crate::archive::clean_path(&item.dest).ok_or_else(|| Error::Failed(format!("invalid archive path: {}", item.dest)))?;
        let path = root.join(&dest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let source = std::fs::canonicalize(&item.source).map_err(|e| Error::Failed(format!("{}: {e}", item.source.display())))?;
        if std::fs::symlink_metadata(&path).is_err() {
            std::os::unix::fs::symlink(&source, &path)?;
        }
        let top = dest.split('/').next().unwrap_or(&dest).to_string();
        if !tops.contains(&top) {
            tops.push(top);
        }
    }
    Ok(tops)
}

/// Group `items` whose archive path is just their own file name by folder:
/// those can be added from that folder without staging.
pub fn plain_groups(items: &[AddItem]) -> Option<Vec<(PathBuf, Vec<String>)>> {
    let mut groups: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for item in items {
        let name = item.source.file_name()?.to_string_lossy().into_owned();
        if crate::archive::clean_path(&item.dest)? != name || name.starts_with('-') {
            return None;
        }
        let parent = item.source.parent().map(Path::to_path_buf).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| PathBuf::from("."));
        match groups.iter_mut().find(|(p, _)| *p == parent) {
            Some((_, names)) => names.push(name),
            None => groups.push((parent, vec![name])),
        }
    }
    Some(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_move_children() {
        let renames = vec![("a".to_string(), "b/c".to_string()), ("x.txt".to_string(), "y.txt".to_string())];
        assert_eq!(apply_renames("a", &renames), "b/c");
        assert_eq!(apply_renames("a/d/e", &renames), "b/c/d/e");
        assert_eq!(apply_renames("ab", &renames), "ab");
        assert_eq!(apply_renames("x.txt", &renames), "y.txt");
    }

    #[test]
    fn volume_discovery() {
        let dir = std::env::temp_dir().join(format!("wayarchive-volumes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.7z.001", "a.7z.002", "b.part1.rar", "b.part2.rar", "c.zip", "c.z01", "d.rar", "single.zip"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let names = |p: &str| volumes(&dir.join(p)).iter().map(|v| v.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(names("a.7z.001"), ["a.7z.001", "a.7z.002"]);
        assert_eq!(names("b.part1.rar"), ["b.part1.rar", "b.part2.rar"]);
        assert_eq!(names("c.zip"), ["c.z01", "c.zip"]);
        assert_eq!(names("d.rar"), ["d.rar"]);
        assert_eq!(names("single.zip"), ["single.zip"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn disk_writer_refuses_links_out_of_root() {
        let dir = std::env::temp_dir().join(format!("wayarchive-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        let mut w = DiskWriter::new(&dir.join("root"));
        w.symlink(&Entry { path: "a".into(), ..Default::default() }, "../outside").unwrap();
        assert!(w.file(&Entry { path: "a/passwd".into(), ..Default::default() }).is_err());
        assert!(w.target("../x").is_err());
        assert!(std::fs::read_dir(dir.join("outside")).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
