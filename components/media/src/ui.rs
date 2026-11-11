//! media_manager.py `MediaPopup`: on-demand media controls (shared
//! PopupController, group `media`). Position is polled only while mapped.

use crate::art::album_name;
use crate::mpris::{Details, Media};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::popup::{header_button, popup_header, set_popup_width, HeaderAction, PopupController, POPUP_PADDING, POPUP_WIDTH};
use tessel_gtk::utils::{accessible, add_css, image, PRIORITY_APPLICATION};

pub const CSS: &str = r#"
window.tessel-media.tray-popup { background: transparent; }
.tessel-media .popup-surface { background: @theme_bg_color; color: @theme_fg_color;
  border-radius: 8px; border: 1px solid alpha(@theme_fg_color,.14); }
.tessel-media .media-title { font-size: 12pt; font-weight: 600; }
.tessel-media .album-cover { border-radius: 8px; }
.tessel-media .media-control { min-width: 46px; min-height: 46px; padding: 0;
  border-radius: 50%; border: 1px solid alpha(@theme_fg_color,.12);
  background: alpha(@theme_fg_color,.06); box-shadow: none; }
.tessel-media .media-control:hover { background: alpha(@theme_selected_bg_color,.22); }
.tessel-media .media-control.suggested-action { background: @theme_selected_bg_color; color: @theme_selected_fg_color; }
.tessel-media .media-control:disabled { opacity: .35; }
.tessel-media .mode-toggle { min-width: 34px; min-height: 34px; padding: 0; border-radius: 50%;
  background: transparent; border: none; box-shadow: none; }
.tessel-media .mode-toggle:checked { background: alpha(@theme_selected_bg_color,.22); color: @theme_selected_bg_color; }
.tessel-media .mode-toggle:disabled { opacity: .35; }
.tessel-media .seek-time { font-size: 9pt; opacity: .72; font-feature-settings: "tnum"; }
.tessel-media scale.seek trough { min-height: 4px; border-radius: 4px; }
"#;

/// `M:SS` or `H:MM:SS`.
pub fn clock(microseconds: f64) -> String {
    let seconds = (microseconds / 1_000_000.0).floor().max(0.0) as i64;
    let (hours, rest) = (seconds / 3600, seconds % 3600);
    if hours > 0 { format!("{hours}:{:02}:{:02}", rest / 60, rest % 60) } else { format!("{}:{:02}", rest / 60, rest % 60) }
}

/// What the popup needs from the application (the real app or a test harness).
pub trait PopupHost {
    fn backend(&self) -> crate::mpris::MediaSession;
    fn icon(&self, media: Option<&Media>) -> gio::Icon;
    fn art(&self, media: Option<&Media>) -> Option<gio::Icon>;
    fn hold(&self);
    fn release(&self);
    fn show_again(&self);
}

pub struct MediaPopup {
    pub window: gtk::ApplicationWindow,
    pub controller: RefCell<Option<PopupController>>,
    host: Weak<dyn PopupHost>,
    style: gtk::CssProvider,
    pub open_player: gtk::Button,
    pub player_switch: gtk::DropDown,
    pub player_names: RefCell<Vec<String>>,
    player_labels: RefCell<Option<Vec<String>>>,
    switching: Cell<bool>,
    pub icon: gtk::Image,
    pub visual: gtk::Stack,
    pub cover: gtk::Image,
    pub player: gtk::Label,
    pub title: gtk::Label,
    pub artist: gtk::Label,
    pub album: gtk::Label,
    pub seek_box: gtk::Box,
    pub seek: gtk::Scale,
    pub elapsed: gtk::Label,
    pub duration: gtk::Label,
    seek_dragging: Cell<bool>,
    seek_pending: RefCell<Option<(String, f64)>>,
    seek_source: RefCell<Option<glib::SourceId>>,
    pub tick_source: RefCell<Option<glib::SourceId>>,
    ticks: Cell<u32>,
    pub shuffle: gtk::ToggleButton,
    pub repeat: gtk::ToggleButton,
    pub buttons: Vec<(&'static str, gtk::Button)>,
    modes_syncing: Cell<bool>,
    pub status: gtk::Label,
}

fn row_label(css: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(28);
    label.add_css_class(css);
    label
}

impl MediaPopup {
    pub fn new(app: &gtk::Application, host: Weak<dyn PopupHost>, edge: &str, offset: i32) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder().application(app).title(tr!("Media")).build();
        set_popup_width(window.upcast_ref(), POPUP_WIDTH);
        window.add_css_class("tessel-media");
        let display = gtk::prelude::WidgetExt::display(&window);
        let style = add_css(CSS, Some(&display), PRIORITY_APPLICATION);
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        surface.add_css_class("popup-surface");
        window.set_child(Some(&surface));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        tessel_gtk::utils::set_margins(&content, POPUP_PADDING);
        surface.append(&content);
        let open_player = header_button("window-new-symbolic", &tr!("Open player"), None, false);
        let heading = popup_header(&["multimedia-player-symbolic"], &tr!("Media"), None, vec![HeaderAction::Widget(open_player.clone().upcast())]);
        content.append(&heading.root);
        // Shown only when more than one player is active.
        let player_switch = gtk::DropDown::from_strings(&[]);
        player_switch.set_tooltip_text(Some(&tr!("Choose media player")));
        accessible(&player_switch, &tr!("Choose media player"));
        player_switch.set_visible(false);
        content.append(&player_switch);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let icon = image("multimedia-player", 56);
        let visual = gtk::Stack::builder().valign(gtk::Align::Center).hhomogeneous(false).vhomogeneous(false).build();
        visual.add_named(&icon, Some("player"));
        let cover = image("audio-x-generic", 96);
        let cover_frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cover_frame.add_css_class("album-cover");
        cover_frame.set_overflow(gtk::Overflow::Hidden);
        cover_frame.append(&cover);
        visual.add_named(&cover_frame, Some("album"));
        row.append(&visual);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 5);
        text.set_hexpand(true);
        let player = row_label("row-detail");
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_max_width_chars(28);
        title.set_lines(2);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("media-title");
        let artist = row_label("row-detail");
        let album = row_label("row-detail");
        album.set_visible(false);
        for label in [&player, &title, &artist, &album] {
            text.append(label);
        }
        row.append(&text);
        content.append(&row);
        let seek_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        seek_box.set_visible(false);
        let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
        seek.set_draw_value(false);
        seek.add_css_class("seek");
        accessible(&seek, &tr!("Playback position"));
        let times = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let elapsed = gtk::Label::new(None);
        elapsed.set_xalign(0.0);
        elapsed.set_hexpand(true);
        elapsed.add_css_class("seek-time");
        let duration = gtk::Label::new(None);
        duration.set_xalign(1.0);
        duration.add_css_class("seek-time");
        times.append(&elapsed);
        times.append(&duration);
        seek_box.append(&seek);
        seek_box.append(&times);
        content.append(&seek_box);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        controls.set_halign(gtk::Align::Center);
        let shuffle = gtk::ToggleButton::new();
        shuffle.set_child(Some(&image("media-playlist-shuffle-symbolic", 18)));
        shuffle.add_css_class("mode-toggle");
        shuffle.set_valign(gtk::Align::Center);
        controls.append(&shuffle);
        let mut buttons = Vec::new();
        for (name, label, icon_name) in [("previous", "Previous track", "media-skip-backward-symbolic"),
                                         ("toggle", "Play", "media-playback-start-symbolic"),
                                         ("next", "Next track", "media-skip-forward-symbolic")] {
            let button = gtk::Button::new();
            button.set_child(Some(&image(icon_name, 23)));
            let text = tessel_core::i18n::translate(label);
            button.set_tooltip_text(Some(&text));
            button.add_css_class("media-control");
            if name == "toggle" {
                button.add_css_class("suggested-action");
            }
            accessible(&button, &text);
            controls.append(&button);
            buttons.push((name, button));
        }
        let repeat = gtk::ToggleButton::new();
        repeat.set_child(Some(&image("media-playlist-repeat-symbolic", 18)));
        repeat.add_css_class("mode-toggle");
        repeat.set_valign(gtk::Align::Center);
        controls.append(&repeat);
        content.append(&controls);
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_max_width_chars(40);
        status.add_css_class("row-detail");
        content.append(&status);
        let popup = Rc::new(Self {
            window, controller: RefCell::new(None), host, style, open_player, player_switch, player_names: RefCell::new(Vec::new()),
            player_labels: RefCell::new(None), switching: Cell::new(false), icon, visual, cover, player, title, artist, album,
            seek_box, seek, elapsed, duration, seek_dragging: Cell::new(false), seek_pending: RefCell::new(None),
            seek_source: RefCell::new(None), tick_source: RefCell::new(None), ticks: Cell::new(0), shuffle, repeat, buttons,
            modes_syncing: Cell::new(false), status,
        });
        popup.connect();
        let weak = Rc::downgrade(&popup);
        let controller = PopupController::new(popup.window.upcast_ref(), move || if let Some(p) = weak.upgrade() { p.apply_state() },
                                              edge, offset, "media");
        popup.controller.replace(Some(controller));
        let weak = Rc::downgrade(&popup);
        popup.window.connect_map(move |_| if let Some(p) = weak.upgrade() { p.start_ticks() });
        let weak = Rc::downgrade(&popup);
        popup.window.connect_unmap(move |_| if let Some(p) = weak.upgrade() { p.stop_ticks() });
        let (display, style) = (display, popup.style.clone());
        popup.window.connect_unrealize(move |_| gtk::style_context_remove_provider_for_display(&display, &style));
        popup.apply_state();
        popup
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.open_player.connect_clicked(move |_| if let Some(p) = weak.upgrade() { p.open_player() });
        let weak = Rc::downgrade(self);
        self.player_switch.connect_selected_notify(move |_| if let Some(p) = weak.upgrade() { p.player_chosen() });
        let weak = Rc::downgrade(self);
        self.seek.connect_change_value(move |_, _, value| match weak.upgrade() {
            Some(p) => p.seek_changed(value),
            None => glib::Propagation::Proceed,
        });
        let drag = gtk::GestureClick::new();
        drag.set_button(1);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        drag.connect_pressed(move |_, _, _, _| if let Some(p) = weak.upgrade() { p.seek_dragging.set(true) });
        let weak = Rc::downgrade(self);
        drag.connect_released(move |_, _, _, _| if let Some(p) = weak.upgrade() { p.seek_released() });
        let weak = Rc::downgrade(self);
        drag.connect_stopped(move |_| if let Some(p) = weak.upgrade() { p.seek_released() });
        self.seek.add_controller(drag);
        let weak = Rc::downgrade(self);
        self.shuffle.connect_toggled(move |button| if let Some(p) = weak.upgrade() {
            if !p.modes_syncing.get() {
                if let Some(host) = p.host.upgrade() {
                    let backend = host.backend();
                    backend.set_shuffle(backend.current().as_ref(), button.is_active());
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.repeat.connect_toggled(move |_| if let Some(p) = weak.upgrade() {
            if !p.modes_syncing.get() {
                if let Some(host) = p.host.upgrade() {
                    let backend = host.backend();
                    backend.cycle_loop(backend.current().as_ref());
                }
            }
        });
        for (name, button) in &self.buttons {
            let weak = Rc::downgrade(self);
            let name = *name;
            button.connect_clicked(move |_| {
                let Some(host) = weak.upgrade().and_then(|p| p.host.upgrade()) else { return };
                let backend = host.backend();
                match name { "previous" => backend.previous(), "next" => backend.next(), _ => backend.toggle() }
            });
        }
    }

    pub fn controller(&self) -> PopupController { self.controller.borrow().clone().expect("controller") }

    pub fn button(&self, name: &str) -> &gtk::Button { &self.buttons.iter().find(|(n, _)| *n == name).expect("button").1 }

    pub fn apply_state(&self) {
        let Some(host) = self.host.upgrade() else { return };
        let backend = host.backend();
        let media = backend.current();
        let m = media.as_ref();
        self.open_player.set_sensitive(m.is_some_and(|m| m.can_raise && !m.busy));
        self.open_player.set_tooltip_text(Some(&if m.is_some_and(|m| m.can_raise) { tr!("Open player") }
                                                else { tr!("This player does not support opening its window.") }));
        self.icon.set_from_gicon(&host.icon(m));
        let art = host.art(m);
        self.visual.set_visible_child_name(if art.is_some() { "album" } else { "player" });
        match &art {
            Some(art) => self.cover.set_from_gicon(art),
            None => self.cover.clear(),
        }
        let album = album_name(m);
        self.album.set_text(&album);
        self.album.set_tooltip_text(Some(&album).filter(|a| !a.is_empty()).map(|a| a.as_str()));
        self.album.set_visible(!album.is_empty());
        self.player.set_text(&match m {
            Some(m) if !m.identity.is_empty() => m.identity.clone(),
            Some(_) => tr!("Media player"),
            None => tr!("Playback"),
        });
        self.title.set_text(&m.map(|m| m.title.clone()).unwrap_or_else(|| tr!("Nothing playing")));
        self.title.set_tooltip_text(m.map(|m| m.title.as_str()));
        self.artist.set_text(m.map(|m| m.artist.as_str()).unwrap_or(""));
        self.artist.set_visible(m.is_some_and(|m| !m.artist.is_empty()));
        let playing = m.is_some_and(|m| m.playing);
        let label = if playing { tr!("Pause") } else { tr!("Play") };
        let toggle = self.button("toggle");
        if let Some(image) = toggle.child().and_downcast::<gtk::Image>() {
            image.set_icon_name(Some(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
        }
        toggle.set_tooltip_text(Some(&label));
        accessible(toggle, &label);
        let busy = m.is_some_and(|m| m.busy);
        self.button("toggle").set_sensitive(m.is_some_and(|m| m.controllable) && !busy);
        self.button("previous").set_sensitive(m.is_some_and(|m| m.can_previous) && !busy);
        self.button("next").set_sensitive(m.is_some_and(|m| m.can_next) && !busy);
        self.status.set_text(&match m {
            Some(m) if !m.error.is_empty() => m.error.clone(),
            Some(_) if playing => tr!("Playing"),
            Some(_) => tr!("Paused"),
            None => tr!("Start music or a video in your media app."),
        });
        self.apply_players(m, &backend);
        self.apply_extras(m, &backend);
    }

    fn apply_players(&self, media: Option<&Media>, backend: &crate::mpris::MediaSession) {
        let choices = backend.available();
        let names: Vec<String> = choices.iter().map(|c| c.name.clone()).collect();
        let labels: Vec<String> = choices.iter().map(|c| if c.identity.is_empty() {
            c.name.strip_prefix("org.mpris.MediaPlayer2.").unwrap_or(&c.name).to_owned()
        } else { c.identity.clone() }).collect();
        self.switching.set(true);
        if names != *self.player_names.borrow() || Some(&labels) != self.player_labels.borrow().as_ref() {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            self.player_switch.set_model(Some(&gtk::StringList::new(&refs)));
            self.player_names.replace(names.clone());
            self.player_labels.replace(Some(labels));
        }
        if let Some(index) = media.and_then(|m| names.iter().position(|n| *n == m.name)) {
            self.player_switch.set_selected(index as u32);
        }
        self.switching.set(false);
        self.player_switch.set_visible(names.len() > 1);
    }

    fn player_chosen(&self) {
        let index = self.player_switch.selected() as usize;
        let name = self.player_names.borrow().get(index).cloned();
        if let (false, Some(name), Some(host)) = (self.switching.get(), name, self.host.upgrade()) {
            host.backend().choose(&name);
        }
    }

    fn apply_extras(&self, media: Option<&Media>, backend: &crate::mpris::MediaSession) {
        let details = backend.details(media);
        let busy = media.is_some_and(|m| m.busy);
        self.modes_syncing.set(true);
        let shuffle = details.as_ref().and_then(|d| d.shuffle).unwrap_or(false);
        self.shuffle.set_visible(details.as_ref().is_some_and(|d| d.shuffle.is_some()));
        self.shuffle.set_active(shuffle);
        self.shuffle.set_sensitive(details.as_ref().is_some_and(|d| d.can_control) && !busy);
        let label = if shuffle { tr!("Shuffle on") } else { tr!("Shuffle off") };
        self.shuffle.set_tooltip_text(Some(&label));
        accessible(&self.shuffle, &label);
        let loop_status = details.as_ref().and_then(|d| d.loop_status.clone());
        self.repeat.set_visible(loop_status.is_some());
        self.repeat.set_active(matches!(loop_status.as_deref(), Some("Playlist" | "Track")));
        self.repeat.set_sensitive(details.as_ref().is_some_and(|d| d.can_control) && !busy);
        if let Some(image) = self.repeat.child().and_downcast::<gtk::Image>() {
            image.set_icon_name(Some(if loop_status.as_deref() == Some("Track") { "media-playlist-repeat-song-symbolic" }
                                     else { "media-playlist-repeat-symbolic" }));
        }
        let label = match loop_status.as_deref() { Some("Track") => tr!("Repeat track"), Some("Playlist") => tr!("Repeat all"),
                                                    _ => tr!("Repeat off") };
        self.repeat.set_tooltip_text(Some(&label));
        accessible(&self.repeat, &label);
        self.modes_syncing.set(false);
        self.apply_position(media, details.as_ref(), backend);
    }

    fn apply_position(&self, media: Option<&Media>, details: Option<&Details>, backend: &crate::mpris::MediaSession) {
        let length = details.map(|d| d.length).unwrap_or(0);
        self.seek_box.set_visible(length != 0);
        let (Some(media), Some(details), true) = (media, details, length != 0) else { return };
        self.seek.set_sensitive(details.can_seek && !media.busy);
        self.seek.adjustment().set_upper(length as f64 / 1_000_000.0);
        self.duration.set_text(&clock(length as f64));
        if self.seek_dragging.get() || self.seek_pending.borrow().is_some() {
            return;
        }
        let position = backend.position(&media.name).min(length);
        self.seek.set_value(position as f64 / 1_000_000.0);
        self.elapsed.set_text(&clock(position as f64));
    }

    fn refresh_position(&self) {
        let Some(host) = self.host.upgrade() else { return };
        let backend = host.backend();
        let media = backend.current();
        let details = backend.details(media.as_ref());
        self.apply_position(media.as_ref(), details.as_ref(), &backend);
    }

    /// The scale's `change-value` handler (public for tests: `emit('change-value', …)`).
    pub fn seek_changed(self: &Rc<Self>, value: f64) -> glib::Propagation {
        let Some(host) = self.host.upgrade() else { return glib::Propagation::Proceed };
        let backend = host.backend();
        let media = backend.current();
        let Some(details) = backend.details(media.as_ref()).filter(|d| d.can_seek) else { return glib::Propagation::Stop };
        let value = value.min(details.length as f64 / 1_000_000.0).max(0.0);
        self.seek_pending.replace(Some((media.unwrap().name, value)));
        self.elapsed.set_text(&clock(value * 1_000_000.0));
        if let Some(id) = self.seek_source.take() {
            id.remove();
        }
        if !self.seek_dragging.get() {
            self.schedule_seek(250);
        }
        glib::Propagation::Proceed
    }

    fn schedule_seek(self: &Rc<Self>, milliseconds: u64) {
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(milliseconds), move || {
            if let Some(p) = weak.upgrade() {
                p.seek_source.replace(None);
                p.send_seek();
            }
        });
        self.seek_source.replace(Some(id));
    }

    fn seek_released(self: &Rc<Self>) {
        self.seek_dragging.set(false);
        if self.seek_pending.borrow().is_some() && self.seek_source.borrow().is_none() {
            self.schedule_seek(1);
        }
    }

    fn send_seek(&self) {
        let pending = self.seek_pending.take();
        let Some(host) = self.host.upgrade() else { return };
        let backend = host.backend();
        let media = backend.current();
        if let (Some((name, value)), Some(media)) = (pending, media.as_ref()) {
            if media.name == name {
                backend.set_position(Some(media), value * 1_000_000.0);
            }
        }
        self.refresh_position();
    }

    fn start_ticks(self: &Rc<Self>) {
        if self.tick_source.borrow().is_none() {
            self.ticks.set(0);
            let weak = Rc::downgrade(self);
            let id = glib::timeout_add_local(std::time::Duration::from_millis(500), move || match weak.upgrade() {
                Some(p) => {
                    p.ticks.set(p.ticks.get() + 1);
                    if p.ticks.get() % 6 == 0 {
                        p.resync();
                    }
                    p.refresh_position();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
            self.tick_source.replace(Some(id));
            self.resync();
        }
    }

    pub fn stop_ticks(&self) {
        if let Some(id) = self.tick_source.take() {
            id.remove();
        }
        if let Some(id) = self.seek_source.take() {
            id.remove();
        }
        self.seek_pending.replace(None);
    }

    fn resync(&self) {
        let Some(host) = self.host.upgrade() else { return };
        let backend = host.backend();
        let media = backend.current();
        if backend.details(media.as_ref()).is_some_and(|d| d.length != 0) {
            backend.refresh_position(&media.unwrap().name);
        }
    }

    fn open_player(&self) {
        let Some(host) = self.host.upgrade() else { return };
        host.hold();
        // Release the popup's keyboard layer before asking the player/browser
        // to focus its own media window or tab. Keep the D-Bus request alive.
        self.window.set_visible(false);
        let weak = self.host.clone();
        let finished = move |success: bool| {
            if let Some(host) = weak.upgrade() {
                if !success {
                    host.show_again();
                }
                host.release();
            }
        };
        let finished: Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>> = Rc::new(RefCell::new(Some(Box::new(finished))));
        let once = finished.clone();
        let sent = host.backend().raise_player(Some(Box::new(move |ok| if let Some(f) = once.take() { f(ok) })));
        if !sent {
            if let Some(f) = finished.take() {
                f(false);
            }
        }
    }

    pub fn destroy(&self) {
        self.stop_ticks();
        if let Some(controller) = self.controller.borrow().as_ref() {
            controller.close();
        }
        self.window.destroy();
    }
}
