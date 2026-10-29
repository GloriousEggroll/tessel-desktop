//! Wayland protocol layer of the desktop suite (PLAN.md rule 7): foreign
//! toplevels (wlr v3), ext-workspace, xdg-output, ext-foreign-toplevel-list +
//! ext-image-copy-capture, and — on GTK's own wl_display — compositor blur
//! (ext-background-effect + waydock_blur_manager_v1). Replaces waydock's
//! `toplevel_helper.c` / `workspace_helper.c`, wayshell's `overview_capture.c`
//! and shared/background_blur.c. See crates/README.md.

pub mod client;
pub mod compat;
pub mod model;
#[cfg(feature = "glib")]
pub mod source;
#[cfg(feature = "gtk")]
pub mod blur;
#[cfg(feature = "gtk")]
pub mod gtk;

pub use client::{connect, ActionError, CaptureError, CaptureId, Client, ConnectError, Event, Frame, Protocols, State};
pub use model::{Model, Output, Window, Workspace, WorkspaceGroup};
#[cfg(feature = "glib")]
pub use source::GlibClient;
pub use wayland_client;
