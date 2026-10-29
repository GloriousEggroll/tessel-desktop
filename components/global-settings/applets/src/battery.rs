//! `applet_pages._battery`: a live battery monitor with charge controls.
use std::cell::RefCell;
use std::rc::Rc;

use battery_manager::ui::{AppHooks, BatterySettingsWindow};
use gs_base::{Host, SettingsPage};
use serde_json::Value;
use suite_power::BatteryModel;

use crate::embed::{embed, Embed};
use crate::{BatterySource, Factories};

/// `page.controller` / `page.backend` of the Battery page.
pub struct BatteryPage {
    pub controller: Rc<BatterySettingsWindow>,
    pub backend: Rc<dyn BatterySource>,
}

/// Global Settings is not a `BatteryApplication`: no application error and
/// no Bluetooth launcher (the page links to the Bluetooth section instead).
struct NoApplication;

impl AppHooks for NoApplication {
    fn error(&self) -> String { String::new() }
    fn bluetooth_available(&self) -> bool { false }
    fn open_bluetooth(&self) -> bool { false }
}

struct Forward(Rc<dyn BatterySource>);

impl BatteryModel for Forward {
    fn power_ready(&self) -> bool { self.0.power_ready() }
    fn power_source_known(&self) -> bool { self.0.power_source_known() }
    fn on_battery(&self) -> bool { self.0.on_battery() }
    fn has_battery(&self) -> bool { self.0.has_battery() }
    fn percentage(&self) -> Option<f64> { self.0.percentage() }
    fn busy(&self) -> bool { self.0.busy() }
    fn error(&self) -> String { self.0.error() }
    fn batteries(&self) -> Vec<Value> { self.0.batteries() }
    fn charge_limit(&self) -> Value { self.0.charge_limit() }
    fn set_charge_thresholds(&self, path: &str, enabled: bool) -> bool { self.0.set_charge_thresholds(path, enabled) }
    fn set_charge_limit(&self, maximum: &Value) -> bool { self.0.set_charge_limit(maximum) }
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let application = crate::application(host)?;
    let view: Rc<RefCell<Option<Rc<BatterySettingsWindow>>>> = Rc::default();
    let changed: Rc<dyn Fn()> = {
        let view = view.clone();
        Rc::new(move || {
            let current = view.borrow().clone();
            if let Some(view) = current {
                view.refresh();
            }
        })
    };
    let failed: Rc<dyn Fn(&str)> = {
        let changed = changed.clone();
        Rc::new(move |_message: &str| changed())
    };
    let backend: Rc<dyn BatterySource> = match &factories.battery {
        Some(factory) => factory(changed.clone(), failed),
        None => {
            let (changed, failed) = (changed.clone(), failed.clone());
            Rc::new(suite_power::backend::PowerBackend::batteries_only(move || changed(), move |message| failed(message)))
        }
    };
    let navigate = Rc::downgrade(host);
    let open_bluetooth: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(host) = navigate.upgrade() {
            host.show_section("bluetooth");
        }
    });
    let window = BatterySettingsWindow::new(Some(&application), Rc::new(NoApplication), Rc::new(Forward(backend.clone())),
                                            Some(open_bluetooth));
    view.replace(Some(window.clone()));
    let cleanup = {
        let (view, backend) = (view.clone(), backend.clone());
        move || {
            if let Some(view) = view.take() {
                view.on_destroy();
            }
            backend.close();
        }
    };
    let page = embed(&window.window, host, Embed {
        refresh: Some(changed), cleanup: Some(Box::new(cleanup)), close_button: Some(window.close_button.clone().into()),
        ..Default::default()
    });
    page.set_controller(Rc::new(BatteryPage { controller: window, backend: backend.clone() }));
    backend.start();
    Ok(page)
}
