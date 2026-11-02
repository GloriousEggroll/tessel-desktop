//! Pure logic of install_prompts.py: the rc.xml `<core>` rewrite (with the
//! here-document delimiter, byte-for-byte), the launcher script and the
//! `org.tessel.Prompt.desktop` entry. Preview only: nothing here writes files.

use regex::Regex;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tessel_core::shell::shell_quote;

pub const PROMPT_NAME: &str = "Confirmation";
pub const PROMPT_LANGUAGES: [&str; 30] = ["ar", "cs", "da", "de", "el", "es", "fi", "fr", "he", "hi", "hu", "id", "it", "ja",
                                          "ko", "nb", "nl", "pl", "pt", "pt_BR", "ro", "ru", "sk", "sv", "th", "tr", "uk", "vi",
                                          "zh_CN", "zh_TW"];
/// `~/.local/bin/tessel-prompt`: the path labwc's rc.xml names (and the
/// delimiter seed). It must stay the same so rc.xml is not rewritten.
pub const LAUNCHER: &str = ".local/bin/tessel-prompt";
/// Suggested install location of the Rust binary (relative to home).
pub const DEFAULT_BINARY: &str = ".local/share/tessel-shell/tessel-prompt";
pub const DESKTOP_ENTRY: &str = ".local/share/applications/org.tessel.Prompt.desktop";

/// `str(pathlib.PurePosixPath(path))`.
pub fn py_path_str(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.is_empty() {
        return ".".into();
    }
    let root = if text.starts_with("//") && !text.starts_with("///") { "//" } else if text.starts_with('/') { "/" } else { "" };
    let parts: Vec<&str> = text.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
    let joined = parts.join("/");
    if root.is_empty() && joined.is_empty() { ".".into() } else { format!("{root}{joined}") }
}

fn path_parts(path: &str) -> (String, Vec<String>) {
    let root = if path.starts_with("//") && !path.starts_with("///") { "//" } else if path.starts_with('/') { "/" } else { "" };
    (root.into(), path.split('/').filter(|p| !p.is_empty() && *p != ".").map(str::to_owned).collect())
}

/// `Path(launcher).relative_to(home)` if `is_relative_to`, as a string.
fn relative_to(launcher: &str, home: &str) -> Option<String> {
    let (lroot, lparts) = path_parts(launcher);
    let (hroot, hparts) = path_parts(home);
    if lroot != hroot || lparts.len() < hparts.len() || lparts[..hparts.len()] != hparts[..] {
        return None;
    }
    let rest = &lparts[hparts.len()..];
    Some(if rest.is_empty() { ".".into() } else { rest.join("/") })
}

/// Python `str.splitlines()` (no keepends).
pub fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let breaks = matches!(ch, '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if breaks {
            lines.push(&text[start..index]);
            let mut end = index + ch.len_utf8();
            if ch == '\r' {
                if let Some(&(next, '\n')) = chars.peek() {
                    chars.next();
                    end = next + 1;
                }
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// The here-document delimiter: `"WAYSHELL_PROMPT_" + sha256(launcher + "\0" +
/// "\0".join(messages))`, with `_END` appended while any message line equals it.
pub fn delimiter(launcher: &str, messages: &[String]) -> String {
    let seed = format!("{launcher}\0{}", messages.join("\0"));
    let digest = Sha256::digest(seed.as_bytes());
    let mut delimiter = String::from("WAYSHELL_PROMPT_");
    for byte in digest {
        delimiter.push_str(&format!("{byte:02x}"));
    }
    while messages.iter().any(|message| py_splitlines(message).contains(&delimiter.as_str())) {
        delimiter.push_str("_END");
    }
    delimiter
}

/// `html.escape(value, quote=False)`.
fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn parse_xml(text: &str) -> Result<roxmltree::Document<'_>, String> {
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    roxmltree::Document::parse_with_options(text, options).map_err(|error| format!("ParseError: {error}"))
}

/// ElementTree's tag test: un-namespaced element named `name`.
fn is_tag(node: &roxmltree::Node, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

fn children<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &'a str) -> impl Iterator<Item = roxmltree::Node<'a, 'input>> + 'a {
    node.children().filter(move |child| is_tag(child, name))
}

/// `[node.get("message", "") for node in tree.findall(".//prompt")]`.
pub fn prompt_messages(document: &roxmltree::Document) -> Vec<String> {
    document.root_element().descendants().skip(1).filter(|node| is_tag(node, "prompt"))
        .map(|node| node.attributes().find(|a| a.namespace().is_none() && a.name() == "message")
             .map_or(String::new(), |a| a.value().to_owned()))
        .collect()
}

fn mask_comments(text: &str) -> String {
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    let comment = COMMENT.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    // Same byte length, so offsets in the masked text index the original.
    comment.replace_all(text, |caps: &regex::Captures| " ".repeat(caps[0].len())).into_owned()
}

/// Python `str.rstrip()`.
fn rstrip(text: &str) -> &str {
    text.trim_end_matches(tessel_core::pyjson::is_py_space)
}

/// The two command strings for `launcher` (Python `values`).
pub fn commands(launcher: &Path, home: Option<&Path>, messages: &[String]) -> (String, String) {
    let launcher_text = py_path_str(launcher);
    let delimiter = delimiter(&launcher_text, messages);
    let mut program = shell_quote(&launcher_text);
    if let Some(relative) = home.and_then(|home| relative_to(&launcher_text, &py_path_str(home))) {
        program = format!("\"$HOME\"/{}", shell_quote(&relative));
    }
    let command = format!("exec {program} --message-stdin <<'{delimiter}'\n%m\n{delimiter}");
    // errorCommand runs without a shell, so "$HOME" needs one.
    let error = if program.starts_with("\"$HOME\"") {
        format!("sh -c {}", shell_quote(&format!("exec {program} --error")))
    } else {
        format!("{program} --error")
    };
    (command, error)
}

/// `transform(data, launcher, home=None)`: change only the core prompt/error
/// commands, preserving other XML verbatim.
pub fn transform(data: &[u8], launcher: &Path, home: Option<&Path>) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(data).map_err(|error| format!("UnicodeDecodeError: {error}"))?;
    let document = parse_xml(text)?;
    let tree = document.root_element();
    if !is_tag(&tree, "labwc_config") {
        return Err("Expected labwc_config root".into());
    }
    if children(tree, "core").count() > 1 {
        return Err("Multiple core sections require review".into());
    }
    if py_path_str(launcher).contains('%') {
        return Err("A labwc prompt command path cannot contain percent signs".into());
    }
    let messages = prompt_messages(&document);
    let (command, error) = commands(launcher, home, &messages);
    let values = [("promptCommand", command), ("errorCommand", error)];
    let masked = mask_comments(text);
    static CORE: OnceLock<Regex> = OnceLock::new();
    static CORE_EMPTY: OnceLock<Regex> = OnceLock::new();
    static END: OnceLock<Regex> = OnceLock::new();
    let core = CORE.get_or_init(|| Regex::new(r"(?s)<core\b[^>]*>(.*?)</core\s*>").unwrap()).captures(&masked);
    let mut text = text.to_owned();
    match core {
        None => {
            let empty = CORE_EMPTY.get_or_init(|| Regex::new(r"<core\b[^>]*/>").unwrap());
            if let Some(found) = empty.find(&masked) {
                let body: String = std::iter::once("\n".to_owned())
                    .chain(values.iter().map(|(key, value)| format!("    <{key}>{}</{key}>\n", escape(value)))).collect();
                text = format!("{}<core>{body}  </core>{}", &text[..found.start()], &text[found.end()..]);
            } else {
                let end = END.get_or_init(|| Regex::new(r"</labwc_config\s*>").unwrap()).find(&masked)
                    .ok_or("Missing labwc_config closing element")?;
                let body: String = std::iter::once("  <core>\n".to_owned())
                    .chain(values.iter().map(|(key, value)| format!("    <{key}>{}</{key}>\n", escape(value))))
                    .chain(std::iter::once("  </core>\n".to_owned())).collect();
                text = format!("{}{body}{}", &text[..end.start()], &text[end.start()..]);
            }
        }
        Some(captures) => {
            let group = captures.get(1).unwrap();
            let mut body = text[group.start()..group.end()].to_owned();
            let core_node = children(tree, "core").next();
            for (key, value) in &values {
                let count = core_node.map_or(0, |core| children(core, key).count());
                if count > 1 {
                    return Err(format!("Multiple {key} entries require review"));
                }
                let masked_body = mask_comments(&body);
                let pattern = Regex::new(&format!(r"(?s)<{key}\b[^>]*(?:/>|>.*?</{key}\s*>)")).unwrap();
                let replacement = format!("<{key}>{}</{key}>", escape(value));
                body = match pattern.find(&masked_body) {
                    Some(old) => format!("{}{replacement}{}", &body[..old.start()], &body[old.end()..]),
                    None => format!("{}\n    {replacement}\n  ", rstrip(&body)),
                };
            }
            text = format!("{}{body}{}", &text[..group.start()], &text[group.end()..]);
        }
    }
    parse_xml(&text)?;
    Ok(text.into_bytes())
}

/// `prompt_desktop_entry()` with `translation(code)` = the catalog's
/// "Confirmation" entry (`None` when missing).
pub fn prompt_desktop_entry_with(translation: &dyn Fn(&str) -> Option<String>) -> String {
    let mut names = String::new();
    for code in PROMPT_LANGUAGES {
        let name = translation(code).unwrap_or_else(|| PROMPT_NAME.into());
        if name != PROMPT_NAME {
            names += &format!("Name[{code}]={name}\n");
        }
    }
    format!("[Desktop Entry]\nType=Application\nName={PROMPT_NAME}\n{names}\
Icon=dialog-question-symbolic\nExec=true\nTerminal=false\nNoDisplay=true\nStartupNotify=false\n")
}

/// `prompt_desktop_entry()` from the suite catalogs (`tessel_core::i18n::catalog`).
pub fn prompt_desktop_entry() -> String {
    prompt_desktop_entry_with(&|code| tessel_core::i18n::catalog(code).get(PROMPT_NAME).cloned())
}

/// `prompt_desktop_entry()` from `<directory>/<code>.json` catalogs.
pub fn prompt_desktop_entry_from(directory: &Path) -> String {
    prompt_desktop_entry_with(&|code| {
        let text = std::fs::read_to_string(directory.join(format!("{code}.json"))).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        value.get(PROMPT_NAME)?.as_str().map(str::to_owned)
    })
}

/// The Python launcher (`#!/bin/sh exec /usr/bin/python3 …/wayshell_prompt.py "$@"`).
pub fn python_launcher_script(home: &Path) -> String {
    let script = py_path_str(&home.join(".local/share/tessel-shell/wayshell_prompt.py"));
    format!("#!/bin/sh\nexec /usr/bin/python3 {} \"$@\"\n", shell_quote(&script))
}

/// The Rust launcher: same path (`~/.local/bin/tessel-prompt`), execs `binary`.
pub fn launcher_script(binary: &Path) -> String {
    format!("#!/bin/sh\nexec {} \"$@\"\n", shell_quote(&py_path_str(binary)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub path: PathBuf,
    pub data: Vec<u8>,
    pub mode: u32,
}

/// `plan(home, config_home)` for the Rust prompt: launcher (0755, execs
/// `binary`, default [`DEFAULT_BINARY`]), the desktop entry and the rewritten
/// rc.xml (its mode kept). Read-only; the binary itself is the installer's job.
pub fn plan(home: &Path, config_home: Option<&Path>, binary: Option<&Path>) -> Result<Vec<PlannedFile>, String> {
    let config_home = config_home.map(Path::to_path_buf).unwrap_or_else(|| home.join(".config"));
    let config = config_home.join("labwc/rc.xml");
    let binary = binary.map(Path::to_path_buf).unwrap_or_else(|| home.join(DEFAULT_BINARY));
    let data = std::fs::read(&config).map_err(|error| format!("{}: {}", config.display(), tessel_core::pyerr::os_error_text(&error)))?;
    let mode = std::fs::metadata(&config).map_err(|error| tessel_core::pyerr::os_error_text(&error))?.permissions().mode() & 0o7777;
    let launcher = home.join(LAUNCHER);
    Ok(vec![
        PlannedFile { path: launcher.clone(), data: launcher_script(&binary).into_bytes(), mode: 0o755 },
        PlannedFile { path: home.join(DESKTOP_ENTRY), data: prompt_desktop_entry().into_bytes(), mode: 0o644 },
        PlannedFile { data: transform(&data, &launcher, Some(home))?, path: config, mode },
    ])
}

/// Files whose content or mode differ from what is on disk (Python `changes`).
pub fn pending(files: &[PlannedFile]) -> Vec<&PlannedFile> {
    files.iter().filter(|file| {
        let current = std::fs::metadata(&file.path).ok().filter(|m| m.is_file());
        match current {
            None => true,
            Some(meta) => std::fs::read(&file.path).ok().as_deref() != Some(&file.data[..]) || meta.permissions().mode() & 0o7777 != file.mode,
        }
    }).collect()
}
