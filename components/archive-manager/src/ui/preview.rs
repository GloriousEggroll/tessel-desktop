//! Ark's internal previewer: text and pictures extracted to a private
//! folder; anything else opens in its application. Files opened from the
//! archive are watched so a saved change can be put back into the archive.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::Path;
use suite_core::tr;

pub enum Kind {
    Text(String),
    Image,
    Other,
}

pub fn classify(path: &Path) -> Kind {
    use std::io::Read;
    let mut head = vec![0u8; 8192];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    head.truncate(n);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mime, _) = gio::content_type_guess(Some(name.as_str()), &head[..]);
    if gio::content_type_is_a(&mime, "image/png") || gio::content_type_is_a(&mime, "image/jpeg") || gio::content_type_is_a(&mime, "image/gif")
        || gio::content_type_is_a(&mime, "image/webp") || gio::content_type_is_a(&mime, "image/bmp") || gio::content_type_is_a(&mime, "image/svg+xml")
        || gio::content_type_is_a(&mime, "image/x-icon") || gio::content_type_is_a(&mime, "image/tiff") {
        return Kind::Image;
    }
    let texty = gio::content_type_is_a(&mime, "text/plain") || (!head.contains(&0) && std::str::from_utf8(&head).is_ok() && n > 0)
        || gio::content_type_is_a(&mime, "application/json") || gio::content_type_is_a(&mime, "application/xml");
    if texty {
        let bytes = std::fs::read(path).unwrap_or_default();
        return Kind::Text(String::from_utf8_lossy(&bytes).into_owned());
    }
    Kind::Other
}

/// Show `path` (named `title`) in a preview window; None if it is not text or a picture.
pub fn show(parent: Option<&gtk::Window>, path: &Path, title: &str) -> Option<gtk::Window> {
    let child: gtk::Widget = match classify(path) {
        Kind::Text(text) => {
            let view = gtk::TextView::builder().editable(false).monospace(true).wrap_mode(gtk::WrapMode::WordChar)
                .left_margin(10).right_margin(10).top_margin(8).bottom_margin(8).build();
            view.set_widget_name("preview-text");
            view.buffer().set_text(&text);
            gtk::ScrolledWindow::builder().child(&view).hexpand(true).vexpand(true).build().upcast()
        }
        Kind::Image => {
            let texture = gdk::Texture::from_filename(path).ok()?;
            let picture = gtk::Picture::for_paintable(&texture);
            picture.set_widget_name("preview-image");
            picture.set_can_shrink(true);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.upcast()
        }
        Kind::Other => return None,
    };
    let window = gtk::Window::builder().title(tr!("Preview of {name}", name = title)).default_width(760).default_height(560).build();
    window.set_widget_name("preview-window");
    window.set_transient_for(parent);
    window.set_titlebar(Some(&suite_gtk::utils::headerbar(&tr!("Preview of {name}", name = title))));
    suite_gtk::theme::attach(&window, None);
    window.set_child(Some(&child));
    let w = window.clone();
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window.present();
    Some(window)
}

/// Call `changed` when `path` is saved (written and closed, or replaced).
pub fn watch(path: &Path, changed: impl Fn() + 'static) -> Option<gio::FileMonitor> {
    let monitor = gio::File::for_path(path).monitor_file(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>).ok()?;
    // Editors save in several steps: report once the burst of events is over.
    let flag = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let f = flag.clone();
    let changed = std::rc::Rc::new(changed);
    monitor.connect_changed(move |_, _, _, event| {
        if matches!(event, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::MovedIn
            | gio::FileMonitorEvent::Renamed) {
            let generation = f.get() + 1;
            f.set(generation);
            let (f, changed) = (f.clone(), changed.clone());
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                if f.get() == generation {
                    changed();
                }
            });
        }
    });
    Some(monitor)
}
