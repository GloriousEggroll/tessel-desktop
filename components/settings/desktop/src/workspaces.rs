//! Glue to gs-system's workspace controls/config (workspace_settings.py,
//! workspace_config.py are owned by the system page crate).
use std::rc::Rc;

use gs_system::workspace_settings::WorkspaceControls;
use gtk::prelude::*;

use crate::dock::WorkspacePart;

impl WorkspacePart for WorkspaceControls {
    fn widget(&self) -> gtk::Widget { self.widget.clone().upcast() }
    fn count(&self) -> gtk::SpinButton { self.count.clone() }
    fn limit(&self) -> gtk::Label { self.limit.clone() }
    fn status(&self) -> gtk::Label { self.status.clone() }
    fn refresh(&self) { WorkspaceControls::refresh(self) }
    fn apply(&self) -> bool { WorkspaceControls::apply(self) }
}

/// `WorkspaceControls()` (the running session's rc.xml).
pub fn default_workspace_controls() -> Result<Rc<dyn WorkspacePart>, String> {
    Ok(WorkspaceControls::new(None) as Rc<dyn WorkspacePart>)
}
