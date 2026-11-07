//! Window-button side and style for GTK and labwc title bars (window_buttons.py).
//!
//! `apply(left, colored, shadows, rounded)` sets GSettings `button-layout` (through an
//! injectable [`ButtonLayoutStore`]), `gtk-decoration-layout` in existing
//! gtk-3.0/gtk-4.0 `settings.ini`, the rc.xml `<theme><titlebar><layout>` and
//! `<theme><dropShadows>`, `<theme><roundWindowContent>`, and the managed labwc theme's SVG buttons and
//! themerc. Each target is rewritten only when its content differs, and labwc
//! is asked to reload (asynchronously, coalesced, only under labwc) when a
//! labwc file changed. `set_palette` follows the GTK header-bar palette in
//! labwc title bars, debounced by 600 ms.

use super::fsx::{self, PathError};
use super::labwc_theme::{self, TitlebarColors};
use super::rc_xml;
use gio::prelude::*;
use std::cell::RefCell;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use tessel_core::scheduler::{GlibScheduler, Scheduler};
use tessel_gtk::palette::Palette;

pub const GTK_LAYOUTS: [&str; 2] = ["icon:minimize,maximize,close", "close,minimize,maximize:"];
pub const LABWC_LAYOUTS: [&str; 2] = ["icon:iconify,max,close", "close,iconify,max:"];
pub const WM_SCHEMA: &str = "org.gnome.desktop.wm.preferences";
pub const PALETTE_DELAY_MS: u32 = 600;

fn layout(table: [&'static str; 2], left: bool) -> &'static str {
    table[left as usize]
}

/// `_atomic_write`: keep the existing mode (else 0644), temporary + replace.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o644);
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| PathError::wrap(e, parent))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut file, temporary) = fsx::mkstemp(parent, &format!(".{name}-"), "")?;
    let result = (|| {
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.write_all(data)?;
        drop(file);
        fsx::replace(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// `with_ini_value`: replace the first `key=` line, else add it under `[Settings]`.
pub fn with_ini_value(text: &str, key: &str, value: &str) -> String {
    let pattern = regex::Regex::new(&format!(r"(?m)^{}\s*=.*$", regex::escape(key))).unwrap();
    let line = format!("{key}={value}");
    if pattern.is_match(text) {
        return pattern.replacen(text, 1, regex::NoExpand(&line)).into_owned();
    }
    if text.contains("[Settings]") {
        return text.replacen("[Settings]", &format!("[Settings]\n{line}"), 1);
    }
    format!("[Settings]\n{line}\n{text}")
}

/// `org.gnome.desktop.wm.preferences button-layout` (never touched in tests).
pub trait ButtonLayoutStore {
    /// `None` when the schema is not installed.
    fn get(&self) -> Option<String>;
    fn set(&self, value: &str) -> Result<(), String>;
}

/// The real GSettings key.
pub struct GioButtonLayout;

impl ButtonLayoutStore for GioButtonLayout {
    fn get(&self) -> Option<String> {
        let source = gio::SettingsSchemaSource::default()?;
        source.lookup(WM_SCHEMA, true)?;
        Some(gio::Settings::new(WM_SCHEMA).string("button-layout").to_string())
    }

    fn set(&self, value: &str) -> Result<(), String> {
        gio::Settings::new(WM_SCHEMA).set_string("button-layout", value).map_err(|e| e.to_string())?;
        gio::Settings::sync();
        Ok(())
    }
}

/// Ask labwc to reload without blocking GTK's main loop. Requests made while
/// a reload is running coalesce into one more reload after it finishes.
pub struct ReconfigureRequest(Rc<ReconfigureInner>);

struct ReconfigureInner {
    argv: Vec<String>,
    under_labwc: Box<dyn Fn() -> bool>,
    state: RefCell<ReconfigureState>,
}

#[derive(Default)]
struct ReconfigureState {
    process: Option<gio::Subprocess>,
    pending: bool,
    timeout: Option<tessel_core::timer::Timeout>,
}

pub const RECONFIGURE_TIMEOUT_MS: u32 = 5000;

impl Default for ReconfigureRequest {
    fn default() -> Self {
        Self::new(vec!["labwc".into(), "--reconfigure".into()],
                  Box::new(|| std::env::var_os("LABWC_PID").is_some_and(|v| !v.is_empty())))
    }
}

impl ReconfigureRequest {
    /// `under_labwc` replaces the `LABWC_PID` check (tests use a fake).
    pub fn new(argv: Vec<String>, under_labwc: Box<dyn Fn() -> bool>) -> Self {
        Self(Rc::new(ReconfigureInner { argv, under_labwc, state: RefCell::default() }))
    }

    pub fn request(&self) {
        if !(self.0.under_labwc)() {
            return;
        }
        let mut state = self.0.state.borrow_mut();
        if state.process.is_some() {
            state.pending = true;
            return;
        }
        drop(state);
        Self::start(&self.0);
    }

    pub fn pending(&self) -> bool {
        self.0.state.borrow().pending
    }

    pub fn running(&self) -> bool {
        self.0.state.borrow().process.is_some()
    }

    fn start(inner: &Rc<ReconfigureInner>) {
        let mut state = inner.state.borrow_mut();
        state.pending = false;
        let argv: Vec<&std::ffi::OsStr> = inner.argv.iter().map(std::ffi::OsStr::new).collect();
        let flags = gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE;
        let Ok(process) = gio::Subprocess::newv(&argv, flags) else {
            state.process = None;
            return;
        };
        state.process = Some(process.clone());
        let expire = process.clone();
        let weak = Rc::downgrade(inner);
        state.timeout = Some(tessel_core::timer::once(RECONFIGURE_TIMEOUT_MS, move || {
            if let Some(inner) = weak.upgrade() {
                inner.state.borrow_mut().timeout = None;
            }
            expire.force_exit();
        }));
        drop(state);
        let weak = Rc::downgrade(inner);
        let finished = process.clone();
        process.wait_async(None::<&gio::Cancellable>, move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let restart = {
                let mut state = inner.state.borrow_mut();
                if let Some(timeout) = state.timeout.take() {
                    timeout.cancel();
                }
                if state.process.as_ref() == Some(&finished) {
                    state.process = None;
                    state.pending
                } else {
                    false
                }
            };
            if restart {
                Self::start(&inner);
            }
        });
    }
}

/// Construction options; `None` uses the real locations and services.
#[derive(Default)]
pub struct WindowButtonsOptions {
    /// `$XDG_CONFIG_HOME` (gtk-3.0/gtk-4.0 settings.ini, labwc/rc.xml).
    pub config_home: Option<PathBuf>,
    pub labwc_rc: Option<PathBuf>,
    /// `$XDG_DATA_HOME` (labwc themes).
    pub data_home: Option<PathBuf>,
    /// palette.json read when no palette was set (labwc_titlebar_theme default).
    pub palette_path: Option<PathBuf>,
    pub reconfigure: Option<Box<dyn Fn()>>,
    pub scheduler: Option<Rc<dyn Scheduler>>,
    pub button_layout: Option<Box<dyn ButtonLayoutStore>>,
}

type PaletteKey = (bool, Option<Palette>);

struct Inner {
    config_home: PathBuf,
    labwc_rc: PathBuf,
    data_home: Option<PathBuf>,
    palette_path: Option<PathBuf>,
    reconfigure: Box<dyn Fn()>,
    scheduler: Rc<dyn Scheduler>,
    button_layout: Box<dyn ButtonLayoutStore>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    applied: Option<(bool, bool, bool, bool)>,
    error: Option<String>,
    palette: Option<PaletteKey>,
    themed: Option<PaletteKey>,
    source: Option<u64>,
}

#[derive(Clone)]
pub struct WindowButtons(Rc<Inner>);

impl WindowButtons {
    pub fn new(options: WindowButtonsOptions) -> Self {
        let config_home = options.config_home.unwrap_or_else(tessel_core::paths::config_home);
        let labwc_rc = options.labwc_rc.unwrap_or_else(|| config_home.join("labwc/rc.xml"));
        let reconfigure = options.reconfigure.unwrap_or_else(|| {
            let request = ReconfigureRequest::default();
            Box::new(move || request.request())
        });
        Self(Rc::new(Inner {
            config_home, labwc_rc, data_home: options.data_home, palette_path: options.palette_path, reconfigure,
            scheduler: options.scheduler.unwrap_or_else(|| Rc::new(GlibScheduler::default())),
            button_layout: options.button_layout.unwrap_or_else(|| Box::new(GioButtonLayout)),
            state: RefCell::default(),
        }))
    }

    pub fn error(&self) -> Option<String> {
        self.0.state.borrow().error.clone()
    }

    pub fn applied(&self) -> Option<(bool, bool, bool, bool)> {
        self.0.state.borrow().applied
    }

    /// Forget the applied choice (Python tests reset `applied = None`).
    pub fn reset_applied(&self) {
        self.0.state.borrow_mut().applied = None;
    }

    /// Follow the GTK header-bar palette in labwc title bars, debounced.
    pub fn set_palette(&self, is_dark: bool, palette: Option<&Palette>) -> bool {
        let value = Some((is_dark, palette.copied()));
        let previous = {
            let mut state = self.0.state.borrow_mut();
            if state.palette == value {
                return false;
            }
            state.palette = value;
            state.source.take()
        };
        if let Some(source) = previous {
            self.0.scheduler.cancel(source);
        }
        let weak = Rc::downgrade(&self.0);
        let id = self.0.scheduler.timeout(PALETTE_DELAY_MS, Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                WindowButtons(inner).palette_settled();
            }
        }));
        self.0.state.borrow_mut().source = Some(id);
        true
    }

    fn palette_settled(&self) {
        let (applied, palette) = {
            let mut state = self.0.state.borrow_mut();
            state.source = None;
            match state.applied {
                Some(applied) if state.palette != state.themed => (applied, state.palette),
                _ => return,
            }
        };
        match self.labwc_buttons(applied.1) {
            Ok(changed) => {
                if changed {
                    (self.0.reconfigure)();
                }
                self.0.state.borrow_mut().themed = palette;
            }
            Err(error) => self.0.state.borrow_mut().error = Some(error),
        }
    }

    pub fn close(&self) {
        let source = self.0.state.borrow_mut().source.take();
        if let Some(source) = source {
            self.0.scheduler.cancel(source);
        }
    }

    fn titlebar_colors(&self) -> TitlebarColors {
        match self.0.state.borrow().palette {
            None => labwc_theme::active_colors(self.0.palette_path.as_deref()),
            Some((is_dark, palette)) => labwc_theme::titlebar_colors(is_dark, palette.as_ref()),
        }
    }

    pub fn apply(&self, left: bool, colored: bool, shadows: bool, rounded: bool) -> bool {
        if self.0.state.borrow().applied == Some((left, colored, shadows, rounded)) {
            return false;
        }
        let mut changed = false;
        let mut reload = false;
        let mut errors = Vec::new();
        match self.gsettings(left) {
            Ok(value) => changed = value || changed,
            Err(error) => errors.push(error),
        }
        match self.settings_ini(left) {
            Ok(value) => changed = value || changed,
            Err(error) => errors.push(error),
        }
        match self.labwc_layout(left) {
            Ok(value) => {
                reload = value;
                match self.labwc_shadows(shadows) {
                    Ok(value) => reload = value || reload,
                    Err(error) => errors.push(error),
                }
                match self.labwc_rounded(rounded) {
                    Ok(value) => reload = value || reload,
                    Err(error) => errors.push(error),
                }
            }
            Err(error) => errors.push(error),
        }
        match self.labwc_buttons(colored) {
            Ok(value) => {
                reload = value || reload;
                let mut state = self.0.state.borrow_mut();
                state.themed = state.palette;
            }
            Err(error) => errors.push(error),
        }
        if reload {
            (self.0.reconfigure)();
        }
        let mut state = self.0.state.borrow_mut();
        state.error = if errors.is_empty() { None } else { Some(errors.join("; ")) };
        if errors.is_empty() {
            state.applied = Some((left, colored, shadows, rounded));
        }
        changed || reload
    }

    fn gsettings(&self, left: bool) -> Result<bool, String> {
        let wanted = layout(GTK_LAYOUTS, left);
        match self.0.button_layout.get() {
            None => Ok(false),
            Some(current) if current == wanted => Ok(false),
            Some(_) => self.0.button_layout.set(wanted).map(|()| true),
        }
    }

    fn settings_ini(&self, left: bool) -> Result<bool, String> {
        let mut changed = false;
        for version in ["gtk-3.0", "gtk-4.0"] {
            let path = self.0.config_home.join(version).join("settings.ini");
            if !path.is_file() {
                continue;
            }
            let text = fsx::read_text(&path).map_err(|e| fsx::error_text(&e))?;
            let updated = with_ini_value(&text, "gtk-decoration-layout", layout(GTK_LAYOUTS, left));
            if updated != text {
                atomic_write(&path, updated.as_bytes()).map_err(|e| fsx::error_text(&e))?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn read_rc(&self) -> Result<Option<Vec<u8>>, String> {
        if !self.0.labwc_rc.is_file() {
            return Ok(None);
        }
        fsx::read_bytes(&self.0.labwc_rc).map(Some).map_err(|e| fsx::error_text(&e))
    }

    fn labwc_layout(&self, left: bool) -> Result<bool, String> {
        let Some(data) = self.read_rc()? else { return Ok(false) };
        let wanted = layout(LABWC_LAYOUTS, left);
        let current = rc_xml::title_layout(&data)?.filter(|l| !l.is_empty()).unwrap_or_else(|| LABWC_LAYOUTS[0].into());
        if current == wanted {
            return Ok(false);
        }
        atomic_write(&self.0.labwc_rc, &rc_xml::with_title_layout(&data, wanted)?).map_err(|e| fsx::error_text(&e))?;
        Ok(true)
    }

    /// labwc draws drop-shadows for every window, client-decorated too.
    fn labwc_shadows(&self, enabled: bool) -> Result<bool, String> {
        let Some(data) = self.read_rc()? else { return Ok(false) };
        let wanted = if enabled { "yes" } else { "no" };
        let current = rc_xml::theme_value(&data, &["dropShadows"])?.filter(|v| !v.is_empty()).unwrap_or_else(|| "no".into());
        if current == wanted {
            return Ok(false);
        }
        atomic_write(&self.0.labwc_rc, &rc_xml::with_theme_value(&data, &["dropShadows"], wanted)?)
            .map_err(|e| fsx::error_text(&e))?;
        Ok(true)
    }

    /// labwc cuts window contents to its corner radius, for applications that
    /// draw square windows themselves (Spotify, browsers).
    fn labwc_rounded(&self, enabled: bool) -> Result<bool, String> {
        let Some(data) = self.read_rc()? else { return Ok(false) };
        let wanted = if enabled { "yes" } else { "no" };
        let current = rc_xml::theme_value(&data, &["roundWindowContent"])?.filter(|v| !v.is_empty()).unwrap_or_else(|| "no".into());
        if current == wanted {
            return Ok(false);
        }
        atomic_write(&self.0.labwc_rc, &rc_xml::with_theme_value(&data, &["roundWindowContent"], wanted)?)
            .map_err(|e| fsx::error_text(&e))?;
        Ok(true)
    }

    fn labwc_buttons(&self, colored: bool) -> Result<bool, String> {
        if !self.0.labwc_rc.is_file() {
            return Ok(false);
        }
        let name = labwc_theme::theme_name(&self.0.labwc_rc).map_err(|e| fsx::error_text(&e))?;
        let target = labwc_theme::theme_dir(&name, self.0.data_home.as_deref());
        let themerc = target.join("themerc");
        if !themerc.is_file() {
            return Ok(false); // Only restyle a theme the suite already manages.
        }
        let text = fsx::read_text(&themerc).map_err(|e| fsx::error_text(&e))?;
        let colors = self.titlebar_colors();
        let mut files = labwc_theme::button_files(colored, &colors);
        files.push(("themerc".into(), labwc_theme::updated_themerc(&text, &colors)));
        let mut changed = false;
        for (name, content) in files {
            let path = target.join(name);
            if !path.is_file() || std::fs::read(&path).ok().as_deref() != Some(content.as_bytes()) {
                atomic_write(&path, content.as_bytes()).map_err(|e| fsx::error_text(&e))?;
                changed = true;
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tessel_core::scheduler::FakeScheduler;
    use tessel_gtk::palette::color_palette;

    const RC: &[u8] = b"<?xml version=\"1.0\"?>
<labwc_config>
  <!-- keep me -->
  <theme>
    <name>GTK</name>
    <titleLayout>NLIMC</titleLayout>
    <active>
      <titlebar color=\"#282a36\"/>
    </active>
  </theme>
  <desktops number=\"4\"/>
</labwc_config>
";

    struct NoGsettings;
    impl ButtonLayoutStore for NoGsettings {
        fn get(&self) -> Option<String> { None }
        fn set(&self, _: &str) -> Result<(), String> { panic!("never write real gsettings") }
    }

    struct FakeGsettings(RefCell<String>, Rc<RefCell<Vec<String>>>);
    impl ButtonLayoutStore for FakeGsettings {
        fn get(&self) -> Option<String> { Some(self.0.borrow().clone()) }
        fn set(&self, value: &str) -> Result<(), String> {
            self.1.borrow_mut().push(value.into());
            *self.0.borrow_mut() = value.into();
            Ok(())
        }
    }

    fn root(name: &str) -> PathBuf {
        let base = std::env::temp_dir()
            .join(format!("d-buttons-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn ini_value_replaces_or_adds() {
        assert_eq!(with_ini_value("[Settings]\na=1\ngtk-decoration-layout=x\n", "gtk-decoration-layout", "y"),
                   "[Settings]\na=1\ngtk-decoration-layout=y\n");
        assert_eq!(with_ini_value("[Settings]\na=1\n", "gtk-decoration-layout", "y"), "[Settings]\ngtk-decoration-layout=y\na=1\n");
        assert_eq!(with_ini_value("a=1\n", "k", "v"), "[Settings]\nk=v\na=1\n");
    }

    #[test]
    fn apply_edits_files_once_and_reloads_labwc() {
        let config = root("apply");
        std::fs::create_dir_all(config.join("labwc")).unwrap();
        std::fs::write(config.join("labwc/rc.xml"), RC).unwrap();
        std::fs::create_dir_all(config.join("gtk-4.0")).unwrap();
        std::fs::write(config.join("gtk-4.0/settings.ini"), "[Settings]\ngtk-decoration-layout=icon:minimize,maximize,close\n").unwrap();
        let reloads = Rc::new(Cell::new(0));
        let counter = reloads.clone();
        let written = Rc::new(RefCell::new(Vec::new()));
        let buttons = WindowButtons::new(WindowButtonsOptions {
            config_home: Some(config.clone()), data_home: Some(config.join("data")),
            reconfigure: Some(Box::new(move || counter.set(counter.get() + 1))),
            scheduler: Some(Rc::new(FakeScheduler::default())),
            button_layout: Some(Box::new(FakeGsettings(RefCell::new(GTK_LAYOUTS[0].into()), written.clone()))),
            ..Default::default()
        });
        assert!(buttons.apply(true, false, false, false));
        assert!(!buttons.apply(true, false, false, false));
        let rc = std::fs::read(config.join("labwc/rc.xml")).unwrap();
        assert_eq!(rc_xml::title_layout(&rc).unwrap().as_deref(), Some(LABWC_LAYOUTS[1]));
        assert!(std::fs::read_to_string(config.join("gtk-4.0/settings.ini")).unwrap().contains("gtk-decoration-layout=close,minimize,maximize:"));
        assert_eq!(*written.borrow(), [GTK_LAYOUTS[1]]);
        assert_eq!(reloads.get(), 1);
        buttons.reset_applied();
        assert!(!buttons.apply(true, false, false, false), "already matching on disk");
        assert_eq!(reloads.get(), 1);
        assert!(buttons.error().is_none());
    }

    #[test]
    fn rounded_corners_reach_labwc_for_applications_that_draw_square_windows() {
        let config = root("rounded");
        std::fs::create_dir_all(config.join("labwc")).unwrap();
        std::fs::write(config.join("labwc/rc.xml"), RC).unwrap();
        let reloads = Rc::new(Cell::new(0));
        let counter = reloads.clone();
        let buttons = WindowButtons::new(WindowButtonsOptions {
            config_home: Some(config.clone()), data_home: Some(config.join("data")),
            reconfigure: Some(Box::new(move || counter.set(counter.get() + 1))),
            scheduler: Some(Rc::new(FakeScheduler::default())), button_layout: Some(Box::new(NoGsettings)),
            ..Default::default()
        });
        let value = || rc_xml::theme_value(&std::fs::read(config.join("labwc/rc.xml")).unwrap(), &["roundWindowContent"]).unwrap();
        assert!(buttons.apply(false, false, false, true));
        assert_eq!(value().as_deref(), Some("yes"));
        assert_eq!(reloads.get(), 1);
        assert!(!buttons.apply(false, false, false, true), "nothing to do");
        assert_eq!(reloads.get(), 1);
        assert!(buttons.apply(false, false, false, false));
        assert_eq!(value().as_deref(), Some("no"));
        assert_eq!(reloads.get(), 2);
    }

    #[test]
    fn malformed_rc_reports_error_and_keeps_file() {
        let config = root("malformed");
        std::fs::create_dir_all(config.join("labwc")).unwrap();
        std::fs::write(config.join("labwc/rc.xml"), b"<labwc_config><theme></labwc_config>").unwrap();
        let buttons = WindowButtons::new(WindowButtonsOptions {
            config_home: Some(config.clone()), data_home: Some(config.join("data")), reconfigure: Some(Box::new(|| panic!("no reload"))),
            scheduler: Some(Rc::new(FakeScheduler::default())), button_layout: Some(Box::new(NoGsettings)), ..Default::default()
        });
        assert!(!buttons.apply(true, false, true, false));
        assert!(buttons.error().is_some());
        assert!(buttons.applied().is_none());
        assert_eq!(std::fs::read(config.join("labwc/rc.xml")).unwrap(), b"<labwc_config><theme></labwc_config>");
    }

    #[test]
    fn palette_changes_regenerate_labwc_theme_once_per_burst() {
        let home = root("palette");
        let config = home.join("config");
        std::fs::create_dir_all(config.join("labwc")).unwrap();
        std::fs::write(config.join("labwc/rc.xml"), fsx::replace_bytes(RC, b"<name>GTK</name>", b"<name>Suite</name>")).unwrap();
        let theme = home.join("data/themes/Suite/openbox-3");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("themerc"), "window.active.title.bg.color: #222226\n").unwrap();
        let reloads = Rc::new(Cell::new(0));
        let counter = reloads.clone();
        let scheduler = Rc::new(FakeScheduler::default());
        let buttons = WindowButtons::new(WindowButtonsOptions {
            config_home: Some(config.clone()), data_home: Some(home.join("data")),
            palette_path: Some(home.join("cache/tessel-dock/palette.json")),
            reconfigure: Some(Box::new(move || counter.set(counter.get() + 1))),
            scheduler: Some(scheduler.clone()), button_layout: Some(Box::new(NoGsettings)), ..Default::default()
        });
        let run = |scheduler: &FakeScheduler| for id in scheduler.ids() { scheduler.fire(id); };
        let (dark, light) = (color_palette(true, Some([20, 26, 40])), color_palette(false, Some([20, 26, 40])));
        assert!(buttons.set_palette(true, Some(&dark)));
        buttons.apply(false, false, true, false);
        assert_eq!(reloads.get(), 1);
        let values = labwc_theme::themerc_values(&labwc_theme::titlebar_colors(true, Some(&dark)));
        let title = format!("window.active.title.bg.color: {}", labwc_theme::themerc_value(&values, "window.active.title.bg.color").unwrap());
        assert!(std::fs::read_to_string(theme.join("themerc")).unwrap().contains(&title));
        run(&scheduler); // Already themed by apply(): no second reload.
        assert_eq!(reloads.get(), 1);
        buttons.set_palette(false, Some(&color_palette(false, None)));
        buttons.set_palette(false, Some(&light));
        assert_eq!(scheduler.ids().len(), 1);
        assert!(!buttons.set_palette(false, Some(&light)));
        assert_eq!(reloads.get(), 1, "Nothing reloads before the burst settles");
        run(&scheduler);
        assert_eq!(reloads.get(), 2);
        let text = std::fs::read_to_string(theme.join("themerc")).unwrap();
        let light_values = labwc_theme::themerc_values(&labwc_theme::titlebar_colors(false, Some(&light)));
        for key in ["window.active.title.bg.color", "window.inactive.title.bg.color", "menu.items.bg.color"] {
            assert!(text.contains(&format!("{key}: {}", labwc_theme::themerc_value(&light_values, key).unwrap())));
        }
        let close = std::fs::read_to_string(theme.join("close-active.svg")).unwrap();
        assert!(close.contains(labwc_theme::themerc_value(&light_values, "window.active.button.unpressed.image.color").unwrap()));
        buttons.set_palette(true, Some(&dark));
        buttons.close();
        assert!(scheduler.ids().is_empty());
    }

    fn wait(context: &glib::MainContext, request: &ReconfigureRequest) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (request.running() || request.pending()) && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn reload_runs_off_the_main_loop_and_coalesces_requests() {
        let folder = root("reload");
        let log = folder.join("runs");
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let request = ReconfigureRequest::new(
                vec!["/bin/sh".into(), "-c".into(), format!("sleep 0.2; echo run >> {}", log.display())], Box::new(|| true));
            let started = std::time::Instant::now();
            for _ in 0..5 {
                request.request();
            }
            assert!(started.elapsed() < std::time::Duration::from_millis(150), "Requests must not wait for labwc");
            assert!(request.pending());
            wait(&context, &request);
        }).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap().matches("run").count(), 2);
    }

    #[test]
    fn no_reload_outside_labwc() {
        let request = ReconfigureRequest::new(vec!["/bin/false".into()], Box::new(|| false));
        request.request();
        assert!(!request.running());
    }
}
