//! `tessel-icon-theme`: at login, fall back to the suite's default icon
//! theme (Papirus-Dark) when the configured one is not installed, e.g. after
//! its package was removed. A valid user choice is never changed.
//!
//! The configured theme is gsettings `org.gnome.desktop.interface icon-theme`,
//! else `gtk-icon-theme-name` in `~/.config/gtk-4.0/settings.ini`. A repair
//! writes gsettings and both GTK `settings.ini` files.
use std::path::PathBuf;

use tessel_core::icon_theme::{self as icons, DEFAULT};

use crate::cli::{Parsed, Parser};

/// The gsettings key (tests use a fake).
pub trait IconSetting {
    fn get(&self) -> Option<String>;
    fn set(&self, theme: &str) -> bool;
}

pub struct GioIconSetting(gio::Settings);

impl IconSetting for GioIconSetting {
    fn get(&self) -> Option<String> {
        use gio::prelude::*;
        Some(self.0.string(icons::GSETTINGS_KEY).to_string()).filter(|v| !v.is_empty())
    }

    fn set(&self, theme: &str) -> bool {
        use gio::prelude::*;
        let ok = self.0.set_string(icons::GSETTINGS_KEY, theme).is_ok();
        // This process exits right away: write the value out first.
        gio::Settings::sync();
        ok
    }
}

pub struct Environment {
    pub setting: Option<Box<dyn IconSetting>>,
    pub search_dirs: Vec<PathBuf>,
    pub config_home: PathBuf,
}

impl Environment {
    pub fn from_session() -> Self {
        Self {
            setting: icons::interface_settings().map(|s| Box::new(GioIconSetting(s)) as Box<dyn IconSetting>),
            search_dirs: icons::search_dirs(),
            config_home: tessel_core::paths::config_home(),
        }
    }

    /// gsettings, else the GTK 4 settings.ini.
    pub fn configured(&self) -> Option<String> {
        self.setting.as_ref().and_then(|s| s.get())
            .or_else(|| icons::read_gtk_ini(&icons::gtk_ini_paths(&self.config_home)[1]))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The configured theme is installed; nothing changed.
    Kept(String),
    /// The configured theme (if any) was missing; the default is set now
    /// (`Err` lists what could not be written).
    Repaired { missing: Option<String>, result: Result<(), String> },
    /// Would repair (`--check`).
    WouldRepair { missing: Option<String> },
    /// Neither the configured theme nor the default is installed.
    DefaultMissing { missing: Option<String> },
}

pub fn ensure(environment: &Environment, apply: bool) -> Outcome {
    let configured = environment.configured();
    if let Some(theme) = &configured {
        if icons::is_installed(&environment.search_dirs, theme) {
            return Outcome::Kept(theme.clone());
        }
    }
    if !icons::is_installed(&environment.search_dirs, DEFAULT) {
        return Outcome::DefaultMissing { missing: configured };
    }
    if !apply {
        return Outcome::WouldRepair { missing: configured };
    }
    let mut errors = Vec::new();
    if let Some(setting) = &environment.setting {
        if !setting.set(DEFAULT) {
            errors.push(format!("could not set gsettings {}", icons::GSETTINGS_KEY));
        }
    }
    if let Err(error) = icons::write_gtk_inis(&environment.config_home, DEFAULT) {
        errors.push(error);
    }
    Outcome::Repaired { missing: configured, result: if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) } }
}

pub fn parser(prog: &str) -> Parser {
    Parser {
        usage: format!("usage: {prog} [-h] [--check]"),
        help: format!("usage: {prog} [-h] [--check]

Use the default icon theme ({DEFAULT}) when the configured one is not installed.

options:
  -h, --help  show this help message and exit
  --check     only report what would change
"),
        flags: &["--check"],
        exclusive: false,
    }
}

fn describe(theme: &Option<String>) -> String {
    match theme {
        Some(theme) => format!("icon theme {theme:?} is not installed"),
        None => "no icon theme is configured".into(),
    }
}

/// Exit status: 0 kept/repaired/would repair, 1 write failure or no default.
pub fn main(args: &[String], prog: &str, environment: &Environment) -> i32 {
    let check = match parser(prog).parse(prog, args) {
        Parsed::Exit(code) => return code,
        Parsed::Flags(flags) => flags.contains("--check"),
    };
    match ensure(environment, !check) {
        Outcome::Kept(theme) => {
            println!("Icon theme {theme} is installed; nothing to do.");
            0
        }
        Outcome::WouldRepair { missing } => {
            println!("The {}; would use {DEFAULT}.", describe(&missing));
            0
        }
        Outcome::Repaired { missing, result: Ok(()) } => {
            println!("The {}; now using {DEFAULT}.", describe(&missing));
            0
        }
        Outcome::Repaired { missing, result: Err(error) } => {
            eprintln!("The {}; switching to {DEFAULT} failed: {error}", describe(&missing));
            1
        }
        Outcome::DefaultMissing { missing } => {
            eprintln!("The {} and the default {DEFAULT} is not installed either; nothing changed.", describe(&missing));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;

    #[derive(Default)]
    struct FakeSetting {
        value: RefCell<Option<String>>,
        writes: Rc<RefCell<Vec<String>>>,
        fail: bool,
    }

    impl IconSetting for FakeSetting {
        fn get(&self) -> Option<String> { self.value.borrow().clone() }
        fn set(&self, theme: &str) -> bool {
            self.writes.borrow_mut().push(theme.into());
            if !self.fail {
                self.value.replace(Some(theme.into()));
            }
            !self.fail
        }
    }

    struct Fixture {
        root: PathBuf,
        writes: Rc<RefCell<Vec<String>>>,
        environment: Environment,
    }

    fn fixture(name: &str, gsettings: Option<Option<&str>>, installed: &[&str]) -> Fixture {
        let root = std::env::temp_dir().join(format!("session-icon-theme-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let icons = root.join("share/icons");
        for theme in installed {
            std::fs::create_dir_all(icons.join(theme)).unwrap();
            std::fs::write(icons.join(theme).join("index.theme"), "[Icon Theme]\nName=x\nDirectories=48x48/places\n").unwrap();
        }
        let writes = Rc::new(RefCell::new(Vec::new()));
        let setting = gsettings.map(|value| Box::new(FakeSetting { value: RefCell::new(value.map(str::to_owned)),
                                                                     writes: writes.clone(), fail: false }) as Box<dyn IconSetting>);
        let environment = Environment { setting, search_dirs: vec![icons], config_home: root.join("config") };
        Fixture { root, writes, environment }
    }

    fn ini(root: &Path, version: &str) -> Option<String> {
        std::fs::read_to_string(root.join("config").join(format!("gtk-{version}/settings.ini"))).ok()
    }

    #[test]
    fn missing_theme_falls_back_to_papirus_dark_everywhere() {
        let f = fixture("missing", Some(Some("breeze")), &["Papirus-Dark", "Adwaita"]);
        std::fs::create_dir_all(f.root.join("config/gtk-3.0")).unwrap();
        std::fs::write(f.root.join("config/gtk-3.0/settings.ini"), "[Settings]\n# mine\ngtk-icon-theme-name=breeze\ngtk-theme-name=Adwaita\n").unwrap();
        assert_eq!(ensure(&f.environment, true), Outcome::Repaired { missing: Some("breeze".into()), result: Ok(()) });
        assert_eq!(*f.writes.borrow(), ["Papirus-Dark"]);
        assert_eq!(ini(&f.root, "3.0").unwrap(), "[Settings]\n# mine\ngtk-icon-theme-name=Papirus-Dark\ngtk-theme-name=Adwaita\n");
        assert_eq!(ini(&f.root, "4.0").unwrap(), "[Settings]\ngtk-icon-theme-name=Papirus-Dark\n");
        // The next login keeps it.
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("Papirus-Dark".into()));
        assert_eq!(f.writes.borrow().len(), 1);
    }

    #[test]
    fn installed_user_choice_is_never_overwritten() {
        let f = fixture("kept", Some(Some("Adwaita")), &["Papirus-Dark", "Adwaita"]);
        std::fs::create_dir_all(f.root.join("config/gtk-4.0")).unwrap();
        std::fs::write(f.root.join("config/gtk-4.0/settings.ini"), "[Settings]\ngtk-icon-theme-name=breeze\n").unwrap();
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("Adwaita".into()));
        assert!(f.writes.borrow().is_empty());
        assert_eq!(ini(&f.root, "4.0").unwrap(), "[Settings]\ngtk-icon-theme-name=breeze\n");
        assert_eq!(ini(&f.root, "3.0"), None);
    }

    #[test]
    fn without_gsettings_the_gtk4_ini_decides() {
        let f = fixture("ini", None, &["Papirus-Dark", "Adwaita"]);
        std::fs::create_dir_all(f.root.join("config/gtk-4.0")).unwrap();
        std::fs::write(f.root.join("config/gtk-4.0/settings.ini"), "[Settings]\ngtk-icon-theme-name=Adwaita\n").unwrap();
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("Adwaita".into()));
        std::fs::write(f.root.join("config/gtk-4.0/settings.ini"), "[Settings]\ngtk-icon-theme-name=gone\n").unwrap();
        assert_eq!(ensure(&f.environment, true), Outcome::Repaired { missing: Some("gone".into()), result: Ok(()) });
        assert_eq!(ini(&f.root, "4.0").unwrap(), "[Settings]\ngtk-icon-theme-name=Papirus-Dark\n");
        // An empty gsettings value also falls through to the ini.
        let f = fixture("empty", Some(None), &["Papirus-Dark"]);
        assert_eq!(ensure(&f.environment, true), Outcome::Repaired { missing: None, result: Ok(()) });
        assert_eq!(*f.writes.borrow(), ["Papirus-Dark"]);
    }

    #[test]
    fn check_only_reports_and_missing_default_changes_nothing() {
        let f = fixture("check", Some(Some("breeze")), &["Papirus-Dark"]);
        assert_eq!(ensure(&f.environment, false), Outcome::WouldRepair { missing: Some("breeze".into()) });
        assert!(f.writes.borrow().is_empty());
        assert_eq!(ini(&f.root, "4.0"), None);
        assert_eq!(main(&["--check".into()], "tessel-icon-theme", &f.environment), 0);
        assert!(f.writes.borrow().is_empty());
        let f = fixture("nodefault", Some(Some("breeze")), &["Adwaita"]);
        assert_eq!(ensure(&f.environment, true), Outcome::DefaultMissing { missing: Some("breeze".into()) });
        assert!(f.writes.borrow().is_empty());
        assert_eq!(ini(&f.root, "3.0"), None);
        assert_eq!(main(&[], "tessel-icon-theme", &f.environment), 1);
    }

    #[test]
    fn failed_gsettings_write_is_reported_but_ini_files_are_still_written() {
        let mut f = fixture("fail", None, &["Papirus-Dark"]);
        f.environment.setting = Some(Box::new(FakeSetting { value: RefCell::new(Some("breeze".into())), writes: f.writes.clone(), fail: true }));
        match ensure(&f.environment, true) {
            Outcome::Repaired { result: Err(error), .. } => assert!(error.contains("gsettings"), "{error}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(ini(&f.root, "4.0").unwrap(), "[Settings]\ngtk-icon-theme-name=Papirus-Dark\n");
        assert_eq!(main(&["--bogus".into()], "tessel-icon-theme", &f.environment), 2);
    }
}
