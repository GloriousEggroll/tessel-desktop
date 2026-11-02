//! `EditorApplication` (editor_ui.py): a unique GtkApplication with HANDLES_OPEN.

use crate::preferences::Preferences;
use crate::recovery::{self, Draft};
use crate::style;
use crate::window::EditorWindow;
use crate::worker::Worker;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_gtk::theme::PaletteClient;

pub const APP_ID: &str = "org.tessel.TextEditor";
pub const DESKTOP_ID: &str = "org.tessel.TextEditor.desktop";

pub struct AppInner {
    pub app: gtk::Application,
    pub preferences: RefCell<Preferences>,
    /// One ordered worker keeps draft writes and removals in sequence.
    pub drafts: Worker,
    pub recovered: RefCell<Option<Vec<Draft>>>,
    pub recovery_offered: Cell<bool>,
    pub windows: RefCell<Vec<EditorWindow>>,
    /// The palette watcher shared by all windows (Python `companion_theme._client`).
    pub palette: RefCell<Option<PaletteClient>>,
}

#[derive(Clone)]
pub struct EditorApplication(pub Rc<AppInner>);

impl std::ops::Deref for EditorApplication {
    type Target = AppInner;
    fn deref(&self) -> &AppInner { &self.0 }
}

impl EditorApplication {
    pub fn new() -> Self {
        let app = gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
        let this = Self(Rc::new(AppInner {
            app, preferences: RefCell::new(Preferences::load()), drafts: Worker::new("text-editor-drafts"),
            recovered: RefCell::new(None), recovery_offered: Cell::new(false), windows: RefCell::new(Vec::new()),
            palette: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&this.0);
        this.app.connect_startup(move |_| {
            if let Some(inner) = weak.upgrade() {
                EditorApplication(inner).startup();
            }
        });
        let weak = Rc::downgrade(&this.0);
        this.app.connect_activate(move |_| {
            // Each launch without a file is a fresh scratch document, never a tab.
            if let Some(inner) = weak.upgrade() {
                EditorWindow::new(&EditorApplication(inner)).present();
            }
        });
        let weak = Rc::downgrade(&this.0);
        this.app.connect_open(move |_, files, _hint| {
            if let Some(inner) = weak.upgrade() {
                EditorApplication(inner).open_files(files);
            }
        });
        // GTK 4 emits "destroy" only at dispose, which our own references
        // delay; gtk_window_destroy() always removes the window from the
        // application, so the editor's destroy handling runs from here.
        let weak = Rc::downgrade(&this.0);
        this.app.connect_window_removed(move |_, window| {
            let Some(inner) = weak.upgrade() else { return };
            if let Some(editor) = EditorApplication(inner).find_window(window) {
                editor.destroyed();
            }
        });
        let weak = Rc::downgrade(&this.0);
        this.app.connect_shutdown(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.drafts.join();
            }
        });
        this
    }

    pub fn downgrade(&self) -> Weak<AppInner> {
        Rc::downgrade(&self.0)
    }

    fn startup(&self) {
        // Let the compositor resolve the desktop entry's icon (no default icon set).
        tessel_gtk::utils::add_css(style::CSS, None, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 31);
        let client = PaletteClient::new(None);
        crate::compat::attach_application(&self.app, &client);
        self.palette.replace(Some(client));
        for (name, accels) in style::ACCELERATORS {
            self.app.set_accels_for_action(&format!("win.{name}"), accels);
        }
        // Drafts left by a crashed session are read off the GTK thread.
        let receiver = self.drafts.run(|| recovery::pending(None));
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let found = receiver.await.unwrap_or_default();
            if let Some(inner) = weak.upgrade() {
                inner.recovered.replace(Some(found));
                EditorApplication(inner).offer_recovery(None);
            }
        });
    }

    pub fn editor_windows(&self) -> Vec<EditorWindow> {
        self.windows.borrow().clone()
    }

    pub fn find_window(&self, window: &gtk::Window) -> Option<EditorWindow> {
        self.windows.borrow().iter().find(|w| w.window.upcast_ref::<gtk::Window>() == window).cloned()
    }

    pub fn offer_recovery(&self, window: Option<&EditorWindow>) {
        if self.recovery_offered.get() || self.recovered.borrow().as_ref().is_none_or(Vec::is_empty) {
            return;
        }
        let window = window.cloned()
            .or_else(|| self.app.active_window().and_then(|w| self.find_window(&w)))
            .or_else(|| self.windows.borrow().first().cloned());
        let Some(window) = window else { return };
        self.recovery_offered.set(true);
        let drafts = self.recovered.borrow().clone().unwrap_or_default();
        window.show_recovery(drafts);
    }

    /// `do_open`: existing documents are focused, the others get their own windows.
    pub fn open_files(&self, files: &[gio::File]) {
        for file in files {
            let existing = self.windows.borrow().iter().find(|w| w.document.borrow().same_location(file)).cloned();
            if let Some(existing) = existing {
                existing.present();
                continue;
            }
            let window = EditorWindow::new(self);
            window.present();
            match file.path() {
                // GVfs locations (SFTP, SMB, WebDAV...) load through GIO.
                None => window.load(file.clone().into(), None),
                Some(path) if path.exists() => window.load(path.into(), None),
                Some(path) => {
                    window.document.borrow_mut().path = Some(std::path::absolute(&path).unwrap_or(path));
                    window.update_status();
                }
            }
        }
    }

    pub fn run(&self, args: &[String]) -> glib::ExitCode {
        self.app.run_with_args(args)
    }
}

impl Default for EditorApplication {
    fn default() -> Self { Self::new() }
}
