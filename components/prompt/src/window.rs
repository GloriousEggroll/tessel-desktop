//! `PromptWindow` of wayshell_prompt.py.

use crate::cli::Options;
use crate::{ACCEPT, DECLINE, DISMISS, TEXT_LIMIT};
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::os::fd::RawFd;
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::utils::{add_css, headerbar, image, key_controller, set_margins, PRIORITY_APPLICATION};

pub const CSS: &str = "
                .tessel-prompt .prompt-heading { font-size: 13pt; font-weight: 700; }
                .tessel-prompt .prompt-body { font-size: 10.5pt; }
                .tessel-prompt .prompt-icon { -gtk-icon-style: symbolic; }
                .tessel-prompt .prompt-actions button { min-height: 28px; padding: 6px 18px;
                    border-radius: 8px; box-shadow: none; text-shadow: none; }
                .tessel-prompt textview { padding: 12px; border-radius: 8px; }
                .tessel-prompt textview, .tessel-prompt textview text {
                    background-color: transparent; color: inherit; }
                .tessel-prompt headerbar { box-shadow: none; }
            ";

pub struct PromptWindow {
    pub window: gtk::ApplicationWindow,
    pub options: Options,
    on_result: Box<dyn Fn(i32)>,
    finished: Cell<bool>,
    input_watch: RefCell<Option<glib::SourceId>>,
    input_fd: Cell<Option<RawFd>>,
    input_data: RefCell<Vec<u8>>,
    input_truncated: Cell<bool>,
    pub css: gtk::CssProvider,
    pub heading: gtk::Label,
    pub body: gtk::Label,
    pub details: Option<gtk::TextView>,
    pub cancel: Option<gtk::Button>,
    pub decline: Option<gtk::Button>,
    pub accept: Option<gtk::Button>,
    pub default_button: gtk::Button,
    pub key_controller: RefCell<Option<gtk::EventControllerKey>>,
}

fn label(max_width_chars: i32, class: &str) -> gtk::Label {
    let label = gtk::Label::builder().xalign(0.0).wrap(true).selectable(true).max_width_chars(max_width_chars).build();
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label.add_css_class(class);
    label
}

impl PromptWindow {
    /// `PromptWindow(application, options, on_result, *, stdin_fd=None)`.
    pub fn new(application: &gtk::Application, options: Options, on_result: impl Fn(i32) + 'static,
               stdin_fd: Option<RawFd>) -> Rc<Self> {
        let title = if options.error { tr!("Configuration Error") } else { tr!("Confirm Action") };
        let window = gtk::ApplicationWindow::builder().application(application).title(title.as_str()).build();
        window.add_css_class("tessel-prompt");
        window.set_default_size(if options.error { 540 } else { 440 }, -1);
        window.set_resizable(options.error);
        let bar = headerbar(&title);
        bar.set_decoration_layout(Some(":close"));
        window.set_titlebar(Some(&bar));
        let css = add_css(CSS, Some(&WidgetExt::display(&window)), PRIORITY_APPLICATION);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 20);
        set_margins(&root, 22);
        window.set_child(Some(&root));
        // The headerbar already names the dialog; the message itself is the
        // heading, with any further lines as the body underneath it.
        let heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let icon = image(if options.error { "dialog-error-symbolic" } else { "dialog-question-symbolic" }, 32);
        icon.add_css_class("prompt-icon");
        icon.set_valign(gtk::Align::Start);
        heading_row.append(&icon);
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).hexpand(true).build();
        let heading = label(52, "prompt-heading");
        text.append(&heading);
        let body = label(58, "prompt-body");
        text.append(&body);
        heading_row.append(&text);
        root.append(&heading_row);
        let mut details = None;
        let actions = gtk::Box::builder().spacing(10).halign(gtk::Align::End).build();
        actions.add_css_class("prompt-actions");
        let (mut cancel, mut decline, mut accept) = (None, None, None);
        let default_button;
        let message = options.message.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| tr!("Reading message…"));
        if options.error {
            let view = gtk::TextView::builder().editable(false).cursor_visible(false).monospace(true)
                .wrap_mode(gtk::WrapMode::WordChar).build();
            view.buffer().set_text(&tr!("Reading configuration errors…"));
            let scroll = gtk::ScrolledWindow::builder().vexpand(true).min_content_height(200).max_content_height(420)
                .propagate_natural_height(true).build();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroll.set_child(Some(&view));
            root.append(&scroll);
            details = Some(view);
            let close = gtk::Button::with_label(&tr!("Close"));
            actions.append(&close);
            default_button = close.clone();
            cancel = Some(close);
        } else {
            // The decline button already answers "no"; Escape and the
            // window's close button still dismiss without either branch.
            let no = gtk::Button::with_label(&options.decline_label);
            actions.append(&no);
            let yes = gtk::Button::with_label(&options.accept_label);
            yes.add_css_class(if options.destructive { "destructive-action" } else { "suggested-action" });
            yes.set_sensitive(!options.message_stdin);
            no.set_sensitive(!options.message_stdin);
            actions.append(&yes);
            // Enter confirms ordinary actions; destructive ones need a click.
            default_button = if options.destructive { no.clone() } else { yes.clone() };
            decline = Some(no);
            accept = Some(yes);
        }
        root.append(&actions);
        let error_mode = options.error;
        let prompt = Rc::new(Self {
            window, options, on_result: Box::new(on_result), finished: Cell::new(false), input_watch: RefCell::new(None),
            input_fd: Cell::new(stdin_fd), input_data: RefCell::default(), input_truncated: Cell::new(false), css,
            heading, body, details, cancel, decline, accept, default_button, key_controller: RefCell::new(None),
        });
        prompt.set_message(&message);
        if error_mode {
            prompt.set_message(&tr!("labwc found errors in its configuration."));
        }
        let weak = Rc::downgrade(&prompt);
        let finish_with = |weak: Weak<Self>, code: i32| move |_: &gtk::Button| {
            if let Some(prompt) = weak.upgrade() {
                prompt.finish(code);
            }
        };
        if let Some(button) = &prompt.cancel {
            button.connect_clicked(finish_with(weak.clone(), ACCEPT));
        }
        if let Some(button) = &prompt.decline {
            button.connect_clicked(finish_with(weak.clone(), DECLINE));
        }
        if let Some(button) = &prompt.accept {
            button.connect_clicked(finish_with(weak.clone(), ACCEPT));
        }
        prompt.window.set_default_widget(Some(&prompt.default_button));
        prompt.default_button.grab_focus();
        let close_weak = weak.clone();
        prompt.window.connect_close_request(move |_| {
            if let Some(prompt) = close_weak.upgrade() {
                prompt.close_requested();
            }
            glib::Propagation::Stop
        });
        let key_weak = weak.clone();
        let controller = key_controller(&prompt.window, true, move |_, keyval, _, _| {
            match key_weak.upgrade() {
                Some(prompt) if keyval == gdk::Key::Escape => {
                    prompt.close_requested();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        prompt.key_controller.replace(Some(controller));
        let destroy_weak = weak.clone();
        prompt.window.connect_destroy(move |_| {
            if let Some(prompt) = destroy_weak.upgrade() {
                prompt.stop_input();
            }
        });
        if prompt.options.error || prompt.options.message_stdin {
            let fd = prompt.input_fd.get().unwrap_or(0);
            prompt.input_fd.set(Some(fd));
            match set_nonblocking(fd) {
                Ok(()) => {
                    let read_weak = weak.clone();
                    let condition = glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR | glib::IOCondition::NVAL;
                    let id = tessel_core::glib_compat::unix_fd_add_local(fd, condition, move |_, condition| {
                        match read_weak.upgrade() {
                            Some(prompt) => prompt.read_input(condition),
                            None => glib::ControlFlow::Break,
                        }
                    });
                    prompt.input_watch.replace(Some(id));
                }
                Err(error) => prompt.input_failed(&error),
            }
        }
        prompt
    }

    /// First line → heading, the rest → body (hidden when empty).
    pub fn set_message(&self, text: &str) {
        let text = text.trim_matches('\n');
        let (heading, body) = text.split_once('\n').unwrap_or((text, ""));
        self.heading.set_text(heading);
        self.body.set_text(body.trim_matches('\n'));
        self.body.set_visible(!self.body.text().is_empty());
    }

    pub fn message_text(&self) -> String {
        let body = self.body.text();
        if body.is_empty() { self.heading.text().to_string() } else { format!("{}\n{body}", self.heading.text()) }
    }

    fn input_failed(&self, error: &std::io::Error) {
        let text = tr!("Could not read message: {error}", error = tessel_core::pyerr::os_error_text(error));
        match &self.details {
            Some(details) => details.buffer().set_text(&text),
            None => self.set_message(&text),
        }
    }

    fn read_input(&self, condition: glib::IOCondition) -> glib::ControlFlow {
        if self.finished.get() {
            self.input_watch.replace(None);
            return glib::ControlFlow::Break;
        }
        let fd = self.input_fd.get().unwrap_or(0);
        let mut ended = false;
        // Drain oversized input without keeping it in memory or blocking
        // labwc's writer. Bound work per main-loop iteration.
        let mut chunk = vec![0u8; 8192];
        for _ in 0..8 {
            let count = loop {
                // SAFETY: reading into an owned buffer of the given length.
                let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
                if count < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break count;
            };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                self.input_failed(&error);
                self.input_watch.replace(None);
                return glib::ControlFlow::Break;
            }
            if count == 0 {
                ended = true;
                break;
            }
            let count = count as usize;
            let mut data = self.input_data.borrow_mut();
            let available = TEXT_LIMIT.saturating_sub(data.len());
            data.extend_from_slice(&chunk[..count.min(available)]);
            if count > available {
                self.input_truncated.set(true);
            }
        }
        let mut text = String::from_utf8_lossy(&self.input_data.borrow()).into_owned();
        if self.options.message_stdin && ended && text.ends_with('\n') {
            text.pop(); // The launcher's here-document adds one newline.
        }
        if self.input_truncated.get() {
            text += "\n\n";
            text += &tr!("Additional text was omitted.");
        }
        match &self.details {
            Some(details) => {
                let shown = if text.is_empty() { tr!("No error details were provided.") } else { text };
                details.buffer().set_text(&shown);
            }
            None => {
                self.set_message(&text);
                // Never approve an unread or truncated action description.
                let ready = ended && !self.input_truncated.get();
                for button in [&self.accept, &self.decline].into_iter().flatten() {
                    button.set_sensitive(ready);
                }
                if self.default_button.is_sensitive() && !self.default_button.has_focus() {
                    self.default_button.grab_focus();
                }
            }
        }
        if ended || condition.intersects(glib::IOCondition::ERR | glib::IOCondition::NVAL) {
            self.input_watch.replace(None);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    }

    pub fn stop_input(&self) {
        if let Some(id) = self.input_watch.take() {
            tessel_core::glib_compat::source_remove(id);
        }
    }

    /// Whether stdin is still watched (Python `input_watch != 0`).
    pub fn input_watched(&self) -> bool {
        self.input_watch.borrow().is_some()
    }

    pub fn input_len(&self) -> usize {
        self.input_data.borrow().len()
    }

    pub fn input_truncated(&self) -> bool {
        self.input_truncated.get()
    }

    pub fn finished(&self) -> bool {
        self.finished.get()
    }

    pub fn finish(&self, code: i32) {
        if self.finished.replace(true) {
            return;
        }
        self.stop_input();
        self.window.destroy();
        (self.on_result)(code);
    }

    pub fn close_requested(&self) {
        self.finish(if self.options.error { ACCEPT } else { DISMISS });
    }
}

fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: plain fcntl on a caller-provided descriptor.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}
