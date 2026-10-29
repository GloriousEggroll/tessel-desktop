//! A three-step, non-destructive-until-Apply desktop language wizard
//! (language_ui.py `LanguageWizard`).

use crate::backend::{ApplyResult, Language, LanguageBackend, LANGUAGES};
use crate::style::{style, style_window};
use crate::worker::{Poster, Worker};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use suite_core::pyjson::casefold;
use suite_core::tr;
use suite_gtk::gtk::prelude::*;
use suite_gtk::gtk::{self, gdk, pango};
use suite_gtk::theme::WindowTheme;
use suite_gtk::utils::{accessible, headerbar, image, key_controller, set_margins};

/// Native names remain unchanged; English names make the search useful when a
/// user cannot yet read the language selected on the computer.
pub const ENGLISH_NAMES: [(&str, &str); 31] = [
    ("en", "English"), ("zh_CN", "Chinese, simplified"), ("zh_TW", "Chinese, traditional"),
    ("ja", "Japanese"), ("ko", "Korean"), ("es", "Spanish"), ("fr", "French"), ("de", "German"),
    ("it", "Italian"), ("pt", "Portuguese"), ("pt_BR", "Portuguese, Brazil"), ("nl", "Dutch"),
    ("pl", "Polish"), ("ru", "Russian"), ("uk", "Ukrainian"), ("cs", "Czech"), ("sk", "Slovak"),
    ("hu", "Hungarian"), ("ro", "Romanian"), ("el", "Greek"), ("sv", "Swedish"), ("da", "Danish"),
    ("nb", "Norwegian, Bokm\u{e5}l"), ("fi", "Finnish"), ("tr", "Turkish"), ("ar", "Arabic"),
    ("he", "Hebrew"), ("hi", "Hindi"), ("id", "Indonesian"), ("th", "Thai"), ("vi", "Vietnamese"),
];

pub fn english_name(code: &str) -> &'static str {
    ENGLISH_NAMES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n).unwrap_or("")
}

/// What the wizard needs from a backend (real or test fake); both calls run
/// on the worker thread.
pub trait WizardBackend: Send + Sync + 'static {
    fn status(&self) -> Result<Value, String>;
    fn apply(&self, code: &str, progress: &dyn Fn(&str)) -> Result<ApplyResult, String>;
}

impl WizardBackend for LanguageBackend {
    fn status(&self) -> Result<Value, String> {
        LanguageBackend::status(self)
    }
    fn apply(&self, code: &str, progress: &dyn Fn(&str)) -> Result<ApplyResult, String> {
        Ok(LanguageBackend::apply(self, code, Some(progress)))
    }
}

pub fn label(text: &str, classes: &[&str], wrap: bool) -> gtk::Label {
    let widget = style(gtk::Label::builder().label(text).xalign(0.0).build(), classes);
    if wrap {
        widget.set_wrap(true);
        widget.set_wrap_mode(pango::WrapMode::WordChar);
        widget.set_max_width_chars(55);
    }
    widget
}

pub struct Row {
    pub language: &'static Language,
    pub row: gtk::ListBoxRow,
    pub check: gtk::Image,
    pub query: String,
}

pub struct Inner {
    pub window: gtk::Window,
    backend: Arc<dyn WizardBackend>,
    pub status: RefCell<Option<Value>>,
    pub selected: Cell<Option<&'static Language>>,
    pub busy: Cell<bool>,
    pub closed: Cell<bool>,
    pub page: Cell<&'static str>,
    pub result: RefCell<Option<ApplyResult>>,
    /// Set by a host (Welcome / Global Settings) that drives the wizard with its own buttons.
    pub on_wizard_changed: RefCell<Option<Rc<dyn Fn()>>>,
    jobs: Worker,
    progress_poster: Cell<Option<Poster>>,
    pub theme: WindowTheme,
    pub language_style_close: Rc<dyn Fn()>,
    pub root: gtk::Box,
    pub step_label: gtk::Label,
    pub stack: gtk::Stack,
    pub message: gtk::Label,
    pub progress_row: gtk::Box,
    pub spinner: gtk::Spinner,
    pub progress: gtk::Label,
    pub footer_separator: gtk::Separator,
    pub footer: gtk::Box,
    pub cancel_button: gtk::Button,
    pub back_button: gtk::Button,
    pub next_button: gtk::Button,
    pub search: gtk::SearchEntry,
    pub language_list: gtk::ListBox,
    pub rows: Vec<Row>,
    pub current_label: gtk::Label,
    pub review_name: gtk::Label,
    pub review_locale: gtk::Label,
    pub review_warning: gtk::Label,
    pub result_icon: gtk::Image,
    pub result_title: gtk::Label,
    pub result_body: gtk::Label,
    pub result_warning: gtk::Label,
}

#[derive(Clone)]
pub struct LanguageWizard(pub Rc<Inner>);

impl std::ops::Deref for LanguageWizard {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

struct Choose {
    content: gtk::Box,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    rows: Vec<Row>,
    current_label: gtk::Label,
}

fn build_choose() -> Choose {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&label(&tr!("Choose your preferred language"), &["section-title"], false));
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&tr!("Search languages")));
    accessible(&search, &tr!("Search languages"));
    content.append(&search);
    let scroll = style(gtk::ScrolledWindow::new(), &["card"]);
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let list = style(gtk::ListBox::new(), &["language-list"]);
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    let mut rows = Vec::new();
    for language in &LANGUAGES {
        let row = gtk::ListBoxRow::new();
        let english = english_name(language.code);
        let query = casefold(&[language.name, language.code, language.locale, english].join(" "));
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 2);
        names.set_hexpand(true);
        names.append(&label(language.name, &["language-name"], false));
        let secondary = if english == language.name { language.locale } else { english };
        names.append(&label(secondary, &["muted", "language-code"], false));
        line.append(&names);
        let check = style(image("object-select-symbolic", 18), &["profile-check"]);
        check.set_opacity(0.0);
        line.append(&check);
        row.set_child(Some(&line));
        list.append(&row);
        rows.push(Row { language, row, check, query });
    }
    list.set_placeholder(Some(&label(&tr!("No matching languages"), &["empty", "muted"], false)));
    scroll.set_child(Some(&list));
    content.append(&scroll);
    let current_label = label(&tr!("Reading current language…"), &["muted"], true);
    content.append(&current_label);
    Choose { content, search, list, rows, current_label }
}

fn build_review() -> (gtk::ScrolledWindow, gtk::Label, gtk::Label, gtk::Label) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.append(&label(&tr!("Review language change"), &["section-title"], false));
    let selection = style(gtk::Box::new(gtk::Orientation::Vertical, 0), &["card"]);
    let selected = gtk::Box::new(gtk::Orientation::Vertical, 5);
    set_margins(&selected, 18);
    let review_name = label("", &["review-value"], false);
    let review_locale = label("", &["muted"], false);
    selected.append(&review_name);
    selected.append(&review_locale);
    selection.append(&selected);
    content.append(&selection);
    let items = [
        ("preferences-desktop-symbolic", tr!("Desktop and components"),
         tr!("Use this language in Waydock, Wayshell, and the desktop applets.")),
        ("computer-symbolic", tr!("System and sign-in"),
         tr!("Set your account language and the system default. Administrator authentication may be required.")),
        ("preferences-desktop-keyboard-symbolic", tr!("Keep your regional settings"),
         tr!("Your keyboard layouts, time zone, and number and date formats stay the same. Change keyboard layouts in Settings › Keyboard.")),
    ];
    for (icon, title, detail) in items {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 13);
        let symbol = image(icon, 21);
        symbol.set_valign(gtk::Align::Start);
        symbol.set_margin_top(3);
        row.append(&symbol);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text.append(&label(&title, &["section-title"], false));
        text.append(&label(&detail, &["muted"], true));
        row.append(&text);
        content.append(&row);
    }
    content.append(&label(&tr!("Sign out and back in after applying to update all running applications."), &["muted"], true));
    let review_warning = label("", &["error"], true);
    content.append(&review_warning);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&content));
    (scroll, review_name, review_locale, review_warning)
}

fn build_result() -> (gtk::Box, gtk::Image, gtk::Label, gtk::Label, gtk::Label) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_valign(gtk::Align::Center);
    let icon = style(image("emblem-ok-symbolic", 38), &["settings-tile", "result-icon"]);
    icon.set_halign(gtk::Align::Center);
    content.append(&icon);
    let title = label("", &["section-title"], true);
    title.set_xalign(0.5);
    content.append(&title);
    let body = label("", &["muted"], true);
    body.set_xalign(0.5);
    body.set_justify(gtk::Justification::Center);
    content.append(&body);
    let warning = label("", &["error"], true);
    content.append(&warning);
    (content, icon, title, body, warning)
}

impl LanguageWizard {
    #[allow(deprecated)]
    pub fn new(application: Option<&gtk::Application>, backend: Arc<dyn WizardBackend>) -> Self {
        let window: gtk::Window = match application {
            Some(app) => gtk::ApplicationWindow::new(app).upcast(),
            None => gtk::Window::new(),
        };
        window.set_title(Some(&tr!("Language Settings")));
        window.set_default_size(570, 730);
        window.set_size_request(470, 520);
        window.set_titlebar(Some(&headerbar(&tr!("Language Settings"))));
        let (theme, language_style_close) = style_window(&window);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 19);
        set_margins(&root, 24);
        window.set_child(Some(&root));
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let tile = style(image("preferences-desktop-locale-symbolic", 24), &["settings-tile"]);
        tile.set_valign(gtk::Align::Center);
        heading.append(&tile);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 5);
        titles.set_hexpand(true);
        titles.append(&label(&tr!("Language"), &["title"], false));
        titles.append(&label(&tr!("One language for your desktop and system."), &["subtitle"], true));
        heading.append(&titles);
        root.append(&heading);
        let step_label = label("", &["muted", "steps"], false);
        root.append(&step_label);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(140);
        let choose = build_choose();
        let (review, review_name, review_locale, review_warning) = build_review();
        let (result, result_icon, result_title, result_body, result_warning) = build_result();
        stack.add_named(&choose.content, Some("choose"));
        stack.add_named(&review, Some("review"));
        stack.add_named(&result, Some("result"));
        root.append(&stack);

        let message = label("", &["error"], true);
        message.set_visible(false);
        root.append(&message);
        let progress_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let spinner = gtk::Spinner::new();
        spinner.set_valign(gtk::Align::Center);
        progress_row.append(&spinner);
        let progress = label(&tr!("Reading language settings…"), &["muted"], true);
        progress_row.append(&progress);
        root.append(&progress_row);
        let footer_separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        root.append(&footer_separator);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let cancel_button = gtk::Button::with_label(&tr!("Cancel"));
        footer.append(&cancel_button);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        footer.append(&spacer);
        let back_button = gtk::Button::with_label(&tr!("Back"));
        footer.append(&back_button);
        let next_button = style(gtk::Button::with_label(&tr!("Continue")), &["primary"]);
        footer.append(&next_button);
        root.append(&footer);

        let wizard = LanguageWizard(Rc::new(Inner {
            window, backend, status: RefCell::new(None), selected: Cell::new(None), busy: Cell::new(false),
            closed: Cell::new(false), page: Cell::new("choose"), result: RefCell::new(None),
            on_wizard_changed: RefCell::new(None), jobs: Worker::new(), progress_poster: Cell::new(None),
            theme, language_style_close, root, step_label, stack, message, progress_row, spinner, progress,
            footer_separator, footer, cancel_button, back_button, next_button,
            search: choose.search, language_list: choose.list, rows: choose.rows, current_label: choose.current_label,
            review_name, review_locale, review_warning, result_icon, result_title, result_body, result_warning,
        }));
        wizard.connect_signals();
        wizard.show_page("choose");
        wizard.spinner.start();
        let backend = wizard.backend.clone();
        let weak = wizard.downgrade();
        wizard.run(move || backend.status(), move |result| {
            if let Some(wizard) = upgrade(&weak) {
                wizard.loaded(result)
            }
        });
        wizard
    }

    pub fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn connect_signals(&self) {
        let weak = self.downgrade();
        let w = weak.clone();
        self.window.connect_close_request(move |_| match upgrade(&w) {
            Some(wizard) if wizard.close_requested() => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        let w = weak.clone();
        self.window.connect_unrealize(move |_| if let Some(wizard) = upgrade(&w) { wizard.unrealized() });
        let w = weak.clone();
        key_controller(&self.window, false, move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(wizard) = upgrade(&w) {
                    if !wizard.busy.get() {
                        wizard.window.close();
                    }
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        let w = weak.clone();
        self.search.connect_search_changed(move |_| if let Some(wizard) = upgrade(&w) { wizard.filter() });
        let w = weak.clone();
        self.language_list.connect_row_selected(move |_, row| if let Some(wizard) = upgrade(&w) { wizard.selected_row(row) });
        let w = weak.clone();
        self.language_list.connect_row_activated(move |_, _| if let Some(wizard) = upgrade(&w) { wizard.next() });
        let w = weak.clone();
        self.language_list.set_filter_func(move |row| upgrade(&w).map(|wizard| wizard.row_visible(row)).unwrap_or(true));
        let w = weak.clone();
        self.cancel_button.connect_clicked(move |_| if let Some(wizard) = upgrade(&w) { wizard.window.close() });
        let w = weak.clone();
        self.back_button.connect_clicked(move |_| if let Some(wizard) = upgrade(&w) { wizard.back() });
        let w = weak;
        self.next_button.connect_clicked(move |_| if let Some(wizard) = upgrade(&w) { wizard.next() });
    }

    /// `_run(action, finished)`: one background job; delivered only while open.
    pub fn run<R: Send + 'static>(&self, action: impl FnOnce() -> Result<R, String> + Send + 'static,
                                  finished: impl FnOnce(Result<R, String>) + 'static) {
        let weak = self.downgrade();
        self.jobs.run(action, move |result| {
            if let Some(wizard) = upgrade(&weak) {
                if !wizard.closed.get() {
                    finished(result)
                }
            }
        });
    }

    #[allow(deprecated)]
    pub fn loaded(&self, result: Result<Value, String>) {
        self.spinner.stop();
        self.progress_row.set_visible(false);
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                self.current_label.set_text(&tr!("Could not read the current language."));
                self.message(&error);
                self.next_button.set_sensitive(false);
                return;
            }
        };
        let code = status.get("language").and_then(Value::as_str).unwrap_or("en").to_owned();
        let warnings: Vec<String> = status.get("warnings").and_then(Value::as_array)
            .map(|items| items.iter().map(suite_core::pyjson::py_str).collect()).unwrap_or_default();
        let locale = status.get("locale").map(suite_core::pyjson::py_str).unwrap_or_else(|| code.clone());
        *self.status.borrow_mut() = Some(status);
        let row = self.row(&code).or_else(|| self.row("en")).map(|r| r.row.clone());
        self.language_list.select_row(row.as_ref());
        let current_name = self.row(&code).map(|r| r.language.name.to_owned()).unwrap_or(locale);
        self.current_label.set_text(&tr!("Current language: {language}", language = current_name));
        if !warnings.is_empty() {
            self.message(&warnings.join("\n"));
        }
        self.update_actions();
    }

    pub fn row(&self, code: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.language.code == code)
    }

    pub fn filter(&self) {
        self.language_list.invalidate_filter();
    }

    fn row_visible(&self, row: &gtk::ListBoxRow) -> bool {
        let Some(item) = self.rows.iter().find(|r| &r.row == row) else { return true };
        let text = casefold(&self.search.text());
        text.split_whitespace().all(|word| item.query.contains(word))
    }

    fn selected_row(&self, row: Option<&gtk::ListBoxRow>) {
        let item = row.and_then(|row| self.rows.iter().find(|r| &r.row == row));
        self.selected.set(item.map(|r| r.language));
        for each in &self.rows {
            each.check.set_opacity(if Some(&each.row) == row { 1.0 } else { 0.0 });
        }
        self.update_actions();
    }

    pub fn show_page(&self, page: &'static str) {
        self.page.set(page);
        self.stack.set_visible_child_name(page);
        self.step_label.set_text(&match page {
            "choose" => tr!("1 of 3 · Choose language"),
            "review" => tr!("2 of 3 · Review"),
            _ => tr!("3 of 3 · Finish"),
        });
        self.back_button.set_visible(page == "review");
        self.cancel_button.set_visible(page != "result");
        self.next_button.set_label(&match page {
            "choose" => tr!("Continue"),
            "review" => tr!("Apply Language"),
            _ => tr!("Close"),
        });
        self.message("");
        self.update_actions();
    }

    pub fn update_actions(&self) {
        let mut ready = self.status.borrow().is_some() && self.selected.get().is_some();
        if self.page.get() == "review" && ready {
            ready = self.apply_problem().is_empty();
        }
        let busy = self.busy.get();
        self.next_button.set_sensitive(!busy && (self.page.get() == "result" || ready));
        self.back_button.set_sensitive(!busy);
        self.cancel_button.set_sensitive(!busy);
        self.search.set_sensitive(!busy);
        self.language_list.set_sensitive(!busy);
        let callback = self.on_wizard_changed.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    // -- driven by a host's own Back/Next (Welcome) ------------------------------

    /// Hide the "1 of 3" stepper and the Cancel/Back/Continue footer.
    pub fn hide_wizard_chrome(&self) {
        self.step_label.set_visible(false);
        self.footer_separator.set_visible(false);
        self.footer.set_visible(false);
    }

    fn language_changed(&self) -> bool {
        let status = self.status.borrow();
        match (status.as_ref(), self.selected.get()) {
            (Some(status), Some(selected)) => status.get("language").and_then(Value::as_str) != Some(selected.code),
            _ => false,
        }
    }

    /// The host's Next label while the wizard needs it, else None.
    pub fn wizard_action(&self) -> Option<String> {
        if self.page.get() == "choose" && self.language_changed() {
            return Some(tr!("Continue"));
        }
        if self.page.get() == "review" {
            return Some(tr!("Apply Language"));
        }
        None
    }

    /// Handle a host Back/Next press; true when the wizard used it.
    pub fn wizard_step(&self, forward: bool) -> bool {
        if self.busy.get() {
            return true;
        }
        if forward {
            if self.page.get() == "choose" && self.language_changed() {
                self.next();
                return true;
            }
            if self.page.get() == "review" {
                if self.next_button.is_sensitive() {
                    self.apply();
                }
                return true;
            }
            return false;
        }
        if self.page.get() == "review" {
            self.back();
            return true;
        }
        false
    }

    pub fn apply_problem(&self) -> String {
        let status = self.status.borrow();
        let (Some(status), Some(selected)) = (status.as_ref(), self.selected.get()) else {
            return tr!("Choose a language to continue.");
        };
        if !status.get("system_available").is_some_and(suite_core::pyjson::truthy) {
            return tr!("The system language service is unavailable. Try again after it is running.");
        }
        // {language["code"]: language.get("installed", False)} — the last entry wins.
        let mut installed = false;
        if let Some(languages) = status.get("languages").and_then(Value::as_array) {
            for item in languages {
                if item.get("code").and_then(Value::as_str) == Some(selected.code) {
                    installed = item.get("installed").is_some_and(suite_core::pyjson::truthy);
                }
            }
        }
        if !installed {
            return tr!("Language support for {language} is not installed. Install it before applying this language.",
                       language = selected.name);
        }
        String::new()
    }

    pub fn next(&self) {
        if self.busy.get() || !self.next_button.is_sensitive() {
            return;
        }
        match self.page.get() {
            "choose" => {
                let Some(selected) = self.selected.get() else { return };
                self.review_name.set_text(selected.name);
                self.review_locale.set_text(selected.locale);
                self.review_warning.set_text(&self.apply_problem());
                self.show_page("review");
            }
            "review" => self.apply(),
            _ => self.window.close(),
        }
    }

    pub fn back(&self) {
        if !self.busy.get() {
            self.show_page("choose");
        }
    }

    #[allow(deprecated)]
    fn apply(&self) {
        let Some(selected) = self.selected.get() else { return };
        self.busy.set(true);
        self.message("");
        self.progress.set_text(&tr!("Applying language settings…"));
        self.progress_row.set_visible(true);
        self.spinner.start();
        self.update_actions();
        let weak = self.downgrade();
        let poster = Poster::new(move |message| {
            if let Some(wizard) = upgrade(&weak) {
                if !wizard.closed.get() && wizard.busy.get() {
                    wizard.progress.set_text(&tr!(message));
                }
            }
        });
        if let Some(old) = self.progress_poster.replace(Some(poster)) {
            old.remove();
        }
        let backend = self.backend.clone();
        let weak = self.downgrade();
        self.run(move || backend.apply(selected.code, &|message| poster.post(message)), move |result| {
            if let Some(wizard) = upgrade(&weak) {
                wizard.applied(result)
            }
        });
    }

    #[allow(deprecated)]
    pub fn applied(&self, result: Result<ApplyResult, String>) {
        self.busy.set(false);
        self.spinner.stop();
        self.progress_row.set_visible(false);
        self.update_actions();
        let result = match result {
            Ok(result) => result,
            Err(error) => return self.message(&error),
        };
        *self.result.borrow_mut() = Some(result.clone());
        if !result.ok && !result.partial {
            let mut message = if result.error.is_empty() { tr!("The language could not be changed.") } else { result.error.clone() };
            if result.rolled_back {
                message.push('\n');
                message.push_str(&tr!("Your previous settings have been restored."));
            }
            return self.message(&message);
        }
        self.result_icon.set_icon_name(Some(if result.ok { "emblem-ok-symbolic" } else { "dialog-warning-symbolic" }));
        self.result_title.set_text(&if result.ok { tr!("Language settings saved") } else { tr!("Some settings could not be updated") });
        if result.ok {
            let name = self.selected.get().map(|s| s.name).unwrap_or("");
            self.result_body.set_text(&tr!("{language} is now selected. Sign out and back in when you are ready to use it across your desktop.",
                                           language = name));
        } else if result.error.is_empty() {
            self.result_body.set_text(&tr!("Please review the remaining settings before signing out."));
        } else {
            self.result_body.set_text(&result.error);
        }
        self.result_warning.set_text(&result.warnings.join("\n"));
        self.show_page("result");
    }

    pub fn message(&self, message: &str) {
        self.message.set_text(message);
        self.message.set_visible(!message.is_empty());
    }

    /// `_close_requested`: true refuses the close while a change is running.
    pub fn close_requested(&self) -> bool {
        if self.busy.get() {
            self.message(&tr!("Please finish or cancel the authentication prompt before closing this window."));
            return true;
        }
        false
    }

    fn unrealized(&self) {
        self.closed.set(true);
        self.jobs.shutdown();
        if let Some(poster) = self.progress_poster.take() {
            poster.remove();
        }
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<LanguageWizard> {
    weak.upgrade().map(LanguageWizard)
}
