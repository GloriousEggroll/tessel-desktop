//! Byte parity with the Python installers: each fixture home is built at the same path
//! under $TMPDIR, then `configure_session.py` / `configure_xdg_autostart.py` and the Rust
//! binaries run `--check`, `--apply`, `--check` against it with the same environment
//! (fake `systemctl`/`labwc` first on PATH, HOME = the copy). stdout, stderr, exit codes,
//! the resulting trees (contents + modes) and the fake command logs must be identical.
//! Skipped when python3 or the btmanager tree is missing.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::Command;

use common::*;

#[derive(Debug, PartialEq)]
struct Outcome {
    runs: Vec<(i32, String, String)>,
    tree: BTreeMap<PathBuf, (Vec<u8>, u32)>,
    log: String,
}

/// Replace backup timestamps (`session-20261029T…`) so runs compare.
fn normalize(text: &str) -> String {
    let pattern = regex::Regex::new(r"(session|xdg-autostart)-\d{8}T\d{12}").unwrap();
    pattern.replace_all(text, "$1-TIMESTAMP").into_owned()
}

fn run_all(fixture: &dyn Fn(&Path), root: &Path, rust: bool, xdg: bool, steps: &[&[&str]], extra_env: &[(&str, String)]) -> Outcome {
    let home = root.join("home");
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(&home).unwrap();
    fixture(&home);
    let log = root.join("commands.log");
    let bin = fake_bin(root, &["systemctl", "labwc"], &log);
    let suite = suite_root().unwrap();
    let mut runs = Vec::new();
    for args in steps {
        let mut command = if rust {
            // The Python-path shims exec the binary with the script name as argv[0].
            let mut command = Command::new(if xdg { env!("CARGO_BIN_EXE_configure-xdg-autostart") } else { env!("CARGO_BIN_EXE_configure-session") });
            command.arg0(if xdg { "configure_xdg_autostart.py" } else { "configure_session.py" });
            command
        } else {
            let mut command = Command::new("python3");
            command.arg("-B").arg(suite.join("session-startup").join(if xdg { "configure_xdg_autostart.py" } else { "configure_session.py" }));
            command
        };
        command
            .args(*args)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("LANG", "C.UTF-8")
            .env("DESKTOP_SUITE_SOURCE", &suite);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !rust && stderr.contains("Traceback (most recent call last)") {
            // configure_xdg_autostart.py has no handler: keep what preceded the traceback
            // and the exception message (the Rust binary prints just the message).
            let (before, rest) = stderr.split_once("Traceback (most recent call last):\n").unwrap();
            let message = rest.lines().last().unwrap_or("").split_once(": ").map(|(_, m)| format!("{m}\n")).unwrap_or_default();
            stderr = format!("{before}{message}");
        }
        runs.push((output.status.code().unwrap_or(-1), normalize(&String::from_utf8_lossy(&output.stdout)), normalize(&stderr)));
    }
    let tree = tree(&home)
        .into_iter()
        .map(|(path, value)| (PathBuf::from(normalize(&path.to_string_lossy())), value))
        .collect();
    Outcome { runs, tree, log: fs::read_to_string(&log).unwrap_or_default() }
}

fn compare(name: &str, xdg: bool, fixture: &dyn Fn(&Path), steps: &[&[&str]], extra_env: &[(&str, String)]) {
    if suite_root().is_none() || Command::new("python3").arg("-c").arg("import xml").output().is_err() {
        eprintln!("skipping {name}: python3 or btmanager missing");
        return;
    }
    let root = std::env::temp_dir().join(format!("parity-{}-{name}", std::process::id()));
    let python = run_all(fixture, &root, false, xdg, steps, extra_env);
    let rust = run_all(fixture, &root, true, xdg, steps, extra_env);
    let _ = fs::remove_dir_all(&root);
    for (index, (p, r)) in python.runs.iter().zip(&rust.runs).enumerate() {
        assert_eq!(p.0, r.0, "{name} step {index} exit code; python stderr: {}\nrust stderr: {}", p.2, r.2);
        assert_eq!(p.1, r.1, "{name} step {index} stdout");
        assert_eq!(p.2, r.2, "{name} step {index} stderr");
    }
    let keys = |o: &Outcome| o.tree.keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&python), keys(&rust), "{name} file list");
    for (path, value) in &python.tree {
        assert!(rust.tree[path] == *value, "{name}: {} differs:\npython:\n{}\nrust:\n{}", path.display(), String::from_utf8_lossy(&value.0), String::from_utf8_lossy(&rust.tree[path].0));
    }
    assert_eq!(python.log, rust.log, "{name} commands");
}

const SESSION_STEPS: &[&[&str]] = &[&["--check"], &["--apply"], &[], &["--apply"]];

fn labwc(home: &Path) -> PathBuf {
    home.join(".config/labwc")
}

#[test]
fn live_home_matches_python() {
    compare("live", false, &|home| live_home(home), SESSION_STEPS, &[]);
}

#[test]
fn live_home_inside_labwc_reconfigures() {
    compare("live-labwc", false, &|home| live_home(home), &[&["--apply"]], &[("LABWC_PID", "1234".into())]);
}

#[test]
fn empty_and_minimal_homes_match_python() {
    compare("empty", false, &|_| {}, SESSION_STEPS, &[]);
    compare("minimal", false, &|home| write(&labwc(home).join("autostart"), "custom-command &\n"), SESSION_STEPS, &[]);
}

#[test]
fn keybinding_variants_match_python() {
    let variants: Vec<(&str, String)> = vec![
        ("nokeyboard", "<labwc_config>\n</labwc_config>\n".into()),
        ("selfclosing", "<?xml version=\"1.0\"?>\n<labwc_config>\n  <keyboard/>\n  <theme/>\n</labwc_config>\n".into()),
        ("user", "<labwc_config>\n  <keyboard>\n    <default/>\n    <keybind key=\"W-Return\"><action name=\"Execute\" command=\"foot\"/></keybind>\n    <keybind key=\"w-l\"><action name=\"Execute\"><command>HOME/.local/bin/wayshell-lock</command></action></keybind>\n    <keybind key=\"XF86AudioMute\">\n      <action name=\"Execute\" command=\"HOME/.config/waybar/scripts/audio-manager/audio_manager.py --toggle-mute\"/>\n    </keybind>\n  </keyboard>\n</labwc_config>\n".into()),
        ("crlf", "<labwc_config>\r\n  <keyboard>\r\n    <default/>\r\n  </keyboard>\r\n  <theme>\r\n\t<font place=\"ActiveWindow\"><name>Inter &amp; Co</name><size> 11 </size></font>\r\n\t<font place=\"InactiveWindow\"><name>X</name></font>\r\n    <active><titlebar color=\"#282a36\"/></active>\r\n    <inactive><border/></inactive>\r\n    <cornerRadius>4</cornerRadius>\r\n  </theme>\r\n</labwc_config>\r\n".into()),
        ("prompts", "<labwc_config>\n  <core>\n    <promptCommand>HOME/bin/prompt 'x' &lt;&lt; EOF</promptCommand>\n    <errorCommand>  HOME/bin/err --a \"b\"  </errorCommand>\n  </core>\n  <keyboard><default/><!-- Waytile keyboard begin --><keybind key=\"W-C-Left\"><action name=\"X\"/></keybind><!-- Waytile keyboard end --></keyboard>\n  <mouse><context name=\"Root\"><mousebind button=\"Left\"><action name=\"Execute\" command='HOME/x'/></mousebind></context></mouse>\n</labwc_config>\n".into()),
        ("unicode", "<labwc_config>\n  <!-- ünïcödé comment with <keybind key=\"W-l\"/> -->\n  <keyboard>\n    <default/>\n    <keybind key=\"W-ä\"><action name=\"Execute\" command=\"bäz\"/></keybind>\n  </keyboard>\n</labwc_config>\n".into()),
    ];
    for (name, text) in variants {
        let text = text.clone();
        compare(
            &format!("rc-{name}"),
            false,
            &move |home| {
                let text = text.replace("HOME", &home.display().to_string());
                write(&labwc(home).join("rc.xml"), &text);
                write(&labwc(home).join("menu.xml"), &format!("<openbox_menu>\n<item label=\"T\"><action name=\"Execute\" command=\" ghostty \"/></item>\n<item label=\"F\"><action name=\"Execute\"><command>{}/bin/f</command></action></item>\n</openbox_menu>\n", home.display()));
            },
            &[&["--check"], &["--apply"], &["--check"]],
            &[],
        );
    }
    // A Global Settings rebind inside an existing block survives.
    compare(
        "rc-rebind",
        false,
        &|home| {
            let (text, _) = session_startup::session::edit_rc(&format!("<labwc_config>\n  <keyboard>\n    <default/>\n  </keyboard>\n</labwc_config>\n"), home).unwrap();
            write(&labwc(home).join("rc.xml"), &text.replace("<keybind key=\"W-l\">", "<keybind key=\"W-x\">").replace("key=\"C-A-Left\"", "key=\"A-Left\""));
        },
        &[&["--check"], &["--apply"], &["--check"]],
        &[],
    );
}

#[test]
fn startup_and_environment_variants_match_python() {
    compare(
        "env",
        false,
        &|home| {
            let l = labwc(home);
            write(&l.join("autostart"), "#!/bin/sh\n# helper\nif [ -x /x ]; then\n  /x &\nfi\nfoo \\\n  --bar &\n\n# Begin custom\nbaz\n# End custom\nsystemctl --user start waydock.service >/dev/null 2>&1 &\nsystemctl --user start chat.service &\ncase $X in a) b ;; esac\n");
            chmod(&l.join("autostart"), 0o700);
            write(&l.join("autostart.local"), "existing &");
            write(&l.join("environment"), "XDG_CURRENT_DESKTOP=sway\r\nexport FOO=1\r\n# comment for BAR\r\nBAR = two \r\nFOO=2\r\nMOZ_ENABLE_WAYLAND=1\r\nXKB_DEFAULT_LAYOUT=de\r\n# Desktop suite language — managed by Language Settings\r\nLANG=de_DE.UTF-8\r\n# End desktop suite language\r\n# way-displays-gui primary output environment begin\r\nX=1");
            write(&l.join("environment.d/90-local.env"), "BAR=two");
        },
        SESSION_STEPS,
        &[],
    );
}

#[test]
fn unit_variants_match_python() {
    compare(
        "units",
        false,
        &|home| {
            let units = home.join(".config/systemd/user");
            let h = home.display().to_string();
            write(&units.join("waydock.service"), &format!("[Service]\nExecStart={h}/.local/share/waydock/waydock\nEnvironmentFile=-{h}/.config/labwc/environment\nDescription={h}/x\n"));
            write(&units.join("waydock.service.d/.hidden.conf"), "[Service]\nX=1\n");
            write(&units.join("waydock.service.d/a.conf"), &format!("[Service]\nWorkingDirectory={h}/w\n"));
            write(&units.join("waydock.service.d/50-desktop-suite-recovery.conf"), "old\n");
            write(&units.join("keep-awake.service.d/z.conf"), "[Service]\nRestart=no\n");
            chmod(&units.join("waydock.service"), 0o600);
            write(&units.join("notifications.service"), "[Service]\nExecStart=/bin/true\n");
            write(&labwc(home).join("autostart"), "");
        },
        SESSION_STEPS,
        &[],
    );
}

#[test]
fn failures_match_python() {
    // Personal startup that does not parse: sh -n fails, every write is rolled back.
    compare("shfail", false, &|home| write(&labwc(home).join("autostart"), "if true; then\n  x\nfi\nfoo )\n"), &[&["--apply"]], &[]);
    // A symlinked rc.xml is refused.
    compare(
        "symlink",
        false,
        &|home| {
            write(&home.join("real-rc.xml"), "<labwc_config/>\n");
            fs::create_dir_all(labwc(home)).unwrap();
            symlink(home.join("real-rc.xml"), labwc(home).join("rc.xml")).unwrap();
        },
        &[&["--check"]],
        &[],
    );
    compare("doctype", false, &|home| write(&labwc(home).join("rc.xml"), "<!DOCTYPE labwc_config>\n<labwc_config/>\n"), &[&["--check"]], &[]);
    compare("argv", false, &|_| {}, &[&["--check", "--apply"], &["bogus"], &["--help"], &["--a=1"]], &[]);
    // Without systemctl the daemon-reload fails after the files are written.
    compare("nosystemctl", false, &|home| write(&labwc(home).join("autostart"), "x &\n"), &[&["--apply"]], &[("PATH", "/nonexistent-bin:/usr/bin:/bin".into())]);
}

fn xdg_fixture(home: &Path) {
    let l = labwc(home);
    write(&l.join("autostart"), "#!/bin/sh\n# Keep systemd's graphical session active for desktop portals.\nsystemctl --user start existing-session-helper.service\n");
    write(&l.join("shutdown"), "#!/bin/sh\nsystemctl --user stop existing-session-helper.service\n");
    write(&l.join("environment"), "XDG_CURRENT_DESKTOP=wlroots\nUNCHANGED=value\n");
    let system = home.parent().unwrap().join("system/autostart");
    for name in ["nm-applet", "blueman"] {
        write(&system.join(format!("{name}.desktop")), &format!("[Desktop Entry]\nType=Application\nName={name}\nExec={name}\nNotShowIn=KDE;\n"));
    }
    write(&home.join(".config/autostart/blueman.desktop"), "[Desktop Entry]\nOnlyShowIn=labwc;GNOME;\n[Desktop Action X]\nName=x\n");
}

#[test]
fn xdg_autostart_matches_python() {
    let root = std::env::temp_dir().join(format!("parity-{}-xdg", std::process::id()));
    let system = root.join("system").display().to_string();
    compare("xdg", true, &xdg_fixture, &[&[], &["--apply"], &["--apply"], &["--help"], &["-x"]], &[("XDG_CONFIG_DIRS", system)]);
}

#[test]
fn xdg_failures_match_python() {
    let root = std::env::temp_dir().join(format!("parity-{}-xdgfail", std::process::id()));
    let system = root.join("system").display().to_string();
    let broken = |home: &Path| {
        xdg_fixture(home);
        write(&labwc(home).join("shutdown"), "#!/bin/sh\nfoo )\n");
    };
    compare("xdgfail", true, &broken, &[&["--apply"]], &[("XDG_CONFIG_DIRS", system.clone())]);
    // No anchor in the startup script.
    let root = std::env::temp_dir().join(format!("parity-{}-xdganchor", std::process::id()));
    let system = root.join("system").display().to_string();
    compare("xdganchor", true, &|home: &Path| { xdg_fixture(home); write(&labwc(home).join("autostart"), "x\n"); }, &[&[]], &[("XDG_CONFIG_DIRS", system)]);
}
