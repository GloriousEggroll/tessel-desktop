//! media_manager.py `MediaApplication`: `org.waydock.Media`, single instance,
//! exits when the popup is destroyed (no hold) except while "Open player"
//! waits for the player's Raise reply.

use crate::art::AlbumArt;
use crate::compat::argparse::{Opt, Parser};
use crate::icons::MediaIcons;
use crate::mpris::{Media, MediaSession};
use crate::ui::{MediaPopup, PopupHost};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub const APPLICATION_ID: &str = "org.waydock.Media";
const EDGES: &[&str] = &["top", "bottom", "left", "right"];

pub fn parser(prog: &str) -> Parser {
    Parser { prog: prog.into(), description: Some("On-demand media controls using the lock screen's MPRIS and icon backends."),
             options: vec![Opt::flag("--popup", None, false), Opt::choice("--popup-edge", EDGES), Opt::int("--popup-offset", "POPUP_OFFSET")] }
}

/// `(edge, offset)` or an argparse failure.
pub fn arguments(prog: &str, argv: &[String]) -> Result<(String, i32), (i32, String, bool)> {
    let parser = parser(prog);
    let parsed = parser.parse(argv)?;
    let offset = parsed.int("--popup-offset").unwrap_or(76);
    if offset < 0 {
        return Err(parser.error("--popup-offset must be nonnegative"));
    }
    Ok((parsed.value("--popup-edge").unwrap_or("bottom").to_owned(), offset.min(i32::MAX as i64) as i32))
}

pub struct MediaApp {
    pub app: gtk::Application,
    pub popup: RefCell<Option<Rc<MediaPopup>>>,
    pub backend: MediaSession,
    pub icons: MediaIcons,
    pub art: AlbumArt,
    holds: RefCell<Vec<gio::ApplicationHoldGuard>>,
    weak_self: RefCell<Weak<MediaApp>>,
    prog: String,
}

impl MediaApp {
    pub fn new(application_id: &str, prog: &str) -> Rc<Self> {
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        let this = Rc::new_cyclic(|weak: &Weak<MediaApp>| {
            let (w1, w2) = (weak.clone(), weak.clone());
            Self {
                app, popup: RefCell::new(None),
                backend: MediaSession::new(move || if let Some(this) = w1.upgrade() { this.changed() }, false),
                icons: MediaIcons::default(),
                art: AlbumArt::new(move || if let Some(this) = w2.upgrade() { this.changed() }, None),
                holds: RefCell::new(Vec::new()), weak_self: RefCell::new(weak.clone()), prog: prog.into(),
            }
        });
        let weak = Rc::downgrade(&this);
        this.app.connect_startup(move |app| {
            suite_gtk::theme::attach_application(app);
            if let Some(this) = weak.upgrade() {
                this.backend.start(false);
            }
        });
        let weak = Rc::downgrade(&this);
        this.app.connect_command_line(move |_, line| {
            let Some(this) = weak.upgrade() else { return glib::ExitCode::FAILURE };
            let words: Vec<String> = line.arguments().iter().skip(1).map(|w| w.to_string_lossy().into_owned()).collect();
            match arguments(&this.prog, &words) {
                Ok((edge, offset)) => {
                    this.show(&edge, offset);
                    glib::ExitCode::SUCCESS
                }
                Err((code, text, stdout)) => {
                    if stdout { line.print_literal(&text) } else { line.printerr_literal(&text) }
                    glib::ExitCode::from(code as u8)
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.app.connect_activate(move |_| if let Some(this) = weak.upgrade() { this.show("bottom", 76) });
        let weak = Rc::downgrade(&this);
        this.app.connect_shutdown(move |_| if let Some(this) = weak.upgrade() { this.shutdown() });
        this
    }

    fn weak(&self) -> Weak<MediaApp> { self.weak_self.borrow().clone() }

    pub fn popup(&self) -> Option<Rc<MediaPopup>> { self.popup.borrow().clone() }

    pub fn show(&self, edge: &str, offset: i32) {
        match self.popup() {
            None => {
                let host: Weak<dyn PopupHost> = self.weak();
                let popup = MediaPopup::new(&self.app, host, edge, offset);
                let weak = self.weak();
                popup.window.connect_visible_notify(move |window| {
                    if !window.is_visible() {
                        let (weak, window) = (weak.clone(), window.clone());
                        glib::idle_add_local_once(move || if let Some(this) = weak.upgrade() { this.hidden(&window) });
                    }
                });
                self.popup.replace(Some(popup.clone()));
                popup.controller().show();
            }
            Some(popup) => {
                let controller = popup.controller();
                controller.set_position(edge, offset);
                controller.activate();
            }
        }
    }

    pub fn changed(&self) {
        if let Some(popup) = self.popup() {
            popup.apply_state();
        }
    }

    fn hidden(&self, window: &gtk::ApplicationWindow) {
        // show_pending: set_position() hid it only to reappear on another edge.
        let Some(popup) = self.popup() else { return };
        if &popup.window == window && !window.is_visible() && !popup.controller().show_pending() {
            self.popup.replace(None);
            popup.destroy();
        }
    }

    pub fn shutdown(&self) {
        self.backend.close();
        self.icons.close();
        self.art.close();
        if let Some(popup) = self.popup.take() {
            popup.destroy();
        }
    }
}

impl PopupHost for MediaApp {
    fn backend(&self) -> MediaSession { self.backend.clone() }
    fn icon(&self, media: Option<&Media>) -> gio::Icon { self.icons.resolve(media) }
    fn art(&self, media: Option<&Media>) -> Option<gio::Icon> { self.art.resolve(media) }
    fn hold(&self) { self.holds.borrow_mut().push(self.app.hold()); }
    fn release(&self) { self.holds.borrow_mut().pop(); }
    fn show_again(&self) { self.show("bottom", 76) }
}
