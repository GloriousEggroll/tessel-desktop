//! Ark's libarchive plugins: `libarchive` (tar with every filter, read-write;
//! iso, cpio, deb, rpm, ar, xar, cab, lha, and unencrypted zip/7z/rar read
//! only) and `libsinglefile` (one compressed file: .gz .bz2 .xz .zst ...).
//! Editing rewrites the archive into a temporary file next to it and
//! replaces the original atomically, as Ark's libarchive plugin does.

use super::{Backend, DiskWriter};
use crate::archive::{clean_path, AddItem, Columns, CreateOptions, Ctx, Entry, Error, Listing, Result};
use crate::ffi::{self, archive, archive_entry};
use tessel_archive_settings::plugins::Kind;
use std::collections::HashSet;
use std::ffi::CString;
use std::path::{Path, PathBuf};

pub struct LibArchive {
    /// `libsinglefile`: raw format (no archive structure).
    pub single: bool,
}

fn cstring(text: &str) -> Result<CString> {
    CString::new(text).map_err(|_| Error::Failed("a name contains a NUL byte".into()))
}

fn path_cstring(path: &Path) -> Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::Failed("a path contains a NUL byte".into()))
}

fn error_of(a: *mut archive, password: Option<&str>) -> Error {
    let message = ffi::text(unsafe { ffi::archive_error_string(a) }).unwrap_or_else(|| "libarchive error".into());
    classify(&message, password)
}

pub fn classify(message: &str, password: Option<&str>) -> Error {
    let lower = message.to_lowercase();
    if lower.contains("passphrase") || lower.contains("password") {
        if lower.contains("incorrect") || lower.contains("wrong") || password.is_some() {
            if password.is_none() {
                return Error::PasswordNeeded;
            }
            return Error::WrongPassword;
        }
        return Error::PasswordNeeded;
    }
    Error::Failed(message.to_string())
}

struct Reader {
    a: *mut archive,
    password: Option<String>,
    _names: Vec<CString>,
}

impl Reader {
    fn open(volumes: &[PathBuf], raw: bool, password: Option<&str>) -> Result<Reader> {
        let a = unsafe { ffi::archive_read_new() };
        if a.is_null() {
            return Err(Error::Failed("libarchive: out of memory".into()));
        }
        let mut reader = Reader { a, password: password.map(str::to_string), _names: Vec::new() };
        unsafe {
            ffi::archive_read_support_filter_all(a);
            if raw {
                ffi::archive_read_support_format_raw(a);
            } else {
                ffi::archive_read_support_format_all(a);
                ffi::archive_read_support_format_empty(a);
            }
            if let Some(password) = password {
                let p = cstring(password)?;
                ffi::archive_read_add_passphrase(a, p.as_ptr());
            }
        }
        let names: Vec<CString> = volumes.iter().map(|p| path_cstring(p)).collect::<Result<_>>()?;
        let status = if names.len() == 1 {
            unsafe { ffi::archive_read_open_filename(a, names[0].as_ptr(), 64 * 1024) }
        } else {
            let mut pointers: Vec<*const libc::c_char> = names.iter().map(|n| n.as_ptr()).collect();
            pointers.push(std::ptr::null());
            unsafe { ffi::archive_read_open_filenames(a, pointers.as_mut_ptr(), 64 * 1024) }
        };
        reader._names = names;
        if status < ffi::ARCHIVE_WARN {
            return Err(error_of(a, password));
        }
        Ok(reader)
    }

    fn next(&mut self) -> Result<Option<*mut archive_entry>> {
        let mut entry: *mut archive_entry = std::ptr::null_mut();
        let status = unsafe { ffi::archive_read_next_header(self.a, &mut entry) };
        match status {
            ffi::ARCHIVE_EOF => Ok(None),
            s if s >= ffi::ARCHIVE_WARN => Ok(Some(entry)),
            _ => Err(error_of(self.a, self.password.as_deref())),
        }
    }

    /// Read the current entry's data in chunks.
    fn data(&mut self, ctx: &Ctx, mut sink: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let mut buffer = vec![0u8; 128 * 1024];
        loop {
            ctx.check()?;
            let n = unsafe { ffi::archive_read_data(self.a, buffer.as_mut_ptr().cast(), buffer.len()) };
            if n == 0 {
                return Ok(());
            }
            if n < 0 {
                return Err(error_of(self.a, self.password.as_deref()));
            }
            sink(&buffer[..n as usize])?;
        }
    }

    fn compressed_read(&self) -> u64 {
        unsafe { ffi::archive_filter_bytes(self.a, -1) }.max(0) as u64
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { ffi::archive_read_free(self.a) };
    }
}

pub fn entry_path(e: *mut archive_entry) -> Option<String> {
    ffi::text(unsafe { ffi::archive_entry_pathname_utf8(e) }).or_else(|| ffi::text(unsafe { ffi::archive_entry_pathname(e) }))
}

fn entry_of(e: *mut archive_entry, raw_name: Option<&str>) -> Option<Entry> {
    let raw = match raw_name {
        Some(name) => name.to_string(),
        None => entry_path(e)?,
    };
    let filetype = unsafe { ffi::archive_entry_filetype(e) } & ffi::AE_IFMT;
    let is_dir = filetype == ffi::AE_IFDIR || raw.ends_with('/');
    let path = clean_path(&raw)?;
    let link = ffi::text(unsafe { ffi::archive_entry_symlink_utf8(e) }).or_else(|| ffi::text(unsafe { ffi::archive_entry_symlink(e) }));
    let mtime = (unsafe { ffi::archive_entry_mtime_is_set(e) } != 0).then(|| unsafe { ffi::archive_entry_mtime(e) } as i64);
    let size_set = unsafe { ffi::archive_entry_size_is_set(e) } != 0;
    let perm = unsafe { ffi::archive_entry_perm(e) };
    let owner = ffi::text(unsafe { ffi::archive_entry_uname(e) })
        .or_else(|| raw_name.is_none().then(|| unsafe { ffi::archive_entry_uid(e) }.to_string()));
    let group = ffi::text(unsafe { ffi::archive_entry_gname(e) })
        .or_else(|| raw_name.is_none().then(|| unsafe { ffi::archive_entry_gid(e) }.to_string()));
    Some(Entry {
        path,
        is_dir,
        size: if size_set && !is_dir { unsafe { ffi::archive_entry_size(e) }.max(0) as u64 } else { 0 },
        mtime: mtime.filter(|t| *t > 0),
        mode: (raw_name.is_none()).then_some(perm & 0o7777),
        owner: owner.filter(|o| !o.is_empty()),
        group: group.filter(|g| !g.is_empty()),
        encrypted: unsafe { ffi::archive_entry_is_encrypted(e) } > 0,
        link: link.filter(|_| filetype == ffi::AE_IFLNK),
        ..Default::default()
    })
}

/// The name a single compressed file decompresses to (`notes.txt.gz` -> `notes.txt`).
pub fn single_file_name(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "data".into());
    let lower = name.to_lowercase();
    if let Some(kind) = Kind::from_name(&name) {
        for extension in kind.info().extensions {
            if lower.ends_with(&format!(".{extension}")) {
                let stem = &name[..name.len() - extension.len() - 1];
                if !stem.is_empty() {
                    return stem.to_string();
                }
            }
        }
    }
    format!("{name}.out")
}

pub fn filter_for(kind: Kind) -> libc::c_int {
    match kind {
        Kind::TarGz | Kind::Gz => ffi::ARCHIVE_FILTER_GZIP,
        Kind::TarBz2 | Kind::Bz2 => ffi::ARCHIVE_FILTER_BZIP2,
        Kind::TarXz | Kind::Xz => ffi::ARCHIVE_FILTER_XZ,
        Kind::TarLzma | Kind::Lzma => ffi::ARCHIVE_FILTER_LZMA,
        Kind::TarZst | Kind::Zst => ffi::ARCHIVE_FILTER_ZSTD,
        Kind::TarLz | Kind::Lz => ffi::ARCHIVE_FILTER_LZIP,
        Kind::TarLzo | Kind::Lzo => ffi::ARCHIVE_FILTER_LZOP,
        Kind::TarLz4 | Kind::Lz4 => ffi::ARCHIVE_FILTER_LZ4,
        Kind::TarLrz | Kind::Lrz => ffi::ARCHIVE_FILTER_LRZIP,
        Kind::TarZ | Kind::Z => ffi::ARCHIVE_FILTER_COMPRESS,
        _ => ffi::ARCHIVE_FILTER_NONE,
    }
}

struct Writer {
    a: *mut archive,
}

impl Writer {
    fn create(path: &Path, kind: Kind, options: &CreateOptions) -> Result<Writer> {
        let a = unsafe { ffi::archive_write_new() };
        if a.is_null() {
            return Err(Error::Failed("libarchive: out of memory".into()));
        }
        let writer = Writer { a };
        let check = |status: libc::c_int| if status < ffi::ARCHIVE_WARN { Err(error_of(a, None)) } else { Ok(()) };
        unsafe {
            if kind.is_single_file() {
                check(ffi::archive_write_set_format_raw(a))?;
            } else if kind.is_tar() {
                check(ffi::archive_write_set_format_pax_restricted(a))?;
            } else {
                return Err(Error::Unsupported(format!("libarchive cannot write {}", kind.label())));
            }
            let filter = filter_for(kind);
            if filter != ffi::ARCHIVE_FILTER_NONE {
                check(ffi::archive_write_add_filter(a, filter))?;
            }
            if let Some(level) = options.level {
                // lz4 accepts 1-9, the others 0/1-9 (zstd up to 22, but Ark offers 0-9).
                let level = if matches!(kind, Kind::Lz4 | Kind::TarLz4 | Kind::Lzo | Kind::TarLzo) { level.max(1) } else { level };
                let option = cstring(&format!("compression-level={level}"))?;
                ffi::archive_write_set_options(a, option.as_ptr());
            }
            ffi::archive_write_set_bytes_in_last_block(a, 1);
            let name = path_cstring(path)?;
            check(ffi::archive_write_open_filename(a, name.as_ptr()))?;
        }
        Ok(writer)
    }

    fn header(&self, e: *mut archive_entry) -> Result<()> {
        if unsafe { ffi::archive_write_header(self.a, e) } < ffi::ARCHIVE_WARN {
            return Err(error_of(self.a, None));
        }
        Ok(())
    }

    fn data(&self, bytes: &[u8]) -> Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            let n = unsafe { ffi::archive_write_data(self.a, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
            if n <= 0 {
                return Err(error_of(self.a, None));
            }
            offset += n as usize;
        }
        Ok(())
    }

    fn close(self) -> Result<()> {
        let status = unsafe { ffi::archive_write_close(self.a) };
        if status < ffi::ARCHIVE_WARN {
            return Err(error_of(self.a, None));
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe { ffi::archive_write_free(self.a) };
    }
}

struct EntryBox(*mut archive_entry);

impl EntryBox {
    fn new() -> EntryBox {
        EntryBox(unsafe { ffi::archive_entry_new() })
    }
}

impl Drop for EntryBox {
    fn drop(&mut self) {
        unsafe { ffi::archive_entry_free(self.0) };
    }
}

fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| ffi::text(pwd.pw_name)).flatten()
}

fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| ffi::text(grp.gr_name)).flatten()
}

/// Every archive path a list of additions writes (folders recursively).
pub fn addition_paths(items: &[AddItem]) -> Result<Vec<(PathBuf, String)>> {
    fn walk(source: &Path, dest: &str, out: &mut Vec<(PathBuf, String)>) -> Result<()> {
        let meta = std::fs::symlink_metadata(source).map_err(|e| Error::Failed(format!("{}: {e}", source.display())))?;
        out.push((source.to_path_buf(), dest.to_string()));
        if meta.is_dir() {
            let mut children: Vec<_> = std::fs::read_dir(source)?.flatten().collect();
            children.sort_by_key(|c| c.file_name());
            for child in children {
                let name = child.file_name().to_string_lossy().into_owned();
                walk(&child.path(), &format!("{dest}/{name}"), out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    for item in items {
        let dest = clean_path(&item.dest).ok_or_else(|| Error::Failed(format!("invalid archive path: {}", item.dest)))?;
        walk(&item.source, &dest, &mut out)?;
    }
    Ok(out)
}

fn write_disk_entry(writer: &Writer, source: &Path, dest: &str, ctx: &Ctx, done: &mut u64, total: u64) -> Result<()> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    ctx.check()?;
    let meta = std::fs::symlink_metadata(source).map_err(|e| Error::Failed(format!("{}: {e}", source.display())))?;
    let entry = EntryBox::new();
    let e = entry.0;
    let file_type = meta.file_type();
    unsafe {
        let name = cstring(&if file_type.is_dir() { format!("{dest}/") } else { dest.to_string() })?;
        ffi::archive_entry_set_pathname_utf8(e, name.as_ptr());
        ffi::archive_entry_set_perm(e, meta.mode() & 0o7777);
        ffi::archive_entry_set_mtime(e, meta.mtime() as libc::time_t, meta.mtime_nsec() as libc::c_long);
        ffi::archive_entry_set_uid(e, i64::from(meta.uid()));
        ffi::archive_entry_set_gid(e, i64::from(meta.gid()));
        if let Some(u) = user_name(meta.uid()) {
            let u = cstring(&u)?;
            ffi::archive_entry_set_uname_utf8(e, u.as_ptr());
        }
        if let Some(g) = group_name(meta.gid()) {
            let g = cstring(&g)?;
            ffi::archive_entry_set_gname_utf8(e, g.as_ptr());
        }
        if file_type.is_dir() {
            ffi::archive_entry_set_filetype(e, ffi::AE_IFDIR);
            ffi::archive_entry_set_size(e, 0);
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(source)?;
            let target = cstring(&target.to_string_lossy())?;
            ffi::archive_entry_set_filetype(e, ffi::AE_IFLNK);
            ffi::archive_entry_set_symlink_utf8(e, target.as_ptr());
            ffi::archive_entry_set_size(e, 0);
        } else if file_type.is_file() {
            ffi::archive_entry_set_filetype(e, ffi::AE_IFREG);
            ffi::archive_entry_set_size(e, meta.len() as i64);
        } else {
            return Ok(()); // sockets, devices, fifos: skipped like Ark
        }
    }
    writer.header(e)?;
    if file_type.is_file() {
        let mut file = std::fs::File::open(source).map_err(|e| Error::Failed(format!("{}: {e}", source.display())))?;
        let mut buffer = vec![0u8; 128 * 1024];
        loop {
            ctx.check()?;
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            writer.data(&buffer[..n])?;
            *done += n as u64;
            ctx.report(if total > 0 { *done as f64 / total as f64 } else { -1.0 }, dest);
        }
    }
    Ok(())
}

impl LibArchive {
    fn raw(&self, kind: Kind) -> bool {
        self.single || kind.is_single_file()
    }

    /// Copy the archive into a new one, mapping each path through `transform`
    /// (None drops the entry), then append `additions`; replace atomically.
    fn rewrite(&self, archive: &Path, kind: Kind, options: &CreateOptions, ctx: &Ctx,
               transform: &dyn Fn(&str) -> Option<String>, additions: &[AddItem]) -> Result<()> {
        let added = addition_paths(additions)?;
        let replaced: HashSet<&str> = added.iter().map(|(_, d)| d.as_str()).collect();
        if self.raw(kind) {
            let files: Vec<_> = added.iter().filter(|(s, _)| s.is_file()).collect();
            if archive.exists() || files.len() != 1 || added.len() != 1 {
                return Err(Error::Unsupported(tessel_core::tr!("A compressed file holds exactly one file. Choose a tar archive for several files.")));
            }
        }
        let total: u64 = added.iter().filter_map(|(s, _)| std::fs::symlink_metadata(s).ok()).filter(|m| m.is_file()).map(|m| m.len()).sum();
        let parent = archive.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let temporary = parent.join(format!(".{name}.tessel-archive-{}", crate::backend::process::random_suffix()));
        let result = (|| -> Result<()> {
            let writer = Writer::create(&temporary, kind, options)?;
            let mut done = 0u64;
            if archive.exists() {
                let mut reader = Reader::open(&[archive.to_path_buf()], false, ctx.password.as_deref())?;
                let size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
                while let Some(e) = reader.next()? {
                    ctx.check()?;
                    let Some(old) = entry_path(e).and_then(|p| clean_path(&p)) else {
                        unsafe { ffi::archive_read_data_skip(reader.a) };
                        continue;
                    };
                    let Some(new) = transform(&old).filter(|n| !replaced.contains(n.as_str())) else {
                        unsafe { ffi::archive_read_data_skip(reader.a) };
                        continue;
                    };
                    let is_dir = unsafe { ffi::archive_entry_filetype(e) } & ffi::AE_IFMT == ffi::AE_IFDIR;
                    let c = cstring(&if is_dir { format!("{new}/") } else { new.clone() })?;
                    unsafe { ffi::archive_entry_set_pathname_utf8(e, c.as_ptr()) };
                    if let Some(hard) = ffi::text(unsafe { ffi::archive_entry_hardlink(e) }).and_then(|h| clean_path(&h)) {
                        match transform(&hard) {
                            Some(target) => {
                                let t = cstring(&target)?;
                                unsafe { ffi::archive_entry_set_hardlink_utf8(e, t.as_ptr()) };
                            }
                            None => continue, // its target was deleted
                        }
                    }
                    writer.header(e)?;
                    reader.data(ctx, |bytes| writer.data(bytes))?;
                    ctx.report(if size > 0 { reader.compressed_read() as f64 / size as f64 * 0.5 } else { -1.0 }, &new);
                }
            }
            for (source, dest) in &added {
                // Folders recurse themselves in `addition_paths`; write each path once, non-recursively.
                write_disk_entry(&writer, source, dest, ctx, &mut done, total)?;
            }
            writer.close()
        })();
        match result {
            Ok(()) => {
                if let Ok(meta) = std::fs::metadata(archive) {
                    let _ = std::fs::set_permissions(&temporary, meta.permissions());
                }
                if let Ok(file) = std::fs::File::open(&temporary) {
                    let _ = file.sync_all();
                }
                std::fs::rename(&temporary, archive).map_err(|e| {
                    let _ = std::fs::remove_file(&temporary);
                    Error::Failed(format!("{}: {e}", archive.display()))
                })
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                Err(error)
            }
        }
    }
}

impl Backend for LibArchive {
    fn id(&self) -> &'static str {
        if self.single { "libsinglefile" } else { "libarchive" }
    }

    fn list(&self, archive: &Path, kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<Listing> {
        let raw = self.raw(kind);
        let mut reader = Reader::open(volumes, raw, ctx.password.as_deref())?;
        let mut listing = Listing { volumes: volumes.len() as u32, ..Default::default() };
        let raw_name = raw.then(|| single_file_name(archive));
        let size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
        let mut count = 0usize;
        while let Some(e) = reader.next()? {
            ctx.check()?;
            if let Some(mut entry) = entry_of(e, raw_name.as_deref()) {
                if raw {
                    // The raw size is unknown until the data is read.
                    let mut n = 0u64;
                    reader.data(ctx, |b| {
                        n += b.len() as u64;
                        Ok(())
                    })?;
                    entry.size = n;
                    entry.compressed = Some(size);
                    entry.mtime = std::fs::metadata(archive).ok().and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
                }
                listing.entries.push(entry);
            }
            count += 1;
            if count % 256 == 0 && size > 0 {
                ctx.report(reader.compressed_read() as f64 / size as f64, "");
            }
        }
        let format = unsafe { ffi::archive_format(reader.a) };
        if !raw && format & ffi::ARCHIVE_FORMAT_BASE_MASK == ffi::ARCHIVE_FORMAT_RAW {
            return Err(Error::Failed(tessel_core::tr!("This file is not an archive.")));
        }
        if unsafe { ffi::archive_read_has_encrypted_entries(reader.a) } > 0 {
            listing.encrypted = true;
        }
        listing.columns = Columns { permissions: !raw, owner: !raw, compressed: raw, ..Default::default() };
        let filters = unsafe { ffi::archive_filter_count(reader.a) };
        if filters > 1 {
            listing.method = ffi::text(unsafe { ffi::archive_format_name(reader.a) });
        }
        Ok(listing.finish())
    }

    fn extract(&self, archive: &Path, kind: Kind, volumes: &[PathBuf], entries: Option<&[String]>, dest: &Path, ctx: &Ctx) -> Result<()> {
        let raw = self.raw(kind);
        let mut reader = Reader::open(volumes, raw, ctx.password.as_deref())?;
        let wanted: Option<HashSet<&str>> = entries.map(|e| e.iter().map(String::as_str).collect());
        let raw_name = raw.then(|| single_file_name(archive));
        let size = volumes.iter().filter_map(|v| std::fs::metadata(v).ok()).map(|m| m.len()).sum::<u64>();
        let mut disk = DiskWriter::new(dest);
        while let Some(e) = reader.next()? {
            ctx.check()?;
            let Some(entry) = entry_of(e, raw_name.as_deref()) else { continue };
            if wanted.as_ref().is_some_and(|w| !w.contains(entry.path.as_str())) {
                continue;
            }
            ctx.report(if size > 0 { reader.compressed_read() as f64 / size as f64 } else { -1.0 }, &entry.path);
            let hard = ffi::text(unsafe { ffi::archive_entry_hardlink(e) }).and_then(|h| clean_path(&h));
            if entry.is_dir {
                disk.dir(&entry)?;
            } else if let Some(target) = &entry.link {
                disk.symlink(&entry, target)?;
            } else if let Some(hard) = hard {
                disk.hardlink(&entry, &hard)?;
            } else {
                let mut file = disk.file(&entry)?;
                reader.data(ctx, |bytes| {
                    use std::io::Write;
                    file.write_all(bytes).map_err(Error::from)
                })?;
                drop(file);
                disk.finish_file(&entry);
            }
        }
        disk.finish();
        Ok(())
    }

    fn test(&self, archive: &Path, kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<()> {
        let raw = self.raw(kind);
        let mut reader = Reader::open(volumes, raw, ctx.password.as_deref())?;
        let size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
        while let Some(e) = reader.next()? {
            let name = entry_path(e).unwrap_or_default();
            reader.data(ctx, |_| Ok(()))?;
            ctx.report(if size > 0 { reader.compressed_read() as f64 / size as f64 } else { -1.0 }, &name);
        }
        Ok(())
    }

    fn add(&self, archive: &Path, kind: Kind, items: &[AddItem], options: &CreateOptions, ctx: &Ctx) -> Result<()> {
        self.rewrite(archive, kind, options, ctx, &|p| Some(p.to_string()), items)
    }

    fn delete(&self, archive: &Path, kind: Kind, entries: &[String], ctx: &Ctx) -> Result<()> {
        if self.raw(kind) {
            return Err(Error::Unsupported(tessel_core::tr!("Entries cannot be removed from a compressed file.")));
        }
        let gone: HashSet<&str> = entries.iter().map(String::as_str).collect();
        self.rewrite(archive, kind, &CreateOptions::default(), ctx,
                     &|p| (!gone.contains(p) && !gone.iter().any(|g| p.starts_with(&format!("{g}/")))).then(|| p.to_string()), &[])
    }

    fn rename(&self, archive: &Path, kind: Kind, renames: &[(String, String)], ctx: &Ctx) -> Result<()> {
        if self.raw(kind) {
            return Err(Error::Unsupported(tessel_core::tr!("Entries cannot be renamed in a compressed file.")));
        }
        self.rewrite(archive, kind, &CreateOptions::default(), ctx, &|p| Some(super::apply_renames(p, renames)), &[])
    }
}
