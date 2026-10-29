//! files_pane.Thumbnails: decode only visible images, with bounded work
//! (two decodes at a time) and a small shared texture cache (96 entries).

use files_model::model::FileItem;
use gtk::{gdk, gdk_pixbuf, glib};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

pub const CACHE_LIMIT: usize = 96;
pub const RUNNING_LIMIT: usize = 2;
pub const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub paths: Vec<String>,
    pub size: i32,
    pub modified: u64,
    pub usec: u32,
    pub nsec: u32,
    pub bytes: u64,
}

/// A decoded pixbuf moved whole from the worker to the main thread.
struct SendPixbuf(Option<gdk_pixbuf::Pixbuf>);
// SAFETY: the pixbuf is created on the worker and only touched by the receiver.
unsafe impl Send for SendPixbuf {}

pub type Notify = Rc<dyn Fn(Option<gdk::Texture>)>;

#[derive(Default)]
struct State {
    cache: VecDeque<(Key, gdk::Texture)>,
    pending: HashMap<Key, Vec<(u64, Notify)>>,
    queued: VecDeque<(Key, Vec<String>, i32)>,
    running: Vec<Key>,
    closed: bool,
    next_id: u64,
    decoder: Option<Decoder>,
}

/// Replaces [`decode`] (tests: `patch.object(service, '_decode', ...)`).
pub type Decoder = std::sync::Arc<dyn Fn(&[String], i32) -> Option<gdk_pixbuf::Pixbuf> + Send + Sync>;

#[derive(Clone, Default)]
pub struct Thumbnails(Rc<RefCell<State>>);

/// Cancels one request (Python's returned lambda).
pub struct Cancel {
    owner: Weak<RefCell<State>>,
    key: Key,
    id: u64,
}

impl Cancel {
    pub fn cancel(self) {
        if let Some(state) = self.owner.upgrade() {
            let mut s = state.borrow_mut();
            let empty = match s.pending.get_mut(&self.key) {
                Some(callbacks) => {
                    callbacks.retain(|(id, _)| *id != self.id);
                    callbacks.is_empty()
                }
                None => true,
            };
            if empty && !s.running.contains(&self.key) {
                s.pending.remove(&self.key);
                s.queued.retain(|(k, _, _)| *k != self.key);
            }
        }
    }
}

/// Decode one candidate after another: `new_from_file_at_scale` + embedded orientation.
pub fn decode(paths: &[String], size: i32) -> Option<gdk_pixbuf::Pixbuf> {
    for path in paths {
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() <= MAX_SOURCE_BYTES => {}
            _ => continue,
        }
        if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true) {
            return Some(pixbuf.apply_embedded_orientation().unwrap_or(pixbuf));
        }
    }
    None
}

impl Thumbnails {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self, item: &FileItem, size: i32, callback: Notify) -> Option<Cancel> {
        let mut paths = Vec::new();
        if let Some(thumbnail) = item.thumbnail_path() {
            if item.thumbnail_valid() {
                paths.push(thumbnail);
            }
        }
        if let Some(path) = item.path() {
            let path = path.to_string_lossy().into_owned();
            if item.content_type().starts_with("image/") && !paths.contains(&path) {
                paths.push(path);
            }
        }
        let info = item.info();
        let key = Key {
            paths: paths.clone(), size, modified: item.modified(), usec: info.attribute_uint32("time::modified-usec"),
            nsec: info.attribute_uint32("time::modified-nsec"), bytes: item.size(),
        };
        self.request_key(key, paths, size, callback)
    }

    pub fn request_key(&self, key: Key, paths: Vec<String>, size: i32, callback: Notify) -> Option<Cancel> {
        let cached = {
            let mut s = self.0.borrow_mut();
            if s.closed || paths.is_empty() {
                return None;
            }
            match s.cache.iter().position(|(k, _)| *k == key) {
                Some(index) => {
                    let entry = s.cache.remove(index).unwrap();
                    let texture = entry.1.clone();
                    s.cache.push_back(entry);
                    Some(texture)
                }
                None => None,
            }
        };
        if let Some(texture) = cached {
            callback(Some(texture));
            return None;
        }
        let id = {
            let mut s = self.0.borrow_mut();
            s.next_id += 1;
            let id = s.next_id;
            match s.pending.get_mut(&key) {
                Some(list) => list.push((id, callback)),
                None => {
                    s.pending.insert(key.clone(), vec![(id, callback)]);
                    s.queued.push_back((key.clone(), paths, size));
                }
            }
            id
        };
        self.start_next();
        Some(Cancel { owner: Rc::downgrade(&self.0), key, id })
    }

    fn start_next(&self) {
        loop {
            let (key, paths, size) = {
                let mut s = self.0.borrow_mut();
                if s.closed || s.running.len() >= RUNNING_LIMIT {
                    return;
                }
                let Some(next) = s.queued.pop_front() else { return };
                s.running.push(next.0.clone());
                next
            };
            let this = self.clone();
            let decoder = self.0.borrow().decoder.clone();
            glib::spawn_future_local(async move {
                let decoded = gtk::gio::spawn_blocking(move || SendPixbuf(match decoder {
                    Some(decoder) => decoder(&paths, size),
                    None => decode(&paths, size),
                })).await.ok().and_then(|p| p.0);
                this.finish(key, decoded);
            });
        }
    }

    fn finish(&self, key: Key, pixbuf: Option<gdk_pixbuf::Pixbuf>) {
        let callbacks = {
            let mut s = self.0.borrow_mut();
            s.running.retain(|k| *k != key);
            let callbacks = s.pending.remove(&key).unwrap_or_default();
            if s.closed {
                return;
            }
            callbacks
        };
        #[allow(deprecated)]
        let texture = pixbuf.map(|p| gdk::Texture::for_pixbuf(&p));
        if let Some(texture) = &texture {
            let mut s = self.0.borrow_mut();
            s.cache.push_back((key, texture.clone()));
            while s.cache.len() > CACHE_LIMIT {
                s.cache.pop_front();
            }
        }
        self.start_next();
        for (_, notify) in callbacks {
            notify(texture.clone());
        }
    }

    pub fn set_decoder(&self, decoder: Option<Decoder>) {
        self.0.borrow_mut().decoder = decoder;
    }

    pub fn close(&self) {
        let mut s = self.0.borrow_mut();
        s.closed = true;
        s.pending.clear();
        s.queued.clear();
        s.cache.clear();
    }

    pub fn closed(&self) -> bool {
        self.0.borrow().closed
    }
    pub fn cache_len(&self) -> usize {
        self.0.borrow().cache.len()
    }
    pub fn queued_len(&self) -> usize {
        self.0.borrow().queued.len()
    }
    pub fn pending_len(&self) -> usize {
        self.0.borrow().pending.len()
    }
    pub fn running_len(&self) -> usize {
        self.0.borrow().running.len()
    }
}
