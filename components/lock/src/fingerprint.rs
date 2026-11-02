//! lock_fingerprint.py: detect an fprintd reader with enrolled fingers; never
//! verifies or claims it. Verification is a second PAM transaction
//! (`tessel-lock-pam --fingerprint`) run by the lock application.
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const NAME: &str = "net.reactivated.Fprint";
pub const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
pub const MANAGER: &str = "net.reactivated.Fprint.Manager";
pub const DEVICE: &str = "net.reactivated.Fprint.Device";
pub const TOUCH: &str = "Touch the fingerprint reader";
pub const SWIPE: &str = "Swipe your finger on the fingerprint reader";

pub type Reply = Box<dyn FnOnce(Result<glib::Variant, glib::Error>)>;

/// The subset of `Gio.DBusConnection.call` the detector uses (fakeable).
pub trait FprintBus {
    #[allow(clippy::too_many_arguments)]
    fn call(&self, path: &str, interface: &str, method: &str, parameters: Option<glib::Variant>, reply: &str,
            auto_start: bool, cancellable: &gio::Cancellable, done: Reply);
}

impl FprintBus for gio::DBusConnection {
    fn call(&self, path: &str, interface: &str, method: &str, parameters: Option<glib::Variant>, reply: &str,
            auto_start: bool, cancellable: &gio::Cancellable, done: Reply) {
        let flags = if auto_start { gio::DBusCallFlags::NONE } else { gio::DBusCallFlags::NO_AUTO_START };
        let reply = glib::VariantType::new(reply).ok();
        gio::DBusConnection::call(self, Some(NAME), path, interface, method, parameters.as_ref(), reply.as_deref(), flags,
                                  5000, Some(cancellable), done);
    }
}

struct Inner {
    found: Box<dyn Fn(&str)>,
    bus: RefCell<Option<Rc<dyn FprintBus>>>,
    username: String,
    device: RefCell<Option<String>>,
    closed: Cell<bool>,
    cancel: gio::Cancellable,
}

/// Calls `found(prompt)` once when the current user has enrolled fingers.
#[derive(Clone)]
pub struct FingerprintDetector(Rc<Inner>);

pub fn current_username() -> String {
    // SAFETY: getpwuid on our uid; the name is copied immediately.
    unsafe {
        let account = libc::getpwuid(libc::getuid());
        if account.is_null() || (*account).pw_name.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr((*account).pw_name).to_string_lossy().into_owned()
    }
}

impl FingerprintDetector {
    pub fn new(found: impl Fn(&str) + 'static, bus: Option<Rc<dyn FprintBus>>, username: Option<String>) -> Self {
        Self(Rc::new(Inner { found: Box::new(found), bus: RefCell::new(bus),
            username: username.unwrap_or_else(current_username), device: RefCell::new(None), closed: false.into(),
            cancel: gio::Cancellable::new() }))
    }

    pub fn start(&self) {
        if self.0.bus.borrow().is_some() {
            self.manager();
            return;
        }
        let this = self.clone();
        gio::bus_get(gio::BusType::System, Some(&self.0.cancel), move |result| {
            let Ok(bus) = result else { return };
            *this.0.bus.borrow_mut() = Some(Rc::new(bus));
            if !this.0.closed.get() {
                this.manager();
            }
        });
    }

    fn call(&self, path: &str, interface: &str, method: &str, parameters: Option<glib::Variant>, reply: &str,
            auto_start: bool, done: impl FnOnce(&Self, glib::Variant) + 'static) {
        let Some(bus) = self.0.bus.borrow().clone() else { return };
        let this = self.clone();
        bus.call(path, interface, method, parameters, reply, auto_start, &self.0.cancel, Box::new(move |result| {
            if this.0.closed.get() {
                return;
            }
            // No fprintd, reader or enrollment: password only.
            if let Ok(value) = result {
                done(&this, value);
            }
        }));
    }

    fn manager(&self) {
        // fprintd is D-Bus activated, so this first call may start it.
        self.call(MANAGER_PATH, MANAGER, "GetDefaultDevice", None, "(o)", true, |this, value| {
            let Some(path) = value.try_child_value(0).and_then(|v| v.str().map(str::to_owned)) else { return };
            *this.0.device.borrow_mut() = Some(path.clone());
            this.call(&path, DEVICE, "ListEnrolledFingers", Some((this.0.username.as_str(),).to_variant()), "(as)", false,
                      |this, value| this.enrolled(value));
        });
    }

    fn enrolled(&self, value: glib::Variant) {
        let fingers = value.try_child_value(0).and_then(|v| v.get::<Vec<String>>()).unwrap_or_default();
        if fingers.is_empty() {
            return;
        }
        let Some(device) = self.0.device.borrow().clone() else { return };
        self.call(&device, "org.freedesktop.DBus.Properties", "Get", Some((DEVICE, "scan-type").to_variant()), "(v)", false,
                  |this, result| {
                      let scan = result.try_child_value(0).and_then(|v| v.as_variant()).and_then(|v| v.str().map(str::to_owned));
                      (this.0.found)(if scan.as_deref() == Some("swipe") { SWIPE } else { TOUCH });
                  });
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test_lock_fingerprint.py FakeBus: answers detection calls only.
    struct FakeBus {
        fingers: Vec<String>,
        device: bool,
        scan: String,
        calls: RefCell<Vec<String>>,
    }

    impl FprintBus for FakeBus {
        fn call(&self, path: &str, interface: &str, method: &str, parameters: Option<glib::Variant>, _reply: &str,
                auto_start: bool, _cancellable: &gio::Cancellable, done: Reply) {
            self.calls.borrow_mut().push(method.to_string());
            let error = |name: &str| glib::Error::new(gio::IOErrorEnum::Failed, name);
            let value = match method {
                "GetDefaultDevice" => {
                    assert!(auto_start && path == MANAGER_PATH && interface == MANAGER);
                    if self.device { Ok(("/net/reactivated/Fprint/Device/0".to_string(),).to_variant()) }
                    else { Err(error("net.reactivated.Fprint.Error.NoSuchDevice")) }
                }
                "ListEnrolledFingers" => {
                    assert!(!auto_start);
                    assert_eq!(parameters.unwrap().child_value(0).str(), Some("tester"));
                    if self.fingers.is_empty() { Err(error("net.reactivated.Fprint.Error.NoEnrolledPrints")) }
                    else { Ok((self.fingers.clone(),).to_variant()) }
                }
                "Get" => Ok((self.scan.to_variant(),).to_variant()),
                other => panic!("unexpected fprintd call {other}"),
            };
            done(value);
        }
    }

    fn detect(fingers: &[&str], device: bool, scan: &str) -> (Vec<String>, Vec<String>) {
        let found = Rc::new(RefCell::new(Vec::new()));
        let bus = Rc::new(FakeBus { fingers: fingers.iter().map(|s| s.to_string()).collect(), device, scan: scan.into(),
                                    calls: RefCell::new(Vec::new()) });
        let f = found.clone();
        FingerprintDetector::new(move |p| f.borrow_mut().push(p.to_string()), Some(bus.clone()), Some("tester".into())).start();
        let calls = bus.calls.borrow().clone();
        let found = found.borrow().clone();
        (found, calls)
    }

    #[test]
    fn enrolled_finger_offers_hint_without_claiming_reader() {
        let (found, calls) = detect(&["right-index-finger"], true, "press");
        assert_eq!(found, [TOUCH]);
        assert_eq!(calls, ["GetDefaultDevice", "ListEnrolledFingers", "Get"]);
        let (found, _) = detect(&["right-index-finger"], true, "swipe");
        assert_eq!(found, [SWIPE]);
    }

    #[test]
    fn absent_reader_or_enrollment_falls_back_silently() {
        for (fingers, device) in [(&["x"][..], false), (&[][..], true)] {
            let (found, calls) = detect(fingers, device, "press");
            assert!(found.is_empty());
            assert!(!calls.iter().any(|c| c == "Claim" || c == "VerifyStart"));
        }
    }

    #[test]
    fn closed_detector_ignores_replies() {
        let found = Rc::new(RefCell::new(Vec::new()));
        let f = found.clone();
        let bus = Rc::new(FakeBus { fingers: vec!["x".into()], device: true, scan: "press".into(), calls: RefCell::new(vec![]) });
        let detector = FingerprintDetector::new(move |p| f.borrow_mut().push(p.to_string()), Some(bus), Some("tester".into()));
        detector.close();
        detector.start();
        assert!(found.borrow().is_empty());
    }
}
