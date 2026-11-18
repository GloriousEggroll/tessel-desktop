//! The keybindings `tessel-configure-session` ships in rc.xml, as data, so the
//! session generator and Settings (reset to default) share one table. The
//! touchpad gestures it ships, and the gesture actions Settings offers, too.

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

pub const SHELL_CLI: &str = if PACKAGED { "/usr/bin/tessel-shell-cli" } else { "~/.local/bin/tessel-shell-cli" };
pub const DOCK: &str = if PACKAGED { "/usr/bin/tessel-dock" } else { "~/.local/bin/tessel-dock" };

/// The touchpad gestures Global Settings offers, as `(type, fingers, direction)`,
/// in the order the page shows them. labwc reads them from rc.xml's `<touchpad>`.
pub const GESTURE_SLOTS: [(&str, u32, &str); 10] = [
    ("swipe", 3, "left"), ("swipe", 3, "right"), ("swipe", 3, "up"), ("swipe", 3, "down"),
    ("swipe", 4, "left"), ("swipe", 4, "right"), ("swipe", 4, "up"), ("swipe", 4, "down"),
    ("pinch", 4, "in"), ("pinch", 4, "out"),
];

/// The ids of the gesture actions Global Settings offers, besides "none"
/// (no `<gesture>` element: the gesture goes to the application).
pub const GESTURE_CHOICES: [&str; 10] = [
    "next-workspace", "previous-workspace", "overview", "app-menu", "show-desktop", "maximize", "minimize", "fullscreen",
    "close", "lock",
];

/// The labwc action of a gesture choice: (action name, attributes in order).
pub fn gesture_action(id: &str) -> Option<(&'static str, Vec<(&'static str, String)>)> {
    let desktop = |to: &str| ("GoToDesktop", vec![("to", to.to_owned()), ("wrap", "no".to_owned())]);
    let execute = |command: String| ("Execute", vec![("command", command)]);
    Some(match id {
        // Fingers moving left bring in the workspace on the right, as on a phone.
        "next-workspace" => desktop("right"),
        "previous-workspace" => desktop("left"),
        "overview" => execute(format!("{SHELL_CLI} show-activities")),
        "app-menu" => execute(format!("{DOCK} --menu-hotkey")),
        "show-desktop" => ("ToggleShowDesktop", Vec::new()),
        "maximize" => ("ToggleMaximize", Vec::new()),
        "minimize" => ("Iconify", Vec::new()),
        "fullscreen" => ("ToggleFullscreen", Vec::new()),
        "close" => ("Close", Vec::new()),
        "lock" => execute(LOCK.to_owned()),
        _ => return None,
    })
}

/// The gestures `tessel-configure-session` writes into an rc.xml that has no
/// `<touchpad>` section yet: (type, fingers, direction, choice id). Three and
/// four fingers do the same; pinches are left to applications.
pub fn default_gestures() -> Vec<(&'static str, u32, &'static str, &'static str)> {
    let mut rows = Vec::new();
    for fingers in [3, 4] {
        for (direction, id) in [("left", "next-workspace"), ("right", "previous-workspace"), ("up", "overview"), ("down", "show-desktop")] {
            rows.push(("swipe", fingers, direction, id));
        }
    }
    rows
}

fn escape_attribute(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// One `<gesture>` element of a choice, as rc.xml has it.
pub fn gesture_xml(kind: &str, fingers: u32, direction: &str, id: &str) -> Option<String> {
    let (name, attributes) = gesture_action(id)?;
    let attributes: String = attributes.iter().map(|(key, value)| format!(" {key}=\"{}\"", escape_attribute(value))).collect();
    Some(format!("<gesture type=\"{kind}\" fingers=\"{fingers}\" direction=\"{direction}\"><action name=\"{name}\"{attributes}/></gesture>"))
}

#[cfg(test)]
mod gesture_tests {
    use super::*;

    #[test]
    fn every_choice_and_default_has_an_action() {
        for id in GESTURE_CHOICES {
            assert!(gesture_action(id).is_some(), "{id}");
        }
        assert!(gesture_action("none").is_none());
        for (kind, fingers, direction, id) in default_gestures() {
            assert!(GESTURE_SLOTS.contains(&(kind, fingers, direction)));
            assert!(GESTURE_CHOICES.contains(&id));
        }
        assert_eq!(default_gestures().len(), 8);
        assert_eq!(gesture_xml("swipe", 3, "left", "next-workspace").unwrap(),
                   r#"<gesture type="swipe" fingers="3" direction="left"><action name="GoToDesktop" to="right" wrap="no"/></gesture>"#);
        assert_eq!(gesture_xml("swipe", 4, "up", "overview").unwrap(),
                   format!(r#"<gesture type="swipe" fingers="4" direction="up"><action name="Execute" command="{SHELL_CLI} show-activities"/></gesture>"#));
        assert_eq!(gesture_xml("pinch", 4, "in", "show-desktop").unwrap(),
                   r#"<gesture type="pinch" fingers="4" direction="in"><action name="ToggleShowDesktop"/></gesture>"#);
    }
}
