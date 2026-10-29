//! `WayshellAuthListener`: a `PolkitAgentListener` subclass registered from
//! Rust (replaces libwayshell-auth-listener.so / auth_listener.c).
//!
//! As in the C bridge, Polkit's opaque `GAsyncReadyCallback`/`user_data` never
//! leave C: `initiate_authentication` wraps them in a `GTask` owned by the
//! listener and the agent only ever completes that task ([`PolkitTask`]).
//! `initiate_authentication_finish` is `g_task_propagate_boolean`.

use crate::ffi;
use crate::secret::Secret;
use gio::ffi::{GAsyncReadyCallback, GAsyncResult, GCancellable, GTask};
use glib::ffi::{gboolean, gchar, gpointer, GError, GList, GType};
use glib::prelude::*;
use glib::translate::*;
use std::ffi::{CStr, CString};
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::sync::OnceLock;

pub const TYPE_NAME: &str = "WayshellAuthListener";
pub const OBJECT_PATH: &str = "/org/wayshell/Authentication";
const HANDLER_KEY: &str = "wayshell-auth-handler";

/// One `initiate_authentication` call, as the C bridge's `authenticate` signal
/// delivered it: `(task, action_id, message, cookie, identity strings, cancellable)`.
/// `icon_name` and `details` are ignored, like in Python.
pub struct AuthenticateCall {
    pub task: PolkitTask,
    pub action_id: String,
    pub message: String,
    pub cookie: Secret,
    /// `polkit_identity_to_string` of each identity, e.g. `unix-user:tcrider`.
    pub identities: Vec<String>,
    pub cancellable: Option<gio::Cancellable>,
}

type Handler = Rc<dyn Fn(AuthenticateCall)>;

/// How a request is completed (a `GTask` in production, a recorder in tests).
pub trait Completion {
    /// The conversation finished: `g_task_return_boolean(TRUE)`. This is not
    /// "authorized" — only polkit's trusted helper reports that to polkitd.
    fn succeed(self: Box<Self>);
    /// `G_IO_ERROR_CANCELLED` with `message`.
    fn cancel(self: Box<Self>, message: &str);
}

/// Owns one reference to the listener's `GTask`.
pub struct PolkitTask(Option<NonNull<GTask>>);

impl PolkitTask {
    fn take(&mut self) -> Option<NonNull<GTask>> {
        self.0.take()
    }

    fn return_cancelled(task: NonNull<GTask>, message: &str) {
        let error = glib::Error::new(gio::IOErrorEnum::Cancelled, message);
        // SAFETY: we own a task reference; g_task_return_error takes the GError.
        unsafe {
            gio::ffi::g_task_return_error(task.as_ptr(), error.into_glib_ptr());
            glib::gobject_ffi::g_object_unref(task.as_ptr() as *mut _);
        }
    }
}

impl Completion for PolkitTask {
    fn succeed(mut self: Box<Self>) {
        if let Some(task) = self.take() {
            unsafe {
                gio::ffi::g_task_return_boolean(task.as_ptr(), glib::ffi::GTRUE);
                glib::gobject_ffi::g_object_unref(task.as_ptr() as *mut _);
            }
        }
    }

    fn cancel(mut self: Box<Self>, message: &str) {
        if let Some(task) = self.take() {
            Self::return_cancelled(task, message);
        }
    }
}

impl Drop for PolkitTask {
    /// Never leave polkitd waiting: an abandoned task reports cancellation.
    fn drop(&mut self) {
        if let Some(task) = self.take() {
            Self::return_cancelled(task, "Authentication cancelled");
        }
    }
}

fn listener_type() -> GType {
    static TYPE: OnceLock<usize> = OnceLock::new();
    *TYPE.get_or_init(|| unsafe {
        let parent = ffi::polkit_agent_listener_get_type();
        let name = CString::new(TYPE_NAME).unwrap();
        let existing = glib::gobject_ffi::g_type_from_name(name.as_ptr());
        if existing != 0 {
            return existing as usize;
        }
        let mut query: glib::gobject_ffi::GTypeQuery = std::mem::zeroed();
        glib::gobject_ffi::g_type_query(parent, &mut query);
        assert!(query.class_size as usize >= std::mem::size_of::<ffi::PolkitAgentListenerClass>(),
                "unexpected PolkitAgentListenerClass size");
        glib::gobject_ffi::g_type_register_static_simple(parent, name.as_ptr(), query.class_size,
                                                         Some(class_init), query.instance_size, None, 0) as usize
    }) as GType
}

unsafe extern "C" fn class_init(class: gpointer, _data: gpointer) {
    let class = &mut *(class as *mut ffi::PolkitAgentListenerClass);
    class.initiate_authentication = Some(initiate);
    class.initiate_authentication_finish = Some(finish);
}

unsafe fn string(pointer: *const gchar) -> String {
    if pointer.is_null() { String::new() } else { CStr::from_ptr(pointer).to_string_lossy().into_owned() }
}

unsafe extern "C" fn initiate(listener: *mut ffi::PolkitAgentListener, action_id: *const gchar, message: *const gchar,
                              _icon_name: *const gchar, _details: *mut ffi::PolkitDetails, cookie: *const gchar,
                              identities: *mut GList, cancellable: *mut GCancellable, callback: GAsyncReadyCallback,
                              user_data: gpointer) {
    // The GTask keeps Polkit's callback + user_data; we only hold the task.
    let task = gio::ffi::g_task_new(listener as *mut _, cancellable, callback, user_data);
    let task = PolkitTask(NonNull::new(task));
    let mut names = Vec::new();
    let mut item = identities;
    while !item.is_null() {
        let text = ffi::polkit_identity_to_string((*item).data);
        names.push(string(text));
        glib::ffi::g_free(text as *mut _);
        item = (*item).next;
    }
    let object: glib::Object = from_glib_none(listener as *mut glib::gobject_ffi::GObject);
    let handler = object.data::<Handler>(HANDLER_KEY).map(|h| h.as_ref().clone());
    let call = AuthenticateCall {
        task,
        action_id: string(action_id),
        message: string(message),
        cookie: Secret::from_ptr(cookie),
        identities: names,
        cancellable: if cancellable.is_null() { None } else { Some(from_glib_none(cancellable)) },
    };
    match handler {
        Some(handler) => handler(call),
        None => Box::new(call.task).cancel("Authentication is unavailable"),
    }
}

unsafe extern "C" fn finish(_listener: *mut ffi::PolkitAgentListener, result: *mut GAsyncResult,
                            error: *mut *mut GError) -> gboolean {
    gio::ffi::g_task_propagate_boolean(result as *mut GTask, error)
}

/// A `WayshellAuthListener` instance plus its Rust request handler.
pub struct Listener {
    object: glib::Object,
}

impl Listener {
    pub fn new(handler: impl Fn(AuthenticateCall) + 'static) -> Self {
        let gtype: glib::Type = unsafe { from_glib(listener_type()) };
        let object = glib::Object::with_type(gtype);
        let handler: Handler = Rc::new(handler);
        unsafe { object.set_data(HANDLER_KEY, handler) };
        Self { object }
    }

    pub fn as_ptr(&self) -> *mut ffi::PolkitAgentListener {
        self.object.as_ptr() as *mut _
    }

    pub fn object(&self) -> &glib::Object {
        &self.object
    }

    /// `PolkitAgent.Listener.register(NONE, subject, object_path, None)`:
    /// libpolkit exports `org.freedesktop.PolicyKit1.AuthenticationAgent` at
    /// `object_path` on the system bus and calls
    /// `Authority.RegisterAuthenticationAgent(subject, locale, object_path)`.
    pub fn register(&self, subject: &Subject, object_path: &str) -> Result<Registration, glib::Error> {
        let path = CString::new(object_path).unwrap();
        let mut error: *mut GError = ptr::null_mut();
        let handle = unsafe {
            ffi::polkit_agent_listener_register(self.as_ptr(), ffi::POLKIT_AGENT_REGISTER_FLAGS_NONE,
                                                subject.0.as_ptr() as *mut _, path.as_ptr(), ptr::null_mut(), &mut error)
        };
        if handle.is_null() {
            return Err(unsafe { from_glib_full(error) });
        }
        Ok(Registration(Some(handle)))
    }

    /// Drop the Rust handler (pending tasks are completed by their owners).
    pub fn clear_handler(&self) {
        unsafe { self.object.steal_data::<Handler>(HANDLER_KEY) };
    }
}

/// `polkit_agent_listener_register`'s handle; unregistering also tells the
/// authority (`UnregisterAuthenticationAgent`).
pub struct Registration(Option<gpointer>);

impl Registration {
    pub fn unregister(&mut self) {
        if let Some(handle) = self.0.take() {
            unsafe { ffi::polkit_agent_listener_unregister(handle) };
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// A `PolkitSubject` (here always a `PolkitUnixSession`).
pub struct Subject(glib::Object);

impl Subject {
    /// `Polkit.UnixSession.new_for_process_sync(pid, None)`.
    pub fn unix_session_for_process(pid: i32) -> Result<Self, glib::Error> {
        let mut error: *mut GError = ptr::null_mut();
        let subject = unsafe { ffi::polkit_unix_session_new_for_process_sync(pid, ptr::null_mut(), &mut error) };
        if subject.is_null() {
            return Err(unsafe { from_glib_full(error) });
        }
        Ok(Self(unsafe { from_glib_full(subject as *mut glib::gobject_ffi::GObject) }))
    }
}

/// The process whose logind session the agent registers for: `$LABWC_PID`
/// (the compositor; user services are not in a session) or our own pid.
/// Python: `int(os.environ.get('LABWC_PID') or os.getpid())`.
pub fn session_pid(labwc_pid: Option<&str>) -> Result<i32, String> {
    match labwc_pid.filter(|value| !value.is_empty()) {
        None => Ok(std::process::id() as i32),
        Some(value) => {
            let text = value.trim().replace('_', "");
            text.strip_prefix('+').unwrap_or(&text).parse::<i32>()
                .map_err(|_| format!("invalid literal for int() with base 10: '{value}'"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_pid_prefers_labwc_pid() {
        assert_eq!(session_pid(Some("4785")), Ok(4785));
        assert_eq!(session_pid(Some(" 12 ")), Ok(12));
        assert_eq!(session_pid(Some("")), Ok(std::process::id() as i32));
        assert_eq!(session_pid(None), Ok(std::process::id() as i32));
        assert!(session_pid(Some("labwc")).is_err());
    }

    #[test]
    fn listener_type_derives_from_polkit_agent_listener() {
        let gtype: glib::Type = unsafe { from_glib(listener_type()) };
        assert_eq!(gtype.name(), TYPE_NAME);
        let parent: glib::Type = unsafe { from_glib(ffi::polkit_agent_listener_get_type()) };
        assert!(gtype.is_a(parent));
    }
}
