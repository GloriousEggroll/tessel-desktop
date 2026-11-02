//! media_controls.py: inline media information and controls, oriented along
//! the dock's edge.
//!
//! API: [`available_player_icon`] (pure-ish: real player/site icon or `None`
//! for generic placeholders/missing files), [`VerticalText`] (a label rotated
//! 270° on the left edge / 90° otherwise, measured with swapped axes),
//! [`MediaControls::new(dock, app, &StatusButton)`] (`tessel-dock-media` box:
//! song label, previous/toggle/next `tessel-dock-status tessel-dock-media-control`
//! buttons calling `status_action("media-<command>")`, then the Media popup
//! button whose child becomes a stack `music` (the glyph visual) / `player`
//! (the player icon)), `.refresh()`, `.close()` and widget getters.
//! Data: `app.status()["media"]` (MediaStatus keys `title artist tooltip
//! playing can_previous can_toggle can_next`) and `app.media_icon()`.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, graphene, gsk, pango};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tessel_core::pyjson::{py_str_or_empty, truthy};
use tessel_core::tr;

use super::host::{accessible_name, DockApp, DockView};
use super::indicators::StatusButton;

const GENERIC: [&str; 6] = ["multimedia-player", "multimedia-player-symbolic", "application-x-executable",
                            "application-x-executable-symbolic", "image-missing", "image-missing-symbolic"];

/// Use a real player/site icon; leave generic placeholders to the note.
pub fn available_player_icon(icon: Option<&gio::Icon>, theme: &gtk::IconTheme) -> Option<gio::Icon> {
    let icon = icon?;
    if let Some(themed) = icon.downcast_ref::<gio::ThemedIcon>() {
        return themed.names().iter()
            .find(|name| !GENERIC.contains(&name.as_str()) && theme.has_icon(name))
            .map(|name| gio::ThemedIcon::new(name).upcast());
    }
    if let Some(file) = icon.downcast_ref::<gio::FileIcon>() {
        return file.file().query_exists(gio::Cancellable::NONE).then(|| icon.clone());
    }
    icon.downcast_ref::<gio::BytesIcon>().map(|_| icon.clone())
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct VerticalText {
        pub label: RefCell<Option<gtk::Label>>,
        pub left: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VerticalText {
        const NAME: &'static str = "WaydockVerticalText";
        type Type = super::VerticalText;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for VerticalText {
        fn dispose(&self) {
            if let Some(label) = self.label.take() {
                if label.parent().as_ref() == Some(self.obj().upcast_ref()) {
                    label.unparent();
                }
            }
        }
    }

    impl WidgetImpl for VerticalText {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(label) = self.label.borrow().clone() else { return (0, 0, -1, -1) };
            let other = if orientation == gtk::Orientation::Horizontal { gtk::Orientation::Vertical }
                        else { gtk::Orientation::Horizontal };
            let (minimum, natural, _, _) = label.measure(other, for_size);
            (minimum, natural, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(label) = self.label.borrow().clone() else { return };
            let left = self.left.get();
            let point = if left { graphene::Point::new(0.0, height as f32) } else { graphene::Point::new(width as f32, 0.0) };
            let transform = gsk::Transform::new().translate(&point).rotate(if left { 270.0 } else { 90.0 });
            label.allocate(height, width, -1, Some(transform));
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(label) = self.label.borrow().as_ref() {
                self.obj().snapshot_child(label, snapshot);
            }
        }
    }
}

glib::wrapper! {
    /// Rotate the text along a side panel without increasing its thickness.
    pub struct VerticalText(ObjectSubclass<imp::VerticalText>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VerticalText {
    pub fn new(label: &gtk::Label, edge: &str) -> Self {
        let widget: Self = glib::Object::new();
        widget.imp().left.set(edge == "left");
        label.set_parent(&widget);
        widget.imp().label.replace(Some(label.clone()));
        widget.set_overflow(gtk::Overflow::Hidden);
        widget
    }

    pub fn label(&self) -> Option<gtk::Label> { self.imp().label.borrow().clone() }

    pub fn close(&self) {
        if let Some(label) = self.imp().label.borrow().as_ref() {
            if label.parent().as_ref() == Some(self.upcast_ref()) {
                label.unparent();
            }
        }
    }
}

struct Inner {
    app: Rc<dyn DockApp>,
    root: gtk::Box,
    player_icon: gtk::Image,
    popup_visual: gtk::Stack,
    song: gtk::Label,
    text: gtk::Widget,
    controls: RefCell<Vec<(String, gtk::Button, gtk::Image)>>,
}

#[derive(Clone)]
pub struct MediaControls(Rc<Inner>);

impl MediaControls {
    pub fn new(dock: &Rc<dyn DockView>, app: &Rc<dyn DockApp>, popup_button: &StatusButton) -> Self {
        let root = gtk::Box::new(dock.orientation(), 6);
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.add_css_class("tessel-dock-media");
        let player_icon = gtk::Image::from_icon_name("multimedia-player-symbolic");
        player_icon.set_pixel_size(20);
        // Reuse the music-note visual as the fallback inside the one popup button.
        popup_button.button.set_child(None::<&gtk::Widget>);
        let popup_visual = gtk::Stack::new();
        popup_visual.set_halign(gtk::Align::Center);
        popup_visual.set_valign(gtk::Align::Center);
        popup_visual.add_named(&popup_button.visual, Some("music"));
        popup_visual.add_named(&player_icon, Some("player"));
        popup_button.button.set_child(Some(&popup_visual));
        let song = gtk::Label::new(None);
        song.set_xalign(0.0);
        song.set_ellipsize(pango::EllipsizeMode::End);
        song.set_single_line_mode(true);
        song.set_width_chars(22);
        song.set_max_width_chars(22);
        song.add_css_class("tessel-dock-media-song");
        let text: gtk::Widget = if dock.horizontal() { song.clone().upcast() }
                                else { VerticalText::new(&song, &dock.config().edge).upcast() };
        root.append(&text);
        let controls_box = gtk::Box::new(dock.orientation(), 0);
        let mut controls = Vec::new();
        for (command, title, icon) in [("previous", "Previous track", "media-skip-backward-symbolic"),
                                       ("toggle", "Play", "media-playback-start-symbolic"),
                                       ("next", "Next track", "media-skip-forward-symbolic")] {
            let picture = gtk::Image::from_icon_name(icon);
            picture.set_pixel_size(16);
            let button = gtk::Button::new();
            button.set_child(Some(&picture));
            let title = tr!(title);
            button.set_tooltip_text(Some(&title));
            button.add_css_class("tessel-dock-status");
            button.add_css_class("tessel-dock-media-control");
            accessible_name(&button, &title);
            let app = app.clone();
            button.connect_clicked(move |_| { app.status_action(&format!("media-{command}")); });
            controls_box.append(&button);
            controls.push((command.to_owned(), button, picture));
        }
        root.append(&controls_box);
        // The existing Media button still opens the complete popup.
        root.append(&popup_button.button);
        Self(Rc::new(Inner { app: app.clone(), root, player_icon, popup_visual, song, text,
                             controls: RefCell::new(controls) }))
    }

    pub fn widget(&self) -> gtk::Box { self.0.root.clone() }
    pub fn song(&self) -> gtk::Label { self.0.song.clone() }
    /// The song label, or its VerticalText wrapper on side edges.
    pub fn text(&self) -> gtk::Widget { self.0.text.clone() }
    pub fn player_icon(&self) -> gtk::Image { self.0.player_icon.clone() }
    pub fn popup_visual(&self) -> gtk::Stack { self.0.popup_visual.clone() }
    pub fn control(&self, command: &str) -> Option<gtk::Button> {
        self.0.controls.borrow().iter().find(|(name, ..)| name == command).map(|(_, button, _)| button.clone())
    }
    pub fn controls(&self) -> HashMap<String, gtk::Button> {
        self.0.controls.borrow().iter().map(|(name, button, _)| (name.clone(), button.clone())).collect()
    }

    pub fn refresh(&self) {
        let status = self.0.app.status();
        let empty = serde_json::Map::new();
        let state = status.get("media").and_then(|value| value.as_object()).unwrap_or(&empty);
        let title = state.get("title").filter(|v| truthy(v)).map(|v| py_str_or_empty(v))
            .unwrap_or_else(|| tr!("Nothing playing"));
        let artist = state.get("artist").filter(|v| truthy(v)).map(|v| py_str_or_empty(v)).unwrap_or_default();
        let text = if artist.is_empty() { title } else { format!("{title} · {artist}") };
        if self.0.song.text() != text {
            self.0.song.set_text(&text);
        }
        let tooltip = state.get("tooltip").filter(|v| truthy(v)).map(|v| py_str_or_empty(v))
            .unwrap_or_else(|| tr!("Media"));
        self.0.song.set_tooltip_text(Some(&tooltip));
        let theme = gtk::IconTheme::for_display(&self.0.root.display());
        let icon = available_player_icon(self.0.app.media_icon().as_ref(), &theme);
        match &icon {
            Some(icon) => {
                let current = self.0.player_icon.gicon();
                if current.as_ref().is_none_or(|current| !current.equal(Some(icon))) {
                    self.0.player_icon.set_from_gicon(icon);
                }
            }
            None => self.0.player_icon.clear(),
        }
        self.0.popup_visual.set_visible_child_name(if icon.is_some() { "player" } else { "music" });
        let playing = state.get("playing").is_some_and(truthy);
        let idle = !state.get("title").is_some_and(truthy);
        for (command, button, picture) in self.0.controls.borrow().iter() {
            if command == "toggle" {
                picture.set_icon_name(Some(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
                let title = if playing { tr!("Pause") } else { tr!("Play") };
                button.set_tooltip_text(Some(&title));
                accessible_name(button, &title);
            }
        }
        // With nothing playing the bar keeps only the media button.
        self.0.text.set_visible(!idle);
        if idle { self.0.root.add_css_class("idle") } else { self.0.root.remove_css_class("idle") }
        for (command, button, _) in self.0.controls.borrow().iter() {
            let enabled = state.get(&format!("can_{command}")).is_some_and(truthy);
            button.set_sensitive(enabled);
            button.set_visible(enabled || !idle);
        }
    }

    pub fn close(&self) {
        if let Some(text) = self.0.text.downcast_ref::<VerticalText>() {
            text.close();
        }
    }
}
