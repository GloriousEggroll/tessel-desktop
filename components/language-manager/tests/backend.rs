//! test_language_backend.py: fake-service tests; never change the machine's
//! locale or login settings. Every file lives under a fresh directory in TMPDIR.

use language_manager::backend::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type LocaleHook = Box<dyn Fn(&Fake, &[String]) -> Result<(), String> + Send + Sync>;
type AccountHook = Box<dyn Fn(&Fake, &str, &str) -> Result<(), String> + Send + Sync>;

#[derive(Default)]
struct State {
    locale: Vec<String>,
    account: Option<(String, String)>,
    calls: Vec<(String, Vec<String>)>,
    session: Vec<Env>,
    fail_system: bool,
    fail_account: bool,
    fail_session: bool,
    fail_restore: bool,
}

#[derive(Default)]
struct Fake {
    state: Mutex<State>,
    set_locale_hook: Mutex<Option<LocaleHook>>,
    set_account_hook: Mutex<Option<AccountHook>>,
}

impl Fake {
    fn new() -> Arc<Self> {
        let fake = Fake::default();
        {
            let mut s = fake.state.lock().unwrap();
            s.locale = vec!["LANG=en_US.utf8".into(), "LC_TIME=en_GB.UTF-8".into()];
            s.account = Some(("/user/1000".into(), "en_US.utf8".into()));
        }
        Arc::new(fake)
    }
    fn s(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
    fn locale(&self) -> Vec<String> {
        self.s().locale.clone()
    }
    fn set_locale_list(&self, values: &[&str]) {
        self.s().locale = values.iter().map(|v| v.to_string()).collect();
    }
    fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.s().calls.clone()
    }
    /// Model systemd 259's validation, merge and redundant-value removal,
    /// including its validation of LANGUAGE as an installed locale.
    fn original_set_locale(&self, values: &[String]) -> Result<(), String> {
        let mut s = self.s();
        s.calls.push(("system".into(), values.to_vec()));
        if s.fail_system || (s.fail_restore && values.iter().any(|v| v == "LANG=en_US.utf8")) {
            return Err("Authentication cancelled".into());
        }
        let mut installed: Vec<String> = LANGUAGES.iter().map(|l| normalized_locale(l.locale)).collect();
        installed.extend(["en_gb.utf8", "c", "c.utf8", "posix"].map(String::from));
        let mut incoming = Env::default();
        for assignment in values {
            let (key, value) = assignment.split_once('=').unwrap();
            if !is_locale_key(key) || !installed.contains(&normalized_locale(value)) {
                return Err(format!("Locale {value} not installed, refusing."));
            }
            incoming.set(key, value);
        }
        let mut merged = locale_mapping(&s.locale);
        for (k, v) in &incoming.0 {
            merged.set(k, v);
        }
        let lang = merged.get("LANG").map(str::to_owned);
        s.locale = merged.0.iter().filter(|(k, v)| k == "LANG" || Some(v) != lang.as_ref())
            .map(|(k, v)| format!("{k}={v}")).collect();
        Ok(())
    }
    fn original_set_account(&self, path: &str, value: &str) -> Result<(), String> {
        let mut s = self.s();
        s.calls.push(("account".into(), vec![value.into()]));
        if s.fail_account {
            return Err("Login language permission denied".into());
        }
        s.account = Some((path.into(), value.into()));
        Ok(())
    }
}

impl Services for Fake {
    fn get_locale(&self) -> Result<Vec<String>, String> {
        Ok(self.locale())
    }
    fn set_locale(&self, values: &[String]) -> Result<(), String> {
        let hook = self.set_locale_hook.lock().unwrap();
        match hook.as_ref() {
            Some(hook) => hook(self, values),
            None => self.original_set_locale(values),
        }
    }
    fn get_account(&self) -> Result<Option<(String, String)>, String> {
        Ok(self.s().account.clone())
    }
    fn set_account_language(&self, path: &str, language: &str) -> Result<(), String> {
        let hook = self.set_account_hook.lock().unwrap();
        match hook.as_ref() {
            Some(hook) => hook(self, path, language),
            None => self.original_set_account(path, language),
        }
    }
    fn update_session(&self, values: &Env) -> Result<Vec<String>, String> {
        let mut s = self.s();
        s.calls.push(("session".into(), values.assignments()));
        s.session.push(values.clone());
        if s.fail_session {
            return Err("Session bus unavailable".into());
        }
        Ok(Vec::new())
    }
}

struct Case {
    home: PathBuf,
    services: Arc<Fake>,
    backend: LanguageBackend,
}

fn scratch() -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = base.join(format!("lm-backend-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn setup() -> Case {
    let home = scratch();
    let services = Fake::new();
    let mut backend = LanguageBackend::new(Some(home.clone()), None, Some(services.clone() as Arc<dyn Services>));
    backend.locale_provider = Box::new(|| Ok(LANGUAGES.iter().map(|l| l.locale.replace("UTF-8", "utf8")).collect()));
    backend.environment = Default::default();
    Case { home, services, backend }
}

fn empty(dir: &Path) -> bool {
    fs::read_dir(dir).unwrap().next().is_none()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { stack.push(path) } else { out.push((path.clone(), fs::read(&path).unwrap())) }
        }
    }
    out.sort();
    out
}

#[test]
fn catalog_exactly_matches_previous_bluetooth_names() {
    assert_eq!(LANGUAGES.len(), 31);
    let source = Path::new("/home/tcrider/btmanager/translations.py");
    if !source.exists() {
        return;
    }
    let script = "import ast,json,sys\ntree=ast.parse(open(sys.argv[1]).read())\n\
        old=next(ast.literal_eval(i.value) for i in tree.body if isinstance(i, ast.Assign) and any(getattr(t,'id',None)=='LANG_NAMES' for t in i.targets))\n\
        print(json.dumps(old))";
    let output = std::process::Command::new("python3").args(["-I", "-c", script]).arg(source).output().unwrap();
    let old: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let ours: serde_json::Map<String, serde_json::Value> =
        LANGUAGES.iter().map(|l| (l.code.to_owned(), serde_json::Value::from(l.name))).collect();
    assert_eq!(ours, old);
}

#[test]
fn status_does_not_change_anything() {
    let c = setup();
    let result = c.backend.status().unwrap();
    assert_eq!(result["language"], "en");
    assert_eq!(result["system_available"], true);
    assert_eq!(result["account_available"], true);
    assert!(result["languages"].as_array().unwrap().iter().all(|l| l["installed"] == true));
    let keys: Vec<&str> = result.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["language", "locale", "system_locale", "system_available", "account_available", "languages", "warnings"]);
    assert!(c.services.calls().is_empty());
    assert!(empty(&c.home));
}

#[test]
fn status_json_is_python_json_dumps() {
    let c = setup();
    c.services.s().account = None;
    let text = c.backend.status_json().unwrap();
    assert!(text.starts_with("{\"language\": \"en\", \"locale\": \""), "{text}");
    assert!(text.contains("{\"code\": \"zh_CN\", \"name\": \"简体中文\", \"locale\": \"zh_CN.UTF-8\", \"installed\": true}"));
    assert!(text.ends_with("\"account_available\": false, \"languages\": [") == false);
    assert!(text.ends_with("], \"warnings\": []}"));
}

#[test]
fn status_reports_unavailable_services_and_bad_preferences() {
    let mut c = setup();
    c.backend.locale_provider = Box::new(|| Err("Command '['locale', '-a']' returned non-zero exit status 1.".into()));
    fs::create_dir_all(c.backend.preference_path.parent().unwrap()).unwrap();
    fs::write(&c.backend.preference_path, "[1]").unwrap();
    let result = c.backend.status().unwrap();
    let warnings: Vec<&str> = result["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert_eq!(warnings, ["Could not check installed languages: Command '['locale', '-a']' returned non-zero exit status 1.",
                          "The saved component language is not a settings object."]);
    assert!(result["languages"].as_array().unwrap().iter().all(|l| l["installed"] == false));
    fs::write(&c.backend.preference_path, r#"{"language": "fr", "locale": 5}"#).unwrap();
    let result = c.backend.status().unwrap();
    assert_eq!(result["language"], "fr");
    assert_eq!(result["locale"], 5);
}

#[test]
fn missing_locale_and_unknown_code_are_rejected_before_changes() {
    let mut c = setup();
    c.backend.locale_provider = Box::new(|| Ok(vec!["en_US.utf8".into()]));
    let result = c.backend.apply("fr", None);
    assert!(!result.ok);
    assert!(result.error.contains("Install"));
    assert_eq!(result.error, "Français requires the fr_FR.UTF-8 locale. Install its language support first.");
    let evil = c.backend.apply("../../evil", None);
    assert!(!evil.ok);
    assert!(!evil.relogin_required);
    assert_eq!(evil.language, "../../evil");
    assert_eq!(evil.error, "Choose one of the supported languages.");
    assert!(c.services.calls().is_empty());
    assert!(empty(&c.home));
}

#[test]
fn success_updates_system_account_preferences_and_session() {
    let c = setup();
    let stages = Mutex::new(Vec::<String>::new());
    let result = c.backend.apply("pt_BR", Some(&|s: &str| stages.lock().unwrap().push(s.to_owned())));
    assert!(result.ok, "{}", result.error);
    assert!(result.relogin_required);
    assert_eq!(c.services.s().account.as_ref().unwrap().1, "pt_BR.UTF-8");
    let locale = c.services.locale();
    assert!(locale.contains(&"LANG=pt_BR.UTF-8".to_owned()));
    let calls = c.services.calls();
    assert!(calls[0].1.contains(&"LANGUAGE=pt_BR.UTF-8".to_owned()));
    assert!(!locale_mapping(&locale).contains("LANGUAGE"));
    assert!(locale.contains(&"LC_TIME=en_GB.UTF-8".to_owned()));
    assert!(locale.contains(&"LC_NUMERIC=en_US.utf8".to_owned()));
    let preference: serde_json::Value = serde_json::from_str(&read(&c.backend.preference_path)).unwrap();
    assert_eq!(preference, serde_json::json!({"version": 1, "language": "pt_BR", "locale": "pt_BR.UTF-8"}));
    assert_eq!(read(&c.backend.preference_path), "{\n  \"version\": 1,\n  \"language\": \"pt_BR\",\n  \"locale\": \"pt_BR.UTF-8\"\n}\n");
    assert_eq!(calls.last().unwrap().0, "session");
    assert_eq!(c.services.s().session.last().unwrap().get("LANGUAGE"), Some("pt_BR:pt:en"));
    let stages = stages.into_inner().unwrap();
    assert_eq!(stages, ["Checking installed language support…", "Saving system language…", "Saving login language…",
                        "Saving component preferences…", "Updating the session environment…"]);
    assert!(!c.home.join(".config/plasma-localerc").exists());
}

#[test]
fn written_files_match_python_bytes() {
    let c = setup();
    assert!(c.backend.apply("fr", None).ok);
    let formats = "LC_NUMERIC=en_US.utf8\nLC_TIME=en_GB.UTF-8\nLC_COLLATE=en_US.utf8\nLC_MONETARY=en_US.utf8\nLC_PAPER=en_US.utf8\n\
        LC_NAME=en_US.utf8\nLC_ADDRESS=en_US.utf8\nLC_TELEPHONE=en_US.utf8\nLC_MEASUREMENT=en_US.utf8\nLC_IDENTIFICATION=en_US.utf8\n\
        LANG=fr_FR.UTF-8\nLANGUAGE=fr:en\nLC_MESSAGES=fr_FR.UTF-8\n";
    assert_eq!(read(&c.home.join(".config/environment.d/90-desktop-suite-language.conf")),
               format!("{MARKER_BEGIN}\n{formats}{MARKER_END}\n"));
    assert_eq!(read(&c.home.join(".config/labwc/environment")), format!("{MARKER_BEGIN}\n{formats}LC_ALL=\n{MARKER_END}\n"));
    assert_eq!(read(&c.home.join(".i18n")), format!("{MARKER_BEGIN}\n{formats}unset LC_ALL\n{MARKER_END}\n"));
    let mode = fs::metadata(c.home.join(".i18n")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn all_languages_use_installed_locales_for_system_and_codes_for_apps() {
    let c = setup();
    for language in &LANGUAGES {
        c.services.s().calls.clear();
        let result = c.backend.apply(language.code, None);
        assert!(result.ok, "{}: {}", language.code, result.error);
        let calls = c.services.calls();
        let request = locale_mapping(&calls[0].1);
        for key in ["LANG", "LANGUAGE", "LC_MESSAGES"] {
            assert_eq!(request.get(key), Some(language.locale));
        }
        let mut keys: Vec<&str> = request.keys().collect();
        keys.sort();
        assert_eq!(keys, LOCALE_KEYS);
        let session = c.services.s().session.last().unwrap().clone();
        assert_eq!(session.get("LANGUAGE").unwrap().split(':').next(), Some(language.code));
    }
}

#[test]
fn short_system_language_is_rejected_by_the_service_fixture() {
    let c = setup();
    let err = c.services.set_locale(&["LANG=en_US.UTF-8".into(), "LANGUAGE=en".into()]).unwrap_err();
    assert!(err.contains("Locale en not installed"));
    let err = c.services.set_locale(&["LANG=ja_JP.UTF-8".into(), "LANGUAGE=ja:en".into()]).unwrap_err();
    assert!(err.contains("Locale ja:en not installed"));
}

#[test]
fn stale_system_language_is_replaced_and_simplification_is_equivalent() {
    let c = setup();
    c.services.s().locale.push("LANGUAGE=de_DE.UTF-8".into());
    assert!(c.backend.apply("en", None).ok);
    assert!(!locale_mapping(&c.services.locale()).contains("LANGUAGE"));
    assert!(equivalent_locales(&c.services.locale(), &c.services.calls()[0].1));
}

#[test]
fn rollback_resets_categories_introduced_by_the_request() {
    let c = setup();
    c.services.set_locale_list(&["LANG=en_US.utf8"]);
    c.services.s().fail_account = true;
    let result = c.backend.apply("ja", None);
    assert!(result.rolled_back, "{result:?}");
    assert!(!result.partial);
    assert_eq!(c.services.locale(), ["LANG=en_US.utf8"]);
    let rollback = locale_mapping(&c.services.calls().last().unwrap().1);
    assert_eq!(rollback.get("LC_MESSAGES"), Some("en_US.utf8"));
    assert_eq!(rollback.get("LANGUAGE"), Some("en_US.utf8"));
}

#[test]
fn rollback_does_not_claim_to_restore_an_unset_default_locale() {
    let c = setup();
    c.services.set_locale_list(&[]);
    c.services.s().fail_account = true;
    let result = c.backend.apply("ja", None);
    assert!(!result.rolled_back);
    assert!(result.partial);
    assert!(result.warnings.iter().any(|w| w.contains("unset default locale")));
    assert_eq!(c.services.locale(), ["LANG=C"]);
}

#[test]
fn preserves_unrelated_settings_formats_comments_and_permissions() {
    let c = setup();
    let env = c.home.join(".config/labwc/environment");
    fs::create_dir_all(env.parent().unwrap()).unwrap();
    fs::write(&env, "# My cursor\nXCURSOR_SIZE=42\nLC_TIME=ja_JP.UTF-8\nLANG=de_DE.utf8\nLC_ALL=C\n").unwrap();
    fs::set_permissions(&env, fs::Permissions::from_mode(0o640)).unwrap();
    let kde = c.home.join(".config/plasma-localerc");
    fs::write(&kde, "[Formats]\nLANG=en_US.utf8\nLC_NUMERIC=fr_FR.UTF-8\n\n[Translations]\nLANGUAGE=de\n").unwrap();
    assert!(c.backend.apply("es", None).ok);
    let text = read(&env);
    assert!(text.contains("# My cursor\nXCURSOR_SIZE=42\nLC_TIME=ja_JP.UTF-8\n"));
    assert_eq!(text.matches("LC_TIME=").count(), 1);
    assert!(!text.contains("LC_ALL=C"));
    assert_eq!(fs::metadata(&env).unwrap().permissions().mode() & 0o777, 0o640);
    assert!(read(&kde).contains("[Formats]\nLANG=en_US.utf8\nLC_NUMERIC=fr_FR.UTF-8"));
    assert!(read(&kde).contains("[Translations]\nLANGUAGE=es:en"));
}

#[test]
fn i18n_assignments_are_readable_by_nobara_profile_parser() {
    let c = setup();
    assert!(c.backend.apply("fr", None).ok);
    let pattern = r#"s/^[[:blank:]]*([[:upper:]_]+)=([[:print:][:digit:]\._-]+|"[[:print:][:digit:]\._-]+")/export \1=\2/;t;d"#;
    let output = std::process::Command::new("sed").args(["-r", "-e", pattern]).arg(c.home.join(".i18n")).output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    for line in ["export LANG=fr_FR.UTF-8\n", "export LC_MESSAGES=fr_FR.UTF-8\n", "export LANGUAGE=fr:en\n", "export LC_TIME=en_GB.UTF-8\n"] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

#[test]
fn repeated_apply_is_idempotent_without_losing_regional_defaults() {
    let c = setup();
    assert!(c.backend.apply("ja", None).ok);
    let before = files(&c.home);
    assert!(c.backend.apply("ja", None).ok);
    assert_eq!(before, files(&c.home));
    assert_eq!(read(&c.home.join(".i18n")).matches("LC_TIME=").count(), 1);
}

#[test]
fn user_formats_survive_inherited_lc_all_in_files_and_new_services() {
    let mut c = setup();
    c.backend.environment = [("LANG", "en_US.UTF-8"), ("LC_TIME", "de_DE.UTF-8"), ("LC_ALL", "fr_FR.UTF-8")]
        .iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert!(c.backend.apply("ja", None).ok);
    let environment = c.services.s().session.last().unwrap().clone();
    assert_eq!(environment.get("LANG"), Some("ja_JP.UTF-8"));
    assert_eq!(environment.get("LC_MESSAGES"), Some("ja_JP.UTF-8"));
    assert_eq!(environment.get("LC_TIME"), Some("fr_FR.UTF-8"));
    assert_eq!(environment.get("LC_NUMERIC"), Some("fr_FR.UTF-8"));
    assert!(read(&c.home.join(".i18n")).contains("LC_TIME=fr_FR.UTF-8"));
    assert!(c.services.locale().contains(&"LC_TIME=en_GB.UTF-8".to_owned()));
}

#[test]
fn system_permission_denied_leaves_user_files_and_account_untouched() {
    let c = setup();
    c.services.s().fail_system = true;
    let result = c.backend.apply("fr", None);
    assert!(!result.ok && !result.partial && !result.rolled_back);
    assert_eq!(result.error, "Authentication cancelled");
    assert_eq!(c.services.calls().len(), 1);
    assert!(empty(&c.home));
}

#[test]
fn account_failure_restores_system() {
    let c = setup();
    let before = c.services.locale();
    c.services.s().fail_account = true;
    let result = c.backend.apply("fr", None);
    assert!(!result.ok);
    assert!(result.rolled_back, "{result:?}");
    assert!(!result.partial);
    assert!(!result.relogin_required);
    assert_eq!(c.services.locale(), before);
    assert!(empty(&c.home));
}

#[test]
fn write_failure_rolls_back_files_system_and_account() {
    let mut c = setup();
    let env = c.home.join(".config/labwc/environment");
    fs::create_dir_all(env.parent().unwrap()).unwrap();
    fs::write(&env, "XCURSOR_SIZE=42\n").unwrap();
    let before_system = c.services.locale();
    let before_account = c.services.s().account.clone();
    let preference = c.backend.preference_path.clone();
    c.backend.writer = Box::new(move |path, contents, mode| {
        if path == preference {
            return Err("Disk full".into());
        }
        atomic_write(path, contents, mode)
    });
    let result = c.backend.apply("fr", None);
    assert!(!result.ok);
    assert_eq!(result.error, "Disk full");
    assert!(result.rolled_back, "{result:?}");
    assert_eq!(fs::read(&env).unwrap(), b"XCURSOR_SIZE=42\n");
    assert!(!c.home.join(".i18n").exists());
    assert_eq!(c.services.locale(), before_system);
    assert_eq!(c.services.s().account, before_account);
    assert!(!c.services.calls().iter().any(|call| call.0 == "session"));
    // No temporary files left behind.
    assert!(files(&c.home).iter().all(|(p, _)| !p.file_name().unwrap().to_string_lossy().starts_with('.')
        || p.file_name().unwrap() == ".i18n"));
}

#[test]
fn failed_system_restore_reports_partial_failure() {
    let c = setup();
    c.services.s().fail_account = true;
    c.services.s().fail_restore = true;
    let result = c.backend.apply("fr", None);
    assert!(!result.ok && !result.rolled_back && result.partial && result.relogin_required);
    assert!(result.warnings.iter().any(|w| w.contains("System language")));
    assert_eq!(result.warnings, ["System language: Authentication cancelled"]);
}

#[test]
fn timed_out_system_write_is_detected_and_restored() {
    let c = setup();
    let before = c.services.locale();
    *c.services.set_locale_hook.lock().unwrap() = Some(Box::new(|fake, values| {
        fake.original_set_locale(values)?;
        if values.iter().any(|v| v == "LANG=fr_FR.UTF-8") {
            return Err("The reply timed out".into());
        }
        Ok(())
    }));
    let result = c.backend.apply("fr", None);
    assert!(!result.ok && result.rolled_back && !result.partial, "{result:?}");
    assert_eq!(c.services.locale(), before);
    assert!(empty(&c.home));
}

#[test]
fn timed_out_account_write_is_detected_and_restored() {
    let c = setup();
    let before = c.services.s().account.clone();
    *c.services.set_account_hook.lock().unwrap() = Some(Box::new(|fake, path, value| {
        fake.original_set_account(path, value)?;
        if value == "fr_FR.UTF-8" {
            return Err("The reply timed out".into());
        }
        Ok(())
    }));
    let result = c.backend.apply("fr", None);
    assert!(!result.ok && result.rolled_back && !result.partial, "{result:?}");
    assert_eq!(c.services.s().account, before);
}

#[test]
fn external_locale_change_before_verification_is_not_rolled_back() {
    let c = setup();
    *c.services.set_locale_hook.lock().unwrap() = Some(Box::new(|fake, values| {
        fake.original_set_locale(values)?;
        fake.set_locale_list(&["LANG=de_DE.UTF-8"]);
        Ok(())
    }));
    let result = c.backend.apply("fr", None);
    assert!(!result.ok && result.partial);
    assert_eq!(c.services.locale(), ["LANG=de_DE.UTF-8"]);
    assert_eq!(c.services.calls().len(), 1);
}

#[test]
fn external_regional_edit_before_verification_survives_later_failure() {
    let c = setup();
    *c.services.set_locale_hook.lock().unwrap() = Some(Box::new(|fake, values| {
        fake.original_set_locale(values)?;
        let mut s = fake.s();
        s.locale = s.locale.iter().map(|i| if i.starts_with("LC_TIME=") { "LC_TIME=ja_JP.UTF-8".into() } else { i.clone() }).collect();
        Ok(())
    }));
    c.services.s().fail_account = true;
    let result = c.backend.apply("fr", None);
    assert!(!result.ok && result.partial);
    assert!(c.services.locale().contains(&"LC_TIME=ja_JP.UTF-8".to_owned()));
    assert_eq!(c.services.calls().iter().filter(|c| c.0 == "system").count(), 1);
}

#[test]
fn concurrent_system_change_is_not_overwritten_during_rollback() {
    let c = setup();
    *c.services.set_account_hook.lock().unwrap() = Some(Box::new(|fake, _, _| {
        fake.set_locale_list(&["LANG=de_DE.UTF-8"]);
        Err("Login language denied".into())
    }));
    let result = c.backend.apply("fr", None);
    assert!(!result.ok && result.partial);
    assert_eq!(c.services.locale(), ["LANG=de_DE.UTF-8"]);
}

#[test]
fn account_service_absent_uses_saved_session_environment() {
    let c = setup();
    c.services.s().account = None;
    let result = c.backend.apply("ko", None);
    assert!(result.ok);
    assert!(!result.warnings.is_empty());
    assert!(c.backend.preference_path.exists());
    assert!(!c.services.calls().iter().any(|call| call.0 == "account"));
}

#[test]
fn session_failure_is_saved_success_with_relogin_warning() {
    let c = setup();
    c.services.s().fail_session = true;
    let result = c.backend.apply("de", None);
    assert!(result.ok && result.relogin_required);
    assert_eq!(result.warnings, ["The current session will use the new language after login: Session bus unavailable"]);
    let preference: serde_json::Value = serde_json::from_str(&read(&c.backend.preference_path)).unwrap();
    assert_eq!(preference["language"], "de");
}

#[test]
fn invalid_saved_config_is_not_overwritten() {
    let c = setup();
    fs::create_dir_all(c.backend.preference_path.parent().unwrap()).unwrap();
    fs::write(&c.backend.preference_path, "not JSON").unwrap();
    let result = c.backend.apply("fr", None);
    assert!(!result.ok);
    assert!(result.error.starts_with("Cannot read the saved component language: "), "{}", result.error);
    assert_eq!(read(&c.backend.preference_path), "not JSON");
    assert!(c.services.calls().is_empty());
}

#[test]
fn saved_config_keeps_other_keys_in_order() {
    let c = setup();
    fs::create_dir_all(c.backend.preference_path.parent().unwrap()).unwrap();
    fs::write(&c.backend.preference_path, r#"{"locale": "x", "extra": [1, 2.5, "é"], "version": 1}"#).unwrap();
    assert!(c.backend.apply("fr", None).ok);
    assert_eq!(read(&c.backend.preference_path),
               "{\n  \"locale\": \"fr_FR.UTF-8\",\n  \"extra\": [\n    1,\n    2.5,\n    \"é\"\n  ],\n  \"version\": 1,\n  \"language\": \"fr\"\n}\n");
}

#[test]
fn symlink_user_config_is_not_replaced() {
    let c = setup();
    fs::create_dir_all(c.backend.preference_path.parent().unwrap()).unwrap();
    let target = c.home.join("target");
    fs::write(&target, "{}").unwrap();
    std::os::unix::fs::symlink(&target, &c.backend.preference_path).unwrap();
    let result = c.backend.apply("fr", None);
    assert!(!result.ok);
    assert_eq!(result.error, format!("Cannot update {}: it is not a regular file.", c.backend.preference_path.display()));
    assert!(fs::symlink_metadata(&c.backend.preference_path).unwrap().file_type().is_symlink());
    assert_eq!(read(&target), "{}");
    assert!(c.services.calls().is_empty());
}

#[test]
fn concurrent_user_edit_is_preserved_on_rollback() {
    let mut c = setup();
    let env = c.home.join(".config/labwc/environment");
    fs::create_dir_all(env.parent().unwrap()).unwrap();
    fs::write(&env, "XCURSOR_SIZE=42\n").unwrap();
    let target = env.clone();
    c.backend.writer = Box::new(move |path, contents, mode| {
        atomic_write(path, contents, mode)?;
        if path.file_name().unwrap() == "90-desktop-suite-language.conf" {
            fs::write(&target, "XCURSOR_SIZE=99\n").unwrap();
        }
        Ok(())
    });
    let before = c.services.locale();
    let result = c.backend.apply("fr", None);
    assert!(!result.ok);
    assert_eq!(result.error, "environment changed while applying. Please retry.");
    assert!(result.rolled_back);
    assert_eq!(read(&env), "XCURSOR_SIZE=99\n");
    assert_eq!(c.services.locale(), before);
}

#[test]
fn system_format_fallbacks_are_preserved_when_lang_changes() {
    let values = system_language_values(&["LANG=en_US.UTF-8", "LC_MONETARY=en_GB.UTF-8"], language_by_code("fr").unwrap());
    assert!(values.contains(&"LC_NUMERIC=en_US.UTF-8".to_owned()));
    assert!(values.contains(&"LC_TIME=en_US.UTF-8".to_owned()));
    assert!(values.contains(&"LC_MONETARY=en_GB.UTF-8".to_owned()));
    assert!(!values.iter().any(|v| v.starts_with("LC_ALL=")));
}

#[test]
fn malformed_managed_block_is_not_silently_deleted() {
    let err = update_environment(&format!("{MARKER_BEGIN}\nOTHER=1\n"), &Env::from([("LANG", "en_US.UTF-8")]), false, false).unwrap_err();
    assert_eq!(err, "The saved language section is incomplete. Check the environment file before retrying.");
    let reversed = format!("{MARKER_END}\n{MARKER_BEGIN}\n");
    assert!(update_environment(&reversed, &Env::default(), false, false).is_err());
    assert_eq!(update_environment("A=1\n", &Env::from([("LANG", "en US")]), false, false).unwrap_err(),
               "An invalid locale value was rejected.");
}

#[test]
fn managed_block_is_replaced_in_place_of_old_assignments() {
    let text = format!("A=1\r\nexport LANG=x\n{MARKER_BEGIN}\nLANG=y\n{MARKER_END}\nB=2");
    let out = update_environment(&text, &Env::from([("LANG", "de_DE.UTF-8")]), true, false).unwrap();
    assert_eq!(out, format!("A=1\r\nB=2\n\n{MARKER_BEGIN}\nLANG=de_DE.UTF-8\nunset LC_ALL\n{MARKER_END}\n"));
}

#[test]
fn kde_inserts_language_section_without_changing_formats() {
    let old = "[Formats]\nLANG=de_DE.UTF-8\n";
    let new = update_kde_language(old, &Env::from([("LANGUAGE", "ja:en")]));
    assert!(new.starts_with(old));
    assert_eq!(new.matches("[Translations]").count(), 1);
    assert_eq!(update_kde_language(&new, &Env::from([("LANGUAGE", "ja:en")])), new);
    assert_eq!(update_kde_language("[Translations]\nX=1\n[Other]\n", &Env::from([("LANGUAGE", "fr:en")])),
               "[Translations]\nX=1\nLANGUAGE=fr:en\n[Other]\n");
}

#[test]
fn environment_d_omits_empty_lc_all_that_systemd_rejects() {
    assert!(update_environment("", &Env::from([("LANG", "de_DE.UTF-8")]), false, false).unwrap().contains("LC_ALL=\n"));
    assert!(!update_environment("", &Env::from([("LANG", "de_DE.UTF-8")]), false, true).unwrap().contains("LC_ALL"));
}

#[test]
fn helpers_match_python() {
    assert_eq!(language_from_locale("pt_BR.UTF-8"), "pt_BR");
    assert_eq!(language_from_locale("ja_JP.utf8"), "ja");
    assert_eq!(language_from_locale("de_AT@euro"), "de");
    assert_eq!(language_from_locale("xx_YY"), "en");
    assert_eq!(splitlines_keepends("a\r\nb\rc\x0bd\u{2028}e"), ["a\r\n", "b\r", "c\x0b", "d\u{2028}", "e"]);
    assert_eq!(normalized_locale(" en_US.UTF-8 "), "en_us.utf8");
    assert!(equivalent_locales(&["LANG=en_US.UTF-8"], &["LANG=en_US.utf8", "LC_TIME=en_US.UTF-8"]));
    assert_eq!(decode_utf8(b"ab\xff").unwrap_err(), "'utf-8' codec can't decode byte 0xff in position 2: invalid start byte");
}

/// Same fixtures, same fake services: the Python and Rust backends must write
/// byte-identical files and send identical requests.
#[test]
fn python_parity_on_fixtures() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/run_python.py");
    if !Path::new("/home/tcrider/btmanager/language-manager/language_backend.py").exists() {
        return;
    }
    let fixtures: &[(&str, &[(&str, &str)], &[(&str, &str)])] = &[
        ("fr", &[], &[]),
        ("pt_BR", &[(".config/labwc/environment", "# cursor\nXCURSOR_SIZE=42\nLC_TIME=ja_JP.UTF-8\nexport LANG=de_DE.utf8\nLC_ALL=C"),
                    (".config/plasma-localerc", "[Formats]\nLANG=en_US.utf8\n\n[Translations]\nLANGUAGE=de\n[Other]\nA=1\n"),
                    (".i18n", "SYSFONT=latarcyrheb\r\nLANG=\"en_US.UTF-8\"\n"),
                    (".config/desktop-suite/language.json", "{\"other\": {\"x\": [1, 2.5e10, null]}, \"language\": \"de\"}")],
         &[("LANG", "en_US.UTF-8"), ("LC_MONETARY", "de_DE.UTF-8")]),
        ("ja", &[(".config/environment.d/90-desktop-suite-language.conf",
                  "# Desktop suite language — managed by Language Settings\nLANG=x\n# End desktop suite language\n")],
         &[("LC_ALL", "fr_FR.UTF-8")]),
        ("de", &[(".config/plasma-localerc", "[Formats]\nLANG=de_DE.UTF-8")], &[]),
        ("ar", &[(".config/labwc/environment", "# Desktop suite language — managed by Language Settings\nOTHER=1\n")], &[]),
    ];
    for (code, files_in, environment) in fixtures {
        let python_home = scratch();
        let rust_home = scratch();
        for home in [&python_home, &rust_home] {
            for (name, text) in *files_in {
                let path = home.join(name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, text).unwrap();
            }
        }
        let env_json = serde_json::Value::Object(environment.iter().map(|(k, v)| (k.to_string(), serde_json::Value::from(*v))).collect());
        let output = std::process::Command::new("python3").args(["-I", "-B"]).arg(&script).arg(&python_home)
            .arg(code).arg(env_json.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let python: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

        let services = Fake::new();
        let mut backend = LanguageBackend::new(Some(rust_home.clone()), None, Some(services.clone() as Arc<dyn Services>));
        backend.locale_provider = Box::new(|| Ok(LANGUAGES.iter().map(|l| l.locale.replace("UTF-8", "utf8")).collect()));
        backend.environment = environment.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let result = backend.apply(code, None);
        let calls: Vec<serde_json::Value> = services.calls().into_iter().map(|(k, v)| serde_json::json!([k, v])).collect();
        let rust = serde_json::json!({"ok": result.ok, "error": result.error, "warnings": result.warnings,
            "rolled_back": result.rolled_back, "partial": result.partial, "calls": calls, "locale": services.locale()});
        assert_eq!(rust, python, "fixture {code}");
        let strip = |home: &Path| files(home).into_iter().map(|(p, b)| (p.strip_prefix(home).unwrap().to_path_buf(), b)).collect::<Vec<_>>();
        assert_eq!(strip(&rust_home), strip(&python_home), "fixture {code}");
    }
}
