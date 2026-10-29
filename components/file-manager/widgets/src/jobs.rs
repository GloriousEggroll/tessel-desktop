//! Port of file_jobs.py: application-wide file job list, shared progress popover
//! and close handling.
//!
//! Every Files window can start any number of operations; they run concurrently
//! on their own worker threads (`files_ops::operations::Job`) and are listed in one
//! application-wide manager. Each running job holds the GApplication so closing
//! the last window never interrupts it, and a completion notification is sent
//! when no window is left to show the result.

use gtk::prelude::*;
use gtk::pango;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use suite_core::tr;
use suite_gtk::utils::{accessible, image, set_margins};

/// Anything a job entry can cancel (`files_ops::operations::Job` implements it).
pub trait JobHandle {
    fn cancel(&self);
}

impl JobHandle for files_ops::operations::Job {
    fn cancel(&self) {
        files_ops::operations::Job::cancel(self);
    }
}

const ALIVE_KEY: &str = "files-window-alive";

/// Mark a Files browser window as alive (Python `window.alive` on a window with a
/// `jobs_button`). The lead's FilesWindow sets `true` when built, `false` on close.
pub fn set_window_alive(window: &impl IsA<gtk::Window>, alive: bool) {
    unsafe { window.as_ref().set_data(ALIVE_KEY, alive) };
}

/// `getattr(window, 'alive', False) and hasattr(window, 'jobs_button')`.
pub fn window_alive(window: &impl IsA<gtk::Window>) -> bool {
    unsafe { window.as_ref().data::<bool>(ALIVE_KEY).map(|p| *p.as_ref()).unwrap_or(false) }
}

pub struct JobEntry {
    pub ident: u64,
    pub title: String,
    job: RefCell<Option<Rc<dyn JobHandle>>>,
    pub detail: RefCell<String>,
    /// `None` pulses an activity bar.
    pub fraction: Cell<Option<f64>>,
    origin: Option<glib::WeakRef<gtk::Window>>,
    /// Free slot for the caller (Python `entry.cut_generation`).
    pub cut_generation: Cell<Option<u64>>,
    pub cancelled: Cell<bool>,
}

impl JobEntry {
    pub fn new(ident: u64, title: &str, job: Option<Rc<dyn JobHandle>>, origin: Option<&gtk::Window>) -> Self {
        Self {
            ident, title: title.to_string(), job: RefCell::new(job), detail: RefCell::new(String::new()), fraction: Cell::new(None),
            origin: origin.map(|w| w.downgrade()), cut_generation: Cell::new(None), cancelled: Cell::new(false),
        }
    }

    /// The originating window while it is still a live Files window.
    pub fn window(&self) -> Option<gtk::Window> {
        self.origin.as_ref().and_then(|w| w.upgrade()).filter(window_alive)
    }

    /// `entry.job = operations.start(...)` after `add()`.
    pub fn set_job(&self, job: Option<Rc<dyn JobHandle>>) {
        self.job.replace(job);
    }

    pub fn job(&self) -> Option<Rc<dyn JobHandle>> {
        self.job.borrow().clone()
    }

    pub fn cancel(&self) {
        self.cancelled.set(true);
        let job = self.job();
        if let Some(job) = job {
            job.cancel();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Progress,
    Removed,
}

impl Change {
    pub fn name(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Progress => "progress",
            Change::Removed => "removed",
        }
    }
}

pub type Listener = Rc<dyn Fn(Change, &Rc<JobEntry>)>;

struct ManagerInner {
    app: gtk::Application,
    entries: RefCell<Vec<Rc<JobEntry>>>,
    listeners: RefCell<Vec<(u64, Listener)>>,
    next: Cell<u64>,
    next_listener: Cell<u64>,
    notifier: RefCell<Option<Notifier>>,
}

/// Replaces `app.send_notification(id, notification)` (test hook).
pub type Notifier = Rc<dyn Fn(&str, &gio::Notification)>;

/// Track running jobs for one application and notify subscribed windows.
#[derive(Clone)]
pub struct JobManager(Rc<ManagerInner>);

impl JobManager {
    pub fn new(app: &gtk::Application) -> Self {
        Self(Rc::new(ManagerInner {
            app: app.clone(), entries: RefCell::default(), listeners: RefCell::default(), next: Cell::new(1), next_listener: Cell::new(1), notifier: RefCell::new(None),
        }))
    }

    fn raw_app(&self) -> *mut gio::ffi::GApplication {
        glib::translate::ToGlibPtr::to_glib_none(self.0.app.upcast_ref::<gio::Application>()).0
    }

    pub fn set_notifier(&self, notifier: Option<Notifier>) {
        self.0.notifier.replace(notifier);
    }

    pub fn app(&self) -> &gtk::Application {
        &self.0.app
    }

    /// `bool(manager)` is `!is_empty()`.
    pub fn is_empty(&self) -> bool {
        self.0.entries.borrow().is_empty()
    }

    pub fn entries(&self) -> Vec<Rc<JobEntry>> {
        self.0.entries.borrow().clone()
    }

    /// Returns the id to pass to [`unsubscribe`](Self::unsubscribe).
    pub fn subscribe(&self, callback: Listener) -> u64 {
        let id = self.0.next_listener.get();
        self.0.next_listener.set(id + 1);
        self.0.listeners.borrow_mut().push((id, callback));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.0.listeners.borrow_mut().retain(|(i, _)| *i != id);
    }

    fn emit(&self, change: Change, entry: &Rc<JobEntry>) {
        let listeners: Vec<Listener> = self.0.listeners.borrow().iter().map(|(_, l)| l.clone()).collect();
        for callback in listeners {
            callback(change, entry);
        }
    }

    fn contains(&self, entry: &Rc<JobEntry>) -> bool {
        self.0.entries.borrow().iter().any(|e| Rc::ptr_eq(e, entry))
    }

    pub fn add(&self, title: &str, origin: Option<&gtk::Window>, job: Option<Rc<dyn JobHandle>>) -> Rc<JobEntry> {
        let entry = Rc::new(JobEntry::new(self.0.next.get(), title, job, origin));
        self.0.next.set(self.0.next.get() + 1);
        self.0.entries.borrow_mut().push(entry.clone());
        // Python pairs hold()/release() by hand (no guard object).
        unsafe { gio::ffi::g_application_hold(self.raw_app()) };
        self.emit(Change::Added, &entry);
        entry
    }

    pub fn update(&self, entry: &Rc<JobEntry>, detail: Option<&str>, fraction: Option<f64>) {
        if !self.contains(entry) {
            return;
        }
        if let Some(detail) = detail {
            entry.detail.replace(detail.to_string());
        }
        entry.fraction.set(fraction);
        self.emit(Change::Progress, entry);
    }

    /// Remove a job; notify when no Files window remains to show it.
    pub fn finish(&self, entry: &Rc<JobEntry>, summary: Option<&str>, failed: bool) {
        if !self.contains(entry) {
            return;
        }
        self.0.entries.borrow_mut().retain(|e| !Rc::ptr_eq(e, entry));
        self.emit(Change::Removed, entry);
        if let Some(summary) = summary.filter(|s| !s.is_empty()) {
            if !entry.cancelled.get() && self.windows().is_empty() {
                let notification = gio::Notification::new(&if failed { tr!("File operation failed") } else { tr!("File operation finished") });
                notification.set_body(Some(summary));
                notification.set_icon(&gio::ThemedIcon::new("system-file-manager"));
                let id = format!("files-job-{}", entry.ident);
                let notifier = self.0.notifier.borrow().clone();
                match notifier {
                    Some(notify) => notify(&id, &notification),
                    None => self.0.app.send_notification(Some(&id), &notification),
                }
            }
        }
        unsafe { gio::ffi::g_application_release(self.raw_app()) };
    }

    pub fn windows(&self) -> Vec<gtk::Window> {
        self.0.app.windows().into_iter().filter(window_alive).collect()
    }

    pub fn owned_by(&self, window: &gtk::Window) -> Vec<Rc<JobEntry>> {
        self.0.entries.borrow().iter().filter(|e| e.window().as_ref() == Some(window)).cloned().collect()
    }

    pub fn cancel_all(&self, entries: Option<&[Rc<JobEntry>]>) {
        let entries = entries.map(|e| e.to_vec()).unwrap_or_else(|| self.entries());
        for entry in entries {
            entry.cancel();
        }
    }
}

type Row = (gtk::Box, gtk::Label, gtk::ProgressBar, gtk::Button);

struct ButtonInner {
    button: gtk::MenuButton,
    manager: JobManager,
    indicator: gtk::ProgressBar,
    list: gtk::Box,
    popover: gtk::Popover,
    rows: RefCell<Vec<(u64, Row)>>,
    pulse_source: RefCell<Option<glib::SourceId>>,
    listener: Cell<u64>,
}

/// Header button with a popover listing every running job of the app.
#[derive(Clone)]
pub struct JobsButton(Rc<ButtonInner>);

impl JobsButton {
    pub fn new(manager: &JobManager) -> Self {
        let button = gtk::MenuButton::builder().tooltip_text(tr!("File operations")).build();
        accessible(&button, &tr!("File operations"));
        let indicator = gtk::ProgressBar::builder().valign(gtk::Align::Center).width_request(28).build();
        indicator.add_css_class("files-jobs-indicator");
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&image("emblem-synchronizing-symbolic", 16));
        content.append(&indicator);
        button.set_child(Some(&content));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        set_margins(&list, 12);
        let heading = gtk::Label::builder().label(tr!("File operations")).xalign(0.0).build();
        heading.add_css_class("heading");
        list.append(&heading);
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true)
            .max_content_height(420).min_content_width(340).build();
        scroll.set_child(Some(&list));
        let popover = gtk::Popover::builder().child(&scroll).build();
        popover.add_css_class("files-jobs");
        button.set_popover(Some(&popover));
        let this = Self(Rc::new(ButtonInner {
            button, manager: manager.clone(), indicator, list, popover, rows: RefCell::default(),
            pulse_source: RefCell::new(None), listener: Cell::new(0),
        }));
        let weak = Rc::downgrade(&this.0);
        let id = manager.subscribe(Rc::new(move |change, entry| {
            if let Some(inner) = weak.upgrade() {
                JobsButton(inner).changed(change, entry);
            }
        }));
        this.0.listener.set(id);
        for entry in manager.entries() {
            this.add(&entry);
        }
        this.refresh();
        this
    }

    pub fn widget(&self) -> &gtk::MenuButton {
        &self.0.button
    }

    pub fn indicator(&self) -> &gtk::ProgressBar {
        &self.0.indicator
    }

    pub fn list(&self) -> &gtk::Box {
        &self.0.list
    }

    /// `(row, title, progress, cancel)` widgets for a job id.
    pub fn row(&self, ident: u64) -> Option<(gtk::Box, gtk::Label, gtk::ProgressBar, gtk::Button)> {
        self.0.rows.borrow().iter().find(|(i, _)| *i == ident).map(|(_, r)| r.clone())
    }

    /// `list(button.rows)`.
    pub fn idents(&self) -> Vec<u64> {
        self.0.rows.borrow().iter().map(|(i, _)| *i).collect()
    }

    pub fn pulsing(&self) -> bool {
        self.0.pulse_source.borrow().is_some()
    }

    pub fn close(&self) {
        self.0.manager.unsubscribe(self.0.listener.get());
        if let Some(source) = self.0.pulse_source.take() {
            source.remove();
        }
    }

    fn add(&self, entry: &Rc<JobEntry>) {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        row.add_css_class("files-job");
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(pango::EllipsizeMode::Middle).max_width_chars(40).build();
        line.append(&title);
        let cancel = gtk::Button::builder().child(&image("process-stop-symbolic", 16)).tooltip_text(tr!("Cancel operation")).build();
        accessible(&cancel, &tr!("Cancel operation"));
        cancel.add_css_class("flat");
        let (weak, target) = (Rc::downgrade(&self.0), Rc::downgrade(entry));
        cancel.connect_clicked(move |_| {
            if let (Some(inner), Some(entry)) = (weak.upgrade(), target.upgrade()) {
                entry.cancel();
                JobsButton(inner).update(&entry);
            }
        });
        line.append(&cancel);
        row.append(&line);
        let progress = gtk::ProgressBar::builder().hexpand(true).build();
        row.append(&progress);
        self.0.list.append(&row);
        self.0.rows.borrow_mut().push((entry.ident, (row, title, progress, cancel)));
        self.update(entry);
    }

    fn update(&self, entry: &JobEntry) {
        let Some((_row, title, progress, cancel)) = self.row(entry.ident) else { return };
        let detail = entry.detail.borrow().clone();
        let mut text = entry.title.clone();
        if !detail.is_empty() {
            text += &format!(" · {detail}");
        }
        if entry.cancelled.get() {
            text += &format!(" · {}", tr!("Cancelling…"));
        }
        title.set_text(&text);
        title.set_tooltip_text(Some(&text));
        cancel.set_sensitive(!entry.cancelled.get());
        match entry.fraction.get() {
            None => progress.pulse(),
            Some(fraction) => progress.set_fraction(fraction),
        }
    }

    fn changed(&self, change: Change, entry: &Rc<JobEntry>) {
        match change {
            Change::Added => self.add(entry),
            Change::Removed => {
                let removed = {
                    let mut rows = self.0.rows.borrow_mut();
                    rows.iter().position(|(i, _)| *i == entry.ident).map(|index| rows.remove(index))
                };
                if let Some((_, (row, ..))) = removed {
                    self.0.list.remove(&row);
                }
            }
            Change::Progress => self.update(entry),
        }
        self.refresh();
    }

    fn refresh(&self) {
        let entries = self.0.manager.entries();
        self.0.button.set_visible(!entries.is_empty());
        let known: Vec<f64> = entries.iter().filter_map(|e| e.fraction.get()).collect();
        if !entries.is_empty() && known.len() == entries.len() {
            self.0.indicator.set_fraction(known.iter().sum::<f64>() / known.len() as f64);
        } else if !entries.is_empty() {
            self.0.indicator.pulse();
        }
        if !entries.is_empty() && self.0.pulse_source.borrow().is_none() {
            let weak = Rc::downgrade(&self.0);
            let source = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                match weak.upgrade() {
                    Some(inner) => JobsButton(inner).pulse(),
                    None => glib::ControlFlow::Break,
                }
            });
            self.0.pulse_source.replace(Some(source));
        }
        if entries.is_empty() {
            if let Some(source) = self.0.pulse_source.take() {
                source.remove();
            }
            self.0.popover.popdown();
        }
        let count = entries.len();
        self.0.button.set_tooltip_text(Some(&if count != 1 {
            tr!("{count} file operations running", count = count)
        } else {
            tr!("1 file operation running")
        }));
    }

    fn pulse(&self) -> glib::ControlFlow {
        let entries = self.0.manager.entries();
        if entries.is_empty() {
            self.0.pulse_source.replace(None);
            return glib::ControlFlow::Break;
        }
        for entry in &entries {
            if entry.fraction.get().is_none() {
                self.update(entry);
            }
        }
        if entries.iter().any(|e| e.fraction.get().is_none()) {
            self.0.indicator.pulse();
        }
        glib::ControlFlow::Continue
    }
}

/// What to do with running jobs when their window closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepAnswer {
    Keep,
    Cancel,
}

/// `ask_keep_running(parent, callback)`: `callback(Some(Keep | Cancel))`, or `None` when
/// the question is dismissed (the window stays open). Keeping them is the default.
pub fn ask_keep_running(parent: Option<&gtk::Window>, callback: impl FnOnce(Option<KeepAnswer>) + 'static) -> gtk::Window {
    let (window, content) = crate::dialogs::dialog_window(parent, &tr!("File Operations Running"), 460, -1);
    content.append(&gtk::Label::builder()
        .label(tr!("File operations started in this window are still running. Keep them running in the background after the window closes, or cancel them? Completed items are retained."))
        .xalign(0.0).wrap(true).max_width_chars(56).build());
    let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
    let cancel = gtk::Button::with_label(&tr!("Cancel Operations"));
    cancel.add_css_class("destructive-action");
    let keep = gtk::Button::with_label(&tr!("Keep Running in Background"));
    keep.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&keep);
    content.append(&buttons);
    window.set_default_widget(Some(&keep));
    let once = crate::dialogs::Once::new(callback);
    for (button, answer) in [(&cancel, KeepAnswer::Cancel), (&keep, KeepAnswer::Keep)] {
        let (once, weak) = (once.clone(), window.downgrade());
        button.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                once.complete(&window, Some(answer));
            }
        });
    }
    window.connect_close_request(move |window| {
        once.complete(window, None);
        glib::Propagation::Proceed
    });
    window.present();
    keep.grab_focus();
    window
}
