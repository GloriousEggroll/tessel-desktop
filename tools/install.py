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
  * repoints labwc keybinds that ran a Python entry (wayshell: A-Tab / A-S-Tab -> wayshell-cli) in
    ~/.config/labwc/rc.xml, changing only those command attributes (every other byte is kept);
    --rollback restores the backup, or, if rc.xml changed since (Global Settings saves there), puts
    back only the original command strings,
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
--restart-shell), followed by the component's health check (waydock --check, wayshell-cli
switcher-status). When rc.xml keybinds changed, `labwc --reconfigure` (a reload, never a restart)
runs only with --restart --restart-shell inside the labwc session; otherwise run it yourself.
--apply/--rollback run `systemctl --user daemon-reload` so systemd sees the drop-in change (pass
--no-reload to skip it).

System-wide entries (--system): the standalone apps desktop-apps/install.py puts in /usr/local
(calculator, text-editor, picture-viewer, system-monitor, capture-manager, file-manager), the new
archive-manager (a fresh install: there is no Python twin, it replaces KDE's Ark) and the
Waylogin greeter (login-manager, /usr/local/lib/waylogin). They are a separate mode:
  install.py --system [--check] [--only a,b]          preview (works as your user)
  pkexec /usr/bin/python3 install.py --system --apply --only calculator
  pkexec /usr/bin/python3 install.py --system --rollback [--only a,b]
--system --apply/--rollback refuse unless the process already runs as root; this tool never runs
sudo/pkexec itself and never runs cargo as root (build as your user first). The binary goes next to
the Python files in /usr/local/lib/desktop-suite/<app>/ (Files: .../file-manager/wayfiles, a stable
path), the catalogs into <that dir>/locale, and only the `exec` line of the /usr/local/bin launcher
changes (the "# Desktop suite standalone application" marker line stays; a "desktop-suite-rust shim"
line is added, so desktop-apps/install.py and file-manager/install.py keep the Rust launcher).
login-manager changes only start-greeter's `--session` argument. State and backups live in
/var/lib/desktop-suite/rust-swap.json and /var/backups/desktop-suite/rust-swap-*/.

archive-manager-mime (user scope, only with --only archive-manager-mime) sets the [Default Applications]
of ~/.config/mimeapps.list for every archive type to org.waydock.ArchiveManager.desktop; --rollback --only
archive-manager-mime puts back the previous values of the keys that still point at Archive Manager.

home-assistant-screen targets the Kasumi device (aarch64): it is planned and installed only into an
explicit --target-root (the Kasumi user's home as mounted or staged here, never this machine's home),
from aarch64 binaries, and never runs systemctl.
"""
import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import html
import json
import os
from pathlib import Path
import pwd
import re
import shlex
import signal
import stat
import subprocess
import sys
import tempfile
import time
from xml.sax.saxutils import escape as xml_escape
import xml.etree.ElementTree as ET

REPO = Path(__file__).resolve().parent.parent
REAL_HOME = Path(pwd.getpwuid(os.getuid()).pw_dir)


def invoking_home():
    """The home of the user who ran pkexec/sudo (root's own home is not where ~/btmanager lives)."""
    if os.getuid() == 0:
        for name in ("PKEXEC_UID", "SUDO_UID"):
            value = os.environ.get(name, "")
            if value.isdigit():
                try:
                    return Path(pwd.getpwuid(int(value)).pw_dir)
                except KeyError:
                    pass
    return REAL_HOME


INVOKING_HOME = invoking_home()
DEFAULT_LOCALE_SOURCE = INVOKING_HOME / "btmanager/shared/locale"

SCRIPTS = ".config/waybar/scripts"
LOCALE = ".local/share/desktop-suite/locale"
UNITS = ".config/systemd/user"
DROPIN = "zz-desktop-suite-rust.conf"
STATE = ".local/state/desktop-suite/rust-swap.json"
BACKUPS = ".local/state/desktop-suite/backups"
SHIM_MARKER = "desktop-suite-rust shim"
RC_XML = ".config/labwc/rc.xml"
SHARED = "_shared"            # state group for files shared by every component (locale)
SHELL_UNITS = {"waydock.service", "wayshell.service"}
# power-manager-lock.service runs only while the screen is locked: the next lock runs the new binary.
NEVER_RESTART = {"labwc.service", "graphical-session.target", "power-manager-lock.service"}
PYTHON = re.compile(r"^python3(?:\.\d+)?$")
# --system: paths relative to the root prefix ("/"; tests inject a private prefix with --root).
SYSTEM_STATE = "var/lib/desktop-suite/rust-swap.json"
SYSTEM_BACKUPS = "var/backups/desktop-suite"
SYSTEM_LIB = "usr/local/lib/desktop-suite"
SYSTEM_BIN = "usr/local/bin"
STANDALONE_MARKER = "# Desktop suite standalone application"     # desktop-apps/install.py WRAPPER_MARKER
EM_AARCH64 = 183
AARCH64_RELEASE = "target/aarch64-unknown-linux-gnu/release"


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
    args: tuple = None        # None: keep the flags the current (Python) ExecStart passes


@dataclass(frozen=True)
class Keybind:
    key: str                  # labwc keybind key ("A-Tab")
    entry: str                # Python script (in the component dir) the Execute command ran with python3
    binary: str               # Rust binary (same dir) that now runs instead
    args: tuple = ()          # the arguments after the script; only commands with exactly these are changed


@dataclass(frozen=True)
class TextEdit:
    path: str                 # relative to the base (HOME or the root prefix)
    old: str                  # exact text (once in the file) the Python install has; "{base}" = the prefix
    new: str                  # what replaces it; nothing else in the file changes


@dataclass(frozen=True)
class MimeDefaults:
    path: str                 # home-relative mimeapps.list
    desktop: str              # desktop id every listed mime type defaults to
    mimes: tuple              # the mime types ([Default Applications] keys)


@dataclass(frozen=True)
class Component:
    key: str
    package: str              # cargo package (components/<package>)
    dir: str                  # <base>/<dir>: binaries, helpers and shims live here
    binaries: tuple           # [[bin]] names of the package (or (cargo name, installed name)), verified by tests
    shims: tuple
    units: tuple = ()
    helpers: tuple = ()       # non-cargo executables from the crate's build output (waykeys-input)
    launchers: tuple = ()     # ~/.local/bin wrappers that exec the Python entry (shims[0])
    desktop: tuple = ()       # .desktop files whose Exec runs the Python entry (shims[0])
    keeps: tuple = ()         # files others import or the binary needs beside it: never touched
    clobbered_by: tuple = ()  # Python installers that rewrite a shimmed entry (besides install_gtk4)
    notes: tuple = ()
    base: str = SCRIPTS       # home-relative parent of dir
    optional: bool = False    # skipped (not blocked) when the Python install is absent
    requires_confirmation: bool = False   # --apply needs --i-am-at-the-keyboard
    signal_on_restart: bool = False       # resident without a unit: --restart SIGTERMs it
    requires_rpms: tuple = ()             # runtime packages the binary loads (checked with rpm -q)
    keybinds: tuple = ()                  # rc.xml Execute commands repointed from the Python entry to a binary
    python_entries: tuple = ()            # Python programs (kept, not shimmed) that --status recognises
    health: tuple = ()                    # argv (binary in dir + args) that must exit 0 after a restart
    scope: str = "user"                   # user (HOME) | system (--system, root) | remote (--target-root)
    locale_dir: str = None                # catalogs into <dir>/<locale_dir> (system entries: <exe dir>/locale)
    edits: tuple = ()                     # TextEdit: one exact string replaced in an installed file
    arch: str = None                      # "aarch64": ELF binaries must be built for it (Kasumi)
    refuse_while_active: tuple = ()       # units that must be inactive for --apply (the locker)
    warning: str = None                   # printed by --check and after --apply
    fresh: bool = False                   # no Python original: launcher/files are created, --rollback removes them
    files: tuple = ()                     # (path relative to the base, repo source, mode) installed verbatim
    mime_defaults: tuple = ()             # MimeDefaults: only these keys of [Default Applications] change
    extra_packages: tuple = ()            # more cargo packages built for `files` (waykeyring-ui, pam-waykeyring)
    uses_locale: bool = True              # user entries: needs the shared catalogs in ~/.local/share/desktop-suite
    explicit: bool = False                # only with --only <key> (never part of "every component")


POPUP = ("--popup", "--popup-edge", "--popup-offset")
ARCHIVE_DESKTOP = "org.waydock.ArchiveManager.desktop"
ARCHIVE_DESKTOP_SOURCE = "components/archive-manager/data/" + ARCHIVE_DESKTOP


def desktop_mimes(relative):
    """The MimeType= list of a desktop file in the repository (the single source of the archive types)."""
    for line in (REPO / relative).read_text().splitlines():
        if line.startswith("MimeType="):
            return tuple(m for m in line[len("MimeType="):].split(";") if m)
    return ()


ARCHIVE_MIMES = desktop_mimes(ARCHIVE_DESKTOP_SOURCE)
APPS = ".local/share/applications"
# Python installers that copy every component *.py back, so they overwrite every shim.
CLOBBER_ALL = ("shared/install_gtk4.py", "install_all.py")
# Python installers that recognise a running waydock child by its *_status.py argv: with the Rust
# status streams they see "an unrelated application" and refuse to restart waydock.service.
STATUS_GUARD = ("shared/install_gtk4.py recognises waydock's status children and standalone apps by their .py "
                "argv or, for swapped components, by RUST_TWINS (script -> binary beside it); without an entry "
                "there, Python installers that restart waydock.service refuse with 'still owns an unrelated "
                "application' or miss a running instance.")

COMPONENTS = (
    Component(
        "keep-awake", "keep-awake", "keep-awake", ("keep-awake",),
        shims=(Shim("keep_awake.py", "keep-awake",
                    ("--background", *POPUP, "--status", "--enable", "--disable", "--toggle"),
                    porter="components/keep-awake/shim/keep_awake.py", fallback="keep_awake_python.py"),),
        units=(Unit("keep-awake.service", "keep-awake", ("--background",)),),
        keeps=("keep_awake_ui.py", "keep_awake_client.py", "keep_awake_backend.py"),
        notes=("idle_inhibit.sh / toggle_idle_inhibit.sh and waydock run keep_awake.py: served by the shim.",
               "wayshell/install_switcher.py leaves a keep_awake.py that carries the shim marker alone.",
               "The porter shim falls back to keep_awake_python.py (the saved Python entry) if the binary is gone.")),
    Component(
        "battery-manager", "battery-manager", "battery-manager", ("battery-manager", "battery-status"),
        shims=(Shim("battery_manager.py", "battery-manager", (*POPUP, "--settings", "--background", "--check"),
                    porter="components/battery-manager/shims/battery_manager.py"),
               Shim("battery_status.py", "battery-status",
                    porter="components/battery-manager/shims/battery_status.py")),
        units=(Unit("battery-manager.service", "battery-manager", ("--background",)),),
        desktop=(f"{APPS}/org.waybar.BatteryManager.desktop",),
        keeps=("battery_ui.py",),
        clobbered_by=("battery-manager/install_waybar.py",),
        notes=("The Rust binary opens Bluetooth via ../btmanager/btmanager.py (Python or its shim).", STATUS_GUARD)),
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
        notes=("install_adjustment_osd audio_owner and install_waybar --update find a running audio_manager.py "
               "by its argv: they will not see the Rust instance.", STATUS_GUARD)),
    Component(
        "brightness-manager", "brightness-manager", "brightness-manager", ("brightness-manager", "brightness-status"),
        shims=(Shim("brightness_manager.py", "brightness-manager",
                    (*POPUP, "--settings", "--background", "--check", "--brightness-up", "--brightness-down",
                     "--idle-dim", "--idle-restore"), porter="components/brightness-manager/shim/brightness_manager.py"),
               Shim("brightness_status.py", "brightness-status",
                    porter="components/brightness-manager/shim/brightness_status.py")),
        units=(Unit("brightness-manager.service", "brightness-manager", ("--background",)),),
        keeps=("brightness_client.py", "brightness_backend.py"),
        clobbered_by=("brightness-manager/install_waybar.py", "shared/install_adjustment_osd.py",
                      "shared/install_keyboard_osd.py"),
        notes=("The base unit (TimeoutStopSec=60) is not edited; the drop-in only resets ExecStart.",
               "way-displays-gui keeps loading brightness_client.py.", STATUS_GUARD)),
    Component(
        "authentication-manager", "authentication-manager", "authentication-manager", ("authentication-manager",),
        shims=(Shim("authentication_manager.py", "authentication-manager",
                    porter="components/authentication-manager/shim/authentication_manager.py"),),
        units=(Unit("polkitman.service", "authentication-manager", ()),),
        keeps=("bluetooth_agent.py",),
        base=".local/share", requires_confirmation=True,
        notes=("polkit agent: --apply refuses without --i-am-at-the-keyboard; test a password prompt "
               "(e.g. pkexec true) right after restarting polkitman.service.",)),
    Component(
        "calendar-manager", "calendar-manager", "calendar-manager", ("calendar-manager",),
        shims=(Shim("calendar_manager.py", "calendar-manager", (*POPUP, "--settings"),
                    porter="components/calendar-manager/shim/calendar_manager.py"),),
        keeps=("date_settings.py", "date_backend.py"),
        clobbered_by=("calendar-manager/install_waybar.py",),
        signal_on_restart=True,
        notes=("No unit; resident after the first popup: --restart SIGTERMs the running instance.", STATUS_GUARD)),
    Component(
        "weather-manager", "weather-manager", "weather-manager", ("weather-manager", "weather-status"),
        shims=(Shim("weather_manager.py", "weather-manager", (*POPUP, "--settings", "--tray"),
                    porter="components/weather-manager/shim/weather_manager.py"),
               Shim("weather_status.py", "weather-status", ("--watch", "--config", "--cache-dir"),
                    porter="components/weather-manager/shim/weather_status.py")),
        keeps=("weather_settings.py", "weather_backend.py"),
        notes=("lock-screen's lock_weather.py keeps importing weather_backend.", STATUS_GUARD)),
    Component(
        "weather-manager-ha", "weather-manager", "weather-manager", ("weather-manager",),
        shims=(Shim("weather_manager.py", "weather-manager", (*POPUP, "--settings", "--tray"),
                    porter="components/weather-manager/shim/weather_manager.py"),),
        units=(Unit("weather-manager.service", "weather-manager"),),
        keeps=("weather_backend.py",),
        base=".local/share", optional=True,
        clobbered_by=("home-assistant-screen/install_remote.py",),
        notes=("Home Assistant tray install; skipped while ~/.local/share/weather-manager does not exist.",)),
    Component(
        "media-manager", "media-manager", "media-manager", ("media-manager",),
        shims=(Shim("media_manager.py", "media-manager", POPUP,
                    porter="components/media-manager/shim/media_manager.py"),),
        launchers=((".local/bin/waymedia", "components/media-manager/shim/waymedia"),),
        keeps=("lock_media.py",),
        clobbered_by=("media-manager/install.py", "shared/install_popup_fixes.py")),
    Component(
        "tiling-manager", "tiling-manager", "tiling-manager", ("waytile", "tiling-status"),
        shims=(Shim("tiling_manager.py", "waytile",
                    ("--toggle", "--enable", "--disable", "--restore", "--status", "--watch", "--settings", *POPUP),
                    porter="components/tiling-manager/shims/tiling_manager.py", fallback="tiling_manager_python.py"),
               Shim("tiling_status.py", "tiling-status", porter="components/tiling-manager/shims/tiling_status.py",
                    fallback="tiling_status_python.py")),
        launchers=(".local/bin/waytile",),
        keeps=("tiling_settings.py", "tiling_config.py", "tiling_client.py", "bindings.py"),
        clobbered_by=("tiling-manager/install.py",),
        notes=("Global Settings and wayshell keep importing tiling_settings/tiling_config/tiling_client/bindings.",)),
    Component(
        "network-discovery", "network-discovery", "network-discovery", ("network-discovery",),
        shims=(Shim("discovery_manager.py", "network-discovery", ("--toggle", "--status", "--settings", *POPUP),
                    porter="components/network-discovery/shims/discovery_manager.py",
                    fallback="discovery_manager_python.py"),),
        keeps=("LICENSE", "discovery_backend.py", "~/.local/share/waydock/discovery_backend.py"),
        clobbered_by=("network-discovery/install.py",),
        notes=("~/.local/bin/network-discovery is left as is: it runs the shim.",
               "Waydock runs discovery_backend.py in-process from ~/.local/share/waydock.")),
    Component(
        "session-manager", "session-manager", "session-manager", ("session-manager",),
        shims=(Shim("session_manager.py", "session-manager", ("--popup", "--menu", "--popup-edge", "--popup-offset"),
                    porter="components/session-manager/shim/session_manager.py"),),
        keeps=("session_backend.py", "session_end.py"),
        clobbered_by=("shared/install_popup_fixes.py",),
        notes=("desktop-suite-action enables the centered menu only while session_manager.py contains '--menu'.",)),
    Component(
        "way-displays", "way-displays", "way-displays", (("way-displays-gui", "way-displays-gui-rs"),),
        shims=(),
        launchers=((".local/bin/way-displays-gui", "components/way-displays/shim/way-displays-gui-launcher",
                    ".local/share/way-displays/way-displays-gui"),),
        keeps=("way-displays-gui", "color_config.py", "color_controls.py"),
        base=".local/share",
        clobbered_by=("way-displays/install_color.py", "brightness-manager/install_waybar.py", "shared/install_gtk4.py"),
        notes=("Global Settings embeds the Python way-displays-gui module, which stays installed.",
               "way-displays.service runs the system /usr/bin/way-displays daemon and is not changed.")),
    Component(
        "language-manager", "language-manager", "language-manager", ("language-manager",),
        shims=(Shim("language_manager.py", "language-manager", ("--status", "--quit"),
                    porter="components/language-manager/shim/language_manager.py",
                    fallback="language_manager_python.py"),),
        keeps=("language_backend.py", "language_ui.py", "language_style.py"),
        base=".local/share",
        notes=("~/.local/bin/language-manager opens Global Settings and is not changed.", STATUS_GUARD)),
    Component(
        "session-startup", "session-startup", "desktop-suite/session", ("desktop-suite-failed", "desktop-suite-icon-theme"),
        shims=(Shim("desktop_suite_failed.py", "desktop-suite-failed",
                    porter="components/session-startup/shim/desktop_suite_failed.py"),),
        keeps=("suite_i18n.py", "language_data.py", "suite_catalogs.py"),
        base=".local/share", optional=True,
        clobbered_by=("session-startup/configure_session.py",),
        notes=("Crash helper only. desktop-suite-failed@.service keeps running python3 desktop_suite_failed.py %i.",
               "configure_session.py stays Python (installers import it) and is never touched.",
               "desktop-suite-icon-theme falls back to Papirus-Dark at login when the configured icon theme is not "
               "installed; packaging/defaults/etc/xdg/autostart/desktop-suite-icon-theme.desktop runs it.")),
    Component(
        "bluetooth-manager", "bluetooth-manager", "btmanager", ("btmanager",),
        shims=(Shim("btmanager.py", "btmanager",
                    (*POPUP, "--lang", "--selftest", "--gen-pot", "--gen-mo"),
                    porter="components/bluetooth-manager/shim/btmanager.py", fallback="btmanager_gtk.py"),),
        keeps=("btpopup.py", "bttransfer.py", "translations.py"),
        notes=("btmanager.py stays importable: imported, the shim loads btmanager_gtk.py (the original), "
               "so Global Settings' embedded Bluetooth page and battery-manager keep working.", STATUS_GUARD)),
    Component(
        "network-manager", "network-manager", "network-manager", ("network-manager", "network-status"),
        shims=(Shim("network_manager.py", "network-manager", (*POPUP, "--settings", "--background", "--check"),
                    porter="components/network-manager/shim/network_manager.py", fallback="network_manager_impl.py"),
               Shim("network_status.py", "network-status",
                    porter="components/network-manager/shim/network_status.py", fallback="network_status_impl.py")),
        units=(Unit("netman.service", "network-manager", ("--background",)),),
        desktop=(f"{APPS}/org.waybar.NetworkManager.desktop",),
        keeps=("network_backend.py", "network_popup.py", "network_settings.py", "network_preferences.py",
               "network_style.py", "secret_agent.py"),
        clobbered_by=("network-manager/install_waybar.py",),
        requires_confirmation=True, requires_rpms=("NetworkManager-libnm",),
        notes=("NetworkManager secret agent: --apply needs --i-am-at-the-keyboard; check Wi-Fi/VPN prompts "
               "right after restarting netman.service.",
               "Imported, network_manager.py / network_status.py re-export network_manager_impl / "
               "network_status_impl (the saved originals) for Global Settings.", STATUS_GUARD)),
    # wayshell.service's own ExecStart (/usr/bin/wayshell) is stale: wayshell.service.d/palette.conf
    # resets it to %h/.local/share/wayshell/wayshell, and that is what runs. The swap does NOT edit
    # palette.conf: like every other unit here it adds zz-desktop-suite-rust.conf, which systemd reads
    # after palette.conf ("p" < "z"), so its `ExecStart=` reset + binary is the effective command.
    # That keeps one mechanism for all units, leaves palette.conf byte-identical (nothing to back up
    # or merge on rollback, which just removes our file), and survives anything that rewrites
    # palette.conf later. The Python daemon `wayshell` stays installed for --rollback, so the binary is
    # installed as wayshell-rs next to it.
    Component(
        "wayshell", "wayshell", "wayshell", (("wayshell", "wayshell-rs"), "wayshell-cli"),
        shims=(Shim("wayshell_cli.py", "wayshell-cli", ("--reverse", "--monitor"),
                    porter="components/wayshell/shim/wayshell_cli.py"),),
        units=(Unit("wayshell.service", "wayshell-rs", ()),),
        keybinds=(Keybind("A-Tab", "wayshell_cli.py", "wayshell-cli", ("switch-window",)),
                  Keybind("A-S-Tab", "wayshell_cli.py", "wayshell-cli", ("switch-window", "--reverse"))),
        keeps=("wayshell", "wayshell_switcher.py", "wayshell_switcher_ui.py", "wayshell_theme.py", "switcher_model.py",
               "overview.py", "wayshell-overview", "wayshell-toplevel", "window_backend.py", "app_catalog.py",
               "companion_theme.py", "wallpaper_chooser.py", "desktop_file_ops.py"),
        python_entries=("wayshell",),
        health=("wayshell-cli", "switcher-status"),
        base=".local/share", requires_confirmation=True,
        notes=("wayshell.service runs palette.conf's ExecStart (the base unit's is stale); zz- sorts after it, "
               "palette.conf and 50-desktop-suite-recovery.conf stay untouched.",
               "rc.xml A-Tab / A-S-Tab run ~/.local/share/wayshell/wayshell-cli directly (no Python startup); "
               "labwc reads them after `labwc --reconfigure` (only with --restart --restart-shell).",
               "wayshell_cli.py becomes the porter shim: waydock hot corners run it, wayshell_switcher.py imports "
               "monitor_name; desktop-suite-action greps no wayshell file.",
               "Restarts the desktop/switcher: --apply needs --i-am-at-the-keyboard.",
               "btmanager installers skip these entries while the swap is active (install_switcher.py: A-Tab "
               "keybinds and shims; install_all.py: the prompt / capture picker / waydock steps).")),
    Component(
        "wayshell-prompt", "wayshell-prompt", "wayshell", ("wayshell-prompt",),
        shims=(),
        launchers=((".local/bin/wayshell-prompt", None, ".local/share/wayshell/wayshell_prompt.py"),),
        keeps=("wayshell_prompt.py", "wayshell_theme.py"),
        base=".local/share",
        clobbered_by=("wayshell/install_prompts.py",),
        notes=("rc.xml promptCommand/errorCommand keep calling ~/.local/bin/wayshell-prompt (same here-doc "
               "delimiter); only the launcher changes. wayshell_prompt.py stays Python (nothing runs it directly).",)),
    Component(
        "wayshell-capture-picker", "wayshell-capture-picker", "wayshell", ("wayshell-capture-picker",),
        shims=(),
        launchers=((".local/bin/wayshell-capture-picker", None, ".local/share/wayshell/capture_picker.py"),),
        keeps=("capture_picker.py", "capture_picker_ui.py", "capture_sources.py", "capture_output.py",
               "capture_selection.py"),
        base=".local/share",
        clobbered_by=("wayshell/install_capture_picker.py",),
        notes=("xdg-desktop-portal-wlr's chooser_cmd keeps running ~/.local/bin/wayshell-capture-picker; "
               "the picker finds ~/.local/share/waydock/waydock-toplevel for window icons.",)),
    Component(
        "waydock", "waydock", "waydock", ("waydock",),
        shims=(),
        units=(Unit("waydock.service", "waydock", ()),),
        launchers=((".local/bin/waydock", None, ".local/share/waydock/waydock.py"),),
        keeps=("waydock.py", "companion_theme.py", "app_catalog.py", "window_backend.py", "waydock-toplevel",
               "dock_style.py", "dock_config.py", "settings_api.py", "group_layout_settings.py", "dock_appearance.py",
               "lock_media.py", "application_audio.py", "labwc_titlebar_theme.py",
               "icons/waydock-app-menu-symbolic.svg"),
        python_entries=("waydock.py",),
        health=("waydock", "--check"),
        base=".local/share", requires_confirmation=True,
        clobbered_by=("waydock/install.py",),
        notes=("The drop-in changes only ExecStart; Type=notify, NotifyAccess=main (the binary is the main PID), "
               "TimeoutStartSec and Restart stay in the base unit.",
               "Every Python module stays: Global Settings embeds waydock.py's SettingsWindow, ~25 components "
               "import waydock modules, and the dock runs app_catalog.py inside each launched app's unit.",
               "~/.local/bin/waydock execs the binary: keybinds (--menu-hotkey), showdesktop scripts and the "
               ".desktop --settings entry are unchanged.",
               "Restarts the dock and tray: --apply needs --i-am-at-the-keyboard.")),
    Component(
        "global-settings", "global-settings", "global-settings", ("global-settings",),
        shims=(),
        launchers=((".local/bin/global-settings", "components/global-settings/shim/global-settings-launcher",
                    ".local/share/global-settings/global_settings.py"),
                   (".local/bin/language-manager", "components/global-settings/shim/language-manager-launcher",
                    ".local/share/global-settings/global_settings.py")),
        keeps=("global_settings.py", "runtime_paths.py", "global_ui.py", "global_style.py", "page_api.py",
               "page_widgets.py", "settings_search.py", "applet_pages.py", "desktop_pages.py", "dock_pages.py",
               "layout_page.py", "layout_presets.py", "lock_page.py", "login_page.py", "language_page.py",
               "weather_page.py", "capture_page.py", "input_page.py", "input_config.py", "input_devices.py",
               "mouse_polling.py", "mouse_polling_ui.py", "keyboard_page.py", "keyboard_config.py",
               "shortcuts_page.py", "shortcuts_model.py", "accessibility_page.py", "default_apps_page.py",
               "night_light.py", "night_light_page.py", "users_page.py", "users_backend.py", "about_page.py",
               "labwc_settings.py", "workspace_config.py", "workspace_settings.py", "layouts/gnome.layout",
               "layouts/kde.layout", "layouts/mac.layout", "layouts/win.layout"),
        base=".local/share",
        notes=("Only the launchers change: ~/.local/bin/global-settings and ~/.local/bin/language-manager "
               "(--section language) exec the binary. The .desktop entry, waydock's gear, applet Settings "
               "buttons, Super+/ (--shortcuts) and Welcome all go through ~/.local/bin/global-settings.",
               "Every Python module stays: tiling-manager's tiling_config.py imports workspace_config from "
               "~/.local/share/global-settings, and the Python Welcome imports global_style, runtime_paths, "
               "desktop_pages, shortcuts_model and the keyboard/layout/language pages.",
               "The binary embeds the Rust applet pages (audio, bluetooth, network, power, battery, keep-awake, "
               "notifications, date/time, weather, language, capture, displays, dock, …) directly; the Python "
               "applet modules it used to import are no longer needed by Global Settings itself.",
               "night-light.service (copied by install_gtk4.py) is unchanged; it runs the Python night_light.py.")),
    Component(
        "welcome", "welcome", "desktop-suite-welcome", (("desktop-suite-welcome", "desktop-suite-welcome"),),
        shims=(),
        launchers=((".local/bin/desktop-suite-welcome", "components/welcome/shim/desktop-suite-welcome-launcher",
                    ".local/share/desktop-suite-welcome/welcome.py"),),
        keeps=("welcome.py", "welcome_ui.py"),
        base=".local/share",
        clobbered_by=("welcome/install.py",),
        notes=("First-login autostart (install_all.py: `desktop-suite-welcome --first-run` while "
               "$XDG_STATE_HOME/desktop-suite/setup-done is missing) and the labwc menu item run the launcher; "
               "the .desktop Exec is the launcher path and does not change.",
               "setup-done is written by the binary exactly as welcome_ui.mark_setup_done did.")),
    # ---- wave 4/5 user-level entries ------------------------------------------------------------
    Component(
        "hdr-calibration", "hdr-calibration", "hdr-calibration", ("hdr-calibration",),
        shims=(Shim("hdr_calibration.py", "hdr-calibration", ("--output", "--sdr-editing", "--smoke-test")),),
        launchers=(".local/bin/hdr-calibration",),
        keeps=("calibration.py", "correction.py", "app_menu.py", "gtk4_utils.py", "hdr-pattern"),
        base=".local/share",
        clobbered_by=("hdr-calibration/install.py",),
        notes=("way-displays' color_controls.py runs `python3 .../hdr-calibration/hdr_calibration.py --output <connector>`:"
               " served by the shim. Global Settings runs ~/.local/bin/hdr-calibration (now execs the binary).",
               "hdr-pattern stays the C helper; the binary finds it next to itself. The .desktop Exec is the launcher.")),
    Component(
        "lock-screen", "lock-screen", "lock-screen", ("lock-screen", "waylock-pam"),
        shims=(Shim("lock_screen.py", "lock-screen", ("--preview", "--check"),
                    porter="components/lock-screen/shim/lock_screen.py"),),
        units=(Unit("power-manager-lock.service", "lock-screen", ()),),
        keeps=("lock_auth.py", "lock_config.py", "lock_dashboard.py", "lock_fingerprint.py", "lock_info.py",
               "lock_media.py", "lock_media_icons.py", "lock_palette.py", "lock_settings.py", "lock_status.py",
               "lock_visuals.py", "lock_weather.py", "voice_state.py", "weather_backend.py", "osk_config.py",
               "osk_devices.py", "osk_keyboard.py", "osk_lock.py", "waykeys-input", "suite_i18n.py"),
        base=".local/share", requires_confirmation=True,
        refuse_while_active=("power-manager-lock.service",),
        clobbered_by=("lock-screen/install_lock.py", "on-screen-keyboard/install.py", "capture-manager/install_session.py"),
        notes=("The drop-in changes only ExecStart (-> lock-screen); Type=notify, NotifyAccess=main, SuccessExitStatus=75, "
               "LimitCORE=0 and Restart stay in the base unit. The unit is never restarted: the next lock runs Rust.",
               "waylock-pam (the PAM helper next to the locker) becomes the Rust build, mode exactly 0755 (never setuid).",
               "lock_screen.py becomes the porter shim (lock_settings.py runs it with --preview); every Python module stays "
               "(Global Settings, capture-manager and the greeter import them).",
               "--apply refuses while power-manager-lock.service is active and needs --i-am-at-the-keyboard."),
        warning=("lock-screen: right after --apply, at the keyboard: run ~/.local/share/lock-screen/lock-screen --check "
                 "(must print three true values), then lock with Super+L and unlock with your password. If it fails, "
                 "switch to a TTY (Ctrl+Alt+F3), log in, run tools/install.py --rollback --only lock-screen, then "
                 "`systemctl --user restart power-manager-lock.service`: the Python locker takes over the still-locked "
                 "session (ext-session-lock keeps it locked when the client dies) and unlocks it.")),
    # ---- --system entries (root-owned, /usr/local; see the module docstring) ---------------------
    *(Component(
        key, key, key, (binary,), shims=(),
        launchers=((f"{SYSTEM_BIN}/{command}", None, f"{SYSTEM_LIB}/{key}/{entry}"),),
        keeps=keeps, base=SYSTEM_LIB, scope="system", locale_dir="locale", requires_rpms=rpms,
        notes=(f"Only the `exec` line of /{SYSTEM_BIN}/{command} changes (the standalone marker line stays); the .desktop "
               f"files keep Exec=/{SYSTEM_BIN}/{command}. desktop-apps/install.py (and file-manager/install.py) keep a "
               "launcher that carries the desktop-suite-rust marker; the Python files stay for --rollback.", *notes))
      for key, binary, command, entry, keeps, rpms, notes in (
        ("calculator", "calculator", "calculator", "calculator.py",
         ("calculator_backend.py", "calculator_ui.py", "gtk4_utils.py", "suite_i18n.py"), (), ()),
        ("text-editor", "text-editor", "text-editor", "text_editor.py",
         ("document.py", "editor_ui.py", "recovery.py", "gtk4_utils.py", "suite_i18n.py"), ("gtksourceview5",), ()),
        ("picture-viewer", "waypictures", "waypictures", "picture_viewer.py",
         ("picture_ui.py", "picture_canvas.py", "picture_model.py", "picture_files.py", "picture_dialogs.py"), (), ()),
        ("system-monitor", "system-monitor", "system-monitor", "system_monitor.py",
         ("monitor_backend.py", "monitor_ui.py", "gtk4_utils.py", "suite_i18n.py"), (), ()),
        ("capture-manager", "waycapture", "waycapture", "capture.py",
         ("libcapture-pipewire.so", "capture_settings.py", "capture_shortcuts.py", "capture_selection.py",
          "popup_placement.py", "capture_ui.py"), (),
         ("libcapture-pipewire.so stays where it is (the binary dlopens it from its own directory); Global Settings "
          "imports capture_settings.py / capture_shortcuts.py from this directory.",
          "shared/install_popup_fixes.py keeps rewriting the Python capture modules here; the launcher is not touched.")),
        ("file-manager", "wayfiles", "wayfiles", "file_manager.py",
         ("files_ui.py", "files_pane.py", "file_operations.py", "file_model.py", "media_chooser.py", "app_catalog.py",
          "dock_appearance.py"), ("gtksourceview5",),
         ("The binary must stay at /usr/local/lib/desktop-suite/file-manager/wayfiles: the launch helper re-executes "
          "current_exe() inside systemd-run --user services.",
          "Verify: wayfiles --check, open Home, right-click a file near the screen middle, Ctrl+Q.")),
    )),
    Component(
        "archive-manager", "archive-manager", "archive-manager", ("wayarchive",), shims=(),
        launchers=((f"{SYSTEM_BIN}/wayarchive", None, f"{SYSTEM_LIB}/archive-manager/wayarchive"),),
        files=((f"usr/local/share/applications/{ARCHIVE_DESKTOP}", ARCHIVE_DESKTOP_SOURCE, 0o644),),
        base=SYSTEM_LIB, scope="system", locale_dir="locale", fresh=True, requires_rpms=("libarchive",),
        notes=("A fresh install (there is no Python twin, it replaces KDE's Ark): the binary, its catalogs, the "
               "/usr/local/bin/wayarchive launcher (standalone + desktop-suite-rust markers) and "
               f"/usr/local/share/applications/{ARCHIVE_DESKTOP} are created; --system --rollback --only "
               "archive-manager removes exactly those (and the folders it created, once empty).",
               "Optional tools used when present: 7z (7zip-standalone), zip/unzip/zipinfo/zipnote, unar/lsar; lzop, "
               "lrzip and lzip for those tar filters. libarchive.so.13 is linked by soname.",
               "Default handler for archives: user-scope entry archive-manager-mime (only with --only).",
               "Verify: wayarchive --check, then open an archive and drag an entry into Files.")),
    Component(
        "keyring", "waykeyring", "keyring", ("waykeyring",), shims=(),
        files=(("usr/local/lib/desktop-suite/keyring/waykeyring-prompt", "target/release/waykeyring-prompt", 0o755),
               ("usr/lib64/security/pam_waykeyring.so", "target/release/libpam_waykeyring.so", 0o755)),
        edits=(TextEdit("etc/pam.d/waylogin", "auth include waylogin-auth\n",
                        "auth substack waylogin-auth\n-auth optional pam_waykeyring.so\n"),
               TextEdit("etc/pam.d/waylogin", "session include system-auth\n",
                        "session include system-auth\n-session optional pam_waykeyring.so\n"),
               TextEdit("etc/pam.d/passwd", "-password   optional\tpam_gnome_keyring.so use_authtok\n",
                        "-password   optional\tpam_gnome_keyring.so use_authtok\n-password   optional\tpam_waykeyring.so\n")),
        extra_packages=("waykeyring-ui", "pam-waykeyring"),
        base=SYSTEM_LIB, scope="system", locale_dir="locale", fresh=True, requires_confirmation=True, explicit=True,
        notes=("waykeyring, the suite's own Secret Service (org.freedesktop.secrets): /usr/local/lib/desktop-suite/keyring/"
               "{waykeyring,waykeyring-prompt}, /usr/lib64/security/pam_waykeyring.so, and three PAM lines: Waylogin's auth "
               "stack runs waylogin-auth as a substack followed by `-auth optional pam_waykeyring.so` (keeps the password "
               "for the session), `-session optional pam_waykeyring.so` (runs `waykeyring --login` as the user), and "
               "passwd's `-password optional pam_waykeyring.so` (re-wraps the keyring when the password changes).",
               "Every pam_waykeyring entry point returns PAM_IGNORE: it never decides a login.",
               "Then the user entry keyring-session (D-Bus activation + waykeyring.service) and a new login.",
               "Runs only when named: --system --apply --only keyring --i-am-at-the-keyboard.",
               "login-manager/install_login.py rewrites /etc/pam.d/waylogin (drops the lines); re-run this entry after it."),
        warning=("keyring: this edits /etc/pam.d/waylogin (Waylogin's login stack). Only optional lines are added and "
                 "`auth include waylogin-auth` becomes `auth substack waylogin-auth`; still, if graphical login fails "
                 "afterwards, switch to a TTY (Ctrl+Alt+F3), log in there and run: sudo /usr/bin/python3 "
                 + str(Path(__file__).resolve()) + " --system --rollback --only keyring")),
    Component(
        "login-manager", "login-manager", "waylogin", ("login-ui", "session-transition", "login-settings-helper"),
        shims=(),
        edits=(TextEdit("usr/local/lib/waylogin/start-greeter",
                        "--session '/usr/bin/python3 -I {base}/usr/local/lib/waylogin/login_ui.py'",
                        "--session '{base}/usr/local/lib/waylogin/login-ui'"),),
        keeps=("login_ui.py", "login_model.py", "login_protocol.py", "login_transition.py", "login_settings_helper.py",
               "session_transition.py", "start-session", "lock_config.py", "lock_visuals.py", "lock_palette.py",
               "osk_lock.py", "waykeys-input", "suite_i18n.py"),
        base="usr/local/lib", scope="system", locale_dir="locale", requires_confirmation=True,
        clobbered_by=("login-manager/install_login.py",),
        notes=("Only start-greeter's --session argument changes. greetd.toml, PAM files, faillock, the polkit rule and "
               "waylogin.service are never read or written; nothing is restarted (greetd runs the new greeter after the "
               "next logout).",
               "The greeter still starts sessions with the Python session_transition.py fade and Global Settings still "
               "runs the Python login_settings_helper.py; the Rust session-transition / login-settings-helper are "
               "installed next to them, unused until those callers switch.",
               "login-manager/install_login.py rewrites start-greeter (Python again); re-run --system --apply after it."),
        warning=("login-manager: a BROKEN GREETER BLOCKS GRAPHICAL LOGIN. Nothing is restarted; the Rust greeter runs at "
                 "the next logout. Test it at the keyboard before you rely on it. If no login screen appears, switch to a "
                 "TTY (Ctrl+Alt+F3), log in there and roll back: sudo /usr/bin/python3 " + str(Path(__file__).resolve())
                 + " --system --rollback --only login-manager  (or copy start-greeter back from the backup printed "
                 "above: /var/backups/desktop-suite/rust-swap-*/usr/local/lib/waylogin/start-greeter), then "
                 "`sudo systemctl restart waylogin.service` from that TTY to get the Python greeter back.")),
    # ---- fresh user entries (no Python twin) ----------------------------------------------------
    Component(
        "clipboard-manager", "clipboard-manager", "clipboard-manager", ("wayclip",), shims=(), fresh=True,
        uses_locale=False, files=((f"{UNITS}/clipboard-manager.service",
                                   "components/clipboard-manager/clipboard-manager.service", 0o644),),
        notes=("A fresh install (there is no Python twin): wayclip and ~/.config/systemd/user/clipboard-manager.service "
               "(WantedBy=graphical-session.target) are created; --rollback --only clipboard-manager removes them.",
               "Nothing is enabled or started: run `systemctl --user enable --now clipboard-manager.service` once.",
               "Settings: ~/.config/desktop-suite/clipboard.json (Global Settings > Clipboard).",
               "Verify: wayclip --check; copy text, close the application, paste.")),
    Component(
        "keyring-session", "waykeyring", "", (), shims=(), fresh=True, uses_locale=False, base=".config", explicit=True,
        files=((".local/share/dbus-1/services/org.freedesktop.secrets.service",
                "components/waykeyring/data/org.freedesktop.secrets.service", 0o644),
               (f"{UNITS}/waykeyring.service", "components/waykeyring/data/waykeyring.service", 0o644),
               (f"{APPS}/org.waydock.Passwords.desktop", "components/waykeyring/data/org.waydock.Passwords.desktop", 0o644)),
        notes=("Makes waykeyring the session's Secret Service: ~/.local/share/dbus-1/services/org.freedesktop.secrets.service "
               "(takes precedence over gnome-keyring's /usr/share file) activates waykeyring.service. Install the system "
               "entry keyring first. Also adds the Passwords launcher (Global Settings > Passwords) to the app menu. "
               "A Secret Service that is already running (gnome-keyring) keeps the name until you "
               "sign out; waykeyring takes over at the next login and imports GNOME's unencrypted keyrings.",)),
    # ---- explicit user entries (never part of a plain --apply) -----------------------------------
    Component(
        "archive-manager-mime", "archive-manager", "", (), shims=(), base=".config", fresh=True, uses_locale=False,
        explicit=True, mime_defaults=(MimeDefaults(".config/mimeapps.list", ARCHIVE_DESKTOP, ARCHIVE_MIMES),),
        notes=("Makes Archive Manager the default application for every archive type it opens: only those keys of "
               "[Default Applications] in ~/.config/mimeapps.list change (Ark's [Added Associations] lines and every "
               "other byte stay). Install the system entry archive-manager first.",
               "--rollback --only archive-manager-mime restores the previous defaults (keys you changed since are "
               "left alone). Runs only when named: --apply --only archive-manager-mime.")),
    # ---- --target-root entry (the Kasumi device, aarch64) --------------------------------------
    Component(
        "home-assistant-screen", "home-assistant-screen", "screensaver", ("luna-clock", "voice-controls"),
        shims=(Shim("luna_clock.py", "luna-clock",
                    ("--show", "--hide", "--status", "--next-wallpaper", "--quit", "--volume", "--brightness")),
               Shim("voice_controls.py", "voice-controls")),
        units=(Unit("luna-clock.service", "luna-clock"),),
        keeps=("admin_panel.py", "screen_keyboard.py", "screen_levels.py", "level_controls.py", "screen_timers.py",
               "screen_alarms.py", "timer_countdowns.py", "alarm_indicators.py", "config.json", "waykeys-input"),
        base="makima", scope="remote", arch="aarch64",
        clobbered_by=("home-assistant-screen/install_remote.py", "home-assistant-screen/install_media.py",
                      "home-assistant-screen/install_keyboard.py"),
        notes=("Kasumi only: --target-root is the Kasumi user's home (mounted or staged here); this machine's home and "
               "units are never touched and systemctl is never run. Binaries must be aarch64 builds "
               f"({AARCH64_RELEASE} or --prebuilt).",
               "luna-clock.service gets the zz- drop-in (same flags as its Python ExecStart); config.json next to the "
               "binary is read, never written. On the device: systemctl --user daemon-reload && systemctl --user "
               "restart luna-clock.service.")),
)
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
    kind: str                 # binary | helper | shim | fallback | dropin | launcher | desktop | rcxml | locale
    data: bytes = None        # None: binary not built yet (preview only)
    mode: int = 0o644
    note: str = ""
    source: Path = None
    commands: dict = None     # rcxml: {key: original raw command}, kept in the state for --rollback
    replacements: list = None # edit: [[old, new]] (formatted), kept in the state for --rollback


def bins(component):
    """[(cargo [[bin]] name, installed file name)]."""
    return [b if isinstance(b, tuple) else (b, b) for b in component.binaries]


def installed_names(component):
    return [installed for _, installed in bins(component)]


def launcher_spec(component, launcher):
    """(home-relative path, porter file or None, text that marks the Python launcher)."""
    relative, porter, match = (tuple(launcher) + (None, None))[:3] if isinstance(launcher, tuple) else (launcher, None, None)
    if match is None:
        match = f"{component.base}/{component.dir}/{component.shims[0].entry}"
    return relative, porter, match


def main_binary(component):
    return component.shims[0].binary if component.shims else installed_names(component)[0]


def component_dir(home, component):
    return home / component.base / component.dir


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


def unit_args(home, component, unit):
    """The unit's flags: fixed in the table, or those the Python ExecStart passes after the script."""
    if unit.args is not None:
        return tuple(unit.args)
    current = effective_execstart(home, unit.name, skip_ours=True) or ""
    words = shlex.split(current.lstrip("-@:+!"))
    scripts = {shim.entry for shim in component.shims}
    for index, word in enumerate(words):
        if Path(word).name in scripts:
            return tuple(words[index + 1:])
    return ()


def unit_command(home, component, unit):
    relative = binary_path(home, component, unit.binary).relative_to(home)
    return " ".join([f"%h/{relative}", *(shlex.quote(arg) for arg in unit_args(home, component, unit))])


def dropin_text(component, unit, home):
    return (f"# Desktop suite Rust swap for {component.key}, generated by nobara-de-rust/tools/install.py.\n"
            f"# Sorts after every other drop-in, so this ExecStart is the one systemd runs.\n"
            f"# Undo: tools/install.py --rollback --only {component.key}\n"
            f"[Service]\nExecStart=\nExecStart={unit_command(home, component, unit)}\n")


def launcher_text(component, home, porter=None):
    if porter:
        return (REPO / porter).read_text()
    binary = binary_path(home, component, main_binary(component))
    if component.scope == "system":
        # desktop-apps/install.py refuses launchers without STANDALONE_MARKER and keeps the ones with SHIM_MARKER.
        return (f"#!/bin/sh\n{STANDALONE_MARKER}\n# {SHIM_MARKER} for {component.key}: installed by "
                f"nobara-de-rust/tools/install.py --system; undo with --system --rollback --only {component.key}\n"
                f"exec {shlex.quote(str(binary))} \"$@\"\n")
    return (f"#!/bin/sh\n# Desktop suite Rust swap for {component.key} (nobara-de-rust/tools/install.py).\n"
            f"exec '{binary}' \"$@\"\n")


def edit_strings(home, edit):
    """(old, new) of a TextEdit with {base} = the prefix ("" for /)."""
    base = str(home).rstrip("/")
    if "'" in base or "\\" in base:
        raise RuntimeError(f"Refusing a prefix with quotes for a shell edit: {home}")
    return edit.old.format(base=base), edit.new.format(base=base)


def edit_revert_text(text, replacements):
    """text with each swapped string (still present exactly once) put back to the Python original."""
    for old, new in replacements:
        if text.count(new) == 1:
            text = text.replace(new, old)
    return text


def edit_state(home, edit):
    path = home / edit.path
    if not path.is_file() or path.is_symlink():
        return "missing"
    old, new = edit_strings(home, edit)
    text = path.read_text(errors="replace")
    return "rust" if new in text else "python" if old in text else "other"


def mimeapps_section(lines):
    """(start, end) line indices of the [Default Applications] body, or None."""
    for index, line in enumerate(lines):
        if line.strip() == "[Default Applications]":
            end = index + 1
            while end < len(lines) and not lines[end].lstrip().startswith("["):
                end += 1
            return index + 1, end
    return None


def mimeapps_swap_text(text, mimes, desktop):
    """(new text, {mime: [original values] or None}): each key of [Default Applications] set to `desktop`."""
    lines = text.splitlines(keepends=True)
    if lines and not lines[-1].endswith("\n"):
        lines[-1] += "\n"
    section = mimeapps_section(lines)
    if section is None:
        if lines and lines[-1].strip():
            lines.append("\n")
        lines.append("[Default Applications]\n")
        section = (len(lines), len(lines))
    start, end = section
    value = f"{desktop};"
    originals, seen = {}, set()
    for index in range(start, end):
        key, sep, current = lines[index].rstrip("\n").partition("=")
        if sep and key.strip() in mimes:
            originals.setdefault(key.strip(), []).append(current)
            seen.add(key.strip())
            lines[index] = f"{key.strip()}={value}\n"
    insert = end
    while insert > start and not lines[insert - 1].strip():
        insert -= 1
    added = [f"{mime}={value}\n" for mime in mimes if mime not in seen]
    for mime in mimes:
        if mime not in seen:
            originals[mime] = None
    lines[insert:insert] = added
    return "".join(lines), originals


def mimeapps_revert_text(text, originals, desktop):
    """Put back the original values of keys that still point at `desktop` (added keys go)."""
    lines = text.splitlines(keepends=True)
    section = mimeapps_section(lines)
    if section is None:
        return text
    start, end = section
    value = f"{desktop};"
    used = {}
    keep = []
    for index, line in enumerate(lines):
        key, sep, current = line.rstrip("\n").partition("=")
        key = key.strip()
        if start <= index < end and sep and key in originals and current == value:
            original = originals[key]
            if original is None:
                continue
            n = used.get(key, 0)
            used[key] = n + 1
            if n < len(original):
                keep.append(f"{key}={original[n]}\n")
            continue
        keep.append(line)
    return "".join(keep)


def mime_state(home, defaults):
    path = home / defaults.path
    if not path.is_file() or path.is_symlink():
        return "missing"
    lines = path.read_text(errors="replace").splitlines()
    section = mimeapps_section(lines)
    if section is None:
        return "other"
    values = {}
    for line in lines[section[0]:section[1]]:
        key, sep, current = line.partition("=")
        if sep:
            values.setdefault(key.strip(), current.split(";")[0])
    return "rust" if all(values.get(m) == defaults.desktop for m in defaults.mimes) else "other"


def elf_machine(path):
    """e_machine of an ELF file (None for scripts and unreadable files)."""
    try:
        with open(path, "rb") as stream:
            head = stream.read(20)
    except OSError:
        return None
    if len(head) < 20 or head[:4] != b"\x7fELF":
        return None
    return int.from_bytes(head[18:20], "little" if head[5] == 1 else "big")


def unsafe_directories(path, root):
    """Existing directories from root down to path's parent that someone else could change under root."""
    found = []
    directory = path.parent
    chain = []
    while True:
        chain.append(directory)
        if directory == root or directory == directory.parent:
            break
        directory = directory.parent
    owner = 0 if root == Path("/") else os.getuid()     # a private test prefix belongs to the test user
    for directory in reversed(chain):
        if not directory.exists():
            continue
        info = directory.stat()
        if info.st_uid != owner or info.st_mode & 0o022:
            found.append(f"{directory} (owner uid {info.st_uid}, mode {stat.S_IMODE(info.st_mode):o})")
    return found


def entry_pattern(home, entry):
    """Match a python3 invocation of the entry, written with an absolute, ~ or $HOME path."""
    forms = [re.escape(str(home / entry)), r"(?:~|\$HOME|\$\{HOME\})/" + re.escape(entry)]
    path = "(?:" + "|".join(forms) + ")"
    return re.compile(r"(?:/usr/bin/)?python3\s+([\"']?)" + path + r"\1")


def desktop_text(text, component, home):
    binary = str(binary_path(home, component, component.shims[0].binary))
    pattern = entry_pattern(home, f"{component.base}/{component.dir}/{component.shims[0].entry}")
    return re.sub(r"(?m)^(Exec=)(.*)$", lambda m: m[1] + pattern.sub(lambda _: binary, m[2]), text)


# ---- labwc rc.xml keybinds (only the command attribute of the matching Execute actions changes) ----

KEYBIND_BLOCK = re.compile(r"<keybind\b(?P<attrs>[^>]*?)(?:/>|>(?P<body>.*?)</keybind\s*>)", re.S)
EXECUTE_TAG = re.compile(r"<action\b[^>]*>", re.S)
ATTRIBUTE = r"\b{name}\s*=\s*(?P<q>[\"'])(?P<value>.*?)(?P=q)"


def mask_comments(text):
    """text with every <!-- ... --> (and CDATA) blanked out, same length, so offsets stay valid."""
    return re.sub(r"<!--.*?-->|<!\[CDATA\[.*?\]\]>", lambda m: " " * len(m[0]), text, flags=re.S)


def attribute(tag, name):
    return re.search(ATTRIBUTE.format(name=name), tag, re.S)


def keybind_commands(text):
    """[(key, start, end, raw value)] for each Execute command attribute inside a <keybind> (not commented)."""
    masked = mask_comments(text)
    found = []
    for block in KEYBIND_BLOCK.finditer(masked):
        key = attribute(block["attrs"], "key")
        if not key or block["body"] is None:
            continue
        offset = block.start("body")
        for tag in EXECUTE_TAG.finditer(block["body"]):
            name = attribute(tag[0], "name")
            command = attribute(tag[0], "command")
            if not name or html.unescape(name["value"]) != "Execute" or not command:
                continue
            start, end = offset + tag.start() + command.start("value"), offset + tag.start() + command.end("value")
            found.append((html.unescape(key["value"]), start, end, text[start:end]))
    return found


def rust_keybind_command(component, keybind):
    """What labwc runs after the swap; labwc runs Execute through sh, which expands ~/ (no home in rc.xml)."""
    return " ".join([f"~/{component.base}/{component.dir}/{keybind.binary}", *keybind.args])


def runs_python_entry(raw, home, component, keybind):
    try:
        words = shlex.split(html.unescape(raw))
    except ValueError:
        return False
    if len(words) < 2 or not PYTHON.match(Path(words[0]).name):
        return False
    script = words[1]
    for prefix in ("~/", "$HOME/", "${HOME}/"):
        if script.startswith(prefix):
            script = str(home / script[len(prefix):])
    return script == str(component_dir(home, component) / keybind.entry) and tuple(words[2:]) == tuple(keybind.args)


def rc_edit(text, edits):
    """Apply [(start, end, new raw value)] (non-overlapping) to text."""
    for start, end, value in sorted(edits, reverse=True):
        text = text[:start] + value + text[end:]
    return text


def rc_swap_text(text, component, home):
    """(new text, {key: original raw command}) with the Python keybinds pointed at the binaries."""
    by_key = {keybind.key: keybind for keybind in component.keybinds}
    edits, originals = [], {}
    for key, start, end, raw in keybind_commands(text):
        keybind = by_key.get(key)
        if keybind and runs_python_entry(raw, home, component, keybind):
            edits.append((start, end, xml_escape(rust_keybind_command(component, keybind), {'"': "&quot;"})))
            originals.setdefault(key, raw)
    return rc_edit(text, edits), originals


def rc_revert_text(text, component, originals):
    """text with the swapped keybinds (still running the binary) back on their original commands."""
    by_key = {keybind.key: keybind for keybind in component.keybinds}
    edits = [(start, end, originals[key]) for key, start, end, raw in keybind_commands(text)
             if key in by_key and key in originals
             and html.unescape(raw) == rust_keybind_command(component, by_key[key])]
    return rc_edit(text, edits)


def keybind_states(home, component):
    """{key: rust | python | other | missing} for --status."""
    path = home / RC_XML
    text = path.read_text(errors="replace") if path.is_file() else ""
    states = {}
    for keybind in component.keybinds:
        values = [raw for key, _s, _e, raw in keybind_commands(text) if key == keybind.key]
        if not values:
            states[keybind.key] = "missing"
        elif all(html.unescape(v) == rust_keybind_command(component, keybind) for v in values):
            states[keybind.key] = "rust"
        elif any(runs_python_entry(v, home, component, keybind) for v in values):
            states[keybind.key] = "python"
        else:
            states[keybind.key] = "other"
    return states


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
        release = REPO / (AARCH64_RELEASE if component.arch == "aarch64" else "target/release")
        for cargo, installed in bins(component):
            sources[component.key, installed] = (prebuilt or release) / cargo
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


def plan_component(component, home, sources, locale_source=None):
    """[Change] for one component (unchanged files left out) and [problem] that block the swap."""
    changes, problems = [], []
    if not component.fresh and not component_dir(home, component).is_dir():
        if component.optional:
            return changes, []
        return changes, [f"{component.base}/{component.dir} is not installed; install the Python component first"]
    for name in (*installed_names(component), *component.helpers):
        target, source = binary_path(home, component, name), sources[component.key, name]
        kind = "helper" if name in component.helpers else "binary"
        if not valid_binary(source):
            build = (f"cargo build --release --target aarch64-unknown-linux-gnu -p {component.package}, or --prebuilt"
                     if component.arch == "aarch64" else f"cargo build --release -p {component.package}")
            changes.append(Change(component.key, target, kind, None, 0o755, f"not built: {source} ({build})"))
            continue
        machine = elf_machine(source)
        if component.arch == "aarch64" and machine is not None and machine != EM_AARCH64:
            problems.append(f"{source} is not an aarch64 build (ELF machine {machine}); the Kasumi device needs aarch64")
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
            if not component.optional:
                problems.append(f"{UNITS}/{unit.name} is not installed")
            continue
        path = home / UNITS / (unit.name + ".d") / DROPIN
        data = dropin_text(component, unit, home).encode()
        if current_digest(path) != digest(data):
            before = effective_execstart(home, unit.name, skip_ours=True)
            changes.append(Change(component.key, path, "dropin", data, 0o644,
                                  f"ExecStart: {before} -> {unit_command(home, component, unit)}"))
    for launcher in component.launchers:
        relative, porter, entry = launcher_spec(component, launcher)
        path = home / relative
        data = launcher_text(component, home, porter).encode()
        if component.fresh:
            if path.is_symlink() or (path.exists() and path.read_bytes() != data
                                     and SHIM_MARKER not in path.read_text(errors="replace")):
                problems.append(f"{relative} exists and was not installed by this tool; not replaced")
            elif not path.exists() or path.read_bytes() != data:
                changes.append(Change(component.key, path, "launcher", data, 0o755,
                                      f"new launcher: exec {binary_path(home, component, main_binary(component))}"))
            continue
        if not path.is_file():
            continue
        text = path.read_text(errors="replace")
        if path.read_bytes() != data and (entry in text or path.is_symlink()):
            changes.append(Change(component.key, path, "launcher", data, stat.S_IMODE(path.lstat().st_mode),
                                  f"exec {binary_path(home, component, main_binary(component))}"
                                  + (f" ({porter})" if porter else "")))
    for relative, source, mode in component.files:
        path = home / relative
        data = (REPO / source).read_bytes()
        if path.is_symlink():
            problems.append(f"{relative} is a symlink; not replaced")
        elif current_digest(path) != digest(data) or stat.S_IMODE(path.stat().st_mode) != mode:
            changes.append(Change(component.key, path, "desktop" if relative.endswith(".desktop") else "file", data, mode,
                                  f"from {source}", REPO / source))
    for defaults in component.mime_defaults:
        path = home / defaults.path
        if path.is_symlink():
            problems.append(f"{defaults.path} is a symlink; not edited")
            continue
        text = path.read_text() if path.is_file() else ""
        new, originals = mimeapps_swap_text(text, defaults.mimes, defaults.desktop)
        if new != text:
            changed = sorted(m for m in defaults.mimes if originals.get(m) != [f"{defaults.desktop};"])
            changes.append(Change(component.key, path, "mimeapps", new.encode(), stat.S_IMODE(path.stat().st_mode)
                                  if path.exists() else 0o644,
                                  f"[Default Applications]: {len(changed)} archive types -> {defaults.desktop}",
                                  commands=originals))
    for relative in component.desktop:
        path = home / relative
        if not path.is_file():
            continue
        text = path.read_text(errors="replace")
        new = desktop_text(text, component, home)
        if new != text:
            changes.append(Change(component.key, path, "desktop", new.encode(), stat.S_IMODE(path.lstat().st_mode),
                                  "Exec runs the binary"))
    rc = home / RC_XML
    if component.keybinds and rc.is_symlink():
        problems.append(f"{RC_XML} is a symlink; its keybinds are not changed")
    elif component.keybinds and rc.is_file():
        text = rc.read_text()
        new, originals = rc_swap_text(text, component, home)
        if new != text:
            try:
                ET.fromstring(text)
            except ET.ParseError:
                pass                # labwc's own parser is more lenient; only keep valid files valid
            else:
                ET.fromstring(new)
            note = "; ".join(f"{key}: {html.unescape(raw)} -> "
                             f"{rust_keybind_command(component, next(k for k in component.keybinds if k.key == key))}"
                             for key, raw in originals.items())
            changes.append(Change(component.key, rc, "rcxml", new.encode(), stat.S_IMODE(rc.stat().st_mode),
                                  note, commands=originals))
    # Several edits of one file are applied in order and become one change.
    edited = {}
    for edit in component.edits:
        path = home / edit.path
        old, new = edit_strings(home, edit)
        if path.is_symlink():
            problems.append(f"{edit.path} is a symlink; not edited")
            continue
        if not path.is_file():
            problems.append(f"{edit.path} is not installed; install the Python component first")
            continue
        original, text, notes, replacements = edited.get(path, (path.read_text(), None, [], []))
        text = original if text is None else text
        if new in text:                             # already swapped (`new` may contain `old`: added lines)
            edited[path] = (original, text, notes, replacements)
            continue
        if text.count(old) != 1:
            problems.append(f"{edit.path} does not contain exactly one `{old}`; not edited (changed by hand?)")
            continue
        edited[path] = (original, text.replace(old, new), notes + [f"{old} -> {new}"], replacements + [[old, new]])
    for path, (original, text, notes, replacements) in edited.items():
        if text != original:
            changes.append(Change(component.key, path, "edit", text.encode(), stat.S_IMODE(path.stat().st_mode),
                                  "\n".join(notes), replacements=replacements))
    if component.locale_dir:
        catalogs = sorted(locale_source.glob("*.json")) if locale_source and locale_source.is_dir() else []
        if not catalogs:
            problems.append(f"no translation catalogs in {locale_source}")
        for source in catalogs:
            target = component_dir(home, component) / component.locale_dir / source.name
            data = source.read_bytes()
            if current_digest(target) != digest(data) or target.is_symlink():
                changes.append(Change(component.key, target, "locale", data, 0o644, f"from {source}", source))
    if component.scope == "system":
        for change in changes:
            for directory in unsafe_directories(change.path, home):
                problems.append(f"refusing to write as root below {directory}: not root-owned or group/world-writable")
        problems = list(dict.fromkeys(problems))
    return changes, problems


def missing_rpms(component):
    missing = []
    for package in component.requires_rpms:
        try:
            found = subprocess.run(["rpm", "-q", package], capture_output=True).returncode == 0
        except OSError:
            found = False
        if not found:
            missing.append(package)
    return missing


def keep_path(home, component, keep):
    return home / keep[2:] if keep.startswith("~/") else component_dir(home, component) / keep


def guard_warnings(component):
    """Python-installer behaviour that undoes or trips over this swap."""
    shims = ", ".join(shim.entry for shim in component.shims)
    if not shims:
        if not component.clobbered_by:
            return []
        files = ", ".join([launcher_spec(component, l)[0] for l in component.launchers]
                          + [edit.path for edit in component.edits])
        again = "--system --apply" if component.scope == "system" else "--apply"
        return [f"re-running {', '.join(component.clobbered_by)} rewrites {files}; re-run {again}"]
    installers = component.clobbered_by if component.scope == "remote" else CLOBBER_ALL + component.clobbered_by
    return [f"re-running {', '.join(installers)} rewrites {shims}"
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

def state_path(home, system=False):
    return home / (SYSTEM_STATE if system else STATE)


def load_state(home, system=False):
    path = state_path(home, system)
    if path.is_symlink():
        raise RuntimeError("Refusing a linked state file: " + str(path))
    if not path.is_file():
        return {"version": 1, "components": {}, "dirs": []}
    return json.loads(path.read_text())


def save_state(home, state, system=False):
    atomic_write(state_path(home, system), (json.dumps(state, indent=2, sort_keys=True) + "\n").encode(), 0o644)


def make_backup(home, prefix, system=False):
    root = home / (SYSTEM_BACKUPS if system else BACKUPS)
    if root.is_symlink():
        raise RuntimeError("Refusing a linked backup directory: " + str(root))
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
    packages = sorted({package for component in components for package in (component.package, *component.extra_packages)})
    for package in packages:
        print(f"cargo build --release -p {package}")
        subprocess.run(["cargo", "build", "--release", "-p", package], cwd=REPO, check=True)


def needs_shared_locale(components):
    return any(component.scope != "system" and component.uses_locale for component in components)


def apply(home, components, sources, locale_source, system=False):
    planned, problems = [], []
    for component in components:
        changes, issues = plan_component(component, home, sources, locale_source)
        planned += changes
        problems += [f"{component.key}: {issue}" for issue in issues]
    if needs_shared_locale(components):
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
    backup = make_backup(home, "rust-swap", system)
    manifest = []
    for change, before in befores:
        if before["kind"] == "file":
            atomic_write(backup / change.path.relative_to(home), before["data"], before["mode"])
        manifest.append({"path": str(change.path), "component": change.component, "kind": change.kind,
                         "before": before["kind"], "after_sha256": digest(change.data)})
    (backup / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    state = load_state(home, system)
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
        if change.commands:
            for key, raw in change.commands.items():
                record.setdefault("commands", {}).setdefault(key, raw)
        for pair in change.replacements or ():
            if pair not in record.setdefault("replacements", []):
                record["replacements"].append(pair)
        record["installed_sha256"] = digest(change.data)
    known = state.setdefault("dirs", [])
    known += [d for directories in created_dirs.values() for d in directories if d not in known]
    save_state(home, state, system)
    print(f"Swapped {len(planned)} files. Backup: {backup}")
    return planned


def mime_desktop(group):
    defaults = BY_KEY[group].mime_defaults
    return defaults[0].desktop if defaults else ARCHIVE_DESKTOP


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
            elif current_digest(path) != record["installed_sha256"] and record.get("kind") == "rcxml" \
                    and path.is_file() and group in BY_KEY and record.get("commands") \
                    and rc_revert_text(path.read_text(), BY_KEY[group], record["commands"]) != path.read_text():
                steps.append((group, path, record, "revert"))     # edited since the swap: undo only our commands
            elif current_digest(path) != record["installed_sha256"] and record.get("kind") == "mimeapps" \
                    and path.is_file() and group in BY_KEY and record.get("commands") \
                    and mimeapps_revert_text(path.read_text(), record["commands"], mime_desktop(group)) != path.read_text():
                steps.append((group, path, record, "revert"))     # edited since the swap: undo only our defaults
            elif current_digest(path) != record["installed_sha256"] and record.get("kind") == "edit" \
                    and path.is_file() and record.get("replacements") \
                    and edit_revert_text(path.read_text(), record["replacements"]) != path.read_text():
                steps.append((group, path, record, "revert"))     # edited since the swap: undo only our string
            elif current_digest(path) != record["installed_sha256"]:
                steps.append((group, path, record, "keep"))
            else:
                steps.append((group, path, record, "remove" if record["before"] == "missing" else "restore"))
    return groups, steps


def rollback(home, keys, preview, force=False, system=False):
    state = load_state(home, system)
    keys = [key for key in keys if key in state["components"]]
    if not keys:
        print("Nothing to roll back: none of these components are swapped.")
        return []
    groups, steps = rollback_plan(home, keys, state)
    for group, path, record, action in steps:
        label = {"restore": "restore", "remove": "remove ", "keep": "KEEP   ", "refuse": "REFUSE ",
                 "revert": "revert "}[action]
        extra = {"keep": "  (changed since the swap; kept, use --force to restore the backup)",
                 "refuse": "  (now a symlink)",
                 "revert": "  (changed since the swap; only the swapped keybind commands are put back)"
                           if record.get("kind") == "rcxml" else
                           "  (changed since the swap; only the archive defaults still set by the swap are put back)"
                           if record.get("kind") == "mimeapps" else
                           "  (changed since the swap; only the swapped text is put back)"}.get(action, "")
        if action == "keep" and force:
            label, extra = "restore", "  (changed since the swap; --force)"
        print(f"  {group:24} {label} {path}{extra}")
    if preview:
        return []
    refused = [path for _, path, _, action in steps if action == "refuse"]
    if refused:
        raise RuntimeError("Refusing to replace linked files: " + ", ".join(map(str, refused)))
    backup = make_backup(home, "rust-rollback", system)
    manifest = []
    for group, path, record, action in steps:
        if action == "keep" and not force:
            continue
        current = snapshot(path)
        if current["kind"] == "file":
            atomic_write(backup / path.relative_to(home), current["data"], current["mode"])
        manifest.append({"path": str(path), "component": group, "before": current["kind"], "restored": record["before"]})
        if action == "revert" and record.get("kind") == "mimeapps":
            atomic_write(path, mimeapps_revert_text(path.read_text(), record["commands"], mime_desktop(group)).encode(),
                         current["mode"])
        elif action == "revert" and record.get("kind") == "edit":
            atomic_write(path, edit_revert_text(path.read_text(), record["replacements"]).encode(), current["mode"])
        elif action == "revert":
            atomic_write(path, rc_revert_text(path.read_text(), BY_KEY[group], record["commands"]).encode(),
                         current["mode"])
        elif record["before"] == "missing":
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
    save_state(home, state, system)
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
    binaries = {str(binary_path(home, component, name)) for name in installed_names(component)}
    entries = {str(entry_path(home, component, shim)) for shim in component.shims}
    entries |= {str(home / launcher_spec(component, l)[2]) for l in component.launchers}
    entries |= {str(component_dir(home, component) / entry) for entry in component.python_entries}
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
    launchers = {}
    for launcher in component.launchers:
        relative, porter, match = launcher_spec(component, launcher)
        path = home / relative
        if not path.is_file() or path.is_symlink():
            launchers[relative] = "missing"
        elif path.read_bytes() == launcher_text(component, home, porter).encode():
            launchers[relative] = "rust"
        else:
            launchers[relative] = "python" if match in path.read_text(errors="replace") else "other"
    units = {}
    for unit in component.units:
        dropin = home / UNITS / (unit.name + ".d") / DROPIN
        pid = unit_pid(unit.name) if component.scope == "user" else 0     # remote units are not on this machine
        info = proc_info(pid) if pid else None
        units[unit.name] = {"dropin": dropin.is_file(), "exec_start": effective_execstart(home, unit.name),
                            "main_pid": pid, "running": (classify(info, home, component) or "other") if info else None,
                            "rss_kb": info["rss_kb"] if info else 0}
    keybinds = keybind_states(home, component)
    edits = {edit.path: edit_state(home, edit) for edit in component.edits}
    edits.update({defaults.path: mime_state(home, defaults) for defaults in component.mime_defaults})
    running = [dict(info, impl=impl) for info in processes for impl in [classify(info, home, component)] if impl]
    configured = (list(shims.values()) + [v for v in launchers.values() if v != "other"]
                  + ["rust" if u["dropin"] else "python" for u in units.values()]
                  + [v for v in keybinds.values() if v in ("rust", "python")]
                  + [v for v in edits.values() if v != "other"])
    if all(state == "missing" for state in [*shims.values(), *launchers.values(), *edits.values()]):
        mode = "not installed"
    elif any(state == "python (shim overwritten)" for state in configured):
        mode = "python (shim overwritten)"
    elif all(state == "rust" for state in configured):
        mode = "rust"
    elif all(state in ("python", "missing") for state in configured):
        mode = "python" if "python" in configured else "not installed"
    else:
        mode = "mixed"
    warnings = guard_warnings(component) + list(component.notes) if mode != "python" and mode != "not installed" else []
    return {"component": component.key, "configured": mode, "shims": shims, "launchers": launchers, "units": units,
            "keybinds": keybinds, "edits": edits,
            "binaries": {n: binary_path(home, component, n).is_file() for n in installed_names(component)},
            "processes": [{"pid": p["pid"], "impl": p["impl"], "rss_kb": p["rss_kb"]} for p in running],
            "rss_kb": sum(p["rss_kb"] for p in running), "warnings": warnings}


def status(home, components, as_json, system=False):
    processes = list(user_processes())
    state = load_state(home, system)
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
        for key, key_state in item["keybinds"].items():
            print(f"    rc.xml {key}: {key_state}")
        for path, edit_state_ in item["edits"].items():
            print(f"    {path}: {edit_state_}")
        for warning in item["warnings"]:
            print(f"    warn: {warning}")
    print(f"{'total':68}{total / 1024:>8.1f} MB")
    return report


# ---- check (preview) ------------------------------------------------------------------------

def check(home, components, sources, locale_source):
    blocked = False
    for component in components:
        changes, problems = plan_component(component, home, sources, locale_source)
        print(f"{component.key}  (binaries: {', '.join(installed_names(component) + list(component.helpers))} "
              f"-> {component_dir(home, component)})")
        if component.optional and not component_dir(home, component).is_dir():
            print(f"  skipped  {component_dir(home, component)} is not installed (optional)")
            continue
        if component.requires_confirmation:
            print("  confirm  --apply needs --i-am-at-the-keyboard for this component")
        if component.scope == "system":
            print("  root     --system --apply/--rollback only as root (pkexec /usr/bin/python3 tools/install.py "
                  f"--system --apply --only {component.key}); never escalates by itself")
        for package in missing_rpms(component):
            print(f"  warn     runtime package {package} is not installed (rpm -q {package}); the binary needs it")
        for problem in problems:
            blocked = True
            print(f"  BLOCKED  {problem}")
        for change in changes:
            if change.kind == "locale" and not change.path.is_symlink():
                continue                    # summarised in the "locale" line below
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
        missing = [k for k in component.keeps if not keep_path(home, component, k).exists()]
        print(f"  kept     {len(component.keeps) - len(missing)}/{len(component.keeps)} imported modules/helpers untouched"
              + (f" (missing now: {', '.join(missing)})" if missing else ""))
        for unit in component.units:
            print(f"  autostart unchanged: systemctl --user start {unit.name}")
        swapped_keys = {key for change in changes if change.commands for key in change.commands}
        for key, key_state in keybind_states(home, component).items():
            if key_state in ("other", "missing") and key not in swapped_keys:
                print(f"  warn     {RC_XML} {key}: {key_state} (no Python {key} keybind to repoint; left as is)")
        if component.health:
            print(f"  health   after --restart: {shlex.join(health_argv(home, component))} must exit 0")
        if component.keybinds:
            print("  reload   labwc --reconfigure only with --restart --restart-shell (labwc is never restarted)")
        for note in component.notes:
            print(f"  note     {note}")
        for warning in guard_warnings(component):
            print(f"  warn     {warning}")
        if component.locale_dir:
            print(f"  locale   {sum(c.kind == 'locale' for c in changes)} catalogs to write into "
                  f"{component_dir(home, component) / component.locale_dir}")
        if component.warning:
            print(f"  WARNING  {component.warning}")
    if needs_shared_locale(components):
        changes, problems = plan_locale(home, locale_source)
        blocked = blocked or bool(problems)
        print(f"locale -> {home / LOCALE}: {len(changes)} catalogs to write"
              + "".join(f"\n  BLOCKED  {p}" for p in problems))
    others = sorted({c.scope for c in COMPONENTS} - {c.scope for c in components})
    hints = {"system": "--system (root-owned /usr/local entries: " + ", ".join(
                 c.key for c in COMPONENTS if c.scope == "system") + ")",
             "remote": "--target-root <Kasumi home> (" + ", ".join(c.key for c in COMPONENTS if c.scope == "remote") + ")",
             "user": "no --system/--target-root (the per-user entries)"}
    for scope in others:
        print(f"Not shown: preview with {hints[scope]}.")
    print("Preview only. Run with --apply to swap; nothing is restarted without --restart.")
    return not blocked


# ---- main -----------------------------------------------------------------------------------

SCOPE_FLAG = {"user": "neither --system nor --target-root", "system": "--system",
              "remote": "--target-root <the Kasumi user's home, mounted or staged here>"}


def selected(only, scope="user"):
    """The components of one mode: all of its scope, or the --only keys (which must belong to it)."""
    if not only:
        return [component for component in COMPONENTS if component.scope == scope and not component.explicit]
    keys = [key.strip() for key in only.split(",") if key.strip()]
    unknown = [key for key in keys if key not in BY_KEY]
    if unknown:
        raise SystemExit(f"Unknown component(s): {', '.join(unknown)}. Known: {', '.join(BY_KEY)}")
    wrong = [key for key in keys if BY_KEY[key].scope != scope]
    if wrong:
        raise SystemExit("; ".join(f"{key} needs {SCOPE_FLAG[BY_KEY[key].scope]}" for key in wrong))
    return [BY_KEY[key] for key in keys]


def stop_resident(component, home):
    """SIGTERM the user's running instances (Python or Rust) of a unit-less resident component."""
    for info in user_processes():
        if info["pid"] != os.getpid() and classify(info, home, component):
            print(f"kill -TERM {info['pid']} ({component.key}: {' '.join(info['argv'][:2])})")
            try:
                os.kill(info["pid"], signal.SIGTERM)
            except ProcessLookupError:
                pass


def health_argv(home, component):
    return [str(binary_path(home, component, component.health[0])), *component.health[1:]]


def wait_healthy(home, component, timeout=20):
    """Poll the component's health command until it exits 0 (and, for JSON, reports ready)."""
    argv = health_argv(home, component)
    deadline = time.monotonic() + timeout
    failure = "no answer"
    while True:
        try:
            result = subprocess.run(argv, capture_output=True, text=True, timeout=5)
            try:
                data = json.loads(result.stdout)
            except ValueError:
                data = None
            ready = not isinstance(data, dict) or "ready" not in data or bool(data["ready"])
            if result.returncode == 0 and ready:
                print(f"healthy: {shlex.join(argv)}")
                return
            failure = (result.stderr or result.stdout).strip() or f"exit {result.returncode}"
        except (OSError, subprocess.TimeoutExpired) as error:
            failure = str(error)
        if time.monotonic() >= deadline:
            raise RuntimeError(f"{component.key}: {shlex.join(argv)} did not become healthy ({failure}). "
                               f"Roll back with --rollback --only {component.key} --restart --restart-shell.")
        time.sleep(0.25)


def reconfigure_labwc():
    """Reload labwc's rc.xml (SIGHUP to $LABWC_PID); never a restart, and only inside the labwc session."""
    if not os.environ.get("LABWC_PID"):
        print("Not in the labwc session (no LABWC_PID); run `labwc --reconfigure` there to load the keybinds.")
        return
    print("labwc --reconfigure")
    subprocess.run(["labwc", "--reconfigure"], check=True)


def restart_units(components, home, allow_shell, keybinds_changed=False, check_health=True):
    for component in components:
        if component.signal_on_restart:
            stop_resident(component, home)
        for unit in component.units:
            if unit.name in NEVER_RESTART or (unit.name in SHELL_UNITS and not allow_shell):
                print(f"Not restarting {unit.name}" + ("" if unit.name in NEVER_RESTART else " (needs --restart-shell)"))
                continue
            if systemctl("is-active", "--quiet", unit.name).returncode != 0:
                print(f"{unit.name} is not active; left stopped.")
                continue
            print(f"systemctl --user restart {unit.name}")
            systemctl("restart", unit.name, check=True)
            if check_health and component.health:
                wait_healthy(home, component)
    if keybinds_changed:
        if allow_shell:
            reconfigure_labwc()
        else:
            print("rc.xml keybinds changed; labwc --reconfigure needs --restart-shell (or run it yourself).")


def target_root(path):
    """--target-root, refused when it is (or contains) this machine's home or /."""
    resolved = path.resolve()
    for protected in {REAL_HOME.resolve(), INVOKING_HOME.resolve(), Path.home().resolve(), Path("/")}:
        if resolved == protected or protected.is_relative_to(resolved):
            raise SystemExit(f"--target-root {resolved} is this machine's {protected}: give the Kasumi user's home as "
                             "mounted or staged here (e.g. an sshfs mount), never the local home or /")
    if not resolved.is_dir():
        raise SystemExit(f"--target-root {resolved} is not a directory")
    return resolved


def require_root(action):
    if os.geteuid() != 0:
        raise RuntimeError(f"Nothing was changed: --system {action} needs root, and this tool never asks for it. Run it "
                           f"yourself, e.g.: pkexec /usr/bin/python3 {Path(__file__).resolve()} --system {action} "
                           "--only <keys> (preview first with --system --check as your user).")


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
    place = parser.add_mutually_exclusive_group()
    place.add_argument("--system", action="store_true",
                       help="the root-owned /usr/local entries (standalone apps, login-manager); --apply/--rollback "
                            "only when already running as root")
    place.add_argument("--target-root", type=Path,
                       help="home-assistant-screen: the Kasumi user's home as mounted or staged here")
    parser.add_argument("--root", type=Path, default=Path("/"), help=argparse.SUPPRESS)   # tests: private prefix
    parser.add_argument("--restart", action="store_true", help="systemctl --user restart the swapped active units")
    parser.add_argument("--restart-shell", action="store_true",
                        help="also allow restarting waydock/wayshell and `labwc --reconfigure` (never a labwc restart)")
    parser.add_argument("--no-reload", action="store_true", help="skip systemctl --user daemon-reload")
    parser.add_argument("--force", action="store_true", help="rollback: also restore files changed since the swap")
    parser.add_argument("--json", action="store_true", help="--status as JSON")
    parser.add_argument("--i-am-at-the-keyboard", action="store_true", dest="confirmed",
                        help="allow --apply for components that need a live check (authentication-manager, "
                             "network-manager, wayshell, waydock, lock-screen, login-manager)")
    args = parser.parse_args(argv)
    if args.list:
        for c in COMPONENTS:
            print(f"{c.key} [{c.scope}]: package {c.package}, into {c.base}/{c.dir}/: "
                  f"{', '.join(installed_names(c) + list(c.helpers))}; "
                  f"units {', '.join(u.name for u in c.units) or '-'}; shims {', '.join(s.entry for s in c.shims)}")
        return 0
    scope = "system" if args.system else "remote" if args.target_root else "user"
    system = scope == "system"
    if scope == "system":
        home = args.root.resolve()
    elif scope == "remote":
        home = target_root(args.target_root)
    else:
        home = args.home.resolve()
    components = selected(args.only, scope)
    prebuilt = args.prebuilt.resolve() if args.prebuilt else None
    user_units = scope == "user"           # systemctl --user only ever concerns this user's own units

    if args.status:
        status(home, components, args.json, system)
        return 0
    if args.rollback:
        if system and not args.check:
            require_root("--rollback")
            os.umask(0o022)
        groups = rollback(home, [c.key for c in components], preview=args.check, force=args.force, system=system)
        if groups and not args.check and user_units:
            if not args.no_reload:
                systemctl("daemon-reload")
            if args.restart:
                swapped = [BY_KEY[g] for g in groups if g in BY_KEY]
                restart_units(swapped, home, args.restart_shell,
                              keybinds_changed=any(c.keybinds for c in swapped), check_health=False)
            elif any(BY_KEY[g].keybinds for g in groups if g in BY_KEY):
                print("rc.xml keybinds restored; run `labwc --reconfigure` (or use --restart --restart-shell).")
        elif groups and not args.check and scope == "remote":
            print("On the Kasumi device: systemctl --user daemon-reload && systemctl --user restart luna-clock.service")
        return 0
    if not args.apply or args.check:
        return 0 if check(home, components, resolve_sources(components, prebuilt), args.locale_source) else 1
    if system:
        require_root("--apply")
        os.umask(0o022)
    unconfirmed = [c.key for c in components if c.requires_confirmation]
    if unconfirmed and not args.confirmed:
        raise RuntimeError("Nothing was changed: " + ", ".join(unconfirmed) + " must be tested right after the swap "
                           "(a password prompt, Wi-Fi, the dock, Alt+Tab, the lock screen or the greeter). Re-run with "
                           "--i-am-at-the-keyboard, or leave it out with --only.")
    if user_units:
        active = [unit for c in components for unit in c.refuse_while_active
                  if systemctl("is-active", "--quiet", unit).returncode == 0]
        if active:
            raise RuntimeError("Nothing was changed: " + ", ".join(active) + " is active (the screen is locked); "
                               "swap it while the session is unlocked.")
    if scope != "user":
        if not prebuilt:
            print("cargo is not run in --system/--target-root mode (never as root, never for another machine): "
                  "using the binaries already built in " + ("target/release" if system else AARCH64_RELEASE) + ".")
    elif not prebuilt and not args.no_build:
        build(components)
    sources = resolve_sources(components, prebuilt)     # after the build: helpers live in OUT_DIR
    changed = apply(home, components, sources, args.locale_source, system)
    for component in components:
        if component.warning:
            print(f"WARNING: {component.warning}")
    if not user_units:
        if args.restart:
            print("--restart is ignored here: nothing is restarted for --system or --target-root entries.")
        if scope == "remote" and changed:
            print("On the Kasumi device: systemctl --user daemon-reload && systemctl --user restart luna-clock.service")
        elif changed:
            print("Nothing was restarted: the next launch of each app (the greeter: the next login screen) runs Rust.")
        return 0
    if any(change.kind == "dropin" for change in changed) and not args.no_reload:
        systemctl("daemon-reload")
    keybinds_changed = any(change.kind == "rcxml" for change in changed)
    if args.restart:
        restart_units(components, home, args.restart_shell, keybinds_changed=keybinds_changed)
    else:
        print("Running services were not restarted; use --restart (or restart them yourself) to run Rust.")
        if keybinds_changed:
            print("rc.xml keybinds changed; run `labwc --reconfigure` (or use --restart --restart-shell).")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as error:
        print(error, file=sys.stderr)
        sys.exit(1)
