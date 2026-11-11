//! Omarchy window-management shortcuts (bindings.py): the BINDINGS table,
//! the managed rc.xml keyboard/mouse blocks and the display-only extras.
//!
//! Global Settings reads this table to list tiling shortcuts and detect
//! conflicts; [`bindings_json`] is the machine-readable export.

use crate::compat::{html_escape, shlex_join};
use std::path::Path;
use std::sync::LazyLock;

/// `(key, label, description [i18n source], command)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub key: String,
    pub label: String,
    pub description: String,
    pub command: String,
}

fn b(key: &str, label: &str, description: &str, command: &str) -> Binding {
    Binding { key: key.into(), label: label.into(), description: description.into(), command: command.into() }
}

pub static BINDINGS: LazyLock<Vec<Binding>> = LazyLock::new(|| {
    let mut list = vec![
        b("W-C-A-Tab", "Super + Ctrl + Alt + Tab", "Focus next monitor", "monitor next"),
        b("W-C-A-S-Tab", "Super + Ctrl + Alt + Shift + Tab", "Focus previous monitor", "monitor previous"),
        b("W-A-Home", "Super + Alt + Home", "Save the current window width", "width-save"),
        b("W-Home", "Super + Home", "Restore the saved window width", "width-restore"),
        b("W-w", "Super + W", "Park window (keep the application running)", "park"),
        b("W-z", "Super + Z", "Restore the most recently parked window", "restore-parked"),
        b("W-A-w", "Super + Alt + W", "Close window", "close"),
        b("W-q", "Super + Q", "Close window", "close"),
        b("W-j", "Super + J", "Toggle horizontal / vertical window positioning", "split"),
        b("W-t", "Super + T", "Toggle this window between tiled and floating", "float"),
        b("W-p", "Super + P", "Toggle natural-size window inside its tile", "pseudo"),
        b("W-f", "Super + F", "Toggle fullscreen", "fullscreen"),
        b("W-A-f", "Super + Alt + F", "Toggle maximized window (keep the panel)", "maximize"),
        b("W-C-f", "Super + Ctrl + F", "Toggle tiled fullscreen within the work area", "tiled-fullscreen"),
        b("W-o", "Super + O", "Float and show this window on every workspace", "pop"),
        b("W-A-l", "Super + Alt + L", "Switch dwindle / scrolling layout on this monitor and workspace", "layout"),
        b("W-s", "Super + S", "Show or hide scratchpad windows", "scratch-show"),
        b("W-A-s", "Super + Alt + S", "Put this window in the scratchpad", "scratch-send"),
        b("W-grave", "Super + `", "Show or hide scratchpad windows", "scratch-show"),
        b("W-S-grave", "Super + Shift + `", "Put this window in the scratchpad", "scratch-send"),
        b("W-Tab", "Super + Tab", "Next workspace", "workspace right"),
        b("W-S-Tab", "Super + Shift + Tab", "Previous workspace", "workspace left"),
        b("W-C-Tab", "Super + Ctrl + Tab", "Previous used workspace", "workspace previous"),
        b("W-g", "Super + G", "Start or dissolve a window group", "group"),
        b("W-A-g", "Super + Alt + G", "Remove this window from its group", "ungroup"),
        b("W-A-Tab", "Super + Alt + Tab", "Next window in group", "group-next"),
        b("W-A-S-Tab", "Super + Alt + Shift + Tab", "Previous window in group", "group-prev"),
        b("W-C-Left", "Super + Ctrl + Left", "Previous window in group", "group-prev"),
        b("W-C-Right", "Super + Ctrl + Right", "Next window in group", "group-next"),
    ];
    for (key, direction) in [("Left", "l"), ("Right", "r"), ("Up", "u"), ("Down", "d")] {
        let lower = key.to_lowercase();
        list.push(b(&format!("W-S-A-{key}"), &format!("Super + Shift + Alt + {key}"),
                    &format!("Move workspace windows to monitor {lower}"), &format!("workspace-monitor {direction}")));
        list.push(b(&format!("W-{key}"), &format!("Super + {key}"), &format!("Focus window {lower}"),
                    &format!("focus {direction}")));
        list.push(b(&format!("W-S-{key}"), &format!("Super + Shift + {key}"), &format!("Swap window {lower}"),
                    &format!("swap {direction}")));
        list.push(b(&format!("W-A-{key}"), &format!("Super + Alt + {key}"), &format!("Join window group {lower}"),
                    &format!("group-move {direction}")));
    }
    for number in 1..=10 {
        let key = number % 10;
        list.push(b(&format!("W-{key}"), &format!("Super + {key}"), &format!("Switch to workspace {number}"),
                    &format!("workspace {number}")));
        list.push(b(&format!("W-S-{key}"), &format!("Super + Shift + {key}"),
                    &format!("Move window to workspace {number} and follow"), &format!("send {number}")));
        list.push(b(&format!("W-S-A-{key}"), &format!("Super + Shift + Alt + {key}"),
                    &format!("Move window to workspace {number} without following"), &format!("send-silent {number}")));
        if number <= 5 {
            list.push(b(&format!("W-A-{key}"), &format!("Super + Alt + {key}"), &format!("Select window {number} in group"),
                        &format!("group-index {number}")));
        }
    }
    for (modifier, label, step) in [("", "", 100), ("A-", "Alt + ", 25), ("C-", "Ctrl + ", 300)] {
        for (key, sign) in [("minus", -1i32), ("equal", 1)] {
            let symbol = if sign < 0 { "−" } else { "=" };
            list.push(b(&format!("W-{modifier}{key}"), &format!("Super + {label}{symbol}"),
                        &format!("Change tile width by {} px", sign * step), &format!("resize x {}", sign * step)));
            list.push(b(&format!("W-{modifier}S-{key}"), &format!("Super + {label}Shift + {symbol}"),
                        &format!("Change tile height by {} px", sign * step), &format!("resize y {}", sign * step)));
        }
    }
    list
});

/// Display-only rows of the shortcut list (`EXTRA_SHORTCUTS`).
pub const EXTRA_SHORTCUTS: [(&str, &str); 8] = [
    ("Super + left drag", "Rearrange tiles or move a floating window"),
    ("Super + right drag", "Resize tiles or a floating window"),
    ("Left drag between tiles", "Resize the tiles on both sides of the gap"),
    ("Super + scroll", "Change workspace"),
    ("Super + Alt + scroll", "Change window in group"),
    ("Alt + Tab / Alt + Shift + Tab", "Tessel Shell activities / reverse activities"),
    ("Super + L / Super + Ctrl + L", "Lock screen"),
    ("Super + K", "Show this shortcut list"),
];

/// The `<keyboard>` block content (4-space indented rows).
pub fn keyboard_xml(launcher: &Path) -> String {
    let mut rows: Vec<String> = BINDINGS.iter()
        .map(|b| format!("<keybind key=\"{}\"><action name=\"Tiling\" command=\"{}\"/></keybind>", b.key, b.command))
        .collect();
    let launcher = launcher.to_string_lossy().into_owned();
    let lock = tessel_core::paths::program("tessel-shell-lock").to_string_lossy().into_owned();
    for (key, command) in [("W-k", vec![launcher.as_str(), "--settings"]), ("W-C-l", vec![lock.as_str()])] {
        rows.push(format!("<keybind key=\"{key}\"><action name=\"Execute\" command=\"{}\"/></keybind>",
                          html_escape(&shlex_join(&command))));
    }
    rows.iter().map(|row| format!("    {row}")).collect::<Vec<_>>().join("\n")
}

/// The `<mouse>` block content.
pub fn mouse_xml() -> &'static str {
    r#"    <context name="Frame">
      <mousebind button="W-Left" action="Press"><action name="Focus"/><action name="Raise"/></mousebind>
      <mousebind button="W-Left" action="Drag"><action name="Move"/></mousebind>
      <mousebind button="W-Right" action="Press"><action name="Focus"/><action name="Raise"/></mousebind>
      <mousebind button="W-Right" action="Drag"><action name="Resize"/></mousebind>
      <mousebind direction="W-Up" action="Scroll"><action name="Tiling" command="workspace left"/></mousebind>
      <mousebind direction="W-Down" action="Scroll"><action name="Tiling" command="workspace right"/></mousebind>
      <mousebind direction="W-A-Up" action="Scroll"><action name="Tiling" command="group-prev"/></mousebind>
      <mousebind direction="W-A-Down" action="Scroll"><action name="Tiling" command="group-next"/></mousebind>
    </context>"#
}

/// `json.dumps([[key, label, description, command], …])` of BINDINGS, the
/// machine-readable export for Global Settings' shortcut model.
pub fn bindings_json() -> String {
    let rows: Vec<serde_json::Value> = BINDINGS.iter()
        .map(|b| serde_json::json!([b.key, b.label, b.description, b.command]))
        .collect();
    tessel_core::pyfmt::json_dumps(&serde_json::Value::Array(rows), &tessel_core::pyfmt::JsonStyle::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn block_never_binds_close_all_or_super_l() {
        let keys: HashSet<&str> = BINDINGS.iter().map(|b| b.key.as_str()).collect();
        assert!(!keys.contains("C-A-Delete"));
        assert!(!keys.contains("W-l"));
        assert!(!BINDINGS.iter().any(|b| b.command == "close-all"));
        assert_eq!(BINDINGS.iter().find(|b| b.key == "W-A-l").unwrap().command, "layout");
        assert_eq!(keys.len(), BINDINGS.len());
        let block = keyboard_xml(Path::new("/tmp/tessel-tiling"));
        assert!(!block.contains("key=\"W-l\""));
        assert!(!block.contains("C-A-Delete"));
        assert!(block.contains("tessel-shell-lock\"/>"));
        assert!(block.contains("command=\"/tmp/tessel-tiling --settings\""));
    }
}
