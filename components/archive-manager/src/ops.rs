//! What the window, the command line and the drag-and-drop service do with
//! an archive: open (format detection + plugin choice), extract (staging +
//! conflict resolution), add, delete, rename/move, copy, create folder,
//! create, test, comment, checksums. Everything here blocks: run it on a
//! worker thread (see `job`).

use crate::archive::{AddItem, CreateOptions, Ctx, Error, Listing, Result};
use crate::backend::{self, process::TempDir};
use archive_settings::plugins::{self, Kind, Plugin};
use archive_settings::Settings;
use std::path::{Path, PathBuf};
use suite_core::tr;

#[derive(Clone)]
pub struct Archive {
    pub path: PathBuf,
    pub kind: Kind,
    pub mime: String,
    pub volumes: Vec<PathBuf>,
    /// Backend that listed the archive (and extracts/tests it).
    pub reader: &'static str,
    /// Backend used for changes; None: read-only.
    pub writer: Option<&'static str>,
    pub listing: Listing,
    pub password: Option<String>,
}

impl Archive {
    pub fn read_only(&self) -> bool {
        self.writer.is_none() || self.volumes.len() > 1 || !writable_file(&self.path)
    }

    pub fn ctx(&self, base: &Ctx) -> Ctx {
        let mut ctx = base.clone();
        if ctx.password.is_none() {
            ctx.password = self.password.clone();
        }
        ctx
    }

    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Why editing is disabled (for the status bar / tooltips).
    pub fn read_only_reason(&self) -> Option<String> {
        if self.writer.is_none() {
            Some(tr!("{format} archives are read-only.", format = tr!(self.kind.label())))
        } else if self.volumes.len() > 1 {
            Some(tr!("Multi-volume archives are read-only."))
        } else if !writable_file(&self.path) {
            Some(tr!("You do not have permission to change this archive."))
        } else {
            None
        }
    }
}

fn writable_file(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let Ok(d) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 && libc::access(d.as_ptr(), libc::W_OK) == 0 }
}

/// The name without a split-volume suffix (`x.7z.001` -> `x.7z`, `x.part2.rar` -> `x.rar`).
pub fn volume_base_name(name: &str) -> String {
    if let Some((stem, number)) = name.rsplit_once('.') {
        if number.len() == 3 && number.bytes().all(|b| b.is_ascii_digit()) && stem.contains('.') {
            return stem.to_string();
        }
    }
    let lower = name.to_lowercase();
    if let Some(pos) = lower.rfind(".part") {
        if lower[pos + 5..].strip_suffix(".rar").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
            return format!("{}.rar", &name[..pos]);
        }
    }
    name.to_string()
}

/// Magic-number fallback when the content type is unknown.
pub fn sniff(data: &[u8]) -> Option<Kind> {
    let starts = |m: &[u8]| data.starts_with(m);
    Some(if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") || starts(b"PK\x07\x08") {
        Kind::Zip
    } else if starts(b"7z\xbc\xaf\x27\x1c") {
        Kind::SevenZip
    } else if starts(b"Rar!\x1a\x07") {
        Kind::Rar
    } else if data.len() > 262 && &data[257..262] == b"ustar" {
        Kind::Tar
    } else if starts(b"\x1f\x8b") {
        Kind::Gz
    } else if starts(b"BZh") {
        Kind::Bz2
    } else if starts(b"\xfd7zXZ\x00") {
        Kind::Xz
    } else if starts(b"\x28\xb5\x2f\xfd") {
        Kind::Zst
    } else if starts(b"\x04\x22\x4d\x18") {
        Kind::Lz4
    } else if starts(b"LZIP") {
        Kind::Lz
    } else if starts(b"\x89LZO") {
        Kind::Lzo
    } else if starts(b"\x1f\x9d") {
        Kind::Z
    } else if starts(b"!<arch>\ndebian") {
        Kind::Deb
    } else if starts(b"!<arch>") {
        Kind::Ar
    } else if starts(b"\xed\xab\xee\xdb") {
        Kind::Rpm
    } else if starts(b"070701") || starts(b"070707") || starts(b"070702") || starts(b"\xc7\x71") {
        Kind::Cpio
    } else if starts(b"xar!") {
        Kind::Xar
    } else if starts(b"MSCF") {
        Kind::Cab
    } else if starts(b"hsqs") {
        Kind::Squashfs
    } else if data.len() > 32774 && &data[32769..32774] == b"CD001" {
        Kind::Iso
    } else {
        return None;
    })
}

/// Detect the format: shared-mime-info (name + content), then magic numbers.
pub fn detect(path: &Path) -> Result<(Kind, String)> {
    use std::io::Read;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let base = volume_base_name(&name);
    let mut data = vec![0u8; 36 * 1024];
    let mut file = std::fs::File::open(path).map_err(|e| Error::Failed(format!("{}: {e}", path.display())))?;
    let mut read = 0;
    while read < data.len() {
        match file.read(&mut data[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) => return Err(Error::Failed(format!("{}: {e}", path.display()))),
        }
    }
    data.truncate(read);
    let (mime, _uncertain) = gio::content_type_guess(Some(base.as_str()), &data[..read.min(4096)]);
    let mime = mime.to_string();
    if let Some(kind) = Kind::from_mime(&mime) {
        // A single-file compressor whose name says tar: trust the name (x.tar.gz renamed tgz…).
        return Ok((kind, mime));
    }
    if let Some(kind) = sniff(&data).or_else(|| Kind::from_name(&base)) {
        return Ok((kind, kind.mime().to_string()));
    }
    Err(Error::NoBackend(tr!("The file {name} is not a supported archive.", name = name)))
}

pub fn enabled_fn(settings: &Settings) -> impl Fn(&str) -> bool + '_ {
    move |id: &str| settings.plugin_enabled(id)
}

fn available(plugin: &Plugin) -> bool {
    plugin.available()
}

pub fn readers(kind: Kind, settings: &Settings) -> Vec<&'static Plugin> {
    plugins::ranked(kind, false, &enabled_fn(settings), &available)
}

pub fn writer(kind: Kind, settings: &Settings) -> Option<&'static Plugin> {
    plugins::ranked(kind, true, &enabled_fn(settings), &available).into_iter().next()
}

/// Open and list an archive. Tries each enabled backend in priority order;
/// password errors are returned at once (the caller asks and retries).
pub fn open(path: &Path, settings: &Settings, ctx: &Ctx) -> Result<Archive> {
    let (kind, mime) = detect(path)?;
    let candidates = readers(kind, settings);
    if candidates.is_empty() {
        return Err(Error::NoBackend(tr!("No enabled plugin can open {format} files. Check the Plugins settings.",
                                        format = tr!(kind.label()))));
    }
    let volumes = backend::volumes(path);
    let mut last = None;
    for plugin in candidates {
        let Some(b) = backend::get(plugin.id) else { continue };
        match b.list(path, kind, &volumes, ctx) {
            Ok(listing) => {
                let writer = writer(kind, settings).map(|p| p.id).filter(|_| !(kind.is_single_file()));
                return Ok(Archive { path: path.to_path_buf(), kind, mime, volumes, reader: plugin.id, writer, listing, password: ctx.password.clone() });
            }
            Err(e @ (Error::PasswordNeeded | Error::WrongPassword | Error::Cancelled)) => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| Error::NoBackend(tr!("No plugin could open this archive."))))
}

/// Ark's conflict choices.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    Overwrite,
    Skip,
    Rename(String),
    OverwriteAll,
    SkipAll,
    Cancel,
}

#[derive(Clone, Debug)]
pub struct Conflict {
    /// The existing file.
    pub target: PathBuf,
    /// The extracted file waiting to replace it.
    pub incoming: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct ExtractOptions {
    pub preserve_paths: bool,
    /// Archive folder whose contents go to the destination root (drag and drop
    /// from inside a folder: Ark drops the dragged entries' parent path).
    pub strip: Option<String>,
    pub subfolder: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExtractReport {
    /// Paths written at the destination root (files or folders).
    pub top: Vec<PathBuf>,
    pub skipped: usize,
    pub folder: PathBuf,
}

/// "archive" for archive.tar.gz / archive.7z.001 / archive.part1.rar.
pub fn subfolder_name(path: &Path) -> String {
    let name = volume_base_name(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let lower = name.to_lowercase();
    let mut best = name.len();
    for format in plugins::FORMATS {
        for extension in format.extensions {
            if lower.ends_with(&format!(".{extension}")) && name.len() > extension.len() + 1 {
                best = best.min(name.len() - extension.len() - 1);
            }
        }
    }
    let stem = &name[..best];
    if stem.is_empty() || stem == name && name.contains('.') {
        name.rsplit_once('.').map(|(s, _)| s.to_string()).filter(|s| !s.is_empty()).unwrap_or(name.clone())
    } else {
        stem.to_string()
    }
}

/// Ark's "single top-level folder" check for automatic subfolders.
pub fn needs_subfolder(listing: &Listing) -> bool {
    let top = listing.top_level();
    !(top.len() == 1 && top[0].is_dir)
}

/// "name (1).ext" style names that do not exist in `folder`.
pub fn free_name(folder: &Path, name: &str) -> String {
    if std::fs::symlink_metadata(folder.join(name)).is_err() {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for i in 1.. {
        let candidate = format!("{stem} ({i}){ext}");
        if std::fs::symlink_metadata(folder.join(&candidate)).is_err() {
            return candidate;
        }
    }
    unreachable!()
}

fn move_into(staged: &Path, target: &Path) -> Result<()> {
    match std::fs::rename(staged, target) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(staged, target)?;
            backend::process::remove_tree(staged);
            Ok(())
        }
        Err(e) => Err(Error::Failed(format!("{}: {e}", target.display()))),
    }
}

pub fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(source)?, target)?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(target)?;
        for child in std::fs::read_dir(source)?.flatten() {
            copy_tree(&child.path(), &target.join(child.file_name()))?;
        }
        let _ = std::fs::set_permissions(target, meta.permissions());
    } else {
        std::fs::copy(source, target)?;
    }
    Ok(())
}

struct Merger<'a> {
    resolve: &'a mut dyn FnMut(&Conflict) -> Resolution,
    all: Option<bool>,
    report: ExtractReport,
}

impl Merger<'_> {
    /// Move `staged` to `target`, asking about existing files.
    fn place(&mut self, staged: &Path, target: &Path, top: bool) -> Result<()> {
        let staged_meta = std::fs::symlink_metadata(staged)?;
        let mut target = target.to_path_buf();
        loop {
            let Ok(existing) = std::fs::symlink_metadata(&target) else {
                move_into(staged, &target)?;
                if top {
                    self.report.top.push(target);
                }
                return Ok(());
            };
            if existing.is_dir() && staged_meta.is_dir() {
                // Merge folders; only files conflict (as in Ark).
                for child in std::fs::read_dir(staged)?.flatten() {
                    self.place(&child.path(), &target.join(child.file_name()), false)?;
                }
                if top {
                    self.report.top.push(target);
                }
                return Ok(());
            }
            let choice = match self.all {
                Some(true) => Resolution::Overwrite,
                Some(false) => Resolution::Skip,
                None => (self.resolve)(&Conflict { target: target.clone(), incoming: staged.to_path_buf() }),
            };
            match choice {
                Resolution::OverwriteAll => {
                    self.all = Some(true);
                    continue;
                }
                Resolution::SkipAll => {
                    self.all = Some(false);
                    continue;
                }
                Resolution::Overwrite => {
                    if existing.is_dir() {
                        backend::process::remove_tree(&target);
                    } else {
                        std::fs::remove_file(&target)?;
                    }
                    continue;
                }
                Resolution::Skip => {
                    self.report.skipped += 1;
                    return Ok(());
                }
                Resolution::Rename(name) => {
                    let name = name.trim().replace('/', "_");
                    if name.is_empty() || name == "." || name == ".." {
                        continue;
                    }
                    target = target.with_file_name(name);
                    continue;
                }
                Resolution::Cancel => return Err(Error::Cancelled),
            }
        }
    }
}

fn files_below(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut children: Vec<_> = read.flatten().collect();
    children.sort_by_key(|c| c.file_name());
    for child in children {
        let path = child.path();
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => files_below(&path, out),
            Ok(_) => out.push(path),
            Err(_) => {}
        }
    }
}

/// Extract `selection` (None = everything) into `destination`.
pub fn extract(archive: &Archive, selection: Option<&[String]>, destination: &Path, options: &ExtractOptions, ctx: &Ctx,
               resolve: &mut dyn FnMut(&Conflict) -> Resolution) -> Result<ExtractReport> {
    let ctx = archive.ctx(ctx);
    let folder = match &options.subfolder {
        Some(sub) if !sub.is_empty() => destination.join(sub.replace('/', "_")),
        _ => destination.to_path_buf(),
    };
    std::fs::create_dir_all(&folder).map_err(|e| Error::Failed(format!("{}: {e}", folder.display())))?;
    let staging = TempDir::new_in(&folder, ".wayarchive-extract-")?;
    let expanded = selection.map(|s| archive.listing.expand(s));
    let b = backend::get(archive.reader).ok_or_else(|| Error::NoBackend(archive.reader.into()))?;
    let inner = Ctx { progress: { let p = ctx.progress.clone(); std::sync::Arc::new(move |f, item| p(if f < 0.0 { f } else { f * 0.95 }, item)) }, ..ctx.clone() };
    b.extract(&archive.path, archive.kind, &archive.volumes, expanded.as_deref(), staging.path(), &inner)?;
    ctx.check()?;
    let mut merger = Merger { resolve, all: None, report: ExtractReport { folder: folder.clone(), ..Default::default() } };
    if options.preserve_paths {
        let root = match options.strip.as_deref().filter(|s| !s.is_empty()) {
            Some(strip) => staging.path().join(strip),
            None => staging.path().to_path_buf(),
        };
        if root.is_dir() {
            let mut children: Vec<_> = std::fs::read_dir(&root)?.flatten().map(|c| c.path()).collect();
            children.sort();
            for child in children {
                ctx.check()?;
                let name = child.file_name().unwrap_or_default().to_os_string();
                merger.place(&child, &folder.join(name), true)?;
            }
        }
    } else {
        let mut files = Vec::new();
        files_below(staging.path(), &mut files);
        for file in files {
            ctx.check()?;
            let name = file.file_name().unwrap_or_default().to_os_string();
            merger.place(&file, &folder.join(name), true)?;
        }
    }
    ctx.report(1.0, "");
    Ok(merger.report)
}

fn writer_backend(archive: &Archive) -> Result<&'static dyn backend::Backend> {
    if let Some(reason) = archive.read_only_reason() {
        return Err(Error::Unsupported(reason));
    }
    backend::get(archive.writer.unwrap_or_default()).ok_or_else(|| Error::NoBackend("writer".into()))
}

/// Add files below `folder` ("" = root) of the archive.
pub fn add(archive: &Archive, sources: &[PathBuf], folder: &str, options: &CreateOptions, ctx: &Ctx) -> Result<()> {
    let ctx = archive.ctx(ctx);
    let items = items_for(sources, folder);
    let mut options = options.clone();
    if options.password.is_none() && archive.listing.encrypted {
        options.password = archive.password.clone();
        options.encrypt_header = archive.listing.header_encrypted;
    }
    writer_backend(archive)?.add(&archive.path, archive.kind, &items, &options, &ctx)
}

pub fn items_for(sources: &[PathBuf], folder: &str) -> Vec<AddItem> {
    sources.iter().filter_map(|source| {
        let name = source.file_name()?.to_string_lossy().into_owned();
        let dest = if folder.is_empty() { name } else { format!("{folder}/{name}") };
        Some(AddItem { source: source.clone(), dest })
    }).collect()
}

pub fn delete(archive: &Archive, entries: &[String], ctx: &Ctx) -> Result<()> {
    let ctx = archive.ctx(ctx);
    let expanded = archive.listing.expand(entries);
    writer_backend(archive)?.delete(&archive.path, archive.kind, &expanded, &ctx)
}

/// Rename one entry (a new name in the same folder).
pub fn rename(archive: &Archive, entry: &str, new_name: &str, ctx: &Ctx) -> Result<()> {
    let new_name = new_name.trim();
    if new_name.is_empty() || new_name.contains('/') || new_name == "." || new_name == ".." {
        return Err(Error::Failed(tr!("The name {name} is not valid.", name = new_name)));
    }
    let parent = entry.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let target = if parent.is_empty() { new_name.to_string() } else { format!("{parent}/{new_name}") };
    move_entries(archive, &[(entry.to_string(), target)], ctx)
}

/// Move entries: (old path, new path) pairs, folders with their contents.
pub fn move_entries(archive: &Archive, pairs: &[(String, String)], ctx: &Ctx) -> Result<()> {
    let ctx = archive.ctx(ctx);
    for (old, new) in pairs {
        if new == old || new.starts_with(&format!("{old}/")) {
            return Err(Error::Failed(tr!("A folder cannot be moved into itself.")));
        }
        if archive.listing.find(new).is_some() {
            return Err(Error::Failed(tr!("An entry named {name} already exists.", name = new)));
        }
    }
    let b = writer_backend(archive)?;
    if b.id() == "cli7z" || b.id() == "libarchive" {
        return b.rename(&archive.path, archive.kind, pairs, &ctx);
    }
    // Info-ZIP renames each stored name.
    let mut expanded = Vec::new();
    for (old, new) in pairs {
        for path in archive.listing.expand(std::slice::from_ref(old)) {
            expanded.push((path.clone(), backend::apply_renames(&path, std::slice::from_ref(&(old.clone(), new.clone())))));
        }
    }
    b.rename(&archive.path, archive.kind, &expanded, &ctx)
}

/// Paste moved entries into `folder` ("" = root).
pub fn move_into_folder(archive: &Archive, entries: &[String], folder: &str, ctx: &Ctx) -> Result<()> {
    let pairs: Vec<(String, String)> = entries.iter().filter_map(|entry| {
        let name = entry.rsplit('/').next()?.to_string();
        let target = if folder.is_empty() { name } else { format!("{folder}/{name}") };
        (target != *entry).then(|| (entry.clone(), target))
    }).collect();
    if pairs.is_empty() {
        return Ok(());
    }
    move_entries(archive, &pairs, ctx)
}

/// Paste copied entries into `folder`: extract them, then add them again.
pub fn copy_into_folder(archive: &Archive, entries: &[String], folder: &str, ctx: &Ctx) -> Result<()> {
    let ctx = archive.ctx(ctx);
    let parent = archive.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let scratch = TempDir::new_in(&parent, ".wayarchive-copy-")?;
    let b = backend::get(archive.reader).ok_or_else(|| Error::NoBackend(archive.reader.into()))?;
    let expanded = archive.listing.expand(entries);
    b.extract(&archive.path, archive.kind, &archive.volumes, Some(&expanded), scratch.path(), &ctx)?;
    let mut items = Vec::new();
    for entry in entries {
        let name = entry.rsplit('/').next().unwrap_or(entry);
        let mut target_name = name.to_string();
        let existing = |n: &str| archive.listing.find(&if folder.is_empty() { n.to_string() } else { format!("{folder}/{n}") }).is_some();
        if existing(&target_name) {
            let (stem, ext) = match name.rfind('.') {
                Some(i) if i > 0 => (&name[..i], &name[i..]),
                _ => (name, ""),
            };
            let mut i = 1;
            while existing(&target_name) {
                target_name = format!("{stem} ({i}){ext}");
                i += 1;
            }
        }
        let dest = if folder.is_empty() { target_name } else { format!("{folder}/{target_name}") };
        items.push(AddItem { source: scratch.path().join(entry), dest });
    }
    let mut options = CreateOptions::default();
    if archive.listing.encrypted {
        options.password = archive.password.clone();
        options.encrypt_header = archive.listing.header_encrypted;
    }
    writer_backend(archive)?.add(&archive.path, archive.kind, &items, &options, &ctx)
}

pub fn create_folder(archive: &Archive, parent: &str, name: &str, ctx: &Ctx) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(Error::Failed(tr!("The name {name} is not valid.", name = name)));
    }
    let path = if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") };
    if archive.listing.find(&path).is_some() {
        return Err(Error::Failed(tr!("An entry named {name} already exists.", name = path)));
    }
    let dir = archive.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let scratch = TempDir::new_in(&dir, ".wayarchive-folder-")?;
    let folder = scratch.path().join(name);
    std::fs::create_dir(&folder)?;
    let ctx = archive.ctx(ctx);
    writer_backend(archive)?.add(&archive.path, archive.kind, &[AddItem { source: folder, dest: path }], &CreateOptions::default(), &ctx)
}

/// Create a new archive at `path` from `sources` (relative to their folders).
pub fn create(path: &Path, kind: Kind, sources: &[PathBuf], options: &CreateOptions, settings: &Settings, ctx: &Ctx) -> Result<()> {
    let plugin = writer(kind, settings).ok_or_else(|| Error::NoBackend(tr!("No enabled plugin can create {format} files.", format = tr!(kind.label()))))?;
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(Error::Failed(tr!("{name} already exists.", name = path.display().to_string())));
    }
    if kind.is_single_file() && (sources.len() != 1 || !sources[0].is_file()) {
        return Err(Error::Unsupported(tr!("A compressed file holds exactly one file. Choose a tar archive for several files.")));
    }
    if options.volume_size.is_some() && plugin.id != "cli7z" && plugin.id != "clizip" {
        return Err(Error::Unsupported(tr!("Split volumes need 7-Zip or Info-ZIP.")));
    }
    let b = backend::get(plugin.id).ok_or_else(|| Error::NoBackend(plugin.id.into()))?;
    let items = items_for(sources, "");
    let result = b.add(path, kind, &items, options, ctx);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result?;
    if kind == Kind::Zip && options.volume_size.is_none() && plugin.id != "cli7z" && plugin.id != "clizip" {
        if let Some(comment) = options.comment.as_deref().filter(|c| !c.is_empty()) {
            backend::zipcomment::write(path, comment)?;
        }
    }
    Ok(())
}

pub fn test(archive: &Archive, ctx: &Ctx) -> Result<()> {
    let ctx = archive.ctx(ctx);
    let b = backend::get(archive.reader).ok_or_else(|| Error::NoBackend(archive.reader.into()))?;
    b.test(&archive.path, archive.kind, &archive.volumes, &ctx)
}

pub fn set_comment(archive: &Archive, comment: &str) -> Result<()> {
    if archive.kind != Kind::Zip {
        return Err(Error::Unsupported(tr!("Only zip archives can store a comment here.")));
    }
    writer_backend(archive)?;
    backend::zipcomment::write(&archive.path, comment)
}

/// MD5, SHA-1 and SHA-256 of the archive file (hex).
pub fn checksums(path: &Path, ctx: &Ctx) -> Result<[String; 3]> {
    use md5::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let (mut a, mut b, mut c) = (md5::Md5::new(), sha1::Sha1::new(), sha2::Sha256::new());
    let mut buffer = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        ctx.check()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        a.update(&buffer[..n]);
        b.update(&buffer[..n]);
        c.update(&buffer[..n]);
        done += n as u64;
        if total > 0 {
            ctx.report(done as f64 / total as f64, "");
        }
    }
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok([hex(&a.finalize()), hex(&b.finalize()), hex(&c.finalize())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subfolder_names() {
        assert_eq!(subfolder_name(Path::new("/x/photos.tar.gz")), "photos");
        assert_eq!(subfolder_name(Path::new("/x/photos.7z.001")), "photos");
        assert_eq!(subfolder_name(Path::new("/x/photos.part1.rar")), "photos");
        assert_eq!(subfolder_name(Path::new("/x/my.backup.zip")), "my.backup");
        assert_eq!(subfolder_name(Path::new("/x/noext")), "noext");
    }

    #[test]
    fn volume_names() {
        assert_eq!(volume_base_name("a.7z.002"), "a.7z");
        assert_eq!(volume_base_name("a.part02.rar"), "a.rar");
        assert_eq!(volume_base_name("a.001"), "a.001");
        assert_eq!(volume_base_name("a.zip"), "a.zip");
    }

    #[test]
    fn magic_numbers() {
        assert_eq!(sniff(b"PK\x03\x04rest"), Some(Kind::Zip));
        assert_eq!(sniff(b"Rar!\x1a\x07\x01\x00"), Some(Kind::Rar));
        assert_eq!(sniff(b"!<arch>\ndebian-binary"), Some(Kind::Deb));
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn free_names() {
        let dir = std::env::temp_dir().join(format!("wayarchive-free-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_name(&dir, "a.txt"), "a.txt");
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("a (1).txt"), "").unwrap();
        assert_eq!(free_name(&dir, "a.txt"), "a (2).txt");
        assert_eq!(free_name(&dir, ".hidden"), ".hidden");
        let _ = std::fs::remove_dir_all(dir);
    }
}
