//! waykeyring-prompt: asks for the login password when waykeyring could not
//! be unlocked at sign-in (fingerprint login, password changed elsewhere) or
//! has not been set up yet. Prints the password to stdout and exits 0;
//! exits 1 when cancelled. waykeyring checks it and runs this again with
//! `--retry` after a wrong password.
//!
//!   waykeyring-prompt --unlock | --setup [--retry]
use gtk::prelude::*;
use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;
use suite_core::tr;
use suite_gtk::utils::{headerbar, image, key_controller};

const CSS: &str = ".keyring-prompt .settings-tile { border-radius: 14px; }";

fn main() {
    suite_core::i18n::initialize();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let setup = args.iter().any(|a| a == "--setup");
    let retry = args.iter().any(|a| a == "--retry");
    if gtk::init().is_err() {
        eprintln!("waykeyring-prompt: no display");
        std::process::exit(1);
    }
    let main_loop = glib::MainLoop::new(None, false);
    let entered = Rc::new(Cell::new(false));

    let title = tr!("Saved Passwords");
    let window = gtk::Window::builder().title(&title).resizable(false).build();
    window.add_css_class("keyring-prompt");
    window.set_titlebar(Some(&headerbar(&title)));
    window.set_default_size(440, -1);
    suite_gtk::theme::attach(&window, None);
    suite_gtk::utils::add_css(CSS, Some(&WidgetExt::display(&window)), suite_gtk::utils::PRIORITY_APPLICATION);
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14)
        .margin_top(22).margin_bottom(20).margin_start(22).margin_end(22).build();
    window.set_child(Some(&body));
    let heading = gtk::Box::builder().spacing(14).build();
    let tile = gtk::Box::builder().halign(gtk::Align::Start).valign(gtk::Align::Start).build();
    tile.add_css_class("settings-tile");
    tile.set_size_request(52, 52);
    let icon = image("dialog-password-symbolic", 32);
    icon.set_hexpand(true);
    icon.set_vexpand(true);
    tile.append(&icon);
    heading.append(&tile);
    let message = if setup {
        tr!("An application wants to save a password. Enter your login password to start keeping saved passwords; it will unlock them automatically from now on.")
    } else {
        tr!("An application needs your saved passwords. Enter your login password to unlock them.")
    };
    heading.append(&gtk::Label::builder().label(&message).xalign(0.0).wrap(true).max_width_chars(44).hexpand(true).build());
    body.append(&heading);
    let password = tr!("Login password");
    let entry = gtk::Entry::with_buffer(&gtk::PasswordEntryBuffer::new());
    entry.set_visibility(false);
    entry.set_input_purpose(gtk::InputPurpose::Password);
    entry.set_placeholder_text(Some(&password));
    entry.update_property(&[gtk::accessible::Property::Label(&password)]);
    body.append(&entry);
    if retry {
        let status = gtk::Label::builder().label(tr!("That password did not work. Try again.")).xalign(0.0).wrap(true).build();
        status.add_css_class("error");
        body.append(&status);
    }
    let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
    let cancel = gtk::Button::with_label(&tr!("Cancel"));
    let unlock = gtk::Button::with_label(&if setup { tr!("Continue") } else { tr!("Unlock") });
    unlock.add_css_class("suggested-action");
    unlock.set_sensitive(false);
    buttons.append(&cancel);
    buttons.append(&unlock);
    body.append(&buttons);

    let submit = {
        let (entry, window, entered) = (entry.clone(), window.clone(), entered.clone());
        Rc::new(move || {
            let text = entry.text();
            if text.is_empty() {
                return;
            }
            let mut out = std::io::stdout().lock();
            let ok = out.write_all(text.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush());
            entry.set_text("");
            entered.set(ok.is_ok());
            window.close();
        })
    };
    let check = unlock.clone();
    entry.connect_changed(move |e| check.set_sensitive(!e.text().is_empty()));
    let s = submit.clone();
    entry.connect_activate(move |_| s());
    let s = submit.clone();
    unlock.connect_clicked(move |_| s());
    let w = window.clone();
    cancel.connect_clicked(move |_| w.close());
    let w = window.clone();
    key_controller(&window, false, move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    let quit = main_loop.clone();
    window.connect_close_request(move |_| {
        quit.quit();
        glib::Propagation::Proceed
    });
    window.present();
    entry.grab_focus();
    main_loop.run();
    std::process::exit(if entered.get() { 0 } else { 1 });
}
