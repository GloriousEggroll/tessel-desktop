//! Archive Manager (`wayarchive`, `org.waydock.ArchiveManager`): the suite's
//! replacement for KDE's Ark. GTK-free engine: `archive` (model), `backend`
//! (plugins: libarchive FFI, 7-Zip, Info-ZIP, The Unarchiver), `ops`
//! (operations), `cli`; GTK: `ui`.

pub mod app;
pub mod archive;
pub mod backend;
pub mod ffi;
pub mod ops;

pub use archive_settings as settings;

pub const APPLICATION_ID: &str = "org.waydock.ArchiveManager";
