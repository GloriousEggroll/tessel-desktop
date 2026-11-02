//! lock_auth.py: asynchronous, cancellable `tessel-lock-pam` driver. Only a
//! verified success — the `(5, "ok")` packet AND helper exit status 0 —
//! completes with `true`.
//!
//! The helper reader runs on a std thread and posts events to the GLib main
//! loop through an async channel; callbacks always run on the main thread.
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const LIMIT: usize = 4096;

/// PAM transaction callbacks (Python's on_prompt, on_message, on_complete).
pub struct Callbacks {
    pub prompt: Box<dyn Fn(&str, bool)>,
    pub message: Box<dyn Fn(&str)>,
    pub complete: Box<dyn Fn(bool)>,
}

/// One PAM transaction as the lock application sees it. Implemented by
/// [`Authentication`] (the real helper) and by test fakes.
pub trait Transaction {
    fn start(&self);
    /// The secret is borrowed; implementations must not keep copies.
    fn respond(&self, text: &str);
    fn cancel(&self);
    fn waiting(&self) -> bool;
    fn set_password_only(&self, value: bool);
    fn password_only(&self) -> bool;
    fn set_fingerprint(&self, value: bool);
    fn fingerprint(&self) -> bool;
    fn as_any(&self) -> &dyn std::any::Any;
}

pub type AuthFactory = Rc<dyn Fn(Callbacks) -> Rc<dyn Transaction>>;

/// The PAM helper, `tessel-lock-pam`, in the program directory (next to the
/// running executable first).
pub fn helper_path() -> PathBuf {
    tessel_core::paths::program("tessel-lock-pam")
}

/// Helper command line for the selected fixed policy.
pub fn helper_arguments(fingerprint: bool, password_only: bool) -> &'static [&'static str] {
    if fingerprint { &["--fingerprint"] } else if password_only { &["--password-only"] } else { &[] }
}

/// The helper's command (`subprocess.Popen(..., close_fds=True)`): stdin and
/// stdout piped, stderr to /dev/null, and every inherited descriptor above 2
/// marked close-on-exec so no GTK/Wayland/D-Bus fd reaches the PAM stack.
pub fn helper_command(helper: &std::path::Path, args: &[&str]) -> Command {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(helper);
    command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // SAFETY: the closure only issues the async-signal-safe close_range(2)
    // syscall between fork and exec. CLOSE_RANGE_CLOEXEC keeps std's own
    // exec-error pipe working (it is close-on-exec already).
    unsafe {
        command.pre_exec(|| {
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
            if libc::syscall(libc::SYS_close_range, 3 as libc::c_uint, libc::c_uint::MAX, CLOSE_RANGE_CLOEXEC) != 0 {
                // Kernels without close_range (< 5.11): mark each open fd.
                for fd in 3..1024 {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                    }
                }
            }
            Ok(())
        });
    }
    command
}

enum Event {
    Prompt(String, bool),
    Message(String),
    Complete(bool),
}

struct Inner {
    callbacks: Callbacks,
    helper: PathBuf,
    child: RefCell<Option<Arc<Mutex<Child>>>>,
    stdin: RefCell<Option<ChildStdin>>,
    cancelled: Cell<bool>,
    waiting: Cell<bool>,
    password_only: Cell<bool>,
    fingerprint: Cell<bool>,
}

#[derive(Clone)]
pub struct Authentication(Rc<Inner>);

impl Authentication {
    pub fn new(callbacks: Callbacks) -> Self { Self::with_helper(helper_path(), callbacks) }

    pub fn with_helper(helper: PathBuf, callbacks: Callbacks) -> Self {
        Self(Rc::new(Inner { callbacks, helper, child: RefCell::new(None), stdin: RefCell::new(None),
            cancelled: false.into(), waiting: false.into(), password_only: false.into(), fingerprint: false.into() }))
    }

    pub fn factory() -> AuthFactory { Rc::new(|callbacks| Rc::new(Authentication::new(callbacks)) as Rc<dyn Transaction>) }

    pub fn cancelled(&self) -> bool { self.0.cancelled.get() }

    fn dispatch(&self, event: Event) {
        let inner = &self.0;
        match event {
            Event::Prompt(message, echo) => if !inner.cancelled.get() {
                inner.waiting.set(true);
                (inner.callbacks.prompt)(&message, echo);
            },
            Event::Message(message) => if !inner.cancelled.get() {
                (inner.callbacks.message)(&message);
            },
            Event::Complete(success) => {
                inner.waiting.set(false);
                inner.stdin.borrow_mut().take();
                inner.child.borrow_mut().take();
                if !inner.cancelled.get() {
                    (inner.callbacks.complete)(success);
                }
            }
        }
    }
}

fn read_exact(stream: &mut impl Read, size: usize) -> std::io::Result<Vec<u8>> {
    let mut buffer = vec![0; size];
    stream.read_exact(&mut buffer)?;
    Ok(buffer)
}

fn wait_for(child: &Mutex<Child>, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.lock().map(|mut c| c.try_wait().ok().flatten()) {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn terminate(child: &Mutex<Child>) {
    if let Ok(mut child) = child.lock() {
        // Only signal an unreaped child: the pid cannot have been recycled.
        if matches!(child.try_wait(), Ok(None)) {
            // SAFETY: kill(2) on our own child.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        }
    }
}

/// The reader thread (`_read`).
fn read_helper(mut stdout: impl Read, child: Arc<Mutex<Child>>, send: async_channel::Sender<Event>) {
    let mut success = false;
    let _ = (|| -> Result<(), ()> {
        loop {
            let header = read_exact(&mut stdout, 5).map_err(|_| ())?;
            let kind = header[0];
            let size = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
            if size > LIMIT {
                return Err(());
            }
            let message = String::from_utf8_lossy(&read_exact(&mut stdout, size).map_err(|_| ())?).into_owned();
            match kind {
                1 | 2 => { let _ = send.send_blocking(Event::Prompt(message, kind == 2)); }
                3 | 4 => { let _ = send.send_blocking(Event::Message(message)); }
                5 if message == "ok" => {
                    success = wait_for(&child, Duration::from_secs(5)).is_some_and(|s| s.code() == Some(0));
                    return Ok(());
                }
                _ => return Err(()),
            }
        }
    })();
    if wait_for(&child, Duration::ZERO).is_none() {
        terminate(&child);
    }
    if wait_for(&child, Duration::from_secs(3)).is_none() {
        if let Ok(mut c) = child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
    let _ = send.send_blocking(Event::Complete(success));
}

impl Transaction for Authentication {
    fn start(&self) {
        let args = helper_arguments(self.0.fingerprint.get(), self.0.password_only.get());
        let spawned = helper_command(&self.0.helper, args).spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(_) => {
                (self.0.callbacks.complete)(false);
                return;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        *self.0.stdin.borrow_mut() = child.stdin.take();
        let child = Arc::new(Mutex::new(child));
        *self.0.child.borrow_mut() = Some(child.clone());
        let (send, receive) = async_channel::unbounded();
        let spawn = std::thread::Builder::new().name("lock-authentication".into())
            .spawn(move || read_helper(stdout, child, send));
        if spawn.is_err() {
            self.cancel();
            (self.0.callbacks.complete)(false);
            return;
        }
        let this = self.clone();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            while let Ok(event) = receive.recv().await {
                let last = matches!(event, Event::Complete(_));
                this.dispatch(event);
                if last {
                    break;
                }
            }
        });
    }

    fn respond(&self, text: &str) {
        let inner = &self.0;
        if inner.cancelled.get() || !inner.waiting.get() || inner.child.borrow().is_none() {
            return;
        }
        inner.waiting.set(false);
        let secret = text.as_bytes();
        if secret.len() > LIMIT || secret.contains(&0) {
            self.cancel();
            (inner.callbacks.complete)(false);
            return;
        }
        let mut packet: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(4 + secret.len()));
        packet.extend_from_slice(&(secret.len() as u32).to_be_bytes());
        packet.extend_from_slice(secret);
        if let Some(stdin) = inner.stdin.borrow_mut().as_mut() {
            let _ = stdin.write_all(&packet).and_then(|_| stdin.flush());
        }
        // `packet` is zeroed on drop.
    }

    fn cancel(&self) {
        self.0.cancelled.set(true);
        self.0.waiting.set(false);
        if let Some(child) = self.0.child.borrow().as_ref() {
            terminate(child);
        }
    }

    fn waiting(&self) -> bool { self.0.waiting.get() }
    fn set_password_only(&self, value: bool) { self.0.password_only.set(value) }
    fn password_only(&self) -> bool { self.0.password_only.get() }
    fn set_fingerprint(&self, value: bool) { self.0.fingerprint.set(value) }
    fn fingerprint(&self) -> bool { self.0.fingerprint.get() }
    fn as_any(&self) -> &dyn std::any::Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_arguments_select_fixed_policies() {
        assert_eq!(helper_arguments(false, false), &[] as &[&str]);
        assert_eq!(helper_arguments(false, true), &["--password-only"]);
        assert_eq!(helper_arguments(true, true), &["--fingerprint"]);
    }

    /// Regression: Python spawns the helper with close_fds=True; a descriptor
    /// the locker holds without O_CLOEXEC must not reach the helper.
    #[test]
    fn helper_does_not_inherit_descriptors() {
        let file = std::fs::File::open("/dev/null").unwrap();
        use std::os::fd::AsRawFd;
        let fd = file.as_raw_fd();
        // SAFETY: clear FD_CLOEXEC on our own descriptor.
        unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
        // `test -e` is a shell builtin: it stats the descriptor without opening any.
        let probe = format!("if [ -e /proc/self/fd/{fd} ]; then echo leaked; else echo closed; fi");
        let mut command = helper_command(std::path::Path::new("/bin/sh"), &["-c", &probe]);
        command.stdin(Stdio::null());
        assert_eq!(String::from_utf8_lossy(&command.output().unwrap().stdout).trim(), "closed", "fd {fd} leaked");
        let mut plain = Command::new("/bin/sh");
        plain.args(["-c", &probe]).stdin(Stdio::null());
        let leaked = String::from_utf8_lossy(&plain.output().unwrap().stdout).trim() == "leaked";
        assert!(leaked, "control: a plain Command does inherit the fd");
    }

    #[test]
    fn missing_helper_completes_false() {
        let done = Rc::new(RefCell::new(Vec::new()));
        let d = done.clone();
        let auth = Authentication::with_helper(PathBuf::from("/nonexistent/tessel-lock-pam"), Callbacks {
            prompt: Box::new(|_, _| panic!("no prompt")), message: Box::new(|_| {}),
            complete: Box::new(move |s| d.borrow_mut().push(s)) });
        auth.start();
        assert_eq!(*done.borrow(), vec![false]);
    }
}
