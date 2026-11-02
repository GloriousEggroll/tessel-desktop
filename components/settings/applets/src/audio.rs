//! `applet_pages._audio`: Audio Settings, embedded.
use std::cell::RefCell;
use std::rc::Rc;

use tessel_audio::api::AudioApi;
use tessel_audio::ui::common::View;
use tessel_audio::ui::settings::{pages, AudioSettingsWindow};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;

use crate::embed::{embed, Embed};
use crate::Factories;

/// `page.controller` / `page.backend` of the Audio page.
pub struct AudioPage {
    pub controller: Rc<AudioSettingsWindow>,
    pub backend: Rc<dyn AudioApi>,
}

/// Each audio tab repeats its sidebar name as a heading; the tab list says it.
pub fn drop_tab_headings(view: &AudioSettingsWindow) {
    let names: Vec<String> = pages().into_iter().map(|page| page.1).collect();
    let mut pending = tessel_gtk::utils::children(&view.stack);
    for _depth in 0..5 {
        let mut following = Vec::new();
        for widget in pending {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                if label.has_css_class("section-title") && names.iter().any(|name| *name == label.text()) {
                    label.set_visible(false);
                    continue;
                }
            }
            following.extend(tessel_gtk::utils::children(&widget));
        }
        pending = following;
    }
}

/// `nav.page_name = key`: the search spotlight selects nested audio pages
/// through the sidebar rows (`ListBoxRow.page_name`).
pub fn tag_navigation(view: &AudioSettingsWindow) {
    for (index, page) in pages().into_iter().enumerate() {
        if let Some(row) = view.navigation.row_at_index(index as i32) {
            gs_base::tags::set_str(&row, gs_base::tags::PAGE_NAME, page.0);
        }
    }
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let application = crate::application(host)?;
    let view: Rc<RefCell<Option<Rc<AudioSettingsWindow>>>> = Rc::default();
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
        let view = view.clone();
        Rc::new(move |message: &str| {
            let current = view.borrow().clone();
            if let Some(view) = current {
                view.set_error(message);
            }
        })
    };
    let backend: Rc<dyn AudioApi> = match &factories.audio {
        Some(factory) => factory(changed.clone(), failed),
        None => {
            let (changed, failed) = (changed.clone(), failed.clone());
            Rc::new(tessel_audio::backend::AudioBackend::new(move || changed(), move |message| failed(message),
                                                              tessel_audio::backend::BackendOptions::native(true)))
        }
    };
    let window = AudioSettingsWindow::new(&application, backend.clone());
    window.settings_host.replace(Some(host.window()));
    view.replace(Some(window.clone()));
    let cleanup = {
        let (view, backend) = (view.clone(), backend.clone());
        move || {
            let Some(view) = view.take() else { return };
            let dialog = view.loopback_dialog.borrow().clone();
            if let Some(dialog) = dialog {
                dialog.dialog.destroy();
            }
            for row in view.node_rows.borrow().values() {
                row.destroyed();
            }
            backend.close();
        }
    };
    drop_tab_headings(&window);
    tag_navigation(&window);
    let page = embed(&window.window, host, Embed {
        refresh: Some(changed), cleanup: Some(Box::new(cleanup)), footer: Some(window.status_label.clone()),
        providers: vec![window.css_provider().clone()], ..Default::default()
    });
    page.set_controller(Rc::new(AudioPage { controller: window, backend: backend.clone() }));
    backend.start();
    Ok(page)
}
