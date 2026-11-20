//! MacBooks whose camera cuts into the top of the screen (the "notch"):
//! what a top bar shows in its middle would sit under it.
//!
//! [`panel_has_notch`] recognizes such a screen, [`applies`] combines that
//! with the `notch_avoidance` setting ([`CHOICES`]), and [`clear_center`]
//! moves the middle groups of a bar to its end, after the system tray (whose
//! last button is More apps) and before the session buttons.
//!
//! There is no Wayland protocol that reports a notch. The screens that have
//! one are the built-in panels of Apple machines that are taller than 16:10
//! (3024x1964, 3456x2234, 2560x1664, 2880x1864: about 1.54:1); the strip
//! beside the camera is the extra height. Asahi Linux hides that strip by
//! default and then reports a plain 16:10 screen, which needs nothing.

/// `notch_avoidance` values and their English labels.
pub const CHOICES: &[(&str, &str)] = &[("auto", "Automatic"), ("always", "Always"), ("never", "Never")];
pub const DEFAULT: &str = "auto";

/// Whether this is an Apple machine: an Apple Silicon device tree, or
/// Apple's name in the firmware tables of an Intel Mac.
pub fn apple_machine() -> bool {
    let compatible = std::fs::read("/proc/device-tree/compatible").unwrap_or_default();
    if compatible.split(|byte| *byte == 0).any(|entry| entry.starts_with(b"apple,")) {
        return true;
    }
    std::fs::read_to_string("/sys/class/dmi/id/sys_vendor").is_ok_and(|vendor| vendor.trim().starts_with("Apple"))
}

/// Whether the screen on `connector` with this size (any unit, any scale)
/// is a built-in Apple panel with a camera notch.
pub fn panel_has_notch(apple: bool, connector: &str, width: i32, height: i32) -> bool {
    if !apple || width <= 0 || height <= 0 || !connector.starts_with("eDP") {
        return false;
    }
    let ratio = width as f64 / height as f64;
    (1.52..1.57).contains(&ratio)
}

/// Whether a top bar on this screen keeps its middle clear, for the
/// `notch_avoidance` setting.
pub fn applies(setting: &str, apple: bool, connector: &str, width: i32, height: i32) -> bool {
    match setting {
        "always" => true,
        "never" => false,
        _ => panel_has_notch(apple, connector, width, height),
    }
}

/// `[start, center, end]` with the center groups moved into `end`: after the
/// system tray, else before the session buttons, else first.
pub fn clear_center(mut zones: Vec<Vec<String>>) -> Vec<Vec<String>> {
    if zones.len() != 3 || zones[1].is_empty() {
        return zones;
    }
    let moved = std::mem::take(&mut zones[1]);
    let end = &mut zones[2];
    let at = end.iter().position(|name| name == "service_apps").map(|index| index + 1)
        .or_else(|| end.iter().position(|name| name == "session"))
        .unwrap_or(0);
    end.splice(at..at, moved);
    zones
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones(start: &[&str], center: &[&str], end: &[&str]) -> Vec<Vec<String>> {
        [start, center, end].iter().map(|zone| zone.iter().map(|name| name.to_string()).collect()).collect()
    }

    #[test]
    fn only_tall_built_in_apple_panels_have_a_notch() {
        for (width, height) in [(3024, 1964), (3456, 2234), (2560, 1664), (2880, 1864), (1512, 982), (1728, 1117)] {
            assert!(panel_has_notch(true, "eDP-1", width, height), "{width}x{height}");
        }
        // The notch strip hidden (Asahi's default), and Intel MacBooks: 16:10.
        for (width, height) in [(3024, 1890), (3456, 2160), (2560, 1600), (2880, 1800)] {
            assert!(!panel_has_notch(true, "eDP-1", width, height), "{width}x{height}");
        }
        assert!(!panel_has_notch(true, "eDP-1", 1920, 1080));
        assert!(!panel_has_notch(true, "eDP-1", 2256, 1504), "3:2");
        assert!(!panel_has_notch(false, "eDP-1", 3024, 1964), "another maker's panel of that shape");
        assert!(!panel_has_notch(true, "DP-1", 3024, 1964), "an external monitor");
        assert!(!panel_has_notch(true, "HDMI-A-1", 3024, 1964));
        assert!(!panel_has_notch(true, "eDP-1", 0, 0));
    }

    #[test]
    fn the_setting_overrides_detection() {
        assert!(applies("auto", true, "eDP-1", 3024, 1964));
        assert!(!applies("auto", false, "DP-1", 3840, 2160));
        assert!(applies("always", false, "DP-1", 3840, 2160));
        assert!(!applies("never", true, "eDP-1", 3024, 1964));
        assert!(applies("unknown", true, "eDP-1", 3024, 1964), "anything else is automatic");
    }

    #[test]
    fn center_groups_go_after_the_tray_and_before_the_session_buttons() {
        // The Tessel layout.
        let tessel = zones(&["app_menu", "workspaces", "pinned", "running", "files"], &["clock", "weather"],
                           &["media", "service_apps", "session", "activities", "focused_title"]);
        assert_eq!(clear_center(tessel), zones(&["app_menu", "workspaces", "pinned", "running", "files"], &[],
            &["media", "service_apps", "clock", "weather", "session", "activities", "focused_title"]));
        // No tray in the end zone: before the session buttons.
        assert_eq!(clear_center(zones(&["service_apps"], &["clock"], &["media", "session"])),
                   zones(&["service_apps"], &[], &["media", "clock", "session"]));
        // Neither: first.
        assert_eq!(clear_center(zones(&[], &["clock"], &["media"])), zones(&[], &[], &["clock", "media"]));
        assert_eq!(clear_center(zones(&[], &["clock"], &[])), zones(&[], &[], &["clock"]));
        // Nothing in the middle: unchanged.
        let plain = zones(&["app_menu"], &[], &["service_apps", "clock", "session"]);
        assert_eq!(clear_center(plain.clone()), plain);
    }
}
