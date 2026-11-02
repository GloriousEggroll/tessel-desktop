//! Protocol use on GTK's own wl_display (gdk4-wayland).

use crate::client::{Client, Frame, Protocols};
use gdk_wayland::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, glib};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

/// The wayland-client [`Connection`] gdk4-wayland wraps around a
/// `GdkWaylandDisplay`'s `wl_display` (shared, cached on the display).
pub fn gdk_connection(display: &gdk::Display) -> Option<Connection> {
    let display = display.downcast_ref::<gdk_wayland::WaylandDisplay>()?;
    if display.is_closed() {
        return None;
    }
    let backend = display.wl_display()?.backend().upgrade()?;
    Some(Connection::from_backend(backend))
}

/// The `wl_surface` of a realized GTK native (window, popover, layer surface).
pub fn gtk_wl_surface(native: &impl IsA<gtk::Native>) -> Option<WlSurface> {
    let surface = native.as_ref().surface()?;
    surface.downcast_ref::<gdk_wayland::WaylandSurface>()?.wl_surface()
}

impl Client {
    /// A client on GTK's connection, needed when requests take a GTK surface
    /// (`set_rectangle`). It uses its own event queue; attach it with
    /// [`crate::GlibClient::attach`] to the default main context.
    pub fn for_gdk_display(display: &gdk::Display, protocols: Protocols) -> Option<Client> {
        Some(Client::new(gdk_connection(display)?, protocols))
    }
}

impl Frame {
    /// A GDK texture of the frame (no copy beyond `glib::Bytes`).
    pub fn to_texture(&self) -> gdk::Texture {
        let format = if self.has_alpha() { gdk::MemoryFormat::B8g8r8a8Premultiplied } else { gdk::MemoryFormat::B8g8r8x8 };
        let bytes = glib::Bytes::from(&self.data[..]);
        gdk::MemoryTexture::new(self.width as i32, self.height as i32, format, &bytes, self.stride as usize).upcast()
    }
}
