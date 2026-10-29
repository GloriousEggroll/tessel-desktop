//! System and desktop language settings, independent of the GUI
//! (language_backend.py).
//!
//! All writes happen in [`LanguageBackend::apply`]. Run status/apply in a
//! worker so polkit and D-Bus cannot block the GTK thread. Newer systemd
//! versions merge omitted locale keys, so requests explicitly retain/reset
//! every effective locale category.

use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use suite_core::pyerr::os_error_path;
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::tr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Language {
    pub code: &'static str,
    pub name: &'static str,
    pub locale: &'static str,
}

const fn build_languages() -> [Language; 31] {
    let mut out = [Language { code: "", name: "", locale: "" }; 31];
    let mut index = 0;
    while index < 31 {
        let (code, name, locale) = suite_core::lang::LANGUAGES[index];
        out[index] = Language { code, name, locale };
        index += 1;
    }
    out
}

/// `LANGUAGES` in `LANG_NAMES` order.
pub const LANGUAGES: [Language; 31] = build_languages();

/// `LANGUAGE_BY_CODE.get(code)`.
pub fn language_by_code(code: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|item| item.code == code)
}

pub const FORMAT_KEYS: [&str; 10] = [
    "LC_NUMERIC", "LC_TIME", "LC_COLLATE", "LC_MONETARY", "LC_PAPER", "LC_NAME",
    "LC_ADDRESS", "LC_TELEPHONE", "LC_MEASUREMENT", "LC_IDENTIFICATION",
];
/// `LOCALE_KEYS`, sorted (Python iterates `sorted(LOCALE_KEYS)` where order matters).
pub const LOCALE_KEYS: [&str; 14] = [
    "LANG", "LANGUAGE", "LC_ADDRESS", "LC_COLLATE", "LC_CTYPE", "LC_IDENTIFICATION",
    "LC_MEASUREMENT", "LC_MESSAGES", "LC_MONETARY", "LC_NAME", "LC_NUMERIC", "LC_PAPER",
    "LC_TELEPHONE", "LC_TIME",
];
pub const LANGUAGE_KEYS: [&str; 4] = ["LANG", "LANGUAGE", "LC_MESSAGES", "LC_ALL"];
pub const MARKER_BEGIN: &str = "# Desktop suite language — managed by Language Settings";
pub const MARKER_END: &str = "# End desktop suite language";

pub fn is_locale_key(key: &str) -> bool {
    LOCALE_KEYS.contains(&key)
}

fn assignment() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    // Python's str `\s` also matches the \x1c-\x1f separators.
    PATTERN.get_or_init(|| Regex::new(r"^[\s\x1c-\x1f]*(?:export[\s\x1c-\x1f]+)?([A-Za-z_][A-Za-z_0-9]*)[\s\x1c-\x1f]*=").unwrap())
}

/// `ASSIGNMENT.match(line).group(1)`.
pub fn assigned_name(line: &str) -> Option<&str> {
    assignment().captures(line).map(|c| c.get(1).unwrap().as_str())
}

fn section_header(line: &str) -> Option<&str> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[\s\x1c-\x1f]*\[([^\]]+)\][\s\x1c-\x1f]*$").unwrap())
        .captures(line).map(|c| c.get(1).unwrap().as_str())
}

fn valid_value(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || "_:.@-".contains(c))
}

/// An insertion-ordered `dict[str, str]` (Python dict update semantics).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env(pub Vec<(String, String)>);

impl Env {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    pub fn set(&mut self, key: &str, value: &str) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value.to_owned(),
            None => self.0.push((key.to_owned(), value.to_owned())),
        }
    }
    pub fn setdefault(&mut self, key: &str, value: &str) {
        if self.get(key).is_none() {
            self.0.push((key.to_owned(), value.to_owned()));
        }
    }
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }
    /// `[f"{k}={v}" for k, v in values.items()]`.
    pub fn assignments(&self) -> Vec<String> {
        self.0.iter().map(|(k, v)| format!("{k}={v}")).collect()
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Env {
    fn from(items: [(&str, &str); N]) -> Self {
        let mut env = Env::default();
        for (k, v) in items {
            env.set(k, v);
        }
        env
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyResult {
    pub ok: bool,
    pub language: String,
    pub locale: String,
    pub error: String,
    pub warnings: Vec<String>,
    pub rolled_back: bool,
    pub partial: bool,
    pub relogin_required: bool,
}

impl ApplyResult {
    /// `ApplyResult(ok, language, locale)` with the Python defaults.
    pub fn new(ok: bool, language: &str, locale: &str) -> Self {
        ApplyResult { ok, language: language.to_owned(), locale: locale.to_owned(), error: String::new(),
                      warnings: Vec::new(), rolled_back: false, partial: false, relogin_required: true }
    }
}

pub fn normalized_locale(value: &str) -> String {
    suite_core::pyjson::py_strip(value).to_lowercase().replace('-', "")
}

pub fn language_from_locale(value: &str) -> String {
    let candidate = value.split(':').next().unwrap_or("").split('.').next().unwrap_or("")
        .split('@').next().unwrap_or("");
    if language_by_code(candidate).is_some() {
        return candidate.to_owned();
    }
    for item in &LANGUAGES {
        if candidate.to_lowercase() == item.locale.split('.').next().unwrap_or("").to_lowercase() {
            return item.code.to_owned();
        }
    }
    let short = candidate.split('_').next().unwrap_or("").to_lowercase();
    if language_by_code(&short).is_some() { short } else { "en".to_owned() }
}

/// `{LANG, LANGUAGE: "code[:base][:en]", LC_MESSAGES}` — region-specific
/// gettext catalogs must be considered before the base language.
pub fn language_environment(language: &Language) -> Env {
    let mut codes = vec![language.code];
    let base = language.code.split('_').next().unwrap_or("");
    if !codes.contains(&base) {
        codes.push(base);
    }
    if !codes.contains(&"en") {
        codes.push("en");
    }
    Env::from([("LANG", language.locale), ("LANGUAGE", &codes.join(":")), ("LC_MESSAGES", language.locale)])
}

pub fn locale_mapping<S: AsRef<str>>(values: &[S]) -> Env {
    let mut result = Env::default();
    for item in values {
        let item = item.as_ref();
        if let Some((key, value)) = item.split_once('=') {
            if is_locale_key(key) && !value.is_empty() {
                result.set(key, value);
            }
        }
    }
    result
}

/// localed omits LC_* and LANGUAGE entries equal to LANG in its read-back.
pub fn equivalent_locales<S: AsRef<str>, T: AsRef<str>>(first: &[S], second: &[T]) -> bool {
    let (left, right) = (locale_mapping(first), locale_mapping(second));
    LOCALE_KEYS.iter().all(|key| {
        let a = left.get(key).or(left.get("LANG")).unwrap_or("C");
        let b = right.get(key).or(right.get("LANG")).unwrap_or("C");
        normalized_locale(a) == normalized_locale(b)
    })
}

/// Express a complete effective state even when SetLocale merges omissions.
/// Empty values are not accepted by localed; inherited categories are reset by
/// explicitly repeating LANG.
pub fn system_locale_assignments<S: AsRef<str>>(values: &[S]) -> Vec<String> {
    let mapping = locale_mapping(values);
    let fallback = mapping.get("LANG").unwrap_or("C").to_owned();
    LOCALE_KEYS.iter().map(|key| format!("{key}={}", mapping.get(key).unwrap_or(&fallback))).collect()
}

pub fn system_language_values<S: AsRef<str>>(old: &[S], language: &Language) -> Vec<String> {
    let mut values = locale_mapping(old);
    let fallback = values.get("LANG").unwrap_or("C.UTF-8").to_owned();
    for key in FORMAT_KEYS {
        values.setdefault(key, &fallback);
    }
    // localed validates LANGUAGE as one installed locale too, not as a gettext
    // code/list; short-code fallbacks stay in language_environment().
    values.set("LANG", language.locale);
    values.set("LANGUAGE", language.locale);
    values.set("LC_MESSAGES", language.locale);
    system_locale_assignments(&values.assignments())
}

/// `str.splitlines(keepends=True)`.
pub fn splitlines_keepends(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let end = match ch {
            '\r' => match chars.peek() {
                Some((next, '\n')) => {
                    let end = next + 1;
                    chars.next();
                    end
                }
                _ => index + 1,
            },
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => index + ch.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn strip_newlines(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn without_language_block(text: &str) -> Result<Vec<&str>, String> {
    let mut lines = splitlines_keepends(text);
    let begin: Vec<usize> = (0..lines.len()).filter(|&i| strip_newlines(lines[i]) == MARKER_BEGIN).collect();
    let end: Vec<usize> = (0..lines.len()).filter(|&i| strip_newlines(lines[i]) == MARKER_END).collect();
    if begin.len() != end.len() || begin.len() > 1 || (!begin.is_empty() && end[0] < begin[0]) {
        return Err(tr!("The saved language section is incomplete. Check the environment file before retrying."));
    }
    if let (Some(&b), Some(&e)) = (begin.first(), end.first()) {
        lines.drain(b..=e);
    }
    Ok(lines)
}

/// Replace only language assignments, leaving unrelated settings/comments intact.
pub fn update_environment(text: &str, values: &Env, shell: bool, systemd: bool) -> Result<String, String> {
    let lines = without_language_block(text)?;
    let mut output: String = lines.into_iter()
        .filter(|line| !assigned_name(line).is_some_and(|name| LANGUAGE_KEYS.contains(&name)))
        .collect();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    if !output.is_empty() && !output.ends_with("\n\n") {
        output.push('\n');
    }
    output.push_str(MARKER_BEGIN);
    output.push('\n');
    // Fedora/Nobara lang.sh reads ~/.i18n with a restricted NAME=value parser
    // and adds export itself.
    for (key, value) in &values.0 {
        if !is_locale_key(key) || !valid_value(value) {
            return Err(tr!("An invalid locale value was rejected."));
        }
        output.push_str(&format!("{key}={value}\n"));
    }
    // environment.d cannot unset and rejects an empty assignment.
    if !systemd {
        output.push_str(if shell { "unset LC_ALL\n" } else { "LC_ALL=\n" });
    }
    output.push_str(MARKER_END);
    output.push('\n');
    Ok(output)
}

/// Keep KDE's existing regional Formats; update its Translations language list.
pub fn update_kde_language(text: &str, values: &Env) -> String {
    let language = values.get("LANGUAGE").unwrap_or("");
    let mut section: Option<String> = None;
    let mut inserted = false;
    let mut result: Vec<String> = Vec::new();
    for line in splitlines_keepends(text) {
        if let Some(name) = section_header(line) {
            if section.as_deref() == Some("Translations") && !inserted {
                result.push(format!("LANGUAGE={language}\n"));
                inserted = true;
            }
            section = Some(name.to_owned());
        }
        if section.as_deref() == Some("Translations") && assigned_name(line) == Some("LANGUAGE") {
            if !inserted {
                result.push(format!("LANGUAGE={language}\n"));
                inserted = true;
            }
            continue;
        }
        result.push(line.to_owned());
    }
    if !inserted {
        if let Some(last) = result.last_mut() {
            if !last.ends_with('\n') {
                last.push('\n');
            }
        }
        if section.as_deref() != Some("Translations") {
            result.push("\n[Translations]\n".to_owned());
        }
        result.push(format!("LANGUAGE={language}\n"));
    }
    result.concat()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileState {
    pub contents: Option<Vec<u8>>,
    pub mode: u32,
}

impl FileState {
    pub const MISSING: FileState = FileState { contents: None, mode: 0o600 };
}

pub fn file_state(path: &Path) -> Result<FileState, String> {
    let info = match std::fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(FileState::MISSING),
        Err(error) => return Err(os_error_path(&error, path)),
    };
    if !info.file_type().is_file() {
        return Err(tr!("Cannot update {path}: it is not a regular file.", path = path.display().to_string()));
    }
    let contents = std::fs::read(path).map_err(|error| os_error_path(&error, path))?;
    Ok(FileState { contents: Some(contents), mode: info.mode() & 0o7777 })
}

/// `bytes.decode("utf-8")`, with Python's `UnicodeDecodeError` text.
pub fn decode_utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|error| {
        let position = error.valid_up_to();
        let reason = match error.error_len() {
            None => "unexpected end of data",
            Some(_) if (0x80..0xc2).contains(&bytes[position]) || bytes[position] >= 0xf5 => "invalid start byte",
            Some(_) => "invalid continuation byte",
        };
        format!("'utf-8' codec can't decode byte 0x{:02x} in position {position}: {reason}", bytes[position])
    })
}

/// mkstemp(prefix=".<name>.") + write + fsync + chmod + os.replace.
pub fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| os_error_path(&error, parent))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut file, temporary) = loop {
        let candidate = parent.join(format!(".{name}.{}", suite_core::secrets::token_urlsafe(6).replace('-', "_")));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&candidate) {
            Ok(file) => break (file, candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(os_error_path(&error, &candidate)),
        }
    };
    let result = (|| {
        file.write_all(contents).map_err(|e| suite_core::pyerr::os_error_text(&e))?;
        file.flush().map_err(|e| suite_core::pyerr::os_error_text(&e))?;
        file.sync_all().map_err(|e| suite_core::pyerr::os_error_text(&e))?;
        drop(file);
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))
            .map_err(|e| os_error_path(&e, &temporary))?;
        std::fs::rename(&temporary, path).map_err(|e| {
            format!("{} -> '{}'", os_error_path(&e, &temporary), path.display())
        })
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// The D-Bus services the backend uses (Python `LocaleServices`).
pub trait Services: Send + Sync {
    fn get_locale(&self) -> Result<Vec<String>, String>;
    fn set_locale(&self, values: &[String]) -> Result<(), String>;
    /// `(account object path, language)`, `None` when AccountsService is absent.
    fn get_account(&self) -> Result<Option<(String, String)>, String>;
    fn set_account_language(&self, path: &str, language: &str) -> Result<(), String>;
    fn update_session(&self, values: &Env) -> Result<Vec<String>, String>;
}

pub type LocaleProvider = Box<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;
pub type Writer = Box<dyn Fn(&Path, &[u8], u32) -> Result<(), String> + Send + Sync>;

/// `subprocess.run(["locale", "-a"], check=True, text=True, timeout=10, env=LC_ALL=C)`.
pub fn installed_locales() -> Result<Vec<String>, String> {
    use std::process::{Command, Stdio};
    let command = "Command '['locale', '-a']'";
    let mut child = Command::new("locale").arg("-a").env("LC_ALL", "C")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .map_err(|error| os_error_path(&error, Path::new("locale")))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let drain = std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = stderr.read_to_end(&mut sink);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{command} timed out after 10 seconds"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(suite_core::pyerr::os_error_text(&error)),
        }
    };
    let out = reader.join().unwrap_or_default();
    let _ = drain.join();
    use std::os::unix::process::ExitStatusExt;
    if let Some(signal) = status.signal() {
        return Err(format!("{command} died with signal {signal}."));
    }
    if !status.success() {
        return Err(format!("{command} returned non-zero exit status {}.", status.code().unwrap_or(-1)));
    }
    Ok(splitlines_keepends(&String::from_utf8_lossy(&out)).into_iter().map(|l| strip_lineend(l).to_owned()).collect())
}

fn strip_lineend(line: &str) -> &str {
    let mut end = line.len();
    for (index, ch) in line.char_indices().rev() {
        if matches!(ch, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            end = index;
        } else {
            break;
        }
    }
    &line[..end]
}

pub struct LanguageBackend {
    pub home: PathBuf,
    pub config: PathBuf,
    pub preference_path: PathBuf,
    services: OnceLock<Arc<dyn Services>>,
    pub locale_provider: LocaleProvider,
    pub writer: Writer,
    pub environment: HashMap<String, String>,
}

impl Default for LanguageBackend {
    fn default() -> Self {
        Self::new(None, None, None)
    }
}

type Plan = (PathBuf, FileState, Vec<u8>);

impl LanguageBackend {
    /// `LanguageBackend(home=…, config_dir=…, services=…)`; the environment is
    /// a copy of the process environment.
    pub fn new(home: Option<PathBuf>, config_dir: Option<PathBuf>, services: Option<Arc<dyn Services>>) -> Self {
        let config = match (&config_dir, &home) {
            (Some(config), _) => config.clone(),
            (None, Some(home)) => home.join(".config"),
            (None, None) => suite_core::paths::config_home(),
        };
        let home = home.unwrap_or_else(suite_core::paths::home);
        let preference_path = config.join("desktop-suite").join("language.json");
        let cell = OnceLock::new();
        if let Some(services) = services {
            let _ = cell.set(services);
        }
        LanguageBackend {
            home, config, preference_path, services: cell,
            locale_provider: Box::new(installed_locales),
            writer: Box::new(atomic_write),
            environment: std::env::vars().collect(),
        }
    }

    pub fn services(&self) -> &Arc<dyn Services> {
        self.services.get_or_init(|| Arc::new(crate::services::LocaleServices::new()))
    }

    fn installed(&self) -> Result<HashSet<String>, String> {
        Ok((self.locale_provider)()?.iter().map(|v| normalized_locale(v)).collect())
    }

    fn preferences(&self) -> Result<Map<String, Value>, String> {
        let bytes = match std::fs::read(&self.preference_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => {
                let error = os_error_path(&error, &self.preference_path);
                return Err(tr!("Cannot read the saved component language: {error}", error = error));
            }
        };
        let value: Value = decode_utf8(&bytes).and_then(|text| serde_json::from_str(text).map_err(|e| e.to_string()))
            .map_err(|error| tr!("Cannot read the saved component language: {error}", error = error))?;
        match value {
            Value::Object(map) => Ok(map),
            _ => Err(tr!("The saved component language is not a settings object.")),
        }
    }

    /// The current state; never writes.
    pub fn status(&self) -> Result<Value, String> {
        let mut warnings: Vec<String> = Vec::new();
        let installed = match self.installed() {
            Ok(installed) => installed,
            Err(error) => {
                warnings.push(tr!("Could not check installed languages: {error}", error = error));
                HashSet::new()
            }
        };
        let (system, available) = match self.services().get_locale() {
            Ok(system) => (system, true),
            Err(error) => {
                warnings.push(tr!("System language settings are unavailable: {error}", error = error));
                (Vec::new(), false)
            }
        };
        let account = match self.services().get_account() {
            Ok(account) => account,
            Err(error) => {
                warnings.push(tr!("Login language settings are unavailable: {error}", error = error));
                None
            }
        };
        let preference = self.preferences().unwrap_or_else(|error| {
            warnings.push(error);
            Map::new()
        });
        let system_locale = locale_mapping(&system).get("LANG").unwrap_or("").to_owned();
        let account_language = account.as_ref().map(|a| a.1.as_str()).unwrap_or("");
        let current_locale = if !account_language.is_empty() {
            account_language.to_owned()
        } else if !system_locale.is_empty() {
            system_locale.clone()
        } else {
            std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".to_owned())
        };
        let code = match preference.get("language") {
            Some(Value::String(code)) if language_by_code(code).is_some() => code.clone(),
            // `code not in LANGUAGE_BY_CODE` raises for unhashable values.
            Some(Value::Array(_)) => return Err("unhashable type: 'list'".to_owned()),
            Some(Value::Object(_)) => return Err("unhashable type: 'dict'".to_owned()),
            _ => language_from_locale(&current_locale),
        };
        let languages: Vec<Value> = LANGUAGES.iter().map(|item| serde_json::json!({
            "code": item.code, "name": item.name, "locale": item.locale,
            "installed": installed.contains(&normalized_locale(item.locale)),
        })).collect();
        let mut out = Map::new();
        out.insert("language".into(), Value::String(code));
        out.insert("locale".into(), preference.get("locale").cloned().unwrap_or(Value::String(current_locale)));
        out.insert("system_locale".into(), Value::String(system_locale));
        out.insert("system_available".into(), Value::Bool(available));
        out.insert("account_available".into(), Value::Bool(account.is_some()));
        out.insert("languages".into(), Value::Array(languages));
        out.insert("warnings".into(), Value::Array(warnings.into_iter().map(Value::String).collect()));
        Ok(Value::Object(out))
    }

    /// The `--status` line: `json.dumps(status, ensure_ascii=False)`.
    pub fn status_json(&self) -> Result<String, String> {
        Ok(json_dumps(&self.status()?, &JsonStyle { ensure_ascii: false, ..JsonStyle::DEFAULT }))
    }

    /// A user's regional formats may differ from the system defaults: keep
    /// their effective values when removing an inherited LC_ALL override.
    pub fn user_environment<S: AsRef<str>>(&self, language: &Language, system_values: &[S]) -> Env {
        let system = locale_mapping(system_values);
        let get = |key: &str| self.environment.get(key).filter(|v| !v.is_empty()).map(String::as_str);
        let mut values = Env::default();
        for key in FORMAT_KEYS {
            let value = get("LC_ALL").or(get(key)).or(get("LANG"))
                .unwrap_or_else(|| system.get(key).unwrap_or("C.UTF-8"));
            values.set(key, value);
        }
        for (key, value) in language_environment(language).0 {
            values.set(&key, &value);
        }
        values
    }

    fn file_updates(&self, language: &Language, system_values: &[String]) -> Result<Vec<Plan>, String> {
        let values = self.user_environment(language, system_values);
        let mut plans = Vec::new();
        let targets = [
            (self.config.join("environment.d").join("90-desktop-suite-language.conf"), false),
            (self.config.join("labwc").join("environment"), false),
            (self.home.join(".i18n"), true),
        ];
        for (path, shell) in targets {
            let old = file_state(&path)?;
            let text = decode_utf8(old.contents.as_deref().unwrap_or(b""))?.to_owned();
            // Regional values already explicitly present in a user file win.
            let explicit: HashSet<&str> = without_language_block(&text)?.into_iter()
                .filter_map(assigned_name).filter(|name| FORMAT_KEYS.contains(name)).collect();
            let effective = Env(values.0.iter().filter(|(k, _)| !explicit.contains(k.as_str())).cloned().collect());
            let systemd = path.parent().and_then(Path::file_name).is_some_and(|n| n == "environment.d");
            let new = update_environment(&text, &effective, shell, systemd)?.into_bytes();
            plans.push((path, old, new));
        }
        let kde_path = self.config.join("plasma-localerc");
        let kde_old = file_state(&kde_path)?;
        // Do not create KDE settings on desktops which have never used KDE.
        if let Some(contents) = &kde_old.contents {
            let text = decode_utf8(contents)?;
            let new = update_kde_language(text, &language_environment(language)).into_bytes();
            plans.push((kde_path, kde_old, new));
        }
        let old = file_state(&self.preference_path)?;
        let mut preferences = self.preferences()?;
        preferences.insert("version".into(), Value::from(1));
        preferences.insert("language".into(), Value::String(language.code.into()));
        preferences.insert("locale".into(), Value::String(language.locale.into()));
        let style = JsonStyle { ensure_ascii: false, indent: Some(2), ..JsonStyle::DEFAULT };
        let text = json_dumps(&Value::Object(preferences), &style) + "\n";
        plans.push((self.preference_path.clone(), old, text.into_bytes()));
        Ok(plans)
    }

    pub fn apply(&self, code: &str, progress: Option<&dyn Fn(&str)>) -> ApplyResult {
        let Some(language) = language_by_code(code) else {
            let mut result = ApplyResult::new(false, code, "");
            result.error = tr!("Choose one of the supported languages.");
            result.relogin_required = false;
            return result;
        };
        let notify = |stage: String| if let Some(progress) = progress { progress(&stage) };
        let services = self.services().clone();
        let mut written: Vec<(PathBuf, FileState, FileState)> = Vec::new();
        let mut old_system: Vec<String> = Vec::new();
        let mut old_account: Option<(String, String)> = None;
        let mut system_attempted = false;
        let mut account_attempted = false;
        let mut requested_system: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        let outcome: Result<(), String> = (|| {
            notify(tr!("Checking installed language support…"));
            if !self.installed()?.contains(&normalized_locale(language.locale)) {
                return Err(tr!("{language} requires the {locale} locale. Install its language support first.",
                               language = language.name, locale = language.locale));
            }
            old_system = services.get_locale()?;
            old_account = services.get_account()?;
            requested_system = system_language_values(&old_system, language);
            let plans = self.file_updates(language, &requested_system)?;
            // All user-file content is validated before privilege prompts.
            notify(tr!("Saving system language…"));
            system_attempted = true;
            services.set_locale(&requested_system)?;
            let observed = services.get_locale()?;
            if normalized_locale(locale_mapping(&observed).get("LANG").unwrap_or("")) != normalized_locale(language.locale) {
                return Err(tr!("The system did not save the selected language."));
            }
            if let Some((path, _)) = &old_account {
                notify(tr!("Saving login language…"));
                account_attempted = true;
                services.set_account_language(path, language.locale)?;
                let current = services.get_account()?;
                if !current.is_some_and(|c| normalized_locale(&c.1) == normalized_locale(language.locale)) {
                    return Err(tr!("The login language could not be verified."));
                }
            } else {
                warnings.push(tr!("The login manager does not provide account language settings; the system and session preferences were saved."));
            }
            notify(tr!("Saving component preferences…"));
            for (path, before, contents) in plans {
                if file_state(&path)? != before {
                    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    return Err(tr!("{file} changed while applying. Please retry.", file = file));
                }
                if before.contents.as_deref() == Some(contents.as_slice()) {
                    continue;
                }
                // Recording after the write makes rollback unambiguous.
                (self.writer)(&path, &contents, before.mode)?;
                let after = FileState { contents: Some(contents), mode: before.mode };
                written.push((path, before, after));
            }
            Ok(())
        })();

        if let Err(error) = outcome {
            let mut rollback_errors: Vec<String> = Vec::new();
            let mut had_changes = !written.is_empty();
            if !written.is_empty() || system_attempted || account_attempted {
                notify(tr!("Restoring previous settings…"));
            }
            for (path, before, after) in written.iter().rev() {
                let restored = (|| {
                    if file_state(path)? != *after {
                        return Err(tr!("The file was changed by another application."));
                    }
                    match &before.contents {
                        None => std::fs::remove_file(path).map_err(|e| os_error_path(&e, path)),
                        Some(contents) => atomic_write(path, contents, before.mode),
                    }
                })();
                if let Err(rollback_error) = restored {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    rollback_errors.push(format!("{name}: {rollback_error}"));
                }
            }
            if account_attempted {
                let restored = (|| {
                    let current = services.get_account()?;
                    let old = old_account.as_ref().expect("account attempted");
                    if current.as_ref() == Some(old) {
                        Ok(())
                    } else if current.as_ref().is_some_and(|c| c.0 == old.0
                        && normalized_locale(&c.1) == normalized_locale(language.locale)) {
                        had_changes = true;
                        services.set_account_language(&old.0, &old.1)
                    } else {
                        Err(tr!("The login language changed again before it could be restored."))
                    }
                })();
                if let Err(rollback_error) = restored {
                    rollback_errors.push(tr!("Login language: {error}", error = rollback_error));
                }
            }
            if system_attempted {
                let restored = (|| {
                    let current = services.get_locale()?;
                    if equivalent_locales(&current, &old_system) {
                        Ok(())
                    } else if equivalent_locales(&current, &requested_system) {
                        had_changes = true;
                        services.set_locale(&system_locale_assignments(&old_system))?;
                        let restored = services.get_locale()?;
                        if !equivalent_locales(&restored, &old_system) {
                            return Err(tr!("The system language could not be restored."));
                        }
                        if !locale_mapping(&old_system).contains("LANG") && locale_mapping(&restored).contains("LANG") {
                            return Err(tr!("The system service could not restore an unset default locale."));
                        }
                        Ok(())
                    } else {
                        Err(tr!("The system language changed again before it could be restored."))
                    }
                })();
                if let Err(rollback_error) = restored {
                    rollback_errors.push(tr!("System language: {error}", error = rollback_error));
                }
            }
            let failed = !rollback_errors.is_empty();
            return ApplyResult {
                ok: false, language: language.code.into(), locale: language.locale.into(), error,
                rolled_back: had_changes && !failed, partial: failed, relogin_required: failed,
                warnings: rollback_errors,
            };
        }
        notify(tr!("Updating the session environment…"));
        match services.update_session(&self.user_environment(language, &requested_system)) {
            Ok(more) => warnings.extend(more),
            Err(error) => warnings.push(tr!("The current session will use the new language after login: {error}", error = error)),
        }
        let mut result = ApplyResult::new(true, language.code, language.locale);
        result.warnings = warnings;
        result
    }
}
