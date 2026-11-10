//! Deterministic date settings model for previews and GUI checks only
//! (fake_date_backend.py). Never touches the system bus or clock.

use crate::backend::{zoned, Clock, DateState, DateSystem, ZonedTime};
use crate::civil::Date;
use crate::tz;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum FakeOp {
    Ntp(bool),
    Date(Date),
    DateTime(Date, Clock),
    Timezone(String),
}

pub struct FakeDateBackend {
    on_changed: RefCell<Rc<dyn Fn()>>,
    on_error: RefCell<Rc<dyn Fn(&str)>>,
    pub state: RefCell<DateState>,
    pub history: RefCell<Vec<FakeOp>>,
    pub defer_changes: Cell<bool>,
    pub fail_next: RefCell<Option<String>>,
    pub pending: RefCell<Option<FakeOp>>,
    pub closed: Cell<bool>,
    now: RefCell<ZonedTime>,
}

fn zone(name: &str) -> tz::ZoneRef {
    tz::zoneinfo(name).unwrap_or_else(|_| tz::Zone::utc())
}

impl FakeDateBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) -> Rc<Self> {
        let timezone = "America/Denver".to_owned();
        let state = DateState {
            ready: true, busy: false, error: String::new(), timezone: timezone.clone(),
            timezones: ["America/Denver", "America/New_York", "Asia/Tokyo", "Etc/UTC", "Europe/London"].map(String::from).to_vec(),
            timezones_ready: true, ntp: true, can_ntp: true, synchronized: true,
        };
        Rc::new(Self { on_changed: RefCell::new(Rc::new(on_changed)), on_error: RefCell::new(Rc::new(on_error)),
                       state: RefCell::new(state), history: RefCell::default(), defer_changes: Cell::new(false),
                       fail_next: RefCell::new(None), pending: RefCell::new(None), closed: Cell::new(false),
                       now: RefCell::new(zoned(2026, 9, 18, 14, 35, 0, 0, &zone(&timezone))) })
    }

    pub fn set_callbacks(&self, on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) {
        self.on_changed.replace(Rc::new(on_changed));
        self.on_error.replace(Rc::new(on_error));
    }

    pub fn changed(&self) {
        if !self.closed.get() {
            let callback = self.on_changed.borrow().clone();
            callback();
        }
    }

    /// `update(**values)`: change state; a new timezone keeps the instant.
    pub fn update(&self, change: impl FnOnce(&mut DateState)) {
        let before = self.state.borrow().timezone.clone();
        change(&mut self.state.borrow_mut());
        let after = self.state.borrow().timezone.clone();
        if after != before {
            let moved = self.now.borrow().astimezone(&zone(&after)).unwrap();
            self.now.replace(moved);
        }
        self.changed();
    }

    pub fn set_now(&self, value: ZonedTime) {
        self.now.replace(value);
        self.changed();
    }

    /// Move the clock as real time does: without a change notification.
    pub fn tick_to(&self, value: ZonedTime) {
        self.now.replace(value);
    }

    fn fail(&self, message: &str) -> bool {
        self.state.borrow_mut().error = message.into();
        let callback = self.on_error.borrow().clone();
        callback(message);
        self.changed();
        false
    }

    fn queue(&self, operation: FakeOp) -> bool {
        let (ready, busy) = { let s = self.state.borrow(); (s.ready, s.busy) };
        if self.closed.get() || !ready {
            return self.fail("System date settings are unavailable.");
        }
        if busy {
            return self.fail("Another date settings change is still pending.");
        }
        self.history.borrow_mut().push(operation.clone());
        if let Some(error) = self.fail_next.take() {
            return self.fail(&error);
        }
        {
            let mut s = self.state.borrow_mut();
            s.error.clear();
            s.busy = true;
        }
        self.pending.replace(Some(operation));
        self.changed();
        if !self.defer_changes.get() {
            self.complete_pending(None);
        }
        true
    }

    pub fn complete_pending(&self, error: Option<&str>) -> bool {
        let operation = self.pending.take().expect("No date settings change is pending.");
        self.state.borrow_mut().busy = false;
        if let Some(error) = error {
            return self.fail(error);
        }
        match operation {
            FakeOp::Ntp(value) => {
                let mut s = self.state.borrow_mut();
                s.ntp = value;
                s.synchronized = value;
            }
            FakeOp::Date(value) => {
                let moved = self.now.borrow().with_date(value);
                self.now.replace(moved);
                self.state.borrow_mut().synchronized = false;
            }
            FakeOp::DateTime(date, time) => {
                let name = self.state.borrow().timezone.clone();
                let (y, m, d) = date.parts();
                self.now.replace(zoned(y, m, d, time.hour, time.minute, time.second, time.micro, &zone(&name)));
                self.state.borrow_mut().synchronized = false;
            }
            FakeOp::Timezone(name) => {
                self.state.borrow_mut().timezone = name.clone();
                let moved = self.now.borrow().astimezone(&zone(&name)).unwrap();
                self.now.replace(moved);
            }
        }
        self.state.borrow_mut().error.clear();
        self.changed();
        true
    }
}

impl DateSystem for FakeDateBackend {
    fn state(&self) -> DateState { self.state.borrow().clone() }
    fn now(&self) -> ZonedTime { self.now.borrow().clone() }
    fn start(&self) { self.changed(); }
    fn close(&self) {
        self.closed.set(true);
        self.pending.replace(None);
        self.state.borrow_mut().busy = false;
    }
    fn set_ntp(&self, enabled: bool) -> bool {
        if !self.state.borrow().can_ntp {
            return self.fail("Automatic time synchronization is unavailable.");
        }
        self.queue(FakeOp::Ntp(enabled))
    }
    fn set_date(&self, selected: Date) -> bool {
        if self.state.borrow().ntp {
            return self.fail("Turn off automatic date and time before applying a date.");
        }
        self.queue(FakeOp::Date(selected))
    }
    fn set_datetime(&self, selected: Date, time: Clock) -> bool {
        if self.state.borrow().ntp {
            return self.fail("Turn off automatic date and time before applying a date and time.");
        }
        self.queue(FakeOp::DateTime(selected, time))
    }
    fn set_timezone(&self, name: &str) -> bool {
        if !self.state.borrow().timezones.iter().any(|z| z == name) {
            return self.fail("Choose a supported time zone.");
        }
        if self.state.borrow().ntp {
            return self.fail("Turn off automatic date and time before changing the time zone.");
        }
        self.queue(FakeOp::Timezone(name.into()))
    }
}
