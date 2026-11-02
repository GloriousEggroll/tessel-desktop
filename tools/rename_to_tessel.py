#!/usr/bin/python3 -I
"""One-time rename of the desktop suite to Tessel (directories, packages, binaries,
units, D-Bus names, paths and prose). Run from the repository root on a clean tree:

    python3 -I tools/rename_to_tessel.py [--dry-run]

Every token is replaced in a single regex pass, so a new name is never renamed again.
Kasumi (components/home-assistant-screen) is left alone until it is ported.
"""
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SKIP_DIRS = ("branding/", "target/")
SKIP_FILES = {"tools/rename_to_tessel.py"}

# Component and crate directories: old -> new.
DIRS = {
    "components/archive-manager": "components/archive",
    "components/audio-manager": "components/audio",
    "components/authentication-manager": "components/auth",
    "components/battery-manager": "components/battery",
    "components/bluetooth-manager": "components/bluetooth",
    "components/brightness-manager": "components/brightness",
    "components/calendar-manager": "components/calendar",
    "components/capture-manager": "components/capture",
    "components/clipboard-manager": "components/clipboard",
    "components/file-manager": "components/files",
    "components/global-settings": "components/settings",
    "components/language-manager": "components/language",
    "components/lock-screen": "components/lock",
    "components/login-manager": "components/login",
    "components/media-manager": "components/media",
    "components/network-manager": "components/network",
    "components/notification-manager": "components/notifications",
    "components/on-screen-keyboard": "components/keyboard",
    "components/picture-viewer": "components/pictures",
    "components/power-manager": "components/power",
    "components/session-manager": "components/session",
    "components/tiling-manager": "components/tiling",
    "components/way-displays": "components/displays",
    "components/waydock": "components/dock",
    "components/waykeyring": "components/keyring",
    "components/wayshell-capture-picker": "components/capture-picker",
    "components/wayshell-prompt": "components/prompt",
    "components/wayshell-theme": "components/theme",
    "components/wayshell": "components/shell",
    "components/weather-manager": "components/weather",
    "crates/suite-core": "crates/core",
    "crates/suite-gtk": "crates/gtk",
    "crates/suite-lock": "crates/locking",
    "crates/suite-power": "crates/power-core",
    "crates/suite-wayland": "crates/wayland",
}

# Exact D-Bus names that do not follow the plain namespace swap (applied before it).
DBUS = {
    "org.waydock.Waydock": "org.tessel.Dock",
    "/org/waydock/Waydock": "/org/tessel/Dock",
}

# systemd user units.
UNITS = {
    "netman.service": "tessel-network.service",
    "polkitman.service": "tessel-auth.service",
    "notifications.service": "tessel-notifications.service",
    "power-manager-lock.service": "tessel-lock.service",
    "power-manager.service": "tessel-power.service",
    "battery-manager.service": "tessel-battery.service",
    "brightness-manager.service": "tessel-brightness.service",
    "audio-manager-loopbacks.service": "tessel-audio-loopbacks.service",
    "keep-awake.service": "tessel-keep-awake.service",
    "on-screen-keyboard.service": "tessel-keyboard.service",
    "clipboard-manager.service": "tessel-clipboard.service",
    "waykeyring.service": "tessel-keyring.service",
    "waydock.service": "tessel-dock.service",
    "wayshell.service": "tessel-shell.service",
    "desktop-suite-failed@": "tessel-failed@",
    "night-light.service": "tessel-night-light.service",
}

# Kebab-case names: packages, binaries, helpers, CSS classes, namespaces, paths.
# Longest first within the alternation (the regex sorts them).
KEBAB = {
    # binaries and packages
    "wayarchive": "tessel-archive",
    "archive-manager": "tessel-archive",
    "archive-settings": "tessel-archive-settings",
    "audio-manager-loopbacks": "tessel-audio-loopbacks",
    "audio-manager": "tessel-audio",
    "authentication-manager": "tessel-auth",
    "battery-manager": "tessel-battery",
    "battery-status": "tessel-battery-status",
    "btmanager": "tessel-bluetooth",
    "bluetooth-manager": "tessel-bluetooth",
    "brightness-manager": "tessel-brightness",
    "brightness-status": "tessel-brightness-status",
    "calendar-manager": "tessel-calendar",
    "waycapture-portal-wlr": "tessel-capture-portal-wlr",
    "waycapture": "tessel-capture",
    "capture-manager": "tessel-capture",
    "wayclip": "tessel-clipboard",
    "clipboard-manager": "tessel-clipboard",
    "clipboard-settings": "tessel-clipboard-settings",
    "wayfiles": "tessel-files",
    "file-manager": "tessel-files",
    "files-model": "tessel-files-model",
    "files-ops": "tessel-files-ops",
    "files-widgets": "tessel-files-widgets",
    "global-settings": "tessel-settings",
    "hdr-calibration": "tessel-hdr-calibration",
    "keep-awake": "tessel-keep-awake",
    "language-manager": "tessel-language",
    "waylock-pam": "tessel-lock-pam",
    "waylock": "tessel-lock",
    "lock-screen": "tessel-lock",
    "login-settings-helper": "tessel-login-helper",
    "session-transition": "tessel-session-transition",
    "login-ui": "tessel-login",
    "login-manager": "tessel-login",
    "waylogin": "tessel-login",
    "media-manager": "tessel-media",
    "network-discovery": "tessel-network-discovery",
    "network-manager": "tessel-network",
    "network-status": "tessel-network-status",
    "notification-manager": "tessel-notifications",
    "notification-status": "tessel-notifications-status",
    "osk-manager": "tessel-keyboard",
    "osk-status": "tessel-keyboard-status",
    "on-screen-keyboard": "tessel-keyboard",
    "waykeys-input": "tessel-keyboard-input",
    "waykeys": "tessel-keyboard",
    "waypictures": "tessel-pictures",
    "picture-viewer": "tessel-pictures",
    "power-manager": "tessel-power",
    "power-status": "tessel-power-status",
    "session-manager": "tessel-session",
    "session-startup": "tessel-session-startup",
    "configure-session": "tessel-configure-session",
    "configure-xdg-autostart": "tessel-xdg-autostart",
    "desktop-suite-failed": "tessel-failed",
    "desktop-suite-icon-theme": "tessel-icon-theme",
    "desktop-suite-welcome": "tessel-welcome",
    "desktop-suite-action": "tessel-action",
    "system-monitor": "tessel-system-monitor",
    "text-editor": "tessel-text-editor",
    "waytile": "tessel-tiling",
    "tiling-manager": "tessel-tiling",
    "tiling-status": "tessel-tiling-status",
    "way-displays-gui": "tessel-displays",
    "waydock-toplevel": "tessel-dock-toplevel",
    "waydock": "tessel-dock",
    "pam-waykeyring": "pam-tessel-keyring",
    "waykeyring-prompt": "tessel-keyring-prompt",
    "waykeyring-ui": "tessel-keyring-ui",
    "waykeyring": "tessel-keyring",
    "wayshell-capture-picker": "tessel-capture-picker",
    "wayshell-install-prompts": "tessel-install-prompts",
    "wayshell-prompt": "tessel-prompt",
    "wayshell-theme": "tessel-theme",
    "wayshell-overview": "tessel-overview",
    "wayshell-cli": "tessel-shell-cli",
    "wayshell": "tessel-shell",
    "weather-manager": "tessel-weather",
    "weather-status": "tessel-weather-status",
    "suite-core": "tessel-core",
    "suite-gtk": "tessel-gtk",
    "suite-lock": "tessel-locking",
    "suite-power": "tessel-power-core",
    "suite-wayland": "tessel-wayland",
    "desktop-suite": "tessel",
}

# Prose and type-name spellings.
TITLE = {
    "Waydock": "Tessel Dock",
    "Wayshell": "Tessel Shell",
    "Waylogin": "Tessel Login",
    "Waykeys": "Tessel Keyboard",
    "Waytile": "Tessel Tiling",
    "Wayfiles": "Tessel Files",
    "DesktopSuite": "Tessel",
    "the desktop suite's": "Tessel's",
    "The desktop suite's": "Tessel's",
    "the desktop suite": "Tessel",
    "The desktop suite": "Tessel",
    "Desktop Suite": "Tessel",
    "Desktop suite": "Tessel",
    "desktop suite": "Tessel",
}

# Names whose words are also ordinary words elsewhere ("accessories-text-editor",
# "network-manager-applet"): replaced only when no letter, digit, "-" or "_" touches them.
STRICT = {"archive-manager", "archive-settings", "audio-manager-loopbacks", "audio-manager",
          "authentication-manager", "battery-manager", "battery-status", "bluetooth-manager",
          "brightness-manager", "brightness-status", "calendar-manager", "capture-manager",
          "clipboard-manager", "clipboard-settings", "file-manager", "files-model", "files-ops",
          "files-widgets", "global-settings", "hdr-calibration", "keep-awake", "language-manager",
          "lock-screen", "login-settings-helper", "session-transition", "login-ui", "login-manager",
          "media-manager", "network-discovery", "network-manager", "network-status",
          "notification-manager", "notification-status", "osk-manager", "osk-status",
          "on-screen-keyboard", "picture-viewer", "power-manager", "power-status", "session-manager",
          "session-startup", "configure-session", "configure-xdg-autostart", "system-monitor",
          "text-editor", "tiling-manager", "tiling-status", "weather-manager", "weather-status"}

# Component directories that keep their name (so their paths are not rewritten as names).
KEPT_DIRS = ("calculator", "hdr-calibration", "keep-awake", "network-discovery", "session-startup",
             "system-monitor", "text-editor", "welcome", "home-assistant-screen")


def snake(name):
    return name.replace("-", "_")


def build():
    table = {}            # old -> (new, kind): plain | loose | strict | snake
    for old in KEPT_DIRS:
        table[f"components/{old}"] = (f"components/{old}", "plain")
    for old, new in DIRS.items():
        table[old] = (new, "plain")
    for old, new in UNITS.items():
        table[old] = (new, "plain")
    for old, new in KEBAB.items():
        table[old] = (new, "strict" if old in STRICT else "loose")
        if snake(old) != old:
            table[snake(old)] = (snake(new), "snake")
    for old, new in TITLE.items():
        table[old] = (new, "loose")
    table["desktop_suite"] = ("tessel", "snake")
    keys = sorted(table, key=len, reverse=True)
    pattern = re.compile(r"(?<![A-Za-z0-9])(" + "|".join(re.escape(k) for k in keys) + r")(?![A-Za-z0-9])")
    return table, pattern


TABLE, PATTERN = build()
# D-Bus namespaces: prefixes, so they are matched without a right boundary (first pass).
PREFIXES = dict(DBUS)
for _ns in ("waydock", "waybar", "wayshell"):
    PREFIXES.update({f"org.{_ns}.": "org.tessel.", f"/org/{_ns}/": "/org/tessel/", f"org/{_ns}/": "org/tessel/",
                     f"org_{_ns}_": "org_tessel_"})
PREFIX_PATTERN = re.compile(r"(?<![A-Za-z0-9])(" + "|".join(re.escape(k) for k in PREFIXES) + ")")
# Relative Cargo paths ("../../waydock"): each segment that is an old directory name.
SEGMENTS = {old.rsplit("/", 1)[1]: new.rsplit("/", 1)[1] for old, new in DIRS.items()}


def cargo_path(match):
    value = match.group(2)
    if value.startswith(("src/", "tests/", "benches/", "examples/")):
        head, _, name = value.rpartition("/")
        return f'{match.group(1)}"{head}/{rewrite(name, "text")}"'
    parts = [SEGMENTS.get(part, part) for part in value.split("/")]
    if "." in parts[-1]:
        parts[-1] = rewrite(parts[-1], "text")   # a moved file keeps up with its new name
    return f'{match.group(1)}"{"/".join(parts)}"'


def is_code(text, position):
    """Whether `position` in Rust source is outside a string literal and a // comment
    (a line-local heuristic: raw and multi-line strings count as code)."""
    line_start = text.rfind("\n", 0, position) + 1
    line = text[line_start:position]
    escaped, in_string = False, False
    for index, char in enumerate(line):
        if escaped:
            escaped = False
            continue
        if char == "\\" and in_string:
            escaped = True
        elif char == '"':
            in_string = not in_string
        elif char == "/" and not in_string and line[index + 1:index + 2] == "/":
            return False
    return not in_string


def rewrite(text, kind_of_file):
    rust, cargo = kind_of_file == "rs", kind_of_file == "cargo"
    # The compositor's blur protocol (labwc patch) keeps its wire names.
    kept = []
    def keep(match):
        kept.append(match.group(0))
        return f"\1{len(kept) - 1}\1"
    text = re.sub(r"waydock[_-]blur[A-Za-z0-9_.-]*", keep, text)
    text = PREFIX_PATTERN.sub(lambda m: PREFIXES[m.group(1)], text)
    # Cargo's per-binary environment variables (CARGO_BIN_EXE_<bin>)
    text = re.sub(r"(CARGO_BIN_EXE_)([A-Za-z0-9-]+)",
                  lambda m: m.group(1) + PATTERN.sub(lambda n: TABLE[n.group(1)][0], m.group(2)), text)
    saved = []
    # Path values are rewritten by directory segment only, untouched by the name pass.
    def hold(match):
        saved.append(cargo_path(match))
        return f"\0{len(saved) - 1}\0"
    if cargo:
        text = re.sub(r'(path\s*=\s*)"([^"]+)"', hold, text)
    elif rust:
        # #[path = "../../media-manager/…"], include_str!("../…")
        text = re.sub(r'(#\[path\s*=\s*|include_(?:str|bytes)!\()"((?:\.\./)+[^"]+)"', hold, text)

    def replace(match):
        old = match.group(1)
        new, kind = TABLE[old]
        start, end = match.start(), match.end()
        before, after = text[start - 1:start] if start else "", text[end:end + 1]
        if kind == "strict" and (before in "-_" and before or after in "-_" and after):
            return old
        crate_context = text[end:end + 2] == "::" or text[max(0, start - 4):start] in ("use ", "::") \
            or text[max(0, start - 13):start] == "extern crate "
        if kind == "snake":
            return new if cargo or (rust and crate_context) else old
        identifier = after == "_" or before == "_" or (before == "{" and text[end:end + 1] in "}:")
        if rust and kind in ("loose", "strict") and "-" in new and (crate_context or identifier
                                                                 or is_code(text, start)):
            return snake(new)
        return new
    text = PATTERN.sub(replace, text)
    text = re.sub("\0([0-9]+)\0", lambda m: saved[int(m.group(1))], text)
    text = re.sub("\1([0-9]+)\1", lambda m: kept[int(m.group(1))], text)
    if cargo:
        # library target names are identifiers
        text = re.sub(r'(\[lib\][^\[]*?\bname\s*=\s*")([^"]+)"', lambda m: m.group(1) + snake(m.group(2)) + '"', text,
                      flags=re.S)
    return text


def tracked():
    out = subprocess.run(["git", "ls-files", "-z"], cwd=REPO, capture_output=True, check=True).stdout
    return [p for p in out.decode().split("\0") if p]


def renamed_path(path):
    parts = path
    for old, new in sorted(DIRS.items(), key=lambda kv: len(kv[0]), reverse=True):
        if parts == old or parts.startswith(old + "/"):
            parts = new + parts[len(old):]
            break
    head, _, name = parts.rpartition("/")
    new_name = rewrite(name, "text")
    return (head + "/" if head else "") + new_name


def main():
    dry = "--dry-run" in sys.argv
    files = [p for p in tracked() if not p.startswith(SKIP_DIRS) and p not in SKIP_FILES]
    changed = 0
    for path in files:
        full = REPO / path
        if full.is_symlink() or not full.is_file():
            continue
        data = full.read_bytes()
        if b"\0" in data[:8192]:
            continue
        try:
            text = data.decode()
        except UnicodeDecodeError:
            continue
        kind = "rs" if path.endswith(".rs") else "cargo" if path.endswith("Cargo.toml") else "text"
        new = rewrite(text, kind)
        if new != text:
            changed += 1
            if not dry:
                full.write_text(new)
    moves = []
    for path in files:
        target = renamed_path(path)
        if target != path:
            moves.append((path, target))
    print(f"{changed} files rewritten, {len(moves)} files moved")
    if dry:
        for old, new in moves[:40]:
            print(f"  {old} -> {new}")
        return
    for old, new in moves:
        (REPO / new).parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "mv", old, new], cwd=REPO, check=True)
    for old in DIRS:
        directory = REPO / old
        if directory.is_dir() and not any(p.is_file() or p.is_symlink() for p in directory.rglob("*")):
            subprocess.run(["rm", "-r", str(directory)], check=True)


if __name__ == "__main__":
    main()
