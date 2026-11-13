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
use tessel_core::tr;

use crate::about_page::{default_runner, Runner};
use crate::compat::{self, casefold, Result};
use crate::labwc_settings;

/// Every archive type of org.tessel.ArchiveManager.desktop's `MimeType=` line (also the
/// packaged labwc-mimeapps.list defaults), zip first because every archive manager opens it.
pub const ARCHIVE_TYPES: &[&str] = &[
    "application/zip", "application/x-tar", "application/x-cbt", "application/x-compressed-tar",
    "application/x-bzip2-compressed-tar", "application/x-bzip-compressed-tar", "application/x-xz-compressed-tar",
    "application/x-lzma-compressed-tar", "application/x-zstd-compressed-tar", "application/x-lzip-compressed-tar",
    "application/x-tzo", "application/x-lz4-compressed-tar", "application/x-lrzip-compressed-tar", "application/x-tarz",
    "application/x-zip-compressed", "application/vnd.comicbook+zip", "application/x-cbz", "application/x-java-archive",
    "application/x-7z-compressed", "application/x-cb7", "application/vnd.rar", "application/x-rar",
    "application/x-rar-compressed", "application/vnd.comicbook-rar", "application/x-cbr", "application/gzip",
    "application/x-gzip", "application/x-bzip2", "application/x-bzip", "application/x-xz", "application/x-lzma",
    "application/zstd", "application/x-zstd", "application/x-lzip", "application/x-lzop", "application/x-lz4",
    "application/x-lrzip", "application/x-compress", "application/x-cd-image", "application/x-iso9660-image",
    "application/x-cpio", "application/x-sv4cpio", "application/x-sv4crc", "application/x-bcpio",
    "application/vnd.debian.binary-package", "application/x-deb", "application/x-rpm", "application/x-source-rpm",
    "application/x-redhat-package-manager", "application/x-archive", "application/x-xar",
    "application/vnd.ms-cab-compressed", "application/x-lha", "application/x-lzh-compressed", "application/x-arj",
    "application/vnd.squashfs", "application/vnd.appimage", "application/x-iso9660-appimage", "application/x-ms-wim",
];

/// `(key, title, MIME types; the first one decides the candidates and current choice)`.
/// The other types are set only when the chosen application lists them in `MimeType=`.
pub const CATEGORIES: &[(&str, &str, &[&str])] = &[
    ("browser", "Web browser", &["x-scheme-handler/http", "x-scheme-handler/https", "text/html", "application/xhtml+xml"]),
    ("mail", "Mail", &["x-scheme-handler/mailto"]),
    ("files", "File manager", &["inode/directory"]),
    ("terminal", "Terminal", &[]),
    ("editor", "Text editor", &["text/plain"]),
    ("documents", "Document viewer", &["application/pdf", "application/postscript", "application/x-dvi", "image/vnd.djvu",
                                       "image/vnd.djvu+multipage", "application/oxps", "application/epub+zip"]),
    ("images", "Image viewer", &["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/tiff"]),
    ("video", "Video player", &["video/mp4", "video/x-matroska", "video/webm", "video/mpeg", "video/quicktime", "video/x-msvideo"]),
    ("music", "Music player", &["audio/mpeg", "audio/flac", "audio/ogg", "audio/x-vorbis+ogg", "audio/x-wav", "audio/mp4", "audio/x-opus+ogg"]),
    ("archives", "Archive manager", ARCHIVE_TYPES),
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

fn base_name(word: &str) -> String {
    Path::new(word).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `flatpak run [options] <app id>` from a Flatpak export's Exec words (field codes,
/// `@@` markers, `--file-forwarding` and the app's own arguments dropped).
fn flatpak_command(words: &[String]) -> Option<Vec<String>> {
    let run = words.iter().position(|w| w == "run")?;
    let mut command = vec!["flatpak".to_owned(), "run".to_owned()];
    for word in &words[run + 1..] {
        if word == "--file-forwarding" || word.starts_with('%') || word.starts_with("@@") {
            continue;
        }
        command.push(word.clone());
        if !word.starts_with('-') {
            return Some(command);
        }
    }
    None
}

/// The command to store in `$TERMINAL` (labwc's lab-sensible-terminal splits it
/// on spaces): the program name, or `flatpak run … <app id>` for a Flatpak.
pub fn app_command(executable: &str, commandline: Option<&str>) -> String {
    let commandline = commandline.filter(|c| !c.is_empty()).unwrap_or(executable);
    let words = compat::shlex_split(commandline).unwrap_or_else(|_| vec![executable.to_owned()]);
    if words.first().is_some_and(|w| base_name(w) == "flatpak") {
        if let Some(command) = flatpak_command(&words) {
            return command.join(" ");
        }
    }
    words.first().map(|w| base_name(w)).unwrap_or_default()
}

/// What identifies a `$TERMINAL` value: the program name, or a Flatpak's app id.
pub fn terminal_key(command: &str) -> String {
    let words: Vec<String> = compat::split_whitespace(command).into_iter().map(str::to_owned).collect();
    let Some(first) = words.first().map(|w| base_name(w)) else { return String::new() };
    if first == "flatpak" {
        if let Some(id) = flatpak_command(&words).and_then(|c| c.last().cloned()) {
            return format!("flatpak:{id}");
        }
    }
    first
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

fn supports(app: &App, mime: &str) -> bool {
    app.info.as_ref().is_some_and(|info| info.supported_types().iter().any(|t| t.as_str() == mime))
}

/// The types of a category to give `app`: the first always (the list came from it),
/// the rest only when the application says it opens them.
pub fn types_to_set<'a>(app: &App, types: &[&'a str]) -> Vec<&'a str> {
    types.iter().enumerate().filter(|(i, mime)| *i == 0 || supports(app, mime)).map(|(_, mime)| *mime).collect()
}

/// `$XDG_CURRENT_DESKTOP` names as GIO uses them for `<desktop>-mimeapps.list`.
fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').filter(|d| !d.is_empty()).map(str::to_lowercase).collect()
}

/// A `~/.config/<desktop>-mimeapps.list` default beats `~/.config/mimeapps.list`,
/// where GIO writes: point the entries it has for `types` at the chosen app too.
pub fn update_desktop_lists(config: &Path, desktops: &[String], types: &[&str], id: &str) -> Result<()> {
    const GROUP: &str = "Default Applications";
    for desktop in desktops {
        let path = config.join(format!("{desktop}-mimeapps.list"));
        if !path.is_file() {
            continue;
        }
        let file = glib::KeyFile::new();
        file.load_from_file(&path, glib::KeyFileFlags::KEEP_COMMENTS | glib::KeyFileFlags::KEEP_TRANSLATIONS)?;
        let mut changed = false;
        for mime in types {
            if file.has_key(GROUP, mime).unwrap_or(false) && file.string(GROUP, mime).map(|v| v != format!("{id};")).unwrap_or(true) {
                file.set_string(GROUP, mime, &format!("{id};"));
                changed = true;
            }
        }
        if changed {
            file.save_to_file(&path)?;
        }
    }
    Ok(())
}

pub struct DefaultAppsBackend {
    pub environment: Option<PathBuf>,
    pub terminals_list: PathBuf,
    pub runner: Runner,
    /// `labwc --reconfigure`: labwc reloads its environment file, so shortcuts and
    /// menus started by labwc get the new `$TERMINAL` at once.
    pub reconfigure: labwc_settings::Reconfigure,
}

impl DefaultAppsBackend {
    pub fn new(environment: Option<PathBuf>, terminals_list: Option<PathBuf>, runner: Option<Runner>) -> Self {
        DefaultAppsBackend {
            environment,
            terminals_list: terminals_list.unwrap_or_else(|| labwc_settings::config_home().join("xdg-terminals.list")),
            runner: runner.unwrap_or_else(default_runner),
            reconfigure: labwc_settings::default_reconfigure(),
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
            let wanted = terminal_key(&value);
            return candidates.iter().find(|app| !wanted.is_empty() && terminal_key(&info_command(app)) == wanted).cloned();
        }
        gio::AppInfo::default_for_type(types[0], false).map(App::from_info)
    }

    fn set(&self, key: &str, types: &[&str], app: &App, done: Done) -> Result<()> {
        if key == "terminal" {
            labwc_settings::update_environment(&labwc_settings::env_values(&[("TERMINAL", Some(&info_command(app)))]), self.environment.as_deref())?;
            self.write_terminals(&app.id)?;
            (self.reconfigure)(Some(Box::new(move |okay, _message| {
                done(true, if okay { String::new() } else { tr!("Applies to newly opened terminals after you sign out and back in.") })
            })));
            return Ok(());
        }
        if let Some(info) = &app.info {
            let types = types_to_set(app, types);
            // Before GIO writes, so its reload of the user lists sees both files.
            let mut failure = update_desktop_lists(&labwc_settings::config_home(), &current_desktops(), &types, &app.id).err();
            // Every type is tried, so one refusal does not leave the rest unchanged.
            for mime in &types {
                if let Err(error) = info.set_as_default_for_type(mime) {
                    failure.get_or_insert(compat::Error::from(error));
                }
            }
            if let Some(error) = failure {
                return Err(error);
            }
        }
        // xdg-settings accepts any desktop file; only hand it a real browser.
        let browser = app.info.is_none() || supports(app, "x-scheme-handler/http");
        if key == "browser" && browser && gtk::glib::find_program_in_path("xdg-settings").is_some() {
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
            let mut names = labels(&apps);
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
            // Show what really opens these files now, not the refused choice.
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || if let Some(t) = weak.upgrade() { if !t.page.closed() { t.refresh() } });
        }
    }
}

/// Dropdown labels; applications with the same name (a Flatpak and a package of
/// one browser, say) also show their desktop id.
pub fn labels(apps: &[App]) -> Vec<String> {
    apps.iter().map(|app| {
        if apps.iter().filter(|other| other.name == app.name).count() > 1 {
            format!("{} ({})", app.name, app.id.trim_end_matches(".desktop"))
        } else {
            app.name.clone()
        }
    }).collect()
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

    const WEZTERM: &str = "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=wezterm --file-forwarding org.wezfurlong.wezterm start --cwd . @@u %U @@";

    #[test]
    fn terminal_commands_and_list() {
        assert_eq!(app_command("/usr/bin/foot", Some("/usr/bin/foot")), "foot");
        assert_eq!(app_command("flatpak", Some("flatpak run org.wezfurlong.wezterm %U")), "flatpak run org.wezfurlong.wezterm");
        assert_eq!(app_command("kitty", Some("kitty \"unterminated")), "kitty");
        let root = scratch("terminals");
        let backend = DefaultAppsBackend::new(Some(root.join("environment")), Some(root.join("xdg-terminals.list")), Some(Rc::new(|_, d| d(true, String::new()))));
        std::fs::write(&backend.terminals_list, "a.desktop\n\nfoot.desktop\nb.desktop").unwrap();
        backend.write_terminals("foot.desktop").unwrap();
        assert_eq!(std::fs::read_to_string(&backend.terminals_list).unwrap(), "foot.desktop\na.desktop\nb.desktop\n");
    }

    #[test]
    fn flatpak_terminals_keep_their_app_and_command() {
        // A real Flatpak export: never a bare `flatpak` in $TERMINAL.
        assert_eq!(app_command("/usr/bin/flatpak", Some(WEZTERM)), "flatpak run --branch=stable --arch=x86_64 --command=wezterm org.wezfurlong.wezterm");
        assert_eq!(app_command("flatpak", Some("flatpak run --command=x org.y")), "flatpak run --command=x org.y");
        assert_eq!(app_command("flatpak", Some("flatpak --version")), "flatpak");
        // The page finds the current Flatpak terminal again from what it saved.
        assert_eq!(terminal_key(&app_command("/usr/bin/flatpak", Some(WEZTERM))), "flatpak:org.wezfurlong.wezterm");
        assert_eq!(terminal_key("/usr/bin/flatpak run org.wezfurlong.wezterm"), "flatpak:org.wezfurlong.wezterm");
        assert_ne!(terminal_key("flatpak run org.other"), terminal_key("flatpak run org.wezfurlong.wezterm"));
        assert_eq!(terminal_key("/usr/bin/foot --title x"), "foot");
        assert_eq!(terminal_key(""), "");
    }

    #[test]
    fn duplicate_names_show_their_ids() {
        let apps = [App::new("firefox.desktop", "Firefox"), App::new("org.mozilla.firefox.desktop", "Firefox"), App::new("b.desktop", "B")];
        assert_eq!(labels(&apps), ["Firefox (firefox)", "Firefox (org.mozilla.firefox)", "B"]);
    }

    #[test]
    fn categories_keep_the_first_type_rule() {
        let keys: Vec<&str> = CATEGORIES.iter().map(|c| c.0).collect();
        assert_eq!(keys, ["browser", "mail", "files", "terminal", "editor", "documents", "images", "video", "music", "archives"]);
        let types = |key: &str| CATEGORIES.iter().find(|c| c.0 == key).unwrap().2;
        assert_eq!(types("documents")[0], "application/pdf");
        assert_eq!(types("archives")[0], "application/zip");
        // Every archive type of Tessel's Archive Manager, which is also the packaged default.
        let desktop = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../archive/data/org.tessel.ArchiveManager.desktop")).unwrap();
        let line = desktop.lines().find_map(|l| l.strip_prefix("MimeType=")).unwrap();
        let mut packaged: Vec<&str> = line.split(';').filter(|t| !t.is_empty()).collect();
        let mut ours = types("archives").to_vec();
        packaged.sort();
        ours.sort();
        assert_eq!(ours, packaged);
        // A stand-in without GIO info gets the first type only.
        assert_eq!(types_to_set(&App::new("x.desktop", "X"), types("images")), ["image/png"]);
    }

    #[test]
    fn desktop_lists_follow_the_choice() {
        let root = scratch("desktop-lists");
        std::fs::write(root.join("labwc-mimeapps.list"), "# mine\n[Default Applications]\nimage/png=old.desktop;\naudio/flac=keep.desktop;\n").unwrap();
        update_desktop_lists(&root, &["wlroots".into(), "labwc".into()], &["image/png", "image/gif"], "new.desktop").unwrap();
        let text = std::fs::read_to_string(root.join("labwc-mimeapps.list")).unwrap();
        assert!(text.contains("# mine") && text.contains("image/png=new.desktop;") && text.contains("audio/flac=keep.desktop;"), "{text}");
        assert!(!text.contains("image/gif"), "types the file did not name stay out: {text}");
        assert!(!root.join("wlroots-mimeapps.list").exists());
    }

    /// The real GIO, xdg-settings and mimeapps.list precedence, in a child process with a
    /// private HOME and XDG dirs (GIO reads them once per process).
    #[test]
    fn choices_win_over_packaged_labwc_defaults() {
        if let Ok(root) = std::env::var("GS_DEFAULT_APPS_CHILD") {
            return precedence_child(Path::new(&root));
        }
        let root = scratch("precedence");
        let (home, sys) = (root.join("home"), root.join("sys"));
        let apps = sys.join("applications");
        for dir in [home.join(".config/labwc"), home.join(".local/share/applications"), apps.clone(), root.join("etc"), root.join("rt")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let _ = std::os::unix::fs::symlink("/usr/share/mime", sys.join("mime"));
        let app = |id: &str, extra: &str| {
            std::fs::write(apps.join(id), format!("[Desktop Entry]\nType=Application\nName={id}\nExec=/bin/true %U\n{extra}\n")).unwrap();
        };
        app("org.tessel.Pictures.desktop", "MimeType=image/png;image/jpeg;image/gif;");
        app("viewer-a.desktop", "MimeType=image/png;image/jpeg;");
        app("viewer-b.desktop", "MimeType=image/png;image/jpeg;");
        app("org.tessel.ArchiveManager.desktop", &format!("MimeType={};", ARCHIVE_TYPES.join(";")));
        app("ark.desktop", "MimeType=application/zip;application/x-tar;");
        app("editor.desktop", "MimeType=text/plain;");
        app("br-a.desktop", "MimeType=x-scheme-handler/http;x-scheme-handler/https;text/html;");
        app("br-b.desktop", "MimeType=x-scheme-handler/http;x-scheme-handler/https;text/html;");
        app("org.wezfurlong.wezterm.desktop", &format!("Categories=System;TerminalEmulator;\nX-Flatpak=org.wezfurlong.wezterm\nExec={WEZTERM}"));
        // GIO finds the MIME types of system data dirs through mimeinfo.cache.
        if std::process::Command::new("update-desktop-database").arg(&apps).status().map(|s| !s.success()).unwrap_or(true) {
            println!("skipped: update-desktop-database (desktop-file-utils) is not installed");
            return;
        }
        // Distro defaults lose to the desktop's own list in the same directory.
        std::fs::write(apps.join("mimeapps.list"), "[Default Applications]\nimage/png=viewer-a.desktop;\n").unwrap();
        let mut packaged = String::from("[Default Applications]\nimage/png=org.tessel.Pictures.desktop;\nimage/jpeg=org.tessel.Pictures.desktop;\nimage/gif=org.tessel.Pictures.desktop;\n");
        for mime in ARCHIVE_TYPES {
            packaged += &format!("{mime}=org.tessel.ArchiveManager.desktop;\n");
        }
        std::fs::write(apps.join("labwc-mimeapps.list"), packaged).unwrap();
        // A user's own per-desktop list beats ~/.config/mimeapps.list, where GIO writes.
        std::fs::write(home.join(".config/labwc-mimeapps.list"), "[Default Applications]\nimage/jpeg=viewer-a.desktop;\n").unwrap();
        // A default naming an application that is not installed.
        std::fs::write(home.join(".config/mimeapps.list"), "[Default Applications]\ntext/plain=gone.desktop;\n").unwrap();
        let test = "default_apps_page::tests::choices_win_over_packaged_labwc_defaults";
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env_clear()
            .envs([("PATH", "/usr/bin:/bin"), ("LANG", "C.UTF-8"), ("XDG_CURRENT_DESKTOP", "wlroots:labwc"), ("GIO_USE_VFS", "local"),
                   ("GSETTINGS_BACKEND", "memory"), ("GTK_USE_PORTAL", "0"), ("GDK_DEBUG", "no-portals")])
            .env("GS_DEFAULT_APPS_CHILD", &root)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_DATA_DIRS", &sys)
            .env("XDG_CONFIG_DIRS", root.join("etc"))
            .env("XDG_RUNTIME_DIR", root.join("rt"))
            .env("TMPDIR", &root)
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-gs-default-apps")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success() && text.contains("1 passed"), "{text}");
    }

    fn precedence_child(root: &Path) {
        let default = |mime: &str| gio::AppInfo::default_for_type(mime, false).and_then(|a| a.id()).map(|i| i.to_string()).unwrap_or_default();
        let types = |key: &str| CATEGORIES.iter().find(|c| c.0 == key).unwrap().2;
        let reloads = Rc::new(Cell::new(0));
        let counter = reloads.clone();
        let mut backend = DefaultAppsBackend::new(Some(root.join("home/.config/labwc/environment")), None,
            Some(Rc::new(|argv: Vec<String>, done: Box<dyn FnOnce(bool, String)>| {
                let okay = std::process::Command::new(&argv[0]).args(&argv[1..]).status().map(|s| s.success()).unwrap_or(false);
                done(okay, String::new());
            })));
        backend.reconfigure = Rc::new(move |done: Option<labwc_settings::Done>| {
            counter.set(counter.get() + 1);
            if let Some(done) = done { done(true, String::new()) }
        });
        let pick = |key: &str, id: &str| {
            let candidates = backend.candidates(key, types(key));
            let app = candidates.iter().find(|a| a.id == id).unwrap_or_else(|| panic!("{key}: {id} not offered")).clone();
            let note = Rc::new(RefCell::new(None));
            let sink = note.clone();
            backend.set(key, types(key), &app, Box::new(move |okay, text| { sink.replace(Some((okay, text))); })).unwrap();
            let result = note.borrow().clone().expect("done");
            result
        };
        let current = |key: &str| {
            let candidates = backend.candidates(key, types(key));
            backend.current(key, types(key), &candidates).map(|a| a.id)
        };

        // Packaged labwc-mimeapps.list beats the distro mimeapps.list.
        assert_eq!(current("images").as_deref(), Some("org.tessel.Pictures.desktop"));
        assert_eq!(default("application/x-rpm"), "org.tessel.ArchiveManager.desktop");
        // A default naming an uninstalled app is not what opens files.
        assert_ne!(current("editor").as_deref(), Some("gone.desktop"));
        assert_eq!(current("editor").as_deref(), Some("editor.desktop"));

        // A choice on the page wins over both packaged and per-desktop user lists.
        assert_eq!(pick("images", "viewer-b.desktop"), (true, String::new()));
        assert_eq!(current("images").as_deref(), Some("viewer-b.desktop"));
        assert_eq!(default("image/jpeg"), "viewer-b.desktop");
        // ...but only for the types the app opens.
        assert_eq!(default("image/gif"), "org.tessel.Pictures.desktop");
        let user = std::fs::read_to_string(root.join("home/.config/labwc-mimeapps.list")).unwrap();
        assert!(user.contains("image/jpeg=viewer-b.desktop;"), "{user}");

        assert_eq!(pick("archives", "ark.desktop"), (true, String::new()));
        assert_eq!(current("archives").as_deref(), Some("ark.desktop"));
        assert_eq!(default("application/x-tar"), "ark.desktop");
        assert_eq!(default("application/x-rpm"), "org.tessel.ArchiveManager.desktop");
        // Back to Tessel's Archive Manager.
        pick("archives", "org.tessel.ArchiveManager.desktop");
        assert_eq!(default("application/x-tar"), "org.tessel.ArchiveManager.desktop");

        // The browser through GIO and xdg-settings (when installed); xdg-settings must not undo it.
        pick("browser", "br-a.desktop");
        assert_eq!(pick("browser", "br-b.desktop"), (true, String::new()));
        for mime in ["x-scheme-handler/http", "x-scheme-handler/https", "text/html"] {
            assert_eq!(default(mime), "br-b.desktop", "{mime}");
        }

        // A Flatpak terminal: a runnable $TERMINAL, found again, labwc told to reload.
        assert_eq!(pick("terminal", "org.wezfurlong.wezterm.desktop"), (true, String::new()));
        let environment = std::fs::read_to_string(root.join("home/.config/labwc/environment")).unwrap();
        assert!(environment.contains("TERMINAL=flatpak run --branch=stable --arch=x86_64 --command=wezterm org.wezfurlong.wezterm"), "{environment}");
        assert_eq!(current("terminal").as_deref(), Some("org.wezfurlong.wezterm.desktop"));
        assert_eq!(std::fs::read_to_string(root.join("home/.config/xdg-terminals.list")).unwrap(), "org.wezfurlong.wezterm.desktop\n");
        assert_eq!(reloads.get(), 1);
    }

    #[test]
    fn terminal_note_when_labwc_cannot_reload() {
        let root = scratch("terminal-note");
        let mut backend = DefaultAppsBackend::new(Some(root.join("environment")), Some(root.join("xdg-terminals.list")), Some(Rc::new(|_, d| d(true, String::new()))));
        backend.reconfigure = Rc::new(|done: Option<labwc_settings::Done>| if let Some(done) = done { done(false, "no labwc".into()) });
        let note = Rc::new(RefCell::new(String::new()));
        let sink = note.clone();
        backend.set("terminal", &[], &App::new("foot.desktop", "Foot"), Box::new(move |_, text| { sink.replace(text); })).unwrap();
        assert!(note.borrow().contains("sign out"), "{}", note.borrow());
        assert_eq!(labwc_settings::read_environment(Some(&root.join("environment"))).unwrap()["TERMINAL"], "foot");
    }
}
