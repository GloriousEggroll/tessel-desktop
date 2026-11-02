//! Opt-in compositor blur for GTK4 surfaces (background_blur.py).
//!
//! One implementation for the whole suite: this re-exports `tessel_wayland::blur`
//! (ext_background_effect_v1 + the private waydock_blur_manager_v1, bound on
//! GTK's own wl_display through gdk4-wayland). API unchanged:
//! `attach(&widget, target_class, include_root, include_popovers) -> BlurBinding`,
//! `.configure(..)`, `.refresh()`, `.surface_count()`, `.geometry(i)`, `.close()`.
pub use tessel_wayland::blur::*;
