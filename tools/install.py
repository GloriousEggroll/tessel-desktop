#!/usr/bin/env python3
"""Swap desktop suite components between the Python suite (~/btmanager) and their Rust ports.

  install.py [--check]                   preview every file and unit change (default)
  install.py --apply [--only a,b]        build, install and switch the components to Rust
  install.py --rollback [--only a,b]     restore the Python files exactly (add --check to preview)
  install.py --status [--json]           which components run Rust or Python, with their RSS
  install.py --list                      the component table

What a swap does, per component:
  * installs the release binaries (and helpers such as waykeys-input) INSIDE the component's
    scripts dir, ~/.config/waybar/scripts/<component>/, next to the Python modules: the ports
    find their helpers relative to the executable (battery: ../btmanager/btmanager.py;
    notifications: waydock-toplevel, fullscreen_guard.py; OSK: waykeys-input). There is no
    <exe dir>/locale there, so the Rust i18n reads ~/.local/share/desktop-suite/locale, which
    this installer fills from ~/btmanager/shared/locale,
  * adds the drop-in <unit>.d/zz-desktop-suite-rust.conf that resets ExecStart to the binary.
    The base unit and the other drop-ins (50-desktop-suite-recovery.conf, desktop-suite.conf,
    palette.conf) stay untouched; "zz-" sorts last, so this ExecStart is the one systemd runs,
  * points ~/.local/bin launchers and .desktop Exec lines that ran the Python entry at the binary,
  * replaces only the Python entry scripts (incl. the *_status.py streams waydock runs with
    `python3 -u`) with shims that exec the binaries with the same argv: the porters' files from
    components/<name>/shim(s)/ where present, generated ones otherwise. Each shim carries the
    marker "desktop-suite-rust shim" and every flag as a quoted literal, because
    desktop-suite-action greps the installed source for them ('--power-button', ...). All other
    modules stay installed: Global Settings, waydock, lock-screen and others import them.

Every changed file is backed up in ~/.local/state/desktop-suite/backups/rust-swap-<time>-*/
(manifest.json + the original files, like the suite installers), and
~/.local/state/desktop-suite/rust-swap.json remembers the pre-swap state of each path so that
--rollback restores it exactly, also after several applies. Symlinked targets are refused.

Nothing is started, stopped or restarted unless --restart is given; then only the swapped units
that are active get `systemctl --user restart` (never labwc; waydock/wayshell also need
--restart-shell). --apply/--rollback run `systemctl --user daemon-reload` so systemd sees the
drop-in change (pass --no-reload to skip it).
"""
import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import stat
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parent.parent
REAL_HOME = Path(pwd.getpwuid(os.getuid()).pw_dir)
DEFAULT_LOCALE_SOURCE = REAL_HOME / "btmanager/shared/locale"

SCRIPTS = ".config/waybar/scripts"
LOCALE = ".local/share/desktop-suite/locale"
UNITS = ".config/systemd/user"
DROPIN = "zz-desktop-suite-rust.conf"
STATE = ".local/state/desktop-suite/rust-swap.json"
BACKUPS = ".local/state/desktop-suite/backups"
SHIM_MARKER = "desktop-suite-rust shim"
SHARED = "_shared"            # state group for files shared by every component (locale)
SHELL_UNITS = {"waydock.service", "wayshell.service"}
NEVER_RESTART = {"labwc.service", "graphical-session.target"}
PYTHON = re.compile(r"^python3(?:\.\d+)?$")


@dataclass(frozen=True)
class Shim:
    entry: str                # file name of the Python entry script in the component's scripts dir
    binary: str               # Rust binary (same dir) that receives the argv
    flags: tuple = ()         # every flag the entry accepts (kept as quoted literals for grep probes)
    porter: str = None        # shim written by the porter, relative to the repo (used verbatim + marker)
    fallback: str = None      # the porter shim execs this saved Python original if the binary is gone


@dataclass(frozen=True)
class Unit:
    name: str
    binary: str
    args: tuple = ()


@dataclass(frozen=True)
class Component:
    key: str
    package: str              # cargo package (components/<package>)
    dir: str                  # ~/.config/waybar/scripts/<dir>: binaries, helpers and shims live here
    binaries: tuple           # [[bin]] names of the package, verified by test_install.py
    shims: tuple
    units: tuple = ()
    helpers: tuple = ()       # non-cargo executables from the crate's build output (waykeys-input)
    launchers: tuple = ()     # ~/.local/bin wrappers that exec the Python entry (shims[0])
    desktop: tuple = ()       # .desktop files whose Exec runs the Python entry (shims[0])
    keeps: tuple = ()         # files others import or the binary needs beside it: never touched
    clobbered_by: tuple = ()  # Python installers that rewrite a shimmed entry (besides install_gtk4)
    notes: tuple = ()


POPUP = ("--popup", "--popup-edge", "--popup-offset")
APPS = ".local/share/applications"
# Python installers that copy every component *.py back, so they overwrite every shim.
CLOBBER_ALL = ("shared/install_gtk4.py", "install_all.py")
# Python installers that recognise a running waydock child by its *_status.py argv: with the Rust
# status streams they see "an unrelated application" and refuse to restart waydock.service.
STATUS_GUARD = ("shared/install_gtk4.py verify_service_children: waydock children running the Rust "
                "*-status binaries no longer match the *_status.py paths, so Python installers that "
                "restart waydock.service refuse with 'still owns an unrelated application'.")

COMPONENTS = (
    Component(
        "keep-awake", "keep-awake", "keep-awake", ("keep-awake",),
        shims=(Shim("keep_awake.py", "keep-awake",
                    ("--background", *POPUP, "--status", "--enable", "--disable", "--toggle"),
                    porter="components/keep-awake/shim/keep_awake.py", fallback="keep_awake_python.py"),),
        units=(Unit("keep-awake.service", "keep-awake", ("--background",)),),
        keeps=("keep_awake_ui.py", "keep_awake_client.py", "keep_awake_backend.py"),
        clobbered_by=("wayshell/install_switcher.py",),
        notes=("idle_inhibit.sh / toggle_idle_inhibit.sh and waydock run keep_awake.py: served by the shim.",
               "The porter shim falls back to keep_awake_python.py (the saved Python entry) if the binary is gone.")),
    Component(
        "battery-manager", "battery-manager", "battery-manager", ("battery-manager", "battery-status"),
        shims=(Shim("battery_manager.py", "battery-manager", (*POPUP, "--settings", "--background", "--check"),
                    porter="components/battery-manager/shims/battery_manager.py"),
               Shim("battery_status.py", "battery-status",
                    porter="components/battery-manager/shims/battery_status.py")),
        units=(Unit("battery-manager.service", "battery-manager", ("--background",)),),
        desktop=(f"{APPS}/org.waybar.BatteryManager.desktop",),
        keeps=("battery_ui.py", "../btmanager/btmanager.py"),
        clobbered_by=("battery-manager/install_waybar.py",),
        notes=(STATUS_GUARD,)),
    Component(
        "power-manager", "power-manager", "power-manager", ("power-manager", "power-status"),
        shims=(Shim("power_manager.py", "power-manager",
                    ("--background", *POPUP, "--settings", "--check", "--idle-action", "--idle-resume",
                     "--policy-token", "--power-button"), porter="components/power-manager/shim/power_manager.py"),
               Shim("power_status.py", "power-status", porter="components/power-manager/shim/power_status.py")),
        units=(Unit("power-manager.service", "power-manager", ("--background",)),),
        desktop=(f"{APPS}/org.waybar.PowerManager.desktop",),
        keeps=("power_backend.py", "power_ui.py", "power_client.py", "power_config.py", "power_battery_ui.py",
               "power_charge_limits.py", "power_style.py"),
        clobbered_by=("power-manager/install_waybar.py",),
        notes=("desktop-suite-action greps power_manager.py for '--power-button': kept in the shim.",
               "power-manager-lock.service belongs to lock-screen and is not changed.", STATUS_GUARD)),
    Component(
        "notification-manager", "notification-manager", "notification-manager",
        ("notification-manager", "notification-status"),
        shims=(Shim("notification_manager.py", "notification-manager", ("--daemon", "--check", "--settings", *POPUP),
                    porter="components/notification-manager/shims/notification_manager.py"),
               Shim("notification_status.py", "notification-status",
                    porter="components/notification-manager/shims/notification_status.py")),
        units=(Unit("notifications.service", "notification-manager", ("--daemon",)),),
        launchers=(".local/bin/notifications",),
        keeps=("notification_client.py", "notification_ui.py", "waydock-toplevel", "fullscreen_guard.py"),
        clobbered_by=("notification-manager/install.py", "shared/install_popup_fixes.py"),
        notes=("notifications.service.d/desktop-suite.conf (Python ExecStart) is kept; zz- overrides it.",
               STATUS_GUARD)),
    Component(
        "on-screen-keyboard", "on-screen-keyboard", "on-screen-keyboard", ("osk-manager", "osk-status"),
        shims=(Shim("osk_manager.py", "osk-manager",
                    ("--background", "--toggle", "--show", "--hide", "--settings", "--quit")),
               Shim("osk_status.py", "osk-status")),
        units=(Unit("on-screen-keyboard.service", "osk-manager", ("--background",)),),
        helpers=("waykeys-input",),
        launchers=(".local/bin/waykeys",),
        keeps=("osk_settings.py",),
        clobbered_by=("on-screen-keyboard/install.py", "shared/install_popup_fixes.py"),
        notes=("waykeys-input is replaced by the crate's build (same native/input.c); "
               "lock-screen's copies in ~/.local/share/lock-screen are not changed.", STATUS_GUARD)),
    Component(
        "audio-manager", "audio-manager", "audio-manager", ("audio-manager", "audio-manager-loopbacks"),
        shims=(Shim("audio_manager.py", "audio-manager",
                    ("--settings", *POPUP, "--volume-up", "--volume-down", "--toggle-mute")),
               Shim("loopback_service.py", "audio-manager-loopbacks", ("--config",))),
        units=(Unit("audio-manager-loopbacks.service", "audio-manager-loopbacks"),),
        keeps=("audio_ui.py", "audio_backend.py", "audio_popup.py", "volume_keys.py", "loopback_store.py"),
        clobbered_by=("audio-manager/install_waybar.py", "shared/install_adjustment_osd.py"),
        notes=("Python installers find a running audio_manager.py by its argv (install_gtk4 "
               "standalone_instances, install_adjustment_osd audio_owner, install_waybar --update): "
               "they will not see the Rust instance.",)),
)
# Future (wave 3): wayshell must override wayshell.service.d/palette.conf, which is what runs it
# (the base unit's ExecStart is stale); "zz-" sorts after "palette.conf", so the same drop-in works.
BY_KEY = {component.key: component for component in COMPONENTS}


# ---- small file helpers (same semantics as the suite installers) -------------------------------

def atomic_write(path, data, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=".desktop-suite-rust-", dir=path.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        temporary.chmod(mode)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def snapshot(path):
    if path.is_symlink():
        return {"kind": "symlink", "link": os.readlink(path)}
    if not path.exists():
        return {"kind": "missing"}
    if not path.is_file():
        raise RuntimeError(f"Expected a regular file: {path}")
    return {"kind": "file", "data": path.read_bytes(), "mode": stat.S_IMODE(path.stat().st_mode)}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def current_digest(path):
    return digest(path.read_bytes()) if path.is_file() and not path.is_symlink() else None


def systemctl(*args, check=False):
    return subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True, check=check)


# ---- what each component's swap writes ----------------------------------------------------------

@dataclass
class Change:
    component: str
    path: Path
    kind: str                 # binary | helper | shim | fallback | dropin | launcher | desktop | locale
    data: bytes = None        # None: binary not built yet (preview only)
    mode: int = 0o644
    note: str = ""
    source: Path = None


def component_dir(home, component):
    return home / SCRIPTS / component.dir


def binary_path(home, component, name):
    return component_dir(home, component) / name


def entry_path(home, component, shim):
    return component_dir(home, component) / shim.entry


def marker_lines(component, shim):
    lines = [f"# {SHIM_MARKER} for {component.key}: installed by nobara-de-rust/tools/install.py;"
             f" undo with --rollback --only {component.key}\n"]
    if shim.flags:
        lines.append("# flags (desktop-suite-action greps for them): "
                     + ", ".join(repr(flag) for flag in shim.flags) + "\n")
    return lines


def shim_text(component, shim):
    """The porter's shim with the marker added after the shebang, or a generated equivalent."""
    if shim.porter:
        lines = (REPO / shim.porter).read_text().splitlines(keepends=True)
        head = lines[:1] if lines and lines[0].startswith("#!") else []
        return "".join(head + marker_lines(component, shim) + lines[len(head):])
    return ("#!/usr/bin/env python3\n" + "".join(marker_lines(component, shim))
            + f'"""Shim: the Rust `{shim.binary}` binary next to this file now implements {shim.entry}.\n\n'
            f"Callers keep running `python3 {shim.entry} ...` with the same arguments; argv[0] stays this\n"
            f'path so usage and error messages still name {shim.entry}."""\n'
            "import os\nimport sys\n\n"
            f"BINARY = os.path.join(os.path.dirname(os.path.realpath(__file__)), {shim.binary!r})\n\n"
            "if __name__ == '__main__':\n"
            "    try:\n"
            "        os.execv(BINARY, [os.path.abspath(__file__), *sys.argv[1:]])\n"
            "    except OSError as error:\n"
            f"        print(f'{shim.entry}: cannot run {{BINARY}}: {{error}}', file=sys.stderr)\n"
            "        sys.exit(127)\n")


def unit_command(home, component, unit):
    relative = binary_path(home, component, unit.binary).relative_to(home)
    return " ".join([f"%h/{relative}", *unit.args])


def dropin_text(component, unit, home):
    return (f"# Desktop suite Rust swap for {component.key}, generated by nobara-de-rust/tools/install.py.\n"
            f"# Sorts after every other drop-in, so this ExecStart is the one systemd runs.\n"
            f"# Undo: tools/install.py --rollback --only {component.key}\n"
            f"[Service]\nExecStart=\nExecStart={unit_command(home, component, unit)}\n")


def launcher_text(component, home):
    binary = binary_path(home, component, component.shims[0].binary)
    return (f"#!/bin/sh\n# Desktop suite Rust swap for {component.key} (nobara-de-rust/tools/install.py).\n"
            f"exec '{binary}' \"$@\"\n")


def entry_pattern(home, entry):
    """Match a python3 invocation of the entry, written with an absolute, ~ or $HOME path."""
    forms = [re.escape(str(home / entry)), r"(?:~|\$HOME|\$\{HOME\})/" + re.escape(entry)]
    path = "(?:" + "|".join(forms) + ")"
    return re.compile(r"(?:/usr/bin/)?python3\s+([\"']?)" + path + r"\1")


def desktop_text(text, component, home):
    binary = str(binary_path(home, component, component.shims[0].binary))
    pattern = entry_pattern(home, f"{SCRIPTS}/{component.dir}/{component.shims[0].entry}")
    return re.sub(r"(?m)^(Exec=)(.*)$", lambda m: m[1] + pattern.sub(lambda _: binary, m[2]), text)


def effective_execstart(home, unit_name, skip_ours=False):
    """ExecStart systemd would use from the user unit and its drop-ins (in filename order)."""
    base = home / UNITS / unit_name
    files = [base] if base.is_file() else []
    dropins = home / UNITS / (unit_name + ".d")
    if dropins.is_dir():
        files += sorted((p for p in dropins.glob("*.conf") if not (skip_ours and p.name == DROPIN)),
                        key=lambda p: p.name)
    value = None
    for path in files:
        section = None
        for line in path.read_text(errors="replace").splitlines():
            line = line.strip()
            if line.startswith("["):
                section = line
            elif section == "[Service]" and line.startswith("ExecStart="):
                value = line[len("ExecStart="):] or None
    return value


def built_helper(package, name):
    """A helper the crate's build.rs puts in OUT_DIR (newest build wins)."""
    found = sorted((REPO / "target/release/build").glob(f"{package}-*/out/{name}"),
                   key=lambda p: p.stat().st_mtime, reverse=True)
    return found[0] if found else REPO / f"target/release/build/{package}-*/out/{name}"


def resolve_sources(components, prebuilt):
    """{(component key, file name): source path} for every binary and helper."""
    sources = {}
    for component in components:
        for name in component.binaries:
            sources[component.key, name] = (prebuilt or REPO / "target/release") / name
        for name in component.helpers:
            sources[component.key, name] = prebuilt / name if prebuilt else built_helper(component.package, name)
    return sources


def valid_binary(path):
    try:
        with open(path, "rb") as stream:
            head = stream.read(4)
    except OSError:
        return False
    return os.access(path, os.X_OK) and (head == b"\x7fELF" or head[:2] == b"#!")


def is_shim(path):
    try:
        return SHIM_MARKER in path.read_text(errors="replace")
    except OSError:
        return False


def plan_component(component, home, sources):
    """[Change] for one component (unchanged files left out) and [problem] that block the swap."""
    changes, problems = [], []
    if not component_dir(home, component).is_dir():
        return changes, [f"{SCRIPTS}/{component.dir} is not installed; install the Python component first"]
    for name in (*component.binaries, *component.helpers):
        target, source = binary_path(home, component, name), sources[component.key, name]
        kind = "helper" if name in component.helpers else "binary"
        if not valid_binary(source):
            changes.append(Change(component.key, target, kind, None, 0o755,
                                  f"not built: {source} (cargo build --release -p {component.package})"))
            continue
        data = source.read_bytes()
        if target.is_symlink() or current_digest(target) != digest(data) or stat.S_IMODE(target.stat().st_mode) != 0o755:
            changes.append(Change(component.key, target, kind, data, 0o755, f"from {source}", source))
    for shim in component.shims:
        path = entry_path(home, component, shim)
        if shim.porter and not (REPO / shim.porter).is_file():
            problems.append(f"porter shim {shim.porter} is missing")
            continue
        data = shim_text(component, shim).encode()
        if not path.is_file() and not path.is_symlink():
            problems.append(f"{path.relative_to(home)} is not installed; install the Python component first")
            continue
        if shim.fallback and not path.is_symlink() and not is_shim(path):
            fallback = component_dir(home, component) / shim.fallback
            original = path.read_bytes()
            if fallback.is_symlink() or current_digest(fallback) != digest(original):
                changes.append(Change(component.key, fallback, "fallback", original, 0o644,
                                      f"save the Python {shim.entry} for the shim's fallback"))
        if path.is_file() and not path.is_symlink() and path.read_bytes() == data:
            continue
        mode = stat.S_IMODE(path.lstat().st_mode) if path.exists() else 0o755
        changes.append(Change(component.key, path, "shim", data, mode,
                              f"replace the Python entry with a shim -> {shim.binary}"
                              + (f" ({shim.porter})" if shim.porter else " (generated)")))
    for unit in component.units:
        if not (home / UNITS / unit.name).is_file():
            problems.append(f"{UNITS}/{unit.name} is not installed")
            continue
        path = home / UNITS / (unit.name + ".d") / DROPIN
        data = dropin_text(component, unit, home).encode()
        if current_digest(path) != digest(data):
            before = effective_execstart(home, unit.name, skip_ours=True)
            changes.append(Change(component.key, path, "dropin", data, 0o644,
                                  f"ExecStart: {before} -> {unit_command(home, component, unit)}"))
    entry = f"{SCRIPTS}/{component.dir}/{component.shims[0].entry}"
    for relative in component.launchers:
        path = home / relative
        if not path.is_file():
            continue
        data = launcher_text(component, home).encode()
        text = path.read_text(errors="replace")
        if path.read_bytes() != data and (entry in text or path.is_symlink()):
            changes.append(Change(component.key, path, "launcher", data, stat.S_IMODE(path.lstat().st_mode),
                                  f"exec {binary_path(home, component, component.shims[0].binary)}"))
    for relative in component.desktop:
        path = home / relative
        if not path.is_file():
            continue
        text = path.read_text(errors="replace")
        new = desktop_text(text, component, home)
        if new != text:
            changes.append(Change(component.key, path, "desktop", new.encode(), stat.S_IMODE(path.lstat().st_mode),
                                  "Exec runs the binary"))
    return changes, problems


def guard_warnings(component):
    """Python-installer behaviour that undoes or trips over this swap."""
    shims = ", ".join(shim.entry for shim in component.shims)
    return [f"re-running {', '.join(CLOBBER_ALL + component.clobbered_by)} rewrites {shims}"
            f" (then --status shows 'python (shim overwritten)'; re-run --apply)"]


def plan_locale(home, locale_source):
    changes, problems = [], []
    catalogs = sorted(locale_source.glob("*.json")) if locale_source.is_dir() else []
    if not catalogs:
        problems.append(f"no translation catalogs in {locale_source}")
    for source in catalogs:
        target = home / LOCALE / source.name
        data = source.read_bytes()
        if current_digest(target) != digest(data) or target.is_symlink():
            changes.append(Change(SHARED, target, "locale", data, 0o644, f"from {source}", source))
    return changes, problems


# ---- state ------------------------------------------------------------------------------------

def load_state(home):
    path = home / STATE
    if path.is_symlink():
        raise RuntimeError("Refusing a linked state file: " + str(path))
    if not path.is_file():
        return {"version": 1, "components": {}, "dirs": []}
    return json.loads(path.read_text())


def save_state(home, state):
    atomic_write(home / STATE, (json.dumps(state, indent=2, sort_keys=True) + "\n").encode(), 0o644)


def make_backup(home, prefix):
    root = home / BACKUPS
    root.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ-")
    return Path(tempfile.mkdtemp(prefix=f"{prefix}-{stamp}", dir=root))


def missing_parents(path, home):
    """Directories atomic_write would create for path, outermost first."""
    created = []
    parent = path.parent
    while parent != home and not parent.exists():
        created.append(parent)
        parent = parent.parent
    return list(reversed(created))


# ---- apply / rollback ---------------------------------------------------------------------------

def build(components):
    packages = sorted({component.package for component in components})
    for package in packages:
        print(f"cargo build --release -p {package}")
        subprocess.run(["cargo", "build", "--release", "-p", package], cwd=REPO, check=True)


def apply(home, components, sources, locale_source):
    planned, problems = [], []
    for component in components:
        changes, issues = plan_component(component, home, sources)
        planned += changes
        problems += [f"{component.key}: {issue}" for issue in issues]
    locale_changes, issues = plan_locale(home, locale_source)
    planned += locale_changes
    problems += [f"locale: {issue}" for issue in issues]
    problems += [f"{change.component}: {change.note}" for change in planned if change.data is None]
    for change in planned:
        if change.path.is_symlink():
            problems.append(f"Refusing to replace a linked installed file: {change.path}")
    if problems:
        raise RuntimeError("Nothing was changed:\n  " + "\n  ".join(problems))
    if not planned:
        print("Everything is already swapped; nothing to change.")
        return []
    befores = [(change, snapshot(change.path)) for change in planned]
    backup = make_backup(home, "rust-swap")
    manifest = []
    for change, before in befores:
        if before["kind"] == "file":
            atomic_write(backup / change.path.relative_to(home), before["data"], before["mode"])
        manifest.append({"path": str(change.path), "component": change.component, "kind": change.kind,
                         "before": before["kind"], "after_sha256": digest(change.data)})
    (backup / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    state = load_state(home)
    written, created_dirs = [], {}
    try:
        for change, before in befores:
            if snapshot(change.path) != before:
                raise RuntimeError("An installed file changed during the swap: " + str(change.path))
            created_dirs.setdefault(change.component, []).extend(
                str(d) for d in missing_parents(change.path, home))
            atomic_write(change.path, change.data, change.mode)
            written.append((change, before))
    except BaseException:
        for change, before in reversed(written):
            restore(change.path, before)
        for directories in created_dirs.values():
            for directory in reversed(directories):
                try:
                    Path(directory).rmdir()
                except OSError:
                    pass
        raise
    for change, before in befores:
        group = state["components"].setdefault(change.component, {"files": {}})
        record = group["files"].get(str(change.path))
        if record is None:          # first swap of this path: remember the Python original
            record = {"before": before["kind"], "backup": str(backup), "kind": change.kind}
            if before["kind"] == "file":
                record["mode"] = before["mode"]
            group["files"][str(change.path)] = record
        record["installed_sha256"] = digest(change.data)
    known = state.setdefault("dirs", [])
    known += [d for directories in created_dirs.values() for d in directories if d not in known]
    save_state(home, state)
    print(f"Swapped {len(planned)} files. Backup: {backup}")
    return planned


def restore(path, before):
    if before["kind"] == "file":
        atomic_write(path, before["data"], before["mode"])
    elif before["kind"] == "missing":
        path.unlink(missing_ok=True)
    else:
        path.unlink(missing_ok=True)
        path.symlink_to(before["link"])


def rollback_plan(home, keys, state):
    """[(group, path, record, action)] with action restore | remove | keep (changed since the swap)."""
    groups = list(keys)
    remaining = set(state["components"]) - set(keys) - {SHARED}
    if not remaining and SHARED in state["components"]:
        groups.append(SHARED)       # catalogs go only once no component uses them any more
    steps = []
    for group in groups:
        for name, record in state["components"].get(group, {}).get("files", {}).items():
            path = Path(name)
            if path.is_symlink():
                steps.append((group, path, record, "refuse"))
            elif current_digest(path) != record["installed_sha256"]:
                steps.append((group, path, record, "keep"))
            else:
                steps.append((group, path, record, "remove" if record["before"] == "missing" else "restore"))
    return groups, steps


def rollback(home, keys, preview, force=False):
    state = load_state(home)
    keys = [key for key in keys if key in state["components"]]
    if not keys:
        print("Nothing to roll back: none of these components are swapped.")
        return []
    groups, steps = rollback_plan(home, keys, state)
    for group, path, record, action in steps:
        label = {"restore": "restore", "remove": "remove ", "keep": "KEEP   ", "refuse": "REFUSE "}[action]
        extra = {"keep": "  (changed since the swap; kept, use --force to restore the backup)",
                 "refuse": "  (now a symlink)"}.get(action, "")
        if action == "keep" and force:
            label, extra = "restore", "  (changed since the swap; --force)"
        print(f"  {group:24} {label} {path}{extra}")
    if preview:
        return []
    refused = [path for _, path, _, action in steps if action == "refuse"]
    if refused:
        raise RuntimeError("Refusing to replace linked files: " + ", ".join(map(str, refused)))
    backup = make_backup(home, "rust-rollback")
    manifest = []
    for group, path, record, action in steps:
        if action == "keep" and not force:
            continue
        current = snapshot(path)
        if current["kind"] == "file":
            atomic_write(backup / path.relative_to(home), current["data"], current["mode"])
        manifest.append({"path": str(path), "component": group, "before": current["kind"], "restored": record["before"]})
        if record["before"] == "missing":
            path.unlink(missing_ok=True)
        elif record["before"] == "file":
            original = Path(record["backup"]) / path.relative_to(home)
            atomic_write(path, original.read_bytes(), record["mode"])
        else:
            raise RuntimeError(f"Unexpected original kind for {path}: {record['before']}")
    (backup / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    for group in groups:
        del state["components"][group]
    # Directories the swaps created go once they are empty (deepest first).
    kept = []
    for directory in sorted(state.get("dirs", []), key=lambda d: d.count("/"), reverse=True):
        try:
            Path(directory).rmdir()
        except FileNotFoundError:
            pass
        except OSError:
            kept.append(directory)
    state["dirs"] = sorted(kept)
    save_state(home, state)
    print(f"Rolled back {', '.join(groups)}. Backup of the Rust files: {backup}")
    return groups


# ---- status ---------------------------------------------------------------------------------

def proc_info(pid):
    base = Path("/proc") / str(pid)
    try:
        argv = (base / "cmdline").read_bytes().decode(errors="replace").rstrip("\0").split("\0")
        exe = os.readlink(base / "exe")
        rss = 0
        for line in (base / "status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                rss = int(line.split()[1])
        return {"pid": int(pid), "argv": argv, "exe": exe, "rss_kb": rss}
    except (OSError, ValueError):
        return None


def user_processes():
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                if entry.stat().st_uid != os.getuid():
                    continue
            except OSError:
                continue
            info = proc_info(entry.name)
            if info:
                yield info


def classify(info, home, component):
    binaries = {str(binary_path(home, component, name)) for name in component.binaries}
    entries = {str(entry_path(home, component, shim)) for shim in component.shims}
    exe = info["exe"].removesuffix(" (deleted)")
    argv = info["argv"]
    if exe in binaries or (argv and argv[0] in binaries):
        return "rust"
    if len(argv) > 1 and PYTHON.match(Path(argv[0]).name):
        script = next((arg for arg in argv[1:] if not arg.startswith("-")), None)   # python3 -u x.py
        if script in entries:
            return "python"
    return None


def unit_pid(unit):
    result = systemctl("show", unit, "-p", "MainPID", "--value")
    try:
        return int(result.stdout.strip() or 0)
    except ValueError:
        return 0


def component_status(home, component, processes, state):
    recorded = state["components"].get(component.key, {}).get("files", {})
    shims = {}
    for shim in component.shims:
        path = entry_path(home, component, shim)
        if path.is_symlink():
            shims[shim.entry] = "symlink"
        elif not path.is_file():
            shims[shim.entry] = "missing"
        elif is_shim(path):
            shims[shim.entry] = "rust"
        else:
            shims[shim.entry] = "python (shim overwritten)" if str(path) in recorded else "python"
    units = {}
    for unit in component.units:
        dropin = home / UNITS / (unit.name + ".d") / DROPIN
        pid = unit_pid(unit.name)
        info = proc_info(pid) if pid else None
        units[unit.name] = {"dropin": dropin.is_file(), "exec_start": effective_execstart(home, unit.name),
                            "main_pid": pid, "running": (classify(info, home, component) or "other") if info else None,
                            "rss_kb": info["rss_kb"] if info else 0}
    running = [dict(info, impl=impl) for info in processes for impl in [classify(info, home, component)] if impl]
    configured = list(shims.values()) + ["rust" if u["dropin"] else "python" for u in units.values()]
    if any(state == "python (shim overwritten)" for state in configured):
        mode = "python (shim overwritten)"
    elif all(state == "rust" for state in configured):
        mode = "rust"
    elif all(state in ("python", "missing") for state in configured):
        mode = "python" if "python" in configured else "not installed"
    else:
        mode = "mixed"
    warnings = guard_warnings(component) + list(component.notes) if mode != "python" and mode != "not installed" else []
    return {"component": component.key, "configured": mode, "shims": shims, "units": units,
            "binaries": {n: binary_path(home, component, n).is_file() for n in component.binaries},
            "processes": [{"pid": p["pid"], "impl": p["impl"], "rss_kb": p["rss_kb"]} for p in running],
            "rss_kb": sum(p["rss_kb"] for p in running), "warnings": warnings}


def status(home, components, as_json):
    processes = list(user_processes())
    state = load_state(home)
    report = [component_status(home, component, processes, state) for component in components]
    if as_json:
        print(json.dumps(report, indent=2))
        return report
    print(f"{'component':24}{'configured':28}{'running':16}{'RSS':>10}")
    total = 0
    for item in report:
        impls = sorted({p["impl"] for p in item["processes"]})
        running = ",".join(impls) + f" ({len(item['processes'])})" if impls else "-"
        total += item["rss_kb"]
        print(f"{item['component']:24}{item['configured']:28}{running:16}{item['rss_kb'] / 1024:>8.1f} MB")
        for name, unit in item["units"].items():
            print(f"    {name}: {'Rust drop-in' if unit['dropin'] else 'Python'}; ExecStart={unit['exec_start']}"
                  f"; MainPID={unit['main_pid'] or '-'}")
        for entry, shim_state in item["shims"].items():
            if shim_state != "rust" and item["configured"] != "python":
                print(f"    {entry}: {shim_state}")
        for warning in item["warnings"]:
            print(f"    warn: {warning}")
    print(f"{'total':68}{total / 1024:>8.1f} MB")
    return report


# ---- check (preview) ------------------------------------------------------------------------

def check(home, components, sources, locale_source):
    blocked = False
    for component in components:
        changes, problems = plan_component(component, home, sources)
        print(f"{component.key}  (binaries: {', '.join(component.binaries + component.helpers)} "
              f"-> {component_dir(home, component)})")
        for problem in problems:
            blocked = True
            print(f"  BLOCKED  {problem}")
        for change in changes:
            exists = "replace" if change.path.exists() or change.path.is_symlink() else "create "
            if change.path.is_symlink():
                blocked, exists = True, "REFUSE (symlink)"
            if change.data is None:
                exists = "missing"
            print(f"  {exists} {change.kind:8} {change.path}\n           {change.note}")
            if change.kind in ("dropin", "launcher") and change.data is not None:
                for line in change.data.decode().splitlines():
                    print(f"           | {line}")
        if not changes and not problems:
            print("  already swapped")
        missing = [k for k in component.keeps if not (component_dir(home, component) / k).exists()]
        print(f"  kept     {len(component.keeps) - len(missing)}/{len(component.keeps)} imported modules/helpers untouched"
              + (f" (missing now: {', '.join(missing)})" if missing else ""))
        for unit in component.units:
            print(f"  autostart unchanged: systemctl --user start {unit.name}")
        for note in component.notes:
            print(f"  note     {note}")
        for warning in guard_warnings(component):
            print(f"  warn     {warning}")
    changes, problems = plan_locale(home, locale_source)
    print(f"locale -> {home / LOCALE}: {len(changes)} catalogs to write" + "".join(f"\n  BLOCKED  {p}" for p in problems))
    print("Preview only. Run with --apply to swap; nothing is restarted without --restart.")
    return not blocked


# ---- main -----------------------------------------------------------------------------------

def selected(only):
    if not only:
        return list(COMPONENTS)
    keys = [key.strip() for key in only.split(",") if key.strip()]
    unknown = [key for key in keys if key not in BY_KEY]
    if unknown:
        raise SystemExit(f"Unknown component(s): {', '.join(unknown)}. Known: {', '.join(BY_KEY)}")
    return [BY_KEY[key] for key in keys]


def restart_units(components, home, allow_shell):
    for component in components:
        for unit in component.units:
            if unit.name in NEVER_RESTART or (unit.name in SHELL_UNITS and not allow_shell):
                print(f"Not restarting {unit.name}" + ("" if unit.name in NEVER_RESTART else " (needs --restart-shell)"))
                continue
            if systemctl("is-active", "--quiet", unit.name).returncode != 0:
                print(f"{unit.name} is not active; left stopped.")
                continue
            print(f"systemctl --user restart {unit.name}")
            systemctl("restart", unit.name, check=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--apply", action="store_true", help="swap the selected components to Rust")
    mode.add_argument("--rollback", action="store_true", help="restore the Python files (with --check: preview)")
    mode.add_argument("--status", action="store_true", help="show Rust/Python state and RSS")
    mode.add_argument("--list", action="store_true", help="print the component table")
    parser.add_argument("--check", action="store_true", help="preview only (default)")
    parser.add_argument("--only", help="comma-separated component keys")
    parser.add_argument("--prebuilt", type=Path, help="directory with already built binaries and helpers (skips cargo)")
    parser.add_argument("--no-build", action="store_true", help="use target/release as it is")
    parser.add_argument("--locale-source", type=Path, default=DEFAULT_LOCALE_SOURCE)
    parser.add_argument("--home", type=Path, default=Path.home(), help=argparse.SUPPRESS)
    parser.add_argument("--restart", action="store_true", help="systemctl --user restart the swapped active units")
    parser.add_argument("--restart-shell", action="store_true", help="also allow restarting waydock/wayshell")
    parser.add_argument("--no-reload", action="store_true", help="skip systemctl --user daemon-reload")
    parser.add_argument("--force", action="store_true", help="rollback: also restore files changed since the swap")
    parser.add_argument("--json", action="store_true", help="--status as JSON")
    args = parser.parse_args(argv)
    home = args.home.resolve()
    components = selected(args.only)
    prebuilt = args.prebuilt.resolve() if args.prebuilt else None

    if args.list:
        for c in COMPONENTS:
            print(f"{c.key}: package {c.package}, into {SCRIPTS}/{c.dir}/: {', '.join(c.binaries + c.helpers)}; "
                  f"units {', '.join(u.name for u in c.units) or '-'}; shims {', '.join(s.entry for s in c.shims)}")
        return 0
    if args.status:
        status(home, components, args.json)
        return 0
    if args.rollback:
        groups = rollback(home, [c.key for c in components], preview=args.check, force=args.force)
        if groups and not args.check:
            if not args.no_reload:
                systemctl("daemon-reload")
            if args.restart:
                restart_units([BY_KEY[g] for g in groups if g in BY_KEY], home, args.restart_shell)
        return 0
    if not args.apply or args.check:
        return 0 if check(home, components, resolve_sources(components, prebuilt), args.locale_source) else 1
    if not prebuilt and not args.no_build:
        build(components)
    sources = resolve_sources(components, prebuilt)     # after the build: helpers live in OUT_DIR
    changed = apply(home, components, sources, args.locale_source)
    if any(change.kind == "dropin" for change in changed) and not args.no_reload:
        systemctl("daemon-reload")
    if args.restart:
        restart_units(components, home, args.restart_shell)
    else:
        print("Running services were not restarted; use --restart (or restart them yourself) to run Rust.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as error:
        print(error, file=sys.stderr)
        sys.exit(1)
