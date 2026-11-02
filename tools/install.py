#!/usr/bin/python3 -I
"""Install Tessel (docs/LAYOUT.md).

  install.py [--check]                 preview what would change (default)
  install.py --apply                   build and install for this user
  install.py --rollback                undo the last --apply
  pkexec /usr/bin/python3 -I install.py --system --apply [--only keyring,session-entry,retire-old]
  pkexec /usr/bin/python3 -I install.py --system --rollback

User scope (no root): programs into ~/.local/lib/tessel with ~/.local/bin links, units,
desktop files, translations, the logo, the Secret Service activation file, the labwc
session files (tessel-configure-session, tessel-xdg-autostart), and the old desktop-suite
desktop files moved out of the menu. Nothing is started, stopped, enabled or restarted:
the new session starts at the next login, where tessel-migrate moves old settings over.

System scope (--system, as root through pkexec): the keyring helper and PAM module, the
PAM lines that unlock saved passwords with the login password, the "Tessel" login session
entry, and the old desktop-suite desktop files in /usr/share/applications moved aside.
Build as your user first; --system never runs cargo.

Every --apply records what it changed under <state>/install/<timestamp>/ (with copies of
replaced and removed files); --rollback restores the newest record exactly.
"""
import argparse
import datetime
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SKIP_PACKAGES = {"home-assistant-screen"}          # Kasumi: its own device and installer
APP_DIRS = ("data/applications",)                  # plus components/*/data/*.desktop

USER_PARTS = ("programs", "locale", "icons", "units", "applications", "dbus", "session", "retire-old")
SYSTEM_PARTS = ("keyring", "lock-pam", "session-entry", "retire-old")
EXPLICIT_SYSTEM_PARTS = ("login",)    # only with --only login (the login screen; takes effect at the next boot)
# Native helpers built by build.rs into the build output (not [[bin]] targets).
HELPERS = ("tessel-keyboard-input", "libcapture-pipewire.so")
# Native pieces the build may skip (missing -devel packages): an installed copy is kept.
# The suite's own build of xdg-desktop-portal-wlr, replaced by the distribution package
# once that carries the chooser-session patch (Nobara 0.8.4-4).
OLD_PORTAL = "usr/local/libexec/waycapture-portal-wlr"
PORTAL_DROPIN = ".config/systemd/user/xdg-desktop-portal-wlr.service.d"
PACKAGED_PORTAL = "usr/libexec/xdg-desktop-portal-wlr"


def packaged_portal_ready(root):
    """The distribution portal has every patch the suite's copy had."""
    try:
        data = (Path(root) / PACKAGED_PORTAL).read_bytes()
    except OSError:
        return False
    return all(marker in data for marker in (b"XDPW_CHOOSER_SESSION", b"XDPW_CHOOSER_REMOTE_DESKTOP"))


def old_portal_dropins(home):
    directory = Path(home) / PORTAL_DROPIN
    if not directory.is_dir():
        return []
    return [path for path in sorted(directory.iterdir())
            if path.suffix == ".conf" and "waycapture-portal-wlr" in path.read_text(errors="replace")]


CARRY_OVER = {"libcapture-pipewire.so": ("usr/local/lib/desktop-suite/capture-manager/libcapture-pipewire.so",)}
# The lock screen's PAM services: (desktop-suite name, Tessel name).
LOCK_PAM = (("waylock", "tessel-lock"), ("waylock-password", "tessel-lock-password"),
            ("waylock-fingerprint", "tessel-lock-fingerprint"))
LOGIN_PROGRAMS = ("tessel-login", "tessel-session-transition", "tessel-login-helper", "tessel-keyboard-input")

# Desktop files of the desktop suite (before Tessel); their Tessel versions replace them.
OLD_USER_DESKTOP = ("org.waydock.", "org.waybar.", "org.wayshell.", "waydock.desktop", "way-displays-gui.desktop",
                    "org.desktop.HDRCalibration.desktop")
OLD_SYSTEM_DESKTOP = ("usr/share/applications/way-displays-gui.desktop", "usr/share/applications/org.waydock.",
                      "usr/local/share/applications/org.waydock.")

# PAM lines that unlock saved passwords with the login password.
PAM_MODULE = "pam_tessel_keyring.so"
PAM_EDITS = {
    "etc/pam.d/waylogin": [
        ("auth include waylogin-auth", "auth substack waylogin-auth\n-auth optional pam_tessel_keyring.so"),
        ("session include system-auth", "session include system-auth\n-session optional pam_tessel_keyring.so"),
    ],
    "etc/pam.d/passwd": [
        ("password   substack     system-auth",
         "password   substack     system-auth\n-password   optional\tpam_tessel_keyring.so"),
    ],
}


class Plan:
    """Changes to make: (target, data bytes | None for removal | Path symlink target, mode)."""

    def __init__(self, base: Path):
        self.base = base
        self.writes = []      # (target, bytes, mode)
        self.links = []       # (target, link text)
        self.removals = []    # target
        self.commands = []    # (argv, env)
        self.notes = []

    def write(self, target, data, mode=0o644):
        if target.is_file() and not target.is_symlink():
            try:
                if target.read_bytes() == data and (target.stat().st_mode & 0o7777) == mode:
                    return
            except OSError:
                pass
        self.writes.append((target, data, mode))

    def link(self, target, to):
        if target.is_symlink() and os.readlink(target) == to:
            return
        self.links.append((target, to))

    def remove(self, target):
        if target.exists() or target.is_symlink():
            self.removals.append(target)

    def empty(self):
        return not (self.writes or self.links or self.removals or self.commands)


# ---------------------------------------------------------------- build ---

def workspace_binaries():
    """{binary name: package} for every [[bin]] of the workspace (Kasumi excluded)."""
    out = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=REPO,
                         capture_output=True, text=True, check=True).stdout
    binaries = {}
    for package in json.loads(out)["packages"]:
        if package["name"] in SKIP_PACKAGES:
            continue
        for target in package["targets"]:
            if "bin" in target["kind"]:
                binaries[target["name"]] = package["name"]
    return binaries


def build(target_dir):
    command = ["cargo", "build", "--release", "--workspace"] + [f"--exclude={p}" for p in sorted(SKIP_PACKAGES)]
    print("$ " + " ".join(command), flush=True)
    subprocess.run(command, cwd=REPO, check=True, env={**os.environ, "CARGO_TARGET_DIR": str(target_dir)})


def built_files(target_dir, names):
    """The built release binaries, and native helpers from the newest build-script output."""
    release = Path(target_dir) / "release"
    found = {name: release / name for name in names if (release / name).is_file()}
    for name in set(names) - set(found):
        if name in HELPERS:
            candidates = sorted(release.glob(f"build/*/out/{name}"), key=lambda p: p.stat().st_mtime)
            if candidates:
                found[name] = candidates[-1]
    missing = sorted(set(names) - set(found))
    return found, missing


# ---------------------------------------------------------- user scope ---

def desktop_sources():
    files = []
    for directory in APP_DIRS:
        files += sorted((REPO / directory).glob("*.desktop"))
    files += sorted(REPO.glob("components/*/data/*.desktop"))
    names = {}
    for path in files:
        names.setdefault(path.name, path)   # data/ first; a component's copy fills gaps only
    return list(names.values())


def desktop_text(text, libdir):
    """@LIBDIR@ and bare `tessel-*` Exec/TryExec programs become absolute paths."""
    lines = []
    for line in text.splitlines(keepends=True):
        line = line.replace("@LIBDIR@", str(libdir))
        for key in ("Exec=", "TryExec="):
            if line.startswith(key + "tessel-"):
                line = f"{key}{libdir}/{line[len(key):]}"
        lines.append(line)
    return "".join(lines)


def user_plan(home, parts, binaries, target_dir):
    plan = Plan(home)
    libdir = home / ".local/lib/tessel"
    bindir = home / ".local/bin"
    if "programs" in parts:
        found, missing = built_files(target_dir, list(binaries) + list(HELPERS))
        for name in [m for m in missing if m in CARRY_OVER]:
            installed = [Path("/") / old for old in CARRY_OVER[name]] + [libdir / name]
            source = next((path for path in installed if path.is_file()), None)
            if source:
                found[name] = source
                missing.remove(name)
                plan.notes.append(f"{name} was not built (install pipewire-devel to build it); keeping {source}.")
        if missing:
            plan.notes.append("Not built (run without --no-build): " + ", ".join(missing))
        for name, path in sorted(found.items()):
            plan.write(libdir / name, path.read_bytes(), 0o755)
            plan.link(bindir / name, f"../lib/tessel/{name}")
        for script in sorted((REPO / "data/bin").iterdir()):
            plan.write(libdir / script.name, script.read_bytes(), 0o755)
            plan.link(bindir / script.name, f"../lib/tessel/{script.name}")
    if "locale" in parts:
        for catalog in sorted((REPO / "i18n/locale").glob("*.json")):
            plan.write(home / ".local/share/tessel/locale" / catalog.name, catalog.read_bytes())
    if "icons" in parts:
        icons = home / ".local/share/icons/hicolor"
        plan.write(icons / "scalable/apps/tessel.svg", (REPO / "branding/tessel-logo.svg").read_bytes())
        plan.write(icons / "symbolic/apps/tessel-symbolic.svg", (REPO / "branding/tessel-symbolic.svg").read_bytes())
    if "units" in parts:
        for unit in sorted((REPO / "data/units").iterdir()):
            plan.write(home / ".config/systemd/user" / unit.name, unit.read_bytes())
    if "applications" in parts:
        for source in desktop_sources():
            text = desktop_text(source.read_text(), libdir)
            plan.write(home / ".local/share/applications" / source.name, text.encode())
    if "dbus" in parts:
        service = REPO / "components/keyring/data/org.freedesktop.secrets.service"
        if service.is_file():
            plan.write(home / ".local/share/dbus-1/services" / service.name, service.read_bytes())
    if "retire-old" in parts:
        applications = home / ".local/share/applications"
        if applications.is_dir():
            for path in sorted(applications.iterdir()):
                if path.name.startswith(OLD_USER_DESKTOP) or path.name in OLD_USER_DESKTOP:
                    plan.remove(path)
        dropins = old_portal_dropins(home)
        if dropins and packaged_portal_ready("/"):
            for path in dropins:
                plan.remove(path)
            if Path(home).resolve() == Path.home().resolve():
                # Reload unit files only (nothing is started, stopped or restarted), so the user
                # manager never keeps the removed override if it outlives this session.
                plan.commands.append((["systemctl", "--user", "daemon-reload"], dict(os.environ)))
            plan.notes.append("The screen-share portal switches to the distribution's xdg-desktop-portal-wlr at the "
                              "next login (the running portal is not restarted).")
        elif dropins:
            plan.notes.append("Kept the waycapture-portal-wlr override: install xdg-desktop-portal-wlr 0.8.4-4 or "
                              "newer first (it carries the chooser-session and RemoteDesktop patches).")
    if "session" in parts:
        environment = {k: v for k, v in os.environ.items() if k != "LABWC_PID"}
        environment["HOME"] = str(home)
        for program in ("tessel-configure-session", "tessel-xdg-autostart"):
            plan.commands.append(([str(libdir / program), "--apply"], environment))
        plan.notes.append("labwc's autostart, rc.xml keybindings and environment are regenerated after the "
                          "programs are in place (they take effect at the next login).")
    return plan


# -------------------------------------------------------- system scope ---

def pam_text(text, edits):
    """Rename an existing pam_waykeyring line, else add the Tessel lines once."""
    if PAM_MODULE in text:
        return text
    if "pam_waykeyring.so" in text:
        return text.replace("pam_waykeyring.so", PAM_MODULE)
    for old, new in edits:
        if old in text:
            text = text.replace(old, new, 1)
    return text


def system_plan(root, parts, target_dir):
    plan = Plan(root)
    libdir = root / "usr/local/lib/tessel"
    if "keyring" in parts:
        found, missing = built_files(target_dir, ["tessel-keyring", "tessel-keyring-prompt"])
        if missing:
            raise SystemExit("Build first, as your user: cargo build --release -p tessel-keyring -p tessel-keyring-ui "
                             "-p pam-tessel-keyring (missing: " + ", ".join(missing) + ")")
        for name, path in found.items():
            plan.write(libdir / name, path.read_bytes(), 0o755)
            plan.link(root / "usr/local/bin" / name, f"../lib/tessel/{name}")
        module = Path(target_dir) / "release/libpam_tessel_keyring.so"
        if not module.is_file():
            raise SystemExit(f"Build first, as your user: cargo build --release -p pam-tessel-keyring ({module})")
        plan.write(root / "usr/lib64/security" / PAM_MODULE, module.read_bytes(), 0o755)
        for relative, edits in PAM_EDITS.items():
            path = root / relative
            if path.is_file():
                text = path.read_text()
                new = pam_text(text, edits)
                if new != text:
                    plan.write(path, new.encode(), path.stat().st_mode & 0o7777)
        plan.notes.append("PAM changed: if a login ever fails, switch to a text console (Ctrl+Alt+F3) and run "
                          "sudo /usr/bin/python3 -I " + str(REPO / "tools/install.py") + " --system --rollback")
    if "lock-pam" in parts:
        pam = root / "etc/pam.d"
        for old, new in LOCK_PAM:
            if (pam / old).is_file() and not (pam / new).exists():
                plan.write(pam / new, (pam / old).read_bytes(), (pam / old).stat().st_mode & 0o7777)
    if "login" in parts:
        found, missing = built_files(target_dir, LOGIN_PROGRAMS)
        if missing:
            raise SystemExit("Build first, as your user: cargo build --release --workspace (missing: "
                             + ", ".join(sorted(missing)) + ")")
        for name, path in found.items():
            plan.write(libdir / name, path.read_bytes(), 0o755)
        for script in sorted((REPO / "data/login").iterdir()):
            plan.write(libdir / script.name, script.read_bytes(), 0o755)
        old, new = root / "etc/waylogin", root / "etc/tessel/login"
        if (old / "appearance.json").is_file() and not (new / "appearance.json").exists():
            data = json.loads((old / "appearance.json").read_text())
            if data.get("default_session") == "labwc":
                data["default_session"] = "tessel"
            plan.write(new / "appearance.json", (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode(), 0o644)
        for name in ("labwc/rc.xml", "labwc/autostart"):
            if (old / name).is_file() and not (new / name).exists():
                plan.write(new / name, (old / name).read_bytes(), (old / name).stat().st_mode & 0o7777)
        if (old / "greetd.toml").is_file():
            text = (old / "greetd.toml").read_text()
            text = text.replace("/usr/local/lib/waylogin/start-greeter", "/usr/local/lib/tessel/tessel-start-greeter")
            text = text.replace('runfile = "/run/waylogin.run"', 'runfile = "/run/tessel-login.run"')
            plan.write(new / "greetd.toml", text.encode(), 0o644)
        unit = root / "etc/systemd/system/waylogin.service"
        if unit.is_file():
            text = unit.read_text()
            new_text = text.replace("--config /etc/waylogin/greetd.toml", "--config /etc/tessel/login/greetd.toml")
            new_text = new_text.replace("Description=Waylogin Python Wayland Login Manager", "Description=Tessel login screen")
            if new_text != text:
                plan.write(unit, new_text.encode(), unit.stat().st_mode & 0o7777)
                if root == Path("/"):
                    plan.commands.append((["systemctl", "daemon-reload"], dict(os.environ)))
        plan.notes.append("The login screen switches at the next boot (the running one is not restarted). If it "
                          "ever fails to appear, press Ctrl+Alt+F3, log in, and run: sudo /usr/bin/python3 -I "
                          + str(REPO / "tools/install.py") + " --system --rollback")
    if "session-entry" in parts:
        entry = REPO / "data/wayland-sessions/tessel.desktop"
        plan.write(root / "usr/local/share/wayland-sessions/tessel.desktop", entry.read_bytes())
        plan.write(root / "usr/local/share/icons/hicolor/scalable/apps/tessel.svg",
                   (REPO / "branding/tessel-logo.svg").read_bytes())
        # The login screen preselects Tessel where it preselected plain labwc (the same session).
        for appearance in (root / "etc/waylogin/appearance.json", root / "etc/tessel/login/appearance.json"):
            if appearance.is_file():
                data = json.loads(appearance.read_text())
                if data.get("default_session") == "labwc":
                    data["default_session"] = "tessel"
                    plan.write(appearance, (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode(),
                               appearance.stat().st_mode & 0o7777)
    if "retire-old" in parts:
        for pattern in OLD_SYSTEM_DESKTOP:
            directory = root / Path(pattern).parent
            prefix = Path(pattern).name
            if directory.is_dir():
                for path in sorted(directory.iterdir()):
                    if path.name.startswith(prefix) and path.suffix == ".desktop":
                        plan.remove(path)
        old_portal = root / OLD_PORTAL
        if old_portal.exists():
            homes = root / "home"
            users = [h for h in sorted(homes.iterdir()) if old_portal_dropins(h)] if homes.is_dir() else []
            if not packaged_portal_ready(root):
                plan.notes.append(f"Kept {old_portal}: the installed xdg-desktop-portal-wlr lacks the chooser-session "
                                  "patch (needs 0.8.4-4 or newer).")
            elif users:
                plan.notes.append(f"Kept {old_portal}: still used by " + ", ".join(str(u) for u in users)
                                  + " (run the user install with --only retire-old there first).")
            else:
                plan.remove(old_portal)
    return plan


# ------------------------------------------------------- apply/rollback ---

def atomic_write(path, data, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tessel-install")
    temporary.write_bytes(data)
    os.chmod(temporary, mode)
    os.replace(temporary, path)


def record_dir(state):
    stamp = datetime.datetime.now().strftime("%Y%m%dT%H%M%S%f")
    directory = state / "install" / stamp
    directory.mkdir(parents=True)
    return directory


def preserve(path, record, base, entries, action):
    """Copy what is at `path` into the record before it changes."""
    entry = {"path": str(path), "action": action}
    if path.is_symlink():
        entry["link"] = os.readlink(path)
    elif path.is_file():
        copy = record / "files" / path.relative_to(base)
        copy.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, copy)
        entry["copy"] = str(copy)
        entry["mode"] = path.stat().st_mode & 0o7777
    entries.append(entry)


def apply(plan, state, owner=None):
    record = record_dir(state)
    entries = []
    try:
        for target, data, mode in plan.writes:
            preserve(target, record, plan.base, entries, "write")
            if target.is_symlink():
                target.unlink()
            atomic_write(target, data, mode)
        for target, to in plan.links:
            preserve(target, record, plan.base, entries, "link")
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.is_symlink() or target.exists():
                target.unlink()
            target.symlink_to(to)
        for target in plan.removals:
            preserve(target, record, plan.base, entries, "remove")
            target.unlink()
    finally:
        (record / "manifest.json").write_text(json.dumps(entries, indent=1) + "\n")
    for argv, environment in plan.commands:
        subprocess.run(argv, check=True, env=environment)
    return record


def rollback(state):
    records = sorted((state / "install").glob("*/manifest.json")) if (state / "install").is_dir() else []
    if not records:
        raise SystemExit("Nothing to roll back.")
    manifest = records[-1]
    entries = json.loads(manifest.read_text())
    for entry in reversed(entries):
        path = Path(entry["path"])
        if path.is_symlink() or path.exists():
            path.unlink()
        if "link" in entry:
            path.symlink_to(entry["link"])
        elif "copy" in entry:
            atomic_write(path, Path(entry["copy"]).read_bytes(), entry["mode"])
    manifest.rename(manifest.with_name("manifest.rolled-back.json"))
    print(f"Restored {len(entries)} files from {manifest.parent}.")


def describe(plan):
    for target, _, _ in plan.writes:
        print(("update  " if target.exists() else "create  ") + str(target))
    for target, to in plan.links:
        print(f"link    {target} -> {to}")
    for target in plan.removals:
        print(f"retire  {target}")
    for argv, _ in plan.commands:
        print("run     " + " ".join(argv))
    for note in plan.notes:
        print("note    " + note)
    total = len(plan.writes) + len(plan.links) + len(plan.removals)
    print(f"{total} files to change.")


def main(argv=None):
    parser = argparse.ArgumentParser(prog="install.py", description=__doc__.split("\n\n")[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="preview only (the default)")
    mode.add_argument("--apply", action="store_true", help="install")
    mode.add_argument("--rollback", action="store_true", help="undo the newest --apply")
    parser.add_argument("--system", action="store_true", help="root-owned parts (run through pkexec)")
    parser.add_argument("--only", help="comma-separated parts (user: %s; system: %s; system, only when named: %s)" % (
        ",".join(USER_PARTS), ",".join(SYSTEM_PARTS), ",".join(EXPLICIT_SYSTEM_PARTS)))
    parser.add_argument("--no-build", action="store_true", help="use the existing release build")
    parser.add_argument("--target-dir", help="cargo target directory (default: <repo>/target)")
    parser.add_argument("--home", help=argparse.SUPPRESS)   # tests
    parser.add_argument("--root", help=argparse.SUPPRESS)   # tests
    args = parser.parse_args(argv)
    target_dir = Path(args.target_dir) if args.target_dir else REPO / "target"
    known = SYSTEM_PARTS if args.system else USER_PARTS
    parts = tuple(p for p in args.only.split(",") if p) if args.only else known
    unknown = [p for p in parts if p not in known + (EXPLICIT_SYSTEM_PARTS if args.system else ())]
    if unknown:
        parser.error(f"unknown part(s) {', '.join(unknown)}; choose from {', '.join(known)}")

    if args.system:
        root = Path(args.root or "/")
        if args.root is None and os.geteuid() != 0:
            raise SystemExit("--system needs root: pkexec /usr/bin/python3 -I " + str(Path(__file__).resolve())
                             + " --system --apply")
        state = root / "var/lib/tessel"
        if args.rollback:
            return rollback(state)
        if args.target_dir is None and args.root is None:
            target_dir = REPO / "target"
        plan = system_plan(root, parts, target_dir)
        describe(plan)
        if args.apply and not plan.empty():
            record = apply(plan, state)
            print(f"Installed. Record: {record}")
        return 0

    if os.geteuid() == 0 and args.home is None:
        raise SystemExit("Run the user install as yourself, not as root.")
    home = Path(args.home or Path.home())
    state = home / ".local/state/tessel"
    if args.rollback:
        return rollback(state)
    binaries = workspace_binaries()
    if args.apply and not args.no_build and "programs" in parts:
        build(target_dir)
    plan = user_plan(home, parts, binaries, target_dir)
    describe(plan)
    if args.apply and not plan.empty():
        record = apply(plan, state)
        print(f"Installed. Record: {record}")
        print("Nothing was started, stopped or restarted. Sign out and back in to start Tessel.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
