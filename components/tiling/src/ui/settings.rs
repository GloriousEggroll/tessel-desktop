//! Shared immediate-apply tiling preferences for the popup and Global
//! Settings (tiling_settings.py).
//!
//! Every compositor request, configuration lock and `labwc --reconfigure`
//! runs on a worker thread; switches keep their previous state until the
//! change is confirmed, and return to it with an error message if it fails.

use crate::client::{self, Reply, SIGN_OUT_REQUIRED};
use crate::compat::{py_str, truthy};
use crate::config::{Error, TilingConfig};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tessel_core::tr;
use tessel_gtk::gtk::prelude::*;
use tessel_gtk::gtk::{self, glib};
use tessel_gtk::utils::accessible;

pub const SIGN_OUT_BEFORE: &str = "Takes effect after you sign out and back in.";

/// Where `work` runs: a worker thread with `done` back on the GTK thread
/// (Python `run_async`), or inline (tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runner {
    Thread,
    Inline,
}

impl Runner {
    pub fn run<T: Send + 'static>(self, work: impl FnOnce() -> Result<T, Error> + Send + 'static,
                                  done: impl FnOnce(Result<T, Error>) + 'static) {
        match self {
            Runner::Inline => done(work()),
            Runner::Thread => {
                glib::MainContext::default().spawn_local(async move {
                    let result = gio::spawn_blocking(work).await
                        .unwrap_or_else(|_| Err(Error("The tiling worker stopped unexpectedly.".into())));
                    done(result);
                });
            }
        }
    }
}

/// The session manager's own Log Out action: ask this labwc instance to exit.
pub fn sign_out_command() -> Vec<String> { vec!["labwc".into(), "--exit".into()] }

/// Whether `$LABWC_PID` names a running labwc (`/proc/PID/comm`).
pub fn labwc_session() -> bool {
    let Ok(pid) = std::env::var("LABWC_PID").unwrap_or_default().parse::<i64>() else { return false };
    pid > 1 && std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "labwc")
}

/// "Sign out now", confirmed with the session manager's Log Out wording.
#[derive(Clone)]
pub struct SignOutButton {
    pub button: gtk::Button,
    pub error: Rc<RefCell<Option<gtk::Label>>>,
    command: Rc<dyn Fn() -> Vec<String>>,
}

impl SignOutButton {
    pub fn new(command: Rc<dyn Fn() -> Vec<String>>) -> Self {
        let button = gtk::Button::builder().label(tr!("Sign out now")).halign(gtk::Align::Start).visible(false).build();
        let this = Self { button: button.clone(), error: Rc::default(), command };
        let weak = this.clone();
        button.connect_clicked(move |_| weak.clicked());
        this
    }

    fn clicked(&self) {
        let root = self.button.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let dialog = gtk::AlertDialog::builder().message(tr!("Log Out"))
            .detail(tr!("Log out of this session? Save your work before continuing."))
            .buttons([tr!("Cancel"), tr!("Log Out")]).modal(true).cancel_button(0).default_button(0).build();
        let this = self.clone();
        dialog.choose(root.as_ref(), None::<&gio::Cancellable>, move |answer| {
            if answer == Ok(1) {
                this.sign_out();
            }
        });
    }

    pub fn sign_out(&self) {
        if !labwc_session() {
            self.failed(&tr!("The labwc session is unavailable."));
            return;
        }
        let argv = (self.command)();
        let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
        if let Err(error) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
            self.failed(error.message());
        }
    }

    fn failed(&self, message: &str) {
        self.button.set_tooltip_text(Some(message));
        if let Some(error) = self.error.borrow().as_ref() {
            error.set_text(message);
            error.set_visible(true);
        }
    }
}

/// `_row(title, detail)` → (row, switch, subtitle).
fn row(title: &str, detail: &str) -> (gtk::Box, gtk::Switch, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let description = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
    description.append(&gtk::Label::builder().label(title).xalign(0.0).build());
    let subtitle = gtk::Label::builder().label(detail).xalign(0.0).wrap(true).max_width_chars(40)
        .css_classes(["settings-muted", "detail", "gs-row-subtitle"]).build();
    description.append(&subtitle);
    row.append(&description);
    let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    accessible(&switch, title);
    row.append(&switch);
    (row, switch, subtitle)
}

pub type Apply = Arc<dyn Fn(bool) -> Result<Value, Error> + Send + Sync>;
pub type Finished = Rc<dyn Fn(bool, Option<Value>, Option<Error>)>;

struct SwitchInner {
    switch: gtk::Switch,
    apply: Apply,
    finished: Finished,
    runner: Runner,
    updating: Cell<bool>,
    busy: Cell<bool>,
}

/// Keep a switch in step with a slow backend change made on a worker thread.
#[derive(Clone)]
pub struct AsyncSwitch(Rc<SwitchInner>);

impl AsyncSwitch {
    pub fn new(switch: &gtk::Switch, apply: Apply, finished: Finished, runner: Runner) -> Self {
        let this = Self(Rc::new(SwitchInner { switch: switch.clone(), apply, finished, runner, updating: Cell::new(false),
                                              busy: Cell::new(false) }));
        let weak = Rc::downgrade(&this.0);
        switch.connect_state_set(move |switch, enabled| {
            match weak.upgrade() {
                Some(inner) => AsyncSwitch(inner).state_set(switch, enabled),
                None => glib::Propagation::Proceed,
            }
        });
        this
    }

    pub fn updating(&self) -> bool { self.0.updating.get() }
    pub fn busy(&self) -> bool { self.0.busy.get() }

    pub fn show(&self, value: bool) {
        self.0.updating.set(true);
        self.0.switch.set_active(value);
        self.0.switch.set_state(value);
        self.0.updating.set(false);
    }

    fn state_set(&self, switch: &gtk::Switch, enabled: bool) -> glib::Propagation {
        if self.0.updating.get() {
            return glib::Propagation::Proceed;
        }
        if self.0.busy.get() {
            return glib::Propagation::Stop;
        }
        let previous = switch.state();
        self.0.busy.set(true);
        switch.set_sensitive(false);
        let this = self.clone();
        let apply = self.0.apply.clone();
        self.0.runner.run(move || apply(enabled), move |result| {
            this.0.busy.set(false);
            this.0.switch.set_sensitive(true);
            // A failed change returns the thumb and the state to the old value.
            this.show(if result.is_err() { previous } else { enabled });
            match result {
                Ok(value) => (this.0.finished)(enabled, Some(value), None),
                Err(error) => (this.0.finished)(enabled, None, Some(error)),
            }
        });
        glib::Propagation::Stop
    }
}

struct GaplessInner {
    backend: Arc<TilingConfig>,
    runner: Runner,
    widget: gtk::Box,
    switch: gtk::Switch,
    error: gtk::Label,
    control: RefCell<Option<AsyncSwitch>>,
    loading: Cell<bool>,
}

#[derive(Clone)]
pub struct GaplessControls(Rc<GaplessInner>);

impl GaplessControls {
    pub fn new(backend: Arc<TilingConfig>, runner: Runner) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let (row, switch, _) = row(&tr!("Gapless"), &tr!(
            "Remove spacing between tiled windows and at screen edges. Reserved panel space is kept. Applies immediately."));
        widget.append(&row);
        let error = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(40).visible(false).css_classes(["error"]).build();
        widget.append(&error);
        let this = Self(Rc::new(GaplessInner { backend: backend.clone(), runner, widget, switch: switch.clone(), error,
                                               control: RefCell::new(None), loading: Cell::new(false) }));
        let weak = Rc::downgrade(&this.0);
        let finished: Finished = Rc::new(move |_, _, error| {
            if let Some(inner) = weak.upgrade() {
                let text = error.map(|e| e.0).unwrap_or_default();
                inner.error.set_text(&text);
                inner.error.set_visible(!text.is_empty());
            }
        });
        let apply: Apply = Arc::new(move |enabled| backend.set_gapless(enabled).map(|_| Value::Null));
        this.0.control.replace(Some(AsyncSwitch::new(&switch, apply, finished, runner)));
        this.refresh();
        this
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.widget }
    pub fn switch(&self) -> &gtk::Switch { &self.0.switch }
    pub fn error(&self) -> &gtk::Label { &self.0.error }
    fn control(&self) -> AsyncSwitch { self.0.control.borrow().clone().unwrap() }
    pub fn busy(&self) -> bool { self.control().busy() }
    pub fn updating(&self) -> bool { self.control().updating() }

    pub fn refresh(&self) {
        if self.control().busy() || self.0.loading.get() {
            return;
        }
        self.0.loading.set(true);
        let backend = self.0.backend.clone();
        let this = self.clone();
        self.0.runner.run(move || backend.load_preferences(), move |result| {
            this.0.loading.set(false);
            if this.control().busy() {
                return;
            }
            match result {
                Err(error) => {
                    this.0.error.set_text(&error.0);
                    this.0.error.set_visible(true);
                    this.0.switch.set_sensitive(false);
                }
                Ok(preferences) => {
                    this.control().show(matches!(preferences.get("gapless"), Some(Value::Bool(true))));
                    this.0.switch.set_sensitive(true);
                }
            }
        });
    }
}

pub type StatusCall = Arc<dyn Fn() -> Reply + Send + Sync>;
pub type StateHook = Rc<dyn Fn(&Reply, &Map<String, Value>)>;

struct ControlsInner {
    backend: Arc<TilingConfig>,
    status_call: StatusCall,
    runner: Runner,
    launcher: PathBuf,
    state: RefCell<Reply>,
    loading: Cell<bool>,
    outline_pending_sign_out: Cell<bool>,
    widget: gtk::Box,
    enabled_switch: gtk::Switch,
    enabled_detail: gtk::Label,
    enabled: RefCell<Option<AsyncSwitch>>,
    gapless: GaplessControls,
    outline_text: String,
    outline_switch: gtk::Switch,
    outline_detail: gtk::Label,
    outline: RefCell<Option<AsyncSwitch>>,
    notice: gtk::Label,
    error: gtk::Label,
    sign_out: SignOutButton,
    shortcuts: gtk::Button,
    state_hook: RefCell<Option<StateHook>>,
}

/// Window tiling on/off, gapless, focus outline and sign-out notices.
#[derive(Clone)]
pub struct TilingControls(Rc<ControlsInner>);

impl TilingControls {
    pub fn new(backend: Option<Arc<TilingConfig>>, status: Option<StatusCall>, show_shortcuts: bool,
               launcher: Option<PathBuf>, runner: Runner) -> Self {
        let backend = backend.unwrap_or_else(|| Arc::new(TilingConfig::default()));
        let status_call = status.unwrap_or_else(|| Arc::new(client::status));
        let launcher = launcher.unwrap_or_else(|| tessel_core::paths::home().join(".local/bin/tessel-tiling"));
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let (enabled_row, enabled_switch, enabled_detail) = row(&tr!("Window tiling"), &tr!(
            "Arrange new windows side by side automatically. Turning it off restores their sizes. Applies immediately."));
        widget.append(&enabled_row);
        let gapless = GaplessControls::new(backend.clone(), runner);
        widget.append(gapless.widget());
        let outline_text = tr!("Use the display’s accent color. Hidden for fullscreen and borderless fullscreen windows.");
        let (outline_row, outline_switch, outline_detail) = row(&tr!("Focused window outline"), &outline_text);
        widget.append(&outline_row);
        let notice = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(40).visible(false)
            .css_classes(["settings-muted", "detail", "gs-row-subtitle"]).build();
        widget.append(&notice);
        let error = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(40).visible(false).css_classes(["error"]).build();
        widget.append(&error);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let sign_out = SignOutButton::new(Rc::new(sign_out_command));
        sign_out.error.replace(Some(error.clone()));
        actions.append(&sign_out.button);
        let shortcuts = gtk::Button::builder().label(tr!("Keyboard shortcuts…")).visible(show_shortcuts)
            .halign(gtk::Align::Start).build();
        shortcuts.set_tooltip_text(Some(&tr!("Show the searchable list of window tiling shortcuts.")));
        actions.append(&shortcuts);
        widget.append(&actions);
        let this = Self(Rc::new(ControlsInner {
            backend: backend.clone(), status_call: status_call.clone(), runner, launcher, state: RefCell::default(),
            loading: Cell::new(false), outline_pending_sign_out: Cell::new(false), widget, enabled_switch: enabled_switch.clone(),
            enabled_detail, enabled: RefCell::new(None), gapless, outline_text, outline_switch: outline_switch.clone(),
            outline_detail, outline: RefCell::new(None), notice, error, sign_out, shortcuts: shortcuts.clone(),
            state_hook: RefCell::new(None),
        }));
        let (b, s) = (backend.clone(), status_call);
        let apply: Apply = Arc::new(move |enabled| {
            let state = s();
            if !truthy(state.get("ready")) {
                let error = state.get("error").filter(|e| truthy(Some(e))).map(py_str).unwrap_or_else(|| tr!(SIGN_OUT_REQUIRED));
                return Err(Error(error));
            }
            b.set_enabled(enabled, true).map(Value::Object)
        });
        let weak = Rc::downgrade(&this.0);
        let finished: Finished = Rc::new(move |_, _, error| {
            if let Some(inner) = weak.upgrade() {
                let this = TilingControls(inner);
                this.show_error(error.as_ref());
                this.refresh();
            }
        });
        this.0.enabled.replace(Some(AsyncSwitch::new(&enabled_switch, apply, finished, runner)));
        let b = backend;
        let apply: Apply = Arc::new(move |enabled| b.set_outline(enabled).map(Value::Bool));
        let weak = Rc::downgrade(&this.0);
        let finished: Finished = Rc::new(move |_, supported, error| {
            if let Some(inner) = weak.upgrade() {
                let this = TilingControls(inner);
                this.show_error(error.as_ref());
                if error.is_none() && !truthy(supported.as_ref()) {
                    // Saved now; this compositor loads the outline at the next login.
                    this.0.outline_pending_sign_out.set(true);
                }
                this.update_notices();
            }
        });
        this.0.outline.replace(Some(AsyncSwitch::new(&outline_switch, apply, finished, runner)));
        let weak = Rc::downgrade(&this.0);
        shortcuts.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                TilingControls(inner).open_shortcuts();
            }
        });
        this.refresh();
        this
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.widget }
    pub fn enabled_switch(&self) -> &gtk::Switch { &self.0.enabled_switch }
    pub fn enabled_detail(&self) -> &gtk::Label { &self.0.enabled_detail }
    pub fn gapless(&self) -> &GaplessControls { &self.0.gapless }
    pub fn outline_switch(&self) -> &gtk::Switch { &self.0.outline_switch }
    pub fn outline_detail(&self) -> &gtk::Label { &self.0.outline_detail }
    pub fn notice(&self) -> &gtk::Label { &self.0.notice }
    pub fn error(&self) -> &gtk::Label { &self.0.error }
    pub fn sign_out(&self) -> &SignOutButton { &self.0.sign_out }
    pub fn shortcuts(&self) -> &gtk::Button { &self.0.shortcuts }
    pub fn state(&self) -> Reply { self.0.state.borrow().clone() }
    pub fn outline_pending_sign_out(&self) -> bool { self.0.outline_pending_sign_out.get() }

    /// Called after every [`Self::apply_state`] (the popup's subtitle).
    pub fn set_state_hook(&self, hook: StateHook) { self.0.state_hook.replace(Some(hook)); }

    fn enabled(&self) -> AsyncSwitch { self.0.enabled.borrow().clone().unwrap() }
    fn outline(&self) -> AsyncSwitch { self.0.outline.borrow().clone().unwrap() }

    pub fn busy(&self) -> bool { self.enabled().busy() || self.outline().busy() || self.0.gapless.busy() }

    fn show_error(&self, error: Option<&Error>) {
        let text = error.map(|e| e.0.clone()).unwrap_or_default();
        self.0.error.set_text(&text);
        self.0.error.set_visible(!text.is_empty());
    }

    fn open_shortcuts(&self) {
        if !self.0.launcher.is_file() {
            self.show_error(Some(&Error(tr!("Window Tiling is not installed."))));
            return;
        }
        let launcher = self.0.launcher.as_os_str();
        if let Err(error) = gio::Subprocess::newv(&[launcher, std::ffi::OsStr::new("--settings")], gio::SubprocessFlags::NONE) {
            self.show_error(Some(&Error(error.message().into())));
        }
    }

    pub fn launcher(&self) -> &Path { &self.0.launcher }

    pub fn refresh(&self) {
        self.0.gapless.refresh();
        if self.0.loading.get() || self.busy() {
            return;
        }
        self.0.loading.set(true);
        let (status, backend) = (self.0.status_call.clone(), self.0.backend.clone());
        let this = self.clone();
        self.0.runner.run(move || Ok((status(), backend.load_preferences()?)), move |result| {
            this.0.loading.set(false);
            match result {
                Err(error) => this.show_error(Some(&error)),
                Ok((state, preferences)) => this.apply_state(&state, &preferences),
            }
        });
    }

    pub fn apply_state(&self, state: &Reply, preferences: &Map<String, Value>) {
        self.0.state.replace(state.clone());
        if !self.enabled().busy() {
            self.enabled().show(truthy(state.get("enabled")));
            self.0.enabled_switch.set_sensitive(truthy(state.get("ready")));
        }
        if !self.outline().busy() {
            self.outline().show(!matches!(preferences.get("focus_outline"), Some(Value::Bool(false))));
        }
        self.update_notices();
        let hook = self.0.state_hook.borrow().clone();
        if let Some(hook) = hook {
            hook(state, preferences);
        }
    }

    fn update_notices(&self) {
        let state = self.0.state.borrow().clone();
        let ready = truthy(state.get("ready"));
        let outline_later = ready && !truthy(state.get("outline_supported"));
        let parking_later = ready && !truthy(state.get("parking_supported"));
        // Say before the change that this session cannot load it yet.
        let detail = if outline_later { format!("{}\n{}", self.0.outline_text, tr!(SIGN_OUT_BEFORE)) } else { self.0.outline_text.clone() };
        self.0.outline_detail.set_text(&detail);
        let mut notices: Vec<String> = Vec::new();
        if !ready && truthy(state.get("error")) {
            notices.push(py_str(&state["error"]));
        }
        if parking_later {
            notices.push(tr!("Sign out and back in to load the window parking update."));
        }
        let pending = self.0.outline_pending_sign_out.get();
        if pending && outline_later {
            notices.push(tr!("Saved. Sign out and back in to load the compositor outline update."));
        }
        self.0.notice.set_text(&notices.join("\n"));
        self.0.notice.set_visible(!notices.is_empty());
        self.0.sign_out.button.set_visible(truthy(state.get("sign_out_required")) || parking_later || (pending && outline_later));
    }
}
