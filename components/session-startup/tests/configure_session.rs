//! test_configure_session.py, ported: LiveMigrationTests, KeybindingTests, CommandLineTests.
//! Homes are copies under $TMPDIR; nothing touches the live session or the real home.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use tessel_session_startup::fsops;
use tessel_session_startup::session::{self, Kind};
use tessel_session_startup::xml;

struct Live {
    home: PathBuf,
    labwc: PathBuf,
    units: PathBuf,
}

fn live() -> Option<Live> {
    suite_root()?;
    let home = scratch("labwc-session-test").join("home");
    live_home(&home);
    Some(Live { labwc: home.join(".config/labwc"), units: home.join(".config/systemd/user"), home })
}

impl Live {
    fn install(&self) -> Vec<String> {
        let (files, notes) = session::plan(&self.home, &source()).unwrap();
        for (path, data, mode) in files {
            fsops::atomic_write(&path, &data, mode).unwrap();
        }
        notes
    }

    fn read(&self, path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }
}

/// Effective variables as labwc reads them: environment, then environment.d/*.env.
fn labwc_environment(labwc: &Path) -> HashMap<String, String> {
    let mut values: HashMap<String, String> =
        session::environment_values(&fs::read_to_string(labwc.join("environment")).unwrap()).into_iter().collect();
    let directory = labwc.join("environment.d");
    if directory.is_dir() {
        let mut paths: Vec<_> = fs::read_dir(&directory).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "env")).collect();
        paths.sort();
        for path in paths {
            values.extend(session::environment_values(&fs::read_to_string(path).unwrap()));
        }
    }
    values
}

#[test]
fn personal_startup_moves_to_local_file_and_still_runs() {
    let Some(t) = live() else { return };
    let before = t.read(&t.labwc.join("autostart"));
    t.install();
    let startup = t.read(&t.labwc.join("autostart"));
    let local = t.read(&t.labwc.join("autostart.local"));
    assert_eq!(startup, *session::AUTOSTART_TEMPLATE);
    assert!(!startup.contains("nordpass"));
    assert!(local.contains("# password manager\nsystemctl --user start nordpass.service >/dev/null 2>&1 &\n"));
    assert!(startup.contains(". \"$local_autostart\""));
    let combined = format!("{startup}{local}");
    let keys: Vec<_> = session::shell_statements(&combined).into_iter().filter(|(k, _)| *k == Kind::Code).map(|(_, t)| session::statement_key(&t)).collect();
    for (kind, text) in session::shell_statements(&before) {
        if kind == Kind::Code {
            assert!(keys.contains(&session::statement_key(&text)), "{text}");
        }
    }
    for name in ["autostart", "autostart.local"] {
        assert!(Command::new("sh").arg("-n").arg(t.labwc.join(name)).status().unwrap().success());
    }
    assert_eq!(fsops::mode(&t.labwc.join("autostart")).unwrap(), 0o755);
}

#[test]
fn environment_effective_values_are_unchanged() {
    let Some(t) = live() else { return };
    let before = labwc_environment(&t.labwc);
    t.install();
    assert_eq!(labwc_environment(&t.labwc), before);
    let generic = t.read(&t.labwc.join("environment"));
    let local = t.read(&t.labwc.join(session::LOCAL_ENVIRONMENT));
    for personal in ["WLR_NO_HARDWARE_CURSORS=1", "WLR_RENDERER=vulkan", "XDPW_FORCE_XFER=pq", "TERMINAL=ghostty", "XCURSOR_THEME=Adwaita:dark", "OBS_VKCAPTURE=1"] {
        assert!(!generic.contains(personal), "{personal}");
        assert!(local.contains(&format!("{personal}\n")), "{personal}");
    }
    assert!(generic.contains("# tessel-displays primary output environment begin\nSDL_VIDEO_DISPLAY_PRIORITY=HDMI-A-1\n"));
    assert!(generic.contains("# Tessel language — managed by Language Settings\n"));
    assert!(generic.contains("XKB_DEFAULT_OPTIONS=grp:alt_shift_toggle\n"));
    assert!(!local.contains("HDMI-A-1"));
}

#[test]
fn reinstall_is_idempotent_and_keeps_local_files() {
    let Some(t) = live() else { return };
    t.install();
    let local = t.labwc.join("autostart.local");
    fs::write(&local, t.read(&local) + "my-extra-command &\n").unwrap();
    let env_local = t.labwc.join(session::LOCAL_ENVIRONMENT);
    fs::write(&env_local, t.read(&env_local) + "MY_VARIABLE=1\n").unwrap();
    let snapshot = tree(&t.home);
    let (changes, _) = session::changes_for(&t.home, &source()).unwrap();
    assert!(changes.is_empty(), "{:?}", changes.iter().map(|c| &c.path).collect::<Vec<_>>());
    assert_eq!(tree(&t.home), snapshot);
}

#[test]
fn new_personal_lines_are_appended_without_rewriting_local_files() {
    let Some(t) = live() else { return };
    t.install();
    let local = t.labwc.join("autostart.local");
    let original = t.read(&local);
    let startup = t.labwc.join("autostart");
    fs::write(&startup, t.read(&startup) + "\n# chat\nsystemctl --user start chat.service >/dev/null 2>&1 &\n").unwrap();
    t.install();
    let now = t.read(&local);
    assert!(now.starts_with(&original));
    assert!(now.contains("# chat\nsystemctl --user start chat.service"));
    assert_eq!(now.matches("nordpass").count(), 1);
}

#[test]
fn keybindings_are_portable_and_replace_waybar_commands() {
    let Some(t) = live() else { return };
    t.install();
    let text = t.read(&t.labwc.join("rc.xml"));
    let document = xml::parse(&text).unwrap();
    assert!(!text.contains(&t.home.display().to_string()));
    assert!(!text.contains("audio_manager.py --volume-down"));
    let root = document.root_element();
    let keyboard = xml::findall(root, "keyboard").next().unwrap();
    let mut binds: HashMap<String, Vec<xml::Node>> = HashMap::new();
    for keybind in xml::findall(keyboard, "keybind") {
        binds.entry(xml::get(keybind, "key").unwrap().to_owned()).or_default().push(keybind);
    }
    for key in ["W-l", "C-A-Delete", "W-Return", "W-slash", "XF86PowerOff", "XF86AudioPlay", "XF86AudioLowerVolume", "XF86MonBrightnessUp", "W-1", "W-S-4", "W-C-Left", "C-A-Right"] {
        assert_eq!(binds[key].len(), 1, "{key}");
    }
    let action = |key: &str| xml::findall(binds[key][0], "action").next().unwrap();
    assert_eq!(xml::get(action("W-l"), "command"), Some("~/.local/bin/tessel-shell-lock"));
    assert_eq!(xml::sorted_attributes(action("W-4")), [("name".into(), "GoToDesktop".into()), ("to".into(), "4".into())]);
    assert_eq!(xml::sorted_attributes(action("W-S-2")), [("name".into(), "SendToDesktop".into()), ("to".into(), "2".into())]);
    assert_eq!(xml::get(action("Super_L"), "command"), Some("~/.local/bin/tessel-dock --menu-hotkey"));
    assert_eq!(xml::get(action("W-F4"), "name"), Some("None"));
    assert!(text.contains("/usr/local/bin/tessel-capture --capture area"));
    let core = xml::findall(root, "core").next().unwrap();
    assert!(xml::findtext(core, "promptCommand").unwrap().starts_with("exec \"$HOME\"/.local/bin/tessel-prompt --message-stdin"));
    assert_eq!(xml::findtext(core, "errorCommand").unwrap(), "sh -c 'exec \"$HOME\"/.local/bin/tessel-prompt --error'");
    assert!(text.find("<default/>").unwrap() < text.find(session::BLOCK_BEGIN).unwrap());
}

#[test]
fn menu_opens_configured_terminal() {
    let Some(t) = live() else { return };
    t.install();
    let text = t.read(&t.labwc.join("menu.xml"));
    assert!(text.contains("command=\"~/.local/bin/tessel-action terminal\""));
    assert!(text.contains("command=\"~/.local/bin/tessel-dock --menu\""));
    assert!(!text.contains("ghostty"));
}

#[test]
fn units_use_home_specifier_and_recover_from_crashes() {
    let Some(t) = live() else { return };
    t.install();
    let network = t.read(&t.units.join("tessel-network.service"));
    assert!(network.contains("ExecStart=/usr/bin/python3 %h/.config/waybar/scripts/tessel-network/network_manager.py"));
    for (unit, environment) in [("tessel-network.service", false), ("tessel-power.service", true), ("tessel-notifications.service", true)] {
        let dropin = t.read(&t.units.join(format!("{unit}.d")).join(session::RECOVERY_DROPIN));
        assert!(dropin.contains("OnFailure=tessel-failed@%n.service\n"));
        assert!(dropin.contains("RestartSec=1\n"));
        assert!(dropin.contains("StartLimitBurst=5\n"));
        assert_eq!(dropin.contains("90-local.env"), environment, "{unit}");
    }
    assert!(!t.units.join("tessel-lock.service.d").exists());
    assert!(!t.units.join("tessel-dock.service.d").exists());
    let template = t.read(&t.units.join("tessel-failed@.service"));
    assert!(template.contains("%h/.local/share/tessel/session/desktop_suite_failed.py %i"));
    assert!(!template.contains("[Install]"));
    for (path, _, _) in session::runtime_files(&t.home, &source()).unwrap() {
        assert!(path.is_file(), "{}", path.display());
    }
}

#[test]
fn missing_base_units_are_created_and_existing_units_kept() {
    let Some(t) = live() else { return };
    write(&t.units.join("tessel-shell.service"), "[Service]\nExecStart=/usr/bin/my-shell\n");
    t.install();
    assert_eq!(t.read(&t.units.join("tessel-shell.service")), "[Service]\nExecStart=/usr/bin/my-shell\n");
    for name in ["tessel-notifications.service", "way-displays.service"] {
        let text = t.read(&t.units.join(name));
        assert_eq!(text, fs::read_to_string(source().session.join("units").join(name)).unwrap());
        assert!(!text.contains("[Install]"));
    }
    let dropin = t.read(&t.units.join("tessel-notifications.service.d").join(session::RECOVERY_DROPIN));
    assert!(dropin.contains("90-local.env"));
    assert!(session::changes_for(&t.home, &source()).unwrap().0.is_empty());
    assert!(session::base_units(&t.home, &source()).unwrap().is_empty());
}

// ---- KeybindingTests ------------------------------------------------------------

const HOME: &str = "/home/example";

fn rc(keyboard: &str) -> String {
    format!("<?xml version=\"1.0\"?>\n<labwc_config>\n  <keyboard>\n    <default/>\n{keyboard}  </keyboard>\n</labwc_config>\n")
}

fn edit(text: &str) -> (String, Vec<String>) {
    session::edit_rc(text, Path::new(HOME)).unwrap()
}

#[test]
fn global_settings_rebind_is_kept() {
    let (text, _) = edit(&rc(""));
    let text = text.replace("<keybind key=\"W-l\">", "<keybind key=\"W-x\">");
    let (again, _) = edit(&text);
    assert_eq!(again, text);
    assert!(!again.contains("key=\"W-l\""));
}

#[test]
fn users_own_binding_is_never_replaced() {
    let (text, skipped) = edit(&rc("    <keybind key=\"W-Return\"><action name=\"Execute\" command=\"foot\"/></keybind>\n"));
    assert_eq!(skipped, ["W-Return"]);
    assert!(text.contains("command=\"foot\""));
    assert_eq!(text.matches("key=\"W-Return\"").count(), 1);
}

#[test]
fn old_suite_bindings_after_defaults_are_replaced() {
    let (text, skipped) = edit(&rc(&format!(
        "    <keybind key=\"XF86AudioMute\"><action name=\"Execute\" command=\"{HOME}/.config/waybar/scripts/tessel-audio/audio_manager.py --toggle-mute\"/></keybind>\n    <keybind key=\"W-2\"><action name=\"GoToDesktop\" to=\"2\"/></keybind>\n"
    )));
    assert!(skipped.is_empty());
    assert!(!text.contains("audio_manager.py"));
    assert_eq!(text.matches("key=\"W-2\"").count(), 1);
    assert_eq!(text.matches("key=\"XF86AudioMute\"").count(), 1);
}

#[test]
fn tiling_block_keeps_precedence_and_is_untouched() {
    let tiling = "    <!-- Tessel Tiling keyboard begin -->\n    <keybind key=\"W-C-Left\"><action name=\"Tiling\" command=\"group-prev\"/></keybind>\n    <!-- Tessel Tiling keyboard end -->\n";
    let (text, skipped) = edit(&rc(tiling));
    assert!(skipped.is_empty());
    assert!(text.contains(tiling));
    assert!(text.find(session::BLOCK_END).unwrap() < text.find("Tessel Tiling keyboard begin").unwrap());
}

#[test]
fn missing_keyboard_section_keeps_labwc_defaults() {
    let (text, _) = edit("<labwc_config>\n</labwc_config>\n");
    assert!(text.find("<default/>").unwrap() < text.find(session::BLOCK_BEGIN).unwrap());
    xml::parse(&text).unwrap();
    let (text, _) = edit("<labwc_config>\n  <keyboard/>\n</labwc_config>\n");
    assert!(text.find("<default/>").unwrap() < text.find(session::BLOCK_BEGIN).unwrap());
}

#[test]
fn commented_bindings_and_bindings_before_defaults_are_left_alone() {
    let text = "<labwc_config><keyboard>\n<keybind key=\"W-l\"><action name=\"Execute\" command=\"early\"/></keybind>\n<!-- <keybind key=\"W-Return\"><action name=\"Execute\" command=\"x\"/></keybind> -->\n<default/>\n</keyboard></labwc_config>\n";
    let (result, skipped) = edit(text);
    assert!(skipped.is_empty());
    assert!(result.contains("command=\"early\""));
    assert!(result.contains("<!-- <keybind key=\"W-Return\">"));
}

#[test]
fn invalid_documents_are_rejected() {
    let home = Path::new(HOME);
    for (text, message) in [
        ("<!DOCTYPE x><labwc_config/>", "labwc XML document types and entities are not supported."),
        ("<openbox_config/>", "Expected a labwc_config document."),
        ("<labwc_config><keyboard/><keyboard/></labwc_config>", "Multiple labwc keyboard sections are not supported."),
        ("<labwc_config><keyboard><default/></keyboard><!-- Tessel keybindings begin --></labwc_config>", "Repair the markers of the labwc keybinding block: <!-- Tessel keybindings begin"),
        ("<labwc_config><keyboard><default/><!-- Tessel keybindings end --><!-- Tessel keybindings begin --></keyboard></labwc_config>", "Reversed labwc keybinding block: <!-- Tessel keybindings begin"),
        ("<labwc_config><keyboard><default/></keyboard><!-- Tessel Tiling keyboard begin --><!-- Tessel Tiling keyboard end --></labwc_config>", "A managed keybinding block is outside the keyboard section."),
    ] {
        assert_eq!(session::edit_rc(text, home).unwrap_err(), message);
    }
    assert!(session::edit_rc("<labwc_config>", home).is_err());
}

#[test]
fn theme_has_matching_title_fonts_12px_corners_and_no_dead_colors() {
    if suite_root().is_none() {
        return;
    }
    let live = data("live-rc.xml").replace("@HOME@", HOME);
    let (result, _) = edit(&live);
    let check = |text: &str, f: &dyn Fn(xml::Node)| {
        let document = xml::parse(text).unwrap();
        let theme = xml::findall(document.root_element(), "theme").next().unwrap();
        f(theme);
    };
    check(&result, &|theme| {
        assert_eq!(xml::findtext(theme, "cornerRadius").as_deref(), Some("12"));
        assert!(xml::findall(theme, "active").next().is_none());
        assert!(xml::findall(theme, "inactive").next().is_none());
        let fonts: HashMap<String, Vec<String>> = xml::findall(theme, "font")
            .map(|f| (xml::get(f, "place").unwrap().to_owned(), ["name", "size", "weight"].iter().map(|k| xml::findtext(f, k).unwrap()).collect()))
            .collect();
        assert_eq!(fonts["ActiveWindow"], ["Sans", "10", "SemiBold"]);
        assert_eq!(fonts["InactiveWindow"], fonts["ActiveWindow"]);
        let titlebar = xml::findall(theme, "titlebar").next().unwrap();
        assert_eq!(xml::findtext(titlebar, "layout").as_deref(), Some("icon:iconify,max,close"));
    });
    assert!(!result.contains("#282a36"));
    assert_eq!(edit(&result).0, result);
    for text in ["<labwc_config>\n</labwc_config>\n", "<labwc_config>\n  <theme/>\n</labwc_config>\n"] {
        check(&edit(text).0, &|theme| {
            assert_eq!(xml::findtext(theme, "cornerRadius").as_deref(), Some("12"));
            assert_eq!(xml::findall(theme, "font").count(), 2);
        });
    }
    let custom = "<labwc_config><theme><font place=\"ActiveWindow\"><name>Inter</name><size>11</size></font><active><something/></active></theme></labwc_config>\n";
    check(&edit(custom).0, &|theme| {
        let fonts: Vec<String> = xml::findall(theme, "font").map(|f| xml::findtext(f, "name").unwrap() + &xml::findtext(f, "size").unwrap()).collect();
        assert_eq!(fonts, ["Inter11", "Inter11"]);
        assert!(xml::findall(theme, "active").next().is_some());
    });
}

#[test]
fn statements_follow_shell_nesting_and_marked_blocks() {
    let script = "a \\\n  b\nif x; then\n  y\nfi\n# Begin Foo\nz\n# End Foo\necho 'if' \"fi\" # case\ncase $x in\n *) ;;\nesac\nmy-if-tool\n";
    let statements: Vec<_> = session::shell_statements(script).into_iter().filter(|(k, _)| *k == Kind::Code).map(|(_, t)| t).collect();
    assert_eq!(statements, ["a \\\n  b\n", "if x; then\n  y\nfi\n", "# Begin Foo\nz\n# End Foo\n", "echo 'if' \"fi\" # case\n", "case $x in\n *) ;;\nesac\n", "my-if-tool\n"]);
    assert_eq!(session::nesting("for i in 1; do $if -fi done_x"), 1);
    assert_eq!(session::statement_key("systemctl --user  restart  a@b.service >/dev/null &\n"), session::Key::Service("a@b.service".into()));
}

#[test]
fn units_are_portable_unless_home_has_a_percent() {
    let text = "[Service]\nExecStart=/home/u/bin/x /home/u/y\nDescription=/home/u/z\nWorkingDirectory=/home/u/w\n";
    assert_eq!(session::portable_unit(text, Path::new("/home/u")), "[Service]\nExecStart=%h/bin/x %h/y\nDescription=/home/u/z\nWorkingDirectory=%h/w\n");
    assert_eq!(session::portable_unit("ExecStart=/home/a%b/x\n", Path::new("/home/a%b")), "ExecStart=/home/a%b/x\n");
}

// ---- CommandLineTests -----------------------------------------------------------

#[test]
fn preview_writes_nothing() {
    let Some(root) = suite_root() else { return };
    let home = scratch("labwc-session-cli");
    write(&home.join(".config/labwc/autostart"), "custom-command &\n");
    let before = tree(&home);
    let output = Command::new(env!("CARGO_BIN_EXE_tessel-configure-session"))
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("DESKTOP_SUITE_SOURCE", &root)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(tree(&home), before);
    assert!(String::from_utf8_lossy(&output.stdout).contains("files prepared"));
}

#[test]
fn argument_errors_match_argparse() {
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_tessel-configure-session")).args(args).env("HOME", "/nonexistent").output().unwrap();
        (output.status.code().unwrap(), String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let (code, stderr) = run(&["--check", "--apply"]);
    assert_eq!(code, 2);
    assert!(stderr.ends_with(": error: argument --apply: not allowed with argument --check\n"), "{stderr}");
    let (code, stderr) = run(&["--apply=1"]);
    assert_eq!(code, 2);
    assert!(stderr.ends_with("error: argument --apply: ignored explicit argument '1'\n"));
    let (code, stderr) = run(&["--", "x"]);
    assert_eq!(code, 2);
    assert!(stderr.ends_with("error: unrecognized arguments: -- x\n"));
}
