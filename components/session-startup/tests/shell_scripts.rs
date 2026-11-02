//! Shell pieces the port keeps verbatim: the tessel-action dispatcher
//! (ActionScriptTests of test_configure_session.py) and the generated ENV/START blocks
//! (test_configure_xdg_autostart.py). All external commands are logging fakes on PATH.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::*;
use tessel_session_startup::xdg;

struct Action {
    root: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    home: PathBuf,
}

fn action() -> Option<Action> {
    suite_root()?;
    let root = scratch("suite-action-test");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let home = root.join("home");
    fs::create_dir_all(home.join(".config/waybar/scripts")).unwrap();
    Some(Action { log: root.join("log"), bin, home, root })
}

impl Action {
    fn tool(&self, name: &str) {
        let path = self.bin.join(name);
        fs::write(&path, format!("#!/bin/sh\necho \"{name} $*\" >> \"{}\"\n", self.log.display())).unwrap();
        chmod(&path, 0o755);
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/bin/tessel-action");
        let mut command = Command::new("/bin/sh");
        command.arg(script).args(args).env_clear().env("HOME", &self.home).env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()));
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[test]
fn media_keys_use_playerctl() {
    let Some(t) = action() else { return };
    t.tool("playerctl");
    t.run(&["next"], &[]);
    assert_eq!(t.log(), "playerctl next\n");
}

#[test]
fn terminal_skips_a_missing_configured_terminal() {
    let Some(t) = action() else { return };
    t.tool("ptyxis");
    t.run(&["terminal"], &[("TERMINAL", "missing-terminal")]);
    assert_eq!(t.log(), "ptyxis \n");
}

#[test]
fn terminal_honours_configured_terminal_with_arguments() {
    let Some(t) = action() else { return };
    t.tool("footclient");
    t.run(&["terminal"], &[("TERMINAL", "footclient --title Shell")]);
    assert_eq!(t.log(), "footclient --title Shell\n");
}

#[test]
fn actions_run_the_installed_tessel_program() {
    let Some(t) = action() else { return };
    let lib = t.home.join(".local/lib/tessel");
    for name in ["tessel-session", "tessel-settings"] {
        write(&lib.join(name), &format!("#!/bin/sh\necho \"{name} $*\" >> \"{}\"\n", t.log.display()));
        chmod(&lib.join(name), 0o755);
    }
    t.run(&["session-menu"], &[]);
    t.run(&["shortcuts"], &[]);
    assert_eq!(t.log(), "tessel-session --menu\ntessel-settings --shortcuts\n");
    let _ = &t.root;
}

#[test]
fn lock_starts_the_lock_unit() {
    let Some(t) = action() else { return };
    let output = t.run(&["lock"], &[]);
    // systemctl is called by absolute path in the lock launcher only; the action uses PATH.
    t.tool("systemctl");
    t.run(&["lock"], &[]);
    assert_eq!(t.log(), "systemctl --user start tessel-lock.service\n", "{output:?}");
}

#[test]
fn unknown_action_fails() {
    let Some(t) = action() else { return };
    let output = t.run(&["nonsense"], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "usage: tessel-action ACTION\n");
}

fn stub(bin: &Path, name: &str, body: &str) {
    let path = bin.join(name);
    fs::write(&path, body).unwrap();
    chmod(&path, 0o755);
}

#[test]
fn shell_imports_optional_xdg_paths_only_when_set_before_reload() {
    let root = scratch("labwc-xdg-env");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = root.join("commands.log");
    for name in ["dbus-update-activation-environment", "systemctl"] {
        stub(&bin, name, &format!("#!/bin/sh\necho \"{name}|$*|$XDG_CURRENT_DESKTOP\" >> \"{}\"\n", log.display()));
    }
    for optional in [false, true] {
        let _ = fs::remove_file(&log);
        let mut command = Command::new("sh");
        command.arg("-c").arg(xdg::ENV_BLOCK).env_clear().env("PATH", format!("{}:/usr/bin:/bin", bin.display())).env("XDG_CURRENT_DESKTOP", "wlroots");
        if optional {
            command.env("XDG_CONFIG_HOME", root.join("config")).env("XDG_CONFIG_DIRS", root.join("system"));
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let calls: Vec<Vec<String>> = fs::read_to_string(&log).unwrap().lines().map(|l| l.split('|').map(str::to_owned).collect()).collect();
        assert_eq!(calls.iter().map(|c| c[0].as_str()).collect::<Vec<_>>(), ["dbus-update-activation-environment", "systemctl"]);
        assert_eq!(calls[0][2], "wlroots:labwc");
        assert_eq!(calls[1][1], "--user daemon-reload");
        for name in ["XDG_CONFIG_HOME", "XDG_CONFIG_DIRS"] {
            assert_eq!(calls[0][1].split(' ').any(|w| w == name), optional);
        }
    }
}

#[test]
fn app_autostart_waits_for_dock_start_to_finish() {
    // Model a slow cold-start readiness handshake: a background start would let XDG apps
    // observe the missing tray watcher before the marker is written.
    let root = scratch("labwc-xdg-start");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (ready, started) = (root.join("tray-ready"), root.join("apps-started"));
    stub(
        &bin,
        "systemctl",
        &format!(
            "#!/bin/sh\nfor last; do :; done\nif [ \"$last\" = tessel-dock.service ]; then sleep 0.15; touch \"{r}\"; else [ -e \"{r}\" ] || {{ echo 'Autostart raced the tray' >&2; exit 1; }}; touch \"{s}\"; fi\n",
            r = ready.display(),
            s = started.display()
        ),
    );
    let output = Command::new("sh").arg("-c").arg(xdg::START_BLOCK).env("PATH", format!("{}:/usr/bin:/bin", bin.display())).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(started.exists());
}

fn xdg_home(root: &Path) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let system = root.join("system");
    let labwc = home.join(".config/labwc");
    write(&labwc.join("autostart"), "#!/bin/sh\n# Keep systemd's graphical session active for desktop portals.\nsystemctl --user start existing-session-helper.service\n");
    write(&labwc.join("shutdown"), "#!/bin/sh\nsystemctl --user stop existing-session-helper.service\n");
    write(&labwc.join("environment"), "XDG_CURRENT_DESKTOP=wlroots\nUNCHANGED=value\n");
    for name in ["nm-applet", "blueman"] {
        write(&system.join(format!("autostart/{name}.desktop")), &format!("[Desktop Entry]\nType=Application\nName={name}\nExec={name}\nNotShowIn=KDE;\n"));
    }
    (home, system)
}

fn invoke_xdg(home: &Path, system: &Path, bin: &Path, apply: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tessel-xdg-autostart"));
    if apply {
        command.arg("--apply");
    }
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_DIRS", system)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("DESKTOP_SUITE_SOURCE", suite_root().unwrap())
        .output()
        .unwrap()
}

#[test]
fn xdg_preview_and_install_are_idempotent_and_do_not_activate_services() {
    if suite_root().is_none() {
        return;
    }
    let root = scratch("labwc-xdg-config");
    let (home, system) = xdg_home(&root);
    let log = root.join("commands.log");
    let bin = fake_bin(&root, &["systemctl", "dbus-update-activation-environment", "labwc"], &log);
    let before = tree(&home);
    let output = invoke_xdg(&home, &system, &bin, false);
    assert!(String::from_utf8_lossy(&output.stdout).contains("6 files prepared"));
    assert_eq!(tree(&home), before);
    let output = invoke_xdg(&home, &system, &bin, true);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Installed."), "{}", String::from_utf8_lossy(&output.stderr));
    let installed = tree(&home);
    let output = invoke_xdg(&home, &system, &bin, true);
    assert!(String::from_utf8_lossy(&output.stdout).contains("0 files prepared"));
    assert_eq!(tree(&home), installed);
    assert!(!log.exists(), "no service command may run");
    let target = fs::read_to_string(home.join(".config/systemd/user/labwc-xdg-autostart.target")).unwrap();
    assert!(!target.contains("[Install]"));
    let startup = fs::read_to_string(home.join(".config/labwc/autostart")).unwrap();
    assert!(startup.find("daemon-reload").unwrap() < startup.find("existing-session-helper.service").unwrap());
    assert!(startup.find("existing-session-helper.service").unwrap() < startup.find(xdg::START_BEGIN).unwrap());
    let shutdown = fs::read_to_string(home.join(".config/labwc/shutdown")).unwrap();
    assert!(shutdown.find("existing-session-helper.service").unwrap() < shutdown.find(xdg::STOP_BEGIN).unwrap());
    assert!(shutdown.contains("stop labwc-xdg-autostart.target graphical-session.target"));
}

#[test]
fn xdg_failed_validation_restores_files_and_removes_new_target() {
    if suite_root().is_none() {
        return;
    }
    let root = scratch("labwc-xdg-fail");
    let (home, system) = xdg_home(&root);
    // `sh -n` fails for every script, like the Python test's CalledProcessError.
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub(&bin, "sh", "#!/bin/bash\nexit 2\n");
    let before = tree(&home);
    let output = invoke_xdg(&home, &system, &bin, true);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("returned non-zero exit status 2."));
    let restored: std::collections::BTreeMap<_, _> =
        tree(&home).into_iter().filter(|(path, _)| !path.starts_with(".local/state/tessel/backups")).collect();
    assert_eq!(restored, before);
}
