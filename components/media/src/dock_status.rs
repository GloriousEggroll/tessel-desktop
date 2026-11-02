//! tessel-dock/media_status.py: one event-driven media controller shared by all
//! dock monitors (runs inside Tessel Dock; ported here with the MPRIS client so
//! the dock port can reuse it). Playback changes only on an explicit action.

use crate::icons::MediaIcons;
use gio::prelude::IconExt;
use crate::mpris::MediaSession;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tessel_core::tr;

#[derive(Clone, Debug, PartialEq)]
pub struct DockMedia {
    pub text: String,
    pub tooltip: String,
    pub visible: bool,
    pub title: String,
    pub artist: String,
    pub player: String,
    pub playing: bool,
    pub can_toggle: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub icon_revision: u64,
    pub classes: Vec<&'static str>,
}

struct Inner {
    changed: Box<dyn Fn(DockMedia)>,
    closed: Cell<bool>,
    icon: RefCell<Option<gio::Icon>>,
    icon_revision: Cell<u64>,
    backend: RefCell<Option<MediaSession>>,
    icons: MediaIcons,
}

#[derive(Clone)]
pub struct MediaStatus(Rc<Inner>);

impl MediaStatus {
    pub fn new(changed: impl Fn(DockMedia) + 'static) -> Self {
        let status = Self(Rc::new(Inner { changed: Box::new(changed), closed: Cell::new(false), icon: RefCell::new(None),
                                          icon_revision: Cell::new(0), backend: RefCell::new(None), icons: MediaIcons::default() }));
        let weak = Rc::downgrade(&status.0);
        let backend = MediaSession::new(move || if let Some(inner) = weak.upgrade() { MediaStatus(inner).refresh() }, false);
        status.0.backend.replace(Some(backend));
        status
    }

    pub fn icon(&self) -> Option<gio::Icon> { self.0.icon.borrow().clone() }

    fn backend(&self) -> MediaSession { self.0.backend.borrow().clone().expect("backend") }

    pub fn start(&self) {
        self.refresh();
        self.backend().start(false);
    }

    pub fn refresh(&self) {
        if self.0.closed.get() {
            return;
        }
        let media = self.backend().current();
        let icon = self.0.icons.resolve(media.as_ref());
        if self.0.icon.borrow().as_ref().is_none_or(|known| !known.equal(Some(&icon))) {
            self.0.icon_revision.set(self.0.icon_revision.get() + 1);
        }
        self.0.icon.replace(Some(icon));
        let mut lines = vec![tr!("Media")];
        match &media {
            Some(m) => {
                let state = if m.playing { tr!("Playing") } else { tr!("Paused") };
                lines.extend([m.identity.clone(), m.title.clone(), m.artist.clone(), state, m.error.clone()].into_iter().filter(|l| !l.is_empty()));
            }
            None => lines.push(tr!("Nothing playing")),
        }
        let busy = media.as_ref().is_some_and(|m| m.busy);
        (self.0.changed)(DockMedia {
            text: "\u{f001}".into(), tooltip: lines.join("\n"), visible: true,
            title: media.as_ref().map(|m| m.title.clone()).unwrap_or_else(|| tr!("Nothing playing")),
            artist: media.as_ref().map(|m| m.artist.clone()).unwrap_or_default(),
            player: media.as_ref().map(|m| m.identity.clone()).unwrap_or_default(),
            playing: media.as_ref().is_some_and(|m| m.playing),
            can_toggle: media.as_ref().is_some_and(|m| m.controllable) && !busy,
            can_previous: media.as_ref().is_some_and(|m| m.can_previous) && !busy,
            can_next: media.as_ref().is_some_and(|m| m.can_next) && !busy,
            icon_revision: self.0.icon_revision.get(),
            classes: media.as_ref().map(|m| vec![if m.playing { "playing" } else { "paused" }]).unwrap_or_default(),
        });
    }

    pub fn action(&self, command: &str) -> bool {
        if self.0.closed.get() {
            return false;
        }
        match command {
            "previous" => self.backend().previous(),
            "toggle" => self.backend().toggle(),
            "next" => self.backend().next(),
            _ => return false,
        }
        true
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.backend().close();
        self.0.icons.close();
    }
}
