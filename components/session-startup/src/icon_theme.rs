//! `tessel-icon-theme`: at login, make sure Tessel has a usable icon theme.
//!
//! The configured theme is gsettings `org.gnome.desktop.interface icon-theme`,
//! else `gtk-icon-theme-name` in `~/.config/gtk-4.0/settings.ini`. It is
//! replaced in two cases:
//!
//! - it is not installed, e.g. after its package was removed;
//! - it is KDE's default (Breeze), which KDE writes into those shared settings
//!   at every Plasma login and which lacks many of Tessel's icons, and it was
//!   not chosen in Global Settings (`~/.config/tessel/icon-theme`).
//!
//! The replacement is the theme chosen in Global Settings when it is
//! installed, else the suite's default (Papirus-Dark). It is written to
//! gsettings and both GTK `settings.ini` files. Any other installed theme is
//! never changed.
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
    /// The configured theme is installed and stays.
    Kept(String),
    /// The configured theme (if any) was missing; `to` is set now
    /// (`Err` lists what could not be written).
    Repaired { missing: Option<String>, to: String, result: Result<(), String> },
    /// Would repair (`--check`).
    WouldRepair { missing: Option<String>, to: String },
    /// The configured theme was another desktop's default, not one chosen
    /// in Tessel; `to` is set now.
    Switched { from: String, to: String, result: Result<(), String> },
    /// Would switch (`--check`).
    WouldSwitch { from: String, to: String },
    /// Neither the configured theme nor the default is installed.
    DefaultMissing { missing: Option<String> },
}

fn write(environment: &Environment, theme: &str) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Some(setting) = &environment.setting {
        if !setting.set(theme) {
            errors.push(format!("could not set gsettings {}", icons::GSETTINGS_KEY));
        }
    }
    if let Err(error) = icons::write_gtk_inis(&environment.config_home, theme) {
        errors.push(error);
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

pub fn ensure(environment: &Environment, apply: bool) -> Outcome {
    let installed = |theme: &str| icons::is_installed(&environment.search_dirs, theme);
    let configured = environment.configured();
    // What Tessel uses when the configured theme is not to be: the theme
    // chosen in Global Settings, else the suite default.
    let choice = icons::read_choice(&environment.config_home).filter(|theme| installed(theme));
    let wanted = choice.clone().or_else(|| installed(DEFAULT).then(|| DEFAULT.to_owned()));
    if let Some(theme) = configured.clone().filter(|theme| installed(theme)) {
        // KDE writes its icon theme into the shared GTK settings at every
        // Plasma login. Its default has no icons for much of Tessel.
        let foreign = icons::KDE_DEFAULTS.contains(&theme.as_str()) && choice.as_deref() != Some(theme.as_str());
        return match wanted {
            Some(to) if foreign && !apply => Outcome::WouldSwitch { from: theme, to },
            Some(to) if foreign => {
                let result = write(environment, &to);
                Outcome::Switched { from: theme, to, result }
            }
            _ => Outcome::Kept(theme),
        };
    }
    let Some(to) = wanted else { return Outcome::DefaultMissing { missing: configured } };
    if !apply {
        return Outcome::WouldRepair { missing: configured, to };
    }
    let result = write(environment, &to);
    Outcome::Repaired { missing: configured, to, result }
}

pub fn parser(prog: &str) -> Parser {
    Parser {
        usage: format!("usage: {prog} [-h] [--check]"),
        help: format!("usage: {prog} [-h] [--check]

Use the icon theme chosen in Global Settings, else the default ({DEFAULT}), when
the configured one is not installed or is another desktop's default.

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
        Outcome::WouldRepair { missing, to } => {
            println!("The {}; would use {to}.", describe(&missing));
            0
        }
        Outcome::Repaired { missing, to, result: Ok(()) } => {
            println!("The {}; now using {to}.", describe(&missing));
            0
        }
        Outcome::Repaired { missing, to, result: Err(error) } => {
            eprintln!("The {}; switching to {to} failed: {error}", describe(&missing));
            1
        }
        Outcome::WouldSwitch { from, to } => {
            println!("Icon theme {from} is KDE's default and was not chosen in Global Settings; would use {to}.");
            0
        }
        Outcome::Switched { from, to, result: Ok(()) } => {
            println!("Icon theme {from} is KDE's default and was not chosen in Global Settings; now using {to}.");
            0
        }
        Outcome::Switched { from, to, result: Err(error) } => {
            eprintln!("Icon theme {from} is KDE's default and was not chosen in Global Settings; switching to {to} failed: {error}");
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
        assert_eq!(ensure(&f.environment, true), Outcome::Repaired { missing: Some("breeze".into()), to: "Papirus-Dark".into(), result: Ok(()) });
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
        assert_eq!(ensure(&f.environment, true), Outcome::Repaired { missing: Some("gone".into()), to: "Papirus-Dark".into(), result: Ok(()) });
        assert_eq!(ini(&f.root, "4.0").unwrap(), "[Settings]\ngtk-icon-theme-name=Papirus-Dark\n");
        // An empty gsettings value also falls through to the ini.
        let f = fixture("empty", Some(None), &["Papirus-Dark"]);
        assert_eq!(ensure(&f.environment, true), Outcome::Repaired { missing: None, to: "Papirus-Dark".into(), result: Ok(()) });
        assert_eq!(*f.writes.borrow(), ["Papirus-Dark"]);
    }

    #[test]
    fn check_only_reports_and_missing_default_changes_nothing() {
        let f = fixture("check", Some(Some("breeze")), &["Papirus-Dark"]);
        assert_eq!(ensure(&f.environment, false), Outcome::WouldRepair { missing: Some("breeze".into()), to: "Papirus-Dark".into() });
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

    fn choose(f: &Fixture, theme: &str) {
        icons::write_choice(&f.environment.config_home, theme).unwrap();
    }

    #[test]
    fn kdes_default_is_replaced_unless_it_was_chosen_in_global_settings() {
        // A Plasma login wrote Breeze into the shared settings.
        let f = fixture("kde", Some(Some("breeze-dark")), &["Papirus-Dark", "breeze-dark", "breeze", "Tela"]);
        assert_eq!(ensure(&f.environment, false), Outcome::WouldSwitch { from: "breeze-dark".into(), to: "Papirus-Dark".into() });
        assert!(f.writes.borrow().is_empty());
        assert_eq!(main(&["--check".into()], "tessel-icon-theme", &f.environment), 0);
        assert_eq!(ensure(&f.environment, true),
                   Outcome::Switched { from: "breeze-dark".into(), to: "Papirus-Dark".into(), result: Ok(()) });
        assert_eq!(*f.writes.borrow(), ["Papirus-Dark"]);
        assert_eq!(ini(&f.root, "4.0").unwrap(), "[Settings]\ngtk-icon-theme-name=Papirus-Dark\n");
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("Papirus-Dark".into()));
        // The theme chosen in Global Settings comes back after the next Plasma login.
        choose(&f, "Tela");
        f.environment.setting.as_ref().unwrap().set("breeze");
        assert_eq!(ensure(&f.environment, true), Outcome::Switched { from: "breeze".into(), to: "Tela".into(), result: Ok(()) });
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("Tela".into()));
        // Breeze chosen in Global Settings is a choice like any other.
        choose(&f, "breeze-dark");
        f.environment.setting.as_ref().unwrap().set("breeze-dark");
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("breeze-dark".into()));
        // The other Breeze is still KDE's, and goes back to the chosen one.
        f.environment.setting.as_ref().unwrap().set("breeze");
        assert_eq!(ensure(&f.environment, true), Outcome::Switched { from: "breeze".into(), to: "breeze-dark".into(), result: Ok(()) });
    }

    #[test]
    fn kdes_default_stays_when_nothing_better_is_installed_and_a_removed_choice_is_not_used() {
        let f = fixture("only", Some(Some("breeze-dark")), &["breeze-dark"]);
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("breeze-dark".into()));
        assert!(f.writes.borrow().is_empty());
        // The chosen theme's package is gone: the default, for a missing theme too.
        let f = fixture("gonechoice", Some(Some("breeze")), &["Papirus-Dark", "breeze"]);
        choose(&f, "Tela");
        assert_eq!(ensure(&f.environment, true), Outcome::Switched { from: "breeze".into(), to: "Papirus-Dark".into(), result: Ok(()) });
        let f = fixture("missingchoice", Some(Some("gone")), &["Papirus-Dark", "Tela"]);
        choose(&f, "Tela");
        assert_eq!(ensure(&f.environment, true), Outcome::Repaired { missing: Some("gone".into()), to: "Tela".into(), result: Ok(()) });
        // Any other installed theme is nobody's default to undo.
        let f = fixture("other", Some(Some("Tela")), &["Papirus-Dark", "Tela"]);
        assert_eq!(ensure(&f.environment, true), Outcome::Kept("Tela".into()));
        assert!(f.writes.borrow().is_empty());
    }
}
