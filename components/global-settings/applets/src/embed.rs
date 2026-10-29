//! `applet_pages._embed`: detach an applet's native controls before they are
//! mapped, keeping their signal owner, and wrap them as a settings page.
use std::cell::Cell;
use std::rc::Rc;

use gs_base::{Host, SettingsPage};
use gtk::prelude::*;

/// Keyword arguments of `_embed(window, host, …)`.
#[derive(Default)]
pub struct Embed {
    pub refresh: Option<Rc<dyn Fn()>>,
    pub cleanup: Option<Box<dyn Fn()>>,
    pub deactivate: Option<Rc<dyn Fn()>>,
    /// Standard page margins for content without its own.
    pub margin: i32,
    pub scroll: bool,
    /// Moved to the bottom of the page as the shared muted status line.
    pub footer: Option<gtk::Label>,
    pub real_error: Option<Rc<dyn Fn() -> bool>>,
    /// `window.close_button` (hidden inside a page).
    pub close_button: Option<gtk::Widget>,
    /// `window._css` / `window._css_provider`: released from the host display on close.
    pub providers: Vec<gtk::CssProvider>,
}

/// GObject data key of `suite_gtk::theme::attach` (`window._waydock_companion_theme`).
const THEME_KEY: &str = "waydock-companion-theme";

/// The companion theme attached to `window`, if any.
pub fn companion_theme(window: &gtk::Window) -> Option<suite_gtk::theme::WindowTheme> {
    // SAFETY: suite_gtk::theme::attach only ever stores a WindowTheme under THEME_KEY.
    unsafe { window.data::<suite_gtk::theme::WindowTheme>(THEME_KEY).map(|theme| theme.as_ref().clone()) }
}

/// CSS classes the page wrapper copies from the applet window.
pub fn copied_classes(source: &gtk::Widget) -> Vec<String> {
    source.css_classes().iter().map(|name| name.to_string())
        .filter(|name| !name.starts_with("waydock-companion-") && name != "background" && name != "tray-popup")
        .collect()
}

/// `_embed(window, host, refresh=…, cleanup=…, …)`. `source` is a
/// `gtk::Window` (its child is detached) or a plain widget page.
pub fn embed(source: &impl IsA<gtk::Widget>, host: &Host, options: Embed) -> SettingsPage {
    let source = source.as_ref().clone();
    let window = source.downcast_ref::<gtk::Window>().cloned();
    let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
    wrapper.set_hexpand(true);
    wrapper.set_vexpand(true);
    for name in copied_classes(&source) {
        wrapper.add_css_class(&name);
    }
    let mut content = match &window {
        Some(window) => {
            let child = window.child().expect("embedded window has content");
            window.set_child(None::<&gtk::Widget>);
            child
        }
        None => source.clone(),
    };
    // Popup chrome (a framed, rounded surface) does not belong inside a page.
    content.remove_css_class("popup-surface");
    content.set_hexpand(true);
    content.set_vexpand(true);
    if let Some(footer) = &options.footer {
        if let Some(parent) = footer.parent() {
            remove_child(&parent, footer.upcast_ref());
        }
    }
    if options.margin != 0 {
        content.set_margin_top(options.margin);
        content.set_margin_bottom(options.margin);
        content.set_margin_start(options.margin);
        content.set_margin_end(options.margin);
    }
    if options.scroll {
        let scroller = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        content.set_vexpand(false);
        content.set_valign(gtk::Align::Start);
        scroller.set_child(Some(&content));
        content = scroller.upcast();
    }
    wrapper.append(&content);
    if let Some(footer) = &options.footer {
        gs_base::style::footer(footer, options.real_error.clone());
        let side = if options.margin != 0 { options.margin } else { 24 };
        footer.set_margin_start(side);
        footer.set_margin_end(side);
        footer.set_margin_bottom(16);
        footer.set_margin_top(8);
        wrapper.append(footer);
    }
    if let Some(button) = &options.close_button {
        button.set_visible(false);
    }
    if let Some(window) = &window {
        if let Some(theme) = companion_theme(window) {
            // The visible host supplies one palette to every embedded page.
            theme.close();
        }
    }
    let closed = Rc::new(Cell::new(false));
    let display = WidgetExt::display(&host.window());
    let Embed { refresh, cleanup, deactivate, providers, .. } = options;
    let close = {
        let closed = closed.clone();
        let window = window.clone();
        move || {
            if closed.replace(true) {
                return;
            }
            if let Some(cleanup) = &cleanup {
                cleanup();
            }
            if let Some(window) = &window {
                // A plain widget page releases its own providers in cleanup().
                for provider in &providers {
                    gtk::style_context_remove_provider_for_display(&display, provider);
                }
                window.destroy();
            }
        }
    };
    let activate = {
        let closed = closed.clone();
        move || {
            if !closed.get() {
                if let Some(refresh) = &refresh {
                    refresh();
                }
            }
        }
    };
    let page = SettingsPage::new(wrapper).with_close(close).with_activate(activate);
    if let Some(deactivate) = deactivate {
        page.set_deactivate(move || deactivate());
    }
    page
}

/// `parent.remove(child)` for any container GTK widget.
pub fn remove_child(parent: &gtk::Widget, child: &gtk::Widget) {
    if let Some(parent) = parent.downcast_ref::<gtk::Box>() {
        parent.remove(child);
    } else if let Some(parent) = parent.downcast_ref::<gtk::Grid>() {
        parent.remove(child);
    } else if let Some(parent) = parent.downcast_ref::<gtk::Stack>() {
        parent.remove(child);
    } else {
        child.unparent();
    }
}

/// Every descendant of `root` breadth first, up to `depth` levels.
pub fn descendants(root: &impl IsA<gtk::Widget>, depth: usize) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut pending = suite_gtk::utils::children(root);
    for _ in 0..depth {
        let mut following = Vec::new();
        for widget in pending {
            following.extend(suite_gtk::utils::children(&widget));
            out.push(widget);
        }
        pending = following;
    }
    out
}
