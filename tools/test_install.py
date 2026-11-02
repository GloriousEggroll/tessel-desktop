#!/usr/bin/env python3
"""Tests for tools/install.py against a fake HOME (never the real one).

Run: TMPDIR=/run/user/1000/cbt/rust-installer python3 -m unittest tools/test_install.py
"""
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest

HERE = Path(__file__).resolve().parent
INSTALL = HERE / "install.py"
sys.path.insert(0, str(HERE))
import install  # noqa: E402

REAL_HOME = Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()
SCRATCH = Path(os.environ.get("TMPDIR") or f"/run/user/{os.getuid()}/cbt/rust-installer")
DSA_PROBE = 'grep -q -E -e "[\\"\']$2[\\"\']" "$1"'   # supports() in tessel-action
USER = [c for c in install.COMPONENTS if c.scope == "user" and not c.explicit]
EXPLICIT = [c for c in install.COMPONENTS if c.explicit]
SYSTEM = [c for c in install.COMPONENTS if c.scope == "system" and not c.explicit]
SYSTEM_SKIP = ("var/lib/tessel", "var/backups")
# Runs install.py as if euid were 0 (only ever together with --root <private prefix>; see run_system).
FAKE_ROOT_EUID = ("import os, runpy, sys; os.geteuid = lambda: 0; sys.argv = sys.argv[1:]; "
                  "runpy.run_path(sys.argv[0], run_name='__main__')")
# tessel-login/start-greeter as installed (the comment block trimmed); {lib} = <prefix>/usr/local/lib/tessel-login.
START_GREETER = """#!/bin/sh
# No user desktop autostart, portals, terminal shortcuts, or X11 fallback.
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
export GDK_BACKEND=wayland
export XDG_CONFIG_HOME=/etc/tessel-login
export PYTHONNOUSERSITE=1
umask 077
exec /usr/bin/dbus-run-session -- /usr/bin/labwc --config-dir /etc/tessel-login/labwc \\
    --session '/usr/bin/python3 -I {lib}/login_ui.py'
"""

PY_ENTRY = "#!/usr/bin/python3\n# python original of {name}\nimport time\nFLAGS = {flags!r}\ntime.sleep(60)\n"
UNIT = "[Unit]\nDescription={name}\n\n[Service]\nType=simple\nExecStart=/usr/bin/python3 %h/{entry} {args}\nRestart=on-failure\n"
RECOVERY = "[Unit]\nStartLimitBurst=5\n\n[Service]\nRestart=on-failure\nRestartSec=1\n"
LABWC = """#!/bin/sh
echo "$@" >> "$LABWC_LOG"
exit 0
"""
# A trimmed copy of the live rc.xml: the Python Alt+Tab keybinds, a commented-out copy that must stay,
# the prompt here-doc, odd spacing/quoting and an unrelated python3 keybind.
RC = """<?xml version="1.0"?>
<labwc_config>
  <core>
    <promptCommand>exec "$HOME"/.local/bin/tessel-prompt --message-stdin &lt;&lt;'WAYSHELL_PROMPT_x'
WAYSHELL_PROMPT_x</promptCommand>
  </core>
  <keyboard>
    <!-- <keybind key="A-Tab"><action name="Execute" command="/usr/bin/python3 ~/.local/share/tessel-shell/wayshell_cli.py switch-window"/></keybind> -->
    <keybind key="W-S-0"><action name="SendToDesktop" to="10"/></keybind>
    <keybind key="A-Tab">
      <action name="Execute" command="/usr/bin/python3 ~/.local/share/tessel-shell/wayshell_cli.py switch-window"/>
    </keybind>
    <keybind  key='A-S-Tab' >
      <action   name="Execute"  command='/usr/bin/python3 ~/.local/share/tessel-shell/wayshell_cli.py switch-window --reverse' />
    </keybind>
    <keybind key="W-Tab"><action name="Execute" command="/usr/bin/python3 ~/.local/share/tessel-shell/wayshell_cli.py show-activities"/></keybind>
    <keybind key="Super_L" onRelease="yes">
      <action name="Execute" command="~/.local/bin/tessel-dock --menu-hotkey"/>
    </keybind>
  </keyboard>
</labwc_config>
"""
RC_PY_TAB = "/usr/bin/python3 ~/.local/share/tessel-shell/wayshell_cli.py switch-window"
RC_RUST_TAB = "~/.local/share/tessel-shell/tessel-shell-cli switch-window"
SYSTEMCTL = """#!/bin/sh
echo "$@" >> "$SYSTEMCTL_LOG"
case "$*" in
  *is-active*) [ -n "$SYSTEMCTL_ACTIVE" ] && exit 0; exit 3 ;;
  *show*) echo 0 ;;
esac
exit 0
"""


def tree(root, skip=(".local/state",)):
    """{relative path: (sha256 | link, mode)} for every file, link and directory under root."""
    result = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if any(relative == s or relative.startswith(s + "/") for s in skip):
            continue
        if path.is_symlink():
            result[relative] = ("link", os.readlink(path))
        elif path.is_dir():
            result[relative] = ("dir", path.stat().st_mode & 0o777)
        else:
            result[relative] = (hashlib.sha256(path.read_bytes()).hexdigest(), path.stat().st_mode & 0o777)
    return result


def scripts(component):
    return f"{component.base}/{component.dir}"


def forwards_args(shim):
    """Porter shims for status streams may drop the arguments (tiling_status.py)."""
    return not shim.porter or "sys.argv[1:]" in (install.REPO / shim.porter).read_text()


class InstallerTest(unittest.TestCase):
    def setUp(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(prefix="install-test-", dir=SCRATCH)).resolve()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.home = self.root / "home"
        assert self.home != REAL_HOME and not str(self.home).startswith(str(REAL_HOME) + "/")
        self.prebuilt = self.root / "prebuilt"
        self.locale = self.root / "locale"
        self.bin = self.root / "bin"
        self.log = self.root / "systemctl.log"
        self.labwc_log = self.root / "labwc.log"
        for directory in (self.home, self.prebuilt, self.locale, self.bin):
            directory.mkdir()
        (self.bin / "systemctl").write_text(SYSTEMCTL)
        (self.bin / "systemctl").chmod(0o755)
        (self.bin / "labwc").write_text(LABWC)
        (self.bin / "labwc").chmod(0o755)
        (self.bin / "rpm").write_text("#!/bin/sh\n[ -n \"$FAKE_RPM_OK\" ]\n")
        (self.bin / "rpm").chmod(0o755)
        (self.bin / "cargo").write_text("#!/bin/sh\necho cargo must not run in tests >&2\nexit 99\n")
        (self.bin / "cargo").chmod(0o755)
        for code in ("de", "fr"):
            (self.locale / f"{code}.json").write_text(json.dumps({"Hello": code}))
        self.make_binaries("v1")
        self.populate()

    def make_binaries(self, version):
        for component in install.COMPONENTS:
            for name in (*(cargo for cargo, _ in install.bins(component)), *component.helpers):
                path = self.prebuilt / name
                # Like the ports: drop the script argument of [python, script.py, ...] shims.
                path.write_text(f'#!/bin/sh\ncase "$1" in *.py) shift ;; esac\necho "rust-{name}-{version}" "$@"\n')
                path.chmod(0o755)

    def populate(self):
        home = self.home
        for component in USER:
            directory = home / scripts(component)
            directory.mkdir(parents=True, exist_ok=True)
            for shim in component.shims:
                entry = directory / shim.entry
                entry.write_text(PY_ENTRY.format(name=shim.entry, flags=shim.flags))
                entry.chmod(0o755)
            for keep in component.keeps:
                (directory / keep).parent.mkdir(parents=True, exist_ok=True)
                (directory / keep).write_text(f"# module {keep}\n")
            for helper in component.helpers:
                (directory / helper).write_text("#!/bin/sh\necho python-suite helper\n")
                (directory / helper).chmod(0o755)
            for unit in component.units:
                units = home / install.UNITS
                (units / (unit.name + ".d")).mkdir(parents=True, exist_ok=True)
                entry = component.shims[0].entry if component.shims else component.python_entries[0]
                (units / unit.name).write_text(UNIT.format(name=unit.name, entry=f"{scripts(component)}/{entry}",
                                                           args=" ".join(unit.args or ())))
                (units / (unit.name + ".d") / "50-tessel-recovery.conf").write_text(RECOVERY)
            for entry in component.python_entries:
                (directory / entry).write_text(PY_ENTRY.format(name=entry, flags=()))
        notif = home / install.UNITS / "tessel-notifications.service"
        notif.write_text("[Service]\nExecStart=dunst\n[Install]\nWantedBy=graphical-session.target\n")
        (home / install.UNITS / "tessel-notifications.service.d/tessel.conf").write_text(
            '[Service]\nExecStart=\nExecStart=/usr/bin/python3 "%h/.config/waybar/scripts/tessel-notifications/'
            'notification_manager.py" --daemon\n')
        weather = home / install.UNITS / "tessel-weather.service"
        weather.write_text("[Service]\nExecStart=/usr/bin/python3 %h/.local/share/tessel-weather/weather_manager.py "
                           "--tray --popup-edge top --popup-offset 40\n")
        loop = home / install.UNITS / "tessel-audio-loopbacks.service"
        loop.write_text('[Service]\nExecStart=/usr/bin/python3 "%h/.config/waybar/scripts/tessel-audio/'
                        'loopback_service.py"\nEnvironment=PYTHONUNBUFFERED=1\n')
        # As installed: tessel-shell.service's ExecStart is stale; palette.conf is what runs the daemon.
        units = home / install.UNITS
        (units / "tessel-shell.service").write_text(
            "[Unit]\nDescription=Tessel Shell Daemon\n\n[Service]\nEnvironmentFile=%h/.config/labwc/environment\n"
            "ExecStart=/usr/bin/tessel-shell\nRestart=on-failure\nType=simple\n\n[Install]\n"
            "WantedBy=graphical-session.target\n")
        (units / "tessel-shell.service.d/palette.conf").write_text(
            "[Service]\nExecStart=\nExecStart=%h/.local/share/tessel-shell/tessel-shell\n")
        (units / "tessel-dock.service").write_text(
            "[Unit]\nDescription=Tessel Dock\nPartOf=graphical-session.target\n\n[Service]\nType=notify\n"
            "NotifyAccess=main\nTimeoutStartSec=30\nEnvironment=GDK_BACKEND=wayland\n"
            'ExecStart=/usr/bin/python3 "%h/.local/share/tessel-dock/tessel-dock.py"\nRestart=on-failure\nRestartSec=1\n')
        (home / ".config/labwc").mkdir(parents=True)
        (home / install.RC_XML).write_text(RC)
        local_bin = home / ".local/bin"
        local_bin.mkdir(parents=True)
        for name, entry in (("tessel-dock", "tessel-dock/tessel-dock.py"), ("tessel-prompt", "tessel-shell/wayshell_prompt.py"),
                            ("tessel-capture-picker", "tessel-shell/capture_picker.py"),
                            ("tessel-hdr-calibration", "tessel-hdr-calibration/hdr_calibration.py")):
            (local_bin / name).write_text(f"#!/bin/sh\nexec /usr/bin/python3 {home}/.local/share/{entry} \"$@\"\n")
        (local_bin / "notifications").write_text(
            '#!/bin/sh\nexec /usr/bin/python3 "$HOME/.config/waybar/scripts/tessel-notifications/notification_manager.py" "$@"\n')
        for name, entry in (("waymedia", "tessel-media/media_manager.py"), ("tessel-tiling", "tessel-tiling/tiling_manager.py")):
            (local_bin / name).write_text(f"#!/bin/sh\nexec /usr/bin/python3 {home}/{install.SCRIPTS}/{entry} \"$@\"\n")
        (local_bin / "tessel-displays").write_text(
            f"#!/bin/sh\nexec /usr/bin/python3 {home}/.local/share/way-displays/tessel-displays \"$@\"\n")
        # The original tessel-bluetooth.py is a module Global Settings imports (tessel-bluetooth.App).
        (home / install.SCRIPTS / "tessel-bluetooth/tessel-bluetooth.py").write_text(
            "import btpopup\nORIGIN = __file__\n\n\nclass App:\n    embedded = True\n\n\n"
            "if __name__ == '__main__':\n    print('python tessel-bluetooth')\n")
        netman = home / install.SCRIPTS / "tessel-network"
        (netman / "network_manager.py").write_text(
            "import network_backend\nAPPLICATION_ID = 'org.tessel.NetworkManager'\nFALLBACK_PORTAL_URI = 'x'\n"
            "ORIGIN = __file__\n\n\nclass NetworkApplication:\n    pass\n\n\ndef arguments():\n    pass\n\n\n"
            "def portal_uri():\n    pass\n")
        (netman / "network_status.py").write_text("STREAM = 'python'\n")
        (local_bin / "tessel-keyboard").write_text(
            f"#!/bin/sh\nexec /usr/bin/python3 {home}/.config/waybar/scripts/tessel-keyboard/osk_manager.py \"$@\"\n")
        for launcher in local_bin.iterdir():
            launcher.chmod(0o755)
        apps = home / install.APPS
        apps.mkdir(parents=True)
        for desktop, entry in (("org.tessel.PowerManager.desktop", "tessel-power/power_manager.py"),
                               ("org.tessel.BatteryManager.desktop", "tessel-battery/battery_manager.py")):
            (apps / desktop).write_text(f"[Desktop Entry]\nName=X\nExec=/usr/bin/python3 {home}/{install.SCRIPTS}/{entry} "
                                        "--settings\nIcon=x\n")
        (apps / "org.tessel.Notifications.desktop").write_text("[Desktop Entry]\nExec=notifications --popup\n")
        (apps / "org.desktop.HDRCalibration.desktop").write_text(
            f"[Desktop Entry]\nName=HDR Calibration\nExec={home}/.local/bin/tessel-hdr-calibration\n")
        # As install_lock.py writes it (the porter's copy with the Python ExecStart).
        (home / install.UNITS / "tessel-lock.service").write_text(
            (install.REPO / "components/lock/tessel-lock.service").read_text().replace(
                "ExecStart=%h/.local/share/tessel-lock/tessel-lock",
                "ExecStart=/usr/bin/python3 %h/.local/share/tessel-lock/lock_screen.py"))
        (home / scripts(install.BY_KEY["tessel-lock"]) / "tessel-lock-pam").write_bytes(b"\x7fELF c pam helper")
        (home / scripts(install.BY_KEY["tessel-lock"]) / "tessel-lock-pam").chmod(0o755)

    def env(self, **extra):
        env = dict(os.environ, HOME=str(self.home), PATH=f"{self.bin}:{os.environ['PATH']}",
                   SYSTEMCTL_LOG=str(self.log), LABWC_LOG=str(self.labwc_log), TMPDIR=str(SCRATCH))
        for name in ("LABWC_PID", "WAYLAND_DISPLAY", "DISPLAY", "XDG_DATA_HOME", "KEEP_AWAKE_BINARY", "WAYTILE_BINARY", "TILING_STATUS_BINARY",
                     "NETWORK_DISCOVERY_BINARY"):
            env.pop(name, None)
        env.update(extra)
        return env

    def run_installer(self, *args, check=True, **env):
        argv = [sys.executable, str(INSTALL), "--home", str(self.home), "--prebuilt", str(self.prebuilt),
                "--locale-source", str(self.locale), *args]
        result = subprocess.run(argv, capture_output=True, text=True, env=self.env(**env))
        if check and result.returncode != 0:
            self.fail(f"{argv} failed ({result.returncode}):\n{result.stdout}\n{result.stderr}")
        return result

    def systemctl_calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def labwc_calls(self):
        return self.labwc_log.read_text().splitlines() if self.labwc_log.exists() else []

    def entry(self, key, name):
        return self.home / scripts(install.BY_KEY[key]) / name

    def statuses(self, only):
        return {item["component"]: item for item in json.loads(
            self.run_installer("--status", "--json", "--only", only).stdout)}

    # ---- tests ---------------------------------------------------------------------------

    def test_check_is_default_and_changes_nothing(self):
        before = tree(self.home, skip=())
        out = self.run_installer().stdout
        self.assertEqual(tree(self.home, skip=()), before)
        for component in USER:
            self.assertIn(component.key, out)
            for shim in component.shims:
                self.assertIn(str(self.home / scripts(component) / shim.entry), out)
        self.assertIn("ExecStart: /usr/bin/python3 %h/.config/waybar/scripts/tessel-keep-awake/keep_awake.py --background"
                      " -> %h/.config/waybar/scripts/tessel-keep-awake/tessel-keep-awake --background", out)
        self.assertIn('ExecStart: /usr/bin/python3 "%h/.config/waybar/scripts/tessel-notifications/'
                      'notification_manager.py" --daemon -> %h/.config/waybar/scripts/tessel-notifications/'
                      'tessel-notifications --daemon', out)
        self.assertIn('-> %h/.config/waybar/scripts/tessel-audio/tessel-audio-loopbacks\n', out)
        self.assertIn("shared/install_popup_fixes.py rewrites notification_manager.py", out)
        self.assertIn("still owns an unrelated application", out)
        self.assertIn("Preview only", out)
        self.assertNotIn("TODO-verify", out)
        self.assertEqual(self.systemctl_calls(), [])

    def test_apply_swaps_everything_without_restarting(self):
        before = tree(self.home)
        keeps = {(c.key, k): (self.home / scripts(c) / k).read_bytes() for c in USER for k in c.keeps}
        result = self.run_installer("--apply", "--i-am-at-the-keyboard")
        self.assertIn("were not restarted", result.stdout)
        home = self.home
        for component in USER:
            directory = home / scripts(component)
            for cargo, name in (*install.bins(component), *((h, h) for h in component.helpers)):
                self.assertEqual((directory / name).read_text(), (self.prebuilt / cargo).read_text())
                self.assertTrue(os.access(directory / name, os.X_OK))
            for shim in component.shims:
                entry = directory / shim.entry
                text = entry.read_text()
                self.assertIn("tessel-rust shim", text)
                compile(text, str(entry), "exec")
                self.assertTrue(text.startswith("#!"))
                self.assertEqual(entry.stat().st_mode & 0o777, 0o755)
                if shim.porter:
                    self.assertIn((install.REPO / shim.porter).read_text().split("\n", 1)[1], text)
                for flag in shim.flags:     # tessel-action's quoted-flag probe still matches
                    probe = subprocess.run(["sh", "-c", DSA_PROBE, "sh", str(entry), flag])
                    self.assertEqual(probe.returncode, 0, (shim.entry, flag))
                # The shim execs the binary next to it with the same arguments (tessel-dock uses -u).
                ran = subprocess.run(["/usr/bin/python3", "-u", str(entry), "--x", "a b"],
                                     capture_output=True, text=True, env=self.env())
                expected = f"rust-{shim.binary}-v1" + (" --x a b" if forwards_args(shim) else "")
                self.assertEqual(ran.stdout.strip(), expected, shim.entry)
            for unit in component.units:
                self.assertEqual(install.effective_execstart(home, unit.name),
                                 install.unit_command(home, component, unit))
                self.assertTrue((home / install.UNITS / (unit.name + ".d/50-tessel-recovery.conf")).is_file())
        self.assertEqual(keeps, {key: (home / scripts(install.BY_KEY[key[0]]) / key[1]).read_bytes() for key in keeps})
        self.assertIn('"--power-button"', self.entry("tessel-power", "power_manager.py").read_text())
        fallback = self.entry("tessel-keep-awake", "keep_awake_python.py")
        self.assertIn("python original of keep_awake.py", fallback.read_text())
        notifications = home / install.SCRIPTS / "tessel-notifications/tessel-notifications"
        self.assertIn(str(notifications), (home / ".local/bin/notifications").read_text())
        ran = subprocess.run([str(home / ".local/bin/tessel-keyboard"), "--settings"], capture_output=True, text=True)
        self.assertEqual(ran.stdout.strip(), "rust-osk-manager-v1 --settings")
        power = (home / install.APPS / "org.tessel.PowerManager.desktop").read_text()
        self.assertIn(f"Exec={home}/{install.SCRIPTS}/tessel-power/tessel-power --settings\n", power)
        self.assertEqual((home / install.APPS / "org.tessel.Notifications.desktop").read_text(),
                         "[Desktop Entry]\nExec=notifications --popup\n")
        self.assertEqual(json.loads((home / install.LOCALE / "fr.json").read_text()), {"Hello": "fr"})
        for name in ("tessel-keep-awake.service", "tessel-notifications.service", "tessel-notifications.service.d/tessel.conf"):
            self.assertEqual(tree(home)[f"{install.UNITS}/{name}"], before[f"{install.UNITS}/{name}"])
        # Only read-only queries (the locker must be inactive) besides the reload.
        self.assertEqual(self.systemctl_calls(), ["--user is-active --quiet tessel-lock.service",
                                                  "--user daemon-reload"])
        backups = list((home / install.BACKUPS).glob("rust-swap-*"))
        self.assertEqual(len(backups), 1)
        manifest = json.loads((backups[0] / "manifest.json").read_text())
        self.assertTrue(any(item["kind"] == "shim" and item["before"] == "file" for item in manifest))
        self.assertTrue(any(item["kind"] == "helper" and item["before"] == "file" for item in manifest))
        original = backups[0] / install.SCRIPTS / "tessel-keep-awake/keep_awake.py"
        self.assertIn("python original", original.read_text())
        # Same flags as the Python unit when the table does not fix them; TimeoutStopSec stays in the base unit.
        self.assertEqual(install.effective_execstart(home, "tessel-weather.service"),
                         "%h/.local/share/tessel-weather/tessel-weather --tray --popup-edge top --popup-offset 40")
        self.assertIn(f"{home}/{install.SCRIPTS}/tessel-media/tessel-media",
                      subprocess.run(["sh", "-x", str(home / ".local/bin/waymedia")], capture_output=True, text=True,
                                     env=self.env()).stderr)
        for name in ("tiling_manager_python.py", "tiling_status_python.py"):
            self.assertIn("python original", self.entry("tessel-tiling", name).read_text())
        self.assertIn("python original", self.entry("tessel-network-discovery", "discovery_manager_python.py").read_text())
        ran = subprocess.run([str(home / ".local/bin/tessel-displays"), "--x"], capture_output=True, text=True,
                             env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-tessel-displays-v1 --x")
        self.assertTrue((home / ".local/share/way-displays/tessel-displays-rs").is_file())
        self.assertIn("'--menu'", self.entry("tessel-session", "session_manager.py").read_text())
        self.assertIn("python original", self.entry("tessel-language", "language_manager_python.py").read_text())
        self.assertIn("nothing to change", self.run_installer("--apply", "--i-am-at-the-keyboard").stdout)

    def test_rollback_restores_exactly(self):
        before = tree(self.home)
        self.run_installer("--apply", "--i-am-at-the-keyboard")
        self.assertNotEqual(tree(self.home), before)
        self.assertIn("restore", self.run_installer("--rollback", "--check").stdout)
        self.assertNotEqual(tree(self.home), before)
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)
        self.assertEqual(json.loads((self.home / install.STATE).read_text())["components"], {})
        self.assertNotIn("restart", " ".join(self.systemctl_calls()))

    def test_only_and_partial_rollback(self):
        before = tree(self.home)
        self.run_installer("--apply", "--only", "tessel-keep-awake,tessel-audio")
        self.assertIn(install.SHIM_MARKER, self.entry("tessel-keep-awake", "keep_awake.py").read_text())
        self.assertIn(install.SHIM_MARKER, self.entry("tessel-audio", "loopback_service.py").read_text())
        self.assertNotIn(install.SHIM_MARKER, self.entry("tessel-power", "power_manager.py").read_text())
        self.assertFalse(self.entry("tessel-power", "tessel-power").exists())
        self.run_installer("--rollback", "--only", "tessel-keep-awake")
        after = tree(self.home)
        key = f"{install.SCRIPTS}/tessel-keep-awake/keep_awake.py"
        self.assertEqual(after[key], before[key])
        self.assertNotIn(f"{install.SCRIPTS}/tessel-keep-awake/keep_awake_python.py", after)
        self.assertIn(f"{install.LOCALE}/de.json", after)          # still used by the audio swap
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)

    def test_symlink_is_refused_and_nothing_written(self):
        entry = self.entry("tessel-keep-awake", "keep_awake.py")
        real = entry.with_name("real_keep_awake.py")
        entry.rename(real)
        entry.symlink_to(real.name)
        before = tree(self.home, skip=())
        result = self.run_installer("--apply", "--only", "tessel-keep-awake", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Refusing to replace a linked installed file", result.stderr)
        self.assertEqual(tree(self.home, skip=()), before)
        self.assertEqual(self.systemctl_calls(), [])

    def test_missing_python_component_blocks(self):
        (self.home / install.UNITS / "tessel-battery.service").unlink()
        result = self.run_installer("--apply", "--only", "tessel-battery", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tessel-battery.service is not installed", result.stderr)
        self.assertFalse(self.entry("tessel-battery", "tessel-battery").exists())

    def test_unbuilt_binary_blocks_apply(self):
        (self.prebuilt / "tessel-power-status").unlink()
        self.assertIn("not built", self.run_installer("--only", "tessel-power", check=False).stdout)
        result = self.run_installer("--apply", "--only", "tessel-power", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not built", result.stderr)
        self.assertFalse(self.entry("tessel-power", "tessel-power").exists())

    def test_restart_only_swapped_active_units(self):
        self.run_installer("--apply", "--only", "tessel-keep-awake", "--restart", SYSTEMCTL_ACTIVE="1")
        restarts = [call for call in self.systemctl_calls() if "restart" in call]
        self.assertEqual(restarts, ["--user restart tessel-keep-awake.service"])
        self.log.unlink()
        self.run_installer("--rollback", "--only", "tessel-keep-awake", "--restart")   # unit inactive: left stopped
        self.assertNotIn("--user restart tessel-keep-awake.service", self.systemctl_calls())

    def test_shell_units_need_restart_shell(self):
        component = install.Component("tessel-shell", "tessel-shell", "tessel-shell", ("tessel-shell",), shims=(),
                                      units=(install.Unit("tessel-shell.service", "tessel-shell"),))
        calls = []
        original = install.systemctl
        install.systemctl = lambda *args, check=False: calls.append(args) or subprocess.CompletedProcess(args, 0)
        try:
            install.restart_units([component], self.home, allow_shell=False)
            self.assertEqual(calls, [])
            install.restart_units([component], self.home, allow_shell=True)
            self.assertIn(("restart", "tessel-shell.service"), calls)
        finally:
            install.systemctl = original

    def test_reapply_upgrade_then_rollback_restores_original(self):
        before = tree(self.home)
        self.run_installer("--apply", "--only", "tessel-keep-awake")
        self.make_binaries("v2")
        self.run_installer("--apply", "--only", "tessel-keep-awake")
        ran = subprocess.run(["/usr/bin/python3", str(self.entry("tessel-keep-awake", "keep_awake.py"))],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-keep-awake-v2")
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)

    def test_overwritten_shim_is_reported_and_kept_on_rollback(self):
        self.run_installer("--apply", "--only", "tessel-power")
        entry = self.entry("tessel-power", "power_manager.py")
        entry.write_text("# newer python from install_all.py\n")
        report = self.statuses("tessel-power")["tessel-power"]
        self.assertEqual(report["configured"], "python (shim overwritten)")
        self.assertEqual(report["shims"]["power_manager.py"], "python (shim overwritten)")
        self.assertEqual(report["shims"]["power_status.py"], "rust")
        self.assertTrue(any("install_gtk4.py" in warning for warning in report["warnings"]))
        self.assertIn("python (shim overwritten)", self.run_installer("--status", "--only", "tessel-power").stdout)
        result = self.run_installer("--rollback", "--only", "tessel-power")
        self.assertIn("KEEP", result.stdout)
        self.assertEqual(entry.read_text(), "# newer python from install_all.py\n")
        self.assertFalse((self.home / install.UNITS / "tessel-power.service.d" / install.DROPIN).exists())

    def test_status_reports_mode_and_rss(self):
        entry = self.entry("tessel-battery", "battery_status.py")
        python = subprocess.Popen(["/usr/bin/python3", "-u", str(entry)])   # how tessel-dock runs status streams
        self.addCleanup(lambda: (python.kill(), python.wait()))
        time.sleep(0.3)
        report = self.statuses("tessel-keep-awake,tessel-battery")
        self.assertEqual(report["tessel-battery"]["configured"], "python")
        self.assertEqual([p["impl"] for p in report["tessel-battery"]["processes"]], ["python"])
        self.assertGreater(report["tessel-battery"]["rss_kb"], 0)
        self.assertEqual(report["tessel-battery"]["warnings"], [])
        python.kill()
        python.wait()
        self.run_installer("--apply", "--only", "tessel-keep-awake")
        binary = self.entry("tessel-keep-awake", "tessel-keep-awake")
        rust = subprocess.Popen(["sh", "-c", f'exec -a "{binary}" sleep 30'])
        self.addCleanup(lambda: (rust.kill(), rust.wait()))
        time.sleep(0.3)
        report = self.statuses("tessel-keep-awake,tessel-battery")
        self.assertEqual(report["tessel-keep-awake"]["configured"], "rust")
        self.assertEqual([p["impl"] for p in report["tessel-keep-awake"]["processes"]], ["rust"])
        self.assertEqual(report["tessel-battery"]["configured"], "python")
        self.assertTrue(report["tessel-keep-awake"]["units"]["tessel-keep-awake.service"]["dropin"])
        table = self.run_installer("--status").stdout
        self.assertIn("tessel-keep-awake", table)
        self.assertIn("MB", table)
        self.assertFalse(any("restart" in call for call in self.systemctl_calls()))

    def test_authentication_needs_confirmation(self):
        before = tree(self.home, skip=())
        result = self.run_installer("--apply", "--only", "tessel-keep-awake,tessel-auth", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--i-am-at-the-keyboard", result.stderr)
        self.assertEqual(tree(self.home, skip=()), before)
        self.assertIn("needs --i-am-at-the-keyboard", self.run_installer("--only", "tessel-auth").stdout)
        self.run_installer("--apply", "--only", "tessel-auth", "--i-am-at-the-keyboard")
        self.assertEqual(install.effective_execstart(self.home, "tessel-auth.service"),
                         "%h/.local/share/tessel-auth/tessel-auth")
        self.run_installer("--rollback", "--only", "tessel-auth")      # rollback needs no confirmation
        self.assertEqual(tree(self.home), {k: v for k, v in before.items() if not k.startswith(".local/state")})

    def test_optional_home_assistant_weather_is_skipped_when_absent(self):
        shutil.rmtree(self.home / ".local/share/tessel-weather")
        (self.home / install.UNITS / "tessel-weather.service").unlink()
        out = self.run_installer("--only", "tessel-weather,weather-manager-ha").stdout
        self.assertIn("is not installed (optional)", out)
        self.run_installer("--apply", "--only", "tessel-weather,weather-manager-ha")
        self.assertIn(install.SHIM_MARKER, self.entry("tessel-weather", "weather_status.py").read_text())
        self.assertFalse((self.home / ".local/share/tessel-weather").exists())

    def test_restart_terminates_resident_calendar_only(self):
        binary = self.entry("tessel-calendar", "tessel-calendar")
        self.run_installer("--apply", "--only", "tessel-calendar")
        calendar = subprocess.Popen(["sh", "-c", f'exec -a "{binary}" sleep 30'])
        other = subprocess.Popen(["sleep", "30"])
        self.addCleanup(lambda: [(p.kill(), p.wait()) for p in (calendar, other)])
        time.sleep(0.3)
        result = self.run_installer("--apply", "--only", "tessel-calendar", "--restart")
        self.assertIn(f"kill -TERM {calendar.pid}", result.stdout)
        self.assertEqual(calendar.wait(timeout=5), -15)
        self.assertIsNone(other.poll())
        self.assertFalse(any("restart" in call for call in self.systemctl_calls()))

    def test_tessel-bluetooth_shim_still_imports_the_python_module(self):
        self.run_installer("--apply", "--only", "tessel-bluetooth")
        directory = self.entry("tessel-bluetooth", "tessel-bluetooth.py").parent
        self.assertIn("class App", (directory / "tessel-bluetooth_gtk.py").read_text())
        # Global Settings' applet_pages.py: sys.path gets the scripts dir, then `import tessel-bluetooth`.
        probe = ("import sys; sys.path.insert(0, sys.argv[1]); import tessel-bluetooth; "
                 "print(tessel-bluetooth.App.embedded, tessel-bluetooth.ORIGIN.endswith('tessel-bluetooth_gtk.py'))")
        ran = subprocess.run([sys.executable, "-c", probe, str(directory)], capture_output=True, text=True,
                             env=self.env())
        self.assertEqual(ran.stdout.strip(), "True True", ran.stderr)
        ran = subprocess.run(["/usr/bin/python3", str(directory / "tessel-bluetooth.py"), "--popup"],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-tessel-bluetooth-v1 --popup")
        self.assertEqual(self.statuses("tessel-bluetooth")["tessel-bluetooth"]["configured"], "rust")
        self.assertEqual(self.statuses("way-displays")["way-displays"]["configured"], "python")

    def test_network_manager_shims_reexport_the_python_modules(self):
        self.assertIn("NetworkManager-libnm is not installed", self.run_installer("--only", "tessel-network").stdout)
        self.assertNotIn("NetworkManager-libnm is not installed",
                         self.run_installer("--only", "tessel-network", FAKE_RPM_OK="1").stdout)
        self.assertNotEqual(self.run_installer("--apply", "--only", "tessel-network", check=False).returncode, 0)
        self.run_installer("--apply", "--only", "tessel-network", "--i-am-at-the-keyboard")
        directory = self.entry("tessel-network", "network_manager.py").parent
        # Global Settings' applet_pages.py: sys.path gets the scripts dir, then `from network_manager import ...`.
        probe = ("import sys; sys.path.insert(0, sys.argv[1]); from network_manager import NetworkApplication, "
                 "APPLICATION_ID; import network_manager, network_status; "
                 "print(APPLICATION_ID, network_manager.ORIGIN.endswith('network_manager_impl.py'), network_status.STREAM)")
        ran = subprocess.run([sys.executable, "-c", probe, str(directory)], capture_output=True, text=True,
                             env=self.env())
        self.assertEqual(ran.stdout.strip(), "org.tessel.NetworkManager True python", ran.stderr)
        ran = subprocess.run(["/usr/bin/python3", str(directory / "network_manager.py"), "--settings"],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-network-manager-v1 --settings")
        self.assertEqual(install.effective_execstart(self.home, "tessel-network.service"),
                         "%h/.config/waybar/scripts/tessel-network/tessel-network --background")

    # ---- wave 3: tessel-shell (+ prompt, capture picker) and tessel-dock ---------------------------

    def expected_rc(self, rc=RC):
        """rc with exactly the two live Alt+Tab commands repointed (the commented copy and W-Tab stay)."""
        tab = f'\n      <action name="Execute" command="{RC_PY_TAB}"/>'
        reverse = f"command='{RC_PY_TAB} --reverse'"
        self.assertEqual((rc.count(tab), rc.count(reverse)), (1, 1))
        return (rc.replace(tab, f'\n      <action name="Execute" command="{RC_RUST_TAB}"/>')
                .replace(reverse, f"command='{RC_RUST_TAB} --reverse'"))

    def test_tessel-shell_swap_edits_only_the_alt_tab_commands(self):
        before = tree(self.home)
        rc = self.home / install.RC_XML
        units = self.home / install.UNITS
        palette = (units / "tessel-shell.service.d/palette.conf").read_bytes()
        result = self.run_installer("--apply", "--only", "tessel-shell", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--i-am-at-the-keyboard", result.stderr)
        self.assertEqual(tree(self.home), before)
        out = self.run_installer("--only", "tessel-shell").stdout
        self.assertIn(f"A-Tab: {RC_PY_TAB} -> {RC_RUST_TAB}", out)
        self.assertIn("ExecStart: %h/.local/share/tessel-shell/tessel-shell -> %h/.local/share/tessel-shell/tessel-shell-rs", out)
        self.assertIn("labwc --reconfigure only with --restart --restart-shell", out)
        self.assertIn("tessel-shell-cli switcher-status must exit 0", out)

        result = self.run_installer("--apply", "--only", "tessel-shell", "--i-am-at-the-keyboard")
        self.assertIn("labwc --reconfigure", result.stdout)
        self.assertEqual(rc.read_text(), self.expected_rc())          # every other byte preserved
        self.assertEqual(rc.stat().st_mode & 0o777, before[install.RC_XML][1])
        # zz- sorts after palette.conf, which stays byte-identical; the base unit too.
        self.assertEqual(install.effective_execstart(self.home, "tessel-shell.service"),
                         "%h/.local/share/tessel-shell/tessel-shell-rs")
        self.assertEqual((units / "tessel-shell.service.d/palette.conf").read_bytes(), palette)
        self.assertEqual(tree(self.home)[f"{install.UNITS}/tessel-shell.service"], before[f"{install.UNITS}/tessel-shell.service"])
        directory = self.home / ".local/share/tessel-shell"
        self.assertEqual((directory / "tessel-shell-rs").read_text(), (self.prebuilt / "tessel-shell").read_text())
        self.assertIn("python original of tessel-shell", (directory / "tessel-shell").read_text())     # kept for rollback
        ran = subprocess.run(["/usr/bin/python3", str(directory / "wayshell_cli.py"), "switch-window", "--reverse"],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-tessel-shell-cli-v1 switch-window --reverse")
        # labwc runs the new command through sh with ~ expanded: it reaches the binary.
        ran = subprocess.run(["sh", "-c", RC_RUST_TAB + " --reverse"], capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-tessel-shell-cli-v1 switch-window --reverse")
        self.assertEqual(self.labwc_calls(), [])
        self.assertFalse(any("restart" in call for call in self.systemctl_calls()))
        report = self.statuses("tessel-shell")["tessel-shell"]
        self.assertEqual(report["keybinds"], {"A-Tab": "rust", "A-S-Tab": "rust"})
        self.assertEqual(report["configured"], "rust")

        swapped = tree(self.home)
        self.assertIn("nothing to change", self.run_installer("--apply", "--only", "tessel-shell",
                                                              "--i-am-at-the-keyboard").stdout)
        self.assertEqual(tree(self.home), swapped)                   # re-apply is idempotent
        self.assertEqual(rc.read_text(), self.expected_rc())
        self.run_installer("--rollback", "--only", "tessel-shell")
        self.assertEqual(tree(self.home), before)
        self.assertEqual(rc.read_text(), RC)
        self.assertEqual(self.statuses("tessel-shell")["tessel-shell"]["keybinds"], {"A-Tab": "python", "A-S-Tab": "python"})

    def test_rc_xml_edited_after_the_swap_rolls_back_only_the_keybinds(self):
        rc = self.home / install.RC_XML
        theme = "  <theme><cornerRadius>8</cornerRadius></theme>\n  <keyboard>\n"
        self.run_installer("--apply", "--only", "tessel-shell", "--i-am-at-the-keyboard")
        # Global Settings saves into rc.xml after the swap.
        edited = rc.read_text().replace("  <keyboard>\n", theme)
        rc.write_text(edited)
        self.assertIn("revert", self.run_installer("--rollback", "--check", "--only", "tessel-shell").stdout)
        self.assertEqual(rc.read_text(), edited)
        result = self.run_installer("--rollback", "--only", "tessel-shell")
        self.assertIn("only the swapped keybind commands are put back", result.stdout)
        self.assertEqual(rc.read_text(), RC.replace("  <keyboard>\n", theme))
        self.assertFalse((self.home / ".local/share/tessel-shell/tessel-shell-rs").exists())
        # A keybind the user re-pointed elsewhere after the swap is left alone.
        self.run_installer("--apply", "--only", "tessel-shell", "--i-am-at-the-keyboard")
        custom = rc.read_text().replace(f'command="{RC_RUST_TAB}"', 'command="my-switcher"')
        rc.write_text(custom)
        self.run_installer("--rollback", "--only", "tessel-shell")
        self.assertIn('command="my-switcher"', rc.read_text())
        self.assertIn(f"command='{RC_PY_TAB} --reverse'", rc.read_text())

    def test_tessel-dock_prompt_and_picker_swap_change_only_execstart_and_launchers(self):
        before = tree(self.home)
        units = self.home / install.UNITS
        base = (units / "tessel-dock.service").read_bytes()
        only = "tessel-dock,tessel-prompt,tessel-capture-picker"
        self.assertNotEqual(self.run_installer("--apply", "--only", "tessel-dock", check=False).returncode, 0)
        self.run_installer("--apply", "--only", only, "--i-am-at-the-keyboard")
        dropin = (units / "tessel-dock.service.d" / install.DROPIN).read_text()
        body = [line for line in dropin.splitlines() if line and not line.startswith("#")]
        self.assertEqual(body, ["[Service]", "ExecStart=", "ExecStart=%h/.local/share/tessel-dock/tessel-dock"])
        self.assertEqual((units / "tessel-dock.service").read_bytes(), base)      # Type=notify, NotifyAccess=main stay
        self.assertIn("Type=notify\nNotifyAccess=main\n", base.decode())
        dock = self.home / ".local/share/tessel-dock"
        self.assertEqual((dock / "tessel-dock").read_text(), (self.prebuilt / "tessel-dock").read_text())
        for keep in set(install.BY_KEY["tessel-dock"].keeps) - set(install.BY_KEY["tessel-dock"].python_entries):
            self.assertEqual((dock / keep).read_text(), f"# module {keep}\n")
        self.assertIn("python original of tessel-dock.py", (dock / "tessel-dock.py").read_text())
        for launcher, binary, args in (("tessel-dock", "tessel-dock", ["--check"]),
                                       ("tessel-prompt", "tessel-prompt", ["--error"]),
                                       ("tessel-capture-picker", "tessel-capture-picker", [])):
            ran = subprocess.run([str(self.home / ".local/bin" / launcher), *args], capture_output=True, text=True,
                                 env=self.env())
            self.assertEqual(ran.stdout.strip(), " ".join([f"rust-{binary}-v1", *args]), launcher)
        self.assertEqual((self.home / install.RC_XML).read_text(), RC)          # only tessel-shell edits rc.xml
        self.assertEqual(json.loads((self.home / install.LOCALE / "de.json").read_text()), {"Hello": "de"})
        reports = self.statuses(only)
        self.assertEqual({k: v["configured"] for k, v in reports.items()},
                         {"tessel-dock": "rust", "tessel-prompt": "rust", "tessel-capture-picker": "rust"})
        swapped = tree(self.home)
        self.assertIn("nothing to change", self.run_installer("--apply", "--only", only,
                                                              "--i-am-at-the-keyboard").stdout)
        self.assertEqual(tree(self.home), swapped)
        self.run_installer("--rollback", "--only", only)
        self.assertEqual(tree(self.home), before)

    def test_shell_restart_needs_restart_shell_then_reconfigures_and_checks_health(self):
        self.run_installer("--apply", "--only", "tessel-shell,tessel-dock", "--i-am-at-the-keyboard", "--restart",
                           SYSTEMCTL_ACTIVE="1", LABWC_PID="1")
        self.assertFalse(any("restart " in call for call in self.systemctl_calls()))
        self.assertEqual(self.labwc_calls(), [])
        self.run_installer("--rollback")
        self.log.unlink()
        result = self.run_installer("--apply", "--only", "tessel-shell,tessel-dock", "--i-am-at-the-keyboard", "--restart",
                                    "--restart-shell", SYSTEMCTL_ACTIVE="1", LABWC_PID="1")
        restarts = [call for call in self.systemctl_calls() if "restart " in call]
        self.assertEqual(restarts, ["--user restart tessel-shell.service", "--user restart tessel-dock.service"])
        self.assertNotIn("labwc.service", " ".join(self.systemctl_calls()))
        self.assertEqual(self.labwc_calls(), ["--reconfigure"])                # a reload, never a restart
        self.assertIn(f"healthy: {self.home}/.local/share/tessel-dock/tessel-dock --check", result.stdout)
        self.assertIn(f"healthy: {self.home}/.local/share/tessel-shell/tessel-shell-cli switcher-status", result.stdout)
        # Rollback reloads the restored keybinds the same way; outside labwc it only says so.
        result = self.run_installer("--rollback", "--restart", "--restart-shell", SYSTEMCTL_ACTIVE="1")
        self.assertEqual(self.labwc_calls(), ["--reconfigure"])
        self.assertIn("no LABWC_PID", result.stdout)
        self.assertEqual((self.home / install.RC_XML).read_text(), RC)

    @unittest.skipUnless((REAL_HOME / install.RC_XML).is_file(), "no live rc.xml to copy")
    def test_live_rc_xml_copy_changes_only_the_two_commands(self):
        """Read-only copy of the user's rc.xml: the swap and its reverse touch only the Alt+Tab commands."""
        text = (REAL_HOME / install.RC_XML).read_text()
        component = install.BY_KEY["tessel-shell"]
        new, originals = install.rc_swap_text(text, component, self.home)
        if not originals:
            self.skipTest("the live rc.xml has no Python Alt+Tab keybind")
        old_lines, new_lines = text.splitlines(keepends=True), new.splitlines(keepends=True)
        self.assertEqual(len(old_lines), len(new_lines))
        changed = [(a, b) for a, b in zip(old_lines, new_lines) if a != b]
        self.assertLessEqual(len(changed), 2)
        for before, after in changed:
            self.assertIn("wayshell_cli.py switch-window", before)
            self.assertEqual(after, before.replace("/usr/bin/python3 ~/.local/share/tessel-shell/wayshell_cli.py",
                                                   "~/.local/share/tessel-shell/tessel-shell-cli"))
        self.assertEqual(install.rc_revert_text(new, component, originals), text)
        self.assertEqual(install.rc_swap_text(new, component, self.home)[0], new)

    def test_health_check_waits_for_ready_and_fails_loudly(self):
        self.run_installer("--apply", "--only", "tessel-dock", "--i-am-at-the-keyboard")
        binary = self.home / ".local/share/tessel-dock/tessel-dock"
        flag = self.root / "ready"
        binary.write_text(f'#!/bin/sh\nif [ -e "{flag}" ]; then echo \'{{"ready": true}}\'; '
                          'else echo \'{"ready": false}\'; fi\n')
        component = install.BY_KEY["tessel-dock"]
        with self.assertRaisesRegex(RuntimeError, "did not become healthy.*--rollback --only tessel-dock"):
            install.wait_healthy(self.home, component, timeout=0.3)
        flag.write_text("")
        install.wait_healthy(self.home, component, timeout=2)

    # ---- wave 4/5 user entries: tessel-hdr-calibration, tessel-lock ---------------------------------

    def test_hdr_calibration_swap_reapply_and_rollback(self):
        before = tree(self.home)
        desktop = (self.home / install.APPS / "org.desktop.HDRCalibration.desktop").read_bytes()
        self.run_installer("--apply", "--only", "tessel-hdr-calibration")
        directory = self.home / ".local/share/tessel-hdr-calibration"
        self.assertEqual((directory / "tessel-hdr-calibration").read_text(), (self.prebuilt / "tessel-hdr-calibration").read_text())
        # way-displays' color_controls.py: python3 .../hdr_calibration.py --output <connector>
        ran = subprocess.run(["/usr/bin/python3", str(directory / "hdr_calibration.py"), "--output", "DP-1"],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-hdr-calibration-v1 --output DP-1")
        ran = subprocess.run([str(self.home / ".local/bin/tessel-hdr-calibration"), "--sdr-editing"], capture_output=True,
                             text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-hdr-calibration-v1 --sdr-editing")
        self.assertEqual((directory / "hdr-pattern").read_text(), "# module hdr-pattern\n")     # the C helper stays
        self.assertEqual((self.home / install.APPS / "org.desktop.HDRCalibration.desktop").read_bytes(), desktop)
        self.assertEqual(self.statuses("tessel-hdr-calibration")["tessel-hdr-calibration"]["configured"], "rust")
        self.assertEqual(json.loads((self.home / install.LOCALE / "de.json").read_text()), {"Hello": "de"})
        swapped = tree(self.home)
        self.assertIn("nothing to change", self.run_installer("--apply", "--only", "tessel-hdr-calibration").stdout)
        self.assertEqual(tree(self.home), swapped)
        self.run_installer("--rollback", "--only", "tessel-hdr-calibration")
        self.assertEqual(tree(self.home), before)

    def test_lock_screen_swap_needs_confirmation_and_an_unlocked_session(self):
        before = tree(self.home)
        units = self.home / install.UNITS
        base = (units / "tessel-lock.service").read_bytes()
        result = self.run_installer("--apply", "--only", "tessel-lock", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--i-am-at-the-keyboard", result.stderr)
        result = self.run_installer("--apply", "--only", "tessel-lock", "--i-am-at-the-keyboard", check=False,
                                    SYSTEMCTL_ACTIVE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tessel-lock.service is active", result.stderr)
        self.assertEqual(tree(self.home), before)
        out = self.run_installer("--only", "tessel-lock").stdout
        self.assertIn("tessel-lock --check", out)
        self.assertIn("ExecStart: /usr/bin/python3 %h/.local/share/tessel-lock/lock_screen.py -> "
                      "%h/.local/share/tessel-lock/tessel-lock\n", out)

        result = self.run_installer("--apply", "--only", "tessel-lock", "--i-am-at-the-keyboard", "--restart")
        self.assertIn("WARNING: tessel-lock", result.stdout)
        self.assertNotIn("restart power-manager-lock", " ".join(self.systemctl_calls()))   # never restarted
        dropin = (units / "tessel-lock.service.d" / install.DROPIN).read_text()
        body = [line for line in dropin.splitlines() if line and not line.startswith("#")]
        self.assertEqual(body, ["[Service]", "ExecStart=", "ExecStart=%h/.local/share/tessel-lock/tessel-lock"])
        self.assertEqual((units / "tessel-lock.service").read_bytes(), base)   # Type=notify, LimitCORE=0 stay
        self.assertIn("Type=notify\nNotifyAccess=main\n", base.decode())
        directory = self.home / ".local/share/tessel-lock"
        for name in ("tessel-lock", "tessel-lock-pam"):
            mode = (directory / name).stat().st_mode
            self.assertEqual(stat.S_IMODE(mode), 0o755, name)
            self.assertFalse(mode & (stat.S_ISUID | stat.S_ISGID), name)
            self.assertEqual((directory / name).read_text(), (self.prebuilt / name).read_text())
        for keep in install.BY_KEY["tessel-lock"].keeps:
            self.assertEqual((directory / keep).read_text(), f"# module {keep}\n")
        # lock_settings.py: python3 lock_screen.py --preview
        ran = subprocess.run(["/usr/bin/python3", str(directory / "lock_screen.py"), "--preview"],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-lock-screen-v1 --preview")
        self.assertEqual(self.statuses("tessel-lock")["tessel-lock"]["configured"], "rust")
        swapped = tree(self.home)
        self.assertIn("nothing to change", self.run_installer("--apply", "--only", "tessel-lock",
                                                              "--i-am-at-the-keyboard").stdout)
        self.assertEqual(tree(self.home), swapped)
        self.run_installer("--rollback", "--only", "tessel-lock")
        self.assertEqual(tree(self.home), before)
        self.assertEqual((directory / "tessel-lock-pam").read_bytes(), b"\x7fELF c pam helper")

    # ---- --system entries (fake root prefix) ---------------------------------------------------

    def populate_system(self):
        root = self.root / "sysroot"
        lib = root / install.SYSTEM_LIB
        for component in SYSTEM:
            if component.fresh:
                continue                     # no Python original (tessel-archive)
            directory = root / scripts(component)
            directory.mkdir(parents=True, exist_ok=True)
            for keep in component.keeps:
                (directory / keep).write_text(f"# module {keep}\n")
            for launcher in component.launchers:
                relative, _porter, entry = install.launcher_spec(component, launcher)
                (root / relative).parent.mkdir(parents=True, exist_ok=True)
                (directory / Path(entry).name).write_text(PY_ENTRY.format(name=Path(entry).name, flags=()))
                # desktop-apps/install.py's wrapper (here with the private prefix).
                (root / relative).write_text(f'#!/bin/sh\n{install.STANDALONE_MARKER}\n'
                                             f'exec /usr/bin/python3 "{root / entry}" "$@"\n')
                (root / relative).chmod(0o755)
        apps = root / "usr/share/applications"
        apps.mkdir(parents=True)
        (apps / "org.tessel.Calculator.desktop").write_text(f"[Desktop Entry]\nExec={root}/usr/local/bin/calculator\n")
        login = root / "usr/local/lib/tessel-login"
        (login / "start-greeter").write_text(START_GREETER.format(lib=login))
        (login / "start-greeter").chmod(0o755)
        for relative in ("etc/tessel-login/greetd.toml", "etc/pam.d/tessel-login", "etc/pam.d/tessel-lock",
                         "etc/polkit-1/rules.d/50-tessel-login-power.rules", "etc/systemd/system/tessel-login.service",
                         "etc/security/tessel-login-faillock.conf"):
            (root / relative).parent.mkdir(parents=True, exist_ok=True)
            (root / relative).write_text(f"# {relative}\n")
        for directory in [root, *(p for p in root.rglob("*") if p.is_dir())]:
            directory.chmod(0o755)            # the safety check refuses group/world-writable parents
        return root

    def run_system(self, root, *args, euid0=True, check=True):
        """install.py --system --root <private prefix>; euid0 fakes euid 0 (only with the private --root)."""
        assert root != Path("/") and str(root).startswith(str(self.root) + "/")
        argv = [str(INSTALL), "--system", "--root", str(root), "--prebuilt", str(self.prebuilt),
                "--locale-source", str(self.locale), *args]
        argv = [sys.executable, "-c", FAKE_ROOT_EUID, *argv] if euid0 else [sys.executable, *argv]
        result = subprocess.run(argv, capture_output=True, text=True, env=self.env())
        if check and result.returncode != 0:
            self.fail(f"{argv} failed ({result.returncode}):\n{result.stdout}\n{result.stderr}")
        return result

    def test_system_mode_refuses_when_not_root(self):
        root = self.populate_system()
        before, home = tree(root, skip=()), tree(self.home, skip=())
        for action in ("--apply", "--rollback"):
            result = self.run_system(root, action, "--only", "calculator", "--i-am-at-the-keyboard",
                                     euid0=False, check=False)
            self.assertNotEqual(result.returncode, 0, action)
            self.assertIn(f"--system {action} needs root", result.stderr)
            self.assertIn("pkexec /usr/bin/python3", result.stderr)          # says how; never runs it
        self.assertEqual(tree(root, skip=()), before)
        self.assertEqual(tree(self.home, skip=()), home)
        out = self.run_system(root, "--check", euid0=False).stdout     # the preview works as the user
        self.assertIn("--system --apply/--rollback only as root", out)
        self.assertEqual(tree(root, skip=()), before)
        # System entries are not part of the per-user mode, and the reverse.
        result = self.run_installer("--apply", "--only", "calculator", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("calculator needs --system", result.stderr)
        result = self.run_system(root, "--apply", "--only", "tessel-keep-awake", check=False)
        self.assertIn("tessel-keep-awake needs neither --system", result.stderr)
        self.assertEqual(tree(root, skip=()), before)
        self.assertEqual(self.systemctl_calls(), [])

    def test_system_apps_swap_reapply_and_rollback(self):
        root = self.populate_system()
        before = tree(root, skip=("var",))
        apps = [c.key for c in SYSTEM if c.key != "tessel-login"]
        out = self.run_system(root, "--apply", "--only", ",".join(apps)).stdout
        self.assertIn("Nothing was restarted", out)
        for component in SYSTEM:
            if component.key == "tessel-login":
                continue
            directory = root / scripts(component)
            binary = install.installed_names(component)[0]
            self.assertEqual((directory / binary).read_text(), (self.prebuilt / binary).read_text())
            self.assertEqual(stat.S_IMODE((directory / binary).stat().st_mode), 0o755)
            for keep in component.keeps:
                self.assertEqual((directory / keep).read_text(), f"# module {keep}\n")
            self.assertEqual(json.loads((directory / "locale/fr.json").read_text()), {"Hello": "fr"})
            relative = install.launcher_spec(component, component.launchers[0])[0]
            launcher = (root / relative).read_text()
            lines = launcher.splitlines()
            self.assertEqual(lines[:2], ["#!/bin/sh", install.STANDALONE_MARKER])
            self.assertIn(install.SHIM_MARKER, lines[2])
            self.assertEqual(lines[3], f"exec {directory / binary} \"$@\"")
            self.assertEqual(stat.S_IMODE((root / relative).stat().st_mode), 0o755)
            ran = subprocess.run([str(root / relative), "--check"], capture_output=True, text=True, env=self.env())
            self.assertEqual(ran.stdout.strip(), f"rust-{binary}-v1 --check")
        self.assertTrue((root / install.SYSTEM_LIB / "tessel-files/tessel-files").is_file())        # the stable path
        self.assertTrue((root / install.SYSTEM_LIB / "tessel-capture/libcapture-pipewire.so").is_file())
        self.assertEqual(tree(root, skip=("var",))["usr/share/applications/org.tessel.Calculator.desktop"],
                         before["usr/share/applications/org.tessel.Calculator.desktop"])
        self.assertTrue((root / install.SYSTEM_STATE).is_file())
        self.assertEqual(len(list((root / install.SYSTEM_BACKUPS).glob("rust-swap-*"))), 1)
        self.assertFalse((self.home / install.STATE).exists())                 # nothing in the user's home
        self.assertFalse((self.home / install.LOCALE).exists())
        self.assertEqual(self.systemctl_calls(), [])
        status = json.loads(self.run_system(root, "--status", "--json", euid0=False).stdout)
        self.assertEqual({item["component"]: item["configured"] for item in status if item["component"] in apps},
                         {key: "rust" for key in apps})
        swapped = tree(root, skip=("var",))
        self.assertIn("nothing to change", self.run_system(root, "--apply", "--only", ",".join(apps)).stdout)
        self.assertEqual(tree(root, skip=("var",)), swapped)
        self.run_system(root, "--rollback")
        self.assertEqual(tree(root, skip=("var",)), before)
        self.assertEqual(json.loads((root / install.SYSTEM_STATE).read_text())["components"], {})

    def test_keyring_adds_pam_lines_and_rolls_back_exactly(self):
        sources = [install.REPO / source for _, source, _ in next(c for c in install.COMPONENTS if c.key == "keyring").files]
        if not all(p.exists() for p in sources):
            self.skipTest("cargo build --release -p tessel-keyring-ui -p pam-tessel-keyring first")
        root = self.populate_system()
        tessel-login = ("#%PAM-1.0\nauth required pam_succeed_if.so user != root quiet\nauth include tessel-login-auth\n"
                    "account include system-auth\nsession include system-auth\n"
                    "-session optional pam_gnome_keyring.so auto_start\n")
        passwd = ("#%PAM-1.0\npassword   substack\tsystem-auth\n-password   optional\tpam_gnome_keyring.so use_authtok\n"
                  "password   substack\tpostlogin\n")
        (root / "etc/pam.d/tessel-login").write_text(tessel-login)
        (root / "etc/pam.d/passwd").write_text(passwd)
        before = tree(root, skip=("var",))
        self.assertNotIn("keyring", self.run_system(root, "--check", euid0=False).stdout.split("Not shown")[0]
                         .replace("rust-installer", ""), "explicit: not part of a plain --system run")
        result = self.run_system(root, "--apply", "--only", "keyring", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(tree(root, skip=("var",)), before)
        self.run_system(root, "--apply", "--only", "keyring", "--i-am-at-the-keyboard")
        self.assertEqual((root / "etc/pam.d/tessel-login").read_text(),
                         "#%PAM-1.0\nauth required pam_succeed_if.so user != root quiet\nauth substack tessel-login-auth\n"
                         "-auth optional pam_waykeyring.so\naccount include system-auth\nsession include system-auth\n"
                         "-session optional pam_waykeyring.so\n-session optional pam_gnome_keyring.so auto_start\n")
        self.assertIn("-password   optional\tpam_tessel-keyring.so\npassword   substack\tpostlogin",
                      (root / "etc/pam.d/passwd").read_text())
        module = root / "usr/lib64/security/pam_waykeyring.so"
        self.assertEqual(module.read_bytes(), sources[1].read_bytes())
        self.assertTrue((root / install.SYSTEM_LIB / "keyring/tessel-keyring").exists())
        self.assertIn("nothing to change",
                      self.run_system(root, "--apply", "--only", "keyring", "--i-am-at-the-keyboard").stdout)
        self.run_system(root, "--rollback", "--only", "keyring")
        self.assertEqual(tree(root, skip=("var",)), before)

    def test_archive_manager_is_a_fresh_install_and_rolls_back_to_nothing(self):
        root = self.populate_system()
        before = tree(root, skip=("var",))
        self.assertFalse((root / install.SYSTEM_LIB / "tessel-archive").exists())
        out = self.run_system(root, "--check", "--only", "tessel-archive", euid0=False).stdout
        self.assertIn("create  launcher", out)
        self.assertIn(f"usr/local/share/applications/{install.ARCHIVE_DESKTOP}", out)
        self.assertEqual(tree(root, skip=("var",)), before)
        self.run_system(root, "--apply", "--only", "tessel-archive")
        directory = root / install.SYSTEM_LIB / "tessel-archive"
        self.assertEqual((directory / "tessel-archive").read_text(), (self.prebuilt / "tessel-archive").read_text())
        self.assertEqual(stat.S_IMODE((directory / "tessel-archive").stat().st_mode), 0o755)
        self.assertEqual(json.loads((directory / "locale/de.json").read_text()), {"Hello": "de"})
        launcher = root / install.SYSTEM_BIN / "tessel-archive"
        lines = launcher.read_text().splitlines()
        self.assertEqual(lines[:2], ["#!/bin/sh", install.STANDALONE_MARKER])
        self.assertIn(install.SHIM_MARKER, lines[2])
        self.assertEqual(lines[3], f"exec {directory / 'tessel-archive'} \"$@\"")
        ran = subprocess.run([str(launcher), "--check"], capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-tessel-archive-v1 --check")
        desktop = root / f"usr/local/share/applications/{install.ARCHIVE_DESKTOP}"
        self.assertEqual(desktop.read_bytes(), (install.REPO / install.ARCHIVE_DESKTOP_SOURCE).read_bytes())
        self.assertEqual(stat.S_IMODE(desktop.stat().st_mode), 0o644)
        self.assertIn("application/x-7z-compressed", install.ARCHIVE_MIMES)
        status = json.loads(self.run_system(root, "--status", "--json", "--only", "tessel-archive", euid0=False).stdout)
        self.assertEqual(status[0]["configured"], "rust")
        self.assertIn("nothing to change", self.run_system(root, "--apply", "--only", "tessel-archive").stdout)
        # Someone else's tessel-archive launcher is never replaced.
        self.run_system(root, "--rollback", "--only", "tessel-archive")
        self.assertEqual(tree(root, skip=("var",)), before)
        foreign = root / install.SYSTEM_BIN / "tessel-archive"
        foreign.write_text("#!/bin/sh\nexec /opt/other/tessel-archive \"$@\"\n")
        result = self.run_system(root, "--apply", "--only", "tessel-archive", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("was not installed by this tool", result.stderr)
        self.assertFalse((root / install.SYSTEM_LIB / "tessel-archive").exists())

    def test_archive_mime_defaults_only_when_named_and_revert_only_our_keys(self):
        mimeapps = self.home / ".config/mimeapps.list"
        mimeapps.parent.mkdir(parents=True, exist_ok=True)
        original = ("[Default Applications]\ninode/directory=org.tessel.Files.desktop\n"
                    "application/zip=org.kde.ark.desktop;\napplication/x-tar=org.kde.ark.desktop;\n"
                    "text/plain=org.tessel.TextEditor.desktop\n\n"
                    "[Added Associations]\napplication/zip=org.kde.ark.desktop;\n")
        mimeapps.write_text(original)
        self.run_installer("--apply", "--only", "tessel-keep-awake")
        self.assertEqual(mimeapps.read_text(), original)          # not part of the other user entries
        self.assertNotIn("archive-manager-mime", self.run_installer("--check").stdout)
        self.run_installer("--apply", "--only", "archive-manager-mime")
        text = mimeapps.read_text()
        defaults = text.split("[Added Associations]")[0]
        for mime in install.ARCHIVE_MIMES:
            self.assertEqual(defaults.count(f"\n{mime}={install.ARCHIVE_DESKTOP};\n"), 1, mime)
        self.assertIn("inode/directory=org.tessel.Files.desktop\n", text)
        self.assertIn("text/plain=org.tessel.TextEditor.desktop\n", text)
        self.assertTrue(text.endswith("[Added Associations]\napplication/zip=org.kde.ark.desktop;\n"))
        self.assertEqual(self.statuses("archive-manager-mime")["archive-manager-mime"]["configured"], "rust")
        backups = list((self.home / install.BACKUPS).glob("rust-swap-*/.config/mimeapps.list"))
        self.assertEqual([b.read_text() for b in backups], [original])
        # unchanged since: exact restore
        self.run_installer("--rollback", "--only", "archive-manager-mime")
        self.assertEqual(mimeapps.read_text(), original)
        # changed since (another default chosen, a new line): only our keys go back
        self.run_installer("--apply", "--only", "archive-manager-mime")
        edited = mimeapps.read_text().replace(f"application/x-tar={install.ARCHIVE_DESKTOP};",
                                              "application/x-tar=org.gnome.FileRoller.desktop;")
        edited = edited.replace("text/plain=org.tessel.TextEditor.desktop\n", "text/plain=gedit.desktop\n")
        mimeapps.write_text(edited)
        self.assertIn("revert", self.run_installer("--rollback", "--check", "--only", "archive-manager-mime").stdout)
        self.run_installer("--rollback", "--only", "archive-manager-mime")
        self.assertEqual(mimeapps.read_text(), original.replace("application/x-tar=org.kde.ark.desktop;",
                                                                "application/x-tar=org.gnome.FileRoller.desktop;")
                         .replace("text/plain=org.tessel.TextEditor.desktop", "text/plain=gedit.desktop"))
        # no mimeapps.list at all: created, and removed again
        mimeapps.unlink()
        self.run_installer("--apply", "--only", "archive-manager-mime")
        self.assertTrue(mimeapps.read_text().startswith("[Default Applications]\n"))
        self.run_installer("--rollback", "--only", "archive-manager-mime")
        self.assertFalse(mimeapps.exists())

    def test_mimeapps_text_helpers(self):
        text = "# comment\n[Default Applications]\na/b=x.desktop;\na/b=y.desktop;\n\n[Other]\nc/d=z\n"
        new, originals = install.mimeapps_swap_text(text, ("a/b", "c/d"), "w.desktop")
        self.assertEqual(new, "# comment\n[Default Applications]\na/b=w.desktop;\na/b=w.desktop;\nc/d=w.desktop;\n\n[Other]\nc/d=z\n")
        self.assertEqual(originals, {"a/b": ["x.desktop;", "y.desktop;"], "c/d": None})
        self.assertEqual(install.mimeapps_revert_text(new, originals, "w.desktop"), text)
        new, originals = install.mimeapps_swap_text("[Other]\nk=v", ("a/b",), "w.desktop")
        self.assertEqual(new, "[Other]\nk=v\n\n[Default Applications]\na/b=w.desktop;\n")

    def test_desktop_apps_guard_keeps_the_rust_launcher(self):
        """~/tessel-bluetooth/desktop-apps/install.py (loaded, never run) skips launchers carrying the swap marker."""
        source = REAL_HOME / "tessel-bluetooth/desktop-apps/install.py"
        if not source.is_file() or b"tessel-rust" not in source.read_bytes():
            self.skipTest("no guarded desktop-apps/install.py")
        root = self.populate_system()
        self.run_system(root, "--apply", "--only", "calculator")
        launcher = root / "usr/local/bin/calculator"
        swapped = launcher.read_bytes()
        import importlib.util
        spec = importlib.util.spec_from_file_location("desktop_apps_install_probe", source)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        python = f'#!/bin/sh\n{install.STANDALONE_MARKER}\nexec /usr/bin/python3 "x" "$@"\n'.encode()
        kept, skipped = module.keep_rust_swaps([(launcher, python, 0o755)])
        self.assertEqual((kept, skipped), ([], [launcher]))
        self.assertEqual(launcher.read_bytes(), swapped)

    def test_login_manager_changes_only_the_session_argument(self):
        root = self.populate_system()
        before = tree(root, skip=("var",))
        greeter = root / "usr/local/lib/tessel-login/start-greeter"
        original = greeter.read_text()
        result = self.run_system(root, "--apply", "--only", "tessel-login", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--i-am-at-the-keyboard", result.stderr)
        self.assertEqual(tree(root, skip=("var",)), before)
        out = self.run_system(root, "--check", "--only", "tessel-login", euid0=False).stdout
        self.assertIn("BROKEN GREETER BLOCKS GRAPHICAL LOGIN", out)
        self.assertIn("--system --rollback --only tessel-login", out)
        result = self.run_system(root, "--apply", "--only", "tessel-login", "--i-am-at-the-keyboard", "--restart")
        self.assertIn("BROKEN GREETER BLOCKS GRAPHICAL LOGIN", result.stdout)
        self.assertIn("--restart is ignored", result.stdout)
        lib = root / "usr/local/lib/tessel-login"
        changed = [(a, b) for a, b in zip(original.splitlines(), greeter.read_text().splitlines()) if a != b]
        self.assertEqual(changed, [(f"    --session '/usr/bin/python3 -I {lib}/login_ui.py'",
                                    f"    --session '{lib}/tessel-login'")])
        self.assertEqual(len(original.splitlines()), len(greeter.read_text().splitlines()))
        self.assertEqual(stat.S_IMODE(greeter.stat().st_mode), 0o755)
        for name in ("tessel-login", "tessel-session-transition", "tessel-login-helper"):
            self.assertEqual((lib / name).read_text(), (self.prebuilt / name).read_text())
        self.assertTrue((lib / "locale/de.json").is_file())
        after = tree(root, skip=("var",))
        for path in before:
            if path.startswith("etc/"):
                self.assertEqual(after[path], before[path], path)       # greetd.toml, PAM, polkit, unit, faillock
        self.assertEqual(self.systemctl_calls(), [])
        status = json.loads(self.run_system(root, "--status", "--json", "--only", "tessel-login", euid0=False).stdout)
        self.assertEqual(status[0]["configured"], "rust")
        swapped = tree(root, skip=("var",))
        self.assertIn("nothing to change", self.run_system(root, "--apply", "--only", "tessel-login",
                                                           "--i-am-at-the-keyboard").stdout)
        self.assertEqual(tree(root, skip=("var",)), swapped)
        self.run_system(root, "--rollback", "--only", "tessel-login")
        self.assertEqual(tree(root, skip=("var",)), before)
        # install_login.py rewrote start-greeter after the swap (here: one more export): only our string goes back.
        self.run_system(root, "--apply", "--only", "tessel-login", "--i-am-at-the-keyboard")
        greeter.write_text(greeter.read_text().replace("umask 077\n", "export X=1\numask 077\n"))
        self.assertIn("revert", self.run_system(root, "--rollback", "--check", euid0=False).stdout)
        self.run_system(root, "--rollback")
        self.assertEqual(greeter.read_text(), original.replace("umask 077\n", "export X=1\numask 077\n"))

    def test_system_mode_refuses_unsafe_directories_and_odd_greeters(self):
        root = self.populate_system()
        directory = root / install.SYSTEM_LIB / "calculator"
        directory.chmod(0o777)
        before = tree(root, skip=("var",))
        result = self.run_system(root, "--apply", "--only", "calculator", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("group/world-writable", result.stderr)
        self.assertEqual(tree(root, skip=("var",)), before)
        directory.chmod(0o755)
        greeter = root / "usr/local/lib/tessel-login/start-greeter"
        greeter.write_text(greeter.read_text().replace("login_ui.py'", "login_ui.py --debug'"))
        result = self.run_system(root, "--apply", "--only", "tessel-login", "--i-am-at-the-keyboard", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not contain exactly one", result.stderr)

    # ---- --target-root entry (Kasumi, aarch64) ---------------------------------------------------

    def populate_kasumi(self):
        target = self.root / "kasumi-home"
        component = install.BY_KEY["home-assistant-screen"]
        directory = target / scripts(component)
        directory.mkdir(parents=True)
        for shim in component.shims:
            (directory / shim.entry).write_text(PY_ENTRY.format(name=shim.entry, flags=shim.flags))
            (directory / shim.entry).chmod(0o755)
        for keep in component.keeps:
            (directory / keep).write_text(f"# module {keep}\n")
        units = target / install.UNITS
        units.mkdir(parents=True)
        (target / ".local").mkdir()
        (units / "luna-clock.service").write_text(
            "[Service]\nExecStart=/usr/bin/python3 %h/makima/screensaver/luna_clock.py\nRestart=always\n")
        return target

    def test_home_assistant_screen_only_into_an_explicit_target_root(self):
        home_before = tree(self.home, skip=())
        result = self.run_installer("--only", "home-assistant-screen", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("needs --target-root", result.stderr)
        for protected in (self.home, REAL_HOME, Path("/")):
            result = self.run_installer("--apply", "--only", "home-assistant-screen", "--target-root", str(protected),
                                        check=False, HOME=str(protected))
            self.assertNotEqual(result.returncode, 0, protected)
            self.assertIn("never the local home", result.stderr)
        target = self.populate_kasumi()
        before = tree(target)
        out = self.run_installer("--target-root", str(target)).stdout
        self.assertIn("ExecStart: /usr/bin/python3 %h/makima/screensaver/luna_clock.py -> "
                      "%h/makima/screensaver/luna-clock", out)
        self.assertEqual(tree(target), before)
        result = self.run_installer("--apply", "--target-root", str(target), "--restart")
        self.assertIn("On the Kasumi device: systemctl --user daemon-reload", result.stdout)
        directory = target / "makima/screensaver"
        for name in ("luna-clock", "voice-controls"):
            self.assertEqual((directory / name).read_text(), (self.prebuilt / name).read_text())
        ran = subprocess.run(["/usr/bin/python3", str(directory / "voice_controls.py"), "brightness", "up"],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-voice-controls-v1 brightness up")
        self.assertEqual(install.effective_execstart(target, "luna-clock.service"), "%h/makima/screensaver/luna-clock")
        self.assertEqual((directory / "config.json").read_text(), "# module config.json\n")
        self.assertTrue((target / install.LOCALE / "de.json").is_file())
        self.assertEqual(self.systemctl_calls(), [])                 # never this machine's systemd
        self.assertEqual(tree(self.home, skip=()), home_before)       # never this machine's home
        swapped = tree(target)
        self.assertIn("nothing to change", self.run_installer("--apply", "--target-root", str(target)).stdout)
        self.assertEqual(tree(target), swapped)
        self.run_installer("--rollback", "--target-root", str(target))
        self.assertEqual(tree(target), before)
        self.assertEqual(tree(self.home, skip=()), home_before)

    def test_home_assistant_screen_refuses_non_aarch64_binaries(self):
        target = self.populate_kasumi()
        before = tree(target)
        host = Path("/usr/bin/true")
        if install.elf_machine(host) in (None, install.EM_AARCH64):
            self.skipTest("needs a non-aarch64 host ELF")
        shutil.copy(host, self.prebuilt / "luna-clock")
        result = self.run_installer("--apply", "--target-root", str(target), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("is not an aarch64 build", result.stderr)
        self.assertEqual(tree(target), before)

    def test_table_is_consistent(self):
        keys = [c.key for c in install.COMPONENTS]
        self.assertEqual(len(keys), len(set(keys)))
        for component in install.COMPONENTS:
            names = set(install.installed_names(component))
            self.assertTrue({s.binary for s in component.shims} | {u.binary for u in component.units} <= names)
            for shim in component.shims:
                self.assertTrue(shim.entry.endswith(".py"))
                self.assertTrue(all(flag.startswith("--") for flag in shim.flags))
                self.assertNotIn(shim.entry, component.keeps)
        self.assertIn("--power-button", install.BY_KEY["tessel-power"].shims[0].flags)


class CrateTableTest(unittest.TestCase):
    """The table matches what the ports actually build (components/<package>)."""

    def test_binaries_exist_in_the_crates(self):
        for component in install.COMPONENTS:
            cargo = (install.REPO / "components" / component.package / "Cargo.toml").read_text()
            bins = set(re.findall(r'\[\[bin\]\]\s*\nname\s*=\s*"([^"]+)"', cargo))
            self.assertTrue({cargo for cargo, _ in install.bins(component)} <= bins, (component.key, bins))
            for helper in component.helpers:
                self.assertIn(f'"{helper}"', (install.REPO / "components" / component.package / "build.rs").read_text())

    def test_porter_shims_exec_the_table_binary(self):
        for component in install.COMPONENTS:
            for shim in component.shims:
                if not shim.porter:
                    continue
                text = (install.REPO / shim.porter).read_text()
                self.assertEqual(Path(shim.porter).name, shim.entry)
                self.assertIn(f'"{shim.binary}"', text, shim.porter)
                compile(install.shim_text(component, shim), shim.entry, "exec")
            for launcher in component.launchers:
                if isinstance(launcher, tuple) and launcher[1]:
                    self.assertTrue((install.REPO / launcher[1]).is_file(), launcher)
        used = {}
        for component in install.COMPONENTS:
            used.setdefault(component.package, set()).update(s.entry for s in component.shims if s.porter)
        for package, entries in used.items():
            porter_dirs = [install.REPO / "components" / package / d for d in ("shim", "shims")]
            # configure_session.py must stay Python (installers import it): its CLI shim is not installed.
            shipped = {p.name for d in porter_dirs if d.is_dir() for p in d.glob("*.py")} - {"configure_session_cli.py"}
            self.assertEqual(shipped, entries, package)


if __name__ == "__main__":
    unittest.main()
