//! Theme & Wallpaper → Fonts: the interface, document and monospace fonts
//! (gsettings `org.gnome.desktop.interface` `font-name`,
//! `document-font-name`, `monospace-font-name`; `gtk-font-name` in
//! `~/.config/gtk-{3,4}.0/settings.ini`) and the window title font in
//! labwc's rc.xml (`<theme><font place="ActiveWindow">` and
//! `place="InactiveWindow"`). Every path, the settings object and the labwc
//! reload are injected so tests never touch the real session.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gs_base::SettingsPage;
use gtk::pango;
use gtk::prelude::*;
use tessel_core::icon_theme::{gtk_ini_paths, ini_get, ini_set, INI_SECTION};
use tessel_core::tr;

use crate::util::{accessible_label, label};

/// GTK `settings.ini` key of the interface font.
pub const INI_KEY: &str = "gtk-font-name";
/// What labwc uses when rc.xml names no font (rcxml.c `init_font_defaults`).
pub const LABWC_DEFAULT_NAME: &str = "sans";
pub const LABWC_DEFAULT_SIZE: i32 = 10;
/// The places this page sets: both window title bars.
pub const TITLE_PLACES: [&str; 2] = ["ActiveWindow", "InactiveWindow"];
/// labwc's weight keywords and the Pango weights they stand for.
const WEIGHTS: [(&str, i32); 12] = [("thin", 100), ("ultralight", 200), ("light", 300), ("semilight", 350), ("book", 380),
    ("normal", 400), ("medium", 500), ("semibold", 600), ("bold", 700), ("ultrabold", 800), ("heavy", 900), ("ultraheavy", 1000)];
/// The monospace sample: characters that look alike in other fonts.
pub const MONOSPACE_SAMPLE: &str = "0O 1lI {} [] () => 0123456789";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontRole {
    Interface,
    Document,
    Monospace,
    Titlebar,
}

impl FontRole {
    pub const ALL: [FontRole; 4] = [FontRole::Interface, FontRole::Document, FontRole::Monospace, FontRole::Titlebar];

    /// The gsettings key, or `None` for the window titles (rc.xml).
    pub fn gsettings_key(self) -> Option<&'static str> {
        match self {
            FontRole::Interface => Some("font-name"),
            FontRole::Document => Some("document-font-name"),
            FontRole::Monospace => Some("monospace-font-name"),
            FontRole::Titlebar => None,
        }
    }

    /// Search target name.
    pub fn target(self) -> &'static str {
        match self {
            FontRole::Interface => "font_interface",
            FontRole::Document => "font_document",
            FontRole::Monospace => "font_monospace",
            FontRole::Titlebar => "font_titlebar",
        }
    }

    pub fn title(self) -> String {
        match self {
            FontRole::Interface => tr!("Interface text"),
            FontRole::Document => tr!("Document text"),
            FontRole::Monospace => tr!("Monospace text"),
            FontRole::Titlebar => tr!("Window titles"),
        }
    }
}

// ------------------------------------------------------------ labwc fonts

/// The window title font `tessel-configure-session` writes into rc.xml
/// (session.rs `TITLE_FONT`, like GTK header bar titles).
pub fn tessel_title_font() -> LabwcFont {
    LabwcFont { name: "Sans".into(), size: 10, slant: "normal".into(), weight: "semibold".into() }
}

/// Whether labwc draws both fonts alike (it ignores case in these values).
pub fn same_font(a: &LabwcFont, b: &LabwcFont) -> bool {
    a.name.eq_ignore_ascii_case(&b.name) && a.size == b.size && a.slant.eq_ignore_ascii_case(&b.slant)
        && a.weight.eq_ignore_ascii_case(&b.weight)
}

/// One labwc `<font>`: `<name>`, `<size>` (whole points), `<slant>`, `<weight>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabwcFont {
    pub name: String,
    pub size: i32,
    pub slant: String,
    pub weight: String,
}

impl Default for LabwcFont {
    fn default() -> Self {
        Self { name: LABWC_DEFAULT_NAME.into(), size: LABWC_DEFAULT_SIZE, slant: "normal".into(), weight: "normal".into() }
    }
}

/// The labwc font for a Pango description such as "Fira Code Bold 11".
/// labwc reads whole sizes only (`atoi`); a missing size is labwc's 10.
pub fn labwc_font(description: &str) -> LabwcFont {
    let desc = pango::FontDescription::from_string(description);
    let name = desc.family().map(|f| f.to_string()).filter(|f| !f.is_empty()).unwrap_or_else(|| LABWC_DEFAULT_NAME.into());
    let size = match desc.size() {
        0 => LABWC_DEFAULT_SIZE,
        size => ((size as f64 / pango::SCALE as f64).round() as i32).max(1),
    };
    let slant = match desc.style() {
        pango::Style::Italic => "italic",
        pango::Style::Oblique => "oblique",
        _ => "normal",
    };
    let wanted = weight_value(desc.weight());
    // The nearest keyword; ties go to the lighter one.
    let weight = WEIGHTS.iter().min_by_key(|(_, value)| (value - wanted).abs()).map(|(name, _)| *name).unwrap_or("normal");
    LabwcFont { name, size, slant: slant.into(), weight: weight.into() }
}

fn weight_value(weight: pango::Weight) -> i32 {
    match weight {
        pango::Weight::Thin => 100,
        pango::Weight::Ultralight => 200,
        pango::Weight::Light => 300,
        pango::Weight::Semilight => 350,
        pango::Weight::Book => 380,
        pango::Weight::Normal => 400,
        pango::Weight::Medium => 500,
        pango::Weight::Semibold => 600,
        pango::Weight::Bold => 700,
        pango::Weight::Ultrabold => 800,
        pango::Weight::Heavy => 900,
        pango::Weight::Ultraheavy => 1000,
        pango::Weight::__Unknown(value) => value,
        _ => 400,
    }
}

/// The Pango description of a labwc font (unknown slants and weights are
/// normal, as labwc reads them).
pub fn pango_description(font: &LabwcFont) -> String {
    let mut desc = pango::FontDescription::new();
    desc.set_family(&font.name);
    desc.set_size(font.size.max(1) * pango::SCALE);
    desc.set_style(match font.slant.to_ascii_lowercase().as_str() {
        "italic" => pango::Style::Italic,
        "oblique" => pango::Style::Oblique,
        _ => pango::Style::Normal,
    });
    let weight = WEIGHTS.iter().find(|(name, _)| name.eq_ignore_ascii_case(&font.weight)).map(|(_, v)| *v).unwrap_or(400);
    desc.set_weight(match weight {
        100 => pango::Weight::Thin,
        200 => pango::Weight::Ultralight,
        300 => pango::Weight::Light,
        350 => pango::Weight::Semilight,
        380 => pango::Weight::Book,
        500 => pango::Weight::Medium,
        600 => pango::Weight::Semibold,
        700 => pango::Weight::Bold,
        800 => pango::Weight::Ultrabold,
        900 => pango::Weight::Heavy,
        1000 => pango::Weight::Ultraheavy,
        _ => pango::Weight::Normal,
    });
    desc.to_str().to_string()
}

// ------------------------------------------------------------ rc.xml scan

/// One element of rc.xml with its byte offsets.
#[derive(Clone, Debug)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    start: usize,
    open_end: usize,
    /// Start and end of the closing tag; `None` for `<x/>`.
    close: Option<(usize, usize)>,
    parent: Option<usize>,
}

impl Element {
    fn end(&self) -> usize {
        self.close.map(|c| c.1).unwrap_or(self.open_end)
    }

    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

const INVALID: &str = "rc.xml is not valid XML.";

fn skip_to(text: &str, from: usize, needle: &str) -> Result<usize, String> {
    text[from..].find(needle).map(|i| from + i + needle.len()).ok_or_else(|| INVALID.to_owned())
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let value = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") => u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32),
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match value {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn parse_attributes(raw: &str) -> Result<Vec<(String, String)>, String> {
    let mut attributes = Vec::new();
    let mut rest = raw.trim_start();
    while !rest.is_empty() {
        let equals = rest.find('=').ok_or_else(|| INVALID.to_owned())?;
        let name = rest[..equals].trim().to_owned();
        let after = rest[equals + 1..].trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'').ok_or_else(|| INVALID.to_owned())?;
        let end = after[1..].find(quote).ok_or_else(|| INVALID.to_owned())? + 1;
        attributes.push((name, unescape(&after[1..end])));
        rest = after[end + 1..].trim_start();
    }
    Ok(attributes)
}

/// Every element of an XML document in document order (comments, CDATA,
/// processing instructions and DOCTYPE are skipped).
fn scan(text: &str) -> Result<Vec<Element>, String> {
    let bytes = text.as_bytes();
    let mut elements: Vec<Element> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut index = 0;
    while let Some(offset) = text[index..].find('<') {
        let start = index + offset;
        let rest = &text[start..];
        if rest.starts_with("<!--") {
            index = skip_to(text, start + 4, "-->")?;
        } else if rest.starts_with("<![CDATA[") {
            index = skip_to(text, start + 9, "]]>")?;
        } else if rest.starts_with("<?") {
            index = skip_to(text, start + 2, "?>")?;
        } else if rest.starts_with("<!") {
            index = skip_to(text, start + 2, ">")?;
        } else if rest.starts_with("</") {
            let end = skip_to(text, start + 2, ">")?;
            let name = text[start + 2..end - 1].trim();
            let current = open.pop().ok_or_else(|| INVALID.to_owned())?;
            if elements[current].name != name {
                return Err(INVALID.into());
            }
            elements[current].close = Some((start, end));
            index = end;
        } else {
            let mut quote = None;
            let mut end = None;
            for (position, byte) in bytes.iter().enumerate().skip(start + 1) {
                match quote {
                    Some(q) if *byte == q => quote = None,
                    Some(_) => {}
                    None if *byte == b'"' || *byte == b'\'' => quote = Some(*byte),
                    None if *byte == b'>' => {
                        end = Some(position + 1);
                        break;
                    }
                    None => {}
                }
            }
            let end = end.ok_or_else(|| INVALID.to_owned())?;
            let inner = &text[start + 1..end - 1];
            let empty = inner.ends_with('/');
            let inner = inner.strip_suffix('/').unwrap_or(inner);
            let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
            let name = inner[..name_end].to_owned();
            if name.is_empty() {
                return Err(INVALID.into());
            }
            let attributes = parse_attributes(&inner[name_end..])?;
            elements.push(Element { name, attributes, start, open_end: end, close: None, parent: open.last().copied() });
            if !empty {
                open.push(elements.len() - 1);
            }
            index = end;
        }
    }
    if !open.is_empty() {
        return Err(INVALID.into());
    }
    Ok(elements)
}

/// The text of an element without its markup (comments dropped, CDATA kept).
fn inner_text(text: &str, element: &Element) -> String {
    let Some((close, _)) = element.close else { return String::new() };
    let raw = &text[element.open_end..close];
    let mut out = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find('<') {
        out.push_str(&unescape(&rest[..start]));
        let tail = &rest[start..];
        if let Some(body) = tail.strip_prefix("<![CDATA[") {
            let end = body.find("]]>").unwrap_or(body.len());
            out.push_str(&body[..end]);
            rest = body.get(end + 3..).unwrap_or("");
        } else {
            let end = if tail.starts_with("<!--") { tail.find("-->").map(|e| e + 3) } else { tail.find('>').map(|e| e + 1) };
            rest = &tail[end.unwrap_or(tail.len())..];
        }
    }
    out.push_str(&unescape(rest));
    out.trim().to_owned()
}

fn children(elements: &[Element], parent: usize) -> impl Iterator<Item = (usize, &Element)> {
    elements.iter().enumerate().filter(move |(_, e)| e.parent == Some(parent))
}

fn root_and_theme(elements: &[Element]) -> Result<(usize, Option<usize>), String> {
    let root = elements.iter().position(|e| e.parent.is_none()).ok_or("rc.xml has no labwc_config element")?;
    let theme = children(elements, root).find(|(_, e)| e.is("theme")).map(|(i, _)| i);
    Ok((root, theme))
}

/// `<font>` children of the theme with their place (`None`: every place).
fn theme_fonts(elements: &[Element], theme: usize) -> Vec<(usize, Option<String>)> {
    children(elements, theme).filter(|(_, e)| e.is("font"))
        .map(|(i, e)| (i, e.attribute("place").filter(|p| !p.is_empty()).map(str::to_owned))).collect()
}

fn place_is(place: &Option<String>, wanted: &str) -> bool {
    place.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(wanted))
}

/// The window title font labwc uses (the active window's), and whether rc.xml
/// sets a title bar font of its own (`place="ActiveWindow"`/`"InactiveWindow"`).
pub fn titlebar_font(text: &str) -> Result<(LabwcFont, bool), String> {
    let elements = scan(text)?;
    let (_, theme) = root_and_theme(&elements)?;
    let mut font = LabwcFont::default();
    let Some(theme) = theme else { return Ok((font, false)) };
    let mut custom = false;
    for (index, place) in theme_fonts(&elements, theme) {
        custom |= TITLE_PLACES.iter().any(|p| place_is(&place, p));
        if !(place.is_none() || place_is(&place, "ActiveWindow")) {
            continue;
        }
        let element = &elements[index];
        let mut values: Vec<(String, String)> = element.attributes.iter().filter(|(n, _)| n != "place").cloned().collect();
        values.extend(children(&elements, index).map(|(_, child)| (child.name.clone(), inner_text(text, child))));
        for (key, value) in values {
            match key.as_str() {
                "name" => font.name = value,
                "size" => font.size = value.trim().split('.').next().and_then(|v| v.parse().ok()).unwrap_or(0),
                "slant" => font.slant = value,
                "weight" => font.weight = value,
                _ => {}
            }
        }
    }
    Ok((font, custom))
}

/// Whitespace between the start of the line and `position`, when only
/// whitespace precedes it on its line.
fn indent_before(text: &str, position: usize) -> Option<&str> {
    let line = text[..position].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent = &text[line..position];
    indent.chars().all(|c| c == ' ' || c == '\t').then_some(indent)
}

fn render_font(place: &str, font: &LabwcFont, indent: &str, unit: &str, newline: &str) -> String {
    let inner = format!("{indent}{unit}");
    format!("<font place=\"{place}\">{newline}{inner}<name>{}</name>{newline}{inner}<size>{}</size>{newline}\
             {inner}<slant>{}</slant>{newline}{inner}<weight>{}</weight>{newline}{indent}</font>",
            escape(&font.name), font.size, escape(&font.slant), escape(&font.weight))
}

/// Insert `blocks` (each one child, no indentation) as the last children of
/// `parent`, opening `<parent/>` when needed.
fn append_children(text: &str, parent: &Element, parent_indent: &str, child_indent: &str, blocks: &[String],
                   newline: &str) -> (usize, usize, String) {
    let lines: String = blocks.iter().map(|b| format!("{child_indent}{b}{newline}")).collect();
    match parent.close {
        None => {
            let raw = &text[parent.start..parent.open_end];
            let opening = raw.strip_suffix("/>").unwrap_or(raw).trim_end();
            (parent.start, parent.open_end, format!("{opening}>{newline}{lines}{parent_indent}</{}>", parent.name))
        }
        Some((close, _)) => match indent_before(text, close) {
            Some(indent) if close - indent.len() > parent.open_end => {
                let at = close - indent.len();
                (at, at, lines)
            }
            _ => (close, close, format!("{newline}{}{parent_indent}", lines.trim_end_matches(newline))),
        },
    }
}

/// The span that removes `element`, with its line when it is alone on it.
fn removal(text: &str, element: &Element) -> (usize, usize) {
    let (start, end) = (element.start, element.end());
    let Some(indent) = indent_before(text, start) else { return (start, end) };
    let after = &text[end..];
    let trailing = after.len() - after.trim_start_matches([' ', '\t']).len();
    let rest = &after[trailing..];
    let newline = if rest.starts_with("\r\n") { 2 } else if rest.starts_with('\n') { 1 } else { return (start, end) };
    (start - indent.len(), end + trailing + newline)
}

/// rc.xml with the window title font set (`Some`) or the title bar fonts
/// removed (`None`). Every other byte is kept; a missing `<theme>` is added.
pub fn with_titlebar_font(text: &str, font: Option<&LabwcFont>) -> Result<String, String> {
    let elements = scan(text)?;
    let (root, theme) = root_and_theme(&elements)?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let root_indent = indent_before(text, elements[root].start).unwrap_or("");
    // The file's indentation step, from the root's first child.
    let unit = children(&elements, root).next().and_then(|(_, e)| indent_before(text, e.start))
        .and_then(|i| i.strip_prefix(root_indent)).filter(|u| !u.is_empty()).unwrap_or("  ").to_owned();
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    match theme {
        None => {
            let Some(font) = font else { return Ok(text.to_owned()) };
            let theme_indent = format!("{root_indent}{unit}");
            let child_indent = format!("{theme_indent}{unit}");
            let fonts: String = TITLE_PLACES.iter()
                .map(|p| format!("{child_indent}{}{newline}", render_font(p, font, &child_indent, &unit, newline))).collect();
            let block = format!("<theme>{newline}{fonts}{theme_indent}</theme>");
            edits.push(append_children(text, &elements[root], root_indent, &theme_indent, &[block], newline));
        }
        Some(theme) => {
            let theme_indent = indent_before(text, elements[theme].start).unwrap_or("").to_owned();
            let child_indent = format!("{theme_indent}{unit}");
            let fonts = theme_fonts(&elements, theme);
            let mut appended = Vec::new();
            for place in TITLE_PLACES {
                let matches: Vec<usize> = fonts.iter().filter(|(_, p)| place_is(p, place)).map(|(i, _)| *i).collect();
                let Some(font) = font else {
                    edits.extend(matches.iter().map(|i| {
                        let (start, end) = removal(text, &elements[*i]);
                        (start, end, String::new())
                    }));
                    continue;
                };
                // A later `<font>` without a place would override this one.
                let overridden = matches.first().is_some_and(|first| {
                    fonts.iter().any(|(i, p)| p.is_none() && elements[*i].start > elements[*first].start)
                });
                let keep = if overridden { None } else { matches.first().copied() };
                for index in &matches {
                    if Some(*index) == keep {
                        let element = &elements[*index];
                        let indent = indent_before(text, element.start).unwrap_or(&child_indent).to_owned();
                        edits.push((element.start, element.end(), render_font(place, font, &indent, &unit, newline)));
                    } else {
                        let (start, end) = removal(text, &elements[*index]);
                        edits.push((start, end, String::new()));
                    }
                }
                if keep.is_none() {
                    appended.push(render_font(place, font, &child_indent, &unit, newline));
                }
            }
            if !appended.is_empty() {
                edits.push(append_children(text, &elements[theme], &theme_indent, &child_indent, &appended, newline));
            }
        }
    }
    edits.sort_by_key(|(start, end, _)| (*start, *end));
    let mut out = text.to_owned();
    for (start, end, replacement) in edits.into_iter().rev() {
        out.replace_range(start..end, &replacement);
    }
    Ok(out)
}

// ------------------------------------------------------------ settings.ini

/// `text` without `key=` assignments in `[section]`; every other line kept.
pub fn ini_remove(text: &str, section: &str, key: &str) -> String {
    let mut inside = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name == section;
        } else if inside && !trimmed.starts_with('#') && !trimmed.starts_with(';')
            && trimmed.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Replace a file atomically, keeping its mode (new files: 0644); a
/// symlinked file is written through to its target.
fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let target = if path.is_symlink() { std::fs::canonicalize(path)? } else { path.to_path_buf() };
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mode = std::fs::metadata(&target).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644);
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = parent.join(format!(".{name}.{}.fonts.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&temporary, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Set (`Some`) or remove (`None`) `gtk-font-name` in one settings.ini.
/// Returns whether the file changed; unchanged files are not rewritten and
/// a missing file is not created to remove a key.
pub fn edit_gtk_ini(path: &Path, value: Option<&str>) -> Result<bool, String> {
    let error = |e: std::io::Error| tessel_core::pyerr::os_error_path(&e, path);
    let current = read_optional(path).map_err(error)?;
    let updated = match (value, &current) {
        (Some(value), current) => ini_set(current.as_deref().unwrap_or(""), INI_SECTION, INI_KEY, value),
        (None, Some(text)) => ini_remove(text, INI_SECTION, INI_KEY),
        (None, None) => return Ok(false),
    };
    if current.as_deref() == Some(updated.as_str()) {
        return Ok(false);
    }
    write_file(path, &updated).map_err(error)?;
    Ok(true)
}

// ------------------------------------------------------------ backends

/// The `org.gnome.desktop.interface` keys this page uses (tests use a fake
/// or a keyfile/memory backend).
pub trait InterfaceKeys {
    fn get(&self, key: &str) -> String;
    fn set(&self, key: &str, value: &str) -> bool;
    /// Whether the key has its schema default.
    fn is_default(&self, key: &str) -> bool;
    fn reset(&self, key: &str) -> bool;
    fn connect_changed(&self, key: &str, handler: Rc<dyn Fn()>) -> u64;
    fn disconnect(&self, id: u64);
}

pub struct GioInterfaceKeys {
    settings: gio::Settings,
    handlers: RefCell<Vec<(u64, glib::SignalHandlerId)>>,
    next: Cell<u64>,
}

impl GioInterfaceKeys {
    pub fn new(settings: gio::Settings) -> Self {
        Self { settings, handlers: RefCell::new(Vec::new()), next: Cell::new(1) }
    }

    /// The session's interface settings when the schema has the font keys.
    pub fn from_session() -> Option<Self> {
        let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.interface", true)?;
        let complete = FontRole::ALL.iter().filter_map(|r| r.gsettings_key()).all(|k| schema.has_key(k));
        complete.then(|| Self::new(gio::Settings::new("org.gnome.desktop.interface")))
    }
}

impl InterfaceKeys for GioInterfaceKeys {
    fn get(&self, key: &str) -> String { self.settings.string(key).to_string() }
    fn set(&self, key: &str, value: &str) -> bool {
        let ok = self.settings.set_string(key, value).is_ok();
        gio::Settings::sync();
        ok
    }
    fn is_default(&self, key: &str) -> bool {
        self.settings.default_value(key).is_none_or(|default| default == self.settings.value(key))
    }
    fn reset(&self, key: &str) -> bool {
        self.settings.reset(key);
        gio::Settings::sync();
        true
    }
    fn connect_changed(&self, key: &str, handler: Rc<dyn Fn()>) -> u64 {
        let id = self.settings.connect_changed(Some(key), move |_, _| handler());
        let number = self.next.get();
        self.next.set(number + 1);
        self.handlers.borrow_mut().push((number, id));
        number
    }
    fn disconnect(&self, id: u64) {
        let mut handlers = self.handlers.borrow_mut();
        if let Some(index) = handlers.iter().position(|(n, _)| *n == id) {
            let (_, handler) = handlers.remove(index);
            self.settings.disconnect(handler);
        }
    }
}

/// Where the fonts are stored (tests use a fake).
pub trait FontSettings {
    /// The configured font as a Pango description; `None` when this font
    /// cannot be changed on this system.
    fn current(&self, role: FontRole) -> Option<String>;
    fn set(&self, role: FontRole, font: &str) -> Result<(), String>;
    /// Whether every font is the default one.
    fn is_default(&self) -> bool;
    fn reset(&self) -> Result<(), String>;
    fn connect_changed(&self, handler: Rc<dyn Fn()>) -> u64;
    fn disconnect(&self, id: u64);
}

/// `labwc --reconfigure` result: `(ok, error)`.
pub type Reconfigure = Rc<dyn Fn() -> (bool, Option<String>)>;

/// gsettings, both GTK settings.ini files and labwc's rc.xml.
pub struct SystemFontSettings {
    keys: Option<Rc<dyn InterfaceKeys>>,
    config_home: PathBuf,
    rc: PathBuf,
    reconfigure: Reconfigure,
    handlers: RefCell<Vec<(u64, Vec<u64>)>>,
    next: Cell<u64>,
}

impl SystemFontSettings {
    pub fn new(keys: Option<Rc<dyn InterfaceKeys>>, config_home: PathBuf, rc: PathBuf, reconfigure: Reconfigure) -> Self {
        Self { keys, config_home, rc, reconfigure, handlers: RefCell::new(Vec::new()), next: Cell::new(1) }
    }

    pub fn from_session() -> Self {
        let keys = GioInterfaceKeys::from_session().map(|k| Rc::new(k) as Rc<dyn InterfaceKeys>);
        // Only a running labwc can be asked to reload.
        let reconfigure: Reconfigure = Rc::new(|| {
            if std::env::var_os("LABWC_PID").is_some_and(|v| !v.is_empty()) {
                tessel_displays::labwc::reconfigure_labwc()
            } else {
                (true, None)
            }
        });
        Self::new(keys, tessel_core::paths::config_home(), tessel_displays::labwc::labwc_config_path(), reconfigure)
    }

    fn gsettings_error() -> String {
        tr!("The shared font setting could not be changed.")
    }

    fn read_rc(&self) -> Result<Option<String>, String> {
        read_optional(&self.rc).map_err(|e| tessel_core::pyerr::os_error_path(&e, &self.rc))
    }

    /// Edit rc.xml under the shared rc.xml lock; reload labwc when it changed.
    fn edit_rc(&self, font: Option<&LabwcFont>) -> Result<(), String> {
        let path = std::fs::canonicalize(&self.rc).unwrap_or_else(|_| self.rc.clone());
        if font.is_none() && !path.exists() {
            return Ok(());
        }
        let directory = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory).map_err(|e| tessel_core::pyerr::os_error_path(&e, directory))?;
        let lock_path = tessel_displays::labwc::rc_lock_path(&path);
        let _lock = tessel_displays::labwc::lock_rc(&path).map_err(|e| tessel_core::pyerr::os_error_path(&e, &lock_path))?;
        let text = match read_optional(&path).map_err(|e| tessel_core::pyerr::os_error_path(&e, &path))? {
            Some(text) => text,
            None if font.is_none() => return Ok(()),
            None => "<?xml version=\"1.0\"?>\n<labwc_config>\n</labwc_config>\n".to_owned(),
        };
        let updated = with_titlebar_font(&text, font)?;
        if updated == text {
            return Ok(());
        }
        write_file(&path, &updated).map_err(|e| tessel_core::pyerr::os_error_path(&e, &path))?;
        drop(_lock);
        match (self.reconfigure)() {
            (true, _) => Ok(()),
            (false, error) => Err(tr!("The window manager did not reload its settings: {error}",
                                      error = error.unwrap_or_default().as_str())),
        }
    }

    /// No title bar font of its own, or Tessel's (an unreadable rc.xml is
    /// not offered for a reset).
    fn title_is_default(&self) -> bool {
        match self.read_rc().ok().flatten().map(|t| titlebar_font(&t)) {
            Some(Ok((font, true))) => same_font(&font, &tessel_title_font()),
            _ => true,
        }
    }

    fn ini_values(&self) -> Vec<Option<String>> {
        gtk_ini_paths(&self.config_home).iter()
            .map(|p| std::fs::read_to_string(p).ok().and_then(|t| ini_get(&t, INI_SECTION, INI_KEY))).collect()
    }
}

impl FontSettings for SystemFontSettings {
    fn current(&self, role: FontRole) -> Option<String> {
        match role.gsettings_key() {
            Some(key) => {
                let value = self.keys.as_ref().map(|k| k.get(key)).filter(|v| !v.is_empty());
                match role {
                    FontRole::Interface => value.or_else(|| self.ini_values().pop().flatten()).or_else(|| Some("Sans 10".into())),
                    _ => value,
                }
            }
            None => {
                let text = self.read_rc().ok().flatten();
                let font = text.and_then(|t| titlebar_font(&t).ok()).map(|(f, _)| f).unwrap_or_default();
                Some(pango_description(&font))
            }
        }
    }

    fn set(&self, role: FontRole, font: &str) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Some(key) = role.gsettings_key() {
            match &self.keys {
                Some(keys) => {
                    if keys.get(key) != font && !keys.set(key, font) {
                        errors.push(Self::gsettings_error());
                    }
                }
                None if role != FontRole::Interface => errors.push(Self::gsettings_error()),
                None => {}
            }
        }
        match role {
            FontRole::Interface => {
                for path in gtk_ini_paths(&self.config_home) {
                    if let Err(error) = edit_gtk_ini(&path, Some(font)) {
                        errors.push(error);
                    }
                }
            }
            FontRole::Titlebar => {
                if let Err(error) = self.edit_rc(Some(&labwc_font(font))) {
                    errors.push(error);
                }
            }
            _ => {}
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join(" ")) }
    }

    fn is_default(&self) -> bool {
        let keys = self.keys.as_ref().is_none_or(|keys| {
            FontRole::ALL.iter().filter_map(|r| r.gsettings_key()).all(|key| keys.is_default(key))
        });
        keys && self.ini_values().iter().all(Option::is_none) && self.title_is_default()
    }

    fn reset(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Some(keys) = &self.keys {
            for key in FontRole::ALL.iter().filter_map(|r| r.gsettings_key()) {
                if !keys.is_default(key) && !keys.reset(key) {
                    errors.push(Self::gsettings_error());
                }
            }
        }
        for path in gtk_ini_paths(&self.config_home) {
            if let Err(error) = edit_gtk_ini(&path, None) {
                errors.push(error);
            }
        }
        if !self.title_is_default() {
            if let Err(error) = self.edit_rc(Some(&tessel_title_font())) {
                errors.push(error);
            }
        }
        errors.dedup();
        if errors.is_empty() { Ok(()) } else { Err(errors.join(" ")) }
    }

    fn connect_changed(&self, handler: Rc<dyn Fn()>) -> u64 {
        let Some(keys) = &self.keys else { return 0 };
        let ids = FontRole::ALL.iter().filter_map(|r| r.gsettings_key())
            .map(|key| keys.connect_changed(key, handler.clone())).collect();
        let number = self.next.get();
        self.next.set(number + 1);
        self.handlers.borrow_mut().push((number, ids));
        number
    }

    fn disconnect(&self, id: u64) {
        let ids = {
            let mut handlers = self.handlers.borrow_mut();
            match handlers.iter().position(|(n, _)| *n == id) {
                Some(index) => handlers.remove(index).1,
                None => return,
            }
        };
        if let Some(keys) = &self.keys {
            for id in ids {
                keys.disconnect(id);
            }
        }
    }
}

// ------------------------------------------------------------ the card

/// The Fonts card of the Theme & Wallpaper page.
pub struct FontChooser {
    pub card: gtk::Box,
    pub buttons: Vec<(FontRole, gtk::FontDialogButton)>,
    pub reset: gtk::Button,
    pub sample: gtk::Label,
    pub monospace_sample: gtk::Label,
    pub status: gtk::Label,
    settings: Rc<dyn FontSettings>,
    handler: Cell<u64>,
    updating: Cell<bool>,
    closed: Cell<bool>,
}

/// Keep a long font name from widening the page.
fn ellipsize_labels(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(label) = current.downcast_ref::<gtk::Label>() {
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_max_width_chars(28);
        }
        ellipsize_labels(&current);
        child = current.next_sibling();
    }
}

fn is_monospace(item: &glib::Object) -> bool {
    if let Some(family) = item.downcast_ref::<pango::FontFamily>() {
        return family.is_monospace();
    }
    if let Some(face) = item.downcast_ref::<pango::FontFace>() {
        return face.family().is_monospace();
    }
    true
}

fn font_attributes(description: &str) -> pango::AttrList {
    let attributes = pango::AttrList::new();
    attributes.insert(pango::AttrFontDesc::new(&pango::FontDescription::from_string(description)));
    attributes
}

impl FontChooser {
    pub fn new(settings: Rc<dyn FontSettings>) -> Rc<Self> {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("gs-card");
        card.add_css_class("font-card");
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        text.append(&label(&tr!("Fonts"), None));
        text.append(&label(&tr!("Choose the fonts used by the desktop and applications."), Some("subtitle")));
        card.append(&text);
        let grid = gtk::Grid::builder().row_spacing(8).column_spacing(16).margin_top(4).build();
        let mut buttons = Vec::new();
        for (row, role) in FontRole::ALL.into_iter().enumerate() {
            let title = role.title();
            let caption = label(&title, None);
            caption.set_hexpand(true);
            caption.set_valign(gtk::Align::Center);
            let dialog = gtk::FontDialog::builder().title(tr!("Choose a font")).modal(true).build();
            if role == FontRole::Monospace {
                dialog.set_filter(Some(&gtk::CustomFilter::new(is_monospace)));
            }
            let button = gtk::FontDialogButton::new(Some(dialog));
            button.set_level(gtk::FontLevel::Font);
            button.set_use_font(true);
            button.set_use_size(false);
            button.set_valign(gtk::Align::Center);
            button.set_halign(gtk::Align::End);
            button.set_widget_name(role.target());
            accessible_label(&button, &title);
            caption.set_mnemonic_widget(Some(&button));
            grid.attach(&caption, 0, row as i32, 1, 1);
            grid.attach(&button, 1, row as i32, 1, 1);
            buttons.push((role, button));
        }
        card.append(&grid);
        let samples = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).margin_top(4).build();
        samples.add_css_class("font-samples");
        let sample = label(&tr!("The quick brown fox jumps over the lazy dog."), None);
        let monospace_sample = label(MONOSPACE_SAMPLE, None);
        for widget in [&sample, &monospace_sample] {
            widget.set_ellipsize(pango::EllipsizeMode::End);
            widget.set_wrap(false);
            samples.append(widget);
        }
        card.append(&samples);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let hint = label(&tr!("Some applications use the new fonts only after they are restarted. Text size is set on the Accessibility page."),
                         Some("subtitle"));
        hint.set_hexpand(true);
        footer.append(&hint);
        let reset = gtk::Button::with_label(&tr!("Restore defaults"));
        reset.set_valign(gtk::Align::Center);
        footer.append(&reset);
        card.append(&footer);
        let status = label("", Some("subtitle"));
        status.set_visible(false);
        card.append(&status);
        let chooser = Rc::new(Self {
            card, buttons, reset, sample, monospace_sample, status, settings, handler: Cell::new(0),
            updating: Cell::new(false), closed: Cell::new(false),
        });
        chooser.sync();
        for (role, button) in &chooser.buttons {
            ellipsize_labels(button.upcast_ref());
            let weak = Rc::downgrade(&chooser);
            let role = *role;
            button.connect_font_desc_notify(move |button| {
                if let Some(chooser) = weak.upgrade() {
                    chooser.chosen(role, button);
                }
            });
        }
        let weak = Rc::downgrade(&chooser);
        chooser.reset.connect_clicked(move |_| {
            if let Some(chooser) = weak.upgrade() {
                chooser.restore_defaults();
            }
        });
        let weak = Rc::downgrade(&chooser);
        let id = chooser.settings.connect_changed(Rc::new(move || {
            if let Some(chooser) = weak.upgrade() {
                chooser.sync();
            }
        }));
        chooser.handler.set(id);
        chooser
    }

    pub fn button(&self, role: FontRole) -> &gtk::FontDialogButton {
        &self.buttons.iter().find(|(r, _)| *r == role).expect("font role").1
    }

    /// Show the configured fonts without saving.
    pub fn sync(&self) {
        if self.closed.get() {
            return;
        }
        self.updating.set(true);
        for (role, button) in &self.buttons {
            match self.settings.current(*role) {
                Some(font) => {
                    let desc = pango::FontDescription::from_string(&font);
                    if button.font_desc().is_none_or(|d| d.to_str() != desc.to_str()) {
                        button.set_font_desc(&desc);
                    }
                    button.set_sensitive(true);
                }
                None => button.set_sensitive(false),
            }
            ellipsize_labels(button.upcast_ref());
        }
        self.updating.set(false);
        let font = |role| self.button(role).font_desc().map(|d| d.to_str().to_string()).unwrap_or_default();
        self.sample.set_attributes(Some(&font_attributes(&font(FontRole::Interface))));
        self.monospace_sample.set_attributes(Some(&font_attributes(&font(FontRole::Monospace))));
        self.reset.set_sensitive(!self.settings.is_default());
    }

    fn show_error(&self, error: String) {
        self.status.set_text(&error);
        self.status.set_visible(true);
    }

    fn chosen(&self, role: FontRole, button: &gtk::FontDialogButton) {
        if self.updating.get() || self.closed.get() {
            return;
        }
        let Some(desc) = button.font_desc() else { return };
        let font = desc.to_str().to_string();
        if self.settings.current(role).is_some_and(|c| pango::FontDescription::from_string(&c).to_str() == font) {
            return;
        }
        match self.settings.set(role, &font) {
            Ok(()) => self.status.set_visible(false),
            Err(error) => self.show_error(tr!("The font could not be changed: {error}", error = error.as_str())),
        }
        self.sync();
    }

    /// Every font back to its default.
    pub fn restore_defaults(&self) {
        if self.closed.get() || self.settings.is_default() {
            return;
        }
        match self.settings.reset() {
            Ok(()) => self.status.set_visible(false),
            Err(error) => self.show_error(tr!("The fonts could not be reset: {error}", error = error.as_str())),
        }
        self.sync();
    }

    /// Search spotlight targets.
    pub fn register_targets(&self, page: &SettingsPage) {
        for (role, button) in &self.buttons {
            page.add_target(role.target(), button);
        }
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        let id = self.handler.replace(0);
        if id != 0 {
            self.settings.disconnect(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(name: &str, size: i32, slant: &str, weight: &str) -> LabwcFont {
        LabwcFont { name: name.into(), size, slant: slant.into(), weight: weight.into() }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-fonts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pango_descriptions_map_to_labwc_fonts_and_back() {
        assert_eq!(labwc_font("Cantarell 11"), font("Cantarell", 11, "normal", "normal"));
        assert_eq!(labwc_font("Fira Code Bold 11"), font("Fira Code", 11, "normal", "bold"));
        assert_eq!(labwc_font("Noto Sans Mono 10.5"), font("Noto Sans Mono", 11, "normal", "normal"));
        assert_eq!(labwc_font("Noto Sans Mono 10.4"), font("Noto Sans Mono", 10, "normal", "normal"));
        assert_eq!(labwc_font("Noto Serif Italic 12"), font("Noto Serif", 12, "italic", "normal"));
        assert_eq!(labwc_font("Source Sans 3 Semi-Bold Oblique 9"), font("Source Sans 3", 9, "oblique", "semibold"));
        assert_eq!(labwc_font("Inter Medium 10"), font("Inter", 10, "normal", "medium"));
        assert_eq!(labwc_font("Inter Heavy 10"), font("Inter", 10, "normal", "heavy"));
        assert_eq!(labwc_font("Sans"), font("Sans", 10, "normal", "normal"));
        assert_eq!(labwc_font(""), font("sans", 10, "normal", "normal"));
        assert_eq!(pango_description(&font("Fira Code", 11, "normal", "bold")), "Fira Code Bold 11");
        assert_eq!(pango_description(&font("Noto Sans Mono", 10, "normal", "normal")), "Noto Sans Mono 10");
        assert_eq!(pango_description(&font("Noto Serif", 12, "Italic", "Bold")), "Noto Serif Bold Italic 12");
        assert_eq!(pango_description(&font("sans", 10, "weird", "weird")), "sans 10");
        for description in ["Fira Code Bold 11", "Noto Serif Bold Italic 12", "Source Sans 3 Semi-Bold 9", "Ubuntu Light 13"] {
            assert_eq!(pango_description(&labwc_font(description)), pango::FontDescription::from_string(description).to_str());
        }
    }

    const RC: &str = "<?xml version=\"1.0\"?>
<labwc_config>
  <!-- keep me <font place=\"ActiveWindow\"> -->
  <theme>
    <name>GTK</name>
    <cornerRadius>8</cornerRadius>
    <font place=\"ActiveWindow\">
      <name>Old</name>
      <size>9</size>
    </font>
    <font place=\"MenuItem\"><name>Menu Font</name><size>12</size></font>
    <font place=\"InactiveWindow\" name=\"Older\" size=\"8\"/>
    <!-- a comment -->
    <font place=\"OnScreenDisplay\">
      <name>OSD</name>
    </font>
  </theme>
  <desktops number=\"4\"/>
</labwc_config>
";

    #[test]
    fn existing_title_fonts_are_replaced_in_place() {
        let (current, custom) = titlebar_font(RC).unwrap();
        assert_eq!((current, custom), (font("Old", 9, "normal", "normal"), true));
        let updated = with_titlebar_font(RC, Some(&font("Fira Code", 11, "normal", "bold"))).unwrap();
        let expected = RC.replace("<font place=\"ActiveWindow\">
      <name>Old</name>
      <size>9</size>
    </font>", "<font place=\"ActiveWindow\">
      <name>Fira Code</name>
      <size>11</size>
      <slant>normal</slant>
      <weight>bold</weight>
    </font>").replace("<font place=\"InactiveWindow\" name=\"Older\" size=\"8\"/>", "<font place=\"InactiveWindow\">
      <name>Fira Code</name>
      <size>11</size>
      <slant>normal</slant>
      <weight>bold</weight>
    </font>");
        assert_eq!(updated, expected);
        assert_eq!(titlebar_font(&updated).unwrap().0, font("Fira Code", 11, "normal", "bold"));
        // The same font again changes nothing.
        assert_eq!(with_titlebar_font(&updated, Some(&font("Fira Code", 11, "normal", "bold"))).unwrap(), updated);
        // Removing them keeps the menu and OSD fonts and the comments.
        let removed = with_titlebar_font(&updated, None).unwrap();
        assert_eq!(removed, "<?xml version=\"1.0\"?>
<labwc_config>
  <!-- keep me <font place=\"ActiveWindow\"> -->
  <theme>
    <name>GTK</name>
    <cornerRadius>8</cornerRadius>
    <font place=\"MenuItem\"><name>Menu Font</name><size>12</size></font>
    <!-- a comment -->
    <font place=\"OnScreenDisplay\">
      <name>OSD</name>
    </font>
  </theme>
  <desktops number=\"4\"/>
</labwc_config>
");
        assert_eq!(titlebar_font(&removed).unwrap(), (LabwcFont::default(), false));
        assert_eq!(with_titlebar_font(&removed, None).unwrap(), removed);
    }

    #[test]
    fn absent_title_fonts_are_added_to_the_theme() {
        let text = "<?xml version=\"1.0\"?>\n<labwc_config>\n  <theme>\n    <cornerRadius>8</cornerRadius>\n  </theme>\n</labwc_config>\n";
        let updated = with_titlebar_font(text, Some(&font("Noto Sans", 10, "italic", "normal"))).unwrap();
        assert_eq!(updated, "<?xml version=\"1.0\"?>\n<labwc_config>\n  <theme>\n    <cornerRadius>8</cornerRadius>\n\
            \x20   <font place=\"ActiveWindow\">\n      <name>Noto Sans</name>\n      <size>10</size>\n      <slant>italic</slant>\n      <weight>normal</weight>\n    </font>\n\
            \x20   <font place=\"InactiveWindow\">\n      <name>Noto Sans</name>\n      <size>10</size>\n      <slant>italic</slant>\n      <weight>normal</weight>\n    </font>\n\
            \x20 </theme>\n</labwc_config>\n");
        assert_eq!(with_titlebar_font(&updated, None).unwrap(), text);
    }

    #[test]
    fn a_missing_or_empty_theme_is_created() {
        let block = |indent: &str| format!("{indent}<theme>\n\
            {indent}  <font place=\"ActiveWindow\">\n{indent}    <name>A</name>\n{indent}    <size>10</size>\n{indent}    <slant>normal</slant>\n{indent}    <weight>normal</weight>\n{indent}  </font>\n\
            {indent}  <font place=\"InactiveWindow\">\n{indent}    <name>A</name>\n{indent}    <size>10</size>\n{indent}    <slant>normal</slant>\n{indent}    <weight>normal</weight>\n{indent}  </font>\n\
            {indent}</theme>");
        let a = font("A", 10, "normal", "normal");
        let text = "<?xml version=\"1.0\"?>\n<labwc_config>\n  <core><gap>10</gap></core>\n</labwc_config>\n";
        assert_eq!(with_titlebar_font(text, Some(&a)).unwrap(),
                   format!("<?xml version=\"1.0\"?>\n<labwc_config>\n  <core><gap>10</gap></core>\n{}\n</labwc_config>\n", block("  ")));
        // The root as headless.sh and first logins write it.
        assert_eq!(with_titlebar_font("<?xml version=\"1.0\"?>\n<labwc_config/>\n", Some(&a)).unwrap(),
                   format!("<?xml version=\"1.0\"?>\n<labwc_config>\n{}\n</labwc_config>\n", block("  ")));
        // A self-closing <theme/> keeps its attributes.
        let text = "<labwc_config>\n  <theme a=\"1\" />\n</labwc_config>\n";
        let updated = with_titlebar_font(text, Some(&a)).unwrap();
        assert!(updated.starts_with("<labwc_config>\n  <theme a=\"1\">\n    <font place=\"ActiveWindow\">\n"), "{updated}");
        assert!(updated.ends_with("    </font>\n  </theme>\n</labwc_config>\n"), "{updated}");
        // Nothing to remove: unchanged.
        assert_eq!(with_titlebar_font(text, None).unwrap(), text);
        assert_eq!(with_titlebar_font("<labwc_config/>", None).unwrap(), "<labwc_config/>");
        // Tabs are kept as the indentation step.
        let text = "<labwc_config>\n\t<theme>\n\t\t<name>x</name>\n\t</theme>\n</labwc_config>\n";
        assert!(with_titlebar_font(text, Some(&a)).unwrap().contains("\t\t<font place=\"ActiveWindow\">\n\t\t\t<name>A</name>\n"));
    }

    #[test]
    fn crlf_files_special_characters_and_errors() {
        let text = "<labwc_config>\r\n  <theme>\r\n    <name>x</name>\r\n  </theme>\r\n</labwc_config>\r\n";
        let updated = with_titlebar_font(text, Some(&font("A & <B> \"C\"", 10, "normal", "normal"))).unwrap();
        assert!(!updated.replace("\r\n", "").contains('\n'), "{updated:?}");
        assert!(updated.contains("<name>A &amp; &lt;B&gt; \"C\"</name>\r\n"), "{updated:?}");
        assert_eq!(titlebar_font(&updated).unwrap().0.name, "A & <B> \"C\"");
        assert_eq!(with_titlebar_font(&updated, None).unwrap(), text);
        assert!(with_titlebar_font("<labwc_config><theme></labwc_config>", None).is_err());
        assert!(with_titlebar_font("<labwc_config><theme>", Some(&LabwcFont::default())).is_err());
        assert!(titlebar_font("").is_err());
    }

    #[test]
    fn a_later_font_without_place_wins_so_the_title_font_moves_after_it() {
        let text = "<labwc_config>\n  <theme>\n    <font place=\"ActiveWindow\"><name>A</name></font>\n    <font><name>All</name><size>14</size></font>\n  </theme>\n</labwc_config>\n";
        assert_eq!(titlebar_font(text).unwrap().0, font("All", 14, "normal", "normal"));
        let updated = with_titlebar_font(text, Some(&font("B", 10, "normal", "normal"))).unwrap();
        assert!(updated.starts_with("<labwc_config>\n  <theme>\n    <font><name>All</name><size>14</size></font>\n    <font place=\"ActiveWindow\">"), "{updated}");
        assert_eq!(titlebar_font(&updated).unwrap().0, font("B", 10, "normal", "normal"));
        // Without a title font of its own the shared one is shown.
        let shared = "<labwc_config><theme><font><name>All</name><size>14</size><weight>bold</weight></font></theme></labwc_config>";
        assert_eq!(titlebar_font(shared).unwrap(), (font("All", 14, "normal", "bold"), false));
    }

    #[test]
    fn ini_remove_keeps_every_other_line() {
        let text = "# mine\n[Settings]\ngtk-font-name=Sans 10\ngtk-icon-theme-name=Papirus\n; gtk-font-name=x\n\n[Other]\ngtk-font-name=keep\n";
        assert_eq!(ini_remove(text, INI_SECTION, INI_KEY),
                   "# mine\n[Settings]\ngtk-icon-theme-name=Papirus\n; gtk-font-name=x\n\n[Other]\ngtk-font-name=keep\n");
        assert_eq!(ini_remove("[Settings]\ngtk-font-name = A\ngtk-font-name=B", INI_SECTION, INI_KEY), "[Settings]\n");
        assert_eq!(ini_remove("", INI_SECTION, INI_KEY), "");
    }

    struct FakeKeys {
        values: RefCell<std::collections::HashMap<String, String>>,
        writes: RefCell<Vec<String>>,
        fail: Cell<bool>,
    }

    const DEFAULTS: [(&str, &str); 3] = [("font-name", "Cantarell 11"), ("document-font-name", "Cantarell 11"),
                                         ("monospace-font-name", "Source Code Pro 10")];

    impl FakeKeys {
        fn new() -> Rc<Self> {
            Rc::new(Self { values: RefCell::new(DEFAULTS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()),
                           writes: RefCell::default(), fail: Cell::new(false) })
        }
    }

    impl InterfaceKeys for FakeKeys {
        fn get(&self, key: &str) -> String { self.values.borrow()[key].clone() }
        fn set(&self, key: &str, value: &str) -> bool {
            self.writes.borrow_mut().push(format!("{key}={value}"));
            if !self.fail.get() {
                self.values.borrow_mut().insert(key.into(), value.into());
            }
            !self.fail.get()
        }
        fn is_default(&self, key: &str) -> bool { DEFAULTS.iter().any(|(k, v)| *k == key && self.get(key) == *v) }
        fn reset(&self, key: &str) -> bool {
            self.writes.borrow_mut().push(format!("reset {key}"));
            let default = DEFAULTS.iter().find(|(k, _)| *k == key).unwrap().1;
            self.values.borrow_mut().insert(key.into(), default.into());
            true
        }
        fn connect_changed(&self, _key: &str, _handler: Rc<dyn Fn()>) -> u64 { 1 }
        fn disconnect(&self, _id: u64) {}
    }

    struct Fixture {
        dir: PathBuf,
        keys: Rc<FakeKeys>,
        reloads: Rc<Cell<u32>>,
        backend: SystemFontSettings,
    }

    fn fixture(name: &str, reload_ok: bool) -> Fixture {
        let dir = scratch(name);
        let config = dir.join("config");
        std::fs::create_dir_all(config.join("gtk-3.0")).unwrap();
        std::fs::create_dir_all(config.join("labwc")).unwrap();
        std::fs::write(config.join("gtk-3.0/settings.ini"), "[Settings]\n# keep\ngtk-icon-theme-name=Papirus-Dark\n").unwrap();
        std::fs::write(config.join("labwc/rc.xml"), "<?xml version=\"1.0\"?>\n<labwc_config>\n  <core><gap>10</gap></core>\n</labwc_config>\n").unwrap();
        let keys = FakeKeys::new();
        let reloads = Rc::new(Cell::new(0));
        let counter = reloads.clone();
        let reconfigure: Reconfigure = Rc::new(move || {
            counter.set(counter.get() + 1);
            if reload_ok { (true, None) } else { (false, Some("no compositor".into())) }
        });
        let backend = SystemFontSettings::new(Some(keys.clone()), config.clone(), config.join("labwc/rc.xml"), reconfigure);
        Fixture { dir, keys, reloads, backend }
    }

    impl Fixture {
        fn read(&self, path: &str) -> String { std::fs::read_to_string(self.dir.join("config").join(path)).unwrap_or_default() }
    }

    #[test]
    fn system_backend_writes_gsettings_ini_files_and_rc_xml() {
        let f = fixture("system", true);
        assert!(f.backend.is_default());
        assert_eq!(f.backend.current(FontRole::Interface).as_deref(), Some("Cantarell 11"));
        assert_eq!(f.backend.current(FontRole::Titlebar).as_deref(), Some("sans 10"));
        f.backend.set(FontRole::Interface, "Noto Sans 10.5").unwrap();
        assert_eq!(*f.keys.writes.borrow(), ["font-name=Noto Sans 10.5"]);
        assert_eq!(f.read("gtk-3.0/settings.ini"), "[Settings]\n# keep\ngtk-icon-theme-name=Papirus-Dark\ngtk-font-name=Noto Sans 10.5\n");
        assert_eq!(f.read("gtk-4.0/settings.ini"), "[Settings]\ngtk-font-name=Noto Sans 10.5\n");
        assert!(!f.backend.is_default());
        f.backend.set(FontRole::Document, "Noto Serif 11").unwrap();
        f.backend.set(FontRole::Monospace, "Fira Code 10").unwrap();
        assert_eq!(f.keys.writes.borrow()[1..], ["document-font-name=Noto Serif 11", "monospace-font-name=Fira Code 10"]);
        assert_eq!(f.reloads.get(), 0);
        f.backend.set(FontRole::Titlebar, "Fira Code Bold 11").unwrap();
        assert_eq!(f.reloads.get(), 1);
        assert!(f.read("labwc/rc.xml").contains("<theme>\n    <font place=\"ActiveWindow\">\n      <name>Fira Code</name>\n      <size>11</size>"));
        assert_eq!(f.backend.current(FontRole::Titlebar).as_deref(), Some("Fira Code Bold 11"));
        // No temporary files left; no lock file inside the gtk folders.
        assert_eq!(std::fs::read_dir(f.dir.join("config/gtk-4.0")).unwrap().count(), 1);

        // Unchanged: nothing is written, labwc is not reloaded.
        let before = (f.read("gtk-3.0/settings.ini"), f.read("labwc/rc.xml"), f.keys.writes.borrow().len());
        f.backend.set(FontRole::Interface, "Noto Sans 10.5").unwrap();
        f.backend.set(FontRole::Titlebar, "Fira Code Bold 11").unwrap();
        assert_eq!((f.read("gtk-3.0/settings.ini"), f.read("labwc/rc.xml"), f.keys.writes.borrow().len()), before);
        assert_eq!(f.reloads.get(), 1);

        // Reset: gsettings reset, ini key removed, Tessel's title font, one reload.
        f.backend.reset().unwrap();
        assert_eq!(f.keys.writes.borrow()[3..], ["reset font-name", "reset document-font-name", "reset monospace-font-name"]);
        assert_eq!(f.read("gtk-3.0/settings.ini"), "[Settings]\n# keep\ngtk-icon-theme-name=Papirus-Dark\n");
        assert_eq!(f.read("gtk-4.0/settings.ini"), "[Settings]\n");
        assert!(f.read("labwc/rc.xml").contains("<font place=\"InactiveWindow\">\n      <name>Sans</name>\n      <size>10</size>\n      <slant>normal</slant>\n      <weight>semibold</weight>\n    </font>\n  </theme>"));
        assert_eq!(f.backend.current(FontRole::Titlebar).as_deref(), Some("Sans Semi-Bold 10"));
        assert_eq!(f.reloads.get(), 2);
        assert!(f.backend.is_default());
        // Resetting again writes nothing.
        f.backend.reset().unwrap();
        assert_eq!(f.keys.writes.borrow().len(), 6);
        assert_eq!(f.reloads.get(), 2);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_failing_write_is_reported_and_the_others_still_happen() {
        let f = fixture("failing", false);
        f.keys.fail.set(true);
        let error = f.backend.set(FontRole::Interface, "Inter 10").unwrap_err();
        assert_eq!(error, "The shared font setting could not be changed.");
        assert_eq!(f.read("gtk-4.0/settings.ini"), "[Settings]\ngtk-font-name=Inter 10\n");
        // A read-only settings.ini does not stop gsettings or the other file.
        f.keys.fail.set(false);
        std::fs::remove_file(f.dir.join("config/gtk-3.0/settings.ini")).unwrap();
        std::fs::create_dir(f.dir.join("config/gtk-3.0/settings.ini")).unwrap();
        let error = f.backend.set(FontRole::Interface, "Inter 11").unwrap_err();
        assert!(error.contains("gtk-3.0/settings.ini"), "{error}");
        assert_eq!(f.keys.get("font-name"), "Inter 11");
        assert_eq!(f.read("gtk-4.0/settings.ini"), "[Settings]\ngtk-font-name=Inter 11\n");
        // labwc not reloading is reported, rc.xml is written.
        let error = f.backend.set(FontRole::Titlebar, "Inter 12").unwrap_err();
        assert_eq!(error, "The window manager did not reload its settings: no compositor");
        assert!(f.read("labwc/rc.xml").contains("<name>Inter</name>"));
        // A broken rc.xml is not written; the reset still resets the rest.
        std::fs::remove_dir(f.dir.join("config/gtk-3.0/settings.ini")).unwrap();
        std::fs::write(f.dir.join("config/labwc/rc.xml"), "<labwc_config><theme>").unwrap();
        let error = f.backend.set(FontRole::Titlebar, "Inter 10").unwrap_err();
        assert_eq!(error, "rc.xml is not valid XML.");
        f.backend.reset().unwrap();
        assert!(f.keys.is_default("font-name"));
        assert_eq!(f.read("gtk-4.0/settings.ini"), "[Settings]\n");
        assert_eq!(f.read("labwc/rc.xml"), "<labwc_config><theme>");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn the_title_font_tessel_configure_session_writes_is_the_default() {
        let f = fixture("tessel-title", true);
        // As tessel-configure-session writes it (session.rs font_block).
        std::fs::write(f.dir.join("config/labwc/rc.xml"), "<labwc_config>\n  <theme>\n  <font place=\"ActiveWindow\">\n    <name>Sans</name>\n    <size>10</size>\n    <weight>SemiBold</weight>\n  </font>\n  <font place=\"InactiveWindow\">\n    <name>Sans</name>\n    <size>10</size>\n    <weight>SemiBold</weight>\n  </font>\n  </theme>\n</labwc_config>\n").unwrap();
        assert!(f.backend.is_default());
        let before = f.read("labwc/rc.xml");
        f.backend.reset().unwrap();
        assert_eq!(f.read("labwc/rc.xml"), before);
        assert_eq!(f.reloads.get(), 0);
        f.backend.set(FontRole::Titlebar, "Sans Bold 10").unwrap();
        assert!(!f.backend.is_default());
        f.backend.reset().unwrap();
        assert!(f.backend.is_default());
        assert_eq!(f.reloads.get(), 2);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn rc_xml_is_created_for_a_title_font_and_left_alone_otherwise() {
        let f = fixture("norc", true);
        std::fs::remove_file(f.dir.join("config/labwc/rc.xml")).unwrap();
        f.backend.reset().unwrap();
        assert!(!f.dir.join("config/labwc/rc.xml").exists());
        f.backend.set(FontRole::Titlebar, "Inter 12").unwrap();
        assert!(f.read("labwc/rc.xml").starts_with("<?xml version=\"1.0\"?>\n<labwc_config>\n  <theme>\n    <font place=\"ActiveWindow\">"));
        assert_eq!(f.reloads.get(), 1);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// Manual end-to-end check inside a private nested labwc only:
    /// `FONTS_E2E_RC=<rc.xml> FONTS_E2E_FONT="Serif Bold Italic 16" cargo test … -- --ignored labwc_title_font`.
    #[test]
    #[ignore]
    fn labwc_title_font_in_a_nested_session() {
        let (Some(rc), Some(font)) = (std::env::var_os("FONTS_E2E_RC"), std::env::var("FONTS_E2E_FONT").ok()) else { return };
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        assert!(!runtime.is_empty() && runtime != "/run/user/1000" && std::env::var_os("LABWC_PID").is_some(), "nested session only");
        let rc = PathBuf::from(rc);
        let config = rc.parent().unwrap().join("gtk-config");
        let backend = SystemFontSettings::new(None, config, rc, Rc::new(tessel_displays::labwc::reconfigure_labwc));
        backend.set(FontRole::Titlebar, &font).unwrap();
        assert_eq!(backend.current(FontRole::Titlebar), Some(pango_description(&labwc_font(&font))));
    }

    #[test]
    fn gio_keys_on_a_memory_backend_report_defaults_and_reset() {
        let Some(schema) = gio::SettingsSchemaSource::default().and_then(|s| s.lookup("org.gnome.desktop.interface", true)) else {
            return; // gsettings-desktop-schemas missing
        };
        let settings = gio::Settings::new_full(&schema, Some(&gio::memory_settings_backend_new()), None);
        let keys = GioInterfaceKeys::new(settings);
        assert!(keys.is_default("font-name"));
        let fired = Rc::new(Cell::new(0));
        let counter = fired.clone();
        let id = keys.connect_changed("monospace-font-name", Rc::new(move || counter.set(counter.get() + 1)));
        assert!(keys.set("monospace-font-name", "Fira Code 10"));
        assert_eq!(keys.get("monospace-font-name"), "Fira Code 10");
        assert!(!keys.is_default("monospace-font-name"));
        assert_eq!(fired.get(), 1);
        assert!(keys.reset("monospace-font-name"));
        assert!(keys.is_default("monospace-font-name"));
        keys.disconnect(id);
        keys.set("monospace-font-name", "Fira Code 11");
        assert_eq!(fired.get(), 2);
    }
}
