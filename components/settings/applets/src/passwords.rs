//! Passwords (tessel-keyring, the desktop's own Secret Service), embedded from
//! the keyring UI crate. The page only talks to tessel-keyring on the session bus.
use std::rc::Rc;

use gs_base::style::PAGE_MARGIN;
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;

use crate::Factories;

/// `page.controller` of the Passwords page.
pub struct PasswordsPage {
    pub controls: Rc<tessel_keyring_ui::page::PasswordsPage>,
}

pub(crate) fn build(_host: &Host, _factories: &Factories) -> Result<SettingsPage, String> {
    let controls = tessel_keyring_ui::page::build_page(tessel_keyring_ui::page::Client::session());
    let content = &controls.page;
    let heading = gtk::Label::builder().label(tessel_core::tr!("Passwords")).xalign(0.0).build();
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
    page.add_target("saved", &controls.search);
    page.add_target("status", &controls.status_title);
    page.add_target("import", &controls.import_file);
    let closing = controls.clone();
    page.set_close(move || closing.close());
    page.set_controller(Rc::new(PasswordsPage { controls }));
    Ok(page)
}
