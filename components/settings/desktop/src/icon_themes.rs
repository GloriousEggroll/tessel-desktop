//! Theme & Wallpaper → Icons: the desktop icon theme, chosen from the
//! installed themes ([`tessel_core::icon_theme::discover`]) and written to
//! gsettings `org.gnome.desktop.interface icon-theme` and
//! `gtk-icon-theme-name` in `~/.config/gtk-{3,4}.0/settings.ini`.
//! Papirus-Dark is selected when nothing valid is configured.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use tessel_core::icon_theme::{self as icons, IconThemeInfo, DEFAULT};
use tessel_core::tr;

use crate::theme::{ColorSettings, GioColorSettings};
use crate::util::{accessible_label, label};

/// Sample icons shown under the chooser (first name the theme has): folder,
/// text file, settings.
pub const SAMPLES: [&[&str]; 3] = [&["folder"], &["text-x-generic", "text-plain"],
                                   &["preferences-system", "applications-system", "preferences-desktop"]];

/// Where the choice is stored (tests use a fake).
pub trait IconThemeSettings {
    /// The configured theme id (gsettings, else the GTK 4 settings.ini).
    fn current(&self) -> Option<String>;
    fn set(&self, theme: &str) -> Result<(), String>;
    fn connect_changed(&self, handler: Rc<dyn Fn()>) -> u64;
    fn disconnect(&self, id: u64);
}

/// gsettings (when the schema is installed) plus both GTK settings.ini files.
pub struct SystemIconThemeSettings {
    gsettings: Option<Rc<dyn ColorSettings>>,
    config_home: PathBuf,
}

impl SystemIconThemeSettings {
    pub fn new(gsettings: Option<Rc<dyn ColorSettings>>, config_home: PathBuf) -> Self {
        Self { gsettings, config_home }
    }

    pub fn from_session() -> Self {
        let gsettings = icons::interface_settings().map(|s| Rc::new(GioColorSettings::new(s)) as Rc<dyn ColorSettings>);
        Self::new(gsettings, tessel_core::paths::config_home())
    }
}

impl IconThemeSettings for SystemIconThemeSettings {
    fn current(&self) -> Option<String> {
        self.gsettings.as_ref().map(|s| s.get_string(icons::GSETTINGS_KEY)).filter(|v| !v.is_empty())
            .or_else(|| icons::read_gtk_ini(&icons::gtk_ini_paths(&self.config_home)[1]))
    }

    fn set(&self, theme: &str) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Some(settings) = &self.gsettings {
            if settings.get_string(icons::GSETTINGS_KEY) != theme && !settings.set_string(icons::GSETTINGS_KEY, theme) {
                errors.push(tr!("The shared icon theme setting could not be changed."));
            }
        }
        if let Err(error) = icons::write_gtk_inis(&self.config_home, theme) {
            errors.push(error);
        }
        // So the next login keeps this theme, also after another desktop
        // wrote its own into the shared settings.
        if let Err(error) = icons::write_choice(&self.config_home, theme) {
            errors.push(error);
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join(" ")) }
    }

    fn connect_changed(&self, handler: Rc<dyn Fn()>) -> u64 {
        self.gsettings.as_ref().map(|s| s.connect_changed(icons::GSETTINGS_KEY, handler)).unwrap_or(0)
    }

    fn disconnect(&self, id: u64) {
        if let Some(settings) = &self.gsettings {
            settings.disconnect(id);
        }
    }
}

/// Dropdown captions: `Name=`, with the id added when two names collide.
pub fn captions(themes: &[IconThemeInfo]) -> Vec<String> {
    themes.iter().map(|theme| {
        if themes.iter().filter(|t| t.name == theme.name).count() > 1 && theme.name != theme.id {
            format!("{} ({})", theme.name, theme.id)
        } else {
            theme.name.clone()
        }
    }).collect()
}

/// The row to select: the configured theme, else Papirus-Dark, else the first.
pub fn selected_index(themes: &[IconThemeInfo], current: Option<&str>) -> Option<usize> {
    current.and_then(|id| themes.iter().position(|t| t.id == id))
        .or_else(|| themes.iter().position(|t| t.id == DEFAULT))
        .or(if themes.is_empty() { None } else { Some(0) })
}

/// The Icons card of the Theme & Wallpaper page.
pub struct IconThemeChooser {
    pub card: gtk::Box,
    pub dropdown: gtk::DropDown,
    pub samples: gtk::Box,
    pub status: gtk::Label,
    pub themes: Vec<IconThemeInfo>,
    settings: Rc<dyn IconThemeSettings>,
    preview: gtk::IconTheme,
    handler: Cell<u64>,
    updating: Cell<bool>,
    closed: Cell<bool>,
    sample_theme: RefCell<Option<String>>,
}

impl IconThemeChooser {
    pub fn new(settings: Rc<dyn IconThemeSettings>, dirs: Vec<PathBuf>) -> Rc<Self> {
        let themes = icons::discover(&dirs);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("gs-card");
        card.add_css_class("icon-theme-card");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        let title = tr!("Icons");
        text.append(&label(&title, None));
        text.append(&label(&tr!("Choose the icon theme used by the desktop and applications."), Some("subtitle")));
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
        let samples = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(12).build();
        samples.add_css_class("icon-theme-samples");
        card.append(&samples);
        let status = label("", Some("subtitle"));
        status.set_visible(false);
        card.append(&status);
        if themes.is_empty() {
            dropdown.set_sensitive(false);
            samples.set_visible(false);
            status.set_text(&tr!("No icon themes are installed."));
            status.set_visible(true);
        }
        let preview = gtk::IconTheme::new();
        let paths: Vec<&std::path::Path> = dirs.iter().map(PathBuf::as_path).collect();
        preview.set_search_path(&paths);
        let chooser = Rc::new(Self {
            card, dropdown, samples, status, themes, settings, preview, handler: Cell::new(0), updating: Cell::new(false),
            closed: Cell::new(false), sample_theme: RefCell::new(None),
        });
        chooser.sync();
        let weak = Rc::downgrade(&chooser);
        chooser.dropdown.connect_selected_notify(move |_| {
            if let Some(chooser) = weak.upgrade() {
                chooser.selected();
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

    /// Select the configured theme (else Papirus-Dark) without saving.
    pub fn sync(&self) {
        if self.closed.get() {
            return;
        }
        if let Some(index) = selected_index(&self.themes, self.settings.current().as_deref()) {
            self.updating.set(true);
            self.dropdown.set_selected(index as u32);
            self.updating.set(false);
        }
        self.show_samples();
    }

    fn selected(&self) {
        self.show_samples();
        if self.updating.get() || self.closed.get() {
            return;
        }
        let Some(theme) = self.selected_id() else { return };
        if self.settings.current().as_deref() == Some(theme.as_str()) {
            return;
        }
        match self.settings.set(&theme) {
            Ok(()) => self.status.set_visible(false),
            Err(error) => {
                self.status.set_text(&tr!("The icon theme could not be changed: {error}", error = error.as_str()));
                self.status.set_visible(true);
                self.sync();
            }
        }
    }

    /// Sample icons drawn from the selected theme (not the app's own theme).
    fn show_samples(&self) {
        let Some(theme) = self.selected_id() else { return };
        if self.sample_theme.borrow().as_deref() == Some(theme.as_str()) {
            return;
        }
        self.preview.set_theme_name(Some(&theme));
        while let Some(child) = self.samples.first_child() {
            self.samples.remove(&child);
        }
        let scale = self.samples.scale_factor().max(1);
        for names in SAMPLES {
            // Only look up icons the theme (or what it inherits) has: GTK's
            // missing-icon fallback recurses without end in a private theme.
            let image = match names.iter().find(|name| self.preview.has_icon(name)) {
                Some(name) => {
                    let paintable = self.preview.lookup_icon(name, &[], 32, scale, gtk::TextDirection::Ltr,
                                                             gtk::IconLookupFlags::empty());
                    gtk::Image::from_paintable(Some(&paintable))
                }
                None => gtk::Image::from_icon_name("image-missing"),
            };
            image.set_pixel_size(32);
            image.set_widget_name(names[0]);
            self.samples.append(&image);
        }
        self.sample_theme.replace(Some(theme));
    }

    /// The icon file each sample resolved to (tests).
    pub fn sample_files(&self) -> Vec<Option<PathBuf>> {
        let mut files = Vec::new();
        let mut child = self.samples.first_child();
        while let Some(widget) = child {
            let file = widget.downcast_ref::<gtk::Image>().and_then(|i| i.paintable())
                .and_then(|p| p.downcast::<gtk::IconPaintable>().ok()).and_then(|p| p.file()).and_then(|f| f.path());
            files.push(file);
            child = widget.next_sibling();
        }
        files
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

    fn info(id: &str, name: &str) -> IconThemeInfo {
        IconThemeInfo { id: id.into(), name: name.into(), dir: PathBuf::from("/x").join(id) }
    }

    #[test]
    fn selection_prefers_the_configured_theme_then_papirus_dark() {
        let themes = [info("Adwaita", "Adwaita"), info("Papirus-Dark", "Papirus-Dark"), info("breeze", "Breeze")];
        assert_eq!(selected_index(&themes, Some("breeze")), Some(2));
        assert_eq!(selected_index(&themes, Some("missing")), Some(1));
        assert_eq!(selected_index(&themes, None), Some(1));
        assert_eq!(selected_index(&themes[..1], Some("missing")), Some(0));
        assert_eq!(selected_index(&[], None), None);
    }

    #[test]
    fn duplicate_names_show_their_ids() {
        let themes = [info("Adwaita", "Adwaita"), info("breeze", "Breeze"), info("breeze-alt", "Breeze")];
        assert_eq!(captions(&themes), ["Adwaita", "Breeze (breeze)", "Breeze (breeze-alt)"]);
    }

    struct FakeGsettings(RefCell<String>, Cell<bool>);

    impl ColorSettings for FakeGsettings {
        fn get_string(&self, _key: &str) -> String { self.0.borrow().clone() }
        fn set_string(&self, key: &str, value: &str) -> bool {
            assert_eq!(key, "icon-theme");
            if self.1.get() {
                self.0.replace(value.into());
            }
            self.1.get()
        }
        fn connect_changed(&self, _key: &str, _handler: Rc<dyn Fn()>) -> u64 { 1 }
        fn disconnect(&self, _id: u64) {}
    }

    #[test]
    fn system_backend_writes_gsettings_and_both_ini_files() {
        let dir = std::env::temp_dir().join(format!("gs-icon-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("gtk-3.0")).unwrap();
        std::fs::write(dir.join("gtk-3.0/settings.ini"), "[Settings]\n# keep\ngtk-theme-name=Adwaita\n").unwrap();
        let gsettings = Rc::new(FakeGsettings(RefCell::new(String::new()), Cell::new(true)));
        let backend = SystemIconThemeSettings::new(Some(gsettings.clone()), dir.clone());
        assert_eq!(backend.current(), None);
        backend.set("Adwaita").unwrap();
        assert_eq!(backend.current().as_deref(), Some("Adwaita"));
        assert_eq!(std::fs::read_to_string(dir.join("gtk-3.0/settings.ini")).unwrap(),
                   "[Settings]\n# keep\ngtk-theme-name=Adwaita\ngtk-icon-theme-name=Adwaita\n");
        assert_eq!(std::fs::read_to_string(dir.join("gtk-4.0/settings.ini")).unwrap(), "[Settings]\ngtk-icon-theme-name=Adwaita\n");
        gsettings.1.set(false);
        assert!(backend.set("Papirus-Dark").is_err());
        // The ini files are written even when gsettings refuses.
        assert_eq!(icons::read_gtk_ini(&dir.join("gtk-4.0/settings.ini")).as_deref(), Some("Papirus-Dark"));
        // Without gsettings the GTK 4 file is the configured value.
        let backend = SystemIconThemeSettings::new(None, dir.clone());
        assert_eq!(backend.current().as_deref(), Some("Papirus-Dark"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
