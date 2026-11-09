//! Port of `~/tessel-bluetooth/shared/wallpaper_import.py`: maintain wallpaper
//! collections without replacing files. Call from a worker thread: system
//! collection imports wait for the desktop's polkit dialog. One
//! `pkexec --disable-internal-agent cp --update=none-fail …` per batch.
//!
//! Every external path (`pkexec`, `cp`, `unlink`, the system folders) lives in
//! [`Importer`] so tests can point them at fakes inside `$TMPDIR`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use tessel_core::pyerr;

pub const COPY_CHUNK_SIZE: usize = 1024 * 1024;
pub const WALLPAPER_SUFFIXES: &[&str] = &[".png", ".jpg", ".jpeg", ".webp", ".bmp", ".gif", ".mp4", ".m4v", ".mov", ".webm"];

/// `WallpaperImportError` / `WallpaperImportCancelled`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    Error(String),
    Cancelled(String),
}

/// `WallpaperRemovalError` / `WallpaperRemovalCancelled`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalError {
    Error(String),
    Cancelled(String),
}

impl ImportError {
    pub fn message(&self) -> &str {
        match self {
            ImportError::Error(m) | ImportError::Cancelled(m) => m,
        }
    }
    pub fn is_cancelled(&self) -> bool {
        matches!(self, ImportError::Cancelled(_))
    }
}

impl RemovalError {
    pub fn message(&self) -> &str {
        match self {
            RemovalError::Error(m) | RemovalError::Cancelled(m) => m,
        }
    }
    pub fn is_cancelled(&self) -> bool {
        matches!(self, RemovalError::Cancelled(_))
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::fmt::Display for RemovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// `(imported paths, file errors, was_cancelled)`.
pub type ImportOutcome = (Vec<PathBuf>, Vec<String>, bool);
/// `(st_dev, st_ino)`.
pub type Identity = (u64, u64);

/// Paths and test seams of the importer (`Importer::default()` = production).
#[derive(Clone)]
pub struct Importer {
    pub system_folders: Vec<PathBuf>,
    pub pkexec: PathBuf,
    pub copy: PathBuf,
    pub unlink: PathBuf,
    /// Test seam (Python's mocked `_copy_unprivileged` / `os.unlink`): behave
    /// as if unprivileged writes and unlinks failed with EACCES.
    pub deny_unprivileged: bool,
    /// Test seam: called after every chunked copy (Python's `_copy_bytes`).
    pub after_copy: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for Importer {
    fn default() -> Self {
        Self {
            system_folders: vec![PathBuf::from("/usr/share/applications/wallpapers"), PathBuf::from("/usr/share/wallpapers")],
            pkexec: PathBuf::from("/usr/bin/pkexec"),
            copy: PathBuf::from("/usr/bin/cp"),
            unlink: PathBuf::from("/usr/bin/unlink"),
            deny_unprivileged: false,
            after_copy: None,
        }
    }
}

type Cancelled<'a> = Option<&'a dyn Fn() -> bool>;
type Validate<'a> = Option<&'a dyn Fn(&Path) -> Result<(), String>>;

fn check_cancelled(cancelled: Cancelled) -> Result<(), ImportError> {
    if cancelled.is_some_and(|c| c()) {
        return Err(ImportError::Cancelled("Wallpaper import was cancelled.".into()));
    }
    Ok(())
}

fn expanduser(path: &Path) -> PathBuf {
    let text = path.as_os_str().as_bytes();
    if text == b"~" || text.starts_with(b"~/") {
        let home = tessel_core::paths::home();
        if text.len() <= 2 {
            return home;
        }
        return home.join(std::ffi::OsStr::from_bytes(&text[2..]));
    }
    path.to_path_buf()
}

/// `os.path.abspath`: absolute and normalized, symlinks untouched.
pub fn abspath(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut out = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            _ => {}
        }
    }
    out
}

fn resolve_strict(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map_err(|e| io::Error::new(e.kind(), pyerr::os_error_path(&e, path)))
}

fn lexists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// `Path.suffix` (a leading dot or a trailing dot is not a suffix).
fn suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => &name[dot..],
        _ => "",
    }
}

fn truncate_bytes(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Keep the recognizable basename, but never a hidden entry or control chars.
pub fn safe_filename(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if (c as u32) < 32 || c as u32 == 127 { '_' } else { c }).collect();
    let cleaned = cleaned.trim_start_matches('.');
    let name = if cleaned.is_empty() { "wallpaper" } else { cleaned };
    let suffix = suffix(name);
    let stem = &name[..name.len() - suffix.len()];
    let stem = truncate_bytes(stem, 180);
    let suffix = truncate_bytes(suffix, 32);
    format!("{}{}", if stem.is_empty() { "wallpaper" } else { stem }, suffix)
}

/// `folder/name`, then `folder/stem (2).ext` … `(10001)`.
pub fn candidates(folder: &Path, name: &str) -> impl Iterator<Item = PathBuf> {
    let safe = safe_filename(name);
    let ext = suffix(&safe).to_string();
    let stem = safe[..safe.len() - ext.len()].to_string();
    let folder = folder.to_path_buf();
    std::iter::once(folder.join(&safe)).chain((2..10002).map(move |n| folder.join(format!("{stem} ({n}){ext}"))))
}

fn identity(meta: &fs::Metadata) -> Identity {
    (meta.dev(), meta.ino())
}

fn remove_own_partial(path: &Path, own: Identity) {
    if let Ok(current) = fs::symlink_metadata(path) {
        if identity(&current) == own {
            let _ = fs::remove_file(path);
        }
    }
}

enum Failure {
    Import(ImportError),
    Os(io::Error),
}

impl From<ImportError> for Failure {
    fn from(error: ImportError) -> Self {
        Failure::Import(error)
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Os(error)
    }
}

impl Failure {
    fn text(&self) -> String {
        match self {
            Failure::Import(e) => e.message().to_string(),
            Failure::Os(e) => py_os_text(e),
        }
    }
}

/// `str(OSError)`: messages that already carry Python text pass through.
fn py_os_text(error: &io::Error) -> String {
    let text = error.to_string();
    if text.starts_with("[Errno") {
        text
    } else if error.raw_os_error().is_some() {
        pyerr::os_error_text(error)
    } else {
        text
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> io::Result<Self> {
        let template = std::env::temp_dir().join(format!("{prefix}XXXXXX"));
        let mut bytes = template.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        let result = unsafe { libc::mkdtemp(bytes.as_mut_ptr() as *mut libc::c_char) };
        if result.is_null() {
            return Err(io::Error::last_os_error());
        }
        bytes.pop();
        Ok(TempDir(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Pending {
    index: usize,
    original: PathBuf,
    snapshot: PathBuf,
    target: PathBuf,
}

fn files_equal(a: &Path, b: &Path) -> bool {
    match (fs::read(a), fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

impl Importer {
    fn copy_bytes(&self, source: &mut File, target: &mut File, cancelled: Cancelled) -> Result<(), Failure> {
        let mut buffer = vec![0u8; COPY_CHUNK_SIZE];
        let outcome = (|| {
            loop {
                check_cancelled(cancelled)?;
                let n = read_full(source, &mut buffer)?;
                if n == 0 {
                    break;
                }
                target.write_all(&buffer[..n])?;
            }
            target.flush()?;
            Ok(())
        })();
        if let Some(hook) = &self.after_copy {
            hook();
        }
        outcome
    }

    fn copy_unprivileged(&self, source: &mut File, folder: &Path, filename: &str, cancelled: Cancelled) -> Result<PathBuf, Failure> {
        if self.deny_unprivileged {
            return Err(Failure::Os(io::Error::from_raw_os_error(libc::EACCES)));
        }
        for target in candidates(folder, filename) {
            check_cancelled(cancelled)?;
            let mut output = match OpenOptions::new().write(true).create_new(true).mode(0o644).open(&target) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            };
            let own = identity(&output.metadata()?);
            match self.copy_bytes(source, &mut output, cancelled) {
                Ok(()) => return Ok(target),
                Err(error) => {
                    drop(output);
                    remove_own_partial(&target, own);
                    return Err(error);
                }
            }
        }
        Err(ImportError::Error("Too many wallpapers already use this filename.".into()).into())
    }

    fn allowed(&self) -> Vec<PathBuf> {
        self.system_folders.iter().map(|p| fs::canonicalize(p).unwrap_or_else(|_| abspath(p))).collect()
    }

    fn require_system_copy(&self, folder: &Path) -> Result<(), ImportError> {
        if !self.allowed().iter().any(|p| p == folder) {
            return Err(ImportError::Error(format!("You do not have permission to add wallpapers to {}.", folder.display())));
        }
        if !self.pkexec.is_file() || !self.copy.is_file() {
            return Err(ImportError::Error("Administrator authentication is unavailable for the system wallpaper folder.".into()));
        }
        Ok(())
    }

    /// The exact elevated command for one batch.
    pub fn batch_command(&self, snapshots: &[PathBuf], folder: &Path) -> Vec<std::ffi::OsString> {
        let mut command: Vec<std::ffi::OsString> = vec![
            self.pkexec.clone().into(),
            "--disable-internal-agent".into(),
            self.copy.clone().into(),
            "--update=none-fail".into(),
            "--no-preserve=mode,ownership".into(),
            "--".into(),
        ];
        command.extend(snapshots.iter().map(|p| p.clone().into_os_string()));
        command.push(folder.as_os_str().to_owned());
        command
    }

    fn copy_system_batch(
        &self,
        pending: &[Pending],
        folder: &Path,
        imported: &mut Vec<(usize, PathBuf)>,
        errors: &mut Vec<String>,
        cancelled: Cancelled,
    ) -> Result<(), Failure> {
        check_cancelled(cancelled)?;
        // One cp process handles the whole batch, so pkexec authenticates once.
        // Let an authenticated copy finish even if the window closes.
        let snapshots: Vec<PathBuf> = pending.iter().map(|p| p.snapshot.clone()).collect();
        let command = self.batch_command(&snapshots, folder);
        let output = Command::new(&command[0]).args(&command[1..]).stdin(Stdio::null()).output()?;
        let code = output.status.code().unwrap_or(1);
        if code == 126 {
            return Err(ImportError::Cancelled("Administrator authentication was cancelled.".into()).into());
        }
        if code == 127 {
            return Err(ImportError::Error(
                "Administrator authentication was denied or unavailable. The wallpapers were not added.".into(),
            )
            .into());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        for item in pending {
            let mut copied = match (fs::symlink_metadata(&item.target), fs::metadata(&item.target), fs::metadata(&item.snapshot)) {
                (Ok(link), Ok(target), Ok(snapshot)) => link.file_type().is_file() && target.len() == snapshot.len(),
                _ => false,
            };
            // cp can complete some files before reporting a collision or disk
            // error. Keep those successes and report the remaining files.
            if copied && code != 0 {
                copied = files_equal(&item.snapshot, &item.target);
            }
            if copied {
                imported.push((item.index, item.target.clone()));
            } else {
                let name = item.original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut message = format!("{name}: Could not copy the wallpaper into the system folder.");
                if !stderr.is_empty() {
                    message.push(' ');
                    message.push_str(&stderr);
                }
                errors.push(message);
            }
        }
        Ok(())
    }

    /// Import one batch. Normal permissions first, then all protected copies
    /// in one authenticated operation. Originals and existing names are kept.
    pub fn import_wallpapers(&self, sources: &[PathBuf], destination: &Path, cancelled: Cancelled, validate: Validate) -> ImportOutcome {
        let mut imported: Vec<(usize, PathBuf)> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let result = self.import_inner(sources, destination, cancelled, validate, &mut imported, &mut errors);
        let was_cancelled = match result {
            Ok(()) => cancelled.is_some_and(|c| c()),
            Err(Failure::Import(ImportError::Cancelled(_))) => true,
            Err(error) => {
                errors.push(error.text());
                false
            }
        };
        imported.sort_by_key(|(index, _)| *index);
        (imported.into_iter().map(|(_, p)| p).collect(), errors, was_cancelled)
    }

    fn import_inner(
        &self,
        sources: &[PathBuf],
        destination: &Path,
        cancelled: Cancelled,
        validate: Validate,
        imported: &mut Vec<(usize, PathBuf)>,
        errors: &mut Vec<String>,
    ) -> Result<(), Failure> {
        check_cancelled(cancelled)?;
        let folder = resolve_strict(&expanduser(destination))?;
        if !folder.is_dir() {
            return Err(ImportError::Error("The wallpaper collection is not a folder.".into()).into());
        }
        let temporary = TempDir::new("wallpaper-import-")?;
        let mut pending: Vec<Pending> = Vec::new();
        let mut reserved: Vec<PathBuf> = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let outcome = (|| -> Result<(), Failure> {
                check_cancelled(cancelled)?;
                if let Some(validate) = validate {
                    validate(source).map_err(ImportError::Error)?;
                }
                let original = resolve_strict(&expanduser(source))?;
                let mut input = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&original)?;
                if !input.metadata()?.file_type().is_file() {
                    return Err(ImportError::Error("Choose a regular image or video file.".into()).into());
                }
                if original.parent() == Some(folder.as_path()) {
                    imported.push((index, original));
                    return Ok(());
                }
                let name = original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match self.copy_unprivileged(&mut input, &folder, &name, cancelled) {
                    Ok(target) => {
                        imported.push((index, target));
                        return Ok(());
                    }
                    Err(Failure::Os(error)) if matches!(error.raw_os_error(), Some(libc::EACCES) | Some(libc::EPERM)) => {}
                    Err(error) => return Err(error),
                }
                self.require_system_copy(&folder)?;
                let target = candidates(&folder, &name)
                    .find(|path| !reserved.contains(path) && !lexists(path))
                    .ok_or_else(|| ImportError::Error("Too many wallpapers already use this filename.".into()))?;
                reserved.push(target.clone());
                let snapshot = temporary.0.join(target.file_name().unwrap());
                input.seek(io::SeekFrom::Start(0))?;
                let mut output = OpenOptions::new().write(true).create_new(true).open(&snapshot)?;
                self.copy_bytes(&mut input, &mut output, cancelled)?;
                pending.push(Pending { index, original, snapshot, target });
                Ok(())
            })();
            match outcome {
                Ok(()) => {}
                Err(Failure::Import(ImportError::Cancelled(m))) => return Err(ImportError::Cancelled(m).into()),
                Err(error) => {
                    let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    errors.push(format!("{name}: {}", error.text()));
                }
            }
        }
        if !pending.is_empty() {
            self.copy_system_batch(&pending, &folder, imported, errors, cancelled)?;
        }
        Ok(())
    }

    /// Single-file interface using the same batch-copy and collision handling.
    pub fn import_wallpaper(&self, source: &Path, destination: &Path, cancelled: Cancelled) -> Result<PathBuf, ImportError> {
        let (imported, errors, was_cancelled) = self.import_wallpapers(&[source.to_path_buf()], destination, cancelled, None);
        if was_cancelled {
            return Err(ImportError::Cancelled("Wallpaper import was cancelled.".into()));
        }
        if let Some(error) = errors.into_iter().next() {
            return Err(ImportError::Error(error));
        }
        Ok(imported.into_iter().next().unwrap())
    }

    /// Remove one confirmed collection entry, never a symlink's target. Only
    /// the system wallpaper folders may request authentication (`unlink`).
    pub fn remove_wallpaper(
        &self,
        path: &Path,
        collection_folder: &Path,
        expected_identity: Option<Identity>,
        cancelled: Cancelled,
    ) -> Result<PathBuf, RemovalError> {
        let check = || -> Result<(), RemovalError> {
            if cancelled.is_some_and(|c| c()) {
                return Err(RemovalError::Cancelled("Wallpaper removal was cancelled.".into()));
            }
            Ok(())
        };
        check()?;
        let wrap = |e: io::Error| RemovalError::Error(format!("Could not remove the wallpaper: {}", py_os_text(&e)));
        let source = abspath(&expanduser(path));
        let folder = resolve_strict(&expanduser(collection_folder)).map_err(wrap)?;
        // Resolve only the parent: the visible entry is removed, not a link target.
        let parent = source.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
        let parent = resolve_strict(&parent).map_err(wrap)?;
        let name = source.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        let suffix_ok = WALLPAPER_SUFFIXES.contains(&suffix(&name.to_string_lossy()).to_lowercase().as_str());
        if parent != folder || !suffix_ok {
            return Err(RemovalError::Error("Choose a wallpaper in the displayed folder.".into()));
        }
        let target = folder.join(&name);
        let dir = File::open(&folder).map_err(wrap)?;
        use std::os::fd::AsRawFd;
        let fd = dir.as_raw_fd();
        let cname = std::ffi::CString::new(name.as_bytes()).map_err(|_| RemovalError::Error("Choose a wallpaper in the displayed folder.".into()))?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatat(fd, cname.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(wrap(io::Error::last_os_error()));
        }
        let kind = st.st_mode & libc::S_IFMT;
        if kind != libc::S_IFREG && kind != libc::S_IFLNK {
            return Err(RemovalError::Error("Only wallpaper files can be removed.".into()));
        }
        let own: Identity = (u64::from(st.st_dev), u64::from(st.st_ino)); // 32 bits wide on some architectures
        let changed = || RemovalError::Error("This wallpaper changed. Select it again before removing it.".into());
        if expected_identity.is_some_and(|expected| expected != own) {
            return Err(changed());
        }
        check()?;
        let unlinked = if self.deny_unprivileged {
            Err(io::Error::from_raw_os_error(libc::EACCES))
        } else if unsafe { libc::unlinkat(fd, cname.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
        match unlinked {
            Ok(()) => return Ok(target),
            Err(error) if matches!(error.raw_os_error(), Some(libc::EACCES) | Some(libc::EPERM)) => {}
            Err(error) => return Err(wrap(error)),
        }
        if !self.allowed().iter().any(|p| *p == folder) {
            return Err(RemovalError::Error(format!("You do not have permission to remove wallpapers from {}.", folder.display())));
        }
        if !self.pkexec.is_file() || !self.unlink.is_file() {
            return Err(RemovalError::Error("Administrator authentication is unavailable for the system wallpaper folder.".into()));
        }
        let current = fs::metadata(&folder).map_err(wrap)?;
        let opened = dir.metadata().map_err(wrap)?;
        let latest = fs::symlink_metadata(&target).map_err(wrap)?;
        if identity(&current) != identity(&opened) || identity(&latest) != own {
            return Err(changed());
        }
        check()?;
        let output = Command::new(&self.pkexec)
            .arg("--disable-internal-agent")
            .arg(&self.unlink)
            .arg("--")
            .arg(&target)
            .stdin(Stdio::null())
            .output()
            .map_err(wrap)?;
        match output.status.code().unwrap_or(1) {
            0 => Ok(target),
            126 => Err(RemovalError::Cancelled("Administrator authentication was cancelled.".into())),
            127 => Err(RemovalError::Error("Administrator authentication was denied or unavailable. The wallpaper was not removed.".into())),
            _ => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                Err(RemovalError::Error(format!("Could not remove the wallpaper.{}", if detail.is_empty() { String::new() } else { format!(" {detail}") })))
            }
        }
    }
}

fn read_full(source: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// `import_wallpapers(sources, folder, cancelled=, validate=)` with production paths.
pub fn import_wallpapers(sources: &[PathBuf], destination: &Path, cancelled: Cancelled, validate: Validate) -> ImportOutcome {
    Importer::default().import_wallpapers(sources, destination, cancelled, validate)
}

pub fn import_wallpaper(source: &Path, destination: &Path, cancelled: Cancelled) -> Result<PathBuf, ImportError> {
    Importer::default().import_wallpaper(source, destination, cancelled)
}

pub fn remove_wallpaper(path: &Path, folder: &Path, expected_identity: Option<Identity>, cancelled: Cancelled) -> Result<PathBuf, RemovalError> {
    Importer::default().remove_wallpaper(path, folder, expected_identity, cancelled)
}

