//! Port of `~/tessel-bluetooth/tessel-shell/desktop_file_ops.py` (contract §7): desktop
//! copy/move transfers that never block the GTK main loop, failure
//! notifications, the async default-browser lookup and the shared Trash watcher.
//!
//! # API (Python name → Rust)
//! - `REPLACE, KEEP_BOTH, SKIP` → [`Action`] + consts [`REPLACE`], [`KEEP_BOTH`], [`SKIP`]
//!   (`Action::as_str()` = `"replace" | "keep" | "skip"`); copy/move → [`Mode`]
//!   (`Mode::from_str("cut" | "move")` = `Move`, anything else `Copy`).
//! - `unique_name(dir, name)` → [`unique_name`] / [`unique_name_with`] (injectable `exists`).
//! - `parse_clipboard_files(text, fmt)` → [`parse_clipboard_files`] →
//!   `(Option<Mode>, Vec<String>)`; [`GNOME_COPIED_FILES`], [`URI_LIST`].
//! - `TransferItem` → [`TransferItem`] (public fields `source, name, conflict, same, is_dir, action`).
//! - `plan_items(sources, dest, mode, cancellable)` → [`plan_items`] (blocking: worker only);
//!   `plan_async(...)` → [`plan_async`] (worker thread, callback on the main loop).
//! - `ConflictResolver(items, ask, done).start()` → [`ConflictResolver::new`] + `start()`;
//!   `ask(&item, remaining, reply)` must eventually call `reply.send(Some(action) | None, apply_to_all)`
//!   (`None` = dismissed = SKIP). No nested main loop; replies may be synchronous.
//! - `FileTransfer(items, dest, mode, on_progress=, on_done=)` → [`FileTransfer::new`]
//!   + `.on_progress(|fraction, name|)` + `.on_done(|TransferResult|)` + `.start()`;
//!   `.cancel()` (also before `start`), `.cancellable()`, `.join()` (tests).
//!   [`TransferResult`] `{ placed, errors: Vec<(name, message)>, cancelled }`.
//! - `notify(summary, body)` → [`notify`] / [`notify_with`] (icon, app name);
//!   [`notify_parameters`] builds the `(susssasa{sv}i)` tuple.
//! - `default_web_browser_async(env, cb)` → [`default_web_browser_async`] (`cb(Option<id>, Option<error>)`).
//! - `TrashWatcher()` → [`TrashWatcher::new`] (or [`TrashWatcher::with_backend`] for tests):
//!   `count()`, `add_listener(cb) -> ListenerId`, `remove_listener(id)`, `has_listener(id)`,
//!   `query()`, `close()`. One per app, shared by every desktop view.
//! - DesktopView helpers that Python kept inline in `tessel-shell`: [`rename_error`]
//!   (`/`, `.`, `..` → `tr("A name can't contain \"/\".")`), [`rename_desktop_entry`]
//!   (`Name=` via `GKeyFile`), [`rename_async`] (`set_display_name_async`),
//!   [`trash_async`], [`empty_trash_async`].
//! - [`mailbox::Mailbox`]: worker-thread → main-loop delivery for non-`Send` handlers.
//!
//! # How DesktopView calls it (mirrors `tessel-shell` `start_file_transfer`)
//! ```ignore
//! let mode = Mode::from_str(mode);
//! plan_async(sources, dest_dir.clone(), mode, move |items| {
//!     ConflictResolver::new(items,
//!         move |item, remaining, reply| view.ask_conflict(item, remaining, reply), // Skip / Keep Both / Replace
//!         move |resolved| view.run_transfer(resolved, dest_dir, mode)).start();
//! }, None);
//! // run_transfer: drop SKIP items; let t = FileTransfer::new(items, dest, mode);
//! // t.on_progress(..).on_done(..); show the `.transfer-toast` row after 400 ms
//! // with a Cancel button → t.cancel(); t.start().
//! ```
//! All callbacks run on the thread-default main context of the thread that
//! created the object (the GTK main thread in the daemon).

pub mod mailbox;

use mailbox::Mailbox;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gio::prelude::*;
use tessel_core::tr;

/// How a transfer item is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Replace,
    KeepBoth,
    Skip,
}

pub const REPLACE: Action = Action::Replace;
pub const KEEP_BOTH: Action = Action::KeepBoth;
pub const SKIP: Action = Action::Skip;

impl Action {
    /// The Python string constants.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Replace => "replace",
            Action::KeepBoth => "keep",
            Action::Skip => "skip",
        }
    }
    pub fn parse(value: &str) -> Option<Action> {
        match value {
            "replace" => Some(Action::Replace),
            "keep" => Some(Action::KeepBoth),
            "skip" => Some(Action::Skip),
            _ => None,
        }
    }
}

/// Copy or move (`"cut"` and `"move"` mean move).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Copy,
    Move,
}

impl Mode {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Mode {
        if matches!(value, "move" | "cut") { Mode::Move } else { Mode::Copy }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Copy => "copy",
            Mode::Move => "move",
        }
    }
}

pub const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
pub const URI_LIST: &str = "text/uri-list";
const QUERY: &str = "standard::name,standard::display-name,standard::type,standard::size";
const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

// ---------------------------------------------------------------- names

/// `os.path.lexists`.
pub fn lexists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// `os.path.splitext` for a bare file name (leading dots are not an extension).
pub fn splitext(name: &str) -> (&str, &str) {
    if let Some(dot) = name.rfind('.') {
        if name[..dot].chars().any(|c| c != '.') {
            return (&name[..dot], &name[dot..]);
        }
    }
    (name, "")
}

/// `name`, or the first free `base (n).ext` inside `directory`.
pub fn unique_name(directory: &Path, name: &str) -> String {
    unique_name_with(directory, name, lexists)
}

/// [`unique_name`] with an injectable existence check (Python's `exists=`).
pub fn unique_name_with(directory: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> String {
    if !exists(&directory.join(name)) {
        return name.to_string();
    }
    let (mut base, mut ext) = splitext(name);
    if base.is_empty() {
        base = name;
        ext = "";
    }
    let mut counter = 1u64;
    while exists(&directory.join(format!("{base} ({counter}){ext}"))) {
        counter += 1;
    }
    format!("{base} ({counter}){ext}")
}

/// Python `str.splitlines()` boundaries.
fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if is_line_break(c) {
            lines.push(&text[start..index]);
            let mut end = index + c.len_utf8();
            if c == '\r' {
                if let Some(&(next, '\n')) = chars.peek() {
                    chars.next();
                    end = next + 1;
                }
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `(mode, uris)` from an `x-special/gnome-copied-files` or `text/uri-list`
/// payload. A gnome payload without its `copy`/`cut` verb is invalid → `(None, [])`.
pub fn parse_clipboard_files(text: &str, fmt: &str) -> (Option<Mode>, Vec<String>) {
    let text = text.replace('\0', "\n");
    let mut lines: Vec<&str> = split_lines(&text).into_iter().map(str::trim).filter(|line| !line.is_empty()).collect();
    let mut mode = Mode::Copy;
    if matches!(lines.first(), Some(&"copy") | Some(&"cut")) {
        if lines[0] == "cut" {
            mode = Mode::Move;
        }
        lines.remove(0);
    } else if fmt == GNOME_COPIED_FILES {
        return (None, Vec::new());
    }
    (Some(mode), lines.into_iter().filter(|line| !line.starts_with('#') && line.contains(':')).map(String::from).collect())
}

/// `os.path.realpath` (non-strict: missing tails are kept).
pub fn realpath(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(path)
    };
    if let Ok(resolved) = std::fs::canonicalize(&absolute) {
        return resolved;
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => realpath(parent).join(name),
        (Some(parent), None) => realpath(parent),
        _ => absolute,
    }
}

fn inside(path: &Path, folder: &Path) -> bool {
    let path = realpath(path);
    let folder = realpath(folder);
    path == folder || path.starts_with(&folder)
}

// ---------------------------------------------------------------- planning

/// One source and the decision about where it lands.
#[derive(Clone, Debug)]
pub struct TransferItem {
    pub source: Option<gio::File>,
    pub name: String,
    pub conflict: bool,
    pub same: bool,
    pub is_dir: bool,
    pub action: Option<Action>,
}

impl TransferItem {
    pub fn new(source: Option<gio::File>, name: impl Into<String>) -> Self {
        Self { source, name: name.into(), conflict: false, same: false, is_dir: false, action: None }
    }
    pub fn with_conflict(mut self, conflict: bool) -> Self {
        self.conflict = conflict;
        self
    }
    pub fn with_same(mut self, same: bool) -> Self {
        self.same = same;
        self
    }
    pub fn with_dir(mut self, is_dir: bool) -> Self {
        self.is_dir = is_dir;
        self
    }
    pub fn with_action(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }
}

fn cancelled(cancellable: Option<&gio::Cancellable>) -> bool {
    cancellable.is_some_and(|c| c.is_cancelled())
}

/// Resolve destination names and conflicts. Blocking: call off the GTK thread.
pub fn plan_items(sources: &[gio::File], dest_dir: &Path, mode: Mode, cancellable: Option<&gio::Cancellable>) -> Vec<TransferItem> {
    let mut items = Vec::new();
    for source in sources {
        if cancelled(cancellable) {
            break;
        }
        let mut name: Option<String> = None;
        let mut is_dir = false;
        if let Ok(info) = source.query_info(QUERY, NOFOLLOW, cancellable) {
            is_dir = info.file_type() == gio::FileType::Directory;
            name = if source.is_native() {
                source.basename().map(|b| b.to_string_lossy().into_owned())
            } else {
                Some(info.display_name().to_string())
            };
        }
        let name = name
            .filter(|n| !n.is_empty())
            .or_else(|| source.basename().map(|b| b.to_string_lossy().into_owned()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| "file".to_string())
            .replace('/', "_");
        let target = dest_dir.join(&name);
        let exists = lexists(&target);
        let same = exists && source.path().is_some_and(|path| realpath(&path) == realpath(&target));
        let mut item = TransferItem::new(Some(source.clone()), name);
        item.conflict = exists && !same;
        item.same = same;
        item.is_dir = is_dir;
        if same {
            // Pasting a desktop item onto the desktop copies it beside itself;
            // moving it onto itself is a no-op.
            item.action = Some(if mode == Mode::Copy { KEEP_BOTH } else { SKIP });
        }
        items.push(item);
    }
    items
}

/// Run [`plan_items`] on a worker; `callback(items)` runs on the main loop.
pub fn plan_async(
    sources: Vec<gio::File>,
    dest_dir: impl Into<PathBuf>,
    mode: Mode,
    callback: impl FnOnce(Vec<TransferItem>) + 'static,
    cancellable: Option<gio::Cancellable>,
) -> std::thread::JoinHandle<()> {
    let dest_dir = dest_dir.into();
    let mailbox = Mailbox::once(callback);
    std::thread::Builder::new()
        .name("desktop-transfer-plan".into())
        .spawn(move || {
            let items = plan_items(&sources, &dest_dir, mode, cancellable.as_ref());
            mailbox.send(items);
        })
        .expect("spawn desktop-transfer-plan")
}

// ---------------------------------------------------------------- conflicts

type Ask = Rc<dyn Fn(&TransferItem, usize, ConflictReply)>;

struct ResolverInner {
    items: RefCell<Vec<TransferItem>>,
    pending: Vec<usize>,
    index: Cell<usize>,
    choice_for_all: Cell<Option<Action>>,
    ask: Ask,
    done: RefCell<Option<Box<dyn FnOnce(Vec<TransferItem>)>>>,
}

/// Ask about each conflict in turn without a nested main loop.
///
/// `ask(item, remaining, reply)` must eventually call `reply.send(..)`;
/// `remaining` counts this conflict and the ones after it. `done(items)`
/// receives the resolved plan: unresolved items become REPLACE if conflicting,
/// otherwise KEEP_BOTH. The resolver keeps itself alive through the reply.
#[derive(Clone)]
pub struct ConflictResolver(Rc<ResolverInner>);

/// The answer to one conflict prompt (consumed on use).
pub struct ConflictReply {
    resolver: Rc<ResolverInner>,
    index: usize,
}

impl ConflictReply {
    /// `choice = None` (dismissed) means SKIP.
    pub fn send(self, choice: Option<Action>, apply_to_all: bool) {
        ConflictResolver(self.resolver).reply(self.index, choice, apply_to_all);
    }
}

impl ConflictResolver {
    pub fn new(
        items: Vec<TransferItem>,
        ask: impl Fn(&TransferItem, usize, ConflictReply) + 'static,
        done: impl FnOnce(Vec<TransferItem>) + 'static,
    ) -> Self {
        let pending = items.iter().enumerate().filter(|(_, item)| item.conflict && item.action.is_none()).map(|(i, _)| i).collect();
        Self(Rc::new(ResolverInner {
            items: RefCell::new(items),
            pending,
            index: Cell::new(0),
            choice_for_all: Cell::new(None),
            ask: Rc::new(ask),
            done: RefCell::new(Some(Box::new(done))),
        }))
    }

    pub fn start(&self) {
        self.next();
    }

    /// The choice applied to all remaining conflicts, once chosen.
    pub fn choice_for_all(&self) -> Option<Action> {
        self.0.choice_for_all.get()
    }

    /// Conflicts still to be asked, including the current one.
    pub fn remaining(&self) -> usize {
        self.0.pending.len().saturating_sub(self.0.index.get())
    }

    fn next(&self) {
        let inner = &self.0;
        while inner.index.get() < inner.pending.len() {
            let position = inner.pending[inner.index.get()];
            if let Some(choice) = inner.choice_for_all.get() {
                inner.items.borrow_mut()[position].action = Some(choice);
                inner.index.set(inner.index.get() + 1);
                continue;
            }
            let remaining = inner.pending.len() - inner.index.get();
            let item = inner.items.borrow()[position].clone();
            let reply = ConflictReply { resolver: inner.clone(), index: inner.index.get() };
            let ask = inner.ask.clone();
            ask(&item, remaining, reply);
            return;
        }
        let items = {
            let mut items = inner.items.borrow_mut();
            for item in items.iter_mut() {
                if item.action.is_none() {
                    item.action = Some(if item.conflict { REPLACE } else { KEEP_BOTH });
                }
            }
            items.clone()
        };
        let done = inner.done.borrow_mut().take();
        if let Some(done) = done {
            done(items);
        }
    }

    fn reply(&self, index: usize, choice: Option<Action>, apply_to_all: bool) {
        let inner = &self.0;
        if index != inner.index.get() || index >= inner.pending.len() {
            return;
        }
        let choice = choice.unwrap_or(SKIP);
        inner.items.borrow_mut()[inner.pending[index]].action = Some(choice);
        if apply_to_all {
            inner.choice_for_all.set(Some(choice));
        }
        inner.index.set(index + 1);
        self.next();
    }
}

// ---------------------------------------------------------------- transfer

/// What a finished transfer did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferResult {
    /// Destination names created or replaced.
    pub placed: Vec<String>,
    /// `(name, message)`.
    pub errors: Vec<(String, String)>,
    pub cancelled: bool,
}

enum TransferMessage {
    Progress(f64, String),
    Done(TransferResult),
}

type ProgressFn = Box<dyn FnMut(f64, &str)>;
type DoneFn = Box<dyn FnOnce(TransferResult)>;

struct TransferInner {
    items: RefCell<Option<Vec<TransferItem>>>,
    names: Vec<String>,
    dest_dir: PathBuf,
    mode: Mode,
    cancellable: gio::Cancellable,
    on_progress: RefCell<Option<ProgressFn>>,
    on_done: RefCell<Option<DoneFn>>,
    thread: RefCell<Option<std::thread::JoinHandle<()>>>,
    use_trash: Cell<bool>,
    report_interval: Cell<Duration>,
    finished: Rc<Cell<bool>>,
}

/// Copy or move resolved items into `dest_dir` on a worker thread.
///
/// `on_progress(fraction, name)` and `on_done(result)` run on the main loop;
/// `cancel()` stops between and within file copies and removes a partial copy.
#[derive(Clone)]
pub struct FileTransfer(Rc<TransferInner>);

impl FileTransfer {
    /// SKIP items are dropped, as in Python.
    pub fn new(items: Vec<TransferItem>, dest_dir: impl Into<PathBuf>, mode: Mode) -> Self {
        let items: Vec<TransferItem> = items.into_iter().filter(|item| item.action != Some(SKIP)).collect();
        Self(Rc::new(TransferInner {
            names: items.iter().map(|item| item.name.clone()).collect(),
            items: RefCell::new(Some(items)),
            dest_dir: dest_dir.into(),
            mode,
            cancellable: gio::Cancellable::new(),
            on_progress: RefCell::new(None),
            on_done: RefCell::new(None),
            thread: RefCell::new(None),
            use_trash: Cell::new(true),
            report_interval: Cell::new(Duration::from_millis(100)),
            finished: Rc::new(Cell::new(false)),
        }))
    }

    pub fn on_progress(&self, callback: impl FnMut(f64, &str) + 'static) -> &Self {
        *self.0.on_progress.borrow_mut() = Some(Box::new(callback));
        self
    }

    pub fn on_done(&self, callback: impl FnOnce(TransferResult) + 'static) -> &Self {
        *self.0.on_done.borrow_mut() = Some(Box::new(callback));
        self
    }

    /// Replace moves the old item to the Trash when possible (default true);
    /// tests turn it off so nothing reaches a real Trash.
    pub fn set_use_trash(&self, use_trash: bool) -> &Self {
        self.0.use_trash.set(use_trash);
        self
    }

    /// Minimum time between progress reports (100 ms, Python's throttle).
    pub fn set_report_interval(&self, interval: Duration) -> &Self {
        self.0.report_interval.set(interval);
        self
    }

    /// Item names (SKIP items excluded).
    pub fn names(&self) -> &[String] {
        &self.0.names
    }
    pub fn dest_dir(&self) -> &Path {
        &self.0.dest_dir
    }
    pub fn mode(&self) -> Mode {
        self.0.mode
    }
    pub fn cancellable(&self) -> gio::Cancellable {
        self.0.cancellable.clone()
    }
    pub fn cancel(&self) {
        self.0.cancellable.cancel();
    }
    pub fn is_started(&self) -> bool {
        self.0.items.borrow().is_none()
    }
    /// `on_done` has run.
    pub fn is_finished(&self) -> bool {
        self.0.finished.get()
    }

    /// Start the worker (once). Must be called on the main thread.
    pub fn start(&self) -> &Self {
        let Some(items) = self.0.items.borrow_mut().take() else {
            return self;
        };
        let mut on_progress = self.0.on_progress.borrow_mut().take();
        let mut on_done = self.0.on_done.borrow_mut().take();
        let cancellable = self.0.cancellable.clone();
        let finished = self.0.finished.clone();
        let mailbox: Mailbox<TransferMessage> = Mailbox::new(move |message| match message {
            TransferMessage::Progress(fraction, name) => {
                if !cancellable.is_cancelled() {
                    if let Some(callback) = on_progress.as_mut() {
                        callback(fraction, &name);
                    }
                }
                true
            }
            TransferMessage::Done(result) => {
                finished.set(true);
                if let Some(callback) = on_done.take() {
                    callback(result);
                }
                false
            }
        });
        let worker = Worker {
            items,
            dest_dir: self.0.dest_dir.clone(),
            mode: self.0.mode,
            cancellable: self.0.cancellable.clone(),
            use_trash: self.0.use_trash.get(),
            partial: None,
            result: TransferResult::default(),
            progress: Progress {
                total: 1,
                done_bytes: 0,
                file_total: 0,
                last_report: None,
                interval: self.0.report_interval.get(),
                current: String::new(),
                mailbox,
            },
        };
        let handle = std::thread::Builder::new()
            .name("desktop-transfer".into())
            .spawn(move || worker.run())
            .expect("spawn desktop-transfer");
        *self.0.thread.borrow_mut() = Some(handle);
        self
    }

    /// Wait for the worker thread (tests). `on_done` still needs the main loop.
    pub fn join(&self) {
        let handle = self.0.thread.borrow_mut().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

struct Progress {
    total: u64,
    done_bytes: u64,
    file_total: u64,
    last_report: Option<Instant>,
    interval: Duration,
    current: String,
    mailbox: Mailbox<TransferMessage>,
}

impl Progress {
    fn report(&mut self, current: i64, total: i64) {
        self.file_total = total.max(0) as u64;
        let now = Instant::now();
        if self.last_report.is_none_or(|last| now.duration_since(last) >= self.interval) {
            self.last_report = Some(now);
            let fraction = ((self.done_bytes as f64 + current.max(0) as f64) / self.total as f64).min(1.0);
            self.mailbox.send(TransferMessage::Progress(fraction, self.current.clone()));
        }
    }
}

enum Failure {
    Gio(glib::Error),
    Os(String),
}

impl From<glib::Error> for Failure {
    fn from(error: glib::Error) -> Self {
        Failure::Gio(error)
    }
}

struct Worker {
    items: Vec<TransferItem>,
    dest_dir: PathBuf,
    mode: Mode,
    cancellable: gio::Cancellable,
    use_trash: bool,
    partial: Option<gio::File>,
    result: TransferResult,
    progress: Progress,
}

const COPY_FLAGS: gio::FileCopyFlags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS.union(gio::FileCopyFlags::ALL_METADATA);

impl Worker {
    fn run(mut self) {
        let items = std::mem::take(&mut self.items);
        let sizes: Vec<u64> = items.iter().map(|item| item.source.as_ref().map_or(0, |s| self.size(s))).collect();
        self.progress.total = sizes.iter().sum::<u64>().max(1);
        let mut completed = 0u64;
        for (item, size) in items.iter().zip(sizes) {
            if self.cancellable.is_cancelled() {
                break;
            }
            self.progress.current = item.name.clone();
            self.progress.done_bytes = completed;
            completed += size;
            match self.transfer(item) {
                Ok(placed) => {
                    self.result.placed.push(placed);
                    self.progress.done_bytes = completed;
                    self.progress.report(0, 0);
                }
                Err(Failure::Gio(error)) => {
                    self.remove_partial();
                    if error.matches(gio::IOErrorEnum::Cancelled) {
                        break;
                    }
                    self.result.errors.push((item.name.clone(), error.message().to_string()));
                }
                Err(Failure::Os(message)) => {
                    self.remove_partial();
                    self.result.errors.push((item.name.clone(), message));
                }
            }
        }
        self.result.cancelled = self.cancellable.is_cancelled();
        let result = std::mem::take(&mut self.result);
        self.progress.mailbox.send(TransferMessage::Done(result));
    }

    fn size(&self, source: &gio::File) -> u64 {
        let Ok(info) = source.query_info(QUERY, NOFOLLOW, Some(&self.cancellable)) else {
            return 0;
        };
        if info.file_type() != gio::FileType::Directory {
            return info.size().max(0) as u64;
        }
        let mut total = 0;
        if let Ok(enumerator) = source.enumerate_children(QUERY, NOFOLLOW, Some(&self.cancellable)) {
            while let Ok(Some(child)) = enumerator.next_file(Some(&self.cancellable)) {
                total += self.size(&enumerator.child(&child));
            }
            let _ = enumerator.close(gio::Cancellable::NONE);
        }
        total
    }

    fn transfer(&mut self, item: &TransferItem) -> Result<String, Failure> {
        let Some(source) = item.source.clone() else {
            return Err(Failure::Os(tr!("The item is no longer available.")));
        };
        let directory = self.dest_dir.clone();
        let mut name = item.name.clone();
        if item.action == Some(KEEP_BOTH) {
            name = unique_name(&directory, &name);
        }
        let target_path = directory.join(&name);
        let target = gio::File::for_path(&target_path);
        if item.is_dir {
            if let Some(source_path) = source.path() {
                if inside(&directory, &source_path) {
                    return Err(Failure::Os(tr!("A folder can't be copied or moved into itself.")));
                }
            }
        }
        let mut flags = COPY_FLAGS;
        if item.action == Some(REPLACE) && lexists(&target_path) {
            if item.is_dir || target_path.is_dir() {
                self.discard(&target)?;
            } else {
                flags |= gio::FileCopyFlags::OVERWRITE;
            }
        }
        // A failed or cancelled copy removes what it created; never a
        // pre-existing item (Replace already moved that out of the way).
        self.partial = if lexists(&target_path) { None } else { Some(target.clone()) };
        if self.mode == Mode::Move {
            let progress = &mut self.progress;
            let moved = source.move_(&target, flags, Some(&self.cancellable), Some(&mut |c, t| progress.report(c, t)));
            match moved {
                Ok(()) => {
                    self.partial = None;
                    return Ok(name);
                }
                Err(error) => {
                    if !(error.matches(gio::IOErrorEnum::WouldRecurse) || error.matches(gio::IOErrorEnum::NotSupported)) {
                        return Err(error.into());
                    }
                }
            }
            // Folders across filesystems: copy everything, then remove the source.
            self.copy_tree(&source, &target, flags)?;
            self.partial = None;
            delete_tree(&source, Some(&self.cancellable))?;
            return Ok(name);
        }
        if item.is_dir {
            self.copy_tree(&source, &target, flags)?;
        } else {
            self.copy_file(&source, &target, flags)?;
        }
        self.partial = None;
        Ok(name)
    }

    fn remove_partial(&mut self) {
        let Some(partial) = self.partial.take() else {
            return;
        };
        if lexists_file(&partial) {
            if let Err(error) = delete_tree(&partial, None) {
                println!("Could not remove partial copy: {}", error.message());
            }
        }
    }

    fn copy_file(&mut self, source: &gio::File, target: &gio::File, flags: gio::FileCopyFlags) -> Result<(), glib::Error> {
        self.progress.file_total = 0;
        let progress = &mut self.progress;
        source.copy(target, flags, Some(&self.cancellable), Some(&mut |c, t| progress.report(c, t)))?;
        self.progress.done_bytes += self.progress.file_total;
        self.progress.file_total = 0;
        Ok(())
    }

    fn copy_tree(&mut self, source: &gio::File, target: &gio::File, flags: gio::FileCopyFlags) -> Result<(), glib::Error> {
        target.make_directory(Some(&self.cancellable))?;
        let enumerator = source.enumerate_children(QUERY, NOFOLLOW, Some(&self.cancellable))?;
        let child_flags = flags.difference(gio::FileCopyFlags::OVERWRITE);
        let outcome = (|| {
            while let Some(info) = enumerator.next_file(Some(&self.cancellable))? {
                let child = enumerator.child(&info);
                let destination = target.child(info.name());
                if info.file_type() == gio::FileType::Directory {
                    self.copy_tree(&child, &destination, child_flags)?;
                } else {
                    self.copy_file(&child, &destination, child_flags)?;
                }
            }
            Ok::<(), glib::Error>(())
        })();
        let _ = enumerator.close(gio::Cancellable::NONE);
        outcome?;
        let _ = source.copy_attributes(target, gio::FileCopyFlags::ALL_METADATA, Some(&self.cancellable));
        Ok(())
    }

    /// Replace keeps the old item recoverable when the Trash is available.
    fn discard(&self, file: &gio::File) -> Result<(), glib::Error> {
        if self.use_trash {
            match file.trash(Some(&self.cancellable)) {
                Ok(()) => return Ok(()),
                Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => return Err(error),
                Err(_) => {}
            }
        }
        delete_tree(file, Some(&self.cancellable))
    }
}

/// `query_exists` without following a dangling symlink.
fn lexists_file(file: &gio::File) -> bool {
    match file.path() {
        Some(path) => lexists(&path),
        None => file.query_exists(gio::Cancellable::NONE),
    }
}

/// Recursively delete `file` (no symlink following).
pub fn delete_tree(file: &gio::File, cancellable: Option<&gio::Cancellable>) -> Result<(), glib::Error> {
    let info = file.query_info(QUERY, NOFOLLOW, cancellable)?;
    if info.file_type() == gio::FileType::Directory {
        let enumerator = file.enumerate_children(QUERY, NOFOLLOW, cancellable)?;
        let outcome = (|| {
            while let Some(child) = enumerator.next_file(cancellable)? {
                delete_tree(&enumerator.child(&child), cancellable)?;
            }
            Ok::<(), glib::Error>(())
        })();
        let _ = enumerator.close(gio::Cancellable::NONE);
        outcome?;
    }
    file.delete(cancellable)
}

// ---------------------------------------------------------------- notify

pub const NOTIFY_BUS: &str = "org.freedesktop.Notifications";
pub const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";

/// The `Notify` arguments `(susssasa{sv}i)`: urgency byte 1, timeout 8000.
pub fn notify_parameters(app_name: &str, icon: &str, summary: &str, body: &str) -> glib::Variant {
    let mut hints = std::collections::HashMap::new();
    hints.insert("urgency".to_string(), 1u8.to_variant());
    (app_name, 0u32, icon, summary, body, Vec::<String>::new(), hints, 8000i32).to_variant()
}

/// The console line Python prints before notifying.
pub fn notify_log_line(summary: &str, body: &str) -> String {
    if body.is_empty() { format!("⚠️ {summary}") } else { format!("⚠️ {summary}: {body}") }
}

/// Send a desktop notification asynchronously (`dialog-error-symbolic`,
/// app name `tr("Desktop")`); failures are only logged.
pub fn notify(summary: &str, body: &str) {
    notify_with(summary, body, "dialog-error-symbolic", None);
}

pub fn notify_with(summary: &str, body: &str, icon: &str, app_name: Option<&str>) {
    println!("{}", notify_log_line(summary, body));
    let app_name = app_name.map(String::from).unwrap_or_else(|| tr!("Desktop"));
    let parameters = notify_parameters(&app_name, icon, summary, body);
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |connection| {
        let connection = match connection {
            Ok(connection) => connection,
            Err(error) => {
                println!("Notification bus unavailable: {}", error.message());
                return;
            }
        };
        connection.call(
            Some(NOTIFY_BUS),
            NOTIFY_PATH,
            NOTIFY_BUS,
            "Notify",
            Some(&parameters),
            Some(glib::VariantTy::new("(u)").unwrap()),
            gio::DBusCallFlags::NONE,
            3000,
            gio::Cancellable::NONE,
            |result| {
                if let Err(error) = result {
                    println!("Notification failed: {}", error.message());
                }
            },
        );
    });
}

// ---------------------------------------------------------------- browser

/// Query `xdg-settings get default-web-browser` without blocking;
/// `callback(desktop_id, error)` runs on the main loop (an immediate spawn
/// failure calls it synchronously, as in Python).
pub fn default_web_browser_async<K, V>(
    environment: impl IntoIterator<Item = (K, V)>,
    callback: impl FnOnce(Option<String>, Option<String>) + 'static,
) where
    K: AsRef<str>,
    V: AsRef<str>,
{
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE);
    let environ: Vec<std::ffi::OsString> =
        environment.into_iter().map(|(k, v)| format!("{}={}", k.as_ref(), v.as_ref()).into()).collect();
    launcher.set_environ(&environ);
    let argv: [&std::ffi::OsStr; 3] = ["xdg-settings".as_ref(), "get".as_ref(), "default-web-browser".as_ref()];
    let process = match launcher.spawn(&argv) {
        Ok(process) => process,
        Err(error) => {
            callback(None, Some(error.message().to_string()));
            return;
        }
    };
    let proc = process.clone();
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| match result {
        Err(error) => callback(None, Some(error.message().to_string())),
        Ok((stdout, stderr)) => {
            let browser = stdout.as_deref().unwrap_or("").trim().to_string();
            if proc.is_successful() && !browser.is_empty() {
                callback(Some(browser), None);
            } else {
                let error = stderr.as_deref().unwrap_or("").trim().to_string();
                callback(None, if error.is_empty() { None } else { Some(error) });
            }
        }
    });
}

// ---------------------------------------------------------------- trash

/// Where the Trash count comes from (Gio `trash:///` in production).
pub trait TrashBackend {
    /// Watch for changes; returns a stopper, or an error when unsupported.
    fn monitor(&self, on_changed: Box<dyn Fn()>) -> Result<Box<dyn FnOnce()>, glib::Error>;
    /// Asynchronously read `trash::item-count` (priority LOW).
    fn query_count(&self, cancellable: &gio::Cancellable, done: Box<dyn FnOnce(Result<u32, glib::Error>)>);
}

/// `Gio.File.new_for_uri("trash:///")`.
pub struct GioTrash {
    pub file: gio::File,
}

impl Default for GioTrash {
    fn default() -> Self {
        Self { file: gio::File::for_uri("trash:///") }
    }
}

impl TrashBackend for GioTrash {
    fn monitor(&self, on_changed: Box<dyn Fn()>) -> Result<Box<dyn FnOnce()>, glib::Error> {
        let monitor = self.file.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)?;
        let handler = monitor.connect_changed(move |_, _, _, _| on_changed());
        Ok(Box::new(move || {
            monitor.disconnect(handler);
            monitor.cancel();
        }))
    }

    fn query_count(&self, cancellable: &gio::Cancellable, done: Box<dyn FnOnce(Result<u32, glib::Error>)>) {
        self.file.query_info_async(
            "trash::item-count",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::LOW,
            Some(cancellable),
            move |result| done(result.map(|info| info.attribute_uint32("trash::item-count"))),
        );
    }
}

/// Identifies a listener for [`TrashWatcher::remove_listener`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

struct WatcherInner {
    backend: Box<dyn TrashBackend>,
    count: Cell<Option<u32>>,
    listeners: RefCell<Vec<(ListenerId, Rc<dyn Fn(u32)>)>>,
    next_listener: Cell<u64>,
    cancellable: gio::Cancellable,
    busy: Cell<bool>,
    again: Cell<bool>,
    debounce: RefCell<Option<tessel_core::timer::Timeout>>,
    stop_monitor: RefCell<Option<Box<dyn FnOnce()>>>,
}

/// One `trash:///` monitor shared by every desktop view. Listeners receive the
/// item count from an asynchronous query of `trash::item-count` whenever the
/// Trash changes (signal driven, 150 ms debounce, no polling).
#[derive(Clone)]
pub struct TrashWatcher(Rc<WatcherInner>);

impl Default for TrashWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl TrashWatcher {
    pub fn new() -> Self {
        Self::with_backend(Box::new(GioTrash::default()))
    }

    pub fn with_backend(backend: Box<dyn TrashBackend>) -> Self {
        let watcher = TrashWatcher(Rc::new(WatcherInner {
            backend,
            count: Cell::new(None),
            listeners: RefCell::new(Vec::new()),
            next_listener: Cell::new(1),
            cancellable: gio::Cancellable::new(),
            busy: Cell::new(false),
            again: Cell::new(false),
            debounce: RefCell::new(None),
            stop_monitor: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&watcher.0);
        match watcher.0.backend.monitor(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                TrashWatcher(inner).changed();
            }
        })) {
            Ok(stop) => *watcher.0.stop_monitor.borrow_mut() = Some(stop),
            Err(error) => println!("Trash monitoring unavailable: {}", error.message()),
        }
        watcher.query();
        watcher
    }

    /// The last known count (`None` until the first query finishes).
    pub fn count(&self) -> Option<u32> {
        self.0.count.get()
    }

    /// `callback(count)` now (if known) and after every change.
    pub fn add_listener(&self, callback: impl Fn(u32) + 'static) -> ListenerId {
        let id = ListenerId(self.0.next_listener.get());
        self.0.next_listener.set(id.0 + 1);
        let callback: Rc<dyn Fn(u32)> = Rc::new(callback);
        self.0.listeners.borrow_mut().push((id, callback.clone()));
        if let Some(count) = self.0.count.get() {
            callback(count);
        }
        id
    }

    pub fn remove_listener(&self, id: ListenerId) {
        self.0.listeners.borrow_mut().retain(|(listener, _)| *listener != id);
    }

    pub fn has_listener(&self, id: ListenerId) -> bool {
        self.0.listeners.borrow().iter().any(|(listener, _)| *listener == id)
    }

    pub fn listener_count(&self) -> usize {
        self.0.listeners.borrow().len()
    }

    /// A debounce timer is pending.
    pub fn debounce_pending(&self) -> bool {
        self.0.debounce.borrow().as_ref().is_some_and(|t| t.is_pending())
    }

    /// Whether the directory monitor is active.
    pub fn is_monitoring(&self) -> bool {
        self.0.stop_monitor.borrow().is_some()
    }

    /// The monitor's `changed` handler (public for tests).
    pub fn changed(&self) {
        if self.debounce_pending() || self.is_closed() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let timer = tessel_core::timer::once(150, move || {
            if let Some(inner) = weak.upgrade() {
                inner.debounce.borrow_mut().take();
                TrashWatcher(inner).query();
            }
        });
        *self.0.debounce.borrow_mut() = Some(timer);
    }

    pub fn query(&self) {
        let inner = &self.0;
        if inner.cancellable.is_cancelled() {
            return;
        }
        if inner.busy.get() {
            inner.again.set(true);
            return;
        }
        inner.busy.set(true);
        let watcher = self.clone();
        inner.backend.query_count(&inner.cancellable, Box::new(move |result| watcher.queried(result)));
    }

    fn queried(&self, result: Result<u32, glib::Error>) {
        let inner = &self.0;
        inner.busy.set(false);
        let count = match result {
            Ok(count) => count,
            Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => return,
            Err(_) => 0,
        };
        if inner.again.get() {
            inner.again.set(false);
            self.query();
        }
        if Some(count) != inner.count.get() {
            inner.count.set(Some(count));
            let listeners: Vec<Rc<dyn Fn(u32)>> = inner.listeners.borrow().iter().map(|(_, cb)| cb.clone()).collect();
            for callback in listeners {
                callback(count);
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.0.cancellable.is_cancelled()
    }

    pub fn close(&self) {
        let inner = &self.0;
        inner.cancellable.cancel();
        inner.listeners.borrow_mut().clear();
        let timer = inner.debounce.borrow_mut().take();
        if let Some(timer) = timer {
            if timer.is_pending() {
                timer.cancel();
            }
        }
        let stop = inner.stop_monitor.borrow_mut().take();
        if let Some(stop) = stop {
            stop();
        }
    }
}

// ---------------------------------------------------------------- rename / trash helpers

/// The rename validation DesktopView applies to non-launcher files:
/// `Some(message)` when `new_name` contains `/` or is `.`/`..`.
pub fn rename_error(new_name: &str) -> Option<String> {
    if new_name.contains('/') || new_name == "." || new_name == ".." {
        Some(tr!("A name can't contain \"/\"."))
    } else {
        None
    }
}

/// Launchers keep their file name; only `[Desktop Entry] Name=` changes.
pub fn rename_desktop_entry(path: &Path, new_name: &str) -> Result<(), glib::Error> {
    let file = glib::KeyFile::new();
    file.load_from_file(path, glib::KeyFileFlags::NONE)?;
    file.set_string("Desktop Entry", "Name", new_name);
    file.save_to_file(path)
}

/// `Gio.File.set_display_name_async`; `done(Err(message))` on failure.
pub fn rename_async(path: &Path, new_name: &str, done: impl FnOnce(Result<PathBuf, String>) + 'static) {
    gio::File::for_path(path).set_display_name_async(new_name, glib::Priority::DEFAULT, gio::Cancellable::NONE, move |result| {
        done(result.map(|file| file.path().unwrap_or_default()).map_err(|error| error.message().to_string()))
    });
}

/// `Gio.File.trash_async`; `done(Err(message))` on failure.
pub fn trash_async(path: &Path, done: impl FnOnce(Result<(), String>) + 'static) {
    gio::File::for_path(path).trash_async(glib::Priority::DEFAULT, gio::Cancellable::NONE, move |result| {
        done(result.map_err(|error| error.message().to_string()))
    });
}

/// Empty Trash on a worker thread: delete every child of `trash`
/// (`trash:///` when `None`); `done(last error)` on the main loop.
pub fn empty_trash_async(trash: Option<gio::File>, done: impl FnOnce(Option<String>) + 'static) {
    let mailbox = Mailbox::once(done);
    let trash = trash.unwrap_or_else(|| gio::File::for_uri("trash:///"));
    std::thread::Builder::new()
        .name("empty-trash".into())
        .spawn(move || {
            let mut error = None;
            match trash.enumerate_children("standard::name", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
                Ok(enumerator) => {
                    loop {
                        match enumerator.next_file(gio::Cancellable::NONE) {
                            Ok(Some(info)) => {
                                if let Err(e) = trash.child(info.name()).delete(gio::Cancellable::NONE) {
                                    error = Some(e.message().to_string());
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                error = Some(e.message().to_string());
                                break;
                            }
                        }
                    }
                    let _ = enumerator.close(gio::Cancellable::NONE);
                }
                Err(e) => error = Some(e.message().to_string()),
            }
            mailbox.send(error);
        })
        .expect("spawn empty-trash");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitext_matches_python() {
        assert_eq!(splitext("a.txt"), ("a", ".txt"));
        assert_eq!(splitext(".profile"), (".profile", ""));
        assert_eq!(splitext("..x"), ("..x", ""));
        assert_eq!(splitext("a.b.c"), ("a.b", ".c"));
        assert_eq!(splitext("folder"), ("folder", ""));
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(split_lines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(split_lines("a\n\nb"), vec!["a", "", "b"]);
    }
}
