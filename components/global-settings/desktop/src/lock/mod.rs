//! Lock Screen (lock_page.py + lock-screen/lock_settings.py, lock_config.py,
//! lock_weather.configured_weather and power_client.PowerClient). Kept in
//! submodules so wave 5 can move them into the lock-screen crate unchanged.
/// Moved to the shared `suite-lock` crate (wave 5).
pub use suite_lock::config;
pub mod power;
pub mod settings;
pub mod weather;

use gs_base::{Host, SettingsPage};

pub use settings::{LockOptions, LockSettings};

/// `lock_page.build_page("lock", host)` with injection.
pub fn build_lock_page(host: &Host, options: LockOptions) -> Result<SettingsPage, String> {
    let controller = LockSettings::new(host, options)?;
    let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&controller.root));
    let page = SettingsPage::new(scroll);
    let (c1, c2, c3) = (controller.clone(), controller.clone(), controller.clone());
    page.set_close(move || c1.close());
    page.set_activate(move || c2.activate());
    page.set_can_close(move || c3.can_close());
    for (key, widget) in controller.search_targets.borrow().iter() {
        page.add_target(key, widget);
    }
    page.set_controller(controller);
    Ok(page)
}
