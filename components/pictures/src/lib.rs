//! Rust port of the retired Python Pictures (picture_viewer.py, picture_model.py,
//! picture_canvas.py, picture_dialogs.py, picture_files.py, picture_ui.py).
//! See contracts/tessel-pictures.md.

pub mod app;
pub mod canvas;
pub mod compat;
pub mod dialogs;
pub mod exif_data;
pub mod files;
pub mod model;
pub mod ui;

pub const APPLICATION_ID: &str = "org.tessel.Pictures";

#[cfg(test)]
mod tests;
