//! The progress window of command-line jobs (--batch, --add, --dialog) and
//! the password-retry helper every job uses.

use crate::archive::{Ctx, Error, Result};
use crate::job::{self, JobHandle, JobUi};
use crate::ops::{Conflict, Resolution};
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tessel_core::tr;

pub struct ProgressWindow {
    pub window: gtk::Window,
    pub title: gtk::Label,
    pub detail: gtk::Label,
    pub bar: gtk::ProgressBar,
    pub cancel: gtk::Button,
    pub handle: Rc<RefCell<Option<JobHandle>>>,
}

impl ProgressWindow {
    pub fn new(app: &gtk::Application, title: &str) -> Rc<ProgressWindow> {
        let window = gtk::Window::builder().application(app).title(title).default_width(460).resizable(false).build();
        window.set_widget_name("progress-window");
        window.add_css_class("archive-dialog");
        window.set_titlebar(Some(&tessel_gtk::utils::headerbar(title)));
        tessel_gtk::theme::attach(&window, None);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
        body.add_css_class("dialog-body");
        let heading = gtk::Label::builder().label(title).xalign(0.0).ellipsize(pango::EllipsizeMode::Middle).build();
        heading.add_css_class("dialog-heading");
        let detail = gtk::Label::builder().xalign(0.0).ellipsize(pango::EllipsizeMode::Middle).build();
        detail.add_css_class("dim-label");
        let bar = gtk::ProgressBar::new();
        bar.set_widget_name("progress-bar");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_mnemonic(&tr!("Cancel"));
        cancel.set_widget_name("progress-cancel");
        buttons.append(&cancel);
        body.append(&heading);
        body.append(&bar);
        body.append(&detail);
        body.append(&buttons);
        window.set_child(Some(&body));
        let this = Rc::new(ProgressWindow { window: window.clone(), title: heading, detail, bar, cancel: cancel.clone(), handle: Rc::default() });
        let h = this.handle.clone();
        cancel.connect_clicked(move |_| {
            if let Some(handle) = h.borrow().as_ref() {
                handle.cancel();
            }
        });
        let h = this.handle.clone();
        window.connect_close_request(move |_| {
            if let Some(handle) = h.borrow().as_ref().filter(|h| h.running()) {
                handle.cancel();
            }
            glib::Propagation::Proceed
        });
        window.present();
        this
    }

    pub fn ui(self: &Rc<Self>) -> JobUi {
        let (bar, detail) = (self.bar.clone(), self.detail.clone());
        let parent = self.window.clone();
        JobUi {
            progress: Rc::new(move |fraction, item| {
                if fraction < 0.0 {
                    bar.pulse();
                } else {
                    bar.set_fraction(fraction.clamp(0.0, 1.0));
                }
                if !item.is_empty() {
                    detail.set_label(item);
                }
            }),
            conflict: conflict_handler(Some(parent)),
        }
    }
}

pub fn conflict_handler(parent: Option<gtk::Window>) -> Rc<dyn Fn(Conflict, Box<dyn FnOnce(Resolution)>)> {
    Rc::new(move |conflict, reply| {
        super::dialogs::conflict(parent.as_ref(), &conflict, reply);
    })
}

pub type Work<T> = Arc<dyn Fn(&Ctx, &mut dyn FnMut(&Conflict) -> Resolution) -> Result<T> + Send + Sync>;

/// Run `work`; on a password error ask (saying when it was wrong) and run it
/// again with the password. `done` gets the result and the password that worked.
pub fn with_password<T: Send + 'static>(parent: Option<gtk::Window>, name: String, password: Option<String>, ui: Rc<dyn Fn() -> JobUi>,
                                        handle: Rc<RefCell<Option<JobHandle>>>, work: Work<T>,
                                        done: Box<dyn FnOnce(Result<T>, Option<String>)>) {
    let w = work.clone();
    let attempt = password.clone();
    let (p, n, u, h) = (parent.clone(), name.clone(), ui.clone(), handle.clone());
    let job = job::spawn(password, ui(), move |ctx, resolve| w(ctx, resolve), move |result| match result {
        Err(error @ (Error::PasswordNeeded | Error::WrongPassword)) => {
            let wrong = error == Error::WrongPassword || attempt.is_some();
            let owner = p.clone();
            super::dialogs::password(owner.as_ref(), &n.clone(), wrong, move |answer| match answer {
                Some(pw) => with_password(p, n, Some(pw), u, h, work, done),
                None => done(Err(Error::Cancelled), None),
            });
        }
        other => done(other, attempt),
    });
    *handle.borrow_mut() = Some(job);
}
