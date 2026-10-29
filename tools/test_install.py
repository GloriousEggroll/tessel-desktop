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
DSA_PROBE = 'grep -q -E -e "[\\"\']$2[\\"\']" "$1"'   # supports() in desktop-suite-action

PY_ENTRY = "#!/usr/bin/python3\n# python original of {name}\nimport time\nFLAGS = {flags!r}\ntime.sleep(60)\n"
UNIT = "[Unit]\nDescription={name}\n\n[Service]\nType=simple\nExecStart=/usr/bin/python3 %h/{entry} {args}\nRestart=on-failure\n"
RECOVERY = "[Unit]\nStartLimitBurst=5\n\n[Service]\nRestart=on-failure\nRestartSec=1\n"
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
        for directory in (self.home, self.prebuilt, self.locale, self.bin):
            directory.mkdir()
        (self.bin / "systemctl").write_text(SYSTEMCTL)
        (self.bin / "systemctl").chmod(0o755)
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
        for component in install.COMPONENTS:
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
                (units / unit.name).write_text(UNIT.format(name=unit.name, entry=f"{scripts(component)}/"
                                                          f"{component.shims[0].entry}", args=" ".join(unit.args or ())))
                (units / (unit.name + ".d") / "50-desktop-suite-recovery.conf").write_text(RECOVERY)
        notif = home / install.UNITS / "notifications.service"
        notif.write_text("[Service]\nExecStart=dunst\n[Install]\nWantedBy=graphical-session.target\n")
        (home / install.UNITS / "notifications.service.d/desktop-suite.conf").write_text(
            '[Service]\nExecStart=\nExecStart=/usr/bin/python3 "%h/.config/waybar/scripts/notification-manager/'
            'notification_manager.py" --daemon\n')
        weather = home / install.UNITS / "weather-manager.service"
        weather.write_text("[Service]\nExecStart=/usr/bin/python3 %h/.local/share/weather-manager/weather_manager.py "
                           "--tray --popup-edge top --popup-offset 40\n")
        loop = home / install.UNITS / "audio-manager-loopbacks.service"
        loop.write_text('[Service]\nExecStart=/usr/bin/python3 "%h/.config/waybar/scripts/audio-manager/'
                        'loopback_service.py"\nEnvironment=PYTHONUNBUFFERED=1\n')
        local_bin = home / ".local/bin"
        local_bin.mkdir(parents=True)
        (local_bin / "notifications").write_text(
            '#!/bin/sh\nexec /usr/bin/python3 "$HOME/.config/waybar/scripts/notification-manager/notification_manager.py" "$@"\n')
        for name, entry in (("waymedia", "media-manager/media_manager.py"), ("waytile", "tiling-manager/tiling_manager.py")):
            (local_bin / name).write_text(f"#!/bin/sh\nexec /usr/bin/python3 {home}/{install.SCRIPTS}/{entry} \"$@\"\n")
        (local_bin / "way-displays-gui").write_text(
            f"#!/bin/sh\nexec /usr/bin/python3 {home}/.local/share/way-displays/way-displays-gui \"$@\"\n")
        # The original btmanager.py is a module Global Settings imports (btmanager.App).
        (home / install.SCRIPTS / "btmanager/btmanager.py").write_text(
            "import btpopup\nORIGIN = __file__\n\n\nclass App:\n    embedded = True\n\n\n"
            "if __name__ == '__main__':\n    print('python btmanager')\n")
        (local_bin / "waykeys").write_text(
            f"#!/bin/sh\nexec /usr/bin/python3 {home}/.config/waybar/scripts/on-screen-keyboard/osk_manager.py \"$@\"\n")
        for launcher in local_bin.iterdir():
            launcher.chmod(0o755)
        apps = home / install.APPS
        apps.mkdir(parents=True)
        for desktop, entry in (("org.waybar.PowerManager.desktop", "power-manager/power_manager.py"),
                               ("org.waybar.BatteryManager.desktop", "battery-manager/battery_manager.py")):
            (apps / desktop).write_text(f"[Desktop Entry]\nName=X\nExec=/usr/bin/python3 {home}/{install.SCRIPTS}/{entry} "
                                        "--settings\nIcon=x\n")
        (apps / "org.waydock.Notifications.desktop").write_text("[Desktop Entry]\nExec=notifications --popup\n")

    def env(self, **extra):
        env = dict(os.environ, HOME=str(self.home), PATH=f"{self.bin}:{os.environ['PATH']}",
                   SYSTEMCTL_LOG=str(self.log), TMPDIR=str(SCRATCH))
        for name in ("XDG_DATA_HOME", "KEEP_AWAKE_BINARY", "WAYTILE_BINARY", "TILING_STATUS_BINARY",
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
        for component in install.COMPONENTS:
            self.assertIn(component.key, out)
            for shim in component.shims:
                self.assertIn(str(self.home / scripts(component) / shim.entry), out)
        self.assertIn("ExecStart: /usr/bin/python3 %h/.config/waybar/scripts/keep-awake/keep_awake.py --background"
                      " -> %h/.config/waybar/scripts/keep-awake/keep-awake --background", out)
        self.assertIn('ExecStart: /usr/bin/python3 "%h/.config/waybar/scripts/notification-manager/'
                      'notification_manager.py" --daemon -> %h/.config/waybar/scripts/notification-manager/'
                      'notification-manager --daemon', out)
        self.assertIn('-> %h/.config/waybar/scripts/audio-manager/audio-manager-loopbacks\n', out)
        self.assertIn("shared/install_popup_fixes.py rewrites notification_manager.py", out)
        self.assertIn("still owns an unrelated application", out)
        self.assertIn("Preview only", out)
        self.assertNotIn("TODO-verify", out)
        self.assertEqual(self.systemctl_calls(), [])

    def test_apply_swaps_everything_without_restarting(self):
        before = tree(self.home)
        keeps = {(c.key, k): (self.home / scripts(c) / k).read_bytes() for c in install.COMPONENTS for k in c.keeps}
        result = self.run_installer("--apply", "--i-am-at-the-keyboard")
        self.assertIn("were not restarted", result.stdout)
        home = self.home
        for component in install.COMPONENTS:
            directory = home / scripts(component)
            for cargo, name in (*install.bins(component), *((h, h) for h in component.helpers)):
                self.assertEqual((directory / name).read_text(), (self.prebuilt / cargo).read_text())
                self.assertTrue(os.access(directory / name, os.X_OK))
            for shim in component.shims:
                entry = directory / shim.entry
                text = entry.read_text()
                self.assertIn("desktop-suite-rust shim", text)
                compile(text, str(entry), "exec")
                self.assertTrue(text.startswith("#!"))
                self.assertEqual(entry.stat().st_mode & 0o777, 0o755)
                if shim.porter:
                    self.assertIn((install.REPO / shim.porter).read_text().split("\n", 1)[1], text)
                for flag in shim.flags:     # desktop-suite-action's quoted-flag probe still matches
                    probe = subprocess.run(["sh", "-c", DSA_PROBE, "sh", str(entry), flag])
                    self.assertEqual(probe.returncode, 0, (shim.entry, flag))
                # The shim execs the binary next to it with the same arguments (waydock uses -u).
                ran = subprocess.run(["/usr/bin/python3", "-u", str(entry), "--x", "a b"],
                                     capture_output=True, text=True, env=self.env())
                expected = f"rust-{shim.binary}-v1" + (" --x a b" if forwards_args(shim) else "")
                self.assertEqual(ran.stdout.strip(), expected, shim.entry)
            for unit in component.units:
                self.assertEqual(install.effective_execstart(home, unit.name),
                                 install.unit_command(home, component, unit))
                self.assertTrue((home / install.UNITS / (unit.name + ".d/50-desktop-suite-recovery.conf")).is_file())
        self.assertEqual(keeps, {key: (home / scripts(install.BY_KEY[key[0]]) / key[1]).read_bytes() for key in keeps})
        self.assertIn('"--power-button"', self.entry("power-manager", "power_manager.py").read_text())
        fallback = self.entry("keep-awake", "keep_awake_python.py")
        self.assertIn("python original of keep_awake.py", fallback.read_text())
        notifications = home / install.SCRIPTS / "notification-manager/notification-manager"
        self.assertIn(str(notifications), (home / ".local/bin/notifications").read_text())
        ran = subprocess.run([str(home / ".local/bin/waykeys"), "--settings"], capture_output=True, text=True)
        self.assertEqual(ran.stdout.strip(), "rust-osk-manager-v1 --settings")
        power = (home / install.APPS / "org.waybar.PowerManager.desktop").read_text()
        self.assertIn(f"Exec={home}/{install.SCRIPTS}/power-manager/power-manager --settings\n", power)
        self.assertEqual((home / install.APPS / "org.waydock.Notifications.desktop").read_text(),
                         "[Desktop Entry]\nExec=notifications --popup\n")
        self.assertEqual(json.loads((home / install.LOCALE / "fr.json").read_text()), {"Hello": "fr"})
        for name in ("keep-awake.service", "notifications.service", "notifications.service.d/desktop-suite.conf"):
            self.assertEqual(tree(home)[f"{install.UNITS}/{name}"], before[f"{install.UNITS}/{name}"])
        self.assertEqual(self.systemctl_calls(), ["--user daemon-reload"])
        backups = list((home / install.BACKUPS).glob("rust-swap-*"))
        self.assertEqual(len(backups), 1)
        manifest = json.loads((backups[0] / "manifest.json").read_text())
        self.assertTrue(any(item["kind"] == "shim" and item["before"] == "file" for item in manifest))
        self.assertTrue(any(item["kind"] == "helper" and item["before"] == "file" for item in manifest))
        original = backups[0] / install.SCRIPTS / "keep-awake/keep_awake.py"
        self.assertIn("python original", original.read_text())
        # Same flags as the Python unit when the table does not fix them; TimeoutStopSec stays in the base unit.
        self.assertEqual(install.effective_execstart(home, "weather-manager.service"),
                         "%h/.local/share/weather-manager/weather-manager --tray --popup-edge top --popup-offset 40")
        self.assertIn(f"{home}/{install.SCRIPTS}/media-manager/media-manager",
                      subprocess.run(["sh", "-x", str(home / ".local/bin/waymedia")], capture_output=True, text=True,
                                     env=self.env()).stderr)
        for name in ("tiling_manager_python.py", "tiling_status_python.py"):
            self.assertIn("python original", self.entry("tiling-manager", name).read_text())
        self.assertIn("python original", self.entry("network-discovery", "discovery_manager_python.py").read_text())
        ran = subprocess.run([str(home / ".local/bin/way-displays-gui"), "--x"], capture_output=True, text=True,
                             env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-way-displays-gui-v1 --x")
        self.assertTrue((home / ".local/share/way-displays/way-displays-gui-rs").is_file())
        self.assertIn("'--menu'", self.entry("session-manager", "session_manager.py").read_text())
        self.assertIn("python original", self.entry("language-manager", "language_manager_python.py").read_text())
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
        self.run_installer("--apply", "--only", "keep-awake,audio-manager")
        self.assertIn(install.SHIM_MARKER, self.entry("keep-awake", "keep_awake.py").read_text())
        self.assertIn(install.SHIM_MARKER, self.entry("audio-manager", "loopback_service.py").read_text())
        self.assertNotIn(install.SHIM_MARKER, self.entry("power-manager", "power_manager.py").read_text())
        self.assertFalse(self.entry("power-manager", "power-manager").exists())
        self.run_installer("--rollback", "--only", "keep-awake")
        after = tree(self.home)
        key = f"{install.SCRIPTS}/keep-awake/keep_awake.py"
        self.assertEqual(after[key], before[key])
        self.assertNotIn(f"{install.SCRIPTS}/keep-awake/keep_awake_python.py", after)
        self.assertIn(f"{install.LOCALE}/de.json", after)          # still used by the audio swap
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)

    def test_symlink_is_refused_and_nothing_written(self):
        entry = self.entry("keep-awake", "keep_awake.py")
        real = entry.with_name("real_keep_awake.py")
        entry.rename(real)
        entry.symlink_to(real.name)
        before = tree(self.home, skip=())
        result = self.run_installer("--apply", "--only", "keep-awake", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Refusing to replace a linked installed file", result.stderr)
        self.assertEqual(tree(self.home, skip=()), before)
        self.assertEqual(self.systemctl_calls(), [])

    def test_missing_python_component_blocks(self):
        (self.home / install.UNITS / "battery-manager.service").unlink()
        result = self.run_installer("--apply", "--only", "battery-manager", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("battery-manager.service is not installed", result.stderr)
        self.assertFalse(self.entry("battery-manager", "battery-manager").exists())

    def test_unbuilt_binary_blocks_apply(self):
        (self.prebuilt / "power-status").unlink()
        self.assertIn("not built", self.run_installer("--only", "power-manager", check=False).stdout)
        result = self.run_installer("--apply", "--only", "power-manager", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not built", result.stderr)
        self.assertFalse(self.entry("power-manager", "power-manager").exists())

    def test_restart_only_swapped_active_units(self):
        self.run_installer("--apply", "--only", "keep-awake", "--restart", SYSTEMCTL_ACTIVE="1")
        restarts = [call for call in self.systemctl_calls() if "restart" in call]
        self.assertEqual(restarts, ["--user restart keep-awake.service"])
        self.log.unlink()
        self.run_installer("--rollback", "--only", "keep-awake", "--restart")   # unit inactive: left stopped
        self.assertNotIn("--user restart keep-awake.service", self.systemctl_calls())

    def test_shell_units_need_restart_shell(self):
        component = install.Component("wayshell", "wayshell", "wayshell", ("wayshell",), shims=(),
                                      units=(install.Unit("wayshell.service", "wayshell"),))
        calls = []
        original = install.systemctl
        install.systemctl = lambda *args, check=False: calls.append(args) or subprocess.CompletedProcess(args, 0)
        try:
            install.restart_units([component], self.home, allow_shell=False)
            self.assertEqual(calls, [])
            install.restart_units([component], self.home, allow_shell=True)
            self.assertIn(("restart", "wayshell.service"), calls)
        finally:
            install.systemctl = original

    def test_reapply_upgrade_then_rollback_restores_original(self):
        before = tree(self.home)
        self.run_installer("--apply", "--only", "keep-awake")
        self.make_binaries("v2")
        self.run_installer("--apply", "--only", "keep-awake")
        ran = subprocess.run(["/usr/bin/python3", str(self.entry("keep-awake", "keep_awake.py"))],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-keep-awake-v2")
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)

    def test_overwritten_shim_is_reported_and_kept_on_rollback(self):
        self.run_installer("--apply", "--only", "power-manager")
        entry = self.entry("power-manager", "power_manager.py")
        entry.write_text("# newer python from install_all.py\n")
        report = self.statuses("power-manager")["power-manager"]
        self.assertEqual(report["configured"], "python (shim overwritten)")
        self.assertEqual(report["shims"]["power_manager.py"], "python (shim overwritten)")
        self.assertEqual(report["shims"]["power_status.py"], "rust")
        self.assertTrue(any("install_gtk4.py" in warning for warning in report["warnings"]))
        self.assertIn("python (shim overwritten)", self.run_installer("--status", "--only", "power-manager").stdout)
        result = self.run_installer("--rollback", "--only", "power-manager")
        self.assertIn("KEEP", result.stdout)
        self.assertEqual(entry.read_text(), "# newer python from install_all.py\n")
        self.assertFalse((self.home / install.UNITS / "power-manager.service.d" / install.DROPIN).exists())

    def test_status_reports_mode_and_rss(self):
        entry = self.entry("battery-manager", "battery_status.py")
        python = subprocess.Popen(["/usr/bin/python3", "-u", str(entry)])   # how waydock runs status streams
        self.addCleanup(lambda: (python.kill(), python.wait()))
        time.sleep(0.3)
        report = self.statuses("keep-awake,battery-manager")
        self.assertEqual(report["battery-manager"]["configured"], "python")
        self.assertEqual([p["impl"] for p in report["battery-manager"]["processes"]], ["python"])
        self.assertGreater(report["battery-manager"]["rss_kb"], 0)
        self.assertEqual(report["battery-manager"]["warnings"], [])
        python.kill()
        python.wait()
        self.run_installer("--apply", "--only", "keep-awake")
        binary = self.entry("keep-awake", "keep-awake")
        rust = subprocess.Popen(["sh", "-c", f'exec -a "{binary}" sleep 30'])
        self.addCleanup(lambda: (rust.kill(), rust.wait()))
        time.sleep(0.3)
        report = self.statuses("keep-awake,battery-manager")
        self.assertEqual(report["keep-awake"]["configured"], "rust")
        self.assertEqual([p["impl"] for p in report["keep-awake"]["processes"]], ["rust"])
        self.assertEqual(report["battery-manager"]["configured"], "python")
        self.assertTrue(report["keep-awake"]["units"]["keep-awake.service"]["dropin"])
        table = self.run_installer("--status").stdout
        self.assertIn("keep-awake", table)
        self.assertIn("MB", table)
        self.assertFalse(any("restart" in call for call in self.systemctl_calls()))

    def test_authentication_needs_confirmation(self):
        before = tree(self.home, skip=())
        result = self.run_installer("--apply", "--only", "keep-awake,authentication-manager", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--i-am-at-the-keyboard", result.stderr)
        self.assertEqual(tree(self.home, skip=()), before)
        self.assertIn("needs --i-am-at-the-keyboard", self.run_installer("--only", "authentication-manager").stdout)
        self.run_installer("--apply", "--only", "authentication-manager", "--i-am-at-the-keyboard")
        self.assertEqual(install.effective_execstart(self.home, "polkitman.service"),
                         "%h/.local/share/authentication-manager/authentication-manager")
        self.run_installer("--rollback", "--only", "authentication-manager")      # rollback needs no confirmation
        self.assertEqual(tree(self.home), {k: v for k, v in before.items() if not k.startswith(".local/state")})

    def test_optional_home_assistant_weather_is_skipped_when_absent(self):
        shutil.rmtree(self.home / ".local/share/weather-manager")
        (self.home / install.UNITS / "weather-manager.service").unlink()
        out = self.run_installer("--only", "weather-manager,weather-manager-ha").stdout
        self.assertIn("is not installed (optional)", out)
        self.run_installer("--apply", "--only", "weather-manager,weather-manager-ha")
        self.assertIn(install.SHIM_MARKER, self.entry("weather-manager", "weather_status.py").read_text())
        self.assertFalse((self.home / ".local/share/weather-manager").exists())

    def test_restart_terminates_resident_calendar_only(self):
        binary = self.entry("calendar-manager", "calendar-manager")
        self.run_installer("--apply", "--only", "calendar-manager")
        calendar = subprocess.Popen(["sh", "-c", f'exec -a "{binary}" sleep 30'])
        other = subprocess.Popen(["sleep", "30"])
        self.addCleanup(lambda: [(p.kill(), p.wait()) for p in (calendar, other)])
        time.sleep(0.3)
        result = self.run_installer("--apply", "--only", "calendar-manager", "--restart")
        self.assertIn(f"kill -TERM {calendar.pid}", result.stdout)
        self.assertEqual(calendar.wait(timeout=5), -15)
        self.assertIsNone(other.poll())
        self.assertFalse(any("restart" in call for call in self.systemctl_calls()))

    def test_btmanager_shim_still_imports_the_python_module(self):
        self.run_installer("--apply", "--only", "bluetooth-manager")
        directory = self.entry("bluetooth-manager", "btmanager.py").parent
        self.assertIn("class App", (directory / "btmanager_gtk.py").read_text())
        # Global Settings' applet_pages.py: sys.path gets the scripts dir, then `import btmanager`.
        probe = ("import sys; sys.path.insert(0, sys.argv[1]); import btmanager; "
                 "print(btmanager.App.embedded, btmanager.ORIGIN.endswith('btmanager_gtk.py'))")
        ran = subprocess.run([sys.executable, "-c", probe, str(directory)], capture_output=True, text=True,
                             env=self.env())
        self.assertEqual(ran.stdout.strip(), "True True", ran.stderr)
        ran = subprocess.run(["/usr/bin/python3", str(directory / "btmanager.py"), "--popup"],
                             capture_output=True, text=True, env=self.env())
        self.assertEqual(ran.stdout.strip(), "rust-btmanager-v1 --popup")
        self.assertEqual(self.statuses("bluetooth-manager")["bluetooth-manager"]["configured"], "rust")
        self.assertEqual(self.statuses("way-displays")["way-displays"]["configured"], "python")

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
        self.assertIn("--power-button", install.BY_KEY["power-manager"].shims[0].flags)


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
                if isinstance(launcher, tuple):
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
