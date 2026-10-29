//! `applet_pages._bluetooth`: the Bluetooth manager, embedded without its
//! window, SIGINT handler or OBEX receiver.
use std::rc::Rc;

use bluetooth_manager::backend::Bt;
use bluetooth_manager::dbus::{Conn, GioConn};
use bluetooth_manager::ui::{obex_sender_factory, Options, Ui};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;

use crate::embed::{embed, Embed};
use crate::Factories;

/// `page.controller` / `page.backend` of the Bluetooth page.
pub struct BluetoothPage {
    pub controller: Rc<Ui>,
    pub backend: Rc<Bt>,
}

/// `ObservedBluetooth()`: BlueZ on the system bus.
fn system_bluez() -> Result<Rc<Bt>, String> {
    let connection = gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>).map_err(|e| e.message().to_owned())?;
    Ok(Bt::new(GioConn::new(connection) as Rc<dyn Conn>))
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let bt = match &factories.bluetooth {
        Some(factory) => factory()?,
        None => system_bluez()?,
    };
    let lang = suite_core::i18n::normalize_language(Some(&suite_core::i18n::selected_language()));
    let controller = Ui::new_embedded(Options {
        application: host.application(), popup: false, popup_edge: "bottom".into(), popup_offset: 76, lang,
        bt: bt.clone(), env: bluetooth_manager::env::GlibEnv::new(),
        services: factories.bluetooth_services.clone().unwrap_or_default(), sender_factory: obex_sender_factory(),
        handle_sigint: false,
    }, &host.window());
    let deactivate: Rc<dyn Fn()> = {
        let weak = Rc::downgrade(&controller);
        Rc::new(move || {
            let Some(ui) = weak.upgrade() else { return };
            let add = ui.add.borrow().as_ref().map(|add| add.window.clone());
            if let Some(window) = add {
                window.destroy();
            }
        })
    };
    let cleanup = {
        let (ui, deactivate) = (controller.clone(), deactivate.clone());
        move || {
            deactivate();
            ui.on_window_destroyed();
            for path in ui.manager.watched() {
                ui.manager.unwatch_object(&path);
            }
            ui.manager.bt.close();
        }
    };
    let refresh = {
        let weak = Rc::downgrade(&controller);
        move || if let Some(ui) = weak.upgrade() { ui.manager.refresh_ui() }
    };
    let page = embed(&controller.window, host, Embed {
        refresh: Some(Rc::new(refresh)), cleanup: Some(Box::new(cleanup)), deactivate: Some(deactivate),
        ..Default::default()
    });
    page.set_controller(Rc::new(BluetoothPage { controller, backend: bt }));
    Ok(page)
}
