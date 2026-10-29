//! `page_widgets.py`: small building blocks shared by the system pages
//! (Keyboard, About, Users…).
use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use suite_gtk::utils::{add_css, PRIORITY_APPLICATION};

use crate::style::{page_header, PageHeader, PAGE_CSS, PAGE_MARGIN};

pub const CSS: &str = "
.system-page .subtitle, .system-page .detail { color: alpha(@theme_fg_color, .7); font-size: 9.5pt; }
.system-page .group-title { font-size: 11pt; font-weight: 700; margin-top: 8px; }
.system-page .row-title { font-size: 10.5pt; }
.system-page .error { color: @error_color; }
.system-page .warning { color: @warning_color; }
.system-page .value { font-family: monospace; }
.system-page .keycap { font-family: monospace; font-size: 9.5pt; padding: 2px 7px; border-radius: 6px;
    border: 1px solid alpha(@theme_fg_color, .22); background-color: alpha(@theme_fg_color, .06); }
.system-page button.pill { padding: 6px 16px; border-radius: 8px; }
.system-page scrollbar slider, .system-page scrollbar.horizontal slider {
    min-width: 6px; min-height: 6px; margin: 0; padding: 0;
    border: 2px solid transparent; border-radius: 8px; background-clip: padding-box; }
.system-page scrollbar.vertical slider { min-height: 30px; }
.system-page scrollbar.horizontal slider { min-width: 30px; }
";

/// `label(text, css=None, xalign=0, wrap=True)`.
pub fn label(text: &str, css: Option<&str>) -> gtk::Label {
    label_with(text, css, true)
}

pub fn label_with(text: &str, css: Option<&str>, wrap: bool) -> gtk::Label {
    let widget = gtk::Label::builder().label(text).xalign(0.0).wrap(wrap).build();
    if wrap {
        widget.set_wrap_mode(pango::WrapMode::WordChar);
    }
    if let Some(css) = css {
        for name in css.split_whitespace() {
            widget.add_css_class(name);
        }
    }
    widget
}

/// Scrollable page with a title, subtitle and CSS released on close.
pub struct Page {
    pub widget: gtk::ScrolledWindow,
    pub provider: gtk::CssProvider,
    pub content: gtk::Box,
    pub header: PageHeader,
    closed: Rc<Cell<bool>>,
}

impl Page {
    pub fn new(css_class: &str, title: &str, subtitle: &str, icon: Option<&str>) -> Page {
        let widget = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        widget.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        widget.add_css_class("system-page");
        widget.add_css_class(css_class);
        let provider = add_css(&format!("{CSS}{PAGE_CSS}"), Some(&WidgetExt::display(&widget)), PRIORITY_APPLICATION);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .margin_top(PAGE_MARGIN).margin_bottom(PAGE_MARGIN).margin_start(PAGE_MARGIN).margin_end(PAGE_MARGIN).build();
        widget.set_child(Some(&content));
        let header = page_header(title, Some(subtitle), icon);
        content.append(&header.root);
        Page { widget, provider, content, header, closed: Rc::new(Cell::new(false)) }
    }

    /// `group(title, detail, parent)` → the `card` box.
    pub fn group(&self, title: Option<&str>, detail: Option<&str>, parent: Option<&gtk::Box>) -> gtk::Box {
        let parent = parent.unwrap_or(&self.content);
        if let Some(title) = title.filter(|t| !t.is_empty()) {
            parent.append(&label(title, Some("group-title")));
        }
        if let Some(detail) = detail.filter(|t| !t.is_empty()) {
            parent.append(&label(detail, Some("detail")));
        }
        let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
        b.add_css_class("card");
        parent.append(&b);
        b
    }

    /// `Page.row(parent, title, widget, detail)`.
    pub fn row(parent: &gtk::Box, title: &str, widget: Option<&gtk::Widget>, detail: Option<&str>) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        row.add_css_class("gs-row");
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true)
            .valign(gtk::Align::Center).build();
        text.append(&label(title, Some("row-title")));
        if let Some(detail) = detail.filter(|t| !t.is_empty()) {
            text.append(&label(detail, Some("detail")));
        }
        row.append(&text);
        if let Some(widget) = widget {
            widget.set_valign(gtk::Align::Center);
            row.append(widget);
            widget.update_property(&[gtk::accessible::Property::Label(title)]);
        }
        parent.append(&row);
        row
    }

    /// Shared closed flag (for callbacks that outlive a borrow of the page).
    pub fn closed_flag(&self) -> Rc<Cell<bool>> { self.closed.clone() }

    pub fn closed(&self) -> bool { self.closed.get() }

    /// `run(work, done)`: work() in a thread, done(result) on the GTK thread
    /// unless the page closed meanwhile. A panic in `work` arrives as `Err`.
    pub fn run<T: Send + 'static>(&self, work: impl FnOnce() -> Result<T, String> + Send + 'static,
                                  done: impl FnOnce(Result<T, String>) + 'static) {
        run_in_thread(self.closed.clone(), work, done)
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.widget), &self.provider);
    }
}

/// Background work delivered to the main context unless `closed` is set.
pub fn run_in_thread<T: Send + 'static>(closed: Rc<Cell<bool>>, work: impl FnOnce() -> Result<T, String> + Send + 'static,
                                        done: impl FnOnce(Result<T, String>) + 'static) {
    let (sender, receiver) = std::sync::mpsc::channel::<Result<T, String>>();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err("worker failed".to_owned()));
        let _ = sender.send(result);
    });
    let mut done = Some(done);
    glib::timeout_add_local(std::time::Duration::from_millis(20), move || match receiver.try_recv() {
        Ok(result) => {
            if !closed.get() {
                if let Some(done) = done.take() {
                    done(result);
                }
            }
            glib::ControlFlow::Break
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(_) => glib::ControlFlow::Break,
    });
}

pub fn dropdown(strings: &[String], selected: i32) -> gtk::DropDown {
    let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
    let widget = gtk::DropDown::from_strings(&refs);
    widget.set_selected(selected.max(0) as u32);
    widget
}

/// `spawn(argv, done, flags)`: start argv asynchronously; `done(success,
/// stdout_text)` when it exits (stderr merged by default).
pub fn spawn(argv: &[String], done: Option<Box<dyn FnOnce(bool, String)>>,
             flags: Option<gio::SubprocessFlags>) -> Option<gio::Subprocess> {
    let flags = flags.unwrap_or(gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE);
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    match gio::Subprocess::newv(&args, flags) {
        Err(error) => {
            if let Some(done) = done {
                done(false, error.message().to_owned());
            }
            None
        }
        Ok(process) => {
            let child = process.clone();
            process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
                let (success, out) = match result {
                    Ok((out, _err)) => (child.is_successful(), out.map(|s| s.to_string()).unwrap_or_default()),
                    Err(error) => (false, error.message().to_owned()),
                };
                if let Some(done) = done {
                    done(success, out);
                }
            });
            Some(process)
        }
    }
}

pub fn status_text(widget: &gtk::Label, text: &str, css: Option<&str>) {
    for name in ["error", "warning"] {
        widget.remove_css_class(name);
    }
    if let Some(css) = css.filter(|c| !c.is_empty()) {
        widget.add_css_class(css);
    }
    widget.set_text(text);
}
