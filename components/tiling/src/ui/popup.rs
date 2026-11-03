//! Themed, searchable Omarchy-style window shortcut reference (tiling_ui.py).

use super::settings::{Runner, TilingControls};
use crate::bindings::{BINDINGS, EXTRA_SHORTCUTS};
use crate::compat::truthy;
use crate::config::TilingConfig;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tessel_core::tr;
use tessel_gtk::gtk::prelude::*;
use tessel_gtk::gtk::{self, glib};
use tessel_gtk::popup::{popup_header, set_popup_width, PopupController, POPUP_PADDING, POPUP_WIDTH};
use tessel_gtk::utils::{accessible, add_css, remove_css, set_margins, PRIORITY_APPLICATION};

pub const APPLICATION_ID: &str = "org.tessel.Tiling";

pub const CSS: &str = ".tiling-shortcuts .popup-surface { background: @theme_bg_color; color: @theme_fg_color;
            border-radius: 12px; border: 1px solid alpha(@theme_fg_color,.14); }
            .tiling-shortcuts .shortcut-row { padding: 8px 2px; border-bottom: 1px solid alpha(@theme_fg_color,.08); }
            .tiling-shortcuts .tiling-card { padding: 12px 14px; border-radius: 10px;
                background-color: alpha(@theme_fg_color,.045); border: 1px solid alpha(@theme_fg_color,.10); }
            .tiling-shortcuts .shortcut-row:last-child { border-bottom: none; }
            .tiling-shortcuts .key { font-size: 9.5pt; font-weight: 600; }
            .tiling-shortcuts .detail { font-size: 9pt; opacity: .7; }";

/// Whether every casefolded word of `query` occurs in `text` (already casefolded).
pub fn matches_query(text: &str, query: &str) -> bool {
    let query = casefold(query);
    query.split_whitespace().all(|word| text.contains(word))
}

/// `str.casefold()` (lowercase plus the common full foldings).
pub fn casefold(text: &str) -> String { text.to_lowercase().replace('ß', "ss").replace('ς', "σ") }

/// The popup subtitle for a status reply.
pub fn subtitle(state: &crate::client::Reply) -> String {
    if truthy(state.get("enabled")) {
        tr!("On · New windows tile automatically.")
    } else if truthy(state.get("ready")) {
        tr!("Off · Windows move and resize normally.")
    } else {
        String::new()
    }
}

struct Inner {
    window: gtk::ApplicationWindow,
    state: gtk::Label,
    controls: TilingControls,
    search: gtk::SearchEntry,
    rows: Vec<(gtk::Box, String)>,
    css: gtk::CssProvider,
    controller: RefCell<Option<PopupController>>,
    timer: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct Shortcuts(Rc<Inner>);

impl Shortcuts {
    pub fn new(app: &gtk::Application, edge: &str, offset: i32, config: Option<Arc<TilingConfig>>,
               status: Option<super::settings::StatusCall>, runner: Runner) -> Self {
        let window = gtk::ApplicationWindow::builder().application(app).title(tr!("Window Tiling")).build();
        window.add_css_class("tiling-shortcuts");
        let width = set_popup_width(window.upcast_ref(), POPUP_WIDTH);
        window.set_default_size(width, 620);
        let css = add_css(CSS, Some(&WidgetExt::display(&window)), PRIORITY_APPLICATION);
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 14);
        surface.add_css_class("popup-surface");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        set_margins(&content, POPUP_PADDING);
        surface.append(&content);
        window.set_child(Some(&surface));
        let heading = popup_header(&["view-grid-symbolic"], &tr!("Window Tiling"), Some(" "), Vec::new());
        content.append(&heading.root);
        let state = heading.subtitle_label.clone();
        // Every status poll and change runs off the GTK thread.
        let controls = TilingControls::new(config, status, false, None, runner);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("tiling-card");
        card.append(controls.widget());
        content.append(&card);
        let hint = gtk::Label::builder()
            .label(tr!("These shortcuts apply while tiling is on. Existing shortcuts return when it is off."))
            .xalign(0.0).wrap(true).max_width_chars(40).build();
        hint.add_css_class("detail");
        content.append(&hint);
        let search = gtk::SearchEntry::builder().placeholder_text(tr!("Search shortcuts")).build();
        accessible(&search, &tr!("Search shortcuts"));
        content.append(&search);
        let listbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let mut rows = Vec::new();
        let items = BINDINGS.iter().map(|b| (b.label.clone(), b.description.clone()))
            .chain(EXTRA_SHORTCUTS.iter().map(|(a, b)| (a.to_string(), b.to_string())));
        for (shortcut, description) in items {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("shortcut-row");
            let key = gtk::Label::builder().label(&shortcut).xalign(0.0).wrap(true).width_chars(16).max_width_chars(16).build();
            key.add_css_class("key");
            let translated = tr!(description);
            let label = gtk::Label::builder().label(&translated).xalign(0.0).wrap(true).max_width_chars(24).hexpand(true).build();
            row.append(&key);
            row.append(&label);
            listbox.append(&row);
            rows.push((row, casefold(&format!("{shortcut} {translated}"))));
        }
        let scrolling = gtk::ScrolledWindow::builder().vexpand(true).build();
        scrolling.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        // Let the shortcut list shrink to leave room for preferences on smaller displays.
        scrolling.set_min_content_height(120);
        scrolling.set_max_content_height(480);
        scrolling.set_child(Some(&listbox));
        content.append(&scrolling);
        let this = Self(Rc::new(Inner { window: window.clone(), state, controls: controls.clone(), search: search.clone(), rows,
                                        css, controller: RefCell::new(None), timer: RefCell::new(None) }));
        let weak = Rc::downgrade(&this.0);
        controls.set_state_hook(Rc::new(move |state, _| {
            if let Some(inner) = weak.upgrade() {
                inner.state.set_text(&subtitle(state));
                inner.state.set_visible(truthy(state.get("ready")));
            }
        }));
        let weak = Rc::downgrade(&this.0);
        search.connect_search_changed(move |entry| {
            if let Some(inner) = weak.upgrade() {
                let query = entry.text();
                for (row, text) in &inner.rows {
                    row.set_visible(matches_query(text, &query));
                }
            }
        });
        let weak = Rc::downgrade(&this.0);
        let controller = PopupController::new(window.upcast_ref(), move || {
            if let Some(inner) = weak.upgrade() {
                Shortcuts(inner).refresh(true);
            }
        }, edge, offset, "service_apps");
        this.0.controller.replace(Some(controller));
        tessel_gtk::theme::attach(&window, None);
        this.refresh(true);
        let weak = Rc::downgrade(&this.0);
        let timer = glib::timeout_add_seconds_local(2, move || match weak.upgrade() {
            Some(inner) => {
                Shortcuts(inner).refresh(false);
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        this.0.timer.replace(Some(timer));
        let weak = Rc::downgrade(&this.0);
        window.connect_unrealize(move |_| {
            if let Some(inner) = weak.upgrade() {
                Shortcuts(inner).closed();
            }
        });
        this
    }

    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }
    pub fn controls(&self) -> &TilingControls { &self.0.controls }
    pub fn state_label(&self) -> &gtk::Label { &self.0.state }
    pub fn search(&self) -> &gtk::SearchEntry { &self.0.search }
    pub fn rows(&self) -> Vec<gtk::Box> { self.0.rows.iter().map(|(r, _)| r.clone()).collect() }
    pub fn controller(&self) -> PopupController { self.0.controller.borrow().clone().unwrap() }
    pub fn has_timer(&self) -> bool { self.0.timer.borrow().is_some() }

    pub fn refresh(&self, force: bool) {
        if !force && !self.0.window.get_visible() {
            return;
        }
        self.0.controls.refresh();
    }

    fn closed(&self) {
        if let Some(timer) = self.0.timer.take() {
            timer.remove();
        }
        remove_css(&WidgetExt::display(&self.0.window), &self.0.css);
    }
}

/// `tiling_ui.run(edge, offset)`: the single-instance popup application.
/// A second launch activates the first instance, which toggles its popup.
pub fn build_application(edge: &str, offset: i32, config: Option<Arc<TilingConfig>>,
                         status: Option<super::settings::StatusCall>, runner: Runner) -> (gtk::Application, Rc<RefCell<Option<Shortcuts>>>) {
    let app = gtk::Application::builder().application_id(APPLICATION_ID).build();
    let current: Rc<RefCell<Option<Shortcuts>>> = Rc::default();
    let (edge, slot) = (edge.to_owned(), current.clone());
    app.connect_activate(move |app| {
        let existing = slot.borrow().clone();
        if let Some(popup) = existing.as_ref().filter(|p| p.window().is_realized()) {
            popup.controller().activate();
            return;
        }
        if let Some(old) = existing {
            old.controller().close();
            old.window().destroy();
        }
        let popup = Shortcuts::new(app, &edge, offset, config.clone(), status.clone(), runner);
        slot.replace(Some(popup.clone()));
        popup.controller().show();
    });
    (app, current)
}

pub fn run(edge: &str, offset: i32) -> i32 {
    let (app, _popup) = build_application(edge, offset, None, None, Runner::Thread);
    let argv0 = std::env::args().next().unwrap_or_else(|| "tessel-tiling".into());
    // Python passes only argv[0]: a remote launch just activates.
    let code = app.run_with_args(&[argv0]);
    i32::from(code)
}
