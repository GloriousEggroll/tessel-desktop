//! Waytile: the client side of labwc's native tiling (tiling-manager).
//!
//! The layout engine lives inside labwc; this crate is the datagram client
//! (`client`, also used by wayshell's capture picker), the status stream,
//! the byte-preserving rc.xml keybinding overlay (`config`, `gaps`), the
//! focus-outline palette sync (`outline`) and the GTK shortcut popup /
//! settings controls (`ui`, GTK is only initialised there).

pub mod bindings;
pub mod cli;
pub mod client;
pub mod compat;
pub mod config;
pub mod gaps;
pub mod outline;
pub mod ui;
pub mod xml;

#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;

    /// A fresh scratch directory under `$TMPDIR` (never /tmp itself when set).
    pub fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("waytile-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }
}
