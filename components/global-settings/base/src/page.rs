//! `page_api.SettingsPage` plus what Python's pages reached through duck
//! typing (`page.controller.search_targets`, `reveal_setting`, `page.tabs`).
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;

/// `settings_search.Setting` (frozen dataclass).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub section: &'static str,
    pub key: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    pub keywords: &'static str,
    pub labels: &'static [&'static str],
    /// Python looked up `getattr(controller, target)`; Rust pages register
    /// that widget in [`SettingsPage::search_targets`] under this name.
    pub target: &'static str,
    pub subpage: &'static str,
}

impl Setting {
    pub const fn new(section: &'static str, key: &'static str, title: &'static str, detail: &'static str) -> Self {
        Setting { section, key, title, detail, keywords: "", labels: &[], target: "", subpage: "" }
    }
}

/// What a page may ask of the window hosting it (Python passed the
/// `GlobalSettingsWindow` itself; Welcome passes its own window).
pub trait PageHost {
    /// `host.show_section(key)` (pages such as Battery link to Bluetooth).
    fn show_section(&self, section: &str);
    /// `host.get_application()`.
    fn application(&self) -> Option<gtk::Application>;
    /// The host window (`host` itself in Python: transient parent, display).
    fn window(&self) -> gtk::Window;
    /// `host.show_page_error(section, error, title, icon)`.
    fn show_page_error(&self, section: &str, error: &str, title: Option<&str>, icon: &str);
    /// `host.closed`.
    fn closed(&self) -> bool { false }
}

pub type Host = Rc<dyn PageHost>;

type Callback = Rc<dyn Fn()>;

/// One embedded page and its lifecycle (`page_api.SettingsPage`).
pub struct SettingsPage {
    pub widget: gtk::Widget,
    close: RefCell<Callback>,
    activate: RefCell<Callback>,
    deactivate: RefCell<Callback>,
    can_close: RefCell<Rc<dyn Fn() -> bool>>,
    /// `controller.search_targets` and every `getattr(controller, target)`
    /// widget, keyed by the setting key or target name.
    pub search_targets: RefCell<HashMap<String, gtk::Widget>>,
    /// `controller.reveal_setting(setting)` (e.g. switch to a tab first).
    pub reveal_setting: RefCell<Option<Rc<dyn Fn(&Setting)>>>,
    /// `page.tabs` (a Gtk.Stack whose child names are sub-pages).
    pub tabs: RefCell<Option<gtk::Stack>>,
    /// `page.error_state` (set on the shared error page).
    pub error_state: RefCell<Option<gtk::Box>>,
    /// Handles kept for diagnostics and fake-backend checks
    /// (`page.controller`, `page.backend`, `page.client` in Python).
    pub controller: RefCell<Option<Rc<dyn Any>>>,
}

impl SettingsPage {
    pub fn new(widget: impl IsA<gtk::Widget>) -> Self {
        SettingsPage {
            widget: widget.upcast(),
            close: RefCell::new(Rc::new(|| {})),
            activate: RefCell::new(Rc::new(|| {})),
            deactivate: RefCell::new(Rc::new(|| {})),
            can_close: RefCell::new(Rc::new(|| true)),
            search_targets: RefCell::new(HashMap::new()),
            reveal_setting: RefCell::new(None),
            tabs: RefCell::new(None),
            error_state: RefCell::new(None),
            controller: RefCell::new(None),
        }
    }

    pub fn with_close(self, f: impl Fn() + 'static) -> Self { self.set_close(f); self }
    pub fn with_activate(self, f: impl Fn() + 'static) -> Self { self.set_activate(f); self }
    pub fn with_deactivate(self, f: impl Fn() + 'static) -> Self { self.set_deactivate(f); self }
    pub fn with_can_close(self, f: impl Fn() -> bool + 'static) -> Self { self.set_can_close(f); self }

    pub fn set_close(&self, f: impl Fn() + 'static) { *self.close.borrow_mut() = Rc::new(f); }
    pub fn set_activate(&self, f: impl Fn() + 'static) { *self.activate.borrow_mut() = Rc::new(f); }
    pub fn set_deactivate(&self, f: impl Fn() + 'static) { *self.deactivate.borrow_mut() = Rc::new(f); }
    pub fn set_can_close(&self, f: impl Fn() -> bool + 'static) { *self.can_close.borrow_mut() = Rc::new(f); }

    pub fn close(&self) { let f = self.close.borrow().clone(); f() }
    pub fn activate(&self) { let f = self.activate.borrow().clone(); f() }
    pub fn deactivate(&self) { let f = self.deactivate.borrow().clone(); f() }
    pub fn can_close(&self) -> bool { let f = self.can_close.borrow().clone(); f() }

    pub fn add_target(&self, name: &str, widget: &impl IsA<gtk::Widget>) {
        self.search_targets.borrow_mut().insert(name.to_owned(), widget.clone().upcast());
    }

    /// The spotlight target for a setting: `search_targets[key]`, else the
    /// widget registered under `setting.target`.
    pub fn search_target(&self, setting: &Setting) -> Option<gtk::Widget> {
        let targets = self.search_targets.borrow();
        targets.get(setting.key).cloned()
            .or_else(|| (!setting.target.is_empty()).then(|| targets.get(setting.target).cloned()).flatten())
    }

    pub fn set_controller<T: Any>(&self, value: Rc<T>) { *self.controller.borrow_mut() = Some(value); }

    pub fn controller<T: Any>(&self) -> Option<Rc<T>> {
        self.controller.borrow().clone().and_then(|value| value.downcast::<T>().ok())
    }
}
