//! Clipboard settings (tessel-clipboard, the Klipper replacement), embedded from the
//! tessel-clipboard settings crate. Both the page and the daemon use
//! `~/.config/tessel/clipboard.json`; tessel-clipboard follows every change.
use std::path::PathBuf;
use std::rc::Rc;

use gs_base::style::PAGE_MARGIN;
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;

use crate::Factories;

/// `page.controller` of the Clipboard page.
pub struct ClipboardPage {
    pub controls: Rc<tessel_clipboard_settings::page::SettingsPage>,
}

pub(crate) fn build_with_path(path: Option<PathBuf>) -> Result<SettingsPage, String> {
    let controls = tessel_clipboard_settings::page::build_page(path);
    let content = &controls.page;
    let heading = gtk::Label::builder().label(tessel_core::tr!("Clipboard")).xalign(0.0).build();
    heading.add_css_class("title-2");
    content.prepend(&heading);
    content.set_margin_top(PAGE_MARGIN);
    content.set_margin_bottom(PAGE_MARGIN);
    content.set_margin_start(PAGE_MARGIN);
    content.set_margin_end(PAGE_MARGIN);
    let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(content));
    let page = SettingsPage::new(scroll);
    for (key, switch) in &controls.switches {
        page.add_target(key, switch);
    }
    page.add_target("max_mb", &controls.max_size);
    let closing = controls.clone();
    page.set_close(move || closing.close());
    page.set_controller(Rc::new(ClipboardPage { controls }));
    Ok(page)
}

pub(crate) fn build(_host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    build_with_path(factories.clipboard_path.clone())
}
