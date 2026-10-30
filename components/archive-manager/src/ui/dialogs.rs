//! Small dialogs: message, confirm, text prompt, password (with wrong
//! password retry), extraction conflicts (Overwrite / Skip / Rename /
//! Overwrite All / Skip All / Cancel) and the archive comment editor.

use crate::ops::{free_name, Conflict, Resolution};
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::RefCell;
use std::rc::Rc;
use suite_core::tr;

pub const CSS: &str = "
.archive-dialog .dialog-body { margin: 18px; }
.archive-dialog .dialog-buttons { margin: 0 18px 18px 18px; }
.archive-dialog .dialog-heading { font-weight: 700; font-size: 12pt; }
.archive-dialog .dim-label, .archive-window .dim-label { opacity: .7; }
.archive-settings .gs-section-title { font-size: 11pt; font-weight: 700; }
.archive-settings .gs-card { padding: 4px 12px; border-radius: 12px; background: alpha(@theme_fg_color, .04); }
.archive-settings .gs-row { min-height: 46px; padding-top: 6px; padding-bottom: 6px; }
.archive-settings .gs-row + .gs-row { border-top: 1px solid alpha(@theme_fg_color, .08); }
.archive-settings .gs-row-subtitle { opacity: .7; font-size: 9.5pt; }
.archive-window .info-panel { padding: 14px; }
.archive-window .info-panel:dir(ltr) { border-left: 1px solid alpha(@theme_fg_color, .1); }
.archive-window .info-panel:dir(rtl) { border-right: 1px solid alpha(@theme_fg_color, .1); }
.archive-window .info-name { font-weight: 700; font-size: 11pt; }
.archive-window .status-bar { padding: 4px 10px; border-top: 1px solid alpha(@theme_fg_color, .1); }
.archive-window .empty-state { padding: 48px; }
.archive-window .empty-title { font-size: 16pt; font-weight: 700; }
.archive-window .drop-highlight { background: alpha(@theme_selected_bg_color, .12); }
";

/// A modal dialog window: (window, body, button row).
pub fn dialog(parent: Option<&gtk::Window>, title: &str, width: i32) -> (gtk::Window, gtk::Box, gtk::Box) {
    let window = gtk::Window::builder().title(title).modal(parent.is_some()).resizable(true).default_width(width).build();
    window.set_transient_for(parent);
    window.set_destroy_with_parent(true);
    window.add_css_class("archive-dialog");
    window.set_titlebar(Some(&suite_gtk::utils::headerbar(title)));
    suite_gtk::theme::attach(&window, None);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("dialog-body");
    body.set_vexpand(true);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.add_css_class("dialog-buttons");
    buttons.set_halign(gtk::Align::End);
    root.append(&body);
    root.append(&buttons);
    window.set_child(Some(&root));
    let w = window.clone();
    let escape = gtk::EventControllerKey::new();
    escape.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(escape);
    (window, body, buttons)
}

pub fn button(row: &gtk::Box, label: &str, name: &str, style: Option<&str>) -> gtk::Button {
    let b = gtk::Button::with_mnemonic(label);
    b.set_widget_name(name);
    if let Some(style) = style {
        b.add_css_class(style);
    }
    row.append(&b);
    b
}

pub fn heading(body: &gtk::Box, text: &str) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar).build();
    l.add_css_class("dialog-heading");
    body.append(&l);
    l
}

pub fn text(body: &gtk::Box, text: &str) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar).selectable(true).build();
    body.append(&l);
    l
}

/// Run `f` once: the first of the buttons/close wins.
pub fn once<T: 'static>(f: impl FnOnce(T) + 'static) -> Rc<dyn Fn(T)> {
    let cell = RefCell::new(Some(f));
    Rc::new(move |value| {
        if let Some(f) = cell.borrow_mut().take() {
            f(value)
        }
    })
}

pub fn message(parent: Option<&gtk::Window>, title: &str, body_text: &str) -> gtk::Window {
    let (window, body, buttons) = dialog(parent, title, 440);
    window.set_widget_name("message-dialog");
    text(&body, body_text);
    let ok = button(&buttons, &tr!("Close"), "message-close", Some("suggested-action"));
    let w = window.clone();
    ok.connect_clicked(move |_| w.close());
    window.set_default_widget(Some(&ok));
    window.present();
    window
}

pub fn error(parent: Option<&gtk::Window>, body_text: &str) -> gtk::Window {
    message(parent, &tr!("Archive Manager"), body_text)
}

pub fn confirm(parent: Option<&gtk::Window>, title: &str, body_text: &str, accept: &str, destructive: bool,
               callback: impl FnOnce(bool) + 'static) -> gtk::Window {
    let (window, body, buttons) = dialog(parent, title, 440);
    window.set_widget_name("confirm-dialog");
    text(&body, body_text);
    let done = once(callback);
    let cancel = button(&buttons, &tr!("Cancel"), "confirm-cancel", None);
    let ok = button(&buttons, accept, "confirm-accept", Some(if destructive { "destructive-action" } else { "suggested-action" }));
    let (d, w) = (done.clone(), window.clone());
    cancel.connect_clicked(move |_| {
        d(false);
        w.close();
    });
    let (d, w) = (done.clone(), window.clone());
    ok.connect_clicked(move |_| {
        d(true);
        w.close();
    });
    let d = done.clone();
    window.connect_close_request(move |_| {
        d(false);
        glib::Propagation::Proceed
    });
    window.set_default_widget(Some(&ok));
    window.present();
    window
}

/// Ask for a name. Selects the part before the extension, like a file manager.
pub fn prompt(parent: Option<&gtk::Window>, title: &str, label: &str, initial: &str, accept: &str,
              callback: impl FnOnce(Option<String>) + 'static) -> gtk::Window {
    let (window, body, buttons) = dialog(parent, title, 420);
    window.set_widget_name("prompt-dialog");
    text(&body, label);
    let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
    entry.set_widget_name("prompt-entry");
    body.append(&entry);
    let done = once(callback);
    let cancel = button(&buttons, &tr!("Cancel"), "prompt-cancel", None);
    let ok = button(&buttons, accept, "prompt-accept", Some("suggested-action"));
    let update = {
        let ok = ok.clone();
        move |e: &gtk::Entry| {
            let t = e.text();
            let t = t.trim();
            ok.set_sensitive(!t.is_empty() && !t.contains('/') && t != "." && t != "..");
        }
    };
    update(&entry);
    entry.connect_changed(update);
    let (d, w) = (done.clone(), window.clone());
    cancel.connect_clicked(move |_| {
        d(None);
        w.close();
    });
    let (d, w, e) = (done.clone(), window.clone(), entry.clone());
    ok.connect_clicked(move |_| {
        d(Some(e.text().trim().to_string()));
        w.close();
    });
    let d = done.clone();
    window.connect_close_request(move |_| {
        d(None);
        glib::Propagation::Proceed
    });
    window.set_default_widget(Some(&ok));
    window.present();
    let stem = initial.rfind('.').filter(|i| *i > 0).unwrap_or(initial.len());
    entry.grab_focus();
    entry.select_region(0, initial[..stem].chars().count() as i32);
    window
}

/// Password prompt; `wrong` shows that the previous attempt failed.
pub fn password(parent: Option<&gtk::Window>, archive: &str, wrong: bool, callback: impl FnOnce(Option<String>) + 'static) -> gtk::Window {
    let (window, body, buttons) = dialog(parent, &tr!("Password Required"), 420);
    window.set_widget_name("password-dialog");
    heading(&body, &tr!("Enter the password for {name}", name = archive));
    if wrong {
        let l = text(&body, &tr!("The password is wrong. Try again."));
        l.add_css_class("error");
        l.set_widget_name("password-wrong");
    }
    let entry = gtk::PasswordEntry::builder().show_peek_icon(true).activates_default(true).build();
    entry.set_widget_name("password-entry");
    body.append(&entry);
    let done = once(callback);
    let cancel = button(&buttons, &tr!("Cancel"), "password-cancel", None);
    let ok = button(&buttons, &tr!("OK"), "password-accept", Some("suggested-action"));
    let (d, w) = (done.clone(), window.clone());
    cancel.connect_clicked(move |_| {
        d(None);
        w.close();
    });
    let (d, w, e) = (done.clone(), window.clone(), entry.clone());
    ok.connect_clicked(move |_| {
        d(Some(e.text().to_string()));
        w.close();
    });
    let d = done.clone();
    window.connect_close_request(move |_| {
        d(None);
        glib::Propagation::Proceed
    });
    window.set_default_widget(Some(&ok));
    window.present();
    entry.grab_focus();
    window
}

pub fn describe_file(path: &std::path::Path) -> String {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return String::new() };
    let size = if meta.is_dir() { tr!("Folder") } else { glib::format_size(meta.len()).to_string() };
    let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| crate::ui::format_time(d.as_secs() as i64)).unwrap_or_default();
    format!("{size} · {modified}")
}

/// Ark's overwrite dialog.
pub fn conflict(parent: Option<&gtk::Window>, conflict: &Conflict, callback: impl FnOnce(Resolution) + 'static) -> gtk::Window {
    let (window, body, buttons) = dialog(parent, &tr!("File Already Exists"), 520);
    window.set_widget_name("conflict-dialog");
    let name = conflict.target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    heading(&body, &tr!("{name} already exists", name = name.clone()));
    let grid = gtk::Grid::builder().column_spacing(12).row_spacing(6).build();
    let row = |r: i32, title: &str, path: &std::path::Path| {
        let t = gtk::Label::builder().label(title).xalign(0.0).build();
        t.add_css_class("dim-label");
        grid.attach(&t, 0, r, 1, 1);
        let v = gtk::Label::builder().label(describe_file(path)).xalign(0.0).hexpand(true).build();
        grid.attach(&v, 1, r, 1, 1);
    };
    row(0, &tr!("Existing file:"), &conflict.target);
    row(1, &tr!("File from the archive:"), &conflict.incoming);
    body.append(&grid);
    let location = conflict.target.parent().map(|p| p.display().to_string()).unwrap_or_default();
    let place = text(&body, &tr!("In {folder}", folder = location));
    place.add_css_class("dim-label");
    place.set_ellipsize(pango::EllipsizeMode::Middle);
    let rename_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let rename_label = gtk::Label::new(Some(&tr!("New name:")));
    let entry = gtk::Entry::builder().hexpand(true).build();
    entry.set_widget_name("conflict-name");
    let folder = conflict.target.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    entry.set_text(&free_name(&folder, &name));
    rename_row.append(&rename_label);
    rename_row.append(&entry);
    body.append(&rename_row);
    let done = once(callback);
    let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).max_children_per_line(6).min_children_per_line(3)
        .column_spacing(6).row_spacing(6).homogeneous(false).build();
    buttons.append(&flow);
    let add = |label: &str, name: &str, style: Option<&str>, result: Box<dyn Fn(&gtk::Entry) -> Resolution>| {
        let b = gtk::Button::with_mnemonic(label);
        b.set_widget_name(name);
        if let Some(style) = style {
            b.add_css_class(style);
        }
        flow.insert(&b, -1);
        let (d, w, e) = (done.clone(), window.clone(), entry.clone());
        b.connect_clicked(move |_| {
            d(result(&e));
            w.close();
        });
        b
    };
    add(&tr!("Cancel"), "conflict-cancel", None, Box::new(|_| Resolution::Cancel));
    add(&tr!("Skip All"), "conflict-skip-all", None, Box::new(|_| Resolution::SkipAll));
    add(&tr!("Skip"), "conflict-skip", None, Box::new(|_| Resolution::Skip));
    let rename = add(&tr!("Rename"), "conflict-rename", None, Box::new(|e| Resolution::Rename(e.text().to_string())));
    add(&tr!("Overwrite All"), "conflict-overwrite-all", Some("destructive-action"), Box::new(|_| Resolution::OverwriteAll));
    let overwrite = add(&tr!("Overwrite"), "conflict-overwrite", Some("destructive-action"), Box::new(|_| Resolution::Overwrite));
    let (r, original) = (rename.clone(), name.clone());
    let check = move |e: &gtk::Entry| {
        let t = e.text();
        r.set_sensitive(!t.trim().is_empty() && t.as_str() != original && !t.contains('/'));
    };
    check(&entry);
    entry.connect_changed(check);
    let d = done.clone();
    window.connect_close_request(move |_| {
        d(Resolution::Cancel);
        glib::Propagation::Proceed
    });
    window.present();
    overwrite.grab_focus();
    window
}

pub fn comment(parent: Option<&gtk::Window>, initial: &str, callback: impl FnOnce(Option<String>) + 'static) -> gtk::Window {
    let (window, body, buttons) = dialog(parent, &tr!("Edit Comment"), 480);
    window.set_widget_name("comment-dialog");
    window.set_default_height(320);
    let view = gtk::TextView::builder().wrap_mode(gtk::WrapMode::WordChar).vexpand(true).build();
    view.set_widget_name("comment-text");
    view.buffer().set_text(initial);
    let scroll = gtk::ScrolledWindow::builder().child(&view).vexpand(true).min_content_height(160).build();
    scroll.add_css_class("frame");
    body.append(&scroll);
    let done = once(callback);
    let cancel = button(&buttons, &tr!("Cancel"), "comment-cancel", None);
    let ok = button(&buttons, &tr!("Save"), "comment-save", Some("suggested-action"));
    let (d, w) = (done.clone(), window.clone());
    cancel.connect_clicked(move |_| {
        d(None);
        w.close();
    });
    let (d, w, v) = (done.clone(), window.clone(), view.clone());
    ok.connect_clicked(move |_| {
        let b = v.buffer();
        d(Some(b.text(&b.start_iter(), &b.end_iter(), false).to_string()));
        w.close();
    });
    let d = done.clone();
    window.connect_close_request(move |_| {
        d(None);
        glib::Propagation::Proceed
    });
    window.present();
    view.grab_focus();
    window
}
