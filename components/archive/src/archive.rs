//! Archive data model shared by every backend: entries, listings, errors,
//! the job context (password, cancellation, progress) and creation options.

use tessel_archive_settings::plugins::Kind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// Full path inside the archive, '/'-separated, no leading or trailing '/'.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub compressed: Option<u64>,
    /// Seconds since the epoch.
    pub mtime: Option<i64>,
    /// Unix permission bits (with file type bits stripped).
    pub mode: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub crc: Option<String>,
    pub method: Option<String>,
    pub version: Option<String>,
    pub encrypted: bool,
    pub link: Option<String>,
}

impl Entry {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
    pub fn parent(&self) -> &str {
        self.path.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
    }
    pub fn ratio(&self) -> Option<f64> {
        match self.compressed {
            Some(c) if self.size > 0 && !self.is_dir => Some(c as f64 / self.size as f64),
            _ => None,
        }
    }
}

/// Normalize a path stored in an archive: '/' separators, no "./" prefix,
/// no leading or trailing '/'. Returns None for paths escaping the root.
pub fn clean_path(raw: &str) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub comment: Option<String>,
    /// Some entries are encrypted.
    pub encrypted: bool,
    /// The file list itself is encrypted (7z -mhe, rar -hp).
    pub header_encrypted: bool,
    /// Number of volumes (1 for ordinary archives).
    pub volumes: u32,
    /// Main compression method, if the backend reports one.
    pub method: Option<String>,
    /// Columns this backend supplied (so the UI hides the others).
    pub columns: Columns,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Columns {
    pub compressed: bool,
    pub permissions: bool,
    pub owner: bool,
    pub crc: bool,
    pub method: bool,
    pub version: bool,
}

impl Listing {
    /// Add the folders implied by entry paths that have no entry of their own.
    pub fn add_implied_folders(&mut self) {
        let mut known: std::collections::HashSet<String> = self.entries.iter().map(|e| e.path.clone()).collect();
        let mut extra = Vec::new();
        for entry in &self.entries {
            let mut path = entry.parent().to_string();
            while !path.is_empty() && known.insert(path.clone()) {
                extra.push(Entry { path: path.clone(), is_dir: true, ..Default::default() });
                path = path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
            }
        }
        self.entries.extend(extra);
    }

    /// Keep the last entry of each path (archives may contain duplicates).
    pub fn dedup(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let mut keep: Vec<Entry> = Vec::with_capacity(self.entries.len());
        for entry in self.entries.drain(..).rev() {
            if seen.insert(entry.path.clone()) {
                keep.push(entry);
            }
        }
        keep.reverse();
        self.entries = keep;
    }

    pub fn finish(mut self) -> Self {
        self.dedup();
        self.add_implied_folders();
        self.encrypted |= self.entries.iter().any(|e| e.encrypted);
        self
    }

    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// The paths of `selection` and of every entry below them.
    pub fn expand(&self, selection: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for entry in &self.entries {
            if selection.iter().any(|s| entry.path == *s || entry.path.starts_with(&format!("{s}/"))) {
                out.push(entry.path.clone());
            }
        }
        out
    }

    pub fn top_level(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| !e.path.contains('/')).collect()
    }

    pub fn files(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_dir).count()
    }

    pub fn folders(&self) -> usize {
        self.entries.iter().filter(|e| e.is_dir).count()
    }

    pub fn total_size(&self) -> u64 {
        self.entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// A password is required (none given).
    PasswordNeeded,
    /// The given password is wrong.
    WrongPassword,
    Cancelled,
    /// This operation is not available for the format/backend.
    Unsupported(String),
    /// No backend can open the file.
    NoBackend(String),
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use tessel_core::tr;
        match self {
            Error::PasswordNeeded => write!(f, "{}", tr!("This archive is protected by a password.")),
            Error::WrongPassword => write!(f, "{}", tr!("The password is wrong.")),
            Error::Cancelled => write!(f, "{}", tr!("The operation was cancelled.")),
            Error::Unsupported(what) | Error::NoBackend(what) | Error::Failed(what) => write!(f, "{what}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Failed(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Progress callback: (fraction 0..1 or negative when unknown, current item).
pub type Progress = Arc<dyn Fn(f64, &str) + Send + Sync>;

#[derive(Clone)]
pub struct Ctx {
    pub password: Option<String>,
    pub cancel: Arc<AtomicBool>,
    pub progress: Progress,
}

impl Ctx {
    pub fn new() -> Self {
        Ctx { password: None, cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(|_, _| {}) }
    }
    pub fn with_password(mut self, password: Option<String>) -> Self {
        self.password = password.filter(|p| !p.is_empty());
        self
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn check(&self) -> Result<()> {
        if self.cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    pub fn report(&self, fraction: f64, item: &str) {
        (self.progress)(fraction, item)
    }
}

impl Default for Ctx {
    fn default() -> Self {
        Self::new()
    }
}

/// Options of a new archive / of added files (Ark's create dialog).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreateOptions {
    /// 0..9, None = backend default.
    pub level: Option<u32>,
    /// Backend method name (e.g. "LZMA2", "Deflate", "BZip2").
    pub method: Option<String>,
    pub password: Option<String>,
    pub encrypt_header: bool,
    /// zip: "AES256" or "ZipCrypto".
    pub encryption: Option<String>,
    /// Split volume size in bytes.
    pub volume_size: Option<u64>,
    pub comment: Option<String>,
}

/// One file to add: a file on disk and its path in the archive.
#[derive(Clone, Debug, PartialEq)]
pub struct AddItem {
    pub source: PathBuf,
    pub dest: String,
}

/// Methods offered per format in the create dialog, default first.
pub fn methods(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::SevenZip => &["LZMA2", "LZMA", "PPMd", "BZip2", "Deflate", "Copy"],
        Kind::Zip => &["Deflate", "Deflate64", "BZip2", "LZMA", "PPMd", "Copy"],
        _ => &[],
    }
}

pub fn supports_password(kind: Kind) -> bool {
    matches!(kind, Kind::SevenZip | Kind::Zip)
}

pub fn supports_header_encryption(kind: Kind) -> bool {
    kind == Kind::SevenZip
}

pub fn supports_volumes(kind: Kind) -> bool {
    matches!(kind, Kind::SevenZip | Kind::Zip)
}

pub fn supports_comment(kind: Kind) -> bool {
    kind == Kind::Zip
}

pub fn supports_level(kind: Kind) -> bool {
    !matches!(kind, Kind::Tar | Kind::Lrz | Kind::TarLrz | Kind::Z | Kind::TarZ)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_paths() {
        assert_eq!(clean_path("./a//b/").as_deref(), Some("a/b"));
        assert_eq!(clean_path("/abs/x").as_deref(), Some("abs/x"));
        assert_eq!(clean_path("a\\b").as_deref(), Some("a/b"));
        assert_eq!(clean_path("../evil"), None);
        assert_eq!(clean_path("a/../../evil"), None);
        assert_eq!(clean_path("./"), None);
    }

    #[test]
    fn implied_folders_and_dedup() {
        let mut listing = Listing {
            entries: vec![Entry { path: "a/b/c.txt".into(), size: 3, ..Default::default() },
                          Entry { path: "x".into(), size: 1, ..Default::default() },
                          Entry { path: "x".into(), size: 2, ..Default::default() }],
            ..Default::default()
        }.finish();
        listing.entries.sort_by(|a, b| a.path.cmp(&b.path));
        let paths: Vec<_> = listing.entries.iter().map(|e| (e.path.as_str(), e.is_dir)).collect();
        assert_eq!(paths, [("a", true), ("a/b", true), ("a/b/c.txt", false), ("x", false)]);
        assert_eq!(listing.find("x").unwrap().size, 2);
        assert_eq!(listing.expand(&["a".into()]), ["a", "a/b", "a/b/c.txt"]);
        assert_eq!(listing.files(), 2);
        assert_eq!(listing.folders(), 2);
    }

    #[test]
    fn entry_helpers() {
        let e = Entry { path: "dir/file.txt".into(), size: 200, compressed: Some(50), ..Default::default() };
        assert_eq!(e.name(), "file.txt");
        assert_eq!(e.parent(), "dir");
        assert_eq!(e.ratio(), Some(0.25));
    }
}
