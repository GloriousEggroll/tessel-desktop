//! DockSurface backgrounds: `set_background`, `_set_popup_blur`, the
//! per-clone display CSS providers (APPLICATION + 2) and blur bindings.

use gtk::gdk;
use gtk::prelude::*;
use std::cell::RefCell;

use super::DockSurface;
use crate::appearance::style::{dock_css, DockCssRequest};
use suite_gtk::palette::Rgb;

/// Arguments of `set_background(...)`. `None` = Python's "argument omitted"
/// (keep the previous value; the first call falls back to the config).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Background {
    pub is_dark: bool,
    pub transparency: i64,
    pub wallpaper: Option<Rgb>,
    /// `None` → `config.effective_rounded_corners`.
    pub rounded_corners: Option<bool>,
    pub apps_transparency: Option<i64>,
    pub apps_frosted: Option<bool>,
    pub apps_rounded_corners: Option<bool>,
    pub frosted: Option<bool>,
    pub frost_strength: Option<i64>,
    pub apps_frost_strength: Option<i64>,
    pub background_color: Option<Option<String>>,
    pub border_sides: Option<Vec<String>>,
    pub border_color: Option<Option<String>>,
}

/// The material attributes `set_background` remembers (`hasattr` = Some).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Material {
    pub border_sides: Option<Vec<String>>,
    pub border_color: Option<Option<String>>,
    pub background_color: Option<Option<String>>,
    pub apps_transparency: Option<i64>,
    pub frosted: Option<bool>,
    pub frost_strength: Option<i64>,
    pub apps_frosted: Option<bool>,
    pub apps_frost_strength: Option<i64>,
    pub apps_rounded_corners: Option<bool>,
}

#[derive(Default)]
pub(super) struct Styles {
    pub material: RefCell<Material>,
    pub options: RefCell<Option<(bool, i64, Option<Rgb>, bool)>>,
    pub background: RefCell<Option<gtk::CssProvider>>,
    pub background_signature: RefCell<Option<String>>,
    pub popup: RefCell<Option<gtk::CssProvider>>,
    pub popup_signature: RefCell<Option<(String, String)>>,
}

impl Styles {
    pub fn close(&self, display: &gdk::Display) {
        for provider in [self.popup.borrow_mut().take(), self.background.borrow_mut().take()].into_iter().flatten() {
            gtk::style_context_remove_provider_for_display(display, &provider);
        }
    }
}

fn strength(value: i64) -> u8 { value.clamp(0, 100) as u8 }

impl DockSurface {
    pub fn material(&self) -> Material { self.styles.material.borrow().clone() }
    /// `_background_options` (is_dark, transparency, wallpaper colour, requested rounding).
    pub fn background_options(&self) -> Option<(bool, i64, Option<Rgb>, bool)> { *self.styles.options.borrow() }
    pub fn panel_css(&self) -> Option<String> { self.styles.background_signature.borrow().clone() }
    pub fn popup_css(&self) -> Option<(String, String)> { self.styles.popup_signature.borrow().clone() }

    /// `_effective_rounding(requested)`: full-length surfaces meet the
    /// screen edges when not floating; maximized windows square the bar.
    pub fn effective_rounding(&self, requested: bool) -> bool {
        requested && !self.has_maximized.get() && (!self.bar_mode || self.float_mode.get())
    }

    /// `_set_popup_blur(popup)` for layer-surface popups (menu or preview).
    pub(crate) fn set_window_popup_blur(&self, popup: &gtk::Window, menu: bool) {
        let binding = suite_gtk::blur::attach(popup, Some("waydock-popup"), true, false);
        let material = self.styles.material.borrow();
        if menu {
            // The menu floats apart from the bar, so screen edges never square it.
            let rounded = self.styles.options.borrow().map(|o| o.3).unwrap_or_else(|| self.config.effective_rounded_corners());
            binding.configure(material.frosted.unwrap_or(false), strength(material.frost_strength.unwrap_or(50)), rounded, None, None);
        } else {
            binding.configure(material.apps_frosted.unwrap_or(false), strength(material.apps_frost_strength.unwrap_or(50)),
                              material.apps_rounded_corners.unwrap_or(true), None, None);
        }
    }

    pub(crate) fn set_popover_blur(&self, popover: &gtk::Popover) {
        let binding = suite_gtk::blur::attach(popover, None, false, false);
        let material = self.styles.material.borrow();
        binding.configure(material.apps_frosted.unwrap_or(false), strength(material.apps_frost_strength.unwrap_or(50)),
                          material.apps_rounded_corners.unwrap_or(true), None, None);
    }

    /// Re-apply the stored background (maximize/float changes).
    pub(crate) fn restyle(&self) {
        let options = *self.styles.options.borrow();
        if let Some((is_dark, transparency, wallpaper, rounded)) = options {
            self.set_background(&Background { is_dark, transparency, wallpaper, rounded_corners: Some(rounded), ..Default::default() });
        }
    }

    pub fn set_background(&self, request: &Background) {
        if self.destroyed.get() {
            return;
        }
        {
            let mut m = self.styles.material.borrow_mut();
            if let Some(sides) = &request.border_sides {
                m.border_sides = Some(sides.clone());
            } else if m.border_sides.is_none() {
                m.border_sides = Some(if self.config.bar_border_enabled { self.config.bar_border_sides.clone() } else { Vec::new() });
            }
            if let Some(color) = &request.border_color {
                m.border_color = Some(color.clone());
            } else if m.border_color.is_none() {
                m.border_color = Some((!self.config.match_wallpaper).then(|| self.config.bar_border_color.clone()));
            }
            if let Some(color) = &request.background_color {
                m.background_color = Some(color.clone());
            } else if m.background_color.is_none() {
                m.background_color = Some(if self.config.match_wallpaper { None } else { self.config.background_color.clone() });
            }
            if request.apps_transparency.is_some() { m.apps_transparency = request.apps_transparency; }
            if request.frosted.is_some() { m.frosted = request.frosted; }
            if request.frost_strength.is_some() { m.frost_strength = request.frost_strength; }
            if request.apps_frosted.is_some() { m.apps_frosted = request.apps_frosted; }
            if request.apps_frost_strength.is_some() { m.apps_frost_strength = request.apps_frost_strength; }
            if request.apps_rounded_corners.is_some() { m.apps_rounded_corners = request.apps_rounded_corners; }
        }
        let requested = request.rounded_corners.unwrap_or_else(|| self.config.effective_rounded_corners());
        self.styles.options.replace(Some((request.is_dark, request.transparency, request.wallpaper, requested)));
        let rounded = self.effective_rounding(requested);
        let m = self.material();
        self.blur.configure(m.frosted.unwrap_or(false), strength(m.frost_strength.unwrap_or(50)), rounded, None, None);
        if let Some(popup) = self.preview().window() {
            self.set_window_popup_blur(&popup, false);
        }
        if let Some(menu) = self.menu_window() {
            self.set_window_popup_blur(&menu, true);
        }
        if let Some(context) = self.context_menu() {
            self.set_popover_blur(&context);
        }
        // Only the outline meets the screen edges; workspace buttons and the
        // application menu keep the requested corners.
        let css = dock_css(&DockCssRequest {
            is_dark: request.is_dark, transparency: request.transparency, wallpaper: request.wallpaper,
            rounded_corners: rounded, requested_rounding: requested, frosted: m.frosted.unwrap_or(false),
            frost_strength: m.frost_strength.unwrap_or(50), scope: self.popup_class.clone(),
            background_color: m.background_color.clone().flatten(),
            border_sides: if self.bar_mode { m.border_sides.clone().unwrap_or_default() } else { Vec::new() },
            border_color: m.border_color.clone().flatten(), shine: self.is_app_dock && self.config.app_dock_shine,
            apps_transparency: m.apps_transparency.unwrap_or(0), apps_frosted: m.apps_frosted.unwrap_or(false),
            apps_frost_strength: m.apps_frost_strength.unwrap_or(50), apps_rounded_corners: m.apps_rounded_corners.unwrap_or(true),
        });
        let css = match css {
            Ok(css) => css,
            Err(error) => {
                eprintln!("Waydock: {error}");
                return;
            }
        };
        let display = WidgetExt::display(&self.window);
        if self.styles.background.borrow().is_none() {
            // A widget-local provider does not style descendants: scope a
            // display provider to this clone's unique panel class instead.
            let provider = gtk::CssProvider::new();
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2);
            self.styles.background.replace(Some(provider));
        }
        if self.styles.background_signature.borrow().as_deref() != Some(css.panel.as_str()) {
            if let Some(provider) = self.styles.background.borrow().as_ref() {
                provider.load_from_string(&css.panel);
            }
            self.styles.background_signature.replace(Some(css.panel.clone()));
        }
        let popup = (css.popup.clone(), css.menu.clone());
        if self.styles.popup_signature.borrow().as_ref() == Some(&popup) {
            return;
        }
        if self.styles.popup.borrow().is_none() {
            let provider = gtk::CssProvider::new();
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2);
            self.styles.popup.replace(Some(provider));
        }
        if let Some(provider) = self.styles.popup.borrow().as_ref() {
            provider.load_from_string(&format!("{}{}", css.popup, css.menu));
        }
        self.styles.popup_signature.replace(Some(popup));
    }
}
