//! `capture_page.py`: Capture's result preferences, embedded without
//! starting Capture.
use std::rc::Rc;

use gs_base::style::{card, PAGE_MARGIN};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;

use crate::Factories;

/// `page.controller` of the Screenshots page (Python `controls`, with `.card`).
pub struct CapturePage {
    pub controls: tessel_capture::settings::SettingsPage,
    pub card: Option<gtk::Box>,
}

/// Capture's switch rows go in the shared card, with the shared row styles.
pub fn shared_card(controls: &tessel_capture::settings::SettingsPage) -> Option<gtk::Box> {
    let page = &controls.page;
    page.set_margin_top(PAGE_MARGIN);
    page.set_margin_bottom(PAGE_MARGIN);
    page.set_margin_start(PAGE_MARGIN);
    page.set_margin_end(PAGE_MARGIN);
    // Python iterates `controls.switches.values()` (insertion = KEYS order).
    let rows: Vec<gtk::Widget> = tessel_capture::settings::KEYS.iter()
        .filter_map(|key| controls.switches.get(key))
        .filter_map(|switch| switch.parent())
        .collect();
    let first = rows.first()?;
    let b = card(0);
    page.insert_child_after(&b, first.prev_sibling().as_ref());
    for row in &rows {
        page.remove(row);
        row.add_css_class("gs-row");
        if let Some(labels) = row.first_child().and_downcast::<gtk::Box>() {
            labels.set_valign(gtk::Align::Center);
            if let Some(title) = labels.first_child().and_downcast::<gtk::Label>() {
                title.add_css_class("gs-row-title");
                if let Some(hint) = title.next_sibling().and_downcast::<gtk::Label>() {
                    for name in ["subtitle", "dim-label"] {
                        hint.remove_css_class(name);
                    }
                    hint.add_css_class("gs-row-subtitle");
                }
            }
        }
        b.append(row);
    }
    Some(b)
}

pub(crate) fn build(_host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let controls = tessel_capture::settings::build_page(factories.capture_path.clone());
    let card = shared_card(&controls);
    let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&controls.page));
    let page = SettingsPage::new(scroll);
    for key in ["copy_to_clipboard", "open_editor"] {
        if let Some(switch) = controls.switches.get(key) {
            page.add_target(key, switch);
        }
    }
    page.set_controller(Rc::new(CapturePage { controls, card }));
    Ok(page)
}
