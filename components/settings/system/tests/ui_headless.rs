//! GTK checks of the system pages (test_system_ui.py, test_input_ui.py and the
//! workspace controls) in the private headless labwc of
//! `components/settings/tests/headless.sh`. Every file the pages write
//! is a scratch copy; every runner, device list, polling backend and account
//! service is a fake. Under plain `cargo test` it skips itself.
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gs_base::{Host, PageHost, Setting, SettingsPage};
use gtk::prelude::*;
use gtk::{gio, glib};

use gs_system::about_page::{AboutPage, AboutValues, RESTART_COMMAND};
use gs_system::accessibility_page::{settings, AccessibilityBackend, AccessibilityPage, INTERFACE};
use gs_system::compat::Result as PyResult;
use gs_system::default_apps_page::{App, DefaultApps, DefaultAppsPage, Done, CATEGORIES};
use gs_system::input_config::{update, InputConfig, Updates};
use gs_system::input_devices::{parse_devices, InputDevice};
use gs_system::input_page::InputControls;
use gs_system::keyboard_config::{Catalog, KeyboardConfig};
use gs_system::keyboard_page::{KeyboardPage, OskEmbed};
use gs_system::mouse_polling::{Polling, PollingState};
use gs_system::night_light::NightLightStore;
use gs_system::night_light_page::NightLightPage;
use gs_system::shortcuts_model::{ShortcutStore, Shortcuts, TilingBinding};
use gs_system::shortcuts_page::{CheatSheet, RebindDialog, ShortcutsPage};
use gs_system::users_backend::{Account, Accounts};
use gs_system::users_page::{PasswordDialog, UsersPage};
use gs_system::workspace_config::WorkspaceConfig;
use gs_system::workspace_settings::WorkspaceControls;

const RC: &str = r#"<?xml version="1.0"?>
<!-- user comment -->
<labwc_config>
  <core><hdr>no</hdr></core>
  <keyboard>
    <default/>
    <repeatRate>30</repeatRate>
    <keybind key="W-d"><action name="ToggleShowDesktop"/></keybind>
    <keybind key="W-l"><action name="Execute"><command>/home/u/.local/bin/tessel-shell-lock</command></action></keybind>
    <keybind key="A-F4"/>
    <!-- Tessel Tiling keyboard begin -->
    <keybind key="W-t"><action name="Tiling" command="float"/></keybind>
    <!-- Tessel Tiling keyboard end -->
  </keyboard>
  <mouse><default/></mouse>
</labwc_config>
"#;

const INPUT: &str = r#"<?xml version="1.0"?>
<!-- Keep the rest of this file byte for byte. -->
<labwc_config>
  <mouse><default/><context name="Root"><mousebind button="Right" action="Press"/></context></mouse>
  <libinput>
    <!-- Device names and unknown options must survive. -->
    <device category="default"><leftHanded>no</leftHanded></device>
    <device category="non-touch" pointerSpeed="0.2"><accelProfile>flat</accelProfile><futureOption>yes</futureOption></device>
    <device category="touchpad"><tap>yes</tap><scrollMethod>twofinger</scrollMethod></device>
  </libinput>
  <hdrOutputs><output name="DP-2"/></hdrOutputs>
</labwc_config>
"#;

const LAPTOP: &str = "Device:                  SNSL002D:00 2C2F:002D Touchpad
Kernel:                  /dev/input/event6
Capabilities:            pointer gesture
Tap-to-click:            disabled
Tap-and-drag:            enabled
Tap button map:          left/right/middle
Tap drag lock:           disabled
Left-handed:             disabled
Nat.scrolling:           disabled
Middle emulation:        disabled
Scroll methods:          *two-finger edge
Scroll button:           n/a
Click methods:           *button-areas clickfinger
Disable-w-typing:        enabled
Accel profiles:          flat *adaptive custom

Device:                  TPPS/2 Elan TrackPoint
Kernel:                  /dev/input/event4
Capabilities:            pointer
Tap-to-click:            n/a
Scroll methods:          *button
Scroll button:           BTN_MIDDLE
Click methods:           none
Accel profiles:          flat *adaptive custom

Device:                  AT Translated Set 2 keyboard
Kernel:                  /dev/input/event3
Capabilities:            keyboard
Tap-to-click:            n/a
";

fn tiling() -> Vec<TilingBinding> {
    vec![("W-t".into(), "Super + T".into(), "Toggle this window between tiled and floating".into(), "float".into()),
         ("W-1".into(), "Super + 1".into(), "Switch to workspace 1".into(), "workspace 1".into())]
}

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait(condition: impl Fn() -> bool, seconds: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end && !condition() {
        settle(0.02);
    }
    condition()
}

fn texts(widget: &impl IsA<gtk::Widget>) -> Vec<String> {
    let mut found = Vec::new();
    fn walk(node: &gtk::Widget, found: &mut Vec<String>) {
        if let Some(label) = node.downcast_ref::<gtk::Label>() {
            found.push(label.text().to_string());
        }
        let mut child = node.first_child();
        while let Some(item) = child {
            walk(&item, found);
            child = item.next_sibling();
        }
    }
    walk(widget.upcast_ref(), &mut found);
    found
}

fn read(path: &Path) -> String { std::fs::read_to_string(path).unwrap_or_default() }

struct TestHost {
    window: gtk::ApplicationWindow,
    app: gtk::Application,
}

impl PageHost for TestHost {
    fn show_section(&self, _section: &str) {}
    fn application(&self) -> Option<gtk::Application> { Some(self.app.clone()) }
    fn window(&self) -> gtk::Window { self.window.clone().upcast() }
    fn show_page_error(&self, _section: &str, _error: &str, _title: Option<&str>, _icon: &str) {}
}

struct Base {
    root: PathBuf,
    app: gtk::Application,
    window: gtk::ApplicationWindow,
    host: Host,
    pages: RefCell<Vec<SettingsPage>>,
}

impl Base {
    fn new(app: &gtk::Application, name: &str) -> Base {
        let root = PathBuf::from(std::env::var("TMPDIR").unwrap()).join("ui").join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // SAFETY: single-threaded test runner.
        unsafe { std::env::set_var("XDG_STATE_HOME", root.join("state")) };
        let window = gtk::ApplicationWindow::builder().application(app).default_width(860).default_height(900).build();
        window.present();
        let host: Host = Rc::new(TestHost { window: window.clone(), app: app.clone() });
        Base { root, app: app.clone(), window, host, pages: RefCell::new(Vec::new()) }
    }

    fn show(&self, page: SettingsPage) -> SettingsPage {
        self.window.set_child(Some(&page.widget));
        settle(0.1);
        page
    }

    fn keep(&self, page: SettingsPage) { self.pages.borrow_mut().push(page) }
}

impl Drop for Base {
    fn drop(&mut self) {
        for page in self.pages.borrow().iter() {
            page.close();
        }
        self.window.destroy();
        settle(0.05);
    }
}

macro_rules! check {
    ($cond:expr) => { if !$cond { return Err(format!("{}:{}: check failed: {}", file!(), line!(), stringify!($cond))); } };
    ($cond:expr, $($arg:tt)+) => { if !$cond { return Err(format!("{}:{}: {}", file!(), line!(), format!($($arg)+))); } };
}

type Case = fn(&gtk::Application) -> Result<(), String>;

// ---- keyboard ------------------------------------------------------------

fn keyboard_layout_edits_and_repeat_are_saved_to_private_files(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "keyboard");
    let (rc, env) = (base.root.join("rc.xml"), base.root.join("environment"));
    std::fs::write(&rc, RC).unwrap();
    std::fs::write(&env, "XKB_DEFAULT_LAYOUT=us\nOTHER=1\n").unwrap();
    let reloads = Rc::new(RefCell::new(0));
    let r = reloads.clone();
    let config = KeyboardConfig::new(Some(&rc), Some(&env), Some(Rc::new(move |done: Option<gs_system::labwc_settings::Done>| {
        *r.borrow_mut() += 1;
        if let Some(done) = done { done(true, String::new()) }
    })));
    let catalog = Catalog::new(&[("us", "English (US)"), ("de", "German")], &[("de", &[("nodeadkeys", "German (no dead keys)")])],
                               &[("grp", &[("grp:alt_shift_toggle", "Alt+Shift")]), ("compose", &[("compose:ralt", "Right Alt")])]);
    let osk = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let factory = Box::new(move || Ok(OskEmbed { widget: osk.upcast(), search_targets: vec![("keyboard-layout".into(), gtk::Label::new(None).upcast())],
                                                 close: Box::new(|| {}) }));
    let page = base.show(gs_system::keyboard_page::build_page_with(Some(config), Some(catalog), Some(factory)));
    let c = page.controller::<KeyboardPage>().unwrap();
    check!(page.search_targets.borrow().contains_key("layout"));
    check!(page.search_targets.borrow().contains_key("keyboard-repeat-delay"));
    c.add_layout(("de".into(), "nodeadkeys".into()));
    check!(read(&env).contains("XKB_DEFAULT_LAYOUT=us,de"), "{}", read(&env));
    check!(read(&env).contains("XKB_DEFAULT_VARIANT=,nodeadkeys"));
    check!(texts(&c.layout_list).contains(&"German (no dead keys)".to_string()));
    c.move_layout(1, -1);
    check!(read(&env).contains("XKB_DEFAULT_LAYOUT=de,us"));
    c.switch.set_selected(1);
    c.compose.set_selected(1);
    check!(read(&env).contains("XKB_DEFAULT_OPTIONS=grp:alt_shift_toggle,compose:ralt"), "{}", read(&env));
    c.delay.set_value(300.0);
    check!(wait(|| read(&rc).contains("<repeatDelay>300</repeatDelay>"), 3.0));
    c.remove_layout(0);
    check!(read(&env).contains("XKB_DEFAULT_LAYOUT=us\n"));
    check!(read(&env).contains("OTHER=1"));
    check!(*reloads.borrow() > 0);
    check!(c.status.text().contains("Saved"));
    base.keep(page);
    Ok(())
}

fn keyboard_numlock_switch_reflects_and_writes_rc(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "numlock");
    let (rc, env) = (base.root.join("rc.xml"), base.root.join("environment"));
    let reloads = Rc::new(RefCell::new(0));
    let open = |text: &str| {
        std::fs::write(&rc, text).unwrap();
        let r = reloads.clone();
        let config = KeyboardConfig::new(Some(&rc), Some(&env), Some(Rc::new(move |done: Option<gs_system::labwc_settings::Done>| {
            *r.borrow_mut() += 1;
            if let Some(done) = done { done(true, String::new()) }
        })));
        let factory: gs_system::keyboard_page::OskFactory = Box::new(|| Err("none".to_owned()));
        base.show(gs_system::keyboard_page::build_page_with(Some(config), Some(Catalog::default()), Some(factory)))
    };
    let off = RC.replace("  <keyboard>\n", "  <keyboard>\n    <numlock>off</numlock>\n");
    let page = open(&off);
    let c = page.controller::<KeyboardPage>().unwrap();
    check!(page.search_targets.borrow().contains_key("keyboard-numlock"));
    check!(!c.numlock.is_active(), "off in the file shows off");
    check!(*reloads.borrow() == 0, "opening the page writes nothing");
    c.numlock.set_active(true);
    check!(read(&rc) == off.replace("<numlock>off</numlock>", "<numlock>on</numlock>"), "{}", read(&rc));
    check!(*reloads.borrow() == 1);
    check!(c.status.text().contains("Saved"));
    c.numlock.set_active(false);
    check!(read(&rc) == off, "{}", read(&rc));
    check!(*reloads.borrow() == 2);
    check!(!env.exists(), "Num Lock does not touch the environment file");
    page.close();
    let page = open(RC);
    let c = page.controller::<KeyboardPage>().unwrap();
    check!(c.numlock.is_active(), "missing shows on (session startup adds it)");
    check!(read(&rc) == RC && *reloads.borrow() == 2);
    c.numlock.set_active(false);
    check!(read(&rc).contains("<keyboard>\n    <numlock>off</numlock>\n    <default/>"), "{}", read(&rc));
    check!(*reloads.borrow() == 3);
    base.keep(page);
    Ok(())
}

// ---- shortcuts -------------------------------------------------------------

fn store(rc: &Path) -> Rc<dyn Shortcuts> {
    let mut store = ShortcutStore::new(Some(rc), Some(Rc::new(|done: Option<gs_system::labwc_settings::Done>| {
        if let Some(done) = done { done(true, String::new()) }
    })));
    store.tiling = Some(tiling());
    Rc::new(store)
}

fn shortcuts_listing_search_conflicts_and_rebinding(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "shortcuts");
    let rc = base.root.join("rc.xml");
    std::fs::write(&rc, RC).unwrap();
    let page = base.show(gs_system::shortcuts_page::build_page_with(Some(store(&rc))));
    let c = page.controller::<ShortcutsPage>().unwrap();
    check!(texts(&c.container).contains(&"Lock screen".to_string()));
    check!(page.search_targets.borrow().contains_key("search"));
    c.search.set_text("lock");
    settle(0.4);
    let visible: Vec<String> = c.list.borrow().as_ref().unwrap().visible().iter().map(|s| s.key.clone()).collect();
    check!(visible == ["W-l"], "{visible:?}");
    let lock = c.list.borrow().as_ref().unwrap().visible()[0].clone();
    let dialog = RebindDialog::new(Some(base.window.upcast_ref()), &lock, &c.shortcuts.borrow(), c.apply_callback());
    check!(!dialog.check("W-t").0);
    check!(dialog.check("W-t").1.contains("Toggle this window"));
    check!(dialog.check("W-a").0);
    check!(!dialog.check("W-l").0);
    dialog.set("W-C-l", false);
    check!(dialog.save.is_sensitive());
    check!(dialog.entry.text() == "W-C-l");
    dialog.apply_key();
    settle(0.05);
    check!(read(&rc).contains("key=\"W-C-l\""));
    check!(c.shortcuts.borrow().iter().any(|s| s.key == "W-C-l"));
    check!(c.status.text().contains("Shortcut changed to"), "{}", c.status.text());
    base.keep(page);
    Ok(())
}

/// An rc.xml as tessel-configure-session writes it, plus a keybind of the user's own.
fn suite_rc() -> String {
    use tessel_core::session_keys::{keybindings, BLOCK_BEGIN, BLOCK_END};
    let mut lines = vec![format!("{BLOCK_BEGIN}: generated by tessel-configure-session."),
                         "     Keys changed in Global Settings are kept on reinstall. -->".to_owned()];
    for (key, name, attribute, value) in keybindings() {
        lines.push(format!("<keybind key=\"{key}\"><action name=\"{name}\" {attribute}={}/></keybind>",
                           gs_system::labwc_settings::quoteattr(&value)));
    }
    lines.push(BLOCK_END.to_owned());
    format!("<?xml version=\"1.0\"?>\n<labwc_config>\n  <keyboard>\n    <default/>\n    {}\n    \
             <keybind key=\"W-e\"><action name=\"Execute\" command=\"foo\"/></keybind>\n  </keyboard>\n</labwc_config>\n",
            lines.join("\n    "))
}

fn rekey(text: &str, key: &str, new_key: &str) -> String {
    text.replacen(&format!("<keybind key=\"{key}\">"), &format!("<keybind key=\"{new_key}\">"), 1)
}

fn undo_buttons(widget: &impl IsA<gtk::Widget>) -> usize {
    fn walk(node: &gtk::Widget) -> usize {
        let own = node.downcast_ref::<gtk::Button>().is_some_and(|b| b.icon_name().as_deref() == Some("edit-undo-symbolic")) as usize;
        let mut count = own;
        let mut child = node.first_child();
        while let Some(item) = child {
            count += walk(&item);
            child = item.next_sibling();
        }
        count
    }
    walk(widget.upcast_ref())
}

fn shortcuts_reset_one_or_all_to_the_shipped_keys(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "shortcuts-reset");
    let rc = base.root.join("rc.xml");
    let shipped = suite_rc();
    std::fs::write(&rc, &shipped).unwrap();
    let page = base.show(gs_system::shortcuts_page::build_page_with(Some(store(&rc))));
    let c = page.controller::<ShortcutsPage>().unwrap();
    let resets = || c.list.borrow().as_ref().unwrap().resets.borrow().clone();
    check!(!c.reset_all.is_sensitive(), "nothing differs from the defaults");
    check!(resets().is_empty() && undo_buttons(&c.container) == 0);
    check!(page.search_targets.borrow().contains_key("shortcuts-reset"));
    std::fs::write(&rc, rekey(&rekey(&shipped, "W-Return", "C-A-t"), "W-l", "W-C-S-l")).unwrap();
    c.reload();
    let keys: Vec<String> = resets().iter().map(|(_, s)| s.key.clone()).collect();
    check!(keys.len() == 2 && keys.contains(&"C-A-t".to_string()) && keys.contains(&"W-C-S-l".to_string()), "{keys:?}");
    check!(undo_buttons(&c.container) == 2);
    check!(c.reset_all.is_sensitive());
    let (button, terminal) = resets().into_iter().find(|(_, s)| s.key == "C-A-t").unwrap();
    check!(button.tooltip_text().as_deref() == Some("Reset to Super + Enter"), "{:?}", button.tooltip_text());
    // The dialog names the default; the user's own shortcut has none.
    let dialog = RebindDialog::new(Some(base.window.upcast_ref()), &terminal, &c.shortcuts.borrow(), c.apply_callback());
    check!(dialog.default.as_ref().map(|l| l.text().to_string()).as_deref() == Some("Default: Super + Enter"));
    check!(texts(&dialog.window).contains(&"Default: Super + Enter".to_string()));
    dialog.window.close();
    let own = c.shortcuts.borrow().iter().find(|s| s.key == "W-e").cloned().unwrap();
    let dialog = RebindDialog::new(Some(base.window.upcast_ref()), &own, &c.shortcuts.borrow(), c.apply_callback());
    check!(dialog.default.is_none());
    dialog.window.close();
    // The read-only cheat sheet has no reset buttons.
    let sheet = CheatSheet::with_store(&base.app, Some(store(&rc)));
    sheet.present();
    settle(0.1);
    check!(undo_buttons(&sheet.window()) == 0);
    sheet.close();
    // One row.
    button.emit_clicked();
    settle(0.05);
    check!(read(&rc).contains("<keybind key=\"W-Return\">") && !read(&rc).contains("C-A-t"));
    check!(c.status.text() == "Shortcut changed to Super + Enter.", "{}", c.status.text());
    check!(resets().len() == 1);
    // All, after confirmation; Cancel first leaves the file as it is.
    let answers = Rc::new(RefCell::new(vec![0, 1]));
    let details = Rc::new(RefCell::new(Vec::<String>::new()));
    let (a, d) = (answers.clone(), details.clone());
    gs_base::style::set_chooser(Some(Rc::new(move |_m, detail, _b, callback, _default, _cancel| {
        d.borrow_mut().push(detail.to_owned());
        let answer = a.borrow_mut().remove(0);
        callback(Some(answer));
    })));
    let result = (|| {
        let before = read(&rc);
        c.reset_all.emit_clicked();
        settle(0.05);
        check!(read(&rc) == before);
        c.reset_all.emit_clicked();
        settle(0.05);
        check!(details.borrow().iter().all(|d| d.contains("Shortcuts you added yourself are kept.")));
        check!(read(&rc) == shipped);
        check!(c.status.text() == "1 shortcut was reset.", "{}", c.status.text());
        check!(!c.reset_all.is_sensitive());
        check!(resets().is_empty());
        Ok(())
    })();
    gs_base::style::set_chooser(None);
    base.keep(page);
    result
}

fn cheat_sheet_filters_and_closes(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "sheet");
    let rc = base.root.join("rc.xml");
    std::fs::write(&rc, RC).unwrap();
    let sheet = CheatSheet::with_store(&base.app, Some(store(&rc)));
    sheet.present();
    settle(0.1);
    check!(!sheet.lists().is_empty());
    check!(sheet.window().title().as_deref() == Some("Keyboard Shortcuts"));
    sheet.search().set_text("zzzz-no-such-shortcut");
    settle(0.4);
    check!(!sheet.lists().iter().any(|c| c.parent().is_some_and(|p| p.is_visible())));
    sheet.close();
    settle(0.05);
    check!(sheet.closed());
    let plain = CheatSheet::new(&base.app);
    plain.present();
    settle(0.05);
    plain.close();
    settle(0.05);
    check!(plain.closed());
    Ok(())
}

// ---- rebind dialog vs compositor keybinds ----------------------------------

/// The private compositor's rc.xml; the original comes back on drop.
struct Compositor {
    pid: i32,
    rc: PathBuf,
    original: String,
}

impl Compositor {
    /// Only the labwc that headless.sh started with `-C $GS_ROOT/cfg`.
    fn private() -> Result<Compositor, String> {
        let root = std::env::var("GS_ROOT").unwrap_or_default();
        let pid: i32 = std::env::var("LABWC_PID").ok().and_then(|p| p.parse().ok()).ok_or("no LABWC_PID")?;
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let config = format!("{root}/cfg");
        check!(!root.is_empty() && cmdline.split(|b| *b == 0).any(|a| a == config.as_bytes()), "labwc {pid} is not the private compositor");
        let rc = PathBuf::from(&config).join("rc.xml");
        Ok(Compositor { pid, original: read(&rc), rc })
    }

    fn load(&self, text: &str) {
        std::fs::write(&self.rc, text).unwrap();
        // SAFETY: plain kill(2) on the private compositor checked above.
        unsafe { libc::kill(self.pid, libc::SIGHUP) };
        settle(0.6);
    }
}

impl Drop for Compositor {
    fn drop(&mut self) { self.load(&self.original.clone()) }
}

/// Type one combination with a virtual keyboard into the private compositor.
fn press(mods: &[&str], key: &str) {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap();
    let mut args: Vec<&str> = Vec::new();
    for m in mods {
        args.extend(["-M", m]);
    }
    args.extend(["-k", key]);
    for m in mods.iter().rev() {
        args.extend(["-m", m]);
    }
    let mut child = std::process::Command::new("/usr/bin/wtype").args(&args).env("WAYLAND_DISPLAY", display).spawn().expect("wtype");
    while child.try_wait().unwrap().is_none() {
        settle(0.01);
    }
}

/// A virtual keyboard that stays plugged in, so the seat has a keyboard and windows get focus.
struct Keyboard(std::process::Child);

impl Keyboard {
    fn plug() -> Keyboard {
        let display = std::env::var("WAYLAND_DISPLAY").unwrap();
        let child = std::process::Command::new("/usr/bin/wtype").args(["-s", "600000"]).env("WAYLAND_DISPLAY", display).spawn().expect("wtype");
        settle(0.5);
        Keyboard(child)
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn focus_title(title: &str) {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap();
    let _ = std::process::Command::new("/usr/bin/wlrctl").args(["toplevel", "focus", &format!("title:{title}")])
        .env("WAYLAND_DISPLAY", display).status();
}

fn inhibited(window: &gtk::Window) -> bool {
    window.surface().and_downcast::<gtk::gdk::Toplevel>().is_some_and(|t| t.is_shortcuts_inhibited())
}

/// (wtype modifiers, wtype key, labwc / dialog key, marker)
const COMBOS: &[(&[&str], &str, &str, &str)] = &[
    (&[], "Print", "Print", "print"),
    (&[], "F9", "F9", "f9"),
    (&["ctrl"], "Print", "C-Print", "c-print"),
    (&["shift"], "Print", "S-Print", "s-print"),
    (&["logo"], "e", "W-e", "w-e"),
    (&["alt"], "F4", "A-F4", "a-f4"),
    (&["logo"], "Left", "W-Left", ""),
    (&["alt"], "Tab", "A-Tab", ""),
];

fn bound_rc(markers: &Path) -> String {
    let mut binds = String::new();
    for (_, _, key, marker) in COMBOS {
        if marker.is_empty() {
            continue;
        }
        binds += &format!("<keybind key=\"{key}\"><action name=\"Execute\"><command>touch {}</command></action></keybind>\n",
                          markers.join(marker).display());
    }
    binds += "<keybind key=\"W-Left\"><action name=\"SnapToEdge\" direction=\"left\"/></keybind>\n";
    binds += "<keybind key=\"A-Tab\"><action name=\"NextWindow\"/></keybind>\n";
    format!("<?xml version=\"1.0\"?>\n<labwc_config><keyboard>\n{binds}</keyboard></labwc_config>\n")
}

fn fired(markers: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(markers).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    for name in &names {
        let _ = std::fs::remove_file(markers.join(name));
    }
    names
}

/// Issue #5: while the dialog listens, combinations labwc has bound (Print takes a
/// screenshot) must reach the dialog instead of running. wtype is a virtual keyboard,
/// which labwc matches by keysym only; the inhibitor check is shared with keycodes.
fn rebind_dialog_keeps_compositor_shortcuts_from_firing(app: &gtk::Application) -> Result<(), String> {
    let _keyboard = Keyboard::plug();
    let base = Base::new(app, "rebind-inhibit");
    base.window.set_title(Some("Shortcuts parent"));
    let markers = base.root.join("fired");
    std::fs::create_dir_all(&markers).unwrap();
    let compositor = Compositor::private()?;
    compositor.load(&bound_rc(&markers));
    let rc = base.root.join("rc.xml");
    std::fs::write(&rc, RC).unwrap();
    let page = base.show(gs_system::shortcuts_page::build_page_with(Some(store(&rc))));
    let c = page.controller::<ShortcutsPage>().unwrap();
    check!(wait(|| base.window.is_active(), 3.0), "parent never became active");
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    let s = seen.clone();
    tessel_gtk::utils::key_controller(&base.window, true, move |_, keyval, _, _| {
        s.borrow_mut().push(keyval.name().map(|n| n.to_string()).unwrap_or_default());
        glib::Propagation::Proceed
    });
    // Without the dialog every combination belongs to the compositor.
    for (mods, key, name, marker) in COMBOS {
        press(mods, key);
        settle(0.3);
        let got = fired(&markers);
        check!(seen.borrow().is_empty(), "{name} reached the parent: {:?}", seen.borrow());
        check!(marker.is_empty() || got == [marker.to_string()], "{name} did not fire: {got:?}");
        focus_title("Shortcuts parent");
        settle(0.1);
    }
    let lock = c.shortcuts.borrow().iter().find(|s| s.key == "W-l").cloned().unwrap();
    let dialog = RebindDialog::new(Some(base.window.upcast_ref()), &lock, &c.shortcuts.borrow(), c.apply_callback());
    dialog.present();
    wait(|| dialog.window.is_active() && inhibited(&dialog.window), 3.0);
    check!(dialog.window.is_active(), "dialog never became active");
    let capture = |name: &str, entry: bool| -> Result<(), String> {
        let (mods, key, _, _) = COMBOS.iter().find(|c| c.2 == name).unwrap();
        dialog.set("", false);
        if entry {
            dialog.entry.grab_focus();
            settle(0.05);
        }
        press(mods, key);
        wait(|| !dialog.key.borrow().is_empty(), 1.0);
        settle(0.1);
        let got = fired(&markers);
        check!(got.is_empty(), "{name} fired while the dialog listened: {got:?}");
        check!(*dialog.key.borrow() == name, "{name}: dialog got {:?}", dialog.key.borrow());
        Ok(())
    };
    for (_, _, name, _) in COMBOS {
        capture(name, false)?;
    }
    check!(inhibited(&dialog.window));
    // Typing in the entry still works, and bare keys are still captured there.
    dialog.set("", false);
    dialog.entry.grab_focus();
    settle(0.05);
    press(&[], "a");
    press(&[], "minus");
    check!(wait(|| dialog.entry.text() == "a-", 1.0), "typed {:?}", dialog.entry.text());
    for name in ["Print", "F9", "S-Print", "W-e"] {
        capture(name, true)?;
    }
    // Focus moves to another window and back: the inhibitor stays.
    let other = gtk::Window::builder().title("Other window").default_width(200).default_height(100).build();
    other.present();
    check!(wait(|| other.is_active() && !dialog.window.is_active(), 3.0), "other window never took focus");
    press(&["ctrl"], "Print");
    settle(0.3);
    check!(fired(&markers) == ["c-print"], "C-Print in another window should fire");
    focus_title("Change Shortcut");
    check!(wait(|| dialog.window.is_active(), 3.0), "dialog did not get focus back");
    check!(inhibited(&dialog.window));
    capture("W-e", false)?;
    other.destroy();
    settle(0.2);
    check!(wait(|| dialog.window.is_active(), 3.0));
    capture("C-Print", false)?;
    // A combination another shortcut uses: explained, not saved, the dialog keeps listening.
    dialog.set("", false);
    press(&["logo"], "t");
    check!(wait(|| *dialog.key.borrow() == "W-t", 1.0));
    check!(dialog.message.text().starts_with("Already used by"), "{}", dialog.message.text());
    check!(!dialog.save.is_sensitive());
    press(&["logo"], "a");
    check!(wait(|| *dialog.key.borrow() == "W-a", 1.0));
    check!(dialog.save.is_sensitive());
    // Escape closes even from the entry; then the compositor gets the combinations again.
    dialog.entry.grab_focus();
    settle(0.05);
    let window = dialog.window.clone();
    press(&[], "Escape");
    check!(wait(|| !window.is_visible(), 1.0), "Escape did not close the dialog");
    check!(!inhibited(&window));
    check!(wait(|| base.window.is_active(), 3.0));
    press(&["ctrl"], "Print");
    settle(0.3);
    check!(fired(&markers) == ["c-print"], "C-Print should fire after the dialog closed");
    check!(*dialog.key.borrow() == "W-a");
    drop(compositor);
    base.keep(page);
    Ok(())
}

// ---- about / users ---------------------------------------------------------

fn about_values_copy_and_restart_command(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "about");
    let calls = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
    let c2 = calls.clone();
    let values = AboutValues { os: "Test OS 1".into(), hostname: "box".into(), kernel: "6.0 (x86_64)".into(), cpu: "CPU × 4".into(),
                               memory: 8 * 1024u64.pow(3), gpu: vec!["GPU A".into(), "GPU B".into()], disk: (10u64.pow(12), 10u64.pow(11)),
                               desktop: String::new(), labwc: "0.20.2".into(), session: "wayland".into() };
    let page = base.show(gs_system::about_page::build_page_with(Some(Box::new(move || values)),
        Some(Rc::new(move |argv, done| { c2.borrow_mut().push(argv); done(true, String::new()) }))));
    let c = page.controller::<AboutPage>().unwrap();
    check!(wait(|| c.copy.is_sensitive(), 3.0));
    let text = c.text();
    check!(text.contains("Test OS 1"));
    check!(text.contains("GPU A; GPU B"));
    check!(text.contains("labwc 0.20.2"));
    check!(text.contains("Development build"));
    c.copy_text();
    c.restart_shell();
    let expected: Vec<String> = RESTART_COMMAND.iter().map(|s| s.to_string()).collect();
    check!(*calls.borrow() == vec![expected]);
    check!(c.restart_status.text() == "The desktop shell was restarted.");
    for key in ["about-info", "about-copy", "about-restart"] {
        check!(page.search_targets.borrow().contains_key(key));
    }
    base.keep(page);
    Ok(())
}

struct FakeAccounts {
    changes: Mutex<Vec<String>>,
}

impl Accounts for FakeAccounts {
    fn current(&self) -> PyResult<Account> {
        Ok(Account { uid: 1000, user_name: "me".into(), real_name: "Old Name".into(), icon_file: String::new(), administrator: true,
                     path: "/user/1000".into() })
    }
    fn others(&self) -> PyResult<Vec<Account>> {
        Ok(vec![Account { uid: 1001, user_name: "guest".into(), real_name: "Guest".into(), path: "/user/1001".into(), ..Default::default() }])
    }
    fn set_real_name(&self, _account: &Account, name: &str) -> PyResult<()> {
        self.changes.lock().unwrap().push(name.into());
        Ok(())
    }
    fn set_icon(&self, _account: &Account, _path: &str) -> PyResult<()> { Ok(()) }
}

fn users_name_and_password_flow(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "users");
    let accounts = Arc::new(FakeAccounts { changes: Mutex::new(Vec::new()) });
    let page = base.show(gs_system::users_page::build_page_with(Some(accounts.clone()), Some(Arc::new(|old: &str, _new: &str| (old == "a", "bad".into())))));
    let c = page.controller::<UsersPage>().unwrap();
    check!(wait(|| c.others.is_visible(), 3.0));
    check!(c.name_entry.text() == "Old Name");
    check!(texts(&c.others).contains(&"Guest".to_string()));
    check!(c.user_label.text() == "me · Administrator");
    c.name_entry.set_text("New Name");
    c.save_name();
    check!(wait(|| *accounts.changes.lock().unwrap() == ["New Name"], 3.0));
    let dialog = PasswordDialog::new(Some(base.window.upcast_ref()), c.page.closed_flag(),
                                     Arc::new(|old: &str, _new: &str| (old == "a", "rejected".into())));
    dialog.present();
    dialog.current.set_text("b");
    dialog.new.set_text("x");
    dialog.confirm.set_text("y");
    check!(!dialog.change.is_sensitive());
    check!(dialog.message.text() == "The new passwords do not match.");
    dialog.confirm.set_text("x");
    check!(dialog.change.is_sensitive());
    dialog.submit();
    check!(wait(|| dialog.message.text().contains("rejected"), 3.0));
    dialog.current.set_text("a");
    dialog.submit();
    check!(wait(|| !dialog.window.is_visible(), 3.0));
    base.keep(page);
    Ok(())
}

// ---- preference pages ------------------------------------------------------

struct FakeApps {
    calls: RefCell<Vec<(String, String)>>,
}

impl DefaultApps for FakeApps {
    fn candidates(&self, _key: &str, _types: &[&str]) -> Vec<App> { vec![App::new("a.desktop", "App A"), App::new("b.desktop", "App B")] }
    fn current(&self, _key: &str, _types: &[&str], _apps: &[App]) -> Option<App> { Some(App::new("a.desktop", "App A")) }
    fn set(&self, key: &str, _types: &[&str], app: &App, done: Done) -> PyResult<()> {
        self.calls.borrow_mut().push((key.into(), app.id.clone()));
        done(true, String::new());
        Ok(())
    }
}

fn default_apps_set_through_backend(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "defaultapps");
    let backend = Rc::new(FakeApps { calls: RefCell::new(Vec::new()) });
    let page = base.show(gs_system::default_apps_page::build_page_with(Some(backend.clone())));
    let c = page.controller::<DefaultAppsPage>().unwrap();
    let mut keys: Vec<&str> = c.controls.borrow().keys().copied().collect();
    keys.sort();
    let mut expected: Vec<&str> = CATEGORIES.iter().map(|(k, _, _)| *k).collect();
    expected.sort();
    check!(keys == expected);
    let browser = c.controls.borrow()["browser"].clone();
    browser.set_selected(1);
    check!(*backend.calls.borrow() == [("browser".to_string(), "b.desktop".to_string())]);
    check!(c.status.text().contains("App B"));
    check!(page.search_targets.borrow().contains_key("default-terminal"));
    // Activation refreshes in place; search targets stay the same widgets.
    page.activate();
    check!(page.search_targets.borrow()["default-browser"] == browser.upcast::<gtk::Widget>());
    base.keep(page);
    Ok(())
}

/// Per-category candidates and current choices, and a `set` that can refuse.
struct StateApps {
    calls: RefCell<Vec<(String, String)>>,
    current: RefCell<Option<App>>,
    refuse: std::cell::Cell<bool>,
}

impl DefaultApps for StateApps {
    fn candidates(&self, key: &str, _types: &[&str]) -> Vec<App> {
        match key {
            "documents" => Vec::new(),
            "browser" => vec![App::new("firefox.desktop", "Firefox"), App::new("org.mozilla.firefox.desktop", "Firefox")],
            _ => vec![App::new("a.desktop", "App A"), App::new("b.desktop", "App B")],
        }
    }
    fn current(&self, key: &str, _types: &[&str], _apps: &[App]) -> Option<App> {
        match key {
            "documents" | "archives" => None,
            "images" => self.current.borrow().clone(),
            "browser" => Some(App::new("firefox.desktop", "Firefox")),
            _ => Some(App::new("a.desktop", "App A")),
        }
    }
    fn set(&self, key: &str, _types: &[&str], app: &App, done: Done) -> PyResult<()> {
        self.calls.borrow_mut().push((key.into(), app.id.clone()));
        if self.refuse.get() {
            return Err(gs_system::compat::Error::Os("refused".into()));
        }
        done(true, String::new());
        Ok(())
    }
}

fn names(widget: &gtk::DropDown) -> Vec<String> {
    let model = widget.model().unwrap();
    (0..model.n_items()).map(|i| model.item(i).and_downcast::<gtk::StringObject>().unwrap().string().to_string()).collect()
}

fn default_apps_states_new_categories_and_refusals(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "defaultapps-states");
    let backend = Rc::new(StateApps { calls: RefCell::new(Vec::new()), current: RefCell::new(Some(App::new("a.desktop", "App A"))),
                                      refuse: std::cell::Cell::new(false) });
    let page = base.show(gs_system::default_apps_page::build_page_with(Some(backend.clone())));
    let c = page.controller::<DefaultAppsPage>().unwrap();
    for key in ["default-documents", "default-archives"] {
        check!(page.search_targets.borrow().contains_key(key), "missing target {key}");
    }
    // Nothing installed: one insensitive placeholder.
    let documents = c.controls.borrow()["documents"].clone();
    check!(names(&documents) == ["No applications installed"]);
    check!(!documents.is_sensitive());
    // Installed but no default: "Not set" first and selected; picking it does nothing.
    let archives = c.controls.borrow()["archives"].clone();
    check!(names(&archives) == ["Not set", "App A", "App B"]);
    check!(archives.selected() == 0 && archives.is_sensitive());
    archives.set_selected(2);
    check!(*backend.calls.borrow() == [("archives".to_string(), "b.desktop".to_string())]);
    // Two applications with one name are told apart.
    let browser = c.controls.borrow()["browser"].clone();
    check!(names(&browser) == ["Firefox (firefox)", "Firefox (org.mozilla.firefox)"], "{:?}", names(&browser));
    // A refused change reports the error and shows the real default again.
    backend.refuse.set(true);
    let images = c.controls.borrow()["images"].clone();
    check!(images.selected() == 0);
    images.set_selected(1);
    check!(c.status.has_css_class("error") && c.status.text().contains("refused"), "{}", c.status.text());
    check!(wait(|| images.selected() == 0, 2.0), "the dropdown went back to the current default");
    // A default that disappeared (uninstalled) reads as "Not set".
    backend.refuse.set(false);
    backend.current.replace(None);
    page.activate();
    check!(names(&images)[0] == "Not set" && images.selected() == 0);
    base.keep(page);
    Ok(())
}

fn accessibility_writes_gsettings_and_cursor_environment(app: &gtk::Application) -> Result<(), String> {
    let Some(interface) = settings(INTERFACE) else {
        println!("    (skipped: GNOME interface schema is not installed)");
        return Ok(());
    };
    let base = Base::new(app, "a11y");
    let env = base.root.join("environment");
    std::fs::write(&env, "XCURSOR_SIZE=24\n").unwrap();
    let commands = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
    let c2 = commands.clone();
    let mut backend = AccessibilityBackend::new(Some(env.clone()), Some(Rc::new(move |argv, _done| c2.borrow_mut().push(argv))));
    backend.orca = Some(true);
    let page = base.show(gs_system::accessibility_page::build_page_with(Some(backend)));
    let c = page.controller::<AccessibilityPage>().unwrap();
    c.motion.set_active(true);
    check!(!interface.boolean("enable-animations"));
    c.large_cursor.set_active(true);
    check!(interface.int("cursor-size") == 48);
    check!(read(&env) == "XCURSOR_SIZE=48\n", "{}", read(&env));
    check!(c.cursor_sizes[c.cursor_size.selected() as usize] == 48);
    let index = c.text_scales.iter().position(|v| *v == 1.5).unwrap();
    c.text_size.set_selected(index as u32);
    check!((interface.double("text-scaling-factor") - 1.5).abs() < 1e-9);
    if c.screen_reader.is_sensitive() {
        c.screen_reader.set_active(true);
        check!(commands.borrow().last().unwrap()[0] == "systemd-run");
        check!(commands.borrow().last().unwrap()[4] == "--unit=orca-screen-reader");
    }
    base.keep(page);
    Ok(())
}

fn night_light_saves_and_controls_the_unit_and_warns_about_hdr(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "night");
    let store = NightLightStore::new(Some(&base.root.join("night.json")), Some(&base.root.join("weather.json")));
    let commands = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
    let c2 = commands.clone();
    let rc = r#"<labwc_config><hdrOutputs><output name="HDMI-A-1" enabled="yes"/></hdrOutputs></labwc_config>"#;
    let page = base.show(gs_system::night_light_page::build_page_with(Some(store.clone()),
        Some(Rc::new(move |argv, done| { c2.borrow_mut().push(argv); done(true, String::new()) })),
        Some((Some(vec!["HDMI-A-1".into(), "DP-1".into()]), rc.into())), Some(true)));
    let c = page.controller::<NightLightPage>().unwrap();
    check!(c.hdr_note.is_visible());
    check!(c.hdr_note.text().contains("HDMI-A-1"));
    check!(c.times_row.is_visible());
    c.enabled.set_active(true);
    check!(store.load().enabled);
    check!(*commands.borrow().last().unwrap() == ["systemctl", "--user", "enable", "--now", "tessel-night-light.service"]);
    c.schedule.set_selected(1);
    check!(store.load().schedule == "manual");
    check!(*commands.borrow().last().unwrap() == ["systemctl", "--user", "restart", "tessel-night-light.service"]);
    c.sunset.set_text("99:00");
    c.delayed();
    check!(c.status.text().contains("HH:MM"));
    check!(store.load().sunset == "20:00");
    base.keep(page);
    Ok(())
}

fn night_light_is_disabled_when_every_output_is_hdr(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "night-hdr");
    let store = NightLightStore::new(Some(&base.root.join("n.json")), Some(&base.root.join("w.json")));
    let page = base.show(gs_system::night_light_page::build_page_with(Some(store), Some(Rc::new(|_, _| {})),
        Some((Some(vec!["DP-1".into()]), "<labwc_config><core><hdr>yes</hdr></core></labwc_config>".into())), Some(true)));
    check!(!page.controller::<NightLightPage>().unwrap().enabled.is_sensitive());
    base.keep(page);
    Ok(())
}

// ---- input -----------------------------------------------------------------

struct FakePolling {
    state: Mutex<PollingState>,
    writes: Mutex<Vec<i64>>,
    error: Mutex<String>,
}

impl FakePolling {
    fn new() -> Arc<FakePolling> {
        Arc::new(FakePolling {
            state: Mutex::new(PollingState { current: Some(1000), rates: vec![125, 500, 1000], backend: "OpenRazer".into(), owner: ":1.22".into(),
                                             target: "/mouse".into(), detail: "Test mouse".into(), ..Default::default() }),
            writes: Mutex::new(Vec::new()), error: Mutex::new(String::new()),
        })
    }
}

impl Polling for FakePolling {
    fn read(&self, _device: &InputDevice) -> PyResult<PollingState> { Ok(self.state.lock().unwrap().clone()) }
    fn apply(&self, _device: &InputDevice, _expected: &PollingState, value: i64) -> PyResult<PollingState> {
        self.writes.lock().unwrap().push(value);
        let error = self.error.lock().unwrap().clone();
        if !error.is_empty() {
            return Err(gs_system::compat::Error::Runtime(error));
        }
        let mut state = self.state.lock().unwrap();
        state.current = Some(value);
        Ok(state.clone())
    }
}

struct InputFixture {
    base: Base,
    path: PathBuf,
    reloads: Arc<Mutex<usize>>,
    backend: InputConfig,
    polling: Arc<FakePolling>,
    controller: Rc<InputControls>,
}

impl InputFixture {
    fn new(app: &gtk::Application, name: &str) -> InputFixture {
        let base = Base::new(app, name);
        let path = base.root.join("rc.xml");
        std::fs::write(&path, INPUT).unwrap();
        let reloads = Arc::new(Mutex::new(0));
        let r = reloads.clone();
        let backend = InputConfig::at(&path, base.root.join("backup"), Arc::new(move || { *r.lock().unwrap() += 1; Ok(()) }));
        let polling = FakePolling::new();
        let controller = InputControls::new(Some(backend.clone()), Some(Arc::new(|| (parse_devices(LAPTOP), String::new()))),
                                            Some(polling.clone()));
        base.window.set_child(Some(&controller.widget));
        settle(0.2);
        InputFixture { base, path, reloads, backend, polling, controller }
    }

    fn select(&self, name: &str) {
        let index = self.controller.profiles.borrow().iter().position(|(n, _, _)| n == name).unwrap();
        self.controller.selector.set_selected(index as u32);
        settle(0.1);
    }

    fn unchanged(&self) -> bool { read(&self.path) == INPUT }

    fn reload_count(&self) -> usize { *self.reloads.lock().unwrap() }
}

impl Drop for InputFixture {
    fn drop(&mut self) { self.controller.close() }
}

fn pending_value(c: &InputControls, name: &str, kind: &str, key: &str) -> Option<Option<String>> {
    c.pending.borrow().iter().find(|(p, _)| p.0 == name && p.1 == kind)
        .and_then(|(_, v)| v.as_ref().and_then(|v| v.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())))
}

fn input_browsing_does_not_write_and_controls_reflect_capabilities(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-browse");
    let pad = parse_devices(LAPTOP)[0].clone();
    f.select(&pad.name);
    check!(f.controller.control("clickMethod").is_some());
    check!(f.controller.control("tap").unwrap().widget().is_sensitive());
    f.select("TPPS/2 Elan TrackPoint");
    check!(f.controller.control("tap").is_none());
    check!(f.controller.control("scrollButton").is_some());
    check!(!f.controller.apply_button.is_sensitive());
    check!(f.unchanged());
    check!(f.reload_count() == 0);
    Ok(())
}

fn input_edits_survive_switching_device_and_apply_once(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-edits");
    f.select("touchpad");
    let natural = f.controller.control("naturalScroll").unwrap();
    check!(!natural.switch().unwrap().is_active());
    natural.switch().unwrap().set_active(true);
    check!(f.controller.test_notice.borrow().as_ref().unwrap().is_visible());
    f.select("non-touch");
    f.controller.control("pointerSpeed").unwrap().scale().unwrap().set_value(0.65);
    f.select("touchpad");
    let natural = f.controller.control("naturalScroll").unwrap();
    check!(natural.switch().unwrap().is_active());
    check!(natural.reset().unwrap().is_visible());
    f.controller.apply_button.emit_clicked();
    settle(0.4);
    let state = f.backend.load().unwrap();
    check!(state.match_profile("pad", "touchpad").get("naturalScroll") == Some("yes"));
    check!(state.match_profile("mouse", "non-touch").get("pointerSpeed") == Some("0.65"));
    check!(f.reload_count() == 1);
    check!(!f.controller.has_pending());
    Ok(())
}

fn input_switch_reset_and_scroll_button_capture(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-switch");
    f.select("touchpad");
    let tap = f.controller.control("tap").unwrap();
    check!(tap.switch().unwrap().is_active()); // labwc default, shown as the value in effect
    tap.switch().unwrap().set_active(false);
    check!(pending_value(&f.controller, "touchpad", "touchpad", "tap") == Some(Some("no".into())));
    f.controller.control("tap").unwrap().reset().unwrap().emit_clicked();
    check!(f.controller.value("tap").is_none());
    check!(f.controller.control("tap").unwrap().switch().unwrap().is_active());
    check!(!f.controller.control("tap").unwrap().reset().unwrap().is_visible());
    f.select("TPPS/2 Elan TrackPoint");
    let gs_system::input_page::Control::ScrollButton { capture, default, .. } = f.controller.control("scrollButton").unwrap() else {
        return Err("scrollButton is not a capture row".into());
    };
    capture.button.emit_clicked();
    check!(capture.listening());
    capture.record(8); // GDK "back" button
    check!(!capture.listening());
    check!(f.controller.value("scrollButton").as_deref() == Some("275"));
    check!(!default.is_active());
    Ok(())
}

fn input_closing_with_pending_changes_asks_apply_discard_or_cancel(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-close");
    let answers = Rc::new(RefCell::new(Vec::<i32>::new()));
    let a = answers.clone();
    gs_base::style::set_chooser(Some(Rc::new(move |_m, _d, _b, callback, _default, _cancel| {
        let answer = a.borrow_mut().remove(0);
        callback(Some(answer));
    })));
    let closed = Rc::new(RefCell::new(0));
    let host = || { let c = closed.clone(); Some(Rc::new(move || *c.borrow_mut() += 1) as Rc<dyn Fn()>) };
    let result = (|| {
        check!(f.controller.can_close(host()));
        f.controller.reset_button.emit_clicked();
        answers.borrow_mut().push(2); // Cancel keeps the edit and the window
        check!(!f.controller.can_close(host()));
        check!(f.controller.has_pending());
        check!(*closed.borrow() == 0);
        answers.borrow_mut().push(1); // Discard
        check!(!f.controller.can_close(host()));
        check!(!f.controller.has_pending());
        check!(*closed.borrow() == 1);
        check!(f.unchanged());
        f.controller.reset_button.emit_clicked();
        answers.borrow_mut().push(0); // Apply, then close
        f.controller.can_close(host());
        settle(0.4);
        check!(!f.controller.has_pending());
        check!(*closed.borrow() == 2);
        check!(!f.unchanged());
        Ok(())
    })();
    gs_base::style::set_chooser(None);
    result
}

fn input_reset_and_discard_do_not_apply(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-reset");
    f.controller.reset_button.emit_clicked();
    check!(f.controller.has_pending());
    check!(f.unchanged());
    f.controller.discard_button.emit_clicked();
    check!(!f.controller.has_pending());
    check!(f.controller.value("pointerSpeed").as_deref() == Some("0.2"));
    Ok(())
}

fn setting(target: &'static str) -> Setting { Setting { target, ..Setting::new("input", "x", "x", "") } }

fn input_search_reveals_touchpad_controls(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-reveal");
    f.controller.reveal_setting(&setting("clickMethod"));
    settle(0.1);
    check!(*f.controller.selected.borrow() == ("touchpad".to_string(), "touchpad".to_string()));
    check!(f.controller.row("clickMethod").unwrap().is_mapped());
    check!(!f.controller.has_pending());
    Ok(())
}

fn input_polling_reports_current_and_applies_without_saving_libinput_edits(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-poll");
    f.select("TPPS/2 Elan TrackPoint");
    let card = f.controller.polling_card.borrow().clone().unwrap();
    check!(wait(|| card.current.text().contains("1000 Hz"), 2.0), "{}", card.current.text());
    check!(!card.apply_button.is_sensitive());
    f.controller.control("leftHanded").unwrap().dropdown().unwrap().set_selected(2);
    let pending = f.controller.pending.borrow().clone();
    card.choice.set_selected(1);
    check!(card.apply_button.is_sensitive());
    card.apply_button.emit_clicked();
    check!(!f.controller.selector.is_sensitive());
    settle(0.3);
    check!(card.current.text().contains("500 Hz"));
    check!(*f.polling.writes.lock().unwrap() == [500]);
    check!(*f.controller.pending.borrow() == pending);
    check!(f.unchanged());
    check!(f.reload_count() == 0);
    check!(f.controller.selector.is_sensitive());
    Ok(())
}

fn input_polling_unavailable_and_failed_write(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-poll-fail");
    f.select("TPPS/2 Elan TrackPoint");
    let card = f.controller.polling_card.borrow().clone().unwrap();
    check!(wait(|| card.current.text().contains("1000 Hz"), 2.0));
    *f.polling.error.lock().unwrap() = "Mouse write failed".into();
    card.choice.set_selected(0);
    card.apply_button.emit_clicked();
    settle(0.3);
    check!(card.detail.text().contains("Mouse write failed"));
    check!(card.current.text().contains("1000 Hz"));
    {
        let mut state = f.polling.state.lock().unwrap();
        state.current = None;
        state.rates.clear();
        state.detail = "Not supported".into();
    }
    card.refresh(false);
    settle(0.2);
    check!(card.current.text().contains("Not reported"));
    check!(!card.choice.is_visible());
    check!(!card.apply_button.is_visible());
    Ok(())
}

fn input_polling_refresh_and_search_do_not_write_or_reset_pending_rate(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-poll-refresh");
    f.controller.reveal_setting(&setting("pollingRate"));
    settle(0.2);
    check!(f.controller.selected.borrow().0 != "non-touch");
    let card = f.controller.polling_card.borrow().clone().unwrap();
    check!(wait(|| card.current.text().contains("1000 Hz"), 2.0));
    card.choice.set_selected(0);
    f.polling.state.lock().unwrap().current = Some(500);
    let _ = card.tick();
    settle(0.2);
    check!(card.current.text().contains("500 Hz"));
    check!(card.value() == Some(125));
    check!(f.polling.writes.lock().unwrap().is_empty());
    check!(f.unchanged());
    f.select("touchpad");
    check!(card.closed.get());
    check!(card.timer.borrow().is_none());
    check!(f.controller.polling_card.borrow().is_none());
    Ok(())
}

fn input_private_compositor_accepts_all_input_options(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-session");
    let config = InputConfig::new(None, None, None, Some(f.base.root.join("private-backup"))).map_err(|e| e.to_string())?;
    let root = std::env::var("GS_ROOT").unwrap();
    check!(config.path().starts_with(&root), "{:?} not under {root}", config.path());
    let original = config.read().map_err(|e| e.to_string())?;
    let values: Vec<(&str, Option<&str>)> = vec![("sendEventsMode", Some("yes")), ("leftHanded", Some("no")), ("pointerSpeed", Some("0.2")),
        ("accelProfile", Some("adaptive")), ("naturalScroll", Some("yes")), ("scrollFactor", Some("1.2")), ("scrollMethod", Some("twofinger")),
        ("tap", Some("yes")), ("tapButtonMap", Some("lrm")), ("clickMethod", Some("buttonAreas")), ("tapAndDrag", Some("yes")),
        ("dragLock", Some("timeout")), ("threeFingerDrag", Some("3")), ("disableWhileTyping", Some("yes")), ("middleEmulation", Some("no"))];
    let result = config.save(&vec![update("touchpad", "touchpad", &values)], &config.load().unwrap());
    let loaded = config.load().map(|s| s.match_profile("touchpad", "touchpad").values_map());
    std::fs::write(config.path(), &original).unwrap();
    result.map_err(|e| e.to_string())?;
    let expected: std::collections::BTreeMap<String, String> = values.iter().map(|(k, v)| (k.to_string(), v.unwrap().to_string())).collect();
    check!(loaded.map_err(|e| e.to_string())? == expected);
    // labwc --reconfigure went to the logging fake on PATH.
    let log = std::fs::read_to_string(Path::new(&root).join("tmp/calls.log")).unwrap_or_default();
    check!(log.contains("labwc --reconfigure"), "{log}");
    Ok(())
}

fn input_remove_override_and_search_targets(app: &gtk::Application) -> Result<(), String> {
    let f = InputFixture::new(app, "input-remove");
    let updates: Updates = vec![update("Mouse", "non-touch", &[("pointerSpeed", Some("1"))])];
    f.backend.save(&updates, &f.backend.load().unwrap()).unwrap();
    f.controller.reload();
    f.select("Mouse");
    check!(f.controller.shared_button.is_visible());
    f.controller.shared_button.emit_clicked();
    check!(!f.controller.body.is_sensitive());
    f.controller.apply_button.emit_clicked();
    settle(0.3);
    check!(!f.backend.load().unwrap().contains("mouse"));
    check!(f.controller.value("pointerSpeed").as_deref() == Some("0.2"));
    // The page registers stable rows for every search target.
    let host = f.base.host.clone();
    let page = gs_system::input_page::build_page_with(&host, Some(f.backend.clone()),
        Some(Arc::new(|| (parse_devices(LAPTOP), String::new()))), Some(f.polling.clone()));
    f.base.window.set_child(Some(&page.widget));
    settle(0.2);
    let reveal = page.reveal_setting.borrow().clone().unwrap();
    let click = Setting { target: "clickMethod", ..Setting::new("input", "areas", "Physical click method", "") };
    reveal(&click);
    settle(0.2);
    let target = page.search_target(&click).unwrap();
    check!(target.is_mapped());
    check!(target.has_css_class("gs-row"));
    check!(page.search_target(&Setting { target: "pollingRate", ..Setting::new("input", "polling", "", "") }).is_some());
    page.close();
    Ok(())
}

// ---- workspaces ------------------------------------------------------------

fn workspace_controls_load_apply_and_report_errors(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "workspaces");
    let path = base.root.join("rc.xml");
    std::fs::write(&path, "<labwc_config><desktops number=\"4\"/></labwc_config>").unwrap();
    let reloads = Arc::new(Mutex::new(0));
    let r = reloads.clone();
    let config = WorkspaceConfig::at(&path, base.root.join("backups"), Arc::new(move || { *r.lock().unwrap() += 1; Ok(()) }));
    let controls = WorkspaceControls::new(Some(Rc::new(config)));
    base.window.set_child(Some(&controls.widget));
    settle(0.1);
    check!(controls.count.value_as_int() == 4);
    check!(controls.limit.text() == "This control supports 1–100 workspaces. labwc has no fixed workspace-count cap.");
    controls.count.set_value(6.0);
    controls.refresh(); // keeps the unapplied edit
    check!(controls.count.value_as_int() == 6);
    check!(controls.apply());
    check!(read(&path).contains("number=\"6\""));
    check!(*reloads.lock().unwrap() == 1);
    std::fs::write(&path, "<labwc_config><desktops number=\"2\"/></labwc_config>").unwrap();
    controls.count.set_value(3.0);
    check!(!controls.apply());
    check!(controls.status.has_css_class("error") && controls.status.is_visible());
    check!(controls.status.text().contains("changed elsewhere"));
    let broken = WorkspaceConfig::at(base.root.join("missing/dir/rc.xml"), base.root.join("b"), Arc::new(|| Ok(())));
    std::fs::create_dir_all(base.root.join("missing/dir")).unwrap();
    std::fs::write(base.root.join("missing/dir/rc.xml"), "<other/>").unwrap();
    let failing = WorkspaceControls::new(Some(Rc::new(broken)));
    check!(!failing.count.is_sensitive());
    check!(failing.status.is_visible() && !failing.status.has_css_class("error"));
    check!(failing.apply());
    Ok(())
}

// ---- every section ---------------------------------------------------------

fn every_section_builds_with_search_targets(app: &gtk::Application) -> Result<(), String> {
    let base = Base::new(app, "sections");
    let expected: &[(&str, &[&str])] = &[
        ("keyboard", &["keyboard-layouts", "keyboard-switch", "keyboard-compose", "keyboard-repeat-delay", "keyboard-repeat-rate",
                       "keyboard-automatic", "automatic", "layout", "size", "devices"]),
        ("shortcuts", &["shortcuts-list", "shortcuts-search", "shortcuts-reset", "search"]),
        ("accessibility", &["a11y-text", "a11y-contrast", "a11y-motion", "a11y-cursor-size", "a11y-large-cursor", "a11y-screen-reader"]),
        ("defaultapps", &["default-browser", "default-mail", "default-files", "default-terminal", "default-editor", "default-documents",
                          "default-images", "default-video", "default-music", "default-archives"]),
        ("nightlight", &["night-enabled", "night-schedule", "night-temperature", "night-times"]),
        ("users", &["users-name", "users-picture", "users-password", "users-others"]),
    ];
    for (section, keys) in expected {
        let page = gs_system::build_page(section, &base.host)?;
        base.window.set_child(Some(&page.widget));
        settle(0.2);
        for key in *keys {
            check!(page.search_targets.borrow().contains_key(*key), "{section}: missing target {key}");
        }
        page.activate();
        page.deactivate();
        check!(page.can_close());
        page.close();
    }
    check!(gs_system::build_page("nope", &base.host).err().as_deref() == Some("nope"));
    let input = gs_system::input_page::build_page_with(&base.host, None, Some(Arc::new(|| (parse_devices(LAPTOP), String::new()))),
                                                        Some(FakePolling::new()));
    for key in ["pointerSpeed", "pollingRate", "accelProfile", "leftHanded", "middleEmulation", "naturalScroll", "scrollFactor",
                "scrollMethod", "tap", "clickMethod", "tapAndDrag", "threeFingerDrag", "disableWhileTyping", "sendEventsMode"] {
        check!(input.search_targets.borrow().contains_key(key), "input: missing {key}");
    }
    input.close();
    check!(gs_system::SECTIONS == ["input", "keyboard", "shortcuts", "accessibility", "defaultapps", "nightlight", "users"]);
    Ok(())
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let root = std::env::var("GS_ROOT").unwrap_or_default();
    // The private compositor's socket lives in its own runtime dir under GS_ROOT.
    if std::env::var("GS_HEADLESS").as_deref() != Ok("1") || root.is_empty() || !runtime.starts_with(&root) || display.is_empty() {
        println!("ui_headless: skipped (run inside components/settings/tests/headless.sh)");
        return;
    }
    gtk::init().expect("GTK");
    let app = gtk::Application::new(None::<&str>, gio::ApplicationFlags::NON_UNIQUE);
    app.register(gio::Cancellable::NONE).expect("register");
    let cases: &[(&str, Case)] = &[
        ("keyboard_layout_edits_and_repeat_are_saved_to_private_files", keyboard_layout_edits_and_repeat_are_saved_to_private_files),
        ("keyboard_numlock_switch_reflects_and_writes_rc", keyboard_numlock_switch_reflects_and_writes_rc),
        ("shortcuts_listing_search_conflicts_and_rebinding", shortcuts_listing_search_conflicts_and_rebinding),
        ("shortcuts_reset_one_or_all_to_the_shipped_keys", shortcuts_reset_one_or_all_to_the_shipped_keys),
        ("cheat_sheet_filters_and_closes", cheat_sheet_filters_and_closes),
        ("rebind_dialog_keeps_compositor_shortcuts_from_firing", rebind_dialog_keeps_compositor_shortcuts_from_firing),
        ("about_values_copy_and_restart_command", about_values_copy_and_restart_command),
        ("users_name_and_password_flow", users_name_and_password_flow),
        ("default_apps_set_through_backend", default_apps_set_through_backend),
        ("default_apps_states_new_categories_and_refusals", default_apps_states_new_categories_and_refusals),
        ("accessibility_writes_gsettings_and_cursor_environment", accessibility_writes_gsettings_and_cursor_environment),
        ("night_light_saves_and_controls_the_unit_and_warns_about_hdr", night_light_saves_and_controls_the_unit_and_warns_about_hdr),
        ("night_light_is_disabled_when_every_output_is_hdr", night_light_is_disabled_when_every_output_is_hdr),
        ("input_browsing_does_not_write_and_controls_reflect_capabilities", input_browsing_does_not_write_and_controls_reflect_capabilities),
        ("input_edits_survive_switching_device_and_apply_once", input_edits_survive_switching_device_and_apply_once),
        ("input_switch_reset_and_scroll_button_capture", input_switch_reset_and_scroll_button_capture),
        ("input_closing_with_pending_changes_asks_apply_discard_or_cancel", input_closing_with_pending_changes_asks_apply_discard_or_cancel),
        ("input_reset_and_discard_do_not_apply", input_reset_and_discard_do_not_apply),
        ("input_search_reveals_touchpad_controls", input_search_reveals_touchpad_controls),
        ("input_polling_reports_current_and_applies_without_saving_libinput_edits", input_polling_reports_current_and_applies_without_saving_libinput_edits),
        ("input_polling_unavailable_and_failed_write", input_polling_unavailable_and_failed_write),
        ("input_polling_refresh_and_search_do_not_write_or_reset_pending_rate", input_polling_refresh_and_search_do_not_write_or_reset_pending_rate),
        ("input_private_compositor_accepts_all_input_options", input_private_compositor_accepts_all_input_options),
        ("input_remove_override_and_search_targets", input_remove_override_and_search_targets),
        ("workspace_controls_load_apply_and_report_errors", workspace_controls_load_apply_and_report_errors),
        ("every_section_builds_with_search_targets", every_section_builds_with_search_targets),
    ];
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let (mut passed, mut failed) = (0, Vec::new());
    for (name, case) in cases {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| case(&app)));
        match outcome {
            Ok(Ok(())) => {
                passed += 1;
                println!("ok   {name}");
            }
            Ok(Err(message)) => {
                println!("FAIL {name}: {message}");
                failed.push(*name);
            }
            Err(_) => {
                println!("FAIL {name}: panicked");
                failed.push(*name);
            }
        }
        settle(0.05);
    }
    println!("ui_headless: {passed} passed, {} failed", failed.len());
    if !failed.is_empty() {
        std::process::exit(1);
    }
}
