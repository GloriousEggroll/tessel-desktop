//! Tessel's session startup: the labwc session generators (`tessel-configure-session`,
//! `tessel-xdg-autostart`), the one-time `tessel-migrate` from the desktop-suite layout,
//! and the `tessel-failed` crash notifier. `tessel-action` is a POSIX shell script
//! (`data/bin/`; labwc execs it directly).

pub mod cli;
pub mod difflib;
pub mod failed;
pub mod fsops;
pub mod icon_theme;
pub mod migrate;
pub mod pystr;
pub mod session;
pub mod xdg;
pub mod xml;
