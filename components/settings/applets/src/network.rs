//! `applet_pages._network`: live connections and saved profiles in two tabs,
//! through a facade application that reuses the network editors without a
//! second NetworkManager service or secret agent.
use std::rc::Rc;

use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use tessel_network::app::{App, Surfaces};
use tessel_network::backend::NetworkBackend;
use tessel_network::gui::{Embedding, Gui};
use tessel_network::preferences::NotificationPreferences;
use tessel_network::ui::popup::NetworkPopup;
use tessel_network::ui::settings::NetworkSettingsWindow;
use tessel_core::tr;

use crate::embed::{embed, Embed};
use crate::Factories;

/// Application id of the facade (`NetworkPageApplication`), `NON_UNIQUE`.
pub const FACADE_ID: &str = "org.tessel.GlobalSettings.NetworkDialogs";

/// `page.controller`, `page.overview`, `page.tabs`, `page.backend`, `page.application`.
pub struct NetworkPage {
    pub controller: Rc<NetworkSettingsWindow>,
    pub overview: Rc<NetworkPopup>,
    pub tabs: gtk::Stack,
    pub backend: NetworkBackend,
    pub application: App,
    pub gui: Rc<Gui>,
    pub gapp: gtk::Application,
}

thread_local! {
    /// The registered facade and the host its `activate` presents. GApplication
    /// exports one object per id on the session connection, so a rebuilt page
    /// (Try Again, tests) reuses the facade instead of registering it twice.
    static FACADE: std::cell::RefCell<Option<(gtk::Application, Rc<std::cell::RefCell<glib::WeakRef<gtk::Window>>>)>> =
        const { std::cell::RefCell::new(None) };
}

/// `NetworkPageApplication()` + `register(None)`; `do_activate` presents the host.
fn facade(host: &gtk::Window) -> Result<gtk::Application, String> {
    FACADE.with(|cell| {
        if let Some((gapp, target)) = cell.borrow().as_ref() {
            target.borrow().set(Some(host));
            return Ok(gapp.clone());
        }
        let gapp = gtk::Application::new(Some(FACADE_ID), gio::ApplicationFlags::NON_UNIQUE);
        let target = Rc::new(std::cell::RefCell::new(glib::WeakRef::new()));
        target.borrow().set(Some(host));
        let current = target.clone();
        gapp.connect_activate(move |_| if let Some(window) = current.borrow().upgrade() { window.present() });
        gapp.register(None::<&gio::Cancellable>).map_err(|error| error.message().to_owned())?;
        cell.replace(Some((gapp.clone(), target)));
        Ok(gapp)
    })
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let host_window = host.window();
    let gapp = facade(&host_window)?;
    let app = App::new(gapp.upcast_ref());
    app.0.quiet.set(true);
    app.0.preferences.replace(Some(NotificationPreferences::new()));
    let changed: Rc<dyn Fn()> = {
        let weak = app.downgrade();
        Rc::new(move || if let Some(inner) = weak.upgrade() { App(inner).changed() })
    };
    let error: Rc<dyn Fn(&str)> = {
        let weak = app.downgrade();
        Rc::new(move |message: &str| if let Some(inner) = weak.upgrade() { App(inner).report_error(message) })
    };
    let backend = match &factories.network {
        Some(factory) => factory(changed.clone(), error),
        None => {
            let (changed, error) = (changed.clone(), error.clone());
            NetworkBackend::new(move || changed(), move |message| error(message), Default::default())
        }
    };
    app.0.backend.replace(Some(backend.clone()));
    let gui = Gui::new(&app);
    app.0.surfaces.replace(Some(gui.clone() as Rc<dyn Surfaces>));
    let tabs = gtk::Stack::new();
    {
        let (saved, connections) = (tabs.downgrade(), tabs.downgrade());
        gui.set_embedding(Embedding {
            host: host_window.clone(),
            show_settings: Rc::new(move || if let Some(tabs) = saved.upgrade() { tabs.set_visible_child_name("saved") }),
            show_popup: Rc::new(move || if let Some(tabs) = connections.upgrade() { tabs.set_visible_child_name("connections") }),
        });
    }
    let view = NetworkSettingsWindow::new(&gui, &backend);
    view.settings_host.replace(Some(host_window.clone()));
    gui.adopt(Some(view.clone()), None);
    // Notification preferences normally live in the standalone headerbar.
    let heading = view.window.child().and_then(|page| page.first_child()).and_downcast::<gtk::Box>()
        .ok_or("The network settings have no heading")?;
    let preferences = tessel_network::ui::style::style(
        &gtk::Button::from_icon_name("preferences-system-notifications-symbolic"), &["icon-button"]);
    preferences.set_tooltip_text(Some(&tr!("Notification preferences")));
    preferences.set_valign(gtk::Align::Center);
    {
        let weak = Rc::downgrade(&gui);
        preferences.connect_clicked(move |_| if let Some(gui) = weak.upgrade() { gui.show_preferences() });
    }
    heading.append(&preferences);

    let close = {
        let (app, gui, view, backend) = (app.clone(), gui.clone(), view.clone(), backend.clone());
        move || {
            app.0.stopping.set(true);
            gui.close_children();
            view.on_destroy();
            backend.close();
        }
    };
    let profiles = embed(&view.window, host, Embed {
        refresh: Some(changed.clone()), cleanup: Some(Box::new(close)), ..Default::default()
    });
    let overview = NetworkPopup::new(&gui, &app, "bottom", 76, true);
    gui.adopt(None, Some(overview.clone()));
    if let Some(surface) = overview.window.child() {
        surface.remove_css_class("popup-surface");
    }
    let live = {
        let (refresh, destroy) = (overview.clone(), overview.clone());
        embed(&overview.window, host, Embed {
            refresh: Some(Rc::new(move || refresh.refresh())), cleanup: Some(Box::new(move || destroy.destroy())),
            ..Default::default()
        })
    };
    tabs.set_hexpand(true);
    tabs.set_vexpand(true);
    tabs.set_transition_type(gtk::StackTransitionType::Crossfade);
    tabs.add_titled(&live.widget, Some("connections"), &tr!("Connections"));
    tabs.add_titled(&profiles.widget, Some("saved"), &tr!("Saved connections"));
    let switcher = gtk::StackSwitcher::builder().stack(&tabs).build();
    switcher.set_halign(gtk::Align::Start);
    switcher.set_margin_start(24);
    switcher.set_margin_top(4);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("networkmanager");
    // One page header (the overview's, with its live subtitle) above the tabs.
    // The tabs replace its settings button and the saved tab's own title.
    let body = overview.rows_box.ancestor(gtk::ScrolledWindow::static_type()).and_then(|scroll| scroll.parent())
        .and_downcast::<gtk::Box>();
    if let Some(body) = &body {
        if let Some(header) = body.first_child() {
            if overview.subtitle.is_ancestor(&header) {
                body.remove(&header);
                header.set_margin_top(24);
                header.set_margin_start(24);
                header.set_margin_end(24);
                header.set_margin_bottom(8);
                overview.settings_button.set_visible(false);
                root.append(&header);
                body.set_margin_start(24);
                body.set_margin_end(24);
                body.set_margin_bottom(24);
                body.set_margin_top(12);
            }
        }
    }
    if let Some(saved_titles) = heading.first_child() {
        if !saved_titles.is::<gtk::Button>() {
            saved_titles.set_visible(false);
            heading.set_halign(gtk::Align::End);
        }
    }
    // Cards keep their natural height instead of sharing the spare space.
    overview.rows_box.set_valign(gtk::Align::Start);
    root.append(&switcher);
    root.append(&tabs);
    let (live, profiles) = (Rc::new(live), Rc::new(profiles));
    let refresh = changed.clone();
    let page = SettingsPage::new(root)
        .with_close(move || {
            live.close();
            profiles.close();
        })
        .with_activate(move || refresh());
    page.tabs.replace(Some(tabs.clone()));
    page.add_target("search_entry", &view.search_entry);
    page.add_target("add_button", &view.add_button);
    page.set_controller(Rc::new(NetworkPage {
        controller: view, overview, tabs, backend: backend.clone(), application: app, gui, gapp,
    }));
    backend.start();
    Ok(page)
}
