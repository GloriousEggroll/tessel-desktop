//! Focused fullscreen detection for the dock's keyboard shortcut and hot
//! corners (port of fullscreen_guard.py).
//!
//! Foreign-toplevel reports compositor fullscreen for both native and
//! Xwayland windows. It does not expose native geometry or decorations, so a
//! native window merely sized to the output cannot be identified. For
//! Xwayland, a short X11 query (pure-Rust client, 120 ms total deadline, only
//! a local `DISPLAY` `:N` / `unix:N`, at most 512 listed clients) additionally
//! recognizes a focused, undecorated, screen-filling window without
//! conflating ordinary maximization. Unknown X11 state fails open (= not
//! fullscreen).
//!
//! Public API:
//! - [`FullscreenCache`] — thread-safe verdicts keyed by [`focus_signature`];
//!   `last_state(Some(&snapshot))` never probes (native fullscreen is exact,
//!   a stale signature reads false), `needs_probe(&snapshot, force)` (forced
//!   refreshes are rate-limited to [`PROBE_MAX_AGE`]), `record`, `invalidate`,
//!   `forget(live ids)`. Windows absent from two complete X11 client lists are
//!   remembered as native Wayland and never probed again. [`cache()`] is the
//!   process-wide instance (Python's module `CACHE`).
//! - [`focused_fullscreen`] (fresh, may block ≤ 120 ms — call off the main
//!   loop), [`focused_fullscreen_with`] (injectable probe), [`last_state`].
//! - [`x11_snapshot`] / [`x11_snapshot_for`] / [`probe`] — the X11 snapshot
//!   (Python's `--probe` JSON object, same keys).
//! - [`FullscreenGuard`] — tessel-dock.py's `focused_fullscreen_state()` /
//!   `refresh_fullscreen_state()`: read the cached verdict (never blocks) and
//!   refresh it on a worker thread, coalesced (one probe in flight, one rerun).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use serde_json::{json, Value};
use tessel_core::pyjson::casefold;

use super::windows::{Output, Window, WindowsSnapshot};

/// Total deadline of one X11 snapshot (Python `PROBE_TIMEOUT = .12`).
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(120);
/// A forced refresh reuses a verdict this young (seconds).
pub const PROBE_MAX_AGE: f64 = 2.0;
pub const MAX_X11_CLIENTS: usize = 512;
/// The Python probe's stdout limit (bytes of `json.dumps(snapshot) + "\n"`).
const OUTPUT_LIMIT: usize = 65536;

/// `(id, app_id, title, fullscreen, maximized, outputs)` of each focused window, sorted.
pub type Signature = Vec<(String, String, String, bool, bool, Vec<String>)>;

/// Focused = activated and not minimized, in window order.
pub fn focused(snapshot: &WindowsSnapshot) -> Vec<(&String, &Window)> {
    snapshot.windows.iter().filter(|(_, w)| w.activated && !w.minimized).collect()
}

/// Foreign-toplevel state that can change a focused-fullscreen verdict.
pub fn focus_signature(snapshot: &WindowsSnapshot) -> Signature {
    let mut signature: Signature = focused(snapshot).into_iter()
        .map(|(id, w)| (id.clone(), w.app_id.clone(), w.title.clone(), w.fullscreen, w.maximized, w.outputs.clone()))
        .collect();
    signature.sort();
    signature
}

/// Python `value == 0` (also `0.0` and `False`).
fn py_zero(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => !flag,
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// `type(value) is int` → its value.
fn py_int(value: &Value) -> Option<i128> {
    match value {
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)),
        _ => None,
    }
}

/// Conservative identity/geometry checks on one fresh X11 focus snapshot.
pub fn matching_x11_fullscreen(window: &Window, outputs: &IndexMap<String, Output>, snapshot: Option<&Value>) -> bool {
    let Some(snapshot) = snapshot.and_then(Value::as_object).filter(|s| !s.is_empty()) else { return false };
    if snapshot.get("focused") != Some(&Value::Bool(true)) {
        return false;
    }
    let classes = match snapshot.get("classes") {
        None => Some(Vec::new()),
        Some(Value::Array(items)) => Some(items.iter().filter_map(Value::as_str).map(casefold).collect::<Vec<_>>()),
        Some(_) => None,
    };
    let Some(classes) = classes else { return false };
    if window.app_id.is_empty() || !classes.contains(&casefold(&window.app_id))
        || snapshot.get("title").and_then(Value::as_str) != Some(window.title.as_str()) {
        return false;
    }
    if snapshot.get("fullscreen") == Some(&Value::Bool(true)) {
        return true;
    }
    if snapshot.get("maximized") == Some(&Value::Bool(true)) || !snapshot.get("border_width").is_some_and(py_zero) {
        return false;
    }
    let zero_extents = matches!(snapshot.get("extents"), Some(Value::Array(e)) if e.len() == 4 && e.iter().all(py_zero));
    let no_decorations = matches!(snapshot.get("motif"), Some(Value::Array(m))
        if m.len() >= 3 && py_int(&m[0]).is_some_and(|flags| flags & 2 != 0) && py_zero(&m[2]));
    if !(zero_extents || no_decorations) {
        return false;
    }
    let geometry: Option<Vec<i128>> = match snapshot.get("geometry") {
        Some(Value::Array(g)) if g.len() == 4 => g.iter().map(py_int).collect(),
        _ => None,
    };
    let Some(geometry) = geometry.filter(|g| g[2].min(g[3]) > 0) else { return false };
    for output in outputs.values() {
        if !window.outputs.is_empty() && !window.outputs.contains(&output.name) {
            continue;
        }
        let expected = [output.x, output.y, output.width, output.height].map(i128::from);
        if expected[2].min(expected[3]) > 0 && geometry.iter().zip(expected).all(|(a, t)| (a - t).abs() <= 1) {
            return true;
        }
    }
    false
}

#[derive(Default)]
struct CacheState {
    signature: Option<Signature>,
    result: bool,
    stamp: Option<f64>,
    /// Window ids proven not to belong to Xwayland.
    native: HashSet<String>,
    /// One missing-from-X11 sighting may be a map race.
    votes: HashMap<String, u32>,
}

/// Thread-safe verdicts keyed by focus signature; reads never probe.
pub struct FullscreenCache {
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
    state: Mutex<CacheState>,
}

impl Default for FullscreenCache {
    fn default() -> Self { Self::new() }
}

fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

impl FullscreenCache {
    pub fn new() -> Self { Self::with_clock(monotonic) }
    pub fn with_clock(clock: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        Self { clock: Box::new(clock), state: Mutex::new(CacheState::default()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> { self.state.lock().unwrap_or_else(|e| e.into_inner()) }

    fn candidates<'a>(state: &CacheState, snapshot: &'a WindowsSnapshot) -> Vec<&'a Window> {
        focused(snapshot).into_iter().filter(|(id, w)| !w.app_id.is_empty() && !state.native.contains(*id))
            .map(|(_, w)| w).collect()
    }

    /// Whether a fresh X11 check could change the cached verdict.
    pub fn needs_probe(&self, snapshot: &WindowsSnapshot, force: bool) -> bool {
        let focused = focused(snapshot);
        if focused.is_empty() || focused.iter().any(|(_, w)| w.fullscreen) {
            return false;
        }
        let state = self.lock();
        if Self::candidates(&state, snapshot).is_empty() {
            return false;
        }
        let Some(stamp) = state.stamp.filter(|_| state.signature.as_ref() == Some(&focus_signature(snapshot))) else {
            return true;
        };
        force && (self.clock)() - stamp >= PROBE_MAX_AGE
    }

    /// Cached verdict, never probing: native state is exact, unknown X11 fails
    /// open. `None` returns the last verdict regardless of focus.
    pub fn last_state(&self, snapshot: Option<&WindowsSnapshot>) -> bool {
        let state = self.lock();
        let Some(snapshot) = snapshot else { return state.result };
        let focused = focused(snapshot);
        if focused.iter().any(|(_, w)| w.fullscreen) {
            return true;
        }
        if focused.is_empty() || Self::candidates(&state, snapshot).is_empty() {
            return false;
        }
        state.signature.as_ref() == Some(&focus_signature(snapshot)) && state.result
    }

    /// Record one fresh check (with the X11 snapshot it used, if any).
    pub fn record(&self, signature: Signature, focused: &[(&String, &Window)], snapshot: Option<&Value>, result: bool) {
        let mut state = self.lock();
        state.signature = Some(signature);
        state.result = result;
        state.stamp = Some((self.clock)());
        let Some(Value::Array(clients)) = snapshot.and_then(|s| s.get("clients")) else { return };
        let known: HashSet<String> = clients.iter().filter_map(Value::as_array)
            .flat_map(|classes| classes.iter().filter_map(Value::as_str).map(casefold)).collect();
        for (id, window) in focused {
            // A focused window absent from two complete X11 client lists is
            // native Wayland for its whole lifetime; one absence could be an
            // Xwayland window probed before its X11 map completed.
            if !window.app_id.is_empty() && !known.contains(&casefold(&window.app_id)) {
                let votes = state.votes.entry((*id).clone()).or_insert(0);
                *votes += 1;
                if *votes >= 2 {
                    state.native.insert((*id).clone());
                }
            } else {
                state.votes.remove(*id);
            }
        }
    }

    /// Focused windows that might be Xwayland: an app id and not proven native.
    pub fn probe_candidates<'a>(&self, snapshot: &'a WindowsSnapshot) -> Vec<&'a Window> {
        Self::candidates(&self.lock(), snapshot)
    }

    /// Require a fresh check on the next `needs_probe`, keeping native memory.
    pub fn invalidate(&self) {
        let mut state = self.lock();
        state.signature = None;
        state.stamp = None;
    }

    /// Drop native-window memory for closed windows.
    pub fn forget<'a>(&self, live: impl IntoIterator<Item = &'a String>) {
        let live: HashSet<&String> = live.into_iter().collect();
        let mut state = self.lock();
        state.native.retain(|id| live.contains(id));
        state.votes.retain(|id, _| live.contains(id));
    }
}

/// The process-wide cache (Python's module-level `CACHE`).
pub fn cache() -> &'static Arc<FullscreenCache> {
    static CACHE: OnceLock<Arc<FullscreenCache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(FullscreenCache::new()))
}

/// Cheap cached verdict from [`cache()`]; never probes.
pub fn last_state(snapshot: Option<&WindowsSnapshot>) -> bool { cache().last_state(snapshot) }

/// Fresh check with the real X11 probe (may block up to [`PROBE_TIMEOUT`]).
pub fn focused_fullscreen(snapshot: &WindowsSnapshot, cache: &FullscreenCache) -> bool {
    focused_fullscreen_with(snapshot, cache, &x11_snapshot)
}

/// Fresh check: native fullscreen is immediate; otherwise the Xwayland
/// snapshot is taken only if a focused window may be Xwayland. The verdict
/// is recorded in `cache`.
pub fn focused_fullscreen_with(snapshot: &WindowsSnapshot, cache: &FullscreenCache, x11: &dyn Fn() -> Option<Value>) -> bool {
    let signature = focus_signature(snapshot);
    let focused = focused(snapshot);
    if focused.iter().any(|(_, w)| w.fullscreen) {
        cache.record(signature, &focused, None, true);
        return true;
    }
    let candidates = cache.probe_candidates(snapshot);
    if candidates.is_empty() {
        cache.record(signature, &focused, None, false);
        return false;
    }
    let x11 = x11();
    let result = candidates.iter().any(|w| matching_x11_fullscreen(w, &snapshot.outputs, x11.as_ref()));
    cache.record(signature, &focused, x11.as_ref(), result);
    result
}

/// Xwayland is local: never turn a shortcut into a remote DISPLAY query.
pub fn is_local_display(display: &str) -> bool {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"\A(?:unix)?:[0-9]+(?:\.[0-9]+)?\z").unwrap()).is_match(display)
}

/// The X11 snapshot for `$DISPLAY` (None: remote/missing display, timeout,
/// X error, or no answer).
pub fn x11_snapshot() -> Option<Value> {
    let display = std::env::var("DISPLAY").ok();
    x11_snapshot_for(display.as_deref(), probe)
}

/// [`x11_snapshot`] with an injectable probe `(display, deadline)`.
pub fn x11_snapshot_for(display: Option<&str>, probe: impl FnOnce(&str, Duration) -> Option<Value>) -> Option<Value> {
    let display = display.filter(|d| is_local_display(d))?;
    let snapshot = probe(display, PROBE_TIMEOUT)?;
    if !snapshot.is_object() {
        return None;
    }
    let printed = tessel_core::pyfmt::json_dumps(&snapshot, &tessel_core::pyfmt::JsonStyle::DEFAULT).len() + 1;
    (printed <= OUTPUT_LIMIT).then_some(snapshot)
}

// ---- the X11 probe ----------------------------------------------------------------------

mod x11 {
    use super::*;
    use std::io::{IoSlice, Result as IoResult};
    use std::os::fd::AsRawFd;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{self, ConnectionExt as _};
    use x11rb::reexports::x11rb_protocol::parse_display::ConnectAddress;
    use x11rb::reexports::x11rb_protocol::xauth::get_auth;
    use x11rb::rust_connection::{DefaultStream, PollMode, RustConnection, Stream};
    use x11rb::utils::RawFdContainer;

    /// DefaultStream whose waits end at a deadline (a stalled X server
    /// cannot hang the caller; Python killed its probe subprocess instead).
    pub(super) struct DeadlineStream {
        inner: DefaultStream,
        deadline: Instant,
    }

    impl Stream for DeadlineStream {
        fn poll(&self, mode: PollMode) -> IoResult<()> {
            let mut events = 0;
            if mode.readable() {
                events |= libc::POLLIN;
            }
            if mode.writable() {
                events |= libc::POLLOUT;
            }
            loop {
                let left = self.deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
                }
                let mut fd = libc::pollfd { fd: self.inner.as_raw_fd(), events, revents: 0 };
                let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
                let ready = unsafe { libc::poll(&mut fd, 1, ms) };
                if ready > 0 {
                    return Ok(());
                }
                if ready < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
        }
        fn read(&self, buf: &mut [u8], fds: &mut Vec<RawFdContainer>) -> IoResult<usize> { self.inner.read(buf, fds) }
        fn write(&self, buf: &[u8], fds: &mut Vec<RawFdContainer>) -> IoResult<usize> { self.inner.write(buf, fds) }
        fn write_vectored(&self, bufs: &[IoSlice<'_>], fds: &mut Vec<RawFdContainer>) -> IoResult<usize> {
            self.inner.write_vectored(bufs, fds)
        }
    }

    #[cfg(test)]
    pub(super) struct DeadlineStreamForTest;
    #[cfg(test)]
    impl DeadlineStreamForTest {
        pub(super) fn new(inner: DefaultStream, deadline: Instant) -> DeadlineStream { DeadlineStream { inner, deadline } }
    }

    /// `:N[.S]` / `unix:N[.S]` → (display, screen); anything else is refused.
    pub(super) fn parse_local(display: &str) -> Option<(u16, usize)> {
        let rest = display.strip_prefix("unix").unwrap_or(display).strip_prefix(':')?;
        let (number, screen) = rest.split_once('.').unwrap_or((rest, "0"));
        let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !all_digits(number) || !all_digits(screen) {
            return None;
        }
        Some((number.parse().ok()?, screen.parse().ok()?))
    }

    pub(super) fn connect(display: &str, deadline: Instant) -> Option<(RustConnection<DeadlineStream>, usize)> {
        let (number, screen) = parse_local(display)?;
        // Local socket only (abstract namespace first, as libxcb does).
        let address = ConnectAddress::Socket(format!("/tmp/.X11-unix/X{number}"));
        let (stream, (family, peer)) = DefaultStream::connect(&address).ok()?;
        let (name, data) = get_auth(family, &peer, number).ok().flatten().unwrap_or_default();
        let stream = DeadlineStream { inner: stream, deadline };
        RustConnection::connect_to_stream_with_auth_info(stream, screen, name, data).ok().map(|c| (c, screen))
    }

    /// A window property as Xlib's `XGetWindowProperty(…, 0, 1024, AnyPropertyType)`.
    #[derive(Debug, Clone, PartialEq)]
    pub(super) enum Prop {
        Bytes(Vec<u8>),
        Longs(Vec<u64>),
    }

    impl Prop {
        fn truthy(&self) -> bool {
            match self {
                Self::Bytes(b) => !b.is_empty(),
                Self::Longs(l) => !l.is_empty(),
            }
        }
    }

    struct Probe<C: Connection> {
        conn: C,
        root: xproto::Window,
        atoms: HashMap<&'static str, u32>,
    }

    /// Any X error or I/O failure aborts the probe (Python's error handler).
    type R<T> = Result<T, ()>;

    impl<C: Connection> Probe<C> {
        fn atom(&mut self, name: &'static str) -> R<u32> {
            if let Some(atom) = self.atoms.get(name) {
                return Ok(*atom);
            }
            let atom = self.conn.intern_atom(true, name.as_bytes()).map_err(drop)?.reply().map_err(drop)?.atom;
            self.atoms.insert(name, atom);
            Ok(atom)
        }

        fn prop(&mut self, window: u32, name: &'static str) -> R<Option<Prop>> {
            let key = self.atom(name)?;
            if key == 0 {
                return Ok(None);
            }
            let reply = self.conn.get_property(false, window, key, xproto::AtomEnum::ANY, 0, 1024)
                .map_err(drop)?.reply().map_err(drop)?;
            if reply.type_ == 0 || reply.bytes_after != 0 {
                return Ok(None);
            }
            Ok(match reply.format {
                8 => Some(Prop::Bytes(reply.value8().map(Iterator::collect).unwrap_or_default())),
                32 => Some(Prop::Longs(reply.value32().map(|v| v.map(u64::from).collect()).unwrap_or_default())),
                _ => None,
            })
        }

        fn longs(&mut self, window: u32, name: &'static str) -> R<Option<Vec<u64>>> {
            Ok(match self.prop(window, name)? {
                Some(Prop::Longs(values)) => Some(values),
                _ => None,
            })
        }

        fn active_window(&mut self) -> R<u64> {
            let root = self.root;
            Ok(self.longs(root, "_NET_ACTIVE_WINDOW")?.and_then(|v| v.first().copied()).unwrap_or(0))
        }

        fn has_focus(&mut self, active: u64) -> R<bool> {
            let mut current = u64::from(self.conn.get_input_focus().map_err(drop)?.reply().map_err(drop)?.focus);
            // Focus can be on a child rendering surface rather than the toplevel.
            for _ in 0..16 {
                if current == active {
                    return Ok(true);
                }
                if current == 0 || current == 1 || current == u64::from(self.root) {
                    return Ok(false);
                }
                let tree = self.conn.query_tree(current as u32).map_err(drop)?.reply().map_err(drop)?;
                if u64::from(tree.parent) == current {
                    return Ok(false);
                }
                current = u64::from(tree.parent);
            }
            Ok(false)
        }

        fn classes(raw: &[u8]) -> Vec<Value> {
            raw.split(|b| *b == 0).filter(|v| !v.is_empty())
                .map(|v| Value::String(String::from_utf8_lossy(v).into_owned())).collect()
        }

        /// Complete, bounded list of managed X11 clients' WM_CLASS values.
        fn client_classes(&mut self) -> R<Value> {
            let root = self.root;
            let Some(listed) = self.longs(root, "_NET_CLIENT_LIST")?.filter(|l| l.len() <= MAX_X11_CLIENTS) else {
                return Ok(Value::Null);
            };
            let mut result = Vec::with_capacity(listed.len());
            for window in listed {
                result.push(Value::Array(match self.prop(window as u32, "WM_CLASS")? {
                    Some(Prop::Bytes(raw)) => Self::classes(&raw),
                    _ => Vec::new(),
                }));
            }
            Ok(Value::Array(result))
        }

        fn snapshot(&mut self) -> R<Option<Value>> {
            let clients = self.client_classes()?;
            let active = self.active_window()?;
            if active == 0 || active == 1 || active == u64::from(self.root) || !self.has_focus(active)? {
                return Ok(Some(json!({"focused": false, "clients": clients})));
            }
            let window = active as u32;
            let Some(Prop::Bytes(raw_class)) = self.prop(window, "WM_CLASS")? else { return Ok(None) };
            let title = match self.prop(window, "_NET_WM_NAME")?.filter(Prop::truthy) {
                Some(value) => Some(value),
                None => self.prop(window, "WM_NAME")?.filter(Prop::truthy),
            };
            let title = match title {
                None => Vec::new(),
                Some(Prop::Bytes(bytes)) => bytes,
                Some(Prop::Longs(_)) => return Ok(None),
            };
            let states = match self.prop(window, "_NET_WM_STATE")?.filter(Prop::truthy) {
                Some(Prop::Longs(values)) => values,
                Some(Prop::Bytes(bytes)) => bytes.into_iter().map(u64::from).collect(),
                None => Vec::new(),
            };
            let geometry = self.conn.get_geometry(window).map_err(drop)?.reply().map_err(drop)?;
            let root = self.root;
            let origin = self.conn.translate_coordinates(window, root, 0, 0).map_err(drop)?.reply().map_err(drop)?;
            if !origin.same_screen {
                return Ok(None);
            }
            let fullscreen = states.contains(&u64::from(self.atom("_NET_WM_STATE_FULLSCREEN")?));
            let vert = u64::from(self.atom("_NET_WM_STATE_MAXIMIZED_VERT")?);
            let horz = u64::from(self.atom("_NET_WM_STATE_MAXIMIZED_HORZ")?);
            let maximized = states.contains(&vert) || states.contains(&horz);
            let title_end = title.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
            let extents = self.longs(window, "_NET_FRAME_EXTENTS")?;
            let motif = self.longs(window, "_MOTIF_WM_HINTS")?;
            let snapshot = json!({
                "focused": true, "classes": Self::classes(&raw_class),
                "title": String::from_utf8_lossy(&title[..title_end]),
                "fullscreen": fullscreen, "maximized": maximized,
                "geometry": [origin.dst_x, origin.dst_y, geometry.width, geometry.height],
                "border_width": geometry.border_width, "extents": extents, "motif": motif, "clients": clients,
            });
            // Focus may have changed while reading: never report an old game.
            if self.active_window()? != active || !self.has_focus(active)? {
                return Ok(None);
            }
            Ok(Some(snapshot))
        }
    }

    pub(super) fn run(display: &str, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        let (conn, screen) = connect(display, deadline)?;
        let root = conn.setup().roots.get(screen)?.root;
        let mut probe = Probe { conn, root, atoms: HashMap::new() };
        probe.snapshot().ok().flatten()
    }
}

/// Read X11 focus state for `display` within `timeout` (Python `_probe()`).
pub fn probe(display: &str, timeout: Duration) -> Option<Value> { x11::run(display, timeout) }

// ---- main-loop guard -----------------------------------------------------------------------

type SnapshotSource = Box<dyn Fn() -> Option<WindowsSnapshot>>;
type X11Source = Arc<dyn Fn() -> Option<Value> + Send + Sync>;

struct GuardInner {
    cache: Arc<FullscreenCache>,
    x11: X11Source,
    source: SnapshotSource,
    running: Cell<bool>,
    again: Cell<bool>,
    finished: RefCell<Option<Box<dyn Fn()>>>,
}

/// tessel-dock.py's coalesced fullscreen refresh. Cheap to clone.
#[derive(Clone)]
pub struct FullscreenGuard(Rc<GuardInner>);

impl FullscreenGuard {
    /// `source` returns the current window state (None = no backend yet);
    /// verdicts go to the process-wide [`cache()`], probes use [`x11_snapshot`].
    pub fn new(source: impl Fn() -> Option<WindowsSnapshot> + 'static) -> Self {
        Self::with_parts(source, cache().clone(), Arc::new(x11_snapshot))
    }

    pub fn with_parts(source: impl Fn() -> Option<WindowsSnapshot> + 'static, cache: Arc<FullscreenCache>, x11: X11Source) -> Self {
        Self(Rc::new(GuardInner {
            cache, x11, source: Box::new(source), running: Cell::new(false), again: Cell::new(false),
            finished: RefCell::new(None),
        }))
    }

    pub fn cache(&self) -> &Arc<FullscreenCache> { &self.0.cache }

    /// Last-known focused fullscreen state; never blocks the main loop
    /// (`focused_fullscreen_state`). Also starts a refresh.
    pub fn state(&self) -> bool {
        let Some(snapshot) = (self.0.source)() else { return false };
        let state = self.0.cache.last_state(Some(&snapshot));
        self.refresh();
        state
    }

    /// `_menu_hotkey_blocked`: the hotkey is suppressed only when fullscreen
    /// use is not allowed and the focused window is (cached) fullscreen.
    pub fn menu_hotkey_blocked(&self, allow_in_fullscreen: bool) -> bool { !allow_in_fullscreen && self.state() }

    /// Window list changed (tessel-dock.py `changed()`): forget closed windows,
    /// refresh the verdict in the background.
    pub fn windows_changed(&self) {
        if let Some(snapshot) = (self.0.source)() {
            self.0.cache.forget(snapshot.windows.keys());
        }
        self.refresh();
    }

    pub fn probe_running(&self) -> bool { self.0.running.get() }

    /// Called on the main loop after each worker run (tests, UI refresh).
    pub fn connect_finished(&self, callback: impl Fn() + 'static) { self.0.finished.replace(Some(Box::new(callback))); }

    /// Refresh the cached Xwayland verdict off the main thread, coalesced.
    pub fn refresh(&self) {
        let Some(snapshot) = (self.0.source)() else { return };
        if self.0.running.get() {
            self.0.again.set(true);
            return;
        }
        if !self.0.cache.needs_probe(&snapshot, false) {
            return;
        }
        self.0.running.set(true);
        let (cache, x11) = (self.0.cache.clone(), self.0.x11.clone());
        let handle = gio::spawn_blocking(move || {
            focused_fullscreen_with(&snapshot, &cache, &*x11);
        });
        let weak: Weak<GuardInner> = Rc::downgrade(&self.0);
        glib::MainContext::ref_thread_default().spawn_local(async move {
            let _ = handle.await;
            let Some(inner) = weak.upgrade() else { return };
            inner.running.set(false);
            let guard = FullscreenGuard(inner);
            if guard.0.again.replace(false) {
                guard.refresh();
            }
            let finished = guard.0.finished.borrow();
            if let Some(callback) = finished.as_ref() {
                callback();
            }
        });
    }
}

#[cfg(test)]
mod tests;
