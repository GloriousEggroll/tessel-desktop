#!/usr/bin/python3 -I
"""Tests of tools/install.py against a fake home, a fake root and fake build output.

    python3 -I tools/test_install.py
"""
import contextlib
import io
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import install  # noqa: E402


def fake_build(target, names, log):
    release = target / "release"
    release.mkdir(parents=True)
    for name in names:
        script = release / name
        script.write_text(f"#!/bin/sh\necho \"{name} $* HOME=$HOME\" >> '{log}'\n")
        script.chmod(0o755)
    (release / "libpam_tessel_keyring.so").write_bytes(b"\x7fELF fake")


def run(argv):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        install.main(argv)
    return out.getvalue()


class UserInstall(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="tessel-install-"))
        self.addCleanup(shutil.rmtree, self.tmp)
        self.home = self.tmp / "home"
        self.target = self.tmp / "target"
        self.log = self.tmp / "commands.log"
        self.names = sorted(install.workspace_binaries())
        fake_build(self.target, self.names, self.log)
        old = self.home / ".local/share/applications"
        old.mkdir(parents=True)
        (old / "org.waydock.Files.desktop").write_text("[Desktop Entry]\nName=Old Files\n")
        (old / "helium.desktop").write_text("[Desktop Entry]\nName=Helium\n")
        self.args = ["--home", str(self.home), "--target-dir", str(self.target), "--no-build"]

    def test_install_is_complete_idempotent_and_rolls_back(self):
        preview = run(self.args)
        self.assertIn("retire  " + str(self.home / ".local/share/applications/org.waydock.Files.desktop"), preview)
        self.assertFalse((self.home / ".local/lib").exists(), "--check writes nothing")

        run(self.args + ["--apply"])
        lib = self.home / ".local/lib/tessel"
        for name in ("tessel-dock", "tessel-shell", "tessel-migrate", "tessel-night-light", "tessel-action",
                     "tessel-shell-lock"):
            self.assertTrue(os.access(lib / name, os.X_OK), name)
            self.assertEqual(os.readlink(self.home / ".local/bin" / name), f"../lib/tessel/{name}")
        self.assertNotIn("luna-clock", os.listdir(lib), "Kasumi is not installed")
        units = self.home / ".config/systemd/user"
        self.assertIn("ExecStart=%h/.local/lib/tessel/tessel-dock\n", (units / "tessel-dock.service").read_text())
        activation = (self.home / ".local/share/dbus-1/services/org.freedesktop.secrets.service").read_text()
        self.assertIn("SystemdService=tessel-keyring.service", activation)
        self.assertIn("BusName=org.freedesktop.secrets", (units / "tessel-keyring.service").read_text())
        files = (self.home / ".local/share/applications/org.tessel.Files.desktop").read_text()
        self.assertIn(f"Exec={lib}/tessel-files %U", files)
        settings = (self.home / ".local/share/applications/org.tessel.GlobalSettings.desktop").read_text()
        self.assertIn(f"Exec={lib}/tessel-settings", settings)
        self.assertNotIn("@LIBDIR@", settings)
        self.assertFalse((self.home / ".local/share/applications/org.waydock.Files.desktop").exists())
        self.assertTrue((self.home / ".local/share/applications/helium.desktop").exists())
        self.assertTrue((self.home / ".local/share/tessel/locale/de.json").is_file())
        self.assertTrue((self.home / ".local/share/icons/hicolor/scalable/apps/tessel.svg").is_file())
        calls = self.log.read_text().splitlines()
        self.assertEqual(calls, [f"tessel-configure-session --apply HOME={self.home}",
                                 f"tessel-xdg-autostart --apply HOME={self.home}"])

        again = run(self.args)
        self.assertIn("0 files to change.", again)

        run(["--home", str(self.home), "--rollback"])
        self.assertFalse((lib / "tessel-dock").exists())
        self.assertFalse((self.home / ".local/bin/tessel-dock").is_symlink())
        self.assertEqual((self.home / ".local/share/applications/org.waydock.Files.desktop").read_text(),
                         "[Desktop Entry]\nName=Old Files\n")
        self.assertFalse((self.home / ".local/share/applications/org.tessel.Files.desktop").exists())

    def test_old_portal_override_waits_for_the_patched_package(self):
        dropins = self.home / install.PORTAL_DROPIN
        dropins.mkdir(parents=True)
        (dropins / "50-waycapture.conf").write_text("[Service]\nExecStart=\nExecStart=/usr/local/libexec/waycapture-portal-wlr\n")
        (dropins / "60-mine.conf").write_text("[Service]\nEnvironment=XDPW_LOG=1\n")
        original = install.packaged_portal_ready
        self.addCleanup(setattr, install, "packaged_portal_ready", original)
        install.packaged_portal_ready = lambda root: False
        self.assertIn("Kept the waycapture-portal-wlr override", run(self.args + ["--apply", "--only", "retire-old"]))
        self.assertTrue((dropins / "50-waycapture.conf").exists())
        install.packaged_portal_ready = lambda root: True
        run(self.args + ["--apply", "--only", "retire-old"])
        self.assertFalse((dropins / "50-waycapture.conf").exists())
        self.assertTrue((dropins / "60-mine.conf").exists())
        self.assertFalse(self.log.exists(), "no commands for a home other than the caller's")

    def test_only_selects_parts(self):
        run(self.args + ["--apply", "--only", "locale"])
        self.assertTrue((self.home / ".local/share/tessel/locale/en.json").is_file())
        self.assertFalse((self.home / ".local/lib/tessel").exists())
        self.assertFalse(self.log.exists())

    def test_unknown_part_is_refused(self):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            install.main(self.args + ["--only", "nope"])


class SystemInstall(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="tessel-system-"))
        self.addCleanup(shutil.rmtree, self.tmp)
        self.root = self.tmp / "root"
        self.target = self.tmp / "target"
        fake_build(self.target, ["tessel-keyring", "tessel-keyring-prompt"], self.tmp / "log")
        pam = self.root / "etc/pam.d"
        pam.mkdir(parents=True)
        self.waylogin = ("auth substack waylogin-auth\n-auth optional pam_waykeyring.so\n"
                         "session include system-auth\n-session optional pam_waykeyring.so\n")
        (pam / "waylogin").write_text(self.waylogin)
        (pam / "passwd").write_text("password   substack     system-auth\n")
        apps = self.root / "usr/share/applications"
        apps.mkdir(parents=True)
        (apps / "org.waydock.Files.desktop").write_text("old")
        (apps / "firefox.desktop").write_text("keep")
        login = self.root / "etc/waylogin"
        login.mkdir(parents=True)
        (login / "appearance.json").write_text('{\n  "picture": "x",\n  "default_session": "labwc"\n}\n')
        self.args = ["--system", "--root", str(self.root), "--target-dir", str(self.target)]

    def test_keyring_pam_session_and_old_files(self):
        run(self.args + ["--apply"])
        self.assertTrue(os.access(self.root / "usr/local/lib/tessel/tessel-keyring", os.X_OK))
        self.assertTrue((self.root / "usr/lib64/security/pam_tessel_keyring.so").is_file())
        self.assertEqual((self.root / "etc/pam.d/waylogin").read_text(), self.waylogin.replace("pam_waykeyring", "pam_tessel_keyring"))
        self.assertEqual((self.root / "etc/pam.d/passwd").read_text(),
                         "password   substack     system-auth\n-password   optional\tpam_tessel_keyring.so\n")
        self.assertIn("Name=Tessel", (self.root / "usr/local/share/wayland-sessions/tessel.desktop").read_text())
        self.assertFalse((self.root / "usr/share/applications/org.waydock.Files.desktop").exists())
        self.assertTrue((self.root / "usr/share/applications/firefox.desktop").exists())
        self.assertEqual((self.root / "etc/waylogin/appearance.json").read_text(),
                         '{\n  "picture": "x",\n  "default_session": "tessel"\n}\n')
        self.assertIn("0 files to change.", run(self.args))

        run(["--system", "--root", str(self.root), "--rollback"])
        self.assertEqual((self.root / "etc/pam.d/waylogin").read_text(), self.waylogin)
        self.assertEqual((self.root / "usr/share/applications/org.waydock.Files.desktop").read_text(), "old")
        self.assertFalse((self.root / "usr/lib64/security/pam_tessel_keyring.so").exists())
        self.assertIn('"labwc"', (self.root / "etc/waylogin/appearance.json").read_text())

    def test_old_portal_binary_is_removed_only_when_unused_and_replaced(self):
        old = self.root / install.OLD_PORTAL
        old.parent.mkdir(parents=True)
        old.write_bytes(b"old portal")
        packaged = self.root / install.PACKAGED_PORTAL
        packaged.parent.mkdir(parents=True)
        packaged.write_bytes(b"stock 0.8.4-2")
        user = self.root / "home/alice" / install.PORTAL_DROPIN
        user.mkdir(parents=True)
        (user / "50-waycapture.conf").write_text("ExecStart=/usr/local/libexec/waycapture-portal-wlr\n")
        only = self.args + ["--apply", "--only", "retire-old"]
        self.assertIn("lacks the chooser-session patch", run(only))
        packaged.write_bytes(b"XDPW_CHOOSER_SESSION XDPW_CHOOSER_REMOTE_DESKTOP")
        self.assertIn("still used by", run(only))
        self.assertTrue(old.exists())
        (user / "50-waycapture.conf").unlink()
        run(only)
        self.assertFalse(old.exists())
        run(["--system", "--root", str(self.root), "--rollback"])
        self.assertEqual(old.read_bytes(), b"old portal")

    def test_lock_pam_services_are_copied_once(self):
        pam = self.root / "etc/pam.d"
        (pam / "waylock").write_text("auth include waylogin-auth\n")
        (pam / "tessel-lock-password").write_text("mine\n")
        run(self.args + ["--apply", "--only", "lock-pam"])
        self.assertEqual((pam / "tessel-lock").read_text(), "auth include waylogin-auth\n")
        self.assertEqual((pam / "tessel-lock-password").read_text(), "mine\n")
        self.assertFalse((pam / "tessel-lock-fingerprint").exists())

    def test_login_screen_is_only_installed_when_named(self):
        login = self.root / "etc/waylogin"
        (login / "labwc").mkdir()
        (login / "labwc/rc.xml").write_text("<labwc_config/>\n")
        (login / "greetd.toml").write_text('[general]\nservice = "waylogin"\nruntime = 1\nrunfile = "/run/waylogin.run"\n'
                                           '[default_session]\ncommand = "/usr/local/lib/waylogin/start-greeter"\n')
        unit = self.root / "etc/systemd/system/waylogin.service"
        unit.parent.mkdir(parents=True)
        unit.write_text("[Unit]\nDescription=Waylogin Python Wayland Login Manager\n[Service]\n"
                        "ExecStart=/usr/bin/greetd --config /etc/waylogin/greetd.toml\n")
        fake_build(self.tmp / "t2", list(install.LOGIN_PROGRAMS) + ["tessel-keyring", "tessel-keyring-prompt"], self.tmp / "log")
        args = ["--system", "--root", str(self.root), "--target-dir", str(self.tmp / "t2")]
        run(args + ["--apply"])
        self.assertFalse((self.root / "etc/tessel/login").exists(), "not part of the default system install")
        run(args + ["--apply", "--only", "login"])
        new = self.root / "etc/tessel/login"
        self.assertIn('"default_session": "tessel"', (new / "appearance.json").read_text())
        self.assertEqual((new / "labwc/rc.xml").read_text(), "<labwc_config/>\n")
        greetd = (new / "greetd.toml").read_text()
        self.assertIn('command = "/usr/local/lib/tessel/tessel-start-greeter"', greetd)
        self.assertIn('service = "waylogin"', greetd)
        self.assertIn("--config /etc/tessel/login/greetd.toml", unit.read_text())
        for name in install.LOGIN_PROGRAMS + ("tessel-start-greeter", "tessel-start-session"):
            self.assertTrue(os.access(self.root / "usr/local/lib/tessel" / name, os.X_OK), name)
        run(["--system", "--root", str(self.root), "--rollback"])
        self.assertIn("--config /etc/waylogin/greetd.toml", unit.read_text())
        self.assertFalse((new / "greetd.toml").exists())

    def test_pam_lines_are_added_once(self):
        fresh = "auth include waylogin-auth\nsession include system-auth\n"
        once = install.pam_text(fresh, install.PAM_EDITS["etc/pam.d/waylogin"])
        self.assertEqual(once, "auth substack waylogin-auth\n-auth optional pam_tessel_keyring.so\n"
                               "session include system-auth\n-session optional pam_tessel_keyring.so\n")
        self.assertEqual(install.pam_text(once, install.PAM_EDITS["etc/pam.d/waylogin"]), once)


class DesktopFiles(unittest.TestCase):
    def test_exec_lines_become_absolute(self):
        text = "Exec=tessel-files %U\nTryExec=tessel-files\nExec=@LIBDIR@/tessel-dock --settings\nExec=true\n"
        self.assertEqual(install.desktop_text(text, Path("/L")),
                         "Exec=/L/tessel-files %U\nTryExec=/L/tessel-files\nExec=/L/tessel-dock --settings\nExec=true\n")


if __name__ == "__main__":
    unittest.main()
