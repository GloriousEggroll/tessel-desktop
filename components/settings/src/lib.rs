//! Global Settings (`org.tessel.GlobalSettings`): the shell (window,
//! sidebar, search, CLI); pages live in gs-applets, gs-desktop and gs-system.
pub mod app;
pub mod cli;
pub mod search;
pub mod window;

pub use gs_base;
use std::rc::Rc;

/// `shortcuts_page.CheatSheet(app)` from gs-system.
pub fn default_cheat_sheet(app: &gtk::Application) -> Rc<dyn app::CheatSheet> {
    cheat::make(app)
}

mod cheat;
