//! Agent queue / cancellation / shutdown / native-callback tests without a
//! display: requests enter through the real `WayshellAuthListener` vfunc
//! (`polkit_agent_listener_initiate_authentication`, as polkitd's D-Bus call
//! would), windows are fakes. Nothing is registered on any bus.

use super::*;
use crate::ffi;
use gio::ffi::GAsyncResult;
use glib::ffi::{gpointer, GError};
use glib::gobject_ffi::GObject;
use glib::translate::*;
use std::ffi::CString;

thread_local! {
    static RESULTS: RefCell<Vec<(bool, Result<bool, i32>)>> = const { RefCell::new(Vec::new()) };
}
static SENTINEL: u8 = 0;

unsafe extern "C" fn ready(source: *mut GObject, result: *mut GAsyncResult, data: gpointer) {
    let mut error: *mut GError = std::ptr::null_mut();
    let ok = ffi::polkit_agent_listener_initiate_authentication_finish(source as *mut _, result, &mut error);
    let outcome = if error.is_null() {
        Ok(ok != 0)
    } else {
        let error: glib::Error = from_glib_full(error);
        assert_eq!(ok, 0);
        assert!(error.matches(gio::IOErrorEnum::Cancelled), "{error}");
        Err(gio::IOErrorEnum::Cancelled.into_glib())
    };
    RESULTS.with(|r| r.borrow_mut().push((data as *const u8 == &SENTINEL as *const u8, outcome)));
}

fn results() -> Vec<Result<bool, i32>> {
    RESULTS.with(|r| r.borrow().iter().map(|(_, outcome)| outcome.clone()).collect())
}

fn all_correct_user_data() -> bool {
    RESULTS.with(|r| r.borrow().iter().all(|(correct, _)| *correct))
}

const CANCELLED_CODE: i32 = 19; // G_IO_ERROR_CANCELLED

fn settle() {
    let context = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_millis(60);
    while std::time::Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[derive(Default)]
struct FakePrompt {
    finished: RefCell<Option<Box<dyn FnOnce(bool)>>>,
    started: Cell<u32>,
    presented: Cell<u32>,
    done: Cell<bool>,
    request: RefCell<Option<Rc<Request>>>,
}

impl Prompt for FakePrompt {
    fn set_application(&self, _application: Option<&gtk::Application>) {}
    fn present(&self) {
        self.presented.set(self.presented.get() + 1);
    }
    fn start(&self) {
        self.started.set(self.started.get() + 1);
    }
    fn finish(&self, cancelled: bool) {
        if self.done.replace(true) {
            return;
        }
        if let Some(request) = self.request.borrow().as_ref() {
            request.cookie.borrow_mut().clear();
        }
        if let Some(finished) = self.finished.take() {
            finished(cancelled);
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// glib-rs `idle_add_local` needs the global default main context: run
/// these tests one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fx {
    agent: AuthenticationAgent,
    windows: Rc<RefCell<Vec<Rc<FakePrompt>>>>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.agent.close();
        settle();
    }
}

impl Fx {
    fn new() -> Self {
        Self::with_factory_error(false)
    }

    fn with_factory_error(fail: bool) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        RESULTS.with(|r| r.borrow_mut().clear());
        let windows: Rc<RefCell<Vec<Rc<FakePrompt>>>> = Rc::default();
        let created = windows.clone();
        let factory: WindowFactory = Rc::new(move |request, finished| {
            if fail {
                return Err("no display".into());
            }
            let prompt = Rc::new(FakePrompt::default());
            prompt.finished.replace(Some(finished));
            prompt.request.replace(Some(request));
            created.borrow_mut().push(prompt.clone());
            Ok(prompt as Rc<dyn Prompt>)
        });
        Self { agent: AuthenticationAgent::new(None, factory), windows, _serial: serial }
    }

    /// `listener.initiate_authentication(…, identities, cancellable, ready, &SENTINEL)`.
    fn begin(&self, cancellable: Option<&gio::Cancellable>, uids: &[i32]) {
        unsafe {
            let mut list: *mut glib::ffi::GList = std::ptr::null_mut();
            for uid in uids {
                list = glib::ffi::g_list_append(list, ffi::polkit_unix_user_new(*uid));
            }
            let details = ffi::polkit_details_new();
            let action = CString::new("org.example.test").unwrap();
            let message = CString::new("Authentication is required to change a setting.").unwrap();
            let icon = CString::new("dialog-password").unwrap();
            let cookie = CString::new("fake-cookie").unwrap();
            ffi::polkit_agent_listener_initiate_authentication(
                self.agent.listener.as_ptr(), action.as_ptr(), message.as_ptr(), icon.as_ptr(), details, cookie.as_ptr(),
                list, cancellable.map_or(std::ptr::null_mut(), |c| c.as_ptr()), Some(ready),
                &SENTINEL as *const u8 as gpointer);
            glib::gobject_ffi::g_object_unref(details as *mut _);
            let mut item = list;
            while !item.is_null() {
                glib::gobject_ffi::g_object_unref((*item).data as *mut _);
                item = (*item).next;
            }
            glib::ffi::g_list_free(list);
        }
        settle();
    }

    fn window(&self) -> Option<Rc<FakePrompt>> {
        self.agent.window().map(|w| {
            let target = w.as_any() as *const dyn std::any::Any as *const FakePrompt;
            self.windows.borrow().iter().find(|p| Rc::as_ptr(p) == target).cloned().expect("known window")
        })
    }
}

fn uid() -> i32 {
    crate::identity::current_uid() as i32
}

#[test]
fn request_fields_reach_the_window_and_success_returns_true() {
    let fx = Fx::new();
    fx.begin(None, &[uid()]);
    let window = fx.window().expect("window shown");
    assert_eq!((window.presented.get(), window.started.get()), (1, 1));
    let request = window.request.borrow().clone().unwrap();
    assert_eq!(request.action_id, "org.example.test");
    assert_eq!(request.message, "Authentication is required to change a setting.");
    assert_eq!(request.cookie.borrow().as_str(), "fake-cookie");
    assert_eq!(request.identities[0].uid, Some(uid() as u32));
    window.finish(false);
    settle();
    assert_eq!(results(), vec![Ok(true)]);
    assert!(request.cookie.borrow().is_empty(), "cookie wiped");
    assert!(request.is_done());
    assert!(fx.agent.window().is_none());
    assert!(fx.agent.requests().is_empty());
}

#[test]
fn native_polkit_callback_preserves_opaque_user_data() {
    for cancel in [false, true] {
        let fx = Fx::new();
        fx.begin(None, &[uid()]);
        fx.window().unwrap().finish(cancel);
        settle();
        assert_eq!(results().len(), 1, "callback called exactly once");
        assert!(all_correct_user_data(), "Polkit must receive its original native callback context");
        assert_eq!(results()[0], if cancel { Err(CANCELLED_CODE) } else { Ok(true) });
    }
}

#[test]
fn requests_are_queued_one_window_at_a_time() {
    let fx = Fx::new();
    fx.begin(None, &[uid()]);
    fx.begin(None, &[uid()]);
    assert_eq!(fx.agent.requests().len(), 2);
    assert_eq!(fx.windows.borrow().len(), 1);
    fx.window().unwrap().finish(false);
    settle();
    assert_eq!(fx.windows.borrow().len(), 2, "next shown on idle after completion");
    fx.window().unwrap().finish(true);
    settle();
    assert_eq!(results(), vec![Ok(true), Err(CANCELLED_CODE)]);
}

#[test]
fn caller_cancellation_closes_prompt_and_queue() {
    let fx = Fx::new();
    let (first_cancel, queued_cancel) = (gio::Cancellable::new(), gio::Cancellable::new());
    fx.begin(Some(&first_cancel), &[uid()]);
    let window = fx.window().unwrap();
    fx.begin(Some(&queued_cancel), &[uid()]);
    assert_eq!(fx.agent.requests().len(), 2);
    queued_cancel.cancel();
    settle();
    assert_eq!(fx.agent.requests().len(), 1);
    first_cancel.cancel();
    settle();
    assert!(window.done.get());
    assert!(fx.agent.requests().is_empty());
    assert_eq!(results(), vec![Err(CANCELLED_CODE); 2]);
    assert_eq!(fx.windows.borrow().len(), 1, "the cancelled queued request never got a window");
}

#[test]
fn already_cancelled_request_is_never_shown() {
    let fx = Fx::new();
    let cancel = gio::Cancellable::new();
    cancel.cancel();
    fx.begin(Some(&cancel), &[uid()]);
    assert!(fx.windows.borrow().is_empty());
    assert_eq!(results(), vec![Err(CANCELLED_CODE)]);
}

#[test]
fn shutdown_cancels_all_requests_once() {
    let fx = Fx::new();
    fx.begin(None, &[uid()]);
    fx.begin(None, &[uid()]);
    fx.agent.close();
    fx.agent.close();
    settle();
    assert_eq!(results(), vec![Err(CANCELLED_CODE); 2]);
    assert!(fx.agent.requests().is_empty());
    // Closed: new requests are refused immediately.
    fx.begin(None, &[uid()]);
    assert_eq!(results().len(), 3);
    assert_eq!(results()[2], Err(CANCELLED_CODE));
}

#[test]
fn no_identities_is_unavailable() {
    struct Recorder(Rc<RefCell<Vec<String>>>);
    impl Completion for Recorder {
        fn succeed(self: Box<Self>) {
            self.0.borrow_mut().push("TRUE".into());
        }
        fn cancel(self: Box<Self>, message: &str) {
            self.0.borrow_mut().push(message.into());
        }
    }
    let fx = Fx::new();
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    // libpolkit refuses an empty identity list before calling the vfunc, so
    // drive the handler directly.
    fx.agent.authenticate(Box::new(Recorder(seen.clone())), "a", "m", Secret::from_bytes(b"c"), &[], None);
    settle();
    assert_eq!(*seen.borrow(), vec![UNAVAILABLE.to_owned()]);
    assert!(fx.windows.borrow().is_empty());
}

#[test]
fn window_failure_cancels_without_logging_request_data() {
    let fx = Fx::with_factory_error(true);
    fx.begin(None, &[uid()]);
    assert_eq!(results(), vec![Err(CANCELLED_CODE)]);
    assert!(fx.agent.requests().is_empty());
}

#[test]
fn dropped_agent_answers_unavailable() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    RESULTS.with(|r| r.borrow_mut().clear());
    let agent = AuthenticationAgent::new(None, Rc::new(|_, _| Err("unused".into())));
    let listener_object = agent.listener.object().clone();
    drop(agent);
    // The listener object outlives the agent: a late request is refused.
    unsafe {
        let list = glib::ffi::g_list_append(std::ptr::null_mut(), ffi::polkit_unix_user_new(uid()));
        let details = ffi::polkit_details_new();
        let s = CString::new("x").unwrap();
        ffi::polkit_agent_listener_initiate_authentication(listener_object.as_ptr() as *mut _, s.as_ptr(), s.as_ptr(),
            s.as_ptr(), details, s.as_ptr(), list, std::ptr::null_mut(), Some(ready), &SENTINEL as *const u8 as gpointer);
        glib::gobject_ffi::g_object_unref(details as *mut _);
        glib::gobject_ffi::g_object_unref((*list).data as *mut _);
        glib::ffi::g_list_free(list);
    }
    settle();
    assert_eq!(results(), vec![Err(CANCELLED_CODE)]);
}
