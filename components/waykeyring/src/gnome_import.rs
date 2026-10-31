//! Import of unencrypted GNOME keyrings (`~/.local/share/keyrings/*.keyring`
//! starting with "[keyring]", i.e. created with an empty password). They are
//! GKeyFile text: `[N]` items with display-name/secret, `[N:attributeM]`
//! groups with name/type/value.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub struct Imported {
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub secret: Zeroizing<Vec<u8>>,
}

pub fn gnome_dir() -> PathBuf {
    suite_core::paths::data_home().join("keyrings")
}

/// Whether the file is a plain-text (unencrypted) GNOME keyring.
pub fn is_plaintext(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 9];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"[keyring]"
}

/// Plain-text keyrings and gnome-keyring's leftover temporary copies of them.
pub fn plaintext_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.is_file() && is_plaintext(p)).collect();
    files.sort();
    files
}

/// Keyring files (not temporary copies) among `plaintext_files`.
pub fn is_keyring_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "keyring")
}

/// GKeyFile value unescaping (\s \n \t \r \\).
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub fn parse(text: &str) -> Vec<Imported> {
    let mut groups: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            groups.push((name.to_owned(), BTreeMap::new()));
        } else if let (Some((key, value)), Some(group)) = (line.split_once('='), groups.last_mut()) {
            group.1.insert(key.trim().to_owned(), unescape(value.trim_start()));
        }
    }
    let mut items: Vec<(String, Imported)> = Vec::new();
    for (name, keys) in &groups {
        if name == "keyring" || name.contains(':') {
            continue;
        }
        let Some(secret) = keys.get("secret") else { continue };
        items.push((name.clone(), Imported {
            label: keys.get("display-name").cloned().unwrap_or_default(),
            attributes: BTreeMap::new(),
            secret: Zeroizing::new(secret.as_bytes().to_vec()),
        }));
    }
    for (name, keys) in &groups {
        let Some((item, _)) = name.split_once(":attribute") else { continue };
        let (Some(key), Some(value)) = (keys.get("name"), keys.get("value")) else { continue };
        if let Some((_, imported)) = items.iter_mut().find(|(n, _)| n == item) {
            imported.attributes.insert(key.clone(), value.clone());
        }
    }
    items.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[keyring]\ndisplay-name=Default keyring\nctime=0\nmtime=0\nlock-on-idle=false\nlock-after=false\n\n\
[1]\nitem-type=0\ndisplay-name=GitHub\nsecret=tok\\sen\\nx\nmtime=1\nctime=1\n\n\
[1:attribute0]\nname=service\ntype=string\nvalue=github\n\n[1:attribute1]\nname=xdg:schema\ntype=string\nvalue=org.x\n\n\
[2]\nitem-type=0\ndisplay-name=No secret\nmtime=1\nctime=1\n";

    #[test]
    fn parses_items_attributes_and_escapes() {
        let items = parse(SAMPLE);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "GitHub");
        assert_eq!(items[0].secret.as_slice(), b"tok en\nx");
        assert_eq!(items[0].attributes.get("service").map(String::as_str), Some("github"));
        assert_eq!(items[0].attributes.get("xdg:schema").map(String::as_str), Some("org.x"));
    }
}
