//! Icon themes shared by Global Settings, session startup and the dock:
//! discovery of installed themes, the GTK `settings.ini` key, and the suite
//! default (Papirus-Dark).
//!
//! A usable icon theme is a directory in an icon search path with an
//! `index.theme` whose `[Icon Theme]` group has a non-empty `Directories=`
//! (cursor-only themes have none). Everything here takes explicit paths so
//! tests use fake directories.
use std::io::Write;
use std::path::{Path, PathBuf};

/// The suite-wide default icon theme.
pub const DEFAULT: &str = "Papirus-Dark";
/// The themes the dock follows with light/dark mode (and recolours).
pub const PAPIRUS_FAMILY: [&str; 3] = ["Papirus", "Papirus-Dark", "Papirus-Light"];
/// `org.gnome.desktop.interface` key.
pub const GSETTINGS_KEY: &str = "icon-theme";
/// GTK `settings.ini` key in `[Settings]`.
pub const INI_KEY: &str = "gtk-icon-theme-name";
pub const INI_SECTION: &str = "Settings";
/// Marker of the dock's private recoloured Papirus mirrors.
pub const MIRROR_MARKER: &str = ".tessel-dock-papirus.json";
/// What KDE writes into the shared GTK settings at every Plasma login for a
/// user on its default icons. Found there, it is KDE's default, not a choice
/// made in Tessel, unless [`read_choice`] names it.
pub const KDE_DEFAULTS: [&str; 2] = ["breeze", "breeze-dark"];
/// Never offered as a choice: the fallback theme and the cursor alias.
pub const HIDDEN_IDS: [&str; 2] = ["hicolor", "default"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconThemeInfo {
    /// Directory name (the value GTK and gsettings store).
    pub id: String,
    /// `Name=` from index.theme (the id when missing).
    pub name: String,
    pub dir: PathBuf,
}

pub fn is_papirus_family(id: &str) -> bool {
    PAPIRUS_FAMILY.contains(&id)
}

/// `$XDG_DATA_HOME/icons`, `~/.icons`, `$XDG_DATA_DIRS/*/icons` (GTK's order).
pub fn search_dirs() -> Vec<PathBuf> {
    let data_dirs: Vec<PathBuf> = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into())
        .split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect();
    search_dirs_for(&crate::paths::data_home(), &crate::paths::home(), &data_dirs)
}

pub fn search_dirs_for(data_home: &Path, home: &Path, data_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = vec![data_home.join("icons"), home.join(".icons")];
    dirs.extend(data_dirs.iter().map(|d| d.join("icons")));
    let mut seen = Vec::new();
    dirs.retain(|d| if seen.contains(d) { false } else { seen.push(d.clone()); true });
    dirs
}

/// `(Name=, has directories, Hidden=true)` of an index.theme `[Icon Theme]` group.
fn parse_index(text: &str) -> (Option<String>, bool, bool) {
    let (mut name, mut directories, mut hidden, mut inside) = (None, false, false, false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[Icon Theme]";
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "Name" if !value.is_empty() => name = Some(value.to_owned()),
            "Directories" | "ScaledDirectories" => directories |= value.split(',').any(|d| !d.trim().is_empty()),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    (name, directories, hidden)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && !id.contains('/')
}

/// The theme in `dir` (named after the directory) when it has icons.
pub fn theme_at(dir: &Path) -> Option<IconThemeInfo> {
    let id = dir.file_name()?.to_str()?.to_owned();
    if !valid_id(&id) {
        return None;
    }
    let text = std::fs::read_to_string(dir.join("index.theme")).ok()?;
    let (name, directories, _) = parse_index(&text);
    directories.then(|| IconThemeInfo { name: name.unwrap_or_else(|| id.clone()), id, dir: dir.to_path_buf() })
}

/// Whether `id` is an installed theme with icons in any of `dirs`.
pub fn is_installed(dirs: &[PathBuf], id: &str) -> bool {
    valid_id(id) && dirs.iter().any(|d| theme_at(&d.join(id)).is_some())
}

/// The themes a user can choose: one per id (earlier search dirs win), no
/// cursor-only or `Hidden=true` themes (except Adwaita), no
/// `hicolor`/`default`, no private dock mirrors; sorted by name.
pub fn discover(dirs: &[PathBuf]) -> Vec<IconThemeInfo> {
    let mut themes: Vec<IconThemeInfo> = Vec::new();
    for root in dirs {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            let Some(id) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if HIDDEN_IDS.contains(&id) || themes.iter().any(|t| t.id == id) || path.join(MIRROR_MARKER).exists() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path.join("index.theme")) else { continue };
            // Hidden= marks fallback themes; Adwaita (GTK's default) sets it
            // too but is a real choice.
            if parse_index(&text).2 && id != "Adwaita" {
                continue;
            }
            if let Some(theme) = theme_at(&path) {
                themes.push(theme);
            }
        }
    }
    themes.sort_by(|a, b| crate::pyjson::casefold(&a.name).cmp(&crate::pyjson::casefold(&b.name)).then(a.id.cmp(&b.id)));
    themes
}

fn split_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with(';') || trimmed.starts_with('[') {
        return None;
    }
    trimmed.split_once('=').map(|(key, _)| key.trim())
}

fn section_header(line: &str) -> Option<&str> {
    let line = line.trim();
    line.strip_prefix('[').and_then(|l| l.strip_suffix(']'))
}

/// The last `key=` value in `[section]` (GLib key files: the last one wins).
pub fn ini_get(text: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    let mut value = None;
    for line in text.lines() {
        if let Some(name) = section_header(line) {
            inside = name == section;
        } else if inside && split_key(line) == Some(key) {
            value = line.split_once('=').map(|(_, v)| v.trim().to_owned());
        }
    }
    value
}

/// `text` with `key=value` in `[section]`: existing assignments there are
/// replaced in place, else the key is added after the section's last
/// non-blank line; a missing section is appended. Every other line (and
/// comment) is kept byte for byte.
pub fn ini_set(text: &str, section: &str, key: &str, value: &str) -> String {
    let assignment = format!("{key}={value}");
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
    let newline = |line: &String| if line.ends_with('\n') { "\n" } else { "" };
    let mut inside = false;
    let mut found = false;
    let mut last_in_section: Option<usize> = None;
    for (index, line) in lines.iter_mut().enumerate() {
        if let Some(name) = section_header(line) {
            if inside && name != section {
                inside = false;
            } else if name == section {
                inside = true;
                if last_in_section.is_none() {
                    last_in_section = Some(index);
                }
            }
            continue;
        }
        if !inside {
            continue;
        }
        if split_key(line) == Some(key) {
            let ending = newline(line);
            *line = format!("{assignment}{ending}");
            found = true;
        }
        if !line.trim().is_empty() {
            last_in_section = Some(index);
        }
    }
    if !found {
        match last_in_section {
            Some(index) => {
                if !lines[index].ends_with('\n') {
                    lines[index].push('\n');
                }
                lines.insert(index + 1, format!("{assignment}\n"));
            }
            None => {
                if let Some(last) = lines.last_mut() {
                    if !last.ends_with('\n') {
                        last.push('\n');
                    }
                }
                if !lines.is_empty() && !lines.last().is_some_and(|l| l.trim().is_empty()) {
                    lines.push("\n".into());
                }
                lines.push(format!("[{section}]\n{assignment}\n"));
            }
        }
    }
    lines.concat()
}

/// `config_home/gtk-3.0/settings.ini` and `config_home/gtk-4.0/settings.ini`.
pub fn gtk_ini_paths(config_home: &Path) -> [PathBuf; 2] {
    [config_home.join("gtk-3.0/settings.ini"), config_home.join("gtk-4.0/settings.ini")]
}

/// The icon theme a GTK `settings.ini` names.
pub fn read_gtk_ini(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    ini_get(&text, INI_SECTION, INI_KEY).filter(|v| !v.is_empty())
}

/// Set `gtk-icon-theme-name` in one `settings.ini` (created when missing),
/// replacing the file atomically and keeping its mode; a symlinked file is
/// written through to its target. Unchanged files are not rewritten.
pub fn write_gtk_ini(path: &Path, theme: &str) -> std::io::Result<()> {
    let target = if path.is_symlink() { std::fs::canonicalize(path)? } else { path.to_path_buf() };
    let current = match std::fs::read_to_string(&target) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let text = ini_set(current.as_deref().unwrap_or(""), INI_SECTION, INI_KEY, theme);
    if current.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mode = std::fs::metadata(&target).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644);
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&temporary, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Write the theme to both GTK `settings.ini` files under `config_home`.
pub fn write_gtk_inis(config_home: &Path, theme: &str) -> Result<(), String> {
    let mut errors = Vec::new();
    for path in gtk_ini_paths(config_home) {
        if let Err(error) = write_gtk_ini(&path, theme) {
            errors.push(crate::pyerr::os_error_path(&error, &path));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

/// Where the theme chosen in Global Settings is recorded: the shared GTK
/// settings alone cannot tell it from what another desktop wrote there.
pub fn choice_path(config_home: &Path) -> PathBuf {
    config_home.join("tessel/icon-theme")
}

/// The icon theme chosen in Global Settings, when one was.
pub fn read_choice(config_home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(choice_path(config_home)).ok()?;
    text.lines().next().map(str::trim).filter(|id| valid_id(id)).map(str::to_owned)
}

/// Record the icon theme chosen in Global Settings.
pub fn write_choice(config_home: &Path, theme: &str) -> Result<(), String> {
    let path = choice_path(config_home);
    if read_choice(config_home).as_deref() == Some(theme) {
        return Ok(());
    }
    let write = || {
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".icon-theme.{}.tmp", std::process::id()));
        std::fs::write(&temporary, format!("{theme}\n"))?;
        std::fs::rename(&temporary, &path).inspect_err(|_| { let _ = std::fs::remove_file(&temporary); })
    };
    write().map_err(|error: std::io::Error| crate::pyerr::os_error_path(&error, &path))
}

/// The shared `org.gnome.desktop.interface` settings when the schema has `icon-theme`.
pub fn interface_settings() -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.interface", true)?;
    schema.has_key(GSETTINGS_KEY).then(|| gio::Settings::new("org.gnome.desktop.interface"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("suite-icon-theme-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn theme(root: &Path, id: &str, index: &str) {
        std::fs::create_dir_all(root.join(id)).unwrap();
        std::fs::write(root.join(id).join("index.theme"), index).unwrap();
    }

    fn icons(name: &str) -> String {
        format!("[Icon Theme]\nName={name}\nComment=x\nDirectories=16x16/apps,48x48/places\n\n[16x16/apps]\nSize=16\n")
    }

    #[test]
    fn discovery_lists_icon_themes_once_by_name_and_skips_cursors_hicolor_default_and_mirrors() {
        let dir = scratch("discover");
        let (user, legacy, system) = (dir.join("data/icons"), dir.join("home/.icons"), dir.join("usr/icons"));
        theme(&system, "Papirus-Dark", &icons("Papirus-Dark"));
        theme(&system, "Adwaita", &icons("Adwaita"));
        theme(&system, "breeze_cursors", "[Icon Theme]\nName=Breeze Cursors\nInherits=x\n");
        theme(&system, "empty", "[Icon Theme]\nName=Empty\nDirectories=\n");
        theme(&system, "hicolor", &icons("Hicolor"));
        theme(&system, "default", "[Icon Theme]\nInherits=Adwaita\nDirectories=x\n");
        theme(&system, "Hidden", "[Icon Theme]\nName=Hid\nHidden=true\nDirectories=x\n");
        theme(&system, "zz", "[Other]\nDirectories=x\n");
        theme(&legacy, "Adwaita", &icons("Shadowed"));
        theme(&user, "Adwaita", &icons("Mine"));
        // The dock's private recoloured mirror under the same id is not a choice.
        theme(&user, "Papirus-Dark", &icons("Mirror"));
        std::fs::write(user.join("Papirus-Dark").join(MIRROR_MARKER), "{}").unwrap();
        theme(&legacy, "Nameless", "[Icon Theme]\nDirectories=a\n");
        std::fs::create_dir_all(system.join("noindex")).unwrap();
        let dirs = search_dirs_for(&dir.join("data"), &dir.join("home"), &[dir.join("usr")]);
        assert_eq!(dirs, [user.clone(), legacy.clone(), system.clone()]);
        let found = discover(&dirs);
        let summary: Vec<(&str, &str)> = found.iter().map(|t| (t.id.as_str(), t.name.as_str())).collect();
        assert_eq!(summary, [("Adwaita", "Mine"), ("Nameless", "Nameless"), ("Papirus-Dark", "Papirus-Dark")]);
        assert_eq!(found[2].dir, system.join("Papirus-Dark"));
        assert!(is_installed(&dirs, "Papirus-Dark"));
        assert!(is_installed(&dirs, "Adwaita"));
        assert!(!is_installed(&dirs, "breeze"));
        assert!(!is_installed(&dirs, "breeze_cursors"));
        assert!(!is_installed(&dirs, ""));
        assert!(!is_installed(&dirs, "../usr/icons/Adwaita"));
        // Adwaita marks itself Hidden=true but stays a choice; other hidden themes do not.
        let other = dir.join("other");
        theme(&other, "Adwaita", "[Icon Theme]\nName=Adwaita\nHidden=true\nDirectories=a\n");
        theme(&other, "AdwaitaLegacy", "[Icon Theme]\nName=AdwaitaLegacy\nHidden=true\nDirectories=a\n");
        assert_eq!(discover(&[other]).iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["Adwaita"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ini_set_replaces_in_place_and_keeps_every_other_line() {
        let text = "# my gtk settings\n[Settings]\ngtk-theme-name=Adwaita\n; old\ngtk-icon-theme-name = breeze\ngtk-font-name=Sans 10\n\n[Other]\ngtk-icon-theme-name=keep\n";
        let edited = ini_set(text, INI_SECTION, INI_KEY, "Papirus-Dark");
        assert_eq!(edited, "# my gtk settings\n[Settings]\ngtk-theme-name=Adwaita\n; old\ngtk-icon-theme-name=Papirus-Dark\ngtk-font-name=Sans 10\n\n[Other]\ngtk-icon-theme-name=keep\n");
        assert_eq!(ini_get(&edited, INI_SECTION, INI_KEY).as_deref(), Some("Papirus-Dark"));
        assert_eq!(ini_get(&edited, "Other", INI_KEY).as_deref(), Some("keep"));
        // Commented keys are not assignments.
        assert_eq!(ini_get("[Settings]\n#gtk-icon-theme-name=x\n", INI_SECTION, INI_KEY), None);
    }

    #[test]
    fn ini_set_adds_key_to_section_or_creates_the_section() {
        assert_eq!(ini_set("", INI_SECTION, INI_KEY, "A"), "[Settings]\ngtk-icon-theme-name=A\n");
        assert_eq!(ini_set("# only a comment", INI_SECTION, INI_KEY, "A"), "# only a comment\n\n[Settings]\ngtk-icon-theme-name=A\n");
        assert_eq!(ini_set("[Other]\nx=1\n", INI_SECTION, INI_KEY, "A"), "[Other]\nx=1\n\n[Settings]\ngtk-icon-theme-name=A\n");
        assert_eq!(ini_set("[Settings]\ngtk-theme-name=X\n\n[Other]\ny=2\n", INI_SECTION, INI_KEY, "A"),
                   "[Settings]\ngtk-theme-name=X\ngtk-icon-theme-name=A\n\n[Other]\ny=2\n");
        assert_eq!(ini_set("[Settings]\ngtk-theme-name=X", INI_SECTION, INI_KEY, "A"), "[Settings]\ngtk-theme-name=X\ngtk-icon-theme-name=A\n");
        assert_eq!(ini_set("[Settings]\n", INI_SECTION, INI_KEY, "A"), "[Settings]\ngtk-icon-theme-name=A\n");
    }

    #[test]
    fn gtk_ini_files_are_created_or_edited_atomically_with_their_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("ini");
        let [gtk3, gtk4] = gtk_ini_paths(&dir);
        std::fs::create_dir_all(gtk3.parent().unwrap()).unwrap();
        std::fs::write(&gtk3, "[Settings]\n# keep me\ngtk-icon-theme-name=breeze\ngtk-cursor-theme-name=Bibata\n").unwrap();
        std::fs::set_permissions(&gtk3, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_gtk_inis(&dir, "Papirus-Dark").unwrap();
        assert_eq!(std::fs::read_to_string(&gtk3).unwrap(),
                   "[Settings]\n# keep me\ngtk-icon-theme-name=Papirus-Dark\ngtk-cursor-theme-name=Bibata\n");
        assert_eq!(std::fs::metadata(&gtk3).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&gtk4).unwrap(), "[Settings]\ngtk-icon-theme-name=Papirus-Dark\n");
        assert_eq!(read_gtk_ini(&gtk4).as_deref(), Some("Papirus-Dark"));
        assert_eq!(std::fs::read_dir(gtk4.parent().unwrap()).unwrap().count(), 1, "no temporary left");
        // A symlinked settings.ini (dotfiles) stays a symlink.
        let real = dir.join("dotfiles.ini");
        std::fs::write(&real, "[Settings]\n").unwrap();
        std::fs::remove_file(&gtk4).unwrap();
        std::os::unix::fs::symlink(&real, &gtk4).unwrap();
        write_gtk_ini(&gtk4, "Adwaita").unwrap();
        assert!(gtk4.is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "[Settings]\ngtk-icon-theme-name=Adwaita\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn papirus_family_is_the_three_dock_variants() {
        assert!(PAPIRUS_FAMILY.iter().all(|t| is_papirus_family(t)));
        assert!(!is_papirus_family("ePapirus"));
        assert!(!is_papirus_family("Adwaita"));
    }

    #[test]
    fn the_theme_chosen_in_global_settings_is_recorded() {
        let config = scratch("choice");
        assert_eq!(read_choice(&config), None);
        write_choice(&config, "Tela").unwrap();
        assert_eq!(std::fs::read_to_string(config.join("tessel/icon-theme")).unwrap(), "Tela\n");
        assert_eq!(read_choice(&config).as_deref(), Some("Tela"));
        write_choice(&config, "Papirus-Dark").unwrap();
        assert_eq!(read_choice(&config).as_deref(), Some("Papirus-Dark"));
        // Nothing usable in the file is no choice.
        std::fs::write(choice_path(&config), "../etc\n").unwrap();
        assert_eq!(read_choice(&config), None);
        std::fs::write(choice_path(&config), "\n").unwrap();
        assert_eq!(read_choice(&config), None);
        let _ = std::fs::remove_dir_all(&config);
    }
}
