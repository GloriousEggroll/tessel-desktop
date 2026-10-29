//! Row widgets of the virtualized views (Python sets `box._picture`,
//! `box._label`, `box._bound_uri`, `box._cancel_thumbnail` and
//! `widget._files_controllers` on plain Gtk.Box/Label objects).

use crate::thumbnails::Cancel;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileTile {
        pub picture: RefCell<Option<gtk::Image>>,
        pub label: RefCell<Option<gtk::Label>>,
        pub bound_uri: RefCell<Option<String>>,
        pub cancel: RefCell<Option<Cancel>>,
        pub controllers: RefCell<Vec<gtk::EventController>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileTile {
        const NAME: &'static str = "WayfilesFileTile";
        type Type = super::FileTile;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for FileTile {}
    impl WidgetImpl for FileTile {}
    impl BoxImpl for FileTile {}
}

glib::wrapper! {
    /// A tile/row box (`files-tile`) or a plain text cell.
    pub struct FileTile(ObjectSubclass<imp::FileTile>) @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl FileTile {
    pub fn new(orientation: gtk::Orientation, spacing: i32) -> Self {
        glib::Object::builder().property("orientation", orientation).property("spacing", spacing).build()
    }

    pub fn set_parts(&self, picture: Option<gtk::Image>, label: gtk::Label) {
        *self.imp().picture.borrow_mut() = picture;
        *self.imp().label.borrow_mut() = Some(label);
    }
    pub fn picture(&self) -> Option<gtk::Image> {
        self.imp().picture.borrow().clone()
    }
    pub fn label(&self) -> Option<gtk::Label> {
        self.imp().label.borrow().clone()
    }
    pub fn bound_uri(&self) -> Option<String> {
        self.imp().bound_uri.borrow().clone()
    }
    pub fn set_bound_uri(&self, uri: Option<String>) {
        *self.imp().bound_uri.borrow_mut() = uri;
    }
    pub fn cancel_thumbnail(&self) {
        if let Some(cancel) = self.imp().cancel.borrow_mut().take() {
            cancel.cancel();
        }
    }
    pub fn set_cancel(&self, cancel: Option<Cancel>) {
        *self.imp().cancel.borrow_mut() = cancel;
    }
}

const CONTROLLERS: &str = "wayfiles-controllers";

/// `widget._files_controllers = [...]` for any widget (tiles, text cells, views).
pub fn set_controllers(widget: &gtk::Widget, controllers: Vec<gtk::EventController>) {
    if let Some(tile) = widget.downcast_ref::<FileTile>() {
        *tile.imp().controllers.borrow_mut() = controllers;
    } else {
        unsafe { widget.set_data(CONTROLLERS, controllers) };
    }
}

/// `BrowserPane._remove_controllers`: detach the gestures (they close over the row).
pub fn remove_controllers(widget: &gtk::Widget) {
    let controllers: Vec<gtk::EventController> = match widget.downcast_ref::<FileTile>() {
        Some(tile) => std::mem::take(&mut *tile.imp().controllers.borrow_mut()),
        None => unsafe { widget.steal_data::<Vec<gtk::EventController>>(CONTROLLERS) }.unwrap_or_default(),
    };
    for controller in controllers {
        widget.remove_controller(&controller);
    }
}

/// The Python `_bound_uri` lookup tests use: the tile bound to `uri` below `root`.
pub fn tile_for_uri(root: &gtk::Widget, uri: &str) -> Option<FileTile> {
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(tile) = widget.downcast_ref::<FileTile>() {
            if tile.bound_uri().as_deref() == Some(uri) {
                return Some(tile.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            stack.push(current);
        }
    }
    None
}

/// Every widget below (and including) `root`, depth first (test helper `descendants`).
pub fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(current) = child {
        result.extend(descendants(&current));
        child = current.next_sibling();
    }
    result
}

const LOCATION: &str = "wayfiles-location-uri";

/// `button._location_uri`.
pub fn set_location_uri(widget: &impl IsA<gtk::Widget>, uri: Option<String>) {
    unsafe { widget.as_ref().set_data(LOCATION, uri) };
}

pub fn location_uri(widget: &impl IsA<gtk::Widget>) -> Option<String> {
    unsafe { widget.as_ref().data::<Option<String>>(LOCATION).and_then(|p| p.as_ref().clone()) }
}
