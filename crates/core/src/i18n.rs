//! Shared display-language preference and explicit UI message translation
//! (port of shared/suite_i18n.py).
//!
//! Call [`initialize`] first in `main`, before GTK initializes (gtk_init calls
//! setlocale too). No preferences or system settings are written here; only the
//! Language wizard may change those settings.

use crate::lang;
use crate::pyfmt::{self, Arg};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

static LANGUAGE: OnceLock<String> = OnceLock::new();
type Catalog = Arc<HashMap<String, String>>;
static CATALOGS: LazyLock<Mutex<HashMap<String, Catalog>>> = LazyLock::new(Default::default);

/// `~/.config/tessel/language.json`.
pub fn preference_path() -> PathBuf {
    crate::paths::suite_config_dir().join("language.json")
}

/// Map any locale-ish value (`pt-br.UTF-8`, `zh-Hant`, `fr_CA:en_US`) to a supported code.
pub fn normalize_language(value: Option<&str>) -> String {
    let value = value.unwrap_or("");
    let value = value.split(':').next().unwrap().split('.').next().unwrap().split('@').next().unwrap().replace('-', "_");
    let mut value = match value.split_once('_') {
        Some((first, rest)) => format!("{}_{}", first.to_lowercase(), rest.to_uppercase()),
        None => value.to_lowercase(),
    };
    let first = |value: &str| value.split('_').next().unwrap().to_owned();
    if matches!(first(&value).as_str(), "no" | "nn") {
        value = "nb".into();
    }
    if lang::is_supported(&value) {
        return value;
    }
    if value.starts_with("zh_") {
        let upper = value.to_uppercase();
        let traditional = upper.split('_').skip(1).any(|tag| matches!(tag, "TW" | "HK" | "MO" | "HANT"));
        return if traditional { "zh_TW" } else { "zh_CN" }.into();
    }
    let first = first(&value);
    if lang::is_supported(&first) { first } else { "en".into() }
}

/// A valid language.json: version 1, a supported language and its exact locale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preference {
    pub language: String,
    pub locale: String,
}

pub fn read_preference() -> Option<Preference> {
    read_preference_at(&preference_path())
}

pub fn read_preference_at(path: &std::path::Path) -> Option<Preference> {
    let data: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let object = data.as_object()?;
    // Python compares `version == 1`, which True and 1.0 also satisfy.
    let version_ok = match object.get("version") {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() == Some(1.0),
        _ => false,
    };
    let language = object.get("language")?.as_str()?;
    let locale = object.get("locale")?.as_str()?;
    (version_ok && lang::locale_name(language) == Some(locale))
        .then(|| Preference { language: language.into(), locale: locale.into() })
}

fn first_set(getenv: &dyn Fn(&str) -> Option<String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| getenv(name).filter(|value| !value.is_empty()))
}

/// The language a starting process should use: preference, else LANGUAGE/LC_ALL/LC_MESSAGES/LANG.
pub fn selected_language() -> String {
    selected_language_with(read_preference().as_ref(), &|name| env::var(name).ok())
}

pub fn selected_language_with(preference: Option<&Preference>, getenv: &dyn Fn(&str) -> Option<String>) -> String {
    match preference {
        Some(preference) => preference.language.clone(),
        None => normalize_language(first_set(getenv, &["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]).as_deref()),
    }
}

const REGIONAL: [&str; 10] = ["LC_NUMERIC", "LC_TIME", "LC_COLLATE", "LC_MONETARY", "LC_PAPER",
                              "LC_NAME", "LC_ADDRESS", "LC_TELEPHONE", "LC_MEASUREMENT", "LC_IDENTIFICATION"];

/// Environment edits `initialize` performs for a preference: `(name, Some(value))`
/// sets, `(name, None)` removes. Empty without a preference.
pub fn environment_changes(preference: Option<&Preference>, getenv: &dyn Fn(&str) -> Option<String>)
                           -> Vec<(String, Option<String>)> {
    let Some(preference) = preference else { return Vec::new() };
    // LC_ALL would mask LC_MESSAGES; preserve the previous regional settings
    // while making the central display-language preference authoritative.
    let lc_all = getenv("LC_ALL");
    let mut changes = Vec::new();
    if lc_all.is_some() {
        changes.push(("LC_ALL".to_owned(), None));
    }
    let lc_all = lc_all.filter(|value| !value.is_empty());
    let previous = lc_all.clone().or_else(|| getenv("LANG")).unwrap_or_else(|| "C.UTF-8".into());
    for category in REGIONAL {
        match &lc_all {
            Some(value) => changes.push((category.to_owned(), Some(value.clone()))),
            None if getenv(category).is_none() => changes.push((category.to_owned(), Some(previous.clone()))),
            None => {}
        }
    }
    changes.push(("LANG".into(), Some(preference.locale.clone())));
    changes.push(("LC_MESSAGES".into(), Some(preference.locale.clone())));
    changes.push(("LANGUAGE".into(), Some(preference.language.clone())));
    changes
}

/// Apply the selected interface language to this newly starting process.
/// Idempotent: later calls return the first result and change nothing.
pub fn initialize() -> &'static str {
    LANGUAGE.get_or_init(|| {
        let preference = read_preference();
        let getenv = |name: &str| env::var(name).ok();
        let language = selected_language_with(preference.as_ref(), &getenv);
        for (name, value) in environment_changes(preference.as_ref(), &getenv) {
            // Runs at startup, before GTK or any other thread exists.
            match value {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
        }
        // A removed locale package must never prevent a control app from
        // opening; the suite's own catalogs still work.
        unsafe { libc::setlocale(libc::LC_ALL, c"".as_ptr()) };
        language
    })
}

/// The process language: the initialized one, else the current selection.
pub fn language() -> String {
    LANGUAGE.get().cloned().unwrap_or_else(selected_language)
}

pub fn is_rtl() -> bool {
    lang::is_rtl(&language())
}

/// Where `<code>.json` catalogs are read from.
///
/// 1. `$DESKTOP_SUITE_LOCALE_DIR` (Rust-only override for tests/dev runs),
/// 2. `<executable dir>/locale` if it exists (Python: `<module dir>/locale`),
/// 3. `$XDG_DATA_HOME/tessel/locale` (`~/.local/share/tessel/locale`).
pub fn catalog_directory() -> PathBuf {
    if let Some(directory) = env::var_os("DESKTOP_SUITE_LOCALE_DIR").filter(|value| !value.is_empty()) {
        return directory.into();
    }
    let local = env::current_exe().ok().and_then(|exe| exe.canonicalize().ok())
        .and_then(|exe| exe.parent().map(|parent| parent.join("locale")));
    match local {
        Some(local) if local.is_dir() => local,
        _ => crate::paths::suite_data_dir().join("locale"),
    }
}

/// Load (and cache) one catalog; string → string entries only, `{}` on any error.
pub fn catalog(code: &str) -> Catalog {
    let mut cache = CATALOGS.lock().unwrap();
    if let Some(catalog) = cache.get(code) {
        return catalog.clone();
    }
    let path = catalog_directory().join(format!("{code}.json"));
    let entries: HashMap<String, String> = std::fs::read_to_string(path).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map.into_iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_owned())))
                .collect()),
            _ => None,
        })
        .unwrap_or_default();
    let catalog = Arc::new(entries);
    cache.insert(code.to_owned(), catalog.clone());
    catalog
}

/// Forget cached catalogs (tests, or after a catalog directory change).
pub fn clear_catalog_cache() {
    CATALOGS.lock().unwrap().clear();
}

/// Translate a source message, safely retaining named format parameters.
pub fn translate(message: &str) -> String {
    translate_with(message, &[])
}

/// `translate(message, **values)`.
pub fn translate_with(message: &str, values: &[(&str, Arg)]) -> String {
    translate_in(&catalog(&language()), message, values)
}

/// Translation against an explicit catalog (the testable core of `translate`).
pub fn translate_in(catalog: &HashMap<String, String>, message: &str, values: &[(&str, Arg)]) -> String {
    let mut translated = catalog.get(message).filter(|text| !text.is_empty()).map_or(message, String::as_str);
    if !same_placeholders(message, translated) {
        translated = message;
    }
    if values.is_empty() {
        return translated.to_owned();
    }
    // Missing/extra fields in a translation must not crash a control UI.
    pyfmt::format(translated, values)
        .or_else(|_| pyfmt::format(message, values))
        // Python would raise here (a source bug); show the raw source instead.
        .unwrap_or_else(|_| message.to_owned())
}

/// Used by catalog validation to catch changed interpolation fields.
pub fn format_fields(message: &str) -> std::collections::BTreeSet<String> {
    pyfmt::format_fields(message).unwrap_or_default()
}

static PERCENT_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"%(?:\([^)]+\))?[#0+\-]*(?:\d+|\*)?(?:\.(?:\d+|\*))?[hlL]?[diouxXeEfFgGcrsa%]").unwrap()
});

/// Reject damaged catalogs without dropping user-visible values: `{}` fields
/// (name, spec, conversion) and `%` placeholders must match as multisets.
pub fn same_placeholders(source: &str, translated: &str) -> bool {
    let fields = |text: &str| -> Option<Vec<(String, String, String)>> {
        let mut fields: Vec<_> = pyfmt::parse(text).ok()?.into_iter()
            .filter_map(|piece| piece.field)
            .map(|(name, spec, conversion)| (name, spec, conversion.map(String::from).unwrap_or_default()))
            .collect();
        fields.sort();
        Some(fields)
    };
    let percents = |text: &str| {
        let mut found: Vec<&str> = PERCENT_FIELD.find_iter(text).map(|m| m.as_str()).collect();
        found.sort_unstable();
        found.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    match (fields(source), fields(translated)) {
        (Some(first), Some(second)) => first == second && percents(source) == percents(translated),
        _ => false,
    }
}

/// `tr!("Text")` or `tr!("Hello {name}", name = value, …)` — the `translate as tr` idiom.
///
/// Values are anything convertible into [`Arg`](crate::pyfmt::Arg): strings,
/// integers, floats and bools.
#[macro_export]
macro_rules! tr {
    ($message:expr $(,)?) => { $crate::i18n::translate(&$message) };
    ($message:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::translate_with(&$message, &[$((stringify!($name), $crate::pyfmt::Arg::from($value))),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("suite-i18n-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn apply(preference: Option<&Preference>, env: &mut HashMap<String, String>) -> String {
        let snapshot = env.clone();
        let getenv = |name: &str| snapshot.get(name).cloned();
        let language = selected_language_with(preference, &getenv);
        for (name, value) in environment_changes(preference, &getenv) {
            match value {
                Some(value) => env.insert(name, value),
                None => env.remove(&name),
            };
        }
        language
    }

    fn preference(code: &str) -> Preference {
        Preference { language: code.into(), locale: lang::locale_name(code).unwrap().into() }
    }

    #[test]
    fn common_locale_variants_match_supported_languages() {
        for (value, expected) in [("pt-br.UTF-8", "pt_BR"), ("pt_PT.utf8", "pt"), ("zh_HK", "zh_TW"),
                                  ("zh-Hant", "zh_TW"), ("zh-Hant-TW", "zh_TW"), ("zh-SG", "zh_CN"),
                                  ("fr_CA:en_US", "fr"), ("no_NO", "nb"), ("nn_NO", "nb"), ("C.UTF-8", "en"),
                                  ("xx_XX", "en"), ("de_DE@euro", "de")] {
            assert_eq!(normalize_language(Some(value)), expected, "{value}");
        }
        assert_eq!(normalize_language(None), "en");
    }

    #[test]
    fn override_preserves_effective_regional_formats_when_lc_all_is_cleared() {
        let mut env = env_of(&[("LANG", "de_DE.UTF-8"), ("LC_ALL", "en_US.UTF-8"), ("LC_TIME", "ja_JP.UTF-8"),
                               ("LC_NUMERIC", "de_DE.UTF-8")]);
        assert_eq!(apply(Some(&preference("fr")), &mut env), "fr");
        assert!(!env.contains_key("LC_ALL"));
        assert_eq!(env["LC_TIME"], "en_US.UTF-8");
        assert_eq!(env["LC_NUMERIC"], "en_US.UTF-8");
        assert_eq!(env["LANG"], "fr_FR.UTF-8");
        assert_eq!(env["LC_MESSAGES"], "fr_FR.UTF-8");
    }

    #[test]
    fn individual_regional_overrides_survive_without_lc_all() {
        let mut env = env_of(&[("LANG", "en_US.UTF-8"), ("LC_TIME", "en_GB.UTF-8"), ("LC_NUMERIC", "de_DE.UTF-8")]);
        apply(Some(&preference("ja")), &mut env);
        assert_eq!(env["LC_TIME"], "en_GB.UTF-8");
        assert_eq!(env["LC_NUMERIC"], "de_DE.UTF-8");
        assert_eq!(env["LC_MONETARY"], "en_US.UTF-8");
        assert_eq!(env["LC_MESSAGES"], "ja_JP.UTF-8");
    }

    #[test]
    fn missing_preference_follows_existing_language_without_rewriting_environment() {
        let mut env = env_of(&[("LANG", "en_US.UTF-8"), ("LANGUAGE", "de:en")]);
        let before = env.clone();
        assert_eq!(apply(None, &mut env), "de");
        assert_eq!(env, before);
    }

    #[test]
    fn preference_file_validation() {
        let directory = temp("preference");
        let path = directory.join("language.json");
        assert_eq!(read_preference_at(&path), None);
        fs::write(&path, r#"{"version": 1, "language": "fr", "locale": "invalid;command"}"#).unwrap();
        assert_eq!(read_preference_at(&path), None);
        let mut env = env_of(&[("LANG", "en_US.UTF-8")]);
        assert_eq!(apply(read_preference_at(&path).as_ref(), &mut env), "en");
        assert!(!env.contains_key("LC_MESSAGES"));
        fs::write(&path, r#"{"version": 1, "language": "fr", "locale": "fr_FR.UTF-8"}"#).unwrap();
        assert_eq!(read_preference_at(&path), Some(preference("fr")));
        for invalid in [r#"{"version": 2, "language": "fr", "locale": "fr_FR.UTF-8"}"#, "[]", "{",
                        r#"{"version": 1, "language": ["fr"], "locale": "fr_FR.UTF-8"}"#] {
            fs::write(&path, invalid).unwrap();
            assert_eq!(read_preference_at(&path), None, "{invalid}");
        }
        fs::write(&path, r#"{"version": true, "language": "fr", "locale": "fr_FR.UTF-8"}"#).unwrap();
        assert!(read_preference_at(&path).is_some(), "Python's True == 1");
        fs::remove_dir_all(directory).unwrap();
    }

    fn one(source: &str, translated: &str) -> HashMap<String, String> {
        HashMap::from([(source.to_owned(), translated.to_owned())])
    }

    #[test]
    fn bad_placeholder_translation_falls_back_without_dropping_values() {
        for translated in ["Aucune valeur", "Valeur {wrong}", "Valeur {number:.1f}", "Valeur {number!r}", "{"] {
            let catalog = one("Value {number}", translated);
            assert_eq!(translate_in(&catalog, "Value {number}", &[("number", 42.into())]), "Value 42", "{translated}");
        }
    }

    #[test]
    fn percent_placeholders_are_preserved_for_bluetooth_callers() {
        let catalog = one("Connected to %s", "Connecté");
        assert_eq!(translate_in(&catalog, "Connected to %s", &[]), "Connected to %s");
        let source = "0% is opaque; 100% is fully transparent. Icons stay opaque.";
        assert!(same_placeholders(source, "0 % est opaque ; 100 % est transparent."));
    }

    #[test]
    fn named_fields_can_be_reordered_in_translation() {
        let catalog = one("{first} then {second}", "{second}, après {first}");
        assert_eq!(translate_in(&catalog, "{first} then {second}", &[("first", "A".into()), ("second", "B".into())]),
                   "B, après A");
    }

    #[test]
    fn missing_message_falls_back_to_formatted_source() {
        assert_eq!(translate_in(&HashMap::new(), "Future feature for {device}", &[("device", "Display".into())]),
                   "Future feature for Display");
        assert_eq!(translate_in(&HashMap::new(), "Literal {{braces}}", &[]), "Literal {{braces}}");
        assert_eq!(translate_in(&one("Cancel", ""), "Cancel", &[]), "Cancel");
    }

    #[test]
    fn shipped_catalogs_load_translate_and_retain_placeholders() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tessel-bluetooth/shared/locale");
        let root = if root.is_dir() { root } else { crate::paths::home().join("tessel-bluetooth/shared/locale") };
        if !root.is_dir() {
            eprintln!("skipping: no source catalogs at {}", root.display());
            return;
        }
        let codes: std::collections::BTreeSet<String> = fs::read_dir(&root).unwrap()
            .filter_map(|entry| entry.ok()?.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(codes, lang::LANGUAGES.iter().map(|entry| entry.0.to_owned()).collect());
        for code in &codes {
            let text = fs::read_to_string(root.join(format!("{code}.json"))).unwrap();
            let catalog: HashMap<String, String> = serde_json::from_str(&text).unwrap();
            for (source, translated) in &catalog {
                assert!(same_placeholders(source, translated), "{code}: {source}");
            }
            if code == "fr" {
                assert_eq!(translate_in(&catalog, "Cancel", &[]), "Annuler");
                let source = "0% is opaque; 100% is fully transparent. Icons stay opaque.";
                assert_ne!(translate_in(&catalog, source, &[]), source);
            }
        }
    }

    #[test]
    fn tr_macro_forms() {
        // No catalog entry for these: the formatted source is returned.
        assert_eq!(tr!("Plain zz-test"), "Plain zz-test");
        assert_eq!(tr!("{n} zz-items at {p:.0f}%", n = 3, p = 41.6), "3 zz-items at 42%");
    }
}
