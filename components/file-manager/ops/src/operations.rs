//! Asynchronous, cancellable GIO file operations for Wayfiles (file_operations.py).
//!
//! All filesystem work runs in one worker thread per job. UI callbacks run on
//! the GLib main context that was thread-default when the job was started.
//! Conflicts pause only the worker, never the GTK thread. No KDE programs,
//! shell expansion, or subprocess file utilities are used.

use crate::error::{Error, Result};
use crate::{fsx, pyos, Target};
use gio::prelude::*;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use suite_core::tr;

pub const FLAGS: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;
pub const COPY_FLAGS: gio::FileCopyFlags =
    gio::FileCopyFlags::NOFOLLOW_SYMLINKS.union(gio::FileCopyFlags::ALL_METADATA);
pub const INFO: &str = "standard::name,standard::type,standard::size,standard::symlink-target,\
time::modified,time::modified-usec,etag::value,id::file";
/// Progress updates closer together than this are dropped (unless forced).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(75);
/// In-session undo history bound.
pub const HISTORY_LIMIT: usize = 20;

/// `as_file(value)`: a `Gio.File`, a path, or a string (URI when it contains `://`).
pub fn as_file(value: impl Into<Target>) -> gio::File {
    match value.into() {
        Target::File(file) => file,
        Target::Path(path) => gio::File::for_path(path),
        Target::Text(text) if text.contains("://") => gio::File::for_uri(&text),
        Target::Text(text) => gio::File::for_path(text),
    }
}

fn none() -> Option<&'static gio::Cancellable> {
    None
}

fn is_cancelled(error: &Error) -> bool {
    error.matches_io(gio::IOErrorEnum::Cancelled)
}

fn exists(file: &gio::File, cancellable: Option<&gio::Cancellable>) -> Result<bool> {
    match file.query_info("standard::type", FLAGS, cancellable) {
        Ok(_) => Ok(true),
        Err(error) if error.matches(gio::IOErrorEnum::NotFound) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn children(file: &gio::File, cancellable: Option<&gio::Cancellable>) -> Result<Vec<(gio::File, gio::FileInfo)>> {
    let enumerator = file.enumerate_children(INFO, FLAGS, cancellable)?;
    let mut result = Vec::new();
    let outcome = loop {
        match enumerator.next_file(cancellable) {
            Ok(Some(info)) => result.push((file.child(info.name()), info)),
            Ok(None) => break Ok(()),
            Err(error) => break Err(Error::from(error)),
        }
    };
    let _ = enumerator.close(none());
    outcome.map(|_| result)
}

/// Metadata snapshot (`_fingerprint`), including descendants but never symlink targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub file_type: i32,
    pub size: i64,
    pub symlink: Option<PathBuf>,
    pub modified: u64,
    pub modified_usec: u32,
    pub etag: Option<String>,
    pub id: Option<String>,
    /// Sorted `(basename, fingerprint)` of a directory's children.
    pub children: Option<Vec<(OsString, Fingerprint)>>,
}

/// `_fingerprint(file)`.
pub fn fingerprint(file: &gio::File) -> Result<Fingerprint> {
    let info = file.query_info(INFO, FLAGS, none())?;
    let symlink = if info.has_attribute("standard::symlink-target") { info.symlink_target() } else { None };
    let file_type = info.file_type();
    let mut result = Fingerprint {
        file_type: file_type.into_glib_value(),
        size: info.size(),
        symlink,
        modified: info.attribute_uint64("time::modified"),
        modified_usec: info.attribute_uint32("time::modified-usec"),
        etag: info.attribute_string("etag::value").map(|s| s.to_string()),
        id: info.attribute_string("id::file").map(|s| s.to_string()),
        children: None,
    };
    if file_type == gio::FileType::Directory {
        let mut entries = Vec::new();
        for (child, _) in children(file, None)? {
            let name = child.basename().map(PathBuf::into_os_string).unwrap_or_default();
            entries.push((name, fingerprint(&child)?));
        }
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        result.children = Some(entries);
    }
    Ok(result)
}

trait GlibValue {
    fn into_glib_value(self) -> i32;
}
impl GlibValue for gio::FileType {
    fn into_glib_value(self) -> i32 {
        use glib::translate::IntoGlib;
        self.into_glib()
    }
}

/// `(id::file, type, size, mtime, mtime-usec)`: GVfs preserves the native file
/// id when exposing an item in Trash; names and original paths alone are not identities.
pub type TrashIdentity = (String, i32, i64, u64, u32);

fn trash_identity(info: &gio::FileInfo) -> Option<TrashIdentity> {
    let ident = info.attribute_string("id::file")?;
    Some((ident.to_string(), info.file_type().into_glib_value(), info.size(),
          info.attribute_uint64("time::modified"), info.attribute_uint32("time::modified-usec")))
}

/// Mount GVfs Trash on a private context of this worker, without blocking GTK (10 s limit).
fn mount_trash(trash: &gio::File, cancellable: &gio::Cancellable) -> Result<()> {
    let context = glib::MainContext::new();
    let main_loop = glib::MainLoop::new(Some(&context), false);
    let pending = gio::Cancellable::new();
    let state: std::rc::Rc<std::cell::RefCell<Option<std::result::Result<(), glib::Error>>>> = Default::default();
    let outcome = context.with_thread_default(|| {
        let timer_pending = pending.clone();
        let timer = glib::timeout_source_new_seconds(10, None, glib::Priority::DEFAULT, move || {
            timer_pending.cancel();
            glib::ControlFlow::Break
        });
        timer.attach(Some(&context));
        let forward = pending.clone();
        let handler = cancellable.connect_cancelled(move |_| forward.cancel());
        let done = state.clone();
        let quit = main_loop.clone();
        trash.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&gio::MountOperation::new()), Some(&pending),
                                     move |result| {
                                         *done.borrow_mut() = Some(result);
                                         quit.quit();
                                     });
        if state.borrow().is_none() {
            main_loop.run();
        }
        timer.destroy();
        if let Some(handler) = handler {
            cancellable.disconnect_cancelled(handler);
        }
    });
    if outcome.is_err() {
        return Err(Error::runtime("The worker main context could not be acquired"));
    }
    cancellable.set_error_if_cancelled()?;
    let finished = state.borrow_mut().take();
    match finished {
        Some(Err(error)) if !error.matches(gio::IOErrorEnum::AlreadyMounted) => Err(error.into()),
        _ => Ok(()),
    }
}

/// Resolve exactly the object trashed by this job, on any GVfs volume.
fn trashed_item(original: &gio::File, identity: Option<&TrashIdentity>, cancellable: &gio::Cancellable) -> Result<gio::File> {
    let Some(identity) = identity else {
        return Err(Error::runtime(tr!("This Trash backend cannot identify the deleted item. Restore it from Trash.")));
    };
    let trash = gio::File::for_uri("trash:///");
    let attributes = format!("{INFO},trash::orig-path");
    let enumerator = match trash.enumerate_children(&attributes, FLAGS, Some(cancellable)) {
        Ok(enumerator) => enumerator,
        Err(error) if error.matches(gio::IOErrorEnum::NotMounted) => {
            mount_trash(&trash, cancellable)?;
            trash.enumerate_children(&attributes, FLAGS, Some(cancellable))?
        }
        Err(error) => return Err(error.into()),
    };
    let mut matches = Vec::new();
    let outcome = loop {
        match enumerator.next_file(Some(cancellable)) {
            Ok(Some(info)) => {
                if let Some(path) = info.attribute_byte_string("trash::orig-path") {
                    if !path.is_empty() && gio::File::for_path(path.as_str()).equal(original)
                        && trash_identity(&info).as_ref() == Some(identity) {
                        matches.push(trash.child(info.name()));
                    }
                }
            }
            Ok(None) => break Ok(()),
            Err(error) => break Err(Error::from(error)),
        }
    };
    let _ = enumerator.close(none());
    outcome?;
    if matches.len() != 1 {
        return Err(Error::runtime(tr!("The deleted item is no longer uniquely identifiable in Trash; nothing was restored.")));
    }
    Ok(matches.remove(0))
}

/// Remove a known operation-created temporary tree, without following links.
fn remove_owned(file: &gio::File) -> Result<()> {
    if !exists(file, None)? {
        return Ok(());
    }
    let info = file.query_info("standard::type", FLAGS, none())?;
    if info.file_type() == gio::FileType::Directory {
        for (child, _) in children(file, None)? {
            remove_owned(&child)?;
        }
    }
    file.delete(none())?;
    Ok(())
}

fn basename_checked(value: &str) -> Result<&str> {
    if value.is_empty() || value == "." || value == ".." || value.contains('/') || value.contains('\0') {
        return Err(Error::value(tr!("Enter a file name without directory separators.")));
    }
    Ok(value)
}

fn uri(file: &gio::File) -> String {
    file.uri().to_string()
}

/// Progress of a job (URIs, bytes of the current file, top-level item counts).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub kind: String,
    pub source: String,
    pub destination: String,
    pub current_bytes: i64,
    pub total_bytes: i64,
    pub completed_items: usize,
    pub total_items: usize,
}

/// A destination that already exists. `choices` are the accepted answers.
#[derive(Clone, Debug)]
pub struct Conflict {
    pub source: gio::File,
    pub destination: gio::File,
    pub is_directory: bool,
    pub can_merge: bool,
    pub choices: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub source: String,
    pub message: String,
}

/// Python `Result`: completed/skipped URIs, failures, cancellation, undo availability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationResult {
    pub kind: String,
    pub completed: Vec<String>,
    pub failures: Vec<Failure>,
    pub skipped: Vec<String>,
    pub cancelled: bool,
    pub undo_available: bool,
}

impl OperationResult {
    fn new(kind: &str) -> Self {
        OperationResult { kind: kind.to_string(), ..Default::default() }
    }
    /// No failures and not cancelled (skipped items are permitted).
    pub fn success(&self) -> bool {
        self.failures.is_empty() && !self.cancelled
    }
}

/// Answer a conflict: `resolve("skip" | "replace" | "rename" | "merge" | "cancel", name)`.
/// Only the first answer counts; it may be called from any thread.
pub type Resolve = Arc<dyn Fn(&str, Option<&str>) + Send + Sync>;

/// Main-thread callbacks of a job (none required).
#[derive(Default)]
pub struct Callbacks {
    pub on_progress: Option<Box<dyn Fn(&Progress)>>,
    pub on_conflict: Option<Box<dyn Fn(&Conflict, Resolve)>>,
    pub on_done: Option<Box<dyn FnOnce(&OperationResult)>>,
}

impl Callbacks {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn on_progress(mut self, callback: impl Fn(&Progress) + 'static) -> Self {
        self.on_progress = Some(Box::new(callback));
        self
    }
    pub fn on_conflict(mut self, callback: impl Fn(&Conflict, Resolve) + 'static) -> Self {
        self.on_conflict = Some(Box::new(callback));
        self
    }
    pub fn on_done(mut self, callback: impl FnOnce(&OperationResult) + 'static) -> Self {
        self.on_done = Some(Box::new(callback));
        self
    }
}

enum Event {
    Progress(Progress),
    Conflict(Conflict, Resolve),
    Done(OperationResult),
}

#[derive(Default)]
struct ConflictState {
    ready: bool,
    answer: Option<(String, Option<String>)>,
}

struct JobShared {
    kind: String,
    sources: Vec<gio::File>,
    destination: Option<gio::File>,
    cancellable: gio::Cancellable,
    finished: Mutex<Option<OperationResult>>,
    finished_cv: Condvar,
    conflict: Mutex<ConflictState>,
    conflict_cv: Condvar,
}

/// A running operation. Keep the application alive until `on_done` fires.
#[derive(Clone)]
pub struct Job {
    shared: Arc<JobShared>,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}
impl Eq for Job {}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job").field("kind", &self.shared.kind).field("running", &self.running()).finish()
    }
}

impl Job {
    pub fn kind(&self) -> &str {
        &self.shared.kind
    }
    pub fn sources(&self) -> &[gio::File] {
        &self.shared.sources
    }
    pub fn destination(&self) -> Option<&gio::File> {
        self.shared.destination.as_ref()
    }
    pub fn cancellable(&self) -> &gio::Cancellable {
        &self.shared.cancellable
    }
    pub fn running(&self) -> bool {
        self.shared.finished.lock().unwrap().is_none()
    }
    /// Cancel; also wakes a pending conflict.
    pub fn cancel(&self) {
        self.shared.cancellable.cancel();
        let mut state = self.shared.conflict.lock().unwrap();
        state.ready = true;
        self.shared.conflict_cv.notify_all();
    }
    /// For shutdown/tests; do not block GTK while a conflict needs input.
    /// `None` = wait forever. Returns `None` on timeout.
    pub fn wait(&self, timeout: Option<Duration>) -> Option<OperationResult> {
        let guard = self.shared.finished.lock().unwrap();
        match timeout {
            None => self.shared.finished_cv.wait_while(guard, |r| r.is_none()).unwrap().clone(),
            Some(timeout) => self.shared.finished_cv.wait_timeout_while(guard, timeout, |r| r.is_none())
                .unwrap().0.clone(),
        }
    }
    /// The finished result, if any (non-blocking).
    pub fn result(&self) -> Option<OperationResult> {
        self.shared.finished.lock().unwrap().clone()
    }
    fn resolver(&self) -> Resolve {
        let shared = Arc::downgrade(&self.shared);
        Arc::new(move |decision: &str, name: Option<&str>| {
            let Some(shared) = shared.upgrade() else { return };
            let mut state = shared.conflict.lock().unwrap();
            if state.answer.is_some() || state.ready {
                return;
            }
            state.answer = Some((decision.to_string(), name.map(str::to_string)));
            state.ready = true;
            shared.conflict_cv.notify_all();
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UndoKind {
    Mkdir,
    Move,
    Remove,
    Trash,
    Batch,
}

#[derive(Clone, Debug)]
struct UndoRecord {
    id: u64,
    kind: UndoKind,
    current: gio::File,
    original: Option<gio::File>,
    fingerprint: Option<Fingerprint>,
    identity: Option<TrashIdentity>,
    batch: Vec<(gio::File, gio::File, Fingerprint)>,
}

fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl UndoRecord {
    fn new(kind: UndoKind, current: &gio::File, original: Option<&gio::File>, fingerprint: Option<Fingerprint>) -> Self {
        UndoRecord { id: next_id(), kind, current: current.clone(), original: original.cloned(), fingerprint,
                     identity: None, batch: Vec::new() }
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct TestHooks {
    /// Replaces `Job._copy_tree` (`patch.object(Job, "_copy_tree", …)`).
    pub copy_tree: Option<Arc<dyn Fn(&Job, &gio::File, &gio::File) -> Result<()> + Send + Sync>>,
    /// Replaces `Gio.File.trash`.
    pub trash: Option<Arc<dyn Fn(&gio::File) -> Result<()> + Send + Sync>>,
    /// Replaces `Job._notify`.
    pub notify: Option<Arc<dyn Fn(&Job) + Send + Sync>>,
    /// Set when `Job._delete` runs.
    pub delete_called: Arc<std::sync::atomic::AtomicBool>,
}

struct OpsInner {
    jobs: Mutex<Vec<Job>>,
    history: Mutex<Vec<Vec<UndoRecord>>>,
    #[cfg(test)]
    hooks: Mutex<TestHooks>,
}

/// Own operation jobs and a bounded in-session undo history.
#[derive(Clone)]
pub struct Operations {
    inner: Arc<OpsInner>,
}

impl Default for Operations {
    fn default() -> Self {
        Self::new()
    }
}

impl Operations {
    pub fn new() -> Self {
        Operations { inner: Arc::new(OpsInner { jobs: Mutex::new(Vec::new()), history: Mutex::new(Vec::new()),
                                                #[cfg(test)]
                                                hooks: Mutex::new(TestHooks::default()) }) }
    }

    #[cfg(test)]
    pub(crate) fn set_hooks(&self, hooks: TestHooks) {
        *self.inner.hooks.lock().unwrap() = hooks;
    }

    /// Running jobs.
    pub fn jobs(&self) -> Vec<Job> {
        self.inner.jobs.lock().unwrap().clone()
    }

    pub fn can_undo(&self) -> bool {
        !self.inner.history.lock().unwrap().is_empty()
    }

    fn remember(&self, records: Vec<UndoRecord>) {
        let mut history = self.inner.history.lock().unwrap();
        history.push(records);
        let excess = history.len().saturating_sub(HISTORY_LIMIT);
        history.drain(..excess);
    }

    /// Start `kind` (`copy move link duplicate rename mkdir touch trash delete restore`).
    /// Must be called on the thread that owns the thread-default main context when
    /// any callback is given (the GTK thread).
    pub fn start(&self, kind: &str, sources: &[gio::File], destination: Option<&gio::File>,
                 callbacks: Callbacks) -> Job {
        self.launch(kind, sources.to_vec(), destination.cloned(), callbacks, Vec::new(), Vec::new())
    }

    /// Undo the most recent operation group; `None` if history is empty.
    pub fn undo(&self, callbacks: Callbacks) -> Option<Job> {
        let records = self.inner.history.lock().unwrap().pop()?;
        Some(self.launch("undo", Vec::new(), None, callbacks, records, Vec::new()))
    }

    /// Preflight and stage a batch of same-folder renames (swaps supported).
    pub fn rename_many(&self, pairs: Vec<(gio::File, gio::File)>, callbacks: Callbacks) -> Job {
        let sources = pairs.iter().map(|(source, _)| source.clone()).collect();
        self.launch("batch_rename", sources, None, callbacks, Vec::new(), pairs)
    }

    pub fn cancel_all(&self) {
        for job in self.jobs() {
            job.cancel();
        }
    }

    fn launch(&self, kind: &str, sources: Vec<gio::File>, destination: Option<gio::File>, callbacks: Callbacks,
              undo_records: Vec<UndoRecord>, rename_pairs: Vec<(gio::File, gio::File)>) -> Job {
        let job = Job { shared: Arc::new(JobShared {
            kind: kind.to_string(), sources, destination, cancellable: gio::Cancellable::new(),
            finished: Mutex::new(None), finished_cv: Condvar::new(),
            conflict: Mutex::new(ConflictState::default()), conflict_cv: Condvar::new(),
        }) };
        let has_progress = callbacks.on_progress.is_some();
        let has_conflict = callbacks.on_conflict.is_some();
        let has_done = callbacks.on_done.is_some();
        let (sender, receiver) = async_channel::unbounded::<Event>();
        if has_progress || has_conflict || has_done {
            let context = glib::MainContext::ref_thread_default();
            let cancellable = job.shared.cancellable.clone();
            let Callbacks { on_progress, on_conflict, mut on_done } = callbacks;
            context.spawn_local(async move {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        Event::Progress(progress) => {
                            if let Some(callback) = &on_progress {
                                callback(&progress);
                            }
                        }
                        Event::Conflict(conflict, resolve) => {
                            if cancellable.is_cancelled() {
                                resolve("cancel", None);
                            } else if let Some(callback) = &on_conflict {
                                let answer = resolve.clone();
                                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    callback(&conflict, resolve)
                                }));
                                if outcome.is_err() {
                                    answer("cancel", None);
                                }
                            }
                        }
                        Event::Done(result) => {
                            if let Some(callback) = on_done.take() {
                                callback(&result);
                            }
                            break;
                        }
                    }
                }
            });
        }
        self.inner.jobs.lock().unwrap().push(job.clone());
        let mut worker = Worker {
            job: job.clone(), ops: self.clone(), sender, has_progress, has_conflict, has_done,
            result: OperationResult::new(kind), undo: Vec::new(),
            undo_remaining: undo_records.clone(), undo_records, rename_pairs,
            replaced: false, last_progress: None,
            #[cfg(test)]
            hooks: self.inner.hooks.lock().unwrap().clone(),
        };
        std::thread::Builder::new().name(format!("Wayfiles-{kind}"))
            .spawn(move || worker.run())
            .expect("spawn file operation worker");
        job
    }
}

struct Worker {
    job: Job,
    ops: Operations,
    sender: async_channel::Sender<Event>,
    has_progress: bool,
    has_conflict: bool,
    has_done: bool,
    result: OperationResult,
    undo: Vec<UndoRecord>,
    undo_records: Vec<UndoRecord>,
    undo_remaining: Vec<UndoRecord>,
    rename_pairs: Vec<(gio::File, gio::File)>,
    replaced: bool,
    last_progress: Option<Instant>,
    #[cfg(test)]
    hooks: TestHooks,
}

impl Worker {
    fn kind(&self) -> &str {
        &self.job.shared.kind
    }

    fn cancellable(&self) -> gio::Cancellable {
        self.job.shared.cancellable.clone()
    }

    fn check(&self) -> Result<()> {
        self.job.shared.cancellable.set_error_if_cancelled().map_err(Error::from)
    }

    fn notify(&mut self, source: Option<&gio::File>, destination: Option<&gio::File>, current: i64, total: i64,
              force: bool) {
        #[cfg(test)]
        if let Some(hook) = self.hooks.notify.clone() {
            hook(&self.job);
            return;
        }
        let now = Instant::now();
        if !force && self.last_progress.is_some_and(|last| now.duration_since(last) < PROGRESS_INTERVAL) {
            return;
        }
        self.last_progress = Some(now);
        if self.has_progress {
            let state = Progress {
                kind: self.kind().to_string(),
                source: source.map(uri).unwrap_or_default(),
                destination: destination.map(uri).unwrap_or_default(),
                current_bytes: current,
                total_bytes: total,
                completed_items: self.result.completed.len(),
                total_items: self.job.shared.sources.len(),
            };
            let _ = self.sender.try_send(Event::Progress(state));
        }
    }

    fn ask(&mut self, source: &gio::File, destination: &gio::File) -> Result<(String, Option<String>)> {
        if !self.has_conflict {
            return Ok(("skip".into(), None));
        }
        {
            let mut state = self.job.shared.conflict.lock().unwrap();
            *state = ConflictState::default();
        }
        let cancellable = self.cancellable();
        let src_dir = source.query_file_type(FLAGS, Some(&cancellable)) == gio::FileType::Directory;
        let dst_dir = destination.query_file_type(FLAGS, Some(&cancellable)) == gio::FileType::Directory;
        let choices = if src_dir && dst_dir {
            vec!["skip", "replace", "rename", "merge", "cancel"]
        } else {
            vec!["skip", "replace", "rename", "cancel"]
        };
        let conflict = Conflict { source: source.clone(), destination: destination.clone(), is_directory: src_dir,
                                  can_merge: src_dir && dst_dir, choices: choices.clone() };
        if self.sender.try_send(Event::Conflict(conflict, self.job.resolver())).is_err() {
            (self.job.resolver())("cancel", None);
        }
        let answer = {
            let state = self.job.shared.conflict.lock().unwrap();
            let state = self.job.shared.conflict_cv.wait_while(state, |s| !s.ready).unwrap();
            state.answer.clone()
        };
        self.check()?;
        let (decision, name) = answer.unwrap_or_else(|| ("cancel".into(), None));
        if !choices.contains(&decision.as_str()) {
            return Err(Error::value(tr!("Invalid conflict decision.")));
        }
        if decision == "cancel" {
            self.job.cancel();
            self.check()?;
        }
        Ok((decision, name))
    }

    fn unique(&self, parent: &gio::File, name: &[u8], suffix: &str) -> Result<gio::File> {
        let (mut stem, mut extension) = splitext_bytes(name);
        if name.starts_with(b".") && stem.is_empty() {
            stem = name;
            extension = b"";
        }
        let cancellable = self.cancellable();
        let mut number = 1u64;
        loop {
            let tag = if number == 1 { suffix.to_string() } else { format!("{suffix} {number}") };
            let mut candidate_name = stem.to_vec();
            candidate_name.extend_from_slice(format!(" ({tag})").as_bytes());
            candidate_name.extend_from_slice(extension);
            let candidate = parent.child(OsString::from_vec(candidate_name));
            if !exists(&candidate, Some(&cancellable))? {
                return Ok(candidate);
            }
            number += 1;
        }
    }

    fn guard(&self, source: &gio::File, destination: &gio::File) -> Result<()> {
        let message = || Error::value(tr!("A folder cannot be copied or moved into itself."));
        if source.equal(destination) || destination.has_prefix(source) {
            return Err(message());
        }
        // GIO URI prefix checks do not resolve symlinks in the destination's
        // parent. Local checks close that loophole without following a source
        // symlink that is itself being copied as a link.
        if let (Some(src_path), Some(dst_path)) = (source.path(), destination.path()) {
            let is_link = std::fs::symlink_metadata(&src_path).is_ok_and(|m| m.file_type().is_symlink());
            if !is_link {
                let src_real = pyos::realpath(&src_path);
                let dst_real = pyos::realpath(&pyos::dirname(&dst_path)).join(pyos::basename(&dst_path));
                let dst_real = PathBuf::from(OsString::from_vec(pyos::normpath(dst_real.as_os_str().as_bytes())));
                if dst_real == src_real || (src_path.is_dir() && pyos::commonpath(&src_real, &dst_real) == src_real) {
                    return Err(message());
                }
            }
        }
        Ok(())
    }

    fn temporary(&self, destination: &gio::File, purpose: &str) -> Result<gio::File> {
        let Some(parent) = destination.parent() else {
            return Err(Error::value(tr!("The filesystem root cannot be replaced.")));
        };
        Ok(parent.child(format!(".wayfiles-{purpose}-{}", fsx::uuid_hex())))
    }

    fn copy_tree(&mut self, source: &gio::File, destination: &gio::File) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = self.hooks.copy_tree.clone() {
            return hook(&self.job, source, destination);
        }
        self.check()?;
        let cancellable = self.cancellable();
        let info = source.query_info(INFO, FLAGS, Some(&cancellable))?;
        if info.file_type() == gio::FileType::Directory {
            destination.make_directory(Some(&cancellable))?;
            for (child, child_info) in children(source, Some(&cancellable))? {
                self.copy_tree(&child, &destination.child(child_info.name()))?;
            }
            // Preserve directory metadata after its children (their creation
            // updates timestamps). Some remote backends cannot set all fields.
            if let Err(error) = source.copy_attributes(destination, COPY_FLAGS, Some(&cancellable)) {
                if error.matches(gio::IOErrorEnum::Cancelled) {
                    return Err(error.into());
                }
            }
        } else {
            let (src, dst) = (source.clone(), destination.clone());
            source.copy(destination, COPY_FLAGS, Some(&cancellable),
                        Some(&mut |current, total| self.notify(Some(&src), Some(&dst), current, total, false)))?;
        }
        Ok(())
    }

    fn atomic_copy(&mut self, source: &gio::File, destination: &gio::File) -> Result<()> {
        let temporary = self.temporary(destination, "copy")?;
        let cancellable = self.cancellable();
        let outcome = (|| {
            self.copy_tree(source, &temporary)?;
            self.check()?;
            temporary.move_(destination, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, Some(&cancellable), None)?;
            Ok(())
        })();
        if exists(&temporary, None)? {
            remove_owned(&temporary)?;
        }
        outcome
    }

    fn move_item(&mut self, source: &gio::File, destination: &gio::File) -> Result<()> {
        let cancellable = self.cancellable();
        if let Err(error) = source.move_(destination, COPY_FLAGS | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE,
                                        Some(&cancellable), None) {
            if !(error.matches(gio::IOErrorEnum::NotSupported) || error.matches(gio::IOErrorEnum::WouldRecurse)) {
                return Err(error.into());
            }
            // Remote/cross-filesystem directory moves need recursive copying.
            // Cancellation is accepted before deleting the original, so a
            // cancelled copy never deletes part of its source.
            let before = fingerprint(source)?;
            self.atomic_copy(source, destination)?;
            if before != fingerprint(source)? {
                return Err(Error::runtime(tr!("The source changed while moving; both copies were kept.")));
            }
            if cancellable.is_cancelled() {
                remove_owned(destination)?;
                self.check()?;
            }
            remove_owned(source)?;
        }
        Ok(())
    }

    fn transfer(&mut self, source: &gio::File, destination: &gio::File, moving: bool, link: bool) -> Result<bool> {
        self.check()?;
        self.guard(source, destination)?;
        let cancellable = self.cancellable();
        let mut backup = None;
        if exists(destination, Some(&cancellable))? {
            let (decision, new_name) = self.ask(source, destination)?;
            match decision.as_str() {
                "skip" => {
                    self.result.skipped.push(uri(source));
                    return Ok(false);
                }
                "rename" => {
                    let parent = destination.parent().ok_or_else(|| Error::value(tr!("The filesystem root cannot be replaced.")))?;
                    let target = match new_name.as_deref() {
                        Some(name) if !name.is_empty() => parent.child(basename_checked(name)?),
                        _ => {
                            let name = destination.basename().map(PathBuf::into_os_string).unwrap_or_default();
                            self.unique(&parent, name.as_bytes(), "copy")?
                        }
                    };
                    return self.transfer(source, &target, moving, link);
                }
                "merge" => {
                    let mut successful = true;
                    for (child, info) in children(source, Some(&cancellable))? {
                        successful = self.transfer(&child, &destination.child(info.name()), moving, link)? && successful;
                    }
                    if moving && successful {
                        source.delete(Some(&cancellable))?;
                        // An emptied merge source directory must be restored before
                        // reverse child moves. See the undo directory record.
                        self.undo.push(UndoRecord::new(UndoKind::Mkdir, source, None, None));
                    }
                    return Ok(successful);
                }
                _ => {
                    let saved = self.temporary(destination, "replaced")?;
                    destination.move_(&saved, COPY_FLAGS | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE,
                                      Some(&cancellable), None)?;
                    backup = Some(saved);
                }
            }
        }
        let outcome = (|| -> Result<()> {
            self.notify(Some(source), Some(destination), 0, 0, true);
            if link {
                let Some(path) = source.path() else {
                    return Err(Error::value(tr!("Symbolic links require a local source.")));
                };
                destination.make_symbolic_link(&path, Some(&cancellable))?;
            } else if moving {
                self.move_item(source, destination)?;
            } else {
                self.atomic_copy(source, destination)?;
            }
            let snapshot = fingerprint(destination)?;
            // Moving an item back to trash needs trash(), not move() to its old
            // backend-owned URI. Restored entries therefore have no move undo.
            if self.kind() != "restore" {
                let kind = if moving { UndoKind::Move } else { UndoKind::Remove };
                self.undo.push(UndoRecord::new(kind, destination, if moving { Some(source) } else { None },
                                               Some(snapshot)));
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            if let Some(saved) = &backup {
                if !exists(destination, None)? {
                    saved.move_(destination, COPY_FLAGS, none(), None)?;
                } else {
                    // Never remove a destination that might have been created
                    // by another process while we were working.
                    self.result.failures.push(Failure {
                        source: uri(saved),
                        message: tr!("The previous destination is preserved here; restore it manually."),
                    });
                }
            }
            return Err(error);
        }
        if let Some(saved) = backup {
            self.replaced = true;
            remove_owned(&saved)?;
        }
        Ok(true)
    }

    fn delete(&mut self, source: &gio::File) -> Result<()> {
        #[cfg(test)]
        self.hooks.delete_called.store(true, std::sync::atomic::Ordering::SeqCst);
        self.check()?;
        let cancellable = self.cancellable();
        if source.query_file_type(FLAGS, Some(&cancellable)) == gio::FileType::Directory {
            for (child, _) in children(source, Some(&cancellable))? {
                self.delete(&child)?;
            }
        }
        source.delete(Some(&cancellable))?;
        Ok(())
    }

    /// Plan first, then stage every source before committing any target.
    fn rename_batch(&mut self, pairs: &[(gio::File, gio::File)], record_undo: bool) -> Result<()> {
        let pairs: Vec<(gio::File, gio::File)> = pairs.iter().filter(|(s, t)| !s.equal(t)).cloned().collect();
        if pairs.is_empty() {
            return Ok(());
        }
        let sources: std::collections::HashSet<String> = pairs.iter().map(|(s, _)| uri(s)).collect();
        let targets: std::collections::HashSet<String> = pairs.iter().map(|(_, t)| uri(t)).collect();
        if sources.len() != pairs.len() || targets.len() != pairs.len() {
            return Err(Error::value(tr!("Every source and destination in a rename batch must be unique.")));
        }
        let cancellable = self.cancellable();
        for (source, target) in &pairs {
            self.check()?;
            let same_parent = match (source.parent(), target.parent()) {
                (Some(a), Some(b)) => a.equal(&b),
                _ => false,
            };
            if !same_parent {
                return Err(Error::value(tr!("Batch rename keeps each item in its existing folder.")));
            }
            if !exists(source, Some(&cancellable))? {
                return Err(Error::value(tr!("The source no longer exists: {name}", name = source.parse_name().as_str())));
            }
            if exists(target, Some(&cancellable))? && !sources.contains(&uri(target)) {
                return Err(Error::value(tr!("The new name is already in use: {name}", name = target.parse_name().as_str())));
            }
        }
        let flags = COPY_FLAGS | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE;
        let mut staged: Vec<(gio::File, gio::File, gio::File)> = Vec::new();
        let mut committed: Vec<(gio::File, gio::File, gio::File)> = Vec::new();
        let outcome = (|| -> Result<()> {
            for (source, target) in &pairs {
                self.check()?;
                let temporary = self.temporary(source, "rename")?;
                source.move_(&temporary, flags, Some(&cancellable), None)?;
                staged.push((source.clone(), temporary, target.clone()));
            }
            for (source, temporary, target) in staged.clone() {
                self.check()?;
                // No OVERWRITE flag: a concurrent creator wins safely.
                temporary.move_(&target, flags, Some(&cancellable), None)?;
                committed.push((source.clone(), temporary, target.clone()));
                self.notify(Some(&source), Some(&target), 0, 0, true);
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            // Vacate every committed final name before restoring originals;
            // this also rolls back cycles such as A -> B and B -> A.
            for (_, temporary, target) in committed.iter().rev() {
                if let Err(failure) = target.move_(temporary, flags, none(), None) {
                    self.result.failures.push(Failure {
                        source: uri(target),
                        message: tr!("Could not roll back this renamed item: {error}",
                                     error = Error::from(failure).to_string()),
                    });
                }
            }
            for (source, temporary, _) in staged.iter().rev() {
                if !exists(temporary, None)? {
                    continue;
                }
                if let Err(failure) = temporary.move_(source, flags, none(), None) {
                    self.result.failures.push(Failure {
                        source: uri(temporary),
                        message: tr!("Original item is preserved here; its name could not be restored: {error}",
                                     error = Error::from(failure).to_string()),
                    });
                }
            }
            return Err(error);
        }
        if record_undo {
            let mut batch = Vec::new();
            for (source, target) in &pairs {
                batch.push((target.clone(), source.clone(), fingerprint(target)?));
            }
            let mut record = UndoRecord::new(UndoKind::Batch, &pairs[0].1, None, None);
            record.batch = batch;
            self.undo.push(record);
        }
        Ok(())
    }

    fn trash_file(&self, source: &gio::File) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = self.hooks.trash.clone() {
            return hook(source);
        }
        source.trash(Some(&self.cancellable()))?;
        Ok(())
    }

    fn perform(&mut self, source: &gio::File) -> Result<bool> {
        let kind = self.kind().to_string();
        let cancellable = self.cancellable();
        match kind.as_str() {
            "copy" | "move" | "link" => {
                let Some(directory) = self.job.shared.destination.clone() else {
                    return Err(Error::value(tr!("Choose a destination folder.")));
                };
                let name = source.basename().unwrap_or_default();
                self.transfer(source, &directory.child(name), kind == "move", kind == "link")
            }
            "duplicate" => {
                let Some(parent) = source.parent() else {
                    return Err(Error::value(tr!("The filesystem root cannot be duplicated.")));
                };
                let name = source.basename().map(PathBuf::into_os_string).unwrap_or_default();
                let target = self.unique(&parent, name.as_bytes(), "copy")?;
                self.transfer(source, &target, false, false)
            }
            "rename" => {
                let Some(target) = self.job.shared.destination.clone() else {
                    return Err(Error::value(tr!("Choose a new file name.")));
                };
                self.transfer(source, &target, true, false)
            }
            "trash" => {
                // GIO failure is deliberately reported, never converted to delete.
                let identity = trash_identity(&source.query_info(INFO, FLAGS, Some(&cancellable))?);
                self.trash_file(source)?;
                let mut record = UndoRecord::new(UndoKind::Trash, source, Some(source), None);
                record.identity = identity;
                self.undo.push(record);
                Ok(true)
            }
            "delete" => {
                self.delete(source)?;
                Ok(true)
            }
            "restore" => {
                let info = source.query_info("trash::orig-path", FLAGS, Some(&cancellable))?;
                let original = info.attribute_byte_string("trash::orig-path").filter(|p| !p.is_empty());
                let Some(original) = original else {
                    return Err(Error::value(tr!("The trash entry has no original location.")));
                };
                let target = gio::File::for_path(Path::new(original.as_str()));
                self.transfer(source, &target, true, false)
            }
            other => Err(Error::value(tr!("Unknown operation: {kind}", kind = other))),
        }
    }

    fn perform_undo(&mut self) -> Result<()> {
        let cancellable = self.cancellable();
        let records: Vec<UndoRecord> = self.undo_records.iter().rev().cloned().collect();
        for record in records {
            self.check()?;
            let outcome = (|| -> Result<()> {
                match record.kind {
                    UndoKind::Trash => {
                        let original = record.original.clone().unwrap_or_else(|| record.current.clone());
                        if exists(&original, Some(&cancellable))? {
                            return Err(Error::runtime(tr!("The original location is occupied; the deleted item remains in Trash.")));
                        }
                        let trashed = trashed_item(&original, record.identity.as_ref(), &cancellable)?;
                        self.move_item(&trashed, &original)
                    }
                    UndoKind::Batch => {
                        for (current, _, snapshot) in &record.batch {
                            if fingerprint(current)? != *snapshot {
                                return Err(Error::runtime(tr!("An item changed since the batch rename; it was left untouched.")));
                            }
                        }
                        let pairs: Vec<(gio::File, gio::File)> =
                            record.batch.iter().map(|(current, original, _)| (current.clone(), original.clone())).collect();
                        self.rename_batch(&pairs, false)
                    }
                    UndoKind::Mkdir => {
                        if exists(&record.current, None)? {
                            return Err(Error::runtime(tr!("The original folder was recreated; undo stopped for it.")));
                        }
                        record.current.make_directory(Some(&cancellable))?;
                        Ok(())
                    }
                    UndoKind::Move | UndoKind::Remove => {
                        if Some(fingerprint(&record.current)?) != record.fingerprint {
                            return Err(Error::runtime(tr!("The item changed since the operation; it was left untouched.")));
                        }
                        if record.kind == UndoKind::Move {
                            let original = record.original.clone().unwrap_or_else(|| record.current.clone());
                            if exists(&original, Some(&cancellable))? {
                                return Err(Error::runtime(tr!("The original location is occupied; it was left untouched.")));
                            }
                            self.move_item(&record.current, &original)
                        } else {
                            self.delete(&record.current)
                        }
                    }
                }
            })();
            match outcome {
                Ok(()) => {
                    self.result.completed.push(uri(&record.current));
                    self.undo_remaining.retain(|r| r.id != record.id);
                }
                Err(error) if is_cancelled(&error) => return Err(error),
                Err(error) => self.result.failures.push(Failure { source: uri(&record.current), message: error.to_string() }),
            }
        }
        Ok(())
    }

    fn body(&mut self) -> Result<()> {
        let kind = self.kind().to_string();
        let cancellable = self.cancellable();
        if kind == "undo" {
            return self.perform_undo();
        }
        if kind == "batch_rename" {
            let pairs = self.rename_pairs.clone();
            self.rename_batch(&pairs, true)?;
            self.result.completed.extend(pairs.iter().map(|(source, _)| uri(source)));
            return Ok(());
        }
        if kind == "mkdir" || kind == "touch" {
            let Some(destination) = self.job.shared.destination.clone() else {
                return Err(Error::value(tr!("Choose a file name.")));
            };
            self.check()?;
            if kind == "mkdir" {
                destination.make_directory(Some(&cancellable))?;
            } else {
                let stream = destination.create(gio::FileCreateFlags::NONE, Some(&cancellable))?;
                stream.close(none())?;
            }
            let snapshot = fingerprint(&destination)?;
            self.undo.push(UndoRecord::new(UndoKind::Remove, &destination, None, Some(snapshot)));
            self.result.completed.push(uri(&destination));
            return Ok(());
        }
        if kind == "rename" && self.job.shared.sources.len() != 1 {
            return Err(Error::value(tr!("Rename one item at a time.")));
        }
        for source in self.job.shared.sources.clone() {
            self.check()?;
            match self.perform(&source) {
                Ok(done) => {
                    if done {
                        self.result.completed.push(uri(&source));
                    }
                    self.notify(Some(&source), None, 0, 0, true);
                }
                Err(error) if is_cancelled(&error) => return Err(error),
                Err(error) => self.result.failures.push(Failure { source: uri(&source), message: error.to_string() }),
            }
        }
        Ok(())
    }

    fn run(&mut self) {
        match self.body() {
            Ok(()) => {}
            Err(error) if is_cancelled(&error) => self.result.cancelled = true,
            Err(error) => self.result.failures.push(Failure { source: String::new(), message: error.to_string() }),
        }
        self.result.cancelled |= self.job.shared.cancellable.is_cancelled();
        // Replacements deliberately have no undo: removing a successful
        // new copy cannot recover an overwritten original.
        if !self.undo.is_empty() && !self.replaced {
            self.ops.remember(std::mem::take(&mut self.undo));
            self.result.undo_available = true;
        } else if self.kind() == "undo" && !self.undo_remaining.is_empty() {
            self.ops.remember(std::mem::take(&mut self.undo_remaining));
            self.result.undo_available = true;
        }
        self.ops.inner.jobs.lock().unwrap().retain(|job| job != &self.job);
        {
            let mut finished = self.job.shared.finished.lock().unwrap();
            *finished = Some(self.result.clone());
            self.job.shared.finished_cv.notify_all();
        }
        if self.has_done {
            let _ = self.sender.try_send(Event::Done(self.result.clone()));
        }
    }
}

fn splitext_bytes(name: &[u8]) -> (&[u8], &[u8]) {
    if let Some(dot) = name.iter().rposition(|&b| b == b'.') {
        if name[..dot].iter().any(|&b| b != b'.') {
            return (&name[..dot], &name[dot..]);
        }
    }
    (name, b"")
}

#[cfg(test)]
mod tests;
