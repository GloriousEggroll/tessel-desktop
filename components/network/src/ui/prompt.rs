//! The agent's credential dialog (`_fields_dialog` of secret_agent.py):
//! 420 px, not resizable, Cancel / Connect (default), masked entries with
//! `InputPurpose.PASSWORD`, optional "Remember credentials in the keyring".
//! Entries are blanked after a response.

use crate::agent::{PromptHandle, Prompter};
use crate::gui::{ensure_gtk, Gui};
use crate::ui::style::{accessible_label, margins};
use crate::vpn_auth::Field;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use tessel_core::tr;

pub struct GtkPrompter {
    pub gui: Weak<Gui>,
}

struct DialogHandle(glib::WeakRef<gtk::Window>);

impl PromptHandle for DialogHandle {
    fn close(&self) {
        if let Some(window) = self.0.upgrade() {
            window.destroy();
        }
    }
}

/// Build the dialog; `(dialog, entries)` like the Python helper.
#[allow(deprecated)]
pub fn fields_dialog(title: &str, message: &str, fields: &[Field]) -> (gtk::Dialog, Vec<(String, gtk::Entry)>) {
    let dialog = gtk::Dialog::new();
    dialog.set_title(Some(title));
    dialog.set_default_size(420, -1);
    dialog.set_resizable(false);
    dialog.add_button(&tr!("Cancel"), gtk::ResponseType::Cancel);
    dialog.add_button(&tr!("Connect"), gtk::ResponseType::Ok);
    dialog.set_default_response(gtk::ResponseType::Ok);
    let content = dialog.content_area();
    margins(&content, 18);
    content.set_spacing(10);
    let caption = gtk::Label::builder().label(message).xalign(0.0).wrap(true).max_width_chars(52).build();
    content.append(&caption);
    let mut entries = Vec::new();
    for (key, label, value, masked) in fields {
        content.append(&gtk::Label::builder().label(label.as_str()).xalign(0.0).build());
        let entry = gtk::Entry::new();
        entry.set_visibility(!masked);
        entry.set_input_purpose(if *masked { gtk::InputPurpose::Password } else { gtk::InputPurpose::FreeForm });
        entry.set_text(value);
        entry.set_activates_default(true);
        accessible_label(&entry, label);
        content.append(&entry);
        entries.push((key.clone(), entry));
    }
    (dialog, entries)
}

impl Prompter for GtkPrompter {
    #[allow(deprecated)]
    fn fields(&self, title: &str, message: &str, fields: &[Field], remember: Option<bool>,
              done: Box<dyn FnOnce(Option<(Vec<(String, String)>, bool)>)>) -> Result<Box<dyn PromptHandle>, String> {
        if !ensure_gtk() {
            return Err("No display is available.".into());
        }
        let gui = self.gui.upgrade().ok_or("The network service is closing.")?;
        let (dialog, entries) = fields_dialog(title, message, fields);
        let check = remember.map(|initial| {
            let check = gtk::CheckButton::with_label(&tr!("Remember credentials in the keyring"));
            check.set_active(initial);
            dialog.content_area().append(&check);
            check
        });
        let done = Rc::new(RefCell::new(Some(done)));
        let respond = {
            let done = done.clone();
            move |dialog: &gtk::Dialog, ok: bool| {
                let values: Vec<(String, String)> = entries.iter().map(|(k, e)| (k.clone(), e.text().to_string())).collect();
                let remembered = check.as_ref().is_some_and(|c| c.is_active());
                for (_, entry) in &entries {
                    entry.set_text("");
                }
                let callback = done.borrow_mut().take();
                if let Some(callback) = callback {
                    callback(if ok { Some((values, remembered)) } else { None });
                }
                dialog.destroy();
            }
        };
        let respond = Rc::new(respond);
        let r = respond.clone();
        dialog.connect_response(move |dialog, response| r(dialog, response == gtk::ResponseType::Ok));
        let d = done.clone();
        dialog.connect_destroy(move |_| {
            // Closed by the window manager or by finish(): a pending answer is a cancel.
            if let Some(callback) = d.borrow_mut().take() {
                callback(None);
            }
        });
        gui.prepare_dialog(Some(dialog.upcast_ref()));
        Ok(Box::new(DialogHandle(dialog.upcast_ref::<gtk::Window>().downgrade())))
    }

    fn before_helper(&self) {
        if let Some(gui) = self.gui.upgrade() {
            gui.prepare_dialog(None);
        }
    }
}
