//! workspace_settings.py: workspace controls embedded in Desktop Behavior.
use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use suite_core::tr;
use suite_gtk::utils::accessible;

use crate::compat::Result;
use crate::workspace_config::{WorkspaceConfig, WorkspaceState, MAX_WORKSPACES};

/// What WorkspaceControls needs from its backend (Python duck typing).
pub trait WorkspaceBackend {
    fn load(&self) -> Result<WorkspaceState>;
    fn save(&self, count: i64, expected: &WorkspaceState) -> Result<WorkspaceState>;
}

impl WorkspaceBackend for WorkspaceConfig {
    fn load(&self) -> Result<WorkspaceState> { WorkspaceConfig::load(self) }
    fn save(&self, count: i64, expected: &WorkspaceState) -> Result<WorkspaceState> { WorkspaceConfig::save(self, count, expected) }
}

pub struct WorkspaceControls {
    pub backend: RefCell<Option<Rc<dyn WorkspaceBackend>>>,
    pub state: RefCell<Option<WorkspaceState>>,
    pub widget: gtk::Box,
    pub count: gtk::SpinButton,
    pub limit: gtk::Label,
    pub status: gtk::Label,
}

fn muted(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).wrap(true).max_width_chars(56).build();
    label.add_css_class("settings-muted");
    label
}

impl WorkspaceControls {
    /// `WorkspaceControls(backend=None)`: None uses the running session's rc.xml.
    pub fn new(backend: Option<Rc<dyn WorkspaceBackend>>) -> Rc<WorkspaceControls> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 14);
        widget.add_css_class("settings-card");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        heading.add_css_class("settings-section-heading");
        let title = gtk::Label::builder().label(tr!("Workspaces")).xalign(0.0).build();
        title.add_css_class("settings-section-title");
        heading.append(&title);
        widget.append(&heading);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        row.append(&gtk::Label::builder().label(tr!("Number of workspaces")).xalign(0.0).hexpand(true).build());
        let count = gtk::SpinButton::with_range(1.0, MAX_WORKSPACES as f64, 1.0);
        count.set_numeric(true);
        count.set_width_chars(3);
        count.set_max_width_chars(5);
        accessible(&count, &tr!("Number of workspaces"));
        row.append(&count);
        widget.append(&row);
        let limit = muted("");
        widget.append(&limit);
        widget.append(&muted(&tr!("Shared across all monitors. Changes apply immediately when you press Apply and persist after signing out. Windows on removed workspaces move to workspace 1.")));
        // Muted while workspaces are unavailable (e.g. outside labwc); red only
        // when saving fails.
        let status = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(56).visible(false).build();
        status.add_css_class("gs-footer");
        widget.append(&status);
        let controls = Rc::new(WorkspaceControls {
            backend: RefCell::new(backend), state: RefCell::new(None), widget, count, limit, status,
        });
        controls.refresh();
        controls
    }

    fn backend(&self) -> Result<Rc<dyn WorkspaceBackend>> {
        if let Some(backend) = self.backend.borrow().clone() {
            return Ok(backend);
        }
        let backend: Rc<dyn WorkspaceBackend> = Rc::new(WorkspaceConfig::session()?);
        *self.backend.borrow_mut() = Some(backend.clone());
        Ok(backend)
    }

    pub fn refresh(&self) {
        if let Some(state) = self.state.borrow().as_ref() {
            if self.count.value_as_int() as i64 != state.count {
                return; // Keep an edit that has not been applied yet.
            }
        }
        match self.backend().and_then(|backend| backend.load()) {
            Ok(state) => {
                let maximum = MAX_WORKSPACES.max(state.count);
                self.count.set_range(1.0, maximum as f64);
                self.count.set_value(state.count as f64);
                self.count.set_sensitive(true);
                self.limit.set_text(&tr!("This control supports 1–{value1} workspaces. labwc has no fixed workspace-count cap.",
                                         value1 = maximum));
                self.status.set_visible(false);
                *self.state.borrow_mut() = Some(state);
            }
            Err(error) => {
                self.count.set_sensitive(false);
                self.status.remove_css_class("error");
                self.status.set_text(error.message());
                self.status.set_visible(true);
            }
        }
    }

    /// Save the count; false (with a red status) when saving failed.
    pub fn apply(&self) -> bool {
        if !self.count.is_sensitive() {
            return true; // An unavailable workspace backend must not block hot corners.
        }
        self.count.update();
        let expected = self.state.borrow().clone().unwrap_or(WorkspaceState { count: 1, signature: String::new() });
        let result = self.backend().and_then(|backend| backend.save(self.count.value_as_int() as i64, &expected));
        match result {
            Ok(state) => {
                *self.state.borrow_mut() = Some(state);
                self.status.set_visible(false);
                true
            }
            Err(error) => {
                self.status.add_css_class("error");
                self.status.set_text(error.message());
                self.status.set_visible(true);
                false
            }
        }
    }
}
