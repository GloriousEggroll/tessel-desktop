//! The keybindings `tessel-configure-session` ships in rc.xml, as data, so the
//! session generator and Settings (reset to default) share one table.

use crate::paths::PACKAGED;

pub const ACTION: &str = if PACKAGED { "/usr/bin/tessel-action" } else { "~/.local/bin/tessel-action" };
pub const LOCK: &str = if PACKAGED { "/usr/bin/tessel-shell-lock" } else { "~/.local/bin/tessel-shell-lock" };
pub const BLOCK_BEGIN: &str = "<!-- Tessel keybindings begin";
pub const BLOCK_END: &str = "<!-- Tessel keybindings end -->";

/// (key, action name, attribute name, attribute value) in block order; later
/// tiling bindings override them.
pub fn keybindings() -> Vec<(String, &'static str, &'static str, String)> {
    let execute = |key: &str, command: String| (key.to_owned(), "Execute", "command", command);
    let action = |key: &str, name: &str| execute(key, format!("{ACTION} {name}"));
    let desktop = |key: &str, name: &'static str, to: String| (key.to_owned(), name, "to", to);
    let mut rows = vec![
        execute("W-l", LOCK.to_owned()),
        action("C-A-Delete", "session-menu"),
        action("W-Return", "terminal"),
        action("W-slash", "shortcuts"),
        action("XF86PowerOff", "power-button"),
        action("XF86AudioPlay", "play-pause"),
        action("XF86AudioPause", "pause"),
        action("XF86AudioNext", "next"),
        action("XF86AudioPrev", "previous"),
        action("XF86AudioStop", "stop"),
        action("XF86AudioLowerVolume", "volume-down"),
        action("XF86AudioRaiseVolume", "volume-up"),
        action("XF86AudioMute", "toggle-mute"),
        action("XF86MonBrightnessUp", "brightness-up"),
        action("XF86MonBrightnessDown", "brightness-down"),
        // Window tiling uses Super+Ctrl+Left/Right for window groups while it
        // is enabled; Ctrl+Alt+Left/Right always changes workspace.
        desktop("W-C-Left", "GoToDesktop", "left".into()),
        desktop("W-C-Right", "GoToDesktop", "right".into()),
        desktop("C-A-Left", "GoToDesktop", "left".into()),
        desktop("C-A-Right", "GoToDesktop", "right".into()),
    ];
    // labwc resolves numeric names by position, so these follow renamed and
    // added workspaces; numbers beyond the configured count do nothing.
    for number in 1..=10 {
        rows.push(desktop(&format!("W-{}", number % 10), "GoToDesktop", number.to_string()));
    }
    for number in 1..=10 {
        rows.push(desktop(&format!("W-S-{}", number % 10), "SendToDesktop", number.to_string()));
    }
    rows
}
