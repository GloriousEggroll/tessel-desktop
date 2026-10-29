//! test_language_ui.py: the wizard against a fake backend in a private
//! headless labwc with its own session bus (tests/run_headless.sh). Under
//! plain `cargo test` it skips itself.

use gio::prelude::*;
use language_manager::app::{LanguageApplication, APPLICATION_ID};
use language_manager::backend::{ApplyResult, LANGUAGES};
use language_manager::style::{OPAQUE_CSS, OPAQUE_PRIORITY};
use language_manager::ui::{LanguageWizard, WizardBackend};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};
use suite_gtk::gtk;
use suite_gtk::gtk::prelude::*;

const BIN: &str = env!("CARGO_BIN_EXE_language-manager");

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(mut condition: impl FnMut() -> bool, seconds: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        settle(0.01);
        if condition() {
            return true;
        }
    }
    false
}

#[derive(Default)]
struct Gate {
    closed: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn set(&self, open: bool) {
        *self.closed.lock().unwrap() = !open;
        self.changed.notify_all();
    }
    fn wait(&self, seconds: u64) {
        let guard = self.closed.lock().unwrap();
        let _ = self.changed.wait_timeout_while(guard, Duration::from_secs(seconds), |closed| *closed).unwrap();
    }
}

#[derive(Default)]
struct FakeBackend {
    calls: Mutex<Vec<String>>,
    threads: Mutex<Vec<ThreadId>>,
    available: Mutex<bool>,
    missing: Mutex<HashSet<String>>,
    wait: Gate,
    outcome: Mutex<Option<ApplyResult>>,
}

impl FakeBackend {
    fn new() -> Arc<Self> {
        let fake = FakeBackend::default();
        *fake.available.lock().unwrap() = true;
        Arc::new(fake)
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn off_main(&self, main: ThreadId) -> bool {
        self.threads.lock().unwrap().iter().all(|t| *t != main)
    }
}

impl WizardBackend for FakeBackend {
    fn status(&self) -> Result<Value, String> {
        self.threads.lock().unwrap().push(std::thread::current().id());
        let missing = self.missing.lock().unwrap().clone();
        Ok(json!({"language": "en", "locale": "en_US.UTF-8", "system_locale": "en_US.UTF-8",
                  "system_available": *self.available.lock().unwrap(), "account_available": true, "warnings": [],
                  "languages": LANGUAGES.iter().map(|l| json!({"code": l.code, "name": l.name, "locale": l.locale,
                      "installed": !missing.contains(l.code)})).collect::<Vec<_>>()}))
    }
    fn apply(&self, code: &str, progress: &dyn Fn(&str)) -> Result<ApplyResult, String> {
        self.calls.lock().unwrap().push(code.to_owned());
        self.threads.lock().unwrap().push(std::thread::current().id());
        progress("Saving system language…");
        self.wait.wait(5);
        let locale = LANGUAGES.iter().find(|l| l.code == code).unwrap().locale;
        Ok(self.outcome.lock().unwrap().clone().unwrap_or_else(|| ApplyResult::new(true, code, locale)))
    }
}

struct Case {
    backend: Arc<FakeBackend>,
    window: LanguageWizard,
}

fn setup() -> Case {
    let backend = FakeBackend::new();
    let window = LanguageWizard::new(None, backend.clone());
    window.window.present();
    assert!(wait_for(|| window.status.borrow().is_some(), 3.0));
    Case { backend, window }
}

impl Drop for Case {
    fn drop(&mut self) {
        self.backend.wait.set(true);
        wait_for(|| !self.window.busy.get(), 3.0);
        self.window.window.destroy();
        settle(0.04);
    }
}

impl Case {
    fn select(&self, code: &str) {
        self.window.language_list.select_row(Some(&self.window.row(code).unwrap().row));
    }
    fn review(&self, code: &str) {
        self.select(code);
        self.window.next_button.emit_clicked();
        assert_eq!(self.window.page.get(), "review");
    }
    fn visible(&self) -> Vec<&'static str> {
        self.window.rows.iter().filter(|r| r.row.is_child_visible()).map(|r| r.language.code).collect()
    }
}

fn open_and_navigation_never_change_settings() {
    let c = setup();
    assert_eq!(c.window.rows.len(), 31);
    assert_eq!(c.window.selected.get().unwrap().code, "en");
    assert!(c.backend.calls().is_empty());
    c.review("de");
    assert_eq!(c.window.review_name.text(), "Deutsch");
    c.window.back_button.emit_clicked();
    assert_eq!(c.window.page.get(), "choose");
    assert!(c.backend.calls().is_empty());
    assert!(c.backend.off_main(std::thread::current().id()));
}

fn search_accepts_native_english_and_locale_names() {
    let c = setup();
    for (query, code) in [("Japanese", "ja"), ("简体", "zh_CN"), ("pt_BR", "pt_BR"), ("German", "de")] {
        c.window.search.set_text(query);
        c.window.filter();
        settle(0.02);
        assert_eq!(c.visible(), [code], "query {query}");
    }
    c.window.search.set_text("no such language");
    c.window.filter();
    settle(0.02);
    assert!(c.visible().is_empty());
    assert!(c.backend.calls().is_empty());
}

fn missing_locale_is_explained_and_cannot_apply() {
    let c = setup();
    {
        let mut status = c.window.status.borrow_mut();
        for item in status.as_mut().unwrap()["languages"].as_array_mut().unwrap() {
            let installed = item["code"] != "ja";
            item["installed"] = Value::Bool(installed);
        }
    }
    c.review("ja");
    assert!(!c.window.next_button.is_sensitive());
    assert!(c.window.review_warning.text().contains("not installed"));
    c.window.next();
    assert!(c.backend.calls().is_empty());
}

fn missing_system_service_is_explained() {
    let c = setup();
    c.window.status.borrow_mut().as_mut().unwrap()["system_available"] = Value::Bool(false);
    c.review("fr");
    assert!(!c.window.next_button.is_sensitive());
    assert!(c.window.review_warning.text().contains("unavailable"));
}

fn apply_runs_in_background_and_protects_transaction() {
    let c = setup();
    c.backend.wait.set(false);
    c.review("fr");
    c.window.next_button.emit_clicked();
    assert!(wait_for(|| !c.backend.calls().is_empty(), 3.0));
    assert!(c.window.busy.get());
    assert!(!c.window.cancel_button.is_sensitive());
    assert!(!c.window.back_button.is_sensitive());
    assert!(c.window.close_requested());
    c.window.window.close();
    settle(0.05);
    assert!(c.window.window.is_visible());
    assert!(wait_for(|| c.window.progress.text() == "Saving system language…", 2.0));
    assert!(c.backend.off_main(std::thread::current().id()));
    c.backend.wait.set(true);
    assert!(wait_for(|| c.window.page.get() == "result", 3.0));
    assert_eq!(c.backend.calls(), ["fr"]);
    assert!(c.window.result_body.text().contains("Sign out"));
    assert!(c.window.next_button.is_sensitive());
}

fn host_buttons_drive_the_wizard_without_its_own_chrome() {
    let c = setup();
    let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<&'static str>::new()));
    let weak = c.window.downgrade();
    let log = changes.clone();
    *c.window.on_wizard_changed.borrow_mut() = Some(std::rc::Rc::new(move || {
        if let Some(inner) = weak.upgrade() {
            log.borrow_mut().push(inner.page.get());
        }
    }));
    c.window.hide_wizard_chrome();
    assert!(!c.window.step_label.is_visible());
    assert!(!c.window.footer.is_visible());
    // The current language needs no wizard step: the host moves on.
    assert_eq!(c.window.wizard_action(), None);
    assert!(!c.window.wizard_step(true));
    c.select("fr");
    assert_eq!(c.window.wizard_action().as_deref(), Some("Continue"));
    assert!(c.window.wizard_step(true));
    assert_eq!(c.window.page.get(), "review");
    assert_eq!(c.window.wizard_action().as_deref(), Some("Apply Language"));
    assert!(c.window.wizard_step(false));
    assert_eq!(c.window.page.get(), "choose");
    assert!(c.window.wizard_step(true));
    assert!(c.window.wizard_step(true));
    assert!(wait_for(|| c.window.page.get() == "result", 3.0));
    assert_eq!(c.backend.calls(), ["fr"]);
    assert!(!c.window.wizard_step(true));
    assert!(changes.borrow().contains(&"review"));
}

fn window_background_is_opaque_above_the_companion_theme() {
    let c = setup();
    assert!(OPAQUE_PRIORITY > gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 30);
    assert!(OPAQUE_CSS.contains("background-image: none"));
    assert!(c.window.window.has_css_class("languagemanager"));
}

fn failed_apply_can_retry_without_losing_selection() {
    let c = setup();
    let mut failed = ApplyResult::new(false, "de", "de_DE.UTF-8");
    failed.error = "Authentication cancelled".into();
    failed.rolled_back = true;
    *c.backend.outcome.lock().unwrap() = Some(failed);
    c.review("de");
    c.window.next_button.emit_clicked();
    assert!(wait_for(|| !c.backend.calls().is_empty() && !c.window.busy.get(), 3.0));
    assert_eq!(c.window.page.get(), "review");
    assert!(c.window.message.text().contains("Authentication cancelled"));
    assert!(c.window.message.text().contains("restored"));
    assert!(c.window.next_button.is_sensitive());
    *c.backend.outcome.lock().unwrap() = None;
    c.window.next_button.emit_clicked();
    assert!(wait_for(|| c.window.page.get() == "result", 3.0));
    assert_eq!(c.backend.calls(), ["de", "de"]);
}

fn partial_failure_is_not_reported_as_success() {
    let c = setup();
    let mut partial = ApplyResult::new(false, "de", "de_DE.UTF-8");
    partial.error = "System rollback failed".into();
    partial.partial = true;
    partial.warnings = vec!["Check system language".into()];
    *c.backend.outcome.lock().unwrap() = Some(partial);
    c.review("de");
    c.window.next_button.emit_clicked();
    assert!(wait_for(|| c.window.page.get() == "result", 3.0));
    assert!(c.window.result_title.text().contains("could not"));
    assert_eq!(c.window.result_body.text(), "System rollback failed");
    assert_eq!(c.window.result_warning.text(), "Check system language");
}

fn cancel_closes_without_mutation_and_theme_unsubscribes() {
    let c = setup();
    let binding = c.window.theme.clone();
    c.window.cancel_button.emit_clicked();
    assert!(wait_for(|| c.window.closed.get(), 3.0));
    assert!(binding.is_closed());
    assert!(c.backend.calls().is_empty());
}

fn second_launch_reuses_current_page_and_window() {
    let backend = FakeBackend::new();
    let shared = backend.clone();
    let app = LanguageApplication::new(APPLICATION_ID, Box::new(move || shared as Arc<dyn WizardBackend>));
    app.register(None::<&gio::Cancellable>).unwrap();
    assert!(!app.is_remote());
    app.activate();
    assert!(wait_for(|| app.window().is_some_and(|w| w.status.borrow().is_some()), 3.0));
    let window = app.window().unwrap();
    window.language_list.select_row(Some(&window.row("es").unwrap().row));
    window.next_button.emit_clicked();
    let mut child = Command::new(BIN).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    assert!(wait_for(|| child.try_wait().unwrap().is_some(), 5.0), "second launch did not return");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(std::rc::Rc::ptr_eq(&app.window().unwrap().0, &window.0));
    assert_eq!(window.page.get(), "review");
    assert_eq!(window.selected.get().unwrap().code, "es");
    assert!(backend.calls().is_empty());
    // --status from a second process is answered by the primary, without a window.
    let mut child = Command::new(BIN).arg("--status").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    assert!(wait_for(|| child.try_wait().unwrap().is_some(), 5.0));
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("{\"language\": \"en\""));
    // --quit is refused while a change is running.
    backend.wait.set(false);
    window.next_button.emit_clicked();
    assert!(wait_for(|| window.busy.get() && !backend.calls().is_empty(), 3.0));
    let mut child = Command::new(BIN).arg("--quit").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    assert!(wait_for(|| child.try_wait().unwrap().is_some(), 5.0));
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "A language change is still in progress.\n");
    backend.wait.set(true);
    assert!(wait_for(|| !window.busy.get(), 3.0));
    window.window.destroy();
    settle(0.05);
    assert!(app.window().is_none());
    // A registered GApplication subclass that never ran cannot be finalized
    // safely (gio unregisters through the half-destroyed instance).
    std::mem::forget(app);
}

fn main() {
    if std::env::var("LANGUAGE_MANAGER_UI").as_deref() != Ok("1") {
        println!("ui: skipped (run tests/run_headless.sh for the private headless compositor)");
        return;
    }
    gtk::init().expect("GTK in the private compositor");
    let tests: &[(&str, fn())] = &[
        ("test_open_and_navigation_never_change_settings", open_and_navigation_never_change_settings),
        ("test_search_accepts_native_english_and_locale_names", search_accepts_native_english_and_locale_names),
        ("test_missing_locale_is_explained_and_cannot_apply", missing_locale_is_explained_and_cannot_apply),
        ("test_missing_system_service_is_explained", missing_system_service_is_explained),
        ("test_apply_runs_in_background_and_protects_transaction", apply_runs_in_background_and_protects_transaction),
        ("test_host_buttons_drive_the_wizard_without_its_own_chrome", host_buttons_drive_the_wizard_without_its_own_chrome),
        ("test_window_background_is_opaque_above_the_companion_theme", window_background_is_opaque_above_the_companion_theme),
        ("test_failed_apply_can_retry_without_losing_selection", failed_apply_can_retry_without_losing_selection),
        ("test_partial_failure_is_not_reported_as_success", partial_failure_is_not_reported_as_success),
        ("test_cancel_closes_without_mutation_and_theme_unsubscribes", cancel_closes_without_mutation_and_theme_unsubscribes),
        ("test_second_launch_reuses_current_page_and_window", second_launch_reuses_current_page_and_window),
    ];
    let mut failed = 0;
    for (name, test) in tests {
        match std::panic::catch_unwind(test) {
            Ok(()) => println!("{name} ... ok"),
            Err(error) => {
                failed += 1;
                let text = error.downcast_ref::<String>().cloned().or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()));
                println!("{name} ... FAILED: {}", text.unwrap_or_default());
            }
        }
    }
    println!("ui: {} passed, {failed} failed", tests.len() - failed);
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
