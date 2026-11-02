//! login_protocol.py: asynchronous greetd IPC. Only greetd may authenticate or
//! create sessions; this module relays prompts and answers and fails closed.
//!
//! Frames are a native-endian `u32` length + `json.dumps(request,
//! ensure_ascii=False)` UTF-8 bytes (byte-identical to Python). Requests run on
//! one worker thread in submission order; results are handed to `dispatch`
//! (the GUI forwards them to the GLib main loop) and applied with
//! [`Client::result`] on the UI thread.
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tessel_core::pyjson::py_str;
use tessel_locking::login_model::Session;
use zeroize::Zeroizing;

pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_USERNAME: usize = 256;
pub const MAX_RESPONSE: usize = 4096;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(120);
pub const UNAVAILABLE: &str = "The login service is unavailable. Please try again.";

/// `OSError` / `ValueError` of the Python module (Display = `str(error)`).
#[derive(Debug)]
pub enum ProtocolError {
    Os(std::io::Error),
    Value(&'static str),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Os(error) => write!(f, "{error}"),
            ProtocolError::Value(text) => f.write_str(text),
        }
    }
}

impl From<std::io::Error> for ProtocolError {
    fn from(error: std::io::Error) -> Self {
        ProtocolError::Os(error)
    }
}

/// A greetd request. The prompt answer is held in zeroizing memory and has no
/// `Debug` output.
pub enum Request {
    CancelSession,
    CreateSession(String),
    /// `{"type": "post_auth_message_response", "response": …}`
    Respond(Zeroizing<String>),
    /// `{"type": "post_auth_message_response"}` (info/error acknowledgement)
    Acknowledge,
    /// `Session.request()`
    StartSession(Map<String, Value>),
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind())?;
        if matches!(self, Request::Respond(_)) {
            f.write_str("(<redacted>)")?;
        }
        Ok(())
    }
}

/// `json.dumps(str, ensure_ascii=False)` escaping (`py_encode_basestring`).
fn escape_len(text: &str) -> usize {
    2 + text.chars().map(|c| match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }).sum::<usize>()
}

fn push_escaped(out: &mut Vec<u8>, text: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'"');
    for c in text.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(c as usize) >> 4]);
                out.push(HEX[(c as usize) & 15]);
            }
            c => {
                let mut buffer = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    out.push(b'"');
}

impl Request {
    pub fn kind(&self) -> &'static str {
        match self {
            Request::CancelSession => "cancel_session",
            Request::CreateSession(_) => "create_session",
            Request::Respond(_) | Request::Acknowledge => "post_auth_message_response",
            Request::StartSession(_) => "start_session",
        }
    }

    /// Python's request dict, for tests and logs that never contain secrets.
    pub fn to_value(&self) -> Option<Value> {
        let mut map = Map::new();
        map.insert("type".into(), self.kind().into());
        match self {
            Request::CreateSession(username) => {
                map.insert("username".into(), username.clone().into());
            }
            Request::Respond(_) => return None,
            Request::StartSession(request) => return Some(Value::Object(request.clone())),
            _ => {}
        }
        Some(Value::Object(map))
    }

    /// The JSON payload, `json.dumps(request, ensure_ascii=False).encode()`.
    /// The buffer is allocated once with its exact size (no reallocation
    /// leaves copies of a secret behind) and is wiped on drop.
    pub fn payload(&self) -> Zeroizing<Vec<u8>> {
        let head = br#"{"type": ""#;
        let (key, text): (&[u8], Option<&str>) = match self {
            Request::CreateSession(username) => (b"username", Some(username.as_str())),
            Request::Respond(response) => (b"response", Some(response.as_str())),
            Request::StartSession(request) => {
                let text = tessel_core::pyfmt::json_dumps(&Value::Object(request.clone()),
                    &tessel_core::pyfmt::JsonStyle { ensure_ascii: false, ..tessel_core::pyfmt::JsonStyle::DEFAULT });
                return Zeroizing::new(text.into_bytes());
            }
            _ => (b"", None),
        };
        let kind = self.kind().as_bytes();
        let size = head.len() + kind.len() + 1 + text.map_or(0, |t| 3 + key.len() + 3 + escape_len(t)) + 1;
        let mut out = Zeroizing::new(Vec::with_capacity(size));
        out.extend_from_slice(head);
        out.extend_from_slice(kind);
        out.push(b'"');
        if let Some(text) = text {
            out.extend_from_slice(b", \"");
            out.extend_from_slice(key);
            out.extend_from_slice(b"\": ");
            push_escaped(&mut out, text);
        }
        out.push(b'}');
        debug_assert_eq!(out.len(), size);
        out
    }

    /// Length header + payload in one buffer (Python sends them with one `sendall`).
    pub fn frame(&self) -> Result<Zeroizing<Vec<u8>>, ProtocolError> {
        let payload = self.payload();
        if payload.len() > MAX_FRAME {
            return Err(ProtocolError::Value("Login request is too large"));
        }
        let mut frame = Zeroizing::new(Vec::with_capacity(4 + payload.len()));
        frame.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }
}

/// `receive(channel, length)`.
pub fn receive(channel: &mut impl Read, length: usize) -> Result<Vec<u8>, ProtocolError> {
    let mut data = vec![0u8; length];
    let mut filled = 0;
    while filled < length {
        match channel.read(&mut data[filled..]) {
            Ok(0) => {
                return Err(ProtocolError::Os(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Login backend disconnected")))
            }
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(data)
}

/// Validate one reply payload: an object whose `type` is success/error/auth_message.
pub fn parse_response(payload: &[u8]) -> Result<Map<String, Value>, ProtocolError> {
    let value: Value = serde_json::from_slice(payload).map_err(|_| ProtocolError::Value("Invalid login backend response"))?;
    match value {
        Value::Object(map) if matches!(map.get("type").and_then(Value::as_str), Some("success" | "error" | "auth_message")) => Ok(map),
        _ => Err(ProtocolError::Value("Invalid login backend response")),
    }
}

/// `exchange(channel, request)`: one request, one validated reply.
pub fn exchange<C: Read + Write>(channel: &mut C, request: &Request) -> Result<Map<String, Value>, ProtocolError> {
    let frame = request.frame()?;
    channel.write_all(&frame)?;
    drop(frame);
    let header = receive(channel, 4)?;
    let length = u32::from_ne_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if !(0 < length && length <= MAX_FRAME) {
        return Err(ProtocolError::Value("Invalid login backend message size"));
    }
    parse_response(&receive(channel, length)?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Resetting,
    Authenticating,
    Prompt,
    Ready,
    Launching,
    Started,
    Error,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Resetting => "resetting",
            State::Authenticating => "authenticating",
            State::Prompt => "prompt",
            State::Ready => "ready",
            State::Launching => "launching",
            State::Started => "started",
            State::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Reset,
    Auth,
    Cancel,
    Start,
}

/// A finished request, delivered by `dispatch` and applied with [`Client::result`].
#[derive(Debug)]
pub struct Completion {
    pub serial: u64,
    pub phase: Phase,
    pub response: Option<Map<String, Value>>,
}

struct Job {
    serial: u64,
    phase: Phase,
    request: Request,
}

struct Shared {
    path: PathBuf,
    channel: Mutex<Option<Arc<UnixStream>>>,
    generation: AtomicU64,
    closed: AtomicBool,
}

impl Shared {
    fn disconnect(&self) {
        let channel = self.channel.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(channel) = channel {
            let _ = channel.shutdown(std::net::Shutdown::Both);
        }
    }

    fn stale(&self, serial: u64) -> bool {
        self.closed.load(Ordering::SeqCst) || serial != self.generation.load(Ordering::SeqCst)
    }

    fn work(&self, job: &Job) -> Result<Option<Map<String, Value>>, ProtocolError> {
        let (channel, connect) = {
            let mut guard = self.channel.lock().unwrap_or_else(|e| e.into_inner());
            if self.stale(job.serial) {
                return Ok(None);
            }
            match &*guard {
                Some(channel) => (channel.clone(), false),
                None => {
                    let channel = Arc::new(unix_socket()?);
                    *guard = Some(channel.clone());
                    (channel, true)
                }
            }
        };
        // Never hold the cancellation mutex during a blocking connect.
        if connect {
            connect_with_timeout(&channel, &self.path, CONNECT_TIMEOUT)?;
            channel.set_read_timeout(Some(EXCHANGE_TIMEOUT))?;
            channel.set_write_timeout(Some(EXCHANGE_TIMEOUT))?;
            // Python closes a disconnected socket so its connect fails; ours was
            // only shut down, so confirm it is still the current one.
            let guard = self.channel.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.as_ref().is_some_and(|current| Arc::ptr_eq(current, &channel)) || self.stale(job.serial) {
                return Err(ProtocolError::Os(std::io::Error::from_raw_os_error(libc::ECONNABORTED)));
            }
        }
        let mut stream: &UnixStream = &channel;
        exchange(&mut stream, &job.request).map(Some)
    }
}

fn unix_socket() -> std::io::Result<UnixStream> {
    // SAFETY: plain socket(2); the fd is owned by the returned UnixStream.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

/// `channel.settimeout(5); channel.connect(path)`.
fn connect_with_timeout(channel: &UnixStream, path: &Path, timeout: Duration) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: zeroed sockaddr_un is valid; bounds are checked below.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let fd = channel.as_raw_fd();
    channel.set_nonblocking(true)?;
    let length = (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
    // SAFETY: valid fd and sockaddr_un of the given length.
    let status = unsafe { libc::connect(fd, &address as *const _ as *const libc::sockaddr, length) };
    if status != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        let mut poll = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as libc::c_int) };
        if ready == 0 {
            return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        if ready < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if let Some(error) = channel.take_error()? {
            return Err(error);
        }
    }
    channel.set_nonblocking(false)
}

type Changed = Rc<dyn Fn(&str, &str)>;
type Dispatch = Arc<dyn Fn(Completion) + Send + Sync>;

struct Inner {
    shared: Arc<Shared>,
    jobs: RefCell<Option<mpsc::Sender<Job>>>,
    state: Cell<State>,
    username: RefCell<String>,
    session: RefCell<Option<Session>>,
    changed: Changed,
    #[cfg(test)]
    hook: RefCell<Option<Box<dyn FnMut(&Request, Phase)>>>,
}

/// `login_protocol.Client` (UI-thread handle; clones share one client).
#[derive(Clone)]
pub struct Client(Rc<Inner>);

impl Client {
    /// `Client(path, dispatch, changed)`. `dispatch` runs on the worker thread
    /// and must hand the completion to the UI thread's [`Client::result`].
    pub fn new(path: impl Into<PathBuf>, dispatch: impl Fn(Completion) + Send + Sync + 'static,
               changed: impl Fn(&str, &str) + 'static) -> Client {
        let shared = Arc::new(Shared { path: path.into(), channel: Mutex::new(None), generation: AtomicU64::new(0),
                                       closed: AtomicBool::new(false) });
        let (sender, receiver) = mpsc::channel::<Job>();
        let worker = shared.clone();
        let dispatch: Dispatch = Arc::new(dispatch);
        std::thread::Builder::new().name("greetd-ipc".into()).spawn(move || {
            for job in receiver {
                let response = match worker.work(&job) {
                    Ok(response) => response,
                    // Never include request data, exception text or input in logs/UI.
                    Err(_) => {
                        let mut map = Map::new();
                        map.insert("type".into(), "error".into());
                        map.insert("description".into(), UNAVAILABLE.into());
                        Some(map)
                    }
                };
                let (serial, phase) = (job.serial, job.phase);
                drop(job); // wipes a prompt answer before the UI sees the result
                dispatch(Completion { serial, phase, response });
            }
        }).expect("greetd IPC worker thread");
        Client(Rc::new(Inner {
            shared, jobs: RefCell::new(Some(sender)), state: Cell::new(State::Idle), username: RefCell::new(String::new()),
            session: RefCell::new(None), changed: Rc::new(changed),
            #[cfg(test)]
            hook: RefCell::new(None),
        }))
    }

    pub fn state(&self) -> State {
        self.0.state.get()
    }

    pub fn generation(&self) -> u64 {
        self.0.shared.generation.load(Ordering::SeqCst)
    }

    pub fn session(&self) -> Option<Session> {
        self.0.session.borrow().clone()
    }

    pub fn username(&self) -> String {
        self.0.username.borrow().clone()
    }

    fn closed(&self) -> bool {
        self.0.shared.closed.load(Ordering::SeqCst)
    }

    fn bump(&self) {
        self.0.shared.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn changed(&self, kind: &str, message: &str) {
        let callback = self.0.changed.clone();
        callback(kind, message);
    }

    fn request(&self, request: Request, phase: Phase) {
        #[cfg(test)]
        if let Some(hook) = self.0.hook.borrow_mut().as_mut() {
            return hook(&request, phase);
        }
        let job = Job { serial: self.generation(), phase, request };
        if let Some(jobs) = self.0.jobs.borrow().as_ref() {
            let _ = jobs.send(job);
        }
    }

    /// Test seam equivalent to Python's `patch.object(client, '_request')`.
    #[cfg(test)]
    pub(crate) fn set_hook(&self, hook: Option<Box<dyn FnMut(&Request, Phase)>>) {
        *self.0.hook.borrow_mut() = hook;
    }

    #[cfg(test)]
    pub(crate) fn force(&self, state: State, generation: u64) {
        self.0.state.set(state);
        self.0.shared.generation.store(generation, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn set_session(&self, session: Session) {
        *self.0.session.borrow_mut() = Some(session);
    }

    pub fn begin(&self, username: &str, session: Option<Session>) {
        if self.closed() || self.state() == State::Launching {
            return;
        }
        if username.is_empty() || username.len() > MAX_USERNAME || username.contains('\0') {
            self.cancel();
            self.0.state.set(State::Error);
            self.changed("error", "Enter a valid username.");
            return;
        }
        self.bump();
        self.0.shared.disconnect();
        *self.0.username.borrow_mut() = username.to_owned();
        *self.0.session.borrow_mut() = session;
        self.0.state.set(State::Resetting);
        self.changed("busy", "Preparing sign-in…");
        // Reset even after an interrupted IPC connection or previous greeter
        // crash. greetd owns the authentication state, not this socket.
        self.request(Request::CancelSession, Phase::Reset);
    }

    /// Answer the current prompt. The answer is moved (not copied) into the
    /// request and wiped after it was written to the socket.
    pub fn respond(&self, text: Zeroizing<String>) {
        if self.state() != State::Prompt {
            return;
        }
        if text.contains('\0') || text.len() > MAX_RESPONSE {
            drop(text);
            self.cancel();
            self.0.state.set(State::Error);
            self.changed("error", "The authentication response is invalid or too long.");
            return;
        }
        self.0.state.set(State::Authenticating);
        self.changed("busy", "Signing in…");
        self.request(Request::Respond(text), Phase::Auth);
    }

    pub fn cancel(&self) {
        if self.closed() || self.state() == State::Launching {
            return;
        }
        self.bump();
        self.0.shared.disconnect();
        self.0.state.set(State::Idle);
        self.request(Request::CancelSession, Phase::Cancel);
    }

    /// The UI may finish a transition only for this successful PAM exchange.
    pub fn start_session(&self, generation: u64) {
        if self.closed() || self.state() != State::Ready || generation != self.generation() {
            return;
        }
        let Some(request) = self.session().map(|session| session.request()) else { return };
        self.0.state.set(State::Launching);
        self.request(Request::StartSession(request), Phase::Start);
    }

    /// `_result(serial, phase, response)`.
    pub fn result(&self, completion: Completion) {
        let Completion { serial, phase, response } = completion;
        let Some(response) = response else { return };
        if self.closed() || serial != self.generation() {
            return;
        }
        if phase == Phase::Cancel {
            return;
        }
        let kind = response.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind == "error" {
            self.0.state.set(State::Error);
            let message = if response.get("error_type").and_then(Value::as_str) == Some("auth_error") {
                "Sign-in failed. Check your credentials and try again.".to_owned()
            } else {
                response.get("description").map(py_str).unwrap_or_else(|| "Sign-in could not be completed.".into())
            };
            self.changed("error", &message);
        } else if kind != "success" && phase != Phase::Auth {
            self.0.state.set(State::Error);
            self.changed("error", "Unexpected response from the login service.");
        } else if phase == Phase::Reset {
            self.0.state.set(State::Authenticating);
            let username = self.username();
            self.request(Request::CreateSession(username), Phase::Auth);
        } else if kind == "auth_message" {
            let message_type = response.get("auth_message_type").and_then(Value::as_str);
            let message = response.get("auth_message").map(py_str).unwrap_or_default();
            match message_type {
                Some(kind @ ("visible" | "secret")) => {
                    self.0.state.set(State::Prompt);
                    self.changed(kind, &message);
                }
                Some("info" | "error") => {
                    self.changed("message", &message);
                    self.request(Request::Acknowledge, Phase::Auth);
                }
                _ => {
                    self.cancel();
                    self.changed("error", "Unsupported authentication prompt.");
                }
            }
        } else if phase == Phase::Auth && kind == "success" {
            self.0.state.set(State::Ready);
            self.changed("authenticated", "Starting your session…");
        } else if phase == Phase::Start && kind == "success" {
            self.0.state.set(State::Started);
            self.changed("started", "");
        }
    }

    pub fn close(&self) {
        self.0.shared.closed.store(true, Ordering::SeqCst);
        self.bump();
        self.0.shared.disconnect();
        // Queued work sees `closed` and is dropped; the worker exits.
        self.0.jobs.borrow_mut().take();
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
