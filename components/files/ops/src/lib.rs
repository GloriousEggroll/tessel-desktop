//! `tessel-files-ops`: the GTK-free backend of Files (tessel-files), ported from
//! the retired Python Files (`{file_operations,file_tools,file_extras,file_preferences}.py`).
//! Contract: `contracts/tessel-files.md` §3. Python class → struct of the same
//! name, function/method → same snake_case name, constants keep their names.
//!
//! # API summary
//!
//! ## [`operations`] (file_operations.py) — asynchronous, cancellable GIO jobs
//! ```ignore
//! let ops = Operations::new();                       // clone-able handle; history ≤ 20
//! let job: Job = ops.start("copy", &[src], Some(&dst_dir), Callbacks::new()
//!     .on_progress(|p: &Progress| …)                 // throttled (75 ms), main thread
//!     .on_conflict(|c: &Conflict, resolve: Resolve| resolve("rename", Some("x.txt")))
//!     .on_done(|r: &OperationResult| …));            // r.success(), r.failures, r.undo_available
//! ops.rename_many(vec![(a, b), (b2, a2)], Callbacks::new());
//! ops.undo(Callbacks::new()) -> Option<Job>;  ops.can_undo();  ops.jobs();  ops.cancel_all();
//! job.cancel(); job.running(); job.wait(Some(Duration)) -> Option<OperationResult>; job.kind();
//! ```
//! Kinds: `copy move link duplicate rename mkdir touch trash delete restore`
//! (see `OPERATIONS.md`). Each job runs on its own std thread; callbacks are
//! delivered on the GLib main context that was thread-default when the job was
//! started (the global default on the GTK thread), like `GLib.idle_add`; they
//! need not be `Send`. `Resolve` = `Arc<dyn Fn(&str, Option<&str>) + Send + Sync>`
//! (choices `skip replace rename merge cancel`; first answer wins; an unknown
//! choice fails the item with "Invalid conflict decision."; no `on_conflict` = skip).
//! Also: [`operations::as_file`], [`operations::fingerprint`], `FLAGS`, `COPY_FLAGS`, `INFO`.
//!
//! ## [`tools`] (file_tools.py) — blocking, call from a worker thread
//! `create_archive(&[paths], dest, cancel, progress) -> Result<PathBuf>` (.zip .tar .tar.gz/.tgz
//! .tar.bz2/.tbz2 .tar.xz/.txz), `extract_archive(archive, dest, cancel, progress) -> Result<Vec<PathBuf>>`,
//! `plan_batch_rename(&[paths], pattern, start) -> Result<Vec<(PathBuf, PathBuf)>>`,
//! `file_metadata(path) -> Result<FileMetadata>`, `recursive_size(&[paths], cancel) -> Result<SizeSummary>`,
//! `checksums(path, &["sha256", "sha512", "md5"], cancel) -> Result<Vec<(String, String)>>`,
//! `set_permissions(path, mode)`, `set_owner(path, Principal, Principal)`, `acl_available()`,
//! `read_acl(path) -> Result<String>`, `write_acl(path, text)`. `cancel: Option<&dyn Cancel>`
//! (implemented for `AtomicBool` and `gio::Cancellable`); `progress: Option<&mut dyn FnMut(u64, u64)>`.
//!
//! ## [`extras`] (file_extras.py) — blocking
//! `MetadataStore::new(None)` (`~/.local/share/tessel/file-metadata.json` fallback):
//! `.read(target) -> Metadata`, `.write(target, &fields_map) -> Result<Metadata>`;
//! `status_for_directory(target, timeout) -> HashMap<PathBuf, String>`;
//! `CustomActions::new(None)` (`~/.config/tessel/file-manager-actions.json`): `.list()`,
//! `CustomActions::expand(&action, &[targets]) -> Result<Vec<OsString>>`, `::invoke(.., directory)`.
//! `Target` = `gio::File` | path | string (URI or path); `From` impls for all of them.
//!
//! ## [`preferences`] (file_preferences.py)
//! `DEFAULTS` (ordered JSON map), `FOLDER_KEYS`, `valid_uri`, `validate(&Value) -> Map`,
//! `FilePreferences::new(None)` (`~/.config/tessel/tessel-files.json`): `.data`, `.get(key)`,
//! `.set(key, value)`, `.update(map)`, `.save()`, `.folder_options(uri)`, `.set_folder_options(uri, map)`;
//! `GTKBookmarks::new(None)` (`~/.config/gtk-3.0/bookmarks`): `.list() -> Vec<Bookmark>`,
//! `.add(uri, label)`, `.remove(uri)`, `.rename(uri, label)`.
//!
//! Errors are [`Error`] (Python exception class in [`ErrorKind`], `Display` = `str(exc)`).

pub mod error;
pub mod extras;
pub mod operations;
pub mod preferences;
pub mod tools;
mod fsx;
mod pyos;

pub use error::{Error, ErrorKind, Result};

use std::path::{Path, PathBuf};

/// A location argument as Python accepts it: a `Gio.File`, a path, or a string
/// (URI when it has a `scheme://` prefix, otherwise a path).
#[derive(Clone, Debug)]
pub enum Target {
    File(gio::File),
    Path(PathBuf),
    Text(String),
}

impl From<gio::File> for Target {
    fn from(file: gio::File) -> Self { Target::File(file) }
}
impl From<&gio::File> for Target {
    fn from(file: &gio::File) -> Self { Target::File(file.clone()) }
}
impl From<PathBuf> for Target {
    fn from(path: PathBuf) -> Self { Target::Path(path) }
}
impl From<&PathBuf> for Target {
    fn from(path: &PathBuf) -> Self { Target::Path(path.clone()) }
}
impl From<&Path> for Target {
    fn from(path: &Path) -> Self { Target::Path(path.to_path_buf()) }
}
impl From<&str> for Target {
    fn from(text: &str) -> Self { Target::Text(text.to_string()) }
}
impl From<String> for Target {
    fn from(text: String) -> Self { Target::Text(text) }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    /// A private fixture directory below the temp dir, removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(prefix: &str) -> Self {
            let mut random = [0u8; 6];
            crate::fsx::getrandom(&mut random);
            let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("{prefix}{}-{suffix}", std::process::id()));
            std::fs::create_dir(&path).expect("create fixture directory");
            TempDir(path)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn write(path: &Path, text: &str) {
        std::fs::write(path, text).expect("write fixture");
    }

    pub fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read fixture")
    }

    pub fn names(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path).expect("list fixture")
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
}
