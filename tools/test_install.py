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
