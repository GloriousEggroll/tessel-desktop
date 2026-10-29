//! `EditorWindow` (editor_ui.py): one document per window, guarded document
//! operations, GtkSourceView controls, crash-recovery drafts.

use crate::app::{EditorApplication, DESKTOP_ID};
use crate::document::{self, DocError, Document, Target, ENCODINGS, NEWLINES};
use crate::preferences::Options;
use crate::recovery::{self, Draft};
use crate::style::{self, editor_surface_css, font_css, line_count};
use crate::worker::Worker;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;
use suite_core::tr;
use suite_gtk::utils::{accessible, headerbar, image, set_margins};

/// A deferred operation run with the window (`guard(callback)`, `save(after=...)`).
pub type Callback = Box<dyn FnOnce(&EditorWindow)>;

const BUSY_ACTIONS: [&str; 8] = ["open", "save", "save-as", "reload", "preferences", "paste", "replace", "print"];

fn button(icon: &str, title: &str, action: Option<&str>) -> gtk::Button {
    let result = gtk::Button::builder().child(&image(icon, 16)).tooltip_text(tr!(title)).build();
    accessible(&result, &tr!(title));
    if let Some(action) = action {
        result.set_action_name(Some(action));
    }
    result
}

pub struct WindowInner {
    pub app: EditorApplication,
    pub window: gtk::ApplicationWindow,
    pub document: RefCell<Document>,
    /// Bumped whenever `document` is replaced (Python compares object identity).
    pub document_serial: Cell<u64>,
    pub options: RefCell<Options>,
    pub encoding: RefCell<String>,
    pub newline: RefCell<String>,
    pub format_modified: Cell<bool>,
    pub busy: Cell<bool>,
    pub closing: Cell<bool>,
    pub restoring: Cell<bool>,
    destroyed_done: Cell<bool>,
    pub worker: Worker,
    pub monitor: RefCell<Option<gio::FileMonitor>>,
    pub pending_monitor: RefCell<Option<glib::SourceId>>,
    pub external_pending: Cell<bool>,
    pub dialog: RefCell<Option<gtk::Window>>,
    pub print_operation: RefCell<Option<gtk::PrintOperation>>,
    pub style_name: RefCell<Option<String>>,
    pub style_timer: RefCell<Option<glib::SourceId>>,
    pub bookmarks: RefCell<Vec<sourceview5::Mark>>,
    pub draft_id: RefCell<String>,
    pub draft_timer: RefCell<Option<glib::SourceId>>,
    pub draft_delay: Cell<f64>,
    pub draft_written: Cell<bool>,
    pub last_remote_check: Cell<i64>,
    pub font_provider: gtk::CssProvider,
    pub font_scope: String,
    pub header: gtk::HeaderBar,
    pub title_label: gtk::Label,
    pub path_label: gtk::Label,
    pub save_button: gtk::Button,
    pub menu_button: RefCell<Option<gtk::MenuButton>>,
    pub main_menu: gio::Menu,
    pub recent_menu: gio::Menu,
    pub root: gtk::Box,
    pub banner: gtk::Box,
    pub banner_label: gtk::Label,
    pub banner_reload: gtk::Button,
    pub recovery_bar: gtk::Box,
    pub recovery_label: gtk::Label,
    pub recovery_discard: gtk::Button,
    pub recovery_restore: gtk::Button,
    pub recovery_drafts: RefCell<Vec<Draft>>,
    pub buffer: sourceview5::Buffer,
    pub view: sourceview5::View,
    pub completion: sourceview5::CompletionWords,
    pub vim: sourceview5::VimIMContext,
    pub vim_controller: gtk::EventControllerKey,
    pub search_settings: sourceview5::SearchSettings,
    pub search_context: sourceview5::SearchContext,
    pub search_panel: gtk::Box,
    pub find_entry: gtk::Entry,
    pub replace_row: gtk::Box,
    pub replace_entry: gtk::Entry,
    pub search_count: gtk::Label,
    pub position: gtk::Label,
    pub details: gtk::Label,
    pub language_label: gtk::Label,
    pub vim_label: gtk::Label,
}

#[derive(Clone)]
pub struct EditorWindow(pub Rc<WindowInner>);

impl std::ops::Deref for EditorWindow {
    type Target = WindowInner;
    fn deref(&self) -> &WindowInner { &self.0 }
}

static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl EditorWindow {
    fn weak(&self) -> Weak<WindowInner> {
        Rc::downgrade(&self.0)
    }

    /// A `Fn()` for signal handlers that runs `f` while the window is alive.
    fn hook(&self, f: impl Fn(&EditorWindow) + 'static) -> impl Fn() + 'static {
        let weak = self.weak();
        move || {
            if let Some(inner) = weak.upgrade() {
                f(&EditorWindow(inner));
            }
        }
    }

    pub fn new(application: &EditorApplication) -> EditorWindow {
        let window = gtk::ApplicationWindow::builder().application(&application.app).title(tr!("Text Editor"))
            .default_width(1000).default_height(720).build();
        window.add_css_class("text-editor");
        let font_scope = format!("editor-font-{}", SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));

        let title_label = gtk::Label::new(Some(&tr!("Untitled")));
        title_label.add_css_class("editor-title");
        let path_label = gtk::Label::builder().label(tr!("Text Editor")).ellipsize(pango::EllipsizeMode::Middle).max_width_chars(60).build();
        path_label.add_css_class("muted");
        path_label.add_css_class("editor-subtitle");
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        titles.append(&title_label);
        titles.append(&path_label);
        let header = headerbar(&tr!("Text Editor"));
        header.set_title_widget(Some(&titles));
        header.pack_start(&button("document-open-symbolic", "Open (Ctrl+O)", Some("win.open")));
        // A regular header tool (icon and label), not an accent-filled block.
        let save_button = button("document-save-symbolic", "Save (Ctrl+S)", Some("win.save"));
        let save_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        save_content.append(&image("document-save-symbolic", 16));
        save_content.append(&gtk::Label::new(Some(&tr!("Save"))));
        save_button.set_child(Some(&save_content));
        window.set_titlebar(Some(&header));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.set_child(Some(&root));

        let banner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        banner.add_css_class("error-banner");
        let banner_label = gtk::Label::builder().xalign(0.0).wrap(true).hexpand(true).selectable(true).build();
        banner.append(&banner_label);
        let banner_reload = gtk::Button::with_label(&tr!("Reload"));
        banner.append(&banner_reload);
        let dismiss = button("window-close-symbolic", "Dismiss", None);
        banner.append(&dismiss);
        banner.set_visible(false);
        root.append(&banner);
        let recovery_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        recovery_bar.add_css_class("error-banner");
        let recovery_label = gtk::Label::builder().xalign(0.0).wrap(true).hexpand(true).build();
        recovery_bar.append(&recovery_label);
        let recovery_discard = gtk::Button::with_label(&tr!("Discard"));
        recovery_bar.append(&recovery_discard);
        let recovery_restore = gtk::Button::with_label(&tr!("Restore"));
        recovery_restore.add_css_class("suggested-action");
        recovery_bar.append(&recovery_restore);
        recovery_bar.set_visible(false);
        root.append(&recovery_bar);

        let buffer = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
        buffer.set_highlight_matching_brackets(true);
        let view = sourceview5::View::with_buffer(&buffer);
        view.add_css_class(&font_scope);
        view.set_monospace(true);
        view.set_left_margin(10);
        view.set_right_margin(10);
        view.set_top_margin(12);
        view.set_bottom_margin(12);
        view.set_highlight_current_line(true);
        view.set_smart_backspace(true);
        view.set_indent_on_tab(true);
        view.set_show_line_marks(true);
        let completion = sourceview5::CompletionWords::new(Some(&tr!("Document Words")));
        completion.register(&buffer);
        view.completion().add_provider(&completion);
        let vim = sourceview5::VimIMContext::new();
        let vim_controller = gtk::EventControllerKey::new();
        vim_controller.set_im_context(Some(&vim));
        view.add_controller(vim_controller.clone());
        let attributes = sourceview5::MarkAttributes::new();
        attributes.set_icon_name("bookmark-new-symbolic");
        view.set_mark_attributes("bookmark", &attributes, 1);
        let search_settings = sourceview5::SearchSettings::new();
        search_settings.set_wrap_around(true);
        let search_context = sourceview5::SearchContext::new(&buffer, Some(&search_settings));

        // _search_panel()
        let search_panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
        search_panel.add_css_class("search-panel");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let find_entry = gtk::Entry::builder().placeholder_text(tr!("Find in document")).hexpand(true).build();
        row.append(&find_entry);
        let previous = button("go-up-symbolic", "Previous match (Shift+F3)", None);
        let next = button("go-down-symbolic", "Next match (F3)", None);
        let close_search = button("window-close-symbolic", "Close search", None);
        row.append(&previous);
        row.append(&next);
        row.append(&close_search);
        search_panel.append(&row);
        let options_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let mut checks = Vec::new();
        for title in ["Match case", "Whole words", "Regular expression"] {
            let check = gtk::CheckButton::with_label(&tr!(title));
            options_row.append(&check);
            checks.push(check);
        }
        let search_count = gtk::Label::builder().xalign(1.0).hexpand(true).build();
        search_count.add_css_class("muted");
        options_row.append(&search_count);
        search_panel.append(&options_row);
        let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let replace_entry = gtk::Entry::builder().placeholder_text(tr!("Replace with")).hexpand(true).build();
        replace_row.append(&replace_entry);
        let replace = gtk::Button::with_label(&tr!("Replace"));
        replace_row.append(&replace);
        let replace_all = gtk::Button::with_label(&tr!("Replace All"));
        replace_row.append(&replace_all);
        search_panel.append(&replace_row);
        root.append(&search_panel);
        search_panel.set_visible(false);

        let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).child(&view).build();
        root.append(&scroll);
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        status.add_css_class("editor-status");
        let position = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        let details = gtk::Label::new(None);
        let language_label = gtk::Label::new(Some(&tr!("Plain Text")));
        let vim_label = gtk::Label::builder().max_width_chars(30).ellipsize(pango::EllipsizeMode::End).build();
        status.append(&position);
        status.append(&vim_label);
        status.append(&language_label);
        status.append(&details);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&status);

        let this = EditorWindow(Rc::new(WindowInner {
            app: application.clone(), window: window.clone(), document: RefCell::new(Document::default()), document_serial: Cell::new(0),
            options: RefCell::new(application.preferences.borrow().options.clone()), encoding: RefCell::new("utf-8".into()),
            newline: RefCell::new("LF".into()), format_modified: Cell::new(false), busy: Cell::new(false), closing: Cell::new(false),
            restoring: Cell::new(false), destroyed_done: Cell::new(false), worker: Worker::new("text-editor-files"), monitor: RefCell::new(None),
            pending_monitor: RefCell::new(None), external_pending: Cell::new(false), dialog: RefCell::new(None),
            print_operation: RefCell::new(None), style_name: RefCell::new(None), style_timer: RefCell::new(None),
            bookmarks: RefCell::new(Vec::new()), draft_id: RefCell::new(recovery::new_identifier()), draft_timer: RefCell::new(None),
            draft_delay: Cell::new(recovery::DELAY_SECONDS), draft_written: Cell::new(false), last_remote_check: Cell::new(0),
            font_provider: gtk::CssProvider::new(), font_scope, header, title_label, path_label, save_button,
            menu_button: RefCell::new(None), main_menu: gio::Menu::new(), recent_menu: gio::Menu::new(), root, banner, banner_label,
            banner_reload, recovery_bar, recovery_label, recovery_discard, recovery_restore, recovery_drafts: RefCell::new(Vec::new()),
            buffer, view, completion, vim, vim_controller, search_settings, search_context, search_panel, find_entry, replace_row,
            replace_entry, search_count, position, details, language_label, vim_label,
        }));
        application.windows.borrow_mut().push(this.clone());

        let weak = this.weak();
        window.connect_close_request(move |_| match weak.upgrade() {
            Some(inner) => EditorWindow(inner).close_requested(),
            None => glib::Propagation::Proceed,
        });
        let weak = this.weak();
        window.connect_destroy(move |_| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).destroyed();
            }
        });
        this.actions();
        this.menus();
        // The primary menu ends the header bar; Save and Find sit before it.
        this.header.pack_end(&this.save_button);
        this.header.pack_end(&button("edit-find-symbolic", "Find (Ctrl+F)", Some("win.find")));
        let h = this.hook(|w| w.reload());
        this.banner_reload.connect_clicked(move |_| h());
        let h = this.hook(|w| w.banner.set_visible(false));
        dismiss.connect_clicked(move |_| h());
        let h = this.hook(|w| w.recovery_response(false));
        this.recovery_discard.connect_clicked(move |_| h());
        let h = this.hook(|w| w.recovery_response(true));
        this.recovery_restore.connect_clicked(move |_| h());

        let h = this.hook(|w| w.vim_command_status());
        this.vim.connect_command_bar_text_notify(move |_| h());
        let h = this.hook(|w| w.vim_command_status());
        this.vim.connect_command_text_notify(move |_| h());
        let weak = this.weak();
        this.vim.connect_execute_command(move |_, command| match weak.upgrade() {
            Some(inner) => EditorWindow(inner).vim_command(command),
            None => false,
        });
        let h = this.hook(|w| {
            w.update_status();
            w.schedule_draft();
        });
        this.buffer.connect_changed(move |_| h());
        let h = Rc::new(this.hook(|w| w.update_status()));
        let c = h.clone();
        this.buffer.connect_modified_changed(move |_| c());
        let c = h.clone();
        this.buffer.connect_mark_set(move |_, _, _| c());
        let c = h.clone();
        this.buffer.connect_can_undo_notify(move |_| c());
        let c = h.clone();
        this.buffer.connect_can_redo_notify(move |_| c());
        let c = h.clone();
        this.buffer.connect_has_selection_notify(move |_| c());
        let h = Rc::new(this.hook(|w| w.search_count_changed()));
        let c = h.clone();
        this.search_context.connect_occurrences_count_notify(move |_| c());
        let c = h.clone();
        this.search_context.connect_regex_error_notify(move |_| c());
        let settings = this.search_settings.clone();
        this.find_entry.connect_changed(move |entry| {
            let text = entry.text();
            settings.set_search_text(if text.is_empty() { None } else { Some(text.as_str()) });
        });
        let h = this.hook(|w| {
            w.find(false);
        });
        this.find_entry.connect_activate(move |_| h());
        let h = this.hook(|w| {
            w.find(true);
        });
        previous.connect_clicked(move |_| h());
        let h = this.hook(|w| {
            w.find(false);
        });
        next.connect_clicked(move |_| h());
        let h = this.hook(|w| w.hide_search());
        close_search.connect_clicked(move |_| h());
        for (index, check) in checks.iter().enumerate() {
            let settings = this.search_settings.clone();
            check.connect_toggled(move |check| match index {
                0 => settings.set_case_sensitive(check.is_active()),
                1 => settings.set_at_word_boundaries(check.is_active()),
                _ => settings.set_regex_enabled(check.is_active()),
            });
        }
        let h = this.hook(|w| w.replace_one());
        replace.connect_clicked(move |_| h());
        let h = this.hook(|w| w.replace_all());
        replace_all.connect_clicked(move |_| h());

        gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &this.font_provider,
                                                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 40);
        this.apply_options(false);
        let weak = this.weak();
        let timer = glib::timeout_add_seconds_local(2, move || match weak.upgrade() {
            Some(inner) => {
                EditorWindow(inner).sync_style();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        this.style_timer.replace(Some(timer));
        this.update_status();
        let controller = gtk::EventControllerKey::new();
        let weak = this.weak();
        controller.connect_key_pressed(move |_, key, _, _| {
            if let Some(w) = weak.upgrade() {
                if key == gdk::Key::Escape && w.search_panel.is_visible() {
                    EditorWindow(w).hide_search();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        window.add_controller(controller);
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = this.weak();
        drop.connect_drop(move |_, value, _, _| {
            let Some(w) = weak.upgrade() else { return false };
            let files = value.get::<gdk::FileList>().map(|list| list.files()).unwrap_or_default();
            if files.is_empty() {
                return false;
            }
            // Dropped files get their own windows and cannot replace unsaved work.
            w.app.app.open(&files, "");
            true
        });
        window.add_controller(drop);
        // Remote (GVfs) files often have no change notifications; check them
        // again whenever the window regains focus.
        let h = this.hook(|w| w.focus_check());
        window.connect_is_active_notify(move |_| h());
        let h = this.hook(|w| w.app.offer_recovery(Some(w)));
        glib::idle_add_local_once(h);
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    fn simple_action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.window.lookup_action(name).and_downcast::<gio::SimpleAction>()
    }

    fn actions(&self) {
        // One document per window: New opens another window (GNOME style)
        // and never replaces this window's document.
        let actions: Vec<(&str, Box<dyn Fn(&EditorWindow)>)> = vec![
            ("new", Box::new(|w| { w.new_window(); })), ("new-window", Box::new(|w| { w.new_window(); })),
            ("set-default", Box::new(|w| w.set_default())),
            ("open", Box::new(|w| w.guard(Box::new(|w| w.open_dialog())))),
            ("save", Box::new(|w| w.save(None))), ("save-as", Box::new(|w| w.save_as(None))),
            ("reload", Box::new(|w| w.reload())), ("close", Box::new(|w| w.window.close())),
            ("print", Box::new(|w| w.print_document(None))),
            ("undo", Box::new(|w| w.buffer.undo())), ("redo", Box::new(|w| w.buffer.redo())),
            ("cut", Box::new(|w| w.buffer.cut_clipboard(&w.window.clipboard(), true))),
            ("copy", Box::new(|w| w.buffer.copy_clipboard(&w.window.clipboard()))),
            ("paste", Box::new(|w| w.buffer.paste_clipboard(&w.window.clipboard(), None, true))),
            ("select-all", Box::new(|w| {
                let (start, end) = w.buffer.bounds();
                w.buffer.select_range(&start, &end);
            })),
            ("find", Box::new(|w| w.show_search(false))), ("replace", Box::new(|w| w.show_search(true))),
            ("find-next", Box::new(|w| { w.find(false); })), ("find-previous", Box::new(|w| { w.find(true); })),
            ("goto", Box::new(|w| w.go_to_line())), ("preferences", Box::new(|w| w.preferences_dialog())),
            ("zoom-in", Box::new(|w| w.zoom(1))), ("zoom-out", Box::new(|w| w.zoom(-1))), ("zoom-reset", Box::new(|w| w.zoom(0))),
            ("bookmark", Box::new(|w| w.toggle_bookmark())), ("next-bookmark", Box::new(|w| w.next_bookmark(false))),
            ("previous-bookmark", Box::new(|w| w.next_bookmark(true))),
        ];
        for (name, callback) in actions {
            let action = gio::SimpleAction::new(name, None);
            let weak = self.weak();
            action.connect_activate(move |_, _| {
                if let Some(inner) = weak.upgrade() {
                    callback(&EditorWindow(inner));
                }
            });
            self.window.add_action(&action);
        }
        let options = self.options.borrow().clone();
        for (key, value) in [("wrap", options.wrap), ("line_numbers", options.line_numbers), ("whitespace", options.whitespace)] {
            let action = gio::SimpleAction::new_stateful(&key.replace('_', "-"), None, &value.to_variant());
            let weak = self.weak();
            action.connect_activate(move |action, _| {
                if let Some(inner) = weak.upgrade() {
                    EditorWindow(inner).toggle_option(action, key);
                }
            });
            self.window.add_action(&action);
        }
    }

    fn menus(&self) {
        // Everything lives in the suite's single primary menu; the window
        // accelerators set at startup work without a menubar.
        let root = &self.main_menu;
        let section = |entries: &[(&str, &str)]| {
            let menu = gio::Menu::new();
            for (label, action) in entries {
                menu.append(Some(&tr!(*label)), Some(&format!("win.{action}")));
            }
            menu
        };
        let files = section(&[("New Window", "new-window"), ("Open…", "open"), ("Save As…", "save-as"),
                              ("Reload", "reload"), ("Print…", "print"), ("Close", "close")]);
        files.insert_submenu(2, Some(&tr!("Open Recent")), &self.recent_menu);
        root.append_section(None, &files);
        let groups = gio::Menu::new();
        for (title, entries) in [
            ("Edit", &[("Undo", "undo"), ("Redo", "redo"), ("Cut", "cut"), ("Copy", "copy"), ("Paste", "paste"),
                       ("Select All", "select-all"), ("Find…", "find"), ("Replace…", "replace"), ("Go to Line…", "goto")][..]),
            ("View", &[("Word Wrap", "wrap"), ("Line Numbers", "line-numbers"), ("Visible Whitespace", "whitespace"),
                       ("Zoom In", "zoom-in"), ("Zoom Out", "zoom-out"), ("Reset Zoom", "zoom-reset")][..]),
            ("Bookmarks", &[("Toggle Bookmark", "bookmark"), ("Next Bookmark", "next-bookmark"),
                            ("Previous Bookmark", "previous-bookmark")][..]),
        ] {
            groups.append_submenu(Some(&tr!(title)), &section(entries));
        }
        root.append_section(None, &groups);
        root.append_section(None, &section(&[("Preferences…", "preferences"), ("Set as Default Text Editor", "set-default")]));
        let details = suite_gtk::app_menu::AboutDetails {
            name: "Text Editor".into(), icon: "accessories-text-editor".into(), version: None,
            comments: Some("Edit plain text and source code".into()), website: None,
        };
        let menu_button = suite_gtk::app_menu::install_app_menu(&self.app.app, &self.window, suite_gtk::app_menu::MenuTarget::Model(root.clone()),
                                                                details, style::shortcuts(), Some(&self.header), None);
        self.menu_button.replace(Some(menu_button));
        let recent = gio::SimpleAction::new("open-recent", Some(glib::VariantTy::STRING));
        let weak = self.weak();
        recent.connect_activate(move |_, parameter| {
            let (Some(inner), Some(path)) = (weak.upgrade(), parameter.and_then(|p| p.str().map(str::to_owned))) else { return };
            EditorWindow(inner).guard(Box::new(move |w| w.load(Target::Arg(path), None)));
        });
        self.window.add_action(&recent);
        self.refresh_recent();
    }

    pub fn menu_button(&self) -> gtk::MenuButton {
        self.menu_button.borrow().clone().expect("menu installed")
    }

    pub fn refresh_recent(&self) {
        self.recent_menu.remove_all();
        for path in self.app.preferences.borrow().recent.iter() {
            let label = gio::File::for_commandline_arg(path).basename().map(|b| b.to_string_lossy().into_owned())
                .filter(|b| !b.is_empty()).unwrap_or_else(|| path.clone());
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("win.open-recent"), Some(&path.to_variant()));
            self.recent_menu.append_item(&item);
        }
    }

    fn remember(&self) {
        let Some(path) = self.document.borrow().identifier() else { return };
        {
            let mut preferences = self.app.preferences.borrow_mut();
            let mut recent = vec![path.clone()];
            recent.extend(preferences.recent.iter().filter(|p| **p != path).cloned());
            recent.truncate(12);
            preferences.recent = recent;
            preferences.save();
        }
        for window in self.app.editor_windows() {
            window.refresh_recent();
        }
    }

    // Search -------------------------------------------------------------
    fn search_count_changed(&self) {
        let error = self.search_context.regex_error();
        let count = self.search_context.occurrences_count();
        match &error {
            Some(error) => self.search_count.set_text(error.message()),
            None => self.search_count.set_text(&tr!("{count} matches", count = count.max(0) as i64)),
        }
        self.search_count.set_tooltip_text(error.as_ref().map(|e| e.message()));
    }

    pub fn show_search(&self, replace: bool) {
        self.search_context.set_highlight(true);
        if let Some((start, end)) = self.buffer.selection_bounds() {
            let text = self.buffer.text(&start, &end, true);
            if !text.contains('\n') {
                self.find_entry.set_text(&text);
            }
        }
        self.search_panel.set_visible(true);
        self.replace_row.set_visible(replace);
        self.find_entry.grab_focus();
        self.find_entry.select_region(0, -1);
    }

    pub fn hide_search(&self) {
        self.search_panel.set_visible(false);
        self.search_context.set_highlight(false);
        self.view.grab_focus();
    }

    pub fn find(&self, backwards: bool) -> bool {
        if self.search_settings.search_text().is_none_or(|t| t.is_empty()) {
            self.show_search(false);
            return false;
        }
        self.search_context.set_highlight(true);
        let start = match self.buffer.selection_bounds() {
            Some((begin, end)) => if backwards { begin } else { end },
            None => self.buffer.iter_at_mark(&self.buffer.get_insert()),
        };
        let found = if backwards { self.search_context.backward(&start) } else { self.search_context.forward(&start) };
        match found {
            Some((mut begin, end, _wrapped)) => {
                self.buffer.select_range(&begin, &end);
                self.view.scroll_to_iter(&mut begin, 0.15, false, 0.0, 0.0);
                true
            }
            None => false,
        }
    }

    pub fn replace_one(&self) {
        if self.busy.get() {
            return;
        }
        let Some((mut start, mut end)) = self.buffer.selection_bounds()
            .filter(|(start, end)| self.search_context.occurrence_position(start, end) > 0) else {
            self.find(false);
            return;
        };
        match self.search_context.replace(&mut start, &mut end, &self.replace_entry.text()) {
            Ok(()) => {
                self.find(false);
            }
            Err(error) => self.message(error.message(), false),
        }
    }

    pub fn replace_all(&self) {
        if self.busy.get() || self.search_settings.search_text().is_none_or(|t| t.is_empty()) {
            return;
        }
        match crate::compat::replace_all(&self.search_context, &self.replace_entry.text()) {
            Ok(count) => self.search_count.set_text(&tr!("{count} replacements", count = count as i64)),
            Err(error) => self.message(error.message(), false),
        }
    }

    // State -------------------------------------------------------------
    pub fn text(&self) -> String {
        let (start, end) = self.buffer.bounds();
        self.buffer.text(&start, &end, true).to_string()
    }

    pub fn modified(&self) -> bool {
        self.buffer.is_modified() || self.format_modified.get()
    }

    pub fn update_status(&self) {
        let (name, folder, bom, mixed) = {
            let document = self.document.borrow();
            (document.name().unwrap_or_else(|| tr!("Untitled")), document.folder(), !document.bom.is_empty(), document.mixed)
        };
        let modified = self.modified();
        self.title_label.set_text(&format!("{}{name}", if modified { "● " } else { "" }));
        self.window.set_title(Some(&format!("{}{name} — {}", if modified { "*" } else { "" }, tr!("Text Editor"))));
        self.path_label.set_text(&folder.unwrap_or_else(|| tr!("Text Editor")));
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let selected = self.buffer.selection_bounds();
        let extra = match &selected {
            Some((start, end)) => tr!(" · {count} selected", count = (end.offset() - start.offset()) as i64),
            None => String::new(),
        };
        self.position.set_text(&(tr!("Line {line}, Column {column}", line = cursor.line() as i64 + 1,
                                     column = self.view.visual_column(&cursor) as i64 + 1) + &extra));
        self.details.set_text(&format!("{}{}  ·  {}{}  ·  {}", self.encoding.borrow().to_uppercase(), if bom { " BOM" } else { "" },
                                       self.newline.borrow(), if mixed { tr!(" (mixed)") } else { String::new() },
                                       line_count(self.buffer.line_count())));
        let busy = self.busy.get();
        let enable = |name: &str, value: bool| {
            if let Some(action) = self.simple_action(name) {
                action.set_enabled(value);
            }
        };
        enable("undo", !busy && self.buffer.can_undo());
        enable("redo", !busy && self.buffer.can_redo());
        enable("cut", !busy && selected.is_some());
        enable("copy", selected.is_some());
    }

    pub fn message(&self, message: &str, reload: bool) {
        self.banner_label.set_text(message);
        self.banner_reload.set_visible(reload);
        self.banner.set_visible(true);
    }

    /// Run `operation` on the window's worker; `done`/`failed` run on the GTK thread.
    pub fn work<T: Send + 'static>(&self, operation: impl FnOnce() -> Result<T, DocError> + Send + 'static,
                                   done: impl FnOnce(&EditorWindow, T) + 'static,
                                   failed: Option<Box<dyn FnOnce(&EditorWindow, DocError)>>) {
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.view.set_editable(false);
        for name in BUSY_ACTIONS {
            if let Some(action) = self.simple_action(name) {
                action.set_enabled(false);
            }
        }
        self.update_status();
        let receiver = self.worker.run(operation);
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.await else { return };
            let Some(inner) = weak.upgrade() else { return };
            let w = EditorWindow(inner);
            if w.closing.get() {
                return;
            }
            w.busy.set(false);
            w.view.set_editable(true);
            for name in BUSY_ACTIONS {
                if let Some(action) = w.simple_action(name) {
                    action.set_enabled(true);
                }
            }
            match result {
                Ok(value) => done(&w, value),
                Err(error) => match failed {
                    Some(failed) => failed(&w, error),
                    None => w.message(&error.to_string(), w.can_reload()),
                },
            }
            w.update_status();
        });
    }

    fn can_reload(&self) -> bool {
        let document = self.document.borrow();
        match &document.path {
            Some(path) => path.exists(),
            None => document.remote.is_some() && document.stored,
        }
    }

    pub fn load(&self, target: Target, encoding: Option<String>) {
        let job_target = target.clone();
        self.work(move || Document::load(&job_target, encoding.as_deref()), |w, document| w.set_document(document),
                  Some(Box::new(move |w, error| match error {
                      DocError::Decode(_) => w.encoding_dialog(target),
                      other => w.message(&other.to_string(), false),
                  })));
    }

    fn replace_document(&self, document: Document) {
        self.document.replace(document);
        self.document_serial.set(self.document_serial.get() + 1);
    }

    pub fn set_document(&self, document: Document) {
        let (encoding, newline, name) = (document.encoding.clone(), document.newline.clone(), document.name());
        let text = document.text.clone();
        self.replace_document(document);
        self.encoding.replace(encoding);
        self.newline.replace(newline);
        self.format_modified.set(false);
        for mark in self.bookmarks.take() {
            if !mark.is_deleted() {
                self.buffer.delete_mark(&mark);
            }
        }
        self.buffer.begin_irreversible_action();
        self.buffer.set_text(&text);
        self.buffer.end_irreversible_action();
        self.buffer.set_modified(false);
        self.buffer.place_cursor(&self.buffer.start_iter());
        let language = name.and_then(|name| sourceview5::LanguageManager::default().guess_language(Some(&name), None));
        self.set_language(language.as_ref());
        // The previous document was saved or deliberately discarded.
        self.discard_draft();
        self.banner.set_visible(false);
        self.watch_file();
        self.remember();
        self.update_status();
        self.view.grab_focus();
    }

    pub fn new_document(&self) {
        self.set_document(Document::default());
    }

    pub fn new_window(&self) -> EditorWindow {
        let window = EditorWindow::new(&self.app);
        window.present();
        window
    }

    pub fn set_default(&self) {
        let Some(info) = suite_core::glib_compat::desktop_app_info(DESKTOP_ID) else {
            self.message(&tr!("Text Editor's application entry is not installed."), false);
            return;
        };
        match info.set_as_default_for_type("text/plain") {
            Ok(()) => self.message(&tr!("Text Editor is now the default application for text files."), false),
            Err(error) => self.message(error.message(), false),
        }
    }

    // Crash-recovery drafts ---------------------------------------------
    fn cancel_draft_timer(&self) {
        if let Some(timer) = self.draft_timer.take() {
            timer.remove();
        }
    }

    fn schedule_draft(&self) {
        if self.closing.get() {
            return;
        }
        self.cancel_draft_timer();
        let weak = self.weak();
        let delay = Duration::from_millis((self.draft_delay.get() * 1000.0) as u64);
        let timer = glib::timeout_add_local_once(delay, move || {
            if let Some(inner) = weak.upgrade() {
                inner.draft_timer.replace(None);
                EditorWindow(inner).write_draft();
            }
        });
        self.draft_timer.replace(Some(timer));
    }

    fn write_draft(&self) {
        if self.closing.get() {
            return;
        }
        let identifier = self.draft_id.borrow().clone();
        if !self.buffer.is_modified() {
            if self.draft_written.get() {
                self.draft_written.set(false);
                self.app.drafts.submit(move || recovery::remove(&identifier, None));
            }
            return;
        }
        self.draft_written.set(true);
        let (text, location) = (self.text(), self.document.borrow().identifier());
        let (encoding, newline) = (self.encoding.borrow().clone(), self.newline.borrow().clone());
        self.app.drafts.submit(move || {
            let _ = recovery::write(&identifier, &text, location.as_deref(), &encoding, &newline, None);
        });
    }

    fn discard_draft(&self) {
        self.cancel_draft_timer();
        if self.draft_written.get() {
            self.draft_written.set(false);
            let identifier = self.draft_id.borrow().clone();
            self.app.drafts.submit(move || recovery::remove(&identifier, None));
        }
        self.draft_id.replace(recovery::new_identifier());
    }

    pub fn show_recovery(&self, drafts: Vec<Draft>) {
        self.recovery_label.set_text(&tr!("Recovered unsaved documents: {count} from a previous session can be restored.",
                                          count = drafts.len() as i64));
        self.recovery_drafts.replace(drafts);
        self.recovery_bar.set_visible(true);
    }

    pub fn recovery_response(&self, restore: bool) {
        let drafts = self.recovery_drafts.take();
        self.recovery_bar.set_visible(false);
        for draft in drafts {
            if !restore {
                let identifier = draft.identifier.clone();
                self.app.drafts.submit(move || recovery::remove(&identifier, None));
                continue;
            }
            let reuse = self.document.borrow().untitled() && !self.modified() && !self.busy.get()
                && !self.recovery_bar.is_visible() && !self.restoring.get();
            let window = if reuse { self.clone() } else { self.new_window() };
            window.restore_draft(draft);
        }
    }

    pub fn restore_draft(&self, draft: Draft) {
        self.restoring.set(true);
        let target = draft.location.clone().filter(|t| !t.is_empty());
        let draft = Rc::new(draft);
        let apply = {
            let draft = draft.clone();
            move |w: &EditorWindow, document: Document| {
                w.restoring.set(false);
                let (doc_text, doc_encoding, doc_newline, untitled) =
                    (document.text.clone(), document.encoding.clone(), document.newline.clone(), document.untitled());
                w.set_document(document);
                w.draft_id.replace(draft.identifier.clone());
                w.draft_written.set(true);
                if ENCODINGS.contains(&draft.encoding.as_str()) || draft.encoding == *w.encoding.borrow() {
                    w.encoding.replace(draft.encoding.clone());
                }
                if document::newline_sequence(&draft.newline).is_some() {
                    w.newline.replace(draft.newline.clone());
                }
                w.format_modified.set(*w.encoding.borrow() != doc_encoding || *w.newline.borrow() != doc_newline);
                w.buffer.begin_irreversible_action();
                w.buffer.set_text(&draft.text);
                w.buffer.end_irreversible_action();
                w.buffer.set_modified(doc_text != draft.text || untitled);
                w.update_status();
            }
        };
        let apply = Rc::new(apply);
        let missing = {
            let apply = apply.clone();
            let target = target.clone();
            move |w: &EditorWindow| match &target {
                None => apply(w, Document::default()),
                Some(target) => match document::location(&Target::Arg(target.clone())) {
                    (Some(local), _) => apply(w, Document::with_path(local)),
                    (None, file) => apply(w, Document::with_remote(file)),
                },
            }
        };
        match target {
            Some(target) => {
                let encoding = Some(draft.encoding.clone());
                let apply = apply.clone();
                self.work(move || Document::load(&Target::Arg(target), encoding.as_deref()), move |w, document| apply(w, document),
                          Some(Box::new(move |w, _| missing(w))));
            }
            None => missing(self),
        }
    }

    pub fn guard(&self, callback: Callback) {
        if self.busy.get() {
            return;
        }
        if !self.modified() {
            callback(self);
            return;
        }
        if let Some(dialog) = self.dialog.borrow().as_ref() {
            dialog.present();
            return;
        }
        let dialog = gtk::Window::builder().application(&self.app.app).transient_for(&self.window).modal(true)
            .title(tr!("Save changes?")).resizable(false).build();
        self.dialog.replace(Some(dialog.clone()));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        set_margins(&content, 22);
        let name = self.document.borrow().name().unwrap_or_else(|| tr!("Untitled"));
        content.append(&gtk::Label::builder().label(tr!("Save changes to {name}?", name = name)).wrap(true).max_width_chars(55).xalign(0.0).build());
        let row = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        content.append(&row);
        let callback = Rc::new(RefCell::new(Some(callback)));
        for (label, choice) in [("Cancel", 0), ("Discard", 1), ("Save", 2)] {
            let control = gtk::Button::with_label(&tr!(label));
            if choice == 2 {
                control.add_css_class("suggested-action");
            }
            let (weak, callback, dialog_ref) = (self.weak(), callback.clone(), dialog.downgrade());
            control.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let w = EditorWindow(inner);
                w.dialog.replace(None);
                if let Some(dialog) = dialog_ref.upgrade() {
                    dialog.destroy();
                }
                let Some(callback) = callback.borrow_mut().take() else { return };
                match choice {
                    1 => callback(&w),
                    2 => w.save(Some(callback)),
                    _ => {}
                }
            });
            row.append(&control);
        }
        let weak = self.weak();
        dialog.connect_close_request(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.dialog.replace(None);
            }
            glib::Propagation::Proceed
        });
        dialog.set_child(Some(&content));
        dialog.present();
    }

    fn close_requested(&self) -> glib::Propagation {
        if self.closing.get() {
            return glib::Propagation::Proceed;
        }
        self.guard(Box::new(|w| w.finish_close()));
        glib::Propagation::Stop
    }

    pub fn finish_close(&self) {
        // Saved or deliberately discarded: the draft is no longer needed.
        self.discard_draft();
        self.closing.set(true);
        self.window.destroy();
    }

    /// The window left the application (`destroy`): idempotent cleanup.
    pub(crate) fn destroyed(&self) {
        if self.destroyed_done.replace(true) {
            return;
        }
        if !self.closing.get() && self.modified() {
            // Destroyed without the unsaved-changes guard (application quit or
            // session end): keep the draft so the next start can restore it.
            self.cancel_draft_timer();
            let location = self.document.borrow().identifier();
            let _ = recovery::write(&self.draft_id.borrow(), &self.text(), location.as_deref(), &self.encoding.borrow(),
                                    &self.newline.borrow(), None);
        }
        self.closing.set(true);
        self.cancel_draft_timer();
        if let Some(monitor) = self.monitor.take() {
            monitor.cancel();
        }
        if let Some(pending) = self.pending_monitor.take() {
            pending.remove();
        }
        if let Some(timer) = self.style_timer.take() {
            timer.remove();
        }
        self.worker.cancel();
        self.vim.set_client_widget(None::<&gtk::Widget>);
        self.completion.unregister(&self.buffer);
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.window), &self.font_provider);
        let me = Rc::as_ptr(&self.0);
        self.app.windows.borrow_mut().retain(|w| Rc::as_ptr(&w.0) != me);
    }

    // Files -------------------------------------------------------------
    fn dialog_failed(&self, error: &glib::Error) {
        if !error.matches(gtk::DialogError::Dismissed) {
            self.message(error.message(), false);
        }
    }

    pub fn open_dialog(&self) {
        let dialog = gtk::FileDialog::builder().title(tr!("Open File")).build();
        let weak = self.weak();
        dialog.open(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let w = EditorWindow(inner);
            match result {
                Ok(file) => w.load(file.path().map(Target::Path).unwrap_or(Target::File(file)), None),
                Err(error) => w.dialog_failed(&error),
            }
        });
    }

    pub fn save(&self, after: Option<Callback>) {
        if self.document.borrow().untitled() {
            self.save_as(after);
        } else {
            self.save_to(None, after, false);
        }
    }

    pub fn save_as(&self, after: Option<Callback>) {
        let (name, parent) = {
            let document = self.document.borrow();
            let parent = if document.untitled() { None } else { document.file().and_then(|f| f.parent()) };
            (document.name().unwrap_or_else(|| "Untitled.txt".into()), parent)
        };
        let dialog = gtk::FileDialog::builder().title(tr!("Save File")).initial_name(name).build();
        if let Some(parent) = parent {
            dialog.set_initial_folder(Some(&parent));
        }
        let weak = self.weak();
        dialog.save(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let w = EditorWindow(inner);
            match result {
                Ok(file) => w.save_to(Some(file.path().map(Target::Path).unwrap_or(Target::File(file))), after, true),
                Err(error) => w.dialog_failed(&error),
            }
        });
    }

    /// Python `_save(path, after=..., overwrite=...)`.
    pub fn save_to(&self, path: Option<Target>, after: Option<Callback>, overwrite: bool) {
        let text = self.text();
        let mut document = self.document.borrow().clone();
        let (encoding, newline) = (self.encoding.borrow().clone(), self.newline.borrow().clone());
        let job_text = text.clone();
        self.work(move || {
            document.save(&job_text, path.as_ref(), overwrite, Some(&encoding), Some(&newline)).map(|_| document)
        }, move |w, document| {
            w.replace_document(document);
            w.format_modified.set(false);
            if w.text() == text {
                w.buffer.set_modified(false);
                w.discard_draft();
            }
            w.banner.set_visible(false);
            w.watch_file();
            w.remember();
            w.update_status();
            if let Some(after) = after {
                after(w);
            }
        }, None);
    }

    pub fn reload(&self) {
        if self.document.borrow().untitled() {
            return;
        }
        self.guard(Box::new(|w| {
            let (target, encoding) = {
                let document = w.document.borrow();
                let target = match (&document.path, &document.remote) {
                    (Some(path), _) => Target::Path(path.clone()),
                    (None, Some(remote)) => Target::File(remote.clone()),
                    _ => return,
                };
                (target, document.encoding.clone())
            };
            w.load(target, Some(encoding));
        }));
    }

    fn watch_file(&self) {
        if let Some(monitor) = self.monitor.take() {
            monitor.cancel();
        }
        let monitor = {
            let document = self.document.borrow();
            if document.untitled() {
                return;
            }
            match (&document.path, &document.remote) {
                (Some(path), _) => gio::File::for_path(path.parent().unwrap_or(path))
                    .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>),
                // Many GVfs backends cannot monitor; focus checks cover them.
                (None, Some(remote)) => remote.monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>),
                _ => return,
            }
        };
        let Ok(monitor) = monitor else { return };
        let weak = self.weak();
        monitor.connect_changed(move |_, file, other, _| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).file_changed(file, other);
            }
        });
        self.monitor.replace(Some(monitor));
    }

    fn file_changed(&self, file: &gio::File, other: Option<&gio::File>) {
        {
            let document = self.document.borrow();
            if document.untitled() {
                return;
            }
            if let Some(path) = &document.path {
                if file.path().as_ref() != Some(path) && other.and_then(|o| o.path()).as_ref() != Some(path) {
                    return;
                }
            }
        }
        self.schedule_external_check();
    }

    fn schedule_external_check(&self) {
        if self.pending_monitor.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let id = glib::timeout_add_local_once(Duration::from_millis(300), move || {
            if let Some(inner) = weak.upgrade() {
                inner.pending_monitor.replace(None);
                EditorWindow(inner).check_external();
            }
        });
        self.pending_monitor.replace(Some(id));
    }

    fn focus_check(&self) {
        {
            let document = self.document.borrow();
            if !self.window.is_active() || document.remote.is_none() || !document.stored {
                return;
            }
        }
        let now = glib::monotonic_time();
        if now - self.last_remote_check.get() < 5_000_000 || self.pending_monitor.borrow().is_some() {
            return;
        }
        self.last_remote_check.set(now);
        self.schedule_external_check();
    }

    pub fn check_external(&self) {
        if self.busy.get() || self.closing.get() || self.external_pending.get() {
            return;
        }
        let document = self.document.borrow().clone();
        let (etag, serial) = (document.etag.clone(), self.document_serial.get());
        self.external_pending.set(true);
        let receiver = self.worker.run(move || document.changed_on_disk());
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let result = receiver.await;
            let Some(inner) = weak.upgrade() else { return };
            let w = EditorWindow(inner);
            w.external_pending.set(false);
            if w.closing.get() || w.busy.get() || w.document_serial.get() != serial || w.document.borrow().etag != etag {
                return;
            }
            if result.unwrap_or(false) {
                w.message(&tr!("This file changed outside Text Editor. Reload it or save your changes as another file."), true);
            }
        });
    }

    fn modal(&self, title: &str) -> gtk::Window {
        gtk::Window::builder().application(&self.app.app).transient_for(&self.window).modal(true).title(title).resizable(false).build()
    }

    pub fn encoding_dialog(&self, target: Target) {
        let dialog = self.modal(&tr!("Choose File Encoding"));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        set_margins(&content, 20);
        content.append(&gtk::Label::builder().label(tr!("This file is not UTF-8. Choose its encoding to open it without losing characters."))
            .wrap(true).max_width_chars(50).xalign(0.0).build());
        let choice = gtk::DropDown::from_strings(&ENCODINGS);
        content.append(&choice);
        let row = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label(&tr!("Cancel"));
        let d = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = d.upgrade() {
                d.destroy();
            }
        });
        row.append(&cancel);
        let open = gtk::Button::with_label(&tr!("Open"));
        open.add_css_class("suggested-action");
        let (d, weak) = (dialog.downgrade(), self.weak());
        open.connect_clicked(move |_| {
            if let Some(d) = d.upgrade() {
                d.destroy();
            }
            if let Some(inner) = weak.upgrade() {
                let encoding = ENCODINGS.get(choice.selected() as usize).map(|e| e.to_string());
                EditorWindow(inner).load(target.clone(), encoding);
            }
        });
        row.append(&open);
        content.append(&row);
        dialog.set_child(Some(&content));
        dialog.present();
    }

    pub fn go_to_line(&self) {
        let dialog = self.modal(&tr!("Go to Line"));
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        set_margins(&content, 18);
        let spin = gtk::SpinButton::with_range(1.0, self.buffer.line_count() as f64, 1.0);
        spin.set_value(self.buffer.iter_at_mark(&self.buffer.get_insert()).line() as f64 + 1.0);
        content.append(&spin);
        let go = gtk::Button::with_label(&tr!("Go"));
        go.add_css_class("suggested-action");
        let jump = {
            let (weak, spin, d) = (self.weak(), spin.clone(), dialog.downgrade());
            Rc::new(move || {
                let Some(inner) = weak.upgrade() else { return };
                let w = EditorWindow(inner);
                if let Some(mut position) = w.buffer.iter_at_line(spin.value_as_int() - 1) {
                    w.buffer.place_cursor(&position);
                    w.view.scroll_to_iter(&mut position, 0.15, false, 0.0, 0.0);
                }
                if let Some(d) = d.upgrade() {
                    d.destroy();
                }
                w.view.grab_focus();
            })
        };
        let j = jump.clone();
        go.connect_clicked(move |_| j());
        spin.connect_activate(move |_| jump());
        content.append(&go);
        dialog.set_child(Some(&content));
        dialog.present();
    }

    pub fn toggle_bookmark(&self) {
        let position = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let marks = self.buffer.source_marks_at_line(position.line(), Some("bookmark"));
        if marks.is_empty() {
            self.bookmarks.borrow_mut().push(self.buffer.create_source_mark(None, "bookmark", &position));
        } else {
            for mark in marks {
                self.buffer.delete_mark(&mark);
                self.bookmarks.borrow_mut().retain(|m| *m != mark);
            }
        }
    }

    pub fn next_bookmark(&self, backwards: bool) {
        self.bookmarks.borrow_mut().retain(|mark| !mark.is_deleted());
        let mut offsets: Vec<i32> = self.bookmarks.borrow().iter().map(|mark| self.buffer.iter_at_mark(mark).offset()).collect();
        offsets.sort_unstable();
        offsets.dedup();
        let (Some(&first), Some(&last)) = (offsets.first(), offsets.last()) else { return };
        let current = self.buffer.iter_at_mark(&self.buffer.get_insert()).offset();
        let offset = if backwards {
            offsets.iter().rev().find(|o| **o < current).copied().unwrap_or(last)
        } else {
            offsets.iter().find(|o| **o > current).copied().unwrap_or(first)
        };
        let mut position = self.buffer.iter_at_offset(offset);
        self.buffer.place_cursor(&position);
        self.view.scroll_to_iter(&mut position, 0.15, false, 0.0, 0.0);
    }

    fn toggle_option(&self, action: &gio::SimpleAction, key: &str) {
        let value = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
        {
            let mut options = self.options.borrow_mut();
            match key {
                "wrap" => options.wrap = value,
                "line_numbers" => options.line_numbers = value,
                _ => options.whitespace = value,
            }
        }
        action.set_state(&value.to_variant());
        self.apply_options(true);
    }

    pub fn apply_options(&self, save: bool) {
        let options = self.options.borrow().clone();
        self.view.set_wrap_mode(if options.wrap { gtk::WrapMode::WordChar } else { gtk::WrapMode::None });
        self.view.set_show_line_numbers(options.line_numbers);
        self.view.set_auto_indent(options.auto_indent);
        self.view.set_insert_spaces_instead_of_tabs(options.spaces);
        self.view.set_tab_width(options.tab_width as u32);
        let drawer = self.view.space_drawer();
        drawer.set_types_for_locations(sourceview5::SpaceLocationFlags::ALL, sourceview5::SpaceTypeFlags::ALL);
        drawer.set_enable_matrix(options.whitespace);
        self.vim.set_client_widget(if options.vim { Some(self.view.upcast_ref::<gtk::Widget>()) } else { None });
        self.vim_controller.set_propagation_phase(if options.vim { gtk::PropagationPhase::Capture } else { gtk::PropagationPhase::None });
        self.vim_label.set_visible(options.vim);
        self.vim_command_status();
        self.sync_style();
        self.update_view_css();
        if save {
            let mut preferences = self.app.preferences.borrow_mut();
            preferences.options = options;
            preferences.save();
        }
    }

    pub fn sync_style(&self) {
        let mut dark = false;
        if let Some(settings) = gtk::Settings::default() {
            #[allow(deprecated)]
            let prefer = settings.is_gtk_application_prefer_dark_theme();
            dark = prefer
                || settings.gtk_theme_name().is_some_and(|name| suite_core::pyjson::casefold(&name).contains("dark"));
        }
        let mut scheme = if dark { "Adwaita-dark" } else { "Adwaita" };
        // This flag follows the system preference; the palette's actual
        // foreground, background and accent colors are not used here.
        if let Some(palette) = self.app.palette.borrow().as_ref().and_then(|client| client.palette()) {
            scheme = if palette.is_dark { "Adwaita-dark" } else { "Adwaita" };
        }
        if self.style_name.borrow().as_deref() != Some(scheme) {
            let manager = sourceview5::StyleSchemeManager::default();
            let chosen = manager.scheme(scheme).or_else(|| manager.scheme("classic"));
            self.buffer.set_style_scheme(chosen.as_ref());
            self.style_name.replace(Some(scheme.to_owned()));
            self.update_view_css();
        }
    }

    fn update_view_css(&self) {
        let mut css = font_css(&self.font_scope, &self.options.borrow().font);
        css += &editor_surface_css(&self.font_scope, self.buffer.style_scheme().as_ref(),
                                   self.style_name.borrow().as_deref() == Some("Adwaita-dark"));
        self.font_provider.load_from_string(&css);
    }

    pub fn zoom(&self, amount: i32) {
        let font = style::zoomed_font(&self.options.borrow().font, amount);
        self.options.borrow_mut().font = font;
        self.apply_options(true);
    }

    pub fn set_language(&self, language: Option<&sourceview5::Language>) {
        self.buffer.set_language(language);
        self.language_label.set_text(&language.map(|l| l.name().to_string()).unwrap_or_else(|| tr!("Plain Text")));
    }

    pub fn preferences_dialog(&self) {
        let dialog = gtk::Window::builder().application(&self.app.app).transient_for(&self.window).title(tr!("Editor Preferences"))
            .modal(true).default_width(480).resizable(false).build();
        dialog.set_titlebar(Some(&headerbar(&tr!("Editor Preferences"))));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        set_margins(&content, 20);
        let row = |label: &str, control: &gtk::Widget| {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            line.append(&gtk::Label::builder().label(tr!(label)).xalign(0.0).hexpand(true).build());
            line.append(control);
            content.append(&line);
        };
        let font = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
        font.set_font_desc(&pango::FontDescription::from_string(&self.options.borrow().font));
        let weak = self.weak();
        font.connect_font_desc_notify(move |font| {
            let (Some(inner), Some(desc)) = (weak.upgrade(), font.font_desc()) else { return };
            let w = EditorWindow(inner);
            w.options.borrow_mut().font = desc.to_str().to_string();
            w.apply_options(true);
        });
        row("Font", font.upcast_ref());
        for (key, label) in [("auto_indent", "Automatic indentation"), ("spaces", "Insert spaces instead of tabs"), ("vim", "Vim editing mode")] {
            let active = match key {
                "auto_indent" => self.options.borrow().auto_indent,
                "spaces" => self.options.borrow().spaces,
                _ => self.options.borrow().vim,
            };
            let switch = gtk::Switch::builder().active(active).valign(gtk::Align::Center).build();
            let weak = self.weak();
            switch.connect_active_notify(move |switch| {
                let Some(inner) = weak.upgrade() else { return };
                let w = EditorWindow(inner);
                {
                    let mut options = w.options.borrow_mut();
                    let value = switch.is_active();
                    match key {
                        "auto_indent" => options.auto_indent = value,
                        "spaces" => options.spaces = value,
                        _ => options.vim = value,
                    }
                }
                w.apply_options(true);
            });
            row(label, switch.upcast_ref());
        }
        let width = gtk::SpinButton::with_range(1.0, 16.0, 1.0);
        width.set_value(self.options.borrow().tab_width as f64);
        let weak = self.weak();
        width.connect_value_changed(move |spin| {
            let Some(inner) = weak.upgrade() else { return };
            let w = EditorWindow(inner);
            w.options.borrow_mut().tab_width = spin.value_as_int() as i64;
            w.apply_options(true);
        });
        row("Indent width", width.upcast_ref());
        let current = self.encoding.borrow().clone();
        let mut encodings = vec![current.clone()];
        encodings.extend(ENCODINGS.iter().map(|e| e.to_string()).filter(|e| *e != current));
        let names: Vec<&str> = encodings.iter().map(String::as_str).collect();
        let encoding = gtk::DropDown::from_strings(&names);
        encoding.set_selected(0);
        let weak = self.weak();
        let choices = encodings.clone();
        encoding.connect_selected_notify(move |drop| {
            if let (Some(inner), Some(choice)) = (weak.upgrade(), choices.get(drop.selected() as usize)) {
                EditorWindow(inner).format(Some(choice), None);
            }
        });
        row("Save encoding", encoding.upcast_ref());
        let endings: Vec<&str> = NEWLINES.iter().map(|(k, _)| *k).collect();
        let ending = gtk::DropDown::from_strings(&endings);
        ending.set_selected(endings.iter().position(|e| *e == *self.newline.borrow()).unwrap_or(0) as u32);
        let weak = self.weak();
        ending.connect_selected_notify(move |drop| {
            if let (Some(inner), Some(choice)) = (weak.upgrade(), NEWLINES.get(drop.selected() as usize)) {
                EditorWindow(inner).format(None, Some(choice.0));
            }
        });
        row("Line endings", ending.upcast_ref());
        let manager = sourceview5::LanguageManager::default();
        let mut languages: Vec<sourceview5::Language> = manager.language_ids().iter().filter_map(|id| manager.language(id)).collect();
        languages.sort_by_key(|lang| suite_core::pyjson::casefold(&lang.name()));
        let mut labels = vec![tr!("Plain Text")];
        labels.extend(languages.iter().map(|lang| lang.name().to_string()));
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let language = gtk::DropDown::from_strings(&label_refs);
        language.set_enable_search(true);
        let current = self.buffer.language();
        let selected = current.and_then(|c| languages.iter().position(|l| *l == c)).map(|p| p + 1).unwrap_or(0);
        language.set_selected(selected as u32);
        let weak = self.weak();
        language.connect_selected_notify(move |drop| {
            let Some(inner) = weak.upgrade() else { return };
            let index = drop.selected() as usize;
            EditorWindow(inner).set_language(if index > 0 { languages.get(index - 1) } else { None });
        });
        row("Syntax highlighting", language.upcast_ref());
        let explanation = gtk::Label::builder()
            .label(tr!("Encoding and line endings apply to this document when saved. Other preferences apply to new editor windows."))
            .wrap(true).max_width_chars(58).xalign(0.0).build();
        explanation.add_css_class("muted");
        content.append(&explanation);
        dialog.set_child(Some(&content));
        dialog.present();
    }

    fn vim_command_status(&self) {
        // The generated getters assume non-NULL strings; the properties are nullable.
        let parts: Vec<String> = ["command-bar-text", "command-text"].into_iter()
            .filter_map(|name| self.vim.property::<Option<String>>(name)).filter(|s| !s.is_empty()).collect();
        let text = parts.join(" ");
        self.vim_label.set_text(if text.is_empty() { "Vim" } else { &text });
    }

    pub fn vim_command(&self, command: &str) -> bool {
        let command = suite_core::pyjson::py_strip(command.trim_start_matches(':'));
        let (name, path) = match command.find(' ') {
            Some(index) => (&command[..index], &command[index + 1..]),
            None => (command, ""),
        };
        let expand = |path: &str| PathBuf::from(crate::compat::expanduser_path(path));
        match name {
            "w" | "write" | "wq" | "x" => {
                let after: Option<Callback> = if matches!(name, "wq" | "x") { Some(Box::new(|w| w.finish_close())) } else { None };
                if path.is_empty() {
                    self.save(after);
                } else {
                    self.save_to(Some(Target::Path(expand(path))), after, false);
                }
                true
            }
            "q" | "quit" => {
                self.window.close();
                true
            }
            "q!" | "quit!" => {
                if !self.busy.get() {
                    self.finish_close();
                }
                true
            }
            "e" | "edit" => {
                if path.is_empty() {
                    self.reload();
                } else {
                    let target = std::path::absolute(expand(path)).unwrap_or_else(|_| expand(path));
                    self.guard(Box::new(move |w| {
                        if target.exists() {
                            w.load(Target::Path(target), None);
                        } else {
                            w.set_document(Document::with_path(target));
                        }
                    }));
                }
                true
            }
            _ => false,
        }
    }

    pub fn format(&self, encoding: Option<&str>, newline: Option<&str>) {
        if let Some(encoding) = encoding.filter(|e| !e.is_empty()) {
            self.encoding.replace(encoding.to_owned());
        }
        if let Some(newline) = newline.filter(|n| !n.is_empty()) {
            self.newline.replace(newline.to_owned());
        }
        let document = self.document.borrow();
        self.format_modified.set(*self.encoding.borrow() != document.encoding || *self.newline.borrow() != document.newline);
        drop(document);
        self.update_status();
    }

    pub fn print_document(&self, output_path: Option<PathBuf>) {
        let compositor = sourceview5::PrintCompositor::from_view(&self.view);
        compositor.set_wrap_mode(gtk::WrapMode::WordChar);
        compositor.set_print_line_numbers(if self.options.borrow().line_numbers { 1 } else { 0 });
        compositor.set_print_header(true);
        let name = self.document.borrow().name().unwrap_or_else(|| tr!("Untitled"));
        compositor.set_header_format(false, Some(&name), None, Some("%N / %Q"));
        let operation = gtk::PrintOperation::new();
        self.print_operation.replace(Some(operation.clone()));
        operation.set_job_name(&name);
        operation.set_allow_async(true);
        if let Some(path) = &output_path {
            operation.set_export_filename(path);
        }
        let c = compositor.clone();
        operation.connect_paginate(move |operation, context| {
            if c.paginate(context) {
                operation.set_n_pages(c.n_pages());
                return true;
            }
            false
        });
        operation.connect_draw_page(move |_, context, page| compositor.draw_page(context, page));
        let weak = self.weak();
        operation.connect_done(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                inner.print_operation.replace(None);
            }
        });
        let action = if output_path.is_some() { gtk::PrintOperationAction::Export } else { gtk::PrintOperationAction::PrintDialog };
        if let Err(error) = operation.run(action, Some(&self.window)) {
            self.message(error.message(), false);
        }
    }
}
