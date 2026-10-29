//! Port of picture_dialogs.py: small themed dialogs shared by viewing and editing tools.

use crate::canvas::{PictureCanvas, Zoom};
use crate::compat::Pool;
use crate::model::load_pixels;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use suite_core::tr;
use suite_gtk::utils::{accessible, headerbar, set_margins};

/// `dialog(parent, title, width=440)`: modal transient window with the suite header bar and palette.
pub fn dialog(parent: &impl IsA<gtk::Window>, title: &str, width: i32) -> (gtk::Window, gtk::Box) {
    let window = gtk::Window::builder().title(title).transient_for(parent).modal(true).default_width(width).build();
    window.set_titlebar(Some(&headerbar(title)));
    let theme = suite_gtk::theme::attach(&window, None);
    // The palette client only holds bindings weakly: the window keeps its own.
    unsafe { window.set_data("pictures-theme", theme) };
    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    set_margins(&content, 20);
    window.set_child(Some(&content));
    (window, content)
}

/// `ask(parent, title, message, choices, callback)`.
pub fn ask(parent: &impl IsA<gtk::Window>, title: &str, message: &str, choices: &[(String, &'static str)],
           callback: impl Fn(&str) + 'static) -> gtk::Window {
    let (window, content) = dialog(parent, title, 440);
    content.append(&gtk::Label::builder().label(message).xalign(0.0).wrap(true).build());
    let row = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
    content.append(&row);
    let callback = Rc::new(callback);
    for (label, value) in choices {
        let button = gtk::Button::with_label(label);
        let (window, callback, value) = (window.clone(), callback.clone(), *value);
        button.connect_clicked(move |_| {
            window.destroy();
            callback(value);
        });
        row.append(&button);
    }
    window.present();
    window
}

/// `form(parent, title, rows, accept, callback)`: the callback's `Err` text is shown and keeps the form open.
pub fn form(parent: &impl IsA<gtk::Window>, title: &str, rows: Vec<(String, gtk::Widget)>, accept: &str,
            callback: impl Fn() -> Result<(), String> + 'static) -> gtk::Window {
    let (window, content) = dialog(parent, title, 440);
    for (label, widget) in rows {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Label::builder().label(label).xalign(0.0).hexpand(true).build());
        row.append(&widget);
        content.append(&row);
    }
    let error = gtk::Label::builder().xalign(0.0).wrap(true).build();
    content.append(&error);
    let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
    let cancel = gtk::Button::with_label(&tr!("Cancel"));
    let apply = gtk::Button::with_label(accept);
    apply.add_css_class("suggested-action");
    let close = window.clone();
    cancel.connect_clicked(move |_| close.destroy());
    let close = window.clone();
    apply.connect_clicked(move |_| match callback() {
        Ok(()) => close.destroy(),
        Err(message) => error.set_text(&message),
    });
    buttons.append(&cancel);
    buttons.append(&apply);
    content.append(&buttons);
    window.present();
    window
}

/// `number(value, lower, upper, step=1)`.
pub fn number(value: f64, lower: f64, upper: f64, step: f64) -> gtk::SpinButton {
    let widget = gtk::SpinButton::with_range(lower, upper, step);
    widget.set_value(value);
    widget
}

/// `open_with(parent, path)`: searchable application chooser, A–Z / Z–A, optional default.
pub fn open_with(parent: &impl IsA<gtk::Window>, path: &Path) -> gtk::Window {
    let file = gio::File::for_path(path);
    let (content_type, _) = gio::content_type_guess(Some(path), None::<&[u8]>);
    let (window, content) = dialog(parent, &tr!("Open With"), 520);
    window.set_default_size(520, 600);
    let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search applications")).build();
    accessible(&search, &tr!("Search applications"));
    content.append(&search);
    let order = gtk::DropDown::from_strings(&["A–Z", "Z–A"]);
    accessible(&order, &tr!("Sort order"));
    content.append(&order);
    let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
    let listing = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
    scroll.set_child(Some(&listing));
    content.append(&scroll);
    let default = gtk::CheckButton::with_label(&tr!("Use this application by default"));
    content.append(&default);
    let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
    content.append(&status);
    let applications: Vec<gio::AppInfo> = gio::AppInfo::all().into_iter()
        .filter(|app| app.should_show() && app.id().as_deref() != Some("org.waydock.Pictures.desktop")).collect();
    let shown: Rc<RefCell<Vec<gio::AppInfo>>> = Rc::default();
    let refresh = {
        let (listing, search, order, shown) = (listing.clone(), search.clone(), order.clone(), shown.clone());
        Rc::new(move || {
            while let Some(child) = listing.first_child() {
                listing.remove(&child);
            }
            let query = suite_core::pyjson::casefold(&search.text());
            let mut sorted = applications.clone();
            sorted.sort_by_key(|a| suite_core::pyjson::casefold(&a.display_name()));
            if order.selected() == 1 {
                sorted.reverse();
            }
            let mut rows = Vec::new();
            for app in sorted {
                let haystack = format!("{} {}", app.display_name(), app.description().map(|d| d.to_string()).unwrap_or_default());
                if !suite_core::pyjson::casefold(&haystack).contains(&query) {
                    continue;
                }
                let row = gtk::ListBoxRow::new();
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                set_margins(&line, 8);
                let icon = app.icon().unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast());
                let image = gtk::Image::from_gicon(&icon);
                image.set_pixel_size(24);
                line.append(&image);
                line.append(&gtk::Label::builder().label(app.display_name()).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build());
                row.set_child(Some(&line));
                listing.append(&row);
                rows.push(app);
            }
            shown.replace(rows);
        })
    };
    let launch = {
        let (listing, shown, window, status, default) = (listing.clone(), shown.clone(), window.clone(), status.clone(), default.clone());
        let parent: gtk::Window = parent.clone().upcast();
        Rc::new(move || {
            let Some(row) = listing.selected_row() else { return };
            let Some(application) = shown.borrow().get(row.index() as usize).cloned() else { return };
            let context = WidgetExt::display(&parent).app_launch_context();
            let result = application.launch(&[file.clone()], Some(&context)).and_then(|_| {
                if default.is_active() { application.set_as_default_for_type(&content_type) } else { Ok(()) }
            });
            match result {
                Ok(()) => window.destroy(),
                Err(error) => status.set_text(error.message()),
            }
        })
    };
    let button = gtk::Button::with_label(&tr!("Open"));
    button.add_css_class("suggested-action");
    let action = launch.clone();
    button.connect_clicked(move |_| action());
    content.append(&button);
    let action = refresh.clone();
    search.connect_search_changed(move |_| action());
    let action = refresh.clone();
    order.connect_selected_notify(move |_| action());
    listing.connect_row_activated(move |_, _| launch());
    refresh();
    window.present();
    search.grab_focus();
    window
}

/// `compare(parent, paths)`: two to four pictures with independent or synchronized zoom.
pub fn compare(parent: &impl IsA<gtk::Window>, paths: &[PathBuf]) -> gtk::Window {
    let (window, content) = dialog(parent, &tr!("Compare Pictures"), 1150);
    window.set_modal(false);
    window.set_default_size(1150, 650);
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&tools);
    let sync = gtk::CheckButton::builder().label(tr!("Synchronize zoom")).active(true).build();
    tools.append(&sync);
    let panes = gtk::Box::builder().spacing(8).vexpand(true).homogeneous(true).build();
    content.append(&panes);
    let canvases: Rc<RefCell<Vec<PictureCanvas>>> = Rc::default();
    let active: Rc<RefCell<Option<PictureCanvas>>> = Rc::default();
    let pool = Pool::new("picture-compare", 2);
    let closed = Rc::new(Cell::new(false));
    for (label, value, name) in [(tr!("Fit"), None, tr!("Fit")), ("100%".to_string(), Some(0.0), "100%".to_string()),
                                 ("−".to_string(), Some(1.0 / 1.25), tr!("Zoom out")), ("+".to_string(), Some(1.25), tr!("Zoom in"))] {
        let button = gtk::Button::builder().label(&label).tooltip_text(&name).build();
        accessible(&button, &name);
        let (canvases, active, sync) = (canvases.clone(), active.clone(), sync.clone());
        button.connect_clicked(move |_| {
            let targets: Vec<PictureCanvas> = if sync.is_active() { canvases.borrow().clone() }
                                              else { active.borrow().iter().cloned().collect() };
            for canvas in targets {
                canvas.set_zoom(match value {
                    None => Zoom::Mode("fit"),
                    Some(v) if v == 0.0 => Zoom::Mode("actual"),
                    Some(factor) => Zoom::Value(canvas.scale() * factor),
                });
            }
        });
        tools.append(&button);
    }
    for path in paths.iter().take(4) {
        let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).hexpand(true).build();
        let title = gtk::Label::builder().label(path.file_name().unwrap_or_default().to_string_lossy())
            .ellipsize(gtk::pango::EllipsizeMode::End).build();
        column.append(&title);
        let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        let canvas = PictureCanvas::new(&scroll);
        scroll.set_child(Some(&canvas.area));
        column.append(&scroll);
        panes.append(&column);
        canvases.borrow_mut().push(canvas.clone());
        if active.borrow().is_none() {
            active.replace(Some(canvas.clone()));
        }
        let click = gtk::GestureClick::new();
        let (slot, target) = (active.clone(), canvas.clone());
        click.connect_pressed(move |_, _, _, _| { slot.replace(Some(target.clone())); });
        canvas.area.add_controller(click);
        let source = path.clone();
        let (_, receiver) = pool.submit(move || load_pixels(&source, None, 0).map(|l| l.image).map_err(|e| e.to_string()));
        let closed = closed.clone();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.await else { return };
            if closed.get() {
                return;
            }
            match result {
                Ok(pixels) => canvas.set_image(&pixels),
                Err(error) => title.set_text(&error),
            }
        });
    }
    window.connect_close_request(move |_| {
        closed.set(true);
        for canvas in canvases.borrow().iter() {
            canvas.clear();
        }
        pool.shutdown();
        glib::Propagation::Proceed
    });
    window.present();
    window
}

/// `conflict(parent, name, folder, remaining, callback(value, every))`: Cancel / Skip / Keep Both / Replace.
pub fn conflict(parent: &impl IsA<gtk::Window>, name: &str, folder: &str, remaining: usize,
                callback: impl Fn(&str, bool) + 'static) -> gtk::Window {
    let (window, content) = dialog(parent, &tr!("File Already Exists"), 480);
    content.append(&gtk::Label::builder().label(tr!("“{name}” already exists in {folder}.", name = name, folder = folder))
        .xalign(0.0).wrap(true).build());
    let every = gtk::CheckButton::with_label(&tr!("Apply to all {count} remaining conflicts", count = remaining as i64));
    every.set_visible(remaining > 1);
    content.append(&every);
    let row = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
    content.append(&row);
    let answered = Rc::new(Cell::new(false));
    let callback = Rc::new(callback);
    for (label, value) in [(tr!("Cancel"), "cancel"), (tr!("Skip"), "skip"), (tr!("Keep Both"), "keep"), (tr!("Replace"), "replace")] {
        let button = gtk::Button::with_label(&label);
        if value == "replace" {
            button.add_css_class("destructive-action");
        }
        let (window, every, answered, callback) = (window.clone(), every.clone(), answered.clone(), callback.clone());
        button.connect_clicked(move |_| {
            answered.set(true);
            window.destroy();
            callback(value, every.is_active());
        });
        row.append(&button);
    }
    window.connect_close_request(move |_| {
        if !answered.get() {
            answered.set(true);
            callback("cancel", false);
        }
        glib::Propagation::Proceed
    });
    window.present();
    window
}

/// `results(parent, title, rows)`: per-file outcome list for a batch file operation.
pub fn results(parent: &impl IsA<gtk::Window>, title: &str, rows: &[(String, String)]) -> gtk::Window {
    let (window, content) = dialog(parent, title, 560);
    window.set_default_size(560, 420);
    let scroll = gtk::ScrolledWindow::builder().vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
    let listing = gtk::Box::new(gtk::Orientation::Vertical, 6);
    scroll.set_child(Some(&listing));
    content.append(&scroll);
    for (name, outcome) in rows {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.append(&gtk::Label::builder().label(name).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End)
            .selectable(true).build());
        line.append(&gtk::Label::builder().label(outcome).xalign(1.0).wrap(true).max_width_chars(36).build());
        listing.append(&line);
    }
    let close = gtk::Button::builder().label(tr!("Close")).halign(gtk::Align::End).build();
    let target = window.clone();
    close.connect_clicked(move |_| target.destroy());
    content.append(&close);
    window.present();
    window
}
