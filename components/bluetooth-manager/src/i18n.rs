//! btmanager's own catalogs (translations.py: `MESSIDS`, `CATALOGS`, 31
//! languages), embedded at build time, plus Python `%` formatting and the
//! `--gen-pot` / `--gen-mo` writers (`generate_po`, `generate_mo`).
//!
//! The suite's shared `locale/*.json` differ from these in a few dozen
//! entries, so btmanager keeps its own text exactly like the Python app.

use std::cell::RefCell;
use std::collections::HashMap;

macro_rules! catalogs {
    ($($code:literal),* $(,)?) => {
        /// `(code, embedded JSON)` in `available_languages()` order (sorted).
        pub const CATALOG_SOURCES: &[(&str, &str)] = &[$(($code, include_str!(concat!("../locale/", $code, ".json")))),*];
    };
}

catalogs!("ar", "cs", "da", "de", "el", "en", "es", "fi", "fr", "he", "hi", "hu", "id", "it", "ja", "ko", "nb", "nl",
          "pl", "pt", "pt_BR", "ro", "ru", "sk", "sv", "th", "tr", "uk", "vi", "zh_CN", "zh_TW");

const MESSIDS_JSON: &str = include_str!("../locale/messids.json");

/// `translations.MESSIDS` (source order).
pub fn messids() -> Vec<String> {
    serde_json::from_str(MESSIDS_JSON).expect("embedded MESSIDS")
}

/// `translations.available_languages()`.
pub fn available_languages() -> Vec<&'static str> {
    CATALOG_SOURCES.iter().map(|(code, _)| *code).collect()
}

/// `translations.CATALOGS[lang]` in file order (empty for an unknown language).
pub fn raw_catalog(lang: &str) -> Vec<(String, String)> {
    let Some((_, source)) = CATALOG_SOURCES.iter().find(|(code, _)| *code == lang) else { return Vec::new() };
    let value: serde_json::Value = serde_json::from_str(source).expect("embedded catalog");
    value.as_object().map(|map| map.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned())).collect())
        .unwrap_or_default()
}

/// `translations.catalog_for(lang)`: every msgid, then the language's strings.
pub fn catalog_for(lang: &str) -> Vec<(String, String)> {
    let mut ordered: Vec<(String, String)> = messids().into_iter().map(|m| (m.clone(), m)).collect();
    for (key, value) in raw_catalog(lang) {
        match ordered.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => ordered.push((key, value)),
        }
    }
    ordered
}

thread_local! {
    static TRANSLATOR: RefCell<(String, HashMap<String, String>)> = RefCell::new(("en".into(), catalog_map("en")));
}

fn catalog_map(lang: &str) -> HashMap<String, String> {
    catalog_for(lang).into_iter().collect()
}

/// `App._rebuild_translator()` / `make_translator(lang)`: the global translator.
pub fn set_language(lang: &str) {
    TRANSLATOR.with(|t| *t.borrow_mut() = (lang.to_owned(), catalog_map(lang)));
}

pub fn current_language() -> String {
    TRANSLATOR.with(|t| t.borrow().0.clone())
}

/// `_()` (GNUTranslations.gettext over the compiled catalog).
pub fn tr(message: &str) -> String {
    TRANSLATOR.with(|t| t.borrow().1.get(message).cloned().unwrap_or_else(|| message.to_owned()))
}

/// Translate with a specific language without touching the global translator.
pub fn tr_in(lang: &str, message: &str) -> String {
    catalog_map(lang).get(message).cloned().unwrap_or_else(|| message.to_owned())
}

/// A `%` argument.
#[derive(Clone, Debug)]
pub enum P {
    S(String),
    I(i64),
}

impl From<&str> for P {
    fn from(v: &str) -> Self { P::S(v.to_owned()) }
}
impl From<String> for P {
    fn from(v: String) -> Self { P::S(v) }
}
impl From<&String> for P {
    fn from(v: &String) -> Self { P::S(v.clone()) }
}
impl From<i64> for P {
    fn from(v: i64) -> Self { P::I(v) }
}
impl From<i32> for P {
    fn from(v: i32) -> Self { P::I(v as i64) }
}
impl From<u32> for P {
    fn from(v: u32) -> Self { P::I(v as i64) }
}

fn render(value: &P, conversion: char) -> String {
    match (value, conversion) {
        (P::I(n), _) => n.to_string(),
        (P::S(s), 'd') => s.parse::<i64>().map(|n| n.to_string()).unwrap_or_else(|_| s.clone()),
        (P::S(s), _) => s.clone(),
    }
}

/// Python `format % args` for `%s`, `%d`, `%%` and `%(name)s` (mapping keys).
/// Positional and named arguments are both accepted; a missing value leaves
/// the placeholder unchanged (where Python would raise).
pub fn pct(format: &str, positional: &[P], named: &[(&str, P)]) -> String {
    let mut out = String::with_capacity(format.len() + 16);
    let mut chars = format.char_indices().peekable();
    let mut index = 0usize;
    while let Some((start, ch)) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some((_, '%')) => {
                chars.next();
                out.push('%');
            }
            Some((_, '(')) => {
                chars.next();
                let mut name = String::new();
                let mut closed = false;
                for (_, c) in chars.by_ref() {
                    if c == ')' {
                        closed = true;
                        break;
                    }
                    name.push(c);
                }
                let conversion = chars.next().map(|(_, c)| c).unwrap_or('s');
                match (closed, named.iter().find(|(k, _)| *k == name)) {
                    (true, Some((_, value))) => out.push_str(&render(value, conversion)),
                    _ => {
                        out.push_str(&format[start..start + 2 + name.len() + usize::from(closed)]);
                        out.push(conversion);
                    }
                }
            }
            Some((_, c @ ('s' | 'd' | 'r' | 'i'))) => {
                chars.next();
                match positional.get(index) {
                    Some(value) => out.push_str(&render(value, c)),
                    None => {
                        out.push('%');
                        out.push(c);
                    }
                }
                index += 1;
            }
            _ => out.push('%'),
        }
    }
    out
}

/// `_(text) % value` with one positional argument.
pub fn tr1(message: &str, value: impl Into<P>) -> String {
    pct(&tr(message), &[value.into()], &[])
}

/// `_(text) % {...}`.
pub fn trn(message: &str, values: &[(&str, P)]) -> String {
    pct(&tr(message), &[], values)
}

/// `generate_po(catalog, lang)` (btmanager.py), byte for byte.
pub fn generate_po(catalog: &[(String, String)], lang: &str) -> String {
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
    }
    let mut out: Vec<String> = vec!["# btmanager translation catalog".into(), String::new()];
    let header = catalog.iter().find(|(k, _)| k.is_empty()).map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty()).unwrap_or_else(|| "Content-Type: text/plain; charset=UTF-8\n".into());
    out.push("msgid \"\"".into());
    out.push("msgstr \"\"".into());
    out.push("\"Project-Id-Version: btmanager 1.0\\n\"".into());
    out.push("\"PO-Revision-Date: 2026-09-16 00:00+0000\\n\"".into());
    out.push("\"Last-Translator: btmanager project\\n\"".into());
    out.push("\"Language-Team: btmanager project\\n\"".into());
    out.push("\"MIME-Version: 1.0\\n\"".into());
    out.push("\"Content-Transfer-Encoding: 8bit\\n\"".into());
    if !lang.is_empty() {
        out.push(format!("\"Language: {lang}\\n\""));
    }
    for line in header.trim_end_matches('\n').split('\n') {
        out.push(format!("\"{}\\n\"", esc(line)));
    }
    out.push(String::new());
    let mut keys: Vec<&(String, String)> = catalog.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    keys.dedup_by(|a, b| a.0 == b.0);
    for (key, value) in keys {
        if key.is_empty() {
            continue;
        }
        out.push(format!("msgid \"{}\"", esc(key)));
        out.push(format!("msgstr \"{}\"", esc(value)));
        out.push(String::new());
    }
    out.join("\n") + "\n"
}

/// `generate_mo(catalog)`: little-endian GNU .mo, keys sorted by UTF-8 bytes.
pub fn generate_mo(catalog: &[(String, String)]) -> Vec<u8> {
    let mut entries: Vec<&(String, String)> = catalog.iter().collect();
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let (mut ids, mut strs) = (Vec::new(), Vec::new());
    let mut offsets = Vec::new();
    for (key, value) in entries {
        offsets.push((ids.len() as u32, key.len() as u32, strs.len() as u32, value.len() as u32));
        ids.extend_from_slice(key.as_bytes());
        ids.push(0);
        strs.extend_from_slice(value.as_bytes());
        strs.push(0);
    }
    let n = offsets.len() as u32;
    let keystart = 7 * 4 + 16 * n;
    let valuestart = keystart + ids.len() as u32;
    let mut out = Vec::new();
    for word in [0x950412DEu32, 0, n, 7 * 4, 7 * 4 + 8 * n, 0, 0] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for (o1, l1, _, _) in &offsets {
        out.extend_from_slice(&l1.to_le_bytes());
        out.extend_from_slice(&(o1 + keystart).to_le_bytes());
    }
    for (_, _, o2, l2) in &offsets {
        out.extend_from_slice(&l2.to_le_bytes());
        out.extend_from_slice(&(o2 + valuestart).to_le_bytes());
    }
    out.extend_from_slice(&ids);
    out.extend_from_slice(&strs);
    out
}

/// The `--gen-pot` catalog: `{m: m for m in MESSIDS}`.
pub fn pot_catalog() -> Vec<(String, String)> {
    messids().into_iter().map(|m| (m.clone(), m)).collect()
}

/// The `--gen-mo` catalog for one language (with the `""` header).
pub fn mo_catalog(lang: &str) -> Vec<(String, String)> {
    let mut catalog = catalog_for(lang);
    catalog.push((String::new(), "Content-Type: text/plain; charset=UTF-8\n".into()));
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_is_complete_like_the_selftest_requires() {
        let ids = messids();
        assert_eq!(ids.len(), 103);
        assert_eq!(available_languages().len(), 31);
        for lang in available_languages() {
            let catalog: HashMap<_, _> = catalog_for(lang).into_iter().collect();
            for id in &ids {
                assert!(!catalog.get(id).map(|s| s.trim()).unwrap_or("").is_empty(), "{lang}: {id}");
            }
        }
    }

    #[test]
    fn translator_switches_and_falls_back_to_the_msgid() {
        set_language("de");
        assert_ne!(tr("Bluetooth is on"), "Bluetooth is on");
        assert_eq!(tr("Not a known message"), "Not a known message");
        set_language("xx");
        assert_eq!(tr("Bluetooth is on"), "Bluetooth is on");
        set_language("en");
        assert_eq!(current_language(), "en");
    }

    #[test]
    fn python_percent_formatting() {
        assert_eq!(pct("Error: %s", &["boom".into()], &[]), "Error: boom");
        assert_eq!(pct("%d%%", &[P::I(42)], &[]), "42%");
        assert_eq!(pct("%(device)s wants “%(name)s” (%(size)s).", &[],
                       &[("device", "Phone".into()), ("name", "a.jpg".into()), ("size", "2.0 kB".into())]),
                   "Phone wants “a.jpg” (2.0 kB).");
        assert_eq!(pct("no args", &[], &[]), "no args");
        assert_eq!(pct("%(missing)s", &[], &[]), "%(missing)s");
    }

    #[test]
    fn mo_round_trips_and_has_gnu_header() {
        let catalog = mo_catalog("fr");
        let mo = generate_mo(&catalog);
        assert_eq!(&mo[..4], &0x950412DEu32.to_le_bytes());
        let n = u32::from_le_bytes(mo[8..12].try_into().unwrap()) as usize;
        assert_eq!(n, catalog.len());
        let read = |table: usize, i: usize| {
            let at = table + 8 * i;
            let len = u32::from_le_bytes(mo[at..at + 4].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(mo[at + 4..at + 8].try_into().unwrap()) as usize;
            String::from_utf8(mo[off..off + len].to_vec()).unwrap()
        };
        let map: HashMap<String, String> = (0..n).map(|i| (read(28, i), read(28 + 8 * n, i))).collect();
        assert_eq!(map["Scan"], tr_in("fr", "Scan"));
        assert!(map[""].starts_with("Content-Type"));
    }

    #[test]
    fn po_header_and_escaping() {
        let po = generate_po(&[("a \"b\"\n".into(), "c\\".into())], "de");
        assert!(po.starts_with("# btmanager translation catalog\n\nmsgid \"\"\nmsgstr \"\"\n\"Project-Id-Version: btmanager 1.0\\n\"\n"));
        assert!(po.contains("\"Language: de\\n\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\nmsgid \"a \\\"b\\\"\\n\"\nmsgstr \"c\\\\\"\n\n"));
        assert!(po.ends_with("\n\n"));
    }
}
