//! default_apps_page.py: which application opens web links, mail, folders,
//! files and media.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gs_base::widgets::{dropdown, label, status_text, Page};
use gs_base::SettingsPage;
use gtk::{gio, glib};
use gtk::prelude::*;
use suite_core::tr;

use crate::about_page::{default_runner, Runner};
use crate::compat::{self, casefold, Result};
use crate::labwc_settings;

/// `(key, title, MIME types; the first one decides the candidates and current choice)`.
pub const CATEGORIES: &[(&str, &str, &[&str])] = &[
    ("browser", "Web browser", &["x-scheme-handler/http", "x-scheme-handler/https", "text/html", "application/xhtml+xml"]),
    ("mail", "Mail", &["x-scheme-handler/mailto"]),
    ("files", "File manager", &["inode/directory"]),
    ("terminal", "Terminal", &[]),
    ("editor", "Text editor", &["text/plain"]),
    ("images", "Image viewer", &["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/tiff"]),
    ("video", "Video player", &["video/mp4", "video/x-matroska", "video/webm", "video/mpeg", "video/quicktime", "video/x-msvideo"]),
    ("music", "Music player", &["audio/mpeg", "audio/flac", "audio/ogg", "audio/x-vorbis+ogg", "audio/x-wav", "audio/mp4", "audio/x-opus+ogg"]),
];

/// An application choice (a `Gio.AppInfo`, or a test stand-in).
#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub name: String,
    pub info: Option<gio::AppInfo>,
}

impl App {
    pub fn new(id: &str, name: &str) -> App { App { id: id.into(), name: name.into(), info: None } }

    pub fn from_info(info: gio::AppInfo) -> App {
        App { id: info.id().map(|s| s.to_string()).unwrap_or_default(), name: info.display_name().to_string(), info: Some(info) }
    }
}

fn sorted(mut apps: Vec<App>) -> Vec<App> {
    apps.sort_by_key(|a| casefold(&a.name));
    apps
}

extern "C" {
    fn g_desktop_app_info_get_type() -> glib::ffi::GType;
    fn g_desktop_app_info_get_categories(info: *mut gtk::gio::ffi::GAppInfo) -> *const std::ffi::c_char;
}

/// `app.get_categories() if isinstance(app, Gio.DesktopAppInfo) else ''`
/// (GDesktopAppInfo lives in libgio but has no binding in the gio crate).
pub fn desktop_categories(app: &gio::AppInfo) -> Option<String> {
    use glib::translate::ToGlibPtr;
    let raw: *mut gtk::gio::ffi::GAppInfo = app.to_glib_none().0;
    // SAFETY: raw is a live GAppInfo; the type check guards the cast.
    unsafe {
        let is_desktop = glib::gobject_ffi::g_type_check_instance_is_a(raw as *mut _, g_desktop_app_info_get_type()) != 0;
        if !is_desktop {
            return None;
        }
        let text = g_desktop_app_info_get_categories(raw);
        (!text.is_null()).then(|| std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned())
    }
}

pub fn terminal_apps(apps: Option<Vec<gio::AppInfo>>) -> Vec<App> {
    let apps = apps.unwrap_or_else(gio::AppInfo::all);
    sorted(apps.into_iter().filter(|app| {
        let categories = desktop_categories(app).unwrap_or_default();
        categories.split(';').any(|c| c == "TerminalEmulator") && app.should_show()
    }).map(App::from_info).collect())
}

/// The program name to store in `$TERMINAL` (labwc's lab-sensible-terminal).
pub fn app_command(executable: &str, commandline: Option<&str>) -> String {
    let commandline = commandline.filter(|c| !c.is_empty()).unwrap_or(executable);
    let words = compat::shlex_split(commandline).unwrap_or_else(|_| vec![executable.to_owned()]);
    if words.first().is_some_and(|w| w == "flatpak") && !commandline.contains("--command") {
        return commandline.split(" %").next().unwrap_or("").to_owned();
    }
    words.first().map(|w| Path::new(w).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).unwrap_or_default()
}

fn info_command(app: &App) -> String {
    match &app.info {
        Some(info) => app_command(&info.executable().to_string_lossy(), info.commandline().as_deref().and_then(|p| p.to_str())),
        None => app.id.trim_end_matches(".desktop").to_owned(),
    }
}

/// `done(okay, note)`.
pub type Done = Box<dyn FnOnce(bool, String)>;

pub trait DefaultApps {
    fn candidates(&self, key: &str, types: &[&str]) -> Vec<App>;
    fn current(&self, key: &str, types: &[&str], candidates: &[App]) -> Option<App>;
    fn set(&self, key: &str, types: &[&str], app: &App, done: Done) -> Result<()>;
}

pub struct DefaultAppsBackend {
    pub environment: Option<PathBuf>,
    pub terminals_list: PathBuf,
    pub runner: Runner,
}

impl DefaultAppsBackend {
    pub fn new(environment: Option<PathBuf>, terminals_list: Option<PathBuf>, runner: Option<Runner>) -> Self {
        DefaultAppsBackend {
            environment,
            terminals_list: terminals_list.unwrap_or_else(|| labwc_settings::config_home().join("xdg-terminals.list")),
            runner: runner.unwrap_or_else(default_runner),
        }
    }

    pub fn write_terminals(&self, desktop_id: &str) -> Result<()> {
        let lines: Vec<String> = std::fs::read_to_string(&self.terminals_list).map(|t| compat::splitlines(&t, false)).unwrap_or_default();
        let mut result = vec![desktop_id.to_owned()];
        result.extend(lines.into_iter().filter(|l| !compat::strip(l).is_empty() && compat::strip(l) != desktop_id));
        labwc_settings::atomic_write(&self.terminals_list, (result.join("\n") + "\n").as_bytes())
    }
}

impl DefaultApps for DefaultAppsBackend {
    fn candidates(&self, key: &str, types: &[&str]) -> Vec<App> {
        if key == "terminal" {
            return terminal_apps(None);
        }
        let mut seen = Vec::new();
        let mut result = Vec::new();
        for app in gio::AppInfo::all_for_type(types[0]) {
            let Some(id) = app.id().map(|s| s.to_string()).filter(|s| !s.is_empty()) else { continue };
            if !seen.contains(&id) && app.should_show() {
                seen.push(id);
                result.push(App::from_info(app));
            }
        }
        sorted(result)
    }

    fn current(&self, key: &str, types: &[&str], candidates: &[App]) -> Option<App> {
        if key == "terminal" {
            let value = labwc_settings::read_environment(self.environment.as_deref()).ok()?.get("TERMINAL").cloned().unwrap_or_default();
            let first = compat::split_whitespace(&value).first().map(|w| Path::new(w).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())?;
            return candidates.iter().find(|app| !value.is_empty() && info_command(app) == first).cloned();
        }
        gio::AppInfo::default_for_type(types[0], false).map(App::from_info)
    }

    fn set(&self, key: &str, types: &[&str], app: &App, done: Done) -> Result<()> {
        if key == "terminal" {
            labwc_settings::update_environment(&labwc_settings::env_values(&[("TERMINAL", Some(&info_command(app)))]), self.environment.as_deref())?;
            self.write_terminals(&app.id)?;
            done(true, tr!("Applies to newly opened terminals after you sign out and back in."));
            return Ok(());
        }
        if let Some(info) = &app.info {
            for mime in types {
                info.set_as_default_for_type(mime).map_err(compat::Error::from)?;
            }
        }
        if key == "browser" && gtk::glib::find_program_in_path("xdg-settings").is_some() {
            (self.runner)(vec!["xdg-settings".into(), "set".into(), "default-web-browser".into(), app.id.clone()],
                          Box::new(move |_okay, _output| done(true, String::new())));
        } else {
            done(true, String::new());
        }
        Ok(())
    }
}

pub struct DefaultAppsPage {
    pub page: Page,
    pub backend: Rc<dyn DefaultApps>,
    pub card: gtk::Box,
    pub controls: RefCell<HashMap<&'static str, gtk::DropDown>>,
    apps: RefCell<HashMap<&'static str, Vec<Option<App>>>>,
    pub status: gtk::Label,
    loading: Cell<bool>,
}

impl DefaultAppsPage {
    pub fn new(backend: Option<Rc<dyn DefaultApps>>) -> Rc<DefaultAppsPage> {
        let page = Page::new("default-apps-page", &tr!("Default Apps"), &tr!("Choose the applications that open links, folders and files."),
                             Some("applications-system-symbolic"));
        let card = page.group(None, None, None);
        let status = label("", Some("detail"));
        page.content.append(&status);
        let this = Rc::new(DefaultAppsPage {
            page, backend: backend.unwrap_or_else(|| Rc::new(DefaultAppsBackend::new(None, None, None))), card,
            controls: Default::default(), apps: Default::default(), status, loading: Cell::new(false),
        });
        // One dropdown per category, kept across refreshes so search targets stay valid.
        for (key, title, _types) in CATEGORIES {
            let widget = dropdown(&[tr!("No applications installed")], 0);
            widget.set_size_request(260, -1);
            let weak = Rc::downgrade(&this);
            widget.connect_selected_notify(move |w| if let Some(t) = weak.upgrade() { t.changed(w, key) });
            Page::row(&this.card, &tr!(title), Some(widget.upcast_ref()), None);
            this.controls.borrow_mut().insert(key, widget);
        }
        this.refresh();
        this
    }

    pub fn refresh(&self) {
        self.loading.set(true);
        for (key, _title, types) in CATEGORIES {
            let mut apps = self.backend.candidates(key, types);
            let current = self.backend.current(key, types, &apps);
            let current_id = current.as_ref().map(|c| c.id.clone());
            if let Some(current) = &current {
                if !apps.iter().any(|a| a.id == current.id) {
                    apps.insert(0, current.clone());
                }
            }
            let mut names: Vec<String> = apps.iter().map(|a| a.name.clone()).collect();
            if names.is_empty() {
                names.push(tr!("No applications installed"));
            }
            let mut choices: Vec<Option<App>> = apps.into_iter().map(Some).collect();
            let mut selected = choices.iter().position(|a| a.as_ref().map(|a| Some(&a.id) == current_id.as_ref()).unwrap_or(false)).map(|i| i as i32).unwrap_or(-1);
            if selected < 0 && !choices.is_empty() {
                names.insert(0, tr!("Not set"));
                choices.insert(0, None);
                selected = 0;
            }
            let widget = self.controls.borrow()[key].clone();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            widget.set_model(Some(&gtk::StringList::new(&refs)));
            widget.set_selected(selected.max(0) as u32);
            widget.set_sensitive(!choices.is_empty());
            self.apps.borrow_mut().insert(key, choices);
        }
        self.loading.set(false);
    }

    fn changed(self: &Rc<Self>, widget: &gtk::DropDown, key: &'static str) {
        if self.loading.get() {
            return;
        }
        let types = CATEGORIES.iter().find(|(k, _, _)| *k == key).unwrap().2;
        let Some(app) = self.apps.borrow().get(key).and_then(|a| a.get(widget.selected() as usize).cloned()).flatten() else { return };
        let weak = Rc::downgrade(self);
        let name = app.name.clone();
        let done: Done = Box::new(move |_okay, note| {
            let Some(t) = weak.upgrade() else { return };
            if !t.page.closed() {
                let text = if note.is_empty() { tr!("{value1} is now the default.", value1 = name) } else { note };
                status_text(&t.status, &text, None);
            }
        });
        if let Err(error) = self.backend.set(key, types, &app, done) {
            status_text(&self.status, &tr!("The default application could not be changed: {value1}", value1 = error.to_string()), Some("error"));
        }
    }
}

pub fn build_page_with(backend: Option<Rc<dyn DefaultApps>>) -> SettingsPage {
    let controller = DefaultAppsPage::new(backend);
    let (close, activate) = (Rc::downgrade(&controller), Rc::downgrade(&controller));
    let page = SettingsPage::new(controller.page.widget.clone())
        .with_close(move || if let Some(c) = close.upgrade() { c.page.close() })
        .with_activate(move || if let Some(c) = activate.upgrade() { c.refresh() });
    for (key, widget) in controller.controls.borrow().iter() {
        page.add_target(&format!("default-{key}"), widget);
    }
    page.set_controller(controller.clone());
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    #[test]
    fn terminal_commands_and_list() {
        assert_eq!(app_command("/usr/bin/foot", Some("/usr/bin/foot")), "foot");
        assert_eq!(app_command("flatpak", Some("flatpak run org.wezfurlong.wezterm %U")), "flatpak run org.wezfurlong.wezterm");
        assert_eq!(app_command("flatpak", Some("flatpak run --command=x org.y")), "flatpak");
        assert_eq!(app_command("kitty", Some("kitty \"unterminated")), "kitty");
        let root = scratch("terminals");
        let backend = DefaultAppsBackend::new(Some(root.join("environment")), Some(root.join("xdg-terminals.list")), Some(Rc::new(|_, d| d(true, String::new()))));
        std::fs::write(&backend.terminals_list, "a.desktop\n\nfoot.desktop\nb.desktop").unwrap();
        backend.write_terminals("foot.desktop").unwrap();
        assert_eq!(std::fs::read_to_string(&backend.terminals_list).unwrap(), "foot.desktop\na.desktop\nb.desktop\n");
    }
}
