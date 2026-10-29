//! Minimal WebKitGTK 6.0 binding, loaded with `dlopen` on first use.
//!
//! Python imports WebKit only when the admin panel is first opened
//! (`gi.require_version('WebKit', '6.0')` inside `AdminPanel.__init__`);
//! linking `libwebkitgtk-6.0` eagerly would cost every idle dashboard
//! ~7–8 MB of private RSS and start-up time, so the few entry points the
//! panel (and its tests) need are resolved at run time instead. Properties
//! and signals go through GObject (`title`, `uri`,
//! `decide-policy`, `load-failed`, `load-changed`, `web-process-terminated`,
//! `context-menu`).
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::OnceLock;

use glib::translate::ToGlibPtr;
use gtk::prelude::*;

pub const LIBRARY: &str = "libwebkitgtk-6.0.so.4";

/// `WebKitPolicyDecisionType`.
pub const NAVIGATION_ACTION: i32 = 0;
pub const NEW_WINDOW_ACTION: i32 = 1;
/// `WebKitLoadEvent`.
pub const LOAD_FINISHED: i32 = 3;

type Callback = unsafe extern "C" fn(*mut glib::gobject_ffi::GObject, *mut gio::ffi::GAsyncResult, *mut c_void);

struct Api {
    view_new: unsafe extern "C" fn() -> *mut gtk::ffi::GtkWidget,
    load_uri: unsafe extern "C" fn(*mut c_void, *const c_char),
    get_settings: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    set_developer_extras: unsafe extern "C" fn(*mut c_void, c_int),
    load_html: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char),
    get_action: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    get_request: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    is_user_gesture: unsafe extern "C" fn(*mut c_void) -> c_int,
    request_uri: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    ignore: unsafe extern "C" fn(*mut c_void),
    evaluate: unsafe extern "C" fn(*mut c_void, *const c_char, isize, *const c_char, *const c_char, *mut c_void, Option<Callback>, *mut c_void),
    evaluate_finish: unsafe extern "C" fn(*mut c_void, *mut gio::ffi::GAsyncResult, *mut *mut glib::ffi::GError) -> *mut c_void,
    value_to_string: unsafe extern "C" fn(*mut c_void) -> *mut c_char,
    permission_deny: unsafe extern "C" fn(*mut c_void),
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> Result<&'static Api, String> {
    API.get_or_init(|| unsafe {
        let name = CString::new(LIBRARY).unwrap();
        let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if handle.is_null() {
            let error = libc::dlerror();
            return Err(if error.is_null() { format!("{LIBRARY}: cannot open") } else { CStr::from_ptr(error).to_string_lossy().into_owned() });
        }
        macro_rules! sym {
            ($name:literal) => {{
                let symbol = libc::dlsym(handle, concat!($name, "\0").as_ptr() as *const c_char);
                if symbol.is_null() {
                    return Err(format!("{LIBRARY}: missing {}", $name));
                }
                std::mem::transmute::<*mut c_void, _>(symbol)
            }};
        }
        Ok(Api {
            view_new: sym!("webkit_web_view_new"),
            load_uri: sym!("webkit_web_view_load_uri"),
            get_settings: sym!("webkit_web_view_get_settings"),
            set_developer_extras: sym!("webkit_settings_set_enable_developer_extras"),
            load_html: sym!("webkit_web_view_load_html"),
            get_action: sym!("webkit_navigation_policy_decision_get_navigation_action"),
            get_request: sym!("webkit_navigation_action_get_request"),
            is_user_gesture: sym!("webkit_navigation_action_is_user_gesture"),
            request_uri: sym!("webkit_uri_request_get_uri"),
            ignore: sym!("webkit_policy_decision_ignore"),
            evaluate: sym!("webkit_web_view_evaluate_javascript"),
            evaluate_finish: sym!("webkit_web_view_evaluate_javascript_finish"),
            value_to_string: sym!("jsc_value_to_string"),
            permission_deny: sym!("webkit_permission_request_deny"),
        })
    }).as_ref().map_err(Clone::clone)
}

fn enum_arg(value: &glib::Value) -> i32 {
    // SAFETY: the signal argument is a GEnum value.
    unsafe { glib::gobject_ffi::g_value_get_enum(value.to_glib_none().0) }
}

/// Hardening (not in Python): WebKit's default network session keeps its
/// website data (HSTS, salts, local storage, caches) in
/// `$XDG_DATA_HOME/webkitgtk` and `$XDG_CACHE_HOME/webkitgtk`, created under
/// the process umask (0755 with umask 022). Create them user-only (0700) or
/// strip group/other bits from an existing directory the user owns, before the
/// first view is made. Symlinks and foreign-owned directories are left alone.
pub fn harden_data_dirs(bases: &[std::path::PathBuf]) {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    for base in bases {
        let dir = base.join("webkitgtk");
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        let Ok(handle) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(&dir) else { continue };
        let Ok(info) = handle.metadata() else { continue };
        // SAFETY: geteuid has no preconditions.
        if info.is_dir() && info.uid() == unsafe { libc::geteuid() } && info.mode() & 0o077 != 0 {
            let _ = handle.set_permissions(std::fs::Permissions::from_mode(info.mode() & 0o700));
        }
    }
}

/// A `WebKitPolicyDecision` passed to `decide-policy` (valid during the handler).
pub struct Decision {
    pointer: *mut c_void,
    pub kind: i32,
}

impl Decision {
    /// The request URI of a navigation / new-window decision.
    pub fn uri(&self) -> Option<String> {
        let api = api().ok()?;
        // SAFETY: navigation decisions (kinds 0/1) are WebKitNavigationPolicyDecisions;
        // the action/request/uri are owned by the decision (transfer none).
        unsafe {
            let action = (api.get_action)(self.pointer);
            if action.is_null() {
                return None;
            }
            let request = (api.get_request)(action);
            if request.is_null() {
                return None;
            }
            let uri = (api.request_uri)(request);
            (!uri.is_null()).then(|| CStr::from_ptr(uri).to_string_lossy().into_owned())
        }
    }

    pub fn is_user_gesture(&self) -> bool {
        let Ok(api) = api() else { return false };
        // SAFETY: as above.
        unsafe {
            let action = (api.get_action)(self.pointer);
            !action.is_null() && (api.is_user_gesture)(action) != 0
        }
    }

    pub fn ignore(&self) {
        if let Ok(api) = api() {
            // SAFETY: a live WebKitPolicyDecision.
            unsafe { (api.ignore)(self.pointer) }
        }
    }
}

/// A `WebKitWebView` (a `GtkWidget`).
#[derive(Clone)]
pub struct WebView(gtk::Widget);

impl WebView {
    /// Loads WebKitGTK on first use; `Err` when it is not installed.
    pub fn new() -> Result<Self, String> {
        let api = api()?;
        harden_data_dirs(&[glib::user_data_dir(), glib::user_cache_dir()]);
        // SAFETY: webkit_web_view_new returns a floating GtkWidget reference.
        let widget: gtk::Widget = unsafe { glib::translate::from_glib_none((api.view_new)()) };
        Ok(Self(widget))
    }

    pub fn widget(&self) -> &gtk::Widget { &self.0 }
    fn pointer(&self) -> *mut c_void { self.0.as_ptr() as *mut c_void }

    pub fn set_enable_developer_extras(&self, enabled: bool) {
        let Ok(api) = api() else { return };
        // SAFETY: the view's own WebKitSettings (transfer none).
        unsafe {
            let settings = (api.get_settings)(self.pointer());
            if !settings.is_null() {
                (api.set_developer_extras)(settings, enabled as c_int);
            }
        }
    }

    pub fn load_uri(&self, uri: &str) {
        let (Ok(api), Ok(uri)) = (api(), CString::new(uri)) else { return };
        // SAFETY: valid view and NUL-terminated string.
        unsafe { (api.load_uri)(self.pointer(), uri.as_ptr()) }
    }

    pub fn load_html(&self, html: &str, base_uri: Option<&str>) {
        let (Ok(api), Ok(html)) = (api(), CString::new(html)) else { return };
        let base = base_uri.and_then(|b| CString::new(b).ok());
        // SAFETY: valid view; base may be NULL.
        unsafe { (api.load_html)(self.pointer(), html.as_ptr(), base.as_ref().map_or(std::ptr::null(), |b| b.as_ptr())) }
    }

    pub fn title(&self) -> Option<String> { self.0.property::<Option<String>>("title") }
    pub fn uri(&self) -> Option<String> { self.0.property::<Option<String>>("uri") }

    /// `decide-policy`; return true when handled (decision used), false for WebKit's default.
    pub fn connect_decide_policy(&self, handler: impl Fn(&Decision) -> bool + 'static) {
        self.0.connect_local("decide-policy", false, move |values| {
            let object = values[1].get::<glib::Object>().ok()?;
            let decision = Decision { pointer: object.as_ptr() as *mut c_void, kind: enum_arg(&values[2]) };
            Some(handler(&decision).to_value())
        });
    }

    /// `load-failed` (the handler result is always "handled").
    pub fn connect_load_failed(&self, handler: impl Fn() + 'static) {
        self.0.connect_local("load-failed", false, move |_| {
            handler();
            Some(true.to_value())
        });
    }

    pub fn connect_web_process_terminated(&self, handler: impl Fn() + 'static) {
        self.0.connect_local("web-process-terminated", false, move |_| {
            handler();
            None
        });
    }

    /// `permission-request` → denied and handled. WebKit's default handler
    /// also denies; this makes "never microphone/camera/geolocation/
    /// notifications/…" explicit instead of relying on that default.
    /// Returns how many requests were denied (for tests).
    pub fn deny_permission_requests(&self) -> std::rc::Rc<std::cell::Cell<u32>> {
        // SAFETY (signal): `permission-request` is (WebKitWebView*, WebKitPermissionRequest*) -> gboolean.
        let denied = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let count = denied.clone();
        self.0.connect_local("permission-request", false, move |values| {
            if let (Ok(api), Ok(request)) = (api(), values[1].get::<glib::Object>()) {
                // SAFETY: the signal's WebKitPermissionRequest, alive for the handler.
                unsafe { (api.permission_deny)(request.as_ptr() as *mut c_void) };
                count.set(count.get() + 1);
            }
            Some(true.to_value())
        });
        denied
    }

    /// `context-menu` → handled (no menu).
    pub fn suppress_context_menu(&self) {
        self.0.connect_local("context-menu", false, |_| Some(true.to_value()));
    }

    pub fn connect_load_changed(&self, handler: impl Fn(i32) + 'static) {
        self.0.connect_local("load-changed", false, move |values| {
            handler(enum_arg(&values[1]));
            None
        });
    }

    /// `evaluate_javascript(code, -1, None, None, None, callback)` → the result as a string.
    pub fn evaluate_javascript(&self, code: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let Ok(api) = api() else { return done(Err("WebKit unavailable".into())) };
        let Ok(script) = CString::new(code) else { return done(Err("NUL in script".into())) };
        type Done = Box<dyn FnOnce(Result<String, String>)>;
        unsafe extern "C" fn finished(source: *mut glib::gobject_ffi::GObject, result: *mut gio::ffi::GAsyncResult, data: *mut c_void) {
            // SAFETY: data is the Box<Done> leaked below; source is the view.
            let done: Box<Done> = Box::from_raw(data as *mut Done);
            let outcome = match self::api() {
                Ok(api) => {
                    let mut error: *mut glib::ffi::GError = std::ptr::null_mut();
                    let value = (api.evaluate_finish)(source as *mut c_void, result, &mut error);
                    if value.is_null() {
                        let message = if error.is_null() { "JavaScript failed".to_owned() }
                                      else { CStr::from_ptr((*error).message).to_string_lossy().into_owned() };
                        if !error.is_null() {
                            glib::ffi::g_error_free(error);
                        }
                        Err(message)
                    } else {
                        let text = (api.value_to_string)(value);
                        let string = if text.is_null() { String::new() } else { CStr::from_ptr(text).to_string_lossy().into_owned() };
                        glib::ffi::g_free(text as *mut c_void);
                        glib::gobject_ffi::g_object_unref(value as *mut glib::gobject_ffi::GObject);
                        Ok(string)
                    }
                }
                Err(error) => Err(error.clone()),
            };
            done(outcome);
        }
        let data = Box::into_raw(Box::new(Box::new(done) as Done)) as *mut c_void;
        // SAFETY: valid view, script outlives the call (copied by WebKit), callback frees data.
        unsafe {
            (api.evaluate)(self.pointer(), script.as_ptr(), -1, std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), Some(finished), data);
        }
    }
}


#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn webkit_data_dirs_become_user_only() {
        let root = std::env::temp_dir().join(format!("kasumi-webkit-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (fresh, existing, link) = (root.join("data"), root.join("cache"), root.join("linked"));
        std::fs::create_dir_all(existing.join("webkitgtk")).unwrap();
        std::fs::set_permissions(existing.join("webkitgtk"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let target = root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(&link).unwrap();
        std::os::unix::fs::symlink(&target, link.join("webkitgtk")).unwrap();
        super::harden_data_dirs(&[fresh.clone(), existing.clone(), link]);
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&fresh.join("webkitgtk")), 0o700);
        assert_eq!(mode(&existing.join("webkitgtk")), 0o700);
        assert_eq!(mode(&target), 0o755, "symlinks are not followed");
        let _ = std::fs::remove_dir_all(root);
    }
}
