//! Port of file_quicklook.py: Space-bar quick preview, a large preview of the
//! selection that steps through the folder.

use crate::compat::Item;
use crate::information::{FilePreview, TEXT};
use gtk::prelude::*;
use gtk::{gdk, pango};
use std::rc::Rc;

/// What Quick Look needs from the browsing pane (Python passes the pane itself).
pub trait QuickLookPane {
    /// `pane.closed`.
    fn closed(&self) -> bool;
    /// `pane.move_selection(delta)`: select the next/previous item and return it.
    fn move_selection(&self, delta: i32) -> Option<Item>;
    /// `pane.activate(item)`: open the item as a double click would.
    fn activate(&self, item: &Item);
}

pub struct QuickLookInner {
    pub window: gtk::Window,
    pub heading: gtk::Label,
    pub print_button: gtk::Button,
    pub open_button: gtk::Button,
    pub preview: FilePreview,
    pane: Rc<dyn QuickLookPane>,
    print_file: Rc<dyn Fn(Item)>,
}

/// The `.files-quicklook` window (non-modal, transient for the Files window).
#[derive(Clone)]
pub struct QuickLook(Rc<QuickLookInner>);

impl std::ops::Deref for QuickLook {
    type Target = QuickLookInner;
    fn deref(&self) -> &QuickLookInner {
        &self.0
    }
}

impl QuickLook {
    /// `QuickLook(parent, pane, print_file)`.
    pub fn new(parent: &impl IsA<gtk::Window>, pane: Rc<dyn QuickLookPane>, print_file: Rc<dyn Fn(Item)>) -> Self {
        let parent = parent.upcast_ref::<gtk::Window>();
        let (width, height) = (parent.width(), parent.height());
        let window = gtk::Window::builder().transient_for(parent).modal(false).title("Quick Preview")
            .destroy_with_parent(true).default_width(640.max((width as f64 * 0.8) as i32))
            .default_height(480.max((height as f64 * 0.85) as i32)).build();
        window.add_css_class("files-quicklook");
        let header = gtk::HeaderBar::new();
        let heading = gtk::Label::builder().ellipsize(pango::EllipsizeMode::End).build();
        heading.add_css_class("title");
        header.set_title_widget(Some(&heading));
        let print_button = gtk::Button::builder().child(&tessel_gtk::utils::image("printer-symbolic", 16))
            .tooltip_text("Print…").visible(false).build();
        header.pack_end(&print_button);
        let open_button = gtk::Button::with_label("Open");
        header.pack_end(&open_button);
        window.set_titlebar(Some(&header));
        let preview = FilePreview::new(true);
        window.set_child(Some(preview.widget()));
        let look = Self(Rc::new(QuickLookInner { window, heading, print_button, open_button, preview, pane, print_file }));
        let weak = Rc::downgrade(&look.0);
        look.print_button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                if let Some(item) = inner.preview.item() {
                    (inner.print_file)(item);
                }
            }
        });
        let weak = Rc::downgrade(&look.0);
        look.open_button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                QuickLook(inner).open_item();
            }
        });
        let weak = Rc::downgrade(&look.0);
        look.preview.set_on_loaded(Some(Rc::new(move |_details: &crate::information::Details, _error: &str| {
            if let Some(inner) = weak.upgrade() {
                inner.print_button.set_visible(inner.preview.printable());
            }
        })));
        let weak = Rc::downgrade(&look.0);
        tessel_gtk::utils::key_controller(&look.window, true, move |_, key, _, state| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            if QuickLook(inner).key_pressed(key, state) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        // The window keeps the handle alive until it closes; closing stops playback and
        // releases decoded pages immediately, not at drop time.
        let keep = look.clone();
        let holder = std::cell::RefCell::new(Some(keep));
        look.window.connect_close_request(move |_| {
            let look = holder.borrow_mut().take();
            if let Some(look) = look {
                look.preview.close();
            }
            glib::Propagation::Proceed
        });
        look
    }

    pub fn window(&self) -> &gtk::Window {
        &self.0.window
    }

    pub fn present(&self) {
        self.0.window.present();
    }

    pub fn close(&self) {
        self.0.window.close();
    }

    pub fn show_item(&self, item: &Item) {
        self.heading.set_text(&item.display_name());
        self.print_button.set_visible(false);
        self.preview.show(Some(item.clone()), Some("No preview for this file type"));
    }

    pub fn open_item(&self) {
        if let Some(item) = self.preview.item() {
            if !self.pane.closed() {
                self.close();
                self.pane.activate(&item);
            }
        }
    }

    /// `_key_pressed`: Space/Escape close, Return opens, Left/Right (and Up/Down
    /// unless a text preview scrolls) step through the folder.
    pub fn key_pressed(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            return false;
        }
        if key == gdk::Key::space || key == gdk::Key::Escape {
            self.close();
        } else if key == gdk::Key::Return || key == gdk::Key::KP_Enter {
            self.open_item();
        } else if key == gdk::Key::Left || key == gdk::Key::Right
            || ((key == gdk::Key::Up || key == gdk::Key::Down) && self.preview.kind() != Some(TEXT)) {
            let delta = if key == gdk::Key::Left || key == gdk::Key::Up { -1 } else { 1 };
            let item = if self.pane.closed() { None } else { self.pane.move_selection(delta) };
            if let Some(item) = item {
                self.show_item(&item);
            }
        } else {
            return false;
        }
        true
    }
}
