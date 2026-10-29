//! Explicit Home/Trash actions through Gio and the GVFS trash backend (port of
//! desktop_actions.py). Constructing anything here performs no action; the
//! UI must confirm permanent deletion before `empty_trash`. No local trash
//! paths are traversed: only top-level `trash:///` children are deleted and
//! GVFS removes trashed directories recursively.
//!
//! API: [`TrashEmptyOperation`] (`new(root, on_done)` — root must be the GVFS
//! trash root; [`TrashEmptyOperation::with_scheme`] for test roots —
//! `start`, `cancel`, `done`), [`DesktopActions`] (`open_home`, `open_trash`,
//! `trash_count`, `empty_trash`, `busy`, `close`) with an injectable URI
//! opener and trash root URI. Errors are Python's `str(error)` texts.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gio::prelude::*;
use suite_core::pyerr::python_error_text;

use super::app_catalog::{DoneFn, LaunchContext, LaunchError, Launcher};

struct Inner {
    root: gio::File,
    scheme: String,
    on_done: RefCell<Option<DoneFn>>,
    cancellable: gio::Cancellable,
    enumerator: RefCell<Option<gio::FileEnumerator>>,
    pending: RefCell<VecDeque<gio::FileInfo>>,
    done: Cell<bool>,
    started: Cell<bool>,
}

/// Delete only top-level `trash://` items, as `gio trash --empty` does.
#[derive(Clone)]
pub struct TrashEmptyOperation(Rc<Inner>);

impl TrashEmptyOperation {
    pub fn new(root: gio::File, on_done: DoneFn) -> Result<Self, String> {
        if root.uri().trim_end_matches('/') != "trash:" {
            return Err("Trash operations require the GVFS trash root.".into());
        }
        Ok(Self::build(root, "trash", on_done, None))
    }

    /// A root on another scheme (tests use a `file://` directory under
    /// `$TMPDIR`); children must stay on `scheme`.
    pub fn with_scheme(root: gio::File, scheme: &str, on_done: DoneFn, cancellable: Option<gio::Cancellable>) -> Result<Self, String> {
        if !root.has_uri_scheme(scheme) {
            return Err("Trash operations require the GVFS trash root.".into());
        }
        Ok(Self::build(root, scheme, on_done, cancellable))
    }

    fn build(root: gio::File, scheme: &str, on_done: DoneFn, cancellable: Option<gio::Cancellable>) -> Self {
        Self(Rc::new(Inner {
            root,
            scheme: scheme.into(),
            on_done: RefCell::new(Some(on_done)),
            cancellable: cancellable.unwrap_or_default(),
            enumerator: RefCell::new(None),
            pending: RefCell::new(VecDeque::new()),
            done: Cell::new(false),
            started: Cell::new(false),
        }))
    }

    pub fn done(&self) -> bool {
        self.0.done.get()
    }

    pub fn start(&self) -> &Self {
        if self.0.started.replace(true) {
            return self;
        }
        let this = self.clone();
        self.0.root.enumerate_children_async(
            "standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
            Some(&self.0.cancellable),
            move |result| match result {
                Ok(enumerator) => {
                    *this.0.enumerator.borrow_mut() = Some(enumerator);
                    this.next_batch();
                }
                Err(error) => this.complete(Some(python_error_text(&error))),
            },
        );
        self
    }

    pub fn cancel(&self) {
        self.0.cancellable.cancel();
    }

    fn next_batch(&self) {
        if self.cancelled() {
            return;
        }
        let Some(enumerator) = self.0.enumerator.borrow().clone() else { return };
        let this = self.clone();
        enumerator.next_files_async(64, glib::Priority::DEFAULT, Some(&self.0.cancellable), move |result| match result {
            Ok(files) if files.is_empty() => this.complete(None),
            Ok(files) => {
                this.0.pending.borrow_mut().extend(files);
                this.delete_next();
            }
            Err(error) => this.complete(Some(python_error_text(&error))),
        });
    }

    fn cancelled(&self) -> bool {
        if self.0.done.get() {
            return true;
        }
        if self.0.cancellable.is_cancelled() {
            self.complete(Some("Emptying the Trash was cancelled.".into()));
            return true;
        }
        false
    }

    fn delete_next(&self) {
        if self.cancelled() {
            return;
        }
        let next = self.0.pending.borrow_mut().pop_front();
        let Some(info) = next else {
            self.next_batch();
            return;
        };
        let name = info.name().to_string_lossy().into_owned();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            self.complete(Some("The Trash returned an invalid item.".into()));
            return;
        }
        let child = self.0.root.child(&name);
        if !child.has_uri_scheme(&self.0.scheme) {
            self.complete(Some("The Trash returned an invalid location.".into()));
            return;
        }
        let this = self.clone();
        child.delete_async(glib::Priority::DEFAULT, Some(&self.0.cancellable), move |result| {
            if let Err(error) = result {
                if !error.matches(gio::IOErrorEnum::NotFound) {
                    this.complete(Some(python_error_text(&error)));
                    return;
                }
            }
            this.delete_next();
        });
    }

    fn complete(&self, error: Option<String>) {
        if self.0.done.replace(true) {
            return;
        }
        self.0.pending.borrow_mut().clear();
        let on_done = self.0.on_done.borrow_mut().take();
        let enumerator = self.0.enumerator.borrow().clone();
        let Some(enumerator) = enumerator else {
            if let Some(on_done) = on_done {
                on_done(error);
            }
            return;
        };
        // Closing must still work after the operation's cancellable is set.
        enumerator.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |result| {
            let failure = error.or_else(|| result.err().map(|e| python_error_text(&e)));
            if let Some(on_done) = on_done {
                on_done(failure);
            }
        });
    }
}

/// `launch_uri(uri, context, on_done)` as DesktopActions uses it.
pub type UriOpener = Box<dyn Fn(&str, Option<Rc<dyn LaunchContext>>, DoneFn) -> Result<(), LaunchError>>;

pub struct DesktopActions {
    on_error: Option<Rc<dyn Fn(String)>>,
    operation: Rc<RefCell<Option<TrashEmptyOperation>>>,
    pub open_uri: UriOpener,
    /// `trash:///` (tests: a `file://` directory with `trash_scheme = "file"`).
    pub trash_uri: String,
    pub trash_scheme: String,
    /// `Path.home().as_uri()`.
    pub home_uri: String,
}

impl DesktopActions {
    pub fn new(on_error: Option<Rc<dyn Fn(String)>>) -> Self {
        let launcher = Rc::new(Launcher::default());
        Self::with_opener(on_error, Box::new(move |uri, context, done| launcher.launch_uri(uri, context, Some(done))))
    }

    pub fn with_opener(on_error: Option<Rc<dyn Fn(String)>>, open_uri: UriOpener) -> Self {
        Self {
            on_error,
            operation: Rc::new(RefCell::new(None)),
            open_uri,
            trash_uri: "trash:///".into(),
            trash_scheme: "trash".into(),
            home_uri: gio::File::for_path(suite_core::paths::home()).uri().to_string(),
        }
    }

    pub fn busy(&self) -> bool {
        self.operation.borrow().is_some()
    }

    fn open(&self, uri: &str, context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) {
        let on_error = self.on_error.clone();
        let on_done = Rc::new(RefCell::new(on_done));
        let finished = move |error: Option<String>| {
            if let Some(on_done) = on_done.borrow_mut().take() {
                on_done(error);
            } else if let (Some(error), Some(on_error)) = (error, &on_error) {
                on_error(error);
            }
        };
        let finished = Rc::new(RefCell::new(Some(finished)));
        let callback = finished.clone();
        let done: DoneFn = Box::new(move |error| {
            if let Some(f) = callback.borrow_mut().take() {
                f(error)
            }
        });
        if let Err(failure) = (self.open_uri)(uri, context, done) {
            if let Some(f) = finished.borrow_mut().take() {
                f(Some(failure.to_string()));
            }
        }
    }

    pub fn open_home(&self, context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) {
        let uri = self.home_uri.clone();
        self.open(&uri, context, on_done);
    }

    pub fn open_trash(&self, context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) {
        self.open("trash:///", context, on_done);
    }

    /// Read the GVFS root count asynchronously: `on_done(count, error)`.
    pub fn trash_count(&self, on_done: Box<dyn FnOnce(Option<u32>, Option<String>)>) -> gio::Cancellable {
        let cancellable = gio::Cancellable::new();
        gio::File::for_uri(&self.trash_uri).query_info_async(
            "trash::item-count",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
            Some(&cancellable),
            move |result| match result {
                Ok(info) if !info.has_attribute("trash::item-count") => {
                    on_done(None, Some("The Trash service does not provide its item count.".into()))
                }
                Ok(info) => on_done(Some(info.attribute_uint32("trash::item-count")), None),
                Err(error) => on_done(None, Some(python_error_text(&error))),
            },
        );
        cancellable
    }

    /// Call only after the user confirms the permanent Empty Trash action.
    /// `on_done(error)` runs on the main context; `Err` = RuntimeError text.
    pub fn empty_trash(&self, on_done: DoneFn) -> Result<TrashEmptyOperation, String> {
        if self.busy() {
            return Err("The Trash is already being emptied.".into());
        }
        let slot = self.operation.clone();
        let finished: DoneFn = Box::new(move |error| {
            slot.borrow_mut().take();
            on_done(error);
        });
        let root = gio::File::for_uri(&self.trash_uri);
        let operation = if self.trash_scheme == "trash" {
            TrashEmptyOperation::new(root, finished)?
        } else {
            TrashEmptyOperation::with_scheme(root, &self.trash_scheme, finished, None)?
        };
        *self.operation.borrow_mut() = Some(operation.clone());
        operation.start();
        Ok(operation)
    }

    pub fn close(&self) {
        let operation = self.operation.borrow().clone();
        if let Some(operation) = operation {
            operation.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testutil::TempDir;
    use std::time::Duration;

    fn run<T>(body: impl FnOnce(&glib::MainContext) -> T) -> T {
        let context = glib::MainContext::new();
        context.with_thread_default(|| body(&context)).unwrap()
    }

    fn collector() -> (Rc<RefCell<Vec<Option<String>>>>, DoneFn) {
        let results = Rc::new(RefCell::new(Vec::new()));
        let sink = results.clone();
        (results, Box::new(move |error| sink.borrow_mut().push(error)))
    }

    #[test]
    fn root_validation_prevents_traversal() {
        let (_r, done) = collector();
        assert_eq!(TrashEmptyOperation::new(gio::File::for_uri("file:///tmp"), done).err().unwrap(), "Trash operations require the GVFS trash root.");
        let (_r, done) = collector();
        assert!(TrashEmptyOperation::new(gio::File::for_uri("trash:///"), done).is_ok());
    }

    #[test]
    fn empty_deletes_top_level_items_without_following_symlinks() {
        let dir = TempDir::new();
        let trash = dir.join("trash");
        let outside = dir.join("outside");
        std::fs::create_dir_all(trash.join("directory")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "x").unwrap();
        std::fs::write(trash.join("file"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, trash.join("symlink")).unwrap();
        let (results, done) = collector();
        run(|context| {
            let operation = TrashEmptyOperation::with_scheme(gio::File::for_path(&trash), "file", done, None).unwrap();
            operation.start();
            operation.start();
            assert!(results.borrow().is_empty());
            assert!(suite_core::timer::wait_for(context, Duration::from_secs(10), || !results.borrow().is_empty()));
            assert!(operation.done());
        });
        assert_eq!(*results.borrow(), vec![None]);
        assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 0);
        assert!(outside.join("keep").exists(), "symlink targets are never traversed");
    }

    #[test]
    fn errors_stop_and_report_once() {
        let dir = TempDir::new();
        let trash = dir.join("trash");
        std::fs::create_dir_all(trash.join("full")).unwrap();
        std::fs::write(trash.join("full/inner"), "x").unwrap();
        let (results, done) = collector();
        run(|context| {
            TrashEmptyOperation::with_scheme(gio::File::for_path(&trash), "file", done, None).unwrap().start();
            suite_core::timer::wait_for(context, Duration::from_secs(10), || !results.borrow().is_empty());
            suite_core::timer::settle(context, Duration::from_millis(50));
        });
        assert_eq!(results.borrow().len(), 1);
        assert!(results.borrow()[0].as_deref().unwrap().starts_with("g-io-error-quark: "), "{:?}", results.borrow());
        assert!(trash.join("full/inner").exists(), "no recursive descent");
    }

    #[test]
    fn enumeration_failure_and_cancel_delete_nothing() {
        let dir = TempDir::new();
        let (results, done) = collector();
        run(|context| {
            TrashEmptyOperation::with_scheme(gio::File::for_path(dir.join("missing")), "file", done, None).unwrap().start();
            suite_core::timer::wait_for(context, Duration::from_secs(10), || !results.borrow().is_empty());
        });
        assert_eq!(results.borrow().len(), 1);
        assert!(results.borrow()[0].is_some());

        let trash = dir.join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        std::fs::write(trash.join("one"), "x").unwrap();
        let (results, done) = collector();
        run(|context| {
            let operation = TrashEmptyOperation::with_scheme(gio::File::for_path(&trash), "file", done, None).unwrap();
            operation.start();
            operation.cancel();
            suite_core::timer::wait_for(context, Duration::from_secs(10), || !results.borrow().is_empty());
        });
        assert!(results.borrow()[0].as_deref().unwrap().contains("cancelled"));
        assert!(trash.join("one").exists());
    }

    #[test]
    fn duplicate_empty_requests_are_blocked_until_async_completion() {
        let dir = TempDir::new();
        std::fs::write(dir.join("file"), "x").unwrap();
        let mut actions = DesktopActions::with_opener(None, Box::new(|_, _, _| Ok(())));
        actions.trash_uri = gio::File::for_path(dir.path()).uri().to_string();
        actions.trash_scheme = "file".into();
        let (results, done) = collector();
        run(|context| {
            actions.empty_trash(done).unwrap();
            assert!(actions.busy());
            let (_r, again) = collector();
            assert_eq!(actions.empty_trash(again).err().unwrap(), "The Trash is already being emptied.");
            suite_core::timer::wait_for(context, Duration::from_secs(10), || !results.borrow().is_empty());
        });
        assert!(!actions.busy());
        assert_eq!(*results.borrow(), vec![None]);
        let mut real = DesktopActions::with_opener(None, Box::new(|_, _, _| Ok(())));
        real.trash_uri = "file:///not-trash".into();
        let (_r, done) = collector();
        assert!(real.empty_trash(done).is_err(), "the default scheme insists on trash:///");
        assert!(!real.busy());
    }

    #[test]
    fn trash_count_reports_missing_metadata() {
        let dir = TempDir::new();
        let mut actions = DesktopActions::with_opener(None, Box::new(|_, _, _| Ok(())));
        actions.trash_uri = gio::File::for_path(dir.path()).uri().to_string();
        let results = Rc::new(RefCell::new(Vec::new()));
        let sink = results.clone();
        run(|context| {
            actions.trash_count(Box::new(move |count, error| sink.borrow_mut().push((count, error))));
            suite_core::timer::wait_for(context, Duration::from_secs(10), || !results.borrow().is_empty());
        });
        assert_eq!(*results.borrow(), vec![(None, Some("The Trash service does not provide its item count.".to_string()))]);
    }

    #[test]
    fn open_home_and_trash_delegate_only_expected_uris_and_report_errors() {
        let uris = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = uris.clone();
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let error_sink = errors.clone();
        let actions = DesktopActions::with_opener(
            Some(Rc::new(move |e| error_sink.borrow_mut().push(e))),
            Box::new(move |uri, _context, done| {
                sink.borrow_mut().push(uri.to_owned());
                done(if uri == "trash:///" { Some("Start failed".into()) } else { None });
                Ok(())
            }),
        );
        actions.open_home(None, None);
        actions.open_trash(None, None);
        assert!(uris.borrow()[0].starts_with("file:///"));
        assert_eq!(uris.borrow()[1], "trash:///");
        assert_eq!(*errors.borrow(), vec!["Start failed".to_string()]);
        let (finished, done) = collector();
        actions.open_home(None, Some(done));
        assert_eq!(*finished.borrow(), vec![None]);

        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let error_sink = errors.clone();
        let failing = DesktopActions::with_opener(
            Some(Rc::new(move |e| error_sink.borrow_mut().push(e))),
            Box::new(|_, _, _| Err(LaunchError::Glib(crate::core::spawn::error("Unavailable")))),
        );
        failing.open_home(None, None);
        assert!(errors.borrow()[0].contains("Unavailable"));
    }
}
