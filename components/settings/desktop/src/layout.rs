//! Desktop Layout (layout_page.LayoutSettings): apply a saved arrangement of
//! the dock, panel and desktop; the current layout is kept for Restore/Undo.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gs_base::style::{choose, page_header, Toast};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use gtk::pango;
use tessel_core::clock::{ClockPreferences, ClockPreferencesStore};
use tessel_core::tr;

use crate::bridge::{settings_fields, BridgeOptions, DockSettingsBridge, PresetBridge};
use crate::layout_presets::{self as presets, ClockBackend, Preset, PresetIcon, TilingBackend, WorkspaceBackend};
use crate::util::{label, Runner};

pub const LAYOUT_CSS: &str = "
.layout-settings .subtitle { color: alpha(@theme_fg_color, .66); }
.layout-settings flowbox { background: transparent; }
.layout-settings flowboxchild { padding: 0; border-radius: 8px; background: transparent; }
.layout-settings .layout-card { padding: 12px; border-radius: 8px;
    border: 2px solid alpha(@theme_fg_color, .10); background-color: alpha(@theme_fg_color, .035); }
.layout-settings flowboxchild:hover .layout-card { background-color: alpha(@theme_fg_color, .07); }
.layout-settings flowboxchild:selected .layout-card { border-color: @theme_selected_bg_color; }
.layout-settings .layout-name { font-weight: 600; }
.layout-settings .layout-current { color: alpha(@theme_fg_color, .66); font-size: 9pt; }
.layout-settings button.suggested-action { padding: 8px 16px; border-radius: 8px; }
";

/// `(workspaces, gapless, clock)` read from the backends.
pub type DesktopState = (Option<i64>, Option<bool>, Option<ClockPreferences>);

pub struct LayoutCard {
    pub child: gtk::FlowBoxChild,
    pub preset: Preset,
    pub current: gtk::Label,
    pub matched: Cell<bool>,
}

/// Python keyword arguments of `LayoutSettings(...)`; `None` = the default backend.
#[derive(Default)]
pub struct LayoutOptions {
    pub bridge: Option<Arc<dyn PresetBridge>>,
    pub workspace_backend: Option<Option<Arc<dyn WorkspaceBackend>>>,
    pub tiling_backend: Option<Option<Arc<dyn TilingBackend>>>,
    pub clock_store: Option<Option<Arc<dyn ClockBackend>>>,
    pub presets: Option<(Vec<Preset>, Vec<String>)>,
    pub previous_path: Option<PathBuf>,
    pub runner: Runner,
}

#[derive(Clone)]
struct Backends {
    bridge: Arc<dyn PresetBridge>,
    workspaces: Option<Arc<dyn WorkspaceBackend>>,
    tiling: Option<Arc<dyn TilingBackend>>,
    clock: Option<Arc<dyn ClockBackend>>,
}

impl Backends {
    fn state(&self) -> DesktopState {
        let workspaces = self.workspaces.as_ref().and_then(|b| b.load().ok()).map(|s| s.count);
        let gapless = self.tiling.as_ref().and_then(|b| b.load_preferences().ok())
            .map(|prefs| prefs.get("gapless") == Some(&serde_json::Value::Bool(true)));
        let clock = self.clock.as_ref().and_then(|c| c.load().ok());
        (workspaces, gapless, clock)
    }
}

pub struct LayoutSettings {
    pub widget: gtk::ScrolledWindow,
    pub flow: gtk::FlowBox,
    pub apply_button: gtk::Button,
    pub restore_button: gtk::Button,
    pub status: gtk::Label,
    pub toast: Toast,
    pub cards: Vec<LayoutCard>,
    pub presets: Vec<Preset>,
    pub busy: Cell<bool>,
    refreshing: Cell<bool>,
    selected_once: Cell<bool>,
    pub current: RefCell<Option<DesktopState>>,
    pub previous: RefCell<Option<Preset>>,
    previous_path: Option<PathBuf>,
    backends: Backends,
    runner: Runner,
    provider: gtk::CssProvider,
}

fn default_workspaces() -> Option<Arc<dyn WorkspaceBackend>> {
    gs_system::workspace_config::WorkspaceConfig::session().ok().map(|c| Arc::new(c) as Arc<dyn WorkspaceBackend>)
}

fn default_tiling() -> Option<Arc<dyn TilingBackend>> {
    Some(Arc::new(tessel_tiling::config::TilingConfig::default()))
}

fn default_clock() -> Option<Arc<dyn ClockBackend>> {
    Some(Arc::new(Mutex::new(ClockPreferencesStore::default())))
}

fn picture(preset: &Preset) -> gtk::Picture {
    let picture = gtk::Picture::builder().can_shrink(true).content_fit(gtk::ContentFit::Contain).build();
    picture.set_size_request(200, 125);
    match &preset.icon {
        PresetIcon::File(path) if path.is_file() => picture.set_filename(Some(path)),
        PresetIcon::Embedded(bytes) => {
            if let Ok(texture) = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_static(bytes)) {
                picture.set_paintable(Some(&texture));
            }
        }
        _ => {}
    }
    picture.set_alternative_text(Some(&preset.name));
    picture
}

impl LayoutSettings {
    pub fn new(application: Option<gtk::Application>, options: LayoutOptions) -> Result<Rc<Self>, String> {
        let bridge: Arc<dyn PresetBridge> = match options.bridge {
            Some(bridge) => bridge,
            None => DockSettingsBridge::new(application, BridgeOptions { fields: settings_fields(), read: false, ..Default::default() },
                                            None)?.core(),
        };
        let backends = Backends {
            bridge,
            workspaces: options.workspace_backend.unwrap_or_else(default_workspaces),
            tiling: options.tiling_backend.unwrap_or_else(default_tiling),
            clock: options.clock_store.unwrap_or_else(default_clock),
        };
        let (preset_list, problems) = options.presets.unwrap_or_else(presets::load_presets);
        let widget = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        widget.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        widget.add_css_class("layout-settings");
        let provider = tessel_gtk::utils::add_css(LAYOUT_CSS, Some(&WidgetExt::display(&widget)), gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(24).margin_bottom(24)
            .margin_start(24).margin_end(24).build();
        widget.set_child(Some(&content));
        content.append(&page_header(&tr!("Desktop Layout"),
            Some(&tr!("Arrange the dock, panel, appearance and desktop behavior from a preset. Wallpapers and pinned apps are not changed. Your current layout is kept so you can restore it.")),
            Some("view-dual-symbolic")).root);
        let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::Single).homogeneous(true)
            .max_children_per_line(4).column_spacing(14).row_spacing(14).halign(gtk::Align::Start).build();
        let mut cards = Vec::new();
        for preset in &preset_list {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
            card.add_css_class("layout-card");
            card.append(&picture(preset));
            let name = gtk::Label::builder().label(&preset.name).xalign(0.5).ellipsize(pango::EllipsizeMode::End).build();
            name.add_css_class("layout-name");
            card.append(&name);
            let current = gtk::Label::builder().label(tr!("Current layout")).xalign(0.5).visible(false).build();
            current.add_css_class("layout-current");
            card.append(&current);
            let child = gtk::FlowBoxChild::builder().child(&card).build();
            flow.append(&child);
            cards.push(LayoutCard { child, preset: preset.clone(), current, matched: Cell::new(false) });
        }
        content.append(&flow);
        if preset_list.is_empty() {
            content.append(&label(&tr!("No desktop layout presets are installed."), Some("subtitle")));
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let apply_button = gtk::Button::builder().label(tr!("Apply Layout")).sensitive(false).build();
        apply_button.add_css_class("suggested-action");
        actions.append(&apply_button);
        let restore_button = gtk::Button::builder().label(tr!("Restore previous layout")).sensitive(false).build();
        restore_button.set_tooltip_text(Some(&tr!("Return to the layout you had before the last preset was applied.")));
        actions.append(&restore_button);
        let status = label("", Some("subtitle"));
        status.set_hexpand(true);
        actions.append(&status);
        content.append(&actions);
        let toast = Toast::new();
        content.append(toast.widget());
        for problem in &problems {
            content.append(&label(problem, Some("error")));
        }
        let page = Rc::new(Self {
            widget, flow: flow.clone(), apply_button: apply_button.clone(), restore_button: restore_button.clone(), status,
            toast, cards, presets: preset_list, busy: Cell::new(false), refreshing: Cell::new(false),
            selected_once: Cell::new(false), current: RefCell::new(None), previous: RefCell::new(None),
            previous_path: options.previous_path, backends, runner: options.runner, provider,
        });
        let weak = Rc::downgrade(&page);
        flow.connect_selected_children_changed(move |_| {
            if let Some(page) = weak.upgrade() {
                page.update();
            }
        });
        let weak = Rc::downgrade(&page);
        flow.connect_child_activated(move |_, _| {
            if let Some(page) = weak.upgrade() {
                page.update();
            }
        });
        let weak = Rc::downgrade(&page);
        apply_button.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.apply();
            }
        });
        let weak = Rc::downgrade(&page);
        restore_button.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.restore_previous();
            }
        });
        page.refresh();
        Ok(page)
    }

    pub fn card(&self, key: &str) -> Option<&LayoutCard> {
        self.cards.iter().find(|card| card.preset.key == key)
    }

    fn card_of(&self, child: &gtk::FlowBoxChild) -> Option<&LayoutCard> {
        self.cards.iter().find(|card| &card.child == child)
    }

    pub fn selected(&self) -> Option<Preset> {
        self.flow.selected_children().first().and_then(|child| self.card_of(child)).map(|card| card.preset.clone())
    }

    /// Read the dock, workspaces, tiling and clock off the GTK thread.
    pub fn refresh(self: &Rc<Self>) {
        if self.busy.get() || self.refreshing.get() {
            return;
        }
        self.refreshing.set(true);
        let backends = self.backends.clone();
        let path = self.previous_path.clone();
        let name = tr!("Previous layout");
        let weak = Rc::downgrade(self);
        self.runner.run(move || {
            backends.bridge.refresh();
            (backends.state(), presets::load_previous(path.as_deref(), &name))
        }, move |result| {
            if let Some(page) = weak.upgrade() {
                page.refreshed(result);
            }
        });
    }

    fn refreshed(&self, result: Option<(DesktopState, Option<Preset>)>) {
        self.refreshing.set(false);
        let Some((state, previous)) = result else { return };
        let config = self.backends.bridge.config();
        for card in &self.cards {
            let matched = presets::matches(&card.preset, &config, state.0, state.1, state.2.as_ref());
            card.matched.set(matched);
            card.current.set_visible(matched);
        }
        self.current.replace(Some(state));
        self.previous.replace(previous);
        if self.selected().is_none() || !self.selected_once.get() {
            // The first read selects the layout in use (GTK may pre-select the first card).
            self.selected_once.set(true);
            let initial = self.cards.iter().find(|c| c.matched.get()).or_else(|| self.cards.first());
            if let Some(initial) = initial {
                self.flow.select_child(&initial.child);
            }
        }
        self.update();
    }

    fn update(&self) {
        let chosen = self.flow.selected_children();
        let matched = chosen.first().and_then(|c| self.card_of(c)).is_some_and(|c| c.matched.get());
        self.apply_button.set_sensitive(!self.busy.get() && !chosen.is_empty() && !matched);
        let previous = self.previous.borrow().clone();
        self.restore_button.set_sensitive(!self.busy.get() && previous.as_ref().is_some_and(|p| !self.matches(p)));
    }

    fn matches(&self, preset: &Preset) -> bool {
        match self.current.borrow().as_ref() {
            Some((w, g, c)) => presets::matches(preset, &self.backends.bridge.config(), *w, *g, c.as_ref()),
            None => false,
        }
    }

    /// Confirm what will change, then apply; the current layout is kept first.
    pub fn apply(self: &Rc<Self>) {
        let Some(preset) = self.selected() else { return };
        if self.busy.get() {
            return;
        }
        let (w, g, c) = self.current.borrow().clone().unwrap_or((None, None, None));
        let Ok(mut changes) = presets::describe_changes(&preset, &self.backends.bridge.config(), w, g, c.as_ref(),
                                                         &presets::translated) else { return };
        if changes.is_empty() {
            changes.push(tr!("No visible changes"));
        }
        let detail = format!("{}\n• {}\n\n{}", tr!("This changes:"), changes.join("\n• "),
                             tr!("Your current layout is saved, so you can restore it afterwards."));
        let weak = Rc::downgrade(self);
        choose(&self.widget, &tr!("Apply the {name} layout?", name = preset.name.as_str()), &detail,
               &[tr!("Cancel"), tr!("Apply Layout")], move |index| {
                   if index == Some(1) {
                       if let Some(page) = weak.upgrade() {
                           page.apply_preset(preset, false);
                       }
                   }
               }, 1, Some(0));
    }

    pub fn restore_previous(self: &Rc<Self>) {
        let previous = self.previous.borrow().clone();
        if let Some(previous) = previous {
            if !self.busy.get() {
                self.apply_preset(previous, true);
            }
        }
    }

    fn apply_preset(self: &Rc<Self>, preset: Preset, restoring: bool) {
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.update();
        self.status.remove_css_class("error");
        self.status.set_text(&tr!("Applying layout…"));
        let backends = self.backends.clone();
        let path = self.previous_path.clone();
        let target = preset.clone();
        let weak = Rc::downgrade(self);
        self.runner.run(move || {
            let (workspaces, gapless, clock) = backends.state();
            let snapshot = presets::capture(&backends.bridge.config(), workspaces, gapless, "Previous layout", clock.as_ref());
            let save_error = presets::save_previous(&snapshot, path.as_deref()).err().map(|e| match &path {
                Some(path) => tessel_core::pyerr::os_error_path(&e, path),
                None => tessel_core::pyerr::os_error_path(&e, &presets::previous_path()),
            });
            let errors = presets::apply_preset(&target, backends.bridge.as_ref(), backends.workspaces.as_deref(),
                                               backends.tiling.as_deref(), backends.clock.as_deref());
            (errors, save_error)
        }, move |result| {
            let Some(page) = weak.upgrade() else { return };
            page.busy.set(false);
            let (errors, save_error) = match result {
                Some((Ok(errors), save_error)) => (errors, save_error),
                Some((Err(error), _)) => (vec![error], None),
                None => (vec!["worker failed".to_owned()], None),
            };
            if !errors.is_empty() {
                page.status.set_text(&tr!("Some settings could not be applied: {error}", error = errors.join("; ")));
                page.status.add_css_class("error");
            } else if restoring {
                page.status.set_text(&tr!("Previous layout restored."));
            } else {
                page.status.set_text(&tr!("{name} layout applied.", name = preset.name.as_str()));
            }
            if let Some(save_error) = save_error {
                let text = format!("{}\n{}", page.status.text(),
                                   tr!("The previous layout could not be saved: {error}", error = save_error.as_str()));
                page.status.set_text(&text);
            } else if errors.is_empty() {
                let message = if restoring { tr!("Previous layout restored.") } else { tr!("{name} layout applied.", name = preset.name.as_str()) };
                let weak = Rc::downgrade(&page);
                page.toast.show(&message, Some(&tr!("Undo")), Some(Box::new(move || {
                    if let Some(page) = weak.upgrade() {
                        page.restore_previous();
                    }
                })), 10);
            }
            page.refresh();
        });
    }

    pub fn close(&self) {
        self.toast.dismiss();
        self.backends.bridge.close();
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.widget), &self.provider);
    }
}

/// `build_page("layout", host)` with injection.
pub fn build_layout_page(host: &Host, options: LayoutOptions) -> Result<SettingsPage, String> {
    let controller = LayoutSettings::new(host.application(), options)?;
    let page = SettingsPage::new(controller.widget.clone());
    let (c1, c2, c3) = (controller.clone(), controller.clone(), controller.clone());
    page.set_close(move || c1.close());
    page.set_activate(move || c2.refresh());
    page.set_can_close(move || !c3.busy.get());
    page.add_target("presets", &controller.flow);
    page.add_target("previous", &controller.restore_button);
    page.set_controller(controller);
    Ok(page)
}
