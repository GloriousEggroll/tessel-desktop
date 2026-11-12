//! Theme & Wallpaper → Pointer: the pointer (Xcursor) theme, chosen from
//! the installed themes ([`tessel_core::cursor_theme::discover`]) and its
//! size. A theme is written to gsettings `org.gnome.desktop.interface
//! cursor-theme`, `gtk-cursor-theme-name` in both GTK settings.ini files,
//! `XCURSOR_THEME` in labwc's environment (then `labwc --reconfigure`),
//! `icons/default/index.theme` and `~/.config/tessel/cursor-theme`. The size
//! uses the Accessibility page's backend.
//! Adwaita is selected when nothing installed is configured.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gs_system::accessibility_page::{AccessibilityBackend, CURSOR_SIZES};
use gs_system::labwc_settings::{self, Reconfigure};
use gtk::prelude::*;
use gtk::gdk;
use tessel_core::cursor_theme::{self as cursors, CursorThemeInfo, DEFAULT};
use tessel_core::tr;

use crate::theme::{ColorSettings, GioColorSettings};
use crate::util::{accessible_label, label};

/// Nominal preview size (logical pixels).
pub const PREVIEW_SIZE: u32 = 32;

/// What applying a change did: messages of the steps that failed, and
/// whether the compositor failed to reload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub errors: Vec<String>,
    pub reload_failed: bool,
}

pub type AppliedDone = Box<dyn FnOnce(Applied)>;

/// Where the choice is stored (tests use a fake).
pub trait CursorThemeSettings {
    /// The configured theme id (gsettings, else the GTK 4 settings.ini).
    fn current(&self) -> Option<String>;
    /// Apply a theme; every step runs even when another fails.
    fn set(&self, theme: &str, done: AppliedDone);
    /// The configured pointer size in pixels.
    fn size(&self) -> i32;
    fn set_size(&self, size: i32, done: AppliedDone);
    fn connect_changed(&self, handler: Rc<dyn Fn()>) -> u64;
    fn disconnect(&self, id: u64);
}

/// The files the system backend writes.
#[derive(Clone, Debug)]
pub struct CursorPaths {
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub home: PathBuf,
    /// labwc's environment file (`None`: the running session's).
    pub environment: Option<PathBuf>,
}

impl CursorPaths {
    pub fn from_session() -> Self {
        Self { config_home: tessel_core::paths::config_home(), data_home: tessel_core::paths::data_home(),
               home: tessel_core::paths::home(), environment: None }
    }
}

/// gsettings (when the schema is installed), both GTK settings.ini files,
/// labwc's environment, the `default` theme and the recorded choice.
pub struct SystemCursorThemeSettings {
    gsettings: Option<Rc<dyn ColorSettings>>,
    /// The same settings for `cursor-size` (an integer key).
    interface: Option<gio::Settings>,
    paths: CursorPaths,
    reconfigure: Reconfigure,
    handlers: RefCell<Vec<(u64, Vec<u64>)>>,
    next: Cell<u64>,
}

impl SystemCursorThemeSettings {
    pub fn new(gsettings: Option<Rc<dyn ColorSettings>>, interface: Option<gio::Settings>, paths: CursorPaths,
               reconfigure: Reconfigure) -> Self {
        Self { gsettings, interface, paths, reconfigure, handlers: RefCell::new(Vec::new()), next: Cell::new(1) }
    }

    pub fn from_session() -> Self {
        let interface = cursors::interface_settings();
        let gsettings = interface.clone().map(|s| Rc::new(GioColorSettings::new(s)) as Rc<dyn ColorSettings>);
        Self::new(gsettings, interface, CursorPaths::from_session(), labwc_settings::default_reconfigure())
    }

    /// Reload labwc when its environment changed, then report.
    fn finish(&self, mut applied: Applied, reload: bool, done: AppliedDone) {
        if !reload {
            done(applied);
            return;
        }
        (self.reconfigure)(Some(Box::new(move |okay, _output| {
            applied.reload_failed = !okay;
            done(applied);
        })));
    }
}

impl CursorThemeSettings for SystemCursorThemeSettings {
    fn current(&self) -> Option<String> {
        self.gsettings.as_ref().map(|s| s.get_string(cursors::GSETTINGS_KEY)).filter(|v| !v.is_empty())
            .or_else(|| cursors::read_gtk_ini(&tessel_core::icon_theme::gtk_ini_paths(&self.paths.config_home)[1]))
    }

    fn set(&self, theme: &str, done: AppliedDone) {
        let mut applied = Applied::default();
        if !cursors::valid_id(theme) {
            applied.errors.push(format!("Invalid pointer theme: {theme}"));
            done(applied);
            return;
        }
        if let Some(settings) = &self.gsettings {
            if settings.get_string(cursors::GSETTINGS_KEY) != theme && !settings.set_string(cursors::GSETTINGS_KEY, theme) {
                applied.errors.push(tr!("The shared pointer theme setting could not be changed."));
            }
        }
        if let Err(error) = cursors::write_gtk_inis(&self.paths.config_home, theme) {
            applied.errors.push(error);
        }
        // labwc reads XCURSOR_THEME from its environment on --reconfigure.
        let values = labwc_settings::env_values(&[(cursors::ENV_KEY, Some(theme))]);
        let reload = match labwc_settings::update_environment(&values, self.paths.environment.as_deref()) {
            Ok(changed) => changed,
            Err(error) => {
                applied.errors.push(error.to_string());
                false
            }
        };
        // XWayland and toolkits that only read the `default` theme.
        if let Err(error) = cursors::write_default_indexes(&self.paths.data_home, &self.paths.home, theme) {
            applied.errors.push(error);
        }
        if let Err(error) = cursors::write_choice(&self.paths.config_home, theme) {
            applied.errors.push(error);
        }
        self.finish(applied, reload, done);
    }

    fn size(&self) -> i32 {
        if let Some(interface) = &self.interface {
            return interface.int(cursors::GSETTINGS_SIZE_KEY);
        }
        labwc_settings::read_environment(self.paths.environment.as_deref()).ok()
            .and_then(|values| values.get("XCURSOR_SIZE").and_then(|v| v.parse().ok())).unwrap_or(CURSOR_SIZES[0])
    }

    fn set_size(&self, size: i32, done: AppliedDone) {
        let backend = AccessibilityBackend { interface: self.interface.clone(), a11y: None, applications: None,
                                             environment: self.paths.environment.clone(),
                                             runner: gs_system::about_page::default_runner(), orca: Some(false) };
        match backend.set_cursor_size(size) {
            Ok(changed) => self.finish(Applied::default(), changed, done),
            Err(error) => done(Applied { errors: vec![error.to_string()], reload_failed: false }),
        }
    }

    fn connect_changed(&self, handler: Rc<dyn Fn()>) -> u64 {
        let Some(settings) = &self.gsettings else { return 0 };
        let ids = [cursors::GSETTINGS_KEY, cursors::GSETTINGS_SIZE_KEY].iter()
            .map(|key| settings.connect_changed(key, handler.clone())).collect();
        let id = self.next.replace(self.next.get() + 1);
        self.handlers.borrow_mut().push((id, ids));
        id
    }

    fn disconnect(&self, id: u64) {
        let Some(settings) = &self.gsettings else { return };
        let mut handlers = self.handlers.borrow_mut();
        if let Some(index) = handlers.iter().position(|(n, _)| *n == id) {
            for inner in handlers.remove(index).1 {
                settings.disconnect(inner);
            }
        }
    }
}

/// Dropdown captions: `Name=`, with the id added when two names collide.
pub fn captions(themes: &[CursorThemeInfo]) -> Vec<String> {
    themes.iter().map(|theme| {
        if themes.iter().filter(|t| t.name == theme.name).count() > 1 && theme.name != theme.id {
            format!("{} ({})", theme.name, theme.id)
        } else {
            theme.name.clone()
        }
    }).collect()
}

/// The row to select: the configured theme, else Adwaita, else the first.
pub fn selected_index(themes: &[CursorThemeInfo], current: Option<&str>) -> Option<usize> {
    current.and_then(|id| themes.iter().position(|t| t.id == id))
        .or_else(|| themes.iter().position(|t| t.id == DEFAULT))
        .or(if themes.is_empty() { None } else { Some(0) })
}

/// The size row closest to `size` (the first of equal distances).
pub fn size_index(size: i32) -> usize {
    let mut best = 0;
    for (index, value) in CURSOR_SIZES.iter().enumerate() {
        if (value - size).abs() < (CURSOR_SIZES[best] - size).abs() {
            best = index;
        }
    }
    best
}

/// A cursor image as a texture (premultiplied ARGB32 little endian = BGRA bytes).
pub fn texture(image: &cursors::CursorImage) -> gdk::MemoryTexture {
    gdk::MemoryTexture::new(image.width as i32, image.height as i32, gdk::MemoryFormat::B8g8r8a8Premultiplied,
                            &glib::Bytes::from(&image.pixels[..]), image.width as usize * 4)
}

/// The Pointer card of the Theme & Wallpaper page.
pub struct CursorThemeChooser {
    pub card: gtk::Box,
    pub dropdown: gtk::DropDown,
    pub size: gtk::DropDown,
    pub samples: gtk::Box,
    pub status: gtk::Label,
    pub themes: Vec<CursorThemeInfo>,
    dirs: Vec<PathBuf>,
    settings: Rc<dyn CursorThemeSettings>,
    handler: Cell<u64>,
    updating: Cell<bool>,
    closed: Cell<bool>,
    sample_key: RefCell<Option<(String, i32)>>,
}

impl CursorThemeChooser {
    pub fn new(settings: Rc<dyn CursorThemeSettings>, dirs: Vec<PathBuf>) -> Rc<Self> {
        let themes = cursors::discover(&dirs);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("gs-card");
        card.add_css_class("cursor-theme-card");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        let title = tr!("Pointer");
        text.append(&label(&title, None));
        text.append(&label(&tr!("Choose the pointer theme used by the desktop and applications."), Some("subtitle")));
        row.append(&text);
        let names = captions(&themes);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let dropdown = gtk::DropDown::from_strings(&refs);
        dropdown.set_valign(gtk::Align::Center);
        if themes.len() > 8 {
            // Typing filters by name.
            dropdown.set_expression(Some(&gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")));
            dropdown.set_enable_search(true);
        }
        accessible_label(&dropdown, &title);
        row.append(&dropdown);
        card.append(&row);
        let samples = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(16).build();
        samples.add_css_class("cursor-theme-samples");
        card.append(&samples);
        let size_row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let size_label = label(&tr!("Size"), None);
        size_label.set_hexpand(true);
        size_row.append(&size_label);
        let sizes: Vec<String> = CURSOR_SIZES.iter().map(|v| tr!("{value1} pixels", value1 = *v as i64)).collect();
        let refs: Vec<&str> = sizes.iter().map(String::as_str).collect();
        let size = gtk::DropDown::from_strings(&refs);
        size.set_valign(gtk::Align::Center);
        accessible_label(&size, &tr!("Cursor size"));
        size_row.append(&size);
        card.append(&size_row);
        let status = label("", Some("subtitle"));
        status.set_visible(false);
        card.append(&status);
        if themes.is_empty() {
            dropdown.set_sensitive(false);
            samples.set_visible(false);
            status.set_text(&tr!("No pointer themes are installed."));
            status.set_visible(true);
        }
        let chooser = Rc::new(Self {
            card, dropdown, size, samples, status, themes, dirs, settings, handler: Cell::new(0), updating: Cell::new(false),
            closed: Cell::new(false), sample_key: RefCell::new(None),
        });
        chooser.sync();
        let weak = Rc::downgrade(&chooser);
        chooser.dropdown.connect_selected_notify(move |_| {
            if let Some(chooser) = weak.upgrade() {
                chooser.selected();
            }
        });
        let weak = Rc::downgrade(&chooser);
        chooser.size.connect_selected_notify(move |_| {
            if let Some(chooser) = weak.upgrade() {
                chooser.size_selected();
            }
        });
        let weak = Rc::downgrade(&chooser);
        chooser.samples.connect_scale_factor_notify(move |_| {
            if let Some(chooser) = weak.upgrade() {
                chooser.show_samples();
            }
        });
        let weak = Rc::downgrade(&chooser);
        let id = chooser.settings.connect_changed(Rc::new(move || {
            if let Some(chooser) = weak.upgrade() {
                chooser.sync();
            }
        }));
        chooser.handler.set(id);
        chooser
    }

    /// The theme id of the dropdown's selection.
    pub fn selected_id(&self) -> Option<String> {
        self.themes.get(self.dropdown.selected() as usize).map(|t| t.id.clone())
    }

    /// The size of the size dropdown's selection.
    pub fn selected_size(&self) -> i32 {
        CURSOR_SIZES.get(self.size.selected() as usize).copied().unwrap_or(CURSOR_SIZES[0])
    }

    /// Select the configured theme (else Adwaita) and size without saving.
    pub fn sync(&self) {
        if self.closed.get() {
            return;
        }
        self.updating.set(true);
        if let Some(index) = selected_index(&self.themes, self.settings.current().as_deref()) {
            self.dropdown.set_selected(index as u32);
        }
        self.size.set_selected(size_index(self.settings.size()) as u32);
        self.updating.set(false);
        self.show_samples();
    }

    fn selected(self: &Rc<Self>) {
        self.show_samples();
        if self.updating.get() || self.closed.get() {
            return;
        }
        let Some(theme) = self.selected_id() else { return };
        if self.settings.current().as_deref() == Some(theme.as_str()) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.settings.set(&theme, Box::new(move |applied| {
            if let Some(chooser) = weak.upgrade() {
                chooser.report(applied, |error| tr!("The pointer theme could not be changed: {error}", error = error));
            }
        }));
    }

    fn size_selected(self: &Rc<Self>) {
        if self.updating.get() || self.closed.get() {
            return;
        }
        let size = self.selected_size();
        if self.settings.size() == size {
            return;
        }
        let weak = Rc::downgrade(self);
        self.settings.set_size(size, Box::new(move |applied| {
            if let Some(chooser) = weak.upgrade() {
                chooser.report(applied, |error| tr!("The setting could not be changed: {value1}", value1 = error));
            }
        }));
    }

    fn report(&self, applied: Applied, failed: impl Fn(&str) -> String) {
        if self.closed.get() {
            return;
        }
        let mut messages = Vec::new();
        if !applied.errors.is_empty() {
            messages.push(failed(&applied.errors.join(" ")));
        }
        if applied.reload_failed {
            messages.push(tr!("Saved, but the desktop could not reload its configuration."));
        }
        self.status.set_text(&messages.join(" "));
        self.status.set_visible(!messages.is_empty());
        if !applied.errors.is_empty() {
            self.sync();
        }
    }

    /// The selected theme's own cursors (not the app's).
    fn show_samples(&self) {
        let Some(theme) = self.selected_id() else { return };
        let scale = self.samples.scale_factor().max(1);
        let key = (theme.clone(), scale);
        if self.sample_key.borrow().as_ref() == Some(&key) {
            return;
        }
        while let Some(child) = self.samples.first_child() {
            self.samples.remove(&child);
        }
        for (name, image) in cursors::sample_images(&self.dirs, &theme, PREVIEW_SIZE * scale as u32) {
            let picture = gtk::Image::from_paintable(Some(&texture(&image)));
            picture.set_pixel_size((image.width.max(image.height) as i32 + scale - 1) / scale);
            picture.set_widget_name(name);
            self.samples.append(&picture);
        }
        self.sample_key.replace(Some(key));
    }

    /// The cursor names shown in the preview (tests).
    pub fn sample_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut child = self.samples.first_child();
        while let Some(widget) = child {
            names.push(widget.widget_name().to_string());
            child = widget.next_sibling();
        }
        names
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        let id = self.handler.replace(0);
        if id != 0 {
            self.settings.disconnect(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn info(id: &str, name: &str) -> CursorThemeInfo {
        CursorThemeInfo { id: id.into(), name: name.into(), dir: PathBuf::from("/x").join(id) }
    }

    #[test]
    fn selection_prefers_the_configured_theme_then_adwaita() {
        let themes = [info("Bibata", "Bibata"), info("Adwaita", "Adwaita"), info("breeze_cursors", "Breeze")];
        assert_eq!(selected_index(&themes, Some("breeze_cursors")), Some(2));
        assert_eq!(selected_index(&themes, Some("missing")), Some(1));
        assert_eq!(selected_index(&themes, None), Some(1));
        assert_eq!(selected_index(&themes[..1], Some("missing")), Some(0));
        assert_eq!(selected_index(&[], None), None);
        assert_eq!(captions(&[info("a", "Same"), info("b", "Same"), info("c", "C")]), ["Same (a)", "Same (b)", "C"]);
        assert_eq!([size_index(24), size_index(30), size_index(40), size_index(200), size_index(0)], [0, 1, 1, 4, 0]);
    }

    struct FakeGsettings(RefCell<String>, Cell<bool>, RefCell<Vec<String>>);

    impl ColorSettings for FakeGsettings {
        fn get_string(&self, _key: &str) -> String { self.0.borrow().clone() }
        fn set_string(&self, key: &str, value: &str) -> bool {
            assert_eq!(key, "cursor-theme");
            self.2.borrow_mut().push(value.into());
            if self.1.get() {
                self.0.replace(value.into());
            }
            self.1.get()
        }
        fn connect_changed(&self, _key: &str, _handler: Rc<dyn Fn()>) -> u64 { 1 }
        fn disconnect(&self, _id: u64) {}
    }

    struct Fixture {
        dir: PathBuf,
        gsettings: Rc<FakeGsettings>,
        reloads: Rc<RefCell<Vec<bool>>>,
        reload_ok: Rc<Cell<bool>>,
        backend: SystemCursorThemeSettings,
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("gs-cursor-themes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config/gtk-3.0")).unwrap();
        std::fs::create_dir_all(dir.join("config/labwc")).unwrap();
        let gsettings = Rc::new(FakeGsettings(RefCell::new("Adwaita".into()), Cell::new(true), RefCell::default()));
        let reloads = Rc::new(RefCell::new(Vec::new()));
        let reload_ok = Rc::new(Cell::new(true));
        let (seen, ok) = (reloads.clone(), reload_ok.clone());
        let reconfigure: Reconfigure = Rc::new(move |done| {
            seen.borrow_mut().push(ok.get());
            if let Some(done) = done {
                done(ok.get(), String::new());
            }
        });
        let paths = CursorPaths { config_home: dir.join("config"), data_home: dir.join("home/.local/share"), home: dir.join("home"),
                                  environment: Some(dir.join("config/labwc/environment")) };
        let backend = SystemCursorThemeSettings::new(Some(gsettings.clone()), None, paths, reconfigure);
        Fixture { dir, gsettings, reloads, reload_ok, backend }
    }

    fn apply(backend: &dyn CursorThemeSettings, theme: &str) -> Applied {
        let result = Rc::new(RefCell::new(None));
        let slot = result.clone();
        backend.set(theme, Box::new(move |applied| { slot.replace(Some(applied)); }));
        result.take().expect("done is called")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// Every file under `dir` with its contents.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push((path.clone(), std::fs::read(&path).unwrap()));
                }
            }
        }
        files.sort();
        files
    }

    #[test]
    fn system_backend_writes_each_step_and_keeps_everything_else() {
        let fx = fixture("steps");
        let config = fx.dir.join("config");
        let gtk3 = "# mine\n[Settings]\ngtk-icon-theme-name=Papirus-Dark\ngtk-cursor-theme-name=breeze_cursors\n; x\ngtk-font-name=Sans 10\n";
        std::fs::write(config.join("gtk-3.0/settings.ini"), gtk3).unwrap();
        let environment = "# labwc\nXKB_DEFAULT_LAYOUT=us\nXCURSOR_SIZE=32\nXCURSOR_THEME=breeze_cursors\n\nexport MOZ_ENABLE_WAYLAND=1\n";
        std::fs::write(config.join("labwc/environment"), environment).unwrap();
        assert_eq!(fx.backend.current().as_deref(), Some("Adwaita"));
        assert_eq!(apply(&fx.backend, "Bibata-Modern-Ice"), Applied::default());
        assert_eq!(*fx.gsettings.2.borrow(), ["Bibata-Modern-Ice"]);
        assert_eq!(fx.backend.current().as_deref(), Some("Bibata-Modern-Ice"));
        assert_eq!(read(&config.join("gtk-3.0/settings.ini")), gtk3.replace("=breeze_cursors", "=Bibata-Modern-Ice"));
        assert_eq!(read(&config.join("gtk-4.0/settings.ini")), "[Settings]\ngtk-cursor-theme-name=Bibata-Modern-Ice\n");
        assert_eq!(read(&config.join("labwc/environment")), environment.replace("=breeze_cursors", "=Bibata-Modern-Ice"));
        assert_eq!(read(&fx.dir.join("home/.local/share/icons/default/index.theme")), "[Icon Theme]\nInherits=Bibata-Modern-Ice\n");
        assert!(!fx.dir.join("home/.icons").exists());
        assert_eq!(read(&config.join("tessel/cursor-theme")), "Bibata-Modern-Ice\n");
        assert_eq!(*fx.reloads.borrow(), [true], "labwc reloads once");
        // The same theme again writes nothing and does not reload.
        let before = snapshot(&fx.dir);
        assert_eq!(apply(&fx.backend, "Bibata-Modern-Ice"), Applied::default());
        assert_eq!(snapshot(&fx.dir), before);
        assert_eq!(fx.gsettings.2.borrow().len(), 1);
        assert_eq!(fx.reloads.borrow().len(), 1);
        // An environment without the variable gets it appended.
        std::fs::write(config.join("labwc/environment"), "A=1").unwrap();
        apply(&fx.backend, "Adwaita");
        assert_eq!(read(&config.join("labwc/environment")), "A=1\nXCURSOR_THEME=Adwaita\n");
        // Without gsettings the GTK 4 file is the configured value.
        let other = SystemCursorThemeSettings::new(None, None, fx.backend.paths.clone(), labwc_settings::default_reconfigure());
        assert_eq!(other.current().as_deref(), Some("Adwaita"));
        assert_eq!(other.size(), 24);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_failing_step_is_reported_and_the_others_still_run() {
        let fx = fixture("failing");
        let config = fx.dir.join("config");
        // gsettings refuses, gtk-4.0/settings.ini is a directory, the environment is not UTF-8.
        fx.gsettings.1.set(false);
        std::fs::create_dir_all(config.join("gtk-4.0/settings.ini")).unwrap();
        std::fs::write(config.join("labwc/environment"), b"A=\xff\n").unwrap();
        let applied = apply(&fx.backend, "Bibata");
        assert_eq!(applied.errors.len(), 3, "{applied:?}");
        assert_eq!(applied.errors[0], "The shared pointer theme setting could not be changed.");
        assert!(applied.errors[1].contains("gtk-4.0/settings.ini"), "{applied:?}");
        assert!(applied.errors[2].contains("utf-8"), "{applied:?}");
        assert!(!applied.reload_failed);
        assert_eq!(read(&config.join("gtk-3.0/settings.ini")), "[Settings]\ngtk-cursor-theme-name=Bibata\n");
        assert_eq!(read(&fx.dir.join("home/.local/share/icons/default/index.theme")), "[Icon Theme]\nInherits=Bibata\n");
        assert_eq!(read(&config.join("tessel/cursor-theme")), "Bibata\n");
        assert!(fx.reloads.borrow().is_empty(), "nothing to reload");
        // The default theme and the record failing do not stop the environment and the reload.
        std::fs::write(config.join("labwc/environment"), "").unwrap();
        std::fs::remove_dir_all(fx.dir.join("home/.local/share/icons/default")).unwrap();
        std::fs::write(fx.dir.join("home/.local/share/icons/default"), "not a directory").unwrap();
        std::fs::remove_file(config.join("tessel/cursor-theme")).unwrap();
        std::fs::create_dir_all(config.join("tessel/cursor-theme")).unwrap();
        fx.reload_ok.set(false);
        let applied = apply(&fx.backend, "Adwaita");
        // gsettings already names Adwaita: not written, no error.
        assert_eq!(applied.errors.len(), 3, "{applied:?}");
        assert!(applied.reload_failed);
        assert_eq!(read(&config.join("labwc/environment")), "XCURSOR_THEME=Adwaita\n");
        assert_eq!(*fx.reloads.borrow(), [false]);
        // An id that cannot be written anywhere is refused before any write.
        let before = snapshot(&fx.dir);
        assert_eq!(apply(&fx.backend, "bad\nid").errors.len(), 1);
        assert_eq!(snapshot(&fx.dir), before);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn size_uses_the_accessibility_backend_and_reloads() {
        let fx = fixture("size");
        let environment = fx.dir.join("config/labwc/environment");
        std::fs::write(&environment, "# keep\nXCURSOR_THEME=Adwaita\n").unwrap();
        let result = Rc::new(RefCell::new(None));
        let slot = result.clone();
        fx.backend.set_size(48, Box::new(move |applied| { slot.replace(Some(applied)); }));
        assert_eq!(result.take(), Some(Applied::default()));
        assert_eq!(read(&environment), "# keep\nXCURSOR_THEME=Adwaita\nXCURSOR_SIZE=48\n");
        assert_eq!(fx.backend.size(), 48);
        assert_eq!(*fx.reloads.borrow(), [true]);
        let slot = result.clone();
        fx.backend.set_size(30, Box::new(move |applied| { slot.replace(Some(applied)); }));
        assert_eq!(result.take().unwrap().errors.len(), 1, "unsupported size");
        assert_eq!(fx.reloads.borrow().len(), 1);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }
}
