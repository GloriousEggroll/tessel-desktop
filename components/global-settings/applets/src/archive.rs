//! Archive Manager settings (Ark's Configure dialog: General, Extraction,
//! Plugins, Previews), embedded from the archive-manager settings crate.
//! Both edit `~/.config/desktop-suite/archive-manager.json`; a running
//! Archive Manager follows every change.
use std::path::PathBuf;
use std::rc::Rc;

use gs_base::style::PAGE_MARGIN;
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;

use crate::Factories;

/// `page.controller` of the Archive Manager page.
pub struct ArchivePage {
    pub controls: Rc<archive_settings::page::SettingsPage>,
}

pub(crate) fn build_with_path(path: Option<PathBuf>) -> Result<SettingsPage, String> {
    let controls = archive_settings::page::build_page(path);
    let content = &controls.page;
    let heading = gtk::Label::builder().label(suite_core::tr!("Archive Manager")).xalign(0.0).build();
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
    for (id, switch) in &controls.plugin_switches {
        page.add_target(&format!("plugin-{id}"), switch);
    }
    page.add_target("preview_max_mb", &controls.max_size);
    let closing = controls.clone();
    page.set_close(move || closing.close());
    page.set_controller(Rc::new(ArchivePage { controls }));
    Ok(page)
}

pub(crate) fn build(_host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    build_with_path(factories.archive_path.clone())
}
