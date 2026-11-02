//! Port of `tessel-bluetooth/tessel-session-startup`: the labwc session generators
//! (`configure_session.py`, `configure_xdg_autostart.py`) as a library with identical
//! `--check`/`--apply` output, and the `desktop_suite_failed.py` crash notifier.
//! `tessel-action` stays a POSIX shell script (labwc execs it directly).

pub mod cli;
pub mod difflib;
pub mod failed;
pub mod fsops;
pub mod icon_theme;
pub mod pystr;
pub mod session;
pub mod xdg;
pub mod xml;
