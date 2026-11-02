//! tessel-files: the Files application (Rust port of ~/tessel-bluetooth/tessel-files;
//! contract in contracts/tessel-files.md). The GTK-free backend lives in the
//! sibling crates `tessel-files-ops` (operations/tools/extras/preferences) and
//! `tessel-files-model` (model/launch/bus/completion/goto/devices); dialogs, previews
//! and job widgets in `tessel-files-widgets`. This crate holds the window, panes,
//! Miller columns, completion popover, Go Anywhere palette and the app.

pub mod app;
pub mod ark_drop;
pub mod bridge;
pub mod cli;
pub mod columns;
pub mod completion;
pub mod goto;
pub mod keys;
pub mod pane;
pub mod service_menu;
pub mod thumbnails;
pub mod tile;
pub mod window;

pub use app::{FilesApplication, APPLICATION_ID};
pub use pane::BrowserPane;
pub use window::{FilesWindow, FolderTab};
