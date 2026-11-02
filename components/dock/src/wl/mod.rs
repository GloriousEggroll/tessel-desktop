//! W-b: Tessel Dock's Wayland backends (contract §4, §4.1, §5).
//!
//! - [`windows`] — `WindowBackend` (window_backend.py): foreign-toplevel
//!   windows + outputs on Tessel Dock's own `wl_display`, namespaced ids,
//!   activate/minimize/unminimize/close.
//! - [`workspaces`] — `WorkspaceBackend` (workspace_backend.py): ext-workspace
//!   state, `sorted_workspaces(output)`, `natural_name`, `activate`.
//! - [`fullscreen_guard`] — focused-fullscreen verdicts (fullscreen_guard.py):
//!   exact native state + Xwayland borderless probe (pure-Rust X11 client with
//!   a 120 ms deadline) on a worker, cached by focus signature.
//! - [`show_desktop`] — the show-desktop state machine of tessel-dock.py and the
//!   `--desktop-status` Waybar JSON.
//! - [`link`] — the in-process transport (helper line protocol rendered by
//!   tessel-wayland's model) and test fakes (`link::fake`).

pub mod link;
pub mod windows;
pub mod workspaces;
pub mod fullscreen_guard;
pub mod show_desktop;

pub use windows::{Output, Window, WindowBackend, WindowsSnapshot};
pub use workspaces::{natural_name, Workspace, WorkspaceBackend, WorkspaceGroup};
