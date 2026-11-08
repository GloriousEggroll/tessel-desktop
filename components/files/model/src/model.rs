//! `file_model.py`: GIO file models, cancellable loading, and presentation helpers.
//!
//! Directory loading uses GIO's asynchronous API. Recursive searches perform
//! their I/O on a cancellable worker thread and deliver bounded batches on the
//! main context that was thread-default when `start` was called. Both accept
//! `gio::File` locations, including GVfs remote and virtual locations.

use std::cell::{Cell, OnceCell, RefCell};
use std::cmp::Ordering;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};

use gio::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::ToGlibPtr;

use crate::util::quote;

/// `FILE_ATTRIBUTES`.
pub const FILE_ATTRIBUTES: &str = "standard::*,time::modified,time::modified-usec,time::modified-nsec,\
access::can-read,access::can-write,access::can-delete,access::can-trash,access::can-rename,\
thumbnail::*,unix::mode,unix::uid,unix::gid,owner::*,trash::orig-path,trash::deletion-date";

/// `_QUERY_FLAGS`: resolve metadata for links so a link to a folder opens in
/// the current pane. Recursive search separately checks `is_symlink`.
pub const QUERY_FLAGS: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NONE;

/// `_DESKTOP_ENTRY_LIMIT`.
pub const DESKTOP_ENTRY_LIMIT: usize = 256 * 1024;

/// `archive_uri(file_or_uri)` for a `gio::File`: GVfs's canonical archive
/// root URI (two escaping layers) without touching the file.
pub fn archive_uri_for_file(file: &gio::File) -> String {
    format!("archive://{}/", quote(&quote(&file.uri(), ""), ""))
}

/// `archive_uri(path_or_uri)`: `~` is expanded, the text is resolved like a
/// command-line argument. `Err("Choose an archive file")` for an empty value.
pub fn archive_uri(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("Choose an archive file".into());
    }
    let file = gio::File::for_commandline_arg(crate::util::expanduser(value));
    Ok(archive_uri_for_file(&file))
}

// ---- FileItem ---------------------------------------------------------------

/// The immutable part of a [`FileItem`] (everything but the icon).
#[derive(Debug)]
pub struct FileItemData {
    pub file: gio::File,
    pub info: gio::FileInfo,
    pub name: String,
    pub display_name: String,
    pub uri: String,
    pub path: Option<PathBuf>,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_hidden: bool,
    pub size: u64,
    pub modified: u64,
    pub content_type: String,
    pub readable: bool,
    pub writeable: bool,
    pub deletable: bool,
    pub trashable: bool,
    pub renamable: bool,
    pub thumbnail_path: Option<String>,
    pub thumbnail_valid: bool,
    pub owner: String,
    pub group: String,
    pub mode: u32,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileItem {
        pub data: OnceCell<FileItemData>,
        pub icon: RefCell<Option<gio::Icon>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileItem {
        const NAME: &'static str = "TesselFilesFileItem";
        type Type = super::FileItem;
    }

    impl ObjectImpl for FileItem {}
}

glib::wrapper! {
    /// An enumerated file usable directly in a `gio::ListStore`.
    ///
    /// File identity is the URI, never the display name. `path` is `None` for
    /// remote files. Permissions default to false if a backend cannot report them.
    pub struct FileItem(ObjectSubclass<imp::FileItem>);
}

/// `info.get_attribute_byte_string(attribute)` (lossy for non-UTF-8 bytes).
pub fn attribute_byte_string(info: &gio::FileInfo, attribute: &str) -> Option<String> {
    let attribute = CString::new(attribute).ok()?;
    // SAFETY: the returned string is owned by the info and copied at once.
    unsafe {
        let value = gio::ffi::g_file_info_get_attribute_byte_string(info.to_glib_none().0, attribute.as_ptr());
        (!value.is_null()).then(|| CStr::from_ptr(value).to_string_lossy().into_owned())
    }
}

fn non_empty(value: Option<glib::GString>) -> Option<String> {
    value.map(|v| v.to_string()).filter(|v| !v.is_empty())
}

impl FileItem {
    /// `FileItem(file, info)`.
    pub fn new(file: &gio::File, info: &gio::FileInfo) -> Self {
        let name = attribute_byte_string(info, "standard::name")
            .filter(|n| !n.is_empty())
            .or_else(|| file.basename().map(|b| b.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let display_name = non_empty(info.attribute_string("standard::display-name")).unwrap_or_else(|| name.clone());
        let is_dir = info.attribute_uint32("standard::type") == gio::FileType::Directory.into_glib_u32();
        let is_hidden = info.boolean("standard::is-hidden") || name.starts_with('.');
        let icon = info
            .attribute_object("standard::icon")
            .and_then(|o| o.downcast::<gio::Icon>().ok())
            .unwrap_or_else(|| gio::ThemedIcon::new(if is_dir { "folder" } else { "text-x-generic" }).upcast());
        let data = FileItemData {
            file: file.clone(),
            info: info.clone(),
            display_name,
            uri: file.uri().to_string(),
            path: file.path(),
            is_dir,
            is_symlink: info.boolean("standard::is-symlink"),
            is_hidden,
            size: info.attribute_uint64("standard::size"),
            modified: info.attribute_uint64("time::modified"),
            content_type: non_empty(info.attribute_string("standard::content-type"))
                .unwrap_or_else(|| "application/octet-stream".into()),
            readable: info.boolean("access::can-read"),
            writeable: info.boolean("access::can-write"),
            deletable: info.boolean("access::can-delete"),
            trashable: info.boolean("access::can-trash"),
            renamable: info.boolean("access::can-rename"),
            thumbnail_path: attribute_byte_string(info, "thumbnail::path"),
            thumbnail_valid: info.boolean("thumbnail::is-valid"),
            owner: info.attribute_string("owner::user").map(|s| s.to_string()).unwrap_or_default(),
            group: info.attribute_string("owner::group").map(|s| s.to_string()).unwrap_or_default(),
            mode: info.attribute_uint32("unix::mode"),
            name,
        };
        let item: FileItem = glib::Object::new();
        let _ = item.imp().data.set(data);
        item.imp().icon.replace(Some(icon));
        item
    }

    /// `FileItem.query(file, cancellable)`: synchronous convenience for
    /// worker code and small local fixtures (also enriches launcher icons).
    pub fn query(file: &gio::File, cancellable: Option<&gio::Cancellable>) -> Result<Self, glib::Error> {
        let info = file.query_info(FILE_ATTRIBUTES, QUERY_FLAGS, cancellable)?;
        let item = Self::new(file, &info);
        load_desktop_icon(&item, cancellable);
        Ok(item)
    }

    pub fn data(&self) -> &FileItemData {
        self.imp().data.get().expect("FileItem constructed with FileItem::new")
    }
    pub fn file(&self) -> gio::File {
        self.data().file.clone()
    }
    pub fn info(&self) -> gio::FileInfo {
        self.data().info.clone()
    }
    pub fn name(&self) -> String {
        self.data().name.clone()
    }
    pub fn display_name(&self) -> String {
        self.data().display_name.clone()
    }
    pub fn uri(&self) -> String {
        self.data().uri.clone()
    }
    pub fn path(&self) -> Option<PathBuf> {
        self.data().path.clone()
    }
    pub fn is_dir(&self) -> bool {
        self.data().is_dir
    }
    pub fn is_symlink(&self) -> bool {
        self.data().is_symlink
    }
    pub fn is_hidden(&self) -> bool {
        self.data().is_hidden
    }
    pub fn size(&self) -> u64 {
        self.data().size
    }
    pub fn modified(&self) -> u64 {
        self.data().modified
    }
    pub fn content_type(&self) -> String {
        self.data().content_type.clone()
    }
    pub fn icon(&self) -> gio::Icon {
        self.imp().icon.borrow().clone().expect("FileItem icon")
    }
    /// `item.icon = icon`.
    pub fn set_icon(&self, icon: &gio::Icon) {
        self.imp().icon.replace(Some(icon.clone()));
    }
    pub fn readable(&self) -> bool {
        self.data().readable
    }
    pub fn writeable(&self) -> bool {
        self.data().writeable
    }
    /// Alias of [`Self::writeable`] (Python has both).
    pub fn writable(&self) -> bool {
        self.data().writeable
    }
    pub fn deletable(&self) -> bool {
        self.data().deletable
    }
    pub fn trashable(&self) -> bool {
        self.data().trashable
    }
    pub fn renamable(&self) -> bool {
        self.data().renamable
    }
    pub fn thumbnail_path(&self) -> Option<String> {
        self.data().thumbnail_path.clone()
    }
    pub fn thumbnail_valid(&self) -> bool {
        self.data().thumbnail_valid
    }
    pub fn owner(&self) -> String {
        self.data().owner.clone()
    }
    pub fn group(&self) -> String {
        self.data().group.clone()
    }
    pub fn mode(&self) -> u32 {
        self.data().mode
    }
    /// `"Folder"` for directories, else `Gio.content_type_get_description`.
    pub fn type_description(&self) -> String {
        if self.is_dir() {
            "Folder".into()
        } else {
            gio::content_type_get_description(&self.data().content_type).to_string()
        }
    }
}

trait FileTypeU32 {
    fn into_glib_u32(self) -> u32;
}
impl FileTypeU32 for gio::FileType {
    fn into_glib_u32(self) -> u32 {
        glib::translate::IntoGlib::into_glib(self) as u32
    }
}

// ---- Launcher icons ---------------------------------------------------------

/// A parsed launcher icon (thread-safe; turned into a `gio::Icon` on the
/// item's thread).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopIcon {
    Themed(String),
    File(PathBuf),
}

fn desktop_entry_parts(path: Option<&Path>, name: &str, file_type: gio::FileType, size: u64) -> bool {
    path.is_some() && name.ends_with(".desktop") && file_type == gio::FileType::Regular && size > 0 && size <= DESKTOP_ENTRY_LIMIT as u64
}

/// `_is_desktop_entry(item)`: never read arbitrary remote files while merely
/// displaying a listing.
pub fn is_desktop_entry(item: &FileItem) -> bool {
    let data = item.data();
    desktop_entry_parts(data.path.as_deref(), &data.name, data.info.file_type(), data.size)
}

/// `_desktop_icon(data)`: presentation metadata only; Exec is never evaluated.
pub fn desktop_icon(data: &[u8]) -> Option<DesktopIcon> {
    let text = std::str::from_utf8(data).ok()?;
    let keyfile = glib::KeyFile::new();
    keyfile.load_from_data(text, glib::KeyFileFlags::NONE).ok()?;
    let kind = keyfile.string("Desktop Entry", "Type").ok()?;
    if !matches!(kind.as_str(), "Application" | "Link" | "Directory") {
        return None;
    }
    let raw = keyfile.string("Desktop Entry", "Icon").ok()?;
    let name = tessel_core::pyjson::py_strip(raw.as_str());
    if name.is_empty() || name.chars().any(|c| (c as u32) < 32) {
        return None;
    }
    if name.starts_with('/') {
        // Missing file icons retain the normal content-type icon.
        return Path::new(name).is_file().then(|| DesktopIcon::File(PathBuf::from(name)));
    }
    if !name.contains(['/', '\\', ':']) {
        return Some(DesktopIcon::Themed(name.to_owned()));
    }
    None
}

#[cfg(test)]
pub(crate) type DesktopHook = Arc<dyn Fn(&Path) + Send + Sync>;
#[cfg(test)]
pub(crate) static DESKTOP_HOOKS: std::sync::Mutex<Vec<(PathBuf, DesktopHook)>> = std::sync::Mutex::new(Vec::new());

/// The read part of `_load_desktop_icon` (any thread): bounded reads, `None`
/// on any error or cancellation.
pub fn read_desktop_icon(file: &gio::File, path: &Path, cancellable: Option<&gio::Cancellable>) -> Option<DesktopIcon> {
    #[cfg(test)]
    {
        let hooks: Vec<DesktopHook> =
            DESKTOP_HOOKS.lock().unwrap().iter().filter(|(p, _)| path.starts_with(p)).map(|(_, h)| h.clone()).collect();
        for hook in hooks {
            hook(path);
        }
    }
    let _ = path;
    if cancellable.is_some_and(|c| c.is_cancelled()) {
        return None;
    }
    let stream = file.read(cancellable).ok()?;
    let mut contents: Vec<u8> = Vec::new();
    let mut failed = false;
    // Bound actual reads as well as the enumerated size: a file can grow.
    while contents.len() <= DESKTOP_ENTRY_LIMIT {
        let want = 65536.min(DESKTOP_ENTRY_LIMIT + 1 - contents.len());
        match stream.read_bytes(want, cancellable) {
            Ok(part) if part.is_empty() => break,
            Ok(part) => contents.extend_from_slice(&part),
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    let _ = stream.close(None::<&gio::Cancellable>);
    if failed || contents.len() > DESKTOP_ENTRY_LIMIT {
        return None;
    }
    desktop_icon(&contents)
}

/// Apply a parsed launcher icon: a themed launcher icon keeps the regular
/// file icon's names as GTK fallbacks.
pub fn apply_desktop_icon(item: &FileItem, icon: &DesktopIcon) {
    let new_icon: gio::Icon = match icon {
        DesktopIcon::File(path) => gio::FileIcon::new(&gio::File::for_path(path)).upcast(),
        DesktopIcon::Themed(name) => {
            let themed = gio::ThemedIcon::new(name);
            if let Ok(current) = item.icon().downcast::<gio::ThemedIcon>() {
                for name in current.names() {
                    themed.append_name(&name);
                }
            }
            themed.upcast()
        }
    };
    item.set_icon(&new_icon);
}

/// `_load_desktop_icon(item, cancellable)`: synchronous launcher-icon
/// enrichment (call on workers or for small fixtures, never during bind).
pub fn load_desktop_icon(item: &FileItem, cancellable: Option<&gio::Cancellable>) {
    if !is_desktop_entry(item) || cancellable.is_some_and(|c| c.is_cancelled()) {
        return;
    }
    let data = item.data();
    if let Some(icon) = read_desktop_icon(&data.file, data.path.as_deref().unwrap_or(Path::new("")), cancellable) {
        apply_desktop_icon(item, &icon);
    }
}

/// `load_desktop_icons_async(items, cancellable, on_done)`: enrich one batch
/// on GIO's worker pool. Without launchers `on_done` runs synchronously;
/// otherwise it always runs later on the caller's thread-default main
/// context (also after cancellation).
pub fn load_desktop_icons_async(
    items: Vec<FileItem>,
    cancellable: Option<&gio::Cancellable>,
    on_done: impl FnOnce(Vec<FileItem>) + 'static,
) {
    let jobs: Vec<(usize, gio::File, PathBuf)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| is_desktop_entry(item))
        .map(|(index, item)| (index, item.file(), item.path().unwrap_or_default()))
        .collect();
    if jobs.is_empty() {
        on_done(items);
        return;
    }
    let cancel = cancellable.cloned();
    let context = glib::MainContext::ref_thread_default();
    context.spawn_local(async move {
        let results = gio::spawn_blocking(move || {
            let mut results = Vec::new();
            for (index, file, path) in jobs {
                if cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                    break;
                }
                results.push((index, read_desktop_icon(&file, &path, cancel.as_ref())));
            }
            results
        })
        .await
        .unwrap_or_default();
        for (index, icon) in results {
            if let Some(icon) = icon {
                apply_desktop_icon(&items[index], &icon);
            }
        }
        on_done(items);
    });
}

// ---- Metadata and reconciliation -------------------------------------------

/// Identity of an item's icon for [`file_item_metadata`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IconKey {
    Text(String),
    /// Opaque, nonserializable icons compare by object identity.
    Opaque(usize),
}

/// The tuple returned by `file_item_metadata(item)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileItemMetadata {
    pub name: String,
    pub display_name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_hidden: bool,
    pub file_type: u32,
    pub size: u64,
    pub modified: u64,
    pub modified_usec: u32,
    pub modified_nsec: u32,
    pub content_type: String,
    pub icon: Option<IconKey>,
    pub readable: bool,
    pub writeable: bool,
    pub deletable: bool,
    pub trashable: bool,
    pub renamable: bool,
    pub mode: u32,
    pub owner: String,
    pub group: String,
    pub thumbnail_path: Option<String>,
    pub thumbnail_valid: bool,
    pub symlink_target: Option<String>,
    pub trash_orig_path: Option<String>,
    pub trash_deletion_date: Option<String>,
}

fn icon_key(icon: &gio::Icon) -> IconKey {
    if let Some(text) = IconExt::to_string(icon) {
        return IconKey::Text(text.to_string());
    }
    match icon.serialize() {
        Some(variant) => IconKey::Text(variant.print(true).to_string()),
        None => IconKey::Opaque(icon.as_ptr() as usize),
    }
}

/// `file_item_metadata(item)`: comparable cached metadata that affects file
/// views or actions; no filesystem queries. Access time is excluded;
/// subsecond modification times are included.
pub fn file_item_metadata(item: &FileItem) -> FileItemMetadata {
    let data = item.data();
    let info = &data.info;
    FileItemMetadata {
        name: data.name.clone(),
        display_name: data.display_name.clone(),
        is_dir: data.is_dir,
        is_symlink: data.is_symlink,
        is_hidden: data.is_hidden,
        file_type: info.attribute_uint32("standard::type"),
        size: data.size,
        modified: data.modified,
        modified_usec: info.attribute_uint32("time::modified-usec"),
        modified_nsec: info.attribute_uint32("time::modified-nsec"),
        content_type: data.content_type.clone(),
        icon: Some(icon_key(&item.icon())),
        readable: data.readable,
        writeable: data.writeable,
        deletable: data.deletable,
        trashable: data.trashable,
        renamable: data.renamable,
        mode: data.mode,
        owner: data.owner.clone(),
        group: data.group.clone(),
        thumbnail_path: data.thumbnail_path.clone(),
        thumbnail_valid: data.thumbnail_valid,
        symlink_target: attribute_byte_string(info, "standard::symlink-target"),
        trash_orig_path: attribute_byte_string(info, "trash::orig-path"),
        trash_deletion_date: info.attribute_string("trash::deletion-date").map(|s| s.to_string()),
    }
}

/// `reconcile_items(store, incoming, remove_missing=…)`: update a
/// `gio::ListStore` of [`FileItem`]s without clearing unchanged objects.
/// Existing order is kept, new URIs are appended (last snapshot of a repeated
/// URI wins). Returns whether anything was inserted, removed or replaced.
pub fn reconcile_items(store: &gio::ListStore, incoming: &[FileItem], remove_missing: bool) -> bool {
    // dict semantics: key order of first occurrence, value of the last.
    let mut order: Vec<String> = Vec::new();
    let mut pending: std::collections::HashMap<String, FileItem> = std::collections::HashMap::new();
    for item in incoming {
        let uri = item.uri();
        if pending.insert(uri.clone(), item.clone()).is_none() {
            order.push(uri);
        }
    }
    let mut changed = false;
    for index in (0..store.n_items()).rev() {
        let Some(previous) = store.item(index).and_then(|o| o.downcast::<FileItem>().ok()) else { continue };
        match pending.remove(&previous.uri()) {
            None => {
                if remove_missing {
                    store.remove(index);
                    changed = true;
                }
            }
            Some(replacement) => {
                if file_item_metadata(&previous) != file_item_metadata(&replacement) {
                    store.splice(index, 1, &[replacement]);
                    changed = true;
                }
            }
        }
    }
    let additions: Vec<FileItem> = order.into_iter().filter_map(|uri| pending.remove(&uri)).collect();
    if !additions.is_empty() {
        store.splice(store.n_items(), 0, &additions);
        changed = true;
    }
    changed
}

// ---- Sorting and matching ---------------------------------------------------

/// One element of [`natural_key`]: `(0, text)` sorts before `(1, int)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NaturalPart {
    Text(String),
    /// Decimal value as normalized ASCII digits (no leading zeros).
    Number(String),
}

impl Ord for NaturalPart {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (NaturalPart::Text(a), NaturalPart::Text(b)) => a.cmp(b),
            (NaturalPart::Number(a), NaturalPart::Number(b)) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            (NaturalPart::Text(_), NaturalPart::Number(_)) => Ordering::Less,
            (NaturalPart::Number(_), NaturalPart::Text(_)) => Ordering::Greater,
        }
    }
}
impl PartialOrd for NaturalPart {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn digits_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\d+").expect("digits"))
}

fn is_unicode_digit(c: char) -> bool {
    let mut buffer = [0u8; 4];
    digits_regex().is_match(c.encode_utf8(&mut buffer))
}

/// Decimal value of a Unicode `Nd` character: digit runs are contiguous
/// blocks of ten starting at zero.
fn digit_value(c: char) -> u32 {
    if let Some(value) = c.to_digit(10) {
        return value;
    }
    let code = c as u32;
    let mut back = 0;
    while back < code && char::from_u32(code - back - 1).is_some_and(is_unicode_digit) {
        back += 1;
    }
    back % 10
}

/// `natural_key(value)`: case-insensitive natural order without mixing types.
pub fn natural_key(value: &str) -> Vec<NaturalPart> {
    let folded = tessel_core::pyjson::casefold(value);
    let mut parts = Vec::new();
    let mut last = 0;
    for found in digits_regex().find_iter(&folded) {
        parts.push(NaturalPart::Text(folded[last..found.start()].to_owned()));
        let digits: String = found.as_str().chars().map(|c| char::from_digit(digit_value(c), 10).unwrap_or('0')).collect();
        let trimmed = digits.trim_start_matches('0');
        parts.push(NaturalPart::Number(if trimmed.is_empty() { "0".into() } else { trimmed.into() }));
        last = found.end();
    }
    parts.push(NaturalPart::Text(folded[last..].to_owned()));
    parts
}

fn text_cmp(a: &str, b: &str, natural: bool) -> Ordering {
    if natural {
        natural_key(a).cmp(&natural_key(b))
    } else {
        tessel_core::pyjson::casefold(a).cmp(&tessel_core::pyjson::casefold(b))
    }
}

/// `compare_items(left, right, key, descending, folders_first, natural_sort)`.
/// `key` is `"name"` (default for anything unknown), `"size"`, `"modified"`
/// or `"type"`. Folders-first is applied before `descending`.
pub fn compare_items(left: &FileItem, right: &FileItem, key: &str, descending: bool, folders_first: bool, natural_sort: bool) -> Ordering {
    if folders_first && left.is_dir() != right.is_dir() {
        return if left.is_dir() { Ordering::Less } else { Ordering::Greater };
    }
    let mut result = match key {
        "size" => left.size().cmp(&right.size()),
        "modified" => left.modified().cmp(&right.modified()),
        "type" => text_cmp(&left.type_description(), &right.type_description(), natural_sort),
        _ => text_cmp(&left.data().display_name, &right.data().display_name, natural_sort),
    };
    if result == Ordering::Equal {
        result = text_cmp(&left.data().display_name, &right.data().display_name, natural_sort);
    }
    if result == Ordering::Equal {
        result = left.data().uri.cmp(&right.data().uri);
    }
    if descending {
        result.reverse()
    } else {
        result
    }
}

/// `sort_items(items, key, descending, folders_first, natural_sort)` (stable).
pub fn sort_items(items: &[FileItem], key: &str, descending: bool, folders_first: bool, natural_sort: bool) -> Vec<FileItem> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| compare_items(a, b, key, descending, folders_first, natural_sort));
    sorted
}

enum GlobToken {
    Star,
    One,
    Literal(char),
    Set { negate: bool, ranges: Vec<(char, char)> },
}

fn glob_tokens(pattern: &[char]) -> Vec<GlobToken> {
    let mut tokens = Vec::new();
    let mut i = 0;
    let n = pattern.len();
    while i < n {
        let c = pattern[i];
        i += 1;
        match c {
            '*' => {
                if !matches!(tokens.last(), Some(GlobToken::Star)) {
                    tokens.push(GlobToken::Star);
                }
            }
            '?' => tokens.push(GlobToken::One),
            '[' => {
                let mut j = i;
                if j < n && pattern[j] == '!' {
                    j += 1;
                }
                if j < n && pattern[j] == ']' {
                    j += 1;
                }
                while j < n && pattern[j] != ']' {
                    j += 1;
                }
                if j >= n {
                    tokens.push(GlobToken::Literal('['));
                    continue;
                }
                let mut stuff = &pattern[i..j];
                i = j + 1;
                let negate = stuff.first() == Some(&'!');
                if negate {
                    stuff = &stuff[1..];
                }
                let mut ranges = Vec::new();
                let mut k = 0;
                while k < stuff.len() {
                    if k + 2 < stuff.len() && stuff[k + 1] == '-' {
                        if stuff[k] <= stuff[k + 2] {
                            ranges.push((stuff[k], stuff[k + 2]));
                        }
                        k += 3;
                    } else {
                        ranges.push((stuff[k], stuff[k]));
                        k += 1;
                    }
                }
                tokens.push(GlobToken::Set { negate, ranges });
            }
            c => tokens.push(GlobToken::Literal(c)),
        }
    }
    tokens
}

fn glob_match(tokens: &[GlobToken], text: &[char]) -> bool {
    // Iterative matcher with single-star backtracking (sufficient for globs).
    let (mut t, mut s) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    let single = |token: &GlobToken, c: char| match token {
        GlobToken::One => true,
        GlobToken::Literal(l) => *l == c,
        GlobToken::Set { negate, ranges } => {
            if ranges.is_empty() {
                // Python: an empty negated set matches any character, an empty set nothing.
                return *negate;
            }
            ranges.iter().any(|(a, b)| *a <= c && c <= *b) != *negate
        }
        GlobToken::Star => false,
    };
    while s < text.len() {
        if t < tokens.len() {
            if let GlobToken::Star = tokens[t] {
                star = Some((t, s));
                t += 1;
                continue;
            }
            if single(&tokens[t], text[s]) {
                t += 1;
                s += 1;
                continue;
            }
        }
        match star {
            Some((star_t, star_s)) => {
                t = star_t + 1;
                s = star_s + 1;
                star = Some((star_t, star_s + 1));
            }
            None => return false,
        }
    }
    tokens[t..].iter().all(|token| matches!(token, GlobToken::Star))
}

/// `fnmatch.fnmatchcase(name, pattern)`.
pub fn fnmatchcase(name: &str, pattern: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = name.chars().collect();
    glob_match(&glob_tokens(&pattern), &text)
}

/// `name_matches(name, pattern)`: substring search, or shell-style matching
/// if a glob (`*?[`) is entered; case-insensitive.
pub fn name_matches(name: &str, pattern: &str) -> bool {
    let name = tessel_core::pyjson::casefold(name);
    let folded = tessel_core::pyjson::casefold(pattern);
    let pattern = tessel_core::pyjson::py_strip(&folded);
    if pattern.is_empty() {
        return true;
    }
    if pattern.contains(['*', '?', '[']) {
        return fnmatchcase(&name, pattern);
    }
    name.contains(pattern)
}

// ---- Presentation -----------------------------------------------------------

/// `item_count(count)`: `1 item` / `{count} items` (translated).
pub fn item_count(count: i64) -> String {
    if count == 1 {
        tessel_core::tr!("1 item")
    } else {
        tessel_core::tr!("{count} items", count = count)
    }
}

/// `format_size(size)`: `"1,023 B"`, `"1.0 KiB"` … `"EiB"`.
pub fn format_size(size: u64) -> String {
    use tessel_core::pyfmt::{format_value, Arg};
    if size < 1024 {
        return format!("{} B", format_value(&Arg::from(size as i64), ",").unwrap_or_else(|_| size.to_string()));
    }
    let mut amount = size as f64;
    for unit in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        amount /= 1024.0;
        if amount < 1024.0 || unit == "EiB" {
            let text = format_value(&Arg::from(amount), ",.1f").unwrap_or_else(|_| format!("{amount:.1}"));
            return format!("{text} {unit}");
        }
    }
    unreachable!()
}

/// `format_modified(timestamp)`: local `"%b %d, %Y %H:%M"`, or `"—"` for 0 /
/// out-of-range values (C `strftime`, like Python).
pub fn format_modified(timestamp: u64) -> String {
    const NONE: &str = "—";
    if timestamp == 0 || timestamp > i64::MAX as u64 {
        return NONE.into();
    }
    let time = timestamp as libc::time_t;
    // SAFETY: localtime_r/strftime write into local buffers only.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return NONE.into();
        }
        let year = tm.tm_year as i64 + 1900;
        if !(1..=9999).contains(&year) {
            return NONE.into();
        }
        let mut buffer = [0u8; 128];
        let length = libc::strftime(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len(), c"%b %d, %Y %H:%M".as_ptr(), &tm);
        if length == 0 {
            return NONE.into();
        }
        String::from_utf8_lossy(&buffer[..length]).into_owned()
    }
}

// ---- DirectoryLoader --------------------------------------------------------

struct DirectoryInner {
    batch_size: i32,
    generation: Cell<u64>,
    cancellable: RefCell<Option<gio::Cancellable>>,
}

/// One directory load at a time, without blocking the main loop (a cheap
/// cloneable handle). `on_done(error)` receives `None` on success.
/// Cancelling or starting another load suppresses every old callback,
/// including completion.
#[derive(Clone)]
pub struct DirectoryLoader(Rc<DirectoryInner>);

impl Default for DirectoryLoader {
    fn default() -> Self {
        Self::new(128)
    }
}

struct DirectoryJob {
    loader: Rc<DirectoryInner>,
    generation: u64,
    cancellable: gio::Cancellable,
    on_batch: Box<dyn Fn(Vec<FileItem>)>,
    on_done: RefCell<Option<Box<dyn FnOnce(Option<glib::Error>)>>>,
}

impl DirectoryJob {
    fn current(&self) -> bool {
        self.loader.generation.get() == self.generation && !self.cancellable.is_cancelled()
    }

    fn complete(&self, error: Option<glib::Error>) {
        if self.current() {
            self.loader.cancellable.replace(None);
            let done = self.on_done.borrow_mut().take();
            if let Some(done) = done {
                done(error);
            }
        }
    }

    fn close(enumerator: &gio::FileEnumerator) {
        enumerator.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
    }

    fn next_batch(self: Rc<Self>, enumerator: gio::FileEnumerator) {
        let batch_size = self.loader.batch_size;
        let cancellable = self.cancellable.clone();
        let source = enumerator.clone();
        enumerator.next_files_async(batch_size, glib::Priority::DEFAULT, Some(&cancellable), move |result| {
            self.batch_ready(source, result)
        });
    }

    fn batch_ready(self: Rc<Self>, enumerator: gio::FileEnumerator, result: Result<Vec<gio::FileInfo>, glib::Error>) {
        let infos = match result {
            Ok(infos) => infos,
            Err(error) => {
                Self::close(&enumerator);
                self.complete(Some(error));
                return;
            }
        };
        if !self.current() {
            Self::close(&enumerator);
            return;
        }
        if infos.is_empty() {
            Self::close(&enumerator);
            self.complete(None);
            return;
        }
        let items: Vec<FileItem> = infos.iter().map(|info| FileItem::new(&enumerator.child(info), info)).collect();
        let job = self.clone();
        load_desktop_icons_async(items, Some(&self.cancellable), move |items| {
            if job.current() {
                (job.on_batch)(items);
            }
            if job.current() {
                job.next_batch(enumerator);
            } else {
                Self::close(&enumerator);
            }
        });
    }
}

impl DirectoryLoader {
    /// `DirectoryLoader(batch_size=128)`, clamped to 1..=512.
    pub fn new(batch_size: i64) -> Self {
        Self(Rc::new(DirectoryInner {
            batch_size: batch_size.clamp(1, 512) as i32,
            generation: Cell::new(0),
            cancellable: RefCell::new(None),
        }))
    }

    pub fn batch_size(&self) -> i32 {
        self.0.batch_size
    }

    /// Whether a load is in flight (Python `_cancellable is not None`).
    pub fn is_loading(&self) -> bool {
        self.0.cancellable.borrow().is_some()
    }

    pub fn cancel(&self) {
        self.0.generation.set(self.0.generation.get() + 1);
        let cancellable = self.0.cancellable.borrow_mut().take();
        if let Some(cancellable) = cancellable {
            cancellable.cancel();
        }
    }

    /// Enumerate `directory` with [`FILE_ATTRIBUTES`]; batches (launcher icons
    /// already enriched) go to `on_batch`, then `on_done(None | Some(error))`.
    pub fn start(
        &self,
        directory: &gio::File,
        on_batch: impl Fn(Vec<FileItem>) + 'static,
        on_done: impl FnOnce(Option<glib::Error>) + 'static,
    ) {
        self.cancel();
        let cancellable = gio::Cancellable::new();
        self.0.cancellable.replace(Some(cancellable.clone()));
        let job = Rc::new(DirectoryJob {
            loader: self.0.clone(),
            generation: self.0.generation.get(),
            cancellable: cancellable.clone(),
            on_batch: Box::new(on_batch),
            on_done: RefCell::new(Some(Box::new(on_done))),
        });
        directory.enumerate_children_async(
            FILE_ATTRIBUTES,
            QUERY_FLAGS,
            glib::Priority::DEFAULT,
            Some(&cancellable),
            move |result| match result {
                Err(error) => job.complete(Some(error)),
                Ok(enumerator) => {
                    if job.current() {
                        job.next_batch(enumerator);
                    } else {
                        DirectoryJob::close(&enumerator);
                    }
                }
            },
        );
    }
}

// ---- SearchLoader -----------------------------------------------------------

/// Keyword options of `SearchLoader.start`.
#[derive(Clone, Copy, Debug)]
pub struct SearchOptions {
    pub contents: bool,
    pub include_hidden: bool,
    pub recursive: bool,
    pub max_results: i64,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self { contents: false, include_hidden: false, recursive: true, max_results: 10000 }
    }
}

/// A batch moved from the worker to the main context. GFileInfo has no thread
/// affinity; ownership is transferred whole and never shared.
struct Handoff(Vec<(gio::File, gio::FileInfo, Option<DesktopIcon>)>);
// SAFETY: see above — each GFileInfo is created on the worker and only ever
// touched again by the receiving thread.
unsafe impl Send for Handoff {}

enum SearchMessage {
    Batch(Handoff),
    Done { error: Option<glib::Error>, skipped: u64, truncated: bool },
}

struct SearchInner {
    batch_size: usize,
    max_content_bytes: usize,
    generation: Arc<AtomicU64>,
    cancellable: RefCell<Option<gio::Cancellable>>,
    truncated: Cell<bool>,
    skipped: Cell<u64>,
}

/// Cancellable recursive name/content search with main-context callbacks
/// (a cheap cloneable handle). Content searches inspect up to
/// `max_content_bytes` (1 MiB by default) per regular file, skip binary data
/// and also match names. Symlinked directories are never traversed.
/// [`Self::truncated`] reports reaching `max_results`; [`Self::skipped`]
/// counts entries/subdirectories that could not be read.
#[derive(Clone)]
pub struct SearchLoader(Rc<SearchInner>);

impl Default for SearchLoader {
    fn default() -> Self {
        Self::new(64, 1024 * 1024)
    }
}

struct SearchWorker {
    generation: Arc<AtomicU64>,
    expected: u64,
    cancellable: gio::Cancellable,
    batch_size: usize,
    max_content_bytes: usize,
}

impl SearchWorker {
    fn current(&self) -> bool {
        self.generation.load(AtomicOrdering::SeqCst) == self.expected && !self.cancellable.is_cancelled()
    }

    fn content_matches(&self, file: &gio::File, query: &str) -> Result<bool, glib::Error> {
        let stream = file.read(Some(&self.cancellable))?;
        let mut data = Vec::new();
        let mut remaining = self.max_content_bytes;
        let result: Result<bool, glib::Error> = (|| {
            while remaining > 0 {
                let chunk = stream.read_bytes(65536.min(remaining), Some(&self.cancellable))?;
                if chunk.is_empty() {
                    break;
                }
                if chunk.contains(&0) {
                    return Ok(false);
                }
                data.extend_from_slice(&chunk);
                remaining = remaining.saturating_sub(chunk.len());
            }
            Ok(true)
        })();
        let _ = stream.close(None::<&gio::Cancellable>);
        if !result? {
            return Ok(false);
        }
        let text = tessel_core::pyjson::casefold(&String::from_utf8_lossy(&data));
        Ok(text.contains(&tessel_core::pyjson::casefold(query)))
    }

    fn run(self, directory: gio::File, query: String, options: SearchOptions, sender: async_channel::Sender<SearchMessage>) {
        let max_results = options.max_results.clamp(1, 100000) as usize;
        let mut pending: Vec<(gio::File, u32)> = vec![(directory, 0)];
        let mut batch = Vec::new();
        let (mut matched, mut skipped) = (0usize, 0u64);
        let mut error = None;
        while let Some((folder, depth)) = (self.current() && matched < max_results).then(|| pending.pop()).flatten() {
            let enumerator = match folder.enumerate_children(FILE_ATTRIBUTES, QUERY_FLAGS, Some(&self.cancellable)) {
                Ok(enumerator) => enumerator,
                Err(exc) => {
                    if depth == 0 {
                        error = Some(exc);
                        break;
                    }
                    skipped += 1;
                    continue;
                }
            };
            let mut failure = None;
            while self.current() && matched < max_results {
                let info = match enumerator.next_file(Some(&self.cancellable)) {
                    Ok(Some(info)) => info,
                    Ok(None) => break,
                    Err(exc) => {
                        failure = Some(exc);
                        break;
                    }
                };
                let child = enumerator.child(&info);
                let name = attribute_byte_string(&info, "standard::name")
                    .filter(|n| !n.is_empty())
                    .or_else(|| child.basename().map(|b| b.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                let display_name = non_empty(info.attribute_string("standard::display-name")).unwrap_or_else(|| name.clone());
                let is_dir = info.attribute_uint32("standard::type") == gio::FileType::Directory.into_glib_u32();
                let is_symlink = info.boolean("standard::is-symlink");
                let is_hidden = info.boolean("standard::is-hidden") || name.starts_with('.');
                if !options.include_hidden && is_hidden {
                    continue;
                }
                if options.recursive && is_dir && !is_symlink && depth < 128 {
                    pending.push((child.clone(), depth + 1));
                }
                let mut matches = name_matches(&display_name, &query);
                if !matches && options.contents && !is_dir && !is_symlink && info.file_type() == gio::FileType::Regular {
                    match self.content_matches(&child, &query) {
                        Ok(found) => matches = found,
                        Err(_) => skipped += 1,
                    }
                }
                if matches {
                    let path = child.path();
                    let icon = if desktop_entry_parts(path.as_deref(), &name, info.file_type(), info.attribute_uint64("standard::size"))
                        && !self.cancellable.is_cancelled()
                    {
                        read_desktop_icon(&child, path.as_deref().unwrap_or(Path::new("")), Some(&self.cancellable))
                    } else {
                        None
                    };
                    batch.push((child, info, icon));
                    matched += 1;
                    if batch.len() >= self.batch_size {
                        let _ = sender.send_blocking(SearchMessage::Batch(Handoff(std::mem::take(&mut batch))));
                    }
                }
            }
            let _ = enumerator.close(None::<&gio::Cancellable>);
            if let Some(exc) = failure {
                if depth == 0 {
                    error = Some(exc);
                    break;
                }
                skipped += 1;
            }
        }
        if self.current() {
            if !batch.is_empty() {
                let _ = sender.send_blocking(SearchMessage::Batch(Handoff(batch)));
            }
            let _ = sender.send_blocking(SearchMessage::Done { error, skipped, truncated: matched >= max_results });
        }
    }
}

impl SearchLoader {
    /// `SearchLoader(batch_size=64, max_content_bytes=1 MiB)` (clamped to
    /// 1..=512 and 1..=16 MiB).
    pub fn new(batch_size: i64, max_content_bytes: i64) -> Self {
        Self(Rc::new(SearchInner {
            batch_size: batch_size.clamp(1, 512) as usize,
            max_content_bytes: max_content_bytes.clamp(1, 16 * 1024 * 1024) as usize,
            generation: Arc::new(AtomicU64::new(0)),
            cancellable: RefCell::new(None),
            truncated: Cell::new(false),
            skipped: Cell::new(0),
        }))
    }

    pub fn batch_size(&self) -> usize {
        self.0.batch_size
    }
    pub fn max_content_bytes(&self) -> usize {
        self.0.max_content_bytes
    }
    pub fn truncated(&self) -> bool {
        self.0.truncated.get()
    }
    pub fn skipped(&self) -> u64 {
        self.0.skipped.get()
    }
    /// Whether a search is in flight.
    pub fn is_searching(&self) -> bool {
        self.0.cancellable.borrow().is_some()
    }

    pub fn cancel(&self) {
        self.0.generation.fetch_add(1, AtomicOrdering::SeqCst);
        let cancellable = self.0.cancellable.borrow_mut().take();
        if let Some(cancellable) = cancellable {
            cancellable.cancel();
        }
    }

    /// Search below `directory` on a worker thread ("file-manager-search");
    /// batches and `on_done` are delivered at idle priority on the calling
    /// thread's thread-default main context.
    pub fn start(
        &self,
        directory: &gio::File,
        query: &str,
        on_batch: impl Fn(Vec<FileItem>) + 'static,
        on_done: impl FnOnce(Option<glib::Error>) + 'static,
        options: SearchOptions,
    ) {
        self.cancel();
        let generation = self.0.generation.load(AtomicOrdering::SeqCst);
        let cancellable = gio::Cancellable::new();
        self.0.cancellable.replace(Some(cancellable.clone()));
        self.0.truncated.set(false);
        self.0.skipped.set(0);
        let query = tessel_core::pyjson::py_strip(query).to_owned();
        let (sender, receiver) = async_channel::unbounded::<SearchMessage>();
        let worker = SearchWorker {
            generation: self.0.generation.clone(),
            expected: generation,
            cancellable: cancellable.clone(),
            batch_size: self.0.batch_size,
            max_content_bytes: self.0.max_content_bytes,
        };
        let directory = directory.clone();
        let spawned = std::thread::Builder::new()
            .name("file-manager-search".into())
            .spawn(move || worker.run(directory, query, options, sender));
        let inner = self.0.clone();
        let current = {
            let (inner, cancellable) = (inner.clone(), cancellable.clone());
            move || inner.generation.load(AtomicOrdering::SeqCst) == generation && !cancellable.is_cancelled()
        };
        if let Err(error) = spawned {
            let error = glib::Error::new(gio::IOErrorEnum::Failed, &error.to_string());
            glib::MainContext::ref_thread_default().spawn_local_with_priority(glib::Priority::DEFAULT_IDLE, async move {
                if current() {
                    inner.cancellable.replace(None);
                    on_done(Some(error));
                }
            });
            return;
        }
        let mut on_done = Some(on_done);
        glib::MainContext::ref_thread_default().spawn_local_with_priority(glib::Priority::DEFAULT_IDLE, async move {
            while let Ok(message) = receiver.recv().await {
                if !current() {
                    continue;
                }
                match message {
                    SearchMessage::Batch(Handoff(entries)) => {
                        let items: Vec<FileItem> = entries
                            .into_iter()
                            .map(|(file, info, icon)| {
                                let item = FileItem::new(&file, &info);
                                if let Some(icon) = icon {
                                    apply_desktop_icon(&item, &icon);
                                }
                                item
                            })
                            .collect();
                        if !items.is_empty() {
                            on_batch(items);
                        }
                    }
                    SearchMessage::Done { error, skipped, truncated } => {
                        inner.skipped.set(skipped);
                        inner.truncated.set(truncated);
                        inner.cancellable.replace(None);
                        if let Some(done) = on_done.take() {
                            done(error);
                        }
                        break;
                    }
                }
            }
            // The worker ended without a result (it panicked): like Python's
            // catch-all, never leave the search spinner running.
            if let Some(done) = on_done.take() {
                if current() {
                    inner.cancellable.replace(None);
                    done(Some(glib::Error::new(gio::IOErrorEnum::Failed, "The search stopped unexpectedly.")));
                }
            }
        });
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
