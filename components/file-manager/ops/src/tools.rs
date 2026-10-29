//! Local file utilities for Files (file_tools.py). No operation runs merely by
//! importing this module. Everything here blocks: call it from a worker thread.

use crate::error::{Error, ErrorKind, Result};
use crate::{fsx, pyos};
use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use suite_core::tr;

/// Something that reports cancellation (Python accepts a `threading.Event`
/// or a `Gio.Cancellable`).
pub trait Cancel {
    fn is_cancelled(&self) -> bool;
}
impl Cancel for std::sync::atomic::AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl Cancel for gio::Cancellable {
    fn is_cancelled(&self) -> bool {
        gio::prelude::CancellableExt::is_cancelled(self)
    }
}

/// Byte progress of the current member: `(copied, total)`.
pub type ProgressFn<'a> = &'a mut dyn FnMut(u64, u64);

const CHUNK: usize = 1024 * 1024;
/// Python `tarfile.RECORDSIZE`: archives are padded to a multiple of this.
const RECORDSIZE: u64 = 20 * 512;
/// Timeout of `getfacl`/`setfacl`.
pub const ACL_TIMEOUT: Duration = Duration::from_secs(15);

fn check(cancel: Option<&dyn Cancel>) -> Result<()> {
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(Error::cancelled());
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> Error {
    // Errors produced by our own adapters keep their Python class.
    if let Some(inner) = error.get_ref().and_then(|e| e.downcast_ref::<Error>()) {
        return inner.clone();
    }
    Error::from_io(&error, None)
}

fn copy_stream(source: &mut dyn Read, destination: &mut dyn Write, cancel: Option<&dyn Cancel>,
               progress: &mut Option<ProgressFn<'_>>, total: u64) -> Result<()> {
    let mut buffer = vec![0u8; CHUNK];
    let mut copied = 0u64;
    loop {
        check(cancel)?;
        let count = loop {
            match source.read(&mut buffer) {
                Ok(count) => break count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(io_error(error)),
            }
        };
        if count == 0 {
            break;
        }
        destination.write_all(&buffer[..count]).map_err(io_error)?;
        copied += count as u64;
        if let Some(progress) = progress.as_mut() {
            progress(copied, total);
        }
    }
    check(cancel)
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn open_flags(path: &Path, flags: i32) -> Result<OwnedFd> {
    let name = fsx::cstring(path)?;
    let fd = unsafe { libc::open(name.as_ptr(), flags, 0o666) };
    if fd < 0 {
        return Err(Error::last_os(Some(path)));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fstat(fd: RawFd, path: Option<&Path>) -> Result<libc::stat> {
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut info) } != 0 {
        return Err(Error::last_os(path));
    }
    Ok(info)
}

fn is_kind(mode: u32, kind: u32) -> bool {
    mode & libc::S_IFMT == kind
}

/// `_regular_reader`: open without following links; regular files only.
fn regular_reader(path: &Path) -> Result<File> {
    let fd = open_flags(path, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)?;
    let info = fstat(fd.as_raw_fd(), Some(path))?;
    if !is_kind(info.st_mode, libc::S_IFREG) {
        return Err(Error::value(tr!("Not a regular file: {path}", path = display(path))));
    }
    Ok(File::from(fd))
}

struct Entry {
    path: PathBuf,
    name: Vec<u8>,
    info: std::fs::Metadata,
}

/// Walk without following symlinks; reject duplicate roots and special files.
fn archive_entries(sources: &[PathBuf], destination: &Path, cancel: Option<&dyn Cancel>) -> Result<Vec<Entry>> {
    let mut seen = std::collections::HashSet::new();
    let target = pyos::abspath(destination);
    let mut result = Vec::new();
    for source in sources {
        let source = pyos::pure_absolute(source);
        let name = source.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
        if name.is_empty() || !seen.insert(name.clone()) {
            return Err(Error::value(tr!("Archive source names must be unique")));
        }
        let mut pending = vec![(source, name)];
        while let Some((path, name)) = pending.pop() {
            check(cancel)?;
            if pyos::abspath(&path) == target {
                return Err(Error::value(tr!("The archive cannot be created inside a selected source folder")));
            }
            let info = std::fs::symlink_metadata(&path).map_err(|e| Error::from_io(&e, Some(&path)))?;
            let kind = info.file_type();
            if kind.is_symlink() {
                return Err(Error::value(tr!("Archive creation does not follow symbolic links: {path}",
                                            path = display(&path))));
            }
            if !(kind.is_file() || kind.is_dir()) {
                return Err(Error::value(tr!("Unsupported special file: {path}", path = display(&path))));
            }
            let is_dir = kind.is_dir();
            result.push(Entry { path: path.clone(), name: name.clone(), info });
            if is_dir {
                // Also reject a not-yet-created destination below a source directory.
                if pyos::commonpath(&path, &target) == path {
                    return Err(Error::value(tr!("Choose an archive destination outside the selected folders")));
                }
                let mut children: Vec<OsString> = std::fs::read_dir(&path).map_err(|e| Error::from_io(&e, Some(&path)))?
                    .map(|entry| entry.map(|e| e.file_name()).map_err(|e| Error::from_io(&e, Some(&path))))
                    .collect::<Result<_>>()?;
                children.sort_by(|a, b| b.as_bytes().cmp(a.as_bytes()));
                for child in children {
                    let mut child_name = name.clone();
                    child_name.push(b'/');
                    child_name.extend_from_slice(child.as_bytes());
                    pending.push((path.join(&child), child_name));
                }
            }
        }
    }
    if result.is_empty() {
        return Err(Error::value(tr!("Select at least one file or folder")));
    }
    Ok(result)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Compression {
    None,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

/// Counts uncompressed tar bytes so the archive can be padded to `RECORDSIZE`.
struct Counting<W: Write> {
    inner: W,
    count: u64,
}
impl<W: Write> Write for Counting<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(data)?;
        self.count += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

enum Compressor {
    Plain(File),
    Gzip(flate2::write::GzEncoder<File>),
    Bzip2(bzip2::write::BzEncoder<File>),
    Xz(xz2::write::XzEncoder<File>),
}
impl Write for Compressor {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        match self {
            Compressor::Plain(w) => w.write(data),
            Compressor::Gzip(w) => w.write(data),
            Compressor::Bzip2(w) => w.write(data),
            Compressor::Xz(w) => w.write(data),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Compressor::Plain(w) => w.flush(),
            Compressor::Gzip(w) => w.flush(),
            Compressor::Bzip2(w) => w.flush(),
            Compressor::Xz(w) => w.flush(),
        }
    }
}
impl Compressor {
    fn finish(self) -> std::io::Result<()> {
        match self {
            Compressor::Plain(mut w) => w.flush(),
            Compressor::Gzip(w) => w.finish().map(drop),
            Compressor::Bzip2(w) => w.finish().map(drop),
            Compressor::Xz(w) => w.finish().map(drop),
        }
    }
}

/// tarfile reads members through this adapter, so cancellation is checked per chunk.
struct CancelReader<'a, 'p> {
    source: File,
    position: u64,
    total: u64,
    cancel: Option<&'a dyn Cancel>,
    progress: &'a mut Option<ProgressFn<'p>>,
}
impl Read for CancelReader<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(std::io::Error::other(Error::cancelled()));
        }
        let remaining = self.total.saturating_sub(self.position);
        let limit = buffer.len().min(remaining as usize).min(CHUNK);
        let count = self.source.read(&mut buffer[..limit])?;
        self.position += count as u64;
        if let Some(progress) = self.progress.as_mut() {
            progress(self.position, self.total);
        }
        Ok(count)
    }
}

fn zip_datetime(seconds: i64) -> Result<zip::DateTime> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time = seconds as libc::time_t;
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return Err(Error::value("timestamp out of range for platform time_t"));
    }
    let year = tm.tm_year + 1900;
    if year < 1980 {
        return Err(Error::value("ZIP does not support timestamps before 1980"));
    }
    zip::DateTime::from_date_and_time(year.min(2107) as u16, (tm.tm_mon + 1) as u8, tm.tm_mday as u8,
                                      tm.tm_hour as u8, tm.tm_min as u8, tm.tm_sec.min(59) as u8)
        .map_err(|_| Error::value("ZIP does not support timestamps after 2107"))
}

fn zip_error(error: zip::result::ZipError) -> Error {
    match error {
        zip::result::ZipError::Io(error) => io_error(error),
        other => Error::value(other.to_string()),
    }
}

fn name_string(name: &[u8]) -> Result<String> {
    String::from_utf8(name.to_vec()).map_err(|_| Error::value("'utf-8' codec can't encode a file name"))
}

fn write_zip(output: File, entries: &[Entry], cancel: Option<&dyn Cancel>, progress: &mut Option<ProgressFn<'_>>) -> Result<()> {
    let mut archive = zip::ZipWriter::new(output);
    for entry in entries {
        check(cancel)?;
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip_datetime(entry.info.mtime())?)
            .unix_permissions(entry.info.mode() & 0o7777);
        let name = name_string(&entry.name)?;
        if entry.info.is_dir() {
            archive.add_directory(format!("{name}/"), options).map_err(zip_error)?;
        } else {
            let mut source = regular_reader(&entry.path)?;
            archive.start_file(name, options.large_file(true)).map_err(zip_error)?;
            copy_stream(&mut source, &mut archive, cancel, progress, entry.info.size())?;
        }
    }
    check(cancel)?;
    archive.finish().map_err(zip_error)?;
    Ok(())
}

fn tar_header(entry: &Entry, name: &[u8]) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    {
        let ustar = header.as_ustar_mut().expect("ustar header");
        let length = name.len().min(100);
        ustar.name = [0; 100];
        ustar.name[..length].copy_from_slice(&name[..length]);
    }
    header.set_mode(entry.info.mode() & 0o777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(entry.info.mtime().max(0) as u64);
    if entry.info.is_dir() {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
    } else {
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(entry.info.size());
    }
    let _ = header.set_username("");
    let _ = header.set_groupname("");
    let _ = header.set_device_major(0);
    let _ = header.set_device_minor(0);
    header.set_cksum();
    header
}

fn write_tar(output: File, destination: &Path, compression: Compression, entries: &[Entry],
             cancel: Option<&dyn Cancel>, progress: &mut Option<ProgressFn<'_>>) -> Result<()> {
    let compressor = match compression {
        Compression::Gzip => {
            // Python's GzipFile header: the archive name without ".gz", mtime now, OS 255.
            let mut name = destination.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
            if name.ends_with(b".gz") {
                name.truncate(name.len() - 3);
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let mut builder = flate2::GzBuilder::new().mtime(now as u32).operating_system(255);
            if !name.is_empty() {
                builder = builder.filename(name);
            }
            Compressor::Gzip(builder.write(output, flate2::Compression::new(9)))
        }
        Compression::Bzip2 => Compressor::Bzip2(bzip2::write::BzEncoder::new(output, bzip2::Compression::new(9))),
        Compression::Xz => Compressor::Xz(xz2::write::XzEncoder::new(output, 6)),
        Compression::None | Compression::Zstd => Compressor::Plain(output),
    };
    let mut builder = tar::Builder::new(Counting { inner: compressor, count: 0 });
    for entry in entries {
        check(cancel)?;
        let mut name = entry.name.clone();
        if entry.info.is_dir() {
            name.push(b'/');
        }
        if !name.is_ascii() || name.len() > 100 {
            builder.append_pax_extensions([("path", name.as_slice())]).map_err(io_error)?;
        }
        let header = tar_header(entry, &name);
        if entry.info.is_dir() {
            builder.append(&header, std::io::empty()).map_err(io_error)?;
        } else {
            let source = regular_reader(&entry.path)?;
            let total = entry.info.size();
            let mut reader = CancelReader { source, position: 0, total, cancel, progress };
            builder.append(&header, &mut reader).map_err(io_error)?;
            if reader.position != total {
                return Err(Error::os("unexpected end of data"));
            }
        }
    }
    check(cancel)?;
    builder.finish().map_err(io_error)?;
    let mut counting = builder.into_inner().map_err(io_error)?;
    let remainder = counting.count % RECORDSIZE;
    if remainder > 0 {
        counting.write_all(&vec![0u8; (RECORDSIZE - remainder) as usize]).map_err(io_error)?;
    }
    counting.inner.finish().map_err(io_error)?;
    check(cancel)
}

/// Create ZIP/TAR/TAR.GZ/TAR.BZ2/TAR.XZ without replacing an existing file.
///
/// `progress` receives `(current_member_bytes, member_total_bytes)` on this thread.
/// Symbolic links and device files are rejected instead of dereferenced. A
/// failed or cancelled archive is removed.
pub fn create_archive(sources: &[PathBuf], destination: &Path, cancel: Option<&dyn Cancel>,
                      progress: Option<ProgressFn<'_>>) -> Result<PathBuf> {
    let destination = pyos::pure_absolute(destination);
    let lower = destination.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let modes = [(".tar.gz", Compression::Gzip), (".tgz", Compression::Gzip),
                 (".tar.bz2", Compression::Bzip2), (".tbz2", Compression::Bzip2),
                 (".tar.xz", Compression::Xz), (".txz", Compression::Xz), (".tar", Compression::None)];
    let mode = modes.iter().find(|(suffix, _)| lower.ends_with(suffix)).map(|(_, mode)| *mode);
    let is_zip = lower.ends_with(".zip");
    if !is_zip && mode.is_none() {
        return Err(Error::value(tr!("Use .zip, .tar, .tar.gz, .tar.bz2, or .tar.xz")));
    }
    let entries = archive_entries(sources, &destination, cancel)?;
    let output = open_flags(&destination, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC)?;
    let mut progress = progress;
    let outcome = if is_zip {
        write_zip(File::from(output), &entries, cancel, &mut progress)
    } else {
        write_tar(File::from(output), &destination, mode.unwrap_or(Compression::None), &entries, cancel, &mut progress)
    };
    match outcome {
        Ok(()) => Ok(destination),
        Err(error) => {
            let _ = std::fs::remove_file(&destination);
            Err(error)
        }
    }
}

/// `_member_path`: reject NUL, backslashes, absolute and drive paths and `..`.
fn member_path(name: &[u8]) -> Result<Vec<Vec<u8>>> {
    let unsafe_member = || Error::value(tr!("Unsafe archive member: {name}", name = String::from_utf8_lossy(name).into_owned()));
    let drive = name.len() >= 2 && name[0].is_ascii_alphabetic() && name[1] == b':';
    if name.contains(&0) || name.contains(&b'\\') || name.starts_with(b"/") || drive {
        return Err(unsafe_member());
    }
    let parts: Vec<Vec<u8>> = name.split(|&b| b == b'/').filter(|p| !p.is_empty() && *p != b".").map(<[u8]>::to_vec).collect();
    if parts.is_empty() || parts.iter().any(|p| p == b"..") {
        return Err(unsafe_member());
    }
    Ok(parts)
}

struct Member {
    position: usize,
    name: Vec<u8>,
    parts: Vec<Vec<u8>>,
    directory: bool,
    mode: u32,
    size: u64,
}

enum Container {
    Zip(Box<zip::ZipArchive<File>>),
    Tar(File, Compression),
}

fn tar_reader(file: &File, compression: Compression) -> Result<Box<dyn Read>> {
    let mut file = file.try_clone().map_err(io_error)?;
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    Ok(match compression {
        Compression::None => Box::new(file),
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(file)),
        Compression::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(file)),
        Compression::Xz => Box::new(xz2::read::XzDecoder::new_multi_decoder(file)),
        Compression::Zstd => Box::new(ruzstd::decoding::StreamingDecoder::new(file)
            .map_err(|e| read_error(format!("not a zstd file: {e}")))?),
    })
}

fn detect_compression(file: &mut File) -> Result<Compression> {
    let mut magic = [0u8; 6];
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut read = 0;
    while read < magic.len() {
        let count = file.read(&mut magic[read..]).map_err(io_error)?;
        if count == 0 {
            break;
        }
        read += count;
    }
    let magic = &magic[..read];
    Ok(if magic.starts_with(&[0x1f, 0x8b]) {
        Compression::Gzip
    } else if magic.starts_with(b"BZh") {
        Compression::Bzip2
    } else if magic.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Compression::Xz
    } else if magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Compression::Zstd
    } else {
        Compression::None
    })
}

/// Python raises `tarfile.ReadError` here (reported as a value error).
fn read_error(detail: impl std::fmt::Display) -> Error {
    Error::value(format!("file could not be opened successfully: {detail}"))
}

/// `Some(true)` = directory, `Some(false)` = regular file, `None` = link/special.
fn tar_kind(entry_type: tar::EntryType, name: &[u8]) -> Option<bool> {
    match entry_type {
        tar::EntryType::Directory => Some(true),
        tar::EntryType::Regular if name.ends_with(b"/") && entry_type.as_byte() == 0 => Some(true),
        tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => Some(false),
        _ => None,
    }
}

fn strip_dir_slashes(name: &mut Vec<u8>) {
    while name.len() > 1 && name.ends_with(b"/") {
        name.pop();
    }
}

fn list_members(container: &mut Container, cancel: Option<&dyn Cancel>) -> Result<Vec<Member>> {
    let mut members: Vec<Member> = Vec::new();
    let mut names: Vec<(Vec<Vec<u8>>, bool)> = Vec::new();
    let mut index_of: HashMap<Vec<Vec<u8>>, usize> = HashMap::new();
    let mut add = |member: Member| -> Result<()> {
        if let Some(&index) = index_of.get(&member.parts) {
            if !(member.directory && names[index].1) {
                return Err(Error::value(tr!("Duplicate archive member: {name}",
                                            name = String::from_utf8_lossy(&member.name).into_owned())));
            }
            names[index].1 = member.directory;
        } else {
            index_of.insert(member.parts.clone(), names.len());
            names.push((member.parts.clone(), member.directory));
        }
        members.push(member);
        Ok(())
    };
    match container {
        Container::Zip(archive) => {
            for position in 0..archive.len() {
                check(cancel)?;
                let file = archive.by_index_raw(position).map_err(zip_error)?;
                // zipfile decodes names as UTF-8 (flag 0x800) or cp437 and cuts them at NUL.
                let mut name = file.name().map_err(zip_error)?.as_bytes().to_vec();
                if let Some(nul) = name.iter().position(|&b| b == 0) {
                    name.truncate(nul);
                }
                let directory = name.ends_with(b"/");
                if directory && (name == b"." || name == b"./") {
                    continue;
                }
                let parts = member_path(&name)?;
                let attributes = file.external_attributes() >> 16;
                let kind = attributes & 0o170000;
                if ![0, libc::S_IFREG, libc::S_IFDIR].contains(&kind) || file.encrypted() {
                    return Err(Error::value(tr!("Unsupported archive link, special file or encrypted entry: {name}",
                                                name = String::from_utf8_lossy(&name).into_owned())));
                }
                let mode = if attributes == 0 { 0o644 } else { attributes & 0o777 };
                let size = file.size();
                drop(file);
                add(Member { position, name, parts, directory, mode, size })?;
            }
        }
        Container::Tar(file, compression) => {
            let reader = tar_reader(file, *compression)?;
            let mut archive = tar::Archive::new(reader);
            let entries = archive.entries().map_err(read_error)?;
            for (position, entry) in entries.enumerate() {
                check(cancel)?;
                let entry = entry.map_err(read_error)?;
                let header = entry.header();
                let entry_type = header.entry_type();
                if matches!(entry_type, tar::EntryType::XGlobalHeader) {
                    continue;
                }
                let mut name = entry.path_bytes().into_owned();
                let kind = tar_kind(entry_type, &name);
                let directory = kind == Some(true);
                if directory {
                    strip_dir_slashes(&mut name);
                }
                if directory && (name == b"." || name == b"./") {
                    continue;
                }
                let parts = member_path(&name)?;
                if kind.is_none() {
                    return Err(Error::value(tr!("Archive links and special files are not extracted: {name}",
                                                name = String::from_utf8_lossy(&name).into_owned())));
                }
                let mode = header.mode().map_err(read_error)? & 0o777;
                let size = entry.size();
                add(Member { position, name, parts, directory, mode, size })?;
            }
        }
    }
    for (parts, _) in &names {
        for index in 1..parts.len() {
            if let Some(&found) = index_of.get(&parts[..index].to_vec()) {
                if !names[found].1 {
                    return Err(Error::value(tr!("An archive file is also used as a folder")));
                }
            }
        }
    }
    Ok(members)
}

fn cstr(part: &[u8]) -> Result<CString> {
    CString::new(part).map_err(|_| Error::value("embedded null byte"))
}

fn openat_dir(fd: RawFd, part: &[u8]) -> Result<OwnedFd> {
    let name = cstr(part)?;
    let next = unsafe { libc::openat(fd, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if next < 0 {
        return Err(Error::last_os(Some(Path::new(OsStr::from_bytes(part)))));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(next) })
}

fn dup(fd: RawFd) -> Result<OwnedFd> {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return Err(Error::last_os(None));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

type Created = Vec<(Vec<Vec<u8>>, bool)>;

/// Walk/create `parts` below the held root without following links.
fn directory_fd(root: &OwnedFd, parts: &[Vec<u8>], created: &mut Created) -> Result<OwnedFd> {
    let mut fd = dup(root.as_raw_fd())?;
    let mut prefix: Vec<Vec<u8>> = Vec::new();
    for part in parts {
        prefix.push(part.clone());
        let name = cstr(part)?;
        if unsafe { libc::mkdirat(fd.as_raw_fd(), name.as_ptr(), 0o755) } == 0 {
            created.push((prefix.clone(), true));
        } else {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(Error::from_io(&error, Some(Path::new(OsStr::from_bytes(part)))));
            }
        }
        fd = openat_dir(fd.as_raw_fd(), part)?;
    }
    Ok(fd)
}

fn write_member(root: &OwnedFd, member: &Member, source: &mut dyn Read, created: &mut Created,
                cancel: Option<&dyn Cancel>, progress: &mut Option<ProgressFn<'_>>) -> Result<()> {
    let fd = directory_fd(root, &member.parts[..member.parts.len() - 1], created)?;
    let last = member.parts.last().expect("non-empty member");
    let name = cstr(last)?;
    let output = unsafe {
        libc::openat(fd.as_raw_fd(), name.as_ptr(),
                     libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600)
    };
    if output < 0 {
        return Err(Error::last_os(Some(Path::new(OsStr::from_bytes(last)))));
    }
    created.push((member.parts.clone(), false));
    let mut stream = unsafe { File::from_raw_fd(output) };
    copy_stream(source, &mut stream, cancel, progress, member.size)?;
    // Retain ordinary read/write/execute bits, never setuid/setgid/sticky.
    if unsafe { libc::fchmod(stream.as_raw_fd(), member.mode as libc::mode_t) } != 0 {
        return Err(Error::last_os(None));
    }
    Ok(())
}

fn rollback(root: &OwnedFd, created: &Created) {
    for (parts, is_directory) in created.iter().rev() {
        // Resolve only through held directories, without following links.
        let Ok(mut parent) = dup(root.as_raw_fd()) else { continue };
        let mut reachable = true;
        for part in &parts[..parts.len() - 1] {
            match openat_dir(parent.as_raw_fd(), part) {
                Ok(next) => parent = next,
                Err(_) => {
                    reachable = false;
                    break;
                }
            }
        }
        if !reachable {
            continue;
        }
        if let Ok(name) = cstr(parts.last().expect("non-empty")) {
            let flags = if *is_directory { libc::AT_REMOVEDIR } else { 0 };
            unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
        }
    }
}

fn join_parts(destination: &Path, parts: &[Vec<u8>]) -> PathBuf {
    let mut path = destination.to_path_buf();
    for part in parts {
        path.push(OsStr::from_bytes(part));
    }
    path
}

/// Extract regular files/directories with no overwrites or followed links.
///
/// Every member is validated before writing. Directory descriptors opened with
/// `O_NOFOLLOW` protect against existing or concurrently substituted symlinks.
/// A failed/cancelled extraction removes only entries created by this operation.
/// Returns the created paths (directories and files) in creation order.
pub fn extract_archive(archive: &Path, destination: &Path, cancel: Option<&dyn Cancel>,
                       progress: Option<ProgressFn<'_>>) -> Result<Vec<PathBuf>> {
    let destination = pyos::pure_absolute(destination);
    let mut input = regular_reader(archive)?;
    let zip_archive = input.try_clone().ok().and_then(|copy| zip::ZipArchive::new(copy).ok());
    let mut container = match zip_archive {
        Some(archive) => Container::Zip(Box::new(archive)),
        None => {
            let compression = detect_compression(&mut input)?;
            Container::Tar(input, compression)
        }
    };
    let members = list_members(&mut container, cancel)?;
    let mut progress = progress;
    let mut created: Created = Vec::new();
    let mut root_created = false;
    let mut root_fd: Option<OwnedFd> = None;
    let outcome = (|| -> Result<()> {
        if !destination.exists() {
            // The parent is chosen by the user, never recursively created.
            std::fs::create_dir(&destination).map_err(|e| Error::from_io(&e, Some(&destination)))?;
            root_created = true;
        }
        let root = open_flags(&destination, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)?;
        let root = root_fd.insert(root);
        match &mut container {
            Container::Zip(archive) => {
                for member in &members {
                    check(cancel)?;
                    if member.directory {
                        directory_fd(root, &member.parts, &mut created)?;
                        continue;
                    }
                    let mut source = archive.by_index(member.position).map_err(zip_error)?;
                    write_member(root, member, &mut source, &mut created, cancel, &mut progress)?;
                }
            }
            Container::Tar(file, compression) => {
                let reader = tar_reader(file, *compression)?;
                let mut tar_archive = tar::Archive::new(reader);
                let mut wanted = members.iter().peekable();
                for (position, entry) in tar_archive.entries().map_err(read_error)?.enumerate() {
                    let Some(member) = wanted.peek() else { break };
                    if member.position != position {
                        continue;
                    }
                    let member = wanted.next().expect("peeked");
                    check(cancel)?;
                    let mut entry = entry.map_err(read_error)?;
                    let mut name = entry.path_bytes().into_owned();
                    if member.directory {
                        strip_dir_slashes(&mut name);
                    }
                    if name != member.name {
                        return Err(Error::value(tr!("The archive changed while it was extracted")));
                    }
                    if member.directory {
                        directory_fd(root, &member.parts, &mut created)?;
                        continue;
                    }
                    write_member(root, member, &mut entry, &mut created, cancel, &mut progress)?;
                }
                if wanted.next().is_some() {
                    return Err(Error::value(tr!("The archive changed while it was extracted")));
                }
            }
        }
        check(cancel)
    })();
    match outcome {
        Ok(()) => Ok(created.iter().map(|(parts, _)| join_parts(&destination, parts)).collect()),
        Err(error) => {
            if let Some(root) = &root_fd {
                rollback(root, &created);
            }
            drop(root_fd);
            if root_created {
                let _ = std::fs::remove_dir(&destination);
            }
            Err(error)
        }
    }
}

/// Return source/destination pairs, preserving suffixes, with a `#` counter.
///
/// Example: `Photo ###` produces `Photo 001.jpg`. Renaming cycles and existing
/// destinations are rejected, so an executor can safely apply in order.
pub fn plan_batch_rename(paths: &[PathBuf], pattern: &str, start: i64) -> Result<Vec<(PathBuf, PathBuf)>> {
    let paths: Vec<PathBuf> = paths.iter().map(|p| pyos::pure_absolute(p)).collect();
    let unique: std::collections::HashSet<&PathBuf> = paths.iter().collect();
    if paths.is_empty() || unique.len() != paths.len() {
        return Err(Error::value(tr!("Select one or more distinct files")));
    }
    if pattern.is_empty() || pattern.contains('/') || pattern.contains('\\') || pattern.contains('\0')
        || pattern == "." || pattern == ".." {
        return Err(Error::value(tr!("Enter a file name without path separators")));
    }
    let suffixes: std::collections::HashSet<OsString> =
        paths.iter().map(|p| pyos::path_suffix(p.file_name().unwrap_or_default())).collect();
    if paths.len() > 1 && !pattern.contains('#') && suffixes.len() < paths.len() {
        return Err(Error::value(tr!("Include # in the name to number multiple files")));
    }
    if start < 0 {
        return Err(Error::value(tr!("The starting number must not be negative")));
    }
    let counter = regex::Regex::new("#+").expect("valid regex");
    let mut result = Vec::new();
    let mut targets = std::collections::HashSet::new();
    for (offset, path) in paths.iter().enumerate() {
        let number = start + offset as i64;
        let info = std::fs::symlink_metadata(path).map_err(|e| Error::from_io(&e, Some(path)))?;
        let suffix = if info.is_dir() { OsString::new() } else { pyos::path_suffix(path.file_name().unwrap_or_default()) };
        let digits = number.to_string();
        let replaced = counter.replace_all(pattern, |captures: &regex::Captures<'_>| {
            let width = captures[0].len();
            format!("{digits:0>width$}")
        });
        let mut name = OsString::from(replaced.as_ref());
        name.push(&suffix);
        if name == "." || name == ".." {
            return Err(Error::value(tr!("Invalid file name")));
        }
        let target = path.with_file_name(&name);
        let exists = std::fs::symlink_metadata(&target).is_ok();
        if targets.contains(&target) || (&target != path && exists) {
            return Err(Error::file_exists(tr!("A file already exists at {target}", target = display(&target))));
        }
        targets.insert(target.clone());
        result.push((path.clone(), target));
    }
    Ok(result)
}

/// `file_metadata(path)` (lstat; owner/group names fall back to the numeric id).
#[derive(Clone, Debug, PartialEq)]
pub struct FileMetadata {
    pub name: OsString,
    pub path: PathBuf,
    pub size: u64,
    pub mode: u32,
    pub mode_text: String,
    pub uid: u32,
    pub gid: u32,
    pub owner: String,
    pub group: String,
    pub modified: f64,
    pub accessed: f64,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub link_target: Option<PathBuf>,
}

/// `stat.filemode`.
pub fn filemode(mode: u32) -> String {
    let table: [&[(u32, char)]; 10] = [
        &[(0o120000, 'l'), (0o140000, 's'), (0o100000, '-'), (0o060000, 'b'), (0o040000, 'd'), (0o020000, 'c'), (0o010000, 'p')],
        &[(0o400, 'r')],
        &[(0o200, 'w')],
        &[(0o100 | 0o4000, 's'), (0o4000, 'S'), (0o100, 'x')],
        &[(0o040, 'r')],
        &[(0o020, 'w')],
        &[(0o010 | 0o2000, 's'), (0o2000, 'S'), (0o010, 'x')],
        &[(0o004, 'r')],
        &[(0o002, 'w')],
        &[(0o001 | 0o1000, 't'), (0o1000, 'T'), (0o001, 'x')],
    ];
    table.iter().map(|group| group.iter().find(|(bit, _)| mode & bit == *bit).map_or('-', |(_, c)| *c)).collect()
}

const NAME_BUFFER: usize = 16384;

fn user_name(uid: u32) -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; NAME_BUFFER];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let status = unsafe { libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    if status != 0 || result.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }.to_string_lossy().into_owned())
}

fn group_name(gid: u32) -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; NAME_BUFFER];
    let mut entry: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let status = unsafe { libc::getgrgid_r(gid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    if status != 0 || result.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(entry.gr_name) }.to_string_lossy().into_owned())
}

fn lookup_uid(name: &str) -> Option<u32> {
    let cname = CString::new(name).ok()?;
    let mut buffer = vec![0 as libc::c_char; NAME_BUFFER];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let status = unsafe { libc::getpwnam_r(cname.as_ptr(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    (status == 0 && !result.is_null()).then_some(entry.pw_uid)
}

fn lookup_gid(name: &str) -> Option<u32> {
    let cname = CString::new(name).ok()?;
    let mut buffer = vec![0 as libc::c_char; NAME_BUFFER];
    let mut entry: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let status = unsafe { libc::getgrnam_r(cname.as_ptr(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    (status == 0 && !result.is_null()).then_some(entry.gr_gid)
}

pub fn file_metadata(path: &Path) -> Result<FileMetadata> {
    let info = std::fs::symlink_metadata(path).map_err(|e| Error::from_io(&e, Some(path)))?;
    let is_symlink = info.file_type().is_symlink();
    let link_target = if is_symlink {
        Some(std::fs::read_link(path).map_err(|e| Error::from_io(&e, Some(path)))?)
    } else {
        None
    };
    let seconds = |s: i64, ns: i64| s as f64 + ns as f64 / 1e9;
    let absolute = pyos::pure_absolute(path);
    Ok(FileMetadata {
        name: path.file_name().map(OsStr::to_os_string).unwrap_or_default(),
        path: absolute,
        size: info.size(),
        mode: info.mode() & 0o7777,
        mode_text: filemode(info.mode()),
        uid: info.uid(),
        gid: info.gid(),
        owner: user_name(info.uid()).unwrap_or_else(|| info.uid().to_string()),
        group: group_name(info.gid()).unwrap_or_else(|| info.gid().to_string()),
        modified: seconds(info.mtime(), info.mtime_nsec()),
        accessed: seconds(info.atime(), info.atime_nsec()),
        is_directory: info.is_dir(),
        is_symlink,
        link_target,
    })
}

/// `recursive_size` result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeSummary {
    pub bytes: u64,
    pub files: u64,
    pub directories: u64,
    pub unreadable: u64,
}

/// Count regular bytes and items without following links, with an unreadable count.
/// Hard links (same device and inode) are counted once.
pub fn recursive_size(paths: &[PathBuf], cancel: Option<&dyn Cancel>) -> Result<SizeSummary> {
    let mut summary = SizeSummary::default();
    let mut pending: Vec<PathBuf> = paths.to_vec();
    let mut visited = std::collections::HashSet::new();
    while let Some(path) = pending.pop() {
        check(cancel)?;
        let info = match std::fs::symlink_metadata(&path) {
            Ok(info) => info,
            Err(_) => {
                summary.unreadable += 1;
                continue;
            }
        };
        if !visited.insert((info.dev(), info.ino())) {
            continue;
        }
        if info.is_dir() {
            summary.directories += 1;
            let children: std::io::Result<Vec<PathBuf>> = std::fs::read_dir(&path)
                .and_then(|entries| entries.map(|entry| entry.map(|e| e.path())).collect());
            match children {
                Ok(children) => pending.extend(children),
                Err(_) => summary.unreadable += 1,
            }
        } else {
            summary.files += 1;
            summary.bytes += info.size();
        }
    }
    Ok(summary)
}

fn hasher(name: &str) -> Option<Box<dyn digest::DynDigest>> {
    use digest::Digest;
    let digest: Box<dyn digest::DynDigest> = match name.to_ascii_lowercase().as_str() {
        "md5" => Box::new(md5::Md5::new()),
        "sha1" => Box::new(sha1::Sha1::new()),
        "sha224" => Box::new(sha2::Sha224::new()),
        "sha256" => Box::new(sha2::Sha256::new()),
        "sha384" => Box::new(sha2::Sha384::new()),
        "sha512" => Box::new(sha2::Sha512::new()),
        "sha512_224" => Box::new(sha2::Sha512_224::new()),
        "sha512_256" => Box::new(sha2::Sha512_256::new()),
        "sha3_224" => Box::new(sha3::Sha3_224::new()),
        "sha3_256" => Box::new(sha3::Sha3_256::new()),
        "sha3_384" => Box::new(sha3::Sha3_384::new()),
        "sha3_512" => Box::new(sha3::Sha3_512::new()),
        "blake2b" => Box::new(blake2::Blake2b512::new()),
        "blake2s" => Box::new(blake2::Blake2s256::new()),
        _ => return None,
    };
    Some(digest)
}

/// Hex digests of a regular file (links are not followed), in `algorithms` order.
pub fn checksums(path: &Path, algorithms: &[&str], cancel: Option<&dyn Cancel>) -> Result<Vec<(String, String)>> {
    let mut hashes = Vec::new();
    for name in algorithms {
        let digest = hasher(name).ok_or_else(|| Error::value(format!("unsupported hash type {name}")))?;
        hashes.push((name.to_string(), digest));
    }
    let mut stream = regular_reader(path)?;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        check(cancel)?;
        let count = stream.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        for (_, digest) in hashes.iter_mut() {
            digest.update(&buffer[..count]);
        }
    }
    Ok(hashes.into_iter().map(|(name, digest)| {
        let bytes = digest.finalize();
        (name, bytes.iter().map(|b| format!("{b:02x}")).collect())
    }).collect())
}

/// `_metadata_fd`: an `O_PATH|O_NOFOLLOW` descriptor of a regular file or folder.
/// `O_PATH` lets the owner repair mode 0000 files without first reading them.
fn metadata_fd(path: &Path) -> Result<OwnedFd> {
    let fd = open_flags(path, libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)?;
    let info = fstat(fd.as_raw_fd(), Some(path))?;
    if !(is_kind(info.st_mode, libc::S_IFREG) || is_kind(info.st_mode, libc::S_IFDIR)) {
        return Err(Error::value(tr!("Permissions can only be edited for regular files and folders")));
    }
    Ok(fd)
}

fn proc_fd(fd: &OwnedFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

pub fn set_permissions(path: &Path, mode: i64) -> Result<()> {
    if !(0..=0o7777).contains(&mode) {
        return Err(Error::value(tr!("Permissions must be an octal mode between 0000 and 7777")));
    }
    let fd = metadata_fd(path)?;
    let target = proc_fd(&fd);
    let name = fsx::cstring(&target)?;
    if unsafe { libc::chmod(name.as_ptr(), mode as libc::mode_t) } != 0 {
        return Err(Error::last_os(Some(&target)));
    }
    Ok(())
}

/// A user or group given as a number (`-1` = unchanged) or a name (decimal names are ids).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Id(i64),
    Name(String),
}
impl From<i64> for Principal {
    fn from(value: i64) -> Self {
        Principal::Id(value)
    }
}
impl From<&str> for Principal {
    fn from(value: &str) -> Self {
        Principal::Name(value.to_string())
    }
}
impl From<String> for Principal {
    fn from(value: String) -> Self {
        Principal::Name(value)
    }
}

fn resolve_principal(value: &Principal, user: bool) -> Result<i64> {
    match value {
        Principal::Id(id) => Ok(*id),
        Principal::Name(name) if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) => {
            name.parse::<i64>().map_err(|_| Error::value("invalid literal for int()"))
        }
        Principal::Name(name) => {
            let found = if user { lookup_uid(name) } else { lookup_gid(name) };
            found.map(i64::from).ok_or_else(|| {
                let function = if user { "getpwnam" } else { "getgrnam" };
                Error::key(format!("{function}(): name not found: {}", crate::error::python_repr(name)))
            })
        }
    }
}

pub fn set_owner(path: &Path, uid: Principal, gid: Principal) -> Result<()> {
    let uid = resolve_principal(&uid, true)?;
    let gid = resolve_principal(&gid, false)?;
    let fd = metadata_fd(path)?;
    let target = proc_fd(&fd);
    let name = fsx::cstring(&target)?;
    if unsafe { libc::chown(name.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) } != 0 {
        return Err(Error::last_os(Some(&target)));
    }
    Ok(())
}

pub fn acl_available() -> bool {
    suite_core::shell::which("getfacl").is_some() && suite_core::shell::which("setfacl").is_some()
}

fn read_all(mut reader: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = reader.read_to_end(&mut text);
        text
    })
}

fn acl_command(path: &Path, write: Option<&str>) -> Result<String> {
    if !acl_available() {
        return Err(Error::runtime(tr!("Install the acl package to manage access control lists")));
    }
    let fd = metadata_fd(path)?;
    let raw = fd.as_raw_fd();
    let target = proc_fd(&fd);
    let mut command = match write {
        None => {
            let mut command = Command::new("getfacl");
            command.args(["--omit-header", "--absolute-names", "--"]).arg(&target);
            command
        }
        Some(_) => {
            let mut command = Command::new("setfacl");
            command.args(["--set-file=-", "--"]).arg(&target);
            command
        }
    };
    command.stdin(if write.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    unsafe {
        // pass_fds=(fd,): keep the O_PATH descriptor open in the child.
        command.pre_exec(move || {
            if libc::fcntl(raw, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| Error::from_io(&e, None))?;
    let stdin = child.stdin.take();
    let input = write.map(str::to_string);
    let writer = std::thread::spawn(move || {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            let _ = stdin.write_all(input.as_bytes());
        }
    });
    let out_reader = read_all(child.stdout.take().expect("piped stdout"));
    let err_reader = read_all(child.stderr.take().expect("piped stderr"));
    let deadline = Instant::now() + ACL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::new(ErrorKind::Timeout,
                                      format!("Command timed out after {} seconds", ACL_TIMEOUT.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(error.into()),
        }
    };
    let _ = writer.join();
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    drop(fd);
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr);
        let message = suite_core::pyjson::py_strip(&message);
        return Err(Error::os(if message.is_empty() {
            tr!("The access control list could not be updated")
        } else {
            message.to_string()
        }));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// `getfacl --omit-header --absolute-names` of a regular file or folder.
pub fn read_acl(path: &Path) -> Result<String> {
    acl_command(path, None)
}

/// `setfacl --set-file=-` with `text`.
pub fn write_acl(path: &Path, text: &str) -> Result<String> {
    if suite_core::pyjson::py_strip(text).is_empty() || text.contains('\0') {
        return Err(Error::value(tr!("Enter a valid access control list")));
    }
    acl_command(path, Some(text))
}

#[cfg(test)]
mod tests;
