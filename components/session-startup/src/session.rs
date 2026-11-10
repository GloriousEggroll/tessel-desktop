//! `tessel-configure-session`: the generic labwc session (keybindings, menu, autostart,
//! environment). Every rewrite keeps the user's bytes outside the managed parts. Units,
//! `tessel-action` and the failure notifier are installed by `tools/install.py` from `data/`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use regex::{Captures, Regex};

use crate::cli::{Parsed, Parser};
use crate::fsops::{self, Snapshot};
use crate::pystr::{casefold, html_escape, html_unescape, py_strip, split_whitespace, splitlines};
use crate::xdg::{ENV_BEGIN, ENV_BLOCK, ENV_END, START_BEGIN, START_BLOCK, START_END};
use crate::xml;

pub const ACTION: &str = if tessel_core::paths::PACKAGED { "/usr/bin/tessel-action" } else { "~/.local/bin/tessel-action" };
pub const LOCK: &str = if tessel_core::paths::PACKAGED { "/usr/bin/tessel-shell-lock" } else { "~/.local/bin/tessel-shell-lock" };
/// The autostart's tiling command (the shell expands `$HOME`).
const TILING: &str = if tessel_core::paths::PACKAGED { "/usr/bin/tessel-tiling" } else { "\"$HOME/.local/bin/tessel-tiling\"" };
pub const BLOCK_BEGIN: &str = "<!-- Tessel keybindings begin";
pub const BLOCK_END: &str = "<!-- Tessel keybindings end -->";
pub const TESSEL_TILING_BEGIN: &str = "<!-- Tessel Tiling keyboard begin -->";
pub const TESSEL_TILING_END: &str = "<!-- Tessel Tiling keyboard end -->";
pub const LOCAL_AUTOSTART: &str = "autostart.local";
pub const LOCAL_ENVIRONMENT: &str = "environment.d/90-local.env";

pub type Error = String;
pub type FileEntry = (PathBuf, Vec<u8>, u32);

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
    };
}

regex!(COMMENT, r"(?s)<!--.*?-->");
regex!(KEYBIND, r"(?s)<keybind\b[^>]*?(?:/>|>.*?</keybind\s*>)");
regex!(KEYBOARD_ANY, r"(?s)<keyboard\b[^>]*?(?:/>|>.*?</keyboard\s*>)");
regex!(KEYBOARD, r"(?s)<keyboard\b[^>]*?>.*?</keyboard\s*>");
regex!(DEFAULT, r"<default\s*/>");
regex!(COMMAND_DQ, r#"(\bcommand\s*=\s*")([^"]*)(")"#);
regex!(COMMAND_SQ, r"(\bcommand\s*=\s*')([^']*)(')");
regex!(COMMAND_ELEMENT, r"(?s)(<command\s*>)(.*?)(</command\s*>)");
regex!(PROMPT, r"(?s)(<promptCommand\s*>)(.*?)(</promptCommand\s*>)");
regex!(ERROR_COMMAND, r"(?s)(<errorCommand\s*>)(.*?)(</errorCommand\s*>)");
regex!(THEME, r"(?s)<theme\b[^>]*?(?:/>|>.*?</theme\s*>)");
regex!(STATE_OPEN, r"<(active|inactive)\b[^>]*>");
regex!(ACTIVE_CLOSE, r"</active\s*>");
regex!(INACTIVE_CLOSE, r"</inactive\s*>");
regex!(RADIUS, r"<cornerRadius\b[^>]*>[^<]*</cornerRadius\s*>");
regex!(FONT, r#"(?s)<font\s+place="(ActiveWindow|InactiveWindow)"\s*>.*?</font\s*>"#);
regex!(QUOTED, r#"'[^']*'|"(?:[^"\\]|\\.)*""#);
regex!(SHELL_COMMENT, r"(?:^|\s)#.*");
regex!(SERVICE, r"\Asystemctl --user (?:start|restart) ([\w@.:-]+\.service)(?: .*)?\z");
regex!(NAME, r"\A[A-Za-z_][A-Za-z0-9_]*\z");
regex!(TEMPLATE_DESKTOP, r"(?m)^XDG_CURRENT_DESKTOP=.*\n");

fn execute(command: &str) -> String {
    format!("<action name=\"Execute\" command=\"{}\"/>", html_escape(command, true))
}

/// (key, action XML) pairs in block order; later tiling bindings override them.
pub fn keybindings() -> Vec<(String, String)> {
    let action = |name: &str| execute(&format!("{ACTION} {name}"));
    let mut rows: Vec<(String, String)> = vec![
        ("W-l".into(), execute(LOCK)),
        ("C-A-Delete".into(), action("session-menu")),
        ("W-Return".into(), action("terminal")),
        ("W-slash".into(), action("shortcuts")),
        ("XF86PowerOff".into(), action("power-button")),
        ("XF86AudioPlay".into(), action("play-pause")),
        ("XF86AudioPause".into(), action("pause")),
        ("XF86AudioNext".into(), action("next")),
        ("XF86AudioPrev".into(), action("previous")),
        ("XF86AudioStop".into(), action("stop")),
        ("XF86AudioLowerVolume".into(), action("volume-down")),
        ("XF86AudioRaiseVolume".into(), action("volume-up")),
        ("XF86AudioMute".into(), action("toggle-mute")),
        ("XF86MonBrightnessUp".into(), action("brightness-up")),
        ("XF86MonBrightnessDown".into(), action("brightness-down")),
        // Window tiling uses Super+Ctrl+Left/Right for window groups while it
        // is enabled; Ctrl+Alt+Left/Right always changes workspace.
        ("W-C-Left".into(), r#"<action name="GoToDesktop" to="left"/>"#.into()),
        ("W-C-Right".into(), r#"<action name="GoToDesktop" to="right"/>"#.into()),
        ("C-A-Left".into(), r#"<action name="GoToDesktop" to="left"/>"#.into()),
        ("C-A-Right".into(), r#"<action name="GoToDesktop" to="right"/>"#.into()),
    ];
    // labwc resolves numeric names by position, so these follow renamed and
    // added workspaces; numbers beyond the configured count do nothing.
    for number in 1..=10 {
        rows.push((format!("W-{}", number % 10), format!(r#"<action name="GoToDesktop" to="{number}"/>"#)));
    }
    for number in 1..=10 {
        rows.push((format!("W-S-{}", number % 10), format!(r#"<action name="SendToDesktop" to="{number}"/>"#)));
    }
    rows
}

/// Existing bindings for these commands are earlier suite versions which the
/// block replaces; any other binding of a block key is the user's own.
pub const SUITE_COMMANDS: [&str; 14] = [
    "tessel-shell-lock", "lab-sensible-terminal", "tessel-action", "tessel-settings", "playerctl",
    // the desktop suite's names before Tessel
    "/audio_manager.py", "/brightness_manager.py", "/session_manager.py", "/power_manager.py",
    "wayshell-lock", "desktop-suite-action", "/global-settings", "global-settings ", "global-settings\"",
];

/// Block markers written before Tessel: (old, new). `edit_rc` takes over the old block.
pub const LEGACY_MARKERS: [(&str, &str); 4] = [
    ("<!-- Desktop suite keybindings begin", BLOCK_BEGIN),
    ("<!-- Desktop suite keybindings end -->", BLOCK_END),
    ("<!-- Waytile keyboard begin -->", TESSEL_TILING_BEGIN),
    ("<!-- Waytile keyboard end -->", TESSEL_TILING_END),
];
pub const TERMINALS: [&str; 14] = [
    "ghostty", "lab-sensible-terminal", "x-terminal-emulator", "xdg-terminal-exec", "ptyxis", "kgx",
    "gnome-terminal", "konsole", "foot", "alacritty", "kitty", "wezterm", "xterm", "$TERMINAL",
];

/// Comments blanked with spaces of the same length, so offsets stay valid.
pub fn mask_comments(text: &str) -> String {
    COMMENT.replace_all(text, |caps: &Captures| " ".repeat(caps[0].len())).into_owned()
}

fn span(text: &str, begin: &str, end: &str) -> Result<Option<(usize, usize)>, Error> {
    let (begins, ends) = (text.matches(begin).count(), text.matches(end).count());
    if begins != ends || begins > 1 {
        return Err(format!("Repair the markers of the labwc keybinding block: {begin}"));
    }
    let Some(start) = text.find(begin) else { return Ok(None) };
    let finish = text.find(end).unwrap();
    if finish < start {
        return Err(format!("Reversed labwc keybinding block: {begin}"));
    }
    Ok(Some((start, finish + end.len())))
}

struct Keybind {
    start: usize,
    end: usize,
    key: String,
    fragment: String,
}

/// Keybinds in `text[start..finish]`, skipping comments; each one parsed like `ET.fromstring`.
fn keybinds(text: &str, start: usize, finish: usize) -> Result<Vec<Keybind>, Error> {
    let masked = mask_comments(text);
    let mut result = Vec::new();
    for found in KEYBIND.find_iter(&masked[start..finish]) {
        let fragment = &text[start + found.start()..start + found.end()];
        let document = xml::parse(fragment)?;
        let key = py_strip(xml::get(document.root_element(), "key").unwrap_or("")).to_owned();
        result.push(Keybind { start: start + found.start(), end: start + found.end(), key, fragment: fragment.to_owned() });
    }
    Ok(result)
}

fn is_suite_binding(fragment: &str, home: &Path) -> Result<bool, Error> {
    let document = xml::parse(fragment)?;
    let actions: Vec<_> = xml::findall(document.root_element(), "action").collect();
    if actions.is_empty() {
        return Ok(false);
    }
    let home_prefix = format!("{}/", home.display());
    for action in actions {
        let name = casefold(xml::get(action, "name").unwrap_or(""));
        if name == "gotodesktop" || name == "sendtodesktop" {
            continue;
        }
        let command = match xml::get(action, "command") {
            Some(command) if !command.is_empty() => command.to_owned(),
            _ => xml::findtext(action, "command").unwrap_or_default(),
        };
        let command = command.replace(&home_prefix, "~/");
        if name != "execute" || !SUITE_COMMANDS.iter().any(|marker| command.contains(marker)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Widen a removed element to its whole line when nothing else shares it.
fn line_span(text: &str, mut start: usize, mut finish: usize) -> (usize, usize) {
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    if py_strip(&text[line_start..start]).is_empty() {
        start = line_start;
        if let Some(offset) = text[finish..].find('\n') {
            let newline = finish + offset;
            if py_strip(&text[finish..newline]).is_empty() {
                finish = newline + 1;
            }
        }
    }
    (start, finish)
}

fn home_prefix(home: &Path) -> String {
    let home = home.display().to_string();
    format!("{}/", home.trim_end_matches('/'))
}

/// Replace the home directory in Execute commands with "~", which labwc expands.
pub fn portable_commands(text: &str, home: &Path) -> String {
    let prefix = home_prefix(home);
    let escaped = html_escape(&prefix, true);
    let attribute = |caps: &Captures| format!("{}{}{}", &caps[1], caps[2].replace(&escaped, "~/").replace(&prefix, "~/"), &caps[3]);
    let text = COMMAND_DQ.replace_all(text, attribute);
    let text = COMMAND_SQ.replace_all(&text, attribute);
    COMMAND_ELEMENT.replace_all(&text, attribute).into_owned()
}

/// promptCommand runs through sh; errorCommand runs without a shell.
pub fn portable_prompts(text: &str, home: &Path) -> String {
    let prefix = home_prefix(home);
    let text = PROMPT.replace_all(text, |caps: &Captures| {
        let content = html_unescape(&caps[2]);
        if !content.contains(&prefix) {
            return caps[0].to_owned();
        }
        format!("{}{}{}", &caps[1], html_escape(&content.replace(&prefix, "\"$HOME\"/"), false), &caps[3])
    });
    ERROR_COMMAND
        .replace_all(&text, |caps: &Captures| {
            let unescaped = html_unescape(&caps[2]);
            let content = py_strip(&unescaped);
            if !content.contains(&prefix) {
                return caps[0].to_owned();
            }
            let wrapped = format!(
                "sh -c {}",
                tessel_core::shell::shell_quote(&format!("exec {}", content.replace(&prefix, "\"$HOME\"/")))
            );
            format!("{}{}{}", &caps[1], html_escape(&wrapped, false), &caps[3])
        })
        .into_owned()
}

type Canonical = (String, Vec<(String, String)>, String);

fn canonical_node(node: xml::Node) -> Canonical {
    (xml::tag(node), xml::sorted_attributes(node), py_strip(&xml::text(node).unwrap_or_default()).to_owned())
}

/// `_canonical(action)` of an action string.
fn canonical(action: &str) -> Result<Canonical, Error> {
    let document = xml::parse(action)?;
    Ok(canonical_node(document.root_element()))
}

/// `_canonical("".join(ET.tostring(child).strip() for child in keybind))`.
fn canonical_children(fragment: &str) -> Result<Canonical, Error> {
    let document = xml::parse(fragment)?;
    let children: Vec<_> = xml::elements(document.root_element()).collect();
    match children.as_slice() {
        [] => Err("no element found: line 1, column 0".into()),
        [child] => {
            if !py_strip(&xml::tail(*child).unwrap_or_default()).is_empty() {
                return Err("junk after document element: line 1".into());
            }
            Ok(canonical_node(*child))
        }
        _ => Err("junk after document element: line 1".into()),
    }
}

fn rindex(text: &str, needle: &str, start: usize, end: usize) -> Result<usize, Error> {
    text[start..end].rfind(needle).map(|index| start + index).ok_or_else(|| "substring not found".to_owned())
}

fn index_from(text: &str, needle: &str, start: usize) -> Result<usize, Error> {
    text[start..].find(needle).map(|index| start + index).ok_or_else(|| "substring not found".to_owned())
}

fn apply_edits(mut text: String, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort();
    for (start, finish, replacement) in edits.into_iter().rev() {
        text.replace_range(start..finish, &replacement);
    }
    text
}

/// Return (rc.xml text, skipped keys) with the suite block current and portable commands.
pub fn edit_rc(text: &str, home: &Path) -> Result<(String, Vec<String>), Error> {
    if text.contains("<!DOCTYPE") || text.contains("<!ENTITY") {
        return Err("labwc XML document types and entities are not supported.".into());
    }
    {
        let document = xml::parse(text)?;
        if xml::tag(document.root_element()) != "labwc_config" {
            return Err("Expected a labwc_config document.".into());
        }
    }
    let mut text = text.to_owned();
    for (old, new) in LEGACY_MARKERS {
        text = text.replace(old, new);
    }
    let masked = mask_comments(&text);
    let sections: Vec<_> = KEYBOARD_ANY.find_iter(&masked).map(|m| (m.start(), m.end())).collect();
    if sections.len() > 1 {
        return Err("Multiple labwc keyboard sections are not supported.".into());
    }
    if sections.is_empty() {
        let end = rindex(&text, "</labwc_config>", 0, text.len())?;
        text.insert_str(end, "  <keyboard>\n    <default/>\n  </keyboard>\n");
    } else if masked[sections[0].0..sections[0].1].ends_with("/>") {
        text.replace_range(sections[0].0..sections[0].1, "<keyboard>\n    <default/>\n  </keyboard>");
    }
    let masked = mask_comments(&text);
    let section = KEYBOARD.find(&masked).ok_or("'NoneType' object has no attribute 'start'")?;
    let open_end = index_from(&masked, ">", section.start())? + 1;
    let close = rindex(&masked, "</keyboard", section.start(), section.end())?;
    let block = span(&text, BLOCK_BEGIN, BLOCK_END)?;
    let tiling = span(&text, TESSEL_TILING_BEGIN, TESSEL_TILING_END)?;
    for span in [block, tiling].into_iter().flatten() {
        if !(open_end <= span.0 && span.1 <= close) {
            return Err("A managed keybinding block is outside the keyboard section.".into());
        }
    }
    let insertion = DEFAULT.find_iter(&masked[open_end..close]).last().map_or(open_end, |m| open_end + m.end());

    // Keep keys rebound in Global Settings: match block rows by their action.
    let mut previous: HashMap<Canonical, VecDeque<String>> = HashMap::new();
    if let Some((start, finish)) = block {
        for keybind in keybinds(&text, start, finish)? {
            previous.entry(canonical_children(&keybind.fragment)?).or_default().push_back(keybind.key);
        }
    }
    let mut rows = Vec::new();
    for (key, action) in keybindings() {
        let kept = previous.get_mut(&canonical(&action)?).and_then(VecDeque::pop_front);
        rows.push((kept.unwrap_or(key), action));
    }

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut custom: HashSet<String> = HashSet::new();
    let row_keys: HashSet<String> = rows.iter().map(|(key, _)| casefold(key)).collect();
    for keybind in keybinds(&text, open_end, close)? {
        let inside = [block, tiling].into_iter().flatten().any(|(a, b)| a <= keybind.start && keybind.start < b);
        if inside || keybind.start < insertion {
            continue;
        }
        if row_keys.contains(&casefold(&keybind.key)) {
            if is_suite_binding(&keybind.fragment, home)? {
                let (start, finish) = line_span(&text, keybind.start, keybind.end);
                edits.push((start, finish, String::new()));
            } else {
                custom.insert(casefold(&keybind.key));
            }
        }
    }
    let skipped: Vec<String> = rows.iter().filter(|(key, _)| custom.contains(&casefold(key))).map(|(key, _)| key.clone()).collect();
    let mut lines = vec![
        format!("{BLOCK_BEGIN}: generated by tessel-configure-session."),
        "     Keys changed in Global Settings are kept on reinstall. -->".to_owned(),
    ];
    for (key, action) in &rows {
        if !custom.contains(&casefold(key)) {
            lines.push(format!("<keybind key=\"{}\">{action}</keybind>", html_escape(key, true)));
        }
    }
    lines.push(BLOCK_END.to_owned());
    let content = lines.join("\n    ");
    match block {
        Some((start, finish)) => edits.push((start, finish, content)),
        None => edits.push((insertion, insertion, format!("\n    {content}"))),
    }
    let text = apply_edits(text, edits);
    let text = edit_theme(&portable_prompts(&portable_commands(&text, home), home))?;
    xml::parse(&text)?;
    Ok((text, skipped))
}

pub const CORNER_RADIUS: &str = "12";
// Both title fonts match GTK header bar titles (weight 600).
const TITLE_FONT: [(&str, &str); 3] = [("name", "Sans"), ("size", "10"), ("weight", "SemiBold")];
// labwc ignores Openbox-style <active>/<inactive><titlebar color=…/> blocks;
// title bar colors come from the theme's themerc (tessel-dock/labwc_titlebar_theme.py).
const IGNORED_STATE_CHILDREN: [&str; 2] = ["titlebar", "border"];

/// (text, masked, open_end, close) for the one <theme> section, adding it if missing.
fn theme_section(text: &str) -> Result<(String, String, usize, usize), Error> {
    let mut text = text.to_owned();
    let masked = mask_comments(&text);
    match THEME.find(&masked) {
        Some(found) if found.as_str().ends_with("/>") => {
            text.replace_range(found.start()..found.end(), "<theme>\n  </theme>");
        }
        Some(_) => {}
        None => {
            let end = rindex(&text, "</labwc_config>", 0, text.len())?;
            text.insert_str(end, "  <theme>\n  </theme>\n");
        }
    }
    let masked = mask_comments(&text);
    let found = THEME.find(&masked).ok_or("'NoneType' object has no attribute 'start'")?;
    let open_end = index_from(&masked, ">", found.start())? + 1;
    let close = rindex(&masked, "</theme", found.start(), found.end())?;
    Ok((text, masked, open_end, close))
}

fn font_block(place: &str, font: &[(String, String)], indent: &str) -> String {
    let inner = format!("{indent}  ");
    let mut out = format!("<font place=\"{place}\">\n");
    for key in ["name", "size", "weight"] {
        let value = &font.iter().find(|(k, _)| k == key).unwrap().1;
        out.push_str(&format!("{inner}<{key}>{}</{key}>\n", html_escape(value, false)));
    }
    out.push_str(&format!("{indent}</font>"));
    out
}

/// `re.finditer(r"<(active|inactive)\b[^>]*>.*?</\1\s*>", text, re.S)` (regex has no backreferences).
fn state_blocks(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut position = 0;
    while position <= text.len() {
        let Some(caps) = STATE_OPEN.captures_at(text, position) else { break };
        let open = caps.get(0).unwrap();
        let closing = if &caps[1] == "active" { &*ACTIVE_CLOSE } else { &*INACTIVE_CLOSE };
        match closing.find_at(text, open.end()) {
            Some(close) => {
                found.push((open.start(), close.end()));
                position = close.end();
            }
            None => {
                position = open.start() + text[open.start()..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    found
}

/// Matching active/inactive title fonts, 12px corners, no dead Openbox title colors.
pub fn edit_theme(text: &str) -> Result<String, Error> {
    let (text, masked, open_end, close) = theme_section(text)?;
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for (start, finish) in state_blocks(&masked[open_end..close]) {
        let (start, finish) = (open_end + start, open_end + finish);
        let document = xml::parse(&text[start..finish])?;
        if xml::elements(document.root_element()).all(|child| IGNORED_STATE_CHILDREN.contains(&xml::tag(child).as_str())) {
            let (start, finish) = line_span(&text, start, finish);
            edits.push((start, finish, String::new()));
        }
    }
    match RADIUS.find(&masked[open_end..close]) {
        Some(radius) => edits.push((open_end + radius.start(), open_end + radius.end(), format!("<cornerRadius>{CORNER_RADIUS}</cornerRadius>"))),
        None => edits.push((open_end, open_end, format!("\n    <cornerRadius>{CORNER_RADIUS}</cornerRadius>"))),
    }
    let mut active = None;
    let mut inactive = None;
    for caps in FONT.captures_iter(&masked) {
        let whole = caps.get(0).unwrap();
        if &caps[1] == "ActiveWindow" {
            active = Some((whole.start(), whole.end()));
        } else {
            inactive = Some((whole.start(), whole.end()));
        }
    }
    let mut font: Vec<(String, String)> = TITLE_FONT.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let mut indent = "    ".to_owned();
    if let Some((start, finish)) = active {
        let document = xml::parse(&text[start..finish])?;
        for key in ["name", "size"] {
            let value = xml::findtext(document.root_element(), key).unwrap_or_default();
            if !py_strip(&value).is_empty() {
                font.iter_mut().find(|(k, _)| k == key).unwrap().1 = py_strip(&value).to_owned();
            }
        }
        let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
        let leading = &text[line_start..start];
        if py_strip(leading).is_empty() {
            indent = leading.to_owned();
        }
    }
    let pair = format!("{}\n{indent}{}", font_block("ActiveWindow", &font, &indent), font_block("InactiveWindow", &font, &indent));
    if let Some((start, finish)) = inactive {
        let (start, finish) = line_span(&text, start, finish);
        edits.push((start, finish, String::new()));
    }
    match active {
        Some((start, finish)) => edits.push((start, finish, pair)),
        None => edits.push((close, close, format!("  {pair}\n  "))),
    }
    Ok(apply_edits(text, edits))
}

/// Open the configured terminal from the desktop menu and use portable commands.
pub fn edit_menu(text: &str, home: &Path) -> Result<String, Error> {
    xml::parse(text)?;
    let terminal = html_escape(&format!("{ACTION} terminal"), true);
    let text = COMMAND_DQ.replace_all(text, |caps: &Captures| {
        if TERMINALS.contains(&py_strip(&html_unescape(&caps[2]))) {
            format!("{}{terminal}{}", &caps[1], &caps[3])
        } else {
            caps[0].to_owned()
        }
    });
    let text = portable_commands(&text, home);
    xml::parse(&text)?;
    Ok(text)
}

// ---- autostart ----------------------------------------------------------------

pub static AUTOSTART_TEMPLATE: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"# Tessel labwc autostart, generated by tessel-configure-session.
# Add personal startup commands to ~/.config/labwc/autostart.local instead;
# it runs at the end of this script and is never overwritten by the installers.

# Import /etc/profile(.d) settings
test -f /etc/profile && . /etc/profile

# Source user-specific profiles
test -f "$HOME/.profile" && . "$HOME/.profile"

{ENV_BLOCK}

# Move settings from before Tessel into ~/.config/tessel (once; a no-op afterwards).
for tessel_lib in "$HOME/.local/lib/tessel" /usr/local/lib/tessel /usr/lib/tessel; do
    if [ -x "$tessel_lib/tessel-migrate" ]; then
        "$tessel_lib/tessel-migrate"
        break
    fi
done

# Keep systemd's graphical session active for desktop portals. The portal
# service has Requisite=graphical-session.target, but labwc is launched
# directly rather than through a desktop-specific systemd target.
systemd-run --user --quiet --collect \
  --unit=labwc-graphical-session-anchor.service \
  --property=Type=oneshot \
  --property=RemainAfterExit=yes \
  --property=Requires=graphical-session.target \
  --property=Before=graphical-session.target \
  --property=PartOf=graphical-session.target \
  /usr/bin/true >/dev/null 2>&1

# A portal that was already running chose its backends for the desktop it was
# started in: another desktop's session, or none when an app started it before
# the environment above was imported. It then has no screen capture here.
# Nothing can be sharing this session's screen yet, so restart it.
for portal_unit in xdg-desktop-portal.service xdg-desktop-portal-wlr.service; do
    portal_pid=$(systemctl --user show --property=MainPID --value "$portal_unit" 2>/dev/null)
    case "$portal_pid" in ''|0) continue ;; esac
    for portal_variable in "XDG_CURRENT_DESKTOP=$XDG_CURRENT_DESKTOP" "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"; do
        if ! tr '\0' '\n' < "/proc/$portal_pid/environ" 2>/dev/null | grep -qxF "$portal_variable"; then
            systemctl --user stop "$portal_unit" >/dev/null 2>&1
            break
        fi
    done
done

# Required for PipeWire screen/window capture (OBS, browsers, and similar).
systemctl --user start \
  xdg-desktop-portal-wlr.service \
  xdg-desktop-portal.service >/dev/null 2>&1

# Configure monitors
systemctl --user start tessel-displays.service >/dev/null 2>&1 &

# Network and Bluetooth managers
systemctl --user start tessel-network.service >/dev/null 2>&1 &

# Desktop notifications (org.freedesktop.Notifications)
systemctl --user start tessel-notifications.service >/dev/null 2>&1 &

# Power settings owns screen locking, display-off and automatic sleep timers.
systemctl --user start tessel-power.service >/dev/null 2>&1 &
systemctl --user start tessel-battery.service >/dev/null 2>&1 &
systemctl --user start tessel-brightness.service >/dev/null 2>&1 &

# Desktop shell
systemctl --user start tessel-shell.service >/dev/null 2>&1 &

# Authentication agent
systemctl --user restart tessel-auth.service >/dev/null 2>&1 &

# Audio manager
systemctl --user start tessel-audio-loopbacks.service >/dev/null 2>&1 &

# Manual session idle inhibitor
systemctl --user start tessel-keep-awake.service >/dev/null 2>&1 &

{START_BLOCK}

# Restore optional window tiling only in the labwc session.
{TILING} --restore >/dev/null 2>&1 &

# Start the automatic touch keyboard only in the labwc session.
systemctl --user start tessel-keyboard.service >/dev/null 2>&1 &

# Personal startup commands, kept when the suite is reinstalled.
local_autostart="${{XDG_CONFIG_HOME:-$HOME/.config}}/labwc/{LOCAL_AUTOSTART}"
if [ -f "$local_autostart" ]; then
    . "$local_autostart"
fi
"#
    )
});
pub const LOCAL_AUTOSTART_HEADER: &str = "# Personal labwc startup commands. labwc's autostart sources this file last;
# Tessel installers never overwrite it.
";

fn block_end(begin: &str) -> Option<&'static str> {
    match begin {
        ENV_BEGIN => Some(ENV_END),
        START_BEGIN => Some(START_END),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Blank,
    Comment,
    Code,
}

/// Split a startup script into (blank|comment|code, text) statements.
pub fn shell_statements(text: &str) -> Vec<(Kind, String)> {
    let lines = splitlines(text, true);
    let mut result = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let stripped = py_strip(line);
        if let Some(name) = stripped.strip_prefix("# Begin ").filter(|name| !name.is_empty()) {
            let end = block_end(stripped).map(str::to_owned).unwrap_or_else(|| format!("# End {name}"));
            if let Some(finish) = (index..lines.len()).find(|&i| py_strip(lines[i]) == end) {
                result.push((Kind::Code, lines[index..=finish].concat()));
                index = finish + 1;
                continue;
            }
        }
        if stripped.is_empty() {
            result.push((Kind::Blank, line.to_owned()));
            index += 1;
            continue;
        }
        if stripped.starts_with('#') {
            result.push((Kind::Comment, line.to_owned()));
            index += 1;
            continue;
        }
        let (start, mut depth) = (index, 0i64);
        while index < lines.len() {
            depth += nesting(lines[index]);
            let continued = crate::pystr::rstrip_chars(lines[index], "\r\n").ends_with('\\');
            index += 1;
            if !continued && depth <= 0 {
                break;
            }
        }
        result.push((Kind::Code, lines[start..index].concat()));
    }
    result
}

const COMPOUND: [&str; 8] = ["case", "if", "for", "while", "until", "esac", "fi", "done"];

/// Net shell compound-command depth opened by one line (quotes and comments ignored).
pub fn nesting(line: &str) -> i64 {
    let line = QUOTED.replace_all(line, "");
    let line = SHELL_COMMENT.replace_all(&line, "");
    let boundary = |c: char| crate::pystr::is_word(c) || c == '-';
    let mut depth = 0;
    let mut previous: Option<char> = None;
    let mut position = 0;
    while position < line.len() {
        let rest = &line[position..];
        let allowed = previous.is_none_or(|c| !(boundary(c) || c == '$'));
        let word = if allowed {
            COMPOUND.iter().find(|word| rest.starts_with(**word) && !rest[word.len()..].chars().next().is_some_and(boundary))
        } else {
            None
        };
        if let Some(word) = word {
            depth += if matches!(*word, "case" | "if" | "for" | "while" | "until") { 1 } else { -1 };
            previous = word.chars().last();
            position += word.len();
            continue;
        }
        let ch = rest.chars().next().unwrap();
        previous = Some(ch);
        position += ch.len_utf8();
    }
    depth
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Block(String),
    Service(String),
    Code(String),
}

pub fn statement_key(statement: &str) -> Key {
    let text = split_whitespace(statement).join(" ");
    let first = splitlines(py_strip(statement), false).first().copied().unwrap_or("").to_owned();
    if block_end(&first).is_some() {
        return Key::Block(first);
    }
    match SERVICE.captures(&text) {
        Some(caps) => Key::Service(caps[1].to_owned()),
        None => Key::Code(text),
    }
}

/// Units and programs the desktop suite started before Tessel. Their statements are
/// the suite's, never personal: the template starts the tessel-* replacements.
pub const LEGACY_UNITS: [&str; 19] = [
    "way-displays.service",
    "netman.service", "notifications.service", "power-manager.service", "power-manager-lock.service",
    "battery-manager.service", "brightness-manager.service", "wayshell.service", "polkitman.service",
    "audio-manager-loopbacks.service", "keep-awake.service", "on-screen-keyboard.service", "waydock.service",
    "dock.service", "clipboard-manager.service", "night-light.service", "waykeyring.service",
    "network-manager.service", "authentication-manager.service",
];
pub const LEGACY_PROGRAMS: [&str; 3] = ["/.local/bin/waytile", "/.local/bin/waydock", "/waybar/scripts/"];

pub fn is_legacy_statement(statement: &str) -> bool {
    match statement_key(statement) {
        Key::Service(name) => LEGACY_UNITS.contains(&name.as_str()),
        // A package's template runs tiling from /usr/bin: an earlier ~/.local install's line is not personal.
        Key::Code(text) => LEGACY_PROGRAMS.iter().any(|program| text.contains(program))
            || (tessel_core::paths::PACKAGED && text.contains("/.local/bin/tessel-tiling")),
        Key::Block(_) => false,
    }
}

/// `local` without the suite's pre-Tessel statements (and the comments directly above
/// them), which earlier installs copied there because they were not in the template.
pub fn clean_local_autostart(local: &str) -> String {
    let mut out = String::new();
    let mut comments = String::new();
    for (kind, text) in shell_statements(local) {
        match kind {
            Kind::Comment => {
                comments.push_str(&text);
                continue;
            }
            Kind::Code if is_legacy_statement(&text) => {}
            _ => {
                out.push_str(&comments);
                out.push_str(&text);
            }
        }
        comments.clear();
    }
    out.push_str(&comments);
    // A removed statement leaves its blank separator behind: collapse runs of blank lines.
    let mut cleaned = String::new();
    let mut blank = false;
    for line in out.split_inclusive('\n') {
        let is_blank = line.trim().is_empty();
        if !(is_blank && blank) {
            cleaned.push_str(line);
        }
        blank = is_blank;
    }
    cleaned.trim_end_matches(['\n', ' ', '\t']).to_owned() + if cleaned.is_empty() { "" } else { "\n" }
}

/// (generic autostart, text to append to autostart.local).
pub fn split_autostart(current: &str, local: &str) -> (String, String) {
    let template = AUTOSTART_TEMPLATE.as_str();
    let mut known: HashSet<Key> = shell_statements(template)
        .iter()
        .chain(shell_statements(local).iter())
        .filter(|(kind, _)| *kind == Kind::Code)
        .map(|(_, text)| statement_key(text))
        .collect();
    let mut personal: Vec<String> = Vec::new();
    let mut comments: Vec<String> = Vec::new();
    for (kind, text) in shell_statements(current) {
        match kind {
            Kind::Comment => {
                comments.push(text);
                continue;
            }
            Kind::Blank => {
                comments.clear();
                continue;
            }
            Kind::Code => {}
        }
        let key = statement_key(&text);
        if !known.contains(&key) && !is_legacy_statement(&text) {
            known.insert(key);
            personal.push(comments.concat() + &text);
        }
        comments.clear();
    }
    if personal.is_empty() {
        return (template.to_owned(), String::new());
    }
    let mut prefix = if local.is_empty() { LOCAL_AUTOSTART_HEADER } else { "" };
    if !local.is_empty() && !local.ends_with('\n') {
        prefix = "\n";
    }
    (template.to_owned(), format!("{prefix}\n{}", personal.join("\n")))
}

// ---- environment --------------------------------------------------------------

pub static ENVIRONMENT_TEMPLATE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "# Tessel labwc environment, generated by tessel-configure-session.
# Personal variables belong in ~/.config/labwc/{LOCAL_ENVIRONMENT}; labwc reads
# it after this file and Tessel installers never overwrite it.
# Global Settings and display tools also keep their own lines below.

# Desktop identity used for xdg-desktop-portal backend selection.
XDG_CURRENT_DESKTOP=wlroots:labwc

# Prefer native Wayland in Firefox.
MOZ_ENABLE_WAYLAND=1

# Java applications such as JetBrains IDEs need this for correct menus.
_JAVA_AWT_WM_NONREPARENTING=1
"
    )
});
pub const LOCAL_ENVIRONMENT_HEADER: &str = "# Personal labwc environment. labwc reads this after ~/.config/labwc/environment;
# Tessel installers never overwrite it.
";
/// Blocks and variables written into the environment file by suite tools.
pub const ENVIRONMENT_BLOCKS: [(&str, &str); 2] = [
    ("# tessel-displays primary output environment begin", "# tessel-displays primary output environment end"),
    ("# Tessel language — managed by Language Settings", "# End Tessel language"),
];
pub const SETTINGS_VARIABLES: [&str; 4] = ["XDG_CURRENT_DESKTOP", "XKB_DEFAULT_LAYOUT", "XKB_DEFAULT_VARIANT", "XKB_DEFAULT_OPTIONS"];

/// `_assignment(line)`: (name, value) of `NAME=value` / `export NAME=value`.
pub fn assignment(line: &str) -> Option<(String, String)> {
    let stripped = py_strip(line);
    if stripped.is_empty() || stripped.starts_with('#') || !stripped.contains('=') {
        return None;
    }
    let (name, value) = stripped.split_once('=').unwrap();
    let mut name = py_strip(name);
    if let Some(rest) = name.strip_prefix("export ") {
        name = py_strip(rest);
    }
    NAME.is_match(name).then(|| (name.to_owned(), py_strip(value).to_owned()))
}

/// `_environment_values(text)` in first-assignment order, last value wins.
pub fn environment_values(text: &str) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = Vec::new();
    for line in splitlines(text, false) {
        if let Some((name, value)) = assignment(line) {
            match values.iter_mut().find(|(n, _)| *n == name) {
                Some(entry) => entry.1 = value,
                None => values.push((name, value)),
            }
        }
    }
    values
}

fn lookup<'a>(values: &'a [(String, String)], name: &str) -> Option<&'a str> {
    values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// (generic environment, text to append to the local env file). Effective values are unchanged.
/// Environment block markers written before Tessel: (old, new).
pub const LEGACY_ENVIRONMENT_MARKERS: [(&str, &str); 4] = [
    ("# way-displays-gui primary output environment begin", "# tessel-displays primary output environment begin"),
    ("# way-displays-gui primary output environment end", "# tessel-displays primary output environment end"),
    ("# Desktop suite language — managed by Language Settings", "# Tessel language — managed by Language Settings"),
    ("# End desktop suite language", "# End Tessel language"),
];

pub fn split_environment(current: &str, local: &str) -> (String, String) {
    let mut current = current.to_owned();
    for (old, new) in LEGACY_ENVIRONMENT_MARKERS {
        current = current.replace(old, new);
    }
    let lines = splitlines(&current, true);
    let (mut blocks, mut outside) = (Vec::<String>::new(), Vec::<&str>::new());
    let mut index = 0;
    while index < lines.len() {
        let stripped = py_strip(lines[index]);
        let pair = ENVIRONMENT_BLOCKS.iter().find(|pair| pair.0 == stripped);
        let finish = pair.and_then(|pair| (index..lines.len()).find(|&i| py_strip(lines[i]) == pair.1));
        if let Some(finish) = finish {
            blocks.push(lines[index..=finish].concat());
            index = finish + 1;
            continue;
        }
        outside.push(lines[index]);
        index += 1;
    }
    let managed: HashSet<String> = blocks.iter().flat_map(|block| environment_values(block)).map(|(n, _)| n).collect();
    let template = environment_values(&ENVIRONMENT_TEMPLATE);
    let mut last: HashMap<String, usize> = HashMap::new();
    for (number, line) in outside.iter().enumerate() {
        if let Some((name, _)) = assignment(line) {
            last.insert(name, number);
        }
    }
    let (mut kept, mut personal, mut comments) = (Vec::<String>::new(), Vec::<String>::new(), Vec::<&str>::new());
    let local_values = environment_values(local);
    for (number, line) in outside.iter().enumerate() {
        let Some((name, value)) = assignment(line) else {
            if py_strip(line).is_empty() {
                comments.clear();
            } else {
                comments.push(line);
            }
            continue;
        };
        if last[&name] == number && !managed.contains(&name) {
            if SETTINGS_VARIABLES.contains(&name.as_str()) && lookup(&template, &name) != Some(value.as_str()) {
                kept.push(format!("{name}={value}\n"));
            } else if lookup(&template, &name) != Some(value.as_str()) && lookup(&local_values, &name) != Some(value.as_str()) {
                personal.push(format!("{}{name}={value}\n", comments.concat()));
            }
        }
        comments.clear();
    }
    let mut text = ENVIRONMENT_TEMPLATE.clone();
    if kept.iter().any(|line| assignment(line).is_some_and(|(name, _)| name == "XDG_CURRENT_DESKTOP")) {
        text = TEMPLATE_DESKTOP.replace_all(&text, "").into_owned();
    }
    if !kept.is_empty() {
        text.push_str("\n# Settings written by Global Settings\n");
        text.push_str(&kept.concat());
    }
    for block in &blocks {
        if block.ends_with('\n') {
            text.push_str(&format!("\n{block}"));
        } else {
            text.push_str(&format!("\n{block}\n"));
        }
    }
    if personal.is_empty() {
        return (text, String::new());
    }
    let mut prefix = if local.is_empty() { LOCAL_ENVIRONMENT_HEADER } else { "" };
    if !local.is_empty() && !local.ends_with('\n') {
        prefix = "\n";
    }
    (text, format!("{prefix}\n{}", personal.concat()))
}

/// Complete labwc files for a user who has none. An existing file is never
/// replaced by these; it only gets the managed changes above.
const DEFAULT_RC: &str = include_str!("../../../packaging/defaults/labwc/rc.xml");
const DEFAULT_MENU: &str = include_str!("../../../packaging/defaults/labwc/menu.xml");
const NEW_USER_TILING: &str = "{\"gapless\": true, \"gapped_spacing\": 10}\n";

/// Program paths for a default file: `(labwc expands "~", a shell expands "$HOME")`.
const DEFAULT_PATHS: (&str, &str) = if tessel_core::paths::PACKAGED {
    ("/usr/bin", "/usr/bin")
} else {
    ("~/.local/bin", "\"$HOME\"/.local/bin")
};

pub fn default_file(template: &str, (bin, shell_bin): (&str, &str)) -> String {
    template.replace("@SHELL_BIN@", shell_bin).replace("@BIN@", bin)
}

/// The existing file, or the default for a user who has none. `None`: something
/// else is at that path, which is left alone.
fn labwc_file(path: &Path, template: &str) -> Result<Option<String>, Error> {
    if path.is_file() {
        return fsops::read_text(path).map(Some);
    }
    Ok((std::fs::symlink_metadata(path).is_err()).then(|| default_file(template, DEFAULT_PATHS)))
}

/// ([(path, bytes, mode)], notes) for every file this installer manages.
pub fn plan(home: &Path) -> Result<(Vec<FileEntry>, Vec<String>), Error> {
    let labwc = home.join(".config/labwc");
    let mut files = Vec::new();
    let mut notes = Vec::new();

    let rc = labwc.join("rc.xml");
    // A new user's rc.xml has no window gap and starts on the Win layout, so the
    // tiling switch has to say so. Anyone with an rc.xml keeps what they have.
    let tiling = home.join(".config/tessel/tiling/config.json");
    if std::fs::symlink_metadata(&rc).is_err() && std::fs::symlink_metadata(&tiling).is_err() {
        files.push((tiling, NEW_USER_TILING.as_bytes().to_vec(), 0o600));
    }
    if let Some(current) = labwc_file(&rc, DEFAULT_RC)? {
        let (text, skipped) = edit_rc(&current, home)?;
        files.push((rc.clone(), text.into_bytes(), fsops::mode_or(&rc, 0o644)?));
        notes.extend(skipped.iter().map(|key| format!("Kept your own binding for {key}; the suite shortcut for it is not installed.")));
    }
    let menu = labwc.join("menu.xml");
    if let Some(current) = labwc_file(&menu, DEFAULT_MENU)? {
        files.push((menu.clone(), edit_menu(&current, home)?.into_bytes(), fsops::mode_or(&menu, 0o644)?));
    }
    let read_if_exists = |path: &Path| -> Result<String, Error> {
        if path.exists() {
            fsops::read_text(path)
        } else {
            Ok(String::new())
        }
    };
    let (startup, startup_local) = (labwc.join("autostart"), labwc.join(LOCAL_AUTOSTART));
    let original_local = read_if_exists(&startup_local)?;
    let local = clean_local_autostart(&original_local);
    let (template, additions) = split_autostart(&read_if_exists(&startup)?, &local);
    files.push((startup.clone(), template.into_bytes(), fsops::mode_or(&startup, 0o755)?));
    if !additions.is_empty() || local != original_local {
        files.push((startup_local.clone(), (local + &additions).into_bytes(), fsops::mode_or(&startup_local, 0o644)?));
    }
    let (environment, environment_local) = (labwc.join("environment"), labwc.join(LOCAL_ENVIRONMENT));
    let local = read_if_exists(&environment_local)?;
    let (template, additions) = split_environment(&read_if_exists(&environment)?, &local);
    files.push((environment.clone(), template.into_bytes(), fsops::mode_or(&environment, 0o644)?));
    if !additions.is_empty() {
        files.push((environment_local.clone(), (local + &additions).into_bytes(), fsops::mode_or(&environment_local, 0o644)?));
    }
    Ok((files, notes))
}

pub struct Change {
    pub path: PathBuf,
    pub data: Vec<u8>,
    pub mode: u32,
    pub original: Snapshot,
}

pub fn changes_for(home: &Path) -> Result<(Vec<Change>, Vec<String>), Error> {
    let (files, notes) = plan(home)?;
    let mut changes = Vec::new();
    for (path, data, mode) in files {
        let original = fsops::snapshot(&path)?;
        if original.as_ref() != Some(&(data.clone(), mode)) {
            changes.push(Change { path, data, mode, original });
        }
    }
    Ok((changes, notes))
}

/// Preview text of one change (`difflib.unified_diff` of the decoded contents).
pub fn change_diff(change: &Change) -> Result<String, Error> {
    let old = match &change.original {
        Some((data, _)) => crate::pystr::decode(data)?,
        None => String::new(),
    };
    let new = crate::pystr::decode(&change.data)?;
    let path = change.path.display().to_string();
    Ok(crate::difflib::file_diff(&old, &new, &path))
}

pub const DESCRIPTION: &str = "Preview/apply Tessel's generic labwc session: keys, menu, startup and environment.

The shipped autostart and environment are generic. Lines that are not part of
them move once into files the installers preserve and never overwrite:
  ~/.config/labwc/autostart.local            sourced at the end of autostart
  ~/.config/labwc/environment.d/90-local.env read by labwc after environment
Keybindings live in one marked rc.xml block after <default/>. Keys rebound in
Global Settings are kept; a user's own binding for a key is never replaced.
Nothing is started, stopped or restarted.
";

pub fn parser(prog: &str) -> Parser {
    let usage = format!("usage: {prog} [-h] [--check | --apply]");
    let help = format!(
        "{usage}\n\n{DESCRIPTION}\noptions:\n  -h, --help  show this help message and exit\n  --check     preview only (the default)\n  --apply     write the previewed files and back up the originals\n"
    );
    Parser { usage, help, flags: &["--check", "--apply"], exclusive: true }
}

/// External commands run after `--apply` (tests inject fakes through `PATH`).
pub struct Options {
    pub home: PathBuf,
    pub daemon_reload_timeout: Duration,
    pub reconfigure_timeout: Duration,
}

impl Options {
    pub fn from_environment() -> Self {
        Self {
            home: fsops::home(),
            daemon_reload_timeout: Duration::from_secs(30),
            reconfigure_timeout: Duration::from_secs(10),
        }
    }
}

fn rollback(written: &[(PathBuf, Snapshot)]) -> Result<(), Error> {
    for (path, original) in written.iter().rev() {
        match original {
            Some((data, mode)) => fsops::atomic_write(path, data, *mode)?,
            None => fsops::unlink_missing_ok(path)?,
        }
    }
    Ok(())
}

/// `main()`: preview (default) or apply. Errors are the text Python passes to `sys.exit`.
pub fn main(args: &[String], prog: &str, options: &Options) -> Result<i32, Error> {
    let apply = match parser(prog).parse(prog, args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Flags(flags) => flags.contains("--apply"),
    };
    let home = &options.home;
    let (changes, notes) = changes_for(home)?;
    for change in &changes {
        print!("{}", change_diff(change)?);
    }
    for note in &notes {
        println!("{note}");
    }
    println!(
        "{} files prepared. Keybindings apply after labwc --reconfigure; startup and environment changes apply at the next labwc login.",
        changes.len()
    );
    if !apply || changes.is_empty() {
        return Ok(0);
    }
    let backup = home.join(".local/state/tessel/backups").join(fsops::timestamp("session-"));
    let mut written: Vec<(PathBuf, Snapshot)> = Vec::new();
    let result = (|| -> Result<(), Error> {
        for change in &changes {
            if fsops::snapshot(&change.path)? != change.original {
                return Err(format!("File changed during installation: {}", change.path.display()));
            }
            if let Some((data, mode)) = &change.original {
                fsops::atomic_write(&backup.join(fsops::relative_to(&change.path, home)?), data, *mode)?;
            }
            fsops::atomic_write(&change.path, &change.data, change.mode)?;
            written.push((change.path.clone(), change.original.clone()));
        }
        let labwc = home.join(".config/labwc");
        for name in ["autostart", LOCAL_AUTOSTART] {
            let script = labwc.join(name);
            if script.exists() {
                fsops::run(&["sh", "-n", &script.display().to_string()], true, None)?;
            }
        }
        for name in ["rc.xml", "menu.xml"] {
            let path = labwc.join(name);
            if path.exists() {
                let text = fsops::read_bytes(&path)?;
                xml::parse(&crate::pystr::decode(&text)?)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        rollback(&written)?;
        return Err(error);
    }
    // Loads the changed unit definitions; running services are not restarted.
    fsops::run(&["systemctl", "--user", "daemon-reload"], false, Some(options.daemon_reload_timeout))?;
    let labwc_pid = std::env::var("LABWC_PID").unwrap_or_default();
    if !labwc_pid.is_empty()
        && written.iter().any(|(path, _)| path.file_name().is_some_and(|n| n == "rc.xml" || n == "menu.xml"))
    {
        fsops::run(&["labwc", "--reconfigure"], false, Some(options.reconfigure_timeout))?;
    }
    println!("Installed. No applications or services were started, stopped or restarted.");
    println!("Backup: {}", backup.display());
    Ok(0)
}

#[cfg(test)]
mod new_user_tests {
    use super::*;

    fn home(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("tessel-new-user-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn file(files: &[FileEntry], name: &str) -> String {
        let (_, data, _) = files.iter().find(|(path, _, _)| path.file_name().is_some_and(|n| n == name)).unwrap();
        String::from_utf8(data.clone()).unwrap()
    }

    #[test]
    fn a_new_user_gets_complete_labwc_files() {
        let home = home("complete");
        let (files, _) = plan(&home).unwrap();
        let rc = file(&files, "rc.xml");
        let menu = file(&files, "menu.xml");
        std::fs::remove_dir_all(&home).unwrap();
        for wanted in ["key=\"A-Tab\"", "tessel-shell-cli switch-window --reverse", "key=\"Super_L\" onRelease=\"yes\"", "tessel-dock --menu-hotkey",
                       "key=\"Print\"", "tessel-capture --toggle-recording", "tessel-prompt --message-stdin", "tessel-prompt --error", "button=\"A-Left\"",
                       "menu=\"right-click-menu\"", "<cornerRadius>12</cornerRadius>", "tessel-action terminal", "key=\"W-l\"", "<name>4</name>"] {
            assert!(rc.contains(wanted), "{wanted} missing:\n{rc}");
        }
        for text in [&rc, &menu] {
            assert!(!text.contains('@'), "an unfilled path:\n{text}");
            xml::parse(text).unwrap();
        }
        assert!(menu.contains("id=\"client-menu\"") && menu.contains("id=\"right-click-menu\"") && menu.contains("tessel-dock --menu"));
    }

    #[test]
    fn defaults_use_the_paths_of_the_install_type() {
        let local = default_file(DEFAULT_RC, ("~/.local/bin", "\"$HOME\"/.local/bin"));
        assert!(local.contains("command=\"~/.local/bin/tessel-dock --menu-hotkey\""));
        assert!(local.contains("exec \"$HOME\"/.local/bin/tessel-prompt --message-stdin"));
        let packaged = default_file(DEFAULT_RC, ("/usr/bin", "/usr/bin"));
        assert!(packaged.contains("command=\"/usr/bin/tessel-dock --menu-hotkey\""));
        assert!(packaged.contains("<command>/usr/bin/tessel-capture --capture screen</command>"));
        assert!(!packaged.contains(".local") && !packaged.contains("$HOME"));
    }

    #[test]
    fn only_a_new_user_starts_without_window_gaps() {
        let home = home("gapless");
        let tiling = home.join(".config/tessel/tiling/config.json");
        let (files, _) = plan(&home).unwrap();
        assert_eq!(file(&files, "config.json"), NEW_USER_TILING);
        for (path, data, mode) in &files {
            fsops::atomic_write(path, data, *mode).unwrap();
        }
        std::fs::remove_file(&tiling).unwrap();
        let (again, _) = plan(&home).unwrap();
        std::fs::remove_dir_all(&home).unwrap();
        assert!(!again.iter().any(|(path, _, _)| *path == tiling), "someone with an rc.xml keeps their gaps");
    }

    #[test]
    fn setting_up_twice_changes_nothing() {
        let home = home("twice");
        let (files, _) = plan(&home).unwrap();
        for (path, data, mode) in &files {
            fsops::atomic_write(path, data, *mode).unwrap();
        }
        let (again, _) = plan(&home).unwrap();
        let unchanged = |name: &str| file(&files, name) == file(&again, name);
        let same = unchanged("rc.xml") && unchanged("menu.xml");
        std::fs::remove_dir_all(&home).unwrap();
        assert!(same);
    }

    #[test]
    fn an_existing_config_keeps_its_own_choices() {
        let home = home("existing");
        let labwc = home.join(".config/labwc");
        std::fs::create_dir_all(&labwc).unwrap();
        std::fs::write(labwc.join("rc.xml"), "<labwc_config><keyboard><default/><keybind key=\"Print\"><action name=\"Execute\" command=\"my-shot\"/></keybind></keyboard></labwc_config>").unwrap();
        let (files, _) = plan(&home).unwrap();
        let rc = file(&files, "rc.xml");
        std::fs::remove_dir_all(&home).unwrap();
        assert!(rc.contains("my-shot") && rc.contains("key=\"W-l\""), "{rc}");
        assert!(!rc.contains("tessel-capture") && !rc.contains("A-Tab") && !rc.contains("promptCommand"), "defaults must not be merged in:\n{rc}");
    }
}
