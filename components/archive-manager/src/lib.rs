//! Archive Manager (`wayarchive`, `org.waydock.ArchiveManager`): the suite's
//! replacement for KDE's Ark. GTK-free engine: `archive` (model), `backend`
//! (plugins: libarchive FFI, 7-Zip, Info-ZIP, The Unarchiver), `ops`
//! (operations), `cli`, `job` (worker threads); GTK: `ui`.

pub mod archive;
pub mod backend;
pub mod cli;
pub mod ffi;
pub mod job;
pub mod ops;
pub mod ui;

pub use archive_settings as settings;
pub use ui::app;

pub const APPLICATION_ID: &str = "org.waydock.ArchiveManager";
