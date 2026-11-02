//! `weather_page.py`: weather settings shared with the panel's weather popup.
use std::rc::Rc;

use gs_base::{Host, SettingsPage};
use tessel_weather::settings::WeatherSettings;

use crate::Factories;

/// `page.controller` of the Weather page.
pub struct WeatherPage {
    pub controller: Rc<WeatherSettings>,
}

pub(crate) fn build(_host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let controller = WeatherSettings::new(factories.weather_store.clone(), factories.weather_services.clone().unwrap_or_default());
    let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&controller.root));
    let (c1, c2, c3) = (controller.clone(), controller.clone(), controller.clone());
    let page = SettingsPage::new(scroll).with_close(move || c1.close()).with_activate(move || c2.activate())
        .with_deactivate(move || c3.deactivate());
    for (key, widget) in controller.search_targets() {
        page.add_target(key, &widget);
    }
    page.set_controller(Rc::new(WeatherPage { controller }));
    Ok(page)
}
