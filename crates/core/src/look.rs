//! Measurements every surface of the desktop shares.

/// The radius of a rounded corner, in pixels: the one the file manager's
/// window has. Windows, the bar and its islands, menus and popups, the blur
/// behind them and the corners labwc itself draws all take it from here, so a
/// rounded corner is the same wherever it is.
///
/// A surface with "Rounded corners" switched off, a bar attached to the edge
/// of the screen and anything under a maximized window are square instead;
/// buttons and other controls inside a surface have their own, smaller radius.
pub const CORNER_RADIUS: i32 = 12;
