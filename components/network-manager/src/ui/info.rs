//! `network_dialogs.py` ConnectionInfo: read-only, selectable details that
//! refresh live, reused per (profile, active, device) identifier triple.

use crate::backend::NetworkBackend;
use crate::ui::settings::type_name;
use crate::ui::style::{expand, label, margins, remove_children, style};
use gtk::prelude::*;
use std::rc::Rc;
use suite_core::tr;

pub type Identifiers = (Option<String>, Option<String>, Option<String>);

pub struct ConnectionInfo {
    pub window: gtk::Window,
    pub identifiers: Identifiers,
    backend: NetworkBackend,
    heading: gtk::Label,
    grid: gtk::Grid,
}

/// `datetime.fromtimestamp(t).strftime(format)` in local time.
pub fn local_time(timestamp: u64, format: &str) -> Option<String> {
    let time = glib::DateTime::from_unix_local(i64::try_from(timestamp).ok()?).ok()?;
    time.format(format).ok().map(|s| s.to_string())
}

impl ConnectionInfo {
    pub fn new(backend: &NetworkBackend, identifiers: Identifiers) -> Rc<Self> {
        let window = gtk::Window::new();
        style(&window, &["networkmanager"]);
        window.set_title(Some(&tr!("Connection Information")));
        window.set_default_size(570, 520);
        window.set_titlebar(Some(&gtk::HeaderBar::builder().show_title_buttons(true).build()));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
        margins(&root, 20);
        window.set_child(Some(&root));
        let heading = label("", Some("title"));
        root.append(&heading);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let grid = gtk::Grid::builder().row_spacing(12).column_spacing(22).build();
        scroll.set_child(Some(&grid));
        root.append(&expand(&scroll, &root));
        let info = Rc::new(Self { window, identifiers, backend: backend.clone(), heading, grid });
        info.refresh();
        info
    }

    pub fn details(&self) -> (String, Vec<(String, String)>) {
        let (profile_id, active_path, device_path) = &self.identifiers;
        let state = self.backend.state();
        let mut profile = profile_id.as_deref().and_then(|id| state.profiles.get(id)).cloned();
        let mut active = active_path.as_deref().and_then(|p| state.active.get(p)).cloned();
        let mut device = device_path.as_deref().and_then(|p| state.devices.get(p)).cloned();
        if active.is_none() {
            if let Some(first) = profile.as_ref().and_then(|p| p.active_ids.first()) {
                active = state.active.get(first).cloned();
            }
        }
        if active.is_none() {
            if let Some(first) = device.as_ref().and_then(|d| d.active_ids.first()) {
                active = state.active.get(first).cloned();
            }
        }
        if profile.is_none() {
            if let Some(a) = &active {
                profile = state.profiles.get(&a.profile_id).cloned();
            }
        }
        if device.is_none() {
            if let Some(first) = active.as_ref().and_then(|a| a.device_ids.first()) {
                device = state.devices.get(first).cloned();
            }
        }
        let heading = profile.as_ref().map(|p| p.name.clone()).or_else(|| active.as_ref().map(|a| a.name.clone()))
            .or_else(|| device.as_ref().map(|d| d.name.clone())).unwrap_or_else(|| tr!("Connection unavailable"));
        let mut details: Vec<(String, String)> = Vec::new();
        let mut put = |k: &str, v: String| {
            if let Some(entry) = details.iter_mut().find(|(x, _)| x == k) {
                entry.1 = v;
            } else {
                details.push((k.to_owned(), v));
            }
        };
        let yes_no = |b: bool| if b { tr!("Yes") } else { tr!("No") };
        if let Some(p) = &profile {
            put("Type", type_name(&p.kind));
            put("Profile ID", p.id.clone());
            put("Automatically connect", yes_no(p.autoconnect));
            if p.timestamp != 0 {
                if let Some(text) = local_time(p.timestamp, "%b %d, %Y at %H:%M") {
                    put("Last used", text);
                }
            }
        }
        if let Some(d) = &device {
            put("Interface", d.interface.clone());
            put("Status", suite_core::i18n::translate(&d.state_label));
            put("Managed", yes_no(d.managed));
            for (k, v) in &d.details {
                put(k, v.clone());
            }
        }
        if let Some(a) = &active {
            put("Connection status", suite_core::i18n::translate(&a.state_label));
            for (k, v) in &a.details {
                put(k, v.clone());
            }
        } else if profile.is_some() {
            put("Connection status", tr!("Not connected"));
        }
        drop(put);
        if details.is_empty() {
            details.push(("Status".into(), tr!("This connection is no longer available.")));
        }
        (heading, details)
    }

    pub fn refresh(&self) {
        let (heading, details) = self.details();
        self.heading.set_text(&heading);
        remove_children(&self.grid);
        for (index, (key, value)) in details.iter().enumerate() {
            let title = label(&suite_core::i18n::translate(key), Some("muted"));
            title.set_valign(gtk::Align::Start);
            self.grid.attach(&title, 0, index as i32, 1, 1);
            let text = label(value, None);
            text.set_selectable(true);
            text.set_ellipsize(gtk::pango::EllipsizeMode::None);
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            text.set_max_width_chars(46);
            text.set_hexpand(true);
            self.grid.attach(&text, 1, index as i32, 1, 1);
        }
        self.grid.set_visible(true);
    }
}
