//! GTK user interface.

pub mod app;
pub mod dialogs;
pub mod dnd;
pub mod forms;
pub mod model;
pub mod preview;
pub mod progress;
pub mod window;

use gtk::{gio, glib};
use suite_core::tr;

pub fn format_time(seconds: i64) -> String {
    glib::DateTime::from_unix_local(seconds).ok().and_then(|d| d.format("%x %X").ok()).map(|s| s.to_string()).unwrap_or_default()
}

pub fn format_size(bytes: u64) -> String {
    glib::format_size(bytes).to_string()
}

pub fn permissions(mode: u32, is_dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir { 'd' } else { '-' });
    for (bit, c) in [(0o400, 'r'), (0o200, 'w'), (0o100, 'x'), (0o040, 'r'), (0o020, 'w'), (0o010, 'x'), (0o004, 'r'), (0o002, 'w'), (0o001, 'x')] {
        s.push(if mode & bit != 0 { c } else { '-' });
    }
    s
}

pub fn content_type(entry: &crate::archive::Entry) -> String {
    if entry.is_dir {
        return "inode/directory".into();
    }
    if entry.link.is_some() {
        return "inode/symlink".into();
    }
    gio::content_type_guess(Some(entry.name()), None::<&[u8]>).0.to_string()
}

pub fn type_description(entry: &crate::archive::Entry) -> String {
    if entry.is_dir {
        return tr!("Folder");
    }
    gio::content_type_get_description(&content_type(entry)).to_string()
}

pub fn icon_for(entry: &crate::archive::Entry) -> gio::Icon {
    if entry.is_dir {
        return gio::ThemedIcon::from_names(&["folder", "folder-symbolic"]).upcast();
    }
    gio::content_type_get_icon(&content_type(entry))
}

use gtk::prelude::*;

#[cfg(test)]
mod tests {
    #[test]
    fn permission_strings() {
        assert_eq!(super::permissions(0o755, true), "drwxr-xr-x");
        assert_eq!(super::permissions(0o640, false), "-rw-r-----");
    }
}
