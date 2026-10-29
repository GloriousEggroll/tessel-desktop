//! GDK monitor helpers shared by the CLI server, desktop and switcher.
use gtk::gdk;
use gtk::prelude::*;

/// `DesktopApp.monitors_by_key`: `get_connector()` or `Monitor-<i+1>`, in GDK order.
pub fn monitors_by_key(display: &gdk::Display) -> Vec<(String, gdk::Monitor)> {
    let list = display.monitors();
    (0..list.n_items())
        .filter_map(|i| list.item(i).and_then(|o| o.downcast::<gdk::Monitor>().ok()).map(|m| (i, m)))
        .map(|(i, m)| (m.connector().map(|c| c.to_string()).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("Monitor-{}", i + 1)), m))
        .collect()
}

pub fn monitor_list(display: &gdk::Display) -> Vec<gdk::Monitor> {
    monitors_by_key(display).into_iter().map(|(_, m)| m).collect()
}
