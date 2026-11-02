//! `applet_pages._notifications`: the notification service's settings page.
use std::cell::RefCell;
use std::rc::Rc;

use gs_base::{Host, SettingsPage};
use tessel_notifications::ui::{Client, SettingsPage as NotificationSettingsPage};
use serde_json::{Map, Value};

use crate::embed::{embed, Embed};
use crate::{Factories, NotificationService};

/// `page.controller` / `page.client` of the Notifications page.
pub struct NotificationsPage {
    pub controller: NotificationSettingsPage,
    pub client: Rc<dyn NotificationService>,
}

struct Forward(Rc<dyn NotificationService>);

impl Client for Forward {
    fn state(&self) -> Map<String, Value> { self.0.state() }
    fn ready(&self) -> bool { self.0.ready() }
    fn error(&self) -> String { self.0.error() }
    fn set_settings(&self, patch: Value) { self.0.set_settings(patch) }
    fn clear_history(&self) { self.0.clear_history() }
    fn clear_app(&self, key: &str) { self.0.clear_app(key) }
    fn mark_read(&self) { self.0.mark_read() }
    fn dismiss(&self, ident: u32) { self.0.dismiss(ident) }
    fn activate(&self, ident: u32, key: &str) { self.0.activate(ident, key) }
    fn show_settings(&self) { self.0.show_settings() }
}

/// `settings_search` targets of this section (`getattr(controller, target)`).
fn register_targets(page: &SettingsPage, view: &NotificationSettingsPage) {
    page.add_target("dnd_switch", &view.0.dnd_switch);
    page.add_target("fullscreen_switch", &view.0.fullscreen_switch);
    page.add_target("timeout_spin", &view.0.timeout_spin);
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let view: Rc<RefCell<Option<NotificationSettingsPage>>> = Rc::default();
    let changed: Rc<dyn Fn()> = {
        let view = view.clone();
        Rc::new(move || {
            let current = view.borrow().clone();
            if let Some(view) = current {
                view.refresh();
            }
        })
    };
    let client: Rc<dyn NotificationService> = match &factories.notifications {
        Some(factory) => factory(changed.clone()),
        None => {
            let changed = changed.clone();
            Rc::new(tessel_notifications::client::NotificationClient::new(move || changed()))
        }
    };
    let settings = NotificationSettingsPage::new(Rc::new(Forward(client.clone())));
    view.replace(Some(settings.clone()));
    let cleanup = {
        let (client, settings) = (client.clone(), settings.clone());
        move || {
            client.close();
            settings.destroy();
        }
    };
    let ready = client.clone();
    let page = embed(settings.widget(), host, Embed {
        refresh: Some(changed), cleanup: Some(Box::new(cleanup)), margin: 24, scroll: true,
        footer: Some(settings.0.status_label.clone()), real_error: Some(Rc::new(move || ready.ready())),
        ..Default::default()
    });
    register_targets(&page, &settings);
    page.set_controller(Rc::new(NotificationsPage { controller: settings, client: client.clone() }));
    client.start();
    Ok(page)
}
