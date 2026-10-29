//! Shared desktop styling for the language setup wizard (language_style.py).

use std::cell::Cell;
use std::rc::Rc;
use suite_gtk::gtk;
use suite_gtk::gtk::prelude::*;
use suite_gtk::theme::WindowTheme;
use suite_gtk::utils::{add_css, remove_css, PRIORITY_APPLICATION};

pub const CSS: &str = r#"
window.languagemanager { background-color: @theme_bg_color; color: @theme_fg_color; }
.languagemanager headerbar {
    background-image: none; background-color: @theme_bg_color;
    border: none; box-shadow: none; padding: 8px 14px;
}
.languagemanager .title { font-size: 18pt; font-weight: 700; }
/* headerbar() gives its title label the "title" class too. */
.languagemanager headerbar .title { font-size: inherit; }
.languagemanager .section-title { font-size: 11pt; font-weight: 600; }
.languagemanager .muted, .languagemanager .subtitle { color: alpha(@theme_fg_color, .7); }
.languagemanager .subtitle { font-size: 9.5pt; }
.languagemanager .steps { font-size: 9pt; }
.languagemanager .settings-tile { min-width: 24px; min-height: 24px; color: @theme_fg_color; }
.languagemanager .card {
    background-color: mix(@theme_base_color, @theme_bg_color, .20);
    border: 1px solid alpha(@theme_fg_color, .12); border-radius: 12px;
}
.languagemanager list { background: transparent; }
.languagemanager .language-list > row {
    background: transparent; padding: 9px 14px; border: none; border-radius: 8px;
}
.languagemanager .language-list > row:hover { background-color: alpha(@theme_fg_color, .06); }
.languagemanager .language-list > row:selected {
    background-color: alpha(@theme_selected_bg_color, .18); color: @theme_fg_color;
}
.languagemanager .language-list .language-name { font-size: 11pt; }
.languagemanager .language-list .language-code { font-size: 9pt; }
.languagemanager .language-list .profile-check { color: @theme_selected_bg_color; }
.languagemanager .empty { padding: 24px; }
.languagemanager entry, .languagemanager searchentry {
    border-radius: 8px; border: 1px solid alpha(@theme_fg_color, .12); padding: 9px 12px;
}
.languagemanager button {
    background-image: none; background-color: alpha(@theme_fg_color, .065);
    color: @theme_fg_color; border: 1px solid alpha(@theme_fg_color, .12);
    border-radius: 8px; padding: 7px 17px; min-height: 24px;
    box-shadow: none; text-shadow: none;
}
.languagemanager button:hover { background-color: alpha(@theme_fg_color, .11); }
.languagemanager button.primary {
    background-color: @theme_selected_bg_color; color: @theme_selected_fg_color;
    border-color: transparent;
}
.languagemanager button:disabled { opacity: .45; }
.languagemanager .error { color: mix(@theme_fg_color, #db5a65, .85); }
.languagemanager .success { color: mix(@theme_fg_color, #28a273, .80); }
.languagemanager .result-icon { min-width: 74px; min-height: 74px; border-radius: 24px;
    background-color: alpha(@theme_selected_bg_color, .12); }
.languagemanager .review-value { font-size: 16pt; font-weight: 600; }
.languagemanager scrollbar { background: transparent; }
.languagemanager scrollbar slider { border-radius: 8px; }
"#;

/// Above the companion theme (APPLICATION + 30): this window has no blur
/// behind it, so a translucent surface would let other windows' text show through.
pub const OPAQUE_CSS: &str = r#"
window.languagemanager { background-image: none; background-color: @theme_bg_color; }
"#;
pub const OPAQUE_PRIORITY: u32 = PRIORITY_APPLICATION + 35;

pub fn style<W: IsA<gtk::Widget>>(widget: W, classes: &[&str]) -> W {
    for name in classes {
        widget.add_css_class(name);
    }
    widget
}

/// `style_window(window)`: class, both providers, companion theme, and a
/// cleanup run once on unrealize (also returned, = `window._language_style_close`).
pub fn style_window(window: &gtk::Window) -> (WindowTheme, Rc<dyn Fn()>) {
    window.add_css_class("languagemanager");
    let display = WidgetExt::display(window);
    let provider = add_css(CSS, Some(&display), PRIORITY_APPLICATION);
    let opaque = add_css(OPAQUE_CSS, Some(&display), OPAQUE_PRIORITY);
    let theme = suite_gtk::theme::attach(window, None);
    let closed = Rc::new(Cell::new(false));
    let cleanup: Rc<dyn Fn()> = {
        let theme = theme.clone();
        Rc::new(move || {
            if closed.replace(true) {
                return;
            }
            remove_css(&display, &provider);
            remove_css(&display, &opaque);
            theme.close();
        })
    };
    let on_unrealize = cleanup.clone();
    window.connect_unrealize(move |_| on_unrealize());
    (theme, cleanup)
}
