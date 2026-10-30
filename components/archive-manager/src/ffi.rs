//! Hand-written libarchive FFI (no headers on this system; linked by soname
//! from build.rs). Only what the libarchive backend uses; constants from
//! archive.h / archive_entry.h (libarchive 3.x ABI).
#![allow(non_camel_case_types, dead_code)]

use libc::{c_char, c_int, c_uint, c_void, size_t, ssize_t, time_t};

#[repr(C)]
pub struct archive {
    _private: [u8; 0],
}
#[repr(C)]
pub struct archive_entry {
    _private: [u8; 0],
}

pub const ARCHIVE_EOF: c_int = 1;
pub const ARCHIVE_OK: c_int = 0;
pub const ARCHIVE_RETRY: c_int = -10;
pub const ARCHIVE_WARN: c_int = -20;
pub const ARCHIVE_FAILED: c_int = -25;
pub const ARCHIVE_FATAL: c_int = -30;

pub const AE_IFMT: c_uint = 0o170000;
pub const AE_IFREG: c_uint = 0o100000;
pub const AE_IFLNK: c_uint = 0o120000;
pub const AE_IFDIR: c_uint = 0o040000;

// archive_filter_code / archive_write_add_filter
pub const ARCHIVE_FILTER_NONE: c_int = 0;
pub const ARCHIVE_FILTER_GZIP: c_int = 1;
pub const ARCHIVE_FILTER_BZIP2: c_int = 2;
pub const ARCHIVE_FILTER_COMPRESS: c_int = 3;
pub const ARCHIVE_FILTER_PROGRAM: c_int = 4;
pub const ARCHIVE_FILTER_LZMA: c_int = 5;
pub const ARCHIVE_FILTER_XZ: c_int = 6;
pub const ARCHIVE_FILTER_UU: c_int = 7;
pub const ARCHIVE_FILTER_RPM: c_int = 8;
pub const ARCHIVE_FILTER_LZIP: c_int = 9;
pub const ARCHIVE_FILTER_LRZIP: c_int = 10;
pub const ARCHIVE_FILTER_LZOP: c_int = 11;
pub const ARCHIVE_FILTER_GRZIP: c_int = 12;
pub const ARCHIVE_FILTER_LZ4: c_int = 13;
pub const ARCHIVE_FILTER_ZSTD: c_int = 14;

// archive_format
pub const ARCHIVE_FORMAT_BASE_MASK: c_int = 0xff0000;
pub const ARCHIVE_FORMAT_CPIO: c_int = 0x10000;
pub const ARCHIVE_FORMAT_TAR: c_int = 0x30000;
pub const ARCHIVE_FORMAT_TAR_USTAR: c_int = 0x30001;
pub const ARCHIVE_FORMAT_TAR_PAX_INTERCHANGE: c_int = 0x30002;
pub const ARCHIVE_FORMAT_TAR_PAX_RESTRICTED: c_int = 0x30003;
pub const ARCHIVE_FORMAT_TAR_GNUTAR: c_int = 0x30004;
pub const ARCHIVE_FORMAT_ISO9660: c_int = 0x40000;
pub const ARCHIVE_FORMAT_ZIP: c_int = 0x50000;
pub const ARCHIVE_FORMAT_AR: c_int = 0x70000;
pub const ARCHIVE_FORMAT_RAW: c_int = 0x90000;
pub const ARCHIVE_FORMAT_7ZIP: c_int = 0xE0000;
pub const ARCHIVE_FORMAT_RAR: c_int = 0xD0000;
pub const ARCHIVE_FORMAT_RAR_V5: c_int = 0x100000;

pub type la_int64_t = i64;

extern "C" {
    pub fn archive_version_string() -> *const c_char;
    pub fn archive_error_string(a: *mut archive) -> *const c_char;
    pub fn archive_errno(a: *mut archive) -> c_int;
    pub fn archive_format(a: *mut archive) -> c_int;
    pub fn archive_format_name(a: *mut archive) -> *const c_char;
    pub fn archive_filter_count(a: *mut archive) -> c_int;
    pub fn archive_filter_code(a: *mut archive, n: c_int) -> c_int;
    pub fn archive_filter_bytes(a: *mut archive, n: c_int) -> la_int64_t;

    // reading
    pub fn archive_read_new() -> *mut archive;
    pub fn archive_read_free(a: *mut archive) -> c_int;
    pub fn archive_read_support_filter_all(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_all(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_raw(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_empty(a: *mut archive) -> c_int;
    pub fn archive_read_set_options(a: *mut archive, opts: *const c_char) -> c_int;
    pub fn archive_read_add_passphrase(a: *mut archive, pass: *const c_char) -> c_int;
    pub fn archive_read_open_filename(a: *mut archive, name: *const c_char, block: size_t) -> c_int;
    pub fn archive_read_open_filenames(a: *mut archive, names: *mut *const c_char, block: size_t) -> c_int;
    pub fn archive_read_next_header(a: *mut archive, entry: *mut *mut archive_entry) -> c_int;
    pub fn archive_read_data(a: *mut archive, buf: *mut c_void, size: size_t) -> ssize_t;
    pub fn archive_read_data_skip(a: *mut archive) -> c_int;
    pub fn archive_read_has_encrypted_entries(a: *mut archive) -> c_int;

    // writing
    pub fn archive_write_new() -> *mut archive;
    pub fn archive_write_free(a: *mut archive) -> c_int;
    pub fn archive_write_close(a: *mut archive) -> c_int;
    pub fn archive_write_set_format(a: *mut archive, code: c_int) -> c_int;
    pub fn archive_write_set_format_pax_restricted(a: *mut archive) -> c_int;
    pub fn archive_write_set_format_raw(a: *mut archive) -> c_int;
    pub fn archive_write_set_format_zip(a: *mut archive) -> c_int;
    pub fn archive_write_set_format_7zip(a: *mut archive) -> c_int;
    pub fn archive_write_add_filter(a: *mut archive, code: c_int) -> c_int;
    pub fn archive_write_set_options(a: *mut archive, opts: *const c_char) -> c_int;
    pub fn archive_write_set_passphrase(a: *mut archive, pass: *const c_char) -> c_int;
    pub fn archive_write_set_bytes_in_last_block(a: *mut archive, bytes: c_int) -> c_int;
    pub fn archive_write_open_filename(a: *mut archive, name: *const c_char) -> c_int;
    pub fn archive_write_header(a: *mut archive, entry: *mut archive_entry) -> c_int;
    pub fn archive_write_data(a: *mut archive, buf: *const c_void, size: size_t) -> ssize_t;
    pub fn archive_write_finish_entry(a: *mut archive) -> c_int;

    // entries
    pub fn archive_entry_new() -> *mut archive_entry;
    pub fn archive_entry_free(e: *mut archive_entry);
    pub fn archive_entry_clear(e: *mut archive_entry) -> *mut archive_entry;
    pub fn archive_entry_clone(e: *mut archive_entry) -> *mut archive_entry;
    pub fn archive_entry_pathname(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_pathname_utf8(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_set_pathname_utf8(e: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_symlink(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_symlink_utf8(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_set_symlink_utf8(e: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_hardlink(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_hardlink_utf8(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_set_hardlink_utf8(e: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_uname(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_gname(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_set_uname_utf8(e: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_set_gname_utf8(e: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_uid(e: *mut archive_entry) -> la_int64_t;
    pub fn archive_entry_gid(e: *mut archive_entry) -> la_int64_t;
    pub fn archive_entry_set_uid(e: *mut archive_entry, uid: la_int64_t);
    pub fn archive_entry_set_gid(e: *mut archive_entry, gid: la_int64_t);
    pub fn archive_entry_size(e: *mut archive_entry) -> la_int64_t;
    pub fn archive_entry_size_is_set(e: *mut archive_entry) -> c_int;
    pub fn archive_entry_set_size(e: *mut archive_entry, size: la_int64_t);
    pub fn archive_entry_filetype(e: *mut archive_entry) -> c_uint;
    pub fn archive_entry_set_filetype(e: *mut archive_entry, t: c_uint);
    pub fn archive_entry_perm(e: *mut archive_entry) -> c_uint;
    pub fn archive_entry_set_perm(e: *mut archive_entry, p: c_uint);
    pub fn archive_entry_mtime(e: *mut archive_entry) -> time_t;
    pub fn archive_entry_mtime_is_set(e: *mut archive_entry) -> c_int;
    pub fn archive_entry_set_mtime(e: *mut archive_entry, t: time_t, ns: libc::c_long);
    pub fn archive_entry_is_encrypted(e: *mut archive_entry) -> c_int;
}

/// Text of a C string (lossy), or None for NULL.
pub fn text(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned())
    }
}

pub fn version() -> String {
    text(unsafe { archive_version_string() }).unwrap_or_default()
}
