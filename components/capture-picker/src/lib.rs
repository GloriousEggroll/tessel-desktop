//! Tessel Shell's on-demand GTK4 screen/window chooser for xdg-desktop-portal-wlr
//! (`chooser_type=dmenu`). Port of ~/tessel-bluetooth/tessel-shell/capture_*.py and the
//! client half of shared/capture_selection.py.

pub mod compat;
pub mod entry;
pub mod gui;
pub mod icons;
pub mod install;
pub mod output;
pub mod portal_config;
mod printable_table;
pub mod selection;
pub mod sources;
pub mod ui;
