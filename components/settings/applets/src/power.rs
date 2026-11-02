//! `applet_pages._power`: Power settings saved through the resident power
//! service (`PowerClient`), never by Global Settings itself.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use tessel_power::config::{defaults, validate_settings};
use tessel_power::ui::{duration_name, Control, PowerSettingsWindow};
use serde_json::Value;
use tessel_core::tr;
use tessel_power_core::backend::PowerSystem;

use crate::embed::{embed, Embed};
use crate::{Factories, PowerPolicy};

/// `page.controller` / `page.backend` / `page.client` of the Power page.
pub struct PowerPage {
    pub controller: Rc<PowerSettingsWindow>,
    pub backend: Rc<dyn PowerSystem>,
    pub client: Rc<dyn PowerPolicy>,
    pub state: Rc<RemoteState>,
}

/// The `PowerPageWindow` subclass' own attributes.
#[derive(Default)]
pub struct RemoteState {
    pub saving_remote: Cell<bool>,
    pub loaded_remote: RefCell<Option<Value>>,
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[allow(deprecated)]
fn has_id(combo: &impl IsA<gtk::ComboBox>, id: &str) -> bool {
    let Some(model) = combo.model() else { return false };
    let Some(mut iter) = model.iter_first() else { return false };
    loop {
        if model.get_value(&iter, 1).get::<Option<String>>().ok().flatten().as_deref() == Some(id) {
            return true;
        }
        if !model.iter_next(&mut iter) {
            return false;
        }
    }
}

#[allow(deprecated)]
fn ensure_duration(combo: &gtk::ComboBoxText, value: &Value) {
    let id = value_text(value);
    if !has_id(combo, &id) {
        combo.append(Some(&id), &duration_name(value.as_i64().unwrap_or(0)));
    }
}

/// `PowerPageWindow.refresh`.
fn refresh(view: &PowerSettingsWindow, client: &dyn PowerPolicy, state: &RemoteState) {
    let mut incoming = if client.ready() { client.settings() } else { None };
    if let Some(value) = &incoming {
        // Older services omit options added later; show their defaults.
        if let Ok(valid) = validate_settings(value) {
            incoming = Some(valid);
        }
    }
    let dirty = *view.settings.borrow() != *view.saved.borrow();
    if let Some(incoming) = incoming {
        if Some(&incoming) != state.loaded_remote.borrow().as_ref() && !dirty && !state.saving_remote.get() {
            state.loaded_remote.replace(Some(incoming.clone()));
            view.settings.replace(incoming.clone());
            view.saved.replace(incoming.clone());
            view.set_syncing(true);
            #[allow(deprecated)]
            for (key, controls) in &view.controls {
                for (field, control) in controls {
                    let value = &incoming[*key][*field];
                    if *field != "sleep_action" {
                        if let Control::Duration(combo) = control {
                            ensure_duration(combo, value);
                        }
                    }
                    control.set_active_id(&value_text(value));
                }
            }
            ensure_duration(&view.lock_choice, &incoming["lock_after"]);
            #[allow(deprecated)]
            view.lock_choice.set_active_id(Some(&value_text(&incoming["lock_after"])));
            view.sync_extra_controls();
            view.set_syncing(false);
        }
    }
    view.refresh_base();
    view.save_button.set_sensitive(view.save_button.is_sensitive() && client.ready() && !state.saving_remote.get());
    let error = client.error();
    if !error.is_empty() {
        view.status_label.set_text(&error);
        view.status_label.set_visible(true);
    }
}

/// `PowerPageWindow._save`: send to the service; keep the saved copy until it acknowledges.
fn save(weak: &Weak<PowerSettingsWindow>, client: &Rc<dyn PowerPolicy>, state: &Rc<RemoteState>) {
    let Some(view) = weak.upgrade() else { return };
    if state.saving_remote.get() || !client.ready() || *view.settings.borrow() == *view.saved.borrow()
        || !view.validation_error().is_empty() {
        return;
    }
    state.saving_remote.set(true);
    let submitted = view.settings.borrow().clone();
    view.set_message(&tr!("Saving…"));
    view.refresh();
    let finished: Rc<dyn Fn(bool, String)> = {
        let (weak, state, submitted) = (weak.clone(), state.clone(), submitted.clone());
        Rc::new(move |ok: bool, error: String| {
            let Some(view) = weak.upgrade() else { return };
            if view.destroyed() {
                return;
            }
            state.saving_remote.set(false);
            if ok {
                view.saved.replace(submitted.clone());
                view.set_message(&tr!("Settings saved."));
            } else {
                view.set_message(&if error.is_empty() { tr!("Could not save settings.") } else { error });
            }
            view.refresh();
        })
    };
    drop(view);
    let reply = finished.clone();
    if !client.set_settings(&submitted, Box::new(move |ok, error| reply(ok, error))) {
        finished(false, client.error());
    }
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let view: Rc<RefCell<Option<Rc<PowerSettingsWindow>>>> = Rc::default();
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
    let backend: Rc<dyn PowerSystem> = match &factories.power_backend {
        Some(factory) => factory(changed.clone(), failed),
        None => {
            let (changed, failed) = (changed.clone(), failed.clone());
            Rc::new(tessel_power_core::backend::PowerBackend::new(move || changed(), move |message| failed(message),
                                                            Default::default()))
        }
    };
    let client: Rc<dyn PowerPolicy> = match &factories.power {
        Some(factory) => factory(changed.clone()),
        None => {
            let changed = changed.clone();
            Rc::new(tessel_power::client::PowerClient::new(move || changed()))
        }
    };
    let state = Rc::new(RemoteState::default());
    let initial = validate_settings(&defaults()).map_err(|error| error.to_string())?;
    let window = PowerSettingsWindow::new(backend.clone(), Rc::new(String::new), &initial, Rc::new(|_settings| false));
    {
        let (weak, policy, remote) = (Rc::downgrade(&window), client.clone(), state.clone());
        window.refresh_override.replace(Some(Rc::new(move || {
            if let Some(view) = weak.upgrade() {
                refresh(&view, policy.as_ref(), &remote);
            }
        })));
        let (weak, policy, remote) = (Rc::downgrade(&window), client.clone(), state.clone());
        window.save_override.replace(Some(Rc::new(move || save(&weak, &policy, &remote))));
    }
    view.replace(Some(window.clone()));
    window.refresh();
    let cleanup = {
        let (view, client, backend) = (view.clone(), client.clone(), backend.clone());
        move || {
            if let Some(view) = view.take() {
                view.on_destroy();
            }
            client.close();
            backend.close();
        }
    };
    // The status line stays above Save Changes, in the shared footer style.
    let ready = client.clone();
    gs_base::style::footer(&window.status_label, Some(Rc::new(move || ready.ready())));
    let page = embed(&window.window, host, Embed {
        refresh: Some(changed), cleanup: Some(Box::new(cleanup)), close_button: Some(window.close_button.clone().into()),
        ..Default::default()
    });
    page.add_target("lock_choice", &window.lock_choice);
    page.set_controller(Rc::new(PowerPage { controller: window, backend: backend.clone(), client: client.clone(), state }));
    backend.start();
    client.start();
    Ok(page)
}
