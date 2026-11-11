//! Measurements every surface of the desktop shares.

/// The radius of a rounded corner, in pixels. Windows, the bar and its
/// islands, menus and popups, the blur behind them and the corners labwc
/// itself draws all take it from here, so a rounded corner is the same
/// wherever it is. It is the radius browsers give their own windows, so a
/// Tessel window beside one has the same corners.
///
/// A surface with "Rounded corners" switched off, a bar attached to the edge
/// of the screen and anything under a maximized window are square instead.
/// Nothing inside a surface is rounder than the surface: cards and panels
/// have this radius at most, buttons and other controls a smaller one. Only
/// what is meant to be round (a pill, a dot, a switch) has more.
pub const CORNER_RADIUS: i32 = 8;
