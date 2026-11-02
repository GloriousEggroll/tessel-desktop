//! `applet_pages._keepawake`: the Keep Awake popup's controls, embedded.
use std::cell::RefCell;
use std::rc::Rc;

use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use tessel_keep_awake::ui::{Controls, KeepAwakePopup};

use crate::embed::{embed, Embed};
use crate::{Factories, KeepAwakeService};

/// `page.controller` / `page.backend` of the Keep Awake page.
pub struct KeepAwakePage {
    pub controller: KeepAwakePopup,
    pub client: Rc<dyn KeepAwakeService>,
}

/// The popup drives the client through [`Controls`].
struct Forward(Rc<dyn KeepAwakeService>);

impl Controls for Forward {
    fn ready(&self) -> bool { self.0.ready() }
    fn active(&self) -> bool { self.0.active() }
    fn requested(&self) -> bool { self.0.requested() }
    fn error(&self) -> String { self.0.error() }
    fn duration(&self) -> u32 { self.0.duration() }
    fn until(&self) -> Option<f64> { self.0.until() }
    fn set_enabled(&self, enabled: bool) { self.0.set_enabled(enabled) }
    fn set_duration(&self, minutes: u32) { self.0.set_duration(minutes) }
    fn has_set_duration(&self) -> bool { self.0.has_set_duration() }
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let controller: Rc<RefCell<Option<KeepAwakePopup>>> = Rc::default();
    let changed: Rc<dyn Fn()> = {
        let controller = controller.clone();
        Rc::new(move || {
            let current = controller.borrow().clone();
            if let Some(controller) = current {
                controller.refresh();
            }
        })
    };
    let client: Rc<dyn KeepAwakeService> = match &factories.keepawake {
        Some(factory) => factory(changed.clone()),
        None => {
            let changed = changed.clone();
            Rc::new(tessel_keep_awake::client::KeepAwakeClient::new(move || changed()))
        }
    };
    let popup = KeepAwakePopup::new(Rc::new(Forward(client.clone())), "bottom", 76, true);
    controller.replace(Some(popup.clone()));
    if let Some(inner) = popup.window().child().and_then(|surface| surface.first_child()) {
        inner.set_margin_top(24);
        inner.set_margin_bottom(24);
        inner.set_margin_start(24);
        inner.set_margin_end(24);
    }
    let cleanup = {
        let (controller, client) = (controller.clone(), client.clone());
        move || {
            if let Some(popup) = controller.take() {
                popup.destroy();
            }
            client.close();
        }
    };
    // "Not running in this session" is a state, not an error: only failures
    // while the service is connected stay red.
    let ready = client.clone();
    let page = embed(popup.window(), host, Embed {
        refresh: Some(changed), cleanup: Some(Box::new(cleanup)), footer: Some(popup.status().clone()),
        real_error: Some(Rc::new(move || ready.ready())), ..Default::default()
    });
    page.set_controller(Rc::new(KeepAwakePage { controller: popup, client: client.clone() }));
    client.start();
    Ok(page)
}
