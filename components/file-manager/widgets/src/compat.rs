//! What the widgets need from a listed file, plus small local helpers.
//!
//! [`PreviewItem`] is the subset of `file_model.FileItem` the previews, printing,
//! Quick Look and the information pane read. It is implemented for
//! `files_model::model::FileItem` (see `item_impl` in lib.rs) and for the
//! standalone [`SimpleItem`] (tests, and any caller holding only a `Gio.File` +
//! `Gio.FileInfo`). Callers hold items as [`Item`] = `Rc<dyn PreviewItem>`;
//! [`PreviewItem::as_any`] lets a pane get its own type back
//! (`item.as_any().downcast_ref::<FileItem>()`).

use gio::prelude::*;
use std::any::Any;
use std::rc::Rc;

pub trait PreviewItem: Any {
    fn file(&self) -> gio::File;
    fn info(&self) -> gio::FileInfo;
    /// `standard::name` (byte string) or the basename.
    fn name(&self) -> String;
    fn display_name(&self) -> String;
    fn uri(&self) -> String;
    fn is_dir(&self) -> bool;
    /// Never empty: `application/octet-stream` when unknown.
    fn content_type(&self) -> String;
    fn icon(&self) -> gio::Icon;
    fn size(&self) -> u64;
    /// `time::modified` seconds.
    fn modified(&self) -> u64;
    /// "Folder" or `Gio.content_type_get_description`.
    fn type_description(&self) -> String {
        if self.is_dir() { "Folder".into() } else { gio::content_type_get_description(&self.content_type()).to_string() }
    }
    fn as_any(&self) -> &dyn Any;
}

pub type Item = Rc<dyn PreviewItem>;

/// The attributes `file_model.FILE_ATTRIBUTES` asks for that the widgets use.
pub const SIMPLE_ATTRIBUTES: &str = "standard::*,time::modified,time::modified-usec,time::modified-nsec";

/// `FileItem(file, info)` field rules for callers without a files-model item.
#[derive(Clone, Debug)]
pub struct SimpleItem {
    pub file: gio::File,
    pub info: gio::FileInfo,
}

impl SimpleItem {
    pub fn new(file: gio::File, info: gio::FileInfo) -> Self {
        Self { file, info }
    }

    /// Synchronous `FileItem.query` (worker threads, small local fixtures).
    pub fn query(file: &gio::File) -> Result<Self, glib::Error> {
        let info = file.query_info(SIMPLE_ATTRIBUTES, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)?;
        Ok(Self::new(file.clone(), info))
    }

    pub fn for_path(path: impl AsRef<std::path::Path>) -> Result<Item, glib::Error> {
        Ok(Rc::new(Self::query(&gio::File::for_path(path))?))
    }
}

fn byte_string(info: &gio::FileInfo, attribute: &str) -> Option<String> {
    info.attribute_byte_string(attribute).map(|s| s.to_string()).filter(|s| !s.is_empty())
}

impl PreviewItem for SimpleItem {
    fn file(&self) -> gio::File {
        self.file.clone()
    }
    fn info(&self) -> gio::FileInfo {
        self.info.clone()
    }
    fn name(&self) -> String {
        byte_string(&self.info, "standard::name")
            .or_else(|| self.file.basename().map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }
    fn display_name(&self) -> String {
        self.info.attribute_string("standard::display-name").map(|s| s.to_string()).filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.name())
    }
    fn uri(&self) -> String {
        self.file.uri().to_string()
    }
    fn is_dir(&self) -> bool {
        self.info.attribute_uint32("standard::type") == gio::FileType::Directory.into_glib() as u32
    }
    fn content_type(&self) -> String {
        self.info.attribute_string("standard::content-type").map(|s| s.to_string()).filter(|s| !s.is_empty())
            .unwrap_or_else(|| "application/octet-stream".into())
    }
    fn icon(&self) -> gio::Icon {
        self.info.attribute_object("standard::icon").and_then(|o| o.downcast::<gio::Icon>().ok())
            .unwrap_or_else(|| gio::ThemedIcon::new(if self.is_dir() { "folder" } else { "text-x-generic" }).upcast())
    }
    fn size(&self) -> u64 {
        self.info.attribute_uint64("standard::size")
    }
    fn modified(&self) -> u64 {
        self.info.attribute_uint64("time::modified")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

use glib::translate::IntoGlib;

/// Python `f"{value:,}"` for integers.
pub fn thousands(value: impl Into<i128>) -> String {
    let value = value.into();
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 { format!("-{out}") } else { out }
}

/// Python `f"{value:g}"`.
pub fn general(value: f64) -> String {
    suite_core::pyfmt::format_value(&suite_core::pyfmt::Arg::Float(value), "g").unwrap_or_else(|_| value.to_string())
}

/// Python `f"{value:.0f}"` (round-half-even on the exact binary value).
pub fn fixed0(value: f64) -> String {
    suite_core::pyfmt::format_value(&suite_core::pyfmt::Arg::Float(value), ".0f").unwrap_or_else(|_| format!("{value:.0}"))
}

/// Python `str.strip()` + `[:limit]` on characters.
pub fn clip(value: &str, limit: usize) -> String {
    suite_core::pyjson::py_strip(&value.replace('\0', "")).chars().take(limit).collect()
}

// ---- file_model formatting comes from files_model --------------------------
pub use files_model::model::{format_modified, format_size, item_count};

// ---- PreviewItem for the model's FileItem -------------------------------------

impl PreviewItem for files_model::model::FileItem {
    fn file(&self) -> gio::File { files_model::model::FileItem::file(self) }
    fn info(&self) -> gio::FileInfo { files_model::model::FileItem::info(self) }
    fn name(&self) -> String { files_model::model::FileItem::name(self) }
    fn display_name(&self) -> String { files_model::model::FileItem::display_name(self) }
    fn uri(&self) -> String { files_model::model::FileItem::uri(self) }
    fn is_dir(&self) -> bool { files_model::model::FileItem::is_dir(self) }
    fn content_type(&self) -> String { files_model::model::FileItem::content_type(self) }
    fn icon(&self) -> gio::Icon { files_model::model::FileItem::icon(self) }
    fn size(&self) -> u64 { files_model::model::FileItem::size(self) }
    fn modified(&self) -> u64 { files_model::model::FileItem::modified(self) }
    fn type_description(&self) -> String { files_model::model::FileItem::type_description(self) }
    fn as_any(&self) -> &dyn Any { self }
}

/// `Rc<dyn PreviewItem>` for a model item.
pub fn item(file_item: &files_model::model::FileItem) -> Item {
    Rc::new(file_item.clone())
}

/// `files_devices.capacity(info)` (from files_model::devices).
pub use files_model::devices::capacity;

/// Python's `headerbar()` goes through `add_css`, which also installs the suite
/// search-field text margins once per display; suite_gtk's `headerbar` uses
/// `css_once` and skips them. Same key as `suite_gtk::utils::add_css`, so it is
/// never installed twice. (Candidate to upstream into suite_gtk::utils::headerbar.)
pub fn ensure_search_css(display: &gtk::gdk::Display) {
    suite_gtk::utils::css_once(display, "suite-search", "searchentry > text, entry.search > text { margin-left: 8px; margin-right: 8px; }",
                               suite_gtk::utils::PRIORITY_APPLICATION + 40);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_bounds() {
        let info = gio::FileInfo::new();
        assert_eq!(capacity(&info), None);
        info.set_attribute_uint64("filesystem::size", 0);
        info.set_attribute_uint64("filesystem::free", 0);
        assert_eq!(capacity(&info), None);
        info.set_attribute_uint64("filesystem::size", 1000);
        assert_eq!(capacity(&info), Some((1000, 0, 1000)));
        info.set_attribute_uint64("filesystem::free", 100);
        info.set_attribute_uint64("filesystem::used", 850);
        assert_eq!(capacity(&info), Some((1000, 100, 850)));
        info.set_attribute_uint64("filesystem::free", 1200);
        info.set_attribute_uint64("filesystem::used", 1200);
        assert_eq!(capacity(&info), Some((1000, 1000, 1000)));
    }

    #[test]
    fn python_number_formats() {
        assert_eq!(thousands(2400), "2,400");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(general(2.8), "2.8");
        assert_eq!(general(50.0), "50");
        assert_eq!(fixed0(209.90277777), "210");
        assert_eq!(clip("  a\0b  ", 512), "ab");
    }
}
