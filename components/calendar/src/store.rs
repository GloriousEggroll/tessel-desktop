//! `EventStore`: loads configured sources off the main thread, guards stale
//! loads with a generation counter and memoizes `range()` (cleared at > 8).

use crate::civil::Date;
use crate::events::{expand, friendly_error, load_source, Event, Expansion, HttpOpener, LoadError, Occurrence, Source};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub type Loader = Arc<dyn Fn(&Source) -> Result<Vec<Event>, LoadError> + Send + Sync>;
pub type LoadResult = (Vec<Event>, Vec<(String, String)>);
pub type Work = Box<dyn FnOnce() -> LoadResult + Send>;
pub type Deliver = Box<dyn FnOnce(LoadResult)>;
/// Runs `work` on a worker thread and `deliver` on the main loop.
pub type Spawner = Rc<dyn Fn(Work, Deliver)>;

pub fn default_loader() -> Loader {
    Arc::new(|source: &Source| load_source(source, &HttpOpener))
}

pub fn thread_spawner() -> Spawner {
    Rc::new(|work: Work, deliver: Deliver| {
        let handle = gio::spawn_blocking(work);
        glib::spawn_future_local(async move {
            if let Ok(result) = handle.await {
                deliver(result);
            }
        });
    })
}

struct Inner {
    changed: RefCell<Rc<dyn Fn()>>,
    loader: Loader,
    spawn: Spawner,
    events: RefCell<Rc<Vec<Event>>>,
    errors: RefCell<Vec<(String, String)>>,
    sources: RefCell<Vec<Source>>,
    loading: Cell<bool>,
    closed: Cell<bool>,
    generation: Cell<u64>,
    cache: RefCell<HashMap<(Date, Date), Rc<Expansion>>>,
}

#[derive(Clone)]
pub struct EventStore(Rc<Inner>);

impl EventStore {
    pub fn new(changed: impl Fn() + 'static, loader: Loader, spawn: Spawner) -> Self {
        Self(Rc::new(Inner { changed: RefCell::new(Rc::new(changed)), loader, spawn, events: RefCell::new(Rc::new(Vec::new())),
                             errors: RefCell::default(), sources: RefCell::default(), loading: Cell::new(false),
                             closed: Cell::new(false), generation: Cell::new(0), cache: RefCell::default() }))
    }

    pub fn with_defaults(changed: impl Fn() + 'static) -> Self {
        Self::new(changed, default_loader(), thread_spawner())
    }

    pub fn set_changed(&self, changed: impl Fn() + 'static) {
        self.0.changed.replace(Rc::new(changed));
    }

    fn notify(&self) {
        let changed = self.0.changed.borrow().clone();
        changed();
    }

    pub fn events(&self) -> Rc<Vec<Event>> { self.0.events.borrow().clone() }
    /// Source location → short English message, in load order.
    pub fn errors(&self) -> Vec<(String, String)> { self.0.errors.borrow().clone() }
    pub fn sources(&self) -> Vec<Source> { self.0.sources.borrow().clone() }
    pub fn loading(&self) -> bool { self.0.loading.get() }

    pub fn set_sources(&self, sources: &[Source], force: bool) {
        let same = *self.0.sources.borrow() == sources;
        if !force && same && (!self.0.events.borrow().is_empty() || !self.0.errors.borrow().is_empty() || self.0.loading.get()) {
            return;
        }
        self.0.sources.replace(sources.to_vec());
        self.reload();
    }

    pub fn reload(&self) {
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        self.0.cache.borrow_mut().clear();
        let sources = self.0.sources.borrow().clone();
        if sources.is_empty() {
            self.0.events.replace(Rc::new(Vec::new()));
            self.0.errors.borrow_mut().clear();
            self.0.loading.set(false);
            self.notify();
            return;
        }
        self.0.loading.set(true);
        let loader = self.0.loader.clone();
        let work: Work = Box::new(move || {
            let mut events = Vec::new();
            let mut errors: Vec<(String, String)> = Vec::new();
            for source in &sources {
                match loader(source) {
                    Ok(loaded) => events.extend(loaded),
                    Err(error) => {
                        let message = friendly_error(&error);
                        match errors.iter_mut().find(|(location, _)| *location == source.location) {
                            Some(entry) => entry.1 = message,
                            None => errors.push((source.location.clone(), message)),
                        }
                    }
                }
            }
            (events, errors)
        });
        let weak = Rc::downgrade(&self.0);
        let deliver: Deliver = Box::new(move |(events, errors)| {
            if let Some(inner) = weak.upgrade() {
                EventStore(inner).finished(generation, events, errors);
            }
        });
        (self.0.spawn)(work, deliver);
    }

    fn finished(&self, generation: u64, events: Vec<Event>, errors: Vec<(String, String)>) {
        if !self.0.closed.get() && generation == self.0.generation.get() {
            self.0.events.replace(Rc::new(events));
            self.0.errors.replace(errors);
            self.0.loading.set(false);
            self.0.cache.borrow_mut().clear();
            self.notify();
        }
    }

    /// Expansion of `first..=last` in the current local zone (empty where Python would raise).
    pub fn range(&self, first: Date, last: Date) -> Rc<Expansion> {
        if let Some(found) = self.0.cache.borrow().get(&(first, last)) {
            return found.clone();
        }
        let events = self.events();
        let Ok(result) = expand(&events, first, last, &crate::tz::local_zone()) else {
            return Rc::new(Expansion::new());
        };
        let result = Rc::new(result);
        let mut cache = self.0.cache.borrow_mut();
        if cache.len() > 8 {
            cache.clear();
        }
        cache.insert((first, last), result.clone());
        result
    }

    pub fn on(&self, day: Date) -> Vec<Occurrence> {
        self.range(day, day).get(&day).cloned().unwrap_or_default()
    }

    pub fn close(&self) {
        self.0.closed.set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::parse_calendar;

    #[test]
    fn loads_off_thread_and_reports_failures_per_source() {
        let delivered = Rc::new(Cell::new(0));
        let good = Source { kind: "file".into(), location: "/good.ics".into(), name: String::new() };
        let bad = Source { kind: "file".into(), location: "/missing.ics".into(), name: String::new() };
        let loader: Loader = Arc::new(|source: &Source| {
            if source.location == "/missing.ics" {
                return Err(LoadError::NotFound);
            }
            Ok(parse_calendar("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:X\r\nDTSTART;VALUE=DATE:20260101\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n", "").unwrap())
        });
        // A real worker thread; delivery runs synchronously like the Python test's schedule.
        let spawn: Spawner = Rc::new(|work: Work, deliver: Deliver| {
            let result = std::thread::spawn(work).join().unwrap();
            deliver(result);
        });
        let counter = delivered.clone();
        let store = EventStore::new(move || counter.set(counter.get() + 1), loader, spawn);
        store.set_sources(&[good.clone(), bad.clone()], false);
        assert!(!store.loading());
        assert_eq!(store.on(Date::ymd(2026, 1, 1)).iter().map(|o| o.summary.as_str()).collect::<Vec<_>>(), ["X"]);
        assert_eq!(store.errors(), vec![("/missing.ics".to_owned(), "File not found".to_owned())]);
        assert_eq!(delivered.get(), 1);
        store.set_sources(&[good, bad], false);
        assert_eq!(delivered.get(), 1, "unchanged sources are not reloaded");
        store.set_sources(&[], false);
        assert!(store.events().is_empty());
        assert_eq!(delivered.get(), 2);
    }

    #[test]
    fn stale_and_closed_loads_are_ignored() {
        let pending: Rc<RefCell<Vec<(Work, Deliver)>>> = Rc::default();
        let queue = pending.clone();
        let spawn: Spawner = Rc::new(move |work, deliver| queue.borrow_mut().push((work, deliver)));
        let loader: Loader = Arc::new(|_: &Source| Err(LoadError::Value("bad".into())));
        let store = EventStore::new(|| {}, loader, spawn);
        let a = Source { kind: "file".into(), location: "/a".into(), name: String::new() };
        store.set_sources(&[a.clone()], false);
        store.set_sources(&[a], true);
        let (first, second) = { let mut p = pending.borrow_mut(); let s = p.pop().unwrap(); (p.pop().unwrap(), s) };
        (first.1)((first.0)());
        assert!(store.loading(), "older generation ignored");
        store.close();
        (second.1)((second.0)());
        assert!(store.loading(), "closed store ignores results");
    }
}
