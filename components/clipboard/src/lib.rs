//! tessel-clipboard keeps the clipboard alive after the application that copied it
//! closes (Klipper's job on KDE).
//!
//! On Wayland the copying application serves every paste itself, so its copy
//! disappears with it. tessel-clipboard watches the selection with the data-control
//! protocol (ext-data-control-v1, else wlr-data-control-unstable-v1): every new
//! selection is read at once in all of its formats. While its owner lives,
//! nothing else happens (pastes still go to the application). When the owner
//! goes away and the compositor clears the selection, tessel-clipboard offers the copy
//! it read as its own selection, until something else is copied.
//!
//! [`capture`] is the protocol-independent part (which formats to read, the
//! size budget, password-manager hints); [`daemon`] drives the protocol.

pub mod capture;
pub mod daemon;

/// Offered with every selection tessel-clipboard owns, so it never reads back its own.
pub const MARKER_MIME: &str = "application/x-tessel-clipboard";
