//! Drag & drop between the menu and the dock (`waydock-app:<desktop-id>`
//! strings; external file drops): `drag_content`, `install_button_drag_source`
//! (dock buttons), [`DropHost`] + `install_drop_target` / `dropped` (the
//! dock's pinned strip). Menu tiles get their source in `app_menu`.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::rc::Rc;

use super::logic::{drop_before, dropped_desktop_ids, pin_drop_index, DropValue, DRAG_PREFIX};
use super::popup::{children, translate};
use crate::core::app_catalog::AppCatalog;

/// `drag_content(desktop_id)`: a `G_TYPE_STRING` provider `waydock-app:<id>`.
pub fn drag_content(desktop_id: &str) -> gdk::ContentProvider {
    gdk::ContentProvider::for_value(&format!("{DRAG_PREFIX}{desktop_id}").to_value())
}

/// `_button_drag_source(button)`: MOVE|COPY; `desktop_id(widget)` reads the
/// button's current `dock_item.desktop_id` at prepare time (`""` = no drag).
/// `begin` = cancel preview timer + close preview + `enter("drag")`;
/// `end` = `leave("drag")`.
pub fn install_button_drag_source(
    button: &impl IsA<gtk::Widget>,
    desktop_id: impl Fn(&gtk::Widget) -> String + 'static,
    begin: impl Fn() + 'static,
    end: impl Fn() + 'static,
) -> gtk::DragSource {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE | gdk::DragAction::COPY);
    // Read the widget from the controller so the closure never owns the button.
    source.connect_prepare(move |source, _x, _y| {
        let ident = source.widget().map(|w| desktop_id(&w)).unwrap_or_default();
        (!ident.is_empty()).then(|| drag_content(&ident))
    });
    source.connect_drag_begin(move |_s, _d| begin());
    source.connect_drag_end(move |_s, _d, _delete| end());
    button.add_controller(source.clone());
    source
}

/// What a drop on the dock needs (`DockSurface` + `self.app`).
pub trait DropHost {
    fn catalog(&self) -> Rc<RefCell<AppCatalog>>;
    /// `self.panel` (drop coordinates are translated into it).
    fn panel(&self) -> gtk::Widget;
    /// `self.pinned_box` children in order with their `dock_item.desktop_id`
    /// (`None` for children without a dock item). Visibility is checked here.
    fn pinned_children(&self) -> Vec<(gtk::Widget, Option<String>)>;
    fn horizontal(&self) -> bool;
    /// `self.app.place_pin(desktop_id, before)` (insert before that pin; `None` appends).
    fn place_pin(&self, desktop_id: &str, before: Option<&str>);
    fn enter(&self, zone: &str);
    fn leave(&self, zone: &str);
}

/// Convert a dropped `glib::Value` (string or `GdkFileList`).
pub fn drop_value(value: &glib::Value) -> Option<DropValue> {
    if let Ok(text) = value.get::<String>() {
        return Some(DropValue::Text(text));
    }
    if let Ok(files) = value.get::<gdk::FileList>() {
        return Some(DropValue::Files(files.files().iter().map(|f| f.path().map(|p| p.to_string_lossy().into_owned())).collect()));
    }
    None
}

/// `pin_drop_index(x, y)` for a point in panel coordinates.
pub fn pin_index_at(host: &dyn DropHost, x: f64, y: f64) -> usize {
    let horizontal = host.horizontal();
    let panel = host.panel();
    let coordinate = if horizontal { x } else { y };
    let buttons: Vec<Option<(f64, f64)>> = host
        .pinned_children()
        .into_iter()
        .filter(|(button, _)| button.is_visible())
        .map(|(button, _)| {
            translate(&button, &panel, 0.0, 0.0).map(|point| {
                // Python's get_allocated_width/height: the margin box.
                #[allow(deprecated)]
                let extent = if horizontal { button.allocated_width() } else { button.allocated_height() };
                (if horizontal { point.0 } else { point.1 }, extent as f64)
            })
        })
        .collect();
    pin_drop_index(coordinate, &buttons)
}

/// `_dropped(target, value, x, y)`: `widget` is the drop target's widget.
pub fn dropped(host: &dyn DropHost, widget: &gtk::Widget, value: &DropValue, x: f64, y: f64) -> bool {
    let identities = dropped_desktop_ids(value, &host.catalog().borrow());
    if identities.is_empty() {
        return false;
    }
    let (px, py) = translate(widget, &host.panel(), x, y).unwrap_or((x, y));
    let index = pin_index_at(host, px, py);
    let pinned: Vec<String> =
        host.pinned_children().into_iter().filter(|(b, id)| b.is_visible() && id.is_some()).filter_map(|(_, id)| id).collect();
    let before = drop_before(index, &pinned);
    for ident in identities {
        host.place_pin(&ident, before.as_deref());
    }
    true
}

/// `_install_drop_target(widget)`: file lists and strings, COPY|MOVE.
pub fn install_drop_target(widget: &impl IsA<gtk::Widget>, host: std::rc::Weak<dyn DropHost>) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY | gdk::DragAction::MOVE);
    target.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
    let enter = host.clone();
    target.connect_enter(move |_t, _x, _y| {
        if let Some(host) = enter.upgrade() {
            host.enter("drop");
        }
        gdk::DragAction::COPY
    });
    let leave = host.clone();
    target.connect_leave(move |_t| {
        if let Some(host) = leave.upgrade() {
            host.leave("drop");
        }
    });
    target.connect_drop(move |target, value, x, y| {
        let (Some(host), Some(widget), Some(value)) = (host.upgrade(), target.widget(), drop_value(value)) else { return false };
        dropped(&*host, &widget, &value, x, y)
    });
    widget.add_controller(target.clone());
    target
}

/// Visible children of a box (helper for hosts' `pinned_children`).
pub fn visible_children(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    children(widget).into_iter().filter(|w| w.is_visible()).collect()
}
