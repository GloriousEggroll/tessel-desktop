# Packaged defaults

System-wide defaults Tessel package ships. They apply to new users and to
anyone who never changed the setting; per-user choices made in Global Settings
(gsettings, `~/.config/gtk-*/settings.ini`, `~/.config/mimeapps.list`) win.

Paths below mirror the install root.

| Default | File | Notes |
|---|---|---|
| Icon theme Papirus-Dark | `usr/share/glib-2.0/schemas/90_tessel.gschema.override` | `org.gnome.desktop.interface icon-theme`; run `glib-compile-schemas` in `%posttrans`. Requires `papirus-icon-theme`. |
| Archive Manager opens archives | `usr/share/applications/labwc-mimeapps.list` | `[Default Applications]` for every archive mime type of `org.tessel.ArchiveManager.desktop` (its `MimeType=` line is the source; keep both in sync). Read for `XDG_CURRENT_DESKTOP=wlroots:labwc` sessions; requires the tessel-archive package (`/usr/local/share/applications/org.tessel.ArchiveManager.desktop` or the packaged path). Per-user equivalent: `tools/install.py --apply --only archive-manager-mime`. |
| Archive Manager settings | none shipped (built in) | Defaults live in code (`components/archive/settings/src/lib.rs`, `Settings::default`): info panel on, preserve paths on, extract into subfolder on, open destination off, close after extraction off, all plugins enabled, internal preview on, preview limit 50 MB. `~/.config/tessel/tessel-archive.json` is written only when the user changes a setting (Archive Manager or Global Settings > Archive Manager). |
| Icon theme Papirus-Dark (GTK fallback) | `etc/gtk-3.0/settings.ini`, `etc/gtk-4.0/settings.ini` | Read by GTK when no settings portal/gsettings value applies. |
| Login repair of a missing icon theme | `etc/xdg/autostart/tessel-icon-theme.desktop` | Runs `tessel-icon-theme` (tessel-session-startup crate; install it on `PATH`, e.g. `/usr/bin`) through `labwc-xdg-autostart.target`. |
| Clipboard kept after apps close | `usr/lib/systemd/user-preset/90-tessel.preset` | `enable tessel-clipboard.service` (WantedBy=graphical-session.target). Package `components/clipboard/tessel-clipboard.service` into `usr/lib/systemd/user/` with `ExecStart=` pointing at the packaged `tessel-clipboard`, and run `systemctl --global preset tessel-clipboard.service` in `%post` (or use `%systemd_user_post`). Per-user equivalent: `tools/install.py --apply --only tessel-clipboard` then `systemctl --user enable --now tessel-clipboard.service`. |
| Clipboard settings | none shipped (built in) | Defaults live in code (`components/clipboard/settings/src/lib.rs`, `Settings::default`): keep the clipboard on, keep the middle-click selection off, skip password-manager secrets on, largest kept copy 64 MB. `~/.config/tessel/clipboard.json` is written only when the user changes a setting (Global Settings > Clipboard). |
| Saved passwords: tessel-keyring is the Secret Service | `usr/share/dbus-1/services/org.freedesktop.secrets.service`, `usr/lib/systemd/user/tessel-keyring.service` (sources: `components/keyring/data/`, with `ExecStart`/`Exec` pointing at the packaged `tessel-keyring`) | The package must replace gnome-keyring's activation file for `org.freedesktop.secrets` (Conflicts: gnome-keyring, or remove its `/usr/share/dbus-1/services/org.freedesktop.secrets.service` and `/usr/share/xdg-desktop-portal/portals/gnome-keyring.portal`; ship a `tessel-keyring.portal` with `DBusName=org.freedesktop.secrets`, `Interfaces=org.freedesktop.impl.portal.Secret`, `UseIn=labwc;wlroots`). Per-user equivalent: `tools/install.py --apply --only keyring-session` (user D-Bus service dir wins). |
| Saved passwords unlock with the login password | `etc/pam.d/tessel-login` (Tessel Login's login stack), `etc/pam.d/passwd` | `auth substack tessel-login-auth` + `-auth optional pam_waykeyring.so`, `-session optional pam_waykeyring.so` after `session include system-auth`, and `-password optional pam_waykeyring.so` in passwd. Ship `pam_waykeyring.so` in `/usr/lib64/security/` and the helper at `/usr/lib/tessel/keyring/tessel-keyring` (the module's `helper=` option, default `/usr/local/lib/tessel/keyring/tessel-keyring`). Every entry point returns PAM_IGNORE. Per-user equivalent: `pkexec install.py --system --apply --only keyring --i-am-at-the-keyboard`. |

Session startup also repairs a missing icon theme at login (falls back to Papirus-Dark when the
configured theme is not installed), so removing a theme package never leaves the desktop without icons.

When adding a new suite-wide default, add its file here and a row to this table.
