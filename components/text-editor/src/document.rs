//! Lossless text decoding and optimistic, atomic file saves (document.py).
//!
//! Unknown encodings are never guessed by substituting replacement characters.
//! GIO's expected etag protects replacement; a content digest also catches tools
//! which deliberately restore the old modification time after changing a file.
//! Local paths keep symlink and atomic-create guarantees; other GIO locations
//! (GVfs SFTP, SMB, WebDAV...) use the same etag-guarded GIO replacement.
//! Everything here blocks: the window runs it on its worker thread.

use crate::codec::{Codec, BOM_UTF16_BE, BOM_UTF16_LE, BOM_UTF32_BE, BOM_UTF32_LE, BOM_UTF8};
use crate::compat;
use gio::prelude::*;
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tessel_core::pyerr::{os_error_path, os_error_text, python_error_text};

pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const ENCODINGS: [&str; 12] = ["utf-8", "utf-8-sig", "utf-16-le", "utf-16-be", "utf-32-le", "utf-32-be",
                                   "windows-1252", "iso-8859-1", "shift_jis", "gb18030", "big5", "koi8-r"];
/// `NEWLINES` (dict order).
pub const NEWLINES: [(&str, &str); 3] = [("LF", "\n"), ("CRLF", "\r\n"), ("CR", "\r")];
pub const CONFLICT: &str = "The file changed outside Text Editor. Reload it or save your changes as another file.";
pub const EXISTS: &str = "The selected file already exists.";
pub const TOO_LARGE: &str = "This file is larger than the editor's 64 MiB limit.";

pub fn newline_sequence(name: &str) -> Option<&'static str> {
    NEWLINES.iter().find(|(key, _)| *key == name).map(|(_, value)| *value)
}

/// The Python exception classes the UI distinguishes; `Display` is `str(error)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocError {
    /// `ConflictError` (an OSError subclass).
    Conflict(String),
    /// `FileExistsError`.
    Exists(String),
    /// `ValueError` (size limit, binary data, untitled, non-regular file).
    Value(String),
    /// `UnicodeDecodeError`: the UI asks for an encoding instead of showing it.
    Decode(String),
    /// `UnicodeEncodeError`.
    Encode(String),
    /// `LookupError` (unknown encoding name).
    Lookup(String),
    /// `OSError` / `GLib.Error` text.
    Os(String),
}

impl fmt::Display for DocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (DocError::Conflict(m) | DocError::Exists(m) | DocError::Value(m) | DocError::Decode(m) | DocError::Encode(m)
        | DocError::Lookup(m) | DocError::Os(m)) = self;
        f.write_str(m)
    }
}

impl From<glib::Error> for DocError {
    fn from(error: glib::Error) -> Self {
        DocError::Os(python_error_text(&error))
    }
}

/// What `location()` accepts: a command-line argument / URI, a path or a GIO file.
#[derive(Clone, Debug)]
pub enum Target {
    Arg(String),
    Path(PathBuf),
    File(gio::File),
}

impl From<&str> for Target {
    fn from(value: &str) -> Self { Target::Arg(value.to_owned()) }
}
impl From<String> for Target {
    fn from(value: String) -> Self { Target::Arg(value) }
}
impl From<PathBuf> for Target {
    fn from(value: PathBuf) -> Self { Target::Path(value) }
}
impl From<&Path> for Target {
    fn from(value: &Path) -> Self { Target::Path(value.to_owned()) }
}
impl From<gio::File> for Target {
    fn from(value: gio::File) -> Self { Target::File(value) }
}

/// `(local absolute path or None, Gio.File)` for a path, URI or GIO file.
pub fn location(target: &Target) -> (Option<PathBuf>, gio::File) {
    let file = match target {
        Target::File(file) => file.clone(),
        Target::Path(path) => gio::File::for_path(path),
        Target::Arg(value) => gio::File::for_commandline_arg(compat::expanduser(value)),
    };
    let path = file.path().map(|p| std::path::absolute(&p).unwrap_or(p));
    (path, file)
}

fn missing(error: &glib::Error) -> bool {
    error.matches(gio::IOErrorEnum::NotFound)
}

pub fn digest(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// `decode()` result: text with `\n` endings, codec name, BOM, dominant ending, mixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub text: String,
    pub encoding: String,
    pub bom: Vec<u8>,
    pub newline: String,
    pub mixed: bool,
}

pub fn decode(data: &[u8], encoding: Option<&str>) -> Result<Decoded, DocError> {
    if data.len() > MAX_BYTES {
        return Err(DocError::Value(TOO_LARGE.into()));
    }
    let mut encoding: Option<String> = match encoding.filter(|e| !e.is_empty()) {
        Some(name) => Some(Codec::lookup(name).map_err(DocError::Lookup)?.name().to_owned()),
        None => None,
    };
    let mut bom: &[u8] = b"";
    for (marker, name) in [(BOM_UTF32_LE, "utf-32-le"), (BOM_UTF32_BE, "utf-32-be"), (BOM_UTF8, "utf-8"),
                           (BOM_UTF16_LE, "utf-16-le"), (BOM_UTF16_BE, "utf-16-be")] {
        let accepted = match encoding.as_deref() {
            None => true,
            Some(current) => current == name || (name == "utf-8" && current == "utf-8-sig"),
        };
        if data.starts_with(marker) && accepted {
            bom = marker;
            encoding.get_or_insert_with(|| name.to_owned());
            break;
        }
    }
    let encoding = encoding.unwrap_or_else(|| "utf-8".into());
    let codec = Codec::lookup(&encoding).map_err(DocError::Lookup)?;
    let text = codec.decode(&data[bom.len()..]).map_err(DocError::Decode)?;
    if text.contains('\0') {
        return Err(DocError::Value("This file contains binary data and cannot be edited safely as text.".into()));
    }
    let crlf = text.matches("\r\n").count();
    let cr = text.matches('\r').count() - crlf;
    let lf = text.matches('\n').count() - crlf;
    // max() over dict order CRLF, CR, LF returns the first maximum.
    let counts = [("CRLF", crlf), ("CR", cr), ("LF", lf)];
    let ending = if counts.iter().any(|(_, n)| *n > 0) {
        let best = counts.iter().map(|(_, n)| *n).max().unwrap_or(0);
        counts.iter().find(|(_, n)| *n == best).map(|(k, _)| *k).unwrap_or("LF")
    } else {
        "LF"
    };
    let mixed = counts.iter().filter(|(_, n)| *n > 0).count() > 1;
    Ok(Decoded { text: text.replace("\r\n", "\n").replace('\r', "\n"), encoding, bom: bom.to_vec(), newline: ending.into(), mixed })
}

#[cfg(test)]
thread_local! {
    /// Replaces `os.link` (test_new_file_creation_race_never_replaces_other_writer).
    pub static LINK_HOOK: std::cell::RefCell<Option<Box<dyn Fn(&Path, &Path) -> std::io::Result<()>>>> = const { std::cell::RefCell::new(None) };
    /// Replaces `Document._create` (raced exclusive GIO create).
    pub static CREATE_HOOK: std::cell::RefCell<Option<DocError>> = const { std::cell::RefCell::new(None) };
}

fn link(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let hooked = LINK_HOOK.with(|hook| hook.borrow().as_ref().map(|f| f(source, destination)));
        if let Some(result) = hooked {
            return result;
        }
    }
    std::fs::hard_link(source, destination)
}

#[derive(Clone, Debug)]
pub struct Document {
    pub path: Option<PathBuf>,
    pub resolved: Option<PathBuf>,
    pub text: String,
    pub encoding: String,
    pub bom: Vec<u8>,
    pub newline: String,
    pub mixed: bool,
    pub raw: Arc<Vec<u8>>,
    pub etag: Option<String>,
    /// Non-native GIO location; local documents use `path` instead.
    pub remote: Option<gio::File>,
    pub stored: bool,
}

impl Default for Document {
    fn default() -> Self {
        Self { path: None, resolved: None, text: String::new(), encoding: "utf-8".into(), bom: Vec::new(), newline: "LF".into(),
               mixed: false, raw: Arc::new(Vec::new()), etag: None, remote: None, stored: false }
    }
}

impl Document {
    /// `Document(path=...)`: a named document that does not exist yet.
    pub fn with_path(path: PathBuf) -> Self {
        Self { path: Some(path), ..Self::default() }
    }

    /// `Document(remote=...)`.
    pub fn with_remote(file: gio::File) -> Self {
        Self { remote: Some(file), ..Self::default() }
    }

    pub fn file(&self) -> Option<gio::File> {
        match &self.path {
            Some(path) => Some(gio::File::for_path(path)),
            None => self.remote.clone(),
        }
    }

    pub fn untitled(&self) -> bool {
        self.path.is_none() && self.remote.is_none()
    }

    pub fn name(&self) -> Option<String> {
        if let Some(path) = &self.path {
            return Some(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        }
        self.remote.as_ref().and_then(|f| f.basename()).map(|n| n.to_string_lossy().into_owned())
    }

    /// A display name for the containing folder, or None when untitled.
    pub fn folder(&self) -> Option<String> {
        if let Some(path) = &self.path {
            return Some(path.parent().unwrap_or(path).to_string_lossy().into_owned());
        }
        let remote = self.remote.as_ref()?;
        Some(match remote.parent() {
            Some(parent) => parent.parse_name().to_string(),
            None => remote.parse_name().to_string(),
        })
    }

    /// A string for recent files and recovery; a path or a URI.
    pub fn identifier(&self) -> Option<String> {
        if let Some(path) = &self.path {
            return Some(path.to_string_lossy().into_owned());
        }
        self.remote.as_ref().map(|f| f.uri().to_string())
    }

    pub fn same_location(&self, file: &gio::File) -> bool {
        if self.untitled() {
            return false;
        }
        if let (Some(path), Some(own)) = (file.path(), &self.path) {
            let mine = self.resolved.clone().unwrap_or_else(|| compat::resolve(own));
            return compat::resolve(&path) == mine;
        }
        self.file().is_some_and(|own| own.equal(file))
    }

    pub fn load(target: &Target, encoding: Option<&str>) -> Result<Document, DocError> {
        let (path, file) = location(target);
        let Some(path) = path else { return Self::load_gio(&file, encoding) };
        let resolved = std::fs::canonicalize(&path).map_err(|e| DocError::Os(os_error_path(&e, &path)))?;
        let meta = std::fs::metadata(&resolved).map_err(|e| DocError::Os(os_error_path(&e, &resolved)))?;
        if !meta.is_file() {
            return Err(DocError::Value("Only regular local files can be opened.".into()));
        }
        if meta.len() as usize > MAX_BYTES {
            return Err(DocError::Value(TOO_LARGE.into()));
        }
        let (data, etag) = gio::File::for_path(&resolved).load_contents(None::<&gio::Cancellable>)?;
        let decoded = decode(&data, encoding)?;
        Ok(Document { path: Some(path), resolved: Some(resolved), text: decoded.text, encoding: decoded.encoding, bom: decoded.bom,
                      newline: decoded.newline, mixed: decoded.mixed, raw: Arc::new(data.to_vec()),
                      etag: etag.map(|e| e.to_string()), remote: None, stored: true })
    }

    /// Load any GIO location (GVfs may block: worker thread only).
    pub fn load_gio(file: &gio::File, encoding: Option<&str>) -> Result<Document, DocError> {
        let info = file.query_info("standard::type,standard::size", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)?;
        if !matches!(info.file_type(), gio::FileType::Regular | gio::FileType::Unknown) {
            return Err(DocError::Value("Only regular files can be opened.".into()));
        }
        if info.size() > MAX_BYTES as i64 {
            return Err(DocError::Value(TOO_LARGE.into()));
        }
        let (data, etag) = file.load_contents(None::<&gio::Cancellable>)?;
        let decoded = decode(&data, encoding)?;
        Ok(Document { path: None, resolved: None, text: decoded.text, encoding: decoded.encoding, bom: decoded.bom,
                      newline: decoded.newline, mixed: decoded.mixed, raw: Arc::new(data.to_vec()),
                      etag: etag.map(|e| e.to_string()).filter(|e| !e.is_empty()), remote: Some(file.clone()), stored: true })
    }

    /// True when the stored GIO file differs from the version we loaded.
    fn remote_changed(&self, file: &gio::File) -> Result<bool, glib::Error> {
        let info = match file.query_info("etag::value,standard::size", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>) {
            Ok(info) => info,
            Err(error) if missing(&error) => return Ok(true),
            Err(error) => return Err(error),
        };
        let current = info.etag().map(|e| e.to_string()).filter(|e| !e.is_empty());
        if let (Some(own), Some(current)) = (self.etag.as_deref().filter(|e| !e.is_empty()), current) {
            return Ok(current != own);
        }
        if info.size() != self.raw.len() as i64 {
            return Ok(true);
        }
        let (data, _) = file.load_contents(None::<&gio::Cancellable>)?;
        Ok(digest(&data) != digest(&self.raw))
    }

    pub fn changed_on_disk(&self) -> bool {
        let Some(path) = &self.path else {
            let Some(remote) = self.remote.as_ref().filter(|_| self.stored) else { return false };
            // Unreachable servers are reported when saving.
            return self.remote_changed(remote).unwrap_or(false);
        };
        let check = || -> Result<bool, ()> {
            let current = std::fs::canonicalize(path).map_err(|_| ())?;
            let resolved = self.resolved.as_ref().ok_or(())?;
            if Some(&current) != self.resolved.as_ref() {
                return Ok(true);
            }
            if std::fs::metadata(resolved).map_err(|_| ())?.len() != self.raw.len() as u64 {
                return Ok(true);
            }
            let (data, _) = gio::File::for_path(resolved).load_contents(None::<&gio::Cancellable>).map_err(|_| ())?;
            Ok(digest(&data) != digest(&self.raw))
        };
        check().unwrap_or(true)
    }

    pub fn encode(&self, text: &str, encoding: Option<&str>, newline: Option<&str>) -> Result<Vec<u8>, DocError> {
        let encoding = encoding.unwrap_or(&self.encoding);
        let newline = newline.unwrap_or(&self.newline);
        if text == self.text && encoding == self.encoding && newline == self.newline && (!self.raw.is_empty() || self.text.is_empty()) {
            return Ok(self.raw.to_vec()); // Unchanged mixed endings and BOM are byte-for-byte stable.
        }
        let sequence = newline_sequence(newline).ok_or_else(|| DocError::Value("Unknown line-ending format.".into()))?;
        let codec = Codec::lookup(encoding).map_err(DocError::Lookup)?;
        let mut data = codec.encode(&text.replace('\n', sequence)).map_err(DocError::Encode)?;
        if encoding == self.encoding && !self.bom.is_empty() && !data.starts_with(&self.bom) {
            let mut with = self.bom.clone();
            with.extend_from_slice(&data);
            data = with;
        } else if encoding != self.encoding {
            let marker: Option<&[u8]> = match encoding {
                "utf-16-le" => Some(BOM_UTF16_LE),
                "utf-16-be" => Some(BOM_UTF16_BE),
                "utf-32-le" => Some(BOM_UTF32_LE),
                "utf-32-be" => Some(BOM_UTF32_BE),
                _ => None,
            };
            if let Some(marker) = marker.filter(|m| !data.starts_with(m)) {
                let mut with = marker.to_vec();
                with.extend_from_slice(&data);
                data = with;
            }
        }
        Ok(data)
    }

    pub fn save(&mut self, text: &str, path: Option<&Target>, overwrite: bool, encoding: Option<&str>, newline: Option<&str>)
                -> Result<(), DocError> {
        let requested = match path {
            None => {
                if self.untitled() {
                    return Err(DocError::Value("Choose a file name before saving.".into()));
                }
                match &self.path {
                    None => {
                        let remote = self.remote.clone().expect("remote document");
                        return self.save_gio(text, &remote, overwrite, encoding, newline);
                    }
                    Some(path) => path.clone(),
                }
            }
            Some(target) => match location(target) {
                (None, file) => return self.save_gio(text, &file, overwrite, encoding, newline),
                (Some(path), _) => path,
            },
        };
        let destination = compat::resolve(&requested);
        let same = self.resolved.as_ref() == Some(&destination);
        let current_name = self.resolved.is_some() && self.path.as_ref() == Some(&requested);
        if (same || current_name) && self.changed_on_disk() {
            return Err(DocError::Conflict(CONFLICT.into()));
        }
        let existed = destination.exists();
        if !same && existed && !overwrite {
            return Err(DocError::Exists(EXISTS.into()));
        }
        let data = self.encode(text, encoding, newline)?;
        if data.len() > MAX_BYTES {
            return Err(DocError::Value(TOO_LARGE.into()));
        }
        if text.contains('\0') {
            return Err(DocError::Value("Binary null characters cannot be saved as a text document.".into()));
        }
        let file = gio::File::for_path(&destination);
        let new_etag;
        if existed {
            let etag = if same {
                self.etag.clone()
            } else {
                file.query_info("etag::value", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)?.etag().map(|e| e.to_string())
            };
            new_etag = file.replace_contents(&data, etag.as_deref(), false, gio::FileCreateFlags::NONE, None::<&gio::Cancellable>)?
                .map(|e| e.to_string());
        } else {
            // Atomically publish a complete new file without a check/create
            // race: link() fails if another writer creates the name meanwhile.
            // The temporary file lives beside the destination, on the same FS.
            let parent = destination.parent().unwrap_or(Path::new("/"));
            let (temporary, mut stream) = compat::named_temporary(parent, ".text-editor-", ".tmp")?;
            let result = (|| -> Result<bool, DocError> {
                stream.write_all(&data).and_then(|_| stream.flush()).and_then(|_| stream.sync_all())
                    .map_err(|e| DocError::Os(os_error_text(&e)))?;
                drop(stream);
                match link(&temporary, &destination) {
                    Ok(()) => Ok(true),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(DocError::Exists(format!(
                        "{}: '{}' -> '{}'", os_error_text(&error), temporary.display(), destination.display()))),
                    // GVfs FUSE and some network filesystems have no hard links.
                    Err(_) => Ok(false),
                }
            })();
            let _ = std::fs::remove_file(&temporary);
            if !result? {
                Self::create(&file, &data)?;
            }
            new_etag = file.query_info("etag::value", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)?.etag().map(|e| e.to_string());
        }
        self.path = Some(requested);
        self.resolved = Some(destination);
        self.text = text.to_owned();
        self.remote = None;
        self.saved(data, new_etag, encoding, newline);
        Ok(())
    }

    /// Create a new file exclusively; an existing name is never replaced.
    fn create(file: &gio::File, data: &[u8]) -> Result<(), DocError> {
        #[cfg(test)]
        if let Some(error) = CREATE_HOOK.with(|hook| hook.borrow().clone()) {
            return Err(error);
        }
        let stream = match file.create(gio::FileCreateFlags::NONE, None::<&gio::Cancellable>) {
            Ok(stream) => stream,
            Err(error) if error.matches(gio::IOErrorEnum::Exists) => return Err(DocError::Exists(EXISTS.into())),
            Err(error) => return Err(error.into()),
        };
        let written = stream.write_all(data, None::<&gio::Cancellable>);
        let closed = stream.close(None::<&gio::Cancellable>);
        written?;
        closed?;
        Ok(())
    }

    fn saved(&mut self, data: Vec<u8>, etag: Option<String>, encoding: Option<&str>, newline: Option<&str>) {
        if let Some(encoding) = encoding {
            self.encoding = encoding.to_owned();
        }
        if let Some(newline) = newline {
            self.newline = newline.to_owned();
        }
        self.bom = [BOM_UTF32_LE, BOM_UTF32_BE, BOM_UTF8, BOM_UTF16_LE, BOM_UTF16_BE].into_iter()
            .find(|marker| data.starts_with(marker)).map(<[u8]>::to_vec).unwrap_or_default();
        self.mixed = decode(&data, Some(&self.encoding)).map(|d| d.mixed).unwrap_or(false);
        self.raw = Arc::new(data);
        self.etag = etag;
        self.stored = true;
    }

    /// Save to any GIO location with an expected-etag replacement.
    pub fn save_gio(&mut self, text: &str, file: &gio::File, overwrite: bool, encoding: Option<&str>, newline: Option<&str>)
                    -> Result<(), DocError> {
        let same = self.stored && self.file().is_some_and(|own| own.equal(file));
        let (info, existed) = match file.query_info("etag::value", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>) {
            Ok(info) => (Some(info), true),
            Err(error) if missing(&error) => (None, false),
            Err(error) => return Err(error.into()),
        };
        if same && (!existed || self.remote_changed(file)?) {
            return Err(DocError::Conflict(CONFLICT.into()));
        }
        if !same && existed && !overwrite {
            return Err(DocError::Exists(EXISTS.into()));
        }
        let data = self.encode(text, encoding, newline)?;
        if data.len() > MAX_BYTES {
            return Err(DocError::Value(TOO_LARGE.into()));
        }
        if text.contains('\0') {
            return Err(DocError::Value("Binary null characters cannot be saved as a text document.".into()));
        }
        let new_etag = if existed {
            let expected = if same { self.etag.clone() } else { info.and_then(|i| i.etag()).map(|e| e.to_string()) }
                .filter(|e| !e.is_empty());
            match file.replace_contents(&data, expected.as_deref(), false, gio::FileCreateFlags::NONE, None::<&gio::Cancellable>) {
                Ok(etag) => etag.map(|e| e.to_string()),
                Err(error) if error.matches(gio::IOErrorEnum::WrongEtag) => return Err(DocError::Conflict(CONFLICT.into())),
                Err(error) => return Err(error.into()),
            }
        } else {
            Self::create(file, &data)?;
            file.query_info("etag::value", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>).ok()
                .and_then(|i| i.etag()).map(|e| e.to_string())
        };
        self.path = None;
        self.resolved = None;
        self.remote = Some(file.clone());
        self.text = text.to_owned();
        self.saved(data, new_etag.filter(|e| !e.is_empty()), encoding, newline);
        Ok(())
    }
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
