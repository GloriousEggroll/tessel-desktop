//! mouse_polling_ui.py: small, asynchronous hardware-rate card for the input settings page.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gs_base::widgets::run_in_thread;
use gtk::glib;
use gtk::prelude::*;
use suite_core::tr;
use suite_gtk::utils::accessible;

use crate::input_devices::InputDevice;
use crate::mouse_polling::{Polling, PollingBackend, PollingState};

pub struct PollingCard {
    /// The card itself (Python's Gtk.Box subclass), class `gs-card`.
    pub widget: gtk::Box,
    pub device: Option<InputDevice>,
    pub backend: Arc<dyn Polling>,
    busy_changed: Rc<dyn Fn()>,
    pub closed: Rc<Cell<bool>>,
    pub busy: Cell<bool>,
    pub reading: Cell<bool>,
    rendering: Cell<bool>,
    pub state: RefCell<PollingState>,
    pub options: RefCell<Vec<i64>>,
    pub current: gtk::Label,
    pub detail: gtk::Label,
    pub choice: gtk::DropDown,
    pub apply_button: gtk::Button,
    pub refresh_button: gtk::Button,
    pub timer: RefCell<Option<glib::SourceId>>,
}

impl PollingCard {
    pub fn new(device: Option<InputDevice>, backend: Option<Arc<dyn Polling>>, busy_changed: Rc<dyn Fn()>) -> Rc<PollingCard> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("gs-card");
        Self::with_root(root, device, backend, busy_changed)
    }

    /// Build the card inside an existing (cleared) box, so the input page can
    /// keep one stable search target across renders.
    pub fn with_root(root: gtk::Box, device: Option<InputDevice>, backend: Option<Arc<dyn Polling>>, busy_changed: Rc<dyn Fn()>) -> Rc<PollingCard> {
        suite_gtk::utils::remove_children(&root);
        let heading = gtk::Label::builder().label(tr!("Polling rate")).xalign(0.0).build();
        heading.add_css_class("gs-section-title");
        root.append(&heading);
        let current = gtk::Label::builder().label(tr!("Reading current polling rate…")).xalign(0.0).build();
        root.append(&current);
        let detail = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(64).build();
        detail.add_css_class("input-detail");
        root.append(&detail);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let choice = gtk::DropDown::builder().hexpand(true).build();
        accessible(&choice, &tr!("Polling rate"));
        row.append(&choice);
        let apply_button = gtk::Button::with_label(&tr!("Apply rate"));
        row.append(&apply_button);
        let refresh_button = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text(tr!("Refresh current polling rate")).build();
        row.append(&refresh_button);
        root.append(&row);
        let this = Rc::new(PollingCard {
            widget: root, device: device.clone(), backend: backend.unwrap_or_else(|| Arc::new(PollingBackend::default())), busy_changed,
            closed: Rc::new(Cell::new(false)), busy: Cell::new(false), reading: Cell::new(false), rendering: Cell::new(false),
            state: RefCell::new(PollingState::default()), options: RefCell::new(Vec::new()), current, detail, choice, apply_button,
            refresh_button, timer: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        this.choice.connect_selected_notify(move |_| if let Some(t) = weak.upgrade() { if !t.rendering.get() { t.buttons() } });
        let weak = Rc::downgrade(&this);
        this.apply_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.apply() });
        let weak = Rc::downgrade(&this);
        this.refresh_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.refresh(false) });
        this.buttons();
        if device.is_none() {
            this.current.set_text(&tr!("Select an individual mouse to view its current polling rate."));
            row.set_visible(false);
        } else {
            this.refresh(false);
            let weak = Rc::downgrade(&this);
            let id = glib::timeout_add_seconds_local(5, move || match weak.upgrade() {
                Some(t) => t.tick(),
                None => glib::ControlFlow::Break,
            });
            *this.timer.borrow_mut() = Some(id);
        }
        this
    }

    /// `_tick()`.
    pub fn tick(self: &Rc<Self>) -> glib::ControlFlow {
        if self.closed.get() {
            self.timer.borrow_mut().take();
            return glib::ControlFlow::Break;
        }
        if self.widget.is_mapped() {
            self.refresh(true);
        }
        glib::ControlFlow::Continue
    }

    /// `_value()`: the selected rate.
    pub fn value(&self) -> Option<i64> {
        let index = self.choice.selected() as usize;
        self.options.borrow().get(index).copied()
    }

    fn buttons(&self) {
        let state = self.state.borrow();
        let adjustable = state.adjustable();
        self.choice.set_visible(adjustable);
        self.apply_button.set_visible(adjustable);
        let idle = !self.busy.get() && !self.reading.get();
        self.choice.set_sensitive(idle);
        let value = self.value();
        self.apply_button.set_sensitive(idle && value.is_some_and(|v| state.rates.contains(&v)) && value != state.current);
        self.refresh_button.set_sensitive(idle);
    }

    pub fn refresh(self: &Rc<Self>, quiet: bool) {
        if self.closed.get() || self.busy.get() || self.reading.get() {
            return;
        }
        let Some(device) = self.device.clone() else { return };
        self.reading.set(true);
        if !quiet {
            self.buttons();
        }
        let backend = self.backend.clone();
        let this = Rc::downgrade(self);
        run_in_thread(self.closed.clone(), move || Ok(backend.read(&device).unwrap_or_else(|e| PollingState::detail(e.message()))),
                      move |result: Result<PollingState, String>| if let Some(t) = this.upgrade() {
                          t.ready(result.unwrap_or_else(|e| PollingState::detail(&e)), String::new(), false)
                      });
    }

    fn ready(&self, state: PollingState, error: String, applied: bool) {
        if self.closed.get() {
            return;
        }
        let chosen = self.value();
        let pending = chosen.is_some() && chosen != self.state.borrow().current && !applied;
        *self.state.borrow_mut() = state.clone();
        self.reading.set(false);
        self.busy.set(false);
        self.current.set_text(&match state.current {
            Some(current) if current != 0 => tr!("Current polling rate: {value1} Hz", value1 = current),
            _ => tr!("Current polling rate: Not reported"),
        });
        let mut detail = if error.is_empty() { suite_core::i18n::translate(&state.detail) } else { error.clone() };
        if applied && error.is_empty() {
            detail = format!("{} {detail}", tr!("Polling rate applied."));
        }
        self.detail.set_text(&detail);
        self.rendering.set(true);
        let options = state.rates.clone();
        if options != *self.options.borrow() {
            *self.options.borrow_mut() = options.clone();
            let names: Vec<String> = options.iter().map(|v| tr!("{value1} Hz", value1 = *v)).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.choice.set_model(Some(&gtk::StringList::new(&refs)));
        }
        let selected = if pending && chosen.is_some_and(|c| options.contains(&c)) { chosen } else { state.current };
        self.choice.set_selected(selected.and_then(|s| options.iter().position(|o| *o == s)).map(|i| i as u32)
                                 .unwrap_or(gtk::INVALID_LIST_POSITION));
        self.rendering.set(false);
        self.buttons();
        (self.busy_changed)();
    }

    pub fn apply(self: &Rc<Self>) {
        let value = self.value();
        let state = self.state.borrow().clone();
        let Some(value) = value.filter(|v| !self.closed.get() && !self.busy.get() && !self.reading.get() && state.adjustable() && state.rates.contains(v))
        else { return };
        if Some(value) == state.current {
            return;
        }
        let Some(device) = self.device.clone() else { return };
        self.busy.set(true);
        self.detail.set_text(&tr!("Applying polling rate…"));
        self.buttons();
        (self.busy_changed)();
        let backend = self.backend.clone();
        let this = Rc::downgrade(self);
        run_in_thread(self.closed.clone(), move || {
            Ok(match backend.apply(&device, &state, value) {
                Ok(state) => (state, String::new()),
                Err(failure) => (backend.read(&device).unwrap_or_default(), failure.to_string()),
            })
        }, move |result: Result<(PollingState, String), String>| if let Some(t) = this.upgrade() {
            let (state, error) = result.unwrap_or_else(|e| (PollingState::default(), e));
            t.ready(state, error, true)
        });
    }

    pub fn close(&self) {
        self.closed.set(true);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
    }

}
