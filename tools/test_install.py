#!/usr/bin/env python3
"""Tests for tools/install.py against a fake HOME (never the real one).

Run: TMPDIR=/run/user/1000/cbt/rust-installer python3 -m unittest tools/test_install.py
"""
import hashlib
import json
import os
from pathlib import Path
import pwd
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
            for name in component.binaries:
                path = self.prebuilt / name
                path.write_text(f'#!/bin/sh\necho "rust-{name}-{version}" "$@"\n')
                path.chmod(0o755)

    def populate(self):
        home = self.home
        for component in install.COMPONENTS:
            for shim in component.shims:
                entry = home / shim.entry
                entry.parent.mkdir(parents=True, exist_ok=True)
                entry.write_text(PY_ENTRY.format(name=shim.entry, flags=shim.flags))
                entry.chmod(0o755)
            for keep in component.keeps:
                (home / keep).write_text(f"# module {keep}\n")
            for unit in component.units:
                units = home / install.UNITS
                (units / (unit.name + ".d")).mkdir(parents=True, exist_ok=True)
                (units / unit.name).write_text(UNIT.format(name=unit.name, entry=component.shims[0].entry,
                                                          args=" ".join(unit.args)))
                (units / (unit.name + ".d") / "50-desktop-suite-recovery.conf").write_text(RECOVERY)
        notif = home / install.UNITS / "notifications.service"
        notif.write_text("[Service]\nExecStart=dunst\n[Install]\nWantedBy=graphical-session.target\n")
        (home / install.UNITS / "notifications.service.d/desktop-suite.conf").write_text(
            '[Service]\nExecStart=\nExecStart=/usr/bin/python3 "%h/.config/waybar/scripts/notification-manager/'
            'notification_manager.py" --daemon\n')
        local_bin = home / ".local/bin"
        local_bin.mkdir(parents=True)
        (local_bin / "notifications").write_text(
            '#!/bin/sh\nexec /usr/bin/python3 "$HOME/.config/waybar/scripts/notification-manager/notification_manager.py" "$@"\n')
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
        env.pop("XDG_DATA_HOME", None)
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

    # ---- tests ---------------------------------------------------------------------------

    def test_check_is_default_and_changes_nothing(self):
        before = tree(self.home, skip=())
        result = self.run_installer()
        self.assertEqual(tree(self.home, skip=()), before)
        out = result.stdout
        for component in install.COMPONENTS:
            self.assertIn(component.key, out)
            for shim in component.shims:
                self.assertIn(str(self.home / shim.entry), out)
        self.assertIn("ExecStart: /usr/bin/python3 %h/.config/waybar/scripts/keep-awake/keep_awake.py --background"
                      " -> %h/.local/libexec/desktop-suite-rust/keep-awake --background", out)
        self.assertIn('ExecStart: /usr/bin/python3 "%h/.config/waybar/scripts/notification-manager/'
                      'notification_manager.py" --daemon -> %h/.local/libexec/desktop-suite-rust/notification-manager'
                      ' --daemon', out)
        self.assertIn("TODO-verify", out)
        self.assertIn("Preview only", out)
        self.assertNotIn("restart", " ".join(self.systemctl_calls()))
        self.assertEqual(self.systemctl_calls(), [])

    def test_apply_swaps_everything_without_restarting(self):
        before = tree(self.home)
        keeps = {k: (self.home / k).read_bytes() for c in install.COMPONENTS for k in c.keeps}
        result = self.run_installer("--apply")
        self.assertIn("were not restarted", result.stdout)
        home = self.home
        libexec = home / install.LIBEXEC
        for component in install.COMPONENTS:
            for name in component.binaries:
                self.assertTrue(os.access(libexec / name, os.X_OK))
            for shim in component.shims:
                entry = home / shim.entry
                text = entry.read_text()
                self.assertIn(install.SHIM_MARKER, text)
                compile(text, str(entry), "exec")
                self.assertEqual(entry.stat().st_mode & 0o777, 0o755)
                for flag in shim.flags:     # desktop-suite-action's quoted-flag probe still matches
                    probe = subprocess.run(["sh", "-c", DSA_PROBE, "sh", str(entry), flag])
                    self.assertEqual(probe.returncode, 0, (shim.entry, flag))
                # The shim execs the binary with the same argv.
                ran = subprocess.run(["/usr/bin/python3", str(entry), "--x", "a b"], capture_output=True, text=True)
                self.assertEqual(ran.stdout.strip(), f"rust-{shim.binary}-v1 --x a b")
            for unit in component.units:
                self.assertEqual(install.effective_execstart(home, unit.name), install.unit_command(home, unit))
                self.assertTrue((home / install.UNITS / (unit.name + ".d/50-desktop-suite-recovery.conf")).is_file())
        self.assertEqual(keeps, {k: (home / k).read_bytes() for k in keeps})
        self.assertIn("'--power-button'", (home / install.SCRIPTS / "power-manager/power_manager.py").read_text())
        self.assertIn(str(libexec / "notification-manager"), (home / ".local/bin/notifications").read_text())
        self.assertIn(str(libexec / "on-screen-keyboard"), (home / ".local/bin/waykeys").read_text())
        ran = subprocess.run([str(home / ".local/bin/waykeys"), "--settings"], capture_output=True, text=True)
        self.assertEqual(ran.stdout.strip(), "rust-on-screen-keyboard-v1 --settings")
        power = (home / install.APPS / "org.waybar.PowerManager.desktop").read_text()
        self.assertIn(f"Exec={libexec / 'power-manager'} --settings\n", power)
        self.assertEqual((home / install.APPS / "org.waydock.Notifications.desktop").read_text(),
                         "[Desktop Entry]\nExec=notifications --popup\n")
        self.assertEqual(json.loads((home / install.LOCALE / "fr.json").read_text()), {"Hello": "fr"})
        # Base units and the existing drop-ins are untouched.
        for name in ("keep-awake.service", "notifications.service", "notifications.service.d/desktop-suite.conf"):
            self.assertEqual(tree(home)[f"{install.UNITS}/{name}"], before[f"{install.UNITS}/{name}"])
        calls = self.systemctl_calls()
        self.assertEqual(calls, ["--user daemon-reload"])
        # Backups: manifest + the original entry scripts.
        backups = list((home / install.BACKUPS).glob("rust-swap-*"))
        self.assertEqual(len(backups), 1)
        manifest = json.loads((backups[0] / "manifest.json").read_text())
        self.assertTrue(any(item["kind"] == "shim" and item["before"] == "file" for item in manifest))
        original = backups[0] / install.SCRIPTS / "keep-awake/keep_awake.py"
        self.assertIn("python original", original.read_text())
        # A second apply has nothing to do.
        self.assertIn("nothing to change", self.run_installer("--apply").stdout)

    def test_rollback_restores_exactly(self):
        before = tree(self.home)
        self.run_installer("--apply")
        self.assertNotEqual(tree(self.home), before)
        preview = self.run_installer("--rollback", "--check")
        self.assertIn("restore", preview.stdout)
        self.assertNotEqual(tree(self.home), before)
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)
        self.assertEqual(json.loads((self.home / install.STATE).read_text())["components"], {})
        self.assertNotIn("restart", " ".join(self.systemctl_calls()))

    def test_only_and_partial_rollback(self):
        before = tree(self.home)
        self.run_installer("--apply", "--only", "keep-awake,audio-manager-loopbacks")
        self.assertIn(install.SHIM_MARKER, (self.home / install.SCRIPTS / "keep-awake/keep_awake.py").read_text())
        self.assertIn(install.SHIM_MARKER, (self.home / install.SCRIPTS / "audio-manager/loopback_service.py").read_text())
        self.assertNotIn(install.SHIM_MARKER, (self.home / install.SCRIPTS / "audio-manager/audio_manager.py").read_text())
        self.assertNotIn(install.SHIM_MARKER, (self.home / install.SCRIPTS / "power-manager/power_manager.py").read_text())
        self.assertFalse((self.home / install.LIBEXEC / "power-manager").exists())
        self.run_installer("--rollback", "--only", "keep-awake")
        after = tree(self.home)
        self.assertEqual(after[f"{install.SCRIPTS}/keep-awake/keep_awake.py"],
                         before[f"{install.SCRIPTS}/keep-awake/keep_awake.py"])
        self.assertIn(f"{install.LOCALE}/de.json", after)          # still used by the loopbacks swap
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)

    def test_symlink_is_refused_and_nothing_written(self):
        entry = self.home / install.SCRIPTS / "keep-awake/keep_awake.py"
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
        self.assertFalse((self.home / install.LIBEXEC).exists())

    def test_unbuilt_binary_blocks_apply(self):
        (self.prebuilt / "power-manager").unlink()
        self.assertIn("not built", self.run_installer("--only", "power-manager", check=False).stdout)
        result = self.run_installer("--apply", "--only", "power-manager", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not built", result.stderr)

    def test_restart_only_swapped_active_units(self):
        self.run_installer("--apply", "--only", "keep-awake,audio-manager", "--restart", SYSTEMCTL_ACTIVE="1")
        restarts = [call for call in self.systemctl_calls() if call.startswith("--user restart")]
        self.assertEqual(restarts, ["--user restart keep-awake.service"])
        self.log.unlink()
        self.run_installer("--rollback", "--only", "keep-awake", "--restart")   # unit inactive: left stopped
        self.assertNotIn("--user restart keep-awake.service", self.systemctl_calls())

    def test_shell_units_need_restart_shell(self):
        component = install.Component("wayshell", "wayshell", ("wayshell",), shims=(),
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
        ran = subprocess.run(["/usr/bin/python3", str(self.home / install.SCRIPTS / "keep-awake/keep_awake.py")],
                             capture_output=True, text=True)
        self.assertEqual(ran.stdout.strip(), "rust-keep-awake-v2")
        self.run_installer("--rollback")
        self.assertEqual(tree(self.home), before)

    def test_rollback_keeps_files_changed_since_swap(self):
        self.run_installer("--apply", "--only", "power-manager")
        entry = self.home / install.SCRIPTS / "power-manager/power_manager.py"
        entry.write_text("# newer python from install_all.py\n")
        result = self.run_installer("--rollback", "--only", "power-manager")
        self.assertIn("KEEP", result.stdout)
        self.assertEqual(entry.read_text(), "# newer python from install_all.py\n")
        self.assertFalse((self.home / install.UNITS / "power-manager.service.d" / install.DROPIN).exists())

    def test_status_reports_mode_and_rss(self):
        entry = self.home / install.SCRIPTS / "keep-awake/keep_awake.py"
        python = subprocess.Popen(["/usr/bin/python3", str(entry)])
        self.addCleanup(lambda: (python.kill(), python.wait()))
        time.sleep(0.3)
        report = {item["component"]: item for item in json.loads(
            self.run_installer("--status", "--json", "--only", "keep-awake,battery-manager").stdout)}
        self.assertEqual(report["keep-awake"]["configured"], "python")
        self.assertEqual([p["impl"] for p in report["keep-awake"]["processes"]], ["python"])
        self.assertGreater(report["keep-awake"]["rss_kb"], 0)
        python.kill()
        python.wait()
        self.run_installer("--apply", "--only", "keep-awake")
        binary = self.home / install.LIBEXEC / "keep-awake"
        rust = subprocess.Popen(["sh", "-c", f'exec -a "{binary}" sleep 30'])
        self.addCleanup(lambda: (rust.kill(), rust.wait()))
        time.sleep(0.3)
        report = {item["component"]: item for item in json.loads(
            self.run_installer("--status", "--json", "--only", "keep-awake,battery-manager").stdout)}
        self.assertEqual(report["keep-awake"]["configured"], "rust")
        self.assertEqual([p["impl"] for p in report["keep-awake"]["processes"]], ["rust"])
        self.assertEqual(report["battery-manager"]["configured"], "python")
        self.assertTrue(report["keep-awake"]["units"]["keep-awake.service"]["dropin"])
        table = self.run_installer("--status").stdout
        self.assertIn("keep-awake", table)
        self.assertIn("MB", table)
        self.assertFalse(any("restart" in call for call in self.systemctl_calls()))

    def test_table_is_consistent(self):
        keys = [c.key for c in install.COMPONENTS]
        self.assertEqual(len(keys), len(set(keys)))
        for component in install.COMPONENTS:
            names = set(component.binaries)
            self.assertTrue(component.shims)
            self.assertTrue({s.binary for s in component.shims} | {u.binary for u in component.units} <= names)
            for shim in component.shims:
                self.assertTrue(shim.entry.endswith(".py"))
                self.assertTrue(all(flag.startswith("--") for flag in shim.flags))
            self.assertNotIn(component.shims[0].entry, component.keeps)
        power = install.BY_KEY["power-manager"].shims[0]
        self.assertIn("--power-button", power.flags)


if __name__ == "__main__":
    unittest.main()
