//! The keybindings `tessel-configure-session` ships in rc.xml, as data, so the
//! session generator and Settings (reset to default) share one table. The
//! touchpad gestures it ships, and the gesture actions Settings offers, too,
//! and the window drag block Settings and tiling both keep last in `<mouse>`.

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

/// Which modifier moves (left drag) and resizes (right drag) windows, as Global
/// Settings offers it: "both" is labwc's Super plus the rc.xml template's Alt.
pub const WINDOW_DRAG_MODES: [&str; 3] = ["both", "super", "alt"];
/// The managed block's markers in rc.xml's `<mouse>`; the begin marker carries
/// the mode: `<!-- Tessel window drag begin: super -->`.
pub const WINDOW_DRAG_BEGIN: &str = "<!-- Tessel window drag begin";
pub const WINDOW_DRAG_END: &str = "<!-- Tessel window drag end -->";
const WINDOW_DRAG_BROKEN: &str = "The window drag block in rc.xml was edited; repair its markers first.";

/// The managed block for `mode` (None for "both", which has no block, and for
/// an unknown mode). labwc lets a later mousebind with the same context, button
/// and event replace an earlier one, and one without actions remove it, so the
/// block clears the other modifier and binds the chosen one; it must stay the
/// last thing in `<mouse>`. Rows are indented for a `<mouse>` at two spaces.
pub fn window_drag_xml(mode: &str) -> Option<String> {
    let (keep, clear) = match mode {
        "super" => ("W", "A"),
        "alt" => ("A", "W"),
        _ => return None,
    };
    let mut rows = vec![format!("{WINDOW_DRAG_BEGIN}: {mode} -->"), "    <context name=\"Frame\">".to_owned()];
    for button in ["Left", "Right"] {
        for event in ["Press", "Drag"] {
            rows.push(format!("      <mousebind button=\"{clear}-{button}\" action=\"{event}\"/>"));
        }
    }
    for (button, drag) in [("Left", "Move"), ("Right", "Resize")] {
        rows.push(format!("      <mousebind button=\"{keep}-{button}\" action=\"Press\"><action name=\"Focus\"/><action name=\"Raise\"/></mousebind>"));
        rows.push(format!("      <mousebind button=\"{keep}-{button}\" action=\"Drag\"><action name=\"{drag}\"/></mousebind>"));
    }
    rows.push("    </context>".to_owned());
    rows.push(format!("    {WINDOW_DRAG_END}"));
    Some(rows.join("\n"))
}

/// The window drag block in `text` (rc.xml's `<mouse>` section, or the whole
/// file): `(start, end, mode)` with `end` just past the end marker, or None when
/// there is none (mode "both"). Unpaired, repeated or unknown markers are an error.
pub fn window_drag_block(text: &str) -> Result<Option<(usize, usize, &'static str)>, &'static str> {
    let begins: Vec<usize> = text.match_indices(WINDOW_DRAG_BEGIN).map(|(at, _)| at).collect();
    let ends: Vec<usize> = text.match_indices(WINDOW_DRAG_END).map(|(at, _)| at).collect();
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([begin], [end]) if begin < end => {
            let rest = &text[begin + WINDOW_DRAG_BEGIN.len()..];
            let mode = rest.strip_prefix(':').and_then(|r| r.split_once("-->")).map(|(m, _)| m.trim()).unwrap_or_default();
            let mode = WINDOW_DRAG_MODES.iter().copied().find(|m| *m == mode).ok_or(WINDOW_DRAG_BROKEN)?;
            Ok(Some((*begin, end + WINDOW_DRAG_END.len(), mode)))
        }
        _ => Err(WINDOW_DRAG_BROKEN),
    }
}

#[cfg(test)]
mod window_drag_tests {
    use super::*;

    #[test]
    fn blocks_clear_the_other_modifier_then_bind_the_chosen_one() {
        assert!(window_drag_xml("both").is_none() && window_drag_xml("ctrl").is_none());
        let block = window_drag_xml("super").unwrap();
        assert_eq!(block, r#"<!-- Tessel window drag begin: super -->
    <context name="Frame">
      <mousebind button="A-Left" action="Press"/>
      <mousebind button="A-Left" action="Drag"/>
      <mousebind button="A-Right" action="Press"/>
      <mousebind button="A-Right" action="Drag"/>
      <mousebind button="W-Left" action="Press"><action name="Focus"/><action name="Raise"/></mousebind>
      <mousebind button="W-Left" action="Drag"><action name="Move"/></mousebind>
      <mousebind button="W-Right" action="Press"><action name="Focus"/><action name="Raise"/></mousebind>
      <mousebind button="W-Right" action="Drag"><action name="Resize"/></mousebind>
    </context>
    <!-- Tessel window drag end -->"#);
        let alt = window_drag_xml("alt").unwrap();
        assert!(alt.contains(r#"<mousebind button="W-Right" action="Drag"/>"#));
        assert!(alt.contains(r#"<mousebind button="A-Left" action="Drag"><action name="Move"/></mousebind>"#));
        assert!(alt.find(r#"button="W-Left" action="Press"/>"#).unwrap() < alt.find(r#"button="A-Left" action="Press">"#).unwrap());
    }

    #[test]
    fn blocks_are_found_with_their_mode() {
        for mode in ["super", "alt"] {
            let block = window_drag_xml(mode).unwrap();
            let text = format!("<mouse><default/>\n    {block}\n  </mouse>");
            let (start, end, found) = window_drag_block(&text).unwrap().unwrap();
            assert_eq!((&text[start..end], found), (block.as_str(), mode));
        }
        assert_eq!(window_drag_block("<mouse><default/></mouse>").unwrap(), None);
        assert_eq!(window_drag_block("<!-- Tessel window drag begin: both --><!-- Tessel window drag end -->").unwrap().unwrap().2, "both");
        assert_eq!(window_drag_block("<!-- Tessel window drag begin:alt--><!-- Tessel window drag end -->").unwrap().unwrap().2, "alt");
        let block = window_drag_xml("super").unwrap();
        for broken in [block.replace(WINDOW_DRAG_END, ""), block.replace("begin: super", "begin: ctrl"),
                       format!("{block}{block}"), format!("{WINDOW_DRAG_END}{WINDOW_DRAG_BEGIN}: super -->"),
                       block.replace("begin: super -->", "begin -->")] {
            assert!(window_drag_block(&broken).is_err(), "{broken}");
        }
    }
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
